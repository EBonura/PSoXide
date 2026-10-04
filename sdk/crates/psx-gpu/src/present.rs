//! [`PresentPair`]: psx-rt's VBlank-kicked present queue behind a safe API.
//!
//! The queue (`psx_rt::present`) takes a finished frame's DMA chain and
//! kicks it from the VBlank handler once the frame before it has drawn, so
//! the CPU builds frame N+1 while the GPU draws frame N and flips land only
//! on blank edges. Its raw entry point, `publish_raw`, is `unsafe`: the
//! chain must stay untouched until a later wait says its walk is over.
//! `PresentPair` owns the two `'static` storages the frames alternate
//! between and runs the queue's protocol itself, so the borrow checker
//! proves that contract:
//!
//! - a storage is rebuilt only after `wait_arena_free`, which returns once
//!   no walk can still read it;
//! - each frame starts with its own draw area, offset and clear (the
//!   handler writes only GP1) and ends on GP0(1Fh), which the queue flips on;
//! - the pair owns the [`Gpu`], and at most one pair exists, since the queue
//!   is a single slot.
//!
//! Build with the `present-queue` feature, and install psx-rt's VBlank
//! counter (`psx_rt::interrupts::install_vblank_counter`) before
//! [`PresentPair::start`].

use crate::chain::LIST_END;
use crate::display::DoubleBuffer;
use crate::frame::{FrameStorage, OtFrame};
use crate::prim::FillRect;
use crate::Gpu;
use core::cell::Cell;
use psx_rt::critical_section::{self, Mutex};
use psx_rt::present;

/// Payload words of a frame's preamble: draw area, draw offset, clear.
const PREAMBLE_WORDS: usize = 6;

/// The node each published frame starts with: GP0(E3h), GP0(E4h),
/// GP0(E5h) for its buffer, then a GP0(02h) fill, linked on to the table.
#[derive(Debug)]
#[repr(C, align(4))]
struct Preamble {
    tag: u32,
    words: [u32; PREAMBLE_WORDS],
}

/// One frame's worth of storage for a [`PresentPair`]: an ordering table,
/// the caller's packet storage `S`, and the frame's preamble node.
#[derive(Debug)]
pub struct PresentStorage<const N: usize, S> {
    preamble: Preamble,
    frame: FrameStorage<N, S>,
}

impl<const N: usize, S> PresentStorage<N, S> {
    /// An empty table and `packets` as the packet storage.
    pub const fn new(packets: S) -> Self {
        Self {
            preamble: Preamble {
                tag: LIST_END,
                words: [0; PREAMBLE_WORDS],
            },
            frame: FrameStorage::new(packets),
        }
    }
}

/// Set while a [`PresentPair`] exists: the queue is one slot, and two pairs
/// publishing into it would each misjudge when the other's storage is free.
static ACTIVE: Mutex<Cell<bool>> = Mutex::new(Cell::new(false));

/// Two frames' storage presented through psx-rt's queue, alternately.
///
/// ```no_run
/// use psx_gpu::display::DoubleBuffer;
/// use psx_gpu::present::{PresentPair, PresentStorage};
/// use psx_gpu::prim::TriFlat;
/// use psx_gpu::Gpu;
///
/// static mut A: PresentStorage<8, [TriFlat; 4]> = PresentStorage::new([TriFlat::new([(0, 0); 3], 0, 0, 0); 4]);
/// static mut B: PresentStorage<8, [TriFlat; 4]> = PresentStorage::new([TriFlat::new([(0, 0); 3], 0, 0, 0); 4]);
///
/// fn run(gpu: Gpu, buffers: DoubleBuffer) {
///     // SAFETY: the only references ever made to A and B.
///     let (a, b) = unsafe { (&mut *(&raw mut A), &mut *(&raw mut B)) };
///     let mut pair = PresentPair::start(a, b, buffers, gpu);
///     loop {
///         pair.present((0, 0, 32), |frame, packets| {
///             packets[0] = TriFlat::new([(8, 8), (64, 8), (8, 64)], 255, 0, 0);
///             frame.add(1, &mut packets[0]);
///         });
///     }
/// }
/// ```
#[derive(Debug)]
pub struct PresentPair<const N: usize, S: 'static> {
    storage: [&'static mut PresentStorage<N, S>; 2],
    /// Index of the storage the next frame is built in.
    next: usize,
    buffers: DoubleBuffer,
    /// GP1(05h) word showing the frame published last; 0 before the first.
    shown: u32,
    gpu: Gpu,
}

