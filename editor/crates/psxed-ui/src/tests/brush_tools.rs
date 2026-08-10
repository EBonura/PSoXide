use super::*;
use crate::workspace::tools::{tool_impl_3d, ToolFrame3d, BRUSH_CREATE_HEIGHT};
use psxed_project::brush::SolvedBrush;

fn brush_frame(harness: &ViewportHarness, pointer: Pos2) -> ToolFrame3d {
    ToolFrame3d {
        rect: harness.viewport,
        pointer_interact: Some(pointer),
        pointer_hover: Some(pointer),
        modifiers: egui::Modifiers::default(),
        pointer_target: None,
        hover_room: None,
        drag_delta_y: 0.0,
    }
}

#[test]
fn brush_tool_drag_creates_selectable_undoable_brush() {
    let mut harness = ViewportHarness::floored_room("brush_tool_create", 4);
    let center = harness.room_center();
    harness.frame(center, 3000.0);
    harness.workspace.active_tool = ViewTool::Brush;
    let tool = tool_impl_3d(ViewTool::Brush);

    // Drag a footprint across the middle of the panel.
    let press = brush_frame(&harness, Pos2::new(300.0, 300.0));
    let drag = brush_frame(&harness, Pos2::new(500.0, 400.0));
    tool.primary_pressed(&mut harness.workspace, &press);
    assert!(harness.workspace.brush_drag.is_some(), "press anchors a drag");
    tool.primary_dragged(&mut harness.workspace, &drag);
    tool.primary_released(&mut harness.workspace, &drag);

    let scene = harness.workspace.project.active_scene();
    assert_eq!(scene.brushes.len(), 1, "release commits the brush");
    let solved = scene.brushes[0].solve();
    assert!(solved.is_valid());
    assert_eq!(solved.min[1], 0.0);
    assert_eq!(solved.max[1], BRUSH_CREATE_HEIGHT as f64);
    assert!(solved.max[0] > solved.min[0]);
    assert!(solved.max[2] > solved.min[2]);
    assert_eq!(
        harness.workspace.selected_brush,
        Some(0),
        "new brush is selected"
    );
    assert!(harness.workspace.brush_drag.is_none(), "drag state cleared");

    // Clicking over the middle of the dragged footprint re-picks it and
    // selects the face the ray entered.
    harness.workspace.selected_brush = None;
    harness.workspace.selected_brush_face = None;
    let click = brush_frame(&harness, Pos2::new(400.0, 350.0));
    tool.primary_clicked(&mut harness.workspace, &click);
    assert_eq!(harness.workspace.selected_brush, Some(0));
    assert!(harness.workspace.selected_brush_face.is_some());

    // Clicking empty sky clears the selection.
    let sky = brush_frame(&harness, Pos2::new(400.0, 10.0));
    tool.primary_clicked(&mut harness.workspace, &sky);
    assert_eq!(harness.workspace.selected_brush, None);

    // The create is one undo step.
    harness.workspace.do_undo();
    assert_eq!(
        harness.workspace.project.active_scene().brushes.len(),
        0,
        "undo removes the brush"
    );
}

