//! Sky cyclorama and far-vista backdrop policy, carved out of
//! `editor-playtest`'s `sky_runtime` module (phase 2 of
//! docs/game-runtime-plan.md). [`SkyCyclorama`] owns the
//! rotation-keyed packet cache the example previously kept in
//! `static mut`s; cooked records arrive as psx-level values, the OT
//! depth arrives as a `const N` generic, screen/focal knobs arrive as
//! plain values, and the example's VRAM slot resolvers arrive as
//! closures until their glue fully migrates.

use psx_engine::{
    Angle, CullMode, DepthPolicy, OtFrame, PrimitiveSink, SkyDirectionProjector, WorldCamera,
    WorldRenderPass, WorldSurfaceOptions, WorldVertex,
};
use psx_gpu::{
    material::TextureMaterial,
    prim::{QuadTexturedMaterial, TriTextured},
};
use psx_level::{
    far_vista_flags, find_asset_of_kind, sky_flags, AssetId, AssetKind, LevelAssetRecord,
    LevelFarVistaRecord, LevelSkyRecord,
};
use psx_math::int32::{clamp_i16, mul_q12_i32};

use crate::room_lighting::rgb_tuple;
use crate::vram::{
    vram_slot_texture_size_u8, VramSlot, VramSlotClutMode, SKY_PANORAMA_HEIGHT,
    SKY_PANORAMA_PALETTE_BANDS, SKY_PANORAMA_WIDTH,
};

/// Cooked sky panoramas occupy two side-by-side 4bpp pages. The
/// texture pixels are outside the double-buffered framebuffer and
/// model-atlas upload regions; each horizontal band gets a dedicated
/// CLUT row so the sky can spend 16 colours per altitude range. The
/// panorama dimensions are the crate vram module's contract.
const SKY_PANORAMA_PAGE_WIDTH: u16 = 256;
const SKY_CYCLORAMA_GRID_POINTS_MAX: usize =
    (SKY_CYCLORAMA_COLUMNS_MAX as usize + 1) * (SKY_PANORAMA_PALETTE_BANDS + 1);
const SKY_CYCLORAMA_COLUMNS_MIN: u8 = 8;
const SKY_CYCLORAMA_COLUMNS_MAX: u8 = 12;

/// Rotation-keyed cache of the cyclorama's finished GP0 packets.
///
/// The whole sky draw is a pure function of the camera ROTATION (the
/// dome is camera-centred, translation is ignored) plus the sky record,
/// so on every frame where the camera does not turn the ~96 packets are
/// bit-identical to the previous frame. Cache them and only
/// relink into the OT; rebuild on an exact rotation/record key change.
/// Safe for the same reason the prebuilt room-quad pool is: the previous
/// frame's OT DMA is drained at the present flip before the next visual
/// frame relinks these packets.
const SKY_CACHE_CAP: usize = SKY_CYCLORAMA_COLUMNS_MAX as usize * SKY_PANORAMA_PALETTE_BANDS;
const SKY_CACHE_PACKET_EMPTY: QuadTexturedMaterial = QuadTexturedMaterial {
    tag: 0,
    tex_window: 0,
    color_cmd: 0,
    v0: 0,
    uv0_clut: 0,
    v1: 0,
    uv1_tpage: 0,
    v2: 0,
    uv2: 0,
    v3: 0,
    uv3: 0,
};
#[derive(Copy, Clone, PartialEq, Eq)]
struct SkyCacheKey {
    sin_yaw: i32,
    cos_yaw: i32,
    sin_pitch: i32,
    cos_pitch: i32,
    texture_asset: u16,
    flags: u32,
    columns: u8,
    rows: u8,
    horizon_percent: u8,
}

/// Owned sky-cyclorama state: the rotation-keyed finished-packet cache
/// (formerly the example's `SKY_CACHE_*` statics). The game keeps one
/// instance in its runtime arenas; the zeroed state is an invalid key,
/// so the first draw always rebuilds.
pub struct SkyCyclorama {
    packets: [QuadTexturedMaterial; SKY_CACHE_CAP],
    count: usize,
    valid: bool,
    key: SkyCacheKey,
}

