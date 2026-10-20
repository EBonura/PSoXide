//! Banded walls through the cook: the pieces cook as ordinary quads and
//! triangles, the tables validate, and a band costs what the report says.

use psx_grid::RoomView;

use super::tests::cook;
use crate::grid::bands::{Anchor, BandSet, NO_BANDS};
use crate::grid::derive::edge_key;
use crate::grid::room::GridRoom;
use crate::grid::sector::WallEdge;
use crate::grid::surface::GridSurface;
use crate::grid::units::GridScale;
use crate::grid::world::GridWorld;
use crate::ResourceId;

fn rid(n: u64) -> ResourceId {
    ron::from_str(&format!("({n})")).expect("a resource id")
}

fn hall() -> GridWorld {
    let mut room = GridRoom::new_derived("hall", 3, 2);
    room.defaults.wall_material = Some(rid(9));
    for z in 0..2 {
        for x in 0..3 {
            let s = room.sector_or_insert(x, z).unwrap();
            s.set_ground_floor(Some(GridSurface::flat(0, Some(rid(1)))));
            s.set_ground_ceiling(Some(GridSurface::flat(2048, Some(rid(2)))));
        }
    }
    GridWorld {
        scale: GridScale::DEFAULT,
        rooms: vec![room],
        ..GridWorld::default()
    }
}

fn three_bands() -> BandSet {
    let mut set = BandSet::single("walls", Some(rid(2)));
    set.insert_line(0, Anchor::Bottom(256));
    set.insert_line(1, Anchor::Top(256));
    set.set_band_material(0, Some(rid(1)));
    set.set_band_material(2, Some(rid(3)));
    set
}

fn primitives(world: &GridWorld) -> (usize, usize) {
    let cooked = cook(world).unwrap();
    let words = psx_grid::write::aligned_copy(&cooked.chunks[0]);
    let view = RoomView::parse(psx_grid::write::words_as_bytes(
        &words,
        cooked.chunks[0].len(),
    ))
    .unwrap();
    view.validate_full(cooked.level.materials.len()).unwrap();
    view.validate_repeat(&cooked.level.materials).unwrap();
    assert_eq!(cooked.report.rooms[0].uv_overflow, 0);
    let r = &cooked.report.rooms[0];
    (r.quads + r.triangles, r.bytes)
}

#[test]
fn a_banded_hall_cooks_with_three_primitives_per_wall_segment() {
    let plain = hall();
    let mut banded = hall();
    banded.rooms[0].band_sets.push(three_bands());
    banded.rooms[0].defaults.bands = Some(0);
    let (before, before_bytes) = primitives(&plain);
    let (after, after_bytes) = primitives(&banded);
    // 10 outline segments: one primitive each plain, three banded.
    let segments = 2 * (3 + 2);
    assert_eq!(after - before, 2 * segments, "{before} -> {after}");
    eprintln!(
        "bands: {segments} wall segments, {before} -> {after} primitives, chunk {before_bytes} -> {after_bytes} B (+{} B per band quad)",
        (after_bytes - before_bytes) / (after - before)
    );
}

#[test]
fn a_banded_wall_next_to_a_different_one_cooks_without_overlong_rings() {
    let mut world = hall();
    {
        let room = &mut world.rooms[0];
        room.band_sets.push(three_bands());
        let mut other = BandSet::single("other", Some(rid(4)));
        other.insert_line(0, Anchor::Bottom(512));
        room.band_sets.push(other);
        room.edge_overrides
            .edit(edge_key([0, 0], WallEdge::North), |o| {
                o.band_set = Some(0);
                o.material = Some(rid(21));
            });
        room.edge_overrides
            .edit(edge_key([1, 0], WallEdge::North), |o| {
                o.band_set = Some(1);
                o.material = Some(rid(22));
            });
        room.edge_overrides
            .edit(edge_key([2, 0], WallEdge::North), |o| {
                o.band_set = Some(NO_BANDS);
                o.material = Some(rid(23));
            });
    }
    // Cooks (a ring past four corners would be an error) and validates.
    let (primitives, _) = primitives(&world);
    assert!(primitives > 0);
}

#[test]
fn the_bands_of_one_wall_cook_with_their_own_materials() {
    // A two band set: the bottom band in material 3, the top in material 4.
    // The wall's own material (9) is drawn nowhere.
    let mut world = hall();
    let mut set = BandSet::single("two", Some(rid(3)));
    set.insert_line(0, Anchor::Abs(1024));
    set.set_band_material(1, Some(rid(4)));
    world.rooms[0].band_sets.push(set);
    world.rooms[0].defaults.bands = Some(0);
    let cooked = cook(&world).unwrap();
    let used: Vec<_> = cooked.materials.iter().filter_map(|(id, _)| *id).collect();
    assert!(used.contains(&rid(3)) && used.contains(&rid(4)), "{used:?}");
    assert!(!used.contains(&rid(9)), "{used:?}");
    // Painting only the top band changes only the top band's material.
    world.rooms[0].band_sets[0].set_band_material(1, Some(rid(5)));
    let used: Vec<_> = cook(&world)
        .unwrap()
        .materials
        .iter()
        .filter_map(|(id, _)| *id)
        .collect();
    assert!(used.contains(&rid(3)) && used.contains(&rid(5)), "{used:?}");
    assert!(!used.contains(&rid(4)), "{used:?}");
}
