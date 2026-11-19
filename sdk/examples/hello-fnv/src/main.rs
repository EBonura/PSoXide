//! `hello-fnv` -- does `psx_pack::Fnv32` give plain FNV-1a on the console CPU?
//!
//! The guest forms of `update` and `update_bulk` are assembly (the multiply by the prime runs on
//! the multiplier with a small first operand), which the host tests of `psx-pack` do not compile.
//! This runs them: every length 0..=67 at every alignment 0..=3 against a plain reference, fed
//! whole, as `fnv1a32`, bytewise through `push`, and cut at every two-way and three-way split,
//! then long buffers; the same for `WordFnv32` over every word count 0..=67 (`update`, `update_raw`,
//! `push`, every two-way split) against `(hash ^ word) * 0x0100_0193`. The verdict goes to the TTY:
//! `FNV PASS cases=N` or `FNV FAIL bad=N`.

#![no_std]
#![no_main]

extern crate psx_rt;

use core::hint::black_box;
use psx_pack::{fnv1a32, Fnv32, WordFnv32};
use psx_rt::tty;

/// Plain FNV-1a over `bytes` from `hash`.
fn reference(mut hash: u32, bytes: &[u8]) -> u32 {
    for &byte in bytes {
        hash = (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193);
    }
    hash
}

/// Plain word FNV from `hash`.
fn reference_words(mut hash: u32, words: &[u32]) -> u32 {
    for &word in words {
        hash = (hash ^ word).wrapping_mul(0x0100_0193);
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
    // SAFETY: the buffer is word aligned and 1100 bytes long; the words are the first 256 of it.
    let words: &[u32] =
        black_box(unsafe { core::slice::from_raw_parts(buffer.as_ptr().cast::<u32>(), 256) });
    for len in (0..=67).chain([128, 255, 256]) {
        let w = &words[..len];
        let want = reference_words(WordFnv32::OFFSET_BASIS, w);
        check(WordFnv32::of(w), want, &mut cases, &mut bad);
        let mut raw = WordFnv32::new();
        // SAFETY: `w` is readable and aligned for its own length.
        unsafe { raw.update_raw(w.as_ptr(), len) };
        check(raw.finish(), want, &mut cases, &mut bad);
        let mut pushed = WordFnv32::new();
        for &word in w {
            pushed.push(word);
        }
        check(pushed.finish(), want, &mut cases, &mut bad);
        if len > 67 {
            continue;
        }
        for a in 0..=len {
            let mut two = WordFnv32::new();
            two.update(&w[..a]);
            two.update(&w[a..]);
            check(two.finish(), want, &mut cases, &mut bad);
            let mut resumed =
                WordFnv32::from_state(reference_words(WordFnv32::OFFSET_BASIS, &w[..a]));
            resumed.update(&w[a..]);
            check(resumed.finish(), want, &mut cases, &mut bad);
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
