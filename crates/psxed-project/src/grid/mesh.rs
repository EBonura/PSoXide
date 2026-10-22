//! Static meshes: decorative and architectural pieces placed in a room.
//!
//! A [`MeshAsset`] is a recipe or a piece of imported geometry in its own
//! local frame, a [`GridMesh`] is one placed copy of an asset in a room. The
//! world keeps the assets, each room keeps its instances (`docs/grid/meshes.md`
//! explains why a mesh is not grid geometry and how it reaches the runtime).
//!
//! Frames and units. Everything is integers in grid units (1024 to a sector).
//! An asset's frame has its origin on the floor at the centre of its base,
//! `+x` to the right, `+y` up, `+z` toward the front. An instance scales it
//! per axis in Q8, turns it with the plane convention (roll about the local
//! z axis, then pitch about x, then yaw about the vertical) and lifts it to
//! `position`, where `x` and `z` are world lattice coordinates like every
//! sector and `y` is relative to the room's `y_offset` like every height.
//!
//! What an asset builds ([`BuiltMesh`]): faces (rings of 3 or 4 corners with
//! texel UVs and a material slot), collision proxies and sockets. The cook
//! turns faces into the room's plane-run quads, proxies into collision plane
//! records and sockets into entity records, so a mesh needs no runtime code of
//! its own.

use serde::{Deserialize, Serialize};

use crate::ResourceId;

pub use super::mesh_arch::{ArchProfile, ArchSpec};
pub use super::mesh_column::{ColumnSpec, RubbleSpec};
pub use super::mesh_obj::{ImportProxy, ImportedFace, ImportedMesh};
pub use super::mesh_span::SpanSpec;

/// Most faces one instance may build (one plane-run in the chunk holds at
/// most 255 quads).
pub const MAX_MESH_FACES: usize = 255;
/// Most instances one room owns. Each instance is one box test per frame in
/// the renderer and up to a dozen collision records.
pub const MAX_MESHES_PER_ROOM: usize = 128;
/// Most assets in a world.
pub const MAX_MESH_ASSETS: usize = 255;
/// Material slots an asset names, at most.
pub const MAX_MESH_SLOTS: usize = 8;
/// Height, runtime units, the hook runtime puts its grip marker above a hook
/// point record (`psx_game_runtime::hook_points::MARKER_HEIGHT`; a test in that
/// crate pins both to the same number).
pub const HOOK_MARKER_HEIGHT_RU: i32 = 96;
/// Unit of the Q8 scale: 256 is 1.0.
pub const SCALE_ONE: u16 = 256;
/// Smallest and largest scale on an axis, Q8 (1/8 to 8).
pub const SCALE_RANGE: (u16, u16) = (32, 2048);

/// One corner list of a mesh face, in the asset frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeshFace {
    /// Corners, grid units. Counter-clockwise seen from the front, so the
    /// right-handed cross product of the ring points at the visible side.
    pub pts: [[i32; 3]; 4],
    /// Corners used, 3 or 4.
    pub len: u8,
    /// Texel coordinates per corner (32 texels to a sector at the default
    /// density), rebased by the cook into the texture.
    pub texels: [[i32; 2]; 4],
    /// Index into the asset's material slots.
    pub slot: u8,
    /// Drawn from both sides.
    pub double_sided: bool,
}

impl MeshFace {
    /// A face from a ring of 3 or 4 points; texels follow `texel`.
    pub fn new(
        ring: &[[i32; 3]],
        slot: u8,
        texel: impl Fn([i32; 3]) -> [i32; 2],
        double_sided: bool,
    ) -> Self {
        let len = ring.len().clamp(3, 4);
        let mut pts = [[0; 3]; 4];
        let mut texels = [[0; 2]; 4];
        for (i, p) in ring.iter().take(len).enumerate() {
            pts[i] = *p;
            texels[i] = texel(*p);
        }
        Self {
            pts,
            len: len as u8,
            texels,
            slot,
            double_sided,
        }
    }

