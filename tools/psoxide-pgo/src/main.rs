//! Turn an emulator PC histogram into an LLVM sample profile (AutoFDO text).
//!
//! The emulator knows exactly how often every guest instruction ran, so a PS1
//! program can be profile-guided with no instrumentation in the guest:
//!
//! 1. Build the guest with `-Cdebuginfo=1 -Zdebug-info-for-profiling
//!    -Cstrip=none`, once as the usual flat image and once more without
//!    `--oformat=binary` so the ELF keeps its DWARF. Both hold the same code at
//!    the same addresses.
//! 2. Run the flat image with `frontend launch --pc-sample-log pc.csv
//!    --pc-sample-instructions 61` over a representative input tape.
//! 3. `psoxide-pgo game.elf pc.csv game.prof`
//! 4. Rebuild with `-Zprofile-sample-use=game.prof` added to the same flags.
//!
//! hl-psx measured +9.3% rendered FPS on the route that produced the profile
//! and +5.9% on one that shared no map with it.
//!
//! Profile names are the build's mangled symbols, and every Rust symbol
//! carries its crate's disambiguator. Cargo derives that from the absolute
//! path of each path dependency outside the workspace (a game's
//! `../.psoxide/sdk` crates), so the same commit checked out somewhere else
//! names every function differently and LLVM matches none of the profile. To
//! use a profile made elsewhere:
//!
//! - `psoxide-pgo portable game.prof portable.prof` rewrites the names
//!   without disambiguators. This is the form to commit.
//! - `psoxide-pgo rebind portable.prof target.elf game.prof` maps them back
//!   onto the symbols of a DWARF ELF built where the profile will be used,
//!   with the same features and profile settings. Adding
//!   `-Zprofile-sample-use` does not change the disambiguators, so the ELF
//!   from step 1 in that checkout serves.

use std::borrow::Cow;
use std::collections::HashMap;
use std::error::Error;
use std::fmt::Write as _;
use std::process::ExitCode;
use std::{env, fs};

use gimli::{AttributeValue, DebuggingInformationEntry, EndianSlice, RunTimeEndian, UnitOffset};
use object::{Object, ObjectSection, ObjectSymbol};

type Reader<'a> = EndianSlice<'a, RunTimeEndian>;
type Dwarf<'a> = gimli::Dwarf<Reader<'a>>;
type Unit<'a> = gimli::Unit<Reader<'a>>;
type Die<'a> = DebuggingInformationEntry<Reader<'a>>;
type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// `DILocation::getBaseDiscriminatorFromDiscriminator`: the base component of
/// LLVM's prefix-encoded discriminator.
fn base_discriminator(raw: u64) -> u32 {
    if raw & 1 != 0 {
        return 0;
    }
    let d = (raw >> 1) as u32;
    if d & 0x20 != 0 {
        ((d >> 1) & 0xFE0) | (d & 0x1F)
    } else {
        d & 0x1F
    }
}

/// Linkage name and declaration line of one function.
#[derive(Clone)]
struct Func {
    name: Option<String>,
    line: u64,
}

/// An inlined call: where it sits, what it calls, and what it inlines in turn.
struct Inlined {
    ranges: Vec<(u64, u64)>,
    func: Func,
    call_line: u64,
    call_discriminator: u32,
    children: Vec<Inlined>,
}

struct Subprogram {
    ranges: Vec<(u64, u64)>,
    func: Func,
    children: Vec<Inlined>,
}

/// One function body in the profile: max sample per (line offset,
/// discriminator), and the inlined callees under their call sites.
#[derive(Default)]
struct Node {
    body: HashMap<(u32, u32), u64>,
    calls: HashMap<(u32, u32, String), Node>,
    head: u64,
}

impl Node {
    fn total(&self) -> u64 {
        self.body.values().sum::<u64>() + self.calls.values().map(Node::total).sum::<u64>()
    }

