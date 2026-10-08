//! The transport state machine ([`Engine`]) and its configuration and counters.

use crate::hw::CdHw;
use crate::request::{
    Completion, Failure, Outcome, Priority, Request, RequestState, SubmitError, Ticket,
    SECTOR_WORDS,
};
use psx_hw::cd::{
    irq as flag, CMD_PAUSE, CMD_READN, CMD_SEEKL, CMD_SETLOC, CMD_SETMODE, MODE_DOUBLE_SPEED,
};

/// Requests that can wait behind the active one.
pub const QUEUE_DEPTH: usize = 4;
/// Finished requests whose results [`Engine::state`] still remembers.
pub const RECENT_RESULTS: usize = 8;

/// Where the controller is in a transfer. The numbers are published in
/// `PSX_CD_STATS` and match the earlier game-local transports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Phase {
    /// Nothing has run since the engine was created.
    Idle = 0,
    /// Setloc sent, waiting for its acknowledge.
    SettingTarget = 1,
    /// SeekL sent, waiting for its acknowledge.
    StartingSeek = 2,
    /// Seek acknowledged, waiting for it to complete.
    Seeking = 3,
    /// Setmode sent, waiting for its acknowledge.
    SettingMode = 4,
    /// ReadN sent, waiting for its acknowledge. Sectors may already arrive.
    StartingRead = 5,
    /// Sectors are arriving.
    Reading = 6,
    /// Pause sent, waiting for its acknowledge.
    Stopping = 7,
    /// Pause acknowledged, waiting for the drive to stop.
    WaitingForStop = 8,
    /// The drive is stopped and the last transfer ended cleanly.
    Done = 9,
    /// A transfer failed; the drive's state is unknown. The next transfer
    /// starts with a recovery Pause.
    Failed = 10,
}

impl Phase {
    const fn is_terminal(self) -> bool {
        matches!(self, Phase::Idle | Phase::Done | Phase::Failed)
    }
}

/// Who may program the controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// Nobody has attached the transport yet, or it was detached: the
    /// program uses the blocking `SectorReader` or drives the controller
    /// itself, and no request starts.
    Boot,
    /// The transport reads. Requests run.
    Data,
    /// CD-DA or XA playback holds the drive. Requests queue but do not start.
    Audio,
}

/// Where an audio lease request stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseState {
    /// No lease is held or wanted.
    None,
    /// Asked for; the in-flight request is being stopped.
    Pending,
    /// The drive is the audio code's. The transport masks its interrupt
    /// source and starts nothing until the lease is released.
    Granted,
}

/// Whether the CD interrupt source stays open between transfers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrqMaskPolicy {
    /// Open from attach until detach or an audio lease.
    AlwaysOn,
    /// Open only while a request is queued or active, so polled code
    /// (the SDK's `SectorReader`) never shares the controller with the
    /// handler between transfers.
    OnlyWhileBusy,
}

/// How the engine behaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// A transfer with no sector, response or phase change for this many
    /// VBlanks fails with [`FailureKind::Watchdog`](crate::FailureKind).
    pub timeout_vblanks: u32,
    /// See [`IrqMaskPolicy`].
    pub irq_mask_policy: IrqMaskPolicy,
    /// Start the first transfer after an audio lease with a recovery Pause,
    /// so a drive that was playing or parked at an audio position is stopped
    /// before the seek. Costs one command; skip it only if the audio code
    /// always ends with its own Pause.
    pub pause_after_audio: bool,
    /// Measure how long each handler call takes, with Timer 2, which the
    /// transport then owns. The longest call is `max_irq_ticks`, in
    /// system-clock / 8 ticks that wrap after about 15.5 ms.
    pub time_handler: bool,
}

impl Config {
    /// The defaults: 600 VBlanks, interrupt source always on, recovery pause
    /// after audio, no handler timing.
    pub const DEFAULT: Config = Config {
        timeout_vblanks: 600,
        irq_mask_policy: IrqMaskPolicy::AlwaysOn,
        pause_after_audio: true,
        time_handler: false,
    };
}

