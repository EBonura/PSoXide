//! Extrude: one command for floors, ceilings and walls.
//!
//! * **Floor**: [`super::raise`] (Raise and Lower are Extrude on a floor).
//! * **Ceiling**: [`CeilingDrag`] moves the ceilings of a region up and down.
//! * **Wall pieces**: [`ExtrudeDrag`] pushes a wall piece (a derived wall
//!   face, or one band of it) out by whole sectors, which opens the sectors
//!   behind it at the piece's own bottom and top: an alcove for a full wall,
//!   a niche for a band, with the derived walls closing it by themselves.
//!   Pushed in, it fills the sectors in front of it: the platform grows, the
//!   ceiling drops, or a sector filled floor to ceiling is gone.
//!
//! Like every verb it keeps the capture contract: `begin` takes what it may
//! touch and every `apply` first restores it, so a drag is repeatable and
//! `apply(0)` gives the document back byte for byte. A refusal leaves the
//! document as it was.
//!
//! Wall pieces move in whole sectors, in rooms whose walls are derived and
//! whose cells are whole sectors; a sector another room occupies is never
//! entered.

use crate::grid::auto_split;
use crate::grid::band_geom::Span;
use crate::grid::edit::bands::wall_bands;
use crate::grid::edit::rects::{grow_room_to, CellRect};
use crate::grid::edit::refs::{FaceRef, FaceTarget, GridElement};
use crate::grid::edit::region::Region;
use crate::grid::overrides::WallMode;
use crate::grid::room::GridRoom;
use crate::grid::sector::{GridSector, WallEdge};
use crate::grid::surface::GridSurface;
use crate::grid::units::{CLICK_UNITS, SECTOR_UNITS};
use crate::grid::world::GridWorld;

use super::rules::{ceiling_corners, floor_corners, set_floor_corners};
use super::{Applied, Snapshot};

/// Why an extrusion was refused. The document is untouched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtrudeRefusal {
    /// Nothing wall-like was picked.
    NoWalls,
    /// The room's walls are hand placed: only derived walls extrude.
    AuthoredWalls,
    /// A diagonal wall has no sector to push into.
    Diagonal,
    /// The piece covers only part of its edge (a triangle).
    PartialWidth,
    /// The pieces do not all face the same way.
    MixedFacing,
    /// The pieces are not all in one room.
    MixedRooms,
    /// The room has cells of several sizes.
    Leaves,
    /// The room does not exist.
    NoRoom,
    /// Another room has geometry in a sector the wall would enter.
    OtherRoom {
        /// The room in the way.
        room: usize,
        /// The sector.
        cell: [i32; 2],
    },
    /// The room would pass its size cap.
    RoomTooLarge,
    /// The piece floats between the floor and the ceiling: there is nothing
    /// solid to grow it from.
    Floating,
}

impl ExtrudeRefusal {
    /// One line for the status bar.
    pub fn describe(&self) -> String {
        match self {
            Self::NoWalls => "Pick wall pieces to extrude".to_string(),
            Self::AuthoredWalls => {
                "This room has hand-placed walls: convert it to derived walls (Rooms panel) to extrude them"
                    .to_string()
            }
            Self::Diagonal => "A diagonal wall cannot be extruded".to_string(),
            Self::PartialWidth => "That piece covers only part of its edge".to_string(),
            Self::MixedFacing => "Pick pieces that face the same way".to_string(),
            Self::MixedRooms => "Pick pieces of one room".to_string(),
            Self::Leaves => "Cells of several sizes: extrude whole sectors only".to_string(),
            Self::NoRoom => "No such room".to_string(),
            Self::OtherRoom { room, cell } => format!(
                "Room {room} is there ({}, {}): extrude stops at another room",
                cell[0], cell[1]
            ),
            Self::RoomTooLarge => "The room would grow past its size limit".to_string(),
            Self::Floating => {
                "That piece floats between floor and ceiling: extrude the whole wall in, or out"
                    .to_string()
            }
        }
    }
}

/// One wall piece: where it is and the shape of its face.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WallPiece {
    /// Room index.
    pub room: usize,
    /// The sector the piece is seen from.
    pub host: [i32; 2],
    /// The host's edge it stands on (a cardinal one).
    pub edge: WallEdge,
    /// `[bottom a, bottom b, top b, top a]` along the edge, room-relative.
    pub heights: [i32; 4],
}

