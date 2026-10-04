//! The raw linked-list layer: DMA channel 2 walking a chain of GP0 nodes.
//!
//! Each node is a tag word, the next node's address in bits 23..=0
//! ([`LIST_END`] ends the list) and the payload word count in bits 31..=24,
//! followed by that many GP0 words. The DMA controller follows the links on
//! its own, so whatever a tag names is read with no regard for Rust's
//! borrows: that is why [`submit_raw`] and [`submit_async_raw`] are
//! `unsafe`. The safe layers above them prove their contract instead:
//! [`crate::frame`] with lifetimes, [`StaticChain`] with `'static`
//! immutability, and [`crate::ordered`] with `'static` storage it owns.
//!
//! Every wait here is bounded. A walk that outlives its budget is aborted
//! and the GPU's half-read command discarded, so a wedged channel costs a
//! reset instead of a hang; [`crate::recovery_stats`] counts the aborts.

use psx_hw::gpu::{gp0, gp1, DmaDirection};
use psx_io::dma::{self, Channel};
use psx_io::gpu::write_display_control;
use psx_io::periph::GpuDma;

/// The link value that ends a list.
pub const LIST_END: u32 = 0x00FF_FFFF;

/// Most payload words one linked-list DMA node may carry (the words after
/// its tag), the depth of the GPU's command FIFO.
///
/// Silicon very likely loses words from longer nodes while it draws.
/// Hardware-tests v1.24 drew the same 16 half-screen Gouraud triangles as
/// 16 nodes and as 4 nodes of 24 words: the packed list's closing GP0(1Fh)
/// arrived at 314,075 clocks against 625,348, about 8 triangles' worth at
/// the 39,084 clocks each costs, and its last-node drain matched the
/// unpacked list's one-triangle gap, so roughly half its drawing never
/// happened. Every SDK node builder stays at or under this limit:
/// [`crate::ordered::OrderedCommandStream`] caps nodes at
/// [`crate::ordered::NODE_PAYLOAD_WORDS`], the frame adds refuse longer
/// packets, and every [`crate::prim`] packet is shorter.
pub const MAX_NODE_WORDS: usize = 16;

/// A linked-list DMA node holding only GP0(1Fh), for the end of a chain.
///
/// Link it as the chain's last node and the GPU raises
/// [`crate::is_draw_done`] when it gets there. It is immutable and shared:
/// every chain can end on [`DRAW_DONE_NODE`].
#[derive(Debug)]
#[repr(C, align(4))]
pub struct DrawDoneNode([u32; 2]);

impl DrawDoneNode {
    /// The node's tag word, the address a chain links to.
    #[inline]
    pub fn as_ptr(&self) -> *const u32 {
        self.0.as_ptr()
    }
}

/// The shared GP0(1Fh) node: one payload word, then the end of the list.
pub static DRAW_DONE_NODE: DrawDoneNode = DrawDoneNode([(1 << 24) | LIST_END, gp0::REQUEST_IRQ]);

/// One immutable linked-list node: `W` GP0 words, then the end of the list.
///
/// Built in a `static`, it is a chain that stays valid for the whole run, so
/// [`crate::Gpu::submit_static`] can kick it from safe code without waiting.
///
/// ```
/// use psx_gpu::chain::StaticPacket;
/// // GP0(E1h) draw mode, then GP0(1Fh).
/// static MODE_THEN_IRQ: StaticPacket<2> = StaticPacket::new([0xE100_0000, 0x1F00_0000]);
/// assert_eq!(MODE_THEN_IRQ.words(), &[0xE100_0000, 0x1F00_0000]);
/// ```
#[derive(Debug)]
#[repr(C, align(4))]
pub struct StaticPacket<const W: usize> {
    tag: u32,
    words: [u32; W],
}

impl<const W: usize> StaticPacket<W> {
    /// A node carrying `words` that ends the list.
    pub const fn new(words: [u32; W]) -> Self {
        const { assert!(W <= MAX_NODE_WORDS, "packet longer than one GPU DMA node") };
        Self {
            tag: ((W as u32) << 24) | LIST_END,
            words,
        }
    }

    /// The payload words.
    pub const fn words(&self) -> &[u32; W] {
        &self.words
    }

    /// The node's tag word, the address a chain links to.
    #[inline]
    pub fn as_ptr(&self) -> *const u32 {
        // From the whole node, so the walk may read the payload after it.
        core::ptr::from_ref(self).cast::<u32>()
    }
}

mod sealed {
    pub trait Sealed {}
}

/// A complete, immutable linked list: [`StaticPacket`] or [`DrawDoneNode`].
///
/// Sealed: implementors guarantee that a shared reference to them is a
/// whole chain whose nodes never change while the reference lives.
pub trait StaticChain: sealed::Sealed {
    /// Address of the first node's tag.
    fn head(&self) -> *const u32;
}

impl sealed::Sealed for DrawDoneNode {}
impl StaticChain for DrawDoneNode {
    #[inline]
    fn head(&self) -> *const u32 {
        self.as_ptr()
    }
}

