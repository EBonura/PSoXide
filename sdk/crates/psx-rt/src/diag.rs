// SPDX-License-Identifier: GPL-2.0-or-later
//! Console stop-panel: what a program was doing when it stopped, on screen.
//!
//! Built only with the `console-diag` feature, for diagnostic discs; a
//! shipping image does not contain any of it. Three things bring the panel up:
//!
//! * a **fault**: the exception handler reached its halt (a `BREAK` from
//!   `panic=immediate-abort`, an instruction-side fault, a fault in a delay
//!   slot);
//! * a **panic** with a message (builds without `panic=immediate-abort`);
//! * a **stall**: the VBlank handler saw no sign of life for [`STALL_VBLANKS`]
//!   frames (no GP1 write, no GPU DMA kick, no CD sector or command), which is
//!   how a bounded or unbounded wait that never ends looks.
//!
//! The panel takes over the display (it resets the GPU and shows its own
//! 320x240 screen), then prints the program counter, return address and stack
//! of the interrupted code, a guess at the call chain, the GPU, DMA and
//! interrupt registers, the SDK's wait and CD counters ([`psx_io::diag`]) and
//! any panic message, and keeps refreshing the frame counters so a photo shows
//! the machine is alive but the program is not.
//!
//! Resolve the printed addresses against the link map of the same build.

use core::arch::global_asm;
use psx_hw::dma::{channel, CHANNEL_BASE, CHANNEL_STRIDE, CHCR, DPCR, MADR};
use psx_hw::gpu::{GP0, GP1};
use psx_hw::irq::{I_MASK, I_STAT};
use psx_io::diag::{self, Block, PSX_DIAG};

/// VBlanks without progress before the stall panel appears (15 seconds at 60 Hz).
pub const STALL_VBLANKS: u32 = 900;

/// The limit in force: [`STALL_VBLANKS`] unless [`set_stall_limit`] changed it.
static mut STALL_LIMIT: u32 = STALL_VBLANKS;

/// Change how many VBlanks without progress bring the stall panel up. A program
/// with long steps that are legitimately silent (no GP1 write, GPU DMA kick or
/// CD command for tens of seconds) raises it so the panel does not take the
/// screen in the middle of a measurement; faults and panics are unaffected.
pub fn set_stall_limit(vblanks: u32) {
    // SAFETY: a plain word written before or between VBlanks on one thread; the
    // watchdog only reads it.
    unsafe { (&raw mut STALL_LIMIT).write_volatile(vblanks) };
}

const COLS: usize = 53;
const ROWS: usize = 27;

const BG: u16 = 0x1000;
const WHITE: u16 = 0x7FFF;
const YELLOW: u16 = 0x03FF;
const GREEN: u16 = 0x03E0;
const RED: u16 = 0x001F;
const CYAN: u16 = 0x7FE0;

/// Saved registers of the interrupted code: index = register number, then HI, LO.
#[no_mangle]
static mut __psx_diag_regs: [u32; 40] = [0; 40];

/// Stack the panel runs on, so it works however the program's own stack looks.
#[no_mangle]
static mut __psx_diag_stack: [u32; 256] = [0; 256];

#[repr(C)]
struct Watch {
    last_sig: u32,
    quiet: u32,
    shown: u32,
    frame: u32,
}

static mut WATCH: Watch = Watch {
    last_sig: 0,
    quiet: 0,
    shown: 0,
    frame: 0,
};

/// Panic text recorded by [`record_panic`].
static mut PANIC_MSG: [u8; 120] = [0; 120];
static mut PANIC_FILE: [u8; 48] = [0; 48];
static mut PANIC_LINE: u32 = 0;
static mut PANIC_SET: u32 = 0;

/// Optional tag the build names itself with (`PSX_DIAG_TAG` at compile time).
const TAG: &str = match option_env!("PSX_DIAG_TAG") {
    Some(tag) => tag,
    None => "",
};

