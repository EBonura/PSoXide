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

// The 512Kx8 EDO DRAMs used for main RAM require 1024 refresh cycles per
// 16 ms. At the 33.8688 MHz CPU clock that is 529.2 clocks between refreshes.
// A refresh occupies roughly one 110 ns DRAM cycle, rounded to four CPU
// clocks. Keep the deterministic integer cadence here; the physical test
// suite observes the resulting occasional extra RAM-read latency and relies
// on it to avoid phase-locking tight timer-sampling loops.
const DRAM_REFRESH_PERIOD_CYCLES: u64 = 529;
const DRAM_REFRESH_BUSY_CYCLES: u64 = 4;

/// Remaining CPU clocks in the current main-RAM refresh slot. A request that
/// arrives outside the four-clock slot proceeds with the ordinary wait-state
/// cost; one that arrives during it waits until DRAM is available again.
pub(super) fn dram_refresh_wait(now: u64) -> u32 {
    let phase = now % DRAM_REFRESH_PERIOD_CYCLES;
    if phase < DRAM_REFRESH_BUSY_CYCLES {
        (DRAM_REFRESH_BUSY_CYCLES - phase) as u32
    } else {
        0
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
                0x0002_0843, // CD-ROM delay/size
                0x0007_0777, // expansion 2 delay/size
                0x0003_1125, // common delay
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
            // The public access-time suite measures approximately five total
            // cycles for all RAM widths: one issue plus four stalls.
            return 4;
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
            // is consistently about three total cycles in the silicon log.
            return 2;
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
            words
        } else if (memory::bios::BASE..memory::bios::BASE + memory::bios::SIZE as u32)
            .contains(&phys)
        {
            self.bios_stalls[AccessWidth::Word.index()] * words
        } else {
            0
        }
    }

    fn recalculate(&mut self) {
        let common = self.regs[8];
        self.exp1_stalls = calculate_stalls(self.regs[2], common);
        self.exp3_stalls = calculate_stalls(self.regs[3], common);
        self.bios_stalls = calculate_stalls(self.regs[4], common);
        self.spu_stalls = calculate_stalls(self.regs[5], common).map(|stall| {
            // CXD2922Q SPU-register reads complete three cycles sooner
            // than the generic memory-controller equation predicts.
            // JaCzekanski's silicon log measures 17.99 cycles for byte
            // and halfword SPUCNT reads with the retail register values;
            // the uncorrected equation produces 21.
            stall.saturating_sub(3)
        });
        self.cdrom_stalls = calculate_stalls(self.regs[6], common).map(|stall| {
            // The CD-ROM register bridge adds one cycle beyond the generic
            // programmed wait: silicon measures 8/14/25.93 cycles versus
            // the equation's 7/13/25 for byte/half/word reads.
            stall.saturating_add(1)
        });
        let exp2 = calculate_stalls(self.regs[7], common);
        // Expansion 2 has additional fixed decode shortcuts not represented
        // by the generic equation. At the retail 0x00070777 setting the
        // measured totals are 10.99/25.99/55.98 rather than 15/29/57.
        self.exp2_stalls = [
            exp2[0].saturating_sub(4),
            exp2[1].saturating_sub(3),
            exp2[2].saturating_sub(1),
        ];
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
        assert_eq!(control.bios_stalls, [6, 12, 24]);
        assert_eq!(control.read_stalls(0xBFC0_0000, AccessWidth::Byte), 6);
        assert_eq!(control.read_stalls(0xBFC0_0000, AccessWidth::Half), 12);
        assert_eq!(control.read_stalls(0xBFC0_0000, AccessWidth::Word), 24);
    }

    #[test]
    fn ram_and_scratchpad_have_width_independent_internal_timing() {
        let control = MemoryControl::default();
        for width in [AccessWidth::Byte, AccessWidth::Half, AccessWidth::Word] {
            assert_eq!(control.read_stalls(0x8000_0000, width), 4);
            assert_eq!(control.read_stalls(0x1F80_0000, width), 0);
        }
    }

    #[test]
    fn dram_refresh_blocks_only_the_four_clock_refresh_slot() {
        assert_eq!(dram_refresh_wait(0), 4);
        assert_eq!(dram_refresh_wait(1), 3);
        assert_eq!(dram_refresh_wait(3), 1);
        assert_eq!(dram_refresh_wait(4), 0);
        assert_eq!(dram_refresh_wait(DRAM_REFRESH_PERIOD_CYCLES - 1), 0);
        assert_eq!(dram_refresh_wait(DRAM_REFRESH_PERIOD_CYCLES), 4);
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
    fn cache_refill_distinguishes_ram_burst_from_bios_rom() {
        let control = MemoryControl::default();
        assert_eq!(control.icache_fill_stalls(0x0001_0000, 4), 4);
        assert_eq!(control.icache_fill_stalls(memory::bios::BASE, 4), 96);
    }

    #[test]
    fn peripheral_bridge_adjustments_match_public_silicon_log() {
        let control = MemoryControl::default();
        assert_eq!(control.cdrom_stalls, [7, 13, 25]);
        assert_eq!(control.spu_stalls, [17, 17, 37]);
        assert_eq!(control.exp2_stalls, [10, 25, 55]);
        assert_eq!(control.exp3_stalls, [5, 5, 9]);
        for width in [AccessWidth::Byte, AccessWidth::Half, AccessWidth::Word] {
            assert_eq!(control.read_stalls(memory::cache_control::ADDR, width), 0);
        }
    }
}
