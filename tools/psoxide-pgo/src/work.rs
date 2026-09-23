//! Split a replay's instructions into work and waiting.
//!
//! A game locked to the display (every frame two vblanks, say) spends its
//! slack in spin loops: `wait_vblank`, `draw_sync`, a DMA-done poll, or a
//! game's own copy of one. Ticks and cycles then come out the same for a
//! faster and a slower build, so `measure` subtracts the waiting: whatever
//! ran inside a wait loop is wait, everything else (interrupt handlers
//! included) is work.
//!
//! Wait loops are found in the code the replay left in RAM, not by symbol,
//! so the same rule covers psx-rt's waits, a game's own ones and every PGO
//! layout of either. A wait loop is a small loop that keeps reading memory
//! nothing inside it changes:
//!
//! - one cycle closed by a backward branch or jump (a profile-guided layout
//!   rotates loops and closes them with `j`), at most [`MAX_SPAN`]
//!   instructions long, with no inner loop and no way in from code after it
//!   (so it is not a piece of a larger loop);
//! - no store, call, syscall, coprocessor write or GTE work;
//! - at least one load, and every load that is not a stack reload reads an
//!   address the loop never changes. LLVM hoists a plain invariant load out of
//!   a loop with no stores, so one left inside is volatile: a hardware
//!   register, or a counter an interrupt writes;
//! - every branch in it depends only on what those loads return, on
//!   values fixed for the whole loop, or on a pure counter (`spins += 1`, a
//!   bounded wait's limit). A register the loop carries in any other way is
//!   state, and a loop that branches on it is doing work.
//!
//! The emulator counts instructions per 16-byte I-cache line, or per word
//! when it has `--pc-log-words`. Per line, a line the loop touches counts as
//! wait in full (the instructions sharing it run once per call, not once
//! per iteration); per word the split is exact.
//!
//! Some waits are beyond this rule. HK's present loop calls
//! `input::checkpoint` (which polls the pad or refills audio when either is
//! due), keeps its clock in a stack slot and branches on a flag it clears.
//! A caller names such a loop by address (`measure --wait-range`, see
//! [`split`]); with per-word counts, its calls count as waiting as far as
//! [`attribute`] can prove, and no further.

use std::collections::{BTreeSet, HashMap, HashSet};

/// Longest loop, in instructions from the branch target to the closing
/// branch, that can be a wait loop. psx-rt's bounded waits take 8.
pub const MAX_SPAN: u32 = 32;

/// `HI` and `LO` as extra registers.
const HI: u8 = 32;
const LO: u8 = 33;
const SP: u8 = 29;

/// What one instruction does, as far as the classifier cares.
#[derive(Clone, Debug, PartialEq)]
enum Op {
    /// Writes `dst` (if any) from `srcs` and nothing else.
    Alu { dst: Vec<u8>, srcs: Vec<u8> },
    /// `dst = memory[base + offset]`, or a coprocessor-0 read when `base` is
    /// `None` (polling Cause, for example).
    Load { dst: Option<u8>, base: Option<u8> },
    /// Conditional branch. `link` is BLTZAL/BGEZAL.
    Branch {
        srcs: Vec<u8>,
        target: u32,
        link: bool,
    },
    /// J or JAL.
    Jump { target: u32, link: bool },
    /// JR or JALR through `rs`.
    JumpRegister { rs: u8, link: bool },
    /// A store through `base`.
    Store { base: u8 },
    /// Anything that is work by definition: a coprocessor write, a GTE
    /// command, a syscall, an unknown opcode.
    Effect,
}

fn decode(word: u32, pc: u32) -> Op {
    let op = word >> 26;
    let rs = ((word >> 21) & 31) as u8;
    let rt = ((word >> 16) & 31) as u8;
    let rd = ((word >> 11) & 31) as u8;
    let branch = pc
        .wrapping_add(4)
        .wrapping_add(((word & 0xffff) as i16 as i32 as u32) << 2);
    let alu = |dst: &[u8], srcs: &[u8]| Op::Alu {
        dst: dst.iter().copied().filter(|&r| r != 0).collect(),
        srcs: srcs.to_vec(),
    };
    match op {
        0 => match word & 63 {
            0x00 | 0x02 | 0x03 => alu(&[rd], &[rt]),
            0x04 | 0x06 | 0x07 => alu(&[rd], &[rt, rs]),
            0x08 => Op::JumpRegister { rs, link: false },
            0x09 => Op::JumpRegister { rs, link: true },
            0x10 => alu(&[rd], &[HI]),
            0x11 => alu(&[HI], &[rs]),
            0x12 => alu(&[rd], &[LO]),
            0x13 => alu(&[LO], &[rs]),
            0x18..=0x1b => alu(&[HI, LO], &[rs, rt]),
            0x20..=0x27 | 0x2a | 0x2b => alu(&[rd], &[rs, rt]),
            _ => Op::Effect, // syscall, break, unknown
        },
        1 => Op::Branch {
            srcs: vec![rs],
            target: branch,
            link: rt & 0x1e == 0x10,
        },
        2 | 3 => Op::Jump {
            target: (pc.wrapping_add(4) & 0xf000_0000) | ((word & 0x03ff_ffff) << 2),
            link: op == 3,
        },
        4 | 5 => Op::Branch {
            srcs: vec![rs, rt],
            target: branch,
            link: false,
        },
        6 | 7 => Op::Branch {
            srcs: vec![rs],
            target: branch,
            link: false,
        },
        0x08..=0x0e => alu(&[rt], &[rs]),
        0x0f => alu(&[rt], &[]),
        // MFC0: a read of state the loop cannot change.
        0x10 if rs == 0 => Op::Load {
            dst: (rt != 0).then_some(rt),
            base: None,
        },
        // LWL and LWR merge into rt, but only the address matters here.
        0x20..=0x26 => Op::Load {
            dst: (rt != 0).then_some(rt),
            base: Some(rs),
        },
        0x28..=0x2b | 0x2e => Op::Store { base: rs },
        _ => Op::Effect, // COP0 writes, RFE, every GTE op, LWC2, SWC2
    }
}

impl Op {
    fn target(&self) -> Option<u32> {
        match self {
            Op::Branch { target, .. } | Op::Jump { target, .. } => Some(*target),
            _ => None,
        }
    }

