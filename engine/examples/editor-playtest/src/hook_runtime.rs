//! Authored elevated hooks share the ranged reticle and the dash body effect.
use super::*;
use psx_game_runtime::hook_points::{self as hooks, HookTravel};
use psx_level::EntityKind;

impl Playtest {
    pub(super) fn arch_slot(index: usize) -> usize {
        ENTITIES[..index]
            .iter()
            .filter(|h| h.kind == EntityKind::HookPoint)
            .count()
    }

    pub(super) fn arch_anchor(&self, index: usize, now: u32) -> RoomPoint {
        let hook = &ENTITIES[index];
        RoomPoint::new(
            hook.x
                + hooks::arch_offset(
                    Self::arch_slot(index),
                    now.wrapping_sub(self.gameplay_epoch.as_u32()),
                ),
            hook.y + hooks::MARKER_HEIGHT,
            hook.z,
        )
    }

    fn arch_perch(&self, index: usize, now: u32) -> RoomPoint {
        let p = self.arch_anchor(index, now);
        let yaw = Self::arch_facing(index);
        let [x, y, z] = hooks::PERCH_HAND;
        RoomPoint::new(
            p.x - yaw.cos().mul_i32(x) - yaw.sin().mul_i32(z),
            p.y - y,
            p.z + yaw.sin().mul_i32(x) - yaw.cos().mul_i32(z),
        )
    }

    pub(super) fn arch_facing(index: usize) -> Angle {
        Angle::from_q12(ENTITIES[index].yaw as u16).add(Angle::HALF)
    }

    fn hook_overlay_absolute_tick(&self) -> u32 {
        self.overlay_sim_tick
            .as_u32()
            .wrapping_add(self.gameplay_epoch.as_u32())
    }

    /// One presentation drives skinning, weapon sockets and combat hit volumes.
    pub(super) fn arch_player_pose(
        &self,
        pose: PlayerActorPoseSnapshot,
        now: SimTick,
    ) -> PlayerActorPoseSnapshot {
        let Some(index) = self.hook_attached else {
            return pose;
        };
        let Some(character) = self.character.as_ref() else {
            return pose;
        };
        let Some(clip) = character
            .action_clip(CharacterAnimationAction::ArchPerch)
            .to_option()
        else {
            return pose;
        };
        let Some(animation) = pose.model().clip(&self.clips, clip) else {
            return pose;
        };
        if animation.frame_count() != hooks::PERCH_FRAMES {
            return pose;
        }
        let facing = Self::arch_facing(index);
        let target = self.ranged_target();
        let p = self.motor.position();
        let dx = target[0] - p.x;
        let dz = target[2] - p.z;
        let yaw = psx_math::atan2_q12(dx, dz);
        let relative = ((i32::from(yaw) - i32::from(facing.as_q12()) + 2048) & 4095) - 2048;
        let distance =
            psx_math::int32::isqrt_i32(dx.saturating_mul(dx).saturating_add(dz.saturating_mul(dz)));
        let pitch =
            ((i32::from(psx_math::atan2_q12(target[1] - p.y - 55, distance.max(1))) + 2048) & 4095)
                - 2048;
        let shot_age = now.as_u32().wrapping_sub(self.ranged_ready.fire_started);
        let recoil = self.ranged_ready.firing(now.as_u32()) && (3..8).contains(&shot_age);
        let (low, high, alpha) = if self.ranged_ready.ready(now.as_u32()) {
            hooks::perch_aim_sample(relative, pitch, recoil)
        } else {
            hooks::perch_aim_sample(0, -341, false)
        };
        let blend =
            animation
                .looped_pose_sample_q12(low)
                .map(|sample| psx_engine::MaskedPoseBlend {
                    sample,
                    alpha_q12: alpha,
                    joint_mask: u32::MAX,
                });
        let scale = pose.pose().local_to_world();
        let origin = WorldVertex::new(p.x, p.y + scale.apply(pose.model().floor_lift), p.z);
        let presentation = psx_game_runtime::actor_pose::ActorPoseSnapshot::new(
            now,
            animation,
            high,
            blend,
            origin,
            psx_engine::Mat3I16 {
                m: [
                    [facing.cos().raw() as i16, 0, facing.sin().raw() as i16],
                    [0, 4096, 0],
                    [-(facing.sin().raw() as i16), 0, facing.cos().raw() as i16],
                ],
            },
            scale,
            psx_engine::ModelPoseTranslation { x: 0, y: 0, z: 0 },
        );
        #[cfg(all(target_arch = "mips", feature = "emulator-telemetry"))]
        if now.as_u32() % 30 == 0 {
            if let Some(hand) = presentation.joint_world_point(21, [861, 274, -396]) {
                let anchor = self.arch_anchor(index, now.as_u32());
                psx_rt::tty::print("arch:contact-error ");
                for v in [hand.x - anchor.x, hand.y - anchor.y, hand.z - anchor.z] {
                    psx_rt::tty::print_hex_u32(v as u32);
                    psx_rt::tty::print(" ");
                }
                psx_rt::tty::println("");
            }
        }
        pose.with_pose(presentation).with_presentation_clip(clip)
    }

