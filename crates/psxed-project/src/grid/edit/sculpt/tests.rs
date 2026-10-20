//! Sculpt on small worlds: welded corners on the 64 lattice, exact replay,
//! every mode, the detail levels, storeys and the headroom rule.

use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::grid::leaf::LeafKey;
use crate::grid::room::GridRoom;
use crate::grid::sector::GridSpan;
use crate::grid::surface::GridSurface;
use crate::grid::units::GridScale;
use crate::ResourceId;

fn rid(n: u64) -> ResourceId {
    ron::from_str(&format!("({n})")).expect("a resource id")
}

/// A derived `w` by `d` room, floors at 0 and (when asked) ceilings at 6144.
fn world(w: usize, d: usize, ceiling: bool) -> GridWorld {
    let mut room = GridRoom::new_derived("field", w, d);
    for z in 0..d {
        for x in 0..w {
            let s = room.sector_or_insert(x, z).unwrap();
            s.set_ground_floor(Some(GridSurface::flat_auto(0, Some(rid(1)))));
            if ceiling {
                s.set_ground_ceiling(Some(GridSurface::flat_auto(6144, Some(rid(2)))));
            }
        }
    }
    GridWorld {
        scale: GridScale::DEFAULT,
        rooms: vec![room],
        ..GridWorld::default()
    }
}

/// Every floor corner height by lattice vertex: the set of values the leaves
/// that meet there give it.
fn lattice(world: &GridWorld) -> BTreeMap<[i32; 2], BTreeSet<i32>> {
    let mut out: BTreeMap<[i32; 2], BTreeSet<i32>> = BTreeMap::new();
    for (key, sector) in world.rooms[0].leaves() {
        let Some(f) = sector.ground_floor() else {
            continue;
        };
        for (v, h) in leaf_corners(key).into_iter().zip(f.heights) {
            out.entry(v).or_default().insert(h);
        }
    }
    out
}

fn at(world: &GridWorld, v: [i32; 2]) -> i32 {
    *lattice(world)[&v].iter().next().unwrap()
}

fn stroke(world: &mut GridWorld, path: &[[f64; 2]], params: SculptParams) -> SculptReport {
    sculpt_path(world, 0, path, params).expect("a stroke")
}

/// `n` dabs on one spot: a press held in place.
fn press(world: &mut GridWorld, at: [f64; 2], n: usize, params: SculptParams) -> SculptReport {
    let mut stroke = Sculpt::begin(world, 0, at, params).expect("a stroke");
    for _ in 1..n {
        stroke.add_dab(at, true);
    }
    stroke.apply(world)
}

fn welded_and_on_the_lattice(world: &GridWorld) {
    for (v, heights) in lattice(world) {
        assert_eq!(heights.len(), 1, "{v:?} is torn: {heights:?}");
        for h in heights {
            assert_eq!(h % FINE_UNITS, 0, "{v:?} is off the 64 lattice: {h}");
        }
    }
    assert!(tree::check(&world.rooms[0]).is_empty());
}

#[test]
fn raise_lifts_the_ground_under_the_brush_and_leaves_the_rest_alone() {
    let mut w = world(12, 12, false);
    let r = press(
        &mut w,
        [24.0, 24.0],
        12,
        SculptParams {
            radius: 8.0,
            strength: 1.0,
            ..SculptParams::default()
        },
    );
    assert!(r.moved > 0 && r.peak >= FINE_UNITS, "{r:?}");
    welded_and_on_the_lattice(&w);
    // Higher at the middle than toward the rim, and untouched outside.
    assert!(at(&w, [24, 24]) > at(&w, [28, 24]), "peak above the slope");
    assert!(at(&w, [28, 24]) > 0);
    assert_eq!(at(&w, [4, 4]), 0);
    assert_eq!(at(&w, [44, 44]), 0);
    // No leaf was divided: Sector detail.
    assert_eq!(r.leaves[0], r.leaves[1]);
}

