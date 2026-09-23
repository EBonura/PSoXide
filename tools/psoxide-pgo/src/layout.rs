//! Function order for the R3000's I-cache, from exact per-word counts.
//!
//! The PS1's I-cache is 4 KB, direct-mapped, 256 lines of 16 bytes: a line's
//! set is bits 4..12 of its address, so a caller and a callee 4 KB apart
//! evict each other on every call. The link order decides which functions
//! share sets, and ld.lld takes that order from `--symbol-ordering-file`.
//!
//! `order` (pipeline.rs) replays a build with a count for every word and
//! writes a layout profile ([`Layout`]): for each function that ran, its
//! portable name, a hash of its code, the count of every word, and the
//! direct calls it made (`jal` and `j` words that land in another function).
//! A `jalr` calls through a register, so indirect calls are not in the
//! graph. `apply --variant ...+order` binds the profile onto its own link
//! ([`bind`]): a function whose code hash differs gets no heat, because its
//! counts belong to other instructions. [`place`] lays the hot functions out
//! and the pipeline relinks with the order it returns.
//!
//! The cost model (the prototype's `cap2.py`): a caller `x` that calls `y`
//! `n` times keeps the lines of the innermost loop around its call sites
//! live across every call, and the rest of its lines once per entry; each of
//! `y`'s lines is live for at most its own count over `n` of those calls.
//! Where both land in one set they refill each other, `2 n` times the
//! product of their use of that set. Two callees of one caller conflict the
//! same way, `min(n_g, n_h)` times when their call sites share a loop, and
//! at most once per entry of the caller when they do not.
//!
//! Placement: the functions in the most conflict terms go first, each at the
//! word offset within the next 4 KB that costs least against the functions
//! already placed. The gap in front of it is filled with the largest cold
//! functions that fit. Then come the executed functions with no call edges,
//! densest first, and the cold ones, largest first. Naive hot-first order
//! and a `.text.hot` script rule both measured slower than the plain link on
//! VoXide, so they are not offered.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use crate::{portable_name, Result};

/// I-cache sets (lines) and size.
const SETS: usize = 256;
const CACHE: u32 = 4096;

/// Offsets tried for a function: every word within one cache size.
const STEP: u32 = 4;

/// The least share of a layout profile's instructions that must bind onto
/// the build before `apply` places anything. A function whose code changed
/// is placed as cold, anywhere (a gap in the hot code included), so the
/// profile has to describe nearly all of the hot code. Stale and
/// cross-feature orders measured as losses on Quake; its chain-route
/// profile binds 94.4% onto its monster-route feature build.
pub const MIN_BOUND: f64 = 0.98;

/// Input-section name prefixes in front of a function's symbol.
const PREFIXES: [&str; 5] = [
    ".text.hot.",
    ".text.unlikely.",
    ".text.startup.",
    ".text.split.",
    ".text.",
];

/// First line of a layout profile.
const MAGIC: &str = "# psoxide-pgo layout 1";
const FEATURES_TAG: &str = "# features: ";
const VARIANT_TAG: &str = "# variant: ";

/// One input section of the link's `.text`.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub start: u32,
    pub size: u32,
    /// The input section's name, such as `.text._RNvCs..._4game4tick`.
    pub name: String,
    /// The symbol an ordering file names it by, when it has a usable one.
    pub symbol: Option<String>,
}

impl Section {
    fn end(&self) -> u32 {
        self.start + self.size
    }
}

fn hex(field: Option<&str>) -> Option<u32> {
    u32::from_str_radix(field?.trim(), 16).ok()
}

/// The input section an ld.lld map line names (`path:(.text.foo)`).
fn input_section(body: &str) -> Option<&str> {
    let inner = body.strip_suffix(')')?;
    let at = inner.rfind(":(")?;
    Some(&inner[at + 2..]).filter(|name| name.starts_with(".text"))
}

/// A plain linker name, as opposed to a demangled path from the map's
/// symbol column (which an ordering file cannot name).
fn plain(symbol: &str) -> bool {
    !symbol.is_empty()
        && symbol
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$'))
}

/// The symbol an ordering file names a section by: the mangled name in the
/// section's own name (the map prints symbols demangled), or its first
/// symbol when that is a plain name.
fn ordering_symbol(section: &str, symbols: &[String]) -> Option<String> {
    let candidate = PREFIXES
        .iter()
        .find_map(|prefix| section.strip_prefix(prefix))
        .unwrap_or("");
    if candidate.starts_with("_R")
        || candidate.starts_with("_ZN")
        || (!candidate.is_empty() && symbols.iter().any(|symbol| symbol == candidate))
    {
        return Some(candidate.to_string());
    }
    symbols.first().filter(|symbol| plain(symbol)).cloned()
}

/// The input sections of `.text` in an ld.lld `-Map`, in address order.
///
/// ELF32 map columns: VMA, LMA, size, alignment, then the output section at
/// column 33, an input section eight columns in and its symbols sixteen in.
pub fn read_map(text: &str) -> Vec<Section> {
    let mut found: Vec<(Section, Vec<String>)> = Vec::new();
    let (mut in_text, mut current) = (false, false);
    for line in text.lines() {
        if line.len() < 34 || line.as_bytes()[8] != b' ' {
            continue;
        }
        let (Some(start), Some(size), Some(rest)) =
            (hex(line.get(0..8)), hex(line.get(18..26)), line.get(33..))
        else {
            continue;
        };
        let body = rest.trim_start_matches(' ');
        let depth = rest.len() - body.len();
        let body = body.trim_end();
        if depth == 0 {
            in_text = body == ".text";
            current = false;
        } else if !in_text {
        } else if depth == 8 {
            current = false;
            if let Some(name) = input_section(body).filter(|_| size > 0) {
                let section = Section {
                    start,
                    size,
                    name: name.to_string(),
                    symbol: None,
                };
                found.push((section, Vec::new()));
                current = true;
            }
        } else if depth >= 16 && current {
            if let Some(last) = found.last_mut() {
                last.1.push(body.to_string());
            }
        }
    }
    let mut sections: Vec<Section> = found
        .into_iter()
        .map(|(mut section, symbols)| {
            section.symbol = ordering_symbol(&section.name, &symbols);
            section
        })
        .collect();
    sections.sort_by_key(|section| section.start);
    sections
}

