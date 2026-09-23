//! `hello-faultbd` -- where does psx-rt's handler resume after a fault taken
//! in a branch delay slot?
//!
//! The handler steps over a recoverable fault (a data-side address error,
//! a bus error, ...). With Cause.BD set the faulting instruction sat in the
//! delay slot of the branch at EPC, and that branch has already run, so the
//! step has to land where the branch sends control: its target when taken,
//! EPC + 8 when not. Resuming at EPC + 4 (the handler's old rule) re-runs
//! the faulting instruction as if the branch had fallen through, which here
//! shows as a second fault and the fall-through marker on every taken case.
//!
//! Each case puts a misaligned load (AdEL) or store (AdES) in the delay
//! slot of one branch kind, taken and not taken, and checks which marker
//! ran, that exactly one fault was counted with Cause.BD set, that the link
//! register holds what the branch wrote, and that the registers the handler
//! borrows ($8-$15) come back intact. One case faults outside a delay slot.
//!
//! On silicon Cause.BD is set for a fault in any delay slot, taken or not.
//! PSoXide (emulator d684f43) sets it for neither kind of data-side fault:
//! it reports EPC at the faulting instruction with BD clear, so there every
//! delay-slot case fails whatever the handler does (the taken ones resume
//! at the fall-through, and none sees BD), and a console run is the real
//! test. An emulator build that sets BD by the MIPS rule passes all 23
//! cases with this handler and fails the 22 delay-slot cases with the old
//! EPC + 4 rule.
//!
//! The verdict goes to the TTY (`FAULTBD PASS ...` or `FAULTBD FAIL ...`)
//! and on screen.

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]

extern crate psx_rt;

use psx_font::{fonts::BASIC, FontAtlas};
use psx_gpu::{self as gpu, framebuf::FrameBuffer, Resolution, VideoMode};
use psx_rt::interrupts;
use psx_rt::tty;
use psx_vram::{Clut, TexDepth, Tpage};

const FONT_TPAGE: Tpage = Tpage::new(320, 0, TexDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);
const WHITE: (u8, u8, u8) = (220, 220, 230);
const GREEN: (u8, u8, u8) = (80, 220, 100);
const RED: (u8, u8, u8) = (230, 80, 80);

/// Values parked in $11-$15 across each case.
const SENTINELS: [u32; 5] = [
    0x1111_0B0B,
    0x1212_0C0C,
    0x1313_0D0D,
    0x1414_0E0E,
    0x1515_0F0F,
];

// jal's target. A jal into a local label would be a call to no function the
// link map names, which tools/hazard_patch.py refuses. This does the taken
// marker and returns to the case's label 2 through $7, leaving $31 as the
// jal wrote it.
core::arch::global_asm!(
    ".set noreorder",
    ".section .text.faultbd_jal_landing",
    ".globl faultbd_jal_landing",
    "faultbd_jal_landing:",
    "jr    $7",
    "addiu $2, $zero, 2",
    ".set reorder",
);

/// What one case left behind.
struct Run {
    /// 1 = fall-through marker ran, 2 = taken marker ran.
    marker: u32,
    /// $31 after the case (zeroed before the branch).
    link: u32,
    /// Address of the fall-through marker: what a linking branch writes.
    return_address: u32,
    /// $8, $9 after the case.
    a: u32,
    b: u32,
    /// $11-$15 after the case.
    sentinels: [u32; 5],
}

