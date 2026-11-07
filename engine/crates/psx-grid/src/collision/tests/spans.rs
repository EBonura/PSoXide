//! Columns of several spans (`ROOM_SPANS`): every span of a column offers
//! its own floor and ceiling slabs, in dense and in tree rooms.
//!
//! Three kinds of check:
//!
//! * a hand made two-span column with the answers worked out by hand;
//! * a brute force scan over every span's slab intervals (flat spans);
//! * the same columns written as **stacked single-span rooms**, one room per
//!   span level over the same footprint. Stacked rooms just add their slabs,
//!   so every query and every trace must answer the same on both.

use super::leaf::{build_chunks, random_room, tree_world, TLeaf, TreeRoom};
use super::*;
use crate::write::SpanTables;

/// Edge of a click (a quarter sector), runtime units.
const Q: i32 = S / 4;

/// The heights of a flat span: the floor (a pit when `None`), and the
/// ceiling (open sky when `None`), solid or not.
fn flat_span(floor: Option<i16>, ceil: Option<i16>, solid: bool) -> Sector {
    let mut flags = 0;
    if floor.is_some() {
        flags |= FLAT;
    }
    if ceil.is_some() {
        flags |= SECTOR_HAS_CEIL;
    }
    if solid {
        flags |= SECTOR_CEIL_SOLID;
    }
    Sector {
        floor: [floor.unwrap_or(0); 4],
        ceil: [ceil.unwrap_or(0); 4],
        flags,
        ext: NO_EXT,
    }
}

/// What a level without a span holds: no floor, no ceiling.
const HOLE: Sector = Sector {
    floor: [0; 4],
    ceil: [0; 4],
    flags: 0,
    ext: NO_EXT,
};

/// Every column of a dense room: the ground span, then the extra spans.
struct Cols {
    w: u8,
    d: u8,
    cells: Vec<Vec<Sector>>,
}

impl Cols {
    /// A room whose every column is the single span `ground`.
    fn new(w: u8, d: u8, ground: Sector) -> Self {
        Self {
            w,
            d,
            cells: vec![vec![ground]; usize::from(w) * usize::from(d)],
        }
    }

    fn column(&mut self, cx: usize, cz: usize) -> &mut Vec<Sector> {
        let w = usize::from(self.w);
        &mut self.cells[cz * w + cx]
    }

    /// One room whose records are the ground spans and whose span tables
    /// hold the rest, with the summary bits a cook that knows nothing about
    /// spans would write.
    fn layered_chunk(&self, summary: bool) -> RoomChunk {
        let mut sectors: Vec<Sector> = self.cells.iter().map(|c| c[0]).collect();
        if summary {
            summarize_sectors(&mut sectors, usize::from(self.w), usize::from(self.d));
        } else {
            summarize_leaves(&mut sectors);
            for s in &mut sectors {
                s.flags &= !(SECTOR_FLAT_FLOOR | SECTOR_FLAT_CEIL);
            }
        }
        let mut index = Vec::new();
        let mut runs = Vec::new();
        let mut spans = Vec::new();
        for cell in &self.cells {
            if cell.len() == 1 {
                index.push(NO_SPAN);
                continue;
            }
            index.push(runs.len() as u16);
            runs.push(SpanRun {
                first: spans.len() as u16,
                count: (cell.len() - 1) as u16,
            });
            let mut extras = cell[1..].to_vec();
            summarize_leaves(&mut extras);
            spans.extend(extras);
        }
        RoomChunk {
            width: self.w,
            depth: self.d,
            sectors,
            spans: (!runs.is_empty()).then_some(SpanTables { index, runs, spans }),
            ..RoomChunk::default()
        }
    }

    fn layered(&self, summary: bool) -> Built {
        let room = Room::flat([0, 0, 0], self.w, self.d, 0);
        build_chunks(&[(room, self.layered_chunk(summary), false)])
    }

