//! The three script levels: sequence, channel, layer.
//!
//! Each level runs when its delay counter runs out. The delay unit is a
//! script tick, not an update: a player's tempo decides how many updates
//! make one tick. Layers hold the notes; a note command sets the layer's
//! delay to the note's length and a gate that releases the note early.

use crate::format::{Bank, NONE16};
use crate::script::*;
use crate::tables::*;
use crate::types::*;
use crate::{Sequencer, MAX_TEMPO};

/// Pack a layer address.
pub(crate) const fn owner(p: usize, c: usize, l: usize) -> u16 {
    ((p as u16) << 6) | ((c as u16) << 2) | l as u16
}

/// Unpack a layer address.
pub(crate) const fn unpack(o: u16) -> (usize, usize, usize) {
    (
        (o >> 6) as usize,
        ((o >> 2) & 15) as usize,
        (o & 3) as usize,
    )
}

/// `a / b` as Q16 without a 64-bit intermediate (`b` non-zero).
pub(crate) fn ratio_q16(mut a: u32, mut b: u32) -> u32 {
    while b >= 1 << 16 {
        a >>= 1;
        b >>= 1;
    }
    let b = b.max(1);
    let q = a / b;
    let r = a % b;
    (q << 16).wrapping_add((r << 16) / b)
}

impl<'a, const P: usize, const N: usize> Sequencer<'a, P, N> {
    // ---- script ticks ----------------------------------------------------

    /// One update of player `p`: tempo gate, scripts, then property update.
    pub(crate) fn step_player(&mut self, p: usize) {
        let code = core::mem::take(&mut self.codes[p]);
        self.process_sequence(p, &mut *code);
        self.codes[p] = code;
        self.player_sound(p);
    }

    fn process_sequence(&mut self, p: usize, code: &mut [u8]) {
        if !self.players[p].enabled {
            return;
        }
        let pl = &mut self.players[p];
        if pl.muted && pl.mute_behavior & MUTE_STOP_SCRIPT != 0 {
            return;
        }
        pl.script_counter = pl.script_counter.wrapping_add(1);
        let acc = pl.tempo_acc as i32 + pl.tempo as i32 + pl.tempo_change as i32;
        if acc < MAX_TEMPO as i32 {
            pl.tempo_acc = acc.max(0) as u16;
            return;
        }
        pl.tempo_acc = (acc - MAX_TEMPO as i32) as u16;
        if pl.stop_script {
            return;
        }
        if pl.delay > 1 {
            pl.delay -= 1;
        } else {
            pl.recalc_volume = true;
            if !self.run_sequence_script(p, code) {
                return;
            }
        }
        for c in 0..CHANNELS {
            if self.players[p].channels[c].enabled {
                self.process_channel(p, c, code);
            }
        }
    }

    /// Returns false when the player stopped scripting for good.
    fn run_sequence_script(&mut self, p: usize, code: &mut [u8]) -> bool {
        loop {
            let mut s = self.players[p].script;
            let cmd = rd8(code, &mut s);
            if cmd >= FLOW_FIRST {
                let arg = flow_arg(code, &mut s, cmd);
                let d = flow(code, &mut s, cmd, arg);
                self.players[p].script = s;
                if d != 0 {
                    if d == SCRIPT_END {
                        self.players[p].finished = true;
                        self.disable_player(p);
                    } else {
                        self.players[p].delay = d as u16;
                    }
                    return d != SCRIPT_END;
                }
                continue;
            }
            if cmd >= 0xc0 {
                let keep_going = self.seq_op(p, code, &mut s, cmd);
                self.players[p].script = s;
                if !keep_going {
                    return false;
                }
                continue;
            }
            let low = (cmd & 15) as usize;
            match cmd & 0xf0 {
                0x00 => s.value = (!self.players[p].channels[low].enabled) as i8,
                0x40 => self.disable_channel(p, low),
                0x50 => s.value = s.value.wrapping_sub(self.players[p].io[low & 7] as i8),
                0x60 => {
                    rd8(code, &mut s);
                    rd8(code, &mut s);
                }
                0x70 => self.players[p].io[low & 7] = s.value as u8,
                0x80 => {
                    s.value = self.players[p].io[low & 7] as i8;
                    if low < 2 {
                        self.players[p].io[low] = IO_NONE;
                    }
                }
                0x90 => {
                    let t = rd16(code, &mut s);
                    self.enable_channel(p, low, t);
                }
                0xa0 => {
                    let rel = rd16(code, &mut s) as i16;
                    let t = (s.pc as i32 + rel as i32) as u16;
                    self.enable_channel(p, low, t);
                }
                0xb0 => {
                    rd8(code, &mut s);
                    rd16(code, &mut s);
                    self.unsupported += 1;
                }
                _ => {}
            }
            self.players[p].script = s;
        }
    }

