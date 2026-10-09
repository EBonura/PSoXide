//! A contiguous run of disc sectors, read through `psx-cdstream` and handed to
//! the caller one sector at a time.
//!
//! The transport writes whole sectors straight into the destination of a
//! request from the CD interrupt handler. The callers here want sectors in
//! order, copied and checksummed into their final homes as they land, so a
//! run reads into a small ring of staging windows: it keeps up to `W`
//! requests of `S` sectors each queued, which the transport
//! chains without a pause or a seek, hands out the sectors of the oldest
//! window as they arrive, and tops the queue up as windows are consumed. If the
//! foreground falls behind and the ring runs dry the drive stops and the next
//! window pays a normal seek; nothing is lost, the run only slows down.
//!
//! A read the arbiter aborts (an audio lease) ends its window with the
//! sectors that landed. The run resubmits the rest, so a lease costs one seek
//! and no sector is read twice or skipped.

use psx_cdstream::{
    Failure, FailureKind, Outcome, Priority, Request, RequestState, SubmitError, Ticket,
};

use super::{
    SECTOR_WORDS, STATUS_CD_ERROR, STATUS_DATA_TIMEOUT, STATUS_OK, STATUS_READ_ACK_TIMEOUT,
};

/// Sectors per staging window (one transport request) in the game's ring.
pub(super) const RING_WINDOW_SECTORS: usize = 1;
/// Staging windows in the game's ring. Two is the least that chains: while one
/// window is consumed and resubmitted the other is already queued behind the
/// active request, so the drive reads straight on. Each window costs
/// `RING_WINDOW_SECTORS * 2048` bytes of `.bss`; the playtest guest has little
/// to spare, so this is the smallest ring and not the safest one: a foreground
/// that stays away longer than one sector time (6.7 ms) lets the drive pause
/// and pays a seek for the next sector.
pub(super) const RING_WINDOWS: usize = 2;

/// The game's staging RAM.
pub(super) type CdStage = Stage<RING_WINDOWS, RING_WINDOW_SECTORS>;
/// The game's run.
pub(super) type CdRun = Run<RING_WINDOWS, RING_WINDOW_SECTORS>;

/// Drive errors a request may resume from before it gives up.
const RESUMES_PER_REQUEST: u8 = 3;

/// Times a run restarts itself after a window ended in a way that leaves the
/// sectors unaccounted for (the transport lost the request, ended it short, or
/// failed it for good) before the failure becomes the caller's. A restart
/// stops the transport, then reads on from the first sector not yet handed
/// out: it costs a seek and neither skips nor repeats a sector.
const RUN_RESTARTS: u8 = 3;

/// Foreground spins to wait for the transport to stop after an abort. The
/// transport's own watchdog ends a stuck transfer long before this runs out.
const QUIESCE_SPIN_LIMIT: u32 = 50_000_000;

/// What the run needs from the CD transport. The console implementation calls
/// `psx-cdstream`; the tests stand in a scripted drive.
pub(super) trait Transport {
    fn submit(&mut self, request: Request) -> Result<Ticket, SubmitError>;
    fn state(&mut self, ticket: Ticket) -> RequestState;
    fn cancel_all(&mut self);
    fn is_idle(&mut self) -> bool;
    /// Foreground upkeep (the no-progress watchdog).
    fn service(&mut self);
    /// Whether there is a drive to read (false on the host build).
    fn available(&self) -> bool {
        true
    }
    /// A new run is about to submit: make sure the transport exists and the
    /// drive is available for data.
    fn begin_transfer(&mut self);
    /// The display clock in VBlanks, for deadlines that must hold in real
    /// time. The host build has no clock.
    fn vblank_count(&mut self) -> u32 {
        0
    }
}

/// The console's transport: the global `psx-cdstream` engine.
#[cfg(target_arch = "mips")]
pub(super) struct Hardware;

#[cfg(target_arch = "mips")]
impl Transport for Hardware {
    fn submit(&mut self, request: Request) -> Result<Ticket, SubmitError> {
        psx_cdstream::submit(request)
    }

    fn state(&mut self, ticket: Ticket) -> RequestState {
        psx_cdstream::state(ticket)
    }

