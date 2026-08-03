//! CPU-facing memory-control registers and access timing.
//!
//! The CXD8514Q memory controller exposes nine registers at
//! `0x1F80_1000..=0x1F80_1020`. Four of the delay registers feed the
//! external bus wait-state generator. Keeping the decoded timing here makes
//! data loads, uncached instruction fetches, and I-cache fills use one model.
//!
//! The delay equation follows the public PSX memory-controller description
//! and is independently checked against JaCzekanski's `cpu/access-time`
//! silicon log. Values returned by this module are *stall* cycles beyond the
//! CPU instruction's normal one-cycle issue cost.

use psx_hw::memory;

pub(super) const BASE: u32 = 0x1F80_1000;
pub(super) const END: u32 = BASE + 9 * 4;

const MEM_DELAY_WRITE_MASK: u32 = 0xAF1F_FFFF;
const COMMON_DELAY_WRITE_MASK: u32 = 0x0003_FFFF;

// The late PAL CXD8606CQ capture exposes an ordinary 515-clock row cadence.
// The probe observes one clock less (514) because it timestamps after the
// contended load. The complete refresh transaction spans row close, refresh,
// and reopen phases; individual accesses only expose the remaining portion.
pub(super) const DRAM_REFRESH_PERIOD_CYCLES: u64 = 515;
pub(super) const DRAM_REFRESH_CACHED_STALL_CYCLES: u32 = 8;
pub(super) const DRAM_REFRESH_UNCACHED_STALL_CYCLES: u32 = 4;

