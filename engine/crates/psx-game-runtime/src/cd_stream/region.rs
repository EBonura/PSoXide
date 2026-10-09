//! Incremental read of one run of sectors, handed to a consumer sector by
//! sector as each lands: the transport half of a streamed-world region
//! install (design 2026-10-08, M7).
//!
//! Unlike [`super::read_chunk_blocking`] there is no destination buffer. The
//! consumer sees each landed sector in place and writes what it needs into
//! its final home, so a region install needs no RAM for the payload at all.
//!
//! Stall and restart policy is the room job's, not a second one: a pump that
//! finds no sector gives up on the display clock ([`STALL_LIMIT_VBLANKS`],
//! counted from the last sector that landed), and a run that fails is
//! restarted at the first sector not yet handed to the sink
//! ([`GROUP_RETRIES`] times, refilled by every sector). The ring under both
//! already restarts a run whose window the transport lost (`Unknown`).

use super::ring::Transport;
use super::*;

/// Where a [`RegionRead`] stands after a poll.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegionReadProgress {
    /// Sectors are still outstanding.
    Reading,
    /// Every sector was handed to the consumer.
    Done,
    /// The read stopped; the code is one of the `STATUS_*` values or the
    /// consumer's own.
    Failed(u32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Idle,
    Armed,
    /// The run under the read failed; the next poll starts it again at the
    /// first sector not yet handed out.
    Restart,
    Reading,
    Done,
    Failed(u32),
}

/// One run of `sectors` sectors from an absolute LBA, read a few sectors per
/// pump. Owns no buffer; the shared [`CdController`] stages each sector.
#[derive(Clone, Copy, Debug)]
pub struct RegionRead {
    lba: u32,
    sectors: u32,
    landed: u32,
    /// The pump found no sector since the last one landed.
    stalled: bool,
    /// Display clock when the stall began.
    stall_since: u32,
    /// Restarts left; refilled by every sector handed out.
    retries: u8,
    /// Restarts since [`Self::start`].
    restarts: u16,
    state: State,
}

impl RegionRead {
    /// An idle read; [`Self::start`] arms it.
    pub const fn new() -> Self {
        Self {
            lba: 0,
            sectors: 0,
            landed: 0,
            stalled: false,
            stall_since: 0,
            retries: GROUP_RETRIES,
            restarts: 0,
            state: State::Idle,
        }
    }

    /// Arm a read of `sectors` sectors starting at absolute `lba`. The drive
    /// is touched on the first [`Self::poll`].
    pub fn start(&mut self, lba: u32, sectors: u32) {
        *self = Self {
            lba,
            sectors,
            landed: 0,
            stalled: false,
            stall_since: 0,
            retries: GROUP_RETRIES,
            restarts: 0,
            state: if sectors == 0 {
                State::Done
            } else {
                State::Armed
            },
        };
    }

    /// Sectors handed to the consumer so far.
    pub const fn landed(&self) -> u32 {
        self.landed
    }

    /// Times the read started its run over after the run failed.
    pub const fn restarts(&self) -> u16 {
        self.restarts
    }

    /// Whether a read is armed or running.
    pub const fn is_active(&self) -> bool {
        matches!(self.state, State::Armed | State::Restart | State::Reading)
    }

    /// Stop a read in flight; the transport pauses the drive at the next
    /// sector. The job returns to idle.
    pub fn abort(&mut self, cd: &mut CdController) {
        self.abort_with(&mut Console, cd);
    }

    fn abort_with<T: Transport>(&mut self, transport: &mut T, cd: &mut CdController) {
        if self.state == State::Reading {
            cd.abort_run(transport);
        }
        *self = Self::new();
    }

    /// Hand up to `max_sectors` landed sectors to `sink`, in order. Each call
    /// gets the sector's [`SECTOR_BYTES`] bytes in the controller's staging
    /// RAM, valid until `sink` returns; an `Err` code stops the read.
    ///
    /// With `wait` the pump stays with the drive for a sector already on its
    /// way (a loading screen); without it the pump returns as soon as nothing
    /// has landed.
    pub fn poll(
        &mut self,
        cd: &mut CdController,
        max_sectors: usize,
        wait: bool,
        sink: &mut impl FnMut(&[u8]) -> Result<(), u32>,
    ) -> RegionReadProgress {
        self.poll_with(&mut Console, cd, max_sectors, wait, sink)
    }

    pub(super) fn poll_with<T: Transport>(
        &mut self,
        transport: &mut T,
        cd: &mut CdController,
        max_sectors: usize,
        wait: bool,
        sink: &mut impl FnMut(&[u8]) -> Result<(), u32>,
    ) -> RegionReadProgress {
        if matches!(self.state, State::Armed | State::Restart) {
            if !transport.available() {
                self.state = State::Failed(STATUS_UNSUPPORTED);
            } else {
                // A restart resumes at the first sector not yet handed out.
                cd.begin_run(
                    transport,
                    self.lba + self.landed,
                    self.sectors - self.landed,
                );
                self.state = State::Reading;
            }
        }
        let mut handed = 0usize;
        while self.state == State::Reading && handed < max_sectors && self.landed < self.sectors {
            let landed = if wait {
                cd.wait_sector_within(transport, SECTOR_ARRIVAL_SPIN_LIMIT)
            } else {
                cd.try_sector(transport)
            };
            match landed {
                Ok(()) => {}
                Err(Stall::Slow) => {
                    // Merely slow: the display clock decides when to give up,
                    // measured from the last sector (or the read's start).
                    let now = transport.vblank_count();
                    if !self.stalled {
                        self.stalled = true;
                        self.stall_since = now;
                    } else if now.wrapping_sub(self.stall_since) > STALL_LIMIT_VBLANKS {
                        self.fail(transport, cd, STATUS_DATA_TIMEOUT);
                    }
                    break;
                }
                Err(Stall::Failed(status)) => {
                    if self.retries > 0 {
                        cd.abort_run(transport);
                        self.retries -= 1;
                        self.restarts = self.restarts.saturating_add(1);
                        self.stalled = false;
                        self.state = State::Restart;
                    } else {
                        self.fail(transport, cd, status);
                    }
                    break;
                }
            }
            self.stalled = false;
            self.retries = GROUP_RETRIES;
            // SAFETY: the sector the call above just landed is SECTOR_BYTES
            // readable bytes in the controller's staging RAM, untouched until
            // the next read call.
            let bytes = unsafe { core::slice::from_raw_parts(cd.sector_bytes(), SECTOR_BYTES) };
            if let Err(code) = sink(bytes) {
                self.fail(transport, cd, code);
                break;
            }
            self.landed += 1;
            handed += 1;
        }
        if self.state == State::Reading && self.landed >= self.sectors {
            self.state = State::Done;
        }
        match self.state {
            State::Done => RegionReadProgress::Done,
            State::Failed(code) => RegionReadProgress::Failed(code),
            _ => RegionReadProgress::Reading,
        }
    }

    fn fail<T: Transport>(&mut self, transport: &mut T, cd: &mut CdController, code: u32) {
        cd.abort_run(transport);
        self.state = State::Failed(code);
    }
}

impl Default for RegionRead {
    fn default() -> Self {
        Self::new()
    }
}
