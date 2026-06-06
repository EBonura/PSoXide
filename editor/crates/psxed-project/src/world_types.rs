use super::*;

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

/// World sky rendering mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkyMode {
    /// Disable authored sky rendering. The renderer clears to
    /// [`SkySettings::lower_color`] only.
    Off,
    /// Draw a cooked cyclorama before world geometry.
    Gradient,
}

impl Default for SkyMode {
    fn default() -> Self {
        Self::Gradient
    }
}

/// World-level sky configuration shared by descendant Rooms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkySettings {
    /// Whether this World renders a sky.
    #[serde(default)]
    pub mode: SkyMode,
    /// Zenith colour.
    #[serde(default = "default_sky_top_color")]
    pub top_color: [u8; 3],
    /// Colour at the authored horizon line.
    #[serde(default = "default_sky_horizon_color")]
    pub horizon_color: [u8; 3],
    /// Colour at the bottom of the frame.
    #[serde(default = "default_sky_lower_color")]
    pub lower_color: [u8; 3],
    /// Horizon line as a percentage of screen height.
    #[serde(default = "default_sky_horizon_percent")]
    pub horizon_percent: u8,
    /// Angular thickness of the horizon band. Wider values hold the
    /// horizon colour longer before blending to zenith/lower sky.
    #[serde(default = "default_sky_horizon_thickness_percent")]
    pub horizon_thickness_percent: u8,
    /// Strength of the warm localized horizon glow baked into the
    /// cyclorama.
    #[serde(default = "default_sky_horizon_glow_percent")]
    pub horizon_glow_percent: u8,
    /// Direction of the warm horizon glow in cyclorama yaw degrees.
    #[serde(default = "default_sky_horizon_glow_yaw_degrees")]
    pub horizon_glow_yaw_degrees: i16,
    /// Whether a cooked sun disc/glow is drawn into the cyclorama.
    #[serde(default = "default_sky_sun_enabled")]
    pub sun_enabled: bool,
    /// Inner sun disc colour.
    #[serde(default = "default_sky_sun_color")]
    pub sun_color: [u8; 3],
    /// Outer sun ring / eclipse border colour.
    #[serde(default = "default_sky_sun_border_color")]
    pub sun_border_color: [u8; 3],
    /// Sun direction in cyclorama yaw degrees.
    #[serde(default = "default_sky_sun_yaw_degrees")]
    pub sun_yaw_degrees: i16,
    /// Sun height in cyclorama pitch degrees.
    #[serde(default = "default_sky_sun_pitch_degrees")]
    pub sun_pitch_degrees: i16,
    /// Cooked sun disc radius.
    #[serde(default = "default_sky_sun_size_percent")]
    pub sun_size_percent: u8,
    /// Strength of the soft glow around the sun disc.
    #[serde(default = "default_sky_sun_glow_percent")]
    pub sun_glow_percent: u8,
    /// Angular spread of the sun glow.
    #[serde(default = "default_sky_sun_glow_size_percent")]
    pub sun_glow_size_percent: u8,
    /// Height/intensity of cooked distant mountain silhouettes.
    /// Values above 100 push the baked ridge higher than the legacy
    /// runtime-geometry range.
    #[serde(default = "default_sky_mountain_height_percent")]
    pub mountain_height_percent: u8,
    /// Tint used near distant mountain peaks.
    #[serde(default = "default_sky_mountain_top_color")]
    pub mountain_top_color: [u8; 3],
    /// Tint used at the mountain bases.
    #[serde(default = "default_sky_mountain_base_color")]
    pub mountain_base_color: [u8; 3],
    /// Gap between the horizon and the mountain ridge. At the lowest
    /// values the ridge can overlap into the horizon/cloud band.
    #[serde(default = "default_sky_mountain_gap_percent")]
    pub mountain_gap_percent: u8,
    /// Jaggedness of the generated mountain silhouette.
    #[serde(default = "default_sky_mountain_roughness_percent")]
    pub mountain_roughness_percent: u8,
    /// Number of parallax-free painted mountain layers.
    #[serde(default = "default_sky_mountain_layer_count")]
    pub mountain_layer_count: u8,
    /// Horizontal cyclorama subdivisions used by the editor preview
    /// and runtime sky renderer.
    #[serde(default = "default_skybox_columns")]
    pub skybox_columns: u8,
    /// Vertical cyclorama subdivisions used by the editor preview
    /// and runtime sky renderer.
    #[serde(default = "default_skybox_rows")]
    pub skybox_rows: u8,
    /// Blend horizon/lower sky toward the room fog colour when
    /// fog is enabled.
    #[serde(default = "default_sky_match_room_fog")]
    pub match_room_fog: bool,
    /// Optional cloud-layer settings folded into the cooked
    /// cyclorama backdrop.
    #[serde(default)]
    pub cloud_layer: CloudLayerSettings,
}

/// Cloud-layer authoring fields. The cooker folds these values into
/// the generated vertex-coloured cyclorama backdrop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudLayerSettings {
    /// Whether the cloud layer is drawn at all.
    #[serde(default)]
    pub enabled: bool,
    /// Cloud highlight colour used by the cyclorama cloud streaks.
    #[serde(default = "default_cloud_color")]
    pub color: [u8; 3],
    /// 0 = no coverage, 255 = maximum coverage.
    #[serde(default = "default_cloud_density")]
    pub density: u8,
    /// Vertical bias for the cyclorama cloud band.
    #[serde(default = "default_cloud_altitude")]
    pub altitude: u16,
    /// Width of the cyclorama cloud band.
    #[serde(default = "default_cloud_extent")]
    pub extent: u16,
    /// Cloud scroll speed reserved for animated cyclorama variants.
    #[serde(default = "default_cloud_scroll_speed")]
    pub scroll_speed: [i16; 2],
    /// Number of noise/tile repeats across the cloud layer. More
    /// tiles = denser-looking cover but smaller-feeling clouds.
    #[serde(default = "default_cloud_tile_count")]
    pub tile_count: u8,
    /// Seed for the cloud noise. Change to get a different cloud
    /// pattern.
    #[serde(default = "default_cloud_noise_seed")]
    pub noise_seed: u32,
}

pub(crate) fn default_cloud_color() -> [u8; 3] {
    [220, 220, 232]
}
pub(crate) const fn default_cloud_density() -> u8 {
    128
}
pub(crate) const fn default_cloud_altitude() -> u16 {
    6144
}
pub(crate) const fn default_cloud_extent() -> u16 {
    24_576
}
pub(crate) const fn default_cloud_scroll_speed() -> [i16; 2] {
    [4, 0]
}
pub(crate) const fn default_cloud_tile_count() -> u8 {
    4
}
pub(crate) const fn default_cloud_noise_seed() -> u32 {
    0x5a7b_c91d
}

impl Default for CloudLayerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            color: default_cloud_color(),
            density: default_cloud_density(),
            altitude: default_cloud_altitude(),
            extent: default_cloud_extent(),
            scroll_speed: default_cloud_scroll_speed(),
            tile_count: default_cloud_tile_count(),
            noise_seed: default_cloud_noise_seed(),
        }
    }
}

impl SkySettings {
    /// Resolve authored sky values against room-local fog metadata.
    pub fn resolved_for_room(self, fog_enabled: bool, fog_color: [u8; 3]) -> ResolvedSkySettings {
        let mut horizon_color = self.horizon_color;
        let mut lower_color = self.lower_color;
        if self.match_room_fog && fog_enabled {
            horizon_color = blend_rgb(self.horizon_color, fog_color, 128);
            lower_color = blend_rgb(self.lower_color, fog_color, 192);
        }
        ResolvedSkySettings {
            enabled: self.mode == SkyMode::Gradient,
            top_color: self.top_color,
            horizon_color,
            lower_color,
            horizon_percent: self.horizon_percent.clamp(5, 95),
            horizon_thickness_percent: self.horizon_thickness_percent.clamp(0, 80),
            horizon_glow_percent: self.horizon_glow_percent.clamp(0, 100),
            horizon_glow_yaw_degrees: self.horizon_glow_yaw_degrees.clamp(-180, 180),
            sun_enabled: self.sun_enabled,
            sun_color: self.sun_color,
            sun_border_color: self.sun_border_color,
            sun_yaw_degrees: self.sun_yaw_degrees.clamp(-180, 180),
            sun_pitch_degrees: self.sun_pitch_degrees.clamp(-30, 75),
            sun_size_percent: self.sun_size_percent.clamp(1, 100),
            sun_glow_percent: self.sun_glow_percent.clamp(0, 100),
            sun_glow_size_percent: self.sun_glow_size_percent.clamp(0, 100),
            mountain_height_percent: self
                .mountain_height_percent
                .clamp(0, SKY_MOUNTAIN_HEIGHT_PERCENT_MAX),
            mountain_top_color: self.mountain_top_color,
            mountain_base_color: self.mountain_base_color,
            mountain_gap_percent: self.mountain_gap_percent.clamp(0, 100),
            mountain_roughness_percent: self.mountain_roughness_percent.clamp(0, 100),
            mountain_layer_count: self.mountain_layer_count.clamp(1, 3),
            skybox_columns: self
                .skybox_columns
                .clamp(SKYBOX_COLUMNS_MIN, SKYBOX_COLUMNS_MAX),
            skybox_rows: self.skybox_rows.clamp(SKYBOX_ROWS_MIN, SKYBOX_ROWS_MAX),
            cloud_layer: self.cloud_layer,
        }
    }
}