/// Each section's portable name, or `None` when it has no symbol or shares
/// its name with another section (a profile could not say which it meant).
pub fn keys(sections: &[Section]) -> Vec<Option<String>> {
    let names: Vec<Option<String>> = sections
        .iter()
        .map(|section| {
            let symbol = section.symbol.as_deref()?;
            Some(portable_name(symbol).into_owned())
        })
        .collect();
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for name in names.iter().flatten() {
        *seen.entry(name).or_default() += 1;
    }
    names
        .iter()
        .map(|name| name.clone().filter(|name| seen[name.as_str()] == 1))
        .collect()
}

/// A flat PSX-EXE: the load address and the bytes after the 2 KB header.
pub struct Image {
    load: u32,
    body: Vec<u8>,
}

impl Image {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 0x800 || !data.starts_with(b"PS-X EXE") {
            return Err("not a PSX-EXE".into());
        }
        let load = u32::from_le_bytes(data[0x18..0x1c].try_into()?);
        Ok(Self {
            load,
            body: data[0x800..].to_vec(),
        })
    }

    pub fn word(&self, pc: u32) -> Option<u32> {
        let at = usize::try_from(pc.checked_sub(self.load)?).ok()?;
        let bytes = self.body.get(at..at.checked_add(4)?)?;
        Some(u32::from_le_bytes(bytes.try_into().ok()?))
    }

    /// A section's words, or `None` when it lies outside the image.
    fn words(&self, section: &Section) -> Option<Vec<u32>> {
        (0..section.size / 4)
            .map(|index| self.word(section.start + 4 * index))
            .collect()
    }
}

/// Where a branch or `j` at `pc` goes (`jal` is a call, not a loop edge).
fn branch_target(word: u32, pc: u32) -> Option<u32> {
    let op = word >> 26;
    let rt = (word >> 16) & 31;
    if matches!(op, 4..=7) || (op == 1 && rt <= 1) {
        let offset = ((word & 0xffff) as u16 as i16 as i32) << 2;
        return Some(pc.wrapping_add(4).wrapping_add(offset as u32));
    }
    (op == 2).then(|| jump_target(word, pc))
}

fn jump_target(word: u32, pc: u32) -> u32 {
    (pc & 0xf000_0000) | ((word & 0x03ff_ffff) << 2)
}

/// A hash of a function's code that the link order does not change.
///
/// Moving a function rewrites the fields the linker fills in: `j`/`jal`
/// targets, `lui` halves of addresses, and the low half an instruction adds
/// to a register a `lui` loaded. Those are masked (so are constants built
/// the same way, such as MMIO addresses); opcodes, registers, branch offsets
/// and every other immediate stay, so a function that gained, lost or
/// reordered an instruction hashes differently and its counts are not used.
pub fn code_hash(words: &[u32]) -> u64 {
    // Registers holding a `lui` half.
    let mut high: u32 = 0;
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for &word in words {
        let op = word >> 26;
        let rs = (word >> 21) & 31;
        let rt = (word >> 16) & 31;
        let rd = (word >> 11) & 31;
        let low_half = matches!(op, 0x09 | 0x0d | 0x20..=0x26 | 0x28..=0x2e | 0x32 | 0x3a)
            && high & (1 << rs) != 0;
        let masked = match op {
            2 | 3 => word & 0xfc00_0000,
            0x0f => word & 0xffff_0000,
            _ if low_half => word & 0xffff_0000,
            _ => word,
        };
        bytes.extend_from_slice(&masked.to_le_bytes());
        let written = match op {
            0x0f => {
                high |= 1 << rt;
                continue;
            }
            0 => Some(rd),
            1 if rt & 0x10 != 0 => Some(31), // bltzal, bgezal
            3 => Some(31),
            0x08..=0x0e | 0x20..=0x26 => Some(rt),
            0x10..=0x13 if rs == 0 || rs == 2 => Some(rt), // mfcN, cfcN
            _ => None,
        };
        if let Some(register) = written {
            high &= !(1 << register);
        }
    }
    crate::pipeline::fnv1a(&bytes)
}

/// A direct call edge: how often the caller's `jal`/`j` words to the callee
/// ran, at which of its words, and the callee's portable name.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub count: u64,
    pub sites: Vec<u32>,
    pub callee: String,
}

/// One executed function in a layout profile.
#[derive(Clone, Debug, PartialEq)]
pub struct Profiled {
    pub key: String,
    pub hash: u64,
    /// Its size in words.
    pub words: u32,
    /// `(word index, count)` for every word that ran.
    pub counts: Vec<(u32, u64)>,
    pub calls: Vec<Call>,
}

impl Profiled {
    fn instructions(&self) -> u64 {
        self.counts.iter().map(|&(_, count)| count).sum()
    }
}

/// A layout profile: what `order` writes and `apply ...+order` reads.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    /// The cargo features and the variant the counts came from.
    pub features: String,
    pub variant: String,
    pub functions: Vec<Profiled>,
}

/// What a replay's counts held beyond the functions.
#[derive(Debug, Default)]
pub struct Collected {
    pub layout: Layout,
    pub instructions: u64,
    /// Instructions outside every named function (the BIOS, psx-rt's
    /// trampolines, a function two sections share a name with).
    pub outside: u64,
    /// `jalr` words executed: calls the graph cannot see.
    pub indirect: u64,
}

/// The section holding `pc`, by binary search over address-ordered sections.
fn find(sections: &[Section], pc: u32) -> Option<usize> {
    let index = sections
        .partition_point(|section| section.start <= pc)
        .checked_sub(1)?;
    (pc < sections[index].end()).then_some(index)
}

