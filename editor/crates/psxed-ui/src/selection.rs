use super::*;

pub(crate) type SectorSelection = (NodeId, u16, u16);

/// What the next paint click would target. Carries world-cell
/// coordinates (which can be negative -- outside the current grid)
/// so the renderer can preview cells the next click would auto-
/// create. Stays populated for any paint tool, mirroring the
/// dispatch so what you preview is what you'll paint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaintTargetPreview {
    /// Floor / ceiling / erase / place -- outlines the cell.
    Cell {
        world_cell_x: i32,
        world_cell_z: i32,
        kind: PaintCellPreviewKind,
    },
    /// PaintWall -- outlines the wall that would be added on the
    /// targeted edge. `stack` is the next-free wall slot index for
    /// that edge, used by the renderer to position the ghost above
    /// any existing walls.
    Wall {
        world_cell_x: i32,
        world_cell_z: i32,
        dir: GridDirection,
        stack: u8,
    },
    /// Portal placement -- highlights the cardinal edge that will
    /// become an open seam. `valid` is false when either side of the
    /// edge is missing authored geometry, so the click will be
    /// rejected instead of creating a marker the cooker ignores.
    PortalEdge {
        world_cell_x: i32,
        world_cell_z: i32,
        dir: GridDirection,
        valid: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaintCellPreviewKind {
    Ground,
    Floor,
    Ceiling,
}

/// One pickable surface on the active Room's grid. Floors and
/// ceilings are addressed by sector; walls add a cardinal direction
/// plus a stack index (a single edge can hold multiple stacked walls
/// -- windows / arches -- and each is independently selectable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaceKind {
    Floor,
    Ceiling,
    Wall { dir: GridDirection, stack: u8 },
}

/// Horizontal surface type for triangle editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HorizontalSurfaceKind {
    Floor,
    Ceiling,
}

impl HorizontalSurfaceKind {
    const fn face_kind(self) -> FaceKind {
        match self {
            Self::Floor => FaceKind::Floor,
            Self::Ceiling => FaceKind::Ceiling,
        }
    }
}

/// Which half of a split floor/ceiling face is being addressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HorizontalTriangleIndex {
    A,
    B,
}

impl HorizontalTriangleIndex {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
        }
    }

    pub(crate) const fn idx(self) -> usize {
        match self {
            Self::A => 0,
            Self::B => 1,
        }
    }
}

/// One triangle half of a floor or ceiling face. The corner list
/// snapshots the split layout at pick time so downstream edit code
/// can move/outline the exact triangle the user selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HorizontalTriangleRef {
    pub room: NodeId,
    pub sx: u16,
    pub sz: u16,
    pub surface: HorizontalSurfaceKind,
    pub index: HorizontalTriangleIndex,
    pub corners: [Corner; 3],
}

impl HorizontalTriangleRef {
    pub const fn parent_face(self) -> FaceRef {
        FaceRef {
            room: self.room,
            sx: self.sx,
            sz: self.sz,
            kind: self.surface.face_kind(),
        }
    }

    pub const fn face_corner(self, corner: Corner) -> FaceCornerRef {
        match self.surface {
            HorizontalSurfaceKind::Floor => FaceCornerRef::Floor {
                sx: self.sx,
                sz: self.sz,
                corner,
            },
            HorizontalSurfaceKind::Ceiling => FaceCornerRef::Ceiling {
                sx: self.sx,
                sz: self.sz,
                corner,
            },
        }
    }
}

/// A face inside the active Room, fully qualified by Room id +
/// sector + face kind. Used by the Select tool's hover / selected
/// state and the per-face inspector that follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaceRef {
    pub room: NodeId,
    pub sx: u16,
    pub sz: u16,
    pub kind: FaceKind,
}

// Corner / WallCorner live in `psxed-project` so faces can carry
// `dropped_corner` data with serde support. Re-exported here so
// existing imports (`use psxed_ui::Corner`) keep working.
pub use psxed_project::{Corner, WallCorner};

/// Which of the four edges of a wall quad. Order matches the
/// perimeter walk used by the picker:
/// `Bottom = BL-BR`, `Right = BR-TR`, `Top = TR-TL`,
/// `Left = TL-BL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallEdge {
    Bottom,
    Right,
    Top,
    Left,
}

