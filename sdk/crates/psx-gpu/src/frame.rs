//! Borrow-checked frame building over an ordering table.
//!
//! The GPU's linked-list DMA reads every packet of a frame after the CPU has
//! handed it over, so a packet must outlive the walk and must not change
//! during it. This module states that with lifetimes instead of `unsafe`:
//!
//! - [`OrderingTable::frame`] clears the table and borrows it as an
//!   [`OtFrame<'f, N>`] for `'f`.
//! - [`OtFrame::add`] takes each packet as `&'f mut P`, so the packet stays
//!   borrowed, and unmovable, for as long as the frame can be walked.
//! - [`OtFrame::submit`] and [`OtFrame::submit_with`] wait for the walk
//!   before they return, which is what lets `'f` end.
//! - [`PrimitiveArena`] splits caller storage into `&'f mut P` packets.
//! - For a walk that outlives the call that kicked it (CPU frame N+1 built
//!   while the GPU walks frame N), [`FrameStorage::draw_async`] needs the
//!   table and packet storage `&'static mut` and returns an [`InFlight`]
//!   that owns them, and the [`GpuDma`] token, until [`InFlight::wait`].
//!   Forgetting it leaks the storage rather than freeing it under the walk.
//!   [`FramePair`] runs the usual two-buffer ping-pong on top.
//!
//! Every type here is a thin wrapper over the same stores and kicks as
//! [`OtFrame::add_raw`] and [`crate::chain::submit_raw`].
//!
//! # Usage
//!
//! ```no_run
//! use psx_gpu::frame::{FramePair, FrameStorage, PrimitiveArena};
//! use psx_gpu::{ot::OrderingTable, prim::TriFlat};
//!
//! fn blocking(ot: &mut OrderingTable<8>, dma: &mut psx_io::periph::GpuDma) {
//!     let mut storage: [TriFlat; 4] =
//!         core::array::from_fn(|_| TriFlat::new([(0, 0); 3], 0, 0, 0));
//!     let mut frame = ot.frame();
//!     let mut arena = PrimitiveArena::new(&mut storage);
//!     if let Some(tri) = arena.push(TriFlat::new([(0, 0), (8, 0), (0, 8)], 255, 0, 0)) {
//!         frame.add(1, tri);
//!     }
//!     frame.submit(dma);
//!     // The walk is over: the table and the storage are free again.
//!     ot.clear();
//!     storage[0] = TriFlat::new([(0, 0); 3], 0, 0, 0);
//! }
//!
//! fn overlapped(
//!     storage: &'static mut FrameStorage<8, [TriFlat; 4]>,
//!     dma: psx_io::periph::GpuDma,
//! ) {
//!     let (in_flight, added) = storage.draw_async(dma, |frame, packets| {
//!         let mut arena = PrimitiveArena::new(packets);
//!         let tri = arena.push(TriFlat::new([(0, 0), (8, 0), (0, 8)], 0, 255, 0));
//!         tri.map(|tri| frame.add(1, tri)).is_some()
//!     });
//!     // ... CPU work while the GPU walks ...
//!     let (storage, dma) = in_flight.wait();
//!     let mut pair = FramePair::new(storage, other_storage(), dma);
//!     pair.present(|_frame, _packets| {});
//! #   let _ = added;
//! }
//! # fn other_storage() -> &'static mut FrameStorage<8, [TriFlat; 4]> { unimplemented!() }
//! ```
//!
//! # What the borrow checker rejects
//!
//! A packet that dies before the frame is submitted:
//!
//! ```compile_fail,E0597
//! # use psx_gpu::{ot::OrderingTable, prim::TriFlat};
//! # fn f(dma: &mut psx_io::periph::GpuDma) {
//! let mut ot = OrderingTable::<8>::new();
//! let mut frame = ot.frame();
//! {
//!     let mut tri = TriFlat::new([(0, 0), (8, 0), (0, 8)], 255, 0, 0);
//!     frame.add(1, &mut tri);
//! } // `tri` dropped here while the frame still links it
//! frame.submit(dma);
//! # }
//! ```
//!
//! Clearing (or otherwise touching) the table while its walk is in flight:
//!
//! ```compile_fail,E0499
//! # use psx_gpu::ot::OrderingTable;
//! # fn f(dma: &mut psx_io::periph::GpuDma) {
//! let mut ot = OrderingTable::<8>::new();
//! let frame = ot.frame();
//! frame.submit_with(dma, || ot.clear());
//! # }
//! ```
//!
//! Reusing the storage of an asynchronously kicked frame before waiting:
//!
//! ```compile_fail,E0499
//! # use psx_gpu::{frame::FrameStorage, prim::TriFlat};
//! # fn f(storage: &'static mut FrameStorage<8, [TriFlat; 4]>, dma: psx_io::periph::GpuDma) {
//! let (in_flight, ()) = storage.draw_async(dma, |_frame, _packets| {});
//! storage.draw_async(unsafe { psx_io::periph::GpuDma::steal() }, |_frame, _packets| {});
//! in_flight.wait();
//! # }
//! ```
//!
//! Restarting an arena over storage whose packets are still linked:
//!
//! ```compile_fail,E0499
//! # use psx_gpu::{frame::PrimitiveArena, ot::OrderingTable, prim::TriFlat};
//! # fn f(dma: &mut psx_io::periph::GpuDma) {
//! let mut ot = OrderingTable::<8>::new();
//! let mut storage: [TriFlat; 4] = core::array::from_fn(|_| TriFlat::new([(0, 0); 3], 0, 0, 0));
//! let mut frame = ot.frame();
//! let mut arena = PrimitiveArena::new(&mut storage);
//! frame.add(1, arena.push(TriFlat::new([(0, 0), (8, 0), (0, 8)], 255, 0, 0)).unwrap());
//! let mut again = PrimitiveArena::new(&mut storage); // still borrowed by the frame
//! again.push(TriFlat::new([(0, 0); 3], 0, 0, 0));
//! frame.submit(dma);
//! # }
//! ```