    /// One single-span room per span level over the same footprint.
    fn stacked(&self) -> Built {
        let levels = self.cells.iter().map(Vec::len).max().unwrap_or(1);
        let rooms: Vec<(Room, RoomChunk, bool)> = (0..levels)
            .map(|k| {
                let mut sectors: Vec<Sector> = self
                    .cells
                    .iter()
                    .map(|c| c.get(k).copied().unwrap_or(HOLE))
                    .collect();
                summarize_sectors(&mut sectors, usize::from(self.w), usize::from(self.d));
                let chunk = RoomChunk {
                    width: self.w,
                    depth: self.d,
                    sectors,
                    ..RoomChunk::default()
                };
                (Room::flat([0, 0, 0], self.w, self.d, 0), chunk, false)
            })
            .collect();
        build_chunks(&rooms)
    }

    /// The span list at a room-local point, none outside the room.
    fn at(&self, x: i32, z: i32) -> Option<&[Sector]> {
        let (cx, cz) = (x.div_euclid(S), z.div_euclid(S));
        if x < 0 || z < 0 || cx >= i32::from(self.w) || cz >= i32::from(self.d) {
            return None;
        }
        Some(&self.cells[cz as usize * usize::from(self.w) + cx as usize])
    }
}

/// A random flat or sloped column of one to three spans, bottom to top.
fn random_column(rng: &mut Lcg, sloped: bool) -> Vec<Sector> {
    let n = 1 + usize::from(rng.range(0, 99) < 50) + usize::from(rng.range(0, 99) < 20);
    let mut base = rng.range(-40, 40);
    let mut out = Vec::new();
    for k in 0..n {
        let last = k + 1 == n;
        let has_floor = k > 0 || rng.range(0, 99) >= 15;
        let has_ceil = !last || rng.range(0, 99) >= 25;
        let solid = rng.range(0, 99) >= 20;
        let corners = |rng: &mut Lcg, at: i32| -> [i16; 4] {
            if sloped && rng.range(0, 99) < 60 {
                [0; 4].map(|_| (at + rng.range(-14, 14)) as i16)
            } else {
                [at as i16; 4]
            }
        };
        let floor = corners(rng, base);
        let top = base + rng.range(30, 110);
        let ceil = corners(rng, top);
        let mut flags = 0;
        if has_floor {
            flags |= FLAT;
        }
        if has_ceil {
            flags |= SECTOR_HAS_CEIL;
        }
        if solid {
            flags |= SECTOR_CEIL_SOLID;
        }
        if sloped && rng.range(0, 1) == 0 {
            flags |= SECTOR_FLOOR_NESW;
        }
        if sloped && rng.range(0, 1) == 0 {
            flags |= SECTOR_CEIL_NESW;
        }
        out.push(Sector {
            floor,
            ceil,
            flags,
            ext: NO_EXT,
        });
        base = top + rng.range(20, 100);
    }
    out
}

fn random_cols(seed: u32, w: u8, d: u8, sloped: bool) -> Cols {
    let mut rng = Lcg(seed);
    let mut cols = Cols::new(w, d, HOLE);
    for cell in &mut cols.cells {
        *cell = random_column(&mut rng, sloped);
    }
    cols
}

/// The two-span column of the hand checks, with a single-span neighbour all
/// around: ground floor 0 under a solid ceiling at 240, a second floor at
/// 320 under a solid ceiling at 560. The neighbours are floor 0, open sky.
fn two_span_cols() -> Cols {
    let mut cols = Cols::new(3, 3, flat_span(Some(0), None, false));
    *cols.column(1, 1) = vec![
        flat_span(Some(0), Some(240), true),
        flat_span(Some(320), Some(560), true),
    ];
    cols
}

/// The tree room of `cols` with whole sector leaves only.
fn tree_of(cols: &Cols) -> TreeRoom {
    let mut room = TreeRoom::new([0, 0, 0], cols.w, cols.d, (0, 0));
    for cz in 0..i32::from(cols.d) {
        for cx in 0..i32::from(cols.w) {
            let column = &cols.cells[(cz * i32::from(cols.w) + cx) as usize];
            let mut leaf = TLeaf::new(cx * 4, cz * 4, leaf_code::SECTOR, [0; 4]);
            leaf.sector = column[0];
            leaf.extras = column[1..].to_vec();
            room.leaves.push(leaf);
        }
    }
    room
}

