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
//! and drops the caller-saved registers at a call. A report is a reason to look; an empty report means no access
//! of those shapes.
//!
//! A data word that holds such an address is a different kind of finding, because it is only dangerous if some
//! code reads it as a pointer. Embedded asset bytes (a cooked texture, a sound bank) contain such words by
//! chance, and a scan that fails on them gets ignored. So a data word fails the scan only when code can reach it:
//! some instruction sequence forms the word's own address (`lui` + `addiu`/`ori`, or the address of a load
//! from it), or forms the value it holds. With `--map LINK.MAP` the test is by input section instead: the word
//! fails when its section is word-aligned (alignment 4 or more, so it can hold pointers) and some code forms an
//! address inside that section; a section of byte-aligned data is a blob of bytes, whatever it holds. Every
//! other data word is listed as a note and the scan passes. `--strict` fails on every data word, as the scan
//! did before.

use std::collections::BTreeSet;
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
    /// The word itself, for a data word (the instruction word otherwise).
    pub value: u32,
}

/// What the scan saw: every hit, and every physical address some code forms or touches.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Scan {
    /// Loads, stores and data words, in image order.
    pub hits: Vec<Hit>,
    /// The physical address of every constant the tracked registers held (`lui`/`ori`/`addiu`/`addu`) and of every
    /// load and store target with a known base: the addresses code can reach by following the constants.
    pub formed: BTreeSet<u32>,
}

/// One input section of the link map.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MapSection {
    /// Physical start address.
    pub start: u32,
    /// Size in bytes.
    pub size: u32,
    /// Alignment in bytes.
    pub align: u32,
    /// The section name, e.g. `.rodata.$anon.1234`.
    pub name: String,
}

/// The input sections of an lld-style map (`VMA LMA Size Align Out In Symbol` columns).
#[must_use]
pub fn parse_map(text: &str) -> Vec<MapSection> {
    let mut sections = Vec::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let (Some(vma), Some(_lma), Some(size), Some(align), Some(input)) =
            (fields.next(), fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let (Ok(vma), Ok(size), Ok(align)) = (
            u32::from_str_radix(vma, 16),
            u32::from_str_radix(size, 16),
            align.parse::<u32>(),
        ) else {
            continue;
        };
        let Some((_, name)) = input.split_once(":(") else { continue };
        let Some(start) = physical(vma) else { continue };
        sections.push(MapSection {
            start,
            size,
            align,
            name: name.trim_end_matches(')').to_string(),
        });
    }
    sections
}

/// Is a data word reachable by code, and if not, why not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Reach {
    /// Code forms the word's address, its value, or (with a map) an address in its pointer-holding section.
    Reachable,
    /// No code forms it: asset bytes or an unused constant.
    Unreached,
    /// Inside a section of byte-aligned data, which holds bytes and not pointers.
    ByteBlob,
}

