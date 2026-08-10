//! Editor-side project model for PSoXide.
//!
//! This is the authoring model, not the final runtime layout. It keeps a
//! Godot-style scene tree and resource list so the editor can stay pleasant,
//! then later cooker stages flatten it into PS1-friendly world surfaces,
//! texture pages, entity spawns, and engine data.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use ron::ser::PrettyConfig;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

mod animation_pose_correction;
pub mod brush;
pub mod brush_collision_hulls;
pub mod brush_compile;
pub mod brush_light;
pub mod brush_pack;
pub mod brush_portal;
pub mod brush_pxbsp;
pub mod brush_playtest;
pub mod brush_world;
pub mod floor_view;
pub use animation_pose_correction::*;
mod import_util;
pub mod model_import;
mod model_material_texture;
pub use model_material_texture::*;
pub mod playtest;
pub mod portal_rooms;
pub mod resolve;
pub mod room_connections;
pub mod spatial;
pub mod streaming;
pub mod texture_import;
mod ui_types;
pub mod world_cook;
pub use ui_types::*;
mod prefab;
pub use prefab::*;
mod scene_grid_types;
pub use scene_grid_types::*;
mod box_prop_erosion;
pub use box_prop_erosion::*;
mod cylinder_prop;
pub use cylinder_prop::*;
mod arch_prop;
pub use arch_prop::*;
mod world_types;
pub use world_types::*;
mod resource_types;
pub use resource_types::*;
mod scene_types;
pub use scene_types::*;
mod document_types;
pub use document_types::*;

/// Embedded copy of the default project's RON, baked at compile
/// time so the editor binary always carries a working starter even
/// if `editor/projects/default/` is absent at runtime. Single source
/// of truth -- edits to the on-disk file propagate to `starter()` on
/// the next build.
const DEFAULT_PROJECT_RON: &str = include_str!("../../../projects/default/project.ron");

/// Where user projects live, resolved for both the dev tree and a shipped
/// binary.
///
/// Precedence, deliberately ordered so the developer workflow cannot regress:
///
/// 1. `PSOXIDE_PROJECTS_DIR`, an explicit override for tests and unusual installs.
/// 2. The source tree, when it exists. `env!("CARGO_MANIFEST_DIR")` is baked at
///    compile time, so this hits for `cargo run` from a checkout and keeps the
///    old behaviour byte-for-byte. A shipped binary's build directory does not
///    exist on the user's machine, so this branch simply misses.
/// 3. The per-user data directory, for a downloaded build.
///
/// This is the single choke point: [`default_project_dir`], [`list_projects`]
/// and the delete guard all derive from it, so they follow automatically.
pub fn projects_dir() -> PathBuf {
    if let Some(override_dir) = std::env::var_os("PSOXIDE_PROJECTS_DIR") {
        return PathBuf::from(override_dir);
    }
    let source_tree = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("projects");
    if source_tree.is_dir() {
        return source_tree;
    }
    user_projects_dir()
}

/// Per-user projects directory for a shipped build.
///
/// Matches the location the runtime already uses for playtest tapes, so a
/// downloaded editor and emulator agree on where a user's data lives. Resolved
/// from the environment rather than through a crate dependency, because the
/// paths are stable platform conventions and this crate has no other need for
/// one.
pub fn user_projects_dir() -> PathBuf {
    const QUALIFIED: &str = "com.psoxide.PSoXide";
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join(QUALIFIED)
                .join("projects");
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("PSoXide").join("projects");
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
            return PathBuf::from(data_home).join("psoxide").join("projects");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("psoxide")
                .join("projects");
        }
    }
    // No home directory at all: keep everything beside the executable rather
    // than writing to the working directory, which a launcher may set anywhere.
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("projects")
}

