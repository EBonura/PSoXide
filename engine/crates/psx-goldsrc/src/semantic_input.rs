//! Allocation-free parser and map-local player for deterministic `HLINPUT1` routes.
//!
//! The wire format is shared with the local reference harness. This module has
//! no PSX or `std` dependencies so malformed-tape handling and cursor semantics
//! can be tested directly on the host.  Route bytes are borrowed in place; a
//! caller chooses whether a reference build embeds them or loads them elsewhere.

#![allow(dead_code)] // shipping builds use Sample/actions; replay builds use the parser

pub const MAGIC: &[u8; 8] = b"HLINPUT1";
pub const TICK_HZ: u16 = 20;

pub const ACTION_ATTACK: u16 = 1 << 0;
pub const ACTION_JUMP: u16 = 1 << 1;
pub const ACTION_DUCK: u16 = 1 << 2;
pub const ACTION_USE: u16 = 1 << 3;
pub const ACTION_ATTACK2: u16 = 1 << 4;
pub const ACTION_RELOAD: u16 = 1 << 5;
// These three are stored as held state too. The gameplay adapter must feed
// them through its existing rising-edge latches, matching the Xash adapter.
pub const ACTION_NEXT_WEAPON: u16 = 1 << 6;
pub const ACTION_PREV_WEAPON: u16 = 1 << 7;
pub const ACTION_FLASHLIGHT: u16 = 1 << 8;
/// SELECT: open and close the buy menu.
pub const ACTION_BUY: u16 = 1 << 9;
pub const KNOWN_ACTIONS: u16 = (1 << 9) - 1;

/// Convert one opposing digital pair to the signed semantic axis used by the
/// movement adapter.  Opposing buttons cancel, matching a centred stick.
///
/// Keep this independent of `psx_pad`: deterministic host tests can then
/// cover the digital-pad fallback used by normal PSoXide sessions.
#[inline(always)]
pub const fn digital_axis(negative: bool, positive: bool) -> i8 {
    match (negative, positive) {
        (true, false) => -127,
        (false, true) => 127,
        _ => 0,
    }
}

/// Convert one signed semantic look axis to the nearest integer Q0.12 angle
/// step.  The Xash reference adapter keeps the `axis * rate / 128` result in a
/// float.  Truncating it on the PS1 loses almost one whole angle unit for a
/// full-scale stick (`127 * 130 / 128 = 128.984375`) every tick, enough to turn
/// the c0a0e station wall from a shallow slide into a dead stop.
#[inline(always)]
pub fn angle_step_nearest(axis: i32, rate: i32) -> i32 {
    let product = axis * rate;
    if product >= 0 {
        (product + 64) / 128
    } else {
        -((-product + 64) / 128)
    }
}

/// One-tick rising edge of a held action.  Both jump consumers need edge
/// semantics from pm_shared.c: PM_Jump (:2554) requires a release between
/// hops, and the ladder detach (:2116) tests IN_JUMP against oldbuttons --
/// a Cross still held from the hop that reached a ladder must neither block
/// the grab nor instantly dismount it.
#[inline(always)]
pub const fn rising_edge(held: bool, was_held: bool) -> bool {
    held && !was_held
}

#[cfg(test)]
mod input_adapter_tests {
    use super::{digital_axis, rising_edge};

    #[test]
    fn digital_axis_reaches_full_semantic_movement() {
        assert_eq!(digital_axis(false, true), 127);
        assert_eq!(digital_axis(true, false), -127);
    }

    #[test]
    fn opposing_or_idle_directions_cancel() {
        assert_eq!(digital_axis(false, false), 0);
        assert_eq!(digital_axis(true, true), 0);
    }

    #[test]
    fn held_jump_carried_into_a_ladder_mount_is_not_a_fresh_press() {
        assert!(!rising_edge(true, true));
        assert!(rising_edge(true, false));
        assert!(!rising_edge(false, true));
        assert!(!rising_edge(false, false));
    }
}

const HEADER_SIZE: usize = 16;
const SEGMENT_SIZE: usize = 48;
const RUN_SIZE: usize = 8;

