//! Host rasterisers: one that draws what the GPU is sent (triangles in, pixels
//! out, PS1 fill rule) and one that draws the SVG shapes directly. Comparing
//! the two checks the cooker; comparing the first against an emulator frame
//! checks the runtime and the GPU path.

use psx_gpu::material::BlendMode;
use psx_gpu::prim::TriGouraud;
use psx_vector::{DrawError, Emit, GroupId, Placement, Scratch, Style, VectorAsset};

use crate::cook::{RefColor, RefShape};

/// A cooked asset held in 4-byte-aligned storage, as [`VectorAsset::parse`]
/// requires.
#[derive(Clone, Debug)]
pub struct AlignedAsset {
    words: Vec<u32>,
    len: usize,
}

impl AlignedAsset {
    /// Copy `bytes` into aligned storage.
    pub fn new(bytes: &[u8]) -> Self {
        let mut words = vec![0u32; bytes.len().div_ceil(4)];
        for (w, chunk) in words.iter_mut().zip(bytes.chunks(4)) {
            let mut b = [0u8; 4];
            b[..chunk.len()].copy_from_slice(chunk);
            *w = u32::from_le_bytes(b);
        }
        Self {
            words,
            len: bytes.len(),
        }
    }

    /// The bytes, aligned to four.
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: `words` owns `4 * words.len() >= len` initialised bytes, `u8`
        // has alignment 1, and the slice borrows `self`.
        unsafe { core::slice::from_raw_parts(self.words.as_ptr().cast::<u8>(), self.len) }
    }
}

/// An 8-bit RGB image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Image {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// `width * height * 3` bytes, row-major.
    pub rgb: Vec<u8>,
}

impl Image {
    /// A `width` x `height` image filled with `bg`.
    pub fn new(width: usize, height: usize, bg: [u8; 3]) -> Self {
        Self {
            width,
            height,
            rgb: bg
                .iter()
                .copied()
                .cycle()
                .take(width * height * 3)
                .collect(),
        }
    }

    /// The pixel at `(x, y)`.
    pub fn pixel(&self, x: usize, y: usize) -> [u8; 3] {
        let i = (y * self.width + x) * 3;
        [self.rgb[i], self.rgb[i + 1], self.rgb[i + 2]]
    }

    fn set(&mut self, x: usize, y: usize, c: [u8; 3]) {
        let i = (y * self.width + x) * 3;
        self.rgb[i..i + 3].copy_from_slice(&c);
    }

    /// Encode as binary PPM (P6).
    pub fn to_ppm(&self) -> Vec<u8> {
        let mut out = format!("P6\n{} {}\n255\n", self.width, self.height).into_bytes();
        out.extend_from_slice(&self.rgb);
        out
    }

    /// Nearest-neighbour enlargement by an integer `factor`.
    pub fn upscaled(&self, factor: usize) -> Self {
        let mut out = Self::new(self.width * factor, self.height * factor, [0, 0, 0]);
        for y in 0..out.height {
            for x in 0..out.width {
                out.set(x, y, self.pixel(x / factor, y / factor));
            }
        }
        out
    }

    /// Encode as an uncompressed PNG (stored deflate blocks), for review frames.
    pub fn to_png(&self) -> Vec<u8> {
        fn crc32(data: &[u8]) -> u32 {
            let mut crc = !0u32;
            for &b in data {
                crc ^= b as u32;
                for _ in 0..8 {
                    crc = if crc & 1 != 0 {
                        (crc >> 1) ^ 0xEDB8_8320
                    } else {
                        crc >> 1
                    };
                }
            }
            !crc
        }
        fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let mut body = kind.to_vec();
            body.extend_from_slice(data);
            out.extend_from_slice(&body);
            out.extend_from_slice(&crc32(&body).to_be_bytes());
        }
        let mut raw = Vec::with_capacity((self.width * 3 + 1) * self.height);
        for row in self.rgb.chunks(self.width * 3) {
            raw.push(0);
            raw.extend_from_slice(row);
        }
        let mut z = vec![0x78, 0x01];
        let mut blocks = raw.chunks(65_535).peekable();
        while let Some(block) = blocks.next() {
            z.push(blocks.peek().is_none() as u8);
            z.extend_from_slice(&(block.len() as u16).to_le_bytes());
            z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
            z.extend_from_slice(block);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for &byte in &raw {
            a = (a + byte as u32) % 65_521;
            b = (b + a) % 65_521;
        }
        z.extend_from_slice(&((b << 16) | a).to_be_bytes());
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&(self.width as u32).to_be_bytes());
        ihdr.extend_from_slice(&(self.height as u32).to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &ihdr);
        chunk(&mut out, b"IDAT", &z);
        chunk(&mut out, b"IEND", &[]);
        out
    }

    /// Decode a binary PPM (P6, maxval 255).
    pub fn from_ppm(bytes: &[u8]) -> Result<Self, String> {
        let mut pos = 0usize;
        let mut token = || -> Result<String, String> {
            while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            let start = pos;
            while pos < bytes.len() && !bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            if start == pos {
                return Err("truncated PPM header".into());
            }
            Ok(String::from_utf8_lossy(&bytes[start..pos]).into_owned())
        };
        if token()? != "P6" {
            return Err("not a P6 PPM".into());
        }
        let width: usize = token()?.parse().map_err(|_| "bad width")?;
        let height: usize = token()?.parse().map_err(|_| "bad height")?;
        if token()? != "255" {
            return Err("only maxval 255 is supported".into());
        }
        pos += 1;
        let data = bytes
            .get(pos..pos + width * height * 3)
            .ok_or("truncated PPM data")?;
        Ok(Self {
            width,
            height,
            rgb: data.to_vec(),
        })
    }
}

