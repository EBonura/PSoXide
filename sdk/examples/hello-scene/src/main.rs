//! `hello-scene` -- two programs and the scene core between them (proof P1).
//!
//! Scene 0 (default build) boots from the BIOS, or from a chain loader on a bigger disc, checks
//! the handoff block the core wrote at its cold entry, checks that a switch is refused while a
//! stream client is attached, then asks the core to switch to scene 1. Scene 1 (`role-b`) checks
//! that it was entered by the core, that the window still hashes to its own core hash, that the
//! block carries the switch (scenes, counter, arguments, disc bases equal to psx-rt's), and reads
//! a sector through the core and through a stream to show the core still owns the drive.
//!
//! Each verdict goes to the TTY (`hello-scene: A ...`, `hello-scene: B PASS` or `FAIL`) and to the
//! screen: blue while scene 0 runs, green for a pass, magenta for a failed check, the core's red
//! fatal screen for a refused switch. Without the `core` feature the same program is built the
//! old way (no core, stock RAM size) to time a plain BIOS boot against.
#![no_std]
#![no_main]

extern crate psx_rt;

use psx_rt::tty;

fn gp0(word: u32) {
    psx_io::gpu::wait_command_ready();
    psx_io::gpu::write_command(word);
}

fn gp1(word: u32) {
    psx_io::gpu::write_display_control(word);
}

/// The whole screen in `colour` (0xBBGGRR), with `mark` drawn as eight bars like the fatal
/// screen's, so a frame dump says which program and which result.
fn screen(colour: u32, mark: u32) {
    gp1(0x0000_0000);
    gp1(0x0800_0001);
    gp1(0x06c6_0260);
    gp1(0x0704_0010);
    gp1(0x0500_0000);
    gp0(0x0200_0000 | colour);
    gp0(0);
    gp0((240 << 16) | 320);
    for bit in 0..8u32 {
        let set = mark & (0x80 >> bit) != 0;
        let (c, top) = if set {
            (0xff_ffff, 80)
        } else {
            (0x60_6060, 140)
        };
        gp0(0x0200_0000 | c);
        gp0((top << 16) | (32 + bit * 32));
        gp0(((160 - top) << 16) | 16);
    }
    gp1(0x0300_0000);
}

/// A word the harness watches (`--route-watch-u32 0x801FFF80`, the handoff block's private
/// area): 0xA0 when scene 0's first frame is up, 0xA1 when it asks for the switch, 0xB0 when
/// scene 1's `main` starts. Its route log then times the boot and the switch to the frame.
const MARK: u32 = 0x801f_ff80;

fn mark(value: u32) {
    // SAFETY: the handoff block's private area, which no core or BIOS writes.
    unsafe { core::ptr::write_volatile(MARK as *mut u32, value) };
}

fn say(label: &str, value: u32) {
    tty::print(label);
    tty::print_hex_u32(value);
    tty::println("");
}

#[cfg(feature = "core")]
mod scene {
    use super::*;
    use psx_core::abi::{block, code, table};
    use psx_core::link as core_;

    const BLUE: u32 = 0xc0_4000;
    const GREEN: u32 = 0x00_c000;
    const MAGENTA: u32 = 0xc0_00c0;

    /// Arguments scene 0 passes; scene 1 checks them.
    const ARGS: [u32; 4] = [0x5ce1_0001, 0x5ce1_0002, 0x5ce1_0003, 0x5ce1_0004];

    fn check(ok: &mut bool, what: &str, got: u32, want: u32) {
        if got != want {
            *ok = false;
            tty::print("hello-scene: mismatch ");
            tty::print(what);
            tty::print(" got ");
            tty::print_hex_u32(got);
            tty::print(" want ");
            tty::print_hex_u32(want);
            tty::println("");
        }
    }

    /// What every scene checks of the block and the window.
    fn common(ok: &mut bool) {
        check(ok, "block-valid", core_::block_valid() as u32, 1);
        check(
            ok,
            "block-version",
            core_::block_word(block::OFF_VERSION),
            block::VERSION | (block::BYTES << 16),
        );
        check(
            ok,
            "block-core-hash",
            core_::block_word(block::OFF_CORE_HASH),
            core_::CORE_HASH,
        );
        check(ok, "window-hash", core_::window_hash(), core_::CORE_HASH);
        check(
            ok,
            "lba-offset",
            core_::block_word(block::OFF_LBA_OFFSET),
            psx_io::disc_base::lba_offset(),
        );
        check(
            ok,
            "cdda-base",
            core_::block_word(block::OFF_CDDA_BASE),
            psx_io::disc_base::cdda_track_base() as u32,
        );
    }

