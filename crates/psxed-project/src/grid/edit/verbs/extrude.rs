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
//! Wall pieces move in whole sectors, in rooms whose cells are whole
//! sectors; a sector another room occupies is never entered.
//!
//! **Hand-placed walls extrude too.** A wall record the user placed (in a
//! derived room, where it sits beside the derived faces, or in a room that
//! derives nothing) is a piece like any other. In the same step the push
//! turns exactly the records it touches into derived behaviour: the picked
//! records go (a room that derives nothing is first converted with its look
//! kept, see [`convert_to_derived`], because the wall mode is per room), the
//! walls the push makes wear the picked wall's material, and any other record
//! on an edge of a sector the push makes or fills moves its look (material,
//! placement, bands, solidity) onto the derived face that now stands exactly
//! where it did, or stays when none does. Where a room has no way to derive
//! a closure (an open top with no `open_wall`) the corridor or alcove is
//! closed with hand-placed walls of the picked wall's material instead.
//! Nothing else in the document is touched.

use std::collections::BTreeSet;

use crate::grid::auto_split;
use crate::grid::band_geom::Span;
use crate::grid::derive::{self, DerivedKind, DerivedWall};
use crate::grid::edit::bands::{effective_set, wall_bands};
use crate::grid::edit::convert::{convert_to_derived, placement, ConvertMode};
use crate::grid::edit::rects::{grow_room_to, CellRect};
use crate::grid::edit::refs::{FaceRef, FaceTarget, GridElement};
use crate::grid::edit::region::Region;
use crate::grid::edit::walls::edge_corner_indices;
use crate::grid::overrides::{EdgeKey, WallMode};
use crate::grid::room::GridRoom;
use crate::grid::sector::{GridSector, WallEdge};
use crate::grid::surface::{GridSurface, GridWall, WallFacing};
use crate::grid::units::{CLICK_UNITS, SECTOR_UNITS};
use crate::grid::world::GridWorld;
use crate::ResourceId;

use super::rules::{ceiling_corners, floor_corners, set_floor_corners};
use super::{Applied, Snapshot};

/// Why an extrusion was refused. The document is untouched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtrudeRefusal {
    /// Nothing wall-like was picked.
    NoWalls,
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
    /// The side the wall is drawn to has no floor: there is no sector to
    /// push it from.
    NoFloor,
}

impl ExtrudeRefusal {
    /// One line for the status bar.
    pub fn describe(&self) -> String {
        match self {
            Self::NoWalls => "Pick wall pieces to extrude".to_string(),
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
            Self::NoFloor => {
                "That wall faces empty space: there is no floor to push it from".to_string()
            }
        }
    }
}

/// How a wall looks, to give to the walls its extrusion makes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Look {
    /// The material.
    pub material: Option<ResourceId>,
    /// The band set, when the whole wall was picked (a band pushed out
    /// makes a niche that is one piece, not banded again).
    pub band_set: Option<u8>,
}

/// A hand-placed wall record a piece stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandPlaced {
    /// The sector that stores the record.
    pub cell: [i32; 2],
    /// The edge stack it is in.
    pub edge: WallEdge,
    /// The record as it was picked.
    pub wall: GridWall,
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
    /// The hand-placed record this piece is, if it is not a derived face.
    pub source: Option<HandPlaced>,
    /// What the walls it makes look like.
    pub look: Look,
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

/// The ring of band `band` of `face`, if the wall is banded that way.
fn band_ring(world: &GridWorld, face: FaceRef, band: Option<u8>) -> Option<Vec<[i32; 2]>> {
    let band = band?;
    wall_bands(world, face)?
        .pieces
        .into_iter()
        .find(|p| p.band == usize::from(band))
        .map(|p| p.ring)
}

/// The material of band `band` of `face`, falling back on the wall's own.
fn band_material(world: &GridWorld, face: FaceRef, band: Option<u8>) -> Option<Option<ResourceId>> {
    let band = band?;
    let (_, set) = effective_set(world, face)?;
    Some(set.bands.get(usize::from(band))?.material)
}

