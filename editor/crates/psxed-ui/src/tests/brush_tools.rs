use super::*;
use crate::workspace::tools::{tool_impl_3d, BrushHandle3d, ToolFrame3d, BRUSH_CREATE_HEIGHT};
use psxed_project::brush::SolvedBrush;

fn polyline_grab_point(polyline: &[Pos2]) -> Pos2 {
    if polyline.len() == 2 {
        polyline[0] + (polyline[1] - polyline[0]) * 0.6
    } else {
        // Ring: a point partway around, away from the seam.
        polyline[polyline.len() / 6]
    }
}

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

fn draw_brush_3d(
    harness: &mut ViewportHarness,
    footprint_start: Pos2,
    footprint_end: Pos2,
    height_pixels: f32,
) {
    let tool = tool_impl_3d(ViewTool::Brush);
    let press = brush_frame(harness, footprint_start);
    let footprint = brush_frame(harness, footprint_end);
    tool.primary_pressed(&mut harness.workspace, &press);
    tool.primary_dragged(&mut harness.workspace, &footprint);
    tool.primary_released(&mut harness.workspace, &footprint);

    let height_end = Pos2::new(footprint_end.x, footprint_end.y - height_pixels);
    let height_press = brush_frame(harness, footprint_end);
    let height_drag = brush_frame(harness, height_end);
    tool.primary_pressed(&mut harness.workspace, &height_press);
    tool.primary_dragged(&mut harness.workspace, &height_drag);
    tool.primary_released(&mut harness.workspace, &height_drag);
}

#[test]
#[ignore = "developer performance benchmark over local editable E1M1"]
fn benchmark_e1m1_viewport_pointer_resolution() {
    let editor_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("editor root");
    let project_root = editor_root.join("projects/quake-e1m1-geometry");
    let project = ProjectDocument::load_from_path(project_root.join("project.ron"))
        .expect("E1M1 project loads");
    assert!(
        project.active_scene().brushes.len() >= 1_200,
        "benchmark must use the full editable E1M1 import"
    );
    let workspace = EditorWorkspace::with_project(project_root, project);
    let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_280.0, 720.0));
    let pointers: Vec<_> = (0..5)
        .flat_map(|y| {
            (0..9).map(move |x| {
                Pos2::new(
                    64.0 + x as f32 * ((1_280.0 - 128.0) / 8.0),
                    64.0 + y as f32 * ((720.0 - 128.0) / 4.0),
                )
            })
        })
        .collect();

    for &pointer in &pointers {
        std::hint::black_box(
            workspace.resolve_viewport_3d_pointer_target(rect, pointer, None, true),
        );
    }
    let repetitions = 100;
    let started = std::time::Instant::now();
    let mut targets = 0_usize;
    for _ in 0..repetitions {
        for &pointer in &pointers {
            targets += workspace
                .resolve_viewport_3d_pointer_target(rect, pointer, None, true)
                .is_some() as usize;
        }
    }
    let calls = repetitions * pointers.len();
    let elapsed = started.elapsed();
    println!(
        "E1M1 viewport pointer resolution: brushes={}, calls={}, targets={}, total={:?}, mean={:.3}us",
        workspace.project.active_scene().brushes.len(),
        calls,
        targets,
        elapsed,
        elapsed.as_secs_f64() * 1_000_000.0 / calls as f64,
    );
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
    assert!(
        workspace.brush_vertex_drag.is_some()
            || workspace.brush_extrude.is_some()
            || workspace.brush_element_transform.is_some(),
        "press started a brush gesture"
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

fn run_real_egui_vertex_snap_drag(workspace: &mut EditorWorkspace, start: Pos2, end: Pos2) {
    let ctx = egui::Context::default();
    let texture = ctx.load_texture(
        "3d-vertex-snap-viewport",
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
    let key_event = |pressed| egui::Event::Key {
        key: egui::Key::B,
        physical_key: Some(egui::Key::B),
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };

    let _ = ctx.run(input(0.0, vec![]), |ctx| draw(ctx, workspace));
    let _ = ctx.run(
        input(
            1.0 / 60.0,
            vec![egui::Event::PointerMoved(start), key_event(true)],
        ),
        |ctx| draw(ctx, workspace),
    );
    assert!(workspace.brush_vertex_snap_hover.is_some());
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
    assert!(
        workspace
            .brush_vertex_drag
            .as_ref()
            .is_some_and(|drag| drag.snap_source.is_some()),
        "held B starts the dedicated vertex-snap drag"
    );
    let _ = ctx.run(
        input(
            4.0 / 60.0,
            vec![key_event(false), egui::Event::PointerMoved(end)],
        ),
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
pub(super) fn text_shape_centers(shapes: &[egui::epaint::ClippedShape], label: &str) -> Vec<Pos2> {
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

/// Drive an always-visible BSP mode button by finding its rendered icon, then
/// issuing real egui pointer events at that icon's screen position.
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
    let select_y = text_shape_centers(&output.shapes, &icons::POINTER.to_string())
        .into_iter()
        .min_by(|a, b| a.y.total_cmp(&b.y))
        .expect("flat BSP toolbar omitted Select")
        .y;
    for toolbar_mode in BspToolbarMode::ALL {
        assert!(
            text_shape_centers(&output.shapes, &toolbar_mode.icon().to_string())
                .into_iter()
                .any(|point| (point.y - select_y).abs() < 2.0),
            "visible BSP toolbar omitted {:?}",
            toolbar_mode
        );
    }
    let toolbar_mode = match mode {
        BrushEditMode::Move => BspToolbarMode::Select,
        BrushEditMode::Face => BspToolbarMode::Face,
        BrushEditMode::Edge => BspToolbarMode::Edge,
        BrushEditMode::Vertex => BspToolbarMode::Vertex,
        BrushEditMode::Clip => BspToolbarMode::Clip,
    };
    let point = text_shape_centers(&output.shapes, &toolbar_mode.icon().to_string())
        .into_iter()
        .filter(|point| (point.y - select_y).abs() < 2.0)
        .min_by(|a, b| a.x.total_cmp(&b.x))
        .expect("requested BSP toolbar mode icon");
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
    let after = ctx.run(input(3.0 / 60.0, vec![]), |ctx| {
        workspace.draw(ctx, viewport.clone(), EditorPlaytestStatus::Idle)
    });
    assert_eq!(
        text_shape_center(&after.shapes, "Extrude").is_some(),
        mode == BrushEditMode::Face,
        "the Extrude button shows exactly in Face mode"
    );
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
            if mode == BrushEditMode::Clip {
                // Clip is click-driven; it has no drag gesture to test here.
                continue;
            }
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
                BrushEditMode::Clip => unreachable!("skipped above"),
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
fn numeric_size_scales_a_multi_brush_selection_as_one_object() {
    let mut project = ProjectDocument::new("numeric multi-brush scale");
    let originals = [
        psxed_project::brush::Brush::cuboid([0, 0, 0], [64, 64, 64]),
        psxed_project::brush::Brush::cuboid([128, 0, 0], [192, 64, 64]),
    ];
    project
        .active_scene_mut()
        .brushes
        .extend(originals.iter().cloned());
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("numeric-multi-scale"), project);
    workspace.replace_brush_selection(0, None);
    workspace.toggle_brush_selection(1);

    assert_eq!(workspace.selected_brush_origin(), Some([0, 0, 0]));
    assert_eq!(workspace.selected_brush_size(), Some([192, 64, 64]));
    workspace.push_undo();
    assert!(workspace.set_selected_brush_size([384, 128, 64]));
    assert_eq!(workspace.selected_brush_size(), Some([384, 128, 64]));
    let first = workspace.project.active_scene().brushes[0].solve();
    let second = workspace.project.active_scene().brushes[1].solve();
    assert_eq!((first.min, first.max), ([0.0; 3], [128.0, 128.0, 64.0]));
    assert_eq!(
        (second.min, second.max),
        ([256.0, 0.0, 0.0], [384.0, 128.0, 64.0])
    );

    workspace.do_undo();
    assert_eq!(workspace.project.active_scene().brushes, originals);
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
fn brush_and_select_tools_require_the_gizmo_for_whole_brush_moves_in_3d() {
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
        assert_eq!(
            harness.workspace.project.active_scene().brushes[0],
            base,
            "{tool:?}: body drag must not move the selected brush"
        );
        assert!(!harness.workspace.is_dirty(), "{tool:?}");
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
fn bsp_marquee_selects_faces_edges_and_vertices_in_orthographic_view_via_real_egui() {
    for (mode, expected) in [
        (BrushEditMode::Face, 6),
        (BrushEditMode::Edge, 12),
        (BrushEditMode::Vertex, 8),
    ] {
        let mut project = ProjectDocument::new("2d BSP element marquee");
        project
            .active_scene_mut()
            .brushes
            .push(psxed_project::brush::Brush::cuboid(
                [0, 0, 0],
                [512, 512, 512],
            ));
        let mut workspace =
            EditorWorkspace::with_project(test_temp_dir("2d-element-marquee"), project);
        workspace.active_workspace = WorkspaceView::Room;
        workspace.view_2d = true;
        workspace.orthographic_view = OrthographicView::Top;
        workspace.active_tool = ViewTool::Select;
        workspace.replace_brush_selection(0, None);
        workspace.set_brush_edit_mode(mode);
        workspace.frame_viewport();

        // Begin clear of the silhouette and sweep every displayed handle.
        run_real_egui_orthographic_drag(
            &mut workspace,
            [-32.0, -32.0],
            [544.0, 544.0],
            egui::Modifiers::NONE,
        );

        assert_eq!(workspace.selected_brush, Some(0), "{mode:?}");
        assert_eq!(
            workspace.selected_brush_elements.len(),
            expected,
            "{mode:?}"
        );
        assert!(
            workspace
                .selected_brush_elements
                .iter()
                .all(|element| matches!(
                    (mode, element),
                    (BrushEditMode::Face, BrushElement::Face(_))
                        | (BrushEditMode::Edge, BrushElement::Edge(..))
                        | (BrushEditMode::Vertex, BrushElement::Vertex(_))
                )),
            "{mode:?}: {:?}",
            workspace.selected_brush_elements
        );
        if mode == BrushEditMode::Face {
            assert_eq!(workspace.selected_brush_faces.len(), expected);
        }
    }
}

#[test]
fn tracked_bsp_starter_top_view_emits_headless_brush_outline_shapes() {
    let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../archive/fixtures/brush-first-playable");
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
    let (mut workspace, rect) = handle_test_workspace(wedge);
    workspace.brush_edit_mode = BrushEditMode::Face;
    workspace.selection_mode = SelectionMode::Face;
    // Load-time normalization prunes the wedge's dead plane, shifting
    // indices: find the slant (the one non-axis-aligned plane).
    let base = workspace.project.active_scene().brushes[0].clone();
    let slant = base
        .faces
        .iter()
        .position(|face| {
            let normal = psxed_project::brush::Plane::from_points(face.points)
                .unwrap()
                .normal;
            (0..3).filter(|&axis| normal[axis] != 0).count() >= 2
        })
        .expect("wedge keeps its slanted plane");
    let (center, _) = EditorWorkspace::face_center_and_normal(&base, slant).unwrap();
    let pointer = workspace.project_brush_point_3d(rect, center).unwrap();
    let picked = workspace.pick_brush_handle_3d(rect, pointer);
    assert!(
        matches!(picked, Some((0, BrushHandle3d::Face { face, .. })) if face == slant),
        "picked {picked:?}, expected face {slant}"
    );

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
        psxed_project::brush::Plane::from_points(moved.faces[slant].points)
            .unwrap()
            .normal,
        psxed_project::brush::Plane::from_points(base.faces[slant].points)
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

fn vertex_snap_test_workspace() -> (EditorWorkspace, Rect, [f64; 3], [f64; 3]) {
    let moving = psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]);
    let target = psxed_project::brush::Brush::cuboid([384, 0, 0], [512, 128, 128]);
    let mut project = ProjectDocument::new("Godot-style vertex snap");
    project.active_scene_mut().brushes.extend([moving, target]);
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("godot-vertex-snap"), project);
    workspace.active_tool = ViewTool::Select;
    workspace.replace_brush_selection(0, None);
    workspace.set_brush_edit_mode(BrushEditMode::Move);
    workspace.snap_units = 16;
    workspace.camera_rig.mode = ViewportCameraMode::Free;
    workspace.camera_rig.free_initialized = true;
    workspace.camera_rig.free_position = [720, 480, -900];
    let (yaw, pitch) = camera_angles_to_look_at([720, 480, -900], [256, 64, 64]).unwrap();
    workspace.camera_rig.free_yaw = yaw;
    workspace.camera_rig.free_pitch = pitch;
    (
        workspace,
        Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0)),
        [128.0, 0.0, 0.0],
        [384.0, 0.0, 0.0],
    )
}

#[test]
fn godot_style_vertex_snap_aligns_a_brush_corner_and_undoes_once() {
    let (mut workspace, rect, source, target) = vertex_snap_test_workspace();
    let base = workspace.project.active_scene().brushes[0].clone();
    let source_screen = workspace.project_brush_point_3d(rect, source).unwrap();
    let target_screen = workspace.project_brush_point_3d(rect, target).unwrap();
    workspace.brush_vertex_snap_key_down = true;
    workspace.update_brush_vertex_snap_hover_3d(rect, Some(source_screen));
    assert_eq!(workspace.brush_vertex_snap_hover, Some(source));

    let tool = tool_impl_3d(ViewTool::Select);
    let mut frame = element_click_frame(rect, source_screen, egui::Modifiers::NONE);
    tool.primary_pressed(&mut workspace, &frame);
    assert_eq!(
        workspace
            .brush_vertex_drag
            .as_ref()
            .and_then(|drag| drag.snap_source),
        Some(source)
    );

    frame.pointer_interact = Some(target_screen);
    frame.pointer_hover = Some(target_screen);
    tool.primary_dragged(&mut workspace, &frame);
    let drag = workspace.brush_vertex_drag.as_ref().unwrap();
    assert_eq!(drag.snap_target, Some(target));
    assert_eq!(drag.applied, [256, 0, 0]);
    let moved_vertices = crate::workspace::brush_elements::unique_vertices(
        &workspace.project.active_scene().brushes[0].solve(),
    );
    assert!(moved_vertices.contains(&target));

    tool.primary_released(&mut workspace, &frame);
    assert!(workspace.is_dirty());
    workspace.do_undo();
    assert_eq!(workspace.project.active_scene().brushes[0], base);
}

#[test]
fn vertex_snap_excludes_the_moving_set_and_moves_multiselection_rigidly() {
    let brushes = [
        psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]),
        psxed_project::brush::Brush::cuboid([384, 0, 0], [512, 128, 128]),
        psxed_project::brush::Brush::cuboid([768, 0, 0], [896, 128, 128]),
    ];
    let mut project = ProjectDocument::new("multi brush vertex snap");
    project.active_scene_mut().brushes.extend(brushes.clone());
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("multi-vertex-snap"), project);
    workspace.active_tool = ViewTool::Select;
    workspace.replace_brush_selection(0, None);
    workspace.selected_brushes = vec![0, 1];
    workspace.set_brush_edit_mode(BrushEditMode::Move);
    workspace.camera_rig.mode = ViewportCameraMode::Free;
    workspace.camera_rig.free_initialized = true;
    workspace.camera_rig.free_position = [1050, 560, -1450];
    let (yaw, pitch) = camera_angles_to_look_at([1050, 560, -1450], [448, 64, 64]).unwrap();
    workspace.camera_rig.free_yaw = yaw;
    workspace.camera_rig.free_pitch = pitch;
    let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let source = [128.0, 0.0, 0.0];
    let selected_neighbor = [384.0, 0.0, 0.0];
    let external_target = [768.0, 0.0, 0.0];
    let source_screen = workspace.project_brush_point_3d(rect, source).unwrap();
    workspace.brush_vertex_snap_key_down = true;
    workspace.update_brush_vertex_snap_hover_3d(rect, Some(source_screen));
    let tool = tool_impl_3d(ViewTool::Select);
    let mut frame = element_click_frame(rect, source_screen, egui::Modifiers::NONE);
    tool.primary_pressed(&mut workspace, &frame);

    let selected_neighbor_screen = workspace
        .project_brush_point_3d(rect, selected_neighbor)
        .unwrap();
    frame.pointer_interact = Some(selected_neighbor_screen);
    frame.pointer_hover = Some(selected_neighbor_screen);
    tool.primary_dragged(&mut workspace, &frame);
    assert_eq!(
        workspace.brush_vertex_drag.as_ref().unwrap().snap_target,
        None,
        "vertices on every moving brush must be excluded as snap targets"
    );

    let target_screen = workspace
        .project_brush_point_3d(rect, external_target)
        .unwrap();
    frame.pointer_interact = Some(target_screen);
    frame.pointer_hover = Some(target_screen);
    tool.primary_dragged(&mut workspace, &frame);
    let drag = workspace.brush_vertex_drag.as_ref().unwrap();
    assert_eq!(drag.snap_target, Some(external_target));
    assert_eq!(drag.applied, [640, 0, 0]);
    assert_eq!(
        workspace.project.active_scene().brushes[1].solve().min,
        [1024.0, 0.0, 0.0],
        "the other selected brush rides the exact same snap delta"
    );

    tool.primary_released(&mut workspace, &frame);
    workspace.do_undo();
    assert_eq!(workspace.project.active_scene().brushes, brushes);
}