    /// LLVM's text sample format: one space of indent per inline level.
    fn emit(&self, depth: usize, out: &mut String) {
        let mut items: Vec<(u32, u32, &str, Option<&Node>, u64)> = self
            .body
            .iter()
            .map(|(&(offset, disc), &count)| (offset, disc, "", None, count))
            .collect();
        items.extend(self.calls.iter().map(|((offset, disc, callee), node)| {
            (*offset, *disc, callee.as_str(), Some(node), 0)
        }));
        items.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
        for (offset, disc, callee, node, count) in items {
            let _ = write!(out, "{:depth$}{offset}", "");
            if disc != 0 {
                let _ = write!(out, ".{disc}");
            }
            match node {
                None => {
                    let _ = writeln!(out, ": {count}");
                }
                Some(node) => {
                    let _ = writeln!(out, ": {callee}:{}", node.total());
                    node.emit(depth + 1, out);
                }
            }
        }
    }
}

struct Symbols<'a> {
    dwarf: &'a Dwarf<'a>,
    units: Vec<Unit<'a>>,
    funcs: HashMap<usize, Func>,
}

impl<'a> Symbols<'a> {
    /// The DIE an `abstract_origin` or `specification` attribute points at,
    /// which under LTO is often in another unit.
    fn follow(
        &self,
        unit: usize,
        value: AttributeValue<Reader<'a>>,
    ) -> Option<(usize, UnitOffset)> {
        match value {
            AttributeValue::UnitRef(offset) => Some((unit, offset)),
            AttributeValue::DebugInfoRef(offset) => {
                self.units.iter().enumerate().find_map(|(index, unit)| {
                    offset
                        .to_unit_offset(&unit.header)
                        .map(|offset| (index, offset))
                })
            }
            _ => None,
        }
    }

    fn string(&self, unit: usize, value: AttributeValue<Reader<'a>>) -> Option<String> {
        let text = self.dwarf.attr_string(&self.units[unit], value).ok()?;
        Some(text.to_string_lossy().into_owned())
    }

    fn section_offset(&self, unit: usize, offset: UnitOffset) -> usize {
        offset
            .to_debug_info_offset(&self.units[unit].header)
            .map_or(0, |offset| offset.0)
    }

    /// First linkage name and first declaration line along the chain of
    /// abstract origins and specifications.
    fn resolve(&mut self, unit: usize, offset: UnitOffset) -> Result<Func> {
        let key = self.section_offset(unit, offset);
        if let Some(func) = self.funcs.get(&key) {
            return Ok(func.clone());
        }
        let (mut name, mut line) = (None, None);
        let mut at = Some((unit, offset));
        for _ in 0..8 {
            let Some((u, o)) = at else { break };
            let die = self.units[u].entry(o)?;
            if name.is_none() {
                for attr in [gimli::DW_AT_linkage_name, gimli::DW_AT_MIPS_linkage_name] {
                    if let Some(value) = die.attr_value(attr) {
                        name = self.string(u, value);
                        break;
                    }
                }
            }
            if line.is_none() {
                line = die
                    .attr_value(gimli::DW_AT_decl_line)
                    .and_then(|v| v.udata_value());
            }
            at = None;
            for attr in [gimli::DW_AT_abstract_origin, gimli::DW_AT_specification] {
                if let Some(value) = die.attr_value(attr) {
                    at = self.follow(u, value);
                    break;
                }
            }
        }
        if name.is_none() {
            // No linkage name anywhere: fall back to the plain one.
            let mut at = Some((unit, offset));
            while let Some((u, o)) = at {
                let die = self.units[u].entry(o)?;
                if let Some(value) = die.attr_value(gimli::DW_AT_name) {
                    name = self.string(u, value);
                    break;
                }
                at = match die.attr_value(gimli::DW_AT_abstract_origin) {
                    Some(value) => self.follow(u, value),
                    None => None,
                };
            }
        }
        let func = Func {
            name,
            line: line.unwrap_or(0),
        };
        self.funcs.insert(key, func.clone());
        Ok(func)
    }

    fn ranges(&self, unit: usize, die: &Die<'a>) -> Result<Vec<(u64, u64)>> {
        let mut out = Vec::new();
        let mut ranges = self.dwarf.die_ranges(&self.units[unit], die)?;
        while let Some(range) = ranges.next()? {
            if range.end > range.begin {
                out.push((range.begin, range.end));
            }
        }
        Ok(out)
    }