fn tree_built(room: &TreeRoom) -> Built {
    tree_world(core::slice::from_ref(room))
}

/// The hand checks on the column of [`two_span_cols`], in any layout.
fn check_two_span_column(w: &GridCollision<'_>) {
    // Standing on the ground floor, in the open and under the slab.
    assert!(!w.body_blocked([120, 0, 120], 12, 48));
    assert!(!w.body_blocked([20, 0, 120], 12, 48));
    // A body rising from the ground floor is stopped by the ceiling of the
    // ground span: feet 192 (head against the slab's underside at 240 plus
    // nothing, the slab interval opens at 240 minus the height).
    let t = trace(w, body([120, 0, 120], [120, 300, 120]));
    assert!(!t.start_solid);
    assert_eq!(t.end.y, 192, "{t:?}");
    assert_eq!(t.normal_q12, [0, -4096, 0]);
    // Standing on the upper floor is fine, inside the slab below is not.
    assert!(!w.body_blocked([120, 320, 120], 12, 48));
    assert!(!w.body_blocked([120, 400, 120], 12, 48));
    assert!(w.body_blocked([120, 300, 120], 12, 48));
    assert!(w.body_blocked([120, 250, 120], 12, 48));
    // The upper span's own ceiling stops a body inside it: feet 512.
    assert!(!w.body_blocked([120, 500, 120], 12, 48));
    assert!(w.body_blocked([120, 520, 120], 12, 48));
    let t = trace(w, body([120, 330, 120], [120, 700, 120]));
    assert_eq!(t.end.y, 512, "{t:?}");
    assert_eq!(t.normal_q12, [0, -4096, 0]);
    // A body falling inside the upper span lands on the upper floor.
    let t = trace(w, body([120, 500, 120], [120, -100, 120]));
    assert_eq!(t.end.y, 320, "{t:?}");
    assert_eq!(t.normal_q12, [0, 4096, 0]);
    // A slanted point ray from inside the upper span hits the upper floor,
    // not the ground floor 320 units lower.
    let t = trace(w, point([100, 500, 100], [140, 100, 140]));
    assert!(t.fraction_q12 < 4096, "{t:?}");
    assert!((t.end.y - 320).abs() <= 1, "{t:?}");
    assert_eq!(t.normal_q12, [0, 4096, 0]);
    // Rays from the side at the height of either slab stop at the column.
    for y in [244, 250, 308, 316] {
        let t = trace(w, point([20, y, 120], [200, y, 120]));
        assert!(t.fraction_q12 < 4096, "y {y}: {t:?}");
        assert!((78..=80).contains(&t.end.x), "y {y}: {t:?}");
        assert_eq!(t.normal_q12, [-4096, 0, 0], "y {y}");
    }
    // Between the two slab intervals the thin slab model leaves a gap (the
    // slabs are 16 thick, the solid between the spans 80).
    let t = trace(w, point([20, 280, 120], [200, 280, 120]));
    assert_eq!(t.fraction_q12, 4096);
    // Bodies walking into the column from the side.
    let low = trace(w, body([20, 100, 120], [200, 100, 120]));
    assert_eq!(low.fraction_q12, 4096, "under the slab, the head is clear");
    let at_slab = trace(w, body([20, 200, 120], [200, 200, 120]));
    assert!(at_slab.fraction_q12 < 4096, "{at_slab:?}");
    let on_upper = trace(w, body([20, 330, 120], [200, 330, 120]));
    assert_eq!(on_upper.fraction_q12, 4096, "{on_upper:?}");
    let in_floor = trace(w, body([20, 300, 120], [200, 300, 120]));
    assert!(in_floor.fraction_q12 < 4096, "{in_floor:?}");
    // Point queries and line of sight.
    assert!(w.point_in_solid([120, 248, 120]));
    assert!(w.point_in_solid([120, 312, 120]));
    assert!(!w.point_in_solid([120, 280, 120]));
    assert!(!w.point_in_solid([120, 400, 120]));
    assert!(w.line_of_sight([20, 330, 120], [200, 330, 120]));
    assert!(!w.line_of_sight([20, 250, 120], [200, 250, 120]));
    assert!(!w.line_of_sight([120, 100, 120], [120, 400, 120]));
    assert!(w.line_of_sight([120, 330, 120], [120, 500, 120]));
    // The single-span neighbour is untouched.
    assert!(!w.body_blocked([20, 300, 20], 12, 48));
    let t = trace(w, body([20, 500, 20], [20, -100, 20]));
    assert_eq!(t.end.y, 0);
    // Heights: the column's spans, from the ground one up.
    assert_eq!(w.floor_at(0, 1, 1, 40, 40), Some(0));
    assert_eq!(w.ceil_at(0, 1, 1, 40, 40), Some(240));
    assert_eq!(w.floor_below(0, (1, 1), (40, 40), 500), Some(320));
    assert_eq!(w.floor_below(0, (1, 1), (40, 40), 320), Some(320));
    assert_eq!(w.floor_below(0, (1, 1), (40, 40), 300), Some(0));
    assert_eq!(w.floor_below(0, (1, 1), (40, 40), -5), None);
    assert_eq!(w.ceil_above(0, (1, 1), (40, 40), 100), Some(240));
    assert_eq!(w.ceil_above(0, (1, 1), (40, 40), 300), Some(560));
    assert_eq!(w.ceil_above(0, (1, 1), (40, 40), 600), None);
    assert_eq!(w.floor_below(0, (0, 0), (40, 40), 500), Some(0));
    // Locate names the column from inside either span.
    for y in [100, 330] {
        let at = w.locate(0, [120, y, 120]).unwrap();
        assert_eq!((at.room, at.cx, at.cz), (0, 1, 1), "y {y}");
    }
}

