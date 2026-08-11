use super::*;
use crate::workspace::tools::{tool_impl_3d, BrushHandle3d, ToolFrame3d, BRUSH_CREATE_HEIGHT};
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

fn run_real_egui_viewport_click(
    workspace: &mut EditorWorkspace,
    point: Pos2,
    viewport: EditorViewport3dPresentation,
) {
    let ctx = egui::Context::default();
    let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let draw = |ctx: &egui::Context, workspace: &mut EditorWorkspace| {
        egui::CentralPanel::default().show(ctx, |ui| {
            workspace.draw_viewport_3d_body(ui, viewport.clone());
        });
    };
    let input = |time, events| egui::RawInput {
        screen_rect: Some(screen),
        time: Some(time),
        events,
        ..egui::RawInput::default()
    };
    let _ = ctx.run(input(0.0, vec![]), |ctx| draw(ctx, workspace));
    let _ = ctx.run(
        input(
            1.0 / 60.0,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        ),
        |ctx| draw(ctx, workspace),
    );
    let _ = ctx.run(
        input(
            2.0 / 60.0,
            vec![egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        ),
        |ctx| draw(ctx, workspace),
    );
}

fn run_real_egui_viewport_drag(workspace: &mut EditorWorkspace, start: Pos2, end: Pos2) {
    let ctx = egui::Context::default();
    let texture = ctx.load_texture(
        "3d-handle-drag-viewport",
        egui::ColorImage::new([1, 1], egui::Color32::BLACK),
        egui::TextureOptions::NEAREST,
    );
    let viewport = EditorViewport3dPresentation::edit(texture.id(), Vec::new());
    let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let input = |time, events| egui::RawInput {
        screen_rect: Some(screen),
        time: Some(time),
        events,
        ..egui::RawInput::default()
    };
    let draw = |ctx: &egui::Context, workspace: &mut EditorWorkspace| {
        egui::CentralPanel::default().show(ctx, |ui| {
            workspace.draw_viewport_3d_body(ui, viewport.clone());
        });
    };
    let _ = ctx.run(input(0.0, vec![]), |ctx| draw(ctx, workspace));
    let _ = ctx.run(
        input(1.0 / 60.0, vec![egui::Event::PointerMoved(start)]),
        |ctx| draw(ctx, workspace),
    );
    let _ = ctx.run(
        input(
            2.0 / 60.0,
            vec![egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
        ),
        |ctx| draw(ctx, workspace),
    );
    // Cross egui's drag threshold while remaining inside the 9 px handle
    // pick radius, then move far enough for one snapped world-space step.
    let threshold = start + (end - start).normalized() * 7.0;
    let _ = ctx.run(
        input(3.0 / 60.0, vec![egui::Event::PointerMoved(threshold)]),
        |ctx| draw(ctx, workspace),
    );
    assert!(workspace.brush_vertex_drag.is_some());
    let _ = ctx.run(
        input(4.0 / 60.0, vec![egui::Event::PointerMoved(end)]),
        |ctx| draw(ctx, workspace),
    );
    let _ = ctx.run(
        input(
            5.0 / 60.0,
            vec![egui::Event::PointerButton {
                pos: end,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        ),
        |ctx| draw(ctx, workspace),
    );
}

fn run_real_egui_viewport_plain_drag(workspace: &mut EditorWorkspace, start: Pos2, end: Pos2) {
    let ctx = egui::Context::default();
    let texture = ctx.load_texture(
        "3d-plain-brush-drag",
        egui::ColorImage::new([1, 1], egui::Color32::BLACK),
        egui::TextureOptions::NEAREST,
    );
    let viewport = EditorViewport3dPresentation::edit(texture.id(), Vec::new());
    let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let input = |time, events| egui::RawInput {
        screen_rect: Some(screen),
        time: Some(time),
        events,
        ..egui::RawInput::default()
    };
    let draw = |ctx: &egui::Context, workspace: &mut EditorWorkspace| {
        egui::CentralPanel::default().show(ctx, |ui| {
            workspace.draw_viewport_3d_body(ui, viewport.clone());
        });
    };
    let _ = ctx.run(input(0.0, vec![]), |ctx| draw(ctx, workspace));
    let _ = ctx.run(
        input(1.0 / 60.0, vec![egui::Event::PointerMoved(start)]),
        |ctx| draw(ctx, workspace),
    );
    let _ = ctx.run(
        input(
            2.0 / 60.0,
            vec![egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
        ),
        |ctx| draw(ctx, workspace),
    );
    let threshold = start + (end - start).normalized() * 7.0;
    let _ = ctx.run(
        input(3.0 / 60.0, vec![egui::Event::PointerMoved(threshold)]),
        |ctx| draw(ctx, workspace),
    );
    let _ = ctx.run(
        input(4.0 / 60.0, vec![egui::Event::PointerMoved(end)]),
        |ctx| draw(ctx, workspace),
    );
    let _ = ctx.run(
        input(
            5.0 / 60.0,
            vec![egui::Event::PointerButton {
                pos: end,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        ),
        |ctx| draw(ctx, workspace),
    );
}

fn run_real_egui_orthographic_click(workspace: &mut EditorWorkspace, world: [f32; 2]) {
    let ctx = egui::Context::default();
    let mut fonts = egui::FontDefinitions::default();
    let proportional = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .expect("default proportional font family");
    fonts
        .families
        .insert(egui::FontFamily::Name("lucide".into()), proportional);
    ctx.set_fonts(fonts);
    let viewport_texture = ctx.load_texture(
        "orthographic-input-viewport",
        egui::ColorImage::new([1, 1], egui::Color32::BLACK),
        egui::TextureOptions::NEAREST,
    );
    let viewport = EditorViewport3dPresentation::edit(viewport_texture.id(), Vec::new());
    let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 900.0));
    let input = |time, events| egui::RawInput {
        screen_rect: Some(screen),
        time: Some(time),
        events,
        ..egui::RawInput::default()
    };
    let draw = |ctx: &egui::Context, workspace: &mut EditorWorkspace| {
        workspace.draw(ctx, viewport.clone(), EditorPlaytestStatus::Idle);
    };
    let _ = ctx.run(input(0.0, vec![]), |ctx| draw(ctx, workspace));
    let rect = workspace.last_orthographic_viewport_rect;
    assert!(
        rect.is_positive(),
        "real orthographic viewport was laid out"
    );
    let pointer = crate::viewport2d::ViewportTransform::from_focus(
        rect,
        workspace
            .orthographic_view
            .project_f32(workspace.orthographic_focus),
        workspace.viewport_zoom,
    )
    .world_to_screen(world);
    assert!(rect.contains(pointer));
    let _ = ctx.run(
        input(
            1.0 / 60.0,
            vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        ),
        |ctx| draw(ctx, workspace),
    );
    let _ = ctx.run(
        input(
            2.0 / 60.0,
            vec![egui::Event::PointerButton {
                pos: pointer,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        ),
        |ctx| draw(ctx, workspace),
    );
}

fn run_real_egui_orthographic_drag(
    workspace: &mut EditorWorkspace,
    start_world: [f32; 2],
    end_world: [f32; 2],
    modifiers: egui::Modifiers,
) {
    let ctx = egui::Context::default();
    let mut fonts = egui::FontDefinitions::default();
    let proportional = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .expect("default proportional font family");
    fonts
        .families
        .insert(egui::FontFamily::Name("lucide".into()), proportional);
    ctx.set_fonts(fonts);
    let viewport_texture = ctx.load_texture(
        "orthographic-drag-viewport",
        egui::ColorImage::new([1, 1], egui::Color32::BLACK),
        egui::TextureOptions::NEAREST,
    );
    let viewport = EditorViewport3dPresentation::edit(viewport_texture.id(), Vec::new());
    let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 900.0));
    let input = |time, events| egui::RawInput {
        screen_rect: Some(screen),
        time: Some(time),
        modifiers,
        events,
        ..egui::RawInput::default()
    };
    let draw = |ctx: &egui::Context, workspace: &mut EditorWorkspace| {
        workspace.draw_viewport(ctx, viewport.clone(), EditorPlaytestStatus::Idle);
    };
    let _ = ctx.run(input(0.0, vec![]), |ctx| draw(ctx, workspace));
    let rect = workspace.last_orthographic_viewport_rect;
    let transform = crate::viewport2d::ViewportTransform::from_focus(
        rect,
        workspace
            .orthographic_view
            .project_f32(workspace.orthographic_focus),
        workspace.viewport_zoom,
    );
    let clamp_to_view = |point: Pos2| {
        Pos2::new(
            point.x.clamp(rect.left() + 3.0, rect.right() - 3.0),
            point.y.clamp(rect.top() + 3.0, rect.bottom() - 3.0),
        )
    };
    let start = clamp_to_view(transform.world_to_screen(start_world));
    let end = clamp_to_view(transform.world_to_screen(end_world));
    let _ = ctx.run(
        input(1.0 / 60.0, vec![egui::Event::PointerMoved(start)]),
        |ctx| draw(ctx, workspace),
    );
    let hover_response = workspace.last_orthographic_response.unwrap();
    assert!(hover_response.1, "viewport must be hovered before press");
    let _ = ctx.run(
        input(
            2.0 / 60.0,
            vec![egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers,
            }],
        ),
        |ctx| draw(ctx, workspace),
    );
    let press_response = workspace.last_orthographic_response.unwrap();
    assert_eq!(
        press_response.0, hover_response.0,
        "stable viewport response id"
    );
    let _ = ctx.run(
        input(
            3.0 / 60.0,
            vec![egui::Event::PointerMoved(start.lerp(end, 0.35))],
        ),
        |ctx| draw(ctx, workspace),
    );
    assert!(ctx.input(|input| input.pointer.primary_down()));
    assert!(
        workspace.interaction.box_select_2d().is_some(),
        "raw drag did not start: pointer={:?}->{:?}, initial_rect={:?}, current_rect={:?}, response={:?}, tool={:?}",
        start,
        end,
        rect,
        workspace.last_orthographic_viewport_rect,
        workspace.last_orthographic_response,
        workspace.active_tool
    );
    let _ = ctx.run(
        input(4.0 / 60.0, vec![egui::Event::PointerMoved(end)]),
        |ctx| draw(ctx, workspace),
    );
    let _ = ctx.run(
        input(
            5.0 / 60.0,
            vec![egui::Event::PointerButton {
                pos: end,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers,
            }],
        ),
        |ctx| draw(ctx, workspace),
    );
}

fn text_shape_center(shapes: &[egui::epaint::ClippedShape], label: &str) -> Option<Pos2> {
    fn find(shape: &egui::Shape, label: &str) -> Option<Pos2> {
        match shape {
            egui::Shape::Text(text) if text.galley.text() == label => {
                Some(text.pos + text.galley.rect.center().to_vec2())
            }
            egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| find(shape, label)),
            _ => None,
        }
    }
    shapes.iter().find_map(|shape| find(&shape.shape, label))
}

/// Every rendered widget whose galley text equals `label`. Tests that click
/// a label by name use this to prove the label is unambiguous first: the
/// Inspector renders several same-named buttons, and `text_shape_center`
/// silently returns whichever one is painted first.
fn text_shape_centers(shapes: &[egui::epaint::ClippedShape], label: &str) -> Vec<Pos2> {
    fn collect(shape: &egui::Shape, label: &str, found: &mut Vec<Pos2>) {
        match shape {
            egui::Shape::Text(text) if text.galley.text() == label => {
                found.push(text.pos + text.galley.rect.center().to_vec2());
            }
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    collect(shape, label, found);
                }
            }
            _ => {}
        }
    }
    let mut found = Vec::new();
    for shape in shapes {
        collect(&shape.shape, label, &mut found);
    }
    found
}

/// Drive an always-visible brush mode button by finding its rendered label,
/// then issuing real egui pointer events at that label's screen position.
fn click_visible_brush_mode(workspace: &mut EditorWorkspace, mode: BrushEditMode) {
    let ctx = egui::Context::default();
    let mut fonts = egui::FontDefinitions::default();
    let proportional = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .unwrap();
    fonts
        .families
        .insert(egui::FontFamily::Name("lucide".into()), proportional);
    ctx.set_fonts(fonts);
    let texture = ctx.load_texture(
        "visible-brush-mode-controls",
        egui::ColorImage::new([1, 1], egui::Color32::BLACK),
        egui::TextureOptions::NEAREST,
    );
    let viewport = EditorViewport3dPresentation::edit(texture.id(), Vec::new());
    let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1800.0, 900.0));
    let input = |time, events| egui::RawInput {
        screen_rect: Some(screen),
        time: Some(time),
        events,
        ..egui::RawInput::default()
    };
    let output = ctx.run(input(0.0, vec![]), |ctx| {
        workspace.draw(ctx, viewport.clone(), EditorPlaytestStatus::Idle)
    });
    assert!(
        text_shape_center(&output.shapes, "Brush Transform").is_some(),
        "selected brush did not expose the Inspector transform section"
    );
    for label in ["Move", "Resize", "Edge", "Vertex"] {
        assert!(
            text_shape_center(&output.shapes, label).is_some(),
            "visible brush toolbar omitted {label:?}"
        );
    }
    let point = text_shape_center(&output.shapes, mode.label()).unwrap();
    let _ = ctx.run(
        input(
            1.0 / 60.0,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        ),
        |ctx| workspace.draw(ctx, viewport.clone(), EditorPlaytestStatus::Idle),
    );
    let _ = ctx.run(
        input(
            2.0 / 60.0,
            vec![egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        ),
        |ctx| workspace.draw(ctx, viewport.clone(), EditorPlaytestStatus::Idle),
    );
    assert_eq!(workspace.brush_edit_mode, mode);
}

