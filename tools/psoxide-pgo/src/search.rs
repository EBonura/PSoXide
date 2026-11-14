//! Placement search scored by the I-cache itself.
//!
//! [`crate::layout::place`] orders functions from per-word counts and a
//! conflict model of who calls whom. The model cannot see the order in which
//! code runs, and that order is what decides a direct-mapped cache: a line
//! misses exactly when another line of its set was fetched since its own last
//! fetch. So two placements the model scores alike can differ by percent in
//! refill stalls, and a game was left choosing between weight draws.
//!
//! This module replays the order instead. The emulator's
//! `--fetch-trace-log` records every retired instruction as sequential runs
//! (format below). [`Traces::read`] binds each run to the function of the
//! link being placed (by portable name and code hash, as layout profiles
//! bind) and reduces it, [`Traces::simulate`] replays it through a model of
//! the R3000's cache at any set of function addresses, and [`search`]
//! moves functions to the cache offsets that cost the fewest stall cycles.
//!
//! The cache (the emulator's model, after the CXD8606Q): 256 lines of 16
//! bytes, direct-mapped on bits 4..12 of the physical address, a valid bit
//! per word, IBLKSZ 4 words with streaming on (cache control 1E988h). A
//! fetch whose line holds another tag refills from the fetched word to the
//! end of the line and stalls `words + 1` cycles; a fetch whose line matches
//! but whose word is not valid refills the whole line and also waits for the
//! words in front of it (`4 + 1 + lead`). Uncached fetches (KSEG1, the BIOS
//! ROM) never touch it. Not modelled: the wait when the CPU leaves a line
//! whose streaming fill has not finished, and cache flushes during the
//! window.
//!
//! The reduction is exact for that model. Code is split into chunks of 2 KB
//! of one function (or of fixed addresses): wherever a chunk is placed, two
//! of its lines never share a set. A segment is a stretch of the trace that
//! stays in one chunk; inside it, a word fetched a second time is always a
//! hit, so a segment is kept as the words it touches, in the order it first
//! touches them, and repeated segments are interned. A gameplay window of a
//! few hundred million instructions becomes a few tens of millions of
//! segment ids.
//!
//! Fetch trace format: the 8 bytes `PSXFTRC1`, then LEB128 varints. A run of
//! `n` sequential words at word address `w` is `zigzag(w - e) + 1` and `n`,
//! `e` being one past the previous run's last word (0 at the start); a lone
//! `0` starts a route tick.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::layout::{self, Image, Section};
use crate::Result;

/// I-cache sets and size.
const SETS: usize = 256;
const CACHE: u32 = 4096;

/// A chunk is `1 << CHUNK_SHIFT` bytes: under the cache size less a line, so
/// its lines are in distinct sets at any address.
const CHUNK_SHIFT: u32 = 11;

/// The unit of code at a fixed address (outside every bound function).
const FIXED: u32 = u32::MAX;

const TRACE_MAGIC: &[u8; 8] = b"PSXFTRC1";

/// Set on an interval's last word when execution jumps away after it (a
/// chunk holds at most 512 words, so the bit is free).
const LEAVES: u16 = 0x8000;

/// The stall cycles of one refill, as the emulator charges them with
/// streaming on. The fill starts at the fetched `word` (a tag miss, one more
/// setup clock when that is mid-line) or at word 0 when the tag matched
/// (`whole_line`, two clocks for each word ahead of the fetched one). Words
/// arrive every two clocks and the core runs each as it lands, waiting one
/// clock for every word after the first of the `ran` it executes; if it
/// jumps away before the line's end (`left`), it waits for the fill to let
/// go of the bus and two clocks more.
fn refill(word: u32, whole_line: bool, ran: u32, left: bool) -> u64 {
    let (stall, busy) = if whole_line {
        (2 + 2 * word, 8)
    } else {
        let mid = u32::from(word != 0);
        (2 + mid, 2 * (4 - word) + mid)
    };
    let mut cycles = stall + ran.saturating_sub(1);
    if left {
        let jump = stall + 2 * ran.saturating_sub(1) + 1;
        cycles += (busy + 2).saturating_sub(jump);
    }
    u64::from(cycles)
}

/// One record of a fetch trace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// `words` instructions retired in sequence from `pc`.
    Run { pc: u32, words: u32 },
    /// A route tick started.
    Tick,
}

/// Decode a fetch trace (see the module documentation).
#[cfg(test)]
fn decode(bytes: &[u8]) -> Result<Vec<Event>> {
    let mut events = Vec::new();
    for_each_event(bytes, |event| events.push(event))?;
    Ok(events)
}

fn for_each_event(bytes: &[u8], mut visit: impl FnMut(Event)) -> Result<()> {
    let body = bytes
        .strip_prefix(TRACE_MAGIC)
        .ok_or("not a fetch trace (no PSXFTRC1 header)")?;
    let mut at = 0usize;
    let mut next = || -> Option<u64> {
        let mut value = 0u64;
        let mut shift = 0;
        loop {
            let byte = *body.get(at)?;
            at += 1;
            value |= u64::from(byte & 0x7f) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
    };
    let mut end: i64 = 0;
    while let Some(head) = next() {
        if head == 0 {
            visit(Event::Tick);
            continue;
        }
        let zigzag = head - 1;
        let delta = (zigzag >> 1) as i64 ^ -((zigzag & 1) as i64);
        let words = next().ok_or("fetch trace ends inside a run")?;
        let start = end + delta;
        visit(Event::Run {
            pc: (start as u32) << 2,
            words: words as u32,
        });
        end = start + words as i64;
    }
    Ok(())
}

/// The code a placement moves: one link's `.text` input sections, with the
/// alignment each asks for and which of them an ordering file can move.
pub struct Link {
    pub sections: Vec<Section>,
    pub image: Image,
    /// Each section's portable name, when it has a unique one.
    pub keys: Vec<Option<String>>,
    align: Vec<u32>,
    /// Placed by a linker-script rule ahead of the catch-all.
    fixed: Vec<bool>,
    /// Where the catch-all's sections start.
    start: u32,
}

/// Every input section's alignment from an ld.lld map, by start address.
fn alignments(map: &str) -> HashMap<(u32, u32), u32> {
    let mut found = HashMap::new();
    for line in map.lines() {
        if line.len() < 34 || line.as_bytes()[8] != b' ' {
            continue;
        }
        let field = |range: std::ops::Range<usize>| {
            line.get(range)
                .and_then(|text| u32::from_str_radix(text.trim(), 16).ok())
        };
        let (Some(start), Some(size)) = (field(0..8), field(18..26)) else {
            continue;
        };
        let Some(align) = line
            .get(27..32)
            .and_then(|text| text.trim().parse::<u32>().ok())
        else {
            continue;
        };
        found.entry((start, size)).or_insert(align.max(1));
    }
    found
}

impl Link {
    /// A link from its map, its image and the linker script's fixed
    /// patterns ([`layout::fixed_patterns`]).
    pub fn new(map: &str, image: Image, patterns: &[String]) -> Self {
        let sections = layout::read_map(map);
        let keys = layout::keys(&sections);
        let aligns = alignments(map);
        let align = sections
            .iter()
            .map(|section| {
                aligns
                    .get(&(section.start, section.size))
                    .copied()
                    .unwrap_or(4)
            })
            .collect();
        let fixed: Vec<bool> = sections
            .iter()
            .map(|section| {
                patterns
                    .iter()
                    .any(|pattern| layout::glob(pattern.as_bytes(), section.name.as_bytes()))
            })
            .collect();
        let start = layout::ordered_start(&sections, patterns);
        Self {
            sections,
            image,
            keys,
            align,
            fixed,
            start,
        }
    }

    /// An ordering file can name it and the catch-all places it.
    fn movable(&self, k: usize) -> bool {
        self.keys[k].is_some() && self.sections[k].symbol.is_some() && !self.fixed[k]
    }

    /// The section holding `pc`.
    fn find(&self, pc: u32) -> Option<usize> {
        let index = self
            .sections
            .partition_point(|section| section.start <= pc)
            .checked_sub(1)?;
        (pc < self.sections[index].start + self.sections[index].size).then_some(index)
    }

    fn hash(&self, k: usize) -> Option<u64> {
        self.image
            .words(&self.sections[k])
            .map(|words| layout::code_hash(&words))
    }

    /// Every section's address when the catch-all lists `listed` first, in
    /// that order, and then the rest in their present order. This is what
    /// ld.lld's `--symbol-ordering-file` does on MIPS: the sections it lists
    /// come first in their input section description, sorted by the order of
    /// the file, and the others follow in the order they had.
    pub fn realize(&self, listed: &[usize]) -> Vec<u32> {
        let mut address: Vec<u32> = self.sections.iter().map(|section| section.start).collect();
        let mut is_listed = vec![false; self.sections.len()];
        let mut at = self.start;
        for &k in listed {
            is_listed[k] = true;
            at = at.next_multiple_of(self.align[k]);
            address[k] = at;
            at += self.sections[k].size;
        }
        for k in 0..self.sections.len() {
            if is_listed[k] || self.fixed[k] || self.sections[k].start < self.start {
                continue;
            }
            at = at.next_multiple_of(self.align[k]);
            address[k] = at;
            at += self.sections[k].size;
        }
        address
    }
}

/// Where a segment's code is: a section of the link and a chunk of it, or
/// 2 KB of fixed addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Chunk {
    unit: u32,
    index: u32,
}

