use super::*;

mod sky;
pub use sky::*;
/// Snapshot of a [`WorldGrid`]'s authoring footprint + cooked-
/// byte estimate. Cheap to compute (single sector pass); the
/// editor recomputes it whenever the inspector for a Room
/// repaints.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorldGridBudget {
    /// Grid width in sectors.
    pub width: u16,
    /// Grid depth in sectors.
    pub depth: u16,
    /// `width * depth`. `.psxw` stores a sector record for
    /// every cell whether it's populated or not, so this is
    /// what the wire-size formula multiplies against.
    pub total_cells: usize,
    /// Cells that have any geometry (floor / ceiling / walls).
    /// Useful for surface-area / drawcall estimates; not the
    /// driver of the byte budget.
    pub populated_cells: usize,
    pub floors: usize,
    pub ceilings: usize,
    pub walls: usize,
    pub horizontal_overrides: usize,
    pub triangles: usize,
    /// Current `.psxw` geometry wire size. The format stores a
    /// sector record for **every** cell, so this uses `total_cells`,
    /// not `populated_cells`.
    pub psxw_bytes: usize,
    /// Additional bytes appended when static per-vertex lighting is
    /// baked into `.psxw` v3 for Embedded Play.
    pub static_light_table_bytes: usize,
    /// Full Embedded Play room asset size: geometry `.psxw` plus the
    /// baked static-light table.
    pub psxw_static_lit_bytes: usize,
    /// Estimated size if we shipped the future compact format
    /// described in `docs/world-format-roadmap.md` (28-byte
    /// sectors, 12-byte walls). Surfaced as a planning aid, not
    /// a contract -- no live `.psxw` is ever this size today.
    pub future_compact_estimated_bytes: usize,
}

impl WorldGridBudget {
    /// `true` if any base geometry cap is exceeded. Mirrors the
    /// generic world-cooker validation before Embedded Play appends
    /// static lighting.
    pub fn over_budget(&self) -> bool {
        self.width > MAX_ROOM_WIDTH
            || self.depth > MAX_ROOM_DEPTH
            || self.triangles > MAX_ROOM_TRIANGLES
            || self.psxw_bytes > MAX_ROOM_BYTES
    }

    /// `true` if Embedded Play's static-lit room asset would exceed
    /// the current runtime chunk limits.
    pub fn static_lit_over_budget(&self) -> bool {
        self.width > MAX_ROOM_WIDTH
            || self.depth > MAX_ROOM_DEPTH
            || self.triangles > MAX_ROOM_TRIANGLES
            || self.psxw_static_lit_bytes > MAX_ROOM_BYTES
    }
}

pub(crate) const ASSET_HEADER_BYTES: usize = 12;
pub(crate) const WORLD_HEADER_BYTES: usize = psxed_format::world::WorldHeader::SIZE;
pub(crate) const PSXW_SECTOR_BYTES: usize = psxed_format::world::SectorRecord::SIZE;
pub(crate) const PSXW_WALL_BYTES: usize = psxed_format::world::WallRecord::SIZE;
pub(crate) const PSXW_HORIZONTAL_OVERRIDE_BYTES: usize =
    psxed_format::world::HorizontalOverrideRecord::SIZE;
pub(crate) const PSXW_SURFACE_LIGHT_BYTES: usize = psxed_format::world::SurfaceLightRecord::SIZE;
pub(crate) const FUTURE_COMPACT_SECTOR_BYTES: usize = 28;
pub(crate) const FUTURE_COMPACT_WALL_BYTES: usize = 12;

pub(crate) const fn default_ambient_color() -> [u8; 3] {
    [32, 32, 32]
}

pub(crate) const fn default_fog_color() -> [u8; 3] {
    [24, 28, 34]
}

pub(crate) const fn default_atmosphere_enabled() -> bool {
    true
}

pub(crate) const fn default_atmosphere_color() -> [u8; 3] {
    [58, 52, 44]
}

pub(crate) const fn default_atmosphere_density() -> i32 {
    44
}

pub(crate) const fn default_atmosphere_fall_speed_q4() -> i32 {
    7
}

pub(crate) const fn default_atmosphere_wind_speed_q4() -> i32 {
    2
}

pub(crate) const fn default_sky_top_color() -> [u8; 3] {
    [7, 8, 14]
}

pub(crate) const fn default_sky_horizon_color() -> [u8; 3] {
    [32, 30, 34]
}

pub(crate) const fn default_sky_lower_color() -> [u8; 3] {
    [5, 7, 12]
}

pub(crate) const fn default_sky_horizon_percent() -> u8 {
    58
}

pub(crate) const fn default_sky_horizon_thickness_percent() -> u8 {
    8
}

pub(crate) const fn default_sky_horizon_glow_percent() -> u8 {
    68
}

pub(crate) const fn default_sky_horizon_glow_yaw_degrees() -> i16 {
    72
}

pub(crate) const fn default_sky_sun_enabled() -> bool {
    false
}

pub(crate) fn default_sky_sun_color() -> [u8; 3] {
    [255, 218, 150]
}

pub(crate) fn default_sky_sun_border_color() -> [u8; 3] {
    [255, 128, 78]
}

pub(crate) const fn default_sky_sun_yaw_degrees() -> i16 {
    72
}

pub(crate) const fn default_sky_sun_pitch_degrees() -> i16 {
    22
}

pub(crate) const fn default_sky_sun_size_percent() -> u8 {
    18
}

pub(crate) const fn default_sky_sun_glow_percent() -> u8 {
    72
}

pub(crate) const fn default_sky_sun_glow_size_percent() -> u8 {
    64
}

pub(crate) const fn default_sky_mountain_height_percent() -> u8 {
    55
}

pub(crate) fn default_sky_mountain_top_color() -> [u8; 3] {
    [84, 96, 124]
}

pub(crate) fn default_sky_mountain_base_color() -> [u8; 3] {
    [24, 28, 42]
}

pub(crate) const fn default_sky_mountain_gap_percent() -> u8 {
    22
}

pub(crate) const fn default_sky_mountain_roughness_percent() -> u8 {
    78
}

pub(crate) const fn default_sky_mountain_layer_count() -> u8 {
    2
}

/// Maximum authored distant mountain height. Values above 100 are
/// intentionally allowed now that runtime uses a baked panorama.
pub const SKY_MOUNTAIN_HEIGHT_PERCENT_MAX: u8 = 200;

/// Minimum number of horizontal cyclorama subdivisions.
pub const SKYBOX_COLUMNS_MIN: u8 = 4;
/// Maximum number of horizontal cyclorama subdivisions.
pub const SKYBOX_COLUMNS_MAX: u8 = 32;
/// Default number of horizontal cyclorama subdivisions.
pub const SKYBOX_COLUMNS_DEFAULT: u8 = 16;
/// Minimum number of vertical cyclorama subdivisions.
pub const SKYBOX_ROWS_MIN: u8 = 3;
/// Maximum number of vertical cyclorama subdivisions.
pub const SKYBOX_ROWS_MAX: u8 = 20;
/// Default number of vertical cyclorama subdivisions.
pub const SKYBOX_ROWS_DEFAULT: u8 = 10;

pub(crate) const fn default_skybox_columns() -> u8 {
    SKYBOX_COLUMNS_DEFAULT
}

pub(crate) const fn default_skybox_rows() -> u8 {
    SKYBOX_ROWS_DEFAULT
}

pub(crate) const fn default_sky_match_room_fog() -> bool {
    true
}

pub(crate) const fn default_far_vista_radius() -> i32 {
    18_000
}

pub(crate) const fn default_far_vista_height() -> i32 {
    4_096
}

pub(crate) const fn default_far_vista_vertical_offset() -> i32 {
    -512
}

pub(crate) const fn default_far_vista_segments() -> u8 {
    12
}

pub(crate) const fn default_far_vista_tint() -> [u8; 3] {
    [54, 58, 62]
}

pub(crate) const fn default_far_vista_match_room_fog() -> bool {
    true
}

/// Maximum number of individually textured cards in a far-vista ring.
pub const FAR_VISTA_TEXTURE_PANEL_COUNT: usize = 16;

pub(crate) const fn default_far_vista_texture_panels(
) -> [Option<ResourceId>; FAR_VISTA_TEXTURE_PANEL_COUNT] {
    [None; FAR_VISTA_TEXTURE_PANEL_COUNT]
}

pub(crate) const fn default_fog_near() -> i32 {
    4096
}

pub(crate) const fn default_fog_far() -> i32 {
    16384
}

pub(crate) const fn default_light_color() -> [u8; 3] {
    [255, 240, 200]
}

/// Distant scenery ring configuration inherited by descendant Rooms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FarVistaSettings {
    /// Whether the far vista ring should be drawn.
    #[serde(default)]
    pub enabled: bool,
    /// Optional transparent 4bpp texture slice repeated around the
    /// ring. When missing, renderers draw a tinted placeholder band.
    #[serde(default)]
    pub texture: Option<ResourceId>,
    /// Optional per-card transparent 4bpp textures. Non-empty panel
    /// assignments take precedence over [`Self::texture`].
    #[serde(default = "default_far_vista_texture_panels")]
    pub texture_panels: [Option<ResourceId>; FAR_VISTA_TEXTURE_PANEL_COUNT],
    /// Radius from the active camera/player in engine units.
    #[serde(default = "default_far_vista_radius")]
    pub radius: i32,
    /// Ring height in engine units.
    #[serde(default = "default_far_vista_height")]
    pub height: i32,
    /// Bottom-edge offset from the camera height in engine units.
    #[serde(default = "default_far_vista_vertical_offset")]
    pub vertical_offset: i32,
    /// Number of cards around the cylinder.
    #[serde(default = "default_far_vista_segments")]
    pub segments: u8,
    /// World yaw rotation in degrees.
    #[serde(default)]
    pub rotation_degrees: i16,
    /// Flat tint used for placeholder cards and textured modulation.
    #[serde(default = "default_far_vista_tint")]
    pub tint: [u8; 3],
    /// Blend tint toward the room fog colour when fog is enabled.
    #[serde(default = "default_far_vista_match_room_fog")]
    pub match_room_fog: bool,
}

