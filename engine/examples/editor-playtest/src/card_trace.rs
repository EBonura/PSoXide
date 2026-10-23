//! Feature `card-trace` (with `emulator-telemetry`): one debug line per event of
//! the far cards, for measuring how late a card is.
//!
//! * `ct E room dist` a room's card came into the band the draw shows cards in
//!   (`hide_all` to `far` plan units from the eye).
//! * `ct A room dist` that room's card is first drawn with any picture.
//! * `ct K room dist` that room's card is first drawn with a picture serving the
//!   eye's side (the stream's own rule: [`view_keep`] or [`view_toward`]).
//! * `ct H y eye_y` the screen row of the horizon at the middle of the view, 4096 added (9999: too
//!   steep to say), and the eye's height, 32768 added, every drawn frame, for `gapscan`.
//! * `ct G gaps checked nearest_gap_length nearest_gap_room` every frame in which a room whose
//!   centre is within the drawn radius and in the view cone has neither its geometry drawn whole
//!   nor its card drawn (`checked` counts the rooms tested).
//! * `ct F band_missing band_stale seen_missing seen_stale seen_bad` every frame
//!   that has any: cards in the band without a picture or with a picture of the
//!   wrong side, the same counted only for cards inside the view cone, and of
//!   those the ones two or more view steps off.
//! * `ct S held0 held1 held2 shown0 shown1 shown2` slots held by each layer of cards, and how many of them show
//!   a picture, every 30 drawn frames.
//! * `ct D drawn looked fading` cards drawn, slots looked at and views fading in, every 30
//!   drawn frames.
//! * `ct P x z` the eye, 32768 added to each, every 30 drawn frames.
//! * `ct R submitted landed failed uploads live starved refused evictions merged backlog blank last_first last_sectors` when
//!   the stream's counters changed.
//!
//! Off in every shipped build; the lines only exist in measurement discs.

use psx_engine::telemetry;
use psx_game_runtime::card_stream::{CardCounters, MAX_ROOMS};
use psx_grid::render::{view_keep, view_toward, CardTable, LiveSlot, NO_VIEW, SLOTS};
use psx_math::int32::isqrt_i32;

struct Trace {
    /// Rooms whose card the draw wrote this frame ([`mark`]).
    drawn: [bool; MAX_ROOMS],
    in_band: [bool; MAX_ROOMS],
    waiting: [bool; MAX_ROOMS],
    blank: [bool; MAX_ROOMS],
    shown: [u8; MAX_ROOMS],
    last: CardCounters,
    frames: u32,
}

static mut TRACE: Trace = Trace {
    drawn: [false; MAX_ROOMS],
    in_band: [false; MAX_ROOMS],
    waiting: [false; MAX_ROOMS],
    blank: [false; MAX_ROOMS],
    shown: [NO_VIEW; MAX_ROOMS],
    last: CardCounters {
        submitted: 0,
        landed: 0,
        failed: 0,
        uploads: 0,
        evictions: 0,
        refused: 0,
        live: 0,
        starved: 0,
        merged: 0,
        backlog: 0,
        blank: 0,
        last_first: 0,
        last_sectors: 0,
    },
    frames: 0,
};

/// A debug line under construction.
struct Line {
    bytes: [u8; 96],
    len: usize,
}

impl Line {
    fn new(tag: &str) -> Self {
        let mut line = Self {
            bytes: [0; 96],
            len: 0,
        };
        line.text(tag);
        line
    }

    fn text(&mut self, text: &str) {
        for &byte in text.as_bytes() {
            if self.len < self.bytes.len() {
                self.bytes[self.len] = byte;
                self.len += 1;
            }
        }
    }

    fn number(&mut self, value: u32) {
        self.text(" ");
        let mut digits = [0u8; 10];
        let (mut rest, mut count) = (value, 0);
        loop {
            digits[count] = b'0' + (rest % 10) as u8;
            count += 1;
            rest /= 10;
            if rest == 0 {
                break;
            }
        }
        while count > 0 {
            count -= 1;
            if self.len < self.bytes.len() {
                self.bytes[self.len] = digits[count];
                self.len += 1;
            }
        }
    }

