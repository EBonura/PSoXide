//! The measurement sequence. Each phase fills a report page and appends its
//! numbers to the payload.

use crate::clock;
use crate::drive::{self, *};
use crate::report::{head, report, Text};
use crate::screen::Screen;
use core::hint::black_box;
use psx_cdstream::{Outcome, RequestState, Ticket};
use psx_hw::cd::{CMD_PAUSE, CMD_PLAY, CMD_SETLOC, CMD_STOP, MODE_AUTO_PAUSE, MODE_CDDA};
use psx_io::cd::{bin_to_bcd, PlayPosition};
use psx_io::periph::Cd;

pub const DISTANCES: [u32; 6] = [1, 16, 128, 512, 2048, 8192];
const REPS: usize = 8;
/// File sector the seek table starts from.
const BASE: u32 = 64;
/// Disc track the tone is on (the first audio track).
const TONE_TRACK: u8 = 2;
const CDDA_MODE: u8 = MODE_CDDA | MODE_AUTO_PAUSE;
const SECOND: u32 = clock::HZ;

fn status(screen: &mut Screen, title: &str, a: &Text, b: &Text) {
    screen.status(title, &[a.as_str(), b.as_str()]);
}

// ----------------------------------------------------------- 1. seek table

/// Seek and read table: one sector at a growing distance from the previous
/// read, forward and back, timed from submit to first sector with the
/// production transport.
pub fn seek_table(screen: &mut Screen) {
    let mut bad = 0u32;
    let mut forward = [[0u32; 4]; 6];
    let mut back = [[0u32; 4]; 6];
    for (i, &distance) in DISTANCES.iter().enumerate() {
        let mut fwd = [0u32; REPS];
        let mut fwd_idle = [0u32; REPS];
        let mut bwd = [0u32; REPS];
        let mut bwd_idle = [0u32; REPS];
        let (mut nf, mut nb) = (0, 0);
        let _ = read_timed(BASE, 1, false);
        for rep in 0..REPS {
            let mut a = Text::new();
            a.s("DISTANCE ").u(distance);
            let mut b = Text::new();
            b.s("REPEAT ").u(rep as u32 + 1).s(" OF ").u(REPS as u32);
            status(screen, "1 SEEK TABLE", &a, &b);

            let f = read_timed(BASE + 1 + distance, 1, true);
            if f.ok {
                fwd[nf] = f.first;
                fwd_idle[nf] = f.idle;
                nf += 1;
            } else {
                bad += 1;
            }
            let g = read_timed(BASE, 1, true);
            if g.ok {
                bwd[nb] = g.first;
                bwd_idle[nb] = g.idle;
                nb += 1;
            } else {
                bad += 1;
            }
        }
        let f = spread(&mut fwd[..nf]);
        let fi = spread(&mut fwd_idle[..nf]);
        let b = spread(&mut bwd[..nb]);
        let bi = spread(&mut bwd_idle[..nb]);
        forward[i] = [f[0], f[1], f[2], fi[1]];
        head().seek_forward_ms[i] = clock::ms(f[1]);
        back[i] = [b[0], b[1], b[2], bi[1]];
    }
    let r = report();
    let mut title = Text::new();
    title.s("1 SEEK MS MIN/MED/MAX BAD ").u(bad);
    r.page(title.as_str(), bad == 0);
    for (i, &distance) in DISTANCES.iter().enumerate() {
        for (tag, row) in [("F", forward[i]), ("B", back[i])] {
            let mut t = Text::new();
            t.pad_u(distance, 4)
                .s(tag)
                .s(" ")
                .u(clock::ms(row[0]))
                .s("/")
                .u(clock::ms(row[1]))
                .s("/")
                .u(clock::ms(row[2]));
            r.line(&t);
            let mut key = Text::new();
            key.s(tag).u(distance);
            r.kv(key.as_str(), &row);
        }
    }
    r.kv("ER", &[bad]);
}

// ------------------------------------------------- 2. rate and CPU cost

