//! Tests of the level verbs on small worlds.

use crate::grid::edit::region::Region;
use crate::grid::edit::CellRect;
use crate::grid::faces::all_faces;
use crate::grid::overrides::WallMode;
use crate::grid::paint_rules::PaintRules;
use crate::grid::room::GridRoom;
use crate::grid::slope::SlopeClass;
use crate::grid::surface::{GridSplit, GridSurface};
use crate::grid::units::{GridScale, CLICK_UNITS};
use crate::grid::world::GridWorld;
use crate::ResourceId;

use super::raise::{default_mode, raise_region, RaiseDrag, RaiseMode, RaiseParams};

pub(super) fn rid(n: u64) -> ResourceId {
    ron::from_str(&format!("({n})")).expect("a resource id")
}

/// A derived room of `w` by `d` sectors, floors at 0 and ceilings at 3072,
/// at the default scale (80 runtime units per sector).
pub(super) fn hall(w: usize, d: usize) -> GridWorld {
    let mut room = GridRoom::new_derived("hall", w, d);
    for z in 0..d {
        for x in 0..w {
            let sector = room.sector_or_insert(x, z).unwrap();
            sector.floor = Some(GridSurface::flat_auto(0, Some(rid(1))));
            sector.ceiling = Some(GridSurface::flat_auto(3072, Some(rid(2))));
        }
    }
    GridWorld {
        scale: GridScale::DEFAULT,
        rooms: vec![room],
        ..GridWorld::default()
    }
}

pub(super) fn params(mode: RaiseMode) -> RaiseParams {
    RaiseParams {
        mode,
        follow_ceiling: true,
        wall_material: None,
        reference: 0,
    }
}

pub(super) fn floor(world: &GridWorld, cell: [i32; 2]) -> GridSurface {
    let room = &world.rooms[0];
    let [x, z] = room.local(cell).unwrap();
    room.sector(x, z).unwrap().floor.clone().unwrap()
}

pub(super) fn ceiling(world: &GridWorld, cell: [i32; 2]) -> GridSurface {
    let room = &world.rooms[0];
    let [x, z] = room.local(cell).unwrap();
    room.sector(x, z).unwrap().ceiling.clone().unwrap()
}

fn square(a: [i32; 2], b: [i32; 2]) -> Region {
    Region::from_rect(0, CellRect::from_corners(a, b))
}

#[test]
fn raising_a_platform_moves_its_corners_and_derives_its_sides() {
    let mut world = hall(6, 6);
    let before = all_faces(&world).len();
    let mut p = params(RaiseMode::Offset);
    p.follow_ceiling = false;
    let applied = raise_region(&mut world, &square([2, 2], [3, 3]), p, 2 * CLICK_UNITS).unwrap();
    assert_eq!(applied.moved, 4);
    assert_eq!(floor(&world, [2, 2]).heights, [512; 4]);
    assert_eq!(
        floor(&world, [1, 2]).heights,
        [0; 4],
        "outside cells keep theirs"
    );
    // Eight risers around a 2 by 2 platform: nothing was welded outward.
    assert_eq!(all_faces(&world).len(), before + 8);
}

#[test]
fn the_ceiling_follows_by_the_same_delta_or_the_floor_is_held() {
    let mut world = hall(4, 4);
    raise_region(
        &mut world,
        &square([1, 1], [1, 1]),
        params(RaiseMode::Offset),
        512,
    )
    .unwrap();
    assert_eq!(ceiling(&world, [1, 1]).heights, [3584; 4]);
    assert_eq!(ceiling(&world, [0, 0]).heights, [3072; 4]);
    // Without follow a floor stops one click under the ceiling.
    let mut world = hall(4, 4);
    let mut p = params(RaiseMode::Offset);
    p.follow_ceiling = false;
    let applied = raise_region(&mut world, &square([1, 1], [1, 1]), p, 4096).unwrap();
    assert_eq!(floor(&world, [1, 1]).heights, [3072 - CLICK_UNITS; 4]);
    assert_eq!(applied.clamped, 1);
    assert_eq!(ceiling(&world, [1, 1]).heights, [3072; 4]);
}

