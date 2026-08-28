use std::path::PathBuf;

use egui::{Pos2, Rect, Vec2};
use psxed_project::{
    AnimationPoseCorrectionKey, CharacterAnimationAction, CombatCapsuleRole, ProjectDocument,
    ResourceData, ResourceId,
};

use crate::model_animation_viewer::{
    moveset_capability_rows, MovesetBindingSource, MovesetCapabilityStatus,
};
use crate::{icons, EditorWorkspace};

use super::brush_tools::{
    press_release, real_egui_workspace_ctx, real_egui_workspace_frame, text_shape_centers,
};

fn default_project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../projects/default")
}

fn default_workspace() -> EditorWorkspace {
    let project_dir = default_project_dir();
    let project = ProjectDocument::load_from_path(project_dir.join("project.ron"))
        .expect("default project parses");
    // `open_directory` deliberately syncs the starter catalogue and may write
    // upgraded resources back to disk. These tests need the production
    // workspace without ever mutating the tracked fixture they exercise.
    EditorWorkspace::with_project(project_dir, project)
}

fn resource_id(
    workspace: &EditorWorkspace,
    name: &str,
    accepts: impl Fn(&ResourceData) -> bool,
) -> ResourceId {
    workspace
        .project()
        .resources
        .iter()
        .find(|resource| resource.name == name && accepts(&resource.data))
        .map(|resource| resource.id)
        .unwrap_or_else(|| panic!("default project is missing {name:?}"))
}

fn character_context(
    workspace: &EditorWorkspace,
) -> (ResourceId, ResourceId, ResourceId, ResourceId) {
    let character_id = resource_id(workspace, "Aletha", |data| {
        matches!(data, ResourceData::Character(_))
    });
    let ResourceData::Character(character) = &workspace
        .project()
        .resource(character_id)
        .expect("Aletha character")
        .data
    else {
        unreachable!()
    };
    let model_id = character.model.expect("Aletha model");
    let animation_set_id = character.animation_set.expect("Aletha animation set");
    let ResourceData::AnimationSet(animation_set) = &workspace
        .project()
        .resource(animation_set_id)
        .expect("Aletha animation set")
        .data
    else {
        unreachable!()
    };
    let idle_clip_id = animation_set
        .action_clip(CharacterAnimationAction::Idle)
        .expect("Aletha idle action");
    (character_id, model_id, animation_set_id, idle_clip_id)
}

fn locate_unique_label(frame: &egui::FullOutput, label: &str) -> Pos2 {
    let found = text_shape_centers(&frame.shapes, label);
    assert_eq!(
        found.len(),
        1,
        "label {label:?} must be visible exactly once, saw {found:?}"
    );
    let point = found[0];
    let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1800.0, 1000.0));
    assert!(
        screen.contains(point),
        "label {label:?} is painted outside the interactive screen at {point:?}"
    );
    let mut clips = Vec::new();
    for clipped in &frame.shapes {
        fn collect_clips(shape: &egui::Shape, clip_rect: Rect, label: &str, clips: &mut Vec<Rect>) {
            match shape {
                egui::Shape::Text(text) if text.galley.text() == label => clips.push(clip_rect),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect_clips(shape, clip_rect, label, clips);
                    }
                }
                _ => {}
            }
        }
        collect_clips(&clipped.shape, clipped.clip_rect, label, &mut clips);
    }
    assert!(
        clips.iter().any(|clip| clip.contains(point)),
        "label {label:?} is painted at {point:?} but clipped by {clips:?}"
    );
    point
}

fn click_label(
    ctx: &egui::Context,
    workspace: &mut EditorWorkspace,
    viewport: &crate::EditorViewport3dPresentation,
    time: &mut f64,
    label: &str,
) -> egui::FullOutput {
    let frame = real_egui_workspace_frame(ctx, workspace, viewport, *time, Vec::new());
    let point = locate_unique_label(&frame, label);
    let (press, release) = press_release(point);
    *time += 1.0 / 60.0;
    let _ = real_egui_workspace_frame(ctx, workspace, viewport, *time, press);
    *time += 1.0 / 60.0;
    let _ = real_egui_workspace_frame(ctx, workspace, viewport, *time, release);
    *time += 1.0 / 60.0;
    real_egui_workspace_frame(ctx, workspace, viewport, *time, Vec::new())
}