/// Arbitrate a pending main-RAM refresh request at the next CPU RAM access.
pub(super) fn dram_refresh_wait(now: u64, next_deadline: &mut u64, stall_cycles: u32) -> u32 {
    if now < *next_deadline {
        return 0;
    }

    // Refresh requests are generated from an autonomous 515-clock divider,
    // but the memory controller arbitrates the transaction at a CPU RAM
    // access. Advance from the original divider phase so a late request never
    // shifts every subsequent refresh.
    let elapsed = now - *next_deadline;
    let skipped_requests = elapsed / DRAM_REFRESH_PERIOD_CYCLES;
    let request_index = *next_deadline / DRAM_REFRESH_PERIOD_CYCLES + skipped_requests;
    let request_lateness = elapsed % DRAM_REFRESH_PERIOD_CYCLES;
    *next_deadline += (skipped_requests + 1) * DRAM_REFRESH_PERIOD_CYCLES;
    // If the CPU left main RAM idle long enough, the controller completed the
    // refresh autonomously and this later access has nothing to arbitrate.
    // Tight load streams reach the request inside this ten-clock transaction
    // window and pay the complete access-side replay cost.
    if request_lateness > 10 {
        return 0;
    }
    if request_index.is_multiple_of(8) {
        stall_cycles
            + if stall_cycles == DRAM_REFRESH_CACHED_STALL_CYCLES {
                1
            } else if stall_cycles == DRAM_REFRESH_UNCACHED_STALL_CYCLES {
                2
            } else {
                0
            }
    } else {
        stall_cycles
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AccessWidth {
    Byte = 0,
    Half = 1,
    Word = 2,
}

impl AccessWidth {
    #[inline]
    const fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct MemoryControl {
    regs: [u32; 9],
    bios_stalls: [u32; 3],
    spu_stalls: [u32; 3],
    cdrom_stalls: [u32; 3],
    exp1_stalls: [u32; 3],
    exp2_stalls: [u32; 3],
    exp3_stalls: [u32; 3],
}

impl Default for MemoryControl {
    fn default() -> Self {
        // Reset values used by retail PS1 firmware and exposed by the
        // public hardware tests. They are writable: BIOS initialization and
        // homebrew can immediately replace them through the MMIO handlers.
        let mut control = Self {
            regs: [
                0x1F00_0000, // expansion 1 base
                0x1F80_2000, // expansion 2 base
                0x0013_243F, // expansion 1 delay/size
                0x0000_3022, // expansion 3 delay/size
                0x0013_243F, // BIOS delay/size
                0x2009_31E1, // SPU delay/size
                0x0002_0943, // CD-ROM delay/size (late PAL retail hardware)
                0x0007_0777, // expansion 2 delay/size
                0x0000_132C, // common delay (SCPH-9902 PAL silicon capture)
            ],
            bios_stalls: [0; 3],
            spu_stalls: [0; 3],
            cdrom_stalls: [0; 3],
            exp1_stalls: [0; 3],
            exp2_stalls: [0; 3],
            exp3_stalls: [0; 3],
        };
        control.recalculate();
        control
    }
}

impl MemoryControl {
    #[inline]
    pub(super) fn contains(phys: u32) -> bool {
        (BASE..END).contains(&phys)
    }

    pub(super) fn read(&self, phys: u32, width: AccessWidth) -> u32 {
        let offset = phys - BASE;
        let index = (offset >> 2) as usize;
        let shift = (offset & 3) * 8;
        let value = self.regs[index];
        match width {
            AccessWidth::Byte => (value >> shift) & 0xFF,
            AccessWidth::Half => (value >> shift) & 0xFFFF,
            AccessWidth::Word => value,
        }
    }

    pub(super) fn write(&mut self, phys: u32, width: AccessWidth, value: u32) {
        let offset = phys - BASE;
        let index = (offset >> 2) as usize;
        let shift = (offset & 3) * 8;
        let lane_mask = match width {
            AccessWidth::Byte => 0xFF,
            AccessWidth::Half => 0xFFFF,
            AccessWidth::Word => u32::MAX,
        } << shift;
        let write_mask = match index {
            0 | 1 => u32::MAX,
            8 => COMMON_DELAY_WRITE_MASK,
            _ => MEM_DELAY_WRITE_MASK,
        } & lane_mask;
        let merged = (self.regs[index] & !write_mask) | ((value << shift) & write_mask);
        if merged != self.regs[index] {
            self.regs[index] = merged;
            if index >= 2 {
                self.recalculate();
            }
        }
    }

    /// CPU load stall beyond the load instruction's one issue cycle.
    pub(super) fn read_stalls(&self, virt: u32, width: AccessWidth) -> u32 {
        if virt == memory::cache_control::ADDR {
            // BIU_CONFIG is CPU-local rather than external MMIO. The public
            // silicon suite measures 0.95/1.09/1.09 total cycles, i.e. the
            // normal issue cycle with no width-dependent bus stall.
            return 0;
        }

        let phys = memory::to_physical(virt);
        if phys < memory::ram::MIRROR_END {
            // The SCPH-9902 timing disc measures approximately seven total
            // cycles for all RAM widths: one issue plus six wait cycles.
            // Its repeated LW/LB/LH probes resolve to 8 clocks per
            // load+delay-slot pair, while scratchpad remains one clock.
            return 6;
        }
        if (memory::scratchpad::BASE..memory::scratchpad::BASE + memory::scratchpad::SIZE as u32)
            .contains(&phys)
            && !(0xA000_0000..0xC000_0000).contains(&virt)
        {
            return 0;
        }
        if (memory::bios::BASE..memory::bios::BASE + memory::bios::SIZE as u32).contains(&phys) {
            return self.bios_stalls[width.index()];
        }
        if (memory::expansion1::BASE..memory::expansion1::BASE + memory::expansion1::SIZE as u32)
            .contains(&phys)
        {
            return self.exp1_stalls[width.index()];
        }
        if (memory::expansion2::BASE..memory::expansion2::BASE + memory::expansion2::SIZE as u32)
            .contains(&phys)
        {
            return self.exp2_stalls[width.index()];
        }
        if (memory::expansion3::BASE..memory::expansion3::BASE + memory::expansion3::SIZE as u32)
            .contains(&phys)
        {
            return self.exp3_stalls[width.index()];
        }
        if (0x1F80_1800..0x1F80_1804).contains(&phys) {
            return self.cdrom_stalls[width.index()];
        }
        if (0x1F80_1C00..0x1F80_2000).contains(&phys) {
            return self.spu_stalls[width.index()];
        }
        if (memory::io::BASE..memory::io::BASE + memory::io::SIZE as u32).contains(&phys) {
            // Internal MMIO (DMA, GPU, timers, IRQ, SIO, memory control)
            // is consistently about five total cycles on SCPH-9902: one
            // issue plus four wait cycles. The independent GPUSTAT, I_STAT,
            // SIO_STAT and memory-control probes all report the same slope.
            return 4;
        }
        2
    }

    /// Stall for an uncached instruction read. RAM instruction fetches use
    /// the external bus's six-cycle path; BIOS uses its programmed word wait.
    pub(super) fn instruction_read_stalls(&self, virt: u32) -> u32 {
        let phys = memory::to_physical(virt);
        if phys < memory::ram::MIRROR_END {
            6
        } else if (memory::bios::BASE..memory::bios::BASE + memory::bios::SIZE as u32)
            .contains(&phys)
        {
            self.bios_stalls[AccessWidth::Word.index()]
        } else {
            self.read_stalls(virt, AccessWidth::Word)
        }
    }

    /// I-cache refill cost. RAM supports one word per cycle during a line
    /// burst; BIOS refills retain the programmed 8-bit ROM word wait.
    pub(super) fn icache_fill_stalls(&self, phys: u32, words: u32) -> u32 {
        if words == 0 {
            return 0;
        }
        if phys < memory::ram::MIRROR_END {
            // Main RAM bursts one word per clock after a one-clock line-fill
            // setup. The 4 KiB cold-cache sweep resolves exactly one setup
            // clock for each of its 256 four-word lines.
            words.saturating_add(1)
        } else if (memory::bios::BASE..memory::bios::BASE + memory::bios::SIZE as u32)
            .contains(&phys)
        {
            self.bios_stalls[AccessWidth::Word.index()] * words
        } else {
            0
        }
    }

    /// SPU DMA FIFO service cadence for the active memory-controller profile.
    /// Earlier COMMON0=5 silicon measurements expose the familiar 16 clocks
    /// per halfword. The late PAL COMMON0=12 capture takes 16,688 clocks for
    /// 512 halfwords, identifying a 32-clock cadence plus fixed polling cost.
    pub(super) fn spu_dma_halfword_cycles(&self) -> u32 {
        if self.regs[8] & 0xF <= 5 {
            16
        } else {
            32
        }
    }

    /// Total channel-4 completion time for `word_count` 32-bit RAM words.
    /// The late profile exposes both the 32-clock SPU halfword cadence and
    /// the source-side DRAM hyper-page transfer (17 clocks per 16 words).
    pub(super) fn spu_dma_cycles(&self, word_count: u32) -> u32 {
        let fifo_cycles = word_count
            .saturating_mul(2)
            .saturating_mul(self.spu_dma_halfword_cycles());
        if self.regs[8] & 0xF <= 5 {
            fifo_cycles
        } else {
            fifo_cycles
                .saturating_add(word_count)
                .saturating_add(word_count.div_ceil(16))
        }
    }

    /// SPU RAM reads are stable only when the delay register's DMA timing
    /// override nibble is non-zero. With the BIOS value (`0x200931E1`) the
    /// controller inserts a dirty halfword at each DMA block boundary.
    pub(super) fn spu_dma_read_is_stable(&self) -> bool {
        (self.regs[5] >> 24) & 0xF != 0
    }

    fn recalculate(&mut self) {
        let common = self.regs[8];
        self.exp1_stalls = calculate_stalls(self.regs[2], common);
        self.exp3_stalls = calculate_stalls(self.regs[3], common).map(|stall| {
            // Expansion 3 crosses an additional two-cycle bridge on the
            // late PAL machine.  Its byte/half/word sweep measures
            // 575/569/821 cycles for 64 accesses, consistently about two
            // cycles per access above the programmable wait-state equation.
            stall.saturating_add(2)
        });
        self.bios_stalls = calculate_stalls(self.regs[4], common);
        let legacy_shortcuts = common & 0xF <= 5;
        self.spu_stalls = calculate_stalls(self.regs[5], common).map(|stall| {
            // The earlier public silicon profile uses COMMON0=5 and takes a
            // three-cycle SPU bridge shortcut. The late PAL COMMON0=12
            // capture does not: its 27/27/54-cycle byte/half/word accesses
            // follow the programmed memory-controller equation directly.
            if legacy_shortcuts {
                stall.saturating_sub(3)
            } else {
                stall
            }
        });
        self.cdrom_stalls = calculate_stalls(self.regs[6], common).map(|stall| {
            // Likewise, the one-cycle CD bridge surcharge is visible in the
            // earlier short-COMMON0 profile but not in the late PAL timings.
            if legacy_shortcuts {
                stall.saturating_add(1)
            } else {
                stall
            }
        });
        let exp2 = calculate_stalls(self.regs[7], common);
        self.exp2_stalls = if legacy_shortcuts {
            // Expansion 2 has fixed decode shortcuts under the earlier
            // COMMON0=5 profile: 10.99/25.99/55.98 total cycles instead of
            // the generic equation's 15/29/57.
            [
                exp2[0].saturating_sub(4),
                exp2[1].saturating_sub(3),
                exp2[2].saturating_sub(1),
            ]
        } else {
            // Late PAL Expansion 2 keeps one additional sequential clock per
            // halfword; a 32-bit access contains three such boundaries.
            [
                exp2[0],
                exp2[1].saturating_add(1),
                exp2[2].saturating_add(3),
            ]
        };
    }
}

/// Decode the programmable wait-state equation. The result is one less than
/// the complete access time because the instruction issue cycle is charged by
/// the CPU itself.
fn calculate_stalls(delay: u32, common: u32) -> [u32; 3] {
    let access = ((delay >> 4) & 0xF) as i32;
    let use_com0 = delay & (1 << 8) != 0;
    let use_com2 = delay & (1 << 10) != 0;
    let use_com3 = delay & (1 << 11) != 0;
    let bus_16bit = delay & (1 << 12) != 0;
    let com0 = (common & 0xF) as i32;
    let com2 = ((common >> 8) & 0xF) as i32;
    let com3 = ((common >> 12) & 0xF) as i32;

    let mut first = 0i32;
    let mut sequential = 0i32;
    let mut minimum = 0i32;
    if use_com0 {
        first += com0 - 1;
        sequential += com0 - 1;
    }
    if use_com2 {
        first += com2;
        sequential += com2;
    }
    if use_com3 {
        minimum = com3;
    }
    if first < 6 {
        first += 1;
    }
    first += access + 2;
    sequential += access + 2;
    first = first.max(minimum + 6);
    sequential = sequential.max(minimum + 2);

    let byte = first;
    let half = if bus_16bit { first } else { first + sequential };
    let word = if bus_16bit {
        first + sequential
    } else {
        first + sequential * 3
    };
    [
        byte.saturating_sub(1) as u32,
        half.saturating_sub(1) as u32,
        word.saturating_sub(1) as u32,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retail_bios_waits_match_silicon_access_time_shape() {
        let control = MemoryControl::default();
        assert_eq!(control.bios_stalls, [8, 16, 32]);
        assert_eq!(control.read_stalls(0xBFC0_0000, AccessWidth::Byte), 8);
        assert_eq!(control.read_stalls(0xBFC0_0000, AccessWidth::Half), 16);
        assert_eq!(control.read_stalls(0xBFC0_0000, AccessWidth::Word), 32);
    }

    #[test]
    fn ram_and_scratchpad_have_width_independent_internal_timing() {
        let control = MemoryControl::default();
        for width in [AccessWidth::Byte, AccessWidth::Half, AccessWidth::Word] {
            assert_eq!(control.read_stalls(0x8000_0000, width), 6);
            assert_eq!(control.read_stalls(0x1F80_0000, width), 0);
        }
    }

    #[test]
    fn dram_refresh_request_keeps_its_divider_phase() {
        let mut deadline = DRAM_REFRESH_PERIOD_CYCLES;
        assert_eq!(dram_refresh_wait(deadline - 1, &mut deadline, 8), 0);
        assert_eq!(dram_refresh_wait(deadline, &mut deadline, 8), 8);
        assert_eq!(deadline, 2 * DRAM_REFRESH_PERIOD_CYCLES);
        assert_eq!(
            dram_refresh_wait(
                deadline + 2 * DRAM_REFRESH_PERIOD_CYCLES + 7,
                &mut deadline,
                4
            ),
            4
        );
        assert_eq!(deadline, 5 * DRAM_REFRESH_PERIOD_CYCLES);
        assert_eq!(dram_refresh_wait(deadline + 11, &mut deadline, 8), 0);
    }

    #[test]
    fn partial_register_writes_recalculate_waits_and_apply_masks() {
        let mut control = MemoryControl::default();
        control.write(BASE + 0x10, AccessWidth::Byte, 0x0F);
        assert_eq!(control.read(BASE + 0x10, AccessWidth::Byte), 0x0F);
        assert_ne!(control.bios_stalls, [6, 12, 24]);

        control.write(BASE + 0x20, AccessWidth::Word, u32::MAX);
        assert_eq!(
            control.read(BASE + 0x20, AccessWidth::Word),
            COMMON_DELAY_WRITE_MASK
        );
    }

    #[test]
    fn spu_dma_cadence_tracks_the_common_delay_profile() {
        let mut control = MemoryControl::default();
        assert_eq!(control.spu_dma_halfword_cycles(), 32);
        assert_eq!(control.spu_dma_cycles(256), 16_656);

        control.write(BASE + 8 * 4, AccessWidth::Word, 0x0003_1125);
        assert_eq!(control.spu_dma_halfword_cycles(), 16);
        assert_eq!(control.spu_dma_cycles(256), 8_192);
    }

    #[test]
    fn cache_refill_distinguishes_ram_burst_from_bios_rom() {
        let control = MemoryControl::default();
        assert_eq!(control.icache_fill_stalls(0x0001_0000, 4), 5);
        assert_eq!(control.icache_fill_stalls(memory::bios::BASE, 4), 128);
    }

    #[test]
    fn peripheral_bridge_adjustments_match_public_silicon_log() {
        let control = MemoryControl::default();
        assert_eq!(control.cdrom_stalls, [16, 33, 67]);
        assert_eq!(control.spu_stalls, [26, 26, 53]);
        assert_eq!(control.exp2_stalls, [22, 46, 94]);
        assert_eq!(control.exp3_stalls, [7, 7, 11]);
        for width in [AccessWidth::Byte, AccessWidth::Half, AccessWidth::Word] {
            assert_eq!(control.read_stalls(memory::cache_control::ADDR, width), 0);
        }
    }
}
