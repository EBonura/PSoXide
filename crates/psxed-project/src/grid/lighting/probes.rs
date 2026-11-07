//! The light probes of a world: where they sit and how they become the
//! tables a room chunk carries.
//!
//! A character is lit by the probes around it (see `psx_grid::probe`), so the
//! cook and the editor preview both need the same answer to "where are the
//! probes". One sits in the middle of every sector, a chest height above each
//! floor of its column; a sector with no floor has none. The floors come from
//! the runtime's own collision, so the probes are where a body can stand.
//!
//! [`ProbePlan`] is only the places. What each place is lit by comes from
//! [`super::BakeRig::probe`] (the lamps and their shadows, plus the gathered
//! ambient of the full sphere around the probe when the world gathers its
//! light), and [`ProbePlan::tables`] puts the colours into the chunk tables.

use psx_grid::write::{aligned_copy, words_as_bytes, LevelCore, ProbeTables, RoomChunk};
use psx_grid::{
    probe_pack, CollisionView, LevelView, ProbeView, ResidentRooms, RoomRecord, Sector, NO_EXT,
    SECTOR_HAS_FLOOR,
};

use super::probe::{room_origins, CollisionProbe};
use crate::grid::cook::units::RuntimeUnits;
use crate::grid::world::GridWorld;

/// Height of a probe above its floor, runtime units: the middle of the player
/// (the Graybox Reach capsule is 64 units tall, authored 1024 in a world at
/// scale 5/4), which is also where a model's bounding sphere is centred.
/// Lower under a low ceiling, see [`ProbePlan::build`].
pub const CHEST_UNITS: i32 = 32;

/// Most floors one column's probes are planned for.
const MAX_FLOORS: usize = 16;

/// One place a probe sits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProbeSite {
    pub room: u16,
    /// Sector, `cz * width + cx`.
    pub cell: u32,
    /// World position in runtime units.
    pub point: [f64; 3],
    /// Height above the room's origin, as the chunk stores it.
    pub local_y: i16,
}

/// Every probe site of a world, rooms ascending, then sectors, then floors
/// bottom to top.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProbePlan {
    pub sites: Vec<ProbeSite>,
    /// Sectors along X and Z of each room.
    dims: Vec<(usize, usize)>,
}

impl ProbePlan {
    /// The probes of `world`, over the floors `probe`'s collision holds.
    pub fn build(
        world: &GridWorld,
        units: &RuntimeUnits,
        probe: &CollisionProbe,
    ) -> Result<Self, String> {
        let s = units.sector() as i32;
        let half = (s / 2) as u32;
        let origins = room_origins(world, units);
        let mut sites = Vec::new();
        probe.with_collision(|collision| {
            for (room_index, room) in world.rooms.iter().enumerate() {
                let origin = origins[room_index];
                for cz in 0..room.depth {
                    for cx in 0..room.width {
                        // Floors of the column at its middle, top down.
                        let mut floors: Vec<i32> = Vec::new();
                        let mut below = i32::MAX;
                        while floors.len() < MAX_FLOORS {
                            let Some(floor) =
                                collision.floor_below(room_index, (cx, cz), (half, half), below)
                            else {
                                break;
                            };
                            floors.push(floor);
                            below = floor - 1;
                        }
                        for &floor in floors.iter().rev() {
                            // Under a low ceiling the probe sits halfway up.
                            let rise = collision
                                .ceil_above(room_index, (cx, cz), (half, half), floor + 1)
                                .map_or(CHEST_UNITS, |ceil| {
                                    CHEST_UNITS.min(((ceil - floor) / 2).max(1))
                                });
                            let point = [
                                origin[0] + cx as i32 * s + s / 2,
                                floor + rise,
                                origin[2] + cz as i32 * s + s / 2,
                            ];
                            sites.push(ProbeSite {
                                room: room_index as u16,
                                cell: (cz * room.width + cx) as u32,
                                point: point.map(f64::from),
                                local_y: (point[1] - origin[1])
                                    .clamp(i32::from(i16::MIN), i32::from(i16::MAX))
                                    as i16,
                            });
                        }
                    }
                }
            }
        })?;
        Ok(Self {
            sites,
            dims: world.rooms.iter().map(|r| (r.width, r.depth)).collect(),
        })
    }

    /// Sites planned.
    pub fn len(&self) -> usize {
        self.sites.len()
    }

    /// Whether no sector has a floor.
    pub fn is_empty(&self) -> bool {
        self.sites.is_empty()
    }

