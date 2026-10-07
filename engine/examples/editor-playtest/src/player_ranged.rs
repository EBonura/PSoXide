//! Player ranged weapons share the enemy projectile pool, swept world traces,
//! authored release frames and stance-aware damage. Lock-on never readies a gun.
use super::*;
use psx_game_runtime::{
    combat,
    projectiles::{self, CombatTeam, ProjectileSpawn},
};

impl Playtest {
    pub(super) fn player_has_ranged_weapon(&self) -> bool {
        self.character.as_ref().is_some_and(|c| {
            c.action_clip(CharacterAnimationAction::RangedAttack)
                .is_some()
                && c.action_clip(CharacterAnimationAction::RangedAim).is_some()
        })
    }

    pub(super) fn ranged_locomotion(anim: PlayerAnim) -> PlayerAnim {
        match anim {
            PlayerAnim::Walk => PlayerAnim::RangedWalk,
            PlayerAnim::WalkBackward => PlayerAnim::RangedBackward,
            PlayerAnim::StrafeLeft => PlayerAnim::RangedLeft,
            PlayerAnim::StrafeRight => PlayerAnim::RangedRight,
            _ => PlayerAnim::RangedAim,
        }
    }

    pub(super) fn ranged_target(&self) -> [i32; 3] {
        if let Some(target) = self.lock_target_indicator_position() {
            // Anchor the offset to the player/target axis, not the pursuing
            // camera, so following it cannot feed back into the aim point.
            let player = self.motor.position();
            let back = psx_engine::yaw_to_point(player, target)
                .unwrap_or(self.motor.yaw())
                .add(Angle::HALF);
            let depth = psx_math::int32::isqrt_i32(distance_xz_sq(player, target))
                .saturating_add(self.camera.distance())
                .clamp(1, 16_383);
            let [x, y] = self.aim_control.angles();
            let displacement = |angle: i16| {
                let a = Angle::ZERO.add_signed_q12(angle);
                depth * a.sin().raw() / a.cos().raw().max(1)
            };
            let right = displacement(x);
            return [
                target.x.saturating_add(back.cos().mul_i32(right)),
                target.y.saturating_sub(displacement(y)),
                target.z.saturating_sub(back.sin().mul_i32(right)),
            ];
        }
        // The orbit basis points from the subject toward the camera, so its
        // horizontal negative is the screen-centre shooting direction.
        let c = self.render_camera;
        let horizontal = c.cos_pitch.mul_i32(2048);
        [
            c.position.x - c.sin_yaw.mul_i32(horizontal),
            c.position.y + c.sin_pitch.mul_i32(2048),
            c.position.z - c.cos_yaw.mul_i32(horizontal),
        ]
    }

    pub(super) fn ranged_facing_yaw(&self) -> Angle {
        let t = self.ranged_target();
        psx_engine::yaw_to_point(self.motor.position(), RoomPoint::new(t[0], t[1], t[2]))
            .unwrap_or(self.motor.yaw())
    }

    pub(super) fn release_player_projectile(&mut self, ctx: &Ctx) {
        if !self.ranged_ready.firing(ctx.sim_tick.as_u32())
            || !self.ranged_ready.can_fire()
            || self.player_stance.active() != VitalityChannelId::Two
        {
            return;
        }
        let (Some(c), Some(pose)) = (self.character, self.player_actor_pose) else {
            return;
        };
        let first = c.combat_capsule_first.to_usize();
        let records = COMBAT_CAPSULES
            .get(first..first + usize::from(c.combat_capsule_count))
            .unwrap_or(&[]);
        let target = self.ranged_target();
        let Some(fire_clip) = self.models[c.model.to_usize()]
            .and_then(|model| model.clip(&self.clips, c.clip_for(PlayerAnim::RangedAttack)))
        else {
            return;
        };
        let phase = psx_game_runtime::model_rendering::animation_phase_at_tick_q12(
            fire_clip,
            ctx.sim_tick
                .as_u32()
                .saturating_sub(self.ranged_ready.fire_started),
            ctx.video_hz,
            false,
            self.player_action_speed_q8(&c, PlayerAnim::RangedAttack),
            c.action_frame_range(CharacterAnimationAction::RangedAttack),
        );
        while let Some((emitter, release)) = combat::authored_projectile_release_pending_at_frame(
            records,
            CharacterAnimationAction::RangedAttack,
            Some(pose.pose()),
            self.ranged_ready.released,
            (phase >> 12).min(u32::from(u16::MAX)) as u16,
        ) {
            if let Some(bsp) = self.bsp.as_mut() {
                let p = self.motor.position();
                let origin = RoomPoint::new(p.x, p.y + c.height * 3 / 4, p.z);
                let muzzle = RoomPoint::new(
                    release.position[0],
                    release.position[1],
                    release.position[2],
                );
                if !bsp.melee_segment_clear(origin, muzzle, &[], &self.destructibles) {
                    self.ranged_ready.released |= 1u16 << emitter;
                    telemetry::debug_log("player projectile:muzzle-blocked");
                    continue;
                }
            }
            let mut visual = release.visual;
            visual.crystal = true;
            visual.core_rgb = [224, 255, 248];
            visual.glow_rgb = [96, 240, 192];
            visual.glow_scale_q8 = 512;
            visual.length_ticks = 3;
            visual.trail_segments = 4;
            visual.impact_rgb = [144, 248, 208];
            let spawn = ProjectileSpawn {
                position: release.position,
                velocity: projectiles::velocity_toward(release.position, target, release.speed),
                radius: release.radius,
                damage: self
                    .vitality_modifiers()
                    .outgoing_damage(VitalityChannelId::Two, release.damage),
                // A ranged weapon deals ordinary health damage. It does not
                // implement a timed firearm parry or guaranteed interrupt.
                poise_damage: release.poise_damage,
                lifetime_ticks: release.lifetime_ticks,
                room: self.room_index,
                team: CombatTeam::Player,
                owner: projectiles::NO_PROJECTILE_OWNER,
                tint_rgb: release.tint_rgb,
                damage_channel: projectiles::ProjectileDamageChannel::Zenith,
                visual,
            };
            if self.combat_projectiles.spawn(spawn).is_err() {
                break;
            }
            let _ = self.combat_projectile_impacts.spawn_muzzle(&spawn);
            self.ranged_ready.released |= 1u16 << emitter;
            self.queue_gameplay_sfx(LevelGameplaySfxEvent::ProjectileLaunch);
            telemetry::debug_log("player projectile:release");
        }
    }
}
