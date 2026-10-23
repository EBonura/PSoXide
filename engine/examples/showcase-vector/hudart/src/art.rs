//! The pixel art of the stance HUD, drawn as code so that every repeated and symmetric part is
//! right by construction, and the palettes that colour it.
//!
//! One sprite per kind of part (health cell, bar end, emblem, gem), drawn once into a 4-bit atlas
//! with palette indices. A colour state (lit cell, empty cell, the other stance) is a CLUT, so two
//! cells in different states are the same pixels by construction. The long flat parts of the bars
//! are horizontal bands of flat colour.

use crate::palette::{desaturate, mix, scale, to_555, Rgb, WHITE};

pub const HORIZON: Rgb = [234, 82, 48];
pub const ZENITH: Rgb = [76, 202, 181];

/// Health cells and gems per HUD.
pub const COUNT: usize = 10;
/// Distance between cells, and between gems, in pixels.
pub const PITCH: i32 = 12;
/// The sloped left end of the bar: its left column. The flat part of the plate starts after it and
/// the sloped right end starts at `CAP_X`. The bar does not touch the emblem.
pub const LCAP_X: i32 = 27;
pub const LCAP_W: usize = 9;
pub const BAR_X0: i32 = LCAP_X + LCAP_W as i32;
pub const CAP_X: i32 = 151;
/// The bar behind sits this many rows above the front one; only these rows of it show.
pub const BACK_OFFSET: i32 = 9;
/// Top row of the front bar, in HUD coordinates.
pub const FRONT_Y: i32 = BACK_OFFSET;
/// Cell origin (left edge of its bottom row) and its top row inside the bar.
pub const CELL_X0: i32 = 33;
pub const CELL_ROW: i32 = 5;
/// Emblem top-left.
pub const EMBLEM_POS: (i32, i32) = (1, 6);
/// First gem's left edge.
pub const GEM_X0: i32 = 35;
/// Gem sprite height.
pub const GEM_H: i32 = 8;

/// What a row of the bar plate is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Row {
    /// The dark rim above the bar, which keeps its edge visible over a bright scene.
    Rim,
    /// The bright outline along the top.
    Out,
    /// The lit bevel under it.
    Hi,
    /// The plate beside the recess.
    Mid,
    /// The first row of the dark glass the cells sit in, in the plate's shadow.
    WellTop,
    /// The dark glass the cells sit in.
    Recess,
    /// The last row of the glass, catching a little light.
    WellLow,
    /// The bevel under the recess.
    Lip,
    /// The plate below.
    Shade,
    /// The dark outline along the bottom.
    Foot,
    /// The dark rim under the bar.
    Drop,
}

/// The rows of the bar plate, top to bottom. The recess holds the cells with one row of margin.
const ROWS: [Row; 18] = [
    Row::Rim,
    Row::Out,
    Row::Hi,
    Row::Mid,
    Row::WellTop,
    Row::Recess,
    Row::Recess,
    Row::Recess,
    Row::Recess,
    Row::Recess,
    Row::Recess,
    Row::Recess,
    Row::Recess,
    Row::WellLow,
    Row::Lip,
    Row::Shade,
    Row::Foot,
    Row::Drop,
];

/// Rows of the bar plate.
pub const BAR_H: i32 = ROWS.len() as i32;
/// The gem row.
pub const GEM_Y: i32 = FRONT_Y + BAR_H + 1;
/// The HUD canvas.
pub const CANVAS: (i32, i32) = (160, GEM_Y + GEM_H);

/// A paletted sprite.
#[derive(Clone)]
pub struct Sprite {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Sprite {
    fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            px: vec![0; w * h],
        }
    }

    fn set(&mut self, x: usize, y: usize, i: u8) {
        self.px[y * self.w + x] = i;
    }

    pub fn get(&self, x: usize, y: usize) -> u8 {
        self.px[y * self.w + x]
    }

    /// The sprite as text, one hex digit per pixel and `.` for transparent.
    pub fn ascii(&self) -> String {
        let mut s = String::new();
        for y in 0..self.h {
            for x in 0..self.w {
                match self.get(x, y) {
                    0 => s.push('.'),
                    i => s.push(char::from_digit(i as u32, 16).unwrap_or('?')),
                }
            }
            s.push('\n');
        }
        s
    }

    /// True when the sprite equals its mirror image, indices and all.
    #[cfg(test)]
    pub fn is_mirror_symmetric(&self) -> bool {
        (0..self.h).all(|y| (0..self.w).all(|x| self.get(x, y) == self.get(self.w - 1 - x, y)))
    }
}

