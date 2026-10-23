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

/// The look of the HUD: the same layout and sprites drawn with different colours and details.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Style {
    /// Round 6: saturated colours, a dark outline, glossy cells.
    Classic,
    /// Worn gunmetal with a thin cold-light edge, dull ember colours, rivets.
    Gunmetal,
    /// Dark lacquer with one hot accent line and glowing cells.
    Lacquer,
    /// Etched bronze and iron: engraved double lines, patina colours, scratches.
    Bronze,
}

/// The committed style: lacquer, which Manny preferred in round 7.
pub const DEFAULT_STYLE: Style = Style::Lacquer;

/// The committed lacquer variant (0 to 6, see [`variant`]): the one picked in round 8.
pub const DEFAULT_VARIANT: u8 = 3;

/// The style to draw (`HUD_STYLE`: classic, gunmetal, lacquer, bronze, or l1 to l6 for the lacquer
/// variants of round 8), for trying looks without editing the code; the committed art is made with
/// the default.
pub fn style() -> Style {
    match std::env::var("HUD_STYLE").as_deref() {
        Ok("classic") => Style::Classic,
        Ok("gunmetal") => Style::Gunmetal,
        Ok("lacquer" | "l1" | "l2" | "l3" | "l4" | "l5" | "l6" | "l13a" | "l13b" | "l13c") => {
            Style::Lacquer
        }
        Ok("bronze") => Style::Bronze,
        _ => DEFAULT_STYLE,
    }
}

/// Which lacquer variant: 0 is the round 7 lacquer, 1 to 6 the round 8 ones (`HUD_STYLE=l1` to
/// `l6`). Only the lacquer style looks at it.
///
/// - 1 quiet: no accent line, a soft glint, a matte emblem centre, hollow outlines for empty gems
/// - 2 tinted: burgundy-black and blue-black plates, a deep accent in each stance's own colour,
///   gems in the stance colour
/// - 3 ramp: a white-hot edge line and a dithered two-step ramp in the lit cells
/// - 4 hybrid: lacquer with the gunmetal cold edge below, rivets, grime specks and bolts
/// - 5 cut: one pixel glint, small cut gems in the stance colour, an extra thin accent ring
/// - 6 cold: a gunmetal grey plate under lacquer cells, a cold edge, hex amber gems
///
/// Round 8b blends of 1 and 3 (`HUD_STYLE=l13a` to `l13c`):
/// - 7 (a) the white-hot edge and dithered ramp of 3 with the hollow empty gems and matte emblem of 1
/// - 8 (b) 1 with the dithered ramp of 3 in its lit cells
/// - 9 (c) 3 with its edge line dimmed to a soft tone, and the hollow empty gems of 1
pub fn variant() -> u8 {
    if style() != Style::Lacquer {
        return 0;
    }
    match std::env::var("HUD_STYLE").as_deref() {
        Ok("l1") => 1,
        Ok("l2") => 2,
        Ok("l3") => 3,
        Ok("l4") => 4,
        Ok("l5") => 5,
        Ok("l6") => 6,
        Ok("l13a") => 7,
        Ok("l13b") => 8,
        Ok("l13c") => 9,
        Ok(_) => 0,
        Err(_) => DEFAULT_VARIANT,
    }
}

/// Whether a colour is the red one of the two stances.
fn is_red(c: Rgb) -> bool {
    c[0] > c[2]
}

/// The cold light of gunmetal, for the hybrids.
const COLD: Rgb = [214, 230, 244];

/// The colour of the Horizon stance in the current style.
pub fn horizon() -> Rgb {
    match style() {
        Style::Classic => [234, 82, 48],
        Style::Gunmetal => [190, 88, 62],
        Style::Lacquer => [226, 66, 40],
        Style::Bronze => [178, 104, 58],
    }
}

/// The colour of the Zenith stance in the current style.
pub fn zenith() -> Rgb {
    match style() {
        Style::Classic => [76, 202, 181],
        Style::Gunmetal => [84, 160, 152],
        Style::Lacquer => [58, 196, 180],
        Style::Bronze => [96, 152, 126],
    }
}

/// The metal of the plate in the current style, before the stance tints it.
fn neutral(c: Rgb) -> Rgb {
    match style() {
        Style::Classic => [0, 0, 0],
        Style::Gunmetal => [100, 110, 122],
        Style::Lacquer => match variant() {
            2 if is_red(c) => [30, 12, 18],
            2 => [12, 18, 34],
            6 => [48, 54, 64],
            _ => [22, 24, 32],
        },
        Style::Bronze => [120, 92, 60],
    }
}

