//! Walkability analysis: where a character can stand and walk, told with
//! the character motor's own numbers.
//!
//! The analysis reads the document through the same grid to runtime rounding
//! and the same triangle normal the cook uses, and compares with the one
//! shared [`psx_engine::WalkLimits`] (step-up, step-down, body height, steepest
//! floor), so what the overlay shows is what the motor does. It reports, per
//! sector, a floor too steep to stand on and headroom lower than the body; per
//! edge, a step taller than the body climbs and a drop to nothing; and, from
//! the player's spawn, which walkable sectors can be reached at all.
//!
//! Scope, stated plainly: edges are judged within one room (a doorway between
//! two rooms joins them for reachability only), a solid wall record on an edge
//! blocks it whatever its height, and the body is a point (the motor's
//! five-point footprint is not modelled).

use std::collections::VecDeque;

pub use psx_engine::WalkLimits;

use super::cook::RuntimeUnits;
use super::derive::{physical_edge_walls, CARDINAL};
use super::room::GridRoom;
use super::sector::{GridSector, WallEdge};
use super::slope::{surface_class, SlopeClass};
use super::surface::GridSurface;
use super::units::SECTOR_UNITS;
use super::world::GridWorld;

/// Why an edge is flagged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeKind {
    /// Two walkable floors differ by more than the body steps up.
    StepTooTall,
    /// A walkable floor ends at nothing: the body falls off.
    Drop,
}

/// One flagged edge: the east or south edge of `cell` (so each physical edge
/// is listed once), or a cardinal edge to the void.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EdgeFlag {
    /// Room index.
    pub room: usize,
    /// World sector the edge belongs to.
    pub cell: [i32; 2],
    /// Which of its edges.
    pub edge: WallEdge,
    /// Why.
    pub kind: EdgeKind,
    /// Largest height difference along the edge, runtime units (0 for a
    /// drop to nothing).
    pub delta_ru: i32,
}

/// What the analysis says of one sector with a floor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellWalk {
    /// A floor triangle steeper than the motor stands on.
    pub steep: bool,
    /// Ceiling less than the body height over the floor at a corner or the
    /// middle.
    pub low_headroom: bool,
    /// The floor is switched off (`walkable` false): a ghost floor.
    pub ghost: bool,
    /// Walkable (a floor, not steep, not ghost) and no spawn can reach it.
    pub unreachable: bool,
    /// Walkable and not low: a body can be here.
    pub standable: bool,
}

/// One room's sector table.
#[derive(Clone, Debug)]
pub struct RoomWalk {
    /// World sector of local `(0, 0)`.
    pub origin: [i32; 2],
    /// Sectors along X.
    pub width: usize,
    /// Sectors along Z.
    pub depth: usize,
    cells: Vec<Option<CellWalk>>,
}

impl RoomWalk {
    /// How many sectors a body can stand in.
    pub fn standable_count(&self) -> usize {
        self.cells.iter().flatten().filter(|c| c.standable).count()
    }

    /// The analysis of a world sector, `None` without a floor.
    pub fn cell(&self, world_cell: [i32; 2]) -> Option<CellWalk> {
        let x = usize::try_from(world_cell[0] - self.origin[0]).ok()?;
        let z = usize::try_from(world_cell[1] - self.origin[1]).ok()?;
        (x < self.width && z < self.depth)
            .then(|| self.cells.get(z * self.width + x).copied().flatten())
            .flatten()
    }
}

/// Where the player starts: a room and a world sector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spawn {
    /// Room index.
    pub room: usize,
    /// World sector.
    pub cell: [i32; 2],
}

/// The result of [`analyse`].
#[derive(Clone, Debug)]
pub struct WalkAnalysis {
    /// The limits the analysis used.
    pub limits: WalkLimits,
    /// Per room.
    pub rooms: Vec<RoomWalk>,
    /// Flagged edges.
    pub edges: Vec<EdgeFlag>,
    /// Whether reachability ran (it needs a spawn that stands on a floor).
    pub reach_ran: bool,
    /// Runtime units per click, for the legend.
    pub click_ru: i32,
}

