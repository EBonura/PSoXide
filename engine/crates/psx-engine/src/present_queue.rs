//! The runner's side of [`RenderSubmission::PresentQueue`]: the frame a scene
//! built, with its recorded overlay, handed to `psx_rt::present::Presenter`
//! as one chain.
//!
//! [`RenderSubmission::PresentQueue`]: crate::scene::RenderSubmission::PresentQueue

use psx_gpu as gpu;
use psx_rt::present::{Part, Presenter};

use crate::scene::{Ctx, QueuedFrame, Scene};
use crate::telemetry;

/// The one tag-only node every frame's ordering table ends on; the runner
/// links it to the overlay of the frame it submits last, or straight to
/// GP0(1Fh).
///
/// It is one node, not one for each frame of the pair, because a scene may keep
/// a chain of its own that every frame's table walks into (the stance HUD's
/// packets are linked into slot 0 of each table and reused): the last packet of
/// such a chain links on to whatever the table ended on, so with a hook for each
/// frame the walk of a frame still being drawn followed the link written for the
/// frame being built, into an overlay that frame's successor had already
/// overwritten. The hook always names the overlay of the newest submitted frame,
/// which stays intact until the frame after the one that follows it is built,
/// and the walk of the frame before is over by then
/// (`psx_rt::present::wait_arena_free`): a walk that reaches the hook after the
/// next frame was submitted draws that frame's overlay, which is whole, instead
/// of its own.
static mut PRESENT_HOOK: u32 = 0x00FF_FFFF;

/// The node a present-queue scene ends its ordering table on.
pub(crate) fn hook() -> *const u32 {
    // Only the address of the static is taken; no reference is formed.
    core::ptr::addr_of!(PRESENT_HOOK)
}

/// Point the hook node at `next`, the overlay of the frame being submitted.
fn link_hook(next: *const u32) {
    // SAFETY: a volatile word store into the runner-owned static, through a raw
    // pointer: one aligned word, which a walk reads whole, and `next` names
    // memory that stays intact until the frame after the one being submitted is
    // built (see `PRESENT_HOOK`).
    unsafe {
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!(PRESENT_HOOK),
            next as u32 & 0x00FF_FFFF,
        );
    }
}

/// Record the overlay behind the frame `scene` just built and submit it.
/// `false` when the scene declined or its overlay did not fit: nothing was
/// submitted, and the caller presents the frame through
/// [`Scene::submit_render`].
pub(crate) fn submit<S: Scene>(presenter: &mut Presenter, scene: &mut S, ctx: &mut Ctx) -> bool {
    let draw_done = gpu::chain::DRAW_DONE_NODE.as_ptr();
    let Some(QueuedFrame {
        head,
        overlay,
        overlay_words,
    }) = scene.take_queued_frame(ctx)
    else {
        link_hook(draw_done);
        return false;
    };

    telemetry::stage_begin(telemetry::stage::RENDER);
    // SAFETY: `take_queued_frame`'s contract keeps `overlay` live and
    // unmodified until the frame after next starts rendering, which is after
    // this frame's walk (`wait_arena_free`).
    let recording = unsafe { psx_io::gpu::start_recording_raw(overlay, overlay_words) };
    scene.render_overlay(ctx);
    let recorded = recording.end();
    telemetry::stage_end(telemetry::stage::RENDER);
    match recorded {
        Ok(Some(recording)) => {
            // SAFETY: the recording has not been submitted yet.
            unsafe { recording.link_to(draw_done) };
            link_hook(recording.head());
        }
        Ok(None) => link_hook(draw_done),
        Err(_) => {
            link_hook(draw_done);
            return false;
        }
    }

    telemetry::stage_begin(telemetry::stage::PRESENT);
    // SAFETY: the chain is the scene's table and packets, the hook, the overlay
    // and the static GP0(1Fh) node, all of which stay untouched until
    // `wait_arena_free` frees this side two frames from now; the presenter
    // links the draw target and clear in front.
    unsafe { presenter.submit(&mut ctx.fb, head, Part::Final) };
    telemetry::stage_end(telemetry::stage::PRESENT);
    true
}
