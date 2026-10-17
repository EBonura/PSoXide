//! Caps, validation and the budget readout for grid worlds.
//!
//! Every limit lives here and nowhere else (plan decision D6). The editor's
//! budget readout, the cook and the runtime pack all read these constants and
//! call [`validate`], so an oversize world fails loudly in the editor instead
//! of rendering empty at runtime, which is what the deleted room streamer did
//! when its cook and runtime caps drifted apart.
//!
//! Caps bind a room, never the world: rooms stream, so only the rooms near
//! the player have to fit memory (`docs/grid/streaming-design.md` section 4).
//!
//! Where a number comes from is stated next to it. The face and extent caps
//! are the limits of the grid pack the renderer prototype reads (`GRD1` v1:
//! vertices indexed by `u16`, positions stored as `i16` after the divide by
//! 16 from editor to engine units). The cook phase replaces the face cap with
//! an exact vertex count; until then the conservative bound below stands.

use super::bands::{MAX_BAND_LINES, MAX_BAND_SETS};
use super::overrides::WallMode;
use super::plane::{PlaneCollision, PlaneShape, MAX_PLANE_SIZE, MIN_PLANE_SIZE};
use super::room::GridRoom;
use super::sector::WallEdge;
use super::units::GridScale;
use super::world::GridWorld;

/// Most rooms in one world: the format's room id limit
/// ([`psx_grid::MAX_ROOMS`]). A world is a directory of rooms and only the
/// rooms near the player need to be resident, so the count is no budget.
pub const MAX_ROOMS: usize = psx_grid::MAX_ROOMS;
/// Most sectors along either side of a room. Matches the bonnie-32 validator.
pub const MAX_ROOM_SIDE: usize = 128;
/// Most wall segments stacked on one sector edge. Matches the bonnie-32
/// validator (its wall tool stops at 3).
pub const MAX_WALLS_PER_EDGE: usize = 16;
/// Most free planes one room owns. Each collidable plane is addressed by a
/// `u8` id in the collision table and the renderer tests every plane box of
/// a room each frame, so the count is kept small on purpose.
pub const MAX_PLANES_PER_ROOM: usize = 64;
/// Largest absolute height or position, grid units. Matches bonnie-32's
/// coordinate bound.
pub const MAX_COORD_UNITS: i32 = 1_000_000;
/// Editor units per engine unit; the cook divides every length by this.
pub const EDITOR_UNITS_PER_ENGINE_UNIT: i32 = 16;
/// Largest absolute coordinate in editor units that still fits the pack's
/// `i16` positions after the divide by [`EDITOR_UNITS_PER_ENGINE_UNIT`].
pub const MAX_EDITOR_COORD: i32 = i16::MAX as i32 * EDITOR_UNITS_PER_ENGINE_UNIT;
/// Most drawable faces in one room: a room chunk indexes its vertices and
/// quads with `u16` and a face has at most four, so this bound can never
/// overflow an index. It binds a room (one streamable unit), not the world.
pub const MAX_ROOM_FACES: usize = u16::MAX as usize / 4;

/// How serious an issue is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Worth showing; the world still cooks.
    Warning,
    /// The world must not cook.
    Error,
}

