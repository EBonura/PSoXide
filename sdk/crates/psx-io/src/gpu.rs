//! GPU MMIO: `GP0`, `GP1`, `GPUREAD`, `GPUSTAT`.
//!
//! Thin wrappers over [`crate::read32`] / [`crate::write32`] that use
//! the register addresses from `psx-hw`. Each helper commits exactly
//! one MMIO access; higher-level SDK code composes them into commands.

use psx_hw::gpu::{GpuStat, GP0, GP1, GPUREAD, GPUSTAT};

/// Push a command or data word to `GP0`. Named `write_gp0` (not just
/// `gp0`) so it doesn't collide with the `psx_hw::gpu::gp0` module
/// that holds the command-word constructors -- that way callers can
/// write `write_gp0(gp0::draw_mode(...))` and each half of the name
/// is unambiguous.
#[inline(always)]
pub fn write_gp0(word: u32) {
    if is_capturing() {
        capture_word(word);
        return;
    }
    unsafe { crate::write32(GP0, word) }
}

/// Payload words per recorded linked-list node. Silicon (hardware-tests
/// v1.24) loses words from DMA nodes longer than 16 while drawing.
pub const CAPTURE_NODE_WORDS: u32 = 16;

struct Capture {
    active: bool,
    overflowed: bool,
    head: *mut u32,
    node: *mut u32,
    node_words: u32,
    next: *mut u32,
    end: *mut u32,
}

static mut CAPTURE: Capture = Capture {
    active: false,
    overflowed: false,
    head: core::ptr::null_mut(),
    node: core::ptr::null_mut(),
    node_words: 0,
    next: core::ptr::null_mut(),
    end: core::ptr::null_mut(),
};

/// A GP0 command stream recorded by [`begin_capture`]: DMA linked-list nodes
/// that end the list until [`link_to`](Self::link_to) chains them onward.
#[derive(Debug)]
pub struct Gp0Recording {
    head: *mut u32,
    tail: *mut u32,
    /// Words used in the buffer, node headers included.
    pub words: usize,
}

impl Gp0Recording {
    /// First node, the address a chain links to or a DMA walk starts at.
    pub fn head(&self) -> *const u32 {
        self.head
    }

    /// Point the last node at `next` (a node header) instead of ending the
    /// list there.
    ///
    /// # Safety
    /// The recording's buffer must still be live and not yet walked.
    pub unsafe fn link_to(&self, next: *const u32) {
        unsafe {
            *self.tail = (*self.tail & 0xFF00_0000) | (next as u32 & 0x00FF_FFFF);
        }
    }
}

/// The recording ran out of buffer, so it was discarded: a command cut short
/// would make the GPU take the next words as its parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureOverflow;

/// True between [`begin_capture`] and [`end_capture`].
#[inline(always)]
pub fn is_capturing() -> bool {
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(CAPTURE.active)) }
}

/// Record every [`write_gp0`] into `buffer` instead of the port, until
/// [`end_capture`], so immediate-mode drawing can be replayed by a DMA walk
/// (a frame's HUD, queued behind its world). The ready-waits return at once
/// while recording. GP1 writes and DMA are not recorded: do not upload images
/// or change display state inside a recording.
///
/// # Safety
/// `buffer` must be valid for `words` writes, word aligned, and stay live
/// and unmodified until any walk of the recording has finished.
pub unsafe fn begin_capture(buffer: *mut u32, words: usize) {
    unsafe {
        let capture = &mut *core::ptr::addr_of_mut!(CAPTURE);
        capture.overflowed = false;
        capture.head = core::ptr::null_mut();
        capture.node = core::ptr::null_mut();
        capture.node_words = 0;
        capture.next = buffer;
        capture.end = buffer.add(words);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(CAPTURE.active), true);
    }
}

/// Stop recording. `Ok(None)` when nothing was written.
pub fn end_capture() -> Result<Option<Gp0Recording>, CaptureOverflow> {
    unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(CAPTURE.active), false);
        let capture = &mut *core::ptr::addr_of_mut!(CAPTURE);
        if capture.overflowed {
            return Err(CaptureOverflow);
        }
        if capture.node.is_null() {
            return Ok(None);
        }
        *capture.node = (capture.node_words << 24) | 0x00FF_FFFF;
        Ok(Some(Gp0Recording {
            head: capture.head,
            tail: capture.node,
            words: capture.next.offset_from(capture.head) as usize,
        }))
    }
}

#[inline(never)]
fn capture_word(word: u32) {
    unsafe {
        let capture = &mut *core::ptr::addr_of_mut!(CAPTURE);
        if capture.overflowed {
            return;
        }
        if capture.node.is_null() || capture.node_words == CAPTURE_NODE_WORDS {
            // A new node needs its header plus at least this word.
            if (capture.end as usize).saturating_sub(capture.next as usize) < 8 {
                capture.overflowed = true;
                return;
            }
            let header = capture.next;
            if capture.node.is_null() {
                capture.head = header;
            } else {
                *capture.node = (capture.node_words << 24) | (header as u32 & 0x00FF_FFFF);
            }
            capture.node = header;
            capture.node_words = 0;
            capture.next = header.add(1);
        } else if capture.next >= capture.end {
            capture.overflowed = true;
            return;
        }
        *capture.next = word;
        capture.next = capture.next.add(1);
        capture.node_words += 1;
    }
}

