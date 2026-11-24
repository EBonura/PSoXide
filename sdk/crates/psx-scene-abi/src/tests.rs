extern crate std;

use super::*;
use std::string::String;
use std::vec;
use std::vec::Vec;

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

#[test]
fn memory_map_is_the_day_0_page() {
    assert_eq!(CORE_WINDOW.start, 0x801f_df00);
    assert_eq!(CORE_WINDOW.end, 0x801f_ff00);
    assert_eq!(CORE_WINDOW.len(), 8_192);
    assert_eq!(CORE_WINDOW.end, HANDOFF_BLOCK.start);
    assert_eq!(HANDOFF_BLOCK.len(), 256);
    assert_eq!(HANDOFF_BLOCK.end, 0x8020_0000);
    assert_eq!(STACK_INIT, 0x801f_df00);
    // psoxide.ld: STACK_INIT = RAM_BASE + RAM_SIZE - 0x100.
    assert_eq!(RAM_SIZE_OVERRIDE, 0x1f_e000);
    assert_eq!(RAM_BASE + RAM_SIZE_OVERRIDE - 0x100, STACK_INIT);
    assert_eq!(PAYLOAD_END, 0x801f_db00);
    assert!(CORE_WINDOW.holds(0x801f_df00, 0x801f_ff00));
    assert!(!CORE_WINDOW.holds(0x801f_defc, 0x801f_e000));
    assert!(!CORE_WINDOW.holds(0x801f_fe00, 0x801f_ff04));
    assert!(!CORE_WINDOW.holds(0x801f_e000, 0x801f_e000));
}