// Cell indices.
const CELL_HI: u8 = 1;
const CELL_L1: u8 = 2;
const CELL_L2: u8 = 3;
const CELL_M: u8 = 4;
const CELL_D: u8 = 5;
const CELL_DD: u8 = 6;
const CELL_LEFT: u8 = 7;
const CELL_RIGHT: u8 = 8;

/// Columns the top-left corner of a cell sits to the right of its bottom-left one, in row `r`.
fn cell_lean(r: usize) -> usize {
    3 - r / 2
}

/// The health cell: a parallelogram 10 pixels wide, 8 rows tall, leaning 3 pixels over its height
/// in two-row steps. 13 x 8 with the lean.
pub fn cell() -> Sprite {
    let mut s = Sprite::new(13, 8);
    for r in 0..8 {
        let o = cell_lean(r);
        for c in o..o + 10 {
            let mut i = match r {
                0 => CELL_HI,
                1 => CELL_L1,
                2 | 3 => CELL_L2,
                4 | 5 => CELL_M,
                6 => CELL_D,
                _ => CELL_DD,
            };
            if r > 0 && r < 7 {
                if c == o {
                    i = CELL_LEFT;
                } else if c == o + 9 {
                    i = CELL_RIGHT;
                }
            }
            s.set(c, r, i);
        }
    }
    s
}

/// The cap index of a row kind.
fn row_index(k: Row) -> u8 {
    match k {
        Row::Out => 1,
        Row::Hi => 2,
        Row::Mid => 3,
        Row::Recess => 4,
        Row::Lip => 5,
        Row::Shade => 6,
        Row::Foot => 7,
        Row::WellTop => 8,
        Row::WellLow => 9,
        Row::Rim => 10,
        Row::Drop => 10,
    }
}

/// Width of the cap sprite.
pub const CAP_W: usize = 9;

/// The sloped right end of the bar, as tall as the plate, its end edge leaning one pixel per two
/// rows like the cells, with the dark rim following it on the outside.
pub fn cap() -> Sprite {
    let mut s = Sprite::new(CAP_W, BAR_H as usize);
    // Columns of the plate (outline included) in row `r`, for the rows that have the plate.
    let end = |r: usize| 8 - (r.max(1) - 1) / 2;
    for (r, &kind) in ROWS.iter().enumerate() {
        match kind {
            Row::Rim => {
                for c in 0..=end(1) {
                    s.set(c, r, row_index(Row::Rim));
                }
            }
            Row::Drop => {
                for c in 0..=end(r - 1) {
                    s.set(c, r, row_index(Row::Drop));
                }
            }
            _ => {
                let e = end(r);
                for c in 0..e {
                    let edge = c + 1 == e;
                    let wall = c + 2 == e;
                    let i = match kind {
                        Row::Out => row_index(Row::Out),
                        Row::Foot => row_index(Row::Foot),
                        _ if edge => {
                            if r + 3 >= ROWS.len() {
                                row_index(Row::Foot)
                            } else {
                                row_index(Row::Out)
                            }
                        }
                        Row::WellTop | Row::Recess | Row::WellLow if wall => row_index(Row::Hi),
                        k => row_index(k),
                    };
                    s.set(c, r, i);
                }
                s.set(e, r, row_index(Row::Rim));
            }
        }
    }
    s
}

