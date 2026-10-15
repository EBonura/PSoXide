//! Who holds the CD drive: streamed data reads or XA music.
//!
//! One laser cannot read a data sector and stream an XA song at once (music is
//! a read of the song's file), so the two share the drive through
//! `psx-cdstream`'s arbiter. Data reads own it by default (the transport is
//! installed on first use). Music takes it with an audio lease: the transport
//! stops the read in flight at the next sector, closes its interrupt source
//! and hands the controller token over, and the music code then drives the
//! controller through an [`psx_io::cd::xa::Player`] built on that token.
//! Releasing the lease hands the token back and queued reads carry on.
//!
//! Music always ends with Pause, never Stop: after Stop the motor spins down
//! and reads started in the following second or two fail on a console.
//!
//! A data reader that needs the drive while music holds it calls
//! [`yield_music_for_data`]. The player notes how far into the song the head
//! was, pauses, releases the lease and later restarts the song from that point
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

/// How far into a song the head was, in sectors from the song's own start.
pub(crate) type SongOffset = u32;

/// Sectors a single-speed XA file is read at per second.
pub(crate) const SECTORS_PER_SECOND: u32 = 75;

/// What one [`music_poll`] saw. Only the console and the host tests poll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(any(target_arch = "mips", test)), allow(dead_code))]
pub(crate) enum Poll {
    /// No song is started.
    Idle,
    /// The song is playing or still seeking.
    Playing,
    /// The song reached its end and the drive restarted it from its first
    /// sector.
    Looped,
    /// The song reached its end and the drive was paused.
    Finished,
}

struct State {
    #[cfg(target_arch = "mips")]
    installed: bool,
    /// Owns the controller token while music holds the lease.
    #[cfg(target_arch = "mips")]
    player: Option<psx_io::cd::xa::Player>,
    /// The music code holds the lease and may program the controller.
    holding: bool,
    /// Music was paused to make room for data; it wants the drive back.
    yielded: bool,
    /// How far into the song the yielded head was.
    resume: Option<SongOffset>,
    /// Sectors into the song at which the current play started.
    start_offset: SongOffset,
}

