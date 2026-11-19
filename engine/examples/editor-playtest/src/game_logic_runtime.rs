//! Phase-3 gameplay-layer glue (docs/game-runtime-plan.md): the
//! example-side movement backend and effect dispatch the runtime
//! crate's `GameEntities`/`LogicRuntime` leave to the owning game.
//!
//! - [`SceneEntityMover`] is the world every enemy moves through: its motor
//!   (the player's motor) runs the same collision solve as the player's
//!   (`WorldRuntime::update_motor`) under the same game rules. A grid room
//!   whose collision is not resident holds the body still (the entity keeps
//!   thinking; it walks again when the room streams in).
//! - Effect dispatch drains `LogicRuntime::take_fired` marks into the
//!   example's presentation: DOOR records toggle their linked box
//!   prop (draw + collision), MESSAGE records open the interactable
//!   message overlay, CHECKPOINT records update the in-memory
//!   checkpoint -- the same UI paths the legacy interactable flow
//!   used, now driven off LOGIC records (the cook pairs them 1:1).
//! - Melee resolution (the combat slice) is one path for every fighter
//!   ([`Playtest::swing_fighters`]): the swing's authored hitboxes on the
//!   exact pose the body and weapon draw, against every hostile fighter's
//!   authored hurtboxes, latched once per strike window per defender.

use super::*;
use psx_engine::CharacterMotorFrame;
use psx_game_runtime::combat::{self, ActiveStrike};
use psx_game_runtime::combatant::stance_scaled;

use crate::playtest_runtime::{fighter_motor_rules, hazard_damage, liquid_motor_rule};
use psx_game_runtime::destructibles::{DamageChannel, DamageOutcome};
use psx_game_runtime::entities::{
    DeferredGameEntityAttack, GameEntityState, MeleeArcStats, PredictedContact, Tell,
};
use psx_game_runtime::model_rendering as mr;
use psx_game_runtime::projectiles::{
    CombatTeam, ProjectileImpactKind, ProjectileImpacts, ProjectileSpawn, ProjectileTarget,
    ProjectileVisualStyle, ProjectileWorldTrace, ProjectileWorldTracer, NO_PROJECTILE_OWNER,
};

const PLAYER_PROJECTILE_TARGET: u16 = u16::MAX - 1;

/// Cold blue charge/bolt to distinguish enemy fire from Aletha's jade cannon.
pub(super) fn enemy_projectile_visual(mut visual: ProjectileVisualStyle) -> ProjectileVisualStyle {
    visual.crystal = true;
    visual.core_rgb = [224, 244, 255];
    visual.glow_rgb = [80, 144, 255];
    visual.impact_rgb = [152, 208, 255];
    visual.glow_scale_q8 = 512;
    visual.length_ticks = 3;
    visual.trail_segments = 4;
    visual
}

fn weapon_swing_sfx(action: psx_level::CharacterAnimationAction) -> Option<LevelGameplaySfxEvent> {
    use psx_level::CharacterAnimationAction as Action;
    match action {
        Action::LightAttack | Action::VertLightAttack | Action::AltLightAttack => {
            Some(LevelGameplaySfxEvent::LightWeaponSwing)
        }
        Action::HeavyAttack
        | Action::VertHeavyAttack
        | Action::AltHeavyAttack
        | Action::ComboAttack
        | Action::VertComboAttack
        | Action::AltComboAttack => Some(LevelGameplaySfxEvent::HeavyWeaponSwing),
        _ => None,
    }
}

/// Combat-music gate: engaged on the first tick any enemy is hostile, released
/// after three quiet seconds so a Custodian flickering at the edge of its
/// aggro radius does not pump the track.
#[derive(Copy, Clone, Default)]
pub(super) struct CombatMusicState {
    pub(super) engaged: bool,
    quiet_ticks: u16,
}

impl CombatMusicState {
    const QUIET_TICKS: u16 = 180;

    pub(super) fn tick(&mut self, hostile: bool) -> Option<bool> {
        if hostile {
            self.quiet_ticks = 0;
            if !self.engaged {
                self.engaged = true;
                return Some(true);
            }
        } else if self.engaged {
            self.quiet_ticks = self.quiet_ticks.saturating_add(1);
            if self.quiet_ticks >= Self::QUIET_TICKS {
                self.engaged = false;
                self.quiet_ticks = 0;
                return Some(false);
            }
        }
        None
    }
}

struct SceneProjectileWorldTracer<'a> {
    world_rt: Option<&'a mut WorldRuntime>,
    destructibles: &'a RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
    prop_blockers: &'a [CharacterCollisionAabb],
    valid: bool,
}

impl ProjectileWorldTracer for SceneProjectileWorldTracer<'_> {
    fn trace_projectile(
        &mut self,
        _room: RoomIndex,
        start: [i32; 3],
        end: [i32; 3],
        _radius: u16,
    ) -> ProjectileWorldTrace {
        if !self.valid {
            return ProjectileWorldTrace::Failed;
        }
        let Some(world_rt) = self.world_rt.as_deref_mut() else {
            // No world is loaded, so there is nothing for a projectile to hit.
            return ProjectileWorldTrace::Clear { end };
        };
        match world_rt.trace_point_segment(
            RoomPoint::new(start[0], start[1], start[2]),
            RoomPoint::new(end[0], end[1], end[2]),
            self.prop_blockers,
            self.destructibles,
        ) {
            Ok(trace) if trace.hit() => ProjectileWorldTrace::Hit {
                end: [trace.end.x, trace.end.y, trace.end.z],
            },
            Ok(trace) => ProjectileWorldTrace::Clear {
                end: [trace.end.x, trace.end.y, trace.end.z],
            },
            Err(_) => ProjectileWorldTrace::Failed,
        }
    }
}

/// Melee occlusion segments trace at collision scale, not model scale: the
/// cooked player hull is 56 units tall while character models render far
/// larger, and the fixture door spans only the lower world units of its
/// doorway. Half the player hull height keeps the segment inside anything
/// that blocks movement.
// ponytail: fixed 28-unit eye lift; derive from the cooked hull if
// multi-floor melee occlusion ever matters.
const MELEE_OCCLUSION_EYE_LIFT: i32 = 28;

fn melee_eye_point(position: [i32; 3]) -> RoomPoint {
    RoomPoint::new(
        position[0],
        position[1].saturating_add(MELEE_OCCLUSION_EYE_LIFT),
        position[2],
    )
}

/// Where a fighter's pose snapshot of this tick lives: read by reference
/// where it is used ([`Playtest::actor_pose`]), never copied.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PoseAt {
    /// Her snapshot this tick.
    Player,
    /// Her snapshot one tick earlier.
    PreviousPlayer,
    /// Model instance `n`'s snapshot this tick.
    Instance(u16),
    /// Model instance `n`'s snapshot one tick earlier.
    PreviousInstance(u16),
}

/// One fighter's swing on this tick, frozen before any attack resolves.
#[derive(Clone, Copy)]
struct Swing {
    /// The swinging slot (the actor slot for the player's character).
    attacker: usize,
    capsules: &'static [psx_level::CombatCapsuleRecord],
    action: CharacterAnimationAction,
    pose: PoseAt,
    /// The attacker's pose one tick earlier on the same clip.
    previous: Option<PoseAt>,
    /// Root position, the start of the occlusion segment.
    position: [i32; 3],
    room: RoomIndex,
    /// The attacker's stance: the side every strike of the swing lands on.
    channel: VitalityChannelId,
    heavy: bool,
    /// The swing is a crit (it started with a crit armed).
    crit: bool,
}

/// One fighter as a melee defender this tick.
#[derive(Clone, Copy)]
struct SwingTarget {
    capsules: &'static [psx_level::CombatCapsuleRecord],
    pose: PoseAt,
    position: [i32; 3],
    room: RoomIndex,
}

/// Movement backend for game entities. It composes other entities' pre-tick
/// bodies and the player, plus active-room and prop collision.
pub(super) struct SceneEntityMover<'a> {
    pub(super) world_rt: Option<&'a mut WorldRuntime>,
    pub(super) destructibles: &'a RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
    pub(super) box_props: &'a RuntimeBoxProps,
    pub(super) models: &'a [Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    /// Pre-tick entity positions (entities move one at a time inside
    /// the tick; the snapshot keeps blocker order deterministic).
    pub(super) entity_positions: &'a [[i32; 3]],
    /// Pre-tick dead flags: corpses stop blocking other movers.
    pub(super) entity_dead: &'a [bool],
    pub(super) player: RoomPoint,
    pub(super) player_radius: i32,
    pub(super) player_height: i32,
    /// Each entity's streamed room (a search hint into the resident rooms):
    /// every fighter is in a room of its own, not the player's.
    pub(super) fighter_rooms: &'a mut [u16],
}

impl SceneEntityMover<'_> {
    /// What blocks entity `entity` in `room`: every other fighter's body
    /// (the other entities at their pre-tick positions, the player) and
    /// the props. None when the cooked prop tables do not fit, which
    /// freezes the body rather than letting it through a dropped blocker
    /// (the player's rule too).
    fn blockers(
        &self,
        entity: usize,
        room: RoomIndex,
        cylinders: &mut psx_engine::FixedScratch<
            CharacterCollisionCylinder,
            MAX_COLLISION_CYLINDERS,
        >,
        aabbs: &mut psx_engine::FixedScratch<CharacterCollisionAabb, MAX_STATIC_PROP_AABB_BLOCKERS>,
    ) -> Option<()> {
        let own_instance = GAME_ENTITIES
            .get(entity)
            .map(|record| record.model_instance)
            .unwrap_or(psx_level::GAME_ENTITY_MODEL_INSTANCE_NONE);
        for (index, inst) in MODEL_INSTANCES.iter().enumerate() {
            if inst.room != room {
                continue;
            }
            let inst_index = index.min(u16::MAX as usize) as u16;
            if inst_index == own_instance {
                continue;
            }
            let Some(model) = self.models.get(inst.model.to_usize()).copied().flatten() else {
                continue;
            };
            let body_radius = i32::from(model.collision_radius).max(1);
            let body_height = (model.world_height as i32).max(1);
            let center = match game_entity_for_instance(inst_index) {
                Some(other) => {
                    let other = other.min(MAX_GAME_ENTITIES - 1);
                    // Dead entities stop blocking (souls corpses).
                    let Some((&dead, &live)) = self
                        .entity_dead
                        .get(other)
                        .zip(self.entity_positions.get(other))
                    else {
                        continue;
                    };
                    if dead {
                        continue;
                    }
                    RoomPoint::new(live[0], live[1], live[2])
                }
                None => RoomPoint::new(inst.x, inst.y, inst.z),
            };
            if !cylinders.try_push(CharacterCollisionCylinder::new(
                center,
                body_radius,
                body_height,
            )) {
                break;
            }
        }
        if self.player_radius > 0 {
            cylinders.try_push(CharacterCollisionCylinder::new(
                self.player,
                self.player_radius,
                self.player_height.max(1),
            ));
        }
        psx_game_runtime::cylinder_props::collect_cylinder_prop_collision_blockers_into(
            CYLINDER_PROPS.as_slice(),
            room,
            cylinders,
        );
        self.box_props.collect_collision_blockers_checked_into(
            BOX_PROPS.as_slice(),
            room,
            aabbs,
        )?;
        psx_game_runtime::arch_props::collect_arch_prop_collision_blockers_checked_into(
            ARCH_PROPS.as_slice(),
            ARCH_PROP_COLLISIONS.as_slice(),
            room,
            aabbs,
        )?;
        psx_game_runtime::image_props::collect_image_prop_collision_blockers_checked_into_filtered(
            IMAGE_PROPS.as_slice(),
            room,
            aabbs,
            |index| {
                typed_world_object_active(
                    WORLD_OBJECTS.as_slice(),
                    self.destructibles,
                    psx_level::world_object_kind::IMAGE_PROP,
                    index,
                )
            },
        )?;
        Some(())
    }
}