impl<const N: usize, S> PresentPair<N, S> {
    /// Start the queue and present frames from `a` and `b` into `buffers`.
    ///
    /// Waits for the GPU to go idle first, as the queue requires.
    ///
    /// # Panics
    ///
    /// If another `PresentPair` exists (or one was dropped without
    /// [`release`](Self::release)): the queue has a single slot.
    pub fn start(
        a: &'static mut PresentStorage<N, S>,
        b: &'static mut PresentStorage<N, S>,
        buffers: DoubleBuffer,
        mut gpu: Gpu,
    ) -> Self {
        let first = critical_section::with(|cs| !ACTIVE.borrow(cs).replace(true));
        assert!(
            first,
            "one PresentPair at a time: the present queue is one slot"
        );
        gpu.wait_idle();
        present::start();
        Self {
            storage: [a, b],
            next: 0,
            buffers,
            shown: 0,
            gpu,
        }
    }

    /// Build the next frame with `build` and hand it to the queue.
    ///
    /// The frame clears its buffer to `clear` first and ends on GP0(1Fh);
    /// the queue kicks it at the first VBlank where the frame before it has
    /// drawn, and shows that earlier frame at the same edge. Returns once
    /// the frame is queued, waiting only when the CPU is a whole frame
    /// ahead. `build` gets the frame and the packet storage, as
    /// `FramePair::build` does.
    pub fn present<R>(
        &mut self,
        clear: (u8, u8, u8),
        build: impl for<'f> FnOnce(&mut OtFrame<'f, N>, &'f mut S) -> R,
    ) -> R {
        // This storage was published two frames ago; after this wait no walk
        // reads it.
        present::wait_arena_free();
        let storage = &mut *self.storage[self.next];
        let result = storage.frame.build(|frame, packets| {
            frame.end_with_draw_done();
            build(frame, packets)
        });
        storage.preamble.words = preamble_words(&self.buffers, clear);
        let table = storage.frame.submit_head().expose_provenance() as u32;
        storage.preamble.tag = ((PREAMBLE_WORDS as u32) << 24) | (table & LIST_END);
        let display = self.shown;
        self.shown = self.buffers.begin_deferred_swap();
        present::wait_slot_empty();
        let head = core::ptr::from_ref(&storage.preamble).cast::<u32>();
        // SAFETY: the slot is empty (waited above). The chain is this
        // storage's preamble, its table, packets the build closure added
        // (from the storage or `'static`, as its `for<'f>` bound allows) and
        // the static GP0(1Fh) node; every node is at most MAX_NODE_WORDS long
        // and lives in `'static` storage. The pair owns the storage, and
        // rebuilds it only after `wait_arena_free` two frames from now.
        unsafe { present::publish_raw(head, display) };
        self.next ^= 1;
        result
    }

    /// The GPU, for immediate drawing between frames. Its first port write
    /// after a publish waits until every queued frame has drawn
    /// (`psx_rt::present::wait_idle`), so it costs the overlap.
    pub fn gpu(&mut self) -> &mut Gpu {
        &mut self.gpu
    }

    /// Wait for every queued frame to draw, show the last one, and hand
    /// back the storages, the buffers and the GPU.
    pub fn release(mut self) -> ([&'static mut PresentStorage<N, S>; 2], DoubleBuffer, Gpu) {
        present::wait_idle();
        if self.shown != 0 {
            let origin = self.buffers.display_origin();
            self.gpu.set_display_start(origin);
        }
        critical_section::with(|cs| ACTIVE.borrow(cs).set(false));
        (self.storage, self.buffers, self.gpu)
    }
}

/// The preamble's payload: the draw target of `buffers`' draw side, then a
/// fill of it with `clear`, the words `DoubleBuffer::apply_draw_target` and
/// `DoubleBuffer::clear` write through the port.
fn preamble_words(buffers: &DoubleBuffer, clear: (u8, u8, u8)) -> [u32; PREAMBLE_WORDS] {
    let [area_top_left, area_bottom_right, offset] = buffers.draw_target_words();
    let fill = FillRect::new(buffers.draw_origin(), buffers.size(), clear);
    [
        area_top_left,
        area_bottom_right,
        offset,
        fill.color_command,
        fill.origin,
        fill.size,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::Resolution;
    use psx_hw::gpu::{gp0, pack_xy};

    #[test]
    fn the_preamble_targets_and_clears_the_draw_side() {
        let mut buffers = DoubleBuffer::with_stride(Resolution::R320X240, 256);
        let _ = buffers.begin_deferred_swap(); // now drawing the second buffer
        let words = preamble_words(&buffers, (1, 2, 3));
        assert_eq!(
            words,
            [
                gp0::draw_area_top_left(0, 256),
                gp0::draw_area_bottom_right(319, 256 + 239),
                gp0::draw_offset(0, 256),
                gp0::fill_rect(1, 2, 3),
                pack_xy(0, 256),
                pack_xy(320, 240),
            ]
        );
    }
}
