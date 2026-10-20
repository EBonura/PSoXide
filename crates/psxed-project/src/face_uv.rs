//! Per-face texture placement for grid surfaces.

use serde::{Deserialize, Serialize};

/// How a face's texture is laid over it. Every face kind (floor, ceiling,
/// authored and derived wall, plane) uses this one model; see
/// `grid::uv` for the frames it is measured against.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UvMode {
    /// Use the material's grid tiling default, else the world's own
    /// default: world aligned at the world's texels per sector (64 for a
    /// new world, 32 for one made before the setting).
    #[default]
    Inherit,
    /// Texel = world position times a density, so neighbouring faces line
    /// up and a bigger face tiles more instead of stretching.
    World {
        /// Texels per sector along the frame's two axes. 64 is one 64
        /// texel tile per sector; 32 is the density older worlds had.
        texels_per_sector: [u16; 2],
    },
    /// The texture repeats a fixed number of times across the whole face
    /// (its frame), whatever its size.
    Fit {
        /// Repeats along the frame's two axes in 1/16 steps: 16 is one
        /// tile, 32 is 2x2.
        repeats_q4: [u16; 2],
    },
}

impl UvMode {
    /// The mapping older worlds had: world aligned, 32 texels per sector.
    pub const LEGACY: Self = Self::world(32);

    /// World aligned at `texels` texels per sector on both axes.
    pub const fn world(texels: u16) -> Self {
        Self::World {
            texels_per_sector: [texels, texels],
        }
    }

    /// FIT with whole-tile repeats `h` by `v`.
    pub const fn fit(h: u16, v: u16) -> Self {
        Self::Fit {
            repeats_q4: [h * 16, v * 16],
        }
    }

    /// Whether this is `Inherit`.
    pub fn is_inherit(&self) -> bool {
        matches!(self, Self::Inherit)
    }
}

/// The zoom presets of the quick scale picker, as Q8 scales: a quarter, a
/// half, one, two and four times. A scale of 2x draws the texture twice as
/// big (half of it across a sector at 64 texels per sector); 1x is the
/// tiling as it stands.
pub const ZOOM_STEPS_Q8: [i16; 5] = [64, 128, 256, 512, 1024];

/// The preset one step from `q8` in the direction of `delta` (positive
/// bigger, negative smaller), clamped at the ends. A scale between presets
/// goes to the preset beyond it.
pub fn step_zoom_q8(q8: i16, delta: i8) -> i16 {
    let q8 = q8.saturating_abs().max(1);
    let steps = ZOOM_STEPS_Q8;
    if delta > 0 {
        steps.iter().copied().find(|&s| s > q8).unwrap_or(steps[4])
    } else if delta < 0 {
        steps
            .iter()
            .rev()
            .copied()
            .find(|&s| s < q8)
            .unwrap_or(steps[0])
    } else {
        q8
    }
}

fn zero_u8(value: &u8) -> bool {
    *value == 0
}

fn no_flip(flip: &[bool; 2]) -> bool {
    !flip[0] && !flip[1]
}

fn no_grow(grow: &[[i32; 2]; 2]) -> bool {
    *grow == [[0; 2]; 2]
}

/// Per-face texture placement. The mode, quarter turns and flips lay the
/// texture over the face's frame; the fine controls then adjust it:
/// `uv' = rotate(rotation_deg, uv * 256 / scale_q8) + offset_texels`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaceUv {
    /// Texel offset added after rotation/scale.
    pub offset_texels: [i16; 2],
    /// Free rotation in degrees.
    pub rotation_deg: i16,
    /// Per-axis scale, Q8 (256 = 1.0). Larger = texture appears bigger.
    pub scale_q8: [i16; 2],
    /// How the texture is laid over the face's frame.
    #[serde(default, skip_serializing_if = "UvMode::is_inherit")]
    pub mode: UvMode,
    /// Quarter turns of the texture on the face, clockwise, `0..=3`.
    #[serde(default, skip_serializing_if = "zero_u8")]
    pub turns: u8,
    /// Mirror the texture horizontally / vertically on the face.
    #[serde(default, skip_serializing_if = "no_flip")]
    pub flip: [bool; 2],
    /// Grid units the face's frame is widened by on each side, so the mode
    /// is measured against a bigger rectangle than the face itself:
    /// `[[s_min, t_min], [s_max, t_max]]` is how far the frame reaches past
    /// the face's own frame towards smaller and larger `s` and `t`. A Fit
    /// mapping then puts its repeats across that whole rectangle and each
    /// face shows its own share: how "2 x 2 across this 4 x 2 patch" is
    /// stored. Relative to the face, so moving it keeps the mapping. All
    /// zero (the default) is the face's own frame.
    #[serde(default, skip_serializing_if = "no_grow")]
    pub grow: [[i32; 2]; 2],
}