#[test]
fn inverted_raise_digs_and_the_falloffs_differ() {
    let mut up = world(12, 12, false);
    let mut down = up.clone();
    let params = SculptParams {
        radius: 8.0,
        strength: 1.0,
        ..SculptParams::default()
    };
    press(&mut up, [24.0, 24.0], 12, params.clone());
    press(
        &mut down,
        [24.0, 24.0],
        12,
        SculptParams {
            invert: true,
            ..params.clone()
        },
    );
    for (v, h) in lattice(&up) {
        let d = lattice(&down)[&v].iter().next().copied().unwrap();
        assert_eq!(*h.iter().next().unwrap(), -d, "{v:?}");
    }
    // A sharp brush leaves less on the shoulder than a linear one.
    let mut sharp = world(12, 12, false);
    let mut linear = sharp.clone();
    press(
        &mut sharp,
        [24.0, 24.0],
        12,
        SculptParams {
            falloff: Falloff::Sharp,
            ..params.clone()
        },
    );
    press(
        &mut linear,
        [24.0, 24.0],
        12,
        SculptParams {
            falloff: Falloff::Linear,
            ..params
        },
    );
    assert!(at(&sharp, [28, 24]) < at(&linear, [28, 24]));
}

#[test]
fn a_stroke_replays_exactly_and_a_longer_stroke_extends_the_shorter_one() {
    let w0 = world(12, 12, false);
    let params = SculptParams {
        radius: 6.0,
        strength: 0.8,
        ..SculptParams::default()
    };
    let mut live = Sculpt::begin(&w0, 0, [10.0, 10.0], params.clone()).unwrap();
    let mut w = w0.clone();
    live.add_dab([14.0, 11.0], true);
    let first = live.apply(&mut w);
    let again = live.apply(&mut w);
    assert_eq!(first, again);
    let short = w.clone();
    live.add_dab([18.0, 12.0], true);
    live.add_dab([22.0, 12.0], true);
    live.apply(&mut w);
    assert_ne!(w, short);
    // Replaying the stroke from scratch gives the same document.
    let mut other = w0.clone();
    let mut replay = Sculpt::begin(&other, 0, [10.0, 10.0], params).unwrap();
    for at in [[14.0, 11.0], [18.0, 12.0], [22.0, 12.0]] {
        replay.add_dab(at, true);
    }
    replay.apply(&mut other);
    assert_eq!(w, other);
    // Nothing of the stroke is left in the base: dropping every dab but the
    // first at zero strength puts the room back.
    let mut idle = Sculpt::begin(
        &w0,
        0,
        [10.0, 10.0],
        SculptParams {
            strength: 0.0,
            ..SculptParams::default()
        },
    )
    .unwrap();
    idle.add_dab([14.0, 14.0], true);
    let mut untouched = w.clone();
    idle.apply(&mut untouched);
    assert_eq!(untouched, w0);
}

#[test]
fn smooth_takes_a_spike_down_and_flatten_levels_to_where_the_stroke_began() {
    let mut w = world(12, 12, false);
    // A spike: one sector corner raised four clicks.
    for (x, z) in [(5usize, 5usize), (4, 5), (5, 4), (4, 4)] {
        let f = w.rooms[0]
            .sector_mut(x, z)
            .unwrap()
            .ground_floor_mut()
            .unwrap();
        let corner = match (x, z) {
            (5, 5) => 0,
            (4, 5) => 1,
            (5, 4) => 3,
            _ => 2,
        };
        f.heights[corner] = 1024;
    }
    assert_eq!(at(&w, [20, 20]), 1024);
    let smooth = SculptParams {
        mode: SculptMode::Smooth,
        radius: 6.0,
        strength: 1.0,
        ..SculptParams::default()
    };
    let mut s = w.clone();
    stroke(&mut s, &[[20.0, 20.0], [20.5, 20.0], [21.0, 20.0]], smooth);
    welded_and_on_the_lattice(&s);
    assert!(at(&s, [20, 20]) < 1024, "the spike came down");
    // Flatten from a point on the flat ground: the spike goes to 0.
    let mut f = w.clone();
    stroke(
        &mut f,
        &[[12.0, 20.0], [16.0, 20.0], [20.0, 20.0], [22.0, 20.0]],
        SculptParams {
            mode: SculptMode::Flatten,
            radius: 6.0,
            strength: 1.0,
            ..SculptParams::default()
        },
    );
    welded_and_on_the_lattice(&f);
    assert_eq!(at(&f, [20, 20]), 0, "level with where the stroke began");
}