impl FarVistaSettings {
    /// Resolve authored far-vista values against room-local fog metadata.
    pub fn resolved_for_room(
        self,
        fog_enabled: bool,
        fog_color: [u8; 3],
    ) -> ResolvedFarVistaSettings {
        let tint = if self.match_room_fog && fog_enabled {
            blend_rgb(self.tint, fog_color, 128)
        } else {
            self.tint
        };
        ResolvedFarVistaSettings {
            enabled: self.enabled,
            texture: self.texture,
            texture_panels: self.texture_panels,
            radius: self.radius.clamp(1_024, 65_535),
            height: self.height.clamp(128, 32_768),
            vertical_offset: self.vertical_offset.clamp(-32_768, 32_768),
            segments: self.segments.clamp(3, 16),
            rotation_degrees: self.rotation_degrees,
            tint,
        }
    }
}

impl Default for FarVistaSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            texture: None,
            texture_panels: default_far_vista_texture_panels(),
            radius: default_far_vista_radius(),
            height: default_far_vista_height(),
            vertical_offset: default_far_vista_vertical_offset(),
            segments: default_far_vista_segments(),
            rotation_degrees: 0,
            tint: default_far_vista_tint(),
            match_room_fog: default_far_vista_match_room_fog(),
        }
    }
}

/// Far-vista values after room-fog matching and validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedFarVistaSettings {
    /// Whether the ring should be drawn.
    pub enabled: bool,
    /// Optional transparent texture slice.
    pub texture: Option<ResourceId>,
    /// Optional per-card transparent texture slices.
    pub texture_panels: [Option<ResourceId>; FAR_VISTA_TEXTURE_PANEL_COUNT],
    /// Radius from camera/player in engine units.
    pub radius: i32,
    /// Ring height in engine units.
    pub height: i32,
    /// Bottom-edge offset from camera height in engine units.
    pub vertical_offset: i32,
    /// Number of cards around the cylinder.
    pub segments: u8,
    /// World yaw rotation in degrees.
    pub rotation_degrees: i16,
    /// Resolved tint.
    pub tint: [u8; 3],
}

/// World-level third-person camera configuration inherited by
/// descendant Rooms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldCameraSettings {
    /// Preferred trailing distance from focus to camera.
    #[serde(default = "default_world_camera_distance")]
    pub distance: i32,
    /// Camera origin height above the player origin.
    #[serde(default = "default_world_camera_height")]
    pub height: i32,
    /// Look-at height above the player origin.
    #[serde(default = "default_world_camera_target_height")]
    pub target_height: i32,
    /// Additional lock-on camera elevation as a percentage of `height`.
    #[serde(default = "default_world_camera_lock_rise_percent")]
    pub lock_rise_percent: u8,
    /// Minimum camera origin height above the sampled floor.
    #[serde(default = "default_world_camera_min_floor_clearance")]
    pub min_floor_clearance: i32,
    /// Manual orbit input speed level. Higher values turn faster.
    #[serde(default = "default_world_camera_orbit_speed_level")]
    pub orbit_speed_level: u8,
    /// Camera origin follow lag shift. Lower values move faster.
    #[serde(default = "default_world_camera_position_lag_shift")]
    pub position_lag_shift: u8,
    /// Camera focus follow lag shift. Lower values move faster.
    #[serde(default = "default_world_camera_focus_lag_shift")]
    pub focus_lag_shift: u8,
    /// Collision boom recovery lag shift. Lower values move faster.
    #[serde(default = "default_world_camera_distance_lag_shift")]
    pub distance_lag_shift: u8,
}

impl WorldCameraSettings {
    /// Clamp authored values to runtime-safe third-person camera ranges.
    pub fn normalized(self) -> Self {
        Self {
            distance: self
                .distance
                .clamp(MIN_WORLD_CAMERA_DISTANCE, MAX_WORLD_CAMERA_DISTANCE),
            height: self.height.clamp(0, MAX_WORLD_CAMERA_HEIGHT),
            target_height: self.target_height.clamp(0, MAX_WORLD_CAMERA_HEIGHT),
            lock_rise_percent: self
                .lock_rise_percent
                .min(MAX_WORLD_CAMERA_LOCK_RISE_PERCENT),
            min_floor_clearance: self
                .min_floor_clearance
                .clamp(0, MAX_WORLD_CAMERA_MIN_FLOOR_CLEARANCE),
            orbit_speed_level: self.orbit_speed_level.clamp(
                MIN_WORLD_CAMERA_ORBIT_SPEED_LEVEL,
                MAX_WORLD_CAMERA_ORBIT_SPEED_LEVEL,
            ),
            position_lag_shift: self.position_lag_shift.min(MAX_WORLD_CAMERA_LAG_SHIFT),
            focus_lag_shift: self.focus_lag_shift.min(MAX_WORLD_CAMERA_LAG_SHIFT),
            distance_lag_shift: self.distance_lag_shift.min(MAX_WORLD_CAMERA_LAG_SHIFT),
        }
    }
}

impl Default for WorldCameraSettings {
    fn default() -> Self {
        Self {
            distance: default_world_camera_distance(),
            height: default_world_camera_height(),
            target_height: default_world_camera_target_height(),
            lock_rise_percent: default_world_camera_lock_rise_percent(),
            min_floor_clearance: default_world_camera_min_floor_clearance(),
            orbit_speed_level: default_world_camera_orbit_speed_level(),
            position_lag_shift: default_world_camera_position_lag_shift(),
            focus_lag_shift: default_world_camera_focus_lag_shift(),
            distance_lag_shift: default_world_camera_distance_lag_shift(),
        }
    }
}

/// Minimum camera-space far plane used by runtime world drawing.
pub const MIN_WORLD_DRAW_DISTANCE: i32 = 4_096;
/// Maximum camera-space far plane exposed for playtest experimentation.
pub const MAX_WORLD_DRAW_DISTANCE: i32 = 262_144;
/// Minimum active streamed chunk radius, in world sectors.
pub const MIN_WORLD_CHUNK_ACTIVATION_RADIUS_SECTORS: i32 = 4;
/// Maximum active streamed chunk radius, in world sectors.
pub const MAX_WORLD_CHUNK_ACTIVATION_RADIUS_SECTORS: i32 = 256;
/// Minimum precomputed cell-visibility traversal radius.
pub const MIN_WORLD_VISIBILITY_RADIUS: u16 = 4;
/// Maximum precomputed cell-visibility traversal radius.
pub const MAX_WORLD_VISIBILITY_RADIUS: u16 = 96;
/// Smallest resident portal-room budget accepted by the runtime.
/// One portal needs at least current + adjacent room residency.
pub const MIN_WORLD_STREAMING_RESIDENT_CHUNKS: u8 = 2;
/// Default portal-room residency budget used by the playtest runtime.
pub const DEFAULT_WORLD_STREAMING_RESIDENT_CHUNKS: u8 = 10;
/// Largest portal-room residency budget supported by the current runtime.
pub const MAX_WORLD_STREAMING_RESIDENT_CHUNKS: u8 = 32;
/// Smallest portal-room visible-window budget accepted by the runtime.
pub const MIN_WORLD_STREAMING_VISIBLE_CHUNKS: u8 = 2;
/// Default portal-room visible-window budget used by the playtest runtime.
pub const DEFAULT_WORLD_STREAMING_VISIBLE_CHUNKS: u8 = DEFAULT_WORLD_STREAMING_RESIDENT_CHUNKS;
/// Largest portal-room visible-window budget supported by the current runtime.
pub const MAX_WORLD_STREAMING_VISIBLE_CHUNKS: u8 = 32;
/// Minimum authored gravity, in engine units per 60 Hz tick squared.
pub const MIN_WORLD_GRAVITY_PER_TICK: i32 = 0;
/// Maximum authored gravity, in engine units per 60 Hz tick squared.
pub const MAX_WORLD_GRAVITY_PER_TICK: i32 = 2_048;
/// Q8 identity weight (`256 = 1.0x`) for entity physics bodies.
pub const PHYSICS_WEIGHT_ONE_Q8: u16 = 256;
/// Smallest authored entity weight multiplier.
pub const MIN_PHYSICS_WEIGHT_Q8: u16 = 1;
/// Largest authored entity weight multiplier.
pub const MAX_PHYSICS_WEIGHT_Q8: u16 = 4_096;

pub(crate) const fn default_world_draw_distance() -> i32 {
    25_000
}

pub(crate) const fn default_world_chunk_activation_radius_sectors() -> i32 {
    64
}

pub(crate) const fn default_world_visibility_radius() -> u16 {
    32
}

pub(crate) const fn default_world_streaming_resident_chunks() -> u8 {
    DEFAULT_WORLD_STREAMING_RESIDENT_CHUNKS
}

pub(crate) const fn default_world_streaming_visible_chunks() -> u8 {
    DEFAULT_WORLD_STREAMING_VISIBLE_CHUNKS
}

pub(crate) const fn default_world_gravity_per_tick() -> i32 {
    96
}

pub(crate) const fn default_physics_weight_q8() -> u16 {
    PHYSICS_WEIGHT_ONE_Q8
}