const SEGMENT_MAP: usize = 0;
const SEGMENT_NEXT_MAP: usize = 16;
const SEGMENT_TOTAL_TICKS: usize = 32;
const SEGMENT_RUN_COUNT: usize = 36;
const SEGMENT_NEUTRAL_TAIL: usize = 40;
const SEGMENT_FLAGS: usize = 44;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Sample {
    pub forward: i8,
    pub strafe: i8,
    pub turn: i8,
    pub look: i8,
    pub actions: u16,
}

impl Sample {
    pub const NEUTRAL: Self = Self {
        forward: 0,
        strafe: 0,
        turn: 0,
        look: 0,
        actions: 0,
    };

    #[inline(always)]
    pub const fn held(self, action: u16) -> bool {
        self.actions & action != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Truncated,
    BadMagic,
    BadTickRate,
    EmptyRoute,
    UnknownFlags,
    InvalidMapName,
    EmptyRuns,
    ZeroDuration,
    UnknownActions,
    TickTotalMismatch,
    BrokenMapChain,
    TrailingData,
    RouteNotStarted,
    RouteAlreadyStarted,
    UnexpectedMap,
    UnexpectedTick,
    RouteExhausted,
    TickOverflow,
}

#[derive(Clone, Copy)]
pub struct Tape<'a> {
    data: &'a [u8],
    segment_count: u16,
}

#[derive(Clone, Copy)]
struct Segment {
    map_offset: usize,
    total_ticks: u32,
    run_count: u32,
    neutral_tail: u32,
    runs_offset: usize,
    next_offset: usize,
}

#[inline(always)]
fn checked_slice(data: &[u8], offset: usize, len: usize) -> Result<&[u8], Error> {
    let end = offset.checked_add(len).ok_or(Error::Truncated)?;
    data.get(offset..end).ok_or(Error::Truncated)
}