impl psx_game_runtime::entities::FighterWorld for SceneEntityMover<'_> {
    /// The entity's motor through the same collision solve the player's
    /// motor runs ([`WorldRuntime::update_motor`]), under the same game
    /// rules ([`fighter_motor_rules`]).
    fn move_body(
        &mut self,
        entity: usize,
        room: RoomIndex,
        motor: &mut CharacterMotorState,
        input: CharacterMotorInput,
        config: CharacterMotorConfig,
        delta: u16,
    ) -> CharacterMotorFrame {
        let mut cylinders =
            psx_engine::FixedScratch::<CharacterCollisionCylinder, MAX_COLLISION_CYLINDERS>::new();
        let mut aabbs =
            psx_engine::FixedScratch::<CharacterCollisionAabb, MAX_STATIC_PROP_AABB_BLOCKERS>::new(
            );
        let gathered = self.blockers(entity, room, &mut cylinders, &mut aabbs);
        // A fighter whose room is not resident holds still: it walks again
        // when its room streams back in.
        let position = motor.position();
        let resident = |world_rt: &WorldRuntime, rooms: &mut [u16]| {
            rooms.get_mut(entity).is_none_or(|hint| {
                world_rt
                    .fighter_room(hint, [position.x, position.y, position.z])
                    .is_some()
            })
        };
        match (gathered, self.world_rt.as_deref_mut()) {
            (Some(()), Some(world_rt)) if !resident(world_rt, self.fighter_rooms) => {
                motor.suspended_frame(None)
            }
            (Some(()), Some(world_rt)) => {
                // A level with no hazard and no liquid cooks the sample out.
                let in_liquid = crate::generated::ANY_HAZARD
                    && world_rt
                        .player_contents(position, config.height)
                        .is_some_and(|sample| sample.water_level > 0);
                world_rt
                .update_motor(
                    motor,
                    input,
                    liquid_motor_rule(fighter_motor_rules(config, room, true), in_liquid),
                    delta,
                    self.destructibles,
                    cylinders.as_slice(),
                    aabbs.as_slice(),
                )
                .expect("grid entity trace failed")
            }
            // No world cooked yet (the placeholder manifest), or a prop
            // table that does not fit: the body holds still.
            _ => motor.suspended_frame(None),
        }
    }

    /// The knockback push: the same collision-checked body step as the
    /// player's hazard push.
    fn push_body(
        &mut self,
        entity: usize,
        room: RoomIndex,
        position: [i32; 3],
        dx: i32,
        dz: i32,
        radius: i32,
        height: i32,
    ) -> [i32; 3] {
        let mut cylinders =
            psx_engine::FixedScratch::<CharacterCollisionCylinder, MAX_COLLISION_CYLINDERS>::new();
        let mut aabbs =
            psx_engine::FixedScratch::<CharacterCollisionAabb, MAX_STATIC_PROP_AABB_BLOCKERS>::new(
            );
        let gathered = self.blockers(entity, room, &mut cylinders, &mut aabbs);
        let (Some(()), Some(world_rt)) = (gathered, self.world_rt.as_deref_mut()) else {
            return position;
        };
        let step = world_rt
            .commit_body_step(
                RoomPoint::new(position[0], position[1], position[2]),
                dx,
                dz,
                radius,
                height,
                self.destructibles,
                cylinders.as_slice(),
                aabbs.as_slice(),
            )
            .expect("grid entity trace failed");
        [step.position.x, step.position.y, step.position.z]
    }

    /// The game's speed knob, the player's: every character runs it.
    fn speed_scale(&self) -> (i32, i32) {
        (PLAYER_SPEED_SCALE_NUM, PLAYER_SPEED_SCALE_DEN)
    }

    fn line_of_sight(&mut self, room: RoomIndex, from: [i32; 3], to: [i32; 3]) -> bool {
        let mut aabbs =
            psx_engine::FixedScratch::<CharacterCollisionAabb, MAX_STATIC_PROP_AABB_BLOCKERS>::new(
            );
        let Some(_) = self.box_props.collect_collision_blockers_checked_into(
            BOX_PROPS.as_slice(),
            room,
            &mut aabbs,
        ) else {
            return false;
        };
        let Some(_) =
            psx_game_runtime::arch_props::collect_arch_prop_collision_blockers_checked_into(
                ARCH_PROPS.as_slice(),
                ARCH_PROP_COLLISIONS.as_slice(),
                room,
                &mut aabbs,
            )
        else {
            return false;
        };
        let Some(_) =
            psx_game_runtime::image_props::collect_image_prop_collision_blockers_checked_into_filtered(
                IMAGE_PROPS.as_slice(),
                room,
                &mut aabbs,
                |index| {
                    typed_world_object_active(
                        WORLD_OBJECTS.as_slice(),
                        self.destructibles,
                        psx_level::world_object_kind::IMAGE_PROP,
                        index,
                    )
                },
            )
        else {
            return false;
        };
        let Some(world_rt) = self.world_rt.as_deref_mut() else {
            return true;
        };
        world_rt
            .trace_point_segment(
                RoomPoint::new(from[0], from[1], from[2]),
                RoomPoint::new(to[0], to[1], to[2]),
                aabbs.as_slice(),
                self.destructibles,
            )
            .is_ok_and(|trace| !trace.hit())
    }
}

/// The game entity owning cooked model instance `instance`, if any.
pub(super) fn game_entity_for_instance(instance: u16) -> Option<usize> {
    if instance == psx_level::GAME_ENTITY_MODEL_INSTANCE_NONE {
        return None;
    }
    GAME_ENTITIES
        .iter()
        .position(|record| record.model_instance == instance)
}

/// Who fired a volley. Energy and the release latches still live in two
/// tables, the player's [`Combatant`] and the entity table.
#[derive(Clone, Copy)]
pub(super) enum Shooter {
    /// The player character.
    Player,
    /// An entity's ranged attack token.
    Entity(DeferredGameEntityAttack),
}

/// One shooter's authored projectile emitters for this tick. Today's
/// per-shooter differences are fields here, not branches in the release.
#[derive(Clone, Copy)]
pub(super) struct Volley {
    pub(super) shooter: Shooter,
    pub(super) capsules: &'static [psx_level::CombatCapsuleRecord],
    pub(super) action: psx_level::CharacterAnimationAction,
    pub(super) pose: PoseAt,
    /// Frame of the event clock the release windows are read at.
    pub(super) frame: u16,
    /// Emitters already consumed this attack.
    pub(super) released: u16,
    /// Aim point every bolt flies toward.
    pub(super) target: [i32; 3],
    pub(super) room: RoomIndex,
    pub(super) team: CombatTeam,
    pub(super) owner: u16,
    /// One channel for every bolt; `None` keeps each emitter's own.
    pub(super) channel: Option<psx_game_runtime::projectiles::ProjectileDamageChannel>,
    /// The shooter's outgoing-damage boons.
    pub(super) modifiers: VitalityModifiers,
    /// Bolt look derived from the emitter's authored style.
    pub(super) visual: fn(ProjectileVisualStyle) -> ProjectileVisualStyle,
    /// A muzzle this point cannot see is consumed without firing.
    pub(super) sight_from: Option<RoomPoint>,
    /// Keep the sub-unit velocity remainder of the aim.
    pub(super) precise: bool,
}

/// The player as a defender for one tick, frozen before any attack resolves.
#[derive(Clone, Copy)]
struct PlayerDefence {
    position: [i32; 3],
    /// The same body-radius source the entity tick input uses.
    radius: i32,
    height: i32,
    /// A fighter's hit passes through her: i-frames past the parry window.
    passes_through: bool,
    /// A fighter's hit is parried: the parry window of her swap.
    parries: bool,
    capsules: &'static [psx_level::CombatCapsuleRecord],
}

/// The table slot of the fighter `id` names: the player's character in the
/// actor slot, an entity in its own; none for the world.
fn fighter_slot(id: CombatantId) -> Option<usize> {
    if id == CombatantId::WORLD {
        None
    } else if id == CombatantId::PLAYER {
        Some(RuntimeGameEntities::ACTOR_SLOT)
    } else {
        Some(usize::from(id.0))
    }
}

/// The vitality channel (stance) a projectile's damage channel belongs to.
fn projectile_vitality_channel(
    channel: psx_game_runtime::projectiles::ProjectileDamageChannel,
) -> VitalityChannelId {
    match channel {
        psx_game_runtime::projectiles::ProjectileDamageChannel::Horizon => VitalityChannelId::One,
        psx_game_runtime::projectiles::ProjectileDamageChannel::Zenith => VitalityChannelId::Two,
    }
}

/// The bolt of a crit shot: the crit gold over its own colours, and a wider
/// halo, so it reads in flight and its impact can be told apart.
fn crit_projectile_visual(mut visual: ProjectileVisualStyle) -> ProjectileVisualStyle {
    visual.glow_rgb = CRIT_BOLT_RGB;
    visual.impact_rgb = CRIT_BOLT_RGB;
    visual.glow_scale_q8 = visual.glow_scale_q8.saturating_mul(2);
    visual
}

/// Glow and impact colour of a crit shot.
pub(super) const CRIT_BOLT_RGB: [u8; 3] = [247, 198, 99];

impl Playtest {
    /// Resolve every attack of this tick after the player and instance pose
    /// snapshots have been frozen, always in this order: the player's
    /// projectile release, every enemy attack token (a projectile release or
    /// a melee contact on the player), the projectile tick, the hits the
    /// player took, and last the player's own swing on the entities. The
    /// order is load-bearing: a bolt that staggers an enemy this tick lands
    /// before the player's blade reaches it, and a hit reaction cancels the
    /// player's swing before it can connect.
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn resolve_combat(&mut self, ctx: &Ctx) {
        self.combat_projectile_impacts.tick();
        self.player_flash.tick();
        self.game_entities.tick_flashes();
        self.release_player_projectile(ctx);
        if !self.deferred_enemy_attacks.is_empty() || !self.combat_projectiles.is_empty() {
            self.resolve_attacks_on_player(ctx);
        }
        self.resolve_player_melee(ctx);
    }

    /// The enemy attack tokens and every live projectile against this tick's
    /// frozen player; each hit lands as it connects.
    fn resolve_attacks_on_player(&mut self, ctx: &Ctx) {
        let player = self.player_defence(ctx);
        let mut prop_blockers =
            psx_engine::FixedScratch::<CharacterCollisionAabb, MAX_STATIC_PROP_AABB_BLOCKERS>::new(
            );
        let prop_blockers_valid = self.world_rt.is_none()
            || self
                .collect_static_prop_aabb_blockers_checked_into(&mut prop_blockers)
                .is_some();
        self.resolve_enemy_attacks(ctx, &player, prop_blockers.as_slice(), prop_blockers_valid);
        let mut impacts = ProjectileImpacts::<MAX_PROJECTILE_IMPACTS>::new();
        self.tick_combat_projectiles(
            ctx,
            &player,
            prop_blockers.as_slice(),
            prop_blockers_valid,
            &mut impacts,
        );
    }

    fn player_defence(&self, ctx: &Ctx) -> PlayerDefence {
        let defender = self.player_defender(ctx);
        let player = self.game_entities.actor_motor().position();
        let (radius, height) = match &self.character {
            Some(character) => (character.radius, character.height),
            None if self.world_rt.is_some() => (WORLD_PLAYER_RADIUS, WORLD_PLAYER_HEIGHT),
            None => (0, 0),
        };
        PlayerDefence {
            position: [player.x, player.y, player.z],
            radius,
            height,
            passes_through: defender.passes_through(),
            parries: defender.parries(),
            capsules: self.player_defence_capsules(),
        }
    }

    /// Whether nothing in the world blocks a melee contact between two
    /// actors' eye points. Always clear without a world runtime.
    fn melee_line_clear(
        &mut self,
        from: [i32; 3],
        to: [i32; 3],
        prop_blockers: &[CharacterCollisionAabb],
    ) -> bool {
        match self.world_rt.as_mut() {
            Some(world_rt) => world_rt.melee_segment_clear(
                melee_eye_point(from),
                melee_eye_point(to),
                prop_blockers,
                &self.destructibles,
            ),
            None => true,
        }
    }