    pub(super) fn refresh_hook_target(&mut self) {
        self.hook_selected = None;
        self.hook_visible = 0;
        if !self.ranged_ready.aiming()
            || self.hook_travel.is_some()
            || self.hook_attached.is_some()
            || self.bsp.is_none()
        {
            return;
        }
        let camera = self.render_camera;
        let [x, y, z] = self.ranged_target();
        let Some(aim) = camera.project_world(RoomPoint::new(x, y, z)) else {
            return;
        };
        let mut aabbs =
            psx_engine::FixedScratch::<CharacterCollisionAabb, MAX_STATIC_PROP_AABB_BLOCKERS>::new(
            );
        if self
            .collect_static_prop_aabb_blockers_checked_into(&mut aabbs)
            .is_none()
        {
            return;
        }
        let player = self.motor.position();
        let mut best = i32::MAX;
        for (slot, (index, hook)) in ENTITIES
            .iter()
            .enumerate()
            .filter(|(_, h)| h.kind == EntityKind::HookPoint)
            .take(hooks::MAX_HOOK_POINTS)
            .enumerate()
        {
            if hook.room != self.room_index {
                continue;
            }
            let landing = self.arch_perch(index, self.hook_overlay_absolute_tick());
            if !hooks::in_range(player, landing) {
                continue;
            }
            let marker = self.arch_anchor(index, self.hook_overlay_absolute_tick());
            let Some(screen) = camera.project_world(marker) else {
                continue;
            };
            if screen.sx < 8
                || screen.sx >= SCREEN_W - 8
                || screen.sy < 8
                || screen.sy >= SCREEN_H - 8
            {
                continue;
            }
            let bsp = self.bsp.as_mut().unwrap();
            if !bsp
                .trace_point_segment(
                    camera.position,
                    marker,
                    aabbs.as_slice(),
                    &self.destructibles,
                )
                .is_ok_and(|t| !t.hit())
            {
                continue;
            }
            self.hook_visible |= 1 << slot;
            if let Some(score) = hooks::aim_score(
                i32::from(screen.sx),
                i32::from(screen.sy),
                i32::from(aim.sx),
                i32::from(aim.sy),
            ) {
                if score < best {
                    best = score;
                    self.hook_selected = Some(index);
                }
            }
        }
        if let Some(index) = self.hook_selected {
            if !self.hook_path_clear(index) {
                self.hook_selected = None;
            }
        }
    }