/// Runtime culling knobs inherited by descendant Rooms from their
/// nearest World node. These are editor/playtest controls, not per-room
/// geometry data, so older projects safely load with the defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldCullingSettings {
    /// Camera-space far plane used for world, actor, and prop drawing.
    #[serde(default = "default_world_draw_distance")]
    pub draw_distance: i32,
    /// Radius around the current room/player used to keep chunks active.
    #[serde(default = "default_world_chunk_activation_radius_sectors")]
    pub chunk_activation_radius_sectors: i32,
    /// Radius used while cooking each room's visibility/PVS cell graph.
    #[serde(default = "default_world_visibility_radius")]
    pub visibility_radius: u16,
}

impl WorldCullingSettings {
    /// Clamp authored values to runtime-safe ranges.
    pub fn normalized(self) -> Self {
        Self {
            draw_distance: self
                .draw_distance
                .clamp(MIN_WORLD_DRAW_DISTANCE, MAX_WORLD_DRAW_DISTANCE),
            chunk_activation_radius_sectors: self.chunk_activation_radius_sectors.clamp(
                MIN_WORLD_CHUNK_ACTIVATION_RADIUS_SECTORS,
                MAX_WORLD_CHUNK_ACTIVATION_RADIUS_SECTORS,
            ),
            visibility_radius: self
                .visibility_radius
                .clamp(MIN_WORLD_VISIBILITY_RADIUS, MAX_WORLD_VISIBILITY_RADIUS),
        }
    }
}

impl Default for WorldCullingSettings {
    fn default() -> Self {
        Self {
            draw_distance: default_world_draw_distance(),
            chunk_activation_radius_sectors: default_world_chunk_activation_radius_sectors(),
            visibility_radius: default_world_visibility_radius(),
        }
    }
}

/// Portal-room streaming controls inherited by descendant Rooms from their
/// nearest World node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldStreamingSettings {
    /// Resident streaming budget, measured in runtime portal-room units.
    /// The playtest runtime converts this to more resident slots when the cooked
    /// rooms are smaller than the maximum stream slot size.
    #[serde(default = "default_world_streaming_resident_chunks")]
    pub resident_chunk_limit: u8,
    /// Maximum portal rooms selected for drawing/collision by the runtime.
    ///
    /// A serialized zero is treated as a legacy project value and inherits the
    /// resident chunk limit during normalization.
    #[serde(default)]
    pub visible_chunk_limit: u8,
}

impl WorldStreamingSettings {
    /// Clamp authored values to cooker-safe ranges.
    pub fn normalized(self) -> Self {
        let resident_chunk_limit = self.resident_chunk_limit.clamp(
            MIN_WORLD_STREAMING_RESIDENT_CHUNKS,
            MAX_WORLD_STREAMING_RESIDENT_CHUNKS,
        );
        let visible_chunk_limit = if self.visible_chunk_limit == 0 {
            resident_chunk_limit
        } else {
            self.visible_chunk_limit
        }
        .clamp(
            MIN_WORLD_STREAMING_VISIBLE_CHUNKS,
            MAX_WORLD_STREAMING_VISIBLE_CHUNKS,
        )
        .min(resident_chunk_limit);
        Self {
            resident_chunk_limit,
            visible_chunk_limit,
        }
    }
}

impl Default for WorldStreamingSettings {
    fn default() -> Self {
        Self {
            resident_chunk_limit: default_world_streaming_resident_chunks(),
            visible_chunk_limit: default_world_streaming_visible_chunks(),
        }
    }
}

/// World-level physics settings inherited by descendant rooms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldPhysicsSettings {
    /// Downward acceleration applied by character/controller physics,
    /// in engine units per fixed 60 Hz tick squared.
    #[serde(default = "default_world_gravity_per_tick")]
    pub gravity_per_tick: i32,
}

impl WorldPhysicsSettings {
    /// Clamp authored values to runtime-safe integer ranges.
    pub fn normalized(self) -> Self {
        Self {
            gravity_per_tick: self
                .gravity_per_tick
                .clamp(MIN_WORLD_GRAVITY_PER_TICK, MAX_WORLD_GRAVITY_PER_TICK),
        }
    }
}

impl Default for WorldPhysicsSettings {
    fn default() -> Self {
        Self {
            gravity_per_tick: default_world_gravity_per_tick(),
        }
    }
}

/// Per-entity physics body settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicsBodySettings {
    /// Gravity multiplier in Q8 fixed point (`256 = 1.0x`).
    #[serde(default = "default_physics_weight_q8")]
    pub weight_q8: u16,
}

/// One authored cell occupied by a [`WaterVolumeSettings`] node.
///
/// Coordinates live in the room's persistent world-cell space rather than
/// array indices, so extending a grid on its negative side never moves an
/// already-painted water footprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WaterVolumeCell {
    /// World-cell X coordinate.
    pub x: i32,
    /// World-cell Z coordinate.
    pub z: i32,
}

impl WaterVolumeCell {
    /// Build a cell from its persistent world-grid coordinates.
    pub const fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }
}

/// Authored water behaviour shared by every cell in one volume.
///
/// Water never provides swimming. Every painted cell is a floor-bound volume:
/// its bottom is the authored terrain and its surface sits
/// `height_above_floor` units above that tile's lowest rendered point.
/// Non-lethal cells scale ground movement; cells at or beyond `lethal_depth`
/// trigger the fall/death flow once the character drops below the surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaterVolumeSettings {
    /// Water surface height measured upward from each painted tile's low point.
    #[serde(default = "default_water_height", alias = "surface_height")]
    pub height_above_floor: u16,
    /// Depth at which the volume becomes lethal.
    #[serde(default = "default_water_lethal_depth")]
    pub lethal_depth: u16,
    /// Ground movement speed retained while wading, as a percentage.
    #[serde(default = "default_water_movement_percent")]
    pub movement_percent: u8,
    /// Simulation ticks between lethal submersion and respawn.
    #[serde(default = "default_water_death_delay_ticks")]
    pub death_delay_ticks: u8,
    /// Distance below the surface required before lethal water commits death.
    #[serde(default = "default_water_death_submerge_depth")]
    pub death_submerge_depth: u16,
}

impl WaterVolumeSettings {
    /// Clamp authoring values to the compact runtime contract.
    pub fn normalized(self) -> Self {
        Self {
            height_above_floor: self.height_above_floor.max(1),
            lethal_depth: self.lethal_depth.max(1),
            movement_percent: self.movement_percent.clamp(1, 100),
            death_delay_ticks: self.death_delay_ticks.max(1),
            death_submerge_depth: self.death_submerge_depth.max(1),
        }
    }
}

impl Default for WaterVolumeSettings {
    fn default() -> Self {
        Self {
            height_above_floor: default_water_height(),
            lethal_depth: default_water_lethal_depth(),
            movement_percent: default_water_movement_percent(),
            death_delay_ticks: default_water_death_delay_ticks(),
            death_submerge_depth: default_water_death_submerge_depth(),
        }
    }
}

pub const fn default_water_height() -> u16 {
    64
}

pub const fn default_water_lethal_depth() -> u16 {
    384
}

pub const fn default_water_movement_percent() -> u8 {
    70
}

pub const fn default_water_death_delay_ticks() -> u8 {
    45
}

pub const fn default_water_death_submerge_depth() -> u16 {
    64
}

impl PhysicsBodySettings {
    /// Clamp authored values to runtime-safe integer ranges.
    pub fn normalized(self) -> Self {
        Self {
            weight_q8: self
                .weight_q8
                .clamp(MIN_PHYSICS_WEIGHT_Q8, MAX_PHYSICS_WEIGHT_Q8),
        }
    }
}

impl Default for PhysicsBodySettings {
    fn default() -> Self {
        Self {
            weight_q8: default_physics_weight_q8(),
        }
    }
}

pub(crate) fn face_triangle_count(face: &GridHorizontalFace) -> usize {
    if face.is_triangle() {
        1
    } else {
        2
    }
}

pub(crate) fn horizontal_face_needs_runtime_override(face: &GridHorizontalFace) -> bool {
    face.is_triangle() || !face.triangle_overrides.is_empty()
}

/// Engine-style grid world authored by a scene node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldGrid {
    /// Width in sectors.
    pub width: u16,
    /// Depth in sectors.
    pub depth: u16,
    /// Engine units per sector.
    pub sector_size: i32,
    /// Flat `[x * depth + z]` sector storage. `None` means no sector.
    pub sectors: Vec<Option<GridSector>>,
    /// World offset (in cell units) of cell index `(0, 0)`. Lets the
    /// editor extend the room into negative `X` / `Z` without
    /// renumbering existing cells: a `-X` grow shifts sectors by
    /// `+1` in X, decrements `origin.x` by `1`, and the renderer's
    /// world coord = `(origin + index) * sector_size`. Default
    /// `[0, 0]` for backward compat with already-saved projects.
    #[serde(default)]
    pub origin: [i32; 2],
    /// Vertical placement of the room, in engine units, used for
    /// adaptive-style stacked rooms. This is the room's `Y`
    /// companion to the cell-unit `X` / `Z` [`Self::origin`]; unlike
    /// `origin` (sectors) it is already in engine units so the
    /// integer-only runtime cook can emit it without a per-cell
    /// conversion. Authored from the Room node's
    /// `Transform3::translation[1]` (sectors) at cook time. Default
    /// `0` keeps every existing room pinned to the ground plane, so
    /// projects saved before vertical placement load unchanged.
    #[serde(default)]
    pub elevation: i32,
    /// Room ambient color used as editor/cooker metadata.
    #[serde(default = "default_ambient_color")]
    pub ambient_color: [u8; 3],
    /// Whether PS1 depth cue/fog should be cooked for this grid.
    pub fog_enabled: bool,
    /// Depth-cue far color for this room.
    #[serde(default = "default_fog_color")]
    pub fog_color: [u8; 3],
    /// Start distance for authored fog/depth cue in engine units.
    #[serde(default = "default_fog_near")]
    pub fog_near: i32,
    /// Fully-fogged distance for authored fog/depth cue in engine units.
    #[serde(default = "default_fog_far")]
    pub fog_far: i32,
    /// Whether a cheap screen-space falling particle pass should render in this room.
    #[serde(default = "default_atmosphere_enabled")]
    pub atmosphere_enabled: bool,
    /// Base particle colour for ash/snow style room atmosphere.
    #[serde(default = "default_atmosphere_color")]
    pub atmosphere_color: [u8; 3],
    /// Number of screen-space particles to draw.
    #[serde(default = "default_atmosphere_density")]
    pub atmosphere_density: i32,
    /// Base vertical particle speed, in 1/16 pixel-per-vblank units.
    #[serde(default = "default_atmosphere_fall_speed_q4")]
    pub atmosphere_fall_speed_q4: i32,
    /// Base horizontal particle speed, in 1/16 pixel-per-vblank units.
    #[serde(default = "default_atmosphere_wind_speed_q4")]
    pub atmosphere_wind_speed_q4: i32,
    /// Additional floors stacked above this one. Floor 0 is this grid;
    /// floor `i` is `floors_above[i - 1]`. Each floor is its own free
    /// grid (footprint + heights), auto-stacked just above the floor
    /// below. Empty for single-floor rooms, so projects saved before
    /// floors load unchanged. Only the base (floor 0) grid uses this;
    /// upper-floor grids keep it empty.
    #[serde(default)]
    pub floors_above: Vec<WorldGrid>,
}