/// What is wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GridIssueKind {
    /// The scale has a non-positive term.
    InvalidScale(GridScale),
    /// More than [`MAX_ROOMS`] rooms.
    TooManyRooms {
        /// Rooms present.
        count: usize,
    },
    /// A room has a side of zero or beyond [`MAX_ROOM_SIDE`].
    RoomSize {
        /// Declared width.
        width: usize,
        /// Declared depth.
        depth: usize,
    },
    /// The sector list does not hold `width * depth` entries.
    StorageMismatch {
        /// Entries present.
        stored: usize,
        /// Entries expected.
        expected: usize,
    },
    /// An edge holds more than [`MAX_WALLS_PER_EDGE`] walls.
    TooManyWalls {
        /// The edge.
        edge: WallEdge,
        /// Walls present.
        count: usize,
    },
    /// A height or room offset beyond [`MAX_COORD_UNITS`].
    CoordinateRange {
        /// The offending value, grid units.
        value: i32,
    },
    /// The world, scaled to editor units, leaves the pack's `i16` range.
    ExtentTooLarge {
        /// Largest absolute coordinate, editor units.
        coordinate: i32,
    },
    /// More than [`MAX_ROOM_FACES`] drawable faces in one room.
    TooManyFaces {
        /// Faces present.
        count: usize,
    },
    /// A wall whose top is not above its bottom at either end.
    DegenerateWall {
        /// The edge.
        edge: WallEdge,
        /// Index in the edge's stack.
        index: usize,
    },
    /// Two walls on one edge with identical corners and facing.
    DuplicateWall {
        /// The edge.
        edge: WallEdge,
        /// Index of the second copy.
        index: usize,
    },
    /// An authored wall inside a derived face on the same edge and side; the
    /// two would draw on top of each other.
    AuthoredDuplicatesDerived {
        /// The edge.
        edge: WallEdge,
        /// Index in the edge's stack.
        index: usize,
    },
    /// More than [`MAX_BAND_SETS`] band sets in a room.
    TooManyBandSets {
        /// Sets present.
        count: usize,
    },
    /// A band set that is not one more band than lines, or has more than
    /// [`MAX_BAND_LINES`] lines. Its walls draw plain.
    BadBandSet {
        /// Index in the room's set list.
        index: usize,
    },
    /// A wall, override or room default naming a band set that does not
    /// exist. The wall draws plain.
    BandSetMissing {
        /// The index named.
        index: u8,
    },
    /// More than [`MAX_PLANES_PER_ROOM`] free planes in a room.
    TooManyPlanes {
        /// Planes present.
        count: usize,
    },
    /// A plane whose width or height is outside the allowed range.
    PlaneSize {
        /// Index in the room's plane list.
        index: usize,
        /// Width and height present.
        size: [i32; 2],
    },
    /// A plane centre beyond [`MAX_COORD_UNITS`].
    PlaneCoordinate {
        /// Index in the room's plane list.
        index: usize,
        /// The offending value, grid units.
        value: i32,
    },
    /// A plane that reaches outside its room's sector rectangle, where the
    /// render box and the collision table cannot hold it.
    PlaneOutsideRoom {
        /// Index in the room's plane list.
        index: usize,
    },
    /// A walkable plane steeper than the character can stand on; it cooks
    /// as solid.
    PlaneTooSteep {
        /// Index in the room's plane list.
        index: usize,
    },
    /// Rough jitter larger than a third of the character's radius: the rock
    /// can clip the model and the camera, and an outward offset is an
    /// invisible wall that size.
    RoughTooLarge {
        /// The largest amplitude, runtime units, rounded up.
        amp_ru: i32,
    },
}

impl GridIssueKind {
    /// Whether the issue blocks the cook.
    pub const fn severity(&self) -> Severity {
        match self {
            Self::DegenerateWall { .. }
            | Self::DuplicateWall { .. }
            | Self::AuthoredDuplicatesDerived { .. }
            | Self::BadBandSet { .. }
            | Self::BandSetMissing { .. }
            | Self::PlaneTooSteep { .. }
            | Self::RoughTooLarge { .. } => Severity::Warning,
            _ => Severity::Error,
        }
    }
}

/// One validation finding with its location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GridIssue {
    /// What is wrong.
    pub kind: GridIssueKind,
    /// Room index, when the issue belongs to a room.
    pub room: Option<usize>,
    /// Local sector `[x, z]`, when the issue belongs to a sector.
    pub cell: Option<[usize; 2]>,
}

impl GridIssue {
    /// Severity of the issue.
    pub const fn severity(&self) -> Severity {
        self.kind.severity()
    }

