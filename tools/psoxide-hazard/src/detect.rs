//! The one hazard detector: the patcher, the scanner and the stack guard all
//! read jump tables and hazards through here, so a hazard class added once
//! is seen by every tool (two separate copies of the detector once drifted
//! and each missed a class: branch operands 2026-09-04, `jr ra` / `jalr` /
//! unresolved `jr` 2026-09-22).
//!
//! The detector works on objdump-syntax text (see [`crate::listing`]):
//! which register an instruction reads or writes is decided from its
//! mnemonic and operand fields, exactly as the original tools decided it,
//! so the reports and the patched bytes stay the same.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::linkmap::LinkMap;
use crate::listing::{word_at_offset, Entry, Listing, HEADER, LOAD_ADDR};
use crate::text::{fields, int_auto, load_operands, squeeze, strip, trailing_hex, whole_hex};

/// psx-rt's `HAZARD_TRAMPOLINES` magic word ("HAZT").
pub const MAGIC: i64 = 0x4841_5A54;

/// Loads, as the detector sees them: anything whose first operand arrives
/// one instruction late.
pub const LOADS: &[&str] = &[
    "lw", "lh", "lhu", "lb", "lbu", "lwl", "lwr", "lwc2", "mfc0", "mfc2", "cfc2",
];
/// Conditional branches (`b` is `beq zero,zero`).
pub const COND: &[&str] = &[
    "beq", "bne", "beqz", "bnez", "blez", "bgtz", "bltz", "bgez", "b",
];
/// Branches and jumps that write the link register.
pub const LINKING: &[&str] = &["jal", "bal", "bltzal", "bgezal", "jalr"];
/// Unconditional immediate jumps.
pub const JUMPS: &[&str] = &["j", "jal"];
/// Stores.
pub const STORES: &[&str] = &["sw", "sh", "sb", "swl", "swr", "swc2"];
/// Instructions whose every register operand is a source: stores,
/// coprocessor moves, register jumps, multiply/divide, and every conditional
/// branch (a `beqz a2, T` consumer reads a2 as its FIRST operand, so the
/// generic "destination first" rule would miss it; this gap let a memcmp
/// whose entry tested a2 read a stale count for its whole life, 2026-09-04).
pub const READS_ALL: &[&str] = &[
    "sw", "sh", "sb", "swl", "swr", "swc2", "mtc0", "mtc2", "ctc2", "jr", "jalr", "mult", "multu",
    "div", "divu", "mthi", "mtlo", "beq", "bne", "beqz", "bnez", "blez", "bgtz", "bltz", "bgez",
    "bltzal", "bgezal", "beql", "bnel",
];
const WRITES_ONLY: &[&str] = &["lui", "li", "mfhi", "mflo"];
/// Registers a callee preserves (o32): their value survives a call.
pub const CALLEE_SAVED: &[&str] = &[
    "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "fp", "sp",
];
/// Registers whose value on entry a function may use: the arguments, the
/// stack and the return address ($gp is never set up in a PS-EXE).
const INCOMING: &[&str] = &["a0", "a1", "a2", "a3", "sp", "ra", "gp"];
/// Instructions that write no general register (besides READS_ALL).
const NO_DEST: &[&str] = &[
    "nop", "break", "syscall", "rfe", "sync", "teq", "tne", "tge", "tgeu", "tlt", "tltu", "j", "b",
];

/// The most entries `Block` reads from one table. A Rust match on a byte can
/// have 256 cases; Quake's TargetGraph::apply_command has 74 (the old cap of
/// 64 would have cut it). Reaching this means the words after the table
/// are code addresses too, so it says so.
pub const TABLE_CEILING: usize = 4096;

/// True when `op` is in `set`.
pub fn is(op: &str, set: &[&str]) -> bool {
    set.contains(&op)
}

/// Every instruction with a delay slot.
pub fn is_branch(op: &str) -> bool {
    is(op, COND) || is(op, LINKING) || is(op, JUMPS) || op == "jr"
}

/// The data guard: true when no undecodable word sits within `words`
/// instructions on either side of `addr`. A PS-EXE carries its tables and
/// assets in the same load, and those decode as random branches.
pub type IsCode<'a> = &'a dyn Fn(&Listing, i64) -> bool;

/// The default data guard, 16 words either side.
pub fn looks_like_code(listing: &Listing, addr: i64) -> bool {
    (-16..=16).all(|i| listing.op(addr + 4 * i) != ".word")
}

/// The guard for a listing cut to a link map's `.text` (see
/// [`Listing::retain_text`]): there every word is code.
pub fn every_word(_listing: &Listing, _addr: i64) -> bool {
    true
}

/// The register a load writes; `None` for anything else and for loads into
/// $zero (cache probes).
pub fn load_destination<'a>(op: &str, args: &'a str) -> Option<&'a str> {
    if !is(op, LOADS) {
        return None;
    }
    let rd = strip(args.split(',').next().unwrap_or(""));
    (rd != "zero").then_some(rd)
}