/// The sloped left end of the bar, the right end's counterpart: the end edge leans one pixel per two
/// rows in the same direction as the cells (the bottom further left), the plate is to its right,
/// and the dark rim follows the edge on the outside.
pub fn lcap() -> Sprite {
    let mut s = Sprite::new(LCAP_W, BAR_H as usize);
    // Column of the end edge in row `r`.
    let edge = |r: usize| 8 - (r.max(1) - 1) / 2;
    for (r, &kind) in ROWS.iter().enumerate() {
        match kind {
            Row::Rim => {
                for c in edge(1) - 1..LCAP_W {
                    s.set(c, r, row_index(Row::Rim));
                }
            }
            Row::Drop => {
                for c in edge(r - 1) - 1..LCAP_W {
                    s.set(c, r, row_index(Row::Drop));
                }
            }
            _ => {
                let e = edge(r);
                for c in e..LCAP_W {
                    let is_edge = c == e;
                    let wall = c == e + 1;
                    let i = match kind {
                        Row::Out => row_index(Row::Out),
                        Row::Foot => row_index(Row::Foot),
                        _ if is_edge => {
                            if r + 3 >= ROWS.len() {
                                row_index(Row::Foot)
                            } else {
                                row_index(Row::Out)
                            }
                        }
                        Row::WellTop | Row::Recess | Row::WellLow if wall => row_index(Row::Hi),
                        k => row_index(k),
                    };
                    s.set(c, r, i);
                }
                s.set(e - 1, r, row_index(Row::Rim));
            }
        }
    }
    s
}

/// Emblem indices.
const EM_OUT: u8 = 1;
const EM_BODY: [u8; 5] = [2, 3, 4, 5, 6];
const EM_RING_HI: u8 = 7;
const EM_RING_LO: u8 = 8;
const EM_CORE_OUT: u8 = 9;
const EM_CORE: [u8; 4] = [10, 11, 12, 13];

/// The emblem: an octagon (7-pixel chamfers, all edges at 0 or 45 degrees, even size so that it
/// mirrors exactly) with a diamond core, 24 x 24.
pub fn emblem() -> Sprite {
    let mut s = Sprite::new(24, 24);
    for y in 0..24usize {
        let inset = if y < 7 {
            7 - y
        } else if y >= 17 {
            y - 16
        } else {
            0
        };
        for x in inset..24 - inset {
            let edge = x == inset || x == 23 - inset || y == 0 || y == 23;
            let ring = x == inset + 1 || x == 22 - inset || y == 1 || y == 22;
            let i = if edge {
                EM_OUT
            } else if ring {
                if y < 12 {
                    EM_RING_HI
                } else {
                    EM_RING_LO
                }
            } else {
                EM_BODY[match y {
                    0..=4 => 0,
                    5..=9 => 1,
                    10..=13 => 2,
                    14..=18 => 3,
                    _ => 4,
                }]
            };
            s.set(x, y, i);
        }
    }
    // The diamond core, 12 x 12 centred on the emblem.
    for k in 0..12usize {
        let half = if k < 6 { k + 1 } else { 12 - k };
        let y = 6 + k;
        for x in 12 - half..12 + half {
            let edge = x == 12 - half || x == 11 + half;
            let i = if edge {
                EM_CORE_OUT
            } else {
                EM_CORE[match k {
                    0..=2 => 0,
                    3..=5 => 1,
                    6..=8 => 2,
                    _ => 3,
                }]
            };
            s.set(x, y, i);
        }
    }
    s
}

// Gem indices.
const GEM_OUT: u8 = 1;
const GEM_FACET: [u8; 6] = [2, 3, 4, 5, 6, 7];
const GEM_SPEC: u8 = 8;

/// The energy gem: a hexagon with a flat top and bottom, outlined, two facets (bright above, dark
/// below) and a two-pixel glint on the top facet, 10 x 8.
pub fn gem() -> Sprite {
    let mut s = Sprite::new(10, 8);
    const WIDTH: [usize; 8] = [4, 6, 8, 10, 10, 8, 6, 4];
    for (y, &w) in WIDTH.iter().enumerate() {
        let x0 = (10 - w) / 2;
        for x in x0..x0 + w {
            let edge = x == x0 || x == x0 + w - 1 || y == 0 || y == 7;
            let i = if edge { GEM_OUT } else { GEM_FACET[y - 1] };
            s.set(x, y, i);
        }
    }
    s.set(4, 1, GEM_SPEC);
    s.set(5, 1, GEM_SPEC);
    s
}

