//! State records for the three levels and the note table.
//!
//! Everything is `Copy` plain data in fixed arrays, so a [`Sequencer`] can
//! live in a `static` and its size is known at compile time.
//!
//! [`Sequencer`]: crate::Sequencer

use crate::format::NONE16;

/// "No note", "no layer" and similar.
pub(crate) const NONE8: u8 = 0xff;
/// "No owner" for a note.
pub(crate) const NO_OWNER: u16 = 0xffff;
/// IO port value meaning "never written".
pub const IO_NONE: u8 = 0xff;
/// Channels per player.
pub const CHANNELS: usize = 16;
/// Layers per channel.
pub const LAYERS: usize = 4;
/// IO ports per player and per channel.
pub const IO_PORTS: usize = 8;

/// Bit in [`Player::mute_behavior`]: lower volume while muted.
pub(crate) const MUTE_SOFTEN: u8 = 0x20;
/// Bit: stop running the sequence script while muted.
pub(crate) const MUTE_STOP_SCRIPT: u8 = 0x80;
/// Bit: stop sounding notes while muted.
pub(crate) const MUTE_STOP_NOTES: u8 = 0x40;
/// Bit: silence notes while muted.
pub(crate) const MUTE_SILENCE: u8 = 0x08;
/// Bit: skip notes in layer scripts while muted.
pub(crate) const MUTE_SKIP_LAYER: u8 = 0x10;

/// One of the three scripts' execution state.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Script {
    pub pc: u16,
    pub depth: u8,
    pub stack: [u16; 4],
    pub loops: [u8; 4],
    pub value: i8,
}

impl Script {
    pub const INIT: Self = Self {
        pc: 0,
        depth: 0,
        stack: [0; 4],
        loops: [0; 4],
        value: 0,
    };
}

/// Where an envelope's points live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EnvRef {
    /// The built-in default.
    Default,
    /// Byte offset into the song's bytecode.
    Song(u16),
    /// Word index into the bank.
    Bank(u16),
}

/// An instrument chosen by a channel or layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InstSel {
    pub font: u8,
    pub id: u8,
}

impl InstSel {
    pub const NONE: Self = Self {
        font: NONE8,
        id: NONE8,
    };
    pub fn is_none(self) -> bool {
        self.id == NONE8
    }
}

/// A sample with its tuning, as a layer holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TunedRef {
    pub sample: u16,
    pub tuning_q16: u32,
}

impl TunedRef {
    pub const NONE: Self = Self {
        sample: NONE16,
        tuning_q16: 0,
    };
}

/// Portamento settings of a layer (copied into a note when it starts).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Porta {
    /// 0 off; bit 7 selects the tempo-relative speed; low bits are the mode.
    pub mode: u8,
    pub extent_q16: i32,
    pub speed: u16,
    pub cur: u16,
}

impl Porta {
    pub const OFF: Self = Self {
        mode: 0,
        extent_q16: 0,
        speed: 0,
        cur: 0,
    };
}

/// Fields of a channel that changed since the last property update.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Changes {
    pub volume: bool,
    pub pan: bool,
    pub freq: bool,
}

impl Changes {
    pub const fn any(self) -> bool {
        self.volume || self.pan || self.freq
    }

    pub const ALL: Self = Self {
        volume: true,
        pan: true,
        freq: true,
    };
    pub const NONE: Self = Self {
        volume: false,
        pan: false,
        freq: false,
    };
}