/// How much of the stance colour the plate takes (of 256).
fn tint() -> u32 {
    match style() {
        Style::Classic => 256,
        Style::Gunmetal => 40,
        Style::Lacquer => match variant() {
            2 => 30,
            6 => 28,
            _ => 24,
        },
        Style::Bronze => 70,
    }
}

/// The light that catches edges, in the current style.
fn light() -> Rgb {
    match style() {
        Style::Classic => WHITE,
        Style::Gunmetal => [214, 230, 244],
        Style::Lacquer => [255, 236, 224],
        Style::Bronze => [244, 212, 156],
    }
}

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
/// `back_drop()`, so the more it is, the more the front bar covers of it.
pub const DEFAULT_DROP: i32 = 3;

/// A number from the environment, for trying variants without editing the code; the committed art
/// is made with the defaults.
fn env_num(name: &str, default: i32) -> i32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// The committed bar height variant: cells of 6 rows at full size, 14 rows with the plate around them.
pub const DEFAULT_HEIGHT: i32 = 2;

/// Rows the bar behind is lower (`HUD_DROP`).
pub fn back_drop() -> i32 {
    env_num("HUD_DROP", DEFAULT_DROP)
}

/// How much thinner the bars are: 0 is the original height, 1 to 3 trim the cells and the rows
/// around them (`HUD_HEIGHT`).
pub fn height_variant() -> usize {
    env_num("HUD_HEIGHT", DEFAULT_HEIGHT).clamp(0, 3) as usize
}
/// Top row of the front bar, in HUD coordinates (the bar behind starts at row 0).
pub const FRONT_Y: i32 = LIFT;
/// Emblem top-left: centred on the front bar.
pub fn emblem_pos() -> (i32, i32) {
    (1, FRONT_Y + geos()[0].bar_h() / 2 - 12)
}
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
    // Per height variant and size: the rows above the well, the cells, the rows below. Each step
    // takes a row off the cells and trims the plate around them to match.
    type Plate = (&'static [Row], usize, &'static [Row]);
    let table: [[Plate; SIZES]; 4] = [
        [
            (&[Rim, Out, Hi, Mid], 8, &[Lip, Shade, Foot, Drop]),
            (&[Rim, Out, Hi, Mid], 7, &[Lip, Shade, Foot, Drop]),
            (&[Rim, Out, Hi], 6, &[Lip, Shade, Foot, Drop]),
            (&[Rim, Out], 5, &[Lip, Foot, Drop]),
        ],
        [
            (&[Rim, Out, Hi, Mid], 7, &[Lip, Foot, Drop]),
            (&[Rim, Out, Hi], 6, &[Lip, Foot, Drop]),
            (&[Rim, Out, Hi], 5, &[Lip, Foot, Drop]),
            (&[Rim, Out], 4, &[Lip, Foot, Drop]),
        ],
        [
            (&[Rim, Out, Hi], 6, &[Lip, Foot, Drop]),
            (&[Rim, Out, Hi], 5, &[Lip, Foot, Drop]),
            (&[Rim, Out], 4, &[Lip, Foot, Drop]),
            (&[Rim, Out], 4, &[Foot, Drop]),
        ],
        [
            (&[Rim, Out], 5, &[Lip, Foot, Drop]),
            (&[Rim, Out], 4, &[Lip, Foot, Drop]),
            (&[Rim, Out], 4, &[Foot, Drop]),
            (&[Rim, Out], 3, &[Foot, Drop]),
        ],
    ];
    const PITCH: [(i32, i32); SIZES] = [(12, 10), (11, 9), (10, 8), (9, 7)];
    let t = &table[height_variant()];
    std::array::from_fn(|i| Geo {
        pitch: PITCH[i].0,
        body: PITCH[i].1,
        cell_h: t[i].1,
        rows: rows(t[i].0, t[i].1, t[i].2),
        depth: i,
    })
}

/// The tone indices of the rows of a cell, top to bottom, for each cell height.
fn cell_tones(h: usize) -> &'static [u8] {
    match h {
        3 => &[CELL_HI, CELL_M, CELL_DD],
        4 => &[CELL_HI, CELL_L2, CELL_D, CELL_DD],
        8 => &[
            CELL_HI, CELL_L1, CELL_L2, CELL_L2, CELL_M, CELL_M, CELL_D, CELL_DD,
        ],
        7 => &[CELL_HI, CELL_L1, CELL_L2, CELL_M, CELL_M, CELL_D, CELL_DD],
        6 => &[CELL_HI, CELL_L1, CELL_L2, CELL_M, CELL_D, CELL_DD],
        _ => &[CELL_HI, CELL_L2, CELL_M, CELL_D, CELL_DD],
    }
}

