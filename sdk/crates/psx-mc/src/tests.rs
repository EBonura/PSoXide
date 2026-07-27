// SPDX-License-Identifier: GPL-2.0-or-later
//! Host tests for the filesystem, run against an in-memory [`RamCard`] so the
//! whole directory/allocation/container logic is exercised without hardware.

use crate::{Card, Entry, Error, RamCard, DATA_BLOCKS};

const NAME: &str = "BASLUS-99999SHEET01";
const NAME2: &str = "BASLUS-99999SHEET02";

fn fresh() -> Card<RamCard> {
    let mut c = Card::new(RamCard::new());
    c.format().unwrap();
    c
}

#[test]
fn format_makes_a_valid_empty_card() {
    let mut c = fresh();
    assert!(c.is_formatted().unwrap());
    assert_eq!(c.free_blocks().unwrap(), DATA_BLOCKS);
    let mut list = [blank_entry(); 15];
    assert_eq!(c.list(&mut list).unwrap(), 0);
}

#[test]
fn unformatted_card_is_detected() {
    let mut c = Card::new(RamCard::new());
    assert!(!c.is_formatted().unwrap());
}

#[test]
fn write_read_roundtrip() {
    let mut c = fresh();
    let data = b"cell A1=hello,B2=42,=SUM(A1:A9)";
    c.write(NAME, "SPREADSHEET", data).unwrap();

    let mut buf = [0u8; 256];
    let n = c.read(NAME, &mut buf).unwrap();
    assert_eq!(&buf[..n], data);
    assert_eq!(c.free_blocks().unwrap(), DATA_BLOCKS - 1);
}

#[test]
fn listing_reports_the_file() {
    let mut c = fresh();
    c.write(NAME, "SPREADSHEET", b"x").unwrap();
    c.write(NAME2, "SHEET TWO", b"yy").unwrap();
    let mut list = [blank_entry(); 15];
    let n = c.list(&mut list).unwrap();
    assert_eq!(n, 2);
    let names: [&str; 2] = [list[0].name(), list[1].name()];
    assert!(names.contains(&NAME));
    assert!(names.contains(&NAME2));
    assert_eq!(list[0].blocks, 1);
}

#[test]
fn overwrite_replaces_and_frees() {
    let mut c = fresh();
    c.write(NAME, "T", b"first version, longer").unwrap();
    c.write(NAME, "T", b"second").unwrap();
    let mut buf = [0u8; 64];
    let n = c.read(NAME, &mut buf).unwrap();
    assert_eq!(&buf[..n], b"second");
    // Still only one block used (overwrite freed the old one).
    assert_eq!(c.free_blocks().unwrap(), DATA_BLOCKS - 1);
}

#[test]
fn delete_frees_blocks() {
    let mut c = fresh();
    c.write(NAME, "T", b"data").unwrap();
    assert_eq!(c.free_blocks().unwrap(), DATA_BLOCKS - 1);
    c.delete(NAME).unwrap();
    assert_eq!(c.free_blocks().unwrap(), DATA_BLOCKS);
    let mut buf = [0u8; 16];
    assert_eq!(c.read(NAME, &mut buf), Err(Error::NotFound));
    // Deleting a missing file is Ok.
    c.delete(NAME).unwrap();
}

#[test]
fn multi_block_file_spans_and_roundtrips() {
    let mut c = fresh();
    // > 7936 payload forces a second block.
    let mut data = [0u8; 10_000];
    for (i, b) in data.iter_mut().enumerate() {
        *b = (i as u8) ^ (i >> 8) as u8;
    }
    c.write(NAME, "BIG", &data).unwrap();
    assert_eq!(c.free_blocks().unwrap(), DATA_BLOCKS - 2);

    let mut list = [blank_entry(); 15];
    c.list(&mut list).unwrap();
    assert_eq!(list[0].blocks, 2);

    let mut buf = [0u8; 10_000];
    let n = c.read(NAME, &mut buf).unwrap();
    assert_eq!(n, data.len());
    assert_eq!(buf, data);
}

#[test]
fn no_space_when_too_large() {
    let mut c = fresh();
    // Bigger than the whole card's usable capacity.
    let huge = [7u8; 130_000];
    assert_eq!(c.write(NAME, "T", &huge), Err(Error::NoSpace));
    assert_eq!(c.free_blocks().unwrap(), DATA_BLOCKS);
}

#[test]
fn read_buffer_too_small() {
    let mut c = fresh();
    c.write(NAME, "T", b"0123456789").unwrap();
    let mut small = [0u8; 4];
    assert_eq!(c.read(NAME, &mut small), Err(Error::BufferTooSmall));
}

