//! Joint-selective pose composition shared by rendering and combat sockets.
/// A pose sample blended toward the primary pose on the joints a mask selects.
#[derive(Clone, Copy, Debug)]
pub struct MaskedPoseBlend<'a> {
    /// The pose blended in.
    pub sample: psx_asset::AnimationPoseSample<'a>,
    /// Blend weight toward `sample`, in Q12.
    pub alpha_q12: u16,
    /// All bits selects the legacy whole skeleton, including joints above 31.
    pub joint_mask: u32,
}
impl MaskedPoseBlend<'_> {
    /// Whether the mask selects `joint`.
    pub fn selects(&self, joint: u16) -> bool {
        self.joint_mask == u32::MAX || (joint < 32 && self.joint_mask & (1u32 << joint) != 0)
    }

    /// The blended pose for `joint`, or `primary` when the mask skips it.
    pub fn blend_toward(&self, primary: psx_asset::JointPose, joint: u16) -> psx_asset::JointPose {
        if !self.selects(joint) {
            return primary;
        }
        psx_asset::ModelPoseBlend {
            sample: self.sample,
            alpha_q12: self.alpha_q12,
        }
        .blend_toward(primary, joint)
    }
}

/// Every blend sampled on top of an actor's primary clip: the outgoing-clip
/// crossfade and a masked layer (a gait on the legs, the cannon on the upper
/// body). They compose: the crossfade runs on the body first and the layer then
/// blends toward that result on its own joints, so turning a layer on never
/// cuts a crossfade that is still running.
#[derive(Clone, Copy, Debug, Default)]
pub struct PoseBlends<'a> {
    /// Outgoing clip blended toward the primary clip.
    pub crossfade: Option<MaskedPoseBlend<'a>>,
    /// Masked layer blended toward the crossfaded body.
    pub layer: Option<MaskedPoseBlend<'a>>,
}

impl<'a> PoseBlends<'a> {
    /// Only a crossfade (no layer).
    pub const fn crossfade(crossfade: Option<MaskedPoseBlend<'a>>) -> Self {
        Self {
            crossfade,
            layer: None,
        }
    }

    /// The blends that still move a joint (a saturated weight shows the
    /// primary clip alone), or `None` when neither does.
    pub fn active(self) -> Option<Self> {
        let live = |blend: Option<MaskedPoseBlend<'a>>| blend.filter(|b| b.alpha_q12 < 1 << 12);
        let crossfade = live(self.crossfade);
        let layer = live(self.layer);
        if crossfade.is_none() && layer.is_none() {
            None
        } else {
            Some(Self { crossfade, layer })
        }
    }

    /// The composed pose for `joint`. A joint the layer replaces outright
    /// (weight 0 of the body there) skips the crossfade's decode.
    #[inline]
    pub fn blend_toward(&self, primary: psx_asset::JointPose, joint: u16) -> psx_asset::JointPose {
        // By reference: a blend is about 100 bytes, and copying one per
        // joint costs more than the blend.
        let layer = match &self.layer {
            Some(layer) if layer.selects(joint) => Some(layer),
            _ => None,
        };
        let body = match (&self.crossfade, layer) {
            (_, Some(layer)) if layer.alpha_q12 == 0 => primary,
            (Some(crossfade), _) => crossfade.blend_toward(primary, joint),
            (None, _) => primary,
        };
        match layer {
            Some(layer) => layer.blend_toward(body, joint),
            None => body,
        }
    }
}
