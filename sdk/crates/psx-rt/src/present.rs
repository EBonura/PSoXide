//! The one present path: every frame a program shows goes to the screen
//! through this module and psx-rt's VBlank handler.
//!
//! A frame is either a DMA chain ([`Presenter::submit`], in one part or
//! several) or drawing done through the GP0 port ([`Presenter::present_drawn`]).
//! Either way the display flips only at a blank edge, only once the frame's
//! closing GP0(1Fh) has run (GPUSTAT bit 24), and no sooner than the cap
//! allows ([`set_cap`], 30 fps unless a program says otherwise). The CPU never
//! spins on an edge: it waits only when it is a whole frame ahead, before a
//! direct GPU access while a frame is still in flight (the guard after a chain
//! frame, [`Presenter::begin_frame`] after a drawn one), and before reusing
//! memory a walk may still read ([`wait_arena_free`]). Only a program that hands
//! over chains or records tests a flag on its port writes; one that only draws
//! through the port writes it as plain stores.
//!
//! # How a chain frame goes out
//!
//! 1. [`Presenter::submit`] links a preamble (the draw area, offset and clear of
//!    the buffer being drawn) in front of the frame's first part, waits until
//!    the slot is empty, writes the part's display word and head into it, and
//!    kicks from the thread at once if the kick's condition holds: the frame
//!    before is on screen (the flip word is 0) and DMA channel 2 is idle. A
//!    frame built in the middle of a display period starts drawing then, not at
//!    the next edge.
//! 2. Otherwise the VBlank handler kicks it at the first edge where the
//!    condition holds (or the thread does, in any of the waits below). The
//!    kick acknowledges bit 24 (GP1(02h)), starts the walk, and moves the
//!    part's display word to the flip word.
//! 3. At each edge the handler applies the flip word once bit 24 is up (the
//!    last part's GP0(1Fh) has run) and the cap's hold is 0, then tries the
//!    kick, so the next frame starts in the same pass. A finished frame is
//!    shown at the first allowed edge; it never waits for its successor.
//!
//! The kick is one implementation in assembly (`PSX_RT_PRESENT_KICK` in
//! `interrupts.rs`), assembled into the handler and into the thread's routine
//! `__psx_rt_present_kick`, which masks CPU interrupts around it. The flip is
//! the handler's alone: a GP1(05h) written mid-scanout shears the picture.
//!
//! # Interrupt safety
//!
//! The shared words are `__psx_rt_present_head` (the slot), `__psx_rt_present_display`
//! (its display word), `__psx_rt_pending_gp1` (the flip word) and the cap's
//! `__psx_rt_flip_min`; the hold, age and histogram are the handler's. Who
//! writes what:
//!
//! - The slot goes from 0 to a head only on the thread, and back to 0 only in a
//!   kick (handler, or thread with interrupts masked). The display word is
//!   written by the thread only while the slot is 0, before the head, and read
//!   by a kick only after it read a head.
//! - The flip word goes from 0 to a display word in a kick (only with the slot
//!   full), or on the thread ([`Presenter::present_drawn`], [`show`],
//!   [`set_display_enabled`]) only after the thread saw the slot empty and the
//!   flip word 0. It goes back to 0 in the handler's flip, or in
//!   [`cancel_flip`] with interrupts masked.
//!
//! So, with a VBlank between any two thread instructions:
//!
//! - **No frame is lost.** A thread store never overwrites a nonzero word the
//!   handler wrote: the slot is filled only by the thread, and the flip word is
//!   written by the thread only when the slot is empty and the word is 0, a
//!   state in which no kick can run (it needs a head) and the flip leaves it 0.
//!   A take-and-clear of the flip word is masked, so a kick cannot set it
//!   between the read and the clear.
//! - **Nothing is kicked twice.** A kick reads a head, starts it and clears the
//!   slot without being interrupted (the handler cannot be, and the thread's
//!   runs masked). A handler between the thread's head store and its own kick
//!   leaves the slot empty or full; the thread's kick sees which.
//! - **No half-drawn buffer is shown.** The flip word only gets a frame's
//!   display word when the frame's last part is kicked, and that kick
//!   acknowledged bit 24 just before starting the walk; nothing acknowledges
//!   again until the flip word is 0. So the flip sees bit 24 only from that
//!   part's own GP0(1Fh), after everything before it in the GPU's queue is
//!   drawn.
//! - **The buffer on screen is never drawn into.** A part is kicked only with
//!   the flip word 0, so the frame before it is already on screen and the part
//!   draws into the other buffer. The thread's own drawing waits for the same
//!   condition: `begin_frame` (after a drawn frame) or the guard (after a chain
//!   frame) runs [`wait_idle`] before the first direct access.
//! - **The channel is never left with nobody to start it.** A waiting part is
//!   kicked by the next edge whose condition holds, and every wait here kicks
//!   from the thread on each pass, so masked interrupts do not strand it.
//!
//! Other interrupts. Besides these words the kick reads channel 2's CHCR and
//! read-modify-writes DPCR, and the thread's kick saves and restores SR. A CD
//! interrupt (psx-cdstream's handler, run by the dispatcher) and the end of a
//! channel 3 transfer touch none of them: channel 3 has its own CHCR, nothing
//! but `psx_io::dma::enable_channel` (interrupts masked) and the boot code
//! writes DPCR, DICR is written only at boot, and SR is written only by masked
//! sections that put it back as they found it. Every handler runs with
//! interrupts off, so none runs inside a kick. `kick_shares_nothing_with_cd` (tests)
//! checks the first three against the sources; `hello-present-cd` runs the
//! present path under polled and interrupt-driven reads and across cache flushes.
//!
//! `model` (tests) runs this module's thread-side code against a transport that
//! injects a VBlank, a walk's end or a draw's end before any of its shared-word
//! accesses, over every boundary, and checks these five statements.
//!
//! Keep interrupt source 1 (GPU) masked in `I_MASK`: GP0(1Fh) raises it and
//! the handler, which owns VBlank only, would acknowledge it as a stray.

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

