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
//! * [`slope`] and [`paint_rules`]: slope classes read as the motor reads them,
//!   and the smart paint rules the level verbs apply.
//! * [`sector`]: one sector and its six wall edges.
//! * [`room`]: one room, its sector storage and its growth operations.
//! * [`world`]: the document root and the [`SceneWorld`] discriminator.
//! * [`limits`]: caps, validation and the budget readout.
//! * [`plane`]: free planes placed off the lattice, and [`plane_faces`]
//!   their drawable faces.
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
pub mod derive;
pub mod edit;
pub mod faces;
mod faces_bands;
mod faces_derived;
pub mod light;
pub mod limits;
pub mod overrides;
pub mod paint_rules;
pub mod palette;
pub mod plane;
pub mod plane_faces;
pub mod room;
pub mod sector;
pub mod slope;
pub mod starter;
pub mod surface;
pub mod units;
pub mod uv;
pub mod uv_split;
pub mod world;

#[cfg(test)]
mod band_tests;
#[cfg(test)]
mod course_merge;
#[cfg(test)]
mod derive_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod uv_golden;

pub use bands::{Anchor, Band, BandSet, BandV};
pub use derive::{DerivedKind, DerivedWall};
pub use faces::{GridFace, GridFaceKind, UvDefaults};
pub use light::FacingShade;
pub use limits::{GridBudget, GridIssue, GridIssueKind, Severity};
pub use overrides::{Axis, EdgeKey, EdgeOverride, EdgeOverrides, RoomDefaults, WallMode};
pub use paint_rules::PaintRules;
pub use plane::{GridPlane, PlaneCollision, PlaneShape, PlaneUvMode};
pub use room::{GridFog, GridRoom};
pub use sector::{GridSector, WallEdge};
pub use slope::SlopeClass;
pub use surface::{GridSplit, GridSurface, GridTri, GridWall, WallFacing};
pub use units::{GridScale, CLICK_UNITS, FINE_UNITS, SECTOR_UNITS, TEXELS_PER_SECTOR};
pub use world::{GridWorld, SceneWorld};