#[inline(always)]
fn read_u16(data: &[u8], offset: usize) -> Result<u16, Error> {
    let bytes = checked_slice(data, offset, 2)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

#[inline(always)]
fn read_u32(data: &[u8], offset: usize) -> Result<u32, Error> {
    let bytes = checked_slice(data, offset, 4)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Validate one fixed-width NUL-padded map name and return its byte length.
fn name_len(raw: &[u8], allow_empty: bool) -> Result<usize, Error> {
    if raw.len() != 16 {
        return Err(Error::InvalidMapName);
    }
    let mut len = 0usize;
    while len < raw.len() && raw[len] != 0 {
        let b = raw[len];
        if !b.is_ascii_alphanumeric() && b != b'_' && b != b'-' {
            return Err(Error::InvalidMapName);
        }
        len += 1;
    }
    // HLINPUT1 names are at most 15 bytes, therefore byte 15 must be NUL.
    if len == raw.len() || (!allow_empty && len == 0) {
        return Err(Error::InvalidMapName);
    }
    let mut padding = len;
    while padding < raw.len() {
        if raw[padding] != 0 {
            return Err(Error::InvalidMapName);
        }
        padding += 1;
    }
    Ok(len)
}

fn runtime_name_valid(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 15 {
        return false;
    }
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if !b.is_ascii_alphanumeric() && b != b'_' && b != b'-' {
            return false;
        }
        i += 1;
    }
    true
}

fn name_matches(raw: &[u8], name: &str) -> bool {
    let Ok(len) = name_len(raw, false) else {
        return false;
    };
    len == name.len() && &raw[..len] == name.as_bytes()
}

impl<'a> Tape<'a> {
    /// Fully validate an HLINPUT1 tape without allocating or retaining indexes.
    pub fn parse(data: &'a [u8]) -> Result<Self, Error> {
        let header = checked_slice(data, 0, HEADER_SIZE)?;
        if &header[..8] != MAGIC {
            return Err(Error::BadMagic);
        }
        if read_u16(data, 8)? != TICK_HZ {
            return Err(Error::BadTickRate);
        }
        let segment_count = read_u16(data, 10)?;
        if segment_count == 0 {
            return Err(Error::EmptyRoute);
        }
        if read_u32(data, 12)? != 0 {
            return Err(Error::UnknownFlags);
        }

        let mut offset = HEADER_SIZE;
        let mut prior_next: Option<&[u8]> = None;
        let mut final_next: &[u8] = &[];
        let mut segment_index = 0u16;
        while segment_index < segment_count {
            let segment = checked_slice(data, offset, SEGMENT_SIZE)?;
            let map_name = &segment[SEGMENT_MAP..SEGMENT_MAP + 16];
            let next_name = &segment[SEGMENT_NEXT_MAP..SEGMENT_NEXT_MAP + 16];
            name_len(map_name, false)?;
            name_len(next_name, true)?;
            if let Some(expected) = prior_next {
                if expected != map_name {
                    return Err(Error::BrokenMapChain);
                }
            }
            if read_u32(segment, SEGMENT_FLAGS)? != 0 {
                return Err(Error::UnknownFlags);
            }
            let declared_ticks = read_u32(segment, SEGMENT_TOTAL_TICKS)?;
            let run_count = read_u32(segment, SEGMENT_RUN_COUNT)?;
            if run_count == 0 {
                return Err(Error::EmptyRuns);
            }

            offset = offset.checked_add(SEGMENT_SIZE).ok_or(Error::Truncated)?;
            let run_bytes = (run_count as usize)
                .checked_mul(RUN_SIZE)
                .ok_or(Error::Truncated)?;
            checked_slice(data, offset, run_bytes)?;

            let mut expanded_ticks = 0u32;
            let mut run_index = 0u32;
            while run_index < run_count {
                let run_offset = offset + run_index as usize * RUN_SIZE;
                let duration = read_u16(data, run_offset)?;
                if duration == 0 {
                    return Err(Error::ZeroDuration);
                }
                expanded_ticks = expanded_ticks
                    .checked_add(duration as u32)
                    .ok_or(Error::TickTotalMismatch)?;
                if read_u16(data, run_offset + 6)? & !KNOWN_ACTIONS != 0 {
                    return Err(Error::UnknownActions);
                }
                run_index += 1;
            }
            if expanded_ticks != declared_ticks {
                return Err(Error::TickTotalMismatch);
            }

            offset += run_bytes;
            prior_next = Some(next_name);
            final_next = next_name;
            segment_index += 1;
        }
        if name_len(final_next, true)? != 0 {
            return Err(Error::BrokenMapChain);
        }
        if offset != data.len() {
            return Err(Error::TrailingData);
        }
        Ok(Self {
            data,
            segment_count,
        })
    }

    #[inline(always)]
    pub const fn segment_count(self) -> u16 {
        self.segment_count
    }

    fn segment(self, offset: usize) -> Result<Segment, Error> {
        let header = checked_slice(self.data, offset, SEGMENT_SIZE)?;
        let run_count = read_u32(header, SEGMENT_RUN_COUNT)?;
        let runs_offset = offset.checked_add(SEGMENT_SIZE).ok_or(Error::Truncated)?;
        let run_bytes = (run_count as usize)
            .checked_mul(RUN_SIZE)
            .ok_or(Error::Truncated)?;
        let next_offset = runs_offset.checked_add(run_bytes).ok_or(Error::Truncated)?;
        checked_slice(self.data, runs_offset, run_bytes)?;
        Ok(Segment {
            map_offset: offset + SEGMENT_MAP,
            total_ticks: read_u32(header, SEGMENT_TOTAL_TICKS)?,
            run_count,
            neutral_tail: read_u32(header, SEGMENT_NEUTRAL_TAIL)?,
            runs_offset,
            next_offset,
        })
    }
}

/// Strict ordered consumer. Each `begin_map` advances to exactly the next tape
/// segment; it never searches by name, so repeated visits are deterministic.
pub struct Player<'a> {
    tape: Tape<'a>,
    next_segment_offset: usize,
    current_map_offset: usize,
    next_run_offset: usize,
    current_total_ticks: u32,
    current_neutral_tail: u32,
    current_run_count: u32,
    runs_read: u32,
    next_tick: u32,
    current_run_remaining: u16,
    current_sample: Sample,
    segments_started: u16,
    active: bool,
}

impl<'a> Player<'a> {
    pub const fn new(tape: Tape<'a>) -> Self {
        Self {
            tape,
            next_segment_offset: HEADER_SIZE,
            current_map_offset: 0,
            next_run_offset: 0,
            current_total_ticks: 0,
            current_neutral_tail: 0,
            current_run_count: 0,
            runs_read: 0,
            next_tick: 0,
            current_run_remaining: 0,
            current_sample: Sample::NEUTRAL,
            segments_started: 0,
            active: false,
        }
    }

