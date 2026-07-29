// PSX hardware-renderer shader - VRAM-shaped target.
//
// The target is a `(1024 * S) × (512 * S)` texture (S = internal
// resolution multiplier). PSX vertex coords are in VRAM space
// (`pos.x ∈ 0..1024`, `pos.y ∈ 0..512`); the vertex shader maps them
// directly to NDC of that target. The wgpu viewport == target dims,
// so the rasterizer rasterises at S× density automatically - no
// per-S math anywhere in the shader.
//
// `HwVertex::flags` bit layout (must mirror `pipeline::flags`):
//   bits  0..=3   tpage_x_units   (× 64 = pixel x)
//   bit       4   tpage_y_index   (× 256 = pixel y; 0 or 256)
//   bits  5..=6   tex_depth        (0=4bpp, 1=8bpp, 2=15bpp)
//   bits  7..=12  clut_x_units    (× 16 = pixel x)
//   bits 13..=21  clut_y          (pixel y, 0..=511)
//   bit      22   TEXTURED
//   bit      23   RAW_TEXTURE     (skip tint modulate)
//   bit      24   SEMI_TRANS
//   bit      25   TEX_OPAQUE_PASS (discard STP texels)
//   bit      26   TEX_SEMI_PASS   (keep only STP texels)
//   bit      27   DITHER          (GP0(E1) bit 9 -- 4x4 ordered dither)
//
// `HwVertex::tex_window` packs GP0(E2) as four bytes:
//   bits  0..=7   mask_x in pixels
//   bits  8..=15  mask_y in pixels
//   bits 16..=23  offset_x in pixels
//   bits 24..=31  offset_y in pixels

const VRAM_W: u32 = 1024u;
const VRAM_H: u32 =  512u;
const VRAM_W_F: f32 = 1024.0;
const VRAM_H_F: f32 =  512.0;

@group(0) @binding(0) var vram: texture_2d<u32>;
// Texture filter in `.x`: 0 = nearest (PSX-native), 1 = bilinear. Global per
// frame; set by the toolbar toggle. `.y` holds the internal-resolution
// multiplier S, used to map a fragment back to its PSX-native pixel.
@group(0) @binding(1) var<uniform> u_texfilter: vec4<u32>;

struct VertexIn {
    @location(0) pos:   vec2<i32>,
    @location(1) color: vec4<f32>,
    @location(2) uv:    vec2<u32>,
    @location(3) flags: u32,
    @location(4) tex_window: u32,
}

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0)       color:    vec4<f32>,
    // UV interpolates as f32 so the rasterizer gives a fresh sample
    // per fragment. WGSL can't interpolate integer types - passing
    // `vec2<u32>` here was a silent no-op (defaults to flat) and the
    // fragment got the provoking vertex's UV for every pixel.
    @location(1)       uv:       vec2<f32>,
    @location(2) @interpolate(flat) flags: u32,
    @location(3) @interpolate(flat) tex_window: u32,
}

// ---------------------------------------------------------------------------
// Fullscreen VRAM->target blit. Used by wireframe mode to rebuild the scaled
// persistent target from the (journal-clean) CPU VRAM every frame, so stale
// edges never accumulate in the target at any internal scale. One oversized
// triangle, no vertex buffer.
// ---------------------------------------------------------------------------