impl Default for SkySettings {
    fn default() -> Self {
        Self {
            mode: SkyMode::Gradient,
            top_color: default_sky_top_color(),
            horizon_color: default_sky_horizon_color(),
            lower_color: default_sky_lower_color(),
            horizon_percent: default_sky_horizon_percent(),
            horizon_thickness_percent: default_sky_horizon_thickness_percent(),
            horizon_glow_percent: default_sky_horizon_glow_percent(),
            horizon_glow_yaw_degrees: default_sky_horizon_glow_yaw_degrees(),
            sun_enabled: default_sky_sun_enabled(),
            sun_color: default_sky_sun_color(),
            sun_border_color: default_sky_sun_border_color(),
            sun_yaw_degrees: default_sky_sun_yaw_degrees(),
            sun_pitch_degrees: default_sky_sun_pitch_degrees(),
            sun_size_percent: default_sky_sun_size_percent(),
            sun_glow_percent: default_sky_sun_glow_percent(),
            sun_glow_size_percent: default_sky_sun_glow_size_percent(),
            mountain_height_percent: default_sky_mountain_height_percent(),
            mountain_top_color: default_sky_mountain_top_color(),
            mountain_base_color: default_sky_mountain_base_color(),
            mountain_gap_percent: default_sky_mountain_gap_percent(),
            mountain_roughness_percent: default_sky_mountain_roughness_percent(),
            mountain_layer_count: default_sky_mountain_layer_count(),
            skybox_columns: default_skybox_columns(),
            skybox_rows: default_skybox_rows(),
            match_room_fog: default_sky_match_room_fog(),
            cloud_layer: CloudLayerSettings::default(),
        }
    }
}

/// Sky values after room-fog matching and validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedSkySettings {
    /// Whether the gradient should be drawn.
    pub enabled: bool,
    /// Zenith colour.
    pub top_color: [u8; 3],
    /// Colour at the horizon line.
    pub horizon_color: [u8; 3],
    /// Colour at the bottom of the frame / clear.
    pub lower_color: [u8; 3],
    /// Horizon line as a percentage of screen height.
    pub horizon_percent: u8,
    /// Angular thickness of the horizon colour band.
    pub horizon_thickness_percent: u8,
    /// Strength of the warm localized horizon glow.
    pub horizon_glow_percent: u8,
    /// Direction of the warm horizon glow in cyclorama yaw degrees.
    pub horizon_glow_yaw_degrees: i16,
    /// Whether a cooked sun disc/glow is drawn.
    pub sun_enabled: bool,
    /// Inner sun disc colour.
    pub sun_color: [u8; 3],
    /// Outer sun ring / eclipse border colour.
    pub sun_border_color: [u8; 3],
    /// Sun direction in cyclorama yaw degrees.
    pub sun_yaw_degrees: i16,
    /// Sun height in cyclorama pitch degrees.
    pub sun_pitch_degrees: i16,
    /// Cooked sun disc radius.
    pub sun_size_percent: u8,
    /// Strength of the soft glow around the sun disc.
    pub sun_glow_percent: u8,
    /// Angular spread of the sun glow.
    pub sun_glow_size_percent: u8,
    /// Height/intensity of cooked distant mountain silhouettes.
    pub mountain_height_percent: u8,
    /// Tint used near distant mountain peaks.
    pub mountain_top_color: [u8; 3],
    /// Tint used at mountain bases.
    pub mountain_base_color: [u8; 3],
    /// Gap between horizon and generated ridge.
    pub mountain_gap_percent: u8,
    /// Jaggedness of the generated mountain silhouette.
    pub mountain_roughness_percent: u8,
    /// Number of painted mountain layers.
    pub mountain_layer_count: u8,
    /// Horizontal cyclorama subdivisions.
    pub skybox_columns: u8,
    /// Vertical cyclorama subdivisions.
    pub skybox_rows: u8,
    /// Resolved cloud layer authoring values used by the cyclorama
    /// generator.
    pub cloud_layer: CloudLayerSettings,
}

/// One generated cyclorama backdrop quad. Directions are unit vectors
/// in Q0.12-ish scale. Runtime/editor preview apply camera rotation
/// only, so this behaves like an infinite authored panorama.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkyCycloramaQuad {
    /// Corner directions ordered top-left, top-right, bottom-left,
    /// bottom-right in angular cyclorama space.
    pub direction_q12: [[i16; 3]; 4],
    /// Per-corner Gouraud colours.
    pub rgb: [[u8; 3]; 4],
}

pub(crate) const SKY_CYCLORAMA_MOUNTAIN_LAYERS: usize = 3;
pub(crate) const SKY_CYCLORAMA_MOUNTAIN_COLUMNS_MAX: usize = 128;
pub(crate) const SKY_CYCLORAMA_CLOUD_STREAK_MAX: usize = 6;
pub(crate) const SKY_CYCLORAMA_CLOUD_HERO_STREAKS: usize = 4;
pub(crate) const SKY_CYCLORAMA_CLOUD_SEGMENTS_MAX: usize = 10;
pub(crate) const SKY_CYCLORAMA_CLOUD_RIBBONS: usize = 3;
pub(crate) const SKY_CYCLORAMA_CLOUD_RIBBON_QUADS: usize = 2;
pub(crate) const SKY_CYCLORAMA_STAR_COUNT_MAX: usize = 64;
pub(crate) const SKY_CYCLORAMA_SUN_SEGMENTS: usize = 24;
pub(crate) const SKY_CYCLORAMA_SUN_GLOW_QUADS: usize = SKY_CYCLORAMA_SUN_SEGMENTS;
pub(crate) const SKY_CYCLORAMA_SUN_BORDER_QUADS: usize = SKY_CYCLORAMA_SUN_SEGMENTS * 2;
pub(crate) const SKY_CYCLORAMA_SUN_CORE_QUADS: usize = SKY_CYCLORAMA_SUN_SEGMENTS;
pub(crate) const SKY_CYCLORAMA_SUN_QUAD_MAX: usize =
    SKY_CYCLORAMA_SUN_GLOW_QUADS + SKY_CYCLORAMA_SUN_BORDER_QUADS + SKY_CYCLORAMA_SUN_CORE_QUADS;
/// Runtime panorama texture width, in 4bpp texels.
pub const SKY_PANORAMA_WIDTH: u16 = 512;
/// Runtime panorama texture height, in 4bpp texels.
pub const SKY_PANORAMA_HEIGHT: u16 = 256;
/// Horizontal 4bpp palette bands. Runtime draws one sky row per
/// band so each altitude range can use its own 16-colour CLUT.
pub const SKY_PANORAMA_PALETTE_BANDS: usize = 8;
pub(crate) const SKY_PANORAMA_PALETTE_COLORS: usize = 16;

/// Maximum number of quads generated by [`generate_sky_cyclorama`].
pub const SKY_CYCLORAMA_QUAD_MAX: usize = SKYBOX_COLUMNS_MAX as usize * SKYBOX_ROWS_MAX as usize
    + SKY_CYCLORAMA_MOUNTAIN_COLUMNS_MAX * SKY_CYCLORAMA_MOUNTAIN_LAYERS
    + (SKY_CYCLORAMA_CLOUD_STREAK_MAX + SKY_CYCLORAMA_CLOUD_HERO_STREAKS)
        * (SKY_CYCLORAMA_CLOUD_SEGMENTS_MAX + 1)
        * SKY_CYCLORAMA_CLOUD_RIBBONS
        * SKY_CYCLORAMA_CLOUD_RIBBON_QUADS
    + SKY_CYCLORAMA_STAR_COUNT_MAX
    + SKY_CYCLORAMA_SUN_QUAD_MAX;

/// Build a Spyro-style cyclorama from authored sky settings.
///
/// This intentionally does the expensive/expressive work at cook
/// time: the output is explicit coloured backdrop geometry. Runtime
/// rendering only projects the baked directions with camera rotation.
pub fn generate_sky_cyclorama(sky: ResolvedSkySettings) -> Vec<SkyCycloramaQuad> {
    if !sky.enabled {
        return Vec::new();
    }

    let columns = sky
        .skybox_columns
        .clamp(SKYBOX_COLUMNS_MIN, SKYBOX_COLUMNS_MAX) as usize;
    let rows = sky.skybox_rows.clamp(SKYBOX_ROWS_MIN, SKYBOX_ROWS_MAX) as usize;
    let horizon_pitch = sky_horizon_pitch_degrees(sky.horizon_percent);
    let top_pitch = (horizon_pitch + 58.0).min(78.0);
    let bottom_pitch = (horizon_pitch - 46.0).max(-72.0);
    let mut out = Vec::with_capacity(SKY_CYCLORAMA_QUAD_MAX);

    for row in 0..rows {
        let t0 = row as f32 / rows as f32;
        let t1 = (row + 1) as f32 / rows as f32;
        let pitch_top = lerp_f32(top_pitch, bottom_pitch, t0);
        let pitch_bottom = lerp_f32(top_pitch, bottom_pitch, t1);
        for column in 0..columns {
            let yaw0 = cyclorama_yaw_for_column(column, columns);
            let yaw1 = cyclorama_yaw_for_column(column + 1, columns);
            push_sky_cyclorama_quad(
                &mut out,
                yaw0,
                yaw1,
                pitch_top,
                pitch_bottom,
                [
                    sky_color_for_pitch_yaw(
                        sky,
                        pitch_top,
                        yaw0,
                        horizon_pitch,
                        top_pitch,
                        bottom_pitch,
                    ),
                    sky_color_for_pitch_yaw(
                        sky,
                        pitch_top,
                        yaw1,
                        horizon_pitch,
                        top_pitch,
                        bottom_pitch,
                    ),
                    sky_color_for_pitch_yaw(
                        sky,
                        pitch_bottom,
                        yaw0,
                        horizon_pitch,
                        top_pitch,
                        bottom_pitch,
                    ),
                    sky_color_for_pitch_yaw(
                        sky,
                        pitch_bottom,
                        yaw1,
                        horizon_pitch,
                        top_pitch,
                        bottom_pitch,
                    ),
                ],
            );
        }
    }

    push_sun_cyclorama(&mut out, sky, horizon_pitch, top_pitch, bottom_pitch);
    push_star_cyclorama(&mut out, sky, horizon_pitch, top_pitch, bottom_pitch);
    push_mountain_cyclorama(&mut out, sky, columns, horizon_pitch);
    push_cloud_streak_cyclorama(&mut out, sky, horizon_pitch, top_pitch, bottom_pitch);
    out.truncate(SKY_CYCLORAMA_QUAD_MAX);
    out
}