    /// The corners in use.
    pub fn ring(&self) -> &[[i32; 3]] {
        &self.pts[..usize::from(self.len)]
    }
}

/// A collision shape of an asset, in the asset frame. The cook turns each into
/// the collision record the grid already has for free planes
/// (`psx_grid::ColPlane`), so bodies, the camera and the hook all see a mesh
/// through the native collision provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Proxy {
    /// A box that blocks from every side. A box turned by a multiple of 90
    /// degrees about the vertical stays exact; any other turn becomes four
    /// vertical walls (and the top if `top`), which are exact.
    Box {
        /// Smallest corner.
        min: [i32; 3],
        /// Largest corner.
        max: [i32; 3],
        /// Also a surface to stand on over the top face.
        top: bool,
    },
    /// A slab under a surface: the four corners `[TL, TR, BR, BL]` of the
    /// surface and the thickness below it. Classified at the cook like a
    /// plane: a platform to stand on when it faces up within the walk slope,
    /// a wall segment when it is upright, else a blocker box (over-blocks).
    Slab {
        /// Surface corners.
        corners: [[i32; 3]; 4],
        /// Thickness, grid units.
        thickness: i32,
    },
}

/// A point of an asset that becomes something at runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Socket {
    /// Where a hand grips: the cook puts a hook point 96 runtime units under
    /// `anchor` (the marker height the hook runtime adds), facing `yaw`
    /// degrees from the instance's front.
    Hook {
        /// The grip point, grid units, asset frame.
        anchor: [i32; 3],
        /// Heading of the body, degrees.
        yaw: i32,
    },
}

/// What an asset builds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuiltMesh {
    /// The drawable faces.
    pub faces: Vec<MeshFace>,
    /// Collision shapes.
    pub proxies: Vec<Proxy>,
    /// Sockets.
    pub sockets: Vec<Socket>,
    /// Names of the material slots the faces use.
    pub slots: Vec<String>,
}

/// What an asset is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MeshKind {
    /// A parametric arch (span, rise, thickness, profile).
    Arch(ArchSpec),
    /// A column with a plinth and a capital.
    Column(ColumnSpec),
    /// A scatter of low-poly stones.
    Rubble(RubbleSpec),
    /// A thick deck between two supports.
    Span(SpanSpec),
    /// Geometry imported from an OBJ file.
    Imported(ImportedMesh),
}

/// A mesh asset: a named recipe or piece of geometry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshAsset {
    /// Name shown in the palette and the MCP.
    pub name: String,
    /// What it is.
    pub kind: MeshKind,
}

impl MeshAsset {
    /// A generated or imported asset.
    pub fn new(name: impl Into<String>, kind: MeshKind) -> Self {
        Self {
            name: name.into(),
            kind,
        }
    }

    /// Build the faces, collision shapes and sockets, or say why the recipe
    /// is not buildable.
    pub fn build(&self) -> Result<BuiltMesh, String> {
        let built = match &self.kind {
            MeshKind::Arch(spec) => spec.build()?,
            MeshKind::Column(spec) => spec.build()?,
            MeshKind::Rubble(spec) => spec.build()?,
            MeshKind::Span(spec) => spec.build()?,
            MeshKind::Imported(mesh) => mesh.build()?,
        };
        if built.faces.len() > MAX_MESH_FACES {
            return Err(format!(
                "{}: {} faces, at most {MAX_MESH_FACES}",
                self.name,
                built.faces.len()
            ));
        }
        if built.slots.len() > MAX_MESH_SLOTS {
            return Err(format!(
                "{}: {} material slots, at most {MAX_MESH_SLOTS}",
                self.name,
                built.slots.len()
            ));
        }
        Ok(built)
    }