#[test]
fn ramp_makes_a_straight_grade_from_a_to_b_with_the_chosen_rise() {
    let mut w = world(16, 6, false);
    let r = stroke(
        &mut w,
        &[[8.0, 12.0], [40.0, 12.0]],
        SculptParams {
            mode: SculptMode::Ramp,
            radius: 6.0,
            rise: 1024,
            ..SculptParams::default()
        },
    );
    assert!(r.moved > 0, "{r:?}");
    welded_and_on_the_lattice(&w);
    let line: Vec<i32> = (8..=40).step_by(4).map(|x| at(&w, [x, 12])).collect();
    assert_eq!(line[0], 0);
    assert_eq!(*line.last().unwrap(), 1024);
    assert!(line.windows(2).all(|p| p[0] <= p[1]), "{line:?}");
    // A straight grade: 1024 over 32 clicks is 128 per four clicks.
    for (i, h) in line.iter().enumerate() {
        let want = 128 * i as i32;
        assert!((h - want).abs() <= FINE_UNITS, "{line:?}");
    }
}

#[test]
fn dunes_ripple_across_the_drag_with_the_given_wavelength_and_height() {
    let mut w = world(16, 8, false);
    stroke(
        &mut w,
        &[[8.0, 16.0], [56.0, 16.0]],
        SculptParams {
            mode: SculptMode::Dunes,
            radius: 6.0,
            wavelength: 16.0,
            dune_height: 256,
            ..SculptParams::default()
        },
    );
    welded_and_on_the_lattice(&w);
    // Along the drag: a crest a quarter wavelength in, a trough at three
    // quarters, back to zero after a whole one.
    assert_eq!(at(&w, [12, 16]), 256);
    assert_eq!(at(&w, [20, 16]), -256);
    assert_eq!(at(&w, [24, 16]), 0);
    // Across the drag the crest line is straight and fades with the falloff.
    assert!(at(&w, [12, 20]) > 0 && at(&w, [12, 20]) < 256);
    assert_eq!(at(&w, [12, 20]).signum(), at(&w, [12, 12]).signum());
    // Shift flips the ripples.
    let mut flipped = world(16, 8, false);
    stroke(
        &mut flipped,
        &[[8.0, 16.0], [56.0, 16.0]],
        SculptParams {
            mode: SculptMode::Dunes,
            radius: 6.0,
            wavelength: 16.0,
            dune_height: 256,
            invert: true,
            ..SculptParams::default()
        },
    );
    assert_eq!(at(&flipped, [12, 16]), -256);
}

