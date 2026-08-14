use super::*;

#[test]
fn place_player_spawn_refuses_second_player_source() {
    let (mut workspace, room) = workspace_with_populated_grid("single-player-spawn", 1, 1);
    workspace.active_tool = ViewTool::Place;
    workspace.place_kind = PlaceKind::PlayerSpawn;

    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [512.0, 0.0, 512.0]);
    let first_spawn = workspace.selected_node_id();
    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [768.0, 0.0, 768.0]);

    let player_sources: Vec<_> = workspace
        .project
        .active_scene()
        .nodes()
        .iter()
        .filter(|node| node_kind_is_player_source(&node.kind))
        .collect();
    assert_eq!(player_sources.len(), 1);
    assert_eq!(player_sources[0].id, first_spawn);
    assert_eq!(workspace.selected_node_id(), first_spawn);
    assert!(workspace
        .status
        .contains("Only one player source is allowed per world"));
}

#[test]
fn place_prop_refuses_duplicate_resource_at_same_position() {
    let (mut workspace, room) = workspace_with_populated_grid("duplicate-prop-place", 1, 1);
    let model = workspace
        .project
        .add_resource("Crate", ResourceData::Model(test_model_resource("crate")));
    workspace.active_tool = ViewTool::Place;
    workspace.place_kind = PlaceKind::ModelInstance;
    workspace.place_resource = Some(model);

    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [512.0, 0.0, 512.0]);
    let first = workspace.selected_node_id();
    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [512.0, 0.0, 512.0]);

    let scene = workspace.project.active_scene();
    let props: Vec<_> = scene
        .nodes()
        .iter()
        .filter(|node| {
            node.parent == Some(room)
                && matches!(node.kind, NodeKind::Entity)
                && entity_model_resource_id(scene, node) == Some(model)
                && entity_character_component_resource_id(scene, node).is_none()
                && entity_weapon_resource_id(scene, node).is_none()
        })
        .collect();
    assert_eq!(props.len(), 1);
    assert_eq!(props[0].id, first);
    assert_eq!(workspace.selected_node_id(), first);
    assert_eq!(workspace.status, "Prop already exists at this position");
}

#[test]
fn place_spawn_marker_refuses_duplicate_at_same_position() {
    let (mut workspace, room) = workspace_with_populated_grid("duplicate-spawn-place", 1, 1);
    workspace.active_tool = ViewTool::Place;
    workspace.place_kind = PlaceKind::SpawnMarker;

    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [512.0, 0.0, 512.0]);
    let first = workspace.selected_node_id();
    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [512.0, 0.0, 512.0]);

    let spawns: Vec<_> = workspace
        .project
        .active_scene()
        .nodes()
        .iter()
        .filter(|node| {
            node.parent == Some(room)
                && matches!(
                    node.kind,
                    NodeKind::SpawnPoint {
                        player: false,
                        character: None
                    }
                )
        })
        .collect();
    assert_eq!(spawns.len(), 1);
    assert_eq!(spawns[0].id, first);
    assert_eq!(workspace.selected_node_id(), first);
    assert_eq!(workspace.status, "Spawn already exists at this position");
}

#[test]
fn place_image_prop_defaults_to_room_sector_size() {
    let mut project = ProjectDocument::new("image-prop-sector-size-place");
    let material = project.add_resource(
        "Banner",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1536),
        },
    );
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.place_kind = PlaceKind::ImageProp;
    workspace.replace_resource_selection(material);

    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [768.0, 0.0, 768.0]);

    let node = workspace
        .project
        .active_scene()
        .node(workspace.selected_node_id())
        .expect("placed image prop is selected");
    let NodeKind::ImageProp { width, height, .. } = &node.kind else {
        panic!("expected image prop node");
    };
    assert_eq!(*width, 1536);
    assert_eq!(*height, 1536);
}

#[test]
fn place_image_prop_with_material_uses_it_directly() {
    // Materials own their image since the material/texture merge, so
    // placing an image prop uses the material as-is; no wrapper
    // resource gets created.
    let mut project = ProjectDocument::new("image-prop-texture-place");
    let material = project.add_resource(
        "Crimson Banner",
        ResourceData::Material(psxed_project::MaterialResource::opaque(Some(
            "assets/textures/crimson_banner.psxt".to_string(),
        ))),
    );
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.place_kind = PlaceKind::ImageProp;
    workspace.place_resource = Some(material);
    let resource_count = workspace.project.resources.len();

    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [512.0, 128.0, 512.0]);

    let node = workspace
        .project
        .active_scene()
        .node(workspace.selected_node_id())
        .expect("placed image prop is selected");
    let NodeKind::ImageProp {
        material: Some(used),
        ..
    } = &node.kind
    else {
        panic!("expected image prop node");
    };
    assert_eq!(*used, material);
    assert_eq!(workspace.project.resources.len(), resource_count);
    assert_eq!(workspace.status, "Placed Image Prop at 0,0");
    assert!(workspace.is_dirty());
}

