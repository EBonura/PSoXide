//! Manifest and generated-asset writer for editor-playtest.

use std::fmt::Write as _;

use super::*;
use crate::{UiFontChoice, UiGradientDirection, UiImageEffect, UiNodeKind, UiValueBinding};

const CD_SECTOR_BYTES: usize = 2048;

fn remove_optional_file(path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// `LevelModelFrameBoundsRecord` stores its sphere as `i16` to halve the
/// per-frame table; refuse a cook whose bounds would not survive that.
fn validate_model_frame_bounds(package: &PlaytestPackage) -> std::io::Result<()> {
    let out_of_range = package.model_frame_bounds.iter().position(|bounds| {
        bounds
            .center
            .iter()
            .chain(core::iter::once(&bounds.radius))
            .any(|&value| i16::try_from(value).is_err())
    });
    match out_of_range {
        None => Ok(()),
        Some(index) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "model frame bounds {index} ({:?}, radius {}) exceed the i16 range of \
                 psx_level::LevelModelFrameBoundsRecord. Nothing was written.",
                package.model_frame_bounds[index].center, package.model_frame_bounds[index].radius,
            ),
        )),
    }
}

pub fn write_package(package: &PlaytestPackage, generated_dir: &Path) -> std::io::Result<()> {
    // Validate the streaming contract BEFORE touching the filesystem.
    //
    // Writing starts by purging the generated directories, so a failure part
    // way through leaves `generated/` emptied and incomplete -- and because the
    // directory is shared by every project, a failed cook of one project
    // destroys the cooked manifest of whichever project was there before. The
    // oversized-chunk case is the one that actually fires, so check every room
    // up front and fail as a clean no-op.
    validate_model_frame_bounds(package)?;
    // Same discipline for the session-resident payloads. Without this the
    // ceiling is only reported by a MIPS link failure naming a section, long
    // after the cook that caused it.
    let resident = super::budget::validate_resident_assets(package)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if resident.near_cap() {
        crate::playtest::emit_cook_output(format_args!(
            "warning: {} (over {}% of the ceiling){}",
            resident.summary(),
            crate::playtest::budget::PLAYTEST_RESIDENT_ASSET_WARN_PERCENT,
            resident.breakdown(6),
        ));
    }

    let stream_chunks_dir = generated_dir.join(STREAM_CHUNKS_DIRNAME);
    let ui_stream_chunks_dir = generated_dir.join(UI_STREAM_CHUNKS_DIRNAME);
    let textures_dir = generated_dir.join(TEXTURES_DIRNAME);
    let models_dir = generated_dir.join(MODELS_DIRNAME);
    let ui_sfx_dir = generated_dir.join(UI_SFX_DIRNAME);
    let cdda_tracks_dir = generated_dir.join(CDDA_TRACKS_DIRNAME);
    std::fs::create_dir_all(&stream_chunks_dir)?;
    std::fs::create_dir_all(&ui_stream_chunks_dir)?;
    std::fs::create_dir_all(&textures_dir)?;
    std::fs::create_dir_all(&models_dir)?;
    std::fs::create_dir_all(&ui_sfx_dir)?;
    std::fs::create_dir_all(&cdda_tracks_dir)?;
    purge_directory_files(&stream_chunks_dir, "psxc")?;
    purge_directory_files(&ui_stream_chunks_dir, "psxt")?;
    purge_directory_files(&textures_dir, "psxt")?;
    purge_directory_files(&ui_sfx_dir, "psau")?;
    purge_directory_files(&cdda_tracks_dir, "cdda")?;
    // Models live in per-model subfolders so the recursive
    // purge needs to traverse one level deeper than rooms /
    // textures.
    purge_models_dir(&models_dir)?;

    let pxbsp_path = generated_dir.join(crate::brush_playtest::BRUSH_WORLD_FILENAME);
    let brush_leak_path = generated_dir.join(crate::brush_playtest::BRUSH_LEAK_FILENAME);
    let world = &package.world_geometry;
    std::fs::write(&pxbsp_path, &world.bytes)?;
    if world.leak_path.is_empty() {
        remove_optional_file(&brush_leak_path)?;
    } else {
        let mut pointfile = String::new();
        for &[x, y, z] in &world.leak_path {
            writeln!(pointfile, "{x} {y} {z}").expect("writing to String cannot fail");
        }
        std::fs::write(&brush_leak_path, pointfile)?;
    }

    for asset in &package.assets {
        // ModelMesh / ModelAnimation / model-folder Texture
        // asset filenames already include their `models/...`
        // subpath; rooms + room-only textures stay flat in
        // their respective dirs.
        let target = match asset.kind {
            PlaytestAssetKind::Texture if asset.filename.contains('/') => {
                generated_dir.join(&asset.filename)
            }
            PlaytestAssetKind::Texture => textures_dir.join(&asset.filename),
            PlaytestAssetKind::ModelMesh | PlaytestAssetKind::ModelAnimation => {
                generated_dir.join(&asset.filename)
            }
        };
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, &asset.bytes)?;
    }
    for sample in &package.ui_sfx_samples {
        std::fs::write(ui_sfx_dir.join(&sample.filename), &sample.bytes)?;
    }
    // Streamed Texture payloads (UI images) are written into the UI
    // stream-chunks dir as raw texture bytes (no chunk header), keyed
    // by asset index, for the ISO packer to assemble into UI.PAK. The
    // same bytes also land in `textures/` above so the non-streaming
    // (`include_bytes!`) build still resolves them.
    for (index, asset) in package.assets.iter().enumerate() {
        if asset.is_streamed() {
            std::fs::write(
                ui_stream_chunks_dir.join(format!("ui_{index:03}.psxt")),
                &asset.bytes,
            )?;
        }
    }
    // The UI SFX bank rides in UI.PAK too, after every asset chunk id. The
    // guest uploads it to SPU RAM once at boot, so the streaming build reads
    // it off the disc instead of linking a copy that is dead after boot. The
    // `ui_sfx/` files above still back the linked (non-streaming) build.
    for (chunk_id, bytes) in ui_sfx_pack_chunks(package) {
        std::fs::write(
            ui_stream_chunks_dir.join(format!("ui_{chunk_id:03}.psxt")),
            bytes,
        )?;
    }
    let manifest = render_manifest_source(package);
    std::fs::write(generated_dir.join(COOKED_MANIFEST_FILENAME), manifest)?;
    std::fs::write(
        generated_dir.join(WORLD_PACK_ORDER_FILENAME),
        render_world_pack_order(),
    )?;
    std::fs::write(
        generated_dir.join(UI_PACK_ORDER_FILENAME),
        render_ui_pack_order(package),
    )?;
    std::fs::write(
        generated_dir.join(CDDA_TRACKS_FILENAME),
        write_cdda_tracks(package, &cdda_tracks_dir)?,
    )?;
    Ok(())
}

/// Render `package` as a Rust source string the runtime example
/// can `include!`. Imports types from `psx_level` rather than
/// re-defining them so the writer here and the reader there
/// stay in lockstep.
/// Emit a `pub static {name}: &[u8]` whose backing blob is forced to
/// 4-byte (word) alignment via [`AlignedAssetBytes`]. Plain
/// `include_bytes!` is byte-aligned, which makes the runtime stream the
/// texture/sky payload to VRAM one word at a time through the GP0 FIFO;
/// a word-aligned blob lets it DMA the payload in a single block-mode
/// transfer instead (`pixel_bytes` sits at a 4-aligned offset).
fn write_aligned_asset_bytes_static(out: &mut String, static_name: &str, include_path: &str) {
    let _ = writeln!(out, "pub static {static_name}: &[u8] = {{");
    let _ = writeln!(out, "    static ALIGNED: &AlignedAssetBytes<u32, [u8]> =");
    let _ = writeln!(
        out,
        "        &AlignedAssetBytes {{ _align: [], bytes: *include_bytes!(\"{include_path}\") }};",
    );
    let _ = writeln!(out, "    &ALIGNED.bytes");
    let _ = writeln!(out, "}};");
}