impl Default for Config {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Counters, published as one block (`PSX_CD_STATS`) for tools that read
/// guest memory. All wrap at 2^32.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct StreamStats {
    /// Interrupts handled.
    pub irq_count: u32,
    /// Sectors stored at a destination.
    pub sectors: u32,
    /// Sectors popped and dropped: after a cancel, after the last sector of
    /// a request while the Pause was on its way, or with nothing to store to.
    pub discarded_sectors: u32,
    /// The longest handler call, in Timer 2 ticks (needs
    /// [`Config::time_handler`]).
    pub max_irq_ticks: u32,
    /// The current [`Phase`], as its number.
    pub phase: u32,
    /// The code of the latest failure ([`Failure::code`]); 0 after a start.
    pub error: u32,
    /// Requests that ended [`Outcome::Done`].
    pub requests_done: u32,
    /// Requests that ended [`Outcome::Cancelled`].
    pub requests_cancelled: u32,
    /// Requests that ended [`Outcome::Failed`].
    pub requests_failed: u32,
    /// Requests started by chaining onto the previous one, without a seek.
    pub chained: u32,
    /// Requests put back in the queue to resume after an error.
    pub resumed: u32,
    /// Submissions refused because the queue was full.
    pub rejected: u32,
}

#[derive(Clone, Copy)]
struct Active {
    ticket: Ticket,
    request: Request,
    /// Sectors a request resumed from an error had stored before this
    /// activation; `request` is only the rest.
    base: u32,
    received: u32,
    cancelled: bool,
    resumes_left: u8,
}

#[derive(Clone, Copy)]
struct Queued {
    ticket: Ticket,
    request: Request,
    /// Sectors already stored by earlier activations (see `Active::base`).
    base: u32,
    /// Submission order; resumed requests carry 0 so they go first within
    /// their priority.
    seq: u32,
    resumes_left: u8,
}

/// The CD transport.
///
/// One engine owns the drive. Foreground code submits [`Request`]s; the CD
/// interrupt handler calls [`Engine::on_interrupt`] once per interrupt, and
/// each call advances the machine by one controller event. Nothing in the
/// machine waits for the drive: a command is written and the machine returns,
/// and the response arrives as the next interrupt.
///
/// # A transfer
///
/// ```text
/// Setloc ──INT3──▶ SeekL ──INT3──▶ (seek) ──INT2──▶ Setmode(double speed)
///   ──INT3──▶ ReadN ──INT3──▶ INT1 × n (one sector each) ──▶ Pause
///   ──INT3──▶ (stop) ──INT2──▶ stopped
/// ```
///
/// The read is seek-first (Setloc, SeekL, then Setmode and ReadN), the form
/// the BIOS uses and the only one proven to stream byte-perfect on a console:
/// a bare Setloc plus ReadN starts delivering while the mechanism is still
/// settling. Each INT1 pops one sector into the request's destination. After
/// a failure the drive's state is unknown, so the next transfer opens with a
/// recovery Pause.
///
/// # Chaining
///
/// When the last sector of a request lands and the best queued request starts
/// at the next sector, the machine swaps the destination inside that same
/// interrupt and keeps reading: no Pause, no seek. A request is `Done` the
/// moment its last sector lands, even though the drive is still running on
/// to the Pause; [`Engine::is_idle`] says when it has stopped.
///
/// # Abort
///
/// [`Engine::cancel`] only sets a flag. The handler drops the next sector it
/// pops and issues Pause itself. Pausing from the foreground while sectors
/// keep arriving lost the Pause acknowledge on silicon (no interrupt for 600
/// VBlanks), so the foreground never does it.
///
/// # Generic over the drive
///
/// On the console the one instance lives in the crate's global and is reached
/// through the free functions at the crate root; on the host a scripted drive
/// stands in.
pub struct Engine<H: CdHw> {
    hw: H,
    config: Config,
    owner: Owner,
    lease_pending: bool,
    /// Whether the CPU-side CD source is currently open, as last written.
    source_open: bool,
    /// The first transfer starts with a recovery Pause.
    recover_next: bool,
    phase: Phase,
    /// Set while the Pause that opens a transfer after a failure is running.
    recovering: bool,
    error: u32,
    /// VBlank count at the last sign of life.
    stamp: u32,
    /// Absolute BCD Setloc parameters of the active request.
    location: [u8; 3],
    active: Option<Active>,
    /// One spare slot beyond [`QUEUE_DEPTH`]: a failing request that resumes
    /// goes back in the queue even when it is full.
    queue: [Option<Queued>; QUEUE_DEPTH + 1],
    results: [Option<Completion>; RECENT_RESULTS],
    results_written: u32,
    next_ticket: u32,
    next_seq: u32,
    stats: StreamStats,
}