/// A 16-entry palette: index 0 is transparent.
pub type Palette = [Rgb; 16];

fn pal(entries: &[(u8, Rgb)]) -> Palette {
    let mut p = [[0u8; 3]; 16];
    for &(i, c) in entries {
        p[i as usize] = c;
    }
    p
}

/// A stance colour as seen on the front bar, or dimmed on the one behind.
#[derive(Clone, Copy)]
pub struct Tone {
    pub base: Rgb,
}

impl Tone {
    pub fn front(base: Rgb) -> Self {
        Self { base }
    }

    /// The inactive stance's bar: darker and greyer.
    pub fn back(base: Rgb) -> Self {
        Self {
            base: scale(desaturate(base, 70), 120),
        }
    }
}

fn row_colour(k: Row, c: Rgb) -> Rgb {
    match k {
        Row::Out => mix(c, WHITE, 90),
        Row::Hi => scale(c, 250),
        Row::Mid => scale(c, 205),
        Row::WellTop => scale(c, 7),
        Row::Recess => scale(c, 15),
        Row::WellLow => scale(c, 30),
        Row::Lip => scale(c, 170),
        Row::Shade => scale(c, 120),
        Row::Foot => scale(c, 66),
        // Never pure black: a black CLUT entry would read as transparent or as glass.
        Row::Rim | Row::Drop => scale(c, 20).map(|v| v.max(8)),
    }
}

/// Whether a row is the translucent glass the cells sit in: the scene shows through it.
fn is_glass(k: Row) -> bool {
    matches!(k, Row::WellTop | Row::Recess | Row::WellLow)
}

/// The bar's colours by row: `(first row, one past the last, colour, glass)`, equal neighbouring
/// rows merged.
pub fn bands(t: Tone) -> Vec<(u8, u8, Rgb, bool)> {
    let mut out: Vec<(u8, u8, Row)> = Vec::new();
    for (r, &k) in ROWS.iter().enumerate() {
        match out.last_mut() {
            Some(last) if last.2 == k => last.1 = r as u8 + 1,
            _ => out.push((r as u8, r as u8 + 1, k)),
        }
    }
    out.into_iter()
        .map(|(a, b, k)| (a, b, row_colour(k, t.base), is_glass(k)))
        .collect()
}

/// Bands per bar, as the generated data declares them.
pub fn band_count() -> usize {
    bands(Tone::front(HORIZON)).len()
}

/// The cap palette indices that are glass (`WellTop`, `Recess`, `WellLow`).
pub const CAP_GLASS: [u8; 3] = [8, 4, 9];

pub fn cap_palette(t: Tone) -> Palette {
    let mut p = [[0u8; 3]; 16];
    for k in [
        Row::Rim,
        Row::Out,
        Row::Hi,
        Row::Mid,
        Row::WellTop,
        Row::Recess,
        Row::WellLow,
        Row::Lip,
        Row::Shade,
        Row::Foot,
    ] {
        p[row_index(k) as usize] = row_colour(k, t.base);
    }
    p
}

pub fn cell_palette(t: Tone, lit: bool) -> Palette {
    let c = t.base;
    if lit {
        pal(&[
            (CELL_HI, mix(c, WHITE, 140)),
            (CELL_L1, mix(c, WHITE, 64)),
            (CELL_L2, c),
            (CELL_M, scale(c, 218)),
            (CELL_D, scale(c, 174)),
            (CELL_DD, scale(c, 128)),
            (CELL_LEFT, mix(c, WHITE, 90)),
            (CELL_RIGHT, scale(c, 140)),
        ])
    } else {
        pal(&[
            (CELL_HI, scale(c, 77)),
            (CELL_L1, scale(c, 46)),
            (CELL_L2, scale(c, 36)),
            (CELL_M, scale(c, 31)),
            (CELL_D, scale(c, 26)),
            (CELL_DD, scale(c, 20)),
            (CELL_LEFT, scale(c, 66)),
            (CELL_RIGHT, scale(c, 18)),
        ])
    }
}

