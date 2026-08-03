//! Phase-3 cook coverage: LOGIC / GAME_ENTITIES record emission from
//! the existing authored node types (synthetic documents -- cortex
//! itself cooks zero of either), name interning, malformed-content
//! rejection, and the contract caps. This is the producer half of the
//! record contract; the runtime consumer half lives in
//! `psx-game-runtime`'s `entities`/`logic` tests.

use super::*;
use crate::{EnemyBehaviorSettings, LogicNodeKind};

fn first_material_id(project: &ProjectDocument) -> ResourceId {
    project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .map(|resource| resource.id)
        .expect("starter has a material")
}

fn room_node_id(project: &ProjectDocument) -> NodeId {
    project
        .active_scene()
        .nodes()
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Section { .. }))
        .map(|n| n.id)
        .expect("room exists")
}

fn add_box_prop(project: &mut ProjectDocument, name: &str) {
    let material = Some(first_material_id(project));
    let room_id = room_node_id(project);
    let scene = project.active_scene_mut();
    scene.add_node(
        room_id,
        name,
        NodeKind::BoxProp {
            materials: [material; crate::BOX_PROP_FACE_COUNT],
            uvs: [crate::GridUvTransform::IDENTITY; crate::BOX_PROP_FACE_COUNT],
            vertices: crate::default_box_prop_vertices(),
            collision_enabled: true,
            break_flags: 0,
            erosion: crate::BoxPropErosion::default(),
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn add_logic_node(
    project: &mut ProjectDocument,
    name: &str,
    kind: LogicNodeKind,
    target: &str,
    delay_ticks: u16,
    wait_ticks: i16,
) {
    let room_id = room_node_id(project);
    let scene = project.active_scene_mut();
    scene.add_node(
        room_id,
        name,
        NodeKind::Logic {
            kind,
            target: target.to_string(),
            killtarget: String::new(),
            master: String::new(),
            delay_ticks,
            wait_ticks,
            enabled: true,
        },
    );
}

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
        .find(|n| matches!(n.kind, NodeKind::Section { .. }))
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
        .find(|n| matches!(n.kind, NodeKind::Section { .. }))
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
    assert_eq!(a.reaction_ticks, enemy.reaction_ticks);
    assert_eq!(a.preferred_distance, enemy.preferred_distance);
    assert_eq!(a.spacing_tolerance, enemy.spacing_tolerance);
    assert_eq!(a.decision_interval_ticks, enemy.decision_interval_ticks);
    assert_eq!(a.circle_chance, enemy.circle_chance);
    assert_eq!(a.attack_priority, enemy.attack_priority);
    assert_eq!(a.attack_cooldown_ticks, enemy.attack_cooldown_ticks);
    assert_eq!(a.group_attack_delay_ticks, enemy.group_attack_delay_ticks);
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
    // State clips resolved from the Character's AnimationSet roles:
    // every one indexes inside the instance model's clip slice, the
    // starter set authors distinct idle/walk/run, and the idle clip
    // matches the cooked idle instance's clip.
    let instance = &package.model_instances[usize::from(a.model_instance)];
    let clip_count = package.models[usize::from(instance.model)].clip_count;
    for clip in [
        a.idle_clip,
        a.walk_clip,
        a.run_clip,
        a.attack_clip,
        a.stagger_clip,
        a.death_clip,
    ] {
        assert!(clip < clip_count, "state clip {clip} out of {clip_count}");
    }
    assert_eq!(a.idle_clip, instance.clip);
    assert_ne!(a.walk_clip, a.idle_clip);
    assert_ne!(a.run_clip, a.walk_clip);
    assert_eq!(
        (a.idle_clip, a.walk_clip, a.run_clip),
        (b.idle_clip, b.walk_clip, b.run_clip),
        "same archetype resolves the same clips"
    );
}

#[test]
fn game_entity_state_clips_fall_back_down_the_chain() {
    // Strip the character's AnimationSet to idle + walk only: run
    // must fall back to walk, and attack/stagger/death to idle. The
    // player resolves its own actions from the Animator overrides,
    // so thinning the set only affects the enemy path.
    let mut project = project_with_one_room();
    let character = player_character_resource_id(&project);
    let set_id = match &project.resource(character).expect("character").data {
        ResourceData::Character(character) => character.animation_set.expect("starter set"),
        _ => unreachable!(),
    };
    match &mut project.resource_mut(set_id).expect("set").data {
        ResourceData::AnimationSet(set) => {
            set.run_clip = None;
            set.turn_clip = None;
            set.roll_clip = None;
            set.backstep_clip = None;
            set.action_clips.clear();
            set.clips.clear();
        }
        _ => unreachable!(),
    }
    add_enemy_entity(
        &mut project,
        "Grunt",
        Some(character),
        EnemyBehaviorSettings::defaults(),
    );
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("cooks");
    let entity = &package.game_entities[0];
    assert_ne!(entity.walk_clip, entity.idle_clip, "walk stays authored");
    assert_eq!(entity.run_clip, entity.walk_clip, "run falls back to walk");
    assert_eq!(entity.attack_clip, entity.idle_clip);
    assert_eq!(entity.stagger_clip, entity.idle_clip);
    assert_eq!(entity.death_clip, entity.idle_clip);
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
        .find(|n| matches!(n.kind, NodeKind::Section { .. }))
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
fn trigger_relay_door_chain_cooks_with_resolved_box_link() {
    let mut project = project_with_one_room();
    add_box_prop(&mut project, "Gate Box");
    add_logic_node(
        &mut project,
        "Entry Trigger",
        LogicNodeKind::TriggerVolume {
            size: [768, 1024, 512],
        },
        "Gate Relay",
        0,
        -1,
    );
    add_logic_node(
        &mut project,
        "Gate Relay",
        LogicNodeKind::Relay,
        "Gate Door",
        30,
        0,
    );
    add_logic_node(
        &mut project,
        "Gate Door",
        LogicNodeKind::Door {
            box_prop: "Gate Box".to_string(),
            start_open: false,
        },
        "",
        0,
        0,
    );

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("cooks");
    assert_eq!(package.logic.len(), 3);
    assert_eq!(package.box_props.len(), 1);

    let trigger = &package.logic[0];
    let relay = &package.logic[1];
    let door = &package.logic[2];
    assert_eq!(trigger.kind, psx_level::logic_kind::TRIGGER_VOLUME);
    assert_eq!(relay.kind, psx_level::logic_kind::RELAY);
    assert_eq!(door.kind, psx_level::logic_kind::DOOR);
    // Graph edges intern to the same ids the named records carry.
    assert_eq!(trigger.target, relay.targetname);
    assert_eq!(relay.target, door.targetname);
    assert_eq!(trigger.wait_ticks, -1);
    assert_eq!(relay.delay_ticks, 30);
    // The trigger AABB is the authored extent, floor-anchored.
    assert_eq!(trigger.max[0] - trigger.min[0], 768);
    assert_eq!(trigger.max[1] - trigger.min[1], 1024);
    assert_eq!(trigger.max[2] - trigger.min[2], 512);
    // The door links the cooked box prop by index.
    assert_eq!(door.link, 0);
    // START_ON follows start_open (false here).
    assert_eq!(door.flags & psx_level::logic_flags::START_ON, 0);
}

#[test]
fn door_box_links_fail_loudly_when_missing_or_ambiguous() {
    // Unknown box name.
    let mut project = project_with_one_room();
    add_logic_node(
        &mut project,
        "Doomed Door",
        LogicNodeKind::Door {
            box_prop: "No Such Box".to_string(),
            start_open: false,
        },
        "",
        0,
        0,
    );
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(package.is_none());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.contains("No Such Box") && e.contains("not a placed")),
        "errors: {:?}",
        report.errors
    );

    // Ambiguous box name (two boxes share it).
    let mut project = project_with_one_room();
    add_box_prop(&mut project, "Twin Box");
    add_box_prop(&mut project, "Twin Box");
    add_logic_node(
        &mut project,
        "Confused Door",
        LogicNodeKind::Door {
            box_prop: "Twin Box".to_string(),
            start_open: false,
        },
        "",
        0,
        0,
    );
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(package.is_none());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.contains("2 placed boxes share")),
        "errors: {:?}",
        report.errors
    );

    // Door with no box named at all.
    let mut project = project_with_one_room();
    add_logic_node(
        &mut project,
        "Empty Door",
        LogicNodeKind::Door {
            box_prop: String::new(),
            start_open: false,
        },
        "",
        0,
        0,
    );
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(package.is_none());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.contains("names no Box Prop")),
        "errors: {:?}",
        report.errors
    );
}

