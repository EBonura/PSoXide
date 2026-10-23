//! Interrupt-serviced drain of a polled read.
//!
//! The polled [`SectorReader`](psx_io::cd::reader::SectorReader) starts a read
//! (Setloc, ReadN) and then has to look at the controller every few
//! milliseconds: the drive delivers a sector every 6.7 ms at double speed, keeps
//! only a few, and hands over the newest when software is late. A frame loop that
//! looks once per frame, and frames take 50 ms to 200 ms on a busy scene, loses
//! most of a read that way.
//!
//! This module is the cheap fix for a program that already loads with the polled
//! reader and wants one read to proceed while the frame loop runs: it takes over
//! only the *popping*. The caller starts the read the way it always did
//! (`prepare`, `start_read`), calls [`start`], and the CD interrupt handler pops
//! each sector straight into the destination as it arrives. The caller looks at
//! [`poll`] once a frame, and when it says [`DrainState::Done`] ends the read the
//! way it always did (`stop`), with the polled reader's Pause protocol, which has
//! been proved on a console. Nothing else about the drive changes.
//!
//! It is small on purpose, because the programs it is for are tight on RAM: no
//! command state machine, no queue, no token. The handler, its exception wrapper
//! and its private stack come to about 1.5 KB. The full transport in the crate
//! root ([`install`](crate::install), [`submit`](crate::submit)) owns the whole
//! drive, commands included, and is the one to use when every read can go
//! through it. Do not use both in one program at the same time: each puts a
//! wrapper in the exception vector and both would answer a CD interrupt.
//!
//! # What the program has to hold to
//!
//! - From [`start`] until [`poll`] reports something other than
//!   [`DrainState::Running`], or [`cancel`], nothing else touches the CD
//!   controller: no polled command, no polled read.
//! - The destination is not read or written by anything else until then.
//! - After `Done` or `Failed` the program ends the read with the polled
//!   reader's `stop` and calls [`cancel`] (or [`start`] again).
//! - `install_vblank_counter` takes psx-rt's handler back and drops the
//!   wrapper; [`start`] puts it in again when it notices, so a program that
//!   calls it for every scene needs nothing else.

use core::cell::UnsafeCell;
use psx_hw::cd::{index_status, irq as flag};
use psx_hw::irq::source;
use psx_io::irq;
use psx_io::periph::Cd;

use crate::request::{Request, SECTOR_WORDS};

/// CD interrupt source bit in `I_STAT` and `I_MASK`.
const CD_BIT: u32 = 1 << source::CDROM;
/// Status reads to wait for the data FIFO to fill after a sector interrupt.
const FIFO_POLLS: u32 = 256;
/// Bytes of private stack the handler runs on. The handler is a flat loop with
/// a small frame: the deepest call measured used 112 bytes (Half-Life, five
/// weapon switches in the emulator, stack painted by [`start`]).
pub const DRAIN_STACK_BYTES: usize = 384;
/// Fill byte of the handler stack, to measure how much of it was used.
const STACK_FILL: u8 = 0xa5;

/// Where a drain stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrainState {
    /// Nothing was started, or the last drain was cancelled.
    Idle,
    /// Sectors are arriving; `received` have been stored.
    Running {
        /// Sectors stored so far.
        received: u32,
    },
    /// Every sector landed. End the read with the polled reader's `stop`.
    Done,
    /// The drain gave up; end the read with the polled reader's `stop`.
    Failed(DrainFailure),
}

/// Why a drain gave up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrainFailure {
    /// The drive answered with an error (INT5).
    Drive {
        /// Status byte of the error response.
        status: u8,
        /// Error code byte.
        code: u8,
    },
    /// The data FIFO never filled after a sector interrupt.
    DataNotReady,
    /// No sector arrived for the timeout given to [`start`].
    Watchdog,
}

const IDLE: u32 = 0;
const RUNNING: u32 = 1;
const DONE: u32 = 2;
const FAILED: u32 = 3;
/// `Drive` failure codes are `0x0500_0000 | status << 8 | code`.
const FAIL_DRIVE: u32 = 0x0500_0000;
const FAIL_DATA_NOT_READY: u32 = 0xfa00_0000;
const FAIL_WATCHDOG: u32 = 0xff00_0000;

struct Drain {
    destination: *mut u32,
    request: Request,
    received: u32,
    state: u32,
    error: u32,
    /// VBlank count at the last progress (start or a stored sector).
    stamp: u32,
    timeout: u32,
}

struct Global(UnsafeCell<Drain>);

// SAFETY: reached from one CPU only, from the foreground with the CD source
// closed or from the interrupt handler, never both at once.
unsafe impl Sync for Global {}

// SAFETY: a placeholder that is never read while `state` is `IDLE`; `start` replaces it.
const NO_REQUEST: Request = unsafe { Request::new_raw(0, 0, core::ptr::null_mut()) };