use psx_gpu::display::DoubleBuffer;
use psx_hw::gpu::{gp0, gp1, pack_xy};
pub use psx_tick::PresentCap;

use crate::interrupts::{
    __psx_rt_pending_gp1, __psx_rt_present_display, __psx_rt_present_head,
    __psx_rt_present_kick_count, __psx_rt_present_skip_count, __psx_rt_present_thread_kick_count,
};
use crate::wait::SPIN_LIMIT;

/// Edges a wait may last before the thread treats the chain ahead of it as
/// stalled (its walk wedged, or it lost its GP0(1Fh)) and stops it by hand.
pub const STALL_VBLANKS: u32 = 8;

/// Words of one preamble: the node tag, the draw area and offset (three
/// words) and the clear (three).
const PREAMBLE_WORDS: usize = 7;

/// The preamble of each buffer's frames. Frame N's is rewritten only when
/// frame N + 2 is submitted, after its slot wait, by which time frame N's walk
/// has ended (see [`wait_arena_free`]).
static mut PREAMBLES: [[u32; PREAMBLE_WORDS]; 2] = [[0; PREAMBLE_WORDS]; 2];

/// Thread-only counters (no handler touches them).
static mut RECOVERY_COUNT: u32 = 0;
static mut SLOT_WAIT_VBLANKS: u32 = 0;
static mut ARENA_WAIT_VBLANKS: u32 = 0;

/// Counters for telemetry, gates and the console's cadence probe.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PresentStats {
    /// Parts kicked, by the handler and by the thread.
    pub kick_count: u32,
    /// Of those, the parts the thread kicked (at submit or in a wait).
    pub thread_kick_count: u32,
    /// Edges on which a published part waited for the flip before it or for
    /// channel 2.
    pub skip_count: u32,
    /// Stalled walks the thread stopped by hand (see [`STALL_VBLANKS`]).
    pub recovery_count: u32,
    /// VBlanks the thread spent waiting for the slot to empty.
    pub slot_wait_vblanks: u32,
    /// VBlanks the thread spent in [`wait_arena_free`].
    pub arena_wait_vblanks: u32,
}

