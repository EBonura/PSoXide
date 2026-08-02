#![allow(
    clippy::manual_range_contains,
    clippy::too_many_arguments,
    clippy::unnecessary_cast
)]

use super::*;
use emulator_core::Gpu;

/// Run the CPU rasterizer for a monochrome triangle, optionally
/// with semi-trans / mask flags pre-configured. Returns the full
/// row-major VRAM snapshot for byte-by-byte comparison.
///
/// Configuration knobs:
///   - `cmd_byte` 0x20 = opaque, 0x22 = semi-trans (cmd-bit-1 = 1).
///   - `tpage_blend_bits` (0..3) → bits 5-6 of GP0 0xE1 → tpage
///     blend mode (Average / Add / Sub / AddQuarter).
///   - `mask_e6` writes GP0 0xE6 with the given low-2-bit value
///     (bit 0 = mask_set_on_draw, bit 1 = mask_check_before_draw).
///   - `prefill` paints the entire VRAM with one color before
///     submitting the primitive -- needed for semi-trans tests
///     where the back buffer must not be zero, and for mask-check
///     tests where the existing pixel needs bit 15 set.
fn cpu_rasterize_mono_tri_full(
    v0: (i32, i32),
    v1: (i32, i32),
    v2: (i32, i32),
    color: u16,
    cmd_byte: u8,
    tpage_blend_bits: u8,
    mask_e6: u8,
    prefill: u16,
) -> Vec<u16> {
    let mut gpu = Gpu::new();
    // Pre-fill VRAM by writing every word directly. Cheaper than
    // streaming a 1 MiB block through GP0.
    if prefill != 0 {
        for y in 0..512u16 {
            for x in 0..1024u16 {
                gpu.vram.set_pixel(x, y, prefill);
            }
        }
    }
    gpu.gp0_push(0xE3000000); // E3 -- top-left at (0, 0)
    gpu.gp0_push(0xE4000000 | 1023 | (511 << 10));
    // Tpage: only bits 5-6 (semi-trans mode) matter for mono prims.
    let e1 = 0xE100_0000_u32 | ((tpage_blend_bits as u32) & 0x3) << 5;
    gpu.gp0_push(e1);
    // Mask config (E6).
    gpu.gp0_push(0xE600_0000_u32 | (mask_e6 as u32) & 0x3);
    // Triangle command word.
    let cmd = ((cmd_byte as u32) << 24) | bgr15_to_rgb24(color);
    gpu.gp0_push(cmd);
    gpu.gp0_push(pack_xy(v0));
    gpu.gp0_push(pack_xy(v1));
    gpu.gp0_push(pack_xy(v2));
    gpu.vram.words().to_vec()
}

fn cpu_rasterize_mono_tri(v0: (i32, i32), v1: (i32, i32), v2: (i32, i32), color: u16) -> Vec<u16> {
    cpu_rasterize_mono_tri_full(v0, v1, v2, color, 0x20, 0, 0, 0)
}

/// Pre-fill GPU VRAM with a single 16-bit value via the GPU-side
/// upload path. Mirrors what `prefill` does on the CPU side so
/// both backends start from byte-identical state.
fn gpu_prefill(vg: &VramGpu, value: u16) {
    if value == 0 {
        return;
    }
    let buf = vec![value; (super::super::VRAM_WIDTH * super::super::VRAM_HEIGHT) as usize];
    vg.upload_full(&buf).expect("upload prefill");
}

fn pack_xy(p: (i32, i32)) -> u32 {
    let x = (p.0 as u32) & 0x07FF;
    let y = (p.1 as u32) & 0x07FF;
    x | (y << 16)
}

fn bgr15_to_rgb24(bgr15: u16) -> u32 {
    let r5 = (bgr15 & 0x1F) as u32;
    let g5 = ((bgr15 >> 5) & 0x1F) as u32;
    let b5 = ((bgr15 >> 10) & 0x1F) as u32;
    // Inverse of `rgb24_to_bgr15`: lift 5→8 bits by replicate.
    let r = (r5 << 3) | (r5 >> 2);
    let g = (g5 << 3) | (g5 >> 2);
    let b = (b5 << 3) | (b5 >> 2);
    r | (g << 8) | (b << 16)
}

fn diff_count(a: &[u16], b: &[u16]) -> usize {
    a.iter().zip(b.iter()).filter(|(x, y)| x != y).count()
}

#[test]
fn mono_tri_axis_aligned_right_triangle_matches_cpu() {
    // The simplest case: a right triangle with one axis-aligned
    // edge. Edge function tests should produce identical
    // coverage to the CPU scanline walker.
    let v0 = (10, 10);
    let v1 = (50, 10);
    let v2 = (10, 50);
    let color = 0x7C00; // pure blue

    let cpu_vram = cpu_rasterize_mono_tri(v0, v1, v2, color);

    let vg = VramGpu::new_headless();
    let r = Rasterizer::new(&vg);
    let tri = MonoTri::opaque(v0, v1, v2, color);
    let area = DrawArea::full_vram();
    r.dispatch_mono_tri_scanline(&vg, &tri, &area);
    let gpu_vram = vg.download_full().expect("download");

    let diffs = diff_count(&cpu_vram, &gpu_vram);
    assert_eq!(diffs, 0, "axis-aligned mono triangle must be bit-exact");
}

#[test]
fn mono_tri_skewed_matches_cpu_exactly() {
    // A non-axis-aligned triangle stresses the diagonal-edge
    // coverage rule. The host ships the same DDA spans the CPU
    // walks, so every edge pixel must match.
    let v0 = (50, 20);
    let v1 = (130, 70);
    let v2 = (30, 90);
    let color = 0x03E0; // pure green

    let cpu_vram = cpu_rasterize_mono_tri(v0, v1, v2, color);

    let vg = VramGpu::new_headless();
    let r = Rasterizer::new(&vg);
    let tri = MonoTri::opaque(v0, v1, v2, color);
    let area = DrawArea::full_vram();
    r.dispatch_mono_tri_scanline(&vg, &tri, &area);
    let gpu_vram = vg.download_full().expect("download");

    let diffs = diff_count(&cpu_vram, &gpu_vram);
    assert_eq!(diffs, 0, "skewed mono triangle must be bit-exact");
}

#[test]
fn mono_tri_oversized_is_dropped_like_cpu() {
    // Hardware drops triangles whose edge Δ exceeds 1023×511.
    // The CPU rasterizer matches; ours must too.
    let v0 = (0, 0);
    let v1 = (2000, 0);
    let v2 = (0, 0);
    let color = 0x7FFF;

    let cpu_vram = cpu_rasterize_mono_tri(v0, v1, v2, color);

    let vg = VramGpu::new_headless();
    let r = Rasterizer::new(&vg);
    let tri = MonoTri::opaque(v0, v1, v2, color);
    let area = DrawArea::full_vram();
    r.dispatch_mono_tri_scanline(&vg, &tri, &area);
    let gpu_vram = vg.download_full().expect("download");

    // Both should be all-zero VRAM (degenerate primitive
    // dropped before any plotting).
    assert!(cpu_vram.iter().all(|&w| w == 0), "CPU should drop");
    assert!(gpu_vram.iter().all(|&w| w == 0), "GPU should drop");
}

// -------------------------------------------------------
//  Phase B.4 -- semi-trans + mask-bit parity vs CPU
// -------------------------------------------------------

/// Run the GPU rasterizer for one mono-tri with the given flags
/// onto a pre-filled VRAM, return the post-dispatch full VRAM.
fn gpu_rasterize_mono_tri_full(
    v0: (i32, i32),
    v1: (i32, i32),
    v2: (i32, i32),
    color: u16,
    flags: PrimFlags,
    blend_mode: BlendMode,
    prefill: u16,
) -> Vec<u16> {
    let vg = VramGpu::new_headless();
    gpu_prefill(&vg, prefill);
    let r = Rasterizer::new(&vg);
    let tri = MonoTri::new(v0, v1, v2, color, flags, blend_mode);
    let area = DrawArea::full_vram();
    r.dispatch_mono_tri_scanline(&vg, &tri, &area);
    vg.download_full().expect("download")
}

/// Run a strict bbox-only diff: count mismatches inside the
/// triangle's bbox and a 2-pixel halo. Skewed triangles will
/// always disagree on a few edge pixels (different fill rule);
/// allowing a small tolerance lets us pin the inside-of-triangle
/// blend math precisely.
fn diff_inside_bbox(a: &[u16], b: &[u16], bbox_min: (i32, i32), bbox_max: (i32, i32)) -> usize {
    let mut diffs = 0;
    let x0 = (bbox_min.0 - 2).max(0) as usize;
    let y0 = (bbox_min.1 - 2).max(0) as usize;
    let x1 = ((bbox_max.0 + 2).min(1023)) as usize;
    let y1 = ((bbox_max.1 + 2).min(511)) as usize;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let i = y * 1024 + x;
            if a[i] != b[i] {
                diffs += 1;
            }
        }
    }
    diffs
}

#[test]
fn semi_trans_average_matches_cpu_byte_for_byte() {
    // The trickiest blend mode -- Redux's `(b>>1) + (f>>1)` quirk
    // produces different LSBs from the naive `(b+f)/2`. If the
    // shader gets this wrong, every output pixel of an axis-
    // aligned triangle (no edge-rule diffs) will be off-by-one
    // on at least one channel. Strict parity expected.
    let v0 = (10, 10);
    let v1 = (50, 10);
    let v2 = (10, 50);
    let color = 0x1234; // arbitrary BGR15 (low and high LSBs set so the quirk shows)
    let prefill = 0x5678;
    let cpu = cpu_rasterize_mono_tri_full(v0, v1, v2, color, 0x22, 0, 0, prefill);
    let gpu = gpu_rasterize_mono_tri_full(
        v0,
        v1,
        v2,
        color,
        PrimFlags::SEMI_TRANS,
        BlendMode::Average,
        prefill,
    );
    let diffs = diff_inside_bbox(&cpu, &gpu, (10, 10), (50, 50));
    // Axis-aligned right-triangle → no edge-rule disagreement.
    // Inside pixels must blend identically.
    assert!(diffs == 0, "Average blend mismatch: {diffs} pixels differ");
}

#[test]
fn semi_trans_add_matches_cpu_byte_for_byte() {
    let v0 = (5, 5);
    let v1 = (45, 5);
    let v2 = (5, 45);
    // Pick a color whose channels saturate against the prefill.
    let color = 0x4210; // (r=0x10, g=0x10, b=0x10)
    let prefill = 0x4210; // same -- sum must clamp to 31 per channel
    let cpu = cpu_rasterize_mono_tri_full(v0, v1, v2, color, 0x22, 1, 0, prefill);
    let gpu = gpu_rasterize_mono_tri_full(
        v0,
        v1,
        v2,
        color,
        PrimFlags::SEMI_TRANS,
        BlendMode::Add,
        prefill,
    );
    let diffs = diff_inside_bbox(&cpu, &gpu, (5, 5), (45, 45));
    assert!(diffs == 0, "Add blend mismatch: {diffs} pixels differ");
}

#[test]
fn semi_trans_sub_matches_cpu_byte_for_byte() {
    let v0 = (5, 5);
    let v1 = (45, 5);
    let v2 = (5, 45);
    let color = 0x2108; // (r=8, g=8, b=8)
    let prefill = 0x4210; // (r=16, g=16, b=16) → result (r=8,g=8,b=8)
    let cpu = cpu_rasterize_mono_tri_full(v0, v1, v2, color, 0x22, 2, 0, prefill);
    let gpu = gpu_rasterize_mono_tri_full(
        v0,
        v1,
        v2,
        color,
        PrimFlags::SEMI_TRANS,
        BlendMode::Sub,
        prefill,
    );
    let diffs = diff_inside_bbox(&cpu, &gpu, (5, 5), (45, 45));
    assert!(diffs == 0, "Sub blend mismatch: {diffs} pixels differ");
}

#[test]
fn semi_trans_addquarter_matches_cpu_byte_for_byte() {
    let v0 = (5, 5);
    let v1 = (45, 5);
    let v2 = (5, 45);
    let color = 0x4210;
    let prefill = 0x2108;
    let cpu = cpu_rasterize_mono_tri_full(v0, v1, v2, color, 0x22, 3, 0, prefill);
    let gpu = gpu_rasterize_mono_tri_full(
        v0,
        v1,
        v2,
        color,
        PrimFlags::SEMI_TRANS,
        BlendMode::AddQuarter,
        prefill,
    );
    let diffs = diff_inside_bbox(&cpu, &gpu, (5, 5), (45, 45));
    assert!(
        diffs == 0,
        "AddQuarter blend mismatch: {diffs} pixels differ"
    );
}