#[derive(Clone, Copy, Debug)]
struct Segment {
    chunk: u32,
    first: u32,
    len: u32,
}

/// One replay window of a fetch trace, reduced.
pub struct Window {
    pub name: String,
    /// Its share of the objective: the search minimises the sum over
    /// windows of `weight * stalls / stalls at the start`.
    pub weight: f64,
    ids: Vec<u32>,
    pub instructions: u64,
    /// Instructions in functions of the traced build that did not bind to
    /// the link (code changed); simulated at the traced address.
    pub unbound: u64,
    pub uncached: u64,
    pub ticks: u64,
    /// Instructions per section of the link.
    pub executed: HashMap<u32, u64>,
}

/// Fetch traces bound to one link and reduced to segments.
#[derive(Default)]
pub struct Traces {
    chunks: Vec<Chunk>,
    segments: Vec<Segment>,
    /// Word ranges within a chunk, inclusive, in first-touch order.
    intervals: Vec<(u16, u16)>,
    pub windows: Vec<Window>,
    interned: HashMap<(u32, Vec<(u16, u16)>), u32>,
    chunk_ids: HashMap<Chunk, u32>,
    /// Instructions per unit over all windows (`FIXED` excluded).
    pub executed: HashMap<u32, u64>,
}

/// Builds the segments of one window.
struct Reducer<'a> {
    traces: &'a mut Traces,
    ids: Vec<u32>,
    chunk: Option<u32>,
    current: Vec<(u16, u16)>,
    touched: [u64; 8],
}

impl Reducer<'_> {
    fn end_segment(&mut self) {
        let Some(chunk) = self.chunk.take() else {
            return;
        };
        let intervals = std::mem::take(&mut self.current);
        self.touched = [0; 8];
        let traces = &mut *self.traces;
        let id = match traces.interned.get(&(chunk, intervals.clone())) {
            Some(&id) => id,
            None => {
                let id = traces.segments.len() as u32;
                traces.segments.push(Segment {
                    chunk,
                    first: traces.intervals.len() as u32,
                    len: intervals.len() as u32,
                });
                traces.intervals.extend_from_slice(&intervals);
                traces.interned.insert((chunk, intervals), id);
                id
            }
        };
        self.ids.push(id);
    }

    /// Words `from..=to` (indices within the chunk) of `chunk` ran.
    /// `ends`: the run stops after word `to` (execution jumps away).
    fn touch(&mut self, chunk: Chunk, from: u16, to: u16, ends: bool) {
        let id = match self.traces.chunk_ids.get(&chunk) {
            Some(&id) => id,
            None => {
                let id = self.traces.chunks.len() as u32;
                self.traces.chunks.push(chunk);
                self.traces.chunk_ids.insert(chunk, id);
                id
            }
        };
        if self.chunk != Some(id) {
            self.end_segment();
            self.chunk = Some(id);
        }
        let mut word = from;
        while word <= to {
            let bit = |word: u16| (word >> 6) as usize;
            if self.touched[bit(word)] >> (word & 63) & 1 != 0 {
                word += 1;
                continue;
            }
            let first = word;
            while word <= to && self.touched[bit(word)] >> (word & 63) & 1 == 0 {
                self.touched[bit(word)] |= 1 << (word & 63);
                word += 1;
            }
            // Not merged with the interval before: where execution left a
            // line decides what its refill cost.
            let leaves = if word > to && ends { LEAVES } else { 0 };
            self.current.push((first, (word - 1) | leaves));
        }
    }
}

/// How a traced build's sections bind to the link: the link's section for
/// each, when the portable name and code agree.
fn bind_sections(traced: &Link, link: &Link) -> Vec<Option<u32>> {
    let index: HashMap<&str, usize> = link
        .keys
        .iter()
        .enumerate()
        .filter_map(|(at, key)| Some((key.as_deref()?, at)))
        .collect();
    (0..traced.sections.len())
        .map(|k| {
            let key = traced.keys[k].as_deref()?;
            let &at = index.get(key)?;
            (traced.sections[k].size == link.sections[at].size
                && traced.hash(k).is_some()
                && traced.hash(k) == link.hash(at))
            .then_some(at as u32)
        })
        .collect()
}

/// The physical address of a cached fetch, in KSEG0 form, or `None` for an
/// uncached one.
fn cached(pc: u32) -> Option<u32> {
    (pc < 0xa000_0000).then_some(0x8000_0000 | (pc & 0x1fff_ffff))
}

