//! `hello-modular` -- a member disc of two programs on the scene core whose scenes declare the
//! images they need (`needs=` in the manifest), nested under a launcher or booted alone.
//!
//! Images, in the member's table order (the index is the loaded-mask bit):
//!
//! | # | image       | what |
//! |---|-------------|------|
//! | 0 | PSX.EXE (P) | the boot program; its overlays run at OVERLAY_VMA |
//! | 1 | Q.EXE (Q)   | the second program (feature `role-q`); its overlay runs over a `.bss` host |
//! | 2 | CARRIED.OVL | 2 KiB of P's own `.data`, carried by P's exe (`carries=`) |
//! | 3 | SVC.OVL     | P's overlay 1, a service image P's start scene needs |
//! | 4 | VOL.OVL     | P's overlay 2, `class=volatile` |
//! | 5 | TAILED.OVL  | P's overlay 3, placed before `WORLD.PAK` with `tail=1` |
//! | 6 | HOSTED.OVL  | Q's overlay 1, over Q's host buffer |
//!
//! Scenes: 0 PSTART (P starts; needs SVC and CARRIED), 1 PPLAY (in P; needs TAILED and CARRIED),
//! 2 PVOL (in P; needs VOL), 3 QSTART (Q starts; needs HOSTED). A disc with a scene named
//! `SOAK<n>` runs the soak instead: P and Q switch to each other n times, each entry loading an
//! overlay, with the pad polled from the core's idle hook and the VBlank counter running.
//!
//! The walk: P checks that SVC is in RAM at its start and that CARRIED was not read again (a
//! canary word it wrote survives), switches to PPLAY, which checks TAILED and its tail (the pack
//! header) and CARRIED, writes over OVERLAY_VMA, switches to PVOL, which checks VOL, writes over it
//! and enters PVOL again (a volatile image is read again), then switches to QSTART, where Q checks
//! HOSTED. A needed image found missing is reported (`hello-modular: MISSING <image>`), loaded by
//! hand so the walk goes on, and counted; Q's verdict is `hello-modular: PASS` with none missing,
//! else `hello-modular: FAIL <n> missing`. Every program prints its entry state first
//! (`hello-modular: entry-state ...`, what psx-rt's entry state guarantees, the same after a BIOS
//! boot and after a core switch) and what may differ (`hello-modular: entry-info ...`).
#![no_std]
#![no_main]

extern crate psx_rt;

use core::hint::black_box;
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use psx_core::{abi, link};
use psx_rt::tty;

/// Image indices in the member's table.
#[allow(dead_code, reason = "each program uses its own")]
mod image {
    pub const CARRIED: u32 = 2;
    #[cfg(not(feature = "role-q"))]
    pub const SVC: u32 = 3;
    #[cfg(not(feature = "role-q"))]
    pub const VOL: u32 = 4;
    #[cfg(not(feature = "role-q"))]
    pub const TAILED: u32 = 5;
    #[cfg(feature = "role-q")]
    pub const HOSTED: u32 = 6;
}

/// Scene ids in the member's table.
#[allow(dead_code, reason = "each program uses its own")]
mod scene {
    pub const PSTART: u32 = 0;
    #[cfg(not(feature = "role-q"))]
    pub const PPLAY: u32 = 1;
    #[cfg(not(feature = "role-q"))]
    pub const PVOL: u32 = 2;
    pub const QSTART: u32 = 3;
}

/// The argument words a switch hands on (the block's 0x20): what the walk carries across scenes
/// and programs (statics do not survive a program switch).
mod arg {
    /// 0 the walk, 1 the soak.
    pub const MODE: usize = 0;
    /// Needed images found missing so far.
    pub const MISSING: usize = 1;
    /// The walk: PVOL's round. The soak: entries so far.
    pub const COUNT: usize = 2;
    /// The VBlank count before an in-program switch (the walk); pad presses seen (the soak).
    pub const EXTRA: usize = 3;
}

const MODE_SOAK: u32 = 1;

/// The program's copy of the member's scene table.
static mut TABLE: [u32; 160] = [0; 160];

fn table() -> &'static mut [u32] {
    // SAFETY: single-threaded; every caller uses the reference at once and keeps none.
    unsafe { &mut *addr_of_mut!(TABLE) }
}

fn args() -> [u32; 4] {
    let at = abi::block::OFF_ARGS;
    [
        link::block_word(at),
        link::block_word(at + 4),
        link::block_word(at + 8),
        link::block_word(at + 12),
    ]
}