pub fn render_manifest_source(package: &PlaytestPackage) -> String {
    let mut out = String::new();
    out.push_str(MANIFEST_HEADER);
    // Reserve exact model tables and the largest single-model projection
    // scratch. Worst-case unused slots otherwise consume tens of KB on PS1.
    let mut model_capacities = [0usize; 4];
    for model in &package.models {
        let Some(mesh) = package
            .assets
            .get(model.mesh_asset_index)
            .and_then(|asset| psx_asset::Model::from_bytes(&asset.bytes).ok())
        else {
            // Invalid packages still produce diagnostics with conservative
            // budgets; the cooker reports their asset errors separately.
            model_capacities = [1024, 2048, 128, 1536];
            break;
        };
        model_capacities[0] = model_capacities[0].max(usize::from(mesh.vertex_count()));
        model_capacities[1] += usize::from(mesh.face_count());
        model_capacities[2] += usize::from(mesh.part_count());
        model_capacities[3] += usize::from(mesh.vertex_count());
    }
    for (name, capacity) in [
        "MODEL_PROJECTED_VERTEX_CAPACITY",
        "MODEL_FACE_CAPACITY",
        "MODEL_PART_CAPACITY",
        "MODEL_DECODED_VERTEX_CAPACITY",
    ]
    .into_iter()
    .zip(model_capacities)
    {
        let _ = writeln!(out, "pub const {name}: usize = {};", capacity.max(1));
    }
    let _ = writeln!(
        out,
        "pub const BSP_COOK_IS_RELEASE: bool = {};\n",
        matches!(
            package.bsp_cook_mode,
            crate::brush_world::BrushWorldCookMode::Release
        )
    );
    let _ = writeln!(
        out,
        "pub const PLAYTEST_PACKET_CAPACITY: usize = {};\n",
        super::budget::cooked_manifest_packet_capacity(package)
    );
    // UI.PAK immediately follows WORLD.PAK in the ISO file order, so its
    // start LBA is the WORLD.PAK start LBA plus the WORLD.PAK total sectors.
    // WORLD.PAK itself starts after a fixed boot area: SYSTEM.CNF and PSX.EXE
    // are first on disc, then invisible padding keeps streamed pack LBAs stable
    // across executable size changes.
    let world_pack_total_sectors = world_pack_layout(package).total_sectors;
    let ui_pack_start_lba = psx_iso::WORLD_PACK_DEFAULT_START_LBA + world_pack_total_sectors;
    let ui_pack_toc = ui_pack_toc(package);
    // Staging-buffer sizing is per streamed class so each runtime buffer
    // stays right-sized: UI images load through a small per-image buffer,
    // gameplay-scoped textures (the sky) through a larger transient one.
    // Both classes share UI.PAK / UI_PACK_TOC; only the staging differs.
    let ui_pack_max_chunk_bytes = streamed_class_max_chunk_bytes(package, StreamedClass::UiImage);
    let ui_pack_image_cache_slots = streamed_class_chunk_count(package, StreamedClass::UiImage);
    // A cube sky is band-streamed straight into VRAM by
    // `VramRuntime::upload_cube_sky_banded`, never staged whole, so it must not
    // size the transient staging buffer. At 1536x256 4bpp it is 196,828 bytes
    // against a 32,924-byte runner-up, and letting it set this constant cost
    // 94 KiB of guest RAM for a texture that is copied once and discarded.
    let cube_sky_assets: Vec<usize> = package
        .rooms
        .iter()
        .filter(|room| room.sky.flags & psx_level::sky_flags::CUBE != 0)
        .filter_map(|room| room.sky.texture_asset_index)
        .collect();
    let gameplay_pack_max_chunk_bytes = package
        .assets
        .iter()
        .enumerate()
        .filter(|(_, asset)| asset.streamed_class == StreamedClass::Gameplay)
        .filter(|(index, _)| !cube_sky_assets.contains(index))
        .map(|(_, asset)| asset.bytes.len())
        .max()
        .unwrap_or(0);
    let persistent_asset_slot_count = package.assets.len().max(1);
    let persistent_asset_page_count = package
        .assets
        .iter()
        .filter(|asset| asset.streamed_class == StreamedClass::PersistentGameplay)
        .map(|asset| asset.bytes.len().next_multiple_of(4))
        .sum::<usize>()
        .div_ceil(CD_SECTOR_BYTES)
        .max(1);
    let _ = writeln!(
        out,
        "pub const PERSISTENT_ASSET_SLOT_COUNT: usize = {persistent_asset_slot_count};\n",
    );
    let _ = writeln!(
        out,
        "pub const PERSISTENT_ASSET_PAGE_COUNT: usize = {persistent_asset_page_count};\n",
    );
    // Box-prop runtime state is one slot per authored prop. Cooking the
    // count keeps ~1.4 KB per unused slot out of the PS1's 2 MB.
    let box_prop_state_count = package.box_props.len().max(1);
    let _ = writeln!(
        out,
        "pub const BOX_PROP_STATE_COUNT: usize = {box_prop_state_count};\n",
    );

    // Force 4-byte alignment on embedded asset blobs. A zero-size
    // `[u32; 0]` marker bumps the wrapper's alignment to a word; the
    // trailing unsized `bytes` field then starts word-aligned, which
    // keeps each texture's `pixel_bytes` DMA-eligible at upload time.
    out.push_str("#[allow(dead_code)]\n");
    out.push_str("#[repr(C)]\n");
    out.push_str("struct AlignedAssetBytes<Align, Bytes: ?Sized> {\n");
    out.push_str("    _align: [Align; 0],\n");
    out.push_str("    bytes: Bytes,\n");
    out.push_str("}\n\n");

    let world = &package.world_geometry;
    out.push_str("pub const PLAYTEST_USES_PXBSP: bool = true;\n");
    // Keep actor/prop lighting on the same ambient contract used by the
    // brush light bake. PXBSP surfaces carry baked vertex light; dynamic
    // world content consumes this generated constant.
    out.push_str("pub const PXBSP_AMBIENT_RGB: [u8; 3] = [32; 3];\n");
    let _ = writeln!(
        out,
        "pub const PXBSP_FACE_CHAIN_CAPACITY: usize = {};",
        world.max_visible_faces,
    );
    write_aligned_asset_bytes_static(
        &mut out,
        "PXBSP_WORLD",
        crate::brush_playtest::BRUSH_WORLD_FILENAME,
    );
    let node_ids = world
        .movers
        .iter()
        .map(|mover| mover.node.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let model_indices = world
        .movers
        .iter()
        .map(|mover| mover.model_index.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(
        out,
        "pub static PXBSP_MOVER_NODE_IDS: &[u32] = &[{node_ids}];"
    );
    let _ = writeln!(
        out,
        "pub static PXBSP_MOVER_MODEL_INDICES: &[u16] = &[{model_indices}];"
    );
    out.push_str(
        "pub static PXBSP_BODY_HULLS: &[psx_bsp::collision_provider::CookedBodyHull] = &[\n",
    );
    for hull in world.body_hulls {
        let _ = writeln!(
            out,
            "    psx_bsp::collision_provider::CookedBodyHull::new({}, {}, {}),",
            hull.hull_index, hull.radius, hull.height,
        );
    }
    out.push_str("];\n\n");

    // Emit one named static per asset so the include_bytes! call
    // sites are easy to grep for. Asset records reference these
    // statics so the slice is still constructible at compile time.
    for (i, asset) in package.assets.iter().enumerate() {
        let include_path = match asset.kind {
            PlaytestAssetKind::Texture if asset.filename.contains('/') => asset.filename.clone(),
            PlaytestAssetKind::Texture => format!("{TEXTURES_DIRNAME}/{}", asset.filename),
            PlaytestAssetKind::ModelMesh | PlaytestAssetKind::ModelAnimation => {
                asset.filename.clone()
            }
        };
        let _ = writeln!(
            out,
            "/// {} - {}",
            asset_static_name(asset, i),
            asset.source_label,
        );
        // Streamed Texture assets (UI images) stream off UI.PAK. They emit
        // empty baked bytes under `cd-stream-bench` and the normal
        // word-aligned `include_bytes!` static when the feature is off.
        if asset.is_streamed() {
            let _ = writeln!(out, "#[cfg(feature = \"cd-stream-bench\")]");
            let _ = writeln!(
                out,
                "pub static {}: &[u8] = &[];",
                asset_static_name(asset, i)
            );
            let _ = writeln!(out, "#[cfg(not(feature = \"cd-stream-bench\"))]");
            write_aligned_asset_bytes_static(&mut out, &asset_static_name(asset, i), &include_path);
        } else {
            write_aligned_asset_bytes_static(&mut out, &asset_static_name(asset, i), &include_path);
        }
    }
    // UI SFX samples stream off UI.PAK under `cd-stream-bench` (see
    // `ui_sfx_pack_chunks`), so that build links empty bytes for them.
    for (i, sample) in package.ui_sfx_samples.iter().enumerate() {
        let static_name = ui_sfx_sample_static_name(i);
        let _ = writeln!(out, "/// {static_name} - {}", sample.source_path);
        let _ = writeln!(out, "#[cfg(feature = \"cd-stream-bench\")]");
        let _ = writeln!(out, "pub static {static_name}: &[u8] = &[];");
        let _ = writeln!(out, "#[cfg(not(feature = \"cd-stream-bench\"))]");
        write_aligned_asset_bytes_static(
            &mut out,
            &static_name,
            &format!("{UI_SFX_DIRNAME}/{}", sample.filename),
        );
    }
    out.push('\n');

    out.push_str("/// Master asset table.\n");
    out.push_str("pub static ASSETS: &[LevelAssetRecord] = &[\n");
    for (i, asset) in package.assets.iter().enumerate() {
        let kind = match asset.kind {
            PlaytestAssetKind::Texture => "AssetKind::Texture",
            PlaytestAssetKind::ModelMesh => "AssetKind::ModelMesh",
            PlaytestAssetKind::ModelAnimation => "AssetKind::ModelAnimation",
        };
        let static_name = asset_static_name(asset, i);
        let vram_bytes = asset_vram_bytes(asset);
        let ram_bytes = asset.bytes.len();
        let flags = match asset.streamed_class {
            StreamedClass::None => "0",
            StreamedClass::UiImage => "asset_flags::STREAMED_UI",
            StreamedClass::Gameplay => "asset_flags::STREAMED_GAMEPLAY_TRANSIENT",
            StreamedClass::PersistentGameplay => "asset_flags::STREAMED_GAMEPLAY_PERSISTENT",
        };
        let _ = writeln!(
            out,
            "    LevelAssetRecord {{ id: AssetId({i}), kind: {kind}, bytes: {static_name}, ram_bytes: {ram_bytes}, vram_bytes: {vram_bytes}, flags: {flags} }},"
        );
    }
    out.push_str("];\n\n");

    for (room_index, room) in package.rooms.iter().enumerate() {
        if room.far_vista.texture_asset_indices.is_empty() {
            continue;
        }
        let assets = room
            .far_vista
            .texture_asset_indices
            .iter()
            .map(|index| {
                index
                    .map(|index| format!("AssetId({index})"))
                    .unwrap_or_else(|| "AssetId(u16::MAX)".to_string())
            })
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            out,
            "static FAR_VISTA_TEXTURES_{room_index}: &[AssetId] = &[{assets}];"
        );
    }
    if package
        .rooms
        .iter()
        .any(|room| !room.far_vista.texture_asset_indices.is_empty())
    {
        out.push('\n');
    }

    let mut sky_cyclorama_defs: Vec<&[crate::SkyCycloramaQuad]> = Vec::new();
    let mut sky_cyclorama_refs: Vec<String> = Vec::with_capacity(package.rooms.len());
    for room in &package.rooms {
        if room.sky.cyclorama_quads.is_empty() {
            sky_cyclorama_refs.push("&[]".to_string());
        } else if let Some(index) = sky_cyclorama_defs
            .iter()
            .position(|quads| *quads == room.sky.cyclorama_quads.as_slice())
        {
            sky_cyclorama_refs.push(format!("SKY_CYCLORAMA_QUADS_{index}"));
        } else {
            let index = sky_cyclorama_defs.len();
            sky_cyclorama_defs.push(room.sky.cyclorama_quads.as_slice());
            sky_cyclorama_refs.push(format!("SKY_CYCLORAMA_QUADS_{index}"));
        }
    }
    for (cyclorama_index, quads) in sky_cyclorama_defs.iter().enumerate() {
        let _ = writeln!(
            out,
            "static SKY_CYCLORAMA_QUADS_{cyclorama_index}: &[LevelCycloramaQuadRecord] = &["
        );
        for quad in *quads {
            let _ = writeln!(
                out,
                "    LevelCycloramaQuadRecord {{ direction_q12: [[{}, {}, {}], [{}, {}, {}], [{}, {}, {}], [{}, {}, {}]], rgb: [[{}, {}, {}], [{}, {}, {}], [{}, {}, {}], [{}, {}, {}]], flags: 0 }},",
                quad.direction_q12[0][0],
                quad.direction_q12[0][1],
                quad.direction_q12[0][2],
                quad.direction_q12[1][0],
                quad.direction_q12[1][1],
                quad.direction_q12[1][2],
                quad.direction_q12[2][0],
                quad.direction_q12[2][1],
                quad.direction_q12[2][2],
                quad.direction_q12[3][0],
                quad.direction_q12[3][1],
                quad.direction_q12[3][2],
                quad.rgb[0][0],
                quad.rgb[0][1],
                quad.rgb[0][2],
                quad.rgb[1][0],
                quad.rgb[1][1],
                quad.rgb[1][2],
                quad.rgb[2][0],
                quad.rgb[2][1],
                quad.rgb[2][2],
                quad.rgb[3][0],
                quad.rgb[3][1],
                quad.rgb[3][2],
            );
        }
        out.push_str("];\n");
    }
    if !sky_cyclorama_defs.is_empty() {
        out.push('\n');
    }

    out.push_str("/// Rooms with material-slice metadata.\n");
    out.push_str("pub static ROOMS: &[LevelRoomRecord] = &[\n");
    for (room_index, room) in package.rooms.iter().enumerate() {
        let far_vista_texture_assets = if room.far_vista.texture_asset_indices.is_empty() {
            "&[]".to_string()
        } else {
            format!("FAR_VISTA_TEXTURES_{room_index}")
        };
        let sky_cyclorama_quads = &sky_cyclorama_refs[room_index];
        let _ = writeln!(
            out,
            "    LevelRoomRecord {{ name: {:?}, sector_size: {}, draw_distance: {}, gravity_per_tick_q8: {}, fog_rgb: [{}, {}, {}], fog_near: {}, fog_far: {}, atmosphere_rgb: [{}, {}, {}], atmosphere_density: {}, atmosphere_fall_speed_q4: {}, atmosphere_wind_speed_q4: {}, sky: LevelSkyRecord {{ top_rgb: [{}, {}, {}], horizon_rgb: [{}, {}, {}], bottom_rgb: [{}, {}, {}], horizon_percent: {}, horizon_thickness_percent: {}, skybox_columns: {}, skybox_rows: {}, flags: {}, texture_asset: AssetId({}), cyclorama_quads: {}, cloud_layer: LevelCloudLayerRecord {{ texture_asset: AssetId({}), color_rgb: [{}, {}, {}], density: {}, altitude: {}, extent: {}, tile_count: {}, scroll_speed: [{}, {}], noise_seed: 0x{:08x}, flags: {} }} }}, far_vista: LevelFarVistaRecord {{ texture_assets: {}, radius: {}, height: {}, vertical_offset: {}, segments: {}, rotation_degrees: {}, tint_rgb: [{}, {}, {}], flags: {} }}, camera: LevelCameraRecord {{ distance: {}, height: {}, target_height: {}, lock_rise_percent: {}, min_floor_clearance: {}, orbit_speed_level: {}, accelerated_orbit: {}, recenter_preserves_pitch: {}, fov_y_degrees: {}, blend_profiles: {}, lock_target_framing: {}, lock_profile: {}, position_lag_shift: {}, position_vertical_lag_shift: {}, focus_lag_shift: {}, focus_vertical_lag_shift: {}, distance_lag_shift: {} }}, flags: {} }},",
            room.name,
            room.sector_size,
            room.draw_distance,
            room.gravity_per_tick_q8,
            room.fog_rgb[0],
            room.fog_rgb[1],
            room.fog_rgb[2],
            room.fog_near,
            room.fog_far,
            room.atmosphere_rgb[0],
            room.atmosphere_rgb[1],
            room.atmosphere_rgb[2],
            room.atmosphere_density,
            room.atmosphere_fall_speed_q4,
            room.atmosphere_wind_speed_q4,
            room.sky.top_rgb[0],
            room.sky.top_rgb[1],
            room.sky.top_rgb[2],
            room.sky.horizon_rgb[0],
            room.sky.horizon_rgb[1],
            room.sky.horizon_rgb[2],
            room.sky.bottom_rgb[0],
            room.sky.bottom_rgb[1],
            room.sky.bottom_rgb[2],
            room.sky.horizon_percent,
            room.sky.horizon_thickness_percent,
            room.sky.skybox_columns,
            room.sky.skybox_rows,
            room.sky.flags,
            room.sky
                .texture_asset_index
                .map(|index| index.to_string())
                .unwrap_or_else(|| "u16::MAX".to_string()),
            sky_cyclorama_quads,
            room.sky
                .cloud_layer
                .texture_asset_index
                .map(|index| index.to_string())
                .unwrap_or_else(|| "u16::MAX".to_string()),
            room.sky.cloud_layer.color_rgb[0],
            room.sky.cloud_layer.color_rgb[1],
            room.sky.cloud_layer.color_rgb[2],
            room.sky.cloud_layer.density,
            room.sky.cloud_layer.altitude,
            room.sky.cloud_layer.extent,
            room.sky.cloud_layer.tile_count,
            room.sky.cloud_layer.scroll_speed[0],
            room.sky.cloud_layer.scroll_speed[1],
            room.sky.cloud_layer.noise_seed,
            room.sky.cloud_layer.flags,
            far_vista_texture_assets,
            room.far_vista.radius,
            room.far_vista.height,
            room.far_vista.vertical_offset,
            room.far_vista.segments,
            room.far_vista.rotation_degrees,
            room.far_vista.tint_rgb[0],
            room.far_vista.tint_rgb[1],
            room.far_vista.tint_rgb[2],
            room.far_vista.flags,
            room.camera.distance,
            room.camera.height,
            room.camera.target_height,
            room.camera.lock_rise_percent,
            room.camera.min_floor_clearance,
            room.camera.orbit_speed_level,
            room.camera.accelerated_orbit,
            room.camera.recenter_preserves_pitch,
            room.camera.fov_y_degrees,
            room.camera.blend_profiles,
            room.camera.lock_target_framing,
            room.camera.lock_profile.map_or_else(|| "None".to_owned(), |p| format!("Some(LevelCameraProfile {{ distance: {}, height: {}, target_height: {}, fov_y_degrees: {}, shoulder_offset: {} }})", p.distance, p.height, p.target_height, p.fov_y_degrees, p.shoulder_offset)),
            room.camera.position_lag_shift,
            room.camera.position_vertical_lag_shift,
            room.camera.focus_lag_shift,
            room.camera.focus_vertical_lag_shift,
            room.camera.distance_lag_shift,
            room.flags,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Per-runtime-room host-baked 4bpp reflection probes.\n");
    out.push_str("pub static ROOM_REFLECTION_PROBES: &[Option<AssetId>] = &[\n");
    for room in &package.rooms {
        let literal = room
            .reflection_probe_asset_index
            .map(|index| format!("Some(AssetId({index}))"))
            .unwrap_or_else(|| "None".to_string());
        let _ = writeln!(out, "    {literal},");
    }
    out.push_str("];\n\n");

    out.push_str(
        "/// Absolute disc LBA where UI.PAK starts in the playtest ISO layout.\n\
         /// UI.PAK is packed immediately after WORLD.PAK by the ISO builder.\n",
    );
    let _ = writeln!(
        out,
        "pub const UI_PACK_START_LBA: u32 = {ui_pack_start_lba};"
    );
    out.push('\n');

    out.push_str("/// Largest streamed UI image chunk in bytes (UI image staging buffer size).\n");
    let _ = writeln!(
        out,
        "pub const UI_PACK_MAX_CHUNK_BYTES: usize = {ui_pack_max_chunk_bytes};",
    );
    out.push('\n');

    out.push_str("/// Number of streamed menu UI image chunks cached at menu startup.\n");
    let _ = writeln!(
        out,
        "pub const UI_PACK_IMAGE_CACHE_SLOTS: usize = {ui_pack_image_cache_slots};",
    );
    out.push('\n');

    out.push_str("/// UI.PAK chunk id of UI SFX sample 0; sample `i` is this plus `i`.\n");
    let _ = writeln!(
        out,
        "pub const UI_SFX_PACK_FIRST_CHUNK: u32 = {};",
        ui_sfx_pack_first_chunk(package),
    );
    out.push_str("/// Largest UI SFX sample in bytes (boot-time staging size).\n");
    let _ = writeln!(
        out,
        "pub const UI_SFX_MAX_SAMPLE_BYTES: usize = {};",
        package
            .ui_sfx_samples
            .iter()
            .map(|sample| sample.bytes.len())
            .max()
            .unwrap_or(0),
    );
    out.push('\n');

    out.push_str(
        "/// Largest streamed gameplay-scoped chunk in bytes (e.g. the sky panorama).\n\
         /// Sizes the transient gameplay staging buffer, kept separate from the UI\n\
         /// image buffer so the small per-image buffer stays small.\n",
    );
    let _ = writeln!(
        out,
        "pub const GAMEPLAY_PACK_MAX_CHUNK_BYTES: usize = {gameplay_pack_max_chunk_bytes};",
    );
    out.push('\n');

    out.push_str(
        "/// Cooked UI.PAK image table generated from the same layout as the ISO packer.\n\
         /// Each entry's `room` field carries the streamed asset index as its chunk id.\n",
    );
    out.push_str("pub static UI_PACK_TOC: &[LevelWorldPackEntryRecord] = &[\n");
    for entry in &ui_pack_toc {
        let _ = writeln!(
            out,
            "    LevelWorldPackEntryRecord {{ room: RoomIndex({}), sector_offset: {}, sector_count: {}, byte_size: {}, checksum: {} }},",
            entry.chunk_id,
            entry.sector_offset,
            entry.sector_count,
            entry.byte_size,
            entry.checksum,
        );
    }
    out.push_str("];\n\n");

    let spawn = package.spawn.unwrap_or(PlaytestSpawn {
        room: 0,
        x: 0,
        y: 0,
        z: 0,
        yaw: 0,
        flags: 0,
    });
    let _ = writeln!(
        out,
        "/// Player spawn.\npub static PLAYER_SPAWN: PlayerSpawnRecord = PlayerSpawnRecord {{ room: RoomIndex({}), x: {}, y: {}, z: {}, yaw: {}, flags: {} }};",
        spawn.room, spawn.x, spawn.y, spawn.z, spawn.yaw, spawn.flags
    );
    out.push('\n');

    // MODELS / MODEL_CLIPS / MODEL_INSTANCES -- emitted as
    // empty slices when there are no model instances, so the
    // runtime always has something to walk.
    out.push_str("/// Per-model clip records, ordered (model, clip).\n");
    out.push_str("pub static MODEL_CLIPS: &[LevelModelClipRecord] = &[\n");
    for clip in &package.model_clips {
        let _ = writeln!(
            out,
            "    LevelModelClipRecord {{ model: ModelIndex({}), name: {:?}, animation_asset: AssetId({}) }},",
            clip.model, clip.name, clip.animation_asset_index,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Per-clip frame-bound slices, ordered like MODEL_CLIPS.\n");
    out.push_str("pub static MODEL_CLIP_BOUNDS: &[LevelModelClipBoundsRecord] = &[\n");
    for bounds in &package.model_clip_bounds {
        let _ = writeln!(
            out,
            "    LevelModelClipBoundsRecord {{ model: ModelIndex({}), clip: ModelClipTableIndex({}), first_frame: ModelFrameBoundsIndex({}), frame_count: {}, floor_y: {}, pose_offset: [{}, {}, {}], flags: {} }},",
            bounds.model,
            bounds.clip,
            bounds.first_frame,
            bounds.frame_count,
            bounds.floor_y,
            bounds.pose_offset[0],
            bounds.pose_offset[1],
            bounds.pose_offset[2],
            bounds.flags,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Conservative per-frame model bounds in model-local engine units.\n");
    out.push_str("pub static MODEL_FRAME_BOUNDS: &[LevelModelFrameBoundsRecord] = &[\n");
    for bounds in &package.model_frame_bounds {
        let _ = writeln!(
            out,
            "    LevelModelFrameBoundsRecord {{ center: [{}, {}, {}], radius: {} }},",
            bounds.center[0], bounds.center[1], bounds.center[2], bounds.radius,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Model attachment sockets, ordered by model.\n");
    out.push_str("pub static MODEL_SOCKETS: &[LevelModelSocketRecord] = &[\n");
    for socket in &package.model_sockets {
        let _ = writeln!(
            out,
            "    LevelModelSocketRecord {{ model: ModelIndex({}), name: {:?}, joint: {}, translation: [{}, {}, {}], rotation_q12: [{}, {}, {}], flags: 0 }},",
            socket.model,
            socket.name,
            socket.joint,
            socket.translation[0],
            socket.translation[1],
            socket.translation[2],
            socket.rotation_q12[0],
            socket.rotation_q12[1],
            socket.rotation_q12[2],
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Cooked models - instances reference these by index.\n");
    out.push_str("pub static MODELS: &[LevelModelRecord] = &[\n");
    for model in &package.models {
        let texture = match model.texture_asset_index {
            Some(idx) => format!("Some(AssetId({idx}))"),
            None => "None".to_string(),
        };
        let _ = writeln!(
            out,
            "    LevelModelRecord {{ name: {:?}, mesh_asset: AssetId({}), texture_asset: {texture}, clip_first: ModelClipTableIndex({}), clip_count: {}, default_clip: ModelClipIndex({}), socket_first: ModelSocketIndex({}), socket_count: {}, world_height: {}, collision_radius: {}, flags: 0 }},",
            model.name,
            model.mesh_asset_index,
            model.clip_first,
            model.clip_count,
            model.default_clip,
            model.socket_first,
            model.socket_count,
            model.world_height,
            model.collision_radius,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Placed model instances, room-local coordinates.\n");
    out.push_str("pub static MODEL_INSTANCES: &[LevelModelInstanceRecord] = &[\n");
    for inst in &package.model_instances {
        let clip = if inst.clip == MODEL_CLIP_INHERIT {
            "MODEL_CLIP_INHERIT".to_string()
        } else {
            format!(
                "OptionalModelClipIndex::some(ModelClipIndex({}))",
                inst.clip
            )
        };
        let _ = writeln!(
            out,
            "    LevelModelInstanceRecord {{ room: RoomIndex({}), model: ModelIndex({}), clip: {clip}, pose_frame: {}, x: {}, y: {}, z: {}, yaw: {}, visual_yaw: {}, pitch: {}, roll: {}, visual_offset: [{}, {}, {}], visual_scale_q8: {}, material_override: {}, flags: {} }},",
            inst.room,
            inst.model,
            inst.pose_frame,
            inst.x,
            inst.y,
            inst.z,
            inst.yaw,
            inst.visual_yaw,
            inst.pitch,
            inst.roll,
            inst.visual_offset[0],
            inst.visual_offset[1],
            inst.visual_offset[2],
            inst.visual_scale_q8,
            model_material_override_literal(&inst.material_override),
            inst.flags,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Shared health/configuration for every destructible target.\n");
    out.push_str("pub static DESTRUCTIBLES: &[LevelDestructibleRecord] = &[\n");
    for destructible in &package.destructibles {
        let _ = writeln!(
            out,
            "    LevelDestructibleRecord {{ max_health: {}, persistent_flag: {}, damage_affinity: {}, flags: {} }},",
            destructible.max_health,
            destructible.persistent_flag,
            destructible.damage_affinity,
            destructible.flags,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Shared spatial registry for specialized world payloads.\n");
    out.push_str("pub static WORLD_OBJECTS: &[LevelWorldObjectRecord] = &[\n");
    for object in &package.world_objects {
        let _ = writeln!(
            out,
            "    LevelWorldObjectRecord {{ room: RoomIndex({}), kind: {}, flags: {}, source_index: {}, destructible: {}, bounds_min: {:?}, bounds_max: {:?} }},",
            object.room,
            object.kind,
            object.flags,
            object.source_index,
            object.destructible,
            object.bounds_min,
            object.bounds_max,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Placed flat image props, room-local coordinates.\n");
    out.push_str("pub static IMAGE_PROPS: &[LevelImagePropRecord] = &[\n");
    for prop in &package.image_props {
        let _ = writeln!(
            out,
            "    LevelImagePropRecord {{ room: RoomIndex({}), texture_asset: AssetId({}), x: {}, y: {}, z: {}, pitch: {}, yaw: {}, roll: {}, width: {}, height: {}, tint_rgb: [{}, {}, {}], baked_vertex_rgb: [({}, {}, {}), ({}, {}, {}), ({}, {}, {}), ({}, {}, {})], collision_min: {:?}, collision_max: {:?}, flags: {} }},",
            prop.room,
            prop.texture_asset_index,
            prop.x,
            prop.y,
            prop.z,
            prop.pitch,
            prop.yaw,
            prop.roll,
            prop.width,
            prop.height,
            prop.tint_rgb[0],
            prop.tint_rgb[1],
            prop.tint_rgb[2],
            prop.baked_vertex_rgb[0].0,
            prop.baked_vertex_rgb[0].1,
            prop.baked_vertex_rgb[0].2,
            prop.baked_vertex_rgb[1].0,
            prop.baked_vertex_rgb[1].1,
            prop.baked_vertex_rgb[1].2,
            prop.baked_vertex_rgb[2].0,
            prop.baked_vertex_rgb[2].1,
            prop.baked_vertex_rgb[2].2,
            prop.baked_vertex_rgb[3].0,
            prop.baked_vertex_rgb[3].1,
            prop.baked_vertex_rgb[3].2,
            prop.collision_min,
            prop.collision_max,
            prop.flags,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Placed editable box props, room-local coordinates.\n");
    out.push_str("pub static BOX_PROPS: &[LevelBoxPropRecord] = &[\n");
    for prop in &package.box_props {
        let texture_assets = render_box_prop_texture_assets(&prop.texture_asset_indices);
        let vertices = render_box_prop_vertices(&prop.vertices);
        let tint_rgb = render_box_prop_tint_rgb(&prop.tint_rgb);
        let baked_vertex_rgb = render_box_prop_baked_vertex_rgb(&prop.baked_vertex_rgb);
        let _ = writeln!(
            out,
            "    LevelBoxPropRecord {{ room: RoomIndex({}), texture_assets: {texture_assets}, blend_modes: {:?}, uvs: {:?}, x: {}, y: {}, z: {}, ground_y: {}, pitch: {}, yaw: {}, roll: {}, vertices: {vertices}, collision_min: {:?}, collision_max: {:?}, surface_first: {}, surface_count: {}, tint_rgb: {tint_rgb}, baked_vertex_rgb: {baked_vertex_rgb}, flags: {} }},",
            prop.room,
            prop.blend_modes,
            prop.uvs,
            prop.x,
            prop.y,
            prop.z,
            prop.ground_y,
            prop.pitch,
            prop.yaw,
            prop.roll,
            prop.collision_min,
            prop.collision_max,
            prop.surface_first,
            prop.surface_count,
            prop.flags,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Cook-generated directional erosion surfaces for BoxProps.\n");
    out.push_str("pub static BOX_PROP_SURFACES: &[LevelBoxPropSurfaceRecord] = &[\n");
    for surface in &package.box_prop_surfaces {
        let _ = writeln!(
            out,
            "    LevelBoxPropSurfaceRecord {{ vertices: {:?}, center: {:?}, normal: {:?}, uv_q8: {:?}, baked_vertex_rgb: {:?}, source_face: {}, flags: {} }},",
            surface.vertices,
            surface.center,
            surface.normal,
            surface.uv_q8,
            surface.baked_vertex_rgb,
            surface.source_face,
            surface.flags,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Placed low-poly procedural radial props.\n");
    out.push_str("pub static CYLINDER_PROPS: &[LevelCylinderPropRecord] = &[\n");
    for prop in &package.cylinder_props {
        let texture_assets = render_cylinder_prop_texture_assets(&prop.texture_asset_indices);
        let _ = writeln!(
            out,
            "    LevelCylinderPropRecord {{ room: RoomIndex({}), texture_assets: {texture_assets}, blend_modes: {:?}, uvs: {:?}, surface_first: {}, surface_count: {}, center: {:?}, cull_radius: {}, bounds_min: {:?}, bounds_max: {:?}, flags: {} }},",
            prop.room,
            prop.blend_modes,
            prop.uvs,
            prop.surface_first,
            prop.surface_count,
            prop.center,
            prop.cull_radius,
            prop.bounds_min,
            prop.bounds_max,
            prop.flags,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Cook-generated CylinderProp triangles and quads.\n");
    out.push_str("pub static CYLINDER_PROP_SURFACES: &[LevelCylinderPropSurfaceRecord] = &[\n");
    for surface in &package.cylinder_prop_surfaces {
        let _ = writeln!(
            out,
            "    LevelCylinderPropSurfaceRecord {{ vertices: {:?}, center: {:?}, normal: {:?}, uv_q8: {:?}, baked_vertex_rgb: {:?}, material_slot: {}, vertex_count: {} }},",
            surface.vertices,
            surface.center,
            surface.normal,
            surface.uv_q8,
            surface.baked_vertex_rgb,
            surface.material_slot,
            surface.vertex_count,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Placed tile-native arches.\n");
    out.push_str("pub static ARCH_PROPS: &[LevelArchPropRecord] = &[\n");
    for prop in &package.arch_props {
        let texture_assets = render_arch_prop_texture_assets(&prop.texture_asset_indices);
        let _ = writeln!(
            out,
            "    LevelArchPropRecord {{ room: RoomIndex({}), texture_assets: {texture_assets}, blend_modes: {:?}, uvs: {:?}, surface_first: {}, surface_count: {}, collision_first: {}, collision_count: {}, center: {:?}, cull_radius: {}, flags: {} }},",
            prop.room,
            prop.blend_modes,
            prop.uvs,
            prop.surface_first,
            prop.surface_count,
            prop.collision_first,
            prop.collision_count,
            prop.center,
            prop.cull_radius,
            prop.flags,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Cook-generated ArchProp quads.\n");
    out.push_str("pub static ARCH_PROP_SURFACES: &[LevelArchPropSurfaceRecord] = &[\n");
    for surface in &package.arch_prop_surfaces {
        let _ = writeln!(
            out,
            "    LevelArchPropSurfaceRecord {{ vertices: {:?}, center: {:?}, normal: {:?}, uv_q8: {:?}, baked_vertex_rgb: {:?}, material_slot: {} }},",
            surface.vertices,
            surface.center,
            surface.normal,
            surface.uv_q8,
            surface.baked_vertex_rgb,
            surface.material_slot,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Per-segment ArchProp collision approximation.\n");
    out.push_str("pub static ARCH_PROP_COLLISIONS: &[LevelArchPropCollisionRecord] = &[\n");
    for collision in &package.arch_prop_collisions {
        let _ = writeln!(
            out,
            "    LevelArchPropCollisionRecord {{ min: {:?}, max: {:?} }},",
            collision.min, collision.max,
        );
    }
    out.push_str("];\n\n");

    let ui_fonts = collect_ui_fonts(&package.ui_nodes);
    out.push_str("/// Cooked UI font sources, compacted to fonts used by cooked UI text.\n");
    out.push_str("pub static UI_FONTS: &[&psx_font::BitmapFont] = &[\n");
    for font in &ui_fonts {
        let source = render_ui_font_source(*font);
        let _ = writeln!(out, "    &{source},");
    }
    out.push_str("];\n");
    out.push_str("const _: () = assert!(UI_FONTS.len() <= 8);\n\n");

    out.push_str("/// Cooked UI gradient paints referenced by UI node color roles.\n");
    out.push_str("pub static UI_PAINTS: &[LevelUiPaintRecord] = &[\n");
    for paint in &package.ui_paints {
        let direction = render_ui_gradient_direction(paint.direction);
        let _ = writeln!(
            out,
            "    LevelUiPaintRecord {{ from: [{}, {}, {}], to: [{}, {}, {}], direction: {direction} }},",
            paint.from[0],
            paint.from[1],
            paint.from[2],
            paint.to[0],
            paint.to[1],
            paint.to[2],
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Cooked screen-space UI nodes.\n");
    out.push_str("pub static UI_NODES: &[LevelUiNodeRecord] = &[\n");
    for node in &package.ui_nodes {
        let parent = node
            .parent
            .map(|index| format!("Some(UiNodeIndex({index}))"))
            .unwrap_or_else(|| "None".to_string());
        let kind = render_ui_node_kind(&node.kind);
        let value = render_ui_value_binding(node.value);
        let max = render_ui_value_binding(node.max);
        let action = render_ui_action(node.action);
        let texture_asset = node
            .texture_asset
            .map(|index| format!("AssetId({index})"))
            .unwrap_or_else(|| "AssetId(u16::MAX)".to_string());
        let font = compact_ui_font_index(&ui_fonts, node);
        let color_paint = render_ui_paint_ref(node.color_paint);
        let background_paint = render_ui_paint_ref(node.background_paint);
        let accent_paint = render_ui_paint_ref(node.accent_paint);
        let image_effect = render_ui_image_effect(node.image_effect);
        let _ = writeln!(
            out,
            "    LevelUiNodeRecord {{ parent: {parent}, kind: {kind}, x: {}, y: {}, width: {}, height: {}, color: [{}, {}, {}], background: [{}, {}, {}], accent: [{}, {}, {}], color_paint: {color_paint}, background_paint: {background_paint}, accent_paint: {accent_paint}, value: {value}, max: {max}, texture_asset: {texture_asset}, image_effect: {image_effect}, text: {:?}, tag: {:?}, action: {action}, option: {}, rotation_degrees: {}, flags: {}, sfx_first: {}, sfx_count: {}, font: {}, font_scale: {}, letter_spacing: {} }},",
            node.x,
            node.y,
            node.width,
            node.height,
            node.color[0],
            node.color[1],
            node.color[2],
            node.background[0],
            node.background[1],
            node.background[2],
            node.accent[0],
            node.accent[1],
            node.accent[2],
            node.text,
            node.tag,
            node.option,
            node.rotation_degrees,
            node.flags,
            node.sfx_first,
            node.sfx_count,
            font,
            node.font_scale,
            node.letter_spacing,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Cooked UI SFX samples.\n");
    out.push_str("pub static UI_SFX_SAMPLES: &[LevelUiSfxSampleRecord] = &[\n");
    for i in 0..package.ui_sfx_samples.len() {
        let static_name = ui_sfx_sample_static_name(i);
        let _ = writeln!(
            out,
            "    LevelUiSfxSampleRecord {{ bytes: {static_name} }},"
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Cooked UI SFX cue bindings.\n");
    out.push_str("pub static UI_SFX_CUES: &[LevelUiSfxCueRecord] = &[\n");
    for cue in &package.ui_sfx_cues {
        let event = render_ui_sfx_event(cue.event);
        let _ = writeln!(
            out,
            "    LevelUiSfxCueRecord {{ sample: {}, event: {event}, volume_percent: {}, pitch_q12: {}, flags: {} }},",
            cue.sample, cue.volume_percent, cue.pitch_q12, cue.flags,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Cooked gameplay SFX cue bindings.\n");
    out.push_str("pub static GAMEPLAY_SFX_CUES: &[LevelGameplaySfxCueRecord] = &[\n");
    for cue in &package.gameplay_sfx_cues {
        let event = render_gameplay_sfx_event(cue.event);
        let _ = writeln!(
            out,
            "    LevelGameplaySfxCueRecord {{ sample: {}, event: {event}, volume_percent: {}, pitch_q12: {}, flags: {} }},",
            cue.sample, cue.volume_percent, cue.pitch_q12, cue.flags,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Addressable cooked UI scenes indexing `UI_NODES`.\n");
    out.push_str("pub static UI_SCENES: &[LevelUiScene] = &[\n");
    for scene in &package.ui_scenes {
        let style = &scene.focus_style;
        let _ = writeln!(
            out,
            "    LevelUiScene {{ id: {}, name: {:?}, node_first: {}, node_count: {}, focus_style: LevelUiFocusStyle {{ effect: LevelUiFocusEffect::{:?}, color_a: ({}, {}, {}), color_b: ({}, {}, {}), period: {}, thickness: {}, margin: {}, corner_len: {} }} }},",
            scene.id,
            scene.name,
            scene.node_first,
            scene.node_count,
            style.effect,
            style.color_a[0],
            style.color_a[1],
            style.color_a[2],
            style.color_b[0],
            style.color_b[1],
            style.color_b[2],
            style.period,
            style.thickness,
            style.margin,
            style.corner_len,
        );
    }
    out.push_str("];\n\n");

    // Loading screen by authoring convention: a UI scene literally named
    // "Loading" (case-insensitive) is drawn by the engine while it streams
    // the next state's world. UI_SCENE_NONE selects the engine's built-in
    // minimal fallback screen.
    let loading_scene = package
        .ui_scenes
        .iter()
        .find(|scene| scene.name.eq_ignore_ascii_case("loading"))
        .map(|scene| scene.id.to_string())
        .unwrap_or_else(|| "psx_level::UI_SCENE_NONE".to_string());
    out.push_str("/// UI scene drawn during world-load (the scene named \"Loading\"),\n");
    out.push_str("/// or `UI_SCENE_NONE` for the engine's built-in fallback.\n");
    let _ = writeln!(out, "pub const LOADING_UI_SCENE: u16 = {loading_scene};\n");

    out.push_str("/// Composed runtime scene states.\n");
    out.push_str("pub static SCENE_STATES: &[LevelSceneState] = &[\n");
    for state in &package.game_flow.scene_states {
        let world = match state.world {
            PlaytestWorldLayer::None => "LevelWorldLayer::None",
            PlaytestWorldLayer::Gameplay => "LevelWorldLayer::Gameplay",
        };
        let _ = writeln!(
            out,
            "    LevelSceneState {{ id: {}, name: {:?}, world: {}, ui_scene: {}, flags: {}, start_state: {} }},",
            state.id, state.name, world, state.ui_scene, state.flags, state.start_state,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Cooked game-state flow.\n");
    out.push_str("pub static GAME_FLOW: GameFlow = GameFlow {\n");
    out.push_str("    states: &[\n");
    for state in &package.game_flow.states {
        let rendered = match state {
            PlaytestFlowState::SceneState { state } => {
                format!("FlowState::SceneState {{ state: {state} }}")
            }
            PlaytestFlowState::UiScene { scene } => {
                format!("FlowState::UiScene {{ scene: {scene} }}")
            }
            PlaytestFlowState::Gameplay => "FlowState::Gameplay".to_string(),
        };
        let _ = writeln!(out, "        {rendered},");
    }
    out.push_str("    ],\n");
    out.push_str("    scene_states: SCENE_STATES,\n");
    let _ = writeln!(out, "    entry: {},", package.game_flow.entry);
    let _ = writeln!(
        out,
        "    transition: {},",
        render_transition(package.game_flow.transition)
    );
    out.push_str("};\n\n");

    out.push_str("/// Cooked project options sliders and SetOption actions bind to.\n");
    out.push_str("pub static OPTIONS: &[LevelOptionDef] = &[\n");
    for option in &package.options {
        let _ = writeln!(
            out,
            "    LevelOptionDef {{ id: {}, min: {}, max: {}, step: {}, default: {} }},",
            option.id, option.min, option.max, option.step, option.default,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Weapon hitboxes, local to weapon grips.\n");
    out.push_str("pub static WEAPON_HITBOXES: &[WeaponHitboxRecord] = &[\n");
    for hitbox in &package.weapon_hitboxes {
        let shape = render_weapon_hit_shape(hitbox.shape);
        let _ = writeln!(
            out,
            "    WeaponHitboxRecord {{ name: {:?}, shape: {shape}, active_start_frame: {}, active_end_frame: {}, flags: 0 }},",
            hitbox.name, hitbox.active_start_frame, hitbox.active_end_frame,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Cooked Weapon resources.\n");
    out.push_str("pub static WEAPONS: &[LevelWeaponRecord] = &[\n");
    for weapon in &package.weapons {
        let model = weapon
            .model
            .map(|model| format!("Some(ModelIndex({model}))"))
            .unwrap_or_else(|| "None".to_string());
        let _ = writeln!(
            out,
            "    LevelWeaponRecord {{ name: {:?}, model: {model}, default_character_socket: {:?}, grip_name: {:?}, grip_translation: [{}, {}, {}], grip_rotation_q12: [{}, {}, {}], hitbox_first: WeaponHitboxIndex({}), hitbox_count: {}, arc_reach: {}, arc_half_angle: {}, damage: {}, poise_damage: {}, flags: 0 }},",
            weapon.name,
            weapon.default_character_socket,
            weapon.grip_name,
            weapon.grip_translation[0],
            weapon.grip_translation[1],
            weapon.grip_translation[2],
            weapon.grip_rotation_q12[0],
            weapon.grip_rotation_q12[1],
            weapon.grip_rotation_q12[2],
            weapon.hitbox_first,
            weapon.hitbox_count,
            weapon.arc_reach,
            weapon.arc_half_angle,
            weapon.damage,
            weapon.poise_damage,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Equipment components, room-local parent transforms.\n");
    out.push_str("pub static EQUIPMENT: &[EquipmentRecord] = &[\n");
    for equipment in &package.equipment {
        let _ = writeln!(
            out,
            "    EquipmentRecord {{ room: RoomIndex({}), weapon: WeaponIndex({}), x: {}, y: {}, z: {}, yaw: {}, character_socket: {:?}, weapon_grip: {:?}, model_instance: {}, flags: {} }},",
            equipment.room,
            equipment.weapon,
            equipment.x,
            equipment.y,
            equipment.z,
            equipment.yaw,
            equipment.character_socket,
            equipment.weapon_grip,
            equipment.model_instance,
            equipment.flags,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Placed point lights, room-local coordinates.\n");
    out.push_str("pub static LIGHTS: &[PointLightRecord] = &[\n");
    for light in &package.lights {
        let _ = writeln!(
            out,
            "    PointLightRecord {{ room: RoomIndex({}), x: {}, y: {}, z: {}, radius: {}, intensity_q8: {}, color: [{}, {}, {}], flags: 0 }},",
            light.room,
            light.x,
            light.y,
            light.z,
            light.radius,
            light.intensity_q8,
            light.color[0],
            light.color[1],
            light.color[2],
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Placed point-projected particle emitters, room-local coordinates.\n");
    out.push_str("pub static PARTICLE_EMITTERS: &[ParticleEmitterRecord] = &[\n");
    for emitter in &package.particle_emitters {
        let _ = writeln!(
            out,
            "    ParticleEmitterRecord {{ room: RoomIndex({}), x: {}, y: {}, z: {}, max_particles: {}, spawn_rate_q8: {}, lifetime_frames: {}, start_size: {}, end_size: {}, start_color: [{}, {}, {}], end_color: [{}, {}, {}], blend_mode: {}, base_velocity_q4: [{}, {}, {}], random_velocity_q4: [{}, {}, {}], acceleration_q4: [{}, {}, {}], spawn_radius: {}, flags: {} }},",
            emitter.room,
            emitter.x,
            emitter.y,
            emitter.z,
            emitter.max_particles,
            emitter.spawn_rate_q8,
            emitter.lifetime_frames,
            emitter.start_size,
            emitter.end_size,
            emitter.start_color[0],
            emitter.start_color[1],
            emitter.start_color[2],
            emitter.end_color[0],
            emitter.end_color[1],
            emitter.end_color[2],
            emitter.blend_mode,
            emitter.base_velocity_q4[0],
            emitter.base_velocity_q4[1],
            emitter.base_velocity_q4[2],
            emitter.random_velocity_q4[0],
            emitter.random_velocity_q4[1],
            emitter.random_velocity_q4[2],
            emitter.acceleration_q4[0],
            emitter.acceleration_q4[1],
            emitter.acceleration_q4[2],
            emitter.spawn_radius,
            emitter.flags,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Text payloads referenced by placed interactables.\n");
    out.push_str("pub static INTERACTABLE_MESSAGES: &[InteractableMessageRecord] = &[\n");
    for message in &package.interactable_messages {
        let _ = writeln!(
            out,
            "    InteractableMessageRecord {{ title: {:?}, body: {:?}, page_first: {}, page_count: {} }},",
            message.title, message.body, message.page_first, message.page_count,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Flat body-page table shared by POI and world messages.\n");
    out.push_str("pub static INTERACTABLE_MESSAGE_PAGES: &[&str] = &[\n");
    for page in &package.interactable_message_pages {
        let _ = writeln!(out, "    {:?},", page);
    }
    out.push_str("];\n\n");

    out.push_str("/// Italian column of INTERACTABLE_MESSAGE_PAGES; \"\" falls back to English.\n");
    out.push_str("pub static INTERACTABLE_MESSAGE_PAGES_IT: &[&str] = &[\n");
    for page in &package.interactable_message_pages_it {
        let _ = writeln!(out, "    {:?},", page);
    }
    out.push_str("];\n\n");

    out.push_str("/// Optional per-scene message shown once per game launch.\n");
    match &package.world_message {
        Some(message) => {
            let _ = writeln!(
                out,
                "pub static WORLD_MESSAGE: Option<InteractableMessageRecord> = Some(InteractableMessageRecord {{ title: {:?}, body: {:?}, page_first: {}, page_count: {} }});",
                message.title, message.body, message.page_first, message.page_count,
            );
        }
        None => {
            out.push_str("pub static WORLD_MESSAGE: Option<InteractableMessageRecord> = None;\n")
        }
    }
    let _ = writeln!(
        out,
        "pub const PERSISTENT_FLAG_COUNT: u16 = {};\n",
        package.persistent_flag_count,
    );
    let _ = writeln!(
        out,
        "pub const PROJECT_SAVE_NAME: &str = {:?};",
        package.save_name,
    );
    let _ = writeln!(
        out,
        "pub const PROJECT_SAVE_TITLE: &str = {:?};\n",
        package.save_title,
    );

    out.push_str("/// Unique collectible modules referenced by POI rewards.\n");
    out.push_str("pub static BOOST_MODULES: &[BoostModuleRecord] = &[\n");
    for module in &package.boost_modules {
        let _ = writeln!(
            out,
            "    BoostModuleRecord {{ name: {:?}, description: {:?}, effect_summary: {:?}, assignment_label: {:?}, remove_label: {:?}, percentages: {:?} }},",
            module.name,
            module.description,
            module.effect_summary,
            module.assignment_label,
            module.remove_label,
            module.percentages,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Italian (name, description) per BOOST_MODULES entry; \"\" keeps English.\n");
    out.push_str("pub static BOOST_MODULES_IT: &[(&str, &str)] = &[\n");
    for module in &package.boost_modules {
        let _ = writeln!(
            out,
            "    ({:?}, {:?}),",
            module.name_it, module.description_it
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Placed gameplay interactables, room-local coordinates.\n");
    out.push_str("pub static INTERACTABLES: &[InteractableRecord] = &[\n");
    for interactable in &package.interactables {
        let kind = match interactable.kind {
            PlaytestInteractableKind::Message => "InteractableKind::Message",
            PlaytestInteractableKind::Checkpoint => "InteractableKind::Checkpoint",
            PlaytestInteractableKind::PointOfInterest => "InteractableKind::PointOfInterest",
        };
        let message = if interactable.message == psx_level::INTERACTABLE_MESSAGE_NONE {
            "psx_level::INTERACTABLE_MESSAGE_NONE".to_string()
        } else {
            interactable.message.to_string()
        };
        let logic = if interactable.logic == psx_level::INTERACTABLE_LOGIC_NONE {
            "psx_level::INTERACTABLE_LOGIC_NONE".to_string()
        } else {
            interactable.logic.to_string()
        };
        let _ = writeln!(
            out,
            "    InteractableRecord {{ room: RoomIndex({}), kind: {kind}, x: {}, y: {}, z: {}, yaw: {}, radius: {}, marker_height: {}, prompt: {:?}, message: {message}, logic: {logic}, checkpoint_id: {:?}, read_flag: {}, reward_flag: {}, reward_resource: {}, reward_quantity: {}, flags: {} }},",
            interactable.room,
            interactable.x,
            interactable.y,
            interactable.z,
            interactable.yaw,
            interactable.radius,
            interactable.marker_height,
            interactable.prompt,
            interactable.checkpoint_id,
            interactable.read_flag,
            interactable.reward_flag,
            interactable.reward_resource,
            interactable.reward_quantity,
            interactable.flags,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Placed dual-vitality fields.\n");
    out.push_str("pub static VITALITY_CIRCLES: &[LevelVitalityCircleRecord] = &[\n");
    for circle in &package.vitality_circles {
        let _ = writeln!(
            out,
            "    LevelVitalityCircleRecord {{ room: RoomIndex({}), x: {}, y: {}, z: {}, radius: {}, axis: {}, refill_per_second: {}, drain_per_second: {} }},",
            circle.room,
            circle.x,
            circle.y,
            circle.z,
            circle.radius,
            circle.axis,
            circle.refill_per_second,
            circle.drain_per_second,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Cooked logic entities (phase-3 event graph), room-local coordinates.\n");
    out.push_str("/// Authored names are interned to u16 ids; the strings died at cook.\n");
    out.push_str("pub static LOGIC: &[LevelLogicRecord] = &[\n");
    for logic in &package.logic {
        let message = if logic.message == psx_level::INTERACTABLE_MESSAGE_NONE {
            "psx_level::INTERACTABLE_MESSAGE_NONE".to_string()
        } else {
            logic.message.to_string()
        };
        let link = if logic.link == psx_level::LOGIC_LINK_NONE {
            "psx_level::LOGIC_LINK_NONE".to_string()
        } else {
            logic.link.to_string()
        };
        let _ = writeln!(
            out,
            "    LevelLogicRecord {{ room: RoomIndex({}), kind: {}, spawnflags: {}, targetname: {}, target: {}, killtarget: {}, master: {}, delay_ticks: {}, wait_ticks: {}, arg0: {}, arg1: {}, link: {link}, message: {message}, x: {}, y: {}, z: {}, min: [{}, {}, {}], max: [{}, {}, {}], flags: {} }},",
            logic.room,
            logic.kind,
            logic.spawnflags,
            logic.targetname,
            logic.target,
            logic.killtarget,
            logic.master,
            logic.delay_ticks,
            logic.wait_ticks,
            logic.arg0,
            logic.arg1,
            logic.x,
            logic.y,
            logic.z,
            logic.min[0],
            logic.min[1],
            logic.min[2],
            logic.max[0],
            logic.max[1],
            logic.max[2],
            logic.flags,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Compact rig-attached hitboxes, hurtboxes, and projectile emitters.\n");
    out.push_str("pub static COMBAT_CAPSULES: &[CombatCapsuleRecord] = &[\n");
    for capsule in &package.combat_capsules {
        let _ = writeln!(
            out,
            "    CombatCapsuleRecord {{ joint: {}, flags: {}, action: {}, reserved: 0, start: [{}, {}, {}], end: [{}, {}, {}], radius: {}, active_start_frame: {}, active_end_frame: {}, damage: {}, poise_damage: {}, projectile_speed: {}, projectile_lifetime_ticks: {}, projectile_min_range: {}, projectile_max_range: {}, projectile_tint_rgb: [{}, {}, {}], projectile_damage_channel: {}, projectile_core_rgb: [{}, {}, {}], projectile_trail_segments: {}, projectile_glow_rgb: [{}, {}, {}], projectile_length_ticks: {}, projectile_impact_rgb: [{}, {}, {}], projectile_trail_spacing_ticks: {}, projectile_charge_start_frame: {}, projectile_glow_scale_q8: {}, projectile_impact_lifetime_ticks: {}, projectile_reserved: 0 }},",
            capsule.joint,
            capsule.flags,
            capsule.action,
            capsule.start[0],
            capsule.start[1],
            capsule.start[2],
            capsule.end[0],
            capsule.end[1],
            capsule.end[2],
            capsule.radius,
            capsule.active_start_frame,
            capsule.active_end_frame,
            capsule.damage,
            capsule.poise_damage,
            capsule.projectile_speed,
            capsule.projectile_lifetime_ticks,
            capsule.projectile_min_range,
            capsule.projectile_max_range,
            capsule.projectile_tint_rgb[0],
            capsule.projectile_tint_rgb[1],
            capsule.projectile_tint_rgb[2],
            capsule.projectile_damage_channel,
            capsule.projectile_core_rgb[0],
            capsule.projectile_core_rgb[1],
            capsule.projectile_core_rgb[2],
            capsule.projectile_trail_segments,
            capsule.projectile_glow_rgb[0],
            capsule.projectile_glow_rgb[1],
            capsule.projectile_glow_rgb[2],
            capsule.projectile_length_ticks,
            capsule.projectile_impact_rgb[0],
            capsule.projectile_impact_rgb[1],
            capsule.projectile_impact_rgb[2],
            capsule.projectile_trail_spacing_ticks,
            capsule.projectile_charge_start_frame,
            capsule.projectile_glow_scale_q8,
            capsule.projectile_impact_lifetime_ticks,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Placed souls-like game entities, room-local coordinates.\n");
    out.push_str("pub static GAME_ENTITIES: &[LevelGameEntityRecord] = &[\n");
    for entity in &package.game_entities {
        let model_instance = if entity.model_instance == psx_level::GAME_ENTITY_MODEL_INSTANCE_NONE
        {
            "psx_level::GAME_ENTITY_MODEL_INSTANCE_NONE".to_string()
        } else {
            entity.model_instance.to_string()
        };
        let _ = writeln!(
            out,
            "    LevelGameEntityRecord {{ room: RoomIndex({}), kind: {}, targetname: {}, model_instance: {model_instance}, idle_clip: {}, alert_clip: {}, turn_clip: {}, walk_clip: {}, walk_backward_clip: {}, strafe_left_clip: {}, strafe_right_clip: {}, run_clip: {}, attack_clip: {}, attack_speed_q8: {}, attack_frame_range: CharacterActionFrameRange {{ start: {}, end: {} }}, heavy_attack_clip: {}, heavy_attack_speed_q8: {}, heavy_attack_frame_range: CharacterActionFrameRange {{ start: {}, end: {} }}, ranged_attack_clip: {}, ranged_attack_speed_q8: {}, ranged_attack_frame_range: CharacterActionFrameRange {{ start: {}, end: {} }}, stagger_clip: {}, stagger_speed_q8: {}, stagger_frame_range: CharacterActionFrameRange {{ start: {}, end: {} }}, stagger_ticks: {}, death_clip: {}, combat_capsule_first: CombatCapsuleIndex({}), combat_capsule_count: {}, ranged_attack_action: {}, x: {}, y: {}, z: {}, yaw: {}, radius: {}, height: {}, walk_speed: {}, run_speed: {}, patrol_x: {}, patrol_y: {}, patrol_z: {}, patrol_wait_ticks: {}, aggro_radius: {}, reaction_ticks: {}, preferred_distance: {}, spacing_tolerance: {}, spacing_speed_percent: {}, decision_interval_ticks: {}, circle_chance: {}, attack_priority: {}, attack_cooldown_ticks: {}, group_attack_delay_ticks: {}, windup_ticks: {}, attack_active_ticks: {}, heavy_attack_active_ticks: {}, ranged_attack_active_ticks: {}, recovery_ticks: {}, attack_min_range: {}, attack_max_range: {}, poise: {}, touch_damage: {}, max_health: {}, max_health_secondary: {}, soul_value: {}, flags: {} }},",
            entity.room,
            entity.kind,
            entity.targetname,
            entity.idle_clip,
            entity.alert_clip,
            entity.turn_clip,
            entity.walk_clip,
            entity.walk_backward_clip,
            entity.strafe_left_clip,
            entity.strafe_right_clip,
            entity.run_clip,
            entity.attack_clip,
            entity.attack_speed_q8,
            entity.attack_frame_range.start,
            entity.attack_frame_range.end,
            entity.heavy_attack_clip,
            entity.heavy_attack_speed_q8,
            entity.heavy_attack_frame_range.start,
            entity.heavy_attack_frame_range.end,
            entity.ranged_attack_clip,
            entity.ranged_attack_speed_q8,
            entity.ranged_attack_frame_range.start,
            entity.ranged_attack_frame_range.end,
            entity.stagger_clip,
            entity.stagger_speed_q8,
            entity.stagger_frame_range.start,
            entity.stagger_frame_range.end,
            entity.stagger_ticks,
            entity.death_clip,
            entity.combat_capsule_first,
            entity.combat_capsule_count,
            entity.ranged_attack_action,
            entity.x,
            entity.y,
            entity.z,
            entity.yaw,
            entity.radius,
            entity.height,
            entity.walk_speed,
            entity.run_speed,
            entity.patrol[0],
            entity.patrol[1],
            entity.patrol[2],
            entity.patrol_wait_ticks,
            entity.aggro_radius,
            entity.reaction_ticks,
            entity.preferred_distance,
            entity.spacing_tolerance,
            entity.spacing_speed_percent,
            entity.decision_interval_ticks,
            entity.circle_chance,
            entity.attack_priority,
            entity.attack_cooldown_ticks,
            entity.group_attack_delay_ticks,
            entity.windup_ticks,
            entity.attack_active_ticks,
            entity.heavy_attack_active_ticks,
            entity.ranged_attack_active_ticks,
            entity.recovery_ticks,
            entity.attack_min_range,
            entity.attack_max_range,
            entity.poise,
            entity.touch_damage,
            entity.max_health,
            entity.max_health_secondary,
            entity.soul_value,
            entity.flags,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Animation-authored equipped weapon visibility beats.\n");
    out.push_str("pub static WEAPON_APPEARANCES: &[WeaponAppearanceRecord] = &[\n");
    for appearance in &package.weapon_appearances {
        let _ = writeln!(
            out,
            "    WeaponAppearanceRecord {{ character: CharacterIndex({}), action: CharacterAnimationAction::{:?}, weapon: WeaponIndex({}), character_socket: {:?}, fully_visible_frame: {}, hidden_frame: {}, transition_frames: {}, trail_start_frame: {}, trail_end_frame: {}, trail_history_frames: {}, trail_segments: {}, trail_blend_mode: {}, trail_root_color: [{}, {}, {}], trail_tip_color: [{}, {}, {}], flags: {} }},",
            appearance.character,
            appearance.action,
            appearance.weapon,
            appearance.character_socket,
            appearance.fully_visible_frame,
            appearance.hidden_frame,
            appearance.transition_frames,
            appearance.trail_start_frame,
            appearance.trail_end_frame,
            appearance.trail_history_frames,
            appearance.trail_segments,
            match appearance.trail_blend_mode {
                crate::WeaponTrailBlendMode::Average => psx_level::weapon_trail_blend_mode::AVERAGE,
                crate::WeaponTrailBlendMode::Add => psx_level::weapon_trail_blend_mode::ADD,
                crate::WeaponTrailBlendMode::Subtract => psx_level::weapon_trail_blend_mode::SUBTRACT,
                crate::WeaponTrailBlendMode::AddQuarter => psx_level::weapon_trail_blend_mode::ADD_QUARTER,
            },
            appearance.trail_root_color[0],
            appearance.trail_root_color[1],
            appearance.trail_root_color[2],
            appearance.trail_tip_color[0],
            appearance.trail_tip_color[1],
            appearance.trail_tip_color[2],
            appearance.flags,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Cooked Character resources - gameplay metadata layered on top of MODELS.\n");
    out.push_str("pub static CHARACTERS: &[LevelCharacterRecord] = &[\n");
    for character in &package.characters {
        let clip_or_none = |slot: u16| -> String {
            if slot == CHARACTER_CLIP_NONE {
                "CHARACTER_CLIP_NONE".to_string()
            } else {
                format!("OptionalModelClipIndex::some(ModelClipIndex({slot}))")
            }
        };
        let action_clips = character
            .action_clips
            .iter()
            .map(|slot| clip_or_none(*slot))
            .collect::<Vec<_>>()
            .join(", ");
        let action_flags = character
            .action_flags
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let action_speeds = character
            .action_speeds
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let action_frame_ranges = character
            .action_frame_ranges
            .iter()
            .map(|range| {
                format!(
                    "CharacterActionFrameRange {{ start: {}, end: {} }}",
                    range.start, range.end
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let action_pushes = character
            .action_pushes
            .iter()
            .map(|push| {
                format!(
                    "CharacterActionPush {{ distance: {}, frame_range: CharacterActionFrameRange {{ start: {}, end: {} }} }}",
                    push.distance, push.frame_range.start, push.frame_range.end
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            out,
            "    LevelCharacterRecord {{ model: ModelIndex({}), action_clips: [{}], action_flags: [{}], action_speeds: [{}], action_frame_ranges: [{}], action_pushes: [{}], action_chains: [{}], combat_windows: [{}], combat_capsule_first: CombatCapsuleIndex({}), combat_capsule_count: {}, visual_offset: [{}, {}, {}], visual_yaw: {}, visual_scale_q8: {}, weight_q8: {}, radius: {}, height: {}, walk_speed: {}, run_speed: {}, turn_speed_degrees_per_second: {}, stamina_max_q12: {}, sprint_min_q12: {}, sprint_drain_q12: {}, stamina_recover_q12: {}, roll_cost_q12: {}, roll_speed: {}, roll_active_frames: {}, roll_recovery_frames: {}, roll_invulnerable_frames: {}, backstep_cost_q12: {}, backstep_speed: {}, backstep_active_frames: {}, backstep_recovery_frames: {}, backstep_invulnerable_frames: {}, stance_aligned_damage_q12: {}, stance_opposed_damage_q12: {}, stance_regen_delay_ticks: {}, stance_broken_regen_delay_ticks: {}, stance_regen_per_tick_q12: {}, stance_break_threshold_q12: {}, stance_swap_cooldown_ticks: {}, stance_swap_duration_ticks: {}, camera_distance: {}, camera_height: {}, camera_target_height: {}, material_override: {}, flags: 0 }},",
            character.model,
            action_clips,
            action_flags,
            action_speeds,
            action_frame_ranges,
            action_pushes,
            character.action_chains.iter().map(|c| format!("psx_level::CharacterActionChain {{ action: {}, next_action: {}, input_start: {}, input_end: {}, handoff_frame: {}, blend_ticks: {} }}", c.action, c.next_action, c.input_start, c.input_end, c.handoff_frame, c.blend_ticks)).collect::<Vec<_>>().join(", "),
            character.combat_windows.iter().map(|w| format!("psx_level::CharacterCombatWindow {{ action: {}, kind: psx_level::CombatWindowKind::{:?}, start: {}, end: {} }}", w.action, w.kind, w.start, w.end)).collect::<Vec<_>>().join(", "),
            character.combat_capsule_first,
            character.combat_capsule_count,
            character.visual_offset[0],
            character.visual_offset[1],
            character.visual_offset[2],
            character.visual_yaw,
            character.visual_scale_q8,
            character.weight_q8,
            character.radius,
            character.height,
            character.walk_speed,
            character.run_speed,
            character.turn_speed_degrees_per_second,
            character.stamina_max_q12,
            character.sprint_min_q12,
            character.sprint_drain_q12,
            character.stamina_recover_q12,
            character.roll_cost_q12,
            character.roll_speed,
            character.roll_active_frames,
            character.roll_recovery_frames,
            character.roll_invulnerable_frames,
            character.backstep_cost_q12,
            character.backstep_speed,
            character.backstep_active_frames,
            character.backstep_recovery_frames,
            character.backstep_invulnerable_frames,
            character.stance_aligned_damage_q12,
            character.stance_opposed_damage_q12,
            character.stance_regen_delay_ticks,
            character.stance_broken_regen_delay_ticks,
            character.stance_regen_per_tick_q12,
            character.stance_break_threshold_q12,
            character.stance_swap_cooldown_ticks,
            character.stance_swap_duration_ticks,
            character.camera_distance,
            character.camera_height,
            character.camera_target_height,
            model_material_override_literal(&character.material_override),
        );
    }
    out.push_str("];\n\n");

    match package.player_controller {
        Some(pc) => {
            let _ = writeln!(
                out,
                "/// Player controller - spawn + Character that drives the player.\npub static PLAYER_CONTROLLER: Option<PlayerControllerRecord> = Some(PlayerControllerRecord {{ spawn: PlayerSpawnRecord {{ room: RoomIndex({}), x: {}, y: {}, z: {}, yaw: {}, flags: {} }}, character: CharacterIndex({}), flags: 0 }});",
                pc.spawn.room, pc.spawn.x, pc.spawn.y, pc.spawn.z, pc.spawn.yaw, pc.spawn.flags, pc.character,
            );
        }
        None => {
            out.push_str(
                "/// Player controller - `None` means no playable character was authored.\n\
                pub static PLAYER_CONTROLLER: Option<PlayerControllerRecord> = None;\n",
            );
        }
    }
    out.push('\n');

    out.push_str("/// Entity markers (legacy MeshInstance with no Model resource).\n");
    out.push_str("pub static ENTITIES: &[EntityRecord] = &[\n");
    for entity in &package.entities {
        let kind = match entity.kind {
            PlaytestEntityKind::HookPoint => "EntityKind::HookPoint",
            PlaytestEntityKind::Marker => "EntityKind::Marker",
            PlaytestEntityKind::StaticMesh => "EntityKind::StaticMesh",
        };
        let _ = writeln!(
            out,
            "    EntityRecord {{ room: RoomIndex({}), kind: {kind}, x: {}, y: {}, z: {}, yaw: {}, resource_slot: ResourceSlot({}), flags: {} }},",
            entity.room, entity.x, entity.y, entity.z, entity.yaw, entity.resource_slot, entity.flags
        );
    }
    out.push_str("];\n");
    out
}

fn render_world_pack_order() -> String {
    String::from(
        "# PSoXide WORLD.PAK room order\n\
         # One cooked room id per line. Generated by cook-playtest.\n",
    )
}

fn write_cdda_tracks(package: &PlaytestPackage, cdda_tracks_dir: &Path) -> std::io::Result<String> {
    let mut out = String::from(
        "# PSoXide cooked CD-DA raw track payloads\n\
         # One path per line. Track 2 is the first line, track 3 the second, etc.\n",
    );
    for track in &package.cdda_tracks {
        let source = Path::new(&track.wav_path);
        let bytes = std::fs::read(source)?;
        let cooked =
            psxed_audio::cook_cdda_track_from_wav_at_speed(&bytes, track.playback_speed_q12)
                .map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("cook CD-DA track {}: {e}", source.display()),
                    )
                })?;
        let filename = format!("track{:02}.cdda", track.track);
        let target = cdda_tracks_dir.join(filename);
        std::fs::write(&target, cooked)?;
        let listed = target.canonicalize().unwrap_or(target);
        let _ = writeln!(out, "{}", listed.display());
    }
    Ok(out)
}

/// WORLD.PAK is header-only: no world content streams off it, but the pack
/// stays on the disc so UI.PAK's start LBA and the loader contract are stable.
fn world_pack_layout(_package: &PlaytestPackage) -> psx_iso::WorldPackLayout {
    psx_iso::build_world_pack_layout(&[])
}

/// Streamed assets in pack order, paired with their asset index (used
/// as the UI.PAK chunk id). Mirrors `world_pack_order`'s pairing but for
/// every CD-streamed Texture asset. Both streamed classes (UI images and
/// gameplay-scoped textures like the sky) share UI.PAK, keyed by asset
/// index, so the disc packer stays unchanged; only the runtime staging
/// buffer differs per class.
fn ui_pack_chunks(package: &PlaytestPackage) -> Vec<(u32, &[u8])> {
    package
        .assets
        .iter()
        .enumerate()
        .filter(|(_, asset)| asset.is_streamed())
        .map(|(index, asset)| (index as u32, asset.bytes.as_slice()))
        .chain(ui_sfx_pack_chunks(package))
        .collect()
}

/// UI.PAK chunk id of the first UI SFX sample: the first id past every asset
/// index, so no asset chunk can collide with it.
fn ui_sfx_pack_first_chunk(package: &PlaytestPackage) -> u32 {
    package.assets.len() as u32
}

/// UI SFX samples as UI.PAK chunks, after every streamed asset.
fn ui_sfx_pack_chunks(package: &PlaytestPackage) -> impl Iterator<Item = (u32, &[u8])> {
    let first = ui_sfx_pack_first_chunk(package);
    package
        .ui_sfx_samples
        .iter()
        .enumerate()
        .map(move |(index, sample)| (first + index as u32, sample.bytes.as_slice()))
}

/// Largest payload in bytes across the streamed assets of one class.
/// Drives the runtime staging-buffer size for that class. Returns `0`
/// when no asset of the class is streamed.
fn streamed_class_max_chunk_bytes(package: &PlaytestPackage, class: StreamedClass) -> usize {
    package
        .assets
        .iter()
        .filter(|asset| asset.streamed_class == class)
        .map(|asset| asset.bytes.len())
        .max()
        .unwrap_or(0)
}

/// Number of streamed assets in one class. Used by the runtime to size fixed
/// cache metadata without reserving a pessimistic slot count.
fn streamed_class_chunk_count(package: &PlaytestPackage, class: StreamedClass) -> usize {
    package
        .assets
        .iter()
        .filter(|asset| asset.streamed_class == class)
        .count()
}

fn ui_pack_toc(package: &PlaytestPackage) -> Vec<psx_iso::WorldPackBuildEntry> {
    let refs = ui_pack_chunks(package);
    psx_iso::build_world_pack_layout(&refs).entries
}

fn ui_pack_order(package: &PlaytestPackage) -> Vec<u32> {
    ui_pack_chunks(package)
        .iter()
        .map(|(index, _)| *index)
        .collect()
}

fn render_ui_pack_order(package: &PlaytestPackage) -> String {
    let mut out = String::from(
        "# PSoXide UI.PAK image order\n\
         # One streamed UI asset index per line. Generated by cook-playtest.\n",
    );
    for index in ui_pack_order(package) {
        let _ = writeln!(out, "{index}");
    }
    out
}

fn asset_vram_bytes(asset: &PlaytestAsset) -> usize {
    match asset.kind {
        PlaytestAssetKind::ModelMesh | PlaytestAssetKind::ModelAnimation => 0,
        PlaytestAssetKind::Texture => texture_vram_bytes(asset).unwrap_or(asset.bytes.len()),
    }
}

fn texture_vram_bytes(asset: &PlaytestAsset) -> Option<usize> {
    let texture = psx_asset::Texture::from_bytes(&asset.bytes).ok()?;
    Some(texture.pixel_bytes().len() + texture.clut_bytes().len())
}

fn render_weapon_hit_shape(shape: PlaytestWeaponHitShape) -> String {
    match shape {
        PlaytestWeaponHitShape::Box {
            center,
            half_extents,
        } => format!(
            "WeaponHitShapeRecord::Box {{ center: [{}, {}, {}], half_extents: [{}, {}, {}] }}",
            center[0], center[1], center[2], half_extents[0], half_extents[1], half_extents[2],
        ),
        PlaytestWeaponHitShape::Capsule { start, end, radius } => format!(
            "WeaponHitShapeRecord::Capsule {{ start: [{}, {}, {}], end: [{}, {}, {}], radius: {} }}",
            start[0], start[1], start[2], end[0], end[1], end[2], radius,
        ),
    }
}

fn collect_ui_fonts(nodes: &[PlaytestUiNode]) -> Vec<UiFontChoice> {
    let mut fonts = Vec::new();
    for node in nodes {
        let Some(font) = authored_ui_font(&node.kind) else {
            continue;
        };
        if !fonts.contains(&font) {
            fonts.push(font);
        }
    }
    if fonts.is_empty() {
        fonts.push(UiFontChoice::Basic);
    }
    fonts
}

fn authored_ui_font(kind: &UiNodeKind) -> Option<UiFontChoice> {
    match kind {
        UiNodeKind::Label { font, .. } | UiNodeKind::Button { font, .. } => Some(*font),
        _ => None,
    }
}

fn compact_ui_font_index(fonts: &[UiFontChoice], node: &PlaytestUiNode) -> u8 {
    authored_ui_font(&node.kind)
        .and_then(|font| fonts.iter().position(|candidate| *candidate == font))
        .unwrap_or(0)
        .min(u8::MAX as usize) as u8
}

fn render_ui_font_source(font: UiFontChoice) -> &'static str {
    match font {
        UiFontChoice::Basic => "psx_font::fonts::BASIC",
        UiFontChoice::Basic8x16 => "psx_font::fonts::BASIC_8X16",
        UiFontChoice::KenneyBlocks => "psx_font::fonts::KENNEY_BLOCKS",
        UiFontChoice::KenneyFuture => "psx_font::fonts::KENNEY_FUTURE",
        UiFontChoice::KenneyFutureNarrow => "psx_font::fonts::KENNEY_FUTURE_NARROW",
        UiFontChoice::KenneyHigh => "psx_font::fonts::KENNEY_HIGH",
        UiFontChoice::KenneyHighSquare => "psx_font::fonts::KENNEY_HIGH_SQUARE",
        UiFontChoice::KenneyMini => "psx_font::fonts::KENNEY_MINI",
        UiFontChoice::KenneyMiniSquare => "psx_font::fonts::KENNEY_MINI_SQUARE",
        UiFontChoice::KenneyMiniSquareMono => "psx_font::fonts::KENNEY_MINI_SQUARE_MONO",
        UiFontChoice::KenneyPixel => "psx_font::fonts::KENNEY_PIXEL",
        UiFontChoice::KenneyPixelSquare => "psx_font::fonts::KENNEY_PIXEL_SQUARE",
        UiFontChoice::KenneyRocket => "psx_font::fonts::KENNEY_ROCKET",
        UiFontChoice::KenneyRocketSquare => "psx_font::fonts::KENNEY_ROCKET_SQUARE",
        UiFontChoice::PressStart2P => "psx_font::fonts::PRESS_START_2P",
        UiFontChoice::Silkscreen => "psx_font::fonts::SILKSCREEN",
        UiFontChoice::PixelifySans => "psx_font::fonts::PIXELIFY_SANS",
        UiFontChoice::Orbitron => "psx_font::fonts::ORBITRON",
        UiFontChoice::Audiowide => "psx_font::fonts::AUDIOWIDE",
        UiFontChoice::Michroma => "psx_font::fonts::MICHROMA",
        UiFontChoice::Electrolize => "psx_font::fonts::ELECTROLIZE",
        UiFontChoice::Oxanium => "psx_font::fonts::OXANIUM",
        UiFontChoice::Rajdhani => "psx_font::fonts::RAJDHANI",
        UiFontChoice::ChakraPetch => "psx_font::fonts::CHAKRA_PETCH",
        UiFontChoice::Tektur => "psx_font::fonts::TEKTUR",
        UiFontChoice::Tomorrow => "psx_font::fonts::TOMORROW",
        UiFontChoice::ZenDots => "psx_font::fonts::ZEN_DOTS",
        UiFontChoice::TurretRoad => "psx_font::fonts::TURRET_ROAD",
        UiFontChoice::Tiny5 => "psx_font::fonts::TINY5",
        UiFontChoice::Jersey10 => "psx_font::fonts::JERSEY_10",
        UiFontChoice::SpaceMono => "psx_font::fonts::SPACE_MONO",
        UiFontChoice::BrunoAce => "psx_font::fonts::BRUNO_ACE",
        UiFontChoice::Aldrich => "psx_font::fonts::ALDRICH",
        UiFontChoice::Syncopate => "psx_font::fonts::SYNCOPATE",
        UiFontChoice::ShareTechMono => "psx_font::fonts::SHARE_TECH_MONO",
        UiFontChoice::Jura => "psx_font::fonts::JURA",
        UiFontChoice::ZenDotsDisplay => "psx_font::fonts::ZEN_DOTS_DISPLAY",
        UiFontChoice::Spleen5x8 => "psx_font::fonts::SPLEEN_5X8",
        UiFontChoice::Spleen5x8Italic => "psx_font::fonts::SPLEEN_5X8_ITALIC",
        UiFontChoice::DrippySpace => "psx_font::fonts::DRIPPY_SPACE",
        UiFontChoice::DrippySpaceDisplay => "psx_font::fonts::DRIPPY_SPACE_DISPLAY",
    }
}

fn render_ui_node_kind(kind: &UiNodeKind) -> &'static str {
    match kind {
        UiNodeKind::Canvas { .. } => "LevelUiNodeKind::Canvas",
        UiNodeKind::Group { .. } => "LevelUiNodeKind::Group",
        UiNodeKind::Rect { .. } => "LevelUiNodeKind::Rect",
        UiNodeKind::Label { .. } => "LevelUiNodeKind::Label",
        UiNodeKind::Image { .. } => "LevelUiNodeKind::Image",
        UiNodeKind::Bar { .. } => "LevelUiNodeKind::Bar",
        UiNodeKind::Button { .. } => "LevelUiNodeKind::Button",
        UiNodeKind::Slider { .. } => "LevelUiNodeKind::Slider",
        UiNodeKind::Music { .. } => "LevelUiNodeKind::Music",
        UiNodeKind::Timer { .. } => "LevelUiNodeKind::Timer",
    }
}

fn render_ui_gradient_direction(direction: UiGradientDirection) -> &'static str {
    match direction {
        UiGradientDirection::Vertical => "LevelUiGradientDirection::Vertical",
        UiGradientDirection::Horizontal => "LevelUiGradientDirection::Horizontal",
    }
}

fn render_ui_image_effect(effect: UiImageEffect) -> &'static str {
    match effect {
        UiImageEffect::None => "LevelUiImageEffect::None",
        UiImageEffect::Shimmer => "LevelUiImageEffect::Shimmer",
        UiImageEffect::FastShimmer => "LevelUiImageEffect::FastShimmer",
        UiImageEffect::DiagonalSweep => "LevelUiImageEffect::DiagonalSweep",
        UiImageEffect::SoftPulse => "LevelUiImageEffect::SoftPulse",
        UiImageEffect::Bob => "LevelUiImageEffect::Bob",
        UiImageEffect::Rise => "LevelUiImageEffect::Rise",
        UiImageEffect::Wind => "LevelUiImageEffect::Wind",
    }
}

fn render_ui_paint_ref(paint: Option<u16>) -> String {
    paint
        .map(|index| index.to_string())
        .unwrap_or_else(|| "psx_level::UI_PAINT_NONE".to_string())
}

fn render_ui_action(action: PlaytestUiAction) -> String {
    match action {
        PlaytestUiAction::GotoState { state } => {
            format!("LevelUiAction::GotoState {{ state: {state} }}")
        }
        PlaytestUiAction::TransitionToState { state, transition } => {
            format!(
                "LevelUiAction::TransitionToState {{ state: {state}, transition: {} }}",
                render_transition(transition)
            )
        }
        PlaytestUiAction::GotoScene { scene } => {
            format!("LevelUiAction::GotoScene {{ scene: {scene} }}")
        }
        PlaytestUiAction::TransitionToScene { scene, transition } => {
            format!(
                "LevelUiAction::TransitionToScene {{ scene: {scene}, transition: {} }}",
                render_transition(transition)
            )
        }
        PlaytestUiAction::StartGameplay => "LevelUiAction::StartGameplay".to_string(),
        PlaytestUiAction::StartGameplayTransition { transition } => {
            format!(
                "LevelUiAction::StartGameplayTransition {{ transition: {} }}",
                render_transition(transition)
            )
        }
        PlaytestUiAction::Back => "LevelUiAction::Back".to_string(),
        PlaytestUiAction::SetOption { option, delta } => {
            format!("LevelUiAction::SetOption {{ option: {option}, delta: {delta} }}")
        }
        PlaytestUiAction::Game { id } => format!("LevelUiAction::Game {{ id: {id} }}"),
    }
}

fn render_transition(transition: PlaytestTransition) -> String {
    let kind = match transition.kind {
        PlaytestTransitionKind::None => "LevelTransitionKind::None",
        PlaytestTransitionKind::Fade => "LevelTransitionKind::Fade",
        PlaytestTransitionKind::BlockDissolve => "LevelTransitionKind::BlockDissolve",
        PlaytestTransitionKind::GlitchBreak => "LevelTransitionKind::GlitchBreak",
    };
    format!(
        "LevelTransition {{ kind: {kind}, frames: {}, color: [{}, {}, {}], seed: {} }}",
        transition.frames,
        transition.color[0],
        transition.color[1],
        transition.color[2],
        transition.seed
    )
}

fn render_ui_sfx_event(event: psx_level::LevelUiSfxEvent) -> &'static str {
    match event {
        psx_level::LevelUiSfxEvent::Focus => "LevelUiSfxEvent::Focus",
        psx_level::LevelUiSfxEvent::Activate => "LevelUiSfxEvent::Activate",
        psx_level::LevelUiSfxEvent::SliderNudge => "LevelUiSfxEvent::SliderNudge",
        psx_level::LevelUiSfxEvent::SliderLimit => "LevelUiSfxEvent::SliderLimit",
    }
}

fn render_gameplay_sfx_event(event: psx_level::LevelGameplaySfxEvent) -> &'static str {
    match event {
        psx_level::LevelGameplaySfxEvent::Footstep => "LevelGameplaySfxEvent::Footstep",
        psx_level::LevelGameplaySfxEvent::LightHit => "LevelGameplaySfxEvent::LightHit",
        psx_level::LevelGameplaySfxEvent::HeavyHit => "LevelGameplaySfxEvent::HeavyHit",
        psx_level::LevelGameplaySfxEvent::PlayerDamage => "LevelGameplaySfxEvent::PlayerDamage",
        psx_level::LevelGameplaySfxEvent::EnemyDeath => "LevelGameplaySfxEvent::EnemyDeath",
        psx_level::LevelGameplaySfxEvent::StanceSwapReady => {
            "LevelGameplaySfxEvent::StanceSwapReady"
        }
        psx_level::LevelGameplaySfxEvent::LightWeaponSwing => {
            "LevelGameplaySfxEvent::LightWeaponSwing"
        }
        psx_level::LevelGameplaySfxEvent::HeavyWeaponSwing => {
            "LevelGameplaySfxEvent::HeavyWeaponSwing"
        }
        psx_level::LevelGameplaySfxEvent::ProjectileCharge => {
            "LevelGameplaySfxEvent::ProjectileCharge"
        }
        psx_level::LevelGameplaySfxEvent::ProjectileLaunch => {
            "LevelGameplaySfxEvent::ProjectileLaunch"
        }
        psx_level::LevelGameplaySfxEvent::EnemyFootstep => "LevelGameplaySfxEvent::EnemyFootstep",
        psx_level::LevelGameplaySfxEvent::EnemyIdle => "LevelGameplaySfxEvent::EnemyIdle",
        psx_level::LevelGameplaySfxEvent::ItemAcquired => "LevelGameplaySfxEvent::ItemAcquired",
        psx_level::LevelGameplaySfxEvent::GameplayEnter => "LevelGameplaySfxEvent::GameplayEnter",
        psx_level::LevelGameplaySfxEvent::IntroShot => "LevelGameplaySfxEvent::IntroShot",
        psx_level::LevelGameplaySfxEvent::CombatStart => "LevelGameplaySfxEvent::CombatStart",
        psx_level::LevelGameplaySfxEvent::Dash => "LevelGameplaySfxEvent::Dash",
        psx_level::LevelGameplaySfxEvent::StanceSwap => "LevelGameplaySfxEvent::StanceSwap",
    }
}

fn render_ui_value_binding(binding: UiValueBinding) -> String {
    match binding {
        UiValueBinding::ConstantQ12(value) => {
            format!("LevelUiValueBinding::ConstantQ12({value})")
        }
        UiValueBinding::Option(option) => {
            format!(
                "LevelUiValueBinding::Option({})",
                crate::playtest::cook_option_id(option)
            )
        }
        UiValueBinding::PlayerStanceActiveHealth => {
            "LevelUiValueBinding::PlayerStanceActiveHealth".to_string()
        }
        UiValueBinding::PlayerStanceActiveHealthMax => {
            "LevelUiValueBinding::PlayerStanceActiveHealthMax".to_string()
        }
        UiValueBinding::PlayerStanceInactiveHealth => {
            "LevelUiValueBinding::PlayerStanceInactiveHealth".to_string()
        }
        UiValueBinding::PlayerStanceInactiveHealthMax => {
            "LevelUiValueBinding::PlayerStanceInactiveHealthMax".to_string()
        }
        UiValueBinding::PlayerStanceSwapProgress => {
            "LevelUiValueBinding::PlayerStanceSwapProgress".to_string()
        }
        UiValueBinding::PlayerStanceActiveIsZenith => {
            "LevelUiValueBinding::PlayerStanceActiveIsZenith".to_string()
        }
        UiValueBinding::PlayerStanceActiveBroken => {
            "LevelUiValueBinding::PlayerStanceActiveBroken".to_string()
        }
        UiValueBinding::PlayerStanceInactiveBroken => {
            "LevelUiValueBinding::PlayerStanceInactiveBroken".to_string()
        }
        UiValueBinding::TargetHealth => "LevelUiValueBinding::TargetHealth".to_string(),
        UiValueBinding::TargetHealthMax => "LevelUiValueBinding::TargetHealthMax".to_string(),
        UiValueBinding::TargetHealthSecondary => {
            "LevelUiValueBinding::TargetHealthSecondary".to_string()
        }
        UiValueBinding::TargetHealthSecondaryMax => {
            "LevelUiValueBinding::TargetHealthSecondaryMax".to_string()
        }
        UiValueBinding::TargetStanceSwapProgress => {
            "LevelUiValueBinding::TargetStanceSwapProgress".to_string()
        }
        UiValueBinding::TargetStanceActiveIsZenith => {
            "LevelUiValueBinding::TargetStanceActiveIsZenith".to_string()
        }
        UiValueBinding::PlayerHealth => "LevelUiValueBinding::PlayerHealth".to_string(),
        UiValueBinding::PlayerHealthMax => "LevelUiValueBinding::PlayerHealthMax".to_string(),
        UiValueBinding::PlayerHealthSecondary => {
            "LevelUiValueBinding::PlayerHealthSecondary".to_string()
        }
        UiValueBinding::PlayerHealthSecondaryMax => {
            "LevelUiValueBinding::PlayerHealthSecondaryMax".to_string()
        }
        UiValueBinding::PlayerHealthEmptyInfluence => {
            "LevelUiValueBinding::PlayerHealthEmptyInfluence".to_string()
        }
        UiValueBinding::PlayerHealthFullInfluence => {
            "LevelUiValueBinding::PlayerHealthFullInfluence".to_string()
        }
        UiValueBinding::PlayerHealthSecondaryEmptyInfluence => {
            "LevelUiValueBinding::PlayerHealthSecondaryEmptyInfluence".to_string()
        }
        UiValueBinding::PlayerHealthSecondaryFullInfluence => {
            "LevelUiValueBinding::PlayerHealthSecondaryFullInfluence".to_string()
        }
        UiValueBinding::PlayerStamina => "LevelUiValueBinding::PlayerStamina".to_string(),
        UiValueBinding::PlayerStaminaMax => "LevelUiValueBinding::PlayerStaminaMax".to_string(),
        UiValueBinding::LoadingProgress => "LevelUiValueBinding::LoadingProgress".to_string(),
    }
}

fn render_box_prop_texture_assets(
    texture_assets: &[Option<usize>; psx_level::BOX_PROP_FACE_COUNT],
) -> String {
    let mut out = String::from("[");
    for (index, texture_asset) in texture_assets.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        match texture_asset {
            Some(asset) => {
                let _ = write!(out, "Some(AssetId({asset}))");
            }
            None => out.push_str("None"),
        }
    }
    out.push(']');
    out
}

fn render_cylinder_prop_texture_assets(
    texture_assets: &[Option<usize>; psx_level::CYLINDER_PROP_MATERIAL_COUNT],
) -> String {
    let mut out = String::from("[");
    for (index, texture_asset) in texture_assets.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        match texture_asset {
            Some(asset) => {
                let _ = write!(out, "Some(AssetId({asset}))");
            }
            None => out.push_str("None"),
        }
    }
    out.push(']');
    out
}

fn render_arch_prop_texture_assets(
    texture_assets: &[Option<usize>; psx_level::ARCH_PROP_MATERIAL_COUNT],
) -> String {
    let mut out = String::from("[");
    for (index, texture_asset) in texture_assets.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        match texture_asset {
            Some(asset) => {
                let _ = write!(out, "Some(AssetId({asset}))");
            }
            None => out.push_str("None"),
        }
    }
    out.push(']');
    out
}

fn render_box_prop_vertices(vertices: &[[i16; 3]; psx_level::BOX_PROP_VERTEX_COUNT]) -> String {
    let mut out = String::from("[");
    for (index, vertex) in vertices.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "[{}, {}, {}]", vertex[0], vertex[1], vertex[2]);
    }
    out.push(']');
    out
}

fn render_box_prop_tint_rgb(tint_rgb: &[[u8; 3]; psx_level::BOX_PROP_FACE_COUNT]) -> String {
    let mut out = String::from("[");
    for (index, tint) in tint_rgb.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "[{}, {}, {}]", tint[0], tint[1], tint[2]);
    }
    out.push(']');
    out
}

fn render_box_prop_baked_vertex_rgb(
    baked: &[[(u8, u8, u8); 4]; psx_level::BOX_PROP_FACE_COUNT],
) -> String {
    let mut out = String::from("[");
    for (face_index, face) in baked.iter().enumerate() {
        if face_index > 0 {
            out.push_str(", ");
        }
        out.push('[');
        for (vertex_index, rgb) in face.iter().enumerate() {
            if vertex_index > 0 {
                out.push_str(", ");
            }
            let _ = write!(out, "({}, {}, {})", rgb.0, rgb.1, rgb.2);
        }
        out.push(']');
    }
    out.push(']');
    out
}

const fn material_flags_for_sidedness(sidedness: crate::MaterialFaceSidedness) -> u16 {
    match sidedness {
        crate::MaterialFaceSidedness::Front => 0,
        crate::MaterialFaceSidedness::Back => 1,
        crate::MaterialFaceSidedness::Both => 2,
    }
}

fn model_material_flags(material: &PlaytestModelMaterialOverride) -> u16 {
    let flags = material_flags_for_sidedness(material.face_sidedness);
    flags | reflection_material_flags(material.reflection_probe)
}

fn reflection_material_flags(reflection: Option<crate::ReflectionProbeMaterial>) -> u16 {
    let mut flags = 0;
    if let Some(reflection) = reflection {
        let roughness_level = u16::from(reflection.roughness >> 6).min(3);
        flags |= psx_level::material_flags::MODEL_REFLECTION_PROBE;
        if reflection.facet_normals {
            flags |= psx_level::material_flags::MODEL_REFLECTION_FACET_NORMALS;
        }
        flags |= roughness_level << psx_level::material_flags::MODEL_REFLECTION_ROUGHNESS_SHIFT;
        flags |= u16::from(reflection.strength)
            << psx_level::material_flags::MODEL_REFLECTION_STRENGTH_SHIFT;
    }
    flags
}

/// Numeric `psx_level::model_override_blend` code for an authored
/// blend mode.
pub(crate) const fn model_override_blend_code(blend_mode: crate::PsxBlendMode) -> u8 {
    match blend_mode {
        crate::PsxBlendMode::Opaque => 0,
        crate::PsxBlendMode::Average => 1,
        crate::PsxBlendMode::Add => 2,
        crate::PsxBlendMode::Subtract => 3,
        crate::PsxBlendMode::AddQuarter => 4,
    }
}

/// `Option<LevelModelMaterialOverride>` literal for the instance and
/// character writers.
fn model_material_override_literal(
    material_override: &Option<PlaytestModelMaterialOverride>,
) -> String {
    match material_override {
        Some(o) => {
            let texture_asset = o
                .texture_asset_index
                .map(|index| format!("Some(AssetId({index}))"))
                .unwrap_or_else(|| "None".to_string());
            let secondary_layer = o.secondary_layer.map_or_else(
                || "None".to_string(),
                |layer| {
                    let texture_asset = layer
                        .texture_asset_index
                        .map(|index| format!("Some(AssetId({index}))"))
                        .unwrap_or_else(|| "None".to_string());
                    format!(
                        "Some(LevelModelSecondaryLayer {{ texture_asset: {texture_asset}, blend_mode: {}, tint_rgb: [{}, {}, {}], motion: LevelMaterialUvMotion {{ enabled: {}, speed_u_q8: {}, speed_v_q8: {}, phase_u: {}, phase_v: {} }}, flags: {} }})",
                        model_override_blend_code(layer.blend_mode),
                        layer.tint_rgb[0],
                        layer.tint_rgb[1],
                        layer.tint_rgb[2],
                        layer.motion.enabled,
                        layer.motion.speed_u_q8,
                        layer.motion.speed_v_q8,
                        layer.motion.phase_u,
                        layer.motion.phase_v,
                        reflection_material_flags(layer.reflection_probe),
                    )
                },
            );
            format!(
                "Some(LevelModelMaterialOverride {{ texture_asset: {texture_asset}, blend_mode: {}, tint_rgb: [{}, {}, {}], motion: LevelMaterialUvMotion {{ enabled: {}, speed_u_q8: {}, speed_v_q8: {}, phase_u: {}, phase_v: {} }}, secondary_layer: {secondary_layer}, flags: {} }})",
                model_override_blend_code(o.blend_mode),
                o.tint_rgb[0],
                o.tint_rgb[1],
                o.tint_rgb[2],
                o.motion.enabled,
                o.motion.speed_u_q8,
                o.motion.speed_v_q8,
                o.motion.phase_u,
                o.motion.phase_v,
                model_material_flags(o),
            )
        }
        None => "None".to_string(),
    }
}

/// Default destination for the playtest example's generated
/// directory. Anchored at the editor crate's manifest dir so the
/// dev workflow finds it regardless of cwd.
pub fn default_generated_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("engine")
        .join("examples")
        .join("editor-playtest")
        .join(GENERATED_DIRNAME)
}

/// One-shot cook + write entry point: validate, package, drop
/// the result at `generated_dir`. Resolves relative texture
/// paths through `project_root`. Returns the validation report;
/// callers must check `report.is_ok()` before assuming the
/// files were written.
pub fn cook_to_dir(
    project: &ProjectDocument,
    project_root: &Path,
    generated_dir: &Path,
) -> std::io::Result<PlaytestValidationReport> {
    let (package, report) = build_package(project, project_root);
    write_cook_result(package.as_ref(), generated_dir)?;
    Ok(report)
}

/// Write an already-built playtest package without rebuilding it. This keeps
/// interactive Play callers from paying for topology, world, material, model,
/// and animation cooking twice merely to produce their status summary.
pub fn write_cook_result(
    package: Option<&PlaytestPackage>,
    generated_dir: &Path,
) -> std::io::Result<()> {
    // A failed cook must not leave a stale cooked manifest for subsequent runtime
    // builds: `failed_cook_removes_stale_cooked_manifest` pins that, and it is the
    // right call, because building against the manifest of a level you just failed
    // to cook would silently run the wrong world.
    //
    // It has a sharp edge worth knowing about. `generated_dir` holds one project at
    // a time and every project shares it, so a failed cook of project B also
    // discards project A's cooked output and A's disc must be rebuilt. Keeping A's
    // manifest would be worse, since the next runtime build would silently be A.
    // Removing the edge means making the generated directory per project rather
    // than picking between the two failure modes; see the streaming audit.
    let cooked_manifest = generated_dir.join(COOKED_MANIFEST_FILENAME);
    if cooked_manifest.exists() {
        std::fs::remove_file(&cooked_manifest)?;
    }
    // Remove the pre-unification brush-only manifest if an older checkout
    // generated one. New cooks always select `level_manifest.cooked.rs`.
    let brush_manifest = generated_dir.join("brush_manifest.cooked.rs");
    if brush_manifest.exists() {
        std::fs::remove_file(&brush_manifest)?;
    }
    if let Some(package) = package {
        write_package(package, generated_dir)?;
    }
    Ok(())
}

/// Resolve the per-asset `static` name for the include_bytes
/// statement. The asset index is part of the symbol because
/// model folders intentionally reuse generic filenames such as
/// `mesh.psxmdl` and `atlas.psxt`.
fn asset_static_name(asset: &PlaytestAsset, index: usize) -> String {
    let stem = Path::new(&asset.filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&asset.filename);
    format!("ASSET_{index:03}_{}_BYTES", stem.to_ascii_uppercase())
}

fn ui_sfx_sample_static_name(index: usize) -> String {
    format!("UI_SFX_SAMPLE_{index:03}_BYTES")
}

fn purge_directory_files(dir: &Path, ext: &str) -> std::io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some(ext) {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// Purge stale per-model subfolders inside `generated/models/`.
/// Each cook re-creates `model_NNN_<safe>/` folders from scratch,
/// so the simplest safe behaviour is to remove every immediate
/// subdirectory before writing.
fn purge_models_dir(dir: &Path) -> std::io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            purge_generated_tree(&path)?;
        }
    }
    Ok(())
}

fn purge_generated_tree(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let child = entry.path();
        if child.is_dir() {
            purge_generated_tree(&child)?;
        } else {
            std::fs::remove_file(&child)?;
        }
    }
    match std::fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
            std::fs::remove_dir_all(path)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
#[cfg(test)]
mod tests;

#[cfg(test)]
fn test_wav_mono_44k(samples: &[i16]) -> Vec<u8> {
    let data_len = samples.len() as u32 * 2;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&44_100u32.to_le_bytes());
    out.extend_from_slice(&(44_100u32 * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

/// Header emitted at the top of every generated manifest. The
/// runtime example wraps the `include!` in a `mod generated`
/// with `#[allow(dead_code)]` on the wrapper, so we don't
/// repeat that here (would be an inner attribute on the wrong
/// item).
const MANIFEST_HEADER: &str = "\
// Generated by `psxed_project::playtest::write_package` --
// do not edit by hand. Regenerate with the editor's
// Play action or the `cook-playtest` CLI.

use psx_level::{
    asset_flags,
    AssetId,
    AssetKind,
    BoostModuleRecord,
    CHARACTER_CLIP_NONE,
    CharacterActionFrameRange,
    CharacterActionPush,
    CharacterAnimationAction,
    CharacterIndex,
    CombatCapsuleIndex,
    CombatCapsuleRecord,
    EntityKind,
    EntityRecord,
    EquipmentRecord,
    FlowState,
    GameFlow,
    InteractableKind,
    InteractableMessageRecord,
    InteractableRecord,
    LevelVitalityCircleRecord,
    LevelAssetRecord,
    LevelBoxPropRecord,
    LevelBoxPropSurfaceRecord,
    LevelCylinderPropRecord,
    LevelCylinderPropSurfaceRecord,
    LevelArchPropRecord,
    LevelArchPropSurfaceRecord,
    LevelArchPropCollisionRecord,
    LevelCameraRecord,
    LevelCameraProfile,
    LevelCloudLayerRecord,
    LevelCharacterRecord,
    LevelGameEntityRecord,
    LevelGameplaySfxCueRecord,
    LevelGameplaySfxEvent,
    LevelLogicRecord,
    LevelCycloramaQuadRecord,
    LevelFarVistaRecord,
    LevelImagePropRecord,
    LevelDestructibleRecord,
    LevelWorldObjectRecord,
    LevelMaterialUvMotion,
    LevelModelClipBoundsRecord,
    LevelModelClipRecord,
    LevelModelFrameBoundsRecord,
    LevelModelInstanceRecord,
    LevelModelMaterialOverride,
    LevelModelRecord,
    LevelModelSecondaryLayer,
    LevelModelSocketRecord,
    LevelOptionDef,
    LevelRoomRecord,
    LevelSceneState,
    LevelSkyRecord,
    LevelTransition,
    LevelTransitionKind,
    LevelUiAction,
    LevelUiFocusEffect,
    LevelUiFocusStyle,
    LevelUiGradientDirection,
    LevelUiImageEffect,
    LevelUiNodeKind,
    LevelUiNodeRecord,
    LevelUiPaintRecord,
    LevelUiScene,
    LevelUiSfxCueRecord,
    LevelUiSfxEvent,
    LevelUiSfxSampleRecord,
    LevelUiValueBinding,
    LevelWorldLayer,
    LevelWeaponRecord,
    LevelWorldPackEntryRecord,
    MODEL_CLIP_INHERIT,
    ModelClipIndex,
    ModelClipTableIndex,
    ModelFrameBoundsIndex,
    ModelIndex,
    ModelSocketIndex,
    ParticleEmitterRecord,
    PlayerControllerRecord,
    PlayerSpawnRecord,
    PointLightRecord,
    OptionalModelClipIndex,
    ResourceSlot,
    RoomIndex,
    UiNodeIndex,
    WeaponHitboxIndex,
    WeaponHitboxRecord,
    WeaponAppearanceRecord,
    WeaponHitShapeRecord,
    WeaponIndex,
};

";