/// One face-corner. `Selection::Vertex(_)` resolves through
/// [`physical_vertex`] to a `Vec<FaceCornerRef>` listing every
/// face-corner currently sharing the same world position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaceCornerRef {
    Floor {
        sx: u16,
        sz: u16,
        corner: Corner,
    },
    FloorTriangle {
        sx: u16,
        sz: u16,
        triangle: HorizontalTriangleIndex,
        corner: Corner,
    },
    Ceiling {
        sx: u16,
        sz: u16,
        corner: Corner,
    },
    CeilingTriangle {
        sx: u16,
        sz: u16,
        triangle: HorizontalTriangleIndex,
        corner: Corner,
    },
    Wall {
        sx: u16,
        sz: u16,
        dir: GridDirection,
        stack: u8,
        corner: WallCorner,
    },
}

/// Vertex in a `Selection`. Carries the *seed* corner -- the one
/// the user actually clicked. Resolve to a `PhysicalVertex` to
/// get every coincident face-corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VertexRef {
    pub room: NodeId,
    pub anchor: VertexAnchor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertexAnchor {
    Floor {
        sx: u16,
        sz: u16,
        corner: Corner,
    },
    Ceiling {
        sx: u16,
        sz: u16,
        corner: Corner,
    },
    Wall {
        sx: u16,
        sz: u16,
        dir: GridDirection,
        stack: u8,
        corner: WallCorner,
    },
}

impl VertexAnchor {
    pub const fn as_face_corner(self) -> FaceCornerRef {
        match self {
            Self::Floor { sx, sz, corner } => FaceCornerRef::Floor { sx, sz, corner },
            Self::Ceiling { sx, sz, corner } => FaceCornerRef::Ceiling { sx, sz, corner },
            Self::Wall {
                sx,
                sz,
                dir,
                stack,
                corner,
            } => FaceCornerRef::Wall {
                sx,
                sz,
                dir,
                stack,
                corner,
            },
        }
    }
}

/// Edge in a `Selection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdgeRef {
    pub room: NodeId,
    pub anchor: EdgeAnchor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeAnchor {
    Floor {
        sx: u16,
        sz: u16,
        dir: GridDirection,
    },
    Ceiling {
        sx: u16,
        sz: u16,
        dir: GridDirection,
    },
    Wall {
        sx: u16,
        sz: u16,
        dir: GridDirection,
        stack: u8,
        edge: WallEdge,
    },
}

/// Tagged selection used by the editor's Select tool. Replaces
/// the previous `selected_face: Option<FaceRef>` so all three
/// modes share one piece of state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    Face(FaceRef),
    Triangle(HorizontalTriangleRef),
    Edge(EdgeRef),
    Vertex(VertexRef),
}

impl Selection {
    /// The room this selection belongs to.
    pub const fn room(&self) -> NodeId {
        match self {
            Self::Face(f) => f.room,
            Self::Triangle(t) => t.room,
            Self::Edge(e) => e.room,
            Self::Vertex(v) => v.room,
        }
    }