    fn seq_op(&mut self, p: usize, code: &mut [u8], s: &mut Script, cmd: u8) -> bool {
        match cmd {
            0xf1 | 0xd0 => {
                let v = rd8(code, s);
                if cmd == 0xd0 {
                    self.players[p].note_alloc_policy = v;
                }
            }
            0xdf | 0xde => {
                if cmd == 0xdf {
                    self.players[p].transpose = 0;
                }
                let v = rd8(code, s) as i8;
                self.players[p].transpose = self.players[p].transpose.wrapping_add(v);
            }
            0xdd => {
                let t = (rd8(code, s) as u32 * 48).min(MAX_TEMPO as u32);
                self.players[p].tempo = if t == 0 { 1 } else { t as u16 };
            }
            0xdc => self.players[p].tempo_change = (rd8(code, s) as i8 as i16) * 48,
            0xda => {
                let mode = rd8(code, s);
                let t = rd16(code, s);
                let pl = &mut self.players[p];
                match mode {
                    0 | 1 if pl.state != 2 => {
                        pl.fade_timer_default = t;
                        pl.state = mode;
                    }
                    2 => {
                        pl.fade_timer = t;
                        pl.state = 2;
                        pl.fade_vel_q24 = if t > 0 { -pl.fade_q24 / t as i32 } else { 0 };
                    }
                    _ => {}
                }
            }
            0xdb => {
                let v = rd8(code, s) as i32;
                let pl = &mut self.players[p];
                if pl.state == 1 {
                    pl.state = 0;
                    pl.fade_q24 = 0;
                }
                if pl.state == 0 {
                    pl.fade_timer = pl.fade_timer_default;
                    let target = ((v << 24) / 127).min(1 << 24);
                    if pl.fade_timer_default != 0 {
                        pl.fade_vel_q24 = (target - pl.fade_q24) / pl.fade_timer as i32;
                    } else {
                        pl.fade_q24 = target;
                    }
                }
            }
            0xd9 => {
                let v = rd8(code, s) as i8 as i32;
                self.players[p].fade_scale_q15 = ((v.max(0) << 15) / 127) as u16;
            }
            0xd7 => {
                let mask = rd16(code, s);
                let font = self.players[p].default_font;
                let mb = self.players[p].mute_behavior;
                let na = self.players[p].note_alloc_policy;
                for c in 0..CHANNELS {
                    if mask & (1 << c) != 0 {
                        let ch = &mut self.players[p].channels[c];
                        ch.font = font;
                        ch.mute_behavior = mb;
                        ch.note_alloc_policy = na;
                    }
                }
            }
            0xd6 => {
                rd16(code, s);
            }
            0xd5 => {
                let v = rd8(code, s) as i8 as i32;
                self.players[p].mute_scale_q15 = ((v.max(0) << 15) / 127) as u16;
            }
            0xd4 => self.players[p].muted = true,
            0xd3 => self.players[p].mute_behavior = rd8(code, s),
            0xd1 => self.players[p].short_gate = Some(rd16(code, s)),
            0xd2 => self.players[p].short_vel = Some(rd16(code, s)),
            0xce => {
                let n = rd8(code, s) as u32;
                let r = self.next_random() >> 2;
                s.value = if n == 0 {
                    (r & 0xff) as i8
                } else {
                    (r % n) as i8
                };
            }
            0xcd => {
                let t = rd16(code, s);
                if s.value != -1 && s.depth != 3 {
                    let at = t as usize + ((s.value as u8 as usize) << 1);
                    let hi = code.get(at).copied().unwrap_or(0xff) as u16;
                    let lo = code.get(at + 1).copied().unwrap_or(0xff) as u16;
                    s.stack[s.depth as usize] = s.pc;
                    s.depth += 1;
                    s.pc = (hi << 8) | lo;
                }
            }
            0xcc => s.value = rd8(code, s) as i8,
            0xc9 => s.value &= rd8(code, s) as i8,
            0xc8 => s.value = s.value.wrapping_sub(rd8(code, s) as i8),
            0xc7 => {
                let add = rd8(code, s);
                let at = rd16(code, s) as usize;
                if let Some(b) = code.get_mut(at) {
                    *b = (s.value as u8).wrapping_add(add);
                }
            }
            0xc6 => {
                self.players[p].stop_script = true;
                return false;
            }
            0xc5 => self.players[p].script_counter = rd16(code, s),
            0xc4 => {
                rd8(code, s);
                rd8(code, s);
                self.unsupported += 1;
            }
            0xef => {
                rd16(code, s);
                rd8(code, s);
            }
            _ => {}
        }
        true
    }

    // ---- channels --------------------------------------------------------

    pub(crate) fn enable_channel(&mut self, p: usize, c: usize, pc: u16) {
        for l in 0..LAYERS {
            self.free_layer(p, c, l);
        }
        let ch = &mut self.players[p].channels[c];
        ch.enabled = true;
        ch.finished = false;
        ch.script.depth = 0;
        ch.script.pc = pc;
        ch.delay = 0;
    }

    pub(crate) fn disable_channel(&mut self, p: usize, c: usize) {
        for l in 0..LAYERS {
            self.free_layer(p, c, l);
        }
        let ch = &mut self.players[p].channels[c];
        ch.enabled = false;
        ch.finished = true;
    }