/// The linker script's own words, whitespace folded.
fn psoxide_ld() -> String {
    include_str!("../../../psoxide.ld")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn the_constants_match_psoxide_ld() {
    let script = psoxide_ld();
    // RAM_SIZE_OVERRIDE lands STACK_INIT on the window only through this formula.
    assert!(script.contains("RAM_SIZE = DEFINED(RAM_SIZE_OVERRIDE) ? RAM_SIZE_OVERRIDE : 2M;"));
    assert!(script.contains("STACK_INIT = RAM_BASE + RAM_SIZE - 0x100;"));
    assert!(script.contains("RAM_BASE = 0x80000000;"));
    // Every scene loads at the default LOAD_ADDR; the override is for boot programs.
    assert!(script.contains(
        "LOAD_ADDR = DEFINED(LOAD_ADDR_OVERRIDE) ? LOAD_ADDR_OVERRIDE : RAM_BASE + BIOS_SIZE;"
    ));
    assert!(script.contains("BIOS_SIZE = 64K;"));
    assert_eq!(LOAD_ADDR, RAM_BASE + 64 * 1024);
    // The image region ends at the stack reserve below STACK_INIT, so nothing a hosting program
    // links (text, data, bss) can reach the window.
    assert!(script
        .contains("LENGTH = EXE_HEAD_BYTES + RAM_BASE + RAM_SIZE - LOAD_ADDR - STACK_RESERVE"));
}

#[test]
fn the_absolute_flag_is_clear_of_every_cd_lba() {
    // 80 minutes of 75 sectors a second is the most a CD holds.
    let last_lba: u32 = 80 * 60 * 75;
    assert_eq!(last_lba & LBA_ABSOLUTE, 0);
    assert_eq!((last_lba | LBA_ABSOLUTE) & !LBA_ABSOLUTE, last_lba);
    assert_eq!(LBA_ABSOLUTE & STREAM_IMPLICIT_SEEK, 0);
}

#[test]
fn jump_table_slots_are_the_day_0_addresses() {
    let slots = [
        slot::ATTACH,
        slot::SWITCH,
        slot::READ,
        slot::PAUSE,
        slot::CLIENT_ATTACH,
        slot::CLIENT_DETACH,
        slot::FATAL,
        slot::RELEASE,
        slot::STREAM_OPEN,
        slot::STREAM_SECTOR,
        slot::CD,
        slot::SET_IDLE_HOOK,
        slot::LOAD_IMAGE,
    ];
    let want = [
        0x801f_df00,
        0x801f_df08,
        0x801f_df10,
        0x801f_df18,
        0x801f_df20,
        0x801f_df28,
        0x801f_df30,
        0x801f_df38,
        0x801f_df40,
        0x801f_df48,
        0x801f_df50,
        0x801f_df58,
        0x801f_df60,
    ];
    assert_eq!(slots, want);
    assert_eq!(slots.len(), slot::COUNT);
    assert_eq!(slot::TABLE_WORD, slot::LOAD_IMAGE + slot::SLOT_BYTES);
    assert_eq!(slot::TABLE_END, slot::TABLE_WORD + 4);
}

#[test]
fn every_slot_has_a_depth() {
    // The table is the slots in order, one per 8 bytes from the window's start, to its end.
    for (i, row) in stack::SLOTS.iter().enumerate() {
        assert_eq!(
            row.addr,
            CORE_WINDOW.start + i as u32 * slot::SLOT_BYTES,
            "{}",
            row.name
        );
        assert_eq!(stack::slot_at(row.addr), Some(*row));
    }
    assert_eq!(
        stack::SLOTS.last().unwrap().addr + slot::SLOT_BYTES,
        slot::TABLE_WORD
    );
    let depths = [
        (slot::ATTACH, stack::ATTACH),
        (slot::SWITCH, stack::SWITCH),
        (slot::READ, stack::READ),
        (slot::PAUSE, stack::PAUSE),
        (slot::CLIENT_ATTACH, stack::CLIENT_ATTACH),
        (slot::CLIENT_DETACH, stack::CLIENT_DETACH),
        (slot::FATAL, stack::FATAL),
        (slot::RELEASE, stack::RELEASE),
        (slot::STREAM_OPEN, stack::STREAM_OPEN),
        (slot::STREAM_SECTOR, stack::STREAM_SECTOR),
        (slot::CD, stack::CD),
        (slot::SET_IDLE_HOOK, stack::SET_IDLE_HOOK),
        (slot::LOAD_IMAGE, stack::LOAD_IMAGE),
    ];
    assert_eq!(depths.len(), slot::COUNT);
    for (addr, depth) in depths {
        assert_eq!(
            stack::slot_at(addr).map(|s| s.depth),
            Some(depth),
            "{addr:08x}"
        );
    }
    // The switch charges its caller only what it uses before it moves to its own stack.
    assert_eq!(stack::slot_at(slot::SWITCH).unwrap().depth, stack::SWITCH);
    assert!(stack::SLOTS.iter().all(|s| s.depth != stack::SWITCH_LOAD));
    // Only the read takes a per-sector hook; a slot with no frame cannot call a hook.
    for row in stack::SLOTS {
        assert_eq!(row.sector_hook, row.addr == slot::READ, "{}", row.name);
        assert!(
            row.depth > 0 || !(row.idle_hook || row.sector_hook),
            "{}",
            row.name
        );
    }
    // Between slots and past the table there is no slot.
    for addr in [
        slot::ATTACH + 4,
        slot::READ + 1,
        slot::TABLE_END,
        CORE_WINDOW.end - 8,
        0,
    ] {
        assert_eq!(stack::slot_at(addr), None, "{addr:08x}");
    }
}

#[test]
fn max_is_the_deepest_caller_stack_slot() {
    let deepest = stack::SLOTS.iter().map(|s| s.depth).max().unwrap();
    assert_eq!(stack::MAX, deepest);
}

/// psx-core's `link` feature defines each slot symbol from these constants (psoxide-core.ld no
/// longer lists them): every slot's name must be one of its `.set` lines.
#[test]
fn the_slot_names_are_the_ones_psx_core_defines() {
    let link = include_str!("../../psx-core/src/link.rs");
    for row in stack::SLOTS {
        let line = std::format!("\".set {}, {{", row.name);
        assert!(link.contains(&line), "{line}");
    }
}

#[test]
fn the_cd_slot_op_codes_are_the_abi() {
    let ops = [
        cd_op::COMMAND,
        cd_op::COMMAND_NO_WAIT,
        cd_op::IRQ_FLAG,
        cd_op::IRQ_ACK,
        cd_op::MIXER,
        cd_op::IRQ_ENABLE,
        cd_op::RESPONSE,
    ];
    assert_eq!(ops, [0, 1, 2, 3, 4, 5, 6]);
    let outcomes = [cd_op::ACKNOWLEDGED, cd_op::DRIVE_ERROR, cd_op::TIMEOUT];
    assert_eq!(outcomes, [0, 1, 2]);
}

#[test]
fn handoff_block_magic_and_offsets() {
    assert_eq!(block::MAGIC.to_le_bytes(), *b"PSCB");
    assert_eq!(block::MAGIC_INV, 0xbdbc_acaf);
    assert_eq!(block::OFF_MAGIC_INV + 4, HANDOFF_BLOCK.len() as usize);
    assert_eq!(block::OFF_SETTINGS + block::SETTINGS_BYTES, 0x70);
    assert_eq!(
        block::OFF_PRIVATE + block::PRIVATE_BYTES,
        block::OFF_MAGIC_INV
    );
    assert_eq!(usize::from(block::LENGTH), HANDOFF_BLOCK.len() as usize);
}
#[test]
fn word_fnv_is_the_sdk_word_fnv() {
    let bytes: Vec<u8> = (0..400u32).map(|i| (i * 37 + 11) as u8).collect();
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|w| u32::from_le_bytes(w.try_into().unwrap()))
        .collect();
    assert_eq!(word_fnv(&bytes), psx_pack::WordFnv32::of(&words));
    assert_eq!(word_fnv(&[]), 0x811c_9dc5);
}