/// Bake the resolved cyclorama into a 4bpp multi-CLUT PSXT panorama.
///
/// The editor preview still uses [`generate_sky_cyclorama`] so sky
/// controls remain inspectable as geometry. The playtest runtime uses
/// this texture path so the authored sky is projected from a compact
/// textured cyclorama mesh instead of hundreds of procedural backdrop
/// polygons.
pub fn generate_sky_panorama_psxt(sky: ResolvedSkySettings) -> Option<Vec<u8>> {
    if !sky.enabled {
        return None;
    }
    let pixels = generate_sky_panorama_pixels(sky);
    let (palette_rows, indices) = sky_quantize_panorama_bands(
        &pixels,
        SKY_PANORAMA_WIDTH as usize,
        SKY_PANORAMA_HEIGHT as usize,
        SKY_PANORAMA_PALETTE_BANDS,
    );
    psxed_tex::encode_indexed_psxt_with_clut_rows(
        SKY_PANORAMA_WIDTH,
        SKY_PANORAMA_HEIGHT,
        psxed_tex::PsxtDepth::Bit4,
        &indices,
        &palette_rows,
        false,
    )
    .ok()
}

pub(crate) fn generate_sky_panorama_pixels(sky: ResolvedSkySettings) -> Vec<[u8; 3]> {
    let width = SKY_PANORAMA_WIDTH as usize;
    let height = SKY_PANORAMA_HEIGHT as usize;
    let horizon_pitch = sky_horizon_pitch_degrees(sky.horizon_percent);
    let top_pitch = (horizon_pitch + 58.0).min(78.0);
    let bottom_pitch = (horizon_pitch - 46.0).max(-72.0);
    let mut pixels = vec![[0, 0, 0]; width * height];

    for y in 0..height {
        let v = (y as f32 + 0.5) / height as f32;
        let pitch = lerp_f32(top_pitch, bottom_pitch, v);
        for x in 0..width {
            let u = (x as f32 + 0.5) / width as f32;
            let yaw = -180.0 + 360.0 * u;
            pixels[y * width + x] =
                sky_color_for_pitch_yaw(sky, pitch, yaw, horizon_pitch, top_pitch, bottom_pitch);
        }
    }

    for quad in generate_sky_cyclorama(sky) {
        rasterize_sky_cyclorama_quad(
            &mut pixels,
            quad,
            SKY_PANORAMA_WIDTH,
            SKY_PANORAMA_HEIGHT,
            top_pitch,
            bottom_pitch,
        );
    }

    pixels
}

#[derive(Clone, Copy)]
pub(crate) struct SkyRasterVertex {
    x: f32,
    y: f32,
    rgb: [u8; 3],
}

pub(crate) fn rasterize_sky_cyclorama_quad(
    pixels: &mut [[u8; 3]],
    quad: SkyCycloramaQuad,
    width: u16,
    height: u16,
    top_pitch: f32,
    bottom_pitch: f32,
) {
    let mut vertices = [
        sky_raster_vertex(
            quad.direction_q12[0],
            quad.rgb[0],
            width,
            top_pitch,
            bottom_pitch,
        ),
        sky_raster_vertex(
            quad.direction_q12[1],
            quad.rgb[1],
            width,
            top_pitch,
            bottom_pitch,
        ),
        sky_raster_vertex(
            quad.direction_q12[2],
            quad.rgb[2],
            width,
            top_pitch,
            bottom_pitch,
        ),
        sky_raster_vertex(
            quad.direction_q12[3],
            quad.rgb[3],
            width,
            top_pitch,
            bottom_pitch,
        ),
    ];
    unwrap_sky_raster_u(&mut vertices, width as f32);
    rasterize_sky_triangle(pixels, width, height, vertices[0], vertices[1], vertices[2]);
    rasterize_sky_triangle(pixels, width, height, vertices[1], vertices[2], vertices[3]);
}

pub(crate) fn sky_raster_vertex(
    dir: [i16; 3],
    rgb: [u8; 3],
    width: u16,
    top_pitch: f32,
    bottom_pitch: f32,
) -> SkyRasterVertex {
    let x = dir[0] as f32 / 4096.0;
    let y = dir[1] as f32 / 4096.0;
    let z = dir[2] as f32 / 4096.0;
    let yaw = (-x).atan2(-z).to_degrees();
    let pitch = y.clamp(-1.0, 1.0).asin().to_degrees();
    let u = ((yaw + 180.0) / 360.0) * width as f32;
    let v =
        ((top_pitch - pitch) / (top_pitch - bottom_pitch).max(0.001)) * SKY_PANORAMA_HEIGHT as f32;
    SkyRasterVertex { x: u, y: v, rgb }
}

pub(crate) fn unwrap_sky_raster_u(vertices: &mut [SkyRasterVertex; 4], width: f32) {
    let base = vertices[0].x;
    for vertex in &mut vertices[1..] {
        while vertex.x - base > width * 0.5 {
            vertex.x -= width;
        }
        while base - vertex.x > width * 0.5 {
            vertex.x += width;
        }
    }
}

pub(crate) fn rasterize_sky_triangle(
    pixels: &mut [[u8; 3]],
    width: u16,
    height: u16,
    a: SkyRasterVertex,
    b: SkyRasterVertex,
    c: SkyRasterVertex,
) {
    let width_i32 = i32::from(width);
    let height_i32 = i32::from(height);
    let width_f = width as f32;
    for offset in [0.0, width_f, -width_f] {
        let mut a = a;
        let mut b = b;
        let mut c = c;
        a.x += offset;
        b.x += offset;
        c.x += offset;
        let area = sky_edge(a.x, a.y, b.x, b.y, c.x, c.y);
        if area.abs() < 0.0001 {
            continue;
        }
        let min_x = a.x.min(b.x).min(c.x).floor() as i32;
        let max_x = a.x.max(b.x).max(c.x).ceil() as i32;
        let min_y = (a.y.min(b.y).min(c.y).floor() as i32).clamp(0, height_i32 - 1);
        let max_y = (a.y.max(b.y).max(c.y).ceil() as i32).clamp(0, height_i32 - 1);
        for y in min_y..=max_y {
            let py = y as f32 + 0.5;
            for x in min_x..=max_x {
                let px = x as f32 + 0.5;
                let wa = sky_edge(b.x, b.y, c.x, c.y, px, py) / area;
                let wb = sky_edge(c.x, c.y, a.x, a.y, px, py) / area;
                let wc = sky_edge(a.x, a.y, b.x, b.y, px, py) / area;
                if wa < -0.001 || wb < -0.001 || wc < -0.001 {
                    continue;
                }
                let dst_x = x.rem_euclid(width_i32) as usize;
                let dst = y as usize * width as usize + dst_x;
                pixels[dst] = [
                    sky_interp_channel(a.rgb[0], b.rgb[0], c.rgb[0], wa, wb, wc),
                    sky_interp_channel(a.rgb[1], b.rgb[1], c.rgb[1], wa, wb, wc),
                    sky_interp_channel(a.rgb[2], b.rgb[2], c.rgb[2], wa, wb, wc),
                ];
            }
        }
    }
}

pub(crate) fn sky_edge(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
    (px - ax) * (by - ay) - (py - ay) * (bx - ax)
}

pub(crate) fn sky_interp_channel(a: u8, b: u8, c: u8, wa: f32, wb: f32, wc: f32) -> u8 {
    (a as f32 * wa + b as f32 * wb + c as f32 * wc)
        .round()
        .clamp(0.0, 255.0) as u8
}

#[derive(Clone)]
pub(crate) struct SkyQuantColor {
    rgb: [u8; 3],
    count: u32,
}

pub(crate) fn sky_quantize_panorama_bands(
    pixels: &[[u8; 3]],
    width: usize,
    height: usize,
    bands: usize,
) -> (Vec<Vec<[u8; 3]>>, Vec<u8>) {
    let bands = bands.max(1);
    let mut palette_rows = Vec::with_capacity(bands);
    let mut indices = vec![0u8; pixels.len()];
    for band in 0..bands {
        let y0 = band * height / bands;
        let y1 = (band + 1) * height / bands;
        let mut band_pixels = Vec::with_capacity((y1 - y0) * width);
        for y in y0..y1 {
            let start = y * width;
            band_pixels.extend_from_slice(&pixels[start..start + width]);
        }
        let (palette, band_indices) =
            sky_quantize_pixels(&band_pixels, SKY_PANORAMA_PALETTE_COLORS);
        let mut src = 0usize;
        for y in y0..y1 {
            let start = y * width;
            for x in 0..width {
                indices[start + x] = band_indices[src];
                src += 1;
            }
        }
        palette_rows.push(palette);
    }
    (palette_rows, indices)
}

