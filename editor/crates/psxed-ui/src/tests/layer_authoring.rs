use super::*;

fn select_cells(workspace: &mut EditorWorkspace, room: NodeId, cells: &[(u16, u16)]) {
    workspace.selection.selected_sectors = cells.iter().map(|&(sx, sz)| (room, sx, sz)).collect();
    workspace.selection.selected_sector = cells.first().copied();
    workspace.replace_node_selection(room);
}

#[test]
fn solid_layer_extrusion_builds_only_the_selected_footprint_and_undoes() {
    let (mut workspace, room) = workspace_with_populated_grid("solid-layer", 2, 2);
    select_cells(&mut workspace, room, &[(0, 0), (1, 0)]);

    workspace.extrude_selected_layer_above(false);

    let NodeKind::Section { grid } = &workspace.project.active_scene().node(room).unwrap().kind
    else {
        panic!("room node");
    };
    assert_eq!(grid.floor_count(), 2);
    assert_eq!(workspace.active_floor, 1);
    let upper = grid.floor(1).unwrap();
    let left = upper.sector(0, 0).unwrap();
    let right = upper.sector(1, 0).unwrap();
    assert!(left.floor.is_some() && left.ceiling.is_some());
    assert!(right.floor.is_some() && right.ceiling.is_some());
    assert!(upper.sector(0, 1).is_none());
    assert!(upper.sector(1, 1).is_none());
    assert!(left.walls.get(GridDirection::East).is_empty());
    assert!(right.walls.get(GridDirection::West).is_empty());
    assert!(!left.walls.get(GridDirection::West).is_empty());
    assert!(!right.walls.get(GridDirection::East).is_empty());

    workspace.do_undo();
    let NodeKind::Section { grid } = &workspace.project.active_scene().node(room).unwrap().kind
    else {
        panic!("room node");
    };
    assert_eq!(grid.floor_count(), 1);
    assert_eq!(workspace.active_floor, 0);
}

#[test]
fn open_layer_extrusion_authors_the_exact_gap_required_by_vertical_portals() {
    let (mut workspace, room) = workspace_with_populated_grid("open-layer", 2, 1);
    {
        let NodeKind::Section { grid } = &mut workspace
            .project
            .active_scene_mut()
            .node_mut(room)
            .unwrap()
            .kind
        else {
            panic!("room node");
        };
        for sx in 0..2 {
            grid.set_ceiling_aligned_to_neighbors(sx, 0, None);
        }
    }
    select_cells(&mut workspace, room, &[(0, 0)]);

    workspace.extrude_selected_layer_above(true);

    let NodeKind::Section { grid } = &workspace.project.active_scene().node(room).unwrap().kind
    else {
        panic!("room node");
    };
    let lower = grid.floor(0).unwrap().sector(0, 0).unwrap();
    let upper = grid.floor(1).unwrap().sector(0, 0).unwrap();
    assert!(lower.ceiling.is_none(), "lower ceiling must be removed");
    assert!(upper.floor.is_none(), "upper floor must be removed");
    assert!(upper.ceiling.is_some(), "upper volume must remain authored");

    workspace.set_selected_slab_below(false);
    let NodeKind::Section { grid } = &workspace.project.active_scene().node(room).unwrap().kind
    else {
        panic!("room node");
    };
    assert!(grid
        .floor(0)
        .unwrap()
        .sector(0, 0)
        .unwrap()
        .ceiling
        .is_some());
    assert!(grid.floor(1).unwrap().sector(0, 0).unwrap().floor.is_some());
}

#[test]
fn extrusion_below_base_preserves_existing_world_height_and_node_floor() {
    let (mut workspace, room) = workspace_with_populated_grid("below-base", 1, 1);
    let entity =
        workspace
            .project
            .active_scene_mut()
            .add_node(room, "Existing entity", NodeKind::Entity);
    workspace
        .project
        .active_scene_mut()
        .node_mut(room)
        .unwrap()
        .transform
        .translation[1] = 3.0;
    select_cells(&mut workspace, room, &[(0, 0)]);

    workspace.extrude_selected_layer_below(false);

    let scene = workspace.project.active_scene();
    let room_node = scene.node(room).unwrap();
    let NodeKind::Section { grid } = &room_node.kind else {
        panic!("room node");
    };
    assert_eq!(grid.floor_count(), 2);
    assert_eq!(room_node.transform.translation[1], 1.0);
    assert_eq!(scene.node(entity).unwrap().floor, 1);
    assert!(grid.floor(0).unwrap().sector(0, 0).unwrap().floor.is_some());
    assert!(grid.floor(1).unwrap().sector(0, 0).unwrap().floor.is_some());
    assert_eq!(workspace.active_floor, 0);
}

#[test]
fn deleting_empty_base_promotes_upper_layer_without_moving_or_losing_nodes() {
    let (mut workspace, room) = workspace_with_populated_grid("delete-base", 1, 1);
    let marker = workspace.project.active_scene_mut().add_node(
        room,
        "Layer marker",
        NodeKind::Portal {
            target_room: None,
            target_entry: String::new(),
            entry_name: String::new(),
            geometry: None,
        },
    );
    let entity =
        workspace
            .project
            .active_scene_mut()
            .add_node(room, "Upper entity", NodeKind::Entity);
    workspace
        .project
        .active_scene_mut()
        .node_mut(entity)
        .unwrap()
        .floor = 1;
    {
        let room_node = workspace.project.active_scene_mut().node_mut(room).unwrap();
        room_node.transform.translation[1] = -2.0;
        let NodeKind::Section { grid } = &mut room_node.kind else {
            panic!("room node");
        };
        grid.push_floor();
        grid.floor_mut(1).unwrap().set_floor(0, 0, 0, None);
        grid.sectors.fill(None);
    }

    assert!(workspace.can_delete_active_empty_layer());
    workspace.delete_active_empty_layer();

    let scene = workspace.project.active_scene();
    let room_node = scene.node(room).unwrap();
    let NodeKind::Section { grid } = &room_node.kind else {
        panic!("room node");
    };
    assert_eq!(grid.floor_count(), 1);
    assert!(grid.sector(0, 0).unwrap().floor.is_some());
    assert_eq!(room_node.transform.translation[1], 0.0);
    assert_eq!(scene.node(marker).unwrap().floor, 0);
    assert_eq!(scene.node(entity).unwrap().floor, 0);
    assert_eq!(workspace.active_floor, 0);

    workspace.do_undo();
    let scene = workspace.project.active_scene();
    let room_node = scene.node(room).unwrap();
    let NodeKind::Section { grid } = &room_node.kind else {
        panic!("room node");
    };
    assert_eq!(grid.floor_count(), 2);
    assert_eq!(room_node.transform.translation[1], -2.0);
    assert_eq!(scene.node(marker).unwrap().floor, 0);
    assert_eq!(scene.node(entity).unwrap().floor, 1);
}

#[test]
fn three_dimensional_face_selection_can_drive_layer_extrusion() {
    let (mut workspace, room) = workspace_with_populated_grid("face-layer", 1, 1);
    workspace.replace_primitive_selection(Selection::Face(FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Floor,
    }));

    assert!(workspace.can_author_selected_layer_footprint());
    workspace.extrude_selected_layer_above(false);

    let NodeKind::Section { grid } = &workspace.project.active_scene().node(room).unwrap().kind
    else {
        panic!("room node");
    };
    assert!(grid.floor(1).unwrap().sector(0, 0).is_some());
}