/// A sequence channel.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Channel {
    pub enabled: bool,
    pub dirty: bool,
    pub muted: bool,
    pub finished: bool,
    pub stop_script: bool,
    pub has_instrument: bool,
    pub large_notes: bool,
    pub script: Script,
    pub delay: u16,
    pub font: u8,
    pub inst_or_wave: u8,
    pub inst: InstSel,
    pub transpose: i8,
    pub mute_behavior: u8,
    pub note_alloc_policy: u8,
    pub env: EnvRef,
    pub decay_idx: u8,
    pub sustain: u8,
    pub volume_q15: u16,
    pub volume_scale_q15: u16,
    pub applied_volume_q15: u16,
    pub new_pan: u8,
    pub pan_weight: u8,
    pub pan: u16,
    pub freq_q16: u32,
    pub changes: Changes,
    pub vib_rate_target: u16,
    pub vib_rate_start: u16,
    pub vib_depth_target: u16,
    pub vib_depth_start: u16,
    pub vib_rate_delay: u16,
    pub vib_depth_delay: u16,
    pub vib_delay: u16,
    pub reverb: u8,
    pub note_priority: u8,
    pub low_priority: u8,
    pub vel_variance: u8,
    pub gate_variance: u8,
    pub io: [u8; IO_PORTS],
    pub ptr: u16,
    /// Start of the table `dyncall` indexes (a list of big-endian script offsets).
    pub dyn_table: u16,
    pub layers: [Layer; LAYERS],
}

impl Channel {
    pub const INIT: Self = Self {
        enabled: false,
        dirty: true,
        muted: false,
        finished: false,
        stop_script: false,
        has_instrument: false,
        large_notes: false,
        script: Script::INIT,
        delay: 0,
        font: NONE8,
        inst_or_wave: 0,
        inst: InstSel::NONE,
        transpose: 0,
        mute_behavior: 0,
        note_alloc_policy: 0,
        env: EnvRef::Default,
        decay_idx: 0xf0,
        sustain: 0,
        volume_q15: 32768,
        volume_scale_q15: 32768,
        applied_volume_q15: 0,
        new_pan: 0x40,
        pan_weight: 0x80,
        pan: 0,
        freq_q16: 1 << 16,
        changes: Changes::ALL,
        vib_rate_target: 0x800,
        vib_rate_start: 0x800,
        vib_depth_target: 0,
        vib_depth_start: 0,
        vib_rate_delay: 0,
        vib_depth_delay: 0,
        vib_delay: 0,
        reverb: 0,
        note_priority: 3,
        low_priority: 1,
        vel_variance: 0,
        gate_variance: 0,
        io: [IO_NONE; IO_PORTS],
        ptr: 0,
        dyn_table: 0,
        layers: [Layer::INIT; LAYERS],
    };
}

/// A layer: one voice's worth of notes within a channel.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Layer {
    pub alloc: bool,
    pub enabled: bool,
    pub finished: bool,
    pub muted: bool,
    pub continuous: bool,
    pub bit1: bool,
    pub bit3: bool,
    pub ignore_drum_pan: bool,
    pub props_need_init: bool,
    pub script: Script,
    pub delay: u16,
    pub gate_delay: i16,
    pub gate_time: u8,
    pub last_delay: u16,
    pub short_delay: u16,
    pub pan: u8,
    pub transpose: i8,
    pub vel_sq_q15: u16,
    pub vel_sq2_q15: u16,
    pub inst_or_wave: u8,
    pub inst: InstSel,
    pub env: EnvRef,
    pub decay_idx: u8,
    pub tuned: TunedRef,
    pub freq_q16: u32,
    pub bend_q16: u32,
    pub porta: Porta,
    pub porta_target: u8,
    pub porta_time: u16,
    pub note: u8,
    pub note_freq_q16: u32,
    pub note_vel_q15: u16,
    pub note_pan: u8,
}