#[test]
fn block_version_2_adds_the_loaded_mask_and_the_in_program_flag() {
    assert_eq!(block::VERSION, 2);
    assert_eq!(block::OFF_LOADED_MASK, 0x74);
    // Clear of the partner credit's u16 before it and the private area after it.
    const { assert!(block::OFF_PARTNER_CREDIT + 2 <= block::OFF_LOADED_MASK) };
    const { assert!(block::OFF_LOADED_MASK + 4 <= block::OFF_PRIVATE) };
    assert_eq!(block::FLAG_IN_PROGRAM, 4);
    assert_eq!(
        block::FLAG_CORE_ENTRY | block::FLAG_NESTED | block::FLAG_IN_PROGRAM,
        0b111
    );
}

fn sample_scene(id: u16) -> SceneRow {
    SceneRow {
        id,
        image: 0x02,
        flags: SCENE_FLAG_PARTNER_CREDIT | SCENE_FLAG_START,
        entry: 0x8001_2345,
        partner_credit: 0xe1e2,
        min_block_version: 0xd2,
        needs: 0x0000_0006,
        name: SceneRow::name_bytes("A_SIXTEEN_BYTE_N"),
    }
}

fn sample_image(kind: Kind) -> ImageRow {
    ImageRow {
        lba: 0x0102_0304 | LBA_ABSOLUTE,
        size: 0x0004_0800,
        load_addr: 0x8001_0000,
        checksum: 0xa1a2_a3a4,
        core_hash: 0xb1b2_b3b4,
        evicts: 0xc1c2_c3c4,
        member_base: 0x0a0b_0c0d,
        kind,
        class: match kind {
            Kind::Program => None,
            Kind::Overlay => Some(Class::Swap),
        },
        owner: 0x1e,
        cdda_delta: 0x1f,
    }
}