#[test]
fn brush_tool_face_drag_extrudes_top_face() {
    let mut harness = ViewportHarness::floored_room("brush_tool_extrude", 4);
    harness.frame(harness.room_center(), 3000.0);
    harness.workspace.active_tool = ViewTool::Brush;
    let tool = tool_impl_3d(ViewTool::Brush);

    // Create a brush.
    let press_create = brush_frame(&harness, Pos2::new(300.0, 300.0));
    let commit = brush_frame(&harness, Pos2::new(500.0, 400.0));
    tool.primary_pressed(&mut harness.workspace, &press_create);
    tool.primary_dragged(&mut harness.workspace, &commit);
    tool.primary_released(&mut harness.workspace, &commit);
    let solved = harness.workspace.project.active_scene().brushes[0].solve();
    let top_center = [
        (solved.min[0] + solved.max[0]) * 0.5,
        solved.max[1],
        (solved.min[2] + solved.max[2]) * 0.5,
    ];

    // Press on the projected top-face centre and drag 64 px upward.
    let camera = harness.workspace.viewport_3d_camera();
    let (nx, ny) = camera
        .normalized_panel_point_for_world([
            top_center[0] as f32,
            top_center[1] as f32,
            top_center[2] as f32,
        ])
        .expect("top face centre projects");
    let rect = harness.viewport;
    let press = Pos2::new(
        rect.center().x + nx * rect.width() * 0.5,
        rect.center().y + ny * rect.height() * 0.5,
    );
    let press_face = brush_frame(&harness, press);
    let lifted = brush_frame(&harness, Pos2::new(press.x, press.y - 64.0));
    tool.primary_pressed(&mut harness.workspace, &press_face);
    assert!(
        harness.workspace.brush_extrude.is_some(),
        "pressing a face starts an extrude"
    );
    tool.primary_dragged(&mut harness.workspace, &lifted);
    tool.primary_released(&mut harness.workspace, &lifted);

    let extruded = harness.workspace.project.active_scene().brushes[0].solve();
    // 64 px * 8 units/px = 512 units, snapped to the 16-unit grid.
    assert_eq!(extruded.max[1], solved.max[1] + 512.0);
    assert!(harness.workspace.brush_extrude.is_none());

    // The whole face drag is one undo step.
    harness.workspace.do_undo();
    let restored = harness.workspace.project.active_scene().brushes[0].solve();
    assert_eq!(restored.max[1], solved.max[1]);
}

#[test]
fn brush_tool_modifier_clicks_clip_selected_brush() {
    let mut harness = ViewportHarness::floored_room("brush_tool_clip", 4);
    harness.frame(harness.room_center(), 3000.0);
    harness.workspace.active_tool = ViewTool::Brush;
    let tool = tool_impl_3d(ViewTool::Brush);

    // Create and keep selected.
    let press = brush_frame(&harness, Pos2::new(280.0, 300.0));
    let commit = brush_frame(&harness, Pos2::new(520.0, 400.0));
    tool.primary_pressed(&mut harness.workspace, &press);
    tool.primary_dragged(&mut harness.workspace, &commit);
    tool.primary_released(&mut harness.workspace, &commit);
    assert_eq!(harness.workspace.selected_brush, Some(0));
    let whole = harness.workspace.project.active_scene().brushes[0].solve();

    // Two cmd-clicks across the middle: one above, one below the
    // footprint on screen, defining a vertical plane through it.
    let mut clip_a = brush_frame(&harness, Pos2::new(400.0, 280.0));
    let mut clip_b = brush_frame(&harness, Pos2::new(400.0, 430.0));
    clip_a.modifiers.command = true;
    clip_b.modifiers.command = true;
    tool.primary_clicked(&mut harness.workspace, &clip_a);
    assert!(harness.workspace.brush_clip_start.is_some());
    tool.primary_clicked(&mut harness.workspace, &clip_b);

    let scene = harness.workspace.project.active_scene();
    assert_eq!(scene.brushes.len(), 2, "clip split the brush in two");
    let a = scene.brushes[0].solve();
    let b = scene.brushes[1].solve();
    assert!(a.is_valid() && b.is_valid());
    // The halves partition the original footprint.
    let area = |s: &SolvedBrush| (s.max[0] - s.min[0]) * (s.max[2] - s.min[2]);
    assert!(area(&a) < area(&whole) - 1.0);
    assert!(area(&b) < area(&whole) - 1.0);
    // AABBs of the halves cover the whole footprint (they may overlap
    // when the clip plane is not axis-aligned, so >= rather than ==).
    assert!(area(&a) + area(&b) >= area(&whole) - 1.0);
    assert_eq!(a.min[1], 0.0);
    assert_eq!(b.min[1], 0.0);

    // One undo restores the unsplit brush.
    harness.workspace.do_undo();
    assert_eq!(harness.workspace.project.active_scene().brushes.len(), 1);
}