/// A sustained stream: `total` sectors in 32-sector requests through four
/// buffers, the queue kept topped up. Returns (HBlanks, chained, dropped, ok).
fn stream(total: u32) -> (u32, u32, u32, bool) {
    const PER: u32 = RING_SECTORS as u32;
    let before = psx_cdstream::stats();
    let mut tickets: [Option<Ticket>; 4] = [None; 4];
    let mut next = 0u32;
    let start = clock::now();
    for slot in 0..4 {
        tickets[slot] = submit_file(next, PER, ring(slot));
        next += PER;
    }
    let mut verified = 0u32;
    let mut all_ok = true;
    let mut slot = 0;
    while verified < total {
        let Some(done) = tickets[slot].and_then(wait) else {
            all_ok = false;
            break;
        };
        all_ok &= done.outcome == Outcome::Done && verify(ring(slot), verified, PER, 16);
        verified += PER;
        if next < total {
            tickets[slot] = submit_file(next, PER, ring(slot));
            next += PER;
        } else {
            tickets[slot] = None;
        }
        slot = (slot + 1) % 4;
    }
    let hblanks = clock::since(start);
    wait_idle();
    let after = psx_cdstream::stats();
    (
        hblanks,
        after.chained.wrapping_sub(before.chained),
        after
            .discarded_sectors
            .wrapping_sub(before.discarded_sectors),
        all_ok,
    )
}

/// What the foreground got done in a window: (iterations, HBlanks).
struct Window {
    iterations: u32,
    hblanks: u32,
}

/// Spin on a fixed loop for `length` HBlanks (idle baseline) or until
/// `ticket` finishes (busy). Both call `state` at the same rate so the two
/// carry the same upkeep.
fn spin(ticket: Ticket, length: Option<u32>) -> Window {
    let start = clock::now();
    let mut iterations = 0u32;
    let mut acc = 0u32;
    loop {
        for k in 0..64u32 {
            acc = black_box(acc.wrapping_add(k));
        }
        iterations += 1;
        let elapsed = clock::since(start);
        if iterations % 32 == 0 {
            let state = psx_cdstream::state(ticket);
            if length.is_none() && matches!(state, RequestState::Finished(_)) {
                break;
            }
        }
        match length {
            Some(length) if elapsed >= length => break,
            None if elapsed > PATIENCE_HB => break,
            _ => {}
        }
    }
    black_box(acc);
    Window {
        iterations,
        hblanks: clock::since(start),
    }
}

/// One CPU-cost run: the foreground's loss to the sector pops of a
/// `sectors`-sector read. Returns [per-sector microseconds, lost permille,
/// handler peak us, IRQs per sector x100, raw idle iterations, idle HBlanks,
/// busy iterations, busy HBlanks].
fn cpu_run(ticket: Ticket, sectors: u32) -> [u32; 8] {
    wait_idle();
    let idle = spin(ticket, Some(SECOND));
    psx_cdstream::reset_max_irq_ticks();
    let before = psx_cdstream::stats();
    let Some(read) = submit_file(0, sectors, big()) else {
        return [0; 8];
    };
    let busy = spin(read, None);
    let after = psx_cdstream::stats();
    wait_idle();
    // Hblanks the busy loop's iterations would have taken with no read.
    let expected =
        (busy.iterations / 16).saturating_mul(idle.hblanks) / (idle.iterations / 16).max(1);
    let lost = busy.hblanks.saturating_sub(expected);
    let per_sector_us = lost.saturating_mul(6356) / 100 / sectors.max(1);
    let permille = lost.saturating_mul(1000) / busy.hblanks.max(1);
    let irqs = after.irq_count.wrapping_sub(before.irq_count) * 100 / sectors.max(1);
    [
        per_sector_us,
        permille,
        clock::timer2_us(after.max_irq_ticks),
        irqs,
        idle.iterations,
        idle.hblanks,
        busy.iterations,
        busy.hblanks,
    ]
}

