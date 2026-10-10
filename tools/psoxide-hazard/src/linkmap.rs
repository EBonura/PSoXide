//! Function bounds and the trampoline array from ld.lld's `-Map` output.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use crate::detect::MAGIC;
use crate::listing::{load_address, word_at_offset, HEADER};
use crate::text::strip;

/// Bytes the kernel reads at a time: the header's payload size is a multiple.
const SECTOR: i64 = 0x800;

/// A map that cannot be read or does not describe the image.
#[derive(Debug)]
pub struct MapError(pub String);

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The message an unreadable file gives, in the form the tools have always
/// printed (`[Errno 2] No such file or directory: 'game.map'`).
pub fn io_message(error: &std::io::Error, path: &Path) -> String {
    let path = path.display();
    match error.raw_os_error() {
        Some(code) => {
            let text = error.to_string();
            let text = text.split(" (os error").next().unwrap_or(&text).to_string();
            format!("[Errno {code}] {text}: '{path}'")
        }
        None => format!("{error}: '{path}'"),
    }
}

/// Function bounds and the trampoline array from ld.lld's `-Map` output
/// for the link that made an image (psoxide.ld's layout). Symbols sit 16
/// columns in, input sections 8.
#[derive(Debug)]
pub struct LinkMap {
    path: String,
    /// `(__text_start, __text_end)`.
    pub text: (i64, i64),
    /// Where the data ends and `.bss` begins: `__data_end`, or `__bss_start`
    /// for a map without it. The header's payload size is this rounded up to
    /// whole sectors.
    data_end: Option<i64>,
    /// `HAZARD_TRAMPOLINES` as `(start, end)`.
    pub trampolines: Option<(i64, i64)>,
    /// Symbols inside .text by address, in map order: `(size, name)`.
    pub names: HashMap<i64, Vec<(i64, String)>>,
    bounds: Vec<i64>,
    starts: Vec<i64>,
    rodata: Vec<(i64, i64)>,
    extra_code: Vec<(i64, i64)>,
    entry: Option<i64>,
    /// `(table section address, size, (function start, end))` for every
    /// `.rodata.<function>` section whose function the map names.
    tables: Vec<(i64, i64, (i64, i64))>,
}

