//! Phase-3 gameplay-layer glue (docs/game-runtime-plan.md): the
//! example-side movement backend and effect dispatch the runtime
//! crate's `GameEntities`/`LogicRuntime` leave to the owning game.
//!
//! - [`SceneEntityMover`] backs entity Patrol/Aggro steps with the active
//!   world's movement solver. Grid projects retain `commit_body_step`; BSP
//!   projects use the same caller-owned static-world + transformed-mover +
//!   dynamic-cylinder trace stack as the player. A grid room whose collision
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
use psx_game_runtime::entities::MeleeArcStats;

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
    damage: u16,
    poise_damage: u16,
}

impl ActivePlayerCapsule {
    const EMPTY: Self = Self {
        capsule: WorldCombatCapsule::EMPTY,
        damage: 0,
        poise_damage: 0,
    };
}

/// Movement backend for game entities. Both world backends compose other
/// entities' pre-tick bodies and the player; grid projects additionally use
/// active-room and prop collision, while BSP projects use resident hulls and
/// transformed brush movers.
pub(super) struct SceneEntityMover<'a> {
    pub(super) bsp: Option<&'a mut BspRuntime>,
    pub(super) window: &'a RuntimeRoomWindow,
    pub(super) box_props: &'a RuntimeBoxProps,
    pub(super) models: &'a [Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    /// Pre-tick entity positions (entities move one at a time inside
    /// the tick; the snapshot keeps blocker order deterministic).
    pub(super) entity_positions: [[i32; 3]; MAX_GAME_ENTITIES],
    /// Pre-tick dead flags: corpses stop blocking other movers.
    pub(super) entity_dead: [bool; MAX_GAME_ENTITIES],
    pub(super) player: RoomPoint,
    pub(super) player_room: RoomIndex,
    pub(super) player_radius: i32,
    pub(super) player_height: i32,
}

impl SceneEntityMover<'_> {
    /// The active-window slot for `room`, if its collision is
    /// resident.
    fn active_room(&self, room: RoomIndex) -> Option<&ActiveRuntimeRoom> {
        self.window
            .rooms
            .iter()
            .flatten()
            .find(|active| active.index == room)
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
        // Body blockers: other entities' cooked instances at their
        // LIVE positions (self excluded), plus the player's capsule.
        let own_instance = GAME_ENTITIES
            .get(entity)
            .map(|record| record.model_instance)
            .unwrap_or(psx_level::GAME_ENTITY_MODEL_INSTANCE_NONE);
        let mut cylinders = [CharacterCollisionCylinder::EMPTY; MAX_COLLISION_CYLINDERS];
        let mut cylinder_count = 0usize;
        for (index, inst) in MODEL_INSTANCES.iter().enumerate() {
            if inst.room != room || cylinder_count >= cylinders.len() {
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
                    if self.entity_dead[other] {
                        continue;
                    }
                    let live = self.entity_positions[other];
                    RoomPoint::new(live[0], live[1], live[2])
                }
                None => RoomPoint::new(inst.x, inst.y, inst.z),
            };
            cylinders[cylinder_count] =
                CharacterCollisionCylinder::new(center, body_radius, body_height);
            cylinder_count += 1;
        }
        if self.player_room == room && self.player_radius > 0 && cylinder_count < cylinders.len() {
            cylinders[cylinder_count] = CharacterCollisionCylinder::new(
                self.player,
                self.player_radius,
                self.player_height.max(1),
            );
            cylinder_count += 1;
        }
        cylinder_count +=
            psx_game_runtime::cylinder_props::collect_cylinder_prop_collision_blockers(
                CYLINDER_PROPS,
                room,
                &mut cylinders[cylinder_count..],
            );

        let start = RoomPoint::new(position[0], position[1], position[2]);
        let mut aabbs = [CharacterCollisionAabb::EMPTY; MAX_STATIC_PROP_AABB_BLOCKERS];
        if let Some(bsp) = self.bsp.as_deref_mut() {
            let Some(mut aabb_count) = self.box_props.collect_collision_blockers_checked(
                BOX_PROPS,
                room,
                &mut aabbs,
            ) else {
                return position;
            };
            let Some(count) =
                psx_game_runtime::arch_props::collect_arch_prop_collision_blockers_checked(
                    ARCH_PROPS,
                    ARCH_PROP_COLLISIONS,
                    room,
                    &mut aabbs[aabb_count..],
                )
            else {
                return position;
            };
            aabb_count += count;
            let Some(count) =
                psx_game_runtime::image_props::collect_image_prop_collision_blockers_checked(
                    IMAGE_PROPS,
                    room,
                    &mut aabbs[aabb_count..],
                )
            else {
                return position;
            };
            aabb_count += count;
            let step = bsp
                .commit_body_step(
                    start,
                    dx,
                    dz,
                    radius,
                    height,
                    &cylinders[..cylinder_count],
                    &aabbs[..aabb_count],
                )
                .expect("PXBSP entity trace failed");
            return [step.position.x, step.position.y, step.position.z];
        }

        let mut aabb_count = self
            .box_props
            .collect_collision_blockers(BOX_PROPS, room, &mut aabbs);
        aabb_count += psx_game_runtime::arch_props::collect_arch_prop_collision_blockers(
            ARCH_PROPS,
            ARCH_PROP_COLLISIONS,
            room,
            &mut aabbs[aabb_count..],
        );
        aabb_count += psx_game_runtime::image_props::collect_image_prop_collision_blockers(
            IMAGE_PROPS,
            room,
            &mut aabbs[aabb_count..],
        );

        // Entity coordinates are their OWN room's local space, so the grid
        // collision room enters with zero offsets (window offsets translate
        // rooms into the CURRENT room's space for the player).
        let Some(active) = self.active_room(room) else {
            return position;
        };
        let collision_rooms = [CharacterCollisionRoom::from_collision(
            active.collision_room,
            0,
            0,
        )];
        let collision = CharacterCollision::rooms_with_aabbs(
            &collision_rooms,
            &cylinders[..cylinder_count],
            &aabbs[..aabb_count],
        );
        let step =
            psx_engine::character_motor::commit_body_step(collision, start, dx, dz, radius, height);
        [step.position.x, step.position.y, step.position.z]
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

