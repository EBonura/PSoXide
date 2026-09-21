//! Where the rotating title ring and the ball of balls above it end up on
//! screen.
//!
//! Straight homage to the PlayStation demo discs: a carousel of glossy blue
//! pills you spin to pick a game, under a slowly turning sphere made of
//! smaller spheres, over a starfield. This crate works out the positions; the
//! launcher draws them, so the geometry stays testable off the console.
//!
//! All integer. Angles are Q12 turns (0..4096), `psx_math::sin_q12` returns
//! Q12, and the projection is a shift-and-divide rather than anything with a
//! decimal point in it.

#![no_std]

use psx_math::{cos_q12, sin_q12};

/// A full turn.
pub const TURN: i32 = 4096;

/// Camera distance from the ring's centre, world units. Well clear of the
/// ring radius: closer would swing the front pill up to several times the
/// size of the back one and off the screen.
const CAMERA_Z: i32 = 520;
/// Focal length. Bigger spreads the ring wider across the screen.
const FOCAL: i32 = 300;
/// Ring radius.
const RING_R: i32 = 205;
/// How far below the ring its reflection is drawn.
///
/// A translation, not a mirror. The ring lies in a horizontal plane and so
/// does its reflection, and this projection maps a plane's depth to a linear
/// vertical offset; reflecting one horizontal plane in another therefore just
/// slides the whole thing down the screen. Mirroring each pill about a line
/// instead flipped the ring's near and far sides, putting the closest pills'
/// reflections at the top of the reflected ring.
///
/// Big enough that the reflected ring's far side clears the real ring's near
/// side, which is what sets the floor low enough for the carousel to look
/// like it is floating over it.
pub const REFLECT_DROP: i16 = 39;

/// How much the reflected ring's own near-to-far spread is flattened, in
/// 256ths. A reflection seen at this grazing an angle is compressed, and
/// without it the reflected ring is as tall as the real one and cannot fit
/// between the carousel and the last scanline.
const REFLECT_SQUASH: i32 = 100;

/// Where a pill's reflection sits, given where the pill is.
///
/// The whole ring slides down and flattens; it does not turn over. Each
/// pill's own shading still flips, which is the part that really is a mirror.
pub fn reflect_y(y: i16) -> i16 {
    RING_Y + REFLECT_DROP + (((y - RING_Y) as i32 * REFLECT_SQUASH) / 256) as i16
}
/// Screen row the ring's centre projects to.
const RING_Y: i16 = 186;
/// How far the ring's far side rides up the screen: the tilt that turns a
/// circle into an ellipse.
const RING_TILT: i32 = 20;

/// Pill size at the front of the ring, before perspective. Wide and deep
/// enough to carry a two-line title without the words hanging off the ends.
const PILL_RX: i32 = 68;
const PILL_RY: i32 = 19;

/// The ball of balls hangs centred above the ring.
const SPHERE_CENTRE_X: i16 = 160;
const SPHERE_CENTRE_Y: i16 = 104;
const SPHERE_R: i32 = 68;
/// Rings of latitude, and points around each. Poles are added separately.
/// Dense enough that the beads crowd each other, which is what stops the
/// cluster reading as scattered confetti.
const SPHERE_LAT: usize = 4;
const SPHERE_LON: usize = 6;
pub const SPHERE_POINTS: usize = SPHERE_LAT * SPHERE_LON + 2;

/// Where one carousel item landed after projection.
#[derive(Copy, Clone)]
pub struct Placed {
    pub x: i16,
    pub y: i16,
    pub rx: i16,
    pub ry: i16,
    /// Depth, larger is further away. Used to sort and to dim.
    pub z: i32,
    /// 0..=255, how much of the way to the front this item is.
    pub front: u8,
}

/// Perspective divide. Returns a Q8 scale factor for something at depth `z`.
fn perspective(z: i32) -> i32 {
    let z = if z < 40 { 40 } else { z };
    (FOCAL << 8) / z
}

