//! Opt-in training duel. Synthetic pad input passes through the ordinary player controller.
use super::*;
use psx_engine::{AnalogSticks, PadMode, PadState};
use psx_game_runtime::{combat_policy, entities::GameEntityState};

// All-zero storage is inactive, matching Playtest's zeroed initialization.
pub(super) struct Duel {
    pub active: bool,
    pub finished: bool,
    target: usize,
    started: u32,
    releasing_start: bool,
    last_seen: [i32; 3],
    seed: u32,
    previous: u16,
    movement: [i32; 2],
    aim: bool,
    sprint: bool,
    escaping: bool,
    escape_heading: [i32; 2],
    escape_rethink: u32,
    escape_until: u32,
    dodge_ready: u32,
    exchange: psx_game_runtime::ranged_tactics::RangedExchange,
    combat_role: u8,
    last_roles: [u8; 2],
    last_attack: u8,
    repeats: u8,
    next_think: u32,
    pub reason: u8,
    pub outcome: u8,
    intent: u8,
    last_snapshot: [u32; 23],
    last_progress: u32,
}
impl Duel {
    pub(super) fn started_tick(&self) -> u32 {
        self.started
    }
    fn roll(&mut self) -> u16 {
        let mut x = self.seed.max(1);
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.seed = x;
        (x % 100) as u16
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
    /// Called before the gameplay controller; physical input is restored by the caller.
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
            self.anim_start_tick = ctx.sim_tick;
            self.game_entities
                .seed_training_actor(GAME_ENTITIES, target, seed ^ 0x9e3779b9);
            self.duel = Duel {
                active: true,
                finished: false,
                target,
                started: ctx.sim_tick.as_u32(),
                releasing_start: true,
                last_seen: self.game_entities.position(target),
                seed,
                previous: 0,
                movement: [0, 0],
                aim: false,
                sprint: false,
                escaping: false,
                escape_heading: [0, 0],
                escape_rethink: 0,
                escape_until: 0,
                dodge_ready: 0,
                exchange: psx_game_runtime::ranged_tactics::RangedExchange::EMPTY,
                combat_role: 0,
                last_roles: [255; 2],
                last_attack: 255,
                repeats: 0,
                next_think: 0,
                reason: 0,
                outcome: 0,
                intent: 0,
                last_snapshot: [0; 23],
                last_progress: 0,
            };
            log_values("duel:start", &[seed, target as u32]);
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
        if !self.duel.active {
            return;
        }
        if self.duel.releasing_start && !ctx.is_held(button::SELECT | button::L2) {
            self.duel.releasing_start = false;
        }
        self.duel.exchange.tick(1);
        let mut buttons = 0;
        if !self.duel.finished && ctx.sim_tick.as_u32() >= self.duel.next_think {
            self.duel.next_think = ctx.sim_tick.as_u32() + 12;
            let i = self.duel.target;
            let record = &GAME_ENTITIES[i];
            let p = self.motor.position();
            let e = self.game_entities.position(i);

            let height = self.character.map_or(40, |c| c.height);
            let from = RoomPoint::new(p.x, p.y + height / 2, p.z);
            let to = RoomPoint::new(e[0], e[1] + i32::from(record.height) / 2, e[2]);
            let visible = {
                let mut blockers = psx_engine::FixedScratch::<
                    CharacterCollisionAabb,
                    MAX_STATIC_PROP_AABB_BLOCKERS,
                >::new();
                let valid = self
                    .collect_static_prop_aabb_blockers_checked_into(&mut blockers)
                    .is_some();
                valid
                    && self.bsp.as_mut().is_none_or(|bsp| {
                        bsp.trace_point_segment(from, to, blockers.as_slice(), &self.destructibles)
                            .is_ok_and(|t| !t.hit())
                    })
            };
            if visible {
                self.duel.last_seen = e;
            }
            let seen = self.duel.last_seen;
            let dx = seen[0].saturating_sub(p.x);
            let dz = seen[2].saturating_sub(p.z);
            let distance =
                isqrt_i32(square_i32_saturating(dx).saturating_add(square_i32_saturating(dz)))
                    .max(1);
            let active = self.player_stance.active();
            let pool = |c| self.player_vitality.pool(c);
            let hp = [
                pool(VitalityChannelId::One).current(),
                pool(VitalityChannelId::Two).current(),
            ];
            let maxima = [
                pool(VitalityChannelId::One).maximum(),
                pool(VitalityChannelId::Two).maximum(),
            ];
            let entry = i32::from(record.preferred_distance.max(record.attack_min_range))
                + i32::from(record.spacing_tolerance);
            let melee = combat_policy::wants_melee(
                distance,
                entry,
                i32::from(record.spacing_tolerance),
                active == VitalityChannelId::One,
            );
            let press = self.duel.last_attack == 2
                && self.duel.repeats >= 2
                && distance < i32::from(record.preferred_distance) * 3;
            let (wanted, reason) = combat_policy::stance_choice(
                melee,
                active,
                hp,
                maxima,
                press,
                if visible {
                    Some((
                        self.game_entities.stance(i),
                        [
                            self.game_entities.health(i),
                            self.game_entities.health_secondary(i),
                        ],
                    ))
                } else {
                    None
                },
            );
            let separated = distance
                > (self.character.map_or(40, |c| c.radius) + i32::from(record.radius) + 24) * 2;
            let wanted = if self
                .combat_flow
                .preferred_stance(wanted.index() as u8, separated)
                == 0
            {
                VitalityChannelId::One
            } else {
                VitalityChannelId::Two
            };
            self.duel.reason = self.combat_flow.stance_reason(reason as u8, separated);
            self.duel.movement = [0, 0];
            self.duel.aim = false;
            self.duel.sprint = false;
            self.duel.intent = 0;
            let free = self.motor.action().is_idle()
                && self.anim_lock_until_tick <= ctx.sim_tick
                && !self
                    .player_stance
                    .swap_in_progress(&self.player_stance_config);
            let swapping = visible && wanted != active && free && self.player_stance.can_swap();
            if swapping {
                buttons |= button::TRIANGLE;
                self.duel.intent = 5;
            }
            {
                // Movement follows the intended weapon phase through a legal stance swap.
                let ranged = wanted == VitalityChannelId::Two;
                let reach = self.character.map_or(40, |c| c.radius) + i32::from(record.radius) + 24;
                let (fire_near, fire_far) = psx_game_runtime::combat_flow::ranged_band(
                    i32::from(record.preferred_distance),
                    reach,
                    i32::from(record.attack_max_range),
                );
                let ranged_order = if ranged && distance > reach * 2 {
                    Some(self.duel_ranged_order(p, seen, height, fire_far))
                } else {
                    self.duel.exchange.reset();
                    None
                };
                self.duel.combat_role = ranged_order.map_or(0, |_| self.duel.exchange.mode);
                let near = if ranged { fire_near } else { reach };
                let far = if ranged { fire_far } else { reach };
                let settle = fire_near + (fire_far - fire_near) / 2;
                let was_escaping = self.duel.escaping;
                self.duel.escaping = ranged
                    && visible
                    && (distance < fire_near || (self.duel.escaping && distance < settle));
                if self.duel.escaping && !was_escaping {
                    self.duel.escape_until = ctx.sim_tick.as_u32() + 180;
                }
                if self.duel.escaping && ctx.sim_tick.as_u32() >= self.duel.escape_until {
                    self.combat_flow.contest_space();
                    self.duel.escaping = false;
                }
                let mut direction = if !visible || distance > far {
                    1
                } else if self.duel.escaping {
                    -1
                } else {
                    0
                };
                if self.duel.escaping && self.motor.stamina_q12() > 768 {
                    self.duel.sprint = true;
                }
                let enemy_state = self.game_entities.state(i);
                let roll = self.duel.roll();
                // Observe committed animation states on a 12-tick decision cadence,
                // never future player inputs or the enemy's random stream.
                if visible
                    && !swapping
                    && wanted == active
                    && !self.duel.sprint
                    && ctx.sim_tick.as_u32() >= self.duel.dodge_ready
                    && enemy_state == GameEntityState::Windup
                    && self.game_entities.state_age(i) >= u16::from(record.windup_ticks) / 2
                    && distance < reach * 2
                    && free
                    && self.motor.stamina_q12() > 768
                    && roll < 75
                {
                    buttons |= button::CIRCLE;
                    direction = -1;
                    self.duel.intent = 4;
                    self.duel.combat_role = 6;
                    self.duel.dodge_ready = ctx.sim_tick.as_u32() + 90;
                } else if !ranged
                    && (self.motor.stamina_q12() < 1024
                        || matches!(
                            enemy_state,
                            GameEntityState::Windup | GameEntityState::Attack
                        ))
                {
                    self.duel.combat_role = if self.motor.stamina_q12() < 1024 {
                        8
                    } else {
                        6
                    };
                    if free && distance < reach * 2 {
                        direction = -1;
                    }
                } else if visible
                    && !swapping
                    && wanted == active
                    && !self
                        .player_stance
                        .swap_in_progress(&self.player_stance_config)
                    && ranged_order.is_none_or(|o| o.fire)
                    && !self.duel.escaping
                    && distance <= far
                    && distance >= if ranged { near } else { 0 }
                {
                    self.duel.aim = false; // Direct R2 fire exercises the real optional-aim controls.
                    if !ranged
                        && matches!(
                            enemy_state,
                            GameEntityState::Recover | GameEntityState::Staggered
                        )
                    {
                        self.duel.combat_role = 7;
                    }
                    let kind = combat_policy::attack_choice(
                        ranged,
                        distance < reach * 3 / 4,
                        self.duel.last_attack,
                        self.duel.repeats,
                        roll,
                    );
                    if !ranged || self.combat_flow.can_shoot() {
                        buttons |= if kind == 0 { button::R1 } else { button::R2 };
                        self.duel.intent = if ranged { 3 } else { 2 };
                    }
                }
                if direction != 0 {
                    self.duel.movement = [dx * direction, dz * direction];
                    if self.duel.escaping {
                        if ctx.sim_tick.as_u32() >= self.duel.escape_rethink {
                            self.duel.escape_heading = self.duel_escape_heading(p, seen, height);
                            self.duel.escape_rethink = ctx.sim_tick.as_u32() + 24;
                        }
                        self.duel.movement = self.duel.escape_heading;
                        if self.duel.movement == [0, 0] {
                            self.combat_flow.contest_space();
                            self.duel.sprint = false;
                        }
                    } else {
                        self.duel.escape_rethink = 0;
                        self.duel.escape_heading = [0, 0];
                    }
                    if self.duel.intent == 0 {
                        self.duel.intent = if direction > 0 { 1 } else { 6 };
                    }
                } else if visible && !free && !ranged {
                    // Hold footing while an attack is committed.
                    self.duel.movement = [0, 0];
                }
            }
            // Cover, peeking and real bullet evasion override ordinary radial spacing.
            if self.duel.combat_role > 0 && self.duel.combat_role <= 5 {
                let order = self.duel_last_ranged_order(p);
                self.duel.sprint = false;
                self.duel.movement = if order.moving {
                    [order.destination[0] - p.x, order.destination[2] - p.z]
                } else {
                    [0, 0]
                };
                if order.evade
                    && free
                    && self.motor.stamina_q12() >= 1024
                    && ctx.sim_tick.as_u32() >= self.duel.dodge_ready
                {
                    buttons |= button::CIRCLE;
                    self.duel.dodge_ready = ctx.sim_tick.as_u32() + 90;
                }
            }
            if !self.is_locked() && visible {
                buttons |= button::R3;
            }
            log_values(
                "duel:decision",
                &[
                    ctx.sim_tick.as_u32() - self.duel.started,
                    self.duel.intent as u32,
                    self.duel.reason as u32,
                    buttons as u32,
                    distance as u32,
                ],
            );
        }
        // Decisions stay at 5 Hz, but steering must stop at the chosen point.
        // Holding a 12-tick-old direction overshoots small cover/peek targets.
        if (1..=5).contains(&self.duel.combat_role) {
            let p = self.motor.position();
            let order = self.duel_last_ranged_order(p);
            self.duel.movement = if order.moving {
                [order.destination[0] - p.x, order.destination[2] - p.z]
            } else {
                [0, 0]
            };
            self.duel.sprint = false;
        }
        if self.duel.aim {
            buttons |= button::L2;
        }
        if self.duel.sprint {
            buttons |= button::CIRCLE;
        }
        let [x, z] = self.duel.movement;
        let len =
            isqrt_i32(square_i32_saturating(x).saturating_add(square_i32_saturating(z))).max(1);
        let forward = self.camera.yaw().add(Angle::HALF);
        let right = forward.sub(Angle::QUARTER);
        let f = (forward.sin().mul_i32(x) + forward.cos().mul_i32(z)) * 110 / len;
        let r = (right.sin().mul_i32(x) + right.cos().mul_i32(z)) * 110 / len;
        let mut pad = PadState::NONE;
        pad.mode = PadMode::Analog;
        pad.id_low = 0x73;
        pad.buttons = psx_pad::ButtonState::from_bits(buttons);
        pad.sticks = AnalogSticks {
            left_x: (128 + r.clamp(-127, 127)) as u8,
            left_y: (128 - f.clamp(-127, 127)) as u8,
            right_x: 128,
            right_y: 128,
        };
        ctx.pad_prev = pad;
        ctx.pad_prev.buttons = psx_pad::ButtonState::from_bits(self.duel.previous);
        ctx.pad = pad;
        self.duel.previous = buttons;
    }

    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    fn duel_ranged_order(
        &mut self,
        p: RoomPoint,
        target: [i32; 3],
        height: i32,
        far: i32,
    ) -> psx_game_runtime::ranged_tactics::RangedOrder {
        let radius = self.character.map_or(12, |c| c.radius);
        let from = [p.x, p.y, p.z];
        let threat = self.combat_projectiles.incoming_threat(
            psx_game_runtime::projectiles::CombatTeam::Player,
            self.room_index,
            from,
            radius,
            height,
        );
        let mut blockers =
            psx_engine::FixedScratch::<CharacterCollisionAabb, MAX_STATIC_PROP_AABB_BLOCKERS>::new(
            );
        let valid = self
            .collect_static_prop_aabb_blockers_checked_into(&mut blockers)
            .is_some();
        let bsp = &mut self.bsp;
        let destructibles = &self.destructibles;
        self.duel.exchange.decide(
            from,
            target,
            radius,
            height,
            far,
            self.combat_flow.energy,
            threat,
            &mut |a, b| {
                valid
                    && bsp.as_mut().is_none_or(|bsp| {
                        bsp.trace_point_segment(
                            RoomPoint::new(a[0], a[1], a[2]),
                            RoomPoint::new(b[0], b[1], b[2]),
                            blockers.as_slice(),
                            destructibles,
                        )
                        .is_ok_and(|trace| !trace.hit())
                    })
            },
        )
    }
    fn duel_last_ranged_order(
        &self,
        p: RoomPoint,
    ) -> psx_game_runtime::ranged_tactics::RangedOrder {
        self.duel.exchange.current_order([p.x, p.y, p.z])
    }

