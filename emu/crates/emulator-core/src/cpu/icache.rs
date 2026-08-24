//! PlayStation R3000A instruction cache.
//!
//! The PS1 has a 4 KiB direct-mapped instruction cache: 256 lines,
//! four 32-bit words per line. Tags use the physical address above the
//! 4 KiB page offset, so KUSEG and KSEG0 aliases share entries. Validity
//! is tracked per word rather than per line.

use crate::bus::Bus;

const LINE_COUNT: usize = 256;
const WORDS_PER_LINE: usize = 4;
const LINE_BYTES: u32 = 16;

#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
struct CacheLine {
    /// Physical address bits 31:12.
    tag: u32,
    /// One valid bit for each word in `words`.
    valid: u8,
    words: [u32; WORDS_PER_LINE],
}

/// Metadata for one instruction-cache refill. This is emulator-owned
/// diagnostic state; it never participates in the emulated cache contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Refill {
    pub(super) set: u8,
    pub(super) incoming_line: u32,
    pub(super) incoming_tag: u32,
    pub(super) victim_line: u32,
    pub(super) victim_tag: u32,
    pub(super) victim_valid_mask: u8,
    pub(super) tag_miss: bool,
    pub(super) fill_words: u8,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct InstructionCache {
    #[serde(with = "crate::serde_big_array::array")]
    lines: [CacheLine; LINE_COUNT],
}

impl Default for InstructionCache {
    fn default() -> Self {
        Self {
            lines: [CacheLine::default(); LINE_COUNT],
        }
    }
}

impl InstructionCache {
    #[inline]
    fn coordinates(phys: u32) -> (usize, usize, u32) {
        let line = ((phys >> 4) & 0xFF) as usize;
        let word = ((phys >> 2) & 3) as usize;
        let tag = phys & 0xFFFF_F000;
        (line, word, tag)
    }

    /// Fetch one instruction, filling according to the CXD8606Q's
    /// documented per-word-valid behaviour.
    pub(super) fn fetch(
        &mut self,
        phys: u32,
        iblksz: u8,
        bus: &mut Bus,
        capture_refill: bool,
    ) -> (u32, u32, Option<Refill>) {
        let (line_index, word_index, tag) = Self::coordinates(phys);
        let valid_bit = 1 << word_index;
        let line = &mut self.lines[line_index];

        if line.tag == tag && line.valid & valid_bit != 0 {
            return (line.words[word_index], 0, None);
        }

        let victim = capture_refill.then(|| {
            let victim_tag = line.tag;
            (
                victim_tag,
                line.valid,
                victim_tag | ((line_index as u32) << 4),
            )
        });
        let tag_matches = line.tag == tag;
        let filled_words;
        if !tag_matches {
            line.tag = tag;
            line.valid = 0;

            // A tag miss fills from the requested word to the end of
            // the line without wrapping. IBLKSZ=0 is the special two-word
            // mode, but only changes a miss beginning at word zero.
            let end = if iblksz == 0 && word_index == 0 {
                2
            } else {
                WORDS_PER_LINE
            };
            let line_base = phys & !(LINE_BYTES - 1);
            for index in word_index..end {
                line.words[index] = bus.read_instruction32(line_base + (index as u32 * 4));
                line.valid |= 1 << index;
            }
            filled_words = (end - word_index) as u32;
        } else {
            // A matching tag with an invalid requested word refills the
            // complete line, irrespective of IBLKSZ.
            let line_base = phys & !(LINE_BYTES - 1);
            for index in 0..WORDS_PER_LINE {
                line.words[index] = bus.read_instruction32(line_base + (index as u32 * 4));
            }
            line.valid = 0xF;
            filled_words = WORDS_PER_LINE as u32;
        }

        let refill = victim.map(|(victim_tag, victim_valid_mask, victim_line)| Refill {
            set: line_index as u8,
            incoming_line: phys & !(LINE_BYTES - 1),
            incoming_tag: tag,
            victim_line,
            victim_tag,
            victim_valid_mask,
            tag_miss: !tag_matches,
            fill_words: filled_words as u8,
        });
        (line.words[word_index], filled_words, refill)
    }

    /// Cache-isolated data-mode word read.
    pub(super) fn read_data(&self, addr: u32) -> u32 {
        let (line, word, _) = Self::coordinates(addr);
        self.lines[line].words[word]
    }

    /// Cache-isolated data-mode word write. Valid bits are intentionally
    /// unchanged; the BIOS clears code RAM separately from cache tags.
    pub(super) fn write_data(&mut self, addr: u32, value: u32) {
        let (line, word, _) = Self::coordinates(addr);
        self.lines[line].words[word] = value;
    }

    /// Cache-isolated tag-mode read. Bits 3:0 expose the valid bits,
    /// bit 4 reports whether the addressed physical tag matches, and the
    /// upper bits leak the selected code word as on hardware.
    pub(super) fn read_tag(&self, addr: u32) -> u32 {
        let (line_index, word_index, tag) = Self::coordinates(addr);
        let line = &self.lines[line_index];
        let tag_match = u32::from(line.tag == tag) << 4;
        (line.words[word_index] & 0xFFFF_FFE0) | tag_match | u32::from(line.valid & 0xF)
    }