/// Copy the bundled sample project into the per-user projects directory the
/// first time a downloaded build runs.
///
/// Deliberately NOT part of [`projects_dir`]. That function is called from the
/// delete guard, and a path lookup that also writes to disk is the kind of
/// surprise that turns into a real bug. Call this once at startup instead.
///
/// Seeds only when the directory is ABSENT, never merely empty: a user who
/// deletes the sample should not have it reappear on next launch.
///
/// The sample is copied rather than read in place because it is meant to be
/// edited. An archive dragged into a read-only location would otherwise give a
/// project that opens but cannot be saved.
///
/// Failure is not fatal and is reported to the caller: no sample beats
/// refusing to start.
pub fn ensure_projects_seeded() -> std::io::Result<bool> {
    let target = projects_dir();
    if target.exists() {
        return Ok(false);
    }
    // Only a shipped layout has projects beside the executable; a checkout
    // resolves to its source tree above and never reaches here.
    let Some(bundled) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .map(|dir| dir.join("projects"))
        .filter(|dir| dir.is_dir())
    else {
        return Ok(false);
    };
    copy_dir_recursive(&bundled, &target)?;
    Ok(true)
}

fn copy_dir_recursive(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let destination = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&source, &destination)?;
        } else {
            std::fs::copy(&source, &destination)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod projects_dir_tests {
    use super::*;

    /// The override must win outright: it is what lets a test, or a user with
    /// an unusual install, point the editor somewhere specific. The delete
    /// guard compares against this same root, so if the override were ignored
    /// the guard would enforce a directory the editor is not using.
    #[test]
    fn explicit_override_beats_every_other_source() {
        let previous = std::env::var_os("PSOXIDE_PROJECTS_DIR");
        // SAFETY: single-threaded test; restored before returning.
        unsafe { std::env::set_var("PSOXIDE_PROJECTS_DIR", "/tmp/psoxide-override") };
        assert_eq!(projects_dir(), PathBuf::from("/tmp/psoxide-override"));
        unsafe {
            match previous {
                Some(value) => std::env::set_var("PSOXIDE_PROJECTS_DIR", value),
                None => std::env::remove_var("PSOXIDE_PROJECTS_DIR"),
            }
        }
    }

    /// Without an override, a checkout keeps its source tree, so `cargo run`
    /// behaviour is unchanged by the shipped-build resolution added around it.
    #[test]
    fn a_checkout_still_resolves_to_the_source_tree() {
        let previous = std::env::var_os("PSOXIDE_PROJECTS_DIR");
        unsafe { std::env::remove_var("PSOXIDE_PROJECTS_DIR") };
        let resolved = projects_dir();
        assert!(
            resolved.is_dir(),
            "source tree should exist when running from a checkout"
        );
        assert!(resolved.ends_with("projects"));
        unsafe {
            if let Some(value) = previous {
                std::env::set_var("PSOXIDE_PROJECTS_DIR", value);
            }
        }
    }

    /// Seeding must respect a deletion. Re-creating the sample every launch
    /// because the directory is empty would be infuriating, so absence of the
    /// directory itself is the trigger.
    #[test]
    fn seeding_is_skipped_when_the_directory_already_exists() {
        let previous = std::env::var_os("PSOXIDE_PROJECTS_DIR");
        let existing = std::env::temp_dir().join("psoxide-seed-existing");
        std::fs::create_dir_all(&existing).expect("temp dir");
        unsafe { std::env::set_var("PSOXIDE_PROJECTS_DIR", &existing) };
        assert!(!ensure_projects_seeded().expect("seed"));
        unsafe {
            match previous {
                Some(value) => std::env::set_var("PSOXIDE_PROJECTS_DIR", value),
                None => std::env::remove_var("PSOXIDE_PROJECTS_DIR"),
            }
        }
        let _ = std::fs::remove_dir_all(&existing);
    }

    /// A shipped build has no source tree, so it must land in per-user data
    /// rather than the working directory, which a launcher may set anywhere.
    #[test]
    fn user_directory_is_absolute_and_project_scoped() {
        let dir = user_projects_dir();
        assert!(dir.ends_with("projects"));
        if std::env::var_os("HOME").is_some() {
            assert!(dir.is_absolute(), "user data dir must not be relative");
        }
    }
}

/// Filesystem-safe stem derived from a project display name.
///
/// The editor keeps `ProjectDocument::name` as the user-facing source
/// of truth, then uses this helper for generated directories and EXE
/// filenames. It intentionally stays ASCII-only because the PSX build
/// output and launcher paths benefit from boring, portable names.
pub fn project_file_stem(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "project".to_string()
    } else {
        trimmed
    }
}

