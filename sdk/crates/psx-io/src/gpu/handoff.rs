//! Command recording and the direct-access guard: what lets a frame's GPU
//! work be handed to a queue (`psx_rt::present`) instead of the ports.
//!
//! Built only with the `present-queue` feature. Without it the port writes
//! in [`super`] compile to a single store, as before.
//!
//! - **Recording.** While the [`ActiveRecording`] that
//!   [`start_recording_raw`] returns is alive, [`super::write_command`]
//!   appends to a caller buffer laid out as DMA linked-list nodes instead of
//!   writing the port, and the ready-waits return at once. Immediate drawing
//!   (every psx-gpu `draw_*`, every HUD helper) can then be replayed later by
//!   one DMA walk. [`ActiveRecording::end`] consumes the guard, so a
//!   recording cannot be ended twice, and a closed recording never reopens:
//!   neither a late [`RecordingPause`] nor the deprecated [`end_recording`]
//!   touches the buffer of a recording that is no longer open.
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

/// The one command recording. The buffer pointers are meaningful only while
/// `open` is set; closing clears `open` and `active` before anything else,
/// so no path reaches a buffer whose recording has ended.
struct Recording {
    /// Between a start and the close of that recording.
    open: bool,
    /// Open and not paused: what [`is_recording`] reports.
    active: bool,
    /// Counts starts, so a guard or pause knows whether the open recording
    /// is still the one it was made for.
    generation: u32,
    overflowed: bool,
    head: *mut u32,
    node: *mut u32,
    node_words: u32,
    next: *mut u32,
    end: *mut u32,
}