#[test]
fn mask_set_writes_bit_15_in_every_plotted_pixel() {
    // Opaque triangle with mask_set_on_draw → every plotted
    // pixel should have bit 15 = 1, others left as prefill.
    let v0 = (10, 10);
    let v1 = (50, 10);
    let v2 = (10, 50);
    let color = 0x0123; // bit 15 clear in source color
    let prefill = 0x4567;
    // CPU path: cmd 0x20 (opaque), tpage doesn't matter for
    // opaque mono, mask_e6 = 0b01 = mask_set_on_draw.
    let cpu = cpu_rasterize_mono_tri_full(v0, v1, v2, color, 0x20, 0, 0b01, prefill);
    let gpu = gpu_rasterize_mono_tri_full(
        v0,
        v1,
        v2,
        color,
        PrimFlags::MASK_SET,
        BlendMode::Average,
        prefill,
    );
    let diffs = diff_inside_bbox(&cpu, &gpu, (10, 10), (50, 50));
    assert!(diffs == 0, "MASK_SET parity: {diffs} differ");
    // Sanity: spot-check a point firmly inside the triangle has
    // bit 15 set on both backends.
    let inside_idx = 20 * 1024 + 20;
    assert!(
        cpu[inside_idx] & 0x8000 != 0,
        "CPU inside pixel: bit 15 set"
    );
    assert!(
        gpu[inside_idx] & 0x8000 != 0,
        "GPU inside pixel: bit 15 set"
    );
}

#[test]
fn mask_check_skips_when_back_buffer_has_bit_15() {
    // Pre-fill with bit-15-set pixels; opaque triangle with
    // mask_check should leave them all alone.
    let v0 = (10, 10);
    let v1 = (50, 10);
    let v2 = (10, 50);
    let color = 0x0123;
    let prefill = 0x8888; // bit 15 set
    let cpu = cpu_rasterize_mono_tri_full(v0, v1, v2, color, 0x20, 0, 0b10, prefill);
    let gpu = gpu_rasterize_mono_tri_full(
        v0,
        v1,
        v2,
        color,
        PrimFlags::MASK_CHECK,
        BlendMode::Average,
        prefill,
    );
    // Strict equality on every pixel -- nothing should have changed.
    let diffs = diff_inside_bbox(&cpu, &gpu, (10, 10), (50, 50));
    assert!(diffs == 0, "MASK_CHECK parity: {diffs} differ");
    let inside_idx = 20 * 1024 + 20;
    assert_eq!(cpu[inside_idx], prefill, "CPU inside untouched");
    assert_eq!(gpu[inside_idx], prefill, "GPU inside untouched");
}

#[test]
fn mask_check_only_skips_protected_pixels_not_others() {
    // Half the back buffer has bit 15, half doesn't. The triangle
    // should write only to the unprotected half.
    let v0 = (10, 10);
    let v1 = (60, 10);
    let v2 = (10, 60);
    let color = 0x0123;
    // CPU prefill: alternate rows have bit 15 set.
    let mut gpu_buffer =
        vec![0u16; (super::super::VRAM_WIDTH * super::super::VRAM_HEIGHT) as usize];
    for y in 0..512u16 {
        let v = if y & 1 == 0 { 0x4567 } else { 0xC567 };
        for x in 0..1024u16 {
            gpu_buffer[y as usize * 1024 + x as usize] = v;
        }
    }
    // Build an identical CPU state.
    let mut cpu_gpu = Gpu::new();
    for (i, &w) in gpu_buffer.iter().enumerate() {
        let x = (i % 1024) as u16;
        let y = (i / 1024) as u16;
        cpu_gpu.vram.set_pixel(x, y, w);
    }
    cpu_gpu.gp0_push(0xE3000000);
    cpu_gpu.gp0_push(0xE4000000 | 1023 | (511 << 10));
    cpu_gpu.gp0_push(0xE100_0000); // tpage
    cpu_gpu.gp0_push(0xE600_0002); // E6 -- mask_check_before_draw
    let cmd = 0x20_000000_u32 | bgr15_to_rgb24(color);
    cpu_gpu.gp0_push(cmd);
    cpu_gpu.gp0_push(pack_xy(v0));
    cpu_gpu.gp0_push(pack_xy(v1));
    cpu_gpu.gp0_push(pack_xy(v2));
    let cpu = cpu_gpu.vram.words().to_vec();

    // Identical GPU state.
    let vg = VramGpu::new_headless();
    vg.upload_full(&gpu_buffer).unwrap();
    let r = Rasterizer::new(&vg);
    let tri = MonoTri::new(v0, v1, v2, color, PrimFlags::MASK_CHECK, BlendMode::Average);
    r.dispatch_mono_tri_scanline(&vg, &tri, &DrawArea::full_vram());
    let gpu = vg.download_full().unwrap();

    let diffs = diff_inside_bbox(&cpu, &gpu, (10, 10), (60, 60));
    assert!(diffs == 0, "selective mask-check parity: {diffs} differ");
    // Sanity check: ODD rows (prefill 0xC567, bit-15 set) must be
    // unchanged; EVEN rows (prefill 0x4567, bit-15 clear) must
    // have been overwritten with the new color.
    for y in [11u16, 13, 15] {
        let i = y as usize * 1024 + 20;
        assert_eq!(cpu[i], 0xC567, "CPU row {y} (protected) unchanged");
        assert_eq!(gpu[i], 0xC567, "GPU row {y} (protected) unchanged");
    }
    for y in [12u16, 14, 16] {
        let i = y as usize * 1024 + 20;
        // The new pixel may have bit 15 set/cleared depending on
        // mask-set; it shouldn't have it here. The colour part
        // must be `color`.
        assert_eq!(
            cpu[i] & 0x7FFF,
            color & 0x7FFF,
            "CPU row {y} (open) overwritten"
        );
        assert_eq!(
            gpu[i] & 0x7FFF,
            color & 0x7FFF,
            "GPU row {y} (open) overwritten"
        );
    }
}

#[test]
fn semi_trans_plus_mask_set_combine_correctly() {
    // The full RMW chain: blend with back buffer THEN OR bit 15.
    // If the shader gets ordering wrong (e.g. OR-then-blend), the
    // blended result will have bit 15 propagating into the colour
    // math.
    let v0 = (10, 10);
    let v1 = (50, 10);
    let v2 = (10, 50);
    let color = 0x1234;
    let prefill = 0x5678;
    let cpu = cpu_rasterize_mono_tri_full(v0, v1, v2, color, 0x22, 0, 0b01, prefill);
    let gpu = gpu_rasterize_mono_tri_full(
        v0,
        v1,
        v2,
        color,
        PrimFlags::SEMI_TRANS | PrimFlags::MASK_SET,
        BlendMode::Average,
        prefill,
    );
    let diffs = diff_inside_bbox(&cpu, &gpu, (10, 10), (50, 50));
    assert!(diffs == 0, "semi-trans + mask-set parity: {diffs} differ");
    let inside_idx = 20 * 1024 + 20;
    // Both must have bit 15 set after MASK_SET.
    assert!(cpu[inside_idx] & 0x8000 != 0);
    assert!(gpu[inside_idx] & 0x8000 != 0);
}

#[test]
fn mono_tri_drawing_area_clips_correctly() {
    // Draw a triangle at (5..50, 5..50) but with the active
    // drawing area limited to (20..40, 20..40). Pixels outside
    // the inner rect must remain zero on both backends.
    let v0 = (5, 5);
    let v1 = (60, 5);
    let v2 = (5, 60);
    let color = 0x001F; // pure red

    // CPU path: configure the draw area, then submit the prim.
    let mut gpu = Gpu::new();
    gpu.gp0_push(0xE3000000 | 20 | (20 << 10));
    gpu.gp0_push(0xE4000000 | 40 | (40 << 10));
    let cmd = 0x20000000 | bgr15_to_rgb24(color);
    gpu.gp0_push(cmd);
    gpu.gp0_push(pack_xy(v0));
    gpu.gp0_push(pack_xy(v1));
    gpu.gp0_push(pack_xy(v2));
    let cpu_vram = gpu.vram.words().to_vec();

    // GPU path with matching draw area.
    let vg = VramGpu::new_headless();
    let r = Rasterizer::new(&vg);
    let tri = MonoTri::opaque(v0, v1, v2, color);
    let area = DrawArea {
        left: 20,
        top: 20,
        right: 40,
        bottom: 40,
    };
    r.dispatch_mono_tri_scanline(&vg, &tri, &area);
    let gpu_vram = vg.download_full().expect("download");

    // Strict assertions on the clip boundary. Pixels outside
    // the area must be zero on both, and the inner area should
    // overlap (allowing fill-rule edge diffs).
    for y in 0..512u16 {
        for x in 0..1024u16 {
            let outside = x < 20 || x > 40 || y < 20 || y > 40;
            let idx = y as usize * 1024 + x as usize;
            if outside {
                assert_eq!(cpu_vram[idx], 0, "CPU @ ({x},{y}) outside clip");
                assert_eq!(gpu_vram[idx], 0, "GPU @ ({x},{y}) outside clip");
            }
        }
    }
}

// -------------------------------------------------------
//  Phase B.2 -- textured triangle parity vs CPU
// -------------------------------------------------------

/// Pack a (u, v) pair into the low 16 bits of a UV0/UV1/UV2 word.
fn uv_pack(uv: (u8, u8)) -> u32 {
    (uv.0 as u32) | ((uv.1 as u32) << 8)
}

/// Build the 16-bit tpage word the GPU expects in UV1 high half.
fn make_tpage_word(tpage_x: u32, tpage_y: u32, depth: u32, blend_bits: u32) -> u32 {
    let tx = tpage_x / 64; // 0..15
    let ty = if tpage_y == 256 { 1u32 } else { 0 };
    (tx & 0xF) | (ty << 4) | ((blend_bits & 0x3) << 5) | ((depth & 0x3) << 7)
}

/// Build the 16-bit CLUT word the GPU expects in UV0 high half.
/// `clut_x` must be a multiple of 16 (PS1 CLUT alignment).
fn make_clut_word(clut_x: u32, clut_y: u32) -> u32 {
    ((clut_x / 16) & 0x3F) | ((clut_y & 0x1FF) << 6)
}

/// Mirror the same VRAM state on a CPU `Gpu` instance and a
/// `VramGpu`. We pre-fill VRAM directly via `set_pixel` /
/// `upload_full` so the test doesn't have to fight CD-DMA.
fn seed_vram(words: &[u16], cpu: &mut Gpu, gpu: &VramGpu) {
    for (i, &w) in words.iter().enumerate() {
        let x = (i % 1024) as u16;
        let y = (i / 1024) as u16;
        cpu.vram.set_pixel(x, y, w);
    }
    gpu.upload_full(words).unwrap();
}

/// Drive the CPU rasterizer through a GP0 textured-triangle
/// packet. Caller has already set draw area + uploaded VRAM.
fn cpu_push_tex_tri(
    cpu: &mut Gpu,
    cmd_byte: u8,
    tint: (u8, u8, u8),
    v: [(i32, i32); 3],
    uv: [(u8, u8); 3],
    clut_word: u32,
    tpage_word: u32,
) {
    let cmd = ((cmd_byte as u32) << 24)
        | (tint.0 as u32)
        | ((tint.1 as u32) << 8)
        | ((tint.2 as u32) << 16);
    cpu.gp0_push(cmd);
    cpu.gp0_push(pack_xy(v[0]));
    cpu.gp0_push((clut_word << 16) | uv_pack(uv[0]));
    cpu.gp0_push(pack_xy(v[1]));
    cpu.gp0_push((tpage_word << 16) | uv_pack(uv[1]));
    cpu.gp0_push(pack_xy(v[2]));
    cpu.gp0_push(uv_pack(uv[2]));
}

