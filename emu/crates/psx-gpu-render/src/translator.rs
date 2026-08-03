//! `cmd_log` → `Vec<HwVertex>` translator.
//!
//! Enhanced-backend lowering of the shared GP0 interpreter stream:
//! packet decoding and state tracking live in [`Interpreter`]; this
//! module turns each [`GpuEvent`] into batched [`HwVertex`] runs for
//! the render pipeline. Quad decomposition, the textured semi-trans
//! two-pass split, sprite UV-wrap chunking, GP0 line quad-expansion
//! and the wireframe debug mode are all enhanced-path concerns and
//! stay here.
//!
//! Events this backend does not draw (VRAM copies) are skipped; the
//! interpreter has still updated the GP0 state so later primitives
//! observe the right tpage / draw area.

use crate::decode::{decode_tint, is_raw_texture, is_semi_trans, rgb24_to_bgr15};
use crate::interpreter::{GpuEvent, Interpreter};
use crate::primitive::BlendMode;
use emulator_core::gpu::GpuCmdLogEntry;

use crate::pipeline::{flags as fbits, BlendKind, HwVertex};

/// Contiguous draw range sharing pipeline state and draw-area clip.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct DrawRun {
    pub kind: BlendKind,
    pub start: u32,
    pub count: u32,
    /// Inclusive PSX-VRAM-space clip rectangle: left, top, right,
    /// bottom. Fill rectangles use the full VRAM clip because GP0
    /// fills ignore draw-area state.
    pub clip: [u16; 4],
}

/// Output of [`Translator::translate`] -- vertices remain in GP0 order
/// and `runs` describes contiguous draw ranges that share pipeline
/// state and draw-area clipping.
pub struct TranslatedFrame<'a> {
    pub vertices: &'a [HwVertex],
    pub runs: &'a [DrawRun],
}

impl TranslatedFrame<'_> {
    /// Total vertex count (sum of all batches).
    pub fn total(&self) -> u32 {
        self.vertices.len() as u32
    }
}

/// The CPU's `RAW_TEXTURE_TINT` sentinel -- an 8-bit-per-channel
/// tint of 0x80 is the identity for `modulate_tint`, and the CPU
/// rasterizer reuses that value to mean "raw, don't dither".
const IDENTITY_TINT: u32 = 0x0080_8080;

pub struct Translator {
    interp: Interpreter,
    /// Frontend debug mode mirroring `Gpu::wireframe_enabled`.
    /// Filled polygons become edge strips; rectangles remain filled
    /// to match the CPU rasterizer's debug path.
    wireframe: bool,
    /// Ordered vertex stream for the current frame. This preserves
    /// GP0 command order, which matters for semi-transparency and
    /// overlapping UI primitives.
    flat: Vec<HwVertex>,
    /// Ordered draw runs over `flat`. Adjacent primitives only merge
    /// when their blend kind and draw-area clip match.
    runs: Vec<DrawRun>,
}

impl Translator {
    pub fn new() -> Self {
        Self {
            interp: Interpreter::new(),
            wireframe: false,
            flat: Vec::with_capacity(4 * 1024),
            runs: Vec::with_capacity(1024),
        }
    }

    /// Current draw-env state (area l/t/r/b, offset x/y) for
    /// incremental-replay diagnostics.
    pub fn debug_env(&self) -> (i32, i32, i32, i32, i32, i32) {
        let a = &self.interp.state.draw_area;
        (
            a.left,
            a.top,
            a.right,
            a.bottom,
            self.interp.state.draw_offset_x,
            self.interp.state.draw_offset_y,
        )
    }