    /// Position an unused player at a named segment before its first
    /// `begin_map`. This is used by the direct-map differential harness: one
    /// full campaign tape can boot any contained map, while every subsequent
    /// changelevel remains strictly ordered.
    pub fn seek_to_map(&mut self, map_name: &str) -> Result<(), Error> {
        if self.active || self.segments_started != 0 {
            return Err(Error::RouteAlreadyStarted);
        }
        if !runtime_name_valid(map_name) {
            return Err(Error::InvalidMapName);
        }

        let mut offset = HEADER_SIZE;
        let mut index = 0u16;
        while index < self.tape.segment_count {
            let segment = self.tape.segment(offset)?;
            let raw_name = checked_slice(self.tape.data, segment.map_offset, 16)?;
            if name_matches(raw_name, map_name) {
                self.next_segment_offset = offset;
                self.segments_started = index;
                return Ok(());
            }
            offset = segment.next_offset;
            index += 1;
        }
        Err(Error::UnexpectedMap)
    }

    /// Select the next ordered segment and anchor its first sample at local tick 0.
    ///
    /// Keep the parser out of `play`: that function is already near the MIPS-I
    /// PC-relative branch span, and replay-only inlining can make LLVM emit an
    /// out-of-range PC16 fixup. This runs once per map, so the call is free in
    /// practice and also keeps the hot instruction footprint smaller.
    #[inline(never)]
    pub fn begin_map(&mut self, map_name: &str) -> Result<(), Error> {
        if !runtime_name_valid(map_name) {
            return Err(Error::InvalidMapName);
        }
        if self.segments_started >= self.tape.segment_count {
            return Err(Error::RouteExhausted);
        }
        let segment = self.tape.segment(self.next_segment_offset)?;
        let raw_name = checked_slice(self.tape.data, segment.map_offset, 16)?;
        if !name_matches(raw_name, map_name) {
            return Err(Error::UnexpectedMap);
        }

        self.next_segment_offset = segment.next_offset;
        self.current_map_offset = segment.map_offset;
        self.next_run_offset = segment.runs_offset;
        self.current_total_ticks = segment.total_ticks;
        self.current_neutral_tail = segment.neutral_tail;
        self.current_run_count = segment.run_count;
        self.runs_read = 0;
        self.next_tick = 0;
        self.current_run_remaining = 0;
        self.current_sample = Sample::NEUTRAL;
        self.segments_started += 1;
        self.active = true;
        Ok(())
    }

    /// Consume exactly one sample for the active map's next local gameplay tick.
    /// One call at 20 Hz is cheaper than duplicating this RLE/error machinery in
    /// the already very large fixed-update loop, especially on the PS1 I-cache.
    #[inline(never)]
    pub fn consume(&mut self, map_name: &str, local_tick: u32) -> Result<Sample, Error> {
        if !self.active {
            return Err(Error::RouteNotStarted);
        }
        let raw_name = checked_slice(self.tape.data, self.current_map_offset, 16)?;
        if !runtime_name_valid(map_name) || !name_matches(raw_name, map_name) {
            return Err(Error::UnexpectedMap);
        }
        if local_tick != self.next_tick {
            return Err(Error::UnexpectedTick);
        }
        // There is no representable following local u32 tick. Fail before
        // mutating the RLE cursor rather than wrapping silently to tick zero.
        if local_tick == u32::MAX {
            return Err(Error::TickOverflow);
        }

        let sample = if local_tick < self.current_total_ticks {
            if self.current_run_remaining == 0 {
                if self.runs_read >= self.current_run_count {
                    return Err(Error::TickTotalMismatch);
                }
                let run = checked_slice(self.tape.data, self.next_run_offset, RUN_SIZE)?;
                let duration = read_u16(run, 0)?;
                if duration == 0 {
                    return Err(Error::ZeroDuration);
                }
                self.current_sample = Sample {
                    forward: run[2] as i8,
                    strafe: run[3] as i8,
                    turn: run[4] as i8,
                    look: run[5] as i8,
                    actions: read_u16(run, 6)?,
                };
                self.current_run_remaining = duration;
                self.next_run_offset += RUN_SIZE;
                self.runs_read += 1;
            }
            self.current_run_remaining -= 1;
            self.current_sample
        } else if local_tick - self.current_total_ticks < self.current_neutral_tail {
            Sample::NEUTRAL
        } else {
            return Err(Error::RouteExhausted);
        };

        self.next_tick += 1;
        Ok(sample)
    }