fn blend(mode: BlendMode, back: [u8; 3], fore: [u8; 3]) -> [u8; 3] {
    let mut out = [0u8; 3];
    for i in 0..3 {
        let (b, f) = (back[i] as i32, fore[i] as i32);
        out[i] = match mode {
            BlendMode::Opaque => f,
            BlendMode::Average => (b + f) / 2,
            BlendMode::Add => (b + f).min(255),
            BlendMode::Subtract => (b - f).max(0),
            BlendMode::AddQuarter => (b + f / 4).min(255),
        } as u8;
    }
    out
}

/// Rasterise one Gouraud triangle with the PS1 top-left fill rule. `origin`
/// is added to the vertices (the GPU's draw offset).
pub fn draw_triangle(img: &mut Image, tri: &TriGouraud, mode: BlendMode, origin: (i32, i32)) {
    let unpack = |pos: u32, col: u32| {
        (
            (pos as u16 as i16 as i64 + origin.0 as i64) * 2,
            ((pos >> 16) as u16 as i16 as i64 + origin.1 as i64) * 2,
            [
                (col & 0xFF) as i64,
                ((col >> 8) & 0xFF) as i64,
                ((col >> 16) & 0xFF) as i64,
            ],
        )
    };
    let mut v = [
        unpack(tri.v0, tri.color0_cmd),
        unpack(tri.v1, tri.color1),
        unpack(tri.v2, tri.color2),
    ];
    let edge = |a: (i64, i64), b: (i64, i64), p: (i64, i64)| {
        (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0)
    };
    let mut area = edge((v[0].0, v[0].1), (v[1].0, v[1].1), (v[2].0, v[2].1));
    if area == 0 {
        return;
    }
    if area < 0 {
        v.swap(1, 2);
        area = -area;
    }
    let min_x = (v.iter().map(|p| p.0).min().unwrap_or(0) / 2).max(0);
    let max_x = ((v.iter().map(|p| p.0).max().unwrap_or(0) + 1) / 2).min(img.width as i64);
    let min_y = (v.iter().map(|p| p.1).min().unwrap_or(0) / 2).max(0);
    let max_y = ((v.iter().map(|p| p.1).max().unwrap_or(0) + 1) / 2).min(img.height as i64);
    // Edge i is opposite vertex i. A top edge is horizontal heading +x, a left
    // edge heads up the screen; ties on those edges are inside.
    let owns = |a: (i64, i64), b: (i64, i64)| (a.1 == b.1 && b.0 > a.0) || b.1 < a.1;
    let edges = [(1usize, 2usize), (2, 0), (0, 1)];
    for y in min_y..max_y {
        for x in min_x..max_x {
            let p = (2 * x + 1, 2 * y + 1);
            let mut w = [0i64; 3];
            let mut inside = true;
            for (i, &(a, b)) in edges.iter().enumerate() {
                let (pa, pb) = ((v[a].0, v[a].1), (v[b].0, v[b].1));
                w[i] = edge(pa, pb, p);
                if w[i] < 0 || (w[i] == 0 && !owns(pa, pb)) {
                    inside = false;
                    break;
                }
            }
            if !inside {
                continue;
            }
            let mut c = [0u8; 3];
            for (ch, slot) in c.iter_mut().enumerate() {
                let sum = w[0] * v[0].2[ch] + w[1] * v[1].2[ch] + w[2] * v[2].2[ch];
                *slot = ((sum + area / 2) / area).clamp(0, 255) as u8;
            }
            let back = img.pixel(x as usize, y as usize);
            img.set(x as usize, y as usize, blend(mode, back, c));
        }
    }
}