// Save every register to `__psx_diag_regs` through $27, then switch to the panel stack.
global_asm!(
    r#"
    .macro PSX_DIAG_SAVE
    .set push
    .set noat
    lui   $27, %hi(__psx_diag_regs)
    addiu $27, $27, %lo(__psx_diag_regs)
    sw    $26, 136($27)
    sw    $1,   4($27)
    sw    $2,   8($27)
    sw    $3,  12($27)
    sw    $4,  16($27)
    sw    $5,  20($27)
    sw    $6,  24($27)
    sw    $7,  28($27)
    sw    $8,  32($27)
    sw    $9,  36($27)
    sw    $10, 40($27)
    sw    $11, 44($27)
    sw    $12, 48($27)
    sw    $13, 52($27)
    sw    $14, 56($27)
    sw    $15, 60($27)
    sw    $16, 64($27)
    sw    $17, 68($27)
    sw    $18, 72($27)
    sw    $19, 76($27)
    sw    $20, 80($27)
    sw    $21, 84($27)
    sw    $22, 88($27)
    sw    $23, 92($27)
    sw    $24, 96($27)
    sw    $25, 100($27)
    sw    $28, 112($27)
    sw    $29, 116($27)
    sw    $30, 120($27)
    sw    $31, 124($27)
    mfhi  $26
    nop
    sw    $26, 128($27)
    mflo  $26
    nop
    sw    $26, 132($27)
    # Run on the interrupted code's own stack, just below its stack pointer, when that is
    # ordinary RAM; the fallback is the small static stack (a scratchpad stack, a wild $sp).
    lw    $2, 116($27)
    lui   $3, 0x8001
    ori   $3, $3, 0x1000
    sltu  $4, $2, $3
    bnez  $4, 8f
    nop
    lui   $3, 0x8020
    sltu  $4, $2, $3
    beqz  $4, 8f
    nop
    andi  $4, $2, 3
    bnez  $4, 8f
    nop
    addiu $29, $2, -256
    b     9f
    nop
8:
    lui   $29, %hi(__psx_diag_stack + 1008)
    addiu $29, $29, %lo(__psx_diag_stack + 1008)
9:
    move  $4, $27
    mfc0  $5, $14
    nop
    .set pop
    .endm

    .macro PSX_DIAG_RESTORE
    .set push
    .set noat
    lui   $27, %hi(__psx_diag_regs)
    addiu $27, $27, %lo(__psx_diag_regs)
    lw    $26, 128($27)
    nop
    mthi  $26
    lw    $26, 132($27)
    nop
    mtlo  $26
    lw    $1,   4($27)
    lw    $2,   8($27)
    lw    $3,  12($27)
    lw    $4,  16($27)
    lw    $5,  20($27)
    lw    $6,  24($27)
    lw    $7,  28($27)
    lw    $8,  32($27)
    lw    $9,  36($27)
    lw    $10, 40($27)
    lw    $11, 44($27)
    lw    $12, 48($27)
    lw    $13, 52($27)
    lw    $14, 56($27)
    lw    $15, 60($27)
    lw    $16, 64($27)
    lw    $17, 68($27)
    lw    $18, 72($27)
    lw    $19, 76($27)
    lw    $20, 80($27)
    lw    $21, 84($27)
    lw    $22, 88($27)
    lw    $23, 92($27)
    lw    $24, 96($27)
    lw    $25, 100($27)
    lw    $28, 112($27)
    lw    $29, 116($27)
    lw    $30, 120($27)
    lw    $31, 124($27)
    lw    $26, 136($27)
    nop
    jr    $26
    nop
    .set pop
    .endm

    # Called from the exception handler's VBlank path with every register still the
    # interrupted code's (the handler has used only $26 and $27). Runs the Rust
    # watchdog on the panel stack, then puts everything back.
    .set noreorder
    .section .text.psx_diag_tramp
    .globl __psx_diag_vblank_tramp
__psx_diag_vblank_tramp:
    PSX_DIAG_SAVE
    jal   __psx_diag_tick
    nop
    PSX_DIAG_RESTORE

    # The fatal-fault halt: never returns.
    .globl __psx_diag_fault_tramp
__psx_diag_fault_tramp:
    PSX_DIAG_SAVE
    jal   __psx_diag_fault
    nop
1:  b     1b
    nop

    .set reorder
    "#
);

// ---------------------------------------------------------------- text

/// One text line under construction.
struct Line {
    buf: [u8; COLS],
    len: usize,
}

impl Line {
    const fn new() -> Self {
        Line {
            buf: [b' '; COLS],
            len: 0,
        }
    }

