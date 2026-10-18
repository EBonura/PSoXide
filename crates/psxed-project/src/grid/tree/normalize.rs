//! Restoring the quadtree invariants after an edit.
//!
//! [`normalize`] is the one function every tree edit ends with:
//!
//! 1. A leaf above a sector (level 0 or 1) is one plane with no walls on it
//!    ([`big_leaf_ok`]); one that stopped being is subdivided.
//! 2. Neighbouring leaves differ by at most one level ([`balance`]); a leaf
//!    two levels coarser than a neighbour is subdivided. Both steps only add
//!    leaves, and a subdivision never changes the surface, so they end.
//! 3. A **hanging corner** M, where a coarse leaf C meets two finer leaves,
//!    is **constrained** when the surface is continuous along C's edge: both
//!    finer leaves' far corners equal C's two ends, and M then takes the
//!    average of C's two end heights ([`constrain`]). Where an end differs
//!    (a cliff) M is free, and the wall between the two is derived per
//!    half.
//!
//! It does not merge: a subdivision the author asked for is not undone by
//! the next edit. Merging is its own command ([`super::ops::merge`]).

use std::collections::BTreeSet;

use super::super::auto_split;
use super::super::leaf::{LeafKey, FINEST_LEVEL, SECTOR_LEVEL};
use super::super::room::GridRoom;
use super::super::sector::{GridSector, WallEdge};
use super::super::surface::GridSurface;
use super::neighbours::{across, facing, SIDES};
use super::ops::subdivide_raw;
use super::surface_split::half;

/// Whether `sector` may be a leaf above a sector: every surface one plane
/// with no split level or void, and no wall on any edge.
pub fn big_leaf_ok(sector: &GridSector) -> bool {
    sector
        .surfaces()
        .all(|s| s.heights_b.is_none() && s.void == 0 && s.is_planar())
        && WallEdge::ALL.iter().all(|e| sector.walls(*e).is_empty())
}

/// What a normalise pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NormalizeReport {
    /// Leaves subdivided to keep big leaves planar and the tree balanced.
    pub subdivided: usize,
    /// Hanging corners rewritten to their constrained height.
    pub corners: usize,
}

/// Restore every invariant of the room's quadtree. Idempotent.
pub fn normalize(room: &mut GridRoom) -> NormalizeReport {
    let subdivided = balance(room);
    let corners = constrain(room);
    NormalizeReport {
        subdivided,
        corners,
    }
}

/// Subdivide leaves until big leaves are planar and neighbours differ by at
/// most one level. Returns how many leaves were subdivided.
pub fn balance(room: &mut GridRoom) -> usize {
    let mut count = 0;
    loop {
        let mut split: BTreeSet<LeafKey> = BTreeSet::new();
        for (key, sector) in room.leaves() {
            if key.level < SECTOR_LEVEL && !big_leaf_ok(sector) {
                split.insert(key);
            }
            for side in SIDES {
                for n in across(&room.leaves, key, side) {
                    if n.level + 1 < key.level {
                        split.insert(n);
                    }
                }
            }
        }
        let mut progress = false;
        for key in split {
            if room.leaf(key).is_some() && subdivide_raw(room, key).is_ok() {
                progress = true;
                count += 1;
            }
        }
        if !progress {
            return count;
        }
    }
}

/// Corner numbers of a surface along `side`, in increasing order along it
/// (west to east for north and south, north to south for east and west).
pub const fn side_corners(side: WallEdge) -> [usize; 2] {
    match side {
        WallEdge::North => [0, 1],
        WallEdge::South => [3, 2],
        WallEdge::West => [0, 3],
        _ => [1, 2],
    }
}

/// The triangle of `surface` that holds both corners of `side`.
fn side_triangle(surface: &GridSurface, side: WallEdge) -> usize {
    let [a, b] = side_corners(side);
    (0..2)
        .find(|t| {
            let corners = surface.split.triangle(*t);
            corners.contains(&a) && corners.contains(&b)
        })
        .unwrap_or(0)
}

/// Height of corner `index` as the triangle on `side` sees it.
pub fn side_height(surface: &GridSurface, side: WallEdge, index: usize) -> i32 {
    surface.heights_for(side_triangle(surface, side))[index]
}

fn set_side_height(surface: &mut GridSurface, side: WallEdge, index: usize, value: i32) {
    if side_triangle(surface, side) == 1 && surface.heights_b.is_some() {
        if let Some(b) = surface.heights_b.as_mut() {
            b[index] = value;
        }
    } else {
        surface.heights[index] = value;
    }
}

/// A fine corner that disagrees with its constraint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HangingCorner {
    /// The finer leaf holding the corner.
    pub fine: LeafKey,
    /// The coarser leaf whose edge the corner lies on.
    pub coarse: LeafKey,
}