impl Playtest {
    /// Resolve enemy attack contact after this tick's player and instance pose
    /// snapshots have been frozen. Authored HITBOX/HURTBOX geometry is
    /// authoritative whenever both sides provide it. The legacy entity arc is
    /// used only when either side has no authored role at all; inactive
    /// authored frames, invalid authored joints, and authored geometric misses
    /// stay misses.
    pub(super) fn resolve_enemy_melee(&mut self, ctx: &Ctx) {
        if self.deferred_enemy_attacks.is_empty() {
            return;
        }
        let player = self.motor.position();
        let player_position = [player.x, player.y, player.z];
        // The same body-radius source the entity tick input uses, so the
        // legacy fallback arc keeps its Character-derived reach.
        let player_radius = match self.character {
            Some(character) => character.radius,
            None if self.bsp.is_some() => BSP_PLAYER_RADIUS,
            None => 0,
        };
        let player_invulnerable = self.motor.is_action_invulnerable(self.motor_config());
        let player_capsules = self
            .character
            .and_then(|character| {
                let first = character.combat_capsule_first.to_usize();
                let end = first.saturating_add(usize::from(character.combat_capsule_count));
                COMBAT_CAPSULES.get(first..end)
            })
            .unwrap_or(&[]);
        let player_pose = self.player_actor_pose.map(|snapshot| snapshot.pose());
        let mut prop_blockers = [CharacterCollisionAabb::EMPTY; MAX_STATIC_PROP_AABB_BLOCKERS];
        let prop_blocker_count = if self.bsp.is_some() {
            let Some(count) = self.collect_static_prop_aabb_blockers_checked(&mut prop_blockers)
            else {
                // Malformed or overflowing authored collision cannot grant a
                // combat connection. Deferred tokens stay available to retry.
                return;
            };
            count
        } else {
            0
        };
        let mut hits = 0u16;
        let mut damage_total = 0u16;
        let mut attack_index = 0usize;
        while attack_index < self.deferred_enemy_attacks.len() {
            let Some(attack) = self.deferred_enemy_attacks.get(attack_index) else {
                break;
            };
            attack_index += 1;
            if player_invulnerable
                || attack.room() != self.room_index
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
            let attacker_pose = self
                .instance_actor_poses
                .get(entity.model_instance as usize)
                .copied()
                .flatten()
                .map(|snapshot| snapshot.pose());
            let contact = combat::resolve_authored_actor_contact(
                attacker_capsules,
                CharacterAnimationAction::LightAttack,
                attacker_pose,
                player_capsules,
                player_pose,
            );
            let damage = match contact {
                combat::AuthoredActorContact::Hit {
                    damage,
                    poise_damage: _,
                } => Some(damage),
                combat::AuthoredActorContact::Miss => None,
                combat::AuthoredActorContact::FallbackRequired => self
                    .game_entities
                    .deferred_attack_legacy_arc_hits(
                        GAME_ENTITIES,
                        attack,
                        player_position,
                        self.room_index,
                        player_radius,
                    )
                    .then_some(entity.touch_damage),
            };
            let Some(damage) = damage else {
                continue;
            };
            // World occlusion is authoritative for BOTH the authored capsule
            // and the legacy arc outcome: a closed door between the frozen
            // attacker position and the player blocks the connection. The
            // token is deliberately not consumed, mirroring an i-frame whiff.
            let attacker = attack.position();
            if let Some(bsp) = self.bsp.as_mut() {
                if !bsp.melee_segment_clear(
                    melee_eye_point(attacker),
                    melee_eye_point(player_position),
                    &prop_blockers[..prop_blocker_count],
                ) {
                    continue;
                }
            }
            if self.game_entities.connect_deferred_attack(attack) {
                hits = hits.saturating_add(1);
                damage_total = damage_total.saturating_add(damage);
            }
        }
        if damage_total > 0 {
            // Combat damage reaching zero arms the SAME delayed
            // death/respawn sequence the environmental hazards use; a
            // hit landing during the countdown keeps health floored
            // without re-arming it.
            let outcome = psx_game_runtime::character::apply_player_damage(
                self.player_health,
                self.hazard_death_ticks_remaining > 0,
                damage_total,
            );
            self.player_health = outcome.health;
            if outcome.died {
                self.arm_player_death(true, BSP_HAZARD_DEATH_TICKS, ctx.sim_tick, ctx.video_hz);
            }
        }
        if hits > 0 {
            telemetry::counter(telemetry::counter::PLAYER_HITS_TAKEN, u32::from(hits));
        }
    }