/// Sustained rate and CPU cost at double and at single speed.
pub fn rate_and_cpu(screen: &mut Screen) {
    let mut sps = [0u32; 2];
    let mut chained = [0u32; 2];
    let mut dropped = [0u32; 2];
    let mut stream_ok = true;
    let mut cpu = [[0u32; 8]; 2];
    let mut cpu_spread = [[0u32; 3]; 2];
    let mut permille = [0u32; 2];
    let Some(ticket) = submit_file(0, 1, big()) else {
        return;
    };
    let _ = wait(ticket);
    for (slot, double) in [true, false].into_iter().enumerate() {
        psx_cdstream::configure(config(double, true));
        let name = if double { "2X" } else { "1X" };
        let mut a = Text::new();
        a.s(name).s(" SUSTAINED STREAM");
        let b = Text::new();
        status(screen, "2 RATE AND CPU", &a, &b);
        let total = if double { 640 } else { 160 };
        let (hblanks, chain, drop, ok) = stream(total);
        stream_ok &= ok;
        sps[slot] = total.saturating_mul(clock::HZ).saturating_mul(10) / hblanks.max(1);
        chained[slot] = chain;
        dropped[slot] = drop;
        let sectors = if double { 256 } else { 128 };
        let mut per_sector = [0u32; 3];
        let mut lost = [0u32; 3];
        for rep in 0..3 {
            let mut a = Text::new();
            a.s(name).s(" CPU COST RUN ").u(rep as u32 + 1);
            status(screen, "2 RATE AND CPU", &a, &b);
            let run = cpu_run(ticket, sectors);
            per_sector[rep] = run[0];
            lost[rep] = run[1];
            cpu[slot] = run;
        }
        cpu_spread[slot] = spread(&mut per_sector);
        permille[slot] = spread(&mut lost)[1];
    }
    psx_cdstream::configure(config(true, true));

    for slot in 0..2 {
        head().rate_x10[slot] = sps[slot];
        head().lost_permille[slot] = permille[slot];
        head().pio_us[slot] = cpu_spread[slot][1];
    }
    head().handler_us = cpu[0][2].max(cpu[1][2]);
    let r = report();
    r.page("2 RATE AND CPU", stream_ok);
    for (slot, name) in ["2X", "1X"].into_iter().enumerate() {
        let mut t = Text::new();
        t.s(name).s(" ").tenths(sps[slot]).s(" SEC/S");
        r.line(&t);
    }
    for (slot, name) in ["2X", "1X"].into_iter().enumerate() {
        let mut t = Text::new();
        t.s(name)
            .s(" PIO ")
            .u(cpu_spread[slot][1])
            .s("US LOST ")
            .tenths(permille[slot])
            .s("%");
        r.line(&t);
    }
    for (slot, name) in ["2X", "1X"].into_iter().enumerate() {
        let mut t = Text::new();
        t.s(name).s(" HANDLER MAX ").u(cpu[slot][2]).s("US");
        r.line(&t);
    }
    let mut t = Text::new();
    t.s("IRQ/SECTOR ")
        .u(cpu[0][3] / 100)
        .s(".")
        .u(cpu[0][3] % 100);
    r.line(&t);
    let mut t = Text::new();
    t.s("DROPPED ")
        .u(dropped[0] + dropped[1])
        .s(" CHAIN ")
        .u(chained[0]);
    r.line(&t);
    let mut t = Text::new();
    t.s("PIO US/SECTOR MIN/MAX");
    r.line(&t);
    for (slot, name) in ["2X", "1X"].into_iter().enumerate() {
        let mut t = Text::new();
        t.s(name)
            .s(" ")
            .u(cpu_spread[slot][0])
            .s("/")
            .u(cpu_spread[slot][2]);
        r.line(&t);
    }
    for (slot, name) in ["2", "1"].into_iter().enumerate() {
        let mut key = Text::new();
        key.s("SP").s(name);
        r.kv(key.as_str(), &[sps[slot], chained[slot], dropped[slot]]);
        let mut key = Text::new();
        key.s("C").s(name);
        r.kv(
            key.as_str(),
            &[
                cpu_spread[slot][0],
                cpu_spread[slot][1],
                cpu_spread[slot][2],
                permille[slot],
                cpu[slot][2],
                cpu[slot][3],
            ],
        );
        let mut key = Text::new();
        key.s("W").s(name);
        r.kv(key.as_str(), &cpu[slot][4..8]);
    }
}

// ---------------------------------------------------- 3. lease and motor