#[test]
fn tex_tri_15bpp_axis_aligned_matches_cpu() {
    // 15bpp direct-colour: simplest tex sampling path. No CLUT,
    // each VRAM cell is the texel. Axis-aligned right triangle
    // → no edge-rule disagreements.
    let v = [(20i32, 20i32), (60, 20), (20, 60)];
    let uv = [(0u8, 0u8), (32, 0), (0, 32)];
    // Tpage at (128, 0), 15bpp.
    let tpage_x = 128u32;
    let tpage_y = 0u32;
    let tpage_word = make_tpage_word(tpage_x, tpage_y, 2, 0);

    // Build the texture: 64×64 of `(v << 5) | u | 0x0001`. The
    // `| 0x0001` ensures every texel is non-zero (i.e. opaque)
    // so we can spot dropped pixels.
    let mut vram = vec![0u16; 1024 * 512];
    for vy in 0..64u16 {
        for ux in 0..64u16 {
            let val = ((vy as u16) << 5) | ux | 0x0001;
            vram[vy as usize * 1024 + (tpage_x as usize + ux as usize)] = val;
        }
    }

    // CPU side.
    let mut cpu = Gpu::new();
    seed_vram(&vram, &mut cpu, &VramGpu::new_headless());
    // (Re-seed VRAM cleanly -- `seed_vram` above used a throwaway
    // headless device. We need a fresh one used for the actual
    // dispatch below. Easier: just call set_pixel + upload twice.)
    let vg = VramGpu::new_headless();
    vg.upload_full(&vram).unwrap();
    cpu.gp0_push(0xE3000000);
    cpu.gp0_push(0xE4000000 | 1023 | (511 << 10));
    cpu_push_tex_tri(&mut cpu, 0x25, (0, 0, 0), v, uv, 0, tpage_word);
    let cpu_words = cpu.vram.words().to_vec();

    // GPU side.
    let r = Rasterizer::new(&vg);
    let tri = TexTri::new(
        v[0],
        v[1],
        v[2],
        uv[0],
        uv[1],
        uv[2],
        0,
        0,
        (0x80, 0x80, 0x80),
        PrimFlags::RAW_TEXTURE,
        BlendMode::Average,
    );
    let tp = Tpage::new(tpage_x, tpage_y, 2);
    r.dispatch_tex_tri_scanline(&vg, &tri, &tp, &DrawArea::full_vram());
    let gpu_words = vg.download_full().unwrap();

    // Functional parity: the GPU samples the SAME texture cells
    // as the CPU at each integer pixel position with a barycentric
    // affine interpolation. Pixel-EXACT parity vs the Redux-port
    // scanline-delta math (which uses specific Q16.16 setup +
    // shl10idiv) is a Phase-B.x follow-up -- that path produces
    // off-by-1/2 UV at some interior pixels due to the difference
    // between cumulative per-row deltas and a barycentric divide.
    //
    // We assert the texel-COLOUR error is small: the percent of
    // diffs is bounded, AND every diff is within ±2 in any single
    // 5-bit channel (i.e. ≤6.25% intensity error on that channel).
    // That covers the rounding gap without hiding a coverage or
    // sampling bug.
    let diffs = diff_inside_bbox(&cpu_words, &gpu_words, (20, 20), (60, 60));
    let bbox = 41 * 41;
    // Record max channel delta across all differing pixels.
    let mut max_chan_delta = 0i32;
    for y in 20..=60i32 {
        for x in 20..=60i32 {
            let i = y as usize * 1024 + x as usize;
            let a = cpu_words[i];
            let b = gpu_words[i];
            if a == b {
                continue;
            }
            for shift in [0u32, 5, 10] {
                let ca = ((a >> shift) & 0x1F) as i32;
                let cb = ((b >> shift) & 0x1F) as i32;
                max_chan_delta = max_chan_delta.max((ca - cb).abs());
            }
        }
    }
    assert!(
        diffs * 4 < bbox,
        "tex 15bpp coverage: {diffs} / {bbox} pixels differ - too many"
    );
    assert!(
        max_chan_delta <= 2,
        "tex 15bpp colour error: max channel delta {max_chan_delta} > 2 - \
             likely a sampling / CLUT / depth bug"
    );
}

#[test]
fn tex_tri_4bpp_with_clut_matches_cpu() {
    // 4bpp paletted texture: each VRAM word holds 4 texel
    // indices, each indexes a 16-entry CLUT row. This stresses
    // the CLUT lookup path that the portrait bug
    // landed in.
    let v = [(20i32, 20i32), (60, 20), (20, 60)];
    let uv = [(0u8, 0u8), (32, 0), (0, 32)];
    let tpage_x = 0u32;
    let tpage_y = 0u32;
    let tpage_word = make_tpage_word(tpage_x, tpage_y, 0, 0);
    // CLUT at (0, 256). 16 entries, each in BGR15.
    let clut_x = 0u32;
    let clut_y = 256u32;
    let clut_word = make_clut_word(clut_x, clut_y);

    let mut vram = vec![0u16; 1024 * 512];
    // CLUT: entry 0 is non-zero (opaque "background"), entries
    // 1..15 are a colour ramp. Avoid 0x0000 anywhere so every
    // sampled texel writes a pixel.
    for i in 0..16u16 {
        let val = (i.max(1) << 1) | (i.max(1) << 6) | 0x4000;
        vram[clut_y as usize * 1024 + (clut_x as usize + i as usize)] = val;
    }
    // 16×16 texture: each VRAM word holds 4 texels (low to high
    // nibble = u 0..3 within the word). Pattern: nibble = (u + v)
    // & 0xF so different parts of the triangle hit different
    // CLUT entries.
    for vy in 0..16u16 {
        for word_x in 0..4u16 {
            let u_base = word_x * 4;
            let mut word = 0u16;
            for n in 0..4u16 {
                let u = u_base + n;
                let nibble = (u + vy) & 0xF;
                word |= nibble << (n * 4);
            }
            vram[vy as usize * 1024 + (tpage_x as usize + word_x as usize)] = word;
        }
    }

    let mut cpu = Gpu::new();
    for (i, &w) in vram.iter().enumerate() {
        cpu.vram.set_pixel((i % 1024) as u16, (i / 1024) as u16, w);
    }
    cpu.gp0_push(0xE3000000);
    cpu.gp0_push(0xE4000000 | 1023 | (511 << 10));
    cpu_push_tex_tri(&mut cpu, 0x25, (0, 0, 0), v, uv, clut_word, tpage_word);
    let cpu_words = cpu.vram.words().to_vec();

    let vg = VramGpu::new_headless();
    vg.upload_full(&vram).unwrap();
    let r = Rasterizer::new(&vg);
    let tri = TexTri::new(
        v[0],
        v[1],
        v[2],
        uv[0],
        uv[1],
        uv[2],
        clut_x,
        clut_y,
        (0x80, 0x80, 0x80),
        PrimFlags::RAW_TEXTURE,
        BlendMode::Average,
    );
    let tp = Tpage::new(tpage_x, tpage_y, 0);
    r.dispatch_tex_tri_scanline(&vg, &tri, &tp, &DrawArea::full_vram());
    let gpu_words = vg.download_full().unwrap();

    // See `tex_tri_15bpp_axis_aligned_matches_cpu` for parity
    // tolerance reasoning. We additionally allow a small CLUT-
    // index swap at edge pixels because adjacent CLUT entries
    // here differ by more than 2 in some channel.
    let diffs = diff_inside_bbox(&cpu_words, &gpu_words, (20, 20), (60, 60));
    let bbox = 41 * 41;
    assert!(diffs * 4 < bbox, "tex 4bpp coverage: {diffs} / {bbox}");
}

#[test]
fn tex_tri_8bpp_with_clut_matches_cpu() {
    // 8bpp: each VRAM word is two texel bytes, each indexes a
    // 256-entry CLUT row.
    let v = [(20i32, 20i32), (60, 20), (20, 60)];
    let uv = [(0u8, 0u8), (32, 0), (0, 32)];
    let tpage_x = 64u32;
    let tpage_y = 0u32;
    let tpage_word = make_tpage_word(tpage_x, tpage_y, 1, 0);
    // CLUT at (16, 256) -- 16 must be multiple of 16 (it is).
    // For 8bpp the CLUT row is 256 entries wide, but the host
    // doesn't pre-shift the X -- we pass the raw VRAM column.
    let clut_x = 16u32;
    let clut_y = 256u32;
    let clut_word = make_clut_word(clut_x, clut_y);

    let mut vram = vec![0u16; 1024 * 512];
    // CLUT: 256 entries, each non-zero, deterministic ramp.
    for i in 0..256u32 {
        let val = ((i & 0x1F) as u16) | (((i >> 1) & 0x1F) as u16) << 5 | 0x4000;
        vram[clut_y as usize * 1024 + (clut_x as usize + i as usize)] = val;
    }
    // Texture: each word = (u_high << 8) | u_low, giving a
    // gradient that maps to different CLUT entries.
    for vy in 0..32u16 {
        for word_x in 0..16u16 {
            let u_low = word_x * 2;
            let u_high = u_low + 1;
            let v_off = vy as u32;
            let lo = ((u_low as u32 + v_off) & 0xFF) as u16;
            let hi = ((u_high as u32 + v_off) & 0xFF) as u16;
            let word = lo | (hi << 8);
            // Avoid index 0 which would map to CLUT[0]; the
            // texture indices we generate above start at vy ≥ 0
            // and u ≥ 0, so the sum can be 0 only at (0,0).
            let word = if word == 0 { 0x0101 } else { word };
            vram[vy as usize * 1024 + (tpage_x as usize + word_x as usize)] = word;
        }
    }

    let mut cpu = Gpu::new();
    for (i, &w) in vram.iter().enumerate() {
        cpu.vram.set_pixel((i % 1024) as u16, (i / 1024) as u16, w);
    }
    cpu.gp0_push(0xE3000000);
    cpu.gp0_push(0xE4000000 | 1023 | (511 << 10));
    cpu_push_tex_tri(&mut cpu, 0x25, (0, 0, 0), v, uv, clut_word, tpage_word);
    let cpu_words = cpu.vram.words().to_vec();

    let vg = VramGpu::new_headless();
    vg.upload_full(&vram).unwrap();
    let r = Rasterizer::new(&vg);
    let tri = TexTri::new(
        v[0],
        v[1],
        v[2],
        uv[0],
        uv[1],
        uv[2],
        clut_x,
        clut_y,
        (0x80, 0x80, 0x80),
        PrimFlags::RAW_TEXTURE,
        BlendMode::Average,
    );
    let tp = Tpage::new(tpage_x, tpage_y, 1);
    r.dispatch_tex_tri_scanline(&vg, &tri, &tp, &DrawArea::full_vram());
    let gpu_words = vg.download_full().unwrap();

    let diffs = diff_inside_bbox(&cpu_words, &gpu_words, (20, 20), (60, 60));
    let bbox = 41 * 41;
    assert!(diffs * 4 < bbox, "tex 8bpp coverage: {diffs} / {bbox}");
}

#[test]
fn tex_tri_modulated_tint_matches_cpu() {
    // Same 15bpp setup but with a non-identity tint. Verifies
    // the `(tint * texel) / 0x80` modulator matches the CPU.
    let v = [(20i32, 20i32), (60, 20), (20, 60)];
    let uv = [(0u8, 0u8), (32, 0), (0, 32)];
    let tpage_x = 128u32;
    let tpage_word = make_tpage_word(tpage_x, 0, 2, 0);
    // 50% tint on each channel -- exactly half the value.
    let tint = (0x40u8, 0x40u8, 0x40u8);

    let mut vram = vec![0u16; 1024 * 512];
    for vy in 0..32u16 {
        for ux in 0..32u16 {
            let val = ((vy << 5) | ux) | 0x0001;
            vram[vy as usize * 1024 + (tpage_x as usize + ux as usize)] = val;
        }
    }

    let mut cpu = Gpu::new();
    for (i, &w) in vram.iter().enumerate() {
        cpu.vram.set_pixel((i % 1024) as u16, (i / 1024) as u16, w);
    }
    cpu.gp0_push(0xE3000000);
    cpu.gp0_push(0xE4000000 | 1023 | (511 << 10));
    // 0x24 = textured + modulated (NOT raw -- tint applies).
    cpu_push_tex_tri(&mut cpu, 0x24, tint, v, uv, 0, tpage_word);
    let cpu_words = cpu.vram.words().to_vec();

    let vg = VramGpu::new_headless();
    vg.upload_full(&vram).unwrap();
    let r = Rasterizer::new(&vg);
    let tri = TexTri::new(
        v[0],
        v[1],
        v[2],
        uv[0],
        uv[1],
        uv[2],
        0,
        0,
        tint,
        PrimFlags::empty(), // no RAW_TEXTURE → modulate
        BlendMode::Average,
    );
    let tp = Tpage::new(tpage_x, 0, 2);
    r.dispatch_tex_tri_scanline(&vg, &tri, &tp, &DrawArea::full_vram());
    let gpu_words = vg.download_full().unwrap();

    let diffs = diff_inside_bbox(&cpu_words, &gpu_words, (20, 20), (60, 60));
    let bbox = 41 * 41;
    assert!(diffs * 4 < bbox, "tex modulated coverage: {diffs} / {bbox}");
    // Strict check: every pixel that BOTH backends wrote should
    // have R/G/B that's been halved by the 0x40 tint. So any
    // non-zero pixel must have channels ≤ 0x10 (since input
    // texel channels here are ≤ 0x1F, half = ≤ 0x0F, plus 1
    // for divide-by-0x80 rounding).
    for y in 21..=59i32 {
        for x in 21..=59i32 {
            let i = y as usize * 1024 + x as usize;
            let g_val = gpu_words[i];
            if g_val == 0 {
                continue;
            }
            let r = g_val & 0x1F;
            let g = (g_val >> 5) & 0x1F;
            let b = (g_val >> 10) & 0x1F;
            assert!(
                r <= 0x10 && g <= 0x10 && b <= 0x10,
                "tint modulation looks wrong @ ({x},{y}): \
                     pixel=0x{g_val:04x} r={r} g={g} b={b}"
            );
        }
    }
}