struct BlitOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_blit(@builtin(vertex_index) vi: u32) -> BlitOut {
    // (-1,-1), (3,-1), (-1,3): one triangle covering the whole target.
    let x = f32(i32((vi & 1u) * 4u) - 1);
    let y = f32(i32((vi >> 1u) * 4u) - 1);
    var out: BlitOut;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    // NDC y points up, VRAM row 0 is the top row.
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

@fragment
fn fs_blit(in: BlitOut) -> @location(0) vec4<f32> {
    let tx = min(u32(in.uv.x * 1024.0), 1023u);
    let ty = min(u32(in.uv.y * 512.0), 511u);
    let word = textureLoad(vram, vec2<u32>(tx, ty), 0).r;
    return vec4<f32>(bgr15_to_rgb(word), 1.0);
}

const FLAG_TEXTURED:    u32 = 1u << 22u;
const FLAG_RAW_TEXTURE: u32 = 1u << 23u;
const FLAG_TEX_OPAQUE_PASS: u32 = 1u << 25u;
const FLAG_TEX_SEMI_PASS:   u32 = 1u << 26u;
const FLAG_DITHER:      u32 = 1u << 27u;

// PSX-SPX signed 4x4 ordered-dither offsets, indexed by
// `(y & 3) * 4 + (x & 3)` -- byte-for-byte the CPU rasterizer's
// `DITHER_OFFSETS` (emulator-core gpu/blend.rs).
const DITHER_OFFSETS = array<i32, 16>(
    -4, 0, -3, 1,
     2, -2, 3, -1,
    -3, 1, -4, 0,
     3, -1, 2, -2,
);

// Apply the ordered dither and truncate to 15bpp, then re-expand to
// display codes the way `bgr15_to_rgb` does. `rgb` is in display-code
// space (0..1 == 0..255); `frag` is the fragment's target-space
// position, converted here to the PSX-native pixel the CPU indexes
// the matrix by. Mirrors `dither_rgb`: add the signed offset to the
// 8-bit channel, clamp to 0..=255, keep the high 5 bits.
fn dither_to_bgr15(rgb: vec3<f32>, frag: vec2<f32>) -> vec3<f32> {
    let scale = f32(max(u_texfilter.y, 1u));
    let px = i32(floor(frag.x / scale));
    let py = i32(floor(frag.y / scale));
    let off = DITHER_OFFSETS[u32(py & 3) * 4u + u32(px & 3)];
    var out: vec3<f32>;
    for (var i = 0; i < 3; i++) {
        let c8 = i32(round(rgb[i] * 255.0));
        let c5 = u32(clamp(c8 + off, 0, 255) >> 3u);
        out[i] = f32((c5 << 3u) | (c5 >> 2u)) / 255.0;
    }
    return out;
}

@vertex
fn vs_main(in: VertexIn) -> VertexOut {
    // PSX-VRAM-space (0..1024, 0..512) → NDC (-1..+1, Y-flipped).
    let pos_psx = vec2<f32>(f32(in.pos.x), f32(in.pos.y));
    let ndc_xy = (pos_psx / vec2<f32>(VRAM_W_F, VRAM_H_F)) * 2.0 - 1.0;
    var out: VertexOut;
    out.position = vec4<f32>(ndc_xy.x, -ndc_xy.y, 0.0, 1.0);
    out.color    = in.color;
    out.uv       = vec2<f32>(f32(in.uv.x), f32(in.uv.y));
    out.flags    = in.flags;
    out.tex_window = in.tex_window;
    return out;
}

// PSX U/V are 8-bit per axis (so wrap on >255). Floor before the
// wrap matches the PSX nearest-neighbour rasterizer the compute
// backend already replicates pixel-for-pixel.
fn page_uv(uv: vec2<f32>) -> vec2<u32> {
    let ix = u32(max(uv.x, 0.0));
    let iy = u32(max(uv.y, 0.0));
    return vec2<u32>(ix & 0xFFu, iy & 0xFFu);
}

fn apply_tex_window(uv8: vec2<u32>, tex_window: u32) -> vec2<u32> {
    let mask_x = tex_window & 0xFFu;
    let mask_y = (tex_window >> 8u) & 0xFFu;
    let off_x = (tex_window >> 16u) & 0xFFu;
    let off_y = (tex_window >> 24u) & 0xFFu;
    return vec2<u32>(
        (uv8.x & (~mask_x & 0xFFu)) | (off_x & mask_x),
        (uv8.y & (~mask_y & 0xFFu)) | (off_y & mask_y),
    );
}

fn tpage_origin(flags: u32) -> vec2<u32> {
    let tx = (flags & 0xFu) * 64u;
    let ty = ((flags >> 4u) & 1u) * 256u;
    return vec2<u32>(tx, ty);
}

fn clut_origin(flags: u32) -> vec2<u32> {
    let cx = ((flags >> 7u) & 0x3Fu) * 16u;
    let cy = (flags >> 13u) & 0x1FFu;
    return vec2<u32>(cx, cy);
}

fn tex_depth(flags: u32) -> u32 {
    return (flags >> 5u) & 0x3u;
}

fn vram_load(x: u32, y: u32) -> u32 {
    let xx = x & (VRAM_W - 1u);
    let yy = y & (VRAM_H - 1u);
    return textureLoad(vram, vec2<i32>(i32(xx), i32(yy)), 0).r;
}

// BGR15 → display-code 0..1 RGB. Bit-replicates 5→8 the same way
// `Vram::to_rgba8` does on the CPU side, so colour quantisation
// matches the existing reference. This is intentionally NOT linear:
// PSX tint/modulate math happens in integer display-code space.
fn bgr15_to_rgb(word: u32) -> vec3<f32> {
    let r5 = word & 0x1Fu;
    let g5 = (word >> 5u) & 0x1Fu;
    let b5 = (word >> 10u) & 0x1Fu;
    let r8 = (r5 << 3u) | (r5 >> 2u);
    let g8 = (g5 << 3u) | (g5 >> 2u);
    let b8 = (b5 << 3u) | (b5 >> 2u);
    return vec3<f32>(f32(r8), f32(g8), f32(b8)) / 255.0;
}

fn srgb_channel_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        return c / 12.92;
    }
    return pow((c + 0.055) / 1.055, 2.4);
}

