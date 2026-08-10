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