/// Run `$branch` (whose target is local label 1) with `$fault` in its delay
/// slot, `$8 = a`, `$9 = b`, `$10` = the address of label 1 (for jr and
/// jalr) and `$7` = the address after the taken marker (for jal's landing).
macro_rules! slot_case {
    ($branch:literal, $fault:literal, $a:expr, $b:expr) => {{
        let (mut a, mut b): (u32, u32) = ($a as u32, $b as u32);
        let [mut s11, mut s12, mut s13, mut s14, mut s15] = SENTINELS;
        let (marker, link, return_address): (u32, u32, u32);
        unsafe {
            core::arch::asm!(
                ".set noreorder",
                // $31 cannot be an operand: park it in $6 and hand its
                // value after the case back in $5.
                "move  $6, $31",
                "la    $10, 1f",
                "la    $7, 2f",
                "move  $2, $zero",
                "move  $31, $zero",
                $branch,
                $fault,
                "3:",
                "addiu $2, $zero, 1",
                "b     2f",
                "nop",
                "1:",
                "addiu $2, $zero, 2",
                "2:",
                "la    $4, 3b",
                "move  $5, $31",
                "move  $31, $6",
                ".set reorder",
                inout("$8") a,
                inout("$9") b,
                out("$10") _,
                inout("$11") s11,
                inout("$12") s12,
                inout("$13") s13,
                inout("$14") s14,
                inout("$15") s15,
                out("$2") marker,
                out("$3") _,
                out("$4") return_address,
                out("$5") link,
                out("$6") _,
                out("$7") _,
                options(nostack)
            );
        }
        Run {
            marker,
            link,
            return_address,
            a,
            b,
            sentinels: [s11, s12, s13, s14, s15],
        }
    }};
}

struct Case {
    name: &'static str,
    run: fn() -> Run,
    a: u32,
    b: u32,
    taken: bool,
    links: bool,
}

macro_rules! case {
    ($name:literal, $branch:literal, $fault:literal, $a:expr, $b:expr, $taken:expr, $links:expr) => {
        Case {
            name: $name,
            run: || slot_case!($branch, $fault, $a, $b),
            a: $a as u32,
            b: $b as u32,
            taken: $taken,
            links: $links,
        }
    };
}

#[rustfmt::skip]
fn cases() -> [Case; 23] {
    [
        case!("beq taken", "beq $8, $9, 1f", "lw $3, 1($zero)", 5, 5, true, false),
        case!("beq not", "beq $8, $9, 1f", "lw $3, 1($zero)", 5, 6, false, false),
        case!("bne taken", "bne $8, $9, 1f", "lw $3, 1($zero)", 5, 6, true, false),
        case!("bne not", "bne $8, $9, 1f", "lw $3, 1($zero)", 5, 5, false, false),
        case!("bgez taken", "bgez $8, 1f", "lw $3, 1($zero)", 0, 0, true, false),
        case!("bgez not", "bgez $8, 1f", "lw $3, 1($zero)", -1i32, 0, false, false),
        case!("bltz taken", "bltz $8, 1f", "lw $3, 1($zero)", -1i32, 0, true, false),
        case!("bltz not", "bltz $8, 1f", "lw $3, 1($zero)", 0, 0, false, false),
        case!("bgezal taken", "bgezal $8, 1f", "lw $3, 1($zero)", 1, 0, true, true),
        case!("bgezal not", "bgezal $8, 1f", "lw $3, 1($zero)", -1i32, 0, false, true),
        case!("bltzal taken", "bltzal $8, 1f", "lw $3, 1($zero)", -1i32, 0, true, true),
        case!("bltzal not", "bltzal $8, 1f", "lw $3, 1($zero)", 1, 0, false, true),
        case!("blez taken", "blez $8, 1f", "lw $3, 1($zero)", 0, 0, true, false),
        case!("blez not", "blez $8, 1f", "lw $3, 1($zero)", 1, 0, false, false),
        case!("bgtz taken", "bgtz $8, 1f", "lw $3, 1($zero)", 1, 0, true, false),
        case!("bgtz not", "bgtz $8, 1f", "lw $3, 1($zero)", 0, 0, false, false),
        case!("j", "j 1f", "lw $3, 1($zero)", 0, 0, true, false),
        case!("jal", "jal faultbd_jal_landing", "lw $3, 1($zero)", 0, 0, true, true),
        // A load in a jr/jalr slot is a load-delay hazard to
        // tools/hazard_patch.py, which would move it into a trampoline: use
        // a store.
        case!("jr ades", "jr $10", "sw $zero, 1($zero)", 0, 0, true, false),
        case!("jalr ades", "jalr $10", "sw $zero, 1($zero)", 0, 0, true, true),
        case!("beq taken ades", "beq $8, $9, 1f", "sw $zero, 1($zero)", 7, 7, true, false),
        case!("bne not ades", "bne $8, $9, 1f", "sw $zero, 1($zero)", 7, 7, false, false),
        // No branch: a plain fault steps to EPC + 4, the fall-through marker.
        case!("no slot", "nop", "lw $3, 1($zero)", 0, 0, false, false),
    ]
}