static DRAIN: Global = Global(UnsafeCell::new(Drain {
    destination: core::ptr::null_mut(),
    request: NO_REQUEST,
    received: 0,
    state: IDLE,
    error: 0,
    stamp: 0,
    timeout: 0,
}));

#[repr(C, align(16))]
struct Stack([u8; DRAIN_STACK_BYTES]);

/// The handler's private stack, painted when [`start`] first puts the wrapper in
/// the vector so a tool can see how much of it a run used ([`stack_unused_bytes`]).
#[no_mangle]
static mut PSX_CDDRAIN_STACK: Stack = Stack([0; DRAIN_STACK_BYTES]);
/// Where the wrapper saves the interrupted context.
#[no_mangle]
static mut PSX_CDDRAIN_CONTEXT: [u32; 34] = [0; 34];
/// The handler the wrapper hands every exception on to: whatever the vector led
/// to when [`start`] first put the wrapper in (psx-rt's, or another wrapper
/// that leads there). Zero until then.
#[no_mangle]
static mut PSX_CDDRAIN_NEXT_HANDLER: u32 = 0;
/// `psx_rt::interrupts::vector_epoch` when the wrapper was put in the vector.
static mut PSX_CDDRAIN_EPOCH: u32 = 0;

// -------------------------------------------------------------- the wrapper

core::arch::global_asm!(
    r#"
    .set noreorder
    .set noat
    .section .text.psx_cddrain_irq
    .globl psx_cddrain_exception_wrapper
psx_cddrain_exception_wrapper:
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
    # The handler that was in the vector before this one: psx-rt's (VBlank,
    # display flip, faults, strays, and the return), or another wrapper that
    # leads there.
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
    cd_bit = const CD_BIT,
    stack = sym PSX_CDDRAIN_STACK,
    stack_bytes = const DRAIN_STACK_BYTES,
    context = sym PSX_CDDRAIN_CONTEXT,
    next = sym PSX_CDDRAIN_NEXT_HANDLER,
    handler = sym psx_cddrain_interrupt,
);

// The wrapper reaches both registers with one `lui` of the I/O window and a
// signed 16-bit offset.
const _: () = {
    let window = psx_hw::memory::io::BASE >> 16;
    assert!(psx_hw::irq::I_STAT >> 16 == window && psx_hw::irq::I_MASK >> 16 == window);
    assert!(psx_hw::irq::I_STAT & 0xffff < 0x8000 && psx_hw::irq::I_MASK & 0xffff < 0x8000);
};

extern "C" {
    fn psx_cddrain_exception_wrapper();
    #[link_name = "__psx_rt_exception_handler"]
    fn psx_rt_exception_handler();
}

// ------------------------------------------------------------------ handler

/// Called by the wrapper, on the private stack, with CPU interrupts off.
#[no_mangle]
extern "C" fn psx_cddrain_interrupt() {
    // SAFETY: exception entry masks CPU interrupts, and every foreground access
    // closes the CD source before it takes its reference, so this is the only
    // live reference to the drain.
    let drain = unsafe { &mut *DRAIN.0.get() };
    let mut cd = token();
    let code = cd.irq_flag_value();
    if code == flag::DATA_READY {
        take_sector(drain, &mut cd);
    } else if code == flag::ERROR {
        let status = cd.read_response_byte();
        let detail = cd.read_response_byte();
        cd.discard_response();
        cd.acknowledge_irq(flag::ACK_ALL);
        fail(
            drain,
            FAIL_DRIVE | u32::from(status) << 8 | u32::from(detail),
        );
    } else if code != 0 {
        // The acknowledge or completion of an earlier command arriving late:
        // nothing to do with the sectors, so answer it and move on.
        cd.discard_response();
        cd.acknowledge_irq(code);
    }
}

/// Pop the sector the interrupt announced and store it. Past the last one the
/// drive is still reading until the caller's Pause lands, so the extra sectors
/// are popped and dropped to keep the controller moving.
fn take_sector(drain: &mut Drain, cd: &mut Cd) {
    let wanted = drain.state == RUNNING && drain.received < drain.request.sectors();
    let (destination, words) = if wanted {
        let at = drain
            .destination
            .wrapping_add(drain.received as usize * SECTOR_WORDS as usize);
        (at, drain.request.store_words(drain.received))
    } else {
        (core::ptr::null_mut(), 0)
    };
    // SAFETY: `destination` is the sector's slot in the buffer `start` was given
    // (valid for `words` words by its contract), and unused when `words` is 0.
    if !unsafe { pop_sector(cd, destination, words) } {
        cd.discard_response();
        cd.acknowledge_irq(flag::ACK_ALL);
        fail(drain, FAIL_DATA_NOT_READY);
        return;
    }
    cd.discard_response();
    cd.acknowledge_irq(flag::DATA_READY);
    if wanted {
        drain.received += 1;
        drain.stamp = psx_rt::interrupts::vblank_count();
        if drain.received == drain.request.sectors() {
            drain.state = DONE;
            close_source();
        }
    }
}

