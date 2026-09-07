//! New-game shots, measured in the engine's fixed 60 Hz simulation ticks.

const FADE_TICKS: u16 = 75;
const BLACK_HOLD: u16 = 12;
const CLOSE_CUT: u16 = 225 + FADE_TICKS;
pub(super) const ANIMATION_CUT: u16 = CLOSE_CUT * 2;
const ANIMATION_START: u16 = ANIMATION_CUT + BLACK_HOLD + FADE_TICKS;
// The selected 13%–84% section of the previous full wake-up camera orbit.
const WAKE_ORBIT_START_Q12: i32 = 837;
const WAKE_ORBIT_END_Q12: i32 = -755;
const HANDOFF_TICKS: u16 = FADE_TICKS * 2 + BLACK_HOLD;
const SKIP_HOLD: u8 = 30;
const WAIT_FOR_RELEASE: u8 = u8::MAX;

#[derive(Default)]
pub(super) struct OpeningSequence {
    tick: u16,
    hold: u8,
    skip_tick: u16,
    fade_out: u16,
    punch_played: bool,
    sound_shot: u8,
}

impl OpeningSequence {
    pub fn start(animation_ticks: u32) -> Self {
        Self {
            tick: 1,
            hold: WAIT_FOR_RELEASE,
            skip_tick: 0,
            punch_played: false,
            sound_shot: 0,
            fade_out: ANIMATION_START
                + animation_ticks.clamp(1, u32::from(u16::MAX - ANIMATION_START - HANDOFF_TICKS))
                    as u16,
        }
    }

    /// One cue per shot, including the first shot's reveal from black.
    pub fn take_shot_sound(&mut self) -> bool {
        if !self.active() || self.skip_tick != 0 || self.gameplay_camera() {
            return false;
        }
        let shot = if self.tick < CLOSE_CUT {
            1
        } else if self.tick < ANIMATION_CUT {
            2
        } else {
            3
        };
        if shot == self.sound_shot {
            return false;
        }
        self.sound_shot = shot;
        true
    }

    /// Take 3's later ground slam: source frame 452 at 30 Hz, cooked frame 181 at 12 Hz.
    pub fn take_punch(&mut self, phase_q12: u32) -> bool {
        if !self.active()
            || self.holding_pose()
            || self.gameplay_camera()
            || self.skip_tick != 0
            || self.punch_played
            || phase_q12 < 181 * 4096
        {
            return false;
        }
        self.punch_played = true;
        true
    }

    pub fn active(&self) -> bool {
        self.tick != 0
    }
    pub fn holding_pose(&self) -> bool {
        self.shot_tick() <= ANIMATION_START
    }
    pub fn gameplay_camera(&self) -> bool {
        self.tick >= self.fade_out + FADE_TICKS
    }
    #[cfg(test)]
    pub fn animation_starts(&self) -> bool {
        self.tick == ANIMATION_START
    }
    pub fn handoff(&self) -> bool {
        self.tick == self.fade_out + FADE_TICKS
    }

    /// A held menu confirmation cannot skip the opening. Release it first.
    pub fn advance(&mut self, cross: bool) {
        if !self.active() {
            return;
        }
        if !cross {
            self.hold = 0;
        } else if self.hold != WAIT_FOR_RELEASE && self.tick < self.fade_out {
            self.hold += 1;
            if self.hold == SKIP_HOLD {
                self.skip_tick = self.tick;
                self.tick = self.fade_out - 1;
            }
        }
        self.tick += 1;
        if self.tick >= self.fade_out + HANDOFF_TICKS {
            self.tick = 0;
        }
    }

    pub fn skip_progress(&self) -> u8 {
        if self.hold == WAIT_FOR_RELEASE {
            0
        } else {
            self.hold.min(SKIP_HOLD)
        }
    }

    fn shot_tick(&self) -> u16 {
        if self.skip_tick != 0 {
            self.skip_tick
        } else {
            self.tick
        }
    }

    pub fn fade(&self) -> u8 {
        let tick = self.tick;
        if tick == 0 {
            return 0;
        }
        let mut amount = FADE_TICKS.saturating_sub(tick);
        for cut in [CLOSE_CUT, ANIMATION_CUT, self.fade_out + FADE_TICKS] {
            let out = cut - FADE_TICKS;
            let reveal = cut + BLACK_HOLD;
            if (out..reveal + FADE_TICKS).contains(&tick) {
                amount = if tick < cut {
                    tick - out
                } else if tick < reveal {
                    FADE_TICKS
                } else {
                    reveal + FADE_TICKS - tick
                };
                break;
            }
        }
        // Smoothstep in Q8: ease gently into and out of black without 64-bit math.
        let x = u32::from(amount) * 256 / u32::from(FADE_TICKS);
        let eased = x * x * (768 - 2 * x) / 65536;
        (eased * 255 / 256) as u8
    }

