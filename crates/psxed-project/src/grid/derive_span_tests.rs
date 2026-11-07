//! Derived walls across columns of several spans (`derive_spans`): the slab
//! edge, closures against a missing neighbour, agreement with the single-span
//! rules, and the open-set tiling property on random columns.

use super::derive::{physical_edge_walls, DerivedKind, DerivedWall};
use super::room::GridRoom;
use super::sector::{GridSector, GridSpan, WallEdge};
use super::surface::GridSurface;
use super::units::SECTOR_UNITS;
use crate::ResourceId;

fn rid(n: u64) -> ResourceId {
    ron::from_str(&format!("({n})")).expect("a resource id")
}

/// A tiny deterministic generator: the tests must not depend on a seed of
/// the environment.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// An integer in `lo..=hi`.
    fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + (self.next() % (hi - lo + 1) as u64) as i32
    }

    fn chance(&mut self, one_in: u64) -> bool {
        self.next().is_multiple_of(one_in)
    }
}

fn flat(height: i32) -> GridSurface {
    GridSurface::flat(height, Some(rid(7)))
}

fn span(floor: Option<GridSurface>, ceiling: Option<GridSurface>) -> GridSpan {
    GridSpan {
        floor,
        ceiling,
        wall_material: Some(rid(9)),
    }
}

/// A column from its spans, bottom to top.
fn column(spans: Vec<GridSpan>) -> GridSector {
    let mut sector = GridSector::default();
    let mut spans = spans.into_iter();
    *sector.spans_mut().first_mut().unwrap() = spans.next().unwrap();
    for (index, s) in spans.enumerate() {
        sector.insert_span(index + 1, s);
    }
    sector
}

/// Two side-by-side cells: `(0, 0)` and `(1, 0)`.
fn pair(left: GridSector, right: Option<GridSector>) -> GridRoom {
    let mut room = GridRoom::new_derived("pair", 2, 1);
    *room.sector_or_insert(0, 0).unwrap() = left;
    if let Some(right) = right {
        *room.sector_or_insert(1, 0).unwrap() = right;
    }
    room
}

fn walls(room: &GridRoom) -> Vec<DerivedWall> {
    physical_edge_walls(room, [0, 0], WallEdge::East)
}

/// `u` back in the canonical direction of the physical edge.
fn canonical(wall: &DerivedWall) -> Vec<[i32; 2]> {
    let backward = matches!(wall.edge, WallEdge::South | WallEdge::West);
    wall.ring
        .iter()
        .map(|p| [if backward { SECTOR_UNITS - p[0] } else { p[0] }, p[1]])
        .collect()
}

#[test]
fn a_slab_seen_from_a_taller_neighbour_is_one_slab_face_owned_above() {
    // The tall hall: one span from 0 to 3072. The column beside it has a
    // slab from 768 to 1024 and a storey above from 1024 to 3072.
    let hall = column(vec![span(Some(flat(0)), Some(flat(3072)))]);
    let neighbour = column(vec![
        span(Some(flat(0)), Some(flat(768))),
        span(Some(flat(1024)), Some(flat(3072))),
    ]);
    let found = walls(&pair(hall, Some(neighbour)));
    let slabs: Vec<&DerivedWall> = found
        .iter()
        .filter(|w| w.kind == DerivedKind::Slab)
        .collect();
    assert_eq!(slabs.len(), 1, "{found:#?}");
    let slab = slabs[0];
    // Seen from the hall, owned by the neighbour's storey above the slab.
    assert_eq!(slab.host, [0, 0]);
    assert_eq!(slab.owner, [1, 0]);
    assert_eq!(slab.heights, Some([768, 768, 1024, 1024]));
    assert_eq!(slab.material, Some(rid(9)));
    // Nothing else on that edge: the storey's walls do not double the slab.
    assert_eq!(found.len(), 1, "{found:#?}");
}

#[test]
fn each_span_of_a_column_with_no_neighbour_closes_itself() {
    let lone = column(vec![
        span(Some(flat(0)), Some(flat(512))),
        span(Some(flat(1024)), Some(flat(2048))),
    ]);
    let found = walls(&pair(lone, None));
    let closures: Vec<[i32; 2]> = found
        .iter()
        .filter(|w| w.kind == DerivedKind::Closure)
        .map(|w| w.heights.map(|h| [h[0], h[3]]).unwrap_or([0, 0]))
        .collect();
    assert_eq!(closures, vec![[0, 512], [1024, 2048]], "{found:#?}");
    assert!(found.iter().all(|w| w.host == [0, 0] && w.owner == [0, 0]));
}