    /// Every deferred enemy attack token in order: a ranged token releases
    /// its authored projectiles, a melee token tests contact on the player.
    /// Authored HITBOX/HURTBOX geometry is authoritative whenever both sides
    /// provide it. The legacy entity arc is used only when either side has
    /// no authored role at all; inactive authored frames, invalid authored
    /// joints, and authored geometric misses stay misses.
    fn resolve_enemy_attacks(
        &mut self,
        ctx: &Ctx,
        player: &PlayerDefence,
        prop_blockers: &[CharacterCollisionAabb],
        prop_blockers_valid: bool,
    ) {
        let mut attack_index = 0usize;
        while attack_index < self.deferred_enemy_attacks.len() {
            let Some(attack) = self.deferred_enemy_attacks.get(attack_index) else {
                break;
            };
            attack_index += 1;
            if attack.room() != self.room_index
                || !self.game_entities.deferred_attack_can_connect(attack)
            {
                continue;
            }
            let Some(entity) = GAME_ENTITIES.get(attack.entity()) else {
                continue;
            };
            let capsule_body = self.game_entities.body(entity);
            let first = capsule_body.combat_capsule_first.to_usize();
            let end = first.saturating_add(usize::from(capsule_body.combat_capsule_count));
            let attacker_capsules = COMBAT_CAPSULES.get(first..end).unwrap_or(&[]);
            let instance = entity.model_instance;
            if attack.is_ranged() {
                // A ranged attack only releases its authored emitters,
                // including frames outside its release window and pool-full
                // retries.
                self.release_enemy_projectiles(ctx, attack, attacker_capsules, instance);
                continue;
            }
            let Some(now) = self.instance_pose(instance) else {
                continue;
            };
            let previous = self
                .previous_instance_actor_poses
                .get(usize::from(instance))
                .and_then(Option::as_ref)
                .is_some_and(|old| old.clip_local() == now.clip_local())
                .then_some(PoseAt::PreviousInstance(instance));
            let index = attack.entity();
            let swing = Swing {
                attacker: index,
                capsules: attacker_capsules,
                action: attack.action(),
                pose: PoseAt::Instance(instance),
                previous,
                position: attack.position(),
                room: attack.room(),
                channel: self.game_entities.stance(index),
                heavy: self.game_entities.attack_kind(index) == 1,
                crit: self.game_entities.attack_is_crit(index),
            };
            if !prop_blockers_valid {
                continue;
            }
            let stats = self.swing_fighters(ctx, swing, Some(player), prop_blockers);
            self.finish_swing(swing, stats);
        }
    }

    /// One fighter's swing this tick against every other fighter on another
    /// side, the one melee contact path: its authored hitboxes active on
    /// this pose (swept from the tick before), each defender's authored
    /// hurtboxes, world occlusion between the two, the one swing latch
    /// (each defender once per strike window), the attacker's outgoing
    /// damage and crit, then the one landing ([`Self::land_on_fighter`]).
    /// The player is a defender when `player` is given (an entity's swing).
    fn swing_fighters(
        &mut self,
        ctx: &Ctx,
        swing: Swing,
        player: Option<&PlayerDefence>,
        prop_blockers: &[CharacterCollisionAabb],
    ) -> MeleeArcStats {
        let mut stats = MeleeArcStats::default();
        let Some(pose) = self.actor_pose(swing.pose) else {
            return stats;
        };
        let strike = ActiveStrike::sample(
            swing.capsules,
            swing.action,
            pose,
            swing
                .previous
                .and_then(|previous| self.actor_pose(previous)),
        );
        if strike.is_empty() {
            return stats;
        }
        let actor = RuntimeGameEntities::ACTOR_SLOT;
        if swing.attacker == actor {
            self.strike_world(swing, &strike);
        }
        let team = self.fighter_team(swing.attacker);
        let count = self.game_entities.count().min(GAME_ENTITIES.len());
        // Every entity in slot order, then the player.
        let mut defender = 0usize;
        while defender <= count {
            let slot = if defender == count { actor } else { defender };
            defender += 1;
            if slot == swing.attacker || self.fighter_team(slot) == team {
                continue;
            }
            let target = if slot == actor {
                match player {
                    Some(player) if !player.passes_through => SwingTarget {
                        capsules: player.capsules,
                        pose: PoseAt::Player,
                        position: player.position,
                        room: self.room_index,
                    },
                    _ => continue,
                }
            } else {
                let record = &GAME_ENTITIES[slot];
                if self.game_entities.state(slot) == GameEntityState::Dead
                    || self
                        .game_entities
                        .passes_through(GAME_ENTITIES.as_slice(), slot)
                {
                    continue;
                }
                let body = self.game_entities.body(record);
                let first = body.combat_capsule_first.to_usize();
                let end = first.saturating_add(usize::from(body.combat_capsule_count));
                SwingTarget {
                    capsules: COMBAT_CAPSULES.get(first..end).unwrap_or(&[]),
                    pose: PoseAt::Instance(record.model_instance),
                    position: self.game_entities.position(slot),
                    room: record.room,
                }
            };
            if target.room != swing.room {
                continue;
            }
            let spent = self.game_entities.spent_windows(swing.attacker, slot);
            let Some(contact) = strike.contact(
                swing.capsules,
                target.capsules,
                self.actor_pose(target.pose),
                spent,
            ) else {
                continue;
            };
            // Capsule overlap alone has no world knowledge: a closed door
            // between the two blocks the connection without latching it, so
            // the same swing can still land once the door opens.
            if !self.melee_line_clear(swing.position, target.position, prop_blockers)
                || !self
                    .game_entities
                    .latch_strike(swing.attacker, slot, contact.window)
            {
                continue;
            }
            let (damage, poise) = self.fighter_outgoing(
                swing.attacker,
                swing.channel,
                contact.damage,
                contact.poise_damage,
            );
            if stats.hits.saturating_add(stats.parries) == 0 && swing.crit {
                self.duel_log_fighter_event(ctx, swing.attacker, duel::DUEL_EVENT_CRIT);
            }
            let strike = Strike {
                attacker: self.fighter_id(swing.attacker),
                kind: HitKind::Melee,
                channel: swing.channel,
                damage,
                poise,
                crit: swing.crit,
            };
            let outcome =
                self.land_on_fighter(ctx, slot, strike, Some(swing.position), swing.heavy);
            stats.parries = stats.parries.saturating_add(u16::from(outcome.parried));
            stats.hits = stats.hits.saturating_add(u16::from(outcome.connected));
            stats.staggers = stats.staggers.saturating_add(u16::from(outcome.staggered));
            stats.deaths = stats.deaths.saturating_add(u16::from(outcome.died));
        }
        stats
    }

    /// What a swing that reached anyone this tick leaves on its attacker:
    /// the crit used up (parried or not), and for the player's character the
    /// hit cues. Energy is paid per landing ([`Self::answer_strike`]).
    fn finish_swing(&mut self, swing: Swing, stats: MeleeArcStats) {
        if stats.hits.saturating_add(stats.parries) == 0 {
            return;
        }
        self.spend_swing_crit(swing.attacker, stats);
        if swing.attacker == RuntimeGameEntities::ACTOR_SLOT {
            self.report_player_melee_stats(stats);
        }
    }

    /// The damage and poise slot `fighter` deals with an authored strike:
    /// its outgoing boons (the player's modules), then its crit.
    fn fighter_outgoing(
        &self,
        fighter: usize,
        channel: VitalityChannelId,
        damage: u16,
        poise: u16,
    ) -> (u16, u16) {
        let damage = if fighter == RuntimeGameEntities::ACTOR_SLOT {
            self.vitality_modifiers().outgoing_damage(channel, damage)
        } else {
            damage
        };
        self.game_entities
            .crit_scaled(GAME_ENTITIES.as_slice(), fighter, damage, poise)
    }

    /// The side slot `fighter` fights on: the player's character on
    /// [`CombatTeam::Player`], an entity on its own.
    fn fighter_team(&self, fighter: usize) -> CombatTeam {
        if fighter == RuntimeGameEntities::ACTOR_SLOT {
            CombatTeam::Player
        } else {
            self.game_entities.team(fighter)
        }
    }

    /// The pose snapshot `at` names, by reference (None when that actor
    /// has no pose this tick).
    pub(super) fn actor_pose(
        &self,
        at: PoseAt,
    ) -> Option<&psx_game_runtime::actor_pose::ActorPoseSnapshot> {
        match at {
            PoseAt::Player => self.player_actor_pose.as_ref().map(|s| s.pose()),
            PoseAt::PreviousPlayer => self.previous_player_actor_pose.as_ref().map(|s| s.pose()),
            PoseAt::Instance(n) => self.instance_pose(n).map(|s| s.pose()),
            PoseAt::PreviousInstance(n) => self
                .previous_instance_actor_poses
                .get(usize::from(n))
                .and_then(Option::as_ref)
                .map(|s| s.pose()),
        }
    }

    /// Model instance `n`'s snapshot this tick, by reference.
    fn instance_pose(&self, n: u16) -> Option<&InstanceActorPoseSnapshot> {
        self.instance_actor_poses
            .get(usize::from(n))
            .and_then(Option::as_ref)
    }

    /// Slot `fighter` as the attacker a strike names.
    fn fighter_id(&self, fighter: usize) -> CombatantId {
        if fighter == RuntimeGameEntities::ACTOR_SLOT {
            CombatantId::PLAYER
        } else {
            CombatantId::entity(fighter)
        }
    }

    /// A duel event of slot `fighter`, on its duel side.
    fn duel_log_fighter_event(&self, ctx: &Ctx, fighter: usize, kind: u32) {
        if fighter == RuntimeGameEntities::ACTOR_SLOT {
            self.duel_log_event(ctx, 0, kind);
        } else {
            self.duel_log_rival_event(ctx, fighter, kind);
        }
    }

    /// The world a swing of the player's character strikes, on its active
    /// blade volumes: the vitality circles it touches on its stance's axis
    /// and the destructible props inside a volume.
    fn strike_world(&mut self, swing: Swing, strike: &ActiveStrike) {
        let channel = match swing.channel {
            VitalityChannelId::One => DamageChannel::Horizon,
            VitalityChannelId::Two => DamageChannel::Zenith,
        };
        let count = strike.volumes().len();
        for volume in 0..count {
            let (capsule, record) = strike.volume(volume);
            for end in [capsule.start, capsule.end] {
                self.vitality_circles.claim_struck(
                    VITALITY_CIRCLES.as_slice(),
                    swing.room,
                    end[0],
                    end[2],
                    capsule.radius,
                    swing.channel,
                );
            }
            let damage = swing.capsules.get(record).map_or(0, |record| record.damage);
            let (damage, _) = self.fighter_outgoing(swing.attacker, swing.channel, damage, 0);
            self.damage_world_destructibles_with_capsule(
                capsule.start,
                capsule.end,
                capsule.radius,
                channel,
                damage,
            );
        }
    }