fn fail(drain: &mut Drain, error: u32) {
    if drain.state == RUNNING {
        drain.state = FAILED;
        drain.error = error;
    }
    close_source();
}

/// Pop one sector's 2048 bytes from the data FIFO as 512 little-endian words,
/// storing the first `store_words` of them at `destination` and dropping the
/// rest. `false` when the FIFO never fills.
///
/// # Safety
///
/// `destination` must be valid for `store_words` word writes.
unsafe fn pop_sector(cd: &mut Cd, destination: *mut u32, store_words: usize) -> bool {
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
    for word in 0..SECTOR_WORDS as usize {
        let b0 = u32::from(cd.read_data_byte());
        let b1 = u32::from(cd.read_data_byte());
        let b2 = u32::from(cd.read_data_byte());
        let b3 = u32::from(cd.read_data_byte());
        if word < store_words {
            // SAFETY: `word < store_words`, inside the caller's contract.
            unsafe {
                destination
                    .add(word)
                    .write_volatile(b0 | b1 << 8 | b2 << 16 | b3 << 24);
            }
        }
    }
    true
}

/// The controller token. The drain only runs while the caller has handed the
/// controller over, which is what [`start`] documents.
fn token() -> Cd {
    // SAFETY: a token is a logic guard (see `psx_io::periph`); the caller of
    // `start` keeps every other user away while a drain runs.
    unsafe { Cd::steal() }
}

// ------------------------------------------------------------- the source

/// Let the CD interrupt source reach the CPU. psx-rt's handler would
/// acknowledge a CD interrupt that reached it pending, so the source is claimed
/// while it is open and this wrapper services it.
fn open_source() {
    psx_rt::interrupts::claim_interrupt_sources(CD_BIT);
    irq::set_mask(irq::mask() | CD_BIT);
}

/// Stop the CD source reaching the CPU and drop the claim, so code that opens it
/// itself (the polled reader) has its strays acknowledged by psx-rt as before.
fn close_source() {
    irq::set_mask(irq::mask() & !CD_BIT);
    psx_rt::interrupts::release_interrupt_sources(CD_BIT);
}

/// Run `f` on the drain with the CD source closed, then reopen the source if the
/// drain is still running.
fn with_drain<R>(f: impl FnOnce(&mut Drain) -> R) -> R {
    close_source();
    // SAFETY: the source is closed, so the handler (the only other user) cannot
    // run until it is reopened below, and the CPU has one thread of control.
    let drain = unsafe { &mut *DRAIN.0.get() };
    let result = f(drain);
    if drain.state == RUNNING {
        open_source();
    }
    result
}

// ------------------------------------------------------------- the foreground

/// Hand the sectors of a read the caller has just started to the CD interrupt
/// handler.
///
/// Call it right after `SectorReader::start_read` (or any polled start of a
/// ReadN stream) returned `true`. `sectors` sectors are stored back to back at
/// `destination`; with `byte_len` between `(sectors - 1) * 2048 + 1` and
/// `sectors * 2048` only that many bytes are written (rounded up to a word), so
/// a transfer never writes past a chunk that does not end on a sector boundary.
/// With `timeout_vblanks` VBlanks passing without a sector the drain fails
/// ([`DrainFailure::Watchdog`]).
///
/// Returns `false`, starting nothing, if the arguments are empty or
/// misaligned.
///
/// # Safety
///
/// `destination` must be word-aligned and valid for writes of the bytes above,
/// and nothing else may read or write them until the drain has finished or been
/// cancelled. The caller must hold to the rules in the module documentation:
/// the controller is the drain's until then.
pub unsafe fn start(
    destination: *mut u32,
    sectors: u32,
    byte_len: u32,
    timeout_vblanks: u32,
) -> bool {
    if destination.is_null() || destination.align_offset(4) != 0 || sectors == 0 {
        return false;
    }
    install_vector();
    // SAFETY: the caller vouches for `destination` and `sectors`.
    let request = unsafe { Request::new_raw(0, sectors, destination) }.with_byte_len(byte_len);
    close_source();
    // The read was started by polled code: anything it left pending on the
    // CPU side must not look like a sector of ours.
    irq::acknowledge(CD_BIT);
    with_drain(|drain| {
        *drain = Drain {
            destination,
            request,
            received: 0,
            state: RUNNING,
            error: 0,
            stamp: psx_rt::interrupts::vblank_count(),
            timeout: timeout_vblanks,
        };
    });
    true
}