/// True when the instruction reads `reg`.
pub fn reads(op: &str, args: &str, reg: &str) -> bool {
    if op == "nop" {
        return false;
    }
    let parts = if args.is_empty() {
        Vec::new()
    } else {
        fields(args)
    };
    let sources: &[&str] = if is(op, LOADS) {
        parts.get(1..).unwrap_or(&[])
    } else if is(op, READS_ALL) {
        &parts
    } else if is(op, WRITES_ONLY) {
        &[]
    } else if parts.len() > 1 {
        &parts[1..]
    } else {
        &parts
    };
    sources
        .iter()
        .any(|source| crate::text::paren_register(source) == Some(reg) || *source == reg)
}

/// True when the instruction writes `reg`.
pub fn writes(op: &str, args: &str, reg: &str) -> bool {
    if matches!(op, "jal" | "bal" | "bltzal" | "bgezal") {
        return reg == "ra";
    }
    if op == "jalr" {
        let parts = fields(args);
        return reg == if parts.len() == 2 { parts[0] } else { "ra" };
    }
    if is(op, READS_ALL) || is(op, NO_DEST) || args.is_empty() {
        return false;
    }
    strip(args.split(',').next().unwrap_or("")) == reg
}

/// The immediate target of a branch or jump, or `None`.
pub fn branch_target(op: &str, args: &str) -> Option<i64> {
    if !is(op, COND) && !matches!(op, "j" | "jal" | "bal" | "bltzal" | "bgezal") {
        return None;
    }
    trailing_hex(args)
}

/// `addiu sp,sp,-N`: a function's first instruction, as far as an image
/// without a map can tell.
fn is_prologue(op: &str, args: &str) -> bool {
    if op != "addiu" {
        return false;
    }
    let squeezed = squeeze(args);
    squeezed
        .strip_prefix("sp,sp,-")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The value lui, li, addiu, ori or move computes, with `operand(reg)`
/// giving its source register's value; `None` for anything else or unknown.
fn constant(op: &str, args: &str, operand: &mut dyn FnMut(&str) -> Option<i64>) -> Option<i64> {
    let parts = fields(args);
    match op {
        "lui" => Some(int_auto(parts.get(1)?)?.wrapping_shl(16) & 0xFFFF_FFFF),
        "li" => Some(int_auto(parts.get(1)?)? & 0xFFFF_FFFF),
        "addiu" | "ori" | "move" => {
            let mut v = operand(parts.get(1)?)?;
            if op == "addiu" {
                v = v.wrapping_add(int_auto(parts.get(2)?)?);
            } else if op == "ori" {
                v |= int_auto(parts.get(2)?)?;
            }
            Some(v & 0xFFFF_FFFF)
        }
        _ => None,
    }
}

/// The operands of a conditional branch that are registers.
pub fn branch_sources(args: &str) -> Vec<&str> {
    fields(args)
        .into_iter()
        .filter(|p| !p.starts_with("0x"))
        .collect()
}

/// A `j`/`jal` word to `target`.
pub fn encode_j(target: i64, link: bool) -> u32 {
    ((if link { 3 } else { 2 }) << 26) | ((target >> 2) as u32 & 0x03FF_FFFF)
}

/// A listed instruction the detector needed but objdump did not list (a
/// collapsed zero run where a jump table entry lands). The original tools
/// stopped with a `KeyError` here.
#[derive(Debug)]
pub struct Unlisted(pub i64);

/// The image a detector reads: the listing, the bytes it came from and
/// where they load.
#[derive(Clone, Copy)]
pub struct Image<'a> {
    /// The listing of `data`.
    pub listing: &'a Listing,
    /// The whole file, header included.
    pub data: &'a [u8],
    /// The load address.
    pub base: i64,
    /// One past the last loaded byte.
    pub image_end: i64,
}

impl Image<'_> {
    /// The word loaded at `addr` (0 outside the file).
    pub fn word_at(&self, addr: i64) -> i64 {
        word_at_offset(self.data, addr - self.base + HEADER).map_or(0, i64::from)
    }

    /// The listed instruction at `addr`; a zero word objdump collapsed into
    /// `...` reads as `nop`.
    fn instruction(&self, addr: i64) -> Option<(&str, &str)> {
        match self.listing.get(addr) {
            Some(e) => Some((e.op, e.args.as_str())),
            None if self.base <= addr && addr < self.image_end && self.word_at(addr) == 0 => {
                Some(("nop", ""))
            }
            None => None,
        }
    }

    fn entry(&self, addr: i64) -> Result<&Entry, Unlisted> {
        self.listing.get(addr).ok_or(Unlisted(addr))
    }
}