pub(crate) fn sky_quantize_pixels(
    pixels: &[[u8; 3]],
    palette_colors: usize,
) -> (Vec<[u8; 3]>, Vec<u8>) {
    let mut counts: BTreeMap<u32, u32> = BTreeMap::new();
    for rgb in pixels {
        let key = ((rgb[0] as u32) << 16) | ((rgb[1] as u32) << 8) | rgb[2] as u32;
        *counts.entry(key).or_insert(0) += 1;
    }
    let entries: Vec<SkyQuantColor> = counts
        .into_iter()
        .map(|(key, count)| SkyQuantColor {
            rgb: [
                ((key >> 16) & 0xff) as u8,
                ((key >> 8) & 0xff) as u8,
                (key & 0xff) as u8,
            ],
            count,
        })
        .collect();
    let mut boxes = vec![entries];
    while boxes.len() < palette_colors {
        let Some(best_index) = sky_best_quant_box(&boxes) else {
            break;
        };
        let source = boxes.swap_remove(best_index);
        let Some((left, right)) = sky_split_quant_box(source) else {
            break;
        };
        boxes.push(left);
        boxes.push(right);
    }
    let mut palette: Vec<[u8; 3]> = boxes
        .iter()
        .filter(|colors| !colors.is_empty())
        .map(|colors| sky_quant_box_average(colors))
        .collect();
    if palette.is_empty() {
        palette.push([0, 0, 0]);
    }
    palette.truncate(palette_colors);
    let indices = pixels
        .iter()
        .map(|rgb| sky_nearest_palette_index(*rgb, &palette))
        .collect();
    (palette, indices)
}

pub(crate) fn sky_best_quant_box(boxes: &[Vec<SkyQuantColor>]) -> Option<usize> {
    let mut best_index = None;
    let mut best_score = 0u64;
    for (index, colors) in boxes.iter().enumerate() {
        if colors.len() <= 1 {
            continue;
        }
        let score = sky_quant_box_score(colors);
        if best_index.is_none() || score > best_score {
            best_index = Some(index);
            best_score = score;
        }
    }
    best_index
}

pub(crate) fn sky_split_quant_box(
    mut colors: Vec<SkyQuantColor>,
) -> Option<(Vec<SkyQuantColor>, Vec<SkyQuantColor>)> {
    if colors.len() <= 1 {
        return None;
    }
    let channel = sky_quant_box_split_channel(&colors);
    colors.sort_by_key(|color| (color.rgb[channel], color.rgb[0], color.rgb[1], color.rgb[2]));
    let total: u32 = colors.iter().map(|color| color.count).sum();
    let midpoint = total / 2;
    let mut running = 0u32;
    let mut split = 1usize;
    for (index, color) in colors.iter().enumerate() {
        running = running.saturating_add(color.count);
        if running >= midpoint {
            split = (index + 1).clamp(1, colors.len() - 1);
            break;
        }
    }
    let right = colors.split_off(split);
    Some((colors, right))
}

pub(crate) fn sky_quant_box_split_channel(colors: &[SkyQuantColor]) -> usize {
    let mut mins = [u8::MAX; 3];
    let mut maxs = [0u8; 3];
    for color in colors {
        for channel in 0..3 {
            mins[channel] = mins[channel].min(color.rgb[channel]);
            maxs[channel] = maxs[channel].max(color.rgb[channel]);
        }
    }
    let mut best_channel = 0usize;
    let mut best_range = 0u8;
    for channel in 0..3 {
        let range = maxs[channel].saturating_sub(mins[channel]);
        if range > best_range {
            best_channel = channel;
            best_range = range;
        }
    }
    best_channel
}

pub(crate) fn sky_quant_box_score(colors: &[SkyQuantColor]) -> u64 {
    let mut mins = [u8::MAX; 3];
    let mut maxs = [0u8; 3];
    let mut total = 0u64;
    for color in colors {
        total += u64::from(color.count);
        for channel in 0..3 {
            mins[channel] = mins[channel].min(color.rgb[channel]);
            maxs[channel] = maxs[channel].max(color.rgb[channel]);
        }
    }
    let range = (0..3)
        .map(|channel| maxs[channel].saturating_sub(mins[channel]) as u64)
        .max()
        .unwrap_or(0);
    (range + 1) * total
}

pub(crate) fn sky_quant_box_average(colors: &[SkyQuantColor]) -> [u8; 3] {
    let total: u64 = colors.iter().map(|color| u64::from(color.count)).sum();
    if total == 0 {
        return [0, 0, 0];
    }
    let mut sums = [0u64; 3];
    for color in colors {
        let count = u64::from(color.count);
        for channel in 0..3 {
            sums[channel] += u64::from(color.rgb[channel]) * count;
        }
    }
    [
        ((sums[0] + total / 2) / total) as u8,
        ((sums[1] + total / 2) / total) as u8,
        ((sums[2] + total / 2) / total) as u8,
    ]
}

pub(crate) fn sky_nearest_palette_index(rgb: [u8; 3], palette: &[[u8; 3]]) -> u8 {
    let mut best_index = 0usize;
    let mut best_distance = u32::MAX;
    for (index, color) in palette.iter().enumerate() {
        let dr = i32::from(rgb[0]) - i32::from(color[0]);
        let dg = i32::from(rgb[1]) - i32::from(color[1]);
        let db = i32::from(rgb[2]) - i32::from(color[2]);
        let distance = (dr * dr + dg * dg + db * db) as u32;
        if distance < best_distance {
            best_index = index;
            best_distance = distance;
        }
    }
    best_index as u8
}

