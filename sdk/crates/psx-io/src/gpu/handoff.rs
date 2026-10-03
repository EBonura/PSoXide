//! Command recording and the direct-access guard: what lets a frame's GPU
//! work be handed to a queue (`psx_rt::present`) instead of the ports.
//!
//! Built only with the `present-queue` feature. Without it the port writes
//! in [`super`] compile to a single store, as before.
//!
//! - **Recording.** Between [`begin_recording_raw`] and [`end_recording`],
//!   [`super::write_command`] appends to a caller buffer laid out as DMA
//!   linked-list nodes instead of writing the port, and the ready-waits
//!   return at once. Immediate drawing (every psx-gpu `draw_*`, every HUD
//!   helper) can then be replayed later by one DMA walk.
//! - **Direct-access guard.** Once armed ([`arm_direct_access_guard`]), the
//!   first direct command or display-control write, or GPU DMA address
//!   store, runs the registered guard function first, which waits until no
//!   queued walk can collide with the access.
//!
//! Both states share one flag, so a plain port write costs one extra load
//! and branch while the feature is on.

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

/// Payload words per recorded linked-list node. Silicon (hardware-tests
/// v1.24) loses words from DMA nodes longer than 16 while drawing.
pub const RECORDING_NODE_WORDS: u32 = 16;

struct Recording {
    active: bool,
    overflowed: bool,
    head: *mut u32,
    node: *mut u32,
    node_words: u32,
    next: *mut u32,
    end: *mut u32,
}

static mut RECORDING: Recording = Recording {
    active: false,
    overflowed: false,
    head: core::ptr::null_mut(),
    node: core::ptr::null_mut(),
    node_words: 0,
    next: core::ptr::null_mut(),
    end: core::ptr::null_mut(),
};

/// Set while recording or while the guard is armed: the rare state the port
/// writes test with one load before their plain store.
static mut SLOW: bool = false;
static mut GUARD_ARMED: bool = false;
static mut GUARD: Option<fn()> = None;

#[inline(always)]
pub(super) fn is_slow() -> bool {
    // SAFETY: a volatile read of a `bool` static; the program is single
    // threaded and the VBlank handler never touches it.
    unsafe { read_volatile(addr_of!(SLOW)) }
}

fn refresh_slow() {
    // SAFETY: volatile reads and a write of `bool` statics owned by this
    // module; the program is single threaded and no interrupt handler
    // touches them.
    unsafe {
        let slow =
            read_volatile(addr_of!(RECORDING.active)) || read_volatile(addr_of!(GUARD_ARMED));
        write_volatile(addr_of_mut!(SLOW), slow);
    }
}

fn set_recording_active(active: bool) {
    // SAFETY: a volatile write of a `bool` static owned by this module.
    unsafe { write_volatile(addr_of_mut!(RECORDING.active), active) };
    refresh_slow();
}

/// The slow half of [`super::write_command`]: record the word, or run the
/// guard before the port write.
#[inline(never)]
pub(super) fn write_command_slow(word: u32) {
    if is_recording() {
        record_word(word);
        return;
    }
    run_direct_access_guard();
    super::write_command_unguarded(word);
}

/// A command stream recorded by [`begin_recording_raw`]: DMA linked-list
/// nodes that end the list until [`link_to`](Self::link_to) chains them
/// onward.
#[derive(Debug)]
pub struct CommandRecording {
    head: *mut u32,
    tail: *mut u32,
    words: usize,
}

impl CommandRecording {
    /// First node, the address a chain links to or a DMA walk starts at.
    #[inline]
    pub fn head(&self) -> *const u32 {
        self.head
    }

    /// Words used in the buffer, node headers included.
    #[inline]
    pub fn len_words(&self) -> usize {
        self.words
    }

    /// Point the last node at `next` (a node tag) instead of ending the list
    /// there.
    ///
    /// # Safety
    ///
    /// The recording's buffer must still be live, and no walk of it may have
    /// started.
    #[inline]
    pub unsafe fn link_to(&self, next: *const u32) {
        // SAFETY: `tail` is the last node's tag inside the caller's buffer,
        // which the caller keeps live and unwalked (this fn's `# Safety`).
        unsafe {
            *self.tail = (*self.tail & 0xFF00_0000) | (next as u32 & 0x00FF_FFFF);
        }
    }
}

