// Monochrome triangle — silicon-matched coverage.
//
// The host runs the CPU rasterizer's center-sampled Q32.32 DDA
// (`emulator-core::gpu::rasterize_triangle`) and ships one
// `[left_x, right_x)` span per scanline; this shader only tests the
// pixel against its row's span and applies the draw-area clamp,
// which is exactly the CPU loop's per-row
// `xs = max(left, draw_left) .. xe = min(right, draw_right + 1)`.

struct MonoTri {
    v0: vec2<i32>,
    v1: vec2<i32>,
    v2: vec2<i32>,
    bbox_min: vec2<i32>,
    bbox_max: vec2<i32>,
    color: u32,
    flags: u32,
}

struct DrawArea { left: i32, top: i32, right: i32, bottom: i32 }

struct RowState {
    left_x: i32,
    right_x: i32,
}

struct ScanlineConsts {
    y_min: i32, y_max: i32,
    _pad0: u32, _pad1: u32,
    r_dadx: u32, r_dady: u32, r_base: u32, _pad2: u32,
    g_dadx: u32, g_dady: u32, g_base: u32, _pad3: u32,
    b_dadx: u32, b_dady: u32, b_base: u32, _pad4: u32,
    u_dadx: u32, u_dady: u32, u_base: u32, _pad5: u32,
    v_dadx: u32, v_dady: u32, v_base: u32, _pad6: u32,
}

@group(0) @binding(0) var<storage, read_write> vram: array<u32>;
@group(0) @binding(1) var<uniform> prim: MonoTri;
@group(0) @binding(2) var<uniform> draw_area: DrawArea;
@group(0) @binding(3) var<storage, read> rows: array<RowState>;
@group(0) @binding(4) var<uniform> consts: ScanlineConsts;

const VRAM_WIDTH: i32 = 1024;
const VRAM_HEIGHT: i32 = 512;

const FLAG_SEMI_TRANS: u32 = 1u << 0u;
const FLAG_MASK_CHECK: u32 = 1u << 1u;
const FLAG_MASK_SET:   u32 = 1u << 2u;

const BLEND_AVERAGE:    u32 = 0u;
const BLEND_ADD:        u32 = 1u;
const BLEND_SUB:        u32 = 2u;
const BLEND_ADDQUARTER: u32 = 3u;

fn blend(bg_word: u32, fg_word: u32, mode: u32) -> u32 {
    let br = i32(bg_word & 0x1Fu);
    let bg = i32((bg_word >> 5u) & 0x1Fu);
    let bb = i32((bg_word >> 10u) & 0x1Fu);
    let fr = i32(fg_word & 0x1Fu);
    let fg = i32((fg_word >> 5u) & 0x1Fu);
    let fb = i32((fg_word >> 10u) & 0x1Fu);
    var r: i32; var g: i32; var b: i32;
    switch mode {
        case BLEND_AVERAGE: {
            r = (br >> 1u) + (fr >> 1u);
            g = (bg >> 1u) + (fg >> 1u);
            b = (bb >> 1u) + (fb >> 1u);
        }
        case BLEND_ADD: {
            r = min(br + fr, 31); g = min(bg + fg, 31); b = min(bb + fb, 31);
        }
        case BLEND_SUB: {
            r = max(br - fr, 0); g = max(bg - fg, 0); b = max(bb - fb, 0);
        }
        case BLEND_ADDQUARTER, default: {
            r = min(br + (fr >> 2u), 31);
            g = min(bg + (fg >> 2u), 31);
            b = min(bb + (fb >> 2u), 31);
        }
    }
    return u32(r) | (u32(g) << 5u) | (u32(b) << 10u) | (fg_word & 0x8000u);
}

@compute @workgroup_size(8, 8)
fn rasterize(@builtin(global_invocation_id) gid: vec3<u32>) {
    let px = prim.bbox_min.x + i32(gid.x);
    let py = prim.bbox_min.y + i32(gid.y);
    if px > prim.bbox_max.x || py > prim.bbox_max.y { return; }
    if py < consts.y_min || py > consts.y_max { return; }
    if px < draw_area.left || px > draw_area.right { return; }
    if py < draw_area.top || py > draw_area.bottom { return; }
    if px < 0 || px >= VRAM_WIDTH || py < 0 || py >= VRAM_HEIGHT { return; }

    let row = rows[u32(py - consts.y_min)];
    if px < row.left_x || px >= row.right_x { return; }

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
