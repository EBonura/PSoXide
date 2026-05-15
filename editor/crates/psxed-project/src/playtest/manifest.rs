//! Manifest and generated-asset writer for editor-playtest.

use std::fmt::Write as _;

use super::*;

const STREAMED_ROOM_SLOT_BYTES: usize = 32 * 1024;

pub fn write_package(package: &PlaytestPackage, generated_dir: &Path) -> std::io::Result<()> {
    let rooms_dir = generated_dir.join(ROOMS_DIRNAME);
    let stream_chunks_dir = generated_dir.join(STREAM_CHUNKS_DIRNAME);
    let textures_dir = generated_dir.join(TEXTURES_DIRNAME);
    let models_dir = generated_dir.join(MODELS_DIRNAME);
    std::fs::create_dir_all(&rooms_dir)?;
    std::fs::create_dir_all(&stream_chunks_dir)?;
    std::fs::create_dir_all(&textures_dir)?;
    std::fs::create_dir_all(&models_dir)?;
    purge_directory_files(&rooms_dir, "psxw")?;
    purge_directory_files(&stream_chunks_dir, "psxc")?;
    purge_directory_files(&textures_dir, "psxt")?;
    // Models live in per-model subfolders so the recursive
    // purge needs to traverse one level deeper than rooms /
    // textures.
    purge_models_dir(&models_dir)?;

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
    for room_index in 0..package.rooms.len().min(u16::MAX as usize + 1) {
        let payload = streamed_room_chunk_payload(package, room_index as u16)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(
            stream_chunks_dir.join(streamed_room_chunk_filename(room_index as u16)),
            payload,
        )?;
    }

    let manifest = render_manifest_source(package);
    std::fs::write(generated_dir.join(COOKED_MANIFEST_FILENAME), manifest)?;
    std::fs::write(
        generated_dir.join(WORLD_PACK_ORDER_FILENAME),
        render_world_pack_order(package),
    )?;
    Ok(())
}

