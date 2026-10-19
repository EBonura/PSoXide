//! Normal-playtest ownership of one cooked sector-grid world.
//!
//! The level core is read off the disc at boot and the room chunks are
//! streamed off `WORLD.PAK` into a page pool as the player moves: the
//! executable embeds no level data. Rooms are `psx-grid` chunks held in a
//! resident set (rooms are added as they arrive and removed when evicted, a
//! missing room is a solid box), collision is the
//! native per-sector provider, draw range and entity activation are walk
//! distances over the portal graph, the sky gate is a per-cell flag, and
//! drawing is the row-span grid renderer.

use core::fmt;

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use psx_engine::{
    commit_body_direction_with_trace_provider, commit_body_step_with_trace_provider,
    trace_collision, BodyStep, CharacterBlockerTraceProvider, CharacterCollisionAabb,
    CharacterCollisionCylinder, CharacterMotorConfig, CharacterMotorFrame, CharacterMotorInput,
    CharacterMotorState, CollisionQueryError, CollisionTrace, CollisionTraceQuery, OtFrame,
    PrimitivePacketArena, RoomPoint, ThirdPersonCameraConfig, ThirdPersonCameraFrame,
    ThirdPersonCameraInput, ThirdPersonCameraState, ThirdPersonCameraTarget, WorldCamera,
};
use psx_game_runtime::cd_stream::{
    read_world_chunk_blocking, read_world_pack_entries_blocking, CdController, WorldChunkInfo,
    ROOM_CHUNK_STATUS_OK,
};
use psx_game_runtime::destructibles::RuntimeDestructibles;
use psx_game_runtime::grid_stream::{wanted_room, DiscTransport, GridStreamer};
use psx_grid::collision::GridTraceProvider;
use psx_grid::contents::{contents_of_liquid_kind, LiquidContentsSample, CONTENTS_EMPTY};
#[cfg(target_arch = "mips")]
use psx_grid::render::load_gte_clip_planes;
use psx_grid::render::{
    load_view_rotation, FrustumPlanes, GridFrame, GridPhase, GridRenderer, RoomCamera,
    ViewProjection, ViewTransform, CLIP_ALL_PLANES,
};
use psx_grid::stream::{room_at, StreamWindow, Want, WantedRoom};
use psx_grid::{FormatError, LevelView, ResidentRooms, RoomActivity, RoomView, LEVEL_HAS_LIQUIDS};
use psx_gte::math::Vec3I32 as GteVec3I32;
use psx_gte::scene;
use psx_level::world_object_flags;
use psx_residency::{CategoryConfig, Class, Wanted};

use crate::generated::{GRID_ROOM_COUNT, GRID_ROOM_POOL_PAGES, WORLD_PACK_START_LBA};
use crate::grid_materials::{bind_pending, Pending};
use crate::level_data::WORLD_OBJECTS;
use crate::world_common::{
    view_camera, world_view_rotation, CountingTraceProvider, VisibilityBounds,
};
use crate::world_objects_runtime::WorldObjectVisibility;
use crate::{telemetry, PROJECTION};

/// Rooms the pool and the residency table can hold at once: never more rooms
/// than pages, and never more than the level has, plus slack for a room backing
/// off after a failed read.
const ROOM_ENTRIES: usize = (if GRID_ROOM_COUNT < GRID_ROOM_POOL_PAGES {
    GRID_ROOM_COUNT
} else {
    GRID_ROOM_POOL_PAGES
}) + 2;

/// Words of the room pool, 512 to a 2 KiB page.
const ROOM_POOL_WORDS: usize = GRID_ROOM_POOL_PAGES * 512;

/// The page memory room chunks are read into: all zero, so it costs the
/// executable nothing.
static mut ROOM_POOL: [u32; ROOM_POOL_WORDS] = [0; ROOM_POOL_WORDS];

/// The residency state of the room pool.
static mut ROOM_STREAMER: GridStreamer<ROOM_ENTRIES, DiscTransport> = GridStreamer::new();

/// Ticks between sight-line tests of one model instance.
const VIEW_REFRESH_TICKS: u32 = 4;

fn box_contains(min: [i32; 3], max: [i32; 3], point: [i32; 3]) -> bool {
    (0..3).all(|axis| point[axis] >= min[axis] && point[axis] <= max[axis])
}

#[derive(Debug)]
pub(super) enum GridRuntimeInitError {
    /// The core (chunk 0 of `WORLD.PAK`) could not be read; the status is a
    /// `cd_stream` status code.
    NoCore(u32),
    /// The level's records chunk did not decode (another version, or damage).
    Records,
    Level(FormatError),
    NoMaterials,
    Collision,
}

impl fmt::Display for GridRuntimeInitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoCore(status) => {
                write!(
                    formatter,
                    "grid level core unreadable from the disc: status {status}"
                )
            }
            Self::Level(e) => write!(formatter, "grid level core refused: {e}"),
            Self::Records => formatter.write_str("grid level records chunk refused"),
            Self::NoMaterials => formatter.write_str("grid level has no materials"),
            Self::Collision => formatter.write_str("grid collision could not be built"),
        }
    }
}