fn run_real_egui_orthographic_brush_drag(
    workspace: &mut EditorWorkspace,
    start_world: [f32; 2],
    end_world: [f32; 2],
) {
    let ctx = egui::Context::default();
    let mut fonts = egui::FontDefinitions::default();
    let proportional = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .unwrap();
    fonts
        .families
        .insert(egui::FontFamily::Name("lucide".into()), proportional);
    ctx.set_fonts(fonts);
    let texture = ctx.load_texture(
        "orthographic-brush-transform",
        egui::ColorImage::new([1, 1], egui::Color32::BLACK),
        egui::TextureOptions::NEAREST,
    );
    let viewport = EditorViewport3dPresentation::edit(texture.id(), Vec::new());
    let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 900.0));
    let input = |time, events| egui::RawInput {
        screen_rect: Some(screen),
        time: Some(time),
        events,
        ..egui::RawInput::default()
    };
    let draw = |ctx: &egui::Context, workspace: &mut EditorWorkspace| {
        workspace.draw_viewport(ctx, viewport.clone(), EditorPlaytestStatus::Idle);
    };
    let _ = ctx.run(input(0.0, vec![]), |ctx| draw(ctx, workspace));
    let rect = workspace.last_orthographic_viewport_rect;
    let transform = crate::viewport2d::ViewportTransform::from_focus(
        rect,
        workspace
            .orthographic_view
            .project_f32(workspace.orthographic_focus),
        workspace.viewport_zoom,
    );
    let start = transform.world_to_screen(start_world);
    let end = transform.world_to_screen(end_world);
    assert!(rect.contains(start) && rect.contains(end));
    let _ = ctx.run(
        input(1.0 / 60.0, vec![egui::Event::PointerMoved(start)]),
        |ctx| draw(ctx, workspace),
    );
    let _ = ctx.run(
        input(
            2.0 / 60.0,
            vec![egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
        ),
        |ctx| draw(ctx, workspace),
    );
    let threshold = start + (end - start).normalized() * 7.0;
    let _ = ctx.run(
        input(3.0 / 60.0, vec![egui::Event::PointerMoved(threshold)]),
        |ctx| draw(ctx, workspace),
    );
    let _ = ctx.run(
        input(4.0 / 60.0, vec![egui::Event::PointerMoved(end)]),
        |ctx| draw(ctx, workspace),
    );
    let _ = ctx.run(
        input(
            5.0 / 60.0,
            vec![egui::Event::PointerButton {
                pos: end,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        ),
        |ctx| draw(ctx, workspace),
    );
}

#[test]
fn visible_brush_modes_drive_plain_drag_move_and_resize_in_every_2d_view() {
    for view in [
        OrthographicView::Top,
        OrthographicView::Front,
        OrthographicView::Side,
    ] {
        for mode in BrushEditMode::ALL {
            let base = psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]);
            let mut project = ProjectDocument::new("visible brush transform controls");
            project.active_scene_mut().brushes.push(base.clone());
            let mut workspace = EditorWorkspace::with_project(test_temp_dir("mode-drag"), project);
            workspace.active_workspace = WorkspaceView::Room;
            workspace.active_tool = ViewTool::Brush;
            workspace.view_2d = true;
            workspace.orthographic_view = view;
            workspace.orthographic_focus = [64.0; 3];
            workspace.viewport_zoom = 2.0;
            workspace.snap_units = 16;
            workspace.replace_brush_selection(0, None);

            click_visible_brush_mode(&mut workspace, mode);
            let (start, end) = match mode {
                BrushEditMode::Move => ([64.0, 64.0], [96.0, 64.0]),
                BrushEditMode::Face | BrushEditMode::Edge => ([128.0, 64.0], [160.0, 64.0]),
                BrushEditMode::Vertex => ([128.0, 128.0], [160.0, 160.0]),
            };
            run_real_egui_orthographic_brush_drag(&mut workspace, start, end);

            assert_ne!(
                workspace.project.active_scene().brushes[0],
                base,
                "{view:?} {mode:?} plain drag did not transform the brush"
            );
            assert!(
                workspace.project.active_scene().brushes[0]
                    .solve()
                    .is_valid(),
                "{view:?} {mode:?} produced an invalid brush"
            );
            assert!(workspace.is_dirty(), "{view:?} {mode:?}");
            workspace.do_undo();
            assert_eq!(workspace.project.active_scene().brushes[0], base);
        }
    }
}

#[test]
fn select_tool_selected_brush_uses_visible_move_and_resize_via_real_egui() {
    for mode in [BrushEditMode::Move, BrushEditMode::Face] {
        let base = psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]);
        let mut project = ProjectDocument::new("select tool direct brush transform");
        project.active_scene_mut().brushes.push(base.clone());
        let mut workspace = EditorWorkspace::with_project(test_temp_dir("select-edit"), project);
        workspace.active_workspace = WorkspaceView::Room;
        workspace.active_tool = ViewTool::Select;
        workspace.view_2d = true;
        workspace.orthographic_view = OrthographicView::Top;
        workspace.orthographic_focus = [64.0; 3];
        workspace.viewport_zoom = 2.0;
        workspace.snap_units = 16;
        workspace.replace_brush_selection(0, None);

        click_visible_brush_mode(&mut workspace, mode);
        let (start, end) = match mode {
            BrushEditMode::Move => ([64.0, 64.0], [96.0, 64.0]),
            BrushEditMode::Face => ([128.0, 64.0], [160.0, 64.0]),
            _ => unreachable!(),
        };
        run_real_egui_orthographic_brush_drag(&mut workspace, start, end);
        assert_ne!(workspace.project.active_scene().brushes[0], base);
        assert!(workspace.is_dirty());
        workspace.do_undo();
        assert_eq!(workspace.project.active_scene().brushes[0], base);
    }
}

#[test]
fn numeric_origin_moves_a_multi_selection_and_survives_save_reload() {
    let dir = test_temp_dir("numeric-group-persistence");
    std::fs::create_dir_all(&dir).unwrap();
    let mut project = ProjectDocument::new("Numeric Group Persistence");
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [0, 0, 0],
            [128, 128, 128],
        ));
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [256, 0, 0],
            [384, 128, 128],
        ));
    project.save_to_path(dir.join("project.ron")).unwrap();
    let mut workspace = EditorWorkspace::open_directory(&dir).unwrap();
    workspace.active_tool = ViewTool::Select;
    workspace.replace_brush_selection(1, None);
    workspace.toggle_brush_selection(0);
    workspace.push_undo();

    assert!(workspace.set_selected_brush_origin([32, 16, -16]));
    assert_eq!(workspace.selected_brush_origin(), Some([32, 16, -16]));
    let secondary = workspace.project.active_scene().brushes[1].solve();
    assert_eq!(secondary.min, [288.0, 16.0, -16.0]);
    assert!(workspace.is_dirty());

    workspace.do_undo();
    assert_eq!(
        workspace.project.active_scene().brushes[0].solve().min,
        [0.0; 3]
    );
    workspace.do_redo();
    assert_eq!(
        workspace.project.active_scene().brushes[0].solve().min,
        [32.0, 16.0, -16.0]
    );
    workspace.save().unwrap();
    assert!(!workspace.is_dirty());

    let reopened = EditorWorkspace::open_directory(&dir).unwrap();
    assert_eq!(
        reopened.project.active_scene().brushes[0].solve().min,
        [32.0, 16.0, -16.0]
    );
    assert_eq!(
        reopened.project.active_scene().brushes[1].solve().min,
        [288.0, 16.0, -16.0]
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn coincident_brushes_cycle_individually_in_every_2d_view_via_real_egui() {
    for view in [
        OrthographicView::Top,
        OrthographicView::Front,
        OrthographicView::Side,
    ] {
        let brush = psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]);
        let mut project = ProjectDocument::new("coincident brush cycle");
        project.active_scene_mut().brushes.push(brush.clone());
        project.active_scene_mut().brushes.push(brush);
        let mut workspace = EditorWorkspace::with_project(test_temp_dir("overlap-cycle"), project);
        workspace.active_workspace = WorkspaceView::Room;
        workspace.active_tool = ViewTool::Select;
        workspace.view_2d = true;
        workspace.orthographic_view = view;
        workspace.orthographic_focus = [64.0; 3];
        workspace.viewport_zoom = 2.0;

        run_real_egui_orthographic_click(&mut workspace, [64.0, 64.0]);
        assert_eq!(workspace.selected_brush, Some(0), "{view:?}");
        run_real_egui_orthographic_click(&mut workspace, [64.0, 64.0]);
        assert_eq!(workspace.selected_brush, Some(1), "{view:?}");
        run_real_egui_orthographic_click(&mut workspace, [64.0, 64.0]);
        assert_eq!(workspace.selected_brush, Some(0), "{view:?}");
    }
}

#[test]
fn subpixel_brush_click_and_nearby_marquee_work_in_every_2d_view_via_real_egui() {
    for view in [
        OrthographicView::Top,
        OrthographicView::Front,
        OrthographicView::Side,
    ] {
        let mut project = ProjectDocument::new("subpixel brush selection");
        project
            .active_scene_mut()
            .brushes
            .push(psxed_project::brush::Brush::cuboid(
                [0, 0, 0],
                [128, 128, 128],
            ));
        let mut workspace = EditorWorkspace::with_project(test_temp_dir("subpixel"), project);
        workspace.active_workspace = WorkspaceView::Room;
        workspace.active_tool = ViewTool::Select;
        workspace.view_2d = true;
        workspace.orthographic_view = view;
        workspace.orthographic_focus = [64.0; 3];
        workspace.viewport_zoom = 0.01;

        // Four screen pixels beyond the brush's right edge: outside exact
        // geometry, but inside the fixed 8 px selection tolerance.
        let nearby_x = 128.0 + 4.0 / workspace.viewport_zoom;
        run_real_egui_orthographic_click(&mut workspace, [nearby_x, 64.0]);
        assert_eq!(workspace.selected_brush, Some(0), "{view:?} click");

        workspace.clear_brush_selection();
        // A narrow vertical marquee also sits four pixels beyond the tiny
        // outline; expanded projected bounds must still intersect it.
        run_real_egui_orthographic_drag(
            &mut workspace,
            [nearby_x, -936.0],
            [nearby_x, 1064.0],
            egui::Modifiers::NONE,
        );
        assert_eq!(workspace.selected_brush_set(), vec![0], "{view:?} marquee");
    }
}

#[test]
fn coincident_brushes_cycle_individually_in_3d_via_real_egui() {
    let mut harness = ViewportHarness::floored_room("3d coincident brush cycle", 1);
    let brush = psxed_project::brush::Brush::cuboid([320, 0, 320], [704, 640, 704]);
    harness
        .workspace
        .project
        .active_scene_mut()
        .brushes
        .push(brush.clone());
    harness
        .workspace
        .project
        .active_scene_mut()
        .brushes
        .push(brush);
    harness.frame([512.0, 256.0, 512.0], 1800.0);
    harness.workspace.active_tool = ViewTool::Select;
    let camera = harness.workspace.viewport_3d_camera();
    let (nx, ny) = camera
        .normalized_panel_point_for_world([512.0, 640.0, 512.0])
        .unwrap();
    let rect = Rect::from_center_size(Pos2::new(400.0, 300.0), Vec2::new(778.6667, 584.0));
    let click = Pos2::new(
        rect.center().x + nx * rect.width() * 0.5,
        rect.center().y + ny * rect.height() * 0.5,
    );
    let context = egui::Context::default();
    let texture = context.load_texture(
        "coincident-3d",
        egui::ColorImage::new([1, 1], egui::Color32::BLACK),
        egui::TextureOptions::NEAREST,
    );
    let presentation = EditorViewport3dPresentation::edit(texture.id(), Vec::new());

    run_real_egui_viewport_click(&mut harness.workspace, click, presentation.clone());
    assert_eq!(harness.workspace.selected_brush, Some(0));
    run_real_egui_viewport_click(&mut harness.workspace, click, presentation.clone());
    assert_eq!(harness.workspace.selected_brush, Some(1));
    run_real_egui_viewport_click(&mut harness.workspace, click, presentation);
    assert_eq!(harness.workspace.selected_brush, Some(0));
}