#[test]
fn brush_tool_clip_keep_back_replaces_in_place() {
    let mut harness = ViewportHarness::floored_room("brush_tool_clip_back", 4);
    harness.frame(harness.room_center(), 3000.0);
    harness.workspace.active_tool = ViewTool::Brush;
    let tool = tool_impl_3d(ViewTool::Brush);

    let press = brush_frame(&harness, Pos2::new(280.0, 300.0));
    let commit = brush_frame(&harness, Pos2::new(520.0, 400.0));
    tool.primary_pressed(&mut harness.workspace, &press);
    tool.primary_dragged(&mut harness.workspace, &commit);
    tool.primary_released(&mut harness.workspace, &commit);
    let whole = harness.workspace.project.active_scene().brushes[0].solve();

    harness.workspace.brush_clip_keep = BrushClipKeep::Back;
    let mut clip_a = brush_frame(&harness, Pos2::new(400.0, 280.0));
    let mut clip_b = brush_frame(&harness, Pos2::new(400.0, 430.0));
    clip_a.modifiers.command = true;
    clip_b.modifiers.command = true;
    tool.primary_clicked(&mut harness.workspace, &clip_a);
    tool.primary_clicked(&mut harness.workspace, &clip_b);

    let scene = harness.workspace.project.active_scene();
    assert_eq!(scene.brushes.len(), 1, "keep-back discards the front half");
    let kept = scene.brushes[0].solve();
    assert!(kept.is_valid());
    let area = |s: &SolvedBrush| (s.max[0] - s.min[0]) * (s.max[2] - s.min[2]);
    assert!(
        area(&kept) < area(&whole) - 1.0,
        "kept half covers less footprint than the whole"
    );
}

#[test]
fn brush_tool_shift_drag_moves_whole_brush() {
    let mut harness = ViewportHarness::floored_room("brush_tool_move", 4);
    harness.frame(harness.room_center(), 3000.0);
    harness.workspace.active_tool = ViewTool::Brush;
    let tool = tool_impl_3d(ViewTool::Brush);

    let press = brush_frame(&harness, Pos2::new(300.0, 300.0));
    let commit = brush_frame(&harness, Pos2::new(500.0, 400.0));
    tool.primary_pressed(&mut harness.workspace, &press);
    tool.primary_dragged(&mut harness.workspace, &commit);
    tool.primary_released(&mut harness.workspace, &commit);
    let before = harness.workspace.project.active_scene().brushes[0].solve();

    // Shift-press over the projected top-face centre, then drag right.
    let camera = harness.workspace.viewport_3d_camera();
    let (nx, ny) = camera
        .normalized_panel_point_for_world([
            ((before.min[0] + before.max[0]) * 0.5) as f32,
            before.max[1] as f32,
            ((before.min[2] + before.max[2]) * 0.5) as f32,
        ])
        .expect("brush projects");
    let rect = harness.viewport;
    let over = Pos2::new(
        rect.center().x + nx * rect.width() * 0.5,
        rect.center().y + ny * rect.height() * 0.5,
    );
    let mut grab = brush_frame(&harness, over);
    grab.modifiers.shift = true;
    let mut dragged = brush_frame(&harness, Pos2::new(over.x + 80.0, over.y));
    dragged.modifiers.shift = true;
    tool.primary_pressed(&mut harness.workspace, &grab);
    assert!(harness.workspace.brush_move.is_some(), "shift-press grabs");
    tool.primary_dragged(&mut harness.workspace, &dragged);
    tool.primary_released(&mut harness.workspace, &dragged);

    let after = harness.workspace.project.active_scene().brushes[0].solve();
    let size_before = before.max[0] - before.min[0];
    let size_after = after.max[0] - after.min[0];
    assert_eq!(size_before, size_after, "move preserves shape");
    assert!(
        after.min[0] != before.min[0] || after.min[2] != before.min[2],
        "brush moved"
    );

    harness.workspace.do_undo();
    let restored = harness.workspace.project.active_scene().brushes[0].solve();
    assert_eq!(restored.min, before.min);
}