    fn defs(&self) -> &[u8] {
        match self {
            Op::Alu { dst, .. } => dst,
            Op::Load { dst: Some(dst), .. } => std::slice::from_ref(dst),
            _ => &[],
        }
    }

    /// `r = r + imm` or `r = r +/- s`, where `s` is fixed for the loop.
    fn counts(&self, word: u32, reg: u8, written: &HashSet<u8>) -> bool {
        let op = word >> 26;
        let rs = ((word >> 21) & 31) as u8;
        let rt = ((word >> 16) & 31) as u8;
        match op {
            0x08 | 0x09 => rs == reg && rt == reg,
            0 => {
                matches!(word & 63, 0x21 | 0x23)
                    && ((word >> 11) & 31) as u8 == reg
                    && rs == reg
                    && !written.contains(&rt)
            }
            _ => false,
        }
    }
}

/// Guest code, read out of a RAM dump.
pub struct Code<'a> {
    ram: &'a [u8],
}

impl<'a> Code<'a> {
    pub fn new(ram: &'a [u8]) -> Self {
        Self { ram }
    }

    fn word(&self, pc: u32) -> Option<u32> {
        let at = (pc & 0x001f_ffff) as usize;
        let bytes = self.ram.get(at..at + 4)?;
        Some(u32::from_le_bytes(bytes.try_into().ok()?))
    }

    fn op(&self, pc: u32) -> Op {
        self.word(pc).map_or(Op::Effect, |word| decode(word, pc))
    }

    /// When `pc` is a site `hazard_patch.py` rerouted, the instruction it
    /// replaced and the trampoline (its first word and length in words):
    ///
    /// - `j`/`jal TRAMP`, `TRAMP: nop ; j T ; nop` stands for `j`/`jal T`;
    /// - `j TRAMP`, `TRAMP: bXX +3 ; nop ; j pc+8 ; nop ; j T ; nop` stands
    ///   for `bXX T` (the slot load cannot write the branch's sources).
    ///
    /// A wait loop whose slot load raced its consumer gets the second shape,
    /// and read as written it leaves the loop's span and never comes back.
    fn trampoline(&self, pc: u32) -> Option<(Op, u32, u32)> {
        let Op::Jump {
            target: tramp,
            link,
        } = self.op(pc)
        else {
            return None;
        };
        let word = |index: u32| self.word(tramp + 4 * index);
        let jump = |index: u32| match self.op(tramp + 4 * index) {
            Op::Jump {
                target,
                link: false,
            } => Some(target),
            _ => None,
        };
        if word(0)? == 0 && word(2)? == 0 {
            if let Some(target) = jump(1) {
                return Some((Op::Jump { target, link }, tramp, 3));
            }
        }
        let branch = word(0)?;
        let Op::Branch { srcs, link, .. } = decode(branch, tramp) else {
            return None;
        };
        let shape = !link
            && branch & 0xffff == 3
            && word(1)? == 0
            && jump(2)? == pc + 8
            && word(3)? == 0
            && word(5)? == 0;
        let target = jump(4).filter(|_| shape)?;
        Some((
            Op::Branch {
                srcs,
                target,
                link: false,
            },
            tramp,
            6,
        ))
    }

    /// The instruction at `pc` as the compiler emitted it: [`Self::op`], or
    /// what a hazard trampoline stands in for.
    fn unpatched(&self, pc: u32) -> Op {
        self.trampoline(pc)
            .map_or_else(|| self.op(pc), |(op, _, _)| op)
    }
}

/// One loop found to be waiting: `start` (the backward branch's target)
/// through `end` (the closing branch's delay slot), and the words of any
/// hazard trampoline it runs through.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct WaitLoop {
    pub start: u32,
    pub end: u32,
    /// Every instruction on the loop's cycle, its trampolines' included.
    pub body: Vec<u32>,
}

/// Is `pc` main-RAM code, the only code a RAM dump holds?
fn in_ram(pc: u32) -> bool {
    pc & 0xffe0_0000 == 0x8000_0000
}

/// The instructions between `start` and the delay slot after `closer` that
/// lie on some path from `start` back to `closer`.
fn cycle(code: &Code<'_>, start: u32, closer: u32) -> Vec<u32> {
    let end = closer + 4;
    let span = |pc: u32| (start..=end).contains(&pc);
    let mut successors: HashMap<u32, Vec<u32>> = HashMap::new();
    for pc in (start..=end).step_by(4) {
        // A branch's delay slot leaves for wherever the branch goes.
        let after = match (pc > start).then(|| code.unpatched(pc - 4)) {
            Some(Op::Branch { target, link, .. }) if !link => vec![target, pc + 4],
            Some(Op::Jump {
                target,
                link: false,
            }) => vec![target],
            Some(Op::JumpRegister { .. }) => vec![],
            _ => vec![pc + 4],
        };
        let after = if pc == end {
            after.into_iter().filter(|&next| next == start).collect()
        } else {
            after.into_iter().filter(|&next| span(next)).collect()
        };
        successors.insert(pc, after);
    }
    let mut predecessors: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&pc, after) in &successors {
        for &next in after {
            predecessors.entry(next).or_default().push(pc);
        }
    }
    let reach = |from: u32, edges: &HashMap<u32, Vec<u32>>| {
        let mut seen = HashSet::new();
        let mut stack = vec![from];
        while let Some(pc) = stack.pop() {
            if seen.insert(pc) {
                stack.extend(edges.get(&pc).into_iter().flatten().copied());
            }
        }
        seen
    };
    let forward = reach(start, &successors);
    let backward = reach(end, &predecessors);
    let mut body: Vec<u32> = forward.intersection(&backward).copied().collect();
    body.sort_unstable();
    body
}

/// How a register's value behaves across the loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Value {
    /// The same on every iteration.
    Fixed,
    /// Steps by a fixed amount each time round (a spin budget).
    Counter,
    /// Came from a polled load.
    Polled,
    /// Carried round the loop in any other way.
    State,
}

