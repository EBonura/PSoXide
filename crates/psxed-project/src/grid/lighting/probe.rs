//! The collision world the light bake shadows through.
//!
//! The runtime's own line of sight decides what blocks a lamp, so the bake and
//! the walkable world agree about what is solid. The cook builds the
//! collision tables anyway; the editor preview builds them here, from the
//! document alone, so both ask the same question of the same tables.

use psx_grid::write::{aligned_copy, words_as_bytes, LevelCore, RoomChunk};
use psx_grid::{CollisionView, LevelView, ResidentRooms, RoomRecord, ROOM_SPANS, ROOM_TREE};

use crate::grid::cook::collision::{self, Tables};
use crate::grid::cook::units::RuntimeUnits;
use crate::grid::world::GridWorld;

/// Origin of each room in runtime units, `[x, y, z]`, as the cook places it.
pub fn room_origins(world: &GridWorld, units: &RuntimeUnits) -> Vec<[i32; 3]> {
    let s = units.sector() as i32;
    world
        .rooms
        .iter()
        .map(|room| {
            [
                room.origin[0] * s,
                units.rt(room.y_offset),
                room.origin[1] * s,
            ]
        })
        .collect()
}

/// Collision tables of every room, in room order. Whether a plane lets the
/// camera through only matters to the camera arm, never to a shadow ray, so
/// the light bake builds them with every plane solid to the camera too.
pub fn collision_tables(world: &GridWorld, units: &RuntimeUnits) -> Result<Vec<Tables>, String> {
    world
        .rooms
        .iter()
        .enumerate()
        .map(|(index, room)| {
            collision::build_with_meshes(units, room, &world.mesh_assets, index, &mut |_| false)
        })
        .collect()
}

/// Cooked collision of a whole world, owned, ready to answer line-of-sight
/// queries from any thread.
#[derive(Clone, Debug)]
pub struct CollisionProbe {
    core: Vec<u32>,
    core_len: usize,
    chunks: Vec<Vec<u32>>,
}

impl CollisionProbe {
    /// Serialise `cols` (one table set per room of `world`, placed at
    /// `origins`) the way a cooked level holds them.
    pub fn new(
        world: &GridWorld,
        units: &RuntimeUnits,
        origins: &[[i32; 3]],
        cols: &[Tables],
    ) -> Result<Self, String> {
        let core = LevelCore {
            sector_units: units.sector() as u16,
            rooms: world
                .rooms
                .iter()
                .zip(cols)
                .zip(origins)
                .map(|((room, col), origin)| {
                    let (lo, hi) = col.range.unwrap_or((0, 0));
                    RoomRecord {
                        origin: *origin,
                        width: room.width as u8,
                        depth: room.depth as u8,
                        min_y: lo as i16,
                        max_y: hi as i16,
                        flags: (if col.tree.is_some() { ROOM_TREE } else { 0 })
                            | (if col.spans.is_some() { ROOM_SPANS } else { 0 }),
                        ..RoomRecord::default()
                    }
                })
                .collect(),
            ..LevelCore::default()
        };
        let chunks = world
            .rooms
            .iter()
            .zip(cols)
            .enumerate()
            .map(|(index, (room, col))| {
                let (lo, hi) = col.range.unwrap_or((0, 0));
                let chunk = RoomChunk {
                    room_id: index as u16,
                    width: room.width as u8,
                    depth: room.depth as u8,
                    min_y: lo as i16,
                    max_y: hi as i16,
                    sectors: col.sectors.clone(),
                    tree: col.tree.clone(),
                    spans: col.spans.clone(),
                    exts: col.exts.clone(),
                    walls: col.walls.clone(),
                    splitbs: col.splitbs.clone(),
                    col_planes: col.planes.clone(),
                    ..RoomChunk::default()
                };
                aligned_copy(&chunk.to_bytes())
            })
            .collect();
        let bytes = core.to_bytes();
        let probe = Self {
            core_len: bytes.len(),
            core: aligned_copy(&bytes),
            chunks,
        };
        // Fail now, not on the first query.
        probe.with_session(|_| ())?;
        Ok(probe)
    }

    /// Run `f` with a line-of-sight query over this world: `true` when the
    /// segment between two points (runtime units) is blocked by solid world.
    /// The collision world is parsed once per call, so batch the queries.
    pub fn with_occlusion<R>(
        &self,
        f: impl FnOnce(&dyn Fn([f64; 3], [f64; 3]) -> bool) -> R,
    ) -> Result<R, String> {
        self.with_session(|set| {
            let collision = set.collision();
            let round = |p: [f64; 3]| p.map(|v| v.round() as i32);
            f(&|from, to| !collision.line_of_sight(round(from), round(to)))
        })
    }

    /// Run `f` with the collision world.
    pub fn with_collision<R>(
        &self,
        f: impl FnOnce(&psx_grid::collision::GridCollision<'_>) -> R,
    ) -> Result<R, String> {
        self.with_session(|set| f(&set.collision()))
    }

    fn with_session<R>(&self, f: impl FnOnce(&ResidentRooms<'_>) -> R) -> Result<R, String> {
        let level = LevelView::parse(words_as_bytes(&self.core, self.core_len))
            .map_err(|e| format!("collision probe core: {e:?}"))?;
        let views: Vec<Option<CollisionView<'_>>> = self
            .chunks
            .iter()
            .map(|words| CollisionView::parse(words_as_bytes(words, words.len() * 4)).ok())
            .collect();
        let set = ResidentRooms::from_collisions(level, &views)
            .ok_or_else(|| "collision world could not be built".to_string())?;
        Ok(f(&set))
    }
}