#[test]
fn brush_2d_drag_creates_and_click_selects() {
    let mut harness = ViewportHarness::floored_room("brush_2d_create", 4);
    harness.workspace.active_tool = ViewTool::Brush;

    // Drag a footprint in 2D world coordinates (XZ plane).
    harness.workspace.begin_brush_drag_2d([100.0, 200.0]);
    harness.workspace.update_brush_drag_2d([612.0, 456.0]);
    harness.workspace.commit_brush_drag();

    let scene = harness.workspace.project.active_scene();
    assert_eq!(scene.brushes.len(), 1);
    let solved = scene.brushes[0].solve();
    // Corners snapped to the 16-unit grid.
    assert_eq!(solved.min[0], 96.0);
    assert_eq!(solved.min[2], 208.0); // 200/16 = 12.5 rounds to 13 -> 208
    assert_eq!(solved.max[1], BRUSH_CREATE_HEIGHT as f64);

    // 2D click inside the footprint selects; outside clears.
    harness.workspace.selected_brush = None;
    assert!(harness.workspace.select_brush_at_2d([300.0, 300.0]));
    assert_eq!(harness.workspace.selected_brush, Some(0));
    assert!(!harness.workspace.select_brush_at_2d([-500.0, -500.0]));
    assert_eq!(harness.workspace.selected_brush, None);
}

#[test]
fn brush_2d_clip_clicks_split_selected() {
    let mut harness = ViewportHarness::floored_room("brush_2d_clip", 4);
    harness.workspace.active_tool = ViewTool::Brush;
    harness.workspace.begin_brush_drag_2d([0.0, 0.0]);
    harness.workspace.update_brush_drag_2d([256.0, 128.0]);
    harness.workspace.commit_brush_drag();
    assert!(harness.workspace.select_brush_at_2d([128.0, 64.0]));

    // Two clip clicks along a vertical world line at x=128.
    harness.workspace.brush_clip_click([128, 0, -64]);
    assert!(harness.workspace.brush_clip_start.is_some());
    harness.workspace.brush_clip_click([128, 0, 192]);

    let scene = harness.workspace.project.active_scene();
    assert_eq!(scene.brushes.len(), 2, "axis clip splits in two");
    let a = scene.brushes[0].solve();
    let b = scene.brushes[1].solve();
    // Exact partition at x=128 for the axis-aligned clip plane.
    assert_eq!(a.max[0].max(b.max[0]), 256.0);
    assert_eq!(a.min[0].min(b.min[0]), 0.0);
    assert!((a.max[0] - 128.0).abs() < 1e-6 || (b.max[0] - 128.0).abs() < 1e-6);
}

#[test]
fn hollow_selected_brush_makes_room_walls() {
    let mut harness = ViewportHarness::floored_room("brush_hollow", 4);
    harness.workspace.active_tool = ViewTool::Brush;
    harness.workspace.begin_brush_drag_2d([0.0, 0.0]);
    harness.workspace.update_brush_drag_2d([512.0, 512.0]);
    harness.workspace.commit_brush_drag();
    assert_eq!(harness.workspace.selected_brush, Some(0));

    harness.workspace.hollow_selected_brush(16);
    assert_eq!(
        harness.workspace.project.active_scene().brushes.len(),
        6,
        "hollow replaces one brush with six slabs"
    );
    harness.workspace.do_undo();
    assert_eq!(harness.workspace.project.active_scene().brushes.len(), 1);
}