    /// One hit on fighter `victim` (an entity or her actor slot), whoever
    /// dealt it (a fighter's swing or bolt, or the world): the victim's hit
    /// path, the attacker's side ([`GameEntities::answer_strike`]: riposte,
    /// melee energy, a bolt's interrupt report), then what every landed hit
    /// shows (the damage number, a swing's contact spark from
    /// `contact_from`, the flash). A parry shows the parrier's tell. What
    /// follows a death is the game's rule: souls for a body that is not
    /// hers, her death sequence for hers. `heavy` marks a heavy swing.
    // Once per landed hit, from every attacker's path: out of line and small.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn land_on_fighter(
        &mut self,
        ctx: &Ctx,
        victim: usize,
        strike: Strike,
        contact_from: Option<[i32; 3]>,
        heavy: bool,
    ) -> psx_game_runtime::entities::GameEntityHitOutcome {
        use psx_game_runtime::entities::GameEntityHitOutcome;
        let actor = RuntimeGameEntities::ACTOR_SLOT;
        let (outcome, applied, opening) = if victim == actor {
            match self.receive_on_player(ctx, strike) {
                Some(landed) => landed,
                None => return GameEntityHitOutcome::MISS,
            }
        } else {
            if GAME_ENTITIES.get(victim).is_none() {
                return GameEntityHitOutcome::MISS;
            }
            let applied = self.game_entities.scaled_stance_damage(
                GAME_ENTITIES.as_slice(),
                victim,
                strike.channel,
                strike.damage,
            );
            // A bolt reads the victim's open windup before it lands.
            let opening = strike.kind == HitKind::Projectile
                && self
                    .game_entities
                    .shot_opening(GAME_ENTITIES.as_slice(), victim);
            let outcome = self
                .game_entities
                .receive_hit(GAME_ENTITIES.as_slice(), victim, strike);
            (outcome, applied, opening)
        };
        let attacker = fighter_slot(strike.attacker);
        let riposte = match attacker {
            Some(attacker) => self.game_entities.answer_strike(
                GAME_ENTITIES.as_slice(),
                attacker,
                self.fighter_id(victim),
                strike.kind,
                heavy,
                outcome,
                opening,
            ),
            None => GameEntityHitOutcome::MISS,
        };
        if outcome.parried {
            self.show_parry(ctx, victim, attacker, riposte);
            return outcome;
        }
        if !outcome.connected {
            return outcome;
        }
        if strike.kind == HitKind::Projectile && opening && outcome.staggered {
            telemetry::debug_log("flow:shot-interrupt");
            self.duel_log_flow(ctx, u32::from(victim == actor));
        }
        let zenith = strike.channel == VitalityChannelId::Two;
        let (position, height, room) = self.fighter_body_at(victim);
        let now = self.gameplay_tick(ctx.sim_tick);
        self.damage_numbers.spawn(
            if victim == actor {
                player_hit_anchor(position, height)
            } else {
                struck_actor_anchor(position, height)
            },
            room,
            applied,
            if strike.crit {
                DamageNumberChannel::Crit
            } else if zenith {
                DamageNumberChannel::Zenith
            } else {
                DamageNumberChannel::Horizon
            },
            now,
        );
        if contact_from.is_some() {
            self.spawn_combat_hit_sparks(
                position,
                height,
                room,
                zenith,
                outcome.staggered,
                outcome.died,
            );
        }
        self.show_hit(LandedHit {
            victim: self.fighter_id(victim),
            contact_from,
            kind: FlashKind::of_hit(strike.crit, outcome.staggered || heavy),
            zenith,
        });
        if outcome.died {
            if victim == actor {
                self.game_entities.clear_hazard_push(actor);
                self.arm_player_death(strike.kind != HitKind::Hazard, ctx.sim_tick, ctx.video_hz);
            } else if let Some(record) = GAME_ENTITIES.get(victim) {
                // A body other than the player's dissolves and drops its
                // souls, whoever or whatever killed it.
                self.souls.award(record.soul_value, now.as_u32());
            }
        }
        outcome
    }

    /// Her side of a hit, the owner's half of the one landing for the
    /// fighter it drives: her boons reduce the damage, [`Combatant::receive_hit`]
    /// over her stats in the fighter table, then the reaction her
    /// animation states play. Returns the outcome, the damage the stance
    /// rule makes of it and whether a bolt found her windup open; `None`
    /// while she is dying (nothing lands).
    fn receive_on_player(
        &mut self,
        ctx: &Ctx,
        strike: Strike,
    ) -> Option<(psx_game_runtime::entities::GameEntityHitOutcome, u16, bool)> {
        if self.death_ticks_remaining != 0 {
            return None;
        }
        let opening = strike.kind == HitKind::Projectile
            && self.player_shot_opening()
            && self.game_entities.actor().flow.can_interrupt();
        let strike = Strike {
            damage: self.vitality_modifiers().incoming_damage(strike.damage),
            ..strike
        };
        let (applied, _) = stance_scaled(
            self.game_entities.actor_stats(),
            self.game_entities.actor().opposes(strike.channel),
            strike.damage,
            0,
        );
        let armored = self.player_armoured(ctx);
        let hit_in = self.player_hit(strike, armored);
        let out = self.game_entities.actor_mut().receive_hit(hit_in);
        if out.parried {
            return Some((
                psx_game_runtime::entities::GameEntityHitOutcome {
                    parried: true,
                    ..psx_game_runtime::entities::GameEntityHitOutcome::MISS
                },
                0,
                false,
            ));
        }
        telemetry::counter(telemetry::counter::PLAYER_HITS_TAKEN, 1);
        if out.damage_applied > 0 {
            self.queue_gameplay_sfx(LevelGameplaySfxEvent::PlayerDamage);
        }
        // The one reaction rule: only a swing flinches; a break staggers
        // whatever broke it.
        let staggered = !out.killed
            && self.react_player(out, armored, strike.kind != HitKind::Melee, false, ctx);
        Some((
            psx_game_runtime::entities::GameEntityHitOutcome {
                connected: true,
                staggered,
                knocked_down: out.knocked_down,
                died: out.killed,
                parried: false,
            },
            applied,
            opening,
        ))
    }

    /// Whether her current action armours her, by the one armour rule:
    /// her authored Armored window, else her heavy while its contact is
    /// live (a hitbox of the action active on her pose).
    fn player_armoured(&self, ctx: &Ctx) -> bool {
        let action = self.anim.state.action();
        let capsules = self.player_defence_capsules();
        let contact = matches!(
            self.anim.state,
            PlayerAnim::HeavyAttack | PlayerAnim::VertHeavyAttack
        ) && self.player_actor_pose.is_some_and(|snapshot| {
            let frame = (snapshot.pose().phase_q12() >> 12).min(u32::from(u16::MAX)) as u16;
            capsules.iter().any(|capsule| {
                capsule.flags & psx_level::combat_capsule_flags::HITBOX != 0
                    && capsule.action == action.to_index() as u8
                    && frame >= capsule.active_start_frame
                    && frame <= capsule.active_end_frame
            })
        });
        psx_game_runtime::combatant::armoured(
            self.game_entities.actor_stats(),
            self.player_combat_sample(ctx)
                .active(psx_level::CombatWindowKind::Armored),
            contact,
        )
    }

    /// Feet, height and room of fighter `slot` (her actor slot or an entity).
    fn fighter_body_at(&self, slot: usize) -> ([i32; 3], u16, RoomIndex) {
        if slot == RuntimeGameEntities::ACTOR_SLOT {
            let p = self.game_entities.actor_motor().position();
            (
                [p.x, p.y, p.z],
                self.character.map_or(0, |c| c.height.max(0) as u16),
                self.room_index,
            )
        } else {
            let record = &GAME_ENTITIES[slot];
            (
                self.game_entities.position(slot),
                self.game_entities.body(record).height,
                record.room,
            )
        }
    }

    /// The tell of a parry by fighter `parrier` of a strike by `attacker`
    /// (`riposte`: what the parry did to an entity attacker): her parry,
    /// an entity's parry of her (whose riposte staggers her here), or one
    /// entity's of another's.
    // Rare (a parry): kept out of line and small.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    fn show_parry(
        &mut self,
        ctx: &Ctx,
        parrier: usize,
        attacker: Option<usize>,
        riposte: psx_game_runtime::entities::GameEntityHitOutcome,
    ) {
        let actor = RuntimeGameEntities::ACTOR_SLOT;
        if parrier == actor {
            self.player_parried(attacker, riposte, ctx);
        } else if attacker == Some(actor) {
            self.entity_parried_player(parrier, ctx);
        } else {
            self.duel_log_rival_event(ctx, parrier, duel::DUEL_EVENT_PARRY);
            let zenith = self.game_entities.stance(parrier) == VitalityChannelId::Two;
            self.game_entities
                .start_flash(parrier, &COMBAT_FEEDBACK, FlashKind::Parry, zenith);
        }
    }

    /// Advance every live projectile against the player's hurtboxes (unless
    /// she is invulnerable) and every live entity's. Every bolt that
    /// strikes a fighter lands at once through the one landing.
    fn tick_combat_projectiles<const I: usize>(
        &mut self,
        ctx: &Ctx,
        player: &PlayerDefence,
        prop_blockers: &[CharacterCollisionAabb],
        prop_blockers_valid: bool,
        impacts: &mut ProjectileImpacts<I>,
    ) {
        // Every fighter is struck by the one rule: each authored hurtbox on
        // its pose, else its body capsule (combat::projectile_hurtboxes).
        let mut projectile_targets = psx_engine::FixedScratch::<
            ProjectileTarget,
            { (MAX_GAME_ENTITIES + 1) * combat::PROJECTILE_HURTBOXES_PER_FIGHTER },
        >::new();
        // No live bolt, nothing to strike: skip sampling every fighter's
        // hurtboxes (each a joint decode) on the ticks with no projectile.
        let live = !self.combat_projectiles.is_empty();
        if live && !player.passes_through {
            combat::projectile_hurtboxes(
                player.capsules,
                self.actor_pose(PoseAt::Player),
                combat::body_hurt_capsule(player.position, player.radius, player.height),
                |hurtbox| {
                    let _ = projectile_targets.try_push(ProjectileTarget {
                        target: PLAYER_PROJECTILE_TARGET,
                        team: CombatTeam::Player,
                        room: self.room_index,
                        hurtbox,
                    });
                },
            );
        }
        for (index, record) in GAME_ENTITIES
            .iter()
            .enumerate()
            .take(if live { self.game_entities.count() } else { 0 })
        {
            // An entity in its i-frames past its parry window is not a
            // target, exactly as the player above: the bolt flies on through.
            if self.game_entities.state(index) == GameEntityState::Dead
                || self
                    .game_entities
                    .passes_through(GAME_ENTITIES.as_slice(), index)
            {
                continue;
            }
            let body = self.game_entities.body(record);
            let first = body.combat_capsule_first.to_usize();
            let team = self.game_entities.team(index);
            combat::projectile_hurtboxes(
                COMBAT_CAPSULES
                    .get(first..first + usize::from(body.combat_capsule_count))
                    .unwrap_or(&[]),
                self.instance_actor_poses
                    .get(usize::from(record.model_instance))
                    .and_then(Option::as_ref)
                    .map(|pose| pose.pose()),
                combat::body_hurt_capsule(
                    self.game_entities.position(index),
                    i32::from(body.radius),
                    i32::from(body.height),
                ),
                |hurtbox| {
                    let _ = projectile_targets.try_push(ProjectileTarget {
                        target: index as u16,
                        team,
                        room: record.room,
                        hurtbox,
                    });
                },
            );
        }
        {
            let mut tracer = SceneProjectileWorldTracer {
                world_rt: self.world_rt.as_mut(),
                destructibles: &self.destructibles,
                prop_blockers,
                valid: prop_blockers_valid,
            };
            let _ =
                self.combat_projectiles
                    .tick(projectile_targets.as_slice(), &mut tracer, impacts);
        }
        for impact in impacts.as_slice() {
            let _ = self.combat_projectile_impacts.spawn(impact);
            let ProjectileImpactKind::Target { target } = impact.kind else {
                continue;
            };
            let victim = if target == PLAYER_PROJECTILE_TARGET {
                RuntimeGameEntities::ACTOR_SLOT
            } else {
                usize::from(target)
            };
            // Her bolts carry no owner: an unowned bolt is hers on an
            // entity and the world's on her.
            let attacker = if impact.owner != NO_PROJECTILE_OWNER {
                CombatantId::entity(usize::from(impact.owner))
            } else if victim == RuntimeGameEntities::ACTOR_SLOT {
                CombatantId::WORLD
            } else {
                CombatantId::PLAYER
            };
            // A world bolt cannot be parried; inside her parry window it is
            // spent on her i-frames.
            if attacker == CombatantId::WORLD && player.parries {
                continue;
            }
            let crit = impact.visual.impact_rgb == CRIT_BOLT_RGB;
            if crit
                && impact.owner != NO_PROJECTILE_OWNER
                && victim != RuntimeGameEntities::ACTOR_SLOT
            {
                self.duel_log_rival_event(ctx, usize::from(impact.owner), duel::DUEL_EVENT_CRIT);
            }
            let channel = projectile_vitality_channel(impact.damage_channel);
            let strike = if victim == RuntimeGameEntities::ACTOR_SLOT {
                // Her posture takes the shot rule as every fighter's does:
                // an open windup takes more.
                let poise = self.game_entities.actor().flow.shot_poise(
                    impact.poise_damage,
                    self.game_entities.actor_stats().poise,
                    self.player_shot_opening(),
                );
                Strike {
                    attacker,
                    kind: HitKind::Projectile,
                    channel,
                    damage: impact.damage,
                    poise,
                    crit,
                }
            } else {
                self.game_entities.projectile_strike(
                    GAME_ENTITIES.as_slice(),
                    victim,
                    attacker,
                    channel,
                    impact.damage,
                    impact.poise_damage,
                    crit,
                )
            };
            self.land_on_fighter(ctx, victim, strike, None, false);
        }
    }

    /// Duel trace: a bolt interrupted a windup. `victim` 0 is the enemy,
    /// 1 the player.
    fn duel_log_flow(&self, ctx: &Ctx, victim: u32) {
        if self.duel.active {
            duel::log_values(
                "duel:flow",
                &[ctx.sim_tick.as_u32() - self.duel.started_tick(), victim, 1],
            );
        }
    }

    /// Release every authored projectile a ranged enemy token has reached.
    fn release_enemy_projectiles(
        &mut self,
        ctx: &Ctx,
        attack: DeferredGameEntityAttack,
        attacker_capsules: &'static [psx_level::CombatCapsuleRecord],
        instance: u16,
    ) {
        let entity = attack.entity();
        let Some(target) = self.game_entities.ranged_target(entity) else {
            return;
        };
        self.release_projectiles(
            ctx,
            Volley {
                shooter: Shooter::Entity(attack),
                capsules: attacker_capsules,
                action: attack.action(),
                pose: PoseAt::Instance(instance),
                frame: self
                    .actor_pose(PoseAt::Instance(instance))
                    .map_or(0, |pose| {
                        (pose.phase_q12() >> 12).min(u32::from(u16::MAX)) as u16
                    }),
                released: self
                    .game_entities
                    .deferred_projectile_release_mask(attack)
                    .unwrap_or(u16::MAX),
                target,
                room: attack.room(),
                team: self.game_entities.team(entity),
                owner: entity.min(u16::MAX as usize) as u16,
                channel: None,
                modifiers: VitalityModifiers::IDENTITY,
                visual: enemy_projectile_visual,
                sight_from: None,
                precise: true,
            },
        );
    }

    /// Fire every authored emitter of `volley` whose release window the
    /// frame has reached, until the shooter runs out of energy or the pool is
    /// full. The one release path for every shooter: what differs between
    /// them today is data on the [`Volley`].
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn release_projectiles(&mut self, ctx: &Ctx, volley: Volley) {
        let mut released = volley.released;
        // Every bolt of the first volley released inside the shooter's crit
        // window is a crit, and the volley uses the crit up.
        let crit = self.shooter_crit_armed(volley.shooter);
        let mut crit_released = false;
        while let Some((emitter, release)) = combat::authored_projectile_release_pending_at_frame(
            volley.capsules,
            volley.action,
            self.actor_pose(volley.pose),
            released,
            volley.frame,
        ) {
            if let (Some(origin), Some(world_rt)) = (volley.sight_from, self.world_rt.as_mut()) {
                let muzzle = RoomPoint::new(
                    release.position[0],
                    release.position[1],
                    release.position[2],
                );
                if !world_rt.melee_segment_clear(origin, muzzle, &[], &self.destructibles) {
                    self.commit_shot(volley.shooter, emitter);
                    released |= 1u16 << emitter;
                    telemetry::debug_log("player projectile:muzzle-blocked");
                    continue;
                }
            }
            if !self.shooter_can_shoot(volley.shooter) {
                break;
            }
            let damage_channel = volley.channel.unwrap_or(release.damage_channel);
            let damage = volley.modifiers.outgoing_damage(
                match damage_channel {
                    psx_game_runtime::projectiles::ProjectileDamageChannel::Horizon => {
                        VitalityChannelId::One
                    }
                    psx_game_runtime::projectiles::ProjectileDamageChannel::Zenith => {
                        VitalityChannelId::Two
                    }
                },
                release.damage,
            );
            let (damage, poise_damage) = if crit {
                self.shooter_crit_scaled(volley.shooter, damage, release.poise_damage)
            } else {
                (damage, release.poise_damage)
            };
            let visual = (volley.visual)(release.visual);
            let spawn = ProjectileSpawn {
                position: release.position,
                velocity: psx_game_runtime::projectiles::velocity_toward(
                    release.position,
                    volley.target,
                    release.speed,
                ),
                radius: release.radius,
                damage,
                // The receiving actor evaluates the visible interrupt window.
                poise_damage,
                lifetime_ticks: release.lifetime_ticks,
                room: volley.room,
                team: volley.team,
                owner: volley.owner,
                tint_rgb: release.tint_rgb,
                damage_channel,
                visual: if crit {
                    crit_projectile_visual(visual)
                } else {
                    visual
                },
            };
            let spawned = if volley.precise {
                self.combat_projectiles
                    .spawn_toward(spawn, volley.target, release.speed)
            } else {
                self.combat_projectiles.spawn(spawn)
            };
            if spawned.is_err() {
                // Retry on a later retained pose; never consume a shot
                // that the bounded pool could not hold.
                break;
            }
            self.shooter_spend_shot(volley.shooter);
            crit_released |= crit;
            self.duel_log_shot(ctx, volley.shooter);
            let _ = self.combat_projectile_impacts.spawn_muzzle(&spawn);
            self.commit_shot(volley.shooter, emitter);
            released |= 1u16 << emitter;
            self.queue_gameplay_sfx(LevelGameplaySfxEvent::ProjectileLaunch);
            match volley.shooter {
                Shooter::Player => telemetry::debug_log("player projectile:release"),
                Shooter::Entity(_) => {
                    telemetry::debug_log("enemy projectile:release");
                    #[cfg(feature = "emulator-telemetry")]
                    self.debug_log_enemy_shot(ctx, &spawn);
                }
            }
        }
        if crit_released {
            let actor = match volley.shooter {
                Shooter::Player => {
                    self.game_entities.actor_mut().spend_crit();
                    0
                }
                Shooter::Entity(attack) => {
                    self.game_entities.spend_crit(attack.entity());
                    1
                }
            };
            self.duel_log_event(ctx, actor, duel::DUEL_EVENT_CRIT);
        }
    }

    /// Whether the shooter's volley under way is a crit.
    fn shooter_crit_armed(&self, shooter: Shooter) -> bool {
        match shooter {
            Shooter::Player => self.game_entities.actor().crit_attack,
            Shooter::Entity(attack) => self.game_entities.attack_is_crit(attack.entity()),
        }
    }

    /// Damage and poise of one of the shooter's bolts through its crit.
    fn shooter_crit_scaled(&self, shooter: Shooter, damage: u16, poise: u16) -> (u16, u16) {
        match shooter {
            Shooter::Player => self.game_entities.actor().crit_scaled(
                self.game_entities.actor_stats(),
                damage,
                poise,
            ),
            Shooter::Entity(attack) => self.game_entities.crit_scaled(
                GAME_ENTITIES.as_slice(),
                attack.entity(),
                damage,
                poise,
            ),
        }
    }

    fn shooter_can_shoot(&self, shooter: Shooter) -> bool {
        match shooter {
            Shooter::Player => self
                .game_entities
                .actor()
                .flow
                .can_shoot(self.game_entities.actor_stats()),
            Shooter::Entity(attack) => GAME_ENTITIES
                .get(attack.entity())
                .is_some_and(|record| self.game_entities.can_fire_energy(record, attack.entity())),
        }
    }

    fn shooter_spend_shot(&mut self, shooter: Shooter) {
        match shooter {
            Shooter::Player => {
                let (actor, stats) = self.game_entities.actor_and_stats();
                let _ = actor.flow.spend_shot(stats);
            }
            Shooter::Entity(attack) => {
                if let Some(record) = GAME_ENTITIES.get(attack.entity()) {
                    self.game_entities
                        .spend_shot_energy(record, attack.entity());
                }
            }
        }
    }

    /// Consume `emitter` so it cannot fire again this attack.
    fn commit_shot(&mut self, shooter: Shooter, emitter: u8) {
        match shooter {
            Shooter::Player => self.ranged_ready.released |= 1u16 << emitter,
            Shooter::Entity(attack) => {
                let _ = self
                    .game_entities
                    .commit_deferred_projectile(attack, emitter);
            }
        }
    }

    /// Duel trace: a shot left the muzzle, with the shooter's energy after
    /// paying for it. Shooter 0 is the player, 1 the enemy.
    fn duel_log_shot(&self, ctx: &Ctx, shooter: Shooter) {
        if !self.duel.active {
            return;
        }
        let (side, energy) = match shooter {
            Shooter::Player => (0, self.game_entities.actor().flow.energy),
            Shooter::Entity(attack) => (
                self.duel_side(attack.entity()),
                self.game_entities.energy(attack.entity()),
            ),
        };
        duel::log_values(
            "duel:shot",
            &[
                ctx.sim_tick.as_u32() - self.duel.started_tick(),
                side,
                u32::from(energy),
            ],
        );
    }

    /// Emulator telemetry: an enemy shot with the player's hurtbox.
    #[cfg(feature = "emulator-telemetry")]
    fn debug_log_enemy_shot(&self, ctx: &Ctx, spawn: &ProjectileSpawn) {
        let player = self.player_defence(ctx);
        let hurt = player
            .capsules
            .iter()
            .find(|c| c.flags & psx_level::combat_capsule_flags::HURTBOX != 0)
            .and_then(|c| {
                player
                    .pose
                    .and_then(|pose| combat::transform_actor_combat_capsule(c, pose))
            });
        if let Some(hurt) = hurt {
            crate::debug_runtime::debug_log_enemy_shot([
                spawn.position[0],
                spawn.position[1],
                spawn.position[2],
                spawn.velocity[0],
                spawn.velocity[1],
                spawn.velocity[2],
                player.position[0],
                player.position[1],
                player.position[2],
                player.height,
                hurt.start[0],
                hurt.start[1],
                hurt.start[2],
                hurt.end[0],
                hurt.end[1],
                hurt.end[2],
                i32::from(hurt.radius),
            ]);
        }
    }

    /// The player's swing as a watcher reads it: her strike clip, how far
    /// into it she is, and her authored hitboxes. None when she is not
    /// striking.
    // Read once per think by a controller; kept out of the large callers.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn player_swing(&self, now: SimTick) -> Option<SwingClock> {
        if !matches!(
            self.anim.state,
            PlayerAnim::LightAttack
                | PlayerAnim::HeavyAttack
                | PlayerAnim::VertLightAttack
                | PlayerAnim::VertHeavyAttack
        ) {
            return None;
        }
        let character = self.character.as_ref()?;
        let action = self.anim.state.action();
        let first = character.combat_capsule_first.to_usize();
        let end = first.saturating_add(usize::from(character.combat_capsule_count));
        Some(SwingClock {
            animation: self
                .models
                .get(character.model.to_usize())
                .copied()
                .flatten()
                .and_then(|model| model.clip(&self.clips, character.clip_for(self.anim.state))),
            local_tick: now.saturating_sub(self.game_entities.actor_action().start),
            looping: character.action_loops(action),
            speed_q8: crate::playtest_runtime::live_action_speed_q8(
                self.vitality_modifiers(),
                character,
                self.anim.state,
            ),
            frame_range: character.action_frame_range(action),
            action,
            capsules: COMBAT_CAPSULES.get(first..end).unwrap_or(&[]),
            record_contact: None,
        })
    }

    /// Time every live entity's attacks from its Character's clips: the clip
    /// rules her actions are timed by, with each attack's authored contact
    /// frames placed on its clip's clock. Once per spawn, before the entities
    /// first think after it; an attack whose clip authors no contact keeps its
    /// record's ticks.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn time_entity_attacks(&mut self, video_hz: VideoHz) {
        let models = &self.models;
        let clips = &self.clips;
        self.game_entities.time_attacks(
            GAME_ENTITIES.as_slice(),
            |record, action, clip, speed_q8, frame_range| {
                let character = CHARACTERS.get(record.character.to_usize())?;
                let animation = models
                    .get(character.model.to_usize())
                    .copied()
                    .flatten()?
                    .clip(clips, ModelClipIndex(clip))?;
                let first = character.combat_capsule_first.to_usize();
                let capsules = COMBAT_CAPSULES
                    .get(first..first + usize::from(character.combat_capsule_count))?;
                let contact = combat::authored_event_frames(capsules, action)?;
                let window = |kind| mr::authored_window(&character.combat_windows, action, kind);
                mr::action_timing(
                    animation,
                    video_hz,
                    speed_q8,
                    frame_range,
                    contact,
                    window(psx_level::CombatWindowKind::Armored),
                    window(psx_level::CombatWindowKind::Invulnerable),
                )
            },
        );
        self.game_entities.time_flinches(
            GAME_ENTITIES.as_slice(),
            |record, clip, speed_q8, frame_range| {
                let character = CHARACTERS.get(record.character.to_usize())?;
                let animation = models
                    .get(character.model.to_usize())
                    .copied()
                    .flatten()?
                    .clip(clips, ModelClipIndex(clip))?;
                let ticks = mr::clip_duration_ticks(animation, video_hz, speed_q8, frame_range);
                Some(ticks.min(u32::from(u16::MAX)) as u16)
            },
        );
        self.entity_attacks_timed = true;
    }

    /// Entity `index`'s melee swing as a watcher reads it: its attack clip,
    /// its swing clock, its authored hitboxes and its record's contact
    /// ticks. None when it is not winding up or swinging a blade.
    // Read once per think by a controller; kept out of the large callers.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn entity_swing(&self, index: usize) -> Option<SwingClock> {
        let records = GAME_ENTITIES.as_slice();
        let entity = records.get(index)?;
        let (action, first, last) = self.game_entities.committed_swing(records, index)?;
        if self.game_entities.attack_kind(index) == 2 {
            return None;
        }
        let clip = self.game_entities.clip_for_state(records, index);
        let capsule_body = self.game_entities.body(entity);
        let capsule_first = capsule_body.combat_capsule_first.to_usize();
        let capsule_end =
            capsule_first.saturating_add(usize::from(capsule_body.combat_capsule_count));
        Some(SwingClock {
            animation: MODEL_INSTANCES
                .get(usize::from(entity.model_instance))
                .and_then(|instance| self.models.get(instance.model.to_usize()))
                .copied()
                .flatten()
                .and_then(|model| model.clip(&self.clips, ModelClipIndex(clip.clip))),
            local_tick: u32::from(clip.phase_ticks),
            looping: !clip.one_shot,
            speed_q8: clip.speed_q8,
            frame_range: clip.frame_range,
            action,
            capsules: COMBAT_CAPSULES
                .get(capsule_first..capsule_end)
                .unwrap_or(&[]),
            record_contact: Some((first, last)),
        })
    }

    /// Predict, for every fighter a blade is swinging at, the tick the blade
    /// first meets its body, and hand it to the fighter's controller for its
    /// parry ([`RuntimeGameEntities::predict_swing_contact`]): an entity's
    /// swing comes from its rival or else from her, hers in a duel from her
    /// rival. Once per entity think, before it.
    // Read once per think; kept out of the large callers.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn predict_swing_contacts(&mut self, now: SimTick, video_hz: VideoHz) {
        let records = GAME_ENTITIES.as_slice();
        let count = self.game_entities.count().min(records.len());
        let player_pose = self
            .player_actor_pose
            .as_ref()
            .map(|snapshot| snapshot.pose());
        let player_capsules = self.player_defence_capsules();
        let entity_pose = |slot: usize| {
            records
                .get(slot)
                .and_then(|record| {
                    self.instance_actor_poses
                        .get(usize::from(record.model_instance))
                })
                .and_then(Option::as_ref)
                .map(|snapshot| snapshot.pose())
        };
        let characters = self.game_entities.characters();
        let entity_capsules = |slot: usize| {
            records.get(slot).map_or(&[][..], |record| {
                let capsule_body = psx_game_runtime::entities::body_of(characters, record);
                let first = capsule_body.combat_capsule_first.to_usize();
                let end = first.saturating_add(usize::from(capsule_body.combat_capsule_count));
                COMBAT_CAPSULES.get(first..end).unwrap_or(&[])
            })
        };
        let player_swing = self.player_swing(now);
        let mut slot = 0usize;
        while slot < count {
            let engaged = !matches!(
                self.game_entities.state(slot),
                GameEntityState::Idle | GameEntityState::Patrol | GameEntityState::Dead
            );
            let contact = if !engaged {
                PredictedContact::Unread
            } else {
                let (swing, attacker_pose) = match self.game_entities.rival(slot) {
                    Some(rival) => (self.entity_swing(rival), entity_pose(rival)),
                    None => (player_swing, player_pose),
                };
                predicted_contact(
                    swing.as_ref(),
                    attacker_pose,
                    entity_capsules(slot),
                    entity_pose(slot),
                    self.game_entities.body(&records[slot]).combat.parry_ticks,
                    video_hz,
                )
            };
            self.game_entities.predict_swing_contact(slot, contact);
            slot += 1;
        }
        if self.duel_drives_actor() {
            let actor = RuntimeGameEntities::ACTOR_SLOT;
            let contact = match self.game_entities.rival(actor) {
                Some(rival) => predicted_contact(
                    self.entity_swing(rival).as_ref(),
                    entity_pose(rival),
                    player_capsules,
                    player_pose,
                    self.game_entities.actor_stats().parry_ticks,
                    video_hz,
                ),
                None => PredictedContact::Unread,
            };
            self.game_entities.predict_swing_contact(actor, contact);
        }
    }

    /// Her authored combat capsules (hurtboxes and hitboxes), empty without
    /// a Character.
    fn player_defence_capsules(&self) -> &'static [psx_level::CombatCapsuleRecord] {
        self.character
            .as_ref()
            .and_then(|character| {
                let first = character.combat_capsule_first.to_usize();
                let end = first.saturating_add(usize::from(character.combat_capsule_count));
                COMBAT_CAPSULES.get(first..end)
            })
            .unwrap_or(&[])
    }

    pub(super) fn track_enemy_tell(&mut self, index: usize, ctx: &Ctx, delta: u16) {
        let Some(attack) = self
            .deferred_enemy_attacks
            .as_slice()
            .iter()
            .copied()
            .find(|attack| attack.entity() == index)
        else {
            return;
        };
        let Some(entity) = GAME_ENTITIES.get(index) else {
            return;
        };
        let clip = attack.clip();
        let Some(animation) = MODEL_INSTANCES
            .get(usize::from(entity.model_instance))
            .and_then(|instance| self.models.get(instance.model.to_usize()))
            .copied()
            .flatten()
            .and_then(|model| model.clip(&self.clips, ModelClipIndex(clip.clip)))
        else {
            return;
        };
        let phase = mr::animation_phase_at_tick_q12(
            animation,
            u32::from(clip.phase_ticks),
            ctx.video_hz,
            !clip.one_shot,
            clip.speed_q8,
            clip.frame_range,
        );
        let capsule_body = self.game_entities.body(entity);
        let first = capsule_body.combat_capsule_first.to_usize();
        let end = first + usize::from(capsule_body.combat_capsule_count);
        let capsules = COMBAT_CAPSULES.get(first..end).unwrap_or(&[]);
        // The tell tracks the fighter it is aimed at: its rival, or her.
        let rival = self
            .game_entities
            .rival(index)
            .and_then(|slot| GAME_ENTITIES.get(slot).map(|record| (slot, record)));
        let target = match rival {
            Some((slot, _)) => self.game_entities.position(slot),
            None => {
                let player = self.game_entities.actor_motor().position();
                [player.x, player.y, player.z]
            }
        };
        if !attack.is_ranged() {
            if combat::melee_tell_tracks(capsules, attack.action(), phase) {
                self.game_entities
                    .track_authored_melee_tell(index, target, delta);
            }
            return;
        }
        if combat::ranged_charge_tracks(capsules, attack.action(), phase) {
            let height = match rival {
                Some((_, record)) => i32::from(self.game_entities.body(record).height),
                None => self
                    .character
                    .as_ref()
                    .map_or(WORLD_PLAYER_HEIGHT, |character| character.height),
            };
            self.game_entities
                .track_authored_ranged_charge(index, target, height, delta);
        }
    }

    pub(super) fn player_swing_sound(&mut self) {
        if !player_anim_is_attack(self.anim.state) {
            return;
        }
        let (Some(character), Some(pose)) =
            (self.character.as_ref(), self.player_actor_pose.as_ref())
        else {
            return;
        };
        let first = character.combat_capsule_first.to_usize();
        let end = first + usize::from(character.combat_capsule_count);
        let previous = self
            .previous_player_actor_pose
            .as_ref()
            .filter(|old| old.clip_local() == pose.clip_local())
            .map(|old| old.pose().phase_q12());
        if combat::melee_window_started(
            COMBAT_CAPSULES.get(first..end).unwrap_or(&[]),
            self.anim.state.action(),
            previous,
            pose.pose().phase_q12(),
        ) {
            if let Some(event) = weapon_swing_sfx(self.anim.state.action()) {
                self.queue_gameplay_sfx(event);
            }
            self.destructibles.begin_swing();
            telemetry::debug_log("player swing:active");
        }
    }

    /// The swing sound of `instance`'s melee window, read off this tick's
    /// and last tick's snapshots in place (each is a few hundred bytes).
    pub(super) fn enemy_swing_sound(&mut self, instance: usize) {
        let Some(pose) = self.instance_actor_poses.get(instance).and_then(Option::as_ref) else {
            return;
        };
        let Some(attack) = self
            .deferred_enemy_attacks
            .as_slice()
            .iter()
            .copied()
            .find(|attack| {
                !attack.is_ranged()
                    && GAME_ENTITIES
                        .get(attack.entity())
                        .is_some_and(|entity| usize::from(entity.model_instance) == instance)
            })
        else {
            return;
        };
        let entity = &GAME_ENTITIES[attack.entity()];
        let capsule_body = self.game_entities.body(entity);
        let first = capsule_body.combat_capsule_first.to_usize();
        let end = first + usize::from(capsule_body.combat_capsule_count);
        let previous = self
            .previous_instance_actor_poses
            .get(instance)
            .and_then(Option::as_ref)
            .filter(|old| old.clip_local() == pose.clip_local())
            .map(|old| old.pose().phase_q12());
        if combat::melee_window_started(
            COMBAT_CAPSULES.get(first..end).unwrap_or(&[]),
            attack.action(),
            previous,
            pose.pose().phase_q12(),
        ) {
            if let Some(event) = weapon_swing_sfx(attack.action()) {
                self.queue_gameplay_sfx(event);
            }
            telemetry::debug_log("enemy swing:active");
        }
    }

    /// The player's character's swing this tick, while an attack action is
    /// locked: the one melee contact path ([`Self::swing_fighters`]) from
    /// the exact snapshot the visible body and equipped weapon consume;
    /// combat never reconstructs animation phase. Outside attack locks this
    /// is two compares.
    fn resolve_player_melee(&mut self, ctx: &Ctx) {
        let now = ctx.sim_tick;
        if self.anim.state == PlayerAnim::RangedAttack
            || !player_anim_is_attack(self.anim.state)
            || self.game_entities.actor_action().lock_until <= now
        {
            return;
        }
        let (Some(character), Some(player_pose)) =
            (self.character.as_ref(), self.player_actor_pose.as_ref())
        else {
            return;
        };
        let mut prop_blockers =
            psx_engine::FixedScratch::<CharacterCollisionAabb, MAX_STATIC_PROP_AABB_BLOCKERS>::new(
            );
        if self.world_rt.is_some()
            && self
                .collect_static_prop_aabb_blockers_checked_into(&mut prop_blockers)
                .is_none()
        {
            // Fail closed before a hit can latch through invalid generated
            // prop state.
            return;
        }
        let first = character.combat_capsule_first.to_usize();
        let end = first.saturating_add(usize::from(character.combat_capsule_count));
        let player = self.game_entities.actor_motor().position();
        let swing = Swing {
            attacker: RuntimeGameEntities::ACTOR_SLOT,
            capsules: COMBAT_CAPSULES.get(first..end).unwrap_or(&[]),
            action: self.anim.state.action(),
            pose: PoseAt::Player,
            previous: self
                .previous_player_actor_pose
                .as_ref()
                .is_some_and(|previous| previous.clip_local() == player_pose.clip_local())
                .then_some(PoseAt::PreviousPlayer),
            position: [player.x, player.y, player.z],
            room: self.room_index,
            channel: Self::player_attack_channel(self.anim.state),
            heavy: matches!(
                self.anim.state,
                PlayerAnim::HeavyAttack | PlayerAnim::VertHeavyAttack
            ),
            crit: self.game_entities.actor().crit_attack,
        };
        // Her authored melee tell tracks the fighter she is locked on, by
        // the rule every fighter's tell follows.
        if let Some(target) = self.lock_target_position() {
            if !self
                .game_entities
                .swing_connected(RuntimeGameEntities::ACTOR_SLOT)
                && combat::melee_tell_tracks(
                    swing.capsules,
                    swing.action,
                    player_pose.pose().phase_q12(),
                )
            {
                self.game_entities.track_tell(
                    RuntimeGameEntities::ACTOR_SLOT,
                    Tell::Melee,
                    [target.x, target.y, target.z],
                    1,
                );
            }
        }
        let stats = self.swing_fighters(ctx, swing, None, prop_blockers.as_slice());
        self.finish_swing(swing, stats);
    }

    /// The swing that reached anyone, hit or parried, uses up slot
    /// `fighter`'s crit.
    // Rare (a parry, crit or knockdown): kept out of line and small.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn spend_swing_crit(&mut self, fighter: usize, stats: MeleeArcStats) {
        if stats.hits.saturating_add(stats.parries) == 0 {
            return;
        }
        if fighter == RuntimeGameEntities::ACTOR_SLOT {
            self.game_entities.actor_mut().spend_crit();
        } else {
            self.game_entities.spend_crit(fighter);
        }
    }

    /// Damage every image-card target through the shared destructible state.
    fn damage_world_destructibles_with_capsule(
        &mut self,
        start: [i32; 3],
        end: [i32; 3],
        radius: u16,
        channel: DamageChannel,
        damage: u16,
    ) -> DamageOutcome {
        let mut alive_before = 0u32;
        for index in 0..DESTRUCTIBLES.len().min(u32::BITS as usize) {
            if self.destructibles.alive(index) {
                alive_before |= 1u32 << index;
            }
        }
        let mut result = DamageOutcome::default();
        let radius = i32::from(radius);
        let capsule_min = core::array::from_fn::<_, 3, _>(|axis| {
            start[axis].min(end[axis]).saturating_sub(radius)
        });
        let capsule_max = core::array::from_fn::<_, 3, _>(|axis| {
            start[axis].max(end[axis]).saturating_add(radius)
        });
        // Image cards use their cooked visual/collision AABB.
        for object in &WORLD_OBJECTS {
            if object.kind != psx_level::world_object_kind::IMAGE_PROP
                || object.destructible == psx_level::WORLD_OBJECT_DESTRUCTIBLE_NONE
                || self
                    .destructibles
                    .hit_this_swing(usize::from(object.destructible))
            {
                continue;
            }
            let overlaps = (0..3).all(|axis| {
                capsule_min[axis] <= object.bounds_max[axis]
                    && capsule_max[axis] >= object.bounds_min[axis]
            });
            if !overlaps {
                continue;
            }
            let outcome =
                self.destructibles
                    .apply_damage(usize::from(object.destructible), channel, damage);
            if outcome.connected {
                result.connected = true;
                result.broke |= outcome.broke;
            }
        }
        if result.broke {
            for index in 0..DESTRUCTIBLES.len().min(u32::BITS as usize) {
                if alive_before & (1u32 << index) != 0 && !self.destructibles.alive(index) {
                    telemetry::debug_log("DESTRUCTIBLE BREAK");
                    self.mark_destructible_broken(index);
                    self.spawn_destructible_break_effect(index);
                }
            }
        } else if result.connected {
            telemetry::debug_log("DESTRUCTIBLE HIT");
        }
        result
    }

    fn spawn_destructible_break_effect(&mut self, index: usize) {
        let Some(record) = DESTRUCTIBLES.get(index) else {
            return;
        };
        let object_origin = WORLD_OBJECTS
            .iter()
            .find(|object| usize::from(object.destructible) == index)
            .map(|object| {
                let center = core::array::from_fn(|axis| {
                    object.bounds_min[axis].saturating_add(object.bounds_max[axis]) / 2
                });
                let largest_extent = (0..3)
                    .map(|axis| {
                        object.bounds_max[axis]
                            .saturating_sub(object.bounds_min[axis])
                            .unsigned_abs()
                    })
                    .max()
                    .unwrap_or(256);
                (center, object.room, largest_extent.clamp(192, 768) as u16)
            });
        let Some((position, room, radius)) = object_origin else {
            return;
        };
        let (core_rgb, glow_rgb, impact_rgb) = match record.damage_affinity {
            psx_level::destructible_affinity::HORIZON => {
                ([255, 184, 112], [224, 56, 24], [255, 92, 42])
            }
            psx_level::destructible_affinity::ZENITH => {
                ([196, 255, 240], [30, 128, 116], [82, 194, 178])
            }
            _ => ([255, 240, 176], [180, 96, 24], [240, 184, 72]),
        };
        let _ = self.combat_projectile_impacts.spawn_effect(
            position,
            room,
            radius,
            ProjectileVisualStyle {
                core_rgb,
                glow_rgb,
                impact_rgb,
                glow_scale_q8: 448,
                length_ticks: 1,
                trail_segments: 0,
                trail_spacing_ticks: 1,
                impact_lifetime_ticks: 32,
                crystal: false,
                // Only the emissive impact flare, no debris.
                break_fragment_count: 0,
                star_points: 0,
            },
        );
    }

    pub(super) fn report_player_melee_stats(&mut self, stats: MeleeArcStats) {
        if stats.hits > 0 {
            telemetry::counter(telemetry::counter::PLAYER_MELEE_HITS, u32::from(stats.hits));
            let event = if matches!(
                self.anim.state,
                PlayerAnim::HeavyAttack | PlayerAnim::VertHeavyAttack
            ) {
                LevelGameplaySfxEvent::HeavyHit
            } else {
                LevelGameplaySfxEvent::LightHit
            };
            self.queue_gameplay_sfx(event);
        }
        if stats.staggers > 0 {
            telemetry::debug_log("enemy poise:break");
            telemetry::counter(
                telemetry::counter::GAME_ENTITY_STAGGER_ENTERS,
                u32::from(stats.staggers),
            );
        }
        if stats.deaths > 0 {
            telemetry::counter(
                telemetry::counter::GAME_ENTITY_DEATHS,
                u32::from(stats.deaths),
            );
            self.queue_gameplay_sfx(LevelGameplaySfxEvent::EnemyDeath);
        }
    }

    /// Build the per-frame pose-override list: every live (visible)
    /// entity with a cooked visual renders at its runtime position and
    /// facing, playing its AI state's cooked clip on the state clock.
    ///
    /// Pushes into caller-owned scratch. The list is push-only and this map
    /// cooks one entity into a 64-slot capacity, so a pre-splatted array would
    /// write 1,536 bytes per call to fill one 24-byte entry; `FixedScratch`
    /// leaves the unused slots untouched.
    #[inline(never)]
    pub(super) fn game_entity_pose_overrides(
        &self,
        out: &mut psx_engine::FixedScratch<ModelInstancePoseOverride, MAX_GAME_ENTITIES>,
    ) {
        for (index, record) in GAME_ENTITIES.iter().enumerate() {
            if record.model_instance == psx_level::GAME_ENTITY_MODEL_INSTANCE_NONE
                || out.len() >= out.capacity()
                // An away fighter is frozen and its instance is not present:
                // nothing poses it.
                || (index < 64 && self.fighters_away_now & (1u64 << index) != 0)
            {
                continue;
            }
            // A final Attack tick may have transitioned the state to Recover
            // before presentation freezes. The deferred token retains the
            // exact attack clip/phase that contact evaluates, so body and
            // equipment visibly sample that same pose this tick.
            let attack = self
                .deferred_enemy_attacks
                .as_slice()
                .iter()
                .copied()
                .find(|attack| attack.entity() == index)
                .map(|attack| (attack.action(), attack.clip()));
            out.try_push(self.game_entities.pose_override(record, index, attack));
        }
    }

    /// Advance every entity's animation to this tick, before its pose is
    /// resolved: the same tracks, clocks and crossfades as hers
    /// ([`psx_game_runtime::fighter_anim`]).
    pub(super) fn update_entity_animation(&mut self, ctx: &Ctx) {
        let models = &self.models;
        let clips = &self.clips;
        let video_hz = ctx.video_hz;
        self.game_entities.update_animation(
            GAME_ENTITIES.as_slice(),
            ctx.sim_tick,
            |index, clip| {
                GAME_ENTITIES
                    .get(index)
                    .and_then(|record| MODEL_INSTANCES.get(usize::from(record.model_instance)))
                    .and_then(|instance| models.get(instance.model.to_usize()).copied().flatten())
                    .and_then(|model| model.clip(clips, ModelClipIndex(clip.clip)))
                    .map_or(0, |animation| {
                        mr::clip_duration_ticks(
                            animation,
                            video_hz,
                            clip.speed_q8,
                            clip.frame_range,
                        )
                    })
            },
        );
        // Each fighter's footsteps, by the player's footfall rule.
        let listener = self.game_entities.actor_motor().position();
        for index in 0..self.game_entities.count().min(GAME_ENTITIES.len()) {
            if self.game_entities.footfall(index)
                && self.entity_audible(index, [listener.x, listener.y, listener.z])
            {
                self.queue_gameplay_sfx(LevelGameplaySfxEvent::EnemyFootstep);
            }
        }
    }

    /// Drain the logic fire marks into example-side effects: doors
    /// toggle their linked box prop, messages open the interactable
    /// overlay, checkpoints update the in-memory checkpoint (with the
    /// same confirmation overlay the legacy interactable path showed).
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn dispatch_logic_effects(&mut self, port: &mut psx_engine::ControllerPort) {
        if !self.logic.any_fired() && !self.logic.any_refused() {
            return;
        }
        for (index, record) in LOGIC.iter().enumerate() {
            // A locked or wrong-side door was asked to open: show its message.
            if self.logic.take_refused(index) {
                self.open_logic_message(index, record);
            }
            if !self.logic.take_fired(index) {
                continue;
            }
            match record.kind {
                psx_level::logic_kind::DOOR => {
                    // A DOOR fire mark only exists when the door state
                    // actually changed (same-state uses return before
                    // marking), so every drain here is one activation.
                    telemetry::counter(telemetry::counter::LOGIC_DOOR_ACTIVATIONS, 1);
                    let open = self.logic.door_open(index);
                    // A sliding door's box follows `sync_sliding_doors`, tick
                    // by tick; only an instant door flips here.
                    if record.link != psx_level::LOGIC_LINK_NONE
                        && self.logic.door_slide(LOGIC.as_slice(), index).is_none()
                    {
                        self.box_props.set_door_open(usize::from(record.link), open);
                    }
                    self.door_motion_sound(index, open);
                    self.store_persistent_logic(index);
                }
                psx_level::logic_kind::SWITCH => self.store_persistent_logic(index),
                psx_level::logic_kind::MESSAGE => {
                    self.open_logic_message(index, record);
                }
                psx_level::logic_kind::CHECKPOINT => {
                    // Fired by prompt, trigger-volume touch, or relay
                    // chain alike: the fire mark is the one dispatch
                    // surface, so trigger-to-checkpoint chains land
                    // here exactly like interact prompts.
                    self.set_checkpoint(
                        RuntimeCheckpoint {
                            room: self.room_index,
                            position: self.game_entities.actor_motor().position(),
                            yaw: self.game_entities.actor_motor().yaw(),
                            checkpoint_id: interactable_for_logic(index)
                                .map(|interactable| interactable.checkpoint_id)
                                .unwrap_or(""),
                        },
                        port,
                    );
                    self.open_logic_message(index, record);
                }
                // Graph plumbing (trigger volumes, relays,
                // multisource gates) has no presentation effect.
                _ => {}
            }
        }
    }

    /// The hazard volumes on her this tick ([`Self::land_hazards`]); the
    /// volume leaves her alone while `exempt` (i-frames, dying).
    pub(super) fn apply_logic_hazard_damage(&mut self, ctx: &Ctx, player: [i32; 3], exempt: bool) {
        if !crate::generated::ANY_HAZARD {
            return;
        }
        let height = self.motor_config().height;
        let liquid = self.fighter_liquid_damage(ctx, player, height, exempt);
        if liquid != 0 {
            telemetry::counter(telemetry::counter::PLAYER_LIQUID_DAMAGE_EVENTS, 1);
        }
        self.land_hazards(ctx, RuntimeGameEntities::ACTOR_SLOT, player, exempt, liquid);
    }

    /// The harmful liquid one fighter stands in this tick, by one rule for
    /// every fighter: the damage of the contents at its body (on the
    /// liquid's own interval), none while `exempt` (i-frames, dying).
    fn fighter_liquid_damage(&self, ctx: &Ctx, position: [i32; 3], height: i32, exempt: bool) -> u16 {
        if exempt {
            return 0;
        }
        self.world_rt
            .as_ref()
            .and_then(|world_rt| {
                world_rt.player_contents(RoomPoint::new(position[0], position[1], position[2]), height)
            })
            .map_or(0, |sample| hazard_damage(sample.contents, ctx.sim_tick.as_u32()))
    }

    /// The hazards on every entity, by the rule her actor slot takes them
    /// ([`Self::land_hazards`]), plus the harmful liquid it stands in. An
    /// invulnerable entity is passed through.
    pub(super) fn apply_entity_hazards(&mut self, ctx: &Ctx) {
        // A level with no hazard volume and no liquid cooks the pass out.
        if !crate::generated::ANY_HAZARD {
            return;
        }
        let count = self.game_entities.count().min(GAME_ENTITIES.len());
        for index in 0..count {
            if self.game_entities.state(index) == GameEntityState::Dead
                || self.game_entities.away(index)
            {
                continue;
            }
            let record = &GAME_ENTITIES[index];
            let position = self.game_entities.position(index);
            let exempt = self
                .game_entities
                .invulnerable(GAME_ENTITIES.as_slice(), index);
            let height = i32::from(self.game_entities.body(record).height);
            let liquid = self.fighter_liquid_damage(ctx, position, height, exempt);
            self.land_hazards(ctx, index, position, exempt, liquid);
        }
    }

    /// One fighter's hazards this tick, the one rule every fighter takes:
    /// the hazard volumes on its own clock
    /// ([`psx_game_runtime::logic::LogicRuntime::touch_hazards`]) plus
    /// `liquid` damage, landed as world damage on its own stance through the
    /// one landing; a volume that shoves also strikes its posture for the
    /// damage it dealt, and the shove starts unless the hit killed.
    fn land_hazards(
        &mut self,
        ctx: &Ctx,
        slot: usize,
        position: [i32; 3],
        exempt: bool,
        liquid: u16,
    ) {
        if !crate::generated::ANY_HAZARD {
            return;
        }
        let Some(clock) = self.game_entities.hazard_clock_mut(slot) else {
            return;
        };
        let touch = self.logic.touch_hazards(
            LOGIC.as_slice(),
            position,
            exempt,
            ctx.sim_tick.as_u32(),
            clock,
        );
        let damage = touch.damage.saturating_add(liquid);
        if damage != 0 {
            let strike = Strike {
                attacker: CombatantId::WORLD,
                kind: HitKind::Hazard,
                channel: self.game_entities.stance(slot),
                damage,
                poise: if touch.push.is_some() {
                    touch.damage
                } else {
                    0
                },
                crit: false,
            };
            if self.land_on_fighter(ctx, slot, strike, None, false).died {
                return;
            }
        }
        if let Some(push) = touch.push {
            self.game_entities.start_hazard_push(slot, push);
        }
    }

    /// One tick of her hazard shove, by every fighter's rule
    /// ([`GameEntities::advance_hazard_push`]), through the same collision
    /// the motor uses, so a wall stops it and nothing is pushed out of the
    /// world.
    pub(super) fn advance_hazard_push(
        &mut self,
        blockers: &[CharacterCollisionCylinder],
        aabb_blockers: &[CharacterCollisionAabb],
    ) {
        let actor = RuntimeGameEntities::ACTOR_SLOT;
        if self.game_entities.hazard_push(actor).left == 0 {
            return;
        }
        if self.death_ticks_remaining != 0 {
            self.game_entities.clear_hazard_push(actor);
            return;
        }
        let config = self.motor_config();
        let mut world_rt = self.world_rt.as_mut();
        let destructibles = &self.destructibles;
        self.game_entities
            .advance_hazard_push(actor, |start, dx, dz| {
                world_rt
                    .as_mut()?
                    .commit_body_step(
                        RoomPoint::new(start[0], start[1], start[2]),
                        dx,
                        dz,
                        config.radius,
                        config.height,
                        destructibles,
                        blockers,
                        aabb_blockers,
                    )
                    .ok()
                    .map(|step| [step.position.x, step.position.y, step.position.z])
            });
    }

    /// The state of the level's visual switches, bit `i` set while slot `i`
    /// is on: the logic record each slot names, read as on or off. Planes,
    /// meshes and emitters cooked against a slot follow it.
    pub(super) fn visual_switch_mask(&self) -> u32 {
        let mut mask = 0u32;
        for (slot, link) in VISUAL_LINKS
            .iter()
            .enumerate()
            .take(psx_grid::visual_switch::SLOTS)
        {
            if self.logic.is_on(LOGIC.as_slice(), usize::from(*link)) {
                mask |= 1 << slot;
            }
        }
        mask
    }

    /// Message overlay for a fired MESSAGE/CHECKPOINT record, through
    /// the same text table + fallbacks the legacy interactable path
    /// used (the cook shares the message index between both records).
    #[cfg_attr(target_arch = "mips", optimize(size))]
    fn open_logic_message(&mut self, index: usize, record: &psx_level::LevelLogicRecord) {
        if self.poi_messages.active().is_some() {
            // POI/world messages are deliberately non-modal. Refuse a hidden
            // legacy overlay rather than letting it freeze gameplay behind the
            // visible Archive panel or consume the same Cross press.
            return;
        }
        let (title, body) = match interactable_for_logic(index) {
            Some(interactable) => interactable_message_text(interactable),
            None => match INTERACTABLE_MESSAGES.get(record.message as usize) {
                Some(message) => (message.title, message.body),
                None => return,
            },
        };
        self.message_overlay = Some(RuntimeMessageOverlay { title, body });
        self.set_poi_page_type_frame(0);
    }

    /// Push every DOOR record's current open state onto its linked
    /// box prop (gameplay init and future checkpoint respawns:
    /// START_ON doors begin open without a fire event).
    pub(super) fn sync_door_box_props(&mut self) {
        for (index, record) in LOGIC.iter().enumerate() {
            if record.kind == psx_level::logic_kind::DOOR
                && record.link != psx_level::LOGIC_LINK_NONE
                && self.logic.door_slide(LOGIC.as_slice(), index).is_none()
            {
                let open = self.logic.door_open(index);
                self.box_props.set_door_open(usize::from(record.link), open);
            }
        }
        self.sync_sliding_doors();
    }

    /// Move the box of every door with motion (and every lever handle) to where the logic runtime has
    /// it: drawn and solid at its offset until it has fully cleared, then
    /// hidden and passable (and solid again the moment it starts closing).
    pub(super) fn sync_sliding_doors(&mut self) {
        for (index, record) in LOGIC.iter().enumerate() {
            let is_door = record.kind == psx_level::logic_kind::DOOR;
            if !(is_door || record.kind == psx_level::logic_kind::SWITCH)
                || record.link == psx_level::LOGIC_LINK_NONE
            {
                continue;
            }
            if let Some(slide) = self.logic.door_slide(LOGIC.as_slice(), index) {
                let link = usize::from(record.link);
                self.box_props.set_door_slide(link, slide.offset);
                // A door's box is gone once it has cleared the doorway; a
                // lever's handle stays where it slid to.
                if is_door {
                    self.box_props.set_door_open(link, slide.cleared);
                }
            }
        }
    }

    /// SEAM: the sound of door `index` starting to open or close. Left empty
    /// on purpose: a door cue needs a `LevelGameplaySfxEvent` pair, their
    /// cook-time bindings to UI-scene audio and authored sounds, none of which
    /// exist yet. Call sites are already one per commanded state change, so
    /// filling this in touches nothing else.
    #[inline(always)]
    fn door_motion_sound(&mut self, index: usize, opening: bool) {
        let _ = (index, opening);
    }
}