    /// Inlined calls directly under `offset`, looking through lexical blocks.
    fn inlined(&mut self, unit: usize, offset: UnitOffset) -> Result<Vec<Inlined>> {
        let mut found = Vec::new();
        let children: Vec<(UnitOffset, gimli::DwTag)> = {
            let mut tree = self.units[unit].entries_tree(Some(offset))?;
            let mut iter = tree.root()?.children();
            let mut list = Vec::new();
            while let Some(child) = iter.next()? {
                list.push((child.entry().offset(), child.entry().tag()));
            }
            list
        };
        for (child, tag) in children {
            if tag == gimli::DW_TAG_inlined_subroutine {
                let (ranges, call_line, call_discriminator) = {
                    let die = self.units[unit].entry(child)?;
                    let call_line = die
                        .attr_value(gimli::DW_AT_call_line)
                        .and_then(|v| v.udata_value())
                        .unwrap_or(0);
                    let disc = die
                        .attr_value(gimli::DW_AT_GNU_discriminator)
                        .and_then(|v| v.udata_value())
                        .map_or(0, base_discriminator);
                    (self.ranges(unit, &die)?, call_line, disc)
                };
                found.push(Inlined {
                    ranges,
                    func: self.resolve(unit, child)?,
                    call_line,
                    call_discriminator,
                    children: self.inlined(unit, child)?,
                });
            } else if tag == gimli::DW_TAG_lexical_block {
                found.extend(self.inlined(unit, child)?);
            }
        }
        Ok(found)
    }

    /// Every out-of-line function with code, through namespaces and types.
    fn subprograms(
        &mut self,
        unit: usize,
        offset: Option<UnitOffset>,
        out: &mut Vec<Subprogram>,
    ) -> Result<()> {
        let children: Vec<(UnitOffset, gimli::DwTag)> = {
            let mut tree = self.units[unit].entries_tree(offset)?;
            let mut iter = tree.root()?.children();
            let mut list = Vec::new();
            while let Some(child) = iter.next()? {
                list.push((child.entry().offset(), child.entry().tag()));
            }
            list
        };
        for (child, tag) in children {
            match tag {
                gimli::DW_TAG_subprogram => {
                    let ranges = {
                        let die = self.units[unit].entry(child)?;
                        self.ranges(unit, &die)?
                    };
                    if !ranges.is_empty() {
                        out.push(Subprogram {
                            ranges,
                            func: self.resolve(unit, child)?,
                            children: self.inlined(unit, child)?,
                        });
                    }
                }
                gimli::DW_TAG_namespace
                | gimli::DW_TAG_structure_type
                | gimli::DW_TAG_enumeration_type
                | gimli::DW_TAG_union_type => self.subprograms(unit, Some(child), out)?,
                _ => {}
            }
        }
        Ok(())
    }
}

/// Line-table rows as (address, line, base discriminator, end of sequence),
/// sorted so the last row at or below a PC is the one that covers it.
fn line_rows(units: &[Unit<'_>]) -> Result<Vec<(u64, u64, u32, bool)>> {
    let mut rows = Vec::new();
    for unit in units {
        let Some(program) = unit.line_program.clone() else {
            continue;
        };
        let mut iter = program.rows();
        while let Some((_, row)) = iter.next_row()? {
            let end = row.end_sequence();
            let line = if end {
                0
            } else {
                row.line().map_or(0, |line| line.get())
            };
            rows.push((
                row.address(),
                line,
                base_discriminator(row.discriminator()),
                end,
            ));
        }
    }
    rows.sort_by_key(|row| (row.0, row.3));
    Ok(rows)
}

fn contains(ranges: &[(u64, u64)], pc: u64) -> bool {
    ranges.iter().any(|&(lo, hi)| lo <= pc && pc < hi)
}

/// The DWARF sections of an ELF, decompressed where needed, and its byte order.
fn dwarf_sections<'d>(
    object: &object::File<'d>,
) -> Result<(gimli::DwarfSections<Cow<'d, [u8]>>, RunTimeEndian)> {
    let endian = if object.is_little_endian() {
        RunTimeEndian::Little
    } else {
        RunTimeEndian::Big
    };
    let load = |id: gimli::SectionId| -> std::result::Result<Cow<'d, [u8]>, object::Error> {
        match object.section_by_name(id.name()) {
            Some(section) => section.uncompressed_data(),
            None => Ok(Cow::Borrowed(&[])),
        }
    };
    Ok((gimli::DwarfSections::load(load)?, endian))
}