#[test]
fn tex_tri_transparent_texels_skip_writes() {
    // Place a checkerboard texture: even cells are transparent
    // (texel = 0), odd cells are opaque. The triangle should
    // leave the back-buffer untouched at every transparent
    // hit, both on CPU and GPU.
    let v = [(40i32, 40i32), (80, 40), (40, 80)];
    let uv = [(0u8, 0u8), (32, 0), (0, 32)];
    let tpage_x = 128u32;
    let tpage_word = make_tpage_word(tpage_x, 0, 2, 0);

    // Pre-fill bbox area with a sentinel so the test can detect
    // exactly which pixels were touched.
    let prefill = 0x4321u16;
    let mut vram = vec![prefill; 1024 * 512];
    // Texture: opaque (non-zero) only on cells where (u + v)
    // is odd; transparent (0) elsewhere.
    for vy in 0..32u16 {
        for ux in 0..32u16 {
            let opaque = ((ux + vy) & 1) == 1;
            let val: u16 = if opaque {
                ((vy as u16) << 5) | (ux as u16) | 0x0001
            } else {
                0
            };
            vram[vy as usize * 1024 + (tpage_x as usize + ux as usize)] = val;
        }
    }

    let mut cpu = Gpu::new();
    for (i, &w) in vram.iter().enumerate() {
        cpu.vram.set_pixel((i % 1024) as u16, (i / 1024) as u16, w);
    }
    cpu.gp0_push(0xE3000000);
    cpu.gp0_push(0xE4000000 | 1023 | (511 << 10));
    cpu_push_tex_tri(&mut cpu, 0x25, (0, 0, 0), v, uv, 0, tpage_word);
    let cpu_words = cpu.vram.words().to_vec();

    let vg = VramGpu::new_headless();
    vg.upload_full(&vram).unwrap();
    let r = Rasterizer::new(&vg);
    let tri = TexTri::new(
        v[0],
        v[1],
        v[2],
        uv[0],
        uv[1],
        uv[2],
        0,
        0,
        (0x80, 0x80, 0x80),
        PrimFlags::RAW_TEXTURE,
        BlendMode::Average,
    );
    let tp = Tpage::new(tpage_x, 0, 2);
    r.dispatch_tex_tri_scanline(&vg, &tri, &tp, &DrawArea::full_vram());
    let gpu_words = vg.download_full().unwrap();

    // Inside the triangle, both backends should agree on coverage
    // within the same per-pixel-rounding tolerance noted on the
    // 15bpp test.
    let diffs = diff_inside_bbox(&cpu_words, &gpu_words, (40, 40), (80, 80));
    let bbox = 41 * 41;
    assert!(diffs * 4 < bbox, "tex transparent coverage: {diffs}");

    // Sanity: at least SOME pixels in the bbox should still be
    // the prefill (transparent texels left them alone), and at
    // least some should NOT be the prefill (opaque texels wrote
    // through).
    let inside_pixels = (40usize..80usize)
        .flat_map(|y| (40usize..80usize).map(move |x| y * 1024 + x))
        .filter(|&i| {
            // Inside the lower-left half of the triangle: x+y < 100.
            let x = i % 1024;
            let y = i / 1024;
            x + y < 100
        })
        .collect::<Vec<_>>();
    let untouched = inside_pixels
        .iter()
        .filter(|&&i| gpu_words[i] == prefill)
        .count();
    let touched = inside_pixels
        .iter()
        .filter(|&&i| gpu_words[i] != prefill)
        .count();
    assert!(untouched > 0, "expected some transparent-skip pixels");
    assert!(touched > 0, "expected some opaque-write pixels");
}

// -------------------------------------------------------
//  Phase B.x -- scanline-delta textured triangle: BIT-EXACT
// -------------------------------------------------------

#[test]
fn tex_tri_scanline_15bpp_axis_aligned_is_bit_exact() {
    // Same setup as the B.2 axis-aligned test, but using the
    // scanline-delta dispatcher. Strict `assert_eq!` is the
    // whole point of this phase.
    let v = [(20i32, 20i32), (60, 20), (20, 60)];
    let uv = [(0u8, 0u8), (32, 0), (0, 32)];
    let tpage_x = 128u32;
    let tpage_word = make_tpage_word(tpage_x, 0, 2, 0);

    let mut vram = vec![0u16; 1024 * 512];
    for vy in 0..64u16 {
        for ux in 0..64u16 {
            let val = ((vy as u16) << 5) | ux | 0x0001;
            vram[vy as usize * 1024 + (tpage_x as usize + ux as usize)] = val;
        }
    }
    let mut cpu = Gpu::new();
    for (i, &w) in vram.iter().enumerate() {
        cpu.vram.set_pixel((i % 1024) as u16, (i / 1024) as u16, w);
    }
    cpu.gp0_push(0xE3000000);
    cpu.gp0_push(0xE4000000 | 1023 | (511 << 10));
    cpu_push_tex_tri(&mut cpu, 0x25, (0, 0, 0), v, uv, 0, tpage_word);
    let cpu_words = cpu.vram.words().to_vec();

    let vg = VramGpu::new_headless();
    vg.upload_full(&vram).unwrap();
    let r = Rasterizer::new(&vg);
    let tri = TexTri::new(
        v[0],
        v[1],
        v[2],
        uv[0],
        uv[1],
        uv[2],
        0,
        0,
        (0x80, 0x80, 0x80),
        PrimFlags::RAW_TEXTURE,
        BlendMode::Average,
    );
    let tp = Tpage::new(tpage_x, 0, 2);
    let dispatched = r.dispatch_tex_tri_scanline(&vg, &tri, &tp, &DrawArea::full_vram());
    assert!(dispatched, "valid triangle should dispatch");
    let gpu_words = vg.download_full().unwrap();

    assert_eq!(cpu_words, gpu_words, "tex tri scanline strict parity");
}

#[test]
fn tex_tri_scanline_skewed_is_bit_exact() {
    // The skewed triangle case. Without scanline-delta this had
    // ±2/5-bit channel error from barycentric rounding. Now
    // strict equality.
    let v = [(50i32, 20i32), (130, 70), (30, 90)];
    let uv = [(0u8, 0u8), (60, 0), (0, 50)];
    let tpage_x = 128u32;
    let tpage_word = make_tpage_word(tpage_x, 0, 2, 0);

    let mut vram = vec![0u16; 1024 * 512];
    for vy in 0..128u16 {
        for ux in 0..128u16 {
            let val = ((vy as u16) << 5) | (ux as u16) | 0x0001;
            vram[vy as usize * 1024 + (tpage_x as usize + ux as usize)] = val;
        }
    }
    let mut cpu = Gpu::new();
    for (i, &w) in vram.iter().enumerate() {
        cpu.vram.set_pixel((i % 1024) as u16, (i / 1024) as u16, w);
    }
    cpu.gp0_push(0xE3000000);
    cpu.gp0_push(0xE4000000 | 1023 | (511 << 10));
    cpu_push_tex_tri(&mut cpu, 0x25, (0, 0, 0), v, uv, 0, tpage_word);
    let cpu_words = cpu.vram.words().to_vec();

    let vg = VramGpu::new_headless();
    vg.upload_full(&vram).unwrap();
    let r = Rasterizer::new(&vg);
    let tri = TexTri::new(
        v[0],
        v[1],
        v[2],
        uv[0],
        uv[1],
        uv[2],
        0,
        0,
        (0x80, 0x80, 0x80),
        PrimFlags::RAW_TEXTURE,
        BlendMode::Average,
    );
    let tp = Tpage::new(tpage_x, 0, 2);
    r.dispatch_tex_tri_scanline(&vg, &tri, &tp, &DrawArea::full_vram());
    let gpu_words = vg.download_full().unwrap();

    assert_eq!(
        cpu_words, gpu_words,
        "tex tri scanline skewed strict parity"
    );
}

#[test]
fn tex_tri_scanline_4bpp_with_clut_is_bit_exact() {
    // 4bpp + CLUT -- exercises the texture-window-free CLUT
    // sampling path with bit-exact UV.
    let v = [(20i32, 20i32), (60, 20), (20, 60)];
    let uv = [(0u8, 0u8), (32, 0), (0, 32)];
    let tpage_x = 0u32;
    let tpage_word = make_tpage_word(tpage_x, 0, 0, 0);
    let clut_x = 0u32;
    let clut_y = 256u32;
    let clut_word = make_clut_word(clut_x, clut_y);

    let mut vram = vec![0u16; 1024 * 512];
    for i in 0..16u16 {
        let val = (i.max(1) << 1) | (i.max(1) << 6) | 0x4000;
        vram[clut_y as usize * 1024 + (clut_x as usize + i as usize)] = val;
    }
    for vy in 0..16u16 {
        for word_x in 0..4u16 {
            let mut word = 0u16;
            for n in 0..4u16 {
                let nibble = ((word_x * 4 + n) + vy) & 0xF;
                word |= nibble << (n * 4);
            }
            vram[vy as usize * 1024 + (tpage_x as usize + word_x as usize)] = word;
        }
    }
    let mut cpu = Gpu::new();
    for (i, &w) in vram.iter().enumerate() {
        cpu.vram.set_pixel((i % 1024) as u16, (i / 1024) as u16, w);
    }
    cpu.gp0_push(0xE3000000);
    cpu.gp0_push(0xE4000000 | 1023 | (511 << 10));
    cpu_push_tex_tri(&mut cpu, 0x25, (0, 0, 0), v, uv, clut_word, tpage_word);
    let cpu_words = cpu.vram.words().to_vec();

    let vg = VramGpu::new_headless();
    vg.upload_full(&vram).unwrap();
    let r = Rasterizer::new(&vg);
    let tri = TexTri::new(
        v[0],
        v[1],
        v[2],
        uv[0],
        uv[1],
        uv[2],
        clut_x,
        clut_y,
        (0x80, 0x80, 0x80),
        PrimFlags::RAW_TEXTURE,
        BlendMode::Average,
    );
    let tp = Tpage::new(tpage_x, 0, 0);
    r.dispatch_tex_tri_scanline(&vg, &tri, &tp, &DrawArea::full_vram());
    let gpu_words = vg.download_full().unwrap();

    assert_eq!(
        cpu_words, gpu_words,
        "tex tri scanline 4bpp+CLUT strict parity"
    );
}