/// The interactable paired with logic record `logic_index`, if any.
pub(super) fn interactable_for_logic(logic_index: usize) -> Option<&'static InteractableRecord> {
    let logic_index = u16::try_from(logic_index).ok()?;
    INTERACTABLES
        .iter()
        .find(|interactable| interactable.logic == logic_index)
}

#[cfg(test)]
mod encounter_audio_tests {
    use super::{weapon_swing_sfx, CombatMusicState, LevelGameplaySfxEvent};

    #[test]
    fn swing_cues_follow_attack_weight_for_both_stances_and_enemies() {
        use psx_level::CharacterAnimationAction as Action;
        for action in [
            Action::LightAttack,
            Action::VertLightAttack,
            Action::AltLightAttack,
        ] {
            assert_eq!(
                weapon_swing_sfx(action),
                Some(LevelGameplaySfxEvent::LightWeaponSwing)
            );
        }
        for action in [
            Action::HeavyAttack,
            Action::VertHeavyAttack,
            Action::AltHeavyAttack,
            Action::ComboAttack,
            Action::VertComboAttack,
            Action::AltComboAttack,
        ] {
            assert_eq!(
                weapon_swing_sfx(action),
                Some(LevelGameplaySfxEvent::HeavyWeaponSwing)
            );
        }
        for action in [Action::Idle, Action::Walk, Action::RangedAttack] {
            assert_eq!(weapon_swing_sfx(action), None);
        }
    }

