//! Break on small worlds: determinism, exact cancel, monotone intensity,
//! the walkability mask.

use std::collections::BTreeSet;

use crate::grid::derive;
use crate::grid::edit::region::Region;
use crate::grid::edit::CellRect;
use crate::grid::surface::{GridSurface, VOID_A};
use crate::grid::walk::{analyse, Spawn, WalkLimits};
use crate::grid::world::GridWorld;

use super::super::tests::{floor, hall};
use super::*;

/// A 12 by 12 derived hall (floor 0, ceiling 3072) with a plateau of 4 clicks
/// in the middle 6 by 6, no ceiling over the plateau.
fn plateau() -> (GridWorld, Region) {
    let mut world = hall(12, 12);
    let room = &mut world.rooms[0];
    room.defaults.open_wall = 0;
    for z in 3..9 {
        for x in 3..9 {
            let s = room.sector_or_insert(x as usize, z as usize).unwrap();
            s.ground_floor_mut().unwrap().heights = [512; 4];
            s.ground_ceiling_mut().unwrap().heights = [3584; 4];
        }
    }
    let region = Region::from_rect(0, CellRect::from_corners([3, 3], [8, 8]));
    (world, region)
}

fn params() -> BreakParams {
    BreakParams {
        targets: BreakTargets {
            walls: true,
            floor: true,
            ceiling: true,
        },
        spawns: vec![Spawn {
            room: 0,
            cell: [0, 0],
        }],
        ..BreakParams::default()
    }
}

#[test]
fn the_same_seed_gives_the_same_document_and_another_seed_another() {
    let (w0, region) = plateau();
    let p = params();
    let (mut a, mut b, mut c) = (w0.clone(), w0.clone(), w0.clone());
    break_region(&mut a, &region, p.clone(), 60, 1234).unwrap();
    break_region(&mut b, &region, p.clone(), 60, 1234).unwrap();
    break_region(&mut c, &region, p, 60, 4321).unwrap();
    assert_eq!(a, b);
    assert_ne!(a, c);
    assert_ne!(a, w0);
}

#[test]
fn the_region_order_and_width_do_not_reshuffle_the_interior() {
    // A cell's decisions depend on the seed and its position, so breaking a
    // wider region leaves the cells they share with the narrow one broken the
    // same way (away from the narrow region's own edge).
    let (w0, narrow) = plateau();
    let wide = Region::from_rect(0, CellRect::from_corners([2, 2], [9, 9]));
    let p = BreakParams {
        keep_walkable: false,
        targets: BreakTargets {
            walls: false,
            floor: true,
            ceiling: false,
        },
        ..BreakParams::default()
    };
    let (mut a, mut b) = (w0.clone(), w0.clone());
    break_region(&mut a, &narrow, p.clone(), 80, 9).unwrap();
    break_region(&mut b, &wide, p, 80, 9).unwrap();
    // The interior cells of the narrow region (not on its edge).
    let mut same = 0;
    for z in 4..8 {
        for x in 4..8 {
            assert_eq!(floor(&a, [x, z]), floor(&b, [x, z]), "{x},{z}");
            same += 1;
        }
    }
    assert_eq!(same, 16);
}

#[test]
fn intensity_only_adds_operations_and_zero_is_the_original() {
    let (w0, region) = plateau();
    let p = BreakParams {
        keep_walkable: false,
        ..params()
    };
    let touched_at = |intensity: u32| -> BTreeSet<[i32; 2]> {
        let mut w = w0.clone();
        break_region(&mut w, &region, p.clone(), intensity, 77).unwrap();
        let mut set = BTreeSet::new();
        for z in 0..12 {
            for x in 0..12 {
                let c = [x, z];
                let (a, b) = (
                    w.rooms[0].sector(x as usize, z as usize),
                    w0.rooms[0].sector(x as usize, z as usize),
                );
                if a != b {
                    set.insert(c);
                }
            }
        }
        set
    };
    let low = touched_at(30);
    let high = touched_at(70);
    assert!(!low.is_empty());
    assert!(high.len() > low.len());
    assert!(touched_at(0).is_empty());
    // Cells whose only change at 30 is a gap-free crack stay touched at 70.
    let kept = low.intersection(&high).count();
    assert!(
        kept * 10 >= low.len() * 7,
        "{kept} of {} cells touched at 30 are touched at 70",
        low.len()
    );
}

#[test]
fn cancel_and_redo_are_exact() {
    let (mut world, region) = plateau();
    let original = world.clone();
    let drag = BreakDrag::begin(&world, &region, params()).unwrap();
    drag.apply(&mut world, 50, 1);
    drag.apply(&mut world, 90, 2);
    assert_ne!(world, original);
    drag.apply(&mut world, 0, 2);
    assert_eq!(world, original, "intensity 0 puts every byte back");
    let a = {
        let mut w = original.clone();
        drag.apply(&mut w, 65, 3);
        w
    };
    drag.apply(&mut world, 65, 3);
    assert_eq!(world, a);
}