    #[inline(never)]
    fn str(&mut self, s: &str) -> &mut Self {
        for &b in s.as_bytes() {
            self.byte(b);
        }
        self
    }

    #[inline(never)]
    fn byte(&mut self, b: u8) {
        if self.len < COLS {
            self.buf[self.len] = b;
            self.len += 1;
        }
    }

    #[inline(never)]
    fn hex(&mut self, v: u32, digits: usize) -> &mut Self {
        let mut i = digits;
        while i > 0 {
            i -= 1;
            let n = ((v >> (i * 4)) & 15) as u8;
            self.byte(if n < 10 { b'0' + n } else { b'A' + n - 10 });
        }
        self
    }

    #[inline(never)]
    fn dec(&mut self, mut v: u32) -> &mut Self {
        let mut tmp = [0u8; 10];
        let mut n = 0;
        if v == 0 {
            tmp[0] = b'0';
            n = 1;
        }
        while v > 0 {
            tmp[n] = b'0' + (v % 10) as u8;
            v /= 10;
            n += 1;
        }
        while n > 0 {
            n -= 1;
            self.byte(tmp[n]);
        }
        self
    }

    #[inline(never)]
    fn sp(&mut self) -> &mut Self {
        self.byte(b' ');
        self
    }
}

/// The 5x7 glyph for an ASCII byte (lower case folds to upper), one byte a row,
/// bit 4 the left pixel. Drawn by this crate; it covers `' '..='_'`.
#[optimize(size)]
#[inline(never)]
fn glyph(c: u8) -> [u8; 7] {
    let c = if c.is_ascii_lowercase() { c - 32 } else { c };
    if !(32..=95).contains(&c) {
        return [31, 17, 17, 17, 17, 17, 31];
    }
    FONT[(c - 32) as usize]
}

const FONT: [[u8; 7]; 64] = [
    [0, 0, 0, 0, 0, 0, 0],
    [4, 4, 4, 4, 4, 0, 4],
    [10, 10, 0, 0, 0, 0, 0],
    [10, 10, 31, 10, 31, 10, 10],
    [4, 15, 20, 14, 5, 30, 4],
    [24, 25, 2, 4, 8, 19, 3],
    [12, 18, 20, 8, 21, 18, 13],
    [4, 4, 0, 0, 0, 0, 0],
    [2, 4, 8, 8, 8, 4, 2],
    [8, 4, 2, 2, 2, 4, 8],
    [0, 4, 21, 14, 21, 4, 0],
    [0, 4, 4, 31, 4, 4, 0],
    [0, 0, 0, 0, 12, 4, 8],
    [0, 0, 0, 31, 0, 0, 0],
    [0, 0, 0, 0, 0, 12, 12],
    [1, 1, 2, 4, 8, 16, 16],
    [14, 17, 19, 21, 25, 17, 14],
    [4, 12, 4, 4, 4, 4, 14],
    [14, 17, 1, 2, 4, 8, 31],
    [31, 2, 4, 2, 1, 17, 14],
    [2, 6, 10, 18, 31, 2, 2],
    [31, 16, 30, 1, 1, 17, 14],
    [6, 8, 16, 30, 17, 17, 14],
    [31, 1, 2, 4, 8, 8, 8],
    [14, 17, 17, 14, 17, 17, 14],
    [14, 17, 17, 15, 1, 2, 12],
    [0, 12, 12, 0, 12, 12, 0],
    [0, 12, 12, 0, 12, 4, 8],
    [2, 4, 8, 16, 8, 4, 2],
    [0, 0, 31, 0, 31, 0, 0],
    [8, 4, 2, 1, 2, 4, 8],
    [14, 17, 1, 2, 4, 0, 4],
    [14, 17, 23, 21, 23, 16, 14],
    [14, 17, 17, 31, 17, 17, 17],
    [30, 17, 17, 30, 17, 17, 30],
    [14, 17, 16, 16, 16, 17, 14],
    [30, 17, 17, 17, 17, 17, 30],
    [31, 16, 16, 30, 16, 16, 31],
    [31, 16, 16, 30, 16, 16, 16],
    [14, 17, 16, 23, 17, 17, 15],
    [17, 17, 17, 31, 17, 17, 17],
    [14, 4, 4, 4, 4, 4, 14],
    [7, 2, 2, 2, 2, 18, 12],
    [17, 18, 20, 24, 20, 18, 17],
    [16, 16, 16, 16, 16, 16, 31],
    [17, 27, 21, 21, 17, 17, 17],
    [17, 17, 25, 21, 19, 17, 17],
    [14, 17, 17, 17, 17, 17, 14],
    [30, 17, 17, 30, 16, 16, 16],
    [14, 17, 17, 17, 21, 18, 13],
    [30, 17, 17, 30, 20, 18, 17],
    [15, 16, 16, 14, 1, 1, 30],
    [31, 4, 4, 4, 4, 4, 4],
    [17, 17, 17, 17, 17, 17, 14],
    [17, 17, 17, 17, 17, 10, 4],
    [17, 17, 17, 21, 21, 27, 17],
    [17, 17, 10, 4, 10, 17, 17],
    [17, 17, 10, 4, 4, 4, 4],
    [31, 1, 2, 4, 8, 16, 31],
    [14, 8, 8, 8, 8, 8, 14],
    [16, 16, 8, 4, 2, 1, 1],
    [14, 2, 2, 2, 2, 2, 14],
    [4, 10, 17, 0, 0, 0, 0],
    [0, 0, 0, 0, 0, 0, 31],
];