/// The function a `jal`/`j` to `target` enters. A hazard trampoline
/// (`nop; j F; nop`, outside `.text`) stands for its own jump's target.
fn callee(sections: &[Section], image: &Image, target: u32) -> Option<usize> {
    if let Some(index) = find(sections, target) {
        return Some(index);
    }
    image.word(target)?;
    (0..4).find_map(|index| {
        let pc = target + 4 * index;
        let word = image.word(pc)?;
        matches!(word >> 26, 2 | 3).then(|| find(sections, jump_target(word, pc)))?
    })
}

/// Build a layout profile from one link's sections and image and the
/// per-word counts of replays of that image.
pub fn collect(sections: &[Section], image: &Image, counts: &HashMap<u32, u64>) -> Collected {
    let keys = keys(sections);
    let mut collected = Collected::default();
    let mut executed: Vec<(u32, u64)> = counts
        .iter()
        .filter(|&(_, &count)| count > 0)
        .map(|(&pc, &count)| (pc, count))
        .collect();
    executed.sort_unstable();
    let mut calls: BTreeMap<(usize, usize), Call> = BTreeMap::new();
    for &(pc, count) in &executed {
        collected.instructions += count;
        let from = find(sections, pc).filter(|&index| keys[index].is_some());
        if from.is_none() {
            collected.outside += count;
        }
        let Some(word) = image.word(pc) else {
            continue;
        };
        let op = word >> 26;
        if op == 0 && word & 0x3f == 9 {
            collected.indirect += count;
            continue;
        }
        if !matches!(op, 2 | 3) {
            continue;
        }
        let to = callee(sections, image, jump_target(word, pc));
        let (Some(from), Some(to)) = (from, to.filter(|&to| keys[to].is_some())) else {
            continue;
        };
        if from == to {
            continue;
        }
        let call = calls.entry((from, to)).or_insert_with(|| Call {
            count: 0,
            sites: Vec::new(),
            callee: keys[to].clone().expect("filtered to named callees"),
        });
        call.count += count;
        call.sites.push((pc - sections[from].start) / 4);
    }
    for (index, section) in sections.iter().enumerate() {
        let Some(key) = &keys[index] else {
            continue;
        };
        let counted: Vec<(u32, u64)> = (0..section.size / 4)
            .filter_map(|word| {
                let count = *counts.get(&(section.start + 4 * word))?;
                (count > 0).then_some((word, count))
            })
            .collect();
        if counted.is_empty() {
            continue;
        }
        let Some(words) = image.words(section) else {
            continue;
        };
        let mut edges: Vec<Call> = calls
            .range((index, 0)..(index + 1, 0))
            .map(|(_, call)| call.clone())
            .collect();
        edges.sort_by(|a, b| a.callee.cmp(&b.callee));
        collected.layout.functions.push(Profiled {
            key: key.clone(),
            hash: code_hash(&words),
            words: section.size / 4,
            counts: counted,
            calls: edges,
        });
    }
    collected.layout.functions.sort_by(|a, b| a.key.cmp(&b.key));
    collected
}

impl Layout {
    /// The text form: one `f` line per function (hash, words, key), `w`
    /// lines of `index:count`, and one `c` line per callee (count, sites,
    /// key). Keys go last on their lines because they may hold spaces.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "{MAGIC}");
        let _ = writeln!(out, "{FEATURES_TAG}{}", self.features);
        let _ = writeln!(out, "{VARIANT_TAG}{}", self.variant);
        out.push_str(
            "# Per-word counts over the gameplay polls (w index:count) and the direct\n\
             # calls from executed jal and j words (c count sites callee). Indirect calls\n\
             # (jalr) are not seen. A function binds by portable name and code hash.\n",
        );
        for function in &self.functions {
            let _ = writeln!(
                out,
                "f {:016x} {} {}",
                function.hash, function.words, function.key
            );
            for chunk in function.counts.chunks(16) {
                let pairs: Vec<String> = chunk
                    .iter()
                    .map(|(word, count)| format!("{word}:{count}"))
                    .collect();
                let _ = writeln!(out, "w {}", pairs.join(" "));
            }
            for call in &function.calls {
                let sites: Vec<String> = call.sites.iter().map(u32::to_string).collect();
                let _ = writeln!(out, "c {} {} {}", call.count, sites.join(","), call.callee);
            }
        }
        out
    }

    pub fn parse(text: &str) -> Result<Self> {
        let mut lines = text.lines();
        if lines.next() != Some(MAGIC) {
            return Err(format!("not a layout profile (no `{MAGIC}` line)").into());
        }
        let mut layout = Layout::default();
        for (number, line) in lines.enumerate() {
            let bad = || format!("layout profile line {}: {line:?}", number + 2);
            if let Some(features) = line.strip_prefix(FEATURES_TAG) {
                layout.features = features.to_string();
                continue;
            }
            if let Some(variant) = line.strip_prefix(VARIANT_TAG) {
                layout.variant = variant.to_string();
                continue;
            }
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let (tag, rest) = line.split_once(' ').ok_or_else(bad)?;
            if tag == "f" {
                let mut fields = rest.splitn(3, ' ');
                let (Some(hash), Some(words), Some(key)) =
                    (fields.next(), fields.next(), fields.next())
                else {
                    return Err(bad().into());
                };
                layout.functions.push(Profiled {
                    key: key.to_string(),
                    hash: u64::from_str_radix(hash, 16).map_err(|_| bad())?,
                    words: words.parse().map_err(|_| bad())?,
                    counts: Vec::new(),
                    calls: Vec::new(),
                });
                continue;
            }
            let function = layout.functions.last_mut().ok_or_else(bad)?;
            match tag {
                "w" => {
                    for pair in rest.split(' ') {
                        let (word, count) = pair.split_once(':').ok_or_else(bad)?;
                        let word: u32 = word.parse().map_err(|_| bad())?;
                        if word >= function.words {
                            return Err(bad().into());
                        }
                        function
                            .counts
                            .push((word, count.parse().map_err(|_| bad())?));
                    }
                }
                "c" => {
                    let mut fields = rest.splitn(3, ' ');
                    let (Some(count), Some(sites), Some(callee)) =
                        (fields.next(), fields.next(), fields.next())
                    else {
                        return Err(bad().into());
                    };
                    let sites = sites
                        .split(',')
                        .map(|site| site.parse::<u32>().ok().filter(|&s| s < function.words))
                        .collect::<Option<Vec<u32>>>()
                        .ok_or_else(bad)?;
                    function.calls.push(Call {
                        count: count.parse().map_err(|_| bad())?,
                        sites,
                        callee: callee.to_string(),
                    });
                }
                _ => return Err(bad().into()),
            }
        }
        Ok(layout)
    }
}