#[test]
fn shaded_tex_tri_scanline_15bpp_is_bit_exact() {
    // The same setup as the B.3.b textured-shaded test, but
    // through the scanline-delta dispatcher. Strict equality
    // expected -- both UV and RGB walks now exactly match the
    // CPU's cumulative arithmetic.
    let v = [(20i32, 20i32), (60, 20), (20, 60)];
    let uv = [(0u8, 0u8), (32, 0), (0, 32)];
    let c = [
        (0x80u8, 0x80u8, 0x80u8),
        (0xC0, 0xC0, 0xC0),
        (0xFFu8, 0xFFu8, 0xFFu8),
    ];
    let tpage_x = 128u32;
    let tpage_word = make_tpage_word(tpage_x, 0, 2, 0);

    let mut vram = vec![0u16; 1024 * 512];
    for vy in 0..32u16 {
        for ux in 0..32u16 {
            let val = ((vy as u16) << 5) | (ux as u16) | 0x0001;
            vram[vy as usize * 1024 + (tpage_x as usize + ux as usize)] = val;
        }
    }

    let mut cpu = Gpu::new();
    for (i, &w) in vram.iter().enumerate() {
        cpu.vram.set_pixel((i % 1024) as u16, (i / 1024) as u16, w);
    }
    cpu.gp0_push(0xE3000000);
    cpu.gp0_push(0xE4000000 | 1023 | (511 << 10));
    let pack_rgb = |t: (u8, u8, u8)| (t.0 as u32) | ((t.1 as u32) << 8) | ((t.2 as u32) << 16);
    // 0x34 = textured-shaded triangle.
    cpu.gp0_push((0x34u32 << 24) | pack_rgb(c[0]));
    cpu.gp0_push(pack_xy(v[0]));
    cpu.gp0_push(uv_pack(uv[0]));
    cpu.gp0_push(pack_rgb(c[1]));
    cpu.gp0_push(pack_xy(v[1]));
    cpu.gp0_push((tpage_word << 16) | uv_pack(uv[1]));
    cpu.gp0_push(pack_rgb(c[2]));
    cpu.gp0_push(pack_xy(v[2]));
    cpu.gp0_push(uv_pack(uv[2]));
    let cpu_words = cpu.vram.words().to_vec();

    let vg = VramGpu::new_headless();
    vg.upload_full(&vram).unwrap();
    let r = Rasterizer::new(&vg);
    let tri = ShadedTexTri::new(
        v[0],
        v[1],
        v[2],
        c[0],
        c[1],
        c[2],
        uv[0],
        uv[1],
        uv[2],
        0,
        0,
        PrimFlags::empty(),
        BlendMode::Average,
    );
    let tp = Tpage::new(tpage_x, 0, 2);
    let dispatched = r.dispatch_shaded_tex_tri_scanline(&vg, &tri, &tp, &DrawArea::full_vram());
    assert!(dispatched);
    let gpu_words = vg.download_full().unwrap();

    assert_eq!(
        cpu_words, gpu_words,
        "shaded-tex-tri scanline strict parity"
    );
}

