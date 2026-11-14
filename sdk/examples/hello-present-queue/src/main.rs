//! `hello-present-queue` -- whole frames through `psx_rt::present::Presenter`,
//! checked edge by edge.
//!
//! Four phases of `PHASE_FRAMES` frames each: the cap at 60, 30 and 20 fps,
//! then 60 with a CPU load that varies from frame to frame (0 to 2 VBlanks of
//! spinning). A frame is one chain (an ordering table ending on a recorded HUD
//! and GP0(1Fh)) or two (the table as a `Part::More`, the HUD as the
//! `Part::Final`); the presenter links the draw target and clear in front.
//!
//! A VBlank hook records, at every edge, how many parts have been kicked and
//! how many frames flipped, and counts a tear when a frame started drawing
//! before the one before it was on screen, or a walk was running while the
//! display showed the buffer it draws into. Afterwards each frame gets the tick
//! it was published, kicked and flipped, and the run fails unless:
//!
//! - every part was kicked once and every frame flipped once, in order;
//! - no tear, no stall recovery, no recording overflow, no fault;
//! - consecutive flips are at least the phase's cap apart;
//! - each frame flipped no later than two edges after it was published, or the
//!   cap's edge after the frame before, whichever is later: a finished frame
//!   never waits for its successor.
//!
//! The verdict goes to the TTY (`PRESENT-QUEUE PASS ...` or
//! `PRESENT-QUEUE FAIL ...`, one `PRESENT-QUEUE PHASE ...` line per phase with
//! flips, VBlanks, the most VBlanks from publish to flip, and the frames the
//! thread kicked at publish) and on screen; `tools/present_gate.sh` reads it.

#![no_std]
#![no_main]

extern crate psx_rt;

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use psx_font::fonts::BASIC;
use psx_font::FontAtlas;
use psx_gpu::chain::DRAW_DONE_NODE;
use psx_gpu::display::{DisplayConfig, DoubleBuffer, Resolution, VideoMode};
use psx_gpu::ot::OrderingTable;
use psx_gpu::prim::{FillRect, TriGouraud};
use psx_gpu::Gpu;
use psx_io::gpu::start_recording_raw;
use psx_rt::present::{self, Part, PresentCap, Presenter};
use psx_rt::{dispatch, interrupts, tty};
use psx_vram::{Clut, TextureDepth, TexturePage};

const FONT_TPAGE: TexturePage = TexturePage::new(320, 0, TextureDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);
const WHITE: (u8, u8, u8) = (220, 220, 230);
const GREEN: (u8, u8, u8) = (80, 220, 100);
const RED: (u8, u8, u8) = (230, 80, 80);

/// Frames in each phase.
const PHASE_FRAMES: usize = 60;
/// The phases' caps: 60, 30 and 20 fps, then 60 under a varying load.
const PHASE_CAPS: [u8; 4] = [1, 2, 3, 1];
const FRAMES: usize = PHASE_FRAMES * PHASE_CAPS.len();
/// Most triangles in one frame; frame `f` draws `f % (TRIS + 1)`.
const TRIS: usize = 4;
/// Edges the hook can record.
const MAX_EDGES: usize = 1024;

const EMPTY: TriGouraud = TriGouraud::new([(0, 0); 3], [(0, 0, 0); 3]);
// Frame N is built while N-1 may still walk, so every buffer a chain links
// comes in two.
static mut TABLES: [OrderingTable<8>; 2] = [OrderingTable::new(), OrderingTable::new()];
static mut PACKETS: [[TriGouraud; TRIS]; 2] = [[EMPTY; TRIS], [EMPTY; TRIS]];
static mut HUDS: [[u32; 1024]; 2] = [[0; 1024]; 2];

/// Written by the thread before a frame's first part is submitted: the index
/// of that part among every part of the run. The hook reads it.
static mut FIRST_PART: [u32; FRAMES + 1] = [u32::MAX; FRAMES + 1];

/// The hook's state: whether it records, the edge of the run's start and the
/// counts it subtracts, its cursor over `FIRST_PART`, the tears it saw, and
/// per edge the parts kicked (low half) and frames flipped (high half).
static mut RECORDING: bool = false;
static mut EDGE_BASE: u32 = 0;
static mut KICK_BASE: u32 = 0;
static mut FLIP_BASE: u32 = 0;
static mut STARTED: u32 = 0;
static mut TEARS: u32 = 0;
static mut EDGES: [u32; MAX_EDGES] = [0; MAX_EDGES];

fn flips_total() -> u32 {
    interrupts::flip_histogram().iter().sum()
}

