// SPDX-License-Identifier: GPL-2.0-or-later
//! GTE NCLIP VARIANTS: the NCLIP cases that read differently on the console of 2026-11-14 than on every earlier
//! console run, measured four ways with the raw MAC0 kept.
//!
//! Conformance case 22 (`NCLIP MAC0 winding sign`, expects `0x0E`) and cases 116 to 120 (`NCLIP MAC0 settle +1`
//! to `+6`, each compares a read after N nops with a settled reference read) passed on the console in v1.24, v1.28
//! and the 2026-11-03 run and failed on both runs of 2026-11-14: case 22 read `0x0F`, cases 116 to 120 read 0 against
//! a reference of `0x64` (`0x4D8` in case 116). Their source did not change. What did change is the build, the GTE
//! state the program starts with (the entry code now zeroes every GTE register) and the executable's layout. Four
//! readings of the same sequence tell those apart:
//!
//! * `as built`: the sequences of the cases, with no change.
//! * `filled`: the same, after every GTE data and control register is written with a fixed non-zero pattern, before
//!   the winding pair and before each gap's group. If the reading follows the GTE's prior state it moves here.
//! * `extra nop`: the same as built with one more `nop` between the last register write and the `NCLIP`. If the
//!   reading follows the gap between a register write and the command it moves here.
//! * `zeroed`: the same, after every GTE register is written with zero, the state the program's entry code leaves
//!   (it now zeroes every GTE register) and, with `filled`, the two ends of the prior state.
//!
//! If none of the variants moves the reading from `as built`, it follows neither the prior state nor the gap.
//! The step runs last in the GTE area, so `as built` here starts from the state the step before leaves and not from the
//! state case 22 saw; that is what `zeroed` and `filled` pin down.
//!
//! Each variant measures twelve values, each the raw MAC0 (32 bits, as two fields) and the FLAG register's bits:
//!
//! * the positive winding (`SXY0` (0,0), `SXY1` (10,0), `SXY2` (0,10)) after 64 nops (case 22's first read), then the
//!   negative winding after 64 nops (its second);
//! * for each gap of 1, 2, 3, 4 and 6 nops (cases 116 to 120): the settled reference (positive winding, 64 nops), then
//!   the probe (the reversed winding as poison, the positive winding again, read after the gap).
//!
//! Variant `v` (0 as built, 1 filled, 2 extra nop, 3 zeroed) uses records `0x980 + 12 * v + k`: `k` 0 the positive winding, 1
//! the negative one, `2 + 2 * g` the reference and `3 + 2 * g` the probe of the gap `g` (0 to 4 for 1, 2, 3, 4, 6).
//!
//! rec gte_nclip: mac0_low, mac0_high, flag_bits (0x980-0x9AF; the raw MAC0 as its low and high 16 bits, and the FLAG register as bits 12 to 26 in bits 0 to 14 and bit 31 in bit 15; the expected silicon values are MAC0 `0x64` for the positive winding and `0xFFFFFF9C` for the negative one)
//!
//! The emulator models the hazard of the old console readings: the positive winding reads 0, or `100 - 10 * SY0` of
//! the stale `SXY0`, not 100, so its values move with the prior state too. INFO only; no pass or fail.

use crate::console_tests::record;
use crate::{push_timing_record, TimingRecord, TIMING_RECORD_COUNT};
use psx_gte::ops as gte_ops;
use psx_gte::regs::pack_xy;
use psx_gte::{read_control, read_data, write_control, write_data};

type Records = [TimingRecord; TIMING_RECORD_COUNT];

/// First record of the step: four variants of twelve, `0x980` to `0x9AF`.
const RECORD: u16 = 0x980;
/// Values a variant measures.
const PER_VARIANT: u16 = 12;
/// The pattern the filled variant writes: register `n` gets this xor `n * 0x0101_0101`.
const PATTERN: u32 = 0x5AC3_96E1;

macro_rules! nops {
    ($n:literal) => {
        #[cfg(target_arch = "mips")]
        // SAFETY: nops only.
        unsafe {
            core::arch::asm!(concat!(".rept ", $n, "\nnop\n.endr"), options(nostack, nomem, preserves_flags));
        }
    };
}