#[test]
fn held_b_vertex_snap_runs_through_real_egui_input() {
    let (mut workspace, _, source, target) = vertex_snap_test_workspace();
    // `draw_viewport_3d_body` allocates this centred canvas inside an
    // 800x600 CentralPanel (the same stable dimensions used by the existing
    // real-egui handle tests).
    let viewport = Rect::from_center_size(Pos2::new(400.0, 300.0), Vec2::new(778.6667, 584.0));
    let source_screen = workspace.project_brush_point_3d(viewport, source).unwrap();
    let target_screen = workspace.project_brush_point_3d(viewport, target).unwrap();

    run_real_egui_vertex_snap_drag(&mut workspace, source_screen, target_screen);

    assert!(workspace.is_dirty());
    assert!(crate::workspace::brush_elements::unique_vertices(
        &workspace.project.active_scene().brushes[0].solve()
    )
    .contains(&target));
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
fn element_gizmo_drag_runs_through_real_egui_in_select_mode() {
    let brush = psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]);
    let (mut workspace, _) = handle_test_workspace(brush.clone());
    workspace.active_tool = ViewTool::Select;
    workspace.set_brush_edit_mode(BrushEditMode::Vertex);
    let viewport = Rect::from_center_size(Pos2::new(400.0, 300.0), Vec2::new(778.6667, 584.0));
    let vertex = crate::workspace::brush_elements::unique_vertices(&brush.solve())[0];
    workspace.apply_brush_element_selection(
        BrushElement::Vertex(crate::workspace::brush_elements::quantize_element_point(
            vertex,
        )),
        egui::Modifiers::NONE,
    );
    let polylines = workspace
        .brush_element_gizmo_polylines_3d(viewport)
        .expect("gizmo axes project");
    let start = polyline_grab_point(&polylines[1]);
    assert_eq!(
        workspace.pick_brush_element_gizmo_axis_3d(viewport, start),
        Some(1),
        "grab point picks the Y axis"
    );

    run_real_egui_viewport_drag(&mut workspace, start, start + Vec2::new(0.0, -48.0));

    assert!(workspace.is_dirty(), "gizmo drag must commit");
    assert_ne!(workspace.project.active_scene().brushes[0], brush);
}

#[test]
fn element_gizmo_rotates_and_scales_faces_with_the_transform_group() {
    let build = || {
        let mut project = ProjectDocument::new("element-rotate-scale");
        project
            .active_scene_mut()
            .brushes
            .push(psxed_project::brush::Brush::cuboid(
                [0, 0, 0],
                [512, 256, 256],
            ));
        let mut workspace =
            EditorWorkspace::with_project(test_temp_dir("element-rotate-scale"), project);
        workspace.active_tool = ViewTool::Select;
        workspace.camera_rig.mode = ViewportCameraMode::Free;
        workspace.camera_rig.free_initialized = true;
        workspace.camera_rig.free_position = [1400, 1200, -1400];
        let (yaw, pitch) = camera_angles_to_look_at([1400, 1200, -1400], [256, 128, 128]).unwrap();
        workspace.camera_rig.free_yaw = yaw;
        workspace.camera_rig.free_pitch = pitch;
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        workspace.replace_brush_selection(0, None);
        workspace.set_brush_edit_mode(BrushEditMode::Face);
        // Top face of the cuboid (+Y is authored face 5).
        workspace.apply_brush_element_selection(BrushElement::Face(5), egui::Modifiers::NONE);
        (workspace, rect)
    };
    let has_vertex = |workspace: &EditorWorkspace, expect: [f64; 3]| {
        crate::workspace::brush_elements::unique_vertices(
            &workspace.project.active_scene().brushes[0].solve(),
        )
        .iter()
        .any(|vertex| (0..3).all(|axis| (vertex[axis] - expect[axis]).abs() <= 1.0))
    };
    let tool = tool_impl_3d(ViewTool::Select);

    // Rotate 90 degrees about Y: the pointer sweeps a quarter turn
    // around the projected centroid. Top corner (0,256,0) orbits the
    // face centroid (256,256,128) to (128,256,384) (or its mirror,
    // depending on sweep sign; the applied magnitude pins it).
    let (mut workspace, rect) = build();
    workspace.set_transform_gizmo_mode(TransformGizmoMode::Rotate);
    let polylines = workspace.brush_element_gizmo_polylines_3d(rect).unwrap();
    let grab = polyline_grab_point(&polylines[1]);
    tool.primary_pressed(
        &mut workspace,
        &element_click_frame(rect, grab, egui::Modifiers::NONE),
    );
    assert!(
        workspace.brush_element_transform.is_some(),
        "rotate grab starts"
    );
    let center = workspace
        .brush_element_transform
        .as_ref()
        .unwrap()
        .center_screen;
    let radial = grab - center;
    // Quarter-turn sweep: rotate the pointer offset 90 degrees around
    // the centre (screen-space).
    let swept = center + Vec2::new(-radial.y, radial.x);
    tool.primary_dragged(
        &mut workspace,
        &element_click_frame(rect, swept, egui::Modifiers::NONE),
    );
    assert_eq!(
        workspace
            .brush_element_transform
            .as_ref()
            .unwrap()
            .applied
            .abs(),
        90
    );
    tool.primary_released(
        &mut workspace,
        &element_click_frame(rect, swept, egui::Modifiers::NONE),
    );
    assert!(
        has_vertex(&workspace, [128.0, 256.0, 384.0])
            || has_vertex(&workspace, [384.0, 256.0, -128.0]),
        "corner orbited a quarter turn either way"
    );
    workspace.do_undo();
    assert!(has_vertex(&workspace, [0.0, 256.0, 0.0]), "undo restores");

    // Scale +50% along X (128 px of travel ALONG the axis's screen
    // direction): corner (0,256,0) stretches to (-128,256,0) about the
    // centroid; the floor stays put.
    let (mut workspace, rect) = build();
    workspace.set_transform_gizmo_mode(TransformGizmoMode::Scale);
    let polylines = workspace.brush_element_gizmo_polylines_3d(rect).unwrap();
    let grab = polyline_grab_point(&polylines[0]);
    tool.primary_pressed(
        &mut workspace,
        &element_click_frame(rect, grab, egui::Modifiers::NONE),
    );
    assert!(
        workspace.brush_element_transform.is_some(),
        "scale grab starts"
    );
    let screen_axis = workspace
        .brush_element_transform
        .as_ref()
        .unwrap()
        .screen_axis;
    let swept = grab + screen_axis * 128.0;
    tool.primary_dragged(
        &mut workspace,
        &element_click_frame(rect, swept, egui::Modifiers::NONE),
    );
    assert_eq!(
        workspace.brush_element_transform.as_ref().unwrap().applied,
        50
    );
    tool.primary_released(
        &mut workspace,
        &element_click_frame(rect, swept, egui::Modifiers::NONE),
    );
    assert!(
        has_vertex(&workspace, [-128.0, 256.0, 0.0]),
        "scaled corner"
    );
    assert!(has_vertex(&workspace, [0.0, 0.0, 0.0]), "floor untouched");
}

#[test]
fn whole_brush_gizmo_rotates_and_scales_the_full_multi_selection() {
    let build = || {
        let originals = vec![
            psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]),
            psxed_project::brush::Brush::cuboid([256, 0, 0], [384, 128, 128]),
        ];
        let mut project = ProjectDocument::new("multi-brush rotate scale");
        project
            .active_scene_mut()
            .brushes
            .extend(originals.iter().cloned());
        let mut workspace =
            EditorWorkspace::with_project(test_temp_dir("multi-brush-gizmo"), project);
        workspace.active_tool = ViewTool::Select;
        workspace.set_brush_edit_mode(BrushEditMode::Move);
        workspace.camera_rig.mode = ViewportCameraMode::Free;
        workspace.camera_rig.free_initialized = true;
        workspace.camera_rig.free_position = [1200, 900, -1200];
        let (yaw, pitch) = camera_angles_to_look_at([1200, 900, -1200], [192, 64, 64]).unwrap();
        workspace.camera_rig.free_yaw = yaw;
        workspace.camera_rig.free_pitch = pitch;
        workspace.replace_brush_selection(0, None);
        workspace.toggle_brush_selection(1);
        (
            workspace,
            originals,
            Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0)),
        )
    };
    let tool = tool_impl_3d(ViewTool::Select);

    let (mut workspace, originals, rect) = build();
    let (pivot, _, _, _) = workspace.brush_gizmo_context().unwrap();
    assert_eq!(pivot, [192.0, 64.0, 64.0]);
    workspace.set_transform_gizmo_mode(TransformGizmoMode::Scale);
    let polylines = workspace.brush_element_gizmo_polylines_3d(rect).unwrap();
    let grab = polyline_grab_point(&polylines[0]);
    tool.primary_pressed(
        &mut workspace,
        &element_click_frame(rect, grab, egui::Modifiers::NONE),
    );
    let drag = workspace
        .brush_element_transform
        .as_ref()
        .expect("scale starts");
    assert_eq!(drag.others.len(), 1, "the second brush rides the gesture");
    let swept = grab + drag.screen_axis * 128.0;
    tool.primary_dragged(
        &mut workspace,
        &element_click_frame(rect, swept, egui::Modifiers::NONE),
    );
    tool.primary_released(
        &mut workspace,
        &element_click_frame(rect, swept, egui::Modifiers::NONE),
    );
    let first = workspace.project.active_scene().brushes[0].solve();
    let second = workspace.project.active_scene().brushes[1].solve();
    assert_eq!((first.min[0], first.max[0]), (-96.0, 96.0));
    assert_eq!((second.min[0], second.max[0]), (288.0, 480.0));
    workspace.do_undo();
    assert_eq!(workspace.project.active_scene().brushes, originals);

    let (mut workspace, originals, rect) = build();
    workspace.set_transform_gizmo_mode(TransformGizmoMode::Rotate);
    let polylines = workspace.brush_element_gizmo_polylines_3d(rect).unwrap();
    let grab = polyline_grab_point(&polylines[1]);
    tool.primary_pressed(
        &mut workspace,
        &element_click_frame(rect, grab, egui::Modifiers::NONE),
    );
    let drag = workspace
        .brush_element_transform
        .as_ref()
        .expect("rotate starts");
    assert_eq!(drag.others.len(), 1, "the second brush rides the gesture");
    let radial = grab - drag.center_screen;
    let swept = drag.center_screen + Vec2::new(-radial.y, radial.x);
    tool.primary_dragged(
        &mut workspace,
        &element_click_frame(rect, swept, egui::Modifiers::NONE),
    );
    tool.primary_released(
        &mut workspace,
        &element_click_frame(rect, swept, egui::Modifiers::NONE),
    );
    assert_ne!(workspace.project.active_scene().brushes[0], originals[0]);
    assert_ne!(workspace.project.active_scene().brushes[1], originals[1]);
    let centers: Vec<_> = workspace
        .project
        .active_scene()
        .brushes
        .iter()
        .map(|brush| {
            let solved = brush.solve();
            [
                (solved.min[0] + solved.max[0]) * 0.5,
                (solved.min[1] + solved.max[1]) * 0.5,
                (solved.min[2] + solved.max[2]) * 0.5,
            ]
        })
        .collect();
    assert!(centers
        .iter()
        .all(|center| (center[0] - 192.0).abs() <= 1.0));
    assert!((centers[0][2] - centers[1][2]).abs() >= 255.0);
    workspace.do_undo();
    assert_eq!(workspace.project.active_scene().brushes, originals);
}