#[test]
fn a_two_span_dense_column_blocks_and_carries_by_span() {
    for summary in [true, false] {
        two_span_cols().layered(summary).run(check_two_span_column);
    }
}

#[test]
fn a_two_span_tree_column_answers_like_the_dense_one() {
    let cols = two_span_cols();
    tree_built(&tree_of(&cols)).run(check_two_span_column);
}

#[test]
fn a_two_span_column_equals_two_stacked_single_span_rooms() {
    let cols = two_span_cols();
    cols.stacked().run(|w| {
        // The stacked form is the specification of the layered one: check
        // the slab part of the hand checks on it too.
        assert!(w.body_blocked([120, 300, 120], 12, 48));
        assert!(!w.body_blocked([120, 320, 120], 12, 48));
    });
}

#[test]
fn a_slab_of_twenty_units_is_solid_from_the_side_at_every_height() {
    let mut cols = Cols::new(3, 3, flat_span(Some(0), None, false));
    *cols.column(1, 1) = vec![
        flat_span(Some(0), Some(240), true),
        flat_span(Some(260), Some(500), true),
    ];
    for built in [cols.layered(true), tree_built(&tree_of(&cols))] {
        built.run(|w| {
            for y in 241..260 {
                let t = trace(w, point([20, y, 120], [200, y, 120]));
                assert!(t.fraction_q12 < 4096, "y {y}: {t:?}");
            }
            // And the span above it is open.
            let t = trace(w, point([20, 270, 120], [200, 270, 120]));
            assert_eq!(t.fraction_q12, 4096);
        });
    }
}

/// A vault: ground span 0 to 240 under a solid ceiling everywhere, so the
/// summary pass marks the interior cells clear, and one column with a second
/// span above (a shaft through the roof).
fn vault_cols() -> Cols {
    let mut cols = Cols::new(5, 5, flat_span(Some(0), Some(240), true));
    cols.column(2, 2)
        .push(flat_span(Some(320), Some(560), true));
    cols
}

