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
//! The emulator counts instructions per 16-byte I-cache line, so a line the
//! loop touches counts as wait in full. The instructions sharing such a line
//! with the loop run once per call, not once per iteration.

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
    /// JR or JALR.
    JumpRegister,
    /// Anything that is work by definition: a store, a call's side, a
    /// coprocessor write, a GTE command, an unknown opcode.
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
            0x08 => Op::JumpRegister,
            0x09 => Op::JumpRegister,
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
        _ => Op::Effect, // stores, COP0 writes, RFE, every GTE op, LWC2, SWC2
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
}

/// One loop found to be waiting: `start` (the backward branch's target)
/// through `end` (the closing branch's delay slot).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct WaitLoop {
    pub start: u32,
    pub end: u32,
    /// Every instruction on the loop's cycle.
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
        let after = match (pc > start).then(|| code.op(pc - 4)) {
            Some(Op::Branch { target, link, .. }) if !link => vec![target, pc + 4],
            Some(Op::Jump {
                target,
                link: false,
            }) => vec![target],
            Some(Op::JumpRegister) => vec![],
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
    let ops: Vec<(u32, Op)> = body.iter().map(|&pc| (pc, code.op(pc))).collect();
    if ops.iter().any(|(_, op)| {
        matches!(
            op,
            Op::Effect
                | Op::JumpRegister
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

/// Every wait loop among the instructions in `lines` (the 16-byte I-cache
/// lines the replay executed), read from `ram`.
pub fn wait_loops(ram: &[u8], lines: &BTreeSet<u32>) -> Vec<WaitLoop> {
    let code = Code::new(ram);
    let executed = || {
        lines
            .iter()
            .filter(|&&line| in_ram(line))
            .flat_map(|&line| (line..line + 16).step_by(4))
    };
    // Where each executed branch and jump can go, to spot a loop that code
    // after it jumps back into.
    let mut sources: HashMap<u32, Vec<u32>> = HashMap::new();
    for pc in executed() {
        if let Some(target) = code.op(pc).target() {
            sources.entry(target).or_default().push(pc);
        }
    }
    let mut found = Vec::new();
    for closer in executed() {
        let op = code.op(closer);
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
                    .op(pc)
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
            found.push(WaitLoop {
                start: target,
                end: closer + 4,
                body,
            });
        }
    }
    found
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
        wait_loops(&ram(words), &lines(words.len().div_ceil(4)))
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
}
