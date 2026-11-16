//! Phase-3 gameplay-layer glue (docs/game-runtime-plan.md): the
//! example-side movement backend and effect dispatch the runtime
//! crate's `GameEntities`/`LogicRuntime` leave to the owning game.
//!
//! - [`SceneEntityMover`] backs entity Patrol/Aggro steps with the active
//!   world's movement solver (`commit_body_step`). A grid room whose collision
//!   is not resident refuses movement (the entity keeps thinking; it walks
//!   again when the room streams in).
//! - Effect dispatch drains `LogicRuntime::take_fired` marks into the
//!   example's presentation: DOOR records toggle their linked box
//!   prop (draw + collision), MESSAGE records open the interactable
//!   message overlay, CHECKPOINT records update the in-memory
//!   checkpoint -- the same UI paths the legacy interactable flow
//!   used, now driven off LOGIC records (the cook pairs them 1:1).
//! - Player melee resolution (the combat slice) maps the locked
//!   attack animation onto the crate's melee arc: the cooked weapon
//!   spec sizes the arc and its hitbox frame windows gate WHEN the
//!   swing is live, using the exact animation-phase math the render
//!   path plays, so contact frames match what the player sees.

use super::*;
use psx_game_runtime::combat::{self, MeleeArc, WorldCombatCapsule};
use psx_game_runtime::combatant::stance_scaled;
use psx_game_runtime::destructibles::{DamageChannel, DamageOutcome};
use psx_game_runtime::entities::{DeferredGameEntityAttack, GameEntityState, MeleeArcStats};
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

#[derive(Clone, Copy)]
struct ActivePlayerCapsule {
    capsule: WorldCombatCapsule,
    previous_capsule: Option<WorldCombatCapsule>,
    damage: u16,
    poise_damage: u16,
}

impl ActivePlayerCapsule {
    const EMPTY: Self = Self {
        capsule: WorldCombatCapsule::EMPTY,
        previous_capsule: None,
        damage: 0,
        poise_damage: 0,
    };

    fn aabb_overlaps(self, target: &WorldCombatCapsule) -> bool {
        match self.previous_capsule {
            Some(previous) => {
                combat::combat_capsule_motion_aabbs_overlap(&previous, &self.capsule, target)
            }
            None => combat::combat_capsule_aabbs_overlap(&self.capsule, target),
        }
    }

    fn overlaps(self, target: &WorldCombatCapsule) -> bool {
        match self.previous_capsule {
            Some(previous) => {
                combat::combat_capsule_motion_overlaps(&previous, &self.capsule, target)
            }
            None => combat::combat_capsules_overlap(&self.capsule, target),
        }
    }
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
}

impl SceneEntityMover<'_> {
    #[allow(clippy::too_many_arguments)]
    fn step_inner(
        &mut self,
        entity: usize,
        room: RoomIndex,
        position: [i32; 3],
        dx: i32,
        dz: i32,
        radius: i32,
        height: i32,
        exact_direction: bool,
    ) -> [i32; 3] {
        // Body blockers: other entities' cooked instances at their
        // LIVE positions (self excluded), plus the player's capsule.
        let own_instance = GAME_ENTITIES
            .get(entity)
            .map(|record| record.model_instance)
            .unwrap_or(psx_level::GAME_ENTITY_MODEL_INSTANCE_NONE);
        let mut cylinders =
            psx_engine::FixedScratch::<CharacterCollisionCylinder, MAX_COLLISION_CYLINDERS>::new();
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
            &mut cylinders,
        );

        let start = RoomPoint::new(position[0], position[1], position[2]);
        let mut aabbs =
            psx_engine::FixedScratch::<CharacterCollisionAabb, MAX_STATIC_PROP_AABB_BLOCKERS>::new(
            );
        {
            let Some(world_rt) = self.world_rt.as_deref_mut() else {
                // No world cooked yet (the placeholder manifest).
                return position;
            };
            let Some(_) = self.box_props.collect_collision_blockers_checked_into(
                BOX_PROPS.as_slice(),
                room,
                &mut aabbs,
            ) else {
                return position;
            };
            let Some(_) =
                psx_game_runtime::arch_props::collect_arch_prop_collision_blockers_checked_into(
                    ARCH_PROPS.as_slice(),
                    ARCH_PROP_COLLISIONS.as_slice(),
                    room,
                    &mut aabbs,
                )
            else {
                return position;
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
                return position;
            };
            let step = if exact_direction {
                world_rt.commit_body_direction(
                    start,
                    dx,
                    dz,
                    radius,
                    height,
                    self.destructibles,
                    cylinders.as_slice(),
                    aabbs.as_slice(),
                )
            } else {
                world_rt.commit_body_step(
                    start,
                    dx,
                    dz,
                    radius,
                    height,
                    self.destructibles,
                    cylinders.as_slice(),
                    aabbs.as_slice(),
                )
            }
            .expect("grid entity trace failed");
            [step.position.x, step.position.y, step.position.z]
        }
    }
}

