use super::*;
use crate::workspace::tools::BRUSH_CREATE_HEIGHT;
use psxed_project::brush::Brush;

fn brush_workspace(label: &str) -> EditorWorkspace {
    let mut workspace = ViewportHarness::floored_room(label, 2).workspace;
    workspace.active_tool = ViewTool::Brush;
    workspace
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

    let scene = workspace.project.active_scene();
    assert_eq!(scene.brushes.len(), 2);
    let a = scene.brushes[0].solve();
    let b = scene.brushes[1].solve();
    assert_eq!(a.min[0].min(b.min[0]), 0.0);
    assert_eq!(a.max[0].max(b.max[0]), 128.0);
    assert!((a.max[0] - 64.0).abs() < 1.0e-6 || (b.max[0] - 64.0).abs() < 1.0e-6);
}
