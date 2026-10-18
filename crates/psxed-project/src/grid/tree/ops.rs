//! Subdividing a leaf and merging four back.
//!
//! Both are exact: a subdivision changes how the room is stored, never what
//! it looks like or stands on, and a merge happens only when the parent
//! reproduces the four children bit for bit. Both leave the tree balanced
//! and the hanging corners constrained ([`super::normalize`]).

use super::super::leaf::{LeafKey, FINEST_LEVEL, SECTOR_LEVEL};
use super::super::room::GridRoom;
use super::super::sector::GridSector;
use super::edge_overrides;
use super::neighbours::{across, SIDES};
use super::normalize::{big_leaf_ok, normalize};
use super::sector_split::{join_sector, split_sector, JoinBlock};

/// Why a leaf could not be subdivided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubdivideBlock {
    /// There is no such leaf.
    NoLeaf,
    /// The leaf is already the finest size, a quarter of a sector.
    Finest,
}

impl SubdivideBlock {
    /// A short phrase for the status bar.
    pub const fn describe(self) -> &'static str {
        match self {
            Self::NoLeaf => "there is no cell there",
            Self::Finest => "the cell is already a quarter sector",
        }
    }
}

/// Why four leaves could not be merged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeBlock {
    /// One of the four cells is missing, or has been subdivided further.
    NotLeaves,
    /// The four cells are not what one cell would give.
    Cells(JoinBlock),
    /// An override sits on an edge between two of the cells, or the two
    /// halves of an outer edge's override differ.
    Overrides,
    /// A neighbour is two levels finer than the merged cell would be.
    Unbalanced,
    /// A cell above a sector must be one plane with no walls on it.
    BigLeaf,
}

impl MergeBlock {
    /// A short phrase for the status bar.
    pub fn describe(self) -> &'static str {
        match self {
            Self::NotLeaves => "the four cells are not all whole cells",
            Self::Cells(b) => b.describe(),
            Self::Overrides => "wall settings differ along the edges",
            Self::Unbalanced => "a finer neighbour needs these cells",
            Self::BigLeaf => "a cell above a sector must be one plane with no walls",
        }
    }
}

/// Subdivide `key` into its four children without restoring the tree
/// invariants. Used by the balance pass, which restores them in a loop.
pub(super) fn subdivide_raw(
    room: &mut GridRoom,
    key: LeafKey,
) -> Result<[LeafKey; 4], SubdivideBlock> {
    let sector = room.leaf(key).ok_or(SubdivideBlock::NoLeaf)?;
    if key.level >= FINEST_LEVEL {
        return Err(SubdivideBlock::Finest);
    }
    let children = split_sector(sector);
    let keys = key.children().ok_or(SubdivideBlock::Finest)?;
    room.remove_leaf(key);
    for (child_key, child) in keys.into_iter().zip(children) {
        room.insert_leaf(child_key, child)
            .expect("the children of a leaf lie inside the leaf's place");
    }
    edge_overrides::split(room, key);
    Ok(keys)
}

/// Subdivide the leaf `key` into four children that look and stand exactly
/// as it did, then balance the tree (a neighbour more than one level
/// coarser is subdivided too) and constrain the hanging corners.
pub fn subdivide(room: &mut GridRoom, key: LeafKey) -> Result<[LeafKey; 4], SubdivideBlock> {
    let keys = subdivide_raw(room, key)?;
    normalize(room);
    Ok(keys)
}

/// Subdivide whatever leaf covers `click` until a leaf of at least `level`
/// holds it (a finer leaf stays). Returns the leaf at the end.
pub fn subdivide_to(
    room: &mut GridRoom,
    click: [i32; 2],
    level: u8,
) -> Result<LeafKey, SubdivideBlock> {
    loop {
        let (key, _) = room.leaf_at(click).ok_or(SubdivideBlock::NoLeaf)?;
        if key.level >= level {
            return Ok(key);
        }
        subdivide(room, key)?;
    }
}