#[test]
fn header_round_trips_at_the_v3_offsets() {
    let header = Header {
        image_count: 0x0003,
        scene_count: 0x0005,
        boot_scene: 0x0004,
        checksum: 0x0607_0809,
        compact: false,
    };
    let mut bytes = [0xeeu8; HEADER_BYTES];
    header.encode(&mut bytes);
    assert_eq!(&bytes[0x00..0x08], b"PSXSCN03");
    assert_eq!(le16(&bytes, 0x08), 3);
    assert_eq!(le16(&bytes, 0x0a), 3);
    assert_eq!(le16(&bytes, 0x0c), 5);
    assert_eq!(le16(&bytes, 0x0e), 32);
    assert_eq!(le16(&bytes, 0x10), 4);
    assert_eq!(le16(&bytes, 0x12), 1);
    assert_eq!(le32(&bytes, 0x14), 0x0607_0809);
    assert!(bytes[0x18..0x40].iter().all(|&b| b == 0));
    assert_eq!(Header::decode(&bytes), Ok(header));

    // Version 1, another magic, another version or shape: refused with the core's 0x21.
    let mut v1 = bytes;
    v1[..8].copy_from_slice(b"PSXSCN01");
    assert_eq!(Header::decode(&v1), Err(TableError::Version1));
    assert_eq!(TableError::Version1.status(), Some(0x21));
    let mut v2 = v1;
    v2[..8].copy_from_slice(b"PSXSCN02");
    assert_eq!(Header::decode(&v2), Err(TableError::Version(2)));
    let mut other = bytes;
    other[0] = b'X';
    assert_eq!(Header::decode(&other), Err(TableError::Magic));
    let mut version = bytes;
    version[0x08] = 4;
    assert_eq!(Header::decode(&version), Err(TableError::Version(4)));
    let mut compact = bytes;
    compact[0x18] = 1;
    assert_eq!(
        Header::decode(&compact),
        Ok(Header {
            compact: true,
            ..header
        })
    );
    for at in [0x0e, 0x12, 0x18, 0x19] {
        let mut shape = bytes;
        shape[at] = shape[at].wrapping_add(2);
        assert_eq!(Header::decode(&shape), Err(TableError::Shape), "{at:#x}");
        assert_eq!(TableError::Shape.status(), Some(status::TABLE));
    }
    // Counts past the limits and a boot scene past the scenes.
    for (images, scenes, want) in [
        (0, 5, TableError::ImageCount(0)),
        (33, 5, TableError::ImageCount(33)),
        (3, 0, TableError::NoScenes),
        (32, 31, TableError::TooManyRows(63)),
        (3, 4, TableError::NoBootScene(4)),
    ] {
        let mut b = bytes;
        b[0x0a..0x0c].copy_from_slice(&u16::to_le_bytes(images));
        b[0x0c..0x0e].copy_from_slice(&u16::to_le_bytes(scenes));
        assert_eq!(Header::decode(&b), Err(want));
    }
    assert_eq!(status::TABLE, 0x21);
    assert_eq!(status::NOT_LOADED, 0x25);
}

#[test]
fn scene_row_round_trips_at_the_v3_offsets() {
    let row = sample_scene(7);
    let mut bytes = [0xeeu8; ROW_BYTES];
    row.encode(&mut bytes);
    assert_eq!(le16(&bytes, 0x00), 7);
    assert_eq!(bytes[0x02], 0x02);
    assert_eq!(bytes[0x03], 0b11);
    assert_eq!(le32(&bytes, 0x04), 0x8001_2345);
    assert_eq!(le16(&bytes, 0x08), 0xe1e2);
    assert_eq!(bytes[0x0a], 0xd2);
    assert_eq!(bytes[0x0b], 0);
    assert_eq!(le32(&bytes, 0x0c), 6);
    assert_eq!(&bytes[0x10..0x20], b"A_SIXTEEN_BYTE_N");
    assert_eq!(SceneRow::decode(&bytes), row);
    assert_eq!(row.name_str(), "A_SIXTEEN_BYTE_N");
    assert!(row.starts_program());
    let short = SceneRow {
        flags: SCENE_FLAG_PARTNER_CREDIT,
        name: SceneRow::name_bytes("MENU"),
        ..row
    };
    short.encode(&mut bytes);
    assert!(bytes[0x14..0x20].iter().all(|&b| b == 0));
    assert_eq!(SceneRow::decode(&bytes).name_str(), "MENU");
    assert!(!short.starts_program());
    assert_eq!((SCENE_FLAG_PARTNER_CREDIT, SCENE_FLAG_START), (1, 2));
}

