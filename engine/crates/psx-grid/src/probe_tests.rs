//! Light probes: the chunk format, the colour packing and the integer blend a
//! character is lit with.

extern crate std;

use crate::format::*;
use crate::probe::ProbeView;
use crate::resident::ResidentRooms;
use crate::view::{CollisionView, FormatError, LevelView, RoomView};
use crate::write::{aligned_copy, words_as_bytes, LevelCore, ProbeTables, RoomChunk};
use alloc::vec;
use alloc::vec::Vec;

const S: i32 = 80;

/// One room of a test level: where it is and what its probes hold.
struct TestRoom {
    /// Origin in sectors.
    origin: [i32; 2],
    width: u8,
    depth: u8,
    probes: Option<ProbeTables>,
}

/// Probes for a `width` by `depth` room from a function of the sector: the
/// floors (height, light colour) it has, bottom to top.
fn probes_with(
    width: usize,
    depth: usize,
    floors: impl Fn(usize, usize) -> Vec<(i16, [u8; 3])>,
) -> ProbeTables {
    let mut tables = ProbeTables::empty(width, depth);
    for cz in 0..depth {
        for cx in 0..width {
            let list: Vec<(i16, u16)> = floors(cx, cz)
                .into_iter()
                .map(|(y, rgb)| (y, probe_pack(rgb)))
                .collect();
            tables.set_cell(cz * width + cx, &list);
        }
    }
    tables
}

fn chunk_of(index: usize, room: &TestRoom) -> RoomChunk {
    let cells = usize::from(room.width) * usize::from(room.depth);
    RoomChunk {
        room_id: index as u16,
        width: room.width,
        depth: room.depth,
        sectors: vec![
            Sector {
                floor: [0; 4],
                ceil: [0; 4],
                flags: SECTOR_HAS_FLOOR,
                ext: NO_EXT,
            };
            cells
        ],
        cells: vec![RCell::default(); cells],
        probes: room.probes.clone(),
        ..RoomChunk::default()
    }
}

/// Run `f` over a resident set holding every room of `rooms` (collision and
/// probes), the way the guest holds a level.
fn with_rooms<R>(rooms: &[TestRoom], f: impl FnOnce(&ResidentRooms<'_>) -> R) -> R {
    let core = LevelCore {
        sector_units: S as u16,
        rooms: rooms
            .iter()
            .map(|r| RoomRecord {
                origin: [r.origin[0] * S, 0, r.origin[1] * S],
                width: r.width,
                depth: r.depth,
                ..RoomRecord::default()
            })
            .collect(),
        ..LevelCore::default()
    };
    let core_bytes = core.to_bytes();
    let core_words = aligned_copy(&core_bytes);
    let chunks: Vec<(Vec<u32>, usize)> = rooms
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let bytes = chunk_of(i, r).to_bytes();
            (aligned_copy(&bytes), bytes.len())
        })
        .collect();
    let level = LevelView::parse(words_as_bytes(&core_words, core_bytes.len())).unwrap();
    let mut set = ResidentRooms::new(level).unwrap();
    for (i, (words, len)) in chunks.iter().enumerate() {
        let bytes = words_as_bytes(words, *len);
        set.insert_collision(i, CollisionView::parse(bytes).unwrap())
            .unwrap();
        set.attach_probes(i, ProbeView::parse_chunk(bytes).unwrap());
    }
    f(&set)
}

/// A 4 by 4 room whose probes read `colour(cx, cz)` at a chest height.
fn grid_room(origin: [i32; 2], colour: impl Fn(usize, usize) -> [u8; 3]) -> TestRoom {
    TestRoom {
        origin,
        width: 4,
        depth: 4,
        probes: Some(probes_with(4, 4, |cx, cz| vec![(32, colour(cx, cz))])),
    }
}

/// A colour table of values the packing keeps exactly (multiples of eight).
fn varied(cx: usize, cz: usize) -> [u8; 3] {
    let n = (cx * 4 + cz) as u8;
    [8 * (n + 1), 8 * ((n * 5) % 31 + 1), 8 * ((n * 11) % 31 + 1)]
}