#[test]
fn level_mode_flattens_and_offset_keeps_the_shape() {
    let mut world = hall(4, 4);
    {
        let room = &mut world.rooms[0];
        room.sector_mut(1, 1)
            .unwrap()
            .floor
            .as_mut()
            .unwrap()
            .heights = [0, 256, 512, 256];
        room.sector_mut(2, 1)
            .unwrap()
            .floor
            .as_mut()
            .unwrap()
            .heights = [256; 4];
    }
    let region = square([1, 1], [2, 1]);
    assert_eq!(default_mode(&world, &region), RaiseMode::Offset);
    let mut level = world.clone();
    let mut p = params(RaiseMode::Level);
    p.reference = 512;
    raise_region(&mut level, &region, p, 256).unwrap();
    assert_eq!(floor(&level, [1, 1]).heights, [768; 4]);
    assert_eq!(floor(&level, [2, 1]).heights, [768; 4]);
    let mut offset = world.clone();
    raise_region(&mut offset, &region, params(RaiseMode::Offset), 256).unwrap();
    assert_eq!(floor(&offset, [1, 1]).heights, [256, 512, 768, 512]);
    assert_eq!(floor(&offset, [2, 1]).heights, [512; 4]);
    // A flat region at one height defaults to Level.
    assert_eq!(
        default_mode(&hall(3, 3), &square([0, 0], [2, 2])),
        RaiseMode::Level
    );
}

#[test]
fn a_drag_is_absolute_apply_zero_restores_the_document_byte_for_byte() {
    let mut world = hall(6, 6);
    world.rooms[0]
        .sector_mut(2, 2)
        .unwrap()
        .floor
        .as_mut()
        .unwrap()
        .heights = [0, 0, 256, 0];
    let original = world.clone();
    let region = square([1, 1], [3, 3]);
    let drag = RaiseDrag::begin(&world, &region, params(RaiseMode::Offset)).unwrap();
    let a = drag.apply(&mut world, 512);
    let b = drag.apply(&mut world, -256);
    assert_ne!(world, original);
    assert!(a.moved > 0 && b.moved > 0);
    drag.apply(&mut world, 0);
    assert_eq!(world, original, "apply 0 puts everything back");
    // The same amount twice gives the same document.
    drag.apply(&mut world, 512);
    let once = world.clone();
    drag.apply(&mut world, 512);
    assert_eq!(world, once);
    // And restoring then re-applying from the snapshot equals a fresh run.
    let mut fresh = original.clone();
    raise_region(&mut fresh, &region, params(RaiseMode::Offset), 512).unwrap();
    assert_eq!(fresh, once);
}

#[test]
fn sectors_without_a_floor_are_skipped_and_counted() {
    let mut world = hall(4, 4);
    world.rooms[0].sector_mut(1, 1).unwrap().floor = None;
    let applied = raise_region(
        &mut world,
        &square([0, 0], [1, 1]),
        params(RaiseMode::Offset),
        256,
    )
    .unwrap();
    assert_eq!((applied.moved, applied.skipped), (3, 1));
    // A region with no floor at all has nothing to raise.
    assert!(raise_region(
        &mut world,
        &square([9, 9], [9, 9]),
        params(RaiseMode::Offset),
        256
    )
    .is_none());
}

