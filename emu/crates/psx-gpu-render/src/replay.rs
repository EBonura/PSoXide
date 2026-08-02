//! Accurate-backend lowering of the shared GP0 interpreter stream.
//!
//! Strategy: each frame the frontend drains the CPU rasterizer's
//! `cmd_log` (already populated by `enable_pixel_tracer`), replays
//! every GP0 packet through this backend's compute dispatchers, and
//! downloads the resulting VRAM for display. The CPU rasterizer
//! remains the source of truth -- its VRAM is uploaded into the
//! compute backend at frame start so VRAM uploads / VRAM-to-VRAM
//! copies / FMV writes are reflected. The compute path then redraws
//! the frame's GP0 packets on top.
//!
//! Packet decoding and GP0 state tracking live in the shared
//! [`Interpreter`]; this module only lowers [`GpuEvent`]s into
//! compute dispatches: primitive structs, flag packing, the CPU's
//! quad split orders and the axis-aligned bilinear quad fast path.
//!
//! This is intentionally a SHADOW renderer for now: if the compute
//! output diverges from the CPU's, the user-visible result is wrong
//! pixels but the next frame the VRAM gets re-synced, so divergences
//! don't accumulate. Behind the runtime `--gpu-compute` flag.
//!
//! What's NOT handled here yet
//!   - Lines / polylines (`0x40..=0x5F`) -- rare in real games; the
//!     shared interpreter decodes them and the render backend
//!     (`translator`) draws them, but this compute path has no line
//!     dispatcher yet and counts them as unhandled.
//!   - GP1 commands -- display-mode state, not rendering.
//!   - VRAM-to-CPU readback (`0xC0..=0xDF`) -- game-side reads, no
//!     visible output.

use std::sync::Arc;

use emulator_core::gpu::GpuCmdLogEntry;

use crate::decode::{decode_tint, is_raw_texture, is_semi_trans, rgb24_to_bgr15};
use crate::interpreter::{GpuEvent, Interpreter};
use crate::primitive::{
    BlendMode, Fill, MonoRect, MonoTri, PrimFlags, ShadedTexTri, ShadedTri, TexQuadBilinear,
    TexRect, TexTri,
};
use crate::rasterizer::Rasterizer;
use crate::vram::{self, VramGpu};

/// Compute backend -- owns `VramGpu` + `Rasterizer` plus the shared
/// interpreter that tracks GP0 state and decodes each packet.
pub struct ComputeBackend {
    vram: VramGpu,
    rasterizer: Rasterizer,
    interp: Interpreter,
    /// Counts of unhandled opcodes so the frontend can surface
    /// "compute backend doesn't yet know how to draw X" warnings.
    pub unhandled: std::collections::BTreeMap<u8, u64>,
}

impl ComputeBackend {
    /// Build the backend on a fresh headless wgpu adapter. The
    /// frontend uses this when `--gpu-compute` is enabled -- sharing
    /// the main `Graphics` device would need an `Arc<Device>`
    /// refactor through the whole gfx layer, and the per-frame VRAM
    /// bounce already goes through CPU memory so a separate adapter
    /// costs nothing extra in the steady state.
    pub fn new_headless() -> Self {
        let vram = VramGpu::new_headless();
        let rasterizer = Rasterizer::new(&vram);
        Self {
            vram,
            rasterizer,
            interp: Interpreter::new(),
            unhandled: std::collections::BTreeMap::new(),
        }
    }

    /// Build on top of an existing wgpu device -- useful for tests
    /// or for a future zero-bounce display path where compute and
    /// display share the same adapter.
    #[allow(dead_code)]
    pub fn new(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) -> Self {
        let vram = VramGpu::new(device, queue);
        let rasterizer = Rasterizer::new(&vram);
        Self {
            vram,
            rasterizer,
            interp: Interpreter::new(),
            unhandled: std::collections::BTreeMap::new(),
        }
    }