impl Traces {
    /// Add one fetch trace, recorded on the build `traced`, as a window of
    /// `link`.
    pub fn read(
        &mut self,
        name: &str,
        weight: f64,
        trace: &[u8],
        traced: &Link,
        link: &Link,
    ) -> Result<()> {
        let bound = bind_sections(traced, link);
        let mut window = Window {
            name: name.to_string(),
            weight,
            ids: Vec::new(),
            instructions: 0,
            unbound: 0,
            uncached: 0,
            ticks: 0,
            executed: HashMap::new(),
        };
        let mut executed: HashMap<u32, u64> = HashMap::new();
        let mut reducer = Reducer {
            traces: self,
            ids: Vec::new(),
            chunk: None,
            current: Vec::new(),
            touched: [0; 8],
        };
        let mut hint = 0usize;
        for_each_event(trace, |event| {
            let (pc, words) = match event {
                Event::Tick => {
                    window.ticks += 1;
                    return;
                }
                Event::Run { pc, words } => (pc, words),
            };
            window.instructions += u64::from(words);
            let Some(mut address) = cached(pc) else {
                window.uncached += u64::from(words);
                return;
            };
            let mut left = words;
            while left > 0 {
                let section = match traced.sections.get(hint) {
                    Some(section)
                        if section.start <= address && address < section.start + section.size =>
                    {
                        Some(hint)
                    }
                    _ => traced.find(address),
                };
                let (unit, offset, room) = match section {
                    Some(k) => {
                        hint = k;
                        let section = &traced.sections[k];
                        let room = (section.start + section.size - address) / 4;
                        match bound[k] {
                            Some(unit) => (unit, address - section.start, room),
                            None => {
                                window.unbound += u64::from(left.min(room));
                                (FIXED, address, room)
                            }
                        }
                    }
                    None => {
                        let next = traced
                            .sections
                            .get(traced.sections.partition_point(|s| s.start <= address))
                            .map_or(u32::MAX, |s| s.start);
                        (FIXED, address, (next.saturating_sub(address) / 4).max(1))
                    }
                };
                let in_chunk = ((1u32 << CHUNK_SHIFT) - (offset & ((1 << CHUNK_SHIFT) - 1))) / 4;
                let n = left.min(room).min(in_chunk);
                let from = ((offset & ((1 << CHUNK_SHIFT) - 1)) / 4) as u16;
                reducer.touch(
                    Chunk {
                        unit,
                        index: offset >> CHUNK_SHIFT,
                    },
                    from,
                    from + n as u16 - 1,
                    n == left,
                );
                if unit != FIXED {
                    *executed.entry(unit).or_default() += u64::from(n);
                }
                address += 4 * n;
                left -= n;
            }
        })?;
        reducer.end_segment();
        window.ids = std::mem::take(&mut reducer.ids);
        for (&unit, &count) in &executed {
            *self.executed.entry(unit).or_default() += count;
        }
        window.executed = executed;
        self.windows.push(window);
        Ok(())
    }

    /// Distinct segments over every window.
    pub fn segments(&self) -> usize {
        self.segments.len()
    }

    /// Segment ids over every window.
    pub fn length(&self) -> usize {
        self.windows.iter().map(|window| window.ids.len()).sum()
    }

    /// Each chunk's address for the sections at `address`.
    fn bases(&self, address: &[u32]) -> Vec<u32> {
        self.chunks
            .iter()
            .map(|chunk| match chunk.unit {
                FIXED => chunk.index << CHUNK_SHIFT,
                unit => address[unit as usize] + (chunk.index << CHUNK_SHIFT),
            })
            .collect()
    }

    /// Replay one window with the link's sections at `address`.
    pub fn simulate(&self, window: usize, address: &[u32]) -> Outcome {
        self.replay(window, &self.bases(address), None)
    }

    /// [`Self::simulate`], charging every stall to the unit that missed.
    pub fn attribute(&self, window: usize, address: &[u32], units: usize) -> (Outcome, Vec<u64>) {
        let mut charged = vec![0u64; units + 1];
        let outcome = self.replay(window, &self.bases(address), Some(&mut charged));
        (outcome, charged)
    }

    fn replay(&self, window: usize, bases: &[u32], mut charge: Option<&mut Vec<u64>>) -> Outcome {
        let mut tag = [u32::MAX; SETS];
        let mut valid = [0u8; SETS];
        let mut outcome = Outcome::default();
        for &id in &self.windows[window].ids {
            let segment = self.segments[id as usize];
            let base = bases[segment.chunk as usize];
            let first = segment.first as usize;
            let mut stalls = 0u64;
            let mut misses = 0u64;
            for &(from, to) in &self.intervals[first..first + segment.len as usize] {
                let leaves = to & LEAVES != 0;
                let to = to & !LEAVES;
                let mut address = base + 4 * u32::from(from);
                let last = base + 4 * u32::from(to);
                loop {
                    let line = address >> 4;
                    let set = (line as usize) & (SETS - 1);
                    let word = (address >> 2) & 3;
                    // Words of this line the interval runs, and whether
                    // execution jumps away before the line ends.
                    let ran = (last.min(address | 0xc) - address) / 4 + 1;
                    let left = leaves && last < (address | 0xc);
                    if tag[set] == line {
                        if valid[set] >> word & 1 == 0 {
                            // The tag matches but the word was never filled:
                            // the whole line refills, from word 0.
                            valid[set] = 0xf;
                            stalls += refill(word, true, ran, left);
                            misses += 1;
                        }
                    } else {
                        tag[set] = line;
                        valid[set] = (0xf << word) & 0xf;
                        stalls += refill(word, false, ran, left);
                        misses += 1;
                    }
                    let next = (line + 1) << 4;
                    if next > last {
                        break;
                    }
                    address = next;
                }
            }
            outcome.stalls += stalls;
            outcome.misses += misses;
            if let Some(charge) = charge.as_deref_mut() {
                let unit = self.chunks[segment.chunk as usize].unit;
                let at = if unit == FIXED {
                    charge.len() - 1
                } else {
                    unit as usize
                };
                charge[at] += stalls;
            }
        }
        outcome
    }

    /// The first and last word each chunk ever runs, over every window.
    fn extents(&self) -> Vec<(u32, u32)> {
        let mut extent = vec![(u32::MAX, 0u32); self.chunks.len()];
        for segment in &self.segments {
            let first = segment.first as usize;
            for &(from, to) in &self.intervals[first..first + segment.len as usize] {
                let slot = &mut extent[segment.chunk as usize];
                slot.0 = slot.0.min(u32::from(from));
                slot.1 = slot.1.max(u32::from(to & !LEAVES));
            }
        }
        extent
    }

    /// The stall cycles of one window in the sets marked in `sets` alone.
    /// A direct-mapped set sees only its own lines, so this is exactly that
    /// part of [`Self::replay`] when `positions` (in order) are every place in
    /// the window where a chunk with a line in those sets runs.
    fn replay_sets(
        &self,
        window: usize,
        bases: &[u32],
        sets: &[bool; SETS],
        positions: impl Iterator<Item = usize>,
    ) -> u64 {
        let ids = &self.windows[window].ids;
        let mut tag = [u32::MAX; SETS];
        let mut valid = [0u8; SETS];
        let mut stalls = 0u64;
        for at in positions {
            let segment = self.segments[ids[at] as usize];
            let base = bases[segment.chunk as usize];
            let first = segment.first as usize;
            for &(from, to) in &self.intervals[first..first + segment.len as usize] {
                let leaves = to & LEAVES != 0;
                let to = to & !LEAVES;
                let mut address = base + 4 * u32::from(from);
                let last = base + 4 * u32::from(to);
                loop {
                    let line = address >> 4;
                    let set = (line as usize) & (SETS - 1);
                    if sets[set] {
                        let word = (address >> 2) & 3;
                        let ran = (last.min(address | 0xc) - address) / 4 + 1;
                        let left = leaves && last < (address | 0xc);
                        if tag[set] == line {
                            if valid[set] >> word & 1 == 0 {
                                valid[set] = 0xf;
                                stalls += refill(word, true, ran, left);
                            }
                        } else {
                            tag[set] = line;
                            valid[set] = (0xf << word) & 0xf;
                            stalls += refill(word, false, ran, left);
                        }
                    }
                    let next = (line + 1) << 4;
                    if next > last {
                        break;
                    }
                    address = next;
                }
            }
        }
        stalls
    }
}

/// What a replay cost.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// I-cache refill stall cycles.
    pub stalls: u64,
    pub misses: u64,
}

/// A small deterministic generator (xorshift64*), so a seed names a search.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// A placement the search works on: the hot sections in link order, each
/// with the offset in the cache it should start at. [`Plan::listing`] lays
/// them out, filling the gap in front of each with cold sections.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub hot: Vec<usize>,
    /// Per entry of `hot`: the wanted start, modulo the cache size.
    pub target: Vec<u32>,
}

