//! Notes: allocation, the software envelope, vibrato, portamento, and the
//! per-update pass that turns all of it into voice register writes.
//!
//! A note is one SPU voice's current job. A layer starts a note, owns it while
//! the note is held, then lets go: the note decays (a natural release at the
//! instrument's rate) or is released fast (3 updates) when it is cut. A voice
//! is only reused once its note's envelope has reached zero, which is why a
//! stolen note keeps ringing for a few updates before the new one starts.

use crate::engine::{owner, unpack};
use crate::format::NONE16;
use crate::tables::*;
use crate::types::*;
use crate::Sequencer;
use psx_sfx::hw::Hw;
use psx_sfx::Sample;
use psx_spu::SpuAddr;

/// A fast release: 1.0 over 3 updates, in Q24.
pub(crate) const FAST_RELEASE_Q24: i32 = (1 << 24) / 3;
/// 1.0 in Q24.
const ONE_Q24: i32 = 1 << 24;
/// Hardware voice level at gain 1.0.
const SPU_FULL: u32 = 0x3FFF;

/// `(arg / 32767)^2` as Q24.
fn level_sq_q24(arg: i16) -> i32 {
    let a = arg.max(0) as u32;
    let x = (a << 15) / 32767;
    ((x * x) >> 6) as i32
}

impl<'a, const P: usize, const N: usize> Sequencer<'a, P, N> {
    // ---- bookkeeping -----------------------------------------------------

    pub(crate) fn next_random(&mut self) -> u32 {
        self.rng = self.rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.rng >> 8
    }

    fn move_front(&mut self, n: usize) {
        self.pos_min -= 1;
        self.notes[n].pos = self.pos_min;
    }

    fn move_back(&mut self, n: usize) {
        self.pos_max += 1;
        self.notes[n].pos = self.pos_max;
    }

    /// The note on `list` that is last in line (`back`) or first.
    fn edge(&self, list: NoteList, back: bool) -> Option<usize> {
        let mut best: Option<usize> = None;
        for (i, n) in self.notes.iter().enumerate() {
            if n.list != list {
                continue;
            }
            best = match best {
                None => Some(i),
                Some(b)
                    if (back && n.pos > self.notes[b].pos)
                        || (!back && n.pos < self.notes[b].pos) =>
                {
                    Some(i)
                }
                keep => keep,
            };
        }
        best
    }

    /// The lowest-priority note on `list`, later ones winning ties, if its
    /// priority is below `limit`.
    fn lowest_below(&self, list: NoteList, limit: u8) -> Option<usize> {
        let mut idx: [usize; N] = [0; N];
        let mut cnt = 0;
        for (i, n) in self.notes.iter().enumerate() {
            if n.list == list {
                idx[cnt] = i;
                cnt += 1;
            }
        }
        // order by position, front first
        for a in 1..cnt {
            let mut b = a;
            while b > 0 && self.notes[idx[b - 1]].pos > self.notes[idx[b]].pos {
                idx.swap(b - 1, b);
                b -= 1;
            }
        }
        let mut best: Option<usize> = None;
        for &i in &idx[..cnt] {
            best = match best {
                Some(b) if self.notes[b].priority < self.notes[i].priority => Some(b),
                _ => Some(i),
            };
        }
        best.filter(|&b| self.notes[b].priority < limit)
    }

    // ---- layers let go of notes -----------------------------------------

    /// The layer lets its note decay at the instrument's rate.
    pub(crate) fn note_decay(&mut self, o: u16) {
        self.decay_or_release(o, false);
    }

    /// The layer cuts its note off with a fast release.
    pub(crate) fn note_release(&mut self, o: u16) {
        self.decay_or_release(o, true);
    }

