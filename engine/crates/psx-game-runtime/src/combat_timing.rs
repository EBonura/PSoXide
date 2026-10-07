//! Shared, allocation-free animation permissions and one-request buffering.
use psx_level::{CharacterCombatWindow, CombatWindowKind};

/// Half-open cooked-frame interval.
#[derive(Clone, Copy, Debug)]
pub struct Window {
    /// First included frame.
    pub start: u16,
    /// First excluded frame.
    pub end: u16,
}
impl Window {
    /// Whether the currently sampled pose is inside the interval.
    pub fn active(self, phase: u32) -> bool {
        phase >= u32::from(self.start) << 12 && phase < u32::from(self.end) << 12
    }
    /// A pending request may cross a short window between two samples.
    fn crossed(self, previous: u32, phase: u32) -> bool {
        self.start < self.end
            && phase >= u32::from(self.start) << 12
            && previous < u32::from(self.end) << 12
            && phase >= previous
    }
}
/// The authored channels for one action at the current sampled pose.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    /// Current cooked clip phase, including the selected frame-range offset.
    pub phase: u32,
    channels: [Option<Window>; 7],
}
impl Sample {
    /// No authored overrides.
    pub const LEGACY: Self = Self {
        phase: 0,
        channels: [None; 7],
    };
    /// Resolve all channels in one bounded traversal.
    pub fn new(windows: &[CharacterCombatWindow], action: u8, phase: u32) -> Self {
        let mut result = Self {
            phase,
            ..Self::LEGACY
        };
        for w in windows {
            if w.action == action {
                result.channels[w.kind as usize] = Some(Window {
                    start: w.start,
                    end: w.end,
                });
            }
        }
        result
    }
    /// None means inherit legacy behavior; Some(false) explicitly closes it.
    pub fn active(self, kind: CombatWindowKind) -> Option<bool> {
        self.window(kind).map(|w| w.active(self.phase))
    }
    /// The authored interval, if any.
    pub fn window(self, kind: CombatWindowKind) -> Option<Window> {
        self.channels[kind as usize]
    }
}
/// One action-scoped request, retained after button release. All-zero is empty.
#[derive(Clone, Copy, Debug, Default)]
pub struct Request {
    source_start: u32,
    phase: u32,
    action: u8,
    tag: u8,
}
impl Request {
    /// Discard queued intent on hit, death, stance change, or an accepted action.
    pub fn clear(&mut self) {
        self.tag = 0;
    }
    /// Accept presses in the arming window, then consume once at permission.
    /// Scope includes the start tick so repeated instances of one action cannot leak input.
    pub fn update(
        &mut self,
        action: u8,
        source_start: u32,
        pressed: u8,
        sample: Sample,
        armed: CombatWindowKind,
        allowed: CombatWindowKind,
    ) -> Option<u8> {
        if self.action != action || self.source_start != source_start {
            *self = Self {
                action,
                source_start,
                phase: sample.phase,
                tag: 0,
            };
        }
        let was_pending = self.tag != 0;
        if pressed != 0 && sample.active(armed).or_else(|| sample.active(allowed)) == Some(true) {
            self.tag = pressed;
        }
        let open = sample.window(allowed).is_some_and(|w| {
            w.active(sample.phase) || (was_pending && w.crossed(self.phase, sample.phase))
        });
        self.phase = sample.phase;
        if self.tag != 0 && open {
            let tag = self.tag;
            self.tag = 0;
            Some(tag)
        } else {
            if sample
                .window(allowed)
                .is_none_or(|w| sample.phase >= u32::from(w.end) << 12)
            {
                self.tag = 0;
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CombatWindowKind::*;
    fn sample(phase: u32) -> Sample {
        Sample::new(
            &[
                CharacterCombatWindow {
                    action: 4,
                    kind: AttackBuffer,
                    start: 2,
                    end: 20,
                },
                CharacterCombatWindow {
                    action: 4,
                    kind: Attack,
                    start: 16,
                    end: 20,
                },
                CharacterCombatWindow {
                    action: 4,
                    kind: Dodge,
                    start: 18,
                    end: 21,
                },
                CharacterCombatWindow {
                    action: 4,
                    kind: Invulnerable,
                    start: 0,
                    end: 7,
                },
            ],
            4,
            phase,
        )
    }
    #[test]
    fn released_press_survives_longer_than_legacy_timeout_and_fires_once() {
        let mut r = Request::default();
        assert_eq!(
            r.update(4, 100, 2, sample(2 << 12), AttackBuffer, Attack),
            None
        );
        for f in 3..16 {
            assert_eq!(
                r.update(4, 100, 0, sample(f << 12), AttackBuffer, Attack),
                None
            );
        }
        assert_eq!(
            r.update(4, 100, 0, sample(16 << 12), AttackBuffer, Attack),
            Some(2)
        );
        assert_eq!(
            r.update(4, 100, 0, sample(17 << 12), AttackBuffer, Attack),
            None
        );
    }
    #[test]
    fn channels_have_independent_boundaries_and_empty_overrides_close() {
        assert_eq!(sample(16 << 12).active(Attack), Some(true));
        assert_eq!(sample(16 << 12).active(Dodge), Some(false));
        assert_eq!(sample(7 << 12).active(Invulnerable), Some(false));
        assert_eq!(sample(20 << 12).active(Attack), Some(false));
        assert_eq!(sample(1).active(Movement), None);
        assert_eq!(
            Sample::new(
                &[CharacterCombatWindow {
                    action: 4,
                    kind: Invulnerable,
                    start: 0,
                    end: 0
                }],
                4,
                0
            )
            .active(Invulnerable),
            Some(false)
        );
    }
    #[test]
    fn early_late_interrupted_and_new_instance_requests_do_not_leak() {
        let mut r = Request::default();
        r.update(4, 100, 1, sample(0), AttackBuffer, Attack);
        assert_eq!(
            r.update(4, 100, 0, sample(16 << 12), AttackBuffer, Attack),
            None
        );
        r.update(4, 200, 1, sample(3 << 12), AttackBuffer, Attack);
        r.clear();
        assert_eq!(
            r.update(4, 200, 0, sample(16 << 12), AttackBuffer, Attack),
            None
        );
        r.update(4, 300, 1, sample(3 << 12), AttackBuffer, Attack);
        assert_eq!(
            r.update(4, 301, 0, sample(16 << 12), AttackBuffer, Attack),
            None
        );
        assert_eq!(
            r.update(4, 301, 1, sample(21 << 12), AttackBuffer, Attack),
            None
        );
    }
    #[test]
    fn skipped_window_consumes_pending_but_does_not_accept_a_late_press() {
        let mut r = Request::default();
        r.update(4, 100, 1, sample(3 << 12), AttackBuffer, Attack);
        assert_eq!(
            r.update(4, 100, 0, sample(21 << 12), AttackBuffer, Attack),
            Some(1)
        );
        assert_eq!(
            r.update(4, 100, 1, sample(22 << 12), AttackBuffer, Attack),
            None
        );
    }
    #[test]
    fn permission_is_animation_time_at_both_video_rates_and_speeds() {
        for hz in [50u32, 60] {
            for speed in [128u32, 256, 512] {
                let mut r = Request::default();
                let mut fired = 0;
                for tick in 0..hz * 3 {
                    let phase = (u64::from(tick) * 20 * 4096 * u64::from(speed)
                        / (u64::from(hz) * 256)) as u32;
                    let press = if phase >= 3 << 12 && phase < 5 << 12 {
                        1
                    } else {
                        0
                    };
                    if r.update(4, 100, press, sample(phase), AttackBuffer, Attack)
                        .is_some()
                    {
                        assert!(phase >= 16 << 12);
                        fired += 1;
                    }
                }
                assert_eq!(fired, 1);
            }
        }
    }
}