#[test]
fn the_detail_option_decides_how_fine_the_cells_under_the_brush_become() {
    let base = world(8, 8, false);
    let narrow = SculptParams {
        radius: 3.0,
        strength: 1.0,
        falloff: Falloff::Sharp,
        ..SculptParams::default()
    };
    let level_of = |w: &GridWorld| w.rooms[0].leaves().map(|(k, _)| k.level).max().unwrap_or(0);
    for (detail, deepest) in [(Detail::Sector, 2), (Detail::Half, 3), (Detail::Quarter, 4)] {
        let mut w = base.clone();
        let r = press(
            &mut w,
            [16.0, 16.0],
            24,
            SculptParams {
                detail,
                ..narrow.clone()
            },
        );
        assert_eq!(level_of(&w), deepest, "{detail:?}: {r:?}");
        if detail != Detail::Sector {
            assert!(r.leaves[1] > r.leaves[0], "{detail:?}: {r:?}");
        }
        welded_and_on_the_lattice(&w);
        // Cells far from the brush are untouched.
        assert!(w.rooms[0].leaf(LeafKey::sector([0, 0])).is_some_and(|s| s
            .ground_floor()
            .unwrap()
            .heights
            == [0; 4]));
    }
    // Auto: a tight bump needs fine cells where it is, nowhere else...
    let mut auto = base.clone();
    let r = press(
        &mut auto,
        [16.0, 16.0],
        24,
        SculptParams {
            detail: Detail::Auto,
            ..narrow.clone()
        },
    );
    assert!(level_of(&auto) > 2, "{r:?}");
    welded_and_on_the_lattice(&auto);
    // ... and a wide gentle ramp needs none.
    let mut gentle = base.clone();
    let r = stroke(
        &mut gentle,
        &[[4.0, 16.0], [28.0, 16.0]],
        SculptParams {
            mode: SculptMode::Ramp,
            radius: 12.0,
            falloff: Falloff::Linear,
            rise: 512,
            detail: Detail::Auto,
            ..SculptParams::default()
        },
    );
    assert!(r.moved > 0, "{r:?}");
    assert_eq!(level_of(&gentle), 2, "a gentle grade needs no finer cells");
    // Auto never creates more cells than Quarter does for the same stroke.
    let mut quarter = base.clone();
    press(
        &mut quarter,
        [16.0, 16.0],
        24,
        SculptParams {
            detail: Detail::Quarter,
            ..narrow
        },
    );
    assert!(auto.rooms[0].leaf_count() < quarter.rooms[0].leaf_count());
}

#[test]
fn a_column_of_storeys_stays_a_whole_sector_and_the_report_says_so() {
    let mut w = world(8, 8, true);
    {
        let s = w.rooms[0].sector_mut(4, 4).unwrap();
        s.insert_span(
            1,
            GridSpan {
                floor: Some(GridSurface::flat(7168, Some(rid(1)))),
                ceiling: Some(GridSurface::flat(10240, Some(rid(2)))),
                wall_material: None,
            },
        );
    }
    let r = press(
        &mut w,
        [18.0, 18.0],
        12,
        SculptParams {
            radius: 4.0,
            strength: 1.0,
            detail: Detail::Quarter,
            ..SculptParams::default()
        },
    );
    assert_eq!(r.storeys, 1, "{r:?}");
    let key = LeafKey::sector([4, 4]);
    assert!(w.rooms[0].leaf(key).is_some(), "the storey cell is whole");
    assert!(!w.rooms[0].leaf(key).unwrap().is_single_span());
    // The cells around it were divided, but only to halves where they touch
    // it (a quarter cell there would make balance divide the column).
    assert!(w.rooms[0].leaves().any(|(k, _)| k.level == 3));
    assert!(w.rooms[0].leaves().all(|(k, _)| k.level < 4));
    // Its ground floor was still sculpted, as a whole sector.
    let f = w.rooms[0].leaf(key).unwrap().ground_floor().unwrap();
    assert!(f.heights.iter().any(|h| *h > 0), "{:?}", f.heights);
    // The upper floor was not touched.
    let up = &w.rooms[0].leaf(key).unwrap().spans()[1];
    assert_eq!(up.floor.as_ref().unwrap().heights, [7168; 4]);
    assert!(tree::check(&w.rooms[0]).is_empty());
}