    fn process_channel(&mut self, p: usize, c: usize, code: &mut [u8]) {
        let skip_script = self.players[p].channels[c].stop_script
            || (self.players[p].muted
                && self.players[p].channels[c].mute_behavior & MUTE_STOP_SCRIPT != 0);
        if !skip_script {
            if self.players[p].channels[c].delay >= 2 {
                self.players[p].channels[c].delay -= 1;
            } else {
                self.run_channel_script(p, c, code);
            }
        }
        for l in 0..LAYERS {
            if self.players[p].channels[c].layers[l].alloc {
                self.process_layer(p, c, l, code);
            }
        }
    }

    fn font_slot_to_index(&self, p: usize, slot: u8) -> Option<u8> {
        let pl = &self.players[p];
        let n = pl.n_fonts as usize;
        let slot = slot as usize;
        if slot >= n {
            return None;
        }
        self.font_index(pl.fonts[n - 1 - slot])
    }

    fn set_instrument(&mut self, p: usize, c: usize, id: u8) {
        let bank = self.bank;
        let font = self.players[p].channels[c].font;
        let ch = &mut self.players[p].channels[c];
        if id >= 0x80 {
            ch.inst_or_wave = id;
            ch.inst = InstSel::NONE;
        } else if id == 0x7f {
            ch.inst_or_wave = 0;
            ch.inst = InstSel::NONE;
        } else if id == 0x7e {
            ch.inst_or_wave = 1;
            ch.inst = InstSel::NONE;
        } else {
            match bank.and_then(|b| inst_lookup(&b, font, id)) {
                Some(i) => {
                    ch.env = if i.env == NONE16 {
                        EnvRef::Default
                    } else {
                        EnvRef::Bank(i.env)
                    };
                    ch.decay_idx = i.decay;
                    ch.inst = InstSel { font, id };
                    ch.inst_or_wave = id + 2;
                }
                None => {
                    ch.inst = InstSel::NONE;
                    ch.has_instrument = false;
                    return;
                }
            }
        }
        ch.has_instrument = true;
    }

    fn run_channel_script(&mut self, p: usize, c: usize, code: &mut [u8]) {
        loop {
            let mut s = self.players[p].channels[c].script;
            let cmd = rd8(code, &mut s);
            let mut a = [0u32; 3];
            if cmd >= 0xb0 {
                let t = ARGS[(cmd - 0xb0) as usize];
                let n = (t & 3) as usize;
                for (i, slot) in a.iter_mut().enumerate().take(n) {
                    let wide = t & (0x80 >> i) != 0;
                    *slot = if wide {
                        rd16(code, &mut s) as u32
                    } else {
                        rd8(code, &mut s) as u32
                    };
                }
                if cmd >= FLOW_FIRST {
                    let d = flow(code, &mut s, cmd, a[0] as u16);
                    self.players[p].channels[c].script = s;
                    if d != 0 {
                        if d == SCRIPT_END {
                            self.disable_channel(p, c);
                        } else {
                            self.players[p].channels[c].delay = d as u16;
                        }
                        return;
                    }
                    continue;
                }
                let stop = self.channel_op(p, c, code, &mut s, cmd, a);
                self.players[p].channels[c].script = s;
                if stop {
                    return;
                }
                continue;
            }
            if cmd >= 0x70 {
                let mut low = (cmd & 7) as usize;
                if cmd & 0xf8 != 0x70 && low >= 4 {
                    low = 0;
                }
                match cmd & 0xf8 {
                    0x80 => {
                        let l = &self.players[p].channels[c].layers[low];
                        s.value = if l.alloc { l.finished as i8 } else { -1 };
                    }
                    0x88 => {
                        let t = rd16(code, &mut s);
                        self.set_layer(p, c, low, t);
                    }
                    0x90 => self.free_layer(p, c, low),
                    0x70 => self.players[p].channels[c].io[cmd as usize & 7] = s.value as u8,
                    0x78 => {
                        let rel = rd16(code, &mut s) as i16;
                        let t = (s.pc as i32 + rel as i32) as u16;
                        self.set_layer(p, c, low, t);
                    }
                    _ => {}
                }
                self.players[p].channels[c].script = s;
                continue;
            }
            let low = (cmd & 15) as usize;
            match cmd & 0xf0 {
                0x00 => {
                    self.players[p].channels[c].delay = low as u16;
                    self.players[p].channels[c].script = s;
                    return;
                }
                0x10 => self.unsupported += 1,
                0x20 => {
                    let t = rd16(code, &mut s);
                    self.enable_channel(p, low, t);
                }
                0x30 => {
                    let port = rd8(code, &mut s) as usize & 7;
                    self.players[p].channels[low].io[port] = s.value as u8;
                }
                0x40 => {
                    let port = rd8(code, &mut s) as usize & 7;
                    s.value = self.players[p].channels[low].io[port] as i8;
                }
                0x50 => {
                    s.value = s
                        .value
                        .wrapping_sub(self.players[p].channels[c].io[low & 7] as i8);
                }
                0x60 => {
                    s.value = self.players[p].channels[c].io[low & 7] as i8;
                    if low < 2 {
                        self.players[p].channels[c].io[low] = IO_NONE;
                    }
                }
                _ => {}
            }
            self.players[p].channels[c].script = s;
        }
    }