impl WallPiece {
    /// The horizontal step out of the host, through the wall.
    pub fn outward(&self) -> [i32; 2] {
        let [x, z] = self.edge.front_direction();
        [-x, -z]
    }
}

fn span_heights(span: &Span) -> Option<[i32; 4]> {
    (span.u[1] - span.u[0] == SECTOR_UNITS && span.u[0] == 0).then_some([
        span.bottom[0],
        span.bottom[1],
        span.top[1],
        span.top[0],
    ])
}

fn piece_of_face(
    world: &GridWorld,
    face: FaceRef,
    band: Option<u8>,
) -> Result<WallPiece, ExtrudeRefusal> {
    let room = face.room_of(world).ok_or(ExtrudeRefusal::NoRoom)?;
    if room.wall_mode != WallMode::Derived {
        return Err(ExtrudeRefusal::AuthoredWalls);
    }
    let FaceTarget::Derived { edge, .. } = face.target else {
        return Err(if matches!(face.target, FaceTarget::Wall { .. }) {
            ExtrudeRefusal::AuthoredWalls
        } else {
            ExtrudeRefusal::NoWalls
        });
    };
    if edge.is_diagonal() {
        return Err(ExtrudeRefusal::Diagonal);
    }
    let wall = face.derived_wall(world).ok_or(ExtrudeRefusal::NoWalls)?;
    let whole = Span::from_ring(&wall.ring).ok_or(ExtrudeRefusal::PartialWidth)?;
    let banded = band.and_then(|b| {
        wall_bands(world, face)
            .and_then(|w| w.pieces.into_iter().find(|p| p.band == usize::from(b)))
    });
    let span = match banded {
        Some(piece) => Span::from_ring(&piece.ring).ok_or(ExtrudeRefusal::PartialWidth)?,
        None => whole,
    };
    let heights = span_heights(&span).ok_or(ExtrudeRefusal::PartialWidth)?;
    Ok(WallPiece {
        room: face.room,
        host: wall.host,
        edge: wall.edge,
        heights,
    })
}

/// The wall pieces a selection names (faces and bands of derived walls);
/// other elements are ignored.
pub fn pieces_of(
    world: &GridWorld,
    elements: &[GridElement],
) -> Result<Vec<WallPiece>, ExtrudeRefusal> {
    let mut pieces: Vec<WallPiece> = Vec::new();
    for element in elements {
        let piece = match element {
            GridElement::Face(face) => piece_of_face(world, *face, None)?,
            GridElement::Band { face, band } => piece_of_face(world, *face, Some(*band))?,
            _ => continue,
        };
        if !pieces.contains(&piece) {
            pieces.push(piece);
        }
    }
    if pieces.is_empty() {
        return Err(ExtrudeRefusal::NoWalls);
    }
    Ok(pieces)
}

/// A started wall extrusion.
#[derive(Clone, Debug)]
pub struct ExtrudeDrag {
    room: usize,
    before: GridRoom,
    pieces: Vec<WallPiece>,
}

/// What an extrusion did, for the status line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Extruded {
    /// Sectors made where there was none.
    pub created: usize,
    /// Existing sectors opened up (floor lowered or ceiling raised).
    pub opened: usize,
    /// Existing sectors raised or lowered solid.
    pub filled: usize,
    /// Sectors filled floor to ceiling, which are gone.
    pub removed: usize,
}

impl ExtrudeDrag {
    /// Start on `pieces`. Refused when they are not all in one derived room
    /// of whole sectors, facing one way.
    pub fn begin(world: &GridWorld, pieces: Vec<WallPiece>) -> Result<Self, ExtrudeRefusal> {
        let first = pieces.first().ok_or(ExtrudeRefusal::NoWalls)?;
        let room = first.room;
        if pieces.iter().any(|p| p.room != room) {
            return Err(ExtrudeRefusal::MixedRooms);
        }
        if pieces.iter().any(|p| p.edge != first.edge) {
            return Err(ExtrudeRefusal::MixedFacing);
        }
        if first.edge.is_diagonal() {
            return Err(ExtrudeRefusal::Diagonal);
        }
        let source = world.rooms.get(room).ok_or(ExtrudeRefusal::NoRoom)?;
        if source.wall_mode != WallMode::Derived {
            return Err(ExtrudeRefusal::AuthoredWalls);
        }
        if !source.leaves.is_sectors_only() {
            return Err(ExtrudeRefusal::Leaves);
        }
        Ok(Self {
            room,
            before: source.clone(),
            pieces,
        })
    }