/// The cold sections a gap may take, largest first.
pub struct Pool(Vec<usize>);

impl Pool {
    /// Movable sections the traces never ran.
    pub fn new(link: &Link, traces: &Traces) -> Self {
        let mut cold: Vec<usize> = (0..link.sections.len())
            .filter(|&k| link.movable(k) && !traces.executed.contains_key(&(k as u32)))
            .collect();
        cold.sort_by_key(|&k| (std::cmp::Reverse(link.sections[k].size), k));
        Self(cold)
    }
}

impl Plan {
    /// The sections to list, hot ones with cold fill in front of each, and
    /// the bytes of fill used.
    pub fn listing(&self, link: &Link, pool: &Pool) -> (Vec<usize>, u32) {
        let mut used = vec![false; pool.0.len()];
        let mut listed = Vec::with_capacity(self.hot.len() * 2);
        let mut at = link.start;
        let mut gap = 0;
        for (&f, &target) in self.hot.iter().zip(&self.target) {
            let align = link.align[f];
            let lands = |at: u32| at.next_multiple_of(align);
            let want = target.wrapping_sub(lands(at)) % CACHE;
            let mut filled = 0;
            if want > 0 {
                for (slot, &k) in pool.0.iter().enumerate() {
                    if used[slot] {
                        continue;
                    }
                    let k_at = (at + filled).next_multiple_of(link.align[k]);
                    let end = k_at + link.sections[k].size;
                    if end - at <= want {
                        used[slot] = true;
                        listed.push(k);
                        filled = end - at;
                        if want - filled < 4 {
                            break;
                        }
                    }
                }
            }
            gap += filled;
            at = lands(at + filled);
            listed.push(f);
            at += link.sections[f].size;
        }
        (listed, gap)
    }
}

/// The search's settings.
#[derive(Clone, Debug)]
pub struct Settings {
    pub seed: u64,
    /// Rounds of the search; each evaluates `batch` candidates.
    pub rounds: usize,
    pub batch: usize,
    pub threads: usize,
    /// Print the objective every 50 rounds.
    pub progress: bool,
}

/// A search's result and what it cost.
pub struct Found {
    pub listed: Vec<usize>,
    /// The objective (1.0 per window at the start) before and after.
    pub start: f64,
    pub end: f64,
    /// Stall cycles per window at the start and at the end.
    pub start_stalls: Vec<u64>,
    pub end_stalls: Vec<u64>,
    pub evaluations: usize,
    pub accepted: usize,
}

/// Scores candidates against the current placement by replaying only the
/// sets the moved code covers, before and after the move: every other set
/// sees the same fetches in the same order, so its stalls cannot change.
struct Delta<'a> {
    traces: &'a Traces,
    extent: Vec<(u32, u32)>,
    /// Per window and chunk, the positions of its segments in the window.
    occurs: Vec<Vec<Vec<u32>>>,
    bases: Vec<u32>,
    stalls: Vec<u64>,
}

impl<'a> Delta<'a> {
    fn new(traces: &'a Traces, address: &[u32], stalls: Vec<u64>) -> Self {
        let occurs = traces
            .windows
            .iter()
            .map(|window| {
                let mut occurs = vec![Vec::new(); traces.chunks.len()];
                for (at, &id) in window.ids.iter().enumerate() {
                    occurs[traces.segments[id as usize].chunk as usize].push(at as u32);
                }
                occurs
            })
            .collect();
        Self {
            traces,
            extent: traces.extents(),
            occurs,
            bases: traces.bases(address),
            stalls,
        }
    }

    /// The lines chunk `c` runs at `base`, first and last.
    fn lines(&self, c: usize, base: u32) -> Option<(u32, u32)> {
        let (from, to) = self.extent[c];
        (from <= to).then(|| ((base + 4 * from) >> 4, (base + 4 * to) >> 4))
    }

    fn touches(&self, c: usize, base: u32, sets: &[bool; SETS]) -> bool {
        self.lines(c, base).is_some_and(|(first, last)| {
            last - first >= SETS as u32 - 1
                || (first..=last).any(|line| sets[(line as usize) & (SETS - 1)])
        })
    }

    /// Each window's stall cycles with the sections at `address`.
    fn stalls(&self, address: &[u32]) -> Vec<u64> {
        let bases = self.traces.bases(address);
        let mut sets = [false; SETS];
        let mut mark = |lines: Option<(u32, u32)>| {
            if let Some((first, last)) = lines {
                for line in first..=last.min(first + SETS as u32 - 1) {
                    sets[(line as usize) & (SETS - 1)] = true;
                }
            }
        };
        let mut moved = false;
        for (c, (&new, &old)) in bases.iter().zip(&self.bases).enumerate() {
            if new != old {
                mark(self.lines(c, old));
                mark(self.lines(c, new));
                moved = true;
            }
        }
        if !moved {
            return self.stalls.clone();
        }
        if sets.iter().filter(|&&set| set).count() > SETS * 3 / 4 {
            return (0..self.traces.windows.len())
                .map(|w| self.traces.replay(w, &bases, None).stalls)
                .collect();
        }
        let live_old: Vec<bool> = (0..bases.len())
            .map(|c| self.touches(c, self.bases[c], &sets))
            .collect();
        let live_new: Vec<bool> = (0..bases.len())
            .map(|c| self.touches(c, bases[c], &sets))
            .collect();
        (0..self.traces.windows.len())
            .map(|w| {
                let before =
                    self.traces
                        .replay_sets(w, &self.bases, &sets, self.positions(w, &live_old));
                let after = self
                    .traces
                    .replay_sets(w, &bases, &sets, self.positions(w, &live_new));
                self.stalls[w] + after - before
            })
            .collect()
    }

    /// Every position in window `w` where a `live` chunk runs, in order.
    fn positions(&self, w: usize, live: &[bool]) -> impl Iterator<Item = usize> {
        let length = self.traces.windows[w].ids.len();
        let mut marks = vec![0u64; length.div_ceil(64)];
        for (c, _) in live.iter().enumerate().filter(|(_, &live)| live) {
            for &at in &self.occurs[w][c] {
                marks[at as usize >> 6] |= 1 << (at & 63);
            }
        }
        marks.into_iter().enumerate().flat_map(|(word, mut bits)| {
            std::iter::from_fn(move || {
                (bits != 0).then(|| {
                    let bit = bits.trailing_zeros() as usize;
                    bits &= bits - 1;
                    word * 64 + bit
                })
            })
        })
    }

    fn accept(&mut self, address: &[u32], stalls: Vec<u64>) {
        self.bases = self.traces.bases(address);
        self.stalls = stalls;
    }
}

/// Total weighted, normalised stalls.
fn objective(traces: &Traces, stalls: &[u64], baseline: &[u64]) -> f64 {
    stalls
        .iter()
        .zip(baseline)
        .zip(&traces.windows)
        .map(|((&s, &b), window)| window.weight * s as f64 / b.max(1) as f64)
        .sum()
}