/// Render `package` as a Rust source string the runtime example
/// can `include!`. Imports types from `psx_level` rather than
/// re-defining them so the writer here and the reader there
/// stay in lockstep.
pub fn render_manifest_source(package: &PlaytestPackage) -> String {
    let mut out = String::new();
    out.push_str(MANIFEST_HEADER);

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
            "/// {} — {}",
            asset_static_name(asset, i),
            asset.source_label,
        );
        if asset.kind == PlaytestAssetKind::RoomWorld {
            let _ = writeln!(out, "#[cfg(feature = \"cd-stream-bench\")]");
            let _ = writeln!(
                out,
                "pub static {}: &[u8] = &[];",
                asset_static_name(asset, i)
            );
            let _ = writeln!(out, "#[cfg(not(feature = \"cd-stream-bench\"))]");
            let _ = writeln!(
                out,
                "pub static {}: &[u8] = include_bytes!(\"{include_path}\");",
                asset_static_name(asset, i),
            );
        } else {
            let _ = writeln!(
                out,
                "pub static {}: &[u8] = include_bytes!(\"{include_path}\");",
                asset_static_name(asset, i),
            );
        }
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
        let _ = writeln!(
            out,
            "    LevelAssetRecord {{ id: AssetId({i}), kind: {kind}, bytes: {static_name}, ram_bytes: {static_name}.len() as u32, vram_bytes: {vram_bytes}, flags: 0 }},"
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Per-room material bindings — slot the `.psxw` stores → texture asset.\n");
    out.push_str("pub static MATERIALS: &[LevelMaterialRecord] = &[\n");
    for material in &package.materials {
        let flags = material_flags_for_sidedness(material.face_sidedness);
        let _ = writeln!(
            out,
            "    LevelMaterialRecord {{ room: RoomIndex({}), local_slot: MaterialSlot({}), texture_asset: AssetId({}), tint_rgb: [{}, {}, {}], flags: {} }},",
            material.room,
            material.local_slot,
            material.texture_asset_index,
            material.tint_rgb[0],
            material.tint_rgb[1],
            material.tint_rgb[2],
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
            "    LevelRoomRecord {{ name: {:?}, world_asset: AssetId({}), origin_x: {}, origin_z: {}, sector_size: {}, material_first: MaterialIndex({}), material_count: {}, fog_rgb: [{}, {}, {}], fog_near: {}, fog_far: {}, sky: LevelSkyRecord {{ top_rgb: [{}, {}, {}], horizon_rgb: [{}, {}, {}], bottom_rgb: [{}, {}, {}], horizon_percent: {}, horizon_thickness_percent: {}, skybox_columns: {}, skybox_rows: {}, flags: {}, cyclorama_quads: {}, cloud_layer: LevelCloudLayerRecord {{ texture_asset: AssetId({}), color_rgb: [{}, {}, {}], density: {}, altitude: {}, extent: {}, tile_count: {}, scroll_speed: [{}, {}], noise_seed: 0x{:08x}, flags: {} }} }}, far_vista: LevelFarVistaRecord {{ texture_assets: {}, radius: {}, height: {}, vertical_offset: {}, segments: {}, rotation_degrees: {}, tint_rgb: [{}, {}, {}], flags: {} }}, camera: LevelCameraRecord {{ distance: {}, height: {}, target_height: {}, min_floor_clearance: {} }}, flags: {} }},",
            room.name,
            room.world_asset_index,
            room.origin_x,
            room.origin_z,
            room.sector_size,
            room.material_first,
            room.material_count,
            room.fog_rgb[0],
            room.fog_rgb[1],
            room.fog_rgb[2],
            room.fog_near,
            room.fog_far,
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
            room.camera.min_floor_clearance,
            room.flags,
        );
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
    for entry in world_pack_toc(package) {
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

    // Per-room residency: required RAM = the room's world
    // asset + every model mesh + every animation clip
    // referenced by an instance OR by the player character in
    // this room; required VRAM = every distinct texture asset
    // (room materials + far-vista panels + model atlases)
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
            "    LevelModelClipBoundsRecord {{ model: ModelIndex({}), clip: ModelClipTableIndex({}), first_frame: ModelFrameBoundsIndex({}), frame_count: {}, flags: 0 }},",
            bounds.model, bounds.clip, bounds.first_frame, bounds.frame_count,
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

    out.push_str("/// Cooked models — instances reference these by index.\n");
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
            "    LevelModelInstanceRecord {{ room: RoomIndex({}), model: ModelIndex({}), clip: {clip}, x: {}, y: {}, z: {}, yaw: {}, flags: {} }},",
            inst.room, inst.model, inst.x, inst.y, inst.z, inst.yaw, inst.flags,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Placed flat image props, room-local coordinates.\n");
    out.push_str("pub static IMAGE_PROPS: &[LevelImagePropRecord] = &[\n");
    for prop in &package.image_props {
        let _ = writeln!(
            out,
            "    LevelImagePropRecord {{ room: RoomIndex({}), texture_asset: AssetId({}), x: {}, y: {}, z: {}, pitch: {}, yaw: {}, roll: {}, width: {}, height: {}, tint_rgb: [{}, {}, {}], flags: {} }},",
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
            prop.flags,
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
            "    LevelWeaponRecord {{ name: {:?}, model: {model}, default_character_socket: {:?}, grip_name: {:?}, grip_translation: [{}, {}, {}], grip_rotation_q12: [{}, {}, {}], hitbox_first: WeaponHitboxIndex({}), hitbox_count: {}, flags: 0 }},",
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
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Equipment components, room-local parent transforms.\n");
    out.push_str("pub static EQUIPMENT: &[EquipmentRecord] = &[\n");
    for equipment in &package.equipment {
        let _ = writeln!(
            out,
            "    EquipmentRecord {{ room: RoomIndex({}), weapon: WeaponIndex({}), x: {}, y: {}, z: {}, yaw: {}, character_socket: {:?}, weapon_grip: {:?}, flags: {} }},",
            equipment.room,
            equipment.weapon,
            equipment.x,
            equipment.y,
            equipment.z,
            equipment.yaw,
            equipment.character_socket,
            equipment.weapon_grip,
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

    out.push_str("/// Cooked Character resources — gameplay metadata layered on top of MODELS.\n");
    out.push_str("pub static CHARACTERS: &[LevelCharacterRecord] = &[\n");
    for character in &package.characters {
        let clip_or_none = |slot: u16| -> String {
            if slot == CHARACTER_CLIP_NONE {
                "CHARACTER_CLIP_NONE".to_string()
            } else {
                format!("OptionalModelClipIndex::some(ModelClipIndex({slot}))")
            }
        };
        let _ = writeln!(
            out,
            "    LevelCharacterRecord {{ model: ModelIndex({}), idle_clip: ModelClipIndex({}), walk_clip: ModelClipIndex({}), run_clip: {}, turn_clip: {}, radius: {}, height: {}, walk_speed: {}, run_speed: {}, turn_speed_degrees_per_second: {}, camera_distance: {}, camera_height: {}, camera_target_height: {}, flags: 0 }},",
            character.model,
            character.idle_clip,
            character.walk_clip,
            clip_or_none(character.run_clip),
            clip_or_none(character.turn_clip),
            character.radius,
            character.height,
            character.walk_speed,
            character.run_speed,
            character.turn_speed_degrees_per_second,
            character.camera_distance,
            character.camera_height,
            character.camera_target_height,
        );
    }
    out.push_str("];\n\n");

    match package.player_controller {
        Some(pc) => {
            let _ = writeln!(
                out,
                "/// Player controller — spawn + Character that drives the player.\npub static PLAYER_CONTROLLER: Option<PlayerControllerRecord> = Some(PlayerControllerRecord {{ spawn: PlayerSpawnRecord {{ room: RoomIndex({}), x: {}, y: {}, z: {}, yaw: {}, flags: {} }}, character: CharacterIndex({}), flags: 0 }});",
                pc.spawn.room, pc.spawn.x, pc.spawn.y, pc.spawn.z, pc.spawn.yaw, pc.spawn.flags, pc.character,
            );
        }
        None => {
            out.push_str(
                "/// Player controller — `None` means no playable character was authored.\n\
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

fn world_pack_toc(package: &PlaytestPackage) -> Vec<psx_iso::WorldPackBuildEntry> {
    let mut chunks: Vec<(u32, Vec<u8>)> = Vec::new();
    for room in world_pack_order(package) {
        let payload =
            streamed_room_chunk_payload(package, room).expect("valid streamed room chunk payload");
        chunks.push((room as u32, payload));
    }
    let refs = chunks
        .iter()
        .map(|(room, bytes)| (*room, bytes.as_slice()))
        .collect::<Vec<_>>();
    psx_iso::build_world_pack_layout(&refs).entries
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
    let asset = package
        .assets
        .get(room_record.world_asset_index)
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

    let collision_payload = compact_collision_payload(&asset.bytes)?;
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

fn compact_collision_payload(bytes: &[u8]) -> Result<Vec<u8>, String> {
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
    Ok(())
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

#[allow(clippy::too_many_arguments)]
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
    world_pack_order_from_chunks(
        package.rooms.len(),
        package.spawn.map(|spawn| spawn.room),
        &package.chunks,
    )
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

const fn material_flags_for_sidedness(sidedness: crate::MaterialFaceSidedness) -> u16 {
    match sidedness {
        crate::MaterialFaceSidedness::Front => 0,
        crate::MaterialFaceSidedness::Back => 1,
        crate::MaterialFaceSidedness::Both => 2,
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
    // A failed cook must not leave a stale cooked manifest for
    // subsequent runtime builds. If validation fails before a
    // package exists, editor-playtest falls back to the tracked
    // placeholder manifest.
    let cooked_manifest = generated_dir.join(COOKED_MANIFEST_FILENAME);
    if cooked_manifest.exists() {
        std::fs::remove_file(&cooked_manifest)?;
    }
    let (package, report) = build_package(project, project_root);
    if let Some(package) = package {
        write_package(&package, generated_dir)?;
    }
    Ok(report)
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
    for prop in &package.image_props {
        if prop.room == room_index as u16 {
            push_unique(&mut required_vram, prop.texture_asset_index);
        }
    }
    let mut required_ram: Vec<usize> = vec![room.world_asset_index];

    // Models the room references -- placed MeshInstance bindings
    // plus the player controller's character when its spawn lives
    // in this room.
    let room_index = room_index as u16;
    let mut seen_models: Vec<u16> = Vec::new();
    for inst in &package.model_instances {
        if inst.room != room_index || seen_models.contains(&inst.model) {
            continue;
        }
        seen_models.push(inst.model);
        include_model_in_residency(package, inst.model, &mut required_ram, &mut required_vram);
    }
    if let Some(pc) = package.player_controller {
        if pc.spawn.room == room_index {
            let model = package.characters[pc.character as usize].model;
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
    let asset = package.assets.get(room.world_asset_index)?;
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

/// Add `model_index`'s mesh + atlas + every clip to a room's
/// residency lists. Idempotent through the caller's seen-set
/// -- also dedupes within `required_ram` / `required_vram` so
/// callers don't have to.
///
/// Pulled out so the per-room walk can register both placed
/// MeshInstance models and the player character's model
/// without duplicating bookkeeping. Without the player path,
/// a Character whose backing model isn't also placed as a
/// MeshInstance would be missing from residency entirely --
/// the runtime would then render the player from un-resident
/// bytes the moment the room loaded.
fn include_model_in_residency(
    package: &PlaytestPackage,
    model_index: u16,
    required_ram: &mut Vec<usize>,
    required_vram: &mut Vec<usize>,
) {
    let Some(model) = package.models.get(model_index as usize) else {
        return;
    };
    if !required_ram.contains(&model.mesh_asset_index) {
        required_ram.push(model.mesh_asset_index);
    }
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
mod tests {
    use super::*;

    #[test]
    fn room_texture_vram_bytes_match_runtime_compact_tile_upload() {
        let bytes = std::fs::read(
            crate::default_project_dir().join("assets/textures/delven_01_slateflr1a_q2.psxt"),
        )
        .expect("starter Delven texture exists");
        let asset = PlaytestAsset {
            kind: PlaytestAssetKind::Texture,
            bytes,
            filename: "texture_000.psxt".to_string(),
            source_label: "Delven slateflr1a q2".to_string(),
        };

        assert_eq!(asset_vram_bytes(&asset), 8 * 32 * 2 + 16 * 2);
    }

    #[test]
    fn model_atlas_vram_bytes_match_runtime_atlas_upload() {
        let bytes = std::fs::read(
            crate::default_project_dir()
                .join("assets/models/obsidian_wraith/obsidian_wraith_128x128_8bpp.psxt"),
        )
        .expect("starter wraith atlas exists");
        let asset = PlaytestAsset {
            kind: PlaytestAssetKind::Texture,
            bytes,
            filename: "models/model_000_obsidian_wraith/atlas.psxt".to_string(),
            source_label: "Obsidian Wraith atlas".to_string(),
        };

        assert_eq!(asset_vram_bytes(&asset), 64 * 128 * 2 + 256 * 2);
    }

    #[test]
    fn world_pack_order_starts_at_spawn_and_walks_chunk_neighbours() {
        let chunks = [
            test_chunk(0, 0, 0, 0, [None, Some(1), Some(2), None]),
            test_chunk(1, 0, 1, 0, [None, None, Some(3), Some(0)]),
            test_chunk(2, 0, 0, 1, [Some(0), Some(3), None, None]),
            test_chunk(3, 0, 1, 1, [Some(1), None, None, Some(2)]),
        ];

        assert_eq!(
            world_pack_order_from_chunks(4, Some(2), &chunks),
            vec![2, 0, 3, 1]
        );
    }

    #[test]
    fn world_pack_order_appends_disconnected_chunks_by_proximity() {
        let chunks = [
            test_chunk(0, 10, 0, 0, [None; 4]),
            test_chunk(1, 11, 50, 0, [None; 4]),
            test_chunk(2, 12, 5, 0, [None; 4]),
        ];

        assert_eq!(
            world_pack_order_from_chunks(3, Some(0), &chunks),
            vec![0, 2, 1]
        );
    }

    #[test]
    fn world_pack_toc_uses_same_layout_as_pack_builder() {
        let mut package = PlaytestPackage::default();
        package.assets = vec![
            test_room_asset(static_lit_test_room_bytes(), 0),
            test_room_asset(static_lit_test_room_bytes(), 1),
            test_room_asset(static_lit_test_room_bytes(), 2),
        ];
        package.rooms = vec![test_room(0), test_room(1), test_room(2)];
        package.chunks = vec![
            test_chunk(0, 0, 0, 0, [None, Some(1), Some(2), None]),
            test_chunk(1, 0, 1, 0, [None, None, None, Some(0)]),
            test_chunk(2, 0, 0, 1, [Some(0), None, None, None]),
        ];
        package.spawn = Some(PlaytestSpawn {
            room: 2,
            x: 0,
            y: 0,
            z: 0,
            yaw: 0,
            flags: 1,
        });

        let order = world_pack_order(&package);
        assert_eq!(order, vec![2, 0, 1]);
        let refs = order
            .iter()
            .map(|room| {
                (
                    *room as u32,
                    streamed_room_chunk_payload(&package, *room).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let refs = refs
            .iter()
            .map(|(room, bytes)| (*room, bytes.as_slice()))
            .collect::<Vec<_>>();
        assert_eq!(
            world_pack_toc(&package),
            psx_iso::build_world_pack_layout(&refs).entries
        );

        let manifest = render_manifest_source(&package);
        assert!(manifest.contains("pub const WORLD_PACK_START_LBA: u32 = 54;"));
        assert!(manifest.contains("pub static WORLD_PACK_TOC: &[LevelWorldPackEntryRecord]"));
        assert!(manifest.contains("LevelWorldPackEntryRecord { room: RoomIndex(2), sector_offset: 1, sector_count: 1, byte_size: 144"));
    }

    #[test]
    fn cd_stream_manifest_does_not_embed_room_bytes_or_global_cache_tables() {
        let mut package = PlaytestPackage::default();
        package.assets = vec![test_room_asset(static_lit_test_room_bytes(), 0)];
        package.rooms = vec![test_room(0)];
        package.room_surface_caches = vec![PlaytestRoomSurfaceCache {
            room: 0,
            cell_first: 0,
            cell_count: 0,
            cell_vertex_first: 0,
            cell_vertex_count: 0,
            vertex_first: 0,
            vertex_count: 0,
            surface_first: 0,
            surface_count: 0,
        }];

        let src = render_manifest_source(&package);
        assert!(src.contains("#[cfg(feature = \"cd-stream-bench\")]\npub static ASSET_000_ROOM_000_BYTES: &[u8] = &[];"));
        assert!(src.contains("#[cfg(not(feature = \"cd-stream-bench\"))]\npub static ASSET_000_ROOM_000_BYTES: &[u8] = include_bytes!(\"rooms/room_000.psxw\");"));
        assert!(src.contains("#[cfg(feature = \"cd-stream-bench\")]\n/// Stream builds read room-surface cache slices from `.psxc` chunks.\npub static ROOM_SURFACE_CACHES: &[LevelRoomSurfaceCacheRecord] = &[];"));
        assert!(src.contains("#[cfg(feature = \"cd-stream-bench\")]\n/// Stream builds read cached room cells from `.psxc` chunks.\npub static ROOM_CACHE_CELLS: &[LevelCachedRoomCellRecord] = &[];"));
        assert!(src.contains("#[cfg(feature = \"cd-stream-bench\")]\n/// Stream builds read cached cell vertex indices from `.psxc` chunks.\npub static ROOM_CACHE_CELL_VERTICES: &[u16] = &[];"));
        assert!(src.contains("#[cfg(feature = \"cd-stream-bench\")]\n/// Stream builds read cached room vertices from `.psxc` chunks.\npub static ROOM_CACHE_VERTICES: &[LevelCachedRoomVertexRecord] = &[];"));
        assert!(src.contains("#[cfg(feature = \"cd-stream-bench\")]\n/// Stream builds read cached room surfaces from `.psxc` chunks.\npub static ROOM_CACHE_SURFACES: &[LevelCachedRoomSurfaceRecord] = &[];"));
    }

    #[test]
    fn streamed_room_chunk_payload_splits_collision_and_render_cache_records() {
        let mut package = PlaytestPackage::default();
        package.assets = vec![test_room_asset(static_lit_test_room_bytes(), 0)];
        package.rooms = vec![test_room(0)];
        package.room_surface_caches = vec![PlaytestRoomSurfaceCache {
            room: 0,
            cell_first: 0,
            cell_count: 1,
            cell_vertex_first: 0,
            cell_vertex_count: 4,
            vertex_first: 0,
            vertex_count: 1,
            surface_first: 0,
            surface_count: 1,
        }];
        package.room_cache_cells = vec![PlaytestCachedRoomCell {
            x: 2,
            z: 3,
            min_y: -4,
            max_y: 5,
            visibility_center: [6, 7, 8],
            visibility_radius: 9,
            surface_first: 10,
            surface_count: 11,
            vertex_first: 0,
            vertex_count: 4,
        }];
        package.room_cache_cell_vertices = vec![0, 1, 2, 3];
        package.room_cache_vertices = vec![PlaytestCachedRoomVertex {
            x: 12,
            y: 13,
            z: 14,
        }];
        package.room_cache_surfaces = vec![PlaytestCachedRoomSurface {
            material_slot: 15,
            vertex_indices: [0, 1, 2, 3],
            sample_sx: 16,
            sample_sz: 17,
            sample_ordinal: 18,
            uv_words: [0x1413, 0x1615, 0x1817, 0x1a19],
            baked_vertex_rgb: [(27, 28, 29), (30, 31, 32), (33, 34, 35), (36, 37, 38)],
            kind_flags: 39,
            wall_direction: 40,
            split: 41,
            triangle_index: 42,
        }];

        let payload = streamed_room_chunk_payload(&package, 0).unwrap();
        assert_eq!(
            &payload[..8],
            psx_level::STREAMED_ROOM_CHUNK_MAGIC.as_slice()
        );
        assert_eq!(u32_at(&payload, 8), psx_level::STREAMED_ROOM_CHUNK_VERSION);
        assert_eq!(u32_at(&payload, 12), 0);
        assert_eq!(u32_at(&payload, 16), payload.len() as u32);
        assert_eq!(u32_at(&payload, 20), 64);
        assert_eq!(u32_at(&payload, 24), 80);
        assert_eq!(u32_at(&payload, 28), 144);
        assert_eq!(u32_at(&payload, 32), 1);
        assert_eq!(u32_at(&payload, 36), 188);
        assert_eq!(u32_at(&payload, 40), 1);
        assert_eq!(u32_at(&payload, 44), 200);
        assert_eq!(u32_at(&payload, 48), 1);
        assert_eq!(u32_at(&payload, 52), 180);
        assert_eq!(u32_at(&payload, 56), 4);
        assert_eq!(
            u32_at(&payload, 60),
            psx_level::STREAMED_ROOM_CHUNK_FLAG_COLLISION_COMPACT
        );
        assert_eq!(
            &payload[64..72],
            psx_level::COMPACT_COLLISION_MAGIC.as_slice()
        );
        assert_eq!(u16_at(&payload, 144), 2);
        assert_eq!(i32_at(&payload, 148), -4);
        assert_eq!(u16_at(&payload, 176), 0);
        assert_eq!(u16_at(&payload, 180), 0);
        assert_eq!(u16_at(&payload, 186), 3);
        assert_eq!(i32_at(&payload, 188), 12);
        assert_eq!(u16_at(&payload, 200), 15);
        assert_eq!(payload[239], 42);
    }

    #[test]
    fn streamed_room_chunk_memory_report_accounts_for_collision_render_and_padding() {
        let mut package = PlaytestPackage::default();
        package.assets = vec![test_room_asset(static_lit_test_room_bytes(), 0)];
        package.rooms = vec![test_room(0)];
        package.room_surface_caches = vec![PlaytestRoomSurfaceCache {
            room: 0,
            cell_first: 0,
            cell_count: 1,
            cell_vertex_first: 0,
            cell_vertex_count: 4,
            vertex_first: 0,
            vertex_count: 1,
            surface_first: 0,
            surface_count: 1,
        }];
        package.room_cache_cells = vec![PlaytestCachedRoomCell {
            x: 2,
            z: 3,
            min_y: -4,
            max_y: 5,
            visibility_center: [6, 7, 8],
            visibility_radius: 9,
            surface_first: 10,
            surface_count: 11,
            vertex_first: 0,
            vertex_count: 4,
        }];
        package.room_cache_cell_vertices = vec![0, 1, 2, 3];
        package.room_cache_vertices = vec![PlaytestCachedRoomVertex {
            x: 12,
            y: 13,
            z: 14,
        }];
        package.room_cache_surfaces = vec![PlaytestCachedRoomSurface {
            material_slot: 15,
            vertex_indices: [0, 1, 2, 3],
            sample_sx: 16,
            sample_sz: 17,
            sample_ordinal: 18,
            uv_words: [0x1413, 0x1615, 0x1817, 0x1a19],
            baked_vertex_rgb: [(27, 28, 29), (30, 31, 32), (33, 34, 35), (36, 37, 38)],
            kind_flags: 39,
            wall_direction: 40,
            split: 41,
            triangle_index: 42,
        }];

        let report = streamed_room_chunk_memory_report(&package).unwrap();
        assert_eq!(report.chunks.len(), 1);
        let chunk = report.chunks[0];
        assert_eq!(chunk.room, 0);
        assert_eq!(
            chunk.payload_bytes,
            streamed_room_chunk_payload(&package, 0).unwrap().len()
        );
        assert_eq!(chunk.collision_bytes, 80);
        assert_eq!(chunk.render_cell_bytes, 36);
        assert_eq!(chunk.render_cell_vertex_bytes, 8);
        assert_eq!(chunk.render_vertex_bytes, 12);
        assert_eq!(chunk.render_surface_bytes, 40);
        assert_eq!(chunk.render_cache_bytes, 96);
        assert_eq!(chunk.alignment_padding_bytes, 0);
        assert_eq!(chunk.sector_count, 1);
        assert_eq!(chunk.stream_bytes, psx_iso::SECTOR_USER_DATA_BYTES);
        assert_eq!(
            chunk.sector_padding_bytes,
            psx_iso::SECTOR_USER_DATA_BYTES - chunk.payload_bytes
        );
        assert_eq!(
            report.totals,
            PlaytestStreamMemoryTotals {
                sector_count: chunk.sector_count,
                payload_bytes: chunk.payload_bytes,
                stream_bytes: chunk.stream_bytes,
                header_bytes: chunk.header_bytes,
                collision_bytes: chunk.collision_bytes,
                render_cell_bytes: chunk.render_cell_bytes,
                render_cell_vertex_bytes: chunk.render_cell_vertex_bytes,
                render_vertex_bytes: chunk.render_vertex_bytes,
                render_surface_bytes: chunk.render_surface_bytes,
                render_cache_bytes: chunk.render_cache_bytes,
                alignment_padding_bytes: chunk.alignment_padding_bytes,
                sector_padding_bytes: chunk.sector_padding_bytes,
            }
        );
    }

    #[test]
    fn compact_collision_payload_matches_runtime_room_collision() {
        let bytes = static_lit_test_room_bytes();
        let payload = compact_collision_payload(&bytes).unwrap();
        assert_eq!(
            payload.len(),
            psx_level::COMPACT_COLLISION_HEADER_BYTES + psx_level::COMPACT_COLLISION_SECTOR_BYTES
        );
        assert_eq!(&payload[..8], psx_level::COMPACT_COLLISION_MAGIC.as_slice());
        assert_eq!(
            u32_at(&payload, psx_level::compact_collision_header::VERSION),
            psx_level::COMPACT_COLLISION_VERSION
        );
        assert_eq!(
            u16_at(&payload, psx_level::compact_collision_header::WIDTH),
            1
        );
        assert_eq!(
            u16_at(&payload, psx_level::compact_collision_header::DEPTH),
            1
        );
        assert_eq!(
            i32_at(&payload, psx_level::compact_collision_header::SECTOR_SIZE),
            1024
        );
        assert_eq!(
            u16_at(&payload, psx_level::compact_collision_header::SECTOR_COUNT),
            1
        );
        assert_eq!(
            &payload[psx_level::compact_collision_header::AMBIENT_RGB
                ..psx_level::compact_collision_header::AMBIENT_RGB + 3],
            &[7, 8, 9]
        );
        let room = psx_engine::CompactCollisionRoom::from_bytes(&payload).unwrap();
        assert_eq!(room.width(), 1);
        assert_eq!(room.depth(), 1);
        assert_eq!(room.ambient_color(), [7, 8, 9]);
        let sector = room.collision().sector(0, 0).unwrap();
        assert!(sector.has_floor());
        assert!(sector.floor_walkable());
        assert_eq!(sector.floor_heights(), [0; 4]);
    }

    fn test_room_asset(bytes: Vec<u8>, index: usize) -> PlaytestAsset {
        PlaytestAsset {
            kind: PlaytestAssetKind::RoomWorld,
            bytes,
            filename: format!("room_{index:03}.psxw"),
            source_label: format!("Room {index}"),
        }
    }

    fn test_room(world_asset_index: usize) -> PlaytestRoom {
        PlaytestRoom {
            name: format!("Room {world_asset_index}"),
            world_asset_index,
            origin_x: 0,
            origin_z: 0,
            sector_size: 1024,
            material_first: 0,
            material_count: 0,
            fog_rgb: [0, 0, 0],
            fog_near: 0,
            fog_far: 0,
            sky: PlaytestSky {
                top_rgb: [0, 0, 0],
                horizon_rgb: [0, 0, 0],
                bottom_rgb: [0, 0, 0],
                horizon_percent: 50,
                horizon_thickness_percent: 8,
                skybox_columns: 16,
                skybox_rows: 10,
                flags: 0,
                cyclorama_quads: Vec::new(),
                cloud_layer: PlaytestCloudLayer {
                    texture_asset_index: None,
                    color_rgb: [0, 0, 0],
                    density: 0,
                    altitude: 0,
                    extent: 0,
                    tile_count: 0,
                    scroll_speed: [0, 0],
                    noise_seed: 0,
                    flags: 0,
                },
            },
            far_vista: PlaytestFarVista {
                texture_asset_indices: Vec::new(),
                radius: 0,
                height: 0,
                vertical_offset: 0,
                segments: 0,
                rotation_degrees: 0,
                tint_rgb: [0, 0, 0],
                flags: 0,
            },
            camera: PlaytestCamera {
                distance: 0,
                height: 0,
                target_height: 0,
                min_floor_clearance: 0,
            },
            flags: 0,
        }
    }

    fn static_lit_test_room_bytes() -> Vec<u8> {
        let asset_header = psxed_format::AssetHeader::SIZE;
        let world_header = psxed_format::world::WorldHeader::SIZE;
        let sector_bytes = psxed_format::world::SectorRecord::SIZE;
        let light_bytes = 2 * psxed_format::world::SurfaceLightRecord::SIZE;
        let payload_len = world_header + sector_bytes + light_bytes;
        let mut out = vec![0u8; asset_header + payload_len];
        out[0..4].copy_from_slice(&psxed_format::world::MAGIC);
        out[4..6].copy_from_slice(&psxed_format::world::VERSION.to_le_bytes());
        out[8..12].copy_from_slice(&(payload_len as u32).to_le_bytes());

        let wh = asset_header;
        out[wh..wh + 2].copy_from_slice(&1u16.to_le_bytes());
        out[wh + 2..wh + 4].copy_from_slice(&1u16.to_le_bytes());
        out[wh + 4..wh + 8].copy_from_slice(&1024i32.to_le_bytes());
        out[wh + 8..wh + 10].copy_from_slice(&1u16.to_le_bytes());
        out[wh + 14..wh + 17].copy_from_slice(&[7, 8, 9]);
        out[wh + 17] = psxed_format::world::world_flags::STATIC_VERTEX_LIGHTING;
        out[wh + 18..wh + 20].copy_from_slice(&2u16.to_le_bytes());

        let sector = wh + world_header;
        out[sector] = psxed_format::world::sector_flags::HAS_FLOOR
            | psxed_format::world::sector_flags::FLOOR_WALKABLE;
        out
    }

    fn test_chunk(
        room: u16,
        authored_room: u32,
        origin_x: i32,
        origin_z: i32,
        neighbours: [Option<u16>; 4],
    ) -> PlaytestChunk {
        PlaytestChunk {
            room,
            authored_room,
            chunk_index: room,
            origin_x,
            origin_z,
            width: 1,
            depth: 1,
            neighbours,
            triangles: 0,
            psxw_bytes: 0,
            static_lit_bytes: 0,
            populated_cells: 0,
            flags: 0,
        }
    }

    fn u16_at(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
    }

    fn u32_at(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ])
    }

    fn i32_at(bytes: &[u8], offset: usize) -> i32 {
        i32::from_le_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ])
    }
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
    AssetId,
    AssetKind,
    CHARACTER_CLIP_NONE,
    CharacterIndex,
    EntityKind,
    EntityRecord,
    EquipmentRecord,
    LevelCachedRoomCellRecord,
    LevelCachedRoomSurfaceRecord,
    LevelCachedRoomVertexRecord,
    LevelAssetRecord,
    LevelCameraRecord,
    LevelCloudLayerRecord,
    LevelCharacterRecord,
    LevelChunkNeighbours,
    LevelChunkRecord,
    LevelCycloramaQuadRecord,
    LevelFarVistaRecord,
    LevelImagePropRecord,
    LevelMaterialRecord,
    LevelModelClipBoundsRecord,
    LevelModelClipRecord,
    LevelModelFrameBoundsRecord,
    LevelModelInstanceRecord,
    LevelModelRecord,
    LevelModelSocketRecord,
    LevelRoomRecord,
    LevelRoomSurfaceCacheRecord,
    LevelRoomVisibilityRecord,
    LevelSkyRecord,
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
    PlayerControllerRecord,
    PlayerSpawnRecord,
    PointLightRecord,
    OptionalModelClipIndex,
    ResourceSlot,
    RoomIndex,
    RoomResidencyRecord,
    VisibilityCellIndex,
    WeaponHitboxIndex,
    WeaponHitboxRecord,
    WeaponHitShapeRecord,
    WeaponIndex,
};

";