pub(super) struct GridRuntime {
    level: LevelView<'static>,
    /// The rooms in memory right now: the ones the streamer has read off the
    /// disc and installed, and not yet evicted.
    resident: ResidentRooms<'static>,
    activity: RoomActivity,
    /// The rooms the level wants resident around the player and the camera.
    window: StreamWindow,
    /// Scratch of the streamer's wanted list.
    wanted: Vec<WantedRoom>,
    wanted_res: Vec<Wanted>,
    /// Room the player was last found in by the core alone, the search hint.
    stream_hint: usize,
    /// Need rooms not resident when the last [`Self::stream`] ended.
    need_missing: u32,
    /// Windows in which a need room was missing after the world was first
    /// ready: the player met a solid box where a room should be.
    late_short_windows: u32,
    /// Set once the first need set was resident.
    streamed_once: bool,
    renderer: GridRenderer,
    /// Every resident room's materials were bound when the set last changed.
    materials_epoch: Option<u32>,
    /// Scratch of [`bind_pending`].
    material_scratch: Vec<Pending>,
    projection: ViewProjection,
    /// Room the observer was last found in, the search hint.
    room_hint: usize,
    /// Scratch: rooms within the draw distance, then the ones that touch the
    /// frustum.
    draw_scratch: Vec<usize>,
    visible_scratch: Vec<usize>,
    last_world_packet_words: usize,
    camera_traces: u32,
    /// Sight-line cache for [`Self::visible_view_bounds_mask`].
    // psx-numeric-allow-next-line: one bit per model instance, the caller's mask width
    view_known: u64,
    // psx-numeric-allow-next-line: one bit per model instance, the caller's mask width
    view_seen: u64,
    view_phase: u32,
}

impl GridRuntime {
    /// Read the level core off the disc (chunk 0 of `WORLD.PAK`) and start the
    /// room stream. No room is resident yet: [`Self::stream`] reads the ones
    /// around the player, and the loading screen waits for them
    /// ([`Self::rooms_ready`]).
    pub(super) fn load_manifest(cd: &mut CdController) -> Result<Self, GridRuntimeInitError> {
        crate::game_trace("editor-playtest: grid manifest begin");
        // The pack header names the core (chunk 0), the level's own records
        // (chunk 1) and the first room chunk (chunk 2); room records count
        // their sector from the last.
        let mut found = [WorldChunkInfo::EMPTY; 3];
        let status =
            read_world_pack_entries_blocking(cd, WORLD_PACK_START_LBA, &[0, 1, 2], &mut found);
        if status != ROOM_CHUNK_STATUS_OK {
            return Err(GridRuntimeInitError::NoCore(status));
        }
        let [core_info, records_info, first_room] = found;
        let core = Self::read_chunk(cd, core_info)?;
        let records = Self::read_chunk(cd, records_info)?;
        let records =
            psx_level::wire::LevelRecords::decode(records).ok_or(GridRuntimeInitError::Records)?;
        crate::level_data::install(records);
        Self::from_core(core, WORLD_PACK_START_LBA + first_room.sector_offset)
    }

