//! `scan-expansion2`: does a linked EXE reach the Expansion 2 window beyond the bytes the entry state configures?
//!
//! `psx_rt::entry` gives Expansion 2 a 128 byte window (`0x1F802000` to `0x1F80207F`). A store to the debug
//! console at `0x1F802080` raises a data bus error on a retail console (seen 2026-11-14 on an SCPH-9002), and the
//! emulator does not model that, so no emulator run can show it. A program that runs on a console must make no
//! access there. This reads every instruction word of the image, tracks the constants built by `lui`, `ori`,
//! `addiu` and `addu`, and reports every load or store whose address lands in `0x1F802080..=0x1F803FFF` (through
//! KUSEG, KSEG0 or KSEG1), and every data word that holds such an address.
//!
//! It is conservative, not exact: it follows registers along the instruction stream without following branches,
//! and drops the caller-saved registers at a call. A data word that happens to look like an address is reported
//! as well. A report is a reason to look; an empty report means no access of those shapes.

use std::io::Write;

use crate::machine_code::exe_words;
use crate::util::{Error, Result};

/// First byte past the window the entry state configures (`0x0007_0777`: 128 bytes).
pub const WINDOW_END: u32 = 0x1F80_2080;
/// Last byte of the 8 KiB Expansion 2 region.
pub const REGION_END: u32 = 0x1F80_3FFF;

/// The constants of `psx_hw::memory::expansion2` and the telemetry port, with what each is.
pub const NAMED: [(&str, u32); 6] = [
    ("expansion2::POST", 0x1F80_2041),
    ("expansion2::DEBUG_CONSOLE", 0x1F80_2080),
    ("expansion2::telemetry::EVENT", 0x1F80_2F00),
    ("expansion2::telemetry::VALUE", 0x1F80_2F04),
    ("expansion2::telemetry::CYCLES", 0x1F80_2F08),
    ("expansion2::telemetry::LOG", 0x1F80_2F0C),
];

/// One reference found.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Hit {
    /// Address of the instruction or data word in the image (`load_address + offset`).
    pub pc: u32,
    /// The physical address reached.
    pub address: u32,
    /// `"store"`, `"load"` or `"data word"`.
    pub kind: &'static str,
}

fn in_window(physical: u32) -> bool {
    (WINDOW_END..=REGION_END).contains(&physical)
}

fn physical(address: u32) -> Option<u32> {
    // KUSEG, KSEG0 and KSEG1 all map the low 512 MiB.
    match address >> 29 {
        0 | 4 | 5 => Some(address & 0x1FFF_FFFF),
        _ => None,
    }
}

fn sext(imm: u32) -> u32 {
    imm as u16 as i16 as i32 as u32
}

/// Scan the words of an EXE image body (the part after the 0x800 byte header) loaded at `load`.
#[must_use]
pub fn scan_words(words: &[u32], load: u32) -> Vec<Hit> {
    let mut hits = Vec::new();
    let mut known: [Option<u32>; 32] = [None; 32];
    known[0] = Some(0);
    for (index, &word) in words.iter().enumerate() {
        let pc = load.wrapping_add(4 * index as u32);
        if let Some(p) = physical(word) {
            if in_window(p) {
                hits.push(Hit { pc, address: p, kind: "data word" });
            }
        }
        let op = word >> 26;
        let rs = ((word >> 21) & 31) as usize;
        let rt = ((word >> 16) & 31) as usize;
        let rd = ((word >> 11) & 31) as usize;
        let imm = word & 0xFFFF;
        let write = |reg: usize, value: Option<u32>, known: &mut [Option<u32>; 32]| {
            if reg != 0 {
                known[reg] = value;
            }
        };
        match op {
            0x0F => write(rt, Some(imm << 16), &mut known),
            0x0D => {
                let v = known[rs].map(|b| b | imm);
                write(rt, v, &mut known);
            }
            0x08 | 0x09 => {
                let v = known[rs].map(|b| b.wrapping_add(sext(imm)));
                write(rt, v, &mut known);
            }
            0x0C | 0x0E | 0x0A | 0x0B => write(rt, None, &mut known),
            // loads: check the address, the destination is unknown
            0x20..=0x26 | 0x32 => {
                if op != 0x32 {
                    if let Some(base) = known[rs] {
                        let a = base.wrapping_add(sext(imm));
                        if let Some(p) = physical(a) {
                            if in_window(p) {
                                hits.push(Hit { pc, address: p, kind: "load" });
                            }
                        }
                    }
                    write(rt, None, &mut known);
                } else if let Some(base) = known[rs] {
                    let a = base.wrapping_add(sext(imm));
                    if let Some(p) = physical(a) {
                        if in_window(p) {
                            hits.push(Hit { pc, address: p, kind: "load" });
                        }
                    }
                }
            }
            // stores (and swc2)
            0x28..=0x2E | 0x3A => {
                if let Some(base) = known[rs] {
                    let a = base.wrapping_add(sext(imm));
                    if let Some(p) = physical(a) {
                        if in_window(p) {
                            hits.push(Hit { pc, address: p, kind: "store" });
                        }
                    }
                }
            }
            0x00 => {
                let funct = word & 63;
                match funct {
                    // addu / add / or with a zero register: a copy or a sum of known values
                    0x21 | 0x20 => {
                        let v = match (known[rs], known[rt]) {
                            (Some(a), Some(b)) => Some(a.wrapping_add(b)),
                            _ => None,
                        };
                        write(rd, v, &mut known);
                    }
                    0x25 => {
                        let v = match (known[rs], known[rt]) {
                            (Some(a), Some(b)) => Some(a | b),
                            _ => None,
                        };
                        write(rd, v, &mut known);
                    }
                    // jr and the others that write no register
                    0x08 | 0x18..=0x1B | 0x11 | 0x13 | 0x0C | 0x0D => {}
                    // jalr: a call
                    0x09 => {
                        for r in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 24, 25, 31] {
                            known[r] = None;
                        }
                    }
                    _ => write(rd, None, &mut known),
                }
            }
            // jal: a call
            0x03 => {
                for r in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 24, 25, 31] {
                    known[r] = None;
                }
            }
            // COP moves into a register: mfc0, mfc2, cfc2
            0x10 | 0x12 if rs == 0 || rs == 2 => write(rt, None, &mut known),
            _ => {}
        }
    }
    hits
}

