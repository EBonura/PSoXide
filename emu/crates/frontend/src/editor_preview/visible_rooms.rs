//! Visible-room selection for the editor 3D preview.

use super::*;

/// Rooms that are not hidden by the editor Scene tree.
pub(super) fn visible_room_grids<'a>(
    project: &'a ProjectDocument,
    hidden_scene_nodes: &HashSet<NodeId>,
) -> Vec<(psxed_project::NodeId, &'a WorldGrid)> {
    let scene = project.active_scene();
    scene
        .nodes()
        .iter()
        .filter_map(|node| {
            if scene_node_hidden(scene, hidden_scene_nodes, node.id) {
                return None;
            }
            let NodeKind::Section { grid } = &node.kind else {
                return None;
            };
            Some((node.id, grid))
        })
        .collect()
}

/// One floor of one room to render, with its world-space Y offset.
pub(super) struct PreviewFloor<'a> {
    pub(super) room: NodeId,
    pub(super) grid: &'a WorldGrid,
    /// Which floor of the room this is (0 = base). Entities/props/lights
    /// belong to exactly one floor (`SceneNode::floor`) and render only
    /// on their own floor entry, so a stacked room draws each entity once
    /// (not once per floor) at the right elevation.
    pub(super) floor_index: usize,
    /// Engine-unit Y offset for this floor's geometry/entities, so
    /// stacked floors render at their real elevation.
    pub(super) y_offset: i32,
    /// True for the floor currently being authored (the edit target).
    pub(super) active: bool,
}

/// Rooms considered by the editable 3D preview.
///
/// Coastal Ruins imports as a real TR room graph, not a toy single-map
/// scene. Rendering every unhidden room every host frame turns the
/// editor preview into a whole-level renderer. Instead, keep the 3D
/// viewport TR-like: render the active room plus a bounded portal
/// neighborhood. The scene tree and 2D map remain the whole-world
/// inspection tools.
pub(super) fn preview_room_grids<'a>(
    project: &'a ProjectDocument,
    hidden_scene_nodes: &HashSet<NodeId>,
    active_room: Option<NodeId>,
    active_floor: usize,
    selected: NodeId,
    selected_primitive: Option<psxed_ui::Selection>,
    selected_primitives: &[psxed_ui::Selection],
    validation_issue_primitives: &[psxed_ui::Selection],
) -> Vec<PreviewFloor<'a>> {
    let scene = project.active_scene();
    let mut seeds = Vec::new();
    push_preview_room_seed(scene, hidden_scene_nodes, active_room, &mut seeds);
    if let Some(selection) = selected_primitive {
        push_preview_room_seed(
            scene,
            hidden_scene_nodes,
            Some(selection.room()),
            &mut seeds,
        );
    }
    for selection in selected_primitives {
        push_preview_room_seed(
            scene,
            hidden_scene_nodes,
            Some(selection.room()),
            &mut seeds,
        );
    }
    if let Some(room) = selected_room_ancestor(scene, hidden_scene_nodes, selected) {
        push_preview_room_seed(scene, hidden_scene_nodes, Some(room), &mut seeds);
    }
    for selection in validation_issue_primitives {
        push_preview_room_seed(
            scene,
            hidden_scene_nodes,
            Some(selection.room()),
            &mut seeds,
        );
    }
    if seeds.is_empty() {
        if let Some((first, _)) = visible_room_grids(project, hidden_scene_nodes).first() {
            seeds.push(*first);
        }
    }

    let mut result = Vec::new();
    let mut seen = HashSet::new();
    let mut queue = VecDeque::new();
    for seed in seeds {
        queue.push_back((seed, 0usize));
    }

    while let Some((room, depth)) = queue.pop_front() {
        if !seen.insert(room) {
            continue;
        }
        if scene_node_hidden(scene, hidden_scene_nodes, room) {
            continue;
        }
        let Some(base) = scene.node(room).and_then(|node| match &node.kind {
            NodeKind::Section { grid } => Some(grid),
            _ => None,
        }) else {
            continue;
        };
        // Sims-style floor view for the active room: the ACTIVE floor is
        // the working plane drawn at Y=0; floors BELOW descend for
        // context; floors ABOVE are hidden. Resolution comes from the
        // shared `floor_view` source of truth so render and pick agree.
        if Some(room) == active_room {
            for resolved in floor_view::active_room_floors(scene, room, active_floor) {
                if let Some(grid) = base.floor(resolved.floor_index) {
                    result.push(PreviewFloor {
                        room,
                        grid,
                        floor_index: resolved.floor_index,
                        y_offset: resolved.y_offset,
                        active: resolved.active,
                    });
                }
            }
        } else {
            result.push(PreviewFloor {
                room,
                grid: base,
                floor_index: 0,
                y_offset: 0,
                active: false,
            });
        }
        if result.len() >= EDITOR_PREVIEW_MAX_ROOMS || depth >= EDITOR_PREVIEW_PORTAL_DEPTH {
            continue;
        }
        for connected in connected_portal_rooms(scene, room) {
            if !seen.contains(&connected) {
                queue.push_back((connected, depth + 1));
            }
        }
    }

    result
}

pub(super) fn push_preview_room_seed(
    scene: &Scene,
    hidden_scene_nodes: &HashSet<NodeId>,
    room: Option<NodeId>,
    seeds: &mut Vec<NodeId>,
) {
    let Some(room) = room else {
        return;
    };
    if seeds.contains(&room) || scene_node_hidden(scene, hidden_scene_nodes, room) {
        return;
    }
    if scene
        .node(room)
        .is_some_and(|node| matches!(node.kind, NodeKind::Section { .. }))
    {
        seeds.push(room);
    }
}

pub(super) fn selected_room_ancestor(
    scene: &Scene,
    hidden_scene_nodes: &HashSet<NodeId>,
    selected: NodeId,
) -> Option<NodeId> {
    let mut current = Some(selected);
    while let Some(id) = current {
        if scene_node_hidden(scene, hidden_scene_nodes, id) {
            return None;
        }
        let node = scene.node(id)?;
        if matches!(node.kind, NodeKind::Section { .. }) {
            return Some(id);
        }
        current = node.parent;
    }
    None
}

pub(super) fn room_ancestor(scene: &Scene, node_id: NodeId) -> Option<NodeId> {
    let mut current = Some(node_id);
    while let Some(id) = current {
        let node = scene.node(id)?;
        if matches!(node.kind, NodeKind::Section { .. }) {
            return Some(id);
        }
        current = node.parent;
    }
    None
}

pub(super) fn connected_portal_rooms(scene: &Scene, room: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    for node in scene.nodes() {
        let NodeKind::Portal {
            target_room: Some(target),
            ..
        } = &node.kind
        else {
            continue;
        };
        let target = *target;
        if room_ancestor(scene, node.id) == Some(room) {
            if target != room && !out.contains(&target) {
                out.push(target);
            }
        } else if target == room {
            if let Some(source) = room_ancestor(scene, node.id) {
                if source != room && !out.contains(&source) {
                    out.push(source);
                }
            }
        }
    }
    out
}
