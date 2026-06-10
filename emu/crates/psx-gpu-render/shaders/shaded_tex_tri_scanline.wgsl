// Textured Gouraud-shaded triangle — silicon-matched coverage +
// plane interpolation.
//
// The host (`scanline.rs`) mirrors the CPU rasterizer's
// `for_each_tri_pixel`: the center-sampled Q32.32 DDA produces one
// `[left_x, right_x)` span per scanline, and all five attributes
// (R/G/B tint + U/V) come from determinant planes
// `attr(x, y) = (base + x*dadx + y*dady) >> 24` in wrapping u32
// arithmetic. Evaluating the same planes here makes the GPU output
// bit-exact with the CPU by construction.

struct ShadedTexTri {
    v0: vec2<i32>,
    v1: vec2<i32>,
    v2: vec2<i32>,
    bbox_min: vec2<i32>,
    bbox_max: vec2<i32>,
    c0: u32,
    c1: u32,
    c2: u32,
    uv0: u32,
    uv1: u32,
    uv2: u32,
    flags: u32,
    clut_x: u32,
    clut_y: u32,
    _pad: u32,
}

struct Tpage {
    tpage_x: u32,
    tpage_y: u32,
    tex_depth: u32,
    _pad: u32,
    tex_window_mask_x: u32,
    tex_window_mask_y: u32,
    tex_window_off_x: u32,
    tex_window_off_y: u32,
}

struct DrawArea {
    left: i32, top: i32, right: i32, bottom: i32,
}

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
@group(0) @binding(1) var<uniform> prim: ShadedTexTri;
@group(0) @binding(2) var<uniform> draw_area: DrawArea;
@group(0) @binding(3) var<uniform> tpage: Tpage;
@group(0) @binding(4) var<storage, read> rows: array<RowState>;
@group(0) @binding(5) var<uniform> consts: ScanlineConsts;

const VRAM_WIDTH: i32 = 1024;
const VRAM_HEIGHT: i32 = 512;
const VRAM_WIDTH_U: u32 = 1024u;
const VRAM_HEIGHT_U: u32 = 512u;

const FLAG_SEMI_TRANS:  u32 = 1u << 0u;
const FLAG_MASK_CHECK:  u32 = 1u << 1u;
const FLAG_MASK_SET:    u32 = 1u << 2u;
const FLAG_RAW_TEXTURE: u32 = 1u << 3u;
const FLAG_DITHER:      u32 = 1u << 6u;

const BLEND_AVERAGE:    u32 = 0u;
const BLEND_ADD:        u32 = 1u;
const BLEND_SUB:        u32 = 2u;
const BLEND_ADDQUARTER: u32 = 3u;

const DEPTH_4BPP:  u32 = 0u;
const DEPTH_8BPP:  u32 = 1u;
const DEPTH_15BPP: u32 = 2u;

const DITHER_TABLE: array<u32, 16> = array<u32, 16>(
    7u, 0u, 6u, 1u, 2u, 5u, 3u, 4u,
    1u, 6u, 0u, 7u, 4u, 3u, 5u, 2u,
);

// Wrapping-u32 determinant-plane evaluation, identical to the CPU's
// `eval`: the result is already the 8-bit attribute value.
fn plane_eval(dadx: u32, dady: u32, base: u32, x: i32, y: i32) -> u32 {
    return (base + u32(x) * dadx + u32(y) * dady) >> 24u;
}

// Texture sampling, blend, modulation — same as tex_tri_scanline.

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