    #[cfg(not(feature = "role-b"))]
    pub fn run() -> ! {
        screen(BLUE, 0xa0);
        mark(0xa0);
        tty::println("hello-scene: A main");
        say("hello-scene: A core-hash ", core_::CORE_HASH);
        say("hello-scene: A core-bytes ", core_::CORE_BYTES);
        say(
            "hello-scene: A lba-offset ",
            psx_io::disc_base::lba_offset(),
        );
        say("hello-scene: A flags ", core_::block_word(block::OFF_FLAGS));
        let mut ok = true;
        common(&mut ok);
        let nested = if psx_io::disc_base::lba_offset() != 0 {
            block::FLAG_NESTED
        } else {
            0
        };
        check(
            &mut ok,
            "flags",
            core_::block_word(block::OFF_FLAGS),
            nested,
        );
        check(
            &mut ok,
            "scenes",
            core_::block_word(block::OFF_SCENES),
            block::NO_SCENE << 16,
        );
        check(
            &mut ok,
            "switches",
            core_::block_word(block::OFF_SWITCHES),
            0,
        );
        // A stream client holds the drive: the switch must be refused and change nothing.
        check(&mut ok, "client-attach", core_::client_attach(), code::OK);
        check(
            &mut ok,
            "client-attach-twice",
            core_::client_attach(),
            code::CLIENT,
        );
        let refused = core_::switch(1, &ARGS);
        say("hello-scene: A switch-with-client ", refused);
        check(&mut ok, "switch-with-client", refused, code::CLIENT);
        core_::client_detach();
        check(
            &mut ok,
            "window-after-refusal",
            core_::window_hash(),
            core_::CORE_HASH,
        );
        if !ok {
            tty::println("hello-scene: A FAIL");
            screen(MAGENTA, 0xaf);
            loop {}
        }
        tty::println("hello-scene: A checks PASS");
        tty::println("hello-scene: A switching");
        mark(0xa1);
        let r = core_::switch(1, &ARGS);
        // Only a refusal comes back.
        say("hello-scene: A switch refused ", r);
        core_::fatal(r)
    }

    #[cfg(feature = "role-b")]
    pub fn run() -> ! {
        mark(0xb0);
        tty::println("hello-scene: B main");
        say("hello-scene: B core-hash ", core_::CORE_HASH);
        say(
            "hello-scene: B lba-offset ",
            psx_io::disc_base::lba_offset(),
        );
        let mut ok = true;
        common(&mut ok);
        let nested = if psx_io::disc_base::lba_offset() != 0 {
            block::FLAG_NESTED
        } else {
            0
        };
        check(
            &mut ok,
            "flags",
            core_::block_word(block::OFF_FLAGS),
            nested | block::FLAG_CORE_ENTRY,
        );
        check(&mut ok, "scenes", core_::block_word(block::OFF_SCENES), 1);
        check(
            &mut ok,
            "switches",
            core_::block_word(block::OFF_SWITCHES),
            1,
        );
        for (i, &want) in ARGS.iter().enumerate() {
            check(
                &mut ok,
                "arg",
                core_::block_word(block::OFF_ARGS + 4 * i as u32),
                want,
            );
        }
        // The core still owns the drive: a blocking read and a stream of the table sector.
        let mut sector = [0u32; 512];
        // SAFETY: one sector's worth of words.
        let r = unsafe { core_::read(table::LBA, 1, sector.as_mut_ptr(), None) };
        check(&mut ok, "read", r, code::OK);
        check(&mut ok, "read-magic", sector[0], table::MAGIC[0]);
        check(&mut ok, "read-row1-id", sector[16 * 2] & 0xffff, 1);
        sector[0] = 0;
        check(&mut ok, "stream-attach", core_::client_attach(), code::OK);
        check(
            &mut ok,
            "read-while-attached",
            unsafe { core_::read(table::LBA, 1, sector.as_mut_ptr(), None) },
            code::CLIENT,
        );
        check(
            &mut ok,
            "stream-open",
            core_::stream_open(table::LBA, 0),
            code::OK,
        );
        // SAFETY: one sector's worth of words.
        check(
            &mut ok,
            "stream-sector",
            unsafe { core_::stream_sector(sector.as_mut_ptr()) },
            code::OK,
        );
        check(&mut ok, "stream-magic", sector[0], table::MAGIC[0]);
        check(&mut ok, "stream-pause", core_::pause(), code::OK);
        core_::client_detach();
        check(
            &mut ok,
            "window-at-end",
            core_::window_hash(),
            core_::CORE_HASH,
        );
        if ok {
            tty::println("hello-scene: B PASS");
            screen(GREEN, 0xb0);
        } else {
            tty::println("hello-scene: B FAIL");
            screen(MAGENTA, 0xbf);
        }
        loop {}
    }
}

#[no_mangle]
fn main() {
    #[cfg(feature = "core")]
    scene::run();
    #[cfg(not(feature = "core"))]
    {
        // The old way: the same program without the core, for the boot-time baseline.
        screen(0xc0_4000, 0xa0);
        mark(0xa0);
        tty::println("hello-scene: A main");
        say(
            "hello-scene: A plain lba-offset ",
            psx_io::disc_base::lba_offset(),
        );
        loop {}
    }
}