#[test]
fn element_gizmo_rotation_and_scale_snap_every_vertex_to_active_grid() {
    let exercise = |rotate: bool| {
        let base = psxed_project::brush::Brush::cuboid([0, 0, 0], [256, 192, 128]);
        let mut project = ProjectDocument::new("quantized element transform");
        project.active_scene_mut().brushes.push(base.clone());
        let mut workspace =
            EditorWorkspace::with_project(test_temp_dir("quantized-transform"), project);
        workspace.snap_units = 32;
        workspace.replace_brush_selection(0, None);

        let start_pointer = if rotate {
            Pos2::new(100.0, 0.0)
        } else {
            Pos2::new(0.0, 0.0)
        };
        workspace.brush_element_transform = Some(crate::BrushElementTransformDrag {
            index: 0,
            base: base.clone(),
            others: Vec::new(),
            element_others: Vec::new(),
            targets: Vec::new(),
            center: [128.0, 96.0, 64.0],
            axis: if rotate { 1 } else { 0 },
            rotate,
            grid_safe_rotation: rotate,
            rotation_snap_degrees: if rotate {
                BRUSH_ROTATION_SNAP_DEGREES
            } else {
                5
            },
            faces: (0..base.faces.len()).collect(),
            start_pointer,
            screen_axis: Vec2::RIGHT,
            center_screen: Pos2::new(0.0, 0.0),
            start_angle: 0.0,
            applied: 0,
        });

        let pointer = if rotate {
            // BSP brush rotation is deliberately lattice-safe: once the
            // pointer crosses the half-turn threshold it snaps to 90°.
            let angle = 70.0_f32.to_radians();
            Pos2::new(angle.cos() * 100.0, angle.sin() * 100.0)
        } else {
            // 2.56 px per percent: this is a deliberately off-grid 15%
            // scale before the active 32-unit quantisation is applied.
            Pos2::new(38.4, 0.0)
        };
        workspace.update_brush_element_transform(pointer, false);
        assert_eq!(
            workspace.brush_element_transform.as_ref().unwrap().applied,
            if rotate { 90 } else { 15 }
        );
        if rotate {
            assert!(
                workspace.status.contains("90°"),
                "live rotation feedback includes the snapped angle: {}",
                workspace.status
            );
        }
        assert!(workspace.commit_brush_element_transform());

        let transformed = workspace.project.active_scene().brushes[0].clone();
        assert_ne!(transformed, base);
        assert!(transformed.solve().is_valid());
        transformed
    };

    for (label, brush) in [("rotation", exercise(true)), ("scale", exercise(false))] {
        for face in &brush.faces {
            for point in face.points {
                for coordinate in point {
                    assert_eq!(
                        coordinate.rem_euclid(32),
                        0,
                        "{label} left authored point {point:?} off the 32-unit grid"
                    );
                }
            }
        }
        for vertex in crate::workspace::brush_elements::unique_vertices(&brush.solve()) {
            for coordinate in vertex {
                let snapped = (coordinate / 32.0).round() * 32.0;
                assert!(
                    (coordinate - snapped).abs() <= 0.01,
                    "{label} left solved vertex {vertex:?} off the 32-unit grid"
                );
            }
        }
    }
}

#[test]
fn face_element_gizmo_drag_moves_the_face_via_real_egui() {
    let brush = psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]);
    let (mut workspace, _) = handle_test_workspace(brush.clone());
    workspace.active_tool = ViewTool::Select;
    workspace.set_brush_edit_mode(BrushEditMode::Face);
    let viewport = Rect::from_center_size(Pos2::new(400.0, 300.0), Vec2::new(778.6667, 584.0));
    workspace.apply_brush_element_selection(BrushElement::Face(5), egui::Modifiers::NONE);
    let polylines = workspace
        .brush_element_gizmo_polylines_3d(viewport)
        .expect("gizmo axes project");
    let start = polyline_grab_point(&polylines[1]);
    assert_eq!(
        workspace.pick_brush_element_gizmo_axis_3d(viewport, start),
        Some(1)
    );

    run_real_egui_viewport_drag(&mut workspace, start, start + Vec2::new(0.0, -48.0));

    assert!(workspace.is_dirty(), "face gizmo drag must commit");
    assert_ne!(workspace.project.active_scene().brushes[0], brush);
    assert!(workspace.project.active_scene().brushes[0]
        .solve()
        .is_valid());
}

#[test]
fn face_element_gizmo_moves_the_full_document_wide_selection() {
    let originals = vec![
        psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]),
        psxed_project::brush::Brush::cuboid([256, 0, 0], [384, 128, 128]),
    ];
    let mut project = ProjectDocument::new("multi-brush face gizmo");
    project
        .active_scene_mut()
        .brushes
        .extend(originals.iter().cloned());
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("multi-brush-face-gizmo"), project);
    workspace.active_tool = ViewTool::Select;
    workspace.camera_rig.mode = ViewportCameraMode::Free;
    workspace.camera_rig.free_initialized = true;
    workspace.camera_rig.free_position = [1200, 900, -1200];
    let (yaw, pitch) = camera_angles_to_look_at([1200, 900, -1200], [192, 128, 64]).unwrap();
    workspace.camera_rig.free_yaw = yaw;
    workspace.camera_rig.free_pitch = pitch;
    workspace.set_brush_edit_mode(BrushEditMode::Face);
    workspace.selected_brush = Some(1);
    workspace.selected_brushes = vec![0, 1];
    workspace.selected_brush_face = Some(5);
    workspace.selected_brush_faces = vec![(0, 5), (1, 5)];
    workspace.selected_brush_elements = vec![BrushElement::Face(5)];

    let viewport = Rect::from_center_size(Pos2::new(400.0, 300.0), Vec2::new(778.6667, 584.0));
    let (pivot, _, _, companions) = workspace.brush_gizmo_context().expect("shared gizmo");
    assert_eq!(companions.len(), 1, "the earlier face remains a target");
    assert!((pivot[0] - 192.0).abs() <= 0.5, "shared pivot: {pivot:?}");
    let polylines = workspace
        .brush_element_gizmo_polylines_3d(viewport)
        .expect("gizmo axes project");
    let start = polyline_grab_point(&polylines[1]);
    run_real_egui_viewport_drag(&mut workspace, start, start + Vec2::new(0.0, -48.0));

    assert!(workspace.is_dirty(), "shared face gizmo drag must commit");
    for (index, original) in originals.iter().enumerate() {
        let moved = &workspace.project.active_scene().brushes[index];
        assert_ne!(moved, original, "selected face on brush {index} moved");
        assert!(moved.solve().is_valid(), "brush {index} remains valid");
    }
    workspace.do_undo();
    assert_eq!(workspace.project.active_scene().brushes, originals);
}

#[test]
fn element_gizmo_moves_every_selected_edge_and_vertex() {
    for mode in [BrushEditMode::Edge, BrushEditMode::Vertex] {
        let original = psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]);
        let (mut workspace, _) = handle_test_workspace(original.clone());
        workspace.active_tool = ViewTool::Select;
        workspace.set_brush_edit_mode(mode);
        let top = original.solve().polygons[5].clone().expect("top polygon");
        workspace.selected_brush_elements = match mode {
            BrushEditMode::Vertex => top
                .verts
                .iter()
                .map(|vertex| {
                    BrushElement::Vertex(crate::workspace::brush_elements::quantize_element_point(
                        *vertex,
                    ))
                })
                .collect(),
            BrushEditMode::Edge => (0..top.verts.len())
                .map(|edge| {
                    let (a, b) = crate::workspace::brush_elements::edge_element_key(
                        top.verts[edge],
                        top.verts[(edge + 1) % top.verts.len()],
                    );
                    BrushElement::Edge(a, b)
                })
                .collect(),
            _ => unreachable!(),
        };

        let viewport = Rect::from_center_size(Pos2::new(400.0, 300.0), Vec2::new(778.6667, 584.0));
        let (_, targets, _, companions) = workspace.brush_gizmo_context().expect("component gizmo");
        assert_eq!(targets.len(), 4, "{mode:?} includes the full top loop");
        assert!(companions.is_empty());
        let polylines = workspace
            .brush_element_gizmo_polylines_3d(viewport)
            .expect("gizmo axes project");
        let start = polyline_grab_point(&polylines[1]);
        run_real_egui_viewport_drag(&mut workspace, start, start + Vec2::new(0.0, -48.0));

        let moved = workspace.project.active_scene().brushes[0].solve();
        assert!(workspace.is_dirty(), "{mode:?} gizmo drag commits");
        assert!(moved.max[1] > 128.0, "{mode:?} raises every top component");
        assert_eq!(moved.min[1], 0.0, "{mode:?} leaves the floor untouched");
        workspace.do_undo();
        assert_eq!(workspace.project.active_scene().brushes[0], original);
    }
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

    assert_eq!(
        harness.workspace.project.active_scene().brushes.len(),
        0,
        "the first release only locks the footprint"
    );
    assert!(harness
        .workspace
        .brush_drag
        .is_some_and(|drag| { drag.stage == BrushCreateStage::Height && !drag.height_dragging }));

    // A second vertical gesture authors Y and commits. Dragging upward by
    // 32 px adds 256 world units to the initial 256-unit preview.
    let height_press = brush_frame(&harness, Pos2::new(500.0, 400.0));
    let height_drag = brush_frame(&harness, Pos2::new(500.0, 368.0));
    tool.primary_pressed(&mut harness.workspace, &height_press);
    tool.primary_dragged(&mut harness.workspace, &height_drag);
    tool.primary_released(&mut harness.workspace, &height_drag);

    let scene = harness.workspace.project.active_scene();
    assert_eq!(scene.brushes.len(), 1, "height release commits the brush");
    let solved = scene.brushes[0].solve();
    assert!(solved.is_valid());
    assert_eq!(solved.min[1], 0.0);
    assert_eq!(solved.max[1], 512.0);
    assert!(solved.max[0] > solved.min[0]);
    assert!(solved.max[2] > solved.min[2]);
    assert_eq!(
        harness.workspace.selected_brush,
        Some(0),
        "new brush is selected"
    );
    assert!(harness.workspace.brush_drag.is_none(), "drag state cleared");

    // Selection is deliberately separate from Draw mode. Switching to
    // Select lets a click re-pick the brush and the face the ray entered.
    harness.workspace.selected_brush = None;
    harness.workspace.selected_brush_face = None;
    harness.workspace.active_tool = ViewTool::Select;
    let select_tool = tool_impl_3d(ViewTool::Select);
    let click = brush_frame(&harness, Pos2::new(400.0, 350.0));
    select_tool.primary_clicked(&mut harness.workspace, &click);
    assert_eq!(harness.workspace.selected_brush, Some(0));
    assert!(harness.workspace.selected_brush_face.is_some());

    // Clicking empty sky clears the selection.
    let sky = brush_frame(&harness, Pos2::new(400.0, 10.0));
    select_tool.primary_clicked(&mut harness.workspace, &sky);
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
fn brush_tool_starts_creation_over_existing_selected_geometry() {
    let mut harness = ViewportHarness::floored_room("brush_tool_draw_over_existing", 4);
    harness.frame(harness.room_center(), 3000.0);
    harness.workspace.active_tool = ViewTool::Brush;
    harness.workspace.replace_brush_selection(0, Some(0));
    let tool = tool_impl_3d(ViewTool::Brush);
    let press = brush_frame(&harness, Pos2::new(400.0, 350.0));

    tool.primary_pressed(&mut harness.workspace, &press);

    assert!(
        harness.workspace.brush_drag.is_some(),
        "existing geometry must not intercept a Draw gesture"
    );
    assert_eq!(harness.workspace.selected_brush, None);
    assert_eq!(harness.workspace.selected_brush_face, None);
}

