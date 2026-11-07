// SPDX-License-Identifier: GPL-2.0-or-later
//! The debug-TTY mirror of a run, and the digests the capture header carries.

use super::*;

pub(crate) struct Hex2 {
    bytes: [u8; 2],
}

impl Hex2 {
    pub(crate) fn as_str(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.bytes) }
    }
}

pub(crate) fn hex2(value: u8) -> Hex2 {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    Hex2 {
        bytes: [HEX[(value >> 4) as usize], HEX[(value & 0xF) as usize]],
    }
}

pub(crate) struct Hex4 {
    bytes: [u8; 4],
}

impl Hex4 {
    pub(crate) fn as_str(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.bytes) }
    }
}

pub(crate) fn hex4(value: u16) -> Hex4 {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    Hex4 {
        bytes: [
            HEX[(value >> 12) as usize & 0xF],
            HEX[(value >> 8) as usize & 0xF],
            HEX[(value >> 4) as usize & 0xF],
            HEX[value as usize & 0xF],
        ],
    }
}

/// A poll is clean when something answered with a valid 0x5A magic and a
/// classified mode (not the `Unknown` desync bucket).
pub(crate) fn probe_column_ok(raw: psx_pad::RawPoll) -> bool {
    raw.mode.is_connected() && !matches!(raw.mode, psx_pad::PadMode::Unknown) && raw.id_high == 0x5A
}

/// Digest over the conformance results. `gte_only` restricts it to the GTE
/// group, which the header carries separately because the GTE cases are where
/// most emulator-versus-silicon differences have been.
pub(crate) fn conformance_hash(results: &[TestResult; TEST_COUNT], gte_only: bool) -> u32 {
    let mut hash = 0x4857_5445;
    hash = mix32(hash, gte_only as u32);
    for (index, spec) in TESTS.iter().enumerate() {
        if gte_only && spec.group != "GTE" {
            continue;
        }
        let result = results[index];
        hash = mix32(hash, index as u32);
        hash = mix_str(hash, spec.group);
        hash = mix_str(hash, spec.name);
        hash = mix32(hash, result.status.code());
        hash = mix32(hash, result.expected);
        hash = mix32(hash, result.observed);
    }
    hash
}

/// `[pass, fail, warn, info]` over the results.
pub(crate) fn tally(results: &[TestResult; TEST_COUNT]) -> [u16; 4] {
    let mut counts = [0u16; 4];
    for result in results {
        match result.status {
            Status::Pass => counts[0] += 1,
            Status::Fail => counts[1] += 1,
            Status::Warn => counts[2] += 1,
            Status::Info => counts[3] += 1,
            Status::Pending => {}
        }
    }
    counts
}

fn test_case_hash(index: usize, spec: TestSpec, result: TestResult) -> u32 {
    let mut hash = 0x4341_5345;
    hash = mix32(hash, index as u32);
    hash = mix_str(hash, spec.group);
    hash = mix_str(hash, spec.name);
    hash = mix32(hash, result.status.code());
    hash = mix32(hash, result.expected);
    hash = mix32(hash, result.observed);
    hash
}

/// One TTY line for the case at `index`.
pub(crate) fn print_case_report(index: usize, result: TestResult) {
    let spec = TESTS[index];
    tty::print("hardware-tests: case ");
    tty_print_dec_u16(index as u16);
    tty::print(" ");
    tty::print(spec.group);
    tty::print(" status=");
    tty::print(result.status.label());
    tty::print(" exp=0x");
    tty::print_hex_u32(result.expected);
    tty::print(" got=0x");
    tty::print_hex_u32(result.observed);
    tty::print(" hash=0x");
    tty::print_hex_u32(test_case_hash(index, spec, result));
    tty::print(" name=");
    tty::println(spec.name);
    if matches!(result.status, Status::Fail | Status::Warn) {
        tty::print("hardware-tests: ");
        tty::print(result.status.label());
        tty::print(" ");
        tty::print(spec.group);
        tty::print(" ");
        tty::print(spec.name);
        tty::print(" note=");
        tty::println(result.note);
        print_case_diagnostics(index, result);
    }
}

/// The end-of-run summary line.
pub(crate) fn print_conformance_report(results: &[TestResult; TEST_COUNT]) {
    let [pass, fail, warn, info] = tally(results);
    tty::print("hardware-tests: ");
    tty::println(SUITE_VERSION);
    tty::print("hardware-tests: conformance pass=");
    tty_print_dec_u16(pass);
    tty::print(" fail=");
    tty_print_dec_u16(fail);
    tty::print(" warn=");
    tty_print_dec_u16(warn);
    tty::print(" info=");
    tty_print_dec_u16(info);
    tty::print("\n");
}