#[test]
fn brush_and_select_tools_plain_drag_move_selected_brush_in_3d_via_real_egui() {
    for tool in [ViewTool::Brush, ViewTool::Select] {
        let mut harness = ViewportHarness::floored_room("3d plain brush move", 1);
        let base = psxed_project::brush::Brush::cuboid([320, 0, 320], [704, 640, 704]);
        harness
            .workspace
            .project
            .active_scene_mut()
            .brushes
            .push(base.clone());
        harness.frame([512.0, 256.0, 512.0], 1800.0);
        harness.workspace.active_tool = tool;
        harness.workspace.brush_edit_mode = BrushEditMode::Move;
        harness.workspace.snap_units = 16;
        harness.workspace.replace_brush_selection(0, None);
        let camera = harness.workspace.viewport_3d_camera();
        let (nx, ny) = camera
            .normalized_panel_point_for_world([512.0, 640.0, 512.0])
            .unwrap();
        let rect = Rect::from_center_size(Pos2::new(400.0, 300.0), Vec2::new(778.6667, 584.0));
        let start = Pos2::new(
            rect.center().x + nx * rect.width() * 0.5,
            rect.center().y + ny * rect.height() * 0.5,
        );
        run_real_egui_viewport_plain_drag(
            &mut harness.workspace,
            start,
            start + Vec2::new(64.0, 0.0),
        );
        assert_ne!(
            harness.workspace.project.active_scene().brushes[0],
            base,
            "{tool:?}"
        );
        assert!(harness.workspace.is_dirty(), "{tool:?}");
        harness.workspace.do_undo();
        assert_eq!(harness.workspace.project.active_scene().brushes[0], base);
    }
}

#[test]
fn distant_subpixel_brush_has_3d_click_tolerance_via_real_egui() {
    let mut project = ProjectDocument::new("distant 3d brush selection");
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [-32, 0, -32],
            [32, 64, 32],
        ));
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("distant-3d"), project);
    workspace.active_tool = ViewTool::Select;
    workspace.camera_rig.mode = ViewportCameraMode::Free;
    workspace.camera_rig.free_initialized = true;
    workspace.camera_rig.free_position = [0, 50_000, -80_000];
    let (yaw, pitch) = camera_angles_to_look_at([0, 50_000, -80_000], [0, 32, 0]).unwrap();
    workspace.camera_rig.free_yaw = yaw;
    workspace.camera_rig.free_pitch = pitch;
    let rect = Rect::from_center_size(Pos2::new(400.0, 300.0), Vec2::new(778.6667, 584.0));
    let center = workspace
        .project_brush_point_3d(rect, [0.0, 32.0, 0.0])
        .unwrap();
    let click = center + Vec2::new(5.0, 0.0);
    let context = egui::Context::default();
    let texture = context.load_texture(
        "distant-3d-click",
        egui::ColorImage::new([1, 1], egui::Color32::BLACK),
        egui::TextureOptions::NEAREST,
    );
    run_real_egui_viewport_click(
        &mut workspace,
        click,
        EditorViewport3dPresentation::edit(texture.id(), Vec::new()),
    );
    assert_eq!(workspace.selected_brush, Some(0));
}

#[test]
fn distant_subpixel_brush_has_3d_marquee_tolerance_via_real_egui() {
    let mut project = ProjectDocument::new("distant 3d brush marquee");
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [-32, 0, -32],
            [32, 64, 32],
        ));
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("distant-3d-box"), project);
    workspace.active_tool = ViewTool::Select;
    workspace.camera_rig.mode = ViewportCameraMode::Free;
    workspace.camera_rig.free_initialized = true;
    workspace.camera_rig.free_position = [0, 50_000, -80_000];
    let (yaw, pitch) = camera_angles_to_look_at([0, 50_000, -80_000], [0, 32, 0]).unwrap();
    workspace.camera_rig.free_yaw = yaw;
    workspace.camera_rig.free_pitch = pitch;
    let rect = Rect::from_center_size(Pos2::new(400.0, 300.0), Vec2::new(778.6667, 584.0));
    let center = workspace
        .project_brush_point_3d(rect, [0.0, 32.0, 0.0])
        .unwrap();
    // Start outside the pick tolerance so Select begins a box gesture; the
    // resulting narrow marquee passes four pixels beside the sub-pixel brush.
    run_real_egui_viewport_plain_drag(
        &mut workspace,
        center + Vec2::new(12.0, -12.0),
        center + Vec2::new(4.0, 12.0),
    );
    assert_eq!(workspace.selected_brush_set(), vec![0]);
}

#[test]
fn bsp_marquee_multi_selects_brushes_in_every_orthographic_view_via_real_egui() {
    for view in [
        OrthographicView::Top,
        OrthographicView::Front,
        OrthographicView::Side,
    ] {
        let mut project = ProjectDocument::new("real egui BSP marquee");
        project
            .active_scene_mut()
            .brushes
            .push(psxed_project::brush::Brush::cuboid(
                [0, 0, 0],
                [128, 128, 128],
            ));
        project
            .active_scene_mut()
            .brushes
            .push(psxed_project::brush::Brush::cuboid(
                [256, 256, 256],
                [384, 384, 384],
            ));
        project
            .active_scene_mut()
            .brushes
            .push(psxed_project::brush::Brush::cuboid(
                [600, 600, 600],
                [728, 728, 728],
            ));
        let mut workspace = EditorWorkspace::with_project(test_temp_dir("marquee"), project);
        workspace.active_workspace = WorkspaceView::Room;
        workspace.view_2d = true;
        workspace.orthographic_view = view;
        workspace.active_tool = ViewTool::Select;
        workspace.frame_viewport();

        run_real_egui_orthographic_drag(
            &mut workspace,
            [-32.0, -32.0],
            [416.0, 416.0],
            egui::Modifiers::NONE,
        );

        assert_eq!(
            workspace.selected_brush_set(),
            vec![0, 1],
            "{view:?}: status={}, interaction={:?}",
            workspace.status,
            workspace.interaction
        );
        assert_eq!(workspace.status_text(), "Selected 2 brushes", "{view:?}");
        assert!(workspace.selection.selected_sectors.is_empty());

        workspace.replace_brush_selection(2, None);
        run_real_egui_orthographic_drag(
            &mut workspace,
            [-32.0, -32.0],
            [416.0, 416.0],
            egui::Modifiers {
                shift: true,
                ..egui::Modifiers::NONE
            },
        );
        assert_eq!(workspace.selected_brush_set(), vec![0, 1, 2], "{view:?}");
        assert_eq!(workspace.selected_brush, Some(2), "{view:?}");
    }
}

#[test]
fn tracked_bsp_starter_top_view_emits_headless_brush_outline_shapes() {
    let fixture_dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../projects/brush-first-playable");
    let project = ProjectDocument::load_from_path(fixture_dir.join("project.ron"))
        .expect("tracked BSP starter loads");
    let expected_segments = project
        .active_scene()
        .brushes
        .iter()
        .map(|brush| {
            brush
                .solve()
                .polygons
                .iter()
                .flatten()
                .map(|polygon| polygon.verts.len())
                .sum::<usize>()
        })
        .sum::<usize>();
    assert!(
        expected_segments > 0,
        "tracked fixture must contain solved brushes"
    );

    let mut workspace = EditorWorkspace::with_project(fixture_dir, project);
    workspace.active_workspace = WorkspaceView::Room;
    workspace.view_2d = true;
    workspace.orthographic_view = OrthographicView::Top;
    workspace.active_tool = ViewTool::Select;
    workspace.frame_viewport();

    let ctx = egui::Context::default();
    let mut fonts = egui::FontDefinitions::default();
    let proportional = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .expect("default proportional font family");
    fonts
        .families
        .insert(egui::FontFamily::Name("lucide".into()), proportional);
    ctx.set_fonts(fonts);
    let texture = ctx.load_texture(
        "tracked-bsp-top-shapes",
        egui::ColorImage::new([1, 1], egui::Color32::BLACK),
        egui::TextureOptions::NEAREST,
    );
    let viewport = EditorViewport3dPresentation::edit(texture.id(), Vec::new());
    let output = ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 900.0))),
            ..egui::RawInput::default()
        },
        |ctx| workspace.draw_viewport(ctx, viewport.clone(), EditorPlaytestStatus::Idle),
    );
    let rect = workspace.last_orthographic_viewport_rect;
    let outline_segments = output
        .shapes
        .iter()
        .filter(|clipped| clipped.clip_rect.intersects(rect))
        .filter(|clipped| match &clipped.shape {
            egui::Shape::LineSegment { points, stroke } => {
                stroke.color == EDITOR_OUTLINE_ACCENT
                    && points.iter().all(|point| rect.contains(*point))
            }
            _ => false,
        })
        .count();
    assert!(
        outline_segments >= expected_segments,
        "Top viewport emitted {outline_segments} outline segments for {expected_segments} solved brush edges"
    );
}

fn handle_test_workspace(brush: psxed_project::brush::Brush) -> (EditorWorkspace, Rect) {
    let mut project = ProjectDocument::new("3D brush handles");
    project.active_scene_mut().brushes.push(brush);
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("3d-handles"), project);
    workspace.active_tool = ViewTool::Brush;
    workspace.replace_brush_selection(0, None);
    workspace.snap_units = 16;
    workspace.camera_rig.mode = ViewportCameraMode::Free;
    workspace.camera_rig.free_initialized = true;
    workspace.camera_rig.free_position = [500, 420, -700];
    let (yaw, pitch) = camera_angles_to_look_at([500, 420, -700], [64, 64, 64]).unwrap();
    workspace.camera_rig.free_yaw = yaw;
    workspace.camera_rig.free_pitch = pitch;
    (
        workspace,
        Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0)),
    )
}

#[test]
fn arbitrary_plane_face_handle_drags_along_its_normal_and_undoes_once() {
    let mut wedge = psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]);
    wedge.faces[5] =
        psxed_project::brush::BrushFace::from_points([[0, 128, 0], [0, 128, 128], [128, 0, 128]]);
    assert!(wedge.solve().is_valid());
    let base = wedge.clone();
    let (mut workspace, rect) = handle_test_workspace(wedge);
    workspace.brush_edit_mode = BrushEditMode::Face;
    workspace.selection_mode = SelectionMode::Face;
    let (center, _) = EditorWorkspace::face_center_and_normal(&base, 5).unwrap();
    let pointer = workspace.project_brush_point_3d(rect, center).unwrap();
    assert!(matches!(
        workspace.pick_brush_handle_3d(rect, pointer),
        Some((0, BrushHandle3d::Face { face: 5, .. }))
    ));

    let mut frame = ToolFrame3d {
        rect,
        pointer_interact: Some(pointer),
        pointer_hover: Some(pointer),
        modifiers: egui::Modifiers::NONE,
        pointer_target: None,
        hover_room: None,
        drag_delta_y: 0.0,
    };
    tool_impl_3d(ViewTool::Brush).primary_pressed(&mut workspace, &frame);
    let drag = workspace.brush_extrude.clone().expect("face handle drag");
    assert!(drag.normal_3d.is_some());
    let end = pointer + drag.screen_direction * 24.0;
    frame.pointer_interact = Some(end);
    frame.pointer_hover = Some(end);
    tool_impl_3d(ViewTool::Brush).primary_dragged(&mut workspace, &frame);
    let applied = workspace.brush_extrude.as_ref().unwrap().applied;
    assert_ne!(
        applied, [0; 3],
        "screen={:?} units_per_pixel={} normal={:?}",
        drag.screen_direction, drag.units_per_pixel, drag.normal_3d
    );
    assert_ne!(applied[0], 0, "slope movement must include X");
    assert_ne!(applied[1], 0, "slope movement must include Y");
    let moved = workspace.project.active_scene().brushes[0].clone();
    assert!(moved.solve().is_valid());
    assert_eq!(
        psxed_project::brush::Plane::from_points(moved.faces[5].points)
            .unwrap()
            .normal,
        psxed_project::brush::Plane::from_points(base.faces[5].points)
            .unwrap()
            .normal
    );
    tool_impl_3d(ViewTool::Brush).primary_released(&mut workspace, &frame);
    assert!(workspace.is_dirty());
    workspace.do_undo();
    assert_eq!(workspace.project.active_scene().brushes[0], base);
}