    /// Player melee resolution for the combat slice: while an attack
    /// action is locked and the current attack-clip frame is inside
    /// the weapon's active window, sweep the melee arc over the live
    /// entities. The swing mask keeps one swing to one connection per
    /// enemy; `start_player_anim_action` clears it when a new attack
    /// starts. Outside attack locks this is two compares.
    pub(super) fn resolve_player_melee(&mut self, ctx: &Ctx) {
        let now = ctx.sim_tick;
        if !player_anim_is_attack(self.anim_state) || self.anim_lock_until_tick <= now {
            return;
        }
        let Some(character) = self.character else {
            return;
        };
        let Some(player_pose) = self.player_actor_pose else {
            return;
        };
        let mut prop_blockers = [CharacterCollisionAabb::EMPTY; MAX_STATIC_PROP_AABB_BLOCKERS];
        let prop_blocker_count = if self.bsp.is_some() {
            let Some(count) = self.collect_static_prop_aabb_blockers_checked(&mut prop_blockers)
            else {
                // Fail closed before either authored capsules or the legacy
                // arc can latch a hit through invalid generated prop state.
                return;
            };
            count
        } else {
            0
        };
        let spec = combat::player_melee_spec(EQUIPMENT, WEAPONS, WEAPON_HITBOXES);
        // ComboAttack is the heaviest swing the player has, so it takes the
        // heavy scaling too; a distinct multiplier would be a tuning change,
        // not plumbing.
        let spec = if matches!(
            self.anim_state,
            PlayerAnim::HeavyAttack | PlayerAnim::ComboAttack
        ) {
            spec.heavy()
        } else {
            spec
        };
        // The active frame comes from the exact snapshot the visible body and
        // equipped weapon consume; combat never reconstructs animation phase.
        let action = self.anim_state.action();
        let phase = player_pose.pose().phase_q12();
        if let Some(stats) = self.resolve_player_combat_capsules(
            character,
            player_pose,
            phase,
            action,
            &prop_blockers[..prop_blocker_count],
        ) {
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
        // still block the connection on BSP worlds.
        let player_eye = melee_eye_point([player.x, player.y, player.z]);
        let entities = &mut self.game_entities;
        let mut bsp = self.bsp.as_mut();
        let stats = entities.apply_melee_arc_occluded(
            GAME_ENTITIES,
            &arc,
            spec.damage,
            spec.poise_damage,
            &mut self.swing_hit_mask,
            |_, target| match bsp.as_deref_mut() {
                Some(bsp) => !bsp.melee_segment_clear(
                    player_eye,
                    melee_eye_point(target),
                    &prop_blockers[..prop_blocker_count],
                ),
                None => false,
            },
        );
        self.report_player_melee_stats(stats);
    }

    /// Resolve authored rig volumes when the selected action has any hitbox.
    /// `None` selects the legacy arc fallback; `Some` means the authored frame
    /// window is authoritative, including frames with no active capsule.
    fn resolve_player_combat_capsules(
        &mut self,
        character: RuntimeCharacter,
        player_pose: PlayerActorPoseSnapshot,
        phase: u32,
        action: CharacterAnimationAction,
        prop_blockers: &[CharacterCollisionAabb],
    ) -> Option<MeleeArcStats> {
        let first = character.combat_capsule_first.to_usize();
        let end = first.saturating_add(usize::from(character.combat_capsule_count));
        let records = COMBAT_CAPSULES.get(first..end)?;
        let action_index = action.to_index() as u8;
        let frame = (phase >> 12).min(u32::from(u16::MAX)) as u16;
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
            if frame < record.active_start_frame
                || frame > record.active_end_frame
                || active_count >= active.len()
            {
                continue;
            }
            let Some(capsule) = combat::transform_actor_combat_capsule(record, player_pose.pose())
            else {
                continue;
            };
            active[active_count] = ActivePlayerCapsule {
                capsule,
                damage: record.damage,
                poise_damage: record.poise_damage,
            };
            active_count += 1;
        }
        if !authored_action {
            return None;
        }
        if active_count == 0 {
            return Some(MeleeArcStats::default());
        }

        let mut stats = MeleeArcStats::default();
        let count = self.game_entities.count().min(GAME_ENTITIES.len());
        let mut entity = 0usize;
        while entity < count {
            let entity_record = &GAME_ENTITIES[entity];
            // psx-numeric-allow-next-line: one-hit-per-swing bitmask; bit ops only, two-word on R3000
            let mask = 1u64 << entity;
            if self.swing_hit_mask & mask != 0
                || entity_record.room != self.room_index
                || self.game_entities.state(entity)
                    == psx_game_runtime::entities::GameEntityState::Dead
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
                .any(|hit| combat::combat_capsule_aabbs_overlap(&hit.capsule, &body));
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
                if let Some(bsp) = self.bsp.as_mut() {
                    if !bsp.melee_segment_clear(
                        melee_eye_point([player.x, player.y, player.z]),
                        melee_eye_point(position),
                        prop_blockers,
                    ) {
                        entity += 1;
                        continue;
                    }
                }
                self.swing_hit_mask |= mask;
                let outcome = self.game_entities.apply_hit(
                    GAME_ENTITIES,
                    entity,
                    hit.damage,
                    hit.poise_damage,
                );
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
                .find(|hit| combat::combat_capsules_overlap(&hit.capsule, &body_fallback));
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
            if let Some(hit) = active
                .iter()
                .copied()
                .find(|hit| combat::combat_capsules_overlap(&hit.capsule, &hurtbox))
            {
                return Some(hit);
            }
        }
        None
    }

