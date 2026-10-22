//! The skin bake: merging, heights, colours and skirts.

use super::skin::{bake, SkinSettings};
use super::tests::cook;
use super::*;
use crate::grid::room::GridRoom;
use crate::grid::surface::GridSurface;

/// A room of `size` by `size` sectors at world sector `origin`, floored at
/// `floor` grid units.
fn plate(origin: [i32; 2], size: usize, floor: i32) -> GridRoom {
    let mut room = GridRoom::new("plate", size, size);
    room.origin = origin;
    for z in 0..size {
        for x in 0..size {
            room.sector_or_insert(x, z)
                .unwrap()
                .set_ground_floor(Some(GridSurface::flat(floor, None)));
        }
    }
    room
}

fn world(rooms: Vec<GridRoom>) -> GridWorld {
    GridWorld {
        rooms,
        ..GridWorld::default()
    }
}

fn albedo(cooked: &CookedLevel) -> Vec<[f32; 3]> {
    vec![[0.5; 3]; cooked.level.materials.len().max(1)]
}

#[test]
fn a_flat_room_is_one_leaf_and_a_step_gets_a_skirt_on_the_high_side_only() {
    // Two 8 by 8 plates side by side, the east one a sector (80 runtime
    // units) higher.
    let cooked = cook(&world(vec![plate([0, 0], 8, 0), plate([8, 0], 8, 1024)])).unwrap();
    let (block, report) = bake(&cooked, &albedo(&cooked), SkinSettings::default()).unwrap();
    // One leaf per room at each of the three levels of detail.
    assert_eq!(report.leaves, 6, "{report:?}");
    assert_eq!(report.per_lod, [2, 2, 2]);
    assert_eq!(report.by_size, [0, 0, 0, 2]);
    assert_eq!(block.cell_first, vec![0, 1, 2, 3, 4, 5, 6]);
    let (low, high) = (block.leaves[0], block.leaves[3]);
    assert_eq!((low.x(), low.z(), low.size_log2()), (0, 0, 3));
    assert_eq!((high.x(), high.z(), high.size_log2()), (8, 0, 3));
    assert_eq!(low.height(), 0);
    let step = i32::from(block.header.height_step);
    assert_eq!(i32::from(high.height()) * step, 80);
    // The high plate drops 80 units towards the low one (its -X edge) and
    // stands on nothing else; the low plate drops nowhere.
    let need = (80 + step - 1) / step;
    assert!(
        psx_grid::SKIRT_STEPS[usize::from(high.skirt(3))] >= need as u16,
        "{high:?}"
    );
    assert_eq!((high.skirt(0), high.skirt(1), high.skirt(2)), (0, 0, 0));
    assert!((0..4).all(|side| low.skirt(side) == 0), "{low:?}");
}

#[test]
fn a_bump_in_a_flat_floor_splits_the_block_down_to_the_bump() {
    let mut room = plate([0, 0], 8, 0);
    room.sector_or_insert(3, 3)
        .unwrap()
        .set_ground_floor(Some(GridSurface::flat(2048, None)));
    let cooked = cook(&world(vec![room])).unwrap();
    let (block, report) = bake(&cooked, &albedo(&cooked), SkinSettings::default()).unwrap();
    // The 8 by 8 block is not flat: it splits in four, the quarter holding the
    // bump splits again, and so on down to the single sector.
    assert!(report.per_lod[0] > 4 && report.per_lod[0] < 64, "{report:?}");
    assert!(report.by_size[0] >= 1, "{report:?}");
    let finest = &block.leaves[..usize::from(block.cell_first[1])];
    let bump = finest
        .iter()
        .find(|l| l.size_log2() == 0 && l.x() == 3 && l.z() == 3)
        .expect("the bump is a leaf of its own");
    let flat = finest.iter().find(|l| l.x() == 4 && l.z() == 4).unwrap();
    assert!(bump.height() > flat.height());
    // The coarsest level of detail has no leaf smaller than 4 by 4.
    let coarse = &block.leaves[usize::from(block.cell_first[2])..];
    assert!(coarse.iter().all(|l| l.size_log2() >= 2), "{coarse:?}");
}

#[test]
fn leaves_never_cross_a_room() {
    // One 8 by 8 floor cut into two rooms of 4 by 8: two 4 by 4 leaves each,
    // not one 8 by 8 leaf over both.
    let cooked = cook(&world(vec![
        {
            let mut a = GridRoom::new("a", 4, 8);
            for z in 0..8 {
                for x in 0..4 {
                    a.sector_or_insert(x, z)
                        .unwrap()
                        .set_ground_floor(Some(GridSurface::flat(0, None)));
                }
            }
            a
        },
        {
            let mut b = GridRoom::new("b", 4, 8);
            b.origin = [4, 0];
            for z in 0..8 {
                for x in 0..4 {
                    b.sector_or_insert(x, z)
                        .unwrap()
                        .set_ground_floor(Some(GridSurface::flat(0, None)));
                }
            }
            b
        },
    ]))
    .unwrap();
    let (block, report) = bake(&cooked, &albedo(&cooked), SkinSettings::default()).unwrap();
    assert_eq!(report.by_size, [0, 0, 4, 0], "{report:?}");
    // Two 4 by 4 leaves per room at every level of detail.
    assert_eq!(block.cell_first, vec![0, 2, 4, 6, 8, 10, 12]);
}

#[test]
fn the_skin_round_trips_through_the_core() {
    let mut cooked = cook(&world(vec![plate([0, 0], 8, 0), plate([8, 0], 8, 1024)])).unwrap();
    let (block, _) = bake(&cooked, &albedo(&cooked), SkinSettings::default()).unwrap();
    cooked.level.skin = Some(block.clone());
    let bytes = cooked.level.try_to_bytes().unwrap();
    let words = psx_grid::write::aligned_copy(&bytes);
    let level = psx_grid::LevelView::parse(psx_grid::write::words_as_bytes(&words, bytes.len()))
        .unwrap();
    assert_ne!(level.header.flags & psx_grid::LEVEL_HAS_SKIN, 0);
    assert_eq!(level.skin.copied(), Some(block.header));
    assert_eq!(level.skin_cells, block.cell_first.as_slice());
    assert_eq!(level.skin_leaves, block.leaves.as_slice());
}
