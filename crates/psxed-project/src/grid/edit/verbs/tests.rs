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
