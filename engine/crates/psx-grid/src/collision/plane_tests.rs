//! Free-plane collision: a bridge over a pit, a ramp, a slanted wall, a
//! blocker box and the invariants every trace keeps.

use super::body::{Query, Raw};
use super::planes::box_misses;
use super::tests::{body, build, point, trace, walk, Built, Lcg, Room};
use super::world::GridCollision;
use crate::format::*;
use psx_engine::{CollisionTraceQuery, RoomPoint};
use psx_math::int32::isqrt_i32;

fn platform(corners: [[i16; 2]; 4], y0: i16, sx: i16, sz: i16) -> ColPlane {
    ColPlane {
        kind: plane_kind::PLATFORM,
        xz: corners,
        y0,
        sx_q12: sx,
        sz_q12: sz,
        extent: 16,
        ..ColPlane::default()
    }
}

fn wall(a: [i16; 2], b: [i16; 2], bottom: i16, height: i16) -> ColPlane {
    let (dx, dz) = (i32::from(b[0] - a[0]), i32::from(b[1] - a[1]));
    let length = isqrt_i32(dx * dx + dz * dz).max(1);
    ColPlane {
        kind: plane_kind::WALL,
        xz: [a, b, [0; 2], [0; 2]],
        y0: bottom,
        sx_q12: (dx * 4096 / length) as i16,
        sz_q12: (dz * 4096 / length) as i16,
        extent: height,
        ..ColPlane::default()
    }
}

fn blocker(min: [i16; 2], max: [i16; 2], bottom: i16, height: i16) -> ColPlane {
    ColPlane {
        kind: plane_kind::BLOCKER,
        xz: [min, max, [0; 2], [0; 2]],
        y0: bottom,
        extent: height,
        ..ColPlane::default()
    }
}

fn rect(x0: i16, z0: i16, x1: i16, z1: i16) -> [[i16; 2]; 4] {
    [[x0, z0], [x1, z0], [x1, z1], [x0, z1]]
}

/// A ditch two cells wide and 200 deep with a narrow bridge across it. (The
/// motor will not walk into void, so the ditch has a floor to land on.)
fn bridge_room() -> Room {
    let mut room = Room::flat([0, 0, 0], 8, 3, 0);
    for cz in 0..3 {
        room.set_floor(3, cz, -200);
        room.set_floor(4, cz, -200);
    }
    room.planes
        .push(platform(rect(200, 100, 420, 140), 0, 0, 0));
    room
}

#[test]
fn a_bridge_carries_the_character_across_a_ditch_and_the_ditch_catches_the_rest() {
    let built = build(&[bridge_room()]);
    built.run(|w| {
        assert!(w.planes);
        let on = walk(w, RoomPoint::new(120, 0, 120), 4096, 0, 160);
        assert!(on.x > 380, "must cross the bridge: {on:?}");
        assert_eq!(on.y, 0, "{on:?}");
        // Along z = 40 there is no bridge: the body walks off the ledge.
        let off = walk(w, RoomPoint::new(120, 0, 40), 4096, 0, 160);
        assert_eq!(off.y, -200, "{off:?}");
    });
}

#[test]
fn the_bridge_has_edges_and_an_underside() {
    let built = build(&[bridge_room()]);
    built.run(|w| {
        // Walking off the side of the bridge falls into the ditch.
        let side = walk(w, RoomPoint::new(280, 0, 120), 0, 4096, 80);
        assert_eq!(side.y, -200, "{side:?}");
        // A body under the bridge does not pass through its slab.
        assert!(w.body_blocked([280, -8, 120], 12, 48));
        assert!(!w.body_blocked([280, -70, 120], 12, 48));
        // Standing on the bridge: the trace down stops on the top with a
        // flat upward normal.
        let t = trace(w, body([280, 40, 120], [280, -200, 120]));
        assert_eq!(t.end.y, 0, "{t:?}");
        assert_eq!(t.normal_q12, [0, 4096, 0]);
        // The same beside the bridge, over the ditch, lands on its floor.
        let t = trace(w, body([280, 40, 60], [280, -200, 60]));
        assert_eq!(t.end.y, -200, "{t:?}");
    });
}