fn srgb_to_linear(rgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        srgb_channel_to_linear(rgb.r),
        srgb_channel_to_linear(rgb.g),
        srgb_channel_to_linear(rgb.b),
    );
}

// Sample the active texture page at PSX UV. Returns the raw 16-bit
// BGR15 texel; texel == 0 is reserved for transparency (caller
// discards).
fn sample_texel(flags: u32, uv8: vec2<u32>) -> u32 {
    let tp = tpage_origin(flags);
    let depth = tex_depth(flags);
    if depth == 0u {
        // 4 bpp: 4 indices per VRAM word, picked by uv.x % 4.
        let word = vram_load(tp.x + (uv8.x >> 2u), tp.y + uv8.y);
        let nibble = (word >> ((uv8.x & 3u) * 4u)) & 0xFu;
        let cl = clut_origin(flags);
        return vram_load(cl.x + nibble, cl.y);
    } else if depth == 1u {
        // 8 bpp: 2 indices per VRAM word, picked by uv.x & 1.
        let word = vram_load(tp.x + (uv8.x >> 1u), tp.y + uv8.y);
        let byte = (word >> ((uv8.x & 1u) * 8u)) & 0xFFu;
        let cl = clut_origin(flags);
        return vram_load(cl.x + byte, cl.y);
    }
    // 15 bpp direct.
    return vram_load(tp.x + uv8.x, tp.y + uv8.y);
}

// PSX modulate, in the CPU rasterizer's integer semantics
// (`modulate_tint_dithered`, emulator-core gpu/blend.rs):
//
//     c8 = min(tint8 * (t5 << 3) / 0x80, 255)     // truncating integer divide
//
// The float form this replaced (`tex * tint * 2`) is `tex * tint * 2/255`, and
// 2/255 != 1/128, so it ran ~0.4% bright -- up to 2/255 off, which is enough to
// flip a channel across a dither bucket boundary.
//
// `texel_rgb` arrives bit-replicated 5->8 (`bgr15_to_rgb`), but the CPU
// modulates `t5 << 3`. `t8 - floor(t8 / 32) == t5 << 3` for every 5-bit code,
// so undo the replication rather than re-quantising to 5 bits (which would
// band the filtered-texture paths).
//
// `RAW_TEXTURE` skips all of this and returns the texel verbatim.
fn modulate(texel_rgb: vec3<f32>, tint_rgba: vec4<f32>, raw: bool) -> vec3<f32> {
    if raw {
        return texel_rgb;
    }
    let t8 = round(texel_rgb * 255.0);
    let tex = t8 - floor(t8 / 32.0);
    let tint = round(tint_rgba.rgb * 255.0);
    return min(floor(tint * tex / 128.0), vec3<f32>(255.0)) / 255.0;
}