impl SkyCyclorama {
    /// All-zero state (link-time `.bss`-safe); the cache reports
    /// invalid, so the first draw rebuilds it.
    pub const fn zeroed() -> Self {
        Self {
            packets: [SKY_CACHE_PACKET_EMPTY; SKY_CACHE_CAP],
            count: 0,
            valid: false,
            key: SkyCacheKey {
                sin_yaw: 0,
                cos_yaw: 0,
                sin_pitch: 0,
                cos_pitch: 0,
                texture_asset: 0,
                flags: 0,
                columns: 0,
                rows: 0,
                horizon_percent: 0,
            },
        }
    }

    /// Draw the cooked sky panorama over a camera-centred dome into the
    /// OT's farthest slot (`OT_DEPTH - 1`). Callers whose world renderer can
    /// also use that slot must insert the sky afterward: OT prepend order then
    /// executes the sky first and keeps same-slot world packets in front.
    /// Streamed sky assets carry empty baked bytes and are uploaded on gameplay
    /// entry, so residency resolves through the caller's slot resolvers; baked
    /// builds upload lazily.
    pub fn draw_panorama<const OT_DEPTH: usize>(
        &mut self,
        sky: LevelSkyRecord,
        camera: WorldCamera,
        assets: &'static [LevelAssetRecord],
        screen_w: i16,
        screen_h: i16,
        focal: i32,
        find_sky_panorama_vram_slot: impl Fn(AssetId) -> Option<VramSlot>,
        ensure_sky_panorama_uploaded: impl FnMut(AssetId, &[u8]) -> Option<VramSlot>,
        sky_panorama_tpage_word: impl Fn(usize) -> u16,
        sky_panorama_clut_word: impl Fn(usize) -> u16,
        ot: &mut OtFrame<'_, OT_DEPTH>,
    ) {
        // The ordinary world band stops one slot earlier. Renderers that also
        // use this farthest slot preserve the background contract through the
        // caller's insertion order documented above.
        let sky_ot_slot = psx_engine::DepthSlot::new(OT_DEPTH - 1);
        if sky.flags & sky_flags::ENABLED == 0 {
            return;
        }
        let Some(asset) =
            find_asset_of_kind(assets, sky.cloud_layer.texture_asset, AssetKind::Texture)
        else {
            return;
        };
        // Streamed sky assets carry empty baked bytes; they are uploaded on gameplay
        // entry (`load_streamed_sky_from_cd`), so resolve the existing VRAM slot
        // rather than re-parsing empty bytes. Baked builds upload lazily here.
        if !sky_panorama_resident(
            asset,
            find_sky_panorama_vram_slot,
            ensure_sky_panorama_uploaded,
        ) {
            return;
        }

        let key = SkyCacheKey {
            sin_yaw: camera.sin_yaw.raw(),
            cos_yaw: camera.cos_yaw.raw(),
            sin_pitch: camera.sin_pitch.raw(),
            cos_pitch: camera.cos_pitch.raw(),
            texture_asset: sky.cloud_layer.texture_asset.0,
            flags: sky.flags as u32,
            columns: sky.skybox_columns,
            rows: sky.skybox_rows,
            horizon_percent: sky.horizon_percent,
        };
        if !self.valid || self.key != key {
            self.count = build_sky_panorama_packets(
                sky,
                camera,
                screen_w,
                screen_h,
                focal,
                &sky_panorama_tpage_word,
                &sky_panorama_clut_word,
                &mut self.packets,
            );
            self.key = key;
            self.valid = true;
        }
        let mut i = 0usize;
        while i < self.count {
            ot.add_packet_slot(sky_ot_slot, &mut self.packets[i]);
            i += 1;
        }
    }
}