    fn emit(&self) {
        telemetry::debug_line(&self.bytes[..self.len]);
    }
}

/// Forget the rooms whose card was drawn: call before the card draw.
pub(super) fn begin_frame() {
    // SAFETY: single-threaded guest; the trace is touched only here, in [`mark`],
    // [`counters`] and [`frame`].
    let trace = unsafe { &mut *core::ptr::addr_of_mut!(TRACE) };
    trace.drawn = [false; MAX_ROOMS];
}

/// Note that the card of `room` was drawn this frame.
pub(super) fn mark(room: u16) {
    // SAFETY: see [`begin_frame`].
    let trace = unsafe { &mut *core::ptr::addr_of_mut!(TRACE) };
    if let Some(drawn) = trace.drawn.get_mut(usize::from(room)) {
        *drawn = true;
    }
}

/// Report the stream's counters when they changed since the last tick.
pub(super) fn counters(now: CardCounters) {
    // SAFETY: single-threaded guest; the trace is touched only here and in
    // [`frame`].
    let trace = unsafe { &mut *core::ptr::addr_of_mut!(TRACE) };
    if now == trace.last {
        return;
    }
    trace.last = now;
    let mut line = Line::new("ct R");
    for value in [
        now.submitted,
        now.landed,
        now.failed,
        now.uploads,
        now.live,
        now.starved,
        now.refused,
        now.evictions,
        now.merged,
        now.backlog,
        now.blank,
        now.last_first,
        now.last_sectors,
    ] {
        line.number(value);
    }
    line.emit();
}

/// What the camera sees of the cards and how the pictures stand, once per
/// drawn frame.
pub(super) struct View<'a> {
    pub table: &'a CardTable<'a>,
    pub live: &'a [LiveSlot; SLOTS],
    pub eye: [i32; 3],
    /// The view rotation (Q12 rows), for `ct Q`.
    pub rotation: [[i16; 3]; 3],
    pub right: [i32; 2],
    pub forward: [i32; 2],
    pub half_tan_q12: i32,
    pub far: i32,
    pub hide_all: i32,
    /// Rooms nearer than this are drawn whole and their cards are not counted late.
    pub hold_near: i32,
    /// Per room, whether the frame drew it whole.
    pub skip: &'a [bool],
    /// Rooms whose centre is within this plan distance are held to be covered.
    pub cover_to: i32,
    /// Screen row of the horizon at the middle of the view.
    pub horizon: Option<i32>,
    /// Cards the draw put in the frame and slots it looked at.
    pub drawn: u16,
    pub looked: u16,
}

