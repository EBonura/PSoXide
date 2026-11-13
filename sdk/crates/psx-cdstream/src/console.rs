//! The console side: the real drive, the one global [`Engine`], and the
//! dispatcher handler that feeds it.
//!
//! Everything here is for the target. (It also compiles under `cfg(doc)` so
//! the documentation shows it; none of it may run on the host.)
//!
//! # Interrupt path
//!
//! The handler is registered with psx-rt's dispatcher (`psx_rt::dispatch`)
//! for the CD source while the engine has the source open (a foreground call
//! masks it for its own length without unregistering). For an interrupt
//! with the CD source pending the dispatcher saves the interrupted CPU
//! context once on its private stack and calls the engine for one sector or
//! one response; psx-rt's own handler has already done the VBlank counter,
//! the display flip and the fault checks, and returns through the
//! GTE-aware path. The dispatcher never touches the interrupted stack, which
//! is what makes it safe with a scratchpad stack.
//!
//! # Foreground path
//!
//! Every foreground call clears the CD bit in `I_MASK` first, so the handler
//! cannot run while the engine is being touched, and sets it again afterwards
//! if the engine still has the source open. The registration changes only when
//! the engine opens or closes the source, not around every call.

use crate::engine::{Config, Engine, LeaseState, Owner, Phase, StreamStats};
use crate::hw::CdHw;
use crate::request::{Request, RequestState, SubmitError, Ticket};
use core::cell::UnsafeCell;
use psx_hw::cd::{index_status, irq as flag};
use psx_hw::irq::source;
use psx_io::irq;
use psx_io::periph::Cd;
use psx_io::timers::{self, Timer};

/// CD interrupt source bit in `I_STAT` and `I_MASK`.
const CD_BIT: u32 = 1 << source::CDROM;
/// Status reads to wait for room in the parameter FIFO.
const PARAM_POLLS: u32 = 32;
/// Status reads to wait for the data FIFO to fill after a sector interrupt.
const FIFO_POLLS: u32 = 256;
/// Bytes of the private stack the handler runs on: the dispatcher's, shared with
/// every other handler.
///
/// The deepest call measured used 248 bytes (a Half-Life run of five weapon
/// switches in the emulator, every sector popped by the handler), and the
/// dispatcher's saved context takes 112 more.
pub const HANDLER_STACK_BYTES: usize = psx_rt::dispatch::STACK_BYTES;

// -------------------------------------------------------------------- drive

/// The CD-ROM controller, through the SDK's register steps.
pub struct Mmio {
    time_handler: bool,
}

impl Mmio {
    const fn new() -> Self {
        Mmio {
            time_handler: false,
        }
    }
}

/// The controller token. The transport owns the controller whenever the
/// engine runs, which is what [`install`] took the real token for.
fn token() -> Cd {
    // SAFETY: a token is a logic guard (see `psx_io::periph`). `install`
    // holds the real one for as long as the engine may touch the
    // controller, and hands it back only when the engine is detached or
    // leased out.
    unsafe { Cd::steal() }
}

impl CdHw for Mmio {
    const TRACE: bool = cfg!(feature = "trace");

    fn issue(&mut self, command: u8, params: &[u8]) -> bool {
        let mut cd = token();
        cd.set_irq_enable_mask(0);
        cd.acknowledge_all_and_reset_parameters();
        cd.discard_response();
        cd.reset_parameter_fifo();
        for &param in params {
            if !cd.wait_parameter_room(PARAM_POLLS) {
                return false;
            }
            cd.send_parameter_byte(param);
        }
        cd.send_command_byte(command);
        cd.set_irq_enable_mask(flag::ALL);
        true
    }

    fn interrupt_code(&mut self) -> u8 {
        token().irq_flag_value()
    }

    fn error_response(&mut self) -> (u8, u8) {
        let mut cd = token();
        let status = cd.read_response_byte();
        (status, cd.read_response_byte())
    }

    fn discard_response(&mut self) {
        token().discard_response();
    }

    fn acknowledge(&mut self, bits: u8) {
        token().acknowledge_irq(bits);
    }