    fn cancel_all(&mut self) {
        psx_cdstream::cancel_all();
    }

    fn is_idle(&mut self) -> bool {
        psx_cdstream::is_idle()
    }

    fn service(&mut self) {
        psx_cdstream::service();
    }

    fn begin_transfer(&mut self) {
        psx_engine::cd_drive::ensure_transport();
        // Music holding the lease would leave this read queued forever. It is
        // paused where it stands and resumes once the reads are done.
        psx_engine::cd_drive::yield_music_for_data();
    }

    fn vblank_count(&mut self) -> u32 {
        psx_engine::cd_drive::vblank_count()
    }
}

/// The host build's transport: there is no drive, so every read reports
/// unsupported before it submits anything.
#[cfg(not(target_arch = "mips"))]
pub(super) struct Unavailable;

#[cfg(not(target_arch = "mips"))]
impl Transport for Unavailable {
    fn submit(&mut self, _request: Request) -> Result<Ticket, SubmitError> {
        Err(SubmitError::QueueFull)
    }

    fn state(&mut self, _ticket: Ticket) -> RequestState {
        RequestState::Unknown
    }

    fn cancel_all(&mut self) {}

    fn is_idle(&mut self) -> bool {
        true
    }

    fn service(&mut self) {}

    fn available(&self) -> bool {
        false
    }

    fn begin_transfer(&mut self) {}
}

/// A `Ticket` before any request: the value of a zeroed word (the struct is one
/// `u32`), so the staging state stays all-zero bytes and lives in `.bss`.
// SAFETY: `Ticket` wraps a `u32`, for which all-zero is a valid value.
const NO_TICKET: Ticket = unsafe { core::mem::zeroed() };

#[derive(Clone, Copy)]
struct Window {
    ticket: Ticket,
    /// Run sector index of the window's first sector.
    first: u32,
    sectors: u32,
    /// Sectors earlier requests of this window landed before the arbiter
    /// cancelled them. The current request holds the rest.
    landed: u32,
    /// Sectors already handed to the caller.
    taken: u32,
}

impl Window {
    const EMPTY: Self = Self {
        ticket: NO_TICKET,
        first: 0,
        sectors: 0,
        landed: 0,
        taken: 0,
    };
}

/// RAM the transport writes into: `W` windows of `S` sectors. All zero bytes,
/// so it costs no disc space.
pub(super) struct Stage<const W: usize, const S: usize> {
    sectors: [[[u32; SECTOR_WORDS]; S]; W],
}

impl<const W: usize, const S: usize> Stage<W, S> {
    pub(super) const fn zeroed() -> Self {
        Self {
            sectors: [[[0; SECTOR_WORDS]; S]; W],
        }
    }

    fn window_ptr(&mut self, slot: usize) -> *mut u32 {
        self.sectors[slot].as_mut_ptr().cast::<u32>()
    }

    fn sector_ptr(&self, slot: usize, index: u32) -> *const u8 {
        self.sectors[slot][index as usize].as_ptr().cast::<u8>()
    }
}

/// A compiler barrier. An empty `asm!` clobbers memory by default;
/// `compiler_fence` would lower to the MIPS-II SYNC instruction, which this CPU
/// does not have.
#[inline(always)]
fn memory_barrier() {
    #[cfg(target_arch = "mips")]
    // SAFETY: an empty asm, no operands and no effects beyond the barrier.
    unsafe {
        core::arch::asm!("", options(nostack, preserves_flags))
    };
}

/// What a request for the next sector found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Poll {
    /// The sector is in the caller's buffer.
    Sector,
    /// Not landed yet.
    Pending,
    /// Every sector of the run has been handed out.
    Done,
    /// The run failed; the code is one of the `STATUS_*` values.
    Failed(u32),
}