/// A candidate move: new targets for some hot entries.
fn mutate(plan: &Plan, rng: &mut Rng, pick: &[f64]) -> Plan {
    let mut next = plan.clone();
    let choose = |rng: &mut Rng| -> usize {
        let total: f64 = pick.iter().sum();
        if total <= 0.0 {
            return rng.below(plan.hot.len());
        }
        let mut x = (rng.next() >> 11) as f64 / (1u64 << 53) as f64 * total;
        for (at, &weight) in pick.iter().enumerate() {
            if x < weight {
                return at;
            }
            x -= weight;
        }
        pick.len() - 1
    };
    let f = choose(rng);
    match rng.below(4) {
        // Anywhere in the cache, on a word.
        0 => next.target[f] = 4 * rng.below((CACHE / 4) as usize) as u32,
        // A few lines either way.
        1 => {
            let lines = rng.below(33) as i32 - 16;
            next.target[f] = (next.target[f] as i32 + 16 * lines).rem_euclid(CACHE as i32) as u32;
        }
        // A word or three either way: which words share a line.
        2 => {
            let words = rng.below(7) as i32 - 3;
            next.target[f] = (next.target[f] as i32 + 4 * words).rem_euclid(CACHE as i32) as u32;
        }
        // Trade offsets with another hot function.
        _ => {
            let g = choose(rng);
            next.target.swap(f, g);
        }
    }
    next
}

/// Search placements from `start`: each round mutates the current plan
/// `batch` times (functions that stall more are picked more often), scores
/// every candidate by replaying the traces, and keeps the best when it
/// lowers the objective. Candidates are drawn in sequence and ties go to the
/// first, so the result depends only on the inputs and the seed, never on
/// the thread count.
pub fn search(
    link: &Link,
    traces: &Traces,
    pool: &Pool,
    start: Plan,
    settings: &Settings,
) -> Found {
    let place = |plan: &Plan| -> (Vec<usize>, Vec<u32>) {
        let (listed, _) = plan.listing(link, pool);
        let address = link.realize(&listed);
        (listed, address)
    };
    let (mut listed, address) = place(&start);
    let baseline: Vec<u64> = (0..traces.windows.len())
        .map(|w| traces.simulate(w, &address).stalls)
        .collect();
    let mut stalls = baseline.clone();
    let mut current = objective(traces, &stalls, &baseline);
    let mut delta = Delta::new(traces, &address, stalls.clone());
    let start_score = current;
    let start_stalls = stalls.clone();
    let mut plan = start;
    let mut rng = Rng::new(settings.seed);
    let mut evaluations = traces.windows.len();
    let mut accepted = 0;
    let began = std::time::Instant::now();
    let index_of: HashMap<usize, usize> =
        plan.hot.iter().enumerate().map(|(i, &k)| (k, i)).collect();
    let mut pick = vec![0.0; plan.hot.len()];
    for round in 0..settings.rounds {
        if round % 16 == 0 {
            // Re-weight the picks by where the stalls are now.
            let address = link.realize(&listed);
            pick.iter_mut().for_each(|weight| *weight = 0.0);
            for (w, (window, &base)) in traces.windows.iter().zip(&baseline).enumerate() {
                let (_, charged) = traces.attribute(w, &address, link.sections.len());
                for (k, &cycles) in charged.iter().enumerate() {
                    if let Some(&i) = index_of.get(&k) {
                        pick[i] += window.weight * cycles as f64 / base.max(1) as f64;
                    }
                }
            }
            // Every function keeps some chance: a quiet one may be the one
            // in the way.
            let floor = pick.iter().sum::<f64>() / pick.len().max(1) as f64 * 0.05;
            pick.iter_mut().for_each(|weight| *weight += floor);
        }
        let candidates: Vec<Plan> = (0..settings.batch)
            .map(|_| mutate(&plan, &mut rng, &pick))
            .collect();
        let results = evaluate(
            traces,
            &delta,
            &candidates,
            &place,
            &baseline,
            settings.threads,
        );
        evaluations += candidates.len() * traces.windows.len();
        let best = results
            .iter()
            .enumerate()
            .min_by(|a, b| a.1 .0.total_cmp(&b.1 .0).then(a.0.cmp(&b.0)));
        if let Some((at, (cost, ..))) = best {
            if *cost < current {
                current = *cost;
                plan = candidates[at].clone();
                listed = results[at].2.clone();
                stalls = results[at].1.clone();
                delta.accept(&results[at].3, stalls.clone());
                accepted += 1;
            }
        }
        if settings.progress && (round + 1) % 50 == 0 {
            println!(
                "  round {}: objective {current:.4}, {accepted} moves kept, {:.0} s",
                round + 1,
                began.elapsed().as_secs_f64()
            );
        }
    }
    Found {
        listed,
        start: start_score,
        end: current,
        start_stalls,
        end_stalls: stalls,
        evaluations,
        accepted,
    }
}

/// Lays a plan out: the listed sections and every section's address.
type Place<'a> = dyn Fn(&Plan) -> (Vec<usize>, Vec<u32>) + Sync + 'a;

type Scored = (f64, Vec<u64>, Vec<usize>, Vec<u32>);

fn evaluate(
    traces: &Traces,
    delta: &Delta<'_>,
    candidates: &[Plan],
    place: &Place<'_>,
    baseline: &[u64],
    threads: usize,
) -> Vec<Scored> {
    let threads = threads.clamp(1, candidates.len().max(1));
    let per = candidates.len().div_ceil(threads);
    let mut results: Vec<Option<Scored>> = vec![None; candidates.len()];
    std::thread::scope(|scope| {
        for (chunk, out) in candidates.chunks(per).zip(results.chunks_mut(per)) {
            scope.spawn(move || {
                for (plan, slot) in chunk.iter().zip(out.iter_mut()) {
                    let (listed, address) = place(plan);
                    let stalls = delta.stalls(&address);
                    let cost = objective(traces, &stalls, baseline);
                    *slot = Some((cost, stalls, listed, address));
                }
            });
        }
    });
    results
        .into_iter()
        .map(|result| result.expect("every candidate is scored"))
        .collect()
}

/// The plan a link already has: its traced sections in address order, each
/// targeting where it is now.
pub fn plan_of(link: &Link, traces: &Traces, address: &[u32]) -> Plan {
    let mut hot: Vec<usize> = (0..link.sections.len())
        .filter(|&k| link.movable(k) && traces.executed.contains_key(&(k as u32)))
        .collect();
    hot.sort_by_key(|&k| (address[k], k));
    let target = hot.iter().map(|&k| address[k] % CACHE).collect();
    Plan { hot, target }
}

/// The addresses an ordering file gives the link.
pub fn addresses_of_order(link: &Link, order: &str) -> Result<Vec<u32>> {
    let index: HashMap<&str, usize> = link
        .sections
        .iter()
        .enumerate()
        .filter_map(|(k, section)| Some((section.symbol.as_deref()?, k)))
        .collect();
    let listed = order
        .lines()
        .filter(|line| !line.is_empty())
        .map(|symbol| {
            index
                .get(symbol)
                .copied()
                .ok_or_else(|| format!("{symbol} from the order is not in the link"))
        })
        .collect::<std::result::Result<Vec<usize>, String>>()?;
    Ok(link.realize(&listed))
}

/// The addresses another link of the same code gives this link's sections,
/// by portable name and code hash; `None` for a section it does not have.
pub fn addresses_of_link(link: &Link, other: &Link) -> Vec<Option<u32>> {
    let bound = bind_sections(link, other);
    bound
        .iter()
        .map(|at| at.map(|at| other.sections[at as usize].start))
        .collect()
}

/// The placement a layout profile carries after a search: every listed
/// section with its code hash, size and the address the search scored.
pub fn recorded(link: &Link, listed: &[usize]) -> layout::Recorded {
    let address = link.realize(listed);
    layout::Recorded {
        start: link.start,
        entries: listed
            .iter()
            .map(|&k| layout::RecordedEntry {
                hash: link.hash(k).unwrap_or(0),
                words: link.sections[k].size / 4,
                address: address[k],
                key: link.keys[k]
                    .clone()
                    .expect("only sections with a portable name are listed"),
            })
            .collect(),
    }
}