fn dwarf_units<'a>(dwarf: &Dwarf<'a>) -> Result<Vec<Unit<'a>>> {
    let mut units = Vec::new();
    let mut headers = dwarf.units();
    while let Some(header) = headers.next()? {
        units.push(dwarf.unit(header)?);
    }
    Ok(units)
}

fn run(elf_path: &str, pc_path: &str, out_path: &str) -> Result<()> {
    let data = fs::read(elf_path)?;
    let object = object::File::parse(&*data)?;
    let (sections, endian) = dwarf_sections(&object)?;
    let dwarf = sections.borrow(|section| EndianSlice::new(section, endian));
    let units = dwarf_units(&dwarf)?;
    let rows = line_rows(&units)?;
    let row_addresses: Vec<u64> = rows.iter().map(|row| row.0).collect();

    let mut symbols = Symbols {
        dwarf: &dwarf,
        units,
        funcs: HashMap::new(),
    };
    let mut tops = Vec::new();
    for unit in 0..symbols.units.len() {
        symbols.subprograms(unit, None, &mut tops)?;
    }
    let mut flat: Vec<(u64, u64, usize)> = tops
        .iter()
        .enumerate()
        .flat_map(|(index, top)| top.ranges.iter().map(move |&(lo, hi)| (lo, hi, index)))
        .collect();
    flat.sort_unstable();
    let flat_lo: Vec<u64> = flat.iter().map(|entry| entry.0).collect();

    // Functions in first-seen order, so equal totals print in a stable order.
    let mut order: Vec<String> = Vec::new();
    let mut profile: HashMap<String, Node> = HashMap::new();
    let (mut mapped, mut unmapped) = (0u64, 0u64);
    for record in fs::read_to_string(pc_path)?.lines().skip(1) {
        let mut fields = record.split(',');
        let (Some(pc), Some(count)) = (fields.next(), fields.next()) else {
            continue;
        };
        let Ok(pc) = u64::from_str_radix(pc.trim().trim_start_matches("0x"), 16) else {
            continue;
        };
        let Ok(count) = count.trim().parse::<u64>() else {
            continue;
        };

        let top = match flat_lo.partition_point(|&lo| lo <= pc).checked_sub(1) {
            Some(index) if pc < flat[index].1 => Some(&tops[flat[index].2]),
            _ => None,
        };
        let row = match row_addresses
            .partition_point(|&address| address <= pc)
            .checked_sub(1)
        {
            Some(index) if !rows[index].3 => Some(rows[index]),
            _ => None,
        };
        let (Some(top), Some((_, line, discriminator, _))) = (top, row) else {
            unmapped += count;
            continue;
        };
        let Some(name) = top.func.name.as_ref().filter(|_| line != 0) else {
            unmapped += count;
            continue;
        };
        mapped += count;
        if !profile.contains_key(name) {
            order.push(name.clone());
        }
        let mut node = profile.entry(name.clone()).or_default();
        if top.ranges.iter().any(|&(lo, _)| lo == pc) {
            node.head = node.head.max(count);
        }
        let mut current = &top.func;
        let mut children = &top.children;
        while let Some(hit) = children.iter().find(|child| contains(&child.ranges, pc)) {
            let Some(callee) = hit.func.name.as_ref() else {
                break;
            };
            let offset = (hit.call_line.wrapping_sub(current.line) & 0xFFFF) as u32;
            node = node
                .calls
                .entry((offset, hit.call_discriminator, callee.clone()))
                .or_default();
            current = &hit.func;
            children = &hit.children;
        }
        let offset = (line.wrapping_sub(current.line) & 0xFFFF) as u32;
        let slot = node.body.entry((offset, discriminator)).or_default();
        *slot = (*slot).max(count);
    }

    order.sort_by_key(|name| std::cmp::Reverse(profile[name].total()));
    let mut out = String::new();
    for name in &order {
        let node = &profile[name];
        let _ = writeln!(out, "{name}:{}:{}", node.total(), node.head);
        node.emit(1, &mut out);
    }
    fs::write(out_path, out)?;
    println!(
        "functions {}  mapped samples {mapped}  unmapped {unmapped}",
        order.len()
    );
    Ok(())
}