    /// One channel opcode in `0xB0..0xF2`. Returns true if the script must stop
    /// for this tick.
    fn channel_op(
        &mut self,
        p: usize,
        c: usize,
        code: &mut [u8],
        s: &mut Script,
        cmd: u8,
        a: [u32; 3],
    ) -> bool {
        let u8_0 = a[0] as u8;
        match cmd {
            0xea => {
                self.players[p].channels[c].stop_script = true;
                return true;
            }
            0xeb => {
                self.channel_font(p, c, u8_0);
                self.set_instrument(p, c, a[1] as u8);
            }
            0xc6 => self.channel_font(p, c, u8_0),
            0xc1 => self.set_instrument(p, c, u8_0),
            0xc3 => self.players[p].channels[c].large_notes = false,
            0xc4 => self.players[p].channels[c].large_notes = true,
            0xdf => {
                let ch = &mut self.players[p].channels[c];
                ch.volume_q15 = vol_q15(u8_0);
                ch.changes.volume = true;
            }
            0xe0 => {
                let ch = &mut self.players[p].channels[c];
                ch.volume_scale_q15 = (u8_0 as u16) << 8;
                ch.changes.volume = true;
            }
            0xde => {
                let ch = &mut self.players[p].channels[c];
                ch.freq_q16 = (a[0] & 0xffff) << 1;
                ch.changes.freq = true;
            }
            0xd3 | 0xee => {
                let idx = u8_0.wrapping_add(0x80) as usize;
                let ch = &mut self.players[p].channels[c];
                ch.freq_q16 = if cmd == 0xd3 {
                    BEND_OCTAVE_Q16[idx]
                } else {
                    BEND_TWO_SEMITONES_Q16[idx]
                };
                ch.changes.freq = true;
            }
            0xdd => {
                let ch = &mut self.players[p].channels[c];
                ch.new_pan = u8_0;
                ch.changes.pan = true;
            }
            0xdc => {
                let ch = &mut self.players[p].channels[c];
                ch.pan_weight = u8_0;
                ch.changes.pan = true;
            }
            0xdb => self.players[p].channels[c].transpose = u8_0 as i8,
            0xda => self.players[p].channels[c].env = EnvRef::Song(a[0] as u16),
            0xd9 => self.players[p].channels[c].decay_idx = u8_0,
            0xd8 => {
                let ch = &mut self.players[p].channels[c];
                ch.vib_depth_target = u8_0 as u16 * 8;
                ch.vib_depth_start = 0;
                ch.vib_depth_delay = 0;
            }
            0xd7 => {
                let ch = &mut self.players[p].channels[c];
                ch.vib_rate_delay = 0;
                ch.vib_rate_target = u8_0 as u16 * 32;
                ch.vib_rate_start = u8_0 as u16 * 32;
            }
            0xe2 => {
                let ch = &mut self.players[p].channels[c];
                ch.vib_depth_start = u8_0 as u16 * 8;
                ch.vib_depth_target = a[1] as u16 * 8;
                ch.vib_depth_delay = a[2] as u16 * 16;
            }
            0xe1 => {
                let ch = &mut self.players[p].channels[c];
                ch.vib_rate_start = u8_0 as u16 * 32;
                ch.vib_rate_target = a[1] as u16 * 32;
                ch.vib_rate_delay = a[2] as u16 * 16;
            }
            0xe3 => self.players[p].channels[c].vib_delay = u8_0 as u16 * 16,
            0xd4 => self.players[p].channels[c].reverb = u8_0,
            0xd2 => self.players[p].channels[c].sustain = u8_0,
            0xd1 => self.players[p].channels[c].note_alloc_policy = u8_0,
            0xd0 => {} // headset and stereo bits have no effect on the SPU's output
            0xca => {
                let ch = &mut self.players[p].channels[c];
                ch.mute_behavior = u8_0;
                ch.changes.volume = true;
            }
            0xc8 => s.value = s.value.wrapping_sub(u8_0 as i8),
            0xc9 => s.value &= u8_0 as i8,
            0xcc => s.value = u8_0 as i8,
            0xcd => self.disable_channel(p, u8_0 as usize & 15),
            0xc7 => {
                if let Some(b) = code.get_mut(a[1] as usize) {
                    *b = (s.value as u8).wrapping_add(u8_0);
                }
            }
            0xcb => {
                let at = a[0] as usize + (s.value as u8 as usize);
                s.value = code.get(at).copied().unwrap_or(0) as i8;
            }
            0xce => self.players[p].channels[c].ptr = a[0] as u16,
            // dyntable: where the table of `dyncall` lives.
            0xc2 => self.players[p].channels[c].dyn_table = a[0] as u16,
            // dyncall: call the script whose offset is entry `value` of that table. A game with many
            // short sounds keeps one script per sound and picks it by number.
            0xe4 => {
                let at =
                    self.players[p].channels[c].dyn_table as usize + 2 * (s.value as u8 as usize);
                let hi = code.get(at).copied().unwrap_or(OP_END);
                let lo = code.get(at + 1).copied().unwrap_or(OP_END);
                if s.depth < 4 {
                    s.stack[s.depth as usize] = s.pc;
                    s.depth += 1;
                }
                s.pc = u16::from_be_bytes([hi, lo]);
            }
            0xcf => {
                let at = a[0] as usize;
                let v = self.players[p].channels[c].ptr;
                if at + 1 < code.len() {
                    code[at] = (v >> 8) as u8;
                    code[at + 1] = v as u8;
                }
            }
            0xe7 => {
                let at = a[0] as usize;
                if at + 8 <= code.len() {
                    let d = [
                        code[at],
                        code[at + 1],
                        code[at + 2],
                        code[at + 3],
                        code[at + 4],
                        code[at + 5],
                        code[at + 6],
                        code[at + 7],
                    ];
                    let ch = &mut self.players[p].channels[c];
                    ch.mute_behavior = d[0];
                    ch.note_alloc_policy = d[1];
                    set_priorities(ch, d[2]);
                    ch.transpose = d[3] as i8;
                    ch.new_pan = d[4];
                    ch.pan_weight = d[5];
                    ch.reverb = d[6];
                    ch.changes.pan = true;
                }
            }
            0xe8 => {
                let mb = a[0] as u8;
                let na = a[1] as u8;
                let pri = a[2] as u8;
                let tr = rd8(code, s) as i8;
                let pan = rd8(code, s);
                let weight = rd8(code, s);
                let rev = rd8(code, s);
                let _idx = rd8(code, s);
                let ch = &mut self.players[p].channels[c];
                ch.mute_behavior = mb;
                ch.note_alloc_policy = na;
                set_priorities(ch, pri);
                ch.transpose = tr;
                ch.new_pan = pan;
                ch.pan_weight = weight;
                ch.reverb = rev;
                ch.changes.pan = true;
            }
            0xe9 => set_priorities(&mut self.players[p].channels[c], u8_0),
            0xec => {
                let ch = &mut self.players[p].channels[c];
                ch.vib_depth_target = 0;
                ch.vib_depth_start = 0;
                ch.vib_depth_delay = 0;
                ch.vib_rate_target = 0;
                ch.vib_rate_start = 0;
                ch.vib_rate_delay = 0;
                ch.sustain = 0;
                ch.vel_variance = 0;
                ch.gate_variance = 0;
                ch.freq_q16 = 1 << 16;
            }
            0xb9 => self.players[p].channels[c].vel_variance = u8_0,
            0xba => self.players[p].channels[c].gate_variance = u8_0,
            0xb8 => {
                let r = self.next_random();
                s.value = if a[0] == 0 {
                    (r & 0xff) as i8
                } else {
                    (r % a[0]) as i8
                };
            }
            0xb0 | 0xb1 | 0xb3 | 0xbb | 0xed => self.unsupported += 1,
            _ => {}
        }
        false
    }

