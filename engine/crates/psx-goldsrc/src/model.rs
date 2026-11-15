//! HMD8 model helpers the GoldSrc ports share: the vertex-capped loader and the out-of-line,
//! size-optimised animation pose and inverse transform (one copy of each for every call site).

pub use psx_asset::hmd8::*;

/// Reject a model before its vertices can exceed the game-owned scratch arena (`vertex_cap`, the
/// game's generated projection-scratch budget).
#[must_use]
pub fn load(data: &'static [u8], vertex_cap: usize) -> Model {
    Model::from_bytes_with_vertex_cap(data, vertex_cap)
}

/// Rendering and studio collision sample the same retained animation pose.
#[must_use]
#[inline(never)]
#[optimize(size)]
pub fn pose<'a>(
    model: &'a Model,
    frame: usize,
    frame2: usize,
    frac16: u32,
    mouth: u8,
    scratch: &'a mut [psx_asset::hma1::Affine],
) -> Pose<'a> {
    model.pose(frame, frame2, frac16, mouth, scratch)
}

/// Transpose transform about `origin`, then scale, keeping the integer operation order.
#[must_use]
#[inline(never)]
#[optimize(size)]
pub fn inverse_affine(
    point: [i32; 3],
    origin: [i32; 3],
    rotation: &psx_gte::math::Mat3I16,
    scale: i32,
) -> [i32; 3] {
    let d = [
        point[0] - origin[0],
        point[1] - origin[1],
        point[2] - origin[2],
    ];
    core::array::from_fn(|axis| {
        ((rotation.m[0][axis] as i32 * d[0]
            + rotation.m[1][axis] as i32 * d[1]
            + rotation.m[2][axis] as i32 * d[2])
            >> 12)
            * scale
    })
}
