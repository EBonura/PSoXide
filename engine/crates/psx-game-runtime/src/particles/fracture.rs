//! Directional contact wedge, short tears and staggered ballistic chips.
//! At most 13 packets; sharp Gouraud kites need no additional texture memory.
use super::*;
use psx_gpu::prim::QuadGouraudBlended;

fn hash(mut value: u32) -> u32 {
    value ^= value >> 16;
    value = value.wrapping_mul(0x7feb352d);
    value ^= value >> 15;
    value.wrapping_mul(0x846ca68b) ^ (value >> 16)
}

fn rotated((x, y): (i32, i32), angle: i32) -> (i32, i32) {
    let sin = i32::from(psx_math::sin_q12((angle & 4095) as u16));
    let cos = i32::from(psx_math::cos_q12((angle & 4095) as u16));
    ((x * cos - y * sin) / 4096, (x * sin + y * cos) / 4096)
}

fn fan_axis(center: ProjectedVertex, tail: Option<ProjectedVertex>, seed: u32) -> (i32, i32) {
    if let Some(tail) = tail {
        let dx = (i32::from(tail.sx) - i32::from(center.sx)).clamp(-1024, 1024);
        let dy = (i32::from(tail.sy) - i32::from(center.sy)).clamp(-1024, 1024);
        let length = psx_math::int32::isqrt_i32(dx * dx + dy * dy);
        if length >= 3 {
            return (dx * 256 / length, dy * 256 / length);
        }
    }
    // A head-on shot has no reliable screen bearing. Choose once per event,
    // rather than letting one-pixel projection noise spin the fan each frame.
    rotated((256, 0), (hash(seed) & 4095) as i32)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Chip {
    angle: i32,
    delay: i32,
    life: i32,
    speed: i32,
    length: i32,
}

fn chip(seed: u32, index: usize) -> Chip {
    let h = hash(seed.wrapping_add((index as u32 + 1).wrapping_mul(0x9e3779b9)));
    Chip {
        // A fan, not a circle: most chips kick back towards the incoming shot.
        angle: (h & 1023) as i32 - 512 + if index == 0 { 900 } else { 0 },
        delay: 2 + ((h >> 10) & 3) as i32,
        life: 13 + ((h >> 12) & 15) as i32,
        speed: 5 + ((h >> 16) & 7) as i32,
        length: 4 + ((h >> 20) & 7) as i32,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_fracture<const OT_DEPTH: usize>(
    effect: ProjectileImpactEffect,
    center: ProjectedVertex,
    tail: Option<ProjectedVertex>,
    focal: i32,
    depth_range: DepthRange,
    material: TextureMaterial,
    ot: &mut OtFrame<'_, OT_DEPTH>,
    packets: &mut PrimitivePacketArena<'_>,
) -> usize {
    let age = i32::from(effect.age_ticks);
    let life = i32::from(effect.visual.impact_lifetime_ticks);
    if age >= life {
        return 0;
    }
    let base = (i32::from(effect.radius.max(10)) * focal / center.sz.max(1)).clamp(5, 24);
    let axis = fan_axis(center, tail, effect.seed);
    let slot = depth_range.slot::<OT_DEPTH>(center.sz);
    let mut count = 0;
    // Modest 10% increase over the original fracture study. Scale final
    // offsets so small base radii do not round away the entire adjustment.
    let scaled = |v: i32| (v * 110 + v.signum() * 50) / 100;
    let at = |x: i32, y: i32| {
        (
            clamp_i16(i32::from(center.sx) + scaled(x)),
            clamp_i16(i32::from(center.sy) + scaled(y)),
        )
    };
    let fade = |color, left: i32, total: i32| {
        rgb_tuple(scale_rgb(color, left.max(0) as u16, total.max(1) as u16))
    };
    // Local coordinate x points back along the shot; y is the side of the fan.
    let local = |x: i32, y: i32| {
        at(
            (axis.0 * x - axis.1 * y) / 256,
            (axis.1 * x + axis.0 * y) / 256,
        )
    };
    if age < 7 {
        let hot = fade(effect.visual.core_rgb, (9 - age).min(7), 7);
        let blue = fade(effect.visual.glow_rgb, 7 - age, 7);
        let width = base * (9 - age) / 7;
        // Off-centre bite and one lopsided torn lip. No halo/ring stamp.
        count += quad(
            [
                local(-base / 2, 0),
                local(base / 3, -width),
                local(base / 2, width / 2),
                local(base * 2, -width / 3),
            ],
            [hot, hot, blue, blue],
            slot,
            ot,
            packets,
        );
        count += quad(
            [
                local(0, -width),
                local(-base / 3, -width / 3),
                local(base / 2, 0),
                local(base, width),
            ],
            [blue, hot, hot, blue],
            slot,
            ot,
            packets,
        );
        count += draw_particle_diamond(
            center,
            scaled((base / 2).max(3)) as i16,
            material.with_tint(hot).with_blend_mode(BlendMode::Add),
            slot,
            ot,
            packets,
        );
    }
    // Two or three dominant unequal tears, fading before the falling debris.
    let tears = 2 + (hash(effect.seed ^ 0x51a3) & 1) as usize;
    for i in 0..tears {
        let h = hash(effect.seed.wrapping_add(i as u32 * 7919));
        let t = age - (h & 1) as i32;
        if !(0..12).contains(&t) {
            continue;
        }
        let angle = ((h >> 4) & 1023) as i32 - 512;
        let dir = rotated(axis, angle);
        let extent = base * (3 + ((h >> 15) & 3) as i32) * (t + 4).min(9) / 9;
        let root = base * t / 10;
        let width = (base * (12 - t) / 30).max(1);
        let rear = at(dir.0 * root / 256, dir.1 * root / 256);
        let shoulder = root + (extent - root) / 5;
        let left = at(
            (dir.0 * shoulder - dir.1 * width) / 256,
            (dir.1 * shoulder + dir.0 * width) / 256,
        );
        let right = at(
            (dir.0 * shoulder + dir.1 * width) / 256,
            (dir.1 * shoulder - dir.0 * width) / 256,
        );
        let tip = at(dir.0 * extent / 256, dir.1 * extent / 256);
        let hot = fade(effect.visual.core_rgb, 12 - t, 12);
        let blue = fade(effect.visual.glow_rgb, 12 - t, 16);
        count += quad(
            [rear, left, right, tip],
            [hot, hot, blue, blue],
            slot,
            ot,
            packets,
        );
    }
    let chips = 5 + (hash(effect.seed ^ 0xa537) % 3) as usize;
    for i in 0..chips {
        let c = chip(effect.seed, i);
        let t = age - c.delay;
        let duration = c.life.min(life - c.delay);
        if t < 0 || t >= duration {
            continue;
        }
        let dir = rotated(axis, c.angle);
        // Fragments separate over a few frames, then settle under gravity.
        let distance = base * (3 + t * c.speed) / 24;
        let gravity = base * t * t / 320;
        let x = dir.0 * distance / 256;
        let y = dir.1 * distance / 256 + gravity;
        let spin = rotated(dir, t * (if i & 1 == 0 { 37 } else { -29 }));
        let length = (base * c.length / 12).max(3);
        let width = (length / 3).clamp(2, 4);
        let rear = at(x - spin.0 * length / 512, y - spin.1 * length / 512);
        let tip = at(x + spin.0 * length / 256, y + spin.1 * length / 256);
        let left = at(x - spin.1 * width / 256, y + spin.0 * width / 256);
        let right = at(x + spin.1 * width / 256, y - spin.0 * width / 256);
        let remaining = duration - t;
        let hot = fade(effect.visual.impact_rgb, remaining.min(10), 10);
        let blue = fade(effect.visual.glow_rgb, remaining.min(10), 13);
        count += quad(
            [rear, left, right, tip],
            [blue, hot, blue, hot],
            slot,
            ot,
            packets,
        );
    }
    count
}

fn quad<const OT_DEPTH: usize>(
    vertices: [(i16, i16); 4],
    colors: [(u8, u8, u8); 4],
    slot: psx_engine::DepthSlot,
    ot: &mut OtFrame<'_, OT_DEPTH>,
    packets: &mut PrimitivePacketArena<'_>,
) -> usize {
    let Some(packet) = packets.push(QuadGouraudBlended::new(vertices, colors, BlendMode::Add))
    else {
        return 0;
    };
    ot.add_slot(slot, packet, QuadGouraudBlended::WORDS);
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragments_keep_parameters_between_frames_but_vary_between_hits() {
        let a: [_; 7] = core::array::from_fn(|i| chip(100, i));
        let b: [_; 7] = core::array::from_fn(|i| chip(101, i));
        assert_eq!(a, core::array::from_fn(|i| chip(100, i)));
        assert_ne!(a, b);
        assert!(a.iter().any(|c| c.life != a[0].life));
        assert!(a.iter().any(|c| c.delay != a[0].delay));
    }

    #[test]
    fn fan_follows_incoming_shot_and_head_on_fallback_is_stable() {
        let center = ProjectedVertex {
            sx: 100,
            sy: 100,
            sz: 100,
        };
        let tail = ProjectedVertex { sx: 80, ..center };
        assert_eq!(fan_axis(center, Some(tail), 100), (-256, 0));
        let a = fan_axis(center, Some(center), 100);
        assert_eq!(a, fan_axis(center, None, 100));
        assert_ne!(a, fan_axis(center, None, 101));
    }
}
