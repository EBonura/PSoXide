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
//! * The memory controller's access timings (Expansion 1 to 3, BIOS ROM, SPU, CD-ROM and common
//!   delay, [`psx_hw::memory::timing::ENTRY`]). BIOS versions differ here (OpenBIOS programs a
//!   CD-ROM delay and a common delay of its own), and the CD controller's register timing is
//!   what every polled CD read depends on. The base-address and RAM-size registers are not touched.
//! * The GTE: every control and data register zero. A BIOS leaves it zeroed, but not every one
//!   of them is documented to, and no driver should rely on it.
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

/// What the program found in the machine registers when it started, before
/// [`set_entry_state`] replaced it (feature `entry-census`).
///
/// This is how the hardware tests learn what each BIOS leaves behind; nothing in the SDK reads it.
#[cfg(feature = "entry-census")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Inherited {
    /// cop0 SR.
    pub status: u32,
    /// `I_STAT`.
    pub i_stat: u32,
    /// `I_MASK`.
    pub i_mask: u32,
    /// `DPCR`.
    pub dpcr: u32,
    /// `DICR`.
    pub dicr: u32,
    /// Bit `n` is set when DMA channel `n` was busy.
    pub dma_busy: u32,
    /// Root counter 0 to 2 mode registers.
    pub timer_mode: [u16; 3],
    /// SPUCNT.
    pub spucnt: u16,
    /// SPUSTAT.
    pub spustat: u16,
    /// Main volume, left and right.
    pub main_volume: [u16; 2],
    /// CD input volume, left and right.
    pub cd_volume: [u16; 2],
    /// Reverb output volume, left and right.
    pub reverb_volume: [u16; 2],
    /// Reverb work-area base.
    pub reverb_base: u16,
    /// Reverb enable mask, voices 0 to 15.
    pub reverb_enable: u16,
    /// SIO0 STAT.
    pub sio_status: u32,
    /// SIO0 CTRL.
    pub sio_control: u16,
    /// The four words at the general exception vector.
    pub vector: [u32; 4],
    /// The memory controller's seven access-timing registers, in the order of
    /// [`psx_hw::memory::timing::ENTRY`].
    pub bus_timing: [u32; 7],
}

#[cfg(feature = "entry-census")]
static mut INHERITED: Inherited = Inherited {
    status: 0,
    i_stat: 0,
    i_mask: 0,
    dpcr: 0,
    dicr: 0,
    dma_busy: 0,
    timer_mode: [0; 3],
    spucnt: 0,
    spustat: 0,
    main_volume: [0; 2],
    cd_volume: [0; 2],
    reverb_volume: [0; 2],
    reverb_base: 0,
    reverb_enable: 0,
    sio_status: 0,
    sio_control: 0,
    vector: [0; 4],
    bus_timing: [0; 7],
};

/// The registers as the program found them (all zero before `_start` has run, or off the console).
#[cfg(feature = "entry-census")]
pub fn inherited() -> Inherited {
    // SAFETY: a copy of a static written once by `set_entry_state` at boot, before `main`.
    unsafe { *core::ptr::addr_of!(INHERITED) }
}