#[test]
fn dead_triggers_and_relays_are_rejected() {
    let mut project = project_with_one_room();
    add_logic_node(
        &mut project,
        "Dead Trigger",
        LogicNodeKind::TriggerVolume {
            size: [768, 1024, 768],
        },
        "",
        0,
        0,
    );
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(package.is_none());
    assert!(
        report.errors.iter().any(|e| e.contains("has no target")),
        "errors: {:?}",
        report.errors
    );

    let mut project = project_with_one_room();
    add_logic_node(&mut project, "Dead Relay", LogicNodeKind::Relay, "", 5, 0);
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(package.is_none());
    assert!(
        report.errors.iter().any(|e| e.contains("has no target")),
        "errors: {:?}",
        report.errors
    );

    // Zero-size trigger volumes are dead content too.
    let mut project = project_with_one_room();
    add_logic_node(
        &mut project,
        "Flat Trigger",
        LogicNodeKind::TriggerVolume {
            size: [768, 0, 768],
        },
        "Somewhere",
        0,
        0,
    );
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(package.is_none());
    assert!(
        report.errors.iter().any(|e| e.contains("zero-size")),
        "errors: {:?}",
        report.errors
    );
}

#[test]
fn interactables_carry_their_paired_logic_index_between_placed_nodes() {
    // Placed logic records interleave with interactable-paired ones;
    // the interactable must point at ITS record, not rely on table
    // position.
    let mut project = project_with_one_room();
    add_logic_node(
        &mut project,
        "Early Relay",
        LogicNodeKind::Relay,
        "Echo Stone",
        0,
        0,
    );
    add_interactable_entity(
        &mut project,
        "Echo Stone",
        crate::InteractableKind::Message {
            title: "T".to_string(),
            body: "B".to_string(),
        },
        96,
    );
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("cooks");
    assert_eq!(package.logic.len(), 2);
    assert_eq!(package.interactables.len(), 1);
    let interactable = &package.interactables[0];
    let paired = &package.logic[usize::from(interactable.logic)];
    assert_eq!(paired.kind, psx_level::logic_kind::MESSAGE);
    assert_eq!(paired.message, interactable.message);
    // The relay's edge resolves to the paired record's interned name.
    assert_eq!(package.logic[0].target, paired.targetname);
}

