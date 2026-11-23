extern crate std;

use super::*;
use std::vec;
use std::vec::Vec;

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn sample_row(id: u16) -> Row {
    Row {
        id,
        kind: Kind::Program,
        flags: ROW_FLAG_PARTNER_CREDIT,
        lba: 0x0102_0304 + u32::from(id),
        sectors: 0x1112_1314,
        t_addr: 0x8001_0000,
        t_size: 0x0004_0800,
        pc: 0x8001_2345,
        checksum: 0xa1a2_a3a4,
        core_hash: 0xb1b2_b3b4,
        sp: 0x801f_d700,
        return_scene: 0xc1c2,
        read_attempts: 0xd1,
        min_block_version: 0xd2,
        partner_credit: 0xe1e2,
        name: Row::name_bytes("GAME.EXE"),
    }
}

#[test]
fn memory_map_is_the_day_0_page() {
    assert_eq!(CORE_WINDOW.start, 0x801f_d700);
    assert_eq!(CORE_WINDOW.end, 0x801f_ff00);
    assert_eq!(CORE_WINDOW.len(), 10_240);
    assert_eq!(CORE_WINDOW.end, HANDOFF_BLOCK.start);
    assert_eq!(HANDOFF_BLOCK.len(), 256);
    assert_eq!(HANDOFF_BLOCK.end, 0x8020_0000);
    assert_eq!(STACK_INIT, 0x801f_d700);
    // psoxide.ld: STACK_INIT = RAM_BASE + RAM_SIZE - 0x100.
    assert_eq!(RAM_SIZE_OVERRIDE, 0x1f_d800);
    assert_eq!(RAM_BASE + RAM_SIZE_OVERRIDE - 0x100, STACK_INIT);
    assert_eq!(PAYLOAD_END, 0x801f_d300);
    assert!(CORE_WINDOW.holds(0x801f_d700, 0x801f_ff00));
    assert!(!CORE_WINDOW.holds(0x801f_d6fc, 0x801f_d800));
    assert!(!CORE_WINDOW.holds(0x801f_fe00, 0x801f_ff04));
    assert!(!CORE_WINDOW.holds(0x801f_e000, 0x801f_e000));
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
        slot::MUSIC_YIELD,
        slot::STREAM_OPEN,
        slot::STREAM_SECTOR,
        slot::CD,
        slot::SET_IDLE_HOOK,
    ];
    let want = [
        0x801f_d700,
        0x801f_d708,
        0x801f_d710,
        0x801f_d718,
        0x801f_d720,
        0x801f_d728,
        0x801f_d730,
        0x801f_d738,
        0x801f_d740,
        0x801f_d748,
        0x801f_d750,
        0x801f_d758,
    ];
    assert_eq!(slots, want);
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
fn header_round_trips_at_the_day_0_offsets() {
    let header = Header {
        version: VERSION,
        row_count: 0x0203,
        table_sectors: 4,
        boot_scene: 0x0405,
        checksum: 0x0607_0809,
    };
    let mut bytes = [0xeeu8; HEADER_BYTES];
    header.encode(&mut bytes);
    assert_eq!(&bytes[0x00..0x08], b"PSXSCN01");
    assert_eq!(le16(&bytes, 0x08), 1);
    assert_eq!(le16(&bytes, 0x0a), 0x0203);
    assert_eq!(le16(&bytes, 0x0c), 64);
    assert_eq!(le16(&bytes, 0x0e), 4);
    assert_eq!(le16(&bytes, 0x10), 0x0405);
    assert_eq!(le16(&bytes, 0x12), 0);
    assert_eq!(le32(&bytes, 0x14), 0x0607_0809);
    assert!(bytes[0x18..0x40].iter().all(|&b| b == 0));
    // 0x0203 rows do not fit 4 sectors: decode refuses, as the core would.
    assert_eq!(Header::decode(&bytes), None);
    let header = Header {
        row_count: 95,
        ..header
    };
    header.encode(&mut bytes);
    assert_eq!(Header::decode(&bytes), Some(header));
    let mut wrong = bytes;
    wrong[7] = b'2';
    assert_eq!(Header::decode(&wrong), None);
}