    /// Cache-isolated tag-mode write. The address supplies the physical
    /// tag and index; only the low four data bits supply word validity.
    pub(super) fn write_tag(&mut self, addr: u32, value: u32) {
        let (line, _, tag) = Self::coordinates(addr);
        self.lines[line].tag = tag;
        self.lines[line].valid = value as u8 & 0xF;
    }

    pub(super) fn invalidate_all(&mut self) {
        for line in &mut self.lines {
            line.valid = 0;
        }
    }

    #[cfg(test)]
    pub(super) fn line_state(&self, addr: u32) -> (u32, u8, [u32; 4]) {
        let (line, _, _) = Self::coordinates(addr);
        let line = self.lines[line];
        (line.tag, line.valid, line.words)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_hw::memory;

    fn bus() -> Bus {
        Bus::new(vec![0; memory::bios::SIZE]).expect("synthetic BIOS has the required size")
    }

    #[test]
    fn tag_miss_fills_from_requested_word_without_wrapping() {
        let mut bus = bus();
        for (index, value) in [0x10, 0x11, 0x12, 0x13].into_iter().enumerate() {
            bus.write32(0x1000 + index as u32 * 4, value);
        }
        let mut cache = InstructionCache::default();

        let (instruction, filled_words, refill) = cache.fetch(0x1008, 3, &mut bus, true);
        assert_eq!(instruction, 0x12);
        assert_eq!(filled_words, 2);
        let refill = refill.expect("tag miss refill");
        assert_eq!(refill.set, 0);
        assert_eq!(refill.incoming_line, 0x1000);
        assert_eq!(refill.victim_line, 0);
        assert_eq!(refill.victim_valid_mask, 0);
        assert!(refill.tag_miss);
        assert_eq!(refill.fill_words, 2);
        let (tag, valid, words) = cache.line_state(0x1008);
        assert_eq!(tag, 0x1000);
        assert_eq!(valid, 0b1100);
        assert_eq!(words[2..], [0x12, 0x13]);
    }

    #[test]
    fn iblksz_zero_limits_word_zero_miss_then_invalid_hit_refills_all() {
        let mut bus = bus();
        for (index, value) in [0x20, 0x21, 0x22, 0x23].into_iter().enumerate() {
            bus.write32(0x2000 + index as u32 * 4, value);
        }
        let mut cache = InstructionCache::default();

        assert_eq!(cache.fetch(0x2000, 0, &mut bus, true).0, 0x20);
        assert_eq!(cache.line_state(0x2000).1, 0b0011);
        let (instruction, filled_words, refill) = cache.fetch(0x2008, 0, &mut bus, true);
        assert_eq!(instruction, 0x22);
        assert_eq!(filled_words, 4);
        let refill = refill.expect("invalid-word refill");
        assert!(!refill.tag_miss);
        assert_eq!(refill.victim_valid_mask, 0b0011);
        assert_eq!(refill.fill_words, 4);
        assert_eq!(cache.line_state(0x2000).1, 0b1111);
    }

    #[test]
    fn hit_returns_stale_code_until_same_index_tag_is_replaced() {
        let mut bus = bus();
        bus.write32(0x3000, 0xAAAA_AAAA);
        bus.write32(0x4000, 0xBBBB_BBBB);
        let mut cache = InstructionCache::default();

        assert_eq!(cache.fetch(0x3000, 3, &mut bus, true).0, 0xAAAA_AAAA);
        bus.write32(0x3000, 0xCCCC_CCCC);
        assert_eq!(
            cache.fetch(0x3000, 3, &mut bus, true),
            (0xAAAA_AAAA, 0, None)
        );

        // These addresses have the same bits 11:4, so the second tag
        // replaces the first direct-mapped line.
        assert_eq!(cache.fetch(0x4000, 3, &mut bus, true).0, 0xBBBB_BBBB);
        let (instruction, filled_words, refill) = cache.fetch(0x3000, 3, &mut bus, true);
        assert_eq!(instruction, 0xCCCC_CCCC);
        assert_eq!(filled_words, 4);
        let refill = refill.expect("same-set replacement");
        assert_eq!(refill.victim_line, 0x4000);
        assert_eq!(refill.victim_tag, 0x4000);
        assert_eq!(refill.victim_valid_mask, 0b1111);
    }

    #[test]
    fn disabled_refill_capture_preserves_fill_width_without_metadata() {
        let mut bus = bus();
        bus.write32(0x6008, 0xCAFE_BABE);
        bus.write32(0x600c, 0x0123_4567);
        let mut cache = InstructionCache::default();

        let (instruction, filled_words, refill) = cache.fetch(0x6008, 3, &mut bus, false);

        assert_eq!(instruction, 0xCAFE_BABE);
        assert_eq!(filled_words, 2);
        assert_eq!(refill, None);
        assert_eq!(cache.line_state(0x6008).1, 0b1100);
    }

    #[test]
    fn isolated_tag_write_controls_tag_and_per_word_validity() {
        let mut cache = InstructionCache::default();
        cache.write_data(0x5010, 0xDEAD_BEE0);
        cache.write_tag(0x5010, 0b0101);

        let (tag, valid, words) = cache.line_state(0x5010);
        assert_eq!(tag, 0x5000);
        assert_eq!(valid, 0b0101);
        assert_eq!(words[0], 0xDEAD_BEE0);
        assert_eq!(cache.read_tag(0x5010), 0xDEAD_BEF5);

        cache.write_tag(0x5010, 0);
        assert_eq!(cache.line_state(0x5010).1, 0);
    }
}