    fn channel_font(&mut self, p: usize, c: usize, slot: u8) {
        if let Some(i) = self.font_slot_to_index(p, slot) {
            self.players[p].channels[c].font = i;
        }
    }

    // ---- layers ----------------------------------------------------------

    pub(crate) fn set_layer(&mut self, p: usize, c: usize, l: usize, pc: u16) {
        if self.players[p].channels[c].layers[l].alloc {
            self.note_decay(owner(p, c, l));
        }
        let (env, decay) = {
            let ch = &self.players[p].channels[c];
            (ch.env, ch.decay_idx)
        };
        let _ = decay;
        let ly = &mut self.players[p].channels[c].layers[l];
        *ly = Layer::INIT;
        ly.alloc = true;
        ly.enabled = true;
        ly.env = env;
        ly.decay_idx = 0;
        ly.script.pc = pc;
    }

    pub(crate) fn free_layer(&mut self, p: usize, c: usize, l: usize) {
        if self.players[p].channels[c].layers[l].alloc {
            self.layer_disable(owner(p, c, l));
            self.players[p].channels[c].layers[l].alloc = false;
        }
    }

    pub(crate) fn layer_disable(&mut self, o: u16) {
        let (p, c, l) = unpack(o);
        if self.players[p].finished {
            self.note_release(o);
        } else {
            self.note_decay(o);
        }
        let ly = &mut self.players[p].channels[c].layers[l];
        ly.enabled = false;
        ly.finished = true;
    }

    fn process_layer(&mut self, p: usize, c: usize, l: usize, code: &mut [u8]) {
        let o = owner(p, c, l);
        {
            let ly = &mut self.players[p].channels[c].layers[l];
            if !ly.enabled {
                return;
            }
            if ly.delay > 1 {
                ly.delay -= 1;
                if !ly.muted && ly.delay as i16 <= ly.gate_delay {
                    self.note_decay(o);
                    self.players[p].channels[c].layers[l].muted = true;
                }
                return;
            }
        }
        // step 1: end the previous note
        {
            let ly = self.players[p].channels[c].layers[l];
            if !ly.continuous || (ly.note != NONE8 && self.notes[ly.note as usize].wanted == o) {
                self.note_decay(o);
            }
            let ly = &mut self.players[p].channels[c].layers[l];
            if ly.porta.mode & 0x7f == 1 || ly.porta.mode & 0x7f == 2 {
                ly.porta.mode = 0;
            }
            ly.props_need_init = true;
            self.players[p].channels[c].dirty = true;
        }
        let Some(cmd) = self.layer_commands(p, c, l, code) else {
            return;
        };
        let Some(cmd) = self.layer_note_args(p, c, l, code, cmd) else {
            self.layer_tail(o, p, c, l);
            return;
        };
        let Some(same) = self.layer_voice(p, c, l, cmd) else {
            self.layer_tail(o, p, c, l);
            return;
        };
        self.layer_alloc(p, c, l, same);
        self.layer_tail(o, p, c, l);
    }