#[test]
fn box_prop_tool_places_without_requiring_a_material_selection() {
    let (mut workspace, room) = workspace_with_populated_grid("box-prop-tool-place", 1, 1);
    workspace.project.add_resource(
        "Stone",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    workspace.project.add_resource(
        "Brick",
        ResourceData::Material(MaterialResource::opaque(None)),
    );

    workspace.set_active_tool_cycle_value((ViewTool::Place, Some(PlaceKind::BoxProp)));
    workspace.dispatch_paint_3d(
        Some((
            FaceRef {
                room,
                sx: 0,
                sz: 0,
                kind: FaceKind::Floor,
            },
            [512.0, 0.0, 512.0],
        )),
        None,
    );

    let node = workspace
        .project
        .active_scene()
        .node(workspace.selected_node_id())
        .expect("placed box prop is selected");
    let NodeKind::BoxProp { materials, .. } = &node.kind else {
        panic!("expected box prop node");
    };
    assert_eq!(*materials, [None; psxed_project::BOX_PROP_FACE_COUNT]);
    assert_eq!(node.parent, Some(room));
    assert_eq!(workspace.status, "Placed Box Prop at 0,0");
    assert_eq!(workspace.active_tool, ViewTool::Select);
    assert!(workspace.is_dirty());
}

#[test]
fn cylinder_prop_tool_places_a_separate_six_sided_node() {
    let (mut workspace, room) = workspace_with_populated_grid("cylinder-prop-tool-place", 1, 1);

    workspace.set_active_tool_cycle_value((ViewTool::Place, Some(PlaceKind::CylinderProp)));
    workspace.dispatch_paint_3d(
        Some((
            FaceRef {
                room,
                sx: 0,
                sz: 0,
                kind: FaceKind::Floor,
            },
            [512.0, 0.0, 512.0],
        )),
        None,
    );

    let node = workspace
        .project
        .active_scene()
        .node(workspace.selected_node_id())
        .expect("placed cylinder prop is selected");
    let NodeKind::CylinderProp {
        materials,
        geometry,
        collision_enabled,
        ..
    } = &node.kind
    else {
        panic!("expected cylinder prop node");
    };
    assert_eq!(
        *materials,
        [None; psxed_project::CYLINDER_PROP_MATERIAL_COUNT]
    );
    assert_eq!(geometry.sides, psxed_project::DEFAULT_CYLINDER_PROP_SIDES);
    assert!(*collision_enabled);
    assert_eq!(node.parent, Some(room));
    assert_eq!(workspace.status, "Placed Cylinder Prop at 0,0");
    assert_eq!(workspace.active_tool, ViewTool::Select);
}

#[test]
fn portal_icon_place_writes_edge_midpoint_marker() {
    let mut project = ProjectDocument::new("portal-edge-place");
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    grid.set_floor(1, 0, 0, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.active_tool = ViewTool::Place;
    workspace.place_kind = PlaceKind::Portal;
    workspace.portal_place_direction = GridDirection::East;

    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [512.0, 0.0, 512.0]);

    let scene = workspace.project.active_scene();
    let node = scene
        .node(workspace.selected_node_id())
        .expect("placed portal is selected");
    assert!(matches!(node.kind, NodeKind::Portal { .. }));
    assert_eq!(node.transform.translation, [0.0, 0.0, 0.0]);
    assert_eq!(workspace.active_tool, ViewTool::Select);
    let grid = workspace.room_grid_view(room).unwrap();
    let edge = portal_edge_for_node(grid, node).expect("portal snaps to edge");
    assert_eq!(edge.x, 0);
    assert_eq!(edge.z, 0);
    assert_eq!(edge.direction, GridDirection::East);
    assert_eq!(workspace.status, "Placed Portal on East edge at 0,0");
    assert!(workspace.is_dirty());
}

#[test]
fn portal_icon_place_refuses_duplicate_edge_marker() {
    let mut project = ProjectDocument::new("portal-edge-duplicate");
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    grid.set_floor(1, 0, 0, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.active_tool = ViewTool::Place;
    workspace.place_kind = PlaceKind::Portal;
    workspace.portal_place_direction = GridDirection::East;

    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [512.0, 0.0, 512.0]);
    let first = workspace.selected_node_id();
    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [512.0, 0.0, 512.0]);

    let portals: Vec<_> = workspace
        .project
        .active_scene()
        .nodes()
        .iter()
        .filter(|node| node.parent == Some(room) && matches!(node.kind, NodeKind::Portal { .. }))
        .collect();
    assert_eq!(portals.len(), 1);
    assert_eq!(portals[0].id, first);
    assert_eq!(workspace.selected_node_id(), first);
    assert_eq!(
        workspace.status,
        "Portal already exists on East edge at 0,0"
    );
}

#[test]
fn visible_portal_bounds_follow_the_authored_seam() {
    let mut project = ProjectDocument::new("portal-seam-bounds");
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    grid.set_floor(1, 0, 0, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.active_tool = ViewTool::Place;
    workspace.place_kind = PlaceKind::Portal;
    workspace.portal_place_direction = GridDirection::East;
    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [512.0, 0.0, 512.0]);

    let portal = workspace.selected_node_id();
    let bounds = workspace
        .collect_entity_bounds(Some(room))
        .into_iter()
        .find(|bounds| bounds.node == portal)
        .expect("visible portal has seam bounds");
    assert_eq!(bounds.kind, EntityBoundKind::Portal);
    assert!((bounds.center[0] - 1024.0).abs() < 0.001);
    assert!((bounds.center[2] - 512.0).abs() < 0.001);
    assert!(bounds.half_extents[0] >= 48.0);
    assert!(bounds.half_extents[2] >= 512.0);
    let t = ray_intersects_aabb(
        [
            bounds.center[0] - 4096.0,
            bounds.center[1],
            bounds.center[2],
        ],
        [1.0, 0.0, 0.0],
        bounds.center,
        bounds.half_extents,
    );
    assert!(t.is_some(), "portal seam bound is pickable");

    workspace.show_portals = false;
    assert!(workspace
        .collect_entity_bounds(Some(room))
        .into_iter()
        .all(|bounds| bounds.node != portal));
}

