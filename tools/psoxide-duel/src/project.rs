//! The fighters of a project, cooked by the project's own cook.
//!
//! The enemy is the project's training enemy as the cook writes its entity
//! record. Aletha becomes a fighter the same way: the training enemy's
//! placement is handed her Character and her controller settings, keeping
//! the enemy's AI persona (its `EnemyBehaviorSettings`: reaction, spacing,
//! windup and recovery, the numbers the disc's duel bot also borrows from the
//! enemy record), and the same cook writes her entity record. Her combat
//! stats are then set to exactly the record the disc gives her
//! (`CHARACTERS[player].combat`). So both fighters are entity records from
//! one cook path and differ only by data.

use std::path::Path;

use psx_level::{game_entity_flags, LevelGameEntityRecord};
use psxed_project::playtest::{build_package, OwnedLevelRecords, PlaytestPackage};
use psxed_project::{NodeKind, ProjectDocument, ResourceData};

use crate::fighter::FighterSpec;

/// What the arena needs from one project.
#[derive(Clone, Debug)]
pub struct ProjectFighters {
    /// Project directory name.
    pub project: String,
    /// The training enemy.
    pub enemy: FighterSpec,
    /// Aletha (the player character) as an entity fighter.
    pub aletha: FighterSpec,
    /// XZ distance between the player spawn and the training enemy: where
    /// the disc duel starts them.
    pub start_distance: i32,
    /// The swap cooldown every entity uses (the player character's, as on
    /// the disc: `set_stance_swap_delay(player swap cooldown)`).
    pub swap_cooldown_ticks: u16,
}

fn cook(project: &ProjectDocument, root: &Path) -> Result<PlaytestPackage, String> {
    let (package, report) = build_package(project, root);
    package.ok_or_else(|| format!("cook failed: {report:?}"))
}

/// The training enemy's record and its capsules in `package`.
fn training_fighter(
    package: &PlaytestPackage,
    name: &str,
) -> Result<(FighterSpec, LevelGameEntityRecord), String> {
    let owned = OwnedLevelRecords::build(package);
    let records = owned.records();
    let index = records
        .game_entities
        .iter()
        .position(|r| r.flags & game_entity_flags::TRAINING != 0)
        .ok_or("no TRAINING enemy in the project")?;
    let record = records.game_entities[index];
    let cooked = &package.game_entities[index];
    let first = usize::from(cooked.combat_capsule_first);
    let capsules = package
        .combat_capsules
        .get(first..first + usize::from(cooked.combat_capsule_count))
        .unwrap_or(&[]);
    let mut spec = FighterSpec::new(name, record, capsules);
    let clocks = entity_clocks(package, &spec.record);
    attach_clocks(&mut spec, clocks, capsules);
    spec.capsules = capsules.to_vec();
    let model = package
        .model_instances
        .get(usize::from(record.model_instance))
        .map(|instance| usize::from(instance.model));
    spec.timings = attack_timings(
        package,
        model,
        [
            (
                record.attack_clip,
                record.attack_speed_q8,
                record.attack_frame_range,
            ),
            (
                record.heavy_attack_clip,
                record.heavy_attack_speed_q8,
                record.heavy_attack_frame_range,
            ),
            (
                record.ranged_attack_clip,
                record.ranged_attack_speed_q8,
                record.ranged_attack_frame_range,
            ),
        ],
        &spec,
    );
    Ok((spec, record))
}

/// Give `spec` its contact windows and release ticks: each authored hitbox
/// and emitter frame through the clock of the attack clip that plays it.
fn attach_clocks(
    spec: &mut FighterSpec,
    clocks: [Option<ClipClock>; 3],
    capsules: &[psxed_project::playtest::PlaytestCombatCapsule],
) {
    spec.contact.clear();
    let heavy = psx_level::CharacterAnimationAction::HeavyAttack.to_index() as u8;
    let kind_of = |action: u8| {
        if action == heavy {
            1
        } else if action == spec.record.ranged_attack_action {
            2
        } else {
            0
        }
    };
    for capsule in capsules {
        let Some(clock) = clocks[kind_of(capsule.action)] else {
            continue;
        };
        if capsule.flags & psx_level::combat_capsule_flags::HITBOX != 0 {
            let window = (
                clock.tick(capsule.active_start_frame),
                clock.tick(capsule.active_end_frame),
            );
            match spec
                .contact
                .iter_mut()
                .find(|(a, _, _)| *a == capsule.action)
            {
                Some(entry) => {
                    entry.1 = entry.1.min(window.0);
                    entry.2 = entry.2.max(window.1);
                }
                None => spec.contact.push((capsule.action, window.0, window.1)),
            }
        }
    }
    for emitter in &mut spec.emitters {
        if let (Some(clock), Some(capsule)) = (
            clocks[kind_of(emitter.action)],
            capsules.get(usize::from(emitter.latch)),
        ) {
            emitter.release_tick = clock.tick(capsule.active_start_frame);
        }
    }
}