/// Resolves a `jr` switch dispatch without a link map, from the dispatch's
/// straight-line block alone. An older resolver took the nearest `lui` of
/// any register and crossed labels, so it could name another table; a
/// wrong table hides a hazard from the patcher and the scanner.
///
/// The jump register must come from `lw rs, off(b)` and `b` from
/// `addu b, x, y` with exactly one of x, y a constant (lui/li/addiu/ori/move
/// chains, as [`Flow`] accepts), each found by walking back from its reader.
/// The walk stops, and the site stays unresolved, where the block may be
/// entered some other way: a branch target, an address a data word names
/// (a case label, a function pointer), a function prologue
/// (`addiu sp,sp,-N`), a word that does not decode, the fall-through of an
/// unconditional jump or a return. It crosses a call only for a
/// callee-saved register. Unresolved costs the patcher one trampoline that
/// moves the load out of the slot, which is always safe; a guess costs a
/// missed hazard when it is wrong. `--map` proves bases loaded farther away
/// (hoisted out of a loop, kept across a branch).
///
/// Tables sit back to back in `.rodata`, so a table runs until the next
/// table any dispatch in the image resolves to, or its first word that is
/// not a code address, at most [`TABLE_CEILING`] entries. On the eleven
/// images with maps it was checked against (2026-09-23), every table it
/// resolved that `Flow` also proved had the same address and at least
/// `Flow`'s entries; the old fixed cap of 64 cut Quake's 74-entry table.
struct Block {
    labels: HashSet<i64>,
    warned: HashSet<i64>,
    starts: Option<Vec<i64>>,
}

impl Block {
    fn new(image: &Image<'_>) -> Self {
        let mut labels = HashSet::new();
        for (_, e) in image.listing.iter() {
            if let Some(target) = branch_target(e.op, &e.args) {
                labels.insert(target);
            }
        }
        let mut addr = image.base;
        while addr < image.image_end - 3 {
            let word = image.word_at(addr);
            if image.base <= word && word < image.image_end && word & 3 == 0 {
                labels.insert(word);
            }
            addr += 4;
        }
        Self {
            labels,
            warned: HashSet::new(),
            starts: None,
        }
    }

    /// True when control may reach `addr` other than from `addr - 4`.
    fn entered_here(&self, image: &Image<'_>, addr: i64) -> bool {
        self.labels.contains(&addr)
            || image
                .instruction(addr)
                .is_some_and(|(op, args)| is_prologue(op, args))
    }

    /// The instruction whose write of `reg` reaches `at` inside its
    /// straight-line block, or `None`.
    fn writer(&self, image: &Image<'_>, reg: &str, at: i64) -> Option<i64> {
        let mut x = at;
        for _ in 0..256 {
            if self.entered_here(image, x) {
                return None;
            }
            let y = x - 4;
            let (op, args) = image.instruction(y)?;
            if op == ".word" {
                return None;
            }
            let before = image.instruction(y - 4).map_or("", |e| e.0);
            if matches!(before, "j" | "b" | "jr") {
                return None; // x follows an unconditional jump: only a label reaches it
            }
            if is(before, LINKING) && !is(reg, CALLEE_SAVED) {
                return None; // x is a call's return point and the callee may change reg
            }
            if writes(op, args, reg) {
                return Some(y);
            }
            x = y;
        }
        None
    }

    fn value(&self, image: &Image<'_>, reg: &str, at: i64, depth: u32) -> Option<i64> {
        if reg == "zero" {
            return Some(0);
        }
        if depth >= 8 {
            return None;
        }
        let d = self.writer(image, reg, at)?;
        let (op, args) = image.instruction(d)?;
        constant(op, args, &mut |source| {
            self.value(image, source, d, depth + 1)
        })
    }

    fn table_address(&self, image: &Image<'_>, jr_addr: i64) -> Option<i64> {
        let rs = strip(image.listing.args(jr_addr)).to_string();
        let load = self.writer(image, &rs, jr_addr)?;
        let (op, args) = image.instruction(load)?;
        let squeezed = squeeze(args);
        let (offset, pointer) = load_operands(&squeezed)?;
        if op != "lw" {
            return None;
        }
        let pointer = pointer.to_string();
        let total = self.writer(image, &pointer, load)?;
        let (op, args) = image.instruction(total)?;
        let parts = fields(args);
        if op != "addu" || parts.len() != 3 {
            return None;
        }
        let known: Vec<i64> = [
            self.value(image, parts[1], total, 0),
            self.value(image, parts[2], total, 0),
        ]
        .into_iter()
        .flatten()
        .collect();
        if known.len() != 1 {
            return None;
        }
        Some((known[0] + offset) & 0xFFFF_FFFF)
    }

    /// Every table address a dispatch in code resolves to, sorted.
    fn table_starts(&mut self, image: &Image<'_>) -> Vec<i64> {
        if self.starts.is_none() {
            let mut set: Vec<i64> = image
                .listing
                .iter()
                .filter(|(a, e)| {
                    e.op == "jr" && strip(&e.args) != "ra" && looks_like_code(image.listing, *a)
                })
                .filter_map(|(a, _)| self.table_address(image, a))
                .collect();
            set.sort_unstable();
            set.dedup();
            self.starts = Some(set);
        }
        self.starts.clone().unwrap()
    }