/// Python's `str.splitlines()`: every Unicode line boundary.
fn split_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let boundary = matches!(
            c,
            '\n' | '\r'
                | '\u{0b}'
                | '\u{0c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if boundary {
            lines.push(&text[start..i]);
            let mut next = i + c.len_utf8();
            if c == '\r' {
                if let Some(&(j, '\n')) = chars.peek() {
                    chars.next();
                    next = j + 1;
                }
            }
            start = next;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// `^([0-9a-f]+) +([0-9a-f]+) +([0-9a-f]+) +(\d+) (.*)$`: address, size and
/// the rest of a map row.
fn map_row(line: &str) -> Option<(i64, i64, &str)> {
    fn hex(s: &str) -> Option<(&str, &str)> {
        let n = s
            .bytes()
            .take_while(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
            .count();
        (n > 0).then(|| s.split_at(n))
    }
    fn spaces(s: &str) -> Option<&str> {
        let n = s.bytes().take_while(|&b| b == b' ').count();
        (n > 0).then(|| &s[n..])
    }
    let (vma, rest) = hex(line)?;
    let (_, rest) = hex(spaces(rest)?)?;
    let (size, rest) = hex(spaces(rest)?)?;
    let rest = spaces(rest)?;
    // `(\d+) (.*)`: the backtracking regex lets the digits stop before a
    // later space, but `\d+` cannot contain one, so the first space after
    // the digits ends them.
    let digits = rest
        .chars()
        .take_while(|c| c.is_numeric())
        .map(char::len_utf8)
        .sum::<usize>();
    if digits == 0 || !rest[digits..].starts_with(' ') {
        return None;
    }
    let tail = &rest[digits + 1..];
    if tail.contains('\n') {
        return None;
    }
    Some((
        i64::from_str_radix(vma, 16).ok()?,
        i64::from_str_radix(size, 16).ok()?,
        tail,
    ))
}

impl LinkMap {
    /// Read and index the map at `path`.
    pub fn open(path: &Path) -> Result<Self, MapError> {
        let bytes = std::fs::read(path).map_err(|e| MapError(io_message(&e, path)))?;
        let text = String::from_utf8_lossy(&bytes);
        Self::parse(&text, &path.display().to_string())
    }

    /// Read a map with code modules declared by the caller. Only symbols in
    /// those exact link ranges join the resident function index.
    pub fn open_with_code(path: &Path, code: &[(i64, i64)]) -> Result<Self, MapError> {
        let bytes = std::fs::read(path).map_err(|e| MapError(io_message(&e, path)))?;
        let text = String::from_utf8_lossy(&bytes);
        Self::parse_with_code(&text, &path.display().to_string(), code, true)
    }

    /// Index a map's text; `path` names it in messages.
    pub fn parse(text: &str, path: &str) -> Result<Self, MapError> {
        Self::parse_with_code(text, path, &[], false)
    }

    fn parse_with_code(
        text: &str,
        path: &str,
        extra_code: &[(i64, i64)],
        guard_roots: bool,
    ) -> Result<Self, MapError> {
        let mut symbols = Vec::new();
        let mut sections = Vec::new();
        let mut rodata = Vec::new();
        let mut text_marks: HashMap<&str, i64> = HashMap::new();
        let mut trampolines = None;
        let mut functions: HashMap<String, (i64, i64)> = HashMap::new();
        let mut tables = Vec::new();
        for line in split_lines(text) {
            let Some((address, size, rest)) = map_row(line) else {
                continue;
            };
            let depth = rest.len() - rest.trim_start_matches(' ').len();
            let name = strip(rest);
            if matches!(
                name,
                "__text_start = ." | "__text_end = ." | "__data_end = ." | "__bss_start = ."
            ) {
                let key = name.split_whitespace().next().unwrap_or(name);
                let key = match key {
                    "__text_start" => "__text_start",
                    "__text_end" => "__text_end",
                    "__data_end" => "__data_end",
                    _ => "__bss_start",
                };
                text_marks.insert(key, address);
            } else if depth == 16 && name == "HAZARD_TRAMPOLINES" {
                trampolines = Some((address, address + size));
            } else if depth == 8 && name.ends_with(')') && name.contains(":(.text") {
                sections.push((address, size));
                if let Some(at) = name.find(":(.text.") {
                    functions.insert(
                        name[at + 8..name.len() - 1].to_string(),
                        (address, address + size),
                    );
                }
            } else if depth == 8 && name.ends_with(')') && name.contains(":(.rodata") && size != 0 {
                rodata.push((address, address + size));
                if let Some(at) = name.find(":(.rodata.") {
                    tables.push((address, size, name[at + 10..name.len() - 1].to_string()));
                }
            } else if depth == 16 && !name.starts_with(".L") && !name.contains(" = ") {
                symbols.push((address, size, name.to_string()));
            }
        }
        let (Some(&lo), Some(&hi)) = (text_marks.get("__text_start"), text_marks.get("__text_end"))
        else {
            return Err(MapError(format!(
                "{path}: no __text_start/__text_end, not an ld.lld map of psoxide.ld"
            )));
        };
        let mut names: HashMap<i64, Vec<(i64, String)>> = HashMap::new();
        let mut name_order = Vec::new();
        for (address, size, name) in symbols {
            if guard_roots
                && name.starts_with("<psx_rt::scratchpad::ScratchpadStack<")
                && !(lo <= address && address < hi)
                && !extra_code
                    .iter()
                    .any(|&(start, end)| start <= address && address < end)
            {
                return Err(MapError(format!(
                    "{path}: scratchpad stack entry {name} at {address:08x} is outside declared module code"
                )));
            }
            if (lo <= address && address < hi)
                || extra_code
                    .iter()
                    .any(|&(start, end)| start <= address && address < end)
            {
                names
                    .entry(address)
                    .or_insert_with(|| {
                        name_order.push(address);
                        Vec::new()
                    })
                    .push((size, name));
            }
        }
        let sections: Vec<(i64, i64)> = sections
            .into_iter()
            .filter(|s| {
                (lo <= s.0 && s.0 < hi)
                    || extra_code
                        .iter()
                        .any(|&(start, end)| start <= s.0 && s.0 < end)
            })
            .collect();
        // Every symbol or section start ends the function before it.
        let mut bounds: Vec<i64> = names.keys().copied().collect();
        bounds.extend(sections.iter().map(|s| s.0));
        bounds.extend(sections.iter().map(|s| s.0 + s.1));
        bounds.push(hi);
        bounds.extend(extra_code.iter().flat_map(|&(start, end)| [start, end]));
        bounds.sort_unstable();
        bounds.dedup();
        let mut starts: Vec<i64> = names.keys().copied().collect();
        starts.sort_unstable();
        rodata.sort_unstable();
        let entry = name_order
            .iter()
            .copied()
            .find(|a| names[a].iter().any(|(_, n)| n == "_start"));
        // LLVM writes a function's jump tables to `.rodata.<its section>`.
        let tables = tables
            .into_iter()
            .filter_map(|(a, size, key)| functions.get(&key).map(|&f| (a, size, f)))
            .collect();
        Ok(Self {
            path: path.to_string(),
            text: (lo, hi),
            data_end: text_marks
                .get("__data_end")
                .or_else(|| text_marks.get("__bss_start"))
                .copied(),
            trampolines,
            names,
            bounds,
            starts,
            rodata,
            extra_code: extra_code.to_vec(),
            entry,
            tables,
        })
    }

    /// Symbol addresses inside .text, sorted.
    pub fn sorted_names(&self) -> Vec<(i64, &[(i64, String)])> {
        let mut out: Vec<_> = self.names.iter().map(|(a, v)| (*a, v.as_slice())).collect();
        out.sort_by_key(|e| e.0);
        out
    }

    /// Refuse a map from another link (a stale one, or another example's).
    /// The header's payload size (__data_end - __text_start, rounded up to
    /// whole 2 KiB sectors) is coarse, so a stale map of a relinked game can
    /// agree with it: one
    /// that put HAZARD_TRAMPOLINES 0x78 bytes early passed and cut 11 of
    /// Quake's jump tables short (2026-09-23). So also probe words every
    /// psoxide.ld guest has at an address only the right map knows: the
    /// entry point, psx-rt's trampoline magic and capacity, every `jal`
    /// into .text landing on a function the map names, and every word of a
    /// function's own `.rodata.<function>` section (its jump tables)
    /// pointing into that function, or into the trampoline array once
    /// patched. Each probe is exact and held on every game's own map it
    /// was tried on.
    pub fn check(&self, data: &[u8]) -> Result<(), MapError> {
        let base = load_address(data);
        let (lo, hi) = self.text;
        let mut problems = Vec::new();
        let word = |addr: i64| -> Option<i64> {
            let off = addr - base + HEADER;
            if HEADER <= off && off <= data.len() as i64 - 4 {
                word_at_offset(data, off).map(i64::from)
            } else {
                None
            }
        };
        if data.starts_with(b"PS-X EXE") {
            let field =
                |at: usize| i64::from(u32::from_le_bytes(data[at..at + 4].try_into().unwrap()));
            let (pc, payload) = (field(0x10), field(0x1C));
            if let Some(data_end) = self.data_end {
                let stored = (data_end - lo + SECTOR - 1) & !(SECTOR - 1);
                if payload != stored || base != lo {
                    problems.push(format!(
                        "payload {payload:#x} at {base:#x}, map says {stored:#x} at {lo:#x}"
                    ));
                }
            }
            if let Some(entry) = self.entry {
                if pc != entry {
                    problems.push(format!("entry point {pc:08x}, map's _start {entry:08x}"));
                }
            }
        }
        if let Some((t0, t1)) = self.trampolines {
            if word(t0) != Some(MAGIC) || word(t0 + 4) != Some((t1 - t0).div_euclid(4) - 2) {
                problems.push(format!("no HAZARD_TRAMPOLINES magic at the map's {t0:08x}"));
            }
        }
        let known: std::collections::HashSet<i64> = self.bounds.iter().copied().collect();
        let mut calls = Vec::new();
        let mut addr = lo;
        while addr < hi {
            if let Some(w) = word(addr) {
                if w >> 26 == 3 {
                    let target = (addr & 0xF000_0000) | (w & 0x03FF_FFFF) << 2;
                    if lo <= target && target < hi && !known.contains(&target) {
                        calls.push((addr, target));
                    }
                }
            }
            addr += 4;
        }
        if let Some(&(at, target)) = calls.first() {
            problems.push(format!(
                "{} calls to no function the map names, first jal {target:08x} at {at:08x}",
                calls.len()
            ));
        }
        let tramp = self.trampolines.unwrap_or((0, 0));
        let lands =
            |w: i64, start: i64, end: i64| (start <= w && w < end) || (tramp.0 <= w && w < tramp.1);
        let mut strays = Vec::new();
        for &(table, size, (start, end)) in &self.tables {
            let mut addr = table;
            while addr < table + size - 3 {
                if !lands(word(addr).unwrap_or(0), start, end) {
                    strays.push((addr, start));
                }
                addr += 4;
            }
        }
        if let Some(&(addr, start)) = strays.first() {
            problems.push(format!(
                "{} jump table words outside their function, first {addr:08x} holds {:08x}, not in the \
                 function at {start:08x}",
                strays.len(),
                word(addr).unwrap_or(0)
            ));
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(MapError(format!(
                "{}: map does not match this image ({}); relink so both come from one link",
                self.path,
                problems.join("; ")
            )))
        }
    }

    /// `(start, end)` of the `.rodata*` input section holding `addr`, or
    /// `None`. psoxide.ld links `.rodata.*` into its `.data` output section,
    /// next to mutable `.data.*`; only the input section name tells them
    /// apart.
    pub fn rodata_section(&self, addr: i64) -> Option<(i64, i64)> {
        let i = self.rodata.partition_point(|&s| s <= (addr, 0xFFFF_FFFF));
        let section = *self.rodata.get(i.checked_sub(1)?)?;
        (section.0 <= addr && addr < section.1).then_some(section)
    }

    /// `(start, end, name)` of the function containing `addr`. A function
    /// is a named symbol, or runs from `addr` to the next boundary.
    pub fn function(&self, addr: i64) -> Option<(i64, i64, String)> {
        let next_bound = |at: i64| self.bounds[self.bounds.partition_point(|&b| b <= at)];
        let i = self.starts.partition_point(|&s| s <= addr);
        if i > 0 {
            let start = self.starts[i - 1];
            let (size, name) = self.names[&start].iter().max().unwrap();
            let end = if *size != 0 {
                start + size
            } else {
                next_bound(start)
            };
            if addr < end {
                return Some((start, end, name.clone()));
            }
        }
        if (self.text.0 <= addr && addr < self.text.1)
            || self
                .extra_code
                .iter()
                .any(|&(lo, hi)| lo <= addr && addr < hi)
        {
            return Some((addr, next_bound(addr), format!("<unnamed {addr:08x}>")));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_and_lines() {
        assert_eq!(
            map_row("80010000 80010000      100     4         /x.o:(.text.f0)"),
            Some((0x8001_0000, 0x100, "        /x.o:(.text.f0)"))
        );
        assert_eq!(
            map_row("     VMA      LMA     Size Align Out     In      Symbol"),
            None
        );
        assert_eq!(split_lines("a\r\nb\rc\n\nd"), ["a", "b", "c", "", "d"]);
    }
}