fn piece_of_face(
    world: &GridWorld,
    face: FaceRef,
    band: Option<u8>,
) -> Result<WallPiece, ExtrudeRefusal> {
    let room = face.room_of(world).ok_or(ExtrudeRefusal::NoRoom)?;
    match face.target {
        FaceTarget::Derived { edge, .. } => {
            if edge.is_diagonal() {
                return Err(ExtrudeRefusal::Diagonal);
            }
            let wall = face.derived_wall(world).ok_or(ExtrudeRefusal::NoWalls)?;
            let whole = Span::from_ring(&wall.ring).ok_or(ExtrudeRefusal::PartialWidth)?;
            let banded = band_ring(world, face, band);
            let span = match &banded {
                Some(ring) => Span::from_ring(ring).ok_or(ExtrudeRefusal::PartialWidth)?,
                None => whole,
            };
            let heights = span_heights(&span).ok_or(ExtrudeRefusal::PartialWidth)?;
            let material = match band_material(world, face, band.filter(|_| banded.is_some())) {
                Some(Some(material)) => Some(material),
                _ => wall.material,
            };
            Ok(WallPiece {
                room: face.room,
                host: wall.host,
                edge: wall.edge,
                heights,
                source: None,
                look: Look {
                    material,
                    band_set: if banded.is_some() {
                        None
                    } else {
                        wall.band_set
                    },
                },
            })
        }
        FaceTarget::Wall { edge, .. } => {
            if edge.is_diagonal() {
                return Err(ExtrudeRefusal::Diagonal);
            }
            let record = face.wall_of(world).ok_or(ExtrudeRefusal::NoWalls)?.clone();
            let cell = face.cell();
            // The side the wall is drawn to is the side it is pushed from.
            let (host, host_edge, mirrored) = hand_placed_side(room, cell, edge, record.facing)?;
            let banded = band_ring(world, face, band);
            let mut heights = match &banded {
                Some(ring) => {
                    let span = Span::from_ring(ring).ok_or(ExtrudeRefusal::PartialWidth)?;
                    span_heights(&span).ok_or(ExtrudeRefusal::PartialWidth)?
                }
                None => record.heights,
            };
            if mirrored {
                // Along the far sector's own walking direction the ends swap.
                heights = [heights[1], heights[0], heights[3], heights[2]];
            }
            let material = match band_material(world, face, band.filter(|_| banded.is_some())) {
                Some(Some(material)) => Some(material),
                _ => record.material,
            };
            Ok(WallPiece {
                room: face.room,
                host,
                edge: host_edge,
                heights,
                look: Look {
                    material,
                    band_set: if banded.is_some() {
                        None
                    } else {
                        record.band_set
                    },
                },
                source: Some(HandPlaced {
                    cell,
                    edge,
                    wall: record,
                }),
            })
        }
        FaceTarget::Surface { .. } => Err(ExtrudeRefusal::NoWalls),
    }
}

/// The sector a hand-placed wall on `edge` of `cell` is seen from, the edge
/// of it the wall lies on and whether that is the far side of the record.
fn hand_placed_side(
    room: &GridRoom,
    cell: [i32; 2],
    edge: WallEdge,
    facing: WallFacing,
) -> Result<([i32; 2], WallEdge, bool), ExtrudeRefusal> {
    let (opposite, offset) = edge.opposite().ok_or(ExtrudeRefusal::Diagonal)?;
    let near = (cell, edge, false);
    let far = ([cell[0] + offset[0], cell[1] + offset[1]], opposite, true);
    let order = match facing {
        WallFacing::Front => [near, near],
        WallFacing::Back => [far, far],
        // Seen from both sides: the sector the record is stored in, unless
        // only the other one has a floor.
        WallFacing::Both => [near, far],
    };
    order
        .into_iter()
        .find(|(host, ..)| has_floor(room, *host))
        .ok_or(ExtrudeRefusal::NoFloor)
}

fn has_floor(room: &GridRoom, cell: [i32; 2]) -> bool {
    room.local(cell)
        .and_then(|[x, z]| room.sector(x, z))
        .is_some_and(|s| s.ground_floor().is_some())
}

