use super::*;
use psxed_project::{UiFocusEffect, UiTransition, UiTransitionKind, UiVisibilityCondition};

#[derive(Clone, Debug)]
pub(crate) struct AnimationSetOption {
    pub(crate) id: ResourceId,
    pub(crate) name: String,
    pub(crate) skeleton: Option<ResourceId>,
    pub(crate) action_clips: [Option<ResourceId>; psxed_project::CHARACTER_ANIMATION_ACTION_COUNT],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CharacterControllerRole {
    Passive,
    Player,
    Enemy,
}

impl CharacterControllerRole {
    pub(crate) fn from_controller(player: bool, settings: &CharacterControllerSettings) -> Self {
        if player {
            Self::Player
        } else if settings.enemy.is_some() {
            Self::Enemy
        } else {
            Self::Passive
        }
    }

    pub(crate) fn apply_to(
        self,
        player: &mut bool,
        settings: &mut CharacterControllerSettings,
    ) -> bool {
        let before = (*player, settings.enemy);
        match self {
            Self::Passive => {
                *player = false;
                settings.enemy = None;
            }
            Self::Player => {
                *player = true;
            }
            Self::Enemy => {
                *player = false;
                if settings.enemy.is_none() {
                    settings.enemy = Some(psxed_project::EnemyBehaviorSettings::defaults());
                }
            }
        }
        before != (*player, settings.enemy)
    }

    fn label(self) -> &'static str {
        match self {
            Self::Passive => "Passive",
            Self::Player => "Player",
            Self::Enemy => "Enemy",
        }
    }
}

/// Inspector body for `ResourceData::Character` profiles. Combines a
/// model picker, role-clip pickers, capsule sizes, controller speed,
/// and camera params.
/// `Auto Assign Clips By Name` walks the bound model's clip
/// list and matches gameplay role substrings
/// -- case-insensitive -- into role slots.
pub(crate) fn draw_character_resource_editor(
    ui: &mut egui::Ui,
    character: &mut psxed_project::CharacterResource,
    ctx: &CharacterEditorContext,
) -> bool {
    let mut changed = false;

    // Resolve the bound model + clip list (if any). Used
    // throughout the inspector to surface clip names instead of
    // raw indices.
    let bound = character
        .model
        .and_then(|id| ctx.models.iter().find(|(mid, _, _)| *mid == id));
    let bound_skeleton = ctx.model_skeleton(character.model);
    let selected_set = ctx.animation_set(character.animation_set);

    egui::CollapsingHeader::new(icons::label(icons::FOCUS, "Spawn Defaults"))
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Role");
                egui::ComboBox::from_id_salt("character-spawn-role")
                    .selected_text(match character.spawn_role {
                        psxed_project::CharacterSpawnRole::Auto => "Auto",
                        psxed_project::CharacterSpawnRole::Player => "Player",
                        psxed_project::CharacterSpawnRole::Enemy => "Enemy",
                    })
                    .show_ui(ui, |ui| {
                        for (role, label) in [
                            (psxed_project::CharacterSpawnRole::Auto, "Auto"),
                            (psxed_project::CharacterSpawnRole::Player, "Player"),
                            (psxed_project::CharacterSpawnRole::Enemy, "Enemy"),
                        ] {
                            if ui
                                .selectable_label(character.spawn_role == role, label)
                                .clicked()
                            {
                                character.spawn_role = role;
                                if role == psxed_project::CharacterSpawnRole::Enemy
                                    && character.enemy_behavior.is_none()
                                {
                                    character.enemy_behavior =
                                        Some(psxed_project::EnemyBehaviorSettings::defaults());
                                }
                                changed = true;
                            }
                        }
                    });
            });
            ui.label(
                RichText::new(match character.spawn_role {
                    psxed_project::CharacterSpawnRole::Auto => {
                        "Auto uses the legacy first-character-is-player placement rule."
                    }
                    psxed_project::CharacterSpawnRole::Player => {
                        "Dropping this profile creates the player and replaces the previous player source."
                    }
                    psxed_project::CharacterSpawnRole::Enemy => {
                        "Dropping this profile always creates an enemy with the tuning below."
                    }
                })
                .color(STUDIO_TEXT_WEAK)
                .small(),
            );
            if character.spawn_role == psxed_project::CharacterSpawnRole::Enemy {
                let enemy = character
                    .enemy_behavior
                    .get_or_insert_with(psxed_project::EnemyBehaviorSettings::defaults);
                changed |= draw_enemy_behavior_fields(ui, enemy).0;
            }
        });

    egui::CollapsingHeader::new(icons::label(icons::BOX, "Model"))
        .default_open(true)
        .show(ui, |ui| {
            let model_options = ctx
                .models
                .iter()
                .map(|(id, name, _)| (*id, name.clone()))
                .collect::<Vec<_>>();
            ui.horizontal(|ui| {
                ui.label("Model");
                let preview = bound.map(|(_, name, _)| name.as_str()).unwrap_or("(none)");
                changed |= searchable_picker(
                    ui,
                    "character-model-picker",
                    &mut character.model,
                    preview,
                    &model_options,
                    SearchablePickerConfig::optional("(none)"),
                );
            });
            if character.model.is_some() && bound.is_none() {
                ui.colored_label(
                    Color32::from_rgb(220, 120, 100),
                    "Bound model resource is missing.",
                );
            }
            changed |= resource_id_picker(
                ui,
                "Material",
                "character-material-picker",
                &mut character.material,
                &ctx.materials,
            );
            changed |= resource_id_picker(
                ui,
                "Action Map",
                "character-animation-set-picker",
                &mut character.animation_set,
                &ctx.animation_sets
                    .iter()
                    .filter(|set| {
                        bound_skeleton.is_none()
                            || set.skeleton.is_none()
                            || set.skeleton == bound_skeleton
                    })
                    .map(|set| (set.id, set.name.clone()))
                    .collect::<Vec<_>>(),
            );
            if character.animation_set.is_some() && selected_set.is_none() {
                ui.colored_label(
                    Color32::from_rgb(220, 120, 100),
                    "Action Map resource is missing.",
                );
            }
            if let Some(set) = selected_set {
                if bound_skeleton.is_some()
                    && set.skeleton.is_some()
                    && set.skeleton != bound_skeleton
                {
                    ui.colored_label(
                        Color32::from_rgb(220, 120, 100),
                        "Action Map targets a different skeleton than the selected Model.",
                    );
                }
            }
        });

    egui::CollapsingHeader::new(icons::label(icons::PALETTE, "Animation Actions"))
        .default_open(true)
        .show(ui, |ui| {
            if character.animation_set.is_none() {
                ui.colored_label(
                    Color32::from_rgb(220, 120, 100),
                    "Assign an Action Map (Animation Set) above to give this character animations.",
                );
                return;
            }
            ui.label(
                RichText::new(
                    "Animations resolve from the assigned Action Map. Edit clip roles on that Animation Set resource.",
                )
                .color(STUDIO_TEXT_WEAK)
                .small(),
            );
            draw_character_effective_roles(ui, selected_set, ctx);

            let has_set_idle = selected_set
                .and_then(|set| set.action_clips[psxed_project::CharacterAnimationAction::Idle.to_index()])
                .is_some();
            let has_set_walk = selected_set
                .and_then(|set| set.action_clips[psxed_project::CharacterAnimationAction::Walk.to_index()])
                .is_some();
            if character.model.is_some() && !has_set_idle {
                ui.colored_label(
                    Color32::from_rgb(220, 120, 100),
                    "Idle clip is required for the player character.",
                );
            }
            if character.model.is_some() && !has_set_walk {
                ui.colored_label(
                    Color32::from_rgb(220, 120, 100),
                    "Walk clip is required for the player character.",
                );
            }
        });

    egui::CollapsingHeader::new(icons::label(icons::GRID, "Camera"))
        .default_open(false)
        .show(ui, |ui| {
            changed |= drag_i32(ui, "Distance", &mut character.camera_distance, 1, 16384);
            changed |= drag_i32(ui, "Height", &mut character.camera_height, 0, 16384);
            changed |= drag_i32(
                ui,
                "Target height",
                &mut character.camera_target_height,
                0,
                16384,
            );
            changed |= drag_u8(
                ui,
                "Lock rise (%)",
                &mut character.camera_lock_rise_percent,
                0,
                100,
            );
            changed |= drag_i32(
                ui,
                "Floor clearance",
                &mut character.camera_min_floor_clearance,
                0,
                4096,
            );
            changed |= drag_u8(
                ui,
                "Orbit speed",
                &mut character.camera_orbit_speed_level,
                1,
                10,
            );
            changed |= drag_u8(
                ui,
                "Position lag",
                &mut character.camera_position_lag_shift,
                0,
                12,
            );
            changed |= drag_u8(
                ui,
                "Focus lag",
                &mut character.camera_focus_lag_shift,
                0,
                12,
            );
            changed |= drag_u8(
                ui,
                "Distance lag",
                &mut character.camera_distance_lag_shift,
                0,
                12,
            );
        });

    changed
}

fn draw_enemy_behavior_fields(
    ui: &mut egui::Ui,
    enemy: &mut psxed_project::EnemyBehaviorSettings,
) -> (bool, bool) {
    let mut changed = false;

    ui.separator();
    ui.label(RichText::new("Awareness & patrol").strong());
    changed |= drag_u16(ui, "Aggro radius", &mut enemy.aggro_radius, 1, 32767);
    changed |= drag_u8(ui, "Reaction ticks", &mut enemy.reaction_ticks, 0, 255);
    changed |= drag_u16(
        ui,
        "Patrol wait ticks",
        &mut enemy.patrol_wait_ticks,
        0,
        32767,
    );
    ui.label(
        RichText::new("Patrol offset")
            .color(STUDIO_TEXT_WEAK)
            .small(),
    );
    changed |= drag_i32(ui, "X", &mut enemy.patrol_offset[0], -32767, 32767);
    changed |= drag_i32(ui, "Y", &mut enemy.patrol_offset[1], -32767, 32767);
    changed |= drag_i32(ui, "Z", &mut enemy.patrol_offset[2], -32767, 32767);

    ui.separator();
    ui.label(RichText::new("Spacing & intent").strong());
    changed |= drag_u16(
        ui,
        "Preferred distance",
        &mut enemy.preferred_distance,
        1,
        32767,
    );
    changed |= drag_u16(
        ui,
        "Spacing tolerance",
        &mut enemy.spacing_tolerance,
        0,
        32767,
    );
    changed |= drag_u8(
        ui,
        "Decision interval",
        &mut enemy.decision_interval_ticks,
        1,
        255,
    );
    changed |= drag_u8(ui, "Circle chance (%)", &mut enemy.circle_chance, 0, 100);

    ui.separator();
    ui.label(RichText::new("Attack pacing").strong());
    let mut attack_changed = false;
    attack_changed |= drag_u8(ui, "Director priority", &mut enemy.attack_priority, 0, 255);
    attack_changed |= drag_u8(
        ui,
        "Attack cooldown ticks",
        &mut enemy.attack_cooldown_ticks,
        0,
        255,
    );
    attack_changed |= drag_u8(
        ui,
        "Group attack delay",
        &mut enemy.group_attack_delay_ticks,
        0,
        255,
    );
    attack_changed |= drag_u8(ui, "Windup ticks", &mut enemy.windup_ticks, 1, 255);
    attack_changed |= drag_u8(ui, "Recovery ticks", &mut enemy.recovery_ticks, 0, 255);
    changed |= attack_changed;

    ui.separator();
    ui.label(RichText::new("Combat stats").strong());
    changed |= drag_u16(ui, "Health", &mut enemy.max_health, 1, u16::MAX);
    changed |= drag_u16(ui, "Poise", &mut enemy.poise, 0, u16::MAX);
    changed |= drag_u16(ui, "Touch damage", &mut enemy.touch_damage, 0, u16::MAX);

    (changed, attack_changed)
}

