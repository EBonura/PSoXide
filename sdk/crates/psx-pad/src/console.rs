// SPDX-License-Identifier: GPL-2.0-or-later
//! The console side of the pad engine: the serial port and a timer behind
//! [`Hw`], the one global [`Engine`], and the exception wrapper that feeds it.
//!
//! Everything here is for the target. (It compiles on the host so the
//! documentation shows it; none of it may run there.)
//!
//! # Interrupt path
//!
//! The wrapper sits in the general exception vector in front of whatever
//! handler was there when [`install`] ran (psx-rt's, or a wrapper such as
//! psx-cdstream's that leads to it). For an interrupt with VBlank, the
//! controller (IRQ7) or root counter 0 pending it saves the interrupted CPU
//! context, switches to a private stack, runs the engine's events, restores
//! the context and jumps on to the next handler either way, so the VBlank
//! counter, the display flip and the fault handling run exactly as before.
//! The wrapper never touches the interrupted stack, which is what makes it
//! safe with a scratchpad stack; [`install`] declares it so.
//!
//! # Foreground path
//!
//! Every foreground call runs with CPU interrupts off for its few
//! instructions, so it never overlaps a handler. Reading a snapshot takes no
//! lock at all ([`snapshot`]).
//!
//! # Memory cards
//!
//! The engine owns the port token. A card driver borrows it as a [`Lease`],
//! which keeps the engine off the port until it is dropped.

use crate::engine::{Config, Engine, Hw, Owner, PortReading, Published, Snapshot, Stats};
use crate::{AnalogRequirement, PadState, Rumble};
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use psx_hw::irq::source;
use psx_hw::sio::sio0::{self, ctrl, stat};
use psx_hw::timers::mode;
use psx_io::controller_port::{Port, Transport};
use psx_io::irq;
use psx_io::periph::ControllerPort;
use psx_io::timers::{self, Timer};

/// The VBlank, controller and root counter 0 sources in `I_STAT` and `I_MASK`.
const VBLANK_BIT: u32 = 1 << source::VBLANK;
const SIO_BIT: u32 = 1 << source::CONTROLLER;
const TIMER_BIT: u32 = 1 << source::TIMER0;
const SOURCES: u32 = VBLANK_BIT | SIO_BIT | TIMER_BIT;
/// Bytes of private stack the handler runs on.
pub const HANDLER_STACK_BYTES: usize = 768;
/// Fill byte of the handler stack, to measure how much of it was used.
const STACK_FILL: u8 = 0xa5;
/// Status reads the handler waits for an `/ACK` pulse to end.
const ACK_RELEASE_SPINS: u32 = 256;
/// VBlanks [`lease`] waits for the engine to go idle before taking the port.
const LEASE_PATIENCE_VBLANKS: u32 = 3;
/// Attempts [`lease`] makes before it stops counting on VBlanks, which never
/// arrive for a caller that has interrupts off. Each is a few dozen cycles, so
/// this is several milliseconds, longer than a round on the wire.
const LEASE_PATIENCE_ATTEMPTS: u32 = 4_000;

// -------------------------------------------------------------------- port

/// The serial port and root counter 0, through the SDK's register steps.
pub struct Mmio {
    /// A deadline is armed and has not been cancelled.
    armed: bool,
    /// Its length in CPU cycles.
    target: u16,
}

impl Mmio {
    const fn new() -> Self {
        Mmio {
            armed: false,
            target: 0,
        }
    }
}

/// The port token. The engine owns the port whenever it runs, which is what
/// [`install`] took the real token for.
fn token() -> ControllerPort {
    // SAFETY: a token is a logic guard (see `psx_io::periph`). `install` holds
    // the real one for as long as the engine may touch the port, and hands it
    // back only when the engine is idle or leased out.
    unsafe { ControllerPort::steal() }
}

impl Hw for Mmio {
    fn status(&mut self) -> u32 {
        token().status()
    }

    fn select(&mut self, port: Port, ack_irq: bool) {
        token().select_with(port, ack_irq);
    }

    fn deselect(&mut self) {
        token().deselect();
    }

    fn reset_uart(&mut self) {
        token().reset();
    }

    fn drain_receive(&mut self) {
        token().drain_receive();
    }

    fn transmit(&mut self, byte: u8) {
        token().transmit(byte);
    }