/// Whether a chain ends its frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Part {
    /// The frame's last (or only) chain. It must end on GP0(1Fh)
    /// (`psx_gpu::chain::DRAW_DONE_NODE`, `OrderingTable::end_with_draw_done`);
    /// the frame is shown once that has run.
    Final,
    /// More chains of this frame follow. It must not hold GP0(1Fh).
    More,
}

/// The shared-word and hardware accesses the thread side makes, one call per
/// access, so the host model can run anything between any two of them.
pub(crate) trait Port {
    /// The slot's head (0 when empty).
    fn head(&mut self) -> u32;
    /// Store the slot's display word.
    fn set_display(&mut self, word: u32);
    /// Store the slot's head.
    fn set_head(&mut self, head: u32);
    /// The flip word (0 when no flip is pending).
    fn flip(&mut self) -> u32;
    /// Store the flip word.
    fn set_flip(&mut self, word: u32);
    /// Read and clear the flip word with interrupts masked.
    fn take_flip(&mut self) -> u32;
    /// The thread's kick, with interrupts masked.
    fn kick(&mut self);
    /// DMA channel 2 busy.
    fn is_channel_busy(&mut self) -> bool;
    /// GPUSTAT bit 24.
    fn is_draw_done(&mut self) -> bool;
    /// The VBlank count.
    fn vblanks(&mut self) -> u32;
    /// GP1(02h).
    fn acknowledge_draw_done(&mut self);
    /// The draw-done sequence through the port's DMA path; returns once its
    /// walk has ended.
    fn signal_draw_done(&mut self);
    /// Stop a walk that is not ending and raise bit 24 by hand.
    fn stop_walk_and_raise(&mut self);
}

/// The hardware: psx-rt's statics and the GPU and DMA registers.
struct Hardware;

#[cfg(target_arch = "mips")]
extern "C" {
    /// The thread's kick (`interrupts.rs`): masks interrupts, runs the kick,
    /// restores them. Clobbers `$8`-`$10` only.
    fn __psx_rt_present_kick();
}

impl Port for Hardware {
    #[inline(always)]
    fn head(&mut self) -> u32 {
        // SAFETY: a volatile aligned word load; see the module's "Interrupt safety".
        unsafe { read_volatile(addr_of!(__psx_rt_present_head)) }
    }

    #[inline(always)]
    fn set_display(&mut self, word: u32) {
        // SAFETY: a volatile aligned word store, made only while the slot is empty.
        unsafe { write_volatile(addr_of_mut!(__psx_rt_present_display), word) }
    }

    #[inline(always)]
    fn set_head(&mut self, head: u32) {
        // SAFETY: a volatile aligned word store, made only while the slot is empty.
        unsafe { write_volatile(addr_of_mut!(__psx_rt_present_head), head) }
    }

    #[inline(always)]
    fn flip(&mut self) -> u32 {
        // SAFETY: a volatile aligned word load.
        unsafe { read_volatile(addr_of!(__psx_rt_pending_gp1)) }
    }

    #[inline(always)]
    fn set_flip(&mut self, word: u32) {
        #[cfg(target_arch = "mips")]
        // SAFETY: a volatile aligned word store, made only with the slot empty and the word 0.
        unsafe {
            write_volatile(addr_of_mut!(__psx_rt_pending_gp1), word)
        }
        // The host has no VBlank: a flip lands at once.
        #[cfg(not(target_arch = "mips"))]
        let _ = word;
    }

    fn take_flip(&mut self) -> u32 {
        psx_io::irq::without_interrupts(|| {
            // SAFETY: with interrupts masked no kick or flip runs between the load and the store.
            unsafe {
                let word = read_volatile(addr_of!(__psx_rt_pending_gp1));
                write_volatile(addr_of_mut!(__psx_rt_pending_gp1), 0);
                word
            }
        })
    }