/// Push a command to `GP1`.
#[inline(always)]
pub fn write_gp1(word: u32) {
    unsafe { crate::write32(GP1, word) }
}

/// Read the GPU status register.
#[inline(always)]
pub fn gpustat() -> GpuStat {
    GpuStat::from_bits_retain(unsafe { crate::read32(GPUSTAT) })
}

/// Read the VRAM-to-CPU / GP1(10h) return latch.
#[inline(always)]
pub fn gpuread() -> u32 {
    unsafe { crate::read32(GPUREAD) }
}

/// Spin until the GPU is ready to accept a new command word.
///
/// Polls `GPUSTAT.READY_CMD`. On real hardware this bit is briefly
/// cleared while the GPU is busy ingesting a multi-word packet; our
/// emulator forces it on, so the loop is essentially a single read.
/// Do not use this between GP0(0xA0) image payload words: after the
/// transfer setup packet, the GPU is waiting for data, not a new
/// normal command.
#[inline]
pub fn wait_cmd_ready() {
    wait_ready(GpuStat::READY_CMD);
}

/// Spin budget for one GPUSTAT ready-wait. Long enough for the slowest
/// legitimate primitive, short enough that a stuck GPU hands control
/// back well inside a frame.
pub const READY_SPINS: u32 = 500_000;

/// Bounded ready-wait with recovery.
///
/// A GPU left mid-command never re-asserts its ready bits, so a bare
/// `while !ready {}` hangs forever. That is not hypothetical: aborting a
/// wedged linked-list DMA (see `psx_io::dma::abort`) stops the walker
/// partway through a packet, and the GPU then sits waiting for command
/// words that will never arrive. On timeout, GP1(01h) resets the command
/// buffer, which is the documented way to discard that partial command
/// and make the GPU accept work again.
fn wait_ready(flag: GpuStat) {
    if is_capturing() {
        return;
    }
    let mut spins = 0u32;
    while !gpustat().contains(flag) {
        if spins >= READY_SPINS {
            write_gp1(0x0100_0000);
            return;
        }
        spins += 1;
    }
}

/// Spin until the GPU can start a DMA block transfer.
#[inline]
pub fn wait_dma_ready() {
    wait_ready(GpuStat::READY_DMA_RECV);
}

/// Poll command readiness without resetting the GPU on timeout.
/// Reads once, then retries at most `spin_limit` times.
#[inline]
pub fn try_wait_cmd_ready(spin_limit: u32) -> bool {
    try_wait_ready(GpuStat::READY_CMD, spin_limit)
}
/// Poll DMA receive readiness without resetting the GPU on timeout.
#[inline]
pub fn try_wait_dma_ready(spin_limit: u32) -> bool {
    try_wait_ready(GpuStat::READY_DMA_RECV, spin_limit)
}
fn try_wait_ready(flag: GpuStat, spin_limit: u32) -> bool {
    if is_capturing() {
        return true;
    }
    poll_ready(spin_limit, || gpustat().contains(flag))
}

fn poll_ready(spin_limit: u32, mut ready: impl FnMut() -> bool) -> bool {
    let mut remaining = spin_limit;
    loop {
        if ready() {
            return true;
        }
        if remaining == 0 {
            return false;
        }
        remaining -= 1;
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// Words land in nodes of at most 16 with headers linking each node to
    /// the next and the last ending the list; waits do not block; an
    /// overflow discards the whole recording.
    #[test]
    fn capture_lays_out_sixteen_word_nodes() {
        let mut buffer = [0u32; 64];
        unsafe { begin_capture(buffer.as_mut_ptr(), buffer.len()) };
        assert!(is_capturing());
        for word in 0..40u32 {
            wait_cmd_ready();
            write_gp0(0x1000 + word);
        }
        let recording = end_capture().expect("fits").expect("words recorded");
        assert!(!is_capturing());
        let base = buffer.as_ptr() as u32;
        assert_eq!(recording.head() as u32, base);
        assert_eq!(buffer[0] >> 24, 16);
        assert_eq!(buffer[0] & 0x00FF_FFFF, (base + 17 * 4) & 0x00FF_FFFF);
        assert_eq!(&buffer[1..3], &[0x1000, 0x1001]);
        assert_eq!(buffer[17] >> 24, 16);
        assert_eq!(buffer[34], (8 << 24) | 0x00FF_FFFF);
        assert_eq!(buffer[42], 0x1000 + 39);
        assert_eq!(recording.words, 43);
        unsafe { recording.link_to(0x8001_2340 as *const u32) };
        assert_eq!(buffer[34], (8 << 24) | 0x0001_2340);

        let mut small = [0u32; 8];
        unsafe { begin_capture(small.as_mut_ptr(), small.len()) };
        for word in 0..10u32 {
            write_gp0(word);
        }
        assert_eq!(end_capture().unwrap_err(), CaptureOverflow);

        let mut empty = [0u32; 4];
        unsafe { begin_capture(empty.as_mut_ptr(), empty.len()) };
        assert!(end_capture().expect("no overflow").is_none());
    }
    #[test]
    fn readiness_budget_includes_initial_probe() {
        for limit in 0..5 {
            let mut reads = 0;
            assert!(!poll_ready(limit, || {
                reads += 1;
                false
            }));
            assert_eq!(reads, limit + 1);
        }
        let mut reads = 0;
        assert!(poll_ready(2, || {
            reads += 1;
            reads == 3
        }));
        assert_eq!(reads, 3);
    }
}
