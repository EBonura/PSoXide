// Shared PSX pixel semantics — prepended to every compute shader at
// pipeline creation (`rasterizer::shader_src`). WGSL has no include
// mechanism, so the host concatenates this header with each shader
// body; the bodies keep only their primitive struct, bindings,
// binding-coupled texture sampling and entry point.
//
// Everything here mirrors the silicon-verified CPU rasterizer
// (`emulator-core::gpu` + `gpu/blend.rs`): the 15bpp blend modes,
// the Redux-derived 4x4 dither rule, tint modulation, and the
// wrapping-u32 determinant-plane attribute evaluation.

const VRAM_WIDTH: i32 = 1024;
const VRAM_HEIGHT: i32 = 512;
const VRAM_WIDTH_U: u32 = 1024u;
const VRAM_HEIGHT_U: u32 = 512u;

// `prim.flags` bit layout — mirrors `primitive::PrimFlags` plus the
// blend mode in bits 8..=9.
const FLAG_SEMI_TRANS:  u32 = 1u << 0u;
const FLAG_MASK_CHECK:  u32 = 1u << 1u;
const FLAG_MASK_SET:    u32 = 1u << 2u;
const FLAG_RAW_TEXTURE: u32 = 1u << 3u;
const FLAG_FLIP_X:      u32 = 1u << 4u;
const FLAG_FLIP_Y:      u32 = 1u << 5u;
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

// Semi-transparency blend of fg over bg, both 15bpp words. The
// foreground's mask bit rides through.
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

// Wrapping-u32 determinant-plane evaluation, identical to the CPU's
// `eval`: the result is already the 8-bit attribute value.
fn plane_eval(dadx: u32, dady: u32, base: u32, x: i32, y: i32) -> u32 {
    return (base + u32(x) * dadx + u32(y) * dady) >> 24u;
}

// 8-bit RGB -> 15bpp with the optional 4x4 dither round-up rule.
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

// Texel tint modulation, packed 24-bit tint variant (flat-tint
// textured primitives). 0x80 = identity per channel.
fn modulate_tint(texel: u32, tint_packed: u32) -> u32 {
    let tr = texel & 0x1Fu;
    let tg = (texel >> 5u) & 0x1Fu;
    let tb = (texel >> 10u) & 0x1Fu;
    let cr = tint_packed & 0xFFu;
    let cg = (tint_packed >> 8u) & 0xFFu;
    let cb = (tint_packed >> 16u) & 0xFFu;
    let r = min((cr * tr) / 0x80u, 0x1Fu);
    let g = min((cg * tg) / 0x80u, 0x1Fu);
    let b = min((cb * tb) / 0x80u, 0x1Fu);
    return r | (g << 5u) | (b << 10u) | (texel & 0x8000u);
}

// Per-channel tint modulation (Gouraud-textured primitives).
fn modulate_5bit(texel: u32, tr: u32, tg: u32, tb: u32) -> u32 {
    let txr = texel & 0x1Fu;
    let txg = (texel >> 5u) & 0x1Fu;
    let txb = (texel >> 10u) & 0x1Fu;
    let r = min((tr * txr) / 0x80u, 0x1Fu);
    let g = min((tg * txg) / 0x80u, 0x1Fu);
    let b = min((tb * txb) / 0x80u, 0x1Fu);
    return r | (g << 5u) | (b << 10u) | (texel & 0x8000u);
}

// Dithered per-channel tint modulation: modulate in 8-bit space,
// dither, truncate to 5 bits (mirrors `modulate_tint_dithered`).
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