/// Project one carousel slot. `angle` is where that slot sits on the ring,
/// measured so that zero is the front: the selected entry sits nearest the
/// camera, centred, which is the whole point of a carousel.
pub fn place(angle: i32) -> Placed {
    let a = ((angle + TURN / 2) & (TURN - 1)) as u16;
    let world_x = (RING_R * sin_q12(a)) >> 12;
    let world_z = (RING_R * cos_q12(a)) >> 12;
    let z = CAMERA_Z + world_z;
    let k = perspective(z);

    // cos is +1 at the back of the ring and -1 at the front.
    let front = (((-cos_q12(a) + 4096) * 255) / 8192) as u8;

    Placed {
        x: (160 + ((world_x * k) >> 8)) as i16,
        y: (RING_Y as i32 - ((world_z * RING_TILT) >> 8)) as i16,
        rx: ((PILL_RX * k) >> 8) as i16,
        ry: ((PILL_RY * k) >> 8) as i16,
        z,
        front,
    }
}

/// A projected sphere bead, before sorting.
#[derive(Copy, Clone, Default)]
pub struct Bead {
    pub x: i16,
    pub y: i16,
    pub r: i16,
    pub z: i32,
    pub lit: u8,
}

/// Project the ball of balls, spun by `angle` and blown outward by `swell`
/// (in 256ths of the resting radius, so 0 is at rest and 128 is half again as
/// wide). Fills `out` and returns how many beads it wrote.
pub fn sphere(yaw: i32, pitch: i32, swell: i32, out: &mut [Bead; SPHERE_POINTS]) -> usize {
    let a = (yaw & (TURN - 1)) as u16;
    let p = (pitch & (TURN - 1)) as u16;
    let (sin_a, cos_a) = (sin_q12(a), cos_q12(a));
    let (sin_p, cos_p) = (sin_q12(p), cos_q12(p));
    let radius = SPHERE_R + (SPHERE_R * swell) / 256;
    let mut n = 0;

    let mut emit = |x: i32, y: i32, z: i32| {
        // Yaw about the vertical axis, then pitch about the horizontal one.
        // Two axes rather than one so a shove can tumble it rather than only
        // spin it on the spot.
        let rx = ((x * cos_a) + (z * sin_a)) >> 12;
        let zy = ((z * cos_a) - (x * sin_a)) >> 12;
        let ry = ((y * cos_p) - (zy * sin_p)) >> 12;
        let rz = ((y * sin_p) + (zy * cos_p)) >> 12;
        let depth = CAMERA_Z + rz;
        let k = perspective(depth);
        // Front beads are bigger and brighter, back ones sink into the dark.
        // Beads round the back sink most of the way into the dark, which is
        // what makes a cloud of ellipses read as one sphere.
        let lit = (24 + ((SPHERE_R - rz).clamp(0, 2 * SPHERE_R) * 231) / (2 * SPHERE_R)) as u8;
        out[n] = Bead {
            x: (SPHERE_CENTRE_X as i32 + ((rx * k) >> 8)) as i16,
            y: (SPHERE_CENTRE_Y as i32 + ((ry * k) >> 8)) as i16,
            r: (((SPHERE_R / 7) * k) >> 8) as i16,
            z: depth,
            lit,
        };
        n += 1;
    };

    emit(0, -radius, 0);
    for lat in 0..SPHERE_LAT {
        // Latitudes spread between the poles, exclusive of both.
        let phi = (((lat as i32 + 1) * TURN) / (2 * (SPHERE_LAT as i32 + 1))) as u16;
        let y = -((radius * cos_q12(phi)) >> 12);
        let ring = (radius * sin_q12(phi)) >> 12;
        for lon in 0..SPHERE_LON {
            // Offset alternate rings so the beads sit in each other's gaps.
            let theta =
                (((lon as i32 * 2 + (lat & 1) as i32) * TURN) / (SPHERE_LON as i32 * 2)) as u16;
            emit(
                (ring * sin_q12(theta)) >> 12,
                y,
                (ring * cos_q12(theta)) >> 12,
            );
        }
    }
    emit(0, radius, 0);
    n
}