    #[inline(always)]
    fn kick(&mut self) {
        #[cfg(target_arch = "mips")]
        // SAFETY: the routine masks interrupts itself and touches only the present queue's
        // statics, the GPU's GP1, DPCR and channel 2's registers, and $8-$10, which the C ABI
        // lets a call clobber. What it kicks is a part a caller of `submit` vouched for.
        unsafe {
            __psx_rt_present_kick()
        }
        // The host has no GPU: a part is kicked, drawn and shown at once.
        #[cfg(not(target_arch = "mips"))]
        if self.head() != 0 {
            self.set_head(0);
        }
    }

    #[inline(always)]
    fn is_channel_busy(&mut self) -> bool {
        #[cfg(target_arch = "mips")]
        return psx_io::dma::is_busy(psx_io::dma::Channel::Gpu);
        #[cfg(not(target_arch = "mips"))]
        false
    }

    #[inline(always)]
    fn is_draw_done(&mut self) -> bool {
        psx_io::gpu::status().contains(psx_hw::gpu::GpuStat::IRQ1)
    }

    #[inline(always)]
    fn vblanks(&mut self) -> u32 {
        crate::interrupts::vblank_count()
    }

    #[inline(always)]
    fn acknowledge_draw_done(&mut self) {
        psx_io::gpu::write_display_control_unguarded(gp1::ACK_IRQ);
    }

    fn signal_draw_done(&mut self) {
        #[cfg(target_arch = "mips")]
        psx_io::gpu::signal_draw_done_unguarded();
    }

    fn stop_walk_and_raise(&mut self) {
        stop_walk_and_raise();
    }
}

/// Set the presentation cap: the fewest VBlanks between two display flips.
///
/// Every program starts at [`PresentCap::HZ30`]: a frame finished early waits
/// for its edge instead of flipping at 60 for a moment and falling back. A
/// program that targets the full display rate says so, once or per phase (a
/// menu at 60, gameplay at 30): `set_cap(PresentCap::UNCAPPED)`. The cap applies
/// to every flip the handler makes; display enable is never held, and frames
/// slower than the cap are not delayed. Takes effect at the next flip.
pub fn set_cap(cap: PresentCap) {
    crate::interrupts::write_flip_min(cap.min_vblanks());
}

/// The cap in force.
#[must_use]
pub fn cap() -> PresentCap {
    PresentCap::every_vblanks(crate::interrupts::read_flip_min())
}

/// True while a published part waits to be kicked.
#[inline]
#[must_use]
pub fn is_slot_full() -> bool {
    Hardware.head() != 0
}

/// True while a flip (or display enable) waits for its edge.
#[inline]
#[must_use]
pub fn is_flip_pending() -> bool {
    Hardware.flip() != 0
}

/// The thread-side kick: start the part in the slot now if its condition
/// holds. Every wait below runs it on each pass.
#[inline]
pub fn kick() {
    Hardware.kick();
}

/// Block until the slot is empty (the part in it has been kicked). Spins only
/// when the CPU is a whole frame ahead. Bounded: a stall is recovered after
/// [`STALL_VBLANKS`], and with no handler running the wait gives up after
/// [`SPIN_LIMIT`] passes and asserts in a debug build.
#[inline(never)]
pub fn wait_slot_empty() {
    let vblanks = wait_slot_empty_on(&mut Hardware);
    add(addr_of_mut!(SLOT_WAIT_VBLANKS), vblanks);
}

/// Block until a pending flip has landed. Bounded like [`wait_slot_empty`].
#[inline(never)]
pub fn wait_flip() {
    wait_flip_on(&mut Hardware);
}