#[test]
fn vertex_and_edge_3d_handles_start_camera_plane_edits_and_keep_brush_valid() {
    for mode in [SelectionMode::Vertex, SelectionMode::Edge] {
        let brush = psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]);
        let (mut workspace, rect) = handle_test_workspace(brush.clone());
        workspace.brush_edit_mode = match mode {
            SelectionMode::Vertex => BrushEditMode::Vertex,
            SelectionMode::Edge => BrushEditMode::Edge,
            SelectionMode::Face => unreachable!(),
        };
        workspace.selection_mode = mode;
        let solved = brush.solve();
        let vertex = solved.polygons.iter().flatten().next().unwrap().verts[0];
        let anchor = match mode {
            SelectionMode::Vertex => vertex,
            SelectionMode::Edge => {
                let polygon = solved.polygons.iter().flatten().next().unwrap();
                let a = polygon.verts[0];
                let b = polygon.verts[1];
                [
                    (a[0] + b[0]) * 0.5,
                    (a[1] + b[1]) * 0.5,
                    (a[2] + b[2]) * 0.5,
                ]
            }
            SelectionMode::Face => unreachable!(),
        };
        let pointer = workspace.project_brush_point_3d(rect, anchor).unwrap();
        let mut frame = ToolFrame3d {
            rect,
            pointer_interact: Some(pointer),
            pointer_hover: Some(pointer),
            modifiers: egui::Modifiers::NONE,
            pointer_target: None,
            hover_room: None,
            drag_delta_y: 0.0,
        };
        tool_impl_3d(ViewTool::Brush).primary_pressed(&mut workspace, &frame);
        assert!(workspace.brush_vertex_drag.is_some(), "{mode:?}");
        let end = pointer + Vec2::new(36.0, -20.0);
        frame.pointer_interact = Some(end);
        frame.pointer_hover = Some(end);
        tool_impl_3d(ViewTool::Brush).primary_dragged(&mut workspace, &frame);
        let drag = workspace.brush_vertex_drag.as_ref().unwrap();
        assert_ne!(drag.applied, [0; 3], "{mode:?}");
        assert!(workspace.project.active_scene().brushes[0]
            .solve()
            .is_valid());
        tool_impl_3d(ViewTool::Brush).primary_released(&mut workspace, &frame);
        workspace.do_undo();
        assert_eq!(workspace.project.active_scene().brushes[0], brush);
    }
}

#[test]
fn vertex_3d_handle_drag_runs_through_real_egui_raw_input_and_commits() {
    let brush = psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]);
    let (mut workspace, _) = handle_test_workspace(brush.clone());
    workspace.brush_edit_mode = BrushEditMode::Vertex;
    workspace.selection_mode = SelectionMode::Vertex;
    let viewport = Rect::from_center_size(Pos2::new(400.0, 300.0), Vec2::new(778.6667, 584.0));
    let vertex = brush
        .solve()
        .polygons
        .iter()
        .flatten()
        .next()
        .unwrap()
        .verts[0];
    let start = workspace
        .project_brush_point_3d(viewport, vertex)
        .expect("vertex projects into the 3D viewport");

    run_real_egui_viewport_drag(&mut workspace, start, start + Vec2::new(42.0, -24.0));

    assert!(workspace.is_dirty());
    assert_ne!(workspace.project.active_scene().brushes[0], brush);
    assert!(workspace.project.active_scene().brushes[0]
        .solve()
        .is_valid());
    workspace.do_undo();
    assert_eq!(workspace.project.active_scene().brushes[0], brush);
}

#[test]
fn top_view_select_and_brush_tools_pick_the_specific_topmost_brush_via_real_egui() {
    for tool in [ViewTool::Select, ViewTool::Brush] {
        let mut project = ProjectDocument::new("real egui Top BSP selection");
        project
            .active_scene_mut()
            .brushes
            .push(psxed_project::brush::Brush::cuboid(
                [0, 0, 0],
                [1024, 256, 768],
            ));
        project
            .active_scene_mut()
            .brushes
            .push(psxed_project::brush::Brush::cuboid(
                [320, 320, 256],
                [704, 640, 512],
            ));
        let old_node = project.active_scene_mut().add_node(
            NodeId::ROOT,
            "Old selection",
            NodeKind::PointLight {
                color: [255; 3],
                intensity: 1.0,
                radius: 1.0,
            },
        );
        project
            .active_scene_mut()
            .node_mut(old_node)
            .unwrap()
            .transform
            .translation = [8192.0, 8192.0, 8192.0];
        let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
        workspace.active_workspace = WorkspaceView::Room;
        workspace.active_tool = tool;
        workspace.show_room_orthographic();
        // Preserve the correct all-brush framing, then seed a stale node
        // selection so the actual pointer dispatch must clear it.
        workspace.replace_node_selection(old_node);

        run_real_egui_orthographic_click(&mut workspace, [512.0, 384.0]);

        assert_eq!(workspace.selected_brush, Some(1), "{tool:?}");
        assert_eq!(workspace.selected_brush_face, Some(5), "{tool:?}");
        assert!(workspace.brush_is_selected(1));
        assert_eq!(workspace.selection.selected_node, NodeId::ROOT);
        assert!(workspace.selection.selected_nodes.is_empty());
        assert!(workspace.selection.selected_primitives.is_empty());
    }
}

#[test]
fn bsp_brush_click_selection_runs_through_real_egui_response_dispatch() {
    for tool in [ViewTool::Brush, ViewTool::Select] {
        let mut harness = ViewportHarness::floored_room("real_egui_bsp_click", 1);
        harness.workspace.project.active_scene_mut().brushes.push(
            psxed_project::brush::Brush::cuboid([256, 0, 256], [768, 512, 768]),
        );
        harness.workspace.project.active_scene_mut().brushes.push(
            psxed_project::brush::Brush::cuboid([320, 320, 320], [704, 640, 704]),
        );
        harness.frame([512.0, 256.0, 512.0], 1800.0);
        harness.workspace.active_tool = tool;
        harness.workspace.clear_brush_selection();

        let camera = harness.workspace.viewport_3d_camera();
        let (nx, ny) = camera
            .normalized_panel_point_for_world([512.0, 640.0, 512.0])
            .expect("brush top projects");
        // CentralPanel leaves an 8 px margin. The 4:3 preview is centred in
        // its remaining 784x584 body, producing this deterministic rect.
        let rect = Rect::from_center_size(Pos2::new(400.0, 300.0), Vec2::new(778.6667, 584.0));
        let click = Pos2::new(
            rect.center().x + nx * rect.width() * 0.5,
            rect.center().y + ny * rect.height() * 0.5,
        );
        let texture = egui::Context::default().load_texture(
            "viewport",
            egui::ColorImage::new([1, 1], egui::Color32::BLACK),
            egui::TextureOptions::NEAREST,
        );
        run_real_egui_viewport_click(
            &mut harness.workspace,
            click,
            EditorViewport3dPresentation::edit(texture.id(), Vec::new()),
        );
        assert_eq!(
            harness.workspace.selected_brush,
            Some(1),
            "{tool:?} must select BSP geometry through the real egui Response path"
        );
        assert_eq!(harness.workspace.selected_brush_face, Some(5));
        assert!(harness.workspace.brush_is_selected(1));
    }
}

#[test]
fn select_tool_resolves_bsp_only_brush_and_clears_old_node_selection() {
    let mut project = ProjectDocument::new("select bsp-only brush");
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [-256, 0, -256],
            [256, 512, 256],
        ));
    let old_node = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Old selection",
        NodeKind::PointLight {
            color: [255; 3],
            intensity: 1.0,
            radius: 1.0,
        },
    );
    project
        .active_scene_mut()
        .node_mut(old_node)
        .unwrap()
        .transform
        .translation = [8192.0, 8192.0, 8192.0];
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.active_tool = ViewTool::Select;
    workspace.replace_node_selection(old_node);
    workspace.camera_rig.mode = ViewportCameraMode::Free;
    workspace.camera_rig.free_initialized = true;
    workspace.camera_rig.free_position = [1200, 1000, -1200];
    let (yaw, pitch) = camera_angles_to_look_at([1200, 1000, -1200], [0, 256, 0]).unwrap();
    workspace.camera_rig.free_yaw = yaw;
    workspace.camera_rig.free_pitch = pitch;
    let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let camera = workspace.viewport_3d_camera();
    let (nx, ny) = camera
        .normalized_panel_point_for_world([0.0, 512.0, 0.0])
        .expect("brush face projects");
    let pointer = Pos2::new(
        rect.center().x + nx * rect.width() * 0.5,
        rect.center().y + ny * rect.height() * 0.5,
    );
    let target = workspace
        .resolve_viewport_3d_pointer_target(rect, pointer, None, true)
        .expect("BSP brush target");
    assert!(matches!(
        target,
        Viewport3dPointerTarget::Brush { brush: 0, face: _ }
    ));
    let frame = ToolFrame3d {
        rect,
        pointer_interact: Some(pointer),
        pointer_hover: Some(pointer),
        modifiers: egui::Modifiers::NONE,
        pointer_target: Some(target),
        hover_room: None,
        drag_delta_y: 0.0,
    };
    tool_impl_3d(ViewTool::Select).primary_clicked(&mut workspace, &frame);

    assert_eq!(workspace.selected_brush, Some(0));
    assert!(workspace.selected_brush_face.is_some());
    assert_eq!(workspace.selection.selected_node, NodeId::ROOT);
    assert!(workspace.selection.selected_nodes.is_empty());
    assert!(workspace.selection.selected_primitives.is_empty());
}

#[test]
fn test1_sized_bsp_room_frames_all_brushes_in_top_view() {
    let mut project = ProjectDocument::new("test1 framing regression");
    for (min, max) in [
        ([0, 0, 0], [1024, 64, 768]),
        ([0, 448, 0], [1024, 512, 768]),
        ([0, 64, 0], [64, 448, 768]),
        ([960, 64, 0], [1024, 448, 768]),
        ([64, 64, 0], [960, 448, 64]),
        ([64, 64, 704], [960, 448, 768]),
        ([480, 64, 64], [544, 448, 320]),
        ([480, 64, 448], [544, 448, 704]),
        ([480, 64, 320], [544, 256, 448]),
    ] {
        project
            .active_scene_mut()
            .brushes
            .push(psxed_project::brush::Brush::cuboid(min, max));
    }
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.active_tool = ViewTool::Brush;
    workspace.last_viewport_size = Vec2::new(1280.0, 720.0);
    workspace.set_orthographic_view(OrthographicView::Top);

    assert_eq!(workspace.orthographic_focus, [512.0, 0.0, 384.0]);
    assert!((workspace.viewport_zoom - 0.675).abs() < 0.001);
    assert!(workspace.viewport_zoom < 1.0, "the complete map must fit");
    assert_eq!(
        crate::viewport2d::format_viewport_zoom(workspace.viewport_zoom),
        "0.675 px/unit"
    );
    assert_eq!(crate::viewport2d::readable_grid_step(16.0, 0.675), 16.0);
    assert_eq!(crate::viewport2d::readable_grid_step(16.0, 0.05), 256.0);
}

#[test]
fn top_view_frames_selected_non_root_node_before_all_bsp_brushes() {
    let mut project = ProjectDocument::new("selected node BSP framing regression");
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [0, 0, 0],
            [1024, 512, 768],
        ));
    let light = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Far selected light",
        NodeKind::PointLight {
            color: [255; 3],
            intensity: 1.0,
            radius: 1.0,
        },
    );
    project
        .active_scene_mut()
        .node_mut(light)
        .unwrap()
        .transform
        .translation = [4096.0, 128.0, -2048.0];
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.active_tool = ViewTool::Select;
    workspace.view_2d = true;
    workspace.orthographic_view = OrthographicView::Top;
    workspace.last_viewport_size = Vec2::new(1280.0, 720.0);
    workspace.replace_node_selection(light);
    workspace.frame_viewport();

    assert_eq!(workspace.orthographic_focus, [4096.0, 0.0, -2048.0]);
    assert!(workspace.viewport_zoom > 10.0);
}