/// Whether the cycle `body` is a wait loop (see the module comment).
fn waits(code: &Code<'_>, body: &[u32]) -> bool {
    let ops: Vec<(u32, Op)> = body.iter().map(|&pc| (pc, code.unpatched(pc))).collect();
    if ops.iter().any(|(_, op)| {
        matches!(
            op,
            Op::Effect
                | Op::Store { .. }
                | Op::JumpRegister { .. }
                | Op::Branch { link: true, .. }
                | Op::Jump { link: true, .. }
        )
    }) {
        return false;
    }
    let written: HashSet<u8> = ops.iter().flat_map(|(_, op)| op.defs()).copied().collect();
    // What each register holds when an iteration starts: a counter if every
    // write to it steps it, otherwise state carried from the last iteration.
    let mut value = [Value::Fixed; 34];
    for &reg in &written {
        let steps = ops.iter().all(|(pc, op)| {
            !op.defs().contains(&reg) || op.counts(code.word(*pc).unwrap_or(0), reg, &written)
        });
        value[reg as usize] = if steps { Value::Counter } else { Value::State };
    }
    // One iteration in address order, which is execution order for a loop
    // this small (a delay slot follows its branch's operand reads).
    let mut polls = 0;
    for (_, op) in &ops {
        match op {
            Op::Load { dst, base } => {
                let loaded = match base {
                    Some(SP) => Value::State,
                    Some(base) if value[*base as usize] != Value::Fixed => return false,
                    _ => {
                        polls += 1;
                        Value::Polled
                    }
                };
                if let Some(dst) = dst {
                    value[*dst as usize] = loaded;
                }
            }
            Op::Branch { srcs, .. }
                if srcs.iter().any(|&reg| value[reg as usize] == Value::State) =>
            {
                return false;
            }
            Op::Alu { dst, srcs } => {
                let result = srcs
                    .iter()
                    .map(|&reg| value[reg as usize])
                    .max()
                    .unwrap_or(Value::Fixed);
                for &reg in dst {
                    value[reg as usize] = result;
                }
            }
            _ => {}
        }
    }
    polls > 0
}

/// Every instruction word the replay ran, from the addresses its logs count
/// (`unit`: 4 for words, 16 for I-cache lines).
pub fn executed(counted: impl IntoIterator<Item = u32>, unit: u32) -> BTreeSet<u32> {
    counted
        .into_iter()
        .filter(|&at| in_ram(at))
        .flat_map(|at| (at..at + unit).step_by(4))
        .collect()
}

/// Where each executed direct branch, jump and call can go, as the
/// compiler emitted it: a hazard trampoline's own jumps back into the code
/// are part of the site that jumps to it.
fn sources(code: &Code<'_>, executed: &BTreeSet<u32>) -> HashMap<u32, Vec<u32>> {
    let trampolines: HashSet<u32> = executed
        .iter()
        .filter_map(|&pc| code.trampoline(pc))
        .flat_map(|(_, tramp, words)| (0..words).map(move |index| tramp + 4 * index))
        .collect();
    let mut sources: HashMap<u32, Vec<u32>> = HashMap::new();
    for &pc in executed {
        if trampolines.contains(&pc) {
            continue;
        }
        if let Some(target) = code.unpatched(pc).target() {
            sources.entry(target).or_default().push(pc);
        }
    }
    sources
}

/// Every wait loop among the `executed` instruction words, read from `ram`.
pub fn wait_loops(ram: &[u8], executed: &BTreeSet<u32>) -> Vec<WaitLoop> {
    let code = Code::new(ram);
    // To spot a loop that code after it jumps back into.
    let sources = sources(&code, executed);
    let mut found = Vec::new();
    for &closer in executed {
        let op = code.unpatched(closer);
        let (Op::Branch { target, .. } | Op::Jump { target, .. }) = op else {
            continue;
        };
        if target > closer || closer - target > 4 * MAX_SPAN || !in_ram(target) {
            continue;
        }
        let body = cycle(&code, target, closer);
        if !body.contains(&closer) {
            continue;
        }
        let members: HashSet<u32> = body.iter().copied().collect();
        // One cycle: no other backward edge inside it.
        let nested = body.iter().any(|&pc| {
            pc != closer
                && code
                    .unpatched(pc)
                    .target()
                    .is_some_and(|to| to <= pc && members.contains(&to))
        });
        // Closed: only code before the loop may jump into it.
        let open = body.iter().any(|pc| {
            sources
                .get(pc)
                .into_iter()
                .flatten()
                .any(|from| *from >= target && !members.contains(from))
        });
        if !nested && !open && waits(&code, &body) {
            let mut body = body;
            let detours: Vec<u32> = body
                .iter()
                .filter_map(|&pc| code.trampoline(pc))
                .flat_map(|(_, tramp, words)| (0..words).map(move |index| tramp + 4 * index))
                .collect();
            body.extend(detours);
            body.sort_unstable();
            body.dedup();
            found.push(WaitLoop {
                start: target,
                end: closer + 4,
                body,
            });
        }
    }
    found
}

const RA: u8 = 31;

/// The successor of a function's return, as a node (no word is odd).
const EXIT: u32 = 1;

/// Most instructions a callee may reach, calls stepped over, before it is
/// left as work.
const MAX_CALLEE: usize = 1024;

/// Most callees `attribute` looks at below one span's wait loops.
const MAX_CALLEES: usize = 256;

/// Whether `op` leaves everything but its own stack frame alone. A call is
/// judged by its callee, apart.
fn quiet(op: &Op) -> bool {
    matches!(
        op,
        Op::Alu { .. }
            | Op::Load { .. }
            | Op::Branch { link: false, .. }
            | Op::Jump { .. }
            | Op::JumpRegister {
                rs: RA,
                link: false
            }
            | Op::Store { base: SP }
    )
}

/// A call instruction's target.
fn call_target(op: &Op) -> Option<u32> {
    match op {
        Op::Jump { target, link: true } => Some(*target),
        _ => None,
    }
}

/// Where control goes after `pc` inside one call of a function, calls
/// stepped over: a delay slot leaves for wherever its branch goes, the
/// delay slot of `jr ra` for [`EXIT`], and a register jump anywhere else
/// nowhere this analysis follows. A function's first instruction is never
/// a delay slot.
fn step(code: &Code<'_>, pc: u32, entry: bool) -> Vec<u32> {
    let before = if entry {
        Op::Alu {
            dst: vec![],
            srcs: vec![],
        }
    } else {
        code.op(pc.wrapping_sub(4))
    };
    match before {
        Op::Branch {
            target,
            link: false,
            ..
        } => vec![target, pc + 4],
        Op::Jump {
            target,
            link: false,
        } => vec![target],
        Op::JumpRegister {
            rs: RA,
            link: false,
        } => vec![EXIT],
        Op::JumpRegister { link: false, .. } | Op::Branch { link: true, .. } => vec![],
        // A call returns to the instruction after its delay slot.
        _ => vec![pc + 4],
    }
}