pub(crate) fn draw_character_controller_settings(
    ui: &mut egui::Ui,
    settings: &mut CharacterControllerSettings,
    player_controlled: bool,
    preview_action: &mut Option<psxed_project::CharacterAnimationAction>,
) -> bool {
    let mut changed = false;

    egui::CollapsingHeader::new(icons::label(icons::SCAN, "Collision Capsule"))
        .default_open(false)
        .show(ui, |ui| {
            changed |= drag_u16(ui, "Radius", &mut settings.radius, 1, 4096);
            changed |= drag_u16(ui, "Height", &mut settings.height, 1, 8192);
        });

    egui::CollapsingHeader::new(icons::label(icons::LAYERS, "Movement"))
        .default_open(true)
        .show(ui, |ui| {
            let walk_changed = drag_i32(ui, "Walk speed", &mut settings.walk_speed, 1, 1024);
            let run_changed = drag_i32(ui, "Run speed", &mut settings.run_speed, 1, 2048);
            changed |= walk_changed | run_changed;
            if walk_changed {
                *preview_action = Some(psxed_project::CharacterAnimationAction::Walk);
            } else if run_changed {
                *preview_action = Some(psxed_project::CharacterAnimationAction::Run);
            }
            let turn_changed = drag_u16(
                ui,
                "Turn speed (deg/s)",
                &mut settings.turn_speed_degrees_per_second,
                1,
                720,
            );
            changed |= turn_changed;
            if turn_changed {
                *preview_action = Some(psxed_project::CharacterAnimationAction::Turn);
            }
            draw_action_preview_buttons(
                ui,
                preview_action,
                &[
                    psxed_project::CharacterAnimationAction::Idle,
                    psxed_project::CharacterAnimationAction::Walk,
                    psxed_project::CharacterAnimationAction::Run,
                    psxed_project::CharacterAnimationAction::Turn,
                ],
            );
        });

    egui::CollapsingHeader::new(icons::label(icons::MOVE, "Stamina"))
        .default_open(false)
        .show(ui, |ui| {
            changed |= drag_i32(ui, "Max", &mut settings.stamina_max_q12, 1, 16384);
            changed |= drag_i32(ui, "Sprint start", &mut settings.sprint_min_q12, 0, 16384);
            changed |= drag_i32(ui, "Sprint drain", &mut settings.sprint_drain_q12, 0, 4096);
            changed |= drag_i32(ui, "Recover", &mut settings.stamina_recover_q12, 0, 4096);
        });

    egui::CollapsingHeader::new(icons::label(icons::PLAY, "Roll"))
        .default_open(false)
        .show(ui, |ui| {
            let mut section_changed = false;
            section_changed |= drag_i32(ui, "Cost", &mut settings.roll_cost_q12, 0, 16384);
            section_changed |= drag_i32(ui, "Speed", &mut settings.roll_speed, 1, 4096);
            section_changed |= drag_u8(
                ui,
                "Active frames",
                &mut settings.roll_active_frames,
                1,
                120,
            );
            section_changed |= drag_u8(
                ui,
                "Recovery frames",
                &mut settings.roll_recovery_frames,
                0,
                120,
            );
            section_changed |= drag_u8(
                ui,
                "Invulnerable frames",
                &mut settings.roll_invulnerable_frames,
                0,
                120,
            );
            changed |= section_changed;
            if section_changed {
                *preview_action = Some(psxed_project::CharacterAnimationAction::Roll);
            }
            draw_action_preview_buttons(
                ui,
                preview_action,
                &[psxed_project::CharacterAnimationAction::Roll],
            );
        });

    egui::CollapsingHeader::new(icons::label(icons::FOCUS, "Enemy AI"))
        .default_open(!player_controlled && settings.enemy.is_some())
        .show(ui, |ui| {
            if player_controlled {
                ui.label(
                    RichText::new("Enemy tuning is preserved but inactive while Role is Player.")
                        .color(STUDIO_TEXT_WEAK)
                        .small(),
                );
            }
            let Some(enemy) = settings.enemy.as_mut() else {
                ui.label(
                    RichText::new("Set Role to Enemy to create and configure enemy behavior.")
                        .color(STUDIO_TEXT_WEAK)
                        .small(),
                );
                return;
            };
            let (enemy_changed, attack_changed) = draw_enemy_behavior_fields(ui, enemy);
            changed |= enemy_changed;
            if attack_changed {
                *preview_action = Some(psxed_project::CharacterAnimationAction::LightAttack);
            }
            draw_action_preview_buttons(
                ui,
                preview_action,
                &[
                    psxed_project::CharacterAnimationAction::LightAttack,
                    psxed_project::CharacterAnimationAction::HeavyAttack,
                    psxed_project::CharacterAnimationAction::ComboAttack,
                    psxed_project::CharacterAnimationAction::Block,
                ],
            );

            draw_action_preview_buttons(
                ui,
                preview_action,
                &[
                    psxed_project::CharacterAnimationAction::HitReact,
                    psxed_project::CharacterAnimationAction::Death,
                ],
            );
        });

    changed
}

pub(crate) fn draw_character_controller_editor(
    ui: &mut egui::Ui,
    character: &mut Option<ResourceId>,
    settings: &mut CharacterControllerSettings,
    player: &mut bool,
    character_options: &[(ResourceId, String)],
    nav_target: &mut Option<ResourceId>,
    preview_action: &mut Option<psxed_project::CharacterAnimationAction>,
) -> bool {
    let mut changed = false;
    let mut role = CharacterControllerRole::from_controller(*player, settings);

    changed |= inspector_property_row(ui, "Role", |ui| {
        let previous = role;
        egui::ComboBox::from_id_salt("character-controller-role-picker")
            .selected_text(role.label())
            .width(ui.available_width().max(96.0))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut role, CharacterControllerRole::Passive, "Passive");
                ui.selectable_value(&mut role, CharacterControllerRole::Player, "Player");
                ui.selectable_value(&mut role, CharacterControllerRole::Enemy, "Enemy");
            });
        if role != previous {
            role.apply_to(player, settings)
        } else {
            false
        }
    });

    let role_hint = match role {
        CharacterControllerRole::Passive => "Movement settings are available, but no player input or enemy AI drives this character.",
        CharacterControllerRole::Player => "Receives player input. Any existing enemy tuning is preserved but inactive.",
        CharacterControllerRole::Enemy => "Driven by the Enemy AI settings below.",
    };
    ui.label(RichText::new(role_hint).color(STUDIO_TEXT_WEAK).small());

    changed |= draw_character_selector(ui, character_options, character, nav_target);
    ui.label(
        RichText::new(
            "Profile supplies reusable defaults; the values below belong to this instance.",
        )
        .color(STUDIO_TEXT_WEAK)
        .small(),
    );
    ui.add_space(4.0);
    changed |= draw_character_controller_settings(ui, settings, *player, preview_action);
    changed
}

fn draw_action_preview_buttons(
    ui: &mut egui::Ui,
    preview_action: &mut Option<psxed_project::CharacterAnimationAction>,
    actions: &[psxed_project::CharacterAnimationAction],
) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Preview").color(STUDIO_TEXT_WEAK).small());
        for action in actions {
            if ui
                .small_button(icons::label(icons::PLAY, action.label()))
                .on_hover_text(format!(
                    "Play the effective {} clip in the 3D viewport",
                    action.label()
                ))
                .clicked()
            {
                *preview_action = Some(*action);
            }
        }
    });
}

pub(crate) fn draw_character_effective_roles(
    ui: &mut egui::Ui,
    set: Option<&AnimationSetOption>,
    ctx: &CharacterEditorContext,
) {
    ui.add_space(4.0);
    ui.label(
        RichText::new("Effective actions")
            .color(STUDIO_TEXT_WEAK)
            .small(),
    );
    egui::Grid::new("character-effective-roles")
        .num_columns(2)
        .spacing([8.0, 3.0])
        .show(ui, |ui| {
            for action in psxed_project::CharacterAnimationAction::AUTHORABLE {
                let set_clip = set.and_then(|set| set.action_clips[action.to_index()]);
                ui.label(action.label());
                if let Some(clip) = set_clip {
                    ui.label(ctx.animation_clip_name(clip));
                } else if action.required_for_player() {
                    ui.colored_label(Color32::from_rgb(220, 120, 100), "missing");
                } else {
                    ui.label(RichText::new("(none)").color(STUDIO_TEXT_WEAK));
                }
                ui.end_row();
            }
        });
}

/// Helper: clip dropdown for one animation role. Renders the
/// clip's display name, falls back to "(none)" when unset, and
/// flags out-of-range indices in red.
pub(crate) fn clip_role_picker(
    ui: &mut egui::Ui,
    label: &str,
    id_salt: &str,
    slot: &mut Option<u16>,
    clips: &[String],
) -> bool {
    let before = *slot;
    let options = clips
        .iter()
        .enumerate()
        .map(|(index, name)| (index as u16, format!("{index}: {name}")))
        .collect::<Vec<_>>();
    ui.horizontal(|ui| {
        ui.label(label);
        let preview = match *slot {
            Some(idx) => clips
                .get(idx as usize)
                .cloned()
                .unwrap_or_else(|| format!("#{idx} (missing)")),
            None => "(none)".to_string(),
        };
        let _ = searchable_picker(
            ui,
            id_salt,
            slot,
            &preview,
            &options,
            SearchablePickerConfig::optional("(none)")
                .with_popup_min_width(380.0)
                .with_search_hint("Search imported animations…"),
        );
    });
    *slot != before
}

