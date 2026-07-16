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

pub mod floor_view;
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
pub mod tr_level;
mod ui_types;
pub mod world_cook;
pub use ui_types::*;
mod scene_grid_types;
pub use scene_grid_types::*;
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

/// Source-tree projects directory: `editor/projects/`.
///
/// Captured via `env!("CARGO_MANIFEST_DIR")` at compile time, so it
/// resolves wherever cargo built this crate from. Works for the
/// dev workflow (`cargo run -p frontend` from anywhere in the
/// repo); will need a different strategy when the editor ever
/// ships as a standalone binary.
pub fn projects_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("projects")
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
