// SPDX-License-Identifier: GPL-2.0-or-later
//! Host tests for the filesystem, run against an in-memory [`RamCard`] so the
//! whole directory/allocation/container logic is exercised without hardware.

use crate::{
    Block, Card, Entry, Error, RamCard, SaveIcon, DATA_BLOCKS, FRAMES_PER_BLOCK, FRAME_SIZE,
};

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
    c.validate_filesystem().unwrap();
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
    c.validate_filesystem().unwrap();
}

#[test]
fn directory_size_is_allocated_in_bios_block_units() {
    let mut c = fresh();
    c.write(NAME, "SMALL", b"x").unwrap();
    let image = c.into_inner();
    assert_eq!(directory_size(image.image(), 0), 0x2000);

    let mut c = fresh();
    c.write(NAME, "BIG", &[0x5a; 10_000]).unwrap();
    let image = c.into_inner();
    assert_eq!(directory_size(image.image(), 0), 0x4000);
}

#[test]
fn strict_validation_rejects_payload_byte_size() {
    let mut c = fresh();
    c.write(NAME, "LEGACY", &[0x5a; 64]).unwrap();
    let mut image = c.into_inner().image().to_vec();
    set_directory_size(&mut image, 0, 16 + 64);

    let mut c = Card::new(RamCard::from_image(&image).unwrap());
    assert_eq!(c.validate_filesystem(), Err(Error::Corrupt));
}

#[test]
fn custom_icon_is_written_in_native_frames() {
    let mut c = fresh();
    let mut palette = [0u16; 16];
    palette[1] = 0x7fff;
    palette[2] = 0x03e0;
    let pixels = [0x21u8; FRAME_SIZE];
    let icon = SaveIcon::new(palette, pixels);
    c.write_with_icon(NAME, "ICON", b"x", &icon).unwrap();

    let base = FRAMES_PER_BLOCK as u16;
    let mut header = [0u8; FRAME_SIZE];
    let mut stored_pixels = [0u8; FRAME_SIZE];
    c.device().read_frame(base, &mut header).unwrap();
    c.device().read_frame(base + 1, &mut stored_pixels).unwrap();
    // Sony's save header stores these as two independent bytes:
    // icon display type (0x11 = one frame), then allocated block count.
    assert_eq!(header[2], 0x11);
    assert_eq!(header[3], 0x01);
    assert_eq!(&header[0x60..0x64], &[0x00, 0x00, 0xff, 0x7f]);
    assert_eq!(stored_pixels, pixels);
}

