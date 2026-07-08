// Screen-space xBR-lv2 upscaler, run as a post-process on the rendered frame.
//
// Ported from Hyllian's xBR-lv2 GLSL shader (MIT, Copyright (C) 2011-2016
// Hyllian - sergiogdb@gmail.com; "Incorporates some of the ideas from SABR
// shader. Thanks to Joshua Street."). Because it operates on the composited
// low-res image -- not per-texture -- it has none of the VRAM-packing seam
// artefacts a texture-space filter hits on PSX content.
//
// The source is the display sub-rect of the HW target (rendered at native
// scale when the filter is on). We sample the neighbourhood by integer texel
// (textureLoad), reconstruct edges, and write an XBR_SCALE-upscaled result.

const XBR_SCALE: f32 = 3.0;
const XBR_EQ_THRESHOLD: f32 = 15.0;
const lv2_cf: f32 = 2.0; // XBR_LV2_COEFFICIENT

const rgbw: vec3<f32> = vec3<f32>(14.352, 28.176, 5.472);

const Ao: vec4<f32> = vec4<f32>( 1.0, -1.0, -1.0, 1.0);
const Bo: vec4<f32> = vec4<f32>( 1.0,  1.0, -1.0,-1.0);
const Co: vec4<f32> = vec4<f32>( 1.5,  0.5, -0.5, 0.5);
const Ax: vec4<f32> = vec4<f32>( 1.0, -1.0, -1.0, 1.0);
const Bx: vec4<f32> = vec4<f32>( 0.5,  2.0, -0.5,-2.0);
const Cx: vec4<f32> = vec4<f32>( 1.0,  1.0, -0.5, 0.0);
const Ay: vec4<f32> = vec4<f32>( 1.0, -1.0, -1.0, 1.0);
const By: vec4<f32> = vec4<f32>( 2.0,  0.5, -2.0,-0.5);
const Cy: vec4<f32> = vec4<f32>( 2.0,  0.0, -1.0, 0.5);
const Ci: vec4<f32> = vec4<f32>(0.25, 0.25, 0.25, 0.25);

struct XbrU {
    // Display sub-rect within the source texture, in source texels.
    src_origin: vec2<f32>,
    src_size: vec2<f32>,
    // Output texture size in pixels (= src_size * XBR_SCALE).
    out_size: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var<uniform> u: XbrU;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>, // 0..1 across the output
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // Fullscreen triangle.
    var out: VsOut;
    let x = f32((vi << 1u) & 2u);
    let y = f32(vi & 2u);
    out.uv = vec2<f32>(x, y);
    out.pos = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    return out;
}

fn load(center: vec2<i32>, ox: i32, oy: i32) -> vec3<f32> {
    return textureLoad(src_tex, center + vec2<i32>(ox, oy), 0).rgb;
}