    fn report_player_melee_stats(&self, stats: MeleeArcStats) {
        if stats.hits > 0 {
            telemetry::counter(telemetry::counter::PLAYER_MELEE_HITS, u32::from(stats.hits));
        }
        if stats.staggers > 0 {
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
        }
    }

    /// Build the per-frame pose-override list: every live (visible)
    /// entity with a cooked visual renders at its runtime position and
    /// facing, playing its AI state's cooked clip on the state clock.
    /// Returns the filled count.
    pub(super) fn game_entity_pose_overrides(
        &self,
        out: &mut [ModelInstancePoseOverride; MAX_GAME_ENTITIES],
    ) -> usize {
        let mut count = 0usize;
        for (index, record) in GAME_ENTITIES.iter().enumerate() {
            if record.model_instance == psx_level::GAME_ENTITY_MODEL_INSTANCE_NONE
                || count >= out.len()
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
                .unwrap_or_else(|| self.game_entities.clip_for_state(GAME_ENTITIES, index));
            out[count] = ModelInstancePoseOverride {
                instance: record.model_instance,
                x: position[0],
                y: position[1],
                z: position[2],
                yaw: self.game_entities.yaw(index),
                clip: psx_level::OptionalModelClipIndex::some(psx_level::ModelClipIndex(clip.clip)),
                phase_ticks: clip.phase_ticks,
                one_shot: clip.one_shot,
            };
            count += 1;
        }
        count
    }