use crate::ot::OrderingTable;
use crate::prim::GpuPacket;
use psx_io::periph::GpuDma;

/// One frame's view of an [`OrderingTable`]: insert packets, then submit.
///
/// Made by [`OrderingTable::frame`]. Every packet added must live for `'f`,
/// the frame's borrow of the table, and is exclusively borrowed for that
/// long; submitting consumes the frame and waits for the walk.
#[derive(Debug)]
#[must_use = "a frame does nothing until it is submitted"]
pub struct OtFrame<'f, const N: usize> {
    ot: &'f mut OrderingTable<N>,
}

impl<const N: usize> OrderingTable<N> {
    /// Clear the table and start building a frame in it.
    ///
    /// The table stays borrowed until the frame is submitted and its walk
    /// has finished, so it can neither be cleared nor moved under the DMA.
    #[doc(alias = "ClearOTagR")]
    #[inline]
    pub fn frame(&mut self) -> OtFrame<'_, N> {
        self.clear();
        OtFrame { ot: self }
    }

    /// Continue a frame in this table without clearing it, for a frame
    /// built in several phases (a scene pass, then an overlay pass).
    ///
    /// # Safety
    ///
    /// Every packet the table links already, through an earlier
    /// [`OtFrame`] or a raw add, must
    /// stay live and unmodified, except for tags later adds write, until the
    /// returned frame's walk has finished (or for good, if it is never
    /// submitted). Packets an earlier frame added with [`OtFrame::add`] were
    /// borrowed only for that frame, so the caller now owns their lifetime.
    /// The table must not be walking.
    #[inline]
    pub unsafe fn resume_frame(&mut self) -> OtFrame<'_, N> {
        OtFrame { ot: self }
    }
}

impl<'f, const N: usize> OtFrame<'f, N> {
    /// Prepend `packet` to depth slot `z` (clamped to `N - 1`). Slot `N - 1`
    /// is walked first, slot 0 last, so lower slots draw on top.
    #[inline(always)]
    pub fn add<P: GpuPacket>(&mut self, z: usize, packet: &'f mut P) {
        const {
            assert!(P::WORDS as usize <= crate::chain::MAX_NODE_WORDS);
            assert!(core::mem::size_of::<P>() >= 4 * (1 + P::WORDS as usize));
            assert!(core::mem::align_of::<P>() >= 4);
        };
        // SAFETY: `packet` is a `GpuPacket` (tag word first, `WORDS` payload
        // words after it, at most one node), borrowed exclusively for 'f.
        // The frame can only be walked by `submit`/`submit_with`, which wait
        // before 'f can end, or by `FrameStorage` with 'f = 'static.
        unsafe { self.ot.link(z, (packet as *mut P).cast::<u32>(), P::WORDS) };
    }

    /// Prepend a raw packet to depth slot `z` (clamped to `N - 1`).
    ///
    /// # Safety
    ///
    /// `packet` must point at a 4-byte-aligned tag word followed by `words`
    /// payload words, all in RAM that stays live and unmodified, except for
    /// the tag this call writes, until this frame's walk has finished (or
    /// for good, if the frame is never submitted).
    ///
    /// # Panics
    ///
    /// If `words` exceeds [`crate::chain::MAX_NODE_WORDS`].
    #[inline(always)]
    pub unsafe fn add_raw(&mut self, z: usize, packet: *mut u32, words: u8) {
        // SAFETY: forwarded contract.
        unsafe { self.ot.link(z, packet, words) };
    }