    fn decay_or_release(&mut self, o: u16, release: bool) {
        let (p, c, l) = unpack(o);
        self.players[p].channels[c].layers[l].bit3 = false;
        let n = self.players[p].channels[c].layers[l].note;
        if n == NONE8 {
            return;
        }
        let ni = n as usize;
        if self.notes[ni].wanted == o {
            self.notes[ni].wanted = NO_OWNER;
        }
        if self.notes[ni].parent != o {
            let nt = &mut self.notes[ni];
            if nt.parent == NO_OWNER && nt.wanted == NO_OWNER && nt.prev_parent == o && !release {
                nt.adsr.fade_out = FAST_RELEASE_Q24;
                nt.adsr.want_release = true;
            }
            return;
        }
        if self.notes[ni].adsr.state != EnvState::Decay {
            let ch = &self.players[p].channels[c];
            let ly = &ch.layers[l];
            let silence = self.players[p].muted && ch.mute_behavior & MUTE_SILENCE != 0;
            let nt = &mut self.notes[ni];
            nt.saved_freq_q16 = ly.note_freq_q16;
            nt.saved_vel_q15 = ly.note_vel_q15;
            nt.saved_pan = ly.note_pan;
            nt.saved_reverb = ch.reverb;
            if silence {
                nt.finished = true;
            }
            nt.priority = ch.low_priority;
            nt.prev_parent = nt.parent;
            nt.parent = NO_OWNER;
            if release {
                nt.adsr.fade_out = FAST_RELEASE_Q24;
                nt.adsr.want_release = true;
                nt.phase = 2;
            } else {
                nt.phase = 1;
                nt.adsr.want_decay = true;
                let idx = if ly.decay_idx == 0 {
                    ch.decay_idx
                } else {
                    ly.decay_idx
                };
                nt.adsr.fade_out = release_step_q24(idx) as i32;
                nt.adsr.sustain = (nt.adsr.current >> 8) * ch.sustain as i32;
            }
        }
        if !release {
            self.notes[ni].list = NoteList::Decaying;
            self.move_front(ni);
        }
    }

    // ---- allocation ------------------------------------------------------

    /// Step 5 of a note command: give the layer a note.
    pub(crate) fn layer_alloc(&mut self, p: usize, c: usize, l: usize, same: bool) {
        let o = owner(p, c, l);
        let ly = self.players[p].channels[c].layers[l];
        if ly.continuous && ly.bit1 {
            return;
        }
        let reuse = ly.continuous
            && ly.note != NONE8
            && ly.bit3
            && same
            && self.notes[ly.note as usize].parent == o;
        if !reuse {
            if !same {
                self.note_decay(o);
            }
            let got = self.alloc_note(o);
            self.players[p].channels[c].layers[l].note = got.map_or(NONE8, |n| n as u8);
            if let Some(n) = got {
                if self.notes[n].parent == o {
                    self.vibrato_init(n);
                }
            }
        }
        let n = self.players[p].channels[c].layers[l].note;
        if n != NONE8 && self.notes[n as usize].parent == o {
            self.notes[n as usize].porta = self.players[p].channels[c].layers[l].porta;
            self.notes[n as usize].porta_freq_q16 = 1 << 16;
        }
    }

    fn alloc_note(&mut self, o: u16) -> Option<usize> {
        let (p, c, l) = unpack(o);
        let (policy, note_priority, ly_note) = {
            let ch = &self.players[p].channels[c];
            (ch.note_alloc_policy, ch.note_priority, ch.layers[l].note)
        };
        if policy & 1 != 0 && ly_note != NONE8 {
            let n = ly_note as usize;
            if self.notes[n].prev_parent == o && self.notes[n].wanted == NO_OWNER {
                self.release_and_take(n, o);
                self.notes[n].list = NoteList::Releasing;
                self.move_back(n);
                return Some(n);
            }
        }
        if let Some(n) = self.edge(NoteList::Disabled, true) {
            self.init_for_layer(n, o);
            self.notes[n].list = NoteList::Active;
            self.move_front(n);
            return Some(n);
        }
        if let Some(n) = self.edge(NoteList::Decaying, true) {
            self.release_and_take(n, o);
            self.notes[n].list = NoteList::Releasing;
            self.move_back(n);
            return Some(n);
        }
        let r = self.lowest_below(NoteList::Releasing, note_priority);
        let a = self.lowest_below(NoteList::Active, note_priority);
        let r_pri = r.map_or(0x10, |i| self.notes[i].priority);
        let a_pri = a.map_or(0x10, |i| self.notes[i].priority);
        match (r, a) {
            (None, None) => {
                self.players[p].channels[c].layers[l].bit3 = true;
                None
            }
            _ if a.is_some() && a_pri < r_pri => {
                let n = a?;
                let old = self.notes[n].parent;
                if old != NO_OWNER {
                    self.note_release(old);
                }
                self.notes[n].wanted = o;
                self.notes[n].list = NoteList::Releasing;
                self.move_back(n);
                self.notes[n].priority = note_priority;
                Some(n)
            }
            _ => {
                let n = r?;
                self.notes[n].wanted = o;
                self.notes[n].priority = note_priority;
                Some(n)
            }
        }
    }

