//! Normal-playtest ownership of one cooked sector-grid world.
//!
//! Rooms are `psx-grid` chunks, collision is the native per-sector provider,
//! activation walks the portal graph, the sky gate is a per-cell flag, and
//! drawing is the row-span grid renderer.

use core::fmt;

use alloc::vec::Vec;

use psx_asset::Texture;
use psx_engine::{
    commit_body_direction_with_trace_provider, commit_body_step_with_trace_provider,
    trace_collision, BodyStep, CharacterBlockerTraceProvider, CharacterCollisionAabb,
    CharacterCollisionCylinder, CharacterMotorConfig, CharacterMotorFrame, CharacterMotorInput,
    CharacterMotorState, CollisionQueryError, CollisionTrace, CollisionTraceQuery, OtFrame,
    PrimitivePacketArena, RoomPoint, ThirdPersonCameraConfig, ThirdPersonCameraFrame,
    ThirdPersonCameraInput, ThirdPersonCameraState, ThirdPersonCameraTarget, WorldCamera,
};
use psx_game_runtime::destructibles::RuntimeDestructibles;
use psx_gpu::material::TextureWindow;
use psx_grid::collision::{GridCollision, GridTraceProvider};
use psx_grid::contents::{contents_of_liquid_kind, LiquidContentsSample, CONTENTS_EMPTY};
#[cfg(target_arch = "mips")]
use psx_grid::render::load_gte_clip_planes;
use psx_grid::render::{
    load_view_rotation, FrustumPlanes, GridFrame, GridMaterial, GridPhase, GridRenderer,
    RoomCamera, ViewProjection, ViewTransform, CLIP_ALL_PLANES,
};
use psx_grid::{CollisionView, FormatError, LevelView, RoomView, LEVEL_HAS_LIQUIDS, SECTOR_BYTES};
use psx_gte::math::{Vec3I16 as GteVec3I16, Vec3I32 as GteVec3I32};
use psx_gte::scene;
use psx_level::{find_asset_of_kind, world_object_flags, AssetId, AssetKind};

use crate::generated::{ASSETS, WORLD_OBJECTS};
use crate::world_common::{
    view_camera, world_view_rotation, CountingTraceProvider, VisibilityBounds,
};
use crate::world_objects_runtime::WorldObjectVisibility;
use crate::{ensure_room_texture_uploaded, ensure_texture_uploaded, telemetry, PROJECTION};

#[repr(C)]
struct Aligned<Bytes: ?Sized> {
    _align: [u32; 0],
    bytes: Bytes,
}

/// The cooked level core, word aligned.
static GRID_LEVEL: &[u8] = {
    static ALIGNED: &Aligned<[u8]> = &Aligned {
        _align: [],
        bytes: *include_bytes!("../generated/grid_level.pgl"),
    };
    &ALIGNED.bytes
};

/// Every room chunk, each starting on a 2048-byte boundary.
static GRID_ROOMS: &[u8] = {
    static ALIGNED: &Aligned<[u8]> = &Aligned {
        _align: [],
        bytes: *include_bytes!("../generated/grid_rooms.pgr"),
    };
    &ALIGNED.bytes
};

/// Portal hops around the player room that count as active.
const ACTIVE_HOPS: usize = 2;
/// Words in the room bitset; the format allows 256 rooms.
const ROOM_SET_WORDS: usize = 8;

/// Ticks between sight-line tests of one model instance.
const VIEW_REFRESH_TICKS: u32 = 4;

type RoomSet = [u32; ROOM_SET_WORDS];

fn box_contains(min: [i32; 3], max: [i32; 3], point: [i32; 3]) -> bool {
    (0..3).all(|axis| point[axis] >= min[axis] && point[axis] <= max[axis])
}

fn set_has(set: &RoomSet, room: usize) -> bool {
    set[room >> 5] & (1 << (room & 31)) != 0
}

fn set_add(set: &mut RoomSet, room: usize) {
    set[room >> 5] |= 1 << (room & 31);
}

#[derive(Debug)]
pub(super) enum GridRuntimeInitError {
    EmptyWorld,
    Level(FormatError),
    Room { room: usize, error: FormatError },
    TooManyRooms(usize),
    NoMaterials,
    Collision,
}

