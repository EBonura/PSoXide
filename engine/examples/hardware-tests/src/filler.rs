// SPDX-License-Identifier: GPL-2.0-or-later
//! The deterministic filler that pads the radius disc out to 78 minutes.
//!
//! One definition, two users: the guest verifies every sector it reads against
//! [`word`], and the host tool (`psoxide-hwtest radius-fill`) writes the same
//! words into `RADIUS.DAT`. The host crate compiles this file with
//! `#[path]`, so the two cannot drift. Keep it free of any other crate.
//!
//! A sector's 512 words depend on the sector's number inside the file and on
//! the word's index, through a murmur3-style finaliser, so a sector that is
//! shifted, repeated or read from the wrong place does not match, and no word
//! is a function of its neighbours (a stuck bus bit or a lost word shows up in
//! the sector's checksum, not only in a compare).

/// 32-bit words in one data sector.
#[allow(dead_code)] pub const SECTOR_WORDS: u32 = 512;

/// Word `index` of sector `sector` of the filler file.
pub const fn word(sector: u32, index: u32) -> u32 {
    let mut x = sector.wrapping_mul(0x9E37_79B1) ^ index.wrapping_mul(0x85EB_CA6B) ^ 0x1B87_3593;
    x ^= x >> 16;
    x = x.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 13;
    x = x.wrapping_mul(0xC2B2_AE35);
    x ^ (x >> 16)
}

/// The FNV-1a offset basis the checksums start from.
pub const FNV_START: u32 = 0x811C_9DC5;

/// One word folded into an FNV-1a style checksum.
pub const fn fnv_step(hash: u32, word: u32) -> u32 {
    (hash ^ word).wrapping_mul(0x0100_0193)
}

/// The checksum a correct read of sector `sector` has.
#[allow(dead_code)] pub const fn sector_checksum(sector: u32) -> u32 {
    let mut hash = FNV_START;
    let mut index = 0;
    while index < SECTOR_WORDS {
        hash = fnv_step(hash, word(sector, index));
        index += 1;
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_differ_by_sector_and_index() {
        assert_ne!(word(0, 0), word(0, 1));
        assert_ne!(word(0, 0), word(1, 0));
        assert_ne!(word(1, 0), word(0, 1));
    }

    #[test]
    fn known_values_stay_put() {
        // The host and the guest must keep agreeing across edits.
        assert_eq!(word(0, 0), 0x67A2_A8EE);
        assert_eq!(word(1, 2), 0x2889_D4E9);
    }

    #[test]
    fn the_checksum_is_the_fold_of_the_words() {
        let mut hash = FNV_START;
        for index in 0..SECTOR_WORDS {
            hash = fnv_step(hash, word(7, index));
        }
        assert_eq!(hash, sector_checksum(7));
    }
}
