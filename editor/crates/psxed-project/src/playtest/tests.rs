use super::*;
use crate::{NodeId, ProjectDocument};

#[test]
fn cook_output_capture_mirrors_main_and_worker_diagnostics() {
    let (value, lines) = capture_cook_output(|| {
        emit_cook_output(format_args!("[cook-capture-test] main"));
        std::thread::scope(|scope| {
            scope.spawn(|| emit_cook_output(format_args!("[cook-capture-test] worker")));
        });
        42
    });

    assert_eq!(value, 42);
    assert!(lines.iter().any(|line| line == "[cook-capture-test] main"));
    assert!(lines
        .iter()
        .any(|line| line == "[cook-capture-test] worker"));
}

#[test]
fn brush_cook_diagnostics_keep_a_typed_editor_focus_target() {
    let target =
        brush_world_validation_target(&crate::brush_world::BrushWorldCookError::InvalidBrush {
            brush: 7,
            face: Some(2),
        });
    assert_eq!(
        target,
        Some(PlaytestValidationTarget::Brush {
            brush: 7,
            face: Some(2),
        })
    );

    let node = NodeId(91);
    assert_eq!(
        brush_world_validation_target(
            &crate::brush_world::BrushWorldCookError::PlayerSpawnInSolid(node)
        ),
        Some(PlaytestValidationTarget::Node(node))
    );

    let resource = ResourceId(41);
    assert_eq!(
        brush_world_validation_target(&crate::brush_world::BrushWorldCookError::MissingMaterial(
            resource
        )),
        Some(PlaytestValidationTarget::Resource(resource))
    );

    let mut project = ProjectDocument::new("invalid brush package");
    let mut invalid = crate::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]);
    invalid.faces.truncate(3);
    invalid.faces[0].points = [[0; 3]; 3];
    project.active_scene_mut().brushes.push(invalid);
    let (package, report) = build_package(&project, Path::new("."));
    assert!(package.is_none());
    assert_eq!(
        report.focus_target(),
        Some(PlaytestValidationTarget::Brush {
            brush: 0,
            face: Some(0),
        })
    );
    assert!(report.errors[0].contains("brush 0 has invalid face 0"));
}

/// The report keeps a target PER ERROR, not one for the whole report, and the
/// `focus_target` convenience still answers with the first focusable one.
/// `blaming` fills in only the errors that named nothing themselves.
#[test]
fn every_report_error_keeps_its_own_focus_target() {
    let node_a = crate::NodeId(7);
    let node_b = crate::NodeId(9);
    let resource = crate::ResourceId(3);

    let mut report = PlaytestValidationReport::default();
    report.error("no offender for this one");
    report.error_at(PlaytestValidationTarget::Node(node_a), "first offender");
    report.blaming(PlaytestValidationTarget::Node(node_b), |report| {
        report.error("raised by a helper that only knows a name");
        report.error_at(PlaytestValidationTarget::Resource(resource), "knows better");
    });

    let targets: Vec<_> = report.errors.iter().map(|error| error.target).collect();
    assert_eq!(
        targets,
        vec![
            None,
            Some(PlaytestValidationTarget::Node(node_a)),
            Some(PlaytestValidationTarget::Node(node_b)),
            Some(PlaytestValidationTarget::Resource(resource)),
        ],
        "blaming fills in untargeted errors and leaves precise ones alone"
    );
    assert_eq!(
        report.focus_target(),
        Some(PlaytestValidationTarget::Node(node_a)),
        "the convenience accessor skips the untargeted first error"
    );
    assert_eq!(
        report.error_messages().join("; "),
        "no offender for this one; first offender; \
raised by a helper that only knows a name; knows better"
    );
}