#[test]
fn image_row_round_trips_at_the_v2_offsets() {
    for kind in [Kind::Program, Kind::Overlay] {
        let row = sample_image(kind);
        let mut bytes = [0xeeu8; ROW_BYTES];
        row.encode(&mut bytes);
        assert_eq!(le32(&bytes, 0x00), 0x8102_0304);
        assert_eq!(le32(&bytes, 0x04), 0x0004_0800);
        assert_eq!(le32(&bytes, 0x08), 0x8001_0000);
        assert_eq!(le32(&bytes, 0x0c), 0xa1a2_a3a4);
        assert_eq!(le32(&bytes, 0x10), 0xb1b2_b3b4);
        assert_eq!(le32(&bytes, 0x14), 0xc1c2_c3c4);
        assert_eq!(le32(&bytes, 0x18), 0x0a0b_0c0d);
        assert_eq!(bytes[0x1c], kind as u8);
        assert_eq!(bytes[0x1d], if kind == Kind::Program { 0 } else { 2 });
        assert_eq!(bytes[0x1e], 0x1e);
        assert_eq!(bytes[0x1f], 0x1f);
        assert_eq!(ImageRow::decode(&bytes), Some(row));
        assert_eq!(
            row.span(),
            Span {
                start: 0x8001_0000,
                end: 0x8005_0800
            }
        );
    }
    for (kind, byte) in [(Kind::Program, 0), (Kind::Overlay, 1)] {
        assert_eq!(Kind::from_u8(byte), Some(kind));
        assert_eq!(kind as u8, byte);
    }
    for (class, byte) in [
        (Class::ResidentSet, 1),
        (Class::Swap, 2),
        (Class::Volatile, 3),
    ] {
        assert_eq!(Class::from_u8(byte), Some(class));
        assert_eq!(class as u8, byte);
    }
    assert_eq!(Class::from_u8(0), None);
    // A VOLATILE row round-trips.
    let mut volatile = sample_image(Kind::Overlay);
    volatile.class = Some(Class::Volatile);
    let mut bytes = [0u8; ROW_BYTES];
    volatile.encode(&mut bytes);
    assert_eq!(bytes[0x1d], 3);
    assert_eq!(ImageRow::decode(&bytes), Some(volatile));
    // An unknown kind, an Overlay without a class, a Program with one: refused.
    let mut bytes = [0u8; ROW_BYTES];
    sample_image(Kind::Overlay).encode(&mut bytes);
    for (at, byte) in [(0x1c, 2), (0x1d, 0), (0x1d, 4)] {
        let mut bad = bytes;
        bad[at] = byte;
        assert_eq!(ImageRow::decode(&bad), None, "{at:#x}={byte}");
    }
    sample_image(Kind::Program).encode(&mut bytes);
    bytes[0x1d] = 1;
    assert_eq!(ImageRow::decode(&bytes), None);
}

#[test]
fn scenes_come_first_and_images_follow_inside_one_sector() {
    assert_eq!(MAX_ROWS, 62);
    assert_eq!(MAX_IMAGES, 32);
    assert_eq!(scene_row_offset(0), 0x40);
    assert_eq!(scene_row_offset(3), 0x40 + 3 * 32);
    assert_eq!(image_row_offset(5, 0), 0x40 + 5 * 32);
    assert_eq!(image_row_offset(30, 31), 0x40 + 61 * 32);
    assert_eq!(image_row_offset(30, 31) + ROW_BYTES, SECTOR_BYTES);
    assert_eq!(BOOT_EXE_LBA, TABLE_LBA + u32::from(TABLE_SECTORS));
    assert_eq!(BOOT_IMAGE, 0);
}

fn sample_table(scenes: usize, images: usize) -> (Vec<SceneRow>, Vec<ImageRow>) {
    let scenes = (0..scenes as u16)
        .map(|id| SceneRow {
            image: (id as usize % images) as u8,
            ..sample_scene(id)
        })
        .collect();
    let images = (0..images)
        .map(|i| ImageRow {
            lba: 23 + i as u32,
            kind: if i % 2 == 0 {
                Kind::Program
            } else {
                Kind::Overlay
            },
            class: (i % 2 == 1).then_some(Class::ResidentSet),
            ..sample_image(Kind::Program)
        })
        .collect();
    (scenes, images)
}