pub const USAGE: &str = "\
       psoxide-pgo search <in.layout> <link.map> <image.exe> <linker-script> <out.layout>
                          (--trace NAME MAP EXE TRACE)... [--weight NAME=W]... [--seed N]
                          [--rounds N] [--batch N] [--threads N] [--start greedy|link]
       psoxide-pgo icache <link.map> <image.exe> <linker-script> (--trace NAME MAP EXE TRACE)...
                          [--order FILE]... [--at MAP EXE]... [--layout FILE]... [--functions]";

/// One `--trace`: a window's name, the build it was recorded on and the log.
struct TraceArg {
    name: String,
    map: String,
    exe: String,
    trace: String,
}

struct Args {
    positional: Vec<String>,
    traces: Vec<TraceArg>,
    weights: HashMap<String, f64>,
    orders: Vec<String>,
    ats: Vec<(String, String)>,
    layouts: Vec<String>,
    settings: Settings,
    start: String,
    functions: bool,
}

fn parse(args: &[String]) -> Result<Args> {
    let mut parsed = Args {
        positional: Vec::new(),
        traces: Vec::new(),
        weights: HashMap::new(),
        orders: Vec::new(),
        ats: Vec::new(),
        layouts: Vec::new(),
        settings: Settings {
            seed: 1,
            rounds: 2000,
            batch: 24,
            threads: std::thread::available_parallelism().map_or(4, |n| n.get()),
            progress: true,
        },
        start: "greedy".to_string(),
        functions: false,
    };
    let mut at = 0;
    let take = |at: &mut usize, count: usize, flag: &str| -> Result<Vec<String>> {
        let values = args
            .get(*at + 1..*at + 1 + count)
            .ok_or_else(|| format!("{flag} wants {count} value(s)"))?
            .to_vec();
        *at += count;
        Ok(values)
    };
    let number = |value: &str, flag: &str| -> Result<usize> {
        value
            .parse()
            .map_err(|_| format!("{flag} wants a number, not {value:?}").into())
    };
    while at < args.len() {
        let flag = args[at].as_str();
        match flag {
            "--trace" => {
                let values = take(&mut at, 4, flag)?;
                parsed.traces.push(TraceArg {
                    name: values[0].clone(),
                    map: values[1].clone(),
                    exe: values[2].clone(),
                    trace: values[3].clone(),
                });
            }
            "--weight" => {
                let value = take(&mut at, 1, flag)?.remove(0);
                let (name, weight) = value.split_once('=').ok_or("--weight wants NAME=WEIGHT")?;
                let weight: f64 = weight.parse().map_err(|_| "--weight wants a number")?;
                parsed.weights.insert(name.to_string(), weight);
            }
            "--order" => parsed.orders.push(take(&mut at, 1, flag)?.remove(0)),
            "--layout" => parsed.layouts.push(take(&mut at, 1, flag)?.remove(0)),
            "--at" => {
                let values = take(&mut at, 2, flag)?;
                parsed.ats.push((values[0].clone(), values[1].clone()));
            }
            "--seed" => {
                parsed.settings.seed = number(&take(&mut at, 1, flag)?[0], flag)? as u64;
            }
            "--rounds" => parsed.settings.rounds = number(&take(&mut at, 1, flag)?[0], flag)?,
            "--batch" => parsed.settings.batch = number(&take(&mut at, 1, flag)?[0], flag)?.max(1),
            "--threads" => {
                parsed.settings.threads = number(&take(&mut at, 1, flag)?[0], flag)?.max(1);
            }
            "--start" => parsed.start = take(&mut at, 1, flag)?.remove(0),
            "--functions" => parsed.functions = true,
            _ if flag.starts_with("--") => return Err(format!("unknown option {flag}").into()),
            _ => parsed.positional.push(args[at].clone()),
        }
        at += 1;
    }
    if parsed.traces.is_empty() {
        return Err("at least one --trace NAME MAP EXE TRACE is needed".into());
    }
    Ok(parsed)
}

fn load_link(map: &str, exe: &str, patterns: &[String]) -> Result<Link> {
    let text = std::fs::read_to_string(map).map_err(|error| format!("{map}: {error}"))?;
    let image = Image::parse(&std::fs::read(exe).map_err(|error| format!("{exe}: {error}"))?)?;
    Ok(Link::new(&text, image, patterns))
}

/// Read every `--trace` and bind it to `link`.
fn load_traces(args: &Args, link: &Link, patterns: &[String]) -> Result<Traces> {
    let mut traces = Traces::default();
    let mut builds: HashMap<(String, String), Link> = HashMap::new();
    for spec in &args.traces {
        let key = (spec.map.clone(), spec.exe.clone());
        if !builds.contains_key(&key) {
            builds.insert(key.clone(), load_link(&spec.map, &spec.exe, patterns)?);
        }
        let traced = &builds[&key];
        let bytes =
            std::fs::read(&spec.trace).map_err(|error| format!("{}: {error}", spec.trace))?;
        let weight = args.weights.get(&spec.name).copied().unwrap_or(1.0);
        traces.read(&spec.name, weight, &bytes, traced, link)?;
        let window = traces.windows.last().expect("read adds a window");
        println!(
            "trace {}: {} instructions ({} uncached, {} in code that did not bind), {} route \
             ticks; {} segment ids so far, {} distinct",
            window.name,
            window.instructions,
            window.uncached,
            window.unbound,
            window.ticks,
            traces.length(),
            traces.segments()
        );
        if window.unbound * 100 > window.instructions {
            return Err(format!(
                "{}: {} of {} instructions ran in functions that are not in this link with \
                 the same code; trace the build being placed",
                window.name, window.unbound, window.instructions
            )
            .into());
        }
    }
    Ok(traces)
}

fn stalls_line(traces: &Traces, address: &[u32]) -> String {
    let mut line = String::new();
    let mut total = 0;
    for (w, window) in traces.windows.iter().enumerate() {
        let outcome = traces.simulate(w, address);
        total += outcome.stalls;
        let _ = write!(line, " {}={}", window.name, outcome.stalls);
    }
    format!("total={total}{line}")
}

