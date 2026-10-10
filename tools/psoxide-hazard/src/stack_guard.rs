//! `stack-guard`: prove every scratchpad stack call tree in a linked PS-EXE
//! fits its region.

use std::collections::HashMap;
use std::fmt;
use std::io::Write;
use std::path::Path;

use regex::Regex;

use crate::detect::{is, Detector, Image, READS_ALL};
use crate::linkmap::{io_message, LinkMap};
use crate::listing::{load_address, Listing, HEADER};
use crate::text::{int_auto, sp_adjust, squeeze, strip, trailing_hex};

/// Usage text.
pub const USAGE: &str = "\
Prove every scratchpad stack call tree in a linked PS-EXE fits its region.

`psx_rt::scratchpad::ScratchpadStack::<START, END>::run(f)` runs `f` with $sp
in scratchpad bytes START..END. Nothing at run time can stop a deep call tree
from running off the bottom of the region, and an inlining change can grow a
tree by hundreds of bytes without any source change. This walks the linked
image's static call graph from every monomorphised entry,

    <psx_rt::scratchpad::ScratchpadStack<START, END>>::stack_entry::<R, F>

sums frame sizes (every `addiu sp,sp,-N`; a tail call counts as a call) down
the deepest path, and fails when the total exceeds the region minus psx-rt's
20-byte overhead. It also fails on what it cannot bound: recursion, calls
through a register, register jumps it cannot prove are a switch, and $sp
adjusted any other way.

    stack-guard game.exe game.map
    stack-guard game.exe game.map --root REGEX --budget BYTES
    stack-guard game.exe game.map --forbid __psx_rt_flush_i_cache

The map is ld.lld's `-Map` output for the same link. The exe may be
hazard-patched: calls rerouted through HAZARD_TRAMPOLINES are followed to
their targets. `--root`/`--budget` check a game's own stack switch instead
(an entry name regex and its byte budget). `--forbid SYMBOL` (repeatable)
fails a tree that reaches the function named SYMBOL: psx-rt's I-cache flush,
`__psx_rt_flush_i_cache`, runs with the scratchpad unmapped, so a frame on the
scratchpad stack would read the cache tags instead. A forbidden symbol that
is not linked is reported but does not fail. Without a map, `--forbid` fails
because symbol reachability cannot be proved; otherwise the tool only checks
that the image does not contain psx-rt's stack switch.
";

const STACK_OVERHEAD: i64 = 20;
const SWITCH: &str = "__psx_rt_call_on_stack";
const LEAVES: [&str; 3] = [SWITCH, "rust_begin_unwind", "__rustc::rust_begin_unwind"];
const COND: &[&str] = &[
    "beq", "bne", "beqz", "bnez", "blez", "bgtz", "bltz", "bgez", "b", "bltzal", "bgezal", "bal",
];

/// A tree the guard cannot bound, or an image it cannot read.
#[derive(Debug)]
pub struct GuardError(pub String);

impl fmt::Display for GuardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A linked image and its map.
pub struct GuardImage {
    /// The whole file.
    pub data: Vec<u8>,
    /// The load address.
    pub base: i64,
    /// One past the last loaded byte.
    pub image_end: i64,
    /// Its listing.
    pub listing: Listing,
    /// Its link map.
    pub map: LinkMap,
}

impl GuardImage {
    /// Read `exe` and its `map_path`, refusing a map from another link.
    pub fn open(exe: &Path, map_path: &Path) -> Result<Self, GuardError> {
        let data = std::fs::read(exe).map_err(|e| GuardError(io_message(&e, exe)))?;
        let base = load_address(&data);
        let image_end = base + data.len() as i64 - HEADER;
        let listing = Listing::new(&data, base);
        let map = LinkMap::open(map_path).map_err(|e| GuardError(e.to_string()))?;
        map.check(&data).map_err(|e| GuardError(e.to_string()))?;
        Ok(Self {
            data,
            base,
            image_end,
            listing,
            map,
        })
    }

    /// The image as the detector sees it.
    pub fn image(&self) -> Image<'_> {
        Image {
            listing: &self.listing,
            data: &self.data,
            base: self.base,
            image_end: self.image_end,
        }
    }

    /// The word loaded at `addr`.
    pub fn word_at(&self, addr: i64) -> i64 {
        self.image().word_at(addr)
    }

    fn in_trampolines(&self, addr: i64) -> bool {
        self.map
            .trampolines
            .is_some_and(|(t0, t1)| t0 <= addr && addr < t1)
    }
}

/// True when the instruction's destination is $sp (stores, branches and
/// coprocessor moves only read their first operand).
fn writes_sp(op: &str, args: &str) -> bool {
    strip(args.split(',').next().unwrap_or("")) == "sp" && !is(op, READS_ALL)
}