    /// Number of pieces.
    pub fn len(&self) -> usize {
        self.pieces.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    /// The pieces.
    pub fn pieces(&self) -> &[WallPiece] {
        &self.pieces
    }

    /// Restore the captured room, then push the pieces `steps` sectors out
    /// (negative: in). On a refusal the room is as it was captured.
    pub fn apply(&self, world: &mut GridWorld, steps: i32) -> Result<Extruded, ExtrudeRefusal> {
        let Some(slot) = world.rooms.get_mut(self.room) else {
            return Err(ExtrudeRefusal::NoRoom);
        };
        *slot = self.before.clone();
        if steps == 0 {
            return Ok(Extruded::default());
        }
        let result = if steps > 0 {
            self.out(world, steps)
        } else {
            self.fill_in(world, -steps)
        };
        if result.is_err() {
            world.rooms[self.room] = self.before.clone();
        }
        result
    }

    fn out(&self, world: &mut GridWorld, steps: i32) -> Result<Extruded, ExtrudeRefusal> {
        // Another room in the way is checked before anything is written.
        for piece in &self.pieces {
            let out = piece.outward();
            for k in 1..=steps {
                let cell = [piece.host[0] + out[0] * k, piece.host[1] + out[1] * k];
                for (index, other) in world.rooms.iter().enumerate() {
                    if index != self.room && occupies(other, cell) {
                        return Err(ExtrudeRefusal::OtherRoom { room: index, cell });
                    }
                }
            }
        }
        let room = &mut world.rooms[self.room];
        let mut done = Extruded::default();
        for piece in &self.pieces {
            let out = piece.outward();
            let host = host_surfaces(room, piece.host);
            let (bottom, top) = patterns(piece);
            for k in 1..=steps {
                let cell = [piece.host[0] + out[0] * k, piece.host[1] + out[1] * k];
                grow_room_to(
                    room,
                    CellRect {
                        min: cell,
                        max: cell,
                    },
                )
                .map_err(|_| ExtrudeRefusal::RoomTooLarge)?;
                let Some([x, z]) = room.local(cell) else {
                    return Err(ExtrudeRefusal::RoomTooLarge);
                };
                let existed = occupies(room, cell);
                let Some(sector) = room.sector_or_insert(x, z) else {
                    return Err(ExtrudeRefusal::RoomTooLarge);
                };
                let opened = open_up(sector, &host, bottom, top);
                if !existed {
                    done.created += 1;
                } else if opened {
                    done.opened += 1;
                }
            }
        }
        Ok(done)
    }

    fn fill_in(&self, world: &mut GridWorld, steps: i32) -> Result<Extruded, ExtrudeRefusal> {
        let room = &mut world.rooms[self.room];
        let mut done = Extruded::default();
        for piece in &self.pieces {
            let front = piece.edge.front_direction();
            let (bottom, top) = patterns(piece);
            for k in 0..steps {
                let cell = [piece.host[0] + front[0] * k, piece.host[1] + front[1] * k];
                let Some([x, z]) = room.local(cell) else {
                    break;
                };
                let Some(sector) = room.sector(x, z) else {
                    break;
                };
                let (Some(floor), ceiling) = (floor_corners(sector), ceiling_corners(sector))
                else {
                    break;
                };
                let low = (0..4).all(|i| bottom[i] <= floor[0][i] && bottom[i] <= floor[1][i]);
                let high =
                    ceiling.is_some_and(|c| (0..4).all(|i| top[i] >= c[0][i] && top[i] >= c[1][i]));
                let how = match (low, high) {
                    (true, true) => Fill::Remove,
                    (true, false) => Fill::RaiseFloor,
                    (false, true) => Fill::LowerCeiling,
                    (false, false) => return Err(ExtrudeRefusal::Floating),
                };
                let Some(sector) = room.sector_or_insert(x, z) else {
                    break;
                };
                match fill(sector, how, bottom, top) {
                    Filled::Removed => {
                        room.set_sector(x, z, None);
                        done.removed += 1;
                    }
                    Filled::Moved => done.filled += 1,
                }
            }
        }
        Ok(done)
    }
}

/// Whether `room` has floor or ceiling in the world sector `cell`.
fn occupies(room: &GridRoom, cell: [i32; 2]) -> bool {
    room.local(cell)
        .and_then(|[x, z]| room.sector(x, z))
        .is_some_and(|s| s.ground_floor().is_some() || s.ground_ceiling().is_some())
}

/// The floor and ceiling of the host, for the new sectors' materials and
/// to know whether it has a ceiling at all.
struct Host {
    floor: Option<GridSurface>,
    ceiling: Option<GridSurface>,
}

fn host_surfaces(room: &GridRoom, cell: [i32; 2]) -> Host {
    let sector = room.local(cell).and_then(|[x, z]| room.sector(x, z));
    Host {
        floor: sector.and_then(|s| s.ground_floor().cloned()),
        ceiling: sector.and_then(|s| s.ground_ceiling().cloned()),
    }
}

/// The corners `[NW, NE, SE, SW]` of a sector, as offsets `[x, z]`.
const CORNERS: [[i32; 2]; 4] = [[0, 0], [1, 0], [1, 1], [0, 1]];

/// The piece's bottom and top heights at each corner of a sector beside it
/// (corners on the shared line take the piece's two ends; the far corners
/// the same, so the opened sector is a prism).
fn patterns(piece: &WallPiece) -> ([i32; 4], [i32; 4]) {
    let (a, _) = piece.edge.endpoints();
    let axis = match piece.edge {
        WallEdge::North | WallEdge::South => 0,
        _ => 1,
    };
    let mut bottom = [0; 4];
    let mut top = [0; 4];
    for (i, corner) in CORNERS.iter().enumerate() {
        let at_a = corner[axis] == a[axis];
        bottom[i] = piece.heights[if at_a { 0 } else { 1 }];
        top[i] = piece.heights[if at_a { 3 } else { 2 }];
    }
    (bottom, top)
}

fn corners_of(surface: &GridSurface) -> [[i32; 4]; 2] {
    [surface.heights_for(0), surface.heights_for(1)]
}

/// Make `sector` at least as open as the piece: its floor no higher than the
/// bottom, its ceiling no lower than the top, corner by corner. A missing
/// floor is made, a missing ceiling only where the host has one. Returns
/// whether an existing surface changed.
fn open_up(sector: &mut GridSector, host: &Host, bottom: [i32; 4], top: [i32; 4]) -> bool {
    let mut changed = false;
    match sector.ground_floor_mut() {
        Some(floor) => {
            let old = corners_of(floor);
            let new = [0, 1].map(|t| core::array::from_fn(|i| old[t][i].min(bottom[i])));
            if new != old {
                set_floor_corners(floor, new);
                auto_split::resplit(floor);
                changed = true;
            }
        }
        None => {
            let material = host.floor.as_ref().and_then(|f| f.tri_a.material);
            let mut floor = GridSurface::flat_auto(0, material);
            floor.heights = bottom;
            auto_split::resplit(&mut floor);
            sector.set_ground_floor(Some(floor));
        }
    }
    match (sector.ground_ceiling_mut(), host.ceiling.as_ref()) {
        (Some(ceiling), _) => {
            let old = corners_of(ceiling);
            let new = [0, 1].map(|t| core::array::from_fn(|i| old[t][i].max(top[i])));
            if new != old {
                set_floor_corners(ceiling, new);
                auto_split::resplit(ceiling);
                changed = true;
            }
        }
        (None, Some(host_ceiling)) => {
            let mut ceiling = GridSurface::flat_auto(0, host_ceiling.tri_a.material);
            ceiling.heights = top;
            auto_split::resplit(&mut ceiling);
            sector.set_ground_ceiling(Some(ceiling));
        }
        (None, None) => {}
    }
    changed
}

#[derive(Clone, Copy)]
enum Fill {
    Remove,
    RaiseFloor,
    LowerCeiling,
}

enum Filled {
    Removed,
    Moved,
}

/// Fill `sector` solid over the piece's extent. A fill that leaves no room
/// between floor and ceiling removes the sector.
fn fill(sector: &mut GridSector, how: Fill, bottom: [i32; 4], top: [i32; 4]) -> Filled {
    match how {
        Fill::Remove => Filled::Removed,
        Fill::RaiseFloor => {
            let ceiling = ceiling_corners(sector);
            let Some(floor) = sector.ground_floor_mut() else {
                return Filled::Removed;
            };
            let old = corners_of(floor);
            let new: [[i32; 4]; 2] =
                [0, 1].map(|t| core::array::from_fn(|i| old[t][i].max(top[i])));
            let closed = ceiling
                .is_some_and(|c| (0..4).any(|i| new[0][i] >= c[0][i] || new[1][i] >= c[1][i]));
            if closed {
                return Filled::Removed;
            }
            set_floor_corners(floor, new);
            auto_split::resplit(floor);
            Filled::Moved
        }
        Fill::LowerCeiling => {
            let floor = floor_corners(sector);
            let Some(ceiling) = sector.ground_ceiling_mut() else {
                return Filled::Removed;
            };
            let old = corners_of(ceiling);
            let new: [[i32; 4]; 2] =
                [0, 1].map(|t| core::array::from_fn(|i| old[t][i].min(bottom[i])));
            let closed =
                floor.is_some_and(|f| (0..4).any(|i| new[0][i] <= f[0][i] || new[1][i] <= f[1][i]));
            if closed {
                return Filled::Removed;
            }
            set_floor_corners(ceiling, new);
            auto_split::resplit(ceiling);
            Filled::Moved
        }
    }
}

impl Region {
    /// The sectors of the region that have a ceiling, and how many do not.
    pub fn with_ceilings(&self, world: &GridWorld) -> (Vec<[i32; 2]>, usize) {
        let Some(room) = world.rooms.get(self.room) else {
            return (Vec::new(), self.len());
        };
        let mut kept = Vec::new();
        let mut skipped = 0;
        for cell in self.cells() {
            let has = room.local(cell).is_some()
                && room
                    .leaves
                    .in_sector(cell)
                    .iter()
                    .any(|(_, s)| s.ground_ceiling().is_some());
            if has {
                kept.push(cell);
            } else {
                skipped += 1;
            }
        }
        (kept, skipped)
    }
}

/// A started ceiling move over a region of whole sectors.
#[derive(Clone, Debug)]
pub struct CeilingDrag {
    snapshot: Snapshot,
    skipped: usize,
}

impl CeilingDrag {
    /// Start on `region`. `None` when no sector of it has a ceiling.
    pub fn begin(world: &GridWorld, region: &Region) -> Option<Self> {
        let (cells, skipped) = region.with_ceilings(world);
        if cells.is_empty() {
            return None;
        }
        Some(Self {
            snapshot: Snapshot::capture(world, region.room, &cells)?,
            skipped,
        })
    }