/// How long a lease request takes to stop a read in flight.
fn lease_latency(screen: &mut Screen) -> [u32; 3] {
    let mut samples = [0u32; 4];
    let mut n = 0;
    for rep in 0..4 {
        let mut a = Text::new();
        a.s("LEASE WHILE READING ").u(rep + 1);
        status(screen, "3 LEASE AND PAUSE", &a, &Text::new());
        wait_idle();
        let Some(ticket) = submit_file(300, 200, big()) else {
            continue;
        };
        let start = clock::now();
        let mut reached = false;
        while clock::since(start) < PATIENCE_HB {
            if matches!(psx_cdstream::state(ticket), RequestState::Active { received } if received >= 20)
            {
                reached = true;
                break;
            }
        }
        if !reached {
            continue;
        }
        let asked = clock::now();
        let _ = psx_cdstream::request_audio_lease();
        while psx_cdstream::lease_state() != psx_cdstream::LeaseState::Granted {
            if clock::since(asked) > PATIENCE_HB {
                break;
            }
        }
        let latency = clock::since(asked);
        samples[n] = latency;
        n += 1;
        if let Some(cd) = psx_cdstream::take_audio_lease() {
            release_audio(cd);
        }
        let _ = wait(ticket);
        wait_idle();
    }
    spread(&mut samples[..n])
}

/// Result of a read after the drive was stopped or left to idle.
fn describe(t: &Text, timed: &Timed) -> Text {
    let mut line = *t;
    if timed.ok {
        line.u(clock::ms(timed.done)).s("MS");
    } else {
        line.s("FAIL ").u(timed.code & 0xFFFF);
    }
    line
}

/// A failed read to start a [`Timed`] from.
const NO_READ: Timed = Timed {
    ok: false,
    code: CODE_TIMEOUT,
    first: 0,
    done: 0,
    idle: 0,
};

/// How fast a lease stops a read, and whether a Pause-parked motor winds down.
pub fn lease_and_pause(screen: &mut Screen) {
    let lease = lease_latency(screen);

    // Pause then wait, then a read: does the motor wind down on its own?
    let mut gaps = [NO_READ; 3];
    for (i, seconds) in [0u32, 5, 15].into_iter().enumerate() {
        let mut a = Text::new();
        a.s("PAUSE THEN WAIT ").u(seconds).s("S");
        status(screen, "3 LEASE AND PAUSE", &a, &Text::new());
        let _ = read_timed(4000, 1, false);
        pause_for(seconds * SECOND);
        gaps[i] = read_timed(4000 + 1 + 128, 1, true);
    }

    head().lease_ms = clock::ms(lease[1]);
    let r = report();
    r.page("3 LEASE AND PAUSE", gaps.iter().all(|g| g.ok));
    let mut t = Text::new();
    t.s("LEASE STOPS READ MS ")
        .u(clock::ms(lease[0]))
        .s("/")
        .u(clock::ms(lease[1]))
        .s("/")
        .u(clock::ms(lease[2]));
    r.line(&t);
    for (i, seconds) in [0u32, 5, 15].into_iter().enumerate() {
        let mut label = Text::new();
        label.s("PAUSE, WAIT ").u(seconds).s("S, READ ");
        r.line(&describe(&label, &gaps[i]));
        let mut key = Text::new();
        key.s("PG").u(seconds);
        r.kv(key.as_str(), &[gaps[i].done, gaps[i].code]);
    }
    r.kv("LG", &lease);
}

