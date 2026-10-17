//! Per-face properties: material, texture placement, split and flags.
//!
//! A split floor or ceiling has two triangles. Edits take a `tri`: `None`
//! applies to both, `Some(0)` or `Some(1)` to just that triangle (which
//! gives the surface its own second-triangle record when it had none).

use crate::face_uv::{FaceUv, UvMode};
use crate::ResourceId;

use super::super::surface::{GridSurface, GridTri, WallFacing};
use super::super::world::GridWorld;
use super::refs::{FaceRef, FaceTarget};

/// A texture placement edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UvOp {
    /// Back to the default world-aligned mapping.
    Reset,
    /// Mirror along U.
    FlipU,
    /// Mirror along V.
    FlipV,
    /// Turn a quarter clockwise.
    Rotate90,
    /// Move by texels.
    Shift([i16; 2]),
    /// Set the offset in texels.
    SetOffset([i16; 2]),
    /// Set the scale, Q8 (256 is one).
    SetScale([i16; 2]),
    /// Set the rotation in degrees.
    SetRotation(i16),
    /// Set how the texture is laid over the face's frame.
    SetMode(UvMode),
    /// Turn the texture a quarter, clockwise when `true`.
    Turn(bool),
    /// Mirror the texture horizontally (`0`) or vertically (`1`) on the
    /// face.
    ToggleFlip(usize),
}

impl UvOp {
    /// Apply to one placement.
    pub fn apply(self, uv: &mut FaceUv) {
        match self {
            Self::Reset => *uv = FaceUv::default(),
            Self::FlipU => uv.scale_q8[0] = uv.scale_q8[0].saturating_neg(),
            Self::FlipV => uv.scale_q8[1] = uv.scale_q8[1].saturating_neg(),
            Self::Rotate90 => uv.rotation_deg = (uv.rotation_deg + 90).rem_euclid(360),
            Self::Shift(d) => {
                uv.offset_texels[0] = uv.offset_texels[0].saturating_add(d[0]);
                uv.offset_texels[1] = uv.offset_texels[1].saturating_add(d[1]);
            }
            Self::SetOffset(o) => uv.offset_texels = o,
            Self::SetScale(s) => uv.scale_q8 = s,
            Self::SetRotation(r) => uv.rotation_deg = r,
            Self::SetMode(mode) => uv.mode = mode,
            Self::Turn(clockwise) => {
                uv.turns = if clockwise {
                    (uv.turns + 1) & 3
                } else {
                    (uv.turns + 3) & 3
                };
            }
            Self::ToggleFlip(axis) => {
                if let Some(flip) = uv.flip.get_mut(axis) {
                    *flip = !*flip;
                }
            }
        }
    }
}

/// Collapse an identical second triangle back into "none", so merged quads
/// stay merged.
fn tidy(surface: &mut GridSurface) {
    if surface.tri_b == Some(surface.tri_a) {
        surface.tri_b = None;
    }
}

/// Run `edit` on the triangle record(s) of `surface` selected by `tri`.
fn edit_tris(surface: &mut GridSurface, tri: Option<u8>, mut edit: impl FnMut(&mut GridTri)) {
    match tri {
        None => {
            edit(&mut surface.tri_a);
            if let Some(b) = surface.tri_b.as_mut() {
                edit(b);
            }
        }
        Some(0) => {
            // Give the second triangle its own copy first, so editing the
            // first does not change both.
            surface.tri_mut(1);
            edit(&mut surface.tri_a);
        }
        Some(_) => edit(surface.tri_mut(1)),
    }
    tidy(surface);
}

/// Set the material of `face` (all triangles, or one with `tri`).
pub fn set_material(
    world: &mut GridWorld,
    face: FaceRef,
    tri: Option<u8>,
    material: Option<ResourceId>,
) -> bool {
    match face.target {
        FaceTarget::Surface(_) => {
            let Some(surface) = face.surface_mut(world) else {
                return false;
            };
            let mut changed = false;
            edit_tris(surface, tri, |t| {
                changed |= t.material != material;
                t.material = material;
            });
            changed
        }
        FaceTarget::Wall { .. } => {
            let Some(wall) = face.wall_mut(world) else {
                return false;
            };
            let changed = wall.material != material;
            wall.material = material;
            changed
        }
        FaceTarget::Derived { .. } => super::derived::set_derived_material(world, face, material),
    }
}

/// The material of `face`'s triangle (or of the wall).
pub fn material_of(world: &GridWorld, face: FaceRef, tri: Option<u8>) -> Option<ResourceId> {
    match face.target {
        FaceTarget::Surface(_) => face
            .surface(world)
            .and_then(|s| s.tri(usize::from(tri.unwrap_or(0))).material),
        FaceTarget::Wall { .. } => face.wall_of(world).and_then(|w| w.material),
        FaceTarget::Derived { .. } => super::derived::derived_material(world, face),
    }
}

