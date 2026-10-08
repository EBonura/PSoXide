// SPDX-License-Identifier: GPL-2.0-or-later
//! Area 0: what the BIOS left behind, read before the runtime or the SDK
//! touches anything.
//!
//! `capture` runs first thing in `main`, before `App::run` replaces the
//! exception vector and before any SPU initialisation. The run reports the
//! snapshot as records and never writes the hardware it describes. The reverb
//! half is what PA5 established on silicon (2026-07-20): the retail BIOS hands
//! over a live full reverb preset, and a later bank DMA over its work area was
//! the real-console noise. The SDK's `spu::init` now zeroes the wet depth and
//! parks the work area; `post_init_reverb` is the standing check that it does.

use crate::payload::fnv16_bytes;
use crate::TimingRecord;

const VECTOR: *const u32 = 0x8000_0080 as *const u32;
const REVERB_VOL_L: u32 = 0x1F80_1D84;
const REVERB_VOL_R: u32 = 0x1F80_1D86;
const EON_LO: u32 = 0x1F80_1D98;
const REVERB_BASE: u32 = 0x1F80_1DA2;
const REVERB_CFG_BASE: u32 = 0x1F80_1DC0;
const STANDARD_VECTOR: [u32; 4] = [0x3C1A_0000, 0x275A_0C80, 0x0340_0008, 0];

/// rec boot_vector: bios_stub_standard, vector_hash_lo, vector_hash_hi
pub(crate) const BOOT_VECTOR: u16 = 0x400;
/// rec boot_reverb_a: spucnt, spustat, reverb_volume_left
pub(crate) const BOOT_REVERB_A: u16 = 0x401;
/// rec boot_reverb_b: reverb_volume_right, reverb_work_base, eon_low
pub(crate) const BOOT_REVERB_B: u16 = 0x402;
/// rec boot_reverb_c: config_hash_low, config_hash_high, config_nonzero_words
pub(crate) const BOOT_REVERB_C: u16 = 0x403;
/// rec post_init_reverb: volume_left, volume_right, work_base (all after spu init)
pub(crate) const POST_INIT_REVERB: u16 = 0x404;

#[derive(Copy, Clone)]
struct Snapshot {
    vector: [u32; 4],
    spucnt: u16,
    spustat: u16,
    reverb_vol_l: u16,
    reverb_vol_r: u16,
    reverb_base: u16,
    eon_lo: u16,
    cfg: [u16; 32],
}

static mut SNAPSHOT: Snapshot = Snapshot {
    vector: [0; 4],
    spucnt: 0,
    spustat: 0,
    reverb_vol_l: 0,
    reverb_vol_r: 0,
    reverb_base: 0,
    eon_lo: 0,
    cfg: [0; 32],
};

/// Read the BIOS-owned state. Call once from `main`, before `App::run`.
pub(crate) fn capture() {
    // SAFETY: plain MMIO and kernel-RAM reads on the one thread, before any
    // handler of ours exists; the static is written once.
    unsafe {
        let read = |addr| psx_io::read_u16(addr);
        let mut snapshot = Snapshot {
            vector: [0; 4],
            spucnt: read(psx_hw::spu::SPUCNT),
            spustat: read(psx_hw::spu::SPUSTAT),
            reverb_vol_l: read(REVERB_VOL_L),
            reverb_vol_r: read(REVERB_VOL_R),
            reverb_base: read(REVERB_BASE),
            eon_lo: read(EON_LO),
            cfg: [0; 32],
        };
        for (i, word) in snapshot.vector.iter_mut().enumerate() {
            *word = core::ptr::read_volatile(VECTOR.add(i));
        }
        for (i, value) in snapshot.cfg.iter_mut().enumerate() {
            *value = read(REVERB_CFG_BASE + i as u32 * 2);
        }
        SNAPSHOT = snapshot;
    }
}

fn record(id: u16, a: u32, b: u32, c: u32) -> TimingRecord {
    crate::console_tests::record(id, a, b, c)
}

/// The snapshot as records.
pub(crate) fn records() -> [TimingRecord; 4] {
    // SAFETY: written once in `capture`, read-only afterwards.
    let s = unsafe { SNAPSHOT };
    let mut bytes = [0u8; 16];
    for (i, word) in s.vector.iter().enumerate() {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    let vector_hash = fnv16_bytes(&bytes);
    let mut cfg_bytes = [0u8; 64];
    for (i, value) in s.cfg.iter().enumerate() {
        cfg_bytes[i * 2..i * 2 + 2].copy_from_slice(&value.to_le_bytes());
    }
    let cfg_hash = fnv16_bytes(&cfg_bytes);
    let nonzero = s.cfg.iter().filter(|&&v| v != 0).count() as u32;
    [
        record(
            BOOT_VECTOR,
            (s.vector == STANDARD_VECTOR) as u32,
            vector_hash & 0xFFFF,
            vector_hash >> 16,
        ),
        record(
            BOOT_REVERB_A,
            s.spucnt as u32,
            s.spustat as u32,
            s.reverb_vol_l as u32,
        ),
        record(
            BOOT_REVERB_B,
            s.reverb_vol_r as u32,
            s.reverb_base as u32,
            s.eon_lo as u32,
        ),
        record(BOOT_REVERB_C, cfg_hash & 0xFFFF, cfg_hash >> 16, nonzero),
    ]
}

/// Reverb wet depth and work-area base as `spu::init` leaves them.
pub(crate) fn post_init_reverb() -> TimingRecord {
    // SAFETY: plain SPU register reads.
    let (l, r, base) = unsafe {
        (
            psx_io::read_u16(REVERB_VOL_L),
            psx_io::read_u16(REVERB_VOL_R),
            psx_io::read_u16(REVERB_BASE),
        )
    };
    record(POST_INIT_REVERB, l as u32, r as u32, base as u32)
}