    fn receive(&mut self) -> u8 {
        token().receive()
    }

    fn wait_ack_release(&mut self) -> bool {
        token().wait_status_clear(stat::DSR_LEVEL, ACK_RELEASE_SPINS)
    }

    fn clear_ack_latch(&mut self, port: Option<Port>, ack_irq: bool) {
        let selected = match port {
            Some(port) => sio0::selected_ctrl(port.is_two(), ack_irq),
            None => 0,
        };
        token().set_control(selected | ctrl::ACK);
    }

    fn arm_deadline(&mut self, cycles: u32) {
        let target = cycles.clamp(1, u32::from(u16::MAX)) as u16;
        self.armed = true;
        if target != self.target {
            self.target = target;
            timers::set_target(Timer::Timer0, target);
        }
        // System clock, interrupt once at the target. Writing the mode
        // restarts the count from zero.
        timers::set_mode(Timer::Timer0, mode::IRQ_AT_TARGET);
    }

    fn cancel_deadline(&mut self) {
        self.armed = false;
        timers::set_mode(Timer::Timer0, 0);
    }

    fn deadline_expired(&mut self) -> bool {
        // An interrupt raised by a deadline that was since replaced or
        // cancelled finds the restarted count below the new target.
        self.armed && timers::counter(Timer::Timer0) >= self.target
    }
}

// ------------------------------------------------------------------- state