impl Default for FaceUv {
    fn default() -> Self {
        Self {
            offset_texels: [0, 0],
            rotation_deg: 0,
            scale_q8: [256, 256],
            mode: UvMode::Inherit,
            turns: 0,
            flip: [false, false],
            grow: [[0; 2]; 2],
        }
    }
}

impl FaceUv {
    /// Whether this is the identity placement.
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }

    /// The zoom, Q8 (256 is 1x), when both axes have the same size: the
    /// scale without its mirror. `None` for a stretched texture.
    pub fn zoom_q8(&self) -> Option<i16> {
        let size = |q8: i16| if q8 == 0 { 256 } else { q8.saturating_abs() };
        let (x, y) = (size(self.scale_q8[0]), size(self.scale_q8[1]));
        (x == y).then_some(x)
    }

    /// Set the size of the texture on both axes to `q8` (256 is 1x), each
    /// axis keeping its mirror.
    pub fn set_zoom_q8(&mut self, q8: i16) {
        let q8 = q8.saturating_abs().max(1);
        for scale in &mut self.scale_q8 {
            *scale = if *scale < 0 { -q8 } else { q8 };
        }
    }

    /// Whether the fine controls (offset, free rotation, scale) are at their
    /// defaults, whatever the mode, turns and flips are.
    pub fn fine_is_identity(&self) -> bool {
        self.offset_texels == [0, 0] && self.rotation_deg == 0 && self.scale_q8 == [256, 256]
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
/// make the shift sampling-identical. A span of a whole 256 texels (four
/// 64 texel tiles, what a repeating primitive of the cook holds) would end
/// on a byte that wraps back to 0, so its far edge is held at 255, as the
/// cook does. Axes whose span cannot fit the window are left alone (the
/// historic wrap behaviour).
pub fn rebase_texel_uvs(uvs: &mut [[f64; 2]], repeat: [f64; 2]) {
    for axis in 0..2 {
        let repeat = repeat[axis].max(1.0);
        let min = uvs.iter().map(|uv| uv[axis]).fold(f64::MAX, f64::min);
        let max = uvs.iter().map(|uv| uv[axis]).fold(f64::MIN, f64::max);
        if !min.is_finite() || !max.is_finite() {
            continue;
        }
        let shift = (min / repeat).floor() * repeat;
        if max - shift <= 256.5 {
            for uv in uvs.iter_mut() {
                uv[axis] = (uv[axis] - shift).min(255.0);
            }
        }
    }
}

#[cfg(test)]
mod rebase_tests {
    use super::*;

    #[test]
    fn a_span_of_four_tiles_holds_its_far_edge_below_the_byte_wrap() {
        // Four 64 texel tiles starting on a tile line: 128..384 shifts to
        // 0..256, whose last byte would wrap to 0.
        let mut uvs = vec![[128.0, 0.0], [384.0, 0.0], [384.0, 64.0], [128.0, 64.0]];
        rebase_texel_uvs(&mut uvs, [64.0, 64.0]);
        assert_eq!(uvs[0], [0.0, 0.0]);
        assert_eq!(uvs[1], [255.0, 0.0]);
        assert_eq!(uvs[2], [255.0, 64.0]);
        // A span that still fits is only shifted.
        let mut uvs = vec![[70.0, 3.0], [130.0, 3.0]];
        rebase_texel_uvs(&mut uvs, [64.0, 64.0]);
        assert_eq!(uvs, vec![[6.0, 3.0], [66.0, 3.0]]);
        // Beyond four tiles the axis is left alone.
        let mut uvs = vec![[0.0, 0.0], [300.0, 0.0]];
        rebase_texel_uvs(&mut uvs, [64.0, 64.0]);
        assert_eq!(uvs[1][0], 300.0);
    }
}