#[test]
fn filesystem_validation_rejects_bad_directory_checksum() {
    let mut image = *fresh().into_inner().image();
    image[FRAME_SIZE] ^= 1;
    let mut c = Card::new(RamCard::from_image(&image).unwrap());
    assert_eq!(c.validate_filesystem(), Err(Error::Corrupt));
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

fn blank_entry() -> Entry {
    Entry {
        name: [0; crate::MAX_NAME_LEN + 1],
        name_len: 0,
        blocks: 0,
    }
}

fn directory_size(image: &[u8], index: usize) -> u32 {
    let at = (1 + index) * FRAME_SIZE + 4;
    u32::from_le_bytes([image[at], image[at + 1], image[at + 2], image[at + 3]])
}

fn set_directory_size(image: &mut [u8], index: usize, size: usize) {
    let base = (1 + index) * FRAME_SIZE;
    image[base + 4..base + 8].copy_from_slice(&(size as u32).to_le_bytes());
    image[base + 127] = image[base..base + 127]
        .iter()
        .fold(0u8, |checksum, byte| checksum ^ byte);
}

#[test]
fn entry_name_reads_ascii_names_back() {
    let mut e = blank_entry();
    e.name[..4].copy_from_slice(b"SAVE");
    e.name_len = 4;
    assert_eq!(e.name(), "SAVE");
}

#[test]
fn entry_name_falls_back_for_corrupt_bytes_and_lengths() {
    // A corrupt card can hand back bytes that are not UTF-8.
    let mut e = blank_entry();
    e.name[..3].copy_from_slice(&[b'A', 0xff, 0xfe]);
    e.name_len = 3;
    assert_eq!(e.name(), "?");
    // A length past the buffer must not index out of it either.
    e.name_len = u8::MAX;
    assert_eq!(e.name(), "?");
}

/// A card that stops answering after a set number of frame writes, as when it
/// is pulled mid-save.
struct PullAfterWrites {
    inner: RamCard,
    left: usize,
}

impl Block for PullAfterWrites {
    fn read_frame(&mut self, frame: u16, out: &mut [u8; FRAME_SIZE]) -> crate::Result<()> {
        self.inner.read_frame(frame, out)
    }
    fn write_frame(&mut self, frame: u16, data: &[u8; FRAME_SIZE]) -> crate::Result<()> {
        if self.left == 0 {
            return Err(Error::NoCard);
        }
        self.left -= 1;
        self.inner.write_frame(frame, data)
    }
}

#[test]
fn overwrite_that_cannot_fit_keeps_the_old_save() {
    // Thirteen blocks belong to other saves and NAME holds one, so a 3-block
    // replacement has only two free blocks to land in. The old save used to be
    // freed before this was known.
    let mut c = fresh();
    for i in 0..13 {
        let mut other = *b"BASLUS-99999OTHER00";
        other[17] = b'0' + i / 10;
        other[18] = b'0' + i % 10;
        c.write(core::str::from_utf8(&other).unwrap(), "OTHER", b"o")
            .unwrap();
    }
    c.write(NAME, "T", b"the old save").unwrap();
    assert_eq!(c.free_blocks().unwrap(), 1);
    let before = *c.device().image();

    let big = [9u8; 20_000];
    assert_eq!(c.write(NAME, "T", &big), Err(Error::NoSpace));

    let mut buf = [0u8; 64];
    let n = c.read(NAME, &mut buf).unwrap();
    assert_eq!(&buf[..n], b"the old save");
    assert_eq!(
        c.device().image(),
        &before,
        "a refused write touches nothing"
    );
}

#[test]
fn overwrite_survives_the_card_being_pulled_at_any_frame() {
    let old = [0x11u8; 9_000]; // two blocks
    let new = [0x22u8; 9_500]; // two blocks
    let mut base = fresh();
    base.write(NAME, "T", &old).unwrap();
    let image = *base.into_inner().image();

    let mut completed = false;
    for allowed in 0..400 {
        let dev = PullAfterWrites {
            inner: RamCard::from_image(&image).unwrap(),
            left: allowed,
        };
        let mut c = Card::new(dev);
        let result = c.write(NAME, "T", &new);

        let mut buf = [0u8; 10_000];
        let n = c
            .read(NAME, &mut buf)
            .unwrap_or_else(|e| panic!("after {allowed} writes the save is unreadable: {e:?}"));
        let got = &buf[..n];
        assert!(
            got == old || got == new,
            "after {allowed} writes the save is neither version"
        );
        if result.is_ok() {
            assert_eq!(got, new);
            completed = true;
            break;
        }
    }
    assert!(completed, "the write never completed inside 400 frames");
}

/// A card with two one-block saves, A in directory entry 0 and B in entry 1,
/// whose entry 0 link byte was flipped on its way to the card so that A claims
/// B's block as a continuation. The checksum byte no longer matches.
fn cross_linked_card() -> Card<RamCard> {
    let mut c = fresh();
    c.write(NAME, "A", b"save A").unwrap();
    c.write(NAME2, "B", b"save B").unwrap();
    let mut image = *c.into_inner().image();
    // Entry 0 is frame 1; its link field is bytes 8..10, `FFFF` for "none".
    assert_eq!(&image[FRAME_SIZE + 8..FRAME_SIZE + 10], &[0xFF, 0xFF]);
    image[FRAME_SIZE + 8] = 1;
    image[FRAME_SIZE + 9] = 0;
    Card::new(RamCard::from_image(&image).unwrap())
}

#[test]
fn a_directory_entry_with_a_bad_checksum_is_refused() {
    let mut c = cross_linked_card();
    let mut list = [blank_entry(); 15];
    assert_eq!(c.list(&mut list), Err(Error::Corrupt));
    assert_eq!(c.free_blocks(), Err(Error::Corrupt));
    let mut buf = [0u8; 16];
    assert_eq!(c.read(NAME, &mut buf), Err(Error::Corrupt));
    assert_eq!(c.write(NAME2, "B", b"new B"), Err(Error::Corrupt));
}

#[test]
fn deleting_through_a_cross_linked_entry_does_not_free_the_other_save() {
    let mut c = cross_linked_card();
    assert_eq!(c.delete(NAME), Err(Error::Corrupt));
    // B's entry is intact and still reads once the damaged entry is out of the way.
    let mut image = *c.into_inner().image();
    image[FRAME_SIZE + 8] = 0xFF;
    image[FRAME_SIZE + 9] = 0xFF;
    let mut c = Card::new(RamCard::from_image(&image).unwrap());
    let mut buf = [0u8; 16];
    let n = c.read(NAME2, &mut buf).unwrap();
    assert_eq!(&buf[..n], b"save B");
}

/// A card that stops answering reads of one frame, as when it is pulled while
/// a save streams in.
struct FailRead {
    inner: RamCard,
    frame: u16,
}

impl Block for FailRead {
    fn read_frame(&mut self, frame: u16, out: &mut [u8; FRAME_SIZE]) -> crate::Result<()> {
        if frame == self.frame {
            return Err(Error::NoCard);
        }
        self.inner.read_frame(frame, out)
    }
    fn write_frame(&mut self, frame: u16, data: &[u8; FRAME_SIZE]) -> crate::Result<()> {
        self.inner.write_frame(frame, data)
    }
}

#[cfg(feature = "compress")]
#[test]
fn a_card_pulled_during_a_compressed_read_reports_the_transport_error() {
    let mut data = [0u8; 512];
    for (i, b) in data[..256].iter_mut().enumerate() {
        *b = (i * 131 + 7) as u8; // incompressible half, then zeros
    }
    let mut c = fresh();
    let mut scratch = [0u8; 1024];
    c.write_compressed(NAME, "T", &data, &mut scratch).unwrap();
    // Block 1 is frames 64..128: title, icon, then the payload from frame 66.
    // The stored bytes run past frame 66 into 67.
    let image = *c.into_inner().image();
    let mut c = Card::new(FailRead {
        inner: RamCard::from_image(&image).unwrap(),
        frame: 67,
    });
    let mut buf = [0u8; 512];
    assert_eq!(c.read(NAME, &mut buf), Err(Error::NoCard));
}