#[test]
fn auto_split_surfaces_follow_their_heights_and_legacy_ones_do_not() {
    let mut world = hall(3, 3);
    world.rooms[0]
        .sector_mut(1, 1)
        .unwrap()
        .floor
        .as_mut()
        .unwrap()
        .auto_split = false;
    // Raise one corner's worth: a ramp over the 1x1 region tilts it, which
    // is planar and keeps the split, so build a bump with a two cell step.
    let region = square([0, 0], [0, 0]);
    let drag = RaiseDrag::begin(&world, &region, params(RaiseMode::Offset)).unwrap();
    drag.apply(&mut world, 256);
    assert_eq!(
        floor(&world, [0, 0]).split,
        GridSplit::NwSe,
        "flat keeps it"
    );
    // Direct check of the surface rule through a bent cell.
    let mut bent = hall(1, 1);
    {
        let f = bent.rooms[0]
            .sector_mut(0, 0)
            .unwrap()
            .floor
            .as_mut()
            .unwrap();
        f.heights = [0, 0, 256, 0];
        f.split = GridSplit::NwSe;
    }
    let drag = RaiseDrag::begin(&bent, &square([0, 0], [0, 0]), params(RaiseMode::Offset)).unwrap();
    drag.apply(&mut bent, 256);
    assert_eq!(
        floor(&bent, [0, 0]).split,
        GridSplit::NeSw,
        "auto surface re-splits"
    );
    drag.apply(&mut bent, 0);
    assert_eq!(floor(&bent, [0, 0]).split, GridSplit::NwSe, "and goes back");
    // The same bent cell as a legacy surface keeps its diagonal.
    bent.rooms[0]
        .sector_mut(0, 0)
        .unwrap()
        .floor
        .as_mut()
        .unwrap()
        .auto_split = false;
    let drag = RaiseDrag::begin(&bent, &square([0, 0], [0, 0]), params(RaiseMode::Offset)).unwrap();
    drag.apply(&mut bent, 256);
    assert_eq!(floor(&bent, [0, 0]).split, GridSplit::NwSe);
}

#[test]
fn rules_paint_a_floor_whose_class_changed_unless_it_was_painted_by_hand() {
    let mut world = hall(4, 1);
    let (flat_mat, slope_mat, wall_mat) = (rid(11), rid(12), rid(13));
    world.rooms[0].defaults.rules = PaintRules {
        floor_by_class: [Some(flat_mat), Some(slope_mat), None],
        trim: None,
        wall: Some(wall_mat),
        ..PaintRules::default()
    };
    world.rooms[0]
        .sector_mut(1, 0)
        .unwrap()
        .floor
        .as_mut()
        .unwrap()
        .paint_locked = true;
    // Level the three cells to 512 at a reference, then tilt cell 0 by
    // lowering only part of it through two applies on different regions.
    let tilt = |world: &mut GridWorld| {
        for x in [0usize, 1] {
            world.rooms[0]
                .sector_mut(x, 0)
                .unwrap()
                .floor
                .as_mut()
                .unwrap()
                .heights = [0, 0, 256, 256];
        }
    };
    tilt(&mut world);
    // Offset keeps the class (slope stays slope): the rule does not run.
    raise_region(
        &mut world,
        &square([0, 0], [1, 0]),
        params(RaiseMode::Offset),
        256,
    )
    .unwrap();
    assert_eq!(floor(&world, [0, 0]).tri_a.material, Some(rid(1)));
    // Level flattens them: class slope to flat. Cell 0 is repainted, the
    // hand painted cell 1 is not.
    let mut p = params(RaiseMode::Level);
    p.reference = 512;
    raise_region(&mut world, &square([0, 0], [1, 0]), p, 0).unwrap();
    assert_eq!(floor(&world, [0, 0]).tri_a.material, Some(flat_mat));
    assert_eq!(floor(&world, [1, 0]).tri_a.material, Some(rid(1)));
    // Every shaped sector got the rule's wall material.
    let room = &world.rooms[0];
    assert_eq!(room.sector(0, 0).unwrap().wall_material, Some(wall_mat));
    assert_eq!(
        room.sector(2, 0).unwrap().wall_material,
        None,
        "outside the region"
    );
    let _ = SlopeClass::Flat;
}