/// Read the registers `set_entry_state` is about to replace.
#[cfg(all(feature = "entry-census", target_arch = "mips"))]
fn take_census() {
    use psx_hw::sio::sio0;
    const EXCEPTION_VECTOR: *const u32 = 0x8000_0080 as *const u32;
    let status: u32;
    // SAFETY: reads cop0 SR into $8 (the nop covers MFC0's load delay), then plain register and
    // kernel-RAM reads, all without side effects except that reading a root counter's mode clears
    // its sticky bits, which `set_entry_state` rewrites next.
    unsafe {
        core::arch::asm!("mfc0 $8, $12", "nop", lateout("$8") status, options(nomem, nostack));
        let mut census = Inherited {
            status,
            i_stat: psx_io::irq::pending(),
            i_mask: psx_io::irq::mask(),
            dpcr: psx_io::read_u32(dma::DPCR),
            dicr: psx_io::read_u32(dma::DICR),
            spucnt: psx_io::read_u16(spu::SPUCNT),
            spustat: psx_io::read_u16(spu::SPUSTAT),
            main_volume: [
                psx_io::read_u16(spu::MAIN_VOL_LEFT),
                psx_io::read_u16(spu::MAIN_VOL_RIGHT),
            ],
            cd_volume: [
                psx_io::read_u16(spu::CD_VOL_LEFT),
                psx_io::read_u16(spu::CD_VOL_RIGHT),
            ],
            reverb_volume: [
                psx_io::read_u16(spu::REVERB_VOL_LEFT),
                psx_io::read_u16(spu::REVERB_VOL_RIGHT),
            ],
            reverb_base: psx_io::read_u16(spu::REVERB_WORK_BASE),
            reverb_enable: psx_io::read_u16(spu::mask::REVERB_ENABLE_LO),
            sio_status: psx_io::read_u32(sio0::STAT),
            sio_control: psx_io::read_u16(sio0::CTRL),
            ..Inherited::default()
        };
        for (n, channel) in CHANNELS.iter().enumerate() {
            if psx_io::dma::is_busy(*channel) {
                census.dma_busy |= 1 << n;
            }
        }
        for (n, timer) in [Timer::Timer0, Timer::Timer1, Timer::Timer2]
            .into_iter()
            .enumerate()
        {
            census.timer_mode[n] = psx_io::timers::mode(timer);
        }
        for (n, word) in census.vector.iter_mut().enumerate() {
            *word = core::ptr::read_volatile(EXCEPTION_VECTOR.add(n));
        }
        for (n, (address, _)) in psx_hw::memory::timing::ENTRY.iter().enumerate() {
            census.bus_timing[n] = psx_io::read_u32(*address);
        }
        INHERITED = census;
    }
}

