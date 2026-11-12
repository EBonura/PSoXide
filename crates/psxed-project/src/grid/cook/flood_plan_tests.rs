//! The flood block the cook writes for a level that streams by flood, and
//! the gates it applies.

use psx_grid::write::{aligned_copy, words_as_bytes};
use psx_grid::{LevelView, FLOOD_WEIGHT_ONE, LEVEL_BUDGET_FLOOD};

use super::tests::cook;
use super::*;
use crate::grid::room::GridRoom;
use crate::grid::surface::GridSurface;
use crate::grid::world::{FloodSettings, StreamRadii};

/// `count` one-sector rooms side by side, 25 to a row, floored and roofed so
/// neighbours get portals.
fn strip(count: usize) -> GridWorld {
    spaced(count, 1)
}

/// [`strip`] with `gap` sectors between the origins of neighbours: 1 touches,
/// 2 leaves a sector between rooms, so they get no portal.
fn spaced(count: usize, gap: usize) -> GridWorld {
    let rooms = (0..count)
        .map(|i| {
            let mut room = GridRoom::new(format!("r{i}"), 1, 1);
            room.origin = [((i % 25) * gap) as i32, ((i / 25) * gap) as i32];
            let sector = room.sector_or_insert(0, 0).unwrap();
            sector.set_ground_floor(Some(GridSurface::flat(0, None)));
            sector.set_ground_ceiling(Some(GridSurface::flat(1024, None)));
            room
        })
        .collect();
    GridWorld {
        rooms,
        ..GridWorld::default()
    }
}

fn flooded(mut world: GridWorld, near: u8, res: u8) -> GridWorld {
    // Entities only think close by, so the activation walk does not keep the
    // whole level resident.
    world.stream.active_radius = 2;
    world.stream.flood = FloodSettings {
        on: true,
        near_radius: near,
        res_radius: res,
        quad_budget: 0,
        skin: false,
        skin_tolerance: 0,
        cards: false,
        card_far: 0,
        card_near: 0,
        card_layers: 0,
        panels: 0,
    };
    world
}

fn view(cooked: &CookedLevel) -> (Vec<u32>, usize) {
    (aligned_copy(&cooked.core), cooked.core.len())
}

#[test]
fn a_level_that_fits_the_pool_is_held_whole() {
    let cooked = cook(&flooded(strip(40), 4, 8)).unwrap();
    let (words, len) = view(&cooked);
    let level = LevelView::parse(words_as_bytes(&words, len)).unwrap();
    level.validate_full().unwrap();
    assert_ne!(level.header.flags & LEVEL_BUDGET_FLOOD, 0);
    let flood = level.budget_flood().expect("the flood block");
    assert_eq!(flood.pool_cap_pages, 0, "a small level is held whole");
    assert!(cooked
        .report
        .warnings
        .iter()
        .any(|w| w.contains("never runs")));
}

#[test]
fn a_big_level_gets_a_page_budget_the_radii_and_unit_weights() {
    let cooked = cook(&flooded(strip(600), 4, 8)).unwrap();
    let (words, len) = view(&cooked);
    let level = LevelView::parse(words_as_bytes(&words, len)).unwrap();
    level.validate_full().unwrap();
    let flood = level.budget_flood().expect("the flood block");
    // The pool the guest links less the largest room (one page).
    assert_eq!(
        usize::from(flood.pool_cap_pages),
        GUEST_POOL_CEILING_KIB / 2 - 1
    );
    assert_eq!((flood.near_radius, flood.res_radius), (4, 8));
    assert_eq!(level.flood_weight.len(), level.portals.len());
    assert!(level.flood_weight.iter().all(|&w| w == FLOOD_WEIGHT_ONE));
    // The core's own radii follow the flood's for observers outside a room.
    assert_eq!((level.header.draw_far, level.header.lead_radius), (4, 4));
    assert!(cooked
        .report
        .warnings
        .iter()
        .any(|w| w.contains("flood streaming: draw 4 sectors, resident 8")));
}

#[test]
fn a_resident_radius_below_the_draw_radius_is_refused() {
    let error = cook(&flooded(strip(40), 8, 4)).unwrap_err();
    assert!(error.contains("resident radius"), "{error}");
}

#[test]
fn a_view_over_the_quad_budget_is_refused() {
    let mut world = flooded(strip(600), 4, 8);
    world.stream.flood.quad_budget = 1;
    let error = cook(&world).unwrap_err();
    assert!(error.contains("quads"), "{error}");
    // A generous budget passes.
    world.stream.flood.quad_budget = 5000;
    assert!(cook(&world).is_ok());
}

