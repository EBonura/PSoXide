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
}

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
}