/// Draw `group` through the runtime's `emit_group` into `img`.
pub fn draw_group(
    img: &mut Image,
    asset: &VectorAsset<'_>,
    group: GroupId,
    place: &Placement,
    style: &Style,
    origin: (i32, i32),
) -> Result<u32, DrawError> {
    let mut scratch: Box<Scratch<4096>> = Box::new(Scratch::new());
    let mut mode = BlendMode::Opaque;
    asset.emit_group(group, place, style, &mut scratch, &mut |step| match step {
        Emit::Blend(m) => mode = m,
        Emit::Triangle(t) => draw_triangle(img, t, mode, origin),
        // The GPU splits a quad into `0 1 2` and `1 2 3`.
        Emit::Quad(q) => {
            let tri = |a: (u32, u32), b: (u32, u32), c: (u32, u32)| TriGouraud {
                tag: 0,
                color0_cmd: a.1,
                v0: a.0,
                color1: b.1,
                v1: b.0,
                color2: c.1,
                v2: c.0,
            };
            let corner = |p: u32, c: u32| (p, c & 0xFF_FFFF);
            let (a, b) = (
                tri(
                    corner(q.v0, q.color0_cmd),
                    corner(q.v1, q.color1),
                    corner(q.v2, q.color2),
                ),
                tri(
                    corner(q.v1, q.color1),
                    corner(q.v2, q.color2),
                    corner(q.v3, q.color3),
                ),
            );
            draw_triangle(img, &a, mode, origin);
            draw_triangle(img, &b, mode, origin);
        }
    })
}

fn winding(contours: &[Vec<(f64, f64)>], p: (f64, f64)) -> i32 {
    let mut w = 0;
    for c in contours {
        for i in 0..c.len() {
            let (a, b) = (c[i], c[(i + 1) % c.len()]);
            if (a.1 <= p.1) != (b.1 <= p.1) {
                let t = (p.1 - a.1) / (b.1 - a.1);
                if a.0 + t * (b.0 - a.0) > p.0 {
                    w += if b.1 > a.1 { 1 } else { -1 };
                }
            }
        }
    }
    w
}

/// Draw the SVG shapes directly by sampling each pixel centre.
pub fn draw_reference(img: &mut Image, shapes: &[RefShape], origin: (i32, i32)) {
    for shape in shapes {
        let (mut lo, mut hi) = ((f64::MAX, f64::MAX), (f64::MIN, f64::MIN));
        for p in shape.contours.iter().flatten() {
            lo = (lo.0.min(p.0), lo.1.min(p.1));
            hi = (hi.0.max(p.0), hi.1.max(p.1));
        }
        let x0 = (lo.0.floor() as i64 + origin.0 as i64).max(0);
        let x1 = (hi.0.ceil() as i64 + origin.0 as i64).min(img.width as i64);
        let y0 = (lo.1.floor() as i64 + origin.1 as i64).max(0);
        let y1 = (hi.1.ceil() as i64 + origin.1 as i64).min(img.height as i64);
        let mode = psx_vector::format::blend_from_code(shape.blend).unwrap_or(BlendMode::Opaque);
        for y in y0..y1 {
            for x in x0..x1 {
                let p = (
                    (x - origin.0 as i64) as f64 + 0.5,
                    (y - origin.1 as i64) as f64 + 0.5,
                );
                let w = winding(&shape.contours, p);
                let filled = match shape.rule {
                    crate::paint::FillRule::NonZero => w != 0,
                    crate::paint::FillRule::EvenOdd => w & 1 != 0,
                };
                if !filled {
                    continue;
                }
                let rgb = match &shape.color {
                    RefColor::Solid(c) => *c,
                    RefColor::Linear { t, gradient } => {
                        gradient.color_at(t[0] * p.0 + t[1] * p.1 + t[2])
                    }
                };
                let k = |v: u8| (v as f64 * shape.scale).round().clamp(0.0, 255.0) as u8;
                let fore = [k(rgb.0), k(rgb.1), k(rgb.2)];
                let back = img.pixel(x as usize, y as usize);
                img.set(x as usize, y as usize, blend(mode, back, fore));
            }
        }
    }
}

/// How two images differ.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Diff {
    /// Pixels where any channel differs by more than the threshold.
    pub differing: usize,
    /// Pixels compared.
    pub total: usize,
    /// Largest single-channel difference.
    pub max_channel: u8,
    /// Mean absolute channel difference.
    pub mean_abs: f64,
}

impl Diff {
    /// Fraction of compared pixels that differ.
    pub fn fraction(&self) -> f64 {
        self.differing as f64 / self.total.max(1) as f64
    }
}

/// Compare `a` and `b` over the rectangle `[x0, x1) x [y0, y1)`.
pub fn compare(a: &Image, b: &Image, threshold: u8, region: [usize; 4]) -> Diff {
    let mut d = Diff {
        differing: 0,
        total: 0,
        max_channel: 0,
        mean_abs: 0.0,
    };
    let mut sum = 0u64;
    for y in region[1]..region[3].min(a.height).min(b.height) {
        for x in region[0]..region[2].min(a.width).min(b.width) {
            let (pa, pb) = (a.pixel(x, y), b.pixel(x, y));
            let worst = (0..3).map(|i| pa[i].abs_diff(pb[i])).max().unwrap_or(0);
            sum += (0..3).map(|i| pa[i].abs_diff(pb[i]) as u64).sum::<u64>();
            d.max_channel = d.max_channel.max(worst);
            d.differing += (worst > threshold) as usize;
            d.total += 1;
        }
    }
    d.mean_abs = sum as f64 / (d.total.max(1) * 3) as f64;
    d
}