#[test]
fn a_table_encodes_checks_and_reads_back() {
    let (scenes, images) = sample_table(5, 3);
    let mut out = vec![0xffu8; 2 * SECTOR_BYTES];
    let len = encode_table(&scenes, &images, 2, &mut out).unwrap();
    assert_eq!(len, SECTOR_BYTES);
    assert!(
        out[SECTOR_BYTES..].iter().all(|&b| b == 0xff),
        "one sector only"
    );
    let table = &out[..len];
    let header = decode_header_checked(table).unwrap();
    assert_eq!(
        (header.scene_count, header.image_count, header.boot_scene),
        (5, 3, 2)
    );
    let rows_end = HEADER_BYTES + 8 * ROW_BYTES;
    let words: Vec<u32> = table[HEADER_BYTES..rows_end]
        .chunks_exact(4)
        .map(|w| u32::from_le_bytes(w.try_into().unwrap()))
        .collect();
    assert_eq!(words.len(), (5 + 3) * 8);
    assert_eq!(header.checksum, psx_pack::WordFnv32::of(&words));
    for (id, scene) in scenes.iter().enumerate() {
        assert_eq!(table_scene(table, &header, id as u16), Some(*scene));
        let at = scene_row_offset(id as u16);
        assert_eq!(le16(table, at), id as u16);
    }
    for (index, image) in images.iter().enumerate() {
        assert_eq!(table_image(table, &header, index as u8), Some(*image));
        assert_eq!(le32(table, 0x40 + (5 + index) * 32), 23 + index as u32);
    }
    assert_eq!(table_scene(table, &header, 5), None);
    assert_eq!(table_image(table, &header, 3), None);
    assert!(table[rows_end..].iter().all(|&b| b == 0));
    // A flipped row byte fails the checksum, which the core refuses as a bad table.
    let mut bad = table.to_vec();
    bad[rows_end - 1] ^= 1;
    assert_eq!(decode_header_checked(&bad), Err(TableError::Checksum));
    assert_eq!(TableError::Checksum.status(), Some(0x21));
    assert_eq!(
        decode_header_checked(&table[..100]),
        Err(TableError::ShortBuffer)
    );
    // The full table: 30 scenes and 32 images fill the sector exactly.
    let (scenes, images) = sample_table(30, 32);
    let len = encode_table(&scenes, &images, 0, &mut out).unwrap();
    let header = decode_header_checked(&out[..len]).unwrap();
    assert_eq!(table_image(&out, &header, 31), Some(images[31]));
}

#[test]
fn a_table_refuses_bad_rows() {
    let mut out = vec![0u8; SECTOR_BYTES];
    let (scenes, images) = sample_table(3, 2);
    assert_eq!(
        encode_table(&[], &images, 0, &mut out),
        Err(TableError::NoScenes)
    );
    assert_eq!(
        encode_table(&scenes, &[], 0, &mut out),
        Err(TableError::ImageCount(0))
    );
    let (s, i) = sample_table(3, 33);
    assert_eq!(
        encode_table(&s, &i, 0, &mut out),
        Err(TableError::ImageCount(33))
    );
    let (s, i) = sample_table(31, 32);
    assert_eq!(
        encode_table(&s, &i, 0, &mut out),
        Err(TableError::TooManyRows(63))
    );
    let mut wrong_id = scenes.clone();
    wrong_id[1].id = 5;
    assert_eq!(
        encode_table(&wrong_id, &images, 0, &mut out),
        Err(TableError::IdNotIndex { index: 1, id: 5 })
    );
    let mut wrong_image = scenes.clone();
    wrong_image[2].image = 2;
    assert_eq!(
        encode_table(&wrong_image, &images, 0, &mut out),
        Err(TableError::NoImage { scene: 2, image: 2 })
    );
    assert_eq!(
        encode_table(&scenes, &images, 3, &mut out),
        Err(TableError::NoBootScene(3))
    );
    assert_eq!(
        encode_table(&scenes, &images, 0, &mut out[..2047]),
        Err(TableError::ShortBuffer)
    );
    assert_eq!(TableError::NoScenes.status(), None);
}

#[test]
fn the_v1_shim_is_the_version_1_layout() {
    assert_eq!(&v1::MAGIC, b"PSXSCN01");
    assert_eq!((v1::VERSION, v1::BLOCK_VERSION), (1, 1));
    assert_eq!((v1::HEADER_BYTES, v1::ROW_BYTES), (64, 64));
    assert_eq!(v1::rows_capacity(1), 31);
    assert_eq!(v1::Kind::Program as u8, 0);
}

