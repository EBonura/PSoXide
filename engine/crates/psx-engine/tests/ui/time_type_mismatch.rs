use psx_engine::{Angle, SimTick, VideoHz, VisualFrame};

fn needs_sim_tick(_: SimTick) {}

fn needs_visual_frame(_: VisualFrame) {}

fn needs_video_hz(_: VideoHz) {}

fn main() {
    let sim = SimTick::from_u32(1);
    let visual = VisualFrame::from_u32(1);

    needs_sim_tick(visual);
    needs_visual_frame(sim);
    needs_video_hz(60u16);

    let _ = Angle::per_frames(60).mul_tick(visual);
    let _ = Angle::per_frames(60).mul_frame(sim);
}
