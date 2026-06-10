// Gouraud-shaded triangle — silicon-matched coverage + plane
// interpolation.
//
// The host (`scanline.rs`) mirrors the CPU rasterizer's
// `for_each_tri_pixel`: the center-sampled Q32.32 DDA produces one
// `[left_x, right_x)` span per scanline, and each colour channel is
// a determinant plane `attr(x, y) = (base + x*dadx + y*dady) >> 24`
// in wrapping u32 arithmetic. Evaluating the same plane here makes
// the GPU output bit-exact with the CPU by construction.

struct ShadedTri {
    v0: vec2<i32>,
    v1: vec2<i32>,
    v2: vec2<i32>,
    bbox_min: vec2<i32>,
    bbox_max: vec2<i32>,
    c0: u32, c1: u32, c2: u32,
    flags: u32,
    _pad0: u32, _pad1: u32,
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
@group(0) @binding(1) var<uniform> prim: ShadedTri;
@group(0) @binding(2) var<uniform> draw_area: DrawArea;
@group(0) @binding(3) var<storage, read> rows: array<RowState>;
@group(0) @binding(4) var<uniform> consts: ScanlineConsts;

const VRAM_WIDTH: i32 = 1024;
const VRAM_HEIGHT: i32 = 512;

const FLAG_SEMI_TRANS: u32 = 1u << 0u;
const FLAG_MASK_CHECK: u32 = 1u << 1u;
const FLAG_MASK_SET:   u32 = 1u << 2u;
const FLAG_DITHER:     u32 = 1u << 6u;

const BLEND_AVERAGE:    u32 = 0u;
const BLEND_ADD:        u32 = 1u;
const BLEND_SUB:        u32 = 2u;
const BLEND_ADDQUARTER: u32 = 3u;

const DITHER_TABLE: array<u32, 16> = array<u32, 16>(
    7u, 0u, 6u, 1u, 2u, 5u, 3u, 4u,
    1u, 6u, 0u, 7u, 4u, 3u, 5u, 2u,
);

// Wrapping-u32 determinant-plane evaluation, identical to the CPU's
// `eval`: the result is already the 8-bit attribute value.
fn plane_eval(dadx: u32, dady: u32, base: u32, x: i32, y: i32) -> u32 {
    return (base + u32(x) * dadx + u32(y) * dady) >> 24u;
}

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

fn pack_rgb_5bit(r8: u32, g8: u32, b8: u32, dither: bool, x: i32, y: i32) -> u32 {
    let r = clamp(i32(r8), 0, 255);
    let g = clamp(i32(g8), 0, 255);
    let b = clamp(i32(b8), 0, 255);
    var rc = u32(r) >> 3u;
    var gc = u32(g) >> 3u;
    var bc = u32(b) >> 3u;
    if dither {
        let coeff = DITHER_TABLE[u32(y & 3) * 4u + u32(x & 3)];
        if rc < 0x1Fu && (u32(r) & 7u) > coeff { rc = rc + 1u; }
        if gc < 0x1Fu && (u32(g) & 7u) > coeff { gc = gc + 1u; }
        if bc < 0x1Fu && (u32(b) & 7u) > coeff { bc = bc + 1u; }
    }
    return rc | (gc << 5u) | (bc << 10u);
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

    let r = plane_eval(consts.r_dadx, consts.r_dady, consts.r_base, px, py);
    let g = plane_eval(consts.g_dadx, consts.g_dady, consts.g_base, px, py);
    let b = plane_eval(consts.b_dadx, consts.b_dady, consts.b_base, px, py);

    let dither = (prim.flags & FLAG_DITHER) != 0u;
    let fg = pack_rgb_5bit(r, g, b, dither, px, py);

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
        pixel = blend(existing, fg, mode);
    } else {
        pixel = fg;
    }
    if (prim.flags & FLAG_MASK_SET) != 0u {
        pixel = pixel | 0x8000u;
    }
    vram[idx] = pixel;
}
