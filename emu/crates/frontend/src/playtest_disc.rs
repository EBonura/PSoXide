//! Editor-playtest disc authoring: wraps the compiled `editor-playtest.exe`
//! and the cooked world/UI packs into a local CUE/BIN disc image, and exports
//! per-project baked discs for the launcher. Moved verbatim out of `app.rs`;
//! only compiled with the `editor` feature (the `mod` declaration is gated).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use psx_iso::{build_world_pack, Exe, IsoBuilder, SECTOR_BYTES};

use crate::{paths_equivalent, repo_root_dir};

/// File the editor-playtest build's stdout+stderr is captured to, so a failed
/// Play surfaces the real compiler error instead of just an exit code.
#[cfg(feature = "editor")]
pub(crate) fn editor_playtest_build_log_path() -> PathBuf {
    repo_root_dir()
        .join("logs")
        .join("editor-playtest-build.log")
}

/// Build a one-line failure detail from the captured build log: the first
/// actionable compiler error line, plus where to read the full log. Falls back
/// to just the log path when the log can't be read or has no obvious error.
#[cfg(feature = "editor")]
pub(crate) fn build_log_failure_detail(log_path: &std::path::Path) -> String {
    let where_full = format!("Full log: {}", log_path.display());
    let Ok(text) = std::fs::read_to_string(log_path) else {
        return where_full;
    };
    // Prefer the first `error[Ennnn]:` / `error:` line -- that's the rustc
    // diagnostic a user can act on. Otherwise show the last non-empty line.
    let first_error = text
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("error[") || line.starts_with("error:"));
    let summary =
        first_error.or_else(|| text.lines().map(str::trim).rfind(|line| !line.is_empty()));
    match summary {
        Some(line) => format!("{}. {where_full}", truncate_for_status(line, 200)),
        None => where_full,
    }
}

/// Clamp a status string to `max` chars on a char boundary, appending an
/// ellipsis when truncated, so a long compiler line cannot blow out the toast.
#[cfg(feature = "editor")]
fn truncate_for_status(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(feature = "editor")]
fn editor_playtest_exe_path() -> PathBuf {
    repo_root_dir()
        .join("build")
        .join("examples")
        .join("mipsel-sony-psx")
        .join("release")
        .join("editor-playtest.exe")
}

#[cfg(feature = "editor")]
fn editor_playtest_disc_path() -> PathBuf {
    repo_root_dir()
        .join("build")
        .join("examples")
        .join("mipsel-sony-psx")
        .join("release")
        .join("editor-playtest.cue")
}

/// Wrap the current editor-playtest executable and generated world pack into
/// the local embedded playtest disc image.
#[cfg(feature = "editor")]
pub(crate) const DEFAULT_EMBEDDED_PLAYTEST_VOLUME_ID: &str = "PSOXIDE";
#[cfg(feature = "editor")]
const ISO_VOLUME_ID_BYTES: usize = 32;

#[cfg(feature = "editor")]
pub(crate) fn build_embedded_playtest_disc(volume_id: &str) -> Result<PathBuf, String> {
    let exe_path = editor_playtest_exe_path();
    let exe_bytes = std::fs::read(&exe_path).map_err(|e| format!("{}: {e}", exe_path.display()))?;
    let world_pack = embedded_world_pack_payload()?;
    let ui_pack = embedded_ui_pack_payload()?;
    let mut image = embedded_playtest_disc_image(volume_id, exe_bytes, world_pack, ui_pack)?;
    let cdda_payloads = embedded_cdda_track_payloads()?;
    let cdda_tracks = append_cdda_tracks_to_image(&mut image, &cdda_payloads)?;

    let cue_path = editor_playtest_disc_path();
    let bin_path = cue_path.with_extension("bin");
    let dir = cue_path
        .parent()
        .ok_or_else(|| format!("invalid playtest disc path: {}", cue_path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    std::fs::write(&bin_path, image).map_err(|e| format!("write {}: {e}", bin_path.display()))?;
    write_cue(&cue_path, &bin_path, &cdda_tracks)?;
    Ok(cue_path)
}

#[cfg(feature = "editor")]
fn write_single_data_track_cue(cue_path: &Path, bin_path: &Path) -> Result<(), String> {
    write_cue(cue_path, bin_path, &[])
}

#[cfg(feature = "editor")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CddaCueTrack {
    number: u8,
    index0_sector: u32,
    index1_sector: u32,
}

#[cfg(feature = "editor")]
fn write_cue(cue_path: &Path, bin_path: &Path, cdda_tracks: &[CddaCueTrack]) -> Result<(), String> {
    let file_name = bin_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("invalid BIN path for CUE: {}", bin_path.display()))?
        .replace('"', "'");
    let mut cue =
        format!("FILE \"{file_name}\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n");
    for track in cdda_tracks {
        cue.push_str(&format!(
            "  TRACK {:02} AUDIO\n    INDEX 00 {}\n    INDEX 01 {}\n",
            track.number,
            cue_msf(track.index0_sector),
            cue_msf(track.index1_sector)
        ));
    }
    std::fs::write(cue_path, cue).map_err(|e| format!("write {}: {e}", cue_path.display()))
}

