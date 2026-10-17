//! The prototype compact room codec ("PGC-A").
//!
//! A predict-and-correct stream over the cooked room tables. The collision
//! layer is coded first (sectors, walls); the render layer is then coded as
//! per-cell lists of items, each either a prediction the decoder can build
//! from what it already holds (a floor or ceiling of the sector, the next
//! course piece of a collision wall) or an explicit ring. Everything the
//! decoder can derive is not coded: vertex ids (welding), plane constants,
//! axis flags, repeat flags, cell tables, vertex ranges, the material list,
//! the extension and split-level tables, the three summary flag bits and the
//! two walkable bits.
//!
//! `code_room` is one traversal for encoder and decoder alike. Encoding runs
//! it with `src` set and returns the reconstruction, which must equal `src`.

use std::collections::HashMap;

use psx_grid::height::triangle_normal_q12;
use psx_grid::{
    ColWall, LodBlock, Material, Quad, RCell, Sector, SectorExt, SplitB, Vtx, NO_EXT, QUAD2_REPEAT,
    QUAD_AXIS, QUAD_AXIS_NEG, QUAD_AXIS_SHIFT, QUAD_DOUBLE, QUAD_TRI, SECTOR_CEIL_NESW,
    SECTOR_CLEAR, SECTOR_FLAT_CEIL, SECTOR_FLAT_FLOOR, SECTOR_FLOOR_NESW, SECTOR_FLOOR_WALK_A,
    SECTOR_FLOOR_WALK_B, SECTOR_HAS_CEIL, SECTOR_HAS_FLOOR, SECTOR_SPLIT_LEVEL,
};

use crate::bits::{eg, seg, tree, Alloc, Coder, EG_CTX, SEG_CTX};
use crate::probe::fine_to_ru;
use crate::tables::Room;

/// Items coded per symbol since the last reset (encoder runs only), for the
/// coverage report.
pub static ITEM_COUNT: [std::sync::atomic::AtomicUsize; 12] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 12];
/// Quads whose UV was explicit, whose normal was stored, and totals.
pub static UV_EXPLICIT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub static NORMAL_STORED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub const L_HDR: usize = 0;
pub const L_SECTORS: usize = 1;
pub const L_WALLS: usize = 2;
pub const L_ITEMS: usize = 3;
pub const L_MATERIAL: usize = 4;
pub const L_UV: usize = 5;
pub const L_COLOUR: usize = 6;
pub const L_ESC: usize = 7;
pub const L_NORMAL: usize = 8;
pub const LABELS: [&str; 9] = [
    "header",
    "sectors",
    "walls",
    "item symbols",
    "material",
    "uv",
    "colour",
    "explicit rings",
    "normals",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColourMode {
    /// Repeat / recent list / palette index / raw 24 bit colour.
    Palette,
    /// As `Palette`, but a new colour is a delta from the previous corner.
    PaletteDelta,
    /// As `PaletteDelta`, with the prediction taken from the nearest decoded
    /// corner of the same normal (lattice neighbours), not the previous corner.
    PaletteNear,
    /// Delta from the previous corner only.
    Delta,
}

#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub colour: ColourMode,
    /// Independent units of this many sectors a side (0: the whole room).
    /// Cells are then coded unit by unit with neighbours and adaptive state
    /// cut at the unit border. Size measurement only: the reconstruction is
    /// not built in the table order.
    pub unit: usize,
}

/// What the decoder knows before the chunk: the core's facts.
pub struct Env<'a> {
    pub s: i32,
    pub w: usize,
    pub d: usize,
    /// Room origin in whole sectors.
    pub origin: [i32; 2],
    pub materials: &'a [Material],
    /// Encoder-only facts, coded into the header.
    pub yoff_f: i32,
    pub repeat_on: bool,
    pub courses: bool,
    /// Height of a closure wall over a ceilingless floor, fine units (0 none).
    pub open_wall_f: i32,
}

type P3 = [i32; 3];

const DERIVED_FLAGS: u16 =
    SECTOR_FLAT_FLOOR | SECTOR_FLAT_CEIL | SECTOR_CLEAR | SECTOR_FLOOR_WALK_A | SECTOR_FLOOR_WALK_B;
const EDGE_CORNERS: [(usize, usize); 6] = [(0, 1), (1, 2), (2, 3), (3, 0), (0, 2), (1, 3)];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WallF {
    edge: u8,
    flags: u8,
    y: [i32; 4],
}

/// Item symbols.
const IT_END: u32 = 0;
const IT_FLOOR_Q: u32 = 1;
const IT_FLOOR_T0: u32 = 2;
const IT_FLOOR_T1: u32 = 3;
const IT_CEIL_Q: u32 = 4;
const IT_CEIL_T0: u32 = 5;
const IT_CEIL_T1: u32 = 6;
const IT_WALL: u32 = 7;
const IT_SKIP: u32 = 8;
const IT_ESC: u32 = 9;
const IT_DER: u32 = 10;
const IT_DSKIP: u32 = 11;

struct Ctx {
    flags_pal: usize,
    corner_mode: [usize; 2],
    corner_seg: [[usize; 4]; 2],
    flat_seg: [usize; 2],
    splitb_seg: usize,
    wall_count: [usize; 2],
    wall_edge: usize,
    wall_flags: usize,
    wall_mode: usize,
    wall_seg: [usize; 4],
    item: usize,
    variant: usize,
    open_sky: usize,
    material: usize,
    double: usize,
    uvmode: usize,
    uv_repeat: usize,
    nidx: usize,
    esc_n: usize,
    esc_first: [usize; 3],
    esc_next: [usize; 3],
    cmode: usize,
    cmtf: usize,
    cpal: usize,
    cdelta: [usize; 3],
}

impl Ctx {
    fn new() -> Self {
        let mut a = Alloc(64);
        Self {
            flags_pal: a.take(17 * 32),
            corner_mode: [a.take(3 * 4), a.take(3 * 4)],
            corner_seg: [
                [
                    a.take(SEG_CTX),
                    a.take(SEG_CTX),
                    a.take(SEG_CTX),
                    a.take(SEG_CTX),
                ],
                [
                    a.take(SEG_CTX),
                    a.take(SEG_CTX),
                    a.take(SEG_CTX),
                    a.take(SEG_CTX),
                ],
            ],
            flat_seg: [a.take(SEG_CTX), a.take(SEG_CTX)],
            splitb_seg: a.take(SEG_CTX),
            wall_count: [a.take(EG_CTX), a.take(EG_CTX)],
            wall_edge: a.take(7 * 8),
            wall_flags: a.take(4),
            wall_mode: a.take(5 * 4),
            wall_seg: [
                a.take(SEG_CTX),
                a.take(SEG_CTX),
                a.take(SEG_CTX),
                a.take(SEG_CTX),
            ],
            item: a.take(12 * 16),
            variant: a.take(3 * 9 * 8),
            open_sky: a.take(4),
            material: a.take(4 * EG_CTX),
            double: a.take(4),
            uvmode: a.take(3 * 4 * 4),
            uv_repeat: a.take(2),
            nidx: a.take(EG_CTX),
            esc_n: a.take(2),
            esc_first: [a.take(SEG_CTX), a.take(SEG_CTX), a.take(SEG_CTX)],
            esc_next: [a.take(SEG_CTX), a.take(SEG_CTX), a.take(SEG_CTX)],
            cmode: a.take(2 * 4),
            cmtf: a.take(16),
            cpal: a.take(EG_CTX),
            cdelta: [a.take(SEG_CTX), a.take(SEG_CTX), a.take(SEG_CTX)],
        }
    }
}