    /// Replace the GPU-side VRAM with the CPU rasterizer's current
    /// VRAM. Called at frame start so the compute path sees the
    /// same texture / framebuffer state the CPU is about to render
    /// against.
    pub fn sync_vram_from_cpu(&self, cpu_words: &[u16]) {
        // `upload_full` validates length matches 1024×512.
        let _ = self.vram.upload_full(cpu_words);
    }

    /// Read back the GPU VRAM for display. Slow per-frame
    /// (1 MiB GPU→CPU bounce) -- acceptable for an opt-in shadow
    /// renderer. A future optimisation would render directly into
    /// the egui texture without the CPU round-trip.
    pub fn download_vram(&self) -> Vec<u16> {
        self.vram.download_full().unwrap_or_default()
    }

    /// Lift a sub-rectangle of CPU VRAM into GPU VRAM. The bisector
    /// uses this to apply CPU-to-VRAM uploads and FillRects whose
    /// pixel data isn't in the cmd_log proper -- it streams via
    /// `ingest_vram_upload_word` on the bus side. Production replay
    /// (frontend / replay_disc) doesn't need this because it
    /// `sync_vram_from_cpu`s the full VRAM at frame boundaries.
    pub fn upload_rect_from_cpu(&self, cpu_words: &[u16], x: u32, y: u32, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        // Honour VRAM wrap (hardware wraps both axes mod 1024 / 512).
        // We slice CPU words row-by-row so partial-row wraps work.
        let mut buf = Vec::with_capacity((w * h) as usize);
        for row in 0..h {
            let py = (y + row) & (vram::VRAM_HEIGHT - 1);
            for col in 0..w {
                let px = (x + col) & (vram::VRAM_WIDTH - 1);
                buf.push(cpu_words[(py * vram::VRAM_WIDTH + px) as usize]);
            }
        }
        let _ = self.vram.upload_rect(
            x & (vram::VRAM_WIDTH - 1),
            y & (vram::VRAM_HEIGHT - 1),
            w.min(vram::VRAM_WIDTH),
            h.min(vram::VRAM_HEIGHT),
            &buf,
        );
    }

    /// Replay one GP0 packet captured by `enable_pixel_tracer` on
    /// the CPU side. The shared interpreter updates state for
    /// `0xE1..=0xE6` and decodes drawables; this lowers them into
    /// compute dispatches.
    pub fn replay_packet(&mut self, entry: &GpuCmdLogEntry) {
        let Some(event) = self.interp.interpret(entry) else {
            return;
        };
        match event {
            GpuEvent::Fill { cmd, x, y, w, h } => self.lower_fill(cmd, x, y, w, h),
            GpuEvent::MonoTri { cmd, v } => self.lower_mono_tri(cmd, v),
            GpuEvent::MonoQuad { cmd, v } => self.lower_mono_quad(cmd, v),
            GpuEvent::TexTri { cmd, v, uv, clut } => self.lower_tex_tri(cmd, v, uv, clut),
            GpuEvent::TexQuad { cmd, v, uv, clut } => self.lower_tex_quad(cmd, v, uv, clut),
            GpuEvent::ShadedTri { cmd, v, colors } => self.lower_shaded_tri(cmd, v, colors),
            GpuEvent::ShadedQuad { cmd, v, colors } => self.lower_shaded_quad(cmd, v, colors),
            GpuEvent::ShadedTexTri {
                cmd,
                v,
                uv,
                colors,
                clut,
            } => self.lower_shaded_tex_tri(cmd, v, uv, colors, clut),
            GpuEvent::ShadedTexQuad {
                cmd,
                v,
                uv,
                colors,
                clut,
            } => self.lower_shaded_tex_quad(cmd, v, uv, colors, clut),
            GpuEvent::MonoRect { cmd, xy, w, h } => self.lower_mono_rect(cmd, xy, w, h),
            GpuEvent::TexRect {
                cmd,
                xy,
                uv,
                clut,
                w,
                h,
            } => self.lower_tex_rect(cmd, xy, uv, clut, w, h),
            GpuEvent::VramCopy {
                sx,
                sy,
                dx,
                dy,
                w,
                h,
            } => {
                self.rasterizer
                    .dispatch_vram_copy(&self.vram, (sx, sy), (dx, dy), (w, h));
            }
            // Lines decode in the shared interpreter now (the render
            // backend draws them), but this compute path still has no
            // line dispatcher -- keep counting them as unhandled so
            // the frontend warning stays truthful.
            GpuEvent::MonoLine { cmd, .. } | GpuEvent::ShadedLine { cmd, .. } => {
                *self.unhandled.entry((cmd >> 24) as u8).or_insert(0) += 1;
            }
            GpuEvent::Unhandled { opcode } => {
                *self.unhandled.entry(opcode).or_insert(0) += 1;
            }
        }
    }

