//! Break on small worlds: determinism, exact cancel, monotone intensity,
//! the walkability mask.

use std::collections::BTreeSet;

use crate::grid::derive;
use crate::grid::edit::region::Region;
use crate::grid::edit::CellRect;
use crate::grid::surface::{GridSurface, VOID_A};
use crate::grid::walk::{analyse, Spawn, WalkLimits};
use crate::grid::world::GridWorld;

use super::super::tests::{floor, hall, rid};
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

/// A 16 by 16 hall with a textured floor (material 1) and ceiling
/// (material 2), the way a real room is painted.
fn painted_hall() -> (GridWorld, Region) {
    let world = hall(16, 16);
    let region = Region::from_rect(0, CellRect::from_corners([1, 1], [14, 14]));
    (world, region)
}

fn voids(world: &GridWorld) -> Vec<[usize; 2]> {
    let room = &world.rooms[0];
    let mut out = Vec::new();
    for z in 0..room.depth {
        for x in 0..room.width {
            let Some(s) = room.sector(x, z) else { continue };
            let floor = s.ground_floor().is_some_and(|f| f.void != 0);
            let ceiling = s.ground_ceiling().is_some_and(|c| c.void != 0);
            if floor || ceiling {
                out.push([x, z]);
            }
        }
    }
    out
}

#[test]
fn a_break_does_not_punch_holes_in_a_floor_unless_asked_to() {
    // Manny's dp-world floor: a break at high intensity left dozens of
    // see-through squares because gaps were on by default and a gap in the
    // middle of a big floor does not cut the route, so the mask let it be.
    let (w0, region) = painted_hall();
    let spawns = vec![Spawn {
        room: 0,
        cell: [0, 0],
    }];
    for seed in 1..=8 {
        for keep_walkable in [true, false] {
            let p = BreakParams {
                targets: BreakTargets {
                    walls: true,
                    floor: true,
                    ceiling: true,
                },
                keep_walkable,
                spawns: spawns.clone(),
                ..BreakParams::default()
            };
            let mut w = w0.clone();
            let report = break_region(&mut w, &region, p, 100, seed).unwrap();
            assert_eq!(report.gaps, 0, "seed {seed} keep {keep_walkable}");
            assert!(
                voids(&w).is_empty(),
                "seed {seed} keep {keep_walkable}: {:?}",
                voids(&w)
            );
        }
    }
}

#[test]
fn gaps_are_an_explicit_option_and_still_obey_the_mask() {
    let (w0, region) = painted_hall();
    let p = BreakParams {
        targets: BreakTargets {
            walls: false,
            floor: true,
            ceiling: false,
        },
        keep_walkable: false,
        gaps: true,
        ..BreakParams::default()
    };
    let mut w = w0.clone();
    let report = break_region(&mut w, &region, p.clone(), 100, 1).unwrap();
    assert!(report.gaps > 0, "{report:?}");
    assert_eq!(voids(&w).len(), report.gaps);
    // Below the gap threshold nothing opens even when asked.
    let mut low = w0;
    let report = break_region(&mut low, &region, p, 50, 1).unwrap();
    assert_eq!(report.gaps, 0);
}

#[test]
fn every_face_a_break_makes_keeps_the_material_it_came_from() {
    // The floor is material 1 and the ceiling 2, no paint rules. Whatever a
    // break splits, cracks or drops, no triangle may end up with another
    // material or none, and every derived riser must still resolve to one.
    let (w0, region) = painted_hall();
    let p = BreakParams {
        targets: BreakTargets {
            walls: true,
            floor: true,
            ceiling: true,
        },
        keep_walkable: false,
        ..BreakParams::default()
    };
    for seed in 1..=6 {
        let mut w = w0.clone();
        let report = break_region(&mut w, &region, p.clone(), 100, seed).unwrap();
        assert!(
            report.cracked + report.tilted + report.ceiling > 0,
            "{report:?}"
        );
        for face in crate::grid::faces::all_faces(&w) {
            assert!(
                face.material.is_some(),
                "seed {seed}: an untextured {:?} at {:?}",
                face.kind,
                face.cell
            );
            match face.kind {
                crate::grid::faces::GridFaceKind::Floor { .. } => {
                    assert_eq!(face.material, Some(rid(1)), "seed {seed} {:?}", face.cell)
                }
                crate::grid::faces::GridFaceKind::Ceiling { .. } => {
                    assert_eq!(face.material, Some(rid(2)), "seed {seed} {:?}", face.cell)
                }
                _ => {}
            }
        }
    }
}

