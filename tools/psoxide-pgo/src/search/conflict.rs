//! Conflict placement: the placement built from the traces' conflicts, one
//! function at a time, then refined by exact replays of a few chosen moves.
//!
//! [`super::search`] draws random moves and keeps the best of each batch. It
//! never asks where a function should go; it tries offsets and replays. This
//! module answers the question directly. A direct-mapped set misses on a
//! fetch exactly when another line of that set was fetched since the line's
//! own last fetch. So against code whose addresses are known, the stalls a
//! function `f` adds at each of the 256 line offsets in the cache can all be
//! counted in one pass over a window:
//!
//! - a fetch of `f`'s line `i` misses at offset `d` when a placed line of set
//!   `d + i` ran since `i` last ran;
//! - a fetch of a placed line that hits without `f` misses at offset `d` when
//!   a line `i` of `f` with `d + i` in its set ran since that line last ran.
//!
//! Both are counted with the refill cost the fetch would pay, so the pass is
//! the cache model itself restricted to `f` and the placed code (the TRG of
//! Gloy and Smith, counted exactly per set instead of summed per pair).
//!
//! [`construct`] places the functions that run most first, each at its
//! cheapest offset against the ones before it and the code at fixed
//! addresses, then orders them so each lands the shortest way round the cache
//! after the one in front (the gap filled with cold code as before). The
//! functions that run least are not placed: they lead the ordered code,
//! packed, so their addresses are known from the start and no later move
//! shifts them. [`refine`] then takes the functions that stall most, asks the
//! same pass for each one's best offsets against everything else as it now
//! lies, and replays only those candidates and a few word shifts, keeping a
//! move when the replay says the objective falls. Every number it decides on
//! comes from the trace; nothing is drawn at random, so one input gives one
//! output.
//!
//! What the ordering file cannot do bounds all of this: it places whole input
//! sections, so the hot and cold parts of one function cannot be split, and
//! it cannot pad, so an offset costs cold code to reach.

use std::collections::HashMap;

use super::{
    objective, refill, Delta, Link, Plan, Pool, Scored, Traces, CACHE, CHUNK_SHIFT, FIXED, LEAVES,
    SETS,
};

/// Lines in a chunk; a chunk whose base is mid-line runs into one more.
const CHUNK_LINES: u32 = (1 << CHUNK_SHIFT) / 16;

/// One line a segment runs, counted from its chunk's first line, and what
/// that fetch costs when it misses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Touch {
    line: u16,
    cost: u16,
}

/// Every distinct segment as the lines it runs, for each word its chunk's
/// base may start at within a line.
pub struct Lines {
    spans: Vec<(u32, u32)>,
    touches: Vec<Touch>,
}

impl Lines {
    pub fn new(traces: &Traces) -> Self {
        let mut spans = Vec::with_capacity(traces.segments.len() * 4);
        let mut touches = Vec::new();
        for segment in &traces.segments {
            let first = segment.first as usize;
            let intervals = &traces.intervals[first..first + segment.len as usize];
            for phase in 0..4u32 {
                let begin = touches.len() as u32;
                let mut seen = [0u64; 3];
                for &(from, to) in intervals {
                    let leaves = to & LEAVES != 0;
                    let to = to & !LEAVES;
                    let mut address = 4 * phase + 4 * u32::from(from);
                    let last = 4 * phase + 4 * u32::from(to);
                    loop {
                        let line = address >> 4;
                        let word = (address >> 2) & 3;
                        let ran = (last.min(address | 0xc) - address) / 4 + 1;
                        let left = leaves && last < (address | 0xc);
                        let bit = 1u64 << (line & 63);
                        if seen[line as usize >> 6] & bit == 0 {
                            seen[line as usize >> 6] |= bit;
                            touches.push(Touch {
                                line: line as u16,
                                cost: refill(word, false, ran, left) as u16,
                            });
                        }
                        let next = (line + 1) << 4;
                        if next > last {
                            break;
                        }
                        address = next;
                    }
                }
                spans.push((begin, touches.len() as u32 - begin));
            }
        }
        Self { spans, touches }
    }

    fn of(&self, segment: u32, phase: u32) -> &[Touch] {
        let (first, len) = self.spans[segment as usize * 4 + phase as usize];
        &self.touches[first as usize..(first + len) as usize]
    }
}

const NIL: u32 = u32::MAX;

