//! The runner's side of [`RenderSubmission::PresentQueue`]: frames handed to
//! psx-rt's VBlank-kicked present queue (`psx_rt::present`).
//!
//! Built with the `present-queue` feature. Without it the runner presents a
//! `PresentQueue` scene the `QueuedDoubleBuffered` way, and this type is an
//! inert stand-in.
//!
//! [`RenderSubmission::PresentQueue`]: crate::scene::RenderSubmission::PresentQueue

#[cfg(feature = "present-queue")]
use psx_gpu as gpu;

use crate::app::Config;
#[cfg(feature = "present-queue")]
use crate::scene::QueuedFrame;
use crate::scene::{Ctx, Scene};
#[cfg(feature = "present-queue")]
use crate::telemetry;
use crate::time::EngineClock;

/// Words in one recorded preamble: the draw area and offset, the clear and a
/// node header, with room to spare.
#[cfg(feature = "present-queue")]
const PRESENT_PREAMBLE_WORDS: usize = 16;

/// Runner-owned nodes for the frames in the present queue, one per frame of
/// the pair (the frame before the last published one is never walked while
/// its successor builds, see `psx_rt::present::wait_arena_free`).
///
/// `PRESENT_HOOKS[i]` is a tag-only node a scene's ordering table ends on;
/// the runner links it to the recorded overlay, or straight to GP0(1Fh).
/// `PRESENT_PREAMBLES[i]` holds the recorded draw target and clear that
/// precede the table.
#[cfg(feature = "present-queue")]
static mut PRESENT_HOOKS: [u32; 2] = [0x00FF_FFFF; 2];
#[cfg(feature = "present-queue")]
static mut PRESENT_PREAMBLES: [[u32; PRESENT_PREAMBLE_WORDS]; 2] = [[0; PRESENT_PREAMBLE_WORDS]; 2];

/// The runner's present-queue state.
#[cfg_attr(not(feature = "present-queue"), allow(dead_code))]
pub(crate) struct PresentQueue {
    pub(crate) active: bool,
    /// GP1(05h) word that shows the last published frame, published with the
    /// next one; 0 before the first.
    display: u32,
    /// Which [`PRESENT_HOOKS`] / [`PRESENT_PREAMBLES`] entry the frame being
    /// built uses.
    frame: usize,
}

/// Whether this build hands `PresentQueue` frames to psx-rt; without the
/// feature the runner presents them as `QueuedDoubleBuffered`.
pub(crate) const ENABLED: bool = cfg!(feature = "present-queue");

impl PresentQueue {
    pub(crate) const fn new() -> Self {
        Self {
            active: false,
            display: 0,
            frame: 0,
        }
    }
}

#[cfg(feature = "present-queue")]
impl PresentQueue {
    /// Enter the queue. The GPU must be idle with nothing queued.
    pub(crate) fn start(&mut self) {
        psx_rt::present::start();
        *self = Self {
            active: true,
            display: 0,
            frame: 0,
        };
    }

    /// Wait until no walk reads the buffers the frame about to be built
    /// links: the frame before the last published one used them.
    pub(crate) fn wait_arena_free(&self) {
        psx_rt::present::wait_arena_free();
    }

    pub(crate) fn hook(&self) -> *const u32 {
        // SAFETY: only the address of the static is taken; no reference is
        // formed.
        unsafe { core::ptr::addr_of!(PRESENT_HOOKS[self.frame]) }
    }

    /// Point this frame's hook node at `next`.
    fn link_hook(&self, next: *const u32) {
        // SAFETY: a volatile word store into the runner-owned static, through
        // a raw pointer. Nothing walks this frame's hook while the frame is
        // being built (`wait_arena_free` ran first).
        unsafe {
            core::ptr::write_volatile(
                core::ptr::addr_of_mut!(PRESENT_HOOKS[self.frame]),
                next as u32 & 0x00FF_FFFF,
            );
        }
    }

