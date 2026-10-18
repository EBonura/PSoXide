//! The cook's streaming-ready output: room-local materials, the world
//! lattice, scratch maxima and per-room (never whole-level) caps.

use psx_grid::write::{aligned_copy, words_as_bytes};
use psx_grid::{LevelView, RoomView};

use super::tests::cook;
use super::*;
use crate::grid::room::GridRoom;
use crate::grid::surface::GridSurface;
use crate::ResourceId;

fn rid(n: u64) -> ResourceId {
    ron::from_str(&format!("({n})")).expect("a resource id")
}

/// `count` one-sector rooms, 25 to a row, room `i` floored with material
/// `1 + i % 3` (so rooms use different subsets of the level's materials).
fn row(count: usize) -> GridWorld {
    let rooms = (0..count)
        .map(|i| {
            let mut room = GridRoom::new(format!("r{i}"), 1, 1);
            room.origin = [(i % 25) as i32 * 3, (i / 25) as i32 * 3];
            let sector = room.sector_or_insert(0, 0).unwrap();
            sector.set_floor(Some(GridSurface::flat(0, Some(rid(1 + (i as u64 % 3))))));
            room
        })
        .collect();
    GridWorld {
        rooms,
        ..GridWorld::default()
    }
}

#[test]
fn a_room_names_its_materials_through_its_own_table() {
    let cooked = cook(&row(3)).unwrap();
    assert_eq!(cooked.level.materials.len(), 3);
    for (i, chunk) in cooked.tables.iter().enumerate() {
        // Each room draws one material, so its table has one entry whatever
        // the level-wide id, and every quad names entry 0.
        assert_eq!(chunk.material_use.len(), 1, "room {i}");
        assert!(chunk.quads.iter().all(|q| q.material == 0));
        let drawn = cooked.level.materials[usize::from(chunk.material_use[0])].texture_asset;
        assert_eq!(u64::from(drawn), 1 + i as u64 % 3);
    }
    // The tables differ between rooms: the indirection is real.
    assert_ne!(cooked.tables[0].material_use, cooked.tables[1].material_use);
}

#[test]
fn the_core_carries_the_lattice_the_maxima_and_the_radii() {
    let mut world = row(40);
    world.stream.draw_far = 20;
    world.stream.active_radius = 10;
    world.stream.lead_radius = 3;
    let cooked = cook(&world).unwrap();
    let words = aligned_copy(&cooked.core);
    let level = LevelView::parse(words_as_bytes(&words, cooked.core.len())).unwrap();
    level.validate_full().unwrap();
    assert_eq!(level.rooms.len(), 40);
    let h = level.header;
    assert_eq!((h.draw_far, h.active_radius, h.lead_radius), (20, 10, 3));
    // Every room is in the lattice cell of its own centre.
    let lattice = level.lattice();
    for (index, rec) in level.rooms.iter().enumerate() {
        let s = level.sector_units() as i32;
        let centre = (rec.origin[0] + s / 2, rec.origin[2] + s / 2);
        let found: Vec<usize> = lattice.candidates(&level, centre, centre).collect();
        assert!(
            found.contains(&index),
            "room {index} not found at its centre"
        );
    }
    // The maxima are the largest room's table lengths.
    let mut most = psx_grid::RenderMaxima::default();
    for chunk in &cooked.tables {
        most.include(
            chunk.vertices.len(),
            chunk.cells.len(),
            chunk.quads.len(),
            chunk.lod_blocks.len(),
            chunk.lod_quads.len(),
        );
    }
    assert_eq!(level.maxima(), most);
    assert!(level.maxima().quads >= 1);
    // The biggest chunk in pages is on the header, not a level total.
    let biggest = cooked
        .chunks
        .iter()
        .map(|c| c.len().div_ceil(2048))
        .max()
        .unwrap();
    assert_eq!(usize::from(h.max_room_pages), biggest);
    assert_eq!(cooked.report.max_room_pages, biggest);
    assert!(cooked.report.pool_pages >= 40);
}

#[test]
fn a_world_of_many_rooms_has_no_world_wide_cap() {
    // More rooms than the old 256 room set, in a world far bigger than the
    // old whole-world page cap would have allowed for chunky rooms.
    let cooked = cook(&row(600)).unwrap();
    assert_eq!(cooked.level.rooms.len(), 600);
    let words = aligned_copy(&cooked.core);
    let level = LevelView::parse(words_as_bytes(&words, cooked.core.len())).unwrap();
    level.validate_full().unwrap();
    // The lattice stays small: a few cells per room at most.
    let cells =
        usize::from(level.header.lattice_cells[0]) * usize::from(level.header.lattice_cells[1]);
    assert!(cells <= 2 * 600, "{cells} lattice cells");
    for chunk in cooked.chunks.iter().take(3) {
        let words = aligned_copy(chunk);
        RoomView::parse(words_as_bytes(&words, chunk.len())).unwrap();
    }
}

#[test]
fn the_default_draw_distance_is_in_the_header() {
    let cooked = cook(&row(2)).unwrap();
    let words = aligned_copy(&cooked.core);
    let level = LevelView::parse(words_as_bytes(&words, cooked.core.len())).unwrap();
    let d = crate::grid::StreamRadii::DEFAULT;
    assert_eq!(
        (
            level.header.draw_far,
            level.header.active_radius,
            level.header.lead_radius
        ),
        (d.draw_far, d.active_radius, d.lead_radius)
    );
}