/// How much of a layout profile bound onto a build.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Coverage {
    pub functions: usize,
    pub bound: usize,
    /// Found by name, but the code differs.
    pub changed: usize,
    pub missing: usize,
    pub instructions: u64,
    pub bound_instructions: u64,
}

impl Coverage {
    pub fn share(&self) -> f64 {
        if self.instructions == 0 {
            return 0.0;
        }
        self.bound_instructions as f64 / self.instructions as f64
    }

    /// Refuse a profile that binds less than `minimum` of its instructions.
    pub fn check(&self, minimum: f64) -> Result<()> {
        if self.share() < minimum {
            return Err(format!(
                "the layout profile binds {:.1}% of its instructions onto this build, under \
                 the {:.0}% it needs ({self}); regenerate it with `order` on this code, \
                 features and variant",
                100.0 * self.share(),
                100.0 * minimum
            )
            .into());
        }
        Ok(())
    }
}

impl std::fmt::Display for Coverage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "bound {} of {} functions, {:.1}% of the profile's instructions; {} changed, {} \
             missing",
            self.bound,
            self.functions,
            100.0 * self.share(),
            self.changed,
            self.missing
        )
    }
}

/// A layout profile's counts and calls on one build's sections.
#[derive(Debug, Default)]
pub struct Bound {
    /// Per section: the count of every word, or empty (cold, changed or
    /// not in the profile).
    pub counts: Vec<Vec<u64>>,
    /// `(caller, callee, count, sites)` between bound sections.
    pub calls: Vec<(usize, usize, u64, Vec<u32>)>,
    pub coverage: Coverage,
}

/// Bind `layout` onto a build by portable name, keeping only the functions
/// whose code hashes the same.
pub fn bind(layout: &Layout, sections: &[Section], image: &Image) -> Bound {
    let keys = keys(sections);
    let index: HashMap<&str, usize> = keys
        .iter()
        .enumerate()
        .filter_map(|(at, key)| Some((key.as_deref()?, at)))
        .collect();
    let mut bound = Bound {
        counts: vec![Vec::new(); sections.len()],
        ..Bound::default()
    };
    let mut bound_at: HashMap<&str, usize> = HashMap::new();
    for function in &layout.functions {
        let instructions = function.instructions();
        let coverage = &mut bound.coverage;
        coverage.functions += 1;
        coverage.instructions += instructions;
        let Some(&at) = index.get(function.key.as_str()) else {
            coverage.missing += 1;
            continue;
        };
        let same = sections[at].size / 4 == function.words
            && image
                .words(&sections[at])
                .is_some_and(|words| code_hash(&words) == function.hash);
        if !same {
            coverage.changed += 1;
            continue;
        }
        coverage.bound += 1;
        coverage.bound_instructions += instructions;
        let mut counts = vec![0; function.words as usize];
        for &(word, count) in &function.counts {
            counts[word as usize] = count;
        }
        bound.counts[at] = counts;
        bound_at.insert(&function.key, at);
    }
    for function in &layout.functions {
        let Some(&from) = bound_at.get(function.key.as_str()) else {
            continue;
        };
        for call in &function.calls {
            if let Some(&to) = bound_at.get(call.callee.as_str()) {
                if to != from {
                    bound.calls.push((from, to, call.count, call.sites.clone()));
                }
            }
        }
    }
    bound.calls.sort_by_key(|call| (call.0, call.1));
    bound
}

/// `*` and `?` glob match, as the linker script's patterns use them.
fn glob(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((sp, st)) = star {
            p = sp + 1;
            t = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&b| b == b'*')
}

/// The `.text` input-section patterns a linker script places ahead of its
/// catch-all (`KEEP(*(.text._start))`, `*(.text.*hot_leaf*)`): an ordering
/// file cannot move what they match.
pub fn fixed_patterns(script: &str) -> Vec<String> {
    let mut text = String::with_capacity(script.len());
    let mut rest = script;
    while let Some(open) = rest.find("/*") {
        text.push_str(&rest[..open]);
        rest = rest[open..]
            .find("*/")
            .map_or("", |close| &rest[open + close + 2..]);
    }
    text.push_str(rest);
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let line = line.strip_prefix("KEEP(").unwrap_or(line);
            let inner = line.strip_prefix("*(")?;
            let (pattern, tail) = inner.split_at(inner.find(')')?);
            let tail = tail[1..].trim();
            let single = pattern.starts_with(".text") && !pattern.contains(char::is_whitespace);
            (single
                && matches!(tail, "" | ";" | ")" | ");")
                && !matches!(pattern, ".text" | ".text.*"))
            .then(|| pattern.to_string())
        })
        .collect()
}

/// A conflict term: `x` (with the loop around its call sites, if any)
/// against `y`, `n` interactions.
struct Term {
    x: usize,
    region: Option<(u32, u32)>,
    y: usize,
    n: f64,
}

struct Model {
    counts: Vec<Vec<f64>>,
    instructions: Vec<f64>,
    entries: Vec<f64>,
    terms: Vec<Term>,
    /// The terms each section is in.
    part: Vec<Vec<usize>>,
}