pub(crate) fn push_sun_cyclorama(
    out: &mut Vec<SkyCycloramaQuad>,
    sky: ResolvedSkySettings,
    horizon_pitch: f32,
    top_pitch: f32,
    bottom_pitch: f32,
) {
    if !sky.sun_enabled {
        return;
    }

    let yaw = sky.sun_yaw_degrees as f32;
    let pitch = (sky.sun_pitch_degrees as f32).clamp(bottom_pitch + 2.0, top_pitch - 2.0);
    let size_t = sky.sun_size_percent.clamp(1, 100) as f32 / 100.0;
    let glow_t = sky.sun_glow_percent.clamp(0, 100) as f32 / 100.0;
    let glow_size_t = sky.sun_glow_size_percent.clamp(0, 100) as f32 / 100.0;
    let disc_radius = lerp_f32(0.75, 5.2, size_t);

    let glow_radius = (disc_radius + lerp_f32(1.15, 6.4, glow_size_t)).min(12.0);
    if glow_t > 0.0 && glow_size_t > 0.0 {
        push_sun_disc_fan(
            out,
            sky,
            yaw,
            pitch,
            glow_radius,
            glow_radius * 0.7,
            0.63,
            0.34,
            |sky, point_yaw, point_pitch, radius_t, theta| {
                let falloff = (1.0 - radius_t.clamp(0.0, 1.0)).powf(1.65);
                let alpha = (24.0 + glow_t * 88.0) * falloff;
                let highlight = sun_directional_weight(theta, 0.68, 2.2);
                let tint = cyclorama_lerp_rgb(
                    brighten_rgb(sky.sun_border_color, 12),
                    [255, 206, 156],
                    (highlight * 96.0).clamp(0.0, 255.0) as u8,
                );
                sun_tinted_sky_color(
                    sky,
                    point_yaw,
                    point_pitch,
                    tint,
                    alpha,
                    horizon_pitch,
                    top_pitch,
                    bottom_pitch,
                )
            },
        );
    }

    push_sun_annulus_triangles(
        out,
        sky,
        yaw,
        pitch,
        disc_radius,
        disc_radius * 0.98,
        0.52,
        1.08,
        0.41,
        0.82,
        |sky, point_yaw, point_pitch, radius_t, theta| {
            let ridge = smooth_falloff(0.34, (radius_t - 0.8).abs());
            let outer_feather = smooth_falloff(0.18, (radius_t - 1.0).abs());
            let alpha = (166.0 + glow_t * 54.0) * ridge.max(outer_feather * 0.25);
            let highlight = sun_directional_weight(theta, 0.74, 3.1);
            let shade = sun_directional_weight(theta, 3.88, 2.0);
            let mut tint = cyclorama_lerp_rgb(
                sky.sun_border_color,
                [255, 226, 184],
                (highlight * 118.0).clamp(0.0, 255.0) as u8,
            );
            tint = cyclorama_lerp_rgb(tint, [60, 22, 26], (shade * 34.0).clamp(0.0, 255.0) as u8);
            sun_tinted_sky_color(
                sky,
                point_yaw,
                point_pitch,
                tint,
                alpha,
                horizon_pitch,
                top_pitch,
                bottom_pitch,
            )
        },
    );

    push_sun_disc_fan(
        out,
        sky,
        yaw,
        pitch,
        disc_radius * 0.58,
        disc_radius * 0.58,
        1.24,
        0.48,
        |sky, point_yaw, point_pitch, radius_t, _theta| {
            let edge = smooth_step(((radius_t - 0.72) / 0.28).clamp(0.0, 1.0));
            let alpha = lerp_f32(255.0, 228.0, edge);
            sun_tinted_sky_color(
                sky,
                point_yaw,
                point_pitch,
                sky.sun_color,
                alpha,
                horizon_pitch,
                top_pitch,
                bottom_pitch,
            )
        },
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn push_sun_disc_fan<F>(
    out: &mut Vec<SkyCycloramaQuad>,
    sky: ResolvedSkySettings,
    center_yaw: f32,
    center_pitch: f32,
    yaw_radius: f32,
    pitch_radius: f32,
    shape_phase: f32,
    shape_strength: f32,
    mut shade_vertex: F,
) where
    F: FnMut(ResolvedSkySettings, f32, f32, f32, f32) -> [u8; 3],
{
    for segment in 0..SKY_CYCLORAMA_SUN_SEGMENTS {
        let theta0 = std::f32::consts::TAU * segment as f32 / SKY_CYCLORAMA_SUN_SEGMENTS as f32;
        let theta1 =
            std::f32::consts::TAU * (segment + 1) as f32 / SKY_CYCLORAMA_SUN_SEGMENTS as f32;
        let (yaw0, pitch0) = sun_polar_point(
            center_yaw,
            center_pitch,
            yaw_radius,
            pitch_radius,
            1.0,
            theta0,
            shape_phase,
            shape_strength,
        );
        let (yaw1, pitch1) = sun_polar_point(
            center_yaw,
            center_pitch,
            yaw_radius,
            pitch_radius,
            1.0,
            theta1,
            shape_phase,
            shape_strength,
        );
        push_sky_cyclorama_triangle(
            out,
            [(center_yaw, center_pitch), (yaw0, pitch0), (yaw1, pitch1)],
            [
                shade_vertex(sky, center_yaw, center_pitch, 0.0, (theta0 + theta1) * 0.5),
                shade_vertex(sky, yaw0, pitch0, 1.0, theta0),
                shade_vertex(sky, yaw1, pitch1, 1.0, theta1),
            ],
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn push_sun_annulus_triangles<F>(
    out: &mut Vec<SkyCycloramaQuad>,
    sky: ResolvedSkySettings,
    center_yaw: f32,
    center_pitch: f32,
    yaw_radius: f32,
    pitch_radius: f32,
    inner_radius: f32,
    outer_radius: f32,
    shape_phase: f32,
    shape_strength: f32,
    mut shade_vertex: F,
) where
    F: FnMut(ResolvedSkySettings, f32, f32, f32, f32) -> [u8; 3],
{
    for segment in 0..SKY_CYCLORAMA_SUN_SEGMENTS {
        let theta0 = std::f32::consts::TAU * segment as f32 / SKY_CYCLORAMA_SUN_SEGMENTS as f32;
        let theta1 =
            std::f32::consts::TAU * (segment + 1) as f32 / SKY_CYCLORAMA_SUN_SEGMENTS as f32;
        let inner0 = sun_polar_point(
            center_yaw,
            center_pitch,
            yaw_radius,
            pitch_radius,
            inner_radius,
            theta0,
            shape_phase,
            shape_strength,
        );
        let inner1 = sun_polar_point(
            center_yaw,
            center_pitch,
            yaw_radius,
            pitch_radius,
            inner_radius,
            theta1,
            shape_phase,
            shape_strength,
        );
        let outer0 = sun_polar_point(
            center_yaw,
            center_pitch,
            yaw_radius,
            pitch_radius,
            outer_radius,
            theta0,
            shape_phase,
            shape_strength,
        );
        let outer1 = sun_polar_point(
            center_yaw,
            center_pitch,
            yaw_radius,
            pitch_radius,
            outer_radius,
            theta1,
            shape_phase,
            shape_strength,
        );
        push_sky_cyclorama_triangle(
            out,
            [inner0, inner1, outer0],
            [
                shade_vertex(sky, inner0.0, inner0.1, inner_radius, theta0),
                shade_vertex(sky, inner1.0, inner1.1, inner_radius, theta1),
                shade_vertex(sky, outer0.0, outer0.1, outer_radius, theta0),
            ],
        );
        push_sky_cyclorama_triangle(
            out,
            [inner1, outer1, outer0],
            [
                shade_vertex(sky, inner1.0, inner1.1, inner_radius, theta1),
                shade_vertex(sky, outer1.0, outer1.1, outer_radius, theta1),
                shade_vertex(sky, outer0.0, outer0.1, outer_radius, theta0),
            ],
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn sun_polar_point(
    center_yaw: f32,
    center_pitch: f32,
    yaw_radius: f32,
    pitch_radius: f32,
    radius: f32,
    theta: f32,
    shape_phase: f32,
    shape_strength: f32,
) -> (f32, f32) {
    let shape = sun_shape_scale(theta, shape_phase, shape_strength);
    let radius = radius * shape;
    (
        center_yaw + theta.cos() * yaw_radius * radius,
        center_pitch + theta.sin() * pitch_radius * radius,
    )
}

pub(crate) fn sun_shape_scale(theta: f32, phase: f32, strength: f32) -> f32 {
    let wave = 0.08 * (theta * 3.0 + phase).sin()
        + 0.05 * (theta * 5.0 - phase * 0.7).cos()
        + 0.035 * (theta * 9.0 + phase * 1.6).sin();
    (1.0 + wave * strength).clamp(0.72, 1.24)
}

pub(crate) fn sun_directional_weight(theta: f32, direction: f32, power: f32) -> f32 {
    theta
        .cos()
        .mul_add(direction.cos(), theta.sin() * direction.sin())
        .max(0.0)
        .powf(power.max(0.01))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn sun_tinted_sky_color(
    sky: ResolvedSkySettings,
    yaw: f32,
    pitch: f32,
    tint: [u8; 3],
    alpha: f32,
    horizon_pitch: f32,
    top_pitch: f32,
    bottom_pitch: f32,
) -> [u8; 3] {
    let base =
        sky_color_for_pitch_yaw_core(sky, pitch, yaw, horizon_pitch, top_pitch, bottom_pitch);
    cyclorama_lerp_rgb(base, tint, alpha.clamp(0.0, 255.0) as u8)
}

pub(crate) fn push_star_cyclorama(
    out: &mut Vec<SkyCycloramaQuad>,
    sky: ResolvedSkySettings,
    horizon_pitch: f32,
    top_pitch: f32,
    bottom_pitch: f32,
) {
    let darkness = (1.0 - rgb_luma(sky.top_color) / 118.0).clamp(0.0, 1.0);
    if darkness <= 0.08 {
        return;
    }
    let upper_bottom = (horizon_pitch + 12.0).max(bottom_pitch + 34.0);
    let upper_top = top_pitch - 3.0;
    if upper_top <= upper_bottom + 4.0 {
        return;
    }
    let cloud = sky.cloud_layer;
    let density_t = if cloud.enabled {
        cloud_density_response(cloud.density)
    } else {
        0.0
    };
    let count = (18.0 + darkness * 34.0 + (1.0 - density_t) * 12.0).round() as usize;
    let count = count.clamp(8, SKY_CYCLORAMA_STAR_COUNT_MAX);
    let seed = cloud.noise_seed ^ 0x7374_6172;
    for star in 0..count {
        let h = sky_hash_u32(seed, star as u32);
        let yaw = -180.0 + sky_hash_unit(h, 0) * 360.0;
        let height_t = sky_hash_unit(h, 1).powf(0.55);
        let pitch = lerp_f32(upper_bottom, upper_top, height_t);
        let twinkle = 0.45 + sky_hash_unit(h, 2) * 0.55;
        let size = (0.1 + sky_hash_unit(h, 3) * 0.2) * (0.8 + twinkle * 0.5);
        if yaw - size <= -180.0 || yaw + size >= 180.0 {
            continue;
        }
        let base =
            sky_color_for_pitch_yaw_core(sky, pitch, yaw, horizon_pitch, top_pitch, bottom_pitch);
        let cool = cyclorama_lerp_rgb([205, 218, 255], [255, 232, 190], sky_hash_u32(h, 4) as u8);
        let alpha = (120.0 + darkness * 92.0 + twinkle * 42.0).clamp(0.0, 255.0) as u8;
        let star_rgb = cyclorama_lerp_rgb(base, cool, alpha);
        push_sky_cyclorama_quad_corners(
            out,
            yaw - size,
            yaw + size,
            pitch + size * 0.72,
            pitch + size * 0.72,
            pitch - size * 0.72,
            pitch - size * 0.72,
            [star_rgb; 4],
        );
    }
}

pub(crate) fn push_mountain_cyclorama(
    out: &mut Vec<SkyCycloramaQuad>,
    sky: ResolvedSkySettings,
    columns: usize,
    horizon_pitch: f32,
) {
    if sky.mountain_height_percent == 0 {
        return;
    }
    let mountain_columns = (columns * 5).clamp(40, SKY_CYCLORAMA_MOUNTAIN_COLUMNS_MAX);
    let height_t = sky
        .mountain_height_percent
        .clamp(0, SKY_MOUNTAIN_HEIGHT_PERCENT_MAX) as f32
        / 100.0;
    let layer_count = sky
        .mountain_layer_count
        .clamp(1, SKY_CYCLORAMA_MOUNTAIN_LAYERS as u8);
    let seed = sky.cloud_layer.noise_seed ^ 0x6d2b_79f5;
    for layer in 0..usize::from(layer_count) {
        let depth_t = (layer + 1) as f32 / layer_count as f32;
        let layer_seed = seed ^ sky_hash_u32(0xa341_316c, layer as u32);
        for column in 0..mountain_columns {
            let yaw0 = cyclorama_yaw_for_column(column, mountain_columns);
            let yaw1 = cyclorama_yaw_for_column(column + 1, mountain_columns);
            push_mountain_layer_cyclorama(
                out,
                layer_seed,
                sky,
                yaw0,
                yaw1,
                horizon_pitch,
                height_t,
                depth_t,
            );
        }
    }
}

pub(crate) fn push_mountain_layer_cyclorama(
    out: &mut Vec<SkyCycloramaQuad>,
    seed: u32,
    sky: ResolvedSkySettings,
    yaw0: f32,
    yaw1: f32,
    horizon_pitch: f32,
    height_t: f32,
    depth_t: f32,
) {
    let phase = 9.0 + depth_t * 19.0;
    let gap_t = sky.mountain_gap_percent.clamp(0, 100) as f32 / 100.0;
    let rough_t = sky.mountain_roughness_percent.clamp(0, 100) as f32 / 100.0;
    let gap_degrees = lerp_f32(-7.0, 18.0, gap_t) + depth_t * 3.0;
    let top_base = horizon_pitch - gap_degrees;
    let amplitude = (4.5 + rough_t * 10.5 + depth_t * 4.0) * height_t;
    let base_pitch = top_base - (13.0 + height_t * 26.0 + depth_t * 8.0);
    let top0 = top_base + mountain_profile(seed, yaw0 + phase, rough_t) * amplitude;
    let top1 = top_base + mountain_profile(seed, yaw1 + phase, rough_t) * amplitude;
    let peak = cyclorama_lerp_rgb(
        sky.horizon_color,
        sky.mountain_top_color,
        (72.0 + depth_t * 118.0) as u8,
    );
    let base = cyclorama_lerp_rgb(
        sky.lower_color,
        sky.mountain_base_color,
        (96.0 + depth_t * 116.0) as u8,
    );
    push_sky_cyclorama_quad_corners(
        out,
        yaw0,
        yaw1,
        top0,
        top1,
        base_pitch,
        base_pitch,
        [peak, peak, base, base],
    );
}

pub(crate) fn push_cloud_streak_cyclorama(
    out: &mut Vec<SkyCycloramaQuad>,
    sky: ResolvedSkySettings,
    horizon_pitch: f32,
    top_pitch: f32,
    bottom_pitch: f32,
) {
    let cloud = sky.cloud_layer;
    if !cloud.enabled || cloud.density == 0 {
        return;
    }
    let tile_count = cloud.tile_count.clamp(1, 16);
    let altitude_t = (cloud.altitude as f32 / u16::MAX as f32).clamp(0.0, 1.0);
    let extent_t = (cloud.extent as f32 / u16::MAX as f32).clamp(0.0, 1.0);
    let detail_t = (tile_count.saturating_sub(1) as f32 / 15.0).clamp(0.0, 1.0);
    let segment_count = (6 + usize::from(tile_count / 4) + usize::from(cloud.density / 128))
        .clamp(6, SKY_CYCLORAMA_CLOUD_SEGMENTS_MAX);
    let count = (3 + usize::from(cloud.density / 64) + usize::from(tile_count / 8))
        .min(SKY_CYCLORAMA_CLOUD_STREAK_MAX);
    let density_t = cloud_density_response(cloud.density);
    let band_center = horizon_pitch + 4.0 + altitude_t * 28.0;
    let pitch_spread = 3.5 + extent_t * 18.0;
    let width_scale = 0.55 + extent_t * 0.88;
    let repeat_scale = 1.05 + detail_t * 0.45;
    let hero_yaw = sky.horizon_glow_yaw_degrees as f32;
    for (bank, offset) in [-92.0_f32, -34.0, 28.0, 88.0].iter().enumerate() {
        let bank_t = bank as f32 / (SKY_CYCLORAMA_CLOUD_HERO_STREAKS - 1).max(1) as f32;
        let width = (72.0 + bank_t * 42.0) * width_scale.min(1.25);
        let center_pitch = band_center + (bank_t - 0.6) * pitch_spread * 0.36;
        let thickness = 1.25 + extent_t * (2.75 + bank_t * 0.95);
        let slant = -4.0 + bank_t * 5.8;
        let tint = cyclorama_lerp_rgb(cloud.color, [255, 166, 150], (64.0 + bank_t * 38.0) as u8);
        push_cloud_streak_segments(
            out,
            sky,
            hero_yaw + offset - width * 0.5,
            width,
            center_pitch,
            thickness,
            slant,
            tint,
            density_t,
            0.96,
            segment_count,
            cloud.noise_seed ^ sky_hash_u32(0x27d4eb2d, bank as u32),
            horizon_pitch,
            top_pitch,
            bottom_pitch,
        );
    }
    for streak in 0..count {
        let h = sky_hash_u32(cloud.noise_seed, streak as u32);
        let yaw_start = -180.0 + sky_hash_unit(h, 0) * 360.0;
        let width = (30.0 + sky_hash_unit(h, 1) * 74.0) * width_scale / repeat_scale;
        let center_pitch = band_center + (sky_hash_unit(h, 2) - 0.5) * pitch_spread;
        let thickness = (1.05 + sky_hash_unit(h, 3) * 3.3) / (0.9 + tile_count as f32 * 0.02);
        let slant = (-6.0 + sky_hash_unit(h, 4) * 12.0) * (0.6 + extent_t * 0.45);
        let tint = cyclorama_lerp_rgb(
            cloud.color,
            [255, 170, 142],
            (32.0 + sky_hash_unit(h, 5) * 92.0) as u8,
        );
        push_cloud_streak_segments(
            out,
            sky,
            yaw_start,
            width,
            center_pitch,
            thickness,
            slant,
            tint,
            density_t,
            1.0,
            segment_count,
            h,
            horizon_pitch,
            top_pitch,
            bottom_pitch,
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn push_cloud_streak_segments(
    out: &mut Vec<SkyCycloramaQuad>,
    sky: ResolvedSkySettings,
    yaw_start: f32,
    width: f32,
    center_pitch: f32,
    thickness: f32,
    slant: f32,
    tint: [u8; 3],
    density_t: f32,
    alpha_scale: f32,
    segment_count: usize,
    seed: u32,
    horizon_pitch: f32,
    top_pitch: f32,
    bottom_pitch: f32,
) {
    let shadow = cyclorama_lerp_rgb(tint, sky.lower_color, 58);
    let body = brighten_rgb(cyclorama_lerp_rgb(tint, [255, 190, 166], 82), 4);
    let warm = brighten_rgb(cyclorama_lerp_rgb(tint, [255, 222, 198], 168), 12);
    let segment_count = segment_count.clamp(2, SKY_CYCLORAMA_CLOUD_SEGMENTS_MAX);
    for segment in 0..segment_count {
        let t0 = segment as f32 / segment_count as f32;
        let t1 = (segment + 1) as f32 / segment_count as f32;
        let yaw0 = yaw_start + width * t0;
        let yaw1 = yaw_start + width * t1;
        let pitch0 =
            center_pitch + slant * (t0 - 0.5) + cloud_lobe_pitch(seed ^ 0x9e37_79b9, t0, thickness);
        let pitch1 =
            center_pitch + slant * (t1 - 0.5) + cloud_lobe_pitch(seed ^ 0x9e37_79b9, t1, thickness);
        let fade0 = cloud_band_alpha(seed, t0, density_t, alpha_scale);
        let fade1 = cloud_band_alpha(seed, t1, density_t, alpha_scale);
        if fade0 <= 0.015 && fade1 <= 0.015 {
            continue;
        }
        let width0 = cloud_band_width(seed, t0);
        let width1 = cloud_band_width(seed, t1);
        let segment_thickness = thickness * ((width0 + width1) * 0.5);
        push_wrapped_cloud_ribbon_cyclorama(
            out,
            sky,
            yaw0,
            yaw1,
            pitch0 - segment_thickness * 0.18,
            pitch1 - segment_thickness * 0.18,
            segment_thickness * 1.42,
            shadow,
            fade0 * 78.0,
            fade1 * 78.0,
            horizon_pitch,
            top_pitch,
            bottom_pitch,
        );
        push_wrapped_cloud_ribbon_cyclorama(
            out,
            sky,
            yaw0,
            yaw1,
            pitch0,
            pitch1,
            segment_thickness * 0.84,
            body,
            fade0 * 154.0,
            fade1 * 154.0,
            horizon_pitch,
            top_pitch,
            bottom_pitch,
        );
        push_wrapped_cloud_ribbon_cyclorama(
            out,
            sky,
            yaw0,
            yaw1,
            pitch0 + segment_thickness * 0.18,
            pitch1 + segment_thickness * 0.18,
            segment_thickness * 0.2,
            warm,
            fade0 * 235.0,
            fade1 * 235.0,
            horizon_pitch,
            top_pitch,
            bottom_pitch,
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn push_wrapped_cloud_ribbon_cyclorama(
    out: &mut Vec<SkyCycloramaQuad>,
    sky: ResolvedSkySettings,
    yaw0: f32,
    yaw1: f32,
    pitch0: f32,
    pitch1: f32,
    half_thickness: f32,
    tint: [u8; 3],
    alpha0: f32,
    alpha1: f32,
    horizon_pitch: f32,
    top_pitch: f32,
    bottom_pitch: f32,
) {
    let mut start = yaw0;
    let mut end = yaw1;
    while start < -180.0 {
        start += 360.0;
        end += 360.0;
    }
    while start >= 180.0 {
        start -= 360.0;
        end -= 360.0;
    }
    if end <= 180.0 {
        push_cloud_ribbon_cyclorama(
            out,
            sky,
            start,
            end,
            pitch0,
            pitch1,
            half_thickness,
            tint,
            alpha0,
            alpha1,
            horizon_pitch,
            top_pitch,
            bottom_pitch,
        );
        return;
    }

    let t = ((180.0 - start) / (end - start).max(0.001)).clamp(0.0, 1.0);
    let split_pitch = lerp_f32(pitch0, pitch1, t);
    let split_alpha = lerp_f32(alpha0, alpha1, t);
    push_cloud_ribbon_cyclorama(
        out,
        sky,
        start,
        180.0,
        pitch0,
        split_pitch,
        half_thickness,
        tint,
        alpha0,
        split_alpha,
        horizon_pitch,
        top_pitch,
        bottom_pitch,
    );
    push_cloud_ribbon_cyclorama(
        out,
        sky,
        -180.0,
        end - 360.0,
        split_pitch,
        pitch1,
        half_thickness,
        tint,
        split_alpha,
        alpha1,
        horizon_pitch,
        top_pitch,
        bottom_pitch,
    );
}

pub(crate) fn push_cloud_ribbon_cyclorama(
    out: &mut Vec<SkyCycloramaQuad>,
    sky: ResolvedSkySettings,
    yaw0: f32,
    yaw1: f32,
    pitch0: f32,
    pitch1: f32,
    half_thickness: f32,
    tint: [u8; 3],
    alpha0: f32,
    alpha1: f32,
    horizon_pitch: f32,
    top_pitch: f32,
    bottom_pitch: f32,
) {
    let width0 = cloud_width_fade(alpha0);
    let width1 = cloud_width_fade(alpha1);
    let top0 = pitch0 + half_thickness * width0;
    let top1 = pitch1 + half_thickness * width1;
    let bottom0 = pitch0 - half_thickness * width0;
    let bottom1 = pitch1 - half_thickness * width1;
    let center0 = pitch0;
    let center1 = pitch1;
    let base_top0 =
        sky_color_for_pitch_yaw_core(sky, top0, yaw0, horizon_pitch, top_pitch, bottom_pitch);
    let base_top1 =
        sky_color_for_pitch_yaw_core(sky, top1, yaw1, horizon_pitch, top_pitch, bottom_pitch);
    let base_center0 =
        sky_color_for_pitch_yaw_core(sky, center0, yaw0, horizon_pitch, top_pitch, bottom_pitch);
    let base_center1 =
        sky_color_for_pitch_yaw_core(sky, center1, yaw1, horizon_pitch, top_pitch, bottom_pitch);
    let base_bottom0 =
        sky_color_for_pitch_yaw_core(sky, bottom0, yaw0, horizon_pitch, top_pitch, bottom_pitch);
    let base_bottom1 =
        sky_color_for_pitch_yaw_core(sky, bottom1, yaw1, horizon_pitch, top_pitch, bottom_pitch);
    let center_tint0 = cyclorama_lerp_rgb(base_center0, tint, alpha0.clamp(0.0, 255.0) as u8);
    let center_tint1 = cyclorama_lerp_rgb(base_center1, tint, alpha1.clamp(0.0, 255.0) as u8);
    push_sky_cyclorama_quad_corners(
        out,
        yaw0,
        yaw1,
        top0,
        top1,
        center0,
        center1,
        [base_top0, base_top1, center_tint0, center_tint1],
    );
    push_sky_cyclorama_quad_corners(
        out,
        yaw0,
        yaw1,
        center0,
        center1,
        bottom0,
        bottom1,
        [center_tint0, center_tint1, base_bottom0, base_bottom1],
    );
}

pub(crate) fn push_sky_cyclorama_quad(
    out: &mut Vec<SkyCycloramaQuad>,
    yaw0: f32,
    yaw1: f32,
    pitch_top: f32,
    pitch_bottom: f32,
    rgb: [[u8; 3]; 4],
) {
    push_sky_cyclorama_quad_corners(
        out,
        yaw0,
        yaw1,
        pitch_top,
        pitch_top,
        pitch_bottom,
        pitch_bottom,
        rgb,
    );
}

pub(crate) fn push_sky_cyclorama_quad_corners(
    out: &mut Vec<SkyCycloramaQuad>,
    yaw0: f32,
    yaw1: f32,
    pitch_top0: f32,
    pitch_top1: f32,
    pitch_bottom0: f32,
    pitch_bottom1: f32,
    rgb: [[u8; 3]; 4],
) {
    if out.len() >= SKY_CYCLORAMA_QUAD_MAX {
        return;
    }
    out.push(SkyCycloramaQuad {
        direction_q12: [
            cyclorama_direction_q12(yaw0, pitch_top0),
            cyclorama_direction_q12(yaw1, pitch_top1),
            cyclorama_direction_q12(yaw0, pitch_bottom0),
            cyclorama_direction_q12(yaw1, pitch_bottom1),
        ],
        rgb,
    });
}

pub(crate) fn push_sky_cyclorama_triangle(
    out: &mut Vec<SkyCycloramaQuad>,
    points: [(f32, f32); 3],
    rgb: [[u8; 3]; 3],
) {
    if out.len() >= SKY_CYCLORAMA_QUAD_MAX {
        return;
    }
    out.push(SkyCycloramaQuad {
        direction_q12: [
            cyclorama_direction_q12(points[0].0, points[0].1),
            cyclorama_direction_q12(points[1].0, points[1].1),
            cyclorama_direction_q12(points[2].0, points[2].1),
            cyclorama_direction_q12(points[2].0, points[2].1),
        ],
        rgb: [rgb[0], rgb[1], rgb[2], rgb[2]],
    });
}

pub(crate) fn cyclorama_direction_q12(yaw_degrees: f32, pitch_degrees: f32) -> [i16; 3] {
    let yaw = yaw_degrees.to_radians();
    let pitch = pitch_degrees.clamp(-82.0, 82.0).to_radians();
    let cp = pitch.cos();
    let scale = 4096.0;
    [
        (-yaw.sin() * cp * scale).round() as i16,
        (pitch.sin() * scale).round() as i16,
        (-yaw.cos() * cp * scale).round() as i16,
    ]
}

pub(crate) fn sky_horizon_pitch_degrees(horizon_percent: u8) -> f32 {
    let y = 120.0 - 240.0 * (horizon_percent.clamp(5, 95) as f32 / 100.0);
    (y / 320.0).atan().to_degrees()
}

pub(crate) fn sky_color_for_pitch(
    sky: ResolvedSkySettings,
    pitch: f32,
    horizon_pitch: f32,
    top_pitch: f32,
    bottom_pitch: f32,
) -> [u8; 3] {
    let base = if pitch >= horizon_pitch {
        let span = (top_pitch - horizon_pitch).max(1.0);
        let t = smooth_step(((pitch - horizon_pitch) / span).clamp(0.0, 1.0));
        cyclorama_lerp_rgb(sky.horizon_color, sky.top_color, (t * 255.0) as u8)
    } else {
        let span = (horizon_pitch - bottom_pitch).max(1.0);
        let t = smooth_step(((horizon_pitch - pitch) / span).clamp(0.0, 1.0));
        cyclorama_lerp_rgb(sky.horizon_color, sky.lower_color, (t * 255.0) as u8)
    };
    let hold_radius = 1.4 + sky.horizon_thickness_percent.clamp(0, 80) as f32 * 0.13;
    let hold = smooth_falloff(hold_radius, (pitch - horizon_pitch).abs());
    cyclorama_lerp_rgb(
        base,
        sky.horizon_color,
        (hold * 92.0).clamp(0.0, 255.0) as u8,
    )
}

pub(crate) fn sky_color_for_pitch_yaw(
    sky: ResolvedSkySettings,
    pitch: f32,
    yaw: f32,
    horizon_pitch: f32,
    top_pitch: f32,
    bottom_pitch: f32,
) -> [u8; 3] {
    let color =
        sky_color_for_pitch_yaw_core(sky, pitch, yaw, horizon_pitch, top_pitch, bottom_pitch);
    sky_cloud_wash_color(sky, color, pitch, yaw, horizon_pitch)
}

pub(crate) fn sky_color_for_pitch_yaw_core(
    sky: ResolvedSkySettings,
    pitch: f32,
    yaw: f32,
    horizon_pitch: f32,
    top_pitch: f32,
    bottom_pitch: f32,
) -> [u8; 3] {
    let base = sky_color_for_pitch(sky, pitch, horizon_pitch, top_pitch, bottom_pitch);
    let mut color = base;
    let pitch_delta = (pitch - horizon_pitch).abs();
    let pitch_weight = smooth_falloff(27.0, pitch_delta);
    if sky.horizon_glow_percent > 0 && pitch_weight > 0.0 {
        let yaw_delta = angular_distance_degrees(yaw, sky.horizon_glow_yaw_degrees as f32);
        let yaw_weight = smooth_falloff(105.0, yaw_delta);
        let strength =
            (sky.horizon_glow_percent.clamp(0, 100) as f32 / 100.0) * pitch_weight * yaw_weight;
        if strength > 0.0 {
            color = cyclorama_lerp_rgb(
                color,
                horizon_glow_color_for_yaw(sky, yaw),
                (strength * 156.0).clamp(0.0, 255.0) as u8,
            );
        }
    }
    color
}

pub(crate) fn horizon_glow_color_for_yaw(sky: ResolvedSkySettings, yaw: f32) -> [u8; 3] {
    let yaw_delta = angular_distance_degrees(yaw, sky.horizon_glow_yaw_degrees as f32);
    let hot = smooth_falloff(42.0, yaw_delta);
    let warm = cyclorama_lerp_rgb(sky.horizon_color, [255, 174, 94], 188);
    let pink = cyclorama_lerp_rgb(sky.horizon_color, [226, 118, 172], 132);
    brighten_rgb(cyclorama_lerp_rgb(pink, warm, (hot * 255.0) as u8), 10)
}

pub(crate) fn sky_cloud_wash_color(
    sky: ResolvedSkySettings,
    base: [u8; 3],
    pitch: f32,
    yaw: f32,
    horizon_pitch: f32,
) -> [u8; 3] {
    let cloud = sky.cloud_layer;
    if !cloud.enabled || cloud.density == 0 {
        return base;
    }
    let altitude_t = (cloud.altitude as f32 / u16::MAX as f32).clamp(0.0, 1.0);
    let extent_t = (cloud.extent as f32 / u16::MAX as f32).clamp(0.0, 1.0);
    let tile_count = cloud.tile_count.clamp(1, 16) as f32;
    let density_t = cloud.density as f32 / 255.0;
    let center = horizon_pitch + 4.0 + altitude_t * 28.0 + cloud_band_wave(cloud.noise_seed, yaw);
    let width = 8.0 + extent_t * 16.0;
    let pitch_weight = smooth_falloff(width, (pitch - center).abs());
    if pitch_weight <= 0.0 {
        return base;
    }
    let phase = (cloud.noise_seed & 0xff) as f32 * 0.037;
    let yaw_r = yaw.to_radians();
    let yaw_weight = 0.58
        + 0.24 * (yaw_r * (tile_count * 0.38) + phase).sin()
        + 0.18 * (yaw_r * (tile_count * 0.71) + phase * 1.7).sin();
    let strength = (density_t * pitch_weight * yaw_weight.clamp(0.18, 1.0)).clamp(0.0, 1.0);
    let tint = cyclorama_lerp_rgb(cloud.color, [255, 180, 148], (strength * 96.0) as u8);
    cyclorama_lerp_rgb(base, tint, (strength * 34.0).clamp(0.0, 255.0) as u8)
}

pub(crate) fn cyclorama_yaw_for_column(column: usize, columns: usize) -> f32 {
    -180.0 + 360.0 * (column as f32 / columns.max(1) as f32)
}

pub(crate) fn angular_distance_degrees(a: f32, b: f32) -> f32 {
    let mut d = (a - b).abs() % 360.0;
    if d > 180.0 {
        d = 360.0 - d;
    }
    d
}

pub(crate) fn smooth_falloff(radius: f32, distance: f32) -> f32 {
    let t = (1.0 - distance / radius.max(0.001)).clamp(0.0, 1.0);
    smooth_step(t)
}

pub(crate) fn smooth_step(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

pub(crate) fn cloud_width_fade(alpha: f32) -> f32 {
    (alpha / 150.0).clamp(0.0, 1.0).sqrt()
}

pub(crate) fn cloud_density_response(density: u8) -> f32 {
    (density as f32 / 255.0).clamp(0.0, 1.0).powf(0.58)
}

pub(crate) fn mountain_profile(seed: u32, yaw_degrees: f32, roughness: f32) -> f32 {
    let roughness = roughness.clamp(0.0, 1.0);
    let spacing = lerp_f32(68.0, 34.0, roughness);
    let phase = (seed & 0xff) as f32 * 0.17;
    let x = (yaw_degrees + 540.0 + phase) / spacing;
    let broad = mountain_value_noise(seed ^ 0x52dc_e729, x * 0.62);
    let mid = mountain_value_noise(seed ^ 0x9e37_79b9, x * (1.12 + roughness * 0.45));
    let fine = mountain_value_noise(seed ^ 0x85eb_ca6b, x * (2.35 + roughness * 1.3));
    let wave = 0.5 + 0.5 * ((yaw_degrees.to_radians() * 1.22) + phase * 0.09).sin();
    let ridge =
        broad * 0.5 + mid * (0.34 + roughness * 0.08) + fine * (roughness * 0.12) + wave * 0.04;
    smooth_step(((ridge - 0.18) / 0.82).clamp(0.0, 1.0)).powf(lerp_f32(1.0, 0.82, roughness))
}

pub(crate) fn mountain_value_noise(seed: u32, x: f32) -> f32 {
    let cell = x.floor() as i32;
    let t = smooth_step(x - cell as f32);
    let a = sky_hash_unit(seed, cell as u32);
    let b = sky_hash_unit(seed, cell.wrapping_add(1) as u32);
    lerp_f32(a, b, t)
}

pub(crate) fn cloud_streak_fade(t: f32) -> f32 {
    (core::f32::consts::PI * t).sin().clamp(0.0, 1.0)
}

pub(crate) fn cloud_lobe_weight(seed: u32, t: f32) -> f32 {
    let phase0 = (seed & 0xff) as f32 * 0.037;
    let phase1 = ((seed >> 8) & 0xff) as f32 * 0.029;
    let a = (core::f32::consts::TAU * (t * 2.0 + phase0)).sin();
    let b = (core::f32::consts::TAU * (t * 3.0 + phase1)).sin();
    (0.62 + 0.25 * a + 0.13 * b).clamp(0.18, 1.0)
}

pub(crate) fn cloud_lobe_pitch(seed: u32, t: f32, thickness: f32) -> f32 {
    let phase = ((seed >> 16) & 0xff) as f32 * 0.041;
    (core::f32::consts::TAU * (t * 1.5 + phase)).sin() * thickness * 0.36
}

pub(crate) fn cloud_band_alpha(seed: u32, t: f32, density_t: f32, alpha_scale: f32) -> f32 {
    cloud_streak_fade(t).powf(0.58)
        * cloud_lobe_weight(seed ^ 0x1b56_c4e9, t)
        * density_t
        * alpha_scale
}

pub(crate) fn cloud_band_width(seed: u32, t: f32) -> f32 {
    let phase0 = (seed & 0xff) as f32 * 0.023;
    let phase1 = ((seed >> 8) & 0xff) as f32 * 0.031;
    let a = (core::f32::consts::TAU * (t * 2.0 + phase0)).sin();
    let b = (core::f32::consts::TAU * (t * 4.0 + phase1)).sin();
    (0.72 + 0.2 * a + 0.08 * b).clamp(0.46, 1.18)
}

pub(crate) fn cloud_band_wave(seed: u32, yaw_degrees: f32) -> f32 {
    let r = yaw_degrees.to_radians();
    let phase0 = (seed & 0xff) as f32 * 0.019;
    let phase1 = ((seed >> 8) & 0xff) as f32 * 0.023;
    (r * 2.0 + phase0).sin() * 1.6 + (r * 5.0 + phase1).sin() * 0.72
}

pub(crate) fn sky_hash_u32(seed: u32, value: u32) -> u32 {
    let mut h = seed ^ value.wrapping_mul(0x9e37_79b9);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^ (h >> 16)
}

pub(crate) fn sky_hash_unit(seed: u32, value: u32) -> f32 {
    (sky_hash_u32(seed, value) >> 8) as f32 / 16_777_215.0
}

pub(crate) fn lerp_f32(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

pub(crate) fn cyclorama_lerp_rgb(a: [u8; 3], b: [u8; 3], t: u8) -> [u8; 3] {
    let inv = 255 - t as u16;
    let t = t as u16;
    [
        ((a[0] as u16 * inv + b[0] as u16 * t) / 255) as u8,
        ((a[1] as u16 * inv + b[1] as u16 * t) / 255) as u8,
        ((a[2] as u16 * inv + b[2] as u16 * t) / 255) as u8,
    ]
}

pub(crate) fn rgb_luma(rgb: [u8; 3]) -> f32 {
    rgb[0] as f32 * 0.2126 + rgb[1] as f32 * 0.7152 + rgb[2] as f32 * 0.0722
}

pub(crate) fn brighten_rgb(rgb: [u8; 3], amount: u8) -> [u8; 3] {
    [
        rgb[0].saturating_add(amount),
        rgb[1].saturating_add(amount),
        rgb[2].saturating_add(amount),
    ]
}

pub(crate) fn blend_rgb(a: [u8; 3], b: [u8; 3], b_weight_256: u16) -> [u8; 3] {
    let weight = b_weight_256.min(256);
    let inv = 256 - weight;
    [
        (((a[0] as u16 * inv) + (b[0] as u16 * weight)) >> 8) as u8,
        (((a[1] as u16 * inv) + (b[1] as u16 * weight)) >> 8) as u8,
        (((a[2] as u16 * inv) + (b[2] as u16 * weight)) >> 8) as u8,
    ]
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
    /// Minimum camera origin height above the sampled floor.
    #[serde(default = "default_world_camera_min_floor_clearance")]
    pub min_floor_clearance: i32,
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
            min_floor_clearance: self
                .min_floor_clearance
                .clamp(0, MAX_WORLD_CAMERA_MIN_FLOOR_CLEARANCE),
        }
    }
}

impl Default for WorldCameraSettings {
    fn default() -> Self {
        Self {
            distance: default_world_camera_distance(),
            height: default_world_camera_height(),
            target_height: default_world_camera_target_height(),
            min_floor_clearance: default_world_camera_min_floor_clearance(),
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
    /// Tomb-Raider-style stacked rooms. This is the room's `Y`
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
        let mut floor = WorldGrid::empty(self.width, self.depth, self.sector_size);
        floor.origin = self.origin;
        floor.elevation = elevation;
        // Inherit the room-level look so stacked floors render
        // consistently (and the sky/fog seed grid is unaffected by which
        // floor is active).
        floor.ambient_color = self.ambient_color;
        floor.fog_enabled = self.fog_enabled;
        floor.fog_color = self.fog_color;
        floor.fog_near = self.fog_near;
        floor.fog_far = self.fog_far;
        floor.atmosphere_enabled = self.atmosphere_enabled;
        floor.atmosphere_color = self.atmosphere_color;
        floor.atmosphere_density = self.atmosphere_density;
        floor.atmosphere_fall_speed_q4 = self.atmosphere_fall_speed_q4;
        floor.atmosphere_wind_speed_q4 = self.atmosphere_wind_speed_q4;
        self.floors_above.push(floor);
        self.floors_above.len()
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
        found.then_some(WorldGridFootprint {
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
            return;
        }
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