    fn jump_table(
        &mut self,
        image: &Image<'_>,
        jr_addr: i64,
        ceiling: usize,
        warnings: &mut Vec<String>,
    ) -> Option<Vec<(i64, i64)>> {
        let table = self.table_address(image, jr_addr)?;
        let starts = self.table_starts(image);
        let stop = starts
            .iter()
            .copied()
            .find(|&s| s > table)
            .unwrap_or(image.image_end)
            .min(image.image_end);
        let mut entries = Vec::new();
        let mut addr = table;
        while let Some(target) = self.target(image, addr, stop) {
            if entries.len() >= ceiling {
                break;
            }
            entries.push((addr, target));
            addr += 4;
        }
        if self.target(image, addr, stop).is_some() && self.warned.insert(jr_addr) {
            warnings.push(format!(
                "warning: the jump table of the jr at {jr_addr:08x} ({table:08x}) still names code after {ceiling} \
                 entries; read no further. Pass --map to bound it to its function"
            ));
        }
        (!entries.is_empty()).then_some(entries)
    }

    /// The code address the table word at `addr` names, or `None`.
    fn target(&self, image: &Image<'_>, addr: i64, stop: i64) -> Option<i64> {
        if !(image.base <= addr && addr < stop) {
            return None;
        }
        let target = image.word_at(addr);
        let listed = image.listing.get(target);
        if target & 3 != 0
            || !(image.base <= target && target < image.image_end)
            || listed.is_none_or(|e| e.op == ".word")
        {
            return None;
        }
        Some(target)
    }
}

type Function = (i64, i64);

/// A proven table: its `(entry, target)` pairs and the set of targets.
type Extent = (Vec<(i64, i64)>, HashSet<i64>);

/// Proves where a `jr` switch dispatch reads its target, from the control
/// flow of one function of a linked image and its link map.
///
/// The table address is a constant LLVM loads once per switch (`lui`, maybe
/// `addiu`, often hoisted out of a loop into a callee-saved register), and
/// it holds on every path the compiler laid out to the dispatch. So the
/// proof walks back from the `jr` along edges that are certainly in the
/// compiled code, and every write of the base register it reaches must
/// compute the same constant. The edges:
///
/// * fall-through, and branches and jumps within the function or through
///   its hazard trampolines;
/// * past a call, only for a callee-saved register and only when the
///   callee can return (it has a `jr` or jumps out); after a noreturn call
///   (a panic) the next word is some other block, often a switch case;
/// * from a switch whose table is already proven to each case it lists.
///
/// A path that reaches something the image cannot show is unknown, and the
/// dispatch stays unresolved: an argument, $sp or $ra at the function's
/// first instruction, a branch in from other code, an undecodable word.
/// A path is dropped when the o32 ABI says compiled code never reads a
/// value along it (a register a call may change, a register that carries
/// nothing into the function). Dropping a real edge can only lose a proof;
/// following one that is not real could find a wrong write, which is why
/// the edges above are restricted to ones the code has. The last kind can
/// overshoot: until the next table of the function is proven, a table seems
/// to run on into it and lends that table's cases to the wrong switch. The
/// writes such a path finds must still agree with every other; a wrong
/// proof needs every real path to dead-end and the lent ones to agree on
/// another constant whose table also jumps only into the function.
///
/// A table must start inside a `.rodata*` input section of the map. LLVM
/// emits every switch table there, and psoxide.ld links `.rodata.*` into
/// the same `.data` output section as mutable `.data.*`: a `static mut`
/// array of code pointers indexed by `lw` reads like a table, but what the
/// image holds is only its initial value, so proving jump targets from it,
/// or patching an entry, would be wrong. Unresolved, its dispatch gets the
/// safe trampoline that moves the slot load out.
///
/// Nothing else bounds a table in the image: an index that is an enum tag
/// has no range check (hk-psx frame::simulate). A proven table runs to the
/// next proven table's start, the end of its `.rodata` section, or its
/// first word that does not jump into the function past its first
/// instruction (a switch never jumps to its function's entry, and a
/// function pointer names nothing else); tables sit back to back in the
/// function's `.rodata`, and reading on would take the next function's
/// table for this one's (hk-psx, 2026-09-23: presentation::service's table
/// ran into menu::run's). An entry may be a `nop ; j T ; nop` trampoline
/// into the function: an earlier patch pointed it there. Every switch of a
/// function is proven together, round by round (one that sits in another's
/// case is only reached through that one's table); then each proof is
/// checked again with the final tables, and one that no longer holds is
/// dropped and the rounds resume.
///
/// On the hk-psx, Quake and GoldSrc images it was checked against
/// (2026-09-23), every table proven this way lies inside its function's own
/// `.rodata.<function>` section, and together they cover each such section
/// exactly.
struct Flow {
    sources: HashMap<i64, Vec<i64>>,
    tables: HashMap<Function, HashMap<i64, Extent>>,
    returning: HashMap<i64, bool>,
}

