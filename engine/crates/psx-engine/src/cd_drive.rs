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
//!
//! # Console timings
//!
//! The emulator hands the drive over in a few milliseconds. A console does
//! not (hardware tests v1.28, console-tested): Pause on a playing track takes
//! [`PAUSE_COMPLETE_MS`], and the first data sector after audio arrives about
//! [`FIRST_SECTOR_AFTER_AUDIO_MS`] after the Pause. Audio coming back after a
//! read is of the same order. Every wait on a handoff is therefore bounded by
//! the VBlank clock, never by a count of polls (a poll count is a proxy for
//! time and the emulator hides how bad a proxy it is), and sized from
//! [`HANDOFF_VBLANKS`].

use core::cell::UnsafeCell;

/// Time a Pause takes to complete on a playing track, in milliseconds.
/// Console-tested (hardware tests v1.28).
pub const PAUSE_COMPLETE_MS: u32 = 123;

/// Time from the Pause to the first data sector of the next read, in
/// milliseconds. Console-tested (hardware tests v1.28).
pub const FIRST_SECTOR_AFTER_AUDIO_MS: u32 = 945;

/// The slowest drive transition measured on the console: a data read issued
/// while a Stop is still spinning the motor down delivers its first sector
/// this long after the read command, in milliseconds (hardware tests v1.28,
/// record 0x315, one sample). Console-tested. A read on a stopped drive takes
/// 1951 ms and the first sector after audio 945 ms, so a stall limit that
/// clears twice this value clears them all.
pub const MOTOR_RESTART_MS: u32 = 2721;

/// One handoff between music and data, in VBlanks: about a second on a
/// console in either direction. The slowest console figure above rounded up
/// to whole seconds at the 60 Hz tick the engine budgets in.
pub const HANDOFF_VBLANKS: u32 = 60;

const _: () = assert!(FIRST_SECTOR_AFTER_AUDIO_MS < HANDOFF_VBLANKS * 1000 / 60);

/// Polled-command spin budget for the position query a yield issues. The
/// controller acknowledges quickly; only completions take mechanical time.
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
        // Takes the drive only if it is free right now and leaves no request
        // pending: a pending lease would be granted a moment later, when the
        // last transfer stops, and a data reader that queued behind it in the
        // belief that the drive is free would wait on a lease nobody holds.
        // Music asks again a tick later instead.
        let Some(token) = psx_cdstream::try_take_audio_lease() else {
            return false;
        };
        with(|state| {
            state.token = Some(token);
            state.holding = true;
        });
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

/// Run `f` with the controller token while music holds the lease. `None` when
/// it does not, so a polled command can never collide with the transport.
/// `f` must not call back into this module.
#[cfg(target_arch = "mips")]
pub(crate) fn with_token<R>(f: impl FnOnce(&mut psx_io::periph::Cd) -> R) -> Option<R> {
    with(|state| state.token.as_mut().map(f))
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
    let response = with_token(|cd| cd.try_command(psx_hw::cd::CMD_GETLOCP, &[], COMMAND_SPINS))??;
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

/// The display clock, in VBlanks. Zero on the host.
pub fn vblank_count() -> u32 {
    #[cfg(target_arch = "mips")]
    {
        psx_rt::interrupts::vblank_count()
    }
    #[cfg(not(target_arch = "mips"))]
    {
        0
    }
}

/// Stop routing the audio and Pause the drive (it stays spun up).
pub(crate) fn stop_playback() {
    #[cfg(target_arch = "mips")]
    {
        psx_spu::enable_cd_audio(false);
        let _ = with_token(|cd| pause_until_complete(cd));
    }
}

/// Send Pause and wait for its completion interrupt, giving up after
/// [`HANDOFF_VBLANKS`]. Returns whether the drive reported completion.
#[cfg(target_arch = "mips")]
fn pause_until_complete(cd: &mut psx_io::periph::Cd) -> bool {
    let Some(saved) = cd.dispatch_command(psx_hw::cd::CMD_PAUSE, &[], COMMAND_SPINS) else {
        return false;
    };
    let start = vblank_count();
    let mut acknowledged = false;
    let done = loop {
        if vblank_count().wrapping_sub(start) > HANDOFF_VBLANKS {
            break false;
        }
        match cd.irq_flag_value() {
            0 => {}
            3 => {
                cd.discard_response();
                cd.acknowledge_irq(3);
                acknowledged = true;
            }
            2 if acknowledged => {
                cd.discard_response();
                cd.acknowledge_irq(2);
                break true;
            }
            other => {
                // A drive error, or an interrupt that is not ours to keep.
                cd.discard_response();
                cd.acknowledge_irq(other);
                if other == 5 {
                    break false;
                }
            }
        }
    };
    // A Pause still in flight may acknowledge late: keep the output masked
    // until the transport takes the controller back.
    cd.restore_irq_output(if done { saved } else { 0 });
    done
}