    /// One-line description for logs and the status bar.
    pub fn describe(&self) -> String {
        let place = match (self.room, self.cell) {
            (Some(r), Some([x, z])) => format!("room {r} sector {x},{z}: "),
            (Some(r), None) => format!("room {r}: "),
            _ => String::new(),
        };
        let text = match &self.kind {
            GridIssueKind::InvalidScale(s) => format!("scale {}/{} must be positive", s.num, s.den),
            GridIssueKind::TooManyRooms { count } => {
                format!("{count} rooms, at most {MAX_ROOMS}")
            }
            GridIssueKind::RoomSize { width, depth } => {
                format!("size {width}x{depth}, each side must be 1 to {MAX_ROOM_SIDE}")
            }
            GridIssueKind::StorageMismatch { stored, expected } => {
                format!("{stored} sector slots stored, {expected} expected")
            }
            GridIssueKind::TooManyWalls { edge, count } => format!(
                "{count} walls on the {} edge, at most {MAX_WALLS_PER_EDGE}",
                edge.label()
            ),
            GridIssueKind::CoordinateRange { value } => {
                format!("value {value} is beyond +/-{MAX_COORD_UNITS} grid units")
            }
            GridIssueKind::ExtentTooLarge { coordinate } => format!(
                "coordinate {coordinate} editor units is beyond +/-{MAX_EDITOR_COORD}, the cooked pack range"
            ),
            GridIssueKind::TooManyFaces { count } => {
                format!("{count} faces, at most {MAX_ROOM_FACES} in one room; split the room")
            }
            GridIssueKind::DegenerateWall { edge, index } => {
                format!("{} wall {index} has no height", edge.label())
            }
            GridIssueKind::DuplicateWall { edge, index } => {
                format!("{} wall {index} duplicates an earlier wall", edge.label())
            }
            GridIssueKind::AuthoredDuplicatesDerived { edge, index } => format!(
                "{} wall {index} lies inside a derived wall and would draw on top of it",
                edge.label()
            ),
            GridIssueKind::TooManyBandSets { count } => {
                format!("{count} band sets, at most {MAX_BAND_SETS}")
            }
            GridIssueKind::BadBandSet { index } => format!(
                "band set {index} needs one more band than lines and at most {MAX_BAND_LINES} lines; its walls draw plain"
            ),
            GridIssueKind::BandSetMissing { index } => {
                format!("a wall names band set {index}, which does not exist; it draws plain")
            }
            GridIssueKind::TooManyPlanes { count } => {
                format!("{count} planes, at most {MAX_PLANES_PER_ROOM}")
            }
            GridIssueKind::PlaneSize { index, size } => format!(
                "plane {index} is {}x{}, each side must be {MIN_PLANE_SIZE} to {MAX_PLANE_SIZE}",
                size[0], size[1]
            ),
            GridIssueKind::PlaneCoordinate { index, value } => {
                format!("plane {index} value {value} is beyond +/-{MAX_COORD_UNITS} grid units")
            }
            GridIssueKind::PlaneOutsideRoom { index } => format!(
                "plane {index} reaches outside the room; move it in or grow the room"
            ),
            GridIssueKind::PlaneTooSteep { index } => {
                format!("plane {index} is walkable but too steep to stand on; it blocks instead")
            }
            GridIssueKind::RoughTooLarge { amp_ru } => format!(
                "rough jitter of up to {amp_ru} runtime units is more than a third of the character's radius ({}); the rock can clip the model and the camera",
                super::rough::MAX_AMP_RU
            ),
        };
        format!("{place}{text}")
    }
}

/// Whether any issue blocks the cook.
pub fn has_errors(issues: &[GridIssue]) -> bool {
    issues.iter().any(|i| i.severity() == Severity::Error)
}

/// Counts for the editor's budget readout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GridBudget {
    /// Rooms.
    pub rooms: usize,
    /// Sectors that exist.
    pub sectors: usize,
    /// Floor surfaces.
    pub floors: usize,
    /// Ceiling surfaces.
    pub ceilings: usize,
    /// Wall segments.
    pub walls: usize,
    /// Free planes.
    pub planes: usize,
    /// Drawable faces after triangulation and wall banding, all rooms.
    pub faces: usize,
    /// Faces of the busiest room, the figure [`MAX_ROOM_FACES`] caps.
    pub largest_room_faces: usize,
}

impl GridBudget {
    /// Count a world.
    pub fn of(world: &GridWorld) -> Self {
        let per_room = super::faces::room_face_counts(world);
        let mut budget = Self {
            rooms: world.rooms.len(),
            faces: per_room.iter().sum(),
            largest_room_faces: per_room.iter().copied().max().unwrap_or(0),
            ..Self::default()
        };
        for room in &world.rooms {
            budget.planes += room.planes.len();
            for (_, _, sector) in room.iter() {
                budget.sectors += 1;
                budget.floors += usize::from(sector.floor.is_some());
                budget.ceilings += usize::from(sector.ceiling.is_some());
                budget.walls += sector.wall_count();
            }
        }
        budget
    }

    /// The busiest room's faces as a fraction of [`MAX_ROOM_FACES`], percent.
    pub fn faces_percent(&self) -> usize {
        self.largest_room_faces * 100 / MAX_ROOM_FACES
    }
}

