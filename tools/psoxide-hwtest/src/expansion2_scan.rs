//! `scan-expansion2`: does a linked EXE reach the Expansion 2 window beyond the bytes the entry state configures?
//!
//! `psx_rt::entry` gives Expansion 2 a 128 byte window (`0x1F802000` to `0x1F80207F`). A store to the debug
//! console at `0x1F802080` raises a data bus error on a retail console (seen 2026-11-14 on an SCPH-9002), and the
//! emulator does not model that, so no emulator run can show it. A program that runs on a console must make no
//! access there. This reads every instruction word of the image, tracks the constants built by `lui`, `ori`,
//! `addiu` and `addu`, and reports every load or store whose address lands in `0x1F802080..=0x1F803FFF` (through
//! KUSEG, KSEG0 or KSEG1), and every data word that holds such an address.
//!
//! It is conservative, not exact: it tracks registers over the control-flow graph of the image's own branches and
//! jumps and keeps a constant only where every path agrees on it (see [`scan`]). A report is a reason to look; an
//! empty report means no access of those shapes.
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

/// The registers a call may overwrite: everything but `$zero`, the callee-saved registers, `$k0`/`$k1`, `$gp`,
/// `$sp` and `$fp`.
const CALLER_SAVED: [usize; 18] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 24, 25, 31];

/// What each register is known to hold at a point of the program.
type Known = [Option<u32>; 32];

/// What nothing is known, except that `$zero` is zero.
fn nothing_known() -> Known {
    let mut known = [None; 32];
    known[0] = Some(0);
    known
}

/// What the instruction stream formed, collected only on the reporting pass.
struct Effects {
    on: bool,
    hits: Vec<Hit>,
    formed: BTreeSet<u32>,
}

impl Effects {
    fn form(&mut self, value: u32) {
        if self.on {
            if let Some(p) = physical(value) {
                self.formed.insert(p);
            }
        }
    }

    /// A load or store at `base + offset`, when the base is known.
    fn access(&mut self, base: Option<u32>, word: u32, pc: u32, kind: &'static str) {
        if let Some(address) = base.map(|b| b.wrapping_add(sext(word & 0xFFFF))) {
            self.form(address);
            if let Some(p) = physical(address).filter(|&p| self.on && in_window(p)) {
                self.hits.push(Hit { pc, address: p, kind, value: word });
            }
        }
    }
}

/// Record `value` as the content of `reg` (never `$zero`), and the address it names as one the code forms.
fn write(reg: usize, value: Option<u32>, known: &mut Known, fx: &mut Effects) {
    if reg != 0 {
        known[reg] = value;
        if let Some(value) = value {
            fx.form(value);
        }
    }
}

/// What executing `word` at `pc` does to `known`. A call's clobbering of the caller-saved registers is not here:
/// it happens after the call's delay slot (see [`Flow`]).
fn step(word: u32, pc: u32, known: &mut Known, fx: &mut Effects) {
    let op = word >> 26;
    let rs = ((word >> 21) & 31) as usize;
    let rt = ((word >> 16) & 31) as usize;
    let rd = ((word >> 11) & 31) as usize;
    let imm = word & 0xFFFF;
    match op {
        0x0F => write(rt, Some(imm << 16), known, fx),
        0x0D => {
            let v = known[rs].map(|b| b | imm);
            write(rt, v, known, fx);
        }
        0x08 | 0x09 => {
            let v = known[rs].map(|b| b.wrapping_add(sext(imm)));
            write(rt, v, known, fx);
        }
        0x0C | 0x0E | 0x0A | 0x0B => write(rt, None, known, fx),
        // loads: check the address, the destination is unknown
        0x20..=0x26 => {
            fx.access(known[rs], word, pc, "load");
            write(rt, None, known, fx);
        }
        // lwc2 reads memory and writes a coprocessor register
        0x32 => fx.access(known[rs], word, pc, "load"),
        // stores (and swc2)
        0x28..=0x2E | 0x3A => fx.access(known[rs], word, pc, "store"),
        0x00 => match word & 63 {
            // addu / add / or: a copy or a sum of known values
            0x21 | 0x20 => {
                let v = known[rs].zip(known[rt]).map(|(a, b)| a.wrapping_add(b));
                write(rd, v, known, fx);
            }
            0x25 => {
                let v = known[rs].zip(known[rt]).map(|(a, b)| a | b);
                write(rd, v, known, fx);
            }
            // jr, mult/div, mthi/mtlo, syscall, break: no general register written
            0x08 | 0x18..=0x1B | 0x11 | 0x13 | 0x0C | 0x0D => {}
            // jalr writes its link register (the callee's clobbering follows the delay slot)
            0x09 => write(rd, None, known, fx),
            _ => write(rd, None, known, fx),
        },
        // jal links
        0x03 => write(31, None, known, fx),
        // COP moves into a register: mfc0, mfc2, cfc2
        0x10 | 0x12 if rs == 0 || rs == 2 => write(rt, None, known, fx),
        _ => {}
    }
}