impl WalkAnalysis {
    /// Counts for the legend: steep, low headroom, step too tall, drops,
    /// unreachable sectors.
    pub fn counts(&self) -> [usize; 5] {
        let mut c = [0usize; 5];
        for room in &self.rooms {
            for cell in room.cells.iter().flatten() {
                c[0] += usize::from(cell.steep);
                c[1] += usize::from(cell.low_headroom);
                c[4] += usize::from(cell.unreachable);
            }
        }
        for e in &self.edges {
            match e.kind {
                EdgeKind::StepTooTall => c[2] += 1,
                EdgeKind::Drop => c[3] += 1,
            }
        }
        c
    }
}

/// Runtime-rounded corner heights of a surface, world runtime units.
fn corners_ru(units: &RuntimeUnits, y_offset: i32, heights: [i32; 4]) -> [i32; 4] {
    heights.map(|h| units.rt(y_offset.saturating_add(h)))
}

/// The two corner heights of `surface` on the cardinal `edge`, in the world
/// direction of the edge (west to east for north and south, north to south
/// for east and west), taken from the triangle that holds the edge.
fn edge_profile(surface: &GridSurface, edge: WallEdge) -> [i32; 2] {
    // Corners [NW, NE, SE, SW] and the corner pair along each edge.
    let (pair, triangle) = match edge {
        WallEdge::North => ([0, 1], 0),
        WallEdge::South => ([3, 2], 1),
        WallEdge::East => (
            [1, 2],
            usize::from(matches!(surface.split, super::surface::GridSplit::NeSw)),
        ),
        _ => (
            [0, 3],
            usize::from(matches!(surface.split, super::surface::GridSplit::NwSe)),
        ),
    };
    let h = surface.heights_for(triangle);
    [h[pair[0]], h[pair[1]]]
}

fn sector_of(room: &GridRoom, cell: [i32; 2]) -> Option<&GridSector> {
    let [x, z] = room.local(cell)?;
    room.sector(x, z)
}

/// Whether a solid wall of either sector, or a derived one, stands on the
/// physical edge between `cell`'s `edge` side.
fn edge_blocked(room: &GridRoom, cell: [i32; 2], edge: WallEdge) -> bool {
    let solid = |sector: Option<&GridSector>, e: WallEdge| {
        sector.is_some_and(|s| s.walls(e).iter().any(|w| w.solid))
    };
    if solid(sector_of(room, cell), edge) {
        return true;
    }
    if let Some((opposite, offset)) = edge.opposite() {
        let other = [cell[0] + offset[0], cell[1] + offset[1]];
        if solid(sector_of(room, other), opposite) {
            return true;
        }
    }
    physical_edge_walls(room, cell, edge)
        .iter()
        .any(|w| w.blocks())
}

fn cell_walk(
    units: &RuntimeUnits,
    limits: &WalkLimits,
    y_offset: i32,
    sector: &GridSector,
) -> Option<CellWalk> {
    let floor = sector.floor()?;
    let class = surface_class(units, y_offset, floor, limits);
    let steep = class == SlopeClass::Steep;
    let ghost = !floor.walkable;
    let mut low = false;
    if let Some(ceiling) = sector.ceiling() {
        let f = corners_ru(units, y_offset, floor.heights);
        let c = corners_ru(units, y_offset, ceiling.heights);
        let mid = |v: [i32; 4]| v.iter().sum::<i32>() / 4;
        low = (0..4).any(|i| c[i] - f[i] < limits.body_height)
            || mid(c) - mid(f) < limits.body_height;
        if let (Some(fb), Some(cb)) = (floor.heights_b, ceiling.heights_b.or(Some(ceiling.heights)))
        {
            let (f, c) = (
                corners_ru(units, y_offset, fb),
                corners_ru(units, y_offset, cb),
            );
            low |= (0..4).any(|i| c[i] - f[i] < limits.body_height);
        }
    }
    Some(CellWalk {
        steep,
        low_headroom: low,
        ghost,
        unreachable: false,
        standable: !steep && !ghost && !low,
    })
}

