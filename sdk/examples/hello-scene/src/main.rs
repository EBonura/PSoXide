//! `hello-scene` -- two programs, two overlays and the scene core between them.
//!
//! The walk (scene table version 2: scenes 0 and 1 in program A, 2 and 3 the overlays O1 and O2
//! at one address, 4 program B): A's start checks the cold handoff block, reads its scene table
//! into RAM and checks that a switch is refused while a stream client is attached; then
//! 0 -> 1 (A's menu, in RAM), which loads O1 with the load that returns, calls it, and switches
//! 1 -> 2 (O1, in RAM) -> 1 (in RAM) -> 2 (in RAM) -> 3 (O2, read, evicts O1) -> 2 (O1, read again)
//! -> 4 (program B, a program switch). B checks the core entry, the window hash, the block (scenes,
//! counter, arguments, disc bases equal to psx-rt's, the loaded mask) and that the core still reads
//! and streams.
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
    use psx_core::abi::{block, code};
    use psx_core::link as core_;

    const BLUE: u32 = 0xc0_4000;
    const GREEN: u32 = 0x00_c000;
    const MAGENTA: u32 = 0xc0_00c0;

    /// Arguments scene 0 passes; scene 1 checks them.
    const ARGS: [u32; 4] = [0x5ce1_0001, 0x5ce1_0002, 0x5ce1_0003, 0x5ce1_0004];
    /// The handoff block's private words: [1] the table copy's address (the overlays read it),
    /// [2] the root counter 2 value just before a switch (the next scene prints the delta).
    const TABLE_AT: u32 = 0x801f_ff84;
    const T0_AT: u32 = 0x801f_ff88;

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

    fn put(addr: u32, v: u32) {
        // SAFETY: the handoff block's private area.
        unsafe { core::ptr::write_volatile(addr as *mut u32, v) };
    }

    fn timer() -> u32 {
        psx_io::timers::counter(psx_io::timers::Timer::Timer2) as u32
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

    fn verdict(ok: bool, who: &str, mark_ok: u32) {
        tty::print("hello-scene: ");
        tty::print(who);
        if ok {
            tty::println(" PASS");
            screen(GREEN, mark_ok);
        } else {
            tty::println(" FAIL");
            screen(MAGENTA, 0xff);
            loop {}
        }
    }

    /// Print the clocks since the switch that entered this scene asked (root counter 2 at the
    /// system clock, wrapping at 65,536: only an in-RAM switch is short enough to read this way).
    fn since_switch() {
        // SAFETY: the handoff block's private area.
        let t0 = unsafe { core::ptr::read_volatile(T0_AT as *const u32) };
        say(
            "hello-scene: clocks since the switch call ",
            timer().wrapping_sub(t0) & 0xffff,
        );
    }

    #[cfg(not(feature = "role-b"))]
    mod base {
        use super::*;

        /// The program's copy of its scene table (header and rows; 5 scenes and 4 images here).
        pub static mut TABLE: [u32; 96] = [0; 96];

        pub fn table() -> &'static [u32] {
            // SAFETY: written once at boot, before any switch reads it.
            unsafe { &*core::ptr::addr_of!(TABLE) }
        }

        pub fn switch(next: u32, args: &[u32; 4]) -> u32 {
            put(T0_AT, timer());
            core_::switch(next, args, table())
        }
    }

    /// Scene 0: the program's start.
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
        check(&mut ok, "mask", core_::block_word(block::OFF_MASK), 1);
        // SAFETY: once, before any switch reads it.
        let words = core_::read_table(unsafe { &mut *core::ptr::addr_of_mut!(base::TABLE) });
        say(
            "hello-scene: A table words ",
            words.map_or_else(|e| e | 0xe000_0000, |w| w as u32),
        );
        check(&mut ok, "read-table", words.is_ok() as u32, 1);
        put(TABLE_AT, base::table().as_ptr() as u32);
        // A stream client holds the drive: a switch must be refused and change nothing.
        check(&mut ok, "client-attach", core_::client_attach(), code::OK);
        check(
            &mut ok,
            "switch-with-client",
            base::switch(1, &ARGS),
            code::CLIENT,
        );
        core_::client_detach();
        check(
            &mut ok,
            "window-after-refusal",
            core_::window_hash(),
            core_::CORE_HASH,
        );
        verdict(ok, "A checks", 0xa0);
        tty::println("hello-scene: A switching");
        let r = base::switch(1, &ARGS);
        say("hello-scene: A switch refused ", r);
        core_::fatal(r)
    }

    /// Nothing calls the menu: only the scene table names it. Keep it linked.
    #[cfg(not(feature = "role-b"))]
    #[used]
    static MENU_ENTRY: extern "C" fn() -> ! = hello_scene_menu;

    /// Scene 1: a scene of the running program, entered in RAM.
    #[cfg(not(feature = "role-b"))]
    #[no_mangle]
    pub extern "C" fn hello_scene_menu() -> ! {
        since_switch();
        let step = core_::block_word(block::OFF_SWITCHES);
        mark(0xa100 | step);
        tty::print("hello-scene: menu step ");
        tty::print_hex_u32(step);
        tty::print(" mask ");
        tty::print_hex_u32(core_::block_word(block::OFF_MASK));
        tty::println("");
        let mut ok = true;
        check(
            &mut ok,
            "menu-flags",
            core_::block_word(block::OFF_FLAGS) & block::FLAG_IN_PROGRAM,
            block::FLAG_IN_PROGRAM,
        );
        check(
            &mut ok,
            "menu-scene",
            core_::block_word(block::OFF_SCENES) & 0xffff,
            1,
        );
        check(
            &mut ok,
            "menu-window",
            core_::window_hash(),
            core_::CORE_HASH,
        );
        let next = match step {
            1 => {
                check(
                    &mut ok,
                    "menu-args",
                    core_::block_word(block::OFF_ARGS),
                    ARGS[0],
                );
                // Load the first overlay and call it (the load that returns), then load it again
                // (in RAM: no read).
                check(
                    &mut ok,
                    "load-image",
                    core_::load_image(1, base::table()),
                    code::OK,
                );
                // SAFETY: the overlay's call entry, loaded and checked just now.
                let call: extern "C" fn(u32) -> u32 =
                    unsafe { core::mem::transmute(0x8010_0008usize) };
                check(&mut ok, "overlay-call", call(0x1234), 0x0c01_1234);
                check(
                    &mut ok,
                    "load-image-again",
                    core_::load_image(1, base::table()),
                    code::OK,
                );
                // An overlay of another program is not this program's to load.
                check(
                    &mut ok,
                    "load-program-image",
                    core_::load_image(3, base::table()),
                    code::BOUNDS,
                );
                2
            }
            3 => 2,
            _ => 0xff,
        };
        verdict(ok, "menu checks", 0xa1);
        let r = base::switch(next, &[step, 0, 0, 0]);
        say("hello-scene: menu switch refused ", r);
        core_::fatal(r)
    }

    /// Scene 4: the second program, after the walk through the first one's scenes.
    #[cfg(feature = "role-b")]
    pub fn run() -> ! {
        mark(0xb0);
        since_switch();
        tty::println("hello-scene: B main");
        say("hello-scene: B core-hash ", core_::CORE_HASH);
        say(
            "hello-scene: B lba-offset ",
            psx_io::disc_base::lba_offset(),
        );
        say("hello-scene: B mask ", core_::block_word(block::OFF_MASK));
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
        check(
            &mut ok,
            "scenes",
            core_::block_word(block::OFF_SCENES),
            4 | (2 << 16),
        );
        check(
            &mut ok,
            "switches",
            core_::block_word(block::OFF_SWITCHES),
            7,
        );
        check(&mut ok, "arg", core_::block_word(block::OFF_ARGS), 6);
        check(&mut ok, "mask", core_::block_word(block::OFF_MASK), 1 << 3);
        // The core still owns the drive: a blocking read and a stream of the table sector.
        let mut sector = [0u32; 512];
        // SAFETY: one sector's worth of words.
        let r = unsafe { core_::read(psx_core::abi::table::LBA, 1, sector.as_mut_ptr(), None) };
        check(&mut ok, "read", r, code::OK);
        check(
            &mut ok,
            "read-magic",
            sector[0],
            psx_core::abi::table::MAGIC[0],
        );
        sector[0] = 0;
        check(&mut ok, "stream-attach", core_::client_attach(), code::OK);
        check(
            &mut ok,
            "stream-open",
            core_::stream_open(psx_core::abi::table::LBA, 0),
            code::OK,
        );
        // SAFETY: one sector's worth of words.
        check(
            &mut ok,
            "stream-sector",
            unsafe { core_::stream_sector(sector.as_mut_ptr()) },
            code::OK,
        );
        check(
            &mut ok,
            "stream-magic",
            sector[0],
            psx_core::abi::table::MAGIC[0],
        );
        check(&mut ok, "stream-pause", core_::pause(), code::OK);
        // A second Pause right away: the first one's stop is proven before it is sent.
        check(&mut ok, "pause-again", core_::pause(), code::OK);
        core_::client_detach();
        // A raw command after a Pause the core only saw acknowledged: the core proves the drive
        // stopped before it sends the command, so the status it returns shows nothing active.
        let mut resp = [0u8; 16];
        // SAFETY: GetStat takes no parameters and answers with at most 16 bytes.
        let r = unsafe {
            core_::cd(
                psx_core::abi::cd_op::COMMAND,
                0,
                0x01 << 8,
                resp.as_mut_ptr(),
            )
        };
        check(&mut ok, "raw-getstat", r & 0xff, 0);
        check(&mut ok, "raw-getstat-idle", u32::from(resp[0] & 0xe0), 0);
        check(
            &mut ok,
            "window-at-end",
            core_::window_hash(),
            core_::CORE_HASH,
        );
        verdict(ok, "B", 0xb0);
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
