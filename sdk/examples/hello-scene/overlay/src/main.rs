//! A hello-scene overlay: a raw image the scene core loads at 0x80100000 for its owner program.
//!
//! Two entries at its start: +0 the scene entry (the core jumps here, `$sp` at the program's
//! stack top, never returns), +8 a call that returns (`psx_core_load_image`'s use: the program
//! loads the overlay and calls it). Each scene step prints to the TTY, marks the handoff block's
//! private word and asks for the next scene of the walk.
#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]

const BLOCK: u32 = 0x801f_ff00;
const MARK: u32 = BLOCK + 0x80;
/// Where the base program leaves its scene-table copy's address.
const TABLE_AT: u32 = BLOCK + 0x84;
const TTY: u32 = 0x1f80_2080;
#[cfg(not(feature = "second"))]
const ID: u32 = 1;
#[cfg(feature = "second")]
const ID: u32 = 2;

extern "C" {
    fn psx_core_switch(scene: u32, args: *const [u32; 4], table: *const u32) -> u32;
}

fn r32(a: u32) -> u32 {
    // SAFETY: the handoff block, fixed RAM.
    unsafe { core::ptr::read_volatile(a as *const u32) }
}

fn w32(a: u32, v: u32) {
    // SAFETY: the handoff block's private area, fixed RAM.
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}

fn print(s: &[u8]) {
    for &b in s {
        // SAFETY: the emulator's debug console port.
        unsafe { core::ptr::write_volatile(TTY as *mut u8, b) };
    }
}

fn hex(v: u32) {
    let mut i = 8;
    while i > 0 {
        i -= 1;
        let n = ((v >> (4 * i)) & 0xf) as u8;
        print(&[if n < 10 { b'0' + n } else { b'A' + n - 10 }]);
    }
}

/// The scene entry.
#[no_mangle]
extern "C" fn overlay_scene() -> ! {
    let step = r32(BLOCK + 0x1c);
    w32(MARK, 0x100 * ID + step);
    print(b"hello-scene: O");
    hex(ID);
    print(b" scene step ");
    hex(step);
    print(b" mask ");
    hex(r32(BLOCK + 0x74));
    print(b" flags ");
    hex(r32(BLOCK + 0x18));
    print(b"\n");
    // The walk: see hello-scene's main.rs.
    let next = match (ID, step) {
        (1, 2) => 1, // back to the program's own scene, O1 stays in RAM
        (1, 4) => 3, // O2: read, evicts O1
        (2, 5) => 2, // O1 again: read again
        (1, 6) => 4, // the second program: a program switch
        _ => 0xff,
    };
    let args = [step, 0, 0, 0];
    // SAFETY: the core's switch slot; the table copy's address the base program left.
    let r = unsafe { psx_core_switch(next, &args, r32(TABLE_AT) as *const u32) };
    print(b"hello-scene: O switch refused ");
    hex(r);
    print(b"\n");
    loop {}
}

/// The call entry: returns a value that names the overlay.
#[no_mangle]
extern "C" fn overlay_call(x: u32) -> u32 {
    0x0c00_0000 | (ID << 16) | (x & 0xffff)
}

core::arch::global_asm!(
    ".section .text.__overlay_table,\"ax\",@progbits",
    ".globl __overlay_table",
    ".set noreorder",
    "__overlay_table:",
    "j overlay_scene",
    "nop",
    "j overlay_call",
    "nop",
    ".set reorder",
);

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