/// Write one register of each kind with its share of the pattern, for every register number in the list.
macro_rules! fill_registers {
    ($salt:ident, $mul:ident; $($n:literal),*) => {
        $(
            write_data!($n, $salt ^ ($n as u32).wrapping_mul($mul));
            write_control!($n, $salt ^ (($n as u32) + 32).wrapping_mul($mul));
        )*
    };
}

/// Every GTE data and control register but the read-only ones (data 31, LZCR) and the FLAG (control 31), which the
/// seed clears: register `n` gets `SALT ^ n * MUL` (the filled variant), or zero with both 0 (the zeroed variant).
fn fill<const SALT: u32, const MUL: u32>() {
    fill_registers!(
        SALT, MUL; 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27,
        28, 29, 30
    );
    write_control!(31, 0);
}

/// How a variant prepares the registers: 0 not at all, 1 the pattern, 2 zero.
macro_rules! prepare {
    ($mode:literal) => {
        if $mode == 1 {
            fill::<PATTERN, 0x0101_0101>();
        } else if $mode == 2 {
            fill::<0, 0>();
        }
    };
}

/// The positive-winding seed, then the command with the variant's gap between them.
fn seed_positive<const EXTRA: bool>() {
    write_control!(31, 0);
    write_data!(12, pack_xy(0, 0));
    write_data!(13, pack_xy(10, 0));
    write_data!(14, pack_xy(0, 10));
    if EXTRA {
        nops!(1);
    }
}

fn command() {
    // SAFETY: the GTE is enabled for the run.
    unsafe { gte_ops::screen_winding() };
}

/// Read MAC0 and FLAG as one record's fields.
fn read_fields(id: u16) -> TimingRecord {
    let mac0 = read_data!(24);
    let flag = read_control!(31);
    record(id, mac0 & 0xFFFF, mac0 >> 16, ((flag >> 12) & 0x7FFF) | ((flag >> 31) << 15))
}

macro_rules! gap_group {
    ($records:expr, $next:expr, $base:expr, $extra:literal, $mode:literal, $gap:literal, $index:literal) => {{
        prepare!($mode);
        // The settled reference: positive winding, 64 nops.
        seed_positive::<$extra>();
        command();
        nops!(64);
        let reference = read_fields($base + 2 + 2 * $index);
        // Poison the stale slot: the reversed winding.
        write_data!(13, pack_xy(0, 10));
        write_data!(14, pack_xy(10, 0));
        command();
        nops!(64);
        // The probe: the positive winding again, read after the gap.
        seed_positive::<$extra>();
        command();
        nops!($gap);
        let probe = read_fields($base + 3 + 2 * $index);
        push_timing_record($records, $next, reference);
        push_timing_record($records, $next, probe);
    }};
}

macro_rules! variant {
    ($records:expr, $next:expr, $v:literal, $extra:literal, $mode:literal) => {{
        let base = RECORD + PER_VARIANT * $v;
        prepare!($mode);
        crate::bounds::record_start(base);
        seed_positive::<$extra>();
        command();
        nops!(64);
        let positive = read_fields(base);
        write_control!(31, 0);
        write_data!(12, pack_xy(0, 0));
        write_data!(13, pack_xy(0, 10));
        write_data!(14, pack_xy(10, 0));
        if $extra {
            nops!(1);
        }
        command();
        nops!(64);
        let negative = read_fields(base + 1);
        push_timing_record($records, $next, positive);
        push_timing_record($records, $next, negative);
        crate::bounds::record_start(base + 2);
        gap_group!($records, $next, base, $extra, $mode, 1, 0);
        gap_group!($records, $next, base, $extra, $mode, 2, 1);
        gap_group!($records, $next, base, $extra, $mode, 3, 2);
        gap_group!($records, $next, base, $extra, $mode, 4, 3);
        gap_group!($records, $next, base, $extra, $mode, 6, 4);
    }};
}

/// The forty-eight records.
pub(crate) fn run(records: &mut Records, next: &mut usize) {
    variant!(records, next, 0, false, 0);
    variant!(records, next, 1, false, 1);
    variant!(records, next, 2, true, 0);
    variant!(records, next, 3, false, 2);
    // Leave the registers as the cases leave them, not full of the pattern.
    write_control!(31, 0);
}