struct Walker<'a> {
    image: &'a GuardImage,
    detector: Detector<'a>,
    memo: HashMap<i64, (i64, Vec<String>)>,
    /// Functions no tree may reach (`--forbid`).
    forbidden: &'a [String],
}

impl<'a> Walker<'a> {
    fn new(image: &'a GuardImage, forbidden: &'a [String]) -> Self {
        Self {
            image,
            detector: Detector::new(image.image(), Some(&image.map)),
            memo: HashMap::new(),
            forbidden,
        }
    }

    /// Call targets outside `[start, end)` for one instruction.
    fn transfers(
        &self,
        start: i64,
        end: i64,
        addr: i64,
        op: &str,
        args: &str,
        name: &str,
    ) -> Result<Vec<i64>, GuardError> {
        if op == "jal" || op == "j" || is(op, COND) {
            let Some(dest) = trailing_hex(args) else {
                return Ok(Vec::new());
            };
            if start <= dest && dest < end {
                return Ok(Vec::new());
            }
            if self.image.in_trampolines(dest) {
                return self.trampoline(start, end, dest, name);
            }
            return Ok(vec![dest]);
        }
        if op == "jalr" {
            return Err(GuardError(format!(
                "{name} calls through a register at {addr:08x}; the callee cannot be bounded"
            )));
        }
        if op == "jr" && strip(args) != "ra" {
            if self.detector.jump_table(addr).is_none() {
                return Err(GuardError(format!(
                    "{name} jumps through a register at {addr:08x} ({op} {args}) and it is not a jump table it can \
                     prove; a BIOS call or a tail call through a pointer cannot be bounded"
                )));
            }
            // A proven table ends at its first entry that leaves the
            // function (a switch never does), so a dispatch calls nothing.
            return Ok(Vec::new());
        }
        Ok(Vec::new())
    }

    /// Where a hazard trampoline goes. `nop ; j T ; nop` stands for a jump,
    /// call or jump-table entry; `bXX +3 ; nop ; j FALL ; nop ; j T ; nop`
    /// for a conditional branch; `LOAD ; jr/jalr rs ; ...` for a register
    /// jump whose slot load it moved.
    fn trampoline(
        &self,
        start: i64,
        end: i64,
        tramp: i64,
        name: &str,
    ) -> Result<Vec<i64>, GuardError> {
        let listing = &self.image.listing;
        let first = listing.op(tramp);
        let jumps = if is(first, COND) {
            vec![tramp + 8, tramp + 16]
        } else if first == "nop" {
            vec![tramp + 4]
        } else {
            let (op, args) = (listing.op(tramp + 4), listing.args(tramp + 4));
            if op == "jr" && strip(args) == "ra" {
                return Ok(Vec::new());
            }
            return Err(GuardError(format!(
                "{name} jumps or calls through a register via trampoline {tramp:08x} ({op} {args})"
            )));
        };
        let mut found = Vec::new();
        for addr in jumps {
            let dest = if listing.op(addr) == "j" {
                trailing_hex(listing.args(addr))
            } else {
                None
            };
            let Some(dest) = dest else {
                return Err(GuardError(format!(
                    "{name} goes through {tramp:08x}, which is not a hazard_patch.py trampoline"
                )));
            };
            if !(start <= dest && dest < end) {
                found.push(dest);
            }
        }
        Ok(found)
    }

