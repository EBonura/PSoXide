use super::*;
use crate::{NodeId, ProjectDocument};

#[test]
fn authored_vitality_circle_cooks_as_dedicated_record() {
    let mut project = ProjectDocument::starter();
    let scene = project.active_scene_mut();
    let id = scene.add_node(
        scene.root,
        "Zenith Field",
        crate::NodeKind::VitalityCircle {
            axis: crate::VitalityCircleAxis::Zenith,
            radius: 333,
            refill_per_second: 17,
            drain_per_second: 11,
            enabled: true,
        },
    );
    scene.node_mut(id).unwrap().transform.translation = [24.0, 8.0, -48.0];
    let (package, report) = build_package(&project, &crate::default_project_dir());
    assert!(report.is_ok(), "circle fixture cooks: {:?}", report.errors);
    let circle = package.expect("package").vitality_circles[0];
    assert_eq!(circle.axis, 1);
    assert_eq!(circle.radius, 21, "world-unit conversion rounds up");
    assert_eq!(circle.refill_per_second, 17);
    assert_eq!(circle.drain_per_second, 11);
}

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

#[test]
fn combat_sections_roundtrip_and_cook_as_independent_strikes_with_shared_geometry() {
    let mut character = crate::CharacterResource::defaults();
    let mut volume = crate::CharacterCombatCapsule {
        name: "Three-swing claw".into(),
        joint: 9,
        capsule: crate::JointCapsule {
            start: [19516, 11992, 1101],
            end: [31889, 11992, 1101],
            radius: 86,
        },
        role: crate::CombatCapsuleRole::Hitbox {
            action: crate::CharacterAnimationAction::HeavyAttack,
            active_start_frame: 8,
            active_end_frame: 11,
            damage: 25,
            poise_damage: 25,
        },
        additional_hit_windows: vec![
            crate::CombatHitWindow { start: 19, end: 22 },
            crate::CombatHitWindow { start: 31, end: 34 },
        ],
        ..Default::default()
    };
    volume.name = "Renamed claw".into();
    character.combat_capsules.push(volume.clone());
    let restored: crate::CharacterResource =
        ron::from_str(&ron::to_string(&character).unwrap()).unwrap();
    assert_eq!(restored, character);
    let mut records = Vec::new();
    let mut report = PlaytestValidationReport::default();
    assert_eq!(
        cook_character_combat_capsules(
            &ProjectDocument::new("sections"),
            "Fighter",
            &restored,
            10,
            &mut records,
            &mut report
        ),
        Some((0, 3))
    );
    assert!(report.is_ok());
    assert_eq!(
        records
            .iter()
            .map(|r| (r.active_start_frame, r.active_end_frame))
            .collect::<Vec<_>>(),
        [(8, 11), (19, 22), (31, 34)]
    );
    for r in &records {
        assert_eq!(r.joint, 9);
        assert_eq!(r.start, [19516, 11992, 1101]);
        assert_eq!(r.end, [31889, 11992, 1101]);
        assert_eq!(r.radius, 86);
        assert_eq!(
            r.action,
            crate::CharacterAnimationAction::HeavyAttack.to_index() as u8
        );
        assert_eq!((r.damage, r.poise_damage), (25, 25));
    }
}

#[test]
fn combat_sections_reject_invalid_ranges_and_expanded_runtime_overflow_without_partial_output() {
    let mut character = crate::CharacterResource::defaults();
    character
        .combat_capsules
        .push(crate::CharacterCombatCapsule {
            role: crate::CombatCapsuleRole::Hitbox {
                action: crate::CharacterAnimationAction::HeavyAttack,
                active_start_frame: 8,
                active_end_frame: 11,
                damage: 25,
                poise_damage: 25,
            },
            ..Default::default()
        });
    for windows in [
        vec![crate::CombatHitWindow { start: 20, end: 19 }],
        vec![
            crate::CombatHitWindow { start: 20, end: 23 };
            psx_level::MAX_CHARACTER_COMBAT_CAPSULES
        ],
    ] {
        character.combat_capsules[0].additional_hit_windows = windows;
        let mut records = Vec::new();
        let mut report = PlaytestValidationReport::default();
        assert_eq!(
            cook_character_combat_capsules(
                &ProjectDocument::new("invalid"),
                "Fighter",
                &character,
                10,
                &mut records,
                &mut report
            ),
            None
        );
        assert!(!report.is_ok());
        assert!(records.is_empty());
    }
}

#[test]
fn legacy_combat_volume_keeps_its_single_section() {
    let volume: crate::CharacterCombatCapsule = ron::from_str(r#"(name: "Claw", role: Hitbox(action: LightAttack, active_start_frame: 8, active_end_frame: 11, damage: 25, poise_damage: 25))"#).unwrap();
    assert!(volume.additional_hit_windows.is_empty());
    assert_eq!(
        volume.hit_windows().collect::<Vec<_>>(),
        [crate::CombatHitWindow { start: 8, end: 11 }]
    );
}