    #[inline(always)]
    pub const fn next_tick(&self) -> u32 {
        self.next_tick
    }

    #[inline(always)]
    pub const fn segments_started(&self) -> u16 {
        self.segments_started
    }

    #[inline(always)]
    pub const fn has_more_segments(&self) -> bool {
        self.segments_started < self.tape.segment_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put_u16(out: &mut Vec<u8>, value: u16) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn put_u32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn put_name(out: &mut Vec<u8>, name: &str) {
        assert!(name.len() <= 15);
        out.extend_from_slice(name.as_bytes());
        out.resize(out.len() + 16 - name.len(), 0);
    }

    fn run(out: &mut Vec<u8>, ticks: u16, sample: Sample) {
        put_u16(out, ticks);
        out.push(sample.forward as u8);
        out.push(sample.strafe as u8);
        out.push(sample.turn as u8);
        out.push(sample.look as u8);
        put_u16(out, sample.actions);
    }

    fn route() -> Vec<u8> {
        let first = Sample {
            forward: -128,
            strafe: 127,
            turn: -1,
            look: 1,
            actions: ACTION_ATTACK | ACTION_FLASHLIGHT,
        };
        let second = Sample {
            forward: 80,
            strafe: -40,
            turn: 8,
            look: -9,
            actions: ACTION_USE | ACTION_JUMP,
        };
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        put_u16(&mut out, TICK_HZ);
        put_u16(&mut out, 2);
        put_u32(&mut out, 0);

        put_name(&mut out, "c1a0b");
        put_name(&mut out, "c1a0c");
        put_u32(&mut out, 3);
        put_u32(&mut out, 2);
        put_u32(&mut out, 2);
        put_u32(&mut out, 0);
        run(&mut out, 2, first);
        run(&mut out, 1, Sample::NEUTRAL);

        put_name(&mut out, "c1a0c");
        put_name(&mut out, "");
        put_u32(&mut out, 1);
        put_u32(&mut out, 1);
        put_u32(&mut out, 1);
        put_u32(&mut out, 0);
        run(&mut out, 1, second);
        out
    }

    #[test]
    fn exact_little_endian_fixture_matches_python_layout() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        put_u16(&mut bytes, 20);
        put_u16(&mut bytes, 1);
        put_u32(&mut bytes, 0);
        put_name(&mut bytes, "c0a0");
        put_name(&mut bytes, "");
        put_u32(&mut bytes, 3);
        put_u32(&mut bytes, 1);
        put_u32(&mut bytes, 4);
        put_u32(&mut bytes, 0);
        run(
            &mut bytes,
            3,
            Sample {
                forward: -128,
                strafe: 127,
                turn: -1,
                look: 1,
                actions: ACTION_ATTACK | ACTION_FLASHLIGHT,
            },
        );
        assert_eq!(
            bytes,
            [
                0x48, 0x4c, 0x49, 0x4e, 0x50, 0x55, 0x54, 0x31, 0x14, 0x00, 0x01, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x63, 0x30, 0x61, 0x30, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
                0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x80, 0x7f, 0xff, 0x01,
                0x01, 0x01,
            ]
        );
        assert_eq!(Tape::parse(&bytes).unwrap().segment_count(), 1);
    }

    #[test]
    fn map_ticks_reset_and_tail_is_neutral() {
        let bytes = route();
        let mut player = Player::new(Tape::parse(&bytes).unwrap());
        assert_eq!(player.consume("c1a0b", 0), Err(Error::RouteNotStarted));
        player.begin_map("c1a0b").unwrap();
        let first = player.consume("c1a0b", 0).unwrap();
        assert_eq!(first.forward, -128);
        assert!(first.held(ACTION_FLASHLIGHT));
        assert_eq!(player.consume("c1a0b", 1).unwrap(), first);
        assert_eq!(player.consume("c1a0b", 2).unwrap(), Sample::NEUTRAL);
        assert_eq!(player.consume("c1a0b", 3).unwrap(), Sample::NEUTRAL);
        assert_eq!(player.consume("c1a0b", 4).unwrap(), Sample::NEUTRAL);
        assert_eq!(player.consume("c1a0b", 5), Err(Error::RouteExhausted));

        player.begin_map("c1a0c").unwrap();
        let next = player.consume("c1a0c", 0).unwrap();
        assert_eq!(
            (next.forward, next.strafe, next.turn, next.look),
            (80, -40, 8, -9)
        );
        assert!(next.held(ACTION_USE));
        assert_eq!(player.consume("c1a0c", 1).unwrap(), Sample::NEUTRAL);
        assert_eq!(player.consume("c1a0c", 2), Err(Error::RouteExhausted));
        assert!(!player.has_more_segments());
    }