/// The placement of `face`'s triangle (or of the wall).
pub fn uv_of(world: &GridWorld, face: FaceRef, tri: Option<u8>) -> Option<FaceUv> {
    match face.target {
        FaceTarget::Surface(_) => face
            .surface(world)
            .map(|s| s.tri(usize::from(tri.unwrap_or(0))).uv),
        FaceTarget::Wall { .. } => face.wall_of(world).map(|w| w.uv),
        FaceTarget::Derived { .. } => super::derived::derived_uv(world, face),
    }
}

/// Edit the texture placement of `face`.
pub fn edit_uv(world: &mut GridWorld, face: FaceRef, tri: Option<u8>, op: UvOp) -> bool {
    match face.target {
        FaceTarget::Surface(_) => {
            let Some(surface) = face.surface_mut(world) else {
                return false;
            };
            let mut changed = false;
            edit_tris(surface, tri, |t| {
                let before = t.uv;
                op.apply(&mut t.uv);
                changed |= before != t.uv;
            });
            changed
        }
        FaceTarget::Wall { .. } => {
            let Some(wall) = face.wall_mut(world) else {
                return false;
            };
            let before = wall.uv;
            op.apply(&mut wall.uv);
            before != wall.uv
        }
        FaceTarget::Derived { .. } => super::derived::edit_derived_uv(world, face, op),
    }
}

/// Flip a floor or ceiling's diagonal. Returns whether `face` is a surface.
pub fn toggle_split(world: &mut GridWorld, face: FaceRef) -> bool {
    match face.surface_mut(world) {
        Some(surface) => {
            surface.split = surface.split.flipped();
            true
        }
        None => false,
    }
}

/// Set the visible side of a wall.
pub fn set_wall_facing(world: &mut GridWorld, face: FaceRef, facing: WallFacing) -> bool {
    match face.wall_mut(world) {
        Some(wall) => {
            let changed = wall.facing != facing;
            wall.facing = facing;
            changed
        }
        None => false,
    }
}