    fn silence_output(&mut self) {
        token().set_irq_enable_mask(0);
    }

    fn drop_data_request(&mut self) {
        token().clear_data_request();
    }

    unsafe fn pop_sector(&mut self, destination: *mut u32, store_words: usize) -> bool {
        let mut cd = token();
        cd.request_data();
        let mut ready = false;
        for _ in 0..FIFO_POLLS {
            if cd.status_register() & index_status::DATA_FIFO_NOT_EMPTY != 0 {
                ready = true;
                break;
            }
        }
        if !ready {
            return false;
        }
        for word in 0..crate::SECTOR_WORDS as usize {
            let b0 = u32::from(cd.read_data_byte());
            let b1 = u32::from(cd.read_data_byte());
            let b2 = u32::from(cd.read_data_byte());
            let b3 = u32::from(cd.read_data_byte());
            if word < store_words {
                // SAFETY: the caller guarantees `destination` is valid for
                // `store_words` word writes (`CdHw::pop_sector`'s contract).
                unsafe {
                    destination
                        .add(word)
                        .write_volatile(b0 | b1 << 8 | b2 << 16 | b3 << 24);
                }
            }
        }
        true
    }

    fn set_source_enabled(&mut self, enabled: bool) {
        // Only the mask bit: a flag that arrived while the source was closed
        // must still be pending when it opens, or its sector is lost. (An
        // acknowledge of the CPU-side bit here dropped sectors in the first
        // emulator run, because the foreground masks the source around
        // every call.) Stale bits are cleared by `attach` and by the
        // preamble of every command.
        //
        // The handler is registered with the dispatcher only while the source is
        // open. Closed, nobody here services it: code that opens the source itself (a
        // polled read, audio playback) has its strays acknowledged by psx-rt, instead
        // of an exception taken again for ever. The registration comes before the
        // mask bit and goes after it, so the source is never open without it.
        if enabled {
            psx_rt::dispatch::attach(psx_rt::dispatch::Source::Cdrom, on_cd_interrupt);
            irq::set_mask(irq::mask() | CD_BIT);
        } else {
            irq::set_mask(irq::mask() & !CD_BIT);
            psx_rt::dispatch::detach(psx_rt::dispatch::Source::Cdrom);
        }
    }

    fn vblank_count(&mut self) -> u32 {
        psx_rt::interrupts::vblank_count()
    }

    fn clock(&mut self) -> u16 {
        if self.time_handler {
            timers::counter(Timer::Timer2)
        } else {
            0
        }
    }

    #[cfg(feature = "trace")]
    fn trace(&mut self, word: u32) {
        // SAFETY: the ring is written only here, from the handler or from
        // a foreground section with the CD source closed, so the two never
        // overlap; readers (tools) only look.
        unsafe {
            let n = core::ptr::read_volatile(&raw const PSX_CD_TRACE_N);
            core::ptr::write_volatile(&raw mut PSX_CD_TRACE_N, n.wrapping_add(1));
            let slot = (&raw mut PSX_CD_TRACE)
                .cast::<u32>()
                .add((n & 127) as usize);
            core::ptr::write_volatile(slot, word);
        }
    }
}

// ------------------------------------------------------------------- state

struct Global(UnsafeCell<Engine<Mmio>>);

// SAFETY: the engine is reached from one CPU only, from the foreground with
// the CD source closed or from the interrupt handler, never both at once.
unsafe impl Sync for Global {}

static ENGINE: Global = Global(UnsafeCell::new(Engine::new(Mmio::new(), Config::DEFAULT)));

/// The controller token while the transport holds it.
struct TokenSlot(UnsafeCell<Option<Cd>>);

// SAFETY: touched from the foreground only, and only inside `with_engine`
// sections or before the handler can run, never from the handler.
unsafe impl Sync for TokenSlot {}

impl TokenSlot {
    fn is_held(&self) -> bool {
        // SAFETY: foreground-only access, one thread of control (see `Sync`).
        unsafe { (*self.0.get()).is_some() }
    }