/// Wait until memory used by the frame before the most recently submitted one
/// is no longer read by a walk. Once the last frame's first part has been
/// kicked, the frame before it was on screen, so its walks had ended; while
/// that part still waits, the only walk that can be running is the older
/// frame's. Call it before rebuilding a frame's chain memory. Bounded like
/// [`wait_slot_empty`].
#[inline(never)]
pub fn wait_arena_free() {
    let vblanks = wait_arena_free_on(&mut Hardware);
    add(addr_of_mut!(ARENA_WAIT_VBLANKS), vblanks);
    compiler_barrier();
}

/// Wait until nothing is in flight: the slot empty, the last flip landed and
/// no walk running. The direct-access guard runs this before the first direct
/// GPU access after a chain was handed over, so a direct access never collides
/// with a walk. Bounded like
/// [`wait_slot_empty`]; a walk it gives up on is stopped by hand.
#[doc(alias = "DrawSync")]
#[inline(never)]
pub fn wait_idle() {
    wait_idle_on(&mut Hardware);
    compiler_barrier();
}

/// Show the buffer at `origin`, filled outside the frame path (a movie frame
/// uploaded by DMA), at the first edge after the GPU has finished with what it
/// was sent: acknowledge bit 24, send the draw-done sequence, queue the flip.
/// Waits first for the slot to empty and any earlier flip to land.
pub fn show(origin: (u16, u16)) {
    show_on(
        &mut Hardware,
        gp1::display_start(u32::from(origin.0), u32::from(origin.1)),
    );
}

/// Turn the video output on or off (GP1(03h)) at the next edge on which bit 24
/// is up, never held by the cap. Waits first for the slot to empty and any
/// earlier flip to land. Call it between frames.
pub fn set_display_enabled(enabled: bool) {
    let port = &mut Hardware;
    wait_slot_empty_on(port);
    wait_flip_on(port);
    port.set_flip(gp1::display_enable(enabled));
}

/// Drop a flip that has not landed and return its word (0 when none was
/// pending), for a program that takes the display over (a movie player
/// handing it back).
#[must_use = "the word is the flip that will not happen"]
pub fn cancel_flip() -> u32 {
    Hardware.take_flip()
}

/// Kick, skip and recovery counts and wait times since boot.
#[must_use]
pub fn stats() -> PresentStats {
    // SAFETY: volatile aligned word reads of counters written with single word stores.
    unsafe {
        let handler = read_volatile(addr_of!(__psx_rt_present_kick_count));
        let thread = read_volatile(addr_of!(__psx_rt_present_thread_kick_count));
        PresentStats {
            kick_count: handler.wrapping_add(thread),
            thread_kick_count: thread,
            skip_count: read_volatile(addr_of!(__psx_rt_present_skip_count)),
            recovery_count: read_volatile(addr_of!(RECOVERY_COUNT)),
            slot_wait_vblanks: read_volatile(addr_of!(SLOT_WAIT_VBLANKS)),
            arena_wait_vblanks: read_volatile(addr_of!(ARENA_WAIT_VBLANKS)),
        }
    }
}

/// The one object that presents frames. Create it once, after
/// [`crate::interrupts::install_vblank_counter`], with the GPU idle; it
/// registers the direct-access guard and raises bit 24 so the first frame's
/// kick finds the frame "before" it on screen.
///
/// The [`DoubleBuffer`] each call takes is the program's; the presenter is the
/// only thing that moves its draw side.
#[derive(Debug)]
pub struct Presenter {
    /// A part of the frame being submitted has gone out (its preamble too).
    frame_open: bool,
    /// The clear in front of every frame, `None` for none.
    clear: Option<(u8, u8, u8)>,
}

impl Presenter {
    /// Take over presentation. `clear` is the colour every chain frame is
    /// cleared to before its first part draws (`None`: not cleared).
    #[must_use]
    pub fn new(clear: Option<(u8, u8, u8)>) -> Presenter {
        psx_io::gpu::set_direct_access_guard(wait_idle);
        #[cfg(target_arch = "mips")]
        {
            psx_io::gpu::wait_command_ready();
            psx_io::gpu::signal_draw_done();
        }
        Presenter {
            frame_open: false,
            clear,
        }
    }