    #[test]
    fn encounter_start_is_once_per_fight_despite_brief_losses_of_aggro() {
        let mut gate = CombatMusicState::default();
        assert_eq!(gate.tick(false), None);
        assert_eq!(gate.tick(true), Some(true));
        for _ in 0..600 {
            assert_eq!(gate.tick(true), None);
        }
        for _ in 0..179 {
            assert_eq!(gate.tick(false), None);
        }
        assert_eq!(gate.tick(true), None);
        for _ in 0..179 {
            assert_eq!(gate.tick(false), None);
        }
        assert_eq!(gate.tick(false), Some(false));
        assert_eq!(gate.tick(false), None);
        assert_eq!(gate.tick(true), Some(true));
    }
}

/// A swing as anyone watching it reads it, whichever fighter makes it: the
/// attack clip and how far into it the swing is, the action, the fighter's
/// authored hitboxes, and the contact ticks its record times when it has no
/// authored hitbox for the action.
#[derive(Clone, Copy)]
pub(super) struct SwingClock {
    pub(super) animation: Option<psx_asset::Animation<'static>>,
    pub(super) local_tick: u32,
    pub(super) looping: bool,
    pub(super) speed_q8: u16,
    pub(super) frame_range: psx_level::CharacterActionFrameRange,
    pub(super) action: CharacterAnimationAction,
    pub(super) capsules: &'static [psx_level::CombatCapsuleRecord],
    pub(super) record_contact: Option<(u16, u16)>,
}

