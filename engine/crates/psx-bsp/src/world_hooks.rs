//! Policy seams of the PXBSP world renderer.
//!
//! The renderer owns the mechanism of drawing a PXBSP world: PVS marking,
//! node-frustum selection, clipping, batching and packet writing. A game that
//! shares that mechanism but has rules of its own (Quake's water portals,
//! texture chains, dynamic lights and stacked floors) supplies them as a
//! [`WorldHooks`] value instead of forking the renderer.
//!
//! Every seam is a monomorphised call guarded by an associated constant, so
//! there is no dynamic dispatch and a game that uses no seam pays nothing:
//! [`NoHooks`] sets every constant to `false`, the guarded code is removed at
//! compile time, and the renderer emits the packets it always did. The
//! `pxbsp_packet_hash` example pins that word for word.

use crate::pxbsp_resident::PxbspResidentMap;
use crate::render::PxbspTextureBinding;

/// What a hook decides for one drawn face.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FaceOverride {
    /// Draw the face with its material's binding, as the renderer would.
    Default,
    /// Do not draw the face (invisible clip and trigger surfaces).
    Skip,
    /// Draw the face with this binding instead of its material's (a liquid
    /// face with its own turbulence tile).
    Bind(PxbspTextureBinding),
}

/// Game rules the world renderer defers to. Each method is reached only when
/// the matching constant is `true`; the defaults describe the unmodified
/// renderer.
pub trait WorldHooks {
    /// [`Self::merge_visibility`] runs after every PVS row is decoded.
    const MERGES_VISIBILITY: bool = false;
    /// [`Self::chain_material`] picks the material a face shows this tick.
    const CHAINS_MATERIALS: bool = false;
    /// [`Self::face_override`] runs for every face that reaches material
    /// resolution.
    const OVERRIDES_FACES: bool = false;
    /// [`Self::light_corner`] adjusts every materialised face corner.
    const LIGHTS_CORNERS: bool = false;
    /// [`Self::depth_bias`] shifts a face's ordering-table slots.
    const BIASES_DEPTH: bool = false;

    /// Changes whenever [`Self::merge_visibility`] would produce a different
    /// row for the same camera leaf. The renderer keeps its marked face chain
    /// while the leaf and this value both stay the same.
    fn visibility_epoch(&self) -> u32 {
        0
    }

    /// Adjust the decoded PVS row of camera leaf `leaf` in place. Bit `i` is
    /// leaf `i + 1`. Quake merges the row of the leaf on the far side of a
    /// water surface into the camera's own.
    fn merge_visibility(&mut self, _map: &PxbspResidentMap, _leaf: usize, _row: &mut [u8]) {}

    /// The material `material` shows at `tick`. Quake's texture chains map
    /// each frame's material to the next frame's.
    fn chain_material(&self, material: usize, _tick: u32) -> usize {
        material
    }

    /// Override one face. `material` is the face's authored material.
    fn face_override(&mut self, _face: usize, _material: usize) -> FaceOverride {
        FaceOverride::Default
    }

    /// The colour word of a face corner at world `position` (whole world
    /// units), given the colour the renderer materialised.
    fn light_corner(&self, _position: [i16; 3], color: u32) -> u32 {
        color
    }

    /// Ordering-table slots to add to every packet of a face with these
    /// cooked flags; negative is nearer. Faces with different biases are
    /// never batched together.
    fn depth_bias(&self, _face_flags: u16) -> i16 {
        0
    }
}

/// The renderer's own behaviour: no seam is used.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoHooks;

impl WorldHooks for NoHooks {}