impl Flow {
    fn new(image: &Image<'_>) -> Self {
        let mut sources: HashMap<i64, Vec<i64>> = HashMap::new();
        for (addr, e) in image.listing.iter() {
            if let Some(target) = branch_target(e.op, &e.args) {
                sources.entry(target).or_default().push(addr);
            }
        }
        Self {
            sources,
            tables: HashMap::new(),
            returning: HashMap::new(),
        }
    }

    fn in_tramps(map: &LinkMap, addr: i64) -> bool {
        map.trampolines
            .is_some_and(|(t0, t1)| t0 <= addr && addr < t1)
    }

    /// T when `addr` is a `nop ; j T ; nop` hazard trampoline.
    fn trampoline_target(image: &Image<'_>, map: &LinkMap, addr: i64) -> Option<i64> {
        if !Self::in_tramps(map, addr) {
            return None;
        }
        if image.listing.op(addr + 4) != "j" {
            return None;
        }
        let target = whole_hex(strip(image.listing.args(addr + 4)))?;
        (image.listing.op(addr) == "nop" && image.listing.op(addr + 8) == "nop").then_some(target)
    }

    /// False when the call at `call` certainly does not come back: its
    /// callee has no `jr` and never jumps out of itself.
    fn returns(&mut self, image: &Image<'_>, map: &LinkMap, call: i64) -> bool {
        let Some((op, args)) = image.instruction(call) else {
            return true;
        };
        let Some(target) = branch_target(op, args) else {
            return true;
        };
        let target = Self::trampoline_target(image, map, target)
            .filter(|&t| t != 0)
            .unwrap_or(target);
        if let Some(&known) = self.returning.get(&target) {
            return known;
        }
        let result = match map.function(target) {
            None => true,
            Some((start, end, _)) => (start..end).step_by(4).any(|a| {
                let (op, args) = image.instruction(a).unwrap_or(("", ""));
                op == "jr"
                    || ((op == "j" || is(op, COND)) && {
                        let to = branch_target(op, args).filter(|&t| t != 0).unwrap_or(start);
                        !(start <= to && to < end)
                    })
            }),
        };
        self.returning.insert(target, result);
        result
    }

    /// Instructions that can run just before `x` with `reg` still holding
    /// the value that reaches `x`, or `None` when one is unknown.
    fn preds(
        &mut self,
        image: &Image<'_>,
        map: &LinkMap,
        x: i64,
        reg: &str,
        func: Function,
    ) -> Option<Vec<i64>> {
        let (start, end) = func;
        if x == start {
            return if is(reg, INCOMING) {
                None
            } else {
                Some(Vec::new())
            };
        }
        let mut found: Vec<i64> = self
            .tables
            .get(&func)
            .map(|t| {
                t.iter()
                    .filter(|(_, (_, targets))| targets.contains(&x))
                    .map(|(j, _)| j + 4)
                    .collect()
            })
            .unwrap_or_default();
        for &source in self.sources.get(&x).map(Vec::as_slice).unwrap_or(&[]) {
            if (start <= source && source < end) || Self::in_tramps(map, source) {
                found.push(source + 4);
            } else if map.text.0 <= source && source < map.text.1 {
                return None;
            }
            // Otherwise a data word that decodes as a branch: data never runs.
        }
        let area = if start <= x && x < end {
            Some(func)
        } else {
            map.trampolines
        };
        if let Some(area) = area {
            if area.0 <= x - 4 && x - 4 < area.1 {
                let before = if area.0 <= x - 8 {
                    image.instruction(x - 8).map_or("", |e| e.0)
                } else {
                    ""
                };
                if is(before, LINKING) {
                    if is(reg, CALLEE_SAVED) && self.returns(image, map, x - 8) {
                        found.push(x - 4);
                    }
                } else if !matches!(before, "j" | "b" | "jr") {
                    found.push(x - 4);
                }
            }
        }
        Some(found)
    }

    /// The instructions whose write of `reg` the walk carries to `at`, or
    /// `None` when a path leads somewhere unknown first.
    fn reaching(
        &mut self,
        image: &Image<'_>,
        map: &LinkMap,
        reg: &str,
        at: i64,
        func: Function,
    ) -> Option<HashSet<i64>> {
        let mut todo = self.preds(image, map, at, reg, func)?;
        let mut defs = HashSet::new();
        let mut seen = HashSet::new();
        while let Some(y) = todo.pop() {
            if !seen.insert(y) {
                continue;
            }
            let (op, args) = image.instruction(y)?;
            if op == ".word" {
                return None;
            }
            if writes(op, args, reg) {
                defs.insert(y);
                continue;
            }
            todo.extend(self.preds(image, map, y, reg, func)?);
        }
        Some(defs)
    }

