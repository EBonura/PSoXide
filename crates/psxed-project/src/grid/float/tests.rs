//! The converter end to end on synthetic models: a flat island with decks
//! hanging over it.

use super::columns::{plan_cells, Frame, HitField, COLUMNS_PER_SECTOR};
use super::place::{LARGE_NAME, SMALL_NAME};
use super::*;
use crate::grid::cook::tests::cook;
use crate::grid::room::GridRoom;
use crate::grid::surface::GridSurface;
use crate::grid::units::SECTOR_UNITS;
use crate::ResourceId;

/// One sector per model unit and the ground at zero: grid units / 1024.
const UNIT: Frame = Frame {
    ground: 0.0,
    sector: 1.0,
};

fn hit(units: i32) -> f32 {
    units as f32 / 1024.0
}

/// A solid box in model units: sectors on the plan, grid units in height.
#[derive(Clone, Copy)]
struct Deck {
    x: (f32, f32),
    z: (f32, f32),
    bottom: i32,
    top: i32,
}

/// An island `size` sectors square standing 512 high with the decks over it.
fn island(size: usize, decks: &[Deck]) -> HitField {
    let side = size * COLUMNS_PER_SECTOR;
    HitField::from_fn(side, side, 8, |row, col| {
        let (x, z) = (
            (col as f32 + 0.5) / COLUMNS_PER_SECTOR as f32,
            (row as f32 + 0.5) / COLUMNS_PER_SECTOR as f32,
        );
        let mut hits = Vec::new();
        for d in decks {
            if x >= d.x.0 && x < d.x.1 && z >= d.z.0 && z < d.z.1 {
                hits.extend([hit(d.top), hit(d.bottom)]);
            }
        }
        hits.push(hit(512));
        hits
    })
}

fn rid(n: u64) -> ResourceId {
    ron::from_str(&format!("({n})")).expect("a resource id")
}

