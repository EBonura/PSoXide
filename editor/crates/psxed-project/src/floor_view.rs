//! Single source of truth for editor floor resolution.
//!
//! A room can have stacked floors (`WorldGrid::floors_above`). Every
//! placed node belongs to exactly one floor (`SceneNode::floor`). The
//! editor shows one "active floor", Sims-style: the active floor is the
//! working plane drawn at Y=0, floors *below* render descending for
//! context, floors *above* are hidden.
//!
//! Both the render pass (frontend `editor_preview`) and the
//! selection/pick/bounds pass (`psxed-ui`) must agree on (a) which floor
//! a node is on and (b) what Y offset that floor draws at. Historically
//! each call site re-derived this independently, so a new render or pick
//! path could silently disagree (entity drawn on the wrong floor,
//! selection landing on the floor below). This module is the one place
//! that resolution lives; every consumer goes through it.

use crate::{NodeId, NodeKind, Scene, WorldGrid};

/// One floor of the active room to render/interact with, in draw order
/// (lowest floor first, active floor last).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedFloor {
    /// Floor index within the room (0 = base grid).
    pub floor_index: usize,
    /// Engine-unit Y offset this floor's geometry/entities draw at. The
    /// active floor is the working plane at 0; floors below are negative.
    pub y_offset: i32,
    /// True for the floor currently being authored (the edit target).
    pub active: bool,
}

/// Which floor a node belongs to, walking ancestors up to the enclosing
/// room and taking the max `floor` seen. A placed entity carries its
/// floor on `SceneNode::floor`; child components (ModelRenderer, etc.)
/// inherit it. Nodes outside any room, or with no floor set, resolve to
/// floor 0 (ground).
pub fn node_floor(scene: &Scene, node_id: NodeId) -> usize {
    let mut current = Some(node_id);
    let mut floor = 0usize;
    while let Some(id) = current {
        let Some(node) = scene.node(id) else { break };
        floor = floor.max(node.floor);
        if matches!(node.kind, NodeKind::Section { .. }) {
            break;
        }
        current = node.parent;
    }
    floor
}

/// The Sims-style floor set for the active room: the active floor plus
/// every floor below it, each with its draw Y offset relative to the
/// active floor (which sits at 0). Floors above the active one are
/// omitted. `active_floor` is clamped to the room's floor count.
///
/// Returns lowest-first so callers can draw bottom-up. Empty if `room`
/// is not a Room node.
pub fn active_room_floors(scene: &Scene, room: NodeId, active_floor: usize) -> Vec<ResolvedFloor> {
    let Some(base) = scene.node(room).and_then(|node| match &node.kind {
        NodeKind::Section { grid } => Some(grid),
        _ => None,
    }) else {
        return Vec::new();
    };
    let active = active_floor.min(base.floor_count().saturating_sub(1));
    let active_elev = base.floor(active).map(|g| g.elevation).unwrap_or(0);
    let mut out = Vec::with_capacity(active + 1);
    for floor_index in 0..=active {
        if let Some(grid) = base.floor(floor_index) {
            out.push(ResolvedFloor {
                floor_index,
                y_offset: grid.elevation - active_elev,
                active: floor_index == active,
            });
        }
    }
    out
}

/// The Y offset a single node draws at in the active room's Sims view,
/// or `None` if the node's floor is hidden (above the active floor) or
/// the room has no such floor. This is the selection/bounds counterpart
/// to [`active_room_floors`]: a node is interactable only when its floor
/// is visible, and its handles must sit at the same Y the renderer used.
pub fn node_draw_offset(
    scene: &Scene,
    room: NodeId,
    active_floor: usize,
    node_id: NodeId,
) -> Option<i32> {
    let node_floor = node_floor(scene, node_id);
    active_room_floors(scene, room, active_floor)
        .into_iter()
        .find(|f| f.floor_index == node_floor)
        .map(|f| f.y_offset)
}

/// Y offset for a node's grid (the room base grid) used to convert
/// between an authored grid and the active room view. Convenience for
/// callers that already know the node's floor index.
pub fn floor_offset(grid: &WorldGrid, floor_index: usize, active_floor: usize) -> i32 {
    let active = active_floor.min(grid.floor_count().saturating_sub(1));
    let active_elev = grid.floor(active).map(|g| g.elevation).unwrap_or(0);
    grid.floor(floor_index)
        .map(|g| g.elevation - active_elev)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProjectDocument;

    fn three_floor_room() -> (ProjectDocument, NodeId) {
        let mut project = ProjectDocument::new("floor-view-test");
        let scene = project.active_scene_mut();
        let mut grid = WorldGrid::empty(1, 1, 1024);
        grid.push_floor(); // floor 1
        grid.push_floor(); // floor 2
        let room = scene.add_node(scene.root, "Stacked", NodeKind::Section { grid });
        (project, room)
    }

    #[test]
    fn active_floor_and_below_only_active_at_zero() {
        let (project, room) = three_floor_room();
        let floors = active_room_floors(project.active_scene(), room, 1);
        let idx: Vec<_> = floors.iter().map(|f| f.floor_index).collect();
        assert_eq!(idx, vec![0, 1], "active(1) + below(0), above hidden");
        let active = floors.iter().find(|f| f.active).unwrap();
        assert_eq!(active.floor_index, 1);
        assert_eq!(active.y_offset, 0, "active floor is the working plane at 0");
        let below = floors.iter().find(|f| f.floor_index == 0).unwrap();
        assert!(below.y_offset < 0, "floor below descends");
    }

    #[test]
    fn node_floor_inherits_through_children() {
        let (mut project, room) = three_floor_room();
        let scene = project.active_scene_mut();
        let entity = scene.add_node(room, "Enemy", NodeKind::Entity);
        scene.node_mut(entity).unwrap().floor = 2;
        let child = scene.add_node(entity, "Model Renderer", NodeKind::Node);
        // Child has floor 0 itself but inherits the entity's floor 2.
        assert_eq!(node_floor(project.active_scene(), child), 2);
        assert_eq!(node_floor(project.active_scene(), entity), 2);
    }

    #[test]
    fn node_on_hidden_floor_has_no_draw_offset() {
        let (mut project, room) = three_floor_room();
        let scene = project.active_scene_mut();
        let upper = scene.add_node(room, "Upper", NodeKind::Entity);
        scene.node_mut(upper).unwrap().floor = 2;
        let lower = scene.add_node(room, "Lower", NodeKind::Entity);
        scene.node_mut(lower).unwrap().floor = 0;
        // Active floor 1: floor-2 node is above (hidden), floor-0 node is
        // below (visible, negative offset).
        assert_eq!(
            node_draw_offset(project.active_scene(), room, 1, upper),
            None
        );
        let lo = node_draw_offset(project.active_scene(), room, 1, lower);
        assert!(lo.is_some() && lo.unwrap() < 0);
    }
}