// ---------------------------------------------------------------- GPU

/// Register `offset` (`MADR`, `CHCR`) of DMA channel `ch`.
const fn dma_reg(ch: u32, offset: u32) -> u32 {
    CHANNEL_BASE + CHANNEL_STRIDE * ch + offset
}

fn rd(addr: u32) -> u32 {
    // SAFETY: an aligned 32-bit read of a PS1 register or RAM word, as the caller intends.
    unsafe { psx_io::read_u32(addr) }
}

fn wr(addr: u32, value: u32) {
    // SAFETY: an aligned 32-bit write to a PS1 register.
    unsafe { psx_io::write_u32(addr, value) }
}

fn gpu_ready() {
    let mut n = 0u32;
    while rd(GP1) & (1 << 26) == 0 && n < 200_000 {
        n += 1;
    }
}

/// Make the screen ours: stop the GPU DMA, reset the GPU, show 320x240 15-bit
/// at the top of VRAM and clear it.
#[optimize(size)]
#[inline(never)]
fn take_screen() {
    // Channel 2 (GPU): drop START so nothing keeps feeding the GPU.
    wr(dma_reg(channel::GPU, CHCR), 0);
    wr(GP1, 0x0000_0000); // reset
    wr(GP1, 0x0400_0000); // DMA off
    wr(GP1, 0x0500_0000); // display start 0,0
    wr(GP1, 0x06C6_0260); // horizontal range for 320
    wr(GP1, 0x0704_0010); // vertical range, NTSC
    wr(GP1, 0x0800_0001); // 320x240, NTSC, 15 bit
    wr(GP1, 0x0300_0000); // display on
    gpu_ready();
    wr(GP0, 0xE100_0000);
    wr(GP0, 0xE300_0000);
    wr(GP0, 0xE403_BD3F);
    wr(GP0, 0xE500_0000);
    wr(GP0, 0x0220_0000); // fill, dark blue
    wr(GP0, 0x0000_0000);
    wr(GP0, (240 << 16) | 320);
    gpu_ready();
}

/// Draw one 8-pixel-tall text row at VRAM row `y`, a pixel row at a time, computing each pair
/// of pixels as it is written so no buffer is needed (the panel may run on a 1 KB stack).
#[optimize(size)]
fn draw_line(y: u16, line: &Line, color: u16) {
    let mut row = 0usize;
    while row < 8 {
        gpu_ready();
        wr(GP0, 0xA000_0000);
        wr(GP0, (y as u32 + row as u32) << 16);
        wr(GP0, (1 << 16) | 320);
        let mut x = 0usize;
        while x < 320 {
            let mut pair = 0u32;
            let mut k = 0;
            while k < 2 {
                let xx = x + k;
                let col = xx / 6;
                let px = xx % 6;
                let on = col < COLS
                    && px < 5
                    && row < 7
                    && glyph(line.buf[col])[row] & (1 << (4 - px)) != 0;
                let c = if on { color } else { BG } as u32;
                pair |= c << (16 * k);
                k += 1;
            }
            wr(GP0, pair);
            x += 2;
        }
        row += 1;
    }
}

