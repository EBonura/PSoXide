// SPDX-License-Identifier: GPL-2.0-or-later
//! Console diagnostics: the counters a stop-panel prints.
//!
//! Built only with the `console-diag` feature. Without it every function here
//! is an empty inline and the block does not exist, so a shipping image is
//! byte-for-byte what it was. With it, the SDK's own bounded waits, CD command
//! writes, sector pops and DMA kicks leave their last state in one
//! `static` block, [`BLOCK`], that `psx_rt::diag` reads when a program stops
//! (a fault, a panic, or no progress for many frames) and paints on screen,
//! so one photo of a console shows where it stopped.
//!
//! Nothing here touches hardware; every store is a plain volatile word store
//! to a private static, safe from the foreground and from the VBlank handler.

/// Wait sites recorded in [`Block::wait_cur`] and [`Block::wait_last`].
pub mod site {
    /// `gpu::wait_command_ready` (GPUSTAT bit 26).
    pub const GPU_READY_CMD: u32 = 1;
    /// `gpu::wait_dma_ready` (GPUSTAT bit 28).
    pub const GPU_READY_DMA: u32 = 2;
    /// `dma::wait_done`; the channel number is added (3 to 9).
    pub const DMA_DONE: u32 = 3;
    /// The CD reader waiting for a command response; the expected flag is in
    /// the high byte of the recorded value.
    pub const CD_RESPONSE: u32 = 11;
    /// The CD reader waiting for the next data sector.
    pub const CD_SECTOR: u32 = 12;
    /// The CD reader waiting for the data FIFO to fill.
    pub const CD_FIFO: u32 = 13;
}

/// The diagnostics block. Every field is a plain word; the panel reads them
/// with volatile loads.
#[repr(C)]
pub struct Block {
    /// [`MAGIC`] once any hook has run.
    pub magic: u32,
    /// GP1 writes seen (one per presented frame in most programs).
    pub gp1_writes: u32,
    /// DMA kicks per channel, `dma::start`.
    pub dma_kicks: [u32; 7],
    /// `dma::wait_done` budgets that ran out.
    pub dma_wait_fail: u32,
    /// Channel of the last such failure.
    pub dma_wait_ch: u32,
    /// GPUSTAT ready-waits that timed out (and reset the GPU's command buffer).
    pub gpu_wait_fail: u32,
    /// Wait in progress now: site in the low byte, aux in the next, 0 for none.
    pub wait_cur: u32,
    /// The last wait that spun out: site, aux like `wait_cur`.
    pub wait_last: u32,
    /// Spin budget of that wait.
    pub wait_last_spins: u32,
    /// Waits that spun out.
    pub wait_timeouts: u32,
    /// Last CD command byte written.
    pub cd_cmd: u32,
    /// CD commands written.
    pub cd_cmds: u32,
    /// Last non-zero CD IRQ flag read.
    pub cd_irq_seen: u32,
    /// Last CD IRQ bits acknowledged.
    pub cd_irq_acked: u32,
    /// INT5 (drive error) responses.
    pub cd_int5: u32,
    /// Status byte of the last INT5.
    pub cd_int5_status: u32,
    /// Error byte of the last INT5.
    pub cd_int5_code: u32,
    /// CD waits that spun out.
    pub cd_timeouts: u32,
    /// `cause<<24 | status<<16 | command<<8 | flag` of the last timeout.
    pub cd_timeout_info: u32,
    /// Sectors popped from the data FIFO.
    pub cd_sectors: u32,
    /// Last absolute LBA a read was started at (after the disc-base shift).
    pub cd_lba: u32,
    /// `read_sector` calls that failed.
    pub cd_read_fail: u32,
    /// The reader's own failure snapshot at the last failure.
    pub cd_fail_word: u32,
    /// Free-form step id set by the program ([`step`]).
    pub step_id: u32,
    /// Free-form step text, NUL padded.
    pub step_text: [u8; 24],
}

/// Written to [`Block::magic`] by the first hook.
pub const MAGIC: u32 = 0x5053_4447;

/// The block. Read it with volatile loads.
#[cfg(feature = "console-diag")]
#[no_mangle]
pub static mut PSX_DIAG: Block = Block {
    magic: 0,
    gp1_writes: 0,
    dma_kicks: [0; 7],
    dma_wait_fail: 0,
    dma_wait_ch: 0,
    gpu_wait_fail: 0,
    wait_cur: 0,
    wait_last: 0,
    wait_last_spins: 0,
    wait_timeouts: 0,
    cd_cmd: 0,
    cd_cmds: 0,
    cd_irq_seen: 0,
    cd_irq_acked: 0,
    cd_int5: 0,
    cd_int5_status: 0,
    cd_int5_code: 0,
    cd_timeouts: 0,
    cd_timeout_info: 0,
    cd_sectors: 0,
    cd_lba: 0,
    cd_read_fail: 0,
    cd_fail_word: 0,
    step_id: 0,
    step_text: [0; 24],
};

#[cfg(feature = "console-diag")]
macro_rules! field {
    ($f:ident) => {
        // SAFETY: a raw pointer to a field of a private static; nothing is read or written here.
        unsafe { &raw mut PSX_DIAG.$f }
    };
}

#[cfg(feature = "console-diag")]
#[inline(always)]
fn bump(p: *mut u32) {
    // SAFETY: a private static word; the program is single threaded and the only other reader is
    // the panel, which runs when the program has stopped.
    unsafe { p.write_volatile(p.read_volatile().wrapping_add(1)) }
}

#[cfg(feature = "console-diag")]
#[inline(always)]
fn set(p: *mut u32, v: u32) {
    // SAFETY: as in `bump`.
    unsafe { p.write_volatile(v) }
}