/// `icache`: the stall cycles each window's trace would take at the link's
/// own addresses, at the addresses an ordering file gives it, at those of
/// another link of the same code, or at a layout's recorded placement.
fn icache(args: &Args) -> Result<()> {
    let [map, exe, script] = args.positional.as_slice() else {
        return Err(format!("usage:\n{USAGE}").into());
    };
    let patterns = layout::fixed_patterns(&std::fs::read_to_string(script)?);
    let link = load_link(map, exe, &patterns)?;
    let traces = load_traces(args, &link, &patterns)?;
    if args.functions {
        // Instructions per function and window, tab-separated, hottest first.
        let mut rows: Vec<(u64, usize)> = (0..link.sections.len())
            .map(|k| (traces.executed.get(&(k as u32)).copied().unwrap_or(0), k))
            .filter(|&(count, _)| count > 0)
            .collect();
        rows.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        for (_, k) in rows {
            let counts: Vec<String> = traces
                .windows
                .iter()
                .map(|window| {
                    window
                        .executed
                        .get(&(k as u32))
                        .copied()
                        .unwrap_or(0)
                        .to_string()
                })
                .collect();
            let name = link.keys[k].as_deref().unwrap_or(&link.sections[k].name);
            println!("function\t{}\t{name}", counts.join("\t"));
        }
    }
    let linked: Vec<u32> = link.sections.iter().map(|section| section.start).collect();
    println!("as linked: {}", stalls_line(&traces, &linked));
    for order in &args.orders {
        let address = addresses_of_order(&link, &std::fs::read_to_string(order)?)?;
        println!("order {order}: {}", stalls_line(&traces, &address));
    }
    for (other_map, other_exe) in &args.ats {
        let other = load_link(other_map, other_exe, &patterns)?;
        let found = addresses_of_link(&link, &other);
        let missing = found.iter().filter(|at| at.is_none()).count();
        let address: Vec<u32> = found
            .iter()
            .zip(&linked)
            .map(|(at, &here)| at.unwrap_or(here))
            .collect();
        println!(
            "at {other_map} ({missing} sections not there): {}",
            stalls_line(&traces, &address)
        );
    }
    for path in &args.layouts {
        let profile = layout::Layout::parse(&std::fs::read_to_string(path)?)?;
        let placement = match &profile.placement {
            Some(recorded) => {
                layout::recorded_order(recorded, &link.sections, &link.image, &patterns)?
            }
            None => {
                let bound = layout::bind(&profile, &link.sections, &link.image);
                layout::place(&link.sections, &link.image, &bound, &patterns)
            }
        };
        let address = link.realize(&placement.order);
        println!("layout {path}: {}", stalls_line(&traces, &address));
    }
    Ok(())
}

/// `search`: place the layout profile's functions by replaying the traces,
/// and write the profile with the placement found.
fn run_search(args: &Args) -> Result<()> {
    let [input, map, exe, script, output] = args.positional.as_slice() else {
        return Err(format!("usage:\n{USAGE}").into());
    };
    let text = std::fs::read_to_string(input)?;
    let mut profile = layout::Layout::parse(&text)?;
    let patterns = layout::fixed_patterns(&std::fs::read_to_string(script)?);
    let link = load_link(map, exe, &patterns)?;
    let traces = load_traces(args, &link, &patterns)?;
    let bound = layout::bind(&profile, &link.sections, &link.image);
    println!("layout profile: {}", bound.coverage);
    bound.coverage.gate()?;
    let address = match args.start.as_str() {
        "greedy" => {
            let placement = layout::place(&link.sections, &link.image, &bound, &patterns);
            link.realize(&placement.order)
        }
        "link" => link.sections.iter().map(|section| section.start).collect(),
        "recorded" => {
            let recorded = profile
                .placement
                .as_ref()
                .ok_or("--start recorded needs a layout with a recorded placement")?;
            let placement =
                layout::recorded_order(recorded, &link.sections, &link.image, &patterns)?;
            link.realize(&placement.order)
        }
        other => return Err(format!("--start wants greedy, link or recorded, not {other}").into()),
    };
    let pool = Pool::new(&link, &traces);
    let start = plan_of(&link, &traces, &address);
    println!(
        "start ({}): {}; {} traced functions to place ({} B), {} cold ones to fill gaps with \
         ({} B), {} B of them used",
        args.start,
        stalls_line(&traces, &address),
        start.hot.len(),
        start
            .hot
            .iter()
            .map(|&k| link.sections[k].size)
            .sum::<u32>(),
        pool.0.len(),
        pool.0.iter().map(|&k| link.sections[k].size).sum::<u32>(),
        start.listing(&link, &pool).1
    );
    let began = std::time::Instant::now();
    let found = search(&link, &traces, &pool, start, &args.settings);
    let settings = &args.settings;
    let summary = format!(
        "seed {} rounds {} batch {}: {} window replays, {} moves kept, objective {:.4} -> {:.4}",
        settings.seed,
        settings.rounds,
        settings.batch,
        found.evaluations,
        found.accepted,
        found.start,
        found.end,
    );
    // The time goes to the terminal only: the file must be the same on every run.
    println!(
        "search: {summary} in {:.0} s",
        began.elapsed().as_secs_f64()
    );
    for (w, window) in traces.windows.iter().enumerate() {
        println!(
            "  {}: {} -> {} stall cycles",
            window.name, found.start_stalls[w], found.end_stalls[w]
        );
    }
    profile.placement = Some(recorded(&link, &found.listed));
    // The profile's own lines stay as they were; the placement follows them,
    // with what made it.
    let mut out = text
        .lines()
        .take_while(|line| {
            !line.starts_with(&format!("{} ", START_TAG_TEXT)) && !line.starts_with("# placement")
        })
        .collect::<Vec<_>>()
        .join("\n");
    out.push('\n');
    let _ = writeln!(out, "# placement: psoxide-pgo search, {summary}");
    let names: Vec<String> = traces
        .windows
        .iter()
        .map(|window| format!("{} (weight {})", window.name, window.weight))
        .collect();
    let _ = writeln!(out, "# placement windows: {}", names.join(", "));
    out.push_str(&profile.placement.as_ref().expect("set above").to_text());
    std::fs::write(output, out)?;
    println!("wrote {output}");
    Ok(())
}

const START_TAG_TEXT: &str = "s";