    fn layer_tail(&mut self, o: u16, p: usize, c: usize, l: usize) {
        let ly = self.players[p].channels[c].layers[l];
        if ly.muted && (ly.note != NONE8 || ly.continuous) {
            self.note_decay(o);
        }
    }

    /// Run layer opcodes up to the next note or delay; `None` if the layer
    /// ended.
    fn layer_commands(&mut self, p: usize, c: usize, l: usize, code: &mut [u8]) -> Option<u8> {
        let o = owner(p, c, l);
        loop {
            let mut s = self.players[p].channels[c].layers[l].script;
            let cmd = rd8(code, &mut s);
            if cmd <= 0xc0 {
                self.players[p].channels[c].layers[l].script = s;
                return Some(cmd);
            }
            if cmd >= FLOW_FIRST {
                let arg = flow_arg(code, &mut s, cmd);
                let d = flow(code, &mut s, cmd, arg);
                self.players[p].channels[c].layers[l].script = s;
                if d != 0 {
                    if d == SCRIPT_END {
                        self.layer_disable(o);
                        return None;
                    }
                    // a layer has no delay opcodes of its own: treat as end
                    self.layer_disable(o);
                    return None;
                }
                continue;
            }
            let track = self.players[p].transpose;
            let ctrans = self.players[p].channels[c].transpose;
            let short_vel = self.players[p].short_vel;
            let short_gate = self.players[p].short_gate;
            let bank = self.bank;
            let font = self.players[p].channels[c].font;
            let mut legato = false;
            let ly = &mut self.players[p].channels[c].layers[l];
            match cmd {
                0xc1 => ly.vel_sq_q15 = vel_sq_q15(rd8(code, &mut s)),
                0xca => ly.pan = rd8(code, &mut s),
                0xc9 => ly.gate_time = rd8(code, &mut s),
                0xc2 => ly.transpose = rd8(code, &mut s) as i8,
                0xc4 | 0xc5 => {
                    ly.continuous = cmd == 0xc4;
                    ly.bit1 = false;
                    legato = true;
                }
                0xc3 => ly.short_delay = rd_delay(code, &mut s),
                0xc6 => {
                    let id = rd8(code, &mut s);
                    if id >= 0x7e {
                        ly.inst_or_wave = if id == 0x7e {
                            1
                        } else if id == 0x7f {
                            0
                        } else {
                            id
                        };
                        if id > 0x7f {
                            ly.inst = InstSel::NONE;
                        }
                        if id == 0xff {
                            ly.decay_idx = 0;
                        }
                    } else {
                        match bank.and_then(|b| inst_lookup(&b, font, id)) {
                            Some(i) => {
                                ly.env = if i.env == NONE16 {
                                    EnvRef::Default
                                } else {
                                    EnvRef::Bank(i.env)
                                };
                                ly.decay_idx = i.decay;
                                ly.inst = InstSel { font, id };
                                ly.inst_or_wave = id + 2;
                            }
                            None => {
                                ly.inst = InstSel::NONE;
                                ly.inst_or_wave = 0xff;
                            }
                        }
                    }
                }
                0xc7 => {
                    ly.porta.mode = rd8(code, &mut s);
                    let t = rd8(code, &mut s)
                        .wrapping_add(ctrans as u8)
                        .wrapping_add(ly.transpose as u8)
                        .wrapping_add(track as u8);
                    ly.porta_target = if t >= 0x80 { 0 } else { t };
                    ly.porta_time = if ly.porta.mode & 0x80 != 0 {
                        rd8(code, &mut s) as u16
                    } else {
                        rd_delay(code, &mut s)
                    };
                }
                0xc8 => ly.porta.mode = 0,
                0xcb | 0xcf => {
                    if cmd == 0xcb {
                        ly.env = EnvRef::Song(rd16(code, &mut s));
                    }
                    ly.decay_idx = rd8(code, &mut s);
                }
                0xcc => ly.ignore_drum_pan = true,
                0xcd => {
                    rd8(code, &mut s);
                }
                0xce => {
                    let v = rd8(code, &mut s);
                    ly.bend_q16 = BEND_TWO_SEMITONES_Q16[v.wrapping_add(0x80) as usize];
                }
                0xd0..=0xdf => {
                    let t = match short_vel {
                        Some(off) => code
                            .get(off as usize + (cmd & 15) as usize)
                            .copied()
                            .unwrap_or(0),
                        None => DEFAULT_SHORT_VELOCITY[(cmd & 15) as usize],
                    };
                    ly.vel_sq_q15 = vel_sq_q15(t);
                }
                0xe0..=0xef => {
                    ly.gate_time = match short_gate {
                        Some(off) => code
                            .get(off as usize + (cmd & 15) as usize)
                            .copied()
                            .unwrap_or(0),
                        None => DEFAULT_SHORT_GATE[(cmd & 15) as usize],
                    };
                }
                _ => {}
            }
            self.players[p].channels[c].layers[l].script = s;
            if legato {
                self.note_decay(o);
            }
        }
    }