fn build_sky_panorama_packets(
    sky: LevelSkyRecord,
    camera: WorldCamera,
    screen_w: i16,
    screen_h: i16,
    focal: i32,
    sky_panorama_tpage_word: impl Fn(usize) -> u16,
    sky_panorama_clut_word: impl Fn(usize) -> u16,
    out: &mut [QuadTexturedMaterial; SKY_CACHE_CAP],
) -> usize {
    let mut columns = sky
        .skybox_columns
        .clamp(SKY_CYCLORAMA_COLUMNS_MIN, SKY_CYCLORAMA_COLUMNS_MAX) as usize;
    if !columns.is_multiple_of(2) {
        columns += 1;
    }
    let rows = sky_panorama_runtime_rows(sky);
    let horizon_pitch = sky_horizon_pitch_degrees_i32(sky.horizon_percent, focal);
    let top_pitch = (horizon_pitch + 58).min(78);
    let bottom_pitch = (horizon_pitch - 46).max(-72);
    let mut projected_grid: [Option<(i16, i16)>; SKY_CYCLORAMA_GRID_POINTS_MAX] =
        [None; SKY_CYCLORAMA_GRID_POINTS_MAX];
    let grid_stride = columns + 1;

    // Project the whole grid on the GTE: load the camera rotation once, then
    // RTPS each direction (hardware rotate + perspective divide) instead of the
    // per-direction CPU rotate (eight muls) and two divides.
    let sky_projector = SkyDirectionProjector::load(camera);
    // Yaw depends only on column and pitch only on row, so precompute the
    // sin/cos of each once instead of four trig lookups per grid point.
    let mut yaw_sin = [0i32; SKY_CYCLORAMA_COLUMNS_MAX as usize + 1];
    let mut yaw_cos = [0i32; SKY_CYCLORAMA_COLUMNS_MAX as usize + 1];
    for column in 0..=columns {
        let yaw = angle_from_degrees_i32(sky_yaw_degrees_for_column(column, columns));
        yaw_sin[column] = yaw.sin().raw();
        yaw_cos[column] = yaw.cos().raw();
    }
    let mut pitch_sin = [0i32; SKY_PANORAMA_PALETTE_BANDS + 1];
    let mut pitch_cos = [0i32; SKY_PANORAMA_PALETTE_BANDS + 1];
    for row in 0..=rows {
        let pitch =
            angle_from_degrees_i32(sky_lerp_i32(top_pitch, bottom_pitch, row, rows).clamp(-82, 82));
        pitch_sin[row] = pitch.sin().raw();
        pitch_cos[row] = pitch.cos().raw();
    }
    for row in 0..=rows {
        let row_base = row * grid_stride;
        for column in 0..=columns {
            let dir = [
                clamp_i16(-mul_q12_i32(yaw_sin[column], pitch_cos[row])),
                clamp_i16(pitch_sin[row]),
                clamp_i16(-mul_q12_i32(yaw_cos[column], pitch_cos[row])),
            ];
            projected_grid[row_base + column] = sky_projector
                .project(dir)
                .map(|(sx, sy)| (sx.clamp(-512, 831), sy.clamp(-256, 495)));
        }
    }

    let mut column_tpage_word = [0u16; SKY_CYCLORAMA_COLUMNS_MAX as usize];
    let mut column_u0 = [0u8; SKY_CYCLORAMA_COLUMNS_MAX as usize];
    let mut column_u1 = [0u8; SKY_CYCLORAMA_COLUMNS_MAX as usize];
    for column in 0..columns {
        let page = sky_panorama_page_for_column(column, columns);
        column_tpage_word[column] = sky_panorama_tpage_word(page);
        column_u0[column] = sky_panorama_local_u(
            sky_coord_for_step(column, columns, SKY_PANORAMA_WIDTH),
            page,
        );
        column_u1[column] = sky_panorama_local_u(
            sky_coord_for_step(column + 1, columns, SKY_PANORAMA_WIDTH),
            page,
        );
    }

    let mut count = 0usize;
    for row in 0..rows {
        let row_base = row * grid_stride;
        let next_row_base = row_base + grid_stride;
        let v0 = sky_uv_for_step(row, rows, SKY_PANORAMA_HEIGHT);
        let v1 = sky_uv_for_step(row + 1, rows, SKY_PANORAMA_HEIGHT);
        let clut_word = sky_panorama_clut_word(sky_panorama_clut_band_for_row(row, rows));
        for column in 0..columns {
            let material =
                TextureMaterial::opaque(clut_word, column_tpage_word[column], (0x80, 0x80, 0x80))
                    .with_raw_texture(true)
                    .with_dither(true);
            let Some(p0) = projected_grid[row_base + column] else {
                continue;
            };
            let Some(p1) = projected_grid[row_base + column + 1] else {
                continue;
            };
            let Some(p2) = projected_grid[next_row_base + column] else {
                continue;
            };
            let Some(p3) = projected_grid[next_row_base + column + 1] else {
                continue;
            };
            let projected = [p0, p1, p2, p3];
            if sky_quad_outside_screen(projected, screen_w, screen_h) {
                continue;
            }
            if count >= out.len() {
                break;
            }
            // Same GP0 words as the old arena-pushed quad; the cyclorama
            // packets now live in the rotation-keyed owned cache and are
            // relinked into the OT background slot each frame.
            out[count] = QuadTexturedMaterial::with_material(
                projected,
                [
                    (column_u0[column], v0),
                    (column_u1[column], v0),
                    (column_u0[column], v1),
                    (column_u1[column], v1),
                ],
                material,
            );
            count += 1;
        }
    }
    count
}

