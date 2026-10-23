//! Manifest and generated-asset writer for editor-playtest.

use std::fmt::Write as _;

use super::*;
use crate::{UiFontChoice, UiGradientDirection, UiImageEffect, UiNodeKind, UiValueBinding};

const CD_SECTOR_BYTES: usize = 2048;

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
    let write_started = std::time::Instant::now();
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
    let xa_music_dir = generated_dir.join(XA_MUSIC_DIRNAME);
    std::fs::create_dir_all(&stream_chunks_dir)?;
    std::fs::create_dir_all(&ui_stream_chunks_dir)?;
    std::fs::create_dir_all(&textures_dir)?;
    std::fs::create_dir_all(&models_dir)?;
    std::fs::create_dir_all(&ui_sfx_dir)?;
    std::fs::create_dir_all(&xa_music_dir)?;
    purge_directory_files(&stream_chunks_dir, "psxc")?;
    purge_directory_files(&ui_stream_chunks_dir, "psxt")?;
    purge_directory_files(&textures_dir, "psxt")?;
    purge_directory_files(&ui_sfx_dir, "psau")?;
    purge_directory_files(&xa_music_dir, "XA")?;
    // Models live in per-model subfolders so the recursive
    // purge needs to traverse one level deeper than rooms /
    // textures.
    purge_models_dir(&models_dir)?;

    // The grid level core and the room chunks ride `WORLD.PAK` and are read off
    // the disc: chunk 0 is the core, chunk 1 the level's records, chunk `i + 2` is room `i`. The guest
    // embeds none of it.
    for (chunk_id, bytes) in world_pack_chunks(package) {
        std::fs::write(
            stream_chunks_dir.join(format!("room_{chunk_id:03}.psxc")),
            bytes,
        )?;
    }
    // The same bytes as two plain files, for the tools that read a cooked level
    // without a disc (the cook hashes, the toolchain probe, the oracle).
    let world = &package.world_geometry;
    std::fs::write(generated_dir.join("grid_level.pgl"), &world.core)?;
    std::fs::write(generated_dir.join("grid_rooms.pgr"), &world.rooms)?;

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
        render_world_pack_order(package),
    )?;
    std::fs::write(
        generated_dir.join(UI_PACK_ORDER_FILENAME),
        render_ui_pack_order(package),
    )?;
    let xa_started = std::time::Instant::now();
    std::fs::write(
        generated_dir.join(XA_MUSIC_FILENAME),
        write_xa_music(package, &xa_music_dir)?,
    )?;
    crate::playtest::emit_cook_output(format_args!(
        "[cook] timing: write {} ms (XA music {} ms)",
        write_started.elapsed().as_millis(),
        xa_started.elapsed().as_millis(),
    ));
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

    out.push_str("pub const PLAYTEST_HAS_WORLD: bool = true;\n\n");

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

    out.push_str(
        "/// Absolute disc LBA where WORLD.PAK starts in the playtest ISO layout: chunk 0\n\
         /// is the grid level core, chunk 1 the level's records, chunk `i + 2` is room `i`.\n",
    );
    let _ = writeln!(
        out,
        "pub const WORLD_PACK_START_LBA: u32 = {};",
        psx_iso::WORLD_PACK_DEFAULT_START_LBA
    );
    out.push('\n');
    // The room pool: how many rooms the level has, and how many 2 KiB pages
    // the guest reserves to hold the ones around the player (`plan_pool`).
    let pool = &package.world_geometry.pool;
    let _ = writeln!(
        out,
        "/// Rooms in the level; room `i` is WORLD.PAK chunk `i + 2`.\n\
         pub const GRID_ROOM_COUNT: usize = {};\n\
         /// Pages (2 KiB) of the room pool. The level wants at most {} pages ({} rooms) at\n\
         /// once, of {} pages in all; the rest of the pool is room for fragmentation.\n\
         pub const GRID_ROOM_POOL_PAGES: usize = {};\n\
         /// Rooms the residency table tracks at once: the most any wanted set holds ({}), with\n\
         /// room for the ones still resident from the last position and one backing off.\n\
         /// Every lookup in the table walks it, so it is no larger than that needs.\n\
         pub const GRID_ROOM_ENTRIES: usize = {};\n",
        package.world_geometry.room_chunks.len(),
        pool.peak_pages,
        pool.peak_rooms,
        pool.total_pages,
        pool.pool_pages.max(1),
        pool.max_rooms,
        (pool.max_rooms + pool.max_rooms / 2 + 8)
            .min(package.world_geometry.room_chunks.len())
            .max(1),
    );

    // Baked chunk skies: where each room's sits in WORLD.PAK.
    let (sky_sectors, sky_counts) = chunk_sky_sectors(package);
    let sky = &package.world_geometry.sky_tables;
    let _ = writeln!(
        out,
        "/// Per room, the sector of its baked sparse cube sky relative to the start of\n\
         /// WORLD.PAK (0 for none), and the sectors it occupies.\n\
         pub const GRID_ROOM_SKY_SECTORS: [u32; GRID_ROOM_COUNT] = {sky_sectors:?};\n\
         pub const GRID_ROOM_SKY_COUNT: [u32; GRID_ROOM_COUNT] = {sky_counts:?};\n\
         /// The index of every texel of the sky no ray hit (`psx_level::sky_tiles::Background`):\n\
         /// where each row of each face starts in the runs, and the runs. Empty: the\n\
         /// world streams no chunk skies.\n\
         pub const GRID_SKY_BG_ROWS: &[u16] = &{:?};\n\
         pub const GRID_SKY_BG_RUNS: &[u8] = &{:?};\n\
         /// The tiles of the sky VRAM starts with (room 0's) that differ from the background.\n\
         pub const GRID_SKY_INITIAL_MASK: &[u32] = &{:?};\n",
        sky.background_rows, sky.background_runs, sky.initial_mask,
    );

    // Far cards: where each room stands, and where its pictures are on the disc.
    let cards = &package.world_geometry.cards;
    let card_sectors = chunk_card_sectors(package);
    let card_copies = card_copy_table(package);
    let _ = writeln!(
        out,
        "/// Far cards (`psx_grid::render::cards`): per room `[x, z, base, height, half width, 0]`\n\
         /// in world runtime units (a zero height: no card), and the sector of its pictures\n\
         /// relative to the start of WORLD.PAK (0 for none). Empty: the world has no cards.\n\
         pub const GRID_CARDS: &[[i16; 6]] = &{:?};\n\
         pub const GRID_ROOM_CARD_SECTORS: &[u32] = &{card_sectors:?};\n\
         /// Sectors from a room's picture of one view to its picture of the next.\n\
         pub const GRID_CARD_VIEW_STRIDE: u32 = {};\n\
         /// Where each tile of rooms stands, and for each pair of tiles (X * tiles + Y) the sector\n\
         /// (low 24 bits) and views (top 8) of the copy of Y's pictures that lies near X; each room's\n\
         /// tile and its place among the tile's rooms. Empty without copies.\n\
         pub const GRID_CARD_TILE_CENTRES: &[[i16; 2]] = &{:?};\n\
         pub const GRID_CARD_COPIES: &[u32] = &{:?};\n\
         pub const GRID_ROOM_CARD_TILE: &[u8] = &{:?};\n\
         pub const GRID_ROOM_CARD_PLACE: &[u8] = &{:?};\n\
         /// Cards show out to this many runtime units from the camera, fading in over the\n\
         /// last `GRID_CARD_FADE`; the colours behind them (below the horizon, at it, at the\n\
         /// top of the screen).\n\
         pub const GRID_CARD_FAR: i32 = {};\n\
         pub const GRID_CARD_FADE: i32 = {};\n\
         /// No card is drawn closer than this (the rooms there are drawn whole).\n\
         pub const GRID_CARD_NEAR: i32 = {};\n\
         pub const GRID_CARD_BACKDROP: [[u8; 3]; 3] = {:?};\n",
        cards.rows,
        cards.stride,
        if cards.blocks.is_empty() { &[][..] } else { &cards.tile_centres[..] },
        card_copies,
        if cards.blocks.is_empty() { &[][..] } else { &cards.room_tile[..] },
        if cards.blocks.is_empty() { &[][..] } else { &cards.room_place[..] },
        cards.far,
        cards.fade,
        cards.near,
        if cards.rows.is_empty() {
            package.world_geometry.panels.backdrop
        } else {
            cards.backdrop
        },
    );

    // Far panels (`psx_grid::render::panels`): the bake eyes, and per eye and heading where the
    // panel starts on the disc and which tiles it stores.
    let panels = &package.world_geometry.panels;
    let panel_blocks = chunk_panel_blocks(package);
    let _ = writeln!(
        out,
        "/// Far panels: the bake eyes (runtime units), and per eye the sector of its block\n\
         /// relative to the start of WORLD.PAK (0 for an eye with no panel). Empty: no panels.\n\
         pub const GRID_PANEL_EYES: &[[i16; 3]] = &{:?};\n\
         pub const GRID_PANEL_BLOCKS: &[u32] = &{panel_blocks:?};\n\
         /// The projection plane the panels were baked for, and the distance between eyes.\n\
         pub const GRID_PANEL_FOCAL: i32 = {};\n\
         pub const GRID_PANEL_SPACING: i32 = {};\n\
         /// Plan distance the rooms are drawn whole to.\n\
         pub const GRID_PANEL_REAL: i32 = {};\n",
        panels.eyes, panels.focal, panels.spacing, panels.real,
    );

    // The shape of the level's own records: how many of each table chunk 1 of
    // WORLD.PAK holds. The records ride the disc; the executable keeps only
    // these counts, to size its arrays and to leave out code for tables the
    // level does not have. The guest refuses a chunk that disagrees.
    out.push_str("/// How many records of each table the level's record chunk holds.\n");
    for (i, name) in psx_level::wire::TABLE_NAMES.iter().enumerate() {
        let _ = writeln!(
            out,
            "pub const LEN_{}: usize = {};",
            name.to_uppercase(),
            package
                .world_geometry
                .record_lens
                .get(i)
                .copied()
                .unwrap_or(0),
        );
    }
    out.push('\n');

    out.push_str(
        "/// Absolute disc LBA where UI.PAK starts in the playtest ISO layout.\n\
         /// UI.PAK is packed immediately after WORLD.PAK by the ISO builder.\n",
    );
    let _ = writeln!(
        out,
        "pub const UI_PACK_START_LBA: u32 = {ui_pack_start_lba};"
    );
    out.push('\n');

    out.push_str(&music_layout(package).render_tracks(music_first_lba(package)));
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

    out
}