/// How control leaves an instruction (the transfer takes effect after its delay slot).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flow {
    /// Falls through to the next word.
    Next,
    /// A conditional branch to `target` (a word index), or on.
    Branch(usize),
    /// An unconditional jump to `target` (a word index, or `None` when it leaves the image).
    Jump(Option<usize>),
    /// `jr`: leaves to an address in a register, never falls through.
    Return,
    /// A call (`jal`, `jalr`, `bltzal`): the callee clobbers the caller-saved registers and control comes back.
    Call,
    /// `break`, `syscall`: nothing follows on this path.
    Stop,
}

fn flow(word: u32, index: usize, words: usize, load: u32) -> Flow {
    let op = word >> 26;
    let rs = (word >> 21) & 31;
    let rt = (word >> 16) & 31;
    let branch = |index: usize| {
        let target = index as i64 + 1 + i64::from(sext(word & 0xFFFF) as i32);
        usize::try_from(target).ok().filter(|&t| t < words)
    };
    match op {
        0x02 => {
            let address = (word & 0x03FF_FFFF) << 2 | (load.wrapping_add(4 * index as u32) & 0xF000_0000);
            let at = address.wrapping_sub(load) / 4;
            Flow::Jump(usize::try_from(at).ok().filter(|&t| address >= load && t < words))
        }
        0x03 => Flow::Call,
        0x04..=0x07 => branch(index).map_or(Flow::Next, Flow::Branch),
        0x01 if rt & 0x1E == 0x10 => Flow::Call,
        0x01 => branch(index).map_or(Flow::Next, Flow::Branch),
        0x10..=0x13 if rs == 8 => branch(index).map_or(Flow::Next, Flow::Branch),
        0x00 => match word & 63 {
            0x08 => Flow::Return,
            0x09 => Flow::Call,
            0x0C | 0x0D => Flow::Stop,
            _ => Flow::Next,
        },
        _ => Flow::Next,
    }
}

/// Scan the words of an EXE image body (the part after the 0x800 byte header) loaded at `load`; every data word
/// that looks like a window address is in the result.
#[must_use]
pub fn scan_words(words: &[u32], load: u32) -> Vec<Hit> {
    scan(words, load).hits
}

/// Join two register files at a merge point: a register keeps its constant only if every path agrees on it.
fn join(into: &mut Known, other: &Known) -> bool {
    let mut changed = false;
    for (mine, theirs) in into.iter_mut().zip(other) {
        if mine.is_some() && *mine != *theirs {
            *mine = None;
            changed = true;
        }
    }
    changed
}

/// Make `at` an entry: nothing known there, whatever else reaches it.
fn seed(entry: &mut [Option<Known>], at: usize, work: &mut Vec<usize>) {
    let changed = match entry[at].as_mut() {
        None => {
            entry[at] = Some(nothing_known());
            true
        }
        Some(state) => join(state, &nothing_known()),
    };
    if changed {
        work.push(at);
    }
}