    fn put(&self, cd: Cd) {
        // SAFETY: as `is_held`.
        unsafe { *self.0.get() = Some(cd) };
    }

    fn take(&self) -> Option<Cd> {
        // SAFETY: as `is_held`.
        unsafe { (*self.0.get()).take() }
    }
}

static TOKEN: TokenSlot = TokenSlot(UnsafeCell::new(None));

/// The counters, as one block for tools that read guest memory:
/// [`StreamStats`] with its field order. Updated after every interrupt and
/// every foreground call.
#[no_mangle]
pub static mut PSX_CD_STATS: StreamStats = StreamStats {
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
};

/// The last 128 controller events (feature `trace`):
/// `kind << 24 | phase << 16 | (flag or command) << 8 | received & 0xff`.
#[cfg(feature = "trace")]
#[no_mangle]
pub static mut PSX_CD_TRACE: [u32; 128] = [0; 128];
/// Events recorded so far (feature `trace`); the next slot is `n & 127`.
#[cfg(feature = "trace")]
#[no_mangle]
pub static mut PSX_CD_TRACE_N: u32 = 0;

/// Copy the counters into `PSX_CD_STATS`. Out of line: every foreground entry and the
/// handler end with this, and inlined it was a dozen loads and stores in each.
#[inline(never)]
fn publish(engine: &Engine<Mmio>) {
    // SAFETY: a plain store of a `repr(C)` block that only this module
    // writes; tools read it from outside the program.
    unsafe { core::ptr::write_volatile(&raw mut PSX_CD_STATS, engine.stats()) }
}

/// Start of a foreground section: mask the CD source before the engine
/// reference exists, so the handler cannot be running (or start) while this
/// one is live. Only the `I_MASK` bit: the engine's own view of the source, and
/// the dispatcher registration that goes with it, stay as they are. Out of line
/// so each caller carries a call, not the sequence.
#[inline(never)]
fn enter() -> &'static mut Engine<Mmio> {
    let mask = irq::mask();
    irq::set_mask(mask & !CD_BIT);
    // SAFETY: the source is masked, so the handler (the only other user of
    // the engine) cannot run until `leave` unmasks it, and no other
    // foreground reference exists because the CPU has one thread of control.
    unsafe { &mut *ENGINE.0.get() }
}

/// End of a foreground section: open or close the source as the engine now
/// wants it (registering or unregistering the handler with it), unmask it if
/// it is open, and publish the counters.
#[inline(never)]
fn leave(engine: &mut Engine<Mmio>) {
    engine.sync_source();
    if engine.is_source_open() {
        irq::set_mask(irq::mask() | CD_BIT);
    }
    publish(engine);
}

/// Run `f` on the engine with the CD source closed, then publish the
/// counters and reopen the source if the engine wants it.
///
/// The section's entry and exit are calls, which the compiler cannot move
/// the engine accesses across (both touch `I_MASK` through volatile stores).
#[inline(always)]
fn with_engine<R>(f: impl FnOnce(&mut Engine<Mmio>) -> R) -> R {
    let engine = enter();
    let result = f(engine);
    leave(engine);
    result
}

// ------------------------------------------------------------------ handler

/// The dispatcher's handler for the CD source (`psx_rt::dispatch`): runs with CPU
/// interrupts off on the dispatcher's stack. It acknowledges `I_STAT` itself, after
/// the engine has read and cleared the controller's flags.
fn on_cd_interrupt() {
    // SAFETY: the dispatcher runs with CPU interrupts off, and every foreground
    // access closes the CD source before it takes its reference, so this is the
    // only live reference to the engine.
    let engine = unsafe { &mut *ENGINE.0.get() };
    engine.on_interrupt();
    publish(engine);
}

// ------------------------------------------------------------------- set-up