/// Analyse `world` against `limits`. `None` when the world's scale cannot be
/// cooked (no runtime units to judge in).
pub fn analyse(world: &GridWorld, spawns: &[Spawn], limits: &WalkLimits) -> Option<WalkAnalysis> {
    let units = RuntimeUnits::new(world.scale).ok()?;
    let mut rooms = Vec::new();
    let mut edges = Vec::new();
    for (ri, room) in world.rooms.iter().enumerate() {
        let mut cells = vec![None; room.width * room.depth];
        for (x, z, sector) in room.iter() {
            cells[z * room.width + x] = cell_walk(&units, limits, room.y_offset, sector);
        }
        rooms.push(RoomWalk {
            origin: room.origin,
            width: room.width,
            depth: room.depth,
            cells,
        });
        flag_edges(&units, limits, ri, room, &rooms[ri], &mut edges);
    }
    let mut analysis = WalkAnalysis {
        limits: *limits,
        rooms,
        edges,
        reach_ran: false,
        click_ru: units.rt(super::units::CLICK_UNITS),
    };
    reach(world, &units, limits, spawns, &mut analysis);
    Some(analysis)
}

/// The height step, runtime units, between the two triangles of a split-level
/// floor along its fold diagonal, when it is more than the body climbs.
fn fold_step(
    units: &RuntimeUnits,
    limits: &WalkLimits,
    y_offset: i32,
    floor: &GridSurface,
) -> Option<i32> {
    floor.heights_b?;
    if floor.void != 0 {
        return None;
    }
    let [a, b] =
        super::edit::walls::edge_corner_indices(super::derive_diag::fold_edge(floor.split));
    let ru = |tri: usize| {
        let h = floor.heights_for(tri);
        [h[a], h[b]].map(|h| units.rt(y_offset.saturating_add(h)))
    };
    let (front, back) = (ru(0), ru(1));
    let delta = (front[0] - back[0]).abs().max((front[1] - back[1]).abs());
    (delta > limits.step_up).then_some(delta)
}

fn flag_edges(
    units: &RuntimeUnits,
    limits: &WalkLimits,
    ri: usize,
    room: &GridRoom,
    walk: &RoomWalk,
    out: &mut Vec<EdgeFlag>,
) {
    let cells: Vec<[i32; 2]> = room.iter().map(|(x, z, _)| room.world_cell(x, z)).collect();
    flag_edges_of(units, limits, ri, room, &cells, &|c| walk.cell(c), out);
}

/// [`flag_edges`] over some sectors, with `info` giving the per-sector result.
fn flag_edges_of(
    units: &RuntimeUnits,
    limits: &WalkLimits,
    ri: usize,
    room: &GridRoom,
    cells: &[[i32; 2]],
    info: &dyn Fn([i32; 2]) -> Option<CellWalk>,
    out: &mut Vec<EdgeFlag>,
) {
    for &cell in cells {
        let Some(sector) = sector_of(room, cell) else {
            continue;
        };
        let Some(here) = info(cell) else {
            continue;
        };
        let Some(floor) = sector.floor() else {
            continue;
        };
        if here.steep || here.ghost {
            continue;
        }
        if let Some(delta) = fold_step(units, limits, room.y_offset, floor) {
            out.push(EdgeFlag {
                room: ri,
                cell,
                edge: super::derive_diag::fold_edge(floor.split),
                kind: EdgeKind::StepTooTall,
                delta_ru: delta,
            });
        }
        for edge in CARDINAL {
            let offset = match edge {
                WallEdge::East => [1, 0],
                WallEdge::South => [0, 1],
                // North and west are some neighbour's east and south; only
                // their drops to nothing are listed from this side.
                WallEdge::North => [0, -1],
                _ => [-1, 0],
            };
            let other = [cell[0] + offset[0], cell[1] + offset[1]];
            let neighbour = sector_of(room, other).filter(|s| s.floor().is_some());
            let Some(neighbour) = neighbour else {
                if !edge_blocked(room, cell, edge) {
                    out.push(EdgeFlag {
                        room: ri,
                        cell,
                        edge,
                        kind: EdgeKind::Drop,
                        delta_ru: 0,
                    });
                }
                continue;
            };
            if !matches!(edge, WallEdge::East | WallEdge::South) {
                continue;
            }
            let (Some(nf), Some(nw)) = (neighbour.floor(), info(other)) else {
                continue;
            };
            if nw.steep || nw.ghost {
                continue;
            }
            let opposite = edge.opposite().map_or(edge, |(e, _)| e);
            let a = edge_profile(floor, edge);
            let b = edge_profile(nf, opposite);
            // The neighbour's profile runs the same world way along the edge.
            let a = a.map(|h| units.rt(room.y_offset.saturating_add(h)));
            let b = b.map(|h| units.rt(room.y_offset.saturating_add(h)));
            let delta = (a[0] - b[0]).abs().max((a[1] - b[1]).abs());
            if delta > limits.step_up && !edge_blocked(room, cell, edge) {
                out.push(EdgeFlag {
                    room: ri,
                    cell,
                    edge,
                    kind: EdgeKind::StepTooTall,
                    delta_ru: delta,
                });
            }
        }
    }
}