/// The bilinear blend in floating point, from the four decoded probes around
/// `(x, z)` of a room at the origin.
fn reference(colour: &dyn Fn(usize, usize) -> [u8; 3], x: i32, z: i32) -> [f64; 3] {
    let at = |cx: i32, cz: i32| -> [f64; 3] {
        let cx = cx.clamp(0, 3) as usize;
        let cz = cz.clamp(0, 3) as usize;
        probe_unpack(probe_pack(colour(cx, cz))).map(f64::from)
    };
    let u = f64::from(x - S / 2) / f64::from(S);
    let v = f64::from(z - S / 2) / f64::from(S);
    let (i, j) = (u.floor() as i32, v.floor() as i32);
    let (fu, fv) = (u - f64::from(i), v - f64::from(j));
    let mut out = [0.0; 3];
    for (di, dj, w) in [
        (0, 0, (1.0 - fu) * (1.0 - fv)),
        (1, 0, fu * (1.0 - fv)),
        (0, 1, (1.0 - fu) * fv),
        (1, 1, fu * fv),
    ] {
        let c = at(i + di, j + dj);
        for k in 0..3 {
            out[k] += w * c[k];
        }
    }
    out
}

#[test]
fn a_probe_colour_packs_to_five_bits_with_the_neutral_exact() {
    assert_eq!(probe_unpack(probe_pack([128; 3])), [128; 3]);
    assert_eq!(probe_unpack(probe_pack([0; 3])), [0; 3]);
    assert_eq!(probe_unpack(probe_pack([32; 3])), [32; 3]);
    assert_eq!(probe_unpack(probe_pack([255; 3])), [248; 3]);
    assert_eq!(probe_unpack(probe_pack([252; 3])), [248; 3]);
    for v in 0..=255u8 {
        let back = probe_unpack(probe_pack([v, v, v]))[0];
        assert!((i32::from(back) - i32::from(v)).abs() <= 7, "{v} -> {back}");
        if v <= 252 {
            assert!((i32::from(back) - i32::from(v)).abs() <= 4, "{v} -> {back}");
        }
    }
    // Bit 15 never belongs to the colour.
    assert_eq!(probe_pack([255; 3]) & PROBE_MULTI, 0);
}

#[test]
fn probes_round_trip_through_a_room_chunk() {
    let room = grid_room([0, 0], varied);
    let mut chunk = chunk_of(0, &room);
    // One sector with two floors, one without any.
    let probes = chunk.probes.as_mut().unwrap();
    probes.set_cell(5, &[(32, probe_pack([40; 3])), (112, probe_pack([200; 3]))]);
    probes.set_cell(6, &[]);
    let bytes = chunk.to_bytes();
    let words = aligned_copy(&bytes);
    let bytes = words_as_bytes(&words, bytes.len());
    // The whole chunk parses, with the probes beside the rest of the render
    // block.
    let view = RoomView::parse(bytes).unwrap();
    view.validate_full(0).unwrap();
    view.render.probes.validate_full().unwrap();
    assert_eq!(view.col.header.flags & ROOM_PROBES, ROOM_PROBES);
    assert_eq!(view.render.probes.cells.len(), 16);
    assert_eq!(view.render.probes.multi.len(), 2);
    let p = view.render.probes;
    assert_eq!(p.nearest(0, 0), Some((32, probe_pack(varied(0, 0)))));
    // The column of two floors answers with the floor nearest in height.
    assert_eq!(p.nearest(5, 0), Some((32, probe_pack([40; 3]))));
    assert_eq!(p.nearest(5, 200), Some((112, probe_pack([200; 3]))));
    assert_eq!(p.nearest(5, 80), Some((112, probe_pack([200; 3]))));
    assert_eq!(p.nearest(5, 60), Some((32, probe_pack([40; 3]))));
    // A pit has none.
    assert_eq!(p.nearest(6, 0), None);
    // The probes alone parse from the same bytes.
    let alone = ProbeView::parse_chunk(bytes).unwrap();
    assert_eq!(alone.cells, p.cells);
}

#[test]
fn a_room_without_probes_keeps_its_bytes_and_reads_none() {
    let mut chunk = chunk_of(0, &grid_room([0, 0], varied));
    chunk.probes = None;
    let bytes = chunk.to_bytes();
    let words = aligned_copy(&bytes);
    let bytes = words_as_bytes(&words, bytes.len());
    let view = RoomView::parse(bytes).unwrap();
    assert_eq!(view.col.header.flags & ROOM_PROBES, 0);
    assert!(view.render.probes.is_empty());
}