impl Layer {
    pub const INIT: Self = Self {
        alloc: false,
        enabled: false,
        finished: false,
        muted: false,
        continuous: false,
        bit1: false,
        bit3: false,
        ignore_drum_pan: false,
        props_need_init: false,
        script: Script::INIT,
        delay: 0,
        gate_delay: 0,
        gate_time: 0x80,
        last_delay: 0,
        short_delay: 0,
        pan: 0x40,
        transpose: 0,
        vel_sq_q15: 0,
        vel_sq2_q15: 0,
        inst_or_wave: 0xff,
        inst: InstSel::NONE,
        env: EnvRef::Default,
        decay_idx: 0,
        tuned: TunedRef::NONE,
        freq_q16: 1 << 16,
        bend_q16: 1 << 16,
        porta: Porta::OFF,
        porta_target: 0,
        porta_time: 0,
        note: NONE8,
        note_freq_q16: 1 << 16,
        note_vel_q15: 0,
        note_pan: 0x40,
    };
}

/// Envelope states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EnvState {
    Disabled,
    Initial,
    Loop,
    Fade,
    Hang,
    Decay,
    Release,
    Sustain,
}

/// A note's envelope.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Adsr {
    pub player: u8,
    pub state: EnvState,
    pub env: EnvRef,
    pub index: u8,
    pub delay: i16,
    pub target: i32,
    pub velocity: i32,
    pub current: i32,
    pub sustain: i32,
    pub fade_out: i32,
    pub want_decay: bool,
    pub want_release: bool,
}

impl Adsr {
    pub const INIT: Self = Self {
        player: 0,
        state: EnvState::Disabled,
        env: EnvRef::Default,
        index: 0,
        delay: 0,
        target: 0,
        velocity: 0,
        current: 0,
        sustain: 0,
        fade_out: 0,
        want_decay: false,
        want_release: false,
    };
}

/// A note's vibrato.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Vibrato {
    pub active: bool,
    /// Driven by the game (a live note), not by a channel's targets.
    pub live: bool,
    pub time: u32,
    /// Q8.
    pub depth: u32,
    /// Q8.
    pub rate: u32,
    pub delay: u16,
    pub depth_timer: u16,
    pub rate_timer: u16,
    /// `player << 4 | channel` of the channel whose targets it follows.
    pub chan: u8,
}

impl Vibrato {
    pub const INIT: Self = Self {
        active: false,
        live: false,
        time: 0,
        depth: 0,
        rate: 0,
        delay: 0,
        depth_timer: 0,
        rate_timer: 0,
        chan: 0,
    };
}

/// Which list a note is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoteList {
    Disabled,
    Active,
    Decaying,
    Releasing,
}

/// A note: one SPU voice's current assignment.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Note {
    pub list: NoteList,
    /// Started by the game through `note_on`, not by a layer.
    pub live: bool,
    /// Bumped each time a live note starts, so a stale handle is harmless.
    pub live_gen: u8,
    /// Release rate index of a live note's instrument.
    pub live_decay: u8,
    /// 0 normal, 1 decaying, 2 releasing.
    pub phase: u8,
    pub priority: u8,
    pub parent: u16,
    pub wanted: u16,
    pub prev_parent: u16,
    pub pos: i32,
    pub adsr: Adsr,
    pub vib: Vibrato,
    pub vib_freq_q16: u32,
    pub porta: Porta,
    pub porta_freq_q16: u32,
    pub tuned: TunedRef,
    pub font: u8,
    /// Attributes saved when the layer lets go of the note.
    pub saved_freq_q16: u32,
    pub saved_vel_q15: u16,
    pub saved_pan: u8,
    pub saved_reverb: u8,
    pub reverb: u8,
    // hardware side
    pub sample_addr: u32,
    pub sample_rate: u16,
    pub sample_blocks: u16,
    pub sample_looped: bool,
    pub needs_key_on: bool,
    pub keyed: bool,
    pub finished: bool,
    pub remaining_q8: u32,
    pub last_l: i16,
    pub last_r: i16,
    pub last_pitch: u16,
}