#[test]
fn a_clear_neighbourhood_does_not_hide_an_extra_span() {
    let cols = vault_cols();
    // The pass that knows nothing about spans marks the neighbours and the
    // column itself clear: the reader must not take that on trust.
    let chunk = cols.layered_chunk(true);
    for (cx, cz) in [(1, 2), (2, 2), (3, 2)] {
        assert_ne!(
            chunk.sectors[cz * 5 + cx].flags & SECTOR_CLEAR,
            0,
            "{cx} {cz}"
        );
    }
    cols.layered(true).run(|w| {
        // In cell (1, 2), whose own slabs are far below, a body whose +x
        // sample reaches into the column (x 150 + 12 > 160) is inside the
        // column's second-floor slab (feet 256 to 320).
        assert!(w.body_blocked([150, 290, 200], 12, 48));
        assert!(!w.body_blocked([140, 290, 200], 12, 48));
        // A body above the roof of the vault walks over the shaft's floor.
        let t = trace(w, body([150, 400, 200], [150, 0, 200]));
        assert_eq!(t.end.y, 320, "{t:?}");
        // The vault itself is unchanged.
        assert!(!w.body_blocked([60, 100, 60], 12, 48));
        assert!(w.body_blocked([60, 200, 60], 12, 48));
    });
}

/// Brute force: the slab intervals of every span of every column the body's
/// samples touch.
fn brute_blocked(cols: &Cols, p: [i32; 3], r: i32, h: i32) -> bool {
    let t = SLAB_THICKNESS;
    let offsets: &[(i32, i32)] = if r > 0 {
        &[(0, 0), (r, 0), (-r, 0), (0, r), (0, -r)]
    } else {
        &[(0, 0)]
    };
    offsets.iter().any(|&(dx, dz)| {
        let Some(column) = cols.at(p[0] + dx, p[2] + dz) else {
            return false;
        };
        column.iter().any(|span| {
            let f = i32::from(span.floor[0]);
            let c = i32::from(span.ceil[0]);
            (span.flags & SECTOR_HAS_FLOOR != 0 && p[1] > f - t - h && p[1] < f)
                || (span.flags & SECTOR_HAS_CEIL != 0
                    && span.flags & SECTOR_CEIL_SOLID != 0
                    && p[1] > c - h
                    && p[1] < c + t)
        })
    })
}

/// Brute force `floor_below` and `ceil_above` of a column.
fn brute_heights(column: &[Sector], y: i32) -> (Option<i32>, Option<i32>) {
    let floor = column
        .iter()
        .filter(|s| s.flags & SECTOR_HAS_FLOOR != 0)
        .map(|s| i32::from(s.floor[0]))
        .filter(|&f| f <= y)
        .max();
    let ceil = column
        .iter()
        .filter(|s| s.flags & SECTOR_HAS_CEIL != 0)
        .map(|s| i32::from(s.ceil[0]))
        .filter(|&c| c >= y)
        .min();
    (floor, ceil)
}

/// Compare the world against the brute force over a lattice of points and
/// heights; `column_at` gives the spans at a room-local point.
fn check_against_brute_force(
    w: &GridCollision<'_>,
    cols: &Cols,
    column_at: &dyn Fn(i32, i32) -> Option<Vec<Sector>>,
) {
    let (mut blocked, mut free) = (0, 0);
    for z in (-20..i32::from(cols.d) * S + 20).step_by(7) {
        for x in (-20..i32::from(cols.w) * S + 20).step_by(7) {
            for y in (-60..620).step_by(9) {
                for (r, h) in [(0, 0), (0, 30), (6, 30), (12, 48)] {
                    let want = brute_blocked(cols, [x, y, z], r, h);
                    let got = if r == 0 && h == 0 {
                        w.point_in_solid([x, y, z])
                    } else {
                        w.body_blocked([x, y, z], r, h)
                    };
                    assert_eq!(got, want, "p {:?} r {r} h {h}", [x, y, z]);
                    if want {
                        blocked += 1;
                    } else {
                        free += 1;
                    }
                }
            }
            if let Some(column) = column_at(x, z) {
                let (cx, cz) = ((x / S) as usize, (z / S) as usize);
                let (lx, lz) = ((x % S) as u32, (z % S) as u32);
                for y in (-60..620).step_by(31) {
                    let (floor, ceil) = brute_heights(&column, y);
                    assert_eq!(
                        w.floor_below(0, (cx, cz), (lx, lz), y),
                        floor,
                        "{x} {z} {y}"
                    );
                    assert_eq!(w.ceil_above(0, (cx, cz), (lx, lz), y), ceil, "{x} {z} {y}");
                }
            }
        }
    }
    assert!(blocked > 2000 && free > 2000, "{blocked} {free}");
}

