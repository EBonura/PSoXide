//! The console side: the real drive, the one global [`Engine`], and the
//! exception wrapper that feeds it.
//!
//! Everything here is for the target. (It also compiles under `cfg(doc)` so
//! the documentation shows it; none of it may run on the host.)
//!
//! # Interrupt path
//!
//! The wrapper sits in the general exception vector in front of psx-rt's
//! handler. For an interrupt with the CD source pending it saves the
//! interrupted CPU context, switches to a private 2 KiB stack, calls the
//! engine for one sector or one response, restores the context, and jumps to
//! psx-rt's handler either way, so the VBlank counter, the display flip and
//! the fault handling run exactly as before. Anything else goes straight
//! through. The wrapper never touches the interrupted stack, which is what
//! makes it safe with a scratchpad stack; [`install`] declares it so
//! (`psx_rt::interrupts::declare_stack_safe_handler`).
//!
//! # Foreground path
//!
//! Every foreground call closes the CD source in `I_MASK` first, so the
//! handler cannot run while the engine is being touched, and opens it again
//! afterwards if the engine wants it open.

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
/// Bytes of private stack the handler runs on.
pub const HANDLER_STACK_BYTES: usize = 2048;
/// Fill byte of the handler stack, to measure how much of it was used.
const STACK_FILL: u8 = 0xa5;

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
        // emulator run, because the foreground closes the source around
        // every call.) Stale bits are cleared by `attach` and by the
        // preamble of every command.
        let mask = irq::mask();
        if enabled {
            irq::set_mask(mask | CD_BIT);
        } else {
            irq::set_mask(mask & !CD_BIT);
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

#[repr(C, align(16))]
struct Stack([u8; HANDLER_STACK_BYTES]);

/// The handler's private stack, painted with `0xa5` by [`install`] so a tool
/// can see how much of it a run used ([`handler_stack_unused_bytes`] does the
/// same from inside).
#[no_mangle]
static mut PSX_CD_IRQ_STACK: Stack = Stack([0; HANDLER_STACK_BYTES]);
/// Where the wrapper saves the interrupted context.
#[no_mangle]
static mut PSX_CD_IRQ_CONTEXT: [u32; 34] = [0; 34];

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

fn publish(engine: &Engine<Mmio>) {
    // SAFETY: a plain store of a `repr(C)` block that only this module
    // writes; tools read it from outside the program.
    unsafe { core::ptr::write_volatile(&raw mut PSX_CD_STATS, engine.stats()) }
}

/// Run `f` on the engine with the CD source closed, then publish the
/// counters and reopen the source if the engine wants it.
fn with_engine<R>(f: impl FnOnce(&mut Engine<Mmio>) -> R) -> R {
    // Close the source before the reference exists, so the handler cannot be
    // running (or start) while this one is live.
    let mask = irq::mask();
    irq::set_mask(mask & !CD_BIT);
    // An empty asm is a compiler barrier. `compiler_fence` lowers to the
    // MIPS-II SYNC instruction, which this CPU does not have.
    // SAFETY: an empty asm, no operands and no effects beyond the barrier.
    unsafe { core::arch::asm!("", options(nostack, preserves_flags)) };
    // SAFETY: the source is closed, so the handler (the only other user of
    // the engine) cannot run until this section reopens it, and no other
    // foreground reference exists because the CPU has one thread of control.
    let engine = unsafe { &mut *ENGINE.0.get() };
    engine.note_source_closed();
    let result = f(engine);
    engine.sync_source();
    publish(engine);
    // SAFETY: as above.
    unsafe { core::arch::asm!("", options(nostack, preserves_flags)) };
    result
}

// -------------------------------------------------------------- the wrapper

#[cfg(target_arch = "mips")]
core::arch::global_asm!(
    r#"
    .set noreorder
    .set noat
    .section .text.psx_cdstream_irq
    .globl psx_cdstream_exception_wrapper
psx_cdstream_exception_wrapper:
    # Not an interrupt: straight through.
    mfc0  $26, $13
    nop
    andi  $26, $26, 0x007c
    bnez  $26, 9f
    nop
    # An interrupt, but is the CD source one of the pending, enabled ones?
    lui   $26, {io_hi}
    lw    $27, {i_stat}($26)
    lw    $26, {i_mask}($26)
    nop
    and   $27, $27, $26
    andi  $27, $27, {cd_bit}
    beqz  $27, 9f
    nop
    # Save the interrupted context (everything but $k0/$k1) in our own block.
    lui   $26, %hi({context})
    addiu $26, $26, %lo({context})
    sw    $1, 0($26)
    sw    $2, 4($26)
    sw    $3, 8($26)
    sw    $4, 12($26)
    sw    $5, 16($26)
    sw    $6, 20($26)
    sw    $7, 24($26)
    sw    $8, 28($26)
    sw    $9, 32($26)
    sw    $10, 36($26)
    sw    $11, 40($26)
    sw    $12, 44($26)
    sw    $13, 48($26)
    sw    $14, 52($26)
    sw    $15, 56($26)
    sw    $16, 60($26)
    sw    $17, 64($26)
    sw    $18, 68($26)
    sw    $19, 72($26)
    sw    $20, 76($26)
    sw    $21, 80($26)
    sw    $22, 84($26)
    sw    $23, 88($26)
    sw    $24, 92($26)
    sw    $25, 96($26)
    sw    $28, 100($26)
    sw    $29, 104($26)
    sw    $30, 108($26)
    sw    $31, 112($26)
    mfhi  $27
    sw    $27, 116($26)
    mflo  $27
    sw    $27, 120($26)
    # Our own stack; the interrupted one is never written.
    lui   $29, %hi({stack}+{stack_bytes})
    addiu $29, $29, %lo({stack}+{stack_bytes})
    addiu $29, $29, -16
    jal   {handler}
    nop
    lui   $26, %hi({context})
    addiu $26, $26, %lo({context})
    lw    $27, 116($26)
    nop
    mthi  $27
    lw    $27, 120($26)
    nop
    mtlo  $27
    lw    $1, 0($26)
    lw    $2, 4($26)
    lw    $3, 8($26)
    lw    $4, 12($26)
    lw    $5, 16($26)
    lw    $6, 20($26)
    lw    $7, 24($26)
    lw    $8, 28($26)
    lw    $9, 32($26)
    lw    $10, 36($26)
    lw    $11, 40($26)
    lw    $12, 44($26)
    lw    $13, 48($26)
    lw    $14, 52($26)
    lw    $15, 56($26)
    lw    $16, 60($26)
    lw    $17, 64($26)
    lw    $18, 68($26)
    lw    $19, 72($26)
    lw    $20, 76($26)
    lw    $21, 80($26)
    lw    $22, 84($26)
    lw    $23, 88($26)
    lw    $24, 92($26)
    lw    $25, 96($26)
    lw    $28, 100($26)
    lw    $29, 104($26)
    lw    $30, 108($26)
    lw    $31, 112($26)
    nop
9:
    # psx-rt's handler: VBlank, display flip, faults, strays, and the return.
    j     __psx_rt_exception_handler
    nop
    .set at
    .set reorder
    "#,
    io_hi = const psx_hw::memory::io::BASE >> 16,
    i_stat = const psx_hw::irq::I_STAT & 0xFFFF,
    i_mask = const psx_hw::irq::I_MASK & 0xFFFF,
    cd_bit = const CD_BIT,
    stack = sym PSX_CD_IRQ_STACK,
    stack_bytes = const HANDLER_STACK_BYTES,
    context = sym PSX_CD_IRQ_CONTEXT,
    handler = sym psx_cdstream_interrupt,
);

// The wrapper reaches both registers with one `lui` of the I/O window and a
// signed 16-bit offset.
#[cfg(target_arch = "mips")]
const _: () = {
    let window = psx_hw::memory::io::BASE >> 16;
    assert!(psx_hw::irq::I_STAT >> 16 == window && psx_hw::irq::I_MASK >> 16 == window);
    assert!(psx_hw::irq::I_STAT & 0xFFFF < 0x8000 && psx_hw::irq::I_MASK & 0xFFFF < 0x8000);
};

#[cfg(target_arch = "mips")]
extern "C" {
    fn psx_cdstream_exception_wrapper();
}

/// Called by the wrapper, on the private stack, with CPU interrupts off.
#[no_mangle]
extern "C" fn psx_cdstream_interrupt() {
    // SAFETY: exception entry masks CPU interrupts, and every foreground
    // access closes the CD source before it takes its reference, so this is
    // the only live reference to the engine.
    let engine = unsafe { &mut *ENGINE.0.get() };
    engine.on_interrupt();
    publish(engine);
}

// ------------------------------------------------------------------- set-up

/// Take over the CD-ROM controller: put the exception wrapper in front of
/// psx-rt's handler and start serving requests.
///
/// The controller must be idle and nobody else may use it from now on: if the
/// boot code read with `SectorReader`, call `SectorReader::release` for the
/// token and pass it here. The token comes back from [`uninstall`] (to read
/// with the polled reader again) or [`take_audio_lease`] (to play audio).
///
/// If psx-rt's handler is not in the vector yet this installs it first
/// (`install_vblank_counter`); call `install` after any code that installs it
/// itself, since that call would overwrite the wrapper.
///
/// Returns the token if the transport is already installed.
///
/// With [`Config::time_handler`] the transport takes Timer 2 for itself.
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
    paint_stack();
    install_vector();
    with_engine(|engine| {
        engine.hw_mut().time_handler = config.time_handler;
        engine.configure(config);
        engine.attach();
    });
    Ok(())
}