fn fail(what: &str, value: u32) -> ! {
    tty::print("hello-modular: FAIL ");
    tty::print(what);
    tty::print(" ");
    tty::print_hex_u32(value);
    tty::println("");
    loop {}
}

fn say(what: &str, value: u32) {
    tty::print("hello-modular: ");
    tty::print(what);
    tty::print(" ");
    tty::print_hex_u32(value);
    tty::println("");
}

fn switch(id: u32, args: [u32; 4]) -> ! {
    let r = link::switch(id, &args, table());
    fail("switch refused", r)
}

/// A word an image puts at a known address: its signature, read before anything calls into it.
fn word_at(at: *const u32) -> u32 {
    // SAFETY: an address inside this program's own RAM (an overlay's range or its own .data).
    unsafe { read_volatile(black_box(at)) }
}

/// Count `image` missing (its signature not where it must be), and load it by hand so the walk
/// goes on.
fn need(image: u32, name: &str, signature: *const u32, want: u32, missing: &mut u32) {
    if word_at(signature) == want {
        return;
    }
    *missing += 1;
    tty::print("hello-modular: MISSING ");
    tty::println(name);
    let r = link::load_image(image, table());
    if r != 0 {
        fail("hand load", r);
    }
    if word_at(signature) != want {
        fail("signature after the hand load", word_at(signature));
    }
}

// ---------------------------------------------------------------------------------------------
// Entry state.

mod entry {
    use super::*;
    use psx_hw::gpu;

    fn r32(addr: u32) -> u32 {
        // SAFETY: a fixed MMIO register.
        unsafe { read_volatile(addr as *const u32) }
    }
    fn r16(addr: u32) -> u32 {
        // SAFETY: a fixed MMIO register.
        u32::from(unsafe { read_volatile(addr as *const u16) })
    }
    fn w32(addr: u32, v: u32) {
        // SAFETY: a fixed MMIO register.
        unsafe { write_volatile(addr as *mut u32, v) }
    }

    fn info(index: u32) -> u32 {
        w32(gpu::GP1, gpu::gp1::get_gpu_info(index));
        r32(gpu::GPUREAD) & 0x00ff_ffff
    }

    /// Words of VRAM rectangle `x, y, w, h` (w even): how many are not zero, and their word FNV.
    fn vram(x: u32, y: u32, w: u32, h: u32) -> (u32, u32) {
        while r32(gpu::GPUSTAT) & (1 << 26) == 0 {}
        w32(gpu::GP0, gpu::gp0::COPY_VRAM_TO_CPU);
        w32(gpu::GP0, y << 16 | x);
        w32(gpu::GP0, h << 16 | w);
        while r32(gpu::GPUSTAT) & (1 << 27) == 0 {}
        let (mut nonzero, mut hash) = (0u32, 0x811c_9dc5u32);
        let mut n = 0;
        while n < w * h / 2 {
            let word = r32(gpu::GPUREAD);
            if word != 0 {
                nonzero += 1;
            }
            hash = (hash ^ word).wrapping_mul(0x0100_0193);
            n += 1;
        }
        (nonzero, hash)
    }

    fn field(name: &str, value: u32) {
        tty::print(" ");
        tty::print(name);
        tty::print("=");
        tty::print_hex_u32(value);
    }

    /// Print what psx-rt's entry state guarantees (`entry-state`, compared run against run) and
    /// what it leaves as found (`entry-info`).
    pub fn probe(who: &str) {
        use psx_hw::{dma, irq, spu, timers};
        let stat = r32(gpu::GPUSTAT);
        let (outside_a, _) = vram(320, 0, 704, 512);
        let (outside_b, _) = vram(0, 240, 320, 272);
        let (_, frame) = vram(0, 0, 320, 240);
        tty::print("hello-modular: entry-state");
        // GPUSTAT bits 0..15: the draw mode (E1h) and the mask bits (E6h).
        field("draw", stat & 0xffff);
        field("texwin", info(2));
        field("area0", info(3));
        field("area1", info(4));
        field("offset", info(5));
        field("vram-outside-frame", outside_a + outside_b);
        field("spucnt", r16(spu::SPUCNT));
        field(
            "rvol",
            r16(spu::REVERB_VOL_LEFT) | r16(spu::REVERB_VOL_RIGHT) << 16,
        );
        field(
            "cdvol",
            r16(spu::CD_VOL_LEFT) | r16(spu::CD_VOL_RIGHT) << 16,
        );
        field("rbase", r16(spu::REVERB_WORK_BASE));
        field("imask", r32(irq::I_MASK) & 0x7ff);
        field("dpcr", r32(dma::DPCR));
        field("dicr", r32(dma::DICR) & 0x00ff_803f);
        let mut t = 0;
        while t < timers::COUNT {
            field("timer", r16(timers::block(t) + timers::MODE) & 0x3ff);
            t += 1;
        }
        tty::println("");
        tty::print("hello-modular: entry-info ");
        tty::print(who);
        field("display", stat >> 16 & 0xff);
        field("frame", frame);
        field("flags", link::block_word(abi::block::OFF_FLAGS));
        field("switches", link::block_word(abi::block::OFF_SWITCHES));
        tty::println("");
    }
}