    /// Record the overlay and preamble around the frame `scene` just built
    /// and publish it. `false` when the scene declined or its overlay did not
    /// fit: nothing was published, and the caller presents the frame through
    /// [`Scene::submit_render`] after [`stop`](Self::stop).
    pub(crate) fn publish<S: Scene>(
        &mut self,
        config: Config,
        scene: &mut S,
        ctx: &mut Ctx,
    ) -> bool {
        let draw_done = gpu::DRAW_DONE_NODE.as_ptr();
        let Some(QueuedFrame {
            head,
            overlay,
            overlay_words,
        }) = scene.take_queued_frame(ctx)
        else {
            self.link_hook(draw_done);
            return false;
        };

        telemetry::stage_begin(telemetry::stage::RENDER);
        // SAFETY: `take_queued_frame`'s contract keeps `overlay` live and
        // unmodified until the frame after next starts rendering, which is
        // after this frame's walk (`wait_arena_free`).
        let recording = unsafe { psx_io::gpu::start_recording_raw(overlay, overlay_words) };
        scene.render_overlay(ctx);
        let recorded = recording.end();
        telemetry::stage_end(telemetry::stage::RENDER);
        match recorded {
            Ok(Some(recording)) => {
                // SAFETY: the recording has not been published yet.
                unsafe { recording.link_to(draw_done) };
                self.link_hook(recording.head());
            }
            Ok(None) => self.link_hook(draw_done),
            Err(_) => {
                self.link_hook(draw_done);
                return false;
            }
        }

        telemetry::stage_begin(telemetry::stage::FRAME_CLEAR);
        // SAFETY: only the address of the runner-owned static is taken.
        let preamble = unsafe { core::ptr::addr_of_mut!(PRESENT_PREAMBLES[self.frame]) };
        // SAFETY: this frame's preamble is not walked again until the frame
        // after next (`wait_arena_free`), and nothing else references it.
        let recording =
            unsafe { psx_io::gpu::start_recording_raw(preamble.cast(), PRESENT_PREAMBLE_WORDS) };
        {
            let (gpu, fb) = ctx.gpu_and_buffers();
            fb.apply_draw_target(gpu);
            fb.clear(gpu, config.clear_color);
        }
        let Ok(Some(preamble)) = recording.end() else {
            unreachable!("the preamble always fits");
        };
        // SAFETY: the preamble has not been published yet.
        unsafe { preamble.link_to(head) };
        telemetry::stage_end(telemetry::stage::FRAME_CLEAR);

        telemetry::stage_begin(telemetry::stage::OT_WAIT);
        psx_rt::present::wait_slot_empty();
        telemetry::stage_end(telemetry::stage::OT_WAIT);
        telemetry::stage_begin(telemetry::stage::PRESENT);
        // SAFETY: the slot is empty. The chain is the preamble, the scene's
        // table and packets, the hook, the overlay and the static GP0(1Fh)
        // node, all of which stay untouched until `wait_arena_free` frees
        // this side two frames from now.
        unsafe { psx_rt::present::publish_raw(preamble.head(), self.display) };
        self.display = ctx.fb.begin_deferred_swap();
        self.frame ^= 1;
        telemetry::stage_end(telemetry::stage::PRESENT);
        true
    }

    /// Leave the queue: wait for the published frames to draw, put the last
    /// one on screen, and point the GPU at the buffer the next frame draws
    /// into, which is where the other presentation paths expect to start.
    pub(crate) fn stop(&mut self, clock: &mut EngineClock, ctx: &mut Ctx) {
        telemetry::stage_begin(telemetry::stage::PRESENT);
        // Waits only if anything was published since the last direct access.
        psx_io::gpu::run_direct_access_guard();
        if self.display != 0 {
            clock.queue_display_flip(ctx.gpu(), self.display);
            if !clock.wait_display_flip() {
                telemetry::counter(telemetry::counter::VISUAL_DEADLINE_MISSES, 1);
            }
        }
        {
            let (gpu, fb) = ctx.gpu_and_buffers();
            fb.apply_draw_target(gpu);
        }
        telemetry::stage_end(telemetry::stage::PRESENT);
        self.active = false;
        self.display = 0;
    }
}

/// Without the feature the runner never selects the queue (see [`ENABLED`]);
/// these keep its call sites compiling.
#[cfg(not(feature = "present-queue"))]
impl PresentQueue {
    pub(crate) fn start(&mut self) {}

    pub(crate) fn wait_arena_free(&self) {}

    pub(crate) fn hook(&self) -> *const u32 {
        core::ptr::null()
    }

    pub(crate) fn publish<S: Scene>(&mut self, _: Config, _: &mut S, _: &mut Ctx) -> bool {
        false
    }

    pub(crate) fn stop(&mut self, _: &mut EngineClock, _: &mut Ctx) {
        self.active = false;
    }
}