fn paint_stack() {
    // SAFETY: the handler cannot run (the source is closed) and nothing else
    // uses this stack; plain volatile stores over a static this module owns.
    unsafe {
        let stack = (&raw mut PSX_CD_IRQ_STACK).cast::<u8>();
        for i in 0..HANDLER_STACK_BYTES {
            stack.add(i).write_volatile(STACK_FILL);
        }
    }
}

#[cfg(target_arch = "mips")]
fn install_vector() {
    const EXCEPTION_VECTOR: *mut u32 = 0x8000_0080 as *mut u32;
    const J_OPCODE: u32 = 0x0800_0000;
    let wrapper = psx_cdstream_exception_wrapper as *const () as usize as u32;
    // SAFETY: the general exception vector is two words of kernel RAM; the
    // wrapper switches stacks before it stores anything and hands over to
    // psx-rt's handler, which is how psx-rt's own installer fills it. The
    // instruction cache is flushed so the CPU fetches the new words, and the
    // wrapper is declared stack-safe so scratchpad stacks stay allowed.
    unsafe {
        core::ptr::write_volatile(EXCEPTION_VECTOR, J_OPCODE | ((wrapper >> 2) & 0x03ff_ffff));
        core::ptr::write_volatile(EXCEPTION_VECTOR.add(1), 0);
        psx_rt::cache::flush_instruction_cache();
        psx_rt::interrupts::declare_stack_safe_handler(psx_cdstream_exception_wrapper);
    }
}

