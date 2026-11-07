//! One-input attack continuation evaluated against cooked animation phase.
use psx_level::CharacterActionChain;

/// A press accepted by the current strike, never an entire queued combo.
#[derive(Clone, Copy, Debug, Default)]
pub struct AttackChainState {
    pending: bool,
    source: u8,
}
impl AttackChainState {
    /// Forget continuation on interruption, stance change or a new action.
    pub fn clear(&mut self) {
        self.pending = false;
    }

    /// Accept an edge only inside the authored window. A pending edge waits
    /// until the handoff, including when a speed modifier skips its exact frame.
    /// Returns the incoming action once, then consumes the request.
    pub fn update(
        &mut self,
        action: u8,
        phase_q12: u32,
        pressed: bool,
        rule: CharacterActionChain,
    ) -> Option<u8> {
        if action != rule.action || rule.action == 255 {
            self.clear();
            return None;
        }
        if self.pending && self.source != action {
            self.clear();
        }
        if pressed
            && phase_q12 >= u32::from(rule.input_start) << 12
            && phase_q12 <= u32::from(rule.input_end) << 12
        {
            self.pending = true;
            self.source = action;
        }
        if self.pending && phase_q12 >= u32::from(rule.handoff_frame) << 12 {
            self.clear();
            Some(rule.next_action)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const RULE: CharacterActionChain = CharacterActionChain {
        action: 6,
        next_action: 34,
        input_start: 12,
        input_end: 34,
        handoff_frame: 34,
        blend_ticks: 4,
    };
    #[test]
    fn early_press_expires_and_holding_does_not_queue() {
        let mut s = AttackChainState::default();
        assert_eq!(s.update(6, (12 << 12) - 1, true, RULE), None);
        assert_eq!(s.update(6, 34 << 12, false, RULE), None);
        assert_eq!(s.update(6, (34 << 12) + 1, true, RULE), None);
    }
    #[test]
    fn accepted_edge_waits_and_is_consumed_once_even_across_skipped_frames() {
        let mut s = AttackChainState::default();
        assert_eq!(s.update(6, 12 << 12, true, RULE), None);
        assert_eq!(s.update(6, 33 << 12, false, RULE), None);
        assert_eq!(s.update(6, 35 << 12, false, RULE), Some(34));
        assert_eq!(s.update(6, 36 << 12, false, RULE), None);
    }
    #[test]
    fn last_frame_is_inclusive_and_interrupt_discards_intent() {
        let mut s = AttackChainState::default();
        assert_eq!(s.update(6, 34 << 12, true, RULE), Some(34));
        s.update(6, 22 << 12, true, RULE);
        s.clear();
        assert_eq!(s.update(6, 34 << 12, false, RULE), None);
        s.update(6, 22 << 12, true, RULE);
        assert_eq!(s.update(35, 34 << 12, false, RULE), None);
        assert_eq!(s.update(6, 34 << 12, false, RULE), None);
    }
    #[test]
    fn a_new_strike_needs_its_own_press_and_finisher_has_no_link() {
        let mut s = AttackChainState::default();
        s.update(6, 25 << 12, true, RULE);
        assert_eq!(s.update(6, 34 << 12, false, RULE), Some(34));
        let second = CharacterActionChain {
            action: 34,
            next_action: 35,
            input_start: 10,
            input_end: 22,
            handoff_frame: 22,
            ..RULE
        };
        assert_eq!(s.update(34, 22 << 12, false, second), None);
        assert_eq!(s.update(34, 22 << 12, true, second), Some(35));
        assert_eq!(
            s.update(35, 22 << 12, true, CharacterActionChain::NONE),
            None
        );
    }
    #[test]
    fn one_press_or_a_held_button_never_adds_a_second_strike() {
        let mut state = AttackChainState::default();
        for frame in 0..=60 {
            // Only the leading edge is a press; a held R1 has no more edges.
            assert_eq!(state.update(6, frame << 12, frame == 0, RULE), None);
        }
    }

    #[test]
    fn repeated_edges_queue_only_one_followup_and_terminal_strike_consumes_none() {
        let mut state = AttackChainState::default();
        for frame in [12, 15, 18, 21, 24, 27, 30, 33] {
            assert_eq!(state.update(6, frame << 12, true, RULE), None);
        }
        assert_eq!(state.update(6, 34 << 12, false, RULE), Some(34));
        for frame in 0..=20 {
            assert_eq!(
                state.update(34, frame << 12, true, CharacterActionChain::NONE),
                None
            );
        }
        assert_eq!(state.update(6, 34 << 12, false, RULE), None);
    }

    #[test]
    fn authored_window_has_the_same_real_time_at_pal_and_ntsc() {
        for hz in [50u32, 60] {
            let mut state = AttackChainState::default();
            let mut continuations = 0;
            for tick in 0..=hz * 2 {
                let phase = tick * 30 * 4096 / hz;
                let second_press = tick == hz / 2; // 500 ms: inside 400–1133 ms.
                if let Some(next) = state.update(6, phase, second_press, RULE) {
                    assert_eq!(next, 34);
                    assert!(tick * 1000 / hz >= 1133);
                    assert!(tick * 1000 / hz <= 1154);
                    continuations += 1;
                }
            }
            assert_eq!(continuations, 1);
        }
    }
}