/// A callee as far as [`attribute`] needs it.
struct Callee {
    /// Whether it has a quiet path: one from its entry to its return that
    /// changes nothing outside its own stack frame, runs no instruction
    /// twice, and calls only callees that have such a path themselves.
    quiet: bool,
    /// The instructions on quiet paths that run only from the entry.
    counted: BTreeSet<u32>,
    /// `(site, target)` for the calls among them.
    calls: Vec<(u32, u32)>,
}

/// Callees analysed so far, by entry: `None` for one left as work.
struct Callees<'a> {
    code: Code<'a>,
    sources: HashMap<u32, Vec<u32>>,
    known: HashMap<u32, Option<Callee>>,
    open: HashSet<u32>,
}

impl Callees<'_> {
    /// Analyse the function at `entry`, unless it is already known.
    fn analyse(&mut self, entry: u32) {
        if self.known.contains_key(&entry) || self.open.contains(&entry) {
            return;
        }
        if self.known.len() >= MAX_CALLEES {
            self.known.insert(entry, None);
            return;
        }
        self.open.insert(entry);
        let callee = self.callee(entry);
        self.open.remove(&entry);
        self.known.insert(entry, callee);
    }

    /// Whether the function at `entry` has a quiet path (a recursive call
    /// has none).
    fn has_quiet_path(&mut self, entry: u32) -> bool {
        self.analyse(entry);
        self.known
            .get(&entry)
            .is_some_and(|callee| callee.as_ref().is_some_and(|callee| callee.quiet))
    }

    fn callee(&mut self, entry: u32) -> Option<Callee> {
        // Everything one call can run, calls stepped over.
        let mut successors: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut stack = vec![entry];
        while let Some(pc) = stack.pop() {
            if pc == EXIT || successors.contains_key(&pc) {
                continue;
            }
            if !in_ram(pc) || successors.len() >= MAX_CALLEE {
                return None;
            }
            let after = step(&self.code, pc, pc == entry);
            stack.extend(&after);
            successors.insert(pc, after);
        }
        let reach = |from: u32, edges: &HashMap<u32, Vec<u32>>, allowed: &dyn Fn(u32) -> bool| {
            let mut seen = HashSet::new();
            let mut stack = vec![from];
            while let Some(pc) = stack.pop() {
                if allowed(pc) && seen.insert(pc) {
                    stack.extend(edges.get(&pc).into_iter().flatten().copied());
                }
            }
            seen
        };
        // An instruction on a cycle can run more than once per call.
        let looping: HashSet<u32> = successors
            .iter()
            .filter(|(pc, after)| {
                after
                    .iter()
                    .any(|&next| reach(next, &successors, &|_| true).contains(pc))
            })
            .map(|(pc, _)| *pc)
            .collect();
        let mut allowed: HashSet<u32> = successors
            .keys()
            .copied()
            .filter(|pc| quiet(&self.code.op(*pc)) && !looping.contains(pc))
            .chain([EXIT])
            .collect();
        let mut predecessors: HashMap<u32, Vec<u32>> = HashMap::new();
        for (&pc, after) in &successors {
            for &next in after {
                predecessors.entry(next).or_default().push(pc);
            }
        }
        // Judge only the calls on an otherwise quiet path, so a callee
        // reached only through real work is never looked at.
        let quiet = loop {
            let forward = reach(entry, &successors, &|pc| allowed.contains(&pc));
            let backward = reach(EXIT, &predecessors, &|pc| forward.contains(&pc));
            // In address order, so the callee budget runs out the same way
            // on every run.
            let mut path: Vec<u32> = backward.iter().copied().collect();
            path.sort_unstable();
            let loud: Vec<u32> = path
                .into_iter()
                .filter(|&pc| {
                    call_target(&self.code.op(pc))
                        .is_some_and(|target| !self.has_quiet_path(target))
                })
                .collect();
            if loud.is_empty() {
                break backward;
            }
            for pc in loud {
                allowed.remove(&pc);
            }
        };
        // An executed branch, jump or call from anywhere else into this
        // code (a switch's cases rejoining a shared return, say) may run
        // what follows it outside a call, or twice in one, so none of that
        // is counted. The rest runs only from the entry, once per call.
        let entered: Vec<u32> = successors
            .keys()
            .copied()
            .filter(|&pc| {
                pc != entry
                    && self.sources.get(&pc).into_iter().flatten().any(|from| {
                        !successors.contains_key(from)
                            || call_target(&self.code.op(*from)).is_some()
                    })
            })
            .collect();
        let mut shared = HashSet::new();
        for pc in entered {
            shared.extend(reach(pc, &successors, &|_| true));
        }
        let counted: BTreeSet<u32> = quiet
            .iter()
            .copied()
            .filter(|pc| *pc != EXIT && !shared.contains(pc))
            .collect();
        let calls = counted
            .iter()
            .filter_map(|&pc| Some((pc, call_target(&self.code.op(pc))?)))
            .collect();
        Some(Callee {
            quiet: !quiet.is_empty(),
            counted,
            calls,
        })
    }
}