fn sky_quad_outside_screen(points: [(i16, i16); 4], screen_w: i16, screen_h: i16) -> bool {
    let min_x = points[0]
        .0
        .min(points[1].0)
        .min(points[2].0)
        .min(points[3].0);
    let max_x = points[0]
        .0
        .max(points[1].0)
        .max(points[2].0)
        .max(points[3].0);
    let min_y = points[0]
        .1
        .min(points[1].1)
        .min(points[2].1)
        .min(points[3].1);
    let max_y = points[0]
        .1
        .max(points[1].1)
        .max(points[2].1)
        .max(points[3].1);
    max_x < 0 || min_x >= screen_w || max_y < 0 || min_y >= screen_h
}

fn angle_from_degrees_i32(degrees: i32) -> Angle {
    Angle::from_q12(((degrees.saturating_mul(4096) / 360) & 0x0fff) as u16)
}

fn sky_horizon_pitch_degrees_i32(horizon_percent: u8, focal: i32) -> i32 {
    let y = 120 - 240 * i32::from(horizon_percent.clamp(5, 95)) / 100;
    y.saturating_mul(57) / focal
}

fn sky_yaw_degrees_for_column(column: usize, columns: usize) -> i32 {
    -180 + (360 * column as i32) / columns.max(1) as i32
}

fn sky_lerp_i32(a: i32, b: i32, index: usize, count: usize) -> i32 {
    let count = count.max(1) as i32;
    a + (b - a) * index as i32 / count
}

fn sky_coord_for_step(step: usize, steps: usize, size: u16) -> u16 {
    if step >= steps {
        return size.saturating_sub(1);
    }
    ((step as u32 * u32::from(size)) / steps.max(1) as u32).min(u32::from(size - 1)) as u16
}

fn sky_uv_for_step(step: usize, steps: usize, size: u16) -> u8 {
    sky_coord_for_step(step, steps, size).min(255) as u8
}

fn sky_panorama_runtime_rows(sky: LevelSkyRecord) -> usize {
    sky.skybox_rows.clamp(1, SKY_PANORAMA_PALETTE_BANDS as u8) as usize
}

fn sky_panorama_clut_band_for_row(row: usize, rows: usize) -> usize {
    let rows = rows.max(1);
    ((row.saturating_mul(2).saturating_add(1)) * SKY_PANORAMA_PALETTE_BANDS / (rows * 2))
        .min(SKY_PANORAMA_PALETTE_BANDS - 1)
}

fn sky_panorama_page_for_column(column: usize, columns: usize) -> usize {
    if column < columns / 2 {
        0
    } else {
        1
    }
}

fn sky_panorama_local_u(global_u: u16, page: usize) -> u8 {
    let page_u = if page == 0 {
        global_u.min(SKY_PANORAMA_PAGE_WIDTH - 1)
    } else {
        global_u
            .saturating_sub(SKY_PANORAMA_PAGE_WIDTH)
            .min(SKY_PANORAMA_PAGE_WIDTH - 1)
    };
    page_u as u8
}

