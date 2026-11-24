//! `hello-core-overlay` -- a program on the scene core with a fixed-address overlay
//! (psoxide-overlay.ld), loaded, written over and loaded again.
//!
//! Overlay 1 holds `menu_sum` and its table `SQUARES`, linked with the program at `OVERLAY_VMA`
//! (ram.ld) but kept out of its EXE: `psx-exe --overlay` writes it as `MENU.OVL`, and the scene
//! disc's manifest names it. `menu_sum` calls back into resident code, the way a title menu calls
//! the game's own drawing and input code.
//!
//! The program loads the overlay with the core and calls it, then writes over every word of its
//! range, as a game does when its world takes that memory, and loads it again. With
//! `class=volatile` (the shipping case) the core reads it again and the second call gives the same
//! sum: `hello-core-overlay: PASS`. With `class=swap` the core trusts the overlay's loaded bit,
//! returns at once and leaves the overwritten words in place: the program sees them and stops with
//! `hello-core-overlay: STALE` instead of jumping into them.
#![no_std]
#![no_main]

extern crate psx_rt;

use core::hint::black_box;
use psx_core::link;
use psx_rt::tty;

extern "C" {
    static OVERLAY_VMA: u32;
    static __overlay_end: u32;
}

/// The overlay's image index in the scene table (image 0 is this program).
const MENU: u32 = 1;
/// The sum `menu_sum(8)` gives: 0 + 1 + 4 + ... + 49.
const SUM: u32 = 140;

/// The program's copy of its scene table: header and rows, read by the first load.
static mut TABLE: [u32; 64] = [0; 64];

/// Read-only data in the overlay.
#[link_section = ".rodata.overlay.1.squares"]
static SQUARES: [u32; 8] = [0, 1, 4, 9, 16, 25, 36, 49];

/// Resident code the overlay calls.
#[inline(never)]
fn resident_add(a: u32, b: u32) -> u32 {
    a.wrapping_add(black_box(b))
}

/// Overlay code: the sum of the first `n` squares, through resident code.
#[inline(never)]
#[link_section = ".text.overlay.1.menu_sum"]
fn menu_sum(n: u32) -> u32 {
    let mut sum = 0;
    for square in SQUARES.iter().take(n as usize) {
        sum = resident_add(sum, *square);
    }
    sum
}

fn fail(what: &str, value: u32) -> ! {
    tty::print("hello-core-overlay: FAIL ");
    tty::print(what);
    tty::print(" ");
    tty::print_hex_u32(value);
    tty::println("");
    loop {}
}

/// Load the overlay and call it, unless the load left stale words in its place.
fn load_and_call(table: &mut [u32], round: &str) -> u32 {
    let r = link::load_image(MENU, table);
    if r != 0 {
        fail("load", r);
    }
    tty::print("hello-core-overlay: ");
    tty::print(round);
    tty::println(" load ok");
    // Link-time symbol: only its address is taken.
    let start = core::ptr::addr_of!(OVERLAY_VMA) as *const u32;
    // SAFETY: the overlay's first word, in this program's RAM.
    if unsafe { core::ptr::read_volatile(start) } == 0xffff_ffff {
        tty::println("hello-core-overlay: STALE");
        loop {}
    }
    menu_sum(black_box(8))
}

#[no_mangle]
fn main() {
    tty::println("hello-core-overlay: main");
    if link::window_hash() != link::CORE_HASH {
        fail("window", link::window_hash());
    }
    // SAFETY: the one reference to the table, taken once.
    let table = unsafe { &mut *core::ptr::addr_of_mut!(TABLE) };
    let first = load_and_call(table, "first");
    if first != SUM {
        fail("first sum", first);
    }
    // What a game's world does to the memory once play starts: every word of the overlay's range
    // written over, with a word that is no instruction.
    // Link-time symbols: only their addresses are taken.
    let start = core::ptr::addr_of!(OVERLAY_VMA) as *mut u32;
    let end = core::ptr::addr_of!(__overlay_end) as *mut u32;
    let mut at = start;
    while at < end {
        // SAFETY: psoxide-overlay.ld's run range, this program's own memory, unused now.
        unsafe { core::ptr::write_volatile(at, 0xffff_ffff) };
        at = at.wrapping_add(1);
    }
    psx_rt::cache::flush_instruction_cache_range(start as u32, end as u32);
    tty::print("hello-core-overlay: overlay memory overwritten ");
    tty::print_hex_u32(start as u32);
    tty::print(" ");
    tty::print_hex_u32(end as u32);
    tty::println("");
    let second = load_and_call(table, "second");
    if second != SUM {
        fail("second sum", second);
    }
    tty::println("hello-core-overlay: PASS");
    loop {}
}