impl WorldGrid {
    fn empty_stacked_floor_like(source: &Self, elevation: i32) -> Self {
        let mut floor = Self::empty(source.width, source.depth, source.sector_size);
        floor.origin = source.origin;
        floor.elevation = elevation;
        floor.ambient_color = source.ambient_color;
        floor.fog_enabled = source.fog_enabled;
        floor.fog_color = source.fog_color;
        floor.fog_near = source.fog_near;
        floor.fog_far = source.fog_far;
        floor.atmosphere_enabled = source.atmosphere_enabled;
        floor.atmosphere_color = source.atmosphere_color;
        floor.atmosphere_density = source.atmosphere_density;
        floor.atmosphere_fall_speed_q4 = source.atmosphere_fall_speed_q4;
        floor.atmosphere_wind_speed_q4 = source.atmosphere_wind_speed_q4;
        floor
    }

    /// Create an empty sparse grid.
    pub fn empty(width: u16, depth: u16, sector_size: i32) -> Self {
        let len = width as usize * depth as usize;
        Self {
            width,
            depth,
            sector_size,
            sectors: vec![None; len],
            origin: [0, 0],
            elevation: 0,
            ambient_color: default_ambient_color(),
            fog_enabled: true,
            fog_color: default_fog_color(),
            fog_near: default_fog_near(),
            fog_far: default_fog_far(),
            atmosphere_enabled: default_atmosphere_enabled(),
            atmosphere_color: default_atmosphere_color(),
            atmosphere_density: default_atmosphere_density(),
            atmosphere_fall_speed_q4: default_atmosphere_fall_speed_q4(),
            atmosphere_wind_speed_q4: default_atmosphere_wind_speed_q4(),
            floors_above: Vec::new(),
        }
    }

    /// Number of floors in this room (1 for a single-floor room). Floor 0
    /// is this base grid; floor `i` is `floors_above[i - 1]`.
    pub fn floor_count(&self) -> usize {
        1 + self.floors_above.len()
    }

    /// Floor `i` (0 = this base grid).
    pub fn floor(&self, i: usize) -> Option<&WorldGrid> {
        if i == 0 {
            Some(self)
        } else {
            self.floors_above.get(i - 1)
        }
    }

    /// Remove exact duplicate wall segments from every sector on THIS floor.
    ///
    /// Stacked floors are separate grids, so callers wanting a whole room must
    /// walk `floor_count()`; [`WorldGrid::dedupe_duplicate_walls_all_floors`]
    /// does that. Returns how many segments were removed.
    pub fn dedupe_duplicate_walls(&mut self) -> usize {
        let mut removed = 0;
        for sector in self.sectors.iter_mut().flatten() {
            removed += sector.walls.dedupe_exact();
        }
        removed
    }

    /// Drop walls on `cells` that sit on a physical edge an untouched
    /// neighbour already claims. Returns how many segments went.
    ///
    /// The wall between `(x, z)` and `(x+1, z)` is one face whether authored
    /// as `East(x, z)` or `West(x+1, z)`, and the cooker rejects a grid that
    /// claims it twice (`DuplicatePhysicalWall`). Tiling pieces edge to edge
    /// produces exactly that, so a stamp runs this over the cells it wrote:
    /// the incoming wall loses, because the destination is the thing already
    /// on screen. Seams *inside* the stamp are left alone -- a piece that
    /// contradicts itself was authored broken and should say so at cook time
    /// rather than be quietly repaired on every placement.
    ///
    /// [`WorldGrid::dedupe_duplicate_walls`] does not cover this: it only
    /// removes segments byte-identical to another on the *same* edge.
    pub fn strip_seam_walls(&mut self, cells: &[(u16, u16)]) -> usize {
        let stamped: HashSet<(u16, u16)> = cells.iter().copied().collect();
        let mut claimed: HashSet<GridPhysicalEdge> = HashSet::new();
        for x in 0..self.width {
            for z in 0..self.depth {
                if stamped.contains(&(x, z)) {
                    continue;
                }
                let Some(sector) = self.sector(x, z) else {
                    continue;
                };
                for direction in GridDirection::CARDINAL {
                    if sector.walls.get(direction).is_empty() {
                        continue;
                    }
                    claimed.extend(direction.physical_edge(x, z));
                }
            }
        }

        let mut stripped = 0;
        for &(x, z) in &stamped {
            let Some(sector) = self.sector_mut(x, z) else {
                continue;
            };
            for direction in GridDirection::CARDINAL {
                if !direction
                    .physical_edge(x, z)
                    .is_some_and(|edge| claimed.contains(&edge))
                {
                    continue;
                }
                let walls = sector.walls.get_mut(direction);
                stripped += walls.len();
                walls.clear();
            }
        }
        stripped
    }

    /// Exact duplicate wall segments on this floor, without modifying it.
    pub fn duplicate_wall_count(&self) -> usize {
        self.sectors
            .iter()
            .flatten()
            .map(|sector| sector.walls.duplicate_count())
            .sum()
    }

    /// [`WorldGrid::dedupe_duplicate_walls`] across this floor and every floor
    /// stacked above it.
    pub fn dedupe_duplicate_walls_all_floors(&mut self) -> usize {
        let mut removed = self.dedupe_duplicate_walls();
        for floor in self.floors_above.iter_mut() {
            removed += floor.dedupe_duplicate_walls();
        }
        removed
    }

    /// [`WorldGrid::duplicate_wall_count`] across this floor and every floor
    /// stacked above it.
    pub fn duplicate_wall_count_all_floors(&self) -> usize {
        self.duplicate_wall_count()
            + self
                .floors_above
                .iter()
                .map(|floor| floor.duplicate_wall_count())
                .sum::<usize>()
    }

    /// Mutable floor `i`.
    pub fn floor_mut(&mut self, i: usize) -> Option<&mut WorldGrid> {
        if i == 0 {
            Some(self)
        } else {
            self.floors_above.get_mut(i - 1)
        }
    }

    /// Add an empty floor stacked just above the current top floor's
    /// ceiling and return its index. The new floor copies the base
    /// footprint dimensions but starts with no sectors, so it can be
    /// painted and extended freely.
    pub fn push_floor(&mut self) -> usize {
        let top = self.floors_above.last().unwrap_or(self);
        let elevation = top.elevation + default_wall_height_for_sector_size(top.sector_size);
        // Inherit the top floor's footprint and room-level look so an
        // adjacent layer starts aligned even when that floor was extended
        // independently from the base grid.
        let floor = Self::empty_stacked_floor_like(top, elevation);
        self.floors_above.push(floor);
        self.floors_above.len()
    }

    /// Insert an empty floor below floor zero while preserving every
    /// existing floor and its authored elevation. The former base becomes
    /// floor one; callers that own scene nodes must shift their floor indices
    /// and room transform to keep existing content at the same world height.
    pub fn push_floor_below(&mut self) -> usize {
        let elevation = self
            .elevation
            .saturating_sub(default_wall_height_for_sector_size(self.sector_size));
        let mut new_base = Self::empty_stacked_floor_like(self, elevation);
        std::mem::swap(self, &mut new_base);

        let previous_upper_floors = std::mem::take(&mut new_base.floors_above);
        self.floors_above.reserve(1 + previous_upper_floors.len());
        self.floors_above.push(new_base);
        self.floors_above.extend(previous_upper_floors);
        0
    }

    /// Remove an empty stacked floor while keeping at least one floor in the
    /// room. Removing floor zero promotes the next floor to the base and
    /// returns the elevation delta callers must add to the owning Room node
    /// to preserve the promoted floor's world-space height. A zero delta
    /// means a non-base floor was removed. Returns `None` for an invalid,
    /// populated, or only floor.
    pub fn remove_empty_floor(&mut self, floor_index: usize) -> Option<i32> {
        if self.floor_count() <= 1
            || self
                .floor(floor_index)
                .is_none_or(|floor| floor.populated_sector_count() != 0)
        {
            return None;
        }

        if floor_index == 0 {
            let old_base_elevation = self.elevation;
            let mut remaining = std::mem::take(&mut self.floors_above);
            let mut promoted = remaining.remove(0);
            let elevation_delta = promoted.elevation.saturating_sub(old_base_elevation);
            promoted.floors_above = remaining;
            *self = promoted;
            Some(elevation_delta)
        } else {
            self.floors_above.remove(floor_index - 1);
            Some(0)
        }
    }