/// One contiguous run of sectors, read through a ring of `W` windows of `S`
/// sectors.
pub(super) struct Run<const W: usize, const S: usize> {
    lba: u32,
    total: u32,
    /// Sectors assigned to windows so far.
    submitted: u32,
    /// Sectors handed to the caller.
    taken: u32,
    windows: [Window; W],
    head: u8,
    live: u8,
    /// A `STATUS_*` code, or 0.
    failure: u32,
    /// The transport may still be writing into the staging windows.
    dirty: bool,
    /// The sector the last `Poll::Sector` handed out: window and index.
    view_slot: u8,
    view_index: u32,
    /// Restarts left (see [`RUN_RESTARTS`]); refilled by every sector handed out.
    restarts: u8,
}

impl<const W: usize, const S: usize> Run<W, S> {
    pub(super) const ZERO: Self = Self {
        lba: 0,
        total: 0,
        submitted: 0,
        taken: 0,
        windows: [Window::EMPTY; W],
        head: 0,
        live: 0,
        failure: STATUS_OK,
        dirty: false,
        view_slot: 0,
        view_index: 0,
        restarts: RUN_RESTARTS,
    };

    /// Start reading `sectors` sectors from `lba` (program-relative). Anything
    /// still running from an earlier run is stopped first.
    pub(super) fn begin<T: Transport>(
        &mut self,
        transport: &mut T,
        stage: &mut Stage<W, S>,
        lba: u32,
        sectors: u32,
    ) {
        self.quiesce(transport);
        *self = Self::ZERO;
        self.lba = lba;
        self.total = sectors;
        transport.begin_transfer();
        self.fill(transport, stage);
    }

    /// Stop the run and wait for the transport to let go of the staging RAM.
    pub(super) fn abort<T: Transport>(&mut self, transport: &mut T) {
        self.dirty = true;
        self.quiesce(transport);
        *self = Self::ZERO;
    }

    fn quiesce<T: Transport>(&mut self, transport: &mut T) {
        if self.live == 0 && !self.dirty {
            return;
        }
        transport.cancel_all();
        let mut spins = 0u32;
        while !transport.is_idle() && spins < QUIESCE_SPIN_LIMIT {
            transport.service();
            spins += 1;
        }
        self.live = 0;
        self.dirty = false;
    }

    fn fill<T: Transport>(&mut self, transport: &mut T, stage: &mut Stage<W, S>) {
        while usize::from(self.live) < W && self.submitted < self.total && self.failure == STATUS_OK
        {
            let slot = (usize::from(self.head) + usize::from(self.live)) % W;
            let sectors = (self.total - self.submitted).min(S as u32);
            // SAFETY: the window is `S` sectors of word-aligned
            // staging RAM; nothing reads it until the request's sectors have
            // landed (`next_sector` checks) and nothing else writes it while
            // the window is live.
            let request = unsafe {
                Request::new_raw(self.lba + self.submitted, sectors, stage.window_ptr(slot))
            }
            .with_resumes(RESUMES_PER_REQUEST);
            let Ok(ticket) = transport.submit(request) else {
                break;
            };
            self.windows[slot] = Window {
                ticket,
                first: self.submitted,
                sectors,
                landed: 0,
                taken: 0,
            };
            self.live += 1;
            self.submitted += sectors;
        }
    }

    /// Stop everything and read on from the first sector not yet handed out,
    /// if a restart is left. The caller keeps the sector it was last given (the
    /// staging windows are only rewritten by later requests). `false` when the
    /// budget is spent.
    fn restart<T: Transport>(&mut self, transport: &mut T, stage: &mut Stage<W, S>) -> bool {
        if self.restarts == 0 {
            return false;
        }
        let restarts = self.restarts - 1;
        let (view_slot, view_index) = (self.view_slot, self.view_index);
        self.dirty = true;
        self.quiesce(transport);
        self.lba += self.taken;
        self.total -= self.taken;
        self.taken = 0;
        self.submitted = 0;
        self.head = 0;
        self.live = 0;
        self.windows = [Window::EMPTY; W];
        self.restarts = restarts;
        self.view_slot = view_slot;
        self.view_index = view_index;
        transport.begin_transfer();
        self.fill(transport, stage);
        true
    }

    fn fail<T: Transport>(&mut self, transport: &mut T, status: u32) -> Poll {
        self.failure = status;
        self.dirty = true;
        transport.cancel_all();
        Poll::Failed(status)
    }