fn scroll_authoring_label_into_view(
    ctx: &egui::Context,
    workspace: &mut EditorWorkspace,
    viewport: &crate::EditorViewport3dPresentation,
    time: &mut f64,
    label: &str,
) {
    let pointer = Pos2::new(1300.0, 420.0);
    for delta_y in std::iter::once(10_000.0).chain(std::iter::repeat(-160.0).take(16)) {
        *time += 1.0 / 60.0;
        let frame = real_egui_workspace_frame(
            ctx,
            workspace,
            viewport,
            *time,
            vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: Vec2::new(0.0, delta_y),
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        if text_shape_centers(&frame.shapes, label).len() == 1 {
            return;
        }
    }
    panic!("authoring label {label:?} was not reachable by scrolling");
}

fn key_event(key: egui::Key, pressed: bool) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }
}

#[test]
fn animation_studio_modes_are_reachable_and_navigation_is_non_destructive() {
    let mut workspace = default_workspace();
    let (character_id, _, animation_set_id, idle_clip_id) = character_context(&workspace);
    assert!(workspace.open_animation_viewer_for_resource(character_id));

    let resources_before = workspace.project().resources.clone();
    let animation_set_before = workspace
        .project()
        .resource(animation_set_id)
        .expect("animation set")
        .data
        .clone();
    let character_before = workspace
        .project()
        .resource(character_id)
        .expect("character")
        .data
        .clone();
    let idle_before = workspace
        .project()
        .resource(idle_clip_id)
        .expect("idle clip")
        .data
        .clone();
    let dirty_before = workspace.is_dirty();

    let (ctx, viewport) = real_egui_workspace_ctx("animation-studio-navigation");
    let mut time = 0.0;
    let initial = real_egui_workspace_frame(&ctx, &mut workspace, &viewport, time, Vec::new());
    for label in [
        icons::label(icons::PLAY, "Preview"),
        icons::label(icons::LAYERS, "Moveset"),
        icons::label(icons::WAYPOINT, "Pose"),
        icons::label(icons::MAP_PIN, "Weapon"),
        icons::label(icons::SCAN, "Combat"),
    ] {
        locate_unique_label(&initial, &label);
    }

    let moveset = click_label(
        &ctx,
        &mut workspace,
        &viewport,
        &mut time,
        &icons::label(icons::LAYERS, "Moveset"),
    );
    locate_unique_label(&moveset, "Moveset Matrix");
    let pose = click_label(
        &ctx,
        &mut workspace,
        &viewport,
        &mut time,
        &icons::label(icons::WAYPOINT, "Pose"),
    );
    locate_unique_label(&pose, "Pose Keys");
    let weapon = click_label(
        &ctx,
        &mut workspace,
        &viewport,
        &mut time,
        &icons::label(icons::MAP_PIN, "Weapon"),
    );
    locate_unique_label(&weapon, "Weapon Studio");
    let combat = click_label(
        &ctx,
        &mut workspace,
        &viewport,
        &mut time,
        &icons::label(icons::SCAN, "Combat"),
    );
    locate_unique_label(&combat, "Combat Volumes");

    assert_eq!(workspace.is_dirty(), dirty_before);
    assert_eq!(workspace.project().resources, resources_before);
    assert_eq!(
        workspace.project().resource(animation_set_id).unwrap().data,
        animation_set_before
    );
    assert_eq!(
        workspace.project().resource(character_id).unwrap().data,
        character_before
    );
    assert_eq!(
        workspace.project().resource(idle_clip_id).unwrap().data,
        idle_before
    );
}

#[test]
fn banked_enemy_atlases_render_in_animation_studio() {
    for character_name in ["Light Enemy", "Heavy Enemy"] {
        let mut workspace = default_workspace();
        let character = resource_id(&workspace, character_name, |data| {
            matches!(data, ResourceData::Character(_))
        });
        assert!(workspace.open_animation_viewer_for_resource(character));

        let (ctx, viewport) =
            real_egui_workspace_ctx(&format!("animation-studio-{character_name}"));
        let frame = real_egui_workspace_frame(&ctx, &mut workspace, &viewport, 0.0, Vec::new());
        assert!(
            text_shape_centers(&frame.shapes, "Model atlas is missing").is_empty(),
            "{character_name} must decode its multi-bank 4bpp atlas"
        );
    }
}