#[test]
fn a_compact_copy_keeps_scene_ids_and_mask_bits() {
    // Six scenes over four images; a program reaches scenes 4 and 1 and images 0 and 2.
    let (scenes, images) = sample_table(6, 4);
    let mut disc = vec![0u8; SECTOR_BYTES];
    encode_table(&scenes, &images, 3, &mut disc).unwrap();
    assert_eq!((scenes[1].image, scenes[4].image), (1, 0));
    let mut scenes_reached = [4u16, 1];
    // Scene 1 runs in image 1, which the mask leaves out: refused.
    let mut out = vec![0xaau8; SECTOR_BYTES];
    assert_eq!(
        encode_compact(&disc, &scenes_reached, 0b0101, &mut out),
        Err(TableError::NoImage { scene: 1, image: 1 })
    );
    scenes_reached[1] = 2; // image 2
    let len = encode_compact(&disc, &scenes_reached, 0b0101, &mut out).unwrap();
    assert_eq!(len, 64 + 32 * (2 + 3));
    assert_eq!(len, compact_bytes(2, 0b0101));
    let copy = &out[..len];
    let header = decode_header_checked(copy).unwrap();
    assert!(header.compact);
    assert_eq!(le16(copy, 0x18), HEADER_FLAG_COMPACT);
    assert_eq!(
        (header.scene_count, header.image_count, header.boot_scene),
        (2, 3, 3)
    );
    // Scenes by id, ascending; rows byte-for-byte the disc's.
    assert_eq!(le16(copy, scene_row_offset(0)), 2);
    assert_eq!(le16(copy, scene_row_offset(1)), 4);
    assert_eq!(
        &copy[scene_row_offset(1)..scene_row_offset(1) + 32],
        &disc[scene_row_offset(4)..scene_row_offset(4) + 32]
    );
    assert_eq!(table_scene(copy, &header, 4), Some(scenes[4]));
    assert_eq!(table_scene(copy, &header, 2), Some(scenes[2]));
    for id in [0, 1, 3, 5, 99] {
        assert_eq!(table_scene(copy, &header, id), None, "scene {id}");
    }
    // Images by mask bit; the unreached one below the highest is a zero row.
    assert_eq!(table_image(copy, &header, 0), Some(images[0]));
    assert_eq!(table_image(copy, &header, 1), None);
    assert!(copy[image_row_offset(2, 1)..image_row_offset(2, 2)]
        .iter()
        .all(|&b| b == 0));
    assert_eq!(table_image(copy, &header, 2), Some(images[2]));
    assert_eq!(table_image(copy, &header, 3), None);
    // A broken copy is refused: a flipped byte, scenes out of order, a scene on an absent image.
    let mut bad = copy.to_vec();
    bad[len - 1] ^= 1;
    assert_eq!(decode_header_checked(&bad), Err(TableError::Checksum));
    let reseal = |copy: &mut [u8]| {
        let sum = word_fnv(&copy[HEADER_BYTES..]);
        copy[0x14..0x18].copy_from_slice(&sum.to_le_bytes());
    };
    let mut swapped = copy.to_vec();
    swapped.copy_within(
        scene_row_offset(1)..scene_row_offset(2),
        scene_row_offset(0),
    );
    reseal(&mut swapped);
    assert_eq!(decode_header_checked(&swapped), Err(TableError::Compact));
    let mut on_absent = copy.to_vec();
    on_absent[scene_row_offset(0) + 2] = 1;
    reseal(&mut on_absent);
    assert_eq!(decode_header_checked(&on_absent), Err(TableError::Compact));
    assert_eq!(TableError::Compact.status(), Some(status::TABLE));
    // Reaching every row gives the disc's rows back, under the compact flag.
    let all: Vec<u16> = (0..6).collect();
    let len = encode_compact(&disc, &all, 0b1111, &mut out).unwrap();
    assert_eq!(len, compact_bytes(6, 0b1111));
    assert_eq!(out[HEADER_BYTES..len], disc[HEADER_BYTES..len]);
    // A duplicate id, an unknown one, a copy of a copy, a short buffer: refused.
    assert_eq!(
        encode_compact(&disc, &[2, 2], 0b0101, &mut out),
        Err(TableError::Compact)
    );
    assert_eq!(
        encode_compact(&disc, &[9], 0b0001, &mut out),
        Err(TableError::Compact)
    );
    let mut copy2 = vec![0u8; SECTOR_BYTES];
    let n = encode_compact(&disc, &[4], 0b0001, &mut copy2).unwrap();
    assert_eq!(
        encode_compact(&copy2[..n], &[4], 0b0001, &mut out),
        Err(TableError::Compact)
    );
    assert_eq!(
        encode_compact(&disc, &[4], 0b0001, &mut out[..100]),
        Err(TableError::ShortBuffer)
    );
    assert_eq!(
        encode_compact(&disc, &[4], 0b1_0001, &mut out),
        Err(TableError::ImageCount(5))
    );
}