#[test]
fn top_view_can_frame_full_i16_bsp_world_bounds_at_minimum_viewport() {
    let mut project = ProjectDocument::new("large BSP framing regression");
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [-32768, 0, -32768],
            [32767, 512, 32767],
        ));
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.active_tool = ViewTool::Brush;
    workspace.last_viewport_size = Vec2::new(320.0, 240.0);
    workspace.set_orthographic_view(OrthographicView::Top);

    assert!(workspace.viewport_zoom > MIN_VIEWPORT_ZOOM);
    assert!(workspace.viewport_zoom < 0.0035);
    let content_width = 65535.0 * workspace.viewport_zoom;
    let content_height = 65535.0 * workspace.viewport_zoom;
    assert!(content_width <= 320.0 * 0.72 + 0.01);
    assert!(content_height <= 240.0 * 0.72 + 0.01);
    assert!(
        crate::viewport2d::readable_grid_step(16.0, MIN_VIEWPORT_ZOOM) * MIN_VIEWPORT_ZOOM >= 10.0
    );
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
    assert!(
        harness.workspace.brush_drag.is_some(),
        "press anchors a drag"
    );
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
    harness.workspace.brush_edit_mode = BrushEditMode::Face;
    harness.workspace.selection_mode = SelectionMode::Face;
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

/// The third `Clip keeps` mode. Both and Back had coverage; Front did not,
/// and it is the one that replaces the brush with the far half.
#[test]
fn brush_clip_keep_front_replaces_with_the_far_half() {
    let mut harness = ViewportHarness::floored_room("brush_clip_front", 4);
    harness.workspace.active_tool = ViewTool::Brush;
    harness.workspace.begin_brush_drag_2d([0.0, 0.0]);
    harness.workspace.update_brush_drag_2d([256.0, 128.0]);
    harness.workspace.commit_brush_drag();
    assert!(harness.workspace.select_brush_at_2d([128.0, 64.0]));

    // Cycle the toolbar control off Both, through Back, onto Front.
    assert_eq!(harness.workspace.brush_clip_keep, BrushClipKeep::Both);
    harness.workspace.brush_clip_keep = harness.workspace.brush_clip_keep.next();
    harness.workspace.brush_clip_keep = harness.workspace.brush_clip_keep.next();
    assert_eq!(harness.workspace.brush_clip_keep, BrushClipKeep::Front);

    harness.workspace.brush_clip_click([128, 0, -64]);
    harness.workspace.brush_clip_click([128, 0, 192]);

    let scene = harness.workspace.project.active_scene();
    assert_eq!(scene.brushes.len(), 1, "keep Front replaces in place");
    let solved = scene.brushes[0].solve();
    assert!(solved.is_valid());
    assert_eq!(solved.max[0] - solved.min[0], 128.0, "one half survived");
    harness.workspace.do_undo();
    assert_eq!(
        harness.workspace.project.active_scene().brushes[0]
            .solve()
            .max[0],
        256.0,
        "one undo restores the unclipped brush"
    );
}

/// Clip is part of the required authoring loop, so a clipped brush has to
/// survive the whole loop: split, save, reopen, cook. A clip that produced a
/// degenerate or non-convex brush would only surface at cook time.
#[test]
fn clipped_brush_saves_reopens_and_still_cooks() {
    let template = psxed_project::new_project_template_dir();
    let dir = test_temp_dir("clip-cook-loop");
    crate::starter_catalogue::copy_dir_recursive(&template, &dir).unwrap();
    let mut workspace = EditorWorkspace::open_directory(&dir).unwrap();
    workspace.active_tool = ViewTool::Brush;

    let before = workspace.project().active_scene().brushes.len();
    // Clip the template's floor slab: pick the brush whose solved box is the
    // widest, then cut it with a vertical plane through its centre.
    let (index, solved) = workspace
        .project()
        .active_scene()
        .brushes
        .iter()
        .enumerate()
        .map(|(index, brush)| (index, brush.solve()))
        .filter(|(_, solved)| solved.is_valid())
        .max_by(|a, b| {
            (a.1.max[0] - a.1.min[0])
                .partial_cmp(&(b.1.max[0] - b.1.min[0]))
                .unwrap()
        })
        .expect("template brush");
    workspace.replace_brush_selection(index, None);
    let mid_x = ((solved.min[0] + solved.max[0]) * 0.5).round() as i32;
    let min_z = solved.min[2].round() as i32;
    let max_z = solved.max[2].round() as i32;
    workspace.brush_clip_click([mid_x, 0, min_z - 64]);
    assert!(workspace.brush_clip_start.is_some(), "first click armed");
    workspace.brush_clip_click([mid_x, 0, max_z + 64]);
    assert_eq!(
        workspace.project().active_scene().brushes.len(),
        before + 1,
        "clip split the brush in two"
    );

    workspace.save_if_dirty().expect("save the clipped project");
    let reopened = EditorWorkspace::open_directory(&dir).expect("reopen");
    assert_eq!(
        reopened.project().active_scene().brushes.len(),
        before + 1,
        "the split survived save and reopen"
    );
    for brush in &reopened.project().active_scene().brushes {
        assert!(brush.solve().is_valid(), "clip left a degenerate brush");
    }

    let project = reopened.project().clone();
    let (package, report) = psxed_project::playtest::build_package(&project, &dir);
    assert!(report.is_ok(), "clipped world must cook: {:?}", report.errors);
    assert!(package.is_some());

    let _ = std::fs::remove_dir_all(dir);
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
        .push(psxed_project::brush::Brush::cuboid(
            [1, 0, -1],
            [65, 63, 62],
        ));
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
    harness.workspace.delete_selected_brushes();
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
fn shift_click_builds_multi_selection_and_group_moves() {
    let mut harness = ViewportHarness::floored_room("brush_multi_move", 4);
    harness.workspace.active_tool = ViewTool::Brush;
    harness
        .workspace
        .set_orthographic_view(OrthographicView::Top);
    let scene = harness.workspace.project.active_scene_mut();
    scene
        .brushes
        .push(psxed_project::brush::Brush::cuboid([0, 0, 0], [64, 64, 64]));
    scene.brushes.push(psxed_project::brush::Brush::cuboid(
        [256, 0, 0],
        [320, 64, 64],
    ));

    // Plain click selects one; shift-click adds the second (through the
    // real 2D click dispatch); shift-click again removes it.
    harness
        .workspace
        .handle_viewport_click([32.0, 32.0], &[], egui::Modifiers::default());
    assert_eq!(harness.workspace.selected_brush, Some(0));
    let shift = egui::Modifiers {
        shift: true,
        ..Default::default()
    };
    harness
        .workspace
        .handle_viewport_click([288.0, 32.0], &[], shift);
    assert_eq!(harness.workspace.selected_brush, Some(1));
    assert_eq!(harness.workspace.selected_brush_set(), vec![0, 1]);
    harness
        .workspace
        .handle_viewport_click([288.0, 32.0], &[], shift);
    assert_eq!(harness.workspace.selected_brush_set(), vec![0]);
    harness
        .workspace
        .handle_viewport_click([288.0, 32.0], &[], shift);

    // A move grabbed on one member drags the whole selection, commits
    // as one undo step, and undo restores both.
    assert!(harness.workspace.begin_brush_move_2d([32.0, 32.0]));
    harness.workspace.update_brush_move_2d([96.0, 32.0]);
    harness.workspace.commit_brush_gesture_2d();
    let a = harness.workspace.project.active_scene().brushes[0].solve();
    let b = harness.workspace.project.active_scene().brushes[1].solve();
    assert_eq!(a.min[0], 64.0, "grabbed brush moved");
    assert_eq!(b.min[0], 320.0, "selected rider moved by the same delta");

    harness.workspace.do_undo();
    let a = harness.workspace.project.active_scene().brushes[0].solve();
    let b = harness.workspace.project.active_scene().brushes[1].solve();
    assert_eq!(a.min[0], 0.0);
    assert_eq!(b.min[0], 256.0, "one undo restores the whole group");

    // Grabbing an unselected brush replaces the selection and moves it alone.
    harness.workspace.clear_brush_selection();
    assert!(harness.workspace.begin_brush_move_2d([288.0, 32.0]));
    harness.workspace.update_brush_move_2d([352.0, 32.0]);
    harness.workspace.commit_brush_gesture_2d();
    let a = harness.workspace.project.active_scene().brushes[0].solve();
    let b = harness.workspace.project.active_scene().brushes[1].solve();
    assert_eq!(a.min[0], 0.0, "unselected brush stays put");
    assert_eq!(b.min[0], 320.0);
    assert_eq!(harness.workspace.selected_brush_set(), vec![1]);
}

#[test]
fn multi_selection_delete_and_duplicate_are_grouped() {
    let mut harness = ViewportHarness::floored_room("brush_multi_edit", 4);
    harness.workspace.active_tool = ViewTool::Brush;
    let scene = harness.workspace.project.active_scene_mut();
    scene
        .brushes
        .push(psxed_project::brush::Brush::cuboid([0, 0, 0], [64, 64, 64]));
    scene.brushes.push(psxed_project::brush::Brush::cuboid(
        [256, 0, 0],
        [320, 64, 64],
    ));
    scene.brushes.push(psxed_project::brush::Brush::cuboid(
        [512, 0, 0],
        [576, 64, 64],
    ));

    // Select brushes 0 and 2, duplicate: two copies appended, the copies
    // become the selection (Cmd+D routes here for the Brush tool).
    harness.workspace.replace_brush_selection(0, None);
    harness.workspace.toggle_brush_selection(2);
    harness.workspace.duplicate_current_selection();
    assert_eq!(harness.workspace.project.active_scene().brushes.len(), 5);
    assert_eq!(harness.workspace.selected_brush_set(), vec![3, 4]);
    assert_eq!(
        harness.workspace.selected_brush,
        Some(4),
        "primary follows its copy"
    );
    harness.workspace.do_undo();
    assert_eq!(harness.workspace.project.active_scene().brushes.len(), 3);

    // Delete both members in one undo step.
    harness.workspace.replace_brush_selection(0, None);
    harness.workspace.toggle_brush_selection(2);
    harness.workspace.delete_selected_brushes();
    assert_eq!(harness.workspace.project.active_scene().brushes.len(), 1);
    assert_eq!(harness.workspace.selected_brush, None);
    let survivor = harness.workspace.project.active_scene().brushes[0].solve();
    assert_eq!(survivor.min[0], 256.0, "the unselected brush survives");
    harness.workspace.do_undo();
    assert_eq!(harness.workspace.project.active_scene().brushes.len(), 3);
}

#[test]
fn undo_reconciles_stale_brush_selection_and_clip_never_panics() {
    let mut harness = ViewportHarness::floored_room("brush_stale_sel", 4);
    harness.workspace.active_tool = ViewTool::Brush;

    // Create two brushes, select the second, then undo its creation:
    // the selection must fall back instead of dangling past the end.
    harness.workspace.begin_brush_drag_2d([0.0, 0.0]);
    harness.workspace.update_brush_drag_2d([128.0, 128.0]);
    harness.workspace.commit_brush_drag();
    harness.workspace.begin_brush_drag_2d([256.0, 0.0]);
    harness.workspace.update_brush_drag_2d([384.0, 128.0]);
    harness.workspace.commit_brush_drag();
    assert_eq!(harness.workspace.selected_brush, Some(1));
    harness.workspace.do_undo();
    assert_eq!(harness.workspace.project.active_scene().brushes.len(), 1);
    assert!(harness
        .workspace
        .selected_brush
        .is_none_or(|index| index < 1));

    // A stale index left behind must not panic the clip path.
    harness.workspace.selected_brush = Some(7);
    harness.workspace.brush_clip_click([0, 0, 0]);
    harness.workspace.brush_clip_click([0, 0, 128]);
    assert_eq!(harness.workspace.project.active_scene().brushes.len(), 1);
}

#[test]
fn shift_click_in_3d_toggles_multi_selection() {
    let mut harness = ViewportHarness::floored_room("brush_multi_3d", 4);
    harness.frame(harness.room_center(), 3000.0);
    harness.workspace.active_tool = ViewTool::Brush;
    let tool = tool_impl_3d(ViewTool::Brush);

    // Two brushes created through the tool.
    for (from, to) in [
        (Pos2::new(280.0, 300.0), Pos2::new(380.0, 360.0)),
        (Pos2::new(430.0, 300.0), Pos2::new(530.0, 360.0)),
    ] {
        let press = brush_frame(&harness, from);
        let drag = brush_frame(&harness, to);
        tool.primary_pressed(&mut harness.workspace, &press);
        tool.primary_dragged(&mut harness.workspace, &drag);
        tool.primary_released(&mut harness.workspace, &drag);
    }
    assert_eq!(harness.workspace.project.active_scene().brushes.len(), 2);

    // Click the first, shift-click the second: both selected.
    let click_a = brush_frame(&harness, Pos2::new(330.0, 330.0));
    tool.primary_clicked(&mut harness.workspace, &click_a);
    assert_eq!(harness.workspace.selected_brush, Some(0));
    let mut click_b = brush_frame(&harness, Pos2::new(480.0, 330.0));
    click_b.modifiers.shift = true;
    tool.primary_clicked(&mut harness.workspace, &click_b);
    assert_eq!(harness.workspace.selected_brush_set(), vec![0, 1]);
    assert!(harness.workspace.brush_is_selected(0));
    assert!(harness.workspace.brush_is_selected(1));

    // Shift-click on empty sky keeps the selection; plain click clears.
    let mut sky_shift = brush_frame(&harness, Pos2::new(400.0, 10.0));
    sky_shift.modifiers.shift = true;
    tool.primary_clicked(&mut harness.workspace, &sky_shift);
    assert_eq!(harness.workspace.selected_brush_set(), vec![0, 1]);
    let sky = brush_frame(&harness, Pos2::new(400.0, 10.0));
    tool.primary_clicked(&mut harness.workspace, &sky);
    assert!(harness.workspace.selected_brush_set().is_empty());
}

fn solved_unique_verts(brush: &psxed_project::brush::Brush) -> Vec<[i64; 3]> {
    let mut verts: Vec<[i64; 3]> = brush
        .solve()
        .polygons
        .iter()
        .flatten()
        .flat_map(|polygon| polygon.verts.iter())
        .map(|v| {
            [
                v[0].round() as i64,
                v[1].round() as i64,
                v[2].round() as i64,
            ]
        })
        .collect();
    verts.sort_unstable();
    verts.dedup();
    verts
}

#[test]
fn vertex_mode_corner_drag_reshapes_footprint() {
    let mut harness = ViewportHarness::floored_room("brush_vertex_drag", 4);
    harness.workspace.active_tool = ViewTool::Brush;
    harness.workspace.selection_mode = SelectionMode::Vertex;
    harness
        .workspace
        .set_orthographic_view(OrthographicView::Top);
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

    // Grab the projected (x=128, z=128) corner column and drag it to
    // x=64: the +X wall tilts and the footprint becomes a trapezoid.
    assert!(harness
        .workspace
        .begin_brush_vertex_drag_2d([127.0, 126.0], 4.0));
    let drag = harness.workspace.brush_vertex_drag.clone().expect("drag");
    assert_eq!(drag.targets.len(), 2, "corner grabs its depth column");
    harness.workspace.update_brush_vertex_drag_2d([63.0, 126.0]);
    harness.workspace.commit_brush_gesture_2d();

    let verts = solved_unique_verts(&harness.workspace.project.active_scene().brushes[0]);
    assert!(verts.contains(&[64, 0, 128]), "corner moved at floor");
    assert!(verts.contains(&[64, 128, 128]), "corner moved at ceiling");
    assert!(verts.contains(&[128, 0, 0]), "other corners untouched");
    assert!(harness.workspace.brush_vertex_drag.is_none());
    assert!(harness.workspace.is_dirty());

    // The whole drag is one undo step.
    harness.workspace.do_undo();
    let verts = solved_unique_verts(&harness.workspace.project.active_scene().brushes[0]);
    assert!(verts.contains(&[128, 0, 128]), "undo restores the cube");
}

#[test]
fn edge_mode_silhouette_drag_slides_whole_side() {
    let mut harness = ViewportHarness::floored_room("brush_edge_drag", 4);
    harness.workspace.active_tool = ViewTool::Brush;
    harness.workspace.selection_mode = SelectionMode::Edge;
    harness
        .workspace
        .set_orthographic_view(OrthographicView::Top);
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

    // Grab the +X silhouette edge (projected segment x=128, z 0..128)
    // near its midpoint: both endpoint columns move together, which
    // slides the whole side without tilting it.
    assert!(harness
        .workspace
        .begin_brush_edge_drag_2d([126.0, 64.0], 4.0));
    let drag = harness.workspace.brush_vertex_drag.clone().expect("drag");
    assert_eq!(drag.targets.len(), 4, "edge grabs both depth columns");
    harness.workspace.update_brush_vertex_drag_2d([158.0, 64.0]);
    harness.workspace.commit_brush_gesture_2d();

    let solved = harness.workspace.project.active_scene().brushes[0].solve();
    assert_eq!(solved.max[0], 160.0, "side slid out to the snapped drop");
    assert_eq!(solved.min, [0.0, 0.0, 0.0]);
    assert_eq!(solved.max[1], 128.0);
    assert_eq!(solved.max[2], 128.0);
}

#[test]
fn vertex_drag_refuses_invalid_shapes_and_escape_cancels() {
    let mut harness = ViewportHarness::floored_room("brush_vertex_invalid", 4);
    harness.workspace.active_tool = ViewTool::Brush;
    harness.workspace.selection_mode = SelectionMode::Edge;
    harness
        .workspace
        .set_orthographic_view(OrthographicView::Top);
    harness
        .workspace
        .project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid([0, 0, 0], [64, 64, 64]));
    harness.workspace.selected_brush = Some(0);
    let original = harness.workspace.project.active_scene().brushes[0].clone();

    // Dragging the +X side past the -X plane would invert the brush:
    // the preview refuses to advance and the commit records nothing.
    assert!(harness
        .workspace
        .begin_brush_edge_drag_2d([63.0, 32.0], 4.0));
    harness
        .workspace
        .update_brush_vertex_drag_2d([-130.0, 32.0]);
    harness.workspace.commit_brush_gesture_2d();
    assert_eq!(
        harness.workspace.project.active_scene().brushes[0],
        original,
        "invalid preview never lands"
    );
    harness.workspace.do_undo();
    assert_eq!(harness.workspace.status, "Nothing to undo");

    // Escape mid-drag restores the base shape.
    assert!(harness
        .workspace
        .begin_brush_edge_drag_2d([63.0, 32.0], 4.0));
    harness.workspace.update_brush_vertex_drag_2d([95.0, 32.0]);
    let solved = harness.workspace.project.active_scene().brushes[0].solve();
    assert_eq!(solved.max[0], 96.0, "preview applied while dragging");
    harness.workspace.cancel_brush_gestures();
    assert_eq!(
        harness.workspace.project.active_scene().brushes[0],
        original,
        "cancel restores the pre-drag brush"
    );
}