impl Note {
    pub const INIT: Self = Self {
        list: NoteList::Disabled,
        live: false,
        live_gen: 0,
        live_decay: 0,
        phase: 0,
        priority: 0,
        parent: NO_OWNER,
        wanted: NO_OWNER,
        prev_parent: NO_OWNER,
        pos: 0,
        adsr: Adsr::INIT,
        vib: Vibrato::INIT,
        vib_freq_q16: 1 << 16,
        porta: Porta::OFF,
        porta_freq_q16: 1 << 16,
        tuned: TunedRef::NONE,
        font: NONE8,
        saved_freq_q16: 1 << 16,
        saved_vel_q15: 0,
        saved_pan: 0x40,
        saved_reverb: 0,
        reverb: 0,
        sample_addr: 0,
        sample_rate: 0,
        sample_blocks: 0,
        sample_looped: false,
        needs_key_on: false,
        keyed: false,
        finished: false,
        remaining_q8: 0,
        last_l: 0,
        last_r: 0,
        last_pitch: 0,
    };
}

/// A sequence player.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Player {
    pub enabled: bool,
    pub finished: bool,
    pub muted: bool,
    pub stop_script: bool,
    pub script: Script,
    pub delay: u16,
    pub tempo: u16,
    pub tempo_acc: u16,
    pub tempo_change: i16,
    pub transpose: i8,
    pub note_alloc_policy: u8,
    pub mute_behavior: u8,
    pub mute_scale_q15: u16,
    pub default_font: u8,
    pub fonts: [u8; 8],
    pub n_fonts: u8,
    pub short_vel: Option<u16>,
    pub short_gate: Option<u16>,
    pub io: [u8; IO_PORTS],
    /// 0 idle, 1 start, 2 stop.
    pub state: u8,
    pub fade_q24: i32,
    pub fade_vel_q24: i32,
    pub fade_timer: u16,
    pub fade_timer_default: u16,
    pub fade_scale_q15: u16,
    pub applied_fade_q15: u16,
    pub recalc_volume: bool,
    pub bend_q16: u32,
    pub script_counter: u16,
    pub channels: [Channel; CHANNELS],
}

impl Player {
    /// Write a player at its start values into `slot`, field by field, so the
    /// seven kilobytes of it are never built on the stack first.
    ///
    /// # Safety
    ///
    /// `slot` must be valid for writes of a `Player` and aligned for one; any
    /// player it held is overwritten without being read.
    pub(crate) unsafe fn init_at(slot: *mut Self) {
        use core::ptr::addr_of_mut;
        macro_rules! put {
            ($field:ident = $value:expr) => {
                addr_of_mut!((*slot).$field).write($value)
            };
        }
        // SAFETY: the caller guarantees `slot` is valid and aligned; each field is written once,
        // through a raw place, without forming a reference to the uninitialised player.
        unsafe {
            put!(enabled = false);
            put!(finished = false);
            put!(muted = false);
            put!(stop_script = false);
            put!(script = Script::INIT);
            put!(delay = 0);
            put!(tempo = 120 * 48);
            put!(tempo_acc = 0);
            put!(tempo_change = 0);
            put!(transpose = 0);
            put!(note_alloc_policy = 0);
            put!(mute_behavior = MUTE_SOFTEN | MUTE_STOP_NOTES);
            put!(mute_scale_q15 = 16384);
            put!(default_font = NONE8);
            put!(fonts = [0; 8]);
            put!(n_fonts = 0);
            put!(short_vel = None);
            put!(short_gate = None);
            put!(io = [IO_NONE; IO_PORTS]);
            put!(state = 1);
            put!(fade_q24 = 1 << 24);
            put!(fade_vel_q24 = 0);
            put!(fade_timer = 0);
            put!(fade_timer_default = 0);
            put!(fade_scale_q15 = 32768);
            put!(applied_fade_q15 = 32768);
            put!(recalc_volume = true);
            put!(bend_q16 = 1 << 16);
            put!(script_counter = 0);
            for c in 0..CHANNELS {
                addr_of_mut!((*slot).channels[c]).write(Channel::INIT);
            }
        }
    }
}