#[test]
fn portal_icon_place_rejects_edges_without_populated_neighbour() {
    let mut project = ProjectDocument::new("portal-edge-invalid");
    let mut grid = WorldGrid::empty(2, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.active_tool = ViewTool::Place;
    workspace.place_kind = PlaceKind::Portal;
    workspace.portal_place_direction = GridDirection::East;

    workspace.run_paint_action(ViewTool::Place, room, 0, 0, None, [512.0, 0.0, 512.0]);

    let portal_count = workspace
        .project
        .active_scene()
        .nodes()
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::Portal { .. }))
        .count();
    assert_eq!(portal_count, 0);
    assert_eq!(
        workspace.status,
        "Portal needs populated sectors on both sides of the East edge"
    );
    assert!(!workspace.is_dirty());
}

#[test]
fn material_assignment_updates_selected_triangle_override() {
    let mut project = ProjectDocument::new("triangle-materials");
    let original = project.add_resource(
        "Original",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let target = project.add_resource(
        "Target",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 0, Some(original));
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    let triangle = HorizontalTriangleRef {
        room,
        sx: 0,
        sz: 0,
        surface: HorizontalSurfaceKind::Floor,
        index: HorizontalTriangleIndex::A,
        corners: [Corner::NW, Corner::NE, Corner::SE],
    };
    workspace.selection.selected_primitive = Some(Selection::Triangle(triangle));

    assert_eq!(workspace.assign_selected_faces_material(Some(target)), 1);

    let grid = workspace.room_grid_view(room).unwrap();
    let floor = grid.sector(0, 0).unwrap().floor.as_ref().unwrap();
    assert_eq!(floor.material, Some(original));
    assert_eq!(floor.triangle_material(0), Some(target));
    assert_eq!(floor.triangle_material(1), Some(original));
    assert!(workspace.is_dirty());
}

#[test]
fn paint_floor_in_triangle_mode_targets_clicked_triangle_only() {
    let mut project = ProjectDocument::new("triangle-floor-paint");
    let original = project.add_resource(
        "Original",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let target = project.add_resource(
        "Target",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 0, Some(original));
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.horizontal_edit_mode = HorizontalEditMode::Triangle;
    workspace.brush_material = Some(target);

    workspace.run_paint_action(ViewTool::PaintFloor, room, 0, 0, None, [800.0, 0.0, 300.0]);

    let grid = workspace.room_grid_view(room).unwrap();
    let floor = grid.sector(0, 0).unwrap().floor.as_ref().unwrap();
    assert_eq!(floor.material, Some(original));
    assert_eq!(floor.triangle_material(0), Some(target));
    assert_eq!(floor.triangle_material(1), Some(original));
    assert!(matches!(
        workspace.selection.selected_primitive,
        Some(Selection::Triangle(HorizontalTriangleRef {
            surface: HorizontalSurfaceKind::Floor,
            index: HorizontalTriangleIndex::A,
            ..
        }))
    ));
    assert_eq!(workspace.selection.selected_resource, Some(target));
    assert!(workspace.is_dirty());
}

#[test]
fn paint_ceiling_in_triangle_mode_targets_clicked_triangle_only() {
    let mut project = ProjectDocument::new("triangle-ceiling-paint");
    let original = project.add_resource(
        "Original",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let target = project.add_resource(
        "Target",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.ensure_sector(0, 0).unwrap().ceiling =
        Some(GridHorizontalFace::flat(1024, Some(original)));
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.horizontal_edit_mode = HorizontalEditMode::Triangle;
    workspace.brush_material = Some(target);

    workspace.run_paint_action(
        ViewTool::PaintCeiling,
        room,
        0,
        0,
        None,
        [200.0, 1024.0, 300.0],
    );

    let grid = workspace.room_grid_view(room).unwrap();
    let ceiling = grid.sector(0, 0).unwrap().ceiling.as_ref().unwrap();
    assert_eq!(ceiling.material, Some(original));
    assert_eq!(ceiling.triangle_material(0), Some(original));
    assert_eq!(ceiling.triangle_material(1), Some(target));
    assert!(matches!(
        workspace.selection.selected_primitive,
        Some(Selection::Triangle(HorizontalTriangleRef {
            surface: HorizontalSurfaceKind::Ceiling,
            index: HorizontalTriangleIndex::B,
            ..
        }))
    ));
    assert_eq!(workspace.selection.selected_resource, Some(target));
    assert!(workspace.is_dirty());
}

fn workspace_with_terrain_strip(label: &str) -> (EditorWorkspace, NodeId, ResourceId, ResourceId) {
    let mut project = ProjectDocument::new(label);
    let generated = |base_color| {
        let mut material = MaterialResource::opaque(None);
        material.texture_mode = MaterialTextureMode::Generated;
        material.generated.base_color = base_color;
        material
    };
    let grass = project.add_resource("Grass", ResourceData::Material(generated([46, 108, 55])));
    let sand = project.add_resource("Sand", ResourceData::Material(generated([176, 142, 88])));
    let grid = WorldGrid::stone_room(3, 1, 1024, Some(grass), Some(grass));
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    (
        EditorWorkspace::with_project(std::env::temp_dir(), project),
        room,
        grass,
        sand,
    )
}

fn workspace_with_terrain_patch(label: &str) -> (EditorWorkspace, NodeId, ResourceId, ResourceId) {
    let mut project = ProjectDocument::new(label);
    let generated = |base_color| {
        let mut material = MaterialResource::opaque(None);
        material.texture_mode = MaterialTextureMode::Generated;
        material.generated.base_color = base_color;
        material
    };
    let grass = project.add_resource("Grass", ResourceData::Material(generated([46, 108, 55])));
    let sand = project.add_resource("Sand", ResourceData::Material(generated([176, 142, 88])));
    let grid = WorldGrid::stone_room(3, 3, 1024, Some(grass), Some(grass));
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    (
        EditorWorkspace::with_project(std::env::temp_dir(), project),
        room,
        grass,
        sand,
    )
}

#[test]
fn material_paint_direct_changes_only_the_clicked_floor() {
    let (mut workspace, room, grass, sand) = workspace_with_terrain_patch("direct-face-paint");
    workspace.brush_material = Some(sand);
    let target = FaceRef {
        room,
        sx: 1,
        sz: 1,
        kind: FaceKind::Floor,
    };

    workspace.run_paint_action(
        ViewTool::PaintMaterial,
        room,
        1,
        1,
        Some(target),
        [1536.0, 0.0, 1536.0],
    );

    let grid = workspace.room_grid_view(room).unwrap();
    for z in 0..3 {
        for x in 0..3 {
            let material = grid.sector(x, z).unwrap().floor.as_ref().unwrap().material;
            assert_eq!(
                material,
                Some(if (x, z) == (1, 1) { sand } else { grass }),
                "unexpected material change at {x},{z}"
            );
        }
    }
    assert!(!workspace.material_paint_blend);
    assert!(!workspace
        .project
        .resources
        .iter()
        .any(|resource| resource.name.starts_with(AUTO_PAINT_BLEND_PREFIX)));
}

#[test]
fn material_paint_eyedropper_samples_once_and_syncs_resource_selection() {
    let (mut workspace, room, grass, sand) = workspace_with_terrain_patch("sample-face-material");
    let target = FaceRef {
        room,
        sx: 1,
        sz: 1,
        kind: FaceKind::Floor,
    };
    workspace.active_tool = ViewTool::PaintMaterial;
    workspace.brush_material = Some(sand);
    workspace.material_paint_sampling = true;

    workspace.dispatch_paint_3d(Some((target, [1536.0, 0.0, 1536.0])), None);

    assert_eq!(workspace.brush_material, Some(grass));
    assert_eq!(workspace.selection.selected_resource, Some(grass));
    assert!(workspace.selection.selected_resources.contains(&grass));
    assert!(!workspace.material_paint_sampling);
    assert_eq!(workspace.face_material(target), Some(grass));
    assert!(workspace.status.contains("Sampled"));
}

#[test]
fn material_paint_eyedropper_unwraps_generated_blend_to_its_brush() {
    let (mut workspace, room, grass, sand) = workspace_with_terrain_patch("sample-face-blend");
    let target = FaceRef {
        room,
        sx: 1,
        sz: 1,
        kind: FaceKind::Floor,
    };
    workspace.active_tool = ViewTool::PaintMaterial;
    workspace.brush_material = Some(sand);
    workspace.material_paint_blend = true;
    workspace.run_paint_action(
        ViewTool::PaintMaterial,
        room,
        1,
        1,
        Some(target),
        [1536.0, 0.0, 1536.0],
    );
    let generated = workspace.face_material(target).unwrap();
    assert_ne!(generated, sand);
    let resource_count = workspace.project.resources.len();

    workspace.replace_resource_selection(grass);
    workspace.material_paint_sampling = true;
    workspace.dispatch_paint_3d(Some((target, [1536.0, 0.0, 1536.0])), None);

    assert_eq!(workspace.brush_material, Some(sand));
    assert_eq!(workspace.selection.selected_resource, Some(sand));
    assert_eq!(workspace.face_material(target), Some(generated));
    assert_eq!(workspace.project.resources.len(), resource_count);
    assert!(!workspace.material_paint_sampling);
}

#[test]
fn material_paint_blend_changes_only_the_clicked_floor() {
    let (mut workspace, room, grass, sand) = workspace_with_terrain_patch("blended-face-paint");
    workspace.brush_material = Some(sand);
    workspace.material_paint_blend = true;
    let target = FaceRef {
        room,
        sx: 1,
        sz: 1,
        kind: FaceKind::Floor,
    };

    workspace.run_paint_action(
        ViewTool::PaintMaterial,
        room,
        1,
        1,
        Some(target),
        [1536.0, 0.0, 1536.0],
    );

    let grid = workspace.room_grid_view(room).unwrap();
    let painted = grid
        .sector(1, 1)
        .unwrap()
        .floor
        .as_ref()
        .unwrap()
        .material
        .unwrap();
    for z in 0..3 {
        for x in 0..3 {
            if (x, z) != (1, 1) {
                assert_eq!(
                    grid.sector(x, z).unwrap().floor.as_ref().unwrap().material,
                    Some(grass),
                    "blend mutated neighbor {x},{z}"
                );
            }
        }
    }
    let material = match &workspace.project.resource(painted).unwrap().data {
        ResourceData::Material(material) => material,
        other => panic!("expected generated transition material, got {other:?}"),
    };
    assert_eq!(material.texture_mode, MaterialTextureMode::Transition);
    assert_eq!(material.transition.source_a, Some(grass));
    assert_eq!(material.transition.source_b, Some(sand));
    assert_eq!(material.transition.shape, TransitionMaskShape::Connected);
    assert_eq!(material.transition.connected_edges, 0);
    assert!(workspace.status.contains("only"));
}

#[test]
fn adjacent_blended_paint_tiles_form_one_continuous_region() {
    let (mut workspace, room, grass, sand) = workspace_with_terrain_patch("connected-face-paint");
    workspace.brush_material = Some(sand);
    workspace.material_paint_blend = true;

    for sx in 0..3 {
        let target = FaceRef {
            room,
            sx,
            sz: 1,
            kind: FaceKind::Floor,
        };
        workspace.run_paint_action(
            ViewTool::PaintMaterial,
            room,
            sx,
            1,
            Some(target),
            [(sx as f32 + 0.5) * 1024.0, 0.0, 1536.0],
        );
    }

    let grid = workspace.room_grid_view(room).unwrap();
    for z in [0, 2] {
        for x in 0..3 {
            assert_eq!(
                grid.sector(x, z).unwrap().floor.as_ref().unwrap().material,
                Some(grass),
                "painting the row mutated unpainted tile {x},{z}"
            );
        }
    }

    let expected_connections = [1 << 1, (1 << 3) | (1 << 1), 1 << 3];
    for (sx, expected) in expected_connections.into_iter().enumerate() {
        let material_id = grid
            .sector(sx as u16, 1)
            .unwrap()
            .floor
            .as_ref()
            .unwrap()
            .material
            .unwrap();
        let ResourceData::Material(material) =
            &workspace.project.resource(material_id).unwrap().data
        else {
            panic!("paint blend should resolve to a material");
        };
        assert_eq!(material.transition.source_a, Some(grass));
        assert_eq!(material.transition.source_b, Some(sand));
        assert_eq!(material.transition.shape, TransitionMaskShape::Connected);
        assert_eq!(material.transition.connected_edges, expected);
        assert_eq!(material.transition.edge_breakup, 20);
    }
}

#[test]
fn blended_paint_connections_follow_authored_face_uv_rotation() {
    let (mut workspace, room, _grass, sand) =
        workspace_with_terrain_patch("rotated-connected-face-paint");
    {
        let grid = workspace.room_floor_grid_mut(room).unwrap();
        for sx in 0..3 {
            grid.sector_mut(sx, 1)
                .unwrap()
                .floor
                .as_mut()
                .unwrap()
                .uv
                .rotation = GridUvRotation::Deg90;
        }
    }
    workspace.brush_material = Some(sand);
    workspace.material_paint_blend = true;

    for sx in 0..3 {
        let target = FaceRef {
            room,
            sx,
            sz: 1,
            kind: FaceKind::Floor,
        };
        workspace.run_paint_action(
            ViewTool::PaintMaterial,
            room,
            sx,
            1,
            Some(target),
            [(sx as f32 + 0.5) * 1024.0, 0.0, 1536.0],
        );
    }

    // A clockwise face UV rotation maps physical E -> texture S and
    // physical W -> texture N. The generated mask must use those transformed
    // edges or the three-tile row appears as three 90-degree islands.
    let expected_connections = [1 << 2, (1 << 0) | (1 << 2), 1 << 0];
    let grid = workspace.room_grid_view(room).unwrap();
    for (sx, expected) in expected_connections.into_iter().enumerate() {
        let material_id = grid
            .sector(sx as u16, 1)
            .unwrap()
            .floor
            .as_ref()
            .unwrap()
            .material
            .unwrap();
        let ResourceData::Material(material) =
            &workspace.project.resource(material_id).unwrap().data
        else {
            panic!("paint blend should resolve to a material");
        };
        assert_eq!(material.transition.connected_edges, expected);
    }
}

#[test]
fn material_paint_blend_uses_adjustable_coverage_and_edge_detail() {
    let (mut workspace, room, _grass, sand) = workspace_with_terrain_patch("detailed-face-paint");
    workspace.brush_material = Some(sand);
    workspace.material_paint_blend = true;
    workspace.material_paint_blend_coverage_percent = 70;
    workspace.material_paint_blend_edge_detail = 44;
    let target = FaceRef {
        room,
        sx: 1,
        sz: 1,
        kind: FaceKind::Floor,
    };

    workspace.run_paint_action(
        ViewTool::PaintMaterial,
        room,
        1,
        1,
        Some(target),
        [1536.0, 0.0, 1536.0],
    );

    let material_id = workspace
        .room_grid_view(room)
        .unwrap()
        .sector(1, 1)
        .unwrap()
        .floor
        .as_ref()
        .unwrap()
        .material
        .unwrap();
    let ResourceData::Material(material) = &workspace.project.resource(material_id).unwrap().data
    else {
        panic!("paint blend should resolve to a material");
    };
    assert_eq!(material.transition.coverage, 179);
    assert_eq!(material.transition.edge_breakup, 44);
}

#[test]
fn material_paint_blend_is_one_undo_step() {
    let (mut workspace, room, grass, sand) = workspace_with_terrain_patch("undo-face-blend");
    workspace.brush_material = Some(sand);
    workspace.material_paint_blend = true;
    let target = FaceRef {
        room,
        sx: 1,
        sz: 1,
        kind: FaceKind::Floor,
    };

    workspace.run_paint_action(
        ViewTool::PaintMaterial,
        room,
        1,
        1,
        Some(target),
        [1536.0, 0.0, 1536.0],
    );
    workspace.do_undo();

    let grid = workspace.room_grid_view(room).unwrap();
    for z in 0..3 {
        for x in 0..3 {
            assert_eq!(
                grid.sector(x, z).unwrap().floor.as_ref().unwrap().material,
                Some(grass)
            );
        }
    }
    assert!(!workspace
        .project
        .resources
        .iter()
        .any(|resource| resource.name.starts_with(AUTO_PAINT_BLEND_PREFIX)));
}

#[test]
fn material_paint_targets_one_ceiling_or_wall_face() {
    let (mut workspace, room, grass, sand) = workspace_with_terrain_strip("paint-all-surfaces");
    {
        let grid = workspace.room_floor_grid_mut(room).unwrap();
        for sx in 0..3 {
            grid.set_ceiling_aligned_to_neighbors(sx, 0, Some(grass));
            grid.add_wall(sx, 0, GridDirection::North, 0, 2048, Some(grass));
        }
    }
    workspace.brush_material = Some(sand);

    workspace.run_paint_action(
        ViewTool::PaintMaterial,
        room,
        1,
        0,
        Some(FaceRef {
            room,
            sx: 1,
            sz: 0,
            kind: FaceKind::Ceiling,
        }),
        [1536.0, 2048.0, 512.0],
    );
    workspace.run_paint_action(
        ViewTool::PaintMaterial,
        room,
        1,
        0,
        Some(FaceRef {
            room,
            sx: 1,
            sz: 0,
            kind: FaceKind::Wall {
                dir: GridDirection::North,
                stack: 1,
            },
        }),
        [1536.0, 1024.0, 1024.0],
    );

    let grid = workspace.room_grid_view(room).unwrap();
    for sx in 0..3 {
        assert_eq!(
            grid.sector(sx, 0)
                .unwrap()
                .ceiling
                .as_ref()
                .unwrap()
                .material,
            Some(if sx == 1 { sand } else { grass })
        );
        let walls = grid.sector(sx, 0).unwrap().walls.get(GridDirection::North);
        assert_eq!(walls[1].material, Some(if sx == 1 { sand } else { grass }));
    }
}

#[test]
fn generated_paint_blends_are_hidden_until_explicitly_searched() {
    let (mut workspace, room, _grass, sand) = workspace_with_terrain_patch("hidden-face-blend");
    workspace.brush_material = Some(sand);
    workspace.material_paint_blend = true;
    workspace.run_paint_action(
        ViewTool::PaintMaterial,
        room,
        1,
        1,
        Some(FaceRef {
            room,
            sx: 1,
            sz: 1,
            kind: FaceKind::Floor,
        }),
        [1536.0, 0.0, 1536.0],
    );
    let generated = workspace
        .project
        .resources
        .iter()
        .find(|resource| resource.name.starts_with(AUTO_PAINT_BLEND_PREFIX))
        .expect("generated paint blend");

    assert!(!workspace
        .project
        .material_options()
        .iter()
        .any(|(id, _)| *id == generated.id));
    assert!(!resource_matches_filter(
        generated,
        ResourceFilter::Material,
        ""
    ));
    assert!(resource_matches_filter(
        generated,
        ResourceFilter::Material,
        "paint blend"
    ));
}
#[test]
fn wall_paint_shape_stamps_diagonal_wall() {
    let mut project = ProjectDocument::new("diagonal-wall-paint");
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);

    workspace.wall_paint_shape = WallPaintShape::NorthEastSouthWest;
    workspace.run_paint_action(ViewTool::PaintWall, room, 0, 0, None, [512.0, 0.0, 512.0]);

    let grid = workspace.room_grid_view(room).unwrap();
    let sector = grid.sector(0, 0).unwrap();
    assert!(sector.walls.get(GridDirection::North).is_empty());
    assert_eq!(sector.walls.get(GridDirection::NorthEastSouthWest).len(), 1);
}

#[test]
fn paint_wall_on_existing_wall_adds_next_stack_entry() {
    let mut project = ProjectDocument::new("stacked-wall-paint");
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.add_wall(0, 0, GridDirection::North, 0, 1024, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    let picked = FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Wall {
            dir: GridDirection::North,
            stack: 0,
        },
    };

    workspace.run_paint_action(
        ViewTool::PaintWall,
        room,
        0,
        0,
        Some(picked),
        [512.0, 512.0, 1024.0],
    );

    let grid = workspace.room_grid_view(room).unwrap();
    let walls = grid.sector(0, 0).unwrap().walls.get(GridDirection::North);
    assert_eq!(walls.len(), 2);
    assert_eq!(walls[1].heights, [1024, 1024, 3072, 3072]);
}

#[test]
fn paint_wall_stamp_ignores_stack_to_prevent_drag_restacking() {
    let mut project = ProjectDocument::new("wall-stamp-stack-dedupe");
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.add_wall(0, 0, GridDirection::North, 0, 1024, None);
    grid.add_wall(0, 0, GridDirection::North, 1024, 2048, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.active_tool = ViewTool::PaintWall;
    let face = |stack| FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Wall {
            dir: GridDirection::North,
            stack,
        },
    };

    let first = workspace.paint_stamp_for(
        room,
        0,
        0,
        Some((face(0), [512.0, 512.0, 1024.0])),
        [512.0, 512.0, 1024.0],
    );
    let second = workspace.paint_stamp_for(
        room,
        0,
        0,
        Some((face(1), [512.0, 1536.0, 1024.0])),
        [512.0, 1536.0, 1024.0],
    );

    assert_eq!(first, second);
    assert_eq!(first.stack, None);
}

#[test]
fn horizontal_edit_mode_picks_triangle_face_halves() {
    let mut project = ProjectDocument::new("triangle-face-pick");
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    let face = FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Floor,
    };

    workspace.horizontal_edit_mode = HorizontalEditMode::Triangle;
    workspace.selection_mode = SelectionMode::Face;
    let picked = workspace.pick_primitive_from_hit(face, [900.0, 0.0, 900.0]);
    assert_eq!(
        picked,
        Selection::Triangle(HorizontalTriangleRef {
            room,
            sx: 0,
            sz: 0,
            surface: HorizontalSurfaceKind::Floor,
            index: HorizontalTriangleIndex::A,
            corners: [Corner::NW, Corner::NE, Corner::SE],
        })
    );

    workspace.horizontal_edit_mode = HorizontalEditMode::Quad;
    assert_eq!(
        workspace.pick_primitive_from_hit(face, [900.0, 0.0, 900.0]),
        Selection::Face(face)
    );
}

#[test]
fn triangle_edit_edge_mode_can_pick_split_diagonal() {
    let mut project = ProjectDocument::new("triangle-edge-pick");
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    let face = FaceRef {
        room,
        sx: 0,
        sz: 0,
        kind: FaceKind::Floor,
    };

    workspace.horizontal_edit_mode = HorizontalEditMode::Triangle;
    workspace.selection_mode = SelectionMode::Edge;
    let picked = workspace.pick_primitive_from_hit(face, [512.0, 0.0, 512.0]);

    assert_eq!(
        picked,
        Selection::Edge(EdgeRef {
            room,
            anchor: EdgeAnchor::Floor {
                sx: 0,
                sz: 0,
                dir: GridDirection::NorthWestSouthEast,
            },
        })
    );
}

#[test]
fn dragging_triangle_face_moves_only_its_three_corners() {
    let mut project = ProjectDocument::new("triangle-drag");
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.set_floor(0, 0, 0, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    let triangle = HorizontalTriangleRef {
        room,
        sx: 0,
        sz: 0,
        surface: HorizontalSurfaceKind::Floor,
        index: HorizontalTriangleIndex::A,
        corners: [Corner::NW, Corner::NE, Corner::SE],
    };

    workspace.selection.hovered_primitive = Some(Selection::Triangle(triangle));
    workspace.begin_primitive_drag(egui::Modifiers::NONE);
    workspace.update_primitive_drag(-8.0);
    workspace.end_primitive_drag();

    let parent_after = floor_heights(&workspace, room, 0, 0);
    assert_eq!(parent_after, [0; 4]);
    let triangle_after = floor_triangle_heights(&workspace, room, 0, 0, HorizontalTriangleIndex::A);
    assert_eq!(triangle_after[0], HEIGHT_QUANTUM);
    assert_eq!(triangle_after[1], HEIGHT_QUANTUM);
    assert_eq!(triangle_after[2], HEIGHT_QUANTUM);
}

#[test]
fn selected_room_bounds_follow_authored_tiles() {
    let mut project = ProjectDocument::new("bounds");
    let mut grid = WorldGrid::empty(6, 6, 1024);
    grid.set_floor(1, 2, 0, None);
    grid.set_floor(3, 4, 0, None);
    let room =
        project
            .active_scene_mut()
            .add_node(NodeId::ROOT, "Room", NodeKind::Section { grid });
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);

    workspace.replace_node_selection(room);

    let (center, half) = workspace
        .selected_bounds_3d()
        .expect("selected room has bounds");
    assert_eq!(center, [2560.0, 512.0, 3584.0]);
    assert_eq!(half, [1536.0, 512.0, 1536.0]);
}

#[test]
fn ctrl_selected_vertices_drag_together() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::legacy_grid_starter_dir()).unwrap();
    let room = workspace.active_room_id().expect("starter has room");
    let (sx, sz) = first_floor_sector(&workspace, room);
    let nw = Selection::Vertex(VertexRef {
        room,
        anchor: VertexAnchor::Floor {
            sx,
            sz,
            corner: Corner::NW,
        },
    });
    let ne = Selection::Vertex(VertexRef {
        room,
        anchor: VertexAnchor::Floor {
            sx,
            sz,
            corner: Corner::NE,
        },
    });
    let before = floor_heights(&workspace, room, sx, sz);

    let mut ctrl = egui::Modifiers::NONE;
    ctrl.ctrl = true;
    workspace.apply_primitive_selection_modifiers(nw, egui::Modifiers::NONE);
    workspace.apply_primitive_selection_modifiers(ne, ctrl);

    assert_eq!(workspace.selection.selected_primitives.len(), 2);

    workspace.selection.hovered_primitive = Some(nw);
    workspace.begin_primitive_drag(egui::Modifiers::NONE);
    workspace.update_primitive_drag(-8.0);
    workspace.end_primitive_drag();

    let after = floor_heights(&workspace, room, sx, sz);
    assert_eq!(
        after[Corner::NW.idx()],
        snap_height(before[Corner::NW.idx()] + HEIGHT_QUANTUM)
    );
    assert_eq!(
        after[Corner::NE.idx()],
        snap_height(before[Corner::NE.idx()] + HEIGHT_QUANTUM)
    );
    assert_eq!(after[Corner::SE.idx()], before[Corner::SE.idx()]);
    assert_eq!(after[Corner::SW.idx()], before[Corner::SW.idx()]);
}

