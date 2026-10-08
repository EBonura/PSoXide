// SPDX-License-Identifier: GPL-2.0-or-later
//! The interrupt-driven pad engine (`psx_pad::console`) on a console: the
//! questions its design could not answer without one.
//!
//! The engine polls from the VBlank interrupt and advances on each byte's
//! `/ACK`. Its defaults are the BIOS-like pacing (`Ack`), which writes the
//! control register once between bytes, and a 7,000-cycle select-to-first-byte
//! setup. The history of the SCPH-1200 and a clone pad says either could be
//! wrong on some pad. These steps measure, they do not judge:
//!
//! * `setup`: the engine's own select-to-first-byte path swept from 2,000 to
//!   8,000 cycles, clean polls counted;
//! * `ack` and `timed`: 600 frames of each pacing with a pad in port 1 and
//!   port 2 as it is, counting updates, faults, identifier changes, the
//!   engine's events and the CPU it leaves a loop;
//! * hot-plug: a prompt, then the frames until a port reads present or absent;
//! * card: a memory-card sector read every ten frames through `lease()`
//!   beside the engine (reads only, never writes);
//! * load: the same engine with a GPU walk, SPU DMA and a CD read going, the
//!   handler's stack use and any stall or spurious interrupt.
//!
//! Work is iterations of an empty loop between VBlanks divided by 16, as in
//! the SDK's `pad-engine-check`: more is more CPU left for a game.

use crate::console_tests::record;
use crate::lever_probes::Activity;
use crate::{push_timing_record, TimingRecord, TIMING_RECORD_COUNT};
use psx_io::controller_port::Port;
use psx_io::periph::ControllerPort;
use psx_io::timers::{self, Timer};
use psx_mc::{Block, Error as CardError, HardwareCard};
use psx_pad::console;
use psx_pad::engine::{BytePacing, Config, Health, PortReading};
use psx_rt::interrupts::vblank_count;

type Records = [TimingRecord; TIMING_RECORD_COUNT];

/// rec engine_setup: clean_updates_port1, faults_port1, health_port1_and_port2 (setup 2000 to 8000 cycles in steps of 1000, 0x660-0x666)
const SETUP_RECORD: u16 = 0x660;
/// rec engine_port: updates, faults, health_and_last_fault (port 1 then port 2, Ack from 0x670, Timed from 0x678)
const PORT_RECORD: u16 = 0x670;
/// rec engine_stats: events, stalls, spurious (Ack 0x672, Timed 0x67A)
const STATS_RECORD: u16 = 0x672;
/// rec engine_work: work_avg, work_min, work_idle_avg (Ack 0x673, Timed 0x67B)
const WORK_RECORD: u16 = 0x673;
/// rec engine_pad: mode_changes, kicks, final_mode_and_buttons_seen (Ack 0x674, Timed 0x67C)
const PAD_RECORD: u16 = 0x674;
/// rec engine_hotplug: transitions, initial_health_and_final_health, frames_absent (port 1 and 2, 0x638-0x639)
const HOTPLUG_RECORD: u16 = 0x638;
/// rec engine_hotplug_frames: first_event_frame, last_event_frame, frames_watched (port 1 and 2, 0x63A-0x63B)
const HOTPLUG_FRAMES_RECORD: u16 = 0x63A;
/// rec engine_card_pad: pad_faults, leased_skips, card_checksum_errors (0x690)
const CARD_PAD_RECORD: u16 = 0x690;
/// rec engine_card_ops: card_ok, card_tried, slot_with_card (0 none, 0x691)
const CARD_OPS_RECORD: u16 = 0x691;
/// rec engine_card_wait: lease_wait_med_cycles, lease_wait_max_cycles, pad_updates (0x692)
const CARD_WAIT_RECORD: u16 = 0x692;
/// rec engine_card_frame: card_frame_hblanks_min, card_frame_hblanks_med, card_frame_hblanks_max (0x693)
const CARD_FRAME_RECORD: u16 = 0x693;
/// rec engine_load_work: rounds_avg, rounds_min, rounds_max (a frame's loop rounds with the load running, engine with no ports 0x6A0, with both 0x6A4)
const LOAD_WORK_RECORD: u16 = 0x6A0;
/// rec engine_load_health: pad_faults, stalls, spurious (0x6A1, 0x6A5)
const LOAD_HEALTH_RECORD: u16 = 0x6A1;
/// rec engine_load_stack: handler_stack_unused_bytes, events, kicks (0x6A2, 0x6A6)
const LOAD_STACK_RECORD: u16 = 0x6A2;

