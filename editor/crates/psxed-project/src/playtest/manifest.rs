//! Manifest and generated-asset writer for editor-playtest.

use std::fmt::Write as _;

use super::*;

pub fn write_package(package: &PlaytestPackage, generated_dir: &Path) -> std::io::Result<()> {
    let rooms_dir = generated_dir.join(ROOMS_DIRNAME);
    let textures_dir = generated_dir.join(TEXTURES_DIRNAME);
    let models_dir = generated_dir.join(MODELS_DIRNAME);
    std::fs::create_dir_all(&rooms_dir)?;
    std::fs::create_dir_all(&textures_dir)?;
    std::fs::create_dir_all(&models_dir)?;
    purge_directory_files(&rooms_dir, "psxw")?;
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

    let manifest = render_manifest_source(package);
    std::fs::write(generated_dir.join(COOKED_MANIFEST_FILENAME), manifest)?;
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
        let _ = writeln!(
            out,
            "pub static {}: &[u8] = include_bytes!(\"{include_path}\");",
            asset_static_name(asset, i),
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

    out.push_str("/// Rooms with material-slice metadata.\n");
    out.push_str("pub static ROOMS: &[LevelRoomRecord] = &[\n");
    for (room_index, room) in package.rooms.iter().enumerate() {
        let far_vista_texture_assets = if room.far_vista.texture_asset_indices.is_empty() {
            "&[]".to_string()
        } else {
            format!("FAR_VISTA_TEXTURES_{room_index}")
        };
        let _ = writeln!(
            out,
            "    LevelRoomRecord {{ name: {:?}, world_asset: AssetId({}), origin_x: {}, origin_z: {}, sector_size: {}, material_first: MaterialIndex({}), material_count: {}, fog_rgb: [{}, {}, {}], fog_near: {}, fog_far: {}, sky: LevelSkyRecord {{ top_rgb: [{}, {}, {}], horizon_rgb: [{}, {}, {}], bottom_rgb: [{}, {}, {}], horizon_percent: {}, flags: {} }}, far_vista: LevelFarVistaRecord {{ texture_assets: {}, radius: {}, height: {}, vertical_offset: {}, segments: {}, rotation_degrees: {}, tint_rgb: [{}, {}, {}], flags: {} }}, camera: LevelCameraRecord {{ distance: {}, height: {}, target_height: {}, min_floor_clearance: {} }}, flags: {} }},",
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
            room.sky.flags,
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
            "    LevelVisibilityCellRecord {{ room: RoomIndex({}), x: {}, z: {}, min_y: {}, max_y: {}, portal_mask: {}, blocker_mask: {}, flags: {} }},",
            cell.room,
            cell.x,
            cell.z,
            cell.min_y,
            cell.max_y,
            cell.portal_mask,
            cell.blocker_mask,
            cell.flags,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Per-room generated room-surface cache slices.\n");
    out.push_str("pub static ROOM_SURFACE_CACHES: &[LevelRoomSurfaceCacheRecord] = &[\n");
    for cache in &package.room_surface_caches {
        let _ = writeln!(
            out,
            "    LevelRoomSurfaceCacheRecord {{ room: RoomIndex({}), cell_first: {}, cell_count: {}, vertex_first: {}, vertex_count: {}, surface_first: {}, surface_count: {}, flags: 0 }},",
            cache.room,
            cache.cell_first,
            cache.cell_count,
            cache.vertex_first,
            cache.vertex_count,
            cache.surface_first,
            cache.surface_count,
        );
    }
    out.push_str("];\n\n");

    out.push_str("/// Generated cached room cells.\n");
    out.push_str("pub static ROOM_CACHE_CELLS: &[LevelCachedRoomCellRecord] = &[\n");
    for cell in &package.room_cache_cells {
        let _ = writeln!(
            out,
            "    LevelCachedRoomCellRecord {{ x: {}, z: {}, min_y: {}, max_y: {}, visibility_center: [{}, {}, {}], visibility_radius: {}, surface_first: {}, surface_count: {} }},",
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
        );
    }
    out.push_str("];\n\n");

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

    out.push_str("/// Generated cached room surfaces.\n");
    out.push_str("pub static ROOM_CACHE_SURFACES: &[LevelCachedRoomSurfaceRecord] = &[\n");
    for surface in &package.room_cache_surfaces {
        let _ = writeln!(
            out,
            "    LevelCachedRoomSurfaceRecord {{ material_slot: {}, vertex_indices: [{}, {}, {}, {}], sample_sx: {}, sample_sz: {}, sample_ordinal: {}, uvs: [({}, {}), ({}, {}), ({}, {}), ({}, {})], baked_vertex_rgb: [({}, {}, {}), ({}, {}, {}), ({}, {}, {}), ({}, {}, {})], kind_flags: {}, wall_direction: {}, split: {}, triangle_index: {} }},",
            surface.material_slot,
            surface.vertex_indices[0],
            surface.vertex_indices[1],
            surface.vertex_indices[2],
            surface.vertex_indices[3],
            surface.sample_sx,
            surface.sample_sz,
            surface.sample_ordinal,
            surface.uvs[0].0,
            surface.uvs[0].1,
            surface.uvs[1].0,
            surface.uvs[1].1,
            surface.uvs[2].0,
            surface.uvs[2].1,
            surface.uvs[3].0,
            surface.uvs[3].1,
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
            "    LevelImagePropRecord {{ room: RoomIndex({}), texture_asset: AssetId({}), x: {}, y: {}, z: {}, yaw: {}, width: {}, height: {}, tint_rgb: [{}, {}, {}], flags: {} }},",
            prop.room,
            prop.texture_asset_index,
            prop.x,
            prop.y,
            prop.z,
            prop.yaw,
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
        let bytes = std::fs::read(crate::default_project_dir().join("assets/textures/floor.psxt"))
            .expect("starter floor texture exists");
        let asset = PlaytestAsset {
            kind: PlaytestAssetKind::Texture,
            bytes,
            filename: "texture_000.psxt".to_string(),
            source_label: "Floor".to_string(),
        };

        assert_eq!(asset_vram_bytes(&asset), 16 * 64 * 2 + 16 * 2);
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
    LevelCharacterRecord,
    LevelChunkNeighbours,
    LevelChunkRecord,
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