#[test]
fn welded_vertex_drag_moves_coincident_grid_corners() {
    let (mut workspace, room) = workspace_with_populated_grid("welded-vertex-drag", 2, 2);
    let target = Selection::Vertex(VertexRef {
        room,
        anchor: VertexAnchor::Floor {
            sx: 0,
            sz: 0,
            corner: Corner::NE,
        },
    });

    workspace.selection.hovered_primitive = Some(target);
    workspace.begin_primitive_drag(egui::Modifiers::NONE);
    workspace.update_primitive_drag(-8.0);
    workspace.end_primitive_drag();

    assert_eq!(
        floor_heights(&workspace, room, 0, 0)[Corner::NE.idx()],
        HEIGHT_QUANTUM
    );
    assert_eq!(
        floor_heights(&workspace, room, 1, 0)[Corner::NW.idx()],
        HEIGHT_QUANTUM
    );
    assert_eq!(
        floor_heights(&workspace, room, 0, 1)[Corner::SE.idx()],
        HEIGHT_QUANTUM
    );
    assert_eq!(
        floor_heights(&workspace, room, 1, 1)[Corner::SW.idx()],
        HEIGHT_QUANTUM
    );
}

#[test]
fn detached_vertex_drag_moves_only_seed_corner() {
    let (mut workspace, room) = workspace_with_populated_grid("detached-vertex-drag", 2, 2);
    workspace.vertex_connectivity = VertexConnectivity::Detached;
    let target = Selection::Vertex(VertexRef {
        room,
        anchor: VertexAnchor::Floor {
            sx: 0,
            sz: 0,
            corner: Corner::NE,
        },
    });

    workspace.selection.hovered_primitive = Some(target);
    workspace.begin_primitive_drag(egui::Modifiers::NONE);
    workspace.update_primitive_drag(-8.0);
    workspace.end_primitive_drag();

    assert_eq!(
        floor_heights(&workspace, room, 0, 0)[Corner::NE.idx()],
        HEIGHT_QUANTUM
    );
    assert_eq!(floor_heights(&workspace, room, 1, 0)[Corner::NW.idx()], 0);
    assert_eq!(floor_heights(&workspace, room, 0, 1)[Corner::SE.idx()], 0);
    assert_eq!(floor_heights(&workspace, room, 1, 1)[Corner::SW.idx()], 0);
}