    #[test]
    fn map_order_and_local_tick_are_strict_but_early_transition_is_allowed() {
        let bytes = route();
        let mut player = Player::new(Tape::parse(&bytes).unwrap());
        assert_eq!(player.begin_map("c1a0c"), Err(Error::UnexpectedMap));
        player.begin_map("c1a0b").unwrap();
        assert_eq!(player.consume("c1a0b", 1), Err(Error::UnexpectedTick));
        player.consume("c1a0b", 0).unwrap();
        assert_eq!(player.consume("c1a0c", 1), Err(Error::UnexpectedMap));
        // A faster implementation may change level before consuming every old
        // map command; the next ordered map still anchors at its own tick zero.
        player.begin_map("c1a0c").unwrap();
        assert_eq!(player.next_tick(), 0);
        player.consume("c1a0c", 0).unwrap();
        assert_eq!(player.segments_started(), 2);
        assert_eq!(player.begin_map("c1a0c"), Err(Error::RouteExhausted));
    }

    #[test]
    fn repeated_map_names_select_ordered_occurrences_not_a_search_hit() {
        let mut bytes = route();
        // c1a0b -> c1a0c -> c1a0c is invalid as currently encoded. Rename the
        // first two map fields so the valid ordered route is loop -> loop.
        let first_map = HEADER_SIZE;
        let first_next = first_map + 16;
        let second_header = HEADER_SIZE + SEGMENT_SIZE + 2 * RUN_SIZE;
        for offset in [first_map, first_next, second_header] {
            bytes[offset..offset + 16].fill(0);
            bytes[offset..offset + 4].copy_from_slice(b"loop");
        }
        let mut player = Player::new(Tape::parse(&bytes).unwrap());
        player.begin_map("loop").unwrap();
        player.consume("loop", 0).unwrap();
        player.begin_map("loop").unwrap();
        let second_visit = player.consume("loop", 0).unwrap();
        assert_eq!(second_visit.forward, 80);
    }

    #[test]
    fn direct_map_seek_keeps_the_remaining_chain_strict() {
        let bytes = route();
        let tape = Tape::parse(&bytes).unwrap();
        let mut player = Player::new(tape);

        player.seek_to_map("c1a0c").unwrap();
        player.begin_map("c1a0c").unwrap();
        assert_eq!(player.segments_started(), 2);
        assert_eq!(player.consume("c1a0c", 0).unwrap().forward, 80);
        assert_eq!(player.seek_to_map("c1a0b"), Err(Error::RouteAlreadyStarted));
        assert!(!player.has_more_segments());
    }

    #[test]
    fn direct_map_seek_rejects_an_absent_segment() {
        let bytes = route();
        let tape = Tape::parse(&bytes).unwrap();
        let mut player = Player::new(tape);
        assert_eq!(player.seek_to_map("c9a9"), Err(Error::UnexpectedMap));
    }