#[test]
fn snap_selected_brush_rounds_points() {
    let mut harness = ViewportHarness::floored_room("brush_snap_sel", 4);
    harness.workspace.active_tool = ViewTool::Brush;
    // Author an off-grid brush directly.
    harness
        .workspace
        .project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid([1, 0, -1], [65, 63, 62]));
    harness.workspace.selected_brush = Some(0);
    harness.workspace.snap_selected_brush();
    let solved = harness.workspace.project.active_scene().brushes[0].solve();
    assert_eq!(solved.min, [0.0, 0.0, 0.0]);
    assert_eq!(solved.max, [64.0, 64.0, 64.0]);
}

#[test]
fn texture_lock_compensates_face_uv_on_move() {
    let mut harness = ViewportHarness::floored_room("brush_uv_lock", 4);
    harness.workspace.active_tool = ViewTool::Brush;
    assert!(harness.workspace.brush_texture_lock, "lock defaults on");
    // Near the room centre so the framed camera can grab it.
    harness.workspace.begin_brush_drag_2d([1792.0, 1792.0]);
    harness.workspace.update_brush_drag_2d([2304.0, 2304.0]);
    harness.workspace.commit_brush_drag();
    let before = harness.workspace.project.active_scene().brushes[0].solve();

    // Shift-drag move through the tool; the top face UV offset must
    // compensate for the world shift (identity UV would stay [0, 0]
    // only if the brush did not move).
    let tool = tool_impl_3d(ViewTool::Brush);
    harness.frame(harness.room_center(), 3000.0);
    let camera = harness.workspace.viewport_3d_camera();
    let (nx, ny) = camera
        .normalized_panel_point_for_world([
            ((before.min[0] + before.max[0]) * 0.5) as f32,
            before.max[1] as f32,
            ((before.min[2] + before.max[2]) * 0.5) as f32,
        ])
        .expect("brush projects");
    let rect = harness.viewport;
    let over = Pos2::new(
        rect.center().x + nx * rect.width() * 0.5,
        rect.center().y + ny * rect.height() * 0.5,
    );
    let mut grab = brush_frame(&harness, over);
    grab.modifiers.shift = true;
    let mut dragged = brush_frame(&harness, Pos2::new(over.x + 90.0, over.y));
    dragged.modifiers.shift = true;
    tool.primary_pressed(&mut harness.workspace, &grab);
    tool.primary_dragged(&mut harness.workspace, &dragged);
    tool.primary_released(&mut harness.workspace, &dragged);

    let scene = harness.workspace.project.active_scene();
    let after = scene.brushes[0].solve();
    assert!(
        after.min != before.min,
        "brush moved (precondition for the lock check)"
    );
    let locked_any = scene.brushes[0]
        .faces
        .iter()
        .any(|face| face.uv.offset_texels != [0, 0]);
    assert!(locked_any, "texture lock compensated at least one face UV");
}

#[test]
fn escape_cancels_gestures_and_delete_removes_brush() {
    let mut harness = ViewportHarness::floored_room("brush_keys", 4);
    harness.workspace.active_tool = ViewTool::Brush;

    // Cancel a create drag: nothing commits.
    harness.workspace.begin_brush_drag_2d([0.0, 0.0]);
    harness.workspace.update_brush_drag_2d([256.0, 256.0]);
    harness.workspace.cancel_brush_gestures();
    harness.workspace.commit_brush_drag();
    assert_eq!(harness.workspace.project.active_scene().brushes.len(), 0);

    // Create one, then delete it (one undo step).
    harness.workspace.begin_brush_drag_2d([0.0, 0.0]);
    harness.workspace.update_brush_drag_2d([256.0, 256.0]);
    harness.workspace.commit_brush_drag();
    assert_eq!(harness.workspace.project.active_scene().brushes.len(), 1);
    harness.workspace.delete_selected_brush();
    assert_eq!(harness.workspace.project.active_scene().brushes.len(), 0);
    assert_eq!(harness.workspace.selected_brush, None);
    harness.workspace.do_undo();
    assert_eq!(harness.workspace.project.active_scene().brushes.len(), 1);
}