#[test]
fn dense_columns_match_a_brute_force_scan_of_every_span() {
    for seed in [1u32, 2, 3] {
        let cols = random_cols(seed, 6, 5, false);
        assert!(cols.cells.iter().filter(|c| c.len() > 1).count() > 5);
        for summary in [true, false] {
            cols.layered(summary).run(|w| {
                check_against_brute_force(w, &cols, &|x, z| cols.at(x, z).map(<[_]>::to_vec));
            });
        }
    }
}

/// A random tree of leaves of every size, each leaf holding a random flat
/// column. `cols` is the brute force model at click resolution.
fn random_tree_with_columns(seed: u32, sloped: bool) -> TreeRoom {
    let mut room = random_room(seed, 7, 6, ((seed % 4) as i32, ((seed / 2) % 4) as i32));
    let mut rng = Lcg(seed ^ 0x5eed);
    for leaf in &mut room.leaves {
        let column = random_column(&mut rng, sloped);
        leaf.sector = column[0];
        leaf.extras = column[1..].to_vec();
    }
    room
}

#[test]
fn tree_columns_match_a_brute_force_scan_of_every_span() {
    for seed in [11u32, 12, 13] {
        let room = random_tree_with_columns(seed, false);
        assert!(room.leaves.iter().filter(|l| !l.extras.is_empty()).count() > 10);
        assert!(room.leaves.iter().any(|l| l.code >= leaf_code::HALF));
        // The model looks the leaf up by click, whatever its size.
        let (w, d) = (room.w, room.d);
        let model = |x: i32, z: i32| -> Option<Vec<Sector>> {
            if x < 0 || z < 0 || x >= i32::from(w) * S || z >= i32::from(d) * S {
                return None;
            }
            room.leaf_at(x.div_euclid(Q), z.div_euclid(Q)).map(|l| {
                let mut all = vec![l.sector];
                all.extend(l.extras.iter().copied());
                all
            })
        };
        tree_built(&room).run(|world| {
            let (mut blocked, mut free) = (0, 0);
            for z in (-20..i32::from(d) * S + 20).step_by(9) {
                for x in (-20..i32::from(w) * S + 20).step_by(9) {
                    for y in (-60..620).step_by(13) {
                        for (r, h) in [(0, 0), (0, 30), (12, 48)] {
                            let want = brute_blocked_with(&model, [x, y, z], r, h);
                            let got = if r == 0 && h == 0 {
                                world.point_in_solid([x, y, z])
                            } else {
                                world.body_blocked([x, y, z], r, h)
                            };
                            assert_eq!(got, want, "p {:?} r {r} h {h}", [x, y, z]);
                            if want {
                                blocked += 1;
                            } else {
                                free += 1;
                            }
                        }
                    }
                    if let Some(column) = model(x, z) {
                        let (cx, cz) = ((x / S) as usize, (z / S) as usize);
                        let (lx, lz) = ((x % S) as u32, (z % S) as u32);
                        for y in (-60..620).step_by(53) {
                            let (floor, ceil) = brute_heights(&column, y);
                            assert_eq!(
                                world.floor_below(0, (cx, cz), (lx, lz), y),
                                floor,
                                "{x} {z} {y}"
                            );
                            assert_eq!(
                                world.ceil_above(0, (cx, cz), (lx, lz), y),
                                ceil,
                                "{x} {z} {y}"
                            );
                        }
                    }
                }
            }
            assert!(blocked > 1000 && free > 1000, "{blocked} {free}");
        });
    }
}