    #[test]
    fn parser_rejects_corrupt_header_flags_names_and_trailing_bytes() {
        let good = route();
        let mut bad = good.clone();
        bad[0] = b'X';
        assert!(matches!(Tape::parse(&bad), Err(Error::BadMagic)));
        bad = good.clone();
        bad[8..10].copy_from_slice(&60u16.to_le_bytes());
        assert!(matches!(Tape::parse(&bad), Err(Error::BadTickRate)));
        bad = good.clone();
        bad[12] = 1;
        assert!(matches!(Tape::parse(&bad), Err(Error::UnknownFlags)));
        bad = good.clone();
        bad[HEADER_SIZE + SEGMENT_FLAGS] = 1;
        assert!(matches!(Tape::parse(&bad), Err(Error::UnknownFlags)));
        bad = good.clone();
        bad[HEADER_SIZE + 5] = b'/';
        assert!(matches!(Tape::parse(&bad), Err(Error::InvalidMapName)));
        bad = good.clone();
        bad[HEADER_SIZE + 6] = b'x'; // nonzero after c1a0b's terminator
        assert!(matches!(Tape::parse(&bad), Err(Error::InvalidMapName)));
        bad = good.clone();
        bad.push(0);
        assert!(matches!(Tape::parse(&bad), Err(Error::TrailingData)));
        assert!(matches!(Tape::parse(&good[..15]), Err(Error::Truncated)));
    }

    #[test]
    fn parser_rejects_bad_runs_totals_actions_chain_and_bounds() {
        let good = route();
        let first_run = HEADER_SIZE + SEGMENT_SIZE;
        let mut bad = good.clone();
        bad[first_run..first_run + 2].fill(0);
        assert!(matches!(Tape::parse(&bad), Err(Error::ZeroDuration)));
        bad = good.clone();
        bad[first_run + 6..first_run + 8].copy_from_slice(&(KNOWN_ACTIONS + 1).to_le_bytes());
        assert!(matches!(Tape::parse(&bad), Err(Error::UnknownActions)));
        bad = good.clone();
        bad[HEADER_SIZE + SEGMENT_TOTAL_TICKS..HEADER_SIZE + SEGMENT_TOTAL_TICKS + 4]
            .copy_from_slice(&999u32.to_le_bytes());
        assert!(matches!(Tape::parse(&bad), Err(Error::TickTotalMismatch)));
        bad = good.clone();
        bad[HEADER_SIZE + SEGMENT_RUN_COUNT..HEADER_SIZE + SEGMENT_RUN_COUNT + 4].fill(0);
        assert!(matches!(Tape::parse(&bad), Err(Error::EmptyRuns)));
        bad = good.clone();
        bad[HEADER_SIZE + 16] = b'x';
        assert!(matches!(Tape::parse(&bad), Err(Error::BrokenMapChain)));
        bad = good.clone();
        bad[HEADER_SIZE + SEGMENT_RUN_COUNT..HEADER_SIZE + SEGMENT_RUN_COUNT + 4]
            .copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(Tape::parse(&bad), Err(Error::Truncated)));
        assert!(matches!(
            Tape::parse(&good[..good.len() - 1]),
            Err(Error::Truncated)
        ));
    }

    #[test]
    fn every_defined_action_bit_is_accepted_and_unknown_bits_are_not() {
        let all = ACTION_ATTACK
            | ACTION_JUMP
            | ACTION_DUCK
            | ACTION_USE
            | ACTION_ATTACK2
            | ACTION_RELOAD
            | ACTION_NEXT_WEAPON
            | ACTION_PREV_WEAPON
            | ACTION_FLASHLIGHT;
        assert_eq!(all, KNOWN_ACTIONS);
        let mut bytes = route();
        let first_run = HEADER_SIZE + SEGMENT_SIZE;
        bytes[first_run + 6..first_run + 8].copy_from_slice(&all.to_le_bytes());
        Tape::parse(&bytes).unwrap();
    }

    #[test]
    fn cursor_is_small_and_owns_no_route_storage() {
        // This host bound is deliberately loose across 32/64-bit ABIs. On the
        // 32-bit PS1 ABI the same fields occupy roughly 56 bytes of caller state.
        assert!(core::mem::size_of::<Player<'static>>() <= 128);
        assert!(core::mem::size_of::<Tape<'static>>() <= 24);
    }

    #[test]
    fn semantic_angle_steps_round_symmetrically_like_the_float_reference() {
        assert_eq!(angle_step_nearest(127, 130), 129);
        assert_eq!(angle_step_nearest(-127, 130), -129);
        assert_eq!(angle_step_nearest(64, 130), 65);
        assert_eq!(angle_step_nearest(-64, 130), -65);
        assert_eq!(angle_step_nearest(1, 95), 1);
        assert_eq!(angle_step_nearest(-1, 95), -1);
        assert_eq!(angle_step_nearest(0, 130), 0);
    }
}