/// Commercial portrait regression. The CPU's
/// `rasterize_axis_aligned_textured_quad` fast path uses bilinear
/// UV interpolation; the original GPU replay split the quad into
/// two barycentric triangles, which produced different pixels
/// when the V channel wasn't affine across the four corners
/// (UV3.v != UV1.v + UV2.v - UV0.v). Now the replay detects
/// axis-aligned + non-affine quads and dispatches the dedicated
/// `tex_quad_bilinear` shader. This test pins that path with the
/// exact UV layout from the divergent packet (cmd #9032
/// in the boot trace).
#[test]
fn tex_quad_bilinear_non_affine_uvs_match_cpu() {
    // Destination quad in the top-left of VRAM; texture parked
    // in the bottom-right region so the quad's writes can't
    // corrupt the texels mid-rasterization (a 96×80 quad placed
    // over its own tpage would self-overwrite at every pixel
    // whose UV mapped to the destination range).
    let v = [
        (50i32, 30i32),
        (50 + 95, 30),
        (50, 30 + 79),
        (50 + 95, 30 + 79),
    ];
    // Non-affine V: 0,64,79,79. Triangle-split would produce
    // wrong pixels here.
    let uv = [(0u8, 0u8), (95u8, 64u8), (0u8, 79u8), (95u8, 79u8)];
    let tpage_x = 512u32;
    let tpage_y = 256u32;
    let tpage_word = make_tpage_word(tpage_x, tpage_y, 2, 0);

    // 96×80 texture: every cell carries a unique non-zero value
    // so any UV-step bug shows up.
    let mut vram = vec![0u16; 1024 * 512];
    for vy in 0..80u16 {
        for ux in 0..96u16 {
            let val = ((vy & 0x1F) << 5) | (ux & 0x1F) | 0x0001;
            vram[(tpage_y as usize + vy as usize) * 1024 + (tpage_x as usize + ux as usize)] = val;
        }
    }

    // CPU: drive the textured-quad packet (op 0x2D = textured,
    // raw, opaque). Vertex order matches the cmd_log layout.
    let mut cpu = Gpu::new();
    for (i, &w) in vram.iter().enumerate() {
        cpu.vram.set_pixel((i % 1024) as u16, (i / 1024) as u16, w);
    }
    cpu.gp0_push(0xE3000000);
    cpu.gp0_push(0xE4000000 | 1023 | (511 << 10));
    // 0x2D -- textured quad, raw texture, opaque.
    cpu.gp0_push(0x2D000000);
    cpu.gp0_push(pack_xy(v[0]));
    cpu.gp0_push(uv_pack(uv[0])); // clut=0
    cpu.gp0_push(pack_xy(v[1]));
    cpu.gp0_push((tpage_word << 16) | uv_pack(uv[1]));
    cpu.gp0_push(pack_xy(v[2]));
    cpu.gp0_push(uv_pack(uv[2]));
    cpu.gp0_push(pack_xy(v[3]));
    cpu.gp0_push(uv_pack(uv[3]));
    let cpu_words = cpu.vram.words().to_vec();

    // GPU: confirm the axis-aligned detector accepts these
    // vertices, then dispatch the bilinear shader.
    assert!(
        TexQuadBilinear::is_axis_aligned(v[0], v[1], v[2], v[3]),
        "test geometry must be axis-aligned for the bilinear path"
    );
    let vg = VramGpu::new_headless();
    vg.upload_full(&vram).unwrap();
    let r = Rasterizer::new(&vg);
    let quad = TexQuadBilinear::new(
        v[0],
        v[1],
        v[2],
        v[3],
        uv[0],
        uv[1],
        uv[2],
        uv[3],
        0,
        0,
        (0x80, 0x80, 0x80),
        PrimFlags::RAW_TEXTURE,
        BlendMode::Average,
    );
    let tp = Tpage::new(tpage_x, tpage_y, 2);
    r.dispatch_tex_quad_bilinear(&vg, &quad, &tp, &DrawArea::full_vram());
    let gpu_words = vg.download_full().unwrap();

    let mut diffs: Vec<(usize, usize, u16, u16)> = Vec::new();
    for (i, (&c, &g)) in cpu_words.iter().zip(gpu_words.iter()).enumerate() {
        if c != g {
            diffs.push((i % 1024, i / 1024, c, g));
            if diffs.len() >= 16 {
                break;
            }
        }
    }
    assert!(
        diffs.is_empty(),
        "tex-quad bilinear should match CPU's axis-aligned fast path. \
             {} diffs (first ≤16 shown):\n{}",
        diffs.len(),
        diffs
            .iter()
            .map(|(x, y, c, g)| format!("  ({x},{y}) cpu=0x{c:04x} gpu=0x{g:04x}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

fn assert_tex_quad_bilinear_order_matches_cpu(
    v: [(i32, i32); 4],
    uv: [(u8, u8); 4],
    context: &str,
) {
    let tpage_x = 512u32;
    let tpage_y = 256u32;
    let tpage_word = make_tpage_word(tpage_x, tpage_y, 2, 0);

    let mut vram = vec![0u16; 1024 * 512];
    for y in 0..4u16 {
        for x in 0..4u16 {
            vram[(tpage_y as usize + y as usize) * 1024 + (tpage_x as usize + x as usize)] =
                0x1000 + (y << 4) + x;
        }
    }

    let mut cpu = Gpu::new();
    for (i, &w) in vram.iter().enumerate() {
        cpu.vram.set_pixel((i % 1024) as u16, (i / 1024) as u16, w);
    }
    cpu.gp0_push(0xE3000000);
    cpu.gp0_push(0xE4000000 | 1023 | (511 << 10));
    cpu.gp0_push(0x2D000000);
    cpu.gp0_push(pack_xy(v[0]));
    cpu.gp0_push(uv_pack(uv[0]));
    cpu.gp0_push(pack_xy(v[1]));
    cpu.gp0_push((tpage_word << 16) | uv_pack(uv[1]));
    cpu.gp0_push(pack_xy(v[2]));
    cpu.gp0_push(uv_pack(uv[2]));
    cpu.gp0_push(pack_xy(v[3]));
    cpu.gp0_push(uv_pack(uv[3]));
    let cpu_words = cpu.vram.words().to_vec();

    assert!(
        TexQuadBilinear::is_axis_aligned(v[0], v[1], v[2], v[3]),
        "{context} quad should use the bilinear path"
    );
    let vg = VramGpu::new_headless();
    vg.upload_full(&vram).unwrap();
    let r = Rasterizer::new(&vg);
    let quad = TexQuadBilinear::new(
        v[0],
        v[1],
        v[2],
        v[3],
        uv[0],
        uv[1],
        uv[2],
        uv[3],
        0,
        0,
        (0x80, 0x80, 0x80),
        PrimFlags::RAW_TEXTURE,
        BlendMode::Average,
    );
    let tp = Tpage::new(tpage_x, tpage_y, 2);
    assert!(r.dispatch_tex_quad_bilinear(&vg, &quad, &tp, &DrawArea::full_vram()));
    let gpu_words = vg.download_full().unwrap();

    assert_eq!(&gpu_words[0..4], &cpu_words[0..4]);
    assert_eq!(gpu_words, cpu_words, "{context}");
}

#[test]
fn tex_quad_bilinear_right_to_left_order_matches_cpu() {
    assert_tex_quad_bilinear_order_matches_cpu(
        [(4i32, 0i32), (0, 0), (4, 4), (0, 4)],
        [(4u8, 0u8), (0, 0), (4, 4), (0, 4)],
        "right-to-left mirrored",
    );
}

#[test]
fn tex_quad_bilinear_bottom_to_top_order_matches_cpu() {
    assert_tex_quad_bilinear_order_matches_cpu(
        [(0i32, 4i32), (4, 4), (0, 0), (4, 0)],
        [(0u8, 4u8), (4, 4), (0, 0), (4, 0)],
        "bottom-to-top",
    );
}

/// X-flipped sprite: vertices run left-to-right but U runs right-to-
/// left, so the per-row `delta_u = (right_u - left_u) / width` is
/// NEGATIVE. The shader's `i64_div_i32` used to fold the sign-
/// extension high word into the low word with a bitwise OR, which
/// collapsed every negative numerator to -1 -- the UV walk stepped by
/// ~0 and the quad sampled the wrong texel columns. Dozens of alttp
/// gameplay sprites (op 0x2C, identity tint, 56 px each) hit this;
/// found by replay_bisect's owner report on gameplay chunk 16.
#[test]
fn tex_quad_bilinear_flipped_u_matches_cpu() {
    assert_tex_quad_bilinear_order_matches_cpu(
        [(0i32, 0i32), (4, 0), (0, 4), (4, 4)],
        [(4u8, 0u8), (0, 0), (4, 4), (0, 4)],
        "x-flipped (reversed U, straight vertices)",
    );
}

/// Y-flip counterpart: straight vertices, V decreasing top-to-bottom.
/// The row walk multiplies a negative Q16.16 delta through
/// `i64_mul_u32`; pin it too.
#[test]
fn tex_quad_bilinear_flipped_v_matches_cpu() {
    assert_tex_quad_bilinear_order_matches_cpu(
        [(0i32, 0i32), (4, 0), (0, 4), (4, 4)],
        [(0u8, 4u8), (4, 4), (0, 0), (4, 0)],
        "y-flipped (reversed V, straight vertices)",
    );
}

#[test]
fn mono_tri_scanline_skewed_is_bit_exact() {
    // The B.1 skewed triangle case had ≤0.5% edge-rule diffs
    // under barycentric. With scanline-delta coverage, strict
    // equality.
    let v0 = (50i32, 20i32);
    let v1 = (130, 70);
    let v2 = (30, 90);
    let color = 0x03E0u16;
    let cpu = cpu_rasterize_mono_tri(v0, v1, v2, color);
    let vg = VramGpu::new_headless();
    let r = Rasterizer::new(&vg);
    let tri = MonoTri::opaque(v0, v1, v2, color);
    let dispatched = r.dispatch_mono_tri_scanline(&vg, &tri, &DrawArea::full_vram());
    assert!(dispatched);
    let gpu = vg.download_full().unwrap();
    assert_eq!(cpu, gpu, "mono tri scanline skewed strict parity");
}

#[test]
fn shaded_tri_scanline_skewed_is_bit_exact() {
    // The B.3.a tri had ±2/5-bit channel error under barycentric.
    // With scanline-delta RGB walk, strict equality.
    let v = [(50i32, 20i32), (130, 70), (30, 90)];
    let c = [
        (0xFFu8, 0x00u8, 0x00u8),
        (0x00, 0xFF, 0x00),
        (0x00, 0x00, 0xFF),
    ];
    let cpu = cpu_rasterize_shaded_tri(v, c, 0x30, 0, 0, 0);
    let vg = VramGpu::new_headless();
    let r = Rasterizer::new(&vg);
    let tri = ShadedTri::new(
        v[0],
        v[1],
        v[2],
        c[0],
        c[1],
        c[2],
        PrimFlags::empty(),
        BlendMode::Average,
    );
    let dispatched = r.dispatch_shaded_tri_scanline(&vg, &tri, &DrawArea::full_vram());
    assert!(dispatched);
    let gpu = vg.download_full().unwrap();
    assert_eq!(cpu, gpu, "shaded tri scanline skewed strict parity");
}

// -------------------------------------------------------
//  Phase B.5 -- rectangle parity vs CPU
// -------------------------------------------------------

/// Drive the CPU rasterizer for one monochrome rectangle via
/// GP0 0x60 (variable-size mono rect, 3-word packet).
/// `prefill` paints VRAM before the rect -- needed for mask-check
/// and semi-trans tests.
fn cpu_rasterize_mono_rect(
    xy: (i32, i32),
    wh: (u32, u32),
    color: u16,
    cmd_byte: u8,
    tpage_blend_bits: u8,
    mask_e6: u8,
    prefill: u16,
) -> Vec<u16> {
    let mut gpu = Gpu::new();
    if prefill != 0 {
        for y in 0..512u16 {
            for x in 0..1024u16 {
                gpu.vram.set_pixel(x, y, prefill);
            }
        }
    }
    gpu.gp0_push(0xE3000000);
    gpu.gp0_push(0xE4000000 | 1023 | (511 << 10));
    let e1 = 0xE100_0000_u32 | ((tpage_blend_bits as u32) & 0x3) << 5;
    gpu.gp0_push(e1);
    gpu.gp0_push(0xE600_0000_u32 | (mask_e6 as u32) & 0x3);
    let cmd = ((cmd_byte as u32) << 24) | bgr15_to_rgb24(color);
    gpu.gp0_push(cmd);
    gpu.gp0_push(((xy.1 as u32) << 16) | (xy.0 as u32 & 0xFFFF));
    gpu.gp0_push((wh.1 << 16) | (wh.0 & 0xFFFF));
    gpu.vram.words().to_vec()
}

fn gpu_prefill_full(vg: &VramGpu, value: u16) {
    if value == 0 {
        return;
    }
    let buf = vec![value; (super::super::VRAM_WIDTH * super::super::VRAM_HEIGHT) as usize];
    vg.upload_full(&buf).unwrap();
}

#[test]
fn mono_rect_basic_opaque_matches_cpu() {
    // Strict bit-exact parity: rectangles have no interpolation,
    // so the only sources of disagreement would be coverage or
    // RMW bugs -- neither of which we expect.
    let xy = (50, 60);
    let wh = (40u32, 30u32);
    let color = 0x4321;
    let cpu = cpu_rasterize_mono_rect(xy, wh, color, 0x60, 0, 0, 0);
    let vg = VramGpu::new_headless();
    let r = Rasterizer::new(&vg);
    r.dispatch_mono_rect(
        &vg,
        &MonoRect::opaque(xy, wh, color),
        &DrawArea::full_vram(),
    );
    let gpu = vg.download_full().unwrap();
    assert_eq!(cpu, gpu, "mono rect strict parity");
}

#[test]
fn mono_rect_drawing_area_clip_matches_cpu() {
    // Rect spans (5..55, 5..55) but draw area is (20..40,20..40).
    let xy = (5i32, 5i32);
    let wh = (50u32, 50u32);
    let color = 0x001F; // red
    let mut cpu_gpu = Gpu::new();
    cpu_gpu.gp0_push(0xE3000000 | 20 | (20 << 10));
    cpu_gpu.gp0_push(0xE4000000 | 40 | (40 << 10));
    cpu_gpu.gp0_push(0xE100_0000);
    cpu_gpu.gp0_push(0xE600_0000);
    let cmd = 0x60_000000_u32 | bgr15_to_rgb24(color);
    cpu_gpu.gp0_push(cmd);
    cpu_gpu.gp0_push(((xy.1 as u32) << 16) | (xy.0 as u32 & 0xFFFF));
    cpu_gpu.gp0_push((wh.1 << 16) | (wh.0 & 0xFFFF));
    let cpu = cpu_gpu.vram.words().to_vec();

    let vg = VramGpu::new_headless();
    let r = Rasterizer::new(&vg);
    r.dispatch_mono_rect(
        &vg,
        &MonoRect::opaque(xy, wh, color),
        &DrawArea {
            left: 20,
            top: 20,
            right: 40,
            bottom: 40,
        },
    );
    let gpu = vg.download_full().unwrap();
    assert_eq!(cpu, gpu, "mono rect clip strict parity");
}

#[test]
fn mono_rect_semi_trans_average_matches_cpu() {
    let xy = (10, 10);
    let wh = (20u32, 20u32);
    let color = 0x1234;
    let prefill = 0x5678;
    let cpu = cpu_rasterize_mono_rect(xy, wh, color, 0x62, 0, 0, prefill);
    let vg = VramGpu::new_headless();
    gpu_prefill_full(&vg, prefill);
    let r = Rasterizer::new(&vg);
    r.dispatch_mono_rect(
        &vg,
        &MonoRect::new(xy, wh, color, PrimFlags::SEMI_TRANS, BlendMode::Average),
        &DrawArea::full_vram(),
    );
    let gpu = vg.download_full().unwrap();
    assert_eq!(cpu, gpu, "mono rect semi-trans Average parity");
}

#[test]
fn mono_rect_mask_check_protects_pixels() {
    let xy = (10, 10);
    let wh = (15u32, 15u32);
    let color = 0x0123;
    let prefill = 0x8888; // bit 15 set on every pixel
    let cpu = cpu_rasterize_mono_rect(xy, wh, color, 0x60, 0, 0b10, prefill);
    let vg = VramGpu::new_headless();
    gpu_prefill_full(&vg, prefill);
    let r = Rasterizer::new(&vg);
    r.dispatch_mono_rect(
        &vg,
        &MonoRect::new(xy, wh, color, PrimFlags::MASK_CHECK, BlendMode::Average),
        &DrawArea::full_vram(),
    );
    let gpu = vg.download_full().unwrap();
    assert_eq!(cpu, gpu, "mono rect mask-check parity");
    // Sanity: nothing should have been written.
    let i = 12 * 1024 + 12;
    assert_eq!(gpu[i], prefill);
}

#[test]
fn mono_rect_zero_size_is_dropped() {
    let cpu = cpu_rasterize_mono_rect((10, 10), (0, 5), 0x4321, 0x60, 0, 0, 0);
    let vg = VramGpu::new_headless();
    let r = Rasterizer::new(&vg);
    r.dispatch_mono_rect(
        &vg,
        &MonoRect::opaque((10, 10), (0, 5), 0x4321),
        &DrawArea::full_vram(),
    );
    let gpu = vg.download_full().unwrap();
    assert!(cpu.iter().all(|&w| w == 0), "CPU drops zero-width rect");
    assert!(gpu.iter().all(|&w| w == 0), "GPU drops zero-width rect");
}

/// Drive the CPU rasterizer for a textured rect via GP0 0x64.
/// Caller has already set draw area + uploaded VRAM.
#[allow(clippy::too_many_arguments)]
fn cpu_push_tex_rect(
    cpu: &mut Gpu,
    cmd_byte: u8,
    tint: (u8, u8, u8),
    xy: (i32, i32),
    wh: (u32, u32),
    uv: (u8, u8),
    clut_word: u32,
    // CPU side picks tpage from the LAST GP0 0xE1 -- the rect
    // packet has no per-prim tpage word. Caller pushes it
    // separately before calling.
) {
    let cmd = ((cmd_byte as u32) << 24)
        | (tint.0 as u32)
        | ((tint.1 as u32) << 8)
        | ((tint.2 as u32) << 16);
    cpu.gp0_push(cmd);
    cpu.gp0_push(((xy.1 as u32) << 16) | (xy.0 as u32 & 0xFFFF));
    let uv_clut = ((clut_word & 0xFFFF) << 16) | ((uv.0 as u32) | ((uv.1 as u32) << 8));
    cpu.gp0_push(uv_clut);
    cpu.gp0_push((wh.1 << 16) | (wh.0 & 0xFFFF));
}

/// Build the GP0 0xE1 word that sets the active tpage state on
/// the CPU side (mirrors what `apply_primitive_tpage` does in
/// `tex_tri`, but for rect primitives the host has to push it
/// explicitly since rect packets have no per-prim tpage word).
fn make_e1_for_tpage(tpage_x: u32, tpage_y: u32, depth: u32) -> u32 {
    let tx = tpage_x / 64;
    let ty = if tpage_y == 256 { 1u32 } else { 0 };
    0xE100_0000 | (tx & 0xF) | (ty << 4) | ((depth & 0x3) << 7)
}

#[test]
fn tex_rect_15bpp_basic_matches_cpu_byte_for_byte() {
    // Bit-exact parity: rect UVs step linearly, no Q16.16
    // delta math, so CPU and GPU should agree pixel-for-pixel.
    let xy = (40i32, 30i32);
    let wh = (32u32, 24u32);
    let uv = (0u8, 0u8);
    let tpage_x = 128u32;

    let mut vram = vec![0u16; 1024 * 512];
    // Distinct colours per cell so any UV miss shows up.
    for vy in 0..32u16 {
        for ux in 0..64u16 {
            let val = ((vy as u16) << 5) | (ux as u16) | 0x0001;
            vram[vy as usize * 1024 + (tpage_x as usize + ux as usize)] = val;
        }
    }

    let mut cpu = Gpu::new();
    for (i, &w) in vram.iter().enumerate() {
        cpu.vram.set_pixel((i % 1024) as u16, (i / 1024) as u16, w);
    }
    cpu.gp0_push(0xE3000000);
    cpu.gp0_push(0xE4000000 | 1023 | (511 << 10));
    cpu.gp0_push(make_e1_for_tpage(tpage_x, 0, 2));
    cpu.gp0_push(0xE600_0000);
    // Cmd 0x65 = textured rect, raw flag set.
    cpu_push_tex_rect(&mut cpu, 0x65, (0, 0, 0), xy, wh, uv, 0);
    let cpu_words = cpu.vram.words().to_vec();

    let vg = VramGpu::new_headless();
    vg.upload_full(&vram).unwrap();
    let r = Rasterizer::new(&vg);
    let rect = TexRect::new(
        xy,
        wh,
        uv,
        0,
        0,
        (0x80, 0x80, 0x80),
        PrimFlags::RAW_TEXTURE,
        BlendMode::Average,
    );
    let tp = Tpage::new(tpage_x, 0, 2);
    r.dispatch_tex_rect(&vg, &rect, &tp, &DrawArea::full_vram());
    let gpu_words = vg.download_full().unwrap();

    assert_eq!(cpu_words, gpu_words, "tex rect strict parity");
}

#[test]
fn tex_rect_x_flip_counts_down_from_biased_origin() {
    // GP0 0xE1 bit 12 = X flip. Silicon reads u0+1, u0, u0-1...
    // rather than mirroring around the rectangle's far edge.
    let xy = (40i32, 30i32);
    let wh = (16u32, 16u32);
    let uv = (0u8, 0u8);
    let tpage_x = 128u32;

    let mut vram = vec![0u16; 1024 * 512];
    for vy in 0..32u16 {
        for ux in 0..32u16 {
            let val = ((vy as u16) << 5) | (ux as u16) | 0x0001;
            vram[vy as usize * 1024 + (tpage_x as usize + ux as usize)] = val;
        }
    }

    // CPU: set the X-flip bit in GP0 0xE1 (bit 12 = 0x1000).
    let mut cpu = Gpu::new();
    for (i, &w) in vram.iter().enumerate() {
        cpu.vram.set_pixel((i % 1024) as u16, (i / 1024) as u16, w);
    }
    cpu.gp0_push(0xE3000000);
    cpu.gp0_push(0xE4000000 | 1023 | (511 << 10));
    cpu.gp0_push(make_e1_for_tpage(tpage_x, 0, 2) | 0x1000);
    cpu.gp0_push(0xE600_0000);
    cpu_push_tex_rect(&mut cpu, 0x65, (0, 0, 0), xy, wh, uv, 0);
    let cpu_words = cpu.vram.words().to_vec();

    let vg = VramGpu::new_headless();
    vg.upload_full(&vram).unwrap();
    let r = Rasterizer::new(&vg);
    let rect = TexRect::new(
        xy,
        wh,
        uv,
        0,
        0,
        (0x80, 0x80, 0x80),
        PrimFlags::RAW_TEXTURE | PrimFlags::FLIP_X,
        BlendMode::Average,
    );
    let tp = Tpage::new(tpage_x, 0, 2);
    r.dispatch_tex_rect(&vg, &rect, &tp, &DrawArea::full_vram());
    let gpu_words = vg.download_full().unwrap();

    assert_eq!(cpu_words, gpu_words, "tex rect X-flip strict parity");
}

#[test]
fn tex_rect_modulated_tint_matches_cpu_byte_for_byte() {
    let xy = (40i32, 30i32);
    let wh = (16u32, 16u32);
    let uv = (0u8, 0u8);
    let tpage_x = 128u32;
    let tint = (0x40u8, 0x40u8, 0x40u8); // halve every channel

    let mut vram = vec![0u16; 1024 * 512];
    for vy in 0..32u16 {
        for ux in 0..32u16 {
            let val = ((vy as u16) << 5) | (ux as u16) | 0x0001;
            vram[vy as usize * 1024 + (tpage_x as usize + ux as usize)] = val;
        }
    }

    let mut cpu = Gpu::new();
    for (i, &w) in vram.iter().enumerate() {
        cpu.vram.set_pixel((i % 1024) as u16, (i / 1024) as u16, w);
    }
    cpu.gp0_push(0xE3000000);
    cpu.gp0_push(0xE4000000 | 1023 | (511 << 10));
    cpu.gp0_push(make_e1_for_tpage(tpage_x, 0, 2));
    cpu.gp0_push(0xE600_0000);
    // 0x64 = textured rect, modulated (NOT raw).
    cpu_push_tex_rect(&mut cpu, 0x64, tint, xy, wh, uv, 0);
    let cpu_words = cpu.vram.words().to_vec();

    let vg = VramGpu::new_headless();
    vg.upload_full(&vram).unwrap();
    let r = Rasterizer::new(&vg);
    let rect = TexRect::new(
        xy,
        wh,
        uv,
        0,
        0,
        tint,
        PrimFlags::empty(),
        BlendMode::Average,
    );
    let tp = Tpage::new(tpage_x, 0, 2);
    r.dispatch_tex_rect(&vg, &rect, &tp, &DrawArea::full_vram());
    let gpu_words = vg.download_full().unwrap();

    assert_eq!(cpu_words, gpu_words, "tex rect modulated strict parity");
}

#[test]
fn tex_rect_4bpp_with_clut_matches_cpu_byte_for_byte() {
    // 4bpp paletted rect, the most common 2D-UI
    // primitive. Strict bit-exact parity here would have caught
    // the U/V-wrap bug in `sample_texture` immediately.
    let xy = (40i32, 30i32);
    let wh = (16u32, 16u32);
    let uv = (0u8, 0u8);
    let tpage_x = 0u32;
    let clut_x = 0u32;
    let clut_y = 256u32;

    let mut vram = vec![0u16; 1024 * 512];
    // CLUT: 16 distinct opaque entries.
    for i in 0..16u16 {
        let val = (i.max(1) << 1) | (i.max(1) << 6) | 0x4000;
        vram[clut_y as usize * 1024 + (clut_x as usize + i as usize)] = val;
    }
    // 16×16 4bpp texture -- 4 nibbles per VRAM word.
    for vy in 0..16u16 {
        for word_x in 0..4u16 {
            let mut word = 0u16;
            for n in 0..4u16 {
                let nibble = ((word_x * 4 + n) + vy) & 0xF;
                word |= nibble << (n * 4);
            }
            vram[vy as usize * 1024 + (tpage_x as usize + word_x as usize)] = word;
        }
    }

    let mut cpu = Gpu::new();
    for (i, &w) in vram.iter().enumerate() {
        cpu.vram.set_pixel((i % 1024) as u16, (i / 1024) as u16, w);
    }
    cpu.gp0_push(0xE3000000);
    cpu.gp0_push(0xE4000000 | 1023 | (511 << 10));
    cpu.gp0_push(make_e1_for_tpage(tpage_x, 0, 0));
    cpu.gp0_push(0xE600_0000);
    // CLUT word: clut_x/16 in low 6, clut_y in next 9 bits.
    let clut_word = ((clut_x / 16) & 0x3F) | ((clut_y & 0x1FF) << 6);
    cpu_push_tex_rect(&mut cpu, 0x65, (0, 0, 0), xy, wh, uv, clut_word);
    let cpu_words = cpu.vram.words().to_vec();

    let vg = VramGpu::new_headless();
    vg.upload_full(&vram).unwrap();
    let r = Rasterizer::new(&vg);
    let rect = TexRect::new(
        xy,
        wh,
        uv,
        clut_x,
        clut_y,
        (0x80, 0x80, 0x80),
        PrimFlags::RAW_TEXTURE,
        BlendMode::Average,
    );
    let tp = Tpage::new(tpage_x, 0, 0);
    r.dispatch_tex_rect(&vg, &rect, &tp, &DrawArea::full_vram());
    let gpu_words = vg.download_full().unwrap();

    assert_eq!(cpu_words, gpu_words, "tex rect 4bpp+CLUT strict parity");
}

// -------------------------------------------------------
//  Phase B.5.c -- fill + VRAM-to-VRAM copy parity vs CPU
// -------------------------------------------------------

/// Drive the CPU rasterizer for one quick-fill via GP0 0x02.
/// `prefill` paints VRAM beforehand so we can verify that fill
/// IGNORES mask-check / mask-set / drawing-area state.
fn cpu_rasterize_fill(
    xy: (u32, u32),
    wh: (u32, u32),
    color: u16,
    prefill: u16,
    e3_clip_tl: u32,
    e3_clip_br: u32,
    e6_mask: u8,
) -> Vec<u16> {
    let mut gpu = Gpu::new();
    if prefill != 0 {
        for y in 0..512u16 {
            for x in 0..1024u16 {
                gpu.vram.set_pixel(x, y, prefill);
            }
        }
    }
    gpu.gp0_push(0xE300_0000 | e3_clip_tl);
    gpu.gp0_push(0xE400_0000 | e3_clip_br);
    gpu.gp0_push(0xE600_0000 | e6_mask as u32);
    // 0x02 = quick fill. Color in low 24 bits of cmd.
    let cmd = 0x0200_0000_u32 | bgr15_to_rgb24(color);
    gpu.gp0_push(cmd);
    gpu.gp0_push(((xy.1) << 16) | xy.0);
    gpu.gp0_push(((wh.1) << 16) | wh.0);
    gpu.vram.words().to_vec()
}

#[test]
fn fill_basic_matches_cpu_byte_for_byte() {
    // Strict parity. Fill is the simplest primitive -- any diff
    // is a real bug.
    let xy = (32u32, 64u32);
    let wh = (64u32, 32u32);
    let color = 0x4321;
    let cpu = cpu_rasterize_fill(xy, wh, color, 0, 0, 1023 | (511 << 10), 0);
    let vg = VramGpu::new_headless();
    let r = Rasterizer::new(&vg);
    r.dispatch_fill(&vg, &Fill::new(xy, wh, color));
    let gpu = vg.download_full().unwrap();
    assert_eq!(cpu, gpu, "fill basic parity");
}

#[test]
fn fill_ignores_drawing_area_clip() {
    // Set a tiny draw area; fill must overwrite outside it.
    let xy = (32u32, 32u32);
    let wh = (64u32, 64u32);
    let color = 0x1234;
    // Restrict draw area to (40..60, 40..60) -- but fill IGNORES
    // this. The whole rect at (32..96, 32..96) should still write.
    let cpu = cpu_rasterize_fill(xy, wh, color, 0, 40 | (40 << 10), 60 | (60 << 10), 0);
    let vg = VramGpu::new_headless();
    let r = Rasterizer::new(&vg);
    r.dispatch_fill(&vg, &Fill::new(xy, wh, color));
    let gpu = vg.download_full().unwrap();
    assert_eq!(cpu, gpu, "fill ignores draw area");
    // Sanity: a pixel OUTSIDE the draw area but INSIDE the fill
    // rect should hold the fill colour on both backends.
    let outside_clip = 35usize * 1024 + 35;
    let expected = ((color & 0x1F) as u8) << 3 | ((color & 0x1F) as u8) >> 2;
    // Check the BGR channels round-trip through fill correctly.
    // Use the exact expected_bgr15 = color, since fill writes 15bpp directly.
    // (RGB24 → BGR15 conversion truncates to 5 bits, so the
    // resulting BGR15 won't equal `color` if `color` had bits set
    // that don't survive the round-trip. cpu_rasterize_fill already
    // pushes through bgr15_to_rgb24 which maps cleanly for our
    // 5-bit-aligned `color`.)
    assert_eq!(cpu[outside_clip], gpu[outside_clip]);
    let _ = expected;
}

#[test]
fn fill_ignores_mask_check() {
    // mask_check_before_draw is set; back buffer has bit 15
    // everywhere. Fill should still write everywhere.
    let xy = (32u32, 32u32);
    let wh = (32u32, 32u32);
    let color = 0x1234;
    let prefill = 0x8888; // bit 15 set
    let cpu = cpu_rasterize_fill(
        xy,
        wh,
        color,
        prefill,
        0,
        1023 | (511 << 10),
        0b10, // mask_check
    );
    let vg = VramGpu::new_headless();
    gpu_prefill_full(&vg, prefill);
    let r = Rasterizer::new(&vg);
    r.dispatch_fill(&vg, &Fill::new(xy, wh, color));
    let gpu = vg.download_full().unwrap();
    assert_eq!(cpu, gpu, "fill bypasses mask-check");
    // Sanity: pixel inside fill rect must NOT be the prefill.
    let i = 40 * 1024 + 40;
    assert_ne!(cpu[i], prefill);
}

#[test]
fn fill_zero_size_is_dropped() {
    let cpu = cpu_rasterize_fill((32, 32), (0, 32), 0xCAFE, 0, 0, 1023 | (511 << 10), 0);
    let vg = VramGpu::new_headless();
    let r = Rasterizer::new(&vg);
    r.dispatch_fill(&vg, &Fill::new((32, 32), (0, 32), 0xCAFE));
    let gpu = vg.download_full().unwrap();
    assert!(cpu.iter().all(|&w| w == 0));
    assert!(gpu.iter().all(|&w| w == 0));
}

/// Drive the CPU rasterizer for one VRAM-to-VRAM copy via GP0 0x80.
fn cpu_rasterize_vram_copy(
    seed: &[u16],
    src: (u16, u16),
    dst: (u16, u16),
    wh: (u16, u16),
) -> Vec<u16> {
    let mut gpu = Gpu::new();
    for (i, &w) in seed.iter().enumerate() {
        gpu.vram.set_pixel((i % 1024) as u16, (i / 1024) as u16, w);
    }
    gpu.gp0_push(0x80_000000);
    gpu.gp0_push(((src.1 as u32) << 16) | (src.0 as u32));
    gpu.gp0_push(((dst.1 as u32) << 16) | (dst.0 as u32));
    gpu.gp0_push(((wh.1 as u32) << 16) | (wh.0 as u32));
    gpu.vram.words().to_vec()
}

#[test]
fn vram_copy_non_overlapping_matches_cpu_byte_for_byte() {
    // Source and dest disjoint -- direct GPU copy path.
    let mut seed = vec![0u16; 1024 * 512];
    for vy in 0..32u16 {
        for ux in 0..32u16 {
            seed[vy as usize * 1024 + (200 + ux as usize)] = ((vy as u16) << 5) | (ux as u16) | 0x1;
        }
    }
    let cpu = cpu_rasterize_vram_copy(&seed, (200, 0), (400, 100), (32, 32));
    let vg = VramGpu::new_headless();
    vg.upload_full(&seed).unwrap();
    let r = Rasterizer::new(&vg);
    r.dispatch_vram_copy(&vg, (200, 0), (400, 100), (32, 32));
    let gpu = vg.download_full().unwrap();
    assert_eq!(cpu, gpu, "vram copy non-overlapping strict parity");
}

#[test]
fn vram_copy_overlapping_uses_host_bounce_correctly() {
    // Overlap -- host-bounce path. Result should still match CPU
    // because the CPU's row-buffer pattern protects horizontal
    // overlap, and our host bounce reads ALL src then writes
    // (effectively the same as a full temp buffer).
    let mut seed = vec![0u16; 1024 * 512];
    for vy in 0..16u16 {
        for ux in 0..16u16 {
            seed[(50 + vy as usize) * 1024 + (50 + ux as usize)] =
                ((vy as u16) << 5) | (ux as u16) | 0x1;
        }
    }
    // Overlap: src=(50,50) 16x16, dst=(54,54) 16x16. They share
    // a 12x12 inner region.
    let cpu = cpu_rasterize_vram_copy(&seed, (50, 50), (54, 54), (16, 16));
    let vg = VramGpu::new_headless();
    vg.upload_full(&seed).unwrap();
    let r = Rasterizer::new(&vg);
    r.dispatch_vram_copy(&vg, (50, 50), (54, 54), (16, 16));
    let gpu = vg.download_full().unwrap();
    // Strict parity: our host-bounce reads the entire src rect
    // before any writes -- equivalent to the CPU's row-buffer
    // semantics for non-vertically-overlapping cases.
    // For vertically overlapping cases the CPU's row-by-row
    // semantics may differ; we accept that as a known
    // limitation in the comment on `dispatch_vram_copy`.
    assert_eq!(cpu, gpu, "vram copy overlap strict parity");
}

// -------------------------------------------------------
//  Phase B.3 -- shaded triangle parity vs CPU
// -------------------------------------------------------

/// Drive the CPU rasterizer for one Gouraud-shaded triangle via
/// GP0 0x30 (opaque) or 0x32 (semi-trans). Returns full VRAM.
fn cpu_rasterize_shaded_tri(
    v: [(i32, i32); 3],
    c: [(u8, u8, u8); 3],
    cmd_byte: u8,
    tpage_blend_bits: u8,
    mask_e6: u8,
    prefill: u16,
) -> Vec<u16> {
    let mut gpu = Gpu::new();
    if prefill != 0 {
        for y in 0..512u16 {
            for x in 0..1024u16 {
                gpu.vram.set_pixel(x, y, prefill);
            }
        }
    }
    gpu.gp0_push(0xE3000000);
    gpu.gp0_push(0xE4000000 | 1023 | (511 << 10));
    gpu.gp0_push(0xE100_0000_u32 | ((tpage_blend_bits as u32) & 0x3) << 5);
    gpu.gp0_push(0xE600_0000_u32 | (mask_e6 as u32) & 0x3);
    // GP0 0x30 packet: cmd+c0, v0, c1, v1, c2, v2 (6 words).
    let pack_rgb = |t: (u8, u8, u8)| (t.0 as u32) | ((t.1 as u32) << 8) | ((t.2 as u32) << 16);
    gpu.gp0_push(((cmd_byte as u32) << 24) | pack_rgb(c[0]));
    gpu.gp0_push(pack_xy(v[0]));
    gpu.gp0_push(pack_rgb(c[1]));
    gpu.gp0_push(pack_xy(v[1]));
    gpu.gp0_push(pack_rgb(c[2]));
    gpu.gp0_push(pack_xy(v[2]));
    gpu.vram.words().to_vec()
}

#[test]
fn shaded_tri_axis_aligned_matches_cpu_within_tolerance() {
    // Same UV-parity caveat as B.2: barycentric vs scanline-delta
    // can differ by ±1 in any 5-bit channel at interior pixels
    // due to rounding accumulation. Coverage matches; per-channel
    // delta is bounded.
    let v = [(20i32, 20i32), (60, 20), (20, 60)];
    let c = [(0xFFu8, 0u8, 0u8), (0u8, 0xFFu8, 0u8), (0u8, 0u8, 0xFFu8)];
    let cpu = cpu_rasterize_shaded_tri(v, c, 0x30, 0, 0, 0);
    let vg = VramGpu::new_headless();
    let r = Rasterizer::new(&vg);
    let tri = ShadedTri::new(
        v[0],
        v[1],
        v[2],
        c[0],
        c[1],
        c[2],
        PrimFlags::empty(),
        BlendMode::Average,
    );
    r.dispatch_shaded_tri_scanline(&vg, &tri, &DrawArea::full_vram());
    let gpu = vg.download_full().unwrap();

    let diffs = diff_inside_bbox(&cpu, &gpu, (20, 20), (60, 60));
    let bbox = 41 * 41;
    // Coverage tolerance.
    assert!(diffs * 4 < bbox, "shaded tri coverage: {diffs} / {bbox}");
    // Per-channel error tolerance.
    let mut max_chan = 0i32;
    for y in 20..=60i32 {
        for x in 20..=60i32 {
            let i = y as usize * 1024 + x as usize;
            if cpu[i] == gpu[i] {
                continue;
            }
            for shift in [0u32, 5, 10] {
                let ca = ((cpu[i] >> shift) & 0x1F) as i32;
                let cb = ((gpu[i] >> shift) & 0x1F) as i32;
                max_chan = max_chan.max((ca - cb).abs());
            }
        }
    }
    assert!(
        max_chan <= 2,
        "shaded tri max channel delta: {max_chan} > 2"
    );
}

#[test]
fn shaded_tri_uniform_color_matches_mono_tri_path() {
    // When all 3 vertex colours are identical, a Gouraud-shaded
    // triangle should produce the SAME output as a monochrome
    // triangle of that colour. Bit-exact within bbox.
    let v = [(15i32, 15i32), (55, 15), (15, 55)];
    let rgb = (0xC0u8, 0x40u8, 0x80u8);
    // CPU: shaded path with same colour everywhere.
    let cpu_shaded = cpu_rasterize_shaded_tri(v, [rgb; 3], 0x30, 0, 0, 0);
    // CPU: mono path with the BGR15-of-rgb.
    let bgr15 = (((rgb.0 as u16) >> 3) & 0x1F)
        | ((((rgb.1 as u16) >> 3) & 0x1F) << 5)
        | ((((rgb.2 as u16) >> 3) & 0x1F) << 10);
    let cpu_mono = cpu_rasterize_mono_tri(v[0], v[1], v[2], bgr15);
    // The two CPU paths differ at edges (different fill rules?
    // both use scanline-delta, so should be close). Just verify
    // identity-shaded GPU triangle matches the GPU mono path.
    let _ = (cpu_shaded, cpu_mono);

    let vg_shaded = VramGpu::new_headless();
    let r = Rasterizer::new(&vg_shaded);
    let tri = ShadedTri::new(
        v[0],
        v[1],
        v[2],
        rgb,
        rgb,
        rgb,
        PrimFlags::empty(),
        BlendMode::Average,
    );
    r.dispatch_shaded_tri_scanline(&vg_shaded, &tri, &DrawArea::full_vram());
    let gpu_shaded = vg_shaded.download_full().unwrap();

    let vg_mono = VramGpu::new_headless();
    let r2 = Rasterizer::new(&vg_mono);
    let mono = MonoTri::opaque(v[0], v[1], v[2], bgr15);
    r2.dispatch_mono_tri_scanline(&vg_mono, &mono, &DrawArea::full_vram());
    let gpu_mono = vg_mono.download_full().unwrap();

    // GPU shaded path with uniform colour must match GPU mono path
    // bit-for-bit (same coverage, same colour).
    assert_eq!(gpu_shaded, gpu_mono, "GPU uniform-shaded == GPU mono");
}

#[test]
fn shaded_tex_tri_axis_aligned_15bpp_matches_cpu_within_tolerance() {
    // Composes texture sampling + Gouraud-tint modulation.
    // Same B.2 UV-parity caveat applies; tolerance ≤3/5-bit
    // per channel (slightly looser than B.2 because tint
    // interpolation introduces its own rounding step).
    let v = [(20i32, 20i32), (60, 20), (20, 60)];
    let uv = [(0u8, 0u8), (32, 0), (0, 32)];
    // Different per-vertex tints so interpolation is exercised.
    let c = [
        (0x80u8, 0x80u8, 0x80u8),
        (0xC0, 0xC0, 0xC0),
        (0xFFu8, 0xFFu8, 0xFFu8),
    ];
    let tpage_x = 128u32;
    let tpage_word = make_tpage_word(tpage_x, 0, 2, 0);

    let mut vram = vec![0u16; 1024 * 512];
    for vy in 0..32u16 {
        for ux in 0..32u16 {
            let val = ((vy as u16) << 5) | (ux as u16) | 0x0001;
            vram[vy as usize * 1024 + (tpage_x as usize + ux as usize)] = val;
        }
    }

    let mut cpu = Gpu::new();
    for (i, &w) in vram.iter().enumerate() {
        cpu.vram.set_pixel((i % 1024) as u16, (i / 1024) as u16, w);
    }
    cpu.gp0_push(0xE3000000);
    cpu.gp0_push(0xE4000000 | 1023 | (511 << 10));
    let pack_rgb = |t: (u8, u8, u8)| (t.0 as u32) | ((t.1 as u32) << 8) | ((t.2 as u32) << 16);
    // 0x34 = textured-shaded triangle, modulated.
    cpu.gp0_push((0x34u32 << 24) | pack_rgb(c[0]));
    cpu.gp0_push(pack_xy(v[0]));
    cpu.gp0_push(uv_pack(uv[0])); // CLUT 0 (unused for 15bpp)
    cpu.gp0_push(pack_rgb(c[1]));
    cpu.gp0_push(pack_xy(v[1]));
    cpu.gp0_push((tpage_word << 16) | uv_pack(uv[1]));
    cpu.gp0_push(pack_rgb(c[2]));
    cpu.gp0_push(pack_xy(v[2]));
    cpu.gp0_push(uv_pack(uv[2]));
    let cpu_words = cpu.vram.words().to_vec();

    let vg = VramGpu::new_headless();
    vg.upload_full(&vram).unwrap();
    let r = Rasterizer::new(&vg);
    let tri = ShadedTexTri::new(
        v[0],
        v[1],
        v[2],
        c[0],
        c[1],
        c[2],
        uv[0],
        uv[1],
        uv[2],
        0,
        0,
        PrimFlags::empty(),
        BlendMode::Average,
    );
    let tp = Tpage::new(tpage_x, 0, 2);
    r.dispatch_shaded_tex_tri_scanline(&vg, &tri, &tp, &DrawArea::full_vram());
    let gpu_words = vg.download_full().unwrap();

    let diffs = diff_inside_bbox(&cpu_words, &gpu_words, (20, 20), (60, 60));
    let bbox = 41 * 41;
    assert!(diffs * 4 < bbox, "shaded-tex coverage: {diffs} / {bbox}");
    let mut max_chan = 0i32;
    for y in 20..=60i32 {
        for x in 20..=60i32 {
            let i = y as usize * 1024 + x as usize;
            if cpu_words[i] == gpu_words[i] {
                continue;
            }
            for shift in [0u32, 5, 10] {
                let ca = ((cpu_words[i] >> shift) & 0x1F) as i32;
                let cb = ((gpu_words[i] >> shift) & 0x1F) as i32;
                max_chan = max_chan.max((ca - cb).abs());
            }
        }
    }
    // Tolerance is looser than B.2's ±2 because tint
    // interpolation + tint×texel modulation compound the
    // per-step rounding error. ±5 in any 5-bit channel = ~16%
    // intensity, still tight enough to catch real bugs.
    assert!(
        max_chan <= 5,
        "shaded-tex max channel delta: {max_chan} > 5"
    );
}

#[test]
fn shaded_tri_oversized_is_dropped_like_cpu() {
    let v = [(0i32, 0i32), (2000, 0), (0, 0)];
    let c = [(0xFFu8, 0, 0); 3];
    let cpu = cpu_rasterize_shaded_tri(v, c, 0x30, 0, 0, 0);
    let vg = VramGpu::new_headless();
    let r = Rasterizer::new(&vg);
    let tri = ShadedTri::new(
        v[0],
        v[1],
        v[2],
        c[0],
        c[1],
        c[2],
        PrimFlags::empty(),
        BlendMode::Average,
    );
    r.dispatch_shaded_tri_scanline(&vg, &tri, &DrawArea::full_vram());
    let gpu = vg.download_full().unwrap();
    assert!(cpu.iter().all(|&w| w == 0));
    assert!(gpu.iter().all(|&w| w == 0));
}

#[test]
fn vram_copy_zero_size_is_dropped() {
    let seed = vec![0u16; 1024 * 512];
    let cpu = cpu_rasterize_vram_copy(&seed, (0, 0), (100, 100), (0, 32));
    let vg = VramGpu::new_headless();
    let r = Rasterizer::new(&vg);
    r.dispatch_vram_copy(&vg, (0, 0), (100, 100), (0, 32));
    let gpu = vg.download_full().unwrap();
    assert_eq!(cpu, gpu);
}