#[test]
fn brush_tool_zero_area_drag_commits_nothing() {
    let mut harness = ViewportHarness::floored_room("brush_tool_zero", 4);
    harness.frame(harness.room_center(), 3000.0);
    harness.workspace.active_tool = ViewTool::Brush;
    let tool = tool_impl_3d(ViewTool::Brush);

    let press = brush_frame(&harness, Pos2::new(300.0, 300.0));
    tool.primary_pressed(&mut harness.workspace, &press);
    tool.primary_released(&mut harness.workspace, &press);

    assert_eq!(harness.workspace.project.active_scene().brushes.len(), 0);
}

#[test]
fn brush_edits_mark_the_project_dirty_for_save_and_cook() {
    let mut harness = ViewportHarness::floored_room("brush_dirty", 4);
    harness.workspace.active_tool = ViewTool::Brush;
    assert!(!harness.workspace.is_dirty(), "fresh workspace is clean");

    // Create.
    harness.workspace.begin_brush_drag_2d([0.0, 0.0]);
    harness.workspace.update_brush_drag_2d([256.0, 256.0]);
    harness.workspace.commit_brush_drag();
    assert!(harness.workspace.is_dirty(), "create marks dirty");

    // Every other committed brush mutation must mark dirty too, or the
    // Play flow's save_if_dirty cooks stale on-disk data.
    let ops: [(&str, fn(&mut EditorWorkspace)); 4] = [
        ("duplicate", |ws| ws.duplicate_selected_brush()),
        ("snap", |ws| {
            ws.project.active_scene_mut().brushes[0] =
                psxed_project::brush::Brush::cuboid([1, 0, -1], [65, 63, 62]);
            ws.selected_brush = Some(0);
            ws.snap_selected_brush();
        }),
        ("hollow", |ws| {
            ws.selected_brush = Some(0);
            ws.hollow_selected_brush(16);
        }),
        ("delete", |ws| {
            ws.selected_brush = Some(0);
            ws.delete_selected_brush();
        }),
    ];
    for (label, op) in ops {
        harness.workspace.dirty = false;
        op(&mut harness.workspace);
        assert!(harness.workspace.is_dirty(), "{label} marks dirty");
    }

    // Gesture commits (move preview path) mark dirty as well.
    harness.workspace.dirty = false;
    harness
        .workspace
        .project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]));
    harness.workspace.set_orthographic_view(OrthographicView::Top);
    assert!(harness.workspace.begin_brush_move_2d([64.0, 64.0]));
    harness.workspace.update_brush_move_2d([128.0, 64.0]);
    harness.workspace.commit_brush_gesture_2d();
    assert!(harness.workspace.is_dirty(), "move commit marks dirty");
}

#[test]
fn brush_mover_binding_accepts_doors_and_is_undoable() {
    let mut harness = ViewportHarness::floored_room("brush_mover_binding", 4);
    harness
        .workspace
        .project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [0, 0, 0],
            [128, 128, 128],
        ));
    harness.workspace.selected_brush = Some(0);
    let non_mover = harness.workspace.project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Decoration",
        NodeKind::Entity,
    );
    let door = harness.workspace.project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Lift Door",
        NodeKind::Logic {
            kind: psxed_project::LogicNodeKind::Door {
                box_prop: String::new(),
                start_open: false,
                open_offset: psxed_project::default_brush_door_open_offset(),
                travel_ticks: psxed_project::default_brush_door_travel_ticks(),
            },
            target: String::new(),
            killtarget: String::new(),
            master: String::new(),
            delay_ticks: 0,
            wait_ticks: 0,
            enabled: true,
        },
    );

    harness.workspace.set_selected_brush_mover(Some(non_mover));
    assert_eq!(
        harness.workspace.project.active_scene().brushes[0].mover,
        None
    );
    harness.workspace.set_selected_brush_mover(Some(door));
    assert_eq!(
        harness.workspace.project.active_scene().brushes[0].mover,
        Some(door)
    );
    harness.workspace.do_undo();
    assert_eq!(
        harness.workspace.project.active_scene().brushes[0].mover,
        None
    );
}
