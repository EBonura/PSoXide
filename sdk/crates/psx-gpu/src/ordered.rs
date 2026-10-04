//! Painter-order command streaming with bounded GPU DMA nodes.
//!
//! Each complete GP0 packet stays together in one node of at most
//! [`NODE_PAYLOAD_WORDS`] payload words after its DMA tag. Submitted storage
//! remains immutable until DMA completes. The backing slice is static, so
//! moving or forgetting the stream cannot invalidate an in-flight DMA
//! address. No allocation is used.

use core::marker::PhantomData;
use core::ptr::NonNull;
use psx_io::periph::GpuDma;

/// Most payload words the stream packs into one node, after its tag.
///
/// One word under [`crate::MAX_NODE_WORDS`], the node length the
/// hardware-tests v1.24 drawing case points to. The spare word is a margin,
/// not a second measured limit.
pub const NODE_PAYLOAD_WORDS: usize = 15;
const _: () = assert!(NODE_PAYLOAD_WORDS <= crate::MAX_NODE_WORDS);
const END: u32 = 0x00ff_ffff;

/// DMA operations required by an ordered stream.
///
/// The default implementation uses the SDK's GPU channel ownership and bounded
/// waits. Alternate implementations can test the transport without MMIO.
///
/// # Safety
///
/// [`OrderedCommandStream`] is a safe API that rewrites node storage as soon
/// as this transport says the hardware is done with it, so an implementation
/// must tell the truth:
///
/// * after [`Self::wait`] returns, nothing reads storage passed to an earlier
///   [`Self::submit`] (a timed-out transfer must be stopped, not abandoned);
/// * [`Self::is_busy`] returns `true` while a submitted list may still be read,
///   because a `false` lets the stream start the next list at once;
/// * [`Self::submit`] reads only the linked nodes it was given.
pub unsafe trait CommandStreamDma {
    /// Whether the previous GPU DMA transfer is still reading its storage.
    fn is_busy(&mut self) -> bool;
    /// Start a valid linked list after the channel becomes idle.
    ///
    /// # Safety
    /// `head` and every linked node must remain valid and immutable until
    /// `wait` returns. Tags must describe aligned RAM addresses and lengths.
    unsafe fn submit(&mut self, head: *const u32);
    /// Wait until the DMA engine no longer reads the submitted storage.
    fn wait(&mut self);
    /// Wait until previously submitted GPU drawing has finished.
    #[doc(alias = "DrawSync")]
    fn wait_idle(&mut self);

    /// Renamed to [`is_busy`](Self::is_busy).
    #[deprecated(note = "renamed to `is_busy`")]
    #[inline(always)]
    fn busy(&mut self) -> bool {
        self.is_busy()
    }

    /// Renamed to [`wait_idle`](Self::wait_idle).
    #[deprecated(note = "renamed to `wait_idle`")]
    #[inline(always)]
    fn draw_sync(&mut self) {
        self.wait_idle()
    }
}

// SAFETY: the token is the channel-2 transport. `is_busy` reads the
// channel's CHCR start bit, which stays set until the walk ends. `wait`
// returns only after that bit clears or after it aborts the channel (and
// resets the GPU) on timeout, so no walk outlives it.
unsafe impl CommandStreamDma for GpuDma {
    #[inline]
    fn is_busy(&mut self) -> bool {
        psx_io::dma::is_busy(psx_io::dma::Channel::Gpu)
    }
    #[inline]
    unsafe fn submit(&mut self, head: *const u32) {
        // SAFETY: forwarded from this method's own contract.
        unsafe { crate::submit_linked_list_async_raw(head) };
    }
    #[inline]
    fn wait(&mut self) {
        crate::submit_linked_list_wait();
    }
    #[inline]
    fn wait_idle(&mut self) {
        crate::wait_idle_impl();
    }
}

/// Forward, incremental GPU command list over caller-owned static storage.
///
/// Append complete packets with [`Self::push_packet`], then [`Self::submit`]
/// to overlap CPU work with GPU drawing. Call [`Self::flush`] before
/// immediate GP0 drawing, VRAM uploads, or framebuffer presentation. Capacity
/// exhaustion performs that same synchronization before reusing storage.
/// Dropping the stream waits for in-flight DMA and discards unsent commands.
///
/// To present through psx-rt's queued flip, arm the draw-done flag
/// ([`crate::Gpu::arm_draw_done`] on the transport [`Self::flush`] returns)
/// before the frame's first packet (nodes can start walking as soon as they
/// close) and end the frame with `push_packet([gp0::REQUEST_IRQ])` and
/// [`Self::submit`]; see [`crate::is_draw_done`].
pub struct OrderedCommandStream<D: CommandStreamDma = GpuDma> {
    // One base pointer for the whole `'static` buffer rather than the
    // `&mut [u32]` itself: every access goes through it, so writing the open
    // node never reborrows the nodes a walk is still reading.
    words: NonNull<u32>,
    capacity: usize,
    _words: PhantomData<&'static mut [u32]>,
    len: usize,
    head: usize,
    sent: usize,
    submitted: bool,
    dma: D,
}