    /// Distance, eye height, focus height, and orbit offset from player yaw.
    pub fn shot(&self) -> (i32, i32, i32, u16) {
        let tick = self.shot_tick();
        match tick {
            0..CLOSE_CUT => (7500, 3500, 220, 2768 - tick),
            CLOSE_CUT..ANIMATION_CUT => (3000, 1900, 220, 2768 - CLOSE_CUT - tick),
            _ => {
                // Use the narrower front arc over the entire shot, including fades.
                let duration = (self.fade_out + FADE_TICKS - ANIMATION_CUT).max(1);
                let elapsed = tick.saturating_sub(ANIMATION_CUT).min(duration);
                let sweep = i32::from(elapsed) * (WAKE_ORBIT_START_Q12 - WAKE_ORBIT_END_Q12)
                    / i32::from(duration);
                (
                    2800,
                    1100,
                    600,
                    ((WAKE_ORBIT_START_Q12 - sweep) & 4095) as u16,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FADE_OUT: u16 = ANIMATION_START + 240;
    const HANDOFF: u16 = FADE_OUT + FADE_TICKS;
    const END: u16 = FADE_OUT + HANDOFF_TICKS;

    #[test]
    fn menu_confirmation_needs_release_and_half_second_hold() {
        let mut intro = OpeningSequence::start(240);
        for _ in 0..60 {
            intro.advance(true);
        }
        assert_eq!(intro.skip_progress(), 0);
        intro.advance(false);
        for _ in 0..29 {
            intro.advance(true);
        }
        assert!(intro.tick < FADE_OUT);
        intro.advance(false);
        assert_eq!(intro.skip_progress(), 0);
        for _ in 0..30 {
            intro.advance(true);
        }
        assert_eq!(intro.tick, FADE_OUT);
        for _ in 0..FADE_TICKS {
            intro.advance(true);
        }
        assert!(intro.handoff());
        assert_eq!(intro.fade(), 255);
    }

    #[test]
    fn cuts_are_hidden_and_animation_waits_for_reveal() {
        let mut intro = OpeningSequence::start(240);
        for tick in 1..END {
            intro.tick = tick;
            if [CLOSE_CUT, ANIMATION_CUT, HANDOFF].contains(&tick) {
                assert_eq!(intro.fade(), 255);
            }
            if tick == ANIMATION_START {
                assert_eq!(intro.fade(), 0);
                assert!(intro.holding_pose());
            }
        }
        for tick in [100, ANIMATION_START + 100] {
            intro.tick = tick;
            intro.skip_tick = 0;
            intro.hold = SKIP_HOLD - 1;
            let shot = intro.shot();
            let holding_pose = intro.holding_pose();
            intro.advance(true);
            assert_eq!(intro.shot(), shot, "skip must not cut before fading out");
            assert_eq!(intro.holding_pose(), holding_pose);
        }
    }

    #[test]
    fn handoff_waits_for_the_selected_clip_and_bounds_long_takes() {
        for duration in [1, 240, 1815, u32::MAX] {
            let mut intro = OpeningSequence::start(duration);
            let play_ticks = duration.min(u32::from(u16::MAX - ANIMATION_START - HANDOFF_TICKS));
            assert_eq!(u32::from(intro.fade_out - ANIMATION_START), play_ticks);
            intro.tick = intro.fade_out;
            assert_eq!(intro.fade(), 0);
            assert!(!intro.gameplay_camera());
            for _ in 0..FADE_TICKS {
                intro.advance(false);
            }
            assert!(intro.handoff());
            assert_eq!(intro.fade(), 255);
            for _ in 0..BLACK_HOLD + FADE_TICKS {
                intro.advance(false);
            }
            assert!(!intro.active());
        }
    }

    #[test]
    fn punch_fires_once_on_frame_crossing_and_never_during_skip() {
        let mut intro = OpeningSequence::start(1815);
        assert!(!intro.take_punch(182 * 4096));
        intro.tick = ANIMATION_START + 220;
        assert!(!intro.take_punch(45 * 4096));
        intro.tick = ANIMATION_START + 905;
        assert!(!intro.take_punch(180 * 4096));
        assert!(intro.take_punch(182 * 4096));
        assert!(!intro.take_punch(183 * 4096));
        let mut skipped = OpeningSequence::start(1815);
        skipped.tick = ANIMATION_START + 100;
        skipped.hold = SKIP_HOLD - 1;
        skipped.advance(true);
        assert!(!skipped.take_punch(182 * 4096));
    }

    #[test]
    fn orbit_keeps_moving_while_each_shot_fades() {
        let mut intro = OpeningSequence::start(1815);
        for tick in [
            15,
            CLOSE_CUT - 60,
            CLOSE_CUT + BLACK_HOLD + 15,
            ANIMATION_CUT - 60,
            ANIMATION_CUT + BLACK_HOLD + 15,
            intro.fade_out + 15,
        ] {
            intro.tick = tick;
            assert!(intro.fade() > 0 && intro.fade() < 255);
            let before = intro.shot().3;
            intro.tick += 20;
            let after = intro.shot().3;
            let travelled = before.wrapping_sub(after) & 4095;
            assert!(travelled > 0 && travelled < 64, "orbit stopped or reversed");
        }
    }

    #[test]
    fn second_shot_starts_at_its_previous_endpoint_without_changing_speed() {
        let mut intro = OpeningSequence::start(1695);
        intro.tick = CLOSE_CUT;
        assert_eq!(intro.shot(), (3000, 1900, 220, 2768 - ANIMATION_CUT));
        let start_yaw = intro.shot().3;
        intro.tick += 60;
        assert_eq!(start_yaw - intro.shot().3, 60);
    }

    #[test]
    fn wake_orbit_uses_selected_arc_without_shortening_the_animation() {
        let mut intro = OpeningSequence::start(1695);
        intro.tick = ANIMATION_CUT;
        assert_eq!(intro.shot(), (2800, 1100, 600, 837));
        assert_eq!(intro.fade_out - ANIMATION_START, 1695);
        intro.tick = intro.fade_out + FADE_TICKS;
        assert_eq!(intro.shot(), (2800, 1100, 600, 3341));
        assert!(intro.handoff());
    }

    #[test]
    fn sound_plays_once_at_each_shot_start_and_not_on_skip_or_handoff() {
        let mut intro = OpeningSequence::start(1695);
        let mut cues = Vec::new();
        while intro.active() {
            if intro.take_shot_sound() {
                cues.push(intro.tick);
            }
            assert!(!intro.take_shot_sound());
            intro.advance(false);
        }
        assert_eq!(cues, [1, CLOSE_CUT, ANIMATION_CUT]);
        assert!(!intro.take_shot_sound());
        let mut skipped = OpeningSequence::start(1695);
        skipped.hold = SKIP_HOLD - 1;
        skipped.advance(true);
        assert!(!skipped.take_shot_sound());
    }

    #[test]
    fn full_sequence_has_one_animation_and_a_black_camera_handoff() {
        let mut intro = OpeningSequence::start(240);
        let mut animation_starts = 0;
        let mut handoffs = 0;
        for _ in 1..END {
            intro.advance(false);
            animation_starts += u16::from(intro.animation_starts());
            if intro.handoff() {
                handoffs += 1;
                assert_eq!(intro.fade(), 255);
            }
        }
        assert_eq!((animation_starts, handoffs), (1, 1));
        assert!(!intro.active());
        assert_eq!(intro.fade(), 0);
    }
}

use super::*;

impl Playtest {
    /// Cinematics freeze the motor, so settle the authored spawn before doing so.
    pub(super) fn ground_opening_player(&mut self) {
        let from = self.motor.position();
        let depth = self.character.map_or(64, |character| character.height) * 4;
        let to = RoomPoint::new(from.x, from.y - depth, from.z);
        if let Some(bsp) = self.bsp.as_mut() {
            if let Ok(trace) = bsp.trace_point_segment(from, to, &[], &self.destructibles) {
                if trace.hit() && !trace.start_solid && trace.normal_q12[1] >= 2048 {
                    self.motor.snap_to(trace.end, self.motor.yaw());
                }
            }
        }
    }

    pub(super) fn update_opening(&mut self, ctx: &mut Ctx) {
        self.opening.advance(ctx.is_held(button::CROSS));
        if self.opening.take_shot_sound() {
            self.queue_gameplay_sfx(LevelGameplaySfxEvent::IntroShot);
        }
        if !self.opening.active() {
            self.gameplay_epoch = ctx.sim_tick;
            self.open_world_message_once();
            self.queue_gameplay_sfx(LevelGameplaySfxEvent::GameplayEnter);
            return;
        }
        // Keep the pose at frame zero through both orbit shots, without a
        // blend from the standing idle pose that was used during loading.
        if self.opening.holding_pose() {
            self.anim_start_tick = ctx.sim_tick;
        }
        if self.opening.handoff() {
            self.player_dash_assembly.cancel();
            self.anim_state = PlayerAnim::Idle;
            self.anim_start_tick = ctx.sim_tick;
            self.anim_lock_until_tick = SimTick::ZERO;
            self.anim_blend_from = None;
            self.camera
                .snap_to_player(self.camera_target(None, false), self.camera_config());
        }
        if self.opening.gameplay_camera() {
            self.render_camera = self.update_follow_camera(ctx);
        } else {
            let (distance, height, focus, yaw) = self.opening.shot();
            let body_height = self.character.map_or(64, |character| character.height);
            let mut config = ThirdPersonCameraConfig::character(
                distance * body_height / 1024,
                height * body_height / 1024,
                focus * body_height / 1024,
            );
            config.position_lag_shift = 0;
            config.focus_lag_shift = 0;
            config.distance_lag_shift = 0;
            let target = self.camera_target(None, false);
            self.camera.snap_to_player_with_yaw(
                target,
                config,
                target.player_yaw.add(Angle::from_q12(yaw)),
            );
            // Authored cinematic shots can sit beyond the gameplay camera's
            // collision boundary. The normal solver resumes at the black handoff.
            self.render_camera = world_camera_from_position_focus(
                PROJECTION,
                self.camera.position(),
                self.camera.focus(),
            );
        }
    }
}
