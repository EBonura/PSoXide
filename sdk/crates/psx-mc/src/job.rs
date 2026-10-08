// SPDX-License-Identifier: GPL-2.0-or-later
//! A card operation spread over game frames.
//!
//! [`Card::write`] on a [`HardwareCard`](crate::HardwareCard) holds the CPU
//! for the whole operation: a directory read is a frame transaction of about
//! 5 ms, a write is one plus the card's commit time, and a save is dozens of
//! them. Called from a game loop that is a freeze of seconds.
//!
//! A [`CardJob`] runs the same filesystem code in three bounded phases and
//! never does more than one card transaction per [`step`](CardJob::step):
//!
//! 1. **Probe**: the directory (frames `0..16`) is read into the job, one
//!    frame per step.
//! 2. **Plan**: the caller's closure runs the ordinary [`Card`] operations
//!    (`format`, `write`, `delete`, ...) against a [`Staged`] card, which serves
//!    reads from the cached directory and the writes already queued, and queues
//!    every write instead of sending it. No card is touched and the cost is
//!    CPU only, in one step.
//! 3. **Write**: the queued frames are sent in the order the filesystem code
//!    wrote them, one per step. After each, the card is left alone for
//!    [`SETTLE_VBLANKS`] while it commits to flash; steps that land inside the
//!    wait return [`Step::Waiting`] without touching the port.
//!
//! The queue keeps the filesystem's write order, so the guarantees of
//! [`Card::write`] hold at every point: the old save stays whole until the new
//! one is complete, and the new file appears with its first directory entry
//! last. A card pulled between steps is in the same state it would be in if it
//! had been pulled during the blocking call.
//!
//! The job does not own the port. Each [`step`](CardJob::step) is handed the
//! device for that call alone, so a game keeps polling its pads between steps:
//!
//! ```text
//! static mut JOB: CardJob<{ queue_frames(2048) }> = CardJob::new();
//!
//! job.begin();
//! // every game frame:
//! let mut card = HardwareCard::on_port(&mut port, Slot::One);
//! match job.step(&mut card, vblank_count())? {
//!     Step::NeedsPlan => job.plan(|c| c.write(NAME, TITLE, &save_bytes))?,
//!     Step::Done => { /* saved */ }
//!     _ => {}
//! }
//! ```

use crate::fs::blocks_for;
use crate::{Block, Card, Error, Result, CONTAINER_LEN, FRAME_COUNT, FRAME_SIZE};

/// Directory frames a job reads before it plans: the `MC` header and the
/// fifteen entries.
pub const DIR_FRAMES: usize = 16;

/// Frames a [`Card::format`] queues: the header, fifteen entries, the
/// broken-sector list and the cleared tail of the directory block.
pub const FORMAT_FRAMES: usize = 64;

/// Video periods the card is left alone after each frame write, while it
/// commits to flash. The blocking path (`HardwareCard::write_frame`) spins
/// 400,000 status reads there, five to six video periods in the emulator and
/// at least two by the reference drivers' rule; a job keeps the same wait,
/// rounded up, by counting vblanks, so a game can spend it drawing instead.
/// A job's wait can be changed with [`CardJob::with_settle_vblanks`].
pub const SETTLE_VBLANKS: u32 = 8;

/// Queue frames for one [`Card::write`] of a `payload_len`-byte save over an
/// older copy of the same size: the title and icon frames, the data frames
/// (container header included), one directory entry per block written, and
/// one per block of the copy released afterwards. A format
/// ([`FORMAT_FRAMES`]) or the deletion of another file is on top.
pub const fn queue_frames(payload_len: usize) -> usize {
    let total = CONTAINER_LEN + payload_len;
    let blocks = blocks_for(total);
    2 + total.div_ceil(FRAME_SIZE) + 2 * blocks
}

/// What one [`CardJob::step`] did.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// No operation is in progress ([`CardJob::begin`] starts one).
    Idle,
    /// The card is committing its last write; the port was not touched.
    Waiting,
    /// One directory frame was read.
    Probing,
    /// The directory is cached. Call [`CardJob::plan`]; nothing else moves
    /// until it has been.
    NeedsPlan,
    /// One queued frame was written.
    Writing,
    /// Every frame is written and the card has finished committing the last.
    Done,
}

