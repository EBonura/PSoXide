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

    /// Wait for a fresh VBlank IRQ edge, regardless of how many vblanks
    /// have already passed since the last present.
    ///
    /// [`wait_next_vblank`](Self::wait_next_vblank) returns immediately
    /// when any vblank elapsed since the previous present, which is right
    /// for pacing but wrong for the display flip: a flip issued mid-frame
    /// shears the picture on real hardware (GP1 display-start applies to
    /// the next scanline). This waits for the next actual edge so the
    /// swap always lands inside the vertical blanking interval.
    pub(crate) fn wait_vblank_edge(&mut self) {
        let entry = platform::vblank_count();
        self.last_present_vblank = platform::wait_present_vblank(entry);
    }

    /// Hand one GP1 display-start word to the VBlank handler and return
    /// immediately.
    ///
    /// The handler applies it at the next blank edge on which the GPU
    /// reports idle, so the flip still lands inside the vertical blanking
    /// interval exactly as [`wait_vblank_edge`](Self::wait_vblank_edge)
    /// plus a direct write would -- but the CPU is free to do the next
    /// frame's work in the meantime instead of spinning out the remainder
    /// of the display period.
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
    /// display periods, which can only happen if the GPU never reports idle
    /// at a blank edge. Continuing in that case shows one bad frame; not
    /// continuing would hang the game, and a wedged GPU is a state the
    /// console has been observed to reach.
    pub(crate) fn wait_display_flip(&mut self) -> bool {
        let entry = platform::vblank_count();
        while platform::display_flip_pending() {
            if platform::vblank_count().wrapping_sub(entry) > FLIP_WAIT_VBLANKS {
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
        psx_rt::interrupts::queue_gp1_at_vblank(display_start);
    }

    pub(super) fn display_flip_pending() -> bool {
        psx_rt::interrupts::gp1_queue_pending()
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
}