/// Subdivide every leaf of `keys` that is coarser than `level` down to it.
/// Returns how many leaves were subdivided.
pub fn subdivide_all_to(room: &mut GridRoom, keys: &[LeafKey], level: u8) -> usize {
    let mut count = 0;
    let mut work: Vec<LeafKey> = keys.to_vec();
    while let Some(key) = work.pop() {
        if key.level >= level || !room.contains_leaf(key) || room.leaf(key).is_none() {
            continue;
        }
        if let Ok(children) = subdivide_raw(room, key) {
            count += 1;
            work.extend(children);
        }
    }
    normalize(room);
    count
}

/// Check that the four children of `parent` can become one leaf, without
/// changing anything.
pub fn can_merge(room: &GridRoom, parent: LeafKey) -> Result<(), MergeBlock> {
    merged(room, parent, true).map(|_| ())
}

fn merged(room: &GridRoom, parent: LeafKey, balanced: bool) -> Result<GridSector, MergeBlock> {
    let keys = parent.children().ok_or(MergeBlock::NotLeaves)?;
    let cells: Vec<&GridSector> = keys.iter().filter_map(|k| room.leaf(*k)).collect();
    if cells.len() != 4 {
        return Err(MergeBlock::NotLeaves);
    }
    let sector =
        join_sector([cells[0], cells[1], cells[2], cells[3]]).map_err(MergeBlock::Cells)?;
    if parent.level < SECTOR_LEVEL && !big_leaf_ok(&sector) {
        return Err(MergeBlock::BigLeaf);
    }
    if !edge_overrides::can_join(room, parent) {
        return Err(MergeBlock::Overrides);
    }
    // The merged leaf must not leave a neighbour two levels finer than it.
    for side in SIDES.into_iter().filter(|_| balanced) {
        if across(&room.leaves, parent, side)
            .iter()
            .any(|n| n.level > parent.level + 1)
        {
            return Err(MergeBlock::Unbalanced);
        }
    }
    Ok(sector)
}

fn merge_raw(room: &mut GridRoom, parent: LeafKey, balanced: bool) -> Result<(), MergeBlock> {
    let sector = merged(room, parent, balanced)?;
    for key in parent.children().into_iter().flatten() {
        room.remove_leaf(key);
    }
    room.insert_leaf(parent, sector)
        .expect("the parent lies where its children did");
    edge_overrides::join(room, parent);
    Ok(())
}

/// Merge the four children of `parent` into `parent`. Exact or refused.
pub fn merge(room: &mut GridRoom, parent: LeafKey) -> Result<(), MergeBlock> {
    merge_raw(room, parent, true)?;
    normalize(room);
    Ok(())
}

/// Merge every group of four leaves over the ground of `within` (or
/// anywhere, for `None`) that the parent reproduces exactly, finest first and repeatedly,
/// never above `top_level` (0 allows merging up to four sectors). Returns
/// how many merges happened.
pub fn merge_all(room: &mut GridRoom, within: Option<&[LeafKey]>, top_level: u8) -> usize {
    merge_all_with(room, within, top_level, true)
}

/// [`merge_all`] without the balance rule and without restoring it: every
/// group that is exact joins. For reading a room at coarser granularity (the
/// cook's sector view), never for a document the editor keeps.
pub fn merge_all_unbalanced(room: &mut GridRoom, top_level: u8) -> usize {
    merge_all_with(room, None, top_level, false)
}

fn merge_all_with(
    room: &mut GridRoom,
    within: Option<&[LeafKey]>,
    top_level: u8,
    balanced: bool,
) -> usize {
    let mut total = 0;
    loop {
        let mut parents: Vec<LeafKey> = room
            .leaves()
            .filter(|(k, _)| k.level > top_level)
            .filter(|(k, _)| within.is_none_or(|set| set.iter().any(|w| overlaps(*w, *k))))
            .filter_map(|(k, _)| k.parent())
            .collect();
        parents.sort_by_key(|p| (core::cmp::Reverse(p.level), *p));
        parents.dedup();
        let mut merged_any = false;
        for parent in parents {
            if merge_raw(room, parent, balanced).is_ok() {
                merged_any = true;
                total += 1;
            }
        }
        if !merged_any {
            if balanced {
                normalize(room);
            }
            return total;
        }
    }
}

/// Whether two cells share ground: one lies inside the other (or they are
/// the same). Aligned cells never cross.
fn overlaps(a: LeafKey, b: LeafKey) -> bool {
    a.contains_click(b.min_click()) || b.contains_click(a.min_click())
}
