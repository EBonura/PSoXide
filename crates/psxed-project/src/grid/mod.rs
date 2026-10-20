//! The sector-grid world document.
//!
//! A grid world is a Tomb Raider style set of rooms, each a rectangle of
//! square sectors with optional floor and ceiling surfaces (four corner
//! heights and a diagonal split) and short stacks of wall segments along each
//! sector edge. It is the authoring model for the grid editor, ported from
//! the bonnie-32 level editor's data model (see `docs/grid/plan.md`).
//!
//! Decisions that shape this module:
//!
//! * The document stores integers only. A sector is [`SECTOR_UNITS`] grid
//!   units, a height click is [`CLICK_UNITS`] and the fine snap is
//!   [`FINE_UNITS`]. Each world carries a rational [`GridScale`] that maps
//!   grid units to editor units, so the same document can drive a character
//!   of any size without a float in the data.
//! * Every wall belongs to exactly one sector edge ([`WallEdge`]). Two
//!   walls on the two sides of one physical edge are two separate surfaces
//!   with an explicit visible side, never one shared record.
//! * Every cap lives in [`limits`], checked by [`limits::validate`] and
//!   read by the editor budget readout, the cook and the runtime alike.
//! * The world lives in [`SceneWorld`] on the scene, defaulting to empty so
//!   a project with no world loads exactly as before.
//!
//! Layout of the module:
//!
//! * [`units`]: constants and the grid-to-editor scale.
//! * [`surface`]: floor and ceiling surfaces, walls, materials and UVs.
//! * [`auto_split`]: choosing a surface's diagonal from its heights.
//! * [`walk`] and [`heat`]: the walkability analysis and the performance
//!   heatmap model behind the overlays.
//! * [`slope`] and [`paint_rules`]: slope classes read as the motor reads them,
//!   and the smart paint rules the level verbs apply.
//! * [`leaf`] and [`leaf_map`]: the quadtree of leaves a room's cells are.
//! * [`sector`]: one leaf, its column of spans and its six wall edges.
//! * [`room`]: one room, its leaves and its growth operations.
//! * [`world`]: the document root and the [`SceneWorld`] discriminator.
//! * [`limits`]: caps, validation and the budget readout.
//! * [`plane`]: free planes placed off the lattice, and [`plane_faces`]
//!   their drawable faces.
//! * [`mesh`]: static meshes (assets, generators, placed instances), and
//!   [`mesh_faces`] their drawable faces.
//! * [`faces`]: the drawable faces derived from the document.
//! * [`light`]: the per-corner light bake of those faces.
//! * [`edit`]: the editing operations the grid tools call.
//! * [`base_project`]: the base project a new grid project starts from.
//! * [`starter`]: the starter room of File > New Grid Project.
//! * [`palette`]: which project materials a grid world's palette offers.

pub mod auto_split;
pub mod band_geom;
pub mod bands;
pub mod base_project;
pub mod cook;
pub mod density;
pub mod derive;
mod derive_diag;
mod derive_spans;
pub mod edit;
pub mod faces;
mod faces_bands;
mod faces_derived;
pub mod graybox_set;
pub mod heat;
pub mod leaf;
pub mod leaf_derive;
pub mod leaf_diag;
mod leaf_faces;
pub mod leaf_map;
pub mod light;
pub mod lighting;
pub mod limits;
pub mod mesh;
mod mesh_arch;
mod mesh_column;
pub mod mesh_faces;
mod mesh_obj;
pub mod overrides;
pub mod paint_rules;
pub mod palette;
pub mod plane;
pub mod plane_faces;
pub mod room;
pub mod sector;
pub mod sector_view;
pub mod slope;
pub mod starter;
pub mod storeys;
pub mod surface;
pub mod tree;
pub mod units;
pub mod uv;
pub mod uv_region;
pub mod uv_split;
pub mod walk;
pub mod world;

#[cfg(test)]
mod band_tests;
#[cfg(test)]
mod course_merge;
#[cfg(test)]
mod derive_diag_tests;
#[cfg(test)]
mod derive_span_tests;
#[cfg(test)]
mod derive_tests;
#[cfg(test)]
mod exterior_tests;
#[cfg(test)]
mod focus_tests;
#[cfg(test)]
mod leaf_face_tests;
#[cfg(test)]
mod leaf_tests;
#[cfg(test)]
mod span_face_tests;
#[cfg(test)]
mod spiral_tests;
#[cfg(test)]
mod storey_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod texel_density_tests;
#[cfg(test)]
mod uv_golden;

pub use bands::{Anchor, Band, BandSet, BandV};
pub use derive::{DerivedKind, DerivedWall};
pub use faces::{GridFace, GridFaceKind, UvDefaults};
pub use leaf::LeafKey;
pub use leaf_map::LeafMap;
pub use light::FacingShade;
pub use lighting::GlobalLight;
pub use limits::{GridBudget, GridIssue, GridIssueKind, Severity};
pub use mesh::{
    ArchProfile, ArchSpec, ColumnSpec, GridMesh, ImportedMesh, MeshAsset, MeshKind, RubbleSpec,
};
pub use overrides::{
    Axis, ClosureFacing, EdgeKey, EdgeOverride, EdgeOverrides, RoomDefaults, WallMode,
};
pub use paint_rules::PaintRules;
pub use plane::{GridPlane, PlaneCollision, PlaneShape, PlaneUvMode};
pub use room::{GridFog, GridRoom};
pub use sector::{GridSector, GridSpan, WallEdge};
pub use slope::SlopeClass;
pub use surface::{GridSplit, GridSurface, GridTri, GridWall, WallFacing};
pub use units::{GridScale, SectorTexels, CLICK_UNITS, FINE_UNITS, SECTOR_UNITS};
pub use world::{GridWorld, SceneWorld, StreamRadii};