#[test]
fn a_ramp_plane_is_walked_up_with_its_slope_normal() {
    // Rises 40 over 160 along +x (slope 0.25), over a floor at -60 so the
    // ramp is the only way up.
    let mut room = Room::flat([0, 0, 0], 8, 3, -60);
    room.planes
        .push(platform(rect(80, 100, 240, 160), 0, 1024, 0));
    let built = build(&[room]);
    built.run(|w| {
        let start = RoomPoint::new(90, 5, 130);
        let end = walk(w, start, 4096, 0, 14);
        let expected = (end.x - 80) / 4;
        assert!(end.x > 120, "{end:?}");
        assert!(
            (end.y - expected).abs() <= 3,
            "feet follow the slope: {end:?} wanted y near {expected}"
        );
        let t = trace(w, body([160, 100, 130], [160, -100, 130]));
        assert!(t.normal_q12[0] < 0 && t.normal_q12[1] > 3900, "{t:?}");
        // The highest of the five footprint samples carries the body: the one
        // 12 units up the slope, 3 units above the surface under the centre.
        assert!((t.end.y - 23).abs() <= 1, "{t:?}");
    });
}

#[test]
fn a_slanted_wall_plane_stops_bodies_and_points() {
    let mut room = Room::flat([0, 0, 0], 8, 4, 0);
    // From (160, 20) to (280, 300): leans across the room.
    room.planes.push(wall([160, 20], [280, 300], 0, 120));
    let built = build(&[room]);
    built.run(|w| {
        let end = walk(w, RoomPoint::new(100, 0, 150), 4096, 0, 200);
        // The wall at z = 150 is at x = 160 + 120 * (130 / 280) = 215.7.
        assert!(end.x < 216, "{end:?}");
        assert!(end.x > 190, "{end:?}");
        // A point trace crosses the wall once and reports a horizontal
        // normal facing back at the start.
        let t = trace(w, point([100, 30, 150], [400, 30, 150]));
        assert!(t.fraction_q12 < 4096, "{t:?}");
        assert_eq!(t.normal_q12[1], 0);
        assert!(t.normal_q12[0] < 0, "{t:?}");
        assert!((t.end.x - 216).abs() <= 2, "{t:?}");
        // Over the top of the wall (above 120) and past its end are clear.
        assert_eq!(
            trace(w, point([100, 130, 150], [400, 130, 150])).fraction_q12,
            4096
        );
        assert_eq!(
            trace(w, point([100, 30, 330], [400, 30, 330])).fraction_q12,
            4096
        );
        // Line of sight follows the same crossing.
        assert!(!w.line_of_sight([100, 30, 150], [400, 30, 150]));
        assert!(w.line_of_sight([100, 130, 150], [400, 130, 150]));
        // From the other side the normal faces the other way.
        let t = trace(w, point([400, 30, 150], [100, 30, 150]));
        assert!(t.normal_q12[0] > 0, "{t:?}");
    });
}

#[test]
fn a_blocker_box_blocks_from_every_side() {
    let mut room = Room::flat([0, 0, 0], 6, 6, 0);
    room.planes.push(blocker([180, 180], [260, 260], 0, 100));
    let built = build(&[room]);
    built.run(|w| {
        for (from, to) in [
            ([100, 0, 220], [400, 0, 220]),
            ([340, 0, 220], [100, 0, 220]),
            ([220, 0, 100], [220, 0, 400]),
            ([220, 0, 340], [220, 0, 100]),
        ] {
            let t = trace(w, body(from, to));
            assert!(t.fraction_q12 < 4096, "{from:?} {t:?}");
        }
        // Above it is clear; its top carries a body.
        let t = trace(w, body([220, 140, 220], [220, 0, 220]));
        assert_eq!(t.end.y, 100, "{t:?}");
        let p = trace(w, point([220, 300, 220], [220, -50, 220]));
        assert!(p.fraction_q12 < 4096 && (p.end.y - 100).abs() <= 1, "{p:?}");
        // A point inside the box is solid.
        assert!(w.point_in_solid([220, 50, 220]));
        assert!(!w.point_in_solid([220, 150, 220]));
    });
}

#[test]
fn rays_start_solid_inside_a_platform_slab_and_pass_beside_it() {
    let built = build(&[bridge_room()]);
    built.run(|w| {
        let t = trace(w, point([280, -8, 120], [280, -8, 130]));
        assert!(t.start_solid, "{t:?}");
        // Skimming the top surface is clear.
        assert_eq!(
            trace(w, point([100, 0, 120], [400, 0, 120])).fraction_q12,
            4096
        );
        // Through the slab from below: blocked, normal down.
        let t = trace(w, point([280, -60, 120], [280, 30, 120]));
        assert!(
            t.fraction_q12 < 4096 && t.normal_q12 == [0, -4096, 0],
            "{t:?}"
        );
    });
}