    /// [`add_raw`](Self::add_raw) without the depth clamp or the node-length
    /// check.
    ///
    /// # Safety
    ///
    /// The contract of [`add_raw`](Self::add_raw), and also: `z` is less
    /// than `N`, and `words` is at most [`crate::chain::MAX_NODE_WORDS`].
    #[inline(always)]
    pub unsafe fn add_raw_unchecked(&mut self, z: usize, packet: *mut u32, words: u8) {
        // SAFETY: forwarded contract.
        unsafe { self.ot.link_unchecked(z, packet, words) };
    }

    /// [`add_raw_unchecked`](Self::add_raw_unchecked) with the word count
    /// already in tag form, in bits 24..31 of `tag_high`.
    ///
    /// # Safety
    ///
    /// The contract of [`add_raw_unchecked`](Self::add_raw_unchecked), with
    /// `tag_high >> 24` as the word count, and also: the low 24 bits of
    /// `tag_high` are zero.
    #[inline(always)]
    pub unsafe fn add_raw_tag_high_unchecked(&mut self, z: usize, packet: *mut u32, tag_high: u32) {
        // SAFETY: forwarded contract.
        unsafe { self.ot.link_tag_high_unchecked(z, packet, tag_high) };
    }

    /// Add packed two-word commands, first to last, the way repeated classic
    /// `addPrim` calls do: packets that share a slot come out in reverse.
    ///
    /// Each command is two machine words: the packet's address (`ptr as
    /// usize`, which exposes it), then a word with the slot in bits 0..=15
    /// and the packet's payload word count in bits 24..=31. On the console
    /// this is one hand-scheduled loop; host builds run a scalar twin.
    ///
    /// # Safety
    ///
    /// `commands` points at `command_count * 2` readable words in that
    /// layout; every slot is less than `N` and every word count at most
    /// [`crate::chain::MAX_NODE_WORDS`]; and every packet meets the contract of
    /// [`add_raw`](Self::add_raw).
    #[inline(always)]
    pub unsafe fn add_packed_commands_unchecked(
        &mut self,
        commands: *const usize,
        command_count: usize,
    ) {
        // SAFETY: forwarded contract.
        unsafe {
            self.ot
                .link_packed_commands_unchecked(commands, command_count)
        };
    }

    /// [`add_packed_commands_unchecked`](Self::add_packed_commands_unchecked)
    /// last to first, so packets that share a slot keep their array order.
    ///
    /// # Safety
    ///
    /// As [`add_packed_commands_unchecked`](Self::add_packed_commands_unchecked).
    #[inline(always)]
    pub unsafe fn add_packed_commands_reverse_unchecked(
        &mut self,
        commands: *const usize,
        command_count: usize,
    ) {
        // SAFETY: forwarded contract.
        unsafe {
            self.ot
                .link_packed_commands_reverse_unchecked(commands, command_count)
        };
    }

    /// Add a contiguous stream of packets whose tags carry their own slot.
    ///
    /// Before this call each tag holds the packet's payload word count in
    /// bits 24..=31 and its slot in bits 0..=15. Packets are prepended from
    /// `first` to `end`, as repeated `addPrim` calls would; slot `0xFFFF`
    /// skips a packet, so separately ordered HUD packets can share the
    /// arena. Renderers stage depth keys in the tags as they build packets
    /// and leave the link pass to this one loop.
    ///
    /// # Safety
    ///
    /// `first..end` is a writable, contiguous sequence of complete packets
    /// in that layout; every word count describes the next packet exactly
    /// and is at most [`crate::chain::MAX_NODE_WORDS`]; every slot other than
    /// `0xFFFF` is less than `N`; and the whole range meets the contract of
    /// [`add_raw`](Self::add_raw).
    #[inline(always)]
    pub unsafe fn add_tagged_packet_stream_unchecked(&mut self, first: *mut u32, end: *mut u32) {
        // SAFETY: forwarded contract.
        unsafe { self.ot.link_tagged_packet_stream_unchecked(first, end) };
    }