#[test]
fn the_palette_material_beats_the_wall_rule_and_trim_makes_one_default_set() {
    let mut world = hall(3, 3);
    world.rooms[0].defaults.rules = PaintRules {
        wall: Some(rid(13)),
        trim: Some(rid(14)),
        ..PaintRules::default()
    };
    let mut p = params(RaiseMode::Offset);
    p.wall_material = Some(rid(20));
    let drag = RaiseDrag::begin(&world, &square([1, 1], [1, 1]), p).unwrap();
    drag.apply(&mut world, 0);
    assert!(
        world.rooms[0].defaults.bands.is_none(),
        "no change, no trim"
    );
    drag.apply(&mut world, 256);
    drag.apply(&mut world, 512);
    let room = &world.rooms[0];
    assert_eq!(room.sector(1, 1).unwrap().wall_material, Some(rid(20)));
    assert_eq!(room.band_sets.len(), 1, "repeated applies add one set");
    assert_eq!(room.defaults.bands, Some(0));
    assert_eq!(room.band_sets[0].bands[0].material, Some(rid(14)));
    // Back to zero: the whole room is as it began.
    drag.apply(&mut world, 0);
    assert!(world.rooms[0].band_sets.is_empty());
    // A room that already has a default band set is left alone.
    let mut world = hall(3, 3);
    world.rooms[0].defaults.rules.trim = Some(rid(14));
    world.rooms[0]
        .band_sets
        .push(crate::grid::BandSet::single("own", None));
    world.rooms[0].defaults.bands = Some(0);
    raise_region(
        &mut world,
        &square([1, 1], [1, 1]),
        params(RaiseMode::Offset),
        256,
    )
    .unwrap();
    assert_eq!(world.rooms[0].band_sets.len(), 1);
    assert_eq!(world.rooms[0].wall_mode, WallMode::Derived);
}

mod ramp {
    use super::*;
    use crate::grid::edit::verbs::ramp::{ramp_region, RampDir, RampDrag, RampParams};
    use crate::grid::slope::{surface_class, SlopeClass};
    use crate::grid::units::FINE_UNITS;
    use psx_engine::WalkLimits;

    fn p(from: RampDir) -> RampParams {
        RampParams {
            from,
            follow_ceiling: true,
            wall_material: None,
        }
    }

    #[test]
    fn a_ramp_runs_from_the_a_edge_to_the_b_edge_in_even_rows() {
        let mut world = hall(6, 6);
        let region = square([1, 1], [2, 3]);
        let applied = ramp_region(&mut world, &region, p(RampDir::North), 768).unwrap();
        assert_eq!(applied.moved, 6);
        // Three rows deep, 768 units over them: 256 a row.
        assert_eq!(floor(&world, [1, 1]).heights, [0, 0, 256, 256]);
        assert_eq!(floor(&world, [1, 2]).heights, [256, 256, 512, 512]);
        assert_eq!(floor(&world, [2, 3]).heights, [512, 512, 768, 768]);
        assert_eq!(
            floor(&world, [0, 1]).heights,
            [0; 4],
            "outside is untouched"
        );
        // From the other side it climbs the other way.
        let mut south = hall(6, 6);
        ramp_region(&mut south, &region, p(RampDir::South), 768).unwrap();
        assert_eq!(floor(&south, [1, 3]).heights, [256, 256, 0, 0]);
        // East and west run along X.
        let mut west = hall(6, 6);
        ramp_region(&mut west, &square([1, 1], [3, 1]), p(RampDir::West), 768).unwrap();
        assert_eq!(floor(&west, [3, 1]).heights, [512, 768, 768, 512]);
        let mut east = hall(6, 6);
        ramp_region(&mut east, &square([1, 1], [3, 1]), p(RampDir::East), 768).unwrap();
        assert_eq!(floor(&east, [1, 1]).heights, [768, 512, 512, 768]);
    }

    #[test]
    fn the_a_edge_keeps_its_height_and_the_ceiling_follows() {
        let mut world = hall(4, 4);
        for x in 0..4 {
            world.rooms[0]
                .sector_mut(x, 1)
                .unwrap()
                .floor
                .as_mut()
                .unwrap()
                .heights = [512; 4];
        }
        let region = square([0, 1], [3, 2]);
        ramp_region(&mut world, &region, p(RampDir::North), 512).unwrap();
        // The A edge sits where the floor stood on it: 512.
        assert_eq!(floor(&world, [0, 1]).heights, [512, 512, 768, 768]);
        assert_eq!(floor(&world, [0, 2]).heights, [768, 768, 1024, 1024]);
        // The ceiling moved by the floor's delta, corner by corner.
        assert_eq!(ceiling(&world, [0, 1]).heights, [3072, 3072, 3328, 3328]);
    }