#[test]
fn brush_tool_face_drag_extrudes_top_face() {
    let mut harness = ViewportHarness::floored_room("brush_tool_extrude", 4);
    harness.frame(harness.room_center(), 3000.0);
    harness.workspace.active_tool = ViewTool::Brush;
    // Create a brush.
    draw_brush_3d(
        &mut harness,
        Pos2::new(300.0, 300.0),
        Pos2::new(500.0, 400.0),
        0.0,
    );
    let tool = tool_impl_3d(ViewTool::Brush);
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
    draw_brush_3d(
        &mut harness,
        Pos2::new(280.0, 300.0),
        Pos2::new(520.0, 400.0),
        0.0,
    );
    assert_eq!(harness.workspace.selected_brush, Some(0));
    let whole = harness.workspace.project.active_scene().brushes[0].solve();

    // Clip mode: two clicks across the middle, one above, one below the
    // footprint on screen, then Enter (apply) cuts along the plane.
    harness.workspace.set_brush_edit_mode(BrushEditMode::Clip);
    let clip_a = brush_frame(&harness, Pos2::new(400.0, 280.0));
    let clip_b = brush_frame(&harness, Pos2::new(400.0, 430.0));
    tool.primary_clicked(&mut harness.workspace, &clip_a);
    assert_eq!(harness.workspace.brush_clip_points.len(), 1);
    tool.primary_clicked(&mut harness.workspace, &clip_b);
    assert_eq!(harness.workspace.brush_clip_points.len(), 2);
    assert!(harness.workspace.apply_brush_clip());

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

    draw_brush_3d(
        &mut harness,
        Pos2::new(280.0, 300.0),
        Pos2::new(520.0, 400.0),
        0.0,
    );
    let whole = harness.workspace.project.active_scene().brushes[0].solve();

    harness.workspace.brush_clip_keep = BrushClipKeep::Back;
    harness.workspace.set_brush_edit_mode(BrushEditMode::Clip);
    let clip_a = brush_frame(&harness, Pos2::new(400.0, 280.0));
    let clip_b = brush_frame(&harness, Pos2::new(400.0, 430.0));
    tool.primary_clicked(&mut harness.workspace, &clip_a);
    tool.primary_clicked(&mut harness.workspace, &clip_b);
    assert!(harness.workspace.apply_brush_clip());

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
fn brush_tool_shift_drag_does_not_bypass_the_whole_brush_gizmo() {
    let mut harness = ViewportHarness::floored_room("brush_tool_move", 4);
    harness.frame(harness.room_center(), 3000.0);
    harness.workspace.active_tool = ViewTool::Brush;
    let tool = tool_impl_3d(ViewTool::Brush);

    draw_brush_3d(
        &mut harness,
        Pos2::new(300.0, 300.0),
        Pos2::new(500.0, 400.0),
        0.0,
    );
    let before = harness.workspace.project.active_scene().brushes[0].clone();

    // Shift-press over the projected top-face centre, then drag right.
    let camera = harness.workspace.viewport_3d_camera();
    let (nx, ny) = camera
        .normalized_panel_point_for_world([
            ((before.solve().min[0] + before.solve().max[0]) * 0.5) as f32,
            before.solve().max[1] as f32,
            ((before.solve().min[2] + before.solve().max[2]) * 0.5) as f32,
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
    assert!(
        harness.workspace.brush_move.is_none(),
        "shift-press must not start a hidden body move"
    );
    tool.primary_dragged(&mut harness.workspace, &dragged);
    tool.primary_released(&mut harness.workspace, &dragged);
    assert_eq!(harness.workspace.project.active_scene().brushes[0], before);
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
fn orthographic_brush_creation_uses_two_real_pointer_gestures() {
    let mut workspace = ViewportHarness::floored_room("brush_2d_staged_create", 4).workspace;
    workspace.active_workspace = WorkspaceView::Room;
    workspace.active_tool = ViewTool::Brush;
    workspace.view_2d = true;
    workspace.orthographic_view = OrthographicView::Top;
    workspace.orthographic_focus = [256.0, 0.0, 256.0];
    workspace.viewport_zoom = 1.0;
    workspace.snap_units = 16;

    run_real_egui_orthographic_brush_drag(&mut workspace, [96.0, 96.0], [416.0, 416.0]);
    assert!(workspace.project.active_scene().brushes.is_empty());
    assert!(workspace
        .brush_drag
        .is_some_and(|drag| drag.stage == BrushCreateStage::Height));

    run_real_egui_orthographic_brush_drag(&mut workspace, [416.0, 416.0], [416.0, 400.0]);
    let scene = workspace.project.active_scene();
    assert_eq!(scene.brushes.len(), 1);
    let height = scene.brushes[0].solve().max[1] - scene.brushes[0].solve().min[1];
    assert_ne!(height, f64::from(BRUSH_CREATE_HEIGHT));
    assert!(height > 0.0);
    assert!(workspace.brush_drag.is_none());
}

#[test]
fn brush_2d_clip_clicks_split_selected() {
    let mut harness = ViewportHarness::floored_room("brush_2d_clip", 4);
    harness.workspace.active_tool = ViewTool::Brush;
    harness.workspace.begin_brush_drag_2d([0.0, 0.0]);
    harness.workspace.update_brush_drag_2d([256.0, 128.0]);
    harness.workspace.commit_brush_drag();
    assert!(harness.workspace.select_brush_at_2d([128.0, 64.0]));

    // Two clip points along a vertical world line at x=128, then apply.
    harness.workspace.brush_clip_click([128, 0, -64]);
    assert_eq!(harness.workspace.brush_clip_points.len(), 1);
    harness.workspace.brush_clip_click([128, 0, 192]);
    assert!(harness.workspace.apply_brush_clip());

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
    assert!(harness.workspace.apply_brush_clip());

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
    assert_eq!(workspace.brush_clip_points.len(), 1, "first click armed");
    workspace.brush_clip_click([mid_x, 0, max_z + 64]);
    assert!(workspace.apply_brush_clip());
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
    assert!(
        report.is_ok(),
        "clipped world must cook: {:?}",
        report.errors
    );
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

    assert!(harness.workspace.csg_hollow_selected());
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

fn off_grid_absolute_snap_workspace(name: &str) -> EditorWorkspace {
    let mut project = ProjectDocument::new(name);
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [10, 0, 0],
            [138, 128, 128],
        ));
    let mut workspace = EditorWorkspace::with_project(test_temp_dir(name), project);
    workspace.active_tool = ViewTool::Select;
    workspace.view_2d = true;
    workspace.orthographic_view = OrthographicView::Top;
    workspace.orthographic_focus = [64.0; 3];
    workspace.snap_units = 16;
    workspace.replace_brush_selection(0, None);
    workspace
}

#[test]
fn off_grid_vertex_and_edge_drags_snap_the_destination_not_the_delta() {
    let mut vertex = off_grid_absolute_snap_workspace("absolute-vertex-snap");
    assert!(vertex.begin_brush_vertex_drag_2d([10.0, 0.0], 0.5));
    vertex.update_brush_vertex_drag_2d([15.0, 0.0]);
    assert_eq!(
        vertex.brush_vertex_drag.as_ref().unwrap().applied,
        [6, 0, 0],
        "X=10 dragged toward the 16-unit line must land at X=16"
    );
    assert!(crate::workspace::brush_elements::unique_vertices(
        &vertex.project.active_scene().brushes[0].solve()
    )
    .iter()
    .any(|point| point[0] == 16.0));

    let mut edge = off_grid_absolute_snap_workspace("absolute-edge-snap");
    assert!(edge.begin_brush_edge_drag_2d([10.0, 64.0], 0.5));
    edge.update_brush_vertex_drag_2d([15.0, 64.0]);
    assert_eq!(
        edge.brush_vertex_drag.as_ref().unwrap().applied,
        [6, 0, 0],
        "the grabbed edge must share one rigid delta that puts it on X=16"
    );
}

#[test]
fn off_grid_face_drag_snaps_its_plane_to_the_absolute_grid() {
    let mut workspace = off_grid_absolute_snap_workspace("absolute-face-snap");
    assert!(workspace.begin_brush_resize_2d([138.0, 64.0], 0.5));
    workspace.update_brush_resize_2d([143.0, 64.0]);
    assert_eq!(
        workspace.brush_extrude.as_ref().unwrap().applied,
        [6, 0, 0],
        "the X=138 face must land on the X=144 grid line"
    );
    assert_eq!(
        workspace.project.active_scene().brushes[0].solve().max[0],
        144.0
    );
}

#[test]
fn snap_level_quantises_every_brush_as_one_undo_step() {
    let originals = [
        psxed_project::brush::Brush::cuboid([1, 1, 1], [65, 65, 65]),
        psxed_project::brush::Brush::cuboid([81, 17, -63], [145, 81, 1]),
    ];
    let mut project = ProjectDocument::new("snap complete level");
    project.active_scene_mut().brushes.extend(originals.clone());
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("snap-level"), project);
    workspace.snap_units = 16;

    workspace.snap_all_brushes_to_grid();

    assert!(workspace.is_dirty());
    assert!(workspace.status.starts_with("Snapped 2 brushes"));
    for brush in &workspace.project.active_scene().brushes {
        for face in &brush.faces {
            for point in face.points {
                assert!(
                    point
                        .iter()
                        .all(|coordinate| coordinate.rem_euclid(16) == 0),
                    "level snap left {point:?} off-grid"
                );
            }
        }
        assert!(brush.solve().is_valid());
    }

    workspace.do_undo();
    assert_eq!(workspace.project.active_scene().brushes, originals);
}

#[test]
fn snap_level_is_atomic_when_the_grid_would_collapse_a_thin_brush() {
    let originals = [
        psxed_project::brush::Brush::cuboid([1, 0, 0], [7, 16, 16]),
        psxed_project::brush::Brush::cuboid([17, 0, 0], [49, 32, 32]),
    ];
    let mut project = ProjectDocument::new("reject destructive level snap");
    project.active_scene_mut().brushes.extend(originals.clone());
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("snap-level-atomic"), project);
    workspace.snap_units = 16;

    workspace.snap_all_brushes_to_grid();

    assert_eq!(workspace.project.active_scene().brushes, originals);
    assert!(workspace.status.starts_with("Snap level aborted:"));
    assert!(workspace.status.contains("choose a finer grid"));
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

    // Move through the orthographic brush gesture; the top face UV offset must
    // compensate for the world shift (identity UV would stay [0, 0]
    // only if the brush did not move).
    harness.workspace.replace_brush_selection(0, None);
    let start = [
        ((before.min[0] + before.max[0]) * 0.5) as f32,
        ((before.min[2] + before.max[2]) * 0.5) as f32,
    ];
    assert!(harness.workspace.begin_brush_move_2d(start));
    harness
        .workspace
        .update_brush_move_2d([start[0] + 256.0, start[1]]);
    harness.workspace.commit_brush_gesture_2d();

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
    let brushes = &harness.workspace.project.active_scene().brushes;
    assert_eq!(brushes[3], brushes[0], "first copy stays in place");
    assert_eq!(brushes[4], brushes[2], "second copy stays in place");
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

fn element_click_workspace() -> (EditorWorkspace, Rect) {
    let mut project = ProjectDocument::new("brush-element-clicks");
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [0, 0, 0],
            [512, 512, 512],
        ));
    let mut workspace = EditorWorkspace::with_project(std::env::temp_dir(), project);
    workspace.active_tool = ViewTool::Select;
    workspace.camera_rig.mode = ViewportCameraMode::Free;
    workspace.camera_rig.free_initialized = true;
    workspace.camera_rig.free_position = [1200, 1000, -1200];
    let (yaw, pitch) = camera_angles_to_look_at([1200, 1000, -1200], [256, 256, 256]).unwrap();
    workspace.camera_rig.free_yaw = yaw;
    workspace.camera_rig.free_pitch = pitch;
    let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    (workspace, rect)
}

fn screen_for_world(workspace: &EditorWorkspace, rect: Rect, world: [f64; 3]) -> Pos2 {
    let camera = workspace.viewport_3d_camera();
    let (nx, ny) = camera
        .normalized_panel_point_for_world([world[0] as f32, world[1] as f32, world[2] as f32])
        .expect("point projects");
    Pos2::new(
        rect.center().x + nx * rect.width() * 0.5,
        rect.center().y + ny * rect.height() * 0.5,
    )
}

fn element_click_frame(rect: Rect, pointer: Pos2, modifiers: egui::Modifiers) -> ToolFrame3d {
    // pointer_target: None reproduces the silhouette case: corner handles
    // sit on the outline where the pick ray misses the solid.
    ToolFrame3d {
        rect,
        pointer_interact: Some(pointer),
        pointer_hover: Some(pointer),
        modifiers,
        pointer_target: None,
        hover_room: None,
        drag_delta_y: 0.0,
    }
}

#[test]
fn clicks_select_vertices_and_edges_individually() {
    let (mut workspace, rect) = element_click_workspace();
    workspace.replace_brush_selection(0, None);
    workspace.set_brush_edit_mode(BrushEditMode::Vertex);

    // Plain click on a corner handle selects that vertex, and the brush
    // selection survives even though the pointer target is None (the
    // silhouette regression).
    let corner = screen_for_world(&workspace, rect, [0.0, 512.0, 0.0]);
    let tool = tool_impl_3d(ViewTool::Select);
    tool.primary_clicked(
        &mut workspace,
        &element_click_frame(rect, corner, egui::Modifiers::NONE),
    );
    assert_eq!(
        workspace.selected_brush_elements,
        vec![BrushElement::Vertex([0, 512, 0])]
    );
    assert_eq!(workspace.selected_brush, Some(0), "brush selection intact");

    // Shift-click adds a second vertex; shift-clicking it again removes it.
    let corner2 = screen_for_world(&workspace, rect, [512.0, 512.0, 0.0]);
    tool.primary_clicked(
        &mut workspace,
        &element_click_frame(rect, corner2, egui::Modifiers::SHIFT),
    );
    assert_eq!(workspace.selected_brush_elements.len(), 2);
    tool.primary_clicked(
        &mut workspace,
        &element_click_frame(rect, corner2, egui::Modifiers::SHIFT),
    );
    assert_eq!(
        workspace.selected_brush_elements,
        vec![BrushElement::Vertex([0, 512, 0])]
    );

    // Edge mode: click the top-front edge midpoint; canonical key.
    workspace.set_brush_edit_mode(BrushEditMode::Edge);
    let midpoint = screen_for_world(&workspace, rect, [256.0, 512.0, 0.0]);
    tool.primary_clicked(
        &mut workspace,
        &element_click_frame(rect, midpoint, egui::Modifiers::NONE),
    );
    assert_eq!(
        workspace.selected_brush_elements,
        vec![BrushElement::Edge([0, 512, 0], [512, 512, 0])]
    );
}

#[test]
fn top_view_click_selects_the_vertex_column() {
    let mut harness = ViewportHarness::floored_room("brush_2d_element_click", 4);
    harness.workspace.active_tool = ViewTool::Select;
    harness
        .workspace
        .project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid([0, 0, 0], [64, 64, 64]));
    harness.workspace.replace_brush_selection(0, None);
    harness.workspace.set_brush_edit_mode(BrushEditMode::Vertex);

    // Clicking the (0,0) corner in Top view selects the whole depth
    // column (two corners: y=0 and y=64), and the brush stays selected.
    harness
        .workspace
        .handle_viewport_click([0.0, 0.0], &[], egui::Modifiers::default());
    assert_eq!(
        harness.workspace.selected_brush_elements.len(),
        2,
        "column selects both stacked corners, got {:?}",
        harness.workspace.selected_brush_elements
    );
    assert_eq!(harness.workspace.selected_brush, Some(0));

    // Shift-click another corner column adds; shift-clicking it again
    // removes the whole column as one unit.
    let shift = egui::Modifiers {
        shift: true,
        ..Default::default()
    };
    harness
        .workspace
        .handle_viewport_click([64.0, 0.0], &[], shift);
    assert_eq!(harness.workspace.selected_brush_elements.len(), 4);
    harness
        .workspace
        .handle_viewport_click([64.0, 0.0], &[], shift);
    assert_eq!(harness.workspace.selected_brush_elements.len(), 2);

    // A plain click far from any handle clears the brush selection as
    // before (no handle in range), proving the handle-first path only
    // consumes clicks that actually land on handles.
    harness
        .workspace
        .handle_viewport_click([4000.0, 4000.0], &[], egui::Modifiers::default());
    assert_eq!(harness.workspace.selected_brush, None);
}