/// The recording ran out of buffer, so it was discarded: a command cut short
/// would make the GPU take the words after it as its parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordingOverflow;

/// True between [`begin_recording_raw`] and [`end_recording`], outside
/// [`pause_recording`].
#[inline(always)]
pub fn is_recording() -> bool {
    // SAFETY: a volatile read of a `bool` static owned by this module.
    unsafe { read_volatile(addr_of!(RECORDING.active)) }
}

/// Record every [`super::write_command`] into `buffer` instead of the port,
/// until [`end_recording`]. The ready-waits return at once while recording.
///
/// Display-control writes and DMA are not recorded: do not change display
/// state inside a recording, and upload images only through
/// [`pause_recording`] (psx-vram's uploads do).
///
/// # Safety
///
/// `buffer` must be valid for `words` word writes, 4-byte aligned, and stay
/// live and unmodified (apart from [`CommandRecording::link_to`]) until
/// every walk of the recording has finished. No other recording may be
/// open.
pub unsafe fn begin_recording_raw(buffer: *mut u32, words: usize) {
    // SAFETY: exclusive access to this module's static (single threaded, no
    // handler touches it). `buffer.add(words)` stays inside or one past the
    // caller's buffer (this fn's `# Safety`).
    unsafe {
        let recording = &mut *addr_of_mut!(RECORDING);
        debug_assert!(!recording.active, "a recording is already open");
        recording.overflowed = false;
        recording.head = core::ptr::null_mut();
        recording.node = core::ptr::null_mut();
        recording.node_words = 0;
        recording.next = buffer;
        recording.end = buffer.add(words);
    }
    set_recording_active(true);
}

/// An open recording paused by [`pause_recording`]; it resumes where it left
/// off when this is dropped.
#[must_use = "the recording resumes as soon as this is dropped"]
#[derive(Debug)]
pub struct RecordingPause {
    resume: bool,
}

/// Pause any open recording until the returned guard is dropped, so command
/// writes in between go to the port (behind the direct-access guard)
/// instead of into the buffer. For VRAM uploads, which a recording cannot
/// carry: a texture first drawn by a recorded HUD is uploaded at once,
/// before the recording is walked, which is all a draw of it needs.
#[inline]
pub fn pause_recording() -> RecordingPause {
    let resume = is_recording();
    if resume {
        set_recording_active(false);
    }
    RecordingPause { resume }
}

impl Drop for RecordingPause {
    #[inline]
    fn drop(&mut self) {
        if self.resume {
            set_recording_active(true);
        }
    }
}

/// Stop recording. `Ok(None)` when nothing was written.
pub fn end_recording() -> Result<Option<CommandRecording>, RecordingOverflow> {
    set_recording_active(false);
    // SAFETY: exclusive access to this module's static. `node` and `head`
    // point into the caller's buffer, which `begin_recording_raw`'s contract
    // keeps live; `next` is in the same buffer at or after `head`.
    unsafe {
        let recording = &mut *addr_of_mut!(RECORDING);
        if recording.overflowed {
            return Err(RecordingOverflow);
        }
        if recording.node.is_null() {
            return Ok(None);
        }
        *recording.node = (recording.node_words << 24) | 0x00FF_FFFF;
        Ok(Some(CommandRecording {
            head: recording.head,
            tail: recording.node,
            words: recording.next.offset_from(recording.head) as usize,
        }))
    }
}

#[inline(never)]
fn record_word(word: u32) {
    // SAFETY: exclusive access to this module's static. Every store lands
    // at `next`, which the checks below keep below `end`, inside the buffer
    // `begin_recording_raw`'s caller provided.
    unsafe {
        let recording = &mut *addr_of_mut!(RECORDING);
        if recording.overflowed {
            return;
        }
        if recording.node.is_null() || recording.node_words == RECORDING_NODE_WORDS {
            // A new node needs its tag plus at least this word.
            if (recording.end as usize).saturating_sub(recording.next as usize) < 8 {
                recording.overflowed = true;
                return;
            }
            let tag = recording.next;
            if recording.node.is_null() {
                recording.head = tag;
            } else {
                *recording.node = (recording.node_words << 24) | (tag as u32 & 0x00FF_FFFF);
            }
            recording.node = tag;
            recording.node_words = 0;
            recording.next = tag.add(1);
        } else if recording.next >= recording.end {
            recording.overflowed = true;
            return;
        }
        *recording.next = word;
        recording.next = recording.next.add(1);
        recording.node_words += 1;
    }
}