/// Nodes in order of their last use, newest first, each with its time.
struct Recency {
    next: Vec<u32>,
    prev: Vec<u32>,
    time: Vec<u32>,
    head: u32,
}

impl Recency {
    fn new(nodes: usize) -> Self {
        Self {
            next: vec![NIL; nodes],
            prev: vec![NIL; nodes],
            time: vec![0; nodes],
            head: NIL,
        }
    }

    /// `node` was used at `now` (times start at 1; 0 is never).
    fn touch(&mut self, node: u32, now: u32) {
        let n = node as usize;
        if self.time[n] != 0 && self.head != node {
            let (prev, next) = (self.prev[n], self.next[n]);
            self.next[prev as usize] = next;
            if next != NIL {
                self.prev[next as usize] = prev;
            }
        }
        if self.head != node {
            self.next[n] = self.head;
            self.prev[n] = NIL;
            if self.head != NIL {
                self.prev[self.head as usize] = node;
            }
            self.head = node;
        }
        self.time[n] = now;
    }

    /// Every node used after `after`, newest first.
    fn since(&self, after: u32) -> impl Iterator<Item = u32> + '_ {
        let mut at = self.head;
        std::iter::from_fn(move || {
            (at != NIL && self.time[at as usize] > after).then(|| {
                let node = at;
                at = self.next[at as usize];
                node
            })
        })
    }
}

/// What a chunk is to one pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// Not placed yet: left out.
    Absent,
    /// At this address.
    Placed(u32),
    /// Part of the function being placed: its first line in that function.
    Candidate(u32),
}

/// The stalls `f` (the `Candidate` chunks, its first line `phase` bytes into
/// a cache line) adds in one window at each line offset of the cache, against
/// the `Placed` chunks.
fn offset_costs(
    traces: &Traces,
    lines: &Lines,
    window: usize,
    roles: &[Role],
    f_lines: usize,
    phase: u32,
) -> [f64; SETS] {
    let mut cost = [0u64; SETS];
    let mut tag = [u32::MAX; SETS];
    // When each set last ran placed code (0: never).
    let mut ran = [0u32; SETS];
    let mut own = Recency::new(f_lines);
    for (at, &id) in traces.windows[window].ids.iter().enumerate() {
        let now = at as u32 + 1;
        let segment = traces.segments[id as usize];
        match roles[segment.chunk as usize] {
            Role::Absent => {}
            Role::Placed(base) => {
                let first = base >> 4;
                for touch in lines.of(id, (base >> 2) & 3) {
                    let line = first + u32::from(touch.line);
                    let set = (line as usize) & (SETS - 1);
                    if tag[set] == line {
                        // A hit without f: a miss wherever one of f's lines
                        // that ran since lands in this set.
                        for i in own.since(ran[set]) {
                            cost[(set.wrapping_sub(i as usize)) & (SETS - 1)] +=
                                u64::from(touch.cost);
                        }
                    }
                    tag[set] = line;
                    ran[set] = now;
                }
            }
            Role::Candidate(first) => {
                for touch in lines.of(id, phase / 4) {
                    let i = first + u32::from(touch.line);
                    let after = own.time[i as usize];
                    if after != 0 {
                        // Misses at every offset that puts this line in a set
                        // placed code ran in since it last did.
                        let c = u64::from(touch.cost);
                        let shift = i as usize & (SETS - 1);
                        for (d, sum) in cost.iter_mut().enumerate() {
                            if ran[(d + shift) & (SETS - 1)] > after {
                                *sum += c;
                            }
                        }
                    }
                    own.touch(i, now);
                }
            }
        }
    }
    cost.map(|cycles| cycles as f64)
}