/// [`brute_blocked`] over any plan lookup.
fn brute_blocked_with(
    column_at: &dyn Fn(i32, i32) -> Option<Vec<Sector>>,
    p: [i32; 3],
    r: i32,
    h: i32,
) -> bool {
    let t = SLAB_THICKNESS;
    let offsets: &[(i32, i32)] = if r > 0 {
        &[(0, 0), (r, 0), (-r, 0), (0, r), (0, -r)]
    } else {
        &[(0, 0)]
    };
    offsets.iter().any(|&(dx, dz)| {
        let Some(column) = column_at(p[0] + dx, p[2] + dz) else {
            return false;
        };
        column.iter().any(|span| {
            let f = i32::from(span.floor[0]);
            let c = i32::from(span.ceil[0]);
            (span.flags & SECTOR_HAS_FLOOR != 0 && p[1] > f - t - h && p[1] < f)
                || (span.flags & SECTOR_HAS_CEIL != 0
                    && span.flags & SECTOR_CEIL_SOLID != 0
                    && p[1] > c - h
                    && p[1] < c + t)
        })
    })
}

/// Two traces that agree up to the rounding of a sloped slab crossing: the
/// same start flags, fractions within 12 of 4096 and ends within one unit,
/// and the same normal unless the two stopped at the very same place (a
/// body touching two surfaces at once names either).
fn near(a: &CollisionTrace, b: &CollisionTrace) -> bool {
    let end = a
        .end
        .to_array()
        .iter()
        .zip(b.end.to_array())
        .all(|(x, y)| (x - y).abs() <= 1);
    a.start_solid == b.start_solid
        && a.all_solid == b.all_solid
        && (a.fraction_q12 - b.fraction_q12).abs() <= 12
        && end
        && (a.normal_q12 == b.normal_q12 || (a.fraction_q12 == b.fraction_q12 && a.end == b.end))
}

/// Random queries on both worlds: every answer must be the same (strict),
/// or the same up to [`near`] for traces over sloped slabs.
fn assert_same_answers(
    layered: &Built,
    stacked: &Built,
    seed: u32,
    extent: (i32, i32),
    strict: bool,
) {
    layered.run(|a| {
        stacked.run(|b| {
            let mut rng = Lcg(seed);
            let mut hits = 0;
            let sampled = |rng: &mut Lcg| {
                [
                    rng.range(-30, extent.0 + 30),
                    rng.range(-60, 620),
                    rng.range(-30, extent.1 + 30),
                ]
            };
            for i in 0..3000 {
                let p = sampled(&mut rng);
                let (r, h) = [(0, 0), (0, 30), (6, 30), (12, 48)][i % 4];
                if r == 0 && h == 0 {
                    assert_eq!(a.point_in_solid(p), b.point_in_solid(p), "{p:?}");
                } else {
                    assert_eq!(
                        a.body_blocked(p, r, h),
                        b.body_blocked(p, r, h),
                        "{p:?} r {r} h {h}"
                    );
                }
                let end = match i % 5 {
                    0 => sampled(&mut rng),
                    1 => [p[0], rng.range(-200, 900), p[2]],
                    2 => [
                        rng.range(-30, extent.0 + 30),
                        p[1],
                        rng.range(-30, extent.1 + 30),
                    ],
                    3 => [
                        p[0] + rng.range(-25, 25),
                        p[1] + rng.range(-25, 25),
                        p[2] + rng.range(-25, 25),
                    ],
                    _ => [
                        p[0] + rng.range(-200, 200),
                        p[1],
                        p[2] + rng.range(-200, 200),
                    ],
                };
                assert_eq!(
                    a.line_of_sight(p, end),
                    b.line_of_sight(p, end),
                    "{p:?} {end:?}"
                );
                let query = if r == 0 && h == 0 {
                    point(p, end)
                } else {
                    CollisionTraceQuery::body(
                        RoomPoint::from_array(p),
                        RoomPoint::from_array(end),
                        r,
                        h,
                    )
                };
                let (ta, tb) = (trace(a, query), trace(b, query));
                if strict {
                    assert_eq!(ta, tb, "{query:?}");
                } else {
                    assert!(near(&ta, &tb), "{query:?}\n  {ta:?}\n  {tb:?}");
                    for (world, t) in [(a, &ta), (b, &tb)] {
                        if !t.start_solid {
                            let end = t.end.to_array();
                            let inside = if r == 0 && h == 0 {
                                world.point_in_solid(end)
                            } else {
                                world.body_blocked(end, r, h)
                            };
                            assert!(!inside, "ends inside solid: {query:?} {t:?}");
                        }
                    }
                }
                if ta.fraction_q12 < 4096 {
                    hits += 1;
                }
            }
            assert!(hits > 600, "the world must produce hits: {hits}");
        });
    });
}