impl psx_game_runtime::entities::GameEntityMover for SceneEntityMover<'_> {
    fn step(
        &mut self,
        entity: usize,
        room: RoomIndex,
        position: [i32; 3],
        dx: i32,
        dz: i32,
        radius: i32,
        height: i32,
    ) -> [i32; 3] {
        self.step_inner(entity, room, position, dx, dz, radius, height, false)
    }

    fn step_direction(
        &mut self,
        entity: usize,
        room: RoomIndex,
        position: [i32; 3],
        dx: i32,
        dz: i32,
        radius: i32,
        height: i32,
    ) -> [i32; 3] {
        self.step_inner(entity, room, position, dx, dz, radius, height, true)
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
    pub(super) pose: Option<psx_game_runtime::actor_pose::ActorPoseSnapshot>,
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
    /// The same body-radius source the entity tick input uses, so the
    /// legacy fallback arc keeps its Character-derived reach.
    radius: i32,
    height: i32,
    /// A fighter's hit passes through her: i-frames past the parry window.
    passes_through: bool,
    /// A fighter's hit is parried: the parry window of her swap.
    parries: bool,
    capsules: &'static [psx_level::CombatCapsuleRecord],
    pose: Option<psx_game_runtime::actor_pose::ActorPoseSnapshot>,
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

/// What the player took this tick. Damage and posture land together after
/// the projectile tick, as one reaction.
#[derive(Clone, Copy)]
struct PlayerHitsTaken {
    hits: u16,
    /// Untyped melee damage from enemy swings.
    melee_damage: u16,
    poise: u16,
    horizon_shots: u16,
    zenith_shots: u16,
    /// A bolt landed inside the player's readable shot opening.
    shot_opening: bool,
    /// The fighter whose hit landed last this tick ([`CombatantId::WORLD`]
    /// when none did).
    attacker: CombatantId,
    /// An enemy swing that landed this tick was a crit.
    crit: bool,
}

impl PlayerHitsTaken {
    const NONE: Self = Self {
        hits: 0,
        melee_damage: 0,
        poise: 0,
        horizon_shots: 0,
        zenith_shots: 0,
        shot_opening: false,
        attacker: CombatantId::WORLD,
        crit: false,
    };
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
const CRIT_BOLT_RGB: [u8; 3] = [247, 198, 99];

impl Playtest {
    /// Resolve every attack of this tick after the player and instance pose
    /// snapshots have been frozen, always in this order: the player's
    /// projectile release, every enemy attack token (a projectile release or
    /// a melee contact on the player), the projectile tick, the hits the
    /// player took, and last the player's own swing on the entities. The
    /// order is load-bearing: a bolt that staggers an enemy this tick lands
    /// before the player's blade reaches it, and a hit reaction cancels the
    /// player's swing before it can connect.
    pub(super) fn resolve_combat(&mut self, ctx: &Ctx) {
        self.combat_projectile_impacts.tick();
        self.release_player_projectile(ctx);
        if !self.deferred_enemy_attacks.is_empty() || !self.combat_projectiles.is_empty() {
            self.resolve_attacks_on_player(ctx);
        }
        self.resolve_player_melee(ctx);
    }

    /// The enemy attack tokens and every live projectile against this tick's
    /// frozen player, then the hits she took.
    fn resolve_attacks_on_player(&mut self, ctx: &Ctx) {
        let player = self.player_defence(ctx);
        let mut prop_blockers =
            psx_engine::FixedScratch::<CharacterCollisionAabb, MAX_STATIC_PROP_AABB_BLOCKERS>::new(
            );
        let prop_blockers_valid = self.world_rt.is_none()
            || self
                .collect_static_prop_aabb_blockers_checked_into(&mut prop_blockers)
                .is_some();
        let mut taken = PlayerHitsTaken::NONE;
        self.resolve_enemy_attacks(
            ctx,
            &player,
            prop_blockers.as_slice(),
            prop_blockers_valid,
            &mut taken,
        );
        let mut impacts = ProjectileImpacts::<MAX_PROJECTILE_IMPACTS>::new();
        self.tick_combat_projectiles(
            ctx,
            &player,
            prop_blockers.as_slice(),
            prop_blockers_valid,
            &mut impacts,
            &mut taken,
        );
        self.land_player_hits(ctx, &player, &impacts, taken);
    }

    fn player_defence(&self, ctx: &Ctx) -> PlayerDefence {
        let defender = self.player_defender(ctx);
        let player = self.motor.position();
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
            capsules: self
                .character
                .as_ref()
                .and_then(|character| {
                    let first = character.combat_capsule_first.to_usize();
                    let end = first.saturating_add(usize::from(character.combat_capsule_count));
                    COMBAT_CAPSULES.get(first..end)
                })
                .unwrap_or(&[]),
            pose: self
                .player_actor_pose
                .as_ref()
                .map(|snapshot| *snapshot.pose()),
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
        taken: &mut PlayerHitsTaken,
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
            let first = entity.combat_capsule_first.to_usize();
            let end = first.saturating_add(usize::from(entity.combat_capsule_count));
            let attacker_capsules = COMBAT_CAPSULES.get(first..end).unwrap_or(&[]);
            let attacker_snapshot = self
                .instance_actor_poses
                .get(entity.model_instance as usize)
                .copied()
                .flatten();
            let attacker_pose = attacker_snapshot.map(|snapshot| snapshot.pose());
            if attack.is_ranged() {
                // An authored ranged attack never falls through to the legacy
                // body-radius melee arc, including frames outside its release
                // window and pool-full retries.
                self.release_enemy_projectiles(ctx, attack, attacker_capsules, attacker_pose);
                continue;
            }
            if player.passes_through {
                continue;
            }
            let previous_attacker_pose = self
                .previous_instance_actor_poses
                .get(entity.model_instance as usize)
                .copied()
                .flatten()
                .filter(|old| {
                    attacker_snapshot
                        .is_some_and(|current| old.clip_local() == current.clip_local())
                })
                .map(|snapshot| snapshot.pose());
            let (contact, window_mask) = combat::resolve_authored_actor_contact_swept_pending(
                attacker_capsules,
                attack.action(),
                attacker_pose,
                previous_attacker_pose,
                player.capsules,
                player.pose,
                self.game_entities
                    .deferred_melee_hit_mask(attack)
                    .unwrap_or(u16::MAX),
            );
            let damage = match contact {
                combat::AuthoredActorContact::Hit {
                    damage,
                    poise_damage,
                } => Some((damage, poise_damage)),
                combat::AuthoredActorContact::Miss => None,
                combat::AuthoredActorContact::FallbackRequired => self
                    .game_entities
                    .deferred_attack_legacy_arc_hits(
                        GAME_ENTITIES.as_slice(),
                        attack,
                        player.position,
                        player.radius,
                    )
                    .then_some((entity.touch_damage, entity.touch_damage.max(20))),
            };
            let Some((damage, poise_damage)) = damage else {
                continue;
            };
            // World occlusion is authoritative for BOTH the authored capsule
            // and the legacy arc outcome: a closed door between the frozen
            // attacker position and the player blocks the connection. The
            // token is deliberately not consumed, mirroring an i-frame whiff.
            if !prop_blockers_valid
                || !self.melee_line_clear(attack.position(), player.position, prop_blockers)
            {
                continue;
            }
            let (damage, poise_damage) = self.game_entities.crit_scaled(
                GAME_ENTITIES.as_slice(),
                attack.entity(),
                damage,
                poise_damage,
            );
            let connected = if window_mask == 0 {
                self.game_entities.connect_deferred_attack(attack)
            } else {
                self.game_entities
                    .connect_deferred_melee_window(attack, window_mask)
            };
            if !connected {
                continue;
            }
            // The swing that reached her uses up the crit, parried or not.
            let crit = self.game_entities.spend_crit(attack.entity());
            if crit {
                self.duel_log_event(ctx, 1, duel::DUEL_EVENT_CRIT);
            }
            let attacker = CombatantId::entity(attack.entity());
            if player.parries {
                self.parry_on_player(
                    Strike {
                        attacker,
                        kind: HitKind::Melee,
                        channel: self.player.stance.active(),
                        damage,
                        poise: poise_damage,
                    },
                    ctx,
                );
                continue;
            }
            {
                self.game_entities.gain_melee_energy(
                    entity,
                    attack.entity(),
                    self.game_entities.attack_kind(attack.entity()) == 1,
                );
                taken.hits = taken.hits.saturating_add(1);
                taken.melee_damage = taken.melee_damage.saturating_add(damage);
                taken.poise = taken.poise.saturating_add(poise_damage);
                taken.attacker = attacker;
                taken.crit |= crit;
            }
        }
    }

    /// Advance every live projectile against the player's hurtboxes (unless
    /// she is invulnerable) and every live entity's. A bolt on an entity
    /// lands at once; a bolt on the player is summed into `taken`.
    fn tick_combat_projectiles<const I: usize>(
        &mut self,
        ctx: &Ctx,
        player: &PlayerDefence,
        prop_blockers: &[CharacterCollisionAabb],
        prop_blockers_valid: bool,
        impacts: &mut ProjectileImpacts<I>,
        taken: &mut PlayerHitsTaken,
    ) {
        let mut projectile_targets = psx_engine::FixedScratch::<
            ProjectileTarget,
            { MAX_GAME_ENTITIES + psx_level::MAX_CHARACTER_COMBAT_CAPSULES },
        >::new();
        if !player.passes_through {
            if let Some(pose) = player.pose {
                for record in player
                    .capsules
                    .iter()
                    .take(psx_level::MAX_CHARACTER_COMBAT_CAPSULES)
                {
                    if record.flags & psx_level::combat_capsule_flags::HURTBOX == 0 {
                        continue;
                    }
                    let Some(hurtbox) = combat::transform_actor_combat_capsule(record, pose) else {
                        continue;
                    };
                    let _ = projectile_targets.try_push(ProjectileTarget {
                        target: PLAYER_PROJECTILE_TARGET,
                        team: CombatTeam::Player,
                        room: self.room_index,
                        hurtbox,
                    });
                }
            }
            if projectile_targets.is_empty() && player.radius > 0 && player.height > 0 {
                let radius = player.radius.min(i32::from(u16::MAX)) as u16;
                let segment_start_y = player.position[1].saturating_add(player.radius);
                let segment_end_y = player.position[1]
                    .saturating_add(player.height)
                    .saturating_sub(player.radius)
                    .max(segment_start_y);
                let _ = projectile_targets.try_push(ProjectileTarget {
                    target: PLAYER_PROJECTILE_TARGET,
                    team: CombatTeam::Player,
                    room: self.room_index,
                    hurtbox: WorldCombatCapsule {
                        start: [player.position[0], segment_start_y, player.position[2]],
                        end: [player.position[0], segment_end_y, player.position[2]],
                        radius,
                    },
                });
            }
        }
        // Use the primary animated hurtbox for projectile contact. A legacy
        // actor without one retains its body capsule; authoring failures do
        // not invent a larger target. Storage stays bounded at the actor limit.
        for (index, record) in GAME_ENTITIES
            .iter()
            .enumerate()
            .take(self.game_entities.count())
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
            let p = self.game_entities.position(index);
            let r = i32::from(record.radius).min(i32::from(record.height) / 2);
            let fallback = WorldCombatCapsule {
                start: [p[0], p[1] + r, p[2]],
                end: [p[0], p[1] + i32::from(record.height) - r, p[2]],
                radius: r.max(1) as u16,
            };
            let first = record.combat_capsule_first.to_usize();
            let authored = COMBAT_CAPSULES
                .get(first..first + usize::from(record.combat_capsule_count))
                .unwrap_or(&[])
                .iter()
                .find(|c| c.flags & psx_level::combat_capsule_flags::HURTBOX != 0);
            let hurtbox = if let Some(capsule) = authored {
                let Some(pose) = self
                    .instance_actor_poses
                    .get(usize::from(record.model_instance))
                    .copied()
                    .flatten()
                else {
                    continue;
                };
                let Some(hurtbox) = combat::transform_actor_combat_capsule(capsule, pose.pose())
                else {
                    continue;
                };
                hurtbox
            } else {
                fallback
            };
            let _ = projectile_targets.try_push(ProjectileTarget {
                target: index as u16,
                team: CombatTeam::Enemy,
                room: record.room,
                hurtbox,
            });
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
            if target == PLAYER_PROJECTILE_TARGET {
                let attacker = if impact.owner == NO_PROJECTILE_OWNER {
                    CombatantId::WORLD
                } else {
                    CombatantId::entity(usize::from(impact.owner))
                };
                if player.parries {
                    // A world bolt (no owner) cannot be parried; inside the
                    // parry window it is spent on her i-frames.
                    if attacker != CombatantId::WORLD {
                        self.parry_on_player(
                            Strike {
                                attacker,
                                kind: HitKind::Projectile,
                                channel: projectile_vitality_channel(impact.damage_channel),
                                damage: impact.damage,
                                poise: impact.poise_damage,
                            },
                            ctx,
                        );
                    }
                    continue;
                }
                taken.attacker = attacker;
                telemetry::debug_log("enemy projectile:player-hit");
                taken.hits = taken.hits.saturating_add(1);
                let exposed = self.player_shot_opening();
                taken.shot_opening |= exposed && self.player.flow.can_interrupt();
                // The tick's posture lands summed and unscaled, so each bolt
                // takes the stance rule here, by its own channel.
                let poise = self.player.flow.shot_poise(
                    impact.poise_damage,
                    self.player_stats.poise,
                    exposed,
                );
                let (_, poise) = stance_scaled(
                    &self.player_stats,
                    self.player
                        .opposes(projectile_vitality_channel(impact.damage_channel)),
                    0,
                    poise,
                );
                taken.poise = taken.poise.saturating_add(poise);
                if exposed && self.player.flow.can_interrupt() {
                    telemetry::debug_log("flow:player-shot-interrupt");
                }
                match impact.damage_channel {
                    psx_game_runtime::projectiles::ProjectileDamageChannel::Horizon => {
                        taken.horizon_shots = taken.horizon_shots.saturating_add(impact.damage);
                    }
                    psx_game_runtime::projectiles::ProjectileDamageChannel::Zenith => {
                        taken.zenith_shots = taken.zenith_shots.saturating_add(impact.damage);
                    }
                }
            } else {
                self.land_shot_on_entity(ctx, usize::from(target), impact);
            }
        }
    }

    /// One player bolt on entity `index`, through the entity's hit path.
    fn land_shot_on_entity(
        &mut self,
        ctx: &Ctx,
        index: usize,
        impact: &psx_game_runtime::projectiles::ProjectileImpact,
    ) {
        let Some(record) = GAME_ENTITIES.get(index) else {
            return;
        };
        let channel = match impact.damage_channel {
            psx_game_runtime::projectiles::ProjectileDamageChannel::Horizon => {
                VitalityChannelId::One
            }
            psx_game_runtime::projectiles::ProjectileDamageChannel::Zenith => {
                VitalityChannelId::Two
            }
        };
        let applied = self.game_entities.scaled_stance_damage(
            GAME_ENTITIES.as_slice(),
            index,
            channel,
            impact.damage,
        );
        let opening = self
            .game_entities
            .shot_opening(GAME_ENTITIES.as_slice(), index);
        let outcome = self.game_entities.apply_projectile_hit(
            GAME_ENTITIES.as_slice(),
            index,
            channel,
            impact.damage,
            impact.poise_damage,
        );
        if outcome.parried {
            self.entity_parried_player(index, ctx);
            return;
        }
        self.player.flow.shot_hit(opening && outcome.staggered);
        if opening && outcome.staggered {
            telemetry::debug_log("flow:enemy-shot-interrupt");
            self.duel_log_flow(ctx, 0);
        }
        let now = self.gameplay_tick(ctx.sim_tick);
        if outcome.connected {
            self.damage_numbers.spawn(
                struck_actor_anchor(self.game_entities.position(index), record.height),
                record.room,
                applied,
                if impact.visual.impact_rgb == CRIT_BOLT_RGB {
                    DamageNumberChannel::Crit
                } else if channel == VitalityChannelId::Two {
                    DamageNumberChannel::Zenith
                } else {
                    DamageNumberChannel::Horizon
                },
                now,
            );
            telemetry::debug_log("player projectile:hit");
        }
        if outcome.died {
            self.souls.award(record.soul_value, now.as_u32());
        }
    }

    /// Land what the player took this tick: enemy melee as one untyped hit,
    /// then the bolts by channel, then the summed posture damage as one
    /// reaction.
    fn land_player_hits<const I: usize>(
        &mut self,
        ctx: &Ctx,
        player: &PlayerDefence,
        impacts: &ProjectileImpacts<I>,
        taken: PlayerHitsTaken,
    ) {
        let gameplay_now = self.gameplay_tick(ctx.sim_tick);
        let anchor = player_hit_anchor(player.position, player.height.max(0) as u16);
        if taken.melee_damage > 0 {
            // Legacy enemy attacks are not axis-authored yet, so Horizon is
            // their deterministic migration channel and excess damage spills
            // into Zenith. Shell reduction is applied before routing; only
            // emptying BOTH pools arms the existing shared death sequence.
            let died = self.hazard_death_ticks_remaining == 0
                && self.player_take_damage(
                    taken.attacker,
                    HitKind::Melee,
                    None,
                    taken.melee_damage,
                );
            // Damage taken floats off the player, not off whoever threw
            // it, and it carries the channel it drained -- Horizon here,
            // matching the migration routing above.
            self.damage_numbers.spawn(
                anchor,
                self.room_index,
                taken.melee_damage,
                if taken.crit {
                    DamageNumberChannel::Crit
                } else {
                    DamageNumberChannel::Horizon
                },
                gameplay_now,
            );
            if died {
                self.arm_player_death(true, HAZARD_DEATH_TICKS, ctx.sim_tick, ctx.video_hz);
            }
        }
        for (channel, number, damage) in [
            (
                psx_game_runtime::projectiles::ProjectileDamageChannel::Horizon,
                DamageNumberChannel::Horizon,
                taken.horizon_shots,
            ),
            (
                psx_game_runtime::projectiles::ProjectileDamageChannel::Zenith,
                DamageNumberChannel::Zenith,
                taken.zenith_shots,
            ),
        ] {
            if damage == 0 {
                continue;
            }
            let died = self.hazard_death_ticks_remaining == 0
                && self.apply_typed_player_damage_from(taken.attacker, channel, damage);
            // A projectile carries its own channel, so a Choir Needle
            // reads teal on the way in exactly as the bolt did.
            self.damage_numbers
                .spawn(anchor, self.room_index, damage, number, gameplay_now);
            if died {
                self.arm_player_death(true, HAZARD_DEATH_TICKS, ctx.sim_tick, ctx.video_hz);
            }
        }
        if taken.hits == 0 {
            return;
        }
        telemetry::counter(telemetry::counter::PLAYER_HITS_TAKEN, u32::from(taken.hits));
        let action = self.anim_state.action();
        let armored = matches!(
            self.anim_state,
            PlayerAnim::HeavyAttack | PlayerAnim::VertHeavyAttack
        ) && player.pose.is_some_and(|pose| {
            let frame = (pose.phase_q12() >> 12).min(u32::from(u16::MAX)) as u16;
            player.capsules.iter().any(|capsule| {
                capsule.flags & psx_level::combat_capsule_flags::HITBOX != 0
                    && capsule.action == action.to_index() as u8
                    && frame >= capsule.active_start_frame
                    && frame <= capsule.active_end_frame
            })
        });
        let armored = self
            .player_combat_sample(ctx)
            .active(psx_level::CombatWindowKind::Armored)
            .unwrap_or(armored);
        let shot_only = taken.melee_damage == 0;
        let staggered =
            self.react_player_to_hit(taken.attacker, taken.poise, armored, shot_only, ctx);
        if shot_only && staggered && taken.shot_opening {
            self.duel_log_flow(ctx, 1);
            for impact in impacts.as_slice() {
                if impact.kind
                    == (ProjectileImpactKind::Target {
                        target: PLAYER_PROJECTILE_TARGET,
                    })
                {
                    self.game_entities
                        .projectile_connected(usize::from(impact.owner), true);
                }
            }
        }
        self.spawn_combat_hit_sparks(
            player.position,
            player.height.max(0) as u16,
            self.room_index,
            self.player.stance.active() == VitalityChannelId::Two,
            staggered,
            self.hazard_death_ticks_remaining != 0,
        );
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
        attacker_pose: Option<psx_game_runtime::actor_pose::ActorPoseSnapshot>,
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
                pose: attacker_pose,
                frame: attacker_pose.map_or(0, |pose| {
                    (pose.phase_q12() >> 12).min(u32::from(u16::MAX)) as u16
                }),
                released: self
                    .game_entities
                    .deferred_projectile_release_mask(attack)
                    .unwrap_or(u16::MAX),
                target,
                room: attack.room(),
                team: CombatTeam::Enemy,
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
    pub(super) fn release_projectiles(&mut self, ctx: &Ctx, volley: Volley) {
        let mut released = volley.released;
        // Every bolt of the first volley released inside the shooter's crit
        // window is a crit, and the volley uses the crit up.
        let crit = self.shooter_crit_armed(volley.shooter);
        let mut crit_released = false;
        while let Some((emitter, release)) = combat::authored_projectile_release_pending_at_frame(
            volley.capsules,
            volley.action,
            volley.pose,
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
                    self.player.spend_crit();
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

    /// Whether the shooter's next attack is a crit.
    fn shooter_crit_armed(&self, shooter: Shooter) -> bool {
        match shooter {
            Shooter::Player => self.player.crit != 0,
            Shooter::Entity(attack) => self.game_entities.crit_armed(attack.entity()),
        }
    }

    /// Damage and poise of one of the shooter's bolts through its crit.
    fn shooter_crit_scaled(&self, shooter: Shooter, damage: u16, poise: u16) -> (u16, u16) {
        match shooter {
            Shooter::Player => self.player.crit_scaled(&self.player_stats, damage, poise),
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
            Shooter::Player => self.player.flow.can_shoot(&self.player_stats),
            Shooter::Entity(attack) => GAME_ENTITIES
                .get(attack.entity())
                .is_some_and(|record| self.game_entities.can_fire_energy(record, attack.entity())),
        }
    }

    fn shooter_spend_shot(&mut self, shooter: Shooter) {
        match shooter {
            Shooter::Player => {
                let _ = self.player.flow.spend_shot(&self.player_stats);
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
            Shooter::Player => (0, self.player.flow.energy),
            Shooter::Entity(attack) => (1, self.game_entities.energy(attack.entity())),
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
        let first = entity.combat_capsule_first.to_usize();
        let end = first + usize::from(entity.combat_capsule_count);
        let capsules = COMBAT_CAPSULES.get(first..end).unwrap_or(&[]);
        let player = self.motor.position();
        if !attack.is_ranged() {
            if combat::melee_tell_tracks(capsules, attack.action(), phase) {
                self.game_entities.track_authored_melee_tell(
                    index,
                    [player.x, player.y, player.z],
                    delta,
                );
            }
            return;
        }
        if combat::ranged_charge_tracks(capsules, attack.action(), phase) {
            let height = self
                .character
                .as_ref()
                .map_or(WORLD_PLAYER_HEIGHT, |character| character.height);
            self.game_entities.track_authored_ranged_charge(
                index,
                [player.x, player.y, player.z],
                height,
                delta,
            );
        }
    }

    pub(super) fn player_swing_sound(&mut self) {
        if !player_anim_is_attack(self.anim_state) {
            return;
        }
        let (Some(character), Some(pose)) = (self.character.as_ref(), self.player_actor_pose)
        else {
            return;
        };
        let first = character.combat_capsule_first.to_usize();
        let end = first + usize::from(character.combat_capsule_count);
        let previous = self
            .previous_player_actor_pose
            .filter(|old| old.clip_local() == pose.clip_local())
            .map(|old| old.pose().phase_q12());
        if combat::melee_window_started(
            COMBAT_CAPSULES.get(first..end).unwrap_or(&[]),
            self.anim_state.action(),
            previous,
            pose.pose().phase_q12(),
        ) {
            if let Some(event) = weapon_swing_sfx(self.anim_state.action()) {
                self.queue_gameplay_sfx(event);
            }
            self.swing_hit_mask = 0;
            self.destructibles.begin_swing();
            telemetry::debug_log("player swing:active");
        }
    }

    pub(super) fn enemy_swing_sound(
        &mut self,
        instance: usize,
        previous: Option<InstanceActorPoseSnapshot>,
        current: Option<InstanceActorPoseSnapshot>,
    ) {
        let Some(pose) = current else {
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
        let first = entity.combat_capsule_first.to_usize();
        let end = first + usize::from(entity.combat_capsule_count);
        let previous = previous
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

    /// Player melee resolution for the combat slice: while an attack
    /// action is locked and the current attack-clip frame is inside
    /// the weapon's active window, sweep the melee arc over the live
    /// entities. The swing mask keeps one swing to one connection per
    /// enemy; `start_player_anim_action` clears it when a new attack
    /// starts. Outside attack locks this is two compares.
    fn resolve_player_melee(&mut self, ctx: &Ctx) {
        let now = ctx.sim_tick;
        if self.anim_state == PlayerAnim::RangedAttack
            || !player_anim_is_attack(self.anim_state)
            || self.anim_lock_until_tick <= now
        {
            return;
        }
        let Some(character) = self.character else {
            return;
        };
        let Some(player_pose) = self.player_actor_pose else {
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
            // Fail closed before either authored capsules or the legacy
            // arc can latch a hit through invalid generated prop state.
            return;
        }
        let spec = combat::player_melee_spec(EQUIPMENT, WEAPONS, WEAPON_HITBOXES);
        let spec = if self.anim_state == PlayerAnim::HeavyAttack {
            spec.heavy()
        } else {
            spec
        };
        // The active frame comes from the exact snapshot the visible body and
        // equipped weapon consume; combat never reconstructs animation phase.
        let action = self.anim_state.action();
        let phase = player_pose.pose().phase_q12();
        let gameplay_now = self.gameplay_tick(now);
        if let Some(stats) = self.resolve_player_combat_capsules(
            ctx,
            &character,
            player_pose,
            self.previous_player_actor_pose,
            phase,
            action,
            spec.reach,
            prop_blockers.as_slice(),
        ) {
            self.spend_player_melee_crit(stats, ctx);
            self.report_player_melee_stats(stats);
            return;
        }
        if !spec.frame_active(phase >> 12) {
            return;
        }
        let player = self.motor.position();
        let arc = MeleeArc {
            room: self.room_index,
            x: player.x,
            z: player.z,
            yaw: self.motor.yaw().as_q12(),
            reach: spec.reach,
            half_angle: spec.half_angle,
        };
        // Same world-occlusion authority as the enemy side: the legacy arc
        // has no blade geometry, so a closed door inside its reach must
        // still block the connection.
        let player_eye = melee_eye_point([player.x, player.y, player.z]);
        let vitality_channel = Self::player_attack_channel(self.anim_state);
        self.vitality_circles.claim_struck(
            VITALITY_CIRCLES.as_slice(),
            self.room_index,
            player.x,
            player.z,
            spec.reach.clamp(0, i32::from(u16::MAX)) as u16,
            vitality_channel,
        );
        let crit = self.player.crit != 0;
        let (outgoing_damage, poise_damage) = self.player.crit_scaled(
            &self.player_stats,
            self.vitality_modifiers()
                .outgoing_damage(vitality_channel, spec.damage),
            spec.poise_damage,
        );
        let channel = match vitality_channel {
            VitalityChannelId::One => DamageChannel::Horizon,
            VitalityChannelId::Two => DamageChannel::Zenith,
        };
        self.damage_player_environment_melee(spec.reach, channel, outgoing_damage);
        // The arc sweep latches every connection into the swing mask, so
        // the bits it newly set ARE the entities it hit. Snapshotting the
        // mask keeps the number spawn on this side of the engine crate's
        // API instead of widening `MeleeArcStats` with positions that
        // only presentation wants.
        let swing_mask_before = self.swing_hit_mask;
        let entities = &mut self.game_entities;
        let mut world_rt = self.world_rt.as_mut();
        let stats = entities.apply_melee_arc_occluded(
            GAME_ENTITIES.as_slice(),
            &arc,
            vitality_channel,
            outgoing_damage,
            poise_damage,
            &mut self.swing_hit_mask,
            |_, target| match world_rt.as_deref_mut() {
                Some(world_rt) => !world_rt.melee_segment_clear(
                    player_eye,
                    melee_eye_point(target),
                    prop_blockers.as_slice(),
                    &self.destructibles,
                ),
                None => false,
            },
        );
        if stats.hits > 0 {
            self.spawn_melee_arc_damage_numbers(
                swing_mask_before,
                outgoing_damage,
                vitality_channel,
                if crit {
                    DamageNumberChannel::Crit
                } else {
                    damage_number_channel_for(channel)
                },
                gameplay_now,
            );
        }
        if stats.parries > 0 {
            // The arc reports a count; the parrier is the newly struck
            // entity still inside its parry window.
            // psx-numeric-allow-next-line: swing bitmask diff; bit ops only, two-word on R3000
            let connected = self.swing_hit_mask & !swing_mask_before;
            if let Some(entity) = (0..self.game_entities.count().min(GAME_ENTITIES.len())).find(
                // psx-numeric-allow-next-line: swing bitmask bit select; no 64-bit arithmetic
                |&entity| connected & (1u64 << entity) != 0 && self.entity_parries(entity),
            ) {
                self.entity_parried_player(entity, ctx);
            }
        }
        if stats.deaths > 0 {
            self.award_melee_arc_souls(swing_mask_before, gameplay_now);
        }
        self.spend_player_melee_crit(stats, ctx);
        self.report_player_melee_stats(stats);
    }

    /// Whether entity `index` parries a hit this tick.
    fn entity_parries(&self, index: usize) -> bool {
        self.game_entities
            .defender(GAME_ENTITIES.as_slice(), index)
            .is_some_and(|defender| defender.parries())
    }

    /// The swing that reached anyone, hit or parried, uses up the player's
    /// crit.
    // Rare (a parry, crit or knockdown): kept out of line and small.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn spend_player_melee_crit(&mut self, stats: MeleeArcStats, ctx: &Ctx) {
        if stats.hits.saturating_add(stats.parries) > 0 && self.player.spend_crit() {
            self.duel_log_event(ctx, 0, duel::DUEL_EVENT_CRIT);
        }
    }

    /// Credit the cooked award of every entity this arc sweep killed.
    ///
    /// The crate's arc API reports only a death COUNT, so the swing mask is
    /// the way back to identities: a bit set since `before` is one fresh
    /// connection this sweep made, and the sweep skips entities that were
    /// already dead, so a newly-connected entity now reading Dead is one this
    /// swing killed. Guarded by `stats.deaths > 0` at the call site, so a
    /// swing that kills nothing never walks the list.
    // psx-numeric-allow-next-line: swing bitmask snapshot; bit ops only, two-word on R3000
    fn award_melee_arc_souls(&mut self, before: u64, now: SimTick) {
        // psx-numeric-allow-next-line: swing bitmask diff; bit ops only, two-word on R3000
        let connected = self.swing_hit_mask & !before;
        if connected == 0 {
            return;
        }
        let count = self.game_entities.count().min(GAME_ENTITIES.len());
        for entity in 0..count {
            // psx-numeric-allow-next-line: swing bitmask bit select; no 64-bit arithmetic
            if connected & (1u64 << entity) == 0
                || self.game_entities.state(entity)
                    != psx_game_runtime::entities::GameEntityState::Dead
            {
                continue;
            }
            self.souls
                .award(GAME_ENTITIES[entity].soul_value, now.as_u32());
        }
    }

    /// Spawn a floating number over every entity this swing newly
    /// connected with. `before` is the swing mask as it stood prior to
    /// the sweep; every bit set since is one fresh connection. The arc
    /// applies one authored `damage`, and each number shows what the
    /// target's guard made of it, the same value the sweep applied.
    fn spawn_melee_arc_damage_numbers(
        &mut self,
        // psx-numeric-allow-next-line: swing bitmask snapshot; bit ops only, two-word on R3000
        before: u64,
        damage: u16,
        attack: VitalityChannelId,
        channel: DamageNumberChannel,
        now: SimTick,
    ) {
        // psx-numeric-allow-next-line: swing bitmask diff; bit ops only, two-word on R3000
        let connected = self.swing_hit_mask & !before;
        if connected == 0 {
            return;
        }
        let count = self.game_entities.count().min(GAME_ENTITIES.len());
        for entity in 0..count {
            // psx-numeric-allow-next-line: swing bitmask bit select; no 64-bit arithmetic
            if connected & (1u64 << entity) == 0 || self.entity_parries(entity) {
                continue;
            }
            self.damage_numbers.spawn(
                struck_actor_anchor(
                    self.game_entities.position(entity),
                    GAME_ENTITIES[entity].height,
                ),
                GAME_ENTITIES[entity].room,
                self.game_entities.scaled_stance_damage(
                    GAME_ENTITIES.as_slice(),
                    entity,
                    attack,
                    damage,
                ),
                channel,
                now,
            );
            let state = self.game_entities.state(entity);
            self.spawn_combat_hit_sparks(
                self.game_entities.position(entity),
                GAME_ENTITIES[entity].height,
                GAME_ENTITIES[entity].room,
                channel == DamageNumberChannel::Zenith,
                state == psx_game_runtime::entities::GameEntityState::Staggered,
                state == psx_game_runtime::entities::GameEntityState::Dead,
            );
        }
    }

    /// Apply one active melee frame to blocking world props through a compact
    /// forward volume. This is deliberately separate from projectile traces:
    /// only the authored/legacy melee windows above call it.
    fn damage_player_environment_melee(&mut self, reach: i32, channel: DamageChannel, damage: u16) {
        let player = self.motor.position();
        let yaw = self.motor.yaw();
        let reach = reach.max(0);
        let radius = self
            .character
            .map(|character| character.radius)
            .unwrap_or(WORLD_PLAYER_RADIUS)
            .clamp(1, i32::from(u16::MAX)) as u16;
        let start = [
            player.x,
            player.y.saturating_add(i32::from(radius)),
            player.z,
        ];
        let end = [
            player
                .x
                .saturating_add((yaw.sin_q12().saturating_mul(reach)) >> 12),
            start[1],
            player
                .z
                .saturating_add((yaw.cos_q12().saturating_mul(reach)) >> 12),
        ];
        self.damage_world_destructibles_with_capsule(start, end, radius, channel, damage);
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
            },
        );
    }

    /// Resolve authored rig volumes when the selected action has any hitbox.
    /// `None` selects the legacy arc fallback; `Some` means the authored frame
    /// window is authoritative, including frames with no active capsule.
    fn resolve_player_combat_capsules(
        &mut self,
        ctx: &Ctx,
        character: &RuntimeCharacter,
        player_pose: PlayerActorPoseSnapshot,
        previous_player_pose: Option<PlayerActorPoseSnapshot>,
        phase: u32,
        action: CharacterAnimationAction,
        environment_reach: i32,
        prop_blockers: &[CharacterCollisionAabb],
    ) -> Option<MeleeArcStats> {
        let now = self.gameplay_tick(ctx.sim_tick);
        let crit = self.player.crit != 0;
        let first = character.combat_capsule_first.to_usize();
        let end = first.saturating_add(usize::from(character.combat_capsule_count));
        let records = COMBAT_CAPSULES.get(first..end)?;
        let action_index = action.to_index() as u8;
        let previous_pose = previous_player_pose
            .filter(|previous| {
                previous.clip_local() == player_pose.clip_local()
                    && player_pose
                        .pose()
                        .tick()
                        .wrapping_sub(previous.pose().tick())
                        == 1
                    && phase >= previous.pose().phase_q12()
            })
            .map(|previous| *previous.pose());
        let mut active = [ActivePlayerCapsule::EMPTY; psx_level::MAX_CHARACTER_COMBAT_CAPSULES];
        let mut active_count = 0usize;
        let mut authored_action = false;
        for record in records {
            if record.flags & psx_level::combat_capsule_flags::HITBOX == 0
                || record.action != action_index
            {
                continue;
            }
            authored_action = true;
            if active_count >= active.len() {
                continue;
            }
            let Some((sweep_start_phase, sweep_end_phase)) = combat::active_frame_sweep_phase_range(
                previous_pose.map(|pose| pose.phase_q12()),
                phase,
                record.active_start_frame,
                record.active_end_frame,
            ) else {
                continue;
            };
            let Some((capsule, previous_capsule)) = combat::transform_actor_combat_capsule_sweep(
                record,
                previous_pose,
                *player_pose.pose(),
                sweep_start_phase,
                sweep_end_phase,
            ) else {
                continue;
            };
            let (damage, poise_damage) = self.player.crit_scaled(
                &self.player_stats,
                self.vitality_modifiers()
                    .outgoing_damage(Self::player_attack_channel(self.anim_state), record.damage),
                record.poise_damage,
            );
            active[active_count] = ActivePlayerCapsule {
                capsule,
                previous_capsule,
                damage,
                poise_damage,
            };
            active_count += 1;
        }
        if !authored_action {
            return None;
        }
        if active_count == 0 {
            return Some(MeleeArcStats::default());
        }

        let vitality_channel = Self::player_attack_channel(self.anim_state);
        let player = self.motor.position();
        self.vitality_circles.claim_struck(
            VITALITY_CIRCLES.as_slice(),
            self.room_index,
            player.x,
            player.z,
            environment_reach.clamp(0, i32::from(u16::MAX)) as u16,
            vitality_channel,
        );
        let channel = match vitality_channel {
            VitalityChannelId::One => DamageChannel::Horizon,
            VitalityChannelId::Two => DamageChannel::Zenith,
        };
        let damage_number_channel = if crit {
            DamageNumberChannel::Crit
        } else {
            damage_number_channel_for(channel)
        };
        for hit in &active[..active_count] {
            // A blocking prop needs a short forward contact volume rather than
            // an inflated copy of a lateral blade capsule. Use the weapon's
            // cooked melee reach, the player's body radius and the exact
            // authored active frames. Projectiles never enter this path.
            self.damage_player_environment_melee(environment_reach, channel, hit.damage);
        }

        let mut stats = MeleeArcStats::default();
        let count = self.game_entities.count().min(GAME_ENTITIES.len());
        let mut entity = 0usize;
        while entity < count {
            let entity_record = &GAME_ENTITIES[entity];
            // psx-numeric-allow-next-line: one-hit-per-swing bitmask; bit ops only, two-word on R3000
            let mask = 1u64 << entity;
            if self.swing_hit_mask & mask != 0 {
                // One-hit-per-swing: this swing already connected with
                // the entity, so its remaining active frames reject the
                // repeat contact here.
                telemetry::counter(telemetry::counter::PLAYER_DUPLICATE_HIT_REJECTIONS, 1);
                entity += 1;
                continue;
            }
            // An entity in its i-frames (past its parry window) is passed
            // through without latching the swing, as an enemy swing passes
            // through the player: a later active frame of the same swing can
            // still land.
            if entity_record.room != self.room_index
                || self.game_entities.state(entity)
                    == psx_game_runtime::entities::GameEntityState::Dead
                || self
                    .game_entities
                    .passes_through(GAME_ENTITIES.as_slice(), entity)
            {
                entity += 1;
                continue;
            }
            let position = self.game_entities.position(entity);
            let body = WorldCombatCapsule {
                start: position,
                end: [
                    position[0],
                    position[1].saturating_add(i32::from(entity_record.height)),
                    position[2],
                ],
                radius: entity_record.radius,
            };
            let coarse_candidate = active[..active_count]
                .iter()
                .copied()
                .any(|hit| hit.aabb_overlaps(&body));
            if !coarse_candidate {
                entity += 1;
                continue;
            }

            let hit =
                self.authored_capsule_hit_entity(entity_record, &active[..active_count], body);
            if let Some(hit) = hit {
                // Capsule overlap alone has no world knowledge; a closed door
                // between the actors blocks the connection without latching
                // the swing bit, so the same swing can still land once the
                // door finishes opening inside the active window.
                let player = self.motor.position();
                if !self.melee_line_clear([player.x, player.y, player.z], position, prop_blockers) {
                    entity += 1;
                    continue;
                }
                self.swing_hit_mask |= mask;
                let applied_damage = self.game_entities.scaled_stance_damage(
                    GAME_ENTITIES.as_slice(),
                    entity,
                    vitality_channel,
                    hit.damage,
                );
                let outcome = self.game_entities.apply_hit(
                    GAME_ENTITIES.as_slice(),
                    entity,
                    vitality_channel,
                    hit.damage,
                    hit.poise_damage,
                );
                if outcome.parried {
                    stats.parries = stats.parries.saturating_add(1);
                    self.entity_parried_player(entity, ctx);
                }
                if outcome.connected {
                    if matches!(
                        self.anim_state,
                        PlayerAnim::HeavyAttack | PlayerAnim::VertHeavyAttack
                    ) {
                        let p = self.motor.position();
                        self.game_entities.recoil_from(entity, [p.x, p.y, p.z]);
                    }
                    // Spawned here rather than from the returned stats
                    // because the damage is per-capsule: only this site
                    // knows which capsule actually connected.
                    self.damage_numbers.spawn(
                        struck_actor_anchor(position, entity_record.height),
                        entity_record.room,
                        applied_damage,
                        damage_number_channel,
                        now,
                    );
                    self.spawn_combat_hit_sparks(
                        position,
                        entity_record.height,
                        entity_record.room,
                        vitality_channel == VitalityChannelId::Two,
                        outcome.staggered,
                        outcome.died,
                    );
                }
                if outcome.died {
                    // The authored-capsule path knows exactly which entity
                    // the killing blow landed on, so it credits its own
                    // cooked award directly.
                    self.souls.award(entity_record.soul_value, now.as_u32());
                }
                stats.hits = stats.hits.saturating_add(u16::from(outcome.connected));
                stats.staggers = stats.staggers.saturating_add(u16::from(outcome.staggered));
                stats.deaths = stats.deaths.saturating_add(u16::from(outcome.died));
            }
            entity += 1;
        }
        Some(stats)
    }

    fn authored_capsule_hit_entity(
        &self,
        entity_record: &LevelGameEntityRecord,
        active: &[ActivePlayerCapsule],
        body_fallback: WorldCombatCapsule,
    ) -> Option<ActivePlayerCapsule> {
        let first = entity_record.combat_capsule_first.to_usize();
        let end = first.saturating_add(usize::from(entity_record.combat_capsule_count));
        let hurtboxes = COMBAT_CAPSULES.get(first..end).unwrap_or(&[]);
        let has_authored_hurtbox = hurtboxes
            .iter()
            .any(|record| record.flags & psx_level::combat_capsule_flags::HURTBOX != 0);
        if !has_authored_hurtbox {
            return active
                .iter()
                .copied()
                .find(|hit| hit.overlaps(&body_fallback));
        }

        let instance_pose = self
            .instance_actor_poses
            .get(entity_record.model_instance as usize)
            .copied()
            .flatten()?;
        for hurtbox in hurtboxes {
            if hurtbox.flags & psx_level::combat_capsule_flags::HURTBOX == 0 {
                continue;
            }
            let Some(hurtbox) =
                combat::transform_actor_combat_capsule(hurtbox, instance_pose.pose())
            else {
                continue;
            };
            if let Some(hit) = active.iter().copied().find(|hit| hit.overlaps(&hurtbox)) {
                return Some(hit);
            }
        }
        None
    }

    pub(super) fn report_player_melee_stats(&mut self, stats: MeleeArcStats) {
        if stats.hits > 0 {
            let heavy = matches!(
                self.anim_state,
                PlayerAnim::HeavyAttack | PlayerAnim::VertHeavyAttack
            );
            for _ in 0..stats.hits {
                self.player.flow.melee_hit(heavy, &self.player_stats);
            }
            telemetry::counter(telemetry::counter::PLAYER_MELEE_HITS, u32::from(stats.hits));
            let event = if matches!(
                self.anim_state,
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
            {
                continue;
            }
            let position = self.game_entities.position(index);
            // A final Attack tick may have transitioned the state to Recover
            // before presentation freezes. The deferred token retains the
            // exact attack clip/phase that contact evaluates, so body and
            // equipment visibly sample that same pose this tick.
            let clip = self
                .deferred_enemy_attacks
                .as_slice()
                .iter()
                .copied()
                .find(|attack| attack.entity() == index)
                .map(|attack| attack.clip())
                .unwrap_or_else(|| {
                    self.game_entities
                        .clip_for_state(GAME_ENTITIES.as_slice(), index)
                });
            out.try_push(ModelInstancePoseOverride {
                instance: record.model_instance,
                x: position[0],
                y: position[1],
                z: position[2],
                yaw: self.game_entities.yaw(index),
                clip: psx_level::OptionalModelClipIndex::some(psx_level::ModelClipIndex(clip.clip)),
                phase_ticks: clip.phase_ticks,
                one_shot: clip.one_shot,
                speed_q8: clip.speed_q8,
                frame_range: clip.frame_range,
            });
        }
    }

    /// Drain the logic fire marks into example-side effects: doors
    /// toggle their linked box prop, messages open the interactable
    /// overlay, checkpoints update the in-memory checkpoint (with the
    /// same confirmation overlay the legacy interactable path showed).
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
                            position: self.motor.position(),
                            yaw: self.motor.yaw(),
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

    /// Hurt the player with what the hazard volumes dealt this tick, and arm
    /// the death sequence when it was lethal. The volume already left an
    /// invulnerable player alone, so this only ever sees real hits.
    pub(super) fn apply_logic_hazard_damage(&mut self, ctx: &Ctx) {
        let damage = self.logic.take_hazard_damage();
        let push = self.logic.take_hazard_push();
        if damage != 0 {
            telemetry::debug_log("player hazard:logic");
            if self.apply_hazard_player_damage(damage) {
                self.hazard_push_left = 0;
                self.arm_player_death(false, HAZARD_DEATH_TICKS, ctx.sim_tick, ctx.video_hz);
                return;
            }
        }
        let Some(push) = push else {
            return;
        };
        if damage != 0 {
            self.react_player_to_hit(CombatantId::WORLD, damage, false, false, ctx);
        }
        let mut toward_max = push.toward_max;
        if push.tie {
            // Dead centre: carry on the way she faces.
            let yaw = self.motor.yaw();
            let facing = if push.axis_z {
                yaw.cos_q12()
            } else {
                yaw.sin_q12()
            };
            if facing != 0 {
                toward_max = facing > 0;
            }
        }
        self.hazard_push_left = push.distance;
        self.hazard_push_axis_z = push.axis_z;
        self.hazard_push_toward_max = toward_max;
    }

    /// One tick of the shove a hazard touch gave the player: a few engine
    /// units along the hazard's thin axis through the same collision the
    /// motor uses, so a wall stops it and nothing is pushed out of the world.
    pub(super) fn advance_hazard_push(
        &mut self,
        blockers: &[CharacterCollisionCylinder],
        aabb_blockers: &[CharacterCollisionAabb],
    ) {
        if self.hazard_push_left == 0 {
            return;
        }
        if self.hazard_death_ticks_remaining != 0 {
            self.hazard_push_left = 0;
            return;
        }
        let step = i32::from(self.hazard_push_left).min(HAZARD_PUSH_STEP);
        let signed = if self.hazard_push_toward_max {
            step
        } else {
            -step
        };
        let (dx, dz) = if self.hazard_push_axis_z {
            (0, signed)
        } else {
            (signed, 0)
        };
        let config = self.motor_config();
        let start = self.motor.position();
        let Some(world_rt) = self.world_rt.as_mut() else {
            self.hazard_push_left = 0;
            return;
        };
        let moved = world_rt
            .commit_body_step(
                start,
                dx,
                dz,
                config.radius,
                config.height,
                &self.destructibles,
                blockers,
                aabb_blockers,
            )
            .ok()
            .map(|step| step.position)
            .filter(|end| {
                // Progress along the push axis, not a slide along a wall.
                if self.hazard_push_axis_z {
                    (end.z - start.z).signum() == dz.signum()
                } else {
                    (end.x - start.x).signum() == dx.signum()
                }
            });
        match moved {
            Some(end) => {
                self.motor.relocate(end);
                self.hazard_push_left -= step as u16;
            }
            None => self.hazard_push_left = 0,
        }
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