/// Sort `items` back to front by depth. Insertion sort: these arrays are
/// tens of entries, and it is already nearly sorted between frames.
pub fn sort_by_depth<T: Copy, F: Fn(&T) -> i32>(items: &mut [T], depth: F) {
    for i in 1..items.len() {
        let mut j = i;
        while j > 0 && depth(&items[j - 1]) < depth(&items[j]) {
            items.swap(j - 1, j);
            j -= 1;
        }
    }
}

/// A shove in some direction nobody can predict, as a unit vector in Q12.
///
/// No RNG on the guest, and none wanted: a hash of the browse count gives a
/// different direction every time while staying identical between runs, which
/// is what makes a bug in this reproducible.
pub fn impulse(seed: u32) -> (i32, i32) {
    let mut h = seed.wrapping_mul(2_654_435_761);
    h ^= h >> 15;
    h = h.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    let a = (h % TURN as u32) as u16;
    (cos_q12(a), sin_q12(a))
}

/// Bleed a browse-kick off the ball's spin rate, one frame's worth.
///
/// The shift alone stalls a few units short of `idle` once the gap is small
/// enough that it rounds to zero, so the last of it closes by hand. Without
/// that the ball would keep creeping after the player stopped browsing.
pub fn ease_spin(rate: i32, idle: i32, decay_shift: i32) -> i32 {
    let excess = idle - rate;
    rate + if excess.abs() < (1 << decay_shift) {
        excess.signum()
    } else {
        excess >> decay_shift
    }
}

/// Nearest a star gets before it wraps back out to [`STAR_FAR`].
const STAR_NEAR: i32 = 40;
/// Furthest a star starts from the camera.
pub const STAR_FAR: i32 = 1400;
/// Half-width of the volume stars are scattered through, in world units.
///
/// Narrow, and deliberately so. At 900 the field was geometrically correct
/// and visually empty: measured, as few as 8 of 120 stars projected inside
/// the screen at once, the rest culled off the edges before they were ever
/// near enough to see. At 320 it is about 70.
const STAR_SPREAD: i32 = 320;

/// One star, already projected.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Star {
    pub x: i16,
    pub y: i16,
    /// Side of the square drawn for it: distant stars are a single pixel.
    pub size: u16,
    pub bright: u8,
    /// False when it projected off the screen and should be skipped.
    pub visible: bool,
}

/// A star flying past the camera. No RNG and no per-star storage on the
/// guest: a hash of the index fixes where it sits in the volume, and
/// `travel` walks the whole field toward the viewer, wrapping each star back
/// out to the far plane on its own schedule.
pub fn star(index: u32, travel: i32) -> Star {
    let mut h = index.wrapping_mul(2_654_435_761);
    h ^= h >> 15;
    h = h.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;

    let world_x = (h % (2 * STAR_SPREAD) as u32) as i32 - STAR_SPREAD;
    let world_y = ((h >> 11) % (2 * STAR_SPREAD) as u32) as i32 - STAR_SPREAD;
    // Stagger the wrap so they do not all reappear together.
    let span = STAR_FAR - STAR_NEAR;
    let offset = ((h >> 22) as i32) % span;
    let z = STAR_FAR - (travel + offset).rem_euclid(span);

    let k = (FOCAL << 8) / z;
    let x = 160 + ((world_x * k) >> 8);
    let y = 120 + ((world_y * k) >> 8);
    if !(0..320).contains(&x) || !(0..240).contains(&y) {
        return Star::default();
    }
    // Brightness and size follow how close it has come. Both start higher
    // than the geometry alone suggests: a single dim pixel on a dark red
    // field reads as nothing, and the field is what these have to carry.
    let nearness = ((STAR_FAR - z) * 255 / span).clamp(0, 255);
    Star {
        x: x as i16,
        y: y as i16,
        size: match nearness {
            0..=110 => 1,
            111..=205 => 2,
            _ => 3,
        },
        bright: (100 + nearness * 155 / 255) as u8,
        visible: true,
    }
}