/// Put the machine in the state described in the [module documentation](self).
///
/// Call it from foreground code before any driver is set up. Off the console it does nothing.
#[cfg(target_arch = "mips")]
pub fn set_entry_state() {
    #[cfg(feature = "entry-census")]
    take_census();
    // SAFETY: the registers written are the interrupt, DMA, root-counter, SIO0 and SPU
    // controllers' own, with values their drivers re-program before use; the program has not
    // started a transfer or taken an interrupt it depends on yet, which is the caller's contract.
    unsafe {
        write_status(SR_ENTRY);
        for (address, value) in psx_hw::memory::timing::ENTRY {
            psx_io::write_u32(address, value);
        }
        zero_gte();
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

/// Zero every GTE data and control register. The data registers go first and FLAG (control 31)
/// is among the control ones, so it ends zero. The writes to registers the GTE ignores (LZCR,
/// ORGB, the read-only ones; the moves are `.word`s because the target's assembler has no COP2 mnemonics) do nothing, and IRGB (28) converts to IR1 to IR3 = 0, which the
/// zero writes before it already left.
#[cfg(target_arch = "mips")]
unsafe fn zero_gte() {
    // SAFETY: COP2 register moves of zero with SR.CU2 set by `write_status` just before; no
    // memory or other register is touched.
    unsafe {
        core::arch::asm!(
            ".word 0x48800000", // mtc2 $0, $0
            ".word 0x48800800", // mtc2 $0, $1
            ".word 0x48801000", // mtc2 $0, $2
            ".word 0x48801800", // mtc2 $0, $3
            ".word 0x48802000", // mtc2 $0, $4
            ".word 0x48802800", // mtc2 $0, $5
            ".word 0x48803000", // mtc2 $0, $6
            ".word 0x48803800", // mtc2 $0, $7
            ".word 0x48804000", // mtc2 $0, $8
            ".word 0x48804800", // mtc2 $0, $9
            ".word 0x48805000", // mtc2 $0, $10
            ".word 0x48805800", // mtc2 $0, $11
            ".word 0x48806000", // mtc2 $0, $12
            ".word 0x48806800", // mtc2 $0, $13
            ".word 0x48807000", // mtc2 $0, $14
            ".word 0x48807800", // mtc2 $0, $15
            ".word 0x48808000", // mtc2 $0, $16
            ".word 0x48808800", // mtc2 $0, $17
            ".word 0x48809000", // mtc2 $0, $18
            ".word 0x48809800", // mtc2 $0, $19
            ".word 0x4880a000", // mtc2 $0, $20
            ".word 0x4880a800", // mtc2 $0, $21
            ".word 0x4880b000", // mtc2 $0, $22
            ".word 0x4880b800", // mtc2 $0, $23
            ".word 0x4880c000", // mtc2 $0, $24
            ".word 0x4880c800", // mtc2 $0, $25
            ".word 0x4880d000", // mtc2 $0, $26
            ".word 0x4880d800", // mtc2 $0, $27
            ".word 0x4880e000", // mtc2 $0, $28
            ".word 0x4880e800", // mtc2 $0, $29
            ".word 0x4880f000", // mtc2 $0, $30
            ".word 0x4880f800", // mtc2 $0, $31
            ".word 0x48c00000", // ctc2 $0, $0
            ".word 0x48c00800", // ctc2 $0, $1
            ".word 0x48c01000", // ctc2 $0, $2
            ".word 0x48c01800", // ctc2 $0, $3
            ".word 0x48c02000", // ctc2 $0, $4
            ".word 0x48c02800", // ctc2 $0, $5
            ".word 0x48c03000", // ctc2 $0, $6
            ".word 0x48c03800", // ctc2 $0, $7
            ".word 0x48c04000", // ctc2 $0, $8
            ".word 0x48c04800", // ctc2 $0, $9
            ".word 0x48c05000", // ctc2 $0, $10
            ".word 0x48c05800", // ctc2 $0, $11
            ".word 0x48c06000", // ctc2 $0, $12
            ".word 0x48c06800", // ctc2 $0, $13
            ".word 0x48c07000", // ctc2 $0, $14
            ".word 0x48c07800", // ctc2 $0, $15
            ".word 0x48c08000", // ctc2 $0, $16
            ".word 0x48c08800", // ctc2 $0, $17
            ".word 0x48c09000", // ctc2 $0, $18
            ".word 0x48c09800", // ctc2 $0, $19
            ".word 0x48c0a000", // ctc2 $0, $20
            ".word 0x48c0a800", // ctc2 $0, $21
            ".word 0x48c0b000", // ctc2 $0, $22
            ".word 0x48c0b800", // ctc2 $0, $23
            ".word 0x48c0c000", // ctc2 $0, $24
            ".word 0x48c0c800", // ctc2 $0, $25
            ".word 0x48c0d000", // ctc2 $0, $26
            ".word 0x48c0d800", // ctc2 $0, $27
            ".word 0x48c0e000", // ctc2 $0, $28
            ".word 0x48c0e800", // ctc2 $0, $29
            ".word 0x48c0f000", // ctc2 $0, $30
            ".word 0x48c0f800", // ctc2 $0, $31
            options(nomem, nostack)
        );
    }
}

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
    fn the_gte_zeroing_words_are_the_moves_they_say() {
        // `zero_gte` spells its COP2 moves as raw words; this pins the first and last of each run
        // to the encoders the rest of the SDK uses.
        assert_eq!(0x4880_0000, psx_hw::gte::mtc2(0, 0));
        assert_eq!(0x4880_f800, psx_hw::gte::mtc2(0, 31));
        assert_eq!(0x48c0_0000, psx_hw::gte::ctc2(0, 0));
        assert_eq!(0x48c0_f800, psx_hw::gte::ctc2(0, 31));
    }

    #[test]
    fn the_bus_timing_entry_writes_only_delay_registers() {
        use psx_hw::memory::timing::{COMMON, ENTRY, EXPANSION1};
        // Only the seven delay registers, which sit together after the two expansion base
        // addresses and well before the RAM size register: moving any of those would take the
        // program's own memory away.
        for (address, _) in ENTRY {
            assert!((EXPANSION1..=COMMON).contains(&address), "{address:#x}");
            assert_eq!(address % 4, 0);
        }
        let mut seen = [0u32; 7];
        for (n, (address, _)) in ENTRY.iter().enumerate() {
            assert!(!seen[..n].contains(address), "{address:#x} twice");
            seen[n] = *address;
        }
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