    /// Short description of what the asset is.
    pub fn describe(&self) -> String {
        match &self.kind {
            MeshKind::Arch(s) => format!(
                "arch {}: span {}, rise {}, spring {}, thickness {}, depth {}, {} segment(s) per half{}",
                s.profile.label(),
                s.span,
                s.rise,
                s.spring,
                s.thickness,
                s.depth,
                s.segments,
                if s.perch { ", perch socket at the crown" } else { "" }
            ),
            MeshKind::Column(s) => format!(
                "column: radius {}, height {}, {} sides, plinth {}, capital {}",
                s.radius, s.height, s.sides, s.plinth, s.capital
            ),
            MeshKind::Rubble(s) => format!(
                "rubble: size {}, {} stone(s), seed {}",
                s.size, s.pieces, s.seed
            ),
            MeshKind::Span(s) => format!(
                "span: length {}, width {}, thickness {}, rise {}",
                s.length, s.width, s.thickness, s.rise
            ),
            MeshKind::Imported(m) => format!(
                "imported: {} face(s), {} slot(s){}",
                m.faces.len(),
                m.slot_names.len().max(1),
                if m.source.is_empty() {
                    String::new()
                } else {
                    format!(", from {}", m.source)
                }
            ),
        }
    }
}

fn yes() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

fn unit_scale() -> [u16; 3] {
    [SCALE_ONE; 3]
}

fn is_unit_scale(scale: &[u16; 3]) -> bool {
    *scale == [SCALE_ONE; 3]
}

/// One placed copy of an asset in a room.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridMesh {
    /// Stable id, unique in the world and never reused while the instance
    /// lives: saves, sockets and the MCP name an instance by it.
    pub id: u16,
    /// Index into the world's mesh assets.
    pub asset: u16,
    /// Position of the asset's origin, grid units: `x` and `z` on the world
    /// lattice, `y` relative to the room's `y_offset`.
    pub position: [i32; 3],
    /// `[yaw, pitch, roll]`, degrees, the plane convention.
    #[serde(default, skip_serializing_if = "is_default")]
    pub rotation: [i32; 3],
    /// Scale per axis, Q8 (256 is 1.0).
    #[serde(default = "unit_scale", skip_serializing_if = "is_unit_scale")]
    pub scale: [u16; 3],
    /// Material per slot of the asset (a missing or `None` entry is the
    /// untextured fallback).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub materials: Vec<Option<ResourceId>>,
    /// Drawn at the material tint, with no light baked into it.
    #[serde(default, skip_serializing_if = "is_default")]
    pub fullbright: bool,
    /// Collides (the asset's proxies cook). Off, the mesh is decoration only.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub collide: bool,
    /// Hidden instances stay in the document but draw and collide with
    /// nothing and spawn no sockets.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub enabled: bool,
}

impl GridMesh {
    /// A new instance of `asset` at `position`, unrotated and unscaled.
    pub fn new(id: u16, asset: u16, position: [i32; 3]) -> Self {
        Self {
            id,
            asset,
            position,
            rotation: [0; 3],
            scale: unit_scale(),
            materials: Vec::new(),
            fullbright: false,
            collide: true,
            enabled: true,
        }
    }

    /// The material of `slot`.
    pub fn material(&self, slot: u8) -> Option<ResourceId> {
        self.materials.get(usize::from(slot)).copied().flatten()
    }

    /// Set the material of `slot`, growing the list.
    pub fn set_material(&mut self, slot: usize, material: Option<ResourceId>) {
        if self.materials.len() <= slot {
            self.materials.resize(slot + 1, None);
        }
        self.materials[slot] = material;
        while self.materials.last() == Some(&None) {
            self.materials.pop();
        }
    }

    /// Clamp the scale and wrap the angles into the document's ranges.
    pub fn normalize(&mut self) {
        for s in &mut self.scale {
            *s = (*s).clamp(SCALE_RANGE.0, SCALE_RANGE.1);
        }
        for angle in &mut self.rotation {
            *angle = super::plane::wrap_degrees(*angle);
        }
    }

    /// Whether the scale is the same on every axis.
    pub fn uniform(&self) -> bool {
        self.scale[0] == self.scale[1] && self.scale[1] == self.scale[2]
    }

