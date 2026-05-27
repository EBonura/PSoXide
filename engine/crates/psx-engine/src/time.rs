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

    pub(crate) fn wait_next_vblank(&mut self) {
        self.last_present_vblank = platform::wait_present_vblank(self.last_present_vblank);
    }
}

#[cfg(target_arch = "mips")]
mod platform {
    use psx_gpu as gpu;

    pub(super) fn init() {
        gpu::configure_vsync_timer();
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