    /// Create a rectangular room with floors and perimeter walls.
    pub fn stone_room(
        width: u16,
        depth: u16,
        sector_size: i32,
        floor_material: Option<ResourceId>,
        wall_material: Option<ResourceId>,
    ) -> Self {
        let mut grid = Self::empty(width, depth, sector_size);
        let wall_top = default_wall_height_for_sector_size(sector_size);
        for x in 0..width {
            for z in 0..depth {
                grid.set_floor(x, z, 0, floor_material);
                if z == depth.saturating_sub(1) {
                    grid.add_wall(x, z, GridDirection::North, 0, wall_top, wall_material);
                }
                if x == width.saturating_sub(1) {
                    grid.add_wall(x, z, GridDirection::East, 0, wall_top, wall_material);
                }
                if z == 0 {
                    grid.add_wall(x, z, GridDirection::South, 0, wall_top, wall_material);
                }
                if x == 0 {
                    grid.add_wall(x, z, GridDirection::West, 0, wall_top, wall_material);
                }
            }
        }
        grid
    }

    /// Flat sector index.
    pub fn sector_index(&self, x: u16, z: u16) -> Option<usize> {
        if x < self.width && z < self.depth {
            Some(x as usize * self.depth as usize + z as usize)
        } else {
            None
        }
    }

    /// Immutable sector.
    pub fn sector(&self, x: u16, z: u16) -> Option<&GridSector> {
        self.sector_index(x, z)
            .and_then(|index| self.sectors.get(index)?.as_ref())
    }

    /// Mutable sector. `None` when out-of-bounds OR the cell hasn't
    /// been authored yet (use `ensure_sector` to create-on-access).
    pub fn sector_mut(&mut self, x: u16, z: u16) -> Option<&mut GridSector> {
        self.sector_index(x, z)
            .and_then(move |index| self.sectors.get_mut(index)?.as_mut())
    }

    /// Mutable sector, creating it if needed.
    pub fn ensure_sector(&mut self, x: u16, z: u16) -> Option<&mut GridSector> {
        let index = self.sector_index(x, z)?;
        if self.sectors[index].is_none() {
            self.sectors[index] = Some(GridSector::empty());
        }
        self.sectors[index].as_mut()
    }

    /// Set or replace a floor.
    pub fn set_floor(&mut self, x: u16, z: u16, height: i32, material: Option<ResourceId>) {
        if let Some(sector) = self.ensure_sector(x, z) {
            sector.floor = Some(GridHorizontalFace::flat(height, material));
        }
    }

    /// Set or clear the floor link above one sector.
    pub fn set_floor_above(&mut self, x: u16, z: u16, link: Option<GridFloorLink>) {
        if let Some(sector) = self.ensure_sector(x, z) {
            sector.floor_above = link;
        }
    }

    /// Set or clear the floor link below one sector.
    pub fn set_floor_below(&mut self, x: u16, z: u16, link: Option<GridFloorLink>) {
        if let Some(sector) = self.ensure_sector(x, z) {
            sector.floor_below = link;
        }
    }

    /// Number of authored vertical floor links in this grid.
    pub fn floor_link_count(&self) -> usize {
        self.sectors
            .iter()
            .filter_map(Option::as_ref)
            .map(|sector| {
                usize::from(sector.floor_above.is_some())
                    + usize::from(sector.floor_below.is_some())
            })
            .sum()
    }

    /// Set or replace a floor, inheriting edge heights from touching
    /// floors. If exactly one flat edge is connected, the whole new
    /// floor adopts that height instead of only matching the shared edge.
    pub fn set_floor_aligned_to_neighbors(
        &mut self,
        x: u16,
        z: u16,
        height: i32,
        material: Option<ResourceId>,
    ) {
        let wcx = self.origin[0] + i32::from(x);
        let wcz = self.origin[1] + i32::from(z);
        let heights = self.floor_heights_aligned_to_neighbors_for_world_cell(wcx, wcz, height);
        if let Some(sector) = self.ensure_sector(x, z) {
            let mut floor = GridHorizontalFace::flat(height, material);
            floor.heights = heights;
            sector.floor = Some(floor);
        }
    }

    /// Candidate floor heights for editor placement by world-cell
    /// coordinate. The returned order is `[NW, NE, SE, SW]`.
    pub fn floor_heights_aligned_to_neighbors_for_world_cell(
        &self,
        wcx: i32,
        wcz: i32,
        height: i32,
    ) -> [i32; 4] {
        self.horizontal_heights_aligned_to_neighbor_faces_for_world_cell(
            wcx,
            wcz,
            HorizontalSurface::Floor,
            [height; 4],
        )
        .map(snap_height)
    }

    /// Set or replace a ceiling, inheriting edge heights from
    /// touching ceilings first and touching wall tops second. Wall
    /// tops win so a newly-painted ceiling sits on the surrounding
    /// authored wall geometry instead of cutting through it.
    pub fn set_ceiling_aligned_to_neighbors(
        &mut self,
        x: u16,
        z: u16,
        material: Option<ResourceId>,
    ) {
        let wcx = self.origin[0] + i32::from(x);
        let wcz = self.origin[1] + i32::from(z);
        let heights = self.ceiling_heights_aligned_to_neighbors_for_world_cell(wcx, wcz);
        let fallback_height = default_wall_height_for_sector_size(self.sector_size);
        if let Some(sector) = self.ensure_sector(x, z) {
            let mut ceiling = GridHorizontalFace::flat(fallback_height, material);
            ceiling.heights = heights;
            sector.ceiling = Some(ceiling);
        }
    }

    /// Candidate ceiling heights for editor placement. The returned
    /// order is `[NW, NE, SE, SW]`.
    pub fn ceiling_heights_aligned_to_neighbors(&self, x: u16, z: u16) -> [i32; 4] {
        let wcx = self.origin[0] + i32::from(x);
        let wcz = self.origin[1] + i32::from(z);
        self.ceiling_heights_aligned_to_neighbors_for_world_cell(wcx, wcz)
    }

    /// Candidate ceiling heights for editor placement by world-cell
    /// coordinate. Used by hover previews for cells that may not be
    /// allocated until the click auto-grows the grid.
    pub fn ceiling_heights_aligned_to_neighbors_for_world_cell(
        &self,
        wcx: i32,
        wcz: i32,
    ) -> [i32; 4] {
        let fallback_height = default_wall_height_for_sector_size(self.sector_size);
        let base_heights = self
            .world_cell_to_array(wcx, wcz)
            .and_then(|(sx, sz)| self.sector(sx, sz))
            .and_then(|sector| sector.ceiling.as_ref())
            .map(|ceiling| ceiling.heights)
            .unwrap_or([fallback_height; 4]);

        let mut heights = self.horizontal_heights_aligned_to_neighbor_faces_for_world_cell(
            wcx,
            wcz,
            HorizontalSurface::Ceiling,
            base_heights,
        );

        for direction in GridDirection::CARDINAL {
            if let Some(edge) =
                self.touching_wall_top_edge_heights_for_world_cell(wcx, wcz, direction)
            {
                set_horizontal_edge_heights(&mut heights, direction, edge);
            }
        }

        heights.map(snap_height)
    }

    fn horizontal_heights_aligned_to_neighbor_faces_for_world_cell(
        &self,
        wcx: i32,
        wcz: i32,
        surface: HorizontalSurface,
        fallback: [i32; 4],
    ) -> [i32; 4] {
        let mut heights = fallback;
        let mut only_edge: Option<[i32; 2]> = None;
        let mut edge_count = 0usize;

        for direction in GridDirection::CARDINAL {
            if let Some(edge) =
                self.neighbor_horizontal_edge_heights_for_world_cell(wcx, wcz, direction, surface)
            {
                set_horizontal_edge_heights(&mut heights, direction, edge);
                only_edge = Some(edge);
                edge_count += 1;
            }
        }

        match (edge_count, only_edge) {
            (1, Some([a, b])) if a == b => [a; 4],
            _ => heights,
        }
    }

    /// Add a wall to an edge.
    pub fn add_wall(
        &mut self,
        x: u16,
        z: u16,
        direction: GridDirection,
        bottom: i32,
        top: i32,
        material: Option<ResourceId>,
    ) {
        if let Some(sector) = self.ensure_sector(x, z) {
            sector
                .walls
                .get_mut(direction)
                .push(GridVerticalFace::flat(bottom, top, material));
        }
    }

    /// Add a wall whose bottom edge follows the floor edge under it
    /// and whose top edge follows the ceiling edge when present.
    /// Missing ceilings fall back to a two-sector wall span above
    /// each bottom endpoint.
    pub fn add_wall_aligned_to_surfaces(
        &mut self,
        x: u16,
        z: u16,
        direction: GridDirection,
        material: Option<ResourceId>,
    ) {
        let heights = self.wall_heights_aligned_to_surfaces(x, z, direction);
        if let Some(sector) = self.ensure_sector(x, z) {
            sector
                .walls
                .get_mut(direction)
                .push(GridVerticalFace::with_heights(heights, material));
        }
    }

    /// Add a wall on the selected edge. When that edge already has
    /// touching wall geometry, the new wall starts at the highest
    /// existing top edge and extends by one default wall height.
    /// Otherwise it uses the regular floor-to-ceiling placement.
    pub fn add_wall_above_stack_or_aligned(
        &mut self,
        x: u16,
        z: u16,
        direction: GridDirection,
        material: Option<ResourceId>,
    ) {
        let heights = self.wall_heights_above_stack_or_surfaces(x, z, direction);
        if let Some(sector) = self.ensure_sector(x, z) {
            sector
                .walls
                .get_mut(direction)
                .push(GridVerticalFace::with_heights(heights, material));
        }
    }

