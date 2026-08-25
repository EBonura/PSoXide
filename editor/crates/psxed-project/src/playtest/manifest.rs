//! Manifest and generated-asset writer for editor-playtest.

use std::fmt::Write as _;

use super::*;
use crate::{UiFontChoice, UiGradientDirection, UiImageEffect, UiNodeKind, UiValueBinding};

const STREAMED_ROOM_SLOT_BYTES: usize = 32 * 1024;
const CD_SECTOR_BYTES: usize = 2048;

fn remove_optional_file(path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Refuse a package whose cooked room chunks cannot fit the runtime's per-room
/// CD slot.
///
/// The runtime slot is sized from `MAX_STREAMED_ROOM_CHUNK_BYTES`, so an
/// oversized chunk would fail every load silently and the room would never
/// appear. Reported for all offending rooms at once rather than one per cook,
/// since a large level tends to trip several.
fn validate_streamed_room_chunks(package: &PlaytestPackage) -> std::io::Result<()> {
    let mut offenders = Vec::new();
    for room_index in 0..package.rooms.len() {
        if package.rooms[room_index].world_asset_index.is_none() {
            continue;
        }
        let payload = streamed_room_chunk_payload(package, room_index as u16)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        if payload.len() > psx_level::MAX_STREAMED_ROOM_CHUNK_BYTES {
            offenders.push((room_index, payload.len()));
        }
    }
    if offenders.is_empty() {
        return Ok(());
    }
    let mut message = format!(
        "{} room chunk(s) exceed the {}-byte runtime room slot \
         (psx_level::MAX_STREAMED_ROOM_CHUNK_BYTES). The portal-room plan splits on \
         authored seams only and does not consult this budget, so a room with no \
         interior seam can cook to any size:",
        offenders.len(),
        psx_level::MAX_STREAMED_ROOM_CHUNK_BYTES,
    );
    for (room_index, len) in &offenders {
        let _ = write!(message, "\n  room {room_index}: {len} bytes");
    }
    let _ = write!(
        message,
        "\nAdd portal seams to split the offending room(s), or reduce their geometry. \
         Nothing was written; the previously generated output is intact."
    );
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message,
    ))
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
    validate_streamed_room_chunks(package)?;
    // Same discipline for the session-resident payloads. Without this the
    // ceiling is only reported by a MIPS link failure naming a section, long
    // after the cook that caused it.
    let resident = super::budget::validate_resident_assets(package)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if resident.near_cap() {
        println!(
            "warning: {} (over {}% of the ceiling){}",
            resident.summary(),
            crate::playtest::budget::PLAYTEST_RESIDENT_ASSET_WARN_PERCENT,
            resident.breakdown(6),
        );
    }

    let rooms_dir = generated_dir.join(ROOMS_DIRNAME);
    let stream_chunks_dir = generated_dir.join(STREAM_CHUNKS_DIRNAME);
    let ui_stream_chunks_dir = generated_dir.join(UI_STREAM_CHUNKS_DIRNAME);
    let textures_dir = generated_dir.join(TEXTURES_DIRNAME);
    let models_dir = generated_dir.join(MODELS_DIRNAME);
    let ui_sfx_dir = generated_dir.join(UI_SFX_DIRNAME);
    let cdda_tracks_dir = generated_dir.join(CDDA_TRACKS_DIRNAME);
    std::fs::create_dir_all(&rooms_dir)?;
    std::fs::create_dir_all(&stream_chunks_dir)?;
    std::fs::create_dir_all(&ui_stream_chunks_dir)?;
    std::fs::create_dir_all(&textures_dir)?;
    std::fs::create_dir_all(&models_dir)?;
    std::fs::create_dir_all(&ui_sfx_dir)?;
    std::fs::create_dir_all(&cdda_tracks_dir)?;
    purge_directory_files(&rooms_dir, "psxw")?;
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
    match &package.world_geometry {
        PlaytestWorldGeometry::Grid => {
            remove_optional_file(&pxbsp_path)?;
            remove_optional_file(&brush_leak_path)?;
        }
        PlaytestWorldGeometry::Pxbsp(world) => {
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
        }
    }

    for asset in &package.assets {
        // ModelMesh / ModelAnimation / model-folder Texture
        // asset filenames already include their `models/...`
        // subpath; rooms + room-only textures stay flat in
        // their respective dirs.
        let target = match asset.kind {
            PlaytestAssetKind::RoomWorld => rooms_dir.join(&asset.filename),
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
    for room_index in world_pack_order(package) {
        let payload = streamed_room_chunk_payload(package, room_index)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // Cook/runtime streaming contract: the runtime's per-room CD slot
        // is sized from this constant; an oversized chunk would fail every
        // runtime load silently and the room would never appear. Refuse to
        // ship it.
        // Already validated before any filesystem mutation; see
        // `validate_streamed_room_chunks`.
        debug_assert!(payload.len() <= psx_level::MAX_STREAMED_ROOM_CHUNK_BYTES);
        std::fs::write(
            stream_chunks_dir.join(streamed_room_chunk_filename(room_index)),
            payload,
        )?;
    }

    let manifest = render_manifest_source(package);
    std::fs::write(generated_dir.join(COOKED_MANIFEST_FILENAME), manifest)?;
    std::fs::write(
        generated_dir.join(WORLD_PACK_ORDER_FILENAME),
        render_world_pack_order(package),
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

fn write_cached_room_lighting_policy(
    out: &mut String,
    has_room_fog: bool,
    all_room_fog_is_black: bool,
) {
    if has_room_fog {
        let source = r#"
#[repr(transparent)]
pub struct ProjectCachedRoomLighting<'a> {
    lighting: &'a super::RuntimeRoomLighting,
}

impl<'a> ProjectCachedRoomLighting<'a> {
    #[inline(always)]
    pub const fn new(lighting: &'a super::RuntimeRoomLighting) -> Self {
        Self { lighting }
    }
}

impl psx_engine::WorldSurfaceLighting for ProjectCachedRoomLighting<'_> {
    #[inline(always)]
    fn shade(
        &self,
        sample: psx_engine::WorldSurfaceSample,
        material: psx_engine::WorldRenderMaterial,
    ) -> psx_engine::WorldRenderMaterial {
        psx_engine::WorldSurfaceLighting::shade(self.lighting, sample, material)
    }

    #[inline(always)]
    fn shade_vertex(
        &self,
        sample: psx_engine::WorldSurfaceSample,
        vertex: psx_engine::RoomPoint,
        material: psx_engine::WorldRenderMaterial,
    ) -> (u8, u8, u8) {
        psx_engine::WorldSurfaceLighting::shade_vertex(self.lighting, sample, vertex, material)
    }

    #[inline(always)]
    fn shade_vertices(
        &self,
        sample: psx_engine::WorldSurfaceSample,
        vertices: [psx_engine::WorldVertex; 4],
        material: psx_engine::WorldRenderMaterial,
    ) -> [(u8, u8, u8); 4] {
        psx_engine::WorldSurfaceLighting::shade_vertices(
            self.lighting,
            sample,
            vertices,
            material,
        )
    }

    #[inline(always)]
    fn shade_vertices_with_depths(
        &self,
        sample: psx_engine::WorldSurfaceSample,
        vertices: [psx_engine::WorldVertex; 4],
        depths: [i32; 4],
        material: psx_engine::WorldRenderMaterial,
    ) -> [(u8, u8, u8); 4] {
        psx_engine::WorldSurfaceLighting::shade_vertices_with_depths(
            self.lighting,
            sample,
            vertices,
            depths,
            material,
        )
    }

    #[inline(always)]
    fn shade_cached_baked_vertices(
        &self,
        sample: psx_engine::WorldSurfaceSample,
        depths: Option<[i32; 4]>,
        _material: psx_engine::WorldRenderMaterial,
    ) -> Option<[(u8, u8, u8); 4]> {
        let vertex_rgb = sample.baked_vertex_rgb?;
        if !self.lighting.fog_enabled || self.lighting.fog_far <= self.lighting.fog_near {
            return Some(vertex_rgb);
        }
        let depths = depths?;
        Some([
            self.lighting.apply_vertex_fog_weight(vertex_rgb[0], depths[0]),
            self.lighting.apply_vertex_fog_weight(vertex_rgb[1], depths[1]),
            self.lighting.apply_vertex_fog_weight(vertex_rgb[2], depths[2]),
            self.lighting.apply_vertex_fog_weight(vertex_rgb[3], depths[3]),
        ])
    }

    #[inline(always)]
    fn shade_prewarmed_baked_vertices(
        &self,
        sample: psx_engine::WorldSurfaceSample,
        depths: Option<[i32; 4]>,
    ) -> Option<[(u8, u8, u8); 4]> {
        let vertex_rgb = sample.baked_vertex_rgb?;
        if !self.lighting.fog_enabled || self.lighting.fog_far <= self.lighting.fog_near {
            return Some(vertex_rgb);
        }
        let depths = depths?;
        Some([
            self.lighting.apply_vertex_fog_weight(vertex_rgb[0], depths[0]),
            self.lighting.apply_vertex_fog_weight(vertex_rgb[1], depths[1]),
            self.lighting.apply_vertex_fog_weight(vertex_rgb[2], depths[2]),
            self.lighting.apply_vertex_fog_weight(vertex_rgb[3], depths[3]),
        ])
    }

    #[inline(always)]
    fn uses_direct_baked_vertex_rgb(&self) -> bool {
        psx_engine::WorldSurfaceLighting::uses_direct_baked_vertex_rgb(self.lighting)
    }

    #[inline(always)]
    fn prepare_vertex_depth(&self, depth: i32) -> i32 {
        psx_engine::WorldSurfaceLighting::prepare_vertex_depth(self.lighting, depth)
    }

    #[inline(always)]
    fn uses_vertex_depths(&self) -> bool {
        psx_engine::WorldSurfaceLighting::uses_vertex_depths(self.lighting)
    }

    #[inline(always)]
    fn needs_surface_sample_center(&self, sample_has_baked_rgb: bool) -> bool {
        psx_engine::WorldSurfaceLighting::needs_surface_sample_center(
            self.lighting,
            sample_has_baked_rgb,
        )
    }
}

macro_rules! draw_project_cached_room {
    (
        $lighting:expr,
        $draw:path,
        [$($before:expr),* $(,)?],
        [$($after:expr),* $(,)?]
    ) => {{
        let cached_lighting = $crate::generated::ProjectCachedRoomLighting::new($lighting);
        $draw($($before,)* &cached_lighting, true, $($after,)*)
    }};
}
pub(crate) use draw_project_cached_room;

"#;
        if all_room_fog_is_black {
            out.push_str(&source.replace(
                "self.lighting.apply_vertex_fog_weight",
                "psx_game_runtime::room_lighting::apply_black_room_fog_weight",
            ));
        } else {
            out.push_str(source);
        }
    } else {
        out.push_str(
            r#"
macro_rules! draw_project_cached_room {
    (
        $lighting:expr,
        $draw:path,
        [$($before:expr),* $(,)?],
        [$($after:expr),* $(,)?]
    ) => {
        $draw($($before,)* $lighting, false, $($after,)*)
    };
}
pub(crate) use draw_project_cached_room;

"#,
        );
    }
}

pub fn render_manifest_source(package: &PlaytestPackage) -> String {
    let mut out = String::new();
    out.push_str(MANIFEST_HEADER);
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
    let has_room_fog = package
        .rooms
        .iter()
        .any(|room| room.flags & psx_level::room_flags::FOG_ENABLED != 0);
    let all_room_fog_is_black = has_room_fog
        && package.rooms.iter().all(|room| {
            room.flags & psx_level::room_flags::FOG_ENABLED == 0 || room.fog_rgb == [0, 0, 0]
        });
    write_cached_room_lighting_policy(&mut out, has_room_fog, all_room_fog_is_black);
    let world_pack_toc = world_pack_toc(package);
    let world_pack_max_chunk_bytes = world_pack_toc
        .iter()
        .map(|entry| entry.byte_size as usize)
        .max()
        .unwrap_or(0);
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
    let gameplay_pack_max_chunk_bytes =
        streamed_class_max_chunk_bytes(package, StreamedClass::Gameplay);
    let persistent_asset_slot_count = package.assets.len().max(1);
    let persistent_asset_page_count = package
        .assets
        .iter()
        .filter(|asset| asset.streamed_class == StreamedClass::PersistentGameplay)
        .map(|asset| asset.bytes.len().next_multiple_of(4))
        .sum::<usize>()
        .div_ceil(CD_SECTOR_BYTES)
        .max(1);
    let resident_chunk_limit = package
        .rooms
        .iter()
        .map(|room| room.resident_chunk_limit as usize)
        .max()
        .unwrap_or(crate::MIN_WORLD_STREAMING_RESIDENT_CHUNKS as usize)
        .clamp(
            crate::MIN_WORLD_STREAMING_RESIDENT_CHUNKS as usize,
            crate::MAX_WORLD_STREAMING_RESIDENT_CHUNKS as usize,
        );
    // Keep two independently evictable look-ahead rooms beyond the authored
    // protected window. RAM is budgeted in exact 2 KiB disc pages, using the
    // largest possible combination of simultaneously resident chunks; this is
    // a cook-time proof that any runtime choice of this many rooms fits.
    let world_stream_slot_count = resident_chunk_limit
        .saturating_add(2)
        .min(world_pack_toc.len())
        .max(1);
    let mut world_chunk_sector_counts = world_pack_toc
        .iter()
        .map(|entry| entry.sector_count as usize)
        .collect::<Vec<_>>();
    world_chunk_sector_counts.sort_unstable_by(|a, b| b.cmp(a));
    let world_resident_page_count = world_chunk_sector_counts
        .iter()
        .take(world_stream_slot_count)
        .copied()
        .sum::<usize>()
        .max(1);
    let _ = writeln!(
        out,
        "pub const WORLD_RESIDENT_CHUNK_LIMIT: usize = {resident_chunk_limit};\n",
    );
    let _ = writeln!(
        out,
        "pub const WORLD_PACK_MAX_CHUNK_BYTES: usize = {world_pack_max_chunk_bytes};\n",
    );
    let _ = writeln!(
        out,
        "pub const WORLD_STREAM_SLOT_COUNT: usize = {world_stream_slot_count};\n",
    );
    let _ = writeln!(
        out,
        "pub const WORLD_RESIDENT_PAGE_COUNT: usize = {world_resident_page_count};\n",
    );
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
    let runtime_depth_sort_mode = package.runtime_depth_sort_mode.manifest_value();
    let _ = writeln!(
        out,
        "pub const CACHED_ROOM_DEPTH_MODE: u8 = {runtime_depth_sort_mode};\n",
    );
    let runtime_texture_split_mode = package.runtime_texture_split_mode.manifest_value();
    let _ = writeln!(
        out,
        "pub const CACHED_ROOM_TEXTURE_SPLIT_MODE: u8 = {runtime_texture_split_mode};\n",
    );
    let runtime_room_draw_order_mode = package.runtime_room_draw_order_mode.manifest_value();
    let _ = writeln!(
        out,
        "pub const CACHED_ROOM_DRAW_ORDER_MODE: u8 = {runtime_room_draw_order_mode};\n",
    );
    let runtime_texture_split_max_edge = package.runtime_texture_split_max_edge;
    let _ = writeln!(
        out,
        "pub const CACHED_ROOM_TEXTURE_SPLIT_MAX_EDGE: u16 = {runtime_texture_split_max_edge};\n",
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

    match &package.world_geometry {
        PlaytestWorldGeometry::Grid => {
            out.push_str("pub const PLAYTEST_USES_PXBSP: bool = false;\n");
            out.push_str("pub const PXBSP_AMBIENT_RGB: [u8; 3] = [0; 3];\n");
            out.push_str("pub const PXBSP_FACE_CHAIN_CAPACITY: usize = 0;\n");
            out.push_str("pub static PXBSP_WORLD: &[u8] = &[];\n");
            out.push_str("pub static PXBSP_MOVER_NODE_IDS: &[u32] = &[];\n");
            out.push_str("pub static PXBSP_MOVER_MODEL_INDICES: &[u16] = &[];\n");
            out.push_str(
                "pub static PXBSP_BODY_HULLS: &[psx_bsp::collision_provider::CookedBodyHull] = &[];\n\n",
            );
        }
        PlaytestWorldGeometry::Pxbsp(world) => {
            out.push_str("pub const PLAYTEST_USES_PXBSP: bool = true;\n");
            // Keep actor/prop lighting on the same ambient contract used by
            // the brush light bake. PXBSP surfaces carry baked vertex light;
            // dynamic world content consumes this generated constant instead
            // of depending on a synthetic PSXW room header.
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
        }
    }

    // Emit one named static per asset so the include_bytes! call
    // sites are easy to grep for. Asset records reference these
    // statics so the slice is still constructible at compile time.
    for (i, asset) in package.assets.iter().enumerate() {
        let include_path = match asset.kind {
            PlaytestAssetKind::RoomWorld => format!("{ROOMS_DIRNAME}/{}", asset.filename),
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
        // Room worlds always stream off WORLD.PAK; streamed Texture
        // assets (UI images) stream off UI.PAK. Both emit empty baked
        // bytes under `cd-stream-bench` and the normal word-aligned
        // `include_bytes!` static when the feature is off.
        if asset.kind == PlaytestAssetKind::RoomWorld || asset.is_streamed() {
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
    for (i, sample) in package.ui_sfx_samples.iter().enumerate() {
        let static_name = ui_sfx_sample_static_name(i);
        let _ = writeln!(out, "/// {static_name} - {}", sample.source_path);
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
            PlaytestAssetKind::RoomWorld => "AssetKind::RoomWorld",
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

    out.push_str("/// Per-room material bindings - slot the `.psxw` stores → texture asset.\n");
    out.push_str("pub static MATERIALS: &[LevelMaterialRecord] = &[\n");
    for material in &package.materials {
        let flags = material_flags_for_sidedness(material.face_sidedness);
        let animation = level_material_animation_literal(material.animation);
        let _ = writeln!(
            out,
            "    LevelMaterialRecord {{ room: RoomIndex({}), local_slot: MaterialSlot({}), texture_asset: AssetId({}), tint_rgb: [{}, {}, {}], blend_mode: {}, animation: {}, flags: {} }},",
            material.room,
            material.local_slot,
            material.texture_asset_index,
            material.tint_rgb[0],
            material.tint_rgb[1],
            material.tint_rgb[2],
            model_override_blend_code(material.blend_mode),
            animation,
            flags,
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
            "    LevelRoomRecord {{ name: {:?}, world_asset: AssetId({}), origin_x: {}, origin_z: {}, origin_y: {}, sector_size: {}, draw_distance: {}, chunk_activation_radius_sectors: {}, visibility_radius: {}, resident_chunk_limit: {}, visible_chunk_limit: {}, gravity_per_tick: {}, material_first: MaterialIndex({}), material_count: {}, portal_first: {}, portal_count: {}, near_room_first: {}, near_room_count: {}, overlapped_room_first: {}, overlapped_room_count: {}, fog_rgb: [{}, {}, {}], fog_near: {}, fog_far: {}, atmosphere_rgb: [{}, {}, {}], atmosphere_density: {}, atmosphere_fall_speed_q4: {}, atmosphere_wind_speed_q4: {}, sky: LevelSkyRecord {{ top_rgb: [{}, {}, {}], horizon_rgb: [{}, {}, {}], bottom_rgb: [{}, {}, {}], horizon_percent: {}, horizon_thickness_percent: {}, skybox_columns: {}, skybox_rows: {}, flags: {}, cyclorama_quads: {}, cloud_layer: LevelCloudLayerRecord {{ texture_asset: AssetId({}), color_rgb: [{}, {}, {}], density: {}, altitude: {}, extent: {}, tile_count: {}, scroll_speed: [{}, {}], noise_seed: 0x{:08x}, flags: {} }} }}, far_vista: LevelFarVistaRecord {{ texture_assets: {}, radius: {}, height: {}, vertical_offset: {}, segments: {}, rotation_degrees: {}, tint_rgb: [{}, {}, {}], flags: {} }}, camera: LevelCameraRecord {{ distance: {}, height: {}, target_height: {}, lock_rise_percent: {}, min_floor_clearance: {}, orbit_speed_level: {}, position_lag_shift: {}, focus_lag_shift: {}, distance_lag_shift: {} }}, flags: {} }},",
            room.name,
            room.world_asset_index
                .unwrap_or(usize::from(u16::MAX)),
            room.origin_x,
            room.origin_z,
            room.origin_y,
            room.sector_size,
            room.draw_distance,
            room.chunk_activation_radius_sectors,
            room.visibility_radius,
            room.resident_chunk_limit,
            room.visible_chunk_limit,
            room.gravity_per_tick,
            room.material_first,
            room.material_count,
            room.portal_first,
            room.portal_count,
            room.near_room_first,
            room.near_room_count,
            room.overlapped_room_first,
            room.overlapped_room_count,
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
            room.camera.position_lag_shift,
            room.camera.focus_lag_shift,
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

    out.push_str("/// Cooked runtime chunk metadata.\n");
    out.push_str("pub static ROOM_CHUNKS: &[LevelChunkRecord] = &[\n");
    for chunk in &package.chunks {
        let [north, east, south, west] = chunk.neighbours;
        let _ = writeln!(
            out,
            "    LevelChunkRecord {{ room: RoomIndex({}), authored_room: {}, chunk_index: {}, origin_x: {}, origin_z: {}, width: {}, depth: {}, neighbours: LevelChunkNeighbours {{ north: {}, east: {}, south: {}, west: {} }}, flags: {} }},",
            chunk.room,
            chunk.authored_room,
            chunk.chunk_index,
            chunk.origin_x,
            chunk.origin_z,
            chunk.width,
            chunk.depth,
            room_index_or_none(north),
            room_index_or_none(east),
            room_index_or_none(south),
            room_index_or_none(west),
            chunk.flags,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Directed runtime room portal graph.\n");
    out.push_str("pub static ROOM_PORTALS: &[LevelRoomPortalRecord] = &[\n");
    for portal in &package.room_portals {
        let _ = writeln!(
            out,
            "    LevelRoomPortalRecord {{ source_room: RoomIndex({}), destination_room: RoomIndex({}), kind: {}, normal_x: {}, normal_y: {}, normal_z: {}, vertex_x: [{}, {}, {}, {}], vertex_y: [{}, {}, {}, {}], vertex_z: [{}, {}, {}, {}] }},",
            portal.source_room,
            portal.destination_room,
            portal.kind,
            portal.normal[0],
            portal.normal[1],
            portal.normal[2],
            portal.vertices[0][0],
            portal.vertices[1][0],
            portal.vertices[2][0],
            portal.vertices[3][0],
            portal.vertices[0][1],
            portal.vertices[1][1],
            portal.vertices[2][1],
            portal.vertices[3][1],
            portal.vertices[0][2],
            portal.vertices[1][2],
            portal.vertices[2][2],
            portal.vertices[3][2],
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Water-covered runtime sectors, sorted by room/x/z.\n");
    out.push_str("pub static WATER_CELLS: &[LevelWaterCellRecord] = &[\n");
    for water in &package.water_cells {
        let texture_asset = water
            .texture_asset_index
            .map(|asset| format!("Some(AssetId({asset}))"))
            .unwrap_or_else(|| "None".to_string());
        let animation = level_material_animation_literal(water.animation);
        let _ = writeln!(
            out,
            "    LevelWaterCellRecord {{ room: RoomIndex({}), x: {}, z: {}, texture_asset: {texture_asset}, blend_mode: {}, tint_rgb: {:?}, animation: {animation}, surface_y: {}, depth: {}, lethal_depth: {}, movement_percent: {}, death_delay_ticks: {}, death_submerge_depth: {} }},",
            water.room,
            water.x,
            water.z,
            water.blend_mode,
            water.tint_rgb,
            water.surface_y,
            water.depth,
            water.lethal_depth,
            water.movement_percent,
            water.death_delay_ticks,
            water.death_submerge_depth,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Room indices near each runtime room, reserved for portal streaming.\n");
    out.push_str("pub static ROOM_NEAR_ROOMS: &[RoomIndex] = &[\n");
    for room in &package.room_near_rooms {
        let _ = writeln!(out, "    RoomIndex({room}),");
    }
    out.push_str("];\n\n");

    out.push_str("/// Room indices overlapping each runtime room, reserved for stacked rooms.\n");
    out.push_str("pub static ROOM_OVERLAPPED_ROOMS: &[RoomIndex] = &[\n");
    for room in &package.room_overlapped_rooms {
        let _ = writeln!(out, "    RoomIndex({room}),");
    }
    out.push_str("];\n\n");

    out.push_str("/// Absolute disc LBA where WORLD.PAK starts in the playtest ISO layout.\n");
    let _ = writeln!(
        out,
        "pub const WORLD_PACK_START_LBA: u32 = {};",
        psx_iso::WORLD_PACK_DEFAULT_START_LBA
    );
    out.push('\n');

    out.push_str(
        "/// Cooked WORLD.PAK room table generated from the same layout as the ISO packer.\n",
    );
    out.push_str("pub static WORLD_PACK_TOC: &[LevelWorldPackEntryRecord] = &[\n");
    for entry in &world_pack_toc {
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

    out.push_str("/// Per-room visibility slices.\n");
    out.push_str("pub static ROOM_VISIBILITY: &[LevelRoomVisibilityRecord] = &[\n");
    for visibility in &package.room_visibility {
        let _ = writeln!(
            out,
            "    LevelRoomVisibilityRecord {{ room: RoomIndex({}), cell_first: VisibilityCellIndex({}), cell_count: {}, pvs_first: {}, pvs_count: {}, flags: 0 }},",
            visibility.room,
            visibility.cell_first,
            visibility.cell_count,
            visibility.pvs_first,
            visibility.pvs_count,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Cooked position-cell PVS bitset slices.\n");
    out.push_str("pub static VISIBILITY_PVS: &[LevelVisibilityPvsRecord] = &[\n");
    for pvs in &package.visibility_pvs {
        let _ = writeln!(
            out,
            "    LevelVisibilityPvsRecord {{ byte_first: {}, byte_count: {}, flags: 0 }},",
            pvs.byte_first, pvs.byte_count,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Cooked position-cell PVS bitset bytes.\n");
    out.push_str("pub static VISIBILITY_PVS_BITS: &[u8] = &[\n");
    for byte in &package.visibility_pvs_bits {
        let _ = writeln!(out, "    {},", byte);
    }
    out.push_str("];\n\n");

    out.push_str("/// Cooked grid-cell visibility metadata.\n");
    out.push_str("pub static VISIBILITY_CELLS: &[LevelVisibilityCellRecord] = &[\n");
    for cell in &package.visibility_cells {
        let _ = writeln!(
            out,
            "    LevelVisibilityCellRecord {{ room: RoomIndex({}), x: {}, z: {}, min_y: {}, max_y: {}, portal_mask: {}, blocker_mask: {}, cache_cell_index: {}, flags: {} }},",
            cell.room,
            cell.x,
            cell.z,
            cell.min_y,
            cell.max_y,
            cell.portal_mask,
            cell.blocker_mask,
            cell.cache_cell_index,
            cell.flags,
        );
    }
    out.push_str("];\n\n");

    out.push_str("#[cfg(feature = \"cd-stream-bench\")]\n");
    out.push_str("/// Stream builds read room-surface cache slices from `.psxc` chunks.\n");
    out.push_str("pub static ROOM_SURFACE_CACHES: &[LevelRoomSurfaceCacheRecord] = &[];\n\n");
    out.push_str("#[cfg(not(feature = \"cd-stream-bench\"))]\n");
    out.push_str("/// Per-room generated room-surface cache slices.\n");
    out.push_str("pub static ROOM_SURFACE_CACHES: &[LevelRoomSurfaceCacheRecord] = &[\n");
    for cache in &package.room_surface_caches {
        let _ = writeln!(
            out,
            "    LevelRoomSurfaceCacheRecord {{ room: RoomIndex({}), cell_first: {}, cell_count: {}, cell_vertex_first: {}, cell_vertex_count: {}, vertex_first: {}, vertex_count: {}, surface_first: {}, surface_count: {}, flags: 0 }},",
            cache.room,
            cache.cell_first,
            cache.cell_count,
            cache.cell_vertex_first,
            cache.cell_vertex_count,
            cache.vertex_first,
            cache.vertex_count,
            cache.surface_first,
            cache.surface_count,
        );
    }
    out.push_str("];\n\n");

    out.push_str("#[cfg(feature = \"cd-stream-bench\")]\n");
    out.push_str("/// Stream builds read cached room cells from `.psxc` chunks.\n");
    out.push_str("pub static ROOM_CACHE_CELLS: &[LevelCachedRoomCellRecord] = &[];\n\n");
    out.push_str("#[cfg(not(feature = \"cd-stream-bench\"))]\n");
    out.push_str("/// Generated cached room cells.\n");
    out.push_str("pub static ROOM_CACHE_CELLS: &[LevelCachedRoomCellRecord] = &[\n");
    for cell in &package.room_cache_cells {
        let _ = writeln!(
            out,
            "    LevelCachedRoomCellRecord {{ x: {}, z: {}, min_y: {}, max_y: {}, visibility_center: [{}, {}, {}], visibility_radius: {}, surface_first: {}, surface_count: {}, vertex_first: {}, vertex_count: {} }},",
            cell.x,
            cell.z,
            cell.min_y,
            cell.max_y,
            cell.visibility_center[0],
            cell.visibility_center[1],
            cell.visibility_center[2],
            cell.visibility_radius,
            cell.surface_first,
            cell.surface_count,
            cell.vertex_first,
            cell.vertex_count,
        );
    }
    out.push_str("];\n\n");

    out.push_str("#[cfg(feature = \"cd-stream-bench\")]\n");
    out.push_str("/// Stream builds read cached cell vertex indices from `.psxc` chunks.\n");
    out.push_str("pub static ROOM_CACHE_CELL_VERTICES: &[u16] = &[];\n\n");
    out.push_str("#[cfg(not(feature = \"cd-stream-bench\"))]\n");
    out.push_str("/// Generated cached cell vertex indices.\n");
    out.push_str("pub static ROOM_CACHE_CELL_VERTICES: &[u16] = &[\n");
    for vertex_index in &package.room_cache_cell_vertices {
        let _ = writeln!(out, "    {},", vertex_index);
    }
    out.push_str("];\n\n");

    out.push_str("#[cfg(feature = \"cd-stream-bench\")]\n");
    out.push_str("/// Stream builds read cached room vertices from `.psxc` chunks.\n");
    out.push_str("pub static ROOM_CACHE_VERTICES: &[LevelCachedRoomVertexRecord] = &[];\n\n");
    out.push_str("#[cfg(not(feature = \"cd-stream-bench\"))]\n");
    out.push_str("/// Generated cached room vertices.\n");
    out.push_str("pub static ROOM_CACHE_VERTICES: &[LevelCachedRoomVertexRecord] = &[\n");
    for vertex in &package.room_cache_vertices {
        let _ = writeln!(
            out,
            "    LevelCachedRoomVertexRecord {{ x: {}, y: {}, z: {} }},",
            vertex.x, vertex.y, vertex.z,
        );
    }
    out.push_str("];\n\n");

    out.push_str("#[cfg(feature = \"cd-stream-bench\")]\n");
    out.push_str("/// Stream builds read cached room surfaces from `.psxc` chunks.\n");
    out.push_str("pub static ROOM_CACHE_SURFACES: &[LevelCachedRoomSurfaceRecord] = &[];\n\n");
    out.push_str("#[cfg(not(feature = \"cd-stream-bench\"))]\n");
    out.push_str("/// Generated cached room surfaces.\n");
    out.push_str("pub static ROOM_CACHE_SURFACES: &[LevelCachedRoomSurfaceRecord] = &[\n");
    for surface in &package.room_cache_surfaces {
        let _ = writeln!(
            out,
            "    LevelCachedRoomSurfaceRecord {{ material_slot: {}, vertex_indices: [{}, {}, {}, {}], sample_sx: {}, sample_sz: {}, sample_ordinal: {}, uv_words: [{}, {}, {}, {}], baked_vertex_rgb: [({}, {}, {}), ({}, {}, {}), ({}, {}, {}), ({}, {}, {})], kind_flags: {}, wall_direction: {}, split: {}, triangle_index: {} }},",
            surface.material_slot,
            surface.vertex_indices[0],
            surface.vertex_indices[1],
            surface.vertex_indices[2],
            surface.vertex_indices[3],
            surface.sample_sx,
            surface.sample_sz,
            surface.sample_ordinal,
            surface.uv_words[0],
            surface.uv_words[1],
            surface.uv_words[2],
            surface.uv_words[3],
            surface.baked_vertex_rgb[0].0,
            surface.baked_vertex_rgb[0].1,
            surface.baked_vertex_rgb[0].2,
            surface.baked_vertex_rgb[1].0,
            surface.baked_vertex_rgb[1].1,
            surface.baked_vertex_rgb[1].2,
            surface.baked_vertex_rgb[2].0,
            surface.baked_vertex_rgb[2].1,
            surface.baked_vertex_rgb[2].2,
            surface.baked_vertex_rgb[3].0,
            surface.baked_vertex_rgb[3].1,
            surface.baked_vertex_rgb[3].2,
            surface.kind_flags,
            surface.wall_direction,
            surface.split,
            surface.triangle_index,
        );
    }
    out.push_str("];\n\n");

    // Per-room residency: required RAM = the room's world asset plus every
    // persistent animation clip referenced by an instance or player character.
    // Model mesh source blobs are streamed through transient scratch and decoded
    // into fixed geometry pools, so they are deliberately absent here.
    // Required VRAM = every distinct texture asset
    // (room materials + room reflection probes + far-vista panels + model atlases)
    // referenced by this room. Warm lists mirror touching chunks
    // so the runtime can preload neighbours without owning their
    // shared assets twice.
    let residency_requirements: Vec<(Vec<usize>, Vec<usize>)> = package
        .rooms
        .iter()
        .enumerate()
        .map(|(i, room)| room_required_assets(package, i, room))
        .collect();
    let warm_requirements: Vec<(Vec<usize>, Vec<usize>)> = package
        .rooms
        .iter()
        .enumerate()
        .map(|(i, _room)| warm_assets_for_room(package, &residency_requirements, i))
        .collect();

    for (i, (required_ram, required_vram)) in residency_requirements.iter().enumerate() {
        let _ = writeln!(out, "/// Room {i} required RAM assets.");
        out.push_str(&format!(
            "pub static ROOM_{i}_REQUIRED_RAM: &[AssetId] = &["
        ));
        for (j, idx) in required_ram.iter().enumerate() {
            if j > 0 {
                out.push_str(", ");
            }
            let _ = write!(out, "AssetId({idx})");
        }
        out.push_str("];\n");
        let _ = writeln!(out, "/// Room {i} required VRAM assets.");
        out.push_str(&format!(
            "pub static ROOM_{i}_REQUIRED_VRAM: &[AssetId] = &["
        ));
        for (j, idx) in required_vram.iter().enumerate() {
            if j > 0 {
                out.push_str(", ");
            }
            let _ = write!(out, "AssetId({idx})");
        }
        out.push_str("];\n");
        let (warm_ram, warm_vram) = &warm_requirements[i];
        let _ = writeln!(out, "/// Room {i} warm RAM assets.");
        out.push_str(&format!("pub static ROOM_{i}_WARM_RAM: &[AssetId] = &["));
        for (j, idx) in warm_ram.iter().enumerate() {
            if j > 0 {
                out.push_str(", ");
            }
            let _ = write!(out, "AssetId({idx})");
        }
        out.push_str("];\n");
        let _ = writeln!(out, "/// Room {i} warm VRAM assets.");
        out.push_str(&format!("pub static ROOM_{i}_WARM_VRAM: &[AssetId] = &["));
        for (j, idx) in warm_vram.iter().enumerate() {
            if j > 0 {
                out.push_str(", ");
            }
            let _ = write!(out, "AssetId({idx})");
        }
        out.push_str("];\n");
    }
    out.push('\n');

    out.push_str("/// Per-room residency contract.\n");
    out.push_str("pub static ROOM_RESIDENCY: &[RoomResidencyRecord] = &[\n");
    for (i, _room) in package.rooms.iter().enumerate() {
        let _ = writeln!(
            out,
            "    RoomResidencyRecord {{ room: RoomIndex({i}), required_ram: ROOM_{i}_REQUIRED_RAM, required_vram: ROOM_{i}_REQUIRED_VRAM, warm_ram: ROOM_{i}_WARM_RAM, warm_vram: ROOM_{i}_WARM_VRAM }},",
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
            "    InteractableMessageRecord {{ title: {:?}, body: {:?} }},",
            message.title, message.body,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Placed gameplay interactables, room-local coordinates.\n");
    out.push_str("pub static INTERACTABLES: &[InteractableRecord] = &[\n");
    for interactable in &package.interactables {
        let kind = match interactable.kind {
            PlaytestInteractableKind::Message => "InteractableKind::Message",
            PlaytestInteractableKind::Checkpoint => "InteractableKind::Checkpoint",
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
            "    InteractableRecord {{ room: RoomIndex({}), kind: {kind}, x: {}, y: {}, z: {}, yaw: {}, radius: {}, prompt: {:?}, message: {message}, logic: {logic}, checkpoint_id: {:?}, flags: {} }},",
            interactable.room,
            interactable.x,
            interactable.y,
            interactable.z,
            interactable.yaw,
            interactable.radius,
            interactable.prompt,
            interactable.checkpoint_id,
            interactable.flags,
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
            "    CombatCapsuleRecord {{ joint: {}, flags: {}, action: {}, reserved: 0, start: [{}, {}, {}], end: [{}, {}, {}], radius: {}, active_start_frame: {}, active_end_frame: {}, damage: {}, poise_damage: {}, projectile_speed: {}, projectile_lifetime_ticks: {}, projectile_min_range: {}, projectile_max_range: {}, projectile_tint_rgb: [{}, {}, {}], projectile_reserved: 0 }},",
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
            "    LevelGameEntityRecord {{ room: RoomIndex({}), kind: {}, targetname: {}, model_instance: {model_instance}, idle_clip: {}, walk_clip: {}, walk_backward_clip: {}, strafe_left_clip: {}, strafe_right_clip: {}, run_clip: {}, attack_clip: {}, stagger_clip: {}, death_clip: {}, combat_capsule_first: CombatCapsuleIndex({}), combat_capsule_count: {}, x: {}, y: {}, z: {}, yaw: {}, radius: {}, height: {}, walk_speed: {}, run_speed: {}, patrol_x: {}, patrol_y: {}, patrol_z: {}, patrol_wait_ticks: {}, aggro_radius: {}, reaction_ticks: {}, preferred_distance: {}, spacing_tolerance: {}, decision_interval_ticks: {}, circle_chance: {}, attack_priority: {}, attack_cooldown_ticks: {}, group_attack_delay_ticks: {}, windup_ticks: {}, recovery_ticks: {}, attack_min_range: {}, attack_max_range: {}, poise: {}, touch_damage: {}, max_health: {}, flags: {} }},",
            entity.room,
            entity.kind,
            entity.targetname,
            entity.idle_clip,
            entity.walk_clip,
            entity.walk_backward_clip,
            entity.strafe_left_clip,
            entity.strafe_right_clip,
            entity.run_clip,
            entity.attack_clip,
            entity.stagger_clip,
            entity.death_clip,
            entity.combat_capsule_first,
            entity.combat_capsule_count,
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
            entity.decision_interval_ticks,
            entity.circle_chance,
            entity.attack_priority,
            entity.attack_cooldown_ticks,
            entity.group_attack_delay_ticks,
            entity.windup_ticks,
            entity.recovery_ticks,
            entity.attack_min_range,
            entity.attack_max_range,
            entity.poise,
            entity.touch_damage,
            entity.max_health,
            entity.flags,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Animation-authored equipped weapon visibility beats.\n");
    out.push_str("pub static WEAPON_APPEARANCES: &[WeaponAppearanceRecord] = &[\n");
    for appearance in &package.weapon_appearances {
        let _ = writeln!(
            out,
            "    WeaponAppearanceRecord {{ character: CharacterIndex({}), action: CharacterAnimationAction::{:?}, weapon: WeaponIndex({}), character_socket: {:?}, fully_visible_frame: {}, hidden_frame: {}, transition_frames: {}, flags: 0 }},",
            appearance.character,
            appearance.action,
            appearance.weapon,
            appearance.character_socket,
            appearance.fully_visible_frame,
            appearance.hidden_frame,
            appearance.transition_frames,
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
            "    LevelCharacterRecord {{ model: ModelIndex({}), action_clips: [{}], action_flags: [{}], action_speeds: [{}], action_frame_ranges: [{}], action_pushes: [{}], combat_capsule_first: CombatCapsuleIndex({}), combat_capsule_count: {}, visual_offset: [{}, {}, {}], visual_yaw: {}, visual_scale_q8: {}, weight_q8: {}, radius: {}, height: {}, walk_speed: {}, run_speed: {}, turn_speed_degrees_per_second: {}, stamina_max_q12: {}, sprint_min_q12: {}, sprint_drain_q12: {}, stamina_recover_q12: {}, roll_cost_q12: {}, roll_speed: {}, roll_active_frames: {}, roll_recovery_frames: {}, roll_invulnerable_frames: {}, backstep_cost_q12: {}, backstep_speed: {}, backstep_active_frames: {}, backstep_recovery_frames: {}, backstep_invulnerable_frames: {}, camera_distance: {}, camera_height: {}, camera_target_height: {}, material_override: {}, flags: 0 }},",
            character.model,
            action_clips,
            action_flags,
            action_speeds,
            action_frame_ranges,
            action_pushes,
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

fn render_world_pack_order(package: &PlaytestPackage) -> String {
    let mut out = String::from(
        "# PSoXide WORLD.PAK room order\n\
         # One cooked room id per line. Generated by cook-playtest.\n",
    );
    for room in world_pack_order(package) {
        let _ = writeln!(out, "{room}");
    }
    out
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

fn world_pack_chunks(package: &PlaytestPackage) -> Vec<(u32, Vec<u8>)> {
    let mut chunks: Vec<(u32, Vec<u8>)> = Vec::new();
    for room in world_pack_order(package) {
        let payload =
            streamed_room_chunk_payload(package, room).expect("valid streamed room chunk payload");
        chunks.push((room as u32, payload));
    }
    chunks
}

fn world_pack_toc(package: &PlaytestPackage) -> Vec<psx_iso::WorldPackBuildEntry> {
    world_pack_layout(package).entries
}

fn world_pack_layout(package: &PlaytestPackage) -> psx_iso::WorldPackLayout {
    let chunks = world_pack_chunks(package);
    let refs = chunks
        .iter()
        .map(|(room, bytes)| (*room, bytes.as_slice()))
        .collect::<Vec<_>>();
    psx_iso::build_world_pack_layout(&refs)
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
        .collect()
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

fn streamed_room_chunk_filename(room: u16) -> String {
    format!("room_{room:03}.psxc")
}

pub fn streamed_room_chunk_memory_report(
    package: &PlaytestPackage,
) -> Result<PlaytestStreamMemoryReport, String> {
    let mut report = PlaytestStreamMemoryReport::default();
    let room_count = package.rooms.len().min(u16::MAX as usize + 1);
    for room in 0..room_count {
        if package.rooms[room].world_asset_index.is_none() {
            continue;
        }
        let memory = streamed_room_chunk_memory(package, room as u16)?;
        report.totals.sector_count += memory.sector_count;
        report.totals.payload_bytes += memory.payload_bytes;
        report.totals.stream_bytes += memory.stream_bytes;
        report.totals.header_bytes += memory.header_bytes;
        report.totals.collision_bytes += memory.collision_bytes;
        report.totals.render_cell_bytes += memory.render_cell_bytes;
        report.totals.render_cell_vertex_bytes += memory.render_cell_vertex_bytes;
        report.totals.render_vertex_bytes += memory.render_vertex_bytes;
        report.totals.render_surface_bytes += memory.render_surface_bytes;
        report.totals.render_cache_bytes += memory.render_cache_bytes;
        report.totals.alignment_padding_bytes += memory.alignment_padding_bytes;
        report.totals.sector_padding_bytes += memory.sector_padding_bytes;
        if report
            .largest_chunk
            .map(|largest| memory.stream_bytes > largest.stream_bytes)
            .unwrap_or(true)
        {
            report.largest_chunk = Some(memory);
        }
        report.chunks.push(memory);
    }
    Ok(report)
}

fn streamed_room_chunk_memory(
    package: &PlaytestPackage,
    room: u16,
) -> Result<PlaytestStreamChunkMemory, String> {
    let layout = streamed_room_chunk_layout(package, room)?;
    let payload = streamed_room_chunk_payload(package, room)?;
    let payload_bytes = payload.len();
    let sector_size = psx_iso::SECTOR_USER_DATA_BYTES;
    let sector_count = payload_bytes.saturating_add(sector_size - 1) / sector_size;
    let stream_bytes = sector_count.saturating_mul(sector_size);
    let render_cell_bytes =
        layout.cell_count * std::mem::size_of::<psx_level::LevelCachedRoomCellRecord>();
    let render_vertex_bytes =
        layout.vertex_count * std::mem::size_of::<psx_level::LevelCachedRoomVertexRecord>();
    let render_cell_vertex_bytes = layout.cell_vertex_count * std::mem::size_of::<u16>();
    let render_surface_bytes =
        layout.surface_count * std::mem::size_of::<psx_level::LevelCachedRoomSurfaceRecord>();
    let render_cache_bytes =
        render_cell_bytes + render_cell_vertex_bytes + render_vertex_bytes + render_surface_bytes;
    let accounted_bytes = psx_level::STREAMED_ROOM_CHUNK_HEADER_BYTES
        + layout.collision_payload.len()
        + render_cache_bytes;
    let alignment_padding_bytes = payload_bytes.saturating_sub(accounted_bytes);
    Ok(PlaytestStreamChunkMemory {
        room,
        sector_count,
        payload_bytes,
        stream_bytes,
        header_bytes: psx_level::STREAMED_ROOM_CHUNK_HEADER_BYTES,
        collision_bytes: layout.collision_payload.len(),
        render_cell_bytes,
        render_cell_vertex_bytes,
        render_vertex_bytes,
        render_surface_bytes,
        render_cache_bytes,
        alignment_padding_bytes,
        sector_padding_bytes: stream_bytes.saturating_sub(payload_bytes),
    })
}

#[derive(Clone)]
struct StreamedRoomChunkLayout<'a> {
    collision_payload: Vec<u8>,
    collision_flags: u32,
    cell_slice: &'a [PlaytestCachedRoomCell],
    cell_vertex_slice: &'a [u16],
    include_cell_vertices: bool,
    vertex_slice: &'a [PlaytestCachedRoomVertex],
    surface_slice: &'a [PlaytestCachedRoomSurface],
    cell_count: usize,
    cell_vertex_count: usize,
    vertex_count: usize,
    surface_count: usize,
}

fn streamed_room_chunk_layout(
    package: &PlaytestPackage,
    room: u16,
) -> Result<StreamedRoomChunkLayout<'_>, String> {
    let room_record = package
        .rooms
        .get(room as usize)
        .ok_or_else(|| format!("missing room record {room}"))?;
    let world_asset_index = room_record
        .world_asset_index
        .ok_or_else(|| format!("room {room} is resident PXBSP and has no streamed PSXW chunk"))?;
    let asset = package
        .assets
        .get(world_asset_index)
        .ok_or_else(|| format!("room {room} references missing world asset"))?;
    if asset.kind != PlaytestAssetKind::RoomWorld {
        return Err(format!(
            "room {room} world asset '{}' is not a collision room payload",
            asset.source_label
        ));
    }

    let cache = package
        .room_surface_caches
        .iter()
        .find(|cache| cache.room == room)
        .copied();
    let cell_slice = cache
        .and_then(|cache| {
            checked_slice(
                &package.room_cache_cells,
                cache.cell_first as usize,
                cache.cell_count as usize,
            )
        })
        .unwrap_or(&[]);
    let vertex_slice = cache
        .and_then(|cache| {
            checked_slice(
                &package.room_cache_vertices,
                cache.vertex_first as usize,
                cache.vertex_count as usize,
            )
        })
        .unwrap_or(&[]);
    let cell_vertex_slice = cache
        .and_then(|cache| {
            checked_slice(
                &package.room_cache_cell_vertices,
                cache.cell_vertex_first as usize,
                cache.cell_vertex_count as usize,
            )
        })
        .unwrap_or(&[]);
    let surface_slice = cache
        .and_then(|cache| {
            checked_slice(
                &package.room_cache_surfaces,
                cache.surface_first as usize,
                cache.surface_count as usize,
            )
        })
        .unwrap_or(&[]);

    let collision_payload =
        compact_collision_payload(&asset.bytes, room, &package.room_floor_links)?;
    let include_cell_vertices = !cell_vertex_slice.is_empty()
        && streamed_room_chunk_payload_len(
            collision_payload.len(),
            cell_slice.len(),
            cell_vertex_slice.len(),
            vertex_slice.len(),
            surface_slice.len(),
        ) <= STREAMED_ROOM_SLOT_BYTES;
    let cell_vertex_slice = if include_cell_vertices {
        cell_vertex_slice
    } else {
        &[]
    };

    Ok(StreamedRoomChunkLayout {
        collision_payload,
        collision_flags: psx_level::STREAMED_ROOM_CHUNK_FLAG_COLLISION_COMPACT,
        cell_slice,
        cell_vertex_slice,
        include_cell_vertices,
        vertex_slice,
        surface_slice,
        cell_count: cell_slice.len(),
        cell_vertex_count: cell_vertex_slice.len(),
        vertex_count: vertex_slice.len(),
        surface_count: surface_slice.len(),
    })
}

fn streamed_room_chunk_payload(package: &PlaytestPackage, room: u16) -> Result<Vec<u8>, String> {
    let layout = streamed_room_chunk_layout(package, room)?;
    let cell_slice = layout.cell_slice;
    let cell_vertex_slice = layout.cell_vertex_slice;
    let vertex_slice = layout.vertex_slice;
    let surface_slice = layout.surface_slice;

    let mut out = vec![0u8; psx_level::STREAMED_ROOM_CHUNK_HEADER_BYTES];
    align_vec(&mut out, 4);
    let collision_offset = out.len();
    out.extend_from_slice(&layout.collision_payload);
    align_vec(&mut out, 4);
    let cells_offset = out.len();
    append_cached_room_cells(&mut out, cell_slice, layout.include_cell_vertices);
    align_vec(&mut out, 2);
    let cell_vertices_offset = out.len();
    append_cached_room_cell_vertices(&mut out, cell_vertex_slice);
    align_vec(&mut out, 4);
    let vertices_offset = out.len();
    append_cached_room_vertices(&mut out, vertex_slice);
    align_vec(&mut out, 4);
    let surfaces_offset = out.len();
    append_cached_room_surfaces(&mut out, surface_slice);
    align_vec(&mut out, 4);

    out[..8].copy_from_slice(&psx_level::STREAMED_ROOM_CHUNK_MAGIC);
    write_u32_le(
        &mut out,
        psx_level::streamed_room_chunk_header::VERSION,
        psx_level::STREAMED_ROOM_CHUNK_VERSION,
    )?;
    write_u32_le(
        &mut out,
        psx_level::streamed_room_chunk_header::ROOM,
        u32::from(room),
    )?;
    let total_len = out.len();
    write_u32_le(
        &mut out,
        psx_level::streamed_room_chunk_header::TOTAL_BYTES,
        checked_u32(total_len, "streamed room chunk size")?,
    )?;
    write_u32_le(
        &mut out,
        psx_level::streamed_room_chunk_header::COLLISION_OFFSET,
        checked_u32(collision_offset, "streamed room collision offset")?,
    )?;
    write_u32_le(
        &mut out,
        psx_level::streamed_room_chunk_header::COLLISION_BYTES,
        checked_u32(
            layout.collision_payload.len(),
            "streamed room collision byte count",
        )?,
    )?;
    write_u32_le(
        &mut out,
        psx_level::streamed_room_chunk_header::CELLS_OFFSET,
        checked_u32(cells_offset, "streamed room cells offset")?,
    )?;
    write_u32_le(
        &mut out,
        psx_level::streamed_room_chunk_header::CELL_COUNT,
        checked_u32(cell_slice.len(), "streamed room cell count")?,
    )?;
    write_u32_le(
        &mut out,
        psx_level::streamed_room_chunk_header::VERTICES_OFFSET,
        checked_u32(vertices_offset, "streamed room vertices offset")?,
    )?;
    write_u32_le(
        &mut out,
        psx_level::streamed_room_chunk_header::VERTEX_COUNT,
        checked_u32(vertex_slice.len(), "streamed room vertex count")?,
    )?;
    write_u32_le(
        &mut out,
        psx_level::streamed_room_chunk_header::SURFACES_OFFSET,
        checked_u32(surfaces_offset, "streamed room surfaces offset")?,
    )?;
    write_u32_le(
        &mut out,
        psx_level::streamed_room_chunk_header::SURFACE_COUNT,
        checked_u32(surface_slice.len(), "streamed room surface count")?,
    )?;
    write_u32_le(
        &mut out,
        psx_level::streamed_room_chunk_header::CELL_VERTICES_OFFSET,
        checked_u32(
            cell_vertices_offset,
            "streamed room cell vertex indices offset",
        )?,
    )?;
    write_u32_le(
        &mut out,
        psx_level::streamed_room_chunk_header::CELL_VERTEX_COUNT,
        checked_u32(
            cell_vertex_slice.len(),
            "streamed room cell vertex index count",
        )?,
    )?;
    write_u32_le(
        &mut out,
        psx_level::streamed_room_chunk_header::FLAGS,
        layout.collision_flags,
    )?;
    Ok(out)
}

fn streamed_room_chunk_payload_len(
    collision_bytes: usize,
    cell_count: usize,
    cell_vertex_count: usize,
    vertex_count: usize,
    surface_count: usize,
) -> usize {
    let mut len = psx_level::STREAMED_ROOM_CHUNK_HEADER_BYTES;
    len = align_usize(len, 4);
    len = len.saturating_add(collision_bytes);
    len = align_usize(len, 4);
    len = len.saturating_add(
        cell_count.saturating_mul(std::mem::size_of::<psx_level::LevelCachedRoomCellRecord>()),
    );
    len = align_usize(len, 2);
    len = len.saturating_add(cell_vertex_count.saturating_mul(std::mem::size_of::<u16>()));
    len = align_usize(len, 4);
    len = len.saturating_add(
        vertex_count.saturating_mul(std::mem::size_of::<psx_level::LevelCachedRoomVertexRecord>()),
    );
    len = align_usize(len, 4);
    len = len.saturating_add(
        surface_count
            .saturating_mul(std::mem::size_of::<psx_level::LevelCachedRoomSurfaceRecord>()),
    );
    align_usize(len, 4)
}

fn align_usize(value: usize, align: usize) -> usize {
    if align <= 1 {
        return value;
    }
    let rem = value % align;
    if rem == 0 {
        value
    } else {
        value.saturating_add(align - rem)
    }
}

fn compact_collision_payload(
    bytes: &[u8],
    room_index: u16,
    floor_links: &[PlaytestRoomFloorLink],
) -> Result<Vec<u8>, String> {
    let room = psx_engine::RuntimeRoom::from_bytes(bytes)
        .map_err(|e| format!("room collision source did not parse: {e:?}"))?;
    let width = room.width();
    let depth = room.depth();
    let sector_count = width
        .checked_mul(depth)
        .ok_or_else(|| "room collision sector count overflowed u16".to_string())?;
    let wall_count = room.world().wall_count();
    let mut sectors =
        Vec::with_capacity(sector_count as usize * psx_level::COMPACT_COLLISION_SECTOR_BYTES);
    let mut height_overrides = Vec::new();

    let render = room.render();
    let collision = room.collision();
    let mut sx = 0u16;
    while sx < width {
        let mut sz = 0u16;
        while sz < depth {
            let render_sector = render.sector(sx, sz);
            let collision_sector = collision.sector(sx, sz);
            append_compact_collision_sector(
                &mut sectors,
                &mut height_overrides,
                sx,
                sz,
                depth,
                render_sector,
                collision_sector,
                compact_floor_link_targets(floor_links, room_index, sx, sz),
            )?;
            sz += 1;
        }
        sx += 1;
    }

    let mut walls =
        Vec::with_capacity(wall_count as usize * psx_level::COMPACT_COLLISION_WALL_BYTES);
    let mut wall_index = 0u16;
    while wall_index < wall_count {
        let wall = room
            .world()
            .wall(wall_index)
            .ok_or_else(|| format!("room collision wall {wall_index} missing"))?;
        append_compact_collision_wall(&mut walls, wall);
        wall_index += 1;
    }

    let override_count =
        height_overrides.len() / psx_level::COMPACT_COLLISION_HEIGHT_OVERRIDE_BYTES;
    if override_count > u16::MAX as usize {
        return Err("room collision height override count overflowed u16".to_string());
    }

    let mut out = vec![0u8; psx_level::COMPACT_COLLISION_HEADER_BYTES];
    out[..8].copy_from_slice(&psx_level::COMPACT_COLLISION_MAGIC);
    write_u32_le(
        &mut out,
        psx_level::compact_collision_header::VERSION,
        psx_level::COMPACT_COLLISION_VERSION,
    )?;
    write_u16_le(&mut out, psx_level::compact_collision_header::WIDTH, width)?;
    write_u16_le(&mut out, psx_level::compact_collision_header::DEPTH, depth)?;
    write_i32_le(
        &mut out,
        psx_level::compact_collision_header::SECTOR_SIZE,
        room.sector_size(),
    )?;
    write_u16_le(
        &mut out,
        psx_level::compact_collision_header::SECTOR_COUNT,
        sector_count,
    )?;
    write_u16_le(
        &mut out,
        psx_level::compact_collision_header::WALL_COUNT,
        wall_count,
    )?;
    write_u16_le(
        &mut out,
        psx_level::compact_collision_header::HEIGHT_OVERRIDE_COUNT,
        override_count as u16,
    )?;
    out[psx_level::compact_collision_header::AMBIENT_RGB
        ..psx_level::compact_collision_header::AMBIENT_RGB + 3]
        .copy_from_slice(&room.render().ambient_color());
    out.extend_from_slice(&sectors);
    out.extend_from_slice(&walls);
    out.extend_from_slice(&height_overrides);
    Ok(out)
}

fn append_compact_collision_sector(
    out: &mut Vec<u8>,
    height_overrides: &mut Vec<u8>,
    sx: u16,
    sz: u16,
    depth: u16,
    render_sector: Option<psx_engine::SectorRender>,
    collision_sector: Option<psx_engine::SectorCollision>,
    floor_links: (Option<u16>, Option<u16>),
) -> Result<(), String> {
    let mut flags = 0u8;
    let mut floor_triangle_flags = 0u8;
    let mut ceiling_triangle_flags = 0u8;
    let floor_split = render_sector
        .map(|sector| sector.floor_split())
        .unwrap_or(0);
    let ceiling_split = render_sector
        .map(|sector| sector.ceiling_split())
        .unwrap_or(0);
    let floor_heights = render_sector
        .map(|sector| sector.floor_heights())
        .unwrap_or([0; 4]);
    let ceiling_heights = render_sector
        .map(|sector| sector.ceiling_heights())
        .unwrap_or([0; 4]);
    let first_wall = render_sector.map(|sector| sector.first_wall()).unwrap_or(0);
    let wall_count = render_sector.map(|sector| sector.wall_count()).unwrap_or(0);

    if let Some(render_sector) = render_sector {
        if render_sector.has_floor() {
            flags |= psx_level::compact_collision_sector_flags::HAS_FLOOR;
        }
        if render_sector.has_ceiling() {
            flags |= psx_level::compact_collision_sector_flags::HAS_CEILING;
        }
        floor_triangle_flags = compact_floor_triangle_flags(render_sector, collision_sector);
        ceiling_triangle_flags = compact_ceiling_triangle_flags(render_sector);
        if collision_sector
            .map(|sector| sector.floor_walkable())
            .unwrap_or(false)
        {
            flags |= psx_level::compact_collision_sector_flags::FLOOR_WALKABLE;
        }
        append_height_override_if_needed(
            height_overrides,
            sx,
            sz,
            depth,
            psx_level::compact_collision_surface::FLOOR,
            floor_split,
            floor_heights,
            [
                render_sector.floor_triangle_heights(0),
                render_sector.floor_triangle_heights(1),
            ],
            floor_triangle_flags,
        )?;
        append_height_override_if_needed(
            height_overrides,
            sx,
            sz,
            depth,
            psx_level::compact_collision_surface::CEILING,
            ceiling_split,
            ceiling_heights,
            [
                render_sector.ceiling_triangle_heights(0),
                render_sector.ceiling_triangle_heights(1),
            ],
            ceiling_triangle_flags,
        )?;
    }
    if floor_links.0.is_some() {
        flags |= psx_level::compact_collision_sector_flags::HAS_FLOOR_ABOVE;
    }
    if floor_links.1.is_some() {
        flags |= psx_level::compact_collision_sector_flags::HAS_FLOOR_BELOW;
    }

    out.push(flags);
    out.push(floor_split);
    out.push(ceiling_split);
    out.push(floor_triangle_flags);
    out.push(ceiling_triangle_flags);
    out.push(0);
    append_u16_le(out, first_wall);
    append_u16_le(out, wall_count);
    append_u16_le(out, 0);
    for value in floor_heights {
        append_i32_le(out, value);
    }
    for value in ceiling_heights {
        append_i32_le(out, value);
    }
    append_u16_le(
        out,
        floor_links
            .0
            .unwrap_or(psx_level::COMPACT_COLLISION_NO_ROOM),
    );
    append_u16_le(
        out,
        floor_links
            .1
            .unwrap_or(psx_level::COMPACT_COLLISION_NO_ROOM),
    );
    Ok(())
}

fn compact_floor_link_targets(
    floor_links: &[PlaytestRoomFloorLink],
    room: u16,
    x: u16,
    z: u16,
) -> (Option<u16>, Option<u16>) {
    floor_links
        .iter()
        .find(|link| link.room == room && link.x == x && link.z == z)
        .map(|link| (link.above_room, link.below_room))
        .unwrap_or((None, None))
}

fn compact_floor_triangle_flags(
    render: psx_engine::SectorRender,
    collision: Option<psx_engine::SectorCollision>,
) -> u8 {
    let mut flags = 0u8;
    for index in 0..2 {
        if render.floor_triangle_present(index) {
            flags |= compact_triangle_present_bit(index);
        }
        if collision
            .map(|sector| sector.floor_triangle_walkable(index))
            .unwrap_or(false)
        {
            flags |= compact_triangle_walkable_bit(index);
        }
    }
    flags
}

fn compact_ceiling_triangle_flags(render: psx_engine::SectorRender) -> u8 {
    let mut flags = 0u8;
    for index in 0..2 {
        if render.ceiling_triangle_present(index) {
            flags |= compact_triangle_present_bit(index);
        }
    }
    flags
}

fn compact_triangle_present_bit(index: usize) -> u8 {
    if index == 0 {
        psx_level::compact_collision_triangle_flags::TRI_A_PRESENT
    } else {
        psx_level::compact_collision_triangle_flags::TRI_B_PRESENT
    }
}

fn compact_triangle_walkable_bit(index: usize) -> u8 {
    if index == 0 {
        psx_level::compact_collision_triangle_flags::TRI_A_WALKABLE
    } else {
        psx_level::compact_collision_triangle_flags::TRI_B_WALKABLE
    }
}

fn append_height_override_if_needed(
    out: &mut Vec<u8>,
    sx: u16,
    sz: u16,
    depth: u16,
    surface: u8,
    split: u8,
    heights: [i32; 4],
    triangle_heights: [[i32; 3]; 2],
    triangle_flags: u8,
) -> Result<(), String> {
    if triangle_flags == 0 {
        return Ok(());
    }
    let derived = [
        compact_horizontal_triangle_heights(heights, split, 0),
        compact_horizontal_triangle_heights(heights, split, 1),
    ];
    if triangle_heights == derived {
        return Ok(());
    }
    let sector_index = sx
        .checked_mul(depth)
        .and_then(|base| base.checked_add(sz))
        .ok_or_else(|| "compact collision override sector index overflowed".to_string())?;
    append_u16_le(out, sector_index);
    out.push(surface);
    out.push(0);
    for value in triangle_heights[0] {
        append_i32_le(out, value);
    }
    for value in triangle_heights[1] {
        append_i32_le(out, value);
    }
    Ok(())
}

fn compact_horizontal_triangle_heights(heights: [i32; 4], split: u8, index: usize) -> [i32; 3] {
    let corners = psxed_format::world::topology::split_triangle(split, index);
    [
        heights[corners[0]],
        heights[corners[1]],
        heights[corners[2]],
    ]
}

fn append_compact_collision_wall(out: &mut Vec<u8>, wall: psx_asset::WorldWall) {
    out.push(wall.direction());
    out.push(if wall.solid() {
        psx_level::compact_collision_wall_flags::SOLID
    } else {
        0
    });
    append_u16_le(out, wall.shape());
    for value in wall.heights() {
        append_i32_le(out, value);
    }
}

fn checked_slice<T>(items: &[T], first: usize, count: usize) -> Option<&[T]> {
    let end = first.checked_add(count)?;
    items.get(first..end)
}

fn align_vec(out: &mut Vec<u8>, align: usize) {
    let padding = (align - (out.len() % align)) % align;
    out.resize(out.len() + padding, 0);
}

fn write_u32_le(out: &mut [u8], offset: usize, value: u32) -> Result<(), String> {
    let dst = out
        .get_mut(offset..offset + 4)
        .ok_or_else(|| format!("streamed chunk header write out of bounds at {offset}"))?;
    dst.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn write_u16_le(out: &mut [u8], offset: usize, value: u16) -> Result<(), String> {
    let dst = out
        .get_mut(offset..offset + 2)
        .ok_or_else(|| format!("streamed chunk header write out of bounds at {offset}"))?;
    dst.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn write_i32_le(out: &mut [u8], offset: usize, value: i32) -> Result<(), String> {
    let dst = out
        .get_mut(offset..offset + 4)
        .ok_or_else(|| format!("streamed chunk header write out of bounds at {offset}"))?;
    dst.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn append_u16_le(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn append_i32_le(out: &mut Vec<u8>, value: i32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn append_cached_room_cells(
    out: &mut Vec<u8>,
    cells: &[PlaytestCachedRoomCell],
    include_cell_vertices: bool,
) {
    debug_assert_eq!(
        std::mem::size_of::<psx_level::LevelCachedRoomCellRecord>(),
        36
    );
    for cell in cells {
        append_u16_le(out, cell.x);
        append_u16_le(out, cell.z);
        append_i32_le(out, cell.min_y);
        append_i32_le(out, cell.max_y);
        for value in cell.visibility_center {
            append_i32_le(out, value);
        }
        append_i32_le(out, cell.visibility_radius);
        append_u16_le(out, cell.surface_first);
        append_u16_le(out, cell.surface_count);
        if include_cell_vertices {
            append_u16_le(out, cell.vertex_first);
            append_u16_le(out, cell.vertex_count);
        } else {
            append_u16_le(out, 0);
            append_u16_le(out, 0);
        }
    }
}

fn append_cached_room_vertices(out: &mut Vec<u8>, vertices: &[PlaytestCachedRoomVertex]) {
    debug_assert_eq!(
        std::mem::size_of::<psx_level::LevelCachedRoomVertexRecord>(),
        12
    );
    for vertex in vertices {
        append_i32_le(out, vertex.x);
        append_i32_le(out, vertex.y);
        append_i32_le(out, vertex.z);
    }
}

fn append_cached_room_cell_vertices(out: &mut Vec<u8>, vertices: &[u16]) {
    for vertex in vertices {
        append_u16_le(out, *vertex);
    }
}

fn append_cached_room_surfaces(out: &mut Vec<u8>, surfaces: &[PlaytestCachedRoomSurface]) {
    debug_assert_eq!(
        std::mem::size_of::<psx_level::LevelCachedRoomSurfaceRecord>(),
        40
    );
    for surface in surfaces {
        append_u16_le(out, surface.material_slot);
        for index in surface.vertex_indices {
            append_u16_le(out, index);
        }
        append_u16_le(out, surface.sample_sx);
        append_u16_le(out, surface.sample_sz);
        append_u16_le(out, surface.sample_ordinal);
        for uv_word in surface.uv_words {
            append_u16_le(out, uv_word);
        }
        for (r, g, b) in surface.baked_vertex_rgb {
            out.push(r);
            out.push(g);
            out.push(b);
        }
        out.push(surface.kind_flags);
        out.push(surface.wall_direction);
        out.push(surface.split);
        out.push(surface.triangle_index);
    }
}

fn world_pack_order(package: &PlaytestPackage) -> Vec<u16> {
    let mut order = world_pack_order_from_chunks(
        package.rooms.len(),
        package.spawn.map(|spawn| spawn.room),
        &package.chunks,
    );
    order.retain(|room| {
        package
            .rooms
            .get(*room as usize)
            .is_some_and(|room| room.world_asset_index.is_some())
    });
    order
}

fn world_pack_order_from_chunks(
    room_count: usize,
    spawn_room: Option<u16>,
    chunks: &[PlaytestChunk],
) -> Vec<u16> {
    let room_count = room_count.min(u16::MAX as usize + 1);
    let mut order = Vec::with_capacity(room_count);
    if room_count == 0 {
        return order;
    }

    let mut visited = vec![false; room_count];
    let mut current = spawn_room
        .filter(|room| (*room as usize) < room_count)
        .unwrap_or(0);

    loop {
        append_world_pack_component(current, chunks, &mut visited, &mut order);
        if order.len() >= room_count {
            break;
        }
        let Some(next) = nearest_unvisited_pack_room(current, room_count, chunks, &visited) else {
            break;
        };
        current = next;
    }

    let mut room = 0usize;
    while room < room_count {
        if !visited[room] {
            visited[room] = true;
            order.push(room as u16);
        }
        room += 1;
    }
    order
}

fn append_world_pack_component(
    start_room: u16,
    chunks: &[PlaytestChunk],
    visited: &mut [bool],
    order: &mut Vec<u16>,
) {
    let start = start_room as usize;
    if start >= visited.len() || visited[start] {
        return;
    }

    let mut queue = Vec::new();
    queue.push(start_room);
    visited[start] = true;
    let mut head = 0usize;
    while head < queue.len() {
        let room = queue[head];
        head += 1;
        order.push(room);

        let Some(chunk) = chunk_for_pack_room(chunks, room) else {
            continue;
        };
        let mut neighbours = [(u8::MAX, u16::MAX); 4];
        let mut neighbour_count = 0usize;
        for (direction, neighbour) in chunk.neighbours.iter().enumerate() {
            let Some(neighbour) = *neighbour else {
                continue;
            };
            if neighbour as usize >= visited.len() || visited[neighbour as usize] {
                continue;
            }
            let same_authored = chunk_for_pack_room(chunks, neighbour)
                .is_some_and(|other| other.authored_room == chunk.authored_room);
            let tier = if same_authored { 0 } else { 1 };
            neighbours[neighbour_count] = (tier * 4 + direction as u8, neighbour);
            neighbour_count += 1;
        }
        neighbours[..neighbour_count].sort_by_key(|(score, room)| (*score, *room));
        let mut i = 0usize;
        while i < neighbour_count {
            let neighbour = neighbours[i].1;
            if (neighbour as usize) < visited.len() && !visited[neighbour as usize] {
                visited[neighbour as usize] = true;
                queue.push(neighbour);
            }
            i += 1;
        }
    }
}

fn nearest_unvisited_pack_room(
    anchor_room: u16,
    room_count: usize,
    chunks: &[PlaytestChunk],
    visited: &[bool],
) -> Option<u16> {
    let (anchor_x, anchor_z) = pack_room_center(chunks, anchor_room);
    let mut best_room = None;
    let mut best_distance = i128::MAX;
    let mut room = 0usize;
    while room < room_count {
        if visited.get(room).copied().unwrap_or(true) {
            room += 1;
            continue;
        }
        let (x, z) = pack_room_center(chunks, room as u16);
        let dx = x as i128 - anchor_x as i128;
        let dz = z as i128 - anchor_z as i128;
        let distance = dx.saturating_mul(dx).saturating_add(dz.saturating_mul(dz));
        if best_room.is_none() || distance < best_distance {
            best_room = Some(room as u16);
            best_distance = distance;
        }
        room += 1;
    }
    best_room
}

fn pack_room_center(chunks: &[PlaytestChunk], room: u16) -> (i64, i64) {
    chunk_for_pack_room(chunks, room)
        .map(|chunk| {
            (
                chunk.origin_x as i64 * 2 + chunk.width as i64,
                chunk.origin_z as i64 * 2 + chunk.depth as i64,
            )
        })
        .unwrap_or((room as i64 * 2, 0))
}

fn chunk_for_pack_room(chunks: &[PlaytestChunk], room: u16) -> Option<&PlaytestChunk> {
    chunks.iter().find(|chunk| chunk.room == room)
}

fn asset_vram_bytes(asset: &PlaytestAsset) -> usize {
    match asset.kind {
        PlaytestAssetKind::RoomWorld
        | PlaytestAssetKind::ModelMesh
        | PlaytestAssetKind::ModelAnimation => 0,
        PlaytestAssetKind::Texture => texture_vram_bytes(asset).unwrap_or(asset.bytes.len()),
    }
}

fn texture_vram_bytes(asset: &PlaytestAsset) -> Option<usize> {
    let texture = psx_asset::Texture::from_bytes(&asset.bytes).ok()?;
    Some(texture.pixel_bytes().len() + texture.clut_bytes().len())
}

fn room_index_or_none(index: Option<u16>) -> String {
    index
        .map(|index| format!("RoomIndex({index})"))
        .unwrap_or_else(|| "LevelChunkNeighbours::NONE".to_string())
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
        flags |= roughness_level << psx_level::material_flags::MODEL_REFLECTION_ROUGHNESS_SHIFT;
        flags |= u16::from(reflection.strength)
            << psx_level::material_flags::MODEL_REFLECTION_STRENGTH_SHIFT;
    }
    flags
}

fn level_material_animation_literal(animation: crate::MaterialAnimation) -> String {
    match animation.mode {
        crate::MaterialAnimationMode::Static => "LevelMaterialAnimation::Static".to_string(),
        crate::MaterialAnimationMode::UvScroll => {
            let motion = animation.uv_scroll;
            format!(
                "LevelMaterialAnimation::UvScroll(LevelMaterialUvMotion {{ enabled: true, speed_u_q8: {}, speed_v_q8: {}, phase_u: {}, phase_v: {} }})",
                motion.speed_u_q8, motion.speed_v_q8, motion.phase_u, motion.phase_v,
            )
        }
        crate::MaterialAnimationMode::Flipbook => {
            let flipbook = animation.flipbook.normalized();
            format!(
                "LevelMaterialAnimation::Flipbook(LevelMaterialFlipbook {{ columns: {}, rows: {}, frame_count: {}, ticks_per_frame: {}, phase: {} }})",
                flipbook.columns,
                flipbook.rows,
                flipbook.frame_count,
                flipbook.ticks_per_frame,
                flipbook.phase,
            )
        }
    }
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

fn room_required_assets(
    package: &PlaytestPackage,
    room_index: usize,
    room: &PlaytestRoom,
) -> (Vec<usize>, Vec<usize>) {
    let first = room.material_first as usize;
    let count = room.material_count as usize;
    let mut required_vram: Vec<usize> = Vec::with_capacity(count);
    for material in &package.materials[first..first + count] {
        push_unique(&mut required_vram, material.texture_asset_index);
    }
    for asset_index in room.far_vista.texture_asset_indices.iter().flatten() {
        push_unique(&mut required_vram, *asset_index);
    }
    if let Some(asset_index) = room.sky.cloud_layer.texture_asset_index {
        push_unique(&mut required_vram, asset_index);
    }
    if let Some(asset_index) = room.reflection_probe_asset_index {
        push_unique(&mut required_vram, asset_index);
    }
    if room_index == 0 {
        if let PlaytestWorldGeometry::Pxbsp(world) = &package.world_geometry {
            for &asset_index in &world.texture_asset_indices {
                push_unique(&mut required_vram, asset_index);
            }
        }
    }
    for prop in &package.image_props {
        if prop.room == room_index as u16 {
            push_unique(&mut required_vram, prop.texture_asset_index);
        }
    }
    for prop in &package.box_props {
        if prop.room == room_index as u16 {
            for asset_index in prop.texture_asset_indices.iter().flatten() {
                push_unique(&mut required_vram, *asset_index);
            }
        }
    }
    for prop in &package.cylinder_props {
        if prop.room == room_index as u16 {
            for asset_index in prop.texture_asset_indices.iter().flatten() {
                push_unique(&mut required_vram, *asset_index);
            }
        }
    }
    for prop in &package.arch_props {
        if prop.room == room_index as u16 {
            for asset_index in prop.texture_asset_indices.iter().flatten() {
                push_unique(&mut required_vram, *asset_index);
            }
        }
    }
    for water in &package.water_cells {
        if water.room == room_index as u16 {
            if let Some(asset_index) = water.texture_asset_index {
                push_unique(&mut required_vram, asset_index);
            }
        }
    }
    let mut required_ram: Vec<usize> = room.world_asset_index.into_iter().collect();

    // Models the room references -- placed MeshInstance bindings
    // plus the player controller's character when its spawn lives
    // in this room.
    let room_index = room_index as u16;
    let mut seen_models: Vec<u16> = Vec::new();
    for inst in &package.model_instances {
        if inst.room != room_index {
            continue;
        }
        // Covering textures upload per instance (not per model), so
        // they ride outside the seen_models dedupe.
        if let Some(material_override) = inst.material_override {
            if let Some(asset_index) = material_override.texture_asset_index {
                push_unique(&mut required_vram, asset_index);
            }
            if let Some(layer) = material_override.secondary_layer {
                if let Some(asset_index) = layer.texture_asset_index {
                    push_unique(&mut required_vram, asset_index);
                }
            }
        }
        if seen_models.contains(&inst.model) {
            continue;
        }
        seen_models.push(inst.model);
        include_model_in_residency(package, inst.model, &mut required_ram, &mut required_vram);
    }
    if let Some(pc) = package.player_controller {
        let character = &package.characters[pc.character as usize];
        // The player renders in every room, so its covering texture
        // is required everywhere (unlike the session-persistent model
        // atlas, prop-mode texture slots are evictable).
        if let Some(material_override) = character.material_override {
            if let Some(asset_index) = material_override.texture_asset_index {
                push_unique(&mut required_vram, asset_index);
            }
            if let Some(layer) = material_override.secondary_layer {
                if let Some(asset_index) = layer.texture_asset_index {
                    push_unique(&mut required_vram, asset_index);
                }
            }
        }
        if pc.spawn.room == room_index {
            let model = character.model;
            if !seen_models.contains(&model) {
                seen_models.push(model);
                include_model_in_residency(package, model, &mut required_ram, &mut required_vram);
            }
        }
    }
    for equipment in &package.equipment {
        if equipment.room != room_index {
            continue;
        }
        let Some(weapon) = package.weapons.get(equipment.weapon as usize) else {
            continue;
        };
        if let Some(model) = weapon.model {
            if !seen_models.contains(&model) {
                seen_models.push(model);
                include_model_in_residency(package, model, &mut required_ram, &mut required_vram);
            }
        }
    }

    (required_ram, required_vram)
}

fn warm_assets_for_room(
    package: &PlaytestPackage,
    residency_requirements: &[(Vec<usize>, Vec<usize>)],
    room_index: usize,
) -> (Vec<usize>, Vec<usize>) {
    let mut warm_ram = Vec::new();
    let mut warm_vram = Vec::new();
    let Some((required_ram, required_vram)) = residency_requirements.get(room_index) else {
        return (warm_ram, warm_vram);
    };
    for neighbour_index in 0..package.rooms.len() {
        if neighbour_index == room_index
            || !package_rooms_touch(package, room_index, neighbour_index)
        {
            continue;
        }
        let Some((neighbour_ram, neighbour_vram)) = residency_requirements.get(neighbour_index)
        else {
            continue;
        };
        for asset in neighbour_ram {
            if !required_ram.contains(asset) {
                push_unique(&mut warm_ram, *asset);
            }
        }
        for asset in neighbour_vram {
            if !required_vram.contains(asset) {
                push_unique(&mut warm_vram, *asset);
            }
        }
    }
    (warm_ram, warm_vram)
}

fn package_rooms_touch(package: &PlaytestPackage, a: usize, b: usize) -> bool {
    let Some((ax0, ax1, az0, az1)) = package_room_bounds(package, a) else {
        return false;
    };
    let Some((bx0, bx1, bz0, bz1)) = package_room_bounds(package, b) else {
        return false;
    };
    bx0 <= ax1 && bx1 >= ax0 && bz0 <= az1 && bz1 >= az0
}

fn package_room_bounds(
    package: &PlaytestPackage,
    room_index: usize,
) -> Option<(i32, i32, i32, i32)> {
    let room = package.rooms.get(room_index)?;
    let asset = package.assets.get(room.world_asset_index?)?;
    let world = psx_asset::World::from_bytes(&asset.bytes).ok()?;
    let sector_size = room.sector_size;
    let x0 = room.origin_x.saturating_mul(sector_size);
    let z0 = room.origin_z.saturating_mul(sector_size);
    let x1 = x0.saturating_add((world.width() as i32).saturating_mul(sector_size));
    let z1 = z0.saturating_add((world.depth() as i32).saturating_mul(sector_size));
    Some((x0, x1, z0, z1))
}

fn push_unique(values: &mut Vec<usize>, value: usize) {
    if !values.contains(&value) {
        values.push(value);
    }
}

/// Add `model_index`'s atlas + every clip to a room's residency lists.
/// Meshes are decoded from transient loading scratch into the fixed runtime
/// geometry pools, so their source blobs do not stay resident. Idempotent
/// through the caller's seen-set
/// -- also dedupes within `required_ram` / `required_vram` so
/// callers don't have to.
///
/// Pulled out so the per-room walk can register both placed MeshInstance models
/// and the player character's model without duplicating bookkeeping. Without
/// the player path, a Character whose backing model isn't also placed as a
/// MeshInstance would miss its VRAM atlas and persistent animation clips.
fn include_model_in_residency(
    package: &PlaytestPackage,
    model_index: u16,
    required_ram: &mut Vec<usize>,
    required_vram: &mut Vec<usize>,
) {
    let Some(model) = package.models.get(model_index as usize) else {
        return;
    };
    if let Some(atlas) = model.texture_asset_index {
        if !required_vram.contains(&atlas) {
            required_vram.push(atlas);
        }
    }
    let cf = model.clip_first as usize;
    let cc = model.clip_count as usize;
    if cf + cc > package.model_clips.len() {
        return;
    }
    for clip in &package.model_clips[cf..cf + cc] {
        if !required_ram.contains(&clip.animation_asset_index) {
            required_ram.push(clip.animation_asset_index);
        }
    }
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
    LevelCachedRoomCellRecord,
    LevelCachedRoomSurfaceRecord,
    LevelCachedRoomVertexRecord,
    LevelAssetRecord,
    LevelBoxPropRecord,
    LevelBoxPropSurfaceRecord,
    LevelCylinderPropRecord,
    LevelCylinderPropSurfaceRecord,
    LevelArchPropRecord,
    LevelArchPropSurfaceRecord,
    LevelArchPropCollisionRecord,
    LevelCameraRecord,
    LevelCloudLayerRecord,
    LevelCharacterRecord,
    LevelGameEntityRecord,
    LevelLogicRecord,
    LevelChunkNeighbours,
    LevelChunkRecord,
    LevelCycloramaQuadRecord,
    LevelFarVistaRecord,
    LevelImagePropRecord,
    LevelMaterialAnimation,
    LevelMaterialFlipbook,
    LevelMaterialRecord,
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
    LevelRoomPortalRecord,
    LevelRoomRecord,
    LevelWaterCellRecord,
    LevelRoomSurfaceCacheRecord,
    LevelRoomVisibilityRecord,
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
    LevelVisibilityCellRecord,
    LevelVisibilityPvsRecord,
    LevelWeaponRecord,
    LevelWorldPackEntryRecord,
    MaterialIndex,
    MaterialSlot,
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
    RoomResidencyRecord,
    UiNodeIndex,
    VisibilityCellIndex,
    WeaponHitboxIndex,
    WeaponHitboxRecord,
    WeaponAppearanceRecord,
    WeaponHitShapeRecord,
    WeaponIndex,
};

";
