//! Integer colour maths for the stance HUD palettes. Everything is 8-bit RGB; the GPU keeps the top
//! five bits, so the baked colours are quantised once, in [`to_555`], and the rectangles and the
//! CLUT entries that must match are built from the same 8-bit value.

/// 8-bit RGB.
pub type Rgb = [u8; 3];

pub const WHITE: Rgb = [255, 255, 255];

/// `a` scaled by `k / 256`, saturating.
pub fn scale(a: Rgb, k: u32) -> Rgb {
    a.map(|c| ((c as u32 * k) >> 8).min(255) as u8)
}

/// `a` blended towards `b` by `t / 256`.
pub fn mix(a: Rgb, b: Rgb, t: u32) -> Rgb {
    let m = |x: u8, y: u8| ((x as u32 * (256 - t) + y as u32 * t) >> 8) as u8;
    [m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2])]
}

/// Pull the colour towards its own grey by `t / 256`.
pub fn desaturate(a: Rgb, t: u32) -> Rgb {
    let grey = ((a[0] as u32 * 77 + a[1] as u32 * 150 + a[2] as u32 * 29) >> 8) as u8;
    mix(a, [grey; 3], t)
}

/// The 15-bit CLUT entry for an opaque colour. An all-black entry would read as transparent, so it
/// gets bit 15 (opaque black); every other entry keeps bit 15 clear.
pub fn to_555(c: Rgb) -> u16 {
    let v = (c[0] as u16 >> 3) | ((c[1] as u16 >> 3) << 5) | ((c[2] as u16 >> 3) << 10);
    if v == 0 {
        0x8000
    } else {
        v
    }
}