    /// Number of sectors that will move.
    pub fn len(&self) -> usize {
        self.snapshot.cells().len()
    }

    /// Whether none would.
    pub fn is_empty(&self) -> bool {
        self.snapshot.cells().is_empty()
    }

    /// Restore the captured sectors, then move their ceilings by `amount`
    /// grid units. A ceiling is held a click over its floor.
    pub fn apply(&self, world: &mut GridWorld, amount: i32) -> Applied {
        self.snapshot.restore(world);
        let mut applied = Applied {
            skipped: self.skipped,
            ..Applied::default()
        };
        if amount == 0 {
            return applied;
        }
        let Some(room) = world.rooms.get_mut(self.snapshot.room()) else {
            return applied;
        };
        for (cell, original) in self.snapshot.cells() {
            if original.is_none() {
                continue;
            }
            let Some([x, z]) = room.local(*cell) else {
                continue;
            };
            let Some(sector) = room.sector_mut(x, z) else {
                continue;
            };
            let floor = floor_corners(sector);
            let Some(old) = ceiling_corners(sector) else {
                continue;
            };
            let mut wanted = old;
            let mut held = false;
            for t in 0..2 {
                for i in 0..4 {
                    let moved = old[t][i].saturating_add(amount);
                    wanted[t][i] = match floor.map(|f| f[t][i] + CLICK_UNITS) {
                        Some(limit) if amount < 0 && moved < limit => {
                            held = true;
                            limit
                        }
                        _ => moved,
                    };
                }
            }
            if let Some(ceiling) = sector.ground_ceiling_mut() {
                set_floor_corners(ceiling, wanted);
                if wanted != old {
                    auto_split::resplit(ceiling);
                }
            }
            applied.moved += usize::from(wanted != old);
            applied.clamped += usize::from(held);
        }
        applied
    }
}
