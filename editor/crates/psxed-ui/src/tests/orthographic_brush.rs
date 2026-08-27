use super::*;
use crate::workspace::tools::BRUSH_CREATE_HEIGHT;
use psxed_project::brush::Brush;

fn brush_workspace(label: &str) -> EditorWorkspace {
    let mut workspace = ViewportHarness::floored_room(label, 2).workspace;
    workspace.active_tool = ViewTool::Brush;
    workspace
}

fn draw_primitive(
    workspace: &mut EditorWorkspace,
    view: OrthographicView,
    shape: BrushDrawShape,
    start: [f32; 2],
    end: [f32; 2],
) {
    workspace.set_orthographic_view(view);
    workspace.brush_draw_settings.shape = shape;
    workspace.begin_brush_drag_2d(start);
    workspace.update_brush_drag_2d(end);
    workspace.commit_brush_drag();
}

#[test]
fn orthographic_projection_and_unprojection_cover_all_world_axes() {
    let world = [11.0, 22.0, 33.0];
    let focus = [1.0, 2.0, 3.0];

    assert_eq!(OrthographicView::Top.project_f32(world), [11.0, 33.0]);
    assert_eq!(
        OrthographicView::Top.unproject([11.0, 33.0], focus),
        [11.0, 2.0, 33.0]
    );

    assert_eq!(OrthographicView::Front.project_f32(world), [11.0, 22.0]);
    assert_eq!(
        OrthographicView::Front.unproject([11.0, 22.0], focus),
        [11.0, 22.0, 3.0]
    );

    assert_eq!(OrthographicView::Side.project_f32(world), [33.0, 22.0]);
    assert_eq!(
        OrthographicView::Side.unproject([33.0, 22.0], focus),
        [1.0, 22.0, 33.0]
    );
}

#[test]
fn orthographic_views_share_world_focus_zoom_grid_and_selection() {
    let mut workspace = brush_workspace("orthographic_shared_state");
    workspace.orthographic_focus = [128.0, 256.0, 512.0];
    workspace.viewport_zoom = 3.25;
    workspace.snap_units = 32;
    workspace.show_grid = false;
    workspace
        .project
        .active_scene_mut()
        .brushes
        .push(Brush::cuboid([0, 0, 0], [64, 64, 64]));
    workspace.selected_brush = Some(0);
    workspace.selected_brush_face = Some(5);
    let panel = Rect::from_min_size(Pos2::new(20.0, 40.0), Vec2::new(640.0, 480.0));

    for view in OrthographicView::ALL {
        workspace.set_orthographic_view(view);
        let transform = ViewportTransform::from_focus(
            panel,
            view.project_f32(workspace.orthographic_focus),
            workspace.viewport_zoom,
        );
        assert_eq!(
            transform.world_to_screen(view.project_f32(workspace.orthographic_focus)),
            panel.center()
        );
        assert_eq!(workspace.orthographic_focus, [128.0, 256.0, 512.0]);
        assert_eq!(workspace.viewport_zoom, 3.25);
        assert_eq!(workspace.snap_units, 32);
        assert!(!workspace.show_grid);
        assert_eq!(workspace.selected_brush, Some(0));
        assert_eq!(workspace.selected_brush_face, Some(5));
    }
}

#[test]
fn viewport_shortcut_cycle_visits_3d_top_front_and_side() {
    let mut workspace = brush_workspace("orthographic_shortcut_cycle");
    workspace.view_2d = false;

    workspace.cycle_view_dimension_group(false);
    assert!(workspace.view_2d);
    assert_eq!(workspace.orthographic_view, OrthographicView::Top);
    workspace.cycle_view_dimension_group(false);
    assert_eq!(workspace.orthographic_view, OrthographicView::Front);
    workspace.cycle_view_dimension_group(false);
    assert_eq!(workspace.orthographic_view, OrthographicView::Side);
    workspace.cycle_view_dimension_group(false);
    assert!(!workspace.view_2d);

    workspace.cycle_view_dimension_group(true);
    assert!(workspace.view_2d);
    assert_eq!(workspace.orthographic_view, OrthographicView::Side);
}

#[test]
fn framing_updates_visible_focus_axes_and_preserves_the_hidden_axis() {
    let mut workspace = brush_workspace("orthographic_frame_focus");
    workspace
        .project
        .active_scene_mut()
        .brushes
        .push(Brush::cuboid([0, 0, 0], [128, 256, 384]));
    workspace.selected_brush = Some(0);
    workspace.last_viewport_size = Vec2::new(800.0, 600.0);
    workspace.orthographic_focus = [10.0, 20.0, 777.0];

    workspace.set_orthographic_view(OrthographicView::Front);
    workspace.frame_viewport();
    assert_eq!(workspace.orthographic_focus, [64.0, 128.0, 777.0]);

    workspace.set_orthographic_view(OrthographicView::Side);
    workspace.frame_viewport();
    assert_eq!(workspace.orthographic_focus, [64.0, 128.0, 192.0]);
}