    fn release_and_take(&mut self, n: usize, o: u16) {
        let (p, c, _) = unpack(o);
        let pri = self.players[p].channels[c].note_priority;
        let nt = &mut self.notes[n];
        nt.wanted = o;
        nt.priority = pri;
        nt.adsr.fade_out = FAST_RELEASE_Q24;
        nt.adsr.want_release = true;
    }

    fn init_for_layer(&mut self, n: usize, o: u16) {
        let (p, c, l) = unpack(o);
        let ch = &self.players[p].channels[c];
        let ly = ch.layers[l];
        let env = if ly.decay_idx == 0 { ch.env } else { ly.env };
        self.order += 1;
        let (font, note_priority, reverb) = (ch.font, ch.note_priority, ch.reverb);
        let nt = &mut self.notes[n];
        nt.prev_parent = NO_OWNER;
        nt.parent = o;
        nt.priority = note_priority;
        nt.adsr = Adsr::INIT;
        nt.adsr.player = p as u8;
        nt.adsr.env = env;
        nt.adsr.state = EnvState::Initial;
        nt.phase = 0;
        nt.tuned = ly.tuned;
        nt.font = font;
        nt.reverb = reverb;
        nt.needs_key_on = true;
        nt.finished = false;
        nt.vib_freq_q16 = 1 << 16;
        let cached = self.bank.and_then(|b| {
            let info = b.sample(ly.tuned.sample)?;
            Some((
                self.spu_base + info.adpcm_off,
                info.rate_hz,
                info.blocks,
                info.loop_block != NONE16,
            ))
        });
        let nt = &mut self.notes[n];
        match cached {
            Some((a, r, bl, lp)) => {
                nt.sample_addr = a;
                nt.sample_rate = r;
                nt.sample_blocks = bl;
                nt.sample_looped = lp;
            }
            None => nt.tuned = TunedRef::NONE,
        }
        self.players[p].channels[c].dirty = true;
        let ly = &mut self.players[p].channels[c].layers[l];
        ly.props_need_init = true;
        ly.bit3 = true;
        ly.note = n as u8;
        ly.note_vel_q15 = 0;
    }

    fn note_disable(&mut self, n: usize, hw: &mut impl Hw) {
        let v = self.voice_of[n];
        let nt = &mut self.notes[n];
        if nt.keyed {
            hw.set_volume(v, 0, 0);
            hw.key_off(1 << v);
        }
        nt.keyed = false;
        nt.needs_key_on = false;
        nt.priority = 0;
        nt.phase = 0;
        nt.finished = false;
        nt.parent = NO_OWNER;
        nt.prev_parent = NO_OWNER;
        nt.adsr.state = EnvState::Disabled;
        nt.adsr.current = 0;
        nt.last_l = 0;
        nt.last_r = 0;
        nt.last_pitch = 0;
    }

    // ---- vibrato and portamento -----------------------------------------

    fn vibrato_init(&mut self, n: usize) {
        let Some(o) = (self.notes[n].parent != NO_OWNER).then_some(self.notes[n].parent) else {
            return;
        };
        let (p, c, _) = unpack(o);
        let ch = &self.players[p].channels[c];
        let nt = &mut self.notes[n];
        nt.vib_freq_q16 = 1 << 16;
        let v = &mut nt.vib;
        v.active = true;
        v.time = 0;
        v.chan = ((p as u8) << 4) | c as u8;
        v.depth_timer = ch.vib_depth_delay;
        v.depth = (if v.depth_timer == 0 {
            ch.vib_depth_target
        } else {
            ch.vib_depth_start
        }) as u32
            * 256;
        v.rate_timer = ch.vib_rate_delay;
        v.rate = (if v.rate_timer == 0 {
            ch.vib_rate_target
        } else {
            ch.vib_rate_start
        }) as u32
            * 256;
        v.delay = ch.vib_delay;
    }