    /// `(bytes, chain)` for the deepest path from the function at `addr`.
    fn depth(&mut self, addr: i64, path: &mut Vec<i64>) -> Result<(i64, Vec<String>), GuardError> {
        let Some((start, end, name)) = self.image.map.function(addr) else {
            return Err(GuardError(format!("call to {addr:08x}, outside .text")));
        };
        // A function can carry several symbols (an alias); any of them counts.
        let named = |forbidden: &String| {
            self.image
                .map
                .names
                .get(&start)
                .is_some_and(|names| names.iter().any(|(_, n)| n == forbidden))
        };
        if let Some(forbidden) = self.forbidden.iter().find(|f| named(f)) {
            let callers: Vec<String> = path
                .iter()
                .filter_map(|&at| self.image.map.function(at))
                .map(|(_, _, caller)| short(&caller))
                .collect();
            return Err(GuardError(format!(
                "{} reaches {forbidden}, which no scratchpad stack tree may (--forbid)",
                callers.last().map_or("the entry", String::as_str)
            )));
        }
        if let Some(known) = self.memo.get(&start) {
            return Ok(known.clone());
        }
        if path.contains(&start) {
            return Err(GuardError(format!(
                "{name} recurses, so its depth has no bound"
            )));
        }
        let listing = &self.image.listing;
        let mut frame = 0;
        let mut pc = start;
        while pc < end {
            let (op, args) = (listing.op(pc), listing.args(pc));
            let adjust = if op == "addiu" {
                sp_adjust(&squeeze(args))
            } else {
                None
            };
            if let Some(n) = adjust {
                frame += 0.max(-n);
            } else if writes_sp(op, args)
                && !(name == SWITCH || matches!(squeeze(args).as_str(), "sp,s8" | "sp,fp"))
            {
                return Err(GuardError(format!(
                    "{name} sets $sp at {pc:08x} ({op} {args}); only addiu frames can be counted"
                )));
            }
            pc += 4;
        }
        let mut deepest: (i64, Vec<String>) = (0, Vec::new());
        if !LEAVES.iter().any(|leaf| name.ends_with(leaf)) {
            let mut pc = start;
            while pc < end {
                let (op, args) = (listing.op(pc), listing.args(pc).to_string());
                for callee in self.transfers(start, end, pc, op, &args, &name)? {
                    path.push(start);
                    let below = self.depth(callee, path);
                    path.pop();
                    let below = below?;
                    if below.0 > deepest.0 {
                        deepest = below;
                    }
                }
                pc += 4;
            }
        }
        let mut chain = vec![format!("{}({frame})", short(&name))];
        chain.extend(deepest.1);
        let result = (frame + deepest.0, chain);
        self.memo.insert(start, result.clone());
        Ok(result)
    }
}

/// A symbol name shortened for a report line: no `::h<hash>` suffix, at
/// most 90 characters.
pub fn short(name: &str) -> String {
    let trimmed = match name.len().checked_sub(19) {
        Some(at)
            if name.is_char_boundary(at)
                && name[at..].starts_with("::h")
                && name[at + 3..]
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
        {
            &name[..at]
        }
        _ => name,
    };
    if trimmed.chars().count() <= 90 {
        trimmed.to_string()
    } else {
        let head: String = trimmed.chars().take(87).collect();
        format!("{head}...")
    }
}

type Root = (i64, String, i64, Option<(i64, i64)>);

fn roots(image: &GuardImage, pattern: Option<&Regex>, budget: Option<i64>) -> Vec<Root> {
    let entry = Regex::new(
        r"^<psx_rt::scratchpad::ScratchpadStack<(\d+)(?:usize)?, (\d+)(?:usize)?>>::stack_entry::<",
    )
    .expect("entry pattern");
    let mut found = Vec::new();
    for (address, entries) in image.map.sorted_names() {
        for (_, name) in entries {
            if let Some(pattern) = pattern {
                if pattern.is_match(name) {
                    found.push((address, name.clone(), budget.unwrap_or(0), None));
                }
                continue;
            }
            if let Some(m) = entry.captures(name) {
                let (Ok(lo), Ok(hi)) = (m[1].parse::<i64>(), m[2].parse::<i64>()) else {
                    continue;
                };
                found.push((
                    address,
                    name.clone(),
                    hi - lo - STACK_OVERHEAD,
                    Some((lo, hi)),
                ));
            }
        }
    }
    found
}

/// psx-rt's switch: `jalr t9` with `move sp,a2` in its delay slot.
fn has_switch(listing: &Listing) -> bool {
    listing.iter().any(|(a, e)| {
        e.op == "jalr" && strip(&e.args) == "t9" && squeeze(listing.args(a + 4)) == "sp,a2"
    })
}

/// Print one line per entry to `out`; return the number of failures.
pub fn check(
    exe: &Path,
    map_path: Option<&Path>,
    pattern: Option<&str>,
    budget: Option<i64>,
    out: &mut dyn Write,
) -> usize {
    check_forbidding(exe, map_path, pattern, budget, &[], out)
}

