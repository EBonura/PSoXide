//! Quadtree operations on a room's leaves: subdivide, merge, balance, hanging
//! corners and the checks that say the tree is sound.
//!
//! The document stores leaves of five sizes ([`LeafKey`](super::leaf::LeafKey),
//! [`LeafMap`](super::leaf_map::LeafMap)); this module is what keeps a room of
//! them a valid tree (`docs/grid/level-tools-design.md` 3.2 and 3.5):
//!
//! * [`ops`]: subdivide a leaf into four, merge four back. Both exact.
//! * [`normalize`]: balance (neighbours differ by at most one level), big
//!   leaves planar, hanging corners constrained to the average of their
//!   edge, and [`check`](normalize::check).
//! * [`sector_split`] and [`surface_split`]: how a leaf's surfaces and walls
//!   divide among four children and join again.
//! * [`edge_overrides`]: the room's per-segment overrides follow the leaves.
//! * [`neighbours`]: the leaves across a side of a leaf.

pub mod edge_overrides;
pub mod neighbours;
pub mod normalize;
pub mod ops;
pub mod sector_split;
pub mod surface_split;

pub use normalize::{
    balance, big_leaf_ok, check, constrain, hanging_mismatches, normalize, HangingCorner,
    NormalizeReport, TreeIssue,
};
pub use ops::{
    can_merge, merge, merge_all, subdivide, subdivide_all_to, subdivide_to, MergeBlock,
    SubdivideBlock,
};
pub use sector_split::{join_sector, split_sector, JoinBlock};

#[cfg(test)]
mod tests;