/// Whether the span of `sector` holds the height `y` at `u` (canonical).
fn open_at(sector: Option<&GridSector>, edge: WallEdge, u: i32, y: i32) -> Option<bool> {
    let sector = sector?;
    let forward = matches!(edge, WallEdge::North | WallEdge::East);
    let at = |surface: &GridSurface| -> i32 {
        let [a, b] = super::edit::walls::edge_corner_indices(edge);
        let heights = surface.heights;
        let (h0, h1) = if forward {
            (heights[a], heights[b])
        } else {
            (heights[b], heights[a])
        };
        h0 + ((h1 - h0) as i64 * i64::from(u) / i64::from(SECTOR_UNITS)) as i32
    };
    let mut inside = false;
    for s in sector.spans() {
        let low = s.floor.as_ref().map(at);
        let high = s.ceiling.as_ref().map(at);
        if low.is_none_or(|l| l < y) && high.is_none_or(|h| y < h) {
            inside = true;
        }
    }
    Some(inside)
}

/// Whether `p` lies strictly inside the convex ring `ring`.
fn inside(ring: &[[i32; 2]], p: [i32; 2]) -> bool {
    let n = ring.len();
    let mut signs = (false, false);
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        let cross = i64::from(b[0] - a[0]) * i64::from(p[1] - a[1])
            - i64::from(b[1] - a[1]) * i64::from(p[0] - a[0]);
        if cross > 0 {
            signs.0 = true;
        } else if cross < 0 {
            signs.1 = true;
        } else {
            return false;
        }
    }
    !(signs.0 && signs.1)
}

/// The height at `u` of every bound of both columns, to skip samples that
/// sit on a boundary.
fn near_a_bound(sectors: [Option<&GridSector>; 2], edge: [WallEdge; 2], u: i32, y: i32) -> bool {
    for (sector, edge) in sectors.iter().zip(edge) {
        let Some(sector) = sector else { continue };
        for s in sector.spans() {
            for surface in s.floor.iter().chain(s.ceiling.iter()) {
                let [a, b] = super::edit::walls::edge_corner_indices(edge);
                let forward = matches!(edge, WallEdge::North | WallEdge::East);
                let (h0, h1) = if forward {
                    (surface.heights[a], surface.heights[b])
                } else {
                    (surface.heights[b], surface.heights[a])
                };
                let value = h0 + ((h1 - h0) as i64 * i64::from(u) / i64::from(SECTOR_UNITS)) as i32;
                if (value - y).abs() <= 2 {
                    return true;
                }
            }
        }
    }
    false
}

/// A random column of up to `max` spans, sorted and disjoint, with sloped
/// corners sometimes, a pit below sometimes and open sky above sometimes.
fn random_column(rng: &mut Rng, max: u64) -> GridSector {
    let count = 1 + rng.next() % max;
    let mut spans = Vec::new();
    let mut top = rng.range(0, 1) * 64;
    for index in 0..count {
        let gap = rng.range(2, 16) * 64;
        let floor_base = top + gap;
        let height = rng.range(4, 32) * 64;
        let sloped = |rng: &mut Rng, base: i32| {
            let mut s = flat(base);
            if rng.chance(3) {
                s.heights = [
                    base,
                    base + rng.range(-1, 1) * 64,
                    base + rng.range(-1, 1) * 64,
                    base,
                ];
            }
            s
        };
        let floor = if index == 0 && rng.chance(5) {
            None
        } else {
            Some(sloped(rng, floor_base))
        };
        let ceiling_base = floor_base + height;
        let ceiling = if index + 1 == count && rng.chance(5) {
            None
        } else {
            Some(sloped(rng, ceiling_base))
        };
        top = ceiling_base;
        spans.push(span(floor, ceiling));
    }
    column(spans)
}