    /// Candidate wall heights for editor placement on a cardinal
    /// edge or diagonal. The returned order is `[BL, BR, TR, TL]`.
    pub fn wall_heights_aligned_to_surfaces(
        &self,
        x: u16,
        z: u16,
        direction: GridDirection,
    ) -> [i32; 4] {
        let bottom = self
            .floor_edge_heights_for_wall(x, z, direction)
            .unwrap_or([0, 0]);
        let top = self
            .ceiling_edge_heights_for_wall(x, z, direction)
            .unwrap_or_else(|| {
                let height = default_wall_height_for_sector_size(self.sector_size);
                [
                    bottom[0].saturating_add(height),
                    bottom[1].saturating_add(height),
                ]
            });
        [bottom[0], bottom[1], top[1], top[0]]
    }

    /// Candidate wall heights for placing the next wall in a stack
    /// at an in-grid cell. Falls back to surface-aligned placement
    /// when there is no existing wall on the touched edge.
    pub fn wall_heights_above_stack_or_surfaces(
        &self,
        x: u16,
        z: u16,
        direction: GridDirection,
    ) -> [i32; 4] {
        self.wall_heights_above_stack_or_surfaces_for_world_cell(
            self.origin[0].saturating_add(x as i32),
            self.origin[1].saturating_add(z as i32),
            direction,
        )
    }

    /// Same as [`Self::wall_heights_aligned_to_surfaces`], but
    /// addressed by world-cell coordinates so hover previews can
    /// match clicks that will auto-grow the grid on commit.
    pub fn wall_heights_aligned_to_surfaces_for_world_cell(
        &self,
        wcx: i32,
        wcz: i32,
        direction: GridDirection,
    ) -> [i32; 4] {
        let bottom = self
            .horizontal_edge_heights_for_world_wall(wcx, wcz, direction, HorizontalSurface::Floor)
            .unwrap_or([0, 0]);
        let top = self
            .horizontal_edge_heights_for_world_wall(wcx, wcz, direction, HorizontalSurface::Ceiling)
            .unwrap_or_else(|| {
                let height = default_wall_height_for_sector_size(self.sector_size);
                [
                    bottom[0].saturating_add(height),
                    bottom[1].saturating_add(height),
                ]
            });
        [bottom[0], bottom[1], top[1], top[0]]
    }

    /// Same as [`Self::wall_heights_above_stack_or_surfaces`], but
    /// addressed by world-cell coordinates so off-grid wall previews
    /// match auto-grown placement.
    pub fn wall_heights_above_stack_or_surfaces_for_world_cell(
        &self,
        wcx: i32,
        wcz: i32,
        direction: GridDirection,
    ) -> [i32; 4] {
        if let Some(bottom) =
            self.touching_wall_top_edge_heights_for_world_cell(wcx, wcz, direction)
        {
            let height = default_wall_height_for_sector_size(self.sector_size);
            let top = [
                bottom[0].saturating_add(height),
                bottom[1].saturating_add(height),
            ];
            return [bottom[0], bottom[1], top[1], top[0]];
        }
        self.wall_heights_aligned_to_surfaces_for_world_cell(wcx, wcz, direction)
    }

    fn floor_edge_heights_for_wall(
        &self,
        x: u16,
        z: u16,
        direction: GridDirection,
    ) -> Option<[i32; 2]> {
        self.horizontal_edge_heights_for_wall(x, z, direction, HorizontalSurface::Floor)
    }

    fn ceiling_edge_heights_for_wall(
        &self,
        x: u16,
        z: u16,
        direction: GridDirection,
    ) -> Option<[i32; 2]> {
        self.horizontal_edge_heights_for_wall(x, z, direction, HorizontalSurface::Ceiling)
    }

    fn neighbor_horizontal_edge_heights_for_world_cell(
        &self,
        wcx: i32,
        wcz: i32,
        direction: GridDirection,
        surface: HorizontalSurface,
    ) -> Option<[i32; 2]> {
        let (nwcx, nwcz, opposite) =
            Self::neighbor_world_cell_across_cardinal_edge(wcx, wcz, direction)?;
        let (sx, sz) = self.world_cell_to_array(nwcx, nwcz)?;
        let mut heights = self
            .sector(sx, sz)
            .and_then(|sector| surface.edge_heights(sector, opposite))?;
        heights.swap(0, 1);
        Some(heights)
    }

    fn touching_wall_top_edge_heights_for_world_cell(
        &self,
        wcx: i32,
        wcz: i32,
        direction: GridDirection,
    ) -> Option<[i32; 2]> {
        if let Some((sx, sz)) = self.world_cell_to_array(wcx, wcz) {
            if let Some(heights) = self
                .sector(sx, sz)
                .and_then(|sector| wall_top_edge_heights(sector.walls.get(direction)))
            {
                return Some(heights);
            }
        }

        let (nwcx, nwcz, opposite) =
            Self::neighbor_world_cell_across_cardinal_edge(wcx, wcz, direction)?;
        let (sx, sz) = self.world_cell_to_array(nwcx, nwcz)?;
        let mut heights = self
            .sector(sx, sz)
            .and_then(|sector| wall_top_edge_heights(sector.walls.get(opposite)))?;
        heights.swap(0, 1);
        Some(heights)
    }

    fn horizontal_edge_heights_for_wall(
        &self,
        x: u16,
        z: u16,
        direction: GridDirection,
        surface: HorizontalSurface,
    ) -> Option<[i32; 2]> {
        if let Some(heights) = self
            .sector(x, z)
            .and_then(|sector| surface.edge_heights(sector, direction))
        {
            return Some(heights);
        }

        let (nx, nz, opposite) = self.neighbor_across_cardinal_edge(x, z, direction)?;
        let mut heights = self
            .sector(nx, nz)
            .and_then(|sector| surface.edge_heights(sector, opposite))?;
        heights.swap(0, 1);
        Some(heights)
    }

    fn neighbor_across_cardinal_edge(
        &self,
        x: u16,
        z: u16,
        direction: GridDirection,
    ) -> Option<(u16, u16, GridDirection)> {
        let opposite = direction.opposite_cardinal()?;
        let (nx, nz) = match direction {
            GridDirection::North => (x, z.checked_add(1)?),
            GridDirection::East => (x.checked_add(1)?, z),
            GridDirection::South => (x, z.checked_sub(1)?),
            GridDirection::West => (x.checked_sub(1)?, z),
            GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => return None,
        };
        (nx < self.width && nz < self.depth).then_some((nx, nz, opposite))
    }

    fn horizontal_edge_heights_for_world_wall(
        &self,
        wcx: i32,
        wcz: i32,
        direction: GridDirection,
        surface: HorizontalSurface,
    ) -> Option<[i32; 2]> {
        if let Some((sx, sz)) = self.world_cell_to_array(wcx, wcz) {
            if let Some(heights) = self
                .sector(sx, sz)
                .and_then(|sector| surface.edge_heights(sector, direction))
            {
                return Some(heights);
            }
        }

        let (nwcx, nwcz, opposite) =
            Self::neighbor_world_cell_across_cardinal_edge(wcx, wcz, direction)?;
        let (sx, sz) = self.world_cell_to_array(nwcx, nwcz)?;
        let mut heights = self
            .sector(sx, sz)
            .and_then(|sector| surface.edge_heights(sector, opposite))?;
        heights.swap(0, 1);
        Some(heights)
    }

    /// Cook-time wall generated for a shared floor edge whose two
    /// sides do not meet. This closes vertical cracks in authored
    /// terrain without requiring artists to hand-place every step
    /// riser. Existing authored walls always win.
    pub fn floor_transition_wall_for_edge(
        &self,
        x: u16,
        z: u16,
        direction: GridDirection,
    ) -> Option<GridVerticalFace> {
        if !direction.is_cardinal() || self.physical_wall_authored(x, z, direction) {
            return None;
        }
        let sector = self.sector(x, z)?;
        let floor = sector.floor.as_ref()?;
        let current = HorizontalSurface::Floor.edge_heights(sector, direction)?;
        let (nx, nz, opposite) = self.neighbor_across_cardinal_edge(x, z, direction)?;
        let neighbour_sector = self.sector(nx, nz)?;
        let neighbour_floor = neighbour_sector.floor.as_ref()?;
        let mut neighbour = HorizontalSurface::Floor.edge_heights(neighbour_sector, opposite)?;
        neighbour.swap(0, 1);
        if current == neighbour {
            return None;
        }

        let bottom = [current[0].min(neighbour[0]), current[1].min(neighbour[1])];
        let top = [current[0].max(neighbour[0]), current[1].max(neighbour[1])];
        if bottom == top {
            return None;
        }
        Some(GridVerticalFace::with_heights(
            [bottom[0], bottom[1], top[1], top[0]],
            floor_transition_wall_material(floor, neighbour_floor, current, neighbour),
        ))
    }

    fn physical_wall_authored(&self, x: u16, z: u16, direction: GridDirection) -> bool {
        if self
            .sector(x, z)
            .is_some_and(|sector| !sector.walls.get(direction).is_empty())
        {
            return true;
        }
        let Some((nx, nz, opposite)) = self.neighbor_across_cardinal_edge(x, z, direction) else {
            return false;
        };
        self.sector(nx, nz)
            .is_some_and(|sector| !sector.walls.get(opposite).is_empty())
    }

    fn neighbor_world_cell_across_cardinal_edge(
        wcx: i32,
        wcz: i32,
        direction: GridDirection,
    ) -> Option<(i32, i32, GridDirection)> {
        let opposite = direction.opposite_cardinal()?;
        let cell = match direction {
            GridDirection::North => (wcx, wcz.saturating_add(1)),
            GridDirection::East => (wcx.saturating_add(1), wcz),
            GridDirection::South => (wcx, wcz.saturating_sub(1)),
            GridDirection::West => (wcx.saturating_sub(1), wcz),
            GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => return None,
        };
        Some((cell.0, cell.1, opposite))
    }