    /// The constant `reg` holds when `at` runs, or `None`. Only lui, li,
    /// addiu, ori and move are followed, and every reaching write must agree.
    fn value(
        &mut self,
        image: &Image<'_>,
        map: &LinkMap,
        reg: &str,
        at: i64,
        func: Function,
        depth: u32,
    ) -> Option<i64> {
        if reg == "zero" {
            return Some(0);
        }
        if depth >= 8 {
            return None;
        }
        let defs = self.reaching(image, map, reg, at, func)?;
        if defs.is_empty() {
            return None;
        }
        let mut values = HashSet::new();
        for d in defs {
            let (op, args) = image.instruction(d)?;
            let v = constant(op, args, &mut |source| {
                self.value(image, map, source, d, func, depth + 1)
            })?;
            values.insert(v);
        }
        if values.len() == 1 {
            values.into_iter().next()
        } else {
            None
        }
    }

    /// Where `jr rs` reads its target: `rs` must come from one
    /// `lw rs, off(b)` whose `b` is always `addu b, x, y` with exactly one
    /// of x, y a constant C (the other is the scaled index). The table is at
    /// C + off.
    fn table_address(
        &mut self,
        image: &Image<'_>,
        map: &LinkMap,
        jr_addr: i64,
        func: Function,
    ) -> Option<i64> {
        let rs = strip(image.listing.args(jr_addr)).to_string();
        let loads = self.reaching(image, map, &rs, jr_addr, func)?;
        if loads.len() != 1 {
            return None;
        }
        let load = *loads.iter().next().unwrap();
        let (op, args) = image.instruction(load)?;
        let squeezed = squeeze(args);
        let (offset, pointer) = load_operands(&squeezed)?;
        if op != "lw" {
            return None;
        }
        let pointer = pointer.to_string();
        let sums = self.reaching(image, map, &pointer, load, func)?;
        if sums.is_empty() {
            return None;
        }
        let mut tables = HashSet::new();
        for d in sums {
            let (op, args) = image.instruction(d)?;
            let parts: Vec<String> = fields(args).into_iter().map(str::to_string).collect();
            if op != "addu" || parts.len() != 3 {
                return None;
            }
            let known: Vec<i64> = [
                self.value(image, map, &parts[1], d, func, 0),
                self.value(image, map, &parts[2], d, func, 0),
            ]
            .into_iter()
            .flatten()
            .collect();
            if known.len() != 1 {
                return None;
            }
            tables.insert((known[0] + offset) & 0xFFFF_FFFF);
        }
        if tables.len() == 1 {
            tables.into_iter().next()
        } else {
            None
        }
    }

    /// The proven `(entry, target)` pairs of `jr_addr`'s table, or `None`.
    fn jump_table(
        &mut self,
        image: &Image<'_>,
        map: &LinkMap,
        jr_addr: i64,
        func: Function,
    ) -> Option<Vec<(i64, i64)>> {
        if !self.tables.contains_key(&func) {
            self.solve(image, map, func);
        }
        self.tables[&func].get(&jr_addr).map(|got| got.0.clone())
    }

    /// Prove every switch of the function together (see the type).
    fn solve(&mut self, image: &Image<'_>, map: &LinkMap, func: Function) {
        let (start, end) = func;
        let is_switch =
            |a: i64| image.listing.op(a) == "jr" && strip(image.listing.args(a)) != "ra";
        let mut sites: Vec<i64> = (start..end).step_by(4).filter(|&a| is_switch(a)).collect();
        if let Some((t0, t1)) = map.trampolines {
            // A patched `jr` moved into a trampoline the function jumps to.
            sites.extend((t0..t1).step_by(4).filter(|&a| {
                is_switch(a)
                    && self
                        .sources
                        .get(&(a - 4))
                        .is_some_and(|s| s.iter().any(|&s| start <= s && s < end))
            }));
        }
        // `proven` keeps insertion order, as the round-by-round proof does.
        let mut proven: Vec<(i64, i64)> = Vec::new();
        for _ in 0..4 * sites.len() + 4 {
            let mut starts: Vec<i64> = proven.iter().map(|p| p.1).collect();
            starts.sort_unstable();
            starts.dedup();
            let round: HashMap<i64, _> = proven
                .iter()
                .map(|&(j, t)| (j, self.extent(image, map, t, &starts, func)))
                .collect();
            self.tables.insert(func, round);
            let mut fresh = Vec::new();
            for &j in &sites {
                if !proven.iter().any(|p| p.0 == j) {
                    if let Some(table) = self.table_address(image, map, j, func) {
                        if !self.extent(image, map, table, &starts, func).0.is_empty()
                            && !fresh.iter().any(|f: &(i64, i64)| f.0 == j)
                        {
                            fresh.push((j, table));
                        }
                    }
                }
            }
            if !fresh.is_empty() {
                proven.extend(fresh);
                continue;
            }
            let stale: Vec<i64> = proven
                .clone()
                .into_iter()
                .filter(|&(j, t)| self.table_address(image, map, j, func) != Some(t))
                .map(|(j, _)| j)
                .collect();
            if stale.is_empty() {
                return;
            }
            proven.retain(|p| !stale.contains(&p.0));
        }
        self.tables.insert(func, HashMap::new());
    }