fn bits_for(n: usize) -> u32 {
    crate::fields::bits_for_count(n as u64)
}

/// Local runtime height of `f` fine steps in a room lifted by `yoff_f`.
fn y_of(f: i32, yoff_f: i32, s: i32) -> i32 {
    fine_to_ru(yoff_f + f, s) - fine_to_ru(yoff_f, s)
}

struct St<'a, C: Coder> {
    c: &'a mut C,
    env: &'a Env<'a>,
    par: Params,
    cx: Ctx,
    yoff_f: i32,
    inv: HashMap<i32, i32>,
}

impl<C: Coder> St<'_, C> {
    fn fine_of(&self, y: i16) -> Result<i32, String> {
        self.inv
            .get(&i32::from(y))
            .copied()
            .ok_or_else(|| format!("height {y} is not on the fine grid"))
    }

    fn ru_of(&self, f: i32) -> i32 {
        y_of(f, self.yoff_f, self.env.s)
    }

    fn pos_ru(&self, p: P3) -> [i32; 3] {
        let click = self.env.s / 4;
        [p[0] * click, self.ru_of(p[1]), p[2] * click]
    }
}

fn corner_xz(cx: i32, cz: i32, i: usize) -> (i32, i32) {
    let (dx, dz) = [(0, 0), (1, 0), (1, 1), (0, 1)][i];
    ((cx + dx) * 4, (cz + dz) * 4)
}

fn tri_corners(nesw: bool, t: usize) -> [usize; 3] {
    match (nesw, t) {
        (false, 0) => [0, 1, 2],
        (false, _) => [0, 2, 3],
        (true, 0) => [0, 1, 3],
        (true, _) => [1, 2, 3],
    }
}

fn variants(ring: &[P3]) -> Vec<Vec<P3>> {
    let n = ring.len();
    let mut out = Vec::new();
    for rev in [false, true] {
        let base: Vec<P3> = if rev {
            ring.iter().rev().copied().collect()
        } else {
            ring.to_vec()
        };
        for r in 0..n {
            let mut v = base.clone();
            v.rotate_left(r);
            out.push(v);
        }
    }
    out
}

fn variant_index(ring: &[P3], target: &[P3]) -> Option<usize> {
    if ring.len() != target.len() {
        return None;
    }
    let n = ring.len();
    for (i, v) in variants(ring).iter().enumerate() {
        if v == target {
            // Index in the 0..8 alphabet: reversed variants start at 4.
            return Some(if i < n { i } else { 4 + (i - n) });
        }
    }
    None
}

fn variant_ring(ring: &[P3], sym: usize) -> Vec<P3> {
    let n = ring.len();
    let (rev, r) = if sym >= 4 {
        (true, sym - 4)
    } else {
        (false, sym)
    };
    let mut v: Vec<P3> = if rev {
        ring.iter().rev().copied().collect()
    } else {
        ring.to_vec()
    };
    v.rotate_left(r % n);
    v
}

struct Piece {
    ring: Vec<P3>,
    /// New (wall index, abs start) after consuming it.
    next: (usize, Option<i32>),
}

fn wall_piece(
    cx: i32,
    cz: i32,
    walls: &[WallF],
    wi: usize,
    start: Option<i32>,
    yoff_f: i32,
    courses: bool,
) -> Option<Piece> {
    let w = walls.get(wi)?;
    let (a, b) = EDGE_CORNERS[usize::from(w.edge)];
    let (xa, za) = corner_xz(cx, cz, a);
    let (xb, zb) = corner_xz(cx, cz, b);
    let [ba, bb, tb, ta] = w.y;
    let rect = ba == bb && ta == tb && tb > ba;
    if rect && courses {
        let top_abs = yoff_f + ta;
        let s0 = start.unwrap_or(yoff_f + ba);
        if s0 >= top_abs {
            return None;
        }
        let next = ((s0.div_euclid(16) + 1) * 16).min(top_abs);
        let (lo, hi) = (s0 - yoff_f, next - yoff_f);
        let ring = vec![[xa, lo, za], [xb, lo, zb], [xb, hi, zb], [xa, hi, za]];
        let next_state = if next >= top_abs {
            (wi + 1, None)
        } else {
            (wi, Some(next))
        };
        Some(Piece {
            ring,
            next: next_state,
        })
    } else {
        let ring = vec![[xa, ba, za], [xb, bb, zb], [xb, tb, zb], [xa, ta, za]];
        Some(Piece {
            ring,
            next: (wi + 1, None),
        })
    }
}

/// Profile of a surface along cell edge `e` (corners `a`, `b`): the heights at
/// both ends, from the triangle that holds the edge.
fn edge_profile(h: [i32; 4], hb: Option<[i32; 4]>, nesw: bool, a: usize, b: usize) -> [i32; 2] {
    let tri = (0..2)
        .find(|t| {
            let c = tri_corners(nesw, *t);
            c.contains(&a) && c.contains(&b)
        })
        .unwrap_or(0);
    let hh = if tri == 1 { hb.unwrap_or(h) } else { h };
    [hh[a], hh[b]]
}

struct Sectors<'a> {
    w: usize,
    d: usize,
    floor: &'a [Option<[i32; 4]>],
    ceil: &'a [Option<[i32; 4]>],
    fb: &'a [Option<[i32; 4]>],
    cb: &'a [Option<[i32; 4]>],
    flags: &'a [u16],
}

/// Derived wall faces hosted by cell `(cx, cz)`, in the order the cook lists
/// them: per cardinal edge, riser, drop, closure. Only the plain quad cases;
/// a crossing of two profiles is not predicted.
fn derived_slots(sx: &Sectors<'_>, cx: i32, cz: i32, open_wall_f: i32) -> Vec<Vec<P3>> {
    let mut out = Vec::new();
    let idx = cz as usize * sx.w + cx as usize;
    let here_flags = sx.flags[idx];
    // Neighbour corner indices matching our a, b per edge.
    const NEIGH: [(i32, i32, usize, usize); 4] =
        [(0, -1, 3, 2), (1, 0, 0, 3), (0, 1, 1, 0), (-1, 0, 2, 1)];
    for e in 0..4usize {
        let (a, b) = EDGE_CORNERS[e];
        let (xa, za) = corner_xz(cx, cz, a);
        let (xb, zb) = corner_xz(cx, cz, b);
        let (dx, dz, na, nb) = NEIGH[e];
        let (nx, nz) = (cx + dx, cz + dz);
        let inside = nx >= 0 && nz >= 0 && (nx as usize) < sx.w && (nz as usize) < sx.d;
        let nidx = if inside {
            Some(nz as usize * sx.w + nx as usize)
        } else {
            None
        };
        let has_space = |f: u16| f & (SECTOR_HAS_FLOOR | SECTOR_HAS_CEIL) != 0;
        let h_floor = sx.floor[idx]
            .map(|f| edge_profile(f, sx.fb[idx], here_flags & SECTOR_FLOOR_NESW != 0, a, b));
        let h_ceil = sx.ceil[idx]
            .map(|c| edge_profile(c, sx.cb[idx], here_flags & SECTOR_CEIL_NESW != 0, a, b));
        let n_floor = nidx.and_then(|n| {
            sx.floor[n]
                .map(|f| edge_profile(f, sx.fb[n], sx.flags[n] & SECTOR_FLOOR_NESW != 0, na, nb))
        });
        let n_ceil = nidx.and_then(|n| {
            sx.ceil[n]
                .map(|c| edge_profile(c, sx.cb[n], sx.flags[n] & SECTOR_CEIL_NESW != 0, na, nb))
        });
        let quad = |lo: [i32; 2], hi: [i32; 2]| -> Vec<P3> {
            vec![
                [xa, lo[0], za],
                [xb, lo[1], zb],
                [xb, hi[1], zb],
                [xa, hi[0], za],
            ]
        };
        // R1: our floor below the neighbour's.
        if let (Some(m), Some(n)) = (h_floor, n_floor) {
            if n[0] >= m[0] && n[1] >= m[1] && (n[0] > m[0] || n[1] > m[1]) {
                out.push(quad(m, n));
            }
        }
        // R2: neighbour ceiling below ours.
        if let (Some(m), Some(n)) = (h_ceil, n_ceil) {
            if m[0] >= n[0] && m[1] >= n[1] && (m[0] > n[0] || m[1] > n[1]) {
                out.push(quad(n, m));
            }
        }
        // R3: closure against nothing.
        let far_space = nidx.is_some_and(|n| has_space(sx.flags[n]));
        if has_space(here_flags) && !far_space {
            if let Some(f) = h_floor {
                let top = match h_ceil {
                    Some(c) => Some(c),
                    None if open_wall_f > 0 => Some([f[0] + open_wall_f, f[1] + open_wall_f]),
                    None => None,
                };
                if let Some(t) = top {
                    if t[0] >= f[0] && t[1] >= f[1] && (t[0] > f[0] || t[1] > f[1]) {
                        out.push(quad(f, t));
                    }
                }
            }
        }
    }
    out
}