/// The report: one line per hit, then one line for each named constant.
///
/// # Errors
/// The file cannot be read or is not a PSX-EXE, or an argument is not recognized.
pub fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> Result<i32> {
    let mut exe = None;
    for arg in args {
        match arg.as_str() {
            flag if flag.starts_with("--") => bail!("unrecognized arguments: {flag}"),
            path if exe.is_none() => exe = Some(path.to_string()),
            extra => bail!("unrecognized arguments: {extra}"),
        }
    }
    let exe = exe.ok_or("the following arguments are required: exe")?;
    let bytes = std::fs::read(&exe).map_err(|e| Error(format!("{exe}: {e}")))?;
    if bytes.get(..8) != Some(b"PS-X EXE") {
        bail!("{exe}: not a PSX-EXE");
    }
    let load = u32::from_le_bytes([bytes[0x18], bytes[0x19], bytes[0x1A], bytes[0x1B]]);
    let hits = scan_words(&exe_words(&bytes), load);
    for hit in &hits {
        writeln!(
            out,
            "expansion2: {} of {:#010X} at {:#010X}",
            hit.kind, hit.address, hit.pc
        )?;
    }
    for (name, address) in NAMED {
        let referenced = hits.iter().any(|h| h.address == address);
        let note = if address < WINDOW_END {
            "inside the 128 byte window, not scanned"
        } else if referenced {
            "REFERENCED"
        } else {
            "not referenced"
        };
        writeln!(out, "expansion2: {name} {address:#010X}: {note}")?;
    }
    if hits.is_empty() {
        writeln!(out, "expansion2: no access beyond the first 128 bytes ({exe})")?;
        Ok(0)
    } else {
        writeln!(
            err,
            "FAIL: {exe} reaches Expansion 2 beyond its first 128 bytes ({} reference(s)); a retail console raises a data bus error there",
            hits.len()
        )?;
        Ok(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lui(rt: u32, imm: u32) -> u32 {
        0x3C00_0000 | rt << 16 | imm
    }
    fn ori(rt: u32, rs: u32, imm: u32) -> u32 {
        0x3400_0000 | rs << 21 | rt << 16 | imm
    }
    fn sb(rt: u32, rs: u32, imm: u32) -> u32 {
        0xA000_0000 | rs << 21 | rt << 16 | imm
    }

    #[test]
    fn the_debug_console_store_loop_is_found() {
        // lui at,0xbf80; ori v0,at,0x2080; sb a0,0(v0)
        let words = [lui(1, 0xBF80), ori(2, 1, 0x2080), sb(4, 2, 0)];
        let hits = scan_words(&words, 0x8001_0000);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].address, 0x1F80_2080);
        assert_eq!(hits[0].pc, 0x8001_0008);
    }

    #[test]
    fn a_store_through_a_base_and_offset_is_found() {
        let words = [lui(2, 0x1F80), sb(4, 2, 0x2F00)];
        assert_eq!(scan_words(&words, 0).len(), 1);
    }

    #[test]
    fn a_store_inside_the_window_and_other_registers_are_not() {
        let words = [lui(2, 0xBF80), sb(4, 2, 0x2041), sb(4, 2, 0x1810), sb(4, 2, 0x207F)];
        assert!(scan_words(&words, 0).is_empty());
    }

    #[test]
    fn a_data_word_holding_the_address_is_found() {
        assert_eq!(scan_words(&[0xBF80_2080], 0)[0].kind, "data word");
        assert!(scan_words(&[0xBF80_1810], 0).is_empty());
    }

    #[test]
    fn a_constant_dropped_by_a_write_is_not_used() {
        // lui v0,0xbf80; lw v0,0(a0); sb a1,0x2080(v0): v0 no longer the constant
        let words = [lui(2, 0xBF80), 0x8C82_0000, sb(5, 2, 0x2080)];
        assert!(scan_words(&words, 0).is_empty());
    }
}