/// Where the drain is. Also runs the no-progress check: a drain that has
/// stored nothing for the `timeout_vblanks` given to [`start`] fails.
pub fn poll() -> DrainState {
    with_drain(|drain| {
        if drain.state == RUNNING
            && psx_rt::interrupts::vblank_count().wrapping_sub(drain.stamp) > drain.timeout
        {
            drain.state = FAILED;
            drain.error = FAIL_WATCHDOG;
        }
        match drain.state {
            RUNNING => DrainState::Running {
                received: drain.received,
            },
            DONE => DrainState::Done,
            FAILED => DrainState::Failed(decode(drain.error)),
            _ => DrainState::Idle,
        }
    })
}

/// End the drain: the CD source is closed and the state goes back to
/// [`DrainState::Idle`]. Call it after `Done` or `Failed`, or to give up on a
/// running one. It does not touch the drive: the caller ends the read with the
/// polled reader's `stop`.
pub fn cancel() {
    close_source();
    // SAFETY: the source is closed, as in `with_drain`.
    let drain = unsafe { &mut *DRAIN.0.get() };
    drain.state = IDLE;
}

fn decode(error: u32) -> DrainFailure {
    match error >> 24 {
        0x05 => DrainFailure::Drive {
            status: (error >> 8) as u8,
            code: error as u8,
        },
        0xfa => DrainFailure::DataNotReady,
        _ => DrainFailure::Watchdog,
    }
}

fn paint_stack() {
    // SAFETY: the handler cannot run (the source is closed, or the drain is not
    // running) and nothing else uses this stack; plain volatile stores over a
    // static this module owns.
    unsafe {
        let stack = (&raw mut PSX_CDDRAIN_STACK).cast::<u8>();
        for i in 0..DRAIN_STACK_BYTES {
            stack.add(i).write_volatile(STACK_FILL);
        }
    }
}

/// Bytes of the handler's stack never touched since [`start`] painted it (when it
/// put the wrapper in the vector).
pub fn stack_unused_bytes() -> usize {
    let mut unused = 0;
    // SAFETY: plain volatile reads of a static this module owns.
    unsafe {
        let stack = (&raw const PSX_CDDRAIN_STACK).cast::<u8>();
        while unused < DRAIN_STACK_BYTES && stack.add(unused).read_volatile() == STACK_FILL {
            unused += 1;
        }
    }
    unused
}

fn install_vector() {
    const EXCEPTION_VECTOR: *mut u32 = 0x8000_0080 as *mut u32;
    const J_OPCODE: u32 = 0x0800_0000;
    const J_MASK: u32 = 0xfc00_0000;
    let wrapper = psx_cddrain_exception_wrapper as *const () as usize as u32;
    // SAFETY: the general exception vector is two words of kernel RAM. The
    // handler it leads to now is kept in a static the wrapper reads, before the
    // vector is rewritten; it is psx-rt's unless something chained in front of
    // psx-rt (the pad engine's wrapper) is there, and anything that is not a `j`
    // is replaced by psx-rt's. A wrapper installed since psx-rt last reset the
    // vector is not put in front again: it is already in the chain, and writing
    // the vector would drop whatever was installed since. The wrapper switches
    // stacks before it stores anything and hands over with `$sp` as it found it.
    // The instruction cache is flushed so the CPU fetches the new words, and the
    // wrapper is declared stack-safe so scratchpad stacks stay allowed.
    unsafe {
        let epoch = psx_rt::interrupts::vector_epoch();
        if core::ptr::read_volatile(&raw const PSX_CDDRAIN_NEXT_HANDLER) != 0
            && core::ptr::read_volatile(&raw const PSX_CDDRAIN_EPOCH) == epoch
        {
            return;
        }
        if !psx_rt::interrupts::is_stack_safe_handler_installed() {
            psx_rt::interrupts::install_vblank_counter();
        }
        paint_stack();
        let epoch = psx_rt::interrupts::vector_epoch();
        core::ptr::write_volatile(&raw mut PSX_CDDRAIN_EPOCH, epoch);
        let current = core::ptr::read_volatile(EXCEPTION_VECTOR);
        let next = if current & J_MASK == J_OPCODE {
            ((current & !J_MASK) << 2) | 0x8000_0000
        } else {
            psx_rt_exception_handler as *const () as usize as u32
        };
        core::ptr::write_volatile(&raw mut PSX_CDDRAIN_NEXT_HANDLER, next);
        core::ptr::write_volatile(EXCEPTION_VECTOR, J_OPCODE | ((wrapper >> 2) & 0x03ff_ffff));
        core::ptr::write_volatile(EXCEPTION_VECTOR.add(1), 0);
        psx_rt::cache::flush_instruction_cache();
        psx_rt::interrupts::declare_stack_safe_handler(psx_cddrain_exception_wrapper);
    }
}