    /// Drain the logic fire marks into example-side effects: doors
    /// toggle their linked box prop, messages open the interactable
    /// overlay, checkpoints update the in-memory checkpoint (with the
    /// same confirmation overlay the legacy interactable path showed).
    pub(super) fn dispatch_logic_effects(&mut self) {
        if !self.logic.any_fired() {
            return;
        }
        for (index, record) in LOGIC.iter().enumerate() {
            if !self.logic.take_fired(index) {
                continue;
            }
            match record.kind {
                psx_level::logic_kind::DOOR => {
                    if record.link != psx_level::LOGIC_LINK_NONE {
                        let open = self.logic.door_open(index);
                        if let Some(bsp) = self.bsp.as_mut() {
                            bsp.set_door_open(usize::from(record.link), open);
                        } else {
                            self.box_props.set_door_open(usize::from(record.link), open);
                        }
                    }
                }
                psx_level::logic_kind::MESSAGE => {
                    self.open_logic_message(index, record);
                }
                psx_level::logic_kind::CHECKPOINT => {
                    self.checkpoint = Some(RuntimeCheckpoint {
                        room: self.room_index,
                        position: self.motor.position(),
                        yaw: self.motor.yaw(),
                        checkpoint_id: interactable_for_logic(index)
                            .map(|interactable| interactable.checkpoint_id)
                            .unwrap_or(""),
                    });
                    self.open_logic_message(index, record);
                }
                // Graph plumbing (trigger volumes, relays,
                // multisource gates) has no presentation effect.
                _ => {}
            }
        }
    }

    /// Message overlay for a fired MESSAGE/CHECKPOINT record, through
    /// the same text table + fallbacks the legacy interactable path
    /// used (the cook shares the message index between both records).
    fn open_logic_message(&mut self, index: usize, record: &psx_level::LevelLogicRecord) {
        let (title, body) = match interactable_for_logic(index) {
            Some(interactable) => interactable_message_text(interactable),
            None => match INTERACTABLE_MESSAGES.get(record.message as usize) {
                Some(message) => (message.title, message.body),
                None => return,
            },
        };
        self.message_overlay = Some(RuntimeMessageOverlay { title, body });
    }

    /// Push every DOOR record's current open state onto its linked
    /// box prop (gameplay init and future checkpoint respawns:
    /// START_ON doors begin open without a fire event).
    pub(super) fn sync_door_box_props(&mut self) {
        for (index, record) in LOGIC.iter().enumerate() {
            if record.kind == psx_level::logic_kind::DOOR
                && record.link != psx_level::LOGIC_LINK_NONE
            {
                let open = self.logic.door_open(index);
                if let Some(bsp) = self.bsp.as_mut() {
                    bsp.set_door_open(usize::from(record.link), open);
                } else {
                    self.box_props.set_door_open(usize::from(record.link), open);
                }
            }
        }
    }

    /// First-playable proximity use routed through the normal logic runtime.
    /// The brush cook stores each door's mover ordinal in `LevelLogicRecord::link`;
    /// firing the record keeps relays/masters/state semantics authoritative.
    pub(super) fn activate_nearest_bsp_door(&mut self, now: u32) -> bool {
        let Some(mover) = self
            .bsp
            .as_ref()
            .and_then(|bsp| bsp.nearest_door(self.motor.position(), BSP_USE_DISTANCE))
        else {
            return false;
        };
        let Some(index) = LOGIC.iter().position(|record| {
            record.kind == psx_level::logic_kind::DOOR && usize::from(record.link) == mover
        }) else {
            panic!("PXBSP mover {mover} has no cooked Door logic record");
        };
        let fired =
            self.logic
                .fire_index(LOGIC, index, psx_game_runtime::logic::use_type::TOGGLE, now);
        if fired {
            self.dispatch_logic_effects();
        }
        fired
    }
}

/// The interactable paired with logic record `logic_index`, if any.
pub(super) fn interactable_for_logic(logic_index: usize) -> Option<&'static InteractableRecord> {
    let logic_index = u16::try_from(logic_index).ok()?;
    INTERACTABLES
        .iter()
        .find(|interactable| interactable.logic == logic_index)
}