pub(crate) fn draw_animator_action_clip_table(
    ui: &mut egui::Ui,
    action_clips: &mut Vec<psxed_project::CharacterActionClip>,
    context: &AnimatorClipContext,
) -> bool {
    let mut changed = false;

    let selected_action_id = ui.make_persistent_id("animator-selected-action");
    let mut selected_index = ui
        .memory_mut(|memory| memory.data.get_persisted::<usize>(selected_action_id))
        .unwrap_or(psxed_project::CharacterAnimationAction::Idle.to_index())
        .min(
            psxed_project::CharacterAnimationAction::AUTHORABLE
                .len()
                .saturating_sub(1),
        );
    let before_selected_index = selected_index;
    let selected_action = psxed_project::CharacterAnimationAction::AUTHORABLE[selected_index];

    ui.horizontal(|ui| {
        ui.label(RichText::new("Action").color(STUDIO_TEXT_WEAK));
        egui::ComboBox::from_id_salt("animator-selected-action-picker")
            .selected_text(selected_action.label())
            .width(180.0)
            .show_ui(ui, |ui| {
                for (index, action) in psxed_project::CharacterAnimationAction::AUTHORABLE
                    .iter()
                    .copied()
                    .enumerate()
                {
                    if ui
                        .selectable_label(selected_index == index, action.label())
                        .clicked()
                    {
                        selected_index = index;
                    }
                }
            });
    });
    if selected_index != before_selected_index {
        ui.memory_mut(|memory| {
            memory
                .data
                .insert_persisted(selected_action_id, selected_index)
        });
    }
    let action = psxed_project::CharacterAnimationAction::AUTHORABLE[selected_index];
    let binding_index = action_clips
        .iter()
        .position(|binding| binding.action == action);
    let inherited_clip = context.profile_action_clips[action.to_index()];
    let mut current = binding_index.map(|index| action_clips[index].clip);
    let mut options = binding_index
        .and_then(|index| action_clips[index].options)
        .unwrap_or_else(|| {
            animator_action_option_defaults(action, current, inherited_clip, context)
        });
    let before_options = options;

    ui.add_space(2.0);
    egui::Frame::new()
        .fill(STUDIO_PANEL_HEADER)
        .stroke(Stroke::new(1.0, STUDIO_BORDER))
        .corner_radius(egui::CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(8, 7))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width().max(220.0));
            ui.horizontal(|ui| {
                ui.label(RichText::new(action.label()).strong());
                if inherited_clip.is_some() && current.is_none() {
                    ui.label(
                        RichText::new("using profile default")
                            .small()
                            .color(STUDIO_TEXT_WEAK),
                    );
                }
            });

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Clip").color(STUDIO_TEXT_WEAK));
                let combo_width = (ui.available_width() - 4.0).max(180.0);
                if animator_action_clip_combo(
                    ui,
                    &format!("animator-action-{}", action.to_index()),
                    combo_width,
                    &mut current,
                    inherited_clip,
                    context.profile_name.as_deref(),
                    &context.clips,
                ) {
                    set_node_action_clip(action_clips, action, current);
                    changed = true;
                }
            });

            let effective_clip = current.or(inherited_clip);
            let enabled = effective_clip.is_some();
            let frame_count = effective_clip
                .and_then(|clip| context.clip_frame_counts.get(clip as usize))
                .copied()
                .flatten();

            ui.add_space(8.0);
            ui.add_enabled_ui(enabled, |ui| {
                let slider_width = ui.available_width().max(220.0);
                changed |= animator_action_frame_range_controls(
                    ui,
                    &format!("animator-action-range-{}", action.to_index()),
                    slider_width,
                    &mut options,
                    frame_count,
                );
            });

            ui.add_space(8.0);
            ui.add_enabled_ui(enabled, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Playback speed").color(STUDIO_TEXT_WEAK));
                    let mut speed_mult = options.speed_q8 as f32 / 256.0;
                    if ui
                        .add(
                            egui::DragValue::new(&mut speed_mult)
                                .speed(0.01)
                                .range(0.25..=4.0)
                                .fixed_decimals(2)
                                .suffix("x"),
                        )
                        .on_hover_text("Playback speed for this action (1.00x = authored rate)")
                        .changed()
                    {
                        options.speed_q8 = (speed_mult * 256.0).round().clamp(
                            psxed_project::ACTION_SPEED_MIN_Q8 as f32,
                            psxed_project::ACTION_SPEED_MAX_Q8 as f32,
                        ) as u16;
                    }
                });
                ui.horizontal(|ui| {
                    changed |= ui
                        .checkbox(&mut options.looping, "Loop")
                        .on_hover_text("Loop this clip while the action remains active")
                        .changed();
                    changed |= ui
                        .checkbox(&mut options.in_place, "In-place")
                        .on_hover_text("Cancel root translation for this action")
                        .changed();
                });

                ui.add_space(8.0);
                ui.separator();
                ui.label(RichText::new("Movement").color(STUDIO_TEXT_WEAK).small());
                changed |= drag_i32(ui, "Forward push", &mut options.push_distance, 0, 8192);
                ui.add_enabled_ui(options.push_distance > 0, |ui| {
                    changed |= animator_action_push_frame_range_controls(
                        ui,
                        &format!("animator-action-push-range-{}", action.to_index()),
                        ui.available_width().max(220.0),
                        &mut options,
                        frame_count,
                    );
                });
            });

            if !enabled {
                ui.add_space(6.0);
                ui.weak("Choose a clip to edit playback details.");
            }
        });

    if current.or(inherited_clip).is_some() && options != before_options {
        if current.is_none() {
            if let Some(clip) = inherited_clip {
                current = Some(clip);
                set_node_action_clip(action_clips, action, current);
            }
        }
        let defaults = animator_action_option_defaults(action, current, inherited_clip, context);
        let stored = (options != defaults).then_some(options);
        set_node_action_options(action_clips, action, stored);
        changed = true;
    }

    changed
}

fn animator_action_frame_range_controls(
    ui: &mut egui::Ui,
    id_salt: &str,
    width: f32,
    options: &mut psxed_project::CharacterActionOptions,
    frame_count: Option<u16>,
) -> bool {
    let Some(frame_count) = frame_count else {
        ui.add_sized(
            [width, 18.0],
            egui::Label::new(
                RichText::new("Frames unavailable")
                    .small()
                    .color(STUDIO_TEXT_WEAK),
            ),
        );
        return false;
    };
    let max_frame = frame_count.saturating_sub(2);
    let mut start = options.frame_start.min(max_frame);
    let mut end = if options.frame_end == psxed_project::ACTION_FRAME_END_FULL {
        max_frame
    } else {
        options.frame_end.min(max_frame)
    };
    if end < start {
        end = start;
    }

    let mut changed = false;
    ui.vertical(|ui| {
        ui.set_width(width);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Frames").small().color(STUDIO_TEXT_WEAK));
            ui.label(RichText::new(format!("{start}-{end}")).small());
        });
        changed |= double_frame_range_slider(ui, id_salt, width, &mut start, &mut end, max_frame);
    });

    if changed {
        options.frame_start = start;
        options.frame_end = if end == max_frame {
            psxed_project::ACTION_FRAME_END_FULL
        } else {
            end
        };
    }
    changed
}

fn animator_action_push_frame_range_controls(
    ui: &mut egui::Ui,
    id_salt: &str,
    width: f32,
    options: &mut psxed_project::CharacterActionOptions,
    frame_count: Option<u16>,
) -> bool {
    let Some(frame_count) = frame_count else {
        ui.add_sized(
            [width, 18.0],
            egui::Label::new(
                RichText::new("Push frames unavailable")
                    .small()
                    .color(STUDIO_TEXT_WEAK),
            ),
        );
        return false;
    };
    let max_frame = frame_count.saturating_sub(2);
    let mut start = options.push_frame_start.min(max_frame);
    let mut end = if options.push_frame_end == psxed_project::ACTION_FRAME_END_FULL {
        max_frame
    } else {
        options.push_frame_end.min(max_frame)
    };
    if end < start {
        end = start;
    }

    let mut changed = false;
    ui.vertical(|ui| {
        ui.set_width(width);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Push frames").small().color(STUDIO_TEXT_WEAK));
            ui.label(RichText::new(format!("{start}-{end}")).small());
        });
        changed |= double_frame_range_slider(ui, id_salt, width, &mut start, &mut end, max_frame);
    });

    if changed {
        options.push_frame_start = start;
        options.push_frame_end = if end == max_frame {
            psxed_project::ACTION_FRAME_END_FULL
        } else {
            end
        };
    }
    changed
}

fn double_frame_range_slider(
    ui: &mut egui::Ui,
    id_salt: &str,
    width: f32,
    start: &mut u16,
    end: &mut u16,
    max_frame: u16,
) -> bool {
    let id = ui.make_persistent_id(id_salt);
    let desired = Vec2::new(width, 18.0);
    let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click_and_drag());
    let rail = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 6.0, rect.center().y - 1.0),
        egui::pos2(rect.right() - 6.0, rect.center().y + 1.0),
    );
    let to_x = |frame: u16| {
        if max_frame == 0 {
            rail.left()
        } else {
            rail.left() + rail.width() * (frame as f32 / max_frame as f32)
        }
    };
    let start_x = to_x(*start);
    let end_x = to_x(*end);
    let painter = ui.painter();
    painter.rect_filled(rail, 1.0, Color32::from_rgb(52, 58, 70));
    let active = egui::Rect::from_min_max(
        egui::pos2(start_x.min(end_x), rail.top()),
        egui::pos2(start_x.max(end_x), rail.bottom()),
    );
    painter.rect_filled(active, 1.0, STUDIO_ACCENT);
    for x in [start_x, end_x] {
        painter.circle_filled(egui::pos2(x, rect.center().y), 5.0, STUDIO_TEXT);
        painter.circle_stroke(
            egui::pos2(x, rect.center().y),
            5.0,
            Stroke::new(1.0, STUDIO_PANEL_DARK),
        );
    }

    let mut changed = false;
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    if max_frame > 0 && (response.clicked() || response.dragged()) {
        if let Some(pointer) = response.interact_pointer_pos() {
            let t = ((pointer.x - rail.left()) / rail.width()).clamp(0.0, 1.0);
            let frame = (t * max_frame as f32).round() as u16;
            let use_start = ui
                .memory_mut(|memory| memory.data.get_temp::<bool>(id))
                .unwrap_or_else(|| {
                    let start_distance = frame.abs_diff(*start);
                    let end_distance = frame.abs_diff(*end);
                    start_distance <= end_distance
                });
            ui.memory_mut(|memory| memory.data.insert_temp(id, use_start));
            if use_start {
                let next = frame.min(*end);
                changed = next != *start;
                *start = next;
            } else {
                let next = frame.max(*start);
                changed = next != *end;
                *end = next;
            }
        }
    }
    if response.drag_stopped() || (response.clicked() && !response.dragged()) {
        ui.memory_mut(|memory| memory.data.remove::<bool>(id));
    }
    changed
}

pub(crate) fn animator_action_option_defaults(
    action: psxed_project::CharacterAnimationAction,
    current: Option<u16>,
    inherited: Option<u16>,
    context: &AnimatorClipContext,
) -> psxed_project::CharacterActionOptions {
    let clip = current.or(inherited);
    psxed_project::CharacterActionOptions {
        looping: action.loops_by_default(),
        in_place: clip
            .and_then(|idx| context.clip_in_place_defaults.get(idx as usize).copied())
            .unwrap_or(true),
        speed_q8: psxed_project::ACTION_SPEED_UNSCALED_Q8,
        frame_start: 0,
        frame_end: psxed_project::ACTION_FRAME_END_FULL,
        push_distance: 0,
        push_frame_start: 0,
        push_frame_end: psxed_project::ACTION_FRAME_END_FULL,
    }
}

pub(crate) fn animator_action_clip_combo(
    ui: &mut egui::Ui,
    id_salt: &str,
    width: f32,
    slot: &mut Option<u16>,
    inherited_clip: Option<u16>,
    inherited_source: Option<&str>,
    clips: &[String],
) -> bool {
    let before = *slot;
    let preview = match *slot {
        Some(idx) => clips
            .get(idx as usize)
            .map(|name| format!("{idx}: {name}"))
            .unwrap_or_else(|| format!("#{idx} (missing)")),
        None => inherited_clip
            .map(|idx| {
                let name = clips
                    .get(idx as usize)
                    .map(String::as_str)
                    .unwrap_or("(missing)");
                match inherited_source {
                    Some(source) => format!("{idx}: {name} from {source}"),
                    None => format!("{idx}: {name} inherited"),
                }
            })
            .unwrap_or_else(|| "(none)".to_string()),
    };
    let options = clips
        .iter()
        .enumerate()
        .map(|(index, name)| (index as u16, format!("{index}: {name}")))
        .collect::<Vec<_>>();
    let inherit_label = inherited_clip
        .map(|idx| {
            let name = clips
                .get(idx as usize)
                .map(String::as_str)
                .unwrap_or("(missing)");
            format!("Inherit {idx}: {name}")
        })
        .unwrap_or_else(|| "(none)".to_string());
    let _ = searchable_picker(
        ui,
        id_salt,
        slot,
        &preview,
        &options,
        SearchablePickerConfig::optional(&inherit_label)
            .with_width(width)
            .with_popup_min_width(width.max(380.0))
            .with_search_hint("Search imported animations…"),
    );
    *slot != before
}

pub(crate) fn animation_picker_filter(ui: &mut egui::Ui, id: egui::Id) -> String {
    let mut filter = ui
        .memory_mut(|memory| memory.data.get_persisted::<String>(id))
        .unwrap_or_default();
    if ui
        .add(
            egui::TextEdit::singleline(&mut filter)
                .hint_text("Search imported animations…")
                .desired_width(f32::INFINITY),
        )
        .changed()
    {
        ui.memory_mut(|memory| memory.data.insert_persisted(id, filter.clone()));
    }
    filter
}

pub(crate) fn animation_name_matches_filter(name: &str, filter: &str) -> bool {
    let filter = filter.trim().to_ascii_lowercase();
    if filter.is_empty() {
        return true;
    }
    let name = name.to_ascii_lowercase();
    filter.split_whitespace().all(|term| name.contains(term))
}

pub(crate) fn set_node_action_clip(
    action_clips: &mut Vec<psxed_project::CharacterActionClip>,
    action: psxed_project::CharacterAnimationAction,
    clip: Option<u16>,
) {
    match clip {
        Some(clip) => {
            if let Some(binding) = action_clips
                .iter_mut()
                .find(|binding| binding.action == action)
            {
                binding.clip = clip;
            } else {
                action_clips.push(psxed_project::CharacterActionClip {
                    action,
                    clip,
                    options: None,
                });
            }
        }
        None => action_clips.retain(|binding| binding.action != action),
    }
}