/// Take over the CD-ROM controller and start serving requests; the handler
/// registers with the dispatcher whenever the CD source is open.
///
/// The controller must be idle and nobody else may use it from now on: if the
/// boot code read with `SectorReader`, call `SectorReader::release` for the
/// token and pass it here. The token comes back from [`uninstall`] (to read
/// with the polled reader again) or [`take_audio_lease`] (to play audio).
///
/// If psx-rt's handler is not in the vector yet this installs it first
/// (`install_vblank_counter`).
///
/// Returns the token if the transport is already installed.
///
/// With [`Config::time_handler`] the transport takes Timer 2 for itself.
///
/// # Errors
///
/// The token comes back when the transport is already installed or the CD source
/// already has a handler.
///
/// # Panics
///
/// Never in practice: the token it takes back was stored a few lines earlier.
pub fn install(cd: Cd, config: Config) -> Result<(), Cd> {
    if TOKEN.is_held() {
        return Err(cd);
    }
    TOKEN.put(cd);
    let mask = irq::mask();
    irq::set_mask(mask & !CD_BIT);
    if !psx_rt::interrupts::is_stack_safe_handler_installed() {
        psx_rt::interrupts::install_vblank_counter();
    }
    if config.time_handler {
        // System clock / 8, free running, no interrupt: wraps after about
        // 15.5 ms.
        timers::set_mode(Timer::Timer2, psx_hw::timers::mode::clock_source(2));
    }
    with_engine(|engine| {
        engine.hw_mut().time_handler = config.time_handler;
        engine.configure(config);
        engine.attach();
    });
    Ok(())
}

/// Give the controller back to polled code. Succeeds only when the transport
/// is idle with nothing queued; call [`cancel_all`] and poll [`is_idle`]
/// first. The CD source is closed and the handler unregistered.
pub fn uninstall() -> Option<Cd> {
    let detached = with_engine(Engine::detach);
    if !detached {
        return None;
    }
    TOKEN.take()
}

/// Install the transport if it is not installed, taking the controller token itself. For a program that reads with the polled
/// `SectorReader` and hands only some reads (a streamed asset the frame loop must
/// not wait for) to the transport: call it before each such read and nothing else
/// needs tracking.
///
/// Idempotent, and `install_vblank_counter` (which Half-Life calls for every map)
/// does not disturb it: the registration is with the dispatcher, not the vector.
/// Returns `false` when it could not: an audio lease holds the controller. Polled commands may run whenever no request
/// is queued or active and [`is_idle`] reads true: with
/// [`IrqMaskPolicy::OnlyWhileBusy`] the CD interrupt source is closed then.
#[must_use]
pub fn ensure_installed(config: Config) -> bool {
    if TOKEN.is_held() {
        // The handler stays registered whatever calls `install_vblank_counter`.
        return true;
    }
    if owner() == Owner::Audio {
        return false;
    }
    // SAFETY: a token is a logic guard (see `psx_io::periph`). The caller keeps
    // polled commands away from the controller while a request is queued or
    // active, which is the only time the transport programs it.
    let cd = unsafe { Cd::steal() };
    install(cd, config).is_ok()
}

/// Cancel every read and wait, for at most `max_vblanks` VBlanks, until the drive
/// has stopped and nothing is queued. `true` once it has: polled commands can follow.
///
/// Cancelling takes effect at the next sector (the handler drops it and pauses the
/// drive itself), so this normally returns within a few milliseconds; a console
/// Pause after a long read can take a few frames. The wait also gives up after a
/// fixed number of polls, so a stopped VBlank clock cannot hang it.
#[must_use]
pub fn stop_and_wait(max_vblanks: u32) -> bool {
    cancel_all();
    let start = psx_rt::interrupts::vblank_count();
    let mut polls = 0u32;
    loop {
        if is_idle() && queued_count() == 0 {
            return true;
        }
        polls += 1;
        if polls >= STOP_POLLS
            || psx_rt::interrupts::vblank_count().wrapping_sub(start) >= max_vblanks
        {
            return false;
        }
    }
}

/// Polls of [`is_idle`] before [`stop_and_wait`] gives up whatever the VBlank clock says.
const STOP_POLLS: u32 = 1 << 18;

// ------------------------------------------------------------ the foreground

/// Change the configuration; it applies from the next event. See
/// [`Engine::configure`]. ([`Config::time_handler`] is only read by
/// [`install`].)
pub fn configure(config: Config) {
    with_engine(|engine| engine.configure(config));
}