/// A palette index no style's palette gives a plain tone: the glint or scratch of a cell.
const CELL_MARK: u8 = 9;
/// A second mark: a speck of grime, where a style has both a glint and grime.
const CELL_SPECK: u8 = 10;

/// Whether the tones of a cell step through a checkerboard dither: gunmetal, and the lacquer ramp.
fn dithers() -> bool {
    style() == Style::Gunmetal || (style() == Style::Lacquer && matches!(variant(), 3 | 7 | 8 | 9))
}

/// The health cell: a parallelogram `body` pixels wide, `cell_h` rows tall, leaning over its height
/// in two-row steps, with a bright top row, a lit left edge and a dark right edge, and the style's
/// finish: dithered steps between tones, a glint, grime or scratches.
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
            // Every other pixel of a row that starts a new tone takes the tone above, so that
            // the step between two tones is a checkerboard.
            if dithers()
                && r >= 3
                && r % 2 == 1
                && i == tone
                && (c + r) % 2 == 0
                && tones[r - 1] != tone
            {
                i = tones[r - 1];
            }
            s.set(c, r, i);
        }
    }
    let mark = |s: &mut Sprite, r: usize, dx: usize, index: u8| {
        if r + 1 < g.cell_h {
            let c = g.lean() - r / 2 + dx;
            if dx + 1 < body {
                s.set(c, r, index);
            }
        }
    };
    let glint = |s: &mut Sprite, pixels: usize| {
        for dx in 2..2 + pixels {
            mark(s, 1, dx, CELL_MARK);
        }
    };
    let specks = |s: &mut Sprite, index: u8| {
        mark(s, 2, body * 2 / 3, index);
        mark(s, 4, 3, index);
    };
    match (style(), variant()) {
        (Style::Gunmetal, _) => specks(&mut s, CELL_MARK),
        (Style::Lacquer, 3 | 7 | 9) => {}
        // A single pixel glint.
        (Style::Lacquer, 5) => glint(&mut s, 1),
        // The hybrids add the grime of gunmetal to the glint.
        (Style::Lacquer, 4 | 6) => {
            glint(&mut s, 2);
            specks(&mut s, CELL_SPECK);
        }
        // Lacquer: a two pixel glint high on the left.
        (Style::Lacquer, _) => glint(&mut s, 2),
        // Bronze: two scratches.
        (Style::Bronze, _) => {
            mark(&mut s, 2, body * 3 / 5, CELL_MARK);
            mark(&mut s, 3, body * 3 / 5 + 1, CELL_MARK);
            mark(&mut s, 4, 2, CELL_MARK);
        }
        _ => {}
    }
    s
}

/// Whether the end pieces carry rivets: gunmetal, bronze and the lacquer hybrids.
fn has_rivets() -> bool {
    matches!(style(), Style::Gunmetal | Style::Bronze)
        || (style() == Style::Lacquer && matches!(variant(), 4 | 6))
}

/// Rivet colours in the cap palette: light above, dark below.
pub const CAP_RIVET_LIGHT: u8 = 11;
pub const CAP_RIVET_DARK: u8 = 12;

/// Gunmetal and bronze: a rivet in the dark glass of an end piece, 2 x 2 pixels, light above dark.
/// `cols` are the columns to use; a pixel is only set where the end piece is glass there.
fn rivet(s: &mut Sprite, g: &Geo, cols: [usize; 2]) {
    if !has_rivets() {
        return;
    }
    let r0 = g.cell_row() + g.cell_h / 2 - 1;
    for (dy, index) in [(0, CAP_RIVET_LIGHT), (1, CAP_RIVET_DARK)] {
        for x in cols {
            if s.get(x, r0 + dy) == row_index(Row::Recess) {
                s.set(x, r0 + dy, index);
            }
        }
    }
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
    rivet(&mut s, g, [1, 2]);
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
    rivet(&mut s, g, [END_W - 2, END_W - 1]);
    s
}

