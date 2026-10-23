//! The pixel art of the stance HUD, drawn as code so that every repeated and symmetric part is
//! right by construction, and the palettes that colour it.
//!
//! A bar comes in a few sizes, each drawn as pixel art of its own (never scaled): the front one at
//! full size and smaller ones for the bar further back, and for the frames in between when the bars
//! trade places. One sprite per kind of part and size (health cell, left end, right end), plus the
//! emblem and the gem, is drawn once into a 4-bit atlas with palette indices. A colour state (lit
//! cell, empty cell, the other stance, how far back a bar is) is a CLUT, so two cells in different
//! states are the same pixels by construction. The long flat parts of the bars are horizontal bands
//! of flat colour.

use crate::palette::{desaturate, mix, scale, to_555, Rgb, WHITE};

pub const HORIZON: Rgb = [234, 82, 48];
pub const ZENITH: Rgb = [76, 202, 181];

/// Health cells and gems per HUD.
pub const COUNT: usize = 10;
/// Number of sizes of a bar: 0 is full size, the others get smaller and darker.
pub const SIZES: usize = 4;
/// The size of the bar behind (1 to `SIZES - 1`); the sizes between are used while the bars move.
pub const BACK_SIZE: usize = 2;
/// Left edge of the left end of every bar: the bars are aligned on their left ends.
pub const LCAP_X: i32 = 27;
/// Width of the sprite of either end of a bar.
pub const END_W: usize = 9;
/// How far the back bar's left end is to the right of the front bar's.
pub const BACK_X: i32 = 0;
/// How many rows the bar behind sits above the front bar.
pub const LIFT: i32 = 9;
/// How many rows lower than the front bar's top minus `LIFT` the bar behind is drawn: its top is at row
/// `BACK_DROP`, so the more it is, the more the front bar covers of it.
pub const BACK_DROP: i32 = 4;
/// Top row of the front bar, in HUD coordinates (the bar behind starts at row 0).
pub const FRONT_Y: i32 = LIFT;
/// Emblem top-left.
pub const EMBLEM_POS: (i32, i32) = (1, 6);
/// First gem's left edge, and the distance between gems.
pub const GEM_X0: i32 = 35;
pub const GEM_PITCH: i32 = 12;
/// Gem sprite height.
pub const GEM_H: i32 = 8;

/// What a row of the bar plate is.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Row {
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

/// One size of the bar: its pixel geometry. Everything but the colours follows from these numbers.
#[derive(Clone)]
pub struct Geo {
    /// Distance between cells.
    pub pitch: i32,
    /// Width of the body of a cell (the parallelogram, not counting its lean).
    pub body: i32,
    /// Height of a cell.
    pub cell_h: usize,
    /// The rows of the bar plate, top to bottom. The recess holds the cells, one row each.
    pub rows: Vec<Row>,
    /// How much the bar fades with distance, 0 (front) to `SIZES - 1`.
    pub depth: usize,
}

impl Geo {
    /// Columns a cell leans over its height: one pixel per two rows, the top to the right.
    pub fn lean(&self) -> usize {
        (self.cell_h - 1) / 2
    }

    /// First row of the recess, where the cells start.
    pub fn cell_row(&self) -> usize {
        self.rows
            .iter()
            .position(|&r| r == Row::Recess)
            .unwrap_or(0)
    }

    /// 1 when the end edges must step on odd rows to run parallel to the cells' edges, else 0.
    fn phase(&self) -> usize {
        (self.cell_row() + 1) % 2
    }

    /// Rows of the bar.
    pub fn bar_h(&self) -> i32 {
        self.rows.len() as i32
    }

    /// Column of the end edge in bar row `r`: 8 at the top, one less every second row.
    pub fn edge_col(&self, r: usize) -> usize {
        8 - (r.max(1) - 1 + self.phase()) / 2
    }

    /// Steps of the end edge from the top row to the first cell row.
    fn k(&self) -> i32 {
        ((self.cell_row() as i32) - 1 + self.phase() as i32) / 2
    }

    /// Left edge of a cell's bottom row: fixes the gap of one pixel of well between the first cell
    /// and the left end.
    pub fn cell_x0(&self) -> i32 {
        LCAP_X - self.lean() as i32 + 11 - self.k()
    }

