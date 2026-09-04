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