/// Default project directory (`editor/projects/default/`). Always
/// present in the source tree; user "New Project" copies its
/// contents into a sibling directory.
pub fn default_project_dir() -> PathBuf {
    projects_dir().join("default")
}

/// Enumerate every directory under [`projects_dir`] that contains a
/// `project.ron`. Cheap directory walk, used by the editor's open /
/// switch flow once that lands. Returns an empty Vec rather than
/// erroring when `projects_dir` doesn't exist -- fresh checkout
/// before the dev runs the editor once.
pub fn list_projects() -> std::io::Result<Vec<PathBuf>> {
    let root = projects_dir();
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() && path.join("project.ron").is_file() {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

pub use world_cook::{
    cook_world_grid, encode_world_grid_psxw, CookedGridHorizontalFace, CookedGridSector,
    CookedGridVerticalFace, CookedGridWalls, CookedWorldGrid, CookedWorldMaterial,
    WorldGridCookError, WorldGridFaceKind,
};

/// Errors raised while reading or writing editor project documents.
#[derive(Debug)]
pub enum ProjectIoError {
    /// Filesystem error.
    Io(std::io::Error),
    /// RON parse error.
    Parse(ron::error::SpannedError),
    /// RON serialization error.
    Serialize(ron::Error),
}

impl std::fmt::Display for ProjectIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "filesystem error: {error}"),
            Self::Parse(error) => write!(f, "project parse error: {error}"),
            Self::Serialize(error) => write!(f, "project serialization error: {error}"),
        }
    }
}

impl std::error::Error for ProjectIoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Parse(error) => Some(error),
            Self::Serialize(error) => Some(error),
        }
    }
}

impl From<std::io::Error> for ProjectIoError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ron::error::SpannedError> for ProjectIoError {
    fn from(error: ron::error::SpannedError) -> Self {
        Self::Parse(error)
    }
}

impl From<ron::Error> for ProjectIoError {
    fn from(error: ron::Error) -> Self {
        Self::Serialize(error)
    }
}

/// Stable identifier for a node inside one scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(u64);

impl NodeId {
    /// The root node id every scene starts with.
    pub const ROOT: Self = Self(1);

    /// Return the raw integer value for compact UI/debug display.
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Stable identifier for a project resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResourceId(u64);

impl ResourceId {
    /// Return the raw integer value for compact UI/debug display.
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Stable identifier for a project [`OptionDef`]. Sliders bind to an
/// option by id; the cook carries the low 16 bits into the runtime
/// record so an option survives renames and reorders.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct OptionId(pub u32);

impl OptionId {
    /// Return the raw integer value for compact UI/debug display.
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// Stable identifier for a node inside one authored UI scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct UiNodeId(u64);

impl UiNodeId {
    /// The root UI node id every UI scene starts with.
    pub const ROOT: Self = Self(1);

    /// Return the raw integer value for compact UI/debug display.
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Stable identifier for an authored UI scene. Assigned once when
/// the scene is created and preserved across renames so the cook
/// and the runtime game-state flow can address a scene by id rather
/// than by list position. `UNASSIGNED` is the load-time sentinel
/// for legacy `project.ron` files that predate the field; such
/// scenes are given a fresh id during normalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct UiSceneId(pub u64);

impl UiSceneId {
    /// Sentinel meaning "no id assigned yet". Normalization replaces
    /// it with the next free id.
    pub const UNASSIGNED: Self = Self(0);

    /// First id handed out to a freshly authored scene.
    pub const FIRST: Self = Self(1);

    /// Return the raw integer value for compact UI/debug display.
    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl Default for UiSceneId {
    fn default() -> Self {
        Self::UNASSIGNED
    }
}

/// Stable identifier for an authored composed screen state. A screen
/// state is the arranger-level object: it can run a world layer, draw a
/// UI scene on top, and define who owns input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SceneStateId(pub u64);

impl SceneStateId {
    /// Load-time sentinel for legacy or hand-authored states without ids.
    pub const UNASSIGNED: Self = Self(0);

    /// First id handed out to an authored screen state.
    pub const FIRST: Self = Self(1);

    /// Return the raw integer value for compact UI/debug display.
    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl Default for SceneStateId {
    fn default() -> Self {
        Self::UNASSIGNED
    }
}

#[cfg(test)]
mod tests;