/// What a Stop does to the next read, immediately and once the motor is
/// quiet. Last on purpose: a read right after Stop is the case that failed on
/// a console, and the drive may need the transport's recovery afterwards.
pub fn stop_tests(screen: &mut Screen) {
    let mut a = Text::new();
    a.s("STOP THEN READ AT ONCE");
    status(screen, "6 STOP", &a, &Text::new());
    let _ = read_timed(4000, 1, false);
    let mut stop_ack = 0;
    let mut stop_after_ack = NO_READ;
    if let Some(mut cd) = acquire_audio() {
        if let Some(t) = timed_command(&mut cd, CMD_STOP, &[], false, 3 * SECOND) {
            stop_ack = t.ack;
        }
        release_audio(cd);
        stop_after_ack = read_timed(4000 + 1 + 128, 1, true);
    }
    // Whatever that did, the transport must be able to read afterwards.
    let recovered = read_timed(4000, 1, true);

    let mut a = Text::new();
    a.s("STOP, WAIT FOR MOTOR");
    status(screen, "6 STOP", &a, &Text::new());
    let mut stop_complete = 0;
    let mut motor_off = 0;
    let mut stop_settled = NO_READ;
    if let Some(mut cd) = acquire_audio() {
        let t0 = clock::now();
        if let Some(t) = timed_command(&mut cd, CMD_STOP, &[], true, 8 * SECOND) {
            stop_complete = t.complete;
        }
        while clock::since(t0) < 8 * SECOND {
            if drive::stat(&mut cd) & STAT_MOTOR_ON == 0 {
                motor_off = clock::since(t0);
                break;
            }
        }
        release_audio(cd);
        stop_settled = read_timed(4000 + 1 + 128, 1, true);
    }

    head().stop_at_once = if stop_after_ack.ok { 0 } else { 1 };
    head().stop_settled_ms = clock::ms(stop_settled.done);
    let r = report();
    r.page("6 STOP", stop_settled.ok && recovered.ok);
    let mut t = Text::new();
    t.s("STOP ACK ").u(clock::ms(stop_ack)).s("MS");
    r.line(&t);
    r.line(&describe(
        Text::new().s("READ AT ONCE AFTER STOP "),
        &stop_after_ack,
    ));
    let mut t = Text::new();
    t.s("NEXT READ ")
        .s(if recovered.ok { "OK" } else { "FAIL" });
    r.line(&t);
    let mut t = Text::new();
    t.s("STOP DONE ").u(clock::ms(stop_complete)).s("MS");
    r.line(&t);
    let mut t = Text::new();
    t.s("MOTOR OFF ").u(clock::ms(motor_off)).s("MS");
    r.line(&t);
    r.line(&describe(
        Text::new().s("READ AFTER MOTOR OFF "),
        &stop_settled,
    ));
    r.kv("SA", &[stop_ack, stop_after_ack.done, stop_after_ack.code]);
    r.kv("SN", &[recovered.done, recovered.code]);
    r.kv(
        "SS",
        &[
            stop_complete,
            motor_off,
            stop_settled.done,
            stop_settled.code,
        ],
    );
}

// ------------------------------------------------------------- 4. CD-DA

fn play_position(cd: &mut Cd) -> Option<PlayPosition> {
    cd.play_position()
        .ok()
        .and_then(|response| PlayPosition::parse(&response))
}

/// Start the tone from the top. Returns (HBlanks until the drive reports
/// PLAYING, whether it ever did).
fn start_tone(cd: &mut Cd) -> (u32, bool) {
    let _ = cd.set_mode(CDDA_MODE);
    let _ = cd.unmute();
    let t0 = clock::now();
    let _ = cd.play_track(TONE_TRACK);
    while clock::since(t0) < 4 * SECOND {
        if drive::stat(cd) & STAT_PLAYING != 0 {
            return (clock::since(t0), true);
        }
    }
    (clock::since(t0), false)
}

/// Resume at an absolute position. Returns (HBlanks until PLAYING, ok).
fn resume_at(cd: &mut Cd, at: &PlayPosition) -> (u32, bool) {
    let _ = cd.set_mode(CDDA_MODE);
    let _ = cd.unmute();
    let target = [
        bin_to_bcd(at.absolute_min),
        bin_to_bcd(at.absolute_sec),
        bin_to_bcd(at.absolute_frame),
    ];
    let _ = cd.command(CMD_SETLOC, &target);
    let t0 = clock::now();
    let _ = cd.command(CMD_PLAY, &[]);
    while clock::since(t0) < 4 * SECOND {
        if drive::stat(cd) & STAT_PLAYING != 0 {
            return (clock::since(t0), true);
        }
    }
    (clock::since(t0), false)
}

/// Absolute frames of a position.
fn absolute_frames(p: &PlayPosition) -> u32 {
    (u32::from(p.absolute_min) * 60 + u32::from(p.absolute_sec)) * 75 + u32::from(p.absolute_frame)
}

/// Stop whatever the drive is doing with a Pause.
fn quiet() {
    if let Some(mut cd) = acquire_audio() {
        let _ = timed_command(&mut cd, CMD_PAUSE, &[], true, 3 * SECOND);
        release_audio(cd);
    }
}

fn answer_code(a: bool) -> u32 {
    u32::from(a)
}

