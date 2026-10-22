//! The N64 sequence bytecode ("aseq"), as far as a static scan needs it.
//!
//! A sequence is a three-level program: one sequence script starts up to 16
//! channel scripts, each channel script starts up to 4 layer scripts, and the
//! layers hold the notes. All three share one pool of control-flow opcodes
//! (`0xF2..=0xFF`). This module knows how long every opcode is at each level,
//! which is enough to walk every reachable path and answer the questions the
//! cook asks before any playback exists: which instruments and drums does
//! the song name, which opcodes does it use, does it patch itself.
//!
//! The scan is an upper bound. It follows both outcomes of every branch,
//! cannot resolve the targets of table-driven calls, and assumes a channel's
//! note encoding is the one in force where its layers are loaded.

use std::collections::{BTreeMap, BTreeSet};

/// Which of the three scripts an opcode belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// The sequence script.
    Seq,
    /// A channel script.
    Chan,
    /// A layer script.
    Layer,
}

/// Argument bytes after a flow-control opcode (`0xF2..=0xFF`).
pub fn flow_args(op: u8) -> usize {
    match op {
        0xf2..=0xf4 | 0xf8 => 1,
        0xf5 | 0xf9..=0xfc => 2,
        _ => 0,
    }
}

/// Argument bytes of a channel opcode `>= 0xB0` (the table-driven range),
/// before any extra bytes the opcode reads itself.
fn chan_table_args(op: u8) -> usize {
    match op {
        0xb0 | 0xb2 | 0xc2 | 0xcb | 0xce | 0xcf | 0xda | 0xde | 0xe7 => 2,
        0xb3 | 0xb7 | 0xb8 | 0xb9 | 0xba => 1,
        0xbb => 3,
        0xbc => 2,
        0xbd => 4,
        0xc1 | 0xc6 | 0xc8 | 0xc9 | 0xca | 0xcc | 0xcd => 1,
        0xc7 => 3,
        0xd0..=0xd9
        | 0xdb
        | 0xdc
        | 0xdd
        | 0xdf
        | 0xe0
        | 0xe3
        | 0xe5
        | 0xe6
        | 0xe9
        | 0xed
        | 0xee
        | 0xf1 => 1,
        0xe1 | 0xe2 | 0xe8 => 3,
        0xeb => 2,
        0xef => 3,
        _ => 0,
    }
}

/// What a scan found.
#[derive(Clone, Debug, Default)]
pub struct Scan {
    /// How often each opcode was met, by level.
    pub ops: BTreeMap<(Level, u8), u32>,
    /// Channel scripts reached.
    pub channels: BTreeSet<usize>,
    /// Layer scripts reached.
    pub layers: BTreeSet<usize>,
    /// `(font slot, instrument)` pairs named by `instr` and `fontinstr`.
    pub instruments: BTreeSet<(u8, u8)>,
    /// Font slots named by `font` and `fontinstr`.
    pub font_slots: BTreeSet<u8>,
    /// Drum keys played by layers that use the drum kit, with the font slot.
    pub drum_keys: BTreeSet<(u8, u8)>,
    /// A script in the song patches the sequence bytes.
    pub self_modifying: bool,
    /// Table-driven jumps or calls whose targets the scan cannot see.
    pub dynamic_targets: bool,
    /// The song reads or writes IO ports.
    pub io: bool,
    /// Offsets of the envelope arrays the song points channels and layers at.
    pub envelopes: BTreeSet<usize>,
    /// Opcodes the scan did not know the length of.
    pub unknown: BTreeSet<(Level, u8)>,
}

#[derive(Clone, Copy, Default)]
struct ChanState {
    large: bool,
    font: u8,
    inst: Option<u8>,
}

struct Scanner<'a> {
    seq: &'a [u8],
    out: Scan,
    seen: BTreeSet<(Level, usize, bool)>,
    work: Vec<(Level, usize, ChanState)>,
}

fn s16(seq: &[u8], at: usize) -> Option<usize> {
    Some(u16::from_be_bytes([*seq.get(at)?, *seq.get(at + 1)?]) as usize)
}