#[test]
fn every_orthographic_axis_picks_the_nearest_visible_brush_face() {
    let mut workspace = brush_workspace("orthographic_face_pick");
    workspace
        .project
        .active_scene_mut()
        .brushes
        .push(Brush::cuboid([0, 0, 0], [128, 256, 384]));

    workspace.set_orthographic_view(OrthographicView::Top);
    assert_eq!(workspace.pick_brush_face_at_2d([64.0, 192.0]), Some((0, 5)));
    assert!(workspace.select_brush_at_2d([64.0, 192.0]));
    assert_eq!(workspace.selected_brush_face, Some(5));

    workspace.set_orthographic_view(OrthographicView::Front);
    assert_eq!(workspace.pick_brush_face_at_2d([64.0, 128.0]), Some((0, 1)));
    assert!(workspace.select_brush_at_2d([64.0, 128.0]));
    assert_eq!(workspace.selected_brush_face, Some(1));

    workspace.set_orthographic_view(OrthographicView::Side);
    assert_eq!(
        workspace.pick_brush_face_at_2d([192.0, 128.0]),
        Some((0, 3))
    );
    assert!(workspace.select_brush_at_2d([192.0, 128.0]));
    assert_eq!(workspace.selected_brush_face, Some(3));
}

#[test]
fn exact_projected_overlap_prefers_brush_nearest_the_viewer() {
    let mut workspace = brush_workspace("orthographic_depth_pick");
    let scene = workspace.project.active_scene_mut();
    scene.brushes.push(Brush::cuboid([0, 0, 0], [128, 64, 128]));
    scene
        .brushes
        .push(Brush::cuboid([0, 128, 0], [128, 192, 128]));

    workspace.set_orthographic_view(OrthographicView::Top);
    assert_eq!(workspace.pick_brush_face_at_2d([64.0, 64.0]), Some((1, 5)));
}

#[test]
fn brush_creation_uses_active_plane_and_preserves_top_defaults() {
    let mut workspace = brush_workspace("orthographic_create");

    workspace.set_orthographic_view(OrthographicView::Top);
    workspace.begin_brush_drag_2d([100.0, 200.0]);
    workspace.update_brush_drag_2d([612.0, 456.0]);
    workspace.commit_brush_drag();
    let top = workspace.project.active_scene().brushes[0].solve();
    assert_eq!(top.min, [96.0, 0.0, 208.0]);
    assert_eq!(top.max, [608.0, BRUSH_CREATE_HEIGHT as f64, 464.0]);

    workspace.orthographic_focus = [0.0, 0.0, 64.0];
    workspace.set_orthographic_view(OrthographicView::Front);
    workspace.begin_brush_drag_2d([32.0, 16.0]);
    workspace.update_brush_drag_2d([160.0, 144.0]);
    workspace.commit_brush_drag();
    let front = workspace.project.active_scene().brushes[1].solve();
    assert_eq!(front.min, [32.0, 16.0, 64.0]);
    assert_eq!(front.max, [160.0, 144.0, 320.0]);

    workspace.orthographic_focus = [-128.0, 0.0, 0.0];
    workspace.set_orthographic_view(OrthographicView::Side);
    workspace.begin_brush_drag_2d([64.0, 0.0]);
    workspace.update_brush_drag_2d([192.0, 128.0]);
    workspace.commit_brush_drag();
    let side = workspace.project.active_scene().brushes[2].solve();
    assert_eq!(side.min, [-128.0, 0.0, 64.0]);
    assert_eq!(side.max, [128.0, 128.0, 192.0]);
}

#[test]
fn ramp_draw_rises_toward_the_selected_world_direction() {
    for (direction, high_axis, high_coordinate) in [
        (BrushCardinalDirection::North, 2, 0.0),
        (BrushCardinalDirection::East, 0, 512.0),
        (BrushCardinalDirection::South, 2, 512.0),
        (BrushCardinalDirection::West, 0, 0.0),
    ] {
        let mut workspace = brush_workspace("primitive_ramp");
        workspace.brush_draw_settings.direction = direction;
        draw_primitive(
            &mut workspace,
            OrthographicView::Top,
            BrushDrawShape::Ramp,
            [0.0, 0.0],
            [512.0, 512.0],
        );
        let brush = &workspace.project.active_scene().brushes[0];
        let solved = brush.solve();
        assert!(solved.is_valid());
        assert_eq!(brush.faces.len(), 5);
        let high_vertices: Vec<_> = solved
            .polygons
            .iter()
            .flatten()
            .flat_map(|polygon| polygon.verts.iter().copied())
            .filter(|vertex| vertex[1] == f64::from(BRUSH_CREATE_HEIGHT))
            .collect();
        assert!(!high_vertices.is_empty());
        assert!(high_vertices
            .iter()
            .all(|vertex| vertex[high_axis] == high_coordinate));
    }
}