    /// Short local probes choose an escape lane; the ordinary motor still owns collision.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    fn duel_escape_heading(&mut self, p: RoomPoint, target: [i32; 3], height: i32) -> [i32; 2] {
        let dx = p.x.saturating_sub(target[0]);
        let dz = p.z.saturating_sub(target[2]);
        let side = if self.duel.seed & 1 == 0 { 1 } else { -1 };
        let cached = self.duel.escape_heading;
        let candidates = [
            cached,
            [dx, dz],
            [dx - dz * side, dz + dx * side],
            [dx + dz * side, dz - dx * side],
            [-dz * side, dx * side],
            [dz * side, -dx * side],
        ];
        let mut blockers =
            psx_engine::FixedScratch::<CharacterCollisionAabb, MAX_STATIC_PROP_AABB_BLOCKERS>::new(
            );
        if self
            .collect_static_prop_aabb_blockers_checked_into(&mut blockers)
            .is_none()
        {
            return [0, 0];
        }
        for v in candidates {
            let len =
                isqrt_i32(square_i32_saturating(v[0]).saturating_add(square_i32_saturating(v[1])));
            if len == 0 || i64::from(v[0]) * i64::from(dx) + i64::from(v[1]) * i64::from(dz) < 0 {
                continue;
            }
            let step = [v[0] * 160 / len, v[1] * 160 / len];
            let radius = self.character.map_or(12, |c| c.radius) + 8;
            let side = [-v[1] * radius / len, v[0] * radius / len];
            let clear = self.bsp.as_mut().is_none_or(|bsp| {
                [-1, 0, 1].into_iter().all(|offset| {
                    let from = RoomPoint::new(
                        p.x + side[0] * offset,
                        p.y + height / 2,
                        p.z + side[1] * offset,
                    );
                    let to = RoomPoint::new(from.x + step[0], from.y, from.z + step[1]);
                    bsp.trace_point_segment(from, to, blockers.as_slice(), &self.destructibles)
                        .is_ok_and(|trace| !trace.hit())
                })
            });
            if clear {
                return step;
            }
        }
        [0, 0]
    }

    fn duel_end(&mut self, result: u32, ctx: &Ctx) {
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
        let i = self.duel.target;
        let p = self.motor.position();
        let e = self.game_entities.position(i);
        let tick = ctx.sim_tick.as_u32() - self.duel.started;
        let mut snap = [
            tick,
            p.x as u32,
            p.z as u32,
            e[0] as u32,
            e[2] as u32,
            self.player_vitality.pool(VitalityChannelId::One).current() as u32,
            self.player_vitality.pool(VitalityChannelId::Two).current() as u32,
            self.game_entities.health(i) as u32,
            self.game_entities.health_secondary(i) as u32,
            self.player_stance.active().index() as u32,
            self.game_entities.stance(i).index() as u32,
            self.anim_state.action().to_index() as u32,
            self.game_entities.state(i) as u32,
            self.duel.intent as u32,
            self.duel.reason as u32,
            self.motor.stamina_q12() as u32,
            self.game_entities.tactical_snapshot(i).generation as u32,
            self.anim_start_tick.as_u32(),
            self.game_entities.tactical_snapshot(i).goal as u32,
            self.game_entities.tactical_snapshot(i).stance_reason as u32,
            self.game_entities.attack_kind(i) as u32,
            self.combat_flow.energy as u32,
            self.game_entities.energy(i) as u32,
        ];
        let roles = [self.duel.combat_role, self.game_entities.combat_role(i)];
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
        // A broken-pool forced switch is not an accepted policy request.
        snap[14] = if snap[9] != old[9] {
            if self.duel.previous & button::TRIANGLE != 0 {
                self.duel.reason as u32
            } else {
                4
            }
        } else {
            old[14]
        };
        let damage = (5..9).any(|n| snap[n] < old[n]);
        let changed = snap[9..13] != old[9..13] || snap[17] != old[17];
        if damage {
            self.duel.last_progress = tick;
        }
        if snap[17] != old[17] {
            let kind = match self.anim_state {
                PlayerAnim::RangedAttack => Some(2),
                PlayerAnim::HeavyAttack => Some(1),
                PlayerAnim::LightAttack
                | PlayerAnim::LightAttackFollowup
                | PlayerAnim::LightAttackFinisher => Some(0),
                _ => None,
            };
            if let Some(kind) = kind {
                self.duel.repeats = if self.duel.last_attack == kind {
                    self.duel.repeats.saturating_add(1)
                } else {
                    1
                };
                self.duel.last_attack = kind;
            }
        }
        if tick == 0 || tick % 60 == 0 || damage || changed {
            log_values("duel:sample", &snap);
        }
        self.duel.last_snapshot = snap;
        let player_dead =
            self.player_vitality.is_defeated() || self.hazard_death_ticks_remaining != 0;
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