/// Emblem indices.
const EM_OUT: u8 = 1;
const EM_BODY: [u8; 5] = [2, 3, 4, 5, 6];
const EM_RING_HI: u8 = 7;
const EM_RING_LO: u8 = 8;
const EM_CORE_OUT: u8 = 9;
const EM_CORE: [u8; 4] = [10, 11, 12, 13];
/// A bolt head (gunmetal) or the sheen across the top (lacquer).
const EM_MARK: u8 = 14;

/// The emblem: an octagon (7-pixel chamfers, all edges at 0 or 45 degrees, even size so that it
/// mirrors exactly) with a diamond core, 24 x 24. Gunmetal adds four bolts, lacquer a sheen across
/// the top and bronze a second engraved ring.
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
            let groove = (style() == Style::Bronze
                || (style() == Style::Lacquer && variant() == 5))
                && (x == inset + 3 || x == 20 - inset || y == 3 || y == 20)
                && x > inset + 1
                && x < 22 - inset
                && y > 1
                && y < 22;
            let i = if edge {
                EM_OUT
            } else if ring {
                if y < 12 {
                    EM_RING_HI
                } else {
                    EM_RING_LO
                }
            } else if groove {
                // Bronze engraves a dark groove; lacquer lays a thin accent ring.
                if style() == Style::Bronze {
                    EM_RING_LO
                } else {
                    EM_RING_HI
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
    let is_body = |s: &Sprite, x: usize, y: usize| EM_BODY.contains(&s.get(x, y));
    let bolts =
        style() == Style::Gunmetal || (style() == Style::Lacquer && matches!(variant(), 4 | 6));
    let sheen = style() == Style::Lacquer && !matches!(variant(), 1 | 6 | 7 | 8);
    if bolts {
        for (bx, by) in [(5usize, 5usize), (17, 5), (5, 17), (17, 17)] {
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                if is_body(&s, bx + dx, by + dy) {
                    s.set(bx + dx, by + dy, EM_MARK);
                }
            }
        }
    }
    if sheen {
        for (y, x0, x1) in [(2usize, 7usize, 16usize), (3, 9, 14)] {
            for x in x0..=x1 {
                if is_body(&s, x, y) {
                    s.set(x, y, EM_MARK);
                }
            }
        }
    }
    s
}

// Gem indices.
const GEM_OUT: u8 = 1;
const GEM_FACET: [u8; 6] = [2, 3, 4, 5, 6, 7];
const GEM_SPEC: u8 = 8;