#[cfg(feature = "editor")]
fn cue_msf(frames: u32) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        frames / (60 * 75),
        (frames / 75) % 60,
        frames % 75
    )
}

#[cfg(feature = "editor")]
fn append_cdda_tracks_to_image(
    image: &mut Vec<u8>,
    payloads: &[PathBuf],
) -> Result<Vec<CddaCueTrack>, String> {
    const CDDA_PREGAP_SECTORS: usize = 150;
    let mut cue_tracks = Vec::with_capacity(payloads.len());
    for (index, payload) in payloads.iter().enumerate() {
        if index >= 98 {
            return Err("CUE sheets can only address tracks 01 through 99".to_string());
        }
        let cdda =
            std::fs::read(payload).map_err(|e| format!("read {}: {e}", payload.display()))?;
        if cdda.is_empty() || cdda.len() % SECTOR_BYTES != 0 {
            return Err(format!(
                "{} is not a non-empty whole number of 2352-byte CD-DA sectors",
                payload.display()
            ));
        }
        let index0_sector = (image.len() / SECTOR_BYTES) as u32;
        image.resize(image.len() + CDDA_PREGAP_SECTORS * SECTOR_BYTES, 0);
        let index1_sector = (image.len() / SECTOR_BYTES) as u32;
        image.extend_from_slice(&cdda);
        cue_tracks.push(CddaCueTrack {
            number: (index + 2) as u8,
            index0_sector,
            index1_sector,
        });
    }
    Ok(cue_tracks)
}

#[cfg(feature = "editor")]
fn embedded_playtest_disc_image(
    volume_id: &str,
    exe_bytes: Vec<u8>,
    world_pack: Option<Vec<u8>>,
    ui_pack: Option<Vec<u8>>,
) -> Result<Vec<u8>, String> {
    Exe::parse(&exe_bytes).map_err(|e| format!("parse EXE: {e:?}"))?;
    let mut builder = IsoBuilder::new().volume_id(volume_id);
    if let Some(system_area) = embedded_playtest_system_area()? {
        builder = builder
            .system_area(system_area)
            .map_err(|_| "PSOXIDE_SYSTEM_AREA did not decode to exactly 16 cooked sectors")?;
    }
    // No system area is the normal, permanent configuration: this project
    // ships no Sony data. Unlicensed discs boot in emulators and on
    // unlocked/ODE consoles, which is the supported target set.
    // Canonical playtest-disc layout, shared with the mkisopsx CLI via
    // psx_iso::add_playtest_files so the on-disc file order (and therefore the
    // cooked WORLD_PACK_START_LBA / UI_PACK_START_LBA) cannot drift between the
    // two builders. Normal project discs omit CDTEST.BIN so the root directory
    // stays close to a retail/homebrew boot layout.
    psx_iso::add_playtest_files(&mut builder, exe_bytes, world_pack, ui_pack, None)
        .map_err(|error| format!("playtest disc layout: {error:?}"))?;
    Ok(builder.build_bin())
}