#[test]
fn space_toggles_animation_playback_without_editing_the_project() {
    let mut workspace = default_workspace();
    let (_, _, _, idle_clip_id) = character_context(&workspace);
    assert!(workspace.open_animation_viewer_for_resource(idle_clip_id));
    let resources_before = workspace.project().resources.clone();
    let dirty_before = workspace.is_dirty();
    let playing_before = workspace.animation_viewer.preview_is_playing();

    let (ctx, viewport) = real_egui_workspace_ctx("animation-studio-space-playback");
    let mut time = 0.0;
    let _ = real_egui_workspace_frame(&ctx, &mut workspace, &viewport, time, Vec::new());
    time += 1.0 / 60.0;
    let _ = real_egui_workspace_frame(
        &ctx,
        &mut workspace,
        &viewport,
        time,
        vec![key_event(egui::Key::Space, true)],
    );
    assert_eq!(
        workspace.animation_viewer.preview_is_playing(),
        !playing_before
    );

    time += 1.0 / 60.0;
    let _ = real_egui_workspace_frame(
        &ctx,
        &mut workspace,
        &viewport,
        time,
        vec![key_event(egui::Key::Space, false)],
    );
    time += 1.0 / 60.0;
    let _ = real_egui_workspace_frame(
        &ctx,
        &mut workspace,
        &viewport,
        time,
        vec![key_event(egui::Key::Space, true)],
    );
    assert_eq!(
        workspace.animation_viewer.preview_is_playing(),
        playing_before
    );

    assert_eq!(workspace.project().resources, resources_before);
    assert_eq!(workspace.is_dirty(), dirty_before);
}

#[test]
fn preview_mode_loads_authored_action_weapons_without_authoring_overlays() {
    let mut workspace = default_workspace();
    let light_weapon = resource_id(&workspace, "Sword1 Light", |data| {
        matches!(data, ResourceData::Weapon(_))
    });
    assert!(workspace.open_animation_viewer_for_resource(light_weapon));
    let resources_before = workspace.project().resources.clone();
    let dirty_before = workspace.is_dirty();

    let (ctx, viewport) = real_egui_workspace_ctx("animation-studio-preview-action-weapons");
    let mut time = 0.0;
    let _ = click_label(
        &ctx,
        &mut workspace,
        &viewport,
        &mut time,
        &icons::label(icons::PLAY, "Preview"),
    );
    assert!(!workspace
        .animation_viewer
        .weapon_authoring_overlays_are_visible());

    workspace.animation_viewer.clear_preview_weapon_cache();
    time += 1.0 / 60.0;
    let _ = real_egui_workspace_frame(&ctx, &mut workspace, &viewport, time, Vec::new());
    assert!(
        workspace.animation_viewer.preview_weapon_model_count() > 0,
        "Preview mode must load the weapon assigned to the selected action"
    );

    assert_eq!(workspace.project().resources, resources_before);
    assert_eq!(workspace.is_dirty(), dirty_before);
}

#[test]
fn combat_capsules_can_be_hidden_and_shown_without_editing_gameplay_data() {
    let mut workspace = default_workspace();
    let (character_id, _, _, _) = character_context(&workspace);
    assert!(workspace.open_animation_viewer_for_resource(character_id));
    let resources_before = workspace.project().resources.clone();
    let dirty_before = workspace.is_dirty();

    let (ctx, viewport) = real_egui_workspace_ctx("animation-studio-combat-capsule-visibility");
    let mut time = 0.0;
    let combat = click_label(
        &ctx,
        &mut workspace,
        &viewport,
        &mut time,
        &icons::label(icons::SCAN, "Combat"),
    );
    locate_unique_label(&combat, &icons::label(icons::EYE_OFF, "Hide capsules"));

    let hidden = click_label(
        &ctx,
        &mut workspace,
        &viewport,
        &mut time,
        &icons::label(icons::EYE_OFF, "Hide capsules"),
    );
    locate_unique_label(&hidden, &icons::label(icons::EYE, "Show capsules"));

    let visible = click_label(
        &ctx,
        &mut workspace,
        &viewport,
        &mut time,
        &icons::label(icons::EYE, "Show capsules"),
    );
    locate_unique_label(&visible, &icons::label(icons::EYE_OFF, "Hide capsules"));

    assert_eq!(workspace.project().resources, resources_before);
    assert_eq!(workspace.is_dirty(), dirty_before);
}