/// Draw the authored far-vista panel ring: a camera-centred cylinder of
/// textured quads at the farthest depth slot behind world geometry.
pub fn draw_far_vista_ring<const OT_DEPTH: usize>(
    camera: WorldCamera,
    vista: LevelFarVistaRecord,
    options: WorldSurfaceOptions,
    assets: &'static [LevelAssetRecord],
    mut ensure_texture_uploaded_with_clut_mode: impl FnMut(
        AssetId,
        &[u8],
        VramSlotClutMode,
    ) -> Option<VramSlot>,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    if vista.flags & far_vista_flags::ENABLED == 0 {
        return;
    }
    let segments = vista.segments.clamp(3, 16);
    let radius = vista.radius.max(1_024);
    let y0 = camera.position.y.saturating_add(vista.vertical_offset);
    let y1 = y0.saturating_add(vista.height.max(128));
    let step = 0x1_0000_u32 / segments as u32;
    let base = angle_from_signed_degrees(vista.rotation_degrees);

    for segment in 0..segments {
        let a0 = base.add(Angle::from_raw_q16(segment as u16 * step as u16));
        let a1 = base.add(Angle::from_raw_q16(
            (segment as u16).wrapping_add(1).wrapping_mul(step as u16),
        ));
        let x0 = camera.position.x.saturating_add(a0.sin().mul_i32(radius));
        let z0 = camera.position.z.saturating_add(a0.cos().mul_i32(radius));
        let x1 = camera.position.x.saturating_add(a1.sin().mul_i32(radius));
        let z1 = camera.position.z.saturating_add(a1.cos().mul_i32(radius));
        let material = far_vista_texture_material(
            far_vista_panel_asset(vista, segment, segments),
            vista.tint_rgb,
            assets,
            &mut ensure_texture_uploaded_with_clut_mode,
        );
        if let Some((material, texture_width, texture_height)) = material {
            let options = options
                .with_depth_policy(DepthPolicy::Farthest)
                .with_cull_mode(CullMode::None)
                .with_material_layer(material);
            let _ = world.submit_textured_world_quad(
                triangles,
                camera,
                [
                    WorldVertex::new(x0, y1, z0),
                    WorldVertex::new(x1, y1, z1),
                    WorldVertex::new(x1, y0, z1),
                    WorldVertex::new(x0, y0, z0),
                ],
                [
                    (0, 0),
                    (texture_width.saturating_sub(1), 0),
                    (
                        texture_width.saturating_sub(1),
                        texture_height.saturating_sub(1),
                    ),
                    (0, texture_height.saturating_sub(1)),
                ],
                material,
                options,
            );
        }
    }
}

fn angle_from_signed_degrees(degrees: i16) -> Angle {
    Angle::from_degrees((degrees as i32).rem_euclid(360) as u32)
}

fn far_vista_panel_asset(vista: LevelFarVistaRecord, segment: u8, segments: u8) -> Option<AssetId> {
    if vista.flags & far_vista_flags::TEXTURED == 0 || vista.texture_assets.is_empty() {
        return None;
    }
    let panel_count = vista.texture_assets.len();
    let panel_index = if panel_count == 1 {
        0
    } else {
        ((segment as usize) * panel_count / (segments as usize).max(1)).min(panel_count - 1)
    };
    let asset = vista.texture_assets[panel_index];
    (asset.0 != u16::MAX).then_some(asset)
}

fn far_vista_texture_material(
    asset_id: Option<AssetId>,
    tint_rgb: [u8; 3],
    assets: &'static [LevelAssetRecord],
    ensure_texture_uploaded_with_clut_mode: &mut impl FnMut(
        AssetId,
        &[u8],
        VramSlotClutMode,
    ) -> Option<VramSlot>,
) -> Option<(TextureMaterial, u8, u8)> {
    let asset = find_asset_of_kind(assets, asset_id?, AssetKind::Texture)?;
    let slot = ensure_texture_uploaded_with_clut_mode(
        asset.id,
        asset.bytes,
        VramSlotClutMode::TransparentZero,
    )?;
    Some((
        TextureMaterial::opaque(slot.clut_word, slot.tpage_word, rgb_tuple(tint_rgb))
            .with_texture_window(slot.texture_window),
        vram_slot_texture_size_u8(slot.texture_width),
        vram_slot_texture_size_u8(slot.texture_height),
    ))
}