    /// Left column of the right end: the same gap after the last cell.
    pub fn cap_x(&self) -> i32 {
        self.cell_x0() + 9 * self.pitch + self.lean() as i32 + self.body + self.k() - 5
    }

    /// Left edge of the flat part of the plate.
    pub fn bar_x0(&self) -> i32 {
        LCAP_X + END_W as i32
    }

    /// Width of the whole bar from the left end's first column.
    pub fn width(&self) -> i32 {
        self.cap_x() + END_W as i32 - LCAP_X
    }

    /// Where the strict check's strips of 'pitch' columns start, 3 or more columns left of the first
    /// cell so that the last strip stops before the right end.
    pub fn strip_x0(&self) -> i32 {
        let need = self.pitch - (self.lean() as i32 + self.body + self.k() - 5);
        self.cell_x0() - need.max(3)
    }
}

/// The sizes of the bar, full size first.
pub fn geos() -> [Geo; SIZES] {
    use Row::*;
    let rows = |head: &[Row], cells: usize, tail: &[Row]| -> Vec<Row> {
        let mut v = head.to_vec();
        v.push(WellTop);
        v.extend(std::iter::repeat_n(Recess, cells));
        v.push(WellLow);
        v.extend_from_slice(tail);
        v
    };
    [
        Geo {
            pitch: 12,
            body: 10,
            cell_h: 8,
            rows: rows(&[Rim, Out, Hi, Mid], 8, &[Lip, Shade, Foot, Drop]),
            depth: 0,
        },
        Geo {
            pitch: 11,
            body: 9,
            cell_h: 7,
            rows: rows(&[Rim, Out, Hi, Mid], 7, &[Lip, Shade, Foot, Drop]),
            depth: 1,
        },
        Geo {
            pitch: 10,
            body: 8,
            cell_h: 6,
            rows: rows(&[Rim, Out, Hi], 6, &[Lip, Shade, Foot, Drop]),
            depth: 2,
        },
        Geo {
            pitch: 9,
            body: 7,
            cell_h: 5,
            rows: rows(&[Rim, Out], 5, &[Lip, Foot, Drop]),
            depth: 3,
        },
    ]
}

/// The tone indices of the rows of a cell, top to bottom, for each cell height.
fn cell_tones(h: usize) -> &'static [u8] {
    match h {
        8 => &[
            CELL_HI, CELL_L1, CELL_L2, CELL_L2, CELL_M, CELL_M, CELL_D, CELL_DD,
        ],
        7 => &[CELL_HI, CELL_L1, CELL_L2, CELL_M, CELL_M, CELL_D, CELL_DD],
        6 => &[CELL_HI, CELL_L1, CELL_L2, CELL_M, CELL_D, CELL_DD],
        _ => &[CELL_HI, CELL_L2, CELL_M, CELL_D, CELL_DD],
    }
}