#[test]
fn combat_mode_loads_the_weapon_for_its_attack_preview() {
    let mut workspace = default_workspace();
    let (character_id, _, _, _) = character_context(&workspace);
    assert!(workspace.open_animation_viewer_for_resource(character_id));
    let resources_before = workspace.project().resources.clone();
    let dirty_before = workspace.is_dirty();

    let (ctx, viewport) = real_egui_workspace_ctx("animation-studio-combat-weapon");
    let mut time = 0.0;
    let _ = click_label(
        &ctx,
        &mut workspace,
        &viewport,
        &mut time,
        &icons::label(icons::SCAN, "Combat"),
    );
    workspace.animation_viewer.clear_preview_weapon_cache();
    time += 1.0 / 60.0;
    let _ = real_egui_workspace_frame(&ctx, &mut workspace, &viewport, time, Vec::new());

    assert!(
        workspace.animation_viewer.preview_weapon_model_count() > 0,
        "Combat must compose the weapon assigned to the active attack"
    );
    assert_eq!(workspace.project().resources, resources_before);
    assert_eq!(workspace.is_dirty(), dirty_before);
}

#[test]
fn root_calibration_toolbar_edits_are_undoable() {
    let mut workspace = default_workspace();
    let (_, _, _, idle_clip_id) = character_context(&workspace);
    assert!(workspace.open_animation_viewer_for_resource(idle_clip_id));
    let before = workspace
        .project()
        .resource(idle_clip_id)
        .expect("idle clip")
        .data
        .clone();

    let (ctx, viewport) = real_egui_workspace_ctx("animation-root-calibration-undo");
    let mut time = 0.0;
    let move_label = icons::MOVE.to_string();
    let opened = click_label(&ctx, &mut workspace, &viewport, &mut time, &move_label);
    locate_unique_label(&opened, "In-place");
    let _ = click_label(&ctx, &mut workspace, &viewport, &mut time, "In-place");
    assert_ne!(
        workspace
            .project()
            .resource(idle_clip_id)
            .expect("idle clip")
            .data,
        before,
        "the toolbar checkbox must author the selected clip"
    );

    workspace.do_undo();
    assert_eq!(
        workspace
            .project()
            .resource(idle_clip_id)
            .expect("idle clip")
            .data,
        before,
        "Undo must include Animation toolbar calibration changes"
    );
}