/// The crate name of the v0 crate root (`C [s<base-62>_] <len> [_] <name>`)
/// at `at`, and the offset just past it.
fn crate_root(symbol: &str, at: usize) -> Option<(&str, usize)> {
    let bytes = symbol.as_bytes();
    let mut at = at;
    if bytes.get(at) != Some(&b'C') {
        return None;
    }
    at += 1;
    if bytes.get(at) == Some(&b's') {
        at += 1 + bytes[at + 1..].iter().position(|&b| b == b'_')? + 1;
    }
    let digits = bytes[at..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    let len: usize = symbol.get(at..at + digits)?.parse().ok()?;
    at += digits;
    if bytes.get(at) == Some(&b'_') {
        at += 1;
    }
    Some((symbol.get(at..at + len)?, at + len))
}

/// The crate a v0 symbol's generic instance was instantiated in, if it
/// names one. rustc-demangle parses it but never prints it.
fn instantiating_crate(symbol: &str, printed: &str) -> Option<String> {
    // The instantiating crate is the trailing path after the item's own. It
    // starts where a shorter prefix of the symbol still demangles, alone, to
    // the same text; it is short, so only the last few bytes are tried.
    let end = symbol.find(['.', '$']).unwrap_or(symbol.len());
    let start = (end.saturating_sub(64)..end).rev().find(|&cut| {
        symbol.is_char_boundary(cut)
            && rustc_demangle::try_demangle(&symbol[..cut])
                .is_ok_and(|shorter| format!("{shorter:#}") == printed)
    })?;
    let tail = &symbol[start..end];
    if let Some(rest) = tail.strip_prefix('B') {
        // A back-reference to a crate root earlier in the symbol, as a
        // base-62 byte offset from just after `_R`.
        let digits = rest.strip_suffix('_')?;
        let offset = if digits.is_empty() {
            0
        } else {
            digits.bytes().try_fold(0u64, |value, b| {
                let digit = match b {
                    b'0'..=b'9' => b - b'0',
                    b'a'..=b'z' => b - b'a' + 10,
                    b'A'..=b'Z' => b - b'A' + 36,
                    _ => return None,
                };
                value.checked_mul(62)?.checked_add(u64::from(digit))
            })? + 1
        };
        let at = 2 + usize::try_from(offset).ok()?;
        return crate_root(symbol, at).map(|(name, _)| name.to_string());
    }
    match crate_root(tail, 0) {
        Some((name, used)) if used == tail.len() => Some(name.to_string()),
        _ => None,
    }
}

/// A name for `symbol` that does not depend on where the guest was built.
///
/// The demangled path without crate disambiguators (`{:#}`) names the same
/// item in any checkout. v0 mangling keeps one copy of a generic instance per
/// crate that instantiates it, and the demangled path drops that crate, so it
/// is appended after `@`. Non-Rust names (`main`, `memcpy`) are returned
/// unchanged, and so are names already in this form.
fn portable_name(symbol: &str) -> Cow<'_, str> {
    let Ok(demangled) = rustc_demangle::try_demangle(symbol) else {
        return Cow::Borrowed(symbol);
    };
    let printed = format!("{demangled:#}");
    match instantiating_crate(symbol, &printed) {
        Some(krate) => Cow::Owned(format!("{printed} @{krate}")),
        None => Cow::Owned(printed),
    }
}

/// Rewrite every function name in a text profile written by this tool.
///
/// Headers are `name:total:head`, inlined call sites
/// `offset[.discriminator]: name:total`, and body lines
/// `offset[.discriminator]: count`. Portable names may contain `:` and
/// spaces, so the numbers are split off from the right.
fn rename_profile(text: &str, mut rename: impl FnMut(&str) -> Result<String>) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    let mut headers = HashMap::new();
    for line in text.lines() {
        let indent = line.len() - line.trim_start_matches(' ').len();
        if indent == 0 {
            let mut fields = line.rsplitn(3, ':');
            let (Some(head), Some(total), Some(name)) =
                (fields.next(), fields.next(), fields.next())
            else {
                return Err(format!("not a profile header: {line}").into());
            };
            let renamed = rename(name)?;
            if let Some(previous) = headers.insert(renamed.clone(), name.to_string()) {
                return Err(format!("{previous} and {name} both become {renamed}").into());
            }
            let _ = writeln!(out, "{renamed}:{total}:{head}");
            continue;
        }
        let Some((location, rest)) = line.split_once(": ") else {
            return Err(format!("not a profile line: {line}").into());
        };
        match rest.parse::<u64>() {
            Ok(_) => out.push_str(line),
            Err(_) => {
                let Some((callee, total)) = rest.rsplit_once(':') else {
                    return Err(format!("not a profile call site: {line}").into());
                };
                let _ = write!(out, "{location}: {}:{total}", rename(callee)?);
            }
        }
        out.push('\n');
    }
    Ok(out)
}

