//! `hello-called` -- a program that calls another program and gets its machine back.
//!
//! Hollow Knight calls its intro blob through the blob's psx-rt `_start`, twice per new game, and
//! goes on with its own VRAM afterwards. A program's `_start` puts the machine in the entry state
//! (all of VRAM black, one display, the CD mixer at unity), which under a running game would wipe
//! what the game has uploaded. A called program is built with psx-rt's `called-program` feature
//! instead: its `_start` returns after `main`, and its entry state leaves the caller's VRAM,
//! display and CD mixer alone.
//!
//! The caller (no feature) sets a display unlike the entry state's (512 wide), fills two VRAM
//! rectangles, copies the blob's EXE (embedded at build time) to its link address and calls its
//! `_start` twice. The blob prints and returns. The caller then checks that both rectangles hold
//! what it drew and that the display mode is the one it set: `hello-called: PASS`, else
//! `hello-called: FAIL <what> <value>`. (The CD mixer's registers cannot be read back; it is left
//! by the same `cfg` as the GPU part.)
#![no_std]
#![no_main]

extern crate psx_rt;

use psx_rt::tty;

#[cfg(feature = "blob")]
#[no_mangle]
fn main() {
    tty::println("hello-called: the called program ran");
}

#[cfg(not(feature = "blob"))]
mod caller {
    use core::ptr::{read_volatile, write_volatile};
    use psx_hw::gpu;
    use psx_rt::tty;

    /// The called program's EXE.
    static BLOB: &[u8] = include_bytes!(env!("HELLO_CALLED_BLOB_EXE"));

    fn r32(addr: u32) -> u32 {
        // SAFETY: a fixed MMIO register.
        unsafe { read_volatile(addr as *const u32) }
    }
    fn w32(addr: u32, v: u32) {
        // SAFETY: a fixed MMIO register.
        unsafe { write_volatile(addr as *mut u32, v) }
    }

    /// Word FNV of VRAM rectangle `x, y, w, h` (w even).
    fn vram(x: u32, y: u32, w: u32, h: u32) -> u32 {
        while r32(gpu::GPUSTAT) & (1 << 26) == 0 {}
        w32(gpu::GP0, gpu::gp0::COPY_VRAM_TO_CPU);
        w32(gpu::GP0, y << 16 | x);
        w32(gpu::GP0, h << 16 | w);
        while r32(gpu::GPUSTAT) & (1 << 27) == 0 {}
        let mut hash = 0x811c_9dc5u32;
        let mut n = 0;
        while n < w * h / 2 {
            hash = (hash ^ r32(gpu::GPUREAD)).wrapping_mul(0x0100_0193);
            n += 1;
        }
        hash
    }

    fn fill(colour: u32, x: u32, y: u32, w: u32, h: u32) {
        psx_io::gpu::wait_command_ready();
        psx_io::gpu::write_command(0x0200_0000 | colour);
        psx_io::gpu::write_command(y << 16 | x);
        psx_io::gpu::write_command(h << 16 | w);
        psx_io::gpu::wait_command_ready();
    }

    fn fail(what: &str, value: u32) -> ! {
        tty::print("hello-called: FAIL ");
        tty::print(what);
        tty::print(" ");
        tty::print_hex_u32(value);
        tty::println("");
        psx_rt::halt();
    }

    fn word(at: usize) -> u32 {
        u32::from_le_bytes([BLOB[at], BLOB[at + 1], BLOB[at + 2], BLOB[at + 3]])
    }

    /// Copy the blob's payload to its link address and call its `_start`.
    fn call_blob() {
        let (pc, t_addr, t_size) = (word(0x10), word(0x18), word(0x1c) as usize);
        let payload = &BLOB[0x800..];
        if payload.len() < t_size {
            fail("blob size", payload.len() as u32);
        }
        // SAFETY: the blob's link range (0x80100000, blob.ld) is RAM this program never uses: its own
        // image and stack lie below it and above it.
        unsafe { core::ptr::copy_nonoverlapping(payload.as_ptr(), t_addr as *mut u8, t_size) };
        psx_rt::cache::flush_instruction_cache();
        // SAFETY: a called program's `_start` is an `extern "C"` function that returns (psx-rt
        // `called-program`), runs on this stack and keeps the callee-saved registers.
        let entry: extern "C" fn(u32, u32, u32) = unsafe { core::mem::transmute(pc as usize) };
        entry(0, 0, 0);
    }

    pub fn main() {
        tty::println("hello-called: caller");
        // A display unlike the entry state's: 512 wide (GP1(08h) horizontal 2), 240 lines, on.
        psx_io::gpu::write_display_control(gpu::gp1::display_mode(2, 0, false, false, false));
        psx_io::gpu::write_display_control(gpu::gp1::display_enable(true));
        let display = r32(gpu::GPUSTAT) >> 16 & 0xff;
        fill(0x0040_80c0, 0, 0, 320, 240);
        fill(0x00c0_4020, 640, 256, 128, 64);
        let (frame, art) = (vram(0, 0, 320, 240), vram(640, 256, 128, 64));
        call_blob();
        call_blob();
        if vram(0, 0, 320, 240) != frame {
            fail("VRAM at 0,0 changed", frame);
        }
        if vram(640, 256, 128, 64) != art {
            fail("VRAM at 640,256 changed", art);
        }
        let after = r32(gpu::GPUSTAT) >> 16 & 0xff;
        if after != display {
            fail("display changed", after);
        }
        tty::println("hello-called: PASS");
    }
}

#[cfg(not(feature = "blob"))]
#[no_mangle]
fn main() {
    caller::main();
}