/// Once per VBlank, after psx-rt's flip and kick.
fn hook() {
    // SAFETY: the hook is the only writer of its counters and `EDGES` while
    // `RECORDING` is set; the thread reads them after clearing it. Every access
    // is a volatile aligned word access through a raw pointer.
    unsafe {
        if !read_volatile(addr_of!(RECORDING)) {
            return;
        }
        let edge = interrupts::vblank_count().wrapping_sub(read_volatile(addr_of!(EDGE_BASE)));
        let kicks = present::stats()
            .kick_count
            .wrapping_sub(read_volatile(addr_of!(KICK_BASE)));
        let flips = flips_total().wrapping_sub(read_volatile(addr_of!(FLIP_BASE)));
        let mut started = read_volatile(addr_of!(STARTED));
        while (started as usize) < FRAMES
            && read_volatile(addr_of!(FIRST_PART[started as usize])) < kicks
        {
            started += 1;
        }
        write_volatile(addr_of_mut!(STARTED), started);
        // A frame starts only once the one before is shown, so at most one
        // frame is started and not flipped; and a running walk draws that
        // frame, into the buffer not on screen.
        let walking = psx_io::dma::is_busy(psx_io::dma::Channel::Gpu);
        if started > flips + 1 || (walking && started != flips + 1) {
            write_volatile(addr_of_mut!(TEARS), read_volatile(addr_of!(TEARS)) + 1);
        }
        if (edge as usize) < MAX_EDGES {
            write_volatile(
                addr_of_mut!(EDGES[edge as usize]),
                (kicks & 0xffff) | (flips << 16),
            );
        }
    }
}

/// Build frame `f`'s triangles into slots 1..=6, leaving slot 0 for the HUD.
fn build_frame(table: &mut OrderingTable<8>, packets: &mut [TriGouraud; TRIS], f: usize) {
    let mut frame = table.frame();
    let count = f % (TRIS + 1);
    for (t, packet) in packets.iter_mut().take(count).enumerate() {
        let shade = ((t * 20 + f * 3) & 0x7f) as u8 + 0x20;
        let verts = if t & 1 == 0 {
            [(0, 0), (319, 0), (0, 199)]
        } else {
            [(319, 199), (0, 199), (319, 0)]
        };
        *packet = TriGouraud::new(verts, [(shade, 0, 40), (0, shade, 40), (40, 0, shade)]);
        frame.add(1 + t % 6, packet);
    }
}

/// Spin until `vblanks` edges have passed and then a little more, for the
/// varying-load phase.
fn load(f: usize) {
    let vblanks = (f * 7 % 5) as u32 / 2;
    let start = interrupts::vblank_count();
    while interrupts::vblank_count().wrapping_sub(start) < vblanks {}
    for _ in 0..(f * 13 % 7) * 2_000 {
        core::hint::spin_loop();
    }
}

/// `label=XXXXXXXX` for the TTY and the screen.
fn line(label: &str, value: u32) -> ([u8; 32], usize) {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    let mut text = [b' '; 32];
    let label = &label.as_bytes()[..label.len().min(22)];
    text[..label.len()].copy_from_slice(label);
    text[label.len()] = b'=';
    for i in 0..8 {
        text[label.len() + 1 + i] = DIGITS[(value >> (28 - 4 * i) & 0xf) as usize];
    }
    (text, label.len() + 9)
}

fn as_str(text: &[u8]) -> &str {
    core::str::from_utf8(text).unwrap_or("?")
}

fn print_line(label: &str, value: u32) {
    let (text, len) = line(label, value);
    tty::print(" ");
    tty::print(as_str(&text[..len]));
}

/// The first recorded edge at which `field` of the edge word reaches `at_least`.
fn first_edge(shift: u32, at_least: u32) -> Option<u32> {
    (0..MAX_EDGES).find_map(|edge| {
        // SAFETY: a volatile read; the hook no longer writes (recording ended).
        let word = unsafe { read_volatile(addr_of!(EDGES[edge])) };
        ((word >> shift) & 0xffff >= at_least).then_some(edge as u32)
    })
}