#[test]
fn a_probe_block_from_another_layout_is_refused() {
    let chunk = chunk_of(0, &grid_room([0, 0], varied));
    let mut bytes = chunk.to_bytes();
    let head: RoomHeader = *RoomView::parse(words_as_bytes(&aligned_copy(&bytes), bytes.len()))
        .unwrap()
        .col
        .header;
    bytes[head.render_offset as usize] ^= 0xFF;
    let words = aligned_copy(&bytes);
    assert_eq!(
        RoomView::parse(words_as_bytes(&words, bytes.len())).err(),
        Some(FormatError::BadLayout)
    );
}

#[test]
fn the_blend_at_a_sector_middle_is_that_sectors_probe() {
    with_rooms(&[grid_room([0, 0], varied)], |set| {
        for cz in 0..4 {
            for cx in 0..4 {
                let p = [cx as i32 * S + S / 2, 32, cz as i32 * S + S / 2];
                let want = probe_unpack(probe_pack(varied(cx, cz)));
                assert_eq!(set.probe_light(p), Some(want), "sector {cx},{cz}");
            }
        }
    });
}

#[test]
fn the_integer_blend_matches_the_float_reference_within_one_step() {
    with_rooms(&[grid_room([0, 0], varied)], |set| {
        let mut worst = 0.0f64;
        let mut checked = 0;
        // Every point of the room's inner area, on a fine lattice of the
        // sector (the outer half sector clamps to the edge probes).
        for x in (S / 2..4 * S - S / 2).step_by(3) {
            for z in (S / 2..4 * S - S / 2).step_by(3) {
                let got = set.probe_light([x, 32, z]).expect("inside the room");
                let want = reference(&varied, x, z);
                for k in 0..3 {
                    worst = worst.max((f64::from(got[k]) - want[k]).abs());
                }
                checked += 1;
            }
        }
        assert!(checked > 500);
        assert!(worst <= 1.0, "worst channel error {worst}");
    });
}

#[test]
fn the_outer_half_sector_holds_the_edge_probe_and_outside_the_grid_there_is_none() {
    with_rooms(&[grid_room([0, 0], varied)], |set| {
        // Near the west edge, mid sector in Z: the blend keeps the edge probe.
        let want = probe_unpack(probe_pack(varied(0, 1)));
        assert_eq!(set.probe_light([2, 32, S + S / 2]), Some(want));
        // Outside the footprint of every room.
        assert_eq!(set.probe_light([-5, 32, 10]), None);
        assert_eq!(set.probe_light([10_000, 32, 10]), None);
    });
}

#[test]
fn a_room_without_probes_gives_no_light() {
    let bare = TestRoom {
        origin: [0, 0],
        width: 4,
        depth: 4,
        probes: None,
    };
    with_rooms(&[bare], |set| {
        assert_eq!(set.probe_light([100, 32, 100]), None);
    });
}

#[test]
fn a_pit_borrows_its_neighbours_and_a_cliff_is_not_blended() {
    // A flat hall where sector (1, 1) is a pit and sector (3, 3) is a deep
    // pocket: its probe is three sectors lower.
    let colour = |cx: usize, cz: usize| -> [u8; 3] {
        if (cx, cz) == (3, 3) {
            [8; 3]
        } else {
            [160; 3]
        }
    };
    let room = TestRoom {
        origin: [0, 0],
        width: 4,
        depth: 4,
        probes: Some(probes_with(4, 4, |cx, cz| match (cx, cz) {
            (1, 1) => vec![],
            (3, 3) => vec![(32 - 3 * S as i16, colour(cx, cz))],
            _ => vec![(32, colour(cx, cz))],
        })),
    };
    with_rooms(&[room], |set| {
        // Over the pit: the neighbours' light, not nothing.
        assert_eq!(set.probe_light([S + S / 2, 32, S + S / 2]), Some([160; 3]));
        // Next to the pocket, on the upper floor: the dark pocket is a
        // different floor and does not leak in.
        let near = set.probe_light([3 * S - 10, 32, 3 * S - 10]);
        assert_eq!(near, Some([160; 3]));
    });
}