/// Set whether a wall blocks movement, or a surface can be walked on.
pub fn set_solid(world: &mut GridWorld, face: FaceRef, value: bool) -> bool {
    match face.target {
        FaceTarget::Surface(_) => match face.surface_mut(world) {
            Some(s) => {
                let changed = s.walkable != value;
                s.walkable = value;
                changed
            }
            None => false,
        },
        FaceTarget::Wall { .. } => match face.wall_mut(world) {
            Some(w) => {
                let changed = w.solid != value;
                w.solid = value;
                changed
            }
            None => false,
        },
        FaceTarget::Derived { .. } => super::derived::set_derived_solid(world, face, value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::room::GridRoom;
    use crate::grid::sector::WallEdge;
    use crate::grid::surface::{GridSplit, GridWall};
    use crate::grid::units::GridScale;
    use crate::{MaterialResource, ProjectDocument, ResourceData};

    fn materials() -> (ResourceId, ResourceId) {
        let mut project = ProjectDocument::new("m");
        let mut add = |name| {
            project.add_resource(name, ResourceData::Material(MaterialResource::opaque(None)))
        };
        (add("a"), add("b"))
    }

    fn world() -> GridWorld {
        let mut room = GridRoom::new("r", 1, 1);
        let s = room.sector_or_insert(0, 0).unwrap();
        s.floor = Some(GridSurface::flat(0, None));
        s.north.push(GridWall::span(0, 512, None));
        GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![room],
            ..GridWorld::default()
        }
    }

    const FLOOR: FaceRef = FaceRef {
        room: 0,
        cell: [0, 0],
        target: FaceTarget::Surface(super::super::refs::SurfaceKind::Floor),
    };

    #[test]
    fn a_whole_face_material_keeps_a_merged_quad_merged() {
        let (a, b) = materials();
        let mut w = world();
        assert!(set_material(&mut w, FLOOR, None, Some(a)));
        assert!(!set_material(&mut w, FLOOR, None, Some(a)));
        assert!(set_material(&mut w, FLOOR, None, Some(b)));
        let floor = FLOOR.surface(&w).unwrap();
        assert!(floor.tri_b.is_none());
        assert_eq!(floor.tri_a.material, Some(b));
    }

    #[test]
    fn one_triangle_can_carry_its_own_material() {
        let (a, b) = materials();
        let mut w = world();
        set_material(&mut w, FLOOR, None, Some(a));
        set_material(&mut w, FLOOR, Some(1), Some(b));
        assert_eq!(material_of(&w, FLOOR, Some(0)), Some(a));
        assert_eq!(material_of(&w, FLOOR, Some(1)), Some(b));
        // Editing the first alone leaves the second as it was.
        let (c, _) = materials();
        set_material(&mut w, FLOOR, Some(0), Some(c));
        assert_eq!(material_of(&w, FLOOR, Some(0)), Some(c));
        assert_eq!(material_of(&w, FLOOR, Some(1)), Some(b));
        // Painting both the same again merges them back.
        set_material(&mut w, FLOOR, None, Some(a));
        assert!(FLOOR.surface(&w).unwrap().tri_b.is_none());
    }

    #[test]
    fn painting_the_first_triangle_of_an_unsplit_floor_leaves_the_second() {
        let (a, b) = materials();
        let mut w = world();
        set_material(&mut w, FLOOR, None, Some(a));
        set_material(&mut w, FLOOR, Some(0), Some(b));
        assert_eq!(material_of(&w, FLOOR, Some(1)), Some(a));
        assert_eq!(material_of(&w, FLOOR, Some(0)), Some(b));
    }

    #[test]
    fn walls_take_materials_and_uv_edits() {
        let (a, _) = materials();
        let mut w = world();
        let wall = FaceRef::wall(0, [0, 0], WallEdge::North, 0);
        assert!(set_material(&mut w, wall, None, Some(a)));
        assert_eq!(material_of(&w, wall, None), Some(a));
        assert!(edit_uv(&mut w, wall, None, UvOp::Rotate90));
        assert!(edit_uv(&mut w, wall, None, UvOp::FlipU));
        let uv = uv_of(&w, wall, None).unwrap();
        assert_eq!((uv.rotation_deg, uv.scale_q8), (90, [-256, 256]));
        assert!(edit_uv(&mut w, wall, None, UvOp::Reset));
        assert!(uv_of(&w, wall, None).unwrap().is_identity());
        assert!(!edit_uv(&mut w, wall, None, UvOp::Reset));
    }

    #[test]
    fn mode_turn_and_flip_ops_edit_the_model_fields() {
        let mut uv = FaceUv::default();
        UvOp::SetMode(UvMode::fit(2, 2)).apply(&mut uv);
        UvOp::Turn(true).apply(&mut uv);
        UvOp::Turn(true).apply(&mut uv);
        UvOp::Turn(false).apply(&mut uv);
        UvOp::ToggleFlip(1).apply(&mut uv);
        assert_eq!(
            (uv.mode, uv.turns, uv.flip),
            (UvMode::fit(2, 2), 1, [false, true])
        );
        UvOp::Turn(false).apply(&mut uv);
        UvOp::Turn(false).apply(&mut uv);
        assert_eq!(uv.turns, 3);
        UvOp::ToggleFlip(1).apply(&mut uv);
        UvOp::SetMode(UvMode::Inherit).apply(&mut uv);
        UvOp::Turn(true).apply(&mut uv);
        assert!(uv.is_identity());
    }

    #[test]
    fn uv_ops_compose() {
        let mut uv = FaceUv::default();
        for _ in 0..5 {
            UvOp::Rotate90.apply(&mut uv);
        }
        assert_eq!(uv.rotation_deg, 90);
        UvOp::Shift([3, -2]).apply(&mut uv);
        UvOp::Shift([1, 1]).apply(&mut uv);
        assert_eq!(uv.offset_texels, [4, -1]);
        UvOp::SetScale([512, 128]).apply(&mut uv);
        UvOp::FlipV.apply(&mut uv);
        assert_eq!(uv.scale_q8, [512, -128]);
    }

    #[test]
    fn per_triangle_uv_edits() {
        let mut w = world();
        edit_uv(&mut w, FLOOR, Some(1), UvOp::Rotate90);
        assert_eq!(uv_of(&w, FLOOR, Some(0)).unwrap().rotation_deg, 0);
        assert_eq!(uv_of(&w, FLOOR, Some(1)).unwrap().rotation_deg, 90);
    }

    #[test]
    fn split_flags_and_facing() {
        let mut w = world();
        assert!(toggle_split(&mut w, FLOOR));
        assert_eq!(FLOOR.surface(&w).unwrap().split, GridSplit::NeSw);
        let wall = FaceRef::wall(0, [0, 0], WallEdge::North, 0);
        assert!(!toggle_split(&mut w, wall));
        assert!(set_wall_facing(&mut w, wall, WallFacing::Both));
        assert!(!set_wall_facing(&mut w, wall, WallFacing::Both));
        assert!(set_solid(&mut w, wall, false));
        assert!(!wall.wall_of(&w).unwrap().solid);
        assert!(set_solid(&mut w, FLOOR, false));
        assert!(!FLOOR.surface(&w).unwrap().walkable);
    }
}