pub(crate) fn set_node_action_options(
    action_clips: &mut [psxed_project::CharacterActionClip],
    action: psxed_project::CharacterAnimationAction,
    options: Option<psxed_project::CharacterActionOptions>,
) {
    if let Some(binding) = action_clips
        .iter_mut()
        .find(|binding| binding.action == action)
    {
        binding.options = options;
    }
}

/// Character-controller / player-spawn inspector helper: pick an
/// optional Character Profile preset. Component-authored players can
/// leave this empty when a Model Renderer and Animator are present;
/// legacy SpawnPoint players can still auto-pick when exactly one
/// profile exists.
pub(crate) fn draw_character_selector(
    ui: &mut egui::Ui,
    options: &[(ResourceId, String)],
    current: &mut Option<ResourceId>,
    jump_to: &mut Option<ResourceId>,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label("Profile");
        let preview = current
            .and_then(|id| {
                options
                    .iter()
                    .find(|(rid, _)| *rid == id)
                    .map(|(_, n)| n.as_str())
            })
            .unwrap_or("(none)");
        changed |= searchable_picker(
            ui,
            "player-spawn-character-picker",
            current,
            preview,
            options,
            SearchablePickerConfig::optional("(none)")
                .with_search_hint("Search character profiles…"),
        );
        if let Some(id) = *current {
            if options.iter().any(|(rid, _)| *rid == id)
                && ui
                    .small_button("Open")
                    .on_hover_text("Open this Character Profile in the resource inspector.")
                    .clicked()
            {
                *jump_to = Some(id);
            }
        }
    });
    if let Some(id) = *current {
        if !options.iter().any(|(rid, _)| *rid == id) {
            ui.colored_label(
                Color32::from_rgb(220, 120, 100),
                "Selected Character Profile resource is missing.",
            );
        }
    } else if options.is_empty() {
        ui.colored_label(
            STUDIO_TEXT_WEAK,
            "No Character Profile resources defined. Cook will fail unless one is added.",
        );
    } else if options.len() > 1 {
        ui.colored_label(
            STUDIO_TEXT_WEAK,
            "Multiple Character Profiles available - pick one explicitly to avoid Cook failures.",
        );
    } else {
        ui.colored_label(
            STUDIO_TEXT_WEAK,
            format!(
                "Legacy SpawnPoint cooks can auto-select \"{}\" because it is the only profile.",
                options[0].1
            ),
        );
    }
    changed
}

pub(crate) fn draw_weapon_selector(
    ui: &mut egui::Ui,
    options: &[(ResourceId, String)],
    current: &mut Option<ResourceId>,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label("Weapon");
        let preview = current
            .and_then(|id| {
                options
                    .iter()
                    .find(|(rid, _)| *rid == id)
                    .map(|(_, n)| n.as_str())
            })
            .unwrap_or("(none)");
        changed |= searchable_picker(
            ui,
            "equipment-weapon-picker",
            current,
            preview,
            options,
            SearchablePickerConfig::optional("(none)").with_search_hint("Search weapons…"),
        );
    });
    if let Some(id) = *current {
        if !options.iter().any(|(rid, _)| *rid == id) {
            ui.colored_label(
                Color32::from_rgb(220, 120, 100),
                "Weapon resource is missing.",
            );
        }
    } else if options.is_empty() {
        ui.colored_label(STUDIO_TEXT_WEAK, "No Weapon resources defined yet.");
    }
    changed
}

pub(crate) fn drag_u16(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut u16,
    min: u16,
    max: u16,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        let mut v = *value as i64;
        if ui
            .add(egui::DragValue::new(&mut v).range(min as i64..=max as i64))
            .changed()
        {
            *value = v.clamp(min as i64, max as i64) as u16;
            changed = true;
        }
    });
    changed
}

pub(crate) fn drag_i16(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut i16,
    min: i16,
    max: i16,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        let mut v = *value as i64;
        if ui
            .add(egui::DragValue::new(&mut v).range(min as i64..=max as i64))
            .changed()
        {
            *value = v.clamp(min as i64, max as i64) as i16;
            changed = true;
        }
    });
    changed
}

pub(crate) fn draw_ui_rect_editor(ui: &mut egui::Ui, rect: &mut UiRect) -> bool {
    let mut changed = false;
    egui::Grid::new(ui.id().with("ui_rect_grid"))
        .num_columns(2)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            changed |= drag_i16(ui, "X", &mut rect.x, -4096, 4096);
            changed |= drag_i16(ui, "Y", &mut rect.y, -4096, 4096);
            ui.end_row();
            changed |= drag_u16(ui, "W", &mut rect.width, 1, 4096);
            changed |= drag_u16(ui, "H", &mut rect.height, 1, 4096);
            ui.end_row();
            changed |= drag_i16(ui, "Rotate", &mut rect.rotation_degrees, -359, 359);
            ui.horizontal(|ui| {
                ui.label("Flip");
                changed |= ui.checkbox(&mut rect.flip_x, "X").changed();
                changed |= ui.checkbox(&mut rect.flip_y, "Y").changed();
            });
            ui.end_row();
        });
    changed |= draw_ui_anchor_editor(ui, &mut rect.anchor);
    changed
}

/// Shared clipped-corner/border editor for shape-backed Rect and Button
/// nodes. The local copy keeps `shape: None` for the legacy/default style so
/// merely opening the inspector does not churn old project files.
pub(crate) fn draw_ui_shape_style_editor(
    ui: &mut egui::Ui,
    transparent: &mut bool,
    shape: &mut Option<UiShapeStyle>,
) -> bool {
    let mut changed = false;
    let mut filled = !*transparent;
    if ui.checkbox(&mut filled, "Fill").changed() {
        *transparent = !filled;
        changed = true;
    }

    let original = *shape;
    let mut style = shape.unwrap_or_default();
    ui.add_enabled_ui(filled, |ui| {
        changed |= ui
            .checkbox(&mut style.semi_transparent_fill, "Semi-transparent fill")
            .on_hover_text("PS1 native 50/50 blend with the framebuffer")
            .changed();
    });
    ui.separator();
    ui.strong("45-degree corner cuts");
    changed |= inspector_property_row(ui, "Cut size", |ui| {
        ui.add(
            egui::DragValue::new(&mut style.corner_cut)
                .range(0..=63)
                .suffix(" px"),
        )
        .changed()
    });
    ui.horizontal_wrapped(|ui| {
        ui.label("Corners");
        changed |= ui.checkbox(&mut style.cut_top_left, "TL").changed();
        changed |= ui.checkbox(&mut style.cut_top_right, "TR").changed();
        changed |= ui.checkbox(&mut style.cut_bottom_left, "BL").changed();
        changed |= ui.checkbox(&mut style.cut_bottom_right, "BR").changed();
    });
    ui.horizontal_wrapped(|ui| {
        ui.weak("Presets");
        if ui.small_button("TL + BR").clicked() {
            style.cut_top_left = true;
            style.cut_top_right = false;
            style.cut_bottom_left = false;
            style.cut_bottom_right = true;
            style.corner_cut = style.corner_cut.max(4);
            changed = true;
        }
        if ui.small_button("TR + BL").clicked() {
            style.cut_top_left = false;
            style.cut_top_right = true;
            style.cut_bottom_left = true;
            style.cut_bottom_right = false;
            style.corner_cut = style.corner_cut.max(4);
            changed = true;
        }
        if ui.small_button("All").clicked() {
            style.cut_top_left = true;
            style.cut_top_right = true;
            style.cut_bottom_left = true;
            style.cut_bottom_right = true;
            style.corner_cut = style.corner_cut.max(4);
            changed = true;
        }
        if ui.small_button("Clear").clicked() {
            style.corner_cut = 0;
            style.cut_top_left = false;
            style.cut_top_right = false;
            style.cut_bottom_left = false;
            style.cut_bottom_right = false;
            changed = true;
        }
    });

    ui.separator();
    let mut border_enabled = style.border_width != 0;
    if ui.checkbox(&mut border_enabled, "Border").changed() {
        style.border_width = if border_enabled { 1 } else { 0 };
        changed = true;
    }
    if border_enabled {
        changed |= inspector_property_row(ui, "Border width", |ui| {
            ui.add(
                egui::DragValue::new(&mut style.border_width)
                    .range(1..=7)
                    .suffix(" px"),
            )
            .changed()
        });
        changed |= color_editor(ui, "Border", &mut style.border_color);
        changed |= draw_ui_gradient_editor(
            ui,
            "Border Gradient",
            &style.border_color,
            &mut style.border_gradient,
        );
    }
    if *transparent && !border_enabled {
        ui.weak("Enable a border to keep this transparent shape visible.");
    }

    let next = (style != UiShapeStyle::default()).then_some(style);
    if next != original {
        *shape = next;
        changed = true;
    }
    changed
}

pub(crate) fn draw_ui_anchor_editor(ui: &mut egui::Ui, anchor: &mut UiAnchor) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label("Anchor");
        ui.weak(anchor.label());
    });
    egui::Grid::new(ui.id().with("ui_anchor_grid"))
        .num_columns(3)
        .spacing([3.0, 3.0])
        .show(ui, |ui| {
            for (index, candidate) in UiAnchor::ALL.into_iter().enumerate() {
                let response = ui
                    .selectable_label(*anchor == candidate, candidate.short_label())
                    .on_hover_text(candidate.label());
                if response.clicked() && *anchor != candidate {
                    *anchor = candidate;
                    changed = true;
                }
                if index % 3 == 2 {
                    ui.end_row();
                }
            }
        });
    changed
}

pub(crate) fn draw_ui_text_align_editor(ui: &mut egui::Ui, align: &mut UiTextAlign) -> bool {
    let mut changed = false;
    egui::ComboBox::from_label("Align")
        .selected_text(align.label())
        .show_ui(ui, |ui| {
            for candidate in UiTextAlign::ALL {
                if ui
                    .selectable_label(*align == candidate, candidate.label())
                    .clicked()
                    && *align != candidate
                {
                    *align = candidate;
                    changed = true;
                }
            }
        });
    changed
}

pub(crate) fn draw_ui_visibility_editor(
    ui: &mut egui::Ui,
    visible_when: &mut UiVisibilityCondition,
) -> bool {
    let mut changed = false;
    egui::ComboBox::from_label("Visible when")
        .selected_text(visible_when.label())
        .show_ui(ui, |ui| {
            for candidate in UiVisibilityCondition::ALL {
                if ui
                    .selectable_label(*visible_when == candidate, candidate.label())
                    .clicked()
                    && *visible_when != candidate
                {
                    *visible_when = candidate;
                    changed = true;
                }
            }
        });
    changed
}

pub(crate) fn draw_ui_image_effect_picker(ui: &mut egui::Ui, effect: &mut UiImageEffect) -> bool {
    let mut changed = false;
    egui::ComboBox::from_label("Effect")
        .selected_text(effect.label())
        .show_ui(ui, |ui| {
            for candidate in UiImageEffect::ALL {
                if ui
                    .selectable_label(*effect == candidate, candidate.label())
                    .clicked()
                    && *effect != candidate
                {
                    *effect = candidate;
                    changed = true;
                }
            }
        });
    changed
}

pub(crate) fn draw_ui_focus_effect_picker(ui: &mut egui::Ui, effect: &mut UiFocusEffect) -> bool {
    let mut changed = false;
    egui::ComboBox::from_label("Focus Effect")
        .selected_text(effect.label())
        .show_ui(ui, |ui| {
            for candidate in UiFocusEffect::ALL {
                if ui
                    .selectable_label(*effect == candidate, candidate.label())
                    .clicked()
                    && *effect != candidate
                {
                    *effect = candidate;
                    changed = true;
                }
            }
        });
    changed
}