#[test]
fn a_level_that_does_not_ask_for_the_flood_cooks_without_the_block() {
    let cooked = cook(&strip(40)).unwrap();
    let (words, len) = view(&cooked);
    let level = LevelView::parse(words_as_bytes(&words, len)).unwrap();
    assert_eq!(level.header.flags & LEVEL_BUDGET_FLOOD, 0);
    assert!(level.budget_flood().is_none());
}

#[test]
fn the_default_flood_settings_are_not_written_to_a_document() {
    let text = ron::to_string(&StreamRadii::DEFAULT).unwrap();
    assert!(!text.contains("flood"), "{text}");
    let mut on = StreamRadii::DEFAULT;
    on.flood.on = true;
    let text = ron::to_string(&on).unwrap();
    assert!(text.contains("flood"), "{text}");
    let back: StreamRadii = ron::from_str(&text).unwrap();
    assert_eq!(back, on);
    // A document written before the setting existed reads as the defaults.
    let old: StreamRadii =
        ron::from_str("(draw_far: 12, active_radius: 12, lead_radius: 4, ring: true)").unwrap();
    assert_eq!(old.flood, FloodSettings::DEFAULT);
}

/// [`cook`] for a guest whose pool may take `ceiling_kib`.
fn cook_within(world: &GridWorld, ceiling_kib: usize) -> Result<CookedLevel, String> {
    cook_level_within(
        CookInput {
            world,
            binder: &mut super::tests::TestBinder,
            lights: &[],
            first_pack_sector: 100,
            lod_far_cells: 0,
            texture_repeat: true,
        },
        None,
        ceiling_kib,
    )
}

/// `count` one-sector rooms a sector apart: no two touch, so no portals.
fn islands(count: usize) -> GridWorld {
    spaced(count, 2)
}

#[test]
fn a_connected_level_just_over_the_ceiling_streams() {
    // 40 connected one page rooms against a 30 page (60 KiB) pool: not held
    // whole, a page budget of the ceiling less the largest room.
    let cooked = cook_within(&flooded(strip(40), 4, 8), 60).unwrap();
    let (words, len) = view(&cooked);
    let level = LevelView::parse(words_as_bytes(&words, len)).unwrap();
    let flood = level.budget_flood().expect("the flood block");
    assert_eq!(flood.pool_cap_pages, 29);
    // And just under it is held whole.
    let cooked = cook_within(&flooded(strip(40), 4, 8), 80).unwrap();
    let (words, len) = view(&cooked);
    let level = LevelView::parse(words_as_bytes(&words, len)).unwrap();
    assert_eq!(level.budget_flood().unwrap().pool_cap_pages, 0);
}

#[test]
fn a_level_whose_largest_room_leaves_no_page_budget_is_refused() {
    // The pool needs the resident set and the largest room. A ceiling of one
    // page holds one room and nothing to stream it past: a page budget of 0
    // would read as "held whole" to the guest, so the cook refuses instead.
    let error = cook_within(&flooded(strip(40), 4, 8), 2).unwrap_err();
    assert!(error.contains("largest room"), "{error}");
}

#[test]
fn separate_islands_stream_even_when_they_would_fit_whole() {
    // 12 one page rooms, none touching: 24 KiB, under a 40 KiB pool, but the
    // player is only ever in one of them.
    let world = flooded(islands(12), 4, 8);
    let cooked = cook_within(&world, 40).unwrap();
    assert!(cooked.level.portals.is_empty());
    let (words, len) = view(&cooked);
    let level = LevelView::parse(words_as_bytes(&words, len)).unwrap();
    let flood = level.budget_flood().expect("the flood block");
    assert_ne!(flood.pool_cap_pages, 0, "islands are not held whole");
    assert!(cooked
        .report
        .warnings
        .iter()
        .any(|w| w.contains("12 islands")));
    // The pool is the worst island, not the sum: a one-room island has nothing landing beside it.
    let chunks: Vec<usize> = cooked
        .chunks
        .iter()
        .map(|c| c.len().div_ceil(2048))
        .collect();
    let plan = psx_grid::stream::plan_pool(&level, &chunks);
    assert_eq!(plan.peak_rooms, 1, "one island, one room, at a time");
    assert_eq!(plan.pool_pages, plan.max_room_pages);
    assert!(plan.pool_pages < chunks.iter().sum::<usize>());
}

#[test]
fn islands_are_counted_through_portals_only() {
    let one = cook_within(&flooded(strip(6), 4, 8), 4096).unwrap();
    let pages: Vec<usize> = one.chunks.iter().map(|c| c.len().div_ceil(2048)).collect();
    assert_eq!(flood_plan::island_pages(&one.level, &pages).len(), 1);
    let apart = cook_within(&flooded(islands(6), 4, 8), 4096).unwrap();
    let pages: Vec<usize> = apart
        .chunks
        .iter()
        .map(|c| c.len().div_ceil(2048))
        .collect();
    assert_eq!(flood_plan::island_pages(&apart.level, &pages).len(), 6);
}