// ---------------------------------------------------------------- panel

#[derive(Clone, Copy, PartialEq, Eq)]
enum Reason {
    Stall,
    Fault,
    Panic,
}

#[inline(never)]
fn is_return_address(w: u32) -> bool {
    if !(0x8001_0000..0x8020_0000).contains(&w) || w & 3 != 0 {
        return false;
    }
    let call = rd(w - 8);
    // JAL, or JALR (SPECIAL, function 9).
    call >> 26 == 3 || (call >> 26 == 0 && call & 0x3F == 9)
}

#[optimize(size)]
#[inline(never)]
fn put_line(row: &mut usize, line: &Line, color: u16) {
    if *row < ROWS {
        draw_line((*row * 8 + 16) as u16, line, color);
        *row += 1;
    }
}

#[optimize(size)]
#[inline(never)]
fn panel(reason: Reason, regs: *const u32, epc: u32) {
    // SAFETY: `regs` is the register save area filled by the trampoline.
    let reg = |n: usize| unsafe { regs.add(n).read_volatile() };
    let b: *const Block = &raw const PSX_DIAG;
    // SAFETY: every caller passes a field pointer into the diagnostics static.
    let d = |f: *const u32| unsafe { diag::get(f) };
    macro_rules! f {
        ($name:ident) => {
            // SAFETY: a field pointer into the diagnostics static.
            d(unsafe { &raw const (*b).$name })
        };
    }
    // SAFETY: the panel runs with interrupts off on one thread; nothing else touches WATCH.
    let frame = unsafe {
        let w = &raw mut WATCH;
        (*w).frame = (*w).frame.wrapping_add(1);
        (*w).frame
    };
    // SAFETY: as above.
    let quiet = unsafe { (&raw const WATCH.quiet).read_volatile() };

    let mut row = 0usize;
    let mut l = Line::new();
    l.str("PSXDIAG ")
        .str(match reason {
            Reason::Stall => "STALL NO PROGRESS",
            Reason::Fault => "FAULT HALT",
            Reason::Panic => "PANIC",
        })
        .sp()
        .str(TAG);
    put_line(
        &mut row,
        &l,
        if reason == Reason::Stall { YELLOW } else { RED },
    );

    let mut l = Line::new();
    l.str("VBL ")
        .hex(crate::interrupts::vblank_count(), 8)
        .str(" FRM ")
        .dec(frame)
        .str(" QUIET ")
        .dec(quiet)
        .str(" BASE ")
        .dec(psx_io::disc_base::lba_offset());
    put_line(&mut row, &l, WHITE);

    let mut l = Line::new();
    l.str("STEP ").dec(f!(step_id)).sp();
    // SAFETY: a byte array in the static, read volatile.
    let text = unsafe { (&raw const (*b).step_text).cast::<u8>() };
    let mut i = 0;
    while i < 24 {
        // SAFETY: `i` < 24, inside `step_text`.
        let c = unsafe { text.add(i).read_volatile() };
        if c == 0 {
            break;
        }
        l.byte(c);
        i += 1;
    }
    put_line(&mut row, &l, CYAN);

    let mut l = Line::new();
    l.str("PC ")
        .hex(epc, 8)
        .str(" RA ")
        .hex(reg(31), 8)
        .str(" SP ")
        .hex(reg(29), 8);
    put_line(&mut row, &l, GREEN);

    let mut l = Line::new();
    l.str("A0 ")
        .hex(reg(4), 8)
        .str(" A1 ")
        .hex(reg(5), 8)
        .str(" V0 ")
        .hex(reg(2), 8);
    put_line(&mut row, &l, WHITE);

    let mut l = Line::new();
    l.str("FAULTS ")
        .dec(crate::interrupts::fault_count())
        .str(" CAUSE ")
        .hex(crate::interrupts::fault_cause(), 8)
        .str(" EPC ")
        .hex(crate::interrupts::fault_pc(), 8);
    put_line(
        &mut row,
        &l,
        if reason == Reason::Fault { RED } else { WHITE },
    );
    let mut l = Line::new();
    l.str("BADVADDR ")
        .hex(crate::interrupts::fault_bad_address(), 8)
        .str(" STRAY IRQ ")
        .dec(crate::interrupts::stray_interrupt_count());
    put_line(&mut row, &l, WHITE);

    // A guess at the call chain: words on the stack that follow a call.
    let sp = reg(29);
    let mut l = Line::new();
    l.str("CALLERS");
    put_line(&mut row, &l, CYAN);
    let mut l = Line::new();
    let mut found = 0;
    if (0x8000_0000..0x801F_FF00).contains(&sp) && sp & 3 == 0 {
        let mut at = sp;
        let end = (sp + 1024).min(0x8020_0000 - 8);
        while at < end && found < 12 {
            let w = rd(at);
            if is_return_address(w) {
                l.hex(w, 8).sp();
                found += 1;
                if found % 5 == 0 {
                    put_line(&mut row, &l, WHITE);
                    l = Line::new();
                }
            }
            at += 4;
        }
    }
    put_line(&mut row, &l, WHITE);

    let mut l = Line::new();
    l.str("GPUSTAT ")
        .hex(rd(GP1), 8)
        .str(" ISTAT ")
        .hex(rd(I_STAT), 4)
        .str(" IMASK ")
        .hex(rd(I_MASK), 4);
    put_line(&mut row, &l, WHITE);
    let mut l = Line::new();
    l.str("DMA2 CHCR ")
        .hex(rd(dma_reg(channel::GPU, CHCR)), 8)
        .str(" MADR ")
        .hex(rd(dma_reg(channel::GPU, MADR)), 8)
        .str(" DPCR ")
        .hex(rd(DPCR), 8);
    put_line(&mut row, &l, WHITE);
    let mut l = Line::new();
    l.str("DMA3 CHCR ")
        .hex(rd(dma_reg(channel::CD, CHCR)), 8)
        .str(" MADR ")
        .hex(rd(dma_reg(channel::CD, MADR)), 8)
        .str(" DMA4 ")
        .hex(rd(dma_reg(channel::SPU, CHCR)), 8);
    put_line(&mut row, &l, WHITE);

    let mut l = Line::new();
    l.str("KICKS");
    // SAFETY: a field pointer into the diagnostics static.
    let kicks = unsafe { (&raw const (*b).dma_kicks).cast::<u32>() };
    let mut ch = 0;
    while ch < 7 {
        // SAFETY: `ch` < 7, inside `dma_kicks`.
        l.sp().dec(d(unsafe { kicks.add(ch) }));
        ch += 1;
    }
    put_line(&mut row, &l, WHITE);

    let mut l = Line::new();
    l.str("GP1 ")
        .dec(f!(gp1_writes))
        .str(" GPURESET ")
        .dec(psx_io::gpu::command_reset_count())
        .str(" GPUWAITFAIL ")
        .dec(f!(gpu_wait_fail));
    put_line(&mut row, &l, WHITE);
    let mut l = Line::new();
    l.str("DMAWAITFAIL ")
        .dec(f!(dma_wait_fail))
        .str(" CH ")
        .dec(f!(dma_wait_ch));
    put_line(&mut row, &l, WHITE);

    let mut l = Line::new();
    l.str("WAIT NOW ")
        .hex(f!(wait_cur), 4)
        .str(" LAST ")
        .hex(f!(wait_last), 4)
        .str(" SPINS ")
        .dec(f!(wait_last_spins))
        .str(" TMO ")
        .dec(f!(wait_timeouts));
    let now = f!(wait_cur);
    put_line(&mut row, &l, if now != 0 { YELLOW } else { WHITE });

    // The CD status register: reading index 0 has no side effect.
    // SAFETY: an aligned byte read of the CD status register, which has no side effect.
    let cd_status = unsafe { psx_io::read_u8(psx_hw::cd::BASE) };
    let mut l = Line::new();
    l.str("CD STAT ")
        .hex(cd_status as u32, 2)
        .str(" CMD ")
        .hex(f!(cd_cmd), 2)
        .str(" N ")
        .dec(f!(cd_cmds))
        .str(" IRQ ")
        .hex(f!(cd_irq_seen), 2)
        .str("/")
        .hex(f!(cd_irq_acked), 2);
    put_line(&mut row, &l, WHITE);
    let mut l = Line::new();
    l.str("CD INT5 ")
        .dec(f!(cd_int5))
        .str(" ST ")
        .hex(f!(cd_int5_status), 2)
        .str(" ERR ")
        .hex(f!(cd_int5_code), 2)
        .str(" TMO ")
        .dec(f!(cd_timeouts))
        .str(" ")
        .hex(f!(cd_timeout_info), 8);
    put_line(
        &mut row,
        &l,
        if f!(cd_int5) + f!(cd_timeouts) != 0 {
            YELLOW
        } else {
            WHITE
        },
    );
    let mut l = Line::new();
    l.str("CD SEC ")
        .dec(f!(cd_sectors))
        .str(" STARTS ")
        .dec(f!(cd_starts))
        .str(" LBA ")
        .dec(f!(cd_lba))
        .str(" FAIL ")
        .dec(f!(cd_read_fail))
        .str(" ")
        .hex(f!(cd_fail_word), 8);
    put_line(
        &mut row,
        &l,
        if f!(cd_read_fail) != 0 { YELLOW } else { WHITE },
    );

    let mut l = Line::new();
    l.str("LL SCANS ")
        .dec(f!(ll_scans))
        .str(" NODES ")
        .dec(f!(ll_nodes))
        .str(" BAD ")
        .dec(f!(ll_bad))
        .str(" MAX ")
        .dec(f!(ll_max))
        .str(" AT ")
        .hex(f!(ll_bad_addr), 6);
    put_line(&mut row, &l, if f!(ll_bad) != 0 { RED } else { WHITE });

    let mut l = Line::new();
    l.str("PRESENT KICK ")
        .dec(crate::interrupts::present_kick_count())
        .str(" SKIP ")
        .dec(crate::interrupts::present_skip_count());
    put_line(&mut row, &l, WHITE);

    // SAFETY: panic records are written once before the panel and only read here.
    if unsafe { (&raw const PANIC_SET).read_volatile() } != 0 {
        let mut l = Line::new();
        let file = (&raw const PANIC_FILE).cast::<u8>();
        let mut i = 0;
        while i < 48 {
            // SAFETY: `i` < 48, inside the buffer.
            let c = unsafe { file.add(i).read_volatile() };
            if c == 0 {
                break;
            }
            l.byte(c);
            i += 1;
        }
        // SAFETY: as above.
        l.str(":")
            .dec(unsafe { (&raw const PANIC_LINE).read_volatile() });
        put_line(&mut row, &l, RED);
        let msg = (&raw const PANIC_MSG).cast::<u8>();
        let mut l = Line::new();
        let mut i = 0;
        while i < 120 {
            // SAFETY: `i` < 120, inside the buffer.
            let c = unsafe { msg.add(i).read_volatile() };
            if c == 0 {
                break;
            }
            if i > 0 && i % COLS == 0 {
                put_line(&mut row, &l, RED);
                l = Line::new();
            }
            l.byte(c);
            i += 1;
        }
        put_line(&mut row, &l, RED);
    }
}