/// The wall pieces a selection names (faces and bands of derived walls and
/// of hand-placed ones); other elements are ignored.
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

impl Extruded {
    /// The words for a result that pushed `steps` sectors (out when
    /// positive), for the status bar and the MCP reply alike.
    pub fn describe(&self, steps: i32) -> String {
        if steps == 0 {
            return "Extrude: no change".to_string();
        }
        let way = if steps > 0 { "out" } else { "in" };
        let n = steps.abs();
        let sectors = if n == 1 { "sector" } else { "sectors" };
        let mut line = format!("Extrude {way} {n} {sectors}");
        let parts = [
            (self.created, "made"),
            (self.opened, "opened"),
            (self.filled, "filled"),
            (self.removed, "removed"),
        ];
        let text: Vec<String> = parts
            .iter()
            .filter(|(n, _)| *n > 0)
            .map(|(n, what)| format!("{n} {what}"))
            .collect();
        if !text.is_empty() {
            line += &format!(": {}", text.join(", "));
        }
        line
    }
}

/// The sectors an extrusion visited, for the walls to settle afterwards.
#[derive(Default)]
struct Touched {
    /// Sectors made, with the piece that made them.
    created: Vec<(usize, [i32; 2])>,
    /// Every sector made, opened, filled or removed.
    cells: Vec<[i32; 2]>,
}

impl ExtrudeDrag {
    /// Start on `pieces`. Refused when they are not all in one room of whole
    /// sectors, facing one way.
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
        if !source.leaves.is_sectors_only() {
            return Err(ExtrudeRefusal::Leaves);
        }
        if pieces.iter().any(|p| !has_floor(source, p.host)) {
            return Err(ExtrudeRefusal::NoFloor);
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
        let result = self.run(world, steps);
        if result.is_err() {
            world.rooms[self.room] = self.before.clone();
        }
        result
    }

    fn run(&self, world: &mut GridWorld, steps: i32) -> Result<Extruded, ExtrudeRefusal> {
        if steps > 0 {
            self.check_clear(world, steps)?;
        }
        // A room that derives nothing derives from here on, with every wall
        // it had looking as it did.
        if world.rooms[self.room].wall_mode != WallMode::Derived {
            convert_to_derived(world, self.room, ConvertMode::PreserveLook);
        }
        let texels = world.texels_per_sector.get();
        let window = self.window(steps);
        let room = &world.rooms[self.room];
        let faces_before = face_signatures(room, &window);
        let hand_before = hand_placed_in(room, &window);
        let mut touched = Touched::default();
        let done = if steps > 0 {
            self.out(world, steps, &mut touched)?
        } else {
            self.fill_in(world, -steps, &mut touched)?
        };
        let room = &mut world.rooms[self.room];
        self.give_new_walls_their_look(room, steps, &faces_before);
        self.settle_hand_placed(room, &hand_before, &touched, texels);
        self.close_open_edges(room, &touched);
        Ok(done)
    }

    /// Another room in the way is checked before anything is written.
    fn check_clear(&self, world: &GridWorld, steps: i32) -> Result<(), ExtrudeRefusal> {
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
        Ok(())
    }

    /// The sectors the push can change a wall of: the ones it visits and a
    /// ring round them.
    fn window(&self, steps: i32) -> BTreeSet<[i32; 2]> {
        self.pieces
            .iter()
            .flat_map(|piece| Self::swath(piece, steps))
            .collect()
    }