fn portable(in_path: &str, out_path: &str) -> Result<()> {
    let text = fs::read_to_string(in_path)?;
    fs::write(
        out_path,
        rename_profile(&text, |name| Ok(portable_name(name).into_owned()))?,
    )?;
    Ok(())
}

/// Map a profile's names, portable or from another checkout, onto the
/// symbols of `elf_path`, which must carry DWARF for its inlined functions.
fn rebind(in_path: &str, elf_path: &str, out_path: &str) -> Result<()> {
    let data = fs::read(elf_path)?;
    let object = object::File::parse(&*data)?;
    let (sections, endian) = dwarf_sections(&object)?;
    let dwarf = sections.borrow(|section| EndianSlice::new(section, endian));
    let units = dwarf_units(&dwarf)?;
    if units.is_empty() {
        return Err(format!("{elf_path} has no DWARF; build it with -Cdebuginfo=1").into());
    }

    // Portable name -> this build's symbol, or None when two symbols share
    // one portable name and the profile cannot say which it meant.
    let mut targets: HashMap<String, Option<String>> = HashMap::new();
    let mut add = |symbol: String| {
        let key = portable_name(&symbol).into_owned();
        if key == symbol {
            return;
        }
        match targets.get(&key) {
            None => {
                targets.insert(key, Some(symbol));
            }
            Some(Some(existing)) if *existing != symbol => {
                targets.insert(key, None);
            }
            Some(_) => {}
        }
    };
    for symbol in object.symbols() {
        if let Ok(name) = symbol.name() {
            add(name.to_string());
        }
    }
    for unit in &units {
        let mut entries = unit.entries();
        while let Some(entry) = entries.next_dfs()? {
            for attr in [gimli::DW_AT_linkage_name, gimli::DW_AT_MIPS_linkage_name] {
                if let Some(value) = entry.attr_value(attr) {
                    add(dwarf
                        .attr_string(unit, value)?
                        .to_string_lossy()
                        .into_owned());
                }
            }
        }
    }

    let (mut kept, mut bound, mut missing, mut ambiguous) = (0, 0, 0, 0);
    let text = fs::read_to_string(in_path)?;
    let rebound = rename_profile(&text, |name| {
        // A Rust name either demangles or is already portable, and every
        // Rust item path has a `::`; C names such as `main` have neither.
        let key = portable_name(name);
        if key == name && !name.contains("::") {
            kept += 1;
            return Ok(name.to_string());
        }
        Ok(match targets.get(key.as_ref()) {
            Some(Some(symbol)) => {
                bound += 1;
                symbol.clone()
            }
            Some(None) => {
                ambiguous += 1;
                name.to_string()
            }
            None => {
                missing += 1;
                name.to_string()
            }
        })
    })?;
    println!("rebound {bound}  missing {missing}  ambiguous {ambiguous}  non-Rust kept {kept}");
    if bound == 0 && missing + ambiguous > 0 {
        return Err(format!("no profile name matched a symbol in {elf_path}").into());
    }
    fs::write(out_path, rebound)?;
    Ok(())
}