impl Model {
    /// Uses of each set by function `k`'s lines per interaction, for a start
    /// address `offset` bytes into a line, indexed from its first line.
    fn fold(&self, k: usize, offset: u32, n: f64, region: Option<(u32, u32)>) -> [f64; SETS] {
        let mut folded = [0.0; SETS];
        let counts = &self.counts[k];
        if self.instructions[k] == 0.0 {
            return folded;
        }
        let once = region.map(|_| (self.entries[k] / n).min(1.0));
        // (line, largest count, largest use) for each line with a counted word.
        let mut lines: Vec<(usize, f64, f64)> = Vec::new();
        for (word, &count) in counts.iter().enumerate() {
            if count <= 0.0 {
                continue;
            }
            let at = (offset as usize + 4 * word) >> 4;
            let used = match (region, once) {
                (Some((from, to)), Some(once)) if !(from as usize..to as usize).contains(&word) => {
                    once
                }
                _ => 1.0,
            };
            match lines.last_mut() {
                Some(line) if line.0 == at => {
                    line.1 = line.1.max(count);
                    line.2 = line.2.max(used);
                }
                _ => lines.push((at, count, used)),
            }
        }
        for (at, most, cap) in lines {
            folded[at % SETS] += cap.min(most / n);
        }
        folded
    }

    /// [`Self::fold`] rotated onto the sets of a start address.
    fn uses(&self, k: usize, address: u32, n: f64, region: Option<(u32, u32)>) -> [f64; SETS] {
        let folded = self.fold(k, address % 16, n, region);
        let first = (address >> 4) as usize % SETS;
        let mut sets = [0.0; SETS];
        for (line, value) in folded.iter().enumerate() {
            sets[(first + line) % SETS] = *value;
        }
        sets
    }

    fn term_cost(&self, term: &Term, x: u32, y: u32) -> f64 {
        let a = self.uses(term.x, x, term.n, term.region);
        let b = self.uses(term.y, y, term.n, None);
        2.0 * term.n * a.iter().zip(&b).map(|(a, b)| a * b).sum::<f64>()
    }

    /// Predicted refills between the functions that have an address.
    fn cost(&self, addresses: &[Option<u32>]) -> f64 {
        self.terms
            .iter()
            .filter_map(|term| Some(self.term_cost(term, addresses[term.x]?, addresses[term.y]?)))
            .sum()
    }
}

/// Backward branches and jumps in `words` as `(target, branch + 1)` word
/// ranges: the loops a call site can sit in.
fn loops(words: &[u32], start: u32) -> Vec<(u32, u32)> {
    words
        .iter()
        .enumerate()
        .filter_map(|(index, &word)| {
            let pc = start + 4 * index as u32;
            let target = branch_target(word, pc)?;
            (start <= target && target <= pc).then(|| ((target - start) / 4, index as u32 + 1))
        })
        .collect()
}

/// The innermost loop holding every one of `sites`.
fn region(loops: &[(u32, u32)], sites: &[u32]) -> Option<(u32, u32)> {
    loops
        .iter()
        .filter(|&&(from, to)| sites.iter().all(|&site| from <= site && site < to))
        .min_by_key(|&&(from, to)| to - from)
        .copied()
}

fn model(sections: &[Section], image: &Image, bound: &Bound) -> Model {
    let counts: Vec<Vec<f64>> = bound
        .counts
        .iter()
        .map(|counts| counts.iter().map(|&count| count as f64).collect())
        .collect();
    let instructions: Vec<f64> = counts.iter().map(|counts| counts.iter().sum()).collect();
    let loops: Vec<Vec<(u32, u32)>> = sections
        .iter()
        .zip(&instructions)
        .map(|(section, &instructions)| match image.words(section) {
            Some(words) if instructions > 0.0 => loops(&words, section.start),
            _ => Vec::new(),
        })
        .collect();
    let mut entries = vec![0.0; sections.len()];
    for &(_, to, count, _) in &bound.calls {
        entries[to] += count as f64;
    }
    for (entry, counts) in entries.iter_mut().zip(&counts) {
        if let Some(&first) = counts.first() {
            *entry = entry.max(first);
        }
    }
    let mut terms = Vec::new();
    // Per caller, its callees as (callee, count, sites).
    type Callees<'a> = Vec<(usize, f64, &'a [u32])>;
    let mut out: BTreeMap<usize, Callees<'_>> = BTreeMap::new();
    for (from, to, count, sites) in &bound.calls {
        let count = *count as f64;
        terms.push(Term {
            x: *from,
            region: region(&loops[*from], sites),
            y: *to,
            n: count,
        });
        out.entry(*from).or_default().push((*to, count, sites));
    }
    for (caller, callees) in &out {
        for (i, &(g, count_g, sites_g)) in callees.iter().enumerate() {
            for &(h, count_h, sites_h) in &callees[i + 1..] {
                let both: Vec<u32> = sites_g.iter().chain(sites_h).copied().collect();
                let n = if region(&loops[*caller], &both).is_some() {
                    count_g.min(count_h)
                } else {
                    count_g.min(count_h).min(entries[*caller])
                };
                if n > 0.0 {
                    terms.push(Term {
                        x: g,
                        region: None,
                        y: h,
                        n,
                    });
                }
            }
        }
    }
    let mut part = vec![Vec::new(); sections.len()];
    for (index, term) in terms.iter().enumerate() {
        part[term.x].push(index);
        part[term.y].push(index);
    }
    Model {
        counts,
        instructions,
        entries,
        terms,
        part,
    }
}

/// An ordering for one link.
#[derive(Debug, PartialEq)]
pub struct Placement {
    /// Section indices, in the order to link them.
    pub order: Vec<usize>,
    /// The placed functions and the addresses the model gave them.
    pub placed: Vec<(usize, u32)>,
    /// Cold bytes moved into gaps in front of placed functions.
    pub gap: u32,
    /// Predicted refills between the placed functions, at their current
    /// addresses and at the placed ones.
    pub before: f64,
    pub after: f64,
}