impl State {
    const fn new() -> Self {
        Self {
            #[cfg(target_arch = "mips")]
            installed: false,
            #[cfg(target_arch = "mips")]
            player: None,
            holding: false,
            yielded: false,
            resume: None,
            start_offset: 0,
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
    /// A song was started (`offset` sectors into it).
    Play {
        /// First sector of the song's XA file.
        lba: u32,
        /// Sectors the song spans.
        span: u32,
        /// Sectors into the song the play started at.
        offset: u32,
        /// Whether the drive restarts the song by itself.
        looping: bool,
    },
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

/// Let the host tests fix how far into the song the head is (sectors from the
/// start of the current play), as the next poll or yield reports it.
#[cfg(test)]
pub(crate) fn host_set_position(position: Option<SongOffset>) {
    HOST_POSITION.with(|slot| slot.set(position));
}

/// Let the host tests script what the next polls answer, one per poll. With
/// nothing scripted a started song reports [`Poll::Playing`].
#[cfg(test)]
pub(crate) fn host_script_polls(polls: &[Poll]) {
    HOST_POLLS.with(|queue| queue.borrow_mut().extend(polls.iter().copied()));
}

/// Let the host tests make the next [`music_play`] calls fail.
#[cfg(test)]
pub(crate) fn host_set_play_fails(fails: bool) {
    HOST_PLAY_FAILS.with(|flag| flag.set(fails));
}

#[cfg(test)]
std::thread_local! {
    static HOST_POSITION: core::cell::Cell<Option<SongOffset>> =
        const { core::cell::Cell::new(None) };
    static HOST_POLLS: core::cell::RefCell<std::collections::VecDeque<Poll>> =
        const { core::cell::RefCell::new(std::collections::VecDeque::new()) };
    static HOST_PLAY_FAILS: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
    static HOST_PLAYING: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
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
        let mut player = psx_io::cd::xa::Player::new(token);
        // The drive's own mixer has to pass the song to the SPU; 0x80 is unity.
        player.set_volume(0x80, 0x80);
        with(|state| {
            state.player = Some(player);
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

/// Run `f` with the music player while music holds the lease. `None` when it
/// does not, so a polled command can never collide with the transport. `f`
/// must not call back into this module.
#[cfg(target_arch = "mips")]
fn with_player<R>(f: impl FnOnce(&mut psx_io::cd::xa::Player) -> R) -> Option<R> {
    with(|state| state.player.as_mut().map(f))
}

/// Start `track` and stream it from `offset` sectors in. With `looping` the
/// drive restarts it from the first sector of the song by itself when
/// [`music_poll`] sees the end. Blocks for the five drive commands of a start
/// (a few milliseconds on a warm drive). `false` when music does not hold the
/// lease or the drive refused a command; the caller tries again later.
pub(crate) fn music_play(
    track: &psx_level::LevelMusicTrack,
    offset: SongOffset,
    looping: bool,
) -> bool {
    if !with(|state| state.holding) {
        return false;
    }
    let offset = offset.min(track.span_sectors.saturating_sub(1));
    with(|state| state.start_offset = offset);
    #[cfg(target_arch = "mips")]
    {
        use psx_io::cd::xa::{DriveSpeed, File};
        // A song shorter than the file it shares ends at its own length: the
        // player ends it at the file's span, which is the song's span here.
        let file = File::new(
            track.file_lba + offset,
            track.span_sectors - offset + psx_hw::cd::xa::END_GUARD_SECTORS,
            track.file_number,
            DriveSpeed::Single,
        );
        with_player(|player| {
            // A song already streaming is paused first, as the launcher does
            // between songs; an idle player sends the drive nothing.
            player.stop();
            player.play(file.song(track.channel), looping).is_ok()
        })
        .unwrap_or(false)
    }
    #[cfg(not(target_arch = "mips"))]
    {
        #[cfg(test)]
        {
            record(HostEvent::Play {
                lba: track.file_lba,
                span: track.span_sectors,
                offset,
                looping,
            });
            if HOST_PLAY_FAILS.with(|flag| flag.get()) {
                return false;
            }
            HOST_PLAYING.with(|flag| flag.set(true));
            HOST_POSITION.with(|slot| slot.set(Some(0)));
        }
        #[cfg(not(test))]
        let _ = looping;
        true
    }
}

/// Call about once per frame while a song plays: notices the end of the song
/// and loops or stops. On a console it asks the drive where the head is, one
/// polled command.
pub(crate) fn music_poll() -> Poll {
    #[cfg(target_arch = "mips")]
    {
        use psx_io::cd::xa::Event;
        with_player(|player| match player.poll() {
            Event::Idle => Poll::Idle,
            Event::Playing => Poll::Playing,
            Event::Looped => Poll::Looped,
            Event::Finished => Poll::Finished,
        })
        .unwrap_or(Poll::Idle)
    }
    #[cfg(all(not(target_arch = "mips"), test))]
    {
        if !HOST_PLAYING.with(|flag| flag.get()) {
            return Poll::Idle;
        }
        let scripted = HOST_POLLS.with(|queue| queue.borrow_mut().pop_front());
        let poll = scripted.unwrap_or(Poll::Playing);
        match poll {
            Poll::Finished => HOST_PLAYING.with(|flag| flag.set(false)),
            Poll::Looped => HOST_POSITION.with(|slot| slot.set(Some(0))),
            _ => {}
        }
        poll
    }
    #[cfg(all(not(target_arch = "mips"), not(test)))]
    {
        Poll::Idle
    }
}

/// How far into the song the head was at the last poll, in sectors from the
/// song's own start, or `None` while the seek of a start has not landed.
pub(crate) fn music_head() -> Option<SongOffset> {
    let offset = with(|state| state.start_offset);
    #[cfg(target_arch = "mips")]
    {
        with_player(|player| {
            player.is_streaming().then(|| {
                // The player counts from the first sector it was given.
                let sectors = (u64::from(player.elapsed_millis()) * u64::from(SECTORS_PER_SECOND))
                    .div_ceil(1000) as u32;
                offset.saturating_add(sectors)
            })
        })
        .flatten()
    }
    #[cfg(not(target_arch = "mips"))]
    {
        #[cfg(test)]
        {
            HOST_POSITION
                .with(|slot| slot.get())
                .map(|head| offset + head)
        }
        #[cfg(not(test))]
        {
            let _ = offset;
            None
        }
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
        let player = with(|state| {
            state.holding = false;
            state.player.take()
        });
        match player {
            Some(mut player) => {
                // The caller paused playback. A pause the drive refused during
                // the seek of a start is retried by one more poll, so the
                // transport never takes over a drive that is still reading
                // the song.
                let _ = player.poll();
                let token = player.release();
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
            HOST_PLAYING.with(|flag| flag.set(false));
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
/// released; the music player restarts the song from that point when the
/// reads are done. Returns whether music was displaced. Data readers call this
/// before they submit, so a read never waits on a lease nobody will give up.
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
/// the pending resume: `Some(offset)` once (the offset is `None` when the head
/// had not been seen in the song, and the song then restarts from its
/// beginning).
pub(crate) fn take_resume() -> Option<Option<SongOffset>> {
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

/// Where the head is: one poll to refresh the player's reading, then how far
/// into the song it is.
fn playback_position() -> Option<SongOffset> {
    #[cfg(target_arch = "mips")]
    {
        let _ = with_player(|player| player.poll());
    }
    music_head()
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
        let _ = with_player(|player| player.stop());
    }
    #[cfg(all(not(target_arch = "mips"), test))]
    HOST_PLAYING.with(|flag| flag.set(false));
}