fn property_world() -> Built {
    let mut room = Room::flat([0, 0, 0], 6, 6, 0);
    room.set_floor(0, 5, -80);
    room.set_floor(5, 0, -80);
    room.planes
        .push(platform(rect(60, 60, 200, 120), 20, 512, -256));
    room.planes
        .push(platform(rect(300, 300, 420, 360), 10, 0, 0));
    room.planes.push(wall([100, 200], [300, 260], 0, 90));
    room.planes.push(wall([380, 40], [440, 220], 10, 60));
    room.planes.push(blocker([220, 20], [270, 80], 0, 70));
    build(&[room])
}

#[test]
fn traces_never_end_inside_solid_with_planes() {
    let built = property_world();
    built.run(|w| {
        let mut rng = Lcg(2026);
        let mut hits = 0;
        for i in 0..6000 {
            let a = [
                rng.range(-20, 500),
                rng.range(-40, 160),
                rng.range(-20, 500),
            ];
            let b = match i % 4 {
                0 => [
                    rng.range(-20, 500),
                    rng.range(-40, 160),
                    rng.range(-20, 500),
                ],
                1 => [a[0], rng.range(-400, 400), a[2]],
                2 => [rng.range(-20, 500), a[1], rng.range(-20, 500)],
                _ => [
                    a[0] + rng.range(-30, 30),
                    a[1] + rng.range(-30, 30),
                    a[2] + rng.range(-30, 30),
                ],
            };
            let (r, h) = match (i / 4) % 3 {
                0 => (0, 0),
                1 => (12, 48),
                _ => (6, 30),
            };
            let q = if r == 0 {
                point(a, b)
            } else {
                CollisionTraceQuery::body(RoomPoint::from_array(a), RoomPoint::from_array(b), r, h)
            };
            let t = trace(w, q);
            let blocked = |p: [i32; 3]| {
                if r == 0 {
                    w.point_in_solid(p)
                } else {
                    w.body_blocked(p, r, h)
                }
            };
            assert!((0..=4096).contains(&t.fraction_q12), "{q:?} {t:?}");
            if t.all_solid {
                assert!(blocked(a) && blocked(b));
            } else if t.start_solid {
                assert!(blocked(a) && !blocked(b), "{q:?} {t:?}");
            } else {
                assert!(!blocked(a), "{q:?}");
                assert!(!blocked(t.end.to_array()), "{q:?} -> {t:?}");
                if t.fraction_q12 < 4096 {
                    hits += 1;
                }
            }
        }
        assert!(hits > 400, "the world must produce hits: {hits}");
    });
}

#[test]
fn a_level_without_planes_keeps_the_plane_paths_off() {
    let built = build(&[Room::flat([0, 0, 0], 3, 3, 0)]);
    built.run(|w| assert!(!w.planes));
}

#[test]
fn the_camera_arm_passes_through_flagged_planes_and_everything_else_still_collides() {
    use super::provider::GridTraceProvider;
    use psx_engine::trace_collision;
    let mut screen = wall([160, 20], [160, 300], 0, 120);
    let blocker_box = blocker([300, 100], [340, 140], 0, 100);
    // Only the wall is flagged; the box is not.
    screen.flags |= COLPLANE_CAMERA_PASS;
    let mut room = Room::flat([0, 0, 0], 8, 4, 0);
    room.planes.push(screen);
    room.planes.push(blocker_box);
    let built = build(&[room]);
    built.run(|w| {
        let across = point([100, 30, 150], [220, 30, 150]);
        let into_box = point([260, 30, 120], [400, 30, 120]);
        // An ordinary provider (the motor, sight checks) hits both.
        let mut normal = GridTraceProvider::new(w);
        assert!(trace_collision(&mut normal, across).unwrap().fraction_q12 < 4096);
        assert!(trace_collision(&mut normal, into_box).unwrap().fraction_q12 < 4096);
        // The camera arm goes through the flagged wall and stops at the box.
        let mut camera = GridTraceProvider::for_camera(w);
        assert_eq!(
            trace_collision(&mut camera, across).unwrap().fraction_q12,
            4096
        );
        assert!(trace_collision(&mut camera, into_box).unwrap().fraction_q12 < 4096);
        // The body of the character still collides with the flagged wall.
        let mut body_trace = GridTraceProvider::new(w);
        let t = trace_collision(&mut body_trace, body([100, 0, 150], [300, 0, 150])).unwrap();
        assert!(t.fraction_q12 < 4096, "{t:?}");
    });
}

