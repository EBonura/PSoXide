// SPDX-License-Identifier: GPL-2.0-or-later
//! Word FNV-1a: `h = (h ^ word) * 0x0100_0193` from 0x811C_9DC5, over little-endian `u32`s.

/// The offset basis.
pub const BASIS: u32 = 0x811C_9DC5;
/// The prime.
pub const PRIME: u32 = 0x0100_0193;

/// One step.
#[inline(always)]
#[must_use]
pub const fn step(hash: u32, word: u32) -> u32 {
    (hash ^ word).wrapping_mul(PRIME)
}

/// Word FNV over `bytes` taken as little-endian words; a short last word is zero-padded.
#[must_use]
pub const fn of_bytes(bytes: &[u8]) -> u32 {
    let mut hash = BASIS;
    let mut at = 0;
    while at < bytes.len() {
        let mut word = 0u32;
        let mut k = 0;
        while k < 4 {
            if at + k < bytes.len() {
                word |= (bytes[at + k] as u32) << (8 * k);
            }
            k += 1;
        }
        hash = step(hash, word);
        at += 4;
    }
    hash
}

/// Word FNV over `words` words of RAM from `at`, read volatile.
///
/// # Safety
/// `at..at + 4 * words` must be readable, word-aligned memory.
#[inline(never)]
#[must_use]
pub unsafe fn of_ram(at: *const u32, words: u32) -> u32 {
    let mut hash = BASIS;
    let mut i = 0;
    while i < words {
        // SAFETY: inside the range the caller vouches for.
        hash = step(hash, unsafe { core::ptr::read_volatile(at.add(i as usize)) });
        i += 1;
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_the_basis() {
        assert_eq!(of_bytes(&[]), BASIS);
    }

    #[test]
    fn bytes_are_little_endian_words() {
        assert_eq!(of_bytes(&[1, 0, 0, 0, 2, 0, 0, 0]), step(step(BASIS, 1), 2));
        assert_eq!(of_bytes(&[0x78, 0x56, 0x34, 0x12]), step(BASIS, 0x1234_5678));
        assert_eq!(of_bytes(&[5]), step(BASIS, 5));
    }
}
