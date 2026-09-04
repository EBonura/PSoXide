//! Bounded visual feedback driven by successful gameplay actions.
//! Samples retain world positions, so a dash wake stays behind when the actor
//! stops. No allocation, collision query, animation resampling or random state.

use crate::projectiles::ProjectileVisualStyle;
use psx_level::RoomIndex;

/// One retained piece of a dash wake.
#[derive(Clone, Copy, Debug)]
pub struct DashSample {
    /// Start of the actual collision-resolved movement.
    pub from: [i32; 3],
    /// End of the movement, or the center of a launch/settle pulse.
    pub to: [i32; 3],
    /// Room-local coordinate space.
    pub room: RoomIndex,
    /// Actor height in world units.
    pub height: u16,
    /// Age at the fixed 60 Hz presentation clock.
    pub age: u8,
    /// Captured stance color; later stance changes do not recolor old samples.
    pub zenith: bool,
    /// Ground pulse instead of a body-height wake.
    pub pulse: bool,
    live: bool,
}

impl DashSample {
    const EMPTY: Self = Self {
        from: [0; 3],
        to: [0; 3],
        room: RoomIndex::ZERO,
        height: 0,
        age: 0,
        zenith: false,
        pulse: false,
        live: false,
    };
}

/// Eight short-lived samples cost less than 300 bytes on the guest.
pub struct DashWake {
    samples: [DashSample; 8],
    active: bool,
    room: RoomIndex,
}

impl DashWake {
    /// Empty, also valid after zero-initialization.
    pub const EMPTY: Self = Self {
        samples: [DashSample::EMPTY; 8],
        active: false,
        room: RoomIndex::ZERO,
    };

    /// Expire visual samples once per gameplay tick, including idle recovery.
    pub fn tick(&mut self) {
        for sample in &mut self.samples {
            if sample.live {
                sample.age += 1;
                sample.live = sample.age < 12;
            }
        }
    }

    /// Feed actual motor movement. Blocked/refused evades create no trail.
    /// Entering a different coordinate space discards old points, avoiding a
    /// ribbon across a portal or checkpoint teleport.
    pub fn observe(
        &mut self,
        from: [i32; 3],
        to: [i32; 3],
        room: RoomIndex,
        height: u16,
        dashing: bool,
        zenith: bool,
    ) {
        if self.room != room || (0..3).any(|i| to[i].saturating_sub(from[i]).saturating_abs() > 128)
        {
            *self = Self::EMPTY;
            self.room = room;
            return;
        }
        let moved = from[0] != to[0] || from[2] != to[2];
        if dashing && moved {
            if !self.active {
                self.insert(DashSample {
                    from,
                    to: from,
                    room,
                    height,
                    age: 0,
                    zenith,
                    pulse: true,
                    live: true,
                });
            }
            self.insert(DashSample {
                from,
                to,
                room,
                height,
                age: 0,
                zenith,
                pulse: false,
                live: true,
            });
        } else if self.active {
            self.insert(DashSample {
                from: to,
                to,
                room,
                height,
                age: 0,
                zenith,
                pulse: true,
                live: true,
            });
        }
        self.active = dashing && moved;
    }

    fn insert(&mut self, sample: DashSample) {
        let index = self
            .samples
            .iter()
            .position(|s| !s.live)
            .unwrap_or_else(|| {
                self.samples
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, s)| s.age)
                    .unwrap()
                    .0
            });
        self.samples[index] = sample;
    }

    /// Retained samples, oldest/newest order is resolved by the world depth table.
    pub fn samples(&self) -> impl Iterator<Item = DashSample> + '_ {
        self.samples.iter().copied().filter(|s| s.live)
    }
}

/// Metal splinters for a hit, a sharper poise break, or a larger defeat burst.
/// Uses the existing impact pool, leaving collision/projectile capacity intact.
pub fn melee_impact_style(zenith: bool, stagger: bool, defeated: bool) -> ProjectileVisualStyle {
    let accent = if zenith {
        [64, 190, 208]
    } else {
        [224, 96, 40]
    };
    ProjectileVisualStyle {
        core_rgb: [220, 232, 224],
        glow_rgb: accent,
        impact_rgb: accent,
        impact_lifetime_ticks: if defeated {
            30
        } else if stagger {
            18
        } else {
            12
        },
        break_fragment_count: if defeated {
            10
        } else if stagger {
            6
        } else {
            3
        },
        ..ProjectileVisualStyle::EMPTY
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dash_wake_follows_resolved_movement_then_expires_without_drifting() {
        let mut wake = DashWake::EMPTY;
        wake.observe([0; 3], [0; 3], RoomIndex::ZERO, 64, true, false);
        assert_eq!(wake.samples().count(), 0, "blocked dash creates no wake");
        wake.observe([0; 3], [8, 0, 0], RoomIndex::ZERO, 64, true, false);
        assert_eq!(wake.samples().count(), 2);
        wake.tick();
        wake.observe([8, 0, 0], [8, 0, 0], RoomIndex::ZERO, 64, false, true);
        assert_eq!(wake.samples().filter(|s| s.pulse).count(), 2);
        let trail = wake.samples().find(|s| !s.pulse).unwrap();
        assert_eq!(trail.to, [8, 0, 0]);
        assert!(!trail.zenith);
        for _ in 0..12 {
            wake.tick();
        }
        assert_eq!(wake.samples().count(), 0);
    }
    #[test]
    fn pool_stays_bounded_and_teleports_clear_it() {
        let mut wake = DashWake::EMPTY;
        for i in 0..100 {
            wake.tick();
            wake.observe([i, 0, 0], [i + 1, 0, 0], RoomIndex::ZERO, 64, true, true);
            assert!(wake.samples().count() <= 8);
        }
        wake.observe([100, 0, 0], [500, 0, 0], RoomIndex::ZERO, 64, true, true);
        assert_eq!(wake.samples().count(), 0);
        assert!(core::mem::size_of::<DashWake>() <= 300);
    }
}
