// SPDX-License-Identifier: GPL-2.0-or-later
//! Recovery logic for the one malformed save created by the first hardware
//! diagnostic disc. This intentionally lives in the diagnostic, not `psx-mc`.

#![no_std]

use psx_mc::{Block, Card, Error, Result, DATA_BLOCKS, FRAME_SIZE};

const E_STATE: usize = 0;
const E_SIZE: usize = 4;
const E_LINK: usize = 8;
const E_NAME: usize = 10;
const E_CHK: usize = 127;
const ST_FIRST: u8 = 0x51;
const ST_MIDDLE: u8 = 0x52;
const ST_LAST: u8 = 0x53;
const LINK_NONE: u16 = 0xffff;
const BLOCK_BYTES: u32 = 0x2000;

/// Return the correct allocated size only if the card has exactly one named
/// entry with `legacy_size`, every chain is structurally sound, and every
/// other entry already has a BIOS-compatible allocated size.
pub fn exact_legacy_size_target<B: Block>(
    card: &mut Card<B>,
    name: &str,
    legacy_size: u32,
) -> Result<Option<u32>> {
    let mut header = [0u8; FRAME_SIZE];
    card.device().read_frame(0, &mut header)?;
    if header[0] != b'M' || header[1] != b'C' {
        return Err(Error::NotFormatted);
    }
    if checksum(&header) != header[E_CHK] {
        return Err(Error::Corrupt);
    }

    let mut states = [0u8; DATA_BLOCKS];
    let mut links = [LINK_NONE; DATA_BLOCKS];
    let mut sizes = [0u32; DATA_BLOCKS];
    let mut named = [false; DATA_BLOCKS];
    let mut named_count = 0u8;
    for index in 0..DATA_BLOCKS {
        let entry = read_dir(card, index)?;
        if checksum(&entry) != entry[E_CHK] {
            return Err(Error::Corrupt);
        }
        let state = entry[E_STATE];
        if !is_free(state) && !matches!(state, ST_FIRST | ST_MIDDLE | ST_LAST) {
            return Err(Error::Corrupt);
        }
        states[index] = state;
        links[index] = u16::from_le_bytes([entry[E_LINK], entry[E_LINK + 1]]);
        sizes[index] = read_u32(&entry[E_SIZE..]);
        if state == ST_FIRST && entry_name_eq(&entry, name.as_bytes()) {
            named[index] = true;
            named_count = named_count.saturating_add(1);
        }
    }
    if named_count > 1 {
        return Ok(None);
    }

    let mut visited = 0u16;
    let mut target = None;
    for first in 0..DATA_BLOCKS {
        if states[first] != ST_FIRST {
            continue;
        }
        let mut current = first;
        let mut first_node = true;
        let mut chain_len = 0u32;
        loop {
            let bit = 1u16 << current;
            if visited & bit != 0 {
                return Err(Error::Corrupt);
            }
            visited |= bit;
            chain_len += 1;

            let state = states[current];
            if first_node {
                if state != ST_FIRST {
                    return Err(Error::Corrupt);
                }
                first_node = false;
            } else if !matches!(state, ST_MIDDLE | ST_LAST) {
                return Err(Error::Corrupt);
            }

            let link = links[current];
            if state == ST_LAST || (state == ST_FIRST && link == LINK_NONE) {
                if link != LINK_NONE {
                    return Err(Error::Corrupt);
                }
                break;
            }
            if link == LINK_NONE || link as usize >= DATA_BLOCKS {
                return Err(Error::Corrupt);
            }
            current = link as usize;
        }

        let expected = chain_len * BLOCK_BYTES;
        if named[first] {
            if sizes[first] != legacy_size || sizes[first] == expected {
                return Ok(None);
            }
            target = Some(expected);
        } else if sizes[first] != expected {
            return Err(Error::Corrupt);
        }
    }

    for index in 0..DATA_BLOCKS {
        if matches!(states[index], ST_MIDDLE | ST_LAST) && visited & (1u16 << index) == 0 {
            return Err(Error::Corrupt);
        }
    }
    Ok(target)
}

/// Repair the exact legacy entry by writing one directory frame, read it back,
/// and run the SDK's strict validator. The caller remains responsible for
/// authenticating the diagnostic payload before calling this function.
pub fn repair_exact_legacy_size<B: Block>(
    card: &mut Card<B>,
    name: &str,
    legacy_size: u32,
) -> Result<u32> {
    let expected = exact_legacy_size_target(card, name, legacy_size)?.ok_or(Error::Corrupt)?;
    let mut target = None;
    for index in 0..DATA_BLOCKS {
        let entry = read_dir(card, index)?;
        if entry[E_STATE] == ST_FIRST && entry_name_eq(&entry, name.as_bytes()) {
            if target.is_some() {
                return Err(Error::Corrupt);
            }
            target = Some((index, entry));
        }
    }
    let (index, mut entry) = target.ok_or(Error::NotFound)?;
    if checksum(&entry) != entry[E_CHK] || read_u32(&entry[E_SIZE..]) != legacy_size {
        return Err(Error::Corrupt);
    }

    entry[E_SIZE..E_SIZE + 4].copy_from_slice(&expected.to_le_bytes());
    entry[E_CHK] = checksum(&entry);
    card.device().write_frame(1 + index as u16, &entry)?;
    if read_dir(card, index)? != entry {
        return Err(Error::Corrupt);
    }
    card.validate_filesystem()?;
    Ok(expected)
}