/// Classify the data word `hit` against what the code forms, and the map if there is one.
#[must_use]
pub fn classify(hit: &Hit, scan: &Scan, map: Option<&[MapSection]>) -> Reach {
    let word_at = physical(hit.pc);
    if let (Some(map), Some(at)) = (map, word_at) {
        if let Some(section) = map
            .iter()
            .find(|s| s.size > 0 && at >= s.start && at - s.start < s.size)
        {
            if section.align < 4 {
                return Reach::ByteBlob;
            }
            let end = section.start.saturating_add(section.size);
            return if scan.formed.range(section.start..end).next().is_some() {
                Reach::Reachable
            } else {
                Reach::Unreached
            };
        }
    }
    let formed_address = word_at.is_some_and(|a| scan.formed.contains(&a));
    if formed_address || scan.formed.contains(&hit.address) {
        Reach::Reachable
    } else {
        Reach::Unreached
    }
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

/// Record `value` as the content of `reg` (never `$zero`), and the address it names as one the code forms.
fn write(
    reg: usize,
    value: Option<u32>,
    known: &mut [Option<u32>; 32],
    formed: &mut BTreeSet<u32>,
) {
    if reg != 0 {
        known[reg] = value;
        if let Some(p) = value.and_then(physical) {
            formed.insert(p);
        }
    }
}

/// Scan the words of an EXE image body (the part after the 0x800 byte header) loaded at `load`; every data word
/// that looks like a window address is in the result.
#[must_use]
pub fn scan_words(words: &[u32], load: u32) -> Vec<Hit> {
    scan(words, load).hits
}

/// [`scan_words`] and the addresses the code forms.
#[must_use]
pub fn scan(words: &[u32], load: u32) -> Scan {
    let mut hits = Vec::new();
    let mut formed = BTreeSet::new();
    let mut known: [Option<u32>; 32] = [None; 32];
    known[0] = Some(0);
    for (index, &word) in words.iter().enumerate() {
        let pc = load.wrapping_add(4 * index as u32);
        if let Some(p) = physical(word) {
            if in_window(p) {
                hits.push(Hit { pc, address: p, kind: "data word", value: word });
            }
        }
        let op = word >> 26;
        let rs = ((word >> 21) & 31) as usize;
        let rt = ((word >> 16) & 31) as usize;
        let rd = ((word >> 11) & 31) as usize;
        let imm = word & 0xFFFF;
        match op {
            0x0F => write(rt, Some(imm << 16), &mut known, &mut formed),
            0x0D => {
                let v = known[rs].map(|b| b | imm);
                write(rt, v, &mut known, &mut formed);
            }
            0x08 | 0x09 => {
                let v = known[rs].map(|b| b.wrapping_add(sext(imm)));
                write(rt, v, &mut known, &mut formed);
            }
            0x0C | 0x0E | 0x0A | 0x0B => write(rt, None, &mut known, &mut formed),
            // loads: check the address, the destination is unknown
            0x20..=0x26 | 0x32 => {
                if op != 0x32 {
                    if let Some(base) = known[rs] {
                        let a = base.wrapping_add(sext(imm));
                        if let Some(p) = physical(a) {
                            formed.insert(p);
                            if in_window(p) {
                                hits.push(Hit { pc, address: p, kind: "load", value: word });
                            }
                        }
                    }
                    write(rt, None, &mut known, &mut formed);
                } else if let Some(base) = known[rs] {
                    let a = base.wrapping_add(sext(imm));
                    if let Some(p) = physical(a) {
                        formed.insert(p);
                        if in_window(p) {
                            hits.push(Hit { pc, address: p, kind: "load", value: word });
                        }
                    }
                }
            }
            // stores (and swc2)
            0x28..=0x2E | 0x3A => {
                if let Some(base) = known[rs] {
                    let a = base.wrapping_add(sext(imm));
                    if let Some(p) = physical(a) {
                        formed.insert(p);
                        if in_window(p) {
                            hits.push(Hit { pc, address: p, kind: "store", value: word });
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
                        write(rd, v, &mut known, &mut formed);
                    }
                    0x25 => {
                        let v = match (known[rs], known[rt]) {
                            (Some(a), Some(b)) => Some(a | b),
                            _ => None,
                        };
                        write(rd, v, &mut known, &mut formed);
                    }
                    // jr and the others that write no register
                    0x08 | 0x18..=0x1B | 0x11 | 0x13 | 0x0C | 0x0D => {}
                    // jalr: a call
                    0x09 => {
                        for r in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 24, 25, 31] {
                            known[r] = None;
                        }
                    }
                    _ => write(rd, None, &mut known, &mut formed),
                }
            }
            // jal: a call
            0x03 => {
                for r in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 24, 25, 31] {
                    known[r] = None;
                }
            }
            // COP moves into a register: mfc0, mfc2, cfc2
            0x10 | 0x12 if rs == 0 || rs == 2 => write(rt, None, &mut known, &mut formed),
            _ => {}
        }
    }
    Scan { hits, formed }
}

