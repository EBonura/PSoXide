//! Short, single-action input memory. Early presses expire; holding a button
//! never creates extra attacks, and interruption can discard the intention.

#[derive(Clone, Copy)]
pub(super) struct AttackBuffer {
    until: u32,
    tag: u8,
}

impl AttackBuffer {
    pub const EMPTY: Self = Self { until: 0, tag: 0 };

    pub fn request(&mut self, tag: u8, now: u32) {
        self.tag = tag;
        self.until = now.wrapping_add(12);
    }

    pub fn clear(&mut self) {
        *self = Self::EMPTY;
    }

    pub fn take(&mut self, now: u32) -> Option<u8> {
        let tag = self.tag;
        let fresh = self.until.wrapping_sub(now) as i32 > 0;
        self.clear();
        (tag != 0 && fresh).then_some(tag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn late_press_runs_once_but_early_press_expires() {
        let mut buffer = AttackBuffer::EMPTY;
        buffer.request(1, 100);
        assert_eq!(buffer.take(111), Some(1));
        assert_eq!(buffer.take(111), None);
        buffer.request(2, 100);
        assert_eq!(buffer.take(112), None);
    }
    #[test]
    fn newest_intent_replaces_old_and_interrupt_clears_it() {
        let mut buffer = AttackBuffer::EMPTY;
        buffer.request(1, 100);
        buffer.request(4, 104);
        assert_eq!(buffer.take(110), Some(4));
        buffer.request(2, 120);
        buffer.clear();
        assert_eq!(buffer.take(120), None);
        buffer.request(3, u32::MAX - 4);
        assert_eq!(buffer.take(2), Some(3));
    }
}

/// Holding aim is a capability, independent of target selection. A dodge or
/// interruption lowers the weapon and a held L2 must ready it again.
#[derive(Clone, Copy, Default)]
pub(super) struct RangedReady {
    ticks: u8,
    pub released: u16,
    pub fire_started: u32,
    pub fire_until: u32,
}
impl RangedReady {
    pub const EMPTY: Self = Self {
        ticks: 0,
        released: 0,
        fire_started: 0,
        fire_until: 0,
    };
    pub fn tick(&mut self, enabled: bool, held: bool, interrupted: bool) {
        if !enabled || interrupted {
            self.ticks = 0;
            self.fire_until = 0;
        } else if held {
            self.ticks = self.ticks.saturating_add(1).min(8);
        } else {
            self.ticks = 0;
        }
    }
    pub fn firing(&self, now: u32) -> bool {
        self.fire_until > now
    }
    pub fn begin_shot(&mut self, now: u32, duration: u32) {
        self.fire_started = now;
        self.fire_until = now.saturating_add(duration);
        self.released = 0;
    }
    pub fn aiming(&self) -> bool {
        self.ticks != 0
    }
    pub fn ready(&self, now: u32) -> bool { self.aiming() || self.firing(now) }
    pub fn can_fire(&self) -> bool {
        self.ticks >= 8
    }
}

#[cfg(test)]
mod ranged_tests {
    use super::*;
    #[test]
    fn releasing_precision_aim_preserves_shot_but_interruption_cancels_it() {
        let mut ready = RangedReady::EMPTY;
        for _ in 0..8 {
            ready.tick(true, true, false);
        }
        ready.begin_shot(100, 26);
        assert!(ready.firing(110));
        assert!(!ready.firing(126));
        ready.tick(true, false, false);
        assert!(ready.firing(111));
        for _ in 0..8 {
            ready.tick(true, true, false);
        }
        assert!(ready.can_fire());
        assert!(ready.firing(120));
        ready.begin_shot(120, 26);
        ready.tick(true, true, true);
        assert!(!ready.firing(121));
    }
    #[test]
    fn lock_on_cannot_replace_held_aim_and_dodge_requires_readying_again() {
        let mut ready = RangedReady::EMPTY;
        for _ in 0..30 {
            ready.tick(true, false, false);
        }
        assert!(!ready.aiming());
        assert!(!ready.can_fire());
        for _ in 0..7 {
            ready.tick(true, true, false);
        }
        assert!(ready.aiming());
        assert!(!ready.can_fire());
        ready.tick(true, true, false);
        assert!(ready.can_fire());
        ready.tick(true, true, true);
        assert!(!ready.aiming());
        ready.tick(true, true, false);
        assert!(!ready.can_fire());
        ready.tick(false, true, false);
        assert!(!ready.aiming());
    }
}