impl<H: CdHw> Engine<H> {
    /// An engine in the [`Owner::Boot`] state: nothing runs until
    /// [`attach`](Self::attach).
    pub const fn new(hw: H, config: Config) -> Self {
        Engine {
            hw,
            config,
            owner: Owner::Boot,
            lease_pending: false,
            source_open: false,
            recover_next: false,
            phase: Phase::Idle,
            recovering: false,
            error: 0,
            stamp: 0,
            location: [0; 3],
            active: None,
            queue: [None; QUEUE_DEPTH + 1],
            results: [None; RECENT_RESULTS],
            results_written: 0,
            next_ticket: 1,
            next_seq: 1,
            stats: StreamStats {
                irq_count: 0,
                sectors: 0,
                discarded_sectors: 0,
                max_irq_ticks: 0,
                phase: 0,
                error: 0,
                requests_done: 0,
                requests_cancelled: 0,
                requests_failed: 0,
                chained: 0,
                resumed: 0,
                rejected: 0,
            },
        }
    }

    /// Replace the configuration. It applies from the next event.
    pub fn configure(&mut self, config: Config) {
        self.config = config;
    }

    /// The drive layer.
    pub fn hw(&self) -> &H {
        &self.hw
    }

    /// The drive layer, mutably.
    pub fn hw_mut(&mut self) -> &mut H {
        &mut self.hw
    }

    /// Who may program the controller.
    pub fn owner(&self) -> Owner {
        self.owner
    }

    /// The controller phase.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// A copy of the counters, with the current phase and error filled in.
    pub fn stats(&self) -> StreamStats {
        StreamStats {
            phase: self.phase as u32,
            error: self.error,
            ..self.stats
        }
    }

    /// Requests waiting behind the active one.
    pub fn queued_count(&self) -> usize {
        self.queue.iter().flatten().count()
    }

    /// Whether the drive is stopped and no request holds it. Queued requests
    /// may still be waiting for their turn (while the owner is not `Data`).
    pub fn is_idle(&self) -> bool {
        self.phase.is_terminal() && self.active.is_none()
    }

    // ----------------------------------------------------------------- owner

    /// Take the drive: [`Owner::Boot`] becomes [`Owner::Data`] and queued
    /// requests start.
    ///
    /// The controller must be in a clean state: the polled reader released,
    /// no command in flight. A no-op in any other state.
    pub fn attach(&mut self) {
        if self.owner != Owner::Boot {
            return;
        }
        self.owner = Owner::Data;
        self.recover_next = false;
        self.hw.discard_response();
        self.hw.acknowledge(flag::ACK_ALL);
        self.settle();
        self.sync_source();
    }

    /// Hand the drive back to polled code. Succeeds only when it is idle and
    /// nothing is queued or wanted; [`cancel_all`](Self::cancel_all) and wait
    /// for [`is_idle`](Self::is_idle) first.
    pub fn detach(&mut self) -> bool {
        if self.owner != Owner::Data
            || !self.is_idle()
            || self.queued_count() != 0
            || self.lease_pending
        {
            return false;
        }
        self.owner = Owner::Boot;
        self.sync_source();
        true
    }