    /// Change the clear colour of the frames submitted from now on.
    pub fn set_clear(&mut self, clear: Option<(u8, u8, u8)>) {
        self.clear = clear;
    }

    /// Hand one chain of the frame being built to the queue and return.
    ///
    /// The first part of a frame gets a preamble linked in front: the draw area
    /// and offset of `buffers`' draw side and the clear. The last part
    /// ([`Part::Final`]) carries the frame's display word and moves `buffers` to
    /// the other side. Waits (bounded) only while the slot holds a part still
    /// waiting to be kicked, then kicks at once if it can (the frame before is
    /// on screen and channel 2 is idle), else the handler does at the first
    /// edge that allows it.
    ///
    /// # Safety
    ///
    /// `head` is the first node tag of a DMA linked-list chain whose nodes are
    /// at most 16 words each. A [`Part::Final`] chain ends on GP0(1Fh) and a
    /// [`Part::More`] chain holds none. The chain and everything it links stay
    /// live and unmodified until its walk has ended: until [`wait_arena_free`]
    /// returns before the frame after next is built, or [`wait_idle`].
    pub unsafe fn submit(&mut self, buffers: &mut DoubleBuffer, head: *const u32, part: Part) {
        let port = &mut Hardware;
        let vblanks = wait_slot_empty_on(port);
        add(addr_of_mut!(SLOT_WAIT_VBLANKS), vblanks);
        let mut first = head as u32;
        if !self.frame_open {
            first = self.write_preamble(buffers, first);
            self.frame_open = true;
        }
        let display = match part {
            Part::Final => {
                self.frame_open = false;
                buffers.begin_deferred_swap()
            }
            Part::More => 0,
        };
        #[cfg(feature = "console-diag")]
        psx_io::diag::ll_scan(first);
        // Keep the chain's stores ahead of the slot stores the kick reads.
        compiler_barrier();
        submit_on(port, first, display);
        psx_io::gpu::arm_direct_access_guard();
    }

    /// Write this frame's preamble, linked to `head`, and return its address.
    fn write_preamble(&self, buffers: &DoubleBuffer, head: u32) -> u32 {
        let side = usize::from(buffers.draw_origin().1 != 0);
        let [top_left, bottom_right, offset] = buffers.draw_target_words();
        // Without a clear the node is three words long and the last three
        // are never read.
        let (words, fill) = match self.clear {
            Some((r, g, b)) => (6u32, gp0::fill_rect(r, g, b)),
            None => (3, 0),
        };
        let (at, size) = (buffers.draw_origin(), buffers.size());
        // SAFETY: only this side's preamble is written, through a raw pointer, and the
        // frame that last linked it (two frames back) finished its walk before the
        // slot wait in `submit` returned (see `wait_arena_free`).
        unsafe {
            let preamble = addr_of_mut!(PREAMBLES[side]).cast::<u32>();
            preamble.write((words << 24) | (head & 0x00ff_ffff));
            preamble.add(1).write(top_left);
            preamble.add(2).write(bottom_right);
            preamble.add(3).write(offset);
            preamble.add(4).write(fill);
            preamble.add(5).write(pack_xy(at.0, at.1));
            preamble.add(6).write(pack_xy(size.0, size.1));
            preamble as u32
        }
    }

    /// Start a frame drawn through the GP0 port (or kicked by the program
    /// itself): wait until the frame handed over last is on screen and nothing
    /// walks, then point the draw area and offset at `buffers`' draw side. Call
    /// it before the frame's first direct GPU access; drawing earlier would land
    /// in the buffer still on screen. Returns at once when nothing is pending.
    ///
    /// The wait is here, at a call the program makes once a frame, and not in
    /// the port writes: a program that never hands over a chain or records
    /// pays nothing on its draws.
    pub fn begin_frame(&mut self, buffers: &DoubleBuffer) {
        psx_io::gpu::run_direct_access_guard();
        let port = &mut Hardware;
        if port.flip() != 0 || port.head() != 0 || port.is_channel_busy() {
            wait_idle_on(port);
        }
        compiler_barrier();
        for word in buffers.draw_target_words() {
            psx_io::gpu::write_command_unguarded(word);
        }
    }