/// The report: one line per finding, then one line for each named constant.
///
/// `--strict` fails on every data word; `--map FILE` classifies data words by the input section they lie in.
///
/// # Errors
/// The file cannot be read or is not a PSX-EXE, or an argument is not recognized.
pub fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> Result<i32> {
    let mut exe = None;
    let mut strict = false;
    let mut map_path = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--strict" => strict = true,
            "--map" => {
                map_path = Some(rest.next().ok_or("--map needs a file")?.to_string());
            }
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
    let map = match &map_path {
        Some(path) => {
            let text = std::fs::read_to_string(path).map_err(|e| Error(format!("{path}: {e}")))?;
            let sections = parse_map(&text);
            if sections.is_empty() {
                bail!("{path}: no input sections found (not a link map with VMA LMA Size Align columns)");
            }
            Some(sections)
        }
        None => None,
    };
    let load = u32::from_le_bytes([bytes[0x18], bytes[0x19], bytes[0x1A], bytes[0x1B]]);
    let found = scan(&exe_words(&bytes), load);
    let mut failing: Vec<&Hit> = Vec::new();
    let mut unreached = 0usize;
    for hit in &found.hits {
        let reach = if hit.kind == "data word" && !strict {
            classify(hit, &found, map.as_deref())
        } else {
            Reach::Reachable
        };
        match reach {
            Reach::Reachable => {
                writeln!(
                    out,
                    "expansion2: {} of {:#010X} at {:#010X}",
                    hit.kind, hit.address, hit.pc
                )?;
                failing.push(hit);
            }
            Reach::Unreached | Reach::ByteBlob => {
                let why = if reach == Reach::ByteBlob {
                    "inside a byte-aligned blob"
                } else {
                    "no code forms its address or value"
                };
                writeln!(
                    out,
                    "expansion2: note: data word of {:#010X} at {:#010X} ignored: {why} (asset bytes)",
                    hit.address, hit.pc
                )?;
                unreached += 1;
            }
        }
    }
    for (name, address) in NAMED {
        let referenced = failing.iter().any(|h| h.address == address);
        let note = if address < WINDOW_END {
            "inside the 128 byte window, not scanned"
        } else if referenced {
            "REFERENCED"
        } else {
            "not referenced"
        };
        writeln!(out, "expansion2: {name} {address:#010X}: {note}")?;
    }
    if failing.is_empty() {
        writeln!(out, "expansion2: no access beyond the first 128 bytes ({exe})")?;
        if unreached > 0 {
            let how = if map.is_some() {
                "classified by link-map section"
            } else {
                "no map given, so a table read by index is not proven; pass --map LINK.MAP to classify by section"
            };
            writeln!(
                out,
                "expansion2: note: {unreached} data word(s) look like Expansion 2 addresses but are not pointers code can reach ({how}); --strict fails on them"
            )?;
        }
        Ok(0)
    } else {
        writeln!(
            err,
            "FAIL: {exe} reaches Expansion 2 beyond its first 128 bytes ({} reference(s)); a retail console raises a data bus error there",
            failing.len()
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

    fn addiu(rt: u32, rs: u32, imm: u32) -> u32 {
        0x2400_0000 | rs << 21 | rt << 16 | imm
    }
    fn lw(rt: u32, rs: u32, imm: u32) -> u32 {
        0x8C00_0000 | rs << 21 | rt << 16 | imm
    }

    /// A body where the data words sit after `code`: `code` then the words, loaded at 0x8001_0000.
    fn image(code: &[u32], data: &[u32]) -> (Vec<u32>, u32) {
        let mut words = code.to_vec();
        words.extend_from_slice(data);
        (words, 0x8001_0000)
    }

    #[test]
    fn an_unreferenced_data_word_is_a_note_not_a_failure() {
        let (words, load) = image(&[0x0000_0000], &[0x1F80_2F73, 0x1234_5678]);
        let found = scan(&words, load);
        assert_eq!(found.hits.len(), 1);
        assert_eq!(classify(&found.hits[0], &found, None), Reach::Unreached);
    }

    #[test]
    fn a_data_word_whose_address_code_forms_is_reachable() {
        // lui a0,0x8001; lw v0,4(a0): loads the word at 0x8001_0004, which is the data word (index 1 of 3 words)
        let (words, load) = image(&[lui(4, 0x8001), lw(2, 4, 8)], &[0x1F80_2F73]);
        let found = scan(&words, load);
        let word = found.hits.iter().find(|h| h.kind == "data word").unwrap();
        assert_eq!(word.pc, 0x8001_0008);
        assert_eq!(classify(word, &found, None), Reach::Reachable);
        // The same through lui + addiu with no load.
        let (words, load) = image(&[lui(4, 0x8001), addiu(4, 4, 8)], &[0x1F80_2F73]);
        let found = scan(&words, load);
        let word = found.hits.iter().find(|h| h.kind == "data word").unwrap();
        assert_eq!(classify(word, &found, None), Reach::Reachable);
    }

    #[test]
    fn a_data_word_whose_value_code_forms_is_reachable() {
        // lui v0,0x1f80; ori v0,v0,0x2f73 forms the value without storing through it
        let (words, load) = image(&[lui(2, 0x1F80), ori(2, 2, 0x2F73)], &[0x1F80_2F73]);
        let found = scan(&words, load);
        let word = found.hits.iter().find(|h| h.kind == "data word").unwrap();
        assert_eq!(classify(word, &found, None), Reach::Reachable);
    }

    #[test]
    fn a_map_classifies_by_section_alignment_and_use() {
        let (words, load) = image(&[lui(4, 0x8001), addiu(4, 4, 8)], &[0x1F80_2F73, 0, 0x1F80_3D0F]);
        let found = scan(&words, load);
        let text = "\
     VMA      LMA     Size Align Out     In      Symbol
80010000 80010000        8     4 .text
80010000 80010000        8     4         /x/a.o:(.text.main)
80010008 80010008        4     4 .rodata
80010008 80010008        4     4         /x/a.o:(.rodata.table)
80010010 80010010        4     1         /x/a.o:(.rodata.$anon.1)
";
        let map = parse_map(text);
        assert_eq!(map.len(), 3);
        assert_eq!(map[1], MapSection { start: 0x0001_0008, size: 4, align: 4, name: ".rodata.table".into() });
        let table = found.hits.iter().find(|h| h.pc == 0x8001_0008).unwrap();
        let blob = found.hits.iter().find(|h| h.pc == 0x8001_0010).unwrap();
        // The table section holds pointers and code forms an address inside it.
        assert_eq!(classify(table, &found, Some(&map)), Reach::Reachable);
        // The byte-aligned blob holds bytes, whatever they look like.
        assert_eq!(classify(blob, &found, Some(&map)), Reach::ByteBlob);
        // A table nobody forms an address in is unreached.
        let (words, load) = image(&[0], &[0x1F80_2F73]);
        let found = scan(&words, load);
        let map = parse_map("80010004 80010004        4     4         /x/a.o:(.rodata.table)\n");
        assert_eq!(classify(&found.hits[0], &found, Some(&map)), Reach::Unreached);
    }

    #[test]
    fn a_store_still_fails_with_a_data_word_ignored() {
        let (words, load) = image(&[lui(2, 0xBF80), ori(2, 2, 0x2080), sb(4, 2, 0)], &[0x1F80_2F73]);
        let found = scan(&words, load);
        assert!(found.hits.iter().any(|h| h.kind == "store"));
        assert_eq!(classify(found.hits.last().unwrap(), &found, None), Reach::Unreached);
    }
}