pub(crate) fn draw_ui_font_choice_editor(ui: &mut egui::Ui, font: &mut UiFontChoice) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label("Font");
        if ui
            .add(egui::Button::new(icons::text(icons::CHEVRON_LEFT, 12.0)))
            .on_hover_text("Previous font")
            .clicked()
        {
            changed |= step_ui_font_choice(font, -1);
        }
        egui::ComboBox::from_id_salt(ui.id().with("font_choice"))
            .selected_text(font.label())
            .show_ui(ui, |ui| {
                for candidate in UI_FONT_CHOICES {
                    if ui
                        .selectable_label(*font == candidate, candidate.label())
                        .clicked()
                        && *font != candidate
                    {
                        *font = candidate;
                        changed = true;
                    }
                }
            });
        if ui
            .add(egui::Button::new(icons::text(icons::CHEVRON_RIGHT, 12.0)))
            .on_hover_text("Next font")
            .clicked()
        {
            changed |= step_ui_font_choice(font, 1);
        }
    });
    changed
}

pub(crate) fn step_ui_font_choice(font: &mut UiFontChoice, delta: isize) -> bool {
    let Some(index) = UI_FONT_CHOICES
        .iter()
        .position(|candidate| *candidate == *font)
    else {
        *font = UiFontChoice::default();
        return true;
    };
    let len = UI_FONT_CHOICES.len() as isize;
    let next = (index as isize + delta).rem_euclid(len) as usize;
    let next_font = UI_FONT_CHOICES[next];
    if *font == next_font {
        return false;
    }
    *font = next_font;
    true
}

pub(crate) fn draw_ui_font_scale_editor(ui: &mut egui::Ui, font_scale: &mut u16) -> bool {
    let mut changed = false;
    let mut value = ui_font_scale_q8_to_f32(*font_scale);
    ui.horizontal(|ui| {
        ui.label("Size");
        changed |= ui
            .add(
                egui::DragValue::new(&mut value)
                    .range(
                        ui_font_scale_q8_to_f32(MIN_UI_FONT_SCALE)
                            ..=ui_font_scale_q8_to_f32(MAX_UI_FONT_SCALE),
                    )
                    .speed(0.05)
                    .suffix("x"),
            )
            .changed();
    });
    let clamped = ui_font_scale_f32_to_q8(value);
    if *font_scale != clamped {
        *font_scale = clamped;
        changed = true;
    }
    changed
}

pub(crate) fn draw_ui_letter_spacing_editor(ui: &mut egui::Ui, letter_spacing: &mut i8) -> bool {
    let mut changed = false;
    let mut value = (*letter_spacing).clamp(MIN_UI_LETTER_SPACING, MAX_UI_LETTER_SPACING) as i32;
    ui.horizontal(|ui| {
        ui.label("Letter spacing");
        changed |= ui
            .add(
                egui::DragValue::new(&mut value)
                    .range(i32::from(MIN_UI_LETTER_SPACING)..=i32::from(MAX_UI_LETTER_SPACING))
                    .speed(1.0)
                    .suffix(" px"),
            )
            .changed();
    });
    let clamped = value.clamp(
        i32::from(MIN_UI_LETTER_SPACING),
        i32::from(MAX_UI_LETTER_SPACING),
    ) as i8;
    if *letter_spacing != clamped {
        *letter_spacing = clamped;
        changed = true;
    }
    changed
}

pub(crate) fn ui_texture_resource_picker(
    ui: &mut egui::Ui,
    label: &str,
    current: &mut Option<ResourceId>,
    options: &[(ResourceId, String)],
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        let preview = current
            .and_then(|id| {
                options
                    .iter()
                    .find(|(rid, _)| *rid == id)
                    .map(|(_, name)| name.as_str())
            })
            .unwrap_or("(none)");
        changed |= searchable_picker(
            ui,
            ui.id().with(("ui-texture-resource-picker", label)),
            current,
            preview,
            options,
            SearchablePickerConfig::optional("(none)"),
        );
    });
    changed
}

pub(crate) fn draw_ui_value_binding_editor(
    ui: &mut egui::Ui,
    label: &str,
    binding: &mut UiValueBinding,
    option_choices: &[(OptionId, String)],
) -> bool {
    let mut changed = false;
    egui::ComboBox::from_label(label)
        .selected_text(binding.label())
        .show_ui(ui, |ui| {
            for (candidate_label, candidate) in [
                ("Player Health", UiValueBinding::PlayerHealth),
                ("Player Health Max", UiValueBinding::PlayerHealthMax),
                (
                    "Player Secondary Health",
                    UiValueBinding::PlayerHealthSecondary,
                ),
                (
                    "Player Secondary Health Max",
                    UiValueBinding::PlayerHealthSecondaryMax,
                ),
                (
                    "Player Health Empty Influence",
                    UiValueBinding::PlayerHealthEmptyInfluence,
                ),
                (
                    "Player Health Full Influence",
                    UiValueBinding::PlayerHealthFullInfluence,
                ),
                (
                    "Secondary Empty Influence",
                    UiValueBinding::PlayerHealthSecondaryEmptyInfluence,
                ),
                (
                    "Secondary Full Influence",
                    UiValueBinding::PlayerHealthSecondaryFullInfluence,
                ),
                ("Player Stamina", UiValueBinding::PlayerStamina),
                ("Player Stamina Max", UiValueBinding::PlayerStaminaMax),
                ("Constant", UiValueBinding::ConstantQ12(4096)),
                (
                    "Option",
                    UiValueBinding::Option(
                        option_choices
                            .first()
                            .map(|(id, _)| *id)
                            .unwrap_or_default(),
                    ),
                ),
            ] {
                if ui
                    .selectable_label(
                        ui_value_binding_same_variant(*binding, candidate),
                        candidate_label,
                    )
                    .clicked()
                    && !ui_value_binding_same_variant(*binding, candidate)
                {
                    *binding = candidate;
                    changed = true;
                }
            }
        });
    if let UiValueBinding::ConstantQ12(value) = binding {
        changed |= drag_i32(ui, "Q12", value, 0, 65536);
    }
    if let UiValueBinding::Option(option) = binding {
        changed |= draw_ui_option_picker(ui, "Option", option, option_choices);
    }
    changed
}

pub(crate) fn ui_value_binding_same_variant(a: UiValueBinding, b: UiValueBinding) -> bool {
    matches!(
        (a, b),
        (
            UiValueBinding::ConstantQ12(_),
            UiValueBinding::ConstantQ12(_)
        ) | (UiValueBinding::PlayerHealth, UiValueBinding::PlayerHealth)
            | (UiValueBinding::Option(_), UiValueBinding::Option(_))
            | (
                UiValueBinding::PlayerHealthMax,
                UiValueBinding::PlayerHealthMax
            )
            | (
                UiValueBinding::PlayerHealthSecondary,
                UiValueBinding::PlayerHealthSecondary
            )
            | (
                UiValueBinding::PlayerHealthSecondaryMax,
                UiValueBinding::PlayerHealthSecondaryMax
            )
            | (
                UiValueBinding::PlayerHealthEmptyInfluence,
                UiValueBinding::PlayerHealthEmptyInfluence
            )
            | (
                UiValueBinding::PlayerHealthFullInfluence,
                UiValueBinding::PlayerHealthFullInfluence
            )
            | (
                UiValueBinding::PlayerHealthSecondaryEmptyInfluence,
                UiValueBinding::PlayerHealthSecondaryEmptyInfluence
            )
            | (
                UiValueBinding::PlayerHealthSecondaryFullInfluence,
                UiValueBinding::PlayerHealthSecondaryFullInfluence
            )
            | (UiValueBinding::PlayerStamina, UiValueBinding::PlayerStamina)
            | (
                UiValueBinding::PlayerStaminaMax,
                UiValueBinding::PlayerStaminaMax
            )
    )
}

/// `true` when two [`UiAction`]s are the same variant, ignoring their
/// payloads. Used to keep the variant combo selection stable while the
/// per-variant payload controls edit the data.
pub(crate) fn ui_action_same_variant(a: &UiAction, b: &UiAction) -> bool {
    matches!(
        (a, b),
        (UiAction::GotoState(_), UiAction::GotoState(_))
            | (
                UiAction::TransitionToState { .. },
                UiAction::TransitionToState { .. }
            )
            | (UiAction::GotoScene(_), UiAction::GotoScene(_))
            | (
                UiAction::TransitionToScene { .. },
                UiAction::TransitionToScene { .. }
            )
            | (UiAction::StartGameplay, UiAction::StartGameplay)
            | (
                UiAction::StartGameplayTransition { .. },
                UiAction::StartGameplayTransition { .. }
            )
            | (UiAction::Back, UiAction::Back)
            | (UiAction::SetOption { .. }, UiAction::SetOption { .. })
            | (UiAction::Game(_), UiAction::Game(_))
    )
}

/// Editor for a button's [`UiAction`]: a variant combo plus the
/// per-variant payload (a scene dropdown for `GotoScene`, an option
/// dropdown + delta for `SetOption`, a raw id for `Game`).
pub(crate) fn draw_ui_action_editor(
    ui: &mut egui::Ui,
    action: &mut UiAction,
    state_options: &[(SceneStateId, String)],
    scene_options: &[(UiSceneId, String)],
    option_choices: &[(OptionId, String)],
) -> bool {
    let mut changed = false;
    let variants = [
        UiAction::GotoState(state_options.first().map(|(id, _)| *id).unwrap_or_default()),
        UiAction::TransitionToState {
            state: state_options.first().map(|(id, _)| *id).unwrap_or_default(),
            transition: UiTransition::glitch_break(),
        },
        UiAction::GotoScene(scene_options.first().map(|(id, _)| *id).unwrap_or_default()),
        UiAction::TransitionToScene {
            scene: scene_options.first().map(|(id, _)| *id).unwrap_or_default(),
            transition: UiTransition::glitch_break(),
        },
        UiAction::StartGameplay,
        UiAction::StartGameplayTransition {
            transition: UiTransition::glitch_break(),
        },
        UiAction::Back,
        UiAction::SetOption {
            option: option_choices
                .first()
                .map(|(id, _)| *id)
                .unwrap_or_default(),
            delta: 1,
        },
        UiAction::Game(0),
    ];
    egui::ComboBox::from_label("Action")
        .selected_text(action.label())
        .show_ui(ui, |ui| {
            for candidate in &variants {
                if ui
                    .selectable_label(ui_action_same_variant(action, candidate), candidate.label())
                    .clicked()
                    && !ui_action_same_variant(action, candidate)
                {
                    *action = *candidate;
                    changed = true;
                }
            }
        });
    match action {
        UiAction::GotoState(state) => {
            changed |= draw_scene_state_picker(ui, "Target", state, state_options);
        }
        UiAction::TransitionToState { state, transition } => {
            changed |= draw_scene_state_picker(ui, "Target", state, state_options);
            changed |= draw_ui_transition_editor(ui, transition);
        }
        UiAction::GotoScene(scene) => {
            changed |= draw_ui_scene_picker(ui, "Target", scene, scene_options);
        }
        UiAction::TransitionToScene { scene, transition } => {
            changed |= draw_ui_scene_picker(ui, "Target", scene, scene_options);
            changed |= draw_ui_transition_editor(ui, transition);
        }
        UiAction::StartGameplayTransition { transition } => {
            changed |= draw_ui_transition_editor(ui, transition);
        }
        UiAction::SetOption { option, delta } => {
            changed |= draw_ui_option_picker(ui, "Option", option, option_choices);
            changed |= drag_i32(ui, "Delta", delta, -65536, 65536);
        }
        UiAction::Game(id) => {
            let mut value = *id as i32;
            if drag_i32(ui, "Id", &mut value, 0, u16::MAX as i32) {
                *id = value.clamp(0, u16::MAX as i32) as u16;
                changed = true;
            }
        }
        UiAction::StartGameplay | UiAction::Back => {}
    }
    changed
}

