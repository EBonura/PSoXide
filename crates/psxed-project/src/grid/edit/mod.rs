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
//! * [`convert`]: converting a legacy room to derived walls.
//! * [`delete`]: deleting faces and sectors.
//! * [`derived`]: material, texture, solidity and visibility of derived walls.
//! * [`surfaces`]: material, texture placement, split and flags.
//! * [`clipboard`]: copy, flip, turn and paste.
//! * [`planes`]: free planes: add, edit, remove, copy and paste.

pub mod clipboard;
pub mod convert;
pub mod delete;
pub mod derived;
pub mod heights;
pub mod planes;
pub mod rects;
pub mod refs;
pub mod selection;
pub mod surfaces;
pub mod walls;

pub use clipboard::{GridClipboard, PasteOutcome, Transform};
pub use convert::{convert_to_derived, ConvertMode, ConvertReport};
pub use delete::{clear_cells, delete_faces};
pub use derived::{
    clean_overrides, derived_solid, is_derived_hidden, reset_derived, set_derived_hidden,
};
pub use heights::{nudge, HeightDrag, WeldIndex};
pub use planes::{
    add_plane, edit_plane_uv, fit_room, remove_planes, set_plane_material, set_plane_uv,
    update_plane, PlaneClipboard, PlaneId, PlaneRefusal,
};
pub use rects::{create_surfaces, delete_surfaces, CellRect, RectOutcome, RectRefusal};
pub use refs::{
    corner_offsets, faces_of_cell, CornerRef, EdgeRef, FaceRef, FaceTarget, GridElement,
    SurfaceKind,
};
pub use selection::{
    all_faces_of_room, cells_in_rect, range_corners, range_surfaces, range_walls, GridSelection,
};
pub use surfaces::{
    edit_uv, material_of, set_material, set_solid, set_wall_facing, toggle_split, uv_of, UvOp,
};
pub use walls::{
    facing_toward, next_wall_heights, place_wall, place_wall_run, remove_wall, wall_gap,
    wall_run_cells, WallRefusal, WallSpec,
};