fn diagnostic_lines_for_case(index: usize) -> &'static [&'static str] {
    match index {
        1 => &["wrapping add", "arithmetic shift", "wrapping multiply"],
        2 => &[
            "ADDU", "SUBU", "AND", "OR", "XOR", "NOR", "SLT", "SLTU", "SLL", "SRL", "SRA", "SLLV",
            "SRLV", "SRAV",
        ],
        3 => &["ADDIU", "ANDI", "ORI", "XORI", "SLTI", "SLTIU", "LUI"],
        4 => &["MULT", "MULTU", "DIV", "DIVU", "MTHI MTLO"],
        5 => &[
            "BEQ delay",
            "BNE delay",
            "BNE fallthrough",
            "BEQ always delay",
            "BLEZ delay",
            "BGTZ delay",
            "BGTZ fallthrough",
            "BLTZ delay",
            "BGEZ delay",
        ],
        6 => &[
            "LW result",
            "SW byte order",
            "LH sign extend",
            "LHU zero extend",
            "SH byte order",
            "LB sign extend",
            "LBU zero extend",
            "SB byte store",
            "load delay slot",
        ],
        10 => &["GPUSTAT24 set", "I_STAT set", "GPUSTAT24 clr", "I_STAT clr"],
        11 => &[
            "OT terminator",
            "OT link 1",
            "OT link 2",
            "OT link 3",
            "OT link 4",
            "OT link 5",
            "OT link 6",
            "OT link 7",
        ],
        12 => &["MADR", "BCR", "CHCR"],
        16 => &[
            "horizontal res",
            "vertical res",
            "DMA direction",
            "ready cmd",
            "ready DMA",
        ],
        17 => &["GP0 IRQ raised", "GP1 IRQ ack clears"],
        18 => &[
            "TriFlat layout",
            "TriFlat words",
            "LineMono words",
            "RectFlat words",
        ],
        19 => &["data register", "control register"],
        21 => &[
            "RTPS", "RTPT", "NCLIP", "OP", "AVSZ3", "AVSZ4", "SQR", "NCDS", "NCCS", "NCS", "NCDT",
            "NCT", "NCCT", "DPCS", "DPCT", "INTPL", "DCPL", "CC", "CDP", "GPF", "GPL", "MVMVA",
        ],
        22 => &[
            "positive MAC0",
            "negative MAC0",
            "positive FLAG",
            "negative FLAG",
        ],
        24 => &[
            "voice volume L",
            "voice volume R",
            "ADSR low",
            "ADSR high",
            "repeat addr",
            "pitch",
        ],
        25 => &["main volume L", "main volume R"],
        27 => &["SIO mode", "SIO baud", "SIO ctrl"],
        30 => &["target sticky", "counter reset"],
        32 => &["target before read", "target cleared by read"],
        33 => &["sync stop holds", "sync free-runs"],
        34 => &["system ticks", "div8 ticks", "ratio min", "ratio max"],
        35 => &["wrap sticky", "counter wrapped"],
        36 => &["target sticky", "IRQ active low"],
        37 => &["wrap sticky", "IRQ active low"],
        39 => &["system ticks", "dot ticks", "ratio min", "ratio max"],
        41 => &[
            "scratch word",
            "scratch half",
            "scratch byte",
            "scratch second word",
        ],
        43 => &["index 0", "index 1", "index 2", "index 3"],
        45 => &[
            "LWL/LWR result",
            "load pre byte",
            "store pre byte",
            "SWL/SWR byte 0",
            "SWL/SWR byte 1",
            "SWL/SWR byte 2",
            "SWL/SWR byte 3",
            "store post byte",
        ],
        46 => &["DMA off", "DMA fifo", "DMA CPU->GP0", "DMA GPUREAD->CPU"],
        47 => &[
            "texture window",
            "draw area TL",
            "draw area BR",
            "draw offset",
        ],
        48 => &["timer0 target", "timer1 target", "timer2 target"],
        _ => &[],
    }
}

pub(crate) fn print_case_diagnostics(index: usize, result: TestResult) {
    let details = diagnostic_lines_for_case(index);
    if details.is_empty() {
        return;
    }

    let mut mismatches = 0u16;
    for (bit, label) in details.iter().enumerate() {
        let expected = (result.expected >> bit) & 1;
        let observed = (result.observed >> bit) & 1;
        if expected == observed {
            continue;
        }
        mismatches = mismatches.wrapping_add(1);
        tty::print("hardware-tests: detail case=");
        tty_print_dec_u16(index as u16);
        tty::print(" bit=");
        tty_print_dec_u16(bit as u16);
        tty::print(" exp=");
        tty_print_dec_u16(expected as u16);
        tty::print(" got=");
        tty_print_dec_u16(observed as u16);
        tty::print(" ");
        tty::println(label);
    }
    if mismatches == 0 {
        tty::print("hardware-tests: detail case=");
        tty_print_dec_u16(index as u16);
        tty::println(" no sub-bit mismatch; inspect raw expected/observed");
    }
}

pub(crate) fn tty_print_dec_u8(value: u8) {
    tty_print_dec_u16(value as u16);
}

pub(crate) fn tty_print_dec_u16(value: u16) {
    let mut divisor = 10000u16;
    let mut started = false;
    while divisor > 0 {
        let digit = value / divisor % 10;
        if digit != 0 || started || divisor == 1 {
            tty_print_digit(digit as u8);
            started = true;
        }
        divisor /= 10;
    }
}

pub(crate) fn tty_print_digit(value: u8) {
    let byte = b'0' + value.min(9);
    let text = [byte];
    let text = unsafe { core::str::from_utf8_unchecked(&text) };
    tty::print(text);
}