    /// Number of populated sectors.
    pub fn populated_sector_count(&self) -> usize {
        self.sectors
            .iter()
            .flatten()
            .filter(|sector| sector.has_geometry())
            .count()
    }

    /// Rectangle enclosing every sector that emits authored
    /// geometry. Empty allocated cells are capacity, not room
    /// footprint, so they do not influence bounds or streaming
    /// subdivision.
    pub fn authored_footprint(&self) -> Option<WorldGridFootprint> {
        let mut min_x = self.width;
        let mut min_z = self.depth;
        let mut max_x = 0u16;
        let mut max_z = 0u16;
        let mut found = false;
        for x in 0..self.width {
            for z in 0..self.depth {
                let Some(sector) = self.sector(x, z) else {
                    continue;
                };
                if !sector.has_geometry() {
                    continue;
                }
                found = true;
                min_x = min_x.min(x);
                min_z = min_z.min(z);
                max_x = max_x.max(x);
                max_z = max_z.max(z);
            }
        }
        found.then(|| WorldGridFootprint {
            x: min_x,
            z: min_z,
            width: max_x - min_x + 1,
            depth: max_z - min_z + 1,
        })
    }

    /// Budget for the authored footprint only. This is the number
    /// authors care about when sparse grid allocation has grown past
    /// the currently placed tiles.
    pub fn authored_budget(&self) -> WorldGridBudget {
        self.authored_footprint()
            .and_then(|f| self.budget_for_rect(f.x, f.z, f.width, f.depth))
            .unwrap_or_default()
    }

    /// Snapshot of the allocated grid rectangle + cooked-byte
    /// estimate. Use [`Self::authored_budget`] when empty capacity
    /// should not count as room footprint.
    pub fn budget(&self) -> WorldGridBudget {
        self.budget_for_rect(0, 0, self.width, self.depth)
            .unwrap_or_default()
    }

    /// Snapshot of one rectangular grid area. The rectangle is in
    /// array-sector coordinates, not world-origin-adjusted cells.
    /// Returns `None` for empty or out-of-bounds rectangles.
    pub fn budget_for_rect(
        &self,
        x: u16,
        z: u16,
        width: u16,
        depth: u16,
    ) -> Option<WorldGridBudget> {
        if width == 0 || depth == 0 {
            return None;
        }
        let x1 = x.checked_add(width)?;
        let z1 = z.checked_add(depth)?;
        if x1 > self.width || z1 > self.depth {
            return None;
        }
        let mut b = WorldGridBudget {
            width,
            depth,
            total_cells: (width as usize) * (depth as usize),
            ..Default::default()
        };
        for sx in x..x1 {
            for sz in z..z1 {
                let Some(sector) = self.sector(sx, sz) else {
                    continue;
                };
                if !sector.has_geometry() {
                    continue;
                }
                b.populated_cells += 1;
                if sector.floor.is_some() {
                    b.floors += 1;
                    if let Some(face) = sector.floor.as_ref() {
                        b.triangles += face_triangle_count(face);
                        if horizontal_face_needs_runtime_override(face) {
                            b.horizontal_overrides += 1;
                        }
                    }
                }
                if sector.ceiling.is_some() {
                    b.ceilings += 1;
                    if let Some(face) = sector.ceiling.as_ref() {
                        b.triangles += face_triangle_count(face);
                        if horizontal_face_needs_runtime_override(face) {
                            b.horizontal_overrides += 1;
                        }
                    }
                }
                for direction in GridDirection::ALL {
                    for wall in sector.walls.get(direction) {
                        let count = wall.autotile_segment_count(self.sector_size);
                        b.walls += count;
                        b.triangles += if wall.is_triangle() { 1 } else { count * 2 };
                    }
                }
                for direction in [GridDirection::East, GridDirection::North] {
                    if let Some(wall) = self.floor_transition_wall_for_edge(sx, sz, direction) {
                        let count = wall.autotile_segment_count(self.sector_size);
                        b.walls += count;
                        b.triangles += if wall.is_triangle() { 1 } else { count * 2 };
                    }
                }
            }
        }
        // Active wire layout (matches `psxed_format::world` records).
        // `.psxw` stores a sector record for every cell -- empty or
        // not -- so the byte count uses `total_cells`. Using
        // `populated_cells` here was the original bug: it under-
        // reported the wire size by one sector record per empty cell.
        // Target compact-format sizes for the planning estimate.
        // See `docs/world-format-roadmap.md`. Plain numeric
        // constants rather than struct sizes so this block doesn't
        // pretend a v2 format exists in code.
        b.psxw_bytes = ASSET_HEADER_BYTES
            + WORLD_HEADER_BYTES
            + b.total_cells * PSXW_SECTOR_BYTES
            + b.walls * PSXW_WALL_BYTES
            + b.horizontal_overrides * PSXW_HORIZONTAL_OVERRIDE_BYTES;
        if b.populated_cells > 0 {
            b.static_light_table_bytes = (b.total_cells * 2 + b.walls) * PSXW_SURFACE_LIGHT_BYTES;
        }
        b.psxw_static_lit_bytes = b.psxw_bytes + b.static_light_table_bytes;
        b.future_compact_estimated_bytes = ASSET_HEADER_BYTES
            + WORLD_HEADER_BYTES
            + b.total_cells * FUTURE_COMPACT_SECTOR_BYTES
            + b.walls * FUTURE_COMPACT_WALL_BYTES;
        Some(b)
    }

    /// World-space X coordinate of the left edge of column `sx`
    /// (array index, not world-cell index). Accounts for `origin`
    /// so the renderer and picking always agree on cell positions.
    pub fn cell_world_x(&self, sx: u16) -> i32 {
        (self.origin[0] + sx as i32) * self.sector_size
    }

    /// World-space Z coordinate of the low-Z edge of row `sz`.
    pub fn cell_world_z(&self, sz: u16) -> i32 {
        (self.origin[1] + sz as i32) * self.sector_size
    }

    /// World-space X/Z bounds of cell `(sx, sz)` in editor
    /// convention. `z0` is the low-Z / south edge and `z1` is
    /// the high-Z / north edge.
    pub fn cell_bounds_world(&self, sx: u16, sz: u16) -> GridCellBounds {
        let x0 = self.cell_world_x(sx);
        let z0 = self.cell_world_z(sz);
        GridCellBounds {
            x0,
            x1: x0 + self.sector_size,
            z0,
            z1: z0 + self.sector_size,
        }
    }

    /// World-space `(x, z)` centre of cell `(sx, sz)` in floating
    /// point -- handy for picking, edge inference, and entity
    /// snapping. Mirrors the renderer's cell positioning so all
    /// three pipelines agree on where each cell physically sits.
    pub fn cell_center_world(&self, sx: u16, sz: u16) -> [f32; 2] {
        let s = self.sector_size as f32;
        [
            (self.origin[0] as f32 + sx as f32 + 0.5) * s,
            (self.origin[1] as f32 + sz as f32 + 0.5) * s,
        ]
    }

    /// Geometric centre of the room in world-cell units. After a
    /// negative-side grow this is `(origin + half)` rather than
    /// just `half`, so callers stay correct without each
    /// re-deriving the offset.
    ///
    /// This is the **canonical** editor centre -- every coordinate
    /// helper that bridges editor-viewport units (sector-units,
    /// room-centre-relative) and world-cell / world-space units
    /// goes through this single source of truth.
    pub fn grid_center_cells(&self) -> [f32; 2] {
        [
            self.origin[0] as f32 + self.width as f32 * 0.5,
            self.origin[1] as f32 + self.depth as f32 * 0.5,
        ]
    }

    /// Convert editor-viewport coordinates (sector-units,
    /// room-centre-relative) to world-cell units. The viewport's
    /// `(0, 0)` is the room centre; world-cell `(0, 0)` is the
    /// runtime cell at the room's first array slot pre-grow.
    pub fn editor_to_world_cells(&self, editor: [f32; 2]) -> [f32; 2] {
        let center = self.grid_center_cells();
        [editor[0] + center[0], editor[1] + center[1]]
    }

    /// Inverse of [`Self::editor_to_world_cells`]. World coords
    /// (post-`/sector_size`) returned from a 3D ground-plane hit
    /// land back in the editor's sector-unit space ready to feed
    /// `world_cell_to_array` or stash on a node transform.
    pub fn world_cells_to_editor(&self, world_cells: [f32; 2]) -> [f32; 2] {
        let center = self.grid_center_cells();
        [world_cells[0] - center[0], world_cells[1] - center[1]]
    }

    /// Editor-viewport position → array `(sx, sz)`. Combines
    /// `editor_to_world_cells` + `floor` + `world_cell_to_array`
    /// in one step so callers don't repeat the conversion at
    /// each call site.
    pub fn editor_cells_to_array(&self, editor: [f32; 2]) -> Option<(u16, u16)> {
        let world = self.editor_to_world_cells(editor);
        let wcx = world[0].floor() as i32;
        let wcz = world[1].floor() as i32;
        self.world_cell_to_array(wcx, wcz)
    }

    /// Editor-viewport position → world-space `(x, 0, z)` in
    /// engine units (room-local, origin-aware). Used by the
    /// editor's 3D preview path which renders cells at
    /// `cell_world_x/z` so authored content keeps its visual
    /// position after a negative-side grow.
    pub fn editor_to_room_local(&self, editor: [f32; 2]) -> [f32; 3] {
        let world_cells = self.editor_to_world_cells(editor);
        let s = self.sector_size as f32;
        [world_cells[0] * s, 0.0, world_cells[1] * s]
    }