/// The energy gem, 10 x 8: outlined, a bright facet above and a dark one below, and a glint. A
/// hexagon in classic and gunmetal, a lozenge in lacquer and a stud with a rivet in bronze.
pub fn gem() -> Sprite {
    let mut s = Sprite::new(10, 8);
    const HEX: [usize; 8] = [4, 6, 8, 10, 10, 8, 6, 4];
    const LOZENGE: [usize; 8] = [2, 4, 6, 8, 8, 6, 4, 2];
    const STUD: [usize; 8] = [6, 8, 10, 10, 10, 10, 8, 6];
    // A small cut stone: six rows with cut corners.
    const CUT: [usize; 8] = [0, 4, 6, 6, 6, 6, 4, 0];
    let width = match (style(), variant()) {
        (Style::Lacquer, 5) => CUT,
        (Style::Lacquer, 6) => HEX,
        (Style::Lacquer, _) => LOZENGE,
        (Style::Bronze, _) => STUD,
        _ => HEX,
    };
    for (y, &w) in width.iter().enumerate() {
        let x0 = (10 - w) / 2;
        for x in x0..x0 + w {
            let edge = x == x0 || x == x0 + w - 1 || y == 0 || y == 7;
            let i = if edge { GEM_OUT } else { GEM_FACET[y - 1] };
            s.set(x, y, i);
        }
    }
    if style() == Style::Bronze {
        for (x, y) in [(4, 3), (5, 3), (4, 4), (5, 4)] {
            s.set(x, y, GEM_SPEC);
        }
    } else {
        s.set(4, 1, GEM_SPEC);
        s.set(5, 1, GEM_SPEC);
    }
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
fn row_colour(k: Row, t: Tone) -> Rgb {
    let c = t.base;
    let glass = |k: Row| match k {
        Row::WellTop => scale(c, 7),
        Row::Recess => scale(c, 15),
        _ => scale(c, 30),
    };
    if style() == Style::Classic {
        return match k {
            Row::Out => mix(c, WHITE, 90),
            Row::Hi => scale(c, 250),
            Row::Mid => scale(c, 205),
            Row::WellTop | Row::Recess | Row::WellLow => glass(k),
            Row::Lip => scale(c, 170),
            Row::Shade => scale(c, 120),
            Row::Foot => scale(c, 66),
            // Never pure black: a black CLUT entry would read as transparent or as glass.
            Row::Rim | Row::Drop => scale(c, 20).map(|v| v.max(8)),
        };
    }
    let plate = scale(mix(neutral(c), c, tint()), t.dim());
    let l = light();
    match (style(), k) {
        (_, Row::WellTop | Row::Recess | Row::WellLow) => glass(k),
        // The outline and the drop shadow are translucent darkness, not an opaque black line.
        (Style::Gunmetal, Row::Rim | Row::Drop) => [4, 6, 12],
        (Style::Lacquer, Row::Rim | Row::Drop) => [0, 0, 8],
        (_, Row::Rim | Row::Drop) => [10, 6, 2],
        (Style::Gunmetal, Row::Out) => mix(plate, l, 140),
        (Style::Gunmetal, Row::Hi) => scale(plate, 285),
        (Style::Gunmetal, Row::Mid) => plate,
        (Style::Gunmetal, Row::Lip) => scale(plate, 215),
        (Style::Gunmetal, Row::Shade) => scale(plate, 150),
        (Style::Gunmetal, Row::Foot) => scale(plate, 96),
        // One hot line along the top, a sheen under it, the rest almost black.
        (Style::Lacquer, Row::Out) => match variant() {
            // No accent line: just the sheen of the lacquer.
            1 | 8 => scale(plate, 560),
            // The stance's own colour, deep (rust for Horizon, teal for Zenith).
            2 => scale(mix(desaturate(c, 60), l, 16), t.dim()),
            // A thin white-hot line.
            3 | 7 => scale(mix([255, 244, 228], c, 30), t.dim()),
            // Dimmed halfway to the plain sheen.
            9 => mix(
                scale(plate, 560),
                scale(mix([255, 244, 228], c, 30), t.dim()),
                118,
            ),
            // The cold edge of gunmetal.
            6 => mix(plate, COLD, 150),
            _ => scale(mix(c, l, 60), t.dim()),
        },
        (Style::Lacquer, Row::Hi) => scale(plate, if variant() == 6 { 300 } else { 520 }),
        (Style::Lacquer, Row::Mid) => scale(plate, if variant() == 6 { 230 } else { 330 }),
        // The hybrids take the cold edge below the cells as well.
        (Style::Lacquer, Row::Lip) if matches!(variant(), 4 | 6) => mix(plate, COLD, 100),
        (Style::Lacquer, Row::Lip) => scale(plate, if variant() == 6 { 270 } else { 400 }),
        (Style::Lacquer, Row::Shade) => scale(plate, if variant() == 6 { 190 } else { 220 }),
        (Style::Lacquer, Row::Foot) => scale(plate, if variant() == 6 { 130 } else { 140 }),
        // Engraved: a light edge, then a dark groove, as a double line cut in the metal.
        (_, Row::Out) => mix(plate, l, 118),
        (_, Row::Hi) => scale(plate, 120),
        (_, Row::Mid) => plate,
        (_, Row::Lip) => mix(plate, l, 56),
        (_, Row::Shade) => scale(plate, 110),
        (_, Row::Foot) => scale(plate, 66),
    }
}

/// Whether a row is translucent: the glass the cells sit in, and in the new styles the outline and
/// the shadow, which darken the scene instead of drawing a black line over it.
fn is_glass(k: Row) -> bool {
    match k {
        Row::WellTop | Row::Recess | Row::WellLow => true,
        Row::Rim | Row::Drop => style() != Style::Classic,
        _ => false,
    }
}

/// A stance colour as seen at a distance from the front: the bar behind is darker and greyer.
#[derive(Clone, Copy)]
pub struct Tone {
    pub base: Rgb,
    /// How far back: 0 is the front bar.
    pub depth: usize,
}

impl Tone {
    /// The stance colour `depth` steps back out of `SIZES - 1`: 0 is the front bar's own colour, the
    /// last is the dim, grey colour of the farthest bar.
    pub fn at_depth(base: Rgb, depth: usize) -> Self {
        let far = scale(desaturate(base, 70), 120);
        Self {
            base: mix(base, far, (depth * 256 / (SIZES - 1)) as u32),
            depth,
        }
    }

    /// How much of its brightness the plate keeps (of 256).
    fn dim(&self) -> u32 {
        256 - 44 * self.depth as u32
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
        .map(|(a, b, k)| (a, b, row_colour(k, t), is_glass(k)))
        .collect()
}

/// The cap palette indices that are glass: the well and, in the new styles, the outline.
pub fn cap_glass() -> Vec<u8> {
    let mut v = vec![8, 4, 9];
    if style() != Style::Classic {
        v.push(10);
    }
    v
}

/// The cap palette, with the rivet colours (11 light, 12 dark) after the rows'.
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
        p[row_index(k) as usize] = row_colour(k, t);
    }
    if has_rivets() {
        p[CAP_RIVET_LIGHT as usize] = if style() == Style::Lacquer {
            scale([176, 190, 206], t.dim())
        } else {
            row_colour(Row::Out, t)
        };
        p[CAP_RIVET_DARK as usize] = row_colour(Row::Foot, t);
    }
    p
}