#[test]
fn bad_names_rejected() {
    let mut c = fresh();
    assert_eq!(c.write("", "T", b"x"), Err(Error::BadName));
    assert_eq!(c.write("HAS SPACE\n", "T", b"x"), Err(Error::BadName));
    assert_eq!(
        c.write("WAY-TOO-LONG-NAME-FOR-A-CARD", "T", b"x"),
        Err(Error::BadName)
    );
    assert_eq!(c.write(NAME, "", b"x"), Err(Error::BadName));
}

#[test]
fn image_survives_reopen() {
    // Persist the image, rebuild a fresh Card from it -> data still there.
    let image;
    {
        let mut c = fresh();
        c.write(NAME, "T", b"persist me").unwrap();
        image = *c.into_inner().image();
    }
    let mut c = Card::new(RamCard::from_image(&image).unwrap());
    assert!(c.is_formatted().unwrap());
    let mut buf = [0u8; 32];
    let n = c.read(NAME, &mut buf).unwrap();
    assert_eq!(&buf[..n], b"persist me");
}

#[cfg(feature = "compress")]
#[test]
fn compressed_roundtrip() {
    let mut c = fresh();
    // Sparse, compressible payload.
    let mut data = [0u8; 4096];
    for k in 0..16 {
        data[256 * k] = k as u8;
        data[256 * k + 1] = 0xFF;
    }
    let mut scratch = [0u8; 4096];
    c.write_compressed(NAME, "COMPRESSED", &data, &mut scratch)
        .unwrap();
    // It should have compressed to a single block (well under 7936).
    assert_eq!(c.free_blocks().unwrap(), DATA_BLOCKS - 1);

    let mut buf = [0u8; 4096];
    let n = c.read(NAME, &mut buf).unwrap();
    assert_eq!(n, data.len());
    assert_eq!(buf, data);
}

#[cfg(feature = "compress")]
#[test]
fn compressed_falls_back_when_incompressible() {
    let mut c = fresh();
    let mut data = [0u8; 512];
    for (i, b) in data.iter_mut().enumerate() {
        *b = (i * 131 + 7) as u8; // high-entropy-ish
    }
    let mut scratch = [0u8; 1024];
    c.write_compressed(NAME, "RAW", &data, &mut scratch)
        .unwrap();
    let mut buf = [0u8; 512];
    let n = c.read(NAME, &mut buf).unwrap();
    assert_eq!(&buf[..n], &data[..]);
}

#[test]
fn corrupt_stored_len_is_rejected_not_panicking() {
    // A corrupt container header whose stored_len exceeds both raw_len and the
    // caller's buffer used to slice out of bounds; it must surface as an error.
    let mut image;
    {
        let mut c = fresh();
        c.write(NAME, "T", b"0123456789").unwrap();
        image = *c.into_inner().image();
    }
    let at = image
        .windows(4)
        .position(|w| w == crate::CONTAINER_MAGIC)
        .unwrap();
    image[at + 12..at + 16].copy_from_slice(&u32::MAX.to_le_bytes());
    let mut c = Card::new(crate::RamCard::from_image(&image).unwrap());
    let mut buf = [0u8; 10];
    assert_eq!(c.read(NAME, &mut buf), Err(Error::BadContainer));
}

#[test]
fn on_card_bytes_match_the_real_bios_format() {
    // These two fields are never read back by this driver (`read()` trusts
    // the block chain + container header instead), so a regression here is
    // invisible to every other test while still being real hardware breaks:
    // a real BIOS validates the directory size field against the block
    // count, and reads the icon flag as a u16 to know how to parse the
    // header -- 216 or a corrupted flag value has been observed to make a
    // save that reads back byte-perfect simply not show up in the BIOS's
    // own memory-card manager.
    let mut c = fresh();
    c.write(NAME, "T", b"tiny payload").unwrap();
    let image = *c.into_inner().image();

    // Directory entry for the first (and only) allocated block: frame 1,
    // i.e. bytes [128..256) of the image.
    let dir = &image[128..256];
    let size = u32::from_le_bytes(dir[4..8].try_into().unwrap());
    assert_eq!(size, 8192, "directory size field must be block-aligned (blocks * 8192), not the payload length");

    // "SC" header, first frame of block 1 (the file's first data block):
    // bytes [8192..8320).
    let hdr = &image[8192..8320];
    assert_eq!(&hdr[0..2], b"SC");
    let icon_flag = u16::from_le_bytes(hdr[2..4].try_into().unwrap());
    assert_eq!(icon_flag, 0x12, "icon flag must be a clean u16 (one static frame), not corrupted by a block-count byte in its high byte");
    assert_eq!(&hdr[4..4 + 1], b"T", "title must start right after the 2-byte icon flag");
}

fn blank_entry() -> Entry {
    Entry {
        name: [0; crate::MAX_NAME + 1],
        name_len: 0,
        blocks: 0,
    }
}
