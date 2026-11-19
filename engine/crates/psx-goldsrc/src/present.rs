//! The present path and frame-loop hand-over both GoldSrc ports share.
//!
//! A frame's lists go out to DMA channel 2 as they are built; only its closing draw-done node goes
//! through the presenter, at the start of the next frame's build ([`FramePresent::hand_over`]), so
//! the whole CPU build overlaps what is left of the previous frame's raster. The next frame starts
//! with [`FramePresent::begin_frame`], which waits until that flip has landed and points the draw
//! area at the buffer to draw. The title and menus flip with [`title_flip`], the one entry that a
//! stack proof of overlay code can bound (no call through a register, no lazy VBlank install).

use psx_gpu::display::DoubleBuffer;
use psx_gpu::Gpu;
use psx_rt::present::{self, Part, Presenter};
use psx_tick::PresentCap;

/// Gameplay presents at 30 Hz.
pub const GAMEPLAY_CAP: PresentCap = PresentCap::HZ30;
/// The boot, menus, pause screens, loading and the title count their animations in frames, one
/// flip per VBlank, so they keep the display rate.
pub const MENU_CAP: PresentCap = PresentCap::UNCAPPED;

/// Present gameplay at [`GAMEPLAY_CAP`].
pub fn set_gameplay_cap() {
    present::set_cap(GAMEPLAY_CAP);
}

/// Present menus at [`MENU_CAP`].
pub fn set_menu_cap() {
    present::set_cap(MENU_CAP);
}

/// How many spins [`title_flip`] waits for a VBlank before it gives up.
pub const TITLE_VBLANK_SPINS: u32 = 4_000_000;

/// Show the buffer just drawn from title or menu code: wait one VBlank with a bound, then flip the
/// draw side and program the new draw target, without the wait on the present cap.
///
/// The title runs uncapped ([`MENU_CAP`]), so that wait would do nothing; it reaches the pacer
/// through a function pointer, which the stack proof of the cold title cannot bound.
/// `interrupts::wait_vblank` is not used either: it can install the VBlank counter lazily and
/// reach the cache flush. A VBlank that never arrives fails closed. Drawing must be finished first.
///
/// # Panics
///
/// When no VBlank arrives within [`TITLE_VBLANK_SPINS`] reads of the counter.
#[inline(never)]
pub fn title_flip(fb: &mut DoubleBuffer, gpu: &mut Gpu) {
    assert!(
        psx_rt::interrupts::try_wait_vblank(TITLE_VBLANK_SPINS),
        "title VBlank did not advance"
    );
    let display = fb.begin_deferred_swap();
    psx_io::gpu::write_display_control(display);
    fb.apply_draw_target(gpu);
}

/// The presenter and whether a built frame is waiting to be handed over.
#[derive(Debug)]
pub struct FramePresent {
    presenter: Presenter,
    pending: bool,
}

impl FramePresent {
    /// Take over presentation with no clear colour. The GPU must be idle.
    #[must_use]
    pub fn new() -> Self {
        Self {
            presenter: Presenter::new(None),
            pending: false,
        }
    }

    /// A frame has been built and submitted but not yet handed to the presenter.
    pub fn frame_built(&mut self) {
        self.pending = true;
    }

    /// Whether a frame waits to be handed over.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.pending
    }

    /// Hand the previous frame to the presenter as the frame's last part, if one is waiting: its
    /// chain is the draw-done node. The presenter links the draw area and offset in front,
    /// acknowledges GPUSTAT bit 24, kicks the walk without waiting for it and queues the display
    /// flip. Returns whether a frame was handed over.
    ///
    /// Every list of the frame must have gone out and been waited out (the node is the next thing
    /// channel 2 walks).
    pub fn hand_over(&mut self, fb: &mut DoubleBuffer) -> bool {
        if !self.pending {
            return false;
        }
        // SAFETY: the chain is the SDK's static draw-done node; the caller has waited out every
        // other list of the frame.
        unsafe {
            self.presenter
                .submit(fb, psx_gpu::chain::DRAW_DONE_NODE.as_ptr(), Part::Final);
        }
        self.pending = false;
        true
    }

    /// Start the next frame: wait until the flip has landed (a few loads at ride pace) and point
    /// the draw area at the buffer to draw. Never clear before this.
    pub fn begin_frame(&mut self, fb: &DoubleBuffer) {
        self.presenter.begin_frame(fb);
    }

    /// Land the last flip before code that flips the display directly (the ending screens).
    pub fn land(&mut self) {
        present::wait_idle();
    }
}

impl Default for FramePresent {
    fn default() -> Self {
        Self::new()
    }
}