pub fn cell_palette(t: Tone, lit: bool) -> Palette {
    let c = t.base;
    let l = light();
    match (style(), lit) {
        (Style::Classic, true) => pal(&[
            (CELL_HI, mix(c, WHITE, 140)),
            (CELL_L1, mix(c, WHITE, 64)),
            (CELL_L2, c),
            (CELL_M, scale(c, 218)),
            (CELL_D, scale(c, 174)),
            (CELL_DD, scale(c, 128)),
            (CELL_LEFT, mix(c, WHITE, 90)),
            (CELL_RIGHT, scale(c, 140)),
        ]),
        (Style::Classic, false) => pal(&[
            (CELL_HI, scale(c, 77)),
            (CELL_L1, scale(c, 46)),
            (CELL_L2, scale(c, 36)),
            (CELL_M, scale(c, 31)),
            (CELL_D, scale(c, 26)),
            (CELL_DD, scale(c, 20)),
            (CELL_LEFT, scale(c, 66)),
            (CELL_RIGHT, scale(c, 18)),
        ]),
        // Dull ember behind a cold edge, tones stepping through a dither.
        (Style::Gunmetal, true) => pal(&[
            (CELL_HI, mix(c, l, 150)),
            (CELL_L1, mix(c, l, 60)),
            (CELL_L2, c),
            (CELL_M, scale(c, 204)),
            (CELL_D, scale(c, 150)),
            (CELL_DD, scale(c, 104)),
            (CELL_LEFT, mix(c, l, 90)),
            (CELL_RIGHT, scale(c, 100)),
            (CELL_MARK, scale(c, 120)),
        ]),
        (Style::Gunmetal, false) => pal(&[
            (CELL_HI, mix(scale(c, 40), [60, 68, 78], 140)),
            (CELL_L1, mix(scale(c, 30), [44, 50, 58], 140)),
            (CELL_L2, mix(scale(c, 26), [38, 44, 52], 140)),
            (CELL_M, mix(scale(c, 22), [34, 40, 46], 140)),
            (CELL_D, mix(scale(c, 18), [28, 32, 38], 140)),
            (CELL_DD, mix(scale(c, 14), [22, 26, 30], 140)),
            (CELL_LEFT, mix(scale(c, 50), [74, 82, 92], 140)),
            (CELL_RIGHT, [18, 21, 25]),
            (CELL_MARK, [12, 14, 18]),
        ]),
        // A hot cell: white at the top, the accent colour, then deep into the dark.
        (Style::Lacquer, true) => pal(&[
            (CELL_HI, mix(c, l, 200)),
            (CELL_L1, mix(c, l, 80)),
            (CELL_L2, c),
            (CELL_M, scale(c, 196)),
            (CELL_D, scale(c, 128)),
            (CELL_DD, scale(c, 70)),
            (CELL_LEFT, mix(c, l, 120)),
            (CELL_RIGHT, scale(c, 84)),
            // The glint is white, or a lighter tone of the cell where the variant is quiet.
            (
                CELL_MARK,
                if matches!(variant(), 1 | 6 | 8) {
                    mix(c, l, 120)
                } else {
                    [255, 250, 240]
                },
            ),
            (CELL_SPECK, scale(c, 60)),
        ]),
        (Style::Lacquer, false) => pal(&[
            (CELL_HI, [52, 56, 70]),
            (CELL_L1, [34, 38, 48]),
            (CELL_L2, [28, 30, 40]),
            (CELL_M, [24, 26, 34]),
            (CELL_D, [20, 22, 28]),
            (CELL_DD, [14, 16, 20]),
            (CELL_LEFT, [60, 66, 82]),
            (CELL_RIGHT, [12, 13, 17]),
            (CELL_MARK, [70, 76, 94]),
            (CELL_SPECK, [8, 9, 12]),
        ]),
        // Patinated metal inlaid in a groove, with scratches.
        (Style::Bronze, true) => pal(&[
            (CELL_HI, mix(c, l, 110)),
            (CELL_L1, mix(c, l, 40)),
            (CELL_L2, c),
            (CELL_M, scale(c, 214)),
            (CELL_D, scale(c, 178)),
            (CELL_DD, scale(c, 140)),
            (CELL_LEFT, scale(c, 120)),
            (CELL_RIGHT, scale(c, 86)),
            (CELL_MARK, mix(c, l, 170)),
        ]),
        (Style::Bronze, false) => pal(&[
            (CELL_HI, mix(scale(c, 40), [58, 46, 32], 150)),
            (CELL_L1, mix(scale(c, 30), [46, 36, 26], 150)),
            (CELL_L2, mix(scale(c, 26), [40, 32, 22], 150)),
            (CELL_M, mix(scale(c, 22), [36, 28, 20], 150)),
            (CELL_D, mix(scale(c, 18), [30, 24, 16], 150)),
            (CELL_DD, mix(scale(c, 14), [24, 18, 12], 150)),
            (CELL_LEFT, mix(scale(c, 60), [84, 66, 44], 150)),
            (CELL_RIGHT, [16, 12, 8]),
            (CELL_MARK, [68, 54, 36]),
        ]),
    }
}