impl<const W: usize> sealed::Sealed for StaticPacket<W> {}
impl<const W: usize> StaticChain for StaticPacket<W> {
    #[inline]
    fn head(&self) -> *const u32 {
        self.as_ptr()
    }
}

/// Kick the chain at `head` and return while the GPU walks it.
///
/// A walk already running on the channel is waited out first (or aborted
/// if it wedged), so the CPU can build the next frame while this one is
/// drawn. Pair it with [`wait`].
///
/// # Safety
///
/// `head` must point at a 4-byte-aligned node tag in main RAM. Every node
/// reachable from it must have at most [`MAX_NODE_WORDS`] payload words and
/// lie in main RAM, and must stay live and unmodified until [`wait`]
/// returns (or a later kick, which waits for this walk first).
#[doc(alias = "DrawOTag")]
#[inline]
pub unsafe fn submit_async_raw(_dma: &mut GpuDma, head: *const u32) {
    // SAFETY: forwarded contract; the token's borrow is the caller's proof
    // that no other code drives the channel.
    unsafe { start_walk(head) }
}

/// Kick the chain at `head` and wait until it has been walked.
///
/// # Safety
///
/// As [`submit_async_raw`], for the duration of this call.
#[doc(alias = "DrawOTag")]
#[inline]
pub unsafe fn submit_raw(dma: &mut GpuDma, head: *const u32) {
    // SAFETY: forwarded contract; the wait ends the walk before return.
    unsafe { submit_async_raw(dma, head) };
    wait(dma);
}

/// Wait until the walk kicked last has finished reading its chain.
///
/// This is the CPU-blocked-on-GPU part of a submission; profiling code
/// times it apart from the kick to split GPU-draw cost from CPU build cost.
/// The GPU may still be drawing the last packets; the chain's memory is
/// free once this returns.
#[inline]
pub fn wait(_dma: &mut GpuDma) {
    wait_walk();
}

/// [`submit_async_raw`]'s body, for callers that hold the token elsewhere.
///
/// # Safety
///
/// As [`submit_async_raw`].
pub(crate) unsafe fn start_walk(head: *const u32) {
    // A finished walk does not mean the GPU has drawn what it read. Do not
    // wait for idle here: channel 2's request handshake queues the next list
    // behind that work, which is how PsyQ and PSn00bSDK keep the GPU fed.
    // Only the channel and the list's storage must be free.
    if !dma::wait_done(Channel::Gpu, dma::DEFAULT_SPINS) {
        abort_wedged_walk();
    }

    // Games re-route DMA for VRAM readback and do not always restore it, so
    // point it at GP0 before every kick.
    write_display_control(gp1::dma_direction(DmaDirection::CpuToGp0 as u32));
    dma::enable_channel(Channel::Gpu);
    // SAFETY: the channel is idle (waited out or aborted above); the caller
    // keeps the chain live and unmodified until the walk is waited out.
    // `dma::start` publishes the payload and tag stores before the CHCR
    // store.
    unsafe {
        dma::start(
            Channel::Gpu,
            dma::Transfer {
                address: head.expose_provenance() as u32,
                // Linked-list mode ignores BCR, but hardware wants it written.
                size: dma::size_words(0),
                control: psx_hw::dma::CHCR_TO_DEVICE
                    | psx_hw::dma::CHCR_SYNC_LINKED
                    | psx_hw::dma::CHCR_START,
            },
        )
    };
}

/// [`wait`]'s body, for callers that hold the token elsewhere.
#[inline]
pub(crate) fn wait_walk() {
    if !dma::wait_done(Channel::Gpu, dma::DEFAULT_SPINS) {
        abort_wedged_walk();
    }
    // Keep the caller's buffer-reuse stores after the completion read (or
    // the abort).
    dma::compiler_barrier();
}

/// Stop a channel-2 walk that outlived its spin budget.
///
/// The walker stopped mid-packet, so the GPU still waits for the rest of a
/// command: GP1(01h) discards it, or every later ready-wait blocks on a GPU
/// that can never become ready. The reset goes past the present-queue
/// direct-access guard, which would wait on the walk that just wedged; the
/// paired-arena fence reaches this from a scratchpad stack that stack-guard
/// bounds only through direct calls, hence `inline(always)`.
#[inline(always)]
fn abort_wedged_walk() {
    dma::abort(Channel::Gpu);
    psx_io::gpu::write_display_control_unguarded(gp1::RESET_CMD_BUFFER);
    // SAFETY: volatile aligned accesses to a private static that only this
    // function writes; the program is single threaded and no handler
    // touches it.
    unsafe {
        let count = core::ptr::addr_of_mut!(DMA_ABORTS);
        count.write_volatile(count.read_volatile().wrapping_add(1));
    }
}

/// Walks [`abort_wedged_walk`] has stopped since boot.
static mut DMA_ABORTS: u32 = 0;

/// Walks aborted since boot.
pub(crate) fn dma_abort_count() -> u32 {
    // SAFETY: a volatile aligned read of a private static.
    unsafe { core::ptr::addr_of!(DMA_ABORTS).read_volatile() }
}
