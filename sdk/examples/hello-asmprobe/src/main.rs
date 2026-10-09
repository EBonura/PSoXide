// SPDX-License-Identifier: GPL-2.0-or-later
//! `hello-asmprobe` -- differential probe and micro-benchmark for the SDK's
//! hand-scheduled kernels.
//!
//! Every kernel is compared bit for bit against a portable Rust reference on
//! random and hand-picked edge inputs, then timed on a fixed workload. The
//! verdict goes to the TTY (`ASMPROBE PASS` / `ASMPROBE FAIL`). Per-function
//! cycles come from the emulator's attribution logs over the link map.

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]

extern crate psx_rt;

mod clip;
mod link;

use psx_rt::tty;

/// xorshift32, the same stream on every run.
pub struct Rng(pub u32);

impl Rng {
    pub fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
}

pub fn tty_fail(name: &str, mode: u32, packets: u32, bad: u32) {
    tty::print("  FAIL ");
    tty::print(name);
    tty::print(" mode=");
    tty::print_hex_u32(mode);
    tty::print(" packets=");
    tty::print_hex_u32(packets);
    tty::print(" bad=");
    tty::print_hex_u32(bad);
    tty::println("");
}

/// The system clock as a 16-bit counter: root counter 1 on the system clock.
pub fn clock_start() {
    psx_io::timers::set_mode(psx_io::timers::Timer::Timer1, 0);
}

#[inline(always)]
pub fn clock() -> u16 {
    psx_io::timers::counter(psx_io::timers::Timer::Timer1)
}

pub fn print_dec(label: &str, v: u32) {
    tty::print(label);
    let mut buf = [0u8; 10];
    let mut n = 0;
    let mut x = v;
    if x == 0 {
        buf[0] = b'0';
        n = 1;
    }
    while x > 0 {
        buf[n] = b'0' + (x % 10) as u8;
        x /= 10;
        n += 1;
    }
    let mut out = [0u8; 10];
    for i in 0..n {
        out[i] = buf[n - 1 - i];
    }
    tty::print(core::str::from_utf8(&out[..n]).unwrap());
    tty::println("");
}

pub fn tty_label_clip(name: &str) {
    tty::print("BENCH clip ");
    tty::print(name);
    tty::print(" cyc/10call=");
}

pub fn tty_label(name: &str, mode: u32) {
    tty::print("BENCH link ");
    tty::print(name);
    tty::print(" mode=");
    tty::print_hex_u32(mode);
    tty::print(" cyc/100pkt=");
}

pub fn print_u32(label: &str, v: u32) {
    tty::print(label);
    tty::print_hex_u32(v);
    tty::println("");
}

#[no_mangle]
fn main() {
    clock_start();
    let mut failures = 0u32;
    let mut cases = 0u32;
    clip::run(&mut cases, &mut failures);
    clip::bench();
    link::run(&mut cases, &mut failures);
    if failures == 0 {
        tty::print("ASMPROBE PASS cases=");
        tty::print_hex_u32(cases);
        tty::println("");
    } else {
        tty::print("ASMPROBE FAIL ");
        tty::print_hex_u32(failures);
        tty::print(" of ");
        tty::print_hex_u32(cases);
        tty::println("");
    }
    link::bench();
    tty::println("ASMPROBE DONE");
    loop {}
}
