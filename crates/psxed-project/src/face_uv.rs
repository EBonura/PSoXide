//! Per-face texture placement shared by brush faces and grid surfaces.

use serde::{Deserialize, Serialize};

/// Per-face texture placement over the paraxial projection:
/// `uv' = rotate(rotation_deg, uv * 256 / scale_q8) + offset_texels`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaceUv {
    /// Texel offset added after rotation/scale.
    pub offset_texels: [i16; 2],
    /// Free rotation in degrees.
    pub rotation_deg: i16,
    /// Per-axis scale, Q8 (256 = 1.0). Larger = texture appears bigger.
    pub scale_q8: [i16; 2],
}

impl Default for FaceUv {
    fn default() -> Self {
        Self {
            offset_texels: [0, 0],
            rotation_deg: 0,
            scale_q8: [256, 256],
        }
    }
}

impl FaceUv {
    /// Whether this is the identity placement.
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }

    /// Compensate the offset so a raw-texel shift of the underlying
    /// surface leaves applied UVs unchanged (texture lock). Exact up to
    /// the i16 offset rounding.
    pub fn compensate_shift(&mut self, raw_shift: [f64; 2]) {
        let with_offset = self.apply(raw_shift);
        let linear = [
            with_offset[0] - f64::from(self.offset_texels[0]),
            with_offset[1] - f64::from(self.offset_texels[1]),
        ];
        self.offset_texels[0] = (f64::from(self.offset_texels[0]) - linear[0]).round() as i16;
        self.offset_texels[1] = (f64::from(self.offset_texels[1]) - linear[1]).round() as i16;
    }

    /// Apply to a raw paraxial texel coordinate.
    pub fn apply(&self, uv: [f64; 2]) -> [f64; 2] {
        let linear = self.apply_linear(uv);
        [
            linear[0] + f64::from(self.offset_texels[0]),
            linear[1] + f64::from(self.offset_texels[1]),
        ]
    }

    /// [`Self::apply`] without the offset term: the scale and rotation
    /// alone. Re-anchoring solves for the offset, so it needs the rest of
    /// the mapping separately.
    pub fn apply_linear(&self, uv: [f64; 2]) -> [f64; 2] {
        let sx = axis_scale_q8(self.scale_q8[0]);
        let sy = axis_scale_q8(self.scale_q8[1]);
        let scaled = [uv[0] / sx, uv[1] / sy];
        let radians = f64::from(self.rotation_deg).to_radians();
        let (sin, cos) = radians.sin_cos();
        [
            scaled[0] * cos - scaled[1] * sin,
            scaled[0] * sin + scaled[1] * cos,
        ]
    }

    /// Re-solve this mapping's offset so `anchor` keeps the UV `previous`
    /// gave it.
    ///
    /// Scale and rotation are applied about the raw projection's own
    /// origin, which is the world origin, not the face. A face 4000 units
    /// away therefore SLIDES by thousands of texels when its scale changes
    /// by a few percent, which is why changing U% read as sliding rather
    /// than resizing. Anchoring at a point on the face itself (the caller
    /// passes the solved polygon centroid) leaves the texture where the
    /// eye expects it and only changes how densely it repeats.
    ///
    /// `slide_texels` is any offset edit the same frame made, re-applied
    /// after the compensation so typing into Offset U/V still slides the
    /// texture deliberately.
    pub fn reanchor(&mut self, previous: &Self, anchor: [f64; 2], slide_texels: [f64; 2]) {
        self.reanchor_to(previous.apply(anchor), anchor, slide_texels);
    }

    /// [`Self::reanchor`] against an explicit target UV.
    ///
    /// A multi-frame edit has to keep solving against the phase the whole
    /// interaction started at. Re-deriving the target from the previous
    /// frame's mapping instead banks this function's own `i16` rounding
    /// once per frame, which walks the texture off the face over a drag.
    pub fn reanchor_to(&mut self, target: [f64; 2], anchor: [f64; 2], slide_texels: [f64; 2]) {
        let linear = self.apply_linear(anchor);
        self.offset_texels = [
            clamp_offset_texels(target[0] - linear[0] + slide_texels[0]),
            clamp_offset_texels(target[1] - linear[1] + slide_texels[1]),
        ];
    }
}

/// Q8 axis scale to a factor. Negative mirrors the axis (Flip H/V);
/// zero is treated as identity so damaged data cannot divide by zero.
fn axis_scale_q8(q8: i16) -> f64 {
    if q8 == 0 {
        1.0
    } else {
        f64::from(q8) / 256.0
    }
}

/// Offsets are stored in an `i16`; a re-anchor of a far-away face at a
/// steep scale change can solve past that, and saturating is the only
/// behaviour that keeps the mapping finite.
fn clamp_offset_texels(value: f64) -> i16 {
    if value.is_nan() {
        return 0;
    }
    value
        .round()
        .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
}

/// Shift a face's applied texel UVs by whole texture repeats so the
/// whole set fits the GPU's u8 window without straddling a 256 wrap.
/// A straddling polygon packs per-vertex wrapped coordinates and
/// rasterizes a backwards texture gradient; power-of-two repeat sizes
/// make the shift sampling-identical. Axes whose span cannot fit the
/// window are left alone (the historic wrap behaviour).
pub fn rebase_texel_uvs(uvs: &mut [[f64; 2]], repeat: [f64; 2]) {
    for axis in 0..2 {
        let repeat = repeat[axis].max(1.0);
        let min = uvs.iter().map(|uv| uv[axis]).fold(f64::MAX, f64::min);
        let max = uvs.iter().map(|uv| uv[axis]).fold(f64::MIN, f64::max);
        if !min.is_finite() || !max.is_finite() {
            continue;
        }
        let shift = (min / repeat).floor() * repeat;
        if max - shift <= 255.0 {
            for uv in uvs.iter_mut() {
                uv[axis] -= shift;
            }
        }
    }
}