    /// Step 3: read a note's operands. `None` ends the tick with no note.
    fn layer_note_args(
        &mut self,
        p: usize,
        c: usize,
        l: usize,
        code: &mut [u8],
        cmd: u8,
    ) -> Option<u8> {
        let mut s = self.players[p].channels[c].layers[l].script;
        if cmd == 0xc0 {
            let d = rd_delay(code, &mut s);
            let ly = &mut self.players[p].channels[c].layers[l];
            ly.delay = d;
            ly.muted = true;
            ly.bit1 = false;
            ly.script = s;
            return None;
        }
        let large = self.players[p].channels[c].large_notes;
        let rnd = self.rng;
        let (vel_var, gate_var) = {
            let ch = &self.players[p].channels[c];
            (ch.vel_variance, ch.gate_variance)
        };
        let ly = &mut self.players[p].channels[c].layers[l];
        ly.muted = false;
        let kind = cmd & 0xc0;
        let delay;
        if large {
            let vel;
            match kind {
                0x00 => {
                    delay = rd_delay(code, &mut s);
                    vel = rd8(code, &mut s);
                    ly.gate_time = rd8(code, &mut s);
                    ly.last_delay = delay;
                }
                0x40 => {
                    delay = rd_delay(code, &mut s);
                    vel = rd8(code, &mut s);
                    ly.gate_time = 0;
                    ly.last_delay = delay;
                }
                _ => {
                    delay = ly.last_delay;
                    vel = rd8(code, &mut s);
                    ly.gate_time = rd8(code, &mut s);
                }
            }
            ly.vel_sq_q15 = vel_sq_q15(vel.min(127));
        } else {
            match kind {
                0x00 => {
                    delay = rd_delay(code, &mut s);
                    ly.last_delay = delay;
                }
                0x40 => delay = ly.short_delay,
                _ => delay = ly.last_delay,
            }
        }
        let note = cmd & 0x3f;
        ly.vel_sq2_q15 = if vel_var != 0 {
            let d = (ly.vel_sq_q15 as u32 * (rnd % vel_var as u32) / 100) as i32;
            let d = if rnd & 0x8000 != 0 { -d } else { d };
            (ly.vel_sq_q15 as i32 + d).clamp(0, 32768) as u16
        } else {
            ly.vel_sq_q15
        };
        ly.delay = delay;
        ly.gate_delay = ((ly.gate_time as u32 * delay as u32) >> 8) as i16;
        if gate_var != 0 {
            let d = (ly.gate_delay as u32 * (rnd % vel_var.max(1) as u32) / 100) as i16;
            let d = if rnd & 0x4000 != 0 { -d } else { d };
            ly.gate_delay = (ly.gate_delay + d).clamp(0, ly.delay as i16);
        }
        ly.script = s;
        let pl = &self.players[p];
        let muted = (pl.muted
            && pl.channels[c].mute_behavior & (MUTE_STOP_NOTES | MUTE_SKIP_LAYER) != 0)
            || pl.channels[c].muted;
        if muted {
            self.players[p].channels[c].layers[l].muted = true;
            return None;
        }
        Some(note)
    }