static mut RECORDING: Recording = Recording {
    open: false,
    active: false,
    generation: 0,
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
/// guard before the port write. Out of line and cold, so each write site
/// carries only the flag load and branch.
#[cold]
#[inline(never)]
pub(super) fn write_command_slow(word: u32) {
    if is_recording() {
        record_word(word);
        return;
    }
    run_direct_access_guard();
    super::write_command_unguarded(word);
}

/// The slow half of [`super::write_display_control`]: run the guard unless a
/// recording is open (display control is never recorded), then write the
/// port.
#[cold]
#[inline(never)]
pub(super) fn write_display_control_slow(word: u32) {
    if !is_recording() {
        run_direct_access_guard();
    }
    super::write_display_control_unguarded(word);
}

/// A command stream recorded by [`start_recording_raw`]: DMA linked-list
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

/// True while a recording is open and not paused by [`pause_recording`].
#[inline(always)]
pub fn is_recording() -> bool {
    // SAFETY: a volatile read of a `bool` static owned by this module.
    unsafe { read_volatile(addr_of!(RECORDING.active)) }
}

/// Open the recording on `buffer` and return its generation.
///
/// # Safety
///
/// As [`start_recording_raw`].
unsafe fn open(buffer: *mut u32, words: usize) -> u32 {
    // SAFETY: exclusive access to this module's static (single threaded, no
    // handler touches it). `buffer.add(words)` stays inside or one past the
    // caller's buffer (the caller's `# Safety`).
    let generation = unsafe {
        let recording = &mut *addr_of_mut!(RECORDING);
        debug_assert!(!recording.open, "a recording is already open");
        recording.generation = recording.generation.wrapping_add(1);
        recording.open = true;
        recording.overflowed = false;
        recording.head = core::ptr::null_mut();
        recording.node = core::ptr::null_mut();
        recording.node_words = 0;
        recording.next = buffer;
        recording.end = buffer.add(words);
        recording.generation
    };
    set_recording_active(true);
    generation
}

/// True when the recording of `generation` is still the open one.
fn is_open(generation: u32) -> bool {
    // SAFETY: volatile reads of this module's static.
    unsafe {
        read_volatile(addr_of!(RECORDING.open))
            && read_volatile(addr_of!(RECORDING.generation)) == generation
    }
}

/// Close the open recording, if it is the one of `generation` (any open one
/// for `None`), and terminate its last node. A recording that is already
/// closed is left alone: `Ok(None)`, and no memory is touched.
fn close(generation: Option<u32>) -> Result<Option<CommandRecording>, RecordingOverflow> {
    // SAFETY: a volatile read of this module's static.
    let open = unsafe { read_volatile(addr_of!(RECORDING.open)) };
    if !open || generation.is_some_and(|generation| !is_open(generation)) {
        return Ok(None);
    }
    set_recording_active(false);
    // SAFETY: exclusive access to this module's static. The recording is
    // open (checked above), so `node` and `head` point into the buffer its
    // start's contract keeps live until it is closed, which is now; `next`
    // is in the same buffer at or after `head`. The pointers are cleared
    // before returning, so nothing reaches the buffer through them again.
    unsafe {
        let recording = &mut *addr_of_mut!(RECORDING);
        recording.open = false;
        let result = if recording.overflowed {
            Err(RecordingOverflow)
        } else if recording.node.is_null() {
            Ok(None)
        } else {
            *recording.node = (recording.node_words << 24) | 0x00FF_FFFF;
            Ok(Some(CommandRecording {
                head: recording.head,
                tail: recording.node,
                words: recording.next.offset_from(recording.head) as usize,
            }))
        };
        recording.head = core::ptr::null_mut();
        recording.node = core::ptr::null_mut();
        recording.node_words = 0;
        recording.next = core::ptr::null_mut();
        recording.end = core::ptr::null_mut();
        result
    }
}

/// The open recording [`start_recording_raw`] returned. [`end`](Self::end)
/// closes it and hands back the recorded nodes; dropping it closes it too,
/// discarding the result.
#[must_use = "dropping the recording ends it at once"]
#[derive(Debug)]
pub struct ActiveRecording {
    generation: u32,
}

impl ActiveRecording {
    /// Stop recording. `Ok(None)` when nothing was written.
    pub fn end(self) -> Result<Option<CommandRecording>, RecordingOverflow> {
        let generation = self.generation;
        core::mem::forget(self);
        close(Some(generation))
    }
}

impl Drop for ActiveRecording {
    fn drop(&mut self) {
        let _ = close(Some(self.generation));
    }
}

/// Record every [`super::write_command`] into `buffer` instead of the port,
/// until the returned [`ActiveRecording`] is ended or dropped. The
/// ready-waits return at once while recording.
///
/// Display-control writes and DMA are not recorded: do not change display
/// state inside a recording, and upload images only through
/// [`pause_recording`] (psx-vram's uploads do).
///
/// # Panics
///
/// In debug builds, if a recording is already open. Release builds close
/// that one first, as dropping its guard would.
///
/// # Safety
///
/// `buffer` must be valid for `words` word writes and 4-byte aligned. It
/// must stay live and unmodified (apart from [`CommandRecording::link_to`])
/// until the recording is closed, which a leaked [`ActiveRecording`] never
/// does, and after that until every walk of the recording has finished.
#[doc(alias = "begin_recording_raw")]
pub unsafe fn start_recording_raw(buffer: *mut u32, words: usize) -> ActiveRecording {
    let _ = close(None);
    // SAFETY: forwarded from this fn's `# Safety`.
    let generation = unsafe { open(buffer, words) };
    ActiveRecording { generation }
}

/// An open recording paused by [`pause_recording`]; it resumes where it left
/// off when this is dropped, provided that recording is still open.
#[must_use = "the recording resumes as soon as this is dropped"]
#[derive(Debug)]
pub struct RecordingPause {
    /// Generation of the recording this paused, if it paused one.
    resume: Option<u32>,
}

/// Pause any open recording until the returned guard is dropped, so command
/// writes in between go to the port (behind the direct-access guard)
/// instead of into the buffer. For VRAM uploads, which a recording cannot
/// carry: a texture first drawn by a recorded HUD is uploaded at once,
/// before the recording is walked, which is all a draw of it needs.
#[inline]
pub fn pause_recording() -> RecordingPause {
    if !is_recording() {
        return RecordingPause { resume: None };
    }
    set_recording_active(false);
    // SAFETY: a volatile read of this module's static.
    let generation = unsafe { read_volatile(addr_of!(RECORDING.generation)) };
    RecordingPause {
        resume: Some(generation),
    }
}

impl Drop for RecordingPause {
    #[inline]
    fn drop(&mut self) {
        // A recording closed (or replaced) while paused stays closed: its
        // buffer may already be gone.
        if let Some(generation) = self.resume {
            if is_open(generation) {
                set_recording_active(true);
            }
        }
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
    if armed {
        run_armed_guard();
    }
}

/// Disarm and run the guard: the rare half of [`run_direct_access_guard`],
/// kept out of line so each GPU DMA start carries only the flag test.
#[cold]
#[inline(never)]
fn run_armed_guard() {
    // SAFETY: volatile accesses to this module's statics.
    unsafe { write_volatile(addr_of_mut!(GUARD_ARMED), false) };
    refresh_slow();
    // SAFETY: as above.
    if let Some(guard) = unsafe { read_volatile(addr_of!(GUARD)) } {
        guard();
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::super::{wait_command_ready, write_command};
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// The recording state is one global, so tests that touch it take turns.
    fn serial() -> MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Words land in nodes of at most 16 with tags linking each node to the
    /// next and the last ending the list; waits do not block; an overflow
    /// discards the whole recording; a paused stretch bypasses it.
    #[test]
    fn recording_lays_out_sixteen_word_nodes() {
        let _serial = serial();
        let mut buffer = [0u32; 64];
        // Every access goes through this one pointer: a reference to
        // `buffer` taken while the recording's pointers are in use would
        // retire them (Tree Borrows).
        let words = buffer.as_mut_ptr();
        // SAFETY: in bounds of `buffer`, which outlives this closure's uses.
        let word = |index: usize| unsafe { words.add(index).read() };
        // SAFETY: `buffer` outlives the recording and is never walked.
        let active = unsafe { start_recording_raw(words, 64) };
        assert!(is_recording());
        for word in 0..40u32 {
            wait_command_ready();
            write_command(0x1000 + word);
        }
        let recording = active.end().expect("fits").expect("words recorded");
        assert!(!is_recording());
        let base = words as u32;
        assert_eq!(recording.head() as u32, base);
        assert_eq!(word(0) >> 24, 16);
        assert_eq!(word(0) & 0x00FF_FFFF, (base + 17 * 4) & 0x00FF_FFFF);
        assert_eq!([word(1), word(2)], [0x1000, 0x1001]);
        assert_eq!(word(17) >> 24, 16);
        assert_eq!(word(34), (8 << 24) | 0x00FF_FFFF);
        assert_eq!(word(42), 0x1000 + 39);
        assert_eq!(recording.len_words(), 43);
        // SAFETY: `buffer` is live and never walked.
        unsafe { recording.link_to(0x8001_2340 as *const u32) };
        assert_eq!(word(34), (8 << 24) | 0x0001_2340);

        let mut small = [0u32; 8];
        // SAFETY: as above.
        let active = unsafe { start_recording_raw(small.as_mut_ptr(), small.len()) };
        for word in 0..10u32 {
            write_command(word);
        }
        assert_eq!(active.end().unwrap_err(), RecordingOverflow);

        let mut empty = [0u32; 4];
        // SAFETY: as above.
        let active = unsafe { start_recording_raw(empty.as_mut_ptr(), empty.len()) };
        assert!(active.end().expect("no overflow").is_none());

        let mut resumed = [0u32; 8];
        // SAFETY: as above.
        let active = unsafe { start_recording_raw(resumed.as_mut_ptr(), resumed.len()) };
        write_command(0xAA);
        {
            let _pause = pause_recording();
            assert!(!is_recording());
        }
        assert!(is_recording());
        write_command(0xBB);
        let recording = active.end().expect("fits").expect("words recorded");
        assert_eq!(recording.len_words(), 3);
        assert_eq!(&resumed[1..3], &[0xAA, 0xBB]);
        assert!(!is_slow());
    }

    /// Starts a recording on a stack buffer, pauses it, ends it while
    /// paused, and hands back the pause after the buffer is gone.
    #[inline(never)]
    fn pause_outliving_its_recording() -> RecordingPause {
        let mut buffer = [0u32; 8];
        // SAFETY: the recording is ended below, before `buffer` goes, and it
        // is never walked.
        let active = unsafe { start_recording_raw(buffer.as_mut_ptr(), buffer.len()) };
        write_command(0xCAFE);
        let pause = pause_recording();
        assert!(active.end().expect("fits").is_some());
        pause
    }

    /// io-01: dropping a pause taken before its recording ended reopened
    /// the closed recording, after which a safe `write_command` recorded
    /// into the dead buffer. The closed recording now stays closed.
    #[test]
    fn a_pause_outliving_its_recording_does_not_reopen_it() {
        let _serial = serial();
        drop(pause_outliving_its_recording());
        assert!(!is_recording());
        assert!(!is_slow());
    }

    /// Dropping the guard without `end` closes the recording, and a pause
    /// that belongs to an older recording never resumes a newer one.
    #[test]
    fn dropping_the_guard_closes_and_old_pauses_stay_inert() {
        let _serial = serial();
        let mut first = [0u32; 8];
        // SAFETY: both recordings end inside this test, before their buffers
        // go, and neither is walked.
        let active = unsafe { start_recording_raw(first.as_mut_ptr(), first.len()) };
        let pause = pause_recording();
        drop(active);
        assert!(!is_recording());

        let mut second = [0u32; 8];
        // SAFETY: as above.
        let active = unsafe { start_recording_raw(second.as_mut_ptr(), second.len()) };
        let inner = pause_recording();
        drop(pause);
        assert!(!is_recording(), "an older pause must not resume");
        drop(inner);
        assert!(is_recording());
        write_command(0x55);
        let recording = active.end().expect("fits").expect("words recorded");
        assert_eq!(recording.len_words(), 2);
        assert_eq!(second[1], 0x55);
        assert!(!is_slow());
    }
}