#[cfg(not(target_arch = "mips"))]
fn install_vector() {}

/// Give the controller back to polled code. Succeeds only when the transport
/// is idle with nothing queued; call [`cancel_all`] and poll [`is_idle`]
/// first. The wrapper stays in the exception vector (a few instructions per
/// exception) but the CD source is closed.
pub fn uninstall() -> Option<Cd> {
    let detached = with_engine(Engine::detach);
    if !detached {
        return None;
    }
    TOKEN.take()
}

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

/// Withdraw a lease that was asked for and has not been granted. Returns
/// whether one was pending.
pub fn withdraw_audio_lease() -> bool {
    with_engine(|engine| {
        engine.lease_state() == LeaseState::Pending && engine.release_audio_lease()
    })
}

// --------------------------------------------------------------- diagnostics

/// Bytes of the handler's private stack never touched since [`install`]
/// painted it. The handler's own frames plus the saved-context area must fit
/// in the rest; a value near 0 means it overflowed.
pub fn handler_stack_unused_bytes() -> usize {
    let mut unused = 0;
    // SAFETY: plain volatile reads of a static this module owns.
    unsafe {
        let stack = (&raw const PSX_CD_IRQ_STACK).cast::<u8>();
        while unused < HANDLER_STACK_BYTES && stack.add(unused).read_volatile() == STACK_FILL {
            unused += 1;
        }
    }
    unused
}