/// The first thing wrong with `case`, if anything.
fn check(case: &Case) -> Option<&'static str> {
    const CAUSE_EXC_MASK: u32 = 0x7C;
    let faults_before = interrupts::fault_count();
    let run = (case.run)();
    let faults = interrupts::fault_count().wrapping_sub(faults_before);
    let cause = interrupts::fault_cause();
    let in_slot = case.name != "no slot";
    let expected_exc = if case.name.ends_with("ades") { 5 } else { 4 };
    if run.marker != if case.taken { 2 } else { 1 } {
        return Some("wrong marker");
    }
    if faults != 1 {
        return Some("fault count not 1");
    }
    if (cause & interrupts::CAUSE_BD != 0) != in_slot {
        return Some("Cause.BD");
    }
    if (cause & CAUSE_EXC_MASK) >> 2 != expected_exc {
        return Some("ExcCode");
    }
    if run.link != if case.links { run.return_address } else { 0 } {
        return Some("link register");
    }
    if run.a != case.a || run.b != case.b || run.sentinels != SENTINELS {
        return Some("registers not restored");
    }
    None
}

/// `label=XXXXXXXX` for the TTY and the screen.
fn line(label: &str, value: u32) -> ([u8; 32], usize) {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    let mut text = [b' '; 32];
    let label = &label.as_bytes()[..label.len().min(22)];
    text[..label.len()].copy_from_slice(label);
    text[label.len()] = b'=';
    for i in 0..8 {
        text[label.len() + 1 + i] = DIGITS[(value >> (28 - 4 * i) & 0xF) as usize];
    }
    (text, label.len() + 9)
}

fn as_str(text: &[u8]) -> &str {
    core::str::from_utf8(text).unwrap_or("?")
}

#[no_mangle]
fn main() {
    // Installs psx-rt's exception handler.
    interrupts::install_vblank_counter();

    let cases = cases();
    let mut failed: [Option<(&str, &str)>; 6] = [None; 6];
    let mut failures = 0u32;
    for case in &cases {
        if let Some(what) = check(case) {
            tty::print("FAULTBD case failed: ");
            tty::print(case.name);
            tty::print(": ");
            tty::println(what);
            if let Some(slot) = failed.get_mut(failures as usize) {
                *slot = Some((case.name, what));
            }
            failures += 1;
        }
    }

    let lines = [
        line("failures", failures),
        line("cases", cases.len() as u32),
        line("faults", interrupts::fault_count()),
    ];
    let (banner, tint) = if failures == 0 {
        ("FAULTBD PASS", GREEN)
    } else {
        ("FAULTBD FAIL", RED)
    };
    tty::print(banner);
    for (text, len) in &lines {
        tty::print(" ");
        tty::print(as_str(&text[..*len]));
    }
    tty::println("");

    gpu::init(VideoMode::Ntsc, Resolution::R320X240);
    let mut fb = FrameBuffer::new(320, 240);
    gpu::set_draw_area(0, 0, 319, 239);
    gpu::set_draw_offset(0, 0);
    let font = FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT);
    loop {
        fb.clear(10, 12, 20);
        font.draw_text(8, 6, "FAULTS IN BRANCH DELAY SLOTS", WHITE);
        font.draw_text(8, 30, banner, tint);
        for (row, (text, len)) in lines.iter().enumerate() {
            font.draw_text(8, 54 + 12 * row as i16, as_str(&text[..*len]), WHITE);
        }
        let mut y = 102;
        for (name, what) in failed.iter().flatten() {
            font.draw_text(8, y, name, RED);
            font.draw_text(128, y, what, RED);
            y += 12;
        }
        gpu::draw_sync();
        interrupts::wait_vblank();
        fb.swap();
    }
}