fn check_planes(room_index: usize, room: &GridRoom, issues: &mut Vec<GridIssue>) {
    let at_room = |kind| GridIssue {
        kind,
        room: Some(room_index),
        cell: None,
    };
    if room.planes.len() > MAX_PLANES_PER_ROOM {
        issues.push(at_room(GridIssueKind::TooManyPlanes {
            count: room.planes.len(),
        }));
    }
    for (index, plane) in room.planes.iter().enumerate() {
        if plane
            .size
            .iter()
            .any(|s| !(MIN_PLANE_SIZE..=MAX_PLANE_SIZE).contains(s))
        {
            issues.push(at_room(GridIssueKind::PlaneSize {
                index,
                size: plane.size,
            }));
            continue;
        }
        if let Some(&value) = plane
            .center
            .iter()
            .find(|v| v.unsigned_abs() > MAX_COORD_UNITS as u32)
        {
            issues.push(at_room(GridIssueKind::PlaneCoordinate { index, value }));
            continue;
        }
        if !plane.enabled {
            continue;
        }
        let (lo, hi) = plane.cell_span();
        let inside = room.local(lo).is_some() && room.local(hi).is_some();
        if !inside {
            issues.push(at_room(GridIssueKind::PlaneOutsideRoom { index }));
        }
        if plane.collision == PlaneCollision::Walkable && plane.shape() != PlaneShape::Platform {
            issues.push(at_room(GridIssueKind::PlaneTooSteep { index }));
        }
    }
}

fn check_bands(room_index: usize, room: &GridRoom, issues: &mut Vec<GridIssue>) {
    let at_room = |kind| GridIssue {
        kind,
        room: Some(room_index),
        cell: None,
    };
    if room.band_sets.len() > MAX_BAND_SETS {
        issues.push(at_room(GridIssueKind::TooManyBandSets {
            count: room.band_sets.len(),
        }));
    }
    for (index, set) in room.band_sets.iter().enumerate() {
        if !set.is_valid() {
            issues.push(at_room(GridIssueKind::BadBandSet { index }));
        }
    }
    let mut named: Vec<u8> = room.defaults.bands.into_iter().collect();
    named.extend(room.edge_overrides.iter().filter_map(|(_, o)| o.band_set));
    for (_, _, sector) in room.iter() {
        for edge in WallEdge::ALL {
            named.extend(sector.walls(edge).iter().filter_map(|w| w.band_set));
        }
    }
    named.sort_unstable();
    named.dedup();
    for index in named {
        if index != super::bands::NO_BANDS && usize::from(index) >= room.band_sets.len() {
            issues.push(at_room(GridIssueKind::BandSetMissing { index }));
        }
    }
}

fn check_room(room_index: usize, room: &GridRoom, issues: &mut Vec<GridIssue>) {
    let at_room = |kind| GridIssue {
        kind,
        room: Some(room_index),
        cell: None,
    };
    if room.width == 0
        || room.depth == 0
        || room.width > MAX_ROOM_SIDE
        || room.depth > MAX_ROOM_SIDE
    {
        issues.push(at_room(GridIssueKind::RoomSize {
            width: room.width,
            depth: room.depth,
        }));
    }
    let expected = room.width.saturating_mul(room.depth);
    if room.sectors.len() != expected {
        issues.push(at_room(GridIssueKind::StorageMismatch {
            stored: room.sectors.len(),
            expected,
        }));
    }
    for value in [room.origin[0], room.origin[1], room.y_offset] {
        if value.unsigned_abs() > MAX_COORD_UNITS as u32 {
            issues.push(at_room(GridIssueKind::CoordinateRange { value }));
        }
    }
    check_planes(room_index, room, issues);
    check_bands(room_index, room, issues);
    for (x, z, sector) in room.iter() {
        let at = |kind| GridIssue {
            kind,
            room: Some(room_index),
            cell: Some([x, z]),
        };
        for surface in [&sector.floor, &sector.ceiling].into_iter().flatten() {
            let heights = surface
                .heights
                .iter()
                .chain(surface.heights_b.iter().flatten());
            for &value in heights {
                if value.unsigned_abs() > MAX_COORD_UNITS as u32 {
                    issues.push(at(GridIssueKind::CoordinateRange { value }));
                }
            }
        }
        for edge in WallEdge::ALL {
            let walls = sector.walls(edge);
            if walls.len() > MAX_WALLS_PER_EDGE {
                issues.push(at(GridIssueKind::TooManyWalls {
                    edge,
                    count: walls.len(),
                }));
            }
            for (index, wall) in walls.iter().enumerate() {
                for &value in &wall.heights {
                    if value.unsigned_abs() > MAX_COORD_UNITS as u32 {
                        issues.push(at(GridIssueKind::CoordinateRange { value }));
                    }
                }
                if wall.is_degenerate() {
                    issues.push(at(GridIssueKind::DegenerateWall { edge, index }));
                }
                if walls[..index]
                    .iter()
                    .any(|e| e.heights == wall.heights && e.facing == wall.facing)
                {
                    issues.push(at(GridIssueKind::DuplicateWall { edge, index }));
                }
                if room.wall_mode == WallMode::Derived
                    && !edge.is_diagonal()
                    && super::derive::duplicates_derived(room, room.world_cell(x, z), edge, wall)
                {
                    issues.push(at(GridIssueKind::AuthoredDuplicatesDerived { edge, index }));
                }
            }
        }
    }
}