    /// [`add_tagged_packet_stream_unchecked`](Self::add_tagged_packet_stream_unchecked)
    /// with every staged slot other than `0xFFFF` shifted right by
    /// `SLOT_SHIFT`, so a smaller table can back a fine depth range
    /// (`SLOT_SHIFT = 3` maps depths 0..2047 onto 256 slots).
    ///
    /// # Safety
    ///
    /// As [`add_tagged_packet_stream_unchecked`](Self::add_tagged_packet_stream_unchecked),
    /// with every shifted slot less than `N`, and `SLOT_SHIFT` less than 16.
    #[inline(always)]
    pub unsafe fn add_tagged_packet_stream_shifted_unchecked<const SLOT_SHIFT: u32>(
        &mut self,
        first: *mut u32,
        end: *mut u32,
    ) {
        // SAFETY: forwarded contract.
        unsafe {
            self.ot
                .link_tagged_packet_stream_shifted_unchecked::<SLOT_SHIFT>(first, end)
        };
    }

    /// End the walk with GP0(1Fh) so [`crate::is_draw_done`] rises once the
    /// frame is drawn; see [`OrderingTable::end_with_draw_done`].
    ///
    /// # Panics
    ///
    /// If anything was already added at slot 0, or the frame already ends
    /// in a link.
    #[inline]
    pub fn end_with_draw_done(&mut self) {
        self.ot.end_with_draw_done();
    }

    /// The node the frame's walk starts at, for a chain that continues into
    /// this frame (a recorded preamble handed to `psx_rt::present`). Only an
    /// address: kicking a walk from it is up to an unsafe call that takes on
    /// the frame's lifetime itself.
    #[inline]
    pub fn submit_head(&self) -> *const u32 {
        self.ot.submit_head()
    }

    /// Continue this frame's walk into `head`, a chain that ends the list
    /// itself; see [`OrderingTable::end_with_chain`].
    ///
    /// # Safety
    ///
    /// Every node reachable from `head` must stay live and unmodified for
    /// `'f`, and for any walk of this frame that outlives it, under
    /// [`crate::chain::submit_async_raw`]'s node rules.
    ///
    /// # Panics
    ///
    /// If anything was already added at slot 0, or the frame already ends
    /// in a link.
    #[inline]
    pub unsafe fn end_with_chain(&mut self, head: *const u32) {
        // SAFETY: forwarded contract.
        unsafe { self.ot.end_with_chain(head) };
    }

    /// Continue this frame's walk into `next` once its own slot 0 is done,
    /// so both tables go out in one kick.
    ///
    /// `next` is consumed: its table stays borrowed for `'f` and is walked as
    /// part of this frame. Its own packets keep their order; anything added
    /// to this frame's slot 0 afterwards still draws before `next`.
    ///
    /// Host builds link for real, but [`OrderingTable::packets`] only
    /// follows links within one table's address window.
    ///
    /// # Panics
    ///
    /// If anything was already added at slot 0, or the frame already ends
    /// in a link or GP0(1Fh): call it before slot 0 is used, and close only
    /// the last frame of a chain with [`end_with_draw_done`](Self::end_with_draw_done).
    #[inline]
    pub fn link_after<const M: usize>(&mut self, next: OtFrame<'f, M>) {
        // SAFETY: `next`'s table is borrowed for 'f, as long as this frame,
        // and was consumed, so nothing else can clear it or submit it.
        unsafe { self.ot.end_with_node(next.ot.submit_head()) };
    }

    /// Kick the walk and wait for it.
    #[doc(alias = "DrawOTag")]
    #[inline]
    pub fn submit(self, dma: &mut GpuDma) {
        // SAFETY: every node is the table (borrowed for 'f), a packet
        // borrowed for 'f through `add`, a raw packet whose `add_raw`
        // contract covers the walk, a linked `OtFrame<'f>`, or the static
        // GP0(1Fh) node. The walk ends before this returns.
        unsafe { crate::chain::submit_raw(dma, self.ot.submit_head()) };
    }

    /// Kick the walk, run `overlap` while the GPU walks, then wait.
    ///
    /// `overlap` cannot reach the table or the packets: they are still
    /// borrowed by this frame.
    #[inline]
    pub fn submit_with<R>(self, _dma: &mut GpuDma, overlap: impl FnOnce() -> R) -> R {
        // SAFETY: as `submit`; `_wait` waits for the walk before this
        // returns, on every path out of `overlap`.
        unsafe { crate::chain::start_walk(self.ot.submit_head()) };
        let _wait = WaitOnDrop;
        overlap()
    }
}

/// Waits for the GPU DMA walk when dropped.
struct WaitOnDrop;

impl Drop for WaitOnDrop {
    #[inline(always)]
    fn drop(&mut self) {
        crate::chain::wait_walk();
    }
}