/// Cook `project_dir` twice (as authored, and with Aletha in the training
/// enemy's place) and return both fighters.
pub fn load(project_dir: &Path) -> Result<ProjectFighters, String> {
    let project = ProjectDocument::load_from_path(project_dir.join("project.ron"))
        .map_err(|e| format!("load {}: {e:?}", project_dir.display()))?;
    let package = cook(&project, project_dir)?;
    let player = package
        .player_controller
        .ok_or("project has no player controller")?;
    let player_character = package
        .characters
        .get(usize::from(player.character))
        .ok_or("player character not cooked")?;

    // Find both controllers in the authored scene.
    let scene = project.active_scene();
    let mut player_node = None;
    let mut enemy_node = None;
    for node in scene.nodes() {
        if let NodeKind::CharacterController {
            character,
            settings,
            player: is_player,
            loadout,
        } = &node.kind
        {
            if *is_player {
                player_node = Some((node.id, *character, *settings, *loadout));
            } else {
                let behaviour = settings.and_then(|s| s.enemy).or_else(|| {
                    character
                        .and_then(|id| project.resource(id))
                        .and_then(|r| match &r.data {
                            ResourceData::Character(c) => c.enemy_behavior,
                            _ => None,
                        })
                });
                if behaviour.is_some_and(|b| b.training) && enemy_node.is_none() {
                    enemy_node = Some((node.id, *character, behaviour));
                }
            }
        }
    }
    let (_, player_char_id, player_settings, player_loadout) =
        player_node.ok_or("no player Character Controller")?;
    let (enemy_id, enemy_char_id, behaviour) = enemy_node.ok_or("no training enemy controller")?;
    let character_name = |id: Option<psxed_project::ResourceId>| {
        id.and_then(|id| project.resource(id))
            .map_or_else(|| "?".to_owned(), |r| r.name.clone())
    };
    let (enemy, enemy_record) = training_fighter(&package, &character_name(enemy_char_id))?;
    let player_char_id = player_char_id.ok_or("player controller has no Character")?;
    let player_resource = match project.resource(player_char_id).map(|r| &r.data) {
        Some(ResourceData::Character(c)) => c,
        _ => return Err("player Character resource missing".into()),
    };

    // Aletha in the training enemy's place: her Character, her controller
    // settings, the enemy's AI persona.
    let mut swapped = project.clone();
    let mut settings = player_settings.unwrap_or_else(|| {
        psxed_project::CharacterControllerSettings::from_character(player_resource)
    });
    // Pin her stats before handing her an enemy persona (an unset `combat`
    // would otherwise read the persona's legacy numbers).
    settings.combat = Some(settings.combat_stats());
    settings.enemy = behaviour;
    if let Some(node) = swapped.active_scene_mut().node_mut(enemy_id) {
        node.kind = NodeKind::CharacterController {
            character: Some(player_char_id),
            settings: Some(settings),
            player: false,
            loadout: player_loadout,
        };
    }
    let swapped_package = cook(&swapped, project_dir)?;
    let (mut aletha, _) =
        training_fighter(&swapped_package, &character_name(Some(player_char_id)))?;
    // Exactly the stats the disc gives her.
    aletha.record.combat = player_character.combat;
    // Her attacks play her own action clips, at her speeds and frame
    // ranges, as the disc plays them (the entity cook resolves attack clips
    // from animation-set roles, which her set does not bind the way an
    // enemy set does). An entity attack is windup, then active, then
    // recovery; the windup and recovery are the persona's, so each kind's
    // active ticks are what is left of her clip (at least the legacy six).
    let player_clocks = {
        let clock = |action: psx_level::CharacterAnimationAction| {
            let i = action.to_index();
            ClipClock::of(
                &package,
                usize::from(player_character.model),
                player_character.action_clips[i],
                player_character.action_speeds[i],
                player_character.action_frame_ranges[i],
            )
        };
        [
            clock(psx_level::CharacterAnimationAction::LightAttack),
            clock(psx_level::CharacterAnimationAction::HeavyAttack),
            clock(psx_level::CharacterAnimationAction::RangedAttack),
        ]
    };
    aletha.record.ranged_attack_action =
        psx_level::CharacterAnimationAction::RangedAttack.to_index() as u8;
    let capsules = aletha.capsules.clone();
    attach_clocks(&mut aletha, player_clocks, &capsules);
    aletha.timings = {
        let clip = |action: psx_level::CharacterAnimationAction| {
            let i = action.to_index();
            (
                player_character.action_clips[i],
                player_character.action_speeds[i],
                player_character.action_frame_ranges[i],
            )
        };
        attack_timings(
            &package,
            Some(usize::from(player_character.model)),
            [
                clip(psx_level::CharacterAnimationAction::LightAttack),
                clip(psx_level::CharacterAnimationAction::HeavyAttack),
                clip(psx_level::CharacterAnimationAction::RangedAttack),
            ],
            &aletha,
        )
    };

    let spawn = player.spawn;
    let dx = i64::from(spawn.x) - i64::from(enemy_record.x);
    let dz = i64::from(spawn.z) - i64::from(enemy_record.z);
    let start_distance = ((dx * dx + dz * dz) as f64).sqrt() as i32;
    Ok(ProjectFighters {
        project: project_dir
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
        enemy,
        aletha,
        start_distance,
        swap_cooldown_ticks: player_character.stance_swap_cooldown_ticks,
    })
}