/// A raw read of one word of the block, for the panel.
#[cfg(feature = "console-diag")]
#[inline(always)]
pub fn get(p: *const u32) -> u32 {
    // SAFETY: as in `bump`.
    unsafe { p.read_volatile() }
}

/// Mark that the block is live.
#[cfg(feature = "console-diag")]
#[inline(always)]
fn touch() {
    set(field!(magic), MAGIC);
}

/// A `dma::start` on channel `ch` (0 to 6).
#[inline(always)]
pub fn dma_kick(ch: u32) {
    #[cfg(feature = "console-diag")]
    {
        touch();
        if ch < 7 {
            // SAFETY: `ch` is in range, and the pointer stays inside the static.
            bump(unsafe { (&raw mut PSX_DIAG.dma_kicks).cast::<u32>().add(ch as usize) });
        }
    }
    #[cfg(not(feature = "console-diag"))]
    let _ = ch;
}

/// A write to GP1.
#[inline(always)]
pub fn gp1_write() {
    #[cfg(feature = "console-diag")]
    bump(field!(gp1_writes));
}

/// A wait begins. `aux` is a small extra id (a channel, an expected flag).
#[inline(always)]
pub fn wait_begin(site: u32, aux: u32) {
    #[cfg(feature = "console-diag")]
    {
        touch();
        set(field!(wait_cur), site | (aux << 8));
    }
    #[cfg(not(feature = "console-diag"))]
    let _ = (site, aux);
}

/// The wait that [`wait_begin`] opened ended normally.
#[inline(always)]
pub fn wait_end() {
    #[cfg(feature = "console-diag")]
    set(field!(wait_cur), 0);
}

/// The wait that [`wait_begin`] opened spun out after `spins` polls.
#[inline(always)]
pub fn wait_timeout(site: u32, aux: u32, spins: u32) {
    #[cfg(feature = "console-diag")]
    {
        set(field!(wait_cur), 0);
        set(field!(wait_last), site | (aux << 8));
        set(field!(wait_last_spins), spins);
        bump(field!(wait_timeouts));
        if site == site::DMA_DONE {
            bump(field!(dma_wait_fail));
            set(field!(dma_wait_ch), aux);
        } else if site <= site::GPU_READY_DMA {
            bump(field!(gpu_wait_fail));
        }
    }
    #[cfg(not(feature = "console-diag"))]
    let _ = (site, aux, spins);
}

/// A CD command byte was written.
#[inline(always)]
pub fn cd_command(command: u8) {
    #[cfg(feature = "console-diag")]
    {
        touch();
        set(field!(cd_cmd), command as u32);
        bump(field!(cd_cmds));
    }
    #[cfg(not(feature = "console-diag"))]
    let _ = command;
}

/// A non-zero CD IRQ flag was read.
#[inline(always)]
pub fn cd_irq_seen(flag: u8) {
    #[cfg(feature = "console-diag")]
    if flag != 0 {
        set(field!(cd_irq_seen), flag as u32);
    }
    #[cfg(not(feature = "console-diag"))]
    let _ = flag;
}

/// CD IRQ bits were acknowledged.
#[inline(always)]
pub fn cd_irq_acked(bits: u8) {
    #[cfg(feature = "console-diag")]
    set(field!(cd_irq_acked), bits as u32);
    #[cfg(not(feature = "console-diag"))]
    let _ = bits;
}

/// The drive answered a command with INT5.
#[inline(always)]
pub fn cd_error(status: u8, code: u8) {
    #[cfg(feature = "console-diag")]
    {
        bump(field!(cd_int5));
        set(field!(cd_int5_status), status as u32);
        set(field!(cd_int5_code), code as u32);
    }
    #[cfg(not(feature = "console-diag"))]
    let _ = (status, code);
}

/// A CD wait spun out; `info` is the reader's snapshot word.
#[inline(always)]
pub fn cd_timeout(info: u32) {
    #[cfg(feature = "console-diag")]
    {
        bump(field!(cd_timeouts));
        set(field!(cd_timeout_info), info);
    }
    #[cfg(not(feature = "console-diag"))]
    let _ = info;
}

/// One sector was popped from the data FIFO.
#[inline(always)]
pub fn cd_sector() {
    #[cfg(feature = "console-diag")]
    bump(field!(cd_sectors));
}

/// A read was started at absolute `lba`.
#[inline(always)]
pub fn cd_start(lba: u32) {
    #[cfg(feature = "console-diag")]
    set(field!(cd_lba), lba);
    #[cfg(not(feature = "console-diag"))]
    let _ = lba;
}

/// `read_sector` failed; `word` is the reader's snapshot.
#[inline(always)]
pub fn cd_read_failed(word: u32) {
    #[cfg(feature = "console-diag")]
    {
        bump(field!(cd_read_fail));
        set(field!(cd_fail_word), word);
    }
    #[cfg(not(feature = "console-diag"))]
    let _ = word;
}

/// Name the step the program has reached, for the panel: `id` and up to 24
/// ASCII bytes of `text`.
#[inline(never)]
pub fn step(id: u32, text: &str) {
    #[cfg(feature = "console-diag")]
    {
        touch();
        set(field!(step_id), id);
        let dst = unsafe { (&raw mut PSX_DIAG.step_text).cast::<u8>() };
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < 24 {
            let b = if i < bytes.len() { bytes[i] } else { 0 };
            // SAFETY: `i < 24`, inside `step_text`.
            unsafe { dst.add(i).write_volatile(b) };
            i += 1;
        }
    }
    #[cfg(not(feature = "console-diag"))]
    let _ = (id, text);
}
