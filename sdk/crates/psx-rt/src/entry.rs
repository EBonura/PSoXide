//! The machine state a program starts from, set rather than inherited.
//!
//! Whatever booted a program (any of the Sony BIOS versions, OpenBIOS, a chain loader
//! handing over from another program) leaves the interrupt controller, DMA, the SPU, the
//! root counters and the cop0 status register in a state of its own. Sony versions do not
//! agree with each other (a retail console handed over a DMA priority register of `0x9099`,
//! not the controller's power-on `0x07654321`), and OpenBIOS differs again. A program that
//! only ORs its own bits into those registers behaves differently on each. [`set_entry_state`]
//! writes every register the SDK's drivers rely on to one documented value instead, so the
//! program starts the same everywhere.
//!
//! `_start` calls it once, after `.bss` is zeroed. The demo disc's chain loader calls it
//! between programs, so a chain-loaded game starts exactly as a game booted from a BIOS does.
//!
//! What it sets, in order:
//!
//! * cop0 SR: only CU2 (the GTE) on. Interrupts off, kernel mode, normal exception vectors,
//!   cache not isolated, no interrupt line unmasked. A driver that wants interrupts turns them
//!   on itself ([`crate::interrupts::install_vblank_counter`]).
//! * `I_MASK` zero and every `I_STAT` bit acknowledged.
//! * DMA: every channel is given its bounded chance to finish, then aborted, then `DPCR`
//!   becomes [`psx_hw::dma::DPCR_ENTRY`] (no channel enabled, the controller's own priority
//!   ladder) and `DICR` is cleared ([`psx_hw::dma::DICR_ENTRY`]). Every driver enables the
//!   channel it uses before the first transfer.
//! * Root counters 0 to 2: mode 0 (system clock, free running, no IRQ, no target), counting from 0.
//!   Drivers that need another clock select it.
//! * SIO0 (pads and cards): UART reset, port deselected. Every transaction programs mode and
//!   baud itself.
//! * SPU: every voice keyed off, reverb output and pitch modulation and noise off, main volume
//!   at its maximum, CD input volume at silence, CD audio input off, the SPU enabled and
//!   unmuted with transfer mode stopped. A program that plays XA or CD-DA turns the input on
//!   and sets its volume (`psx_spu::enable_cd_audio` and `set_cd_volume`) without having to
//!   know what the BIOS left in the control register.
//! * The instruction cache is flushed and the cache control register set to its running value
//!   ([`crate::cache::flush_instruction_cache`]), so the scratchpad and the caches are
//!   configured by the program and not by the BIOS.
//!
//! Every wait in it is a counted loop. Nothing here reads a timer, a BIOS table or kernel RAM.
//!
//! It does not touch the exception vector: the word found there is recorded by `_start`
//! ([`crate::interrupts`]) and replaced only when a driver installs a handler.

#[cfg(target_arch = "mips")]
use psx_hw::{dma, spu};
#[cfg(target_arch = "mips")]
use psx_io::dma::Channel;
#[cfg(target_arch = "mips")]
use psx_io::timers::Timer;

/// cop0 SR as a program starts: CU2 only.
pub const SR_ENTRY: u32 = 1 << 30;

/// Spins to wait for the SPU to apply a control-register write.
#[cfg(target_arch = "mips")]
const SPU_APPLY_SPINS: u32 = 100_000;

/// Every DMA channel, for the quiesce.
#[cfg(target_arch = "mips")]
const CHANNELS: [Channel; 7] = [
    Channel::MdecIn,
    Channel::MdecOut,
    Channel::Gpu,
    Channel::Cd,
    Channel::Spu,
    Channel::Expansion,
    Channel::OrderingTableClear,
];

/// Put the machine in the state described in the [module documentation](self).
///
/// Call it from foreground code before any driver is set up. Off the console it does nothing.
#[cfg(target_arch = "mips")]
pub fn set_entry_state() {
    // SAFETY: the registers written are the interrupt, DMA, root-counter, SIO0 and SPU
    // controllers' own, with values their drivers re-program before use; the program has not
    // started a transfer or taken an interrupt it depends on yet, which is the caller's contract.
    unsafe {
        write_status(SR_ENTRY);
        psx_io::irq::set_mask(0);
        psx_io::irq::acknowledge(u32::MAX);

        // Interrupts are closed, so nothing can start another transfer while this waits.
        for channel in CHANNELS {
            let _ = psx_io::dma::wait_or_abort(channel, psx_io::dma::DEFAULT_SPINS);
        }
        psx_io::write_u32(dma::DPCR, dma::DPCR_ENTRY);
        psx_io::write_u32(dma::DICR, dma::DICR_ENTRY);

        for timer in [Timer::Timer0, Timer::Timer1, Timer::Timer2] {
            psx_io::timers::set_mode(timer, 0);
        }

        psx_io::write_u16(psx_hw::sio::sio0::CTRL, psx_hw::sio::sio0::ctrl::RESET);
        psx_io::write_u16(psx_hw::sio::sio0::CTRL, 0);

        spu_entry_state();

        // Last: the timers and SIO0 above may have latched a bit.
        psx_io::irq::acknowledge(u32::MAX);
    }
    crate::cache::flush_instruction_cache();
}