fn draw_ui_transition_editor(ui: &mut egui::Ui, transition: &mut UiTransition) -> bool {
    let mut changed = false;
    let kinds = [
        UiTransitionKind::None,
        UiTransitionKind::Fade,
        UiTransitionKind::BlockDissolve,
        UiTransitionKind::GlitchBreak,
    ];
    egui::ComboBox::from_label("Transition")
        .selected_text(transition.kind.label())
        .show_ui(ui, |ui| {
            for kind in kinds {
                if ui
                    .selectable_label(transition.kind == kind, kind.label())
                    .clicked()
                    && transition.kind != kind
                {
                    transition.kind = kind;
                    changed = true;
                }
            }
        });
    changed |= drag_u16(ui, "Frames", &mut transition.frames, 0, 600);
    changed |= drag_u16(ui, "Seed", &mut transition.seed, 0, u16::MAX);
    let labels = ["R", "G", "B"];
    for (index, label) in labels.iter().enumerate() {
        let mut value = i32::from(transition.color[index]);
        if drag_i32(ui, label, &mut value, 0, 255) {
            transition.color[index] = value.clamp(0, 255) as u8;
            changed = true;
        }
    }
    changed
}

/// Dropdown that picks a [`SceneStateId`] from the project's screen states.
pub(crate) fn draw_scene_state_picker(
    ui: &mut egui::Ui,
    label: &str,
    current: &mut SceneStateId,
    options: &[(SceneStateId, String)],
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        let preview = options
            .iter()
            .find(|(id, _)| id == current)
            .map(|(_, name)| name.as_str())
            .unwrap_or("(none)");
        let mut selected = Some(*current);
        changed |= searchable_picker(
            ui,
            ui.id().with(("scene-state-picker", label)),
            &mut selected,
            preview,
            options,
            SearchablePickerConfig::required(),
        );
        if let Some(selected) = selected {
            *current = selected;
        }
    });
    changed
}

/// Dropdown that picks a [`UiSceneId`] from the project's UI scenes.
pub(crate) fn draw_ui_scene_picker(
    ui: &mut egui::Ui,
    label: &str,
    current: &mut UiSceneId,
    options: &[(UiSceneId, String)],
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        let preview = options
            .iter()
            .find(|(id, _)| id == current)
            .map(|(_, name)| name.as_str())
            .unwrap_or("(none)");
        let mut selected = Some(*current);
        changed |= searchable_picker(
            ui,
            ui.id().with(("ui-scene-picker", label)),
            &mut selected,
            preview,
            options,
            SearchablePickerConfig::required(),
        );
        if let Some(selected) = selected {
            *current = selected;
        }
    });
    changed
}

/// Dropdown that picks an [`OptionId`] from the project's options.
/// Shows "(none)" when the project has no options or the bound id is
/// unresolved.
pub(crate) fn draw_ui_option_picker(
    ui: &mut egui::Ui,
    label: &str,
    current: &mut OptionId,
    options: &[(OptionId, String)],
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        let preview = options
            .iter()
            .find(|(id, _)| id == current)
            .map(|(_, name)| name.as_str())
            .unwrap_or("(none)");
        let mut selected = Some(*current);
        changed |= searchable_picker(
            ui,
            ui.id().with(("ui-option-picker", label)),
            &mut selected,
            preview,
            options,
            SearchablePickerConfig::required(),
        );
        if let Some(selected) = selected {
            *current = selected;
        }
    });
    changed
}

/// Dropdown that picks an optional project option.
pub(crate) fn draw_optional_ui_option_picker(
    ui: &mut egui::Ui,
    label: &str,
    current: &mut Option<OptionId>,
    options: &[(OptionId, String)],
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        let preview = current
            .and_then(|current| {
                options
                    .iter()
                    .find(|(id, _)| *id == current)
                    .map(|(_, name)| name.as_str())
            })
            .unwrap_or("(none)");
        changed |= searchable_picker(
            ui,
            ui.id().with(("optional-ui-option-picker", label)),
            current,
            preview,
            options,
            SearchablePickerConfig::optional("(none)"),
        );
    });
    changed
}

pub(crate) fn draw_button_sfx_editor(
    ui: &mut egui::Ui,
    sfx: &mut UiSfxBindings,
    wav_options: &[String],
    project_root: &Path,
    preview_message: &mut Option<String>,
) -> bool {
    let mut changed = false;
    egui::CollapsingHeader::new(icons::label(icons::AUDIO_LINES, "SFX"))
        .default_open(false)
        .show(ui, |ui| {
            changed |= draw_ui_sfx_pool_editor(
                ui,
                "Focus",
                &mut sfx.focus,
                wav_options,
                project_root,
                preview_message,
            );
            changed |= draw_ui_sfx_pool_editor(
                ui,
                "Press",
                &mut sfx.activate,
                wav_options,
                project_root,
                preview_message,
            );
        });
    changed
}

pub(crate) fn draw_slider_sfx_editor(
    ui: &mut egui::Ui,
    sfx: &mut UiSfxBindings,
    wav_options: &[String],
    project_root: &Path,
    preview_message: &mut Option<String>,
) -> bool {
    let mut changed = false;
    egui::CollapsingHeader::new(icons::label(icons::AUDIO_LINES, "SFX"))
        .default_open(false)
        .show(ui, |ui| {
            changed |= draw_ui_sfx_pool_editor(
                ui,
                "Focus",
                &mut sfx.focus,
                wav_options,
                project_root,
                preview_message,
            );
            changed |= draw_ui_sfx_pool_editor(
                ui,
                "Nudge",
                &mut sfx.nudge,
                wav_options,
                project_root,
                preview_message,
            );
            changed |= draw_ui_sfx_pool_editor(
                ui,
                "Limit",
                &mut sfx.limit,
                wav_options,
                project_root,
                preview_message,
            );
        });
    changed
}

pub(crate) fn draw_ui_sfx_pool_editor(
    ui: &mut egui::Ui,
    label: &str,
    cues: &mut Vec<UiSfxCue>,
    wav_options: &[String],
    project_root: &Path,
    preview_message: &mut Option<String>,
) -> bool {
    let mut changed = false;
    ui.separator();
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).strong());
        if ui
            .small_button(icons::label(icons::PLUS, "Sound"))
            .clicked()
        {
            cues.push(UiSfxCue::default());
            changed = true;
        }
    });
    let mut remove = None;
    for (index, cue) in cues.iter_mut().enumerate() {
        ui.push_id((label, index), |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("#{}", index + 1));
                changed |= draw_sfx_wav_picker(ui, "WAV", &mut cue.wav_path, wav_options);
                if ui
                    .small_button(icons::label(icons::PLAY, "Preview"))
                    .clicked()
                {
                    *preview_message = Some(match preview_ui_sfx_cue(project_root, cue) {
                        Ok(()) => format!("Previewing {}", cue.wav_path),
                        Err(error) => error,
                    });
                }
                if ui
                    .small_button(icons::label(icons::TRASH, ""))
                    .on_hover_text("Remove sound")
                    .clicked()
                {
                    remove = Some(index);
                }
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Path").color(STUDIO_TEXT_WEAK));
                changed |= ui.text_edit_singleline(&mut cue.wav_path).changed();
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Volume").color(STUDIO_TEXT_WEAK));
                let mut volume = cue.volume.min(100) as i32;
                if ui
                    .add(egui::Slider::new(&mut volume, 0..=100).suffix("%"))
                    .changed()
                {
                    cue.volume = volume as u8;
                    changed = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Pitch").color(STUDIO_TEXT_WEAK));
                let mut pitch = (cue.pitch_q12.max(1) as f32) / 4096.0;
                if ui
                    .add(egui::Slider::new(&mut pitch, 0.25..=2.0).suffix("x"))
                    .changed()
                {
                    cue.pitch_q12 = ((pitch * 4096.0).round() as i32).clamp(1, 0x3FFF) as u16;
                    changed = true;
                }
            });
        });
    }
    if let Some(index) = remove {
        cues.remove(index);
        changed = true;
    }
    changed
}

pub(crate) fn draw_sfx_wav_picker(
    ui: &mut egui::Ui,
    label: &str,
    current: &mut String,
    options: &[String],
) -> bool {
    let picker_options = options
        .iter()
        .enumerate()
        .map(|(index, path)| (index, path.clone()))
        .collect::<Vec<_>>();
    let mut selected = options.iter().position(|path| path == current);
    let preview = if current.trim().is_empty() {
        "(none)"
    } else {
        current.as_str()
    };
    let changed = searchable_picker(
        ui,
        ui.id().with(("sfx-wav-picker", label)),
        &mut selected,
        preview,
        &picker_options,
        SearchablePickerConfig::optional("(none)")
            .with_width(150.0)
            .with_search_hint("Search WAV files…"),
    );
    if changed {
        *current = selected
            .and_then(|index| options.get(index))
            .cloned()
            .unwrap_or_default();
    }
    changed
}

pub(crate) fn preview_ui_sfx_cue(project_root: &Path, cue: &UiSfxCue) -> Result<(), String> {
    let trimmed = cue.wav_path.trim();
    if trimmed.is_empty() {
        return Err("Choose a WAV before previewing SFX".to_string());
    }
    let path = psxed_project::model_import::resolve_path(trimmed, Some(project_root));
    if !path.is_file() {
        return Err(format!("SFX preview source not found: {}", path.display()));
    }
    let volume = cue.volume.min(100).to_string();
    let pitch = (cue.pitch_q12.max(1) as f32 / 4096.0).clamp(0.25, 2.0);
    let path_arg = path.to_string_lossy().into_owned();
    let filter = format!("asetrate=44100*{pitch:.4},aresample=44100");
    let mut ffplay_args = vec![
        "-nodisp".to_string(),
        "-autoexit".to_string(),
        "-loglevel".to_string(),
        "quiet".to_string(),
        "-volume".to_string(),
        volume.clone(),
    ];
    if (pitch - 1.0).abs() > 0.001 {
        ffplay_args.push("-af".to_string());
        ffplay_args.push(filter);
    }
    ffplay_args.push(path_arg.clone());
    if spawn_audio_preview("ffplay", &ffplay_args).is_ok()
        || spawn_audio_preview("/opt/homebrew/bin/ffplay", &ffplay_args).is_ok()
    {
        return Ok(());
    }
    let afplay_args = vec![
        "-v".to_string(),
        format!("{:.3}", cue.volume.min(100) as f32 / 100.0),
        path_arg,
    ];
    spawn_audio_preview("afplay", &afplay_args)
        .map_err(|error| format!("Could not launch audio preview: {error}"))
}

pub(crate) fn spawn_audio_preview(program: &str, args: &[String]) -> std::io::Result<()> {
    std::process::Command::new(program)
        .args(args)
        .spawn()
        .map(|_| ())
}

pub(crate) fn draw_music_wav_picker(
    ui: &mut egui::Ui,
    label: &str,
    current: &mut String,
    options: &[String],
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        let picker_options = options
            .iter()
            .enumerate()
            .map(|(index, path)| (index, path.clone()))
            .collect::<Vec<_>>();
        let mut selected = options.iter().position(|path| path == current);
        let preview = if current.trim().is_empty() {
            "(none)"
        } else {
            current.as_str()
        };
        if searchable_picker(
            ui,
            ui.id().with(("music-wav-picker", label)),
            &mut selected,
            preview,
            &picker_options,
            SearchablePickerConfig::optional("(none)").with_search_hint("Search WAV files…"),
        ) {
            *current = selected
                .and_then(|index| options.get(index))
                .cloned()
                .unwrap_or_default();
            changed = true;
        }
    });
    ui.horizontal(|ui| {
        ui.label("Path");
        changed |= ui.text_edit_singleline(current).changed();
    });
    changed
}

pub(crate) fn collect_project_wav_options(project_dir: &Path) -> Vec<String> {
    let roots = [project_dir.join("assets"), project_dir.to_path_buf()];
    let mut seen = std::collections::BTreeSet::new();
    for root in roots {
        collect_wav_options_from_dir(project_dir, &root, &mut seen);
    }
    seen.into_iter().collect()
}