#[test]
fn group_drag_moves_every_selected_vertex() {
    let (mut workspace, rect) = element_click_workspace();
    workspace.replace_brush_selection(0, None);
    workspace.set_brush_edit_mode(BrushEditMode::Vertex);
    let tool = tool_impl_3d(ViewTool::Select);

    // Select two top corners.
    let corner_a = screen_for_world(&workspace, rect, [0.0, 512.0, 0.0]);
    let corner_b = screen_for_world(&workspace, rect, [512.0, 512.0, 0.0]);
    tool.primary_clicked(
        &mut workspace,
        &element_click_frame(rect, corner_a, egui::Modifiers::NONE),
    );
    tool.primary_clicked(
        &mut workspace,
        &element_click_frame(rect, corner_b, egui::Modifiers::SHIFT),
    );
    assert_eq!(workspace.selected_brush_elements.len(), 2);

    // Drag corner A upward through the real press/update/commit chain;
    // both corners must ride the drag.
    tool.primary_pressed(
        &mut workspace,
        &element_click_frame(rect, corner_a, egui::Modifiers::NONE),
    );
    assert!(
        workspace.brush_vertex_drag.is_some(),
        "handle grab starts a drag"
    );
    assert_eq!(
        workspace.brush_vertex_drag.as_ref().unwrap().targets.len(),
        2,
        "grabbing a selected handle drags the whole set"
    );
    let lift = screen_for_world(&workspace, rect, [0.0, 640.0, 0.0]);
    tool.primary_dragged(
        &mut workspace,
        &element_click_frame(rect, lift, egui::Modifiers::NONE),
    );
    let applied = workspace.brush_vertex_drag.as_ref().unwrap().applied;
    assert!(applied != [0; 3], "drag applied a delta, got {applied:?}");
    tool.primary_released(
        &mut workspace,
        &element_click_frame(rect, lift, egui::Modifiers::NONE),
    );

    // Both authored top-front corners moved by the same delta.
    let brush = &workspace.project.active_scene().brushes[0];
    let solved = brush.solve();
    let verts = crate::workspace::brush_elements::unique_vertices(&solved);
    let moved = |target: [f64; 3]| {
        verts.iter().any(|vertex| {
            (0..3)
                .all(|axis| (vertex[axis] - (target[axis] + f64::from(applied[axis]))).abs() <= 0.5)
        })
    };
    assert!(
        moved([0.0, 512.0, 0.0]),
        "corner A moved by the applied delta"
    );
    assert!(
        moved([512.0, 512.0, 0.0]),
        "corner B moved by the applied delta"
    );

    // The selection survived its own drag (keys remapped).
    assert_eq!(
        workspace.selected_brush_elements.len(),
        2,
        "selection survives the drag commit"
    );
}

#[test]
fn edge_drag_through_the_degenerate_zone_never_leaves_bounded_geometry() {
    // Regression for the live crash: dragging an edge onto the opposite
    // plane made an infinite wedge that passed is_valid and overflowed
    // the preview renderer. Previews must refuse unbounded solids and
    // hold the last valid state instead.
    let (mut workspace, rect) = element_click_workspace();
    workspace.replace_brush_selection(0, None);
    workspace.set_brush_edit_mode(BrushEditMode::Edge);
    let tool = tool_impl_3d(ViewTool::Select);

    // Select and grab the top-front edge, then drag it down past the
    // floor plane in steps, crossing the degenerate configuration.
    let midpoint = screen_for_world(&workspace, rect, [256.0, 512.0, 0.0]);
    tool.primary_clicked(
        &mut workspace,
        &element_click_frame(rect, midpoint, egui::Modifiers::NONE),
    );
    assert_eq!(workspace.selected_brush_elements.len(), 1);
    tool.primary_pressed(
        &mut workspace,
        &element_click_frame(rect, midpoint, egui::Modifiers::NONE),
    );
    assert!(workspace.brush_vertex_drag.is_some());
    for step in 1..=12 {
        let y = 512.0 - f64::from(step) * 96.0;
        let pointer = screen_for_world(&workspace, rect, [256.0, y, 0.0]);
        tool.primary_dragged(
            &mut workspace,
            &element_click_frame(rect, pointer, egui::Modifiers::NONE),
        );
        let solved = workspace.project.active_scene().brushes[0].solve();
        assert!(
            solved.is_valid()
                && solved.within_extent(psxed_project::brush::BRUSH_EDIT_EXTENT_LIMIT),
            "unbounded preview reached the scene at step {step}: {:?}..{:?}",
            solved.min,
            solved.max
        );
    }
    let release = screen_for_world(&workspace, rect, [256.0, -640.0, 0.0]);
    tool.primary_released(
        &mut workspace,
        &element_click_frame(rect, release, egui::Modifiers::NONE),
    );
    let solved = workspace.project.active_scene().brushes[0].solve();
    assert!(
        solved.is_valid() && solved.within_extent(psxed_project::brush::BRUSH_EDIT_EXTENT_LIMIT),
        "committed brush must stay bounded"
    );
}

#[test]
fn face_mode_first_click_selects_brush_and_face_together() {
    let (mut workspace, rect) = element_click_workspace();
    workspace.set_brush_edit_mode(BrushEditMode::Face);
    assert_eq!(workspace.selected_brush, None);
    // One click on the top face: brush selected AND the face element
    // recorded, no second click needed, nearest face (no cycling).
    let pointer = screen_for_world(&workspace, rect, [256.0, 512.0, 256.0]);
    let target = workspace.resolve_viewport_3d_pointer_target(rect, pointer, None, true);
    let mut frame = element_click_frame(rect, pointer, egui::Modifiers::NONE);
    frame.pointer_target = target;
    tool_impl_3d(ViewTool::Select).primary_clicked(&mut workspace, &frame);
    assert_eq!(workspace.selected_brush, Some(0));
    let face = match workspace.selected_brush_elements.as_slice() {
        [BrushElement::Face(face)] => *face,
        other => panic!("expected one face element, got {other:?}"),
    };
    assert_eq!(workspace.selected_brush_face, Some(face));
    // A second click at the same spot keeps the SAME face (nearest, not
    // the old click-through cycle).
    tool_impl_3d(ViewTool::Select).primary_clicked(&mut workspace, &frame);
    assert_eq!(
        workspace.selected_brush_elements,
        vec![BrushElement::Face(face)],
        "repeat clicks must not cycle to another face"
    );
}

#[test]
fn element_gizmo_drags_are_axis_constrained() {
    let (mut workspace, rect) = element_click_workspace();
    workspace.replace_brush_selection(0, None);
    workspace.set_brush_edit_mode(BrushEditMode::Vertex);
    let tool = tool_impl_3d(ViewTool::Select);
    let corner = screen_for_world(&workspace, rect, [0.0, 512.0, 0.0]);
    tool.primary_clicked(
        &mut workspace,
        &element_click_frame(rect, corner, egui::Modifiers::NONE),
    );
    assert_eq!(workspace.selected_brush_elements.len(), 1);

    // Grab the Y axis of the gizmo (a point most of the way up the arrow)
    // and drag diagonally: only Y may change.
    let polylines = workspace
        .brush_element_gizmo_polylines_3d(rect)
        .expect("gizmo axes project");
    let grab = polyline_grab_point(&polylines[1]);
    assert_eq!(
        workspace.pick_brush_element_gizmo_axis_3d(rect, grab),
        Some(1),
        "grab point picks the Y axis"
    );
    tool.primary_pressed(
        &mut workspace,
        &element_click_frame(rect, grab, egui::Modifiers::NONE),
    );
    let drag = workspace
        .brush_vertex_drag
        .as_ref()
        .expect("gizmo grab starts a drag");
    assert_eq!(drag.axis_mask, [false, true, false]);
    // Diagonal screen movement: only the masked-in Y axis may apply.
    let lift = grab + Vec2::new(48.0, -48.0);
    tool.primary_dragged(
        &mut workspace,
        &element_click_frame(rect, lift, egui::Modifiers::NONE),
    );
    let applied = workspace.brush_vertex_drag.as_ref().unwrap().applied;
    assert_eq!(applied[0], 0, "X is masked");
    assert_eq!(applied[2], 0, "Z is masked");
    assert!(applied[1] != 0, "Y follows the drag, got {applied:?}");
    tool.primary_released(
        &mut workspace,
        &element_click_frame(rect, lift, egui::Modifiers::NONE),
    );
}

#[test]
fn face_mode_click_selects_face_and_mirrors_uv_state() {
    let (mut workspace, rect) = element_click_workspace();
    workspace.replace_brush_selection(0, None);
    workspace.set_brush_edit_mode(BrushEditMode::Face);
    // Click the brush body (a visible face centre): resolves the real
    // pointer target and routes through the face element path.
    let pointer = screen_for_world(&workspace, rect, [256.0, 512.0, 256.0]);
    let target = workspace.resolve_viewport_3d_pointer_target(rect, pointer, None, true);
    let mut frame = element_click_frame(rect, pointer, egui::Modifiers::NONE);
    frame.pointer_target = target;
    tool_impl_3d(ViewTool::Select).primary_clicked(&mut workspace, &frame);
    let face = match workspace.selected_brush_elements.as_slice() {
        [BrushElement::Face(face)] => *face,
        other => panic!("expected one face element, got {other:?}"),
    };
    assert_eq!(
        workspace.selected_brush_face,
        Some(face),
        "face element mirrors into the UV/inspector state"
    );
}

#[test]
fn clip_mode_cuts_the_whole_multi_selection_in_one_undo_step() {
    let mut harness = ViewportHarness::floored_room("clip_multi", 4);
    harness.workspace.active_tool = ViewTool::Select;
    let scene = harness.workspace.project.active_scene_mut();
    scene.brushes.push(psxed_project::brush::Brush::cuboid(
        [0, 0, 0],
        [128, 64, 64],
    ));
    scene.brushes.push(psxed_project::brush::Brush::cuboid(
        [256, 0, 0],
        [384, 64, 64],
    ));
    harness.workspace.replace_brush_selection(0, None);
    harness.workspace.toggle_brush_selection(1);
    harness.workspace.set_brush_edit_mode(BrushEditMode::Clip);

    // A vertical plane at z=32 crosses both brushes (Top view points).
    harness.workspace.brush_clip_click([-64, 0, 32]);
    harness.workspace.brush_clip_click([512, 0, 32]);
    assert!(harness.workspace.apply_brush_clip());
    assert_eq!(
        harness.workspace.project.active_scene().brushes.len(),
        4,
        "both selected brushes split"
    );
    assert!(
        harness.workspace.brush_clip_points.is_empty(),
        "points consumed by the cut"
    );
    harness.workspace.do_undo();
    assert_eq!(
        harness.workspace.project.active_scene().brushes.len(),
        2,
        "one undo restores the whole cut"
    );
}

#[test]
fn clip_three_points_cut_a_sloped_plane_and_escape_clears() {
    let mut harness = ViewportHarness::floored_room("clip_sloped", 4);
    harness.workspace.active_tool = ViewTool::Select;
    harness
        .workspace
        .project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [0, 0, 0],
            [128, 128, 128],
        ));
    harness.workspace.replace_brush_selection(0, None);
    harness.workspace.set_brush_edit_mode(BrushEditMode::Clip);

    // Escape (cancel_brush_gestures) clears pending points.
    harness.workspace.brush_clip_click([0, 0, 0]);
    assert_eq!(harness.workspace.brush_clip_points.len(), 1);
    harness.workspace.cancel_brush_gestures();
    assert!(harness.workspace.brush_clip_points.is_empty());

    // Three points define an exact sloped (Y-Z diagonal) plane.
    harness.workspace.brush_clip_click([0, 0, 0]);
    harness.workspace.brush_clip_click([128, 0, 0]);
    harness.workspace.brush_clip_click([0, 128, 128]);
    assert!(harness.workspace.apply_brush_clip());
    let scene = harness.workspace.project.active_scene();
    assert_eq!(scene.brushes.len(), 2, "sloped cut split the cuboid");
    let a = scene.brushes[0].solve();
    let b = scene.brushes[1].solve();
    assert!(a.is_valid() && b.is_valid());
}

#[test]
fn brush_element_enumerators_dedup_and_canonicalize() {
    use crate::workspace::brush_elements::{edge_element_key, unique_edges, unique_vertices};
    let brush = psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 64, 256]);
    let solved = brush.solve();
    assert_eq!(unique_vertices(&solved).len(), 8, "cuboid corners");
    let edges = unique_edges(&solved);
    assert_eq!(edges.len(), 12, "shared edges enumerate once");
    // Every enumerated edge is stored in canonical key order, and the key
    // is winding-independent.
    for (a, b) in &edges {
        let key = edge_element_key(*a, *b);
        let swapped = edge_element_key(*b, *a);
        assert_eq!(key, swapped);
        assert!(key.0 <= key.1);
    }
}

#[test]
fn selection_domains_are_exclusive_and_empty_click_clears_all() {
    let mut harness = ViewportHarness::floored_room("brush_sel_domains", 4);
    let scene = harness.workspace.project.active_scene_mut();
    scene
        .brushes
        .push(psxed_project::brush::Brush::cuboid([0, 0, 0], [64, 64, 64]));
    let entity = scene.add_node(NodeId::ROOT, "Entity", psxed_project::NodeKind::Entity);

    // Brush selected, then picking an entity must drop the brush selection.
    harness.workspace.replace_brush_selection(0, None);
    let order = harness.workspace.scene_node_order();
    harness
        .workspace
        .apply_node_selection_modifiers(entity, egui::Modifiers::default(), &order);
    assert_eq!(
        harness.workspace.selected_brush, None,
        "entity pick clears brush"
    );
    assert_eq!(harness.workspace.selection.selected_node, entity);

    // Reverse direction is covered by the click handler; the promote-on-drag
    // path must clear too.
    harness.workspace.replace_brush_selection(0, None);
    harness.workspace.commit_node_selection(entity);
    assert_eq!(
        harness.workspace.selected_brush, None,
        "drag promote clears brush"
    );

    // Empty click clears every domain.
    harness.workspace.replace_brush_selection(0, None);
    harness.workspace.selection.hovered_primitive = None;
    harness
        .workspace
        .commit_face_selection(egui::Modifiers::default());
    assert_eq!(harness.workspace.selected_brush, None);
    assert_eq!(harness.workspace.selection.selected_node, NodeId::ROOT);
    assert!(harness.workspace.selection.selected_nodes.is_empty());
}

#[test]
fn duplicate_routes_to_brushes_from_the_select_tool() {
    // A brush selected through the general Select tool is directly editable,
    // so Cmd+D must copy the brush there too, not only under ViewTool::Brush.
    let mut harness = ViewportHarness::floored_room("brush_dup_select_tool", 4);
    harness.workspace.active_tool = ViewTool::Select;
    harness
        .workspace
        .project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid([0, 0, 0], [64, 64, 64]));
    harness.workspace.replace_brush_selection(0, None);
    harness.workspace.duplicate_current_selection();
    assert_eq!(harness.workspace.project.active_scene().brushes.len(), 2);
    assert_eq!(harness.workspace.selected_brush, Some(1));
    assert_eq!(
        harness.workspace.project.active_scene().brushes[1],
        harness.workspace.project.active_scene().brushes[0],
        "Cmd+D keeps the duplicate exactly on the source"
    );
}