/// The emblem of the stance `active`, its core in the colour of the other one.
pub fn emblem_palette(active: Rgb, other: Rgb) -> Palette {
    let (c, o) = (active, other);
    let l = light();
    if style() == Style::Classic {
        return pal(&[
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
        ]);
    }
    let plate = mix(neutral(c), c, tint());
    let (out, body, ring_hi, ring_lo, mark): (Rgb, [Rgb; 5], Rgb, Rgb, Rgb) = match style() {
        Style::Gunmetal => (
            mix(plate, l, 140),
            [
                mix(plate, l, 100),
                scale(plate, 285),
                scale(plate, 230),
                scale(plate, 180),
                scale(plate, 130),
            ],
            mix(c, l, 40),
            scale(c, 120),
            [196, 206, 216],
        ),
        Style::Lacquer => {
            let v = variant();
            let accent = if v == 2 {
                mix(desaturate(c, 60), l, 16)
            } else {
                c
            };
            let body = if v == 6 {
                [
                    mix(plate, COLD, 90),
                    scale(plate, 280),
                    scale(plate, 230),
                    scale(plate, 180),
                    scale(plate, 130),
                ]
            } else {
                [
                    scale(plate, 560),
                    scale(plate, 380),
                    scale(plate, 300),
                    scale(plate, 230),
                    scale(plate, 160),
                ]
            };
            let (out, ring_hi, ring_lo) = match v {
                // Matte: the accent is only a faint edge.
                1 | 7 | 8 => (scale(c, 110), scale(c, 150), scale(c, 90)),
                6 => (
                    mix(plate, COLD, 130),
                    mix(plate, COLD, 100),
                    scale(plate, 200),
                ),
                _ => (scale(accent, 170), mix(accent, l, 50), scale(accent, 130)),
            };
            let mark = match v {
                4 => [150, 160, 176],
                6 => [196, 206, 216],
                _ => [120, 128, 150],
            };
            (out, body, ring_hi, ring_lo, mark)
        }
        _ => (
            mix(plate, l, 150),
            [
                mix(plate, l, 90),
                scale(plate, 250),
                scale(plate, 210),
                scale(plate, 170),
                scale(plate, 130),
            ],
            mix(plate, l, 110),
            scale(plate, 62),
            [0, 0, 0],
        ),
    };
    pal(&[
        (EM_OUT, out),
        (EM_BODY[0], body[0]),
        (EM_BODY[1], body[1]),
        (EM_BODY[2], body[2]),
        (EM_BODY[3], body[3]),
        (EM_BODY[4], body[4]),
        (EM_RING_HI, ring_hi),
        (EM_RING_LO, ring_lo),
        (EM_CORE_OUT, scale(o, 80)),
        (
            EM_CORE[0],
            mix(
                o,
                l,
                if style() == Style::Lacquer && variant() == 1 {
                    30
                } else {
                    110
                },
            ),
        ),
        (EM_CORE[1], mix(o, l, 36)),
        (EM_CORE[2], scale(o, 200)),
        (EM_CORE[3], scale(o, 130)),
        (EM_MARK, mark),
    ])
}

