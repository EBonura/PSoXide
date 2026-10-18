//! Editing bands: which set a wall run uses, the lines and bands of a set,
//! and what lies under a point of a banded wall.
//!
//! Sets live on the room (see [`super::super::bands`]). Editing is
//! copy-on-write: changing a set with the scope **Run** clones it when
//! anything outside the run also uses it and points just the run at the
//! clone; scope **Room** edits the set in place. Sets nothing refers to are
//! dropped after every edit and the indices that follow them move down.

use crate::ResourceId;

use super::super::band_geom::{self, Piece, Span};
use super::super::bands::{self, Anchor, BandSet, MAX_BAND_SETS, NO_BANDS};
use super::super::derive::{self, DerivedWall};
use super::super::room::GridRoom;
use super::super::sector::{GridSector, WallEdge};
use super::super::units::SECTOR_UNITS;
use super::super::world::GridWorld;
use super::refs::{FaceRef, FaceTarget};

/// How far a band edit reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BandScope {
    /// Just the selected run: its walls get their own copy of the set when
    /// others share it.
    Run,
    /// The set the run uses, wherever it is used (the room default).
    Room,
}

/// Cells along a wall's walking direction.
fn along(edge: WallEdge) -> Option<[i32; 2]> {
    match edge {
        WallEdge::North => Some([1, 0]),
        WallEdge::East => Some([0, 1]),
        WallEdge::South => Some([-1, 0]),
        WallEdge::West => Some([0, -1]),
        _ => None,
    }
}

fn sector_at(room: &GridRoom, cell: [i32; 2]) -> Option<&GridSector> {
    let [x, z] = room.local(cell)?;
    room.sector(x, z)
}

/// Whether `face` is a wall (derived or authored) a band set can apply to.
pub fn is_bandable(face: &FaceRef) -> bool {
    matches!(
        face.target,
        FaceTarget::Derived { .. } | FaceTarget::Wall { .. }
    )
}

/// The walls of the run `face` belongs to: for a derived wall the maximal
/// chain of collinear faces a fit mapping treats as one face, for an
/// authored one the collinear neighbours with the same shape, material and
/// facing.
pub fn run_faces(world: &GridWorld, face: FaceRef) -> Vec<FaceRef> {
    let Some(room) = face.room_of(world) else {
        return Vec::new();
    };
    match face.target {
        FaceTarget::Derived { .. } => {
            let Some(wall) = face.derived_wall(world) else {
                return Vec::new();
            };
            derive::run_members(room, &wall)
                .into_iter()
                .map(|w| FaceRef::derived(face.room, w.host, w.edge, w.kind))
                .collect()
        }
        FaceTarget::Wall { edge, index } => {
            let Some(wall) = face.wall_of(world) else {
                return Vec::new();
            };
            let Some(step) = along(edge) else {
                return vec![face];
            };
            let same = |cell: [i32; 2]| -> Option<usize> {
                sector_at(room, cell)?.walls(edge).iter().position(|w| {
                    w.heights == wall.heights
                        && w.material == wall.material
                        && w.facing == wall.facing
                })
            };
            let mut out = vec![face];
            for sign in [-1, 1] {
                let mut cell = face.cell();
                while let Some(at) = {
                    cell = [cell[0] + step[0] * sign, cell[1] + step[1] * sign];
                    same(cell)
                } {
                    out.push(FaceRef::wall(face.room, cell, edge, at));
                }
            }
            let _ = index;
            out.sort();
            out
        }
        FaceTarget::Surface(_) => Vec::new(),
    }
}

/// The set a wall names itself: `None` for "the room default", `Some`
/// otherwise (an index, or [`NO_BANDS`]). The outer `None` means `face` is
/// not a wall that exists.
pub fn assignment(world: &GridWorld, face: FaceRef) -> Option<Option<u8>> {
    match face.target {
        FaceTarget::Derived { .. } => face.derived_wall(world).map(|w| w.band_set),
        FaceTarget::Wall { .. } => face.wall_of(world).map(|w| w.band_set),
        FaceTarget::Surface(_) => None,
    }
}