impl Scanner<'_> {
    fn byte(&self, at: usize) -> Option<u8> {
        self.seq.get(at).copied()
    }

    fn push(&mut self, lv: Level, pc: usize, st: ChanState) {
        if pc < self.seq.len() {
            self.work.push((lv, pc, st));
        }
    }

    fn count(&mut self, lv: Level, op: u8) {
        *self.out.ops.entry((lv, op)).or_insert(0) += 1;
    }

    /// Flow control shared by all levels. Returns false when the linear path
    /// ends.
    fn flow(&mut self, lv: Level, op: u8, pc: &mut usize, st: ChanState) -> bool {
        let arg = flow_args(op);
        let here = *pc;
        *pc += arg;
        let a16 = if arg == 2 { s16(self.seq, here) } else { None };
        match op {
            0xff => return false,
            0xfb => {
                if let Some(t) = a16 {
                    self.push(lv, t, st);
                }
                return false;
            }
            0xf5 | 0xf9 | 0xfa | 0xfc => {
                if let Some(t) = a16 {
                    self.push(lv, t, st);
                }
            }
            0xf2 | 0xf3 => {
                if let Some(o) = self.byte(here) {
                    self.push(lv, (*pc as isize + o as i8 as isize) as usize, st);
                }
            }
            0xf4 => {
                if let Some(o) = self.byte(here) {
                    self.push(lv, (*pc as isize + o as i8 as isize) as usize, st);
                }
                return false;
            }
            0xfd => {
                // delay with a compressed length
                if self.byte(*pc).is_some_and(|b| b & 0x80 != 0) {
                    *pc += 2;
                } else {
                    *pc += 1;
                }
            }
            _ => {}
        }
        true
    }

    fn run(&mut self) {
        while let Some((lv, start, st0)) = self.work.pop() {
            if !self.seen.insert((lv, start, st0.large)) {
                continue;
            }
            let mut pc = start;
            let mut st = st0;
            let mut steps = 0u32;
            while pc < self.seq.len() && steps < 20_000 {
                steps += 1;
                let op = self.seq[pc];
                pc += 1;
                self.count(lv, op);
                if op >= 0xf2 {
                    if !self.flow(lv, op, &mut pc, st) {
                        break;
                    }
                    continue;
                }
                let go = match lv {
                    Level::Seq => self.seq_op(op, &mut pc),
                    Level::Chan => self.chan_op(op, &mut pc, &mut st),
                    Level::Layer => self.layer_op(op, &mut pc, &mut st),
                };
                if !go {
                    break;
                }
            }
        }
    }

    fn seq_op(&mut self, op: u8, pc: &mut usize) -> bool {
        let here = *pc;
        match op {
            0x90..=0x9f => {
                if let Some(t) = s16(self.seq, here) {
                    let ch = (op & 15) as usize;
                    self.out.channels.insert(ch);
                    self.push(Level::Chan, t, ChanState::default());
                }
                *pc += 2;
            }
            0xa0..=0xaf => {
                if let Some(o) = s16(self.seq, here) {
                    let ch = (op & 15) as usize;
                    self.out.channels.insert(ch);
                    let t = (*pc as isize + 2 + o as u16 as i16 as isize) as usize;
                    self.push(Level::Chan, t, ChanState::default());
                }
                *pc += 2;
            }
            0x60..=0x6f => *pc += 2,
            0xb0..=0xbf => *pc += 3,
            0x70..=0x8f => self.out.io = true,
            0xc4 => *pc += 2,
            0xc5 => *pc += 2,
            0xc6 => return false,
            0xc7 => {
                self.out.self_modifying = true;
                *pc += 3;
            }
            0xc8 | 0xc9 | 0xcc | 0xce | 0xd0 | 0xd3 | 0xd5 | 0xd9 | 0xdb | 0xdc | 0xdd | 0xde
            | 0xdf | 0xf1 => *pc += 1,
            0xcd => {
                self.out.dynamic_targets = true;
                *pc += 2;
            }
            0xd1 | 0xd2 | 0xd6 | 0xd7 => *pc += 2,
            0xda => *pc += 3,
            0xef => *pc += 3,
            _ => {}
        }
        true
    }

    fn chan_op(&mut self, op: u8, pc: &mut usize, st: &mut ChanState) -> bool {
        let here = *pc;
        if op >= 0xb0 {
            let n = chan_table_args(op);
            *pc += n;
            match op {
                0xc1 => {
                    let i = self.byte(here).unwrap_or(0);
                    st.inst = Some(i);
                    if i < 0x7e {
                        self.out.instruments.insert((st.font, i));
                    }
                }
                0xc3 => st.large = false,
                0xc4 => st.large = true,
                0xc6 => {
                    let slot = self.byte(here).unwrap_or(0);
                    st.font = slot;
                    self.out.font_slots.insert(slot);
                }
                0xeb => {
                    let slot = self.byte(here).unwrap_or(0);
                    let i = self.byte(here + 1).unwrap_or(0);
                    st.font = slot;
                    st.inst = Some(i);
                    self.out.font_slots.insert(slot);
                    if i < 0x7e {
                        self.out.instruments.insert((slot, i));
                    }
                }
                0xc7 | 0xcf => self.out.self_modifying = true,
                0xc2 | 0xe4 | 0xc5 | 0xb4 | 0xb5 | 0xb6 => self.out.dynamic_targets = true,
                0xda => {
                    if let Some(t) = s16(self.seq, here) {
                        self.out.envelopes.insert(t);
                    }
                }
                0xe8 => *pc += 5,
                0xea => return false,
                _ => {}
            }
            return true;
        }
        if op >= 0x70 {
            match op & 0xf8 {
                0x78 | 0x88 => {
                    // rldlayer is relative to the pc after its argument
                    let target = if op & 0xf8 == 0x88 {
                        s16(self.seq, here)
                    } else {
                        s16(self.seq, here)
                            .map(|o| (here as isize + 2 + o as u16 as i16 as isize) as usize)
                    };
                    if let Some(t) = target {
                        self.out.layers.insert(t);
                        self.push(Level::Layer, t, *st);
                    }
                    *pc += 2;
                }
                0x70 => self.out.io = true,
                0x98 => self.out.dynamic_targets = true,
                _ => {}
            }
            return true;
        }
        match op & 0xf0 {
            0x20 => {
                if let Some(t) = s16(self.seq, here) {
                    self.push(Level::Chan, t, *st);
                }
                *pc += 2;
            }
            0x30 | 0x40 => {
                self.out.io = true;
                *pc += 1;
            }
            0x50 | 0x60 => self.out.io = true,
            _ => {}
        }
        true
    }

    fn layer_op(&mut self, op: u8, pc: &mut usize, st: &mut ChanState) -> bool {
        let here = *pc;
        let compressed = |s: &Self, at: usize| -> usize {
            if s.byte(at).is_some_and(|b| b & 0x80 != 0) {
                2
            } else {
                1
            }
        };
        if op < 0xc0 {
            // a note: the arguments depend on the channel's note encoding
            let kind = op & 0xc0;
            if st.large {
                match kind {
                    0x00 => *pc += compressed(self, here) + 2,
                    0x40 => *pc += compressed(self, here) + 1,
                    _ => *pc += 2,
                }
            } else if kind == 0x00 {
                *pc += compressed(self, here);
            }
            if st.inst == Some(0x7f) {
                self.out.drum_keys.insert((st.font, op & 0x3f));
            }
            return true;
        }
        match op {
            0xc0 | 0xc3 => *pc += compressed(self, here),
            0xc1 | 0xc2 | 0xc9 | 0xca | 0xcd | 0xce | 0xcf => *pc += 1,
            0xc6 => {
                let i = self.byte(here).unwrap_or(0xff);
                *pc += 1;
                if i < 0xff {
                    st.inst = Some(i);
                    if i < 0x7e {
                        self.out.instruments.insert((st.font, i));
                    }
                }
            }
            0xc7 => {
                let first = self.byte(here).unwrap_or(0);
                *pc += 2;
                if first & 0x80 != 0 {
                    *pc += 1;
                } else {
                    *pc += compressed(self, here + 2);
                }
            }
            0xcb => {
                if let Some(t) = s16(self.seq, here) {
                    self.out.envelopes.insert(t);
                }
                *pc += 3;
            }
            _ => {}
        }
        true
    }
}