    fn out(
        &self,
        world: &mut GridWorld,
        steps: i32,
        touched: &mut Touched,
    ) -> Result<Extruded, ExtrudeRefusal> {
        let room = &mut world.rooms[self.room];
        let mut done = Extruded::default();
        for (index, piece) in self.pieces.iter().enumerate() {
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
                touched.cells.push(cell);
                if !existed {
                    done.created += 1;
                    touched.created.push((index, cell));
                } else if opened {
                    done.opened += 1;
                }
            }
        }
        Ok(done)
    }

    fn fill_in(
        &self,
        world: &mut GridWorld,
        steps: i32,
        touched: &mut Touched,
    ) -> Result<Extruded, ExtrudeRefusal> {
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
                touched.cells.push(cell);
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

    /// The faces the push made, in each piece's swath, wear the piece's
    /// look (a face that was there before keeps its own).
    fn give_new_walls_their_look(
        &self,
        room: &mut GridRoom,
        steps: i32,
        before: &BTreeSet<FaceSignature>,
    ) {
        let mut claimed: BTreeSet<EdgeKey> = BTreeSet::new();
        for piece in &self.pieces {
            if piece.look == Look::default() {
                continue;
            }
            let walls: Vec<DerivedWall> = Self::swath(piece, steps)
                .iter()
                .flat_map(|&c| derive::cell_walls(room, c))
                .collect();
            for wall in walls {
                if wall.hidden || before.contains(&signature(&wall)) || !claimed.insert(wall.key) {
                    continue;
                }
                let look = piece.look.clone();
                room.edge_overrides.edit(wall.key, |o| {
                    if o.material.is_none() && look.material != wall.material {
                        o.material = look.material;
                    }
                    if o.band_set.is_none() {
                        o.band_set = look.band_set;
                    }
                });
            }
        }
    }

    /// The sectors one piece's push runs through and the ring round them.
    fn swath(piece: &WallPiece, steps: i32) -> BTreeSet<[i32; 2]> {
        let out = piece.outward();
        let mut cells = BTreeSet::new();
        for k in steps.min(0)..=steps.max(0) {
            let at = [piece.host[0] + out[0] * k, piece.host[1] + out[1] * k];
            for dz in -1..=1 {
                for dx in -1..=1 {
                    cells.insert([at[0] + dx, at[1] + dz]);
                }
            }
        }
        cells
    }

    /// The hand-placed records the push touched become derived behaviour.
    fn settle_hand_placed(
        &self,
        room: &mut GridRoom,
        hand_before: &[HandPlaced],
        touched: &Touched,
        texels: u16,
    ) {
        // The picked records: gone, their look on whatever stands at their
        // place now.
        let picked: Vec<&HandPlaced> = self
            .pieces
            .iter()
            .filter_map(|p| p.source.as_ref())
            .collect();
        for record in &picked {
            remove_record(room, record);
            let key = derive::edge_key(record.cell, record.edge);
            // The record stood for the derived face the user had hidden.
            room.edge_overrides.edit(key, |o| o.hidden = false);
            let faces = derive::physical_edge_walls(room, record.cell, record.edge);
            if let Some(face) = faces.first() {
                carry_look(room, record, face, texels);
            }
        }
        // The others on the edges of the sectors the push visited: the ones
        // a derived face now stands exactly on move onto it, the rest stay.
        let keys: BTreeSet<EdgeKey> = touched
            .cells
            .iter()
            .flat_map(|&cell| {
                derive::CARDINAL
                    .into_iter()
                    .map(move |edge| derive::edge_key(cell, edge))
            })
            .collect();
        for record in hand_before {
            if picked.contains(&record)
                || !keys.contains(&derive::edge_key(record.cell, record.edge))
            {
                continue;
            }
            let faces = derive::physical_edge_walls(room, record.cell, record.edge);
            let Some(face) = faces.iter().find(|f| covers(f, record)) else {
                continue;
            };
            remove_record(room, record);
            carry_look(room, record, face, texels);
        }
    }

    /// Where the room cannot derive a wall (an open top with no wall
    /// height), a sector the push made is closed by hand-placed walls of
    /// the picked wall's material, as the wall it was cut from was.
    fn close_open_edges(&self, room: &mut GridRoom, touched: &Touched) {
        for &(index, cell) in &touched.created {
            let piece = &self.pieces[index];
            if piece.source.is_none() {
                continue;
            }
            let (_, top) = patterns(piece);
            for edge in derive::CARDINAL {
                let front = edge.front_direction();
                let beyond = [cell[0] - front[0], cell[1] - front[1]];
                if occupies(room, beyond) || !derive::edge_walls(room, cell, edge).is_empty() {
                    continue;
                }
                let Some([x, z]) = room.local(cell) else {
                    continue;
                };
                let Some(sector) = room.sector_mut(x, z) else {
                    continue;
                };
                if !sector.walls(edge).is_empty() {
                    continue;
                }
                let Some(floor) = sector.ground_floor().map(|f| f.heights) else {
                    continue;
                };
                let [a, b] = edge_corner_indices(edge);
                let heights = [floor[a], floor[b], top[b], top[a]];
                let wall = GridWall {
                    heights,
                    material: piece.look.material,
                    band_set: piece.look.band_set,
                    ..GridWall::span(0, 0, None)
                };
                if !wall.is_degenerate() {
                    sector.walls_mut(edge).push(wall);
                }
            }
        }
    }
}

