//! `hello-fnv` -- does `psx_pack::Fnv32` give plain FNV-1a on the console CPU?
//!
//! The guest forms of `update` and `update_bulk` are assembly (the multiply by the prime runs on
//! the multiplier with a small first operand), which the host tests of `psx-pack` do not compile.
//! This runs them: every length 0..=67 at every alignment 0..=3 against a plain reference, fed
//! whole, as `fnv1a32`, bytewise through `push`, and cut at every two-way and three-way split,
//! then long buffers. The verdict goes to the TTY: `FNV PASS cases=N` or `FNV FAIL bad=N`.

#![no_std]
#![no_main]

extern crate psx_rt;

use core::hint::black_box;
use psx_pack::{fnv1a32, Fnv32};
use psx_rt::tty;

/// Plain FNV-1a over `bytes` from `hash`.
fn reference(mut hash: u32, bytes: &[u8]) -> u32 {
    for &byte in bytes {
        hash = (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193);
    }
    hash
}

/// A word-aligned buffer, so offsets 0..=3 give every alignment.
#[repr(align(4))]
struct Buffer([u8; 1100]);

static mut BUFFER: Buffer = Buffer([0; 1100]);

/// Counts one case; a mismatch counts as bad.
fn check(got: u32, want: u32, cases: &mut u32, bad: &mut u32) {
    *cases += 1;
    if got != want {
        *bad += 1;
    }
}

#[no_mangle]
fn main() {
    // SAFETY: the only reference to the buffer, taken once at start.
    let buffer = unsafe { &mut *core::ptr::addr_of_mut!(BUFFER.0) };
    let mut x = 0x9e37_79b9u32;
    for byte in buffer.iter_mut() {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *byte = (x >> 24) as u8;
    }
    let buffer: &[u8] = black_box(&buffer[..]);
    let (mut cases, mut bad) = (0u32, 0u32);
    let basis = Fnv32::OFFSET_BASIS;
    for offset in 0..4 {
        for len in (0..=67).chain([255, 256, 257, 1000, 1024, 1090]) {
            let bytes = &buffer[offset..offset + len];
            let want = reference(basis, bytes);
            check(fnv1a32(bytes), want, &mut cases, &mut bad);
            let mut whole = Fnv32::new();
            whole.update_bulk(bytes);
            check(whole.finish(), want, &mut cases, &mut bad);
            let mut small = Fnv32::new();
            small.update(bytes);
            check(small.finish(), want, &mut cases, &mut bad);
            let mut pushed = Fnv32::new();
            for &byte in bytes {
                pushed.push(byte);
            }
            check(pushed.finish(), want, &mut cases, &mut bad);
            if len > 67 {
                continue;
            }
            for a in 0..=len {
                let mut two = Fnv32::new();
                two.update_bulk(&bytes[..a]);
                two.update_bulk(&bytes[a..]);
                check(two.finish(), want, &mut cases, &mut bad);
                let mut mixed = Fnv32::new();
                mixed.update(&bytes[..a]);
                mixed.update_bulk(&bytes[a..]);
                check(mixed.finish(), want, &mut cases, &mut bad);
                for b in a..=len {
                    let mut three = Fnv32::new();
                    three.update_bulk(&bytes[..a]);
                    three.update_bulk(&bytes[a..b]);
                    three.update_bulk(&bytes[b..]);
                    check(three.finish(), want, &mut cases, &mut bad);
                }
            }
        }
    }
    if bad == 0 {
        tty::print("FNV PASS cases=0x");
        tty::print_hex_u32(cases);
    } else {
        tty::print("FNV FAIL bad=0x");
        tty::print_hex_u32(bad);
    }
    tty::print("\n");
    loop {}
}