#[test]
fn game_entities_cook_character_bound_body_and_speeds() {
    let mut project = project_with_one_room();
    let character = player_character_resource_id(&project);
    add_enemy_entity(
        &mut project,
        "Runner",
        Some(character),
        EnemyBehaviorSettings::defaults(),
    );
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("cooks");
    let entity = &package.game_entities[0];
    let defaults = crate::CharacterControllerSettings::defaults();
    assert_eq!(entity.radius, defaults.radius);
    assert_eq!(entity.height, defaults.height);
    assert_eq!(entity.walk_speed, defaults.walk_speed);
    assert_eq!(entity.run_speed, defaults.run_speed);

    // Malformed speeds fail loudly.
    let mut project = project_with_one_room();
    let character = player_character_resource_id(&project);
    let room_id = room_node_id(&project);
    let scene = project.active_scene_mut();
    let entity_id = scene.add_node(room_id, "Slug", NodeKind::Entity);
    let mut settings = crate::CharacterControllerSettings::defaults();
    settings.walk_speed = 0;
    settings.enemy = Some(EnemyBehaviorSettings::defaults());
    scene.add_node(
        entity_id,
        "Character Controller",
        NodeKind::CharacterController {
            character: Some(character),
            settings,
            player: false,
        },
    );
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(package.is_none());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.contains("non-positive walk/run speed")),
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