/// What identifies a derived face across an edit.
type FaceSignature = (EdgeKey, [i32; 2], DerivedKind, u8);

fn signature(wall: &DerivedWall) -> FaceSignature {
    (wall.key, wall.host, wall.kind, wall.span)
}

fn face_signatures(room: &GridRoom, cells: &BTreeSet<[i32; 2]>) -> BTreeSet<FaceSignature> {
    cells
        .iter()
        .flat_map(|&c| derive::cell_walls(room, c))
        .map(|w| signature(&w))
        .collect()
}

/// Every hand-placed record stored in the sectors of `cells`.
fn hand_placed_in(room: &GridRoom, cells: &BTreeSet<[i32; 2]>) -> Vec<HandPlaced> {
    let mut out = Vec::new();
    for &cell in cells {
        let Some(sector) = room.local(cell).and_then(|[x, z]| room.sector(x, z)) else {
            continue;
        };
        for edge in derive::CARDINAL {
            for wall in sector.walls(edge) {
                out.push(HandPlaced {
                    cell,
                    edge,
                    wall: wall.clone(),
                });
            }
        }
    }
    out
}

fn remove_record(room: &mut GridRoom, record: &HandPlaced) {
    let Some([x, z]) = room.local(record.cell) else {
        return;
    };
    let Some(sector) = room.sector_mut(x, z) else {
        return;
    };
    let stack = sector.walls_mut(record.edge);
    if let Some(at) = stack.iter().position(|w| *w == record.wall) {
        stack.remove(at);
    }
}

/// Whether `face` is the same wall as the record: the same shape, seen
/// from the side the record is drawn to.
fn covers(face: &DerivedWall, record: &HandPlaced) -> bool {
    let Some(shape) = face.as_wall_of(record.cell, record.edge) else {
        return false;
    };
    let from_front = face.host == record.cell && face.edge == record.edge && !face.back;
    shape.heights == record.wall.heights
        && match record.wall.facing {
            WallFacing::Front => from_front,
            WallFacing::Back => !from_front,
            WallFacing::Both => true,
        }
}

/// Put a hand-placed wall's look on the override of the derived `face` that
/// replaces it.
fn carry_look(room: &mut GridRoom, record: &HandPlaced, face: &DerivedWall, texels: u16) {
    let wall = &record.wall;
    let mirrored = !(face.host == record.cell && face.edge == record.edge);
    let uv = placement(wall, (face.host, face.edge), mirrored, texels);
    let closure = face.kind == DerivedKind::Closure;
    let chained = face.material;
    let existing = room
        .edge_overrides
        .get(&face.key)
        .cloned()
        .unwrap_or_default();
    room.edge_overrides.edit(face.key, |o| {
        o.hidden = false;
        if wall.material != chained || existing.material.is_some() {
            o.material = wall.material;
        }
        if let Some(uv) = uv {
            o.uv = uv;
        }
        if wall.band_set.is_some() {
            o.band_set = wall.band_set;
        }
        o.solid = match (closure, wall.solid) {
            (true, true) => None,
            (true, false) => Some(false),
            (false, true) => Some(true),
            (false, false) => None,
        };
    });
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