// SAFETY: the stream owns its `'static` buffer exclusively, as the
// `&'static mut [u32]` it was built from did, so moving it to another
// context is as safe as moving that reference and the transport.
unsafe impl<D: CommandStreamDma + Send> Send for OrderedCommandStream<D> {}
// SAFETY: `&self` methods read no buffer word, so shared references are as
// safe as for the `&'static mut [u32]` it replaces.
unsafe impl<D: CommandStreamDma + Sync> Sync for OrderedCommandStream<D> {}

impl OrderedCommandStream {
    /// A stream over `words` that drives channel 2 without holding its
    /// token.
    #[deprecated(
        note = "use `OrderedCommandStream::with_dma(words, gpu_dma)` with the `GpuDma` token"
    )]
    pub fn new(words: &'static mut [u32]) -> Self {
        // SAFETY: a token is a logic guard, not a memory-safety one (see
        // `psx_io::periph`); this constructor has always driven channel 2
        // without one, and keeps doing so for one stage.
        Self::with_dma(words, unsafe { GpuDma::steal() })
    }
}

impl<D: CommandStreamDma> OrderedCommandStream<D> {
    /// A stream over `words`, at least 17 word-aligned RAM words, driven by
    /// `dma`: the `GpuDma` token on the console, a test double on the host.
    pub fn with_dma(words: &'static mut [u32], dma: D) -> Self {
        assert!(
            words.len() >= NODE_PAYLOAD_WORDS + 2,
            "ordered stream needs a packet, tag, and spare tag"
        );
        words[0] = END;
        Self {
            capacity: words.len(),
            words: NonNull::from(words).cast(),
            _words: PhantomData,
            len: 1,
            head: 0,
            sent: 0,
            submitted: false,
            dma,
        }
    }

    /// Word `index` of the buffer.
    ///
    /// # Panics
    ///
    /// If `index` is past the buffer, as slice indexing would.
    #[inline(always)]
    fn word_mut(&mut self, index: usize) -> &mut u32 {
        assert!(index < self.capacity);
        // SAFETY: `index` is inside the buffer this stream owns for `'static`;
        // the borrow covers this one word, which no walk is reading (callers
        // only touch the open node and words past it).
        unsafe { &mut *self.words.as_ptr().add(index) }
    }

    #[inline]
    fn open_node(&mut self) {
        self.head = self.len;
        *self.word_mut(self.head) = END;
        self.len += 1;
    }

    #[inline]
    fn close_node(&mut self, next: Option<usize>) {
        let payload = (self.len - self.head - 1) as u32;
        let link = next.map_or(END, |i| {
            self.words.as_ptr().wrapping_add(i).expose_provenance() as u32 & END
        });
        // Finish the tag before checking the channel; the shared submit helper
        // supplies the compiler release barrier before DMA starts.
        // SAFETY: `head` indexes a tag word written by `open_node`, so it is
        // below `len` and inside `words`.
        unsafe {
            core::ptr::write_volatile(self.words.as_ptr().add(self.head), payload << 24 | link);
        }
        if !self.dma.is_busy() {
            self.kick_pending();
        }
    }

    fn kick_pending(&mut self) {
        if self.sent > self.head {
            return;
        }
        let tag = self.word_mut(self.head);
        *tag = *tag & 0xff00_0000 | END;
        // Only closed nodes are visible. Future appends start beyond len,
        // never in the region now owned by DMA.
        // SAFETY: `sent..len` holds closed nodes whose tags link only inside
        // `words`, which is `'static`. The stream writes none of them again
        // until `dma.wait()` returns (flush and drop both wait first).
        unsafe {
            self.dma.submit(self.words.as_ptr().add(self.sent));
        }
        self.sent = self.len;
        self.submitted = true;
    }

