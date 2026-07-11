//! Phase-3 cook coverage: LOGIC / GAME_ENTITIES record emission from
//! the existing authored node types (synthetic documents -- cortex
//! itself cooks zero of either), name interning, malformed-content
//! rejection, and the contract caps. This is the producer half of the
//! record contract; the runtime consumer half lives in
//! `psx-game-runtime`'s `entities`/`logic` tests.

use super::*;
use crate::EnemyBehaviorSettings;

fn add_interactable_entity(
    project: &mut ProjectDocument,
    name: &str,
    kind: crate::InteractableKind,
    radius: u16,
) {
    let scene = project.active_scene_mut();
    let room_id = scene
        .nodes()
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Room { .. }))
        .map(|n| n.id)
        .expect("room exists");
    let entity_id = scene.add_node(room_id, name, NodeKind::Entity);
    scene.add_node(
        entity_id,
        "Interactable",
        NodeKind::Interactable {
            kind,
            prompt: String::new(),
            radius,
            enabled: true,
        },
    );
}

fn add_enemy_entity(
    project: &mut ProjectDocument,
    name: &str,
    character: Option<ResourceId>,
    enemy: EnemyBehaviorSettings,
) {
    let scene = project.active_scene_mut();
    let room_id = scene
        .nodes()
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Room { .. }))
        .map(|n| n.id)
        .expect("room exists");
    let entity_id = scene.add_node(room_id, name, NodeKind::Entity);
    let mut settings = crate::CharacterControllerSettings::defaults();
    settings.enemy = Some(enemy);
    scene.add_node(
        entity_id,
        "Character Controller",
        NodeKind::CharacterController {
            character,
            settings,
            player: false,
        },
    );
}

fn starter_character_name(project: &ProjectDocument) -> String {
    let id = player_character_resource_id(project);
    project
        .resource(id)
        .map(|resource| resource.name.clone())
        .expect("starter character resource has a name")
}

#[test]
fn interactables_cook_paired_logic_records() {
    let mut project = project_with_one_room();
    add_interactable_entity(
        &mut project,
        "Echo Stone",
        crate::InteractableKind::Message {
            title: "TITLE".to_string(),
            body: "BODY".to_string(),
        },
        96,
    );
    add_interactable_entity(
        &mut project,
        "Relay Bench",
        crate::InteractableKind::Checkpoint {
            checkpoint_id: "cp_1".to_string(),
            title: String::new(),
            body: String::new(),
        },
        128,
    );
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("cooks");

    assert_eq!(package.interactables.len(), 2);
    assert_eq!(package.logic.len(), 2);

    let message = &package.logic[0];
    assert_eq!(message.kind, psx_level::logic_kind::MESSAGE);
    assert_ne!(message.targetname, psx_level::LOGIC_NAME_NONE);
    assert_eq!(message.message, package.interactables[0].message);
    assert_eq!(message.target, psx_level::LOGIC_NAME_NONE);
    assert_eq!(message.flags & psx_level::logic_flags::ENABLED, 1);
    // XZ-radius bounds around the origin, zero height.
    assert_eq!(message.min[0], message.x - 96);
    assert_eq!(message.max[0], message.x + 96);
    assert_eq!(message.min[1], message.y);
    assert_eq!(message.max[1], message.y);

    let checkpoint = &package.logic[1];
    assert_eq!(checkpoint.kind, psx_level::logic_kind::CHECKPOINT);
    assert_ne!(checkpoint.targetname, message.targetname);
}

#[test]
fn enemy_controller_cooks_game_entity_with_interned_archetype() {
    let mut project = project_with_one_room();
    let character = player_character_resource_id(&project);
    let archetype_name = starter_character_name(&project);
    assert!(!archetype_name.trim().is_empty());
    let enemy = EnemyBehaviorSettings {
        patrol_offset: [512, 0, 0],
        ..EnemyBehaviorSettings::defaults()
    };
    add_enemy_entity(&mut project, "Grunt A", Some(character), enemy);
    add_enemy_entity(&mut project, "Grunt B", Some(character), enemy);

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("cooks");

    assert_eq!(package.game_entities.len(), 2);
    let [a, b] = [&package.game_entities[0], &package.game_entities[1]];
    // One archetype, one interned kind id; distinct targetnames.
    assert_ne!(a.kind, psx_level::LOGIC_NAME_NONE);
    assert_eq!(a.kind, b.kind);
    assert_ne!(a.targetname, b.targetname);
    // Patrol anchor one is spawn + offset.
    assert_eq!(a.patrol[0], a.x + 512);
    assert_eq!(a.patrol[1], a.y);
    assert_eq!(a.patrol[2], a.z);
    // Archetype params flow through.
    assert_eq!(a.aggro_radius, enemy.aggro_radius);
    assert_eq!(a.windup_ticks, enemy.windup_ticks);
    assert_eq!(a.recovery_ticks, enemy.recovery_ticks);
    assert_eq!(a.poise, enemy.poise);
    assert_eq!(a.touch_damage, enemy.touch_damage);
    assert_eq!(a.max_health, enemy.max_health);
    assert_eq!(a.flags & psx_level::game_entity_flags::ENABLED, 1);
    // The non-player controller still cooks its idle model instance
    // and the record links it by index.
    assert_ne!(a.model_instance, psx_level::GAME_ENTITY_MODEL_INSTANCE_NONE);
    assert!(usize::from(a.model_instance) < package.model_instances.len());
    assert_ne!(a.model_instance, b.model_instance);
}