    /// The chunk tables of every room: `colour(index, site)` is the light at
    /// site number `index` of [`Self::sites`], packed to RGB555. `None` for a
    /// room with no probe at all.
    pub fn tables(
        &self,
        mut colour: impl FnMut(usize, &ProbeSite) -> [u8; 3],
    ) -> Vec<Option<ProbeTables>> {
        let mut out: Vec<Option<ProbeTables>> = vec![None; self.dims.len()];
        let mut at = 0;
        while at < self.sites.len() {
            let first = self.sites[at];
            let mut end = at;
            while end < self.sites.len()
                && self.sites[end].room == first.room
                && self.sites[end].cell == first.cell
            {
                end += 1;
            }
            let floors: Vec<(i16, u16)> = (at..end)
                .map(|i| (self.sites[i].local_y, probe_pack(colour(i, &self.sites[i]))))
                .collect();
            let (width, depth) = self.dims[usize::from(first.room)];
            out[usize::from(first.room)]
                .get_or_insert_with(|| ProbeTables::empty(width, depth))
                .set_cell(first.cell as usize, &floors);
            at = end;
        }
        out
    }
}

/// The probes of a world held the way a running game holds them, for the
/// editor preview and for tests: a level core and one minimal chunk per
/// room, read through the same [`ResidentRooms::probe_light`] the guest runs.
pub struct ProbeStore {
    core: Vec<u32>,
    core_len: usize,
    chunks: Vec<Vec<u32>>,
}

impl ProbeStore {
    /// A store over `tables` (one entry per room of `world`).
    pub fn new(
        world: &GridWorld,
        units: &RuntimeUnits,
        tables: &[Option<ProbeTables>],
    ) -> Result<Self, String> {
        let origins = room_origins(world, units);
        let core = LevelCore {
            occupancy: crate::grid::cook::occupancy::derive(world),
            sector_units: units.sector() as u16,
            rooms: world
                .rooms
                .iter()
                .zip(&origins)
                .map(|(room, origin)| RoomRecord {
                    origin: *origin,
                    width: room.width as u8,
                    depth: room.depth as u8,
                    ..RoomRecord::default()
                })
                .collect(),
            ..LevelCore::default()
        };
        let core_bytes = core.try_to_bytes()?;
        let chunks = world
            .rooms
            .iter()
            .enumerate()
            .map(|(index, room)| {
                let cells = room.width * room.depth;
                let chunk = RoomChunk {
                    room_id: index as u16,
                    width: room.width as u8,
                    depth: room.depth as u8,
                    sectors: vec![
                        Sector {
                            flags: SECTOR_HAS_FLOOR,
                            ext: NO_EXT,
                            ..Sector::default()
                        };
                        cells
                    ],
                    probes: tables.get(index).cloned().flatten(),
                    ..RoomChunk::default()
                };
                aligned_copy(&chunk.to_bytes())
            })
            .collect();
        let store = Self {
            core_len: core_bytes.len(),
            core: aligned_copy(&core_bytes),
            chunks,
        };
        store.with_set(|_| ())?;
        Ok(store)
    }

    /// Run `f` over the resident set of the store. Parsed once per call, so
    /// sample as many points as you can in it.
    pub fn with_set<R>(&self, f: impl FnOnce(&ResidentRooms<'_>) -> R) -> Result<R, String> {
        let level = LevelView::parse(words_as_bytes(&self.core, self.core_len))
            .map_err(|e| format!("probe store core: {e:?}"))?;
        let mut set = ResidentRooms::new(level)
            .ok_or_else(|| "probe store set could not be built".to_string())?;
        for (room, words) in self.chunks.iter().enumerate() {
            let bytes = words_as_bytes(words, words.len() * 4);
            let col = CollisionView::parse(bytes)
                .map_err(|e| format!("probe store room {room}: {e:?}"))?;
            set.insert_collision(room, col)
                .map_err(|e| format!("probe store room {room}: {e:?}"))?;
            let probes = ProbeView::parse_chunk(bytes)
                .map_err(|e| format!("probe store room {room}: {e:?}"))?;
            set.attach_probes(room, probes);
        }
        Ok(f(&set))
    }

    /// The light at the world point `p` (runtime units), as the game would
    /// light a character there.
    pub fn light_at(&self, p: [i32; 3]) -> Option<[u8; 3]> {
        self.with_set(|set| set.probe_light(p)).ok().flatten()
    }
}