#[cfg(feature = "editor")]
fn embedded_playtest_system_area() -> Result<Option<Vec<u8>>, String> {
    let Some(path) = std::env::var_os("PSOXIDE_SYSTEM_AREA") else {
        return Ok(None);
    };
    let path = PathBuf::from(path);
    let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    extract_playstation_system_area(&bytes).map(Some).map_err(|e| {
        format!(
            "{}: {e}. Supply either the first 16 cooked 2048-byte sectors or a raw BIN/CUE-style image.",
            path.display()
        )
    })
}

#[cfg(feature = "editor")]
fn extract_playstation_system_area(bytes: &[u8]) -> Result<Vec<u8>, String> {
    const COOKED_BYTES: usize = 16 * psx_iso::iso9660::SECTOR_SIZE;
    const RAW_BYTES: usize = 16 * psx_iso::iso9660::RAW_SECTOR_SIZE;

    if bytes.len() >= RAW_BYTES && looks_like_raw_sector(&bytes[..SECTOR_BYTES]) {
        let mut out = Vec::with_capacity(COOKED_BYTES);
        for sector in bytes[..RAW_BYTES].chunks_exact(SECTOR_BYTES) {
            out.extend_from_slice(&sector[24..24 + psx_iso::iso9660::SECTOR_SIZE]);
        }
        return Ok(out);
    }

    if bytes.len() >= COOKED_BYTES {
        return Ok(bytes[..COOKED_BYTES].to_vec());
    }

    Err(format!(
        "system area needs at least {COOKED_BYTES} cooked bytes or {RAW_BYTES} raw bytes"
    ))
}

#[cfg(feature = "editor")]
fn looks_like_raw_sector(sector: &[u8]) -> bool {
    sector.len() >= SECTOR_BYTES
        && sector[0] == 0x00
        && sector[11] == 0x00
        && sector[1..11] == [0xFF; 10]
}

#[cfg(feature = "editor")]
fn editor_playtest_generated_dir() -> PathBuf {
    repo_root_dir()
        .join("engine")
        .join("examples")
        .join("editor-playtest")
        .join("generated")
}

#[cfg(feature = "editor")]
fn embedded_cdda_track_payloads() -> Result<Vec<PathBuf>, String> {
    let tracks_file =
        editor_playtest_generated_dir().join(psxed_project::playtest::CDDA_TRACKS_FILENAME);
    if !tracks_file.is_file() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&tracks_file)
        .map_err(|e| format!("read {}: {e}", tracks_file.display()))?;
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        out.push(PathBuf::from(trimmed));
    }
    Ok(out)
}

#[cfg(feature = "editor")]
fn embedded_world_pack_payload() -> Result<Option<Vec<u8>>, String> {
    let generated_dir = editor_playtest_generated_dir();
    let chunks_dir = generated_dir.join(psxed_project::playtest::STREAM_CHUNKS_DIRNAME);
    if !chunks_dir.is_dir() {
        return Ok(None);
    }
    let mut rooms = Vec::new();
    for entry in
        std::fs::read_dir(&chunks_dir).map_err(|e| format!("read {}: {e}", chunks_dir.display()))?
    {
        let entry = entry.map_err(|e| format!("read {}: {e}", chunks_dir.display()))?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("psxc") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let Some(raw_index) = stem.strip_prefix("room_") else {
            continue;
        };
        let chunk_id = raw_index
            .parse::<u32>()
            .map_err(|_| format!("invalid room chunk filename: {}", path.display()))?;
        let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        rooms.push((chunk_id, bytes));
    }
    if rooms.is_empty() {
        return Ok(None);
    }
    rooms.sort_by_key(|(chunk_id, _)| *chunk_id);
    let order_file = generated_dir.join(psxed_project::playtest::WORLD_PACK_ORDER_FILENAME);
    if order_file.is_file() {
        let order = read_embedded_world_pack_order(&order_file)?;
        apply_embedded_world_pack_order(&mut rooms, &order, &order_file)?;
    }
    let refs: Vec<(u32, &[u8])> = rooms
        .iter()
        .map(|(chunk_id, bytes)| (*chunk_id, bytes.as_slice()))
        .collect();
    Ok(Some(build_world_pack(&refs)))
}