#[test]
fn layered_dense_rooms_answer_like_stacked_rooms() {
    for (seed, sloped) in [(21u32, false), (22, true), (23, true)] {
        let cols = random_cols(seed, 6, 5, sloped);
        let stacked = cols.stacked();
        for summary in [true, false] {
            assert_same_answers(
                &cols.layered(summary),
                &stacked,
                seed,
                (6 * S, 5 * S),
                !sloped,
            );
        }
    }
}

#[test]
fn layered_tree_rooms_answer_like_stacked_tree_rooms() {
    for (seed, sloped) in [(31u32, false), (32, true), (33, true), (34, true)] {
        let room = random_tree_with_columns(seed, sloped);
        let levels = room
            .leaves
            .iter()
            .map(|l| 1 + l.extras.len())
            .max()
            .unwrap();
        assert!(levels >= 3);
        let stacked: Vec<TreeRoom> = (0..levels)
            .map(|k| {
                let mut layer = TreeRoom::new(room.origin, room.w, room.d, room.om);
                for leaf in &room.leaves {
                    let mut copy = leaf.clone();
                    copy.sector = if k == 0 {
                        leaf.sector
                    } else {
                        leaf.extras.get(k - 1).copied().unwrap_or(HOLE)
                    };
                    copy.extras.clear();
                    layer.leaves.push(copy);
                }
                layer
            })
            .collect();
        assert_same_answers(
            &tree_built(&room),
            &tree_world(&stacked),
            seed,
            (7 * S, 6 * S),
            !sloped,
        );
    }
}

#[test]
fn locate_finds_a_point_inside_any_span_of_its_column() {
    // Two rooms over one footprint: a column with a second span, and a room
    // whose floor is closer to a point inside that second span than the
    // column's ground ceiling is.
    let cols = two_span_cols();
    let mut upper = Room::flat([0, 0, 0], 3, 3, 400);
    upper.ceiling(0, 0, 500, true);
    let chunks = vec![
        (
            Room::flat([0, 0, 0], 3, 3, 0),
            cols.layered_chunk(true),
            false,
        ),
        (upper.clone(), upper.chunk(true), false),
    ];
    build_chunks(&chunks).run(|w| {
        for hint in [0, 1] {
            // Inside the second span of room 0 and below room 1's floor:
            // room 0's gap is zero, room 1's is 60. (Measured to the ground
            // span alone, room 0 would be 100 away and lose.)
            let at = w.locate(hint, [120, 340, 120]).unwrap();
            assert_eq!(at.room, 0, "hint {hint}");
        }
        // Inside both: the hint room wins, as for any pair of rooms.
        assert_eq!(w.locate(0, [120, 420, 120]).unwrap().room, 0);
        assert_eq!(w.locate(1, [120, 420, 120]).unwrap().room, 1);
    });
}