/// VBlank watchdog, entered from the exception handler on every VBlank with the
/// interrupted code's registers saved in `regs`.
#[no_mangle]
extern "C" fn __psx_diag_tick(regs: *const u32, epc: u32) {
    // SAFETY: the handler runs with interrupts off on one thread, so nothing else touches WATCH.
    unsafe {
        let w = &raw mut WATCH;
        let b: *const Block = &raw const PSX_DIAG;
        let mut sig = diag::get(&raw const (*b).gp1_writes);
        let kicks = (&raw const (*b).dma_kicks).cast::<u32>();
        sig = sig.wrapping_add(diag::get(kicks.add(2)).wrapping_mul(3));
        sig = sig.wrapping_add(diag::get(&raw const (*b).cd_sectors).wrapping_mul(5));
        sig = sig.wrapping_add(diag::get(&raw const (*b).cd_cmds).wrapping_mul(7));
        sig = sig.wrapping_add(crate::interrupts::present_kick_count().wrapping_mul(11));
        if sig != (*w).last_sig {
            (*w).last_sig = sig;
            (*w).quiet = 0;
            return;
        }
        (*w).quiet = (*w).quiet.wrapping_add(1);
        if (*w).quiet < (&raw const STALL_LIMIT).read_volatile() {
            return;
        }
        // Once stalled, take the screen on the first frame, then refresh every fourth.
        if (*w).shown == 0 {
            (*w).shown = 1;
            take_screen();
        } else if (*w).quiet & 3 != 0 {
            return;
        }
    }
    panel(Reason::Stall, regs, epc);
}