    /// `(entries, targets)` of the table at `table`, up to the next of
    /// `starts`, the end of its `.rodata` input section, or its first word
    /// that does not jump into the function past its first instruction. A
    /// table outside `.rodata` has no entries.
    fn extent(
        &self,
        image: &Image<'_>,
        map: &LinkMap,
        table: i64,
        starts: &[i64],
        func: Function,
    ) -> Extent {
        let (start, end) = func;
        let Some(section) = map.rodata_section(table) else {
            return (Vec::new(), HashSet::new());
        };
        let stop = starts
            .iter()
            .copied()
            .find(|&s| s > table)
            .unwrap_or(image.image_end)
            .min(section.1)
            .min(image.image_end);
        let mut entries = Vec::new();
        let mut addr = table;
        while image.base <= addr && addr < stop {
            let target = image.word_at(addr);
            let into = Self::trampoline_target(image, map, target)
                .filter(|&t| t != 0)
                .unwrap_or(target);
            if target & 3 != 0 || !(start < into && into < end) {
                break;
            }
            entries.push((addr, target));
            addr += 4;
        }
        let targets = entries.iter().map(|e| e.1).collect();
        (entries, targets)
    }
}

/// One hazard: a branch whose delay slot loads a register the next executed
/// instruction reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hazard {
    /// The branch.
    pub addr: i64,
    /// Its mnemonic and operands.
    pub op: &'static str,
    /// Its operands.
    pub args: String,
    /// The delay-slot load's mnemonic.
    pub slot_op: &'static str,
    /// The delay-slot load's operands.
    pub slot_args: String,
    /// The instruction that reads the load too early; `None` for a register
    /// jump whose destination is not in the image (`jr ra`, `jalr`, and a
    /// `jr` whose table cannot be resolved).
    pub consumer: Option<i64>,
    /// The jump-table word that names the consumer, for a `jr` switch.
    pub entry: Option<i64>,
}

/// Finds hazards and jump tables in one image. It keeps the per-image state
/// the searches build (the block labels, the proven tables), so each image
/// gets its own.
pub struct Detector<'a> {
    /// The image.
    pub image: Image<'a>,
    map: Option<&'a LinkMap>,
    block: RefCell<Option<Block>>,
    flow: RefCell<Option<Flow>>,
    /// Where `Block` stops reading a table (see [`TABLE_CEILING`]).
    pub table_ceiling: usize,
    warnings: RefCell<Vec<String>>,
}

impl<'a> Detector<'a> {
    /// A detector over `image`; `map`, a [`LinkMap`] of the same link,
    /// makes [`Detector::jump_table`] prove each table and bound it to its
    /// own function.
    pub fn new(image: Image<'a>, map: Option<&'a LinkMap>) -> Self {
        Self {
            image,
            map,
            block: RefCell::new(None),
            flow: RefCell::new(None),
            table_ceiling: TABLE_CEILING,
            warnings: RefCell::new(Vec::new()),
        }
    }

    /// Warnings the searches printed since the last call, in order.
    pub fn take_warnings(&self) -> Vec<String> {
        std::mem::take(&mut self.warnings.borrow_mut())
    }

    fn with_block<R>(&self, f: impl FnOnce(&mut Block) -> R) -> R {
        let mut block = self.block.borrow_mut();
        f(block.get_or_insert_with(|| Block::new(&self.image)))
    }

    /// Resolve the table a `jr rs` dispatches through: `(entry address,
    /// target)` pairs, or `None`. LLVM lowers a switch as `sll idx,idx,2 ;
    /// lui t,%hi(T) ; addu ; lw rs,%lo(T)(...) ; jr rs`.
    ///
    /// With the link map the answer is proven, and only a table in
    /// `.rodata` counts; see `Flow`. Without one, `Block` follows the jump
    /// register back to that load and its base back to the constant, inside
    /// the dispatch's own straight-line block, and gives up (`None`, so the
    /// site stays unresolved) when the chain leaves the block. Entries then
    /// run until the next table another dispatch resolves to, or a word
    /// that is not a code address. A neighbouring table no dispatch
    /// resolves still reads as part of this one; nothing without function
    /// bounds can tell whose table its words are, so it reads on rather
    /// than drop a real entry. For the patcher an extra entry costs one
    /// detour when its target happens to read the slot load; the stack
    /// guard always has a map.
    pub fn jump_table(&self, jr_addr: i64) -> Option<Vec<(i64, i64)>> {
        match self.map {
            Some(map) => {
                let (start, end, _) = map.function(jr_addr)?;
                let mut flow = self.flow.borrow_mut();
                flow.get_or_insert_with(|| Flow::new(&self.image))
                    .jump_table(&self.image, map, jr_addr, (start, end))
            }
            None => {
                let ceiling = self.table_ceiling;
                let mut warnings = self.warnings.borrow_mut();
                self.with_block(|block| {
                    block.jump_table(&self.image, jr_addr, ceiling, &mut warnings)
                })
            }
        }
    }

