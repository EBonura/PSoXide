//! `pad-engine-check` -- the interrupt-driven pad engine, measured against the
//! synchronous driver on the same frames.
//!
//! Phases of 120 frames each, in one program. The main loop does one
//! unit of "game work" (a counter increment) until the next VBlank, so the
//! iterations per frame are the CPU the game keeps:
//!
//! 1. nothing polls: the ceiling;
//! 2. the synchronous driver polls both ports once per frame, as a two-player
//!    game does;
//! 3. the engine polls, in each of its configurations, and the loop reads its
//!    snapshot.
//!
//! Then it exercises the lease (a memory card's turn) and reports the engine's
//! counters. Every result is a `padcheck:` line on the TTY, for a headless run
//! (`frontend launch --path pad-engine-check.exe --steps ...`); a press on pad
//! 1 (`--pad-pulses`) shows up as a `buttons=` line in phase 3.
#![no_std]
#![no_main]
#![allow(deprecated)]

extern crate psx_rt;

use psx_pad::console;
use psx_pad::engine::{BytePacing, Config};
use psx_pad::{poll_on, Port};
use psx_rt::interrupts::{install_vblank_counter, vblank_count};
use psx_rt::tty;

const FRAMES: u32 = 120;

fn dec(mut v: u32) {
    let mut digits = [0u8; 10];
    let mut n = 0;
    loop {
        digits[n] = b'0' + (v % 10) as u8;
        n += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    while n > 0 {
        n -= 1;
        psx_rt::bios::write_char(digits[n]);
    }
}

fn line(label: &str, value: u32) {
    tty::print("padcheck: ");
    tty::print(label);
    tty::print("=");
    dec(value);
    tty::print("\n");
}

/// Spin until the next VBlank; returns the iterations spent.
fn spin_to_next_vblank() -> u32 {
    let start = vblank_count();
    let mut iterations = 0u32;
    while vblank_count() == start {
        iterations = iterations.wrapping_add(1);
    }
    iterations
}

#[no_mangle]
fn main() {
    let peripherals = psx_rt::Peripherals::take().expect("peripherals are taken once");
    let mut port = peripherals.controller_port;
    install_vblank_counter();

    // Phase 1: nothing polls.
    spin_to_next_vblank();
    let mut total = 0u32;
    for _ in 0..FRAMES {
        total = total.wrapping_add(spin_to_next_vblank() / 16);
    }
    line("idle_work_per_frame_x16", total / FRAMES);

    // Phase 2: the synchronous driver, both ports.
    let mut total = 0u32;
    for _ in 0..FRAMES {
        let _ = poll_on(&mut port, Port::One);
        let _ = poll_on(&mut port, Port::Two);
        total = total.wrapping_add(spin_to_next_vblank() / 16);
    }
    line("sync_work_per_frame_x16", total / FRAMES);

    // Phase 3: the engine, in each configuration, 120 frames apiece.
    if console::install(port, Config::DEFAULT).is_err() {
        tty::println("padcheck: install refused");
        loop {}
    }
    line("installed", u32::from(console::is_installed()));
    let nothing = Config {
        ports: [false, false],
        ..Config::DEFAULT
    };
    let timed = Config {
        pacing: BytePacing::Timed,
        ..Config::DEFAULT
    };
    let every_other = Config {
        kick_every: 2,
        ..Config::DEFAULT
    };
    let variants: [(&str, Config); 6] = [
        ("engine_no_ports", nothing),
        ("engine_port1", Config::PORT1_ONLY),
        ("engine_both", Config::DEFAULT),
        ("engine_both_timed", timed),
        ("engine_both_every_other", every_other),
        ("engine_both", Config::DEFAULT),
    ];
    let mut last_buttons = 0xFFFF_u16;
    let mut last_seq = 0u32;
    let mut frame_no = 0u32;
    for (label, config) in variants {
        console::configure(config);
        let before = console::stats();
        let mut total = 0u32;
        for _ in 0..FRAMES {
            total = total.wrapping_add(spin_to_next_vblank() / 16);
            frame_no += 1;
            let snap = console::snapshot();
            let one = snap.port(Port::One);
            if one.pad.buttons.bits() != last_buttons && snap.seq != 0 {
                last_buttons = one.pad.buttons.bits();
                tty::print("padcheck: frame=");
                dec(frame_no);
                tty::print(" buttons=0x");
                tty::print_hex_u32(u32::from(last_buttons));
                tty::print(" mode=");
                dec(one.pad.mode as u32);
                tty::print("\n");
            }
            last_seq = snap.seq;
        }
        line(label, total / FRAMES);
        let after = console::stats();
        line("  events_per_frame_x10", (after.events - before.events) * 10 / FRAMES);
    }
    line("seq", last_seq);
    let snap = console::snapshot();
    line("port1_health", snap.port(Port::One).health as u32);
    line("port1_updates", snap.port(Port::One).updates);
    line("port1_faults", snap.port(Port::One).faults);
    line("port1_mode", snap.port(Port::One).pad.mode as u32);
    line("port2_health", snap.port(Port::Two).health as u32);
    line("port2_faults", snap.port(Port::Two).faults);

    // The lease: a card's turn. Hold the port across three VBlanks.
    let skips_before = console::stats().leased_skips;
    {
        let _lease = console::lease();
        let start = vblank_count();
        while vblank_count().wrapping_sub(start) < 3 {}
    }
    line("leased_skips", console::stats().leased_skips - skips_before);
    for _ in 0..4 {
        spin_to_next_vblank();
    }
    line("updates_after_lease", console::snapshot().port(Port::One).updates);

    let stats = console::stats();
    line("events", stats.events);
    line("kicks", stats.kicks);
    line("stalls", stats.stalls);
    line("spurious", stats.spurious);
    line("stack_unused", console::handler_stack_unused_bytes() as u32);
    tty::println("padcheck: done");
    loop {}
}