/// Register the function that makes direct GPU access safe while a queued
/// frame may be walking (`psx_rt::present` registers its `wait_idle`). It
/// runs once, on the first direct command or display-control write or GPU
/// DMA address store after [`arm_direct_access_guard`], and is then
/// disarmed.
pub fn set_direct_access_guard(guard: fn()) {
    // SAFETY: a volatile write of this module's static.
    unsafe { write_volatile(addr_of_mut!(GUARD), Some(guard)) };
}

/// Arm the guard: the GPU may be busy with work the CPU handed off, so the
/// next direct access must wait for it first.
pub fn arm_direct_access_guard() {
    // SAFETY: a volatile write of a `bool` static owned by this module.
    unsafe { write_volatile(addr_of_mut!(GUARD_ARMED), true) };
    refresh_slow();
}

/// Run and disarm the guard if it is armed. Direct-access paths outside the
/// port writes (GPU DMA setup) call it first.
#[inline]
pub fn run_direct_access_guard() {
    // SAFETY: volatile accesses to this module's statics.
    let armed = unsafe { read_volatile(addr_of!(GUARD_ARMED)) };
    if !armed {
        return;
    }
    // SAFETY: as above.
    unsafe { write_volatile(addr_of_mut!(GUARD_ARMED), false) };
    refresh_slow();
    // SAFETY: as above.
    if let Some(guard) = unsafe { read_volatile(addr_of!(GUARD)) } {
        guard();
    }
}

#[cfg(test)]
mod tests {
    use super::super::{wait_command_ready, write_command};
    use super::*;

    /// Words land in nodes of at most 16 with tags linking each node to the
    /// next and the last ending the list; waits do not block; an overflow
    /// discards the whole recording; a paused stretch bypasses it. One test,
    /// because the recording state is a global.
    #[test]
    fn recording_lays_out_sixteen_word_nodes() {
        let mut buffer = [0u32; 64];
        // SAFETY: `buffer` outlives the recording and is never walked.
        unsafe { begin_recording_raw(buffer.as_mut_ptr(), buffer.len()) };
        assert!(is_recording());
        for word in 0..40u32 {
            wait_command_ready();
            write_command(0x1000 + word);
        }
        let recording = end_recording().expect("fits").expect("words recorded");
        assert!(!is_recording());
        let base = buffer.as_ptr() as u32;
        assert_eq!(recording.head() as u32, base);
        assert_eq!(buffer[0] >> 24, 16);
        assert_eq!(buffer[0] & 0x00FF_FFFF, (base + 17 * 4) & 0x00FF_FFFF);
        assert_eq!(&buffer[1..3], &[0x1000, 0x1001]);
        assert_eq!(buffer[17] >> 24, 16);
        assert_eq!(buffer[34], (8 << 24) | 0x00FF_FFFF);
        assert_eq!(buffer[42], 0x1000 + 39);
        assert_eq!(recording.len_words(), 43);
        // SAFETY: `buffer` is live and never walked.
        unsafe { recording.link_to(0x8001_2340 as *const u32) };
        assert_eq!(buffer[34], (8 << 24) | 0x0001_2340);

        let mut small = [0u32; 8];
        // SAFETY: as above.
        unsafe { begin_recording_raw(small.as_mut_ptr(), small.len()) };
        for word in 0..10u32 {
            write_command(word);
        }
        assert_eq!(end_recording().unwrap_err(), RecordingOverflow);

        let mut empty = [0u32; 4];
        // SAFETY: as above.
        unsafe { begin_recording_raw(empty.as_mut_ptr(), empty.len()) };
        assert!(end_recording().expect("no overflow").is_none());

        let mut resumed = [0u32; 8];
        // SAFETY: as above.
        unsafe { begin_recording_raw(resumed.as_mut_ptr(), resumed.len()) };
        write_command(0xAA);
        {
            let _pause = pause_recording();
            assert!(!is_recording());
        }
        assert!(is_recording());
        write_command(0xBB);
        let recording = end_recording().expect("fits").expect("words recorded");
        assert_eq!(recording.len_words(), 3);
        assert_eq!(&resumed[1..3], &[0xAA, 0xBB]);
        assert!(!is_slow());
    }
}