/// An ordering table together with the packet storage its frames link.
///
/// The unit [`draw_async`](Self::draw_async) and [`FramePair`] keep
/// `'static` while the GPU walks it. `S` is whatever holds the packets:
/// an array, or a struct of arrays per packet type.
#[derive(Debug)]
pub struct FrameStorage<const N: usize, S> {
    ot: OrderingTable<N>,
    packets: S,
}

impl<const N: usize, S> FrameStorage<N, S> {
    /// An empty table and `packets` as the packet storage.
    pub const fn new(packets: S) -> Self {
        Self {
            ot: OrderingTable::new(),
            packets,
        }
    }

    /// The packet storage, outside any frame.
    pub fn packets_mut(&mut self) -> &mut S {
        &mut self.packets
    }

    /// Clear the table and run `build` with the frame and the storage.
    fn build<R>(&mut self, build: impl for<'f> FnOnce(&mut OtFrame<'f, N>, &'f mut S) -> R) -> R {
        let Self { ot, packets } = self;
        let mut frame = ot.frame();
        build(&mut frame, packets)
    }

    /// Kick the table built last without waiting.
    ///
    /// # Safety
    ///
    /// `self` must stay untouched (no `build`, no access to the packets)
    /// until the walk has been waited out.
    #[inline]
    unsafe fn kick(&mut self) {
        // SAFETY: the table and packets were linked by `build`, which only
        // admits packets in `self` or `'static` ones; the caller keeps
        // `self` untouched until the wait.
        unsafe { crate::chain::start_walk(self.ot.submit_head()) };
    }

    /// Build a frame with `build`, then kick it and return without waiting.
    ///
    /// The storage and the DMA token move into the returned [`InFlight`]
    /// until [`InFlight::wait`] hands them back. `build` gets the frame and
    /// the packet storage for a lifetime it cannot leak.
    pub fn draw_async<R>(
        &'static mut self,
        dma: GpuDma,
        build: impl for<'f> FnOnce(&mut OtFrame<'f, N>, &'f mut S) -> R,
    ) -> (InFlight<N, S>, R) {
        let result = self.build(build);
        // SAFETY: the storage moves into `InFlight`, which hands it back
        // only from `wait`, after the walk.
        unsafe { self.kick() };
        (InFlight { storage: self, dma }, result)
    }
}

/// A frame the GPU is walking, owning its `'static` storage and the
/// [`GpuDma`] token until [`wait`](Self::wait).
///
/// Dropping or forgetting it leaks both: the storage then stays out of
/// reach for the rest of the run instead of being reused under the walk.
#[derive(Debug)]
#[must_use = "dropping an in-flight frame leaks its storage; call wait()"]
pub struct InFlight<const N: usize, S: 'static> {
    storage: &'static mut FrameStorage<N, S>,
    dma: GpuDma,
}

impl<const N: usize, S> InFlight<N, S> {
    /// True once the walk has finished (the GPU may still be drawing the
    /// last packets; the packets themselves are free).
    #[inline]
    pub fn is_done(&self) -> bool {
        !psx_io::dma::is_busy(psx_io::dma::Channel::Gpu)
    }

    /// Wait for the walk, then hand back the storage and the token.
    #[inline]
    pub fn wait(self) -> (&'static mut FrameStorage<N, S>, GpuDma) {
        crate::chain::wait_walk();
        (self.storage, self.dma)
    }
}

/// Two [`FrameStorage`]s in the usual ping-pong: build frame N+1 into one
/// while the GPU walks frame N from the other.
///
/// ```ignore
/// loop {
///     pair.build(|frame, packets| { /* add packets */ });
///     pair.wait();                // frame N's walk is done
///     psx_gpu::arm_draw_done();   // anything that must precede the kick
///     pair.kick();                // frame N+1 goes out; storages swap
/// }
/// ```
#[derive(Debug)]
pub struct FramePair<const N: usize, S: 'static> {
    storage: [&'static mut FrameStorage<N, S>; 2],
    /// Index of the storage `build` writes; the other one may be in flight.
    next: usize,
    dma: GpuDma,
}

impl<const N: usize, S> FramePair<N, S> {
    /// Ping-pong between `a` and `b`, owning the GPU DMA token.
    pub fn new(
        a: &'static mut FrameStorage<N, S>,
        b: &'static mut FrameStorage<N, S>,
        dma: GpuDma,
    ) -> Self {
        Self {
            storage: [a, b],
            next: 0,
            dma,
        }
    }

    /// Build the next frame into the storage the GPU is not walking.
    ///
    /// Building again before [`kick`](Self::kick) starts that frame over.
    pub fn build<R>(
        &mut self,
        build: impl for<'f> FnOnce(&mut OtFrame<'f, N>, &'f mut S) -> R,
    ) -> R {
        self.storage[self.next].build(build)
    }