    /// Present a frame drawn through the GP0 port into `buffers`' draw side,
    /// started with [`begin_frame`](Self::begin_frame), and return at once:
    /// acknowledge bit 24, send the draw-done sequence, queue the flip, move
    /// `buffers` to the other side. The next frame's `begin_frame` waits until
    /// the flip has landed.
    pub fn present_drawn(&mut self, buffers: &mut DoubleBuffer) {
        // The previous flip must have landed before the acknowledge, or it
        // would wait on this frame's GP0(1Fh) instead of its own: normally
        // `begin_frame` saw to it; a frame that skipped it waits here.
        psx_io::gpu::run_direct_access_guard();
        let port = &mut Hardware;
        if port.flip() != 0 || port.head() != 0 {
            wait_idle_on(port);
        }
        let display = buffers.begin_deferred_swap();
        present_drawn_on(port, display);
    }

    /// [`present_drawn`](Self::present_drawn) and wait until the frame is on
    /// screen: a loading screen, the last frame before a long blocking read.
    pub fn present_blocking(&mut self, buffers: &mut DoubleBuffer) {
        self.present_drawn(buffers);
        wait_flip();
    }
}

/// Publish one chain with the GP1(05h) word that shows the frame before it
/// (0 for none): the first present queue's contract.
///
/// # Safety
///
/// As [`Presenter::submit`], with the slot empty.
#[deprecated(
    note = "use `Presenter::submit`, which links the draw target and shows each frame when it is drawn"
)]
pub unsafe fn publish_raw(head: *const u32, display: u32) {
    let port = &mut Hardware;
    compiler_barrier();
    // The old contract carries the previous frame's flip with the next frame;
    // queue it as this part's flip, after the slot wait, as the old handler did
    // at the kick.
    wait_slot_empty_on(port);
    wait_flip_on(port);
    if display != 0 {
        port.set_flip(display);
    }
    submit_on(port, head as u32, 0);
    psx_io::gpu::arm_direct_access_guard();
}

/// Register the guard and raise bit 24: the first present queue's start.
#[deprecated(note = "use `Presenter::new`")]
pub fn start() {
    let _ = Presenter::new(None);
}

// The thread side, over a `Port`. On the console the port is `Hardware` and
// every method inlines to the access it names; the host tests run the same code
// over the model.

/// Fill the slot (it is empty: only the thread fills it, and the caller just
/// waited for that) and kick at once if the kick's condition holds.
fn submit_on<P: Port>(port: &mut P, head: u32, display: u32) {
    debug_assert_eq!(port.head(), 0, "submit_on with the slot full");
    // The display word first: the kick reads it only after it reads a head.
    port.set_display(display);
    port.set_head(head);
    port.kick();
}

/// The thread side of `present_drawn`, from the acknowledge on. The caller has
/// waited, so the slot is empty and the flip word 0, and only the thread could
/// change either.
fn present_drawn_on<P: Port>(port: &mut P, display: u32) {
    port.acknowledge_draw_done();
    port.signal_draw_done();
    port.set_flip(display);
}

fn show_on<P: Port>(port: &mut P, display: u32) {
    wait_slot_empty_on(port);
    wait_flip_on(port);
    port.acknowledge_draw_done();
    port.signal_draw_done();
    port.set_flip(display);
}

/// The VBlanks between `start` and now.
fn since<P: Port>(port: &mut P, start: u32) -> u32 {
    port.vblanks().wrapping_sub(start)
}

/// A stalled walk: with bit 24 still down after [`STALL_VBLANKS`], stop the
/// walk and raise the flag by hand, so the flip waiting on it can land.
fn recover<P: Port>(port: &mut P) {
    if !port.is_draw_done() {
        port.stop_walk_and_raise();
    }
}