fn render_world_pack_order(package: &PlaytestPackage) -> String {
    let mut out = String::from(
        "# PSoXide WORLD.PAK room order\n\
         # One cooked room id per line. Generated by cook-playtest.\n",
    );
    for (chunk_id, _) in world_pack_chunks(package) {
        let _ = writeln!(out, "{chunk_id}");
    }
    out
}

/// Where the music files go on the disc: they follow UI.PAK, the last file of
/// the shared playtest layout (see `psx_iso::add_playtest_files`), so the
/// manifest knows their sectors before the image exists.
fn music_first_lba(package: &PlaytestPackage) -> u32 {
    let ui_chunks = ui_pack_chunks(package);
    let ui_pack_sectors = if ui_chunks.is_empty() {
        0
    } else {
        psx_iso::build_world_pack_layout(&ui_chunks).total_sectors
    };
    psx_iso::WORLD_PACK_DEFAULT_START_LBA
        + world_pack_layout(package).total_sectors
        + ui_pack_sectors
}

fn music_layout(package: &PlaytestPackage) -> super::xa_music::Layout {
    let frames: Vec<usize> = package
        .cdda_tracks
        .iter()
        .map(|track| track.xa_frames as usize)
        .collect();
    super::xa_music::layout(&frames)
}

/// Encode the Music-node songs to XA files in `xa_music_dir` and list them for
/// the disc builders (path, tab, sector).
fn write_xa_music(package: &PlaytestPackage, xa_music_dir: &Path) -> std::io::Result<String> {
    let mut out = String::from(
        "# PSoXide cooked XA music files\n\
         # One per line, in disc order: path, a tab, and the sector it must land on.\n",
    );
    if package.cdda_tracks.is_empty() {
        return Ok(out);
    }
    let wavs = package
        .cdda_tracks
        .iter()
        .map(|track| std::fs::read(&track.wav_path))
        .collect::<std::io::Result<Vec<_>>>()?;
    let sources: Vec<_> = wavs
        .iter()
        .zip(&package.cdda_tracks)
        .map(|(wav, track)| super::xa_music::Source {
            wav,
            playback_speed_q12: track.playback_speed_q12,
        })
        .collect();
    let layout = music_layout(package);
    let names = layout.file_names();
    // Encoding the songs (resample, ADPCM) takes seconds and depends only on
    // the WAV bytes, the baked speeds and the file layout, so a cook that did
    // not touch the music reuses the last result.
    let cache_dir = xa_music_dir.parent().map(|dir| dir.join(XA_CACHE_DIRNAME));
    let key = xa_cache_key(&wavs, package, &layout);
    let cached = cache_dir.as_ref().map(|dir| {
        names
            .iter()
            .map(|name| dir.join(format!("{key:016x}-{name}")))
            .collect::<Vec<_>>()
    });
    let cache_hit = cached
        .as_ref()
        .is_some_and(|paths| paths.iter().all(|path| path.is_file()));
    let files: Vec<Option<Vec<u8>>> = if cache_hit {
        names.iter().map(|_| None).collect()
    } else {
        super::xa_music::encode(&sources, &layout)
            .map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("cook XA music: {e}"),
                )
            })?
            .into_iter()
            .map(Some)
            .collect()
    };
    if let (Some(dir), Some(paths), false) = (cache_dir.as_ref(), cached.as_ref(), cache_hit) {
        // One entry at a time: the files are megabytes each.
        let _ = std::fs::remove_dir_all(dir);
        if std::fs::create_dir_all(dir).is_ok() {
            for (path, bytes) in paths.iter().zip(&files) {
                if let Some(bytes) = bytes {
                    let _ = std::fs::write(path, bytes);
                }
            }
        }
    }
    let mut lba = music_first_lba(package);
    for (index, (name, sectors)) in names.iter().zip(&layout.file_sectors).enumerate() {
        let target = xa_music_dir.join(name);
        match (&files[index], cached.as_ref()) {
            (Some(bytes), _) => std::fs::write(&target, bytes)?,
            (None, Some(paths)) => {
                let _ = std::fs::remove_file(&target);
                if std::fs::hard_link(&paths[index], &target).is_err() {
                    std::fs::copy(&paths[index], &target)?;
                }
            }
            (None, None) => unreachable!("a cache hit needs a cache directory"),
        }
        let listed = target.canonicalize().unwrap_or(target);
        let _ = writeln!(out, "{}\t{lba}", listed.display());
        lba += sectors;
    }
    Ok(out)
}