pub(super) fn frame(view: &View<'_>) {
    // SAFETY: see [`counters`].
    let trace = unsafe { &mut *core::ptr::addr_of_mut!(TRACE) };
    trace.shown = [NO_VIEW; MAX_ROOMS];
    for entry in view.live {
        // A view fading in over another counts once it has taken over.
        if entry.view == NO_VIEW || entry.fade {
            continue;
        }
        if let Some(shown) = trace.shown.get_mut(usize::from(entry.room)) {
            *shown = entry.view;
        }
    }
    let [ex, _, ez] = view.eye;
    trace.frames = trace.frames.wrapping_add(1);
    // Every frame: the camera, then the cards drawn with the view each shows.
    let mut line = Line::new("ct Q");
    for value in view.eye {
        line.number((value + 32768) as u32);
    }
    for row in view.rotation {
        for value in row {
            line.number((i32::from(value) + 32768) as u32);
        }
    }
    line.emit();
    for entry in view.live {
        if entry.view != NO_VIEW && trace.drawn.get(usize::from(entry.room)).copied() == Some(true)
        {
            let mut line = Line::new("ct W");
            line.number(u32::from(entry.room));
            line.number(u32::from(entry.view));
            line.number(u32::from(entry.fade));
            line.emit();
        }
    }
    if trace.frames % 30 == 0 {
        let mut line = Line::new("ct P");
        line.number((ex + 32768) as u32);
        line.number((ez + 32768) as u32);
        line.emit();
        let mut line = Line::new("ct D");
        line.number(u32::from(view.drawn));
        line.number(u32::from(view.looked));
        line.number(view.live.iter().filter(|s| s.fade).count() as u32);
        line.emit();
        // Slots held by each layer of cards, and how many of them show a picture.
        let mut held = [0u32; 3];
        let mut shown = [0u32; 3];
        for slot in view.live {
            if let Some(card) = view.table.card(usize::from(slot.room)) {
                let layer = usize::from(card.layer).min(2);
                held[layer] += 1;
                shown[layer] += u32::from(slot.view != NO_VIEW);
            }
        }
        let mut line = Line::new("ct S");
        for value in held.into_iter().chain(shown) {
            line.number(value);
        }
        line.emit();
    }
    let (mut band_missing, mut band_stale, mut seen_missing, mut seen_stale, mut seen_bad) =
        (0u32, 0, 0, 0, 0);
    for room in 0..view.table.len().min(MAX_ROOMS) {
        let Some(card) = view.table.card(room) else {
            continue;
        };
        // Each layer of cards has its own band.
        let (near, far) = match usize::from(card.layer).checked_sub(1) {
            None => (view.hide_all.max(view.hold_near), view.far),
            Some(k) => {
                let band =
                    crate::vram_runtime::card_bands()[k.min(psx_grid::render::OUTER_LAYERS - 1)];
                (band.near, band.far)
            }
        };
        let (dx, dz) = (ex - card.centre[0], ez - card.centre[1]);
        let length = if dx.abs() > 8000 || dz.abs() > 8000 {
            i32::MAX
        } else {
            isqrt_i32(dx * dx + dz * dz).max(1)
        };
        let in_band = length >= near && length <= far;
        if !in_band {
            trace.in_band[room] = false;
            trace.waiting[room] = false;
            trace.blank[room] = false;
            continue;
        }
        if !trace.in_band[room] {
            trace.in_band[room] = true;
            trace.waiting[room] = true;
            trace.blank[room] = true;
            let mut line = Line::new("ct E");
            line.number(room as u32);
            line.number(length as u32);
            line.emit();
        }
        let shown = trace.shown[room];
        if shown != NO_VIEW && trace.blank[room] {
            trace.blank[room] = false;
            let mut line = Line::new("ct A");
            line.number(room as u32);
            line.number(length as u32);
            line.emit();
        }
        let serves = shown != NO_VIEW && (view_keep(shown, dx, dz) || shown == view_toward(dx, dz));
        if serves && trace.waiting[room] {
            trace.waiting[room] = false;
            let mut line = Line::new("ct K");
            line.number(room as u32);
            line.number(length as u32);
            line.emit();
        }
        let ahead = (view.forward[0] * -dx + view.forward[1] * -dz) >> 12;
        let across = (view.right[0] * -dx + view.right[1] * -dz) >> 12;
        let seen = ahead >= -card.half_width
            && across.abs() <= ((ahead.max(0) * view.half_tan_q12) >> 12) + card.half_width;
        if !serves {
            if shown == NO_VIEW {
                band_missing += 1;
                seen_missing += u32::from(seen);
            } else {
                band_stale += 1;
                seen_stale += u32::from(seen);
                // Two or more steps of 45 degrees off the side the eye is on.
                let step = (shown + 8 - view_toward(dx, dz)) % 8;
                seen_bad += u32::from(seen && step.min(8 - step) >= 2);
            }
        }
    }
    let mut line = Line::new("ct H");
    line.number(view.horizon.map_or(9999, |y| (y + 4096).max(0) as u32));
    line.number((view.eye[1] + 32768).max(0) as u32);
    line.emit();
    // Coverage: every room in the view cone within the drawn radius is drawn
    // whole or by its card.
    let (mut gaps, mut checked, mut nearest, mut nearest_room) = (0u32, 0u32, i32::MAX, 0u32);
    for room in 0..view.table.len().min(MAX_ROOMS) {
        let Some(card) = view.table.card(room).filter(|card| card.layer == 0) else {
            continue;
        };
        let (dx, dz) = (ex - card.centre[0], ez - card.centre[1]);
        if dx.abs() > view.cover_to || dz.abs() > view.cover_to {
            continue;
        }
        let length = isqrt_i32(dx * dx + dz * dz).max(1);
        if length > view.cover_to {
            continue;
        }
        let ahead = (view.forward[0] * -dx + view.forward[1] * -dz) >> 12;
        let across = (view.right[0] * -dx + view.right[1] * -dz) >> 12;
        let seen = ahead >= -card.half_width
            && across.abs() <= ((ahead.max(0) * view.half_tan_q12) >> 12) + card.half_width;
        if !seen {
            continue;
        }
        checked += 1;
        if !(view.skip.get(room).copied().unwrap_or(false) || trace.drawn[room]) {
            gaps += 1;
            if length < nearest {
                nearest = length;
                nearest_room = room as u32;
            }
        }
    }
    if gaps > 0 {
        let mut line = Line::new("ct G");
        for value in [gaps, checked, nearest as u32, nearest_room] {
            line.number(value);
        }
        line.emit();
    }
    if band_missing + band_stale > 0 {
        let mut line = Line::new("ct F");
        for value in [band_missing, band_stale, seen_missing, seen_stale, seen_bad] {
            line.number(value);
        }
        line.emit();
    }
}

