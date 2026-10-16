//! Floor, ceiling and wall surfaces.

use serde::{Deserialize, Serialize};

use crate::face_uv::FaceUv;
use crate::ResourceId;

/// Which diagonal cuts a floor or ceiling quad into its two triangles.
///
/// Corners are ordered `[NW, NE, SE, SW]`. [`GridSplit::NwSe`] gives the
/// triangles `NW,NE,SE` and `NW,SE,SW`; [`GridSplit::NeSw`] gives `NW,NE,SW`
/// and `NE,SE,SW`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GridSplit {
    /// Diagonal from the north-west corner to the south-east corner.
    #[default]
    NwSe,
    /// Diagonal from the north-east corner to the south-west corner.
    NeSw,
}

impl GridSplit {
    /// Corner indices of triangle `0` or `1`, into `[NW, NE, SE, SW]`.
    pub const fn triangle(self, index: usize) -> [usize; 3] {
        match (self, index) {
            (Self::NwSe, 0) => [0, 1, 2],
            (Self::NwSe, _) => [0, 2, 3],
            (Self::NeSw, 0) => [0, 1, 3],
            (Self::NeSw, _) => [1, 2, 3],
        }
    }

    /// The other diagonal.
    pub const fn flipped(self) -> Self {
        match self {
            Self::NwSe => Self::NeSw,
            Self::NeSw => Self::NwSe,
        }
    }
}

/// Material and texture placement of one floor or ceiling triangle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridTri {
    /// Material resource, `None` for the untextured fallback.
    #[serde(default)]
    pub material: Option<ResourceId>,
    /// Offset, rotation and scale over the world-aligned default mapping.
    #[serde(default, skip_serializing_if = "FaceUv::is_identity")]
    pub uv: FaceUv,
}

impl GridTri {
    /// A triangle with `material` and the default mapping.
    pub fn with_material(material: Option<ResourceId>) -> Self {
        Self {
            material,
            uv: FaceUv::default(),
        }
    }
}

fn yes() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

/// A floor or ceiling: four corner heights, a diagonal split and a material
/// per triangle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridSurface {
    /// Corner heights in grid units, `[NW, NE, SE, SW]`, relative to the
    /// room's vertical offset. A slope is just unequal corners.
    pub heights: [i32; 4],
    /// Diagonal split.
    #[serde(default)]
    pub split: GridSplit,
    /// Corner heights of the second triangle when it sits at its own level
    /// (a "split-level" surface). `None` means both triangles share
    /// [`Self::heights`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heights_b: Option<[i32; 4]>,
    /// First triangle (and the second one unless [`Self::tri_b`] is set).
    #[serde(default)]
    pub tri_a: GridTri,
    /// Second triangle override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tri_b: Option<GridTri>,
    /// Whether characters may stand on it. Persisted for the collision pass.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub walkable: bool,
}

impl GridSurface {
    /// A flat surface at `height` with `material` on both triangles.
    pub fn flat(height: i32, material: Option<ResourceId>) -> Self {
        Self {
            heights: [height; 4],
            split: GridSplit::NwSe,
            heights_b: None,
            tri_a: GridTri::with_material(material),
            tri_b: None,
            walkable: true,
        }
    }

    /// Triangle `0` or `1` (`1` falls back to the first when unset).
    pub fn tri(&self, index: usize) -> &GridTri {
        match (index, &self.tri_b) {
            (0, _) | (_, None) => &self.tri_a,
            (_, Some(tri)) => tri,
        }
    }

    /// Mutable triangle `0` or `1`. Asking for `1` materialises the override
    /// as a copy of the first triangle.
    pub fn tri_mut(&mut self, index: usize) -> &mut GridTri {
        if index == 0 {
            &mut self.tri_a
        } else {
            let first = self.tri_a;
            self.tri_b.get_or_insert(first)
        }
    }

    /// Corner heights that triangle `index` uses.
    pub fn heights_for(&self, index: usize) -> [i32; 4] {
        if index == 0 {
            self.heights
        } else {
            self.heights_b.unwrap_or(self.heights)
        }
    }