    fn vibrato_scale(&mut self, n: usize) -> u32 {
        let (cp, cc) = (
            (self.notes[n].vib.chan >> 4) as usize,
            (self.notes[n].vib.chan & 15) as usize,
        );
        let ch = &self.players[cp].channels[cc];
        let (dt, rt, dd, rd) = (
            ch.vib_depth_target as u32 * 256,
            ch.vib_rate_target as u32 * 256,
            ch.vib_depth_delay,
            ch.vib_rate_delay,
        );
        let v = &mut self.notes[n].vib;
        if v.delay != 0 {
            v.delay -= 1;
            return 1 << 16;
        }
        ramp(&mut v.depth, &mut v.depth_timer, dt, dd);
        ramp(&mut v.rate, &mut v.rate_timer, rt, rd);
        if v.depth == 0 {
            return 1 << 16;
        }
        v.time = v.time.wrapping_add(v.rate >> 8);
        let curve = VIBRATO_SINE[((v.time >> 10) & 63) as usize] as i32;
        let p_q16 = (curve + 32768) as u32;
        let d = (1u32 << 16) + v.depth / 16;
        let inv = u32::MAX / d;
        let t = mul_q16(d - inv, p_q16) + inv;
        u32::MAX / t.max(1)
    }

    fn porta_scale(&mut self, n: usize) -> u32 {
        let pt = &mut self.notes[n].porta;
        pt.cur = pt.cur.wrapping_add(pt.speed);
        let mut lo = ((pt.cur >> 8) & 0xFF) as usize;
        if lo >= 127 {
            lo = 127;
            pt.mode = 0;
        }
        let frac = BEND_OCTAVE_Q16[lo + 128] - (1 << 16);
        let mag = mul_q16(pt.extent_q16.unsigned_abs(), frac) as i32;
        let delta = if pt.extent_q16 < 0 { -mag } else { mag };
        ((1i32 << 16) + delta).max(1) as u32
    }

    // ---- envelope --------------------------------------------------------

    /// One point of an envelope: `(delay, arg)`.
    fn env_point(&self, player: u8, r: EnvRef, idx: u8) -> (i16, i16) {
        match r {
            EnvRef::Default => DEFAULT_ENVELOPE
                .get(idx as usize)
                .copied()
                .unwrap_or((0, 0)),
            EnvRef::Song(off) => {
                let at = off as usize + 4 * idx as usize;
                let code = &self.codes[player as usize];
                match code.get(at..at + 4) {
                    Some(b) => (
                        i16::from_be_bytes([b[0], b[1]]),
                        i16::from_be_bytes([b[2], b[3]]),
                    ),
                    None => (0, 0),
                }
            }
            EnvRef::Bank(w) => self
                .bank
                .and_then(|b| b.envelope_point(w as u32 + idx as u32))
                .unwrap_or((0, 0)),
        }
    }