#[test]
fn a_floor_stays_a_body_under_its_ceiling_and_the_ceiling_can_be_sculpted_too() {
    let mut w = world(8, 8, true);
    let up = SculptParams {
        radius: 8.0,
        strength: 1.0,
        ..SculptParams::default()
    };
    let mut clamped = 0;
    for _ in 0..120 {
        // Each stroke starts from the document the last one left.
        clamped += stroke(&mut w, &[[16.0, 16.0]], up.clone()).clamped;
        if clamped > 0 {
            break;
        }
    }
    assert!(clamped > 0, "the headroom rule never held a corner back");
    let mut most = 0;
    for (_, s) in w.rooms[0].leaves() {
        for h in s.ground_floor().unwrap().heights {
            most = most.max(h);
        }
    }
    assert!(most <= 6144 - HEADROOM, "{most}");
    // The ceiling: dig it up toward the sky.
    let mut roof = world(8, 8, true);
    stroke(
        &mut roof,
        &[[16.0, 16.0], [17.0, 16.0]],
        SculptParams {
            target: Target::Ceiling,
            radius: 6.0,
            strength: 1.0,
            ..SculptParams::default()
        },
    );
    let top = roof.rooms[0]
        .leaves()
        .flat_map(|(_, s)| s.ground_ceiling().unwrap().heights)
        .max()
        .unwrap();
    assert!(top > 6144, "{top}");
    // The floor did not move.
    assert!(roof.rooms[0]
        .leaves()
        .all(|(_, s)| s.ground_floor().unwrap().heights == [0; 4]));
}

#[test]
fn rooms_and_cells_outside_the_ground_are_ignored() {
    // A stroke over empty space moves nothing, and a missing room is None.
    let mut w = world(4, 4, false);
    let r = stroke(&mut w, &[[200.0, 200.0]], SculptParams::default());
    assert_eq!(r.moved, 0);
    assert_eq!(r.vertices, 0);
    assert!(sculpt_path(&mut w, 3, &[[1.0, 1.0]], SculptParams::default()).is_none());
}