/// The weighted, normalised stalls `f` adds at each line offset, summed
/// over every window, for each of `phases` (the windows and phases shared
/// among `threads` threads).
fn costs(
    traces: &Traces,
    lines: &Lines,
    roles: &[Role],
    f_lines: usize,
    phases: &[u32],
    scale: &[f64],
    threads: usize,
) -> Vec<[f64; SETS]> {
    let tasks: Vec<(usize, usize)> = (0..phases.len())
        .flat_map(|p| (0..traces.windows.len()).map(move |w| (p, w)))
        .collect();
    let per_thread = tasks.len().div_ceil(threads.max(1)).max(1);
    let done: Vec<(usize, usize, [f64; SETS])> = std::thread::scope(|scope| {
        let handles: Vec<_> = tasks
            .chunks(per_thread)
            .map(|some| {
                scope.spawn(move || {
                    some.iter()
                        .map(|&(p, w)| {
                            let cost = offset_costs(traces, lines, w, roles, f_lines, phases[p]);
                            (p, w, cost)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("a pass does not panic"))
            .collect()
    });
    let mut total = vec![[0.0; SETS]; phases.len()];
    for (p, w, cost) in done {
        for (sum, &cycles) in total[p].iter_mut().zip(&cost) {
            *sum += scale[w] * cycles;
        }
    }
    total
}

/// The lines a function at byte `phase` of a line spans.
fn line_count(link: &Link, f: usize, phase: u32) -> usize {
    ((phase + link.sections[f].size).div_ceil(16) + CHUNK_LINES) as usize
}

/// Each traced movable function's weight: the refill cost of every line it
/// fetches, each window scaled as the objective scales it. A function
/// cannot miss, or make another miss, more often than it fetches lines.
pub fn weights(link: &Link, traces: &Traces, lines: &Lines, scale: &[f64]) -> Vec<(usize, f64)> {
    let per_segment: Vec<u64> = (0..traces.segments.len() as u32)
        .map(|id| lines.of(id, 0).iter().map(|t| u64::from(t.cost)).sum())
        .collect();
    let mut weight: HashMap<usize, f64> = HashMap::new();
    for (window, &scale) in traces.windows.iter().zip(scale) {
        let mut cycles: HashMap<u32, u64> = HashMap::new();
        for &id in &window.ids {
            let unit = traces.chunks[traces.segments[id as usize].chunk as usize].unit;
            if unit != FIXED {
                *cycles.entry(unit).or_default() += per_segment[id as usize];
            }
        }
        for (unit, cycles) in cycles {
            *weight.entry(unit as usize).or_default() += scale * cycles as f64;
        }
    }
    let mut ranked: Vec<(usize, f64)> = weight
        .into_iter()
        .filter(|&(k, w)| link.movable(k) && w > 0.0)
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked
}

/// The chunks every pass knows the address of before anything is placed:
/// code at fixed addresses and sections the ordering cannot move.
fn fixed_roles(link: &Link, traces: &Traces) -> Vec<Role> {
    traces
        .chunks
        .iter()
        .map(|chunk| {
            let offset = chunk.index << CHUNK_SHIFT;
            match chunk.unit {
                FIXED => Role::Placed(offset),
                unit => {
                    let k = unit as usize;
                    let stays = link.fixed[k] || link.sections[k].start < link.start;
                    if stays {
                        Role::Placed(link.sections[k].start + offset)
                    } else {
                        Role::Absent
                    }
                }
            }
        })
        .collect()
}

/// The chunks of each unit.
fn chunks_of(traces: &Traces) -> HashMap<u32, Vec<usize>> {
    let mut of: HashMap<u32, Vec<usize>> = HashMap::new();
    for (c, chunk) in traces.chunks.iter().enumerate() {
        of.entry(chunk.unit).or_default().push(c);
    }
    of
}

/// The cheapest line offset; among offsets that cost the same, the nearest
/// at or after `prefer` (so functions with nothing between them pack).
fn cheapest(cost: &[f64; SETS], prefer: usize) -> usize {
    let least = cost.iter().copied().fold(f64::INFINITY, f64::min);
    let tolerance = least.abs() * 1e-9 + 1e-15;
    (0..SETS)
        .map(|step| (prefer + step) & (SETS - 1))
        .find(|&d| cost[d] <= least + tolerance)
        .unwrap_or(0)
}

/// What [`construct`] decided.
pub struct Constructed {
    pub plan: Plan,
    /// The cold code that fills the gaps.
    pub pool: Pool,
    pub placed_weight: f64,
    pub total_weight: f64,
}

/// Place the functions that carry `share` of the traced weight, heaviest
/// first, each at the line offset that costs least against those before it
/// (the rest of the traced code leads the ordered code, packed); then order
/// them for the fewest bytes of gap.
pub fn construct(
    link: &Link,
    traces: &Traces,
    lines: &Lines,
    scale: &[f64],
    share: f64,
    threads: usize,
    progress: bool,
) -> Constructed {
    let ranked = weights(link, traces, lines, scale);
    let total: f64 = ranked.iter().map(|&(_, w)| w).sum();
    let mut pinned = Vec::new();
    let mut covered = 0.0;
    for &(k, w) in &ranked {
        if covered >= share * total {
            break;
        }
        pinned.push(k);
        covered += w;
    }
    let spare: Vec<usize> = ranked[pinned.len()..].iter().map(|&(k, _)| k).collect();
    let mut roles = fixed_roles(link, traces);
    let chunks = chunks_of(traces);
    // The functions left unplaced lead the ordered code, packed: their
    // addresses are known before anything is placed and no later move
    // shifts them.
    let mut lead = Vec::with_capacity(spare.len());
    let mut at = link.start;
    for &k in &spare {
        at = at.next_multiple_of(link.align[k]);
        lead.push((k, at));
        for &c in chunks.get(&(k as u32)).map_or(&[][..], Vec::as_slice) {
            roles[c] = Role::Placed(at + (traces.chunks[c].index << CHUNK_SHIFT));
        }
        at += link.sections[k].size;
    }
    let mut offset: HashMap<usize, u32> = HashMap::new();
    let mut found: HashMap<usize, [f64; SETS]> = HashMap::new();
    let mut prefer = 0usize;
    // Each placed function gets addresses of its own below KSEG0, so no line
    // of it matches the tag of another function or of code at a fixed address.
    let mut page = 0x1000_0000u32;
    let began = std::time::Instant::now();
    for (rank, &f) in pinned.iter().enumerate() {
        let mine = chunks.get(&(f as u32)).map_or(&[][..], Vec::as_slice);
        for &c in mine {
            roles[c] = Role::Candidate(traces.chunks[c].index * CHUNK_LINES);
        }
        let cost = costs(
            traces,
            lines,
            &roles,
            line_count(link, f, 0),
            &[0],
            scale,
            threads,
        )[0];
        let d = cheapest(&cost, prefer);
        offset.insert(f, d as u32 * 16);
        found.insert(f, cost);
        let base = page + d as u32 * 16;
        for &c in mine {
            roles[c] = Role::Placed(base + (traces.chunks[c].index << CHUNK_SHIFT));
        }
        page += (link.sections[f].size + CACHE).next_multiple_of(CACHE);
        prefer = (d + link.sections[f].size.div_ceil(16) as usize) & (SETS - 1);
        if progress && (rank + 1) % 50 == 0 {
            println!(
                "  placed {} of {}, {:.0} s",
                rank + 1,
                pinned.len(),
                began.elapsed().as_secs_f64()
            );
        }
    }
    let weight: HashMap<usize, f64> = ranked.iter().copied().collect();
    let pool = Pool::new(link, traces);
    Constructed {
        plan: sequence(link, &pool, &lead, &pinned, &offset, &found, &weight),
        pool,
        placed_weight: covered,
        total_weight: total,
    }
}

/// How [`sequence`] ranks the next function: off its offset, gap, index.
type Pick = (bool, u32, usize);

/// The share of a function's own fetch cost a landing off its offset may
/// add and still count as on it.
const SLACK: f64 = 0.01;

/// The placed functions in link order, after the `lead` (unplaced, packed
/// where the ordered code starts, each with its address). Each step takes
/// the function that lands on its offset once the gap in front is filled
/// from `pool` (a landing that costs it under [`SLACK`] of its own fetch cost
/// by its conflict pass counts as on it), then the shortest gap, then the
/// heavier (earlier in `pinned`).
fn sequence(
    link: &Link,
    pool: &Pool,
    lead: &[(usize, u32)],
    pinned: &[usize],
    offset: &HashMap<usize, u32>,
    found: &HashMap<usize, [f64; SETS]>,
    weight: &HashMap<usize, f64>,
) -> Plan {
    let mut left: Vec<usize> = pinned.to_vec();
    let mut hot: Vec<usize> = lead.iter().map(|&(k, _)| k).collect();
    let mut target: Vec<u32> = lead.iter().map(|&(_, at)| at % CACHE).collect();
    let mut used = vec![false; pool.0.len()];
    let mut at = lead
        .last()
        .map_or(link.start, |&(k, at)| at + link.sections[k].size);
    while !left.is_empty() {
        // (off its offset, gap, index), the pool slots filling the gap, and
        // where the function lands.
        let mut best: Option<(Pick, Vec<usize>, u32)> = None;
        for (i, &f) in left.iter().enumerate() {
            let want = offset[&f].wrapping_sub(at.next_multiple_of(link.align[f])) % CACHE;
            if best
                .as_ref()
                .is_some_and(|(key, ..)| !key.0 && key.1 < want)
            {
                continue;
            }
            let (slots, filled) = pool.fill(link, &used, at, want);
            let lands = (at + filled).next_multiple_of(link.align[f]);
            let line = ((lands % CACHE) >> 4) as usize;
            let cost = &found[&f];
            let off = lands % CACHE != offset[&f]
                && cost[line] - cost[(offset[&f] >> 4) as usize] > SLACK * weight[&f];
            let key = (off, want, i);
            if best.as_ref().is_none_or(|(least, ..)| key < *least) {
                best = Some((key, slots, lands));
            }
        }
        let ((_, _, pick), slots, lands) = best.expect("left is not empty");
        for slot in slots {
            used[slot] = true;
        }
        let f = left.remove(pick);
        at = lands + link.sections[f].size;
        hot.push(f);
        target.push(offset[&f]);
    }
    Plan { hot, target }
}

/// How [`refine`] went.
pub struct Refined {
    pub listed: Vec<usize>,
    pub stalls: Vec<u64>,
    pub cost: f64,
    pub evaluations: usize,
    pub accepted: usize,
}

/// The refinement's settings.
#[derive(Clone, Copy, Debug)]
pub struct Refine {
    /// Passes over the functions that stall most; stops early when a pass
    /// keeps nothing.
    pub passes: usize,
    /// Functions per pass.
    pub top: usize,
    /// Most word offsets replayed per function, the ones its conflict pass
    /// says gain most.
    pub offsets: usize,
    pub threads: usize,
    pub progress: bool,
}

/// A plan laid out with the fill of each gap kept, so a move refills only
/// the gaps beside the function it moves and the rest of the layout stays
/// where it was (a replay of the sets it touched then scores it).
#[derive(Clone, Debug)]
struct Laid {
    plan: Plan,
    /// Per entry of the plan: the pool slots in the gap in front of it.
    fills: Vec<Vec<usize>>,
    used: Vec<bool>,
}

impl Laid {
    /// `plan` as [`Plan::listing`] lays it out.
    fn new(plan: Plan, link: &Link, pool: &Pool) -> Self {
        let mut laid = Self {
            fills: vec![Vec::new(); plan.hot.len()],
            used: vec![false; pool.0.len()],
            plan,
        };
        let mut at = link.start;
        for i in 0..laid.plan.hot.len() {
            at = laid.refill(link, pool, i, at);
        }
        laid
    }

    /// Fill the gap in front of entry `i` from free slots, its function
    /// starting no earlier than `at`; where its function ends.
    fn refill(&mut self, link: &Link, pool: &Pool, i: usize, at: u32) -> u32 {
        let f = self.plan.hot[i];
        let want = self.plan.target[i].wrapping_sub(at.next_multiple_of(link.align[f])) % CACHE;
        let (slots, filled) = pool.fill(link, &self.used, at, want);
        for &slot in &slots {
            self.used[slot] = true;
        }
        self.fills[i] = slots;
        (at + filled).next_multiple_of(link.align[f]) + link.sections[f].size
    }

    fn free(&mut self, i: usize) {
        for slot in std::mem::take(&mut self.fills[i]) {
            self.used[slot] = false;
        }
    }

    /// Entry `i` at `target`: the gaps in front of it and of the next entry
    /// refilled; later gaps are refilled only if the next entry no longer
    /// lands where it did.
    fn moved(&self, link: &Link, pool: &Pool, i: usize, target: u32) -> Self {
        let mut next = self.clone();
        next.plan.target[i] = target;
        let ends = self.ends(link, pool);
        let mut at = if i == 0 { link.start } else { ends[i - 1] };
        next.free(i);
        if i + 1 < next.plan.hot.len() {
            next.free(i + 1);
        }
        for j in i..next.plan.hot.len() {
            if j > i + 1 && at == ends[j - 1] {
                break;
            }
            if j > i + 1 {
                next.free(j);
            }
            at = next.refill(link, pool, j, at);
        }
        next
    }

    /// Where each entry's function ends.
    fn ends(&self, link: &Link, pool: &Pool) -> Vec<u32> {
        let mut at = link.start;
        self.plan
            .hot
            .iter()
            .zip(&self.fills)
            .map(|(&f, fill)| {
                for &slot in fill {
                    let k = pool.0[slot];
                    at = at.next_multiple_of(link.align[k]) + link.sections[k].size;
                }
                at = at.next_multiple_of(link.align[f]) + link.sections[f].size;
                at
            })
            .collect()
    }

    fn listed(&self, pool: &Pool) -> Vec<usize> {
        let mut listed = Vec::with_capacity(self.plan.hot.len() * 2);
        for (&f, fill) in self.plan.hot.iter().zip(&self.fills) {
            listed.extend(fill.iter().map(|&slot| pool.0[slot]));
            listed.push(f);
        }
        listed
    }
}

/// Each candidate's objective, stalls, listing and addresses, scored on
/// `threads` threads.
fn score(
    link: &Link,
    traces: &Traces,
    pool: &Pool,
    delta: &Delta<'_>,
    candidates: &[Laid],
    baseline: &[u64],
    threads: usize,
) -> Vec<Scored> {
    let threads = threads.clamp(1, candidates.len().max(1));
    let per = candidates.len().div_ceil(threads);
    let mut results = vec![None; candidates.len()];
    std::thread::scope(|scope| {
        for (chunk, out) in candidates.chunks(per).zip(results.chunks_mut(per)) {
            scope.spawn(move || {
                for (laid, slot) in chunk.iter().zip(out.iter_mut()) {
                    let listed = laid.listed(pool);
                    let address = link.realize(&listed);
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

/// Improve `plan` by exact replays of chosen moves: per pass, the `top`
/// functions by the stalls they take now. For each, the conflict pass at
/// every word phase predicts what each of the cache's word offsets would cost
/// against everything else as it lies; the `offsets` that it says gain most
/// are replayed, and the best is kept when the replay says the objective
/// falls.
pub fn refine(
    link: &Link,
    traces: &Traces,
    lines: &Lines,
    pool: &Pool,
    plan: Plan,
    baseline: &[u64],
    settings: &Refine,
) -> Refined {
    let scale: Vec<f64> = traces
        .windows
        .iter()
        .zip(baseline)
        .map(|(window, &base)| window.weight / base.max(1) as f64)
        .collect();
    let mut laid = Laid::new(plan, link, pool);
    let mut listed = laid.listed(pool);
    let mut address = link.realize(&listed);
    let mut stalls: Vec<u64> = (0..traces.windows.len())
        .map(|w| traces.simulate(w, &address).stalls)
        .collect();
    let mut current = objective(traces, &stalls, baseline);
    let mut delta = Delta::new(traces, &address, stalls.clone());
    let mut evaluations = traces.windows.len();
    let mut accepted = 0;
    let began = std::time::Instant::now();
    for pass in 0..settings.passes {
        let mut charged = vec![0.0; link.sections.len() + 1];
        for (w, &scale) in scale.iter().enumerate() {
            let (_, cycles) = traces.attribute(w, &address, link.sections.len());
            for (sum, &c) in charged.iter_mut().zip(&cycles) {
                *sum += scale * c as f64;
            }
        }
        evaluations += traces.windows.len();
        let hot = &laid.plan.hot;
        let mut order: Vec<usize> = (0..hot.len()).collect();
        order.sort_by(|&a, &b| charged[hot[b]].total_cmp(&charged[hot[a]]).then(a.cmp(&b)));
        let kept_before = accepted;
        let mut replayed = 0;
        for &i in order.iter().take(settings.top) {
            let f = laid.plan.hot[i];
            let bases = traces.bases(&address);
            let roles: Vec<Role> = traces
                .chunks
                .iter()
                .enumerate()
                .map(|(c, chunk)| {
                    if chunk.unit == f as u32 {
                        Role::Candidate(chunk.index * CHUNK_LINES)
                    } else {
                        Role::Placed(bases[c])
                    }
                })
                .collect();
            let cost = costs(
                traces,
                lines,
                &roles,
                line_count(link, f, 12),
                &[0, 4, 8, 12],
                &scale,
                settings.threads,
            );
            // What each word offset would cost against everything else as it
            // lies now; only moves the pass says gain are replayed.
            let now = address[f] % CACHE;
            let predicted = |at: u32| cost[(at as usize >> 2) & 3][at as usize >> 4];
            let mut gains: Vec<(f64, u32)> = (0..CACHE / 4)
                .map(|word| word * 4)
                .filter(|&at| at != now && at % link.align[f].min(CACHE) == 0)
                .map(|at| (predicted(at) - predicted(now), at))
                .filter(|&(gain, _)| gain < 0.0)
                .collect();
            gains.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            let targets: Vec<u32> = gains
                .iter()
                .take(settings.offsets)
                .map(|&(_, at)| at)
                .collect();
            if targets.is_empty() {
                continue;
            }
            replayed += targets.len();
            let candidates: Vec<Laid> = targets
                .iter()
                .map(|&at| laid.moved(link, pool, i, at))
                .collect();
            let results = score(
                link,
                traces,
                pool,
                &delta,
                &candidates,
                baseline,
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
                    laid = candidates[at].clone();
                    listed = results[at].2.clone();
                    address = results[at].3.clone();
                    stalls = results[at].1.clone();
                    delta.accept(&address, stalls.clone());
                    accepted += 1;
                }
            }
        }
        if settings.progress {
            println!(
                "  pass {}: objective {current:.4}, {} moves kept of {replayed} replayed, \
                 {:.0} s",
                pass + 1,
                accepted - kept_before,
                began.elapsed().as_secs_f64()
            );
        }
        if accepted == kept_before {
            break;
        }
    }
    Refined {
        listed,
        stalls,
        cost: current,
        evaluations,
        accepted,
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{link, traces_for};
    use super::super::{plan_of, Event, Rng};
    use super::*;

    #[test]
    fn recency_lists_what_ran_since_newest_first() {
        let mut recency = Recency::new(4);
        recency.touch(2, 1);
        recency.touch(0, 2);
        recency.touch(3, 3);
        recency.touch(0, 4);
        assert_eq!(recency.since(0).collect::<Vec<_>>(), vec![0, 3, 2]);
        assert_eq!(recency.since(2).collect::<Vec<_>>(), vec![0, 3]);
        assert_eq!(recency.since(4).count(), 0);
    }

    #[test]
    fn a_segment_runs_each_line_once_at_the_cost_of_its_first_fetch() {
        let link = link(&[64]);
        // Words 1..6 then 0: lines 0 and 1, entered at word 1 and word 0.
        let traces = traces_for(
            &link,
            &[
                Event::Run {
                    pc: 0x8001_0004,
                    words: 6,
                },
                Event::Run {
                    pc: 0x8001_0000,
                    words: 1,
                },
            ],
        );
        let lines = Lines::new(&traces);
        let touches = lines.of(0, 0);
        assert_eq!(touches.len(), 2);
        assert_eq!(touches[0].line, 0);
        assert_eq!(u64::from(touches[0].cost), refill(1, false, 3, false));
        assert_eq!(u64::from(touches[1].cost), refill(0, false, 3, true));
        // From word 3 of a line the same words span three lines.
        assert_eq!(lines.of(0, 3).len(), 3);
    }

    /// Two functions calling each other in a pseudo-random pattern, every
    /// run starting on a line so no fill is partial.
    fn two_functions() -> (Link, Traces) {
        let link = link(&[256, 192]);
        let mut rng = Rng::new(11);
        let mut events = Vec::new();
        for _ in 0..400 {
            let k = rng.below(2);
            let lines = link.sections[k].size / 16;
            let from = rng.below(lines as usize) as u32;
            let words = 1 + rng.below((4 * (lines - from)) as usize) as u32;
            events.push(Event::Run {
                pc: link.sections[k].start + 16 * from,
                words,
            });
        }
        let traces = traces_for(&link, &events);
        (link, traces)
    }

    #[test]
    fn the_pass_gives_the_stalls_of_a_replay_at_every_offset() {
        let (link, traces) = two_functions();
        let lines = Lines::new(&traces);
        let roles: Vec<Role> = traces
            .chunks
            .iter()
            .map(|chunk| match chunk.unit {
                0 => Role::Placed(link.sections[0].start + (chunk.index << CHUNK_SHIFT)),
                _ => Role::Candidate(chunk.index * CHUNK_LINES),
            })
            .collect();
        let cost = offset_costs(&traces, &lines, 0, &roles, line_count(&link, 1, 0), 0);
        let mut address = link.realize(&[]);
        let replay = |address: &[u32]| traces.simulate(0, address).stalls as f64;
        address[1] = 0x8020_0000;
        let fixed = replay(&address) - cost[0];
        for d in 0..SETS as u32 {
            address[1] = 0x8020_0000 + 16 * d;
            assert_eq!(replay(&address) - cost[d as usize], fixed, "offset {d}");
        }
        // Somewhere f1 overlaps f0 and costs more.
        assert!(cost[0] > cost[16]);
    }

    /// f0 and f2 alternate and share a set as linked; f1 and f3 are cold.
    fn conflicting() -> (Link, Traces) {
        let link = link(&[16, 4080, 16, 4096]);
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
        (link, traces)
    }

    #[test]
    fn construction_separates_two_functions_that_share_a_set() {
        let (link, traces) = conflicting();
        assert_eq!(traces.simulate(0, &link.realize(&[])).stalls, 200 * 5);
        let lines = Lines::new(&traces);
        let built = construct(&link, &traces, &lines, &[1.0], 1.0, 2, false);
        assert_eq!(built.plan.hot.len(), 2);
        let (listed, _) = built.plan.listing(&link, &built.pool);
        assert_eq!(traces.simulate(0, &link.realize(&listed)).stalls, 2 * 5);
    }

    #[test]
    fn sequencing_takes_the_shortest_gap_and_fills_it_to_the_offset() {
        // f0 and f1 are placed; f2..f4 are cold fill of 0x80, 0x40, 0x20.
        let link = link(&[32, 32, 0x80, 0x40, 0x20]);
        let traces = traces_for(
            &link,
            &[
                Event::Run {
                    pc: 0x8001_0000,
                    words: 8,
                },
                Event::Run {
                    pc: 0x8001_0020,
                    words: 8,
                },
            ],
        );
        let pool = Pool::new(&link, &traces);
        let offset = HashMap::from([(0, 0x100), (1, 0x000)]);
        let flat = HashMap::from([(0, [0.0; SETS]), (1, [0.0; SETS])]);
        let weight = HashMap::from([(0, 1.0), (1, 1.0)]);
        let plan = sequence(&link, &pool, &[], &[0, 1], &offset, &flat, &weight);
        // f1 lands where the ordered code starts (offset 0) with no gap;
        // f0 then wants 0xe0 more, all three cold functions.
        assert_eq!(plan.hot, vec![1, 0]);
        let (listed, gap) = plan.listing(&link, &pool);
        assert_eq!(gap, 0xe0);
        let address = link.realize(&listed);
        assert_eq!(address[0] % CACHE, 0x100);
        assert_eq!(address[1] % CACHE, 0x000);
    }

    #[test]
    fn refinement_finds_the_separation_and_is_the_same_on_any_thread_count() {
        let (link, traces) = conflicting();
        let lines = Lines::new(&traces);
        let pool = Pool::new(&link, &traces);
        let start = plan_of(&link, &traces, &link.realize(&[]));
        let baseline = vec![200 * 5];
        let settings = Refine {
            passes: 3,
            top: 2,
            offsets: 4,
            threads: 1,
            progress: false,
        };
        let one = refine(
            &link,
            &traces,
            &lines,
            &pool,
            start.clone(),
            &baseline,
            &settings,
        );
        assert_eq!(one.stalls, vec![2 * 5]);
        let three = refine(
            &link,
            &traces,
            &lines,
            &pool,
            start,
            &baseline,
            &Refine {
                threads: 3,
                ..settings
            },
        );
        assert_eq!(three.listed, one.listed);
    }

    #[test]
    fn a_move_refills_the_gaps_beside_it_and_leaves_the_rest() {
        // f0..f2 placed; f3..f7 cold fill. Both layouts fill every gap
        // exactly: 0x80 0x40 0x20 then 0x100 0x20, or the other way round.
        let link = link(&[32, 32, 32, 0x100, 0x80, 0x40, 0x20, 0x20]);
        let events: Vec<Event> = (0..3)
            .map(|k| Event::Run {
                pc: link.sections[k].start,
                words: 8,
            })
            .collect();
        let traces = traces_for(&link, &events);
        let pool = Pool::new(&link, &traces);
        let plan = Plan {
            hot: vec![0, 1, 2],
            target: vec![0x000, 0x100, 0x240],
        };
        let laid = Laid::new(plan.clone(), &link, &pool);
        assert_eq!(laid.listed(&pool), plan.listing(&link, &pool).0);
        let before = link.realize(&laid.listed(&pool));
        let moved = laid.moved(&link, &pool, 1, 0x140);
        let after = link.realize(&moved.listed(&pool));
        assert_eq!(after[1] % CACHE, 0x140);
        assert_eq!(after[0], before[0]);
        assert_eq!(after[2], before[2]);
        assert_eq!(moved.ends(&link, &pool)[2], laid.ends(&link, &pool)[2]);
    }
}