/// Build the embedded `UI.PAK` from the generated `ui_stream_chunks/` directory,
/// the same way [`embedded_world_pack_payload`] builds `WORLD.PAK`. Each
/// `ui_{index}.psxt` chunk is keyed by its cooked asset index; `ui_pack_order.txt`
/// fixes the on-disc order so the offsets match the cooked `UI_PACK_TOC`. Returns
/// `None` when the project cooked no streamed UI assets.
#[cfg(feature = "editor")]
fn embedded_ui_pack_payload() -> Result<Option<Vec<u8>>, String> {
    let generated_dir = editor_playtest_generated_dir();
    let chunks_dir = generated_dir.join(psxed_project::playtest::UI_STREAM_CHUNKS_DIRNAME);
    if !chunks_dir.is_dir() {
        return Ok(None);
    }
    let mut chunks = Vec::new();
    for entry in
        std::fs::read_dir(&chunks_dir).map_err(|e| format!("read {}: {e}", chunks_dir.display()))?
    {
        let entry = entry.map_err(|e| format!("read {}: {e}", chunks_dir.display()))?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("psxt") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let Some(raw_index) = stem.strip_prefix("ui_") else {
            continue;
        };
        let chunk_id = raw_index
            .parse::<u32>()
            .map_err(|_| format!("invalid ui chunk filename: {}", path.display()))?;
        let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        chunks.push((chunk_id, bytes));
    }
    if chunks.is_empty() {
        return Ok(None);
    }
    chunks.sort_by_key(|(chunk_id, _)| *chunk_id);
    let order_file = generated_dir.join(psxed_project::playtest::UI_PACK_ORDER_FILENAME);
    if order_file.is_file() {
        let order = read_embedded_world_pack_order(&order_file)?;
        apply_embedded_world_pack_order(&mut chunks, &order, &order_file)?;
    }
    let refs: Vec<(u32, &[u8])> = chunks
        .iter()
        .map(|(chunk_id, bytes)| (*chunk_id, bytes.as_slice()))
        .collect();
    Ok(Some(build_world_pack(&refs)))
}

#[cfg(feature = "editor")]
fn read_embedded_world_pack_order(path: &Path) -> Result<Vec<u32>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut order = Vec::new();
    let mut seen = BTreeSet::new();
    for (line_index, line) in text.lines().enumerate() {
        let trimmed = line.split('#').next().unwrap_or("").trim();
        if trimmed.is_empty() {
            continue;
        }
        let room = trimmed
            .parse::<u32>()
            .map_err(|_| format!("{}:{} invalid room id", path.display(), line_index + 1))?;
        if !seen.insert(room) {
            return Err(format!(
                "{}:{} duplicate room id {room}",
                path.display(),
                line_index + 1
            ));
        }
        order.push(room);
    }
    Ok(order)
}

#[cfg(feature = "editor")]
fn apply_embedded_world_pack_order(
    rooms: &mut Vec<(u32, Vec<u8>)>,
    order: &[u32],
    order_file: &Path,
) -> Result<(), String> {
    if order.is_empty() {
        return Err(format!(
            "{}: world pack order is empty",
            order_file.display()
        ));
    }
    let mut ordered = Vec::with_capacity(rooms.len());
    for &chunk_id in order {
        let Some(index) = rooms.iter().position(|(room, _)| *room == chunk_id) else {
            return Err(format!(
                "{}: room id {chunk_id} has no matching room_{chunk_id:03}.psxw",
                order_file.display()
            ));
        };
        ordered.push(rooms.remove(index));
    }
    if !rooms.is_empty() {
        let missing = rooms
            .iter()
            .map(|(chunk_id, _)| chunk_id.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "{}: order file missing room ids {missing}",
            order_file.display()
        ));
    }
    *rooms = ordered;
    Ok(())
}

#[cfg(feature = "editor")]
pub(crate) fn project_baked_disc_path(project_dir: &Path, project_name: &str) -> PathBuf {
    project_dir
        .join("baked")
        .join(format!("{}.cue", safe_project_build_stem(project_name)))
}