impl fmt::Display for GridRuntimeInitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyWorld => {
                formatter.write_str("manifest selected a grid world but embedded none")
            }
            Self::Level(e) => write!(formatter, "grid level core refused: {e}"),
            Self::Room { room, error } => write!(formatter, "grid room {room} refused: {error}"),
            Self::TooManyRooms(n) => {
                write!(formatter, "{n} grid rooms, at most {}", ROOM_SET_WORDS * 32)
            }
            Self::NoMaterials => formatter.write_str("grid level has no materials"),
            Self::Collision => formatter.write_str("grid collision could not be built"),
        }
    }
}

/// Resolve one level material to its VRAM binding. Returns `None` while the
/// texture upload is still queued.
fn resolve_material(material: &psx_grid::Material) -> Option<GridMaterial> {
    let asset_id = AssetId(material.texture_asset);
    let asset = find_asset_of_kind(ASSETS, asset_id, AssetKind::Texture).unwrap_or_else(|| {
        panic!(
            "grid material references missing texture asset {}",
            material.texture_asset
        )
    });
    assert!(
        !asset.bytes.is_empty(),
        "grid texture asset {} has no baked bytes",
        material.texture_asset
    );
    let texture = Texture::from_bytes(asset.bytes).unwrap_or_else(|_| {
        panic!(
            "grid texture asset {} is not a valid PSXT",
            material.texture_asset
        )
    });
    let slot = if texture.is_index_zero_transparent() || material.blend != 0 {
        ensure_texture_uploaded(asset_id, asset.bytes)
    } else {
        ensure_room_texture_uploaded(asset_id, asset.bytes)
    }?;
    if !slot.ready {
        return None;
    }
    let [u, v] = slot.texture_window.origin_texels();
    // Blend code in tpage bits 5-6: average 0, add 1, subtract 2, add-quarter 3.
    let (blend_bits, command_bits) = match material.blend {
        1 => (0u16, 0x0200_0000u32),
        2 => (1 << 5, 0x0200_0000),
        3 => (2 << 5, 0x0200_0000),
        4 => (3 << 5, 0x0200_0000),
        _ => (0, 0),
    };
    Some(GridMaterial {
        tpage: (slot.tpage_word & !0x0060) | blend_bits,
        clut: slot.clut_word,
        uv_origin: u16::from(u) | (u16::from(v) << 8),
        command_bits,
        // A tile spans 64 page-local texels; the packed add in the packet
        // writer must not carry between bytes.
        resident: u16::from(u) + 64 <= 256 && u16::from(v) + 64 <= 256,
        // The window that wraps raw UVs back onto this texture. A texture
        // without a window placement (a whole page) cannot repeat; its
        // `REPEAT` quads are skipped, not drawn wrong.
        window_word: if slot.texture_window == TextureWindow::NONE {
            0
        } else {
            slot.texture_window.word()
        },
    })
}