/// Review capture: `PSXED_SCULPT_PROJECT=<project.ron> PSXED_SCULPT_OUT=<dir>`
/// takes the biggest room of a project, levels its floor, and writes three
/// projects for the editor preview to render: `flat`, `dunes` (a dune field)
/// and `valley` (a winding valley with banks), plus a `numbers.txt` of the
/// face counts. Without the variables this does nothing.
#[test]
fn sculpt_captures() {
    let (Some(src), Some(out)) = (
        std::env::var_os("PSXED_SCULPT_PROJECT"),
        std::env::var_os("PSXED_SCULPT_OUT"),
    ) else {
        return;
    };
    let out = std::path::PathBuf::from(out);
    std::fs::create_dir_all(&out).unwrap();
    let mut project = crate::ProjectDocument::load_from_path(&src).expect("project loads");
    let (room, origin, size) = {
        let world = project.active_scene_mut().world.grid_mut().unwrap();
        let room = (0..world.rooms.len())
            .max_by_key(|&i| world.rooms[i].width * world.rooms[i].depth)
            .unwrap();
        let r = &mut world.rooms[room];
        // A bright room so the relief reads in the picture.
        r.ambient = [190, 184, 170];
        for z in 0..r.depth {
            for x in 0..r.width {
                if let Some(f) = r.sector_mut(x, z).and_then(|s| s.ground_floor_mut()) {
                    f.heights = [0; 4];
                    f.heights_b = None;
                    f.void = 0;
                }
            }
        }
        (room, r.origin, [r.width as i32, r.depth as i32])
    };
    let mut numbers = String::new();
    let faces = |p: &crate::ProjectDocument| {
        crate::grid::faces::room_face_count(p.active_scene().world.grid().unwrap(), room)
    };
    let flat = project.clone();
    numbers += &format!(
        "room {room} origin {origin:?} size {size:?} scale {:?}\n",
        flat.active_scene().world.grid().unwrap().scale
    );
    numbers += &format!("flat: faces {}\n", faces(&flat));
    // Click coordinates of a point `fx`, `fz` of the way across the room.
    let at = |fx: f64, fz: f64| {
        [
            f64::from(origin[0]) * 4.0 + fx * f64::from(size[0]) * 4.0,
            f64::from(origin[1]) * 4.0 + fz * f64::from(size[1]) * 4.0,
        ]
    };
    // Dunes: three long drags at a slant, Auto detail.
    let mut dunes = flat.clone();
    {
        let world = dunes.active_scene_mut().world.grid_mut().unwrap();
        for (a, b) in [
            ((0.15, 0.25), (0.85, 0.30)),
            ((0.15, 0.50), (0.85, 0.58)),
            ((0.15, 0.75), (0.85, 0.80)),
        ] {
            let r = sculpt_path(
                world,
                room,
                &[at(a.0, a.1), at(b.0, b.1)],
                SculptParams {
                    mode: SculptMode::Dunes,
                    radius: 14.0,
                    wavelength: 20.0,
                    dune_height: 2 * CLICK_UNITS,
                    falloff: Falloff::Linear,
                    detail: Detail::Auto,
                    ..SculptParams::default()
                },
            )
            .unwrap();
            numbers += &format!("dunes stroke: {r:?}\n");
        }
    }
    numbers += &format!("dunes: faces {}\n", faces(&dunes));
    dunes.save_to_path(out.join("dunes.ron")).unwrap();
    // Valley: dig a winding trench, bank it with two ridges, smooth it.
    let mut valley = flat.clone();
    {
        let world = valley.active_scene_mut().world.grid_mut().unwrap();
        let curve: Vec<[f64; 2]> = (0..=24)
            .map(|i| {
                let t = f64::from(i) / 24.0;
                at(
                    0.1 + 0.8 * t,
                    0.5 + 0.18 * (t * core::f64::consts::TAU).sin(),
                )
            })
            .collect();
        let dig = SculptParams {
            radius: 9.0,
            strength: 1.0,
            invert: true,
            detail: Detail::Auto,
            falloff: Falloff::Smooth,
            ..SculptParams::default()
        };
        for _ in 0..16 {
            let r = sculpt_path(world, room, &curve, dig.clone()).unwrap();
            numbers += &format!("valley dig: {r:?}\n");
        }
        for side in [-1.0, 1.0] {
            let bank: Vec<[f64; 2]> = curve.iter().map(|p| [p[0], p[1] + side * 17.0]).collect();
            for _ in 0..8 {
                sculpt_path(
                    world,
                    room,
                    &bank,
                    SculptParams {
                        invert: false,
                        radius: 8.0,
                        ..dig.clone()
                    },
                )
                .unwrap();
            }
        }
        let smooth = SculptParams {
            mode: SculptMode::Smooth,
            radius: 12.0,
            strength: 0.8,
            invert: false,
            ..dig
        };
        for _ in 0..3 {
            sculpt_path(world, room, &curve, smooth.clone()).unwrap();
        }
    }
    numbers += &format!("valley: faces {}\n", faces(&valley));
    // The click lattice heights of each state, for a plan picture.
    for (name, doc) in [("flat", &flat), ("dunes", &dunes), ("valley", &valley)] {
        let world = doc.active_scene().world.grid().unwrap();
        let r = &world.rooms[room];
        let mut csv = String::new();
        for z in 0..=size[1] * 4 {
            let row: Vec<String> = (0..=size[0] * 4)
                .map(|x| {
                    let v = [origin[0] * 4 + x, origin[1] * 4 + z];
                    vertex_height(r, Target::Floor, v).map_or("nan".into(), |h| format!("{h:.0}"))
                })
                .collect();
            csv += &row.join(",");
            csv.push('\n');
        }
        std::fs::write(out.join(format!("heights-{name}.csv")), csv).unwrap();
    }
    flat.save_to_path(out.join("flat.ron")).unwrap();
    valley.save_to_path(out.join("valley.ron")).unwrap();
    std::fs::write(out.join("numbers.txt"), numbers).unwrap();
}