fn read_dir<B: Block>(card: &mut Card<B>, index: usize) -> Result<[u8; FRAME_SIZE]> {
    let mut entry = [0u8; FRAME_SIZE];
    card.device().read_frame(1 + index as u16, &mut entry)?;
    Ok(entry)
}

fn checksum(frame: &[u8; FRAME_SIZE]) -> u8 {
    frame[..E_CHK].iter().fold(0u8, |sum, byte| sum ^ byte)
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn is_free(state: u8) -> bool {
    state & 0xf0 == 0xa0
}

fn entry_name_eq(entry: &[u8; FRAME_SIZE], name: &[u8]) -> bool {
    if name.len() > 20 || &entry[E_NAME..E_NAME + name.len()] != name {
        return false;
    }
    name.len() == 20 || entry[E_NAME + name.len()] == 0
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{exact_legacy_size_target, repair_exact_legacy_size};
    use psx_mc::{Card, Entry, Error, RamCard, CARD_SIZE, DATA_BLOCKS, FRAME_SIZE, MAX_NAME};
    use std::vec::Vec;

    const TARGET: &str = "BESLES-00000PSXMC01";
    const BEFORE: &str = "BESLES-00000BEFORE1";
    const AFTER: &str = "BESLES-00000AFTER01";

    fn card_with_legacy_entry() -> Vec<u8> {
        let mut card = Card::new(RamCard::new());
        card.format().unwrap();
        card.write(BEFORE, "BEFORE", b"before").unwrap();
        card.write(TARGET, "PSOXIDE MC HARDWARE TEST", &[0x5a; 64])
            .unwrap();
        card.write(AFTER, "AFTER", b"after").unwrap();
        let mut image = card.into_inner().image().to_vec();
        set_directory_size(&mut image, 1, 0x50);
        image
    }

    #[test]
    fn changes_only_target_size_and_checksum_then_survives_cold_boot() {
        let image = card_with_legacy_entry();
        let before = image.clone();
        let mut card = Card::new(RamCard::from_image(&image).unwrap());
        assert_eq!(card.validate_filesystem(), Err(Error::Corrupt));
        assert_eq!(
            exact_legacy_size_target(&mut card, TARGET, 0x50),
            Ok(Some(0x2000))
        );
        assert_eq!(
            repair_exact_legacy_size(&mut card, TARGET, 0x50),
            Ok(0x2000)
        );

        let after = card.into_inner().image().to_vec();
        let base = (1 + 1) * FRAME_SIZE;
        let changed: Vec<_> = (0..CARD_SIZE)
            .filter(|&index| before[index] != after[index])
            .collect();
        assert_eq!(changed, [base + 4, base + 5, base + 127]);

        let mut cold_boot = Card::new(RamCard::from_image(&after).unwrap());
        cold_boot.validate_filesystem().unwrap();
        let mut entries = [blank_entry(); DATA_BLOCKS];
        assert_eq!(cold_boot.list(&mut entries).unwrap(), 3);
        assert_eq!(entries[0].name(), BEFORE);
        assert_eq!(entries[1].name(), TARGET);
        assert_eq!(entries[2].name(), AFTER);
    }

    #[test]
    fn refuses_wrong_size_name_or_unrelated_corruption() {
        let image = card_with_legacy_entry();
        let mut card = Card::new(RamCard::from_image(&image).unwrap());
        assert_eq!(exact_legacy_size_target(&mut card, TARGET, 0x60), Ok(None));
        assert_eq!(
            exact_legacy_size_target(&mut card, "BESLES-00000UNKNOWN", 0x50),
            Err(Error::Corrupt)
        );

        let mut image = image;
        set_directory_size(&mut image, 2, 0x60);
        let mut card = Card::new(RamCard::from_image(&image).unwrap());
        assert_eq!(
            exact_legacy_size_target(&mut card, TARGET, 0x50),
            Err(Error::Corrupt)
        );
        assert_eq!(
            repair_exact_legacy_size(&mut card, TARGET, 0x50),
            Err(Error::Corrupt)
        );
    }

    fn set_directory_size(image: &mut [u8], index: usize, size: u32) {
        let base = (1 + index) * FRAME_SIZE;
        image[base + 4..base + 8].copy_from_slice(&size.to_le_bytes());
        image[base + 127] = image[base..base + 127]
            .iter()
            .fold(0u8, |sum, byte| sum ^ byte);
    }

    const fn blank_entry() -> Entry {
        Entry {
            name: [0; MAX_NAME + 1],
            name_len: 0,
            blocks: 0,
        }
    }
}