pub(crate) fn collect_wav_options_from_dir(
    project_dir: &Path,
    root: &Path,
    out: &mut std::collections::BTreeSet<String>,
) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let is_wav = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"));
            if !is_wav {
                continue;
            }
            let display = path
                .strip_prefix(project_dir)
                .unwrap_or(path.as_path())
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(display);
        }
    }
}

/// Editor for one [`OptionKind`]: a variant combo plus the per-variant
/// fields (bounds + step + default for `IntRange`, a default toggle
/// for `Bool`, and a default-index picker for `Enum` with its variant
/// labels editable as a newline-separated list).
pub(crate) fn draw_option_kind_editor(ui: &mut egui::Ui, kind: &mut OptionKind) -> bool {
    let mut changed = false;
    let variants = [
        OptionKind::IntRange {
            min: 0,
            max: 10,
            step: 1,
            default: 5,
        },
        OptionKind::Enum {
            variants: vec!["Off".to_string(), "On".to_string()],
            default: 0,
        },
        OptionKind::Bool { default: false },
    ];
    egui::ComboBox::from_id_salt(ui.id().with("option_kind"))
        .selected_text(kind.label())
        .show_ui(ui, |ui| {
            for candidate in &variants {
                let same = candidate.label() == kind.label();
                if ui.selectable_label(same, candidate.label()).clicked() && !same {
                    *kind = candidate.clone();
                    changed = true;
                }
            }
        });
    match kind {
        OptionKind::IntRange {
            min,
            max,
            step,
            default,
        } => {
            changed |= drag_i32(ui, "Min", min, i32::MIN / 2, i32::MAX / 2);
            changed |= drag_i32(ui, "Max", max, i32::MIN / 2, i32::MAX / 2);
            changed |= drag_i32(ui, "Step", step, 1, i32::MAX / 2);
            changed |= drag_i32(ui, "Default", default, *min, *max);
        }
        OptionKind::Enum { variants, default } => {
            let mut joined = variants.join("\n");
            ui.horizontal(|ui| {
                ui.label("Variants");
                if ui
                    .add(egui::TextEdit::multiline(&mut joined).desired_rows(2))
                    .changed()
                {
                    *variants = joined.lines().map(str::to_string).collect();
                    changed = true;
                }
            });
            let max_index = variants.len().saturating_sub(1);
            let mut value = (*default).min(max_index) as i32;
            if drag_i32(ui, "Default", &mut value, 0, max_index as i32) {
                *default = value.max(0) as usize;
                changed = true;
            }
        }
        OptionKind::Bool { default } => {
            changed |= ui.checkbox(default, "Default on").changed();
        }
    }
    changed
}

pub(crate) fn drag_u8(ui: &mut egui::Ui, label: &str, value: &mut u8, min: u8, max: u8) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        let mut v = *value as i64;
        if ui
            .add(egui::DragValue::new(&mut v).range(min as i64..=max as i64))
            .changed()
        {
            *value = v.clamp(min as i64, max as i64) as u8;
            changed = true;
        }
    });
    changed
}

pub(crate) fn collider_shape_editor(ui: &mut egui::Ui, shape: &mut ColliderShape) -> bool {
    let mut changed = false;
    let current = match shape {
        ColliderShape::Box { .. } => "Box",
        ColliderShape::Sphere { .. } => "Sphere",
        ColliderShape::Capsule { .. } => "Capsule",
    };
    egui::ComboBox::from_label("Shape")
        .selected_text(current)
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(matches!(shape, ColliderShape::Box { .. }), "Box")
                .clicked()
            {
                *shape = ColliderShape::Box {
                    half_extents: [256, 256, 256],
                };
                changed = true;
            }
            if ui
                .selectable_label(matches!(shape, ColliderShape::Sphere { .. }), "Sphere")
                .clicked()
            {
                *shape = ColliderShape::Sphere { radius: 256 };
                changed = true;
            }
            if ui
                .selectable_label(matches!(shape, ColliderShape::Capsule { .. }), "Capsule")
                .clicked()
            {
                *shape = ColliderShape::Capsule {
                    radius: 192,
                    height: 1024,
                };
                changed = true;
            }
        });
    match shape {
        ColliderShape::Box { half_extents } => {
            changed |= drag_u16(ui, "Half X", &mut half_extents[0], 0, 8192);
            changed |= drag_u16(ui, "Half Y", &mut half_extents[1], 0, 8192);
            changed |= drag_u16(ui, "Half Z", &mut half_extents[2], 0, 8192);
        }
        ColliderShape::Sphere { radius } => {
            changed |= drag_u16(ui, "Radius", radius, 0, 8192);
        }
        ColliderShape::Capsule { radius, height } => {
            changed |= drag_u16(ui, "Radius", radius, 0, 8192);
            changed |= drag_u16(ui, "Height", height, 0, 16384);
        }
    }
    changed
}

pub(crate) fn drag_i32(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut i32,
    min: i32,
    max: i32,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        if ui
            .add(egui::DragValue::new(value).range(min..=max))
            .changed()
        {
            *value = (*value).clamp(min, max);
            changed = true;
        }
    });
    changed
}

pub(crate) fn model_scale_axis_editor(ui: &mut egui::Ui, label: &str, value: &mut u16) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        let mut q8 = (*value).max(1) as i32;
        if ui
            .add(egui::DragValue::new(&mut q8).range(1..=4096).speed(16.0))
            .changed()
        {
            *value = q8.clamp(1, u16::MAX as i32) as u16;
            changed = true;
        }
        ui.label(
            RichText::new(format!("{:.3}x", *value as f32 / MODEL_SCALE_ONE_Q8 as f32))
                .color(STUDIO_TEXT_WEAK)
                .monospace(),
        );
    });
    changed
}

/// "13 · RightHand" when names are known for the joint, else
/// "Joint 13". Rig namespace prefixes (`mixamorig:RightHand`) are
/// stripped for display only.
pub(crate) fn joint_label(joint: u16, joint_names: Option<&[String]>) -> String {
    match joint_names
        .and_then(|names| names.get(joint as usize))
        .map(|name| name.rsplit(':').next().unwrap_or(name).trim())
        .filter(|name| !name.is_empty())
    {
        Some(name) => format!("{joint} · {name}"),
        None => format!("Joint {joint}"),
    }
}

pub(crate) fn attachment_socket_list_editor(
    ui: &mut egui::Ui,
    sockets: &mut Vec<psxed_project::AttachmentSocket>,
    joint_count: Option<u16>,
    joint_names: Option<&[String]>,
) -> bool {
    let mut changed = false;
    let mut remove: Option<usize> = None;
    let issues = attachment_socket_issue_counts(sockets, joint_count);
    if let Some(joint_count) = joint_count {
        ui.label(
            RichText::new(format!("Rig joints: {joint_count}"))
                .color(STUDIO_TEXT_WEAK)
                .small(),
        );
    }
    if issues.empty_names > 0 || issues.duplicate_names > 0 || issues.invalid_joints > 0 {
        ui.colored_label(
            Color32::from_rgb(220, 160, 80),
            format!(
                "{} empty names, {} duplicate names, {} invalid joints",
                issues.empty_names, issues.duplicate_names, issues.invalid_joints
            ),
        );
    }
    for (index, socket) in sockets.iter_mut().enumerate() {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("#{index}")).color(STUDIO_TEXT_WEAK));
                changed |= ui.text_edit_singleline(&mut socket.name).changed();
                if ui
                    .small_button(icons::label(icons::TRASH, "Remove"))
                    .on_hover_text("Remove socket")
                    .clicked()
                {
                    remove = Some(index);
                }
            });
            let max_joint = joint_count
                .map(|count| count.saturating_sub(1))
                .unwrap_or(u16::MAX);
            changed |= drag_u16(ui, "Joint", &mut socket.joint, 0, max_joint);
            if let Some(name) = joint_names
                .and_then(|names| names.get(socket.joint as usize))
                .filter(|name| !name.trim().is_empty())
            {
                ui.label(RichText::new(name.as_str()).small().color(STUDIO_TEXT_WEAK));
            }
            if let Some(count) = joint_count {
                if socket.joint >= count {
                    ui.colored_label(
                        Color32::from_rgb(220, 120, 100),
                        format!(
                            "Joint {} is outside this model's 0..{} range",
                            socket.joint, count
                        ),
                    );
                }
            }
            changed |= int_vec3_editor(ui, "Offset", &mut socket.translation, -32768, 32767, 8.0);
            changed |= q12_rotation_editor(ui, "Rotation", &mut socket.rotation_q12);
        });
        ui.add_space(4.0);
    }
    if let Some(index) = remove {
        sockets.remove(index);
        changed = true;
    }
    let has_right_hand = sockets
        .iter()
        .any(|socket| socket.name == "right_hand_grip");
    let has_left_hand = sockets.iter().any(|socket| socket.name == "left_hand_grip");
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !has_right_hand,
                egui::Button::new(icons::label(icons::PLUS, "Right hand")),
            )
            .clicked()
        {
            sockets.push(psxed_project::AttachmentSocket::right_hand_grip());
            changed = true;
        }
        if ui
            .add_enabled(
                !has_left_hand,
                egui::Button::new(icons::label(icons::PLUS, "Left hand")),
            )
            .clicked()
        {
            sockets.push(psxed_project::AttachmentSocket::left_hand_grip());
            changed = true;
        }
        if ui
            .button(icons::label(icons::PLUS, "Custom point"))
            .clicked()
        {
            let mut suffix = sockets.len() + 1;
            let name = loop {
                let candidate = format!("attachment_{suffix}");
                if sockets.iter().all(|socket| socket.name != candidate) {
                    break candidate;
                }
                suffix += 1;
            };
            sockets.push(psxed_project::AttachmentSocket {
                name,
                joint: 0,
                translation: [0, 0, 0],
                rotation_q12: [0, 0, 0],
            });
            changed = true;
        }
    });
    changed
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AttachmentSocketIssueCounts {
    pub(crate) empty_names: usize,
    pub(crate) duplicate_names: usize,
    pub(crate) invalid_joints: usize,
}

pub(crate) fn attachment_socket_issue_counts(
    sockets: &[psxed_project::AttachmentSocket],
    joint_count: Option<u16>,
) -> AttachmentSocketIssueCounts {
    let mut out = AttachmentSocketIssueCounts::default();
    let mut names = HashSet::new();
    for socket in sockets {
        let name = socket.name.trim();
        if name.is_empty() {
            out.empty_names += 1;
        } else if !names.insert(name.to_ascii_lowercase()) {
            out.duplicate_names += 1;
        }
        if joint_count.is_some_and(|count| socket.joint >= count) {
            out.invalid_joints += 1;
        }
    }
    out
}

pub(crate) fn draw_weapon_resource_editor(
    ui: &mut egui::Ui,
    weapon: &mut psxed_project::WeaponResource,
    model_options: &[(ResourceId, String, Vec<String>)],
    known_socket_names: &[String],
) -> bool {
    let mut changed = false;

    egui::CollapsingHeader::new(icons::label(icons::BOX, "Visual Model"))
        .default_open(true)
        .show(ui, |ui| {
            changed |= model_resource_picker(ui, "Model", &mut weapon.model, model_options);
            changed |= socket_name_picker(
                ui,
                "Character Socket",
                &mut weapon.default_character_socket,
                known_socket_names,
            );
        });

    egui::CollapsingHeader::new(icons::label(icons::WAYPOINT, "Grip"))
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Name");
                changed |= ui.text_edit_singleline(&mut weapon.grip.name).changed();
            });
            changed |= int_vec3_editor(
                ui,
                "Offset",
                &mut weapon.grip.translation,
                -32768,
                32767,
                8.0,
            );
            changed |= q12_rotation_editor(ui, "Rotation", &mut weapon.grip.rotation_q12);
        });

    egui::CollapsingHeader::new(icons::label(icons::SCAN, "Melee Arc"))
        .default_open(true)
        .show(ui, |ui| {
            ui.weak(
                "Gameplay hit volume: a flat arc swept in front of the wielder. \
                 Reach is engine units from the body origin; the half-angle opens \
                 to each side of the facing. Hitbox frame windows below gate WHEN \
                 the arc is live during the attack clip.",
            );
            changed |= drag_u16(ui, "Arc Reach", &mut weapon.arc_reach, 1, 8192);
            changed |= drag_u16(
                ui,
                "Arc Half-Angle (deg)",
                &mut weapon.arc_half_angle_degrees,
                1,
                170,
            );
            changed |= drag_u16(ui, "Damage", &mut weapon.damage, 1, 999);
            changed |= drag_u16(ui, "Poise Damage", &mut weapon.poise_damage, 0, 999);
        });

    egui::CollapsingHeader::new(icons::label(icons::SCAN, "Hitboxes"))
        .default_open(true)
        .show(ui, |ui| {
            ui.weak("Hit volumes are local to the weapon grip and use integer engine units.");
            changed |= weapon_hitbox_list_editor(ui, &mut weapon.hitboxes);
        });

    egui::CollapsingHeader::new(icons::label(icons::WAYPOINT, "Attachment Lab"))
        .default_open(true)
        .show(ui, |ui| {
            draw_weapon_attachment_lab(ui, weapon, known_socket_names);
        });

    changed
}