/// How `spec`'s light, heavy and ranged attacks run when played on `clips`
/// (clip, speed, frame range) of `model`: the guest's own rules
/// (`model_rendering::action_timing` over the authored contact frames), at
/// the disc's 60 Hz.
fn attack_timings(
    package: &PlaytestPackage,
    model: Option<usize>,
    clips: [(u16, u16, psx_level::CharacterActionFrameRange); 3],
    spec: &FighterSpec,
) -> [Option<psx_game_runtime::fighter_action::ActionTiming>; 3] {
    use psx_level::CharacterAnimationAction as Action;
    let actions = [
        Action::LightAttack,
        Action::HeavyAttack,
        Action::from_index(usize::from(spec.record.ranged_attack_action))
            .unwrap_or(Action::RangedAttack),
    ];
    let mut out = [None; 3];
    for (kind, (clip, speed_q8, range)) in clips.into_iter().enumerate() {
        out[kind] = (|| {
            let model = package.models.get(model?)?;
            let cooked = package
                .model_clips
                .get(usize::from(model.clip_first) + usize::from(clip))?;
            let asset = package.assets.get(cooked.animation_asset_index)?;
            // The guest's clip is 'static; a host tool leaks its one copy.
            let bytes: &'static [u8] = Box::leak(asset.bytes.clone().into_boxed_slice());
            let animation = psx_asset::Animation::from_bytes(bytes).ok()?;
            let contact = psx_game_runtime::combat::event_frames(
                spec.capsules
                    .iter()
                    .map(|c| (c.flags, c.action, c.active_start_frame, c.active_end_frame)),
                actions[kind],
            )?;
            psx_game_runtime::model_rendering::action_timing(
                animation,
                psx_engine::VideoHz::NTSC,
                speed_q8,
                range,
                contact,
            )
        })();
    }
    out
}

/// A clip's playback clock as the guest runs it: which tick into the clip a
/// source frame plays at, through the action's speed and frame range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipClock {
    step_q12: u32,
    start: u16,
    end: u16,
}

impl ClipClock {
    /// The clock of model clip `clip` (model-local) of model `model`.
    pub fn of(
        package: &PlaytestPackage,
        model: usize,
        clip: u16,
        speed_q8: u16,
        range: psx_level::CharacterActionFrameRange,
    ) -> Option<Self> {
        let model = package.models.get(model)?;
        let clip = package
            .model_clips
            .get(usize::from(model.clip_first) + usize::from(clip))?;
        let asset = package.assets.get(clip.animation_asset_index)?;
        let animation = psx_asset::Animation::from_bytes(&asset.bytes).ok()?;
        // The cook's and the guest's phase step (cooked_game_entity_attack_active_ticks).
        let base_step_q12 = (u32::from(animation.sample_rate_hz().max(1)) << 12) / 60;
        let step_q12 = ((u64::from(base_step_q12) * u64::from(speed_q8.max(1)))
            / u64::from(psx_level::CHARACTER_ACTION_SPEED_UNSCALED_Q8))
            as u32;
        let last = animation.frame_count().saturating_sub(2);
        let start = range.start.min(last);
        let end = if range.end == psx_level::CHARACTER_ACTION_FRAME_END_FULL {
            last
        } else {
            range.end.min(last)
        }
        .max(start);
        Some(Self {
            step_q12: step_q12.max(1),
            start,
            end,
        })
    }

    /// The tick into the clip at which source frame `frame` plays.
    pub fn tick(self, frame: u16) -> u16 {
        let relative = frame.clamp(self.start, self.end) - self.start;
        let phase = u32::from(relative) << 12;
        (phase.saturating_add(self.step_q12 - 1) / self.step_q12).min(u32::from(u16::MAX)) as u16
    }

    /// The clip's length in ticks.
    pub fn length(self) -> u16 {
        self.tick(self.end)
    }
}

/// The clocks of an entity record's three attack clips (light, heavy,
/// ranged), on its model.
pub fn entity_clocks(
    package: &PlaytestPackage,
    record: &LevelGameEntityRecord,
) -> [Option<ClipClock>; 3] {
    let model = package
        .model_instances
        .get(usize::from(record.model_instance))
        .map(|instance| usize::from(instance.model));
    let clock = |clip: u16, speed: u16, range| {
        model.and_then(|m| ClipClock::of(package, m, clip, speed, range))
    };
    [
        clock(
            record.attack_clip,
            record.attack_speed_q8,
            record.attack_frame_range,
        ),
        clock(
            record.heavy_attack_clip,
            record.heavy_attack_speed_q8,
            record.heavy_attack_frame_range,
        ),
        clock(
            record.ranged_attack_clip,
            record.ranged_attack_speed_q8,
            record.ranged_attack_frame_range,
        ),
    ]
}