// ---------------------------------------------------------------------------------------------
// The pad, from the core's idle hook: once per VBlank, while the core waits on the drive.

static mut POLLS: u32 = 0;
static mut PRESSES: u32 = 0;
static mut LAST_VBLANK: u32 = 0;
static mut PADS: psx_pad::Controllers = psx_pad::Controllers::new();

extern "C" fn idle() {
    let now = psx_rt::interrupts::vblank_count();
    // SAFETY: the hook runs from the main loop's core calls only, never nested.
    unsafe {
        if now == read_volatile(addr_of!(LAST_VBLANK)) {
            return;
        }
        write_volatile(addr_of_mut!(LAST_VBLANK), now);
        write_volatile(addr_of_mut!(POLLS), read_volatile(addr_of!(POLLS)) + 1);
        // The controller port is this hook's alone: the program polls nothing anywhere else.
        let mut port = psx_io::periph::ControllerPort::steal();
        let pads = &mut *addr_of_mut!(PADS);
        if pads.poll(&mut port, psx_pad::Port::One).buttons.bits() != 0 {
            write_volatile(addr_of_mut!(PRESSES), read_volatile(addr_of!(PRESSES)) + 1);
        }
    }
}

fn presses() -> u32 {
    // SAFETY: as above.
    unsafe { read_volatile(addr_of!(PRESSES)) }
}

#[cfg(not(feature = "role-q"))]
/// The soak's length: n of a scene named `SOAK<n>` in the table, `None` on the walk's discs.
fn soak_length() -> Option<u32> {
    let t = table();
    let scenes = t[3] & 0xffff;
    let mut i = 0;
    while i < scenes {
        let at = (16 + 8 * i) as usize;
        let name = &t[at + 4..at + 8];
        let bytes = |k: usize| (name[k / 4] >> (8 * (k % 4))) as u8;
        if bytes(0) == b'S' && bytes(1) == b'O' && bytes(2) == b'A' && bytes(3) == b'K' {
            let mut n = 0u32;
            let mut k = 4;
            while k < 16 && bytes(k).is_ascii_digit() {
                n = n * 10 + u32::from(bytes(k) - b'0');
                k += 1;
            }
            return Some(n);
        }
        i += 1;
    }
    None
}

/// Common start of both programs: entry state, window, VBlank counter, idle hook, table.
fn start(who: &str) {
    // The soak enters hundreds of times: its entries skip the probe (a whole VRAM read).
    if args()[arg::MODE] != MODE_SOAK {
        entry::probe(who);
    }
    if link::window_hash() != link::CORE_HASH {
        fail("window", link::window_hash());
    }
    psx_rt::interrupts::install_vblank_counter();
    link::set_idle_hook(Some(idle));
    if let Err(r) = link::read_table(table()) {
        fail("table", r);
    }
}

/// The program asks the core for what its start scene needs. This core has no such call yet, so
/// this asks nothing and the checks below find the images missing.
fn require() {}

// ---------------------------------------------------------------------------------------------
// The carried image: 2 KiB of P's .data, the same bytes as CARRIED.OVL.

#[cfg(not(feature = "role-q"))]
#[repr(C, align(2048))]
struct Carried([u32; 512]);

#[cfg(not(feature = "role-q"))]
const CARRIED_CANARY: u32 = 0x0c0f_fee0;

#[cfg(not(feature = "role-q"))]
#[no_mangle]
#[link_section = ".data.hello_modular_carried"]
static mut CARRIED: Carried = {
    let mut w = [0u32; 512];
    let mut i = 0;
    while i < 512 {
        w[i] = 0xca77_0000 | i as u32;
        i += 1;
    }
    Carried(w)
};

#[cfg(not(feature = "role-q"))]
fn carried_word(i: usize) -> *mut u32 {
    // SAFETY: an element of the static, by raw pointer.
    unsafe { addr_of_mut!(CARRIED.0[i]) }
}