/// `search` and `icache`.
pub fn main(mode: &str, args: &[String]) -> Result<()> {
    let args = parse(args)?;
    match mode {
        "search" => run_search(&args),
        "icache" => icache(&args),
        _ => Err(format!("unknown mode {mode}").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(events: &[Event]) -> Vec<u8> {
        let mut out = TRACE_MAGIC.to_vec();
        let mut end: i64 = 0;
        let varint = |out: &mut Vec<u8>, mut value: u64| loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                break;
            }
            out.push(byte | 0x80);
        };
        for event in events {
            match *event {
                Event::Tick => out.push(0),
                Event::Run { pc, words } => {
                    let start = i64::from(pc >> 2);
                    let delta = start - end;
                    varint(&mut out, (((delta << 1) ^ (delta >> 63)) as u64) + 1);
                    varint(&mut out, u64::from(words));
                    end = start + i64::from(words);
                }
            }
        }
        out
    }

    #[test]
    fn traces_decode_runs_and_ticks() {
        let events = vec![
            Event::Run {
                pc: 0x8001_0000,
                words: 3,
            },
            Event::Run {
                pc: 0x8000_0080,
                words: 2,
            },
            Event::Tick,
            Event::Run {
                pc: 0x8000_0088,
                words: 1,
            },
            Event::Run {
                pc: 0xbfc0_0000,
                words: 1,
            },
        ];
        assert_eq!(decode(&encode(&events)).unwrap(), events);
        assert!(decode(b"nope").is_err());
    }

    /// A link of `sizes` byte functions from 0x80010000, named f0, f1, ...
    fn link(sizes: &[u32]) -> Link {
        let mut map = String::from("     VMA      LMA     Size Align Out     In      Symbol\n");
        let total: u32 = sizes.iter().sum();
        let _ = writeln!(map, "80010000 80010000 {total:8x}     4 .text");
        let mut at = 0x8001_0000u32;
        let mut body = vec![0u8; 0x800];
        body[..8].copy_from_slice(b"PS-X EXE");
        body[0x18..0x1c].copy_from_slice(&at.to_le_bytes());
        for (index, &size) in sizes.iter().enumerate() {
            let _ = writeln!(
                map,
                "{at:08x} {at:08x} {size:8x}     4         a.o:(.text.f{index})"
            );
            let _ = writeln!(
                map,
                "{at:08x} {at:08x}        0     1                 f{index}"
            );
            // Distinct code per function, so every hash differs.
            for word in 0..size / 4 {
                body.extend_from_slice(
                    &(0x2400_0000u32 | (index as u32) << 8 | word).to_le_bytes(),
                );
            }
            at += size;
        }
        Link::new(&map, Image::parse(&body).unwrap(), &[])
    }

    fn traces_for(link: &Link, events: &[Event]) -> Traces {
        let mut traces = Traces::default();
        traces.read("t", 1.0, &encode(events), link, link).unwrap();
        traces
    }

    #[test]
    fn a_loop_inside_one_function_misses_once_per_line() {
        let link = link(&[64]);
        let body = Event::Run {
            pc: 0x8001_0000,
            words: 16,
        };
        let traces = traces_for(&link, &[body; 100]);
        // One segment repeated would still be one segment: the loop never
        // leaves its chunk.
        assert_eq!(traces.windows[0].ids.len(), 1);
        let outcome = traces.simulate(0, &link.realize(&[]));
        assert_eq!(outcome.misses, 4);
        assert_eq!(outcome.stalls, 4 * 5);
    }

    #[test]
    fn two_functions_in_one_set_evict_each_other_and_apart_do_not() {
        // f0 calls f1 50 times: f0's line, f1's line, back to f0's.
        let link = link(&[16, 4096, 16]);
        let call = [
            Event::Run {
                pc: 0x8001_0000,
                words: 4,
            },
            Event::Run {
                pc: 0x8001_1010,
                words: 4,
            },
        ];
        let events: Vec<Event> = call.iter().copied().cycle().take(100).collect();
        let traces = traces_for(&link, &events);
        // As linked, f2 starts 4 KB + 16 after f0: different sets.
        let apart = traces.simulate(0, &link.realize(&[]));
        assert_eq!(apart.misses, 2);
        // f2 listed first lands at f0's old place and f0 4 KB + 16 later:
        // still apart. Put f2 exactly 4 KB after f0 instead.
        let mut address = link.realize(&[]);
        address[2] = address[0] + 4096;
        let together = traces.simulate(0, &address);
        assert_eq!(together.misses, 100);
        assert_eq!(together.stalls, 100 * 5);
    }

    #[test]
    fn entering_a_line_late_then_early_refills_it_whole() {
        let link = link(&[16]);
        // Words 2..3, then 0..3: the tag matches but word 0 was never filled.
        let traces = traces_for(
            &link,
            &[
                Event::Run {
                    pc: 0x8001_0008,
                    words: 2,
                },
                Event::Tick,
                Event::Run {
                    pc: 0x8001_0000,
                    words: 4,
                },
            ],
        );
        let outcome = traces.simulate(0, &link.realize(&[]));
        assert_eq!(outcome.misses, 2);
        // A mid-line tag miss: 3 to the first word, 1 for the second. Then
        // the whole line from word 0: 2, and 1 for word 1.
        assert_eq!(outcome.stalls, (3 + 1) + (2 + 1));
    }

    #[test]
    fn uncached_fetches_do_not_touch_the_cache() {
        let link = link(&[16]);
        let traces = traces_for(
            &link,
            &[
                Event::Run {
                    pc: 0x8001_0000,
                    words: 4,
                },
                Event::Run {
                    pc: 0xbfc0_0000,
                    words: 4,
                },
                Event::Run {
                    pc: 0x8001_0000,
                    words: 4,
                },
            ],
        );
        assert_eq!(traces.windows[0].uncached, 4);
        assert_eq!(traces.simulate(0, &link.realize(&[])).misses, 1);
    }

    #[test]
    fn listing_fills_gaps_with_cold_code_to_reach_the_target() {
        // f0 and f1 run, f2..f4 are cold fill of 32, 64 and 16 bytes.
        let link = link(&[16, 16, 32, 64, 16]);
        let traces = traces_for(
            &link,
            &[
                Event::Run {
                    pc: 0x8001_0000,
                    words: 4,
                },
                Event::Run {
                    pc: 0x8001_0010,
                    words: 4,
                },
            ],
        );
        let pool = Pool::new(&link, &traces);
        let plan = Plan {
            hot: vec![0, 1],
            target: vec![0x000, 0x070],
        };
        let (listed, gap) = plan.listing(&link, &pool);
        // f1 wants 0x70: 0x60 of fill after f0's 0x10, largest first: f3
        // (64) then f2 (32).
        assert_eq!(listed, vec![0, 3, 2, 1]);
        assert_eq!(gap, 0x60);
        let address = link.realize(&listed);
        assert_eq!(address[1] % CACHE, 0x070);
        assert_eq!(address[4], 0x8001_0080);
    }

    #[test]
    fn realize_follows_the_listed_order_then_the_rest_in_link_order() {
        let link = link(&[16, 32, 8, 16]);
        let address = link.realize(&[3, 1]);
        assert_eq!(
            address,
            vec![0x8001_0030, 0x8001_0010, 0x8001_0040, 0x8001_0000]
        );
    }

    #[test]
    fn replaying_only_the_moved_sets_gives_the_stalls_of_a_full_replay() {
        // Six functions calling round in a pseudo-random order, two cold.
        let link = link(&[48, 96, 32, 4000, 160, 64, 2048, 1024]);
        let starts: Vec<u32> = link.sections.iter().map(|s| s.start).collect();
        let mut rng = Rng::new(3);
        let mut events = Vec::new();
        for _ in 0..2000 {
            let k = [0, 1, 2, 4, 5, 7][rng.below(6)];
            let words = link.sections[k].size / 4;
            let from = rng.below(words as usize) as u32;
            let len = 1 + rng.below((words - from) as usize) as u32;
            events.push(Event::Run {
                pc: starts[k] + 4 * from,
                words: len,
            });
        }
        let traces = traces_for(&link, &events);
        let pool = Pool::new(&link, &traces);
        let mut plan = plan_of(&link, &traces, &link.realize(&[]));
        let full = |address: &[u32]| traces.simulate(0, address).stalls;
        let first = link.realize(&plan.listing(&link, &pool).0);
        let mut delta = Delta::new(&traces, &first, vec![full(&first)]);
        for _ in 0..200 {
            let next = mutate(&plan, &mut rng, &[]);
            let address = link.realize(&next.listing(&link, &pool).0);
            let stalls = delta.stalls(&address);
            assert_eq!(stalls, vec![full(&address)]);
            if rng.below(2) == 0 {
                delta.accept(&address, stalls);
                plan = next;
            }
        }
    }

    #[test]
    fn the_search_separates_two_functions_that_share_a_set() {
        let link = link(&[16, 4080, 16, 4096]);
        // f0 and f2 alternate; as linked f2 sits 4 KB after f0, so they
        // share a set. f1 and f3 are cold fill.
        let call = [
            Event::Run {
                pc: 0x8001_0000,
                words: 4,
            },
            Event::Run {
                pc: 0x8001_1000,
                words: 4,
            },
        ];
        let events: Vec<Event> = call.iter().copied().cycle().take(200).collect();
        let traces = traces_for(&link, &events);
        let pool = Pool::new(&link, &traces);
        let start = plan_of(&link, &traces, &link.realize(&[]));
        let settings = Settings {
            seed: 7,
            rounds: 40,
            batch: 8,
            threads: 2,
            progress: false,
        };
        let found = search(&link, &traces, &pool, start.clone(), &settings);
        assert_eq!(found.start_stalls, vec![200 * 5]);
        assert_eq!(found.end_stalls, vec![2 * 5]);
        // The same seed finds the same plan with another thread count.
        let again = search(
            &link,
            &traces,
            &pool,
            start,
            &Settings {
                threads: 5,
                ..settings
            },
        );
        assert_eq!(again.listed, found.listed);
    }
}
