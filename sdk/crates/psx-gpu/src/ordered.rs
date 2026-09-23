//! Painter-order command streaming with bounded GPU DMA nodes.
//!
//! Each complete GP0 packet stays together in a node of at most 15 payload
//! words (16 including its DMA tag). Submitted storage remains immutable until
//! DMA completes. The backing slice is static so moving or forgetting the
//! stream cannot invalidate an in-flight DMA address. No allocation is used.

/// Conservative merged primitive payload limit, excluding the DMA tag.
pub const NODE_PAYLOAD_WORDS: usize = 15;
const _: () = assert!(NODE_PAYLOAD_WORDS <= crate::MAX_NODE_WORDS);
const END: u32 = 0x00ff_ffff;

/// DMA operations required by an ordered stream.
///
/// The default implementation uses the SDK's GPU channel ownership and bounded
/// waits. Alternate implementations can test the transport without MMIO.
pub trait CommandStreamDma {
    /// Whether the previous GPU DMA transfer is still reading its storage.
    fn busy(&mut self) -> bool;
    /// Start a valid linked list after the channel becomes idle.
    ///
    /// # Safety
    /// `head` and every linked node must remain valid and immutable until
    /// `wait` returns. Tags must describe aligned RAM addresses and lengths.
    unsafe fn submit(&mut self, head: *const u32);
    /// Wait until the DMA engine no longer reads the submitted storage.
    fn wait(&mut self);
    /// Wait until previously submitted GPU drawing has finished.
    fn draw_sync(&mut self);
}

/// SDK channel-2 transport for [`OrderedCommandStream`].
pub struct GpuDma;
impl CommandStreamDma for GpuDma {
    #[inline]
    fn busy(&mut self) -> bool {
        psx_io::dma::is_busy(psx_io::dma::Channel::Gpu)
    }
    #[inline]
    unsafe fn submit(&mut self, head: *const u32) {
        crate::submit_linked_list_async(head);
    }
    #[inline]
    fn wait(&mut self) {
        crate::submit_linked_list_wait();
    }
    #[inline]
    fn draw_sync(&mut self) {
        crate::draw_sync();
    }
}

/// Forward, incremental GPU command list over caller-owned static storage.
///
/// Append complete packets with [`Self::push_packet`], then [`Self::submit`]
/// to overlap CPU work with GPU drawing. Call [`Self::draw_sync`] before
/// immediate GP0 drawing, VRAM uploads, or framebuffer presentation. Capacity
/// exhaustion performs that same synchronization before reusing storage.
/// Dropping the stream waits for in-flight DMA and discards unsent commands.
///
/// To present through psx-rt's queued flip, call [`crate::arm_draw_done`]
/// before the frame's first packet (nodes can start walking as soon as they
/// close) and end the frame with `push_packet([gp0::REQUEST_IRQ])` and
/// [`Self::submit`]; see [`crate::draw_done`].
pub struct OrderedCommandStream<D: CommandStreamDma = GpuDma> {
    words: &'static mut [u32],
    len: usize,
    head: usize,
    sent: usize,
    submitted: bool,
    dma: D,
}

impl OrderedCommandStream {
    /// Use a static, word-aligned RAM buffer of at least 17 words.
    pub fn new(words: &'static mut [u32]) -> Self {
        Self::with_dma(words, GpuDma)
    }
}

impl<D: CommandStreamDma> OrderedCommandStream<D> {
    /// Construct a stream with an explicit DMA transport.
    pub fn with_dma(words: &'static mut [u32], dma: D) -> Self {
        assert!(
            words.len() >= NODE_PAYLOAD_WORDS + 2,
            "ordered stream needs a packet, tag, and spare tag"
        );
        words[0] = END;
        Self {
            words,
            len: 1,
            head: 0,
            sent: 0,
            submitted: false,
            dma,
        }
    }

    #[inline]
    fn open_node(&mut self) {
        self.head = self.len;
        self.words[self.head] = END;
        self.len += 1;
    }

    #[inline]
    fn close_node(&mut self, next: Option<usize>) {
        let payload = (self.len - self.head - 1) as u32;
        let link = next.map_or(END, |i| self.words.as_ptr().wrapping_add(i) as u32 & END);
        // Finish the tag before checking the channel; the shared submit helper
        // supplies the compiler release barrier before DMA starts.
        unsafe {
            core::ptr::write_volatile(self.words.as_mut_ptr().add(self.head), payload << 24 | link);
        }
        if !self.dma.busy() {
            self.kick_pending();
        }
    }

    fn kick_pending(&mut self) {
        if self.sent > self.head {
            return;
        }
        self.words[self.head] = self.words[self.head] & 0xff00_0000 | END;
        // Only closed nodes are visible. Future appends start beyond len,
        // never in the region now owned by DMA.
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
        if self.len + count + 1 > self.words.len() {
            self.reuse_full_buffer();
        }
        if self.len - self.head - 1 + count > NODE_PAYLOAD_WORDS {
            if self.len + count + 2 > self.words.len() {
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
        self.draw_sync();
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
            unsafe {
                *self.words.get_unchecked_mut(len) = word;
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
    /// VRAM upload helpers after [`Self::draw_sync`].
    pub fn push_upload(&mut self, x: u16, y: u16, width: u16, height: u16, pixels: &[u16]) {
        assert!(width > 0 && height > 0);
        assert_eq!(pixels.len(), usize::from(width) * usize::from(height));
        let count = 3 + pixels.len().div_ceil(2);
        self.reserve(count);
        self.words[self.len] = 0xa000_0000;
        self.words[self.len + 1] = (u32::from(y) << 16) | u32::from(x);
        self.words[self.len + 2] = (u32::from(height) << 16) | u32::from(width);
        self.len += 3;
        for pair in pixels.chunks(2) {
            self.words[self.len] =
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
    pub fn draw_sync(&mut self) {
        self.submit();
        if self.submitted {
            self.dma.wait();
            self.submitted = false;
        }
        self.len = 1;
        self.head = 0;
        self.sent = 0;
        self.words[0] = END;
        self.dma.draw_sync();
    }
}

impl<D: CommandStreamDma> Drop for OrderedCommandStream<D> {
    fn drop(&mut self) {
        if self.submitted {
            self.dma.wait();
        }
    }
}
