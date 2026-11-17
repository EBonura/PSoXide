//! Opt-in training duel. Aletha's moves are decided by the tactical
//! controller every entity uses (`GameEntities::actor_intent`) and run by her
//! own rules, the ones the pad drives.
use super::*;
use psx_game_runtime::combat_intent::{CombatAction, CombatActions};
use psx_game_runtime::{entities::GameEntityState, projectiles::CombatTeam};

// All-zero storage is inactive, matching Playtest's zeroed initialization.
/// Columns of one `duel:sample` row: tick, player x/z, enemy x/z, player
/// health, enemy health, stances, action and state, her tactical goal and
/// stance reason, stamina, tactical goal, action start, goal, enemy stance
/// reason, attack kind, then both energies. The summariser in `psxed-mcp`
/// reads the same order.
const SAMPLE_COLUMNS: usize = 21;

/// `duel:event` kind: the actor parried a hit.
pub(super) const DUEL_EVENT_PARRY: u32 = 1;
/// `duel:event` kind: the actor's attack was a crit.
pub(super) const DUEL_EVENT_CRIT: u32 = 2;
/// `duel:event` kind: the actor was knocked down.
pub(super) const DUEL_EVENT_KNOCKDOWN: u32 = 3;

/// Ticks her controller keeps the weapon raised after it asks for a shot:
/// her rules fire only from a raised weapon (8 ticks), and the press waits
/// in her attack buffer meanwhile. The shot itself is her clip's.
const DUEL_AIM_TICKS: u32 = 24;

pub(super) struct Duel {
    pub active: bool,
    pub finished: bool,
    target: usize,
    /// The second training entity when the duel sets two entities against
    /// each other (`target` is the first); None in a duel with the player.
    rival: Option<u8>,
    started: u32,
    releasing_start: bool,
    /// The record her tactical controller decides by: the training enemy's
    /// persona (spacing, reach, reaction, kits, home) with her own body, as
    /// the host harness gives her.
    persona: LevelGameEntityRecord,
    /// Her controller's latest intent, and whether its presses are still to
    /// be run (they run on the first tick after the think).
    plan: CombatIntent,
    fresh: bool,
    /// The actions she held last tick, for the release edges.
    held: CombatActions,
    /// The tick until which her controller keeps her weapon raised.
    aim_until: u32,
    pub outcome: u8,
    last_snapshot: [u32; SAMPLE_COLUMNS],
    last_progress: u32,
    last_roles: [u8; 2],
}
impl Duel {
    pub(super) fn started_tick(&self) -> u32 {
        self.started
    }
}

pub(super) fn log_values(label: &str, values: &[u32]) {
    #[cfg(target_arch = "mips")]
    {
        psx_rt::tty::print(label);
        for v in values {
            psx_rt::tty::print(" ");
            psx_rt::tty::print_hex_u32(*v);
        }
        psx_rt::tty::println("");
    }
    #[cfg(not(target_arch = "mips"))]
    {
        let _ = (label, values);
    }
}