#[test]
fn cylinder_draw_uses_requested_sides_and_quantises_every_authored_point() {
    let mut workspace = brush_workspace("primitive_cylinder");
    workspace.snap_units = 16;
    workspace.brush_draw_settings.cylinder_sides = 8;
    draw_primitive(
        &mut workspace,
        OrthographicView::Top,
        BrushDrawShape::Cylinder,
        [0.0, 0.0],
        [512.0, 512.0],
    );

    let brush = &workspace.project.active_scene().brushes[0];
    assert_eq!(brush.faces.len(), 10);
    assert!(brush.solve().is_valid());
    assert!(brush
        .faces
        .iter()
        .flat_map(|face| face.points)
        .flatten()
        .all(|coordinate| coordinate % 16 == 0));
}

#[test]
fn doorway_and_curved_arches_create_grouped_native_brushes_in_one_undo_step() {
    let mut doorway = brush_workspace("primitive_doorway_arch");
    doorway.snap_units = 16;
    doorway.brush_draw_settings.direction = BrushCardinalDirection::North;
    doorway.brush_draw_settings.arch_segments = 6;
    doorway.brush_draw_settings.arch_thickness = 64;
    draw_primitive(
        &mut doorway,
        OrthographicView::Front,
        BrushDrawShape::DoorwayArch,
        [-256.0, 0.0],
        [256.0, 512.0],
    );
    let doorway_scene = doorway.project.active_scene();
    assert_eq!(doorway_scene.brushes.len(), 8);
    let doorway_group = doorway_scene.brushes[0].group.expect("arch group");
    assert!(doorway_scene
        .brushes
        .iter()
        .all(|brush| brush.group == Some(doorway_group) && brush.solve().is_valid()));
    assert!(matches!(
        doorway_scene.node(doorway_group).map(|node| &node.kind),
        Some(NodeKind::Group)
    ));
    doorway.do_undo();
    assert!(doorway.project.active_scene().brushes.is_empty());
    assert!(doorway.project.active_scene().node(doorway_group).is_none());

    let mut curved = brush_workspace("primitive_curved_wall");
    curved.snap_units = 16;
    curved.brush_draw_settings.arch_segments = 4;
    curved.brush_draw_settings.arch_thickness = 64;
    curved.brush_draw_settings.curved_wall_arc_degrees = 90;
    draw_primitive(
        &mut curved,
        OrthographicView::Top,
        BrushDrawShape::CurvedWall,
        [0.0, 0.0],
        [512.0, 512.0],
    );
    assert_eq!(curved.project.active_scene().brushes.len(), 4);
    assert!(curved
        .project
        .active_scene()
        .brushes
        .iter()
        .all(|brush| brush.group.is_some() && brush.solve().is_valid()));
}

#[test]
fn stair_draw_creates_quantised_world_up_steps_as_one_group() {
    let mut workspace = brush_workspace("primitive_stairs");
    workspace.snap_units = 16;
    workspace.brush_draw_settings.direction = BrushCardinalDirection::East;
    workspace.brush_draw_settings.stair_steps = 4;
    draw_primitive(
        &mut workspace,
        OrthographicView::Top,
        BrushDrawShape::Stairs,
        [0.0, 0.0],
        [512.0, 512.0],
    );

    let scene = workspace.project.active_scene();
    assert_eq!(scene.brushes.len(), 4);
    let group = scene.brushes[0].group.expect("stairs group");
    let tops: Vec<_> = scene
        .brushes
        .iter()
        .map(|brush| brush.solve().max[1])
        .collect();
    assert_eq!(tops, vec![64.0, 128.0, 192.0, 256.0]);
    assert!(scene
        .brushes
        .iter()
        .all(|brush| brush.group == Some(group) && brush.solve().is_valid()));
}

#[test]
fn staged_stair_draw_uses_the_authored_height_instead_of_a_fixed_rise() {
    let mut workspace = brush_workspace("primitive_stairs_authored_height");
    workspace.snap_units = 16;
    workspace.brush_draw_settings.shape = BrushDrawShape::Stairs;
    workspace.brush_draw_settings.direction = BrushCardinalDirection::East;
    workspace.brush_draw_settings.stair_steps = 4;
    workspace.set_orthographic_view(OrthographicView::Top);

    workspace.begin_brush_drag_2d([0.0, 0.0]);
    workspace.update_brush_drag_2d([512.0, 512.0]);
    workspace.commit_brush_gesture_2d();
    assert!(workspace
        .brush_drag
        .is_some_and(|drag| { drag.stage == BrushCreateStage::Height && !drag.height_dragging }));
    assert!(workspace.begin_brush_height_drag(400.0));
    workspace.update_brush_height_drag(384.0);
    workspace.commit_brush_gesture_2d();

    let tops: Vec<_> = workspace
        .project
        .active_scene()
        .brushes
        .iter()
        .map(|brush| brush.solve().max[1])
        .collect();
    assert_eq!(tops, vec![96.0, 192.0, 288.0, 384.0]);
}