/// Run the data-flow from the words in `work` until no entry state changes.
fn propagate(words: &[u32], load: u32, entry: &mut [Option<Known>], work: &mut Vec<usize>) {
    let mut effects = Effects { on: false, hits: Vec::new(), formed: BTreeSet::new() };
    let mut queued = vec![false; words.len()];
    for &at in work.iter() {
        queued[at] = true;
    }
    while let Some(index) = work.pop() {
        queued[index] = false;
        let Some(mut state) = entry[index] else { continue };
        let pc = load.wrapping_add(4 * index as u32);
        step(words[index], pc, &mut state, &mut effects);
        // The word before a transfer is the transfer; its delay slot is this word.
        let before = index.checked_sub(1).map(|i| flow(words[i], i, words.len(), load));
        let mut successors: Vec<(usize, Known)> = Vec::new();
        match before {
            Some(Flow::Branch(target)) => {
                successors.push((target, state));
                successors.push((index + 1, state));
            }
            Some(Flow::Jump(Some(target))) => successors.push((target, state)),
            Some(Flow::Call) => {
                let mut after = state;
                for r in CALLER_SAVED {
                    after[r] = None;
                }
                successors.push((index + 1, after));
            }
            Some(Flow::Jump(None) | Flow::Return) => {}
            _ => {
                if !matches!(flow(words[index], index, words.len(), load), Flow::Stop) {
                    successors.push((index + 1, state));
                }
            }
        }
        for (at, state) in successors {
            if at >= words.len() {
                continue;
            }
            let changed = match entry[at].as_mut() {
                None => {
                    entry[at] = Some(state);
                    true
                }
                Some(existing) => join(existing, &state),
            };
            if changed && !queued[at] {
                queued[at] = true;
                work.push(at);
            }
        }
    }
}

/// What is known on entry to each word: a forward data-flow over the control-flow graph the image's own branches
/// and jumps make, met by intersection. Every word ends with a state: none is left unscanned.
///
/// Entries, where nothing is known (a caller's registers mean nothing in the callee, and a callee-saved register
/// holds its caller's value, not a constant this function formed):
/// 1. the start of the image, the target of every `jal`, and every word whose address appears as a data word;
/// 2. every code address the code forms (`lui` + `addiu`/`ori` of a function's address, for `jalr`, a callback, an
///    interrupt handler), found by running the data-flow, reading the addresses it forms, and seeding them, until
///    nothing new is seeded;
/// 3. then the first word of every run of words still no path reaches (a function nothing in the image refers to,
///    or data), and the data-flow again, until every word is reached. A constant is never carried into such a run,
///    but one formed inside it is tracked, as the linear scan did.
fn entry_states(words: &[u32], load: u32) -> Vec<Option<Known>> {
    let mut entry: Vec<Option<Known>> = vec![None; words.len()];
    if words.is_empty() {
        return entry;
    }
    let mut work: Vec<usize> = Vec::new();
    let index_of = |address: u32| {
        (address >= load && address.is_multiple_of(4))
            .then(|| usize::try_from((address - load) / 4).ok())
            .flatten()
            .filter(|&t| t < words.len())
    };
    seed(&mut entry, 0, &mut work);
    for &word in words {
        let target = match word >> 26 {
            0x03 => Some((word & 0x03FF_FFFF) << 2 | (load & 0xF000_0000)),
            _ => physical(word).map(|_| word),
        };
        if let Some(at) = target.and_then(index_of) {
            seed(&mut entry, at, &mut work);
        }
    }
    let base = physical(load).unwrap_or(load);
    loop {
        propagate(words, load, &mut entry, &mut work);
        // Code addresses the code forms, read off the states reached so far.
        let mut fx = Effects { on: true, hits: Vec::new(), formed: BTreeSet::new() };
        for (index, &word) in words.iter().enumerate() {
            let pc = load.wrapping_add(4 * index as u32);
            let mut known = entry[index].unwrap_or_else(nothing_known);
            step(word, pc, &mut known, &mut fx);
        }
        let before = work.len();
        for &p in &fx.formed {
            if let Some(at) = p.checked_sub(base).filter(|d| d.is_multiple_of(4)).map(|d| (d / 4) as usize) {
                if at < words.len() && entry[at].is_none() {
                    seed(&mut entry, at, &mut work);
                }
            }
        }
        if work.len() > before {
            continue;
        }
        // Nothing more is formed: the first word of each run of unreached words is an entry.
        let mut previous_reached = true;
        for at in 0..words.len() {
            let reached = entry[at].is_some();
            if !reached && previous_reached {
                seed(&mut entry, at, &mut work);
            }
            previous_reached = reached;
        }
        if work.is_empty() {
            return entry;
        }
    }
}