#[test]
fn brush_numeric_origin_and_face_plane_edits() {
    let mut harness = ViewportHarness::floored_room("brush_numeric", 4);
    harness.workspace.active_tool = ViewTool::Brush;
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
    harness.workspace.selected_brush_face = Some(3); // +X face

    // Whole-brush origin entry translates without changing size.
    assert_eq!(harness.workspace.selected_brush_origin(), Some([0, 0, 0]));
    assert!(harness.workspace.set_selected_brush_origin([32, 16, -64]));
    let solved = harness.workspace.project.active_scene().brushes[0].solve();
    assert_eq!(solved.min, [32.0, 16.0, -64.0]);
    assert_eq!(solved.max, [160.0, 144.0, 64.0]);

    // The +X face reports its dominant axis and exact plane position.
    assert_eq!(harness.workspace.selected_brush_face_axis(), Some((0, 160)));
    assert!(harness.workspace.set_selected_brush_face_axis_position(200));
    let solved = harness.workspace.project.active_scene().brushes[0].solve();
    assert_eq!(solved.max[0], 200.0);

    // The numeric fallback is exact: off-grid values are not snapped.
    assert!(harness.workspace.set_selected_brush_face_axis_position(203));
    let solved = harness.workspace.project.active_scene().brushes[0].solve();
    assert_eq!(solved.max[0], 203.0);

    // Sliding the face past the opposite side would stop the brush
    // enclosing volume: rejected, brush untouched.
    assert!(!harness
        .workspace
        .set_selected_brush_face_axis_position(-500));
    let solved = harness.workspace.project.active_scene().brushes[0].solve();
    assert_eq!(solved.max[0], 203.0);
    assert!(harness.workspace.is_dirty());
}

#[test]
fn brush_numeric_drag_coalesces_to_one_undo_step() {
    let mut harness = ViewportHarness::floored_room("brush_numeric_undo", 4);
    harness.workspace.active_tool = ViewTool::Brush;
    harness
        .workspace
        .project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid([0, 0, 0], [64, 64, 64]));
    harness.workspace.selected_brush = Some(0);

    // Three frames of a held DragValue drag, exactly as the inspector
    // wrapper sees them: one coalesced undo step at the end.
    let drag = InspectorUndoInput {
        pointer_down: true,
        ..InspectorUndoInput::default()
    };
    for x in [16, 32, 48] {
        let before = harness.workspace.project.clone();
        let epoch = harness.workspace.history.epoch();
        assert!(harness.workspace.set_selected_brush_origin([x, 0, 0]));
        harness.workspace.finish_inspector_undo(before, epoch, drag);
    }
    harness
        .workspace
        .prepare_inspector_undo(InspectorUndoInput::default());

    harness.workspace.do_undo();
    let solved = harness.workspace.project.active_scene().brushes[0].solve();
    assert_eq!(solved.min[0], 0.0, "one undo unwinds the whole drag");
    harness.workspace.do_redo();
    let solved = harness.workspace.project.active_scene().brushes[0].solve();
    assert_eq!(solved.min[0], 48.0, "redo restores the final drag state");
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
        ("duplicate", |ws| ws.duplicate_selected_brushes()),
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
            ws.delete_selected_brushes();
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
        .push(psxed_project::brush::Brush::cuboid(
            [0, 0, 0],
            [128, 128, 128],
        ));
    harness
        .workspace
        .set_orthographic_view(OrthographicView::Top);
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

#[test]
fn brush_contents_apply_to_multiselection_clear_movers_and_are_undoable() {
    use psxed_project::brush::BrushContents;

    let mut harness = ViewportHarness::floored_room("brush_contents", 4);
    let door = harness.workspace.project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Liquid Candidate Door",
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
    let mut first = psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]);
    first.mover = Some(door);
    let second = psxed_project::brush::Brush::cuboid([256, 0, 0], [384, 128, 128]);
    harness
        .workspace
        .project
        .active_scene_mut()
        .brushes
        .extend([first, second]);
    harness.workspace.selected_brush = Some(0);
    harness.workspace.selected_brushes = vec![0, 1];

    harness
        .workspace
        .set_selected_brush_contents(BrushContents::Water);
    assert!(harness
        .workspace
        .project
        .active_scene()
        .brushes
        .iter()
        .all(|brush| brush.contents == BrushContents::Water));
    assert_eq!(
        harness.workspace.project.active_scene().brushes[0].mover,
        None
    );
    assert!(harness.workspace.status.contains("removed 1 Door binding"));
    assert!(harness.workspace.is_dirty());

    harness.workspace.set_selected_brush_mover(Some(door));
    assert_eq!(
        harness.workspace.project.active_scene().brushes[0].mover,
        None
    );
    assert!(harness
        .workspace
        .status
        .contains("cannot be bound to a Door"));

    harness.workspace.do_undo();
    let brushes = &harness.workspace.project.active_scene().brushes;
    assert_eq!(brushes[0].contents, BrushContents::Solid);
    assert_eq!(brushes[0].mover, Some(door));
    assert_eq!(brushes[1].contents, BrushContents::Solid);
}

