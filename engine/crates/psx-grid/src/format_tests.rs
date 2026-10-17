//! The wire format: golden bytes of version 4, and the refusal of version 3.
//!
//! The golden files are the exact bytes the writer produces for the tables
//! below. A deliberate format change bumps `VERSION`, regenerates them with
//! `PGL_REGENERATE_GOLDEN=1 cargo test -p psx-grid golden` and keeps the old
//! version's files next to the new ones.

extern crate std;

use crate::format::*;
use crate::view::{FormatError, LevelView, RoomView};
use crate::write::{aligned_copy, words_as_bytes, LevelCore, RoomChunk};
use alloc::vec;
use alloc::vec::Vec;

const GOLDEN_CORE: &[u8] = include_bytes!("../tests/fixtures/pgl_v4_core.bin");
const GOLDEN_ROOM: &[u8] = include_bytes!("../tests/fixtures/pgl_v4_room.bin");

fn level() -> LevelCore {
    LevelCore {
        flags: LEVEL_HAS_SKY,
        sector_units: 80,
        max_room_pages: 1,
        draw_far: 48,
        active_radius: 24,
        lead_radius: 8,
        maxima: RenderMaxima {
            vertices: 4,
            cells: 1,
            quads: 2,
            lod_blocks: 0,
            lod_quads: 0,
        },
        rooms: vec![RoomRecord {
            pack_sector: 0,
            pack_sectors: 1,
            origin: [0, 0, 0],
            width: 1,
            depth: 1,
            min_y: 0,
            max_y: 80,
            ..RoomRecord::default()
        }],
        portals: Vec::new(),
        materials: vec![
            // A 64 by 64 texture that can repeat.
            Material {
                texture_asset: 3,
                flags: 0,
                blend: 0,
                uv_motion: 0,
                tex_shape: tex_shape(64, 64),
            },
            // An untextured stand-in.
            Material {
                texture_asset: 0,
                flags: 0,
                blend: 0,
                uv_motion: 0,
                tex_shape: 0,
            },
        ],
    }
}

fn room() -> RoomChunk {
    let vtx = |x: i16, y: i16, z: i16| Vtx { x, y, z, pad: 0 };
    let quad = |material: u8, flags2: u8, far: u16| Quad {
        v: [0, 1, 2, 3],
        color: [0x0080_8080; 4],
        uv: [0, far, far | (far << 8), far << 8],
        d: -80,
        n: [-127, 0, 0],
        material,
        flags: QUAD_AXIS | QUAD_AXIS_NEG,
        flags2,
    };
    RoomChunk {
        width: 1,
        depth: 1,
        min_y: 0,
        max_y: 80,
        sectors: vec![Sector {
            floor: [0; 4],
            ceil: [80; 4],
            flags: SECTOR_HAS_FLOOR | SECTOR_HAS_CEIL,
            ext: NO_EXT,
        }],
        vertices: vec![
            vtx(80, 0, 0),
            vtx(80, 0, 80),
            vtx(80, 80, 80),
            vtx(80, 80, 0),
        ],
        quads: vec![quad(0, 0, 63), quad(0, QUAD2_REPEAT, 255)],
        cells: vec![RCell {
            first: 0,
            count: 2,
            flags: 0,
            min_y: 0,
            max_y: 80,
        }],
        material_use: vec![0],
        ..RoomChunk::default()
    }
}

#[test]
fn golden_v4_bytes() {
    let (core, chunk) = (level().to_bytes(), room().to_bytes());
    if std::env::var("PGL_REGENERATE_GOLDEN").is_ok() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        std::fs::write(dir.join("pgl_v4_core.bin"), &core).unwrap();
        std::fs::write(dir.join("pgl_v4_room.bin"), &chunk).unwrap();
    }
    assert_eq!(core, GOLDEN_CORE, "level core bytes changed");
    assert_eq!(chunk, GOLDEN_ROOM, "room chunk bytes changed");
}

#[test]
fn the_golden_files_read_back() {
    let words = aligned_copy(GOLDEN_CORE);
    let level = LevelView::parse(words_as_bytes(&words, GOLDEN_CORE.len())).unwrap();
    assert_eq!(level.header.version, 4);
    assert_eq!(level.header.min_reader, 4);
    assert_eq!(tex_size(level.materials[0].tex_shape), Some([64, 64]));
    assert_eq!(tex_size(level.materials[1].tex_shape), None);
    let words = aligned_copy(GOLDEN_ROOM);
    let room = RoomView::parse(words_as_bytes(&words, GOLDEN_ROOM.len())).unwrap();
    room.validate_full(level.materials.len()).unwrap();
    room.validate_repeat(level.materials).unwrap();
    assert_eq!(room.render.quads[0].flags2, 0);
    assert_eq!(room.render.quads[1].flags2, QUAD2_REPEAT);
    assert_eq!(room.col.header.version, 4);
}