pub(crate) fn socket_name_picker(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    known_socket_names: &[String],
) -> bool {
    let mut changed = false;
    let picker_options = known_socket_names
        .iter()
        .enumerate()
        .map(|(index, name)| (index, name.clone()))
        .collect::<Vec<_>>();
    ui.horizontal(|ui| {
        ui.label(label);
        changed |= ui.text_edit_singleline(value).changed();
        if !known_socket_names.is_empty() {
            let mut selected = known_socket_names.iter().position(|name| name == value);
            if searchable_picker(
                ui,
                ui.id().with(("socket-name-picker", label)),
                &mut selected,
                "Known",
                &picker_options,
                SearchablePickerConfig::required()
                    .with_width(100.0)
                    .with_search_hint("Search sockets…"),
            ) {
                if let Some(name) = selected.and_then(|index| known_socket_names.get(index)) {
                    *value = name.clone();
                    changed = true;
                }
            }
        }
    });
    changed
}

pub(crate) fn draw_weapon_attachment_lab(
    ui: &mut egui::Ui,
    weapon: &psxed_project::WeaponResource,
    known_socket_names: &[String],
) {
    let summary = weapon_attachment_summary(weapon, known_socket_names);
    ui.horizontal_wrapped(|ui| {
        attachment_lab_metric(ui, "Hitboxes", summary.hitbox_count.to_string());
        attachment_lab_metric(ui, "Active window", summary.active_window_label);
        attachment_lab_metric(ui, "Max reach", format!("{} u", summary.max_reach));
    });
    for warning in summary.warnings {
        ui.colored_label(Color32::from_rgb(220, 160, 80), warning);
    }
}

pub(crate) fn attachment_lab_metric(ui: &mut egui::Ui, label: &str, value: String) {
    ui.group(|ui| {
        ui.label(RichText::new(label).color(STUDIO_TEXT_WEAK).small());
        ui.label(RichText::new(value).monospace());
    });
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WeaponAttachmentSummary {
    pub(crate) hitbox_count: usize,
    pub(crate) active_window_label: String,
    pub(crate) max_reach: i32,
    pub(crate) warnings: Vec<String>,
}

pub(crate) fn weapon_attachment_summary(
    weapon: &psxed_project::WeaponResource,
    known_socket_names: &[String],
) -> WeaponAttachmentSummary {
    let mut warnings = Vec::new();
    let socket_name = weapon.default_character_socket.trim();
    if socket_name.is_empty() {
        warnings.push("Default character socket is empty.".to_string());
    } else if !known_socket_names.is_empty()
        && !known_socket_names.iter().any(|name| name == socket_name)
    {
        warnings.push(format!(
            "No current model resource defines socket \"{socket_name}\"."
        ));
    }
    if weapon.grip.name.trim().is_empty() {
        warnings.push("Weapon grip name is empty.".to_string());
    }
    if weapon.model.is_none() {
        warnings.push("Weapon has no visual model assigned.".to_string());
    }
    if weapon.hitboxes.is_empty() {
        warnings.push("Weapon has no hitboxes.".to_string());
    }

    let active_start = weapon
        .hitboxes
        .iter()
        .map(|hitbox| hitbox.active_start_frame)
        .min();
    let active_end = weapon
        .hitboxes
        .iter()
        .map(|hitbox| hitbox.active_end_frame)
        .max();
    let active_window_label = match (active_start, active_end) {
        (Some(start), Some(end)) => format!("{start}..{end}"),
        _ => "none".to_string(),
    };
    let max_reach = weapon
        .hitboxes
        .iter()
        .map(|hitbox| weapon_hitbox_max_reach(&hitbox.shape))
        .max()
        .unwrap_or(0);

    WeaponAttachmentSummary {
        hitbox_count: weapon.hitboxes.len(),
        active_window_label,
        max_reach,
        warnings,
    }
}

pub(crate) fn weapon_hitbox_max_reach(shape: &psxed_project::WeaponHitShape) -> i32 {
    match shape {
        psxed_project::WeaponHitShape::Box {
            center,
            half_extents,
        } => center
            .iter()
            .zip(half_extents.iter())
            .map(|(c, h)| c.abs().saturating_add(*h as i32))
            .max()
            .unwrap_or(0),
        psxed_project::WeaponHitShape::Capsule { start, end, radius } => start
            .iter()
            .chain(end.iter())
            .map(|v| v.abs().saturating_add(*radius as i32))
            .max()
            .unwrap_or(0),
    }
}

pub(crate) fn model_resource_picker(
    ui: &mut egui::Ui,
    label: &str,
    current: &mut Option<ResourceId>,
    options: &[(ResourceId, String, Vec<String>)],
) -> bool {
    let mut changed = false;
    let picker_options = options
        .iter()
        .map(|(id, name, _)| (*id, name.clone()))
        .collect::<Vec<_>>();
    ui.horizontal(|ui| {
        ui.label(label);
        let preview = current
            .and_then(|id| {
                options
                    .iter()
                    .find(|(rid, _, _)| *rid == id)
                    .map(|(_, name, _)| name.as_str())
            })
            .unwrap_or("(none)");
        changed |= searchable_picker(
            ui,
            ui.id().with(("model-resource-picker", label)),
            current,
            preview,
            &picker_options,
            SearchablePickerConfig::optional("(none)"),
        );
    });
    if let Some(id) = *current {
        if !options.iter().any(|(rid, _, _)| *rid == id) {
            ui.colored_label(
                Color32::from_rgb(220, 120, 100),
                "Model resource is missing.",
            );
        }
    }
    changed
}

pub(crate) fn weapon_hitbox_list_editor(
    ui: &mut egui::Ui,
    hitboxes: &mut Vec<psxed_project::WeaponHitbox>,
) -> bool {
    let mut changed = false;
    let mut remove: Option<usize> = None;
    for (index, hitbox) in hitboxes.iter_mut().enumerate() {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("#{index}")).color(STUDIO_TEXT_WEAK));
                changed |= ui.text_edit_singleline(&mut hitbox.name).changed();
                if ui
                    .small_button(icons::label(icons::TRASH, ""))
                    .on_hover_text("Remove hitbox")
                    .clicked()
                {
                    remove = Some(index);
                }
            });
            changed |= drag_u16(
                ui,
                "Start frame",
                &mut hitbox.active_start_frame,
                0,
                u16::MAX,
            );
            changed |= drag_u16(ui, "End frame", &mut hitbox.active_end_frame, 0, u16::MAX);
            if hitbox.active_end_frame < hitbox.active_start_frame {
                hitbox.active_end_frame = hitbox.active_start_frame;
                changed = true;
            }
            changed |= weapon_hit_shape_editor(ui, &mut hitbox.shape);
        });
        ui.add_space(4.0);
    }
    if let Some(index) = remove {
        hitboxes.remove(index);
        changed = true;
    }
    if ui.button(icons::label(icons::PLUS, "Hitbox")).clicked() {
        hitboxes.push(psxed_project::WeaponHitbox::default());
        changed = true;
    }
    changed
}

pub(crate) fn weapon_hit_shape_editor(
    ui: &mut egui::Ui,
    shape: &mut psxed_project::WeaponHitShape,
) -> bool {
    let mut changed = false;
    let mut shape_kind = match shape {
        psxed_project::WeaponHitShape::Box { .. } => 0,
        psxed_project::WeaponHitShape::Capsule { .. } => 1,
    };
    ui.horizontal(|ui| {
        ui.label("Shape");
        egui::ComboBox::from_id_salt(ui.id().with("weapon-hit-shape"))
            .selected_text(if shape_kind == 0 { "Box" } else { "Capsule" })
            .show_ui(ui, |ui| {
                if ui.selectable_label(shape_kind == 0, "Box").clicked() {
                    shape_kind = 0;
                }
                if ui.selectable_label(shape_kind == 1, "Capsule").clicked() {
                    shape_kind = 1;
                }
            });
    });
    match (shape_kind, &mut *shape) {
        (0, psxed_project::WeaponHitShape::Capsule { .. }) => {
            *shape = psxed_project::WeaponHitShape::Box {
                center: [0, 256, 0],
                half_extents: [64, 256, 64],
            };
            changed = true;
        }
        (1, psxed_project::WeaponHitShape::Box { .. }) => {
            *shape = psxed_project::WeaponHitShape::Capsule {
                start: [0, 0, 0],
                end: [0, 512, 0],
                radius: 48,
            };
            changed = true;
        }
        _ => {}
    }

    match shape {
        psxed_project::WeaponHitShape::Box {
            center,
            half_extents,
        } => {
            changed |= int_vec3_editor(ui, "Center", center, -32768, 32767, 8.0);
            changed |= u16_vec3_editor(ui, "Half Extents", half_extents, 1, 8192);
        }
        psxed_project::WeaponHitShape::Capsule { start, end, radius } => {
            changed |= int_vec3_editor(ui, "Start", start, -32768, 32767, 8.0);
            changed |= int_vec3_editor(ui, "End", end, -32768, 32767, 8.0);
            changed |= drag_u16(ui, "Radius", radius, 1, 8192);
        }
    }
    changed
}

pub(crate) fn int_vec3_editor(
    ui: &mut egui::Ui,
    label: &str,
    values: &mut [i32; 3],
    min: i32,
    max: i32,
    speed: f64,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        for axis in 0..3 {
            let prefix = ["X ", "Y ", "Z "][axis];
            let response = ui.add(
                egui::DragValue::new(&mut values[axis])
                    .prefix(prefix)
                    .range(min..=max)
                    .speed(speed),
            );
            if response.changed() {
                values[axis] = values[axis].clamp(min, max);
                changed = true;
            }
        }
    });
    changed
}

pub(crate) fn u16_vec3_editor(
    ui: &mut egui::Ui,
    label: &str,
    values: &mut [u16; 3],
    min: u16,
    max: u16,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        for axis in 0..3 {
            let prefix = ["X ", "Y ", "Z "][axis];
            let mut value = values[axis] as i32;
            let response = ui.add(
                egui::DragValue::new(&mut value)
                    .prefix(prefix)
                    .range(min as i32..=max as i32),
            );
            if response.changed() {
                values[axis] = value.clamp(min as i32, max as i32) as u16;
                changed = true;
            }
        }
    });
    changed
}

pub(crate) fn q12_rotation_editor(ui: &mut egui::Ui, label: &str, values: &mut [i16; 3]) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        for axis in 0..3 {
            let prefix = ["X ", "Y ", "Z "][axis];
            let mut value = values[axis] as i32;
            let response = ui.add(
                egui::DragValue::new(&mut value)
                    .prefix(prefix)
                    .range(-4096..=4096)
                    .speed(16.0),
            );
            if response.changed() {
                values[axis] = value.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                changed = true;
            }
        }
    });
    changed
}