/// The exception handler's fatal halt: draw the panel for good.
#[no_mangle]
extern "C" fn __psx_diag_fault(regs: *const u32, epc: u32) -> ! {
    take_screen();
    loop {
        panel(Reason::Fault, regs, epc);
        // Pace on the VBlank flag, which still latches with interrupts off.
        let mut n = 0u32;
        while rd(I_STAT) & 1 == 0 && n < 2_000_000 {
            n += 1;
        }
        wr(I_STAT, !1u32);
    }
}

/// Clear the CPU's interrupt-enable bit, so the VBlank handler cannot run on the panel's
/// stack while it draws.
fn interrupts_off() {
    // SAFETY: reads, clears bit 0 and writes back COP0 Status; the `nop` after the read covers
    // the MFC0 load delay.
    unsafe {
        core::arch::asm!(
            "mfc0 {t}, $12",
            "nop",
            "srl  {t}, {t}, 1",
            "sll  {t}, {t}, 1",
            "mtc0 {t}, $12",
            "nop",
            t = out(reg) _,
            options(nostack),
        );
    }
}

extern "C" fn panic_entry(info: *const u8) -> ! {
    interrupts_off();
    take_screen();
    let regs = &raw const __psx_diag_regs;
    loop {
        // SAFETY: `regs` is the register save area; word 31 is the saved return address.
        panel(Reason::Panic, regs.cast::<u32>(), unsafe {
            regs.cast::<u32>().add(31).read_volatile()
        });
        let mut n = 0u32;
        while rd(I_STAT) & 1 == 0 && n < 2_000_000 {
            n += 1;
        }
        wr(I_STAT, !1u32);
        let _ = info;
    }
}