/// The health cell: a parallelogram `body` pixels wide, `cell_h` rows tall, leaning over its height
/// in two-row steps, with a bright top row, a lit left edge and a dark right edge.
pub fn cell(g: &Geo) -> Sprite {
    let body = g.body as usize;
    let mut s = Sprite::new(body + g.lean(), g.cell_h);
    let tones = cell_tones(g.cell_h);
    for (r, &tone) in tones.iter().enumerate().take(g.cell_h) {
        let o = g.lean() - r / 2;
        for c in o..o + body {
            let mut i = tone;
            if r > 0 && r + 1 < g.cell_h {
                if c == o {
                    i = CELL_LEFT;
                } else if c == o + body - 1 {
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

/// The sloped right end of the bar, as tall as the plate, its end edge leaning one pixel per two
/// rows like the cells, with the dark rim following it on the outside.
pub fn cap(g: &Geo) -> Sprite {
    let mut s = Sprite::new(END_W, g.rows.len());
    let end = |r: usize| g.edge_col(r);
    for (r, &kind) in g.rows.iter().enumerate() {
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
                            if r + 3 >= g.rows.len() {
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
pub fn lcap(g: &Geo) -> Sprite {
    let mut s = Sprite::new(END_W, g.rows.len());
    let edge = |r: usize| g.edge_col(r);
    for (r, &kind) in g.rows.iter().enumerate() {
        match kind {
            Row::Rim => {
                for c in edge(1) - 1..END_W {
                    s.set(c, r, row_index(Row::Rim));
                }
            }
            Row::Drop => {
                for c in edge(r - 1) - 1..END_W {
                    s.set(c, r, row_index(Row::Drop));
                }
            }
            _ => {
                let e = edge(r);
                for c in e..END_W {
                    let is_edge = c == e;
                    let wall = c == e + 1;
                    let i = match kind {
                        Row::Out => row_index(Row::Out),
                        Row::Foot => row_index(Row::Foot),
                        _ if is_edge => {
                            if r + 3 >= g.rows.len() {
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

pub type Palette = [Rgb; 16];

fn pal(entries: &[(u8, Rgb)]) -> Palette {
    let mut p = [[0u8; 3]; 16];
    for &(i, c) in entries {
        p[i as usize] = c;
    }
    p
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

/// A stance colour as seen at a distance from the front: the bar behind is darker and greyer.
#[derive(Clone, Copy)]
pub struct Tone {
    pub base: Rgb,
}

impl Tone {
    /// The stance colour `depth` steps back out of `SIZES - 1`: 0 is the front bar's own colour, the
    /// last is the dim, grey colour of the farthest bar.
    pub fn at_depth(base: Rgb, depth: usize) -> Self {
        let far = scale(desaturate(base, 70), 120);
        Self {
            base: mix(base, far, (depth * 256 / (SIZES - 1)) as u32),
        }
    }
}

/// The bar's colours by row: `(first row, one past the last, colour, glass)`, equal neighbouring
/// rows merged.
pub fn bands(g: &Geo, t: Tone) -> Vec<(u8, u8, Rgb, bool)> {
    let mut out: Vec<(u8, u8, Row)> = Vec::new();
    for (r, &k) in g.rows.iter().enumerate() {
        match out.last_mut() {
            Some(last) if last.2 == k => last.1 = r as u8 + 1,
            _ => out.push((r as u8, r as u8 + 1, k)),
        }
    }
    out.into_iter()
        .map(|(a, b, k)| (a, b, row_colour(k, t.base), is_glass(k)))
        .collect()
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

/// Width of the atlas in texels: a multiple of 8 so that a row is a whole number of VRAM words.
pub const ATLAS_W: usize = 64;

/// A sprite and where it sits in the atlas.
pub struct Placed {
    pub name: String,
    pub sprite: Sprite,
    pub pos: (usize, usize),
}

/// All sprites by atlas name: the emblem and the gem, then the ends and the cell of every size.
fn named_sprites() -> Vec<(String, Sprite)> {
    let mut v = vec![("EMBLEM".to_string(), emblem()), ("GEM".to_string(), gem())];
    for (i, g) in geos().iter().enumerate() {
        v.push((format!("CAP_{i}"), cap(g)));
        v.push((format!("LCAP_{i}"), lcap(g)));
        v.push((format!("CELL_{i}"), cell(g)));
    }
    v
}

/// The sprites packed into shelves of the atlas, and the atlas height.
pub fn pack() -> (Vec<Placed>, usize) {
    let mut out = Vec::new();
    let (mut x, mut y, mut shelf) = (0usize, 0usize, 0usize);
    for (name, sprite) in named_sprites() {
        if x + sprite.w > ATLAS_W {
            x = 0;
            y += shelf;
            shelf = 0;
        }
        shelf = shelf.max(sprite.h);
        out.push(Placed {
            name,
            pos: (x, y),
            sprite: sprite.clone(),
        });
        x += sprite.w;
    }
    (out, y + shelf)
}

/// The 4-bit atlas as VRAM halfwords, row by row, four texels per halfword with the leftmost texel
/// in the lowest nibble.
pub fn atlas_halfwords() -> Vec<u16> {
    let (placed, height) = pack();
    let mut texels = vec![0u8; ATLAS_W * height];
    for p in &placed {
        for y in 0..p.sprite.h {
            for x in 0..p.sprite.w {
                texels[(p.pos.1 + y) * ATLAS_W + p.pos.0 + x] = p.sprite.get(x, y);
            }
        }
    }
    texels
        .chunks(4)
        .map(|q| q[0] as u16 | (q[1] as u16) << 4 | (q[2] as u16) << 8 | (q[3] as u16) << 12)
        .collect()
}