#[test]
fn real_egui_brush_inspector_exposes_and_changes_bsp_contents() {
    use psxed_project::brush::BrushContents;

    let mut harness = ViewportHarness::floored_room("brush_contents_egui", 4);
    let mut water_brush = psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]);
    water_brush.contents = BrushContents::Water;
    let solid_brush = psxed_project::brush::Brush::cuboid([256, 0, 0], [384, 128, 128]);
    harness
        .workspace
        .project
        .active_scene_mut()
        .brushes
        .extend([water_brush, solid_brush]);
    harness.workspace.active_workspace = WorkspaceView::Room;
    harness.workspace.active_tool = ViewTool::Select;
    harness.workspace.selected_brush = Some(0);
    harness.workspace.selected_brushes = vec![0, 1];

    let ctx = egui::Context::default();
    let mut fonts = egui::FontDefinitions::default();
    let proportional = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .expect("default proportional font family");
    fonts
        .families
        .insert(egui::FontFamily::Name("lucide".into()), proportional);
    ctx.set_fonts(fonts);
    let texture = ctx.load_texture(
        "brush-contents-inspector",
        egui::ColorImage::new([1, 1], egui::Color32::BLACK),
        egui::TextureOptions::NEAREST,
    );
    let viewport = EditorViewport3dPresentation::edit(texture.id(), Vec::new());
    let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1800.0, 1000.0));
    let input = |time, events| egui::RawInput {
        screen_rect: Some(screen),
        time: Some(time),
        events,
        ..egui::RawInput::default()
    };
    let draw = |ctx: &egui::Context, workspace: &mut EditorWorkspace| {
        workspace.draw(ctx, viewport.clone(), EditorPlaytestStatus::Idle)
    };

    let initial = ctx.run(input(0.0, vec![]), |ctx| draw(ctx, &mut harness.workspace));
    assert!(text_shape_center(&initial.shapes, "BSP contents").is_some());
    let mixed = text_shape_center(&initial.shapes, "Mixed").expect("mixed combo value");
    let _ = ctx.run(
        input(
            1.0 / 60.0,
            vec![
                egui::Event::PointerMoved(mixed),
                egui::Event::PointerButton {
                    pos: mixed,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        ),
        |ctx| draw(ctx, &mut harness.workspace),
    );
    let _ = ctx.run(
        input(
            2.0 / 60.0,
            vec![egui::Event::PointerButton {
                pos: mixed,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        ),
        |ctx| draw(ctx, &mut harness.workspace),
    );
    let menu = ctx.run(input(3.0 / 60.0, vec![]), |ctx| {
        draw(ctx, &mut harness.workspace)
    });
    for label in ["Water", "Slime", "Lava"] {
        assert!(
            text_shape_center(&menu.shapes, label).is_some(),
            "open contents combo omitted {label}"
        );
    }
    let water = text_shape_center(&menu.shapes, "Water").expect("Water option");
    let _ = ctx.run(
        input(
            4.0 / 60.0,
            vec![
                egui::Event::PointerMoved(water),
                egui::Event::PointerButton {
                    pos: water,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        ),
        |ctx| draw(ctx, &mut harness.workspace),
    );
    let _ = ctx.run(
        input(
            5.0 / 60.0,
            vec![egui::Event::PointerButton {
                pos: water,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        ),
        |ctx| draw(ctx, &mut harness.workspace),
    );
    assert!(harness
        .workspace
        .project
        .active_scene()
        .brushes
        .iter()
        .all(|brush| brush.contents == BrushContents::Water));
    assert!(harness.workspace.is_dirty());
    harness.workspace.do_undo();
    assert_eq!(
        harness.workspace.project.active_scene().brushes[0].contents,
        BrushContents::Water
    );
    assert_eq!(
        harness.workspace.project.active_scene().brushes[1].contents,
        BrushContents::Solid
    );
}

/// Real-egui context plus edit-mode viewport for driving whole
/// `workspace.draw` frames. Shared by the Inspector click/type helpers so
/// both see the same font set (the `lucide` icon family is aliased onto the
/// proportional family, otherwise icon labels render as tofu and cannot be
/// located by galley text).
fn real_egui_workspace_ctx(name: &str) -> (egui::Context, EditorViewport3dPresentation) {
    let ctx = egui::Context::default();
    let mut fonts = egui::FontDefinitions::default();
    let proportional = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .expect("default proportional font family");
    fonts
        .families
        .insert(egui::FontFamily::Name("lucide".into()), proportional);
    ctx.set_fonts(fonts);
    let texture = ctx.load_texture(
        name,
        egui::ColorImage::new([1, 1], egui::Color32::BLACK),
        egui::TextureOptions::NEAREST,
    );
    (
        ctx,
        EditorViewport3dPresentation::edit(texture.id(), Vec::new()),
    )
}

/// One full `workspace.draw` frame at `time` with `events` delivered.
fn real_egui_workspace_frame(
    ctx: &egui::Context,
    workspace: &mut EditorWorkspace,
    viewport: &EditorViewport3dPresentation,
    time: f64,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1800.0, 1000.0))),
            time: Some(time),
            events,
            ..egui::RawInput::default()
        },
        |ctx| workspace.draw(ctx, viewport.clone(), EditorPlaytestStatus::Idle),
    )
}

/// Locate the one widget whose rendered galley text equals `label`, and
/// prove the pointer can actually reach it.
///
/// Panics when the label is missing, ambiguous (the Inspector paints several
/// same-named buttons, so taking the first match could drive a control from
/// another section), or painted outside the screen. That last check is not
/// pedantry: an Inspector row wide enough to overflow its panel still emits
/// its text shapes, but egui clips the widget out of the interact layer, so
/// the control becomes permanently unclickable in the real editor.
fn locate_unique_label(
    ctx: &egui::Context,
    workspace: &mut EditorWorkspace,
    viewport: &EditorViewport3dPresentation,
    label: &str,
) -> Pos2 {
    let frame = real_egui_workspace_frame(ctx, workspace, viewport, 0.0, vec![]);
    let found = text_shape_centers(&frame.shapes, label);
    assert_eq!(
        found.len(),
        1,
        "label {label:?} must be visible exactly once in the drawn frame, saw {found:?}"
    );
    let point = found[0];
    let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1800.0, 1000.0));
    assert!(
        screen.contains(point),
        "label {label:?} is painted at {point:?}, outside the {screen:?} screen: \
         its row overflows the panel and the control is unreachable"
    );
    point
}

fn press_release(point: Pos2) -> (Vec<egui::Event>, Vec<egui::Event>) {
    (
        vec![
            egui::Event::PointerMoved(point),
            egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
        vec![egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
    )
}

/// Click the widget whose rendered galley text equals `label`, through full
/// `workspace.draw` frames so the Inspector transaction wrapper owns history
/// exactly as it does in production.
fn run_real_egui_workspace_click_on_label(workspace: &mut EditorWorkspace, label: &str) {
    let (ctx, viewport) = real_egui_workspace_ctx("workspace-label-click");
    let point = locate_unique_label(&ctx, workspace, &viewport, label);
    let (press, release) = press_release(point);
    let _ = real_egui_workspace_frame(&ctx, workspace, &viewport, 1.0 / 60.0, press);
    let _ = real_egui_workspace_frame(&ctx, workspace, &viewport, 2.0 / 60.0, release);
    let _ = real_egui_workspace_frame(&ctx, workspace, &viewport, 3.0 / 60.0, vec![]);
}

/// Click the DragValue whose current galley text is `label`, type `entry`,
/// and commit with Enter.
fn run_real_egui_type_into_drag_value(workspace: &mut EditorWorkspace, label: &str, entry: &str) {
    let (ctx, viewport) = real_egui_workspace_ctx("workspace-drag-value-entry");
    let point = locate_unique_label(&ctx, workspace, &viewport, label);
    let (press, release) = press_release(point);
    let _ = real_egui_workspace_frame(&ctx, workspace, &viewport, 1.0 / 60.0, press);
    let _ = real_egui_workspace_frame(&ctx, workspace, &viewport, 2.0 / 60.0, release);
    let _ = real_egui_workspace_frame(
        &ctx,
        workspace,
        &viewport,
        3.0 / 60.0,
        vec![egui::Event::Text(entry.to_string())],
    );
    for (index, pressed) in [(4.0, true), (5.0, false)] {
        let _ = real_egui_workspace_frame(
            &ctx,
            workspace,
            &viewport,
            index / 60.0,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: Some(egui::Key::Enter),
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
    }
    // One quiet frame so the Inspector undo transaction closes when the
    // committed edit surrenders keyboard focus.
    let _ = real_egui_workspace_frame(&ctx, workspace, &viewport, 6.0 / 60.0, vec![]);
}

/// The Inspector's "Apply to face" button paints exactly the selected face
/// with the PICKED material, as one undo step, through real egui.
#[test]
fn apply_to_face_button_paints_only_the_selected_face_and_undoes_once() {
    let mut project = ProjectDocument::new("face material apply");
    // Two materials, so a bug that grabbed "the first material" instead of
    // the picked one would still fail the assertion below.
    let stone = project.add_resource(
        "Stone",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let moss = project.add_resource(
        "Moss",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [0, 0, 0],
            [128, 128, 128],
        ));
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("apply-face"), project);
    workspace.active_workspace = WorkspaceView::Room;
    workspace.active_tool = ViewTool::Select;
    workspace.replace_brush_selection(0, Some(2));
    workspace.brush_material = Some(moss);

    run_real_egui_workspace_click_on_label(
        &mut workspace,
        &icons::label(icons::PALETTE, "Apply to face"),
    );

    let faces = &workspace.project.active_scene().brushes[0].faces;
    assert_eq!(faces[2].material, Some(moss), "selected face painted");
    assert_ne!(faces[2].material, Some(stone), "picked material, not first");
    for (index, face) in faces.iter().enumerate() {
        if index != 2 {
            assert_eq!(face.material, None, "face {index} must stay untouched");
        }
    }
    assert!(workspace.is_dirty());

    workspace.do_undo();
    assert!(workspace.project.active_scene().brushes[0]
        .faces
        .iter()
        .all(|face| face.material.is_none()));
    workspace.do_undo();
    assert_eq!(
        workspace.status, "Nothing to undo",
        "apply-to-face must cost exactly one undo step"
    );
    workspace.do_redo();
    assert_eq!(
        workspace.project.active_scene().brushes[0].faces[2].material,
        Some(moss)
    );
}

/// Face UV offset / rotation / scale numeric edits, typed through the real
/// Inspector DragValues: each edit is one undo step (owned by the Inspector
/// transaction wrapper, no local `push_undo` needed), the authored values
/// land in the cooked brush world, and "Reset UV" restores identity.
#[test]
fn face_uv_numeric_edits_cook_into_the_brush_world_and_undo_per_edit() {
    let template = psxed_project::new_project_template_dir();
    let dir = test_temp_dir("face-uv-cook");
    crate::starter_catalogue::copy_dir_recursive(&template, &dir).unwrap();
    let mut workspace = EditorWorkspace::open_directory(&dir).unwrap();
    workspace.active_workspace = WorkspaceView::Room;
    workspace.active_tool = ViewTool::Select;
    workspace.replace_brush_selection(0, Some(0));
    assert_eq!(
        workspace.project.active_scene().brushes[0].faces[0].uv,
        psxed_project::brush::FaceUv::default(),
        "fixture face must start at the identity UV mapping"
    );

    let cook_before = test_temp_dir("face-uv-cook-before");
    workspace.cook_playtest_to_dir(&cook_before).expect("cook");

    run_real_egui_type_into_drag_value(&mut workspace, "U 0", "24");
    run_real_egui_type_into_drag_value(&mut workspace, "0\u{b0}", "15");
    run_real_egui_type_into_drag_value(&mut workspace, "100% U", "150");

    let uv = workspace.project.active_scene().brushes[0].faces[0].uv;
    assert_eq!(uv.offset_texels, [24, 0]);
    assert_eq!(uv.rotation_deg, 15);
    assert_eq!(uv.scale_q8, [384, 256], "150% is 384 in Q8");
    assert!(workspace.is_dirty());

    // The authored UV values reach the cooked brush world.
    let cook_after = test_temp_dir("face-uv-cook-after");
    workspace.cook_playtest_to_dir(&cook_after).expect("recook");
    let world = psxed_project::brush_playtest::BRUSH_WORLD_FILENAME;
    assert_ne!(
        std::fs::read(cook_before.join(world)).unwrap(),
        std::fs::read(cook_after.join(world)).unwrap(),
        "UV edits must change the cooked brush world"
    );

    // One undo step per typed edit, unwound in reverse order.
    workspace.do_undo();
    let uv = workspace.project.active_scene().brushes[0].faces[0].uv;
    assert_eq!((uv.scale_q8, uv.rotation_deg), ([256, 256], 15));
    workspace.do_undo();
    let uv = workspace.project.active_scene().brushes[0].faces[0].uv;
    assert_eq!((uv.rotation_deg, uv.offset_texels), (0, [24, 0]));
    workspace.do_undo();
    assert_eq!(
        workspace.project.active_scene().brushes[0].faces[0].uv,
        psxed_project::brush::FaceUv::default()
    );
    workspace.do_undo();
    assert_eq!(
        workspace.status, "Nothing to undo",
        "three typed edits must cost exactly three undo steps"
    );

    // Redo the full stack, then "Reset UV" restores identity as one step.
    for _ in 0..3 {
        workspace.do_redo();
    }
    let uv = workspace.project.active_scene().brushes[0].faces[0].uv;
    assert_eq!(uv.offset_texels, [24, 0]);
    assert_eq!(uv.scale_q8, [384, 256]);
    run_real_egui_workspace_click_on_label(&mut workspace, "Reset UV");
    assert_eq!(
        workspace.project.active_scene().brushes[0].faces[0].uv,
        psxed_project::brush::FaceUv::default()
    );
    workspace.do_undo();
    let uv = workspace.project.active_scene().brushes[0].faces[0].uv;
    assert_eq!(uv.offset_texels, [24, 0], "one undo unwinds Reset UV");

    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(cook_before);
    let _ = std::fs::remove_dir_all(cook_after);
}

/// Same slope plane as `arbitrary_plane_face_handle_drags_along_its_normal_and_undoes_once`,
/// but authored from points that all sit OFF the solved polygon: the face
/// handle must key off the solved polygon centre plus the authored plane,
/// never off the raw authored points.
#[test]
fn off_corner_authored_plane_face_handle_drags_along_its_normal_and_undoes_once() {
    let mut wedge = psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]);
    // Plane x + y = 128 (outward normal (1,1,0)), authored from three points
    // far outside the brush bounds.
    wedge.faces[5] = psxed_project::brush::BrushFace::from_points([
        [-64, 192, 0],
        [-64, 192, 128],
        [192, -64, 128],
    ]);
    assert!(wedge.solve().is_valid());
    // Fixture precondition: no authored plane point coincides with a solved
    // polygon corner.
    let corners = solved_unique_verts(&wedge);
    for point in wedge.faces[5].points {
        assert!(
            !corners.contains(&point.map(i64::from)),
            "authored point {point:?} must sit off the solved polygon"
        );
    }
    let base = wedge.clone();
    let (mut workspace, rect) = handle_test_workspace(wedge);
    workspace.brush_edit_mode = BrushEditMode::Face;
    workspace.selection_mode = SelectionMode::Face;
    let (center, _) = EditorWorkspace::face_center_and_normal(&base, 5).unwrap();
    let pointer = workspace.project_brush_point_3d(rect, center).unwrap();
    assert!(matches!(
        workspace.pick_brush_handle_3d(rect, pointer),
        Some((0, BrushHandle3d::Face { face: 5, .. }))
    ));

    let mut frame = ToolFrame3d {
        rect,
        pointer_interact: Some(pointer),
        pointer_hover: Some(pointer),
        modifiers: egui::Modifiers::NONE,
        pointer_target: None,
        hover_room: None,
        drag_delta_y: 0.0,
    };
    tool_impl_3d(ViewTool::Brush).primary_pressed(&mut workspace, &frame);
    let drag = workspace.brush_extrude.clone().expect("face handle drag");
    assert!(drag.normal_3d.is_some());
    let end = pointer + drag.screen_direction * 24.0;
    frame.pointer_interact = Some(end);
    frame.pointer_hover = Some(end);
    tool_impl_3d(ViewTool::Brush).primary_dragged(&mut workspace, &frame);
    let applied = workspace.brush_extrude.as_ref().unwrap().applied;
    assert_ne!(applied, [0; 3]);
    assert_ne!(applied[0], 0, "slope movement must include X");
    assert_ne!(applied[1], 0, "slope movement must include Y");
    let moved = workspace.project.active_scene().brushes[0].clone();
    assert!(moved.solve().is_valid());
    assert_eq!(
        psxed_project::brush::Plane::from_points(moved.faces[5].points)
            .unwrap()
            .normal,
        psxed_project::brush::Plane::from_points(base.faces[5].points)
            .unwrap()
            .normal,
        "drag along the normal must preserve the authored plane orientation"
    );
    tool_impl_3d(ViewTool::Brush).primary_released(&mut workspace, &frame);
    assert!(workspace.is_dirty());
    workspace.do_undo();
    assert_eq!(workspace.project.active_scene().brushes[0], base);
}