/// What `constrain` would do, or does when `apply`.
fn constrain_pass(room: &mut GridRoom, apply: bool) -> Vec<HangingCorner> {
    let mut found = Vec::new();
    for level in 0..FINEST_LEVEL {
        let keys: Vec<LeafKey> = room
            .leaves()
            .map(|(k, _)| k)
            .filter(|k| k.level == level)
            .collect();
        for coarse in keys {
            for side in SIDES {
                let ns = across(&room.leaves, coarse, side);
                if ns.len() != 2 || ns.iter().any(|n| n.level != level + 1) {
                    continue;
                }
                let spans = ns
                    .iter()
                    .filter_map(|n| room.leaf(*n).map(GridSector::span_count))
                    .chain(room.leaf(coarse).map(GridSector::span_count))
                    .min()
                    .unwrap_or(0);
                for span in 0..spans {
                    for ceiling in [false, true] {
                        if let Some(corner) =
                            constrain_edge(room, coarse, side, [ns[0], ns[1]], span, ceiling, apply)
                        {
                            found.extend(corner);
                        }
                    }
                }
            }
        }
    }
    found
}

type SurfaceOf = fn(&mut GridSector, usize) -> Option<&mut GridSurface>;

fn floor_of(s: &mut GridSector, span: usize) -> Option<&mut GridSurface> {
    s.spans_mut().get_mut(span)?.floor.as_mut()
}

fn ceiling_of(s: &mut GridSector, span: usize) -> Option<&mut GridSurface> {
    s.spans_mut().get_mut(span)?.ceiling.as_mut()
}

/// Constrain the corner(s) of the two finer leaves on one side of `coarse`
/// for one surface. Returns the corners that disagreed.
fn constrain_edge(
    room: &mut GridRoom,
    coarse: LeafKey,
    side: WallEdge,
    fine: [LeafKey; 2],
    span: usize,
    ceiling: bool,
    apply: bool,
) -> Option<Vec<HangingCorner>> {
    let pick: SurfaceOf = if ceiling { ceiling_of } else { floor_of };
    let [c0, c1] = side_corners(side);
    let (a, b) = {
        let s = pick(room.leaf_mut(coarse)?, span)?;
        (side_height(s, side, c0), side_height(s, side, c1))
    };
    let mid = half(a, b);
    let fside = facing(side);
    let [f0, f1] = side_corners(fside);
    // The first fine leaf starts where the coarse side starts, the second
    // ends where it ends; the corners between them hang. The edge is
    // continuous only when both far corners are welded to the coarse ends.
    let mut hung = Vec::new();
    let mut welded = true;
    for (i, leaf) in fine.into_iter().enumerate() {
        let Some(s) = pick(room.leaf_mut(leaf)?, span) else {
            welded = false;
            continue;
        };
        let (anchor_index, hang_index, anchor_value) =
            if i == 0 { (f0, f1, a) } else { (f1, f0, b) };
        welded &= side_height(s, fside, anchor_index) == anchor_value;
        hung.push((leaf, hang_index));
    }
    let mut out = Vec::new();
    if welded {
        for (leaf, hang_index) in hung {
            let Some(s) = pick(room.leaf_mut(leaf)?, span) else {
                continue;
            };
            if side_height(s, fside, hang_index) != mid {
                out.push(HangingCorner { fine: leaf, coarse });
                if apply {
                    set_side_height(s, fside, hang_index, mid);
                    auto_split::resplit(s);
                }
            }
        }
    }
    Some(out)
}

/// Rewrite the constrained hanging corners. Returns how many changed.
pub fn constrain(room: &mut GridRoom) -> usize {
    constrain_pass(room, true).len()
}

/// The hanging corners that disagree with their constraint, without
/// changing anything.
pub fn hanging_mismatches(room: &GridRoom) -> Vec<HangingCorner> {
    let mut copy = room.clone();
    constrain_pass(&mut copy, false)
}

/// What is wrong with a room's quadtree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeIssue {
    /// Two neighbours are more than one level apart (`a` is the finer).
    Unbalanced {
        /// The finer leaf.
        a: LeafKey,
        /// The coarser leaf.
        b: LeafKey,
    },
    /// A leaf above a sector is not one plane without walls.
    BigLeafNotPlanar(LeafKey),
    /// A leaf below a sector has a void, a split level or a diagonal wall,
    /// which the leaf derivation does not carry.
    DiagonalBelowSector(LeafKey),
    /// A constrained hanging corner is not at the average of its edge.
    HangingCorner(HangingCorner),
}

/// Every invariant violation of the room's tree. Alignment and disjointness
/// are structural (the map refuses misaligned or overlapping keys).
pub fn check(room: &GridRoom) -> Vec<TreeIssue> {
    let mut issues = Vec::new();
    for (key, sector) in room.leaves() {
        if key.level < SECTOR_LEVEL && !big_leaf_ok(sector) {
            issues.push(TreeIssue::BigLeafNotPlanar(key));
        }
        if key.level > SECTOR_LEVEL && super::ops::has_diagonal_features(sector) {
            issues.push(TreeIssue::DiagonalBelowSector(key));
        }
        for side in SIDES {
            for n in across(&room.leaves, key, side) {
                if n.level > key.level + 1 {
                    issues.push(TreeIssue::Unbalanced { a: n, b: key });
                }
            }
        }
    }
    issues.extend(
        hanging_mismatches(room)
            .into_iter()
            .map(TreeIssue::HangingCorner),
    );
    issues
}