/// What the calls a wait loop makes spend waiting, per instruction word:
/// `sites` are those calls, `counts` the replay's exact per-word counts.
///
/// A callee is shared (HK's `input::checkpoint` runs from its present loop
/// and from inside real work), and the counts cannot tell whose call ran
/// an instruction, so this counts a lower bound. For a callee called `n`
/// times from waiting and `other` times in all else, an instruction on a
/// quiet path (see [`Callee`]) that ran `c` times ran at least
/// `c - other` times for the waiting calls, because each call runs it at
/// most once. Its own calls pass that bound down the same way. Anything
/// with a side effect, and anything a call does only now and then (a pad
/// poll due once a vblank), stays work.
pub fn attribute(
    ram: &[u8],
    counts: &HashMap<u32, u64>,
    sites: &[(u32, u32)],
) -> HashMap<u32, u64> {
    let code = Code::new(ram);
    let executed = executed(counts.keys().copied(), 4);
    let mut callees = Callees {
        sources: sources(&code, &executed),
        code: Code::new(ram),
        known: HashMap::new(),
        open: HashSet::new(),
    };
    let count = |pc: u32| counts.get(&pc).copied().unwrap_or(0);
    let mut waiting: HashMap<u32, u64> = HashMap::new();
    let mut stack = Vec::new();
    for &(site, target) in sites {
        *waiting.entry(target).or_default() += count(site);
        stack.push(target);
    }
    // Every callee reachable through quiet calls, and how many callers
    // each has among them.
    let mut callers: HashMap<u32, usize> = HashMap::new();
    let mut seen = HashSet::new();
    while let Some(entry) = stack.pop() {
        if !seen.insert(entry) {
            continue;
        }
        callees.analyse(entry);
        if let Some(Some(callee)) = callees.known.get(&entry) {
            for &(_, target) in &callee.calls {
                *callers.entry(target).or_default() += 1;
                stack.push(target);
            }
        }
    }
    // Callers before callees, so every bound a callee gets is in first.
    // Quiet calls never form a cycle: a call back into a callee still being
    // analysed counts as work.
    let mut ready: Vec<u32> = seen
        .iter()
        .copied()
        .filter(|entry| !callers.contains_key(entry))
        .collect();
    let mut wait = HashMap::new();
    while let Some(entry) = ready.pop() {
        let Some(Some(callee)) = callees.known.get(&entry) else {
            continue;
        };
        let calls = waiting.get(&entry).copied().unwrap_or(0);
        let other = count(entry).saturating_sub(calls);
        for &pc in &callee.counted {
            let bound = count(pc).saturating_sub(other).min(calls);
            if bound > 0 {
                wait.insert(pc, bound);
            }
        }
        for &(site, target) in &callee.calls {
            let bound = count(site).saturating_sub(other).min(calls);
            *waiting.entry(target).or_default() += bound;
            let left = callers.get_mut(&target).expect("counted above");
            *left -= 1;
            if *left == 0 {
                ready.push(target);
            }
        }
    }
    wait
}

/// A caller-named wait loop (`measure --wait-range`) and what it added.
pub struct Named {
    pub start: u32,
    pub end: u32,
    /// Instructions it ran itself.
    pub own: u64,
    /// Calls it makes.
    pub calls: usize,
    /// What those calls spent waiting, when the counts are per word.
    pub called: Option<u64>,
}

/// A replay's instructions split into work and waiting.
pub struct Split {
    /// Instructions spent waiting, per counted address; never more than
    /// its count.
    pub wait: HashMap<u32, u64>,
    /// The wait loops found, with the instructions each ran.
    pub loops: Vec<(WaitLoop, u64)>,
    pub named: Vec<Named>,
}