#[test]
fn ctrl_selected_edges_drag_together() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::legacy_grid_starter_dir()).unwrap();
    let room = workspace.active_room_id().expect("starter has room");
    let (sx, sz) = first_floor_sector(&workspace, room);
    let north = Selection::Edge(EdgeRef {
        room,
        anchor: EdgeAnchor::Floor {
            sx,
            sz,
            dir: GridDirection::North,
        },
    });
    let east = Selection::Edge(EdgeRef {
        room,
        anchor: EdgeAnchor::Floor {
            sx,
            sz,
            dir: GridDirection::East,
        },
    });
    let before = floor_heights(&workspace, room, sx, sz);

    let mut ctrl = egui::Modifiers::NONE;
    ctrl.ctrl = true;
    workspace.apply_primitive_selection_modifiers(north, egui::Modifiers::NONE);
    workspace.apply_primitive_selection_modifiers(east, ctrl);

    assert_eq!(workspace.selection.selected_primitives.len(), 2);

    workspace.selection.hovered_primitive = Some(north);
    workspace.begin_primitive_drag(egui::Modifiers::NONE);
    workspace.update_primitive_drag(-8.0);
    workspace.end_primitive_drag();

    let after = floor_heights(&workspace, room, sx, sz);
    assert_eq!(
        after[Corner::NW.idx()],
        snap_height(before[Corner::NW.idx()] + HEIGHT_QUANTUM)
    );
    assert_eq!(
        after[Corner::NE.idx()],
        snap_height(before[Corner::NE.idx()] + HEIGHT_QUANTUM)
    );
    assert_eq!(
        after[Corner::SE.idx()],
        snap_height(before[Corner::SE.idx()] + HEIGHT_QUANTUM)
    );
    assert_eq!(after[Corner::SW.idx()], before[Corner::SW.idx()]);
}
