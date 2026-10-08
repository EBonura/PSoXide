use super::*;

mod sky;
pub use sky::*;
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

/// Optional combat camera composition, in authored world units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorldCameraProfile {
    /// Preferred trailing distance.
    pub distance: i32,
    /// Camera height above the player origin.
    pub height: i32,
    /// Focus height above the player origin.
    pub target_height: i32,
    /// Signed camera-right offset while locked, in authored world units.
    pub shoulder_offset: i32,
    /// Vertical field of view, clamped to 38-48 degrees.
    pub fov_y_degrees: u8,
}

impl Default for WorldCameraProfile {
    fn default() -> Self {
        Self {
            distance: default_world_camera_distance(),
            height: default_world_camera_height(),
            target_height: default_world_camera_target_height(),
            fov_y_degrees: 43,
            shoulder_offset: 0,
        }
    }
}

impl WorldCameraProfile {
    pub fn normalized(self) -> Self {
        Self {
            distance: self
                .distance
                .clamp(MIN_WORLD_CAMERA_DISTANCE, MAX_WORLD_CAMERA_DISTANCE),
            height: self.height.clamp(0, MAX_WORLD_CAMERA_HEIGHT),
            target_height: self.target_height.clamp(0, MAX_WORLD_CAMERA_HEIGHT),
            fov_y_degrees: self.fov_y_degrees.clamp(38, 48),
            shoulder_offset: self
                .shoulder_offset
                .clamp(-MAX_WORLD_CAMERA_DISTANCE, MAX_WORLD_CAMERA_DISTANCE),
        }
    }
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
    /// Ramp held orbit input from a gentle rate to a fast rate.
    #[serde(default)]
    pub accelerated_orbit: bool,
    /// Keep the selected elevation when recentering behind the player.
    #[serde(default)]
    pub recenter_preserves_pitch: bool,
    /// Vertical field of view in degrees; zero preserves the legacy lens.
    #[serde(default)]
    pub fov_y_degrees: u8,
    /// Smooth changes to camera distance, offsets and field of view.
    #[serde(default)]
    pub blend_profiles: bool,
    /// Frame the live lock target using a separate elevated camera anchor.
    #[serde(default)]
    pub lock_target_framing: bool,
    /// Optional distance, height and lens while locked on.
    #[serde(default)]
    pub lock_profile: Option<crate::WorldCameraProfile>,
    /// Camera origin follow lag shift. Lower values move faster.
    #[serde(default = "default_world_camera_position_lag_shift")]
    pub position_lag_shift: u8,
    /// Vertical position smoothing override; absent follows the shared lag setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_vertical_lag_shift: Option<u8>,
    /// Camera focus follow lag shift. Lower values move faster.
    #[serde(default = "default_world_camera_focus_lag_shift")]
    pub focus_lag_shift: u8,
    /// Vertical focus smoothing override; absent follows the shared lag setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_vertical_lag_shift: Option<u8>,
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
            accelerated_orbit: self.accelerated_orbit,
            recenter_preserves_pitch: self.recenter_preserves_pitch,
            fov_y_degrees: if self.fov_y_degrees == 0 {
                0
            } else {
                self.fov_y_degrees.clamp(38, 48)
            },
            blend_profiles: self.blend_profiles,
            lock_target_framing: self.lock_target_framing,
            lock_profile: self.lock_profile.map(WorldCameraProfile::normalized),
            position_lag_shift: self.position_lag_shift.min(MAX_WORLD_CAMERA_LAG_SHIFT),
            position_vertical_lag_shift: self
                .position_vertical_lag_shift
                .map(|shift| shift.min(MAX_WORLD_CAMERA_LAG_SHIFT)),
            focus_lag_shift: self.focus_lag_shift.min(MAX_WORLD_CAMERA_LAG_SHIFT),
            focus_vertical_lag_shift: self
                .focus_vertical_lag_shift
                .map(|shift| shift.min(MAX_WORLD_CAMERA_LAG_SHIFT)),
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
            accelerated_orbit: false,
            recenter_preserves_pitch: false,
            fov_y_degrees: 0,
            blend_profiles: false,
            lock_target_framing: false,
            lock_profile: None,
            position_lag_shift: default_world_camera_position_lag_shift(),
            position_vertical_lag_shift: None,
            focus_lag_shift: default_world_camera_focus_lag_shift(),
            focus_vertical_lag_shift: None,
            distance_lag_shift: default_world_camera_distance_lag_shift(),
        }
    }
}

/// Minimum camera-space far plane used by runtime world drawing.
pub const MIN_WORLD_DRAW_DISTANCE: i32 = 4_096;
/// Maximum camera-space far plane exposed for playtest experimentation.
pub const MAX_WORLD_DRAW_DISTANCE: i32 = 262_144;
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
    /// Maximum authored BSP render-patch extent before the resident face budget fallback.
    /// Smaller patches resolve vertex lighting and near-plane clipping more finely.
    #[serde(default = "default_bsp_patch_extent")]
    pub bsp_patch_extent: i32,
}

fn default_bsp_patch_extent() -> i32 {
    2048
}

impl WorldCullingSettings {
    /// Clamp authored values to runtime-safe ranges.
    pub fn normalized(self) -> Self {
        Self {
            bsp_patch_extent: self.bsp_patch_extent.clamp(1024, 4096),
            draw_distance: self
                .draw_distance
                .clamp(MIN_WORLD_DRAW_DISTANCE, MAX_WORLD_DRAW_DISTANCE),
        }
    }
}

impl Default for WorldCullingSettings {
    fn default() -> Self {
        Self {
            draw_distance: default_world_draw_distance(),
            bsp_patch_extent: default_bsp_patch_extent(),
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
    /// Optional precise acceleration in Q8 authoring units per 60 Hz tick squared.
    /// Takes precedence over the legacy whole-unit value when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gravity_per_tick_q8: Option<i32>,
}

impl WorldPhysicsSettings {
    /// Clamp authored values to runtime-safe integer ranges.
    pub fn normalized(self) -> Self {
        Self {
            gravity_per_tick: self
                .gravity_per_tick
                .clamp(MIN_WORLD_GRAVITY_PER_TICK, MAX_WORLD_GRAVITY_PER_TICK),
            gravity_per_tick_q8: self
                .gravity_per_tick_q8
                .map(|v| v.clamp(0, MAX_WORLD_GRAVITY_PER_TICK * 256)),
        }
    }
}

impl Default for WorldPhysicsSettings {
    fn default() -> Self {
        Self {
            gravity_per_tick: default_world_gravity_per_tick(),
            gravity_per_tick_q8: None,
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
