//! Load a project, cook it for Play and parse the PGL blobs.

use std::path::PathBuf;

use psx_grid::write::{aligned_copy, words_as_bytes};
use psx_grid::{LevelView, RoomView};
use psxed_project::grid::GridWorld;
use psxed_project::playtest::build_package;
use psxed_project::ProjectDocument;

/// One cooked level with its document.
pub struct Loaded {
    pub name: String,
    pub world: GridWorld,
    core: Vec<u32>,
    core_len: usize,
    chunks: Vec<(Vec<u32>, usize)>,
}

impl Loaded {
    /// `name` is a directory under `editor/projects`, or a path to a
    /// project directory.
    pub fn load(name: &str) -> Result<Self, String> {
        let direct = PathBuf::from(name);
        let dir = if direct.join("project.ron").is_file() {
            direct
        } else {
            psxed_project::projects_dir().join(name)
        };
        let mut project = ProjectDocument::load_from_path(dir.join("project.ron"))
            .map_err(|e| format!("load: {e:?}"))?;
        if std::env::var_os("GRID_COMPACT_DERIVE").is_some() {
            if let Some(world) = project.active_scene_mut().world.grid_mut() {
                for room in 0..world.rooms.len() {
                    psxed_project::grid::edit::convert_to_derived(
                        world,
                        room,
                        psxed_project::grid::edit::ConvertMode::PreserveLook,
                    );
                }
            }
        }
        let world = project
            .active_scene()
            .world
            .grid()
            .ok_or("not a grid project")?
            .clone();
        let (core_blob, rooms_blob) = cooked_blobs(name, &project, &dir)?;
        let geometry = Blobs {
            core: core_blob,
            rooms: rooms_blob,
        };
        let core = aligned_copy(&geometry.core);
        let level = LevelView::parse(words_as_bytes(&core, geometry.core.len()))
            .map_err(|e| format!("core: {e:?}"))?;
        let mut chunks = Vec::new();
        for record in level.rooms {
            let start = record.pack_sector as usize * psx_grid::SECTOR_BYTES;
            let end = start + usize::from(record.pack_sectors) * psx_grid::SECTOR_BYTES;
            let bytes = &geometry.rooms[start..end.min(geometry.rooms.len())];
            // The header says the unpadded length.
            let words = aligned_copy(bytes);
            let probe = RoomView::parse(words_as_bytes(&words, bytes.len()))
                .map_err(|e| format!("room: {e:?}"))?;
            let len = probe.col.header.byte_len as usize;
            chunks.push((words, len));
        }
        let core_len = geometry.core.len();
        Ok(Self {
            name: name.to_string(),
            world,
            core,
            core_len,
            chunks,
        })
    }

    pub fn level(&self) -> LevelView<'_> {
        LevelView::parse(words_as_bytes(&self.core, self.core_len)).expect("parsed at load")
    }

    pub fn room(&self, index: usize) -> RoomView<'_> {
        let (words, len) = &self.chunks[index];
        RoomView::parse(words_as_bytes(words, *len)).expect("parsed at load")
    }

    pub fn room_bytes(&self, index: usize) -> usize {
        self.chunks[index].1
    }

    pub fn rooms(&self) -> usize {
        self.chunks.len()
    }

    pub fn core_bytes(&self) -> usize {
        self.core_len
    }
}

struct Blobs {
    core: Vec<u8>,
    rooms: Vec<u8>,
}

/// The cooked core and room blobs, cooked once per project and kept in
/// `$GRID_COMPACT_CACHE` (default `/tmp/grid-compact-cache`) because the
/// full Play cook also cooks every animation and takes about a minute.
/// `GRID_COMPACT_REFRESH=1` recooks.
fn cooked_blobs(
    name: &str,
    project: &ProjectDocument,
    dir: &std::path::Path,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let cache = PathBuf::from(
        std::env::var("GRID_COMPACT_CACHE").unwrap_or_else(|_| "/tmp/grid-compact-cache".into()),
    );
    let tag = std::env::var("PSXED_GRID_LOD_FAR").unwrap_or_default();
    let derive = if std::env::var_os("GRID_COMPACT_DERIVE").is_some() {
        "-derived"
    } else {
        ""
    };
    let stem = format!(
        "{}{}{derive}",
        name.replace('/', "_"),
        if tag.is_empty() {
            String::new()
        } else {
            format!("-lod{tag}")
        }
    );
    let core_path = cache.join(format!("{stem}.core"));
    let rooms_path = cache.join(format!("{stem}.rooms"));
    if std::env::var_os("GRID_COMPACT_REFRESH").is_none() {
        if let (Ok(core), Ok(rooms)) = (std::fs::read(&core_path), std::fs::read(&rooms_path)) {
            return Ok((core, rooms));
        }
    }
    let (package, report) = build_package(project, dir);
    let package = package.ok_or_else(|| format!("cook failed: {:?}", report.error_messages()))?;
    let geometry = package.world_geometry;
    let _ = std::fs::create_dir_all(&cache);
    let _ = std::fs::write(&core_path, &geometry.core);
    let _ = std::fs::write(&rooms_path, &geometry.rooms);
    Ok((geometry.core, geometry.rooms))
}

impl Loaded {
    /// The unpadded bytes of chunk `index`.
    pub fn room_raw(&self, index: usize) -> Vec<u8> {
        let (words, len) = &self.chunks[index];
        words_as_bytes(words, *len).to_vec()
    }
}