/// What the mask of a destructive verb counts around some sectors of a room:
/// floors too steep to stand on, headroom lower than the body, and steps the
/// body cannot climb (between sectors and across a split-level fold). The
/// same predicates [`analyse`] flags with, for the sectors and the one-ring
/// around them only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LocalProblems {
    /// Floors steeper than the motor stands on.
    pub steep: usize,
    /// Sectors with less headroom than the body.
    pub low_headroom: usize,
    /// Steps taller than the body climbs.
    pub steps: usize,
}

impl LocalProblems {
    /// Total.
    pub const fn total(self) -> usize {
        self.steep + self.low_headroom + self.steps
    }
}

/// Count the problems in and around `cells` of `room` (see
/// [`LocalProblems`]). `None` when the scale cannot be cooked.
pub fn local_problems(
    world: &GridWorld,
    room: usize,
    cells: &[[i32; 2]],
    limits: &WalkLimits,
) -> Option<LocalProblems> {
    let units = RuntimeUnits::new(world.scale).ok()?;
    let r = world.rooms.get(room)?;
    let mut ring: std::collections::BTreeSet<[i32; 2]> = std::collections::BTreeSet::new();
    for &c in cells {
        for dz in -1..=1 {
            for dx in -1..=1 {
                ring.insert([c[0] + dx, c[1] + dz]);
            }
        }
    }
    let info = |c: [i32; 2]| sector_of(r, c).and_then(|s| cell_walk(&units, limits, r.y_offset, s));
    let list: Vec<[i32; 2]> = ring.iter().copied().collect();
    let mut out = LocalProblems::default();
    for &c in &list {
        if let Some(w) = info(c) {
            out.steep += usize::from(w.steep);
            out.low_headroom += usize::from(w.low_headroom);
        }
    }
    let mut edges = Vec::new();
    flag_edges_of(&units, limits, room, r, &list, &info, &mut edges);
    out.steps = edges
        .iter()
        .filter(|e| e.kind == EdgeKind::StepTooTall)
        .count();
    Some(out)
}