/// Integer texels per ring corner for a UV mode, `None` when the mode does
/// not apply to this ring.
fn texels(env: &Env<'_>, yoff_f: i32, ring: &[P3], mode: u32) -> Option<Vec<[i32; 2]>> {
    let [ox, oz] = env.origin;
    match mode {
        1 => Some(
            ring.iter()
                .map(|p| [ox * 32 + p[0] * 8, oz * 32 + p[2] * 8])
                .collect(),
        ),
        2 | 3 => {
            let su = if mode == 2 { 1 } else { -1 };
            let along_z = ring.iter().all(|p| p[0] == ring[0][0]);
            let along_x = ring.iter().all(|p| p[2] == ring[0][2]);
            if along_z == along_x {
                return None;
            }
            Some(
                ring.iter()
                    .map(|p| {
                        let u = if along_z {
                            oz * 32 + p[2] * 8
                        } else {
                            ox * 32 + p[0] * 8
                        };
                        [su * u, -2 * (yoff_f + p[1])]
                    })
                    .collect(),
            )
        }
        _ => None,
    }
}

/// UVs as the cook packs them, and whether the primitive repeats.
fn rebase(t: &[[i32; 2]], limit: i64) -> (Vec<[u8; 2]>, bool) {
    let mut out: Vec<[i64; 2]> = t
        .iter()
        .map(|v| [i64::from(v[0]), i64::from(v[1])])
        .collect();
    let mut repeat = false;
    for axis in 0..2 {
        let min = out.iter().map(|v| v[axis]).min().unwrap();
        let max = out.iter().map(|v| v[axis]).max().unwrap();
        let shift = min.div_euclid(64) * 64;
        let touched = ((max - shift + 63).div_euclid(64)).max(1);
        let tiles = touched.min(limit);
        if tiles > 1 {
            repeat = true;
        }
        let top = tiles * 64 - 1;
        for v in &mut out {
            v[axis] = (v[axis] - shift).clamp(0, top);
        }
    }
    (
        out.iter().map(|v| [v[0] as u8, v[1] as u8]).collect(),
        repeat,
    )
}