#[cfg(not(feature = "role-q"))]
/// CARRIED holds its bytes and the canary written before the require: carried, never read again.
fn check_carried(missing: &mut u32) {
    need(
        image::CARRIED,
        "CARRIED",
        carried_word(1),
        0xca77_0001,
        missing,
    );
    if word_at(carried_word(511)) != CARRIED_CANARY {
        fail(
            "CARRIED read again (canary gone)",
            word_at(carried_word(511)),
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Program P.

#[cfg(not(feature = "role-q"))]
mod p {
    use super::*;

    extern "C" {
        static __overlay_vma: u32;
        static __overlay_end: u32;
        static __overlay_3_bytes: u32;
    }

    #[link_section = ".rodata.overlay.1.signature"]
    static SVC_SIGNATURE: u32 = 0x5356_4331;
    #[link_section = ".rodata.overlay.2.signature"]
    static VOL_SIGNATURE: u32 = 0x564f_4c32;
    #[link_section = ".rodata.overlay.3.signature"]
    static TAILED_SIGNATURE: u32 = 0x5441_4933;

    #[inline(never)]
    #[link_section = ".text.overlay.1.svc"]
    fn svc(x: u32) -> u32 {
        black_box(x).wrapping_mul(3) ^ SVC_SIGNATURE
    }
    #[inline(never)]
    #[link_section = ".text.overlay.2.vol"]
    fn vol(x: u32) -> u32 {
        black_box(x).wrapping_mul(5) ^ VOL_SIGNATURE
    }
    #[inline(never)]
    #[link_section = ".text.overlay.3.tailed"]
    fn tailed(x: u32) -> u32 {
        black_box(x).wrapping_mul(7) ^ TAILED_SIGNATURE
    }

    /// "PSOXWPAK", the pack header's magic.
    const PACK_MAGIC: [u32; 2] = [0x584f_5350, 0x4b41_5057];

    /// Write over every word of OVERLAY_VMA's range, as the owner's world does once play starts.
    fn scribble() {
        // Link-time symbols: only their addresses are taken.
        let start = addr_of!(__overlay_vma) as *mut u32;
        let end = addr_of!(__overlay_end) as *mut u32;
        let mut at = start;
        while at < end {
            // SAFETY: psoxide-overlay.ld's run range, this program's own memory.
            unsafe { write_volatile(at, 0xffff_ffff) };
            at = at.wrapping_add(1);
        }
        psx_rt::cache::flush_instruction_cache_range(start as u32, end as u32);
    }

    pub fn main() -> ! {
        start("P");
        let a = args();
        if let Some(n) = soak_length() {
            soak(n, a);
        }
        let mut missing = a[arg::MISSING];
        // The canary: CARRIED is carried (in RAM since P's start), so its walk reads nothing.
        // SAFETY: a word of P's own .data.
        unsafe { write_volatile(carried_word(511), CARRIED_CANARY) };
        require();
        need(
            image::SVC,
            "SVC",
            addr_of!(SVC_SIGNATURE),
            0x5356_4331,
            &mut missing,
        );
        if svc(2) != 6 ^ 0x5356_4331 {
            fail("svc", svc(2));
        }
        check_carried(&mut missing);
        tty::println("hello-modular: P start scene ok");
        switch(
            scene::PPLAY,
            [0, missing, 0, psx_rt::interrupts::vblank_count()],
        )
    }

    /// Nothing calls the in-program scenes: only the scene table names them. Keep them linked.
    #[used]
    static ENTRIES: [extern "C" fn() -> !; 2] = [hello_modular_pplay, hello_modular_pvol];

    /// PPLAY, an in-program scene of P: needs TAILED (and its tail) and CARRIED.
    #[no_mangle]
    extern "C" fn hello_modular_pplay() -> ! {
        let a = args();
        let mut missing = a[arg::MISSING];
        let loads_before = psx_rt::interrupts::vblank_count();
        need(
            image::TAILED,
            "TAILED",
            addr_of!(TAILED_SIGNATURE),
            0x5441_4933,
            &mut missing,
        );
        // The core's load (the walk's, or the hand load) ran with VBlank on: the counter moved.
        if psx_rt::interrupts::vblank_count() == a[arg::EXTRA] && loads_before == a[arg::EXTRA] {
            fail("no VBlank during the scene's load", a[arg::EXTRA]);
        }
        if tailed(3) != 21 ^ 0x5441_4933 {
            fail("tailed", tailed(3));
        }
        // Link-time symbol: only its address is taken (the overlay file's size).
        let own = addr_of!(__overlay_3_bytes) as u32;
        match link::image_tail(image::TAILED, own, table()) {
            Some(tail) if tail[..2] == PACK_MAGIC => {}
            Some(tail) => fail("tail magic", tail[0]),
            None => fail("no tail", 0),
        }
        check_carried(&mut missing);
        tty::println("hello-modular: PPLAY ok");
        scribble();
        switch(scene::PVOL, [0, missing, 0, 0])
    }

    /// PVOL, an in-program scene of P: needs VOL, which is volatile, so every entry reads it.
    #[no_mangle]
    extern "C" fn hello_modular_pvol() -> ! {
        let a = args();
        let mut missing = a[arg::MISSING];
        need(
            image::VOL,
            "VOL",
            addr_of!(VOL_SIGNATURE),
            0x564f_4c32,
            &mut missing,
        );
        if vol(4) != 20 ^ 0x564f_4c32 {
            fail("vol", vol(4));
        }
        check_carried(&mut missing);
        if a[arg::COUNT] == 0 {
            tty::println("hello-modular: PVOL ok, again over the written range");
            scribble();
            switch(scene::PVOL, [0, missing, 1, 0]);
        }
        tty::println("hello-modular: PVOL read again");
        switch(scene::QSTART, [0, missing, 0, 0])
    }

    /// The soak: an overlay load, then over to Q, `n` entries in all.
    fn soak(n: u32, a: [u32; 4]) -> ! {
        let (count, missing, pressed) = if a[arg::MODE] == MODE_SOAK {
            (a[arg::COUNT], a[arg::MISSING], a[arg::EXTRA])
        } else {
            (0, 0, 0)
        };
        if count >= n {
            say("soak pad presses", pressed + presses());
            if missing != 0 {
                say("SOAK FAIL needed images found missing", missing);
                loop {}
            }
            say("SOAK PASS entries", count);
            loop {}
        }
        let r = link::load_image(image::VOL, table());
        if r != 0 || word_at(addr_of!(VOL_SIGNATURE)) != 0x564f_4c32 {
            fail("soak VOL", r);
        }
        if vol(1) != 5 ^ 0x564f_4c32 {
            fail("soak vol", vol(1));
        }
        scribble();
        switch(
            scene::QSTART,
            [MODE_SOAK, missing, count + 1, pressed + presses()],
        )
    }
}

// ---------------------------------------------------------------------------------------------
// Program Q.

#[cfg(feature = "role-q")]
mod q {
    use super::*;

    extern "C" {
        static __overlay_vma: u32;
    }

    /// Q's world memory: the host buffer its overlay runs over.
    #[link_section = ".bss.overlay_host"]
    static mut WORLD: [u32; 2048] = [0; 2048];

    #[link_section = ".rodata.overlay.1.signature"]
    static HOSTED_SIGNATURE: u32 = 0x484f_5354;

    #[inline(never)]
    #[link_section = ".text.overlay.1.hosted"]
    fn hosted(x: u32) -> u32 {
        black_box(x).wrapping_mul(11) ^ HOSTED_SIGNATURE
    }

    pub fn main() -> ! {
        start("Q");
        // Two symbols, one address: hidden from the compiler.
        let world = black_box(addr_of!(WORLD) as u32);
        // Link-time symbol: only its address is taken.
        if world != addr_of!(__overlay_vma) as u32 {
            fail("host address", world);
        }
        let a = args();
        let mut missing = a[arg::MISSING];
        require();
        need(
            image::HOSTED,
            "HOSTED",
            addr_of!(HOSTED_SIGNATURE),
            0x484f_5354,
            &mut missing,
        );
        if hosted(5) != 55 ^ 0x484f_5354 {
            fail("hosted", hosted(5));
        }
        if a[arg::MODE] == MODE_SOAK {
            // The world takes the host buffer back: the next entry reads HOSTED again.
            let mut i = 0;
            while i < 2048 {
                // SAFETY: Q's own host buffer, by raw pointer.
                unsafe { write_volatile(addr_of_mut!(WORLD[i]), 0xffff_ffff) };
                i += 1;
            }
            switch(
                scene::PSTART,
                [
                    MODE_SOAK,
                    missing,
                    a[arg::COUNT] + 1,
                    a[arg::EXTRA] + presses(),
                ],
            );
        }
        if missing == 0 {
            tty::println("hello-modular: PASS");
        } else {
            say("FAIL missing", missing);
        }
        loop {}
    }
}

#[no_mangle]
fn main() {
    #[cfg(not(feature = "role-q"))]
    p::main();
    #[cfg(feature = "role-q")]
    q::main();
}