/// The camera of every frame the panels are drawn for, `pt Q`: the eye and the nine words of
/// the view rotation, 32768 added to each.
pub(super) fn panel_pose(eye: [i32; 3], rotation: &[[i16; 3]; 3], eye_index: u32) {
    let mut line = Line::new("pt Q");
    for value in eye {
        line.number((value + 32768) as u32);
    }
    for row in rotation {
        for &value in row {
            line.number((i32::from(value) + 32768) as u32);
        }
    }
    line.number(eye_index);
    line.emit();
}

/// One panel drawn: `pt D packets tiles_looked_at`.
pub(super) fn panel_frame(frame: &psx_grid::render::GridFrame, heading: u8, eye: u16) {
    let mut line = Line::new("pt D");
    line.number(frame.packets);
    line.number(u32::from(frame.cells_visible));
    line.number(u32::from(frame.quads_visible));
    line.number(u32::from(frame.depth_skipped));
    line.number(u32::from(frame.screen_culled));
    line.number(u32::from(heading));
    line.number(u32::from(eye));
    line.emit();
}

/// Report the panel stream's counters when they changed.
pub(super) fn panel_counters(now: psx_game_runtime::panel_stream::PanelCounters) {
    // SAFETY: single-threaded guest; the trace is touched only here and in the card trace.
    let trace = unsafe { &mut *core::ptr::addr_of_mut!(PANEL_LAST) };
    if now == *trace {
        return;
    }
    *trace = now;
    let mut line = Line::new("pt R");
    for value in [
        now.submitted,
        now.landed,
        now.failed,
        now.uploads,
        now.evictions,
        now.refused,
        now.held,
        now.wanted,
        now.missing,
        now.starved,
        now.eye,
        now.last_first,
        now.last_sectors,
        now.sectors,
    ] {
        line.number(value);
    }
    line.emit();
}

static mut PANEL_LAST: psx_game_runtime::panel_stream::PanelCounters =
    psx_game_runtime::panel_stream::PanelCounters {
        submitted: 0,
        landed: 0,
        failed: 0,
        uploads: 0,
        evictions: 0,
        refused: 0,
        held: 0,
        wanted: 0,
        missing: 0,
        starved: 0,
        eye: 0,
        last_first: 0,
        last_sectors: 0,
        sectors: 0,
    };

/// What the panel stream was told about the camera, `pt I`, when the heading it looks along
/// changes: the forward direction in plan (Q12, 32768 added), the pitch's `tan` (32768 added)
/// and the eye.
pub(super) fn panel_input(forward: [i32; 2], pitch_tan: i32, eye: [i32; 3]) {
    static mut LAST: [i32; 2] = [i32::MAX; 2];
    // SAFETY: single-threaded guest; the trace is touched only here.
    let last = unsafe { &mut *core::ptr::addr_of_mut!(LAST) };
    if (forward[0] - last[0]).abs() < 256 && (forward[1] - last[1]).abs() < 256 {
        return;
    }
    *last = forward;
    let mut line = Line::new("pt I");
    for value in [forward[0], forward[1], pitch_tan, eye[0], eye[1], eye[2]] {
        line.number((value + 32768) as u32);
    }
    line.emit();
}