#[test]
fn a_repeat_quad_on_a_material_without_a_shape_is_refused() {
    let words = aligned_copy(GOLDEN_ROOM);
    let room = RoomView::parse(words_as_bytes(&words, GOLDEN_ROOM.len())).unwrap();
    let mut materials = level().materials;
    materials[0].tex_shape = 0;
    assert_eq!(
        room.validate_repeat(&materials),
        Err(FormatError::Inconsistent)
    );
}

#[test]
fn unknown_second_flag_bits_are_refused() {
    let mut chunk = room();
    chunk.quads[0].flags2 = 0x80;
    let words = aligned_copy(&chunk.to_bytes());
    let bytes = words_as_bytes(&words, words.len() * 4);
    let view = RoomView::parse(bytes).unwrap();
    assert_eq!(view.validate_full(2), Err(FormatError::IndexRange));
}

/// Rewrite the version fields of golden bytes to 3, as a file cooked before
/// the streaming-ready core would carry.
fn as_v3(bytes: &[u8], version_at: usize, min_reader_at: Option<usize>) -> Vec<u8> {
    let mut out = bytes.to_vec();
    out[version_at..version_at + 2].copy_from_slice(&3u16.to_le_bytes());
    if let Some(at) = min_reader_at {
        out[at..at + 2].copy_from_slice(&3u16.to_le_bytes());
    }
    out
}

#[test]
fn version_3_files_are_refused_with_a_clear_error() {
    // Level core: magic(4) version(2) min_reader(2).
    let core = as_v3(GOLDEN_CORE, 4, Some(6));
    let words = aligned_copy(&core);
    let error = LevelView::parse(words_as_bytes(&words, core.len())).err();
    assert_eq!(error, Some(FormatError::BadVersion));
    // Room chunk: magic(4) version(2).
    let chunk = as_v3(GOLDEN_ROOM, 4, None);
    let words = aligned_copy(&chunk);
    let error = RoomView::parse(words_as_bytes(&words, chunk.len())).err();
    assert_eq!(error, Some(FormatError::BadVersion));
    // The message says what to do about it.
    let text = std::format!("{}", FormatError::BadVersion);
    assert!(
        text.contains("version 4") && text.contains("cook"),
        "{text}"
    );
}

#[test]
fn the_layout_hash_covers_the_new_fields() {
    // A reader built against the version 2 layout (no tex_shape, no flags2
    // offsets in the hash) computed another value; pin the current one so a
    // layout change cannot slip by unnoticed.
    assert_eq!(LAYOUT_HASH, level_header_hash(GOLDEN_CORE));
    assert_eq!(tex_shape(64, 64), 0x66);
    assert_eq!(tex_shape(128, 8), 0x37);
    assert_eq!(tex_shape(48, 64), 0);
    assert_eq!(tex_shape(256, 64), 0);
    assert_eq!(tex_size(0x66), Some([64, 64]));
    assert_eq!(tex_size(0x37), Some([128, 8]));
    assert_eq!(tex_size(0), None);
    assert_eq!(tex_size(0x1066), None);
}

fn level_header_hash(core: &[u8]) -> u32 {
    // layout_hash is the last u32 of the 52 byte header.
    u32::from_le_bytes([core[48], core[49], core[50], core[51]])
}

#[test]
fn a_mover_has_a_stable_id_that_is_neither_its_room_nor_its_index() {
    // `room << 8 | local` stops working at 256 rooms and renumbers when a
    // room is cut; the id is a hash of the authored name, stored in the
    // record, so the same door is the same id in any room of any cook.
    let door = MoverId::from_name("gate door");
    let mut a = Mover {
        kind: mover_kind::DOOR,
        id: 3,
        ..Mover::default()
    };
    a.set_stable_id(door);
    assert_eq!(a.stable_id(), door);
    assert_eq!(a.stable, [door.0 as u16, (door.0 >> 16) as u16]);
    // The local index is only an index inside the chunk and can change.
    let b = Mover { id: 9, ..a };
    assert_eq!(b.stable_id(), door);
    assert_ne!(door, MoverId::from_name("gate door 2"));
    // FNV-1a, the same hash as `psx_level::StableId` so one name gives one id.
    assert_eq!(MoverId::from_name("a").0, 0xe40c_292c);
    // The 32-byte record did not grow, and a chunk round trips the id.
    assert_eq!(core::mem::size_of::<Mover>(), 32);
    let mut chunk = room();
    chunk.movers = vec![a];
    let words = aligned_copy(&chunk.to_bytes());
    let bytes = words_as_bytes(&words, words.len() * 4);
    let view = RoomView::parse(bytes).unwrap();
    assert_eq!(view.col.movers[0].stable_id(), door);
}