#[test]
fn plain_non_player_controller_cooks_no_game_entity() {
    // Back-compat: without the enemy opt-in the controller keeps the
    // pre-phase-3 semantics (idle model instance only).
    let mut project = project_with_one_room();
    let character = player_character_resource_id(&project);
    let scene = project.active_scene_mut();
    let room_id = scene
        .nodes()
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Room { .. }))
        .map(|n| n.id)
        .expect("room exists");
    let entity_id = scene.add_node(room_id, "Idle NPC", NodeKind::Entity);
    scene.add_node(
        entity_id,
        "Character Controller",
        NodeKind::CharacterController {
            character: Some(character),
            settings: crate::CharacterControllerSettings::defaults(),
            player: false,
        },
    );
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("cooks");
    assert!(package.game_entities.is_empty());
    assert!(package.logic.is_empty());
}

#[test]
fn malformed_enemies_fail_the_cook_loudly() {
    let character_of = |project: &ProjectDocument| player_character_resource_id(project);

    // Zero aggro radius.
    let mut project = project_with_one_room();
    let character = character_of(&project);
    add_enemy_entity(
        &mut project,
        "Bad Aggro",
        Some(character),
        EnemyBehaviorSettings {
            aggro_radius: 0,
            ..EnemyBehaviorSettings::defaults()
        },
    );
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(package.is_none());
    assert!(
        report.errors.iter().any(|e| e.contains("aggro radius 0")),
        "errors: {:?}",
        report.errors
    );

    // Zero windup (no telegraph).
    let mut project = project_with_one_room();
    let character = character_of(&project);
    add_enemy_entity(
        &mut project,
        "Bad Windup",
        Some(character),
        EnemyBehaviorSettings {
            windup_ticks: 0,
            ..EnemyBehaviorSettings::defaults()
        },
    );
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(package.is_none());
    assert!(
        report.errors.iter().any(|e| e.contains("windup 0")),
        "errors: {:?}",
        report.errors
    );

    // Zero health.
    let mut project = project_with_one_room();
    let character = character_of(&project);
    add_enemy_entity(
        &mut project,
        "Bad Health",
        Some(character),
        EnemyBehaviorSettings {
            max_health: 0,
            ..EnemyBehaviorSettings::defaults()
        },
    );
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(package.is_none());
    assert!(
        report.errors.iter().any(|e| e.contains("max health 0")),
        "errors: {:?}",
        report.errors
    );

    // Enemy with no Character (the archetype tag source).
    let mut project = project_with_one_room();
    add_enemy_entity(
        &mut project,
        "Tagless",
        None,
        EnemyBehaviorSettings::defaults(),
    );
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(package.is_none());
    assert!(
        report.errors.iter().any(|e| e.contains("has no Character")),
        "errors: {:?}",
        report.errors
    );
}

#[test]
fn logic_record_cap_rejects_over_cap_content() {
    let mut project = project_with_one_room();
    for index in 0..(psx_level::MAX_LOGIC_RECORDS + 1) {
        add_interactable_entity(
            &mut project,
            &format!("Echo {index}"),
            crate::InteractableKind::Message {
                title: "T".to_string(),
                body: String::new(),
            },
            96,
        );
    }
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(package.is_none());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.contains("MAX_LOGIC_RECORDS")),
        "errors: {:?}",
        report.errors
    );
}

#[test]
fn manifest_renders_the_phase3_tables() {
    let mut project = project_with_one_room();
    let character = player_character_resource_id(&project);
    add_enemy_entity(
        &mut project,
        "Grunt",
        Some(character),
        EnemyBehaviorSettings::defaults(),
    );
    add_interactable_entity(
        &mut project,
        "Echo",
        crate::InteractableKind::Message {
            title: "T".to_string(),
            body: String::new(),
        },
        96,
    );
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let source = render_manifest_source(&package.expect("cooks"));
    assert!(source.contains("pub static LOGIC: &[LevelLogicRecord] = &["));
    assert!(source.contains("pub static GAME_ENTITIES: &[LevelGameEntityRecord] = &["));
    assert!(source.contains("LevelLogicRecord { room: RoomIndex(0), kind: 1,"));
    assert!(source.contains("LevelGameEntityRecord { room: RoomIndex(0), kind: "));
    // Empty projects render empty (but present) tables, so the
    // runtime's imports always resolve.
    let (empty_package, empty_report) =
        build_package(&project_with_one_room(), &starter_project_root());
    assert!(empty_report.is_ok());
    let empty_source = render_manifest_source(&empty_package.expect("cooks"));
    assert!(empty_source.contains("pub static LOGIC: &[LevelLogicRecord] = &[\n];"));
    assert!(empty_source.contains("pub static GAME_ENTITIES: &[LevelGameEntityRecord] = &[\n];"));
}