const NONE: u32 = 0xFFFF;
/// Give a frame up after this many loop rounds if no VBlank arrives.
const SPIN_CAP: u32 = 100_000_000;
const PACING_FRAMES: u32 = 600;
const SETUP_FRAMES: u32 = 100;
const LOAD_FRAMES: u32 = 120;
const CARD_FRAMES: u32 = 600;
/// A card read every this many frames.
const CARD_EVERY: u32 = 10;
/// Frames the hot-plug window stays open.
const HOTPLUG_FRAMES: u32 = 360;

fn token() -> ControllerPort {
    // SAFETY: a token is a logic guard; nothing else drives SIO0 during these
    // steps, and the engine is handed it for the length of a session.
    unsafe { ControllerPort::steal() }
}

/// The engine installed for the length of a measurement, the port back after.
struct Session {
    installed: bool,
}

impl Session {
    fn start(config: Config) -> Self {
        let installed = console::install(token(), config).is_ok();
        Self { installed }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if !self.installed {
            return;
        }
        // A transaction in flight finishes within a frame or two.
        for _ in 0..8 {
            if console::uninstall().is_some() {
                // The wrapper stays in the vector after `uninstall`, and
                // `install` refuses a vector that already leads to it, so put
                // the runtime's handler back for the next session.
                psx_rt::interrupts::install_vblank_counter();
                return;
            }
            let _ = spin_frame(|| {});
        }
    }
}

/// Wait for the next VBlank, doing `work` in a loop meanwhile; returns the
/// number of rounds.
fn spin_frame(mut work: impl FnMut()) -> u32 {
    let start = vblank_count();
    let mut rounds = 0u32;
    while vblank_count() == start {
        work();
        rounds = rounds.wrapping_add(1);
        if rounds > SPIN_CAP {
            break;
        }
    }
    rounds
}

fn health_code(reading: &PortReading) -> u32 {
    reading.health as u32
}

fn fault_code(reading: &PortReading) -> u32 {
    match reading.last_fault {
        None => 0,
        Some(fault) => 1 + fault as u32,
    }
}

fn push(records: &mut Records, next: &mut usize, row: TimingRecord) {
    push_timing_record(records, next, row);
}

/// Rows for a session that could not install (a handler in the vector that
/// cannot be chained): an all-ones marker in the first one.
fn refused(records: &mut Records, next: &mut usize, id: u16) {
    push(records, next, record(id, NONE, NONE, NONE));
}

// ------------------------------------------------------------ setup sweep

pub(crate) fn setup_sweep(records: &mut Records, next: &mut usize) {
    for index in 0..7u32 {
        let id = SETUP_RECORD + index as u16;
        let config = Config {
            setup_cycles: 2_000 + 1_000 * index,
            ..Config::DEFAULT
        };
        let session = Session::start(config);
        if !session.installed {
            refused(records, next, id);
            continue;
        }
        // The first frames settle the pad; count from the snapshot after.
        for _ in 0..4 {
            let _ = spin_frame(|| {});
        }
        let before = console::snapshot();
        for _ in 0..SETUP_FRAMES {
            let _ = spin_frame(|| {});
        }
        let after = console::snapshot();
        let (one, two) = (after.port(Port::One), after.port(Port::Two));
        push(
            records,
            next,
            record(
                id,
                one.updates.wrapping_sub(before.port(Port::One).updates),
                one.faults.wrapping_sub(before.port(Port::One).faults),
                (health_code(one) << 8) | health_code(two),
            ),
        );
    }
}

// ----------------------------------------------------------- pacing, 600