pub(super) struct GridRuntime {
    level: LevelView<'static>,
    rooms: &'static [RoomView<'static>],
    world: GridCollision<'static>,
    renderer: GridRenderer,
    materials: Vec<GridMaterial>,
    materials_latched: bool,
    projection: ViewProjection,
    /// Room the observer was last found in, the search hint.
    room_hint: usize,
    /// Rooms within [`ACTIVE_HOPS`] of the last observer room.
    active: RoomSet,
    active_for: Option<usize>,
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
    pub(super) fn load_manifest() -> Result<Self, GridRuntimeInitError> {
        crate::game_trace("editor-playtest: grid manifest begin");
        if GRID_LEVEL.is_empty() {
            return Err(GridRuntimeInitError::EmptyWorld);
        }
        let level = LevelView::parse(GRID_LEVEL).map_err(GridRuntimeInitError::Level)?;
        level.validate_full().map_err(GridRuntimeInitError::Level)?;
        if level.rooms.len() > ROOM_SET_WORDS * 32 {
            return Err(GridRuntimeInitError::TooManyRooms(level.rooms.len()));
        }
        if level.materials.is_empty() {
            return Err(GridRuntimeInitError::NoMaterials);
        }
        let mut rooms: Vec<RoomView<'static>> = Vec::with_capacity(level.rooms.len());
        let mut collisions: Vec<Option<CollisionView<'static>>> =
            Vec::with_capacity(level.rooms.len());
        for (index, record) in level.rooms.iter().enumerate() {
            let start = usize::try_from(record.pack_sector).unwrap_or(usize::MAX) * SECTOR_BYTES;
            let bytes = GRID_ROOMS.get(start..).ok_or(GridRuntimeInitError::Room {
                room: index,
                error: FormatError::Truncated,
            })?;
            let view = RoomView::parse(bytes)
                .map_err(|error| GridRuntimeInitError::Room { room: index, error })?;
            collisions.push(Some(view.col));
            rooms.push(view);
        }
        let rooms: &'static [RoomView<'static>] = rooms.leak();
        let collisions: &'static [Option<CollisionView<'static>>] = collisions.leak();
        let world = GridCollision::new(level, collisions).ok_or(GridRuntimeInitError::Collision)?;
        let renderer = GridRenderer::for_rooms(rooms.iter());
        let projection = ViewProjection {
            focal_length: PROJECTION.focal_length,
            half_width: i32::from(PROJECTION.screen_x),
            half_height: i32::from(PROJECTION.screen_y),
            ..ViewProjection::DEFAULT
        };
        crate::game_trace("editor-playtest: grid runtime assemble");
        Ok(Self {
            level,
            rooms,
            world,
            renderer,
            materials: alloc::vec![GridMaterial::default(); level.materials.len()],
            materials_latched: false,
            projection,
            room_hint: 0,
            active: [0; ROOM_SET_WORDS],
            active_for: None,
            last_world_packet_words: 0,
            camera_traces: 0,
            view_known: 0,
            view_seen: 0,
            view_phase: 0,
        })
    }

    // ------------------------------------------------------------------
    // Rooms, activation, visibility
    // ------------------------------------------------------------------

    /// Room containing `point`, updating the search hint.
    fn room_of(&mut self, point: [i32; 3]) -> Option<usize> {
        let located = self.world.locate(self.room_hint, point)?;
        self.room_hint = located.room;
        Some(located.room)
    }

    /// World-space bounds of room `index`.
    fn room_bounds(&self, index: usize) -> ([i32; 3], [i32; 3]) {
        let record = &self.level.rooms[index];
        let s = self.level.sector_units() as i32;
        let min = [
            record.origin[0],
            record.origin[1] + i32::from(record.min_y),
            record.origin[2],
        ];
        let max = [
            record.origin[0] + i32::from(record.width) * s,
            record.origin[1] + i32::from(record.max_y),
            record.origin[2] + i32::from(record.depth) * s,
        ];
        (min, max)
    }

    /// Rooms within [`ACTIVE_HOPS`] portal hops of `from`, nearest first.
    fn ring(&self, from: usize, hops: usize, out: &mut Vec<usize>) {
        out.clear();
        out.push(from);
        let mut frontier = 0usize;
        for _ in 0..hops {
            let end = out.len();
            while frontier < end {
                let room = out[frontier];
                frontier += 1;
                for portal in self.level.room_portals(room) {
                    let dest = usize::from(portal.dest);
                    if !out.contains(&dest) {
                        out.push(dest);
                    }
                }
            }
        }
    }

    /// Refresh the active room set around `observer`. Returns whether the
    /// observer is inside the world at all.
    fn activate(&mut self, observer: RoomPoint) -> bool {
        let Some(room) = self.room_of([observer.x, observer.y, observer.z]) else {
            return false;
        };
        if self.active_for != Some(room) {
            let mut ring = Vec::new();
            self.ring(room, ACTIVE_HOPS, &mut ring);
            self.active = [0; ROOM_SET_WORDS];
            for &r in &ring {
                set_add(&mut self.active, r);
            }
            self.active_for = Some(room);
        }
        true
    }

    fn point_active(&mut self, point: [i32; 3]) -> bool {
        self.world
            .locate(self.room_hint, point)
            .is_some_and(|l| set_has(&self.active, l.room))
    }

    fn bounds_active(&self, min: [i32; 3], max: [i32; 3]) -> bool {
        (0..self.rooms.len()).any(|room| {
            if !set_has(&self.active, room) {
                return false;
            }
            let (rmin, rmax) = self.room_bounds(room);
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
        [2, 1, 3].into_iter().any(|quarter_steps| {
            let y = min[1].saturating_add(height / 4 * quarter_steps);
            self.world.line_of_sight(from, [cx, y, cz])
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
        samples
            .into_iter()
            .any(|sample| self.world.line_of_sight(from, sample))
    }

    pub(super) fn visible_world_objects(
        &mut self,
        camera: WorldCamera,
        destructibles: &RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
    ) -> WorldObjectVisibility {
        let observer = RoomPoint::new(camera.position.x, camera.position.y, camera.position.z);
        if !self.activate(observer) {
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
            if !self.bounds_active(object.bounds_min, object.bounds_max) {
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
            let code = self.world.contents_at([position.x, y, position.z]);
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

    /// Resolve every level material through the playtest VRAM owner. Returns
    /// `true` only when every queued upload is ready.
    pub(super) fn refresh_materials(&mut self) -> bool {
        if self.materials_latched {
            return true;
        }
        let mut ready = true;
        for (slot, material) in self.level.materials.iter().enumerate() {
            match resolve_material(material) {
                Some(binding) => self.materials[slot] = binding,
                None => {
                    self.materials[slot] = GridMaterial::default();
                    ready = false;
                }
            }
        }
        self.materials_latched = ready;
        ready
    }

    /// Drop the resolved material table so the next `refresh_materials`
    /// rebuilds it.
    pub(super) fn invalidate_materials(&mut self) {
        self.materials_latched = false;
        for material in &mut self.materials {
            material.resident = false;
        }
    }

    pub(super) fn materials_ready(&self) -> bool {
        self.materials_latched
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
        let mut provider = GridTraceProvider::new(&self.world);
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
        let mut provider = GridTraceProvider::new(&self.world);
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
        let mut provider = GridTraceProvider::new(&self.world);
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
        let mut provider = GridTraceProvider::new(&self.world);
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
        let mut provider = GridTraceProvider::new(&self.world);
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
        let mut provider = GridTraceProvider::for_camera(&self.world);
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

    /// Rooms to draw: the camera room's neighbourhood that touches the
    /// frustum, or every room that does when the camera is outside the world.
    fn visible_rooms(&mut self, eye: [i32; 3], frustum: &FrustumPlanes, out: &mut Vec<usize>) {
        out.clear();
        let mut ring = Vec::new();
        match self.room_of(eye) {
            Some(room) => self.ring(room, ACTIVE_HOPS, &mut ring),
            None => ring.extend(0..self.rooms.len()),
        }
        let clamp = |v: i32| v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        for room in ring {
            let (min, max) = self.room_bounds(room);
            let lo = GteVec3I16 {
                x: clamp(min[0]),
                y: clamp(min[1]),
                z: clamp(min[2]),
            };
            let hi = GteVec3I16 {
                x: clamp(max[0]),
                y: clamp(max[1]),
                z: clamp(max[2]),
            };
            if frustum.cull_aabb(lo, hi, CLIP_ALL_PLANES).is_some() {
                out.push(room);
            }
        }
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
        assert!(
            self.materials_ready(),
            "grid render started before texture residency"
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
        let world_frustum =
            FrustumPlanes::from_view(&view.rotation, view.translation, self.projection);
        // The box tests below read the plane dots from the GTE on the guest,
        // so the world frustum has to be loaded first.
        #[cfg(target_arch = "mips")]
        load_gte_clip_planes(&world_frustum.planes);
        let mut rooms: Vec<usize> = Vec::new();
        self.visible_rooms(eye, &world_frustum, &mut rooms);
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
                for &room_index in &rooms {
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
                    let room = &self.rooms[room_index];
                    let frame = self.renderer.draw_room_with_marks(
                        room,
                        &eye_view,
                        &self.materials,
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
                u32::from(total.material_missing),
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