// Componentwise |A-B|.
fn df(a: vec4<f32>, b: vec4<f32>) -> vec4<f32> {
    return abs(a - b);
}
// 1.0 where components differ (float notEqual), 0.0 where equal.
fn diff(a: vec4<f32>, b: vec4<f32>) -> vec4<f32> {
    return step(vec4<f32>(1e-4), abs(a - b));
}
// 1.0 where components are within the equality threshold.
fn eq(a: vec4<f32>, b: vec4<f32>) -> vec4<f32> {
    return step(df(a, b), vec4<f32>(XBR_EQ_THRESHOLD));
}
fn neq(a: vec4<f32>, b: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(1.0) - eq(a, b);
}
// Weighted distance across the neighbourhood (small_details = 0 path).
fn wd(a: vec4<f32>, b: vec4<f32>, c: vec4<f32>, d: vec4<f32>, e: vec4<f32>, f: vec4<f32>, g: vec4<f32>, h: vec4<f32>) -> vec4<f32> {
    return df(a, b) + df(a, c) + df(d, e) + df(d, f) + 4.0 * df(g, h);
}
fn c_df(c1: vec3<f32>, c2: vec3<f32>) -> f32 {
    let d = abs(c1 - c2);
    return d.r + d.g + d.b;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Map this output pixel to a source-texel position.
    let src_pos = u.src_origin + in.uv * u.src_size;
    let fp = fract(src_pos);
    let center = vec2<i32>(floor(src_pos));

    // 3x3 core + the four extra arms (5x5 cross), matching the GLSL layout.
    let A1 = load(center, -1, -2); let B1 = load(center, 0, -2); let C1 = load(center, 1, -2);
    let A  = load(center, -1, -1); let B  = load(center, 0, -1); let C  = load(center, 1, -1);
    let D  = load(center, -1,  0); let E  = load(center, 0,  0); let F  = load(center, 1,  0);
    let G  = load(center, -1,  1); let H  = load(center, 0,  1); let I  = load(center, 1,  1);
    let G5 = load(center, -1,  2); let H5 = load(center, 0,  2); let I5 = load(center, 1,  2);
    let A0 = load(center, -2, -1); let D0 = load(center, -2, 0); let G0 = load(center, -2, 1);
    let C4 = load(center,  2, -1); let F4 = load(center,  2, 0); let I4 = load(center,  2, 1);

    let b = vec4<f32>(dot(B, rgbw), dot(D, rgbw), dot(H, rgbw), dot(F, rgbw));
    let c = vec4<f32>(dot(C, rgbw), dot(A, rgbw), dot(G, rgbw), dot(I, rgbw));
    let d = b.yzwx;
    let e = vec4<f32>(dot(E, rgbw));
    let f = b.wxyz;
    let g = c.zwxy;
    let h = b.zwxy;
    let i = c.wxyz;

    let i4 = vec4<f32>(dot(I4, rgbw), dot(C1, rgbw), dot(A0, rgbw), dot(G5, rgbw));
    let i5 = vec4<f32>(dot(I5, rgbw), dot(C4, rgbw), dot(A1, rgbw), dot(G0, rgbw));
    let h5 = vec4<f32>(dot(H5, rgbw), dot(F4, rgbw), dot(B1, rgbw), dot(D0, rgbw));
    let f4 = h5.yzwx;

    let fx   = Ao * fp.y + Bo * fp.x;
    let fx_l = Ax * fp.y + Bx * fp.x;
    let fx_u = Ay * fp.y + By * fp.x;

    let irlv0 = diff(e, f) * diff(e, h);
    // CORNER_C rule.
    let irlv1 = irlv0 * (neq(f, b) * neq(f, c) + neq(h, d) * neq(h, g)
        + eq(e, i) * (neq(f, f4) * neq(f, i4) + neq(h, h5) * neq(h, i5)) + eq(e, g) + eq(e, c));

    let irlv2l = diff(e, g) * diff(d, g);
    let irlv2u = diff(e, c) * diff(b, c);

    let delta   = vec4<f32>(1.0 / XBR_SCALE);
    let delta_l = vec4<f32>(0.5 / XBR_SCALE, 1.0 / XBR_SCALE, 0.5 / XBR_SCALE, 1.0 / XBR_SCALE);
    let delta_u = delta_l.yxwz;

    let fx45i = clamp((fx   + delta   - Co - Ci) / (2.0 * delta),   vec4<f32>(0.0), vec4<f32>(1.0));
    var fx45  = clamp((fx   + delta   - Co)      / (2.0 * delta),   vec4<f32>(0.0), vec4<f32>(1.0));
    var fx30  = clamp((fx_l + delta_l - Cx)      / (2.0 * delta_l), vec4<f32>(0.0), vec4<f32>(1.0));
    var fx60  = clamp((fx_u + delta_u - Cy)      / (2.0 * delta_u), vec4<f32>(0.0), vec4<f32>(1.0));

    let wd1 = wd(e, c,  g, i, h5, f4, h, f);
    let wd2 = wd(h, d, i5, f, i4,  b, e, i);

    let edri  = step(wd1, wd2) * irlv0;
    let edr   = step(wd1 + vec4<f32>(0.1), wd2) * step(vec4<f32>(0.5), irlv1);
    let edr_l = step(lv2_cf * df(f, g), df(h, c)) * irlv2l * edr;
    let edr_u = step(lv2_cf * df(h, c), df(f, g)) * irlv2u * edr;

    fx45  = edr   * fx45;
    fx30  = edr_l * fx30;
    fx60  = edr_u * fx60;
    let fx45i2 = edri * fx45i;

    let px = step(df(e, f), df(e, h));

    // SMOOTH_TIPS (CORNER_A not defined).
    let maximos = max(max(fx30, fx60), max(fx45, fx45i2));

    var res1 = E;
    res1 = mix(res1, mix(H, F, px.x), maximos.x);
    res1 = mix(res1, mix(B, D, px.z), maximos.z);

    var res2 = E;
    res2 = mix(res2, mix(F, B, px.y), maximos.y);
    res2 = mix(res2, mix(D, H, px.w), maximos.w);

    let res = mix(res1, res2, step(c_df(E, res1), c_df(E, res2)));
    return vec4<f32>(res, 1.0);
}