fn pacing_run(pacing: BytePacing, records: &mut Records, next: &mut usize) {
    let base = match pacing {
        BytePacing::Ack => 0u16,
        _ => 8,
    };
    // The loop with nothing polling, for the comparison.
    let mut idle_total = 0u32;
    let _ = spin_frame(|| {});
    for _ in 0..60 {
        idle_total += spin_frame(|| {}) / 16;
    }
    let idle = idle_total / 60;

    let config = Config {
        pacing,
        ..Config::DEFAULT
    };
    let session = Session::start(config);
    if !session.installed {
        refused(records, next, PORT_RECORD + base);
        return;
    }
    // Ask for analog mode as a game does; the pad settles within a few frames.
    console::request_analog(Port::One);
    for _ in 0..12 {
        let _ = spin_frame(|| {});
    }
    let before = console::snapshot();
    let stats_before = console::stats();
    let mut work_total = 0u32;
    let mut work_min = u32::MAX;
    let mut last_mode = before.port(Port::One).pad.mode as u32;
    let mut mode_changes = 0u32;
    for _ in 0..PACING_FRAMES {
        let work = spin_frame(|| {}) / 16;
        work_total += work;
        work_min = work_min.min(work);
        let mode = console::snapshot().port(Port::One).pad.mode as u32;
        if mode != last_mode {
            mode_changes += 1;
            last_mode = mode;
        }
    }
    let after = console::snapshot();
    let stats = console::stats();
    let rows = [(0u16, Port::One), (1u16, Port::Two)];
    for (offset, port) in rows {
        let (a, b) = (before.port(port), after.port(port));
        push(
            records,
            next,
            record(
                PORT_RECORD + base + offset,
                b.updates.wrapping_sub(a.updates),
                b.faults.wrapping_sub(a.faults),
                (health_code(b) << 8) | fault_code(b),
            ),
        );
    }
    push(
        records,
        next,
        record(
            STATS_RECORD + base,
            stats.events.wrapping_sub(stats_before.events),
            stats.stalls.wrapping_sub(stats_before.stalls),
            stats.spurious.wrapping_sub(stats_before.spurious),
        ),
    );
    push(
        records,
        next,
        record(
            WORK_RECORD + base,
            work_total / PACING_FRAMES,
            work_min,
            idle,
        ),
    );
    let pad = after.port(Port::One).pad;
    push(
        records,
        next,
        record(
            PAD_RECORD + base,
            mode_changes,
            stats.kicks.wrapping_sub(stats_before.kicks),
            ((pad.mode as u32) << 8) | (pad.buttons.bits() as u32 & 0xFF),
        ),
    );
}

pub(crate) fn pacing_ack(records: &mut Records, next: &mut usize) {
    pacing_run(BytePacing::Ack, records, next);
}

pub(crate) fn pacing_timed(records: &mut Records, next: &mut usize) {
    pacing_run(BytePacing::Timed, records, next);
}

// ---------------------------------------------------------------- hot-plug

/// Both ports watched through the engine for [`HOTPLUG_FRAMES`]; every change
/// of what a port reads is logged. `progress` gets the seconds left once a
/// second. Nothing happening is the usual result.
pub(crate) fn hotplug(mut progress: impl FnMut(u32), records: &mut Records, next: &mut usize) {
    let session = Session::start(Config::DEFAULT);
    if !session.installed {
        refused(records, next, HOTPLUG_RECORD);
        return;
    }
    for _ in 0..6 {
        let _ = spin_frame(|| {});
    }
    let snapshot = console::snapshot();
    let mut state = [
        health_code(snapshot.port(Port::One)),
        health_code(snapshot.port(Port::Two)),
    ];
    let initial = state;
    let mut transitions = [0u32; 2];
    let mut first = [NONE; 2];
    let mut last = [NONE; 2];
    let mut absent = [0u32; 2];
    progress(HOTPLUG_FRAMES / 60);
    for frame in 1..=HOTPLUG_FRAMES {
        let _ = spin_frame(|| {});
        if frame % 60 == 0 {
            progress((HOTPLUG_FRAMES - frame) / 60);
        }
        let snapshot = console::snapshot();
        for (index, port) in [Port::One, Port::Two].into_iter().enumerate() {
            let now = health_code(snapshot.port(port));
            if snapshot.port(port).health == Health::Absent {
                absent[index] += 1;
            }
            if now != state[index] {
                transitions[index] += 1;
                if first[index] == NONE {
                    first[index] = frame;
                }
                last[index] = frame;
                state[index] = now;
            }
        }
    }
    for index in 0..2 {
        push(
            records,
            next,
            record(
                HOTPLUG_RECORD + index as u16,
                transitions[index],
                (initial[index] << 8) | state[index],
                absent[index],
            ),
        );
    }
    for index in 0..2 {
        push(
            records,
            next,
            record(
                HOTPLUG_FRAMES_RECORD + index as u16,
                first[index],
                last[index],
                HOTPLUG_FRAMES,
            ),
        );
    }
}

// ----------------------------------------------------------- card, leased