    // ========== Flag helpers ==========

    fn mono_blend_mode_and_flags(&self, cmd: u32) -> (PrimFlags, BlendMode) {
        let mut flags = self.interp.state.base_flags();
        if is_semi_trans(cmd) {
            flags |= PrimFlags::SEMI_TRANS;
        }
        (flags, self.interp.state.tex_blend_mode)
    }

    fn tex_flags_and_mode(&self, cmd: u32) -> (PrimFlags, BlendMode) {
        let mut flags = self.interp.state.base_flags();
        if is_raw_texture(cmd) {
            flags |= PrimFlags::RAW_TEXTURE;
        }
        if is_semi_trans(cmd) {
            flags |= PrimFlags::SEMI_TRANS;
        }
        (flags, self.interp.state.tex_blend_mode)
    }

    fn rect_flags_and_mode(&self, cmd: u32) -> (PrimFlags, BlendMode) {
        let mut flags = self.interp.state.base_flags();
        if is_semi_trans(cmd) {
            flags |= PrimFlags::SEMI_TRANS;
        }
        (flags, self.interp.state.tex_blend_mode)
    }

    fn tex_rect_flags_and_mode(&self, cmd: u32) -> (PrimFlags, BlendMode) {
        let mut flags = self.interp.state.rect_flip_flags();
        if is_raw_texture(cmd) {
            flags |= PrimFlags::RAW_TEXTURE;
        }
        if is_semi_trans(cmd) {
            flags |= PrimFlags::SEMI_TRANS;
        }
        (flags, self.interp.state.tex_blend_mode)
    }

    // ========== Lowering ==========

    fn lower_fill(&mut self, cmd: u32, x: u32, y: u32, w: u32, h: u32) {
        let color = rgb24_to_bgr15(cmd & 0x00FF_FFFF);
        let fill = Fill::new((x, y), (w, h), color);
        self.rasterizer.dispatch_fill(&self.vram, &fill);
    }

    fn lower_mono_tri(&mut self, cmd: u32, v: [(i32, i32); 3]) {
        let color = rgb24_to_bgr15(cmd & 0x00FF_FFFF);
        let (flags, mode) = self.mono_blend_mode_and_flags(cmd);
        let tri = MonoTri::new(v[0], v[1], v[2], color, flags, mode);
        self.rasterizer
            .dispatch_mono_tri_scanline(&self.vram, &tri, &self.interp.state.draw_area);
    }

    fn lower_mono_quad(&mut self, cmd: u32, v: [(i32, i32); 4]) {
        let color = rgb24_to_bgr15(cmd & 0x00FF_FFFF);
        let (flags, mode) = self.mono_blend_mode_and_flags(cmd);
        // Quad → 2 triangles in Redux/CPU order: (v1, v3, v2) then
        // (v0, v1, v2). Flat color makes the order invisible today,
        // but keep every quad lowering on the same convention.
        let t1 = MonoTri::new(v[1], v[3], v[2], color, flags, mode);
        let t2 = MonoTri::new(v[0], v[1], v[2], color, flags, mode);
        self.rasterizer
            .dispatch_mono_tri_scanline(&self.vram, &t1, &self.interp.state.draw_area);
        self.rasterizer
            .dispatch_mono_tri_scanline(&self.vram, &t2, &self.interp.state.draw_area);
    }

