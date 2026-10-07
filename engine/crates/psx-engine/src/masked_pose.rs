//! Joint-selective pose composition shared by rendering and combat sockets.
#[derive(Clone, Copy, Debug)]
pub struct MaskedPoseBlend<'a> {
    pub sample: psx_asset::AnimationPoseSample<'a>,
    pub alpha_q12: u16,
    /// All bits selects the legacy whole skeleton, including joints above 31.
    pub joint_mask: u32,
}
impl MaskedPoseBlend<'_> {
    pub fn blend_toward(&self, primary: psx_asset::JointPose, joint: u16) -> psx_asset::JointPose {
        if self.joint_mask != u32::MAX && (joint >= 32 || self.joint_mask & (1u32 << joint) == 0) {
            return primary;
        }
        psx_asset::ModelPoseBlend {
            sample: self.sample,
            alpha_q12: self.alpha_q12,
        }
        .blend_toward(primary, joint)
    }
}