#[test]
fn moveset_matrix_separates_enabled_actions_from_visual_fallbacks() {
    let mut workspace = default_workspace();
    let character_id = resource_id(&workspace, "Heavy Enemy", |data| {
        matches!(data, ResourceData::Character(_))
    });
    let animation_set_id = match &workspace
        .project()
        .resource(character_id)
        .expect("Heavy Enemy character")
        .data
    {
        ResourceData::Character(character) => {
            character.animation_set.expect("Heavy Enemy animation set")
        }
        _ => unreachable!(),
    };
    let resources_before = workspace.project().resources.clone();
    let dirty_before = workspace.is_dirty();

    let rows = moveset_capability_rows(workspace.project(), character_id)
        .expect("Aletha has a valid Animation Set");
    let row = |action| {
        rows.iter()
            .find(|row| row.action == action)
            .unwrap_or_else(|| panic!("matrix omitted {action:?}"))
    };

    for action in [
        CharacterAnimationAction::Idle,
        CharacterAnimationAction::Walk,
    ] {
        let row = row(action);
        assert_eq!(row.status, MovesetCapabilityStatus::Ready);
        assert!(row.clip.is_some());
        assert!(row.clip_name.is_some());
        assert_eq!(row.binding_source, Some(MovesetBindingSource::Action));
    }

    let run = row(CharacterAnimationAction::Run);
    assert_eq!(run.status, MovesetCapabilityStatus::Disabled);
    assert_eq!(run.clip, None, "the Heavy Enemy must not gain Run");
    assert_eq!(
        run.visual_fallback_action,
        Some(CharacterAnimationAction::Walk),
        "Run may preview Walk without enabling sprinting"
    );
    let run_visual_label = format!(
        "Visual: Walk · {}",
        run.visual_fallback_name
            .as_deref()
            .expect("Heavy Enemy walk fallback name")
    );

    let heavy = row(CharacterAnimationAction::HeavyAttack);
    assert_eq!(heavy.status, MovesetCapabilityStatus::Disabled);
    assert_eq!(heavy.clip, None, "fallback must not enable Heavy Attack");
    assert_eq!(
        heavy.visual_fallback_action,
        Some(CharacterAnimationAction::LightAttack)
    );
    assert!(heavy.visual_fallback_clip.is_some());
    assert!(heavy.visual_fallback_name.is_some());

    let roll = row(CharacterAnimationAction::Roll);
    assert_eq!(roll.status, MovesetCapabilityStatus::Disabled);
    assert_eq!(
        roll.visual_fallback_action,
        Some(CharacterAnimationAction::Walk),
        "the matrix must mirror RuntimeCharacter::clip_for fallback order"
    );

    let model_id = match &workspace
        .project()
        .resource(character_id)
        .expect("Heavy Enemy character")
        .data
    {
        ResourceData::Character(character) => character.model.expect("Heavy Enemy model"),
        _ => unreachable!(),
    };
    let mut malformed = workspace.project().clone();
    let ResourceData::AnimationSet(set) = &mut malformed
        .resource_mut(animation_set_id)
        .expect("Heavy Enemy animation set")
        .data
    else {
        unreachable!()
    };
    set.set_action_clip(CharacterAnimationAction::Idle, None);
    set.set_action_clip(CharacterAnimationAction::Walk, Some(model_id));
    let malformed_rows = moveset_capability_rows(&malformed, character_id)
        .expect("malformed set still has a report");
    assert_eq!(
        malformed_rows[CharacterAnimationAction::Idle.to_index()].status,
        MovesetCapabilityStatus::Missing
    );
    assert_eq!(
        malformed_rows[CharacterAnimationAction::Walk.to_index()].status,
        MovesetCapabilityStatus::Broken
    );

    assert!(workspace.open_animation_viewer_for_resource(character_id));
    let (ctx, viewport) = real_egui_workspace_ctx("animation-studio-moveset-matrix");
    let mut time = 0.0;
    let matrix = click_label(
        &ctx,
        &mut workspace,
        &viewport,
        &mut time,
        &icons::label(icons::LAYERS, "Moveset"),
    );
    locate_unique_label(&matrix, "Moveset Matrix");
    locate_unique_label(&matrix, "core requirements ready");
    locate_unique_label(&matrix, "MOTION / VISUAL FALLBACK");
    locate_unique_label(&matrix, &run_visual_label);
    let fallback_preview = click_label(&ctx, &mut workspace, &viewport, &mut time, "Run");
    locate_unique_label(&fallback_preview, "Moveset Matrix");

    assert_eq!(workspace.is_dirty(), dirty_before);
    assert_eq!(workspace.project().resources, resources_before);
    assert!(workspace.project().resource(animation_set_id).is_some());
}