/// Start the longest-handler measurement over. See
/// [`Engine::reset_max_irq_ticks`].
pub fn reset_max_irq_ticks() {
    with_engine(Engine::reset_max_irq_ticks);
}

/// Queue a read. See [`Engine::submit`].
pub fn submit(request: Request) -> Result<Ticket, SubmitError> {
    with_engine(|engine| engine.submit(request))
}

/// Cancel a read. See [`Engine::cancel`].
pub fn cancel(ticket: Ticket) -> bool {
    with_engine(|engine| engine.cancel(ticket))
}

/// Cancel everything. See [`Engine::cancel_all`].
pub fn cancel_all() {
    with_engine(Engine::cancel_all);
}

/// Where a read is. See [`Engine::state`].
pub fn state(ticket: Ticket) -> RequestState {
    with_engine(|engine| engine.state(ticket))
}

/// Check for a stalled transfer. See [`Engine::service`].
pub fn service() {
    with_engine(Engine::service);
}

/// Whether the drive is stopped and no read holds it.
pub fn is_idle() -> bool {
    with_engine(|engine| engine.is_idle())
}

/// Reads waiting behind the active one.
pub fn queued_count() -> usize {
    with_engine(|engine| engine.queued_count())
}

/// Who may program the controller.
pub fn owner() -> Owner {
    with_engine(|engine| engine.owner())
}

/// The controller phase.
pub fn phase() -> Phase {
    with_engine(|engine| engine.phase())
}

/// A copy of the counters.
pub fn stats() -> StreamStats {
    with_engine(|engine| engine.stats())
}

// -------------------------------------------------------------------- audio

/// Ask for the drive on behalf of CD-DA or XA playback. See
/// [`Engine::request_audio_lease`]; once this reads `Granted`, collect the
/// controller token with [`take_audio_lease`].
pub fn request_audio_lease() -> LeaseState {
    with_engine(|engine| engine.request_audio_lease())
}

/// Where the lease stands.
pub fn lease_state() -> LeaseState {
    with_engine(|engine| engine.lease_state())
}

/// The controller token, once the lease is granted (and not yet taken). The
/// audio code programs the controller through it until it hands it back with
/// [`release_audio_lease`]; the CD interrupt source stays closed meanwhile.
pub fn take_audio_lease() -> Option<Cd> {
    with_engine(|engine| {
        if engine.lease_state() != LeaseState::Granted {
            return None;
        }
        TOKEN.take()
    })
}

/// End the lease: hand the controller back after the audio code has ended
/// playback with Pause (not Stop). Gives the token back if there was no
/// lease to end.
pub fn release_audio_lease(cd: Cd) -> Result<(), Cd> {
    with_engine(|engine| {
        if engine.owner() != Owner::Audio {
            return Err(cd);
        }
        TOKEN.put(cd);
        engine.release_audio_lease();
        Ok(())
    })
}

/// Take the drive for audio only if it is free right now, in one step: the
/// controller token if the lease was granted, `None` if a read is in flight.
/// Nothing is left pending and the read is not aborted, so a read submitted
/// afterwards is never parked behind a lease granted later. See
/// [`Engine::try_audio_lease`]; end the lease with [`release_audio_lease`].
pub fn try_take_audio_lease() -> Option<Cd> {
    with_engine(|engine| {
        if engine.try_audio_lease() {
            TOKEN.take()
        } else {
            None
        }
    })
}

/// Withdraw a lease that was asked for and has not been granted. Returns
/// whether one was pending. See [`Engine::withdraw_audio_lease`].
pub fn withdraw_audio_lease() -> bool {
    with_engine(Engine::withdraw_audio_lease)
}

// --------------------------------------------------------------- diagnostics

/// Bytes of the dispatcher's private stack (shared with every handler) never
/// touched since boot. A value near 0 means a handler overflowed it.
pub fn handler_stack_unused_bytes() -> usize {
    psx_rt::dispatch::stack_unused_bytes()
}