/// The set `face` is drawn with, as `(index, set)`.
pub fn effective_set(world: &GridWorld, face: FaceRef) -> Option<(u8, &BandSet)> {
    let room = face.room_of(world)?;
    bands::resolve(room, assignment(world, face)?)
}

fn assign(world: &mut GridWorld, face: FaceRef, value: Option<u8>) -> bool {
    match face.target {
        FaceTarget::Derived { .. } => {
            let Some(key) = face.derived_wall(world).map(|w| w.key) else {
                return false;
            };
            let Some(room) = world.rooms.get_mut(face.room) else {
                return false;
            };
            let before = room.edge_overrides.get(&key).and_then(|o| o.band_set);
            room.edge_overrides.edit(key, |o| o.band_set = value);
            before != value
        }
        FaceTarget::Wall { .. } => match face.wall_mut(world) {
            Some(wall) if wall.band_set != value => {
                wall.band_set = value;
                true
            }
            _ => false,
        },
        FaceTarget::Surface(_) => false,
    }
}

/// How many places refer to each set of `room`: the default, derived
/// overrides and authored walls.
fn references(room: &GridRoom) -> Vec<u32> {
    let mut counts = vec![0u32; room.band_sets.len()];
    let mut note = |index: Option<u8>| {
        if let Some(slot) = index.and_then(|i| counts.get_mut(usize::from(i))) {
            *slot += 1;
        }
    };
    note(room.defaults.bands);
    for (_, o) in room.edge_overrides.iter() {
        note(o.band_set);
    }
    for (_, _, sector) in room.iter() {
        for edge in WallEdge::ALL {
            for wall in sector.walls(edge) {
                note(wall.band_set);
            }
        }
    }
    counts
}

/// Drop the sets nothing refers to and renumber what follows them.
pub fn prune_sets(room: &mut GridRoom) -> usize {
    let counts = references(room);
    let keep: Vec<bool> = counts.iter().map(|c| *c > 0).collect();
    if keep.iter().all(|k| *k) {
        return 0;
    }
    // New index of each kept set.
    let mut next = 0u8;
    let map: Vec<Option<u8>> = keep
        .iter()
        .map(|k| {
            k.then(|| {
                next += 1;
                next - 1
            })
        })
        .collect();
    let remap = |index: Option<u8>| -> Option<u8> {
        index.and_then(|i| {
            if i == NO_BANDS {
                Some(NO_BANDS)
            } else {
                map.get(usize::from(i)).copied().flatten()
            }
        })
    };
    room.defaults.bands = remap(room.defaults.bands);
    room.edge_overrides
        .map_band_sets(|index| remap(Some(index)));
    let (width, depth) = (room.width, room.depth);
    for z in 0..depth {
        for x in 0..width {
            if let Some(sector) = room.sector_mut(x, z) {
                for edge in WallEdge::ALL {
                    for wall in sector.walls_mut(edge) {
                        wall.band_set = remap(wall.band_set);
                    }
                }
            }
        }
    }
    let removed = keep.iter().filter(|k| !**k).count();
    let mut flags = keep.into_iter();
    room.band_sets.retain(|_| flags.next().unwrap_or(true));
    removed
}

/// The material a new set starts with: the wall's own.
fn wall_material(world: &GridWorld, face: FaceRef) -> Option<ResourceId> {
    match face.target {
        FaceTarget::Derived { .. } => face.derived_wall(world)?.material,
        FaceTarget::Wall { .. } => face.wall_of(world)?.material,
        FaceTarget::Surface(_) => None,
    }
}