pub fn cdda(screen: &mut Screen) {
    // 1. Does the tone play, and how long until the drive says so?
    let mut a = Text::new();
    a.s("STARTING THE TONE");
    status(screen, "4 CD-DA", &a, &Text::new());
    let mut play_start = (0, false);
    let mut play_stat = 0u8;
    if let Some(mut cd) = acquire_audio() {
        play_start = start_tone(&mut cd);
        delay(SECOND);
        play_stat = drive::stat(&mut cd);
        release_audio(cd);
    }
    let heard = screen.ask(
        "LISTEN 1 OF 4",
        &[
            "A RISING TONE",
            "SHOULD BE PLAYING",
            "NOW, STEPPING UP",
            "A NOTE EVERY",
            "FOUR TENTHS OF A",
            "SECOND.",
            "DO YOU HEAR IT?",
        ],
    );

    // 2. The proper hand-off: Pause the audio, data read, resume the audio.
    let mut a = Text::new();
    a.s("PAUSE, READ, RESUME");
    status(screen, "4 CD-DA", &a, &Text::new());
    let mut t_ack = 0;
    let mut t_complete = 0;
    let mut stat_paused = 0u8;
    let mut saved = None;
    let mut before_pause = 0u32;
    if let Some(mut cd) = acquire_audio() {
        let _ = start_tone(&mut cd);
        delay(5 * SECOND / 2);
        before_pause = play_position(&mut cd).map_or(0, |p| p.relative_millis());
        if let Some(t) = timed_command(&mut cd, CMD_PAUSE, &[], true, 4 * SECOND) {
            t_ack = t.ack;
            t_complete = t.complete;
        }
        stat_paused = drive::stat(&mut cd);
        saved = play_position(&mut cd);
        release_audio(cd);
    }
    let read_after_pause = read_timed(5000, 200, true);
    let mut t_resume = (0, false);
    let mut moved = 0u32;
    let mut resumed_ms = 0u32;
    if let (Some(saved), Some(mut cd)) = (saved, acquire_audio()) {
        t_resume = resume_at(&mut cd, &saved);
        delay(2 * SECOND);
        if let Some(now) = play_position(&mut cd) {
            moved = absolute_frames(&now).wrapping_sub(absolute_frames(&saved));
            resumed_ms = now.relative_millis();
        }
        release_audio(cd);
    }
    let resumed_right = screen.ask(
        "LISTEN 2 OF 4",
        &[
            "THE TONE WAS",
            "PAUSED FOR A DATA",
            "READ, THEN RESUMED.",
            "DID IT CONTINUE",
            "FROM WHERE IT",
            "STOPPED, NOT",
            "FROM THE START?",
        ],
    );

    // The same hand-off once more with the recovery Pause off, to see what the
    // recovery costs when the audio side already paused.
    let mut a = Text::new();
    a.s("PAUSE, READ, NO RECOVERY");
    status(screen, "4 CD-DA", &a, &Text::new());
    psx_cdstream::configure(config(true, false));
    let mut read_plain = NO_READ;
    if let Some(mut cd) = acquire_audio() {
        let _ = start_tone(&mut cd);
        delay(2 * SECOND);
        let _ = timed_command(&mut cd, CMD_PAUSE, &[], true, 4 * SECOND);
        release_audio(cd);
        read_plain = read_timed(5000, 200, true);
    }
    psx_cdstream::configure(config(true, true));

    // 3. Audio still playing when the lease is released: the transport's
    //    recovery Pause has to stop it before the seek.
    let mut a = Text::new();
    a.s("READ OVER PLAYING AUDIO");
    let mut b = Text::new();
    b.s("RECOVERY PAUSE ON");
    status(screen, "4 CD-DA", &a, &b);
    let mut stat_before_recovery = 0u8;
    if let Some(mut cd) = acquire_audio() {
        let _ = start_tone(&mut cd);
        delay(2 * SECOND);
        stat_before_recovery = drive::stat(&mut cd);
        release_audio(cd);
    }
    let read_recovery = read_timed(5000, 200, true);
    let stopped_with_pause = screen.ask(
        "LISTEN 3 OF 4",
        &[
            "THE TONE WAS",
            "PLAYING WHEN A",
            "DATA READ STARTED",
            "(WITH THE RECOVERY",
            "PAUSE).",
            "LISTEN: DID THE",
            "TONE STOP?",
        ],
    );

    // 4. The same with no Pause at all: a bare seek and read over the audio.
    let mut a = Text::new();
    a.s("READ OVER PLAYING AUDIO");
    let mut b = Text::new();
    b.s("NO PAUSE");
    status(screen, "4 CD-DA", &a, &b);
    psx_cdstream::configure(config(true, false));
    let mut stat_before_bare = 0u8;
    if let Some(mut cd) = acquire_audio() {
        let _ = start_tone(&mut cd);
        delay(2 * SECOND);
        stat_before_bare = drive::stat(&mut cd);
        release_audio(cd);
    }
    let read_bare = read_timed(5000, 200, true);
    let stopped_bare = screen.ask(
        "LISTEN 4 OF 4",
        &[
            "THE TONE WAS",
            "PLAYING WHEN A",
            "DATA READ STARTED",
            "(NO PAUSE FIRST).",
            "LISTEN: DID THE",
            "TONE STOP?",
        ],
    );
    psx_cdstream::configure(config(true, true));
    let after_a = read_timed(100, 1, true);
    let after_b = read_timed(100 + 1 + 2048, 1, true);
    quiet();

    let ok = read_after_pause.ok && read_recovery.ok && after_b.ok && t_resume.1;
    {
        let h = head();
        h.pause_idle_ms10 = clock::ms10(t_complete);
        h.first_after_audio_ms = clock::ms(read_after_pause.first);
        h.resume_ms = clock::ms(t_resume.0);
        h.answers = [
            answer_code(heard),
            answer_code(resumed_right),
            answer_code(stopped_with_pause),
            answer_code(stopped_bare),
        ];
    }
    let r = report();
    r.page("4 CD-DA HAND-OFF", ok);
    let mut t = Text::new();
    t.s("TONE START ").u(clock::ms(play_start.0)).s("MS");
    r.line(&t);
    let mut t = Text::new();
    t.s("HEARD IT ").s(if heard { "YES" } else { "NO" });
    r.line(&t);
    let mut t = Text::new();
    t.s("PAUSE ACK ").u(clock::ms(t_ack)).s("MS");
    r.line(&t);
    let mut t = Text::new();
    t.s("PAUSE IDLE ").tenths(clock::ms10(t_complete)).s("MS");
    r.line(&t);
    r.line(&describe(
        Text::new().s("READ AFTER AUDIO "),
        &read_after_pause,
    ));
    let mut t = Text::new();
    t.s("FIRST SECTOR ")
        .u(clock::ms(read_after_pause.first))
        .s("MS");
    r.line(&t);
    let mut t = Text::new();
    t.s("  WITHOUT RECOVERY ")
        .u(clock::ms(read_plain.first))
        .s("MS");
    r.line(&t);
    let mut t = Text::new();
    t.s("RESUME ").u(clock::ms(t_resume.0)).s("MS");
    r.line(&t);
    let mut t = Text::new();
    t.s("RESUMED RIGHT ")
        .s(if resumed_right { "YES" } else { "NO" });
    r.line(&t);
    r.page("4 CD-DA OVER READS", read_recovery.ok && read_bare.ok);
    r.line(&describe(Text::new().s("RECOVERY RD "), &read_recovery));
    let mut t = Text::new();
    t.s("TONE STOPPED ")
        .s(if stopped_with_pause { "YES" } else { "NO" });
    r.line(&t);
    r.line(&describe(Text::new().s("NO PAUSE RD "), &read_bare));
    let mut t = Text::new();
    t.s("TONE STOPPED ")
        .s(if stopped_bare { "YES" } else { "NO" });
    r.line(&t);
    r.line(&describe(Text::new().s("AFTER NEAR "), &after_a));
    r.line(&describe(Text::new().s("AFTER FAR "), &after_b));
    let mut t = Text::new();
    t.s("STAT PLAY ")
        .u(u32::from(play_stat))
        .s(" PAUSE ")
        .u(u32::from(stat_paused));
    r.line(&t);
    let mut t = Text::new();
    t.s("RESUME MOVED ").u(moved).s(" FR");
    r.line(&t);

    r.kv(
        "PL",
        &[
            play_start.0,
            u32::from(play_start.1),
            u32::from(play_stat),
            answer_code(heard),
        ],
    );
    r.kv(
        "TP",
        &[t_ack, t_complete, u32::from(stat_paused), before_pause],
    );
    r.kv(
        "D2",
        &[
            read_after_pause.first,
            read_after_pause.done,
            read_after_pause.idle,
            read_after_pause.code,
        ],
    );
    r.kv(
        "RS",
        &[
            t_resume.0,
            u32::from(t_resume.1),
            moved,
            resumed_ms,
            answer_code(resumed_right),
        ],
    );
    r.kv(
        "D3",
        &[
            read_recovery.first,
            read_recovery.done,
            read_recovery.idle,
            read_recovery.code,
            answer_code(stopped_with_pause),
            u32::from(stat_before_recovery),
        ],
    );
    r.kv(
        "D4",
        &[
            read_bare.first,
            read_bare.done,
            read_bare.idle,
            read_bare.code,
            answer_code(stopped_bare),
            u32::from(stat_before_bare),
        ],
    );
    r.kv(
        "PS",
        &[after_a.done, after_a.code, after_b.done, after_b.code],
    );
    r.kv(
        "D2B",
        &[
            read_plain.first,
            read_plain.done,
            read_plain.idle,
            read_plain.code,
        ],
    );
}