#[test]
fn staged_height_authors_every_brush_generator() {
    for shape in [
        BrushDrawShape::Box,
        BrushDrawShape::Ramp,
        BrushDrawShape::Cylinder,
        BrushDrawShape::DoorwayArch,
        BrushDrawShape::CurvedWall,
        BrushDrawShape::Stairs,
    ] {
        let mut workspace = brush_workspace("primitive_authored_height");
        workspace.snap_units = 16;
        workspace.brush_draw_settings.shape = shape;
        workspace.brush_draw_settings.arch_segments = 4;
        workspace.brush_draw_settings.stair_steps = 4;
        workspace.set_orthographic_view(OrthographicView::Top);
        workspace.begin_brush_drag_2d([0.0, 0.0]);
        workspace.update_brush_drag_2d([512.0, 512.0]);
        workspace.commit_brush_gesture_2d();
        assert!(workspace.begin_brush_height_drag(400.0), "{shape:?}");
        workspace.update_brush_height_drag(384.0);
        assert_eq!(workspace.brush_drag.unwrap().height_end, 384, "{shape:?}");
        workspace.commit_brush_gesture_2d();

        let brushes = &workspace.project.active_scene().brushes;
        assert!(!brushes.is_empty(), "{shape:?}");
        assert!(
            brushes.iter().any(|brush| brush.solve().max[1] == 384.0),
            "{shape:?}"
        );
    }
}

#[test]
fn front_view_move_changes_only_its_two_visible_axes_and_is_undoable() {
    let mut workspace = brush_workspace("orthographic_move");
    workspace
        .project
        .active_scene_mut()
        .brushes
        .push(Brush::cuboid([0, 0, 0], [128, 128, 128]));
    workspace.set_orthographic_view(OrthographicView::Front);

    assert!(workspace.begin_brush_move_2d([64.0, 64.0]));
    workspace.update_brush_move_2d([96.0, 96.0]);
    workspace.commit_brush_gesture_2d();
    let moved = workspace.project.active_scene().brushes[0].solve();
    assert_eq!(moved.min, [32.0, 32.0, 0.0]);
    assert_eq!(moved.max, [160.0, 160.0, 128.0]);

    workspace.do_undo();
    let restored = workspace.project.active_scene().brushes[0].solve();
    assert_eq!(restored.min, [0.0, 0.0, 0.0]);
    assert_eq!(restored.max, [128.0, 128.0, 128.0]);
}

#[test]
fn front_view_edge_drag_resizes_the_corresponding_face() {
    let mut workspace = brush_workspace("orthographic_resize");
    workspace
        .project
        .active_scene_mut()
        .brushes
        .push(Brush::cuboid([0, 0, 0], [128, 128, 128]));
    workspace.set_orthographic_view(OrthographicView::Front);

    assert!(workspace.begin_brush_resize_2d([64.0, 128.0], 1.0));
    assert_eq!(workspace.selected_brush_face, Some(5));
    workspace.update_brush_resize_2d([64.0, 160.0]);
    workspace.commit_brush_gesture_2d();

    let resized = workspace.project.active_scene().brushes[0].solve();
    assert_eq!(resized.min, [0.0, 0.0, 0.0]);
    assert_eq!(resized.max, [128.0, 160.0, 128.0]);
}

#[test]
fn front_view_clip_uses_the_hidden_z_axis_for_its_plane() {
    let mut workspace = brush_workspace("orthographic_clip");
    workspace
        .project
        .active_scene_mut()
        .brushes
        .push(Brush::cuboid([0, 0, 0], [128, 128, 128]));
    workspace.selected_brush = Some(0);
    workspace.set_orthographic_view(OrthographicView::Front);

    let first = workspace.brush_snap_2d([64.0, -64.0]);
    let second = workspace.brush_snap_2d([64.0, 192.0]);
    workspace.brush_clip_click(first);
    workspace.brush_clip_click(second);
    assert!(workspace.apply_brush_clip());

    let scene = workspace.project.active_scene();
    assert_eq!(scene.brushes.len(), 2);
    let a = scene.brushes[0].solve();
    let b = scene.brushes[1].solve();
    assert_eq!(a.min[0].min(b.min[0]), 0.0);
    assert_eq!(a.max[0].max(b.max[0]), 128.0);
    assert!((a.max[0] - 64.0).abs() < 1.0e-6 || (b.max[0] - 64.0).abs() < 1.0e-6);
}