#[test]
fn a_column_of_two_floors_lights_each_floor_by_its_own_probe() {
    let room = TestRoom {
        origin: [0, 0],
        width: 2,
        depth: 2,
        probes: Some(probes_with(2, 2, |_, _| {
            vec![(32, [48; 3]), (32 + 2 * S as i16, [200; 3])]
        })),
    };
    with_rooms(&[room], |set| {
        let at = |y: i32| set.probe_light([S, y, S]).unwrap();
        assert_eq!(at(30), [48; 3]);
        assert_eq!(at(40), [48; 3]);
        assert_eq!(at(32 + 2 * S), [200; 3]);
        assert_eq!(at(32 + 2 * S + 15), [200; 3]);
        // Between the floors the nearer one wins; high above, the top one.
        assert_eq!(at(32 + S / 2), [48; 3]);
        assert_eq!(at(32 + 3 * S / 2), [200; 3]);
        assert_eq!(at(1000), [200; 3]);
    });
}

#[test]
fn the_light_is_smooth_across_a_room_seam() {
    // Two rooms side by side: a dark one and a bright one. A point right at
    // the seam is halfway, not clamped to the room it happens to be in.
    let dark = grid_room([0, 0], |_, _| [32; 3]);
    let bright = grid_room([4, 0], |_, _| [200; 3]);
    with_rooms(&[dark, bright], |set| {
        let at = |x: i32| set.probe_light([x, 32, S + S / 2]).unwrap()[0];
        let seam = at(4 * S);
        assert!((seam as i32 - 116).abs() <= 2, "{seam}");
        // Monotone through the seam, no step.
        let mut last = 0u8;
        for x in (3 * S + S / 2..4 * S + S / 2).step_by(1) {
            let v = at(x);
            assert!(v >= last, "x {x}: {v} < {last}");
            if x > 3 * S + S / 2 {
                assert!(i32::from(v) - i32::from(last) <= 3, "step at {x}");
            }
            last = v;
        }
        assert_eq!(at(3 * S + S / 2), 32);
        assert_eq!(at(4 * S + S / 2), 200);
    });
}

#[test]
fn stacked_rooms_are_told_apart_by_height() {
    // Two rooms over the same footprint: a cellar at y 0 and a hall 3 sectors
    // above it, each lit differently.
    let cellar = TestRoom {
        origin: [0, 0],
        width: 2,
        depth: 2,
        probes: Some(probes_with(2, 2, |_, _| vec![(32, [16; 3])])),
    };
    let hall = TestRoom {
        origin: [0, 0],
        width: 2,
        depth: 2,
        probes: Some(probes_with(2, 2, |_, _| vec![(32, [184; 3])])),
    };
    let core = LevelCore {
        sector_units: S as u16,
        rooms: vec![
            RoomRecord {
                origin: [0, 0, 0],
                width: 2,
                depth: 2,
                ..RoomRecord::default()
            },
            RoomRecord {
                origin: [0, 3 * S, 0],
                width: 2,
                depth: 2,
                ..RoomRecord::default()
            },
        ],
        ..LevelCore::default()
    };
    let core_bytes = core.to_bytes();
    let core_words = aligned_copy(&core_bytes);
    let chunks: Vec<(Vec<u32>, usize)> = [&cellar, &hall]
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let bytes = chunk_of(i, r).to_bytes();
            (aligned_copy(&bytes), bytes.len())
        })
        .collect();
    let level = LevelView::parse(words_as_bytes(&core_words, core_bytes.len())).unwrap();
    let mut set = ResidentRooms::new(level).unwrap();
    for (i, (words, len)) in chunks.iter().enumerate() {
        let bytes = words_as_bytes(words, *len);
        set.insert_collision(i, CollisionView::parse(bytes).unwrap())
            .unwrap();
        set.attach_probes(i, ProbeView::parse_chunk(bytes).unwrap());
    }
    assert_eq!(set.probe_light([S, 30, S]), Some([16; 3]));
    assert_eq!(set.probe_light([S, 3 * S + 30, S]), Some([184; 3]));
}

#[test]
fn the_resident_set_can_be_viewed_for_a_shorter_lifetime() {
    // The guest keeps its set for the rest of the run and hands a borrow of
    // it to the lighting view of each frame.
    fn shrink<'a>(set: &'a ResidentRooms<'static>) -> &'a ResidentRooms<'a> {
        set
    }
    let _ = shrink;
}