// ----------------------------------------------------------------- summary

fn yes_no(code: u32) -> &'static str {
    match code {
        1 => "Y",
        0 => "N",
        _ => "-",
    }
}

/// One page with the headline numbers, built last.
pub fn summary(memory_free_k: u32, stack_bytes: u32) {
    let h = *head();
    let r = report();
    r.page("SUMMARY", true);
    let mut t = Text::new();
    t.s("SEEK MS 1:")
        .u(h.seek_forward_ms[0])
        .s(" 16:")
        .u(h.seek_forward_ms[1])
        .s(" 128:")
        .u(h.seek_forward_ms[2]);
    r.line(&t);
    let mut t = Text::new();
    t.s("SEEK MS 512:")
        .u(h.seek_forward_ms[3])
        .s(" 2K:")
        .u(h.seek_forward_ms[4])
        .s(" 8K:")
        .u(h.seek_forward_ms[5]);
    r.line(&t);
    let mut t = Text::new();
    t.s("READ 2X ")
        .tenths(h.rate_x10[0])
        .s("/S 1X ")
        .tenths(h.rate_x10[1])
        .s("/S");
    r.line(&t);
    let mut t = Text::new();
    t.s("PIO US/SECTOR 2X ")
        .u(h.pio_us[0])
        .s(" 1X ")
        .u(h.pio_us[1]);
    r.line(&t);
    let mut t = Text::new();
    t.s("CPU LOST 2X ")
        .tenths(h.lost_permille[0])
        .s("% 1X ")
        .tenths(h.lost_permille[1])
        .s("%");
    r.line(&t);
    let mut t = Text::new();
    t.s("HANDLER MAX ").u(h.handler_us).s("US");
    r.line(&t);
    let mut t = Text::new();
    t.s("LEASE ")
        .u(h.lease_ms)
        .s("MS PAUSE-IDLE ")
        .tenths(h.pause_idle_ms10)
        .s("MS");
    r.line(&t);
    let mut t = Text::new();
    t.s("1ST SECTOR AFTER AUDIO ")
        .u(h.first_after_audio_ms)
        .s("MS");
    r.line(&t);
    let mut t = Text::new();
    t.s("AUDIO RESUME ").u(h.resume_ms).s("MS");
    r.line(&t);
    let mut t = Text::new();
    t.s("HEARD ")
        .s(yes_no(h.answers[0]))
        .s(" RESUMED ")
        .s(yes_no(h.answers[1]))
        .s(" STOP ")
        .s(yes_no(h.answers[2]))
        .s("/")
        .s(yes_no(h.answers[3]));
    r.line(&t);
    let mut t = Text::new();
    t.s("READ AT ONCE AFTER STOP ").s(match h.stop_at_once {
        0 => "OK",
        1 => "FAIL",
        _ => "-",
    });
    r.line(&t);
    let mut t = Text::new();
    t.s("FREE ")
        .u(memory_free_k)
        .s("K STACK ")
        .u(stack_bytes)
        .s("B");
    r.line(&t);
}