    /// Wait until the frame kicked last has been walked, and lend the token
    /// for work that must not overlap it (VRAM uploads, immediate GP0).
    #[inline]
    pub fn wait(&mut self) -> &mut GpuDma {
        crate::chain::wait_walk();
        &mut self.dma
    }

    /// Wait for the previous walk, kick the frame built last, and switch
    /// [`build`](Self::build) to the other storage.
    #[inline]
    pub fn kick(&mut self) {
        // SAFETY: `raw_async` waits for the previous walk before it starts
        // this one. The kicked storage is not touched again until the next
        // `kick` has waited this walk out: `build` only writes the other
        // storage from now on.
        unsafe { self.storage[self.next].kick() };
        self.next ^= 1;
    }

    /// Wait for the last walk, then hand back both storages and the token.
    pub fn release(self) -> ([&'static mut FrameStorage<N, S>; 2], GpuDma) {
        crate::chain::wait_walk();
        (self.storage, self.dma)
    }

    /// [`build`](Self::build), then [`kick`](Self::kick).
    pub fn present<R>(
        &mut self,
        build: impl for<'f> FnOnce(&mut OtFrame<'f, N>, &'f mut S) -> R,
    ) -> R {
        let result = self.build(build);
        self.kick();
        result
    }
}

/// Hands out packets from caller storage as `&'f mut P`, one slot each.
///
/// The storage is borrowed for `'f`, so it cannot be reused (a new arena
/// over it, say) while any packet it handed out is still linked into a
/// frame.
#[derive(Debug)]
pub struct PrimitiveArena<'f, P> {
    // A base pointer and a count rather than a shrinking `&mut [P]`: a push is
    // then one compare and one increment, where re-slicing loaded and stored
    // the slice's pointer and length on every packet and measured +1% work
    // per frame in NitroXide's wall pass.
    storage: *mut P,
    capacity: usize,
    used: usize,
    _storage: core::marker::PhantomData<&'f mut [P]>,
}

impl<'f, P> PrimitiveArena<'f, P> {
    /// Hand out the slots of `storage`, front to back.
    #[inline]
    pub fn new(storage: &'f mut [P]) -> Self {
        Self {
            storage: storage.as_mut_ptr(),
            capacity: storage.len(),
            used: 0,
            _storage: core::marker::PhantomData,
        }
    }

    /// Store `packet` in the next free slot and return it for the rest of
    /// `'f`; `None` once the storage is used up.
    #[inline(always)]
    pub fn push(&mut self, packet: P) -> Option<&'f mut P> {
        if self.used >= self.capacity {
            return None;
        }
        // SAFETY: `used < capacity`, so the slot lies in the storage `new` took
        // as `&'f mut [P]`; `used` only grows, so no slot is handed out twice
        // and the returned borrow aliases nothing else for 'f.
        let slot = unsafe { &mut *self.storage.add(self.used) };
        self.used += 1;
        *slot = packet;
        Some(slot)
    }

    /// Slots handed out so far.
    #[inline]
    pub fn len(&self) -> usize {
        self.used
    }

    /// True if nothing was handed out yet.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.used == 0
    }

    /// Slots still free.
    #[inline]
    pub fn remaining(&self) -> usize {
        self.capacity - self.used
    }
}

#[cfg(all(test, not(target_arch = "mips")))]
mod tests {
    use super::*;
    use crate::prim::{RectFlat, TriFlat};

    const END: u32 = 0x00FF_FFFF;

    fn addr<T>(value: &T) -> u32 {
        (value as *const T as usize as u32) & END
    }

    #[test]
    fn frame_links_packets_like_insert() {
        let mut storage: [TriFlat; 2] =
            core::array::from_fn(|_| TriFlat::new([(0, 0); 3], 0, 0, 0));
        let mut ot = OrderingTable::<4>::new();
        let mut frame = ot.frame();
        let mut arena = PrimitiveArena::new(&mut storage);
        let a = arena
            .push(TriFlat::new([(1, 2), (3, 4), (5, 6)], 1, 2, 3))
            .unwrap();
        let a_addr = addr(a);
        frame.add(2, a);
        let b = arena
            .push(TriFlat::new([(1, 2), (3, 4), (5, 6)], 4, 5, 6))
            .unwrap();
        let b_addr = addr(b);
        frame.add(2, b);
        assert!(arena.push(TriFlat::new([(0, 0); 3], 0, 0, 0)).is_none());
        assert_eq!((arena.len(), arena.remaining()), (2, 0));
        drop(frame);

        // Slot 2 now heads b, which links a, which links slot 1's entry.
        let walked: [(usize, u8); 2] = {
            // SAFETY: the table links only `storage`, alive for the test.
            let mut it = unsafe { ot.packets() };
            let first = it.next().unwrap();
            let second = it.next().unwrap();
            assert!(it.next().is_none());
            [
                (first.0 as usize & END as usize, first.1),
                (second.0 as usize & END as usize, second.1),
            ]
        };
        assert_eq!(
            walked,
            [
                (b_addr as usize, TriFlat::WORDS),
                (a_addr as usize, TriFlat::WORDS)
            ]
        );
        assert_eq!(storage[1].tag >> 24, TriFlat::WORDS as u32);
        assert_eq!(storage[1].tag & END, a_addr);
    }