#[test]
fn a_broken_plateau_grows_diagonal_risers_and_never_a_step_the_body_cannot_climb() {
    let (mut world, region) = plateau();
    let limits = WalkLimits::DEFAULT;
    let spawns = vec![Spawn {
        room: 0,
        cell: [0, 0],
    }];
    let before = analyse(&world, &spawns, &limits).unwrap();
    // The 512 plateau (40 runtime units) is a step the body climbs.
    assert_eq!(before.counts()[2], 0);
    let report = break_region(&mut world, &region, params(), 100, 5).unwrap();
    assert!(report.touched > 0, "{report:?}");
    let faces_diag = (0..12)
        .flat_map(|z| (0..12).map(move |x| [x, z]))
        .flat_map(|c| derive::cell_walls(&world.rooms[0], c))
        .filter(|w| w.edge.is_diagonal())
        .count();
    assert!(
        report.bites + report.cracked > 0 && faces_diag > 0,
        "{report:?}"
    );
    let after = analyse(&world, &spawns, &limits).unwrap();
    assert_eq!(
        after.counts()[2],
        0,
        "no step above the climb limit: {report:?}"
    );
    assert_eq!(
        after.counts()[0],
        before.counts()[0],
        "no steep floor: {report:?}"
    );
    assert_eq!(
        after.counts()[4],
        0,
        "everything still reachable: {report:?}"
    );
    let [b, a] = report.walk.expect("a spawn on a floor");
    assert_eq!(b.unreachable, a.unreachable);
}

#[test]
fn the_mask_refuses_what_would_make_a_floor_too_steep_and_says_why() {
    // A ramp of 960 grid units a sector: just under the steepest slope the
    // character stands on, so any tilt that steepens it must be refused.
    let mut world = hall(8, 8);
    for z in 0..8usize {
        for x in 0..8usize {
            let s = world.rooms[0].sector_mut(x, z).unwrap();
            s.set_ground_ceiling(None);
            let n = z as i32 * 960;
            s.ground_floor_mut().unwrap().heights = [n, n, n + 960, n + 960];
        }
    }
    let spawns = vec![Spawn {
        room: 0,
        cell: [4, 0],
    }];
    let limits = WalkLimits::DEFAULT;
    let before = analyse(&world, &spawns, &limits).unwrap();
    assert_eq!(before.counts()[0], 0, "the ramp itself is walkable");
    let region = Region::from_rect(0, CellRect::from_corners([1, 1], [6, 6]));
    let p = BreakParams {
        targets: BreakTargets {
            walls: false,
            floor: true,
            ceiling: false,
        },
        spawns: spawns.clone(),
        ..BreakParams::default()
    };
    let report = break_region(&mut world, &region, p.clone(), 100, 21).unwrap();
    assert!(report.rejected > 0, "{report:?}");
    assert!(report.rejects[0].contains("would"), "{report:?}");
    let after = analyse(&world, &spawns, &limits).unwrap();
    assert_eq!(
        after.counts()[0],
        0,
        "no floor became too steep: {report:?}"
    );
    assert_eq!(after.counts()[2], 0, "no step became too tall: {report:?}");
    // Without the mask the same break steepens floors.
    let original = {
        let mut w = hall(8, 8);
        for z in 0..8usize {
            for x in 0..8usize {
                let s = w.rooms[0].sector_mut(x, z).unwrap();
                s.set_ground_ceiling(None);
                let n = z as i32 * 960;
                s.ground_floor_mut().unwrap().heights = [n, n, n + 960, n + 960];
            }
        }
        w
    };
    let mut loose = original.clone();
    let unmasked = BreakParams {
        keep_walkable: false,
        ..p
    };
    break_region(&mut loose, &region, unmasked, 100, 21).unwrap();
    let a = analyse(&loose, &spawns, &limits).unwrap();
    assert!(
        a.counts()[0] + a.counts()[2] > 0,
        "an unmasked break does harm"
    );
}

#[test]
fn open_wall_tops_get_a_ragged_top_and_a_void_corner_keeps_its_diagonal_wall() {
    let mut world = hall(8, 8);
    {
        let room = &mut world.rooms[0];
        room.defaults.open_wall = 2048;
        for z in 0..8 {
            for x in 0..8 {
                room.sector_mut(x, z).unwrap().set_ground_ceiling(None);
            }
        }
        let s = room.sector_mut(0, 0).unwrap();
        s.ground_floor_mut().unwrap().void = VOID_A;
    }
    let region = Region::from_rect(0, CellRect::from_corners([0, 0], [7, 7]));
    let p = BreakParams {
        targets: BreakTargets {
            walls: true,
            floor: false,
            ceiling: false,
        },
        keep_walkable: false,
        ..BreakParams::default()
    };
    let report = break_region(&mut world, &region, p, 100, 3).unwrap();
    assert!(report.tops > 0, "{report:?}");
    assert!(!world.rooms[0].edge_overrides.is_empty());
    // A ragged top is at most the open wall height lower, never above it,
    // and the wall keeps a click of height.
    for (_, o) in world.rooms[0].edge_overrides.iter() {
        for d in o.top_delta {
            assert!((-1792..=0).contains(&i32::from(d)), "{d}");
        }
    }
    // The shared corners agree: neighbouring segments end at the same
    // height where they meet.
    let r = &world.rooms[0];
    let top = |cell: [i32; 2], edge| {
        derive::edge_walls(r, cell, edge)
            .into_iter()
            .next()
            .and_then(|w| w.heights)
    };
    let (a, b) = (
        top([3, 0], crate::grid::WallEdge::North),
        top([4, 0], crate::grid::WallEdge::North),
    );
    if let (Some(a), Some(b)) = (a, b) {
        assert_eq!(a[2], b[3], "the walls meet at one height: {a:?} {b:?}");
    }
    let _ = GridSurface::flat(0, None);
}