    /// `p` (asset frame) placed in the world, grid units, rounded once so
    /// neighbouring faces of one instance share corners exactly. `y` is
    /// relative to the room's offset.
    pub fn place(&self, p: [i32; 3]) -> [i32; 3] {
        let v = self.turn(self.scaled(p));
        core::array::from_fn(|i| self.position[i] + v[i].round() as i32)
    }

    fn scaled(&self, p: [i32; 3]) -> [f64; 3] {
        core::array::from_fn(|i| f64::from(p[i]) * f64::from(self.scale[i]) / f64::from(SCALE_ONE))
    }

    /// Roll about z, pitch about x (positive tilts the front up), then yaw
    /// about the vertical: the plane convention (`GridPlane::axes`).
    fn turn(&self, v: [f64; 3]) -> [f64; 3] {
        let [yaw, pitch, roll] = self.rotation.map(|d| f64::from(d).to_radians());
        let (sr, cr) = roll.sin_cos();
        let (sp, cp) = pitch.sin_cos();
        let (sy, cy) = yaw.sin_cos();
        let v = [v[0] * cr - v[1] * sr, v[0] * sr + v[1] * cr, v[2]];
        let v = [v[0], v[1] * cp + v[2] * sp, -v[1] * sp + v[2] * cp];
        [v[0] * cy + v[2] * sy, v[1], -v[0] * sy + v[2] * cy]
    }

    /// A direction (asset frame) turned like the instance, unit length kept.
    pub fn turn_dir(&self, d: [f64; 3]) -> [f64; 3] {
        self.turn(d)
    }

    /// Heading of the instance about the vertical, degrees.
    pub fn yaw(&self) -> i32 {
        self.rotation[0]
    }

    /// Whether the instance is turned only about the vertical, by a multiple
    /// of 90 degrees (boxes stay boxes).
    pub fn quarter_turned(&self) -> bool {
        self.rotation[1] == 0 && self.rotation[2] == 0 && self.rotation[0].rem_euclid(90) == 0
    }

    /// Whether the instance stands upright (no pitch or roll).
    pub fn upright(&self) -> bool {
        self.rotation[1] == 0 && self.rotation[2] == 0
    }

    /// Box around the placed faces, `(min, max)`, grid units (room-relative
    /// `y`), or `None` when the asset does not build or has no faces.
    pub fn bounds(&self, built: &BuiltMesh) -> Option<([i32; 3], [i32; 3])> {
        let mut lo = [i32::MAX; 3];
        let mut hi = [i32::MIN; 3];
        for face in &built.faces {
            for p in face.ring() {
                let q = self.place(*p);
                for i in 0..3 {
                    lo[i] = lo[i].min(q[i]);
                    hi[i] = hi[i].max(q[i]);
                }
            }
        }
        (lo[0] != i32::MAX).then_some((lo, hi))
    }

    /// The placed footprint: the asset's plan bounds, turned and moved, as four
    /// `[x, z]` corners in order (grid units). The Top view and `grid_plan`
    /// draw it; `None` for an asset with no faces.
    pub fn footprint(&self, built: &BuiltMesh) -> Option<[[i32; 2]; 4]> {
        let mut lo = [i32::MAX; 2];
        let mut hi = [i32::MIN; 2];
        for face in &built.faces {
            for p in face.ring() {
                lo = [lo[0].min(p[0]), lo[1].min(p[2])];
                hi = [hi[0].max(p[0]), hi[1].max(p[2])];
            }
        }
        if lo[0] == i32::MAX {
            return None;
        }
        let at = |x: i32, z: i32| {
            let g = self.place([x, 0, z]);
            [g[0], g[2]]
        };
        Some([
            at(lo[0], lo[1]),
            at(hi[0], lo[1]),
            at(hi[0], hi[1]),
            at(lo[0], hi[1]),
        ])
    }