    #[inline(always)]
    fn reserve(&mut self, count: usize) {
        assert!(
            count > 0 && count <= NODE_PAYLOAD_WORDS,
            "ordered GP0 packet must contain 1..=15 words"
        );
        // Reserve the next tag too, even if this packet fits the current node.
        // Otherwise an exactly full arena makes submit's open_node overflow.
        if self.len + count + 1 > self.capacity {
            self.reuse_full_buffer();
        }
        if self.len - self.head - 1 + count > NODE_PAYLOAD_WORDS {
            if self.len + count + 2 > self.capacity {
                self.reuse_full_buffer();
            } else {
                self.close_node(Some(self.len));
                self.open_node();
            }
        }
    }

    // Capacity exhaustion is rare for normal frame-sized storage. Keep its
    // complete DMA/GPU drain out of every inlined packet emitter, where it
    // otherwise increases live values and stack spills on the R3000.
    #[cold]
    #[inline(never)]
    fn reuse_full_buffer(&mut self) {
        self.flush();
    }

    /// Append one complete GP0 packet in painter order.
    ///
    /// `N` must be 1..=15. Packets are never split across DMA nodes.
    #[inline(always)]
    pub fn push_packet<const N: usize>(&mut self, words: [u32; N]) {
        self.reserve(N);
        let mut len = self.len;
        for word in words {
            // reserve(N) checked the whole packet plus a spare tag, including
            // any capacity-driven reset. This index is therefore in bounds;
            // repeating a slice check per GP0 word bloats hot draw loops.
            // SAFETY: as above, `len < self.capacity` for every word of the
            // packet.
            unsafe {
                self.words.as_ptr().add(len).write(word);
            }
            len += 1;
        }
        self.len = len;
    }

    /// Append a small A0 upload between surrounding draw commands.
    ///
    /// Coordinates and dimensions use VRAM halfwords. The rectangle must
    /// match `pixels` and fit one node (at most 24 pixels). Odd pixel counts
    /// have a zero-padded final halfword. Large asset uploads should use the
    /// VRAM upload helpers after [`Self::flush`].
    pub fn push_upload(&mut self, x: u16, y: u16, width: u16, height: u16, pixels: &[u16]) {
        assert!(width > 0 && height > 0);
        assert_eq!(pixels.len(), usize::from(width) * usize::from(height));
        let count = 3 + pixels.len().div_ceil(2);
        self.reserve(count);
        *self.word_mut(self.len) = 0xa000_0000;
        *self.word_mut(self.len + 1) = (u32::from(y) << 16) | u32::from(x);
        *self.word_mut(self.len + 2) = (u32::from(height) << 16) | u32::from(width);
        self.len += 3;
        for pair in pixels.chunks(2) {
            *self.word_mut(self.len) =
                u32::from(pair[0]) | (u32::from(*pair.get(1).unwrap_or(&0)) << 16);
            self.len += 1;
        }
    }

    /// Submit all pending commands, retaining storage until completion.
    pub fn submit(&mut self) {
        if self.len <= self.head + 1 {
            return;
        }
        self.close_node(None);
        if self.sent <= self.head {
            self.dma.wait();
            self.kick_pending();
        }
        self.open_node();
    }

    /// Submit, wait for DMA and GPU, and reset the buffer for reuse.
    ///
    /// Returns the transport, idle, for work that must not overlap the
    /// stream: `Gpu::from_dma_mut(stream.flush())` draws immediately or
    /// arms the draw-done flag before the next frame's first packet.
    #[doc(alias = "DrawSync")]
    pub fn flush(&mut self) -> &mut D {
        self.submit();
        if self.submitted {
            self.dma.wait();
            self.submitted = false;
        }
        self.len = 1;
        self.head = 0;
        self.sent = 0;
        *self.word_mut(0) = END;
        self.dma.wait_idle();
        &mut self.dma
    }

    /// Flush, then hand back the buffer and the transport.
    pub fn release(self) -> (&'static mut [u32], D) {
        let mut this = core::mem::ManuallyDrop::new(self);
        this.flush();
        // SAFETY: `this` is never dropped, so the transport is read out of
        // it exactly once; the buffer is the `&'static mut [u32]` the stream
        // was built from, rebuilt from its base and length now that nothing
        // walks it.
        unsafe {
            (
                core::slice::from_raw_parts_mut(this.words.as_ptr(), this.capacity),
                core::ptr::read(&this.dma),
            )
        }
    }

    /// Renamed to [`flush`](Self::flush).
    #[deprecated(note = "renamed to `flush`")]
    #[inline(always)]
    pub fn draw_sync(&mut self) {
        self.flush();
    }
}

impl<D: CommandStreamDma> Drop for OrderedCommandStream<D> {
    fn drop(&mut self) {
        if self.submitted {
            self.dma.wait();
        }
    }
}