    /// Lowest and highest corner over both triangles.
    pub fn height_range(&self) -> (i32, i32) {
        let b = self.heights_for(1);
        let all = self.heights.iter().chain(b.iter());
        (
            all.clone().copied().min().unwrap_or(0),
            all.copied().max().unwrap_or(0),
        )
    }

    /// Whether all four corners of triangle 0 lie in one plane.
    pub fn is_planar(&self) -> bool {
        let h = self.heights;
        // Opposite corners sum equal exactly when the quad is planar.
        h[0] + h[2] == h[1] + h[3]
    }
}

/// Which side of a wall is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WallFacing {
    /// Visible from inside the owning sector.
    #[default]
    Front,
    /// Visible from the neighbour side.
    Back,
    /// Visible from both sides.
    Both,
}

/// One wall segment on a sector edge.
///
/// `heights` are `[bottom at a, bottom at b, top at b, top at a]` where
/// `a -> b` is the edge's walking direction, see
/// [`WallEdge::endpoints`](super::sector::WallEdge::endpoints). Slanted tops
/// and bottoms are just unequal heights.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridWall {
    /// Corner heights in grid units, relative to the room's vertical offset.
    pub heights: [i32; 4],
    /// Material resource, `None` for the untextured fallback.
    #[serde(default)]
    pub material: Option<ResourceId>,
    /// Offset, rotation and scale over the world-aligned default mapping.
    #[serde(default, skip_serializing_if = "FaceUv::is_identity")]
    pub uv: FaceUv,
    /// Drawn side.
    #[serde(default)]
    pub facing: WallFacing,
    /// Whether it blocks movement. Persisted for the collision pass.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub solid: bool,
}

impl GridWall {
    /// A rectangular wall between `bottom` and `top`.
    pub fn span(bottom: i32, top: i32, material: Option<ResourceId>) -> Self {
        Self {
            heights: [bottom, bottom, top, top],
            material,
            uv: FaceUv::default(),
            facing: WallFacing::Front,
            solid: true,
        }
    }

    /// Lowest and highest corner.
    pub fn height_range(&self) -> (i32, i32) {
        (
            self.heights.iter().copied().min().unwrap_or(0),
            self.heights.iter().copied().max().unwrap_or(0),
        )
    }

    /// Whether the wall has no area: both rails collapse onto each other.
    pub fn is_degenerate(&self) -> bool {
        let h = self.heights;
        h[0] >= h[3] && h[1] >= h[2]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_triangles_cover_the_quad() {
        for split in [GridSplit::NwSe, GridSplit::NeSw] {
            let mut seen = [0u8; 4];
            for tri in 0..2 {
                for corner in split.triangle(tri) {
                    seen[corner] += 1;
                }
            }
            // Both triangles share the two corners on the diagonal.
            assert_eq!(seen.iter().sum::<u8>(), 6);
            assert!(seen.iter().all(|n| *n >= 1));
        }
    }

    #[test]
    fn second_triangle_defaults_to_the_first() {
        let mut surface = GridSurface::flat(0, None);
        assert_eq!(surface.tri(1), surface.tri(0));
        surface.tri_mut(1).uv.rotation_deg = 90;
        assert_eq!(surface.tri(1).uv.rotation_deg, 90);
        assert_eq!(surface.tri(0).uv.rotation_deg, 0);
    }

    #[test]
    fn planarity_follows_the_opposite_corner_rule() {
        let mut surface = GridSurface::flat(0, None);
        assert!(surface.is_planar());
        surface.heights = [0, 256, 512, 256];
        assert!(surface.is_planar());
        surface.heights = [0, 0, 256, 0];
        assert!(!surface.is_planar());
    }

    #[test]
    fn height_range_spans_both_triangles() {
        let mut surface = GridSurface::flat(256, None);
        surface.heights_b = Some([256, 256, 768, 256]);
        assert_eq!(surface.height_range(), (256, 768));
    }

    #[test]
    fn collapsed_walls_are_degenerate() {
        assert!(GridWall::span(256, 256, None).is_degenerate());
        assert!(!GridWall::span(0, 256, None).is_degenerate());
        // A triangle wall (one rail collapsed) still has area.
        let mut triangle = GridWall::span(0, 256, None);
        triangle.heights[1] = 256;
        assert!(!triangle.is_degenerate());
    }
}