    fn hook_path_clear(&mut self, index: usize) -> bool {
        let config = self.motor_config();
        let mut bodies =
            psx_engine::FixedScratch::<CharacterCollisionCylinder, MAX_COLLISION_CYLINDERS>::new();
        self.collect_collision_blockers_into(&mut bodies);
        let mut aabbs =
            psx_engine::FixedScratch::<CharacterCollisionAabb, MAX_STATIC_PROP_AABB_BLOCKERS>::new(
            );
        if self
            .collect_static_prop_aabb_blockers_checked_into(&mut aabbs)
            .is_none()
        {
            return false;
        }
        let landing = self.arch_perch(index, self.hook_overlay_absolute_tick());
        let start = self.motor.position();
        let Some(bsp) = self.bsp.as_mut() else {
            return false;
        };
        // The perch is deliberately unsupported. Sweep the full upright body,
        // then test occupancy; visibility alone never authorizes travel through a wall.
        [(start, landing), (landing, landing)]
            .into_iter()
            .all(|(from, to)| {
                bsp.trace_hook_body(
                    from,
                    to,
                    config,
                    &self.destructibles,
                    bodies.as_slice(),
                    aabbs.as_slice(),
                )
                .is_ok_and(|t| !t.hit() && !t.start_solid)
            })
    }

    pub(super) fn begin_hook(&mut self, now: SimTick) -> bool {
        if self.combat_flow.air_left == 0 {
            return false;
        }
        let Some(index) = self.hook_selected else {
            return false;
        };
        if !self.hook_path_clear(index) {
            return false;
        }
        self.fall_from_arch = false;
        self.hook_travel = Some(HookTravel {
            start: self.motor.position(),
            landing: self.arch_perch(index, now.as_u32()),
            started: now.as_u32(),
        });
        self.hook_target = Some(index);
        self.player_dash_assembly.cancel();
        self.hook_burst_started = false;
        self.hook_camera_pitch = self.camera.pitch_q12();
        self.attack_buffer.clear();
        self.attack_chain.clear();
        self.ranged_ready = combat_input::RangedReady::EMPTY;
        self.lock_target = None;
        self.soft_lock_target = None;
        self.aim_control = aim_control::AimControl::EMPTY;
        self.hook_selected = None;
        self.hook_visible = 0;
        self.anim_blend_from = Some((
            self.anim_state,
            now.saturating_sub(self.anim_start_tick),
            now,
        ));
        self.anim_state = PlayerAnim::HookLaunch;
        self.anim_start_tick = now;
        self.anim_lock_until_tick = now;
        self.motor.interrupt_action();
        if let Some(yaw) =
            psx_engine::yaw_to_point(self.motor.position(), self.arch_perch(index, now.as_u32()))
        {
            self.motor.face(yaw);
        }
        self.queue_gameplay_sfx(LevelGameplaySfxEvent::StanceSwap);
        #[cfg(target_arch = "mips")]
        {
            psx_rt::tty::print("hook:depart landing=");
            let p = self.arch_perch(index, now.as_u32());
            for v in [p.x, p.y, p.z] {
                psx_rt::tty::print_hex_u32(v as u32);
                psx_rt::tty::print(" ");
            }
            psx_rt::tty::println("");
        }
        true
    }

    fn detach_arch(&mut self, now: SimTick) {
        self.fall_from_arch = self.hook_attached.is_some();
        if let Some(index) = self.hook_attached {
            self.motor.face(Self::arch_facing(index));
        }
        // Start the release pose on the same tick as detachment: otherwise
        // one ordinary standing/aim frame flashes before gravity resumes.
        if !matches!(
            self.anim_state,
            PlayerAnim::Death | PlayerAnim::HitReact | PlayerAnim::Stun
        ) && self.character.is_some_and(|c| {
            c.action_clip(CharacterAnimationAction::Fall)
                .to_option()
                .is_some()
        }) {
            self.anim_state = PlayerAnim::Fall;
            self.anim_start_tick =
                SimTick::from_u32(now.as_u32().saturating_sub(if self.fall_from_arch {
                    0
                } else {
                    14
                }));
            self.anim_lock_until_tick = now;
            self.anim_blend_from = None;
        }
        self.hook_attached = None;
        self.hook_target = None;
        self.hook_travel = None;
        self.hook_charge.tick(false, false);
        self.player_dash_assembly.cancel();
        self.evade_run_hold_consumed = true;
        self.evade_run_hold_ticks = 0;
        self.evade_buffer_vblanks = 0;
        // Preserve an active hit/stun lock when enemy damage breaks the tether.
        telemetry::debug_log("arch:drop");
    }