/// Plane numeric fallback on a non-axis-aligned face: sliding along the
/// dominant axis translates the whole authored plane, preserving its
/// orientation, and rejects an edit that would stop enclosing volume.
#[test]
fn face_plane_numeric_edit_slides_a_non_axis_aligned_face() {
    let mut harness = ViewportHarness::floored_room("brush_numeric_slope", 4);
    harness.workspace.active_tool = ViewTool::Brush;
    let mut wedge = psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]);
    wedge.faces[5] =
        psxed_project::brush::BrushFace::from_points([[0, 128, 0], [0, 128, 128], [128, 0, 128]]);
    assert!(wedge.solve().is_valid());
    let normal_before = psxed_project::brush::Plane::from_points(wedge.faces[5].points)
        .unwrap()
        .normal;
    harness
        .workspace
        .project
        .active_scene_mut()
        .brushes
        .push(wedge);
    harness.workspace.selected_brush = Some(0);
    harness.workspace.selected_brush_face = Some(5);

    // The slope's dominant axis resolves to X with the authored reference
    // point at x = 0.
    assert_eq!(harness.workspace.selected_brush_face_axis(), Some((0, 0)));
    assert!(harness.workspace.set_selected_brush_face_axis_position(32));
    assert_eq!(harness.workspace.selected_brush_face_axis(), Some((0, 32)));
    let moved = harness.workspace.project.active_scene().brushes[0].clone();
    assert!(moved.solve().is_valid());
    let plane = psxed_project::brush::Plane::from_points(moved.faces[5].points).unwrap();
    assert_eq!(plane.normal, normal_before, "slide keeps the slope");
    // x + y = 160 after the +32 slide (normal components are 128 * 128).
    assert_eq!(plane.dist, 160 * plane.normal[0]);
    assert!(harness.workspace.is_dirty());

    // Sliding past the far side would invert the wedge: rejected, untouched.
    assert!(!harness
        .workspace
        .set_selected_brush_face_axis_position(-1000));
    assert_eq!(harness.workspace.project.active_scene().brushes[0], moved);
}

/// A 3D marquee sweeping two separated brushes selects both, through real
/// egui pointer dispatch.
#[test]
fn marquee_3d_selects_multiple_brushes_via_real_egui() {
    let mut project = ProjectDocument::new("3d multi-brush marquee");
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [-192, 0, -32],
            [-64, 64, 32],
        ));
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [64, 0, -32],
            [192, 64, 32],
        ));
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("marquee-3d-multi"), project);
    workspace.active_tool = ViewTool::Select;
    workspace.camera_rig.mode = ViewportCameraMode::Free;
    workspace.camera_rig.free_initialized = true;
    workspace.camera_rig.free_position = [0, 600, -1400];
    let (yaw, pitch) = camera_angles_to_look_at([0, 600, -1400], [0, 32, 0]).unwrap();
    workspace.camera_rig.free_yaw = yaw;
    workspace.camera_rig.free_pitch = pitch;
    let rect = Rect::from_center_size(Pos2::new(400.0, 300.0), Vec2::new(778.6667, 584.0));
    let left = workspace
        .project_brush_point_3d(rect, [-128.0, 32.0, 0.0])
        .unwrap();
    let right = workspace
        .project_brush_point_3d(rect, [128.0, 32.0, 0.0])
        .unwrap();

    // Start well outside both brushes' pick tolerance so Select begins a
    // marquee, then sweep a rect that covers both projected boxes (the
    // camera looks along +Z, so world +X projects left of centre).
    let min_x = left.x.min(right.x);
    let max_x = left.x.max(right.x);
    run_real_egui_viewport_plain_drag(
        &mut workspace,
        Pos2::new(min_x - 40.0, left.y - 60.0),
        Pos2::new(max_x + 40.0, right.y + 60.0),
    );

    assert_eq!(workspace.selected_brush_set(), vec![0, 1]);
    assert_eq!(workspace.status_text(), "Selected 2 brushes");
}

/// Two side-by-side brushes viewed head-on from -Z, with the Material Paint
/// tool armed on a BSP scene. Returns the workspace plus the screen points
/// of each brush's centre, so a caller can click or sweep across both.
fn bsp_face_paint_fixture(label: &str) -> (EditorWorkspace, ResourceId, ResourceId, Pos2, Pos2) {
    let mut project = ProjectDocument::new("bsp face paint");
    let stone = project.add_resource(
        "Stone",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let moss = project.add_resource(
        "Moss",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    for min_x in [-192, 64] {
        project
            .active_scene_mut()
            .brushes
            .push(psxed_project::brush::Brush::cuboid(
                [min_x, 0, -32],
                [min_x + 128, 64, 32],
            ));
    }
    let mut workspace = EditorWorkspace::with_project(test_temp_dir(label), project);
    workspace.active_workspace = WorkspaceView::Room;
    workspace.view_2d = false;
    workspace.set_active_tool_cycle_value((ViewTool::PaintMaterial, None));
    workspace.brush_material = Some(moss);
    workspace.camera_rig.mode = ViewportCameraMode::Free;
    workspace.camera_rig.free_initialized = true;
    workspace.camera_rig.free_position = [0, 600, -1400];
    let (yaw, pitch) = camera_angles_to_look_at([0, 600, -1400], [0, 32, 0]).unwrap();
    workspace.camera_rig.free_yaw = yaw;
    workspace.camera_rig.free_pitch = pitch;
    let rect = Rect::from_center_size(Pos2::new(400.0, 300.0), Vec2::new(778.6667, 584.0));
    let left = workspace
        .project_brush_point_3d(rect, [-128.0, 32.0, -32.0])
        .unwrap();
    let right = workspace
        .project_brush_point_3d(rect, [128.0, 32.0, -32.0])
        .unwrap();
    (workspace, stone, moss, left, right)
}

/// Material Paint in a BSP scene paints the brush face under the cursor.
/// A click paints exactly one face of one brush, and the eyedropper reads a
/// painted face back into the shared picker.
#[test]
fn material_paint_click_paints_one_bsp_brush_face_and_samples_it_back() {
    let (mut workspace, stone, moss, left, _right) = bsp_face_paint_fixture("bsp-face-paint-click");
    assert!(
        workspace.bsp_face_paint_active(),
        "Material Paint must address brush faces in a roomless brush scene"
    );
    let texture_id = egui::TextureId::default();
    let viewport = EditorViewport3dPresentation::edit(texture_id, Vec::new());

    run_real_egui_viewport_click(&mut workspace, left, viewport.clone());

    let painted: Vec<_> = workspace.project.active_scene().brushes[0]
        .faces
        .iter()
        .enumerate()
        .filter(|(_, face)| face.material.is_some())
        .collect();
    assert_eq!(painted.len(), 1, "exactly one face of brush 0 is painted");
    assert_eq!(painted[0].1.material, Some(moss));
    assert!(
        workspace.project.active_scene().brushes[1]
            .faces
            .iter()
            .all(|face| face.material.is_none()),
        "the brush that was not under the cursor stays untouched"
    );
    assert!(workspace.is_dirty());

    // One undo per gesture: a single click is one step.
    workspace.do_undo();
    assert!(workspace.project.active_scene().brushes[0]
        .faces
        .iter()
        .all(|face| face.material.is_none()));
    workspace.do_undo();
    assert_eq!(workspace.status, "Nothing to undo");
    workspace.do_redo();

    // The eyedropper reads the painted face back into the shared picker.
    workspace.brush_material = Some(stone);
    workspace.material_paint_sampling = true;
    run_real_egui_viewport_click(&mut workspace, left, viewport);
    assert_eq!(workspace.brush_material, Some(moss), "sampled the face");
    assert!(!workspace.material_paint_sampling, "eyedropper is one-shot");
}

/// A paint drag that sweeps across two BSP brushes paints both and costs
/// exactly one undo step, matching the "one undo per gesture" contract.
#[test]
fn material_paint_drag_across_bsp_brush_faces_is_one_undo_step() {
    let (mut workspace, _stone, moss, left, right) = bsp_face_paint_fixture("bsp-face-paint-drag");

    run_real_egui_viewport_plain_drag(&mut workspace, left, right);

    let painted = |workspace: &EditorWorkspace, brush: usize| {
        workspace.project.active_scene().brushes[brush]
            .faces
            .iter()
            .filter(|face| face.material == Some(moss))
            .count()
    };
    assert_eq!(painted(&workspace, 0), 1, "swept brush 0");
    assert_eq!(painted(&workspace, 1), 1, "swept brush 1");

    workspace.do_undo();
    assert_eq!(painted(&workspace, 0), 0);
    assert_eq!(painted(&workspace, 1), 0);
    workspace.do_undo();
    assert_eq!(
        workspace.status, "Nothing to undo",
        "a whole paint gesture must cost exactly one undo step"
    );
}

/// The Move/Resize/Edge/Vertex mode buttons stay visible and clickable
/// while the 3D viewport (not an orthographic view) is active.
#[test]
fn brush_mode_buttons_visible_and_clickable_with_3d_viewport_active() {
    let mut project = ProjectDocument::new("3d visible brush modes");
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [0, 0, 0],
            [128, 128, 128],
        ));
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("mode-buttons-3d"), project);
    workspace.active_workspace = WorkspaceView::Room;
    workspace.active_tool = ViewTool::Brush;
    workspace.view_2d = false;
    workspace.replace_brush_selection(0, None);

    // The helper asserts every mode button is present in the drawn frame
    // before clicking, so this proves visibility with the 3D viewport up.
    click_visible_brush_mode(&mut workspace, BrushEditMode::Vertex);
    assert_eq!(workspace.brush_edit_mode, BrushEditMode::Vertex);
}