/// Walk every reachable path of `seq` and report what it uses.
pub fn scan(seq: &[u8]) -> Scan {
    let mut s = Scanner {
        seq,
        out: Scan::default(),
        seen: BTreeSet::new(),
        work: vec![(Level::Seq, 0, ChanState::default())],
    };
    s.run();
    s.out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scan_follows_a_sequence_into_a_channel_and_its_layer() {
        // seq: initchan, ldchan 0 -> 0x0008, end
        // chan @8: noshort, instr 3, ldlayer 0 -> 0x0010, end
        // layer @0x10: note, end
        let mut code = vec![0xd7, 0, 1, 0x90, 0, 8, 0xff, 0xff];
        code.extend_from_slice(&[0xc4, 0xc1, 3, 0x88, 0, 0x10, 0xff, 0x00]);
        code.extend_from_slice(&[0x27, 0x30, 100, 0, 0xff]);
        let s = scan(&code);
        assert!(s.channels.contains(&0));
        assert!(s.layers.contains(&0x10));
        assert!(s.instruments.contains(&(0, 3)));
    }

    #[test]
    fn both_outcomes_of_a_branch_are_scanned() {
        // seq: ldi 1 ; beqz -> 12 ; end ; @12: ldchan 2 -> 20 ; end
        let mut code = vec![0xcc, 1, 0xfa, 0, 12, 0xff, 0, 0, 0, 0, 0, 0];
        code.extend_from_slice(&[0x92, 0, 20, 0xff, 0, 0, 0, 0]);
        code.extend_from_slice(&[0xff]);
        let s = scan(&code);
        assert!(s.channels.contains(&2));
    }

    #[test]
    fn drum_layers_name_their_keys() {
        // channel with drum instrument 0x7F plays key 5 in a layer
        let mut code = vec![0xd7, 0, 1, 0x90, 0, 8, 0xff, 0xff];
        code.extend_from_slice(&[0xc4, 0xc1, 0x7f, 0x88, 0, 0x10, 0xff, 0x00]);
        code.extend_from_slice(&[0x05, 0x30, 100, 0, 0xff]);
        let s = scan(&code);
        assert!(s.drum_keys.contains(&(0, 5)), "{:?}", s.drum_keys);
    }

    #[test]
    fn io_and_self_modifying_ops_are_flagged() {
        let s = scan(&[0xc7, 0, 0, 0, 0xff]);
        assert!(s.self_modifying);
        let s = scan(&[0x73, 0xff]);
        assert!(s.io);
    }
}