#[test]
fn row_round_trips_at_the_day_0_offsets() {
    let row = sample_row(7);
    let mut bytes = [0xeeu8; ROW_BYTES];
    row.encode(&mut bytes);
    assert_eq!(le16(&bytes, 0x00), 7);
    assert_eq!(bytes[0x02], 0);
    assert_eq!(bytes[0x03], 1);
    assert_eq!(le32(&bytes, 0x04), 0x0102_030b);
    assert_eq!(le32(&bytes, 0x08), 0x1112_1314);
    assert_eq!(le32(&bytes, 0x0c), 0x8001_0000);
    assert_eq!(le32(&bytes, 0x10), 0x0004_0800);
    assert_eq!(le32(&bytes, 0x14), 0x8001_2345);
    assert_eq!(le32(&bytes, 0x18), 0xa1a2_a3a4);
    assert_eq!(le32(&bytes, 0x1c), 0xb1b2_b3b4);
    assert_eq!(le32(&bytes, 0x20), 0x801f_d700);
    assert_eq!(le16(&bytes, 0x24), 0xc1c2);
    assert_eq!(bytes[0x26], 0xd1);
    assert_eq!(bytes[0x27], 0xd2);
    assert_eq!(le16(&bytes, 0x28), 0xe1e2);
    assert_eq!(le16(&bytes, 0x2a), 0);
    assert_eq!(&bytes[0x2c..0x34], b"GAME.EXE");
    assert!(bytes[0x34..0x40].iter().all(|&b| b == 0));
    assert_eq!(Row::decode(&bytes), Some(row));
    assert_eq!(row.name_str(), "GAME.EXE");
    for (kind, byte) in [
        (Kind::Program, 0),
        (Kind::Image, 1),
        (Kind::Data, 2),
        (Kind::Module, 3),
        (Kind::Call, 4),
        (Kind::Boot, 5),
    ] {
        assert_eq!(Kind::from_u8(byte), Some(kind));
        assert_eq!(kind as u8, byte);
    }
    bytes[0x02] = 6;
    assert_eq!(Row::decode(&bytes), None);
}

#[test]
fn no_row_crosses_a_sector() {
    assert_eq!(rows_capacity(1), 31);
    assert_eq!(rows_capacity(2), 63);
    assert_eq!(rows_capacity(4), 127);
    for index in 0..rows_capacity(MAX_TABLE_SECTORS) as u16 {
        let (sector, offset) = row_location(index);
        assert!(offset + ROW_BYTES <= SECTOR_BYTES, "row {index}");
        assert!(sector < u32::from(MAX_TABLE_SECTORS));
    }
    assert_eq!(row_location(30), (0, 0x7c0));
    assert_eq!(row_location(31), (1, 0));
    assert_eq!(table_sectors_for(1), Some(1));
    assert_eq!(table_sectors_for(31), Some(1));
    assert_eq!(table_sectors_for(32), Some(2));
    assert_eq!(table_sectors_for(127), Some(4));
    assert_eq!(table_sectors_for(128), None);
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
fn a_table_encodes_checks_and_reads_back() {
    let rows: Vec<Row> = (0..40).map(sample_row).collect();
    let mut out = vec![0xffu8; 3 * SECTOR_BYTES];
    let len = encode_table(&rows, 2, &mut out).unwrap();
    assert_eq!(len, 2 * SECTOR_BYTES);
    let header = decode_header_checked(&out[..len]).unwrap();
    assert_eq!(header.row_count, 40);
    assert_eq!(header.table_sectors, 2);
    assert_eq!(header.boot_scene, 2);
    let words: Vec<u32> = out[HEADER_BYTES..HEADER_BYTES + 40 * ROW_BYTES]
        .chunks_exact(4)
        .map(|w| u32::from_le_bytes(w.try_into().unwrap()))
        .collect();
    assert_eq!(words.len(), 40 * 16);
    assert_eq!(header.checksum, psx_pack::WordFnv32::of(&words));
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(table_row(&out[..len], &header, i as u16), Some(*row));
    }
    assert_eq!(table_row(&out[..len], &header, 40), None);
    // Past the rows the table is zero, to the end of its last sector.
    assert!(out[HEADER_BYTES + 40 * ROW_BYTES..len]
        .iter()
        .all(|&b| b == 0));
    // A flipped row byte fails the checksum.
    let mut bad = out.clone();
    bad[HEADER_BYTES + 5] ^= 1;
    assert_eq!(decode_header_checked(&bad[..len]), None);
    // Short of its sectors.
    assert_eq!(decode_header_checked(&out[..SECTOR_BYTES]), None);
}

#[test]
fn a_table_refuses_bad_rows() {
    let mut out = vec![0u8; 4 * SECTOR_BYTES];
    assert_eq!(encode_table(&[], 0, &mut out), Err(TableError::Empty));
    let rows = [sample_row(0), sample_row(5)];
    assert_eq!(
        encode_table(&rows, 0, &mut out),
        Err(TableError::IdNotIndex { index: 1, id: 5 })
    );
    let rows = [sample_row(0)];
    assert_eq!(
        encode_table(&rows, 1, &mut out),
        Err(TableError::NoBootRow(1))
    );
    let many: Vec<Row> = (0..128).map(sample_row).collect();
    assert_eq!(
        encode_table(&many, 0, &mut out),
        Err(TableError::TooManyRows(128))
    );
    let rows: Vec<Row> = (0..32).map(sample_row).collect();
    assert_eq!(
        encode_table(&rows, 0, &mut out[..SECTOR_BYTES]),
        Err(TableError::ShortBuffer {
            needed: 2 * SECTOR_BYTES
        })
    );
}