/// True once the room's sky panorama AND far-vista panel textures are
/// VRAM-resident (the streamed-boot readiness gate).
#[cfg(feature = "cd-stream-bench")]
pub fn room_backdrop_textures_ready(
    record: &psx_level::LevelRoomRecord,
    assets: &'static [LevelAssetRecord],
    find_sky_panorama_vram_slot: impl Fn(AssetId) -> Option<VramSlot>,
    ensure_sky_panorama_uploaded: impl FnMut(AssetId, &[u8]) -> Option<VramSlot>,
    ensure_texture_uploaded_with_clut_mode: impl FnMut(
        AssetId,
        &[u8],
        VramSlotClutMode,
    ) -> Option<VramSlot>,
) -> bool {
    sky_panorama_texture_ready(
        record.sky,
        assets,
        find_sky_panorama_vram_slot,
        ensure_sky_panorama_uploaded,
    ) & far_vista_textures_ready(
        record.far_vista,
        assets,
        ensure_texture_uploaded_with_clut_mode,
    )
}

#[cfg(feature = "cd-stream-bench")]
fn sky_panorama_texture_ready(
    sky: LevelSkyRecord,
    assets: &'static [LevelAssetRecord],
    find_sky_panorama_vram_slot: impl Fn(AssetId) -> Option<VramSlot>,
    ensure_sky_panorama_uploaded: impl FnMut(AssetId, &[u8]) -> Option<VramSlot>,
) -> bool {
    if sky.flags & sky_flags::ENABLED == 0 {
        return true;
    }
    let Some(asset) = find_asset_of_kind(assets, sky.cloud_layer.texture_asset, AssetKind::Texture)
    else {
        return true;
    };
    sky_panorama_resident(
        asset,
        find_sky_panorama_vram_slot,
        ensure_sky_panorama_uploaded,
    )
}

/// Resolve whether the room sky's panorama is uploaded to VRAM. Streamed sky
/// assets (empty baked bytes) are uploaded on gameplay entry, so this only
/// queries the existing slot; baked sky assets upload lazily on first call.
fn sky_panorama_resident(
    asset: &LevelAssetRecord,
    find_sky_panorama_vram_slot: impl Fn(AssetId) -> Option<VramSlot>,
    mut ensure_sky_panorama_uploaded: impl FnMut(AssetId, &[u8]) -> Option<VramSlot>,
) -> bool {
    if asset.bytes.is_empty() {
        find_sky_panorama_vram_slot(asset.id).is_some()
    } else {
        ensure_sky_panorama_uploaded(asset.id, asset.bytes).is_some()
    }
}

#[cfg(feature = "cd-stream-bench")]
fn far_vista_textures_ready(
    vista: LevelFarVistaRecord,
    assets: &'static [LevelAssetRecord],
    mut ensure_texture_uploaded_with_clut_mode: impl FnMut(
        AssetId,
        &[u8],
        VramSlotClutMode,
    ) -> Option<VramSlot>,
) -> bool {
    if vista.flags & far_vista_flags::ENABLED == 0 || vista.flags & far_vista_flags::TEXTURED == 0 {
        return true;
    }
    let segments = vista.segments.clamp(3, 16);
    let mut ready = true;
    let mut segment = 0u8;
    while segment < segments {
        if let Some(asset_id) = far_vista_panel_asset(vista, segment, segments) {
            if let Some(asset) = find_asset_of_kind(assets, asset_id, AssetKind::Texture) {
                if ensure_texture_uploaded_with_clut_mode(
                    asset.id,
                    asset.bytes,
                    VramSlotClutMode::TransparentZero,
                )
                .is_none()
                {
                    ready = false;
                }
            }
        }
        segment += 1;
    }
    ready
}