    #[test]
    fn an_l_shaped_region_slopes_by_position() {
        let mut world = hall(6, 6);
        let region = crate::grid::edit::Region::from_cells(0, [[1, 1], [1, 2], [1, 3], [2, 3]]);
        ramp_region(&mut world, &region, p(RampDir::North), 768).unwrap();
        // The foot of the L is on the last row: every corner there is level
        // with the row's position, not a plane of its own.
        assert_eq!(floor(&world, [1, 1]).heights, [0, 0, 256, 256]);
        assert_eq!(floor(&world, [2, 3]).heights, [512, 512, 768, 768]);
    }

    #[test]
    fn a_drag_restores_exactly_and_the_steepest_rise_is_the_walkable_limit() {
        let mut world = hall(8, 8);
        let original = world.clone();
        let region = square([1, 1], [6, 3]);
        let drag = RampDrag::begin(&world, &region, p(RampDir::North)).unwrap();
        drag.apply(&mut world, 768);
        drag.apply(&mut world, -256);
        drag.apply(&mut world, 0);
        assert_eq!(world, original);
        // The largest walkable rise: every floor of it can be stood on, and
        // one fine step more leaves a floor too steep.
        let max = drag.max_walkable_rise();
        assert!(max > 0 && max % FINE_UNITS == 0, "{max}");
        let units = crate::grid::cook::RuntimeUnits::new(world.scale).unwrap();
        let class_after = |rise: i32| {
            let mut w = original.clone();
            drag.apply(&mut w, rise);
            (1..=6)
                .flat_map(|x| (1..=3).map(move |z| [x, z]))
                .map(|cell| surface_class(&units, 0, &floor(&w, cell), &WalkLimits::DEFAULT))
                .max()
                .unwrap()
        };
        assert_ne!(class_after(max), SlopeClass::Steep, "{max}");
        assert_eq!(class_after(max + FINE_UNITS), SlopeClass::Steep, "{max}");
        // Lowering is the same slope the other way.
        assert_ne!(class_after(-max), SlopeClass::Steep);
        assert_eq!(class_after(-max - FINE_UNITS), SlopeClass::Steep);
    }

    #[test]
    fn rules_paint_a_ramp_by_its_class_unless_painted_by_hand() {
        let mut world = hall(4, 4);
        world.rooms[0].defaults.rules = PaintRules {
            floor_by_class: [None, Some(rid(31)), Some(rid(32))],
            ..PaintRules::default()
        };
        world.rooms[0]
            .sector_mut(1, 2)
            .unwrap()
            .floor
            .as_mut()
            .unwrap()
            .paint_locked = true;
        // Three rows of 256: a gentle slope.
        ramp_region(&mut world, &square([1, 1], [1, 3]), p(RampDir::North), 768).unwrap();
        assert_eq!(floor(&world, [1, 1]).tri_a.material, Some(rid(31)));
        assert_eq!(
            floor(&world, [1, 2]).tri_a.material,
            Some(rid(1)),
            "painted by hand"
        );
        // Far too steep: the steep rule.
        let mut steep = world.clone();
        ramp_region(
            &mut steep,
            &square([1, 1], [1, 3]),
            p(RampDir::North),
            12288,
        )
        .unwrap();
        assert_eq!(floor(&steep, [1, 3]).tri_a.material, Some(rid(32)));
    }
}

mod stairs {
    use super::*;
    use crate::grid::edit::verbs::stairs::{
        lay_stairs, max_riser, Refusal, RiserOption, StairsDrag, StairsParams,
    };

    fn p() -> StairsParams {
        StairsParams::default()
    }

    fn straight(n: i32) -> Vec<[i32; 2]> {
        vec![[1, 2], [n, 2]]
    }