#[test]
fn room_copy_and_paste_shortcuts_copy_brush_geometry() {
    let mut harness = ViewportHarness::floored_room("brush_copy_paste_shortcuts", 4);
    harness.workspace.active_workspace = WorkspaceView::Room;
    harness.workspace.active_tool = ViewTool::Select;
    let brush = psxed_project::brush::Brush::cuboid([32, 0, 64], [160, 192, 224]);
    harness
        .workspace
        .project
        .active_scene_mut()
        .brushes
        .push(brush.clone());
    harness.workspace.replace_brush_selection(0, None);

    let send = |workspace: &mut EditorWorkspace, key| {
        let context = egui::Context::default();
        let _ = context.run(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::COMMAND,
                }],
                ..egui::RawInput::default()
            },
            |ctx| workspace.handle_global_shortcuts(ctx, EditorPlaytestStatus::Idle),
        );
    };
    send(&mut harness.workspace, egui::Key::C);
    assert!(harness.workspace.portable_geometry_clipboard.is_some());
    send(&mut harness.workspace, egui::Key::V);

    let brushes = &harness.workspace.project.active_scene().brushes;
    assert_eq!(brushes.len(), 2);
    assert_eq!(brushes[1], brush, "paste keeps the authored coordinates");
    assert_eq!(harness.workspace.selected_brush_set(), vec![1]);
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
    let _ = harness.workspace.apply_brush_clip();
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
        draw_brush_3d(&mut harness, from, to, 0.0);
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
    assert!(
        harness.workspace.status.contains("Move rejected"),
        "invalid geometry explains why the position was refused: {}",
        harness.workspace.status
    );
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
    type WorkspaceOperation = (&'static str, fn(&mut EditorWorkspace));
    let ops: [WorkspaceOperation; 4] = [
        ("duplicate", |ws| ws.duplicate_selected_brushes()),
        ("snap", |ws| {
            ws.project.active_scene_mut().brushes[0] =
                psxed_project::brush::Brush::cuboid([1, 0, -1], [65, 63, 62]);
            ws.selected_brush = Some(0);
            ws.snap_selected_brush();
        }),
        ("hollow", |ws| {
            ws.selected_brush = Some(0);
            ws.snap_units = 16;
            assert!(ws.csg_hollow_selected());
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
pub(super) fn real_egui_workspace_ctx(name: &str) -> (egui::Context, EditorViewport3dPresentation) {
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
pub(super) fn real_egui_workspace_frame(
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

pub(super) fn press_release(point: Pos2) -> (Vec<egui::Event>, Vec<egui::Event>) {
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

/// Open a searchable picker by its current label, then choose one visible
/// option from the popup. Both clicks travel through full workspace frames.
fn run_real_egui_workspace_select_picker_option(
    workspace: &mut EditorWorkspace,
    current_label: &str,
    option_label: &str,
) {
    let (ctx, viewport) = real_egui_workspace_ctx("workspace-picker-option");
    let picker = locate_unique_label(&ctx, workspace, &viewport, current_label);
    let (press, release) = press_release(picker);
    let _ = real_egui_workspace_frame(&ctx, workspace, &viewport, 1.0 / 60.0, press);
    let _ = real_egui_workspace_frame(&ctx, workspace, &viewport, 2.0 / 60.0, release);

    let frame = real_egui_workspace_frame(&ctx, workspace, &viewport, 3.0 / 60.0, vec![]);
    let found = text_shape_centers(&frame.shapes, option_label);
    assert_eq!(
        found.len(),
        1,
        "picker option {option_label:?} must be visible exactly once, saw {found:?}"
    );
    let option = found[0];
    let (press, release) = press_release(option);
    let _ = real_egui_workspace_frame(&ctx, workspace, &viewport, 4.0 / 60.0, press);
    let _ = real_egui_workspace_frame(&ctx, workspace, &viewport, 5.0 / 60.0, release);
    let _ = real_egui_workspace_frame(&ctx, workspace, &viewport, 6.0 / 60.0, vec![]);
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
    workspace.brush_edit_mode = BrushEditMode::Face;
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

/// Regression: choosing a different material in the brush face Inspector
/// applies immediately. The Apply button is not required as a second step.
#[test]
fn inspector_material_picker_assigns_the_selected_brush_face_via_real_egui() {
    let mut project = ProjectDocument::new("face inspector material picker");
    let stone = project.add_resource(
        "Stone",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let moss = project.add_resource(
        "Moss",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let mut brush = psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]);
    for face in &mut brush.faces {
        face.material = Some(stone);
    }
    project.active_scene_mut().brushes.push(brush);
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("inspector-face-picker"), project);
    workspace.active_workspace = WorkspaceView::Room;
    workspace.active_tool = ViewTool::Select;
    workspace.brush_edit_mode = BrushEditMode::Face;
    workspace.resources_open = false;
    workspace.replace_brush_selection(0, Some(2));

    run_real_egui_workspace_select_picker_option(&mut workspace, "Stone", "Moss");

    assert_eq!(
        workspace.project.active_scene().brushes[0].faces[2].material,
        Some(moss)
    );
    assert!(workspace.project.active_scene().brushes[0]
        .faces
        .iter()
        .enumerate()
        .all(|(index, face)| index == 2 || face.material == Some(stone)));
    assert_eq!(workspace.selected_brush, Some(0));
    assert_eq!(workspace.selected_brush_face, Some(2));

    workspace.do_undo();
    assert!(workspace.project.active_scene().brushes[0]
        .faces
        .iter()
        .all(|face| face.material == Some(stone)));
}

/// Regression: a plain click on a Material card applies to the selected BSP
/// brush face instead of merely changing resource selection.
#[test]
fn resource_browser_material_card_assigns_the_selected_brush_face_via_real_egui() {
    let mut project = ProjectDocument::new("face resource browser material");
    let stone = project.add_resource(
        "Stone",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let moss = project.add_resource(
        "Moss",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let mut brush = psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]);
    for face in &mut brush.faces {
        face.material = Some(stone);
    }
    project.active_scene_mut().brushes.push(brush);
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("resource-face-card"), project);
    workspace.active_workspace = WorkspaceView::Room;
    workspace.active_tool = ViewTool::Select;
    workspace.brush_edit_mode = BrushEditMode::Face;
    workspace.inspector_open = false;
    workspace.resources_open = true;
    workspace.replace_brush_selection(0, Some(2));

    run_real_egui_workspace_click_on_label(&mut workspace, "Moss");

    assert_eq!(
        workspace.project.active_scene().brushes[0].faces[2].material,
        Some(moss)
    );
    assert!(workspace.project.active_scene().brushes[0]
        .faces
        .iter()
        .enumerate()
        .all(|(index, face)| index == 2 || face.material == Some(stone)));
    assert_eq!(workspace.selected_brush, Some(0));
    assert_eq!(workspace.selected_brush_face, Some(2));
    assert_eq!(workspace.selection.selected_resource, Some(moss));

    workspace.do_undo();
    assert!(workspace.project.active_scene().brushes[0]
        .faces
        .iter()
        .all(|face| face.material == Some(stone)));
}

#[test]
fn selected_brush_face_is_a_resource_browser_material_target() {
    let mut project = ProjectDocument::new("brush resource material assignment");
    let stone = project.add_resource(
        "Stone",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let moss = project.add_resource(
        "Moss",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let mut brush = psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]);
    for face in &mut brush.faces {
        face.material = Some(stone);
    }
    project.active_scene_mut().brushes.push(brush);
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("brush-resource"), project);
    workspace.brush_edit_mode = BrushEditMode::Face;
    workspace.replace_brush_selection(0, Some(2));

    assert_eq!(
        workspace.selected_material_targets(),
        vec![MaterialTarget::BrushFace { brush: 0, face: 2 }]
    );
    assert_eq!(workspace.assign_selected_faces_material(Some(moss)), 1);
    assert_eq!(
        workspace.project.active_scene().brushes[0].faces[2].material,
        Some(moss)
    );
    assert!(workspace.project.active_scene().brushes[0]
        .faces
        .iter()
        .enumerate()
        .all(|(index, face)| index == 2 || face.material == Some(stone)));

    workspace.do_undo();
    assert!(workspace.project.active_scene().brushes[0]
        .faces
        .iter()
        .all(|face| face.material == Some(stone)));
}

#[test]
fn brush_mode_material_assignment_updates_every_face_despite_retained_hit_face() {
    let mut project = ProjectDocument::new("whole brush material assignment");
    let stone = project.add_resource(
        "Stone",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let moss = project.add_resource(
        "Moss",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let mut brush = psxed_project::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]);
    for face in &mut brush.faces {
        face.material = Some(stone);
    }
    project.active_scene_mut().brushes.push(brush);
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("whole-brush-resource"), project);
    workspace.active_workspace = WorkspaceView::Room;
    workspace.active_tool = ViewTool::Select;
    workspace.resources_open = false;
    workspace.brush_edit_mode = BrushEditMode::Move;
    // Picking a brush retains the hit face for overlap cycling. It must not
    // narrow an object-mode material assignment to that face.
    workspace.replace_brush_selection(0, Some(2));

    let targets = workspace.selected_brush_material_targets();
    assert_eq!(targets.len(), 6);
    run_real_egui_workspace_select_picker_option(&mut workspace, "Stone", "Moss");
    assert!(workspace.project.active_scene().brushes[0]
        .faces
        .iter()
        .all(|face| face.material == Some(moss)));
    assert!(workspace.is_dirty());

    workspace.do_undo();
    assert!(workspace.project.active_scene().brushes[0]
        .faces
        .iter()
        .all(|face| face.material == Some(stone)));
}

#[test]
fn multi_brush_material_assignment_updates_every_face_in_one_undo_step() {
    let mut project = ProjectDocument::new("multi-brush material assignment");
    let stone = project.add_resource(
        "Stone",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    let moss = project.add_resource(
        "Moss",
        ResourceData::Material(MaterialResource::opaque(None)),
    );
    for (min, max) in [([0, 0, 0], [128, 128, 128]), ([256, 0, 0], [384, 128, 128])] {
        let mut brush = psxed_project::brush::Brush::cuboid(min, max);
        for face in &mut brush.faces {
            face.material = Some(stone);
        }
        project.active_scene_mut().brushes.push(brush);
    }
    let mut workspace =
        EditorWorkspace::with_project(test_temp_dir("multi-brush-material"), project);
    workspace.replace_brush_selection(0, None);
    workspace.toggle_brush_selection(1);

    let targets = workspace.selected_brush_material_targets();
    assert_eq!(targets.len(), 12);
    assert!(targets.contains(&MaterialTarget::BrushFace { brush: 0, face: 0 }));
    assert!(targets.contains(&MaterialTarget::BrushFace { brush: 1, face: 5 }));
    assert_eq!(workspace.assign_selected_faces_material(Some(moss)), 12);
    assert!(workspace
        .project
        .active_scene()
        .brushes
        .iter()
        .flat_map(|brush| &brush.faces)
        .all(|face| face.material == Some(moss)));

    workspace.do_undo();
    assert!(workspace
        .project
        .active_scene()
        .brushes
        .iter()
        .flat_map(|brush| &brush.faces)
        .all(|face| face.material == Some(stone)));
    workspace.do_undo();
    assert_eq!(workspace.status, "Nothing to undo");
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
    workspace.brush_edit_mode = BrushEditMode::Face;
    workspace.replace_brush_selection(0, Some(0));
    assert_eq!(
        workspace.project.active_scene().brushes[0].faces[0].uv,
        psxed_project::brush::FaceUv::default(),
        "fixture face must start at the identity UV mapping"
    );

    let cook_before = test_temp_dir("face-uv-cook-before");
    workspace.cook_playtest_to_dir(&cook_before).expect("cook");

    // A pure offset edit still slides the texture by exactly what was typed.
    run_real_egui_type_into_drag_value(&mut workspace, "U 0", "24");
    assert_eq!(
        workspace.project.active_scene().brushes[0].faces[0]
            .uv
            .offset_texels,
        [24, 0],
        "an Offset U edit must slide the texture by the typed amount"
    );

    // Rotation and scale re-anchor instead: the offset absorbs whatever
    // keeps the face's own centroid on the same texel.
    let before_rotate = workspace.project.active_scene().brushes[0].faces[0].uv;
    let anchor = workspace.project.active_scene().brushes[0]
        .face_uv_anchor(0)
        .expect("solved face anchor");
    run_real_egui_type_into_drag_value(&mut workspace, "0\u{b0}", "15");
    run_real_egui_type_into_drag_value(&mut workspace, "100% U", "150");

    let uv = workspace.project.active_scene().brushes[0].faces[0].uv;
    assert_eq!(uv.rotation_deg, 15);
    assert_eq!(uv.scale_q8, [384, 256], "150% is 384 in Q8");
    let held = before_rotate.apply(anchor);
    let moved = uv.apply(anchor);
    assert!(
        (held[0] - moved[0]).abs() <= 1.0 && (held[1] - moved[1]).abs() <= 1.0,
        "rotation and scale must hold the face anchor: {held:?} became {moved:?}"
    );
    assert_ne!(
        uv.offset_texels,
        [24, 0],
        "holding the anchor has to move the stored offset"
    );
    let compensated = uv.offset_texels;
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
    assert_eq!(uv.offset_texels, compensated);
    assert_eq!(uv.scale_q8, [384, 256]);
    run_real_egui_workspace_click_on_label(&mut workspace, "Reset UV");
    assert_eq!(
        workspace.project.active_scene().brushes[0].faces[0].uv,
        psxed_project::brush::FaceUv::default()
    );
    workspace.do_undo();
    let uv = workspace.project.active_scene().brushes[0].faces[0].uv;
    assert_eq!(uv.offset_texels, compensated, "one undo unwinds Reset UV");

    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(cook_before);
    let _ = std::fs::remove_dir_all(cook_after);
}

/// The author's retexture step, end to end on the starter courtyard: pick the
/// other material, press "Apply to face", save, reopen, recook.
///
/// `apply_to_face_button_paints_only_the_selected_face_and_undoes_once` proves
/// the click paints the right face of an in-memory document, and
/// `face_uv_numeric_edits_cook_into_the_brush_world_and_undo_per_edit` proves
/// UV numbers reach the cook. Neither proves the MATERIAL choice does, which
/// is the half of a retexture the author actually sees once the map is
/// running: a cook that dropped it would leave both of those green.
#[test]
fn face_material_swap_survives_reopen_and_reaches_the_cooked_brush_world() {
    let template = psxed_project::new_project_template_dir();
    let dir = test_temp_dir("face-material-cook");
    crate::starter_catalogue::copy_dir_recursive(&template, &dir).unwrap();
    let mut workspace = EditorWorkspace::open_directory(&dir).unwrap();
    workspace.active_workspace = WorkspaceView::Room;
    workspace.active_tool = ViewTool::Select;
    workspace.brush_edit_mode = BrushEditMode::Face;

    // The starter courtyard carries two surface materials on purpose. Built-in
    // sky aperture materials are project-local test resources, not candidates
    // for this ordinary face retexture check.
    let materials = workspace
        .project
        .material_options()
        .into_iter()
        .filter(|(id, _)| {
            matches!(
                workspace.project.resource(*id).map(|resource| &resource.data),
                Some(ResourceData::Material(material)) if !material.sky_aperture
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        materials.len(),
        2,
        "the starter courtyard offers exactly Courtyard Cobbles and Courtyard Brick"
    );
    workspace.replace_brush_selection(0, Some(0));
    let original = workspace.project.active_scene().brushes[0].faces[0].material;
    let swapped = materials
        .iter()
        .map(|(id, _)| *id)
        .find(|id| Some(*id) != original)
        .expect("a second material to swap to");

    let cook_before = test_temp_dir("face-material-cook-before");
    workspace
        .cook_playtest_to_dir(&cook_before)
        .expect("cook the untouched courtyard");

    workspace.brush_material = Some(swapped);
    run_real_egui_workspace_click_on_label(
        &mut workspace,
        &icons::label(icons::PALETTE, "Apply to face"),
    );
    assert_eq!(
        workspace.project.active_scene().brushes[0].faces[0].material,
        Some(swapped)
    );
    assert!(workspace.is_dirty());

    workspace.save().expect("save the retextured courtyard");
    let mut reopened = EditorWorkspace::open_directory(&dir).expect("reopen retextured project");
    assert_eq!(
        reopened.project().active_scene().brushes[0].faces[0].material,
        Some(swapped),
        "a face retexture must survive save and reopen"
    );

    let cook_after = test_temp_dir("face-material-cook-after");
    reopened
        .cook_playtest_to_dir(&cook_after)
        .expect("recook the retextured courtyard");
    let world = psxed_project::brush_playtest::BRUSH_WORLD_FILENAME;
    assert_ne!(
        std::fs::read(cook_before.join(world)).unwrap(),
        std::fs::read(cook_after.join(world)).unwrap(),
        "a face retexture must change the cooked brush world"
    );

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
    let far_points: [[i32; 3]; 3] = [[-64, 192, 0], [-64, 192, 128], [192, -64, 128]];
    let (mut workspace, rect) = handle_test_workspace(wedge);
    workspace.brush_edit_mode = BrushEditMode::Face;
    workspace.selection_mode = SelectionMode::Face;
    // Load-time normalization prunes the wedge's dead plane, shifting
    // indices: find the slant (the one non-axis-aligned plane).
    let loaded = workspace.project.active_scene().brushes[0].clone();
    let slant = loaded
        .faces
        .iter()
        .position(|face| {
            let normal = psxed_project::brush::Plane::from_points(face.points)
                .unwrap()
                .normal;
            (0..3).filter(|&axis| normal[axis] != 0).count() >= 2
        })
        .expect("wedge keeps its slanted plane");
    // Off-corner authored points can only exist mid-session now (load
    // normalizes them away), which is exactly how gestures meet them:
    // re-author the slant with the far triple in place.
    workspace.project.active_scene_mut().brushes[0].faces[slant].points = far_points;
    let base = workspace.project.active_scene().brushes[0].clone();
    // Fixture precondition: no authored plane point coincides with a
    // solved polygon corner.
    let corners = solved_unique_verts(&base);
    for point in base.faces[slant].points {
        assert!(
            !corners.contains(&point.map(i64::from)),
            "authored point {point:?} must sit off the solved polygon"
        );
    }
    let (center, _) = EditorWorkspace::face_center_and_normal(&base, slant).unwrap();
    let pointer = workspace.project_brush_point_3d(rect, center).unwrap();
    let picked = workspace.pick_brush_handle_3d(rect, pointer);
    assert!(
        matches!(picked, Some((0, BrushHandle3d::Face { face, .. })) if face == slant),
        "picked {picked:?}, expected face {slant}"
    );

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
        psxed_project::brush::Plane::from_points(moved.faces[slant].points)
            .unwrap()
            .normal,
        psxed_project::brush::Plane::from_points(base.faces[slant].points)
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

#[test]
fn marquee_3d_selects_faces_edges_and_vertices_via_real_egui() {
    for (mode, expected) in [
        (BrushEditMode::Face, None),
        (BrushEditMode::Edge, Some(12)),
        (BrushEditMode::Vertex, Some(8)),
    ] {
        let (mut workspace, _) = element_click_workspace();
        workspace.replace_brush_selection(0, None);
        workspace.set_brush_edit_mode(mode);
        let viewport = Rect::from_center_size(Pos2::new(400.0, 300.0), Vec2::new(778.6667, 584.0));
        let solved = workspace.project.active_scene().brushes[0].solve();
        let projected: Vec<_> = crate::workspace::brush_elements::unique_vertices(&solved)
            .into_iter()
            .filter_map(|vertex| workspace.project_brush_point_3d(viewport, vertex))
            .collect();
        let bounds = projected.iter().fold(Rect::NOTHING, |mut bounds, point| {
            bounds.extend_with(*point);
            bounds
        });

        // The expanded corner starts outside the solid and every handle,
        // allowing Select to enter marquee mode before crossing the brush.
        run_real_egui_viewport_plain_drag(
            &mut workspace,
            bounds.min - Vec2::splat(20.0),
            bounds.max + Vec2::splat(20.0),
        );

        assert_eq!(workspace.selected_brush, Some(0), "{mode:?}");
        assert!(
            workspace
                .selected_brush_elements
                .iter()
                .all(|element| matches!(
                    (mode, element),
                    (BrushEditMode::Face, BrushElement::Face(_))
                        | (BrushEditMode::Edge, BrushElement::Edge(..))
                        | (BrushEditMode::Vertex, BrushElement::Vertex(_))
                )),
            "{mode:?}: {:?}",
            workspace.selected_brush_elements
        );
        if let Some(expected) = expected {
            assert_eq!(
                workspace.selected_brush_elements.len(),
                expected,
                "{mode:?}"
            );
        } else {
            assert!(
                !workspace.selected_brush_elements.is_empty(),
                "Face mode should collect the camera-facing handles"
            );
            assert_eq!(
                workspace.selected_brush_faces.len(),
                workspace.selected_brush_elements.len()
            );
        }
    }
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

#[test]
fn vertex_snap_has_a_visible_latched_toolbar_button_beside_the_b_shortcut() {
    let mut project = ProjectDocument::new("visible vertex snap toggle");
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [0, 0, 0],
            [128, 128, 128],
        ));
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("vertex-snap-toggle"), project);
    workspace.active_workspace = WorkspaceView::Room;
    workspace.active_tool = ViewTool::Select;
    workspace.view_2d = false;
    workspace.replace_brush_selection(0, None);
    let (ctx, viewport) = real_egui_workspace_ctx("vertex-snap-toggle");
    let label = "V↔V".to_string();
    let button = locate_unique_label(&ctx, &mut workspace, &viewport, &label);

    let (press, release) = press_release(button);
    let _ = real_egui_workspace_frame(&ctx, &mut workspace, &viewport, 1.0 / 60.0, press);
    let _ = real_egui_workspace_frame(&ctx, &mut workspace, &viewport, 2.0 / 60.0, release);
    assert!(workspace.brush_vertex_snap_enabled);
    assert_eq!(workspace.brush_edit_mode, BrushEditMode::Move);
    assert!(workspace.status.starts_with("Vertex Snap enabled"));
}

#[test]
fn snap_level_toolbar_button_runs_the_global_quantisation_command() {
    let mut project = ProjectDocument::new("visible level snap");
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid([1, 1, 1], [65, 65, 65]));
    let mut workspace = EditorWorkspace::with_project(test_temp_dir("snap-level-button"), project);
    workspace.active_workspace = WorkspaceView::Room;
    workspace.active_tool = ViewTool::Select;
    workspace.view_2d = false;
    workspace.snap_units = 16;
    let (ctx, viewport) = real_egui_workspace_ctx("snap-level-button");
    let button = locate_unique_label(&ctx, &mut workspace, &viewport, "Snap level");

    let (press, release) = press_release(button);
    let _ = real_egui_workspace_frame(&ctx, &mut workspace, &viewport, 1.0 / 60.0, press);
    let _ = real_egui_workspace_frame(&ctx, &mut workspace, &viewport, 2.0 / 60.0, release);

    assert!(workspace.status.starts_with("Snapped 1 brush"));
    assert!(workspace.project.active_scene().brushes[0]
        .faces
        .iter()
        .flat_map(|face| face.points)
        .flatten()
        .all(|coordinate| coordinate.rem_euclid(16) == 0));
}

#[test]
fn draw_toolbar_exposes_a_compact_primitive_selector_in_3d() {
    let mut workspace = ViewportHarness::floored_room("draw-primitive-selector", 4).workspace;
    workspace.active_workspace = WorkspaceView::Room;
    workspace.active_tool = ViewTool::Brush;
    workspace.view_2d = false;
    let (ctx, viewport) = real_egui_workspace_ctx("draw-primitive-selector");

    let box_label = icons::label(icons::BOX, "Box");
    let box_button = locate_unique_label(&ctx, &mut workspace, &viewport, &box_label);
    let (press, release) = press_release(box_button);
    let _ = real_egui_workspace_frame(&ctx, &mut workspace, &viewport, 1.0 / 60.0, press);
    let _ = real_egui_workspace_frame(&ctx, &mut workspace, &viewport, 2.0 / 60.0, release);
    let frame = real_egui_workspace_frame(&ctx, &mut workspace, &viewport, 3.0 / 60.0, vec![]);
    let cylinder_label = icons::label(icons::BOX, "Cylinder");
    let cylinder = text_shape_centers(&frame.shapes, &cylinder_label);
    assert_eq!(cylinder.len(), 1, "primitive menu contents: {cylinder:?}");

    let (press, release) = press_release(cylinder[0]);
    let _ = real_egui_workspace_frame(&ctx, &mut workspace, &viewport, 4.0 / 60.0, press);
    let _ = real_egui_workspace_frame(&ctx, &mut workspace, &viewport, 5.0 / 60.0, release);
    let _ = real_egui_workspace_frame(&ctx, &mut workspace, &viewport, 6.0 / 60.0, vec![]);
    assert_eq!(
        workspace.brush_draw_settings.shape,
        BrushDrawShape::Cylinder
    );
}

/// The defect this covers: the live move preview called plain `translate`
/// while the commit called `translate_with_uv_lock`, so a locked drag showed
/// the texture swimming across the brush and then snapped to a different
/// mapping the moment the mouse came up.
#[test]
fn locked_move_preview_uvs_match_the_committed_uvs() {
    let mut harness = ViewportHarness::floored_room("brush_uv_preview", 4);
    harness.workspace.active_tool = ViewTool::Brush;
    assert!(harness.workspace.brush_texture_lock, "lock defaults on");

    // Two brushes, both selected, so the rider path is covered too. Neither
    // sits at the origin and the first carries a non-identity mapping, which
    // is where an offset-only compensation would go wrong.
    let scene = harness.workspace.project.active_scene_mut();
    scene.brushes.push(psxed_project::brush::Brush::cuboid(
        [1280, 0, 1280],
        [1408, 128, 1408],
    ));
    scene.brushes.push(psxed_project::brush::Brush::cuboid(
        [1600, 0, 1600],
        [1728, 128, 1728],
    ));
    for face in &mut scene.brushes[0].faces {
        face.uv = psxed_project::brush::FaceUv {
            offset_texels: [13, -7],
            rotation_deg: 20,
            scale_q8: [320, 192],
        };
    }
    harness.workspace.replace_brush_selection(0, Some(0));
    harness.workspace.toggle_brush_selection(1);
    harness.workspace.selected_brush = Some(0);

    assert!(
        harness.workspace.begin_brush_move_2d([1344.0, 1344.0]),
        "grab the first brush"
    );
    harness.workspace.update_brush_move_2d([1600.0, 1344.0]);
    // Several mouse moves: the preview must be rebuilt from the base each
    // time, not compensated again on top of the last preview.
    harness.workspace.update_brush_move_2d([1728.0, 1472.0]);
    harness.workspace.update_brush_move_2d([1856.0, 1600.0]);

    let preview: Vec<Vec<psxed_project::brush::FaceUv>> = harness
        .workspace
        .project
        .active_scene()
        .brushes
        .iter()
        .map(|brush| brush.faces.iter().map(|face| face.uv).collect())
        .collect();

    harness.workspace.commit_brush_gesture_2d();

    let committed: Vec<Vec<psxed_project::brush::FaceUv>> = harness
        .workspace
        .project
        .active_scene()
        .brushes
        .iter()
        .map(|brush| brush.faces.iter().map(|face| face.uv).collect())
        .collect();
    assert_eq!(
        preview, committed,
        "releasing the mouse must not move the texture the drag was showing"
    );
    assert!(
        committed[0].iter().any(|uv| uv.offset_texels != [13, -7]),
        "the locked move has to compensate the primary brush"
    );
    assert!(
        committed[1].iter().any(|uv| uv.offset_texels != [0, 0]),
        "and every rider in the multi-selection"
    );
}

/// Lock off is a deliberate mode, not a bug: the mapping stays world-aligned
/// and the brush slides under its texture.
#[test]
fn unlocked_move_preview_leaves_the_mapping_world_aligned() {
    let mut harness = ViewportHarness::floored_room("brush_uv_unlocked", 4);
    harness.workspace.active_tool = ViewTool::Brush;
    harness.workspace.brush_texture_lock = false;
    harness
        .workspace
        .project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [1280, 0, 1280],
            [1408, 128, 1408],
        ));
    harness.workspace.replace_brush_selection(0, Some(0));

    assert!(harness.workspace.begin_brush_move_2d([1344.0, 1344.0]));
    harness.workspace.update_brush_move_2d([1728.0, 1600.0]);
    let preview: Vec<psxed_project::brush::FaceUv> =
        harness.workspace.project.active_scene().brushes[0]
            .faces
            .iter()
            .map(|face| face.uv)
            .collect();
    assert!(
        preview
            .iter()
            .all(|uv| *uv == psxed_project::brush::FaceUv::default()),
        "unlocked moves must not touch the mapping"
    );
    harness.workspace.commit_brush_gesture_2d();
    let committed: Vec<psxed_project::brush::FaceUv> =
        harness.workspace.project.active_scene().brushes[0]
            .faces
            .iter()
            .map(|face| face.uv)
            .collect();
    assert_eq!(preview, committed);
}

/// Texture lock used to live in the Brush-only toolbar, where the workflow
/// that needs it (select a brush, drag it, watch the texture) could not find
/// it. It belongs with the rest of the texture mapping controls, and it has
/// to be reachable in Select mode too, with or without a face selected.
#[test]
fn texture_lock_is_reachable_from_the_inspector_in_both_tools() {
    let template = psxed_project::new_project_template_dir();
    let dir = test_temp_dir("uv-lock-inspector");
    crate::starter_catalogue::copy_dir_recursive(&template, &dir).unwrap();
    let mut workspace = EditorWorkspace::open_directory(&dir).unwrap();
    workspace.active_workspace = WorkspaceView::Room;

    for tool in [ViewTool::Select, ViewTool::Brush] {
        workspace.active_tool = tool;
        workspace.brush_edit_mode = BrushEditMode::Move;
        // No face selected: the lock still shows, with an instruction for
        // how to reach the per-face controls.
        workspace.replace_brush_selection(0, None);
        let before = workspace.brush_texture_lock;
        run_real_egui_workspace_click_on_label(&mut workspace, "Lock texture while moving");
        assert_eq!(
            workspace.brush_texture_lock, !before,
            "the Inspector lock must toggle in {tool:?} with no face selected"
        );
        run_real_egui_workspace_click_on_label(&mut workspace, "Lock texture while moving");
        assert_eq!(workspace.brush_texture_lock, before);

        // With a face selected the per-face controls come with it.
        workspace.brush_edit_mode = BrushEditMode::Face;
        workspace.replace_brush_selection(0, Some(0));
        let (ctx, viewport) = real_egui_workspace_ctx("uv-lock-labels");
        for label in ["Texture Coordinates", "UV offset", "UV scale", "Reset UV"] {
            let _ = locate_unique_label(&ctx, &mut workspace, &viewport, label);
        }
    }

    let _ = std::fs::remove_dir_all(dir);
}

/// The UV canvas lives inside the Inspector ScrollArea. A wheel gesture over
/// it is intentionally inert: it must neither zoom the UV canvas nor scroll
/// the surrounding Inspector.
#[test]
fn uv_canvas_wheel_is_consumed_without_zooming_or_scrolling_the_inspector() {
    fn uv_canvas_rect(shapes: &[egui::epaint::ClippedShape]) -> Option<Rect> {
        fn find(shape: &egui::Shape) -> Option<Rect> {
            match shape {
                egui::Shape::Rect(rect)
                    if rect.fill == egui::Color32::from_gray(18)
                        && (160.0..=340.0).contains(&rect.rect.width())
                        && (160.0..=260.0).contains(&rect.rect.height()) =>
                {
                    Some(rect.rect)
                }
                egui::Shape::Vec(shapes) => shapes.iter().find_map(find),
                _ => None,
            }
        }
        shapes.iter().find_map(|shape| find(&shape.shape))
    }

    let template = psxed_project::new_project_template_dir();
    let dir = test_temp_dir("uv-canvas-wheel-ownership");
    crate::starter_catalogue::copy_dir_recursive(&template, &dir).unwrap();
    let mut workspace = EditorWorkspace::open_directory(&dir).unwrap();
    workspace.active_workspace = WorkspaceView::Room;
    workspace.active_tool = ViewTool::Brush;
    workspace.brush_edit_mode = BrushEditMode::Face;
    workspace.replace_brush_selection(0, Some(0));

    let (ctx, viewport) = real_egui_workspace_ctx("uv-canvas-wheel-ownership");
    let before = real_egui_workspace_frame(&ctx, &mut workspace, &viewport, 0.0, vec![]);
    let canvas = uv_canvas_rect(&before.shapes).expect("selected face draws the UV canvas");
    let heading_before = text_shape_center(&before.shapes, "Texture Coordinates")
        .expect("UV heading is visible before scrolling");
    let zoom_before = workspace
        .brush_uv_canvas_view
        .expect("canvas view was fitted")
        .pixels_per_texel;

    let after = real_egui_workspace_frame(
        &ctx,
        &mut workspace,
        &viewport,
        1.0 / 60.0,
        vec![
            egui::Event::PointerMoved(canvas.center()),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0.0, -96.0),
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    let heading_after = text_shape_center(&after.shapes, "Texture Coordinates")
        .expect("UV heading remains visible after scrolling");
    let zoom_after = workspace
        .brush_uv_canvas_view
        .expect("canvas view survives scrolling")
        .pixels_per_texel;
    assert_eq!(zoom_before, zoom_after, "wheel must not zoom the UV canvas");
    assert!(
        (heading_after.y - heading_before.y).abs() < 0.1,
        "UV wheel leaked into the Inspector scroll: {heading_before:?} -> {heading_after:?}"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// Drive one held Inspector interaction frame by frame, the way a DragValue
/// does, and report the worst anchor drift plus the mapping it ends on.
fn drag_uv(
    workspace: &mut EditorWorkspace,
    steps: usize,
    shape: impl Fn(&mut psxed_project::brush::FaceUv, usize),
) -> (f64, psxed_project::brush::FaceUv) {
    let start = workspace.project.active_scene().brushes[0].faces[0].uv;
    let anchor = workspace.project.active_scene().brushes[0]
        .face_uv_anchor(0)
        .expect("anchor");
    let held = start.apply(anchor);
    let mut worst = 0.0f64;
    for step in 0..steps {
        let current = workspace.project.active_scene().brushes[0].faces[0].uv;
        let mut edited = current;
        shape(&mut edited, step);
        let resolved = workspace.apply_face_uv_edit(0, 0, current, edited, false, true);
        workspace.project.active_scene_mut().brushes[0].faces[0].uv = resolved;
        let now = resolved.apply(anchor);
        worst = worst
            .max((now[0] - held[0]).abs())
            .max((now[1] - held[1]).abs());
    }
    (
        worst,
        workspace.project.active_scene().brushes[0].faces[0].uv,
    )
}

/// The defect this covers: re-anchoring against the PREVIOUS frame's already
/// rounded mapping banks up to half a texel of `i16` rounding every frame, so
/// a hundred one-percent steps walk the texture tens of texels off the face.
/// One held transaction solves every frame against the phase the interaction
/// started at, so the whole drag costs one rounding.
#[test]
fn a_held_uv_interaction_does_not_accumulate_rounding_drift() {
    let template = psxed_project::new_project_template_dir();
    let dir = test_temp_dir("uv-drag-drift");
    crate::starter_catalogue::copy_dir_recursive(&template, &dir).unwrap();

    // Q8 scale 256 -> 512 one step at a time, then the same edit in one shot.
    for (label, shape) in [
        (
            "scale U",
            &(|uv: &mut psxed_project::brush::FaceUv, _step: usize| {
                uv.scale_q8[0] = uv.scale_q8[0].saturating_add(1);
            }) as &dyn Fn(&mut psxed_project::brush::FaceUv, usize),
        ),
        ("scale V", &|uv: &mut psxed_project::brush::FaceUv,
                      _step: usize| {
            uv.scale_q8[1] = uv.scale_q8[1].saturating_add(1);
        }),
        ("rotation", &|uv: &mut psxed_project::brush::FaceUv,
                       _step: usize| {
            uv.rotation_deg = uv.rotation_deg.saturating_add(1);
        }),
    ] {
        let steps = if label == "rotation" { 180 } else { 256 };

        let mut incremental = EditorWorkspace::open_directory(&dir).unwrap();
        incremental.active_workspace = WorkspaceView::Room;
        incremental.replace_brush_selection(0, Some(0));
        let (worst, ended) = drag_uv(&mut incremental, steps, shape);
        assert!(
            worst <= 1.0,
            "{label}: a held interaction drifted {worst} texels over {steps} frames"
        );

        // The same total change applied in one frame has to land on the same
        // mapping the held drag ended on.
        let mut one_shot = EditorWorkspace::open_directory(&dir).unwrap();
        one_shot.active_workspace = WorkspaceView::Room;
        one_shot.replace_brush_selection(0, Some(0));
        let current = one_shot.project.active_scene().brushes[0].faces[0].uv;
        let mut edited = current;
        for step in 0..steps {
            shape(&mut edited, step);
        }
        let resolved = one_shot.apply_face_uv_edit(0, 0, current, edited, false, false);
        assert_eq!(
            resolved, ended,
            "{label}: {steps} held frames must land where the one-shot edit lands"
        );
    }

    let _ = std::fs::remove_dir_all(dir);
}

/// The transaction is scoped to one interaction on one face: releasing,
/// changing selection, undoing or sliding the offset all re-base it.
#[test]
fn a_uv_interaction_ends_on_release_selection_change_undo_and_offset_edits() {
    let template = psxed_project::new_project_template_dir();
    let dir = test_temp_dir("uv-drag-scope");
    crate::starter_catalogue::copy_dir_recursive(&template, &dir).unwrap();
    let mut workspace = EditorWorkspace::open_directory(&dir).unwrap();
    workspace.active_workspace = WorkspaceView::Room;
    workspace.replace_brush_selection(0, Some(0));
    let identity = psxed_project::brush::FaceUv::default();

    // Not interacting: the transaction closes the moment it is applied.
    let shaped = psxed_project::brush::FaceUv {
        scale_q8: [300, 256],
        ..identity
    };
    let _ = workspace.apply_face_uv_edit(0, 0, identity, shaped, false, false);
    assert!(workspace.brush_uv_edit.is_none(), "release ends it");

    // Interacting: it stays open, and a selection change drops it.
    let _ = workspace.apply_face_uv_edit(0, 0, identity, shaped, false, true);
    assert!(workspace.brush_uv_edit.is_some(), "a live drag holds it");
    workspace.replace_brush_selection(0, Some(1));
    assert!(
        workspace.brush_uv_edit.is_none(),
        "selection change ends it"
    );

    // Undo drops it.
    workspace.replace_brush_selection(0, Some(0));
    let _ = workspace.apply_face_uv_edit(0, 0, identity, shaped, false, true);
    assert!(workspace.brush_uv_edit.is_some());
    workspace.do_undo();
    assert!(workspace.brush_uv_edit.is_none(), "undo ends it");

    // Reset UV drops it and is not re-anchored.
    workspace.replace_brush_selection(0, Some(0));
    let _ = workspace.apply_face_uv_edit(0, 0, identity, shaped, false, true);
    let reset = workspace.apply_face_uv_edit(0, 0, shaped, identity, true, true);
    assert_eq!(reset, identity, "Reset UV means the identity mapping");
    assert!(workspace.brush_uv_edit.is_none(), "Reset UV ends it");

    // A pure offset edit slides by exactly what was typed and re-bases.
    let _ = workspace.apply_face_uv_edit(0, 0, identity, shaped, false, true);
    assert!(workspace.brush_uv_edit.is_some());
    let slid = psxed_project::brush::FaceUv {
        offset_texels: [9, -4],
        ..shaped
    };
    let resolved = workspace.apply_face_uv_edit(0, 0, shaped, slid, false, true);
    assert_eq!(
        resolved.offset_texels,
        [9, -4],
        "an offset edit must slide by the typed amount"
    );
    assert!(
        workspace.brush_uv_edit.is_none(),
        "an offset edit re-bases the next shaping interaction"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// The real release sequence. A pointer release or a focus loss arrives as a
/// frame where nothing changed, so ending the interaction only on a CHANGED
/// frame left the captured target alive across the release and the next edit
/// on that face solved against a mapping the user had stopped editing.
#[test]
fn an_unchanged_release_frame_ends_the_uv_interaction() {
    let template = psxed_project::new_project_template_dir();
    let dir = test_temp_dir("uv-release-frame");
    crate::starter_catalogue::copy_dir_recursive(&template, &dir).unwrap();
    let mut workspace = EditorWorkspace::open_directory(&dir).unwrap();
    workspace.active_workspace = WorkspaceView::Room;
    workspace.replace_brush_selection(0, Some(0));

    // 1. A changed shaping frame while the widget is held opens it.
    let start = workspace.project.active_scene().brushes[0].faces[0].uv;
    let shaped = psxed_project::brush::FaceUv {
        scale_q8: [400, 256],
        ..start
    };
    let held = workspace.apply_face_uv_edit(0, 0, start, shaped, false, true);
    workspace.project.active_scene_mut().brushes[0].faces[0].uv = held;
    let opened = workspace.brush_uv_edit.expect("a held drag opens one");
    assert_eq!(opened.origin, start);

    // 2. The release frame changes nothing and is not interacting.
    let unchanged = workspace.apply_face_uv_edit(0, 0, held, held, false, false);
    assert_eq!(unchanged, held, "a release frame must not move the mapping");
    assert!(
        workspace.brush_uv_edit.is_none(),
        "an unchanged release frame has to end the interaction"
    );

    // A focused but unchanged frame mid-drag keeps it, so a paused pointer
    // does not silently re-base the drag.
    let _ = workspace.apply_face_uv_edit(0, 0, held, shaped_from(held, 401), false, true);
    let paused = workspace.apply_face_uv_edit(0, 0, held, held, false, true);
    assert_eq!(paused, held);
    assert!(
        workspace.brush_uv_edit.is_some(),
        "a focused pause must not end the interaction"
    );
    let _ = workspace.apply_face_uv_edit(0, 0, held, held, false, false);

    // 3. A later edit seeds from the CURRENT mapping, not the old target.
    let anchor = workspace.project.active_scene().brushes[0]
        .face_uv_anchor(0)
        .expect("anchor");
    let next = shaped_from(held, 600);
    let resolved = workspace.apply_face_uv_edit(0, 0, held, next, false, true);
    let before = held.apply(anchor);
    let after = resolved.apply(anchor);
    assert!(
        (before[0] - after[0]).abs() <= 1.0 && (before[1] - after[1]).abs() <= 1.0,
        "the new interaction must hold the phase it started from: {before:?} -> {after:?}"
    );
    assert_eq!(
        workspace.brush_uv_edit.expect("second interaction").origin,
        held,
        "the second interaction seeds from the mapping the first one left"
    );

    let _ = std::fs::remove_dir_all(dir);
}

fn shaped_from(uv: psxed_project::brush::FaceUv, scale_u: i16) -> psxed_project::brush::FaceUv {
    psxed_project::brush::FaceUv {
        scale_q8: [scale_u, uv.scale_q8[1]],
        ..uv
    }
}