#[cfg(feature = "editor")]
fn safe_project_build_stem(name: &str) -> String {
    psxed_project::project_file_stem(name)
}

#[cfg(feature = "editor")]
pub(crate) fn project_disc_volume_id(project_name: &str) -> String {
    let mut volume_id = safe_project_build_stem(project_name).to_ascii_uppercase();
    if volume_id.len() > ISO_VOLUME_ID_BYTES {
        volume_id.truncate(ISO_VOLUME_ID_BYTES);
    }
    volume_id
}

#[cfg(feature = "editor")]
fn remove_stale_project_builds(dest_path: &Path) -> Result<usize, String> {
    let dest_dir = dest_path
        .parent()
        .ok_or_else(|| format!("invalid build output path: {}", dest_path.display()))?;
    let dest_name = dest_path
        .file_name()
        .ok_or_else(|| format!("invalid build output path: {}", dest_path.display()))?;
    let dest_stem = dest_path
        .file_stem()
        .ok_or_else(|| format!("invalid build output path: {}", dest_path.display()))?;
    if !dest_dir.exists() {
        return Ok(0);
    }

    let mut removed = 0;
    for entry in std::fs::read_dir(dest_dir)
        .map_err(|error| format!("read {}: {error}", dest_dir.display()))?
    {
        let entry = entry.map_err(|error| format!("read {}: {error}", dest_dir.display()))?;
        let path = entry.path();
        let is_build_artifact = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("bin")
                    || extension.eq_ignore_ascii_case("cue")
                    || extension.eq_ignore_ascii_case("exe")
                    || extension.eq_ignore_ascii_case("iso")
            });
        let is_current_build = path.file_name().is_some_and(|name| name == dest_name)
            || path.file_stem().is_some_and(|stem| stem == dest_stem);
        if !is_build_artifact || is_current_build {
            continue;
        }
        std::fs::remove_file(&path)
            .map_err(|error| format!("remove {}: {error}", path.display()))?;
        removed += 1;
    }
    Ok(removed)
}

#[cfg(feature = "editor")]
pub(crate) fn copy_project_disc(
    source_cue_path: &Path,
    dest_cue_path: &Path,
) -> Result<u64, String> {
    let source_bin_path = source_cue_path.with_extension("bin");
    let dest_bin_path = dest_cue_path.with_extension("bin");
    let dest_dir = dest_cue_path
        .parent()
        .ok_or_else(|| format!("invalid build output path: {}", dest_cue_path.display()))?;
    std::fs::create_dir_all(dest_dir)
        .map_err(|error| format!("mkdir {}: {error}", dest_dir.display()))?;
    remove_stale_project_builds(dest_cue_path)?;
    let bin_bytes = std::fs::copy(&source_bin_path, &dest_bin_path).map_err(|error| {
        format!(
            "copy {} to {}: {error}",
            source_bin_path.display(),
            dest_bin_path.display()
        )
    })?;
    rewrite_cue_for_copied_bin(source_cue_path, dest_cue_path, &dest_bin_path)?;
    let cue_bytes = std::fs::metadata(dest_cue_path)
        .map_err(|error| format!("stat {}: {error}", dest_cue_path.display()))?
        .len();
    Ok(bin_bytes + cue_bytes)
}

#[cfg(feature = "editor")]
fn rewrite_cue_for_copied_bin(
    source_cue_path: &Path,
    dest_cue_path: &Path,
    dest_bin_path: &Path,
) -> Result<(), String> {
    let source = std::fs::read_to_string(source_cue_path)
        .map_err(|error| format!("read {}: {error}", source_cue_path.display()))?;
    let dest_name = dest_bin_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("invalid BIN path for CUE: {}", dest_bin_path.display()))?
        .replace('"', "'");
    let mut replaced_file = false;
    let mut out = String::with_capacity(source.len() + dest_name.len());
    for line in source.lines() {
        if !replaced_file && line.trim_start().to_ascii_uppercase().starts_with("FILE ") {
            out.push_str(&format!("FILE \"{dest_name}\" BINARY\n"));
            replaced_file = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !replaced_file {
        return write_single_data_track_cue(dest_cue_path, dest_bin_path);
    }
    std::fs::write(dest_cue_path, out)
        .map_err(|error| format!("write {}: {error}", dest_cue_path.display()))
}

#[cfg(feature = "editor")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectBuildMenuMetadata {
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) current: bool,
}