    #[test]
    fn a_straight_flight_steps_up_one_riser_a_sector() {
        let mut world = hall(9, 5);
        let before = all_faces(&world).len();
        let flight = lay_stairs(&mut world, 0, &straight(6), p(), 6 * CLICK_UNITS).unwrap();
        assert_eq!((flight.steps, flight.riser, flight.total), (6, 256, 1536));
        for (i, x) in (1..=6).enumerate() {
            assert_eq!(
                floor(&world, [x, 2]).heights,
                [256 * (i as i32 + 1); 4],
                "{x}"
            );
        }
        assert_eq!(
            floor(&world, [0, 2]).heights,
            [0; 4],
            "the bottom step is the floor before it"
        );
        assert_eq!(floor(&world, [7, 2]).heights, [0; 4]);
        // The ceiling followed each tread.
        assert_eq!(ceiling(&world, [3, 2]).heights, [3072 + 768; 4]);
        // Derived risers: one between sectors 0|1 ... 5|6, plus the sides of
        // the tall end against the unraised rows north and south (every
        // tread has two) and the sector after the top.
        assert!(all_faces(&world).len() > before + 6);
    }

    #[test]
    fn the_max_riser_option_uses_taller_steps_and_a_landing_at_the_top() {
        let mut world = hall(9, 5);
        let units = crate::grid::cook::RuntimeUnits::new(world.scale).unwrap();
        // 40 runtime units: two clicks at the default scale.
        assert_eq!(max_riser(&units, &psx_engine::WalkLimits::DEFAULT), 512);
        let mut params = p();
        params.riser = RiserOption::Max;
        let flight = lay_stairs(&mut world, 0, &straight(6), params, 1536).unwrap();
        // 1536 / 512 = 3 steps, then a flat landing.
        assert_eq!((flight.steps, flight.riser), (3, 512));
        let tops: Vec<i32> = (1..=6).map(|x| floor(&world, [x, 2]).heights[0]).collect();
        assert_eq!(tops, vec![512, 1024, 1536, 1536, 1536, 1536]);
    }

    #[test]
    fn a_flight_that_would_need_a_riser_over_the_limit_is_refused_and_writes_nothing() {
        let mut world = hall(9, 5);
        let original = world.clone();
        // Three treads, twelve clicks: four clicks a riser, over the two
        // click limit.
        let drag = StairsDrag::begin(&world, 0, &[[1, 2], [3, 2]], p()).unwrap();
        let flight = drag.apply(&mut world, 12 * CLICK_UNITS);
        assert_eq!(
            flight.refusal,
            Some(Refusal::TooSteep {
                riser: 1024,
                limit: 512
            })
        );
        assert_eq!(world, original);
        // The ramp option has no steps to limit.
        let mut params = p();
        params.ramp = true;
        let mut with_ramp = original.clone();
        let flight = lay_stairs(
            &mut with_ramp,
            0,
            &[[1, 2], [3, 2]],
            params,
            12 * CLICK_UNITS,
        )
        .unwrap();
        assert!(flight.refusal.is_none());
    }

    #[test]
    fn a_turn_is_a_landing_and_the_flight_goes_on() {
        let mut world = hall(9, 7);
        // East along z = 1 then south along x = 5: the corner (5, 1) is a landing.
        let path = [[1, 1], [5, 1], [5, 4]];
        let flight = lay_stairs(&mut world, 0, &path, p(), 6 * CLICK_UNITS).unwrap();
        // Seven path cells, one a landing: six treads.
        assert_eq!(flight.steps, 6);
        assert_eq!(floor(&world, [4, 1]).heights[0], 4 * 256);
        assert_eq!(
            floor(&world, [5, 1]).heights[0],
            4 * 256,
            "the landing repeats"
        );
        assert_eq!(floor(&world, [5, 2]).heights[0], 5 * 256);
        assert_eq!(floor(&world, [5, 4]).heights[0], 6 * 256);
    }

    #[test]
    fn a_wide_flight_steps_across_its_width_and_going_down_works() {
        let mut world = hall(9, 7);
        let mut params = p();
        params.width = 3;
        lay_stairs(&mut world, 0, &straight(4), params, -3 * CLICK_UNITS).unwrap();
        for z in 1..=3 {
            assert_eq!(floor(&world, [2, z]).heights[0], -512, "row {z}");
        }
        assert_eq!(floor(&world, [2, 0]).heights[0], 0);
        assert_eq!(floor(&world, [4, 2]).heights[0], -768);
    }