/// [`check`], also failing a tree that reaches any function in `forbidden`
/// (symbol names, as in the map): psx-rt's `__psx_rt_flush_i_cache` unmaps
/// the scratchpad the tree's frames are on.
pub fn check_forbidding(
    exe: &Path,
    map_path: Option<&Path>,
    pattern: Option<&str>,
    budget: Option<i64>,
    forbidden: &[String],
    out: &mut dyn Write,
) -> usize {
    let Some(map_path) = map_path else {
        if !forbidden.is_empty() {
            let _ = writeln!(
                out,
                "stack guard: --forbid needs a link map to prove symbol reachability"
            );
            return 1;
        }
        let data = match std::fs::read(exe) {
            Ok(data) => data,
            Err(error) => {
                let _ = writeln!(out, "stack guard: {}", io_message(&error, exe));
                return 1;
            }
        };
        let listing = Listing::new(&data, load_address(&data));
        if has_switch(&listing) {
            let _ = writeln!(
                out,
                "stack guard: {} switches to a scratchpad stack; pass its link map (ld.lld -Map)",
                exe.display()
            );
            return 1;
        }
        let _ = writeln!(out, "stack guard: no scratchpad stack in {}", exe.display());
        return 0;
    };
    let pattern = match pattern.map(Regex::new).transpose() {
        Ok(pattern) => pattern,
        Err(error) => {
            let _ = writeln!(out, "stack guard: bad --root pattern: {error}");
            return 1;
        }
    };
    let image = match GuardImage::open(exe, map_path) {
        Ok(image) => image,
        Err(error) => {
            let _ = writeln!(out, "stack guard: {error}");
            return 1;
        }
    };
    for symbol in forbidden {
        if !image
            .map
            .names
            .values()
            .any(|aliases| aliases.iter().any(|(_, name)| name == symbol))
        {
            let _ = writeln!(out, "stack guard: forbidden symbol {symbol} is not linked");
        }
    }
    let entries = roots(&image, pattern.as_ref(), budget);
    if entries.is_empty() {
        if let Some(pattern) = &pattern {
            let _ = writeln!(
                out,
                "stack guard: no symbol matches '{}' in {}",
                pattern.as_str(),
                map_path.display()
            );
            return 1;
        }
        if has_switch(&image.listing) {
            let _ = writeln!(
                out,
                "stack guard: {} contains psx-rt's stack switch but no ScratchpadStack entry is in {}",
                exe.display(),
                map_path.display()
            );
            return 1;
        }
        let _ = writeln!(
            out,
            "stack guard: no scratchpad stack entries in {}",
            exe.display()
        );
        return 0;
    }
    let mut walker = Walker::new(&image, forbidden);
    let mut failures = 0;
    for (address, name, limit, region) in entries {
        let place = region.map_or(String::new(), |(lo, hi)| format!("region {lo}..{hi}, "));
        let (total, chain) = match walker.depth(address, &mut Vec::new()) {
            Ok(found) => found,
            Err(error) => {
                let _ = writeln!(out, "FAIL {}: {error}", short(&name));
                failures += 1;
                continue;
            }
        };
        let verdict = if total <= limit { "ok  " } else { "FAIL" };
        let _ = writeln!(
            out,
            "{verdict} {}: {total} of {limit} bytes ({place}{address:08x}) via {}",
            short(&name),
            chain.join(" > ")
        );
        if total > limit {
            failures += 1;
        }
    }
    failures
}

/// The jump table the guard proves for the `jr` at `jr_addr` (for tests).
pub fn jump_table(image: &GuardImage, jr_addr: i64) -> Option<Vec<(i64, i64)>> {
    Detector::new(image.image(), Some(&image.map)).jump_table(jr_addr)
}

/// Run the guard with command-line `args` (no program name); returns the
/// exit status.
pub fn main(args: &[String], out: &mut dyn Write) -> i32 {
    let (mut paths, mut pattern, mut budget) = (Vec::new(), None, None);
    let mut forbidden = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--root" => match it.next() {
                Some(value) => pattern = Some(value.clone()),
                None => {
                    let _ = writeln!(out, "stack guard: --root needs a pattern");
                    return 1;
                }
            },
            "--forbid" => match it.next() {
                Some(value) => forbidden.push(value.clone()),
                None => {
                    let _ = writeln!(out, "stack guard: --forbid needs a symbol");
                    return 1;
                }
            },
            "--budget" => match it.next().map(|v| int_auto(v)) {
                Some(Some(value)) => budget = Some(value),
                _ => {
                    let _ = writeln!(out, "stack guard: --budget needs a number");
                    return 1;
                }
            },
            _ => paths.push(arg.clone()),
        }
    }
    if !(1..=2).contains(&paths.len()) || pattern.is_none() != budget.is_none() {
        let _ = write!(out, "{USAGE}");
        return 2;
    }
    let exe = Path::new(&paths[0]);
    let map = paths.get(1).map(Path::new);
    let failures = check_forbidding(exe, map, pattern.as_deref(), budget, &forbidden, out);
    if failures != 0 {
        let _ = writeln!(
            out,
            "stack guard: {failures} scratchpad stack entries fail in {}",
            paths[0]
        );
    }
    i32::from(failures != 0)
}

#[cfg(test)]
mod tests {
    use super::short;

    #[test]
    fn short_names() {
        assert_eq!(short("game::f::h0123456789abcdef"), "game::f");
        assert_eq!(short("game::f::h0123"), "game::f::h0123");
        let long = "x".repeat(95);
        assert_eq!(short(&long), format!("{}...", "x".repeat(87)));
    }
}