    /// The sector the last [`Poll::Sector`] handed out: 2048 readable bytes,
    /// valid until the next call to [`Self::next_sector`] or [`Self::begin`].
    pub(super) fn sector_ptr(&self, stage: &Stage<W, S>) -> *const u8 {
        stage.sector_ptr(usize::from(self.view_slot), self.view_index)
    }

    /// Hand out the next sector of the run if it has landed.
    pub(super) fn next_sector<T: Transport>(
        &mut self,
        transport: &mut T,
        stage: &mut Stage<W, S>,
    ) -> Poll {
        loop {
            if self.failure != STATUS_OK {
                return Poll::Failed(self.failure);
            }
            if self.taken >= self.total {
                return Poll::Done;
            }
            self.fill(transport, stage);
            if self.live == 0 {
                return Poll::Pending;
            }
            let slot = usize::from(self.head);
            let window = self.windows[slot];
            let (landed, ended) = match transport.state(window.ticket) {
                RequestState::Queued => (window.landed, None),
                RequestState::Active { received } => (window.landed + received, None),
                RequestState::Finished(done) => (window.landed + done.received, Some(done.outcome)),
                // The result fell out of the transport's memory, which a run
                // that polls every window cannot cause. Treat it as a lost read.
                RequestState::Unknown => {
                    // The transport has no record of the window. Whatever it
                    // is doing for us, start the rest of the run over.
                    if self.restart(transport, stage) {
                        return Poll::Pending;
                    }
                    return self.fail(transport, STATUS_DATA_TIMEOUT);
                }
            };
            let landed = landed.min(window.sectors);
            if window.taken < landed {
                // The handler wrote this sector through a pointer the compiler
                // cannot see. The state read above is volatile; this barrier
                // keeps the caller's loads of the sector behind it.
                memory_barrier();
                self.restarts = RUN_RESTARTS;
                self.view_slot = slot as u8;
                self.view_index = window.taken;
                self.windows[slot].taken += 1;
                self.taken += 1;
                if window.taken + 1 == window.sectors {
                    self.head = ((usize::from(self.head) + 1) % W) as u8;
                    self.live -= 1;
                }
                return Poll::Sector;
            }
            match ended {
                None => return Poll::Pending,
                Some(Outcome::Done) => {
                    // Done with fewer sectors than the window holds.
                    if self.restart(transport, stage) {
                        return Poll::Pending;
                    }
                    return self.fail(transport, STATUS_DATA_TIMEOUT);
                }
                Some(Outcome::Failed(failure)) => {
                    if self.restart(transport, stage) {
                        return Poll::Pending;
                    }
                    return self.fail(transport, status_for(failure));
                }
                Some(Outcome::Cancelled) => {
                    // The arbiter took the drive. Queue the rest of the window
                    // ahead of the other windows and carry on when it returns.
                    let rest = window.sectors - landed;
                    // SAFETY: `landed < sectors`, so the advanced pointer stays
                    // inside the window; same ownership as in `fill`.
                    let request = unsafe {
                        Request::new_raw(
                            self.lba + window.first + landed,
                            rest,
                            stage.window_ptr(slot).add(landed as usize * SECTOR_WORDS),
                        )
                    }
                    .with_priority(Priority::URGENT)
                    .with_resumes(RESUMES_PER_REQUEST);
                    let Ok(ticket) = transport.submit(request) else {
                        return Poll::Pending;
                    };
                    self.windows[slot].ticket = ticket;
                    self.windows[slot].landed = landed;
                }
            }
        }
    }
}

/// The `STATUS_*` code a transport failure reports.
fn status_for(failure: Failure) -> u32 {
    match failure.kind() {
        FailureKind::CommandRefused { .. } => STATUS_READ_ACK_TIMEOUT,
        FailureKind::DataNotReady | FailureKind::Watchdog => STATUS_DATA_TIMEOUT,
        FailureKind::Drive { .. }
        | FailureKind::UnexpectedInterrupt { .. }
        | FailureKind::Unknown => STATUS_CD_ERROR,
    }
}

#[cfg(test)]
mod tests;