/// Flood fill from the spawns. A body steps up at most the limit and falls
/// any distance; a solid wall on the edge or a low ceiling stops it.
fn reach(
    world: &GridWorld,
    units: &RuntimeUnits,
    limits: &WalkLimits,
    spawns: &[Spawn],
    analysis: &mut WalkAnalysis,
) {
    type Node = (usize, [i32; 2]);
    let standable = |a: &WalkAnalysis, (r, c): Node| {
        a.rooms
            .get(r)
            .and_then(|room| room.cell(c))
            .is_some_and(|c| c.standable)
    };
    let mut seen: std::collections::BTreeSet<Node> = std::collections::BTreeSet::new();
    let mut queue: VecDeque<Node> = VecDeque::new();
    for s in spawns {
        if standable(analysis, (s.room, s.cell)) && seen.insert((s.room, s.cell)) {
            queue.push_back((s.room, s.cell));
        }
    }
    if queue.is_empty() {
        return;
    }
    analysis.reach_ran = true;
    while let Some((ri, cell)) = queue.pop_front() {
        let Some(room) = world.rooms.get(ri) else {
            continue;
        };
        let Some(floor) = sector_of(room, cell).and_then(|s| s.floor()) else {
            continue;
        };
        for edge in CARDINAL {
            let off = match edge {
                WallEdge::North => [0, -1],
                WallEdge::East => [1, 0],
                WallEdge::South => [0, 1],
                _ => [-1, 0],
            };
            let other = [cell[0] + off[0], cell[1] + off[1]];
            if edge_blocked(room, cell, edge) {
                continue;
            }
            let here = edge_profile(floor, edge);
            let here_ru = here.map(|h| units.rt(room.y_offset.saturating_add(h)));
            // Every room with a standable floor at the neighbour that the
            // body can step onto from here.
            for (rj, other_room) in world.rooms.iter().enumerate() {
                if seen.contains(&(rj, other)) || !standable(analysis, (rj, other)) {
                    continue;
                }
                let Some(nf) = sector_of(other_room, other).and_then(|s| s.floor()) else {
                    continue;
                };
                let opposite = edge.opposite().map_or(edge, |(e, _)| e);
                let there = edge_profile(nf, opposite);
                let there_ru = there.map(|h| units.rt(other_room.y_offset.saturating_add(h)));
                // Climb at either end of the shared edge.
                let climb = (there_ru[0] - here_ru[0]).max(there_ru[1] - here_ru[1]);
                if climb <= limits.step_up {
                    seen.insert((rj, other));
                    queue.push_back((rj, other));
                }
            }
        }
    }
    for (ri, room) in analysis.rooms.iter_mut().enumerate() {
        let origin = room.origin;
        let width = room.width;
        for (i, cell) in room.cells.iter_mut().enumerate() {
            if let Some(c) = cell {
                if c.standable {
                    let world_cell = [
                        origin[0] + (i % width) as i32,
                        origin[1] + (i / width) as i32,
                    ];
                    c.unreachable = !seen.contains(&(ri, world_cell));
                }
            }
        }
    }
}

/// The player spawns of the active scene as the sectors they stand over: a
/// spawn point marked as the player's, or an entity with a player character
/// controller.
pub fn player_spawns(project: &crate::ProjectDocument, world: &GridWorld) -> Vec<Spawn> {
    use crate::NodeKind;
    let scene = project.active_scene();
    let mut out = Vec::new();
    for node in scene.nodes() {
        let is_spawn = matches!(node.kind, NodeKind::SpawnPoint { player: true, .. })
            || node.children.iter().any(|c| {
                scene.node(*c).is_some_and(|k| {
                    matches!(k.kind, NodeKind::CharacterController { player: true, .. })
                })
            });
        if is_spawn {
            if let Some(spawn) = locate_spawn(world, node.transform.translation) {
                out.push(spawn);
            }
        }
    }
    out
}