    /// Where an audio lease stands.
    pub fn lease_state(&self) -> LeaseState {
        if self.owner == Owner::Audio {
            LeaseState::Granted
        } else if self.lease_pending {
            LeaseState::Pending
        } else {
            LeaseState::None
        }
    }

    /// Ask for the drive on behalf of CD-DA or XA playback.
    ///
    /// New requests are still accepted but queue. A request in flight is
    /// aborted at its next sector (it ends [`Outcome::Cancelled`] with what
    /// landed; resume it with [`Request::remaining_after`] after the lease).
    /// When the drive has stopped the lease is granted: the CD interrupt
    /// source is masked, and the audio code may program the controller
    /// directly. Poll [`lease_state`](Self::lease_state) until it reads
    /// [`LeaseState::Granted`] if this returns `Pending`.
    ///
    /// Before [`attach`](Self::attach) the program already holds the drive
    /// and this returns `Granted` without changing anything.
    pub fn request_audio_lease(&mut self) -> LeaseState {
        match self.owner {
            Owner::Boot => return LeaseState::Granted,
            Owner::Audio => return LeaseState::Granted,
            Owner::Data => {}
        }
        self.lease_pending = true;
        if let Some(active) = self.active.as_mut() {
            active.cancelled = true;
        }
        if self.is_idle() {
            self.grant_lease();
        }
        self.sync_source();
        self.lease_state()
    }

    /// Give the drive back after audio playback, or withdraw a lease that is
    /// still pending. Returns whether there was a lease to end.
    ///
    /// The audio code must have ended playback with Pause (never Stop: the
    /// motor then spins down, reports status 0 for a second or two, and reads
    /// started in that window failed on a console). The next transfer starts
    /// with a recovery Pause if [`Config::pause_after_audio`] is set.
    pub fn release_audio_lease(&mut self) -> bool {
        if self.owner == Owner::Data && self.lease_pending {
            self.lease_pending = false;
            return true;
        }
        if self.owner != Owner::Audio {
            return false;
        }
        self.hw.discard_response();
        self.hw.acknowledge(flag::ACK_ALL);
        self.owner = Owner::Data;
        self.recover_next = self.config.pause_after_audio;
        self.settle();
        self.sync_source();
        true
    }

    fn grant_lease(&mut self) {
        self.owner = Owner::Audio;
        self.lease_pending = false;
    }

    // -------------------------------------------------------------- requests