/// Split the instructions `counts` holds (per 4-byte word or 16-byte line,
/// `unit`) into work and waiting: the wait loops found in `ram`, and the
/// caller's `ranges` (START..END), each a wait loop the rule above cannot
/// see, like HK's present loop, which calls out to poll. A range counts
/// what it ran itself as waiting, bar any unit holding a store outside the
/// stack, a coprocessor write or a GTE command; with per-word counts, its
/// calls count what [`attribute`] can prove they spent waiting.
pub fn split(
    ram: &[u8],
    counts: &HashMap<u32, u64>,
    unit: u32,
    ranges: &[(u32, u32)],
) -> Result<Split, String> {
    let code = Code::new(ram);
    let on = |at: u32| counts.get(&at).copied().unwrap_or(0);
    let units = |words: &mut dyn Iterator<Item = u32>| {
        let mut units: Vec<u32> = words.map(|pc| pc & !(unit - 1)).collect();
        units.sort_unstable();
        units.dedup();
        units
    };
    let executed = executed(counts.keys().copied(), unit);
    let mut wait = HashMap::new();
    let mut loops = Vec::new();
    for found in wait_loops(ram, &executed) {
        let body = units(&mut found.body.iter().copied());
        let count = body.iter().map(|&at| on(at)).sum();
        for at in body {
            wait.insert(at, on(at));
        }
        loops.push((found, count));
    }
    let mut named = Vec::new();
    for &(start, end) in ranges {
        let inside: Vec<u32> = executed.range(start..end).copied().collect();
        if inside.is_empty() {
            return Err(format!(
                "--wait-range {start:#010x}..{end:#010x} ran no instructions; its addresses \
                 belong to one build's layout"
            ));
        }
        let loud = units(&mut inside.iter().copied().filter(|&pc| !quiet(&code.op(pc))));
        let quiet = units(&mut inside.iter().copied());
        let quiet: Vec<u32> = quiet.into_iter().filter(|at| !loud.contains(at)).collect();
        for &at in &quiet {
            wait.insert(at, on(at));
        }
        let sites: Vec<(u32, u32)> = inside
            .iter()
            .filter_map(|&pc| Some((pc, call_target(&code.op(pc))?)))
            .collect();
        let called = (unit == 4).then(|| {
            let called = attribute(ram, counts, &sites);
            let total = called.values().sum();
            for (at, count) in called {
                let entry = wait.entry(at).or_default();
                *entry = (*entry).max(count);
            }
            total
        });
        named.push(Named {
            start,
            end,
            own: quiet.iter().map(|&at| on(at)).sum(),
            calls: sites.len(),
            called,
        });
    }
    Ok(Split { wait, loops, named })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u32 = 0x8001_0000;

    /// A RAM image holding `words` at BASE.
    fn ram(words: &[u32]) -> Vec<u8> {
        let mut ram = vec![0u8; 0x0002_0000];
        for (index, word) in words.iter().enumerate() {
            let at = (BASE & 0x001f_ffff) as usize + 4 * index;
            ram[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
        ram
    }

    fn lines(count: usize) -> BTreeSet<u32> {
        (0..count as u32).map(|index| BASE + 16 * index).collect()
    }

    fn found(words: &[u32]) -> Vec<(u32, u32)> {
        wait_loops(&ram(words), &executed(lines(words.len().div_ceil(4)), 16))
            .into_iter()
            .map(|found| (found.start - BASE, found.end - BASE))
            .collect()
    }

    // Encoders for the few instructions the tests need.
    fn i(op: u32, rs: u32, rt: u32, imm: i32) -> u32 {
        (op << 26) | (rs << 21) | (rt << 16) | (imm as u32 & 0xffff)
    }
    fn r(rs: u32, rt: u32, rd: u32, function: u32) -> u32 {
        (rs << 21) | (rt << 16) | (rd << 11) | function
    }
    /// Branch offset from the instruction at word `from` to word `to`.
    fn to(from: i32, to: i32) -> i32 {
        to - from - 1
    }
    fn j(to_word: u32) -> u32 {
        (2 << 26) | (((BASE + 4 * to_word) >> 2) & 0x03ff_ffff)
    }
    const NOP: u32 = 0;
    const AT: u32 = 1;
    const V0: u32 = 2;
    const V1: u32 = 3;
    const A0: u32 = 4;
    const A1: u32 = 5;
    const T0: u32 = 8;
    fn lw(rt: u32, base: u32, offset: i32) -> u32 {
        i(0x23, base, rt, offset)
    }
    fn lbu(rt: u32, base: u32, offset: i32) -> u32 {
        i(0x24, base, rt, offset)
    }
    fn sw(rt: u32, base: u32, offset: i32) -> u32 {
        i(0x2b, base, rt, offset)
    }
    fn beq(rs: u32, rt: u32, offset: i32) -> u32 {
        i(4, rs, rt, offset)
    }
    fn bne(rs: u32, rt: u32, offset: i32) -> u32 {
        i(5, rs, rt, offset)
    }
    fn addiu(rt: u32, rs: u32, imm: i32) -> u32 {
        i(9, rs, rt, imm)
    }
    fn lui(rt: u32, imm: i32) -> u32 {
        i(0xf, 0, rt, imm)
    }
    fn and(rd: u32, rs: u32, rt: u32) -> u32 {
        r(rs, rt, rd, 0x24)
    }

    #[test]
    fn a_vblank_counter_poll_waits() {
        // wait_vblank: while vblank_count() == v {}
        let code = [
            lui(V0, 0x800c),
            lw(V1, V0, 0x79b0),
            lw(A0, V0, 0x79b0), // 2: loop
            NOP,
            beq(A0, V1, to(4, 2)),
            NOP,
        ];
        assert_eq!(found(&code), vec![(8, 20)]);
    }

    #[test]
    fn a_bounded_gpustat_wait_counts_its_spins_and_waits() {
        // psx_io::gpu::wait_ready, as present_pending inlines it.
        let code = [
            lui(AT, 0x1f80),
            i(0xd, AT, A1, 0x1814), // ori a1, at, GPUSTAT
            addiu(V1, V1, -1),      // 2: loop
            beq(V1, 0, to(3, 10)),  // timeout
            NOP,
            lw(AT, A1, 0),
            NOP,
            and(AT, AT, V0),
            beq(AT, 0, to(8, 2)),
            NOP,
            NOP, // 10: timeout path
        ];
        assert_eq!(found(&code), vec![(8, 36)]);
    }

    #[test]
    fn a_loop_closed_by_a_jump_still_waits() {
        // A profile-guided layout rotates the loop: exit forward, `j` back.
        let code = [
            lw(AT, A0, 0), // 0: loop
            NOP,
            bne(AT, 0, to(2, 6)),
            NOP,
            j(0),
            NOP,
            NOP, // 6: exit
        ];
        assert_eq!(found(&code), vec![(0, 20)]);
    }

    #[test]
    fn a_loop_through_a_hazard_trampoline_still_waits() {
        // NitroXide's flip wait after hazard_patch.py: the slot load of the
        // exit branch raced `subu`, so the branch became `j TRAMP`, and the
        // trampoline re-evaluates it and jumps back to the fall-through.
        let v1_minus_a2 = r(V1, 6, AT, 0x23); // subu at, v1, a2
        let mut code = vec![
            lw(AT, A0, 0), // 0: loop
            NOP,
            j(16), // was beq at, zero, 8
            lw(V1, A1, 0),
            v1_minus_a2,       // 4
            i(0xb, AT, AT, 9), // sltiu at, at, 9
            bne(AT, 0, to(6, 0)),
            NOP,
            NOP, // 8: exit
        ];
        code.resize(16, NOP);
        // 16: TRAMP
        code.extend([beq(AT, 0, 3), NOP, j(4), NOP, j(8), NOP]);
        let ram = ram(&code);
        let loops = wait_loops(&ram, &executed(lines(code.len().div_ceil(4)), 16));
        assert_eq!(loops.len(), 1);
        assert_eq!((loops[0].start - BASE, loops[0].end - BASE), (0, 28));
        // The trampoline's words run once per iteration: they wait too.
        let tramp: Vec<u32> = (16..22).map(|word| BASE + 4 * word).collect();
        assert!(tramp.iter().all(|pc| loops[0].body.contains(pc)));
        // A stub of any other shape is followed as written, and the loop
        // never gets back from it.
        let mut other = code.clone();
        other[18] = j(5);
        assert!(found(&other).is_empty());
    }

    #[test]
    fn a_status_read_in_a_fixed_delay_waits() {
        // psx_pad's setup delay reads SIO_STAT 1024 times and ignores it.
        let code = [
            lw(AT, V0, 0), // 0: loop
            addiu(V1, V1, 1),
            bne(V1, 0, to(2, 0)),
            NOP,
        ];
        assert_eq!(found(&code), vec![(0, 12)]);
    }

    #[test]
    fn loops_that_walk_memory_or_store_are_work() {
        // strlen: the address moves.
        let strlen = [
            lbu(AT, A0, 0), // 0
            addiu(A0, A0, 1),
            bne(AT, 0, to(2, 0)),
            NOP,
        ];
        assert!(found(&strlen).is_empty());
        // A copy stores.
        let copy = [
            lw(AT, A0, 0), // 0
            addiu(V1, V1, -1),
            sw(AT, A1, 0),
            bne(V1, 0, to(3, 0)),
            NOP,
        ];
        assert!(found(&copy).is_empty());
        // A register-only delay loop reads nothing.
        let delay = [addiu(V1, V1, -1), bne(V1, 0, to(1, 0)), NOP];
        assert!(found(&delay).is_empty());
        // A stack reload of a spilled bound is not a poll.
        let spilled = [
            lw(AT, SP as u32, 16), // 0
            addiu(V1, V1, 1),
            bne(V1, AT, to(2, 0)),
            NOP,
        ];
        assert!(found(&spilled).is_empty());
    }

    #[test]
    fn a_branch_on_carried_state_is_work() {
        // Cortex's face selection: the flag set in the delay slot ends the
        // second pass, so the fixed-address loads are not a poll.
        let code = [
            i(0xc, T0, AT, 1),    // 0: andi at, t0, 1
            bne(AT, 0, to(1, 8)), // exit
            addiu(T0, 0, 1),      // li t0, 1
            lw(V1, A0, 0),
            NOP,
            beq(V1, 0, to(5, 0)),
            NOP,
            NOP,
            NOP, // 8: exit
        ];
        assert!(found(&code).is_empty());
    }

    #[test]
    fn a_piece_of_a_larger_loop_is_work() {
        // Code after the candidate jumps back into it (a binary search
        // whose bounds move outside the span).
        let code = [
            lw(AT, A0, 0), // 0
            NOP,
            beq(AT, V0, to(2, 0)),
            NOP,
            addiu(A0, A0, 4),
            j(0),
            NOP,
        ];
        assert!(found(&code).is_empty());
    }

    #[test]
    fn a_loop_with_an_inner_loop_is_work() {
        // A bitmap scan: the inner loop moves the address.
        let code = [
            lw(AT, A1, 0), // 0: outer
            NOP,
            and(AT, AT, V0), // 2: inner
            bne(AT, 0, to(3, 7)),
            NOP,
            bne(V1, 0, to(5, 2)),
            NOP,
            beq(AT, 0, to(7, 0)), // 7
            NOP,
        ];
        assert!(found(&code).is_empty());
    }

    // --- Calls from a wait loop -------------------------------------------

    fn jal(to_word: u32) -> u32 {
        (3 << 26) | (((BASE + 4 * to_word) >> 2) & 0x03ff_ffff)
    }
    const JR_RA: u32 = (31 << 21) | 8;
    const SP_: u32 = SP as u32;
    const S0: u32 = 16;

    /// `(word index, instruction, count)` rows as a RAM image and exact
    /// per-word counts.
    fn program(rows: &[(u32, u32, u64)]) -> (Vec<u8>, HashMap<u32, u64>) {
        let size = rows.iter().map(|row| row.0 + 1).max().unwrap_or(0) as usize;
        let mut words = vec![NOP; size];
        let mut counts = HashMap::new();
        for &(index, word, count) in rows {
            words[index as usize] = word;
            if count > 0 {
                counts.insert(BASE + 4 * index, count);
            }
        }
        (ram(&words), counts)
    }

    /// `attribute` for calls from the given sites (word indices), as word
    /// index to wait count, sorted.
    fn attributed(rows: &[(u32, u32, u64)], sites: &[u32]) -> Vec<(u32, u64)> {
        let (ram, counts) = program(rows);
        let code = Code::new(&ram);
        let sites: Vec<(u32, u32)> = sites
            .iter()
            .map(|&site| {
                let pc = BASE + 4 * site;
                (pc, call_target(&code.op(pc)).expect("a call"))
            })
            .collect();
        let mut found: Vec<(u32, u64)> = attribute(&ram, &counts, &sites)
            .into_iter()
            .map(|(pc, count)| ((pc - BASE) / 4, count))
            .collect();
        found.sort_unstable();
        found
    }

    /// A poll at word 32: read a status register, return a bit of it.
    fn poll(calls: u64) -> Vec<(u32, u32, u64)> {
        vec![
            (32, lui(V0, 0x1f80), calls),
            (33, lw(V0, V0, 0x10a8), calls),
            (34, NOP, calls),
            (35, JR_RA, calls),
            (36, and(V0, V0, A0), calls),
        ]
    }

    #[test]
    fn a_polled_callee_waits_for_the_calls_a_wait_loop_makes() {
        // The loop at 0 makes 100 calls; code at 20 makes 10 more.
        let mut rows = vec![
            (0, jal(32), 100),
            (1, NOP, 100),
            (20, jal(32), 10),
            (21, NOP, 10),
        ];
        rows.extend(poll(110));
        let waited: Vec<(u32, u64)> = (32..37).map(|word| (word, 100)).collect();
        assert_eq!(attributed(&rows, &[0]), waited);
    }

    #[test]
    fn a_callee_mostly_called_from_work_waits_only_for_what_is_proven() {
        // 100 waiting calls, 1000 from work. A branch skips word 35 on some
        // calls: it ran 50 times, which the work calls alone could explain.
        let rows = vec![
            (0, jal(32), 100),
            (1, NOP, 100),
            (20, jal(32), 1000),
            (21, NOP, 1000),
            (32, lw(V0, A0, 0), 1100),
            (33, NOP, 1100),
            (34, beq(V0, 0, to(34, 36)), 1100),
            (35, NOP, 1100),
            (36, addiu(V0, V0, 1), 50),
            (37, JR_RA, 1100),
            (38, NOP, 1100),
        ];
        let waited: Vec<(u32, u64)> = [32, 33, 34, 35, 37, 38]
            .into_iter()
            .map(|word| (word, 100))
            .collect();
        assert_eq!(attributed(&rows, &[0]), waited);
    }

    #[test]
    fn a_callee_side_effect_stays_work() {
        // A callee that saves to its stack frame is quiet; its store to a
        // global, on the path it takes when something is due, is work, as
        // is everything only that path runs.
        let rows = vec![
            (0, jal(32), 100),
            (1, NOP, 100),
            (32, addiu(SP_, SP_, -8), 100),
            (33, sw(S0, SP_, 0), 100),
            (34, lw(V0, A0, 0), 100),
            (35, NOP, 100),
            (36, beq(V0, 0, to(36, 40)), 100),
            (37, NOP, 100),
            (38, addiu(V1, V1, 1), 100),
            (39, sw(V1, A1, 0), 100),
            (40, lw(S0, SP_, 0), 100),
            (41, JR_RA, 100),
            (42, addiu(SP_, SP_, 8), 100),
        ];
        let waited: Vec<(u32, u64)> = [32, 33, 34, 35, 36, 37, 40, 41, 42]
            .into_iter()
            .map(|word| (word, 100))
            .collect();
        assert_eq!(attributed(&rows, &[0]), waited);
    }

    #[test]
    fn a_nested_poll_waits_for_the_calls_proven_to_reach_it() {
        // The loop calls A (word 32) 100 times; work calls A 5 more times
        // and calls B (word 48) directly 7 times. A calls B on every call.
        let mut rows = vec![
            (0, jal(32), 100),
            (1, NOP, 100),
            (20, jal(32), 5),
            (21, NOP, 5),
            (24, jal(48), 7),
            (25, NOP, 7),
            (32, addiu(SP_, SP_, -8), 105),
            (33, sw(31, SP_, 4), 105),
            (34, jal(48), 105),
            (35, NOP, 105),
            (36, lw(31, SP_, 4), 105),
            (37, JR_RA, 105),
            (38, addiu(SP_, SP_, 8), 105),
        ];
        rows.extend(
            poll(112)
                .into_iter()
                .map(|(word, op, count)| (word + 16, op, count)),
        );
        let mut waited: Vec<(u32, u64)> = (32..39).map(|word| (word, 100)).collect();
        // B: 112 calls, at least 100 of them from A's waiting calls.
        waited.extend((48..53).map(|word| (word, 100)));
        assert_eq!(attributed(&rows, &[0]), waited);
    }

    #[test]
    fn a_callee_loop_or_a_shared_tail_stays_work() {
        // A delay loop in the callee can run many times per call: no quiet
        // path, so nothing waits.
        let delay = vec![
            (0, jal(32), 100),
            (1, NOP, 100),
            (32, addiu(V1, V1, -1), 5000),
            (33, bne(V1, 0, to(33, 32)), 5000),
            (34, NOP, 5000),
            (35, JR_RA, 100),
            (36, NOP, 100),
        ];
        assert!(attributed(&delay, &[0]).is_empty());
        // Code elsewhere (word 20) jumps into the callee's return: that tail
        // may run outside any call, so only the words before it wait.
        let mut shared = vec![
            (0, jal(32), 100),
            (1, NOP, 100),
            (20, j(35), 30),
            (21, NOP, 30),
        ];
        shared.extend(poll(100));
        shared.retain(|row| row.0 < 35);
        shared.extend([(35, JR_RA, 130), (36, and(V0, V0, A0), 130)]);
        let waited: Vec<(u32, u64)> = (32..35).map(|word| (word, 100)).collect();
        assert_eq!(attributed(&shared, &[0]), waited);
    }

    #[test]
    fn a_recursive_call_stays_work() {
        // Every entry runs the callee's words at most once, so the 50
        // recursive entries count against the 100 waiting calls like any
        // other caller's; the recursive call itself is left as work.
        let rows = vec![
            (0, jal(32), 100),
            (1, NOP, 100),
            (32, lw(V0, A0, 0), 150),
            (33, NOP, 150),
            (34, beq(V0, 0, to(34, 38)), 150),
            (35, NOP, 150),
            (36, jal(32), 50),
            (37, NOP, 50),
            (38, JR_RA, 150),
            (39, NOP, 150),
        ];
        let waited: Vec<(u32, u64)> = [32, 33, 34, 35, 38, 39]
            .into_iter()
            .map(|word| (word, 100))
            .collect();
        assert_eq!(attributed(&rows, &[0]), waited);
    }

    // --- Named wait loops -------------------------------------------------

    /// HK's present loop in miniature: poll a phase byte, call a poll, spill
    /// the clock to the stack, and loop until the phase is 3.
    fn present_loop(iterations: u64) -> Vec<(u32, u32, u64)> {
        let mut rows = vec![
            (0, lbu(V0, S0, 0), iterations),
            (1, jal(32), iterations),
            (2, NOP, iterations),
            (3, sw(V0, SP_, 16), iterations),
            (4, addiu(AT, V0, -3), iterations),
            (5, bne(AT, 0, to(5, 0)), iterations),
            (6, NOP, iterations),
        ];
        rows.extend(poll(iterations));
        rows
    }

    fn split_at(rows: &[(u32, u32, u64)], unit: u32, ranges: &[(u32, u32)]) -> Split {
        let (ram, counts) = program(rows);
        let counts = if unit == 4 {
            counts
        } else {
            let mut lines = HashMap::new();
            for (pc, count) in counts {
                *lines.entry(pc & !15).or_default() += count;
            }
            lines
        };
        let ranges: Vec<(u32, u32)> = ranges
            .iter()
            .map(|&(start, end)| (BASE + 4 * start, BASE + 4 * end))
            .collect();
        split(&ram, &counts, unit, &ranges).expect("the ranges ran")
    }

    #[test]
    fn a_named_loop_waits_with_its_calls() {
        let rows = present_loop(50);
        // The rule alone sees a store and a call: work.
        assert!(split_at(&rows, 4, &[]).wait.is_empty());
        let split = split_at(&rows, 4, &[(0, 7)]);
        let named = &split.named[0];
        assert_eq!(
            (named.own, named.calls, named.called),
            (7 * 50, 1, Some(5 * 50))
        );
        assert_eq!(split.wait.values().sum::<u64>(), 12 * 50);
    }

    #[test]
    fn a_named_loop_keeps_a_global_store_as_work() {
        // Word 3 stores to a global instead of the stack.
        let mut rows = present_loop(50);
        rows[3].1 = sw(V0, S0, 16);
        let split = split_at(&rows, 4, &[(0, 7)]);
        assert_eq!(split.named[0].own, 6 * 50);
        assert!(!split.wait.contains_key(&(BASE + 12)));
        // Per I-cache line, the line holding it stays work in full.
        let split = split_at(&rows, 16, &[(0, 7)]);
        assert_eq!(split.named[0].own, 3 * 50);
    }

    #[test]
    fn a_named_loop_leaves_its_calls_as_work_without_word_counts() {
        let split = split_at(&present_loop(50), 16, &[(0, 7)]);
        let named = &split.named[0];
        assert_eq!((named.own, named.calls, named.called), (7 * 50, 1, None));
        // The poll's lines are not waiting.
        assert!(!split.wait.contains_key(&(BASE + 128)));
    }

    #[test]
    fn a_named_range_that_ran_nothing_is_refused() {
        let (ram, counts) = program(&present_loop(50));
        assert!(split(&ram, &counts, 4, &[(BASE + 400, BASE + 420)]).is_err());
    }
}