    #[test]
    fn frame_clamps_depth_into_the_table() {
        let mut rect = RectFlat::new(0, 0, 4, 4, 9, 9, 9);
        let mut ot = OrderingTable::<4>::new();
        let mut frame = ot.frame();
        frame.add(99, &mut rect);
        drop(frame);
        // SAFETY: the table links only `rect`, alive for the test.
        let first = unsafe { ot.packets() }.next().unwrap();
        assert_eq!(first.0 as usize & END as usize, addr(&rect) as usize);
    }

    #[test]
    fn link_after_continues_into_the_next_table() {
        let mut first = OrderingTable::<4>::new();
        let mut second = OrderingTable::<2>::new();
        let mut frame = first.frame();
        let next = second.frame();
        let next_head = next.ot.submit_head() as usize as u32 & END;
        frame.link_after(next);
        drop(frame);
        // Slot 0, walked last, now links the second table's head.
        // SAFETY: `submit_head` is entry 3 of a 4-entry table; entry 0 is
        // three words below it, inside the same array.
        let slot0 = unsafe { first.submit_head().sub(3).read() };
        assert_eq!(slot0, next_head);
    }

    #[test]
    #[should_panic(expected = "empty slot 0")]
    fn link_after_refuses_a_used_slot_zero() {
        let mut rect = RectFlat::new(0, 0, 4, 4, 9, 9, 9);
        let mut first = OrderingTable::<4>::new();
        let mut second = OrderingTable::<2>::new();
        let mut frame = first.frame();
        frame.add(0, &mut rect);
        frame.link_after(second.frame());
    }

    #[test]
    fn frame_storage_builds_into_its_own_packets() {
        let mut storage = FrameStorage::<4, [RectFlat; 2]>::new(core::array::from_fn(|_| {
            RectFlat::new(0, 0, 0, 0, 0, 0, 0)
        }));
        let count = storage.build(|frame, packets| {
            let mut arena = PrimitiveArena::new(packets);
            for z in 0..3 {
                match arena.push(RectFlat::new(z, z, 2, 2, 1, 1, 1)) {
                    Some(rect) => frame.add(z as usize, rect),
                    None => break,
                }
            }
            arena.len()
        });
        assert_eq!(count, 2);
        // SAFETY: the table links only the storage's own packets.
        let walked = unsafe { storage.ot.packets() }.count();
        assert_eq!(walked, 2);
        assert_eq!(storage.packets_mut()[1].tag >> 24, RectFlat::WORDS as u32);
    }

    #[test]
    fn resume_frame_keeps_the_links_already_made() {
        let mut first = RectFlat::new(0, 0, 4, 4, 1, 1, 1);
        let mut second = RectFlat::new(0, 0, 4, 4, 2, 2, 2);
        let mut ot = OrderingTable::<4>::new();
        let mut frame = ot.frame();
        frame.add(1, &mut first);
        drop(frame);
        // SAFETY: `first` and `second` outlive every use of the table, and
        // nothing walks it.
        let mut frame = unsafe { ot.resume_frame() };
        frame.add(2, &mut second);
        drop(frame);
        let walked: [usize; 2] = {
            // SAFETY: the table links only `first` and `second`, alive here.
            let mut it = unsafe { ot.packets() };
            let order = [it.next().unwrap().0 as usize, it.next().unwrap().0 as usize];
            assert!(it.next().is_none());
            order
        };
        assert_eq!(
            walked.map(|a| a as u32 & END),
            [addr(&second), addr(&first)]
        );
    }