struct Global(UnsafeCell<Engine<'static, Mmio>>);

// SAFETY: the engine is reached from one CPU only, from the foreground with
// CPU interrupts off or from the exception wrapper, never both at once.
unsafe impl Sync for Global {}

/// Where the engine publishes; the foreground reads it without a lock.
static PUBLISHED: Published = Published::new();

static ENGINE: Global = Global(UnsafeCell::new(Engine::new(
    Mmio::new(),
    &PUBLISHED,
    Config::DEFAULT,
)));

/// The port token while the engine holds it.
struct TokenSlot(UnsafeCell<Option<ControllerPort>>);

// SAFETY: touched from the foreground only, and only inside `with_engine`
// sections or before the wrapper can run, never from the handler.
unsafe impl Sync for TokenSlot {}

static TOKEN: TokenSlot = TokenSlot(UnsafeCell::new(None));

#[repr(C, align(16))]
struct Stack([u8; HANDLER_STACK_BYTES]);

/// The handler's private stack, painted by [`install`] so a tool can see how
/// much of it a run used ([`handler_stack_unused_bytes`] does the same from
/// inside).
#[no_mangle]
static mut PSX_PAD_IRQ_STACK: Stack = Stack([0; HANDLER_STACK_BYTES]);
/// Where the wrapper saves the interrupted context.
#[no_mangle]
static mut PSX_PAD_IRQ_CONTEXT: [u32; 34] = [0; 34];
/// The handler the wrapper hands every exception on to: whatever the vector
/// led to when [`install`] ran.
#[no_mangle]
static mut PSX_PAD_NEXT_HANDLER: u32 = 0;
/// Set by [`install`], cleared by [`uninstall`].
static mut INSTALLED: bool = false;

/// Run `f` on the engine with CPU interrupts off.
#[inline(always)]
fn with_engine<R>(f: impl FnOnce(&mut Engine<'static, Mmio>) -> R) -> R {
    irq::without_interrupts(|| {
        // SAFETY: CPU interrupts are off, so the wrapper (the only other user
        // of the engine) cannot run, and the CPU has one thread of control, so
        // no other foreground reference exists.
        f(unsafe { &mut *ENGINE.0.get() })
    })
}

// -------------------------------------------------------------- the wrapper

#[cfg(target_arch = "mips")]
core::arch::global_asm!(
    r#"
    .set noreorder
    .set noat
    .section .text.psx_pad_irq
    .globl psx_pad_exception_wrapper
psx_pad_exception_wrapper:
    # Not an interrupt: straight through.
    mfc0  $26, $13
    nop
    andi  $26, $26, 0x007c
    bnez  $26, 9f
    nop
    # An interrupt, but is VBlank, the controller or counter 0 one of the
    # pending, enabled sources?
    lui   $26, {io_hi}
    lw    $27, {i_stat}($26)
    lw    $26, {i_mask}($26)
    nop
    and   $27, $27, $26
    andi  $27, $27, {sources}
    beqz  $27, 9f
    nop
    # Save the interrupted context in our own block: every register the
    # handler may change. $16-$23 and $30 are callee-saved, so the handler
    # returns them as it found them; $k0/$k1 are the exception handler's own.
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
    sw    $24, 92($26)
    sw    $25, 96($26)
    sw    $28, 100($26)
    sw    $29, 104($26)
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
    lw    $24, 92($26)
    lw    $25, 96($26)
    lw    $28, 100($26)
    lw    $29, 104($26)
    lw    $31, 112($26)
    nop
9:
    # The handler that was in the vector before this one: psx-rt's, or a
    # wrapper that leads to it.
    lui   $26, %hi({next})
    lw    $26, %lo({next})($26)
    nop
    jr    $26
    nop
    .set at
    .set reorder
    "#,
    io_hi = const psx_hw::memory::io::BASE >> 16,
    i_stat = const psx_hw::irq::I_STAT & 0xFFFF,
    i_mask = const psx_hw::irq::I_MASK & 0xFFFF,
    sources = const SOURCES,
    stack = sym PSX_PAD_IRQ_STACK,
    stack_bytes = const HANDLER_STACK_BYTES,
    context = sym PSX_PAD_IRQ_CONTEXT,
    next = sym PSX_PAD_NEXT_HANDLER,
    handler = sym psx_pad_interrupt,
);

// The wrapper reaches both registers with one `lui` of the I/O window and a
// signed 16-bit offset.
#[cfg(target_arch = "mips")]
const _: () = {
    let window = psx_hw::memory::io::BASE >> 16;
    assert!(psx_hw::irq::I_STAT >> 16 == window && psx_hw::irq::I_MASK >> 16 == window);
    assert!(psx_hw::irq::I_STAT & 0xFFFF < 0x8000 && psx_hw::irq::I_MASK & 0xFFFF < 0x8000);
    assert!(SOURCES <= 0xFFFF);
};

#[cfg(target_arch = "mips")]
extern "C" {
    fn psx_pad_exception_wrapper();
}

/// Called by the wrapper, on the private stack, with CPU interrupts off.
#[no_mangle]
extern "C" fn psx_pad_interrupt() {
    let pending = irq::pending() & irq::mask() & SOURCES;
    // The controller and the timer are acknowledged before the engine runs,
    // so an interrupt they raise meanwhile is not lost. VBlank is left for
    // psx-rt's handler, which acknowledges it after the engine has seen it.
    irq::acknowledge(pending & (SIO_BIT | TIMER_BIT));
    // SAFETY: exception entry masks CPU interrupts, and every foreground
    // access runs with them off, so this is the only live reference to the
    // engine.
    let engine = unsafe { &mut *ENGINE.0.get() };
    if pending & VBLANK_BIT != 0 {
        engine.on_vblank();
    }
    // An acknowledgement that landed with a deadline is handled first: the
    // byte it ends then re-arms the deadline, which retires the old one.
    if pending & SIO_BIT != 0 {
        engine.on_ack();
    }
    if pending & TIMER_BIT != 0 {
        engine.on_deadline();
    }
}

// ------------------------------------------------------------------- set-up

#[cfg(target_arch = "mips")]
const EXCEPTION_VECTOR: *mut u32 = 0x8000_0080 as *mut u32;
#[cfg(target_arch = "mips")]
const J_OPCODE: u32 = 0x0800_0000;
#[cfg(target_arch = "mips")]
const J_MASK: u32 = 0xfc00_0000;

#[cfg(target_arch = "mips")]
fn install_vector() -> bool {
    // SAFETY: the general exception vector is two words of kernel RAM. The
    // word read is the handler this one chains to; it is kept in a static the
    // wrapper reads, before the vector is rewritten, and only a `j` is
    // accepted since anything else cannot be chained to. The wrapper switches
    // stacks before it stores anything, and hands over to the next handler
    // with `$sp` as it found it. The instruction cache is flushed so the CPU
    // fetches the new words, and the wrapper is declared stack-safe so
    // scratchpad stacks stay allowed.
    unsafe {
        // Installed before (and uninstalled since): already in the chain.
        if core::ptr::read_volatile(&raw const PSX_PAD_NEXT_HANDLER) != 0 {
            return true;
        }
        let current = core::ptr::read_volatile(EXCEPTION_VECTOR);
        if current & J_MASK != J_OPCODE {
            return false;
        }
        let next = ((current & !J_MASK) << 2) | 0x8000_0000;
        let wrapper = psx_pad_exception_wrapper as *const () as usize as u32;
        core::ptr::write_volatile(&raw mut PSX_PAD_NEXT_HANDLER, next);
        core::ptr::write_volatile(EXCEPTION_VECTOR, J_OPCODE | ((wrapper >> 2) & !J_MASK));
        core::ptr::write_volatile(EXCEPTION_VECTOR.add(1), 0);
        psx_rt::cache::flush_instruction_cache();
        psx_rt::interrupts::declare_stack_safe_handler(psx_pad_exception_wrapper);
    }
    true
}

#[cfg(not(target_arch = "mips"))]
fn install_vector() -> bool {
    true
}

fn paint_stack() {
    // SAFETY: plain volatile writes to a static this module owns, before the
    // wrapper can run.
    unsafe {
        let stack = (&raw mut PSX_PAD_IRQ_STACK).cast::<u8>();
        for i in 0..HANDLER_STACK_BYTES {
            stack.add(i).write_volatile(STACK_FILL);
        }
    }
}

/// Take over the controller port: put the exception wrapper in front of the
/// handler the vector leads to and start polling on every VBlank.
///
/// From here the port belongs to the engine, so nothing may poll it
/// synchronously; a memory card borrows it through [`lease`]. It chains to
/// whatever the vector led to, and psx-cdstream's wrapper does the same, so
/// the two can be installed in either order. Ask for analog mode with the synchronous
/// `require_analog_on` before installing, or with [`request_analog`] after.
///
/// If psx-rt's handler is not in the vector yet this installs it first
/// (`install_vblank_counter`). The root counter 0 is the engine's from here.
///
/// Returns the token if the engine is already installed, or if the vector
/// holds something that cannot be chained to.
pub fn install(port: ControllerPort, config: Config) -> Result<(), ControllerPort> {
    // SAFETY: foreground, before the wrapper can run; the flag is read here
    // and set below under the same condition.
    if unsafe { (*TOKEN.0.get()).is_some() } {
        return Err(port);
    }
    if !psx_rt::interrupts::is_stack_safe_handler_installed() {
        psx_rt::interrupts::install_vblank_counter();
    }
    let masked = irq::mask() & !(SIO_BIT | TIMER_BIT);
    irq::set_mask(masked);
    paint_stack();
    if !install_vector() {
        return Err(port);
    }
    // SAFETY: foreground, with the SIO and counter sources masked and the
    // engine idle; see above.
    unsafe { *TOKEN.0.get() = Some(port) };
    let mut port_io = token();
    port_io.deselect();
    port_io.drain_receive();
    timers::set_mode(Timer::Timer0, 0);
    irq::acknowledge(SIO_BIT | TIMER_BIT);
    with_engine(|engine| {
        *engine = Engine::new(Mmio::new(), &PUBLISHED, config);
        engine.publish_readings();
    });
    irq::set_mask(irq::mask() | SIO_BIT | TIMER_BIT | VBLANK_BIT);
    // A polled CD read cuts `I_MASK` down to VBlank for its whole stream; the
    // engine runs on these two sources alone, so it asks to keep them.
    irq::keep_enabled_while_polling(SIO_BIT | TIMER_BIT);
    // psx-rt's handler would acknowledge these as strays if they reach it
    // pending, behind another wrapper's handler or this one's.
    psx_rt::interrupts::claim_interrupt_sources(SIO_BIT | TIMER_BIT);
    // SAFETY: foreground; written here and in `uninstall` only.
    unsafe { core::ptr::write_volatile(&raw mut INSTALLED, true) };
    Ok(())
}

/// Give the port back to synchronous code. Waits for the transaction in
/// flight, as [`lease`] does (and abandons it after the same patience), then
/// closes the controller and counter 0 sources. The wrapper stays in the
/// exception vector, chained, a few instructions per exception; [`install`]
/// finds it there and takes the port again.
///
/// `None` when the engine is not installed or a [`Lease`] holds the port: a
/// card transaction is not the engine's to cut short.
pub fn uninstall() -> Option<ControllerPort> {
    if !is_installed() {
        return None;
    }
    // Leave nothing spinning in a pad that is about to be polled by something
    // that does not know about it.
    if with_engine(|engine| engine.motors_requested()) {
        stop_motors();
    }
    let start = psx_rt::interrupts::vblank_count();
    let mut attempts = 0u32;
    loop {
        let claimed = with_engine(|engine| {
            if engine.owner() == Owner::Lease {
                None
            } else {
                Some(engine.try_lease())
            }
        })?;
        if claimed {
            break;
        }
        attempts += 1;
        let waited = psx_rt::interrupts::vblank_count().wrapping_sub(start);
        if waited >= LEASE_PATIENCE_VBLANKS || attempts >= LEASE_PATIENCE_ATTEMPTS {
            with_engine(Engine::force_lease);
            break;
        }
    }
    irq::set_mask(irq::mask() & !(SIO_BIT | TIMER_BIT));
    irq::release_polling_keep(SIO_BIT | TIMER_BIT);
    psx_rt::interrupts::release_interrupt_sources(SIO_BIT | TIMER_BIT);
    timers::set_mode(Timer::Timer0, 0);
    irq::acknowledge(SIO_BIT | TIMER_BIT);
    // SAFETY: foreground; written here and in `install` only.
    unsafe { core::ptr::write_volatile(&raw mut INSTALLED, false) };
    // SAFETY: foreground; the engine is leased to this call, so nothing else
    // takes the token.
    unsafe { (*TOKEN.0.get()).take() }
}

/// Whether [`install`] has run and [`uninstall`] has not. The wrapper is then
/// in the exception chain: first in the vector, or behind psx-cdstream's, which
/// chains to it.
pub fn is_installed() -> bool {
    // SAFETY: a volatile read of a flag the foreground writes.
    unsafe { core::ptr::read_volatile(&raw const INSTALLED) }
}

// ------------------------------------------------------------ the foreground

/// The latest publication of both ports. A copy: no lock, no interrupt
/// masking.
pub fn snapshot() -> Snapshot {
    PUBLISHED.read()
}

/// The latest clean state of `port`.
pub fn pad(port: Port) -> PadState {
    PUBLISHED.read().pad(port)
}

/// The latest reading of `port`: the pad's state and whether the socket is
/// present.
pub fn reading(port: Port) -> PortReading {
    *PUBLISHED.read().port(port)
}

/// Change the settings; they apply from the next transaction.
pub fn configure(config: Config) {
    with_engine(|engine| engine.configure(config));
}

/// A copy of the counters.
pub fn stats() -> Stats {
    with_engine(|engine| engine.stats())
}

/// Ask for analog mode on `port` and lock it there. See
/// [`Engine::request_analog`]; [`analog_outcome`] answers a few frames later.
pub fn request_analog(port: Port) {
    with_engine(|engine| engine.request_analog(port));
}

/// What the pad settled on after [`request_analog`]; `None` while pending.
pub fn analog_outcome(port: Port) -> Option<AnalogRequirement> {
    with_engine(|engine| engine.analog_outcome(port))
}

/// Put the motors of the pad on `port` under the poll. The engine maps them
/// (command 0x4D, in the same visit to configuration mode as an analog request
/// if one is pending) whenever an analog DualShock is there and again each time
/// one is plugged in, then sends [`set_rumble`]'s request with every poll.
/// [`rumble_mapped`] says whether the pad took it.
pub fn enable_rumble(port: Port) {
    with_engine(|engine| engine.enable_rumble(port));
}

/// Stop wanting the motors on `port`. They are told to stop on the next poll.
pub fn disable_rumble(port: Port) {
    with_engine(|engine| engine.disable_rumble(port));
}

/// What the motors on `port` are asked to do on every poll. Takes effect on a
/// pad that has been mapped; a digital pad ignores it.
pub fn set_rumble(port: Port, rumble: Rumble) {
    with_engine(|engine| engine.set_rumble(port, rumble));
}

/// Whether the pad on `port` took the motor mapping.
pub fn rumble_mapped(port: Port) -> bool {
    with_engine(|engine| engine.rumble_mapped(port))
}

/// Stop every motor now and return once a poll with the motors off has gone
/// out on each port, so a program can pause, open a menu, or hand the port over
/// knowing nothing is still spinning. Waits as [`lease`] does, three VBlanks at
/// most; a port a [`Lease`] holds is not polled meanwhile. An unplugged pad has
/// nothing to stop.
pub fn stop_motors() {
    let (start, idle) = with_engine(|engine| {
        engine.stop_motors();
        (engine.stats().kicks, engine.is_idle())
    });
    // A round already on the wire took its motor bytes when it started; the
    // next round is the first to send zeros.
    let target = start.wrapping_add(if idle { 1 } else { 2 });
    let first = psx_rt::interrupts::vblank_count();
    let mut attempts = 0u32;
    loop {
        let done = with_engine(|engine| {
            let kicks = engine.stats().kicks;
            kicks.wrapping_sub(target) < 0x8000_0000 && engine.is_idle()
        });
        attempts += 1;
        let waited = psx_rt::interrupts::vblank_count().wrapping_sub(first);
        if done || waited >= LEASE_PATIENCE_VBLANKS || attempts >= LEASE_PATIENCE_ATTEMPTS {
            return;
        }
    }
}

/// Who may use the port now.
pub fn owner() -> Owner {
    with_engine(|engine| engine.owner())
}

// --------------------------------------------------------------------- lease

/// The port, on loan from the engine: it derefs to the [`ControllerPort`] a
/// memory card or a diagnostic needs, and the engine polls nothing until it
/// is dropped. VBlanks that pass meanwhile are not polled, so the snapshot
/// stands still; keep a lease to one transaction.
pub struct Lease {
    port: Option<ControllerPort>,
}

impl Deref for Lease {
    type Target = ControllerPort;

    fn deref(&self) -> &ControllerPort {
        self.port
            .as_ref()
            .expect("a lease holds the port until it is dropped")
    }
}

impl DerefMut for Lease {
    fn deref_mut(&mut self) -> &mut ControllerPort {
        self.port
            .as_mut()
            .expect("a lease holds the port until it is dropped")
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(port) = self.port.take() {
            with_engine(|engine| {
                // SAFETY: foreground, inside the engine section; the token
                // went out with this lease and comes back with it.
                unsafe { *TOKEN.0.get() = Some(port) };
                engine.release_lease();
            });
        }
    }
}

fn take_lease(take: impl FnOnce(&mut Engine<'static, Mmio>) -> bool) -> Option<Lease> {
    with_engine(|engine| {
        if !take(engine) {
            return None;
        }
        // SAFETY: foreground, inside the engine section.
        let port = unsafe { (*TOKEN.0.get()).take() };
        Some(Lease { port })
    })
}

/// Borrow the port if no transaction is on the wire right now.
pub fn try_lease() -> Option<Lease> {
    take_lease(Engine::try_lease)
}

/// Borrow the port. Waits for a transaction in flight (under two
/// milliseconds on the wire); if the engine has not gone idle after a few
/// VBlanks it abandons the transaction instead of waiting longer. A caller
/// with CPU interrupts off cannot let the transaction finish, so it waits a
/// fixed number of attempts instead and then takes the port.
pub fn lease() -> Lease {
    let start = psx_rt::interrupts::vblank_count();
    let mut attempts = 0u32;
    loop {
        if let Some(lease) = try_lease() {
            return lease;
        }
        attempts += 1;
        let waited = psx_rt::interrupts::vblank_count().wrapping_sub(start);
        if waited >= LEASE_PATIENCE_VBLANKS || attempts >= LEASE_PATIENCE_ATTEMPTS {
            let lease = take_lease(|engine| {
                engine.force_lease();
                true
            });
            return lease.expect("the engine holds the port token once installed");
        }
    }
}

// --------------------------------------------------------------- diagnostics

/// Bytes of the handler's private stack never touched since [`install`]
/// painted it.
pub fn handler_stack_unused_bytes() -> usize {
    let mut unused = 0;
    // SAFETY: plain volatile reads of a static this module owns.
    unsafe {
        let stack = (&raw const PSX_PAD_IRQ_STACK).cast::<u8>();
        while unused < HANDLER_STACK_BYTES && stack.add(unused).read_volatile() == STACK_FILL {
            unused += 1;
        }
    }
    unused
}
