//! Engine display-clock access.
//!
//! Public scene code sees only two runtime counters through
//! [`crate::scene::Ctx`]: `sim_tick` and `visual_frame`. This module
//! keeps the platform VBlank counter private to the app runner.

pub(crate) struct EngineClock {
    origin_vblank: u32,
    last_present_vblank: u32,
}

impl EngineClock {
    pub(crate) fn new() -> Self {
        platform::init();
        let now = platform::vblank_count();
        Self {
            origin_vblank: now,
            last_present_vblank: now,
        }
    }

    pub(crate) fn elapsed_sim_ticks(&self) -> u32 {
        platform::vblank_count().wrapping_sub(self.origin_vblank)
    }

    pub(crate) fn reset_origin(&mut self) {
        self.align_origin_to_sim_tick(0);
    }

    pub(crate) fn align_origin_to_sim_tick(&mut self, sim_tick: u32) {
        let now = platform::vblank_count();
        self.origin_vblank = now.wrapping_sub(sim_tick);
        self.last_present_vblank = now;
    }

    pub(crate) fn wait_next_vblank(&mut self) {
        self.last_present_vblank = platform::wait_present_vblank(self.last_present_vblank);
    }

    /// Close the frame drawn so far with GP0(1Fh), hand one GP1
    /// display-start word to the VBlank handler and return immediately.
    ///
    /// psx-rt's handler applies the word at the first blank edge on which
    /// GPUSTAT bit 24 is set. GP0(1Fh) sets that bit when the GPU reaches it
    /// in its command stream, after everything sent before it is drawn, so
    /// every GP0 command issued before this call is on screen when the flip
    /// lands. GPUSTAT bit 28, the old gate, is not a drawing-complete test on
    /// silicon: it rises about one large primitive early (hardware-tests
    /// v1.24). The flag is acknowledged with GP1(02h) first; the caller must
    /// only queue once the previous flip has landed, or that acknowledge
    /// would hide the previous frame's completion from the handler.
    ///
    /// The flip lands inside the vertical blanking interval: a flip written
    /// mid-frame shears the picture on real hardware, since GP1 display
    /// start applies from the next scanline. The CPU is free to do other
    /// work until [`wait_display_flip`](Self::wait_display_flip).
    pub(crate) fn queue_display_flip(&mut self, display_start: u32) {
        platform::queue_display_flip(display_start);
    }

    /// `true` while a word passed to
    /// [`queue_display_flip`](Self::queue_display_flip) is still waiting for
    /// its blank edge. The caller must not draw into (or clear) the newly
    /// selected buffer until this goes false: until the flip lands, that
    /// buffer is the one on screen.
    pub(crate) fn display_flip_pending(&self) -> bool {
        platform::display_flip_pending()
    }

    /// Block until a queued flip has been applied.
    ///
    /// Returns `false` if it did not land within [`FLIP_WAIT_VBLANKS`]
    /// display periods, which can only happen if the GPU never reaches the
    /// frame's closing GP0(1Fh). The caller carries on either way: not continuing
    /// would hang the game, and a wedged GPU is a state the console has been
    /// observed to reach.
    ///
    /// On the giving-up path the word is applied here, at the next blank
    /// edge so it still does not tear, rather than left in the handler's
    /// slot. Abandoning it is not one bad frame, it is permanent:
    /// the next frame's [`Self::queue_display_flip`] overwrites the slot, so
    /// that display start never reaches the GPU while `FrameBuffer` has
    /// already moved its draw side on. From then on the runner clears and
    /// redraws the buffer the display is scanning out, and every overlay drawn
    /// after the world -- the HUD, message panels, damage numbers -- is wiped
    /// by the next frame's clear before it is ever on screen. Measured on the
    /// Cortex Ignition benchmark tape: 84% of frame clears landed on the
    /// visible buffer, and the HUD was absent from roughly a third of
    /// presented frames.
    pub(crate) fn wait_display_flip(&mut self) -> bool {
        let entry = platform::vblank_count();
        while platform::display_flip_pending() {
            if platform::vblank_count().wrapping_sub(entry) > FLIP_WAIT_VBLANKS {
                platform::wait_present_vblank(platform::vblank_count());
                platform::apply_pending_display_flip();
                self.last_present_vblank = platform::vblank_count();
                return false;
            }
        }
        self.last_present_vblank = platform::vblank_count();
        true
    }
}

/// Display periods a queued flip is given before the runner gives up on it.
const FLIP_WAIT_VBLANKS: u32 = 8;

#[cfg(target_arch = "mips")]
mod platform {
    pub(super) fn init() {
        psx_rt::interrupts::install_vblank_counter();
    }

    pub(super) fn vblank_count() -> u32 {
        psx_rt::interrupts::vblank_count()
    }

    pub(super) fn wait_present_vblank(last_present: u32) -> u32 {
        loop {
            let now = vblank_count();
            if now != last_present {
                return now;
            }
        }
    }

    pub(super) fn queue_display_flip(display_start: u32) {
        psx_gpu::arm_draw_done();
        psx_gpu::signal_draw_done();
        psx_rt::interrupts::queue_gp1_at_vblank(display_start);
    }

    pub(super) fn display_flip_pending() -> bool {
        psx_rt::interrupts::gp1_queue_pending()
    }

    /// Write a still-queued display start straight to GP1. Called just after
    /// a blank edge, so it lands in the blanking interval; keeping the
    /// display side in step with the draw side is worth showing a frame whose
    /// GP0(1Fh) never arrived.
    pub(super) fn apply_pending_display_flip() {
        let word = psx_rt::interrupts::take_pending_gp1();
        if word != 0 {
            psx_io::gpu::write_gp1(word);
        }
    }
}

#[cfg(not(target_arch = "mips"))]
mod platform {
    pub(super) fn init() {}

    pub(super) fn vblank_count() -> u32 {
        0
    }

    pub(super) fn wait_present_vblank(last_present: u32) -> u32 {
        last_present.wrapping_add(1)
    }

    /// Host: no IRQ exists to consume the queue, so a flip is applied the
    /// instant it is queued and never reads back as pending.
    pub(super) fn queue_display_flip(_display_start: u32) {}

    pub(super) fn display_flip_pending() -> bool {
        false
    }

    /// Host: the flip is applied the instant it is queued, so there is never
    /// anything left to force through.
    pub(super) fn apply_pending_display_flip() {}
}