/// [`scan_words`] and the addresses the code forms.
///
/// A register's constant is used only where it holds on every path that reaches the use: it dies at function entry
/// (a `jal` target), at a call for the caller-saved registers, wherever two paths meet with different values, and
/// wherever the register is written with anything but a known constant. A word nothing refers to starts with nothing
/// known, and one formed inside it is tracked (see `entry_states`).
#[must_use]
pub fn scan(words: &[u32], load: u32) -> Scan {
    let entry = entry_states(words, load);
    let mut fx = Effects { on: true, hits: Vec::new(), formed: BTreeSet::new() };
    for (index, &word) in words.iter().enumerate() {
        let pc = load.wrapping_add(4 * index as u32);
        if let Some(p) = physical(word).filter(|&p| in_window(p)) {
            fx.hits.push(Hit { pc, address: p, kind: "data word", value: word });
        }
        let mut known = entry[index].unwrap_or_else(nothing_known);
        step(word, pc, &mut known, &mut fx);
    }
    Scan { hits: fx.hits, formed: fx.formed }
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

    fn beq(rs: u32, rt: u32, offset: i32) -> u32 {
        0x1000_0000 | rs << 21 | rt << 16 | (offset as u32 & 0xFFFF)
    }
    fn bne(rs: u32, rt: u32, offset: i32) -> u32 {
        0x1400_0000 | rs << 21 | rt << 16 | (offset as u32 & 0xFFFF)
    }
    fn jal(index: u32) -> u32 {
        0x0C00_0000 | (((0x8001_0000 & 0x0FFF_FFFF) >> 2) + index)
    }
    const JR_RA: u32 = 0x03E0_0008;
    const NOP: u32 = 0;
    const OR_S0_A0: u32 = 0x0080_8025; // or s0,a0,zero

    #[test]
    fn a_constant_left_in_an_argument_register_does_not_reach_the_next_function() {
        // A function leaves a0 = 0x1F801824 (an I/O address it passed on) and returns. The next function
        // takes its argument in a0, keeps it in the callee-saved s0 and reads 7008 bytes past it.
        let words = [
            lui(4, 0x1F80),
            addiu(4, 4, 0x1824),
            JR_RA,
            NOP,
            // index 4: the callee, entered by the jal below
            OR_S0_A0,
            lw(2, 16, 7008),
            JR_RA,
            NOP,
            // index 8: a caller
            jal(4),
            NOP,
            JR_RA,
            NOP,
        ];
        assert!(scan_words(&words, 0x8001_0000).is_empty());
        // The same bytes with the callee reached by falling into it (no `jr` before it, no call to it) still has
        // the constant: that is one function, and the access is real.
        let mut one = words;
        one[2] = NOP;
        one[3] = NOP;
        one[8] = NOP;
        assert_eq!(scan_words(&one, 0x8001_0000).len(), 1);
    }

    fn jalr(rs: u32) -> u32 {
        rs << 21 | 0x0000_F809
    }

    #[test]
    fn a_word_no_path_reaches_is_still_scanned_with_its_own_constants() {
        // The constant formed before a `jr` is not carried into the unreached word after it ...
        let words = [lui(2, 0xBF80), JR_RA, NOP, sb(4, 2, 0x2080)];
        assert!(scan_words(&words, 0x8001_0000).is_empty());
        // ... but a function nothing refers to (dead, or reached some way the image does not show) is an
        // entry, and a constant it forms itself is tracked: the store is found.
        let dead = [JR_RA, NOP, lui(2, 0xBF80), sb(4, 2, 0x2080), JR_RA, NOP];
        assert_eq!(scan_words(&dead, 0x8001_0000).len(), 1);
    }

    #[test]
    fn a_function_reached_only_through_a_pointer_the_code_forms_is_scanned() {
        // main: t0 = address of the callback (lui+addiu), jalr t0; the callback (index 8) stores to the debug
        // console through a constant it forms.
        let words = [
            lui(8, 0x8001),
            addiu(8, 8, 0x0020),
            jalr(8),
            NOP,
            JR_RA,
            NOP,
            NOP,
            NOP,
            // index 8
            lui(2, 0xBF80),
            sb(4, 2, 0x2080),
            JR_RA,
            NOP,
        ];
        let hits = scan_words(&words, 0x8001_0000);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].pc, 0x8001_0024);
    }
    #[test]
    fn paths_that_disagree_on_a_register_lose_it_and_paths_that_agree_keep_it() {
        // lui v0,0xbf80; beq a0,zero,+2; nop; lui v0,0x1000; sb a1,0x2f00(v0)
        let disagree = [lui(2, 0xBF80), beq(4, 0, 2), NOP, lui(2, 0x1000), sb(5, 2, 0x2F00)];
        assert!(scan_words(&disagree, 0x8001_0000).is_empty());
        // The same with both paths holding the constant: the join keeps it.
        let agree = [lui(2, 0xBF80), beq(4, 0, 2), NOP, lui(2, 0xBF80), sb(5, 2, 0x2F00)];
        assert_eq!(scan_words(&agree, 0x8001_0000).len(), 1);
        // A constant built before a branch is still there after it.
        let across = [lui(2, 0xBF80), bne(4, 0, 2), NOP, NOP, sb(5, 2, 0x2F00)];
        assert_eq!(scan_words(&across, 0x8001_0000).len(), 1);
    }

    #[test]
    fn a_loop_keeps_a_constant_formed_before_it() {
        // lui v0,0xbf80; L: sb a1,0x2080(v0); addiu a0,a0,-1; bne a0,zero,L; nop
        let words = [lui(2, 0xBF80), sb(5, 2, 0x2080), addiu(4, 4, 0xFFFF), bne(4, 0, -3), NOP];
        assert_eq!(scan_words(&words, 0x8001_0000).len(), 1);
        // A value that changes round the loop is not a constant at its head, so it is not tracked.
        let words = [lui(2, 0xBF80), sb(5, 2, 0x2080), addiu(2, 2, 4), bne(4, 0, -3), NOP];
        assert!(scan_words(&words, 0x8001_0000).is_empty());
    }

    #[test]
    fn a_call_drops_caller_saved_registers_after_its_delay_slot_only() {
        // s0 survives a call; v0 does not, and neither does a constant built in the delay slot.
        let kept = [lui(16, 0xBF80), jal(5), NOP, NOP, sb(5, 16, 0x2080), JR_RA, NOP];
        assert_eq!(scan_words(&kept, 0x8001_0000).len(), 1);
        let dropped = [lui(2, 0xBF80), jal(5), NOP, NOP, sb(5, 2, 0x2080), JR_RA, NOP];
        assert!(scan_words(&dropped, 0x8001_0000).is_empty());
        let slot = [jal(5), lui(2, 0xBF80), NOP, NOP, sb(5, 2, 0x2080), JR_RA, NOP];
        assert!(scan_words(&slot, 0x8001_0000).is_empty());
    }
}