/// Order a link's `.text` from bound counts. `fixed` are the linker
/// script's patterns ahead of its catch-all ([`fixed_patterns`]).
pub fn place(sections: &[Section], image: &Image, bound: &Bound, fixed: &[String]) -> Placement {
    let model = model(sections, image, bound);
    let is_fixed: Vec<bool> = sections
        .iter()
        .map(|section| {
            fixed
                .iter()
                .any(|pattern| glob(pattern.as_bytes(), section.name.as_bytes()))
        })
        .collect();
    let free = |k: usize| sections[k].symbol.is_some() && !is_fixed[k];
    let mut at = sections
        .iter()
        .zip(&is_fixed)
        .filter(|(_, &fixed)| fixed)
        .map(|(section, _)| section.end())
        .max()
        .or_else(|| sections.iter().map(|section| section.start).min())
        .unwrap_or(0);
    let mut weight = vec![0.0; sections.len()];
    for term in &model.terms {
        weight[term.x] += term.n;
        weight[term.y] += term.n;
    }
    let mut hot: Vec<usize> = (0..sections.len())
        .filter(|&k| weight[k] > 0.0 && free(k) && model.instructions[k] > 0.0)
        .collect();
    hot.sort_by(|&a, &b| weight[b].total_cmp(&weight[a]));
    let mut is_hot = vec![false; sections.len()];
    for &k in &hot {
        is_hot[k] = true;
    }
    let density = |k: usize| model.instructions[k] / f64::from(sections[k].size.max(1));
    let mut rest: Vec<usize> = (0..sections.len())
        .filter(|&k| model.instructions[k] > 0.0 && !is_hot[k] && free(k))
        .collect();
    rest.sort_by(|&a, &b| density(b).total_cmp(&density(a)));
    let mut pool: Vec<usize> = (0..sections.len())
        .filter(|&k| model.instructions[k] == 0.0 && free(k))
        .collect();
    pool.sort_by_key(|&k| std::cmp::Reverse(sections[k].size));
    let mut used = vec![false; sections.len()];

    let mut addresses: Vec<Option<u32>> = vec![None; sections.len()];
    let mut order = Vec::new();
    let mut placed = Vec::new();
    let mut gap = 0;
    for &f in &hot {
        let mut want = 0;
        // Per term with a placed partner: its weight, f's uses folded for
        // each start offset within a line, and the partner's uses by set.
        let prepared: Vec<(f64, [[f64; SETS]; 4], [f64; SETS])> = model.part[f]
            .iter()
            .filter_map(|&index| {
                let term = &model.terms[index];
                let (own, partner, partner_region) = if term.x == f {
                    (term.region, term.y, None)
                } else {
                    (None, term.x, term.region)
                };
                let partner_at = addresses[partner]?;
                let folds = [0, 4, 8, 12].map(|offset| model.fold(f, offset, term.n, own));
                let uses = model.uses(partner, partner_at, term.n, partner_region);
                Some((term.n, folds, uses))
            })
            .collect();
        if !prepared.is_empty() {
            let mut best: Option<(f64, u32)> = None;
            for d in (0..CACHE).step_by(STEP as usize) {
                let address = at + d;
                let offset = (address % 16 / 4) as usize;
                let first = (address >> 4) as usize % SETS;
                let mut cost = 0.0;
                for (n, folds, uses) in &prepared {
                    let fold = &folds[offset];
                    let dot: f64 = (0..SETS)
                        .map(|line| fold[line] * uses[(first + line) % SETS])
                        .sum();
                    cost += 2.0 * n * dot;
                }
                cost += 1e-6 * f64::from(d);
                if best.is_none_or(|(least, _)| cost < least) {
                    best = Some((cost, d));
                }
            }
            want = best.map_or(0, |(_, d)| d);
        }
        let mut filled = 0;
        if want > 0 {
            for &k in &pool {
                if used[k] {
                    continue;
                }
                if sections[k].size <= want - filled {
                    used[k] = true;
                    order.push(k);
                    filled += sections[k].size;
                    if want - filled < 4 {
                        break;
                    }
                }
            }
        }
        gap += filled;
        at += filled;
        addresses[f] = Some(at);
        placed.push((f, at));
        order.push(f);
        at += sections[f].size;
    }
    order.extend(rest);
    order.extend(pool.into_iter().filter(|&k| !used[k]));

    let current: Vec<Option<u32>> = sections
        .iter()
        .enumerate()
        .map(|(k, section)| is_hot[k].then_some(section.start))
        .collect();
    Placement {
        order,
        placed,
        gap,
        before: model.cost(&current),
        after: model.cost(&addresses),
    }
}

/// The ordering file for a placement: one symbol per line.
pub fn order_file(sections: &[Section], placement: &Placement) -> String {
    let mut out = String::new();
    for &k in &placement.order {
        if let Some(symbol) = &sections[k].symbol {
            out.push_str(symbol);
            out.push('\n');
        }
    }
    out
}

/// How the relinked map followed an ordering file.
#[derive(Debug, PartialEq)]
pub struct Followed {
    pub listed: usize,
    /// Placed functions not at the address the model gave them.
    pub drifted: usize,
}