/// Directory under `generated/` holding the last encoded XA music files.
const XA_CACHE_DIRNAME: &str = ".cook-cache";

/// Bump when `psx-audio-cook` or `xa_music::encode` change what they emit for
/// the same input, so a stale cache cannot outlive the encoder.
const XA_COOK_REVISION: u32 = 1;

/// FNV-1a over everything the encoded files are a function of.
fn xa_cache_key(
    wavs: &[Vec<u8>],
    package: &PlaytestPackage,
    layout: &super::xa_music::Layout,
) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut feed = |bytes: &[u8]| {
        for &byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    };
    feed(&XA_COOK_REVISION.to_le_bytes());
    for (wav, track) in wavs.iter().zip(&package.cdda_tracks) {
        feed(&(wav.len() as u64).to_le_bytes());
        feed(wav);
        feed(&track.playback_speed_q12.to_le_bytes());
    }
    for sectors in &layout.file_sectors {
        feed(&sectors.to_le_bytes());
    }
    hash
}

/// What `WORLD.PAK` holds, in disc order: the grid level core as chunk 0, the
/// level's own records as chunk 1, then room `i` as chunk `i + 2`, then (for a
/// world that bakes them) room `i`'s sky as chunk `rooms + i + 2`, and (for a world
/// with far cards) every card's pictures as the one chunk `2 * rooms + 2`. The three
/// the guest needs first sit in the pack's first header sector. A package without a grid world (the placeholder
/// manifest) has an empty pack, a header-only `WORLD.PAK`, which keeps
/// `UI.PAK`'s start LBA and the loader contract stable.
fn world_pack_chunks(package: &PlaytestPackage) -> Vec<(u32, &[u8])> {
    let world = &package.world_geometry;
    if world.core.is_empty() {
        return Vec::new();
    }
    let rooms = world.room_chunks.len();
    let room_chunk = |room: u32| (room + 2, world.room_chunks[room as usize].as_slice());
    let mut chunks = vec![(0, world.core.as_slice()), (1, world.records.as_slice())];
    if world.cards.interleaved() {
        let tile = world.cards.tile_sectors() as usize * 2048;
        for item in world.cards.pack_order(rooms) {
            chunks.push(match item {
                PackItem::Room(room) => room_chunk(room),
                PackItem::Tile(t) => (
                    2 * rooms as u32 + 2 + t,
                    &world.cards.pictures[t as usize * tile..(t as usize + 1) * tile],
                ),
                PackItem::Block(x) => (
                    2 * rooms as u32 + 2 + world.cards.tiles() + x,
                    world.cards.blocks[x as usize].as_slice(),
                ),
                PackItem::Eye(_) => continue,
            });
        }
        return chunks;
    }
    if !world.panels.is_empty() {
        for item in &world.panels.order {
            match *item {
                PackItem::Room(room) => chunks.push(room_chunk(room)),
                PackItem::Eye(eye) => chunks.push((
                    2 * rooms as u32 + 2 + eye,
                    world.panels.blocks[eye as usize].as_slice(),
                )),
                PackItem::Tile(_) | PackItem::Block(_) => {}
            }
        }
        return chunks;
    }
    chunks.extend((0..rooms as u32).map(room_chunk));
    chunks.extend(
        world
            .chunk_skies
            .iter()
            .enumerate()
            .filter(|(_, bytes)| !bytes.is_empty())
            .map(|(room, bytes)| ((rooms + room) as u32 + 2, bytes.as_slice())),
    );
    chunks
}