    /// One line for a scan without a link map of an image that has register
    /// jumps, or `None`: without the map no table is proven.
    pub fn unmapped_warning(&self, is_code: IsCode<'_>) -> Option<String> {
        let listing = self.image.listing;
        let sites: Vec<i64> = listing
            .iter()
            .filter(|(a, e)| e.op == "jr" && strip(&e.args) != "ra" && is_code(listing, *a))
            .map(|(a, _)| a)
            .collect();
        if sites.is_empty() {
            return None;
        }
        let resolved = self.with_block(|block| {
            sites
                .iter()
                .filter(|&&a| block.table_address(&self.image, a).is_some())
                .count()
        });
        Some(format!(
            "warning: no --map: {resolved} of {} register jumps resolve to a jump table from their own block and none \
             is proven; pass the link map (--map game.map)",
            sites.len()
        ))
    }

    /// Every hazard, in address order. `is_code` is the data guard (see
    /// [`looks_like_code`]); a caller that knows its .text can pass one that
    /// never skips it.
    pub fn find_hazards(&self, is_code: IsCode<'_>) -> Result<Vec<Hazard>, Unlisted> {
        let image = &self.image;
        let listing = image.listing;
        let mut found = Vec::new();
        for (addr, e) in listing.iter() {
            let (op, args) = (e.op, e.args.as_str());
            let Some(slot) = listing.get(addr + 4) else {
                continue;
            };
            if !is_branch(op) {
                continue;
            }
            let Some(rd) = load_destination(slot.op, &slot.args) else {
                continue;
            };
            if !is_code(listing, addr) {
                continue;
            }
            let hazard = |consumer, entry| Hazard {
                addr,
                op,
                args: args.to_string(),
                slot_op: slot.op,
                slot_args: slot.args.clone(),
                consumer,
                entry,
            };
            if op == "jr" && strip(args) == "ra" {
                // A function returning a value loaded in its own delay slot:
                // the caller's first instruction reads it one instruction
                // early, and through a function pointer there is no call
                // site to check, so every one counts (cs-psx 31517d4, hl-psx
                // settings::value). A slot load into ra itself only changes
                // a register the caller never reads before restoring it, so
                // it is left alone.
                if rd != "ra" {
                    found.push(hazard(None, None));
                }
                continue;
            }
            if op == "jalr" {
                // The callee, and so its first instruction, is unknown.
                found.push(hazard(None, None));
                continue;
            }
            if op == "jr" {
                // A switch dispatch: the table words are data, so an entry
                // whose target consumes the slot load can be pointed at a
                // trampoline. An unresolved table leaves the target unknown,
                // like a return.
                match self.jump_table(addr) {
                    None => found.push(hazard(None, None)),
                    Some(entries) => {
                        for (entry, target) in entries {
                            let t = image.entry(target)?;
                            if reads(t.op, &t.args, rd) {
                                found.push(hazard(Some(target), Some(entry)));
                            }
                        }
                    }
                }
                continue;
            }
            let mut targets = Vec::new();
            if let Some(target) = trailing_hex(args) {
                targets.push(target);
            }
            // A call returns to the fall-through much later; only its target
            // can consume the slot load early. An unconditional jump never
            // falls through.
            if !is(op, JUMPS) && !is(op, LINKING) {
                targets.push(addr + 8);
            }
            for target in targets {
                if let Some(t) = listing.get(target) {
                    if reads(t.op, &t.args, rd) {
                        found.push(hazard(Some(target), None));
                    }
                }
            }
        }
        Ok(found)
    }
}

/// Addresses of loads the very next instruction reads, outside any delay
/// slot. LLVM's MIPS-I scheduler keeps these apart; a register allocator or
/// scheduler switch that breaks that (-regalloc=pbqp did, 2026-09-04) shows
/// up here first.
pub fn straight_line_pairs(listing: &Listing, is_code: IsCode<'_>) -> Vec<i64> {
    let mut pairs = Vec::new();
    for (addr, e) in listing.iter() {
        let Some(rd) = load_destination(e.op, &e.args) else {
            continue;
        };
        let Some(next) = listing.get(addr + 4) else {
            continue;
        };
        if !is_code(listing, addr) {
            continue;
        }
        if is_branch(listing.op(addr - 4)) && listing.contains(addr - 4) {
            continue; // a delay-slot load is find_hazards' case
        }
        if reads(next.op, &next.args, rd) {
            pairs.push(addr);
        }
    }
    pairs
}

/// The default load address, for callers building images by hand.
pub const DEFAULT_LOAD_ADDR: i64 = LOAD_ADDR;