/// The ray loop as it was before the box test: every primary plane, no
/// rejection.
fn ray_planes_unfiltered(w: &GridCollision<'_>, room: usize, q: &Query, hit: &mut Raw) {
    let (Some(view), rec) = (w.view(room), w.rec(room)) else {
        return;
    };
    let a = [
        q.a[0] - rec.origin[0],
        q.a[1] - rec.origin[1],
        q.a[2] - rec.origin[2],
    ];
    let b = [a[0] + q.d[0], a[1] + q.d[1], a[2] + q.d[2]];
    for (index, plane) in view.col_planes.iter().enumerate() {
        if plane.flags & COLPLANE_PRIMARY == 0 {
            continue;
        }
        if w.camera_pass && plane.flags & COLPLANE_CAMERA_PASS != 0 {
            continue;
        }
        match plane.kind {
            plane_kind::WALL => w.ray_wall(room, index, plane, &a, &b, q, hit),
            plane_kind::PLATFORM => w.ray_platform(room, index, plane, &a, &b, q, hit),
            _ => w.ray_blocker(room, index, plane, &a, &b, hit),
        }
    }
}

#[test]
fn the_box_test_never_changes_what_a_ray_finds() {
    let built = property_world();
    built.run(|w| {
        let mut rng = Lcg(77);
        let mut found = 0;
        let mut skipped = 0;
        for i in 0..20000 {
            let a = [
                rng.range(-60, 520),
                rng.range(-120, 200),
                rng.range(-60, 520),
            ];
            let d = match i % 5 {
                0 => [rng.range(-40, 40), rng.range(-40, 40), rng.range(-40, 40)],
                1 => [
                    rng.range(-600, 600),
                    rng.range(-200, 200),
                    rng.range(-600, 600),
                ],
                2 => [0, rng.range(-400, 400), 0],
                3 => [rng.range(-300, 300), 0, 0],
                _ => [0, 0, rng.range(-300, 300)],
            };
            let q = Query {
                a,
                d,
                r: 0,
                h: 0,
                body: false,
            };
            let (mut with, mut without) = (Raw::new(), Raw::new());
            with.fraction = 4097;
            without.fraction = 4097;
            w.ray_planes(0, &q, &mut with);
            ray_planes_unfiltered(w, 0, &q, &mut without);
            assert_eq!(with.fraction, without.fraction, "{a:?} {d:?}");
            assert_eq!(with.normal, without.normal, "{a:?} {d:?}");
            assert_eq!(with.start_solid, without.start_solid, "{a:?} {d:?}");
            let (s, t) = (with.src, without.src);
            assert_eq!(
                (s.kind, s.aux, s.cx, s.cz, s.room, s.span),
                (t.kind, t.aux, t.cx, t.cz, t.room, t.span)
            );
            found += usize::from(with.fraction <= 4096 || with.start_solid);
            // Count how many planes the box test saved a crossing test.
            let view = w.view(0).unwrap();
            let rec = w.rec(0);
            let lo = [0, 1, 2].map(|k| (a[k] - rec.origin[k]).min(a[k] - rec.origin[k] + d[k]));
            let hi = [0, 1, 2].map(|k| (a[k] - rec.origin[k]).max(a[k] - rec.origin[k] + d[k]));
            skipped += view
                .col_planes
                .iter()
                .filter(|p| p.flags & COLPLANE_PRIMARY != 0 && box_misses(p, &lo, &hi))
                .count();
        }
        assert!(found > 500, "the rays must hit planes: {found}");
        assert!(skipped > 20000, "the box test must skip planes: {skipped}");
    });
}

#[test]
fn the_room_plane_box_holds_every_plane_and_is_inverted_without_planes() {
    let built = property_world();
    built.run(|w| {
        let view = w.view(0).unwrap();
        let [x0, z0, x1, z1] = view.plane_box;
        assert!(x0 < x1 && z0 < z1);
        for plane in view.col_planes {
            let corners = if plane.kind == plane_kind::PLATFORM {
                4
            } else {
                2
            };
            for c in &plane.xz[..corners] {
                let (x, z) = (i32::from(c[0]), i32::from(c[1]));
                assert!(
                    (x0..=x1).contains(&x) && (z0..=z1).contains(&z),
                    "{plane:?}"
                );
            }
        }
        // The box is tight: the wall at x = 440 is the east edge.
        assert_eq!((x0, x1), (60, 440));
    });
    let bare = build(&[Room::flat([0, 0, 0], 3, 3, 0)]);
    bare.run(|w| {
        let [x0, z0, x1, z1] = w.view(0).unwrap().plane_box;
        assert!(x0 > x1 && z0 > z1);
    });
}