/// Apply `change` to the band set of `face`'s run. With no set yet the run
/// gets a new one of a single band in the wall's own material (scope Room
/// makes it the room's default). Returns whether anything changed; a change
/// that does nothing leaves the document untouched.
pub fn edit_bands(
    world: &mut GridWorld,
    face: FaceRef,
    scope: BandScope,
    change: impl FnOnce(&mut BandSet) -> bool,
) -> bool {
    if !is_bandable(&face) {
        return false;
    }
    let faces = run_faces(world, face);
    let Some(assigned) = assignment(world, face) else {
        return false;
    };
    let current = effective_set(world, face).map(|(i, s)| (i, s.clone()));
    // Edit a copy first so a no-op leaves nothing behind.
    let (existing, mut edited) = match &current {
        Some((i, set)) => (Some(*i), set.clone()),
        None => (None, BandSet::single("Bands", wall_material(world, face))),
    };
    if !change(&mut edited) || !edited.is_valid() {
        return false;
    }
    let Some(room) = world.rooms.get(face.room) else {
        return false;
    };
    let full = room.band_sets.len() >= MAX_BAND_SETS;
    match (scope, existing) {
        (BandScope::Room, Some(i)) => {
            if let Some(room) = world.rooms.get_mut(face.room) {
                room.band_sets[usize::from(i)] = edited;
            }
        }
        (BandScope::Run, Some(i)) => {
            // Private when every reference to the set is one of this run's
            // own walls.
            let counts = references(room);
            let mine = faces
                .iter()
                .filter(|f| assignment(world, **f) == Some(Some(i)))
                .count() as u32;
            let private = counts.get(usize::from(i)).copied() == Some(mine) && mine > 0;
            if private {
                if let Some(room) = world.rooms.get_mut(face.room) {
                    room.band_sets[usize::from(i)] = edited;
                }
            } else {
                if full {
                    return false;
                }
                let Some(room) = world.rooms.get_mut(face.room) else {
                    return false;
                };
                room.band_sets.push(edited);
                let new = (room.band_sets.len() - 1) as u8;
                for f in &faces {
                    assign(world, *f, Some(new));
                }
            }
        }
        (BandScope::Room, None) => {
            if full {
                return false;
            }
            let Some(room) = world.rooms.get_mut(face.room) else {
                return false;
            };
            room.band_sets.push(edited);
            room.defaults.bands = Some((room.band_sets.len() - 1) as u8);
            // A run that opted out of bands joins the default.
            if assigned == Some(NO_BANDS) {
                for f in &faces {
                    assign(world, *f, None);
                }
            }
        }
        (BandScope::Run, None) => {
            if full {
                return false;
            }
            let Some(room) = world.rooms.get_mut(face.room) else {
                return false;
            };
            room.band_sets.push(edited);
            let new = (room.band_sets.len() - 1) as u8;
            for f in &faces {
                assign(world, *f, Some(new));
            }
        }
    }
    if let Some(room) = world.rooms.get_mut(face.room) {
        prune_sets(room);
    }
    true
}

/// Take the bands off `face`'s run (scope Run: an explicit "none", so the
/// run stays plain in a room with a default) or clear the room default
/// (scope Room).
pub fn clear_bands(world: &mut GridWorld, face: FaceRef, scope: BandScope) -> bool {
    if !is_bandable(&face) {
        return false;
    }
    let changed = match scope {
        BandScope::Run => {
            let faces = run_faces(world, face);
            let default = face
                .room_of(world)
                .is_some_and(|r| r.defaults.bands.is_some());
            let value = default.then_some(NO_BANDS);
            let mut any = false;
            for f in faces {
                any |= assign(world, f, value);
            }
            any
        }
        BandScope::Room => match world.rooms.get_mut(face.room) {
            Some(room) if room.defaults.bands.is_some() => {
                room.defaults.bands = None;
                true
            }
            _ => false,
        },
    };
    if changed {
        if let Some(room) = world.rooms.get_mut(face.room) {
            prune_sets(room);
        }
    }
    changed
}

/// A banded wall as the editor sees it.
#[derive(Clone, Debug)]
pub struct WallBands {
    /// Index of the set in the room.
    pub set: u8,
    /// The wall's region in `(u, y)`, room relative grid units.
    pub span: Span,
    /// What the anchors measure against.
    pub reference: Span,
    /// The pieces the faces are made from, before neighbour stitching.
    pub pieces: Vec<Piece>,
}