    /// Inverse of [`Self::editor_to_room_local`] -- world-space
    /// `(x, _, z)` → editor-viewport `(x, z)` (sector-units,
    /// room-centre-relative). The `y` component is dropped:
    /// cell positioning is purely XZ.
    pub fn room_local_to_editor(&self, room_local: [f32; 3]) -> [f32; 2] {
        let s = self.sector_size as f32;
        self.world_cells_to_editor([room_local[0] / s, room_local[2] / s])
    }

    /// Convert a world position to the world-cell coordinate
    /// (which can be negative). The world-cell is the same coord
    /// system the renderer uses; subtract `origin` to get the
    /// array index.
    pub fn world_x_to_cell(&self, world_x: f32) -> i32 {
        (world_x / self.sector_size as f32).floor() as i32
    }

    pub fn world_z_to_cell(&self, world_z: f32) -> i32 {
        (world_z / self.sector_size as f32).floor() as i32
    }

    /// Floor height under a room-local world-space X/Z point.
    /// Returns `None` when the point is outside the allocated grid
    /// or the addressed sector has no floor face.
    pub fn floor_height_at_room_local(&self, world_x: i32, world_z: i32) -> Option<i32> {
        let s = self.sector_size;
        if s <= 0 {
            return None;
        }
        let wcx = world_x.div_euclid(s);
        let wcz = world_z.div_euclid(s);
        let (sx, sz) = self.world_cell_to_array(wcx, wcz)?;
        let sector = self.sector(sx, sz)?;
        let floor = sector.floor.as_ref()?;
        let local_x = world_x.rem_euclid(s);
        let local_z = world_z.rem_euclid(s);
        Some(floor.height_at_local(local_x, local_z, s))
    }

    /// Translate a world-cell coordinate to its array index, or
    /// `None` if the cell isn't currently allocated.
    pub fn world_cell_to_array(&self, wcx: i32, wcz: i32) -> Option<(u16, u16)> {
        let ax = wcx.checked_sub(self.origin[0])?;
        let az = wcz.checked_sub(self.origin[1])?;
        if ax < 0 || az < 0 {
            return None;
        }
        let ax = ax as u32;
        let az = az as u32;
        if ax >= self.width as u32 || az >= self.depth as u32 {
            return None;
        }
        Some((ax as u16, az as u16))
    }

    /// Ensure the world-cell `(wcx, wcz)` is addressable. Grows
    /// the grid in `+X` / `+Z` and / or shifts existing sectors
    /// (with `origin` decrementing in lockstep) when growth is
    /// needed in `-X` / `-Z`. Existing cells keep the same world
    /// position throughout. Returns the resolved array index.
    pub fn extend_to_include(&mut self, wcx: i32, wcz: i32) -> (u16, u16) {
        let rel_x = wcx - self.origin[0];
        let rel_z = wcz - self.origin[1];
        let shift_x = (-rel_x).max(0) as u16;
        let shift_z = (-rel_z).max(0) as u16;
        // The new array width must hold both the shifted existing
        // data ([shift, shift + old_width)) AND the new cell (at
        // shift + max(rel, 0)). Same logic for depth.
        let new_cell_x = (rel_x.max(0) as u16) + shift_x;
        let new_cell_z = (rel_z.max(0) as u16) + shift_z;
        let new_w = (shift_x + self.width).max(new_cell_x + 1);
        let new_d = (shift_z + self.depth).max(new_cell_z + 1);
        if shift_x == 0 && shift_z == 0 && new_w == self.width && new_d == self.depth {
            return (rel_x as u16, rel_z as u16);
        }
        // Rebuild the sector array, shifting existing data by
        // (shift_x, shift_z) so its world position is preserved.
        let new_len = new_w as usize * new_d as usize;
        let mut new_sectors: Vec<Option<GridSector>> = vec![None; new_len];
        for x in 0..self.width {
            for z in 0..self.depth {
                let old_idx = x as usize * self.depth as usize + z as usize;
                let new_x = x as usize + shift_x as usize;
                let new_z = z as usize + shift_z as usize;
                if new_x < new_w as usize && new_z < new_d as usize {
                    let new_idx = new_x * new_d as usize + new_z;
                    new_sectors[new_idx] = self.sectors[old_idx].take();
                }
            }
        }
        self.width = new_w;
        self.depth = new_d;
        self.origin[0] -= shift_x as i32;
        self.origin[1] -= shift_z as i32;
        self.sectors = new_sectors;
        (
            (rel_x + shift_x as i32) as u16,
            (rel_z + shift_z as i32) as u16,
        )
    }

    /// Reshape the grid to `new_width × new_depth`.
    ///
    /// Sectors that lie inside both the old and new bounds keep
    /// their authored content; cells that were outside the old
    /// bounds (a grow operation) come up empty; cells outside the
    /// new bounds (a shrink) are dropped.
    ///
    /// No-op when the dims already match.
    pub fn resize(&mut self, new_width: u16, new_depth: u16) {
        if new_width == self.width && new_depth == self.depth {
            return;
        }
        let new_len = new_width as usize * new_depth as usize;
        let mut new_sectors: Vec<Option<GridSector>> = vec![None; new_len];
        let copy_w = self.width.min(new_width);
        let copy_d = self.depth.min(new_depth);
        for x in 0..copy_w {
            for z in 0..copy_d {
                let old_idx = x as usize * self.depth as usize + z as usize;
                let new_idx = x as usize * new_depth as usize + z as usize;
                new_sectors[new_idx] = self.sectors[old_idx].take();
            }
        }
        self.width = new_width;
        self.depth = new_depth;
        self.sectors = new_sectors;
    }

    /// Change this grid's sector size and scale engine-unit
    /// vertical geometry by the same ratio. X/Z authored positions
    /// are stored in sector units, so they inherit the new physical
    /// size through `sector_size`.
    pub fn rescale_sector_size(&mut self, new_sector_size: i32) {
        let new_sector_size = snap_world_sector_size(new_sector_size);
        let old_sector_size = self.sector_size.max(1);
        if old_sector_size == new_sector_size {
            self.sector_size = new_sector_size;
            self.snap_heights_to_quantum();
            for floor in &mut self.floors_above {
                floor.rescale_sector_size(new_sector_size);
            }
            return;
        }
        self.elevation = snap_height(scale_i32_ratio(
            self.elevation,
            old_sector_size,
            new_sector_size,
        ));
        for sector in self.sectors.iter_mut().flatten() {
            if let Some(face) = &mut sector.floor {
                for h in &mut face.heights {
                    *h = snap_height(scale_i32_ratio(*h, old_sector_size, new_sector_size));
                }
                for idx in 0..2 {
                    if let Some(heights) = face.triangle_override_mut(idx).heights.as_mut() {
                        for h in heights {
                            *h = snap_height(scale_i32_ratio(*h, old_sector_size, new_sector_size));
                        }
                    }
                }
            }
            if let Some(face) = &mut sector.ceiling {
                for h in &mut face.heights {
                    *h = snap_height(scale_i32_ratio(*h, old_sector_size, new_sector_size));
                }
                for idx in 0..2 {
                    if let Some(heights) = face.triangle_override_mut(idx).heights.as_mut() {
                        for h in heights {
                            *h = snap_height(scale_i32_ratio(*h, old_sector_size, new_sector_size));
                        }
                    }
                }
            }
            for direction in GridDirection::ALL {
                for wall in sector.walls.get_mut(direction) {
                    for h in &mut wall.heights {
                        *h = snap_height(scale_i32_ratio(*h, old_sector_size, new_sector_size));
                    }
                }
            }
        }
        self.fog_near = scale_i32_ratio(self.fog_near, old_sector_size, new_sector_size).max(0);
        self.fog_far = scale_i32_ratio(self.fog_far, old_sector_size, new_sector_size)
            .max(self.fog_near + HEIGHT_QUANTUM);
        self.sector_size = new_sector_size;
        for floor in &mut self.floors_above {
            floor.rescale_sector_size(new_sector_size);
        }
    }

    /// Apply a normalized sector size to every stacked floor without changing
    /// authored engine-unit geometry. Used while loading projects whose World
    /// node already owns the canonical size.
    pub fn normalize_stacked_sector_size(&mut self, sector_size: i32) {
        let sector_size = snap_world_sector_size(sector_size);
        self.sector_size = sector_size;
        self.snap_heights_to_quantum();
        for floor in &mut self.floors_above {
            floor.normalize_stacked_sector_size(sector_size);
        }
    }

    /// Snap all authored vertical geometry to the cooker-supported
    /// height quantum. This is load/save normalization for stale or
    /// hand-edited project data; live editor controls call
    /// [`snap_height`] at the point of edit.
    pub fn snap_heights_to_quantum(&mut self) {
        for sector in self.sectors.iter_mut().flatten() {
            if let Some(face) = &mut sector.floor {
                for h in &mut face.heights {
                    *h = snap_height(*h);
                }
                for idx in 0..2 {
                    if let Some(heights) = face.triangle_override_mut(idx).heights.as_mut() {
                        for h in heights {
                            *h = snap_height(*h);
                        }
                    }
                }
            }
            if let Some(face) = &mut sector.ceiling {
                for h in &mut face.heights {
                    *h = snap_height(*h);
                }
                for idx in 0..2 {
                    if let Some(heights) = face.triangle_override_mut(idx).heights.as_mut() {
                        for h in heights {
                            *h = snap_height(*h);
                        }
                    }
                }
            }
            for direction in GridDirection::ALL {
                for wall in sector.walls.get_mut(direction) {
                    for h in &mut wall.heights {
                        *h = snap_height(*h);
                    }
                }
            }
        }
    }
}