    fn move_arch_body(&mut self, next: RoomPoint) -> bool {
        let config = self.motor_config();
        let mut bodies =
            psx_engine::FixedScratch::<CharacterCollisionCylinder, MAX_COLLISION_CYLINDERS>::new();
        self.collect_collision_blockers_into(&mut bodies);
        let mut aabbs =
            psx_engine::FixedScratch::<CharacterCollisionAabb, MAX_STATIC_PROP_AABB_BLOCKERS>::new(
            );
        if self
            .collect_static_prop_aabb_blockers_checked_into(&mut aabbs)
            .is_none()
        {
            return false;
        }
        if !self.bsp.as_mut().is_some_and(|bsp| {
            bsp.trace_hook_body(
                self.motor.position(),
                next,
                config,
                &self.destructibles,
                bodies.as_slice(),
                aabbs.as_slice(),
            )
            .is_ok_and(|t| !t.hit() && !t.start_solid)
        }) {
            return false;
        }
        let old = self.motor.position();
        self.motor.teleport_to(next);
        self.camera.relocate_room_space(RoomPoint::new(
            next.x - old.x,
            next.y - old.y,
            next.z - old.z,
        ));
        self.player_moved_last_tick = next != old;
        true
    }

    pub(super) fn step_hook(&mut self, ctx: &Ctx) -> bool {
        if self.hook_attached.is_some() || self.hook_travel.is_some() {
            if self.hazard_death_ticks_remaining != 0
                || matches!(self.anim_state, PlayerAnim::Stun | PlayerAnim::Death)
            {
                self.detach_arch(ctx.sim_tick);
                return false;
            }
            if ctx.just_pressed(button::CIRCLE) {
                self.detach_arch(ctx.sim_tick);
                // Consume this press so release cannot accidentally start a dodge.
                return true;
            }
        }
        if self.combat_flow.air_tick(
            self.hook_attached.is_some(),
            self.hook_travel.is_none() && self.motor.grounded(),
            self.ranged_ready.firing(ctx.sim_tick.as_u32()),
        ) {
            self.detach_arch(ctx.sim_tick);
            telemetry::debug_log("arch:timeout");
            return false;
        }
        if let Some(index) = self.hook_attached {
            let next = self.arch_perch(index, ctx.sim_tick.as_u32());
            if ENTITIES[index].room != self.room_index || !self.move_arch_body(next) {
                self.detach_arch(ctx.sim_tick);
            }
            // Continue normal aiming, stance changes, enemy AI and projectile combat.
            // Only the motor solve is suspended later in update_gameplay.
            return false;
        }
        let Some(mut flight) = self.hook_travel else {
            self.hook_charge.tick(false, self.motor.grounded());
            return false;
        };
        if let Some(index) = self.hook_target {
            flight.landing = self.arch_perch(index, ctx.sim_tick.as_u32());
            self.hook_travel = Some(flight);
        }
        let age = ctx.sim_tick.as_u32().wrapping_sub(flight.started);
        if age >= hooks::LAUNCH_TICKS && !self.hook_burst_started {
            self.player_dash_assembly
                .traverse(ctx.sim_tick, hooks::TRAVEL_TICKS.saturating_sub(age));
            self.hook_burst_started = true;
        }
        if !self.move_arch_body(flight.position(ctx.sim_tick.as_u32())) {
            self.detach_arch(ctx.sim_tick);
            telemetry::debug_log("arch:blocked");
            return false;
        }
        self.player_scarf = psx_game_runtime::model_rendering::PlayerScarf::new();
        self.anim_state = PlayerAnim::HookLaunch;
        self.ranged_ready = combat_input::RangedReady::EMPTY;
        self.render_camera = self.update_follow_camera(ctx);
        if flight.finished(ctx.sim_tick.as_u32()) {
            self.hook_travel = None;
            self.hook_attached = self.hook_target.take();
            if let Some(index) = self.hook_attached {
                self.motor.face(Self::arch_facing(index));
            }
            self.switch_player_anim(PlayerAnim::Idle, ctx.sim_tick, ctx.video_hz);
            telemetry::debug_log("arch:attached");
        }
        true
    }
}