/// A room of whole floored sectors at `origin`.
fn room(name: &str, origin: [i32; 2], width: usize, depth: usize) -> GridRoom {
    let mut room = GridRoom::new_derived(name, width, depth);
    room.origin = origin;
    for z in 0..depth {
        for x in 0..width {
            room.sector_or_insert(x, z)
                .unwrap()
                .set_ground_floor(Some(GridSurface::flat(0, Some(rid(7)))));
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

/// Quarter-sector fitting with the tight tolerance the exact decks need.
fn config() -> FloatConfig {
    FloatConfig {
        origin_sector: [0, 0],
        block_cells: 1,
        tolerance: 256,
        min_area_cells: 4,
        ..FloatConfig::default()
    }
}

fn style() -> SlabStyle {
    SlabStyle {
        materials: [Some(rid(8)), Some(rid(9)), Some(rid(10))],
        ..SlabStyle::default()
    }
}

/// World box `(min, max)` of every placed instance, in placement order.
fn boxes(world: &GridWorld) -> Vec<(usize, [i32; 3], [i32; 3])> {
    let mut out = Vec::new();
    for (index, room) in world.rooms.iter().enumerate() {
        for mesh in &room.meshes {
            let built = world.mesh_assets[usize::from(mesh.asset)].build().unwrap();
            let (lo, hi) = mesh.bounds(&built).unwrap();
            out.push((index, lo, hi));
        }
    }
    out
}

fn run(
    world: &mut GridWorld,
    field: &HitField,
    config: &FloatConfig,
) -> (FloatReport, Vec<PlanCell>) {
    let cells = plan_cells(field, &UNIT, config);
    let report = float_slabs(world, &cells, config, &style()).unwrap();
    (report, cells)
}

#[test]
fn a_deck_over_the_island_becomes_one_slab_at_its_exact_height() {
    let deck = Deck {
        x: (2.0, 6.5),
        z: (3.0, 4.25),
        bottom: 6144,
        top: 6912,
    };
    let field = island(8, &[deck]);
    let mut w = world(vec![room("island", [0, 0], 8, 8)]);
    let (report, _) = run(&mut w, &field, &config());
    assert_eq!(report.placed(), 1, "{}", report.summary());
    // The box is where the hits are. Heights come from the hits: 6144 and
    // 6912, whatever the voxel layers of the model would have been.
    assert_eq!(
        boxes(&w),
        vec![(0, [2 * 1024, 6144, 3 * 1024], [6656 + 0, 6912, 4352])]
    );
    let mesh = &w.rooms[0].meshes[0];
    assert!(mesh.collide);
    assert_eq!(
        mesh.materials,
        vec![Some(rid(8)), Some(rid(9)), Some(rid(10))]
    );
}

#[test]
fn a_lintel_over_the_ground_is_left_to_the_ground_fit() {
    let lintel = Deck {
        x: (1.0, 3.0),
        z: (1.0, 2.0),
        bottom: 1536,
        top: 2560,
    };
    let field = island(4, &[lintel]);
    let mut w = world(vec![room("island", [0, 0], 4, 4)]);
    let (report, cells) = run(&mut w, &field, &config());
    assert!(cells.is_empty());
    assert_eq!(report.placed(), 0);
}

#[test]
fn a_patch_under_the_minimum_area_is_dropped() {
    let small = Deck {
        x: (1.0, 1.5),
        z: (1.0, 1.25),
        bottom: 4096,
        top: 5120,
    };
    let field = island(4, &[small]);
    let mut w = world(vec![room("island", [0, 0], 4, 4)]);
    let (report, cells) = run(&mut w, &field, &config());
    assert_eq!(cells.len(), 2);
    assert_eq!((report.placed(), report.small_cells), (0, 2));
}

#[test]
fn a_deck_across_a_chunk_border_is_cut_into_each_chunks_own_sectors() {
    let deck = Deck {
        x: (1.0, 7.0),
        z: (1.0, 2.5),
        bottom: 4096,
        top: 5120,
    };
    let field = island(8, &[deck]);
    // Two rooms side by side, the border between sectors 3 and 4.
    let mut w = world(vec![room("west", [0, 0], 4, 8), room("east", [4, 0], 4, 8)]);
    let (report, _) = run(&mut w, &field, &config());
    let placed = boxes(&w);
    assert_eq!(placed.len(), 2, "{}", report.summary());
    assert_eq!(placed[0], (0, [1024, 4096, 1024], [4096, 5120, 2560]));
    assert_eq!(placed[1], (1, [4096, 4096, 1024], [7168, 5120, 2560]));
    // Each instance lies wholly inside the sectors of the room that holds it.
    for (index, lo, hi) in placed {
        let r = &w.rooms[index];
        for sx in lo[0].div_euclid(1024)..=(hi[0] - 1).div_euclid(1024) {
            assert!(r.local([sx, 1]).is_some(), "room {index} sector {sx}");
        }
    }
}

#[test]
fn a_run_can_be_repeated_without_doubling_the_slabs() {
    let deck = Deck {
        x: (2.0, 4.0),
        z: (2.0, 3.0),
        bottom: 4096,
        top: 5120,
    };
    let field = island(6, &[deck]);
    let mut w = world(vec![room("island", [0, 0], 6, 6)]);
    let (first, _) = run(&mut w, &field, &config());
    let (second, _) = run(&mut w, &field, &config());
    assert_eq!((first.removed, second.removed), (0, 1));
    assert_eq!(w.rooms[0].meshes.len(), 1);
}

#[test]
fn a_long_gantry_stretches_the_large_cube_or_is_cut_for_the_small_one() {
    // 20 sectors long, 2 wide: the large cube stretches to it.
    let wide = Deck {
        x: (2.0, 22.0),
        z: (2.0, 4.0),
        bottom: 6144,
        top: 7168,
    };
    let mut w = world(vec![room("island", [0, 0], 24, 6)]);
    run(&mut w, &island(24, &[wide]), &config());
    // The field is 24 square, the room 24 by 6: the deck stays in rows 2..4.
    assert_eq!(w.rooms[0].meshes.len(), 1);
    assert_eq!(
        w.mesh_assets[usize::from(w.rooms[0].meshes[0].asset)].name,
        LARGE_NAME
    );
    assert_eq!(
        boxes(&w),
        vec![(0, [2048, 6144, 2048], [22528, 7168, 4096])]
    );
    // A quarter sector wide it is cut into small pieces that meet.
    let thin = Deck {
        x: (2.0, 22.0),
        z: (2.0, 2.25),
        bottom: 6144,
        top: 7168,
    };
    let mut w = world(vec![room("island", [0, 0], 24, 6)]);
    run(&mut w, &island(24, &[thin]), &config());
    let placed = boxes(&w);
    assert_eq!(placed.len(), 3, "20 sectors in pieces of at most 8");
    assert!(w.mesh_assets.iter().all(|a| a.name == SMALL_NAME));
    assert_eq!(placed.iter().map(|b| b.1[0]).min(), Some(2048));
    assert_eq!(placed.iter().map(|b| b.2[0]).max(), Some(22528));
    let mut spans: Vec<(i32, i32)> = placed.iter().map(|b| (b.1[0], b.2[0])).collect();
    spans.sort_unstable();
    assert!(spans.windows(2).all(|p| p[0].1 == p[1].0), "{spans:?}");
}

#[test]
fn a_full_room_keeps_the_largest_slabs() {
    // 130 separate 1 sector decks in a checkerboard over a 29 by 29 island.
    let mut decks = Vec::new();
    for i in 0..130 {
        let (x, z) = ((i % 12) as f32 * 2.0 + 1.0, (i / 12) as f32 * 2.0 + 1.0);
        let grow = if i == 0 { 1.0 } else { 0.5 };
        decks.push(Deck {
            x: (x, x + grow),
            z: (z, z + grow),
            bottom: 4096,
            top: 5120,
        });
    }
    let field = island(28, &decks);
    let mut w = world(vec![room("island", [0, 0], 28, 28)]);
    let (report, _) = run(&mut w, &field, &config());
    assert_eq!(w.rooms[0].meshes.len(), MAX_MESHES_PER_ROOM);
    assert_eq!(report.rooms[0].dropped, 2, "{}", report.summary());
    // The big one survived.
    assert!(boxes(&w).iter().any(|(_, lo, hi)| hi[0] - lo[0] == 1024));
}

#[test]
fn a_slab_world_cooks_and_the_body_stands_on_the_deck() {
    use psx_engine::{
        Angle, CharacterMotorConfig, CharacterMotorInput, CharacterMotorState, RoomPoint,
    };
    use psx_grid::collision::{GridCollision, GridTraceProvider};
    use psx_grid::write::aligned_copy;
    use psx_grid::{CollisionView, LevelView};

    let deck = Deck {
        x: (2.0, 4.0),
        z: (2.0, 4.0),
        bottom: 3072,
        top: 4096,
    };
    let mut w = world(vec![room("island", [0, 0], 6, 6)]);
    run(&mut w, &island(6, &[deck]), &config());
    let cooked = cook(&w).unwrap();
    let leak = |bytes: &[u8]| -> &'static [u8] {
        let words: &'static mut [u32] = aligned_copy(bytes).leak();
        // SAFETY: the words live for the program and cover `bytes.len()` bytes.
        unsafe { core::slice::from_raw_parts(words.as_ptr().cast::<u8>(), bytes.len()) }
    };
    let level = LevelView::parse(leak(&cooked.core)).unwrap();
    let rooms: Vec<Option<CollisionView<'static>>> = cooked
        .chunks
        .iter()
        .map(|c| Some(CollisionView::parse(leak(c)).unwrap()))
        .collect();
    let rooms: &'static [Option<CollisionView<'static>>] = rooms.leak();
    let collision: GridCollision<'static> = psx_grid::ResidentRooms::from_collisions(level, rooms)
        .expect("a collision world")
        .leak_collision();
    let config =
        CharacterMotorConfig::character_with_body(12, 48, 8 << 8, 8 << 8, Angle::from_q12(1024));
    let mut motor = CharacterMotorState::new(RoomPoint::new(240, 400, 240), Angle::ZERO);
    for _ in 0..80 {
        let mut provider = GridTraceProvider::new(&collision);
        motor
            .update_vblanks_with_trace_provider(
                &mut provider,
                CharacterMotorInput::default(),
                config,
                1,
            )
            .unwrap();
    }
    // The deck's top is 4096 grid units = 320 runtime units (80 to a sector).
    let y = motor.position().y;
    assert!((y - 320).abs() <= 2, "stood at {y}");
    assert_eq!(SECTOR_UNITS, 1024);
}

#[test]
fn a_rough_deck_fits_sector_blocks_where_quarter_cells_would_shatter() {
    // A deck whose underside wobbles by a third of a sector from quarter cell
    // to quarter cell: one slab per sector block, not one per wobble.
    let field = HitField::from_fn(
        8 * COLUMNS_PER_SECTOR,
        8 * COLUMNS_PER_SECTOR,
        8,
        |row, col| {
            let (cx, cz) = (col / 4, row / 4);
            let (x, z) = (cx as f32 / 4.0, cz as f32 / 4.0);
            let wobble = ((cx * 7 + cz * 13) % 5) as i32 * 64;
            if (2.0..6.0).contains(&x) && (2.0..5.0).contains(&z) {
                vec![hit(5120 + wobble), hit(4096 + wobble), hit(512)]
            } else {
                vec![hit(512)]
            }
        },
    );
    let sectors = FloatConfig {
        origin_sector: [0, 0],
        ..FloatConfig::default()
    };
    let mut w = world(vec![room("island", [0, 0], 8, 8)]);
    let (report, _) = run(&mut w, &field, &sectors);
    assert_eq!(report.placed(), 1, "{}", report.summary());
    let (_, lo, hi) = boxes(&w)[0];
    assert_eq!((lo[0], hi[0], lo[2], hi[2]), (2048, 6144, 2048, 5120));
    // Heights are the median of the wobble, on the lattice.
    assert!(lo[1] % 64 == 0 && (4096..=4352).contains(&lo[1]), "{lo:?}");
}

#[test]
fn a_mass_thicker_than_a_deck_is_not_floated() {
    let tall = Deck {
        x: (2.0, 4.0),
        z: (2.0, 4.0),
        bottom: 4096,
        top: 4096 + 9 * 1024,
    };
    let field = island(6, &[tall]);
    let mut w = world(vec![room("island", [0, 0], 6, 6)]);
    let (report, cells) = run(&mut w, &field, &config());
    assert!(cells.is_empty());
    assert_eq!(report.placed(), 0);
}