/// The span, reference and band set of `face`, when it is a wall with bands.
pub fn wall_bands(world: &GridWorld, face: FaceRef) -> Option<WallBands> {
    let room = face.room_of(world)?;
    let (set_index, set) = effective_set(world, face)?;
    let (span, reference) = wall_spans(world, face)?;
    let pieces = band_geom::bands(&span, &reference, set);
    Some(WallBands {
        set: set_index,
        span,
        reference,
        pieces,
    })
    .filter(|_| room.band_sets.get(usize::from(set_index)).is_some())
}

/// A wall's own span and the reference its anchors measure against, with or
/// without bands.
pub fn wall_spans(world: &GridWorld, face: FaceRef) -> Option<(Span, Span)> {
    match face.target {
        FaceTarget::Derived { .. } => {
            let wall: DerivedWall = face.derived_wall(world)?;
            let span = Span::from_ring(&wall.ring)?;
            Some((span, span))
        }
        FaceTarget::Wall { edge, .. } => {
            let wall = face.wall_of(world)?;
            let length = if edge.is_diagonal() {
                (f64::from(SECTOR_UNITS) * core::f64::consts::SQRT_2).round() as i32
            } else {
                SECTOR_UNITS
            };
            let span = Span::from_heights(wall.heights, length);
            let reference = face
                .sector(world)
                .and_then(|s| band_geom::stack_reference(s.walls(edge), length))
                .unwrap_or(span);
            Some((span, reference))
        }
        FaceTarget::Surface(_) => None,
    }
}

/// The heights of the set's cut lines at both ends of `face`, bottom up:
/// `[at the start, at the end]` per line.
pub fn line_ends(world: &GridWorld, face: FaceRef) -> Vec<[i32; 2]> {
    let (Some((_, set)), Some((span, reference))) =
        (effective_set(world, face), wall_spans(world, face))
    else {
        return Vec::new();
    };
    let start = band_geom::cuts_at(&span, &reference, set, span.u[0]);
    let end = band_geom::cuts_at(&span, &reference, set, span.u[1]);
    start.into_iter().zip(end).map(|(a, b)| [a, b]).collect()
}

fn inside(ring: &[[i32; 2]], p: [i32; 2]) -> bool {
    let n = ring.len();
    let sign = band_geom::twice_area(ring).signum();
    if sign == 0 {
        return false;
    }
    (0..n).all(|i| {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let cross = i64::from(b[0] - a[0]) * i64::from(p[1] - a[1])
            - i64::from(b[1] - a[1]) * i64::from(p[0] - a[0]);
        cross.signum() * sign >= 0
    })
}

/// The band at position `u` along the wall and room-relative height `y`.
pub fn band_at(world: &GridWorld, face: FaceRef, u: i32, y: i32) -> Option<usize> {
    let bands = wall_bands(world, face)?;
    bands
        .pieces
        .iter()
        .find(|p| inside(&p.ring, [u, y]))
        .map(|p| p.band)
}

/// Put `material` on band `index` of `face`'s set.
pub fn paint_band(
    world: &mut GridWorld,
    face: FaceRef,
    index: usize,
    material: Option<ResourceId>,
    scope: BandScope,
) -> bool {
    edit_bands(world, face, scope, |set| {
        set.set_band_material(index, material)
    })
}

/// How many wall faces the run of `face` has, for the "applies to N wall
/// segments" line.
pub fn run_len(world: &GridWorld, face: FaceRef) -> usize {
    run_faces(world, face).len()
}

/// Position along `face`'s walking direction of the world grid point `xz`,
/// grid units. The faces' own `u` (0 at the walking start of the segment).
pub fn position_on_wall(face: FaceRef, xz: [i32; 2]) -> Option<i32> {
    let (cell, edge) = match face.target {
        FaceTarget::Derived { edge, .. } | FaceTarget::Wall { edge, .. } => (face.cell(), edge),
        FaceTarget::Surface(_) => return None,
    };
    let (ea, eb) = edge.endpoints();
    let at = |e: [i32; 2]| {
        [
            (cell[0] + e[0]) * SECTOR_UNITS,
            (cell[1] + e[1]) * SECTOR_UNITS,
        ]
    };
    let (a, b) = (at(ea), at(eb));
    let d = [f64::from(b[0] - a[0]), f64::from(b[1] - a[1])];
    let length = d[0].hypot(d[1]);
    if length == 0.0 {
        return None;
    }
    let p = [f64::from(xz[0] - a[0]), f64::from(xz[1] - a[1])];
    Some(((p[0] * d[0] + p[1] * d[1]) / length).round() as i32)
}