    /// The eight corners of the asset's bounding box, placed: the four at the
    /// bottom (`y` lowest) then the four at the top, each ring in the same
    /// order. For overlays and picking; `None` for an asset with no faces.
    pub fn oriented_box(&self, built: &BuiltMesh) -> Option<[[i32; 3]; 8]> {
        let mut lo = [i32::MAX; 3];
        let mut hi = [i32::MIN; 3];
        for face in &built.faces {
            for p in face.ring() {
                for i in 0..3 {
                    lo[i] = lo[i].min(p[i]);
                    hi[i] = hi[i].max(p[i]);
                }
            }
        }
        if lo[0] == i32::MAX {
            return None;
        }
        let at = |x: i32, y: i32, z: i32| self.place([x, y, z]);
        Some([
            at(lo[0], lo[1], lo[2]),
            at(hi[0], lo[1], lo[2]),
            at(hi[0], lo[1], hi[2]),
            at(lo[0], lo[1], hi[2]),
            at(lo[0], hi[1], lo[2]),
            at(hi[0], hi[1], lo[2]),
            at(hi[0], hi[1], hi[2]),
            at(lo[0], hi[1], hi[2]),
        ])
    }

    /// World sector rectangle `(min, max)`, inclusive, the instance's box
    /// overlaps (a box ending exactly on a sector edge does not reach the
    /// next sector).
    pub fn cell_span(&self, built: &BuiltMesh) -> ([i32; 2], [i32; 2]) {
        let sector = super::units::SECTOR_UNITS;
        match self.bounds(built) {
            Some((lo, hi)) => {
                let first = |v: i32| v.div_euclid(sector);
                let last = |lo: i32, hi: i32| (hi - 1).div_euclid(sector).max(first(lo));
                (
                    [first(lo[0]), first(lo[2])],
                    [last(lo[0], hi[0]), last(lo[2], hi[2])],
                )
            }
            None => {
                let c = self.center_cell();
                (c, c)
            }
        }
    }

    /// World sector holding the origin.
    pub fn center_cell(&self) -> [i32; 2] {
        let sector = super::units::SECTOR_UNITS;
        [
            self.position[0].div_euclid(sector),
            self.position[2].div_euclid(sector),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: [i32; 3], b: [i32; 3]) -> bool {
        a == b
    }

    #[test]
    fn an_unturned_instance_only_moves_the_asset() {
        let m = GridMesh::new(1, 0, [1000, 256, 2000]);
        assert!(near(m.place([10, 20, 30]), [1010, 276, 2030]));
    }

    #[test]
    fn yaw_turns_about_the_vertical_like_a_plane() {
        let mut m = GridMesh::new(1, 0, [0, 0, 0]);
        m.rotation = [90, 0, 0];
        // +x turns toward -z, +z toward +x (GridPlane::axes at yaw 90).
        assert!(near(m.place([100, 0, 0]), [0, 0, -100]));
        assert!(near(m.place([0, 0, 100]), [100, 0, 0]));
        assert!(m.quarter_turned());
        m.rotation = [45, 0, 0];
        assert!(!m.quarter_turned());
    }

    #[test]
    fn scale_is_per_axis_q8() {
        let mut m = GridMesh::new(1, 0, [0, 0, 0]);
        m.scale = [512, 128, 256];
        assert!(near(m.place([100, 100, 100]), [200, 50, 100]));
        assert!(!m.uniform());
    }

    #[test]
    fn materials_trim_trailing_nones() {
        let mut m = GridMesh::new(1, 0, [0; 3]);
        m.set_material(2, Some(ResourceId(7)));
        assert_eq!(m.materials.len(), 3);
        assert_eq!(m.material(2), Some(ResourceId(7)));
        assert_eq!(m.material(0), None);
        m.set_material(2, None);
        assert!(m.materials.is_empty());
    }

    #[test]
    fn the_document_form_round_trips_and_omits_defaults() {
        let m = GridMesh::new(3, 1, [1024, 0, 2048]);
        let text = ron::to_string(&m).unwrap();
        assert!(
            !text.contains("rotation") && !text.contains("scale"),
            "{text}"
        );
        assert_eq!(ron::from_str::<GridMesh>(&text).unwrap(), m);
    }
}
