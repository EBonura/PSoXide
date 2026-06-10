// Monochrome rectangle rasterizer (GP0 0x60..=0x63 + fixed-size
// 1×1 / 8×8 / 16×16 variants). Conceptually a tile of `MonoTri`
// minus the edge-function test — every pixel inside `wh` is
// covered. Drawing-area clip + RMW (mask + semi-trans) match
// `mono_tri.wgsl` byte-for-byte.

struct MonoRect {
    xy: vec2<i32>,
    wh: vec2<u32>,
    color: u32,
    flags: u32,
    _pad0: u32,
    _pad1: u32,
}

struct DrawArea {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

@group(0) @binding(0) var<storage, read_write> vram: array<u32>;
@group(0) @binding(1) var<uniform> prim: MonoRect;
@group(0) @binding(2) var<uniform> draw_area: DrawArea;

@compute @workgroup_size(8, 8)
fn rasterize(@builtin(global_invocation_id) gid: vec3<u32>) {
    // One thread per pixel inside the rect. Out-of-rect threads
    // (when `wh` doesn't divide WG size cleanly) bail.
    if gid.x >= prim.wh.x || gid.y >= prim.wh.y { return; }
    let px = prim.xy.x + i32(gid.x);
    let py = prim.xy.y + i32(gid.y);
    if px < draw_area.left || px > draw_area.right { return; }
    if py < draw_area.top || py > draw_area.bottom { return; }
    if px < 0 || px >= VRAM_WIDTH || py < 0 || py >= VRAM_HEIGHT { return; }

    let idx = u32(py * VRAM_WIDTH + px);

    let needs_read =
        ((prim.flags & FLAG_MASK_CHECK) != 0u) ||
        ((prim.flags & FLAG_SEMI_TRANS) != 0u);
    var existing: u32 = 0u;
    if needs_read { existing = vram[idx]; }
    if (prim.flags & FLAG_MASK_CHECK) != 0u {
        if (existing & 0x8000u) != 0u { return; }
    }
    var pixel: u32;
    if (prim.flags & FLAG_SEMI_TRANS) != 0u {
        let mode = (prim.flags >> 8u) & 0x3u;
        pixel = blend(existing, prim.color, mode);
    } else {
        pixel = prim.color;
    }
    if (prim.flags & FLAG_MASK_SET) != 0u {
        pixel = pixel | 0x8000u;
    }
    vram[idx] = pixel;
}
