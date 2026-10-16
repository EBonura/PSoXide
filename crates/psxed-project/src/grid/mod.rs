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
//! * The world lives in [`SceneWorld`] on the scene, defaulting to brushes so
//!   every existing project loads exactly as before.
//!
//! Layout of the module:
//!
//! * [`units`]: constants and the grid-to-editor scale.
//! * [`surface`]: floor and ceiling surfaces, walls, materials and UVs.
//! * [`sector`]: one sector and its six wall edges.
//! * [`room`]: one room, its sector storage and its growth operations.
//! * [`world`]: the document root and the [`SceneWorld`] discriminator.
//! * [`limits`]: caps, validation and the budget readout.
//! * [`faces`]: the drawable faces derived from the document.
//! * [`light`]: the per-corner light bake of those faces.
//! * [`bonnie`] and [`bonnie_project`]: the bonnie-32 level importer.

pub mod bonnie;
pub mod bonnie_project;
pub mod faces;
pub mod light;
pub mod limits;
pub mod room;
pub mod sector;
pub mod surface;
pub mod units;
pub mod world;

#[cfg(test)]
mod bonnie_tests;
#[cfg(test)]
mod import_tests;
#[cfg(test)]
mod tests;

pub use faces::{GridFace, GridFaceKind};
pub use limits::{GridBudget, GridIssue, GridIssueKind, Severity};
pub use room::{GridFog, GridRoom};
pub use sector::{GridSector, WallEdge};
pub use surface::{GridSplit, GridSurface, GridTri, GridWall, WallFacing};
pub use units::{GridScale, CLICK_UNITS, FINE_UNITS, SECTOR_UNITS, TEXELS_PER_SECTOR};
pub use world::{GridWorld, SceneWorld};
