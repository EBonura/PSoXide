//! Editing operations on a grid world.
//!
//! Pure functions over [`GridWorld`](super::GridWorld): no UI types, no
//! undo, no I/O. The editor calls them between `push_undo` and `mark_dirty`,
//! the tests drive them directly, and a future MCP can reuse them as is.
//!
//! * [`refs`]: stable references to faces, corners and cells.
//! * [`heights`]: corner height edits with coincident corners welded.
//! * [`rects`]: floors and ceilings over a rectangle, room auto-grow.
//! * [`walls`]: gap-filling wall placement along edges.
//! * [`selection`]: the selection set and range rules.
//! * [`region`]: the set of sectors the level verbs act on.
//! * [`rough`]: the rock level of sectors and the room's jitter settings.
//! * [`verbs`]: the level verbs (Raise/Lower, Ramp, Stairs).
//! * [`convert`]: converting a legacy room to derived walls.
//! * [`delete`]: deleting faces and sectors.
//! * [`natural`]: cracks, void triangles and direct wall heights.
//! * [`outline`]: polygon and corridor footprints, 45 degree corner cuts.
//! * [`bands`]: horizontal wall bands: sets, runs and the copy-on-write edits.
//! * [`derived`]: material, texture, solidity and visibility of derived walls.
//! * [`surfaces`]: material, texture placement, split and flags.
//! * [`clipboard`]: copy, flip, turn and paste.
//! * [`planes`]: free planes: add, edit, remove, copy and paste.
//! * [`meshes`]: static meshes: assets, instances and the arch between two points.

pub mod bands;
pub mod clipboard;
pub mod convert;
pub mod delete;
pub mod fit;
pub mod derived;
pub mod heights;
pub mod lens;
pub mod meshes;
pub mod natural;
pub mod outline;
pub mod planes;
pub mod rects;
pub mod refs;
pub mod region;
pub mod rough;
pub mod selection;
pub mod surfaces;
pub mod verbs;
pub mod walls;

pub use clipboard::{GridClipboard, PasteOutcome, Transform};
pub use convert::{convert_to_derived, ConvertMode, ConvertReport};
pub use delete::{clear_cells, delete_faces};
pub use fit::{split_to_fit, ClickRect};
pub use derived::{
    clean_overrides, derived_solid, is_derived_hidden, reset_derived, set_derived_hidden,
};
pub use heights::{nudge, HeightDrag, WeldIndex};
pub use meshes::{
    add_asset, arch_span, delete_meshes, place_mesh, remove_asset, set_asset, set_mesh_material,
    solve_arch_span, update_mesh, ArchPlacement, MeshRefusal,
};
pub use planes::{
    add_plane, edit_plane_uv, fit_room, remove_planes, set_plane_material, set_plane_uv,
    update_plane, PlaneClipboard, PlaneId, PlaneRefusal,
};
pub use rects::{
    create_surfaces, create_surfaces_cells, delete_surfaces, delete_surfaces_cells, CellRect, RectOutcome, RectRefusal};
pub use refs::{
    corner_offsets, faces_of_cell, faces_of_leaf, CornerRef, EdgeRef, FaceRef, FaceTarget,
    GridElement, SurfaceKind,
};
pub use region::Region;
pub use selection::{
    all_faces_of_room, cells_in_rect, range_corners, range_surfaces, range_walls, GridSelection,
};
pub use surfaces::{
    edit_uv, material_of, set_material, set_solid, set_wall_facing, toggle_split, uv_of, UvOp,
};
pub use walls::{
    facing_toward, leaf_wall_gap, next_wall_heights, place_wall, place_wall_leaf, place_wall_run, remove_wall, wall_gap,
    wall_run_cells, WallRefusal, WallSpec,
};

#[cfg(test)]
mod leaf_tests;
