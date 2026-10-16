//! Notes the game plays itself, over the music: an ocarina, a jingle, a
//! voice. They go through the same note engine as the song's own notes (the
//! instrument's envelope, vibrato, the priority allocator), but nothing in a
//! script owns them: the game starts, bends and releases each one.

use crate::engine::vel_sq_q15;
use crate::format::NONE16;
use crate::tables::*;
use crate::types::*;
use crate::Sequencer;
use psx_sfx::hw::Hw;

/// A note started by [`Sequencer::note_on`]. A stale handle (the note ended
/// and its voice was reused) is harmless: every call on it returns `false`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoteHandle(u16);

impl<'a, const P: usize, const N: usize> Sequencer<'a, P, N> {
    fn live_index(&self, h: NoteHandle) -> Option<usize> {
        let n = (h.0 >> 8) as usize;
        let nt = self.notes.get(n)?;
        (nt.live && nt.live_gen == (h.0 & 0xFF) as u8 && nt.list != NoteList::Disabled).then_some(n)
    }

    /// Start a note: `instrument` of the soundfont whose original id is
    /// `font_id` (it must be in the loaded bank), at semitone `key`, with
    /// MIDI-like `velocity` and `pan` (0 left, 64 centre, 127 right) and a
    /// note `priority` (1 to 15, larger is more important; it takes a free
    /// voice, then an old decaying one, then the least important sounding
    /// note below it). Returns `None` when no voice can be had.
    ///
    /// ```
    /// # use psx_seq::Sequencer;
    /// # use psx_sfx::hw::Recorder;
    /// let mut seq: Sequencer<'_, 1, 4> = Sequencer::new(0b1111);
    /// let mut hw = Recorder::<8>::new();
    /// // no bank loaded: nothing to play
    /// assert!(seq.note_on(&mut hw, 0, 0, 60, 100, 64, 9).is_none());
    /// ```
    #[allow(clippy::too_many_arguments)]
    pub fn note_on(
        &mut self,
        hw: &mut impl Hw,
        font_id: u8,
        instrument: u8,
        key: u8,
        velocity: u8,
        pan: u8,
        priority: u8,
    ) -> Option<NoteHandle> {
        let bank = self.bank?;
        let font = bank.font_index(font_id)?;
        let ins = bank.instrument(font, instrument)?;
        let tuned = ins.for_key(key);
        let info = bank.sample(tuned.sample)?;
        let priority = priority.clamp(1, 15);
        let n = self.live_voice(hw, priority)?;
        let gen = self.notes[n].live_gen.wrapping_add(1);
        let nt = &mut self.notes[n];
        *nt = Note::INIT;
        nt.live = true;
        nt.live_gen = gen;
        nt.live_decay = ins.decay;
        nt.priority = priority;
        nt.adsr.env = if ins.env == NONE16 {
            EnvRef::Default
        } else {
            EnvRef::Bank(ins.env)
        };
        nt.adsr.state = EnvState::Initial;
        nt.tuned = TunedRef {
            sample: tuned.sample,
            tuning_q16: tuned.tuning_q16,
        };
        nt.sample_addr = self.spu_base + info.adpcm_off;
        nt.sample_rate = info.rate_hz;
        nt.sample_blocks = info.blocks;
        nt.sample_looped = info.loop_block != NONE16;
        nt.saved_freq_q16 = mul_q16(PITCH_Q16[(key & 0x7F) as usize], tuned.tuning_q16);
        nt.saved_vel_q15 = vel_sq_q15(velocity);
        nt.saved_pan = pan.min(127);
        nt.needs_key_on = true;
        nt.list = NoteList::Active;
        self.move_front(n);
        Some(NoteHandle(((n as u16) << 8) | gen as u16))
    }

    /// A voice for a live note of `priority`: free, else the oldest decaying,
    /// else the least important sounding note below `priority` (cut off at
    /// once, which can click; it is the last resort).
    fn live_voice(&mut self, hw: &mut impl Hw, priority: u8) -> Option<usize> {
        let pick = self
            .edge(NoteList::Disabled, true)
            .or_else(|| self.edge(NoteList::Decaying, true))
            .or_else(|| self.lowest_below(NoteList::Releasing, priority))
            .or_else(|| self.lowest_below(NoteList::Active, priority))?;
        if self.notes[pick].list != NoteList::Disabled {
            let parent = self.notes[pick].parent;
            if parent != NO_OWNER {
                let (p, c, l) = crate::engine::unpack(parent);
                self.players[p].channels[c].layers[l].note = NONE8;
            }
            self.note_disable(pick, hw);
            self.notes[pick].wanted = NO_OWNER;
        }
        Some(pick)
    }

    /// Whether the note is still sounding.
    pub fn note_alive(&self, h: NoteHandle) -> bool {
        self.live_index(h).is_some()
    }

    /// Bend the note's pitch by `bend_q16` (65536 = none, 131072 = an octave
    /// up). Returns whether the handle was live.
    pub fn note_bend(&mut self, h: NoteHandle, bend_q16: u32) -> bool {
        match self.live_index(h) {
            Some(n) => {
                self.notes[n].porta_freq_q16 = bend_q16.max(1);
                true
            }
            None => false,
        }
    }

    /// Give the note vibrato, with the depth and rate units of the `vibdepth`
    /// and `vibfreq` channel opcodes (0 turns it off).
    pub fn note_vibrato(&mut self, h: NoteHandle, depth: u8, rate: u8) -> bool {
        match self.live_index(h) {
            Some(n) => {
                let v = &mut self.notes[n].vib;
                v.live = true;
                v.active = depth != 0;
                v.depth = depth as u32 * 8 * 256;
                v.rate = rate as u32 * 32 * 256;
                v.delay = 0;
                if depth == 0 {
                    self.notes[n].vib_freq_q16 = 1 << 16;
                }
                true
            }
            None => false,
        }
    }

    /// Change a held note's loudness (`velocity` as in [`note_on`](Self::note_on)).
    pub fn note_velocity(&mut self, h: NoteHandle, velocity: u8) -> bool {
        match self.live_index(h) {
            Some(n) => {
                self.notes[n].saved_vel_q15 = vel_sq_q15(velocity);
                true
            }
            None => false,
        }
    }

    /// Let the note go: it decays at its instrument's release rate.
    pub fn note_off(&mut self, h: NoteHandle) -> bool {
        match self.live_index(h) {
            Some(n) => {
                if self.notes[n].phase == 0 {
                    let decay = self.notes[n].live_decay;
                    let nt = &mut self.notes[n];
                    nt.phase = 1;
                    nt.adsr.want_decay = true;
                    nt.adsr.fade_out = release_step_q24(decay) as i32;
                    nt.adsr.sustain = 0;
                    nt.list = NoteList::Decaying;
                    self.move_front(n);
                }
                true
            }
            None => false,
        }
    }
}
