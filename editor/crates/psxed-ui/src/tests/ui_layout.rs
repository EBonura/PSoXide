use super::*;

#[test]
fn ui_resize_handles_remain_hittable_outside_canvas_for_border_images() {
    let mut scene = psxed_project::UiScene::default_hud();
    let image = scene.add_node(
        scene.root,
        "Image".to_string(),
        UiNodeKind::Image {
            rect: UiRect::new(0, 0, 64, 64),
            texture: None,
            tag: String::new(),
            tint: [128, 128, 128],
            effect: UiImageEffect::None,
        },
    );
    let canvas = Rect::from_min_size(Pos2::ZERO, Vec2::new(320.0, 240.0));
    let hidden_ui_nodes = HashSet::new();

    assert_eq!(
        ui_scene_resize_handle_target(
            &scene,
            &hidden_ui_nodes,
            image,
            canvas,
            [320, 240],
            Pos2::new(-4.0, -4.0)
        ),
        Some((image, UiResizeHandle::TopLeft))
    );
}

#[test]
fn ui_center_snap_aligns_node_to_canvas_midpoints() {
    let result = snap_ui_rect_to_canvas_center(UiRect::new(121, 109, 80, 20), [320, 240]);

    assert!(result.snap_x);
    assert!(result.snap_y);
    assert_eq!(result.rect, UiRect::new(120, 110, 80, 20));
}

#[test]
fn ui_center_snap_can_snap_one_axis_without_the_other() {
    let result = snap_ui_rect_to_canvas_center(UiRect::new(117, 114, 80, 20), [320, 240]);

    assert!(result.snap_x);
    assert!(!result.snap_y);
    assert_eq!(result.rect, UiRect::new(120, 114, 80, 20));
}

#[test]
fn ui_center_snap_applies_absolute_delta_to_anchored_local_rects() {
    let local = UiRect::new(0, 0, 80, 20).with_anchor(UiAnchor::Center);
    let absolute = UiRect::new(121, 109, 80, 20);

    let result = snap_moved_ui_rect_to_canvas_center(local, absolute, [320, 240]);

    assert!(result.snap_x);
    assert!(result.snap_y);
    assert_eq!(
        result.rect,
        UiRect::new(-1, 1, 80, 20).with_anchor(UiAnchor::Center)
    );
}

#[test]
fn ui_center_snap_leaves_rects_outside_tolerance_unchanged() {
    let rect = UiRect::new(112, 120, 80, 20);

    let result = snap_ui_rect_to_canvas_center(rect, [320, 240]);

    assert!(!result.snap_x);
    assert!(!result.snap_y);
    assert_eq!(result.rect, rect);
}