/// Returns the VBlanks waited.
fn wait_slot_empty_on<P: Port>(port: &mut P) -> u32 {
    let start = port.vblanks();
    let mut spins = 0u32;
    loop {
        port.kick();
        if port.head() == 0 {
            break;
        }
        if since(port, start) >= STALL_VBLANKS {
            recover(port);
        }
        spins += 1;
        if spins >= SPIN_LIMIT {
            debug_assert!(
                false,
                "present: the slot never emptied; is the VBlank handler installed?"
            );
            break;
        }
    }
    since(port, start)
}

fn wait_flip_on<P: Port>(port: &mut P) {
    let start = port.vblanks();
    let mut spins = 0u32;
    while port.flip() != 0 {
        if since(port, start) >= STALL_VBLANKS {
            recover(port);
        }
        spins += 1;
        if spins >= SPIN_LIMIT {
            debug_assert!(
                false,
                "present: the flip never landed; is the VBlank handler installed?"
            );
            break;
        }
    }
}

/// Returns the VBlanks waited.
fn wait_arena_free_on<P: Port>(port: &mut P) -> u32 {
    let start = port.vblanks();
    let mut spins = 0u32;
    while port.head() != 0 && port.is_channel_busy() {
        if since(port, start) >= STALL_VBLANKS {
            recover(port);
        }
        spins += 1;
        if spins >= SPIN_LIMIT {
            debug_assert!(
                false,
                "present: the arena never freed; is the VBlank handler installed?"
            );
            break;
        }
    }
    // The walk ahead has ended: start the waiting part now rather than at the edge.
    port.kick();
    since(port, start)
}

fn wait_idle_on<P: Port>(port: &mut P) {
    wait_slot_empty_on(port);
    let start = port.vblanks();
    let mut spins = 0u32;
    while port.flip() != 0 || port.is_channel_busy() {
        spins += 1;
        if since(port, start) >= STALL_VBLANKS || spins >= SPIN_LIMIT {
            port.stop_walk_and_raise();
            break;
        }
    }
}

/// Add `vblanks` to a thread-only counter.
fn add(counter: *mut u32, vblanks: u32) {
    // SAFETY: `counter` is one of this module's thread-only statics; no handler touches it.
    unsafe { write_volatile(counter, read_volatile(counter).wrapping_add(vblanks)) }
}

/// Stop a walk that is not ending and raise bit 24 through the port, past
/// the direct-access guard: this runs inside it ([`wait_idle`]), and from the
/// paired-arena fence, whose scratchpad stack the stack guard must bound
/// without the guard's function pointer.
#[cold]
#[inline(never)]
fn stop_walk_and_raise() {
    #[cfg(target_arch = "mips")]
    {
        use psx_io::dma::{self, Channel};
        if dma::is_busy(Channel::Gpu) && !dma::wait_done(Channel::Gpu, dma::DEFAULT_SPINS) {
            dma::abort(Channel::Gpu);
            psx_io::gpu::write_display_control_unguarded(gp1::RESET_CMD_BUFFER);
        }
        // `wait_command_ready`, with its timeout reset written past the guard.
        if !psx_io::gpu::try_wait_command_ready(psx_io::gpu::READY_SPINS) {
            psx_io::gpu::write_display_control_unguarded(gp1::RESET_CMD_BUFFER);
        }
        psx_io::gpu::signal_draw_done_unguarded();
    }
    add(addr_of_mut!(RECOVERY_COUNT), 1);
}

/// Compiler-only barrier (see `psx_io::dma`'s): the pinned MIPS-I backend
/// lowers a compiler fence to `SYNC`, which the R3000 lacks, so the target
/// uses an empty `asm!` with its default memory clobber.
#[inline(always)]
fn compiler_barrier() {
    #[cfg(target_arch = "mips")]
    // SAFETY: an empty asm block; it only constrains compiler ordering.
    unsafe {
        core::arch::asm!("", options(nostack, preserves_flags));
    }
    #[cfg(not(target_arch = "mips"))]
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
#[path = "present_model.rs"]
mod model;