    fn lower_tex_tri(&mut self, cmd: u32, v: [(i32, i32); 3], uv: [(u8, u8); 3], clut: (u32, u32)) {
        let tint = decode_tint(cmd & 0x00FF_FFFF);
        let (flags, mode) = self.tex_flags_and_mode(cmd);
        let tri = TexTri::new(
            v[0], v[1], v[2], uv[0], uv[1], uv[2], clut.0, clut.1, tint, flags, mode,
        );
        self.rasterizer.dispatch_tex_tri_scanline(
            &self.vram,
            &tri,
            &self.interp.state.tpage,
            &self.interp.state.draw_area,
        );
    }

    fn lower_tex_quad(
        &mut self,
        cmd: u32,
        v: [(i32, i32); 4],
        uv: [(u8, u8); 4],
        clut: (u32, u32),
    ) {
        let tint = decode_tint(cmd & 0x00FF_FFFF);
        let (flags, mode) = self.tex_flags_and_mode(cmd);

        // Phase C bug fix: when the quad is axis-aligned the CPU
        // rasterizer skips the triangle split and runs a bilinear
        // UV walk over all four corners. Triangle-split + bary
        // interpolation produces different pixels for non-affine
        // UV layouts (commercial character draws hit this). Mirror
        // the CPU's fast path here so VRAM stays in sync.
        if TexQuadBilinear::is_axis_aligned(v[0], v[1], v[2], v[3]) {
            let q = TexQuadBilinear::new(
                v[0], v[1], v[2], v[3], uv[0], uv[1], uv[2], uv[3], clut.0, clut.1, tint, flags,
                mode,
            );
            self.rasterizer.dispatch_tex_quad_bilinear(
                &self.vram,
                &q,
                &self.interp.state.tpage,
                &self.interp.state.draw_area,
            );
            return;
        }

        // Non-axis-aligned: fall back to the same triangle split
        // the CPU uses (v1, v3, v2) then (v0, v1, v2).
        let t1 = TexTri::new(
            v[1], v[3], v[2], uv[1], uv[3], uv[2], clut.0, clut.1, tint, flags, mode,
        );
        let t2 = TexTri::new(
            v[0], v[1], v[2], uv[0], uv[1], uv[2], clut.0, clut.1, tint, flags, mode,
        );
        self.rasterizer.dispatch_tex_tri_scanline(
            &self.vram,
            &t1,
            &self.interp.state.tpage,
            &self.interp.state.draw_area,
        );
        self.rasterizer.dispatch_tex_tri_scanline(
            &self.vram,
            &t2,
            &self.interp.state.tpage,
            &self.interp.state.draw_area,
        );
    }

    fn lower_shaded_tri(&mut self, cmd: u32, v: [(i32, i32); 3], colors: [u32; 3]) {
        let c0 = decode_tint(colors[0] & 0x00FF_FFFF);
        let c1 = decode_tint(colors[1] & 0x00FF_FFFF);
        let c2 = decode_tint(colors[2] & 0x00FF_FFFF);
        let (flags, mode) = self.mono_blend_mode_and_flags(cmd);
        let tri = ShadedTri::new(v[0], v[1], v[2], c0, c1, c2, flags, mode);
        self.rasterizer.dispatch_shaded_tri_scanline(
            &self.vram,
            &tri,
            &self.interp.state.draw_area,
        );
    }

    fn lower_shaded_quad(&mut self, cmd: u32, v: [(i32, i32); 4], colors: [u32; 4]) {
        let c0 = decode_tint(colors[0] & 0x00FF_FFFF);
        let c1 = decode_tint(colors[1] & 0x00FF_FFFF);
        let c2 = decode_tint(colors[2] & 0x00FF_FFFF);
        let c3 = decode_tint(colors[3] & 0x00FF_FFFF);
        let (flags, mode) = self.mono_blend_mode_and_flags(cmd);
        // Redux/CPU split order: (v1, v3, v2) first, then (v0, v1, v2)
        // so the first half wins the shared diagonal. Gouraud colors
        // interpolate slightly differently per half, so drawing the
        // halves in the other order diverges by one LSB on diagonal
        // pixels (alttp menu quad, replay_bisect op 0x38).
        let t1 = ShadedTri::new(v[1], v[3], v[2], c1, c3, c2, flags, mode);
        let t2 = ShadedTri::new(v[0], v[1], v[2], c0, c1, c2, flags, mode);
        self.rasterizer
            .dispatch_shaded_tri_scanline(&self.vram, &t1, &self.interp.state.draw_area);
        self.rasterizer
            .dispatch_shaded_tri_scanline(&self.vram, &t2, &self.interp.state.draw_area);
    }

