//! Compact crystal cannon VFX: a hot core, broken iris ring and ballistic shards.
//! At most 19 textured quads per event, using the existing particle page/CLUT.
use super::*;

const OCTAGON: [(i32, i32); 8] = [
    (256, 0),
    (181, 181),
    (0, 256),
    (-181, 181),
    (-256, 0),
    (-181, -181),
    (0, -256),
    (181, -181),
];

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_crystal_discharge<'a, const OT_DEPTH: usize>(
    effect: ProjectileImpactEffect,
    center: ProjectedVertex,
    focal: i32,
    depth_range: DepthRange,
    particle_material: TextureMaterial,
    ot: &mut OtFrame<'a, OT_DEPTH>,
    packets: &mut PrimitivePacketArena<'a>,
) -> usize {
    let muzzle = effect.kind == ProjectileEffectKind::Muzzle;
    let actor = effect.kind == ProjectileEffectKind::Actor;
    let life = i32::from(effect.visual.impact_lifetime_ticks.max(1));
    let age = i32::from(effect.age_ticks).min(life);
    if age >= life {
        return 0;
    }
    let base = (i32::from(effect.radius.max(if muzzle { 7 } else { 10 })) * focal
        / center.sz.max(1))
    .clamp(5, if muzzle { 20 } else { 26 });
    let slot = depth_range.slot::<OT_DEPTH>(center.sz);
    let at = |x: i32, y: i32| ProjectedVertex {
        sx: clamp_i16(i32::from(center.sx).saturating_add(x)),
        sy: clamp_i16(i32::from(center.sy).saturating_add(y)),
        ..center
    };
    let material = |rgb, remaining: i32, duration: i32| {
        particle_material
            .with_tint(rgb_tuple(scale_rgb(
                rgb,
                remaining.max(0) as u16,
                duration.max(1) as u16,
            )))
            .with_blend_mode(BlendMode::Add)
    };
    let fade = life - age;
    let mut submitted = 0;
    // Hold the core across several rendered frames at PS1 frame rates.
    // The core collapses first; the separated ring and shards carry the tail.
    if age < 7 {
        let hot = material(effect.visual.core_rgb, (9 - age).min(7), 7);
        submitted += draw_particle_diamond(
            center,
            (base * (9 - age) / 6).max(3) as i16,
            hot,
            slot,
            ot,
            packets,
        );
        submitted += draw_projectile_segment(
            at(-base * 2, 0),
            at(base * 2, 0),
            if age < 3 { 3 } else { 2 },
            hot,
            slot,
            ot,
            packets,
        );
    }
    if age < 11 {
        submitted += draw_particle_diamond(
            center,
            (base * 3 / 4).max(3) as i16,
            material(effect.visual.glow_rgb, 11 - age, 11),
            slot,
            ot,
            packets,
        );
    }
    // An expanding, broken octagon echoes the split rotating cannon collars.
    // Its fade is quadratic so the final frames do not leave a solid hoop.
    let motion_age = age * if muzzle { 9 } else { 22 } / life;
    let radius = base + base * motion_age / if muzzle { 5 } else { 7 };
    let ring = material(effect.visual.glow_rgb, fade * fade, life * life);
    for i in 0..8 {
        if muzzle && (i + age as usize / 2) % 4 == 0 {
            continue;
        }
        if !muzzle && !actor && i % 2 == 1 {
            continue;
        }
        let (ax, ay) = OCTAGON[i];
        let (bx, by) = OCTAGON[(i + 1) % 8];
        submitted += draw_projectile_segment(
            at(ax * radius / 256, ay * radius * 3 / 1024),
            at(bx * radius / 256, by * radius * 3 / 1024),
            if age < life / 2 { 2 } else { 1 },
            ring,
            slot,
            ot,
            packets,
        );
    }
    // Uneven radial speeds break the symmetry. Wall sparks stay narrow;
    // actor impacts shed wider, pale fragments before their jade afterglow.
    let count = if muzzle {
        4
    } else if actor {
        8
    } else {
        6
    };
    for i in 0..count {
        let (dx, dy) = OCTAGON[(i * 3 + 1) % 8];
        let speed = 3 + (i % 3) as i32;
        let travel = base * (4 + motion_age * speed) / 8;
        let length = (base * 3 * (life - age) / (2 * life)).max(3);
        let gravity = if muzzle {
            0
        } else {
            motion_age * motion_age * base / 240
        };
        let tip = at(dx * travel / 256, dy * travel / 256 + gravity);
        let tail = at(
            dx * (travel - length) / 256,
            dy * (travel - length) / 256 + gravity,
        );
        let tint = if i % 3 == 0 {
            effect.visual.core_rgb
        } else {
            effect.visual.impact_rgb
        };
        submitted += draw_projectile_segment(
            tail,
            tip,
            if actor && i % 3 == 0 && age < 12 {
                3
            } else if age < life * 2 / 3 {
                2
            } else {
                1
            },
            material(tint, fade * fade, life * life),
            slot,
            ot,
            packets,
        );
    }
    submitted
}