// One bilinear corner: premultiplied colour, coverage 0 on transparent texels
// (DuckStation "no edge blending" / binary alpha -- transparent neighbours
// never bleed colour into an opaque edge).
fn tex_corner(flags: u32, tex_window: u32, uvf: vec2<f32>) -> vec4<f32> {
    let uv8 = apply_tex_window(page_uv(uvf), tex_window);
    let w = sample_texel(flags, uv8);
    if w == 0u {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    return vec4<f32>(bgr15_to_rgb(w), 1.0);
}

// JINC2: Hyllian's windowed-jinc 2-lobe + anti-ringing (beetle-psx / DuckStation
// port). Samples a 4x4 texel neighbourhood via tex_corner (premultiplied,
// binary alpha), so transparent neighbours never bleed. Returns (rgb, coverage).
fn jinc_resampler(x: vec4<f32>) -> vec4<f32> {
    let wa = 1.382300768;
    let wb = 2.576105976;
    let a = sin(x * wa) * sin(x * wb) / (x * x);
    return select(a, vec4<f32>(wa * wb), x == vec4<f32>(0.0));
}

fn filter_jinc2(flags: u32, tw: u32, uv: vec2<f32>) -> vec4<f32> {
    let dx = vec2<f32>(1.0, 0.0);
    let dy = vec2<f32>(0.0, 1.0);
    let pc = uv;
    let tc = floor(pc - vec2<f32>(0.5, 0.5)) + vec2<f32>(0.5, 0.5);
    let w0 = jinc_resampler(vec4<f32>(length(pc - (tc - dx - dy)), length(pc - (tc - dy)), length(pc - (tc + dx - dy)), length(pc - (tc + 2.0 * dx - dy))));
    let w1 = jinc_resampler(vec4<f32>(length(pc - (tc - dx)), length(pc - tc), length(pc - (tc + dx)), length(pc - (tc + 2.0 * dx))));
    let w2 = jinc_resampler(vec4<f32>(length(pc - (tc - dx + dy)), length(pc - (tc + dy)), length(pc - (tc + dx + dy)), length(pc - (tc + 2.0 * dx + dy))));
    let w3 = jinc_resampler(vec4<f32>(length(pc - (tc - dx + 2.0 * dy)), length(pc - (tc + 2.0 * dy)), length(pc - (tc + dx + 2.0 * dy)), length(pc - (tc + 2.0 * dx + 2.0 * dy))));
    let c00 = tex_corner(flags, tw, tc - dx - dy); let c10 = tex_corner(flags, tw, tc - dy); let c20 = tex_corner(flags, tw, tc + dx - dy); let c30 = tex_corner(flags, tw, tc + 2.0 * dx - dy);
    let c01 = tex_corner(flags, tw, tc - dx); let c11 = tex_corner(flags, tw, tc); let c21 = tex_corner(flags, tw, tc + dx); let c31 = tex_corner(flags, tw, tc + 2.0 * dx);
    let c02 = tex_corner(flags, tw, tc - dx + dy); let c12 = tex_corner(flags, tw, tc + dy); let c22 = tex_corner(flags, tw, tc + dx + dy); let c32 = tex_corner(flags, tw, tc + 2.0 * dx + dy);
    let c03 = tex_corner(flags, tw, tc - dx + 2.0 * dy); let c13 = tex_corner(flags, tw, tc + 2.0 * dy); let c23 = tex_corner(flags, tw, tc + dx + 2.0 * dy); let c33 = tex_corner(flags, tw, tc + 2.0 * dx + 2.0 * dy);
    let wsum = dot(w0, vec4<f32>(1.0)) + dot(w1, vec4<f32>(1.0)) + dot(w2, vec4<f32>(1.0)) + dot(w3, vec4<f32>(1.0));
    var color = w0.x * c00.rgb + w0.y * c10.rgb + w0.z * c20.rgb + w0.w * c30.rgb;
    color += w1.x * c01.rgb + w1.y * c11.rgb + w1.z * c21.rgb + w1.w * c31.rgb;
    color += w2.x * c02.rgb + w2.y * c12.rgb + w2.z * c22.rgb + w2.w * c32.rgb;
    color += w3.x * c03.rgb + w3.y * c13.rgb + w3.z * c23.rgb + w3.w * c33.rgb;
    color = color / wsum;
    var alpha = w0.x * c00.a + w0.y * c10.a + w0.z * c20.a + w0.w * c30.a;
    alpha += w1.x * c01.a + w1.y * c11.a + w1.z * c21.a + w1.w * c31.a;
    alpha += w2.x * c02.a + w2.y * c12.a + w2.z * c22.a + w2.w * c32.a;
    alpha += w3.x * c03.a + w3.y * c13.a + w3.z * c23.a + w3.w * c33.a;
    alpha = alpha / wsum;
    // Anti-ringing: pull back toward the centre-4 range.
    let mn = min(min(c11.rgb, c21.rgb), min(c12.rgb, c22.rgb));
    let mx = max(max(c11.rgb, c21.rgb), max(c12.rgb, c22.rgb));
    color = mix(color, clamp(color, mn, mx), 0.8);
    if alpha > 0.0 {
        color = color / alpha;
    }
    return vec4<f32>(color, alpha);
}

// xBR: Hyllian's xBR-vertex, sample-time port (DuckStation shadergen). Samples a
// 5x5 texel neighbourhood via tex_corner (premultiplied, binary alpha). Edge-
// directed: sharp on drawn art, but poor on dithered/tiled 2D content.
const XBR_LUM: f32 = 1.0;
const XBR_EQ_TOL: f32 = 0.1176470588235294;
const XBR_STEEP: f32 = 2.2;
const XBR_DOMINANT: f32 = 3.6;
const XBR_W: vec4<f32> = vec4<f32>(0.2627, 0.6780, 0.0593, 0.5);

fn xbr_dist(a: vec4<f32>, b: vec4<f32>) -> f32 {
    let scaleB = 0.5 / (1.0 - XBR_W.b);
    let scaleR = 0.5 / (1.0 - XBR_W.r);
    let diff = a - b;
    let Y = dot(diff, XBR_W);
    let Cb = scaleB * (diff.b - Y);
    let Cr = scaleR * (diff.r - Y);
    return sqrt((XBR_LUM * Y) * (XBR_LUM * Y) + Cb * Cb + Cr * Cr);
}

fn xbr_eq(a: vec4<f32>, b: vec4<f32>) -> bool { return xbr_dist(a, b) < XBR_EQ_TOL; }
fn xbr_veq(a: vec4<f32>, b: vec4<f32>) -> bool { return all(a == b); }
fn xbr_vneq(a: vec4<f32>, b: vec4<f32>) -> bool { return !all(a == b); }

fn xbr_left_ratio(center: vec2<f32>, origin: vec2<f32>, direction: vec2<f32>, scale: vec2<f32>) -> f32 {
    let P0 = center - origin;
    let proj = direction * (dot(P0, direction) / dot(direction, direction));
    let distv = P0 - proj;
    let orth = vec2<f32>(-direction.y, direction.x);
    let side = sign(dot(P0, orth));
    let v = side * length(distv * scale);
    let s = sqrt(2.0) / 2.0;
    return smoothstep(-s, s, v);
}

fn filter_xbr(flags: u32, tw: u32, uv: vec2<f32>) -> vec4<f32> {
    let coords = uv;
    let scale = vec2<f32>(8.0, 8.0);
    let pos = fract(coords) - vec2<f32>(0.5, 0.5);
    let coord = coords - pos;

    let A = tex_corner(flags, tw, coord + vec2<f32>(-1.0, -1.0));
    let B = tex_corner(flags, tw, coord + vec2<f32>(0.0, -1.0));
    let C = tex_corner(flags, tw, coord + vec2<f32>(1.0, -1.0));
    let D = tex_corner(flags, tw, coord + vec2<f32>(-1.0, 0.0));
    let E = tex_corner(flags, tw, coord + vec2<f32>(0.0, 0.0));
    let F = tex_corner(flags, tw, coord + vec2<f32>(1.0, 0.0));
    let G = tex_corner(flags, tw, coord + vec2<f32>(-1.0, 1.0));
    let H = tex_corner(flags, tw, coord + vec2<f32>(0.0, 1.0));
    let I = tex_corner(flags, tw, coord + vec2<f32>(1.0, 1.0));

    var br = vec4<i32>(0, 0, 0, 0); // x|y / w|z

    if !((xbr_veq(E, F) && xbr_veq(H, I)) || (xbr_veq(E, H) && xbr_veq(F, I))) {
        let dist_H_F = xbr_dist(G, E) + xbr_dist(E, C) + xbr_dist(tex_corner(flags, tw, coord + vec2<f32>(0.0, 2.0)), I) + xbr_dist(I, tex_corner(flags, tw, coord + vec2<f32>(2.0, 0.0))) + 4.0 * xbr_dist(H, F);
        let dist_E_I = xbr_dist(D, H) + xbr_dist(H, tex_corner(flags, tw, coord + vec2<f32>(1.0, 2.0))) + xbr_dist(B, F) + xbr_dist(F, tex_corner(flags, tw, coord + vec2<f32>(2.0, 1.0))) + 4.0 * xbr_dist(E, I);
        let dom = (XBR_DOMINANT * dist_H_F) < dist_E_I;
        if (dist_H_F < dist_E_I) && xbr_vneq(E, F) && xbr_vneq(E, H) { br.z = select(1, 2, dom); }
    }
    if !((xbr_veq(D, E) && xbr_veq(G, H)) || (xbr_veq(D, G) && xbr_veq(E, H))) {
        let dist_G_E = xbr_dist(tex_corner(flags, tw, coord + vec2<f32>(-2.0, 1.0)), D) + xbr_dist(D, B) + xbr_dist(tex_corner(flags, tw, coord + vec2<f32>(-1.0, 2.0)), H) + xbr_dist(H, F) + 4.0 * xbr_dist(G, E);
        let dist_D_H = xbr_dist(tex_corner(flags, tw, coord + vec2<f32>(-2.0, 0.0)), G) + xbr_dist(G, tex_corner(flags, tw, coord + vec2<f32>(0.0, 2.0))) + xbr_dist(A, E) + xbr_dist(E, I) + 4.0 * xbr_dist(D, H);
        let dom = (XBR_DOMINANT * dist_D_H) < dist_G_E;
        if (dist_G_E > dist_D_H) && xbr_vneq(E, D) && xbr_vneq(E, H) { br.w = select(1, 2, dom); }
    }
    if !((xbr_veq(B, C) && xbr_veq(E, F)) || (xbr_veq(B, E) && xbr_veq(C, F))) {
        let dist_E_C = xbr_dist(D, B) + xbr_dist(B, tex_corner(flags, tw, coord + vec2<f32>(1.0, -2.0))) + xbr_dist(H, F) + xbr_dist(F, tex_corner(flags, tw, coord + vec2<f32>(2.0, -1.0))) + 4.0 * xbr_dist(E, C);
        let dist_B_F = xbr_dist(A, E) + xbr_dist(E, I) + xbr_dist(tex_corner(flags, tw, coord + vec2<f32>(0.0, -2.0)), C) + xbr_dist(C, tex_corner(flags, tw, coord + vec2<f32>(2.0, 0.0))) + 4.0 * xbr_dist(B, F);
        let dom = (XBR_DOMINANT * dist_B_F) < dist_E_C;
        if (dist_E_C > dist_B_F) && xbr_vneq(E, B) && xbr_vneq(E, F) { br.y = select(1, 2, dom); }
    }
    if !((xbr_veq(A, B) && xbr_veq(D, E)) || (xbr_veq(A, D) && xbr_veq(B, E))) {
        let dist_D_B = xbr_dist(tex_corner(flags, tw, coord + vec2<f32>(-2.0, 0.0)), A) + xbr_dist(A, tex_corner(flags, tw, coord + vec2<f32>(0.0, -2.0))) + xbr_dist(G, E) + xbr_dist(E, C) + 4.0 * xbr_dist(D, B);
        let dist_A_E = xbr_dist(tex_corner(flags, tw, coord + vec2<f32>(-2.0, -1.0)), D) + xbr_dist(D, H) + xbr_dist(tex_corner(flags, tw, coord + vec2<f32>(-1.0, -2.0)), B) + xbr_dist(B, F) + 4.0 * xbr_dist(A, E);
        let dom = (XBR_DOMINANT * dist_D_B) < dist_A_E;
        if (dist_D_B < dist_A_E) && xbr_vneq(E, D) && xbr_vneq(E, B) { br.x = select(1, 2, dom); }
    }

    var res = E;
    let inv_sqrt2 = 1.0 / sqrt(2.0);

    if br.z != 0 {
        let dist_F_G = xbr_dist(F, G);
        let dist_H_C = xbr_dist(H, C);
        let doLineBlend = br.z == 2 || !((br.y != 0 && !xbr_eq(E, G)) || (br.w != 0 && !xbr_eq(E, C)) || (xbr_eq(G, H) && xbr_eq(H, I) && xbr_eq(I, F) && xbr_eq(F, C) && !xbr_eq(E, I)));
        var origin = vec2<f32>(0.0, inv_sqrt2);
        var direction = vec2<f32>(1.0, -1.0);
        if doLineBlend {
            let shallow = (XBR_STEEP * dist_F_G <= dist_H_C) && xbr_vneq(E, G) && xbr_vneq(D, G);
            let steep = (XBR_STEEP * dist_H_C <= dist_F_G) && xbr_vneq(E, C) && xbr_vneq(B, C);
            origin = select(vec2<f32>(0.0, 0.5), vec2<f32>(0.0, 0.25), shallow);
            direction.x += select(0.0, 1.0, shallow);
            direction.y -= select(0.0, 1.0, steep);
        }
        let blendPix = mix(H, F, step(xbr_dist(E, F), xbr_dist(E, H)));
        res = mix(res, blendPix, xbr_left_ratio(pos, origin, direction, scale));
    }
    if br.w != 0 {
        let dist_H_A = xbr_dist(H, A);
        let dist_D_I = xbr_dist(D, I);
        let doLineBlend = br.w == 2 || !((br.z != 0 && !xbr_eq(E, A)) || (br.x != 0 && !xbr_eq(E, I)) || (xbr_eq(A, D) && xbr_eq(D, G) && xbr_eq(G, H) && xbr_eq(H, I) && !xbr_eq(E, G)));
        var origin = vec2<f32>(-inv_sqrt2, 0.0);
        var direction = vec2<f32>(1.0, 1.0);
        if doLineBlend {
            let shallow = (XBR_STEEP * dist_H_A <= dist_D_I) && xbr_vneq(E, A) && xbr_vneq(B, A);
            let steep = (XBR_STEEP * dist_D_I <= dist_H_A) && xbr_vneq(E, I) && xbr_vneq(F, I);
            origin = select(vec2<f32>(-0.5, 0.0), vec2<f32>(-0.25, 0.0), shallow);
            direction.y += select(0.0, 1.0, shallow);
            direction.x += select(0.0, 1.0, steep);
        }
        let blendPix = mix(H, D, step(xbr_dist(E, D), xbr_dist(E, H)));
        res = mix(res, blendPix, xbr_left_ratio(pos, origin, direction, scale));
    }
    if br.y != 0 {
        let dist_B_I = xbr_dist(B, I);
        let dist_F_A = xbr_dist(F, A);
        let doLineBlend = br.y == 2 || !((br.x != 0 && !xbr_eq(E, I)) || (br.z != 0 && !xbr_eq(E, A)) || (xbr_eq(I, F) && xbr_eq(F, C) && xbr_eq(C, B) && xbr_eq(B, A) && !xbr_eq(E, C)));
        var origin = vec2<f32>(inv_sqrt2, 0.0);
        var direction = vec2<f32>(-1.0, -1.0);
        if doLineBlend {
            let shallow = (XBR_STEEP * dist_B_I <= dist_F_A) && xbr_vneq(E, I) && xbr_vneq(H, I);
            let steep = (XBR_STEEP * dist_F_A <= dist_B_I) && xbr_vneq(E, A) && xbr_vneq(D, A);
            origin = select(vec2<f32>(0.5, 0.0), vec2<f32>(0.25, 0.0), shallow);
            direction.y -= select(0.0, 1.0, shallow);
            direction.x -= select(0.0, 1.0, steep);
        }
        let blendPix = mix(F, B, step(xbr_dist(E, B), xbr_dist(E, F)));
        res = mix(res, blendPix, xbr_left_ratio(pos, origin, direction, scale));
    }
    if br.x != 0 {
        let dist_D_C = xbr_dist(D, C);
        let dist_B_G = xbr_dist(B, G);
        let doLineBlend = br.x == 2 || !((br.w != 0 && !xbr_eq(E, C)) || (br.y != 0 && !xbr_eq(E, G)) || (xbr_eq(C, B) && xbr_eq(B, A) && xbr_eq(A, D) && xbr_eq(D, G) && !xbr_eq(E, A)));
        var origin = vec2<f32>(0.0, -inv_sqrt2);
        var direction = vec2<f32>(-1.0, 1.0);
        if doLineBlend {
            let shallow = (XBR_STEEP * dist_D_C <= dist_B_G) && xbr_vneq(E, C) && xbr_vneq(F, C);
            let steep = (XBR_STEEP * dist_B_G <= dist_D_C) && xbr_vneq(E, G) && xbr_vneq(H, G);
            origin = select(vec2<f32>(0.0, -0.5), vec2<f32>(0.0, -0.25), shallow);
            direction.x -= select(0.0, 1.0, shallow);
            direction.y += select(0.0, 1.0, steep);
        }
        let blendPix = mix(D, B, step(xbr_dist(E, B), xbr_dist(E, D)));
        res = mix(res, blendPix, xbr_left_ratio(pos, origin, direction, scale));
    }

    var alpha = res.a;
    var rgb = res.rgb;
    if alpha > 0.0 {
        rgb = rgb / alpha;
    }
    return vec4<f32>(rgb, alpha);
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let textured = (in.flags & FLAG_TEXTURED) != 0u;
    let dither = (in.flags & FLAG_DITHER) != 0u;
    if !textured {
        var rgb = in.color.rgb;
        if dither {
            rgb = dither_to_bgr15(rgb, in.position.xy);
        }
        return vec4<f32>(srgb_to_linear(rgb), in.color.a);
    }
    let uv8 = apply_tex_window(page_uv(in.uv), in.tex_window);
    let texel = sample_texel(in.flags, uv8);
    if texel == 0u {
        discard;
    }
    let stp = (texel & 0x8000u) != 0u;
    if ((in.flags & FLAG_TEX_OPAQUE_PASS) != 0u) && stp {
        discard;
    }
    if ((in.flags & FLAG_TEX_SEMI_PASS) != 0u) && !stp {
        discard;
    }
    // Bilinear (u_texfilter.x == 1) with binary-alpha edge handling: blend the
    // 2x2 texel colours premultiplied so transparent neighbours never bleed;
    // the silhouette/STP stay on the nearest (center) texel above. Seam-free
    // filtering isn't possible on PSX's packed VRAM (even DuckStation leaves
    // texture-boundary seams), so this matches the established emulator result.
    var tex_rgb = bgr15_to_rgb(texel);
    if u_texfilter.x == 1u {
        let uvf = in.uv - vec2<f32>(0.5, 0.5);
        let base = floor(uvf);
        let fr = uvf - base;
        let c00 = tex_corner(in.flags, in.tex_window, base);
        let c10 = tex_corner(in.flags, in.tex_window, base + vec2<f32>(1.0, 0.0));
        let c01 = tex_corner(in.flags, in.tex_window, base + vec2<f32>(0.0, 1.0));
        let c11 = tex_corner(in.flags, in.tex_window, base + vec2<f32>(1.0, 1.0));
        let acc = mix(mix(c00, c10, fr.x), mix(c01, c11, fr.x), fr.y);
        if acc.a > 0.0039 {
            tex_rgb = acc.rgb / acc.a;
        }
    } else if u_texfilter.x == 2u {
        let j = filter_jinc2(in.flags, in.tex_window, in.uv);
        if j.a > 0.0039 {
            tex_rgb = j.rgb;
        }
    } else if u_texfilter.x == 3u {
        let x = filter_xbr(in.flags, in.tex_window, in.uv);
        if x.a > 0.0039 {
            tex_rgb = x.rgb;
        }
    }
    let raw = (in.flags & FLAG_RAW_TEXTURE) != 0u;
    var rgb = modulate(tex_rgb, in.color, raw);
    // Raw-texture primitives bypass the modulator on silicon and are
    // never dithered; the translator already withholds FLAG_DITHER
    // for them, so `raw` needs no second check here.
    if dither {
        rgb = dither_to_bgr15(rgb, in.position.xy);
    }
    return vec4<f32>(srgb_to_linear(rgb), 1.0);
}