    fn lower_shaded_tex_tri(
        &mut self,
        cmd: u32,
        v: [(i32, i32); 3],
        uv: [(u8, u8); 3],
        colors: [u32; 3],
        clut: (u32, u32),
    ) {
        let c0 = decode_tint(colors[0] & 0x00FF_FFFF);
        let c1 = decode_tint(colors[1] & 0x00FF_FFFF);
        let c2 = decode_tint(colors[2] & 0x00FF_FFFF);
        let (flags, mode) = self.tex_flags_and_mode(cmd);
        let tri = ShadedTexTri::new(
            v[0], v[1], v[2], c0, c1, c2, uv[0], uv[1], uv[2], clut.0, clut.1, flags, mode,
        );
        self.rasterizer.dispatch_shaded_tex_tri_scanline(
            &self.vram,
            &tri,
            &self.interp.state.tpage,
            &self.interp.state.draw_area,
        );
    }

    fn lower_shaded_tex_quad(
        &mut self,
        cmd: u32,
        v: [(i32, i32); 4],
        uv: [(u8, u8); 4],
        colors: [u32; 4],
        clut: (u32, u32),
    ) {
        let c0 = decode_tint(colors[0] & 0x00FF_FFFF);
        let c1 = decode_tint(colors[1] & 0x00FF_FFFF);
        let c2 = decode_tint(colors[2] & 0x00FF_FFFF);
        let c3 = decode_tint(colors[3] & 0x00FF_FFFF);
        let (flags, mode) = self.tex_flags_and_mode(cmd);
        let t1 = ShadedTexTri::new(
            v[1], v[3], v[2], c1, c3, c2, uv[1], uv[3], uv[2], clut.0, clut.1, flags, mode,
        );
        let t2 = ShadedTexTri::new(
            v[0], v[1], v[2], c0, c1, c2, uv[0], uv[1], uv[2], clut.0, clut.1, flags, mode,
        );
        self.rasterizer.dispatch_shaded_tex_tri_scanline(
            &self.vram,
            &t1,
            &self.interp.state.tpage,
            &self.interp.state.draw_area,
        );
        self.rasterizer.dispatch_shaded_tex_tri_scanline(
            &self.vram,
            &t2,
            &self.interp.state.tpage,
            &self.interp.state.draw_area,
        );
    }

    fn lower_mono_rect(&mut self, cmd: u32, xy: (i32, i32), w: u32, h: u32) {
        let color = rgb24_to_bgr15(cmd & 0x00FF_FFFF);
        let (flags, mode) = self.rect_flags_and_mode(cmd);
        let rect = MonoRect::new(xy, (w, h), color, flags, mode);
        self.rasterizer
            .dispatch_mono_rect(&self.vram, &rect, &self.interp.state.draw_area);
    }

    fn lower_tex_rect(
        &mut self,
        cmd: u32,
        xy: (i32, i32),
        uv: (u8, u8),
        clut: (u32, u32),
        w: u32,
        h: u32,
    ) {
        let tint = decode_tint(cmd & 0x00FF_FFFF);
        let (flags, mode) = self.tex_rect_flags_and_mode(cmd);
        let rect = TexRect::new(xy, (w, h), uv, clut.0, clut.1, tint, flags, mode);
        self.rasterizer.dispatch_tex_rect(
            &self.vram,
            &rect,
            &self.interp.state.tpage,
            &self.interp.state.draw_area,
        );
    }
}
