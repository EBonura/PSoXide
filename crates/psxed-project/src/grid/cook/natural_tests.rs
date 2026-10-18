//! Break against the Cathedral: the hand-made reference for the look.
//!
//! The Cathedral's author made its broken tiles and 45 degree plateau edges
//! by hand: seven split-level cells with a diagonal riser of one or two
//! clicks, never a step above what the character climbs. The generator is
//! held to the same vocabulary, not to the same cells: a histogram of the
//! primitives (split-level cells and their offsets, diagonal risers and their
//! heights, bent tiles) over the whole level, and no step above the climb
//! limit and no unreachable cell the original did not have.

use std::collections::BTreeMap;

use psx_engine::WalkLimits;

use super::tests::load_reference;
use crate::grid::derive::{cell_walls, DerivedKind};
use crate::grid::edit::region::Region;
use crate::grid::edit::verbs::break_verb::{break_region, BreakParams, BreakTargets};
use crate::grid::edit::{convert_to_derived, CellRect, ConvertMode};
use crate::grid::walk::{analyse, player_spawns};
use crate::grid::world::GridWorld;

/// The primitives of a level's floors, as counts and the offsets they use.
#[derive(Debug, Default)]
struct Census {
    floors: usize,
    /// Split-level floors, by the offset between their triangles (gu).
    cracks: BTreeMap<i32, usize>,
    /// Non-planar floors.
    bent: usize,
    /// Derived diagonal risers by height in runtime units.
    diagonal_risers: BTreeMap<i32, usize>,
}

fn census(world: &GridWorld) -> Census {
    let mut c = Census::default();
    let units = super::RuntimeUnits::new(world.scale).unwrap();
    for room in &world.rooms {
        for (x, z, s) in room.iter() {
            let Some(f) = s.floor() else { continue };
            c.floors += 1;
            c.bent += usize::from(!f.is_planar());
            if let Some(b) = f.heights_b {
                let offset = (0..4)
                    .map(|i| (f.heights[i] - b[i]).abs())
                    .max()
                    .unwrap_or(0);
                *c.cracks.entry(offset).or_default() += 1;
            }
            for w in cell_walls(room, room.world_cell(x, z)) {
                if w.edge.is_diagonal() && w.kind == DerivedKind::Riser {
                    if let Some(h) = w.heights {
                        let ru = units.rt(h[2].max(h[3])) - units.rt(h[0].min(h[1]));
                        *c.diagonal_risers.entry(ru).or_default() += 1;
                    }
                }
            }
        }
    }
    c
}

#[test]
fn the_cathedrals_hand_made_look_is_what_break_is_measured_against() {
    let project = load_reference("grid-cathedral").unwrap();
    let mut world = project.active_scene().world.grid().unwrap().clone();
    for room in 0..world.rooms.len() {
        convert_to_derived(&mut world, room, ConvertMode::PreserveLook).unwrap();
    }
    let hand = census(&world);
    eprintln!("hand made: {hand:?}");
    // The reference: seven split-level cells, offsets of a click or two.
    assert_eq!(hand.cracks.values().sum::<usize>(), 7);
    assert!(
        hand.cracks.keys().all(|o| (256..=512).contains(o)),
        "{hand:?}"
    );
    let limits = WalkLimits::DEFAULT;
    let spawns = player_spawns(&project, &world);
    let before = analyse(&world, &spawns, &limits).unwrap();

    // Break the whole level, floors and plateau edges, at a modest intensity.
    let r = &world.rooms[0];
    let region = Region::from_rect(
        0,
        CellRect::from_corners(
            r.origin,
            [
                r.origin[0] + r.width as i32 - 1,
                r.origin[1] + r.depth as i32 - 1,
            ],
        ),
    );
    let params = BreakParams {
        targets: BreakTargets {
            walls: true,
            floor: true,
            ceiling: false,
        },
        spawns: spawns.clone(),
        ..BreakParams::default()
    };
    let mut broken = world.clone();
    let report = break_region(&mut broken, &region, params, 40, 2026).unwrap();
    eprintln!("{report:?}");
    let made = census(&broken);
    eprintln!("generated: {made:?}");
    // The same primitives: cracked tiles whose halves differ by a click or
    // less at this amplitude, and diagonal risers, none above the step.
    assert!(made.cracks.values().sum::<usize>() > hand.cracks.values().sum::<usize>());
    assert!(
        made.diagonal_risers.keys().all(|ru| *ru <= limits.step_up),
        "a diagonal riser above the climb limit: {:?}",
        made.diagonal_risers
    );
    // The level keeps its route: nothing new unreachable, no step the body
    // cannot climb that the original did not have, no new steep floor.
    let after = analyse(&broken, &spawns, &limits).unwrap();
    let (b, a) = (before.counts(), after.counts());
    assert!(a[4] <= b[4], "unreachable {} -> {}", b[4], a[4]);
    assert!(a[2] <= b[2], "steps {} -> {}", b[2], a[2]);
    assert!(a[0] <= b[0], "steep {} -> {}", b[0], a[0]);
    // At full intensity the tiles drop by up to a click, the Cathedral's own
    // size (its two-click cracks are beyond the default amplitude), and the
    // route still holds.
    let mut full = world.clone();
    let params = BreakParams {
        spawns: spawns.clone(),
        ..BreakParams::default()
    };
    break_region(&mut full, &region, params, 100, 2026).unwrap();
    let big = census(&full);
    eprintln!("generated at 100: {big:?}");
    let new_cracks: Vec<i32> = big
        .cracks
        .keys()
        .copied()
        .filter(|o| *o < 256 || !hand.cracks.contains_key(o))
        .collect();
    assert!(
        new_cracks.iter().all(|o| (64..=256).contains(o)),
        "{new_cracks:?}"
    );
    assert!(big.cracks.keys().any(|o| *o > 64), "{big:?}");
    assert!(big.diagonal_risers.keys().all(|ru| *ru <= limits.step_up));
    let after = analyse(&full, &spawns, &limits).unwrap().counts();
    assert!(
        after[4] <= b[4] && after[2] <= b[2] && after[0] <= b[0],
        "{after:?} vs {b:?}"
    );
    // And the same seed always gives the same level.
    let mut again = world.clone();
    let params = BreakParams {
        spawns,
        ..BreakParams::default()
    };
    let region2 = region.clone();
    break_region(&mut again, &region2, params.clone(), 40, 2026).unwrap();
    let mut third = world.clone();
    break_region(&mut third, &region2, params, 40, 2026).unwrap();
    assert_eq!(again, third);
}
