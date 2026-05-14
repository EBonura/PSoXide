//! Cooks the editor's starter room into `OUT_DIR/room.psxw` so
//! the example can `include_bytes!` it at compile time.
//!
//! Also pins the minimal material invariant the runtime side bakes
//! in -- slot 0 must exist because the example provides a slot-0
//! fallback material at runtime.

use psxed_project::{
    world_cook::{cook_world_grid, encode_world_grid_psxw},
    NodeKind, ProjectDocument,
};

const ROOM_PSXW: &str = "room.psxw";

fn main() {
    // The starter is baked into psxed-project at compile time
    // (see DEFAULT_PROJECT_RON), so cargo's automatic
    // build-dependency tracking re-runs us when that crate
    // changes. No explicit rerun-if-changed needed beyond Cargo's
    // default.
    let project = ProjectDocument::starter();

    let grid = project
        .active_scene()
        .nodes()
        .iter()
        .find_map(|node| match &node.kind {
            NodeKind::Room { grid } => Some(grid.clone()),
            _ => None,
        })
        .expect("starter project must contain a Room node");

    let cooked = cook_world_grid(&project, &grid).expect("starter grid cooks cleanly");
    assert_slot_ordering(&project, &cooked);

    let bytes = encode_world_grid_psxw(&project, &grid).expect("starter grid encodes cleanly");

    let out_dir = std::env::var_os("OUT_DIR").expect("OUT_DIR set by cargo");
    let out_path = std::path::Path::new(&out_dir).join(ROOM_PSXW);
    std::fs::write(&out_path, bytes).expect("write room.psxw to OUT_DIR");
}

/// The runtime-side example supplies material slot 0. The default
/// starter project is intentionally tiny now, so don't pin the old
/// floor+wall texture set here.
fn assert_slot_ordering(
    _project: &ProjectDocument,
    cooked: &psxed_project::world_cook::CookedWorldGrid,
) {
    let entry = cooked
        .materials
        .first()
        .unwrap_or_else(|| panic!("starter cook must yield at least one material slot"));
    assert_eq!(entry.slot, 0, "first cooked material slot drifted");
    assert!(entry.texture.is_some(), "slot 0 material has no texture");
}