    #[test]
    fn unchecked_adds_link_like_add_raw() {
        let mut checked = OrderingTable::<4>::new();
        let mut unchecked = OrderingTable::<4>::new();
        let mut a = [0u32; 3];
        let mut b = [0u32; 3];
        let mut c = [0u32; 3];
        let mut d = [0u32; 3];
        {
            let mut frame = checked.frame();
            // SAFETY: the arrays outlive every use of the tables in this
            // test and hold a tag plus two words.
            unsafe {
                frame.add_raw(2, a.as_mut_ptr(), 2);
                frame.add_raw(2, b.as_mut_ptr(), 2);
            }
        }
        {
            let mut frame = unchecked.frame();
            // SAFETY: as above; slot 2 is below 4 and two words fit a node.
            unsafe {
                frame.add_raw_unchecked(2, c.as_mut_ptr(), 2);
                frame.add_raw_tag_high_unchecked(2, d.as_mut_ptr(), 2 << 24);
            }
        }
        assert_eq!(a[0] >> 24, c[0] >> 24);
        assert_eq!(b[0] >> 24, d[0] >> 24);
        assert_eq!(b[0] & END, a.as_ptr() as usize as u32 & END);
        assert_eq!(d[0] & END, c.as_ptr() as usize as u32 & END);
        // SAFETY: the tables link only the arrays above, alive here.
        let heads = unsafe {
            (
                checked.packets().next().unwrap().0,
                unchecked.packets().next().unwrap().0,
            )
        };
        assert_eq!(heads, (b.as_ptr(), d.as_ptr()));
    }

    #[test]
    fn packed_and_tagged_adds_forward_to_the_table() {
        let mut a = [0u32; 2];
        let mut b = [0u32; 2];
        let commands: [usize; 4] = [
            a.as_mut_ptr() as usize,
            3 | (1 << 24),
            b.as_mut_ptr() as usize,
            3 | (1 << 24),
        ];
        let mut stream = [(1u32 << 24) | 1, 0xAAAA_AAAA];
        let mut ot = OrderingTable::<4>::new();
        let mut frame = ot.frame();
        // SAFETY: the commands, packets and stream outlive every use of the
        // table; slots 3 and 1 fit it and every node is one word long.
        unsafe {
            frame.add_packed_commands_reverse_unchecked(commands.as_ptr(), 2);
            let first = stream.as_mut_ptr();
            frame.add_tagged_packet_stream_unchecked(first, first.add(stream.len()));
        }
        drop(frame);
        // SAFETY: the table links only `a`, `b` and `stream`, alive here.
        let walked: [*const u32; 3] = unsafe {
            let mut it = ot.packets();
            let order = [
                it.next().unwrap().0,
                it.next().unwrap().0,
                it.next().unwrap().0,
            ];
            assert!(it.next().is_none());
            order
        };
        assert_eq!(walked, [a.as_ptr(), b.as_ptr(), stream.as_ptr()]);
    }

    #[test]
    fn arena_hands_out_every_slot_once_then_refuses() {
        let mut storage: [RectFlat; 3] =
            core::array::from_fn(|_| RectFlat::new(0, 0, 0, 0, 0, 0, 0));
        let base = storage.as_ptr() as usize;
        let mut arena = PrimitiveArena::new(&mut storage);
        assert_eq!((arena.len(), arena.remaining()), (0, 3));
        let a = arena.push(RectFlat::new(1, 0, 1, 1, 1, 0, 0)).unwrap();
        let b = arena.push(RectFlat::new(2, 0, 1, 1, 2, 0, 0)).unwrap();
        let c = arena.push(RectFlat::new(3, 0, 1, 1, 3, 0, 0)).unwrap();
        assert!(arena.push(RectFlat::new(4, 0, 1, 1, 4, 0, 0)).is_none());
        assert_eq!((arena.len(), arena.remaining()), (3, 0));
        // Each slot is its own, in storage order: write through every borrow,
        // then read every one back.
        let size = core::mem::size_of::<RectFlat>();
        for (index, slot) in [&*a, &*b, &*c].iter().enumerate() {
            assert_eq!(*slot as *const RectFlat as usize, base + index * size);
        }
        a.tag = 0x11;
        b.tag = 0x22;
        c.tag = 0x33;
        assert_eq!([a.tag, b.tag, c.tag], [0x11, 0x22, 0x33]);
        assert_eq!(
            [storage[0].tag, storage[1].tag, storage[2].tag],
            [0x11, 0x22, 0x33]
        );
    }

    #[test]
    fn arena_over_empty_storage_refuses_the_first_push() {
        let mut storage: [RectFlat; 0] = [];
        let mut arena = PrimitiveArena::new(&mut storage);
        assert!(arena.push(RectFlat::new(0, 0, 0, 0, 0, 0, 0)).is_none());
        assert!(arena.is_empty());
        assert_eq!(arena.remaining(), 0);
    }
}