    /// Step the envelope one update. Returns its level, Q24, in the squared
    /// amplitude domain.
    fn adsr_update(&mut self, n: usize) -> i32 {
        let mut a = self.notes[n].adsr;
        match a.state {
            EnvState::Disabled => return 0,
            EnvState::Initial | EnvState::Loop | EnvState::Fade => {
                if a.state == EnvState::Initial {
                    a.index = 0;
                    a.state = EnvState::Loop;
                }
                if a.state == EnvState::Loop {
                    loop {
                        let (d, arg) = self.env_point(a.player, a.env, a.index);
                        a.delay = d;
                        match d {
                            0 => a.state = EnvState::Disabled,
                            -1 => a.state = EnvState::Hang,
                            -2 => {
                                a.index = arg as u8;
                                continue;
                            }
                            -3 => a.state = EnvState::Initial,
                            _ => {
                                a.delay = ((d as i32 * 3) / 4).max(1) as i16;
                                a.target = level_sq_q24(arg);
                                a.velocity = (a.target - a.current) / a.delay as i32;
                                a.state = EnvState::Fade;
                                a.index = a.index.wrapping_add(1);
                            }
                        }
                        break;
                    }
                }
                if a.state == EnvState::Fade {
                    a.current += a.velocity;
                    a.delay -= 1;
                    if a.delay <= 0 {
                        a.state = EnvState::Loop;
                    }
                }
            }
            EnvState::Hang => {}
            EnvState::Decay | EnvState::Release => {
                a.current -= a.fade_out;
                if a.sustain != 0 && a.state == EnvState::Decay {
                    if a.current < a.sustain {
                        a.current = a.sustain;
                        a.delay = 128;
                        a.state = EnvState::Sustain;
                    }
                } else if a.current < 168 {
                    a.current = 0;
                    a.state = EnvState::Disabled;
                }
            }
            EnvState::Sustain => {
                a.delay -= 1;
                if a.delay == 0 {
                    a.state = EnvState::Release;
                }
            }
        }
        if a.want_decay {
            a.state = EnvState::Decay;
            a.want_decay = false;
        }
        if a.want_release {
            a.state = EnvState::Release;
            a.want_release = false;
        }
        let level = a.current.clamp(0, ONE_Q24);
        self.notes[n].adsr = a;
        level
    }

    // ---- players' sound --------------------------------------------------

    /// Fade and per-channel property update for player `p`, once per update.
    pub(crate) fn player_sound(&mut self, p: usize) {
        let mut disable = false;
        {
            let pl = &mut self.players[p];
            if pl.fade_timer != 0 {
                pl.fade_q24 = (pl.fade_q24 + pl.fade_vel_q24).clamp(0, ONE_Q24);
                pl.recalc_volume = true;
                pl.fade_timer -= 1;
                if pl.fade_timer == 0 && pl.state == 2 {
                    disable = true;
                }
            }
        }
        if disable {
            self.disable_player(p);
            return;
        }
        let recalc = self.players[p].recalc_volume;
        if recalc {
            let pl = &mut self.players[p];
            pl.applied_fade_q15 =
                (((pl.fade_q24 >> 9) as u32 * pl.fade_scale_q15 as u32) >> 15) as u16;
        }
        let apply_bend = false;
        for c in 0..CHANNELS {
            if self.players[p].channels[c].enabled {
                self.channel_sound(p, c, recalc, apply_bend);
            }
        }
        self.players[p].recalc_volume = false;
    }

    fn channel_sound(&mut self, p: usize, c: usize, recalc: bool, apply_bend: bool) {
        let fade = self.players[p].applied_fade_q15 as u32;
        let muted = self.players[p].muted;
        let mute_scale = self.players[p].mute_scale_q15 as u32;
        let pbend = self.players[p].bend_q16;
        let ch = &mut self.players[p].channels[c];
        if !(ch.dirty || recalc || apply_bend || ch.changes.any()) {
            return;
        }
        ch.dirty = false;
        if ch.changes.volume || recalc {
            let mut v = mul_q15(
                mul_q15(ch.volume_q15 as u32, ch.volume_scale_q15 as u32),
                fade,
            );
            if muted && ch.mute_behavior & MUTE_SOFTEN != 0 {
                v = mul_q15(v, mute_scale);
            }
            ch.applied_volume_q15 = mul_q15(v, v) as u16;
        }
        if ch.changes.pan {
            ch.pan = ch.new_pan as u16 * ch.pan_weight as u16;
        }
        let mut chan_freq = ch.freq_q16;
        if apply_bend {
            chan_freq = mul_q16(chan_freq, pbend);
            ch.changes.freq = true;
        }
        let changes = ch.changes;
        let (pan, weight, applied) = (ch.pan, ch.pan_weight as u16, ch.applied_volume_q15 as u32);
        for ly in ch.layers.iter_mut() {
            if !(ly.alloc && ly.enabled && ly.note != NONE8) {
                continue;
            }
            let note_pan = ((pan + ly.pan as u16 * (0x80 - weight)) >> 7).min(127) as u8;
            if ly.props_need_init {
                ly.note_freq_q16 = mul_q16(ly.freq_q16, chan_freq);
                ly.note_vel_q15 = mul_q15(ly.vel_sq2_q15 as u32, applied) as u16;
                ly.note_pan = note_pan;
                ly.props_need_init = false;
            } else {
                if changes.freq {
                    ly.note_freq_q16 = mul_q16(ly.freq_q16, chan_freq);
                }
                if changes.volume || recalc {
                    ly.note_vel_q15 = mul_q15(ly.vel_sq2_q15 as u32, applied) as u16;
                }
                if changes.pan {
                    ly.note_pan = note_pan;
                }
            }
        }
        ch.changes = Changes::NONE;
    }

