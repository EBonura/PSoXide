//! Who holds the CD drive: streamed data reads or CD-DA music.
//!
//! One laser cannot read a data sector and play a CD-DA track at once, so the
//! two share the drive through `psx-cdstream`'s arbiter. Data reads own it by
//! default (the transport is installed on first use). Music takes it with an
//! audio lease: the transport stops the read in flight at the next sector,
//! closes its interrupt source and hands the controller token over, and the
//! music code then drives the controller with polled commands. Releasing the
//! lease hands the token back and queued reads carry on.
//!
//! Music always ends with Pause, never Stop: after Stop the motor spins down
//! and reads started in the following second or two fail on a console.
//!
//! A data reader that needs the drive while music holds it calls
//! [`yield_music_for_data`]. The player notes where the track was
//! (GetlocP), pauses, releases the lease and later resumes from that position
//! once no data read is queued or running ([`take_resume`]).

use core::cell::UnsafeCell;

/// Polled-command spin budget for the pause and position query a yield issues.
#[cfg(target_arch = "mips")]
const COMMAND_SPINS: u32 = 131_072;

/// Absolute disc position of CD-DA playback: BCD minute, second, frame.
pub(crate) type DiscPosition = [u8; 3];

struct State {
    #[cfg(target_arch = "mips")]
    installed: bool,
    #[cfg(target_arch = "mips")]
    token: Option<psx_io::periph::Cd>,
    /// The music code holds the lease and may program the controller.
    holding: bool,
    /// Music was paused to make room for data; it wants the drive back.
    yielded: bool,
    /// Where the yielded track was, when the position query answered.
    resume: Option<DiscPosition>,
}

impl State {
    const fn new() -> Self {
        Self {
            #[cfg(target_arch = "mips")]
            installed: false,
            #[cfg(target_arch = "mips")]
            token: None,
            holding: false,
            yielded: false,
            resume: None,
        }
    }
}

#[cfg(not(test))]
struct Shared(UnsafeCell<State>);

// SAFETY: the target is single threaded and no interrupt handler touches this
// state (the CD handler lives in psx-cdstream and never calls up into the
// engine). Host builds only compile this path for the editor frontend, which
// never reads the CD drive.
#[cfg(not(test))]
unsafe impl Sync for Shared {}

#[cfg(not(test))]
static STATE: Shared = Shared(UnsafeCell::new(State::new()));

#[cfg(test)]
extern crate std;

// Host tests run on parallel threads, so each gets its own drive.
#[cfg(test)]
std::thread_local! {
    static STATE: UnsafeCell<State> = const { UnsafeCell::new(State::new()) };
    static DATA_BUSY: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
    static EVENTS: core::cell::RefCell<std::vec::Vec<HostEvent>> =
        const { core::cell::RefCell::new(std::vec::Vec::new()) };
}

/// What the host build records in place of drive commands.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostEvent {
    /// Music asked for and received the lease.
    Acquire,
    /// Music handed the lease back.
    Release,
    /// Music was paused and the position saved for a data read.
    Yield,
}

#[cfg(test)]
fn record(event: HostEvent) {
    EVENTS.with(|events| events.borrow_mut().push(event));
}

#[cfg(test)]
pub(crate) fn host_events() -> std::vec::Vec<HostEvent> {
    EVENTS.with(|events| events.borrow().clone())
}

/// Let the host tests say whether a data read is queued or running.
#[cfg(test)]
pub(crate) fn host_set_data_busy(busy: bool) {
    DATA_BUSY.with(|flag| flag.set(busy));
}

/// Let the host tests fix the position the next yield reports.
#[cfg(test)]
pub(crate) fn host_set_position(position: Option<DiscPosition>) {
    HOST_POSITION.with(|slot| slot.set(position));
}