const USAGE: &str = "usage: psoxide-pgo <elf-with-dwarf> <pc.csv> <out.prof>
       psoxide-pgo portable <in.prof> <out.prof>
       psoxide-pgo rebind <in.prof> <target-elf-with-dwarf> <out.prof>";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["portable", input, output] => portable(input, output),
        ["rebind", input, elf, output] => rebind(input, elf, output),
        [elf, pc, output] if !["portable", "rebind"].contains(elf) => run(elf, pc, output),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("psoxide-pgo: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_discriminator_decodes_the_prefix_encoding() {
        assert_eq!(base_discriminator(0), 0);
        assert_eq!(base_discriminator(1), 0); // odd: no base component
        assert_eq!(base_discriminator(3 << 1), 3);
        // Bases above 0x1f: low five bits, a flag, then the rest.
        let encoded = ((0x25 & 0xFE0) << 1) | (0x25 & 0x1F) | 0x20;
        assert_eq!(base_discriminator(encoded << 1), 0x25);
    }

    // Symbols from the same guest built in two checkouts: only the crate
    // disambiguators differ.
    const STEP_A: &str = "_RNvCs17CBFHtzUmz_9hello_gte11step_entity";
    const STEP_B: &str = "_RNvCskfeMIapBdWl_9hello_gte11step_entity";
    const NEXT_A: &str = "_RNvXs4_NtNtCs9xyZLwYLJOW_4core4iter5rangeINtNtNtB9_3ops5range5RangejENtNtNtB7_6traits8iterator8Iterator4nextCs17CBFHtzUmz_9hello_gte";
    const NEXT_B: &str = "_RNvXs4_NtNtCs9xyZLwYLJOW_4core4iter5rangeINtNtNtB9_3ops5range5RangejENtNtNtB7_6traits8iterator8Iterator4nextCskfeMIapBdWl_9hello_gte";
    const NEXT: &str =
        "<core::ops::range::Range<usize> as core::iter::traits::iterator::Iterator>::next";

    #[test]
    fn portable_names_drop_crate_disambiguators() {
        assert_eq!(portable_name(STEP_A), "hello_gte::step_entity");
        assert_eq!(portable_name(STEP_A), portable_name(STEP_B));
        assert_eq!(portable_name("main"), "main");
        // Already portable: unchanged.
        assert_eq!(
            portable_name("hello_gte::step_entity"),
            "hello_gte::step_entity"
        );
    }

    #[test]
    fn portable_names_keep_the_instantiating_crate() {
        assert_eq!(portable_name(NEXT_A), format!("{NEXT} @hello_gte"));
        assert_eq!(portable_name(NEXT_A), portable_name(NEXT_B));
        // The same instance copied into another crate stays distinct.
        let in_gpu = NEXT_A.replace("Cs17CBFHtzUmz_9hello_gte", "Cs17CBFHtzUmz_7psx_gpu");
        assert_eq!(portable_name(&in_gpu), format!("{NEXT} @psx_gpu"));
        // Instantiating crate given as a back-reference (`B3_`).
        assert_eq!(
            portable_name("_RNCNvCsb9SZb48y33G_6hl_psx25classic_native_patch_masks0_0B3_"),
            "hl_psx::classic_native_patch_mask::{closure#2} @hl_psx"
        );
    }

    #[test]
    fn profiles_rename_through_portable_names_and_back() {
        let profile = format!("{STEP_A}:10:1\n 6.1: {NEXT_A}:7\n  1: 7\n 7: 3\nmain:1:0\n 1: 1\n");
        let portable = rename_profile(&profile, |name| Ok(portable_name(name).into_owned()))
            .expect("portable");
        assert_eq!(
            portable,
            format!(
                "hello_gte::step_entity:10:1\n 6.1: {NEXT} @hello_gte:7\n  1: 7\n 7: 3\nmain:1:0\n 1: 1\n"
            )
        );
        let targets: HashMap<String, &str> = [STEP_B, NEXT_B]
            .into_iter()
            .map(|symbol| (portable_name(symbol).into_owned(), symbol))
            .collect();
        let rebound = rename_profile(&portable, |name| {
            Ok(targets.get(name).map_or(name, |symbol| symbol).to_string())
        })
        .expect("rebind");
        assert_eq!(
            rebound,
            profile.replace(STEP_A, STEP_B).replace(NEXT_A, NEXT_B)
        );
    }

    #[test]
    fn two_records_with_one_portable_name_are_refused() {
        let profile = format!("{STEP_A}:1:0\n 1: 1\n{STEP_B}:1:0\n 1: 1\n");
        assert!(rename_profile(&profile, |name| Ok(portable_name(name).into_owned())).is_err());
    }

    #[test]
    fn inlined_callsites_nest_one_space_per_level() {
        let mut top = Node::default();
        top.body.insert((2, 0), 10);
        top.calls
            .entry((5, 1, "callee".into()))
            .or_default()
            .body
            .insert((1, 0), 7);
        let mut out = String::new();
        top.emit(1, &mut out);
        assert_eq!(top.total(), 17);
        assert_eq!(out, " 2: 10\n 5.1: callee:7\n  1: 7\n");
    }
}