    /// Stop a player and let go of everything it holds.
    pub(crate) fn disable_player(&mut self, p: usize) {
        for c in 0..CHANNELS {
            if self.players[p].channels[c].enabled
                || self.players[p].channels[c].layers.iter().any(|l| l.alloc)
            {
                self.disable_channel(p, c);
            }
        }
        let pl = &mut self.players[p];
        if pl.enabled {
            pl.enabled = false;
            pl.finished = true;
        }
    }

    // ---- the per-update note pass ---------------------------------------

    /// Walk every note: transfer ownership, step envelopes, and write the
    /// voice registers.
    pub(crate) fn process_notes(&mut self, hw: &mut impl Hw) {
        for n in 0..N {
            let nt = self.notes[n];
            let mut run = true;
            if nt.parent != NO_OWNER {
                let (p, c, l) = unpack(nt.parent);
                let pl = &self.players[p];
                let ch = &pl.channels[c];
                let ly = &ch.layers[l];
                let (ly_note, ly_enabled) = (ly.note, ly.enabled);
                let layer_gone = ly_note != n as u8 && nt.phase == 0;
                if layer_gone {
                    let nt = &mut self.notes[n];
                    nt.adsr.want_release = true;
                    nt.adsr.fade_out = FAST_RELEASE_Q24;
                    nt.priority = 1;
                    nt.phase = 2;
                } else if (!ly_enabled && nt.phase == 0 && nt.priority >= 1)
                    || (pl.muted && ch.mute_behavior & MUTE_STOP_NOTES != 0)
                {
                    self.note_release(nt.parent);
                    self.notes[n].list = NoteList::Decaying;
                    self.move_front(n);
                    self.notes[n].priority = 1;
                    self.notes[n].phase = 2;
                }
            } else if nt.phase == 0 && nt.priority >= 1 {
                run = false;
            }
            if !run || self.notes[n].priority == 0 {
                continue;
            }
            let nt = self.notes[n];
            if nt.phase >= 1 || nt.finished {
                if nt.adsr.state == EnvState::Disabled || nt.finished {
                    if nt.wanted != NO_OWNER {
                        let w = nt.wanted;
                        self.note_disable(n, hw);
                        let (wp, wc, wl) = unpack(w);
                        let _ = (wc, wl);
                        if self.players[wp].channels[wc].layers[wl].alloc {
                            self.init_for_layer(n, w);
                            self.vibrato_init(n);
                            self.notes[n].porta = self.players[wp].channels[wc].layers[wl].porta;
                            self.notes[n].porta_freq_q16 = 1 << 16;
                            self.notes[n].list = NoteList::Active;
                            self.move_back(n);
                            self.notes[n].wanted = NO_OWNER;
                        } else {
                            self.notes[n].list = NoteList::Disabled;
                            self.notes[n].wanted = NO_OWNER;
                            continue;
                        }
                    } else {
                        if nt.parent != NO_OWNER {
                            let (p, c, l) = unpack(nt.parent);
                            self.players[p].channels[c].layers[l].bit1 = true;
                        }
                        self.note_disable(n, hw);
                        self.notes[n].list = NoteList::Disabled;
                        continue;
                    }
                }
            } else if nt.adsr.state == EnvState::Disabled {
                if nt.parent != NO_OWNER {
                    let (p, c, l) = unpack(nt.parent);
                    self.players[p].channels[c].layers[l].bit1 = true;
                }
                self.note_disable(n, hw);
                self.notes[n].list = NoteList::Disabled;
                continue;
            }
            self.drive_voice(n, hw);
        }
    }