/// Where the warp streak behind star `index` starts: its position `delta`
/// travel ago, or `None` when a line between the two ends would be wrong.
///
/// Wrong two ways. Either end off screen is the easy one. The subtle one is a
/// star that wrapped back to the far plane between the two samples, where the
/// line would cross the whole sky. No distance threshold separates that from
/// a legitimately fast near star, but brightness does, exactly: approaching
/// is brightening, so a tail brighter than its head can only be a wrap.
pub fn streak_tail(index: u32, travel: i32, delta: i32) -> Option<(i16, i16)> {
    let head = star(index, travel);
    let tail = star(index, travel - delta);
    (head.visible && tail.visible && tail.bright <= head.bright).then_some((tail.x, tail.y))
}

/// Beats in a bar. Drum and bass is four to the floor at this level, so the
/// downbeat is every fourth.
pub const BEATS_PER_BAR: u32 = 4;

/// Where `now_ms` sits on the beat grid.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Beat {
    /// Which beat since the grid started.
    pub index: u32,
    /// 255 on the beat, falling to 0 just before the next one.
    pub pulse: u8,
    /// 255 exactly between two beats, falling to 0 on them. The other half of
    /// the bar from [`Beat::pulse`].
    pub offbeat: u8,
    /// 255 on the downbeat, falling across the whole bar.
    pub bar: u8,
}

impl Beat {
    /// Whether this is the first beat of a bar.
    pub fn is_downbeat(&self) -> bool {
        self.index % BEATS_PER_BAR == 0
    }
}