#[cfg(feature = "editor")]
pub(crate) fn project_build_menu_metadata(
    path: &Path,
    project_root: &Path,
) -> Option<ProjectBuildMenuMetadata> {
    let project_dir = project_dir_for_build(path, project_root)?;
    let project =
        psxed_project::ProjectDocument::load_from_path(project_dir.join("project.ron")).ok()?;
    let expected_stem = safe_project_build_stem(&project.name);
    let actual_stem = path.file_stem()?.to_str()?;
    let subtitle = path
        .strip_prefix(project_root)
        .ok()
        .and_then(|relative| relative.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| path.display().to_string());
    Some(ProjectBuildMenuMetadata {
        title: project.name,
        subtitle,
        current: actual_stem == expected_stem,
    })
}

#[cfg(feature = "editor")]
fn project_dir_for_build(path: &Path, project_root: &Path) -> Option<PathBuf> {
    let mut dir = path.parent()?;
    loop {
        if dir.join("project.ron").is_file() {
            return Some(dir.to_path_buf());
        }
        if paths_equivalent(dir, project_root) {
            return None;
        }
        dir = dir.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use psx_iso::Disc;

    use super::*;

    #[test]
    #[cfg(feature = "editor")]
    fn project_build_disc_name_is_filesystem_safe() {
        assert_eq!(
            safe_project_build_stem("Stone Room: Vertical Slice!"),
            "stone_room_vertical_slice"
        );
        assert_eq!(safe_project_build_stem("..."), "project");
        assert_eq!(
            project_baked_disc_path(Path::new("editor/projects/default"), "Demo Project"),
            Path::new("editor/projects/default")
                .join("baked")
                .join("demo_project.cue")
        );
    }

    #[test]
    #[cfg(feature = "editor")]
    fn project_disc_volume_id_uses_project_name() {
        assert_eq!(project_disc_volume_id("Demo 10"), "DEMO_10");
        assert_eq!(project_disc_volume_id("..."), "PROJECT");
        assert_eq!(
            project_disc_volume_id("A very very very very very long project name"),
            "A_VERY_VERY_VERY_VERY_VERY_LONG_"
        );
    }

    #[test]
    #[cfg(feature = "editor")]
    fn embedded_playtest_disc_image_boots_psx_exe() {
        let mut exe = vec![0u8; psx_iso::EXE_HEADER_BYTES];
        exe[..8].copy_from_slice(b"PS-X EXE");
        exe[0x10..0x14].copy_from_slice(&0x8001_2340u32.to_le_bytes());
        exe[0x18..0x1C].copy_from_slice(&0x8001_0000u32.to_le_bytes());
        exe[0x1C..0x20].copy_from_slice(&4u32.to_le_bytes());
        exe.extend_from_slice(&[1, 2, 3, 4]);

        let world_pack = psx_iso::build_world_pack(&[(0, b"room-zero".as_slice())]);
        let image = embedded_playtest_disc_image("DEMO_10", exe, Some(world_pack), None)
            .expect("disc image builds");
        let disc = Disc::from_bin(image);
        let boot = psx_iso::load_boot_exe_from_disc(&disc).expect("disc boots");
        let pvd_sector = disc.read_sector_user(16).expect("PVD sector exists");
        let boot_sector = disc
            .read_sector_user(psx_iso::PLAYTEST_BOOT_EXE_START_LBA)
            .expect("boot exe sector exists");
        let world_pack_sector = disc
            .read_sector_user(psx_iso::WORLD_PACK_DEFAULT_START_LBA)
            .expect("world pack sector exists");

        assert_eq!(boot.boot_path, "PSX.EXE;1");
        assert_eq!(boot.exe.initial_pc, 0x8001_2340);
        assert_eq!(boot.exe.payload, vec![1, 2, 3, 4]);
        assert_eq!(&pvd_sector[40..47], b"DEMO_10");
        assert_eq!(&boot_sector[..8], b"PS-X EXE");
        assert_eq!(
            &world_pack_sector[..psx_iso::WORLD_PACK_MAGIC.len()],
            &psx_iso::WORLD_PACK_MAGIC
        );
    }

    #[test]
    #[cfg(feature = "editor")]
    fn project_build_export_removes_stale_sibling_builds() {
        let root = frontend_test_temp_dir("stale-project-build-exes");
        let baked = root.join("baked");
        std::fs::create_dir_all(&baked).unwrap();
        let stale = baked.join("untitled_ps1_project.exe");
        let stale_bin = baked.join("old_demo.bin");
        let stale_cue = baked.join("old_demo.cue");
        let current = baked.join("demo2.cue");
        let current_bin = baked.join("demo2.bin");
        let notes = baked.join("notes.txt");
        std::fs::write(&stale, b"old").unwrap();
        std::fs::write(&stale_bin, b"old bin").unwrap();
        std::fs::write(&stale_cue, b"old cue").unwrap();
        std::fs::write(&current, b"new").unwrap();
        std::fs::write(&current_bin, b"new bin").unwrap();
        std::fs::write(&notes, b"keep").unwrap();

        assert_eq!(remove_stale_project_builds(&current).unwrap(), 3);
        assert!(!stale.exists());
        assert!(!stale_bin.exists());
        assert!(!stale_cue.exists());
        assert!(current.exists());
        assert!(current_bin.exists());
        assert!(notes.exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(feature = "editor")]
    fn project_build_export_rewrites_cue_for_renamed_bin() {
        let root = frontend_test_temp_dir("project-build-export-cue");
        let source_dir = root.join("source");
        std::fs::create_dir_all(&source_dir).unwrap();
        let source_cue = source_dir.join("editor-playtest.cue");
        let source_bin = source_dir.join("editor-playtest.bin");
        std::fs::write(&source_bin, b"disc image bytes").unwrap();
        write_single_data_track_cue(&source_cue, &source_bin).unwrap();

        let dest_cue = root
            .join("cortex_ignition_v1")
            .join("baked")
            .join("cortex_ignition_v1.cue");
        let copied_bytes = copy_project_disc(&source_cue, &dest_cue).unwrap();
        let dest_bin = dest_cue.with_extension("bin");
        let cue = std::fs::read_to_string(&dest_cue).unwrap();

        assert!(copied_bytes > 0);
        assert_eq!(std::fs::read(&dest_bin).unwrap(), b"disc image bytes");
        assert!(cue.contains("FILE \"cortex_ignition_v1.bin\" BINARY"));
        assert!(!cue.contains("editor-playtest.bin"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(feature = "editor")]
    fn project_build_menu_metadata_uses_project_name_and_marks_stale_builds() {
        let root = frontend_test_temp_dir("project-build-menu-metadata");
        let project_dir = root.join("demo2");
        let baked = project_dir.join("baked");
        std::fs::create_dir_all(&baked).unwrap();
        psxed_project::ProjectDocument::new("Demo Two")
            .save_to_path(project_dir.join("project.ron"))
            .unwrap();

        let current = baked.join("demo_two.cue");
        let stale = baked.join("untitled_ps1_project.cue");
        std::fs::write(&current, b"current").unwrap();
        std::fs::write(&stale, b"stale").unwrap();

        let current_metadata = project_build_menu_metadata(&current, &root).unwrap();
        assert_eq!(current_metadata.title, "Demo Two");
        assert!(current_metadata.subtitle.contains("demo2"));
        assert!(current_metadata.current);

        let stale_metadata = project_build_menu_metadata(&stale, &root).unwrap();
        assert_eq!(stale_metadata.title, "Demo Two");
        assert!(!stale_metadata.current);

        let _ = std::fs::remove_dir_all(root);
    }

    fn frontend_test_temp_dir(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "psoxide-frontend-{name}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