#[test]
fn weapon_timing_controls_edit_only_the_selected_visibility_beat() {
    let mut workspace = default_workspace();
    let (character_id, model_id, animation_set_id, _) = character_context(&workspace);
    let weapon_id = resource_id(&workspace, "Sword1 Light", |data| {
        matches!(data, ResourceData::Weapon(_))
    });
    assert!(workspace.open_animation_viewer_for_resource(weapon_id));

    // Keep this UI fixture independent of the currently authored Light Attack
    // timing and trail authoring. A finite disappearance frame adds a second
    // "Use playhead" button and is exercised after the first edit below.
    let ResourceData::AnimationSet(animation_set) = &mut workspace
        .project
        .resource_mut(animation_set_id)
        .expect("animation set")
        .data
    else {
        unreachable!()
    };
    let visibility_track = animation_set
        .weapon_appearance_tracks
        .iter_mut()
        .find(|track| {
            track.action == CharacterAnimationAction::LightAttack
                && track.weapon == weapon_id
                && track.character_socket == "right_hand_grip"
        })
        .expect("light attack weapon beat");
    visibility_track.hidden_frame = psxed_project::ACTION_FRAME_END_FULL;
    visibility_track.trail = None;

    let ResourceData::AnimationSet(animation_set) = &workspace
        .project()
        .resource(animation_set_id)
        .expect("animation set")
        .data
    else {
        unreachable!()
    };
    let tracks_before = animation_set.weapon_appearance_tracks.clone();
    let target_frame_before = tracks_before
        .iter()
        .find(|track| {
            track.action == CharacterAnimationAction::LightAttack
                && track.weapon == weapon_id
                && track.character_socket == "right_hand_grip"
        })
        .expect("light attack weapon beat")
        .fully_visible_frame;

    let weapon_before = workspace
        .project()
        .resource(weapon_id)
        .unwrap()
        .data
        .clone();
    let model_before = workspace.project().resource(model_id).unwrap().data.clone();
    let character_before = workspace
        .project()
        .resource(character_id)
        .unwrap()
        .data
        .clone();

    let (ctx, viewport) = real_egui_workspace_ctx("animation-studio-weapon-beat");
    let mut time = 0.0;
    let initial = real_egui_workspace_frame(&ctx, &mut workspace, &viewport, time, Vec::new());
    locate_unique_label(&initial, "Weapon Studio");
    time += 1.0 / 60.0;
    let _ = real_egui_workspace_frame(
        &ctx,
        &mut workspace,
        &viewport,
        time,
        vec![
            egui::Event::PointerMoved(Pos2::new(1300.0, 420.0)),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0.0, -480.0),
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    time += 1.0 / 60.0;
    let after_scroll = real_egui_workspace_frame(&ctx, &mut workspace, &viewport, time, Vec::new());
    locate_unique_label(&after_scroll, "Use playhead");
    let _ = click_label(&ctx, &mut workspace, &viewport, &mut time, "Use playhead");

    let ResourceData::AnimationSet(animation_set) = &workspace
        .project()
        .resource(animation_set_id)
        .expect("animation set")
        .data
    else {
        unreachable!()
    };
    let edited_track = animation_set
        .weapon_appearance_tracks
        .iter()
        .find(|track| {
            track.action == CharacterAnimationAction::LightAttack
                && track.weapon == weapon_id
                && track.character_socket == "right_hand_grip"
        })
        .expect("edited light attack weapon beat");
    assert_ne!(
        edited_track.fully_visible_frame, target_frame_before,
        "Use playhead must author the current frame"
    );
    for (before, after) in tracks_before
        .iter()
        .zip(&animation_set.weapon_appearance_tracks)
        .filter(|(track, _)| {
            !(track.action == CharacterAnimationAction::LightAttack
                && track.weapon == weapon_id
                && track.character_socket == "right_hand_grip")
        })
    {
        assert_eq!(after, before, "visibility timing touched another beat");
    }
    let expected_tracks = animation_set.weapon_appearance_tracks.clone();

    // Persisting the beat rebuilds the resource-backed panel. Return to the
    // timing controls before exercising the disappearance toggle, independent
    // of how many optional authoring lanes the fixture currently enables.
    scroll_authoring_label_into_view(
        &ctx,
        &mut workspace,
        &viewport,
        &mut time,
        "Gone at clip end",
    );
    let disappeared = click_label(
        &ctx,
        &mut workspace,
        &viewport,
        &mut time,
        "Gone at clip end",
    );
    locate_unique_label(&disappeared, "Gone by");
    let ResourceData::AnimationSet(animation_set) = &workspace
        .project()
        .resource(animation_set_id)
        .expect("animation set")
        .data
    else {
        unreachable!()
    };
    let edited_track = animation_set
        .weapon_appearance_tracks
        .iter()
        .find(|track| {
            track.action == CharacterAnimationAction::LightAttack
                && track.weapon == weapon_id
                && track.character_socket == "right_hand_grip"
        })
        .expect("edited light attack weapon beat");
    assert_ne!(
        edited_track.hidden_frame,
        psxed_project::ACTION_FRAME_END_FULL,
        "unchecking Gone at clip end must expose an authored disappearance frame"
    );
    assert!(edited_track.hidden_frame > edited_track.fully_visible_frame);
    for (before, after) in expected_tracks
        .iter()
        .zip(&animation_set.weapon_appearance_tracks)
        .filter(|(track, _)| {
            !(track.action == CharacterAnimationAction::LightAttack
                && track.weapon == weapon_id
                && track.character_socket == "right_hand_grip")
        })
    {
        assert_eq!(after, before, "disappearance timing touched another beat");
    }
    assert_eq!(
        workspace.project().resource(weapon_id).unwrap().data,
        weapon_before,
        "visibility timing must not move the weapon grip"
    );
    assert_eq!(
        workspace.project().resource(model_id).unwrap().data,
        model_before,
        "visibility timing must not move the character socket"
    );
    assert_eq!(
        workspace.project().resource(character_id).unwrap().data,
        character_before,
        "visibility timing must not edit combat data"
    );
    assert!(workspace.is_dirty());
}

#[test]
fn assign_sword_to_left_hand_uses_the_explicit_hand_control() {
    let mut workspace = default_workspace();
    let (_, _, animation_set_id, _) = character_context(&workspace);
    let light_weapon = resource_id(&workspace, "Sword1 Light", |data| {
        matches!(data, ResourceData::Weapon(_))
    });
    assert!(workspace.open_animation_viewer_for_resource(light_weapon));

    let ResourceData::AnimationSet(animation_set) = &workspace
        .project()
        .resource(animation_set_id)
        .expect("animation set")
        .data
    else {
        unreachable!()
    };
    let tracks_before = animation_set.weapon_appearance_tracks.clone();

    let (ctx, viewport) = real_egui_workspace_ctx("animation-studio-assign-sword-to-left-hand");
    let mut time = 0.0;
    let initial = real_egui_workspace_frame(&ctx, &mut workspace, &viewport, time, Vec::new());
    locate_unique_label(&initial, "Weapon Studio");
    let assigned = click_label(
        &ctx,
        &mut workspace,
        &viewport,
        &mut time,
        &icons::label(icons::PLUS, "Left hand"),
    );

    let ResourceData::AnimationSet(animation_set) = &workspace
        .project()
        .resource(animation_set_id)
        .expect("animation set")
        .data
    else {
        unreachable!()
    };
    assert_eq!(
        animation_set.weapon_appearance_tracks.len(),
        tracks_before.len() + 1
    );
    assert_eq!(
        &animation_set.weapon_appearance_tracks[..tracks_before.len()],
        tracks_before.as_slice(),
        "assigning a sword must append without rewriting existing timing"
    );
    let added = animation_set.weapon_appearance_tracks.last().unwrap();
    assert_eq!(added.action, CharacterAnimationAction::LightAttack);
    assert_eq!(added.weapon, light_weapon);
    assert_eq!(added.character_socket, "left_hand_grip");

    let mut pairs = std::collections::HashSet::new();
    for track in animation_set
        .weapon_appearance_tracks
        .iter()
        .filter(|track| track.action == CharacterAnimationAction::LightAttack)
    {
        assert!(
            pairs.insert((track.weapon, track.character_socket.as_str())),
            "hand assignment created the duplicate pair rejected by the cooker"
        );
    }

    let remove_buttons =
        text_shape_centers(&assigned.shapes, &icons::label(icons::TRASH, "Remove"));
    assert_eq!(
        remove_buttons.len(),
        2,
        "each visible hand assignment needs its own Remove button"
    );
    let left_hand_remove = remove_buttons
        .into_iter()
        .max_by(|left, right| left.y.total_cmp(&right.y))
        .expect("new left-hand assignment is the lower row");
    let (press, release) = press_release(left_hand_remove);
    time += 1.0 / 60.0;
    let _ = real_egui_workspace_frame(&ctx, &mut workspace, &viewport, time, press);
    time += 1.0 / 60.0;
    let _ = real_egui_workspace_frame(&ctx, &mut workspace, &viewport, time, release);

    let ResourceData::AnimationSet(animation_set) = &workspace
        .project()
        .resource(animation_set_id)
        .expect("animation set")
        .data
    else {
        unreachable!()
    };
    assert_eq!(animation_set.weapon_appearance_tracks, tracks_before);
    assert!(workspace.is_dirty());
}

#[test]
fn pose_and_combat_buttons_write_to_their_own_resources() {
    let mut workspace = default_workspace();
    let (character_id, model_id, animation_set_id, idle_clip_id) = character_context(&workspace);
    let weapon_id = resource_id(&workspace, "Sword1 Light", |data| {
        matches!(data, ResourceData::Weapon(_))
    });
    let animation_set_before = workspace
        .project()
        .resource(animation_set_id)
        .unwrap()
        .data
        .clone();
    let model_before = workspace.project().resource(model_id).unwrap().data.clone();
    let weapon_before = workspace
        .project()
        .resource(weapon_id)
        .unwrap()
        .data
        .clone();
    let character_before = workspace
        .project()
        .resource(character_id)
        .unwrap()
        .data
        .clone();

    assert!(workspace.open_animation_viewer_for_resource(idle_clip_id));
    let ResourceData::AnimationClip(idle) = &workspace
        .project()
        .resource(idle_clip_id)
        .expect("idle clip")
        .data
    else {
        unreachable!()
    };
    let mut expected_pose_keys = idle.pose_corrections.clone();
    assert!(
        !expected_pose_keys
            .iter()
            .any(|key| key.frame == 0 && key.joint == 0),
        "fixture needs an empty pose cell at frame 0 / joint 0"
    );
    expected_pose_keys.push(AnimationPoseCorrectionKey {
        frame: 0,
        joint: 0,
        ..Default::default()
    });
    expected_pose_keys.sort_by_key(|key| (key.joint, key.frame));

    let (ctx, viewport) = real_egui_workspace_ctx("animation-studio-pose-combat");
    let mut time = 0.0;
    let pose = real_egui_workspace_frame(&ctx, &mut workspace, &viewport, time, Vec::new());
    locate_unique_label(&pose, "Pose Keys");
    let _ = click_label(
        &ctx,
        &mut workspace,
        &viewport,
        &mut time,
        &icons::label(icons::PLUS, "Add key"),
    );
    let ResourceData::AnimationClip(idle) = &workspace
        .project()
        .resource(idle_clip_id)
        .expect("idle clip")
        .data
    else {
        unreachable!()
    };
    assert_eq!(idle.pose_corrections, expected_pose_keys);
    assert_eq!(
        workspace.project().resource(character_id).unwrap().data,
        character_before,
        "pose authoring must not alter character combat or movement data"
    );

    assert!(workspace.open_animation_viewer_for_resource(character_id));
    let combat = click_label(
        &ctx,
        &mut workspace,
        &viewport,
        &mut time,
        &icons::label(icons::SCAN, "Combat"),
    );
    locate_unique_label(&combat, "Combat Volumes");
    let ResourceData::Character(mut expected_character) = character_before.clone() else {
        unreachable!()
    };
    expected_character
        .combat_capsules
        .push(psxed_project::CharacterCombatCapsule {
            name: "Attack Hitbox".to_string(),
            role: CombatCapsuleRole::Hitbox {
                action: CharacterAnimationAction::LightAttack,
                active_start_frame: 8,
                active_end_frame: 14,
                damage: 25,
                poise_damage: 25,
            },
            ..Default::default()
        });
    let _ = click_label(
        &ctx,
        &mut workspace,
        &viewport,
        &mut time,
        &icons::label(icons::PLUS, "Hitbox"),
    );

    let ResourceData::Character(character) = &workspace
        .project()
        .resource(character_id)
        .expect("character")
        .data
    else {
        unreachable!()
    };
    assert_eq!(character, &expected_character);
    assert!(matches!(
        character.combat_capsules.last().unwrap().role,
        CombatCapsuleRole::Hitbox {
            action: CharacterAnimationAction::LightAttack,
            active_start_frame: 8,
            active_end_frame: 14,
            damage: 25,
            poise_damage: 25,
        }
    ));
    assert_eq!(
        workspace.project().resource(animation_set_id).unwrap().data,
        animation_set_before,
        "pose/combat authoring must not alter weapon timing"
    );
    assert_eq!(
        workspace.project().resource(model_id).unwrap().data,
        model_before,
        "pose/combat authoring must not alter attachment sockets"
    );
    assert_eq!(
        workspace.project().resource(weapon_id).unwrap().data,
        weapon_before,
        "pose/combat authoring must not alter weapon grip data"
    );
    assert!(workspace.is_dirty());
}