    /// Walk `cmd_log`, return the vertex stream for this frame
    /// laid out as one slice per `BlendKind`. The slices borrow
    /// from `self`; copy out before the next call.
    pub fn translate(&mut self, log: &[GpuCmdLogEntry]) -> TranslatedFrame<'_> {
        self.translate_with_wireframe(log, false)
    }

    /// Same as [`Translator::translate`], with the frontend's
    /// wireframe debug flag applied to polygon primitives.
    pub fn translate_with_wireframe(
        &mut self,
        log: &[GpuCmdLogEntry],
        wireframe: bool,
    ) -> TranslatedFrame<'_> {
        self.wireframe = wireframe;
        self.flat.clear();
        self.runs.clear();
        for entry in log {
            self.process(entry);
        }
        TranslatedFrame {
            vertices: &self.flat,
            runs: &self.runs,
        }
    }

    /// Active blend kind for the *next* primitive about to be
    /// emitted. Mono / shaded primitives always read it via this
    /// helper; textured primitives that set `is_semi_trans` also
    /// route here. `cmd-bit-25` clear → opaque (state's tex blend
    /// mode is irrelevant); set → translate the active tpage's
    /// blend mode into our `BlendKind`.
    fn blend_kind(&self, cmd: u32) -> BlendKind {
        if !is_semi_trans(cmd) {
            return BlendKind::Opaque;
        }
        match self.interp.state.tex_blend_mode {
            BlendMode::Average => BlendKind::Average,
            BlendMode::Add => BlendKind::Add,
            BlendMode::Sub => BlendKind::Sub,
            BlendMode::AddQuarter => BlendKind::AddQuarter,
        }
    }

    /// `fbits::DITHER` while GP0(E1) bit 9 is set, else 0. The CPU
    /// rasterizer dithers Gouraud polygons, lines, and tint-modulated
    /// textured polygons; flat monochrome primitives and raw-texture
    /// primitives are never dithered, so those emitters don't call
    /// this.
    fn dither_flag(&self) -> u32 {
        if self.interp.state.dither {
            fbits::DITHER
        } else {
            0
        }
    }

    fn current_clip(&self) -> [u16; 4] {
        let a = &self.interp.state.draw_area;
        [
            a.left.clamp(0, (crate::target::VRAM_WIDTH - 1) as i32) as u16,
            a.top.clamp(0, (crate::target::VRAM_HEIGHT - 1) as i32) as u16,
            a.right.clamp(0, (crate::target::VRAM_WIDTH - 1) as i32) as u16,
            a.bottom.clamp(0, (crate::target::VRAM_HEIGHT - 1) as i32) as u16,
        ]
    }

    fn push_vertex(&mut self, kind: BlendKind, clip: [u16; 4], vertex: HwVertex) {
        let start = self.flat.len() as u32;
        if let Some(run) = self.runs.last_mut() {
            if run.kind == kind && run.clip == clip && run.start + run.count == start {
                run.count += 1;
                self.flat.push(vertex);
                return;
            }
        }
        self.runs.push(DrawRun {
            kind,
            start,
            count: 1,
            clip,
        });
        self.flat.push(vertex);
    }

    fn process(&mut self, entry: &GpuCmdLogEntry) {
        let Some(event) = self.interp.interpret(entry) else {
            return;
        };
        match event {
            GpuEvent::Fill { cmd, x, y, w, h } => self.emit_fill_rect(cmd, x, y, w, h),
            GpuEvent::MonoTri { cmd, v } => self.emit_mono_tri(cmd, v),
            GpuEvent::MonoQuad { cmd, v } => self.emit_mono_quad(cmd, v),
            GpuEvent::TexTri { cmd, v, uv, clut } => self.emit_tex_tri(cmd, v, uv, clut),
            GpuEvent::TexQuad { cmd, v, uv, clut } => self.emit_tex_quad(cmd, v, uv, clut),
            GpuEvent::ShadedTri { cmd, v, colors } => self.emit_shaded_tri(cmd, v, colors),
            GpuEvent::ShadedQuad { cmd, v, colors } => self.emit_shaded_quad(cmd, v, colors),
            GpuEvent::ShadedTexTri {
                cmd,
                v,
                uv,
                colors,
                clut,
            } => self.emit_shaded_tex_tri(cmd, v, uv, colors, clut),
            GpuEvent::ShadedTexQuad {
                cmd,
                v,
                uv,
                colors,
                clut,
            } => self.emit_shaded_tex_quad(cmd, v, uv, colors, clut),
            GpuEvent::MonoRect { cmd, xy, w, h } => {
                self.push_mono_rect(cmd, xy.0, xy.1, w as i32, h as i32)
            }
            GpuEvent::TexRect {
                cmd,
                xy,
                uv,
                clut,
                w,
                h,
            } => self.push_tex_rect(cmd, xy.0, xy.1, w as i32, h as i32, uv, clut),
            GpuEvent::MonoLine { cmd, points } => self.emit_mono_line(cmd, &points),
            GpuEvent::ShadedLine {
                cmd,
                points,
                colors,
            } => self.emit_shaded_line(cmd, &points, &colors),
            // The render path redraws on top of the VRAM-synced
            // target; copies are not lowered here.
            GpuEvent::VramCopy { .. } | GpuEvent::Unhandled { .. } => {}
        }
    }

    // -------- Primitive emitters --------

    fn emit_mono_tri(&mut self, cmd: u32, v: [(i32, i32); 3]) {
        let color = mono_color_rgba8(cmd);
        let kind = self.blend_kind(cmd);
        let [v0, v1, v2] = v;
        if self.wireframe {
            self.push_wire_tri(v0, color, v1, color, v2, color, kind);
        } else {
            self.push_tri(v0, v1, v2, color, kind);
        }
    }

    fn emit_mono_quad(&mut self, cmd: u32, v: [(i32, i32); 4]) {
        let color = mono_color_rgba8(cmd);
        let kind = self.blend_kind(cmd);
        let [v0, v1, v2, v3] = v;
        // Match the CPU rasterizer's split order
        // (`Gpu::draw_monochrome_quad`): lower/right half first, then
        // upper/left, so pixels on the shared diagonal are owned by
        // (v0, v1, v2).
        if self.wireframe {
            self.push_wire_tri(v1, color, v3, color, v2, color, kind);
            self.push_wire_tri(v0, color, v1, color, v2, color, kind);
        } else {
            self.push_tri(v1, v3, v2, color, kind);
            self.push_tri(v0, v1, v2, color, kind);
        }
    }

    fn push_tri(
        &mut self,
        v0: (i32, i32),
        v1: (i32, i32),
        v2: (i32, i32),
        color: [u8; 4],
        kind: BlendKind,
    ) {
        let clip = self.current_clip();
        for v in [v0, v1, v2] {
            self.push_vertex(
                kind,
                clip,
                HwVertex {
                    pos: [v.0 as i16, v.1 as i16],
                    color,
                    uv: [0, 0],
                    flags: 0,
                    tex_window: 0,
                },
            );
        }
    }

    fn push_wire_tri(
        &mut self,
        v0: (i32, i32),
        c0: [u8; 4],
        v1: (i32, i32),
        c1: [u8; 4],
        v2: (i32, i32),
        c2: [u8; 4],
        kind: BlendKind,
    ) {
        self.push_line_strip(v0, c0, v1, c1, kind);
        self.push_line_strip(v1, c1, v2, c2, kind);
        self.push_line_strip(v2, c2, v0, c0, kind);
    }

    fn push_line_strip(
        &mut self,
        v0: (i32, i32),
        c0: [u8; 4],
        v1: (i32, i32),
        c1: [u8; 4],
        kind: BlendKind,
    ) {
        if v0 == v1 {
            self.push_pixel_quad(v0, c0, kind);
            return;
        }

        // The CPU debug path plots Bresenham pixels. The HW path is
        // triangle-only, so model each edge as a one-PSX-pixel strip.
        // That keeps the outline visible at any internal scale while
        // avoiding optional wgpu line/polygon-mode features.
        let dx = (v1.0 - v0.0).abs();
        let dy = (v1.1 - v0.1).abs();
        let (ox, oy) = if dx >= dy { (0, 1) } else { (1, 0) };
        let v0b = (v0.0 + ox, v0.1 + oy);
        let v1b = (v1.0 + ox, v1.1 + oy);
        self.push_shaded_tri(v0, c0, v1, c1, v0b, c0, 0, kind);
        self.push_shaded_tri(v1, c1, v1b, c1, v0b, c0, 0, kind);
    }

    /// One PSX pixel as a quad -- two triangles over
    /// `[x, x+1] × [y, y+1]`, which lights exactly the `S × S`
    /// target block at any internal scale.
    fn push_pixel_quad(&mut self, v: (i32, i32), c: [u8; 4], kind: BlendKind) {
        let v2 = (v.0 + 1, v.1);
        let v3 = (v.0, v.1 + 1);
        let v4 = (v.0 + 1, v.1 + 1);
        let dither = self.dither_flag();
        self.push_shaded_tri(v, c, v2, c, v3, c, dither, kind);
        self.push_shaded_tri(v2, c, v4, c, v3, c, dither, kind);
    }

    // ----- GP0 lines (0x40..=0x5F) -----

    /// `0x40..=0x47` / `0x48..=0x4F` -- monochrome line / polyline.
    /// One quad band per consecutive point pair. Zero-length
    /// segments plot one pixel, matching the silicon-coordinate DDA
    /// used by the CPU rasterizer.
    /// The CPU routes dithered mono lines through its shaded walker;
    /// this backend carries the same dither through `fbits::DITHER`,
    /// so only the band-vs-Bresenham coverage differs.
    fn emit_mono_line(&mut self, cmd: u32, points: &[(i32, i32)]) {
        let color = mono_color_rgba8(cmd);
        let kind = self.blend_kind(cmd);
        for pair in points.windows(2) {
            let (v0, v1) = (pair[0], pair[1]);
            if v0 == v1 {
                self.push_pixel_quad(v0, color, kind);
                continue;
            }
            self.push_gp0_line_segment(v0, color, v1, color, kind);
        }
    }

    /// `0x50..=0x57` / `0x58..=0x5F` -- Gouraud line / polyline.
    /// Host interpolation replaces the CPU's per-step colour walk;
    /// endpoint colours land exactly, mid-segment colours carry the
    /// same f32-vs-integer tolerance as shaded triangles.
    fn emit_shaded_line(&mut self, cmd: u32, points: &[(i32, i32)], colors: &[u32]) {
        let kind = self.blend_kind(cmd);
        let n = points.len().min(colors.len());
        for i in 1..n {
            let (v0, v1) = (points[i - 1], points[i]);
            let c0 = mono_color_rgba8(colors[i - 1]);
            let c1 = mono_color_rgba8(colors[i]);
            if v0 == v1 {
                // The CPU shaded walker plots exactly one pixel for
                // a zero-length segment (plot, then break) at the
                // segment's start colour.
                self.push_pixel_quad(v0, c0, kind);
                continue;
            }
            self.push_gp0_line_segment(v0, c0, v1, c1, kind);
        }
    }

    /// One GP0 line segment as a one-PSX-pixel quad band (two
    /// triangles) -- the wireframe edge strip's construction
    /// ([`Translator::push_line_strip`]) plus a +1 extension on the
    /// major-axis max end. GP0 lines are endpoint-inclusive in the
    /// CPU rasterizer's Bresenham walk, and the host GPU samples
    /// pixel centers at +0.5: without the extension the far
    /// column/row's center falls just outside the band and the
    /// endpoint pixel goes dark. Wireframe edges keep the
    /// unextended strip: triangle outlines close their loops (the
    /// next edge covers the shared corner), and extending them
    /// would double-blend the corners of semi-transparent
    /// outlines. Polyline joints double-blend HERE by design: the
    /// CPU walker also plots every segment endpoint-inclusive, so
    /// shared polyline vertices are written twice on both paths.
    ///
    /// At native scale the band lights one pixel per major-axis
    /// step, connected like Bresenham; individual steps may sit one
    /// minor-axis pixel off the CPU's walk on rounding ties
    /// (center-sampled band vs integer-stepped walker). At internal
    /// scale S the band scales with the target like every other
    /// primitive.
    fn push_gp0_line_segment(
        &mut self,
        v0: (i32, i32),
        c0: [u8; 4],
        v1: (i32, i32),
        c1: [u8; 4],
        kind: BlendKind,
    ) {
        debug_assert_ne!(v0, v1, "degenerate segments handled by callers");
        let dx = v1.0 - v0.0;
        let dy = v1.1 - v0.1;
        let (mut e0, mut e1) = (v0, v1);
        let (ox, oy) = if dx.abs() >= dy.abs() {
            // x-major: one-pixel-tall band, extend the max-x end.
            if dx >= 0 {
                e1.0 += 1;
            } else {
                e0.0 += 1;
            }
            (0, 1)
        } else {
            // y-major: one-pixel-wide band, extend the max-y end.
            if dy >= 0 {
                e1.1 += 1;
            } else {
                e0.1 += 1;
            }
            (1, 0)
        };
        let e0b = (e0.0 + ox, e0.1 + oy);
        let e1b = (e1.0 + ox, e1.1 + oy);
        let dither = self.dither_flag();
        self.push_shaded_tri(e0, c0, e1, c1, e0b, c0, dither, kind);
        self.push_shaded_tri(e1, c1, e1b, c1, e0b, c0, dither, kind);
    }

    // ----- Phase 2: textured tris + quads -----

    /// `0x24..=0x27` -- textured triangle. The interpreter has
    /// already applied uv1's tpage half to the state.
    fn emit_tex_tri(&mut self, cmd: u32, v: [(i32, i32); 3], uv: [(u8, u8); 3], clut: (u32, u32)) {
        let [v0, v1, v2] = v;
        let [uv0, uv1, uv2] = uv;
        let prim_flags = self.tex_prim_flags(cmd, clut, true);
        let color = tex_tint(cmd);
        let kind = self.blend_kind(cmd);
        if self.wireframe {
            self.push_wire_tri(v0, color, v1, color, v2, color, BlendKind::Opaque);
        } else {
            self.push_tex_tri_psx(
                v0,
                uv16(uv0),
                v1,
                uv16(uv1),
                v2,
                uv16(uv2),
                color,
                prim_flags,
                kind,
            );
        }
    }

    /// `0x2C..=0x2F` -- textured quad. Decomposes to two triangles
    /// using the same winding/order the CPU rasterizer's
    /// `draw_textured_quad` uses (`v1,v3,v2` then `v0,v1,v2`), so
    /// semi-trans / mask behaviour stays pixel-equivalent.
    fn emit_tex_quad(&mut self, cmd: u32, v: [(i32, i32); 4], uv: [(u8, u8); 4], clut: (u32, u32)) {
        let [v0, v1, v2, v3] = v;
        let [uv0, uv1, uv2, uv3] = uv;
        let prim_flags = self.tex_prim_flags(cmd, clut, true);
        let color = tex_tint(cmd);
        let kind = self.blend_kind(cmd);

        if self.wireframe {
            self.push_wire_tri(v1, color, v3, color, v2, color, BlendKind::Opaque);
            self.push_wire_tri(v0, color, v1, color, v2, color, BlendKind::Opaque);
        } else {
            self.push_tex_tri_psx(
                v1,
                uv16(uv1),
                v3,
                uv16(uv3),
                v2,
                uv16(uv2),
                color,
                prim_flags,
                kind,
            );
            self.push_tex_tri_psx(
                v0,
                uv16(uv0),
                v1,
                uv16(uv1),
                v2,
                uv16(uv2),
                color,
                prim_flags,
                kind,
            );
        }
    }

    /// Pack the per-primitive state setter bits + vertex flags
    /// `bits` into the format the shader expects. `clut` is in
    /// PSX VRAM pixels.
    fn tex_prim_flags(&self, cmd: u32, clut: (u32, u32), flat_tint: bool) -> u32 {
        let tp = &self.interp.state.tpage;
        let depth = tp.tex_depth;
        let mut flags = fbits::TEXTURED;
        flags |= fbits::pack_tpage(tp.tpage_x, tp.tpage_y, depth);
        flags |= fbits::pack_clut(clut.0, clut.1);
        if is_raw_texture(cmd) {
            flags |= fbits::RAW_TEXTURE;
        } else if !(flat_tint && cmd & 0x00FF_FFFF == IDENTITY_TINT) {
            // Flat-tint prims: the CPU gates on
            // `tint != RAW_TEXTURE_TINT`, and that sentinel is literally
            // (0x80, 0x80, 0x80) -- so an identity-tinted non-raw prim
            // takes the same undithered path a raw one does. Match it,
            // or every unmodulated sprite dithers here and not there.
            // Shaded-textured prims carry per-vertex tints and have no
            // such sentinel: the CPU gates on `!raw && dither` alone.
            flags |= self.dither_flag();
        }
        if is_semi_trans(cmd) {
            // Textured semi-transparency is split into an opaque
            // non-STP pass and a blended STP-only pass at emission
            // time. Keep the primitive bit here so debug dumps can
            // still identify the original GP0 state.
            flags |= fbits::SEMI_TRANS;
        }
        flags
    }

    fn tex_window_word(&self) -> u32 {
        let tp = &self.interp.state.tpage;
        (tp.tex_window_mask_x & 0xFF)
            | ((tp.tex_window_mask_y & 0xFF) << 8)
            | ((tp.tex_window_off_x & 0xFF) << 16)
            | ((tp.tex_window_off_y & 0xFF) << 24)
    }

    fn push_tex_tri_psx(
        &mut self,
        v0: (i32, i32),
        uv0: (u16, u16),
        v1: (i32, i32),
        uv1: (u16, u16),
        v2: (i32, i32),
        uv2: (u16, u16),
        color: [u8; 4],
        prim_flags: u32,
        kind: BlendKind,
    ) {
        if kind == BlendKind::Opaque {
            self.push_tex_tri(v0, uv0, v1, uv1, v2, uv2, color, prim_flags, kind);
            return;
        }

        // Textured PS1 semi-transparency is per texel: cmd-bit semi-trans
        // enables blending only for sampled texels whose bit 15 is set.
        // Fixed-function blending is per draw, so emit the primitive twice:
        // solid texels first, then STP texels through the requested blend.
        self.push_tex_tri(
            v0,
            uv0,
            v1,
            uv1,
            v2,
            uv2,
            color,
            prim_flags | fbits::TEX_OPAQUE_PASS,
            BlendKind::Opaque,
        );
        self.push_tex_tri(
            v0,
            uv0,
            v1,
            uv1,
            v2,
            uv2,
            color,
            prim_flags | fbits::TEX_SEMI_PASS,
            kind,
        );
    }

    fn push_tex_tri(
        &mut self,
        v0: (i32, i32),
        uv0: (u16, u16),
        v1: (i32, i32),
        uv1: (u16, u16),
        v2: (i32, i32),
        uv2: (u16, u16),
        color: [u8; 4],
        prim_flags: u32,
        kind: BlendKind,
    ) {
        let clip = self.current_clip();
        let tex_window = self.tex_window_word();
        let make = |v: (i32, i32), uv: (u16, u16)| HwVertex {
            pos: [v.0 as i16, v.1 as i16],
            color,
            uv: [uv.0, uv.1],
            flags: prim_flags,
            tex_window,
        };
        self.push_vertex(kind, clip, make(v0, uv0));
        self.push_vertex(kind, clip, make(v1, uv1));
        self.push_vertex(kind, clip, make(v2, uv2));
    }

    // ----- Phase 3: shaded (Gouraud) tris + quads -----

    /// `0x30..=0x33` -- Gouraud-shaded triangle. The fragment shader
    /// interpolates `color` linearly across the tri; we just push
    /// three different vertex colours.
    fn emit_shaded_tri(&mut self, cmd: u32, v: [(i32, i32); 3], colors: [u32; 3]) {
        let kind = self.blend_kind(cmd);
        let [v0, v1, v2] = v;
        let [c0, c1, c2] = colors.map(mono_color_rgba8);
        if self.wireframe {
            self.push_wire_tri(v0, c0, v1, c1, v2, c2, kind);
        } else {
            self.push_shaded_tri(v0, c0, v1, c1, v2, c2, self.dither_flag(), kind);
        }
    }

    /// `0x38..=0x3B` -- Gouraud-shaded quad.
    fn emit_shaded_quad(&mut self, cmd: u32, v: [(i32, i32); 4], colors: [u32; 4]) {
        let kind = self.blend_kind(cmd);
        let [v0, v1, v2, v3] = v;
        let [c0, c1, c2, c3] = colors.map(mono_color_rgba8);
        if self.wireframe {
            self.push_wire_tri(v1, c1, v3, c3, v2, c2, kind);
            self.push_wire_tri(v0, c0, v1, c1, v2, c2, kind);
        } else {
            let dither = self.dither_flag();
            self.push_shaded_tri(v1, c1, v3, c3, v2, c2, dither, kind);
            self.push_shaded_tri(v0, c0, v1, c1, v2, c2, dither, kind);
        }
    }

    /// `0x34..=0x37` -- Gouraud + textured triangle.
    fn emit_shaded_tex_tri(
        &mut self,
        cmd: u32,
        v: [(i32, i32); 3],
        uv: [(u8, u8); 3],
        colors: [u32; 3],
        clut: (u32, u32),
    ) {
        let [v0, v1, v2] = v;
        let [uv0, uv1, uv2] = uv;
        let [c0, c1, c2] = colors.map(mono_color_rgba8);
        let prim_flags = self.tex_prim_flags(cmd, clut, false);
        let kind = self.blend_kind(cmd);
        if self.wireframe {
            self.push_wire_tri(v0, c0, v1, c1, v2, c2, BlendKind::Opaque);
        } else {
            self.push_tex_tri_shaded_psx(
                v0,
                uv16(uv0),
                c0,
                v1,
                uv16(uv1),
                c1,
                v2,
                uv16(uv2),
                c2,
                prim_flags,
                kind,
            );
        }
    }

    /// `0x3C..=0x3F` -- Gouraud + textured quad.
    fn emit_shaded_tex_quad(
        &mut self,
        cmd: u32,
        v: [(i32, i32); 4],
        uv: [(u8, u8); 4],
        colors: [u32; 4],
        clut: (u32, u32),
    ) {
        let [v0, v1, v2, v3] = v;
        let [uv0, uv1, uv2, uv3] = uv;
        let [c0, c1, c2, c3] = colors.map(mono_color_rgba8);
        let prim_flags = self.tex_prim_flags(cmd, clut, false);
        let kind = self.blend_kind(cmd);
        if self.wireframe {
            self.push_wire_tri(v1, c1, v3, c3, v2, c2, BlendKind::Opaque);
            self.push_wire_tri(v0, c0, v1, c1, v2, c2, BlendKind::Opaque);
        } else {
            self.push_tex_tri_shaded_psx(
                v1,
                uv16(uv1),
                c1,
                v3,
                uv16(uv3),
                c3,
                v2,
                uv16(uv2),
                c2,
                prim_flags,
                kind,
            );
            self.push_tex_tri_shaded_psx(
                v0,
                uv16(uv0),
                c0,
                v1,
                uv16(uv1),
                c1,
                v2,
                uv16(uv2),
                c2,
                prim_flags,
                kind,
            );
        }
    }

    fn push_shaded_tri(
        &mut self,
        v0: (i32, i32),
        c0: [u8; 4],
        v1: (i32, i32),
        c1: [u8; 4],
        v2: (i32, i32),
        c2: [u8; 4],
        prim_flags: u32,
        kind: BlendKind,
    ) {
        let clip = self.current_clip();
        let make = |v: (i32, i32), c: [u8; 4]| HwVertex {
            pos: [v.0 as i16, v.1 as i16],
            color: c,
            uv: [0, 0],
            flags: prim_flags,
            tex_window: 0,
        };
        self.push_vertex(kind, clip, make(v0, c0));
        self.push_vertex(kind, clip, make(v1, c1));
        self.push_vertex(kind, clip, make(v2, c2));
    }

    #[allow(clippy::too_many_arguments)]
    fn push_tex_tri_shaded_psx(
        &mut self,
        v0: (i32, i32),
        uv0: (u16, u16),
        c0: [u8; 4],
        v1: (i32, i32),
        uv1: (u16, u16),
        c1: [u8; 4],
        v2: (i32, i32),
        uv2: (u16, u16),
        c2: [u8; 4],
        prim_flags: u32,
        kind: BlendKind,
    ) {
        if kind == BlendKind::Opaque {
            self.push_tex_tri_shaded(v0, uv0, c0, v1, uv1, c1, v2, uv2, c2, prim_flags, kind);
            return;
        }

        self.push_tex_tri_shaded(
            v0,
            uv0,
            c0,
            v1,
            uv1,
            c1,
            v2,
            uv2,
            c2,
            prim_flags | fbits::TEX_OPAQUE_PASS,
            BlendKind::Opaque,
        );
        self.push_tex_tri_shaded(
            v0,
            uv0,
            c0,
            v1,
            uv1,
            c1,
            v2,
            uv2,
            c2,
            prim_flags | fbits::TEX_SEMI_PASS,
            kind,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn push_tex_tri_shaded(
        &mut self,
        v0: (i32, i32),
        uv0: (u16, u16),
        c0: [u8; 4],
        v1: (i32, i32),
        uv1: (u16, u16),
        c1: [u8; 4],
        v2: (i32, i32),
        uv2: (u16, u16),
        c2: [u8; 4],
        prim_flags: u32,
        kind: BlendKind,
    ) {
        let clip = self.current_clip();
        let tex_window = self.tex_window_word();
        let make = |v: (i32, i32), uv: (u16, u16), c: [u8; 4]| HwVertex {
            pos: [v.0 as i16, v.1 as i16],
            color: c,
            uv: [uv.0, uv.1],
            flags: prim_flags,
            tex_window,
        };
        self.push_vertex(kind, clip, make(v0, uv0, c0));
        self.push_vertex(kind, clip, make(v1, uv1, c1));
        self.push_vertex(kind, clip, make(v2, uv2, c2));
    }

    // ----- Phase 3: rectangles -----

    /// `0x02` -- fill rectangle. Clears a VRAM region to a solid
    /// colour. Bypasses `draw_offset` (XY is absolute VRAM coords)
    /// and `draw_area` (always opaque, ignores scissor). Most demos
    /// emit one per frame as their clear-screen primitive -- without
    /// this the HW target keeps stale pixels everywhere the game
    /// hasn't redrawn this frame.
    ///
    /// Packet: `[cmd+rgb24, xy, wh]`
    /// - xy: 10-bit x in low 16, 9-bit y in high 16 (no sign extend)
    /// - wh: 10-bit w in low 16, 9-bit h in high 16
    /// - color: low 24 bits of cmd word
    fn emit_fill_rect(&mut self, cmd: u32, x: u32, y: u32, w: u32, h: u32) {
        let (x, y, w, h) = (x as i32, y as i32, w as i32, h as i32);
        if w <= 0 || h <= 0 {
            return;
        }
        let color = mono_color_rgba8(cmd);
        // Always opaque, regardless of state -- fills aren't blended.
        let clip = full_clip();
        let make = |vx: i32, vy: i32| HwVertex {
            pos: [vx as i16, vy as i16],
            color,
            uv: [0, 0],
            flags: 0,
            tex_window: 0,
        };
        // Two tris covering [x..x+w] × [y..y+h]. Same winding as
        // push_mono_rect -- semi-trans / mask-bit behaviour stays
        // pixel-equivalent in later phases.
        let v00 = (x, y);
        let v10 = (x + w, y);
        let v01 = (x, y + h);
        let v11 = (x + w, y + h);
        for v in [v00, v10, v01] {
            self.push_vertex(BlendKind::Opaque, clip, make(v.0, v.1));
        }
        for v in [v10, v11, v01] {
            self.push_vertex(BlendKind::Opaque, clip, make(v.0, v.1));
        }
    }

    fn push_mono_rect(&mut self, cmd: u32, x: i32, y: i32, w: i32, h: i32) {
        if w <= 0 || h <= 0 {
            return;
        }
        let color = mono_color_rgba8(cmd);
        let kind = self.blend_kind(cmd);
        let v00 = (x, y);
        let v10 = (x + w, y);
        let v01 = (x, y + h);
        let v11 = (x + w, y + h);
        self.push_tri(v00, v10, v01, color, kind);
        self.push_tri(v10, v11, v01, color, kind);
    }

    fn push_tex_rect(
        &mut self,
        cmd: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        uv0: (u8, u8),
        clut: (u32, u32),
    ) {
        if w <= 0 || h <= 0 {
            return;
        }
        let color = tex_tint(cmd);
        let prim_flags = self.tex_prim_flags(cmd, clut, true);
        let kind = self.blend_kind(cmd);
        // Sprite UV counters are 8-bit on PS1 and wrap per pixel.
        // Host-GPU interpolation cannot represent a 255→0 jump
        // inside one quad, so split at U/V wrap boundaries. E1's
        // rectangle flip bits reverse the counter direction.
        let u0 = uv0.0 as i32;
        let v0 = uv0.1 as i32;
        let flip_x = self.interp.state.flip_x;
        let flip_y = self.interp.state.flip_y;
        let mut dy = 0;
        while dy < h {
            let src_y = if flip_y { v0 - dy } else { v0 + dy };
            let y_phase = src_y & 0xFF;
            let chunk_h = (h - dy).min(if flip_y { y_phase + 1 } else { 256 - y_phase });
            let (uv_top_v, uv_bottom_v) = if flip_y {
                ((y_phase + 1) as u16, (y_phase + 1 - chunk_h) as u16)
            } else {
                (y_phase as u16, (y_phase + chunk_h) as u16)
            };

            let mut dx = 0;
            while dx < w {
                let src_x = if flip_x { u0 + 1 - dx } else { u0 + dx };
                let x_phase = src_x & 0xFF;
                let chunk_w = (w - dx).min(if flip_x { x_phase + 1 } else { 256 - x_phase });
                let (uv_left_u, uv_right_u) = if flip_x {
                    ((x_phase + 1) as u16, (x_phase + 1 - chunk_w) as u16)
                } else {
                    (x_phase as u16, (x_phase + chunk_w) as u16)
                };

                let p_a = (x + dx, y + dy);
                let p_b = (x + dx + chunk_w, y + dy);
                let p_c = (x + dx, y + dy + chunk_h);
                let p_d = (x + dx + chunk_w, y + dy + chunk_h);
                let uv_a = (uv_left_u, uv_top_v);
                let uv_b = (uv_right_u, uv_top_v);
                let uv_c = (uv_left_u, uv_bottom_v);
                let uv_d = (uv_right_u, uv_bottom_v);
                self.push_tex_tri_psx(p_a, uv_a, p_b, uv_b, p_c, uv_c, color, prim_flags, kind);
                self.push_tex_tri_psx(p_b, uv_b, p_d, uv_d, p_c, uv_c, color, prim_flags, kind);
                dx += chunk_w;
            }
            dy += chunk_h;
        }
    }
}

fn full_clip() -> [u16; 4] {
    [
        0,
        0,
        (crate::target::VRAM_WIDTH - 1) as u16,
        (crate::target::VRAM_HEIGHT - 1) as u16,
    ]
}

impl Default for Translator {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert a GP0 cmd word's low 24 bits into an RGBA8 tuple. PSX
/// writes BGR15 to VRAM; the CPU rasterizer goes via
/// `rgb24_to_bgr15` then `plot_pixel`, which is what shows up on
/// screen. For the HW renderer we keep RGB8 because the output
/// texture is `Rgba8UnormSrgb` -- the channel reduction to 5 bits
/// happens on the CPU side, the HW side renders the full-precision
/// RGB. Phase 7 may add a knob to clamp to PSX 5-bit precision for
/// strict-look games.
fn mono_color_rgba8(cmd: u32) -> [u8; 4] {
    let (r, g, b) = decode_tint(cmd & 0x00FF_FFFF);
    let _ = rgb24_to_bgr15; // imported for future BGR15 round-trip
    [r, g, b, 0xFF]
}

/// Tint colour for a textured primitive. Same layout as the mono
/// case (low 24 bits of `cmd`). Used as the modulator on top of
/// the sampled texel; raw-texture mode skips this and uses
/// `(0x80, 0x80, 0x80)` semantics, but we leave the original
/// tint here and let the shader decide whether to modulate
/// based on the `RAW_TEXTURE` flag bit. Keeps the vertex format
/// uniform across raw / non-raw primitives.
fn tex_tint(cmd: u32) -> [u8; 4] {
    let (r, g, b) = decode_tint(cmd & 0x00FF_FFFF);
    [r, g, b, 0xFF]
}

fn uv16(uv: (u8, u8)) -> (u16, u16) {
    (uv.0 as u16, uv.1 as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use emulator_core::gpu::GpuCmdLogEntry;

    fn entry(opcode: u8, fifo: Vec<u32>) -> GpuCmdLogEntry {
        GpuCmdLogEntry {
            index: 0,
            opcode,
            fifo,
        }
    }

    fn xy(x: u16, y: u16) -> u32 {
        u32::from(x) | (u32::from(y) << 16)
    }

    fn uv(u: u8, v: u8, high: u16) -> u32 {
        u32::from(u) | (u32::from(v) << 8) | (u32::from(high) << 16)
    }

    /// GP0(E1) with dither on, then one primitive. Returns the flag
    /// word every emitted vertex carries.
    fn dithered_prim_flags(opcode: u8, fifo: Vec<u32>) -> u32 {
        let log = [entry(0xE1, vec![0xE100_0200]), entry(opcode, fifo)];
        let mut translator = Translator::new();
        let frame = translator.translate(&log);
        assert!(frame.total() > 0, "primitive emitted nothing");
        let flags = frame.vertices[0].flags;
        for v in frame.vertices {
            assert_eq!(v.flags, flags, "flags must be uniform across the prim");
        }
        flags
    }

    /// Which primitives inherit GP0(E1) bit 9. The CPU rasterizer's
    /// rule is not "dither unless raw": for flat-tint textured prims it
    /// gates on `tint != RAW_TEXTURE_TINT`, and that sentinel is the
    /// identity tint 0x808080, so an unmodulated non-raw sprite skips
    /// dither too. Shaded-textured prims have per-vertex tints and no
    /// sentinel, so they gate on `!raw` alone. Getting this wrong
    /// dithers (or fails to dither) whole classes of sprite against the
    /// CPU backend.
    #[test]
    fn dither_flag_follows_the_cpu_per_primitive_rule() {
        let tri = |tint: u32| vec![tint, xy(10, 10), xy(20, 10), xy(10, 20)];
        let tex_tri = |cmd: u32| {
            vec![
                cmd,
                xy(10, 10),
                uv(0, 0, 0),
                xy(20, 10),
                uv(8, 0, 0),
                xy(10, 20),
                uv(0, 8, 0),
            ]
        };

        // Gouraud + mono: colour-only prims. The CPU dithers the
        // Gouraud walker and leaves flat mono on the undithered path.
        assert_ne!(
            dithered_prim_flags(
                0x30,
                vec![
                    0x3040_8040,
                    xy(10, 10),
                    0x00C0_40C0,
                    xy(20, 10),
                    0x0080_C080,
                    xy(10, 20)
                ]
            ) & fbits::DITHER,
            0,
            "Gouraud triangle must dither"
        );
        assert_eq!(
            dithered_prim_flags(0x20, tri(0x2040_8040)) & fbits::DITHER,
            0,
            "flat mono triangle must not dither"
        );

        // Flat-tint textured: dither unless raw or identity-tinted.
        assert_ne!(
            dithered_prim_flags(0x24, tex_tri(0x2440_8040)) & fbits::DITHER,
            0,
            "tint-modulated textured triangle must dither"
        );
        assert_eq!(
            dithered_prim_flags(0x24, tex_tri(0x2480_8080)) & fbits::DITHER,
            0,
            "identity tint hits the CPU's RAW_TEXTURE_TINT sentinel"
        );
        assert_eq!(
            dithered_prim_flags(0x25, tex_tri(0x2540_8040)) & fbits::DITHER,
            0,
            "raw-texture triangle must not dither"
        );

        // Shaded-textured: no sentinel, so the identity tint still
        // dithers; the raw bit still suppresses it.
        let shaded_tex = |cmd: u32| {
            vec![
                cmd,
                xy(10, 10),
                uv(0, 0, 0),
                0x0080_8080,
                xy(20, 10),
                uv(8, 0, 0),
                0x0080_8080,
                xy(10, 20),
                uv(0, 8, 0),
            ]
        };
        assert_ne!(
            dithered_prim_flags(0x34, shaded_tex(0x3480_8080)) & fbits::DITHER,
            0,
            "shaded-textured triangle dithers even at the identity tint"
        );
        assert_eq!(
            dithered_prim_flags(0x35, shaded_tex(0x3580_8080)) & fbits::DITHER,
            0,
            "raw shaded-textured triangle must not dither"
        );
    }

    /// Without GP0(E1) bit 9 nothing carries the flag -- guards against
    /// a fix that just turns dither on unconditionally.
    #[test]
    fn no_dither_flag_when_e1_bit_9_is_clear() {
        let log = [
            entry(0xE1, vec![0xE100_0000]),
            entry(
                0x30,
                vec![
                    0x3040_8040,
                    xy(10, 10),
                    0x00C0_40C0,
                    xy(20, 10),
                    0x0080_C080,
                    xy(10, 20),
                ],
            ),
        ];
        let mut translator = Translator::new();
        let frame = translator.translate(&log);
        assert!(frame.total() > 0);
        for v in frame.vertices {
            assert_eq!(v.flags & fbits::DITHER, 0);
        }
    }

    #[test]
    fn opaque_textured_tri_emits_one_opaque_pass() {
        let log = [entry(
            0x24,
            vec![
                0x2480_8080,
                xy(10, 10),
                uv(0, 0, 0),
                xy(20, 10),
                uv(8, 0, 0),
                xy(10, 20),
                uv(0, 8, 0),
            ],
        )];
        let mut translator = Translator::new();
        let frame = translator.translate(&log);

        assert_eq!(frame.total(), 3);
        assert_eq!(frame.runs.len(), 1);
        assert_eq!(frame.runs[0].kind, BlendKind::Opaque);
        assert_eq!(frame.runs[0].count, 3);
        for v in frame.vertices {
            assert_eq!(v.flags & fbits::TEX_OPAQUE_PASS, 0);
            assert_eq!(v.flags & fbits::TEX_SEMI_PASS, 0);
        }
    }

    #[test]
    fn textured_quad_uses_cpu_redux_split_order() {
        let log = [entry(
            0x2C,
            vec![
                0x2C80_8080,
                xy(10, 10),
                uv(1, 2, 0),
                xy(20, 10),
                uv(11, 2, 0),
                xy(10, 20),
                uv(1, 12, 0),
                xy(20, 20),
                uv(11, 12, 0),
            ],
        )];
        let mut translator = Translator::new();
        let frame = translator.translate(&log);

        assert_eq!(frame.total(), 6);
        let actual: Vec<([i16; 2], [u16; 2])> =
            frame.vertices.iter().map(|v| (v.pos, v.uv)).collect();
        assert_eq!(
            actual,
            vec![
                ([20, 10], [11, 2]),
                ([20, 20], [11, 12]),
                ([10, 20], [1, 12]),
                ([10, 10], [1, 2]),
                ([20, 10], [11, 2]),
                ([10, 20], [1, 12]),
            ]
        );
    }

    #[test]
    fn textured_tri_carries_active_texture_window_to_vertices() {
        let log = [
            entry(0xE2, vec![0xE204_2318]),
            entry(
                0x24,
                vec![
                    0x2480_8080,
                    xy(10, 10),
                    uv(0, 0, 0),
                    xy(20, 10),
                    uv(64, 0, 0),
                    xy(10, 20),
                    uv(0, 128, 0),
                ],
            ),
        ];
        let mut translator = Translator::new();
        let frame = translator.translate(&log);

        assert_eq!(frame.total(), 3);
        for v in frame.vertices {
            assert_eq!(v.tex_window, 0x4040_C0C0);
        }
    }

    #[test]
    fn textured_rect_splits_at_u_wrap_boundary() {
        let log = [entry(
            0x65,
            vec![0x6580_8080, xy(10, 20), uv(250, 0, 0), xy(20, 1)],
        )];
        let mut translator = Translator::new();
        let frame = translator.translate(&log);

        assert_eq!(frame.total(), 12);
        let actual: Vec<([i16; 2], [u16; 2])> =
            frame.vertices.iter().map(|v| (v.pos, v.uv)).collect();
        assert_eq!(
            &actual[0..6],
            &[
                ([10, 20], [250, 0]),
                ([16, 20], [256, 0]),
                ([10, 21], [250, 1]),
                ([16, 20], [256, 0]),
                ([16, 21], [256, 1]),
                ([10, 21], [250, 1]),
            ]
        );
        assert_eq!(
            &actual[6..12],
            &[
                ([16, 20], [0, 0]),
                ([30, 20], [14, 0]),
                ([16, 21], [0, 1]),
                ([30, 20], [14, 0]),
                ([30, 21], [14, 1]),
                ([16, 21], [0, 1]),
            ]
        );
    }

    #[test]
    fn textured_rect_honors_draw_mode_x_flip() {
        let log = [
            entry(0xE1, vec![0xE100_1000]),
            entry(0x65, vec![0x6580_8080, xy(10, 20), uv(0, 0, 0), xy(20, 1)]),
        ];
        let mut translator = Translator::new();
        let frame = translator.translate(&log);

        // Silicon starts at u0+1 then counts down. Starting at zero
        // therefore wraps after two pixels and needs two host quads.
        assert_eq!(frame.total(), 12);
        let actual: Vec<([i16; 2], [u16; 2])> =
            frame.vertices.iter().map(|v| (v.pos, v.uv)).collect();
        assert_eq!(
            actual,
            vec![
                ([10, 20], [2, 0]),
                ([12, 20], [0, 0]),
                ([10, 21], [2, 1]),
                ([12, 20], [0, 0]),
                ([12, 21], [0, 1]),
                ([10, 21], [2, 1]),
                ([12, 20], [256, 0]),
                ([30, 20], [238, 0]),
                ([12, 21], [256, 1]),
                ([30, 20], [238, 0]),
                ([30, 21], [238, 1]),
                ([12, 21], [256, 1]),
            ]
        );
    }

    #[test]
    fn draw_mode_preserves_active_texture_window() {
        let log = [
            entry(0xE2, vec![0xE204_2318]),
            entry(0xE1, vec![0xE100_0020]),
            entry(
                0x24,
                vec![
                    0x2480_8080,
                    xy(10, 10),
                    uv(0, 0, 0),
                    xy(20, 10),
                    uv(64, 0, 0),
                    xy(10, 20),
                    uv(0, 128, 0),
                ],
            ),
        ];
        let mut translator = Translator::new();
        let frame = translator.translate(&log);

        assert_eq!(frame.total(), 3);
        for v in frame.vertices {
            assert_eq!(v.tex_window, 0x4040_C0C0);
        }
    }

    #[test]
    fn semi_trans_textured_tri_splits_opaque_and_stp_passes() {
        let log = [entry(
            0x26,
            vec![
                0x2680_8080,
                xy(10, 10),
                uv(0, 0, 0),
                xy(20, 10),
                uv(8, 0, 0),
                xy(10, 20),
                uv(0, 8, 0),
            ],
        )];
        let mut translator = Translator::new();
        let frame = translator.translate(&log);

        assert_eq!(frame.total(), 6);
        assert_eq!(frame.runs.len(), 2);
        assert_eq!(frame.runs[0].kind, BlendKind::Opaque);
        assert_eq!(frame.runs[0].start, 0);
        assert_eq!(frame.runs[0].count, 3);
        assert_eq!(frame.runs[1].kind, BlendKind::Average);
        assert_eq!(frame.runs[1].start, 3);
        assert_eq!(frame.runs[1].count, 3);

        for v in &frame.vertices[0..3] {
            assert_ne!(v.flags & fbits::SEMI_TRANS, 0);
            assert_ne!(v.flags & fbits::TEX_OPAQUE_PASS, 0);
            assert_eq!(v.flags & fbits::TEX_SEMI_PASS, 0);
        }
        for v in &frame.vertices[3..6] {
            assert_ne!(v.flags & fbits::SEMI_TRANS, 0);
            assert_eq!(v.flags & fbits::TEX_OPAQUE_PASS, 0);
            assert_ne!(v.flags & fbits::TEX_SEMI_PASS, 0);
        }
    }

    #[test]
    fn wireframe_mono_tri_emits_edge_strips() {
        let log = [entry(
            0x20,
            vec![0x20FF_FFFF, xy(10, 10), xy(20, 10), xy(10, 20)],
        )];
        let mut translator = Translator::new();
        let frame = translator.translate_with_wireframe(&log, true);

        // 3 edge strips x 6 verts; interiors are transparent (stale-edge
        // cleanup is the CPU-side journal, not a fill).
        assert_eq!(frame.total(), 18);
        assert_eq!(frame.runs.len(), 1);
        assert_eq!(frame.runs[0].kind, BlendKind::Opaque);
        assert_eq!(frame.runs[0].count, 18);
        for v in frame.vertices {
            assert_eq!(v.flags, 0);
        }
    }

    #[test]
    fn wireframe_textured_tri_ignores_texture_and_stays_opaque() {
        let log = [entry(
            0x26,
            vec![
                0x2680_8080,
                xy(10, 10),
                uv(0, 0, 0),
                xy(20, 10),
                uv(8, 0, 0),
                xy(10, 20),
                uv(0, 8, 0),
            ],
        )];
        let mut translator = Translator::new();
        let frame = translator.translate_with_wireframe(&log, true);

        // 3 edge strips x 6 verts; texture dropped, edges opaque.
        assert_eq!(frame.total(), 18);
        assert_eq!(frame.runs.len(), 1);
        assert_eq!(frame.runs[0].kind, BlendKind::Opaque);
        assert_eq!(frame.runs[0].count, 18);
        for v in frame.vertices {
            assert_eq!(v.flags, 0);
        }
    }

    #[test]
    fn wireframe_leaves_rectangles_filled() {
        let log = [entry(0x60, vec![0x60FF_FFFF, xy(10, 10), xy(16, 16)])];
        let mut translator = Translator::new();
        let frame = translator.translate_with_wireframe(&log, true);

        assert_eq!(frame.total(), 6);
        assert_eq!(frame.runs.len(), 1);
        assert_eq!(frame.runs[0].kind, BlendKind::Opaque);
        assert_eq!(frame.runs[0].count, 6);
    }

    #[test]
    fn mono_line_emits_endpoint_extended_band() {
        // Horizontal line (10,20)->(30,20): x-major, the max-x end
        // extends by 1 so pixel-center sampling covers column 30
        // (the CPU walk plots x0..=x1 inclusive).
        let log = [entry(0x40, vec![0x40FF_FFFF, xy(10, 20), xy(30, 20)])];
        let mut translator = Translator::new();
        let frame = translator.translate(&log);

        assert_eq!(frame.total(), 6);
        assert_eq!(frame.runs.len(), 1);
        assert_eq!(frame.runs[0].kind, BlendKind::Opaque);
        let pos: Vec<[i16; 2]> = frame.vertices.iter().map(|v| v.pos).collect();
        assert_eq!(
            pos,
            vec![[10, 20], [31, 20], [10, 21], [31, 20], [31, 21], [10, 21],]
        );
        for v in frame.vertices {
            assert_eq!(v.color, [0xFF, 0xFF, 0xFF, 0xFF]);
            assert_eq!(v.flags, 0);
        }
    }

    #[test]
    fn mono_line_reversed_extends_the_max_end_not_the_start() {
        // Same segment drawn right-to-left: the +1 lands on the
        // max-x endpoint (the START here), producing the same band.
        let log = [entry(0x40, vec![0x40FF_FFFF, xy(30, 20), xy(10, 20)])];
        let mut translator = Translator::new();
        let frame = translator.translate(&log);

        let pos: Vec<[i16; 2]> = frame.vertices.iter().map(|v| v.pos).collect();
        assert_eq!(
            pos,
            vec![[31, 20], [10, 20], [31, 21], [10, 20], [10, 21], [31, 21],]
        );
    }

    #[test]
    fn vertical_line_band_is_one_pixel_wide() {
        let log = [entry(0x40, vec![0x40FF_FFFF, xy(5, 10), xy(5, 20)])];
        let mut translator = Translator::new();
        let frame = translator.translate(&log);

        let pos: Vec<[i16; 2]> = frame.vertices.iter().map(|v| v.pos).collect();
        assert_eq!(
            pos,
            vec![[5, 10], [5, 21], [6, 10], [5, 21], [6, 21], [6, 10]]
        );
    }

    #[test]
    fn zero_length_mono_line_plots_one_pixel() {
        let log = [entry(0x40, vec![0x40FF_FFFF, xy(10, 10), xy(10, 10)])];
        let mut translator = Translator::new();
        let frame = translator.translate(&log);

        assert_eq!(frame.total(), 6);
        let pos: Vec<[i16; 2]> = frame.vertices.iter().map(|v| v.pos).collect();
        assert_eq!(
            pos,
            vec![[10, 10], [11, 10], [10, 11], [11, 10], [11, 11], [10, 11]]
        );
    }

    #[test]
    fn zero_length_shaded_line_plots_one_pixel() {
        // CPU shaded walker plots one pixel then breaks.
        let log = [entry(
            0x50,
            vec![0x5000_00FF, xy(10, 10), 0x0000_FF00, xy(10, 10)],
        )];
        let mut translator = Translator::new();
        let frame = translator.translate(&log);

        assert_eq!(frame.total(), 6);
        let pos: Vec<[i16; 2]> = frame.vertices.iter().map(|v| v.pos).collect();
        assert_eq!(
            pos,
            vec![[10, 10], [11, 10], [10, 11], [11, 10], [11, 11], [10, 11]]
        );
        for v in frame.vertices {
            assert_eq!(v.color, [0xFF, 0x00, 0x00, 0xFF]);
        }
    }

    #[test]
    fn shaded_line_carries_endpoint_colors() {
        let log = [entry(
            0x50,
            vec![0x5000_00FF, xy(0, 0), 0x0000_FF00, xy(0, 10)],
        )];
        let mut translator = Translator::new();
        let frame = translator.translate(&log);

        assert_eq!(frame.total(), 6);
        // y-major band; per-vertex colours follow the endpoints.
        let cv: Vec<([i16; 2], [u8; 4])> =
            frame.vertices.iter().map(|v| (v.pos, v.color)).collect();
        assert_eq!(
            cv,
            vec![
                ([0, 0], [0xFF, 0x00, 0x00, 0xFF]),
                ([0, 11], [0x00, 0xFF, 0x00, 0xFF]),
                ([1, 0], [0xFF, 0x00, 0x00, 0xFF]),
                ([0, 11], [0x00, 0xFF, 0x00, 0xFF]),
                ([1, 11], [0x00, 0xFF, 0x00, 0xFF]),
                ([1, 0], [0xFF, 0x00, 0x00, 0xFF]),
            ]
        );
    }

    #[test]
    fn semi_trans_line_routes_through_tpage_blend_kind() {
        // E1 selects Add (bits 5-6 = 1); cmd bit 25 arms semi-trans.
        let log = [
            entry(0xE1, vec![0xE100_0020]),
            entry(0x42, vec![0x42FF_FFFF, xy(0, 0), xy(10, 0)]),
        ];
        let mut translator = Translator::new();
        let frame = translator.translate(&log);

        assert_eq!(frame.runs.len(), 1);
        assert_eq!(frame.runs[0].kind, BlendKind::Add);
    }

    #[test]
    fn mono_polyline_emits_one_band_per_segment() {
        let log = [entry(
            0x48,
            vec![
                0x48FF_FFFF,
                xy(0, 0),
                xy(10, 0),
                xy(10, 10), // continuation vertex from the cmd_log capture
            ],
        )];
        let mut translator = Translator::new();
        let frame = translator.translate(&log);

        // Two segments x 6 vertices.
        assert_eq!(frame.total(), 12);
        assert_eq!(frame.runs.len(), 1);
        assert_eq!(frame.runs[0].count, 12);
    }
}