    /// Envelope, vibrato, portamento and the register writes for one note.
    fn drive_voice(&mut self, n: usize, hw: &mut impl Hw) {
        let env = self.adsr_update(n) as u32;
        if self.notes[n].porta.mode != 0 {
            let f = self.porta_scale(n);
            self.notes[n].porta_freq_q16 = f;
        }
        if self.notes[n].vib.active {
            let f = self.vibrato_scale(n);
            self.notes[n].vib_freq_q16 = f;
        }
        let nt = self.notes[n];
        let (mut freq, mut vel, pan) = if nt.phase >= 1 {
            (nt.saved_freq_q16, nt.saved_vel_q15 as u32, nt.saved_pan)
        } else {
            let (p, c, l) = unpack(nt.parent);
            let ch = &self.players[p].channels[c];
            let ly = &ch.layers[l];
            if self.players[p].muted && ch.mute_behavior & MUTE_SILENCE != 0 {
                (0, 0, ly.note_pan)
            } else {
                (ly.note_freq_q16, ly.note_vel_q15 as u32, ly.note_pan)
            }
        };
        freq = mul_q16(mul_q16(freq, nt.vib_freq_q16), nt.porta_freq_q16);
        vel = mul_q15(vel, (env >> 9).min(32768));
        let pan = pan.min(127) as usize;
        let l15 = mul_q15(vel, PAN_Q15[pan] as u32);
        let r15 = mul_q15(vel, PAN_Q15[127 - pan] as u32);
        let to_spu = |v: u32| -> i16 {
            let s = (v * SPU_FULL) >> 15;
            ((s * self.master_q12 as u32) >> 12).min(SPU_FULL) as i16
        };
        let (lv, rv) = (to_spu(l15), to_spu(r15));
        let pitch = (freq >> 4).clamp(1, 0xFFFF) as u16;
        let voice = self.voice_of[n];
        if nt.tuned.sample == NONE16 {
            return;
        }
        if nt.needs_key_on {
            let sample = Sample::resident(
                SpuAddr::new(nt.sample_addr),
                nt.sample_rate as u32,
                nt.sample_blocks as u32,
            );
            hw.configure(voice, &sample, nt.sample_looped, pitch);
            hw.set_volume(voice, lv, rv);
            hw.key_on(1 << voice);
            let ntm = &mut self.notes[n];
            ntm.needs_key_on = false;
            ntm.keyed = true;
            ntm.last_l = lv;
            ntm.last_r = rv;
            ntm.last_pitch = pitch;
            ntm.remaining_q8 = if nt.sample_looped {
                u32::MAX
            } else {
                nt.sample_blocks as u32 * 28 * 256
            };
            return;
        }
        let ntm = &mut self.notes[n];
        if !ntm.keyed {
            return;
        }
        if (lv, rv) != (ntm.last_l, ntm.last_r) {
            hw.set_volume(voice, lv, rv);
            ntm.last_l = lv;
            ntm.last_r = rv;
        }
        if pitch != ntm.last_pitch {
            hw.set_pitch(voice, pitch);
            ntm.last_pitch = pitch;
        }
        if ntm.remaining_q8 != u32::MAX {
            let step = (ntm.sample_rate as u32 * pitch as u32) / 2880;
            ntm.remaining_q8 = ntm.remaining_q8.saturating_sub(step);
            if ntm.remaining_q8 == 0 {
                ntm.finished = true;
            }
        }
    }
}

/// Move `value` toward `target`: over `timer` updates when a ramp is running,
/// else jump or start a ramp of `delay` updates.
fn ramp(value: &mut u32, timer: &mut u16, target: u32, delay: u16) {
    if *timer != 0 {
        if *timer == 1 {
            *value = target;
        } else {
            let step = (target as i32 - *value as i32) / *timer as i32;
            *value = (*value as i32 + step) as u32;
        }
        *timer -= 1;
    } else if target != *value {
        *timer = delay;
        if delay == 0 {
            *value = target;
        }
    }
}