/// Per eye, the sector of its block relative to the start of `WORLD.PAK` (0 for none); empty
/// when the world has no panels.
fn chunk_panel_blocks(package: &PlaytestPackage) -> Vec<u32> {
    let world = &package.world_geometry;
    if world.panels.is_empty() {
        return Vec::new();
    }
    let rooms = world.room_chunks.len() as u32;
    let layout = world_pack_layout(package);
    (0..world.panels.eyes.len() as u32)
        .map(|eye| {
            layout
                .entries
                .iter()
                .find(|entry| entry.chunk_id == 2 * rooms + 2 + eye)
                .map_or(0, |entry| entry.sector_offset)
        })
        .collect()
}

/// Per room, the sector of its baked sky relative to the start of
/// `WORLD.PAK` (0 for none), and the sectors it occupies.
fn chunk_sky_sectors(package: &PlaytestPackage) -> (Vec<u32>, Vec<u32>) {
    let world = &package.world_geometry;
    let rooms = world.room_chunks.len();
    let layout = world_pack_layout(package);
    let mut sectors = vec![0u32; rooms];
    let mut counts = vec![0u32; rooms];
    for entry in &layout.entries {
        let id = entry.chunk_id as usize;
        if id >= rooms + 2 && id < 2 * rooms + 2 {
            sectors[id - rooms - 2] = entry.sector_offset;
            counts[id - rooms - 2] = entry.sector_count;
        }
    }
    (sectors, counts)
}

