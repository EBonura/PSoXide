use super::*;

/// World sky rendering mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SkyMode {
    /// Disable authored sky rendering. The renderer clears to
    /// [`SkySettings::lower_color`] only.
    Off,
    /// Draw a cooked cyclorama before world geometry.
    #[default]
    Gradient,
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
        for (sum, component) in sums.iter_mut().zip(color.rgb) {
            *sum += u64::from(component) * count;
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