fn sample_texture(u_in: u32, v_in: u32) -> u32 {
    let u8v = u_in & 0xFFu;
    let v8v = v_in & 0xFFu;
    let mx = tpage.tex_window_mask_x;
    let my = tpage.tex_window_mask_y;
    let ox = tpage.tex_window_off_x;
    let oy = tpage.tex_window_off_y;
    let uw = (u8v & ~mx) | (ox & mx);
    let vw = (v8v & ~my) | (oy & my);
    let tpy = (tpage.tpage_y + vw) & (VRAM_HEIGHT_U - 1u);
    var texel: u32 = 0u;
    switch tpage.tex_depth {
        case DEPTH_4BPP: {
            let tpx = (tpage.tpage_x + (uw >> 2u)) & (VRAM_WIDTH_U - 1u);
            let word = vram[tpy * VRAM_WIDTH_U + tpx];
            let idx = (word >> ((uw & 3u) * 4u)) & 0xFu;
            let cx = (prim.clut_x + idx) & (VRAM_WIDTH_U - 1u);
            let cy = prim.clut_y & (VRAM_HEIGHT_U - 1u);
            texel = vram[cy * VRAM_WIDTH_U + cx];
        }
        case DEPTH_8BPP: {
            let tpx = (tpage.tpage_x + (uw >> 1u)) & (VRAM_WIDTH_U - 1u);
            let word = vram[tpy * VRAM_WIDTH_U + tpx];
            let idx = (word >> ((uw & 1u) * 8u)) & 0xFFu;
            let cx = (prim.clut_x + idx) & (VRAM_WIDTH_U - 1u);
            let cy = prim.clut_y & (VRAM_HEIGHT_U - 1u);
            texel = vram[cy * VRAM_WIDTH_U + cx];
        }
        case DEPTH_15BPP, default: {
            let tpx = (tpage.tpage_x + uw) & (VRAM_WIDTH_U - 1u);
            texel = vram[tpy * VRAM_WIDTH_U + tpx];
        }
    }
    return texel;
}

fn modulate_5bit(texel: u32, tr: u32, tg: u32, tb: u32) -> u32 {
    let txr = texel & 0x1Fu;
    let txg = (texel >> 5u) & 0x1Fu;
    let txb = (texel >> 10u) & 0x1Fu;
    let r = min((tr * txr) / 0x80u, 0x1Fu);
    let g = min((tg * txg) / 0x80u, 0x1Fu);
    let b = min((tb * txb) / 0x80u, 0x1Fu);
    return r | (g << 5u) | (b << 10u) | (texel & 0x8000u);
}

fn modulate_dithered(texel: u32, tr: u32, tg: u32, tb: u32, x: i32, y: i32) -> u32 {
    let txr = (texel & 0x1Fu) << 3u;
    let txg = ((texel >> 5u) & 0x1Fu) << 3u;
    let txb = ((texel >> 10u) & 0x1Fu) << 3u;
    let r = min((tr * txr) / 0x80u, 0xFFu);
    let g = min((tg * txg) / 0x80u, 0xFFu);
    let b = min((tb * txb) / 0x80u, 0xFFu);
    let coeff = DITHER_TABLE[u32(y & 3) * 4u + u32(x & 3)];
    var rc = r >> 3u;
    var gc = g >> 3u;
    var bc = b >> 3u;
    if rc < 0x1Fu && (r & 7u) > coeff { rc = rc + 1u; }
    if gc < 0x1Fu && (g & 7u) > coeff { gc = gc + 1u; }
    if bc < 0x1Fu && (b & 7u) > coeff { bc = bc + 1u; }
    return rc | (gc << 5u) | (bc << 10u) | (texel & 0x8000u);
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

    let u = plane_eval(consts.u_dadx, consts.u_dady, consts.u_base, px, py);
    let v = plane_eval(consts.v_dadx, consts.v_dady, consts.v_base, px, py);

    let texel = sample_texture(u, v);
    if texel == 0u { return; }

    var fg: u32;
    if (prim.flags & FLAG_RAW_TEXTURE) != 0u {
        fg = texel;
    } else {
        let r = plane_eval(consts.r_dadx, consts.r_dady, consts.r_base, px, py);
        let g = plane_eval(consts.g_dadx, consts.g_dady, consts.g_base, px, py);
        let b = plane_eval(consts.b_dadx, consts.b_dady, consts.b_base, px, py);
        let dither = (prim.flags & FLAG_DITHER) != 0u;
        if dither {
            fg = modulate_dithered(texel, r, g, b, px, py);
        } else {
            fg = modulate_5bit(texel, r, g, b);
        }
    }

    let idx = u32(py * VRAM_WIDTH + px);
    let semi_trans_active =
        ((prim.flags & FLAG_SEMI_TRANS) != 0u) && ((texel & 0x8000u) != 0u);
    let needs_read = ((prim.flags & FLAG_MASK_CHECK) != 0u) || semi_trans_active;
    var existing: u32 = 0u;
    if needs_read { existing = vram[idx]; }
    if (prim.flags & FLAG_MASK_CHECK) != 0u {
        if (existing & 0x8000u) != 0u { return; }
    }
    var pixel: u32;
    if semi_trans_active {
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