/// Per room, the sector of its card's picture of view 0 relative to the start
/// of `WORLD.PAK` (0 for none); empty when the world has no cards. The picture
/// of view `k` is `GRID_CARD_VIEW_STRIDE * k` sectors on.
fn chunk_card_sectors(package: &PlaytestPackage) -> Vec<u32> {
    let world = &package.world_geometry;
    if !world.cards.interleaved() {
        return Vec::new();
    }
    let rooms = world.room_chunks.len();
    let layout = world_pack_layout(package);
    let tile_sectors = world.cards.tile_sectors();
    world
        .cards
        .offsets
        .iter()
        .map(|&offset| {
            if offset == u32::MAX {
                return 0;
            }
            let tile = offset / tile_sectors;
            layout
                .entries
                .iter()
                .find(|entry| entry.chunk_id == 2 * rooms as u32 + 2 + tile)
                .map_or(0, |entry| entry.sector_offset + offset % tile_sectors)
        })
        .collect()
}

/// Per (X, Y) pair of tiles, where the copy of Y's pictures near X starts, as
/// a sector from the start of `WORLD.PAK` in the low 24 bits and the views it
/// holds in the top 8 (0 for no copy); empty when the world has none.
fn card_copy_table(package: &PlaytestPackage) -> Vec<u32> {
    let world = &package.world_geometry;
    if world.cards.blocks.is_empty() {
        return Vec::new();
    }
    let rooms = world.room_chunks.len() as u32;
    let tiles = world.cards.tiles();
    let layout = world_pack_layout(package);
    let block_start = |x: u32| {
        layout
            .entries
            .iter()
            .find(|entry| entry.chunk_id == 2 * rooms + 2 + tiles + x)
            .map_or(0, |entry| entry.sector_offset)
    };
    world
        .cards
        .copies
        .iter()
        .enumerate()
        .map(|(pair, &copy)| {
            let mask = copy >> 24;
            if mask == 0 {
                0
            } else {
                let x = (pair / tiles as usize) as u32;
                (block_start(x) + (copy & 0x00ff_ffff)) | (mask << 24)
            }
        })
        .collect()
}

fn world_pack_layout(package: &PlaytestPackage) -> psx_iso::WorldPackLayout {
    psx_iso::build_world_pack_layout(&world_pack_chunks(package))
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

const fn material_flags_for_sidedness(sidedness: crate::MaterialFaceSidedness) -> u16 {
    match sidedness {
        crate::MaterialFaceSidedness::Front => 0,
        crate::MaterialFaceSidedness::Back => 1,
        crate::MaterialFaceSidedness::Both => 2,
    }
}

pub(super) fn model_material_flags(material: &PlaytestModelMaterialOverride) -> u16 {
    let flags = material_flags_for_sidedness(material.face_sidedness);
    flags | reflection_material_flags(material.reflection_probe)
}

pub(super) fn reflection_material_flags(reflection: Option<crate::ReflectionProbeMaterial>) -> u16 {
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