/// The emblem of the stance `active`, its core in the colour of the other one.
pub fn emblem_palette(active: Rgb, other: Rgb) -> Palette {
    let c = active;
    let o = other;
    pal(&[
        (EM_OUT, mix(c, WHITE, 90)),
        (EM_BODY[0], mix(c, WHITE, 50)),
        (EM_BODY[1], c),
        (EM_BODY[2], scale(c, 220)),
        (EM_BODY[3], scale(c, 184)),
        (EM_BODY[4], scale(c, 148)),
        (EM_RING_HI, mix(c, WHITE, 115)),
        (EM_RING_LO, scale(c, 112)),
        (EM_CORE_OUT, scale(o, 90)),
        (EM_CORE[0], mix(o, WHITE, 128)),
        (EM_CORE[1], mix(o, WHITE, 38)),
        (EM_CORE[2], scale(o, 205)),
        (EM_CORE[3], scale(o, 140)),
    ])
}

pub fn gem_palette(lit: bool) -> Palette {
    if lit {
        pal(&[
            (GEM_OUT, [96, 44, 0]),
            (GEM_FACET[0], [255, 242, 190]),
            (GEM_FACET[1], [255, 214, 100]),
            (GEM_FACET[2], [255, 200, 70]),
            (GEM_FACET[3], [232, 140, 16]),
            (GEM_FACET[4], [200, 100, 6]),
            (GEM_FACET[5], [168, 84, 0]),
            (GEM_SPEC, [255, 255, 236]),
        ])
    } else {
        pal(&[
            (GEM_OUT, [46, 28, 16]),
            (GEM_FACET[0], [66, 42, 24]),
            (GEM_FACET[1], [58, 36, 20]),
            (GEM_FACET[2], [52, 32, 18]),
            (GEM_FACET[3], [44, 28, 16]),
            (GEM_FACET[4], [40, 24, 14]),
            (GEM_FACET[5], [34, 20, 12]),
            (GEM_SPEC, [74, 48, 28]),
        ])
    }
}

/// A palette as 16 CLUT entries (entry 0 stays transparent). The entries listed in `glass` get the
/// semi-transparency bit: a sprite drawn with semi-transparency on blends them with the scene and
/// draws the rest opaque.
pub fn clut(p: &Palette, glass: &[u8]) -> [u16; 16] {
    let mut out = [0u16; 16];
    for (i, (o, c)) in out.iter_mut().zip(p.iter()).enumerate().skip(1) {
        *o = if glass.contains(&(i as u8)) {
            (to_555(*c) & 0x7fff) | 0x8000
        } else if *c == [0, 0, 0] {
            // Unused entry: transparent, not opaque black.
            0
        } else {
            to_555(*c)
        };
    }
    out
}

/// Where each sprite sits in the atlas.
/// A multiple of 8 so that a row is a whole number of VRAM words.
pub const ATLAS_W: usize = 64;
pub const ATLAS_H: usize = 24;
pub const ATLAS_POS: [(&str, (usize, usize)); 5] = [
    ("EMBLEM", (0, 0)),
    ("CAP", (24, 0)),
    ("LCAP", (50, 0)),
    ("CELL", (36, 0)),
    ("GEM", (36, 10)),
];

/// All sprites by atlas name.
pub fn sprites() -> [(&'static str, Sprite); 5] {
    [
        ("EMBLEM", emblem()),
        ("CAP", cap()),
        ("LCAP", lcap()),
        ("CELL", cell()),
        ("GEM", gem()),
    ]
}

/// The 4-bit atlas as VRAM halfwords, row by row, four texels per halfword with the leftmost texel
/// in the lowest nibble.
pub fn atlas_halfwords() -> Vec<u16> {
    let mut texels = vec![0u8; ATLAS_W * ATLAS_H];
    for ((_, sprite), (_, (ox, oy))) in sprites().iter().zip(ATLAS_POS.iter()) {
        for y in 0..sprite.h {
            for x in 0..sprite.w {
                texels[(oy + y) * ATLAS_W + ox + x] = sprite.get(x, y);
            }
        }
    }
    texels
        .chunks(4)
        .map(|q| q[0] as u16 | (q[1] as u16) << 4 | (q[2] as u16) << 8 | (q[3] as u16) << 12)
        .collect()
}