/// Off the console there is no machine to set up.
#[cfg(not(target_arch = "mips"))]
pub fn set_entry_state() {}

/// Write cop0 SR.
#[cfg(target_arch = "mips")]
unsafe fn write_status(value: u32) {
    // SAFETY: the caller passes a value that leaves interrupts disabled; the nops let the write
    // settle before the next instruction.
    unsafe { core::arch::asm!("mtc0 $8, $12", "nop", "nop", in("$8") value, options(nostack)) };
}

/// The SPU part of [`set_entry_state`]: the register writes of `psx_spu::Spu::new` that put the
/// chip in a known state, without its sample-memory upload.
#[cfg(target_arch = "mips")]
unsafe fn spu_entry_state() {
    // SAFETY: 16-bit writes to the SPU's own registers, as the SPU driver makes them.
    unsafe {
        psx_io::write_u16(spu::mask::KEY_OFF_LO, 0xffff);
        psx_io::write_u16(spu::mask::KEY_OFF_HI, 0x00ff);
        for mask in [
            spu::mask::REVERB_ENABLE_LO,
            spu::mask::REVERB_ENABLE_HI,
            spu::mask::PITCH_MOD_LO,
            spu::mask::PITCH_MOD_HI,
            spu::mask::NOISE_LO,
            spu::mask::NOISE_HI,
            spu::REVERB_VOL_LEFT,
            spu::REVERB_VOL_RIGHT,
            spu::CD_VOL_LEFT,
            spu::CD_VOL_RIGHT,
        ] {
            psx_io::write_u16(mask, 0);
        }
        // The reverb work area parked at the top of sound RAM, as the SPU driver does: a base
        // left low in memory reserves most of it for the reverb engine.
        psx_io::write_u16(spu::REVERB_WORK_BASE, 0xfffe);
        psx_io::write_u16(spu::MAIN_VOL_LEFT, MAIN_VOLUME);
        psx_io::write_u16(spu::MAIN_VOL_RIGHT, MAIN_VOLUME);

        // Enable, then unmute, waiting for the chip to apply each (SPUCNT's low bits take effect
        // after a delay); transfer mode stopped, CD input off.
        set_control(spu::control::ENABLE);
        set_control(spu::control::ENABLE | spu::control::UNMUTE);
        psx_io::write_u16(spu::TRANSFER_CTRL, spu::TRANSFER_CTRL_NORMAL);
    }
}

/// Main volume register value for the maximum volume (fixed-volume mode, 0x3FFF).
#[cfg(target_arch = "mips")]
const MAIN_VOLUME: u16 = 0x3fff;

/// Write SPUCNT and wait, counted, for SPUSTAT to show the applied low bits.
#[cfg(target_arch = "mips")]
unsafe fn set_control(value: u16) {
    // SAFETY: 16-bit accesses to SPUCNT and SPUSTAT.
    unsafe {
        psx_io::write_u16(spu::SPUCNT, value);
        let want = value & spu::control::MODE_MASK;
        let mut spins = 0;
        while psx_io::read_u16(spu::SPUSTAT) & spu::status::MODE_MASK != want
            && spins < SPU_APPLY_SPINS
        {
            spins += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_status_word_enables_only_the_gte() {
        // Bit 0 (interrupt enable), bits 8..=15 (interrupt masks), bit 16 (cache isolate), bit 22 (BEV) and the other CU bits are 0.
        assert_eq!(SR_ENTRY, 0x4000_0000);
    }

    #[test]
    fn the_dma_entry_values_enable_nothing() {
        let enables: u32 = (0..7).map(psx_hw::dma::dpcr_enable).sum();
        assert_eq!(psx_hw::dma::DPCR_ENTRY & enables, 0);
        // Priorities 1..=7 in channel order.
        for ch in 0..7u32 {
            assert_eq!((psx_hw::dma::DPCR_ENTRY >> (4 * ch)) & 0x7, ch + 1);
        }
        // Only flag bits 24..=30 are written: acknowledges all, enables none.
        assert_eq!(psx_hw::dma::DICR_ENTRY & !0x7f00_0000, 0);
    }
}