    /// Convenience: when the selection is a face, hand it to
    /// callers that still want the old `FaceRef` shape (e.g.
    /// the per-face inspector).
    pub const fn as_face(&self) -> Option<FaceRef> {
        match self {
            Self::Face(f) => Some(*f),
            Self::Triangle(t) => Some(t.parent_face()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MaterialTarget {
    Face(FaceRef),
    Triangle(HorizontalTriangleRef),
    BrushFace { brush: usize, face: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BoxPropMaterialAssignment {
    pub(crate) material: ResourceId,
    pub(crate) targets: usize,
    pub(crate) updated: usize,
}

pub(crate) fn world_cook_error_primitives(
    room: NodeId,
    error: &WorldGridCookError,
    array_origin: [u16; 2],
) -> Vec<Selection> {
    let face = |x: u16, z: u16, kind: WorldGridFaceKind| {
        world_cook_face_selection(
            room,
            x.saturating_add(array_origin[0]),
            z.saturating_add(array_origin[1]),
            kind,
        )
    };

    match *error {
        WorldGridCookError::UnassignedMaterial { x, z, face: kind } => vec![face(x, z, kind)],
        WorldGridCookError::InvalidWallHeights {
            x, z, direction, ..
        }
        | WorldGridCookError::UnsupportedDiagonalWall { x, z, direction }
        | WorldGridCookError::WallStackExceeded {
            x, z, direction, ..
        } => vec![face(x, z, WorldGridFaceKind::Wall(direction))],
        WorldGridCookError::DuplicatePhysicalWall {
            x,
            z,
            direction,
            other_x,
            other_z,
            other_direction,
        } => vec![
            face(x, z, WorldGridFaceKind::Wall(direction)),
            face(other_x, other_z, WorldGridFaceKind::Wall(other_direction)),
        ],
        WorldGridCookError::HeightNotQuantized {
            x, z, face: kind, ..
        }
        | WorldGridCookError::TriangleFaceNotSupported { x, z, face: kind } => {
            vec![face(x, z, kind)]
        }
        _ => Vec::new(),
    }
}

pub(crate) fn world_cook_face_selection(
    room: NodeId,
    sx: u16,
    sz: u16,
    kind: WorldGridFaceKind,
) -> Selection {
    let kind = match kind {
        WorldGridFaceKind::Floor => FaceKind::Floor,
        WorldGridFaceKind::Ceiling => FaceKind::Ceiling,
        WorldGridFaceKind::Wall(dir) => FaceKind::Wall { dir, stack: 0 },
    };
    Selection::Face(FaceRef { room, sx, sz, kind })
}

/// Kind label for an [`EntityBounds`]. Drives picking
/// priorities and per-kind gizmo rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityBoundKind {
    /// Model-backed `MeshInstance` with parsed model bounds.
    Model,
    /// Legacy / unbound `MeshInstance` -- fallback box.
    MeshFallback,
    /// Flat `ImageProp`.
    ImageProp,
    /// Editable boxed prop.
    BoxProp,
    /// Low-poly procedural radial prop.
    CylinderProp,
    /// Tile-snapped procedural arch.
    ArchProp,
    /// `SpawnPoint` (player or non-player).
    SpawnPoint,
    /// `PointLight`. Marker box only -- radius ring is drawn
    /// separately so a wide-radius light doesn't intercept
    /// every click in the room.
    PointLight,
    /// `ParticleEmitter`.
    ParticleEmitter,
    /// `Portal`.
    Portal,
    /// Placed `Logic` graph node (trigger volume / relay /
    /// multisource / door).
    Logic,
}

/// World-space AABB for one selectable scene entity.
/// Coordinates use [`psxed_project::spatial::node_preview_bounds_center`]
/// for entities under a Room, so bounds line up with the same
/// origin-aware preview world used by rendered models, markers, and
/// lights.
#[derive(Debug, Clone, Copy)]
pub struct EntityBounds {
    /// Owning scene-tree node id.
    pub node: NodeId,
    /// Enclosing Room id, if any. Used to filter picking to
    /// the active room.
    pub room: Option<NodeId>,
    /// Bound class for visual styling + picking priority.
    pub kind: EntityBoundKind,
    /// World-space AABB centre.
    pub center: [f32; 3],
    /// World-space half-extents along X / Y / Z. Always
    /// positive.
    pub half_extents: [f32; 3],
    /// Authored Y rotation in degrees. Stored on the bound
    /// so the renderer can draw a facing arrow without
    /// re-walking the scene tree.
    pub yaw_degrees: f32,
}

/// Result of a successful entity-bound pick.
#[derive(Debug, Clone, Copy)]
pub struct EntityBoundHit {
    /// Hit node.
    pub node: NodeId,
    /// Distance from the ray origin to the first hit slab,
    /// in world units. Used to compare against grid hits and
    /// other entity hits.
    pub distance: f32,
    /// World-space hit point along the ray.
    pub point: [f32; 3],
    /// Bounds that produced the hit.
    pub bounds: EntityBounds,
}

/// Slab-intersection ray-vs-AABB. Returns the smallest
/// non-negative `t` for which `origin + t * dir` lands on
/// the box surface (or inside it).
///
/// * `dir` is *not* required to be unit length; the returned
///   `t` is in the same units as `dir`. When the editor uses
///   normalized rays (`camera_ray_for_pointer`), `t` lands in
///   world units.
/// * Box must have positive `half_extents`. Zero-extent boxes
///   never hit.
/// * Rays starting *inside* the box return `t = 0` so callers
///   can still pick something they're standing on.
pub fn ray_intersects_aabb(
    origin: [f32; 3],
    dir: [f32; 3],
    center: [f32; 3],
    half_extents: [f32; 3],
) -> Option<f32> {
    let mut t_min = f32::NEG_INFINITY;
    let mut t_max = f32::INFINITY;
    for axis in 0..3 {
        let half = half_extents[axis];
        if half <= 0.0 {
            return None;
        }
        let lo = center[axis] - half;
        let hi = center[axis] + half;
        let o = origin[axis];
        let d = dir[axis];
        if d.abs() < 1e-6 {
            // Ray parallel to this axis -- only hits if origin
            // is between the slabs.
            if o < lo || o > hi {
                return None;
            }
        } else {
            let inv = 1.0 / d;
            let t1 = (lo - o) * inv;
            let t2 = (hi - o) * inv;
            let (t_near, t_far) = if t1 < t2 { (t1, t2) } else { (t2, t1) };
            if t_near > t_min {
                t_min = t_near;
            }
            if t_far < t_max {
                t_max = t_far;
            }
            if t_min > t_max {
                return None;
            }
        }
    }
    if t_max < 0.0 {
        return None;
    }
    Some(if t_min < 0.0 { 0.0 } else { t_min })
}

/// Intersect a ray with the horizontal plane `y = plane_y`.
/// Used by the entity-drag path to project mouse-move into
/// world-space on the same plane the entity lives on.
/// Returns `None` for parallel rays or hits behind the camera.
pub fn ray_intersects_horizontal_plane(
    origin: [f32; 3],
    dir: [f32; 3],
    plane_y: f32,
) -> Option<[f32; 3]> {
    if dir[1].abs() < 1e-6 {
        return None;
    }
    let t = (plane_y - origin[1]) / dir[1];
    if t < 0.0 {
        return None;
    }
    Some([origin[0] + dir[0] * t, plane_y, origin[2] + dir[2] * t])
}

pub(crate) fn ray_intersects_axis_aligned_plane(
    origin: [f32; 3],
    dir: [f32; 3],
    normal_axis: PrimitiveGizmoAxis,
    plane_coord: f32,
) -> Option<[f32; 3]> {
    let axis = normal_axis.index();
    if dir[axis].abs() < 1e-6 {
        return None;
    }
    let t = (plane_coord - origin[axis]) / dir[axis];
    if t < 0.0 {
        return None;
    }
    let mut hit = [
        origin[0] + dir[0] * t,
        origin[1] + dir[1] * t,
        origin[2] + dir[2] * t,
    ];
    hit[axis] = plane_coord;
    Some(hit)
}

#[cfg(test)]
mod entity_bounds_tests {
    use super::ray_intersects_aabb as ray_aabb;
    use super::ray_intersects_horizontal_plane as ray_plane;

    #[test]
    fn ray_aabb_hits_centred_box() {
        // Ray along +Z toward origin AABB at distance 10.
        let t = ray_aabb(
            [0.0, 0.0, -10.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
        );
        assert!(t.is_some());
        // Hit should land on the near slab at t = 9.
        assert!((t.unwrap() - 9.0).abs() < 1e-3);
    }

    #[test]
    fn ray_aabb_misses_offset_box() {
        // Box offset to +X by 100 -- a +Z ray at origin misses.
        let t = ray_aabb(
            [0.0, 0.0, -10.0],
            [0.0, 0.0, 1.0],
            [100.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
        );
        assert!(t.is_none());
    }

    #[test]
    fn ray_aabb_origin_inside_box_returns_zero() {
        let t = ray_aabb(
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0],
            [10.0, 10.0, 10.0],
        );
        assert_eq!(t, Some(0.0));
    }

    #[test]
    fn ray_aabb_zero_extent_never_hits() {
        let t = ray_aabb(
            [0.0, 0.0, -10.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0],
            [0.0, 1.0, 1.0],
        );
        assert!(t.is_none());
    }

    #[test]
    fn ray_aabb_ray_parallel_to_slab() {
        // Ray on the X axis at Y=10, box at Y=0. Parallel +X
        // ray never enters the Y slab so it must miss.
        let t = ray_aabb(
            [0.0, 10.0, 0.0],
            [1.0, 0.0, 0.0],
            [50.0, 0.0, 0.0],
            [5.0, 5.0, 5.0],
        );
        assert!(t.is_none());
    }

    #[test]
    fn ray_aabb_nearest_of_two_boxes() {
        // Two co-axial boxes; near box at z=10, far box at
        // z=50. Nearest t corresponds to the near box.
        let near = ray_aabb(
            [0.0, 0.0, -10.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
        );
        let far = ray_aabb(
            [0.0, 0.0, -10.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 50.0],
            [1.0, 1.0, 1.0],
        );
        assert!(near.unwrap() < far.unwrap());
    }

    #[test]
    fn ray_plane_hits_horizontal_plane_below() {
        // Camera 100 above origin looking down → +Z forward,
        // -Y up. Hit floor plane y=0 at t=100.
        let p = ray_plane([0.0, 100.0, 0.0], [0.0, -1.0, 0.0], 0.0);
        assert!(p.is_some());
        let p = p.unwrap();
        assert!((p[1] - 0.0).abs() < 1e-3);
    }

    #[test]
    fn ray_plane_misses_when_parallel() {
        let p = ray_plane([0.0, 100.0, 0.0], [1.0, 0.0, 0.0], 0.0);
        assert!(p.is_none());
    }

    #[test]
    fn ray_plane_misses_when_behind_camera() {
        // Ray points away from the plane.
        let p = ray_plane([0.0, 100.0, 0.0], [0.0, 1.0, 0.0], 0.0);
        assert!(p.is_none());
    }
}