/// A card read through the engine's lease, beside the engine polling pads.
/// Only the first slot that holds a card is used. Reads only.
pub(crate) fn card_lease(records: &mut Records, next: &mut usize) {
    let session = Session::start(Config::DEFAULT);
    if !session.installed {
        refused(records, next, CARD_PAD_RECORD);
        return;
    }
    for _ in 0..6 {
        let _ = spin_frame(|| {});
    }
    let mut slot = None;
    for candidate in [Port::One, Port::Two] {
        let mut lease = console::lease();
        let mut card = HardwareCard::on_port(&mut *lease, candidate);
        let mut buf = [0u8; 128];
        if !matches!(card.read_frame(0, &mut buf), Err(CardError::NoCard)) {
            slot = Some(candidate);
            break;
        }
    }
    let Some(slot) = slot else {
        push(records, next, record(CARD_PAD_RECORD, 0, 0, NONE));
        push(records, next, record(CARD_OPS_RECORD, 0, 0, 0));
        push(records, next, record(CARD_WAIT_RECORD, NONE, NONE, NONE));
        push(records, next, record(CARD_FRAME_RECORD, NONE, NONE, NONE));
        return;
    };
    let before = console::snapshot();
    let skips_before = console::stats().leased_skips;
    timers::set_mode(Timer::Timer1, 0x0100);
    let mut waits = [0u32; (CARD_FRAMES / CARD_EVERY) as usize];
    let mut frames = [0u32; (CARD_FRAMES / CARD_EVERY) as usize];
    let (mut ok, mut tried, mut checksum) = (0u32, 0u32, 0u32);
    let mut buf = [0u8; 128];
    for frame in 0..CARD_FRAMES {
        let _ = spin_frame(|| {});
        if frame % CARD_EVERY != 0 {
            continue;
        }
        let index = (frame / CARD_EVERY) as usize;
        timers::set_mode(Timer::Timer2, 0);
        timers::set_counter(Timer::Timer2, 0);
        let mut lease = console::lease();
        waits[index] = timers::counter(Timer::Timer2) as u32;
        let mut card = HardwareCard::on_port(&mut *lease, slot);
        timers::set_counter(Timer::Timer1, 0);
        let result = card.read_frame(((index as u16) * 5) % 64, &mut buf);
        frames[index] = timers::counter(Timer::Timer1) as u32;
        tried += 1;
        match result {
            Ok(()) => ok += 1,
            Err(CardError::BadChecksum) => checksum += 1,
            Err(_) => {}
        }
    }
    let after = console::snapshot();
    let skips = console::stats().leased_skips.wrapping_sub(skips_before);
    let (_, wait_med, wait_max) = crate::console_tests::spread(&mut waits);
    let (fmin, fmed, fmax) = crate::console_tests::spread(&mut frames);
    let (a, b) = (before.port(slot), after.port(slot));
    push(
        records,
        next,
        record(
            CARD_PAD_RECORD,
            b.faults.wrapping_sub(a.faults),
            skips,
            checksum,
        ),
    );
    push(
        records,
        next,
        record(
            CARD_OPS_RECORD,
            ok,
            tried,
            if slot == Port::One { 1 } else { 2 },
        ),
    );
    push(
        records,
        next,
        record(
            CARD_WAIT_RECORD,
            wait_med,
            wait_max,
            b.updates.wrapping_sub(a.updates),
        ),
    );
    push(records, next, record(CARD_FRAME_RECORD, fmin, fmed, fmax));
}

// ------------------------------------------------------------ under load

pub(crate) fn under_load(records: &mut Records, next: &mut usize) {
    for phase in 0..2u16 {
        let config = if phase == 0 {
            Config {
                ports: [false, false],
                ..Config::DEFAULT
            }
        } else {
            Config::DEFAULT
        };
        let base = 4 * phase;
        let session = Session::start(config);
        if !session.installed {
            refused(records, next, LOAD_WORK_RECORD + base);
            continue;
        }
        let mut load = Activity::start_without_pad();
        let _ = spin_frame(|| load.service());
        let before = console::snapshot();
        let stats_before = console::stats();
        let (mut total, mut least, mut most) = (0u32, u32::MAX, 0u32);
        for _ in 0..LOAD_FRAMES {
            let rounds = spin_frame(|| load.service());
            total += rounds;
            least = least.min(rounds);
            most = most.max(rounds);
        }
        let after = console::snapshot();
        let stats = console::stats();
        let _ = load.stop();
        push(
            records,
            next,
            record(LOAD_WORK_RECORD + base, total / LOAD_FRAMES, least, most),
        );
        push(
            records,
            next,
            record(
                LOAD_HEALTH_RECORD + base,
                after
                    .port(Port::One)
                    .faults
                    .wrapping_sub(before.port(Port::One).faults),
                stats.stalls.wrapping_sub(stats_before.stalls),
                stats.spurious.wrapping_sub(stats_before.spurious),
            ),
        );
        push(
            records,
            next,
            record(
                LOAD_STACK_RECORD + base,
                console::handler_stack_unused_bytes() as u32,
                stats.events.wrapping_sub(stats_before.events),
                stats.kicks.wrapping_sub(stats_before.kicks),
            ),
        );
    }
}