    #[test]
    fn floors_are_made_where_the_path_crosses_holes_and_the_room_never_grows() {
        let mut world = hall(9, 5);
        world.rooms[0].sector_mut(3, 2).unwrap().floor = None;
        let flight = lay_stairs(&mut world, 0, &[[1, 2], [12, 2]], p(), 4 * CLICK_UNITS).unwrap();
        assert_eq!(flight.applied.created, 1);
        assert_eq!(
            floor(&world, [3, 2]).heights[0],
            3 * 256 + 256 * (flight.steps as i32 - 4).max(0)
        );
        assert!(
            flight.applied.skipped > 0,
            "sectors past the room are skipped"
        );
        assert_eq!(world.rooms[0].width, 9);
    }

    #[test]
    fn the_ramp_option_slopes_each_tread_from_its_entry_to_its_exit() {
        let mut world = hall(9, 5);
        let mut params = p();
        params.ramp = true;
        lay_stairs(&mut world, 0, &straight(4), params, 4 * CLICK_UNITS).unwrap();
        // Eastbound: west edge is the entry.
        assert_eq!(floor(&world, [1, 2]).heights, [0, 256, 256, 0]);
        assert_eq!(floor(&world, [3, 2]).heights, [512, 768, 768, 512]);
    }

    #[test]
    fn a_drag_is_absolute_and_restores_exactly() {
        let mut world = hall(9, 5);
        let original = world.clone();
        let drag = StairsDrag::begin(&world, 0, &straight(6), p()).unwrap();
        drag.apply(&mut world, 1536);
        drag.apply(&mut world, 512);
        drag.apply(&mut world, 0);
        assert_eq!(world, original);
    }
}

#[test]
fn height_rules_paint_floors_by_height_and_make_a_strata_band_set() {
    use crate::grid::paint_rules::HeightRule;
    let mut world = hall(6, 6);
    let (scree, rock, snow, strata_a, strata_b) = (rid(21), rid(22), rid(23), rid(24), rid(25));
    world.rooms[0].defaults.rules = PaintRules {
        floor_by_height: HeightRule {
            at: [4, 12, 0],
            materials: [Some(scree), Some(rock), Some(snow), None],
        },
        wall_by_height: HeightRule {
            at: [8, 0, 0],
            materials: [Some(strata_a), Some(strata_b), None, None],
        },
        ..PaintRules::default()
    };
    let region = square([1, 1], [2, 2]);
    // One click (256) is below the first threshold (4 * 64 = 256 is the
    // threshold itself, so band 1): raise by half a click first.
    raise_region(&mut world, &region, params(RaiseMode::Offset), 128).unwrap();
    assert_eq!(floor(&world, [1, 1]).tri_a.material, Some(scree));
    raise_region(&mut world, &region, params(RaiseMode::Level), 512).unwrap();
    assert_eq!(floor(&world, [1, 1]).tri_a.material, Some(rock));
    raise_region(&mut world, &region, params(RaiseMode::Level), 1024).unwrap();
    assert_eq!(floor(&world, [2, 2]).tri_a.material, Some(snow));
    // A floor painted by hand keeps its material.
    world.rooms[0]
        .sector_mut(1, 1)
        .unwrap()
        .floor
        .as_mut()
        .unwrap()
        .paint_locked = true;
    raise_region(&mut world, &region, params(RaiseMode::Level), 0).unwrap();
    assert_eq!(floor(&world, [1, 1]).tri_a.material, Some(snow));
    assert_eq!(floor(&world, [2, 2]).tri_a.material, Some(scree));
    // The verbs made the room's strata: one set, an absolute line at 8 * 64.
    let room = &world.rooms[0];
    assert_eq!(room.band_sets.len(), 1);
    assert_eq!(room.band_sets[0].name, "Strata");
    assert_eq!(
        room.band_sets[0].lines,
        vec![crate::grid::bands::Anchor::Abs(512)]
    );
    assert_eq!(room.defaults.bands, Some(0));
    // Running a verb again updates it in place.
    raise_region(&mut world, &region, params(RaiseMode::Level), 256).unwrap();
    assert_eq!(world.rooms[0].band_sets.len(), 1);
}