#[test]
fn the_derived_faces_tile_the_open_set_boundary_exactly() {
    // Along each column of samples the boundary (open on one side, solid on
    // the other) splits into runs. A run that reaches the first or last
    // sample runs off to a pit or open sky, which makes no face (the pit edge
    // stays open, `derive`). Every other run is covered exactly once.
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut checked = 0;
    for _ in 0..150 {
        let left = random_column(&mut rng, 3);
        let right = random_column(&mut rng, 3);
        // Two single-span columns keep the old rules (`derive`), which extend a
        // riser above the lower ceiling on purpose; this property is for edges
        // that take the open-set path.
        if left.is_single_span() && right.is_single_span() {
            continue;
        }
        let room = pair(left.clone(), Some(right.clone()));
        let found = walls(&room);
        let edges = [WallEdge::East, WallEdge::West];
        // The window covers every finite bound of both columns with a margin,
        // so a run that reaches its ends runs off to a pit or open sky.
        let (mut lo, mut hi) = (i32::MAX, i32::MIN);
        for column in [&left, &right] {
            for h in column.surfaces().flat_map(|s| s.heights) {
                lo = lo.min(h);
                hi = hi.max(h);
            }
        }
        let ys: Vec<i32> = (lo - 256..hi + 256).step_by(32).map(|y| y + 13).collect();
        for u in (32..SECTOR_UNITS).step_by(64) {
            // (sample, boundary?) with the samples on a bound left out.
            let mut samples: Vec<Option<bool>> = Vec::new();
            for &y in &ys {
                if near_a_bound([Some(&left), Some(&right)], edges, u, y) {
                    samples.push(None);
                    continue;
                }
                let a = open_at(Some(&left), WallEdge::East, u, y).unwrap();
                let b = open_at(Some(&right), WallEdge::West, u, y).unwrap();
                samples.push(Some(a != b));
            }
            let last = ys.len() - 1;
            let mut run_start: Option<usize> = None;
            for i in 0..=ys.len() {
                let on = samples.get(i) == Some(&Some(true));
                match (on, run_start) {
                    (true, None) => run_start = Some(i),
                    (false, Some(start)) => {
                        let bounded = start > 0 && i - 1 < last;
                        for &y in ys.iter().take(i).skip(start) {
                            let covered = found
                                .iter()
                                .filter(|w| inside(&canonical(w), [u, y]))
                                .count();
                            assert_eq!(
                                covered,
                                usize::from(bounded),
                                "u {u} y {y}, run {start}..{i}"
                            );
                            checked += 1;
                        }
                        run_start = None;
                    }
                    _ => {}
                }
            }
            // A sample off the boundary is never covered.
            for (i, sample) in samples.iter().enumerate() {
                if *sample == Some(false) {
                    let covered = found.iter().any(|w| inside(&canonical(w), [u, ys[i]]));
                    assert!(
                        !covered,
                        "u {u} y {}: covered off the boundary {:?} left {:?} right {:?}",
                        ys[i],
                        found
                            .iter()
                            .filter(|w| inside(&canonical(w), [u, ys[i]]))
                            .map(|w| (w.kind, w.host, w.owner, canonical(w)))
                            .collect::<Vec<_>>(),
                        left.spans()
                            .iter()
                            .map(|s| (
                                s.floor.as_ref().map(|f| f.heights),
                                s.ceiling.as_ref().map(|c| c.heights)
                            ))
                            .collect::<Vec<_>>(),
                        right
                            .spans()
                            .iter()
                            .map(|s| (
                                s.floor.as_ref().map(|f| f.heights),
                                s.ceiling.as_ref().map(|c| c.heights)
                            ))
                            .collect::<Vec<_>>()
                    );
                }
            }
        }
    }
    assert!(checked > 5_000, "only {checked} samples");
}

#[test]
fn single_span_edges_cover_the_open_set_boundary_and_no_more_visible_face() {
    // The single-span rules (`derive`) keep a riser's strip up to the upper
    // floor even above the lower column's ceiling, a face inside solid that
    // nobody sees. The open-set rule leaves that out. So on a single-span
    // edge: every open-set boundary sample is covered by the old faces, and
    // every sample only the old faces cover is solid on both sides.
    use super::derive_spans::edge_walls as by_open_set;
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    for _ in 0..200 {
        let left = column(vec![random_single(&mut rng)]);
        let right = column(vec![random_single(&mut rng)]);
        let room = pair(left.clone(), Some(right.clone()));
        let old = walls(&room);
        let new = by_open_set(&room, [0, 0], WallEdge::East, [1, 0], WallEdge::West);
        for u in (32..SECTOR_UNITS).step_by(64) {
            for y in (-256..4096).step_by(32).map(|y| y + 13) {
                if near_a_bound(
                    [Some(&left), Some(&right)],
                    [WallEdge::East, WallEdge::West],
                    u,
                    y,
                ) {
                    continue;
                }
                let a = open_at(Some(&left), WallEdge::East, u, y).unwrap();
                let b = open_at(Some(&right), WallEdge::West, u, y).unwrap();
                let in_new = new.iter().any(|w| inside(&canonical(w), [u, y]));
                let in_old = old.iter().any(|w| inside(&canonical(w), [u, y]));
                assert_eq!(in_new, a != b, "open-set at u {u} y {y}");
                if in_old && !in_new {
                    assert!(a == b && !a, "visible boundary lost at u {u} y {y}");
                }
            }
        }
    }
}

/// A single span with a floor and a ceiling, flat or with one tilted corner,
/// heights on the 64 lattice.
fn random_single(rng: &mut Rng) -> GridSpan {
    let floor = rng.range(0, 8) * 64;
    let ceiling = floor + rng.range(4, 32) * 64;
    span(Some(flat(floor)), Some(flat(ceiling)))
}