/// Place `now_ms` on the grid. `beat_ms` and `phase_ms` come from the disc
/// table, measured offline. A zero `beat_ms` means the track has no grid, and
/// everything stays still.
pub fn beat_at(now_ms: u32, beat_ms: u32, phase_ms: u32) -> Beat {
    if beat_ms == 0 {
        return Beat::default();
    }
    let since = now_ms.saturating_sub(phase_ms);
    let into = since % beat_ms;
    let pulse = (255 - (into * 255 / beat_ms).min(255)) as u8;
    // Furthest from either neighbouring beat is the middle.
    let offbeat = 255 - (pulse as i32 * 2 - 255).unsigned_abs().min(255) as u8;
    let index = since / beat_ms;
    let bar_ms = beat_ms * BEATS_PER_BAR;
    let into_bar = since % bar_ms;
    let bar = (255 - (into_bar * 255 / bar_ms).min(255)) as u8;
    Beat {
        index,
        pulse,
        offbeat,
        bar,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 340 ms a beat, grid starting 34 ms in.
    fn at(ms: u32) -> Beat {
        beat_at(ms, 340, 34)
    }

    #[test]
    fn a_pulse_peaks_on_the_beat_and_falls_away() {
        assert_eq!(at(34).pulse, 255, "on the beat");
        assert!(at(34 + 170).pulse < 140, "half a beat later");
        assert_eq!(at(34 + 340).pulse, 255, "and again next beat");
        assert!(at(34 + 339).pulse < 4, "just before it");
    }

    #[test]
    fn the_offbeat_peaks_between_two_beats() {
        assert!(at(34).offbeat < 4, "on the beat, nothing");
        assert!(at(34 + 170).offbeat > 250, "halfway, everything");
        assert!(at(34 + 339).offbeat < 8, "just before the next beat");
    }

    #[test]
    fn the_bar_runs_four_beats_and_the_downbeat_is_the_first() {
        assert!(at(34).is_downbeat());
        assert_eq!(at(34).bar, 255);
        for beat in 1..BEATS_PER_BAR {
            assert!(!at(34 + beat * 340).is_downbeat(), "beat {beat}");
        }
        assert!(at(34 + BEATS_PER_BAR * 340).is_downbeat(), "next bar");
        // The bar envelope falls the whole way across, not per beat.
        assert!(at(34 + 2 * 340).bar < 140);
    }

    #[test]
    fn a_track_with_no_measured_tempo_does_not_pulse() {
        for ms in [0, 1, 500, 100_000] {
            assert_eq!(beat_at(ms, 0, 0), Beat::default());
        }
    }

    #[test]
    fn an_impulse_points_somewhere_different_each_time_and_is_a_unit_vector() {
        let mut seen = [(0i32, 0i32); 8];
        for (i, slot) in seen.iter_mut().enumerate() {
            *slot = impulse(i as u32);
            // Q12 unit vector, so the squares sum to about 4096 squared.
            let len2 = slot.0 * slot.0 + slot.1 * slot.1;
            let unit = 4096i32 * 4096;
            assert!(
                (len2 - unit).abs() < unit / 20,
                "impulse {i} is not unit length: {len2}"
            );
        }
        // Different seeds, different directions. Not a proof of uniformity,
        // just that it is not stuck.
        assert!(seen.iter().any(|d| *d != seen[0]));
    }

    #[test]
    fn pitching_the_ball_moves_its_beads_off_the_yaw_only_positions() {
        let mut flat = [Bead::default(); SPHERE_POINTS];
        let mut tipped = [Bead::default(); SPHERE_POINTS];
        sphere(0, 0, 0, &mut flat);
        sphere(0, TURN / 8, 0, &mut tipped);
        assert!(
            flat.iter().zip(tipped.iter()).any(|(a, b)| a.y != b.y),
            "a pitch has to move something"
        );
    }

    #[test]
    fn a_swelling_ball_pushes_its_beads_apart() {
        let mut resting = [Bead::default(); SPHERE_POINTS];
        let mut swollen = [Bead::default(); SPHERE_POINTS];
        sphere(0, 0, 0, &mut resting);
        sphere(0, 0, 128, &mut swollen);
        let spread = |b: &[Bead; SPHERE_POINTS]| {
            b.iter().map(|x| x.y).max().unwrap() - b.iter().map(|x| x.y).min().unwrap()
        };
        assert!(spread(&swollen) > spread(&resting), "the ball itself grows");
    }

    #[test]
    fn a_visible_star_is_always_on_screen() {
        for travel in [0, 1, 700, 5_000, 100_000, -5_000] {
            for i in 0..256 {
                let s = star(i, travel);
                if !s.visible {
                    continue;
                }
                assert!(
                    (0..320).contains(&s.x),
                    "star {i} travel {travel} x={}",
                    s.x
                );
                assert!(
                    (0..240).contains(&s.y),
                    "star {i} travel {travel} y={}",
                    s.y
                );
            }
        }
    }

    #[test]
    fn stars_get_nearer_brighter_and_bigger_as_they_come_at_you() {
        // Each star wraps back out to the far plane on its own schedule, so
        // advancing `travel` does not always bring a given one closer: it can
        // jump from the near plane to the far one between two samples. Walk
        // one star and check brightness only rises, treating a sharp drop as
        // the wrap it is.
        let index = 7u32;
        let mut previous: Option<Star> = None;
        let mut approaches = 0;
        for travel in (0..STAR_FAR * 2).step_by(11) {
            let now = star(index, travel);
            if let Some(before) = previous {
                let wrapped = (before.bright as i32 - now.bright as i32) > 40;
                if !wrapped {
                    assert!(
                        now.bright >= before.bright,
                        "dimmer without wrapping at travel {travel}"
                    );
                    assert!(now.size >= before.size, "smaller without wrapping");
                    approaches += 1;
                }
            }
            previous = Some(now);
        }
        assert!(approaches > 100, "not enough of an approach to test");
    }

    #[test]
    fn the_field_wraps_rather_than_running_out() {
        let span = STAR_FAR - STAR_NEAR;
        assert_eq!(star(7, 0), star(7, span), "one lap round is where it began");
    }

    /// At full warp speed roughly a fifth of the field wraps between two
    /// frames; the guard exists for those, and must not eat the rest of the
    /// effect with them.
    #[test]
    fn most_stars_still_streak_at_warp_speed() {
        for travel in (0..STAR_FAR).step_by(97) {
            let (mut visible, mut streaked) = (0, 0);
            for i in 0..120u32 {
                if !star(i, travel).visible {
                    continue;
                }
                visible += 1;
                if streak_tail(i, travel, 280).is_some() {
                    streaked += 1;
                }
            }
            assert!(
                streaked * 2 >= visible,
                "only {streaked} of {visible} streak at travel {travel}"
            );
        }
    }

    /// A tail is the same world point seen further away, so it always sits
    /// radially inward of its head. A wrap that slipped the guard would put
    /// tails outward, all over the sky.
    #[test]
    fn a_streak_points_at_the_centre_of_the_screen() {
        for travel in (0..STAR_FAR * 2).step_by(53) {
            for i in 0..120u32 {
                if let Some((tx, ty)) = streak_tail(i, travel, 280) {
                    let head = star(i, travel);
                    assert!(
                        (tx as i32 - 160).abs() <= (head.x as i32 - 160).abs() + 1,
                        "star {i} travel {travel}: tail x outward of head"
                    );
                    assert!(
                        (ty as i32 - 120).abs() <= (head.y as i32 - 120).abs() + 1,
                        "star {i} travel {travel}: tail y outward of head"
                    );
                }
            }
        }
    }

    #[test]
    fn the_front_of_the_ring_is_lower_bigger_and_brighter_than_the_back() {
        // Zero is the front, where the selected entry sits; half a turn is
        // the far side of the ring.
        let front = place(0);
        let back = place(TURN / 2);
        assert!(front.z < back.z, "front is nearer");
        assert!(front.rx > back.rx, "front is bigger");
        assert!(front.y > back.y, "front sits lower on screen");
        assert!(front.front > back.front);
        assert_eq!(front.x, 160, "the selection is centred");
    }

    #[test]
    fn a_browse_kick_always_settles_back_to_the_idle_drift() {
        const IDLE: i32 = 5;
        for start in [IDLE + 110, IDLE - 110, IDLE, IDLE + 1, IDLE - 1] {
            let mut rate = start;
            for _ in 0..400 {
                rate = ease_spin(rate, IDLE, 4);
            }
            assert_eq!(rate, IDLE, "starting from {start}");
        }
    }

    #[test]
    fn a_kick_decays_rather_than_snapping() {
        let after_one_frame = ease_spin(115, 5, 4);
        assert!(after_one_frame < 115, "it slows");
        assert!(after_one_frame > 40, "but is still clearly spun up");
    }

    #[test]
    fn a_reflection_keeps_the_rings_near_and_far_the_way_round_they_were() {
        // Front of the ring sits lower on screen than the back; its
        // reflection has to as well, or the ring reads as turned over.
        // `place` offsets by half a turn, so angle zero is the front.
        let front = place(0);
        let back = place(TURN / 2);
        assert!(front.y > back.y, "front is lower to begin with");
        assert!(
            reflect_y(front.y) > reflect_y(back.y),
            "and lower still in the reflection"
        );
    }

    #[test]
    fn a_reflection_is_flatter_than_the_ring_and_sits_below_it() {
        let front = place(0);
        let back = place(TURN / 2);
        let real_spread = front.y - back.y;
        let reflected_spread = reflect_y(front.y) - reflect_y(back.y);
        assert!(reflected_spread < real_spread, "flattened");
        assert!(reflect_y(back.y) > front.y, "clear of the ring above it");
    }

    #[test]
    fn depth_sort_puts_the_far_ones_first() {
        let mut items = [3i32, 1, 4, 1, 5, 9, 2, 6];
        sort_by_depth(&mut items, |v| *v);
        assert_eq!(items, [9, 6, 5, 4, 3, 2, 1, 1]);
    }

    #[test]
    fn every_sphere_point_is_written() {
        let mut beads = [Bead::default(); SPHERE_POINTS];
        assert_eq!(sphere(0, 0, 0, &mut beads), SPHERE_POINTS);
        // The poles are the extremes; nothing should be outside them.
        let top = beads.iter().map(|b| b.y).min().unwrap();
        let bottom = beads.iter().map(|b| b.y).max().unwrap();
        assert!(bottom > top);
    }
}

#[cfg(test)]
mod field_density {
    use super::*;

    /// A starfield nobody can see is just arithmetic. This is the check that
    /// the volume still projects into the screen rather than around it.
    #[test]
    fn most_of_the_field_is_on_screen_at_any_moment() {
        for travel in (0..STAR_FAR).step_by(53) {
            let visible = (0..120u32).filter(|i| star(*i, travel).visible).count();
            assert!(
                visible >= 40,
                "only {visible} of 120 stars on screen at travel {travel}"
            );
        }
    }
}