/// Record a panic's message and location, then show the panel; never returns.
/// Called by psx-rt's panic handler on diagnostic builds.
pub(crate) fn panic_panel(info: &core::panic::PanicInfo<'_>) -> ! {
    // SAFETY: the program is dead after this call; nothing else writes these.
    unsafe {
        let msg = (&raw mut PANIC_MSG).cast::<u8>();
        let mut w = MsgWriter { at: msg, len: 0 };
        use core::fmt::Write;
        let _ = write!(w, "{}", info.message());
        w.finish();
        let file = (&raw mut PANIC_FILE).cast::<u8>();
        if let Some(loc) = info.location() {
            let name = loc.file().as_bytes();
            // Keep the tail of a long path: the end names the file.
            let start = name.len().saturating_sub(47);
            let mut i = 0;
            while start + i < name.len() && i < 47 {
                file.add(i).write_volatile(name[start + i]);
                i += 1;
            }
            file.add(i).write_volatile(0);
            (&raw mut PANIC_LINE).write_volatile(loc.line());
        }
        (&raw mut PANIC_SET).write_volatile(1);
        // The stack and return address the panic ran on, so the panel can walk the call chain.
        let (sp, ra): (u32, u32);
        core::arch::asm!("move {}, $sp", "move {}, $ra", out(reg) sp, out(reg) ra, options(nomem, nostack));
        let regs = (&raw mut __psx_diag_regs).cast::<u32>();
        regs.add(29).write_volatile(sp);
        regs.add(31).write_volatile(ra);
        panic_entry(core::ptr::null())
    }
}

struct MsgWriter {
    at: *mut u8,
    len: usize,
}

impl MsgWriter {
    fn finish(&mut self) {
        // SAFETY: `len` < 120, inside the buffer.
        unsafe { self.at.add(self.len).write_volatile(0) };
    }
}

impl core::fmt::Write for MsgWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            if self.len < 119 {
                // SAFETY: `len` < 119, inside the buffer.
                unsafe { self.at.add(self.len).write_volatile(b) };
                self.len += 1;
            }
        }
        Ok(())
    }
}