    /// Queue a request. It starts at once when the drive is free.
    ///
    /// Higher priority (lower number) goes first; equal priorities go in
    /// submission order, except that one starting at the sector after the
    /// active request's last is picked ahead of its peers so it chains
    /// without a seek.
    pub fn submit(&mut self, request: Request) -> Result<Ticket, SubmitError> {
        if request.destination.is_null() {
            return Err(SubmitError::NullDestination);
        }
        if request.destination as usize & 3 != 0 {
            return Err(SubmitError::MisalignedDestination);
        }
        if request.sectors == 0 {
            return Err(SubmitError::Empty);
        }
        if self.queued_count() >= QUEUE_DEPTH {
            self.stats.rejected = self.stats.rejected.wrapping_add(1);
            return Err(SubmitError::QueueFull);
        }
        let ticket = self.new_ticket();
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1).max(1);
        self.enqueue(Queued {
            ticket,
            request,
            base: 0,
            seq,
            resumes_left: request.resumes,
        });
        self.service();
        // A free drive picks the request up now; a busy one keeps it queued
        // and `settle` runs when it frees up.
        self.settle();
        self.sync_source();
        Ok(ticket)
    }

    /// Stop a request. Returns whether `ticket` was queued or active.
    ///
    /// A queued request is dropped at once ([`Outcome::Cancelled`], 0
    /// sectors). The active one is flagged: the handler drops the next
    /// sector, pauses the drive, and the request ends `Cancelled` with the
    /// sectors that had landed.
    pub fn cancel(&mut self, ticket: Ticket) -> bool {
        if self.active.is_some_and(|active| active.ticket == ticket) {
            self.cancel_active();
            return true;
        }
        for index in 0..self.queue.len() {
            if self.queue[index].is_some_and(|queued| queued.ticket == ticket) {
                self.drop_queued(index);
                return true;
            }
        }
        false
    }

    /// [`cancel`](Self::cancel) everything: the queue and the active request.
    pub fn cancel_all(&mut self) {
        for index in 0..self.queue.len() {
            self.drop_queued(index);
        }
        if self.active.is_some() {
            self.cancel_active();
        }
    }

    fn cancel_active(&mut self) {
        self.trace(2, 0);
        if let Some(active) = self.active.as_mut() {
            active.cancelled = true;
        }
    }

    fn drop_queued(&mut self, index: usize) {
        if let Some(queued) = self.queue[index].take() {
            self.record(Completion {
                ticket: queued.ticket,
                outcome: Outcome::Cancelled,
                received: queued.base,
            });
        }
    }

    /// Where `ticket` is. Also runs the no-progress check ([`service`](Self::service)).
    pub fn state(&mut self, ticket: Ticket) -> RequestState {
        self.service();
        self.sync_source();
        if let Some(active) = &self.active {
            if active.ticket == ticket {
                return RequestState::Active {
                    received: active.base + active.received,
                };
            }
        }
        if self.queue.iter().flatten().any(|q| q.ticket == ticket) {
            return RequestState::Queued;
        }
        if let Some(done) = self.results.iter().flatten().find(|c| c.ticket == ticket) {
            return RequestState::Finished(*done);
        }
        RequestState::Unknown
    }

    /// Foreground upkeep: fail a transfer that has made no progress for
    /// [`Config::timeout_vblanks`]. [`state`](Self::state) calls this; a main
    /// loop that never asks about a ticket should call it once per frame.
    pub fn service(&mut self) {
        if self.owner != Owner::Data || self.phase.is_terminal() {
            return;
        }
        let now = self.hw.vblank_count();
        if now.wrapping_sub(self.stamp) > self.config.timeout_vblanks {
            self.fail(Failure::watchdog(self.phase as u32));
            self.sync_source();
        }
    }

    // ------------------------------------------------------------- interrupt

    /// Handle one CD interrupt. Called by the interrupt handler (the crate's
    /// exception wrapper on the console, the test harness on the host).
    pub fn on_interrupt(&mut self) {
        let started = self.hw.clock();
        self.stats.irq_count = self.stats.irq_count.wrapping_add(1);
        if self.owner == Owner::Data {
            let code = self.hw.interrupt_code();
            self.trace(1, code);
            self.dispatch(code);
            self.sync_source();
        }
        let ticks = u32::from(self.hw.clock().wrapping_sub(started));
        self.stats.max_irq_ticks = self.stats.max_irq_ticks.max(ticks);
    }

    fn dispatch(&mut self, code: u8) {
        if code == flag::DATA_READY {
            match self.take_sector() {
                Ok(()) => {
                    self.hw.discard_response();
                    self.hw.acknowledge(code);
                    self.after_sector();
                }
                Err(failure) => {
                    self.hw.discard_response();
                    self.hw.acknowledge(flag::ACK_ALL);
                    self.fail(failure);
                }
            }
            return;
        }
        if code == flag::ERROR {
            let (status, detail) = self.hw.error_response();
            self.hw.discard_response();
            self.hw.acknowledge(flag::ACK_ALL);
            self.fail(Failure::drive(status, self.phase as u32, detail));
            return;
        }
        self.hw.discard_response();
        self.hw.acknowledge(code);
        match (self.phase, code) {
            (Phase::SettingTarget, flag::ACKNOWLEDGE) => {
                self.command(CMD_SEEKL, &[], Phase::StartingSeek)
            }
            (Phase::StartingSeek, flag::ACKNOWLEDGE) => self.change(Phase::Seeking),
            (Phase::Seeking, flag::COMPLETE) => {
                self.command(CMD_SETMODE, &[MODE_DOUBLE_SPEED], Phase::SettingMode)
            }
            (Phase::SettingMode, flag::ACKNOWLEDGE) => {
                self.command(CMD_READN, &[], Phase::StartingRead)
            }
            (Phase::StartingRead, flag::ACKNOWLEDGE) => {
                self.change(Phase::Reading);
                if self.active.is_some_and(|a| a.cancelled) {
                    self.pause();
                }
            }
            (Phase::Stopping, flag::ACKNOWLEDGE) => self.change(Phase::WaitingForStop),
            (Phase::WaitingForStop, flag::COMPLETE) => self.pause_complete(),
            (_, 0) => {}
            _ => self.fail(Failure::unexpected(self.phase as u32, code)),
        }
    }

    /// Pop the sector the interrupt announced; store it when a request wants
    /// it. Fails when the data FIFO never filled.
    fn take_sector(&mut self) -> Result<(), Failure> {
        let reading = matches!(self.phase, Phase::Reading | Phase::StartingRead);
        let (destination, words) = match self.active {
            Some(a) if reading && !a.cancelled && a.received < a.request.sectors => {
                // SAFETY: `received < sectors`, so this stays inside the
                // `sectors * 2048` bytes `Request::new_raw` vouched for.
                let at = unsafe {
                    a.request
                        .destination
                        .add(a.received as usize * SECTOR_WORDS as usize)
                };
                (at, a.request.store_words(a.received))
            }
            _ => (core::ptr::null_mut(), 0),
        };
        // SAFETY: `destination` is the sector's slot inside the active
        // request's buffer, valid for `words` words by the contract of
        // `Request::new_raw` (and unused when `words` is 0).
        if !unsafe { self.hw.pop_sector(destination, words) } {
            return Err(Failure::data_not_ready(self.phase as u32));
        }
        if words != 0 {
            if let Some(a) = self.active.as_mut() {
                a.received += 1;
            }
            self.stats.sectors = self.stats.sectors.wrapping_add(1);
        } else {
            self.stats.discarded_sectors = self.stats.discarded_sectors.wrapping_add(1);
        }
        self.stamp = self.hw.vblank_count();
        Ok(())
    }

    /// After a sector was popped and acknowledged: finish the request, chain
    /// onto the next, or pause for a cancel.
    fn after_sector(&mut self) {
        if !matches!(self.phase, Phase::Reading | Phase::StartingRead) {
            return;
        }
        let Some(active) = self.active else {
            return;
        };
        if active.cancelled {
            self.pause();
            return;
        }
        if active.received < active.request.sectors {
            return;
        }
        self.active = None;
        self.finish(active, Outcome::Done);
        match self.take_chain(active.request.end_lba()) {
            Some(next) => {
                self.start_chained(next);
                self.stats.chained = self.stats.chained.wrapping_add(1);
            }
            None => self.pause(),
        }
    }

    fn pause_complete(&mut self) {
        if self.recovering && self.active.is_some_and(|a| !a.cancelled) {
            // The recovery Pause is done: start the seek it was clearing the
            // way for.
            self.recovering = false;
            let location = self.location;
            self.hw.drop_data_request();
            self.command(CMD_SETLOC, &location, Phase::SettingTarget);
            return;
        }
        self.recovering = false;
        self.change(Phase::Done);
        self.hw.silence_output();
        if let Some(active) = self.active.take() {
            self.finish(active, Outcome::Cancelled);
        }
        self.settle();
    }

    // -------------------------------------------------------------- stepping

    fn change(&mut self, phase: Phase) {
        self.phase = phase;
        self.stamp = self.hw.vblank_count();
    }

    /// Send a command; its acknowledge and completion arrive as interrupts.
    fn command(&mut self, command: u8, params: &[u8], next: Phase) {
        self.trace(6, command);
        if !self.hw.issue(command, params) {
            self.fail(Failure::command_refused(command));
            return;
        }
        self.change(next);
    }

    fn pause(&mut self) {
        self.command(CMD_PAUSE, &[], Phase::Stopping);
    }

    /// End the transfer in the failed state. The active request resumes from
    /// where it stopped if it has resumes left, else it ends `Failed`.
    fn fail(&mut self, failure: Failure) {
        self.trace(4, (failure.0 >> 24) as u8);
        self.trace(5, (failure.0 >> 8) as u8);
        self.error = failure.0;
        self.recovering = false;
        self.change(Phase::Failed);
        self.hw.silence_output();
        self.hw.acknowledge(0);
        if let Some(active) = self.active.take() {
            if active.cancelled {
                self.finish(active, Outcome::Cancelled);
            } else if active.resumes_left > 0 && active.received < active.request.sectors {
                self.requeue_remainder(active);
            } else {
                self.finish(active, Outcome::Failed(failure));
            }
        }
        self.settle();
    }

    fn requeue_remainder(&mut self, active: Active) {
        // SAFETY: `received` is what this transfer stored, so the tail
        // begins inside the request's destination (`Request::new_raw`).
        let rest = unsafe { active.request.remaining_after(active.received) };
        if let Some(request) = rest {
            self.stats.resumed = self.stats.resumed.wrapping_add(1);
            self.enqueue(Queued {
                ticket: active.ticket,
                request,
                base: active.base + active.received,
                seq: 0,
                resumes_left: active.resumes_left - 1,
            });
        }
    }

    /// The drive has stopped (or is in an unknown state after a failure) and
    /// no request holds it: grant a waiting lease, start the next request, or
    /// stay idle.
    fn settle(&mut self) {
        if self.owner != Owner::Data || !self.is_ready_to_start() {
            return;
        }
        if self.lease_pending {
            self.grant_lease();
            return;
        }
        if let Some(index) = self.select(None) {
            if let Some(queued) = self.queue[index].take() {
                self.begin(queued);
            }
        }
    }

    fn is_ready_to_start(&self) -> bool {
        self.active.is_none() && matches!(self.phase, Phase::Idle | Phase::Done | Phase::Failed)
    }

    /// Start a transfer from a stopped drive.
    fn begin(&mut self, queued: Queued) {
        let recover = self.phase == Phase::Failed || self.recover_next;
        self.recover_next = false;
        self.error = 0;
        self.active = Some(Active {
            ticket: queued.ticket,
            request: queued.request,
            base: queued.base,
            received: 0,
            cancelled: false,
            resumes_left: queued.resumes_left,
        });
        self.location =
            psx_io::cd::lba_to_bcd_msf(psx_io::disc_base::shift_lba(queued.request.lba));
        self.trace(3, 0);
        if recover {
            // The drive may still be reading or playing: stop it, then seek.
            self.recovering = true;
            self.pause();
        } else {
            self.recovering = false;
            self.hw.drop_data_request();
            let location = self.location;
            self.command(CMD_SETLOC, &location, Phase::SettingTarget);
        }
    }

    /// Continue reading into the next request with no command in between.
    fn start_chained(&mut self, queued: Queued) {
        self.active = Some(Active {
            ticket: queued.ticket,
            request: queued.request,
            base: queued.base,
            received: 0,
            cancelled: false,
            resumes_left: queued.resumes_left,
        });
        self.stamp = self.hw.vblank_count();
    }

    /// The queued request to chain onto a request that ended just before
    /// `end_lba`: the best-ranked one, if it starts exactly there.
    fn take_chain(&mut self, end_lba: u32) -> Option<Queued> {
        // No owner or lease check: the handler only runs for the data owner,
        // and a lease request aborts the active request before it can end.
        let index = self.select(Some(end_lba))?;
        if self.queue[index]?.request.lba != end_lba {
            return None;
        }
        self.queue[index].take()
    }

    /// Index of the best queued request: most urgent priority first, then
    /// one that continues from `contiguous_with`, then submission order.
    fn select(&self, contiguous_with: Option<u32>) -> Option<usize> {
        let rank = |q: &Queued| -> (Priority, bool, u32) {
            (
                q.request.priority,
                contiguous_with != Some(q.request.lba),
                q.seq,
            )
        };
        let mut best: Option<usize> = None;
        for (index, slot) in self.queue.iter().enumerate() {
            let Some(candidate) = slot else {
                continue;
            };
            let better = match best.and_then(|b| self.queue[b].as_ref()) {
                None => true,
                Some(current) => rank(candidate) < rank(current),
            };
            if better {
                best = Some(index);
            }
        }
        best
    }

    fn enqueue(&mut self, queued: Queued) {
        if let Some(slot) = self.queue.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(queued);
        }
    }

    fn new_ticket(&mut self) -> Ticket {
        let ticket = Ticket(self.next_ticket);
        self.next_ticket = self.next_ticket.wrapping_add(1).max(1);
        ticket
    }

    fn finish(&mut self, active: Active, outcome: Outcome) {
        match outcome {
            Outcome::Done => self.stats.requests_done = self.stats.requests_done.wrapping_add(1),
            Outcome::Cancelled => {
                self.stats.requests_cancelled = self.stats.requests_cancelled.wrapping_add(1)
            }
            Outcome::Failed(_) => {
                self.stats.requests_failed = self.stats.requests_failed.wrapping_add(1)
            }
        }
        self.record(Completion {
            ticket: active.ticket,
            outcome,
            received: active.base + active.received,
        });
    }

    fn record(&mut self, completion: Completion) {
        let slot = self.results_written as usize % RECENT_RESULTS;
        self.results[slot] = Some(completion);
        self.results_written = self.results_written.wrapping_add(1);
    }

    // ------------------------------------------------------------ the source

    /// Whether the CD interrupt source should reach the CPU right now.
    pub fn source_wanted(&self) -> bool {
        match self.owner {
            Owner::Boot | Owner::Audio => false,
            Owner::Data => match self.config.irq_mask_policy {
                IrqMaskPolicy::AlwaysOn => true,
                IrqMaskPolicy::OnlyWhileBusy => {
                    !self.is_idle() || self.queue.iter().any(Option::is_some)
                }
            },
        }
    }

    /// Close the CPU-side CD source. The console wrapper calls this before it
    /// touches the engine from the foreground, so the handler cannot run
    /// meanwhile; every foreground method ends by opening it again if
    /// [`source_wanted`](Self::source_wanted).
    pub fn close_source(&mut self) {
        self.hw.set_source_enabled(false);
        self.source_open = false;
    }

    /// Record that the source was closed behind the engine's back (by the
    /// console wrapper, through `I_MASK` directly), so the next
    /// [`sync_source`](Self::sync_source) reopens it.
    pub fn note_source_closed(&mut self) {
        self.source_open = false;
    }

    /// Open or close the source to match [`source_wanted`](Self::source_wanted).
    pub fn sync_source(&mut self) {
        let wanted = self.source_wanted();
        if wanted != self.source_open {
            self.hw.set_source_enabled(wanted);
            self.source_open = wanted;
        }
    }

    #[inline(always)]
    fn trace(&mut self, kind: u32, value: u8) {
        if H::TRACE {
            let received = self.active.map_or(0, |a| a.received);
            self.hw.trace(
                kind << 24 | (self.phase as u32) << 16 | u32::from(value) << 8 | received & 0xff,
            );
        }
    }
}