/// When the blade of `swing` (posed by `attacker_pose`) first meets a
/// defender with `defender_capsules` posed by `defender_pose`, for a parry
/// window of `parry_ticks`: [`combat::predicted_contact_tick`] over the ticks
/// from the swing's first authored contact frame to the last one a swap made
/// now still parries, `parry_ticks - 1`
/// ([`psx_game_runtime::combat_policy::parry_swap`]). Unread (the controller
/// keeps the swing's own clock) with no swing, no contact coming within the
/// window yet, or a side without authored capsules or a pose.
fn predicted_contact(
    swing: Option<&SwingClock>,
    attacker_pose: Option<&psx_game_runtime::actor_pose::ActorPoseSnapshot>,
    defender_capsules: &[psx_level::CombatCapsuleRecord],
    defender_pose: Option<&psx_game_runtime::actor_pose::ActorPoseSnapshot>,
    parry_ticks: u16,
    video_hz: VideoHz,
) -> PredictedContact {
    let Some(swing) = swing else {
        return PredictedContact::Unread;
    };
    let Some(from) = swing_ticks_to_contact(swing, video_hz).filter(|&t| t < parry_ticks) else {
        return PredictedContact::Unread;
    };
    let authored = combat::authored_contact_frames(swing.capsules, swing.action).is_some()
        && defender_capsules
            .iter()
            .any(|record| record.flags & psx_level::combat_capsule_flags::HURTBOX != 0);
    let (true, Some(animation), Some(attacker_pose), Some(defender_pose)) =
        (authored, swing.animation, attacker_pose, defender_pose)
    else {
        return PredictedContact::Unread;
    };
    let phase_at = |k: u16| {
        mr::animation_phase_at_tick_q12(
            animation,
            swing.local_tick.saturating_add(u32::from(k)),
            video_hz,
            swing.looping,
            swing.speed_q8,
            swing.frame_range,
        )
    };
    match combat::predicted_contact_tick(
        swing.capsules,
        swing.action,
        attacker_pose,
        phase_at,
        defender_capsules,
        defender_pose,
        from,
        parry_ticks - 1,
    ) {
        Some(ticks) => PredictedContact::In(ticks),
        None => PredictedContact::Outside,
    }
}

/// Ticks until `swing` first makes contact: from its first authored
/// contact frame when it has one, from its record's contact ticks
/// otherwise ([`combat::ticks_to_contact`]). The one reading the enemy
/// makes of the player's swing and the duel bot makes of the enemy's.
pub(super) fn swing_ticks_to_contact(swing: &SwingClock, video_hz: VideoHz) -> Option<u16> {
    if let (Some(animation), Some((first, last))) = (
        swing.animation,
        combat::authored_contact_frames(swing.capsules, swing.action),
    ) {
        let at = |tick: u32| {
            mr::animation_phase_at_tick_q12(
                animation,
                tick,
                video_hz,
                swing.looping,
                swing.speed_q8,
                swing.frame_range,
            )
        };
        let now = at(swing.local_tick);
        let next = at(swing.local_tick.saturating_add(1));
        return combat::ticks_to_contact(first, last, now, next.saturating_sub(now));
    }
    let (first, last) = swing.record_contact?;
    combat::ticks_to_contact(first, last, swing.local_tick << 12, 1 << 12)
}