pub fn gem_palette(lit: bool, stance: usize) -> Palette {
    let p = |out: Rgb, f: [Rgb; 6], spec: Rgb| {
        pal(&[
            (GEM_OUT, out),
            (GEM_FACET[0], f[0]),
            (GEM_FACET[1], f[1]),
            (GEM_FACET[2], f[2]),
            (GEM_FACET[3], f[3]),
            (GEM_FACET[4], f[4]),
            (GEM_FACET[5], f[5]),
            (GEM_SPEC, spec),
        ])
    };
    if style() == Style::Lacquer {
        let l = light();
        match (variant(), lit) {
            // Gems in the colour of the stance they follow.
            (2 | 5, true) => {
                let c = if stance == 0 { horizon() } else { zenith() };
                return p(
                    scale(c, 25).map(|v| v.max(8)),
                    [
                        mix(c, l, 200),
                        mix(c, l, 100),
                        c,
                        scale(c, 200),
                        scale(c, 150),
                        scale(c, 100),
                    ],
                    [255, 255, 248],
                );
            }
            // Empty gems are an outline only: the facets are transparent.
            (1 | 7 | 8 | 9, false) => return p([46, 48, 60], [[0, 0, 0]; 6], [0, 0, 0]),
            // The gunmetal gems.
            (6, true) => {
                return p(
                    [52, 36, 24],
                    [
                        [240, 214, 160],
                        [226, 180, 104],
                        [214, 156, 70],
                        [186, 118, 40],
                        [158, 92, 28],
                        [128, 70, 22],
                    ],
                    [252, 240, 208],
                )
            }
            (6, false) => {
                return p(
                    [30, 32, 38],
                    [
                        [58, 62, 70],
                        [52, 56, 64],
                        [46, 50, 58],
                        [40, 44, 50],
                        [34, 38, 44],
                        [30, 32, 38],
                    ],
                    [74, 80, 90],
                )
            }
            _ => {}
        }
    }
    match (style(), lit) {
        (Style::Classic, true) => p(
            [96, 44, 0],
            [
                [255, 242, 190],
                [255, 214, 100],
                [255, 200, 70],
                [232, 140, 16],
                [200, 100, 6],
                [168, 84, 0],
            ],
            [255, 255, 236],
        ),
        (Style::Classic, false) => p(
            [46, 28, 16],
            [
                [66, 42, 24],
                [58, 36, 20],
                [52, 32, 18],
                [44, 28, 16],
                [40, 24, 14],
                [34, 20, 12],
            ],
            [74, 48, 28],
        ),
        (Style::Gunmetal, true) => p(
            [52, 36, 24],
            [
                [240, 214, 160],
                [226, 180, 104],
                [214, 156, 70],
                [186, 118, 40],
                [158, 92, 28],
                [128, 70, 22],
            ],
            [252, 240, 208],
        ),
        (Style::Gunmetal, false) => p(
            [30, 32, 38],
            [
                [58, 62, 70],
                [52, 56, 64],
                [46, 50, 58],
                [40, 44, 50],
                [34, 38, 44],
                [30, 32, 38],
            ],
            [74, 80, 90],
        ),
        (Style::Lacquer, true) => p(
            [24, 6, 0],
            [
                [255, 244, 214],
                [255, 200, 110],
                [255, 150, 50],
                [236, 100, 24],
                [196, 64, 10],
                [150, 40, 4],
            ],
            [255, 255, 248],
        ),
        (Style::Lacquer, false) => p(
            [8, 8, 12],
            [
                [40, 42, 54],
                [32, 34, 44],
                [26, 28, 36],
                [22, 24, 30],
                [18, 20, 26],
                [14, 16, 20],
            ],
            [80, 86, 104],
        ),
        (Style::Bronze, true) => p(
            [46, 28, 10],
            [
                [236, 196, 120],
                [214, 156, 76],
                [196, 128, 52],
                [170, 102, 36],
                [140, 80, 28],
                [110, 62, 22],
            ],
            [255, 230, 170],
        ),
        (Style::Bronze, false) => p(
            [22, 16, 10],
            [
                [66, 52, 36],
                [58, 46, 32],
                [50, 40, 28],
                [44, 34, 24],
                [38, 30, 20],
                [32, 24, 16],
            ],
            [86, 70, 48],
        ),
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