/// The world sector under an editor-space point, as `(room, sector)`: the
/// room whose floor there is nearest below (or at) `y`, else nearest.
pub fn locate_spawn(world: &GridWorld, position: [f32; 3]) -> Option<Spawn> {
    let (gx, gz) = (
        world.scale.to_grid(position[0].round() as i32),
        world.scale.to_grid(position[2].round() as i32),
    );
    let cell = [gx.div_euclid(SECTOR_UNITS), gz.div_euclid(SECTOR_UNITS)];
    let mut best: Option<(f32, usize)> = None;
    for (ri, room) in world.rooms.iter().enumerate() {
        let Some(floor) = sector_of(room, cell).and_then(|s| s.floor()) else {
            continue;
        };
        let mean = floor.heights.iter().sum::<i32>() / 4;
        let y = world.scale.to_editor(room.y_offset + mean) as f32;
        // Prefer floors at or under the spawn; a floor above is penalised.
        let score = if y <= position[1] + 64.0 {
            position[1] - y
        } else {
            (y - position[1]) * 4.0 + 1.0e6
        };
        if best.is_none_or(|(s, _)| score < s) {
            best = Some((score, ri));
        }
    }
    best.map(|(_, room)| Spawn { room, cell })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::surface::{GridSurface, GridWall};
    use crate::grid::units::GridScale;

    /// A derived row of `w` sectors with floors at 0 and ceilings at 6144,
    /// at the default scale (80 runtime units a sector, 5 per fine step).
    fn row(w: usize) -> GridWorld {
        let mut room = GridRoom::new_derived("row", w, 1);
        for x in 0..w {
            let s = room.sector_or_insert(x, 0).unwrap();
            s.set_floor(Some(GridSurface::flat(0, None)));
            s.set_ceiling(Some(GridSurface::flat(6144, None)));
        }
        GridWorld {
            scale: GridScale::DEFAULT,
            rooms: vec![room],
            ..GridWorld::default()
        }
    }

    fn floor_mut(world: &mut GridWorld, x: usize) -> &mut GridSurface {
        world.rooms[0]
            .sector_mut(x, 0)
            .unwrap()
            .floor_mut()
            .unwrap()
    }

    fn spawn_at(x: i32) -> Spawn {
        Spawn {
            room: 0,
            cell: [x, 0],
        }
    }

    fn analyse_row(world: &GridWorld, spawn: i32) -> WalkAnalysis {
        analyse(world, &[spawn_at(spawn)], &WalkLimits::DEFAULT).unwrap()
    }

    fn cell(a: &WalkAnalysis, x: i32) -> CellWalk {
        a.rooms[0].cell([x, 0]).unwrap()
    }

    #[test]
    fn a_floor_steeper_than_the_motor_stands_is_flagged_steep() {
        let mut world = row(4);
        // 85 runtime units of rise over an 80 unit sector: past 45 degrees.
        floor_mut(&mut world, 1).heights = [0, 0, 1088, 1088];
        // 80 units exactly: 45 degrees, stands.
        floor_mut(&mut world, 2).heights = [0, 0, 1024, 1024];
        let a = analyse_row(&world, 0);
        assert!(cell(&a, 1).steep && !cell(&a, 1).standable);
        assert!(!cell(&a, 2).steep && cell(&a, 2).standable);
        assert_eq!(a.counts()[0], 1);
    }

    #[test]
    fn headroom_under_the_body_height_is_flagged_low() {
        let mut world = row(3);
        // 48 runtime units is 614.4 grid units: 640 clears it, 576 does not.
        world.rooms[0]
            .sector_mut(1, 0)
            .unwrap()
            .ceiling_mut()
            .unwrap()
            .heights = [640; 4];
        world.rooms[0]
            .sector_mut(2, 0)
            .unwrap()
            .ceiling_mut()
            .unwrap()
            .heights = [576; 4];
        let a = analyse_row(&world, 0);
        assert!(!cell(&a, 1).low_headroom, "48 ru clears a 48 ru body");
        assert!(cell(&a, 2).low_headroom);
        assert!(!cell(&a, 2).standable);
        // A single low corner counts too.
        world.rooms[0]
            .sector_mut(1, 0)
            .unwrap()
            .ceiling_mut()
            .unwrap()
            .heights = [640, 640, 640, 512];
        assert!(cell(&analyse_row(&world, 0), 1).low_headroom);
    }

    #[test]
    fn a_step_over_the_limit_is_flagged_and_one_at_the_limit_is_not() {
        let mut world = row(4);
        // 512 grid units is 40 runtime units: exactly the step-up limit.
        floor_mut(&mut world, 1).heights = [512; 4];
        // 576 is 45: too tall.
        floor_mut(&mut world, 3).heights = [576; 4];
        let a = analyse_row(&world, 0);
        let steps: Vec<(i32, i32)> = a
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::StepTooTall)
            .map(|e| (e.cell[0], e.delta_ru))
            .collect();
        // Cell 1 (at the limit) is not flagged against 0; cell 2 (0) against
        // cell 3 (45 ru) is, from cell 2's east edge.
        assert_eq!(steps, vec![(2, 45)], "{:?}", a.edges);
    }

    #[test]
    fn the_limit_is_the_shared_one() {
        let a = analyse_row(&row(2), 0);
        assert_eq!(a.limits, WalkLimits::DEFAULT);
        assert_eq!(a.click_ru, 20);
    }

    #[test]
    fn a_walkable_floor_that_ends_in_nothing_is_a_drop_unless_a_wall_closes_it() {
        let mut world = row(3);
        world.rooms[0].sector_mut(2, 0).unwrap().set_floor(None);
        let a = analyse_row(&world, 0);
        let drops: Vec<[i32; 2]> = a
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Drop)
            .map(|e| e.cell)
            .collect();
        // Cell 1's east edge leads off the floor. The row's other open edges
        // (north and south of every cell) are closed by derived walls.
        assert_eq!(drops, vec![[1, 0]], "{:?}", a.edges);
        // A solid authored wall on that edge closes it.
        world.rooms[0]
            .sector_mut(1, 0)
            .unwrap()
            .east
            .push(GridWall::span(0, 2048, None));
        assert!(analyse_row(&world, 0)
            .edges
            .iter()
            .all(|e| e.kind != EdgeKind::Drop));
    }

    #[test]
    fn reachability_hatches_a_walkable_island_and_follows_one_way_drops() {
        // Floors 0 to 2, a void at 3, floors 4 to 6: the far part is an island.
        let mut world = row(7);
        world.rooms[0].sector_mut(3, 0).unwrap().set_floor(None);
        let a = analyse_row(&world, 0);
        assert!(a.reach_ran);
        for x in 0..3 {
            assert!(!cell(&a, x).unreachable, "{x}");
        }
        for x in 4..7 {
            assert!(cell(&a, x).unreachable, "{x}");
        }
        assert_eq!(a.counts()[4], 3);
        // From the island the first part is the island's problem too.
        assert!(cell(&analyse_row(&world, 5), 0).unreachable);
        // A tall step is a one way trip: climb it and you cannot, drop off it
        // and you can.
        let mut stepped = row(4);
        for x in 2..4 {
            floor_mut(&mut stepped, x).heights = [1024; 4];
        }
        let from_low = analyse_row(&stepped, 0);
        assert!(cell(&from_low, 2).unreachable && cell(&from_low, 3).unreachable);
        let from_high = analyse_row(&stepped, 3);
        assert!(!cell(&from_high, 0).unreachable, "falling is allowed");
        // A solid wall between two floors stops the walk either way.
        let mut walled = row(3);
        walled.rooms[0]
            .sector_mut(1, 0)
            .unwrap()
            .east
            .push(GridWall::span(0, 2048, None));
        assert!(cell(&analyse_row(&walled, 0), 2).unreachable);
    }

    #[test]
    fn a_ghost_floor_is_not_standable_and_is_never_called_unreachable() {
        let mut world = row(3);
        floor_mut(&mut world, 1).walkable = false;
        let a = analyse_row(&world, 0);
        assert!(cell(&a, 1).ghost && !cell(&a, 1).unreachable);
        // The cells beyond a ghost floor are reached only through it, which
        // the body falls through: they count as unreachable.
        assert!(cell(&a, 2).unreachable);
    }

    #[test]
    fn with_no_standable_spawn_reachability_does_not_run() {
        let a = analyse(&row(3), &[], &WalkLimits::DEFAULT).unwrap();
        assert!(!a.reach_ran);
        assert!(a.rooms[0].cell([2, 0]).is_some_and(|c| !c.unreachable));
    }

    #[test]
    fn a_spawn_is_found_on_the_floor_it_stands_over() {
        let mut world = row(3);
        floor_mut(&mut world, 1).heights = [0; 4];
        let at = |x: f32, y: f32| locate_spawn(&world, [x, y, 640.0]).unwrap();
        // Scale 5/4: a sector is 1280 editor units.
        assert_eq!(at(1920.0, 10.0).cell, [1, 0]);
        assert_eq!(at(10.0, 10.0).cell, [0, 0]);
        assert!(locate_spawn(&world, [99_999.0, 0.0, 0.0]).is_none());
    }
}