    /// Step 4: pick the sample and frequency for the note. `Some(same)` where
    /// `same` says the sample is unchanged; `None` ends the tick.
    fn layer_voice(&mut self, p: usize, c: usize, l: usize, key: u8) -> Option<bool> {
        let bank = self.bank?;
        let (track, tempo) = (self.players[p].transpose, self.players[p].tempo);
        let (has_instrument, ch_inst_or_wave, ch_font, ch_transpose, ch_inst) = {
            let ch = &self.players[p].channels[c];
            (
                ch.has_instrument,
                ch.inst_or_wave,
                ch.font,
                ch.transpose,
                ch.inst,
            )
        };
        let mut ly = self.players[p].channels[c].layers[l];
        let mut inst_or_wave = ly.inst_or_wave;
        let mut same = true;
        if inst_or_wave == 0xff {
            if !has_instrument {
                return None;
            }
            inst_or_wave = ch_inst_or_wave;
        }
        let mut semitone = key;
        match inst_or_wave {
            0 => {
                semitone = semitone
                    .wrapping_add(ch_transpose as u8)
                    .wrapping_add(ly.transpose as u8);
                let Some(d) = bank.drum(ch_font, semitone) else {
                    ly.muted = true;
                    self.players[p].channels[c].layers[l] = ly;
                    return None;
                };
                ly.env = if d.env == NONE16 {
                    EnvRef::Default
                } else {
                    EnvRef::Bank(d.env)
                };
                ly.decay_idx = d.decay;
                if !ly.ignore_drum_pan {
                    ly.pan = d.pan;
                }
                ly.tuned = TunedRef {
                    sample: d.tuned.sample,
                    tuning_q16: d.tuned.tuning_q16,
                };
                ly.freq_q16 = d.tuned.tuning_q16;
            }
            1 => {
                ly.muted = true;
                self.unsupported += 1;
                self.players[p].channels[c].layers[l] = ly;
                return None;
            }
            _ => {
                semitone = semitone
                    .wrapping_add(track as u8)
                    .wrapping_add(ch_transpose as u8)
                    .wrapping_add(ly.transpose as u8);
                if semitone >= 0x80 {
                    ly.muted = true;
                    self.players[p].channels[c].layers[l] = ly;
                    return None;
                }
                let inst = if ly.inst_or_wave == 0xff {
                    ch_inst
                } else {
                    ly.inst
                };
                let Some(ins) = (if inst.is_none() {
                    None
                } else {
                    inst_lookup(&bank, inst.font, inst.id)
                }) else {
                    // synthetic wave or missing instrument: nothing to play
                    ly.tuned = TunedRef::NONE;
                    ly.muted = true;
                    self.players[p].channels[c].layers[l] = ly;
                    return None;
                };
                if ly.porta.mode != 0 {
                    let top = semitone.max(ly.porta_target);
                    let t = ins.for_key(top);
                    same = ly.tuned.sample == t.sample && ly.tuned.tuning_q16 == t.tuning_q16;
                    ly.tuned = TunedRef {
                        sample: t.sample,
                        tuning_q16: t.tuning_q16,
                    };
                    let f_here = mul_q16(PITCH_Q16[semitone as usize], t.tuning_q16);
                    let f_target = mul_q16(PITCH_Q16[ly.porta_target as usize], t.tuning_q16);
                    let (f1, f2) = match ly.porta.mode & 0x7f {
                        1 | 3 | 5 => (f_target, f_here),
                        2 | 4 => (f_here, f_target),
                        _ => (f_here, f_here),
                    };
                    let q = ratio_q16(f2, f1.max(1));
                    ly.porta.extent_q16 = q as i32 - (1 << 16);
                    let mut speed: i32 = if ly.porta.mode & 0x80 != 0 {
                        let mut sp = (tempo as i32 * 0x8000) / MAX_TEMPO as i32;
                        if ly.delay != 0 {
                            sp = sp * 0x100 / (ly.delay as i32 * ly.porta_time.max(1) as i32);
                        }
                        sp
                    } else {
                        0x20000 / (ly.porta_time.max(1) as i32 * 3)
                    };
                    speed = speed.clamp(1, 0x7fff);
                    ly.porta.speed = speed as u16;
                    ly.porta.cur = 0;
                    ly.freq_q16 = f1;
                    if ly.porta.mode & 0x7f == 5 {
                        ly.porta_target = semitone;
                    }
                } else {
                    let t = ins.for_key(semitone);
                    same = ly.tuned.sample == t.sample && ly.tuned.tuning_q16 == t.tuning_q16;
                    ly.tuned = TunedRef {
                        sample: t.sample,
                        tuning_q16: t.tuning_q16,
                    };
                    ly.freq_q16 = mul_q16(PITCH_Q16[semitone as usize], t.tuning_q16);
                }
            }
        }
        ly.freq_q16 = mul_q16(ly.freq_q16, ly.bend_q16);
        if ly.delay == 0 {
            // a note with no written length lasts as long as its sample
            let end = bank.sample(ly.tuned.sample).map_or(0, |s| s.end_samples);
            let x = end.min(60_000) * tempo.min(MAX_TEMPO) as u32;
            // seconds * ticks per second: end * tempo / (freq * 32000 * 60)
            let d = if ly.freq_q16 < 1 << 21 {
                (ly.freq_q16 * 1875) >> 6
            } else {
                (ly.freq_q16 >> 6) * 1875
            };
            let ticks = (x / d.max(1)).min(0x7ffe);
            ly.gate_delay = 0;
            ly.delay = ticks as u16 + 1;
        }
        self.players[p].channels[c].layers[l] = ly;
        Some(same)
    }
}

/// Instrument `id` of the font index `font`.
pub(crate) fn inst_lookup(bank: &Bank<'_>, font: u8, id: u8) -> Option<crate::format::Instrument> {
    if font == NONE8 {
        return None;
    }
    bank.instrument(font, id)
}

fn set_priorities(ch: &mut Channel, v: u8) {
    if v & 15 != 0 {
        ch.note_priority = v & 15;
    }
    if v >> 4 != 0 {
        ch.low_priority = v >> 4;
    }
}

/// `vel / 127` as Q15.
pub(crate) fn vol_q15(v: u8) -> u16 {
    (((v.min(127) as u32) << 15) / 127) as u16
}

/// `(vel / 127)^2` as Q15.
pub(crate) fn vel_sq_q15(v: u8) -> u16 {
    let v = v.min(127) as u32;
    ((v * v) << 15).wrapping_div(127 * 127) as u16
}