impl Playtest {
    /// Start or stop the duel (Select+L2), and hand the pad back on any
    /// physical press. Called before the gameplay controller.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn duel_pad(&mut self, ctx: &mut Ctx) {
        let toggle = ctx.is_held(button::SELECT) && ctx.just_pressed(button::L2);
        if toggle {
            if self.duel.active {
                self.duel_end(4, ctx);
                self.duel.active = false;
                return;
            }
            let Some(target) = GAME_ENTITIES
                .iter()
                .position(|r| r.flags & psx_level::game_entity_flags::TRAINING != 0)
            else {
                return;
            };
            // A duel must exercise ranged combat on both characters.
            if !self.player_has_ranged_weapon()
                || GAME_ENTITIES[target].ranged_attack_clip == u16::MAX
            {
                log_values("duel:unavailable", &[1]);
                return;
            }
            let seed = if ctx.pad.sticks.right_x == 128 {
                1
            } else {
                u32::from(ctx.pad.sticks.right_x).max(1)
            };
            self.reset_new_game();
            self.respawn_after_death();
            self.gameplay_epoch = ctx.sim_tick;
            self.gameplay_epoch_set = true;
            self.game_entities.actor_action_mut().start = ctx.sim_tick;
            self.game_entities.seed_training_actor(
                GAME_ENTITIES.as_slice(),
                target,
                seed ^ 0x9e3779b9,
            );
            // Her persona is the training placement's, home included (the
            // host harness cooks her into that placement): the leash is the
            // training ground's, not her spawn's.
            let mut persona = GAME_ENTITIES[target];
            if let Some(character) = self.character {
                persona.radius = character.radius.clamp(0, i32::from(u16::MAX)) as u16;
                persona.height = character.height.clamp(0, i32::from(u16::MAX)) as u16;
            }
            self.duel = Duel {
                active: true,
                finished: false,
                target,
                rival: None,
                started: ctx.sim_tick.as_u32(),
                releasing_start: true,
                persona,
                plan: CombatIntent::IDLE,
                fresh: false,
                held: CombatActions::NONE,
                aim_until: 0,
                outcome: 0,
                last_snapshot: [0; SAMPLE_COLUMNS],
                last_progress: 0,
                last_roles: [255; 2],
            };
            // The third value is the sample format: 1 is one health value
            // per fighter (before it, two pools each and no third value).
            log_values("duel:start", &[seed, target as u32, 1]);
            if ctx.pad.sticks.right_y == 0 {
                self.duel_start_rivals(target, seed);
            } else {
                // Both fighters start engaged, as the rivals duel's do: each
                // notices the other, reaction pause included.
                self.game_entities
                    .engage_actor(&persona, target, seed.rotate_left(8) ^ 0x9e3779b9);
                self.game_entities
                    .engage(target, psx_game_runtime::combatant::CombatantId::PLAYER);
                // She keeps her rival in view as an entity faces its target.
                self.lock_target = Some(target);
            }
        } else if self.duel.active
            && !self.duel.releasing_start
            && ctx.pad.buttons.bits() != 0
            && !(ctx.is_held(button::SELECT) && ctx.is_held(button::L2))
        {
            // Any physical button takes control back. The human pad is left intact.
            self.duel_end(4, ctx);
            self.duel.active = false;
            return;
        }
        if self.duel.releasing_start && !ctx.is_held(button::SELECT | button::L2) {
            self.duel.releasing_start = false;
        }
    }

    /// The record her tactical controller decides by.
    pub(super) fn duel_persona(&self) -> LevelGameEntityRecord {
        self.duel.persona
    }

    /// Whether her tactical controller drives her this tick.
    pub(super) fn duel_drives_actor(&self) -> bool {
        self.duel.active && !self.duel.finished && self.duel.rival.is_none()
    }

    /// Her controller's think, at the entities' cadence: what holds her now
    /// (0 nothing, 1 an action, 2 an attack), then its intent.
    pub(super) fn duel_actor_busy(&self, now: SimTick) -> u8 {
        if self.game_entities.actor_action().locked(now) {
            if player_anim_is_attack(self.anim_state) {
                2
            } else {
                1
            }
        } else {
            u8::from(!self.game_entities.actor_motor().action().is_idle())
        }
    }

    /// Store her controller's latest decision.
    pub(super) fn duel_set_plan(&mut self, ctx: &Ctx, plan: CombatIntent) {
        self.duel.plan = plan;
        self.duel.fresh = true;
        let snapshot = self
            .game_entities
            .tactical_snapshot(RuntimeGameEntities::ACTOR_SLOT);
        log_values(
            "duel:decision",
            &[
                ctx.sim_tick.as_u32() - self.duel.started,
                snapshot.goal as u32,
                u32::from(snapshot.stance_reason),
                CombatAction::ALL
                    .iter()
                    .enumerate()
                    .filter(|(_, action)| plan.pressed.has(**action))
                    .fold(0, |bits, (n, _)| bits | 1 << n),
                0,
            ],
        );
    }

    /// The intent her rules run this tick: the pad's, or in a duel her
    /// controller's latest decision, its presses on the first tick after
    /// it, with the release edges a pad would give.
    pub(super) fn duel_intent(&mut self, pad: CombatIntent, now: SimTick) -> CombatIntent {
        if !self.duel_drives_actor() {
            return pad;
        }
        let mut intent = self.duel.plan;
        intent.pressed = if core::mem::take(&mut self.duel.fresh) {
            // She keeps her rival in view as an entity faces its target: a
            // lost lock is taken again at the next think.
            intent
                .pressed
                .with_if(CombatAction::Lock, !self.is_locked())
        } else {
            CombatActions::NONE
        };
        if intent.pressed.has(CombatAction::Shoot) {
            self.duel.aim_until = now.as_u32().saturating_add(DUEL_AIM_TICKS);
        }
        let mut held = intent.held;
        for action in CombatAction::ALL {
            held = held.with_if(action, intent.pressed.has(action));
        }
        held = held.with_if(CombatAction::Aim, now.as_u32() < self.duel.aim_until);
        let mut released = CombatActions::NONE;
        for action in CombatAction::ALL {
            released = released.with_if(action, self.duel.held.has(action) && !held.has(action));
        }
        intent.held = held;
        intent.released = released;
        self.duel.held = held;
        intent
    }

    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn duel_log_event(&self, ctx: &Ctx, actor: u32, kind: u32) {
        if self.duel.active && !self.duel.finished {
            log_values(
                "duel:event",
                &[ctx.sim_tick.as_u32() - self.duel.started, actor, kind],
            );
        }
    }

    /// Set the duel's training entity against the next training entity (a
    /// right stick held up at the start): each the other's rival, on
    /// opposite sides, the player out of it.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    fn duel_start_rivals(&mut self, first: usize, seed: u32) {
        let Some(second) = GAME_ENTITIES
            .iter()
            .enumerate()
            .skip(first + 1)
            .find(|(_, r)| r.flags & psx_level::game_entity_flags::TRAINING != 0)
            .map(|(index, _)| index)
        else {
            log_values("duel:unavailable", &[2]);
            return;
        };
        self.game_entities.seed_training_actor(
            GAME_ENTITIES.as_slice(),
            second,
            seed.rotate_left(16) ^ 0x9e3779b9,
        );
        self.game_entities
            .set_rival(first, Some(second), CombatTeam::Enemy);
        self.game_entities
            .set_rival(second, Some(first), CombatTeam::Rival);
        self.duel.rival = Some(second as u8);
        log_values("duel:rivals", &[first as u32, second as u32]);
    }

    /// A duel event of the entity in `slot`: actor 0 is the duel's training
    /// entity, 1 its rival.
    pub(super) fn duel_log_rival_event(&self, ctx: &Ctx, slot: usize, kind: u32) {
        self.duel_log_event(ctx, self.duel_side(slot), kind);
    }

    /// The duel side of the entity in `slot`: 1 in a duel with the player
    /// (she is 0); in a duel between entities, 0 for the training entity and
    /// 1 for its rival.
    pub(super) fn duel_side(&self, slot: usize) -> u32 {
        u32::from(self.duel.rival.is_none() || slot != self.duel.target)
    }

    /// One sample of a fight between two entities: tick, both positions
    /// (x/z), health, stance, state, energy; then knockdowns and the end.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    fn duel_observe_rivals(&mut self, ctx: &Ctx, second: usize) {
        let first = self.duel.target;
        let tick = ctx.sim_tick.as_u32() - self.duel.started;
        let a = self.game_entities.position(first);
        let b = self.game_entities.position(second);
        let mut snap = [0u32; SAMPLE_COLUMNS];
        let row = [
            tick,
            a[0] as u32,
            a[2] as u32,
            b[0] as u32,
            b[2] as u32,
            u32::from(self.game_entities.health(first)),
            u32::from(self.game_entities.health(second)),
            self.game_entities.stance(first).index() as u32,
            self.game_entities.stance(second).index() as u32,
            self.game_entities.state(first) as u32,
            self.game_entities.state(second) as u32,
            u32::from(self.game_entities.energy(first)),
            u32::from(self.game_entities.energy(second)),
        ];
        snap[..row.len()].copy_from_slice(&row);
        let old = self.duel.last_snapshot;
        let damage = (5..7).any(|n| snap[n] < old[n]);
        if damage {
            self.duel.last_progress = tick;
        }
        for actor in 0..2 {
            if snap[9 + actor] == GameEntityState::KnockedDown as u32
                && old[9 + actor] != snap[9 + actor]
            {
                self.duel_log_event(ctx, actor as u32, DUEL_EVENT_KNOCKDOWN);
            }
        }
        if tick == 0 || tick % 60 == 0 || damage || snap[7..11] != old[7..11] {
            log_values("duel:rival-sample", &row);
        }
        self.duel.last_snapshot = snap;
        let first_dead = self.game_entities.state(first) == GameEntityState::Dead;
        let second_dead = self.game_entities.state(second) == GameEntityState::Dead;
        if first_dead || second_dead {
            self.duel_end(
                if first_dead && second_dead {
                    3
                } else if second_dead {
                    1
                } else {
                    2
                },
                ctx,
            );
        } else if tick >= 10800 {
            self.duel_end(5, ctx);
        } else if tick.saturating_sub(self.duel.last_progress) >= 1800 {
            self.duel_end(6, ctx);
        }
    }

    fn duel_end(&mut self, result: u32, ctx: &Ctx) {
        if let Some(second) = self.duel.rival {
            let first = self.duel.target;
            self.game_entities.set_rival(first, None, CombatTeam::Enemy);
            self.game_entities
                .set_rival(usize::from(second), None, CombatTeam::Enemy);
        }
        if !self.duel.finished {
            self.duel.finished = true;
            self.duel.outcome = result as u8;
            log_values(
                "duel:end",
                &[ctx.sim_tick.as_u32() - self.duel.started, result],
            );
        }
    }

    /// Capture after both real contact resolvers, before any future respawn.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn duel_observe(&mut self, ctx: &Ctx) {
        if !self.duel.active || self.duel.finished {
            return;
        }
        if let Some(second) = self.duel.rival {
            self.duel_observe_rivals(ctx, usize::from(second));
            return;
        }
        let i = self.duel.target;
        let p = self.game_entities.actor_motor().position();
        let e = self.game_entities.position(i);
        let tick = ctx.sim_tick.as_u32() - self.duel.started;
        let actor = self
            .game_entities
            .tactical_snapshot(RuntimeGameEntities::ACTOR_SLOT);
        let snap = [
            tick,
            p.x as u32,
            p.z as u32,
            e[0] as u32,
            e[2] as u32,
            self.game_entities.actor().health() as u32,
            self.game_entities.health(i) as u32,
            self.game_entities.actor().stance.active().index() as u32,
            self.game_entities.stance(i).index() as u32,
            self.anim_state.action().to_index() as u32,
            self.game_entities.state(i) as u32,
            actor.goal as u32,
            u32::from(actor.stance_reason),
            self.game_entities.actor_motor().stamina_q12() as u32,
            self.game_entities.tactical_snapshot(i).generation as u32,
            self.game_entities.actor_action().start.as_u32(),
            self.game_entities.tactical_snapshot(i).goal as u32,
            self.game_entities.tactical_snapshot(i).stance_reason as u32,
            self.game_entities.attack_kind(i) as u32,
            self.game_entities.actor().flow.energy as u32,
            self.game_entities.energy(i) as u32,
        ];
        let roles = [
            self.game_entities
                .combat_role(RuntimeGameEntities::ACTOR_SLOT),
            self.game_entities.combat_role(i),
        ];
        for actor in 0..2 {
            if roles[actor] != self.duel.last_roles[actor] {
                log_values(
                    "duel:tactic",
                    &[tick, actor as u32, u32::from(roles[actor])],
                );
            }
        }
        self.duel.last_roles = roles;
        let old = self.duel.last_snapshot;
        let damage = (5..7).any(|n| snap[n] < old[n]);
        let changed = snap[7..11] != old[7..11] || snap[15] != old[15];
        if damage {
            self.duel.last_progress = tick;
        }
        // A knockdown is the moment either fighter enters its down state.
        if snap[9] == CharacterAnimationAction::Knockdown.to_index() as u32 && old[9] != snap[9] {
            self.duel_log_event(ctx, 0, DUEL_EVENT_KNOCKDOWN);
        }
        if snap[10] == GameEntityState::KnockedDown as u32 && old[10] != snap[10] {
            self.duel_log_event(ctx, 1, DUEL_EVENT_KNOCKDOWN);
        }
        if tick == 0 || tick % 60 == 0 || damage || changed {
            log_values("duel:sample", &snap);
        }
        self.duel.last_snapshot = snap;
        let player_dead =
            self.game_entities.actor().defeated() || self.hazard_death_ticks_remaining != 0;
        let enemy_dead = self.game_entities.state(i) == GameEntityState::Dead;
        if player_dead || enemy_dead {
            self.duel_end(
                if player_dead && enemy_dead {
                    3
                } else if enemy_dead {
                    1
                } else {
                    2
                },
                ctx,
            );
        } else if tick >= 10800 {
            self.duel_end(5, ctx);
        } else if tick.saturating_sub(self.duel.last_progress) >= 1800 {
            self.duel_end(6, ctx);
        }
    }
}