/// Check that the relink put every listed symbol in the listed order, and
/// count the placed functions that did not land where the model put them.
/// ld.lld skips a name it cannot find without a word under
/// `--no-warn-symbol-ordering`, and a guest whose build.rs never passes the
/// order links as before, so this is the only proof the order was used.
pub fn check_order(
    order: &str,
    placed: &[(String, u32)],
    relinked: &[Section],
) -> Result<Followed> {
    let mut starts: HashMap<&str, u32> = HashMap::new();
    for section in relinked {
        if let Some(symbol) = &section.symbol {
            starts.entry(symbol).or_insert(section.start);
        }
    }
    let mut previous: Option<(&str, u32)> = None;
    let mut listed = 0;
    for symbol in order.lines() {
        let start = *starts
            .get(symbol)
            .ok_or_else(|| format!("{symbol} from the order file is not in the relinked map"))?;
        if let Some((before, at)) = previous.filter(|&(_, at)| at >= start) {
            return Err(format!(
                "the relink did not follow the order file: {symbol} is at {start:#010x}, not \
                 after {before} at {at:#010x}. Does the guest's build.rs pass \
                 PSOXIDE_LINK_ORDER to the link (see README.md)?"
            )
            .into());
        }
        previous = Some((symbol, start));
        listed += 1;
    }
    let drifted = placed
        .iter()
        .filter(|(symbol, at)| starts.get(symbol.as_str()) != Some(at))
        .count();
    Ok(Followed { listed, drifted })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OBJ: &str = "/g/deps/game-0123.game.o";

    fn map_line(start: u32, size: u32, depth: usize, body: &str) -> String {
        format!(
            "{start:8x} {start:8x} {size:8x} {:5} {}{body}\n",
            4,
            " ".repeat(depth)
        )
    }

    /// A map with `_start` fixed first and `(section, symbol, size)` after.
    fn map(functions: &[(&str, &str, u32)]) -> String {
        let mut text = String::from("     VMA      LMA     Size Align Out     In      Symbol\n");
        let mut at = 0x8001_0000;
        let total: u32 = 0x10 + functions.iter().map(|f| f.2).sum::<u32>();
        text += &map_line(at, total, 0, ".text");
        text += &map_line(at, 0, 8, "__text_start = .");
        text += &map_line(at, 0x10, 8, &format!("{OBJ}:(.text._start)"));
        text += &map_line(at, 0x10, 16, "_start");
        at += 0x10;
        for (section, symbol, size) in functions {
            text += &map_line(at, *size, 8, &format!("{OBJ}:({section})"));
            text += &map_line(at, *size, 16, symbol);
            at += size;
        }
        text += &map_line(at, 0x20, 0, ".data");
        text += &map_line(at, 0x20, 8, &format!("{OBJ}:(.data.HAZARD_TRAMPOLINES)"));
        text
    }

    // One function in two checkouts: only the crate disambiguator differs.
    const TICK_A: &str = "_RNvCs17CBFHtzUmz_4game4tick";
    const TICK_B: &str = "_RNvCskfeMIapBdWl_4game4tick";

    #[test]
    fn maps_name_sections_by_their_mangled_symbol() {
        let text = map(&[
            (&format!(".text.{TICK_A}"), "game::tick", 0x20),
            (".text", "memcpy", 0x10),
            (".text.anon", "<game::X as core::Trait>::f", 0x8),
        ]);
        let sections = read_map(&text);
        let names: Vec<(u32, Option<&str>)> = sections
            .iter()
            .map(|s| (s.start, s.symbol.as_deref()))
            .collect();
        assert_eq!(
            names,
            vec![
                (0x8001_0000, Some("_start")),
                (0x8001_0010, Some(TICK_A)),
                (0x8001_0030, Some("memcpy")),
                // A demangled path is no name an ordering file can use.
                (0x8001_0040, None),
            ]
        );
    }

    #[test]
    fn keys_translate_across_checkouts_and_drop_shared_names() {
        let a = read_map(&map(&[(&format!(".text.{TICK_A}"), "game::tick", 8)]));
        let b = read_map(&map(&[(&format!(".text.{TICK_B}"), "game::tick", 8)]));
        assert_eq!(keys(&a), keys(&b));
        assert_eq!(keys(&a)[1].as_deref(), Some("game::tick"));
        // Two sections one name: neither can carry counts.
        let twice = read_map(&map(&[
            (&format!(".text.{TICK_A}"), "game::tick", 8),
            (&format!(".text.{TICK_B}"), "game::tick", 8),
        ]));
        assert_eq!(keys(&twice)[1..], [None, None]);
    }

    const LUI_AT: u32 = 0x3c01_0000; // lui at, 0
    const LW_T0_AT: u32 = 0x8c28_0000; // lw t0, 0(at)
    const JAL: u32 = 0x0c00_0000;
    const ADDU: u32 = 0x0109_4021; // addu t0, t0, t1
    const LW_T0_SP: u32 = 0x8fa8_0010; // lw t0, 16(sp)

    #[test]
    fn code_hashes_ignore_what_the_linker_fills_in() {
        let base = [LUI_AT | 0x8002, LW_T0_AT | 0x1234, JAL | 0x4000, ADDU];
        let moved = [LUI_AT | 0x8003, LW_T0_AT | 0x0010, JAL | 0x4321, ADDU];
        assert_eq!(code_hash(&base), code_hash(&moved));
        // A different register, stack offset or instruction is other code.
        assert_ne!(
            code_hash(&base),
            code_hash(&[base[0], base[1], base[2], ADDU ^ 0x800])
        );
        assert_ne!(code_hash(&[LW_T0_SP]), code_hash(&[LW_T0_SP + 4]));
        assert_ne!(code_hash(&base), code_hash(&base[..3]));
        // Once `at` is overwritten, its offsets are real again.
        let reloaded = [LUI_AT, 0x2401_0000, LW_T0_AT | 4]; // li at, 0; lw t0, 4(at)
        let other = [LUI_AT, 0x2401_0000, LW_T0_AT | 8];
        assert_ne!(code_hash(&reloaded), code_hash(&other));
    }

    fn image(words: &[(u32, u32)]) -> Image {
        let mut data = vec![0u8; 0x800 + 0x2000];
        data[..8].copy_from_slice(b"PS-X EXE");
        data[0x18..0x1c].copy_from_slice(&0x8001_0000u32.to_le_bytes());
        for &(pc, word) in words {
            let at = 0x800 + (pc - 0x8001_0000) as usize;
            data[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
        Image::parse(&data).unwrap()
    }

    fn jal(target: u32) -> u32 {
        JAL | ((target >> 2) & 0x03ff_ffff)
    }

    /// `main` (0x40 bytes) calls `a` and `b` in a loop; `c` is cold.
    fn game() -> (Vec<Section>, Image, HashMap<u32, u64>) {
        let text = map(&[
            (".text.main", "main", 0x40),
            (".text.a", "a", 0x40),
            (".text.b", "b", 0x40),
            (".text.c", "c", 0x100),
        ]);
        let sections = read_map(&text);
        let main = 0x8001_0010;
        let (a, b) = (0x8001_0050, 0x8001_0090);
        // Loop: jal a; nop; jal b; nop; bne ... back to the first jal.
        let image = image(&[
            (main + 8, jal(a)),
            (main + 16, jal(b)),
            (main + 24, 0x1500_fffb), // bne t0, zero, main+8
            (a, ADDU),
            (b, ADDU),
        ]);
        let mut counts = HashMap::new();
        for word in 0..16 {
            counts.insert(
                main + 4 * word,
                if (2..8).contains(&word) { 100 } else { 1 },
            );
        }
        for word in 0..4 {
            counts.insert(a + 4 * word, 100);
            counts.insert(b + 4 * word, 100);
        }
        counts.insert(0xbfc0_0000, 7); // the BIOS
        (sections, image, counts)
    }

    #[test]
    fn layouts_round_trip_through_their_text() {
        let (sections, image, counts) = game();
        let mut collected = collect(&sections, &image, &counts);
        collected.layout.features = "(default)".into();
        collected.layout.variant = "hot=500+profi".into();
        assert_eq!(collected.outside, 7);
        let main = &collected.layout.functions[2];
        assert_eq!(main.key, "main");
        assert_eq!(
            main.calls,
            vec![
                Call {
                    count: 100,
                    sites: vec![2],
                    callee: "a".into()
                },
                Call {
                    count: 100,
                    sites: vec![4],
                    callee: "b".into()
                },
            ]
        );
        let text = collected.layout.to_text();
        assert_eq!(Layout::parse(&text).unwrap(), collected.layout);
    }

    #[test]
    fn coverage_counts_changed_code_as_unbound() {
        let (sections, image, counts) = game();
        let layout = collect(&sections, &image, &counts).layout;
        let bound = bind(&layout, &sections, &image);
        assert_eq!(bound.coverage.bound, 3);
        assert_eq!(bound.coverage.share(), 1.0);
        assert_eq!(bound.calls.len(), 2);
        assert!(bound.coverage.check(MIN_BOUND).is_ok());

        // `main` changed: its 610 of 1410 instructions no longer bind, and
        // neither do its calls.
        let (_, mut changed, _) = game();
        changed.body[0x10 + 12..0x10 + 16].copy_from_slice(&ADDU.to_le_bytes());
        let bound = bind(&layout, &sections, &changed);
        assert_eq!((bound.coverage.bound, bound.coverage.changed), (2, 1));
        assert!(bound.calls.is_empty());
        assert!((bound.coverage.share() - 800.0 / 1410.0).abs() < 1e-9);
        assert!(bound.coverage.check(MIN_BOUND).is_err());
        assert!(bound.coverage.check(0.5).is_ok());
    }

    #[test]
    fn placement_is_deterministic_and_keeps_callees_apart() {
        let (sections, image, counts) = game();
        let layout = collect(&sections, &image, &counts).layout;
        let bound = bind(&layout, &sections, &image);
        let fixed = fixed_patterns("KEEP(*(.text._start));\n*(.text .text.*);\n");
        assert_eq!(fixed, vec![".text._start"]);
        let first = place(&sections, &image, &bound, &fixed);
        assert_eq!(first, place(&sections, &image, &bound, &fixed));
        // Every orderable section once; `_start` stays where the script puts it.
        let mut listed = first.order.clone();
        listed.sort_unstable();
        assert_eq!(listed, vec![1, 2, 3, 4]);
        // Here the plain order already keeps all three in their own sets.
        assert_eq!(first.before, 0.0);
        assert_eq!(first.after, 0.0);
        assert_eq!(first.placed.len(), 3);
        let symbols: Vec<(String, u32)> = first
            .placed
            .iter()
            .map(|&(k, at)| (sections[k].symbol.clone().unwrap(), at))
            .collect();
        let order = order_file(&sections, &first);
        // The link as the model predicts it follows the order.
        let mut relinked = sections.clone();
        for (symbol, at) in &symbols {
            let section = relinked
                .iter_mut()
                .find(|s| s.symbol.as_ref() == Some(symbol))
                .unwrap();
            section.start = *at;
        }
        let cold = relinked.iter_mut().find(|s| s.name == ".text.c").unwrap();
        cold.start = 0x8002_0000;
        let followed = check_order(&order, &symbols, &relinked).unwrap();
        assert_eq!(
            followed,
            Followed {
                listed: 4,
                drifted: 0
            }
        );
        // The link as it was does not, unless the order is the old one.
        let reversed: String = order
            .lines()
            .rev()
            .map(|line| format!("{line}\n"))
            .collect();
        assert!(check_order(&reversed, &symbols, &sections).is_err());
    }

    #[test]
    fn a_callee_a_cache_size_away_moves_off_its_callers_sets() {
        // `a` sits 4 KB after `main`'s loop: the plain order conflicts on
        // every call, the placement does not.
        let text = map(&[
            (".text.main", "main", 0x40),
            (".text.pad", "pad", 0x1000 - 0x40),
            (".text.a", "a", 0x40),
        ]);
        let sections = read_map(&text);
        let (main, a) = (0x8001_0010, 0x8001_1010);
        let image = image(&[(main + 8, jal(a)), (main + 16, 0x1500_fffd)]);
        let mut counts = HashMap::new();
        for word in 0..16 {
            counts.insert(main + 4 * word, 100);
            counts.insert(a + 4 * word, 100);
        }
        let bound = bind(
            &collect(&sections, &image, &counts).layout,
            &sections,
            &image,
        );
        let placement = place(&sections, &image, &bound, &[".text._start".to_string()]);
        assert!(placement.before > 0.0);
        assert_eq!(placement.after, 0.0);
    }

    #[test]
    fn fixed_patterns_skip_comments_and_the_catch_all() {
        let script = "/* *(.text.in_a_comment); */\n.text : {\n KEEP(*(.text._start));\n \
                      *(.text.*hot_leaf*)\n *(.text .text.*);\n}\n";
        // `*(.text.*hot_leaf*)` has no `;`, as in psoxide.ld: it still counts.
        assert_eq!(
            fixed_patterns(script),
            vec![".text._start", ".text.*hot_leaf*"]
        );
        assert!(glob(b".text.*hot_leaf*", b".text._RNv4game8hot_leaf3run"));
        assert!(!glob(b".text.*hot_leaf*", b".text._RNv4game4cold"));
    }
}