fn cross(a: [i32; 3], b: [i32; 3]) -> [i32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn ring_normal(ring: &[[i32; 3]]) -> [i32; 3] {
    for i in 1..ring.len() - 1 {
        let a = [
            ring[i][0] - ring[0][0],
            ring[i][1] - ring[0][1],
            ring[i][2] - ring[0][2],
        ];
        let b = [
            ring[i + 1][0] - ring[0][0],
            ring[i + 1][1] - ring[0][1],
            ring[i + 1][2] - ring[0][2],
        ];
        let n = cross(a, b);
        if n != [0; 3] {
            return n;
        }
    }
    [0; 3]
}

/// Chunk tables the traversal cannot code: planes, liquids, movers, LOD
/// quads. They are carried verbatim (an honest cost) or must be empty.
fn carries_unsupported(src: &Room) -> bool {
    !(src.col_planes.is_empty()
        && src.liquids.is_empty()
        && src.movers.is_empty()
        && src.mover_verts.is_empty()
        && src.plane_refs.is_empty()
        && src.lod_quads.is_empty())
}

pub fn code_room<C: Coder>(
    c: &mut C,
    env: &Env<'_>,
    par: Params,
    src: Option<&Room>,
) -> Result<Room, String> {
    let enc = src.is_some();
    if let Some(s) = src {
        if carries_unsupported(s) {
            return Err("room carries planes, liquids, movers or LOD quads".into());
        }
    }
    let s = env.s;
    let (w, d) = (env.w, env.d);
    // ----- header ------------------------------------------------------
    c.label(L_HDR);
    let mut a = Alloc(0);
    let hb = a.take(64);
    let yoff_f = seg(c, hb, env.yoff_f);
    let repeat_on = c.bit(hb + 20, env.repeat_on);
    let courses = c.bit(hb + 21, env.courses);
    let open_wall_f = seg(c, hb + 62 - 20, env.open_wall_f);
    let lod_far = c.raw(8, src.map_or(0, |r| u32::from(r.lod_far_cells)));
    let lod_shift = c.raw(8, src.map_or(0, |r| u32::from(r.lod_shift)));
    let min_y = seg(c, hb + 22, src.map_or(0, |r| i32::from(r.min_y)));
    let max_y = seg(c, hb + 22, src.map_or(0, |r| i32::from(r.max_y)));
    let mut inv = HashMap::new();
    for f in -3000..=3000 {
        inv.entry(y_of(f, yoff_f, s)).or_insert(f);
    }
    // Sector flag palette (flags without the derived bits).
    let src_flags: Vec<u16> = src
        .map(|r| r.sectors.iter().map(|x| x.flags & !DERIVED_FLAGS).collect())
        .unwrap_or_default();
    let mut palette: Vec<u16> = Vec::new();
    for f in &src_flags {
        if !palette.contains(f) {
            palette.push(*f);
        }
    }
    let pal_n = eg(c, hb + 42, palette.len() as u32) as usize;
    if pal_n > 32 {
        return Err("more than 32 distinct sector flag words".into());
    }
    let mut pal = Vec::new();
    for i in 0..pal_n {
        pal.push(c.raw(16, palette.get(i).copied().map_or(0, u32::from)) as u16);
    }
    let pal_bits = bits_for(pal_n);

    let st = St {
        c,
        env,
        par,
        cx: Ctx::new(),
        yoff_f,
        inv,
    };
    let src_f = |st: &St<'_, C>, y: i16| -> Result<i32, String> { st.fine_of(y) };

    // ----- sector layer --------------------------------------------------
    st.c.label(L_SECTORS);
    let cells = w * d;
    let unit = st.par.unit;
    let uw = if unit == 0 { 1 } else { w.div_ceil(unit) };
    let unit_of = move |x: usize, z: usize| -> usize {
        z.checked_div(unit).unwrap_or(0) * uw + x.checked_div(unit).unwrap_or(0)
    };
    let order: Vec<(usize, usize)> = {
        let mut v: Vec<(usize, usize)> = (0..d).flat_map(|z| (0..w).map(move |x| (x, z))).collect();
        if unit > 0 {
            v.sort_by_key(|&(x, z)| (unit_of(x, z), z, x));
        }
        v
    };
    let mut out = Room {
        width: w,
        depth: d,
        s,
        min_y: min_y as i16,
        max_y: max_y as i16,
        lod_far_cells: lod_far as u8,
        lod_shift: lod_shift as u8,
        ..Room::default()
    };
    let mut floor_f: Vec<Option<[i32; 4]>> = vec![None; cells];
    let mut ceil_f: Vec<Option<[i32; 4]>> = vec![None; cells];
    let mut fb_f: Vec<Option<[i32; 4]>> = vec![None; cells];
    let mut cb_f: Vec<Option<[i32; 4]>> = vec![None; cells];
    let mut walls_f: Vec<Vec<WallF>> = vec![Vec::new(); cells];
    let mut flag_words: Vec<u16> = vec![0; cells];
    let mut prev_pal = 0usize;
    let mut last_nw = [0i32; 2];
    let mut head = 0i32;
    let mut prev_mode = [0usize; 2];
    let mut prev_wall_mode = 0usize;
    let mut prev_edge = 6usize;
    let mut prev_count_nz = 0usize;
    let mut last_height = 16i32;
    let mut cur_unit = usize::MAX;
    for &(cx, cz) in &order {
        {
            let idx = cz * w + cx;
            if unit_of(cx, cz) != cur_unit {
                cur_unit = unit_of(cx, cz);
                if unit > 0 {
                    st.c.reset();
                    prev_pal = 0;
                    last_nw = [0; 2];
                    head = 0;
                    prev_mode = [0; 2];
                    prev_wall_mode = 0;
                    prev_edge = 6;
                    prev_count_nz = 0;
                    last_height = 16;
                }
            }
            let sec_src = src.map(|r| r.sectors[idx]);
            // Flag word by palette index.
            let want = sec_src.map_or(0usize, |x| {
                pal.iter()
                    .position(|p| *p == x.flags & !DERIVED_FLAGS)
                    .unwrap()
            });
            let base = st.cx.flags_pal + prev_pal * 32;
            let pi = tree(st.c, base, pal_bits, want as u32) as usize;
            prev_pal = pi.min(16);
            let fl = pal[pi];
            flag_words[idx] = fl;
            let has_floor = fl & SECTOR_HAS_FLOOR != 0;
            let has_ceil = fl & SECTOR_HAS_CEIL != 0;
            let split = fl & SECTOR_SPLIT_LEVEL != 0;
            // Floor and ceiling corners.
            for layer in 0..2 {
                let present = if layer == 0 { has_floor } else { has_ceil };
                if !present {
                    continue;
                }
                let grid = if layer == 0 { &floor_f } else { &ceil_f };
                let at = |x: i32, z: i32| -> Option<[i32; 4]> {
                    if x < 0
                        || z < 0
                        || x >= w as i32
                        || z >= d as i32
                        || unit_of(x as usize, z as usize) != unit_of(cx, cz)
                    {
                        None
                    } else {
                        grid[z as usize * w + x as usize]
                    }
                };
                let l = at(cx as i32 - 1, cz as i32);
                let u = at(cx as i32, cz as i32 - 1);
                let ul = at(cx as i32 - 1, cz as i32 - 1);
                let ur = at(cx as i32 + 1, cz as i32 - 1);
                let nw = l.map(|v| v[1]).or(u.map(|v| v[3])).or(ul.map(|v| v[2]));
                let known = nw.is_some();
                let pnw = nw.unwrap_or_else(|| {
                    if layer == 1 {
                        floor_f[idx].map_or(last_nw[1], |f| f[0] + head)
                    } else {
                        last_nw[0]
                    }
                });
                let pne = u.map(|v| v[2]).or(ur.map(|v| v[3])).unwrap_or(pnw);
                let psw = l.map(|v| v[2]).unwrap_or(pnw);
                let pse = pne + psw - pnw;
                let mut p = [pnw, pne, pse, psw];
                if !known && layer == 1 {
                    if let Some(f) = floor_f[idx] {
                        p = [f[0] + head, f[1] + head, f[2] + head, f[3] + head];
                    }
                }
                let actual: [i32; 4] = match sec_src {
                    Some(sx) => {
                        let h = if layer == 0 { sx.floor } else { sx.ceil };
                        [
                            src_f(&st, h[0])?,
                            src_f(&st, h[1])?,
                            src_f(&st, h[2])?,
                            src_f(&st, h[3])?,
                        ]
                    }
                    None => [0; 4],
                };
                let mode_want = if actual == p {
                    0
                } else if actual.iter().all(|v| *v == actual[0]) {
                    1
                } else {
                    2
                };
                let mode = tree(
                    st.c,
                    st.cx.corner_mode[layer] + prev_mode[layer] * 4,
                    2,
                    mode_want,
                ) as usize;
                prev_mode[layer] = mode;
                let v: [i32; 4] = match mode {
                    0 => p,
                    1 => {
                        let r = seg(st.c, st.cx.flat_seg[layer], actual[0] - p[0]);
                        [p[0] + r; 4]
                    }
                    _ => {
                        let mut v = [0; 4];
                        for i in 0..4 {
                            v[i] = p[i] + seg(st.c, st.cx.corner_seg[layer][i], actual[i] - p[i]);
                        }
                        v
                    }
                };
                if layer == 0 {
                    floor_f[idx] = Some(v);
                    last_nw[0] = v[0];
                } else {
                    ceil_f[idx] = Some(v);
                    last_nw[1] = v[0];
                    if let Some(f) = floor_f[idx] {
                        head = v[0] - f[0];
                    }
                }
            }
            if split {
                let sb = src.map(|r| {
                    let e = r.sectors[idx].ext;
                    r.splitbs[usize::from(r.exts[usize::from(e)].splitb)]
                });
                let mut fb = [0; 4];
                let mut cb = [0; 4];
                let base_f = floor_f[idx].unwrap_or([0; 4]);
                let base_c = ceil_f[idx].unwrap_or([0; 4]);
                for i in 0..4 {
                    let want = match sb {
                        Some(b) => src_f(&st, b.floor_b[i])? - base_f[i],
                        None => 0,
                    };
                    fb[i] = base_f[i] + seg(st.c, st.cx.splitb_seg, want);
                }
                for i in 0..4 {
                    let want = match sb {
                        Some(b) => src_f(&st, b.ceil_b[i])? - base_c[i],
                        None => 0,
                    };
                    cb[i] = base_c[i] + seg(st.c, st.cx.splitb_seg, want);
                }
                fb_f[idx] = Some(fb);
                cb_f[idx] = Some(cb);
            }
            // Walls.
            st.c.label(L_WALLS);
            let src_walls: Vec<WallF> = match (src, sec_src) {
                (Some(r), Some(sx)) if sx.ext != NO_EXT => {
                    let e = r.exts[usize::from(sx.ext)];
                    let mut v = Vec::new();
                    for k in 0..usize::from(e.wall_count) {
                        let cw = r.walls[usize::from(e.wall_first) + k];
                        v.push(WallF {
                            edge: cw.edge,
                            flags: cw.flags,
                            y: [
                                src_f(&st, cw.y[0])?,
                                src_f(&st, cw.y[1])?,
                                src_f(&st, cw.y[2])?,
                                src_f(&st, cw.y[3])?,
                            ],
                        });
                    }
                    v
                }
                _ => Vec::new(),
            };
            let count = eg(
                st.c,
                st.cx.wall_count[prev_count_nz],
                src_walls.len() as u32,
            ) as usize;
            prev_count_nz = usize::from(count > 0);
            for k in 0..count {
                let sw = src_walls.get(k).copied();
                let edge = tree(
                    st.c,
                    st.cx.wall_edge + prev_edge * 8,
                    3,
                    sw.map_or(0, |x| u32::from(x.edge)),
                ) as usize;
                prev_edge = edge;
                let flags = tree(
                    st.c,
                    st.cx.wall_flags,
                    2,
                    sw.map_or(0, |x| u32::from(x.flags)),
                );
                let (ia, ib) = EDGE_CORNERS[edge];
                let fl = floor_f[idx];
                let ce = ceil_f[idx];
                let prev_here = walls_f[idx].last().copied();
                let run = {
                    let (nx, nz) = if edge.is_multiple_of(2) || edge >= 4 {
                        (cx as i32 - 1, cz as i32)
                    } else {
                        (cx as i32, cz as i32 - 1)
                    };
                    if nx < 0 || nz < 0 || unit_of(nx as usize, nz as usize) != unit_of(cx, cz) {
                        None
                    } else {
                        walls_f[nz as usize * w + nx as usize]
                            .iter()
                            .filter(|x| usize::from(x.edge) == edge)
                            .nth(
                                walls_f[idx]
                                    .iter()
                                    .filter(|x| usize::from(x.edge) == edge)
                                    .count(),
                            )
                            .copied()
                    }
                };
                let preds: [Option<[i32; 4]>; 4] = {
                    let m0 = {
                        let ba = fl.map_or(0, |f| f[ia]);
                        let bb = fl.map_or(0, |f| f[ib]);
                        let (tb, ta) = match ce {
                            Some(c2) => (c2[ib], c2[ia]),
                            None => (bb + last_height, ba + last_height),
                        };
                        Some([ba, bb, tb, ta])
                    };
                    let m1 = prev_here.map(|p| {
                        let (ha, hb2) = (p.y[3] - p.y[0], p.y[2] - p.y[1]);
                        [p.y[3], p.y[2], p.y[2] + hb2, p.y[3] + ha]
                    });
                    [m0, m1, run.map(|r| r.y), prev_here.map(|p| p.y)]
                };
                let want_y = sw.map_or([0; 4], |x| x.y);
                let mut best = (usize::MAX, 0usize);
                for (m, p) in preds.iter().enumerate() {
                    if let Some(p) = p {
                        let cost: i32 = (0..4).map(|i| (want_y[i] - p[i]).abs()).sum();
                        let cost = cost as usize * 4 + m;
                        if cost < best.0 {
                            best = (cost, m);
                        }
                    }
                }
                let mode =
                    tree(st.c, st.cx.wall_mode + prev_wall_mode * 4, 2, best.1 as u32) as usize;
                prev_wall_mode = mode.min(4);
                let p = preds[mode].ok_or("wall predictor missing")?;
                let mut y = [0; 4];
                for i in 0..4 {
                    y[i] = p[i] + seg(st.c, st.cx.wall_seg[i], want_y[i] - p[i]);
                }
                last_height = (y[3] - y[0]).max(1);
                walls_f[idx].push(WallF {
                    edge: edge as u8,
                    flags: flags as u8,
                    y,
                });
            }
            st.c.label(L_SECTORS);
        }
    }
    // Rebuild the collision tables.
    let mut walls_total = 0usize;
    let mut split_total = 0usize;
    for idx in 0..cells {
        let fl = flag_words[idx];
        let mut sec = Sector {
            flags: fl,
            ext: NO_EXT,
            ..Sector::default()
        };
        if let Some(f) = floor_f[idx] {
            for i in 0..4 {
                sec.floor[i] = st.ru_of(f[i]) as i16;
            }
        }
        if let Some(f) = ceil_f[idx] {
            for i in 0..4 {
                sec.ceil[i] = st.ru_of(f[i]) as i16;
            }
        }
        let split = fl & SECTOR_SPLIT_LEVEL != 0;
        if fl & SECTOR_HAS_FLOOR != 0 {
            let nesw = fl & SECTOR_FLOOR_NESW != 0;
            let n = triangle_normal_q12(sec.floor, nesw, false, s as u32);
            if n[1] >= 2867 {
                sec.flags |= SECTOR_FLOOR_WALK_A;
            }
            let second = match fb_f[idx] {
                Some(f) if split => {
                    let mut h = [0i16; 4];
                    for i in 0..4 {
                        h[i] = st.ru_of(f[i]) as i16;
                    }
                    h
                }
                _ => sec.floor,
            };
            let n = triangle_normal_q12(second, nesw, true, s as u32);
            if n[1] >= 2867 {
                sec.flags |= SECTOR_FLOOR_WALK_B;
            }
        }
        let walls = &walls_f[idx];
        if !walls.is_empty() || split {
            let mut ext = SectorExt {
                wall_first: walls_total as u16,
                wall_count: walls.len() as u8,
                plane_first: 0,
                plane_count: 0,
                splitb: NO_EXT,
            };
            for wl in walls {
                let mut y = [0i16; 4];
                for i in 0..4 {
                    y[i] = st.ru_of(wl.y[i]) as i16;
                }
                out.walls.push(ColWall {
                    edge: wl.edge,
                    flags: wl.flags,
                    y,
                    reserved: 0,
                });
            }
            walls_total += walls.len();
            if split {
                let mut sb = SplitB::default();
                let fb = fb_f[idx].unwrap();
                let cb = cb_f[idx].unwrap();
                for i in 0..4 {
                    sb.floor_b[i] = st.ru_of(fb[i]) as i16;
                    sb.ceil_b[i] = st.ru_of(cb[i]) as i16;
                }
                ext.splitb = split_total as u16;
                split_total += 1;
                out.splitbs.push(sb);
            }
            sec.ext = out.exts.len() as u16;
            out.exts.push(ext);
        }
        out.sectors.push(sec);
    }
    psx_grid::collision::summarize_sectors(&mut out.sectors, w, d);

    // ----- render layer --------------------------------------------------
    let mut vmap: HashMap<[i32; 3], u16> = HashMap::new();
    let mut cmap: HashMap<(u16, [i8; 3]), u32> = HashMap::new();
    let mut near: HashMap<(i32, i32, [i8; 3]), Vec<(i32, u32)>> = HashMap::new();
    let mut ntab: Vec<[i8; 3]> = Vec::new();
    let mut mtf: Vec<u8> = (0..env.materials.len() as u16).map(|v| v as u8).collect();
    let mut cpal: Vec<u32> = Vec::new();
    let mut cmtf: Vec<u32> = Vec::new();
    let mut last_colour = 0u32;
    let mut prev_sym = 0usize;
    let mut prev_var = [[0usize; 9]; 3];
    let mut prev_uvmode = [[0usize; 4]; 3];
    let limit_base = |mat: u8| -> i64 {
        if repeat_on && env.materials[usize::from(mat)].tex_shape != 0 {
            4
        } else {
            1
        }
    };
    let mut cur_unit = usize::MAX;
    for &(cx, cz) in &order {
        {
            let idx = cz * w + cx;
            if unit_of(cx, cz) != cur_unit {
                cur_unit = unit_of(cx, cz);
                if unit > 0 {
                    st.c.reset();
                    vmap.clear();
                    cmap.clear();
                    near.clear();
                    ntab.clear();
                    mtf = (0..env.materials.len() as u16).map(|v| v as u8).collect();
                    cpal.clear();
                    cmtf.clear();
                    last_colour = 0;
                    prev_sym = 0;
                    prev_var = [[0usize; 9]; 3];
                    prev_uvmode = [[0usize; 4]; 3];
                }
            }
            let (cxi, czi) = (cx as i32, cz as i32);
            let fl = flag_words[idx];
            let src_range = src.map(|r| r.cell_quads(idx));
            let first_quad = out.quads.len();
            let walls = &walls_f[idx];
            let (mut wi, mut wstart): (usize, Option<i32>) = (0, None);
            let floor = floor_f[idx];
            let ceil = ceil_f[idx];
            let fnesw = fl & SECTOR_FLOOR_NESW != 0;
            let cnesw = fl & SECTOR_CEIL_NESW != 0;
            let mut sq = src_range.clone().map(|rg| rg.start);
            let der = derived_slots(
                &Sectors {
                    w,
                    d,
                    floor: &floor_f,
                    ceil: &ceil_f,
                    fb: &fb_f,
                    cb: &cb_f,
                    flags: &flag_words,
                },
                cxi,
                czi,
                open_wall_f,
            );
            let mut di = 0usize;
            let mut pending: Vec<u32> = Vec::new();
            loop {
                // Candidate rings by item symbol.
                let cand = |sym: u32,
                            wi: usize,
                            wstart: Option<i32>,
                            di: usize|
                 -> Option<(Vec<P3>, Option<(usize, Option<i32>)>)> {
                    let tri_ring = |h: [i32; 4], nesw: bool, t: usize| -> Vec<P3> {
                        tri_corners(nesw, t)
                            .iter()
                            .map(|&i| {
                                let (x, z) = corner_xz(cxi, czi, i);
                                [x, h[i], z]
                            })
                            .collect()
                    };
                    let quad_ring = |h: [i32; 4]| -> Vec<P3> {
                        (0..4)
                            .map(|i| {
                                let (x, z) = corner_xz(cxi, czi, i);
                                [x, h[i], z]
                            })
                            .collect()
                    };
                    match sym {
                        IT_FLOOR_Q => floor.map(|h| (quad_ring(h), None)),
                        IT_FLOOR_T0 => floor.map(|h| (tri_ring(h, fnesw, 0), None)),
                        IT_FLOOR_T1 => {
                            floor.map(|h| (tri_ring(fb_f[idx].unwrap_or(h), fnesw, 1), None))
                        }
                        IT_CEIL_Q => ceil.map(|h| (quad_ring(h), None)),
                        IT_CEIL_T0 => ceil.map(|h| (tri_ring(h, cnesw, 0), None)),
                        IT_CEIL_T1 => {
                            ceil.map(|h| (tri_ring(cb_f[idx].unwrap_or(h), cnesw, 1), None))
                        }
                        IT_WALL => wall_piece(cxi, czi, walls, wi, wstart, yoff_f, courses)
                            .map(|p| (p.ring, Some(p.next))),
                        IT_DER => der.get(di).map(|r| (r.clone(), None)),
                        _ => None,
                    }
                };
                // Encoder: choose the item for the next source quad.
                let mut want_var = 0usize;
                let mut src_ring: Vec<P3> = Vec::new();
                let mut src_q: Option<Quad> = None;
                if enc && pending.is_empty() {
                    let mut want = vec![IT_END];
                    if let (Some(r), Some(qi), Some(rg)) = (src, sq, src_range.as_ref()) {
                        if qi < rg.end {
                            let q = r.quads[qi];
                            let n = if q.flags & QUAD_TRI != 0 { 3 } else { 4 };
                            let click = s / 4;
                            src_ring = (0..n)
                                .map(|k| {
                                    let p = r.pos(q.v[k]);
                                    Ok([p[0] / click, src_f(&st, p[1] as i16)?, p[2] / click])
                                })
                                .collect::<Result<Vec<_>, String>>()?;
                            want = vec![IT_ESC];
                            'search: for sym in [
                                IT_FLOOR_Q,
                                IT_FLOOR_T0,
                                IT_FLOOR_T1,
                                IT_CEIL_Q,
                                IT_CEIL_T0,
                                IT_CEIL_T1,
                            ] {
                                if let Some((ring, _)) = cand(sym, wi, wstart, di) {
                                    if let Some(v) = variant_index(&ring, &src_ring) {
                                        want = vec![sym];
                                        want_var = v;
                                        break 'search;
                                    }
                                }
                            }
                            if want == vec![IT_ESC] {
                                for skip in 0..4usize {
                                    let (w2, s2) = if skip == 0 {
                                        (wi, wstart)
                                    } else {
                                        (wi + skip, None)
                                    };
                                    if let Some((ring, _)) = cand(IT_WALL, w2, s2, di) {
                                        if let Some(v) = variant_index(&ring, &src_ring) {
                                            want = vec![IT_SKIP; skip];
                                            want.push(IT_WALL);
                                            want_var = v;
                                            break;
                                        }
                                    }
                                }
                            }
                            if want == vec![IT_ESC] {
                                for skip in 0..der.len().saturating_sub(di) {
                                    if let Some((ring, _)) = cand(IT_DER, wi, wstart, di + skip) {
                                        if let Some(v) = variant_index(&ring, &src_ring) {
                                            want = vec![IT_DSKIP; skip];
                                            want.push(IT_DER);
                                            want_var = v;
                                            break;
                                        }
                                    }
                                }
                            }
                            src_q = Some(q);
                        }
                    }
                    pending = want;
                } else if enc {
                    // Still inside a skip run: the source quad is the same one.
                    if let (Some(r), Some(qi)) = (src, sq) {
                        let q = r.quads[qi];
                        let n = if q.flags & QUAD_TRI != 0 { 3 } else { 4 };
                        let click = s / 4;
                        src_ring = (0..n)
                            .map(|k| {
                                let p = r.pos(q.v[k]);
                                Ok([p[0] / click, src_f(&st, p[1] as i16)?, p[2] / click])
                            })
                            .collect::<Result<Vec<_>, String>>()?;
                        src_q = Some(q);
                        // Recompute the variant for the final item.
                        if let Some(sym) = pending
                            .iter()
                            .copied()
                            .find(|x| *x != IT_SKIP && *x != IT_DSKIP)
                        {
                            let (w2, d2) = (
                                wi + pending.iter().filter(|x| **x == IT_SKIP).count(),
                                di + pending.iter().filter(|x| **x == IT_DSKIP).count(),
                            );
                            let s2 = if pending.contains(&IT_SKIP) {
                                None
                            } else {
                                wstart
                            };
                            if let Some((ring, _)) = cand(sym, w2, s2, d2) {
                                want_var = variant_index(&ring, &src_ring).unwrap_or(0);
                            }
                        }
                    }
                }
                st.c.label(L_ITEMS);
                let mut sym;
                loop {
                    let ask = if enc { pending.remove(0) } else { IT_END };
                    sym = tree(st.c, st.cx.item + prev_sym * 16, 4, ask);
                    prev_sym = sym as usize;
                    match sym {
                        IT_SKIP => {
                            wi += 1;
                            wstart = None;
                            continue;
                        }
                        IT_DSKIP => {
                            di += 1;
                            continue;
                        }
                        _ => break,
                    }
                }
                if sym == IT_END {
                    break;
                }
                if enc {
                    ITEM_COUNT[sym as usize].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                // Ring.
                let class = match sym {
                    IT_FLOOR_Q | IT_FLOOR_T0 | IT_FLOOR_T1 => 0,
                    IT_CEIL_Q | IT_CEIL_T0 | IT_CEIL_T1 => 1,
                    _ => 2,
                };
                let ring: Vec<P3> = if sym == IT_ESC {
                    st.c.label(L_ESC);
                    let tri = st.c.bit(st.cx.esc_n, src_ring.len() == 3);
                    let n = if tri { 3 } else { 4 };
                    let base_f = floor.map_or(0, |f| f[0]);
                    let mut ring: Vec<P3> = Vec::new();
                    for k in 0..n {
                        let sp = src_ring.get(k).copied().unwrap_or([0; 3]);
                        let (ref_x, ref_f, ref_z, ctxs) = if k == 0 {
                            (cxi * 4, base_f, czi * 4, st.cx.esc_first)
                        } else {
                            (
                                ring[k - 1][0],
                                ring[k - 1][1],
                                ring[k - 1][2],
                                st.cx.esc_next,
                            )
                        };
                        let dx = seg(st.c, ctxs[0], sp[0] - ref_x);
                        let df = seg(st.c, ctxs[1], sp[1] - ref_f);
                        let dz = seg(st.c, ctxs[2], sp[2] - ref_z);
                        ring.push([ref_x + dx, ref_f + df, ref_z + dz]);
                    }
                    ring
                } else {
                    let (base, adv) =
                        cand(sym, wi, wstart, di).ok_or("item without a prediction")?;
                    let nvar = if base.len() == 3 { 6 } else { 8 };
                    let _ = nvar;
                    st.c.label(L_ITEMS);
                    let v = tree(
                        st.c,
                        st.cx.variant + (class * 9 + prev_var[class][0]) * 8,
                        3,
                        want_var as u32,
                    ) as usize;
                    prev_var[class][0] = v;
                    if let Some((nw, ns)) = adv {
                        wi = nw;
                        wstart = ns;
                    }
                    if sym == IT_DER {
                        di += 1;
                    }
                    variant_ring(&base, v)
                };
                let n = ring.len();
                // Vertices by welding.
                let mut vids = [0u16; 4];
                for k in 0..n {
                    let key = ring[k];
                    let next = out.vertices.len();
                    let id = *vmap.entry(key).or_insert_with(|| {
                        let p = [key[0] * (s / 4), y_of(key[1], yoff_f, s), key[2] * (s / 4)];
                        out.vertices.push(Vtx {
                            x: p[0] as i16,
                            y: p[1] as i16,
                            z: p[2] as i16,
                            pad: 0,
                        });
                        next as u16
                    });
                    vids[k] = id;
                }
                if n == 3 {
                    vids[3] = vids[2];
                }
                // Derived plane.
                let ru: Vec<[i32; 3]> = ring.iter().map(|p| st.pos_ru(*p)).collect();
                let normal = ring_normal(&ru);
                let zeros = normal.iter().filter(|v| **v == 0).count();
                let mut flags = 0u8;
                if n == 3 {
                    flags |= QUAD_TRI;
                }
                let axis = (zeros == 2).then(|| normal.iter().position(|v| *v != 0).unwrap());
                // Material.
                st.c.label(L_MATERIAL);
                let src_mat = src_q.map_or(0u8, |q| q.material);
                let rank = mtf.iter().position(|m| *m == src_mat).unwrap_or(0);
                let rank = eg(st.c, st.cx.material + class * EG_CTX, rank as u32) as usize;
                let material = mtf.remove(rank);
                mtf.insert(0, material);
                let mat = env.materials[usize::from(material)];
                // Double.
                let extra_double =
                    src_q.is_some_and(|q| q.flags & QUAD_DOUBLE != 0) && (mat.flags & 3 != 2);
                let extra_double = st.c.bit(st.cx.double + class, extra_double);
                if extra_double || mat.flags & 3 == 2 {
                    flags |= QUAD_DOUBLE;
                }
                // Normal and plane constant.
                st.c.label(L_NORMAL);
                let n8: [i8; 3];
                let dconst: i16;
                if let Some(ax) = axis {
                    let neg = normal[ax] < 0;
                    let mut v = [0i8; 3];
                    v[ax] = if neg { -127 } else { 127 };
                    n8 = v;
                    flags |= QUAD_AXIS | ((ax as u8) << QUAD_AXIS_SHIFT);
                    if neg {
                        flags |= QUAD_AXIS_NEG;
                    }
                    let coord = ru[0][ax];
                    dconst = if neg { -coord } else { coord } as i16;
                } else {
                    if enc {
                        NORMAL_STORED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    let want = src_q.map_or([0i8; 3], |q| q.n);
                    let pos = ntab.iter().position(|x| *x == want).map_or(0, |p| p + 1);
                    let got = eg(st.c, st.cx.nidx, pos as u32) as usize;
                    if got == 0 {
                        let mut v = [0i8; 3];
                        for i in 0..3 {
                            v[i] =
                                (st.c.raw(8, (i32::from(want[i]) + 128) as u32) as i32 - 128) as i8;
                        }
                        ntab.push(v);
                        n8 = v;
                    } else {
                        n8 = ntab[got - 1];
                    }
                    let p0 = ru[0];
                    dconst = ((i32::from(n8[0]) * p0[0]
                        + i32::from(n8[1]) * p0[1]
                        + i32::from(n8[2]) * p0[2])
                        >> 7) as i16;
                }
                // UV.
                st.c.label(L_UV);
                let lim = limit_base(material);
                let mut uv = [0u16; 4];
                let mut repeat = false;
                if mat.tex_shape == 0 {
                    let _ = tree(
                        st.c,
                        st.cx.uvmode + (class * 4 + prev_uvmode[class][0]) * 4,
                        2,
                        1,
                    );
                    prev_uvmode[class][0] = 1;
                } else {
                    let mut want_mode = 0u32;
                    if let Some(q) = src_q {
                        for mode in 1..4u32 {
                            if let Some(t) = texels(env, yoff_f, &ring, mode) {
                                let (u8s, rep) = rebase(&t, lim);
                                let ok = (0..n).all(|k| {
                                    q.uv[k] == (u16::from(u8s[k][0]) | (u16::from(u8s[k][1]) << 8))
                                }) && (rep == (q.flags2 & QUAD2_REPEAT != 0));
                                if ok {
                                    want_mode = mode;
                                    break;
                                }
                            }
                        }
                    }
                    let mode = tree(
                        st.c,
                        st.cx.uvmode + (class * 4 + prev_uvmode[class][0]) * 4,
                        2,
                        want_mode,
                    );
                    prev_uvmode[class][0] = mode as usize;
                    if mode == 0 {
                        if enc {
                            UV_EXPLICIT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                        for k in 0..n {
                            let want = src_q.map_or(0, |q| u32::from(q.uv[k]));
                            uv[k] = st.c.raw(16, want) as u16;
                        }
                        repeat = st.c.bit(
                            st.cx.uv_repeat,
                            src_q.is_some_and(|q| q.flags2 & QUAD2_REPEAT != 0),
                        );
                    } else {
                        let t = texels(env, yoff_f, &ring, mode).ok_or("uv mode without texels")?;
                        let (u8s, rep) = rebase(&t, lim);
                        for k in 0..n {
                            uv[k] = u16::from(u8s[k][0]) | (u16::from(u8s[k][1]) << 8);
                        }
                        repeat = rep;
                    }
                }
                if n == 3 {
                    uv[3] = uv[2];
                }
                // Colour per corner.
                st.c.label(L_COLOUR);
                let mut colours = [0u32; 4];
                for k in 0..n {
                    let key = (vids[k], n8);
                    if let Some(col) = cmap.get(&key) {
                        colours[k] = *col;
                        continue;
                    }
                    let want = src_q.map_or(0, |q| q.color[k]);
                    let ctx_k = usize::from(k > 0);
                    let col = match st.par.colour {
                        ColourMode::Palette
                        | ColourMode::PaletteDelta
                        | ColourMode::PaletteNear => {
                            let near_pred = if st.par.colour == ColourMode::PaletteNear {
                                let p = ring[k];
                                let mut best: Option<(i32, u32)> = None;
                                for (dx, dz) in [(-4, 0), (0, -4), (4, 0), (0, 4), (0, 0)] {
                                    if let Some(list) = near.get(&(p[0] + dx, p[2] + dz, n8)) {
                                        for (f, col) in list {
                                            let dist = (dx.abs() + dz.abs()) * 4 + (f - p[1]).abs();
                                            if best.is_none_or(|b| dist < b.0) {
                                                best = Some((dist, *col));
                                            }
                                        }
                                    }
                                }
                                best.map(|b| b.1)
                            } else {
                                None
                            };
                            let prev_corner =
                                near_pred.or(if k > 0 { Some(colours[k - 1]) } else { None });
                            let mode_want = if prev_corner == Some(want) {
                                0
                            } else if cmtf.contains(&want) {
                                1
                            } else if cpal.contains(&want) {
                                2
                            } else {
                                3
                            };
                            let mode = tree(st.c, st.cx.cmode + ctx_k * 4, 2, mode_want);
                            match mode {
                                0 => prev_corner.unwrap_or(0),
                                1 => {
                                    let r = cmtf.iter().position(|c2| *c2 == want).unwrap_or(0);
                                    let r = tree(st.c, st.cx.cmtf, 4, r as u32) as usize;
                                    cmtf[r.min(cmtf.len().saturating_sub(1))]
                                }
                                2 => {
                                    let r = cpal.iter().position(|c2| *c2 == want).unwrap_or(0);
                                    let r = eg(st.c, st.cx.cpal, r as u32) as usize;
                                    cpal[r]
                                }
                                _ if st.par.colour != ColourMode::Palette => {
                                    let prev = prev_corner.unwrap_or(last_colour);
                                    let mut out_c = 0u32;
                                    for ch in 0..3 {
                                        let pw = ((prev >> (8 * ch)) & 0xff) as i32;
                                        let ww = ((want >> (8 * ch)) & 0xff) as i32;
                                        let r = seg(st.c, st.cx.cdelta[ch], ww - pw);
                                        out_c |= ((pw + r) as u32 & 0xff) << (8 * ch);
                                    }
                                    out_c
                                }
                                _ => st.c.raw(24, want),
                            }
                        }
                        ColourMode::Delta => {
                            let prev = if k > 0 { colours[k - 1] } else { last_colour };
                            let same = st.c.bit(st.cx.cmode + ctx_k, want == prev);
                            if same {
                                prev
                            } else {
                                let mut out_c = 0u32;
                                for ch in 0..3 {
                                    let pw = ((prev >> (8 * ch)) & 0xff) as i32;
                                    let ww = ((want >> (8 * ch)) & 0xff) as i32;
                                    let r = seg(st.c, st.cx.cdelta[ch], ww - pw);
                                    out_c |= ((pw + r) as u32 & 0xff) << (8 * ch);
                                }
                                out_c
                            }
                        }
                    };
                    colours[k] = col;
                    last_colour = col;
                    if st.par.colour != ColourMode::Delta {
                        if let Some(p) = cmtf.iter().position(|c2| *c2 == col) {
                            cmtf.remove(p);
                        }
                        cmtf.insert(0, col);
                        cmtf.truncate(16);
                        if !cpal.contains(&col) {
                            cpal.push(col);
                        }
                    }
                    cmap.insert(key, col);
                    near.entry((ring[k][0], ring[k][2], n8))
                        .or_default()
                        .push((ring[k][1], col));
                }
                if n == 3 {
                    colours[3] = colours[2];
                }
                let mut q = Quad {
                    v: vids,
                    color: colours,
                    uv,
                    d: dconst,
                    n: n8,
                    material,
                    flags,
                    flags2: if repeat { QUAD2_REPEAT } else { 0 },
                };
                if n == 3 {
                    q.v[3] = q.v[2];
                }
                out.quads.push(q);
                if let Some(qi) = sq.as_mut() {
                    *qi += 1;
                }
            }
            // Cell record.
            st.c.label(L_ITEMS);
            let count = out.quads.len() - first_quad;
            let src_open = src.is_some_and(|r| r.cells[idx].flags & psx_grid::CELL_OPEN_SKY != 0);
            let ctx = usize::from(fl & SECTOR_HAS_FLOOR != 0) * 2
                + usize::from(fl & SECTOR_HAS_CEIL != 0);
            let open = st.c.bit(st.cx.open_sky + ctx, src_open);
            let (mut lo, mut hi) = (i32::MAX, i32::MIN);
            for q in &out.quads[first_quad..] {
                let n = if q.flags & QUAD_TRI != 0 { 3 } else { 4 };
                for k in 0..n {
                    let y = i32::from(out.vertices[usize::from(q.v[k])].y);
                    lo = lo.min(y);
                    hi = hi.max(y);
                }
            }
            out.cells.push(RCell {
                first: first_quad as u16,
                count: count as u8,
                flags: u8::from(open),
                min_y: if count == 0 { 0 } else { lo as i16 },
                max_y: if count == 0 { 0 } else { hi as i16 },
            });
            let _ = (&mut prev_var, &mut prev_uvmode);
        }
    }
    // Derived tables.
    let mut used = [false; 256];
    for q in &out.quads {
        used[usize::from(q.material)] = true;
    }
    out.material_use = (0..=255u8).filter(|m| used[usize::from(*m)]).collect();
    let block = 1usize << lod_shift;
    for bz in 0..d.div_ceil(block) {
        for bx in 0..w.div_ceil(block) {
            let (mut lo, mut hi) = (i16::MAX, i16::MIN);
            let mut flags = 0u8;
            for dz in 0..block {
                for dx in 0..block {
                    let (cx, cz) = (bx * block + dx, bz * block + dz);
                    if cx >= w || cz >= d {
                        continue;
                    }
                    let cell = out.cells[cz * w + cx];
                    if cell.count > 0 {
                        lo = lo.min(cell.min_y);
                        hi = hi.max(cell.max_y);
                        if cell.flags & psx_grid::CELL_OPEN_SKY != 0 {
                            flags |= psx_grid::BLOCK_OPEN_SKY;
                        }
                    }
                }
            }
            out.lod_blocks.push(LodBlock {
                first: 0,
                count: 0,
                flags,
                min_y: if lo > hi { 0 } else { lo },
                max_y: if lo > hi { 0 } else { hi },
            });
        }
    }
    out.flags = src.map_or(0, |r| r.flags);
    let _ = enc;
    Ok(out)
}