/// Check a world against every cap. Returns all findings, errors first.
pub fn validate(world: &GridWorld) -> Vec<GridIssue> {
    let mut issues = Vec::new();
    let world_issue = |kind| GridIssue {
        kind,
        room: None,
        cell: None,
    };
    if !world.scale.is_valid() {
        issues.push(world_issue(GridIssueKind::InvalidScale(world.scale)));
    }
    if world.rooms.len() > MAX_ROOMS {
        issues.push(world_issue(GridIssueKind::TooManyRooms {
            count: world.rooms.len(),
        }));
    }
    for (index, room) in world.rooms.iter().enumerate() {
        check_room(index, room, &mut issues);
        if world.scale.is_valid() && room.rough.jitters() {
            // One runtime unit is 16 editor units; an editor unit is
            // `den / num` grid units.
            let largest = i64::from(room.rough.amp.iter().copied().max().unwrap_or(0));
            let ru = (largest * i64::from(world.scale.num) + 16 * i64::from(world.scale.den) - 1)
                / (16 * i64::from(world.scale.den));
            if ru > i64::from(super::rough::MAX_AMP_RU) {
                issues.push(GridIssue {
                    kind: GridIssueKind::RoughTooLarge { amp_ru: ru as i32 },
                    room: Some(index),
                    cell: None,
                });
            }
        }
    }
    // Everything below needs sane storage and scale to compute.
    if !has_errors(&issues) {
        if let Some(bounds) = world.editor_bounds() {
            let worst = bounds
                .min
                .iter()
                .chain(bounds.max.iter())
                .map(|v| v.unsigned_abs())
                .max()
                .unwrap_or(0);
            if worst > MAX_EDITOR_COORD as u32 {
                issues.push(world_issue(GridIssueKind::ExtentTooLarge {
                    coordinate: worst.min(i32::MAX as u32) as i32,
                }));
            }
        }
        for (index, faces) in super::faces::room_face_counts(world)
            .into_iter()
            .enumerate()
        {
            if faces > MAX_ROOM_FACES {
                issues.push(GridIssue {
                    kind: GridIssueKind::TooManyFaces { count: faces },
                    room: Some(index),
                    cell: None,
                });
            }
        }
    }
    issues.sort_by_key(|i| core::cmp::Reverse(i.severity()));
    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::surface::{GridSurface, GridWall};

    fn world_with_room(room: GridRoom) -> GridWorld {
        GridWorld {
            scale: GridScale::DEFAULT,
            rooms: vec![room],
            ..GridWorld::default()
        }
    }

    fn kinds(world: &GridWorld) -> Vec<GridIssueKind> {
        validate(world).into_iter().map(|i| i.kind).collect()
    }

    #[test]
    fn a_small_world_is_clean() {
        let mut room = GridRoom::new("r", 2, 2);
        room.sector_or_insert(0, 0).unwrap().floor = Some(GridSurface::flat(0, None));
        assert!(validate(&world_with_room(room)).is_empty());
    }

    #[test]
    fn room_size_caps_are_enforced() {
        let wide = GridRoom::new("wide", MAX_ROOM_SIDE + 1, 1);
        assert!(kinds(&world_with_room(wide))
            .iter()
            .any(|k| matches!(k, GridIssueKind::RoomSize { .. })));
        let empty = GridRoom::new("none", 0, 4);
        assert!(kinds(&world_with_room(empty))
            .iter()
            .any(|k| matches!(k, GridIssueKind::RoomSize { .. })));
        let max = GridRoom::new("max", MAX_ROOM_SIDE, MAX_ROOM_SIDE);
        assert!(!kinds(&world_with_room(max))
            .iter()
            .any(|k| matches!(k, GridIssueKind::RoomSize { .. })));
    }

    #[test]
    fn storage_length_must_match_the_declared_size() {
        let mut room = GridRoom::new("r", 3, 3);
        room.sectors.pop();
        let found = kinds(&world_with_room(room));
        assert_eq!(
            found,
            vec![GridIssueKind::StorageMismatch {
                stored: 8,
                expected: 9
            }]
        );
    }

    #[test]
    fn wall_stack_cap_and_degenerate_walls() {
        let mut room = GridRoom::new("r", 1, 1);
        let sector = room.sector_or_insert(0, 0).unwrap();
        for i in 0..=MAX_WALLS_PER_EDGE {
            sector
                .north
                .push(GridWall::span(i as i32 * 256, i as i32 * 256 + 256, None));
        }
        sector.east.push(GridWall::span(512, 512, None));
        let found = kinds(&world_with_room(room));
        assert!(found.contains(&GridIssueKind::TooManyWalls {
            edge: WallEdge::North,
            count: MAX_WALLS_PER_EDGE + 1
        }));
        assert!(found.contains(&GridIssueKind::DegenerateWall {
            edge: WallEdge::East,
            index: 0
        }));
    }

    #[test]
    fn duplicate_walls_warn_but_do_not_block() {
        let mut room = GridRoom::new("r", 1, 1);
        let sector = room.sector_or_insert(0, 0).unwrap();
        sector.south.push(GridWall::span(0, 256, None));
        sector.south.push(GridWall::span(0, 256, None));
        let issues = validate(&world_with_room(room));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity(), Severity::Warning);
        assert!(!has_errors(&issues));
    }

    #[test]
    fn errors_sort_before_warnings() {
        let mut room = GridRoom::new("r", 1, 1);
        let sector = room.sector_or_insert(0, 0).unwrap();
        sector.south.push(GridWall::span(0, 0, None));
        sector.floor = Some(GridSurface::flat(MAX_COORD_UNITS + 1, None));
        let issues = validate(&world_with_room(room));
        assert!(issues.len() >= 2);
        assert_eq!(issues[0].severity(), Severity::Error);
        assert_eq!(issues.last().unwrap().severity(), Severity::Warning);
    }

    #[test]
    fn bad_scale_and_room_count_are_world_level() {
        let mut world = GridWorld::new();
        world.scale = GridScale { num: 0, den: 16 };
        world.rooms = (0..=MAX_ROOMS).map(|_| GridRoom::new("r", 1, 1)).collect();
        let found = kinds(&world);
        assert!(found
            .iter()
            .any(|k| matches!(k, GridIssueKind::InvalidScale(_))));
        assert!(found.contains(&GridIssueKind::TooManyRooms {
            count: MAX_ROOMS + 1
        }));
    }

    #[test]
    fn extent_beyond_the_pack_range_is_an_error() {
        let mut room = GridRoom::new("far", 1, 1);
        room.origin = [MAX_EDITOR_COORD / 1280 + 8, 0];
        room.sector_or_insert(0, 0).unwrap().floor = Some(GridSurface::flat(0, None));
        let found = kinds(&world_with_room(room));
        assert!(found
            .iter()
            .any(|k| matches!(k, GridIssueKind::ExtentTooLarge { .. })));
    }

    #[test]
    fn budget_counts_surfaces_and_faces() {
        let mut room = GridRoom::new("r", 2, 1);
        room.sector_or_insert(0, 0).unwrap().floor = Some(GridSurface::flat(0, None));
        let second = room.sector_or_insert(1, 0).unwrap();
        second.floor = Some(GridSurface::flat(0, None));
        second.ceiling = Some(GridSurface::flat(1024, None));
        second.west.push(GridWall::span(0, 1024, None));
        let budget = GridBudget::of(&world_with_room(room));
        assert_eq!(budget.rooms, 1);
        assert_eq!(budget.sectors, 2);
        assert_eq!(budget.floors, 2);
        assert_eq!(budget.ceilings, 1);
        assert_eq!(budget.walls, 1);
        assert_eq!(budget.faces, 4);
    }
}