    /// Read one chunk of `WORLD.PAK` whole into memory that lives for the run,
    /// word aligned.
    fn read_chunk(
        cd: &mut CdController,
        info: WorldChunkInfo,
    ) -> Result<&'static [u8], GridRuntimeInitError> {
        let mut words = vec![0u32; info.byte_size.div_ceil(4)];
        let read = read_world_chunk_blocking(cd, WORLD_PACK_START_LBA, info, &mut words);
        if read.status != ROOM_CHUNK_STATUS_OK || read.bytes != info.byte_size {
            return Err(GridRuntimeInitError::NoCore(read.status));
        }
        let words: &'static [u32] = Box::leak(words.into_boxed_slice());
        // SAFETY: a `u32` slice is valid as bytes, `byte_size` is within it.
        Ok(unsafe { core::slice::from_raw_parts(words.as_ptr().cast::<u8>(), info.byte_size) })
    }

    /// A runtime over the level core `core` (word aligned), with room records
    /// read from `base_lba + pack_sector`.
    fn from_core(core: &'static [u8], base_lba: u32) -> Result<Self, GridRuntimeInitError> {
        let level = LevelView::parse(core).map_err(GridRuntimeInitError::Level)?;
        level.validate_full().map_err(GridRuntimeInitError::Level)?;
        if level.materials.is_empty() {
            return Err(GridRuntimeInitError::NoMaterials);
        }
        let resident = ResidentRooms::new(level).ok_or(GridRuntimeInitError::Collision)?;
        // Scratch comes from the core's maxima, so it is right whichever
        // rooms are resident.
        let renderer = GridRenderer::with_maxima(&level.maxima());
        let projection = ViewProjection {
            focal_length: PROJECTION.focal_length,
            half_width: i32::from(PROJECTION.screen_x),
            half_height: i32::from(PROJECTION.screen_y),
            ..ViewProjection::DEFAULT
        };
        let window = StreamWindow::new(&level);
        // An unwanted room farther than the lead radius is a free victim; one
        // inside the hysteresis band (a quarter of the radius, or eight
        // sectors) goes last, so a doorway cannot make two rooms thrash.
        let lead = u32::try_from(window.lead_radius().min(1 << 27)).unwrap_or(0);
        let keep_radius = CategoryConfig::keep_radius_for(lead, level.sector_units() * 8);
        // SAFETY: the pool is a static used only here, once; the streamer owns
        // it from now on.
        unsafe {
            (*core::ptr::addr_of_mut!(ROOM_STREAMER)).start(
                &mut *core::ptr::addr_of_mut!(ROOM_POOL),
                base_lba,
                keep_radius,
            );
        }
        crate::game_trace("editor-playtest: grid runtime assemble");
        Ok(Self {
            level,
            resident,
            activity: RoomActivity::new(&level),
            window,
            wanted: Vec::new(),
            wanted_res: Vec::new(),
            stream_hint: 0,
            need_missing: u32::MAX,
            late_short_windows: 0,
            streamed_once: false,
            renderer,
            materials_epoch: None,
            material_scratch: Vec::new(),
            projection,
            room_hint: 0,
            draw_scratch: Vec::new(),
            visible_scratch: Vec::new(),
            last_world_packet_words: 0,
            camera_traces: 0,
            view_known: 0,
            view_seen: 0,
            view_phase: 0,
        })
    }

    // ------------------------------------------------------------------
    // The resident set
    // ------------------------------------------------------------------

    fn streamer() -> &'static mut GridStreamer<ROOM_ENTRIES, DiscTransport> {
        // SAFETY: the guest is single threaded and no interrupt touches the
        // streamer; each call site uses the borrow before the next call.
        unsafe { &mut *core::ptr::addr_of_mut!(ROOM_STREAMER) }
    }

    /// Make room `room` resident from its chunk bytes. Its materials are
    /// bound by the next [`Self::refresh_materials`] and it is not drawn
    /// before that. (The stream inserts through its install callback; this is
    /// the same step for the tests.)
    #[cfg_attr(not(test), allow(dead_code))]
    fn add_room(&mut self, room: usize, bytes: &'static [u8]) -> bool {
        let Ok(view) = RoomView::parse(bytes) else {
            return false;
        };
        if self.resident.insert(room, view).is_err() {
            return false;
        }
        self.materials_epoch = None;
        true
    }

    /// Drop room `room` from memory. Bodies and traces meet a solid box where
    /// it was; nothing draws or activates in it.
    #[cfg_attr(not(test), allow(dead_code))]
    fn remove_room(&mut self, room: usize) -> bool {
        self.materials_epoch = None;
        self.resident.remove(room).is_some()
    }

    /// Keep the rooms around the player and the camera resident: ask the
    /// streamer for the need set and the lead ring, evict what it gives up,
    /// and install at most one landed room. Call once per fixed tick.
    ///
    /// `demand_only` holds the lead ring back (no read the player does not
    /// need now), for the moments a read would cost something else: combat
    /// music is playing and a read would pause it.
    pub(super) fn stream(&mut self, player: [i32; 3], camera: Option<[i32; 3]>, demand_only: bool) {
        telemetry::stage_begin(telemetry::stage::CD_ROOM_CHUNK_LOAD);
        self.stream_inner(player, camera, demand_only);
        telemetry::stage_end(telemetry::stage::CD_ROOM_CHUNK_LOAD);
    }

    fn stream_inner(&mut self, player: [i32; 3], camera: Option<[i32; 3]>, demand_only: bool) {
        let player_room = room_at(&self.level, self.stream_hint, player);
        if let Some(room) = player_room {
            self.stream_hint = room;
        }
        let camera_room = camera.and_then(|at| room_at(&self.level, self.stream_hint, at));
        self.window.wanted(
            &self.level,
            player_room.map(|room| (room, player)),
            camera.zip(camera_room).map(|(at, room)| (room, at)),
            &mut self.wanted,
        );
        self.wanted_res.clear();
        for wanted in &self.wanted {
            if demand_only && wanted.want == Want::Lead {
                continue;
            }
            if self.wanted_res.len() == ROOM_ENTRIES {
                break;
            }
            let class = match wanted.want {
                Want::Need => Class::Demand,
                Want::Lead => Class::Lead,
            };
            self.wanted_res.push(wanted_room(
                wanted.room,
                class,
                wanted.distance.max(0) as u32,
            ));
        }
        let streamer = Self::streamer();
        let window = &self.window;
        let mut evicted = false;
        let resident = &mut self.resident;
        let summary = streamer.step(
            &mut DiscTransport,
            self.level.rooms,
            &self.wanted_res,
            &|room| window.distance_of(room),
            &mut |room| {
                evicted |= resident.remove(usize::from(room)).is_some();
            },
        );
        telemetry::counter(telemetry::counter::CD_ROOM_CHUNK_LOADS, summary.issued);
        let mut installed = false;
        let resident = &mut self.resident;
        streamer.install_next(&mut |room, bytes| {
            let Ok(view) = RoomView::parse(bytes) else {
                return false;
            };
            installed = resident.insert(usize::from(room), view).is_ok();
            installed
        });
        if evicted || installed {
            self.materials_epoch = None;
        }
        if installed {
            telemetry::counter(telemetry::counter::CD_ROOM_CHUNK_HITS, 1);
        }
        self.need_missing = self
            .wanted
            .iter()
            .filter(|w| w.want == Want::Need && !self.resident.contains(usize::from(w.room)))
            .count() as u32;
        if self.need_missing == 0 {
            self.streamed_once = true;
        } else if self.streamed_once {
            self.late_short_windows = self.late_short_windows.saturating_add(1);
        }
    }

    /// Whether every room the level wants resident now is: the loading screen
    /// waits for it, and nothing draws a hole while it holds.
    pub(super) fn rooms_ready(&self) -> bool {
        self.need_missing == 0
    }

    /// Rooms in memory.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn resident_rooms(&self) -> usize {
        self.resident.len()
    }

    /// The stream's state as numbers, for the sim-trace log: rooms resident,
    /// rooms installed so far, evicted, reads in flight, pages in use, need
    /// rooms missing now, windows that were short after the first fill, reads
    /// the transport lost, installs refused.
    #[cfg(feature = "sim-trace")]
    pub(super) fn stream_snapshot(&self) -> [i32; 9] {
        let streamer = Self::streamer();
        let counters = streamer.counters();
        [
            self.resident.len() as i32,
            counters.installed as i32,
            streamer.totals().evictions as i32,
            streamer.in_flight() as i32,
            streamer.pages_in_use() as i32,
            self.need_missing.min(1 << 20) as i32,
            self.late_short_windows as i32,
            counters.failed_reads as i32,
            counters.failed_installs as i32,
        ]
    }

    // ------------------------------------------------------------------
    // Rooms, activation, visibility
    // ------------------------------------------------------------------

    /// Room containing `point`, updating the search hint.
    fn room_of(&mut self, point: [i32; 3]) -> Option<usize> {
        let located = self.resident.collision().locate(self.room_hint, point)?;
        self.room_hint = located.room;
        Some(located.room)
    }

    /// Refresh the activation search around `observer`. Returns whether the
    /// observer is inside the world at all.
    fn activate(&mut self, observer: RoomPoint) -> bool {
        let at = [observer.x, observer.y, observer.z];
        let Some(room) = self.room_of(at) else {
            return false;
        };
        telemetry::stage_begin(telemetry::stage::SIM_ROOM_TRACK);
        self.activity.track_player(&self.level, room, at);
        telemetry::stage_end(telemetry::stage::SIM_ROOM_TRACK);
        true
    }

    fn point_active(&mut self, point: [i32; 3]) -> bool {
        self.resident
            .collision()
            .locate(self.room_hint, point)
            .is_some_and(|l| self.activity.room_active(&self.resident, l.room))
    }

    fn bounds_active(&self, min: [i32; 3], max: [i32; 3]) -> bool {
        self.activity.active_rooms(&self.resident).any(|room| {
            let (rmin, rmax) = self.level.room_bounds(room);
            (0..3).all(|axis| min[axis] <= rmax[axis] && max[axis] >= rmin[axis])
        })
    }

    /// Like [`Self::activate`], for a camera position: the camera and the
    /// player keep separate searches.
    fn activate_camera(&mut self, observer: RoomPoint) -> bool {
        let at = [observer.x, observer.y, observer.z];
        let Some(room) = self.room_of(at) else {
            return false;
        };
        self.activity.track_camera(&self.level, room, at);
        true
    }

    fn camera_bounds_active(&self, min: [i32; 3], max: [i32; 3]) -> bool {
        self.activity
            .camera_active_rooms(&self.resident)
            .any(|room| {
                let (rmin, rmax) = self.level.room_bounds(room);
                (0..3).all(|axis| min[axis] <= rmax[axis] && max[axis] >= rmin[axis])
            })
    }

    // psx-numeric-allow-next-line: one bit per queried point; the width IS the caller's point capacity
    pub(super) fn visible_points_mask(&mut self, observer: RoomPoint, points: &[[i32; 3]]) -> u64 {
        if !self.activate(observer) {
            return 0;
        }
        let mut mask = 0u64;
        for (index, point) in points.iter().enumerate().take(64) {
            if self.point_active(*point) {
                mask |= 1u64 << index;
            }
        }
        mask
    }

    // psx-numeric-allow-next-line: one bit per queried bounds; the width IS the caller's capacity
    pub(super) fn visible_bounds_mask(
        &mut self,
        observer: RoomPoint,
        bounds: &[VisibilityBounds],
        // psx-numeric-allow-next-line: one bit per queried bounds; return width is the caller's fixed capacity
    ) -> u64 {
        if !self.activate(observer) {
            return 0;
        }
        let mut mask = 0u64;
        for (index, b) in bounds.iter().enumerate().take(64) {
            if self.bounds_active(b.min(), b.max()) {
                mask |= 1u64 << index;
            }
        }
        mask
    }

    /// Render and targeting visibility for model instances: in an active room
    /// and seen along a straight line from the player's body or from the
    /// camera. An actor hidden behind a pillar costs nothing to draw.
    // psx-numeric-allow-next-line: one bit per queried bounds; the width IS the caller's capacity
    pub(super) fn visible_view_bounds_mask(
        &mut self,
        observer: RoomPoint,
        eye: RoomPoint,
        bounds: &[VisibilityBounds],
        // psx-numeric-allow-next-line: bit per bounds that gets a sight-line test; the others are visible in an active room
        sight_tested: u64,
        // psx-numeric-allow-next-line: one bit per queried bounds; return width is the caller's fixed capacity
    ) -> u64 {
        if !self.activate(observer) {
            return 0;
        }
        let from_body = [observer.x, observer.y, observer.z];
        let from_eye = [eye.x, eye.y, eye.z];
        let phase = self.view_phase;
        self.view_phase = phase.wrapping_add(1);
        let mut mask = 0u64;
        for (index, b) in bounds.iter().enumerate().take(64) {
            let (min, max) = (b.min(), b.max());
            if !self.bounds_active(min, max) {
                continue;
            }
            // psx-numeric-allow-next-line: one bit per model instance, the caller's mask width
            let bit = 1u64 << index;
            if sight_tested & bit == 0 {
                mask |= bit;
                continue;
            }
            // Sight lines are the expensive part: each instance is re-tested
            // on its own turn and keeps the last answer in between, so an
            // actor reappears within a few ticks of stepping out of cover.
            let due = self.view_known & bit == 0
                || (phase.wrapping_add(index as u32) % VIEW_REFRESH_TICKS == 0);
            if due {
                let seen = box_contains(min, max, from_body)
                    || self.column_seen_from(from_eye, min, max)
                    || self.column_seen_from(from_body, min, max);
                self.view_known |= bit;
                if seen {
                    self.view_seen |= bit;
                } else {
                    self.view_seen &= !bit;
                }
            }
            mask |= self.view_seen & bit;
        }
        mask
    }

    /// Whether the line from `from` to the box's centre column (a quarter,
    /// half and three quarters of its height) is clear of solid world.
    fn column_seen_from(&self, from: [i32; 3], min: [i32; 3], max: [i32; 3]) -> bool {
        let cx = min[0].saturating_add(max[0].saturating_sub(min[0]) / 2);
        let cz = min[2].saturating_add(max[2].saturating_sub(min[2]) / 2);
        let height = max[1].saturating_sub(min[1]);
        let world = self.resident.collision();
        [2, 1, 3].into_iter().any(|quarter_steps| {
            let y = min[1].saturating_add(height / 4 * quarter_steps);
            world.line_of_sight(from, [cx, y, cz])
        })
    }

    /// Whether the straight line from `from` to a few points of the box is
    /// clear of solid world.
    fn box_clear_from(&self, from: [i32; 3], min: [i32; 3], max: [i32; 3]) -> bool {
        let center = core::array::from_fn::<_, 3, _>(|axis| {
            min[axis].saturating_add(max[axis].saturating_sub(min[axis]) / 2)
        });
        let samples = [
            center,
            [min[0], center[1], center[2]],
            [max[0], center[1], center[2]],
            [center[0], min[1], center[2]],
            [center[0], max[1], center[2]],
            [center[0], center[1], min[2]],
            [center[0], center[1], max[2]],
        ];
        let world = self.resident.collision();
        samples
            .into_iter()
            .any(|sample| world.line_of_sight(from, sample))
    }

    pub(super) fn visible_world_objects(
        &mut self,
        camera: WorldCamera,
        destructibles: &RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
    ) -> WorldObjectVisibility {
        let observer = RoomPoint::new(camera.position.x, camera.position.y, camera.position.z);
        if !self.activate_camera(observer) {
            return WorldObjectVisibility::NONE;
        }
        let eye = [observer.x, observer.y, observer.z];
        let mut visibility = WorldObjectVisibility::NONE;
        let count = WORLD_OBJECTS.len().min(psx_level::MAX_WORLD_OBJECTS);
        for index in 0..count {
            let object = &WORLD_OBJECTS[index];
            if object.destructible != psx_level::WORLD_OBJECT_DESTRUCTIBLE_NONE
                && !destructibles.alive(usize::from(object.destructible))
            {
                continue;
            }
            if !self.camera_bounds_active(object.bounds_min, object.bounds_max) {
                continue;
            }
            if object.flags & world_object_flags::DIRECT_OCCLUSION != 0
                && !self.box_clear_from(eye, object.bounds_min, object.bounds_max)
            {
                continue;
            }
            visibility.set(index);
        }
        visibility
    }

    pub(super) fn typed_world_object_directly_visible(
        &mut self,
        observer: RoomPoint,
        kind: u8,
        source_index: usize,
        destructibles: &RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
    ) -> bool {
        let Ok(source_index) = u16::try_from(source_index) else {
            return false;
        };
        let Ok(index) = WORLD_OBJECTS.binary_search_by_key(&(kind, source_index), |object| {
            (object.kind, object.source_index)
        }) else {
            return false;
        };
        let object = &WORLD_OBJECTS[index];
        if object.destructible != psx_level::WORLD_OBJECT_DESTRUCTIBLE_NONE
            && !destructibles.alive(usize::from(object.destructible))
        {
            return false;
        }
        let from = [observer.x, observer.y, observer.z];
        let inside =
            (0..3).all(|a| from[a] >= object.bounds_min[a] && from[a] <= object.bounds_max[a]);
        inside || self.box_clear_from(from, object.bounds_min, object.bounds_max)
    }

    /// Liquid contents at the player's feet, torso and head. Grid documents
    /// have no liquids yet, so a level without the flag answers at once.
    pub(super) fn player_contents(
        &self,
        position: RoomPoint,
        height: i32,
    ) -> Option<LiquidContentsSample> {
        if self.level.header.flags & LEVEL_HAS_LIQUIDS == 0 {
            return None;
        }
        let height = height.max(1);
        let samples = [
            position.y.saturating_add(1),
            position.y.saturating_add(height >> 1),
            position.y.saturating_add(height.saturating_sub(1)),
        ];
        let mut out = LiquidContentsSample::default();
        for (i, y) in samples.into_iter().enumerate() {
            let code = self
                .resident
                .collision()
                .contents_at([position.x, y, position.z]);
            if code != 0 {
                let contents = contents_of_liquid_kind(code);
                if out.contents == CONTENTS_EMPTY || contents < out.contents {
                    out.contents = contents;
                }
                if out.water_level == 0 {
                    out.water_level = (i + 1) as u8;
                }
            }
        }
        Some(out)
    }

    // ------------------------------------------------------------------
    // Materials
    // ------------------------------------------------------------------

    /// Bind the materials of every resident room that has none yet through
    /// the playtest VRAM owner. Returns `true` only when every resident
    /// room's queued uploads are ready.
    pub(super) fn refresh_materials(&mut self, textures_ready: bool) -> bool {
        if self.materials_epoch == Some(self.resident.epoch()) {
            return true;
        }
        let ready = bind_pending(
            &self.level,
            &mut self.resident,
            &mut self.material_scratch,
            textures_ready,
        );
        if ready {
            self.materials_epoch = Some(self.resident.epoch());
        }
        ready
    }

    /// Drop every room's resolved materials so the next `refresh_materials`
    /// binds them again.
    pub(super) fn invalidate_materials(&mut self) {
        self.materials_epoch = None;
        self.resident.invalidate_materials();
    }

    pub(super) fn materials_ready(&self) -> bool {
        self.materials_epoch == Some(self.resident.epoch())
    }

    // ------------------------------------------------------------------
    // Collision
    // ------------------------------------------------------------------

    pub(super) fn update_motor(
        &mut self,
        motor: &mut CharacterMotorState,
        input: CharacterMotorInput,
        config: CharacterMotorConfig,
        delta_vblanks: u16,
        _destructibles: &RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
        blockers: &[CharacterCollisionCylinder],
        aabb_blockers: &[CharacterCollisionAabb],
    ) -> Result<CharacterMotorFrame, CollisionQueryError> {
        let world = self.resident.collision();
        let mut provider = GridTraceProvider::new(&world);
        let mut provider =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut provider, blockers, aabb_blockers);
        motor.update_vblanks_with_trace_provider(&mut provider, input, config, delta_vblanks)
    }

    pub(super) fn trace_hook_body(
        &mut self,
        from: RoomPoint,
        to: RoomPoint,
        config: CharacterMotorConfig,
        _destructibles: &RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
        blockers: &[CharacterCollisionCylinder],
        aabb_blockers: &[CharacterCollisionAabb],
    ) -> Result<CollisionTrace, CollisionQueryError> {
        let world = self.resident.collision();
        let mut provider = GridTraceProvider::new(&world);
        let mut provider =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut provider, blockers, aabb_blockers);
        trace_collision(
            &mut provider,
            CollisionTraceQuery::body(from, to, config.radius, config.height),
        )
    }

    pub(super) fn commit_body_step(
        &mut self,
        start: RoomPoint,
        dx: i32,
        dz: i32,
        radius: i32,
        height: i32,
        _destructibles: &RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
        blockers: &[CharacterCollisionCylinder],
        aabb_blockers: &[CharacterCollisionAabb],
    ) -> Result<BodyStep, CollisionQueryError> {
        let (radius, height) = (radius.max(0), height.max(1));
        let world = self.resident.collision();
        let mut provider = GridTraceProvider::new(&world);
        let mut provider =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut provider, blockers, aabb_blockers);
        commit_body_step_with_trace_provider(&mut provider, start, dx, dz, radius, height)
    }

    pub(super) fn commit_body_direction(
        &mut self,
        start: RoomPoint,
        dx: i32,
        dz: i32,
        radius: i32,
        height: i32,
        _destructibles: &RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
        blockers: &[CharacterCollisionCylinder],
        aabb_blockers: &[CharacterCollisionAabb],
    ) -> Result<BodyStep, CollisionQueryError> {
        let (radius, height) = (radius.max(0), height.max(1));
        let world = self.resident.collision();
        let mut provider = GridTraceProvider::new(&world);
        let mut provider =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut provider, blockers, aabb_blockers);
        commit_body_direction_with_trace_provider(&mut provider, start, dx, dz, radius, height)
    }

    pub(super) fn melee_segment_clear(
        &mut self,
        from: RoomPoint,
        to: RoomPoint,
        aabb_blockers: &[CharacterCollisionAabb],
        destructibles: &RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
    ) -> bool {
        self.trace_point_segment(from, to, aabb_blockers, destructibles)
            .is_ok_and(|trace| !trace.hit())
    }

    pub(super) fn trace_point_segment(
        &mut self,
        from: RoomPoint,
        to: RoomPoint,
        aabb_blockers: &[CharacterCollisionAabb],
        _destructibles: &RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
    ) -> Result<CollisionTrace, CollisionQueryError> {
        let world = self.resident.collision();
        let mut provider = GridTraceProvider::new(&world);
        let mut provider =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut provider, &[], aabb_blockers);
        trace_collision(&mut provider, CollisionTraceQuery::point(from, to))
    }

    pub(super) fn update_camera(
        &mut self,
        camera: &mut ThirdPersonCameraState,
        target: ThirdPersonCameraTarget,
        input: ThirdPersonCameraInput,
        config: ThirdPersonCameraConfig,
        delta_vblanks: u16,
        aabb_blockers: &[CharacterCollisionAabb],
        _destructibles: &RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
    ) -> Result<ThirdPersonCameraFrame, CollisionQueryError> {
        // The camera arm passes through translucent and alpha-cut planes.
        let world = self.resident.collision();
        let mut provider = GridTraceProvider::for_camera(&world);
        let mut provider =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut provider, &[], aabb_blockers);
        let mut counting = CountingTraceProvider::new(&mut provider);
        let frame = camera.update_vblanks_with_trace_provider(
            PROJECTION,
            &mut counting,
            target,
            input,
            config,
            delta_vblanks,
        );
        self.camera_traces = counting.traces();
        frame
    }

    pub(super) fn camera_traces(&self) -> u32 {
        self.camera_traces
    }

    // ------------------------------------------------------------------
    // Drawing
    // ------------------------------------------------------------------

    pub(super) fn last_world_packet_words(&self) -> usize {
        self.last_world_packet_words
    }

    pub(super) fn fits_before_fence(&self, primitive_packets: &PrimitivePacketArena<'_>) -> bool {
        let predicted_words = self
            .last_world_packet_words
            .saturating_add(self.last_world_packet_words / 8);
        !primitive_packets.fence_pending()
            || primitive_packets.remaining_words_before_fence() >= predicted_words
    }

    /// Rooms to draw: resident rooms within the draw distance of the eye
    /// that touch the frustum (the whole box test is camera-relative).
    fn visible_rooms(&mut self, eye: [i32; 3], frustum: &FrustumPlanes) {
        self.visible_scratch.clear();
        let room = self.room_of(eye);
        telemetry::stage_begin(telemetry::stage::PORTAL_VISIBILITY);
        let mut candidates = core::mem::take(&mut self.draw_scratch);
        self.activity
            .draw_rooms(&self.level, &self.resident, room, eye, &mut candidates);
        telemetry::stage_end(telemetry::stage::PORTAL_VISIBILITY);
        for &room in &candidates {
            let (min, max) = self.level.room_bounds(room);
            if frustum
                .cull_world_aabb(eye, min, max, CLIP_ALL_PLANES)
                .is_some()
            {
                self.visible_scratch.push(room);
            }
        }
        self.draw_scratch = candidates;
    }

    pub(super) fn draw<const DEPTH: usize>(
        &mut self,
        camera: WorldCamera,
        _visibility_origin: Option<RoomPoint>,
        _material_tick: u32,
        _destructibles: &RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
        primitive_packets: &mut PrimitivePacketArena<'_>,
        ot: &mut OtFrame<'_, DEPTH>,
    ) -> bool {
        assert_eq!(
            DEPTH, 2048,
            "grid packet tags require the canonical 2048-entry ordering table"
        );
        scene::set_screen_offset(
            i32::from(PROJECTION.screen_x) << 16,
            i32::from(PROJECTION.screen_y) << 16,
        );
        scene::set_projection_plane(
            camera.projection.focal_length.clamp(1, i32::from(u16::MAX)) as u16
        );
        scene::set_average_z_weights(0x155, 0x100);
        self.projection.focal_length = camera.projection.focal_length;
        self.projection.half_width = i32::from(camera.projection.screen_x);
        self.projection.half_height = i32::from(camera.projection.screen_y);
        let view_rotation = world_view_rotation(camera);
        let cam = view_camera(camera);
        let view = load_view_rotation(cam.origin, view_rotation);
        let eye = [cam.origin.x >> 12, cam.origin.y >> 12, cam.origin.z >> 12];
        // Planes for a camera at the origin: the room boxes below are made
        // eye-relative, so nothing here depends on how far the world is from
        // its origin.
        let world_frustum = FrustumPlanes::at_eye(&view.rotation, self.projection);
        // The box tests below read the plane dots from the GTE on the guest,
        // so the world frustum has to be loaded first.
        #[cfg(target_arch = "mips")]
        load_gte_clip_planes(&world_frustum.planes);
        self.visible_rooms(eye, &world_frustum);
        let screen = [
            self.projection.half_width * 2,
            self.projection.half_height * 2,
        ];
        let sector = self.level.sector_units() as i32;

        primitive_packets.try_fence_completed();
        if !self.fits_before_fence(primitive_packets) {
            primitive_packets.fence();
        }
        let mut redraws = 0u32;
        loop {
            if primitive_packets.remaining_before_fence() == 0 {
                primitive_packets.fence();
            }
            let capacity = primitive_packets.remaining_words_before_fence();
            let can_grow = primitive_packets.fence_pending();
            let Some(mut reservation) = primitive_packets.reserve_packet_words(capacity) else {
                return false;
            };
            let mut total = GridFrame::default();
            let mut unready = 0u32;
            let reverse;
            {
                // A descending arena takes the stream against the end of the
                // reservation, so rooms fill it back to front and nothing
                // slides at the commit.
                reverse = reservation.is_descending();
                let packets = if reverse {
                    reservation.tail_words_mut()
                } else {
                    reservation.words_mut()
                };
                let packet_count = packets.len();
                for &room_index in &self.visible_scratch {
                    let Some(slot) = self.resident.get(room_index) else {
                        continue;
                    };
                    let Some(room) = slot.view() else {
                        continue;
                    };
                    if !slot.materials_ready {
                        // Landed but not bound: not drawn until it is.
                        unready += 1;
                        continue;
                    }
                    let record = &self.level.rooms[room_index];
                    let origin = record.origin;
                    // Room-local view: the rotated, camera-relative origin of
                    // the room becomes the GTE translation, so the room's
                    // `i16` vertices project with no per-vertex add.
                    let rows = &view.rotation.m;
                    let d = [origin[0] - eye[0], origin[1] - eye[1], origin[2] - eye[2]];
                    let t = |row: usize| {
                        (i32::from(rows[row][0]) * d[0]
                            + i32::from(rows[row][1]) * d[1]
                            + i32::from(rows[row][2]) * d[2])
                            >> 12
                    };
                    let translation = GteVec3I32::new(t(0), t(1), t(2));
                    scene::load_translation(translation);
                    let local_view = ViewTransform {
                        rotation: view.rotation,
                        translation,
                    };
                    let local_eye = [-d[0], -d[1], -d[2]];
                    let frustum =
                        FrustumPlanes::from_view(&view.rotation, translation, self.projection);
                    let eye_view = RoomCamera {
                        frustum: &frustum,
                        view: &local_view,
                        camera: local_eye,
                        screen,
                        focal_length: self.projection.focal_length,
                        sector,
                    };
                    let frame = self.renderer.draw_room_with_marks(
                        &room,
                        &eye_view,
                        &slot.materials,
                        if reverse {
                            &mut packets[..packet_count - total.packet_words]
                        } else {
                            &mut packets[total.packet_words..]
                        },
                        reverse,
                        &mut |phase, begin| {
                            let id = match phase {
                                GridPhase::Cells => telemetry::stage::ROOM_CELL_SELECT,
                                GridPhase::Quads => telemetry::stage::ROOM_VISIBLE_LIST,
                                GridPhase::Project => telemetry::stage::ROOM_PROJECT,
                                GridPhase::Emit => telemetry::stage::ROOM_SURFACE_DRAW,
                            };
                            if begin {
                                telemetry::stage_begin(id);
                            } else {
                                telemetry::stage_end(id);
                            }
                        },
                    );
                    total.absorb(&frame);
                    if frame.overflowed {
                        break;
                    }
                }
            }
            scene::load_translation(view.translation);
            telemetry::counter(
                telemetry::counter::ROOM_SURFACES_CONSIDERED,
                u32::from(total.quads_visible),
            );
            telemetry::counter(telemetry::counter::ROOM_SURFACE_PACKETS, total.packets);
            telemetry::counter(
                telemetry::counter::ROOM_PROJECTED_VERTICES,
                u32::from(total.vertices_projected),
            );
            telemetry::counter(
                telemetry::counter::ROOM_SURF_SPLIT_TRIS,
                u32::from(total.robust_quads),
            );
            telemetry::counter(
                telemetry::counter::ROOM_SURFACE_COMMANDS,
                u32::from(total.cells_visible),
            );
            telemetry::counter(
                telemetry::counter::ROOM_SURF_BACKFACE_CULLED,
                u32::from(total.backface_culled),
            );
            telemetry::counter(
                telemetry::counter::ROOM_SURF_SCREEN_CULLED,
                u32::from(total.screen_culled),
            );
            telemetry::counter(
                telemetry::counter::ROOM_SUBMIT_FALLBACK_CALLS,
                u32::from(total.material_missing) + u32::from(total.rooms_refused) + unready,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SURF_TR_SUBDIVISION_CANDIDATES,
                u32::from(total.depth_skipped),
            );
            telemetry::counter(
                telemetry::counter::ROOM_SURF_TR_SUBDIVISION_SUBMITTED,
                u32::from(total.robust_packets),
            );
            if total.overflowed && can_grow {
                // The uncommitted reservation is abandoned, not linked.
                redraws += 1;
                primitive_packets.fence();
                continue;
            }
            self.last_world_packet_words = total.packet_words;
            telemetry::counter(telemetry::counter::ROOM_SUBMIT_PRIMITIVE_OVERFLOWS, redraws);
            let stream = if reverse {
                reservation.commit_tail(total.packet_words, total.packets as usize)
            } else {
                reservation.commit(total.packet_words, total.packets as usize)
            }
            .expect("grid renderer reported an invalid shared-arena stream");
            unsafe {
                ot.add_committed_tagged_packet_stream_unchecked(stream);
            }
            return total.sky_visible;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_grid::write::{aligned_copy, LevelCore, RoomChunk};
    use psx_grid::{
        Material, Portal, RCell, RenderMaxima, RoomRecord, Sector, Vtx, NO_EXT, PORTAL_ALWAYS_OPEN,
        SECTOR_FLAT_FLOOR, SECTOR_FLOOR_WALK_A, SECTOR_FLOOR_WALK_B, SECTOR_HAS_FLOOR,
    };

    const S: i32 = 80;

    /// One floored sector with a ceiling at 80.
    fn chunk() -> Vec<u8> {
        RoomChunk {
            width: 1,
            depth: 1,
            min_y: 0,
            max_y: 80,
            sectors: vec![Sector {
                floor: [0; 4],
                ceil: [80; 4],
                flags: SECTOR_HAS_FLOOR
                    | SECTOR_FLOOR_WALK_A
                    | SECTOR_FLOOR_WALK_B
                    | SECTOR_FLAT_FLOOR,
                ext: NO_EXT,
            }],
            vertices: vec![Vtx::default(); 4],
            cells: vec![RCell {
                first: 0,
                count: 0,
                flags: 0,
                min_y: 0,
                max_y: 80,
            }],
            ..RoomChunk::default()
        }
        .to_bytes()
    }

    /// Two one-sector rooms side by side along X, joined by a portal.
    fn core() -> Vec<u8> {
        let wall = |at: i16, dest: u16| Portal {
            dest,
            flags: PORTAL_ALWAYS_OPEN,
            v: [[at, 0, 0], [at, 0, 80], [at, 80, 80], [at, 80, 0]],
            ..Portal::default()
        };
        let room = |i: i32, first: u16| RoomRecord {
            origin: [i * S, 0, 0],
            width: 1,
            depth: 1,
            portal_first: first,
            portal_count: 1,
            min_y: 0,
            max_y: 80,
            pack_sector: i as u32,
            pack_sectors: 1,
            ..RoomRecord::default()
        };
        LevelCore {
            sector_units: S as u16,
            rooms: vec![room(0, 0), room(1, 1)],
            portals: vec![wall(80, 1), wall(0, 0)],
            materials: vec![Material::default()],
            maxima: RenderMaxima {
                vertices: 4,
                cells: 1,
                quads: 1,
                ..RenderMaxima::default()
            },
            ..LevelCore::default()
        }
        .to_bytes()
    }

    fn leak(bytes: &[u8]) -> &'static [u8] {
        let words: &'static [u32] = Box::leak(aligned_copy(bytes).into_boxed_slice());
        // SAFETY: a `u32` slice is valid as bytes.
        unsafe { core::slice::from_raw_parts(words.as_ptr().cast::<u8>(), bytes.len()) }
    }

    /// A point inside the plan footprint of room `room`.
    fn inside(runtime: &GridRuntime, room: usize) -> [i32; 3] {
        let (min, max) = runtime.level.room_bounds(room);
        [(min[0] + max[0]) / 2, min[1] + 1, (min[2] + max[2]) / 2]
    }

    #[test]
    fn rooms_leave_and_come_back_while_the_runtime_keeps_answering() {
        let mut runtime = GridRuntime::from_core(leak(&core()), 1000).expect("the core loads");
        // Nothing is resident until the stream brings rooms in.
        assert_eq!(runtime.resident_rooms(), 0);
        assert!(!runtime.rooms_ready());
        let bytes = [leak(&chunk()), leak(&chunk())];
        assert!(runtime.add_room(0, bytes[0]));
        assert!(runtime.add_room(1, bytes[1]));
        assert_eq!(runtime.resident_rooms(), 2);
        let at = inside(&runtime, 0);
        assert_eq!(runtime.room_of(at), Some(0));

        // Evict room 0: nothing is found there and the world is a solid box.
        assert!(runtime.remove_room(0));
        assert!(!runtime.remove_room(0));
        assert_eq!(runtime.resident_rooms(), 1);
        assert_eq!(runtime.room_of(at), None);
        assert!(runtime
            .resident
            .collision()
            .body_blocked([at[0], at[1] + 8, at[2]], 12, 48));
        assert!(!runtime.materials_ready());

        // Load it again: it is a room once more.
        assert!(runtime.add_room(0, bytes[0]));
        assert_eq!(runtime.resident_rooms(), 2);
        assert_eq!(runtime.room_of(at), Some(0));
        // Loading a room that is already here is refused, not duplicated.
        assert!(!runtime.add_room(0, bytes[0]));
    }

    #[test]
    fn the_core_alone_says_where_the_player_is_before_any_room_is_here() {
        let runtime = GridRuntime::from_core(leak(&core()), 1000).expect("the core loads");
        assert_eq!(room_at(&runtime.level, 0, inside(&runtime, 1)), Some(1));
        assert_eq!(room_at(&runtime.level, 0, [-5, 1, 1]), None);
    }
}