#[cfg(test)]
std::thread_local! {
    static HOST_POSITION: core::cell::Cell<Option<DiscPosition>> =
        const { core::cell::Cell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    #[cfg(not(test))]
    {
        // SAFETY: single threaded, and `f` never re-enters `with`.
        f(unsafe { &mut *STATE.0.get() })
    }
    #[cfg(test)]
    {
        STATE.with(|cell| {
            // SAFETY: thread local, and `f` never re-enters `with`.
            f(unsafe { &mut *cell.get() })
        })
    }
}

/// Install the CD transport if no code has yet. Data readers and the music
/// player both call this before they touch the drive. It must run after the
/// engine's display clock has installed the exception handler (any scene or
/// loading step does), because installing that handler again would replace the
/// transport's. A no-op on the host.
pub fn ensure_transport() {
    #[cfg(target_arch = "mips")]
    with(|state| {
        if state.installed {
            return;
        }
        // SAFETY: nothing else in the engine owns the controller token, the
        // polled CD-DA commands use the SDK's free functions, and no read has
        // started yet.
        let cd = unsafe { psx_io::periph::Cd::steal() };
        // The earlier owner may be a BIOS-booted program mid-sequence; attach
        // drops any stale response before the first transfer.
        let _ = psx_cdstream::install(cd, psx_cdstream::Config::DEFAULT);
        state.installed = true;
    });
}

/// The audio-lease half of the transport, so the acquire policy can be tested
/// against a scripted drive.
pub trait LeaseSource {
    /// Ask the transport for the audio lease.
    fn request(&mut self) -> psx_cdstream::LeaseState;
    /// Collect the controller token once the lease is granted.
    fn take(&mut self) -> bool;
    /// Withdraw a lease that was asked for and not yet granted.
    fn withdraw(&mut self);
}

/// Ask for the lease without leaving a request behind.
///
/// A lease the transport has not granted yet is withdrawn on the spot rather
/// than left pending. A pending lease is granted by the transport itself a
/// moment later, when its last transfer stops, and by then a data reader may
/// have queued behind it believing the drive is free; the reads would wait on
/// a lease nobody holds. Music asks again a tick later instead.
pub fn try_lease<L: LeaseSource>(source: &mut L) -> bool {
    match source.request() {
        psx_cdstream::LeaseState::Granted => source.take(),
        psx_cdstream::LeaseState::Pending => {
            source.withdraw();
            false
        }
        psx_cdstream::LeaseState::None => false,
    }
}

#[cfg(target_arch = "mips")]
struct Console;

#[cfg(target_arch = "mips")]
impl LeaseSource for Console {
    fn request(&mut self) -> psx_cdstream::LeaseState {
        psx_cdstream::request_audio_lease()
    }

    fn take(&mut self) -> bool {
        let Some(token) = psx_cdstream::take_audio_lease() else {
            return false;
        };
        with(|state| state.token = Some(token));
        true
    }

    fn withdraw(&mut self) {
        let _ = psx_cdstream::withdraw_audio_lease();
    }
}

/// Ask for the drive on behalf of music. `true` once the lease is granted and
/// the music code may issue polled commands; `false` while a data read is
/// still being stopped (ask again next tick) or a data read is queued or
/// running, which music never pre-empts.
pub(crate) fn music_acquire() -> bool {
    if with(|state| state.holding) {
        return true;
    }
    if data_busy() {
        return false;
    }
    #[cfg(target_arch = "mips")]
    {
        ensure_transport();
        if !try_lease(&mut Console) {
            return false;
        }
        with(|state| state.holding = true);
        true
    }
    #[cfg(not(target_arch = "mips"))]
    {
        with(|state| state.holding = true);
        #[cfg(test)]
        record(HostEvent::Acquire);
        true
    }
}

/// Whether the music code currently holds the lease.
pub(crate) fn music_holds() -> bool {
    with(|state| state.holding)
}

/// Hand the drive back. The caller has paused playback first (Pause, never
/// Stop). Also withdraws a lease that was asked for and not yet granted.
pub(crate) fn music_release() {
    #[cfg(target_arch = "mips")]
    {
        let token = with(|state| {
            state.holding = false;
            state.token.take()
        });
        match token {
            Some(token) => {
                let _ = psx_cdstream::release_audio_lease(token);
            }
            None => {
                let _ = psx_cdstream::withdraw_audio_lease();
            }
        }
    }
    #[cfg(not(target_arch = "mips"))]
    {
        let was = with(|state| core::mem::replace(&mut state.holding, false));
        #[cfg(test)]
        if was {
            record(HostEvent::Release);
        }
        let _ = was;
    }
}

/// Forget a pending resume: music was stopped on purpose, not displaced.
pub(crate) fn clear_yield() {
    with(|state| {
        state.yielded = false;
        state.resume = None;
    });
}

/// Whether a data read is queued or running on the transport.
pub fn data_busy() -> bool {
    #[cfg(target_arch = "mips")]
    {
        !psx_cdstream::is_idle() || psx_cdstream::queued_count() != 0
    }
    #[cfg(all(not(target_arch = "mips"), test))]
    {
        DATA_BUSY.with(|flag| flag.get())
    }
    #[cfg(all(not(target_arch = "mips"), not(test)))]
    {
        false
    }
}

/// Make the drive available for a data read.
///
/// If music holds the lease it is paused where it stands and the lease is
/// released; the music player resumes from that position when the reads are
/// done. Returns whether music was displaced. Data readers call this before
/// they submit, so a read never waits on a lease nobody will give up.
pub fn yield_music_for_data() -> bool {
    if !with(|state| state.holding) {
        return false;
    }
    let position = playback_position();
    stop_playback();
    with(|state| {
        state.yielded = true;
        state.resume = position;
    });
    music_release();
    #[cfg(test)]
    record(HostEvent::Yield);
    true
}

/// If music was displaced by a data read and the drive is free again, claim
/// the pending resume: `Some(position)` once (the position is `None` when the
/// query had not answered, and the track then restarts from its beginning).
pub(crate) fn take_resume() -> Option<Option<DiscPosition>> {
    if !with(|state| state.yielded) || data_busy() {
        return None;
    }
    Some(with(|state| {
        state.yielded = false;
        state.resume.take()
    }))
}

/// Whether music is waiting for a displacing data read to finish.
pub(crate) fn resume_pending() -> bool {
    with(|state| state.yielded)
}

#[cfg(target_arch = "mips")]
fn playback_position() -> Option<DiscPosition> {
    let response = psx_io::cd::try_command(psx_hw::cd::CMD_GETLOCP, &[], COMMAND_SPINS)?;
    let bytes = response.bytes();
    // Track, index, relative MSF, absolute MSF.
    if bytes.len() < 8 || bytes[0] == 0 {
        return None;
    }
    Some([bytes[5], bytes[6], bytes[7]])
}

#[cfg(not(target_arch = "mips"))]
fn playback_position() -> Option<DiscPosition> {
    #[cfg(test)]
    {
        HOST_POSITION.with(|slot| slot.get())
    }
    #[cfg(not(test))]
    {
        None
    }
}

/// Stop routing the audio and Pause the drive (it stays spun up).
pub(crate) fn stop_playback() {
    #[cfg(target_arch = "mips")]
    {
        psx_spu::enable_cd_audio(false);
        let _ = psx_io::cd::try_pause_until_complete(COMMAND_SPINS);
    }
}