/// Which anchor a new line at room-relative height `y` and position `u`
/// gets: near the bottom of the wall it follows the bottom edge, near the
/// top the top edge, between them it is an absolute datum.
pub fn anchor_for(world: &GridWorld, face: FaceRef, u: i32, y: i32) -> Option<Anchor> {
    let (span, reference) = wall_spans(world, face)?;
    let bottom = band_geom::rail_height(reference.bottom, &reference, u);
    let top = band_geom::rail_height(reference.top, &reference, u);
    let height = (top - bottom).max(1);
    let up = i64::from(y - bottom) * 4 / i64::from(height);
    let _ = span;
    Some(if up < 1 {
        Anchor::Bottom((y - bottom).max(0))
    } else if up >= 3 {
        Anchor::Top((top - y).max(0))
    } else {
        Anchor::Abs(y)
    })
}

/// Where a line would sit at position `u` on `face`.
pub fn anchor_height_at(world: &GridWorld, face: FaceRef, anchor: Anchor, u: i32) -> Option<i32> {
    let (span, reference) = wall_spans(world, face)?;
    let ends = band_geom::anchor_ends(&span, &reference, anchor);
    let t = f64::from(u - span.u[0]) / f64::from((span.u[1] - span.u[0]).max(1));
    Some((f64::from(ends[0]) + f64::from(ends[1] - ends[0]) * t).round() as i32)
}

/// Where a new line at height `y` goes in the set's order: after every
/// existing line that sits below it at position `u`.
pub fn insert_index(world: &GridWorld, face: FaceRef, u: i32, y: i32) -> usize {
    let Some((span, _)) = wall_spans(world, face) else {
        return 0;
    };
    let Some((_, set)) = effective_set(world, face) else {
        return 0;
    };
    let Some((_, reference)) = wall_spans(world, face) else {
        return 0;
    };
    band_geom::cuts_at(&span, &reference, set, u)
        .iter()
        .filter(|h| **h <= y)
        .count()
}

/// Move line `index` of `face`'s set so it sits at room-relative height `y`
/// at position `u`, between its neighbours, keeping its kind of anchor.
pub fn move_line(
    world: &mut GridWorld,
    face: FaceRef,
    index: usize,
    u: i32,
    y: i32,
    scope: BandScope,
) -> bool {
    let Some((_, set)) = effective_set(world, face) else {
        return false;
    };
    let (Some(old), Some((span, reference))) =
        (set.lines.get(index).copied(), wall_spans(world, face))
    else {
        return false;
    };
    let cuts = band_geom::cuts_at(&span, &reference, set, u);
    let bottom = band_geom::rail_height(span.bottom, &span, u);
    let top = band_geom::rail_height(span.top, &span, u);
    let below = if index == 0 { bottom } else { cuts[index - 1] };
    let above = cuts.get(index + 1).copied().unwrap_or(top);
    let y = y.clamp(below.min(above), above.max(below));
    // The anchor's value that puts the line at `y` here.
    let ref_bottom = band_geom::rail_height(reference.bottom, &reference, u);
    let ref_top = band_geom::rail_height(reference.top, &reference, u);
    let value = match old {
        Anchor::Bottom(_) => y - ref_bottom,
        Anchor::Top(_) => ref_top - y,
        Anchor::Abs(_) => y,
    };
    let new = old.with_value(value);
    edit_bands(world, face, scope, |set| set.set_line(index, new))
}

/// Where `anchor` falls at both ends of `face`.
pub fn anchor_ends_of(world: &GridWorld, face: FaceRef, anchor: Anchor) -> Option<[i32; 2]> {
    let (span, reference) = wall_spans(world, face)?;
    Some(band_geom::anchor_ends(&span, &reference, anchor))
}