/// The card as the planning closure sees it. Reads come from the writes
/// queued so far, then from the cached directory; writes are queued, not
/// sent. It is the [`Block`] under the `Card` that [`CardJob::plan`] passes.
///
/// It holds the directory only: a read of any other frame that no queued
/// write covers fails with [`Error::OutOfRange`]. `format`, `write`, `delete`,
/// `list`, `free_blocks` and `is_formatted` read nothing else. A write past
/// the queue's capacity fails with [`Error::NoSpace`].
pub struct Staged<'a, const N: usize>(&'a mut Stage<N>);

// Every field is a whole number of words, so the all-zero job has no padding
// bytes and the linker keeps a `static` one in `.bss`, not in the executable.
#[repr(C)]
struct Stage<const N: usize> {
    len: u32,
    frame: [u32; N],
    dir: [[u8; FRAME_SIZE]; DIR_FRAMES],
    data: [[u8; FRAME_SIZE]; N],
}

impl<const N: usize> Block for Staged<'_, N> {
    fn read_frame(&mut self, frame: u16, out: &mut [u8; FRAME_SIZE]) -> Result<()> {
        let stage = &*self.0;
        for i in (0..stage.len as usize).rev() {
            if stage.frame[i] == frame as u32 {
                *out = stage.data[i];
                return Ok(());
            }
        }
        match stage.dir.get(frame as usize) {
            Some(cached) => {
                *out = *cached;
                Ok(())
            }
            None => Err(Error::OutOfRange),
        }
    }

    fn write_frame(&mut self, frame: u16, data: &[u8; FRAME_SIZE]) -> Result<()> {
        if frame as usize >= FRAME_COUNT {
            return Err(Error::OutOfRange);
        }
        let stage = &mut *self.0;
        let at = stage.len as usize;
        if at == N {
            return Err(Error::NoSpace);
        }
        stage.frame[at] = frame as u32;
        stage.data[at] = *data;
        stage.len += 1;
        Ok(())
    }
}

#[derive(Copy, Clone, PartialEq, Eq)]
#[repr(u32)]
enum Phase {
    Idle = 0,
    /// Reading the directory frame at `cursor`.
    Probe,
    NeedsPlan,
    /// Sending the queued frame at `cursor`.
    Write,
    /// Everything sent; waiting out the last commit.
    Drain,
}

/// A save, format or delete run a little at a time. `N` is the number of
/// frame writes it can queue (see [`queue_frames`] and [`FORMAT_FRAMES`]); the
/// job holds `N` frames of 128 bytes plus the 2 KiB directory, so keep it in a
/// `static`. The idle job is all zero bytes, so a `static` one costs no space
/// in the executable.
#[repr(C)]
pub struct CardJob<const N: usize> {
    phase: Phase,
    cursor: u32,
    /// The vblank count at which the card may be touched again.
    ready_at: u32,
    /// Commit wait in vblanks; 0 is [`SETTLE_VBLANKS`].
    settle: u32,
    stage: Stage<N>,
}

impl<const N: usize> CardJob<N> {
    /// An idle job with the default [`SETTLE_VBLANKS`] commit wait.
    pub const fn new() -> Self {
        CardJob {
            phase: Phase::Idle,
            cursor: 0,
            ready_at: 0,
            settle: 0,
            stage: Stage {
                len: 0,
                frame: [0; N],
                dir: [[0; FRAME_SIZE]; DIR_FRAMES],
                data: [[0; FRAME_SIZE]; N],
            },
        }
    }

    /// The same job with `vblanks` as the commit wait after each frame write,
    /// for a card slower than the default allows for. 0 is the default.
    pub const fn with_settle_vblanks(mut self, vblanks: u32) -> Self {
        self.settle = vblanks;
        self
    }

    fn settle_vblanks(&self) -> u32 {
        if self.settle == 0 {
            SETTLE_VBLANKS
        } else {
            self.settle
        }
    }