#[no_mangle]
fn main() {
    interrupts::install_vblank_counter();
    let Some(peripherals) = psx_rt::Peripherals::take() else {
        return;
    };
    let mut gpu = Gpu::new(
        peripherals.gpu_dma,
        DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240),
    );
    let mut fb = DoubleBuffer::new(Resolution::R320X240);
    fb.apply_draw_target(&mut gpu);
    let font = FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT);
    gpu.wait_idle();
    let hooked = dispatch::on_vblank(hook).is_ok();

    let mut presenter = Presenter::new(Some((8, 24, 8)));
    let before = present::stats();
    let mut overflows = 0u32;
    let mut parts = 0u32;
    let mut publish = [0u32; FRAMES];
    let mut thread_kicked = [false; FRAMES];
    // SAFETY: the hook reads these only while `RECORDING` is set.
    unsafe {
        write_volatile(addr_of_mut!(KICK_BASE), before.kick_count);
        write_volatile(addr_of_mut!(FLIP_BASE), flips_total());
        write_volatile(addr_of_mut!(EDGE_BASE), interrupts::vblank_count());
        write_volatile(addr_of_mut!(RECORDING), true);
    }
    for (f, (published, kicked_at_publish)) in
        publish.iter_mut().zip(thread_kicked.iter_mut()).enumerate()
    {
        let phase = f / PHASE_FRAMES;
        if f % PHASE_FRAMES == 0 {
            // A new cap applies from the next flip on: let the last phase's
            // frames go out under their own.
            present::wait_idle();
            present::set_cap(PresentCap::every_vblanks(PHASE_CAPS[phase]));
        }
        let two_parts = match phase {
            0 => false,
            1 => true,
            _ => f % 3 == 1,
        };
        let side = f & 1;
        // The frame two back linked this side's buffers.
        present::wait_arena_free();
        if phase == 3 {
            load(f);
        }
        // SAFETY: `wait_arena_free` guarantees no walk reads this side, and
        // these are the only references made to it this frame.
        let (table, packets, hud) = unsafe {
            (
                &mut (*addr_of_mut!(TABLES))[side],
                &mut (*addr_of_mut!(PACKETS))[side],
                &mut (*addr_of_mut!(HUDS))[side],
            )
        };
        build_frame(table, packets, f);

        // SAFETY: `hud` is static and not written again until
        // `wait_arena_free` says its walk has ended.
        let recording = unsafe { start_recording_raw(hud.as_mut_ptr(), hud.len()) };
        gpu.draw(&FillRect::new(
            (0, fb.draw_origin().1 + 200),
            (320, 40),
            (20, 24, 60),
        ));
        font.draw_text(8, 212, "QUEUED HUD", WHITE);
        let hud_head = match recording.end() {
            // SAFETY: the recording and the static GP0(1Fh) node live as long
            // as `hud`.
            Ok(Some(recording)) => unsafe {
                recording.link_to(DRAW_DONE_NODE.as_ptr());
                recording.head()
            },
            _ => {
                overflows += 1;
                DRAW_DONE_NODE.as_ptr()
            }
        };
        // SAFETY: the hook reads this entry only once kicks pass it.
        unsafe { write_volatile(addr_of_mut!(FIRST_PART[f]), parts) };
        let threads_before = present::stats().thread_kick_count;
        // SAFETY: every chain below is static and stays untouched until
        // `wait_arena_free` frees its side two frames from now; the table
        // holds no GP0(1Fh) and the HUD ends on it.
        unsafe {
            if two_parts {
                presenter.submit(&mut fb, table.submit_head(), Part::More);
                *kicked_at_publish = present::stats().thread_kick_count != threads_before;
                presenter.submit(&mut fb, hud_head, Part::Final);
                parts += 2;
            } else {
                table.end_with_chain(hud_head);
                presenter.submit(&mut fb, table.submit_head(), Part::Final);
                *kicked_at_publish = present::stats().thread_kick_count != threads_before;
                parts += 1;
            }
        }
        *published = interrupts::vblank_count();
    }
    present::wait_idle();
    // Two more edges so the hook sees the last flip.
    interrupts::wait_vblank();
    interrupts::wait_vblank();
    // SAFETY: the hook stops writing once this is clear.
    let (tears, edge_base) = unsafe {
        write_volatile(addr_of_mut!(RECORDING), false);
        (
            read_volatile(addr_of!(TEARS)),
            read_volatile(addr_of!(EDGE_BASE)),
        )
    };
    let after = present::stats();
    let kicks = after.kick_count.wrapping_sub(before.kick_count);
    let recoveries = after.recovery_count.wrapping_sub(before.recovery_count);

    // Per frame: flipped at which edge, how long after publish.
    let mut lost = 0u32;
    let mut too_soon = 0u32;
    let mut late = 0u32;
    let mut phase_flips = [0u32; PHASE_CAPS.len()];
    let mut phase_span = [0u32; PHASE_CAPS.len()];
    let mut phase_worst = [0u32; PHASE_CAPS.len()];
    let mut phase_thread = [0u32; PHASE_CAPS.len()];
    let mut previous: Option<u32> = None;
    let mut phase_first = [0u32; PHASE_CAPS.len()];
    for f in 0..FRAMES {
        let phase = f / PHASE_FRAMES;
        let cap = u32::from(PHASE_CAPS[phase]);
        let published = publish[f].wrapping_sub(edge_base);
        let Some(flipped) = first_edge(16, f as u32 + 1) else {
            lost += 1;
            continue;
        };
        phase_flips[phase] += 1;
        if f % PHASE_FRAMES == 0 {
            phase_first[phase] = flipped;
        }
        phase_span[phase] = flipped.wrapping_sub(phase_first[phase]);
        phase_worst[phase] = phase_worst[phase].max(flipped.wrapping_sub(published));
        phase_thread[phase] += u32::from(thread_kicked[f]);
        if let Some(previous) = previous {
            if f % PHASE_FRAMES != 0 && flipped.wrapping_sub(previous) < cap {
                too_soon += 1;
                tty::print("PRESENT-QUEUE too soon");
                print_line("frame", f as u32);
                print_line("gap", flipped.wrapping_sub(previous));
                tty::println("");
            }
            // The first frame of a phase waits out the hold the last one armed
            // with the old cap.
            if f % PHASE_FRAMES != 0 && flipped > (published + 2).max(previous + cap) {
                late += 1;
                tty::print("PRESENT-QUEUE late");
                print_line("frame", f as u32);
                print_line("published", published);
                print_line("previous", previous);
                print_line("flipped", flipped);
                tty::println("");
            }
        }
        previous = Some(flipped);
    }

    let checks = [
        (hooked, "no VBlank hook slot"),
        (kicks == parts, "parts not each kicked once"),
        (lost == 0, "frames never flipped"),
        (tears == 0, "a frame drew into the buffer on screen"),
        (too_soon == 0, "a flip came sooner than the cap"),
        // Without GPUSTAT bit 24 (an FPGA core that loses GP0(1Fh)) every flip
        // waits for the draw-done fallback, one VBlank more by design.
        (
            late == 0 || interrupts::draw_done_fallback_count() != 0,
            "a finished frame waited past its edge",
        ),
        (recoveries == 0, "present queue stalled"),
        (overflows == 0, "recording overflowed"),
        (interrupts::fault_count() == 0, "exceptions other than IRQs"),
    ];
    let failures = checks.iter().filter(|check| !check.0).count() as u32;
    for (_, what) in checks.iter().filter(|check| !check.0) {
        tty::print("PRESENT-QUEUE check failed: ");
        tty::println(what);
    }
    for phase in 0..PHASE_CAPS.len() {
        tty::print("PRESENT-QUEUE PHASE");
        print_line("cap", u32::from(PHASE_CAPS[phase]));
        print_line("flips", phase_flips[phase]);
        print_line("vblanks", phase_span[phase]);
        print_line("worst", phase_worst[phase]);
        print_line("threadkicks", phase_thread[phase]);
        tty::println("");
    }
    let lines = [
        line("failures", failures),
        line("frames", FRAMES as u32),
        line("parts", parts),
        line("kicks", kicks),
        line(
            "threadkicks",
            after.thread_kick_count - before.thread_kick_count,
        ),
        line("tears", tears),
        line("late", late),
        line("toosoon", too_soon),
        line("fallbacks", interrupts::draw_done_fallback_count()),
    ];
    let (banner, tint) = if failures == 0 {
        ("PRESENT-QUEUE PASS", GREEN)
    } else {
        ("PRESENT-QUEUE FAIL", RED)
    };
    tty::print(banner);
    for (text, len) in &lines {
        tty::print(" ");
        tty::print(as_str(&text[..*len]));
    }
    tty::println("");

    // The result screen: drawn through the port, presented with `present_drawn`.
    loop {
        fb.clear(&mut gpu, (10, 12, 20));
        font.draw_text(8, 6, "ONE PRESENT PATH", WHITE);
        font.draw_text(8, 30, banner, tint);
        for (row, (text, len)) in lines.iter().enumerate() {
            font.draw_text(8, 54 + 12 * row as i16, as_str(&text[..*len]), WHITE);
        }
        let mut y = 162;
        for (_, what) in checks.iter().filter(|check| !check.0) {
            font.draw_text(8, y, what, RED);
            y += 12;
        }
        presenter.present_drawn(&mut fb);
    }
}