#[test]
fn mixing_materials_never_swaps_in_nothing() {
    // The neighbour a cracked half borrows from has no material: the half
    // keeps its own rather than turning untextured.
    let (mut w0, region) = painted_hall();
    // A plateau so the break has edges to bite; the low ring is unpainted.
    for z in 0..16usize {
        for x in 0..16usize {
            let s = w0.rooms[0].sector_mut(x, z).unwrap();
            let f = s.ground_floor_mut().unwrap();
            if (4..12).contains(&x) && (4..12).contains(&z) {
                f.heights = [512; 4];
            } else {
                f.tri_a.material = None;
            }
        }
    }
    let p = BreakParams {
        targets: BreakTargets {
            walls: true,
            floor: false,
            ceiling: false,
        },
        keep_walkable: false,
        mix_materials: true,
        ..BreakParams::default()
    };
    let mut cracked = 0;
    for seed in 1..=8 {
        let mut w = w0.clone();
        let report = break_region(&mut w, &region, p.clone(), 100, seed).unwrap();
        cracked += report.bites;
        for z in 4..12 {
            for x in 4..12 {
                let f = floor(&w, [x, z]);
                assert_eq!(f.tri_a.material, Some(rid(1)), "seed {seed} {x},{z}");
                if let Some(b) = f.tri_b {
                    assert_eq!(b.material, Some(rid(1)), "seed {seed} {x},{z}");
                }
            }
        }
    }
    assert!(cracked > 0, "the break bit nothing");
}

/// Review capture: `PSXED_BREAK_PROJECT=<project.ron> PSXED_BREAK_OUT=<dir>`
/// takes a project whose floors a break already pierced with gaps (Manny's
/// dp-world), closes those gaps, breaks every room that had them again at
/// intensity 100 with and without `gaps`, and writes the two projects for
/// the editor preview to render. Without the variables this does nothing.
#[test]
fn break_captures() {
    let (Some(src), Some(out)) = (
        std::env::var_os("PSXED_BREAK_PROJECT"),
        std::env::var_os("PSXED_BREAK_OUT"),
    ) else {
        return;
    };
    let out = std::path::PathBuf::from(out);
    let mut project = crate::ProjectDocument::load_from_path(&src).expect("project loads");
    let spawns;
    let mut rooms: Vec<usize> = Vec::new();
    {
        let scene = project.active_scene_mut();
        let world = scene.world.grid_mut().expect("a grid world");
        for (index, room) in world.rooms.iter_mut().enumerate() {
            let mut pierced = false;
            for z in 0..room.depth {
                for x in 0..room.width {
                    let Some(s) = room.sector_mut(x, z) else {
                        continue;
                    };
                    if let Some(f) = s.ground_floor_mut().filter(|f| f.void == 3) {
                        f.void = 0;
                        pierced = true;
                    }
                    if let Some(c) = s.ground_ceiling_mut().filter(|c| c.void == 3) {
                        c.void = 0;
                    }
                }
            }
            if pierced {
                rooms.push(index);
            }
        }
    }
    {
        let scene = project.active_scene();
        let world = scene.world.grid().unwrap();
        spawns = crate::grid::walk::player_spawns(&project, world);
    }
    assert!(!rooms.is_empty(), "no room had a gap");
    for gaps in [true, false] {
        let mut p = project.clone();
        let world = p.active_scene_mut().world.grid_mut().unwrap();
        for &room in &rooms {
            let r = &world.rooms[room];
            let cells: Vec<[i32; 2]> = r
                .face_cells()
                .into_iter()
                .map(|[x, z]| r.world_cell(x, z))
                .collect();
            let region = Region::from_cells(room, cells);
            let params = BreakParams {
                targets: BreakTargets {
                    walls: true,
                    floor: true,
                    ceiling: false,
                },
                spawns: spawns.clone(),
                gaps,
                ..BreakParams::default()
            };
            let report = break_region(world, &region, params, 100, 1).expect("something to break");
            eprintln!("room {room} gaps {gaps}: {report:?}");
        }
        let dir = out.join(if gaps { "gaps-on" } else { "gaps-off" });
        std::fs::create_dir_all(&dir).unwrap();
        p.save_to_path(dir.join("project.ron")).unwrap();
    }
}