    /// Start an operation: the next steps read the directory. Anything in
    /// progress is dropped (what it had already written stays written).
    pub fn begin(&mut self) {
        self.stage.len = 0;
        self.cursor = 0;
        self.phase = Phase::Probe;
    }

    /// Drop the operation in progress. Frames already sent stay on the card.
    pub fn abort(&mut self) {
        self.stage.len = 0;
        self.phase = Phase::Idle;
    }

    /// Whether an operation is in progress.
    pub fn is_busy(&self) -> bool {
        self.phase != Phase::Idle
    }

    /// How far along the operation is, 0 to 256: the directory read is the
    /// first sixteenth and the frame writes the rest.
    pub fn progress_q8(&self) -> u32 {
        match self.phase {
            Phase::Idle => 0,
            Phase::Probe => self.cursor * 16 / DIR_FRAMES as u32,
            Phase::NeedsPlan => 16,
            Phase::Write => 16 + self.cursor * 240 / self.stage.len.max(1),
            Phase::Drain => 256,
        }
    }

    fn card_ready(&self, now: u32) -> bool {
        now.wrapping_sub(self.ready_at) < 0x8000_0000
    }

    /// Advance by at most one card transaction. `now` is the vblank count
    /// (`psx_rt::interrupts::vblank_count()`), which times the commit wait;
    /// `dev` is the card for this call only.
    ///
    /// A card error ends the operation (the job is idle again) and is
    /// returned.
    pub fn step<B: Block>(&mut self, dev: &mut B, now: u32) -> Result<Step> {
        match self.phase {
            Phase::Idle => return Ok(Step::Idle),
            Phase::NeedsPlan => return Ok(Step::NeedsPlan),
            _ if !self.card_ready(now) => return Ok(Step::Waiting),
            _ => {}
        }
        match self.phase {
            Phase::Probe => {
                let n = self.cursor as usize;
                if let Err(e) = dev.read_frame(n as u16, &mut self.stage.dir[n]) {
                    self.abort();
                    return Err(e);
                }
                if n + 1 < DIR_FRAMES {
                    self.cursor += 1;
                } else {
                    self.phase = Phase::NeedsPlan;
                }
                Ok(Step::Probing)
            }
            Phase::Write => {
                let i = self.cursor as usize;
                let sent =
                    dev.write_frame_unsettled(self.stage.frame[i] as u16, &self.stage.data[i]);
                if let Err(e) = sent {
                    self.abort();
                    return Err(e);
                }
                self.ready_at = now.wrapping_add(self.settle_vblanks());
                if i + 1 < self.stage.len as usize {
                    self.cursor += 1;
                } else {
                    self.phase = Phase::Drain;
                }
                Ok(Step::Writing)
            }
            // `Idle` and `NeedsPlan` returned above, and `Drain` is the only
            // phase left once the card is ready.
            _ => {
                self.stage.len = 0;
                self.phase = Phase::Idle;
                Ok(Step::Done)
            }
        }
    }

    /// Run the filesystem operations for this job on a [`Staged`] card, after
    /// [`step`](Self::step) has returned [`Step::NeedsPlan`]. The closure sees
    /// the card as it was probed and may read what it has written; its result
    /// is passed through.
    ///
    /// On `Ok` the queued writes are sent by the following steps. On `Err` the
    /// job is idle again and nothing was written, so a caller that wants to
    /// act on the error (ask before formatting a blank card, say) starts a new
    /// operation with [`begin`](Self::begin).
    ///
    /// Out of order, this returns [`Error::Protocol`] and changes nothing.
    pub fn plan<R>(
        &mut self,
        build: impl FnOnce(&mut Card<Staged<'_, N>>) -> Result<R>,
    ) -> Result<R> {
        if self.phase != Phase::NeedsPlan {
            return Err(Error::Protocol);
        }
        self.stage.len = 0;
        let mut card = Card::new(Staged(&mut self.stage));
        match build(&mut card) {
            Ok(value) => {
                self.cursor = 0;
                self.phase = if self.stage.len == 0 {
                    Phase::Drain
                } else {
                    Phase::Write
                };
                Ok(value)
            }
            Err(e) => {
                self.abort();
                Err(e)
            }
        }
    }
}

impl<const N: usize> Default for CardJob<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
