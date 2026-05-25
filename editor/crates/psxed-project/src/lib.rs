//! Editor-side project model for PSoXide.
//!
//! This is the authoring model, not the final runtime layout. It keeps a
//! Godot-style scene tree and resource list so the editor can stay pleasant,
//! then later cooker stages flatten it into PS1-friendly world surfaces,
//! texture pages, entity spawns, and engine data.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use ron::ser::PrettyConfig;
use serde::{Deserialize, Serialize};

pub mod model_import;
pub mod playtest;
pub mod portal_rooms;
pub mod resolve;
pub mod room_connections;
pub mod spatial;
pub mod streaming;
pub mod texture_import;
pub mod tr_level;
pub mod world_cook;

/// Embedded copy of the default project's RON, baked at compile
/// time so the editor binary always carries a working starter even
/// if `editor/projects/default/` is absent at runtime. Single source
/// of truth -- edits to the on-disk file propagate to `starter()` on
/// the next build.
const DEFAULT_PROJECT_RON: &str = include_str!("../../../projects/default/project.ron");

/// Source-tree projects directory: `editor/projects/`.
///
/// Captured via `env!("CARGO_MANIFEST_DIR")` at compile time, so it
/// resolves wherever cargo built this crate from. Works for the
/// dev workflow (`cargo run -p frontend` from anywhere in the
/// repo); will need a different strategy when the editor ever
/// ships as a standalone binary.
pub fn projects_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("projects")
}

/// Filesystem-safe stem derived from a project display name.
///
/// The editor keeps `ProjectDocument::name` as the user-facing source
/// of truth, then uses this helper for generated directories and EXE
/// filenames. It intentionally stays ASCII-only because the PSX build
/// output and launcher paths benefit from boring, portable names.
pub fn project_file_stem(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "project".to_string()
    } else {
        trimmed
    }
}

/// Default project directory (`editor/projects/default/`). Always
/// present in the source tree; user "New Project" copies its
/// contents into a sibling directory.
pub fn default_project_dir() -> PathBuf {
    projects_dir().join("default")
}

/// Enumerate every directory under [`projects_dir`] that contains a
/// `project.ron`. Cheap directory walk, used by the editor's open /
/// switch flow once that lands. Returns an empty Vec rather than
/// erroring when `projects_dir` doesn't exist -- fresh checkout
/// before the dev runs the editor once.
pub fn list_projects() -> std::io::Result<Vec<PathBuf>> {
    let root = projects_dir();
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() && path.join("project.ron").is_file() {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

pub use world_cook::{
    cook_world_grid, encode_world_grid_psxw, CookedGridHorizontalFace, CookedGridSector,
    CookedGridVerticalFace, CookedGridWalls, CookedWorldGrid, CookedWorldMaterial,
    WorldGridCookError, WorldGridFaceKind,
};

/// Errors raised while reading or writing editor project documents.
#[derive(Debug)]
pub enum ProjectIoError {
    /// Filesystem error.
    Io(std::io::Error),
    /// RON parse error.
    Parse(ron::error::SpannedError),
    /// RON serialization error.
    Serialize(ron::Error),
}

impl std::fmt::Display for ProjectIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "filesystem error: {error}"),
            Self::Parse(error) => write!(f, "project parse error: {error}"),
            Self::Serialize(error) => write!(f, "project serialization error: {error}"),
        }
    }
}

impl std::error::Error for ProjectIoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Parse(error) => Some(error),
            Self::Serialize(error) => Some(error),
        }
    }
}

impl From<std::io::Error> for ProjectIoError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ron::error::SpannedError> for ProjectIoError {
    fn from(error: ron::error::SpannedError) -> Self {
        Self::Parse(error)
    }
}

impl From<ron::Error> for ProjectIoError {
    fn from(error: ron::Error) -> Self {
        Self::Serialize(error)
    }
}

/// Stable identifier for a node inside one scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(u64);

impl NodeId {
    /// The root node id every scene starts with.
    pub const ROOT: Self = Self(1);

    /// Return the raw integer value for compact UI/debug display.
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Stable identifier for a project resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResourceId(u64);

impl ResourceId {
    /// Return the raw integer value for compact UI/debug display.
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Explicit material assignment for one floor/ceiling triangle.
/// Missing override means "inherit the parent face material"; this
/// enum represents the two explicit states a triangle can choose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GridTriangleMaterialOverride {
    /// The triangle is intentionally unassigned even if the parent
    /// face has a material.
    Unassigned,
    /// The triangle uses this material instead of the parent face.
    Resource(ResourceId),
}

impl GridTriangleMaterialOverride {
    pub const fn from_material(material: Option<ResourceId>) -> Self {
        match material {
            Some(id) => Self::Resource(id),
            None => Self::Unassigned,
        }
    }

    pub const fn material(self) -> Option<ResourceId> {
        match self {
            Self::Unassigned => None,
            Self::Resource(id) => Some(id),
        }
    }
}

/// Basic 3D transform used by authored nodes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transform3 {
    /// World/local translation in editor units.
    pub translation: [f32; 3],
    /// Euler rotation in degrees, matching common editor UI language.
    pub rotation_degrees: [f32; 3],
    /// Per-axis scale.
    pub scale: [f32; 3],
}

impl Default for Transform3 {
    fn default() -> Self {
        Self {
            translation: [0.0, 0.0, 0.0],
            rotation_degrees: [0.0, 0.0, 0.0],
            scale: [1.0, 1.0, 1.0],
        }
    }
}

/// PS1 semi-transparency mode exposed at editor level.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PsxBlendMode {
    /// Opaque textured or flat surface.
    #[default]
    Opaque,
    /// `(background + foreground) / 2`.
    Average,
    /// `background + foreground`, clamped per channel.
    Add,
    /// `background - foreground`, clamped per channel.
    Subtract,
    /// `background + foreground / 4`, clamped per channel.
    AddQuarter,
}

impl PsxBlendMode {
    /// User-facing label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Opaque => "Opaque",
            Self::Average => "Average",
            Self::Add => "Add",
            Self::Subtract => "Subtract",
            Self::AddQuarter => "Add Quarter",
        }
    }
}

/// Which side of an authored face should render.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaterialFaceSidedness {
    /// Render the face's authored/front winding only.
    Front,
    /// Render only the opposite side.
    Back,
    /// Render both sides.
    #[default]
    Both,
}

impl MaterialFaceSidedness {
    /// User-facing label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Front => "Front",
            Self::Back => "Back",
            Self::Both => "Both",
        }
    }

    /// Convert the old checkbox value into the new enum.
    pub const fn from_double_sided(double_sided: bool) -> Self {
        if double_sided {
            Self::Both
        } else {
            Self::Front
        }
    }
}

/// Authoring material. The cooker maps this to runtime texture/material state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterialResource {
    /// Source texture resource, if any.
    pub texture: Option<ResourceId>,
    /// PS1 blend mode.
    pub blend_mode: PsxBlendMode,
    /// Texture modulation tint. `0x80` is neutral for PS1 textured polys.
    pub tint: [u8; 3],
    /// Which side(s) of faces using this material should render.
    #[serde(default)]
    pub face_sidedness: MaterialFaceSidedness,
    /// Legacy project field. New code reads/writes
    /// [`face_sidedness`](Self::face_sidedness); this remains so older
    /// `.ron` projects migrate without losing their two-sided setting.
    #[serde(default)]
    pub double_sided: bool,
}

/// Default authored width/height for image props, in engine/editor units.
pub const DEFAULT_IMAGE_PROP_SIZE: u16 = DEFAULT_WORLD_SECTOR_SIZE as u16;

const fn default_image_prop_size() -> u16 {
    DEFAULT_IMAGE_PROP_SIZE
}

/// Default authored collision-box full-size (width / height / depth)
/// for an image prop. Sized to match `DEFAULT_IMAGE_PROP_SIZE` so a
/// fresh prop with collision toggled on has a sensible cube around
/// its visible plane.
const fn default_image_prop_collision_size() -> [u16; 3] {
    [
        DEFAULT_IMAGE_PROP_SIZE,
        DEFAULT_IMAGE_PROP_SIZE,
        DEFAULT_IMAGE_PROP_SIZE,
    ]
}

/// Face slots on an authored boxed prop.
pub const BOX_PROP_FACE_COUNT: usize = 6;
/// Editable vertex count on an authored boxed prop.
pub const BOX_PROP_VERTEX_COUNT: usize = 8;
/// Default authored cube size for boxed props, in engine/editor units.
pub const DEFAULT_BOX_PROP_SIZE: u16 = DEFAULT_WORLD_SECTOR_SIZE as u16;

/// User-facing face order for boxed prop material slots.
pub const BOX_PROP_FACE_NAMES: [&str; BOX_PROP_FACE_COUNT] =
    ["Front", "Right", "Back", "Left", "Top", "Bottom"];

const fn default_box_prop_materials() -> [Option<ResourceId>; BOX_PROP_FACE_COUNT] {
    [None; BOX_PROP_FACE_COUNT]
}

const fn default_box_prop_vertices() -> [[i16; 3]; BOX_PROP_VERTEX_COUNT] {
    box_prop_vertices_for_size(DEFAULT_BOX_PROP_SIZE)
}

/// Build the default bottom-anchored cube vertices for a boxed prop.
pub const fn box_prop_vertices_for_size(size: u16) -> [[i16; 3]; BOX_PROP_VERTEX_COUNT] {
    let half = (size / 2) as i16;
    let height = size as i16;
    [
        [-half, 0, -half],
        [half, 0, -half],
        [half, 0, half],
        [-half, 0, half],
        [-half, height, -half],
        [half, height, -half],
        [half, height, half],
        [-half, height, half],
    ]
}

impl MaterialResource {
    /// Build an opaque neutral material.
    pub const fn opaque(texture: Option<ResourceId>) -> Self {
        Self {
            texture,
            blend_mode: PsxBlendMode::Opaque,
            tint: [0x80, 0x80, 0x80],
            face_sidedness: MaterialFaceSidedness::Both,
            double_sided: true,
        }
    }

    /// Build a translucent neutral material.
    pub const fn translucent(texture: Option<ResourceId>, blend_mode: PsxBlendMode) -> Self {
        Self {
            texture,
            blend_mode,
            tint: [0x80, 0x80, 0x80],
            face_sidedness: MaterialFaceSidedness::Both,
            double_sided: true,
        }
    }

    /// Resolved sidedness. Missing `face_sidedness` defaults to
    /// `Both` so old projects keep matching the editor preview, while
    /// legacy `double_sided = true` still upgrades an explicit/front
    /// value to two-sided.
    pub const fn sidedness(&self) -> MaterialFaceSidedness {
        if self.double_sided && matches!(self.face_sidedness, MaterialFaceSidedness::Front) {
            MaterialFaceSidedness::Both
        } else {
            self.face_sidedness
        }
    }

    /// Keep the legacy field aligned after editing `face_sidedness`.
    pub fn sync_legacy_sidedness(&mut self) {
        self.double_sided = matches!(self.face_sidedness, MaterialFaceSidedness::Both);
    }
}

/// World-grid diagonal split.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GridSplit {
    /// Split from north-west to south-east.
    #[default]
    NorthWestSouthEast,
    /// Split from north-east to south-west.
    NorthEastSouthWest,
}

impl GridSplit {
    /// Stored `.psxw` split id for this logical diagonal.
    pub const fn psxw_id(self) -> u8 {
        match self {
            Self::NorthWestSouthEast => psxed_format::world::split::NORTH_WEST_SOUTH_EAST,
            Self::NorthEastSouthWest => psxed_format::world::split::NORTH_EAST_SOUTH_WEST,
        }
    }
}

/// Texture rotation preset for authored grid faces.
///
/// PS1 textured polygons carry per-corner 8-bit UVs, not a texture
/// matrix, so these rotations are represented by rewriting the UVs
/// sent with each face.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GridUvRotation {
    /// No texture rotation.
    #[default]
    Deg0,
    /// Rotate texture coordinates 45 degrees clockwise on the face.
    Deg45,
    /// Rotate texture coordinates 90 degrees clockwise on the face.
    Deg90,
    /// Rotate texture coordinates 135 degrees clockwise on the face.
    Deg135,
    /// Rotate texture coordinates 180 degrees.
    Deg180,
    /// Rotate texture coordinates 225 degrees clockwise on the face.
    Deg225,
    /// Rotate texture coordinates 270 degrees clockwise on the face.
    Deg270,
    /// Rotate texture coordinates 315 degrees clockwise on the face.
    Deg315,
}

/// Non-destructive texture-coordinate transform for one grid face.
///
/// `offset` is in PS1 texels and is applied after flip/rotation. It
/// wraps in the 8-bit UV coordinate space, which matches packet-level
/// PS1 UVs; runtime room materials use texture-window state so this
/// can repeat a compact material tile without rebaking the texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridUvTransform {
    /// Signed `[u, v]` texel offset.
    #[serde(default)]
    pub offset: [i16; 2],
    /// Optional `[u, v]` UV span in texels. Zero means "use the
    /// source quad's native span" for that axis.
    #[serde(default, skip_serializing_if = "is_default_uv_span")]
    pub span: [u16; 2],
    /// Texture rotation preset.
    #[serde(default)]
    pub rotation: GridUvRotation,
    /// Mirror horizontally before rotation.
    #[serde(default)]
    pub flip_u: bool,
    /// Mirror vertically before rotation.
    #[serde(default)]
    pub flip_v: bool,
}

impl GridUvTransform {
    /// Identity transform.
    pub const IDENTITY: Self = Self {
        offset: [0, 0],
        span: [0, 0],
        rotation: GridUvRotation::Deg0,
        flip_u: false,
        flip_v: false,
    };

    /// `true` when this transform leaves UVs unchanged.
    pub const fn is_identity(&self) -> bool {
        self.offset[0] == 0
            && self.offset[1] == 0
            && self.span[0] == 0
            && self.span[1] == 0
            && matches!(self.rotation, GridUvRotation::Deg0)
            && !self.flip_u
            && !self.flip_v
    }

    /// Apply the transform to a quad's corner UVs.
    ///
    /// The input order can be any perimeter order (`[NW, NE, SE, SW]`
    /// for floors or `[BL, BR, TR, TL]` for walls); the transform is
    /// computed inside the UV rectangle spanned by those four points.
    pub fn apply_to_quad(self, uvs: [(u8, u8); 4]) -> [(u8, u8); 4] {
        if self.is_identity() {
            return uvs;
        }
        let bounds = uv_bounds(uvs);
        [
            self.apply_one(uvs[0], bounds),
            self.apply_one(uvs[1], bounds),
            self.apply_one(uvs[2], bounds),
            self.apply_one(uvs[3], bounds),
        ]
    }

    fn apply_one(self, uv: (u8, u8), bounds: UvBounds) -> (u8, u8) {
        let width = bounds.max_u - bounds.min_u;
        let height = bounds.max_v - bounds.min_v;
        if width == 0 || height == 0 {
            return (
                wrap_uv(uv.0 as i32 + self.offset[0] as i32),
                wrap_uv(uv.1 as i32 + self.offset[1] as i32),
            );
        }

        let mut u = uv.0 as i32 - bounds.min_u;
        let mut v = uv.1 as i32 - bounds.min_v;
        if self.flip_u {
            u = width - u;
        }
        if self.flip_v {
            v = height - v;
        }

        let (u, v) = match self.rotation {
            GridUvRotation::Deg0 => (u, v),
            GridUvRotation::Deg45 => rotate_uv_diagonal_fit(u, v, width, height, 1),
            GridUvRotation::Deg90 => (
                width - scale_rounded(v, width, height),
                scale_rounded(u, height, width),
            ),
            GridUvRotation::Deg135 => rotate_uv_diagonal_fit(u, v, width, height, 3),
            GridUvRotation::Deg180 => (width - u, height - v),
            GridUvRotation::Deg225 => rotate_uv_diagonal_fit(u, v, width, height, 5),
            GridUvRotation::Deg270 => (
                scale_rounded(v, width, height),
                height - scale_rounded(u, height, width),
            ),
            GridUvRotation::Deg315 => rotate_uv_diagonal_fit(u, v, width, height, 7),
        };
        let span_u = self.effective_span_axis(0, width);
        let span_v = self.effective_span_axis(1, height);
        let u = scale_rounded(u, span_u, width);
        let v = scale_rounded(v, span_v, height);

        (
            wrap_uv(bounds.min_u + u + self.offset[0] as i32),
            wrap_uv(bounds.min_v + v + self.offset[1] as i32),
        )
    }

    fn effective_span_axis(self, axis: usize, fallback: i32) -> i32 {
        let span = self.span[axis];
        if span == 0 {
            fallback
        } else {
            i32::from(span.min(255))
        }
    }
}

impl Default for GridUvTransform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[derive(Debug, Clone, Copy)]
struct UvBounds {
    min_u: i32,
    max_u: i32,
    min_v: i32,
    max_v: i32,
}

fn uv_bounds(uvs: [(u8, u8); 4]) -> UvBounds {
    let mut min_u = uvs[0].0 as i32;
    let mut max_u = min_u;
    let mut min_v = uvs[0].1 as i32;
    let mut max_v = min_v;
    for (u, v) in uvs {
        let u = u as i32;
        let v = v as i32;
        min_u = min_u.min(u);
        max_u = max_u.max(u);
        min_v = min_v.min(v);
        max_v = max_v.max(v);
    }
    UvBounds {
        min_u,
        max_u,
        min_v,
        max_v,
    }
}

fn scale_rounded(value: i32, numerator: i32, denominator: i32) -> i32 {
    if denominator == 0 {
        0
    } else {
        (value.saturating_mul(numerator) + denominator / 2) / denominator
    }
}

fn signed_div_round(value: i32, denominator: i32) -> i32 {
    if denominator == 0 {
        0
    } else if value >= 0 {
        (value + denominator / 2) / denominator
    } else {
        (value - denominator / 2) / denominator
    }
}

fn rotate_uv_diagonal_fit(
    u: i32,
    v: i32,
    width: i32,
    height: i32,
    clockwise_steps: u8,
) -> (i32, i32) {
    const Q: i32 = 4096;
    const HALF_Q: i32 = Q / 2;

    let du = signed_div_round((u.saturating_mul(2) - width).saturating_mul(Q), width);
    let dv = signed_div_round((v.saturating_mul(2) - height).saturating_mul(Q), height);
    let (cos_q, sin_q) = match clockwise_steps & 7 {
        1 => (HALF_Q, HALF_Q),
        3 => (-HALF_Q, HALF_Q),
        5 => (-HALF_Q, -HALF_Q),
        7 => (HALF_Q, -HALF_Q),
        _ => (Q, 0),
    };
    let rotated_u = signed_div_round(
        cos_q
            .saturating_mul(du)
            .saturating_sub(sin_q.saturating_mul(dv)),
        Q,
    );
    let rotated_v = signed_div_round(
        sin_q
            .saturating_mul(du)
            .saturating_add(cos_q.saturating_mul(dv)),
        Q,
    );

    (
        signed_div_round((rotated_u + Q).saturating_mul(width), Q * 2),
        signed_div_round((rotated_v + Q).saturating_mul(height), Q * 2),
    )
}

fn wrap_uv(value: i32) -> u8 {
    value.rem_euclid(256) as u8
}

pub(crate) fn wrap_tiled_uv_offset_i16(value: i64) -> i16 {
    value.rem_euclid(i64::from(psxed_format::world::TILE_UV)) as i16
}

const fn is_default_uv_span(span: &[u16; 2]) -> bool {
    span[0] == 0 && span[1] == 0
}

/// Optional authored overrides for one half of a split floor or
/// ceiling face. Every field inherits from the parent face when
/// `None`, keeping old projects compact and behavior-compatible.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridHorizontalTriangleOverride {
    /// Optional material override. `None` inherits the parent face.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<GridTriangleMaterialOverride>,
    /// Optional UV override. `None` inherits the parent face UV.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uv: Option<GridUvTransform>,
    /// Optional walkability override. `None` inherits the parent face.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub walkable: Option<bool>,
    /// Optional triangle-local heights in that triangle's corner
    /// order. `None` inherits the parent face corner heights. This
    /// keeps the common quad case compact while allowing rare
    /// split-height triangles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heights: Option<[i32; 3]>,
}

impl GridHorizontalTriangleOverride {
    pub const fn is_empty(&self) -> bool {
        self.material.is_none()
            && self.uv.is_none()
            && self.walkable.is_none()
            && self.heights.is_none()
    }
}

/// Optional overrides for the two triangles emitted by a
/// floor/ceiling split. `a` and `b` match the triangle order used by
/// the editor/runtime split tables.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridHorizontalTriangleOverrides {
    #[serde(
        default,
        skip_serializing_if = "GridHorizontalTriangleOverride::is_empty"
    )]
    pub a: GridHorizontalTriangleOverride,
    #[serde(
        default,
        skip_serializing_if = "GridHorizontalTriangleOverride::is_empty"
    )]
    pub b: GridHorizontalTriangleOverride,
}

impl GridHorizontalTriangleOverrides {
    pub const fn is_empty(&self) -> bool {
        self.a.is_empty() && self.b.is_empty()
    }

    pub const fn get(&self, index: usize) -> &GridHorizontalTriangleOverride {
        if index == 0 {
            &self.a
        } else {
            &self.b
        }
    }

    pub const fn get_mut(&mut self, index: usize) -> &mut GridHorizontalTriangleOverride {
        if index == 0 {
            &mut self.a
        } else {
            &mut self.b
        }
    }
}

/// Floor / ceiling corner index. Maps directly to the
/// `[NW, NE, SE, SW]` order every height array uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Corner {
    NW,
    NE,
    SE,
    SW,
}

impl Corner {
    /// Index into `[NW, NE, SE, SW]`.
    pub const fn idx(self) -> usize {
        match self {
            Self::NW => 0,
            Self::NE => 1,
            Self::SE => 2,
            Self::SW => 3,
        }
    }

    /// Convert a `[NW, NE, SE, SW]` index to a corner. Unknown
    /// indices fall back to `NW`.
    pub const fn from_idx(index: usize) -> Self {
        match index {
            1 => Self::NE,
            2 => Self::SE,
            3 => Self::SW,
            _ => Self::NW,
        }
    }

    /// Diagonal-opposite corner. NW ↔ SE, NE ↔ SW. Used by the
    /// vertex-delete pinch flow to find which neighbour the
    /// dropped corner welds to.
    pub const fn diagonal(self) -> Self {
        match self {
            Self::NW => Self::SE,
            Self::NE => Self::SW,
            Self::SE => Self::NW,
            Self::SW => Self::NE,
        }
    }

    /// Diagonal split that keeps a triangle alive when this
    /// corner is dropped. Drop NE / SW → NW-SE keeps one half;
    /// drop NW / SE → NE-SW keeps one half. Picking the *other*
    /// diagonal would put the dropped corner on the cut line,
    /// killing both triangles.
    pub const fn surviving_split(self) -> GridSplit {
        match self {
            Self::NE | Self::SW => GridSplit::NorthWestSouthEast,
            Self::NW | Self::SE => GridSplit::NorthEastSouthWest,
        }
    }
}

/// Wall corner index. Maps to the
/// `[bottom-left, bottom-right, top-right, top-left]` order in
/// every wall heights array.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WallCorner {
    BL,
    BR,
    TR,
    TL,
}

impl WallCorner {
    pub const fn idx(self) -> usize {
        match self {
            Self::BL => 0,
            Self::BR => 1,
            Self::TR => 2,
            Self::TL => 3,
        }
    }

    /// Convert a `[BL, BR, TR, TL]` index to a wall corner. Unknown
    /// indices fall back to `BL`.
    pub const fn from_idx(index: usize) -> Self {
        match index {
            1 => Self::BR,
            2 => Self::TR,
            3 => Self::TL,
            _ => Self::BL,
        }
    }

    /// `true` when this corner sits at the wall's bottom.
    pub const fn is_bottom(self) -> bool {
        matches!(self, Self::BL | Self::BR)
    }
}

/// Corner members for one authored horizontal split triangle.
pub const fn horizontal_triangle_corners(split: GridSplit, triangle_index: usize) -> [Corner; 3] {
    let corners = psxed_format::world::topology::split_triangle(split.psxw_id(), triangle_index);
    [
        Corner::from_idx(corners[0]),
        Corner::from_idx(corners[1]),
        Corner::from_idx(corners[2]),
    ]
}

/// `true` when an authored horizontal split triangle contains
/// `corner`.
pub const fn horizontal_triangle_contains_corner(
    split: GridSplit,
    triangle_index: usize,
    corner: Corner,
) -> bool {
    psxed_format::world::topology::triangle_contains_corner(
        psxed_format::world::topology::split_triangle(split.psxw_id(), triangle_index),
        corner.idx(),
    )
}

/// Wall-corner members for one authored wall split triangle. The
/// corner order is `[BL, BR, TR, TL]`.
pub const fn wall_triangle_corners(split: GridSplit, triangle_index: usize) -> [WallCorner; 3] {
    let corners = psxed_format::world::topology::split_triangle(split.psxw_id(), triangle_index);
    [
        WallCorner::from_idx(corners[0]),
        WallCorner::from_idx(corners[1]),
        WallCorner::from_idx(corners[2]),
    ]
}

/// Shape id produced by dropping an authored wall corner.
pub const fn wall_shape_for_dropped_corner(corner: WallCorner) -> u16 {
    psxed_format::world::topology::wall_shape_for_dropped_corner(corner.idx())
}

/// Wall-corner members for the single triangle surviving a wall shape.
pub const fn wall_shape_triangle_corners(shape: u16) -> Option<[WallCorner; 3]> {
    match psxed_format::world::topology::wall_shape_triangle_corners(shape) {
        Some(corners) => Some([
            WallCorner::from_idx(corners[0]),
            WallCorner::from_idx(corners[1]),
            WallCorner::from_idx(corners[2]),
        ]),
        None => None,
    }
}

/// Cardinal or diagonal grid edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GridDirection {
    /// Editor north edge, +Z.
    North,
    /// East edge, +X.
    East,
    /// Editor south edge, -Z.
    South,
    /// West edge, -X.
    West,
    /// Diagonal from north-west to south-east.
    NorthWestSouthEast,
    /// Diagonal from north-east to south-west.
    NorthEastSouthWest,
}

impl GridDirection {
    /// Cardinal directions in editor perimeter order.
    pub const CARDINAL: [Self; 4] = [Self::North, Self::East, Self::South, Self::West];

    /// Diagonal directions in editor split order.
    pub const DIAGONAL: [Self; 2] = [Self::NorthWestSouthEast, Self::NorthEastSouthWest];

    /// Every authored grid direction.
    pub const ALL: [Self; 6] = [
        Self::North,
        Self::East,
        Self::South,
        Self::West,
        Self::NorthWestSouthEast,
        Self::NorthEastSouthWest,
    ];

    /// `true` for the four perimeter edges.
    pub const fn is_cardinal(self) -> bool {
        matches!(self, Self::North | Self::East | Self::South | Self::West)
    }

    /// Opposite cardinal edge. Diagonals do not have a single
    /// opposite perimeter edge.
    pub const fn opposite_cardinal(self) -> Option<Self> {
        match self {
            Self::North => Some(Self::South),
            Self::East => Some(Self::West),
            Self::South => Some(Self::North),
            Self::West => Some(Self::East),
            Self::NorthWestSouthEast | Self::NorthEastSouthWest => None,
        }
    }

    /// Canonical physical edge claimed by this authored cardinal
    /// direction. Editor authoring uses North=+Z and South=-Z;
    /// this key lets opposing-cell wall claims collide without
    /// duplicating the convention in each caller.
    pub const fn physical_edge(self, x: u16, z: u16) -> Option<GridPhysicalEdge> {
        match self {
            Self::North => Some(GridPhysicalEdge {
                x: x as i32,
                z: z as i32 + 1,
                axis: GridEdgeAxis::EastWest,
            }),
            Self::South => Some(GridPhysicalEdge {
                x: x as i32,
                z: z as i32,
                axis: GridEdgeAxis::EastWest,
            }),
            Self::West => Some(GridPhysicalEdge {
                x: x as i32,
                z: z as i32,
                axis: GridEdgeAxis::NorthSouth,
            }),
            Self::East => Some(GridPhysicalEdge {
                x: x as i32 + 1,
                z: z as i32,
                axis: GridEdgeAxis::NorthSouth,
            }),
            Self::NorthWestSouthEast | Self::NorthEastSouthWest => None,
        }
    }
}

/// Axis of a canonical physical edge in editor cell coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GridEdgeAxis {
    /// Edge runs along Z, separating cells across X.
    NorthSouth,
    /// Edge runs along X, separating cells across Z.
    EastWest,
}

/// Canonical integer address of one physical cardinal wall edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GridPhysicalEdge {
    pub x: i32,
    pub z: i32,
    pub axis: GridEdgeAxis,
}

/// World-space X/Z bounds for one editor grid cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridCellBounds {
    pub x0: i32,
    pub x1: i32,
    pub z0: i32,
    pub z1: i32,
}

impl GridCellBounds {
    /// X/Z position of a horizontal face corner in editor
    /// convention: NW/NE live on the high-Z edge.
    pub const fn horizontal_corner_xz(self, corner: Corner) -> [i32; 2] {
        match corner {
            Corner::NW => [self.x0, self.z1],
            Corner::NE => [self.x1, self.z1],
            Corner::SE => [self.x1, self.z0],
            Corner::SW => [self.x0, self.z0],
        }
    }

    /// Wall bottom-edge endpoints `(BL, BR)` in editor convention.
    pub const fn wall_endpoints_xz(self, direction: GridDirection) -> Option<([i32; 2], [i32; 2])> {
        match direction {
            GridDirection::North => Some(([self.x0, self.z1], [self.x1, self.z1])),
            GridDirection::East => Some(([self.x1, self.z1], [self.x1, self.z0])),
            GridDirection::South => Some(([self.x1, self.z0], [self.x0, self.z0])),
            GridDirection::West => Some(([self.x0, self.z0], [self.x0, self.z1])),
            GridDirection::NorthWestSouthEast => Some(([self.x0, self.z1], [self.x1, self.z0])),
            GridDirection::NorthEastSouthWest => Some(([self.x1, self.z1], [self.x0, self.z0])),
        }
    }
}

/// Authored horizontal grid face.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridHorizontalFace {
    /// Corner heights `[NW, NE, SE, SW]` in engine world units.
    pub heights: [i32; 4],
    /// Diagonal split.
    pub split: GridSplit,
    /// Material used by the face.
    pub material: Option<ResourceId>,
    /// Non-destructive texture-coordinate transform.
    #[serde(default, skip_serializing_if = "GridUvTransform::is_identity")]
    pub uv: GridUvTransform,
    /// Whether character collision treats this face as walkable.
    pub walkable: bool,
    /// Optional per-triangle material / UV / walkability overrides.
    /// Empty by default, so old projects keep one surface record per
    /// floor/ceiling face until the user edits a specific triangle.
    #[serde(
        default,
        skip_serializing_if = "GridHorizontalTriangleOverrides::is_empty"
    )]
    pub triangle_overrides: GridHorizontalTriangleOverrides,
    /// `Some(corner)` when one corner has been deleted, turning
    /// the face into a triangle. The renderer skips the half
    /// containing the missing corner; `split` is forced to the
    /// surviving diagonal at edit time. Default `None` =
    /// authored as a normal quad.
    #[serde(default)]
    pub dropped_corner: Option<Corner>,
}

impl GridHorizontalFace {
    /// Flat face at `height`.
    pub const fn flat(height: i32, material: Option<ResourceId>) -> Self {
        Self {
            heights: [height, height, height, height],
            split: GridSplit::NorthWestSouthEast,
            material,
            uv: GridUvTransform::IDENTITY,
            walkable: true,
            triangle_overrides: GridHorizontalTriangleOverrides {
                a: GridHorizontalTriangleOverride {
                    material: None,
                    uv: None,
                    walkable: None,
                    heights: None,
                },
                b: GridHorizontalTriangleOverride {
                    material: None,
                    uv: None,
                    walkable: None,
                    heights: None,
                },
            },
            dropped_corner: None,
        }
    }

    pub const fn triangle_override(&self, index: usize) -> &GridHorizontalTriangleOverride {
        self.triangle_overrides.get(index)
    }

    pub const fn triangle_override_mut(
        &mut self,
        index: usize,
    ) -> &mut GridHorizontalTriangleOverride {
        self.triangle_overrides.get_mut(index)
    }

    pub const fn triangle_material(&self, index: usize) -> Option<ResourceId> {
        match self.triangle_override(index).material {
            Some(override_material) => override_material.material(),
            None => self.material,
        }
    }

    pub const fn triangle_uv(&self, index: usize) -> GridUvTransform {
        match self.triangle_override(index).uv {
            Some(uv) => uv,
            None => self.uv,
        }
    }

    pub const fn triangle_walkable(&self, index: usize) -> bool {
        match self.triangle_override(index).walkable {
            Some(walkable) => walkable,
            None => self.walkable,
        }
    }

    /// Triangle-local heights in the same corner order returned by
    /// [`horizontal_triangle_corners`].
    pub fn triangle_heights(&self, index: usize) -> [i32; 3] {
        if let Some(heights) = self.triangle_override(index).heights {
            return heights;
        }
        let corners = horizontal_triangle_corners(self.split, index);
        [
            self.heights[corners[0].idx()],
            self.heights[corners[1].idx()],
            self.heights[corners[2].idx()],
        ]
    }

    /// Materialize a triangle-local height override from the current
    /// parent face heights. Returns the mutable override array.
    pub fn triangle_heights_mut(&mut self, index: usize) -> &mut [i32; 3] {
        let inherited = self.triangle_heights(index);
        let target = self.triangle_override_mut(index);
        target.heights.get_or_insert(inherited)
    }

    /// Drop one corner -- the face becomes a visible triangle.
    /// Forces `split` to the diagonal that keeps a triangle
    /// alive (drop NE / SW → NW-SE; drop NW / SE → NE-SW). The
    /// dropped corner's stored height is left untouched so the
    /// user can recover by un-dropping.
    pub fn drop_corner(&mut self, corner: Corner) {
        self.dropped_corner = Some(corner);
        self.split = corner.surviving_split();
    }

    /// Restore the face to a full quad.
    pub fn restore_corner(&mut self) {
        self.dropped_corner = None;
    }

    /// Interpolated height at local sector coordinates in the
    /// editor grid convention. `local_z = 0` is the south / low-Z
    /// edge, while authored corners are stored as
    /// `[NW, NE, SE, SW]`.
    pub fn height_at_local(&self, local_x: i32, local_z: i32, sector_size: i32) -> i32 {
        let sector_size = sector_size.max(1);
        let [nw, ne, se, sw] = self.heights;
        // Reuse the same Z-flipped convention the cooker writes:
        // runtime local Z=0 corresponds to the editor's south edge.
        let runtime_heights = [sw, se, ne, nw];
        let runtime_split = match self.split {
            GridSplit::NorthWestSouthEast => GridSplit::NorthEastSouthWest,
            GridSplit::NorthEastSouthWest => GridSplit::NorthWestSouthEast,
        };
        let editor_index =
            horizontal_triangle_index_at_local(self.split, local_x, local_z, sector_size);
        if self.triangle_override(editor_index).heights.is_some() {
            let runtime_index = if editor_index == 0 { 1 } else { 0 };
            let runtime_triangle_heights = runtime_horizontal_triangle_heights(
                self,
                editor_index,
                runtime_split,
                runtime_index,
            );
            let heights = quad_heights_for_triangle(
                runtime_split,
                runtime_index,
                runtime_triangle_heights,
                runtime_heights,
            );
            return height_at_local_for_split(
                heights,
                runtime_split,
                local_x,
                local_z,
                sector_size,
            );
        }
        height_at_local_for_split(
            runtime_heights,
            runtime_split,
            local_x,
            local_z,
            sector_size,
        )
    }

    /// `true` when the face is currently a triangle.
    pub const fn is_triangle(&self) -> bool {
        self.dropped_corner.is_some()
    }
}

fn horizontal_triangle_index_at_local(
    split: GridSplit,
    local_x: i32,
    local_z: i32,
    sector_size: i32,
) -> usize {
    let sector_size = sector_size.max(1);
    let u = local_x.clamp(0, sector_size);
    let v = local_z.clamp(0, sector_size);
    match split {
        GridSplit::NorthWestSouthEast => {
            if u + v >= sector_size {
                0
            } else {
                1
            }
        }
        GridSplit::NorthEastSouthWest => {
            if v >= u {
                0
            } else {
                1
            }
        }
    }
}

fn runtime_horizontal_triangle_heights(
    face: &GridHorizontalFace,
    editor_index: usize,
    runtime_split: GridSplit,
    runtime_index: usize,
) -> [i32; 3] {
    let editor_corners = horizontal_triangle_corners(face.split, editor_index);
    let editor_heights = face.triangle_heights(editor_index);
    let mut editor_quad = face.heights;
    for (corner, height) in editor_corners.into_iter().zip(editor_heights) {
        editor_quad[corner.idx()] = height;
    }
    let runtime_quad = [
        editor_quad[Corner::SW.idx()],
        editor_quad[Corner::SE.idx()],
        editor_quad[Corner::NE.idx()],
        editor_quad[Corner::NW.idx()],
    ];
    let runtime_corners = horizontal_triangle_corners(runtime_split, runtime_index);
    [
        runtime_quad[runtime_corners[0].idx()],
        runtime_quad[runtime_corners[1].idx()],
        runtime_quad[runtime_corners[2].idx()],
    ]
}

fn quad_heights_for_triangle(
    split: GridSplit,
    index: usize,
    triangle_heights: [i32; 3],
    mut fallback: [i32; 4],
) -> [i32; 4] {
    let corners = horizontal_triangle_corners(split, index);
    for (corner, height) in corners.into_iter().zip(triangle_heights) {
        fallback[corner.idx()] = height;
    }
    fallback
}

fn height_at_local_for_split(
    heights: [i32; 4],
    split: GridSplit,
    local_x: i32,
    local_z: i32,
    sector_size: i32,
) -> i32 {
    let sector_size = sector_size.max(1);
    let u = local_x.clamp(0, sector_size);
    let v = local_z.clamp(0, sector_size);
    let [nw, ne, se, sw] = heights;
    match split {
        GridSplit::NorthWestSouthEast => {
            if v <= u {
                nw.saturating_add(mul_sector_i32(ne.saturating_sub(nw), u - v, sector_size))
                    .saturating_add(mul_sector_i32(se.saturating_sub(nw), v, sector_size))
            } else {
                nw.saturating_add(mul_sector_i32(se.saturating_sub(sw), u, sector_size))
                    .saturating_add(mul_sector_i32(sw.saturating_sub(nw), v, sector_size))
            }
        }
        GridSplit::NorthEastSouthWest => {
            if u + v <= sector_size {
                nw.saturating_add(mul_sector_i32(ne.saturating_sub(nw), u, sector_size))
                    .saturating_add(mul_sector_i32(sw.saturating_sub(nw), v, sector_size))
            } else {
                sw.saturating_add(mul_sector_i32(se.saturating_sub(sw), u, sector_size))
                    .saturating_add(mul_sector_i32(
                        ne.saturating_sub(se),
                        sector_size - v,
                        sector_size,
                    ))
            }
        }
    }
}

fn mul_sector_i32(delta: i32, amount: i32, sector_size: i32) -> i32 {
    if sector_size <= 0 {
        0
    } else {
        delta.saturating_mul(amount) / sector_size
    }
}

/// Authored vertical grid wall.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridVerticalFace {
    /// Corner heights `[bottom-left, bottom-right, top-right, top-left]`.
    pub heights: [i32; 4],
    /// Material used by the wall.
    pub material: Option<ResourceId>,
    /// Non-destructive texture-coordinate transform.
    #[serde(default, skip_serializing_if = "GridUvTransform::is_identity")]
    pub uv: GridUvTransform,
    /// Whether collision treats this wall as blocking.
    pub solid: bool,
    /// `Some(corner)` when one wall corner has been deleted,
    /// turning the wall quad into a triangle. Default `None`.
    #[serde(default)]
    pub dropped_corner: Option<WallCorner>,
}

impl GridVerticalFace {
    /// Wall from explicit per-corner heights in `[BL, BR, TR, TL]`
    /// order.
    pub const fn with_heights(heights: [i32; 4], material: Option<ResourceId>) -> Self {
        Self {
            heights,
            material,
            uv: GridUvTransform::IDENTITY,
            solid: true,
            dropped_corner: None,
        }
    }

    /// Flat wall from `bottom` to `top`.
    pub const fn flat(bottom: i32, top: i32, material: Option<ResourceId>) -> Self {
        Self::with_heights([bottom, bottom, top, top], material)
    }

    pub fn drop_corner(&mut self, corner: WallCorner) {
        self.dropped_corner = Some(corner);
    }

    pub fn restore_corner(&mut self) {
        self.dropped_corner = None;
    }

    pub const fn is_triangle(&self) -> bool {
        self.dropped_corner.is_some()
    }

    /// Set this wall's V span so texel density follows the world
    /// grid: `TILE_UV` texels cover one sector-height.
    ///
    /// The wall geometry is not changed. Returns `true` when the
    /// requested span had to be clamped to the PS1 packet UV range.
    pub fn autotile_uv(&mut self, sector_size: i32) -> bool {
        let (span_v, clamped) = uv_span_for_world_span(self.max_vertical_span(), sector_size);
        self.uv.span[0] = 0;
        self.uv.span[1] = stored_uv_span(span_v);
        clamped
    }

    /// Number of runtime wall records needed to draw this wall
    /// without asking one PS1 primitive to encode a V span beyond
    /// the packet's 8-bit UV coordinate range.
    pub fn autotile_segment_count(&self, sector_size: i32) -> usize {
        if !self.should_split_autotile_segments(sector_size) {
            return 1;
        }
        let sector_size = sector_size.max(1) as usize;
        let max_span = self.max_vertical_span().max(0) as usize;
        ((max_span + sector_size - 1) / sector_size).max(1)
    }

    /// Split this wall into sector-height stack entries and retile
    /// each segment so every cooked primitive stays within the
    /// packet's 8-bit V coordinate range.
    pub fn split_into_autotile_segments(&self, sector_size: i32) -> Vec<Self> {
        if !self.should_split_autotile_segments(sector_size) {
            return vec![self.clone()];
        }
        let sector_size = sector_size.max(1);
        let max_span = self.max_vertical_span();
        if max_span == 0 {
            return vec![self.clone()];
        }

        let mut out = Vec::with_capacity(self.autotile_segment_count(sector_size));
        let mut start = 0;
        while start < max_span {
            let end = start.saturating_add(sector_size).min(max_span);
            let mut wall = self.clone();
            wall.heights = self.segment_heights(start, end, max_span);
            let (span_v, _) = uv_span_for_world_span(end.saturating_sub(start), sector_size);
            wall.uv.span[1] = stored_uv_span(span_v);
            let start_v = div_round_i64(
                i64::from(start) * i64::from(psxed_format::world::TILE_UV),
                i64::from(sector_size),
            );
            wall.uv.offset[1] =
                wrap_tiled_uv_offset_i16(i64::from(self.uv.offset[1]).saturating_add(start_v));
            out.push(wall);
            start = end;
        }
        out
    }

    /// Split this wall into sector-height stack entries without
    /// changing its material or UV settings.
    pub fn split_into_height_segments(&self, sector_size: i32) -> Vec<Self> {
        if self.is_triangle() {
            return vec![self.clone()];
        }
        let sector_size = sector_size.max(1);
        let max_span = self.max_vertical_span();
        if max_span == 0 {
            return vec![self.clone()];
        }

        let mut out = Vec::new();
        let mut start = 0;
        while start < max_span {
            let end = start.saturating_add(sector_size).min(max_span);
            let mut wall = self.clone();
            wall.heights = self.segment_heights(start, end, max_span);
            out.push(wall);
            start = end;
        }
        out
    }

    fn should_split_autotile_segments(&self, sector_size: i32) -> bool {
        if self.is_triangle() {
            return false;
        }
        let max_span = self.max_vertical_span();
        let (expected_span, clamped) = uv_span_for_world_span(max_span, sector_size);
        clamped && self.uv.span[1] == stored_uv_span(expected_span)
    }

    fn segment_heights(&self, start: i32, end: i32, max_span: i32) -> [i32; 4] {
        [
            lerp_i32_ratio(
                self.heights[WallCorner::BL.idx()],
                self.heights[WallCorner::TL.idx()],
                start,
                max_span,
            ),
            lerp_i32_ratio(
                self.heights[WallCorner::BR.idx()],
                self.heights[WallCorner::TR.idx()],
                start,
                max_span,
            ),
            lerp_i32_ratio(
                self.heights[WallCorner::BR.idx()],
                self.heights[WallCorner::TR.idx()],
                end,
                max_span,
            ),
            lerp_i32_ratio(
                self.heights[WallCorner::BL.idx()],
                self.heights[WallCorner::TL.idx()],
                end,
                max_span,
            ),
        ]
    }

    fn max_vertical_span(&self) -> i32 {
        let left_span =
            self.heights[WallCorner::TL.idx()].saturating_sub(self.heights[WallCorner::BL.idx()]);
        let right_span =
            self.heights[WallCorner::TR.idx()].saturating_sub(self.heights[WallCorner::BR.idx()]);
        left_span.unsigned_abs().max(right_span.unsigned_abs()) as i32
    }
}

fn uv_span_for_world_span(world_span: i32, sector_size: i32) -> (u16, bool) {
    if world_span <= 0 {
        return (u16::from(psxed_format::world::TILE_UV), false);
    }
    let sector_size = sector_size.max(1);
    let unclamped = div_round_i64(
        i64::from(world_span) * i64::from(psxed_format::world::TILE_UV),
        i64::from(sector_size),
    );
    let texels = unclamped.clamp(1, 255) as u16;
    (texels, unclamped > 255)
}

fn stored_uv_span(span: u16) -> u16 {
    if span == u16::from(psxed_format::world::TILE_UV) {
        0
    } else {
        span
    }
}

fn lerp_i32_ratio(a: i32, b: i32, numerator: i32, denominator: i32) -> i32 {
    if denominator <= 0 {
        return a;
    }
    let delta = i64::from(b).saturating_sub(i64::from(a));
    i64::from(a)
        .saturating_add(div_round_i64(
            delta.saturating_mul(i64::from(numerator)),
            i64::from(denominator),
        ))
        .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn div_round_i64(numerator: i64, denominator: i64) -> i64 {
    if denominator == 0 {
        return 0;
    }
    if numerator >= 0 {
        numerator.saturating_add(denominator / 2) / denominator
    } else {
        numerator.saturating_sub(denominator / 2) / denominator
    }
}

/// Array-sector rectangle enclosing authored grid geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldGridFootprint {
    pub x: u16,
    pub z: u16,
    pub width: u16,
    pub depth: u16,
}

impl WorldGridFootprint {
    pub fn end_x(self) -> u16 {
        self.x + self.width
    }

    pub fn end_z(self) -> u16 {
        self.z + self.depth
    }
}

/// Wall lists for one grid sector.
///
/// **Ownership rule**: a physical wall between cells `(x, z)` and
/// `(x+1, z)` is the **East** wall of `(x, z)` AND the **West**
/// wall of `(x+1, z)` simultaneously. The editor's PaintWall tool
/// stamps only one side (whichever the user clicked). When both
/// sides claim the same physical edge the cooker rejects the grid
/// with `DuplicatePhysicalWall` -- render-+-collision-correct
/// double walls aren't a thing, and silent-dedup risks the editor
/// and runtime disagreeing about which side won. North/South share
/// `North(x, z)` ↔ `South(x, z+1)` under the same rule.
///
/// Diagonal walls are authoring-only for now: cooker rejects them
/// (`UnsupportedDiagonalWall`) until render / pick / collision
/// agree on their geometry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridWalls {
    /// Walls on the north edge.
    pub north: Vec<GridVerticalFace>,
    /// Walls on the east edge.
    pub east: Vec<GridVerticalFace>,
    /// Walls on the south edge.
    pub south: Vec<GridVerticalFace>,
    /// Walls on the west edge.
    pub west: Vec<GridVerticalFace>,
    /// Diagonal NW-SE walls.
    pub north_west_south_east: Vec<GridVerticalFace>,
    /// Diagonal NE-SW walls.
    pub north_east_south_west: Vec<GridVerticalFace>,
}

impl GridWalls {
    /// Immutable walls for one direction.
    pub fn get(&self, direction: GridDirection) -> &[GridVerticalFace] {
        match direction {
            GridDirection::North => &self.north,
            GridDirection::East => &self.east,
            GridDirection::South => &self.south,
            GridDirection::West => &self.west,
            GridDirection::NorthWestSouthEast => &self.north_west_south_east,
            GridDirection::NorthEastSouthWest => &self.north_east_south_west,
        }
    }

    /// Mutable walls for one direction.
    pub fn get_mut(&mut self, direction: GridDirection) -> &mut Vec<GridVerticalFace> {
        match direction {
            GridDirection::North => &mut self.north,
            GridDirection::East => &mut self.east,
            GridDirection::South => &mut self.south,
            GridDirection::West => &mut self.west,
            GridDirection::NorthWestSouthEast => &mut self.north_west_south_east,
            GridDirection::NorthEastSouthWest => &mut self.north_east_south_west,
        }
    }
}

/// One authored grid sector.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridSector {
    /// Optional floor.
    pub floor: Option<GridHorizontalFace>,
    /// Optional ceiling.
    pub ceiling: Option<GridHorizontalFace>,
    /// Sector edge walls.
    pub walls: GridWalls,
    /// Room/floor reached by moving upward through this sector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub floor_above: Option<GridFloorLink>,
    /// Room/floor reached by moving downward through this sector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub floor_below: Option<GridFloorLink>,
}

impl GridSector {
    /// Empty sector.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Sector with one floor face.
    pub fn with_floor(height: i32, material: Option<ResourceId>) -> Self {
        Self {
            floor: Some(GridHorizontalFace::flat(height, material)),
            ..Self::default()
        }
    }

    /// True if the sector emits any geometry.
    pub fn has_geometry(&self) -> bool {
        self.floor.is_some()
            || self.ceiling.is_some()
            || !self.walls.north.is_empty()
            || !self.walls.east.is_empty()
            || !self.walls.south.is_empty()
            || !self.walls.west.is_empty()
            || !self.walls.north_west_south_east.is_empty()
            || !self.walls.north_east_south_west.is_empty()
    }

    /// True when this sector carries vertical room/floor traversal metadata.
    pub fn has_floor_links(&self) -> bool {
        self.floor_above.is_some() || self.floor_below.is_some()
    }
}

/// Link from one room sector to a vertically adjacent room floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridFloorLink {
    /// Target room node. `None` keeps imported or partially-authored
    /// floor links visible until the room can be repaired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_room: Option<NodeId>,
    /// Target floor within that room. PSoXide currently cooks floor
    /// zero only; the field is present so TR-style vertical stacking
    /// has a stable authored address before runtime traversal uses it.
    #[serde(default, skip_serializing_if = "is_zero_u16")]
    pub target_floor: u16,
}

impl GridFloorLink {
    /// Link to floor zero of a target room.
    pub const fn room(target_room: NodeId) -> Self {
        Self {
            target_room: Some(target_room),
            target_floor: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HorizontalSurface {
    Floor,
    Ceiling,
}

impl HorizontalSurface {
    fn edge_heights(self, sector: &GridSector, direction: GridDirection) -> Option<[i32; 2]> {
        let heights = match self {
            Self::Floor => sector.floor.as_ref()?.heights,
            Self::Ceiling => sector.ceiling.as_ref()?.heights,
        };
        horizontal_edge_heights_for_wall(heights, direction)
    }
}

fn horizontal_edge_heights_for_wall(
    heights: [i32; 4],
    direction: GridDirection,
) -> Option<[i32; 2]> {
    match direction {
        GridDirection::North => Some([heights[Corner::NW.idx()], heights[Corner::NE.idx()]]),
        GridDirection::East => Some([heights[Corner::NE.idx()], heights[Corner::SE.idx()]]),
        GridDirection::South => Some([heights[Corner::SE.idx()], heights[Corner::SW.idx()]]),
        GridDirection::West => Some([heights[Corner::SW.idx()], heights[Corner::NW.idx()]]),
        GridDirection::NorthWestSouthEast => {
            Some([heights[Corner::NW.idx()], heights[Corner::SE.idx()]])
        }
        GridDirection::NorthEastSouthWest => {
            Some([heights[Corner::NE.idx()], heights[Corner::SW.idx()]])
        }
    }
}

fn set_horizontal_edge_heights(heights: &mut [i32; 4], direction: GridDirection, edge: [i32; 2]) {
    match direction {
        GridDirection::North => {
            heights[Corner::NW.idx()] = edge[0];
            heights[Corner::NE.idx()] = edge[1];
        }
        GridDirection::East => {
            heights[Corner::NE.idx()] = edge[0];
            heights[Corner::SE.idx()] = edge[1];
        }
        GridDirection::South => {
            heights[Corner::SE.idx()] = edge[0];
            heights[Corner::SW.idx()] = edge[1];
        }
        GridDirection::West => {
            heights[Corner::SW.idx()] = edge[0];
            heights[Corner::NW.idx()] = edge[1];
        }
        GridDirection::NorthWestSouthEast => {
            heights[Corner::NW.idx()] = edge[0];
            heights[Corner::SE.idx()] = edge[1];
        }
        GridDirection::NorthEastSouthWest => {
            heights[Corner::NE.idx()] = edge[0];
            heights[Corner::SW.idx()] = edge[1];
        }
    }
}

fn wall_top_edge_heights(walls: &[GridVerticalFace]) -> Option<[i32; 2]> {
    walls
        .iter()
        .max_by_key(|wall| {
            i64::from(wall.heights[WallCorner::TL.idx()])
                + i64::from(wall.heights[WallCorner::TR.idx()])
        })
        .map(|wall| {
            [
                wall.heights[WallCorner::TL.idx()],
                wall.heights[WallCorner::TR.idx()],
            ]
        })
}

fn floor_transition_wall_material(
    floor: &GridHorizontalFace,
    neighbour_floor: &GridHorizontalFace,
    floor_edge: [i32; 2],
    neighbour_edge: [i32; 2],
) -> Option<ResourceId> {
    let floor_sum = i64::from(floor_edge[0]) + i64::from(floor_edge[1]);
    let neighbour_sum = i64::from(neighbour_edge[0]) + i64::from(neighbour_edge[1]);
    if floor_sum >= neighbour_sum {
        floor.material.or(neighbour_floor.material)
    } else {
        neighbour_floor.material.or(floor.material)
    }
}

/// Hard caps on a single room's authoring shape. The cooker
/// rejects past these, and the editor inspector warns as the
/// budget approaches them -- both to keep the cooked `.psxw`
/// inside reasonable PSX-side memory and to surface coordinate
/// safety early (32-sector room × 1024 sector_size = 32 768,
/// right at the i16 cliff; the renderer uses anchor-relative
/// coords now but still respects the cap as belt-and-braces).
pub const MAX_ROOM_WIDTH: u16 = 32;
pub const MAX_ROOM_DEPTH: u16 = 32;
pub const MAX_WALL_STACK: usize = 4;
pub const MAX_ROOM_TRIANGLES: usize = 2048;
pub const MAX_ROOM_BYTES: usize = 64 * 1024;

/// World-unit step every authored vertex height must align to.
///
/// The X / Z grid is locked by construction -- corners are always
/// computed from the cell's array index and `sector_size`. Y is
/// the only free axis, and we constrain it to multiples of this
/// step so the editor can't author noise heights that the runtime
/// quantises away anyway.
///
/// 64 is `sector_size / 16` at the default 1024 -- fine enough for
/// authored slopes, coarse enough that PS1 i16 vertex jitter never
/// fights the snap.
pub const HEIGHT_QUANTUM: i32 = 64;
/// World grid size quantum. The editor stores one sector size per
/// World node and snaps it to this step so room/cook math stays
/// integer and PSX-friendly.
pub const WORLD_SECTOR_SIZE_QUANTUM: i32 = 128;
/// Default sector size used by starter/legacy projects.
pub const DEFAULT_WORLD_SECTOR_SIZE: i32 = 1024;
/// Default third-person camera distance inherited by rooms.
pub const DEFAULT_WORLD_CAMERA_DISTANCE: i32 = 2700;
/// Default camera origin height above the player origin.
pub const DEFAULT_WORLD_CAMERA_HEIGHT: i32 = 1280;
/// Default look-at height above the player origin.
pub const DEFAULT_WORLD_CAMERA_TARGET_HEIGHT: i32 = 640;
/// Default minimum camera origin height above the sampled floor.
pub const DEFAULT_WORLD_CAMERA_MIN_FLOOR_CLEARANCE: i32 = HEIGHT_QUANTUM;
/// Minimum authored third-person camera distance.
pub const MIN_WORLD_CAMERA_DISTANCE: i32 = 384;
/// Maximum authored third-person camera distance.
pub const MAX_WORLD_CAMERA_DISTANCE: i32 = 16_384;
/// Maximum authored camera vertical offset.
pub const MAX_WORLD_CAMERA_HEIGHT: i32 = 16_384;
/// Maximum authored minimum floor clearance for the third-person camera.
pub const MAX_WORLD_CAMERA_MIN_FLOOR_CLEARANCE: i32 = 4_096;
/// Default wall span when no ceiling is authored above the edge.
pub const DEFAULT_WALL_HEIGHT_SECTORS: i32 = 2;
/// Minimum authored sector size.
pub const MIN_WORLD_SECTOR_SIZE: i32 = WORLD_SECTOR_SIZE_QUANTUM;
/// Maximum authored sector size. This is an authoring sanity cap,
/// not a PSX wire-format limit.
pub const MAX_WORLD_SECTOR_SIZE: i32 = 8192;
/// Fixed-point one for authored model resource scale.
pub const MODEL_SCALE_ONE_Q8: u16 = 256;

/// Snap a vertex height to the nearest [`HEIGHT_QUANTUM`] multiple.
///
/// Round-half-away-from-zero so the snap is symmetric for
/// negative heights -- `snap_height(-31)` returns `0`,
/// `snap_height(-32)` returns `-64`. Plain integer math; no
/// float intermediaries.
pub fn snap_height(y: i32) -> i32 {
    let q = HEIGHT_QUANTUM;
    let half = q / 2;
    if y >= 0 {
        ((y + half) / q) * q
    } else {
        -(((-y + half) / q) * q)
    }
}

/// Snap a requested World sector size to a positive 128-unit grid.
pub fn snap_world_sector_size(size: i32) -> i32 {
    let clamped = size.clamp(MIN_WORLD_SECTOR_SIZE, MAX_WORLD_SECTOR_SIZE);
    ((clamped + WORLD_SECTOR_SIZE_QUANTUM / 2) / WORLD_SECTOR_SIZE_QUANTUM)
        * WORLD_SECTOR_SIZE_QUANTUM
}

fn default_world_sector_size() -> i32 {
    DEFAULT_WORLD_SECTOR_SIZE
}

const fn is_zero_u16(value: &u16) -> bool {
    *value == 0
}

fn default_world_camera_distance() -> i32 {
    DEFAULT_WORLD_CAMERA_DISTANCE
}

fn default_world_camera_height() -> i32 {
    DEFAULT_WORLD_CAMERA_HEIGHT
}

fn default_world_camera_target_height() -> i32 {
    DEFAULT_WORLD_CAMERA_TARGET_HEIGHT
}

fn default_world_camera_min_floor_clearance() -> i32 {
    DEFAULT_WORLD_CAMERA_MIN_FLOOR_CLEARANCE
}

fn default_wall_height_for_sector_size(sector_size: i32) -> i32 {
    sector_size.saturating_mul(DEFAULT_WALL_HEIGHT_SECTORS)
}

fn default_model_scale_q8() -> [u16; 3] {
    [MODEL_SCALE_ONE_Q8; 3]
}

fn default_model_renderer_visual_scale_q8() -> u16 {
    MODEL_SCALE_ONE_Q8
}

fn scale_i32_ratio(value: i32, from: i32, to: i32) -> i32 {
    if from <= 0 || from == to {
        return value;
    }
    (((value as i64) * (to as i64) + (from as i64 / 2)) / (from as i64)) as i32
}

fn scale_u16_ratio(value: u16, from: i32, to: i32) -> u16 {
    scale_i32_ratio(value as i32, from, to).clamp(0, u16::MAX as i32) as u16
}

/// Snapshot of a [`WorldGrid`]'s authoring footprint + cooked-
/// byte estimate. Cheap to compute (single sector pass); the
/// editor recomputes it whenever the inspector for a Room
/// repaints.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorldGridBudget {
    /// Grid width in sectors.
    pub width: u16,
    /// Grid depth in sectors.
    pub depth: u16,
    /// `width * depth`. `.psxw` stores a sector record for
    /// every cell whether it's populated or not, so this is
    /// what the wire-size formula multiplies against.
    pub total_cells: usize,
    /// Cells that have any geometry (floor / ceiling / walls).
    /// Useful for surface-area / drawcall estimates; not the
    /// driver of the byte budget.
    pub populated_cells: usize,
    pub floors: usize,
    pub ceilings: usize,
    pub walls: usize,
    pub horizontal_overrides: usize,
    pub triangles: usize,
    /// Current `.psxw` geometry wire size. The format stores a
    /// sector record for **every** cell, so this uses `total_cells`,
    /// not `populated_cells`.
    pub psxw_bytes: usize,
    /// Additional bytes appended when static per-vertex lighting is
    /// baked into `.psxw` v3 for Embedded Play.
    pub static_light_table_bytes: usize,
    /// Full Embedded Play room asset size: geometry `.psxw` plus the
    /// baked static-light table.
    pub psxw_static_lit_bytes: usize,
    /// Estimated size if we shipped the future compact format
    /// described in `docs/world-format-roadmap.md` (28-byte
    /// sectors, 12-byte walls). Surfaced as a planning aid, not
    /// a contract -- no live `.psxw` is ever this size today.
    pub future_compact_estimated_bytes: usize,
}

impl WorldGridBudget {
    /// `true` if any base geometry cap is exceeded. Mirrors the
    /// generic world-cooker validation before Embedded Play appends
    /// static lighting.
    pub fn over_budget(&self) -> bool {
        self.width > MAX_ROOM_WIDTH
            || self.depth > MAX_ROOM_DEPTH
            || self.triangles > MAX_ROOM_TRIANGLES
            || self.psxw_bytes > MAX_ROOM_BYTES
    }

    /// `true` if Embedded Play's static-lit room asset would exceed
    /// the current runtime chunk limits.
    pub fn static_lit_over_budget(&self) -> bool {
        self.width > MAX_ROOM_WIDTH
            || self.depth > MAX_ROOM_DEPTH
            || self.triangles > MAX_ROOM_TRIANGLES
            || self.psxw_static_lit_bytes > MAX_ROOM_BYTES
    }
}

const ASSET_HEADER_BYTES: usize = 12;
const WORLD_HEADER_BYTES: usize = psxed_format::world::WorldHeader::SIZE;
const PSXW_SECTOR_BYTES: usize = psxed_format::world::SectorRecord::SIZE;
const PSXW_WALL_BYTES: usize = psxed_format::world::WallRecord::SIZE;
const PSXW_HORIZONTAL_OVERRIDE_BYTES: usize = psxed_format::world::HorizontalOverrideRecord::SIZE;
const PSXW_SURFACE_LIGHT_BYTES: usize = psxed_format::world::SurfaceLightRecord::SIZE;
const FUTURE_COMPACT_SECTOR_BYTES: usize = 28;
const FUTURE_COMPACT_WALL_BYTES: usize = 12;

const fn default_ambient_color() -> [u8; 3] {
    [32, 32, 32]
}

const fn default_fog_color() -> [u8; 3] {
    [24, 28, 34]
}

const fn default_atmosphere_enabled() -> bool {
    true
}

const fn default_atmosphere_color() -> [u8; 3] {
    [58, 52, 44]
}

const fn default_atmosphere_density() -> i32 {
    44
}

const fn default_atmosphere_fall_speed_q4() -> i32 {
    7
}

const fn default_atmosphere_wind_speed_q4() -> i32 {
    2
}

const fn default_sky_top_color() -> [u8; 3] {
    [7, 8, 14]
}

const fn default_sky_horizon_color() -> [u8; 3] {
    [32, 30, 34]
}

const fn default_sky_lower_color() -> [u8; 3] {
    [5, 7, 12]
}

const fn default_sky_horizon_percent() -> u8 {
    58
}

const fn default_sky_horizon_thickness_percent() -> u8 {
    8
}

const fn default_sky_horizon_glow_percent() -> u8 {
    68
}

const fn default_sky_horizon_glow_yaw_degrees() -> i16 {
    72
}

const fn default_sky_sun_enabled() -> bool {
    false
}

fn default_sky_sun_color() -> [u8; 3] {
    [255, 218, 150]
}

fn default_sky_sun_border_color() -> [u8; 3] {
    [255, 128, 78]
}

const fn default_sky_sun_yaw_degrees() -> i16 {
    72
}

const fn default_sky_sun_pitch_degrees() -> i16 {
    22
}

const fn default_sky_sun_size_percent() -> u8 {
    18
}

const fn default_sky_sun_glow_percent() -> u8 {
    72
}

const fn default_sky_sun_glow_size_percent() -> u8 {
    64
}

const fn default_sky_mountain_height_percent() -> u8 {
    55
}

fn default_sky_mountain_top_color() -> [u8; 3] {
    [84, 96, 124]
}

fn default_sky_mountain_base_color() -> [u8; 3] {
    [24, 28, 42]
}

const fn default_sky_mountain_gap_percent() -> u8 {
    22
}

const fn default_sky_mountain_roughness_percent() -> u8 {
    78
}

const fn default_sky_mountain_layer_count() -> u8 {
    2
}

/// Maximum authored distant mountain height. Values above 100 are
/// intentionally allowed now that runtime uses a baked panorama.
pub const SKY_MOUNTAIN_HEIGHT_PERCENT_MAX: u8 = 200;

/// Minimum number of horizontal cyclorama subdivisions.
pub const SKYBOX_COLUMNS_MIN: u8 = 4;
/// Maximum number of horizontal cyclorama subdivisions.
pub const SKYBOX_COLUMNS_MAX: u8 = 32;
/// Default number of horizontal cyclorama subdivisions.
pub const SKYBOX_COLUMNS_DEFAULT: u8 = 16;
/// Minimum number of vertical cyclorama subdivisions.
pub const SKYBOX_ROWS_MIN: u8 = 3;
/// Maximum number of vertical cyclorama subdivisions.
pub const SKYBOX_ROWS_MAX: u8 = 20;
/// Default number of vertical cyclorama subdivisions.
pub const SKYBOX_ROWS_DEFAULT: u8 = 10;

const fn default_skybox_columns() -> u8 {
    SKYBOX_COLUMNS_DEFAULT
}

const fn default_skybox_rows() -> u8 {
    SKYBOX_ROWS_DEFAULT
}

const fn default_sky_match_room_fog() -> bool {
    true
}

const fn default_far_vista_radius() -> i32 {
    18_000
}

const fn default_far_vista_height() -> i32 {
    4_096
}

const fn default_far_vista_vertical_offset() -> i32 {
    -512
}

const fn default_far_vista_segments() -> u8 {
    12
}

const fn default_far_vista_tint() -> [u8; 3] {
    [54, 58, 62]
}

const fn default_far_vista_match_room_fog() -> bool {
    true
}

/// Maximum number of individually textured cards in a far-vista ring.
pub const FAR_VISTA_TEXTURE_PANEL_COUNT: usize = 16;

const fn default_far_vista_texture_panels() -> [Option<ResourceId>; FAR_VISTA_TEXTURE_PANEL_COUNT] {
    [None; FAR_VISTA_TEXTURE_PANEL_COUNT]
}

const fn default_fog_near() -> i32 {
    4096
}

const fn default_fog_far() -> i32 {
    16384
}

const fn default_light_color() -> [u8; 3] {
    [255, 240, 200]
}

/// World sky rendering mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkyMode {
    /// Disable authored sky rendering. The renderer clears to
    /// [`SkySettings::lower_color`] only.
    Off,
    /// Draw a cooked cyclorama before world geometry.
    Gradient,
}

impl Default for SkyMode {
    fn default() -> Self {
        Self::Gradient
    }
}

/// World-level sky configuration shared by descendant Rooms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkySettings {
    /// Whether this World renders a sky.
    #[serde(default)]
    pub mode: SkyMode,
    /// Zenith colour.
    #[serde(default = "default_sky_top_color")]
    pub top_color: [u8; 3],
    /// Colour at the authored horizon line.
    #[serde(default = "default_sky_horizon_color")]
    pub horizon_color: [u8; 3],
    /// Colour at the bottom of the frame.
    #[serde(default = "default_sky_lower_color")]
    pub lower_color: [u8; 3],
    /// Horizon line as a percentage of screen height.
    #[serde(default = "default_sky_horizon_percent")]
    pub horizon_percent: u8,
    /// Angular thickness of the horizon band. Wider values hold the
    /// horizon colour longer before blending to zenith/lower sky.
    #[serde(default = "default_sky_horizon_thickness_percent")]
    pub horizon_thickness_percent: u8,
    /// Strength of the warm localized horizon glow baked into the
    /// cyclorama.
    #[serde(default = "default_sky_horizon_glow_percent")]
    pub horizon_glow_percent: u8,
    /// Direction of the warm horizon glow in cyclorama yaw degrees.
    #[serde(default = "default_sky_horizon_glow_yaw_degrees")]
    pub horizon_glow_yaw_degrees: i16,
    /// Whether a cooked sun disc/glow is drawn into the cyclorama.
    #[serde(default = "default_sky_sun_enabled")]
    pub sun_enabled: bool,
    /// Inner sun disc colour.
    #[serde(default = "default_sky_sun_color")]
    pub sun_color: [u8; 3],
    /// Outer sun ring / eclipse border colour.
    #[serde(default = "default_sky_sun_border_color")]
    pub sun_border_color: [u8; 3],
    /// Sun direction in cyclorama yaw degrees.
    #[serde(default = "default_sky_sun_yaw_degrees")]
    pub sun_yaw_degrees: i16,
    /// Sun height in cyclorama pitch degrees.
    #[serde(default = "default_sky_sun_pitch_degrees")]
    pub sun_pitch_degrees: i16,
    /// Cooked sun disc radius.
    #[serde(default = "default_sky_sun_size_percent")]
    pub sun_size_percent: u8,
    /// Strength of the soft glow around the sun disc.
    #[serde(default = "default_sky_sun_glow_percent")]
    pub sun_glow_percent: u8,
    /// Angular spread of the sun glow.
    #[serde(default = "default_sky_sun_glow_size_percent")]
    pub sun_glow_size_percent: u8,
    /// Height/intensity of cooked distant mountain silhouettes.
    /// Values above 100 push the baked ridge higher than the legacy
    /// runtime-geometry range.
    #[serde(default = "default_sky_mountain_height_percent")]
    pub mountain_height_percent: u8,
    /// Tint used near distant mountain peaks.
    #[serde(default = "default_sky_mountain_top_color")]
    pub mountain_top_color: [u8; 3],
    /// Tint used at the mountain bases.
    #[serde(default = "default_sky_mountain_base_color")]
    pub mountain_base_color: [u8; 3],
    /// Gap between the horizon and the mountain ridge. At the lowest
    /// values the ridge can overlap into the horizon/cloud band.
    #[serde(default = "default_sky_mountain_gap_percent")]
    pub mountain_gap_percent: u8,
    /// Jaggedness of the generated mountain silhouette.
    #[serde(default = "default_sky_mountain_roughness_percent")]
    pub mountain_roughness_percent: u8,
    /// Number of parallax-free painted mountain layers.
    #[serde(default = "default_sky_mountain_layer_count")]
    pub mountain_layer_count: u8,
    /// Horizontal cyclorama subdivisions used by the editor preview
    /// and runtime sky renderer.
    #[serde(default = "default_skybox_columns")]
    pub skybox_columns: u8,
    /// Vertical cyclorama subdivisions used by the editor preview
    /// and runtime sky renderer.
    #[serde(default = "default_skybox_rows")]
    pub skybox_rows: u8,
    /// Blend horizon/lower sky toward the room fog colour when
    /// fog is enabled.
    #[serde(default = "default_sky_match_room_fog")]
    pub match_room_fog: bool,
    /// Optional cloud-layer settings folded into the cooked
    /// cyclorama backdrop.
    #[serde(default)]
    pub cloud_layer: CloudLayerSettings,
}

/// Cloud-layer authoring fields. The cooker folds these values into
/// the generated vertex-coloured cyclorama backdrop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudLayerSettings {
    /// Whether the cloud layer is drawn at all.
    #[serde(default)]
    pub enabled: bool,
    /// Cloud highlight colour used by the cyclorama cloud streaks.
    #[serde(default = "default_cloud_color")]
    pub color: [u8; 3],
    /// 0 = no coverage, 255 = maximum coverage.
    #[serde(default = "default_cloud_density")]
    pub density: u8,
    /// Vertical bias for the cyclorama cloud band.
    #[serde(default = "default_cloud_altitude")]
    pub altitude: u16,
    /// Width of the cyclorama cloud band.
    #[serde(default = "default_cloud_extent")]
    pub extent: u16,
    /// Cloud scroll speed reserved for animated cyclorama variants.
    #[serde(default = "default_cloud_scroll_speed")]
    pub scroll_speed: [i16; 2],
    /// Number of noise/tile repeats across the cloud layer. More
    /// tiles = denser-looking cover but smaller-feeling clouds.
    #[serde(default = "default_cloud_tile_count")]
    pub tile_count: u8,
    /// Seed for the cloud noise. Change to get a different cloud
    /// pattern.
    #[serde(default = "default_cloud_noise_seed")]
    pub noise_seed: u32,
}

fn default_cloud_color() -> [u8; 3] {
    [220, 220, 232]
}
const fn default_cloud_density() -> u8 {
    128
}
const fn default_cloud_altitude() -> u16 {
    6144
}
const fn default_cloud_extent() -> u16 {
    24_576
}
const fn default_cloud_scroll_speed() -> [i16; 2] {
    [4, 0]
}
const fn default_cloud_tile_count() -> u8 {
    4
}
const fn default_cloud_noise_seed() -> u32 {
    0x5a7b_c91d
}

impl Default for CloudLayerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            color: default_cloud_color(),
            density: default_cloud_density(),
            altitude: default_cloud_altitude(),
            extent: default_cloud_extent(),
            scroll_speed: default_cloud_scroll_speed(),
            tile_count: default_cloud_tile_count(),
            noise_seed: default_cloud_noise_seed(),
        }
    }
}

impl SkySettings {
    /// Resolve authored sky values against room-local fog metadata.
    pub fn resolved_for_room(self, fog_enabled: bool, fog_color: [u8; 3]) -> ResolvedSkySettings {
        let mut horizon_color = self.horizon_color;
        let mut lower_color = self.lower_color;
        if self.match_room_fog && fog_enabled {
            horizon_color = blend_rgb(self.horizon_color, fog_color, 128);
            lower_color = blend_rgb(self.lower_color, fog_color, 192);
        }
        ResolvedSkySettings {
            enabled: self.mode == SkyMode::Gradient,
            top_color: self.top_color,
            horizon_color,
            lower_color,
            horizon_percent: self.horizon_percent.clamp(5, 95),
            horizon_thickness_percent: self.horizon_thickness_percent.clamp(0, 80),
            horizon_glow_percent: self.horizon_glow_percent.clamp(0, 100),
            horizon_glow_yaw_degrees: self.horizon_glow_yaw_degrees.clamp(-180, 180),
            sun_enabled: self.sun_enabled,
            sun_color: self.sun_color,
            sun_border_color: self.sun_border_color,
            sun_yaw_degrees: self.sun_yaw_degrees.clamp(-180, 180),
            sun_pitch_degrees: self.sun_pitch_degrees.clamp(-30, 75),
            sun_size_percent: self.sun_size_percent.clamp(1, 100),
            sun_glow_percent: self.sun_glow_percent.clamp(0, 100),
            sun_glow_size_percent: self.sun_glow_size_percent.clamp(0, 100),
            mountain_height_percent: self
                .mountain_height_percent
                .clamp(0, SKY_MOUNTAIN_HEIGHT_PERCENT_MAX),
            mountain_top_color: self.mountain_top_color,
            mountain_base_color: self.mountain_base_color,
            mountain_gap_percent: self.mountain_gap_percent.clamp(0, 100),
            mountain_roughness_percent: self.mountain_roughness_percent.clamp(0, 100),
            mountain_layer_count: self.mountain_layer_count.clamp(1, 3),
            skybox_columns: self
                .skybox_columns
                .clamp(SKYBOX_COLUMNS_MIN, SKYBOX_COLUMNS_MAX),
            skybox_rows: self.skybox_rows.clamp(SKYBOX_ROWS_MIN, SKYBOX_ROWS_MAX),
            cloud_layer: self.cloud_layer,
        }
    }
}

impl Default for SkySettings {
    fn default() -> Self {
        Self {
            mode: SkyMode::Gradient,
            top_color: default_sky_top_color(),
            horizon_color: default_sky_horizon_color(),
            lower_color: default_sky_lower_color(),
            horizon_percent: default_sky_horizon_percent(),
            horizon_thickness_percent: default_sky_horizon_thickness_percent(),
            horizon_glow_percent: default_sky_horizon_glow_percent(),
            horizon_glow_yaw_degrees: default_sky_horizon_glow_yaw_degrees(),
            sun_enabled: default_sky_sun_enabled(),
            sun_color: default_sky_sun_color(),
            sun_border_color: default_sky_sun_border_color(),
            sun_yaw_degrees: default_sky_sun_yaw_degrees(),
            sun_pitch_degrees: default_sky_sun_pitch_degrees(),
            sun_size_percent: default_sky_sun_size_percent(),
            sun_glow_percent: default_sky_sun_glow_percent(),
            sun_glow_size_percent: default_sky_sun_glow_size_percent(),
            mountain_height_percent: default_sky_mountain_height_percent(),
            mountain_top_color: default_sky_mountain_top_color(),
            mountain_base_color: default_sky_mountain_base_color(),
            mountain_gap_percent: default_sky_mountain_gap_percent(),
            mountain_roughness_percent: default_sky_mountain_roughness_percent(),
            mountain_layer_count: default_sky_mountain_layer_count(),
            skybox_columns: default_skybox_columns(),
            skybox_rows: default_skybox_rows(),
            match_room_fog: default_sky_match_room_fog(),
            cloud_layer: CloudLayerSettings::default(),
        }
    }
}

/// Sky values after room-fog matching and validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedSkySettings {
    /// Whether the gradient should be drawn.
    pub enabled: bool,
    /// Zenith colour.
    pub top_color: [u8; 3],
    /// Colour at the horizon line.
    pub horizon_color: [u8; 3],
    /// Colour at the bottom of the frame / clear.
    pub lower_color: [u8; 3],
    /// Horizon line as a percentage of screen height.
    pub horizon_percent: u8,
    /// Angular thickness of the horizon colour band.
    pub horizon_thickness_percent: u8,
    /// Strength of the warm localized horizon glow.
    pub horizon_glow_percent: u8,
    /// Direction of the warm horizon glow in cyclorama yaw degrees.
    pub horizon_glow_yaw_degrees: i16,
    /// Whether a cooked sun disc/glow is drawn.
    pub sun_enabled: bool,
    /// Inner sun disc colour.
    pub sun_color: [u8; 3],
    /// Outer sun ring / eclipse border colour.
    pub sun_border_color: [u8; 3],
    /// Sun direction in cyclorama yaw degrees.
    pub sun_yaw_degrees: i16,
    /// Sun height in cyclorama pitch degrees.
    pub sun_pitch_degrees: i16,
    /// Cooked sun disc radius.
    pub sun_size_percent: u8,
    /// Strength of the soft glow around the sun disc.
    pub sun_glow_percent: u8,
    /// Angular spread of the sun glow.
    pub sun_glow_size_percent: u8,
    /// Height/intensity of cooked distant mountain silhouettes.
    pub mountain_height_percent: u8,
    /// Tint used near distant mountain peaks.
    pub mountain_top_color: [u8; 3],
    /// Tint used at mountain bases.
    pub mountain_base_color: [u8; 3],
    /// Gap between horizon and generated ridge.
    pub mountain_gap_percent: u8,
    /// Jaggedness of the generated mountain silhouette.
    pub mountain_roughness_percent: u8,
    /// Number of painted mountain layers.
    pub mountain_layer_count: u8,
    /// Horizontal cyclorama subdivisions.
    pub skybox_columns: u8,
    /// Vertical cyclorama subdivisions.
    pub skybox_rows: u8,
    /// Resolved cloud layer authoring values used by the cyclorama
    /// generator.
    pub cloud_layer: CloudLayerSettings,
}

/// One generated cyclorama backdrop quad. Directions are unit vectors
/// in Q0.12-ish scale. Runtime/editor preview apply camera rotation
/// only, so this behaves like an infinite authored panorama.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkyCycloramaQuad {
    /// Corner directions ordered top-left, top-right, bottom-left,
    /// bottom-right in angular cyclorama space.
    pub direction_q12: [[i16; 3]; 4],
    /// Per-corner Gouraud colours.
    pub rgb: [[u8; 3]; 4],
}

const SKY_CYCLORAMA_MOUNTAIN_LAYERS: usize = 3;
const SKY_CYCLORAMA_MOUNTAIN_COLUMNS_MAX: usize = 128;
const SKY_CYCLORAMA_CLOUD_STREAK_MAX: usize = 6;
const SKY_CYCLORAMA_CLOUD_HERO_STREAKS: usize = 4;
const SKY_CYCLORAMA_CLOUD_SEGMENTS_MAX: usize = 10;
const SKY_CYCLORAMA_CLOUD_RIBBONS: usize = 3;
const SKY_CYCLORAMA_CLOUD_RIBBON_QUADS: usize = 2;
const SKY_CYCLORAMA_STAR_COUNT_MAX: usize = 64;
const SKY_CYCLORAMA_SUN_SEGMENTS: usize = 24;
const SKY_CYCLORAMA_SUN_GLOW_QUADS: usize = SKY_CYCLORAMA_SUN_SEGMENTS;
const SKY_CYCLORAMA_SUN_BORDER_QUADS: usize = SKY_CYCLORAMA_SUN_SEGMENTS * 2;
const SKY_CYCLORAMA_SUN_CORE_QUADS: usize = SKY_CYCLORAMA_SUN_SEGMENTS;
const SKY_CYCLORAMA_SUN_QUAD_MAX: usize =
    SKY_CYCLORAMA_SUN_GLOW_QUADS + SKY_CYCLORAMA_SUN_BORDER_QUADS + SKY_CYCLORAMA_SUN_CORE_QUADS;
/// Runtime panorama texture width, in 4bpp texels.
pub const SKY_PANORAMA_WIDTH: u16 = 512;
/// Runtime panorama texture height, in 4bpp texels.
pub const SKY_PANORAMA_HEIGHT: u16 = 256;
/// Horizontal 4bpp palette bands. Runtime draws one sky row per
/// band so each altitude range can use its own 16-colour CLUT.
pub const SKY_PANORAMA_PALETTE_BANDS: usize = 8;
const SKY_PANORAMA_PALETTE_COLORS: usize = 16;

/// Maximum number of quads generated by [`generate_sky_cyclorama`].
pub const SKY_CYCLORAMA_QUAD_MAX: usize = SKYBOX_COLUMNS_MAX as usize * SKYBOX_ROWS_MAX as usize
    + SKY_CYCLORAMA_MOUNTAIN_COLUMNS_MAX * SKY_CYCLORAMA_MOUNTAIN_LAYERS
    + (SKY_CYCLORAMA_CLOUD_STREAK_MAX + SKY_CYCLORAMA_CLOUD_HERO_STREAKS)
        * (SKY_CYCLORAMA_CLOUD_SEGMENTS_MAX + 1)
        * SKY_CYCLORAMA_CLOUD_RIBBONS
        * SKY_CYCLORAMA_CLOUD_RIBBON_QUADS
    + SKY_CYCLORAMA_STAR_COUNT_MAX
    + SKY_CYCLORAMA_SUN_QUAD_MAX;

/// Build a Spyro-style cyclorama from authored sky settings.
///
/// This intentionally does the expensive/expressive work at cook
/// time: the output is explicit coloured backdrop geometry. Runtime
/// rendering only projects the baked directions with camera rotation.
pub fn generate_sky_cyclorama(sky: ResolvedSkySettings) -> Vec<SkyCycloramaQuad> {
    if !sky.enabled {
        return Vec::new();
    }

    let columns = sky
        .skybox_columns
        .clamp(SKYBOX_COLUMNS_MIN, SKYBOX_COLUMNS_MAX) as usize;
    let rows = sky.skybox_rows.clamp(SKYBOX_ROWS_MIN, SKYBOX_ROWS_MAX) as usize;
    let horizon_pitch = sky_horizon_pitch_degrees(sky.horizon_percent);
    let top_pitch = (horizon_pitch + 58.0).min(78.0);
    let bottom_pitch = (horizon_pitch - 46.0).max(-72.0);
    let mut out = Vec::with_capacity(SKY_CYCLORAMA_QUAD_MAX);

    for row in 0..rows {
        let t0 = row as f32 / rows as f32;
        let t1 = (row + 1) as f32 / rows as f32;
        let pitch_top = lerp_f32(top_pitch, bottom_pitch, t0);
        let pitch_bottom = lerp_f32(top_pitch, bottom_pitch, t1);
        for column in 0..columns {
            let yaw0 = cyclorama_yaw_for_column(column, columns);
            let yaw1 = cyclorama_yaw_for_column(column + 1, columns);
            push_sky_cyclorama_quad(
                &mut out,
                yaw0,
                yaw1,
                pitch_top,
                pitch_bottom,
                [
                    sky_color_for_pitch_yaw(
                        sky,
                        pitch_top,
                        yaw0,
                        horizon_pitch,
                        top_pitch,
                        bottom_pitch,
                    ),
                    sky_color_for_pitch_yaw(
                        sky,
                        pitch_top,
                        yaw1,
                        horizon_pitch,
                        top_pitch,
                        bottom_pitch,
                    ),
                    sky_color_for_pitch_yaw(
                        sky,
                        pitch_bottom,
                        yaw0,
                        horizon_pitch,
                        top_pitch,
                        bottom_pitch,
                    ),
                    sky_color_for_pitch_yaw(
                        sky,
                        pitch_bottom,
                        yaw1,
                        horizon_pitch,
                        top_pitch,
                        bottom_pitch,
                    ),
                ],
            );
        }
    }

    push_sun_cyclorama(&mut out, sky, horizon_pitch, top_pitch, bottom_pitch);
    push_star_cyclorama(&mut out, sky, horizon_pitch, top_pitch, bottom_pitch);
    push_mountain_cyclorama(&mut out, sky, columns, horizon_pitch);
    push_cloud_streak_cyclorama(&mut out, sky, horizon_pitch, top_pitch, bottom_pitch);
    out.truncate(SKY_CYCLORAMA_QUAD_MAX);
    out
}

/// Bake the resolved cyclorama into a 4bpp multi-CLUT PSXT panorama.
///
/// The editor preview still uses [`generate_sky_cyclorama`] so sky
/// controls remain inspectable as geometry. The playtest runtime uses
/// this texture path so the authored sky is projected from a compact
/// textured cyclorama mesh instead of hundreds of procedural backdrop
/// polygons.
pub fn generate_sky_panorama_psxt(sky: ResolvedSkySettings) -> Option<Vec<u8>> {
    if !sky.enabled {
        return None;
    }
    let pixels = generate_sky_panorama_pixels(sky);
    let (palette_rows, indices) = sky_quantize_panorama_bands(
        &pixels,
        SKY_PANORAMA_WIDTH as usize,
        SKY_PANORAMA_HEIGHT as usize,
        SKY_PANORAMA_PALETTE_BANDS,
    );
    psxed_tex::encode_indexed_psxt_with_clut_rows(
        SKY_PANORAMA_WIDTH,
        SKY_PANORAMA_HEIGHT,
        psxed_tex::PsxtDepth::Bit4,
        &indices,
        &palette_rows,
        false,
    )
    .ok()
}

fn generate_sky_panorama_pixels(sky: ResolvedSkySettings) -> Vec<[u8; 3]> {
    let width = SKY_PANORAMA_WIDTH as usize;
    let height = SKY_PANORAMA_HEIGHT as usize;
    let horizon_pitch = sky_horizon_pitch_degrees(sky.horizon_percent);
    let top_pitch = (horizon_pitch + 58.0).min(78.0);
    let bottom_pitch = (horizon_pitch - 46.0).max(-72.0);
    let mut pixels = vec![[0, 0, 0]; width * height];

    for y in 0..height {
        let v = (y as f32 + 0.5) / height as f32;
        let pitch = lerp_f32(top_pitch, bottom_pitch, v);
        for x in 0..width {
            let u = (x as f32 + 0.5) / width as f32;
            let yaw = -180.0 + 360.0 * u;
            pixels[y * width + x] =
                sky_color_for_pitch_yaw(sky, pitch, yaw, horizon_pitch, top_pitch, bottom_pitch);
        }
    }

    for quad in generate_sky_cyclorama(sky) {
        rasterize_sky_cyclorama_quad(
            &mut pixels,
            quad,
            SKY_PANORAMA_WIDTH,
            SKY_PANORAMA_HEIGHT,
            top_pitch,
            bottom_pitch,
        );
    }

    pixels
}

#[derive(Clone, Copy)]
struct SkyRasterVertex {
    x: f32,
    y: f32,
    rgb: [u8; 3],
}

fn rasterize_sky_cyclorama_quad(
    pixels: &mut [[u8; 3]],
    quad: SkyCycloramaQuad,
    width: u16,
    height: u16,
    top_pitch: f32,
    bottom_pitch: f32,
) {
    let mut vertices = [
        sky_raster_vertex(
            quad.direction_q12[0],
            quad.rgb[0],
            width,
            top_pitch,
            bottom_pitch,
        ),
        sky_raster_vertex(
            quad.direction_q12[1],
            quad.rgb[1],
            width,
            top_pitch,
            bottom_pitch,
        ),
        sky_raster_vertex(
            quad.direction_q12[2],
            quad.rgb[2],
            width,
            top_pitch,
            bottom_pitch,
        ),
        sky_raster_vertex(
            quad.direction_q12[3],
            quad.rgb[3],
            width,
            top_pitch,
            bottom_pitch,
        ),
    ];
    unwrap_sky_raster_u(&mut vertices, width as f32);
    rasterize_sky_triangle(pixels, width, height, vertices[0], vertices[1], vertices[2]);
    rasterize_sky_triangle(pixels, width, height, vertices[1], vertices[2], vertices[3]);
}

fn sky_raster_vertex(
    dir: [i16; 3],
    rgb: [u8; 3],
    width: u16,
    top_pitch: f32,
    bottom_pitch: f32,
) -> SkyRasterVertex {
    let x = dir[0] as f32 / 4096.0;
    let y = dir[1] as f32 / 4096.0;
    let z = dir[2] as f32 / 4096.0;
    let yaw = (-x).atan2(-z).to_degrees();
    let pitch = y.clamp(-1.0, 1.0).asin().to_degrees();
    let u = ((yaw + 180.0) / 360.0) * width as f32;
    let v =
        ((top_pitch - pitch) / (top_pitch - bottom_pitch).max(0.001)) * SKY_PANORAMA_HEIGHT as f32;
    SkyRasterVertex { x: u, y: v, rgb }
}

fn unwrap_sky_raster_u(vertices: &mut [SkyRasterVertex; 4], width: f32) {
    let base = vertices[0].x;
    for vertex in &mut vertices[1..] {
        while vertex.x - base > width * 0.5 {
            vertex.x -= width;
        }
        while base - vertex.x > width * 0.5 {
            vertex.x += width;
        }
    }
}

fn rasterize_sky_triangle(
    pixels: &mut [[u8; 3]],
    width: u16,
    height: u16,
    a: SkyRasterVertex,
    b: SkyRasterVertex,
    c: SkyRasterVertex,
) {
    let width_i32 = i32::from(width);
    let height_i32 = i32::from(height);
    let width_f = width as f32;
    for offset in [0.0, width_f, -width_f] {
        let mut a = a;
        let mut b = b;
        let mut c = c;
        a.x += offset;
        b.x += offset;
        c.x += offset;
        let area = sky_edge(a.x, a.y, b.x, b.y, c.x, c.y);
        if area.abs() < 0.0001 {
            continue;
        }
        let min_x = a.x.min(b.x).min(c.x).floor() as i32;
        let max_x = a.x.max(b.x).max(c.x).ceil() as i32;
        let min_y = (a.y.min(b.y).min(c.y).floor() as i32).clamp(0, height_i32 - 1);
        let max_y = (a.y.max(b.y).max(c.y).ceil() as i32).clamp(0, height_i32 - 1);
        for y in min_y..=max_y {
            let py = y as f32 + 0.5;
            for x in min_x..=max_x {
                let px = x as f32 + 0.5;
                let wa = sky_edge(b.x, b.y, c.x, c.y, px, py) / area;
                let wb = sky_edge(c.x, c.y, a.x, a.y, px, py) / area;
                let wc = sky_edge(a.x, a.y, b.x, b.y, px, py) / area;
                if wa < -0.001 || wb < -0.001 || wc < -0.001 {
                    continue;
                }
                let dst_x = x.rem_euclid(width_i32) as usize;
                let dst = y as usize * width as usize + dst_x;
                pixels[dst] = [
                    sky_interp_channel(a.rgb[0], b.rgb[0], c.rgb[0], wa, wb, wc),
                    sky_interp_channel(a.rgb[1], b.rgb[1], c.rgb[1], wa, wb, wc),
                    sky_interp_channel(a.rgb[2], b.rgb[2], c.rgb[2], wa, wb, wc),
                ];
            }
        }
    }
}

fn sky_edge(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
    (px - ax) * (by - ay) - (py - ay) * (bx - ax)
}

fn sky_interp_channel(a: u8, b: u8, c: u8, wa: f32, wb: f32, wc: f32) -> u8 {
    (a as f32 * wa + b as f32 * wb + c as f32 * wc)
        .round()
        .clamp(0.0, 255.0) as u8
}

#[derive(Clone)]
struct SkyQuantColor {
    rgb: [u8; 3],
    count: u32,
}

fn sky_quantize_panorama_bands(
    pixels: &[[u8; 3]],
    width: usize,
    height: usize,
    bands: usize,
) -> (Vec<Vec<[u8; 3]>>, Vec<u8>) {
    let bands = bands.max(1);
    let mut palette_rows = Vec::with_capacity(bands);
    let mut indices = vec![0u8; pixels.len()];
    for band in 0..bands {
        let y0 = band * height / bands;
        let y1 = (band + 1) * height / bands;
        let mut band_pixels = Vec::with_capacity((y1 - y0) * width);
        for y in y0..y1 {
            let start = y * width;
            band_pixels.extend_from_slice(&pixels[start..start + width]);
        }
        let (palette, band_indices) =
            sky_quantize_pixels(&band_pixels, SKY_PANORAMA_PALETTE_COLORS);
        let mut src = 0usize;
        for y in y0..y1 {
            let start = y * width;
            for x in 0..width {
                indices[start + x] = band_indices[src];
                src += 1;
            }
        }
        palette_rows.push(palette);
    }
    (palette_rows, indices)
}

fn sky_quantize_pixels(pixels: &[[u8; 3]], palette_colors: usize) -> (Vec<[u8; 3]>, Vec<u8>) {
    let mut counts: BTreeMap<u32, u32> = BTreeMap::new();
    for rgb in pixels {
        let key = ((rgb[0] as u32) << 16) | ((rgb[1] as u32) << 8) | rgb[2] as u32;
        *counts.entry(key).or_insert(0) += 1;
    }
    let entries: Vec<SkyQuantColor> = counts
        .into_iter()
        .map(|(key, count)| SkyQuantColor {
            rgb: [
                ((key >> 16) & 0xff) as u8,
                ((key >> 8) & 0xff) as u8,
                (key & 0xff) as u8,
            ],
            count,
        })
        .collect();
    let mut boxes = vec![entries];
    while boxes.len() < palette_colors {
        let Some(best_index) = sky_best_quant_box(&boxes) else {
            break;
        };
        let source = boxes.swap_remove(best_index);
        let Some((left, right)) = sky_split_quant_box(source) else {
            break;
        };
        boxes.push(left);
        boxes.push(right);
    }
    let mut palette: Vec<[u8; 3]> = boxes
        .iter()
        .filter(|colors| !colors.is_empty())
        .map(|colors| sky_quant_box_average(colors))
        .collect();
    if palette.is_empty() {
        palette.push([0, 0, 0]);
    }
    palette.truncate(palette_colors);
    let indices = pixels
        .iter()
        .map(|rgb| sky_nearest_palette_index(*rgb, &palette))
        .collect();
    (palette, indices)
}

fn sky_best_quant_box(boxes: &[Vec<SkyQuantColor>]) -> Option<usize> {
    let mut best_index = None;
    let mut best_score = 0u64;
    for (index, colors) in boxes.iter().enumerate() {
        if colors.len() <= 1 {
            continue;
        }
        let score = sky_quant_box_score(colors);
        if best_index.is_none() || score > best_score {
            best_index = Some(index);
            best_score = score;
        }
    }
    best_index
}

fn sky_split_quant_box(
    mut colors: Vec<SkyQuantColor>,
) -> Option<(Vec<SkyQuantColor>, Vec<SkyQuantColor>)> {
    if colors.len() <= 1 {
        return None;
    }
    let channel = sky_quant_box_split_channel(&colors);
    colors.sort_by_key(|color| (color.rgb[channel], color.rgb[0], color.rgb[1], color.rgb[2]));
    let total: u32 = colors.iter().map(|color| color.count).sum();
    let midpoint = total / 2;
    let mut running = 0u32;
    let mut split = 1usize;
    for (index, color) in colors.iter().enumerate() {
        running = running.saturating_add(color.count);
        if running >= midpoint {
            split = (index + 1).clamp(1, colors.len() - 1);
            break;
        }
    }
    let right = colors.split_off(split);
    Some((colors, right))
}

fn sky_quant_box_split_channel(colors: &[SkyQuantColor]) -> usize {
    let mut mins = [u8::MAX; 3];
    let mut maxs = [0u8; 3];
    for color in colors {
        for channel in 0..3 {
            mins[channel] = mins[channel].min(color.rgb[channel]);
            maxs[channel] = maxs[channel].max(color.rgb[channel]);
        }
    }
    let mut best_channel = 0usize;
    let mut best_range = 0u8;
    for channel in 0..3 {
        let range = maxs[channel].saturating_sub(mins[channel]);
        if range > best_range {
            best_channel = channel;
            best_range = range;
        }
    }
    best_channel
}

fn sky_quant_box_score(colors: &[SkyQuantColor]) -> u64 {
    let mut mins = [u8::MAX; 3];
    let mut maxs = [0u8; 3];
    let mut total = 0u64;
    for color in colors {
        total += u64::from(color.count);
        for channel in 0..3 {
            mins[channel] = mins[channel].min(color.rgb[channel]);
            maxs[channel] = maxs[channel].max(color.rgb[channel]);
        }
    }
    let range = (0..3)
        .map(|channel| maxs[channel].saturating_sub(mins[channel]) as u64)
        .max()
        .unwrap_or(0);
    (range + 1) * total
}

fn sky_quant_box_average(colors: &[SkyQuantColor]) -> [u8; 3] {
    let total: u64 = colors.iter().map(|color| u64::from(color.count)).sum();
    if total == 0 {
        return [0, 0, 0];
    }
    let mut sums = [0u64; 3];
    for color in colors {
        let count = u64::from(color.count);
        for channel in 0..3 {
            sums[channel] += u64::from(color.rgb[channel]) * count;
        }
    }
    [
        ((sums[0] + total / 2) / total) as u8,
        ((sums[1] + total / 2) / total) as u8,
        ((sums[2] + total / 2) / total) as u8,
    ]
}

fn sky_nearest_palette_index(rgb: [u8; 3], palette: &[[u8; 3]]) -> u8 {
    let mut best_index = 0usize;
    let mut best_distance = u32::MAX;
    for (index, color) in palette.iter().enumerate() {
        let dr = i32::from(rgb[0]) - i32::from(color[0]);
        let dg = i32::from(rgb[1]) - i32::from(color[1]);
        let db = i32::from(rgb[2]) - i32::from(color[2]);
        let distance = (dr * dr + dg * dg + db * db) as u32;
        if distance < best_distance {
            best_index = index;
            best_distance = distance;
        }
    }
    best_index as u8
}

fn push_sun_cyclorama(
    out: &mut Vec<SkyCycloramaQuad>,
    sky: ResolvedSkySettings,
    horizon_pitch: f32,
    top_pitch: f32,
    bottom_pitch: f32,
) {
    if !sky.sun_enabled {
        return;
    }

    let yaw = sky.sun_yaw_degrees as f32;
    let pitch = (sky.sun_pitch_degrees as f32).clamp(bottom_pitch + 2.0, top_pitch - 2.0);
    let size_t = sky.sun_size_percent.clamp(1, 100) as f32 / 100.0;
    let glow_t = sky.sun_glow_percent.clamp(0, 100) as f32 / 100.0;
    let glow_size_t = sky.sun_glow_size_percent.clamp(0, 100) as f32 / 100.0;
    let disc_radius = lerp_f32(0.75, 5.2, size_t);

    let glow_radius = (disc_radius + lerp_f32(1.15, 6.4, glow_size_t)).min(12.0);
    if glow_t > 0.0 && glow_size_t > 0.0 {
        push_sun_disc_fan(
            out,
            sky,
            yaw,
            pitch,
            glow_radius,
            glow_radius * 0.7,
            0.63,
            0.34,
            |sky, point_yaw, point_pitch, radius_t, theta| {
                let falloff = (1.0 - radius_t.clamp(0.0, 1.0)).powf(1.65);
                let alpha = (24.0 + glow_t * 88.0) * falloff;
                let highlight = sun_directional_weight(theta, 0.68, 2.2);
                let tint = cyclorama_lerp_rgb(
                    brighten_rgb(sky.sun_border_color, 12),
                    [255, 206, 156],
                    (highlight * 96.0).clamp(0.0, 255.0) as u8,
                );
                sun_tinted_sky_color(
                    sky,
                    point_yaw,
                    point_pitch,
                    tint,
                    alpha,
                    horizon_pitch,
                    top_pitch,
                    bottom_pitch,
                )
            },
        );
    }

    push_sun_annulus_triangles(
        out,
        sky,
        yaw,
        pitch,
        disc_radius,
        disc_radius * 0.98,
        0.52,
        1.08,
        0.41,
        0.82,
        |sky, point_yaw, point_pitch, radius_t, theta| {
            let ridge = smooth_falloff(0.34, (radius_t - 0.8).abs());
            let outer_feather = smooth_falloff(0.18, (radius_t - 1.0).abs());
            let alpha = (166.0 + glow_t * 54.0) * ridge.max(outer_feather * 0.25);
            let highlight = sun_directional_weight(theta, 0.74, 3.1);
            let shade = sun_directional_weight(theta, 3.88, 2.0);
            let mut tint = cyclorama_lerp_rgb(
                sky.sun_border_color,
                [255, 226, 184],
                (highlight * 118.0).clamp(0.0, 255.0) as u8,
            );
            tint = cyclorama_lerp_rgb(tint, [60, 22, 26], (shade * 34.0).clamp(0.0, 255.0) as u8);
            sun_tinted_sky_color(
                sky,
                point_yaw,
                point_pitch,
                tint,
                alpha,
                horizon_pitch,
                top_pitch,
                bottom_pitch,
            )
        },
    );

    push_sun_disc_fan(
        out,
        sky,
        yaw,
        pitch,
        disc_radius * 0.58,
        disc_radius * 0.58,
        1.24,
        0.48,
        |sky, point_yaw, point_pitch, radius_t, _theta| {
            let edge = smooth_step(((radius_t - 0.72) / 0.28).clamp(0.0, 1.0));
            let alpha = lerp_f32(255.0, 228.0, edge);
            sun_tinted_sky_color(
                sky,
                point_yaw,
                point_pitch,
                sky.sun_color,
                alpha,
                horizon_pitch,
                top_pitch,
                bottom_pitch,
            )
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn push_sun_disc_fan<F>(
    out: &mut Vec<SkyCycloramaQuad>,
    sky: ResolvedSkySettings,
    center_yaw: f32,
    center_pitch: f32,
    yaw_radius: f32,
    pitch_radius: f32,
    shape_phase: f32,
    shape_strength: f32,
    mut shade_vertex: F,
) where
    F: FnMut(ResolvedSkySettings, f32, f32, f32, f32) -> [u8; 3],
{
    for segment in 0..SKY_CYCLORAMA_SUN_SEGMENTS {
        let theta0 = std::f32::consts::TAU * segment as f32 / SKY_CYCLORAMA_SUN_SEGMENTS as f32;
        let theta1 =
            std::f32::consts::TAU * (segment + 1) as f32 / SKY_CYCLORAMA_SUN_SEGMENTS as f32;
        let (yaw0, pitch0) = sun_polar_point(
            center_yaw,
            center_pitch,
            yaw_radius,
            pitch_radius,
            1.0,
            theta0,
            shape_phase,
            shape_strength,
        );
        let (yaw1, pitch1) = sun_polar_point(
            center_yaw,
            center_pitch,
            yaw_radius,
            pitch_radius,
            1.0,
            theta1,
            shape_phase,
            shape_strength,
        );
        push_sky_cyclorama_triangle(
            out,
            [(center_yaw, center_pitch), (yaw0, pitch0), (yaw1, pitch1)],
            [
                shade_vertex(sky, center_yaw, center_pitch, 0.0, (theta0 + theta1) * 0.5),
                shade_vertex(sky, yaw0, pitch0, 1.0, theta0),
                shade_vertex(sky, yaw1, pitch1, 1.0, theta1),
            ],
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn push_sun_annulus_triangles<F>(
    out: &mut Vec<SkyCycloramaQuad>,
    sky: ResolvedSkySettings,
    center_yaw: f32,
    center_pitch: f32,
    yaw_radius: f32,
    pitch_radius: f32,
    inner_radius: f32,
    outer_radius: f32,
    shape_phase: f32,
    shape_strength: f32,
    mut shade_vertex: F,
) where
    F: FnMut(ResolvedSkySettings, f32, f32, f32, f32) -> [u8; 3],
{
    for segment in 0..SKY_CYCLORAMA_SUN_SEGMENTS {
        let theta0 = std::f32::consts::TAU * segment as f32 / SKY_CYCLORAMA_SUN_SEGMENTS as f32;
        let theta1 =
            std::f32::consts::TAU * (segment + 1) as f32 / SKY_CYCLORAMA_SUN_SEGMENTS as f32;
        let inner0 = sun_polar_point(
            center_yaw,
            center_pitch,
            yaw_radius,
            pitch_radius,
            inner_radius,
            theta0,
            shape_phase,
            shape_strength,
        );
        let inner1 = sun_polar_point(
            center_yaw,
            center_pitch,
            yaw_radius,
            pitch_radius,
            inner_radius,
            theta1,
            shape_phase,
            shape_strength,
        );
        let outer0 = sun_polar_point(
            center_yaw,
            center_pitch,
            yaw_radius,
            pitch_radius,
            outer_radius,
            theta0,
            shape_phase,
            shape_strength,
        );
        let outer1 = sun_polar_point(
            center_yaw,
            center_pitch,
            yaw_radius,
            pitch_radius,
            outer_radius,
            theta1,
            shape_phase,
            shape_strength,
        );
        push_sky_cyclorama_triangle(
            out,
            [inner0, inner1, outer0],
            [
                shade_vertex(sky, inner0.0, inner0.1, inner_radius, theta0),
                shade_vertex(sky, inner1.0, inner1.1, inner_radius, theta1),
                shade_vertex(sky, outer0.0, outer0.1, outer_radius, theta0),
            ],
        );
        push_sky_cyclorama_triangle(
            out,
            [inner1, outer1, outer0],
            [
                shade_vertex(sky, inner1.0, inner1.1, inner_radius, theta1),
                shade_vertex(sky, outer1.0, outer1.1, outer_radius, theta1),
                shade_vertex(sky, outer0.0, outer0.1, outer_radius, theta0),
            ],
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn sun_polar_point(
    center_yaw: f32,
    center_pitch: f32,
    yaw_radius: f32,
    pitch_radius: f32,
    radius: f32,
    theta: f32,
    shape_phase: f32,
    shape_strength: f32,
) -> (f32, f32) {
    let shape = sun_shape_scale(theta, shape_phase, shape_strength);
    let radius = radius * shape;
    (
        center_yaw + theta.cos() * yaw_radius * radius,
        center_pitch + theta.sin() * pitch_radius * radius,
    )
}

fn sun_shape_scale(theta: f32, phase: f32, strength: f32) -> f32 {
    let wave = 0.08 * (theta * 3.0 + phase).sin()
        + 0.05 * (theta * 5.0 - phase * 0.7).cos()
        + 0.035 * (theta * 9.0 + phase * 1.6).sin();
    (1.0 + wave * strength).clamp(0.72, 1.24)
}

fn sun_directional_weight(theta: f32, direction: f32, power: f32) -> f32 {
    theta
        .cos()
        .mul_add(direction.cos(), theta.sin() * direction.sin())
        .max(0.0)
        .powf(power.max(0.01))
}

#[allow(clippy::too_many_arguments)]
fn sun_tinted_sky_color(
    sky: ResolvedSkySettings,
    yaw: f32,
    pitch: f32,
    tint: [u8; 3],
    alpha: f32,
    horizon_pitch: f32,
    top_pitch: f32,
    bottom_pitch: f32,
) -> [u8; 3] {
    let base =
        sky_color_for_pitch_yaw_core(sky, pitch, yaw, horizon_pitch, top_pitch, bottom_pitch);
    cyclorama_lerp_rgb(base, tint, alpha.clamp(0.0, 255.0) as u8)
}

fn push_star_cyclorama(
    out: &mut Vec<SkyCycloramaQuad>,
    sky: ResolvedSkySettings,
    horizon_pitch: f32,
    top_pitch: f32,
    bottom_pitch: f32,
) {
    let darkness = (1.0 - rgb_luma(sky.top_color) / 118.0).clamp(0.0, 1.0);
    if darkness <= 0.08 {
        return;
    }
    let upper_bottom = (horizon_pitch + 12.0).max(bottom_pitch + 34.0);
    let upper_top = top_pitch - 3.0;
    if upper_top <= upper_bottom + 4.0 {
        return;
    }
    let cloud = sky.cloud_layer;
    let density_t = if cloud.enabled {
        cloud_density_response(cloud.density)
    } else {
        0.0
    };
    let count = (18.0 + darkness * 34.0 + (1.0 - density_t) * 12.0).round() as usize;
    let count = count.clamp(8, SKY_CYCLORAMA_STAR_COUNT_MAX);
    let seed = cloud.noise_seed ^ 0x7374_6172;
    for star in 0..count {
        let h = sky_hash_u32(seed, star as u32);
        let yaw = -180.0 + sky_hash_unit(h, 0) * 360.0;
        let height_t = sky_hash_unit(h, 1).powf(0.55);
        let pitch = lerp_f32(upper_bottom, upper_top, height_t);
        let twinkle = 0.45 + sky_hash_unit(h, 2) * 0.55;
        let size = (0.1 + sky_hash_unit(h, 3) * 0.2) * (0.8 + twinkle * 0.5);
        if yaw - size <= -180.0 || yaw + size >= 180.0 {
            continue;
        }
        let base =
            sky_color_for_pitch_yaw_core(sky, pitch, yaw, horizon_pitch, top_pitch, bottom_pitch);
        let cool = cyclorama_lerp_rgb([205, 218, 255], [255, 232, 190], sky_hash_u32(h, 4) as u8);
        let alpha = (120.0 + darkness * 92.0 + twinkle * 42.0).clamp(0.0, 255.0) as u8;
        let star_rgb = cyclorama_lerp_rgb(base, cool, alpha);
        push_sky_cyclorama_quad_corners(
            out,
            yaw - size,
            yaw + size,
            pitch + size * 0.72,
            pitch + size * 0.72,
            pitch - size * 0.72,
            pitch - size * 0.72,
            [star_rgb; 4],
        );
    }
}

fn push_mountain_cyclorama(
    out: &mut Vec<SkyCycloramaQuad>,
    sky: ResolvedSkySettings,
    columns: usize,
    horizon_pitch: f32,
) {
    if sky.mountain_height_percent == 0 {
        return;
    }
    let mountain_columns = (columns * 5).clamp(40, SKY_CYCLORAMA_MOUNTAIN_COLUMNS_MAX);
    let height_t = sky
        .mountain_height_percent
        .clamp(0, SKY_MOUNTAIN_HEIGHT_PERCENT_MAX) as f32
        / 100.0;
    let layer_count = sky
        .mountain_layer_count
        .clamp(1, SKY_CYCLORAMA_MOUNTAIN_LAYERS as u8);
    let seed = sky.cloud_layer.noise_seed ^ 0x6d2b_79f5;
    for layer in 0..usize::from(layer_count) {
        let depth_t = (layer + 1) as f32 / layer_count as f32;
        let layer_seed = seed ^ sky_hash_u32(0xa341_316c, layer as u32);
        for column in 0..mountain_columns {
            let yaw0 = cyclorama_yaw_for_column(column, mountain_columns);
            let yaw1 = cyclorama_yaw_for_column(column + 1, mountain_columns);
            push_mountain_layer_cyclorama(
                out,
                layer_seed,
                sky,
                yaw0,
                yaw1,
                horizon_pitch,
                height_t,
                depth_t,
            );
        }
    }
}

fn push_mountain_layer_cyclorama(
    out: &mut Vec<SkyCycloramaQuad>,
    seed: u32,
    sky: ResolvedSkySettings,
    yaw0: f32,
    yaw1: f32,
    horizon_pitch: f32,
    height_t: f32,
    depth_t: f32,
) {
    let phase = 9.0 + depth_t * 19.0;
    let gap_t = sky.mountain_gap_percent.clamp(0, 100) as f32 / 100.0;
    let rough_t = sky.mountain_roughness_percent.clamp(0, 100) as f32 / 100.0;
    let gap_degrees = lerp_f32(-7.0, 18.0, gap_t) + depth_t * 3.0;
    let top_base = horizon_pitch - gap_degrees;
    let amplitude = (4.5 + rough_t * 10.5 + depth_t * 4.0) * height_t;
    let base_pitch = top_base - (13.0 + height_t * 26.0 + depth_t * 8.0);
    let top0 = top_base + mountain_profile(seed, yaw0 + phase, rough_t) * amplitude;
    let top1 = top_base + mountain_profile(seed, yaw1 + phase, rough_t) * amplitude;
    let peak = cyclorama_lerp_rgb(
        sky.horizon_color,
        sky.mountain_top_color,
        (72.0 + depth_t * 118.0) as u8,
    );
    let base = cyclorama_lerp_rgb(
        sky.lower_color,
        sky.mountain_base_color,
        (96.0 + depth_t * 116.0) as u8,
    );
    push_sky_cyclorama_quad_corners(
        out,
        yaw0,
        yaw1,
        top0,
        top1,
        base_pitch,
        base_pitch,
        [peak, peak, base, base],
    );
}

fn push_cloud_streak_cyclorama(
    out: &mut Vec<SkyCycloramaQuad>,
    sky: ResolvedSkySettings,
    horizon_pitch: f32,
    top_pitch: f32,
    bottom_pitch: f32,
) {
    let cloud = sky.cloud_layer;
    if !cloud.enabled || cloud.density == 0 {
        return;
    }
    let tile_count = cloud.tile_count.clamp(1, 16);
    let altitude_t = (cloud.altitude as f32 / u16::MAX as f32).clamp(0.0, 1.0);
    let extent_t = (cloud.extent as f32 / u16::MAX as f32).clamp(0.0, 1.0);
    let detail_t = (tile_count.saturating_sub(1) as f32 / 15.0).clamp(0.0, 1.0);
    let segment_count = (6 + usize::from(tile_count / 4) + usize::from(cloud.density / 128))
        .clamp(6, SKY_CYCLORAMA_CLOUD_SEGMENTS_MAX);
    let count = (3 + usize::from(cloud.density / 64) + usize::from(tile_count / 8))
        .min(SKY_CYCLORAMA_CLOUD_STREAK_MAX);
    let density_t = cloud_density_response(cloud.density);
    let band_center = horizon_pitch + 4.0 + altitude_t * 28.0;
    let pitch_spread = 3.5 + extent_t * 18.0;
    let width_scale = 0.55 + extent_t * 0.88;
    let repeat_scale = 1.05 + detail_t * 0.45;
    let hero_yaw = sky.horizon_glow_yaw_degrees as f32;
    for (bank, offset) in [-92.0_f32, -34.0, 28.0, 88.0].iter().enumerate() {
        let bank_t = bank as f32 / (SKY_CYCLORAMA_CLOUD_HERO_STREAKS - 1).max(1) as f32;
        let width = (72.0 + bank_t * 42.0) * width_scale.min(1.25);
        let center_pitch = band_center + (bank_t - 0.6) * pitch_spread * 0.36;
        let thickness = 1.25 + extent_t * (2.75 + bank_t * 0.95);
        let slant = -4.0 + bank_t * 5.8;
        let tint = cyclorama_lerp_rgb(cloud.color, [255, 166, 150], (64.0 + bank_t * 38.0) as u8);
        push_cloud_streak_segments(
            out,
            sky,
            hero_yaw + offset - width * 0.5,
            width,
            center_pitch,
            thickness,
            slant,
            tint,
            density_t,
            0.96,
            segment_count,
            cloud.noise_seed ^ sky_hash_u32(0x27d4eb2d, bank as u32),
            horizon_pitch,
            top_pitch,
            bottom_pitch,
        );
    }
    for streak in 0..count {
        let h = sky_hash_u32(cloud.noise_seed, streak as u32);
        let yaw_start = -180.0 + sky_hash_unit(h, 0) * 360.0;
        let width = (30.0 + sky_hash_unit(h, 1) * 74.0) * width_scale / repeat_scale;
        let center_pitch = band_center + (sky_hash_unit(h, 2) - 0.5) * pitch_spread;
        let thickness = (1.05 + sky_hash_unit(h, 3) * 3.3) / (0.9 + tile_count as f32 * 0.02);
        let slant = (-6.0 + sky_hash_unit(h, 4) * 12.0) * (0.6 + extent_t * 0.45);
        let tint = cyclorama_lerp_rgb(
            cloud.color,
            [255, 170, 142],
            (32.0 + sky_hash_unit(h, 5) * 92.0) as u8,
        );
        push_cloud_streak_segments(
            out,
            sky,
            yaw_start,
            width,
            center_pitch,
            thickness,
            slant,
            tint,
            density_t,
            1.0,
            segment_count,
            h,
            horizon_pitch,
            top_pitch,
            bottom_pitch,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn push_cloud_streak_segments(
    out: &mut Vec<SkyCycloramaQuad>,
    sky: ResolvedSkySettings,
    yaw_start: f32,
    width: f32,
    center_pitch: f32,
    thickness: f32,
    slant: f32,
    tint: [u8; 3],
    density_t: f32,
    alpha_scale: f32,
    segment_count: usize,
    seed: u32,
    horizon_pitch: f32,
    top_pitch: f32,
    bottom_pitch: f32,
) {
    let shadow = cyclorama_lerp_rgb(tint, sky.lower_color, 58);
    let body = brighten_rgb(cyclorama_lerp_rgb(tint, [255, 190, 166], 82), 4);
    let warm = brighten_rgb(cyclorama_lerp_rgb(tint, [255, 222, 198], 168), 12);
    let segment_count = segment_count.clamp(2, SKY_CYCLORAMA_CLOUD_SEGMENTS_MAX);
    for segment in 0..segment_count {
        let t0 = segment as f32 / segment_count as f32;
        let t1 = (segment + 1) as f32 / segment_count as f32;
        let yaw0 = yaw_start + width * t0;
        let yaw1 = yaw_start + width * t1;
        let pitch0 =
            center_pitch + slant * (t0 - 0.5) + cloud_lobe_pitch(seed ^ 0x9e37_79b9, t0, thickness);
        let pitch1 =
            center_pitch + slant * (t1 - 0.5) + cloud_lobe_pitch(seed ^ 0x9e37_79b9, t1, thickness);
        let fade0 = cloud_band_alpha(seed, t0, density_t, alpha_scale);
        let fade1 = cloud_band_alpha(seed, t1, density_t, alpha_scale);
        if fade0 <= 0.015 && fade1 <= 0.015 {
            continue;
        }
        let width0 = cloud_band_width(seed, t0);
        let width1 = cloud_band_width(seed, t1);
        let segment_thickness = thickness * ((width0 + width1) * 0.5);
        push_wrapped_cloud_ribbon_cyclorama(
            out,
            sky,
            yaw0,
            yaw1,
            pitch0 - segment_thickness * 0.18,
            pitch1 - segment_thickness * 0.18,
            segment_thickness * 1.42,
            shadow,
            fade0 * 78.0,
            fade1 * 78.0,
            horizon_pitch,
            top_pitch,
            bottom_pitch,
        );
        push_wrapped_cloud_ribbon_cyclorama(
            out,
            sky,
            yaw0,
            yaw1,
            pitch0,
            pitch1,
            segment_thickness * 0.84,
            body,
            fade0 * 154.0,
            fade1 * 154.0,
            horizon_pitch,
            top_pitch,
            bottom_pitch,
        );
        push_wrapped_cloud_ribbon_cyclorama(
            out,
            sky,
            yaw0,
            yaw1,
            pitch0 + segment_thickness * 0.18,
            pitch1 + segment_thickness * 0.18,
            segment_thickness * 0.2,
            warm,
            fade0 * 235.0,
            fade1 * 235.0,
            horizon_pitch,
            top_pitch,
            bottom_pitch,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn push_wrapped_cloud_ribbon_cyclorama(
    out: &mut Vec<SkyCycloramaQuad>,
    sky: ResolvedSkySettings,
    yaw0: f32,
    yaw1: f32,
    pitch0: f32,
    pitch1: f32,
    half_thickness: f32,
    tint: [u8; 3],
    alpha0: f32,
    alpha1: f32,
    horizon_pitch: f32,
    top_pitch: f32,
    bottom_pitch: f32,
) {
    let mut start = yaw0;
    let mut end = yaw1;
    while start < -180.0 {
        start += 360.0;
        end += 360.0;
    }
    while start >= 180.0 {
        start -= 360.0;
        end -= 360.0;
    }
    if end <= 180.0 {
        push_cloud_ribbon_cyclorama(
            out,
            sky,
            start,
            end,
            pitch0,
            pitch1,
            half_thickness,
            tint,
            alpha0,
            alpha1,
            horizon_pitch,
            top_pitch,
            bottom_pitch,
        );
        return;
    }

    let t = ((180.0 - start) / (end - start).max(0.001)).clamp(0.0, 1.0);
    let split_pitch = lerp_f32(pitch0, pitch1, t);
    let split_alpha = lerp_f32(alpha0, alpha1, t);
    push_cloud_ribbon_cyclorama(
        out,
        sky,
        start,
        180.0,
        pitch0,
        split_pitch,
        half_thickness,
        tint,
        alpha0,
        split_alpha,
        horizon_pitch,
        top_pitch,
        bottom_pitch,
    );
    push_cloud_ribbon_cyclorama(
        out,
        sky,
        -180.0,
        end - 360.0,
        split_pitch,
        pitch1,
        half_thickness,
        tint,
        split_alpha,
        alpha1,
        horizon_pitch,
        top_pitch,
        bottom_pitch,
    );
}

fn push_cloud_ribbon_cyclorama(
    out: &mut Vec<SkyCycloramaQuad>,
    sky: ResolvedSkySettings,
    yaw0: f32,
    yaw1: f32,
    pitch0: f32,
    pitch1: f32,
    half_thickness: f32,
    tint: [u8; 3],
    alpha0: f32,
    alpha1: f32,
    horizon_pitch: f32,
    top_pitch: f32,
    bottom_pitch: f32,
) {
    let width0 = cloud_width_fade(alpha0);
    let width1 = cloud_width_fade(alpha1);
    let top0 = pitch0 + half_thickness * width0;
    let top1 = pitch1 + half_thickness * width1;
    let bottom0 = pitch0 - half_thickness * width0;
    let bottom1 = pitch1 - half_thickness * width1;
    let center0 = pitch0;
    let center1 = pitch1;
    let base_top0 =
        sky_color_for_pitch_yaw_core(sky, top0, yaw0, horizon_pitch, top_pitch, bottom_pitch);
    let base_top1 =
        sky_color_for_pitch_yaw_core(sky, top1, yaw1, horizon_pitch, top_pitch, bottom_pitch);
    let base_center0 =
        sky_color_for_pitch_yaw_core(sky, center0, yaw0, horizon_pitch, top_pitch, bottom_pitch);
    let base_center1 =
        sky_color_for_pitch_yaw_core(sky, center1, yaw1, horizon_pitch, top_pitch, bottom_pitch);
    let base_bottom0 =
        sky_color_for_pitch_yaw_core(sky, bottom0, yaw0, horizon_pitch, top_pitch, bottom_pitch);
    let base_bottom1 =
        sky_color_for_pitch_yaw_core(sky, bottom1, yaw1, horizon_pitch, top_pitch, bottom_pitch);
    let center_tint0 = cyclorama_lerp_rgb(base_center0, tint, alpha0.clamp(0.0, 255.0) as u8);
    let center_tint1 = cyclorama_lerp_rgb(base_center1, tint, alpha1.clamp(0.0, 255.0) as u8);
    push_sky_cyclorama_quad_corners(
        out,
        yaw0,
        yaw1,
        top0,
        top1,
        center0,
        center1,
        [base_top0, base_top1, center_tint0, center_tint1],
    );
    push_sky_cyclorama_quad_corners(
        out,
        yaw0,
        yaw1,
        center0,
        center1,
        bottom0,
        bottom1,
        [center_tint0, center_tint1, base_bottom0, base_bottom1],
    );
}

fn push_sky_cyclorama_quad(
    out: &mut Vec<SkyCycloramaQuad>,
    yaw0: f32,
    yaw1: f32,
    pitch_top: f32,
    pitch_bottom: f32,
    rgb: [[u8; 3]; 4],
) {
    push_sky_cyclorama_quad_corners(
        out,
        yaw0,
        yaw1,
        pitch_top,
        pitch_top,
        pitch_bottom,
        pitch_bottom,
        rgb,
    );
}

fn push_sky_cyclorama_quad_corners(
    out: &mut Vec<SkyCycloramaQuad>,
    yaw0: f32,
    yaw1: f32,
    pitch_top0: f32,
    pitch_top1: f32,
    pitch_bottom0: f32,
    pitch_bottom1: f32,
    rgb: [[u8; 3]; 4],
) {
    if out.len() >= SKY_CYCLORAMA_QUAD_MAX {
        return;
    }
    out.push(SkyCycloramaQuad {
        direction_q12: [
            cyclorama_direction_q12(yaw0, pitch_top0),
            cyclorama_direction_q12(yaw1, pitch_top1),
            cyclorama_direction_q12(yaw0, pitch_bottom0),
            cyclorama_direction_q12(yaw1, pitch_bottom1),
        ],
        rgb,
    });
}

fn push_sky_cyclorama_triangle(
    out: &mut Vec<SkyCycloramaQuad>,
    points: [(f32, f32); 3],
    rgb: [[u8; 3]; 3],
) {
    if out.len() >= SKY_CYCLORAMA_QUAD_MAX {
        return;
    }
    out.push(SkyCycloramaQuad {
        direction_q12: [
            cyclorama_direction_q12(points[0].0, points[0].1),
            cyclorama_direction_q12(points[1].0, points[1].1),
            cyclorama_direction_q12(points[2].0, points[2].1),
            cyclorama_direction_q12(points[2].0, points[2].1),
        ],
        rgb: [rgb[0], rgb[1], rgb[2], rgb[2]],
    });
}

fn cyclorama_direction_q12(yaw_degrees: f32, pitch_degrees: f32) -> [i16; 3] {
    let yaw = yaw_degrees.to_radians();
    let pitch = pitch_degrees.clamp(-82.0, 82.0).to_radians();
    let cp = pitch.cos();
    let scale = 4096.0;
    [
        (-yaw.sin() * cp * scale).round() as i16,
        (pitch.sin() * scale).round() as i16,
        (-yaw.cos() * cp * scale).round() as i16,
    ]
}

fn sky_horizon_pitch_degrees(horizon_percent: u8) -> f32 {
    let y = 120.0 - 240.0 * (horizon_percent.clamp(5, 95) as f32 / 100.0);
    (y / 320.0).atan().to_degrees()
}

fn sky_color_for_pitch(
    sky: ResolvedSkySettings,
    pitch: f32,
    horizon_pitch: f32,
    top_pitch: f32,
    bottom_pitch: f32,
) -> [u8; 3] {
    let base = if pitch >= horizon_pitch {
        let span = (top_pitch - horizon_pitch).max(1.0);
        let t = smooth_step(((pitch - horizon_pitch) / span).clamp(0.0, 1.0));
        cyclorama_lerp_rgb(sky.horizon_color, sky.top_color, (t * 255.0) as u8)
    } else {
        let span = (horizon_pitch - bottom_pitch).max(1.0);
        let t = smooth_step(((horizon_pitch - pitch) / span).clamp(0.0, 1.0));
        cyclorama_lerp_rgb(sky.horizon_color, sky.lower_color, (t * 255.0) as u8)
    };
    let hold_radius = 1.4 + sky.horizon_thickness_percent.clamp(0, 80) as f32 * 0.13;
    let hold = smooth_falloff(hold_radius, (pitch - horizon_pitch).abs());
    cyclorama_lerp_rgb(
        base,
        sky.horizon_color,
        (hold * 92.0).clamp(0.0, 255.0) as u8,
    )
}

fn sky_color_for_pitch_yaw(
    sky: ResolvedSkySettings,
    pitch: f32,
    yaw: f32,
    horizon_pitch: f32,
    top_pitch: f32,
    bottom_pitch: f32,
) -> [u8; 3] {
    let color =
        sky_color_for_pitch_yaw_core(sky, pitch, yaw, horizon_pitch, top_pitch, bottom_pitch);
    sky_cloud_wash_color(sky, color, pitch, yaw, horizon_pitch)
}

fn sky_color_for_pitch_yaw_core(
    sky: ResolvedSkySettings,
    pitch: f32,
    yaw: f32,
    horizon_pitch: f32,
    top_pitch: f32,
    bottom_pitch: f32,
) -> [u8; 3] {
    let base = sky_color_for_pitch(sky, pitch, horizon_pitch, top_pitch, bottom_pitch);
    let mut color = base;
    let pitch_delta = (pitch - horizon_pitch).abs();
    let pitch_weight = smooth_falloff(27.0, pitch_delta);
    if sky.horizon_glow_percent > 0 && pitch_weight > 0.0 {
        let yaw_delta = angular_distance_degrees(yaw, sky.horizon_glow_yaw_degrees as f32);
        let yaw_weight = smooth_falloff(105.0, yaw_delta);
        let strength =
            (sky.horizon_glow_percent.clamp(0, 100) as f32 / 100.0) * pitch_weight * yaw_weight;
        if strength > 0.0 {
            color = cyclorama_lerp_rgb(
                color,
                horizon_glow_color_for_yaw(sky, yaw),
                (strength * 156.0).clamp(0.0, 255.0) as u8,
            );
        }
    }
    color
}

fn horizon_glow_color_for_yaw(sky: ResolvedSkySettings, yaw: f32) -> [u8; 3] {
    let yaw_delta = angular_distance_degrees(yaw, sky.horizon_glow_yaw_degrees as f32);
    let hot = smooth_falloff(42.0, yaw_delta);
    let warm = cyclorama_lerp_rgb(sky.horizon_color, [255, 174, 94], 188);
    let pink = cyclorama_lerp_rgb(sky.horizon_color, [226, 118, 172], 132);
    brighten_rgb(cyclorama_lerp_rgb(pink, warm, (hot * 255.0) as u8), 10)
}

fn sky_cloud_wash_color(
    sky: ResolvedSkySettings,
    base: [u8; 3],
    pitch: f32,
    yaw: f32,
    horizon_pitch: f32,
) -> [u8; 3] {
    let cloud = sky.cloud_layer;
    if !cloud.enabled || cloud.density == 0 {
        return base;
    }
    let altitude_t = (cloud.altitude as f32 / u16::MAX as f32).clamp(0.0, 1.0);
    let extent_t = (cloud.extent as f32 / u16::MAX as f32).clamp(0.0, 1.0);
    let tile_count = cloud.tile_count.clamp(1, 16) as f32;
    let density_t = cloud.density as f32 / 255.0;
    let center = horizon_pitch + 4.0 + altitude_t * 28.0 + cloud_band_wave(cloud.noise_seed, yaw);
    let width = 8.0 + extent_t * 16.0;
    let pitch_weight = smooth_falloff(width, (pitch - center).abs());
    if pitch_weight <= 0.0 {
        return base;
    }
    let phase = (cloud.noise_seed & 0xff) as f32 * 0.037;
    let yaw_r = yaw.to_radians();
    let yaw_weight = 0.58
        + 0.24 * (yaw_r * (tile_count * 0.38) + phase).sin()
        + 0.18 * (yaw_r * (tile_count * 0.71) + phase * 1.7).sin();
    let strength = (density_t * pitch_weight * yaw_weight.clamp(0.18, 1.0)).clamp(0.0, 1.0);
    let tint = cyclorama_lerp_rgb(cloud.color, [255, 180, 148], (strength * 96.0) as u8);
    cyclorama_lerp_rgb(base, tint, (strength * 34.0).clamp(0.0, 255.0) as u8)
}

fn cyclorama_yaw_for_column(column: usize, columns: usize) -> f32 {
    -180.0 + 360.0 * (column as f32 / columns.max(1) as f32)
}

fn angular_distance_degrees(a: f32, b: f32) -> f32 {
    let mut d = (a - b).abs() % 360.0;
    if d > 180.0 {
        d = 360.0 - d;
    }
    d
}

fn smooth_falloff(radius: f32, distance: f32) -> f32 {
    let t = (1.0 - distance / radius.max(0.001)).clamp(0.0, 1.0);
    smooth_step(t)
}

fn smooth_step(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

fn cloud_width_fade(alpha: f32) -> f32 {
    (alpha / 150.0).clamp(0.0, 1.0).sqrt()
}

fn cloud_density_response(density: u8) -> f32 {
    (density as f32 / 255.0).clamp(0.0, 1.0).powf(0.58)
}

fn mountain_profile(seed: u32, yaw_degrees: f32, roughness: f32) -> f32 {
    let roughness = roughness.clamp(0.0, 1.0);
    let spacing = lerp_f32(68.0, 34.0, roughness);
    let phase = (seed & 0xff) as f32 * 0.17;
    let x = (yaw_degrees + 540.0 + phase) / spacing;
    let broad = mountain_value_noise(seed ^ 0x52dc_e729, x * 0.62);
    let mid = mountain_value_noise(seed ^ 0x9e37_79b9, x * (1.12 + roughness * 0.45));
    let fine = mountain_value_noise(seed ^ 0x85eb_ca6b, x * (2.35 + roughness * 1.3));
    let wave = 0.5 + 0.5 * ((yaw_degrees.to_radians() * 1.22) + phase * 0.09).sin();
    let ridge =
        broad * 0.5 + mid * (0.34 + roughness * 0.08) + fine * (roughness * 0.12) + wave * 0.04;
    smooth_step(((ridge - 0.18) / 0.82).clamp(0.0, 1.0)).powf(lerp_f32(1.0, 0.82, roughness))
}

fn mountain_value_noise(seed: u32, x: f32) -> f32 {
    let cell = x.floor() as i32;
    let t = smooth_step(x - cell as f32);
    let a = sky_hash_unit(seed, cell as u32);
    let b = sky_hash_unit(seed, cell.wrapping_add(1) as u32);
    lerp_f32(a, b, t)
}

fn cloud_streak_fade(t: f32) -> f32 {
    (core::f32::consts::PI * t).sin().clamp(0.0, 1.0)
}

fn cloud_lobe_weight(seed: u32, t: f32) -> f32 {
    let phase0 = (seed & 0xff) as f32 * 0.037;
    let phase1 = ((seed >> 8) & 0xff) as f32 * 0.029;
    let a = (core::f32::consts::TAU * (t * 2.0 + phase0)).sin();
    let b = (core::f32::consts::TAU * (t * 3.0 + phase1)).sin();
    (0.62 + 0.25 * a + 0.13 * b).clamp(0.18, 1.0)
}

fn cloud_lobe_pitch(seed: u32, t: f32, thickness: f32) -> f32 {
    let phase = ((seed >> 16) & 0xff) as f32 * 0.041;
    (core::f32::consts::TAU * (t * 1.5 + phase)).sin() * thickness * 0.36
}

fn cloud_band_alpha(seed: u32, t: f32, density_t: f32, alpha_scale: f32) -> f32 {
    cloud_streak_fade(t).powf(0.58)
        * cloud_lobe_weight(seed ^ 0x1b56_c4e9, t)
        * density_t
        * alpha_scale
}

fn cloud_band_width(seed: u32, t: f32) -> f32 {
    let phase0 = (seed & 0xff) as f32 * 0.023;
    let phase1 = ((seed >> 8) & 0xff) as f32 * 0.031;
    let a = (core::f32::consts::TAU * (t * 2.0 + phase0)).sin();
    let b = (core::f32::consts::TAU * (t * 4.0 + phase1)).sin();
    (0.72 + 0.2 * a + 0.08 * b).clamp(0.46, 1.18)
}

fn cloud_band_wave(seed: u32, yaw_degrees: f32) -> f32 {
    let r = yaw_degrees.to_radians();
    let phase0 = (seed & 0xff) as f32 * 0.019;
    let phase1 = ((seed >> 8) & 0xff) as f32 * 0.023;
    (r * 2.0 + phase0).sin() * 1.6 + (r * 5.0 + phase1).sin() * 0.72
}

fn sky_hash_u32(seed: u32, value: u32) -> u32 {
    let mut h = seed ^ value.wrapping_mul(0x9e37_79b9);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^ (h >> 16)
}

fn sky_hash_unit(seed: u32, value: u32) -> f32 {
    (sky_hash_u32(seed, value) >> 8) as f32 / 16_777_215.0
}

fn lerp_f32(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn cyclorama_lerp_rgb(a: [u8; 3], b: [u8; 3], t: u8) -> [u8; 3] {
    let inv = 255 - t as u16;
    let t = t as u16;
    [
        ((a[0] as u16 * inv + b[0] as u16 * t) / 255) as u8,
        ((a[1] as u16 * inv + b[1] as u16 * t) / 255) as u8,
        ((a[2] as u16 * inv + b[2] as u16 * t) / 255) as u8,
    ]
}

fn rgb_luma(rgb: [u8; 3]) -> f32 {
    rgb[0] as f32 * 0.2126 + rgb[1] as f32 * 0.7152 + rgb[2] as f32 * 0.0722
}

fn brighten_rgb(rgb: [u8; 3], amount: u8) -> [u8; 3] {
    [
        rgb[0].saturating_add(amount),
        rgb[1].saturating_add(amount),
        rgb[2].saturating_add(amount),
    ]
}

fn blend_rgb(a: [u8; 3], b: [u8; 3], b_weight_256: u16) -> [u8; 3] {
    let weight = b_weight_256.min(256);
    let inv = 256 - weight;
    [
        (((a[0] as u16 * inv) + (b[0] as u16 * weight)) >> 8) as u8,
        (((a[1] as u16 * inv) + (b[1] as u16 * weight)) >> 8) as u8,
        (((a[2] as u16 * inv) + (b[2] as u16 * weight)) >> 8) as u8,
    ]
}

/// Distant scenery ring configuration inherited by descendant Rooms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FarVistaSettings {
    /// Whether the far vista ring should be drawn.
    #[serde(default)]
    pub enabled: bool,
    /// Optional transparent 4bpp texture slice repeated around the
    /// ring. When missing, renderers draw a tinted placeholder band.
    #[serde(default)]
    pub texture: Option<ResourceId>,
    /// Optional per-card transparent 4bpp textures. Non-empty panel
    /// assignments take precedence over [`Self::texture`].
    #[serde(default = "default_far_vista_texture_panels")]
    pub texture_panels: [Option<ResourceId>; FAR_VISTA_TEXTURE_PANEL_COUNT],
    /// Radius from the active camera/player in engine units.
    #[serde(default = "default_far_vista_radius")]
    pub radius: i32,
    /// Ring height in engine units.
    #[serde(default = "default_far_vista_height")]
    pub height: i32,
    /// Bottom-edge offset from the camera height in engine units.
    #[serde(default = "default_far_vista_vertical_offset")]
    pub vertical_offset: i32,
    /// Number of cards around the cylinder.
    #[serde(default = "default_far_vista_segments")]
    pub segments: u8,
    /// World yaw rotation in degrees.
    #[serde(default)]
    pub rotation_degrees: i16,
    /// Flat tint used for placeholder cards and textured modulation.
    #[serde(default = "default_far_vista_tint")]
    pub tint: [u8; 3],
    /// Blend tint toward the room fog colour when fog is enabled.
    #[serde(default = "default_far_vista_match_room_fog")]
    pub match_room_fog: bool,
}

impl FarVistaSettings {
    /// Resolve authored far-vista values against room-local fog metadata.
    pub fn resolved_for_room(
        self,
        fog_enabled: bool,
        fog_color: [u8; 3],
    ) -> ResolvedFarVistaSettings {
        let tint = if self.match_room_fog && fog_enabled {
            blend_rgb(self.tint, fog_color, 128)
        } else {
            self.tint
        };
        ResolvedFarVistaSettings {
            enabled: self.enabled,
            texture: self.texture,
            texture_panels: self.texture_panels,
            radius: self.radius.clamp(1_024, 65_535),
            height: self.height.clamp(128, 32_768),
            vertical_offset: self.vertical_offset.clamp(-32_768, 32_768),
            segments: self.segments.clamp(3, 16),
            rotation_degrees: self.rotation_degrees,
            tint,
        }
    }
}

impl Default for FarVistaSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            texture: None,
            texture_panels: default_far_vista_texture_panels(),
            radius: default_far_vista_radius(),
            height: default_far_vista_height(),
            vertical_offset: default_far_vista_vertical_offset(),
            segments: default_far_vista_segments(),
            rotation_degrees: 0,
            tint: default_far_vista_tint(),
            match_room_fog: default_far_vista_match_room_fog(),
        }
    }
}

/// Far-vista values after room-fog matching and validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedFarVistaSettings {
    /// Whether the ring should be drawn.
    pub enabled: bool,
    /// Optional transparent texture slice.
    pub texture: Option<ResourceId>,
    /// Optional per-card transparent texture slices.
    pub texture_panels: [Option<ResourceId>; FAR_VISTA_TEXTURE_PANEL_COUNT],
    /// Radius from camera/player in engine units.
    pub radius: i32,
    /// Ring height in engine units.
    pub height: i32,
    /// Bottom-edge offset from camera height in engine units.
    pub vertical_offset: i32,
    /// Number of cards around the cylinder.
    pub segments: u8,
    /// World yaw rotation in degrees.
    pub rotation_degrees: i16,
    /// Resolved tint.
    pub tint: [u8; 3],
}

/// World-level third-person camera configuration inherited by
/// descendant Rooms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldCameraSettings {
    /// Preferred trailing distance from focus to camera.
    #[serde(default = "default_world_camera_distance")]
    pub distance: i32,
    /// Camera origin height above the player origin.
    #[serde(default = "default_world_camera_height")]
    pub height: i32,
    /// Look-at height above the player origin.
    #[serde(default = "default_world_camera_target_height")]
    pub target_height: i32,
    /// Minimum camera origin height above the sampled floor.
    #[serde(default = "default_world_camera_min_floor_clearance")]
    pub min_floor_clearance: i32,
}

impl WorldCameraSettings {
    /// Clamp authored values to runtime-safe third-person camera ranges.
    pub fn normalized(self) -> Self {
        Self {
            distance: self
                .distance
                .clamp(MIN_WORLD_CAMERA_DISTANCE, MAX_WORLD_CAMERA_DISTANCE),
            height: self.height.clamp(0, MAX_WORLD_CAMERA_HEIGHT),
            target_height: self.target_height.clamp(0, MAX_WORLD_CAMERA_HEIGHT),
            min_floor_clearance: self
                .min_floor_clearance
                .clamp(0, MAX_WORLD_CAMERA_MIN_FLOOR_CLEARANCE),
        }
    }
}

impl Default for WorldCameraSettings {
    fn default() -> Self {
        Self {
            distance: default_world_camera_distance(),
            height: default_world_camera_height(),
            target_height: default_world_camera_target_height(),
            min_floor_clearance: default_world_camera_min_floor_clearance(),
        }
    }
}

/// Minimum camera-space far plane used by runtime world drawing.
pub const MIN_WORLD_DRAW_DISTANCE: i32 = 4_096;
/// Maximum camera-space far plane exposed for playtest experimentation.
pub const MAX_WORLD_DRAW_DISTANCE: i32 = 262_144;
/// Minimum active streamed chunk radius, in world sectors.
pub const MIN_WORLD_CHUNK_ACTIVATION_RADIUS_SECTORS: i32 = 4;
/// Maximum active streamed chunk radius, in world sectors.
pub const MAX_WORLD_CHUNK_ACTIVATION_RADIUS_SECTORS: i32 = 256;
/// Minimum precomputed cell-visibility traversal radius.
pub const MIN_WORLD_VISIBILITY_RADIUS: u16 = 4;
/// Maximum precomputed cell-visibility traversal radius.
pub const MAX_WORLD_VISIBILITY_RADIUS: u16 = 96;
/// Smallest resident portal-room budget accepted by the runtime.
/// One portal needs at least current + adjacent room residency.
pub const MIN_WORLD_STREAMING_RESIDENT_CHUNKS: u8 = 2;
/// Default portal-room residency budget used by the playtest runtime.
pub const DEFAULT_WORLD_STREAMING_RESIDENT_CHUNKS: u8 = 10;
/// Largest portal-room residency budget supported by the current runtime.
pub const MAX_WORLD_STREAMING_RESIDENT_CHUNKS: u8 = 32;
/// Smallest portal-room visible-window budget accepted by the runtime.
pub const MIN_WORLD_STREAMING_VISIBLE_CHUNKS: u8 = 2;
/// Default portal-room visible-window budget used by the playtest runtime.
pub const DEFAULT_WORLD_STREAMING_VISIBLE_CHUNKS: u8 = DEFAULT_WORLD_STREAMING_RESIDENT_CHUNKS;
/// Largest portal-room visible-window budget supported by the current runtime.
pub const MAX_WORLD_STREAMING_VISIBLE_CHUNKS: u8 = 32;

const fn default_world_draw_distance() -> i32 {
    25_000
}

const fn default_world_chunk_activation_radius_sectors() -> i32 {
    64
}

const fn default_world_visibility_radius() -> u16 {
    32
}

const fn default_world_streaming_resident_chunks() -> u8 {
    DEFAULT_WORLD_STREAMING_RESIDENT_CHUNKS
}

const fn default_world_streaming_visible_chunks() -> u8 {
    DEFAULT_WORLD_STREAMING_VISIBLE_CHUNKS
}

/// Runtime culling knobs inherited by descendant Rooms from their
/// nearest World node. These are editor/playtest controls, not per-room
/// geometry data, so older projects safely load with the defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldCullingSettings {
    /// Camera-space far plane used for world, actor, and prop drawing.
    #[serde(default = "default_world_draw_distance")]
    pub draw_distance: i32,
    /// Radius around the current room/player used to keep chunks active.
    #[serde(default = "default_world_chunk_activation_radius_sectors")]
    pub chunk_activation_radius_sectors: i32,
    /// Radius used while cooking each room's visibility/PVS cell graph.
    #[serde(default = "default_world_visibility_radius")]
    pub visibility_radius: u16,
}

impl WorldCullingSettings {
    /// Clamp authored values to runtime-safe ranges.
    pub fn normalized(self) -> Self {
        Self {
            draw_distance: self
                .draw_distance
                .clamp(MIN_WORLD_DRAW_DISTANCE, MAX_WORLD_DRAW_DISTANCE),
            chunk_activation_radius_sectors: self.chunk_activation_radius_sectors.clamp(
                MIN_WORLD_CHUNK_ACTIVATION_RADIUS_SECTORS,
                MAX_WORLD_CHUNK_ACTIVATION_RADIUS_SECTORS,
            ),
            visibility_radius: self
                .visibility_radius
                .clamp(MIN_WORLD_VISIBILITY_RADIUS, MAX_WORLD_VISIBILITY_RADIUS),
        }
    }
}

impl Default for WorldCullingSettings {
    fn default() -> Self {
        Self {
            draw_distance: default_world_draw_distance(),
            chunk_activation_radius_sectors: default_world_chunk_activation_radius_sectors(),
            visibility_radius: default_world_visibility_radius(),
        }
    }
}

/// Portal-room streaming controls inherited by descendant Rooms from their
/// nearest World node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldStreamingSettings {
    /// Resident streaming budget, measured in runtime portal-room units.
    /// The playtest runtime converts this to more resident slots when the cooked
    /// rooms are smaller than the maximum stream slot size.
    #[serde(default = "default_world_streaming_resident_chunks")]
    pub resident_chunk_limit: u8,
    /// Maximum portal rooms selected for drawing/collision by the runtime.
    ///
    /// A serialized zero is treated as a legacy project value and inherits the
    /// resident chunk limit during normalization.
    #[serde(default)]
    pub visible_chunk_limit: u8,
}

impl WorldStreamingSettings {
    /// Clamp authored values to cooker-safe ranges.
    pub fn normalized(self) -> Self {
        let resident_chunk_limit = self.resident_chunk_limit.clamp(
            MIN_WORLD_STREAMING_RESIDENT_CHUNKS,
            MAX_WORLD_STREAMING_RESIDENT_CHUNKS,
        );
        let visible_chunk_limit = if self.visible_chunk_limit == 0 {
            resident_chunk_limit
        } else {
            self.visible_chunk_limit
        }
        .clamp(
            MIN_WORLD_STREAMING_VISIBLE_CHUNKS,
            MAX_WORLD_STREAMING_VISIBLE_CHUNKS,
        )
        .min(resident_chunk_limit);
        Self {
            resident_chunk_limit,
            visible_chunk_limit,
        }
    }
}

impl Default for WorldStreamingSettings {
    fn default() -> Self {
        Self {
            resident_chunk_limit: default_world_streaming_resident_chunks(),
            visible_chunk_limit: default_world_streaming_visible_chunks(),
        }
    }
}

fn face_triangle_count(face: &GridHorizontalFace) -> usize {
    if face.is_triangle() {
        1
    } else {
        2
    }
}

fn horizontal_face_needs_runtime_override(face: &GridHorizontalFace) -> bool {
    face.is_triangle() || !face.triangle_overrides.is_empty()
}

/// Engine-style grid world authored by a scene node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldGrid {
    /// Width in sectors.
    pub width: u16,
    /// Depth in sectors.
    pub depth: u16,
    /// Engine units per sector.
    pub sector_size: i32,
    /// Flat `[x * depth + z]` sector storage. `None` means no sector.
    pub sectors: Vec<Option<GridSector>>,
    /// World offset (in cell units) of cell index `(0, 0)`. Lets the
    /// editor extend the room into negative `X` / `Z` without
    /// renumbering existing cells: a `-X` grow shifts sectors by
    /// `+1` in X, decrements `origin.x` by `1`, and the renderer's
    /// world coord = `(origin + index) * sector_size`. Default
    /// `[0, 0]` for backward compat with already-saved projects.
    #[serde(default)]
    pub origin: [i32; 2],
    /// Room ambient color used as editor/cooker metadata.
    #[serde(default = "default_ambient_color")]
    pub ambient_color: [u8; 3],
    /// Whether PS1 depth cue/fog should be cooked for this grid.
    pub fog_enabled: bool,
    /// Depth-cue far color for this room.
    #[serde(default = "default_fog_color")]
    pub fog_color: [u8; 3],
    /// Start distance for authored fog/depth cue in engine units.
    #[serde(default = "default_fog_near")]
    pub fog_near: i32,
    /// Fully-fogged distance for authored fog/depth cue in engine units.
    #[serde(default = "default_fog_far")]
    pub fog_far: i32,
    /// Whether a cheap screen-space falling particle pass should render in this room.
    #[serde(default = "default_atmosphere_enabled")]
    pub atmosphere_enabled: bool,
    /// Base particle colour for ash/snow style room atmosphere.
    #[serde(default = "default_atmosphere_color")]
    pub atmosphere_color: [u8; 3],
    /// Number of screen-space particles to draw.
    #[serde(default = "default_atmosphere_density")]
    pub atmosphere_density: i32,
    /// Base vertical particle speed, in 1/16 pixel-per-vblank units.
    #[serde(default = "default_atmosphere_fall_speed_q4")]
    pub atmosphere_fall_speed_q4: i32,
    /// Base horizontal particle speed, in 1/16 pixel-per-vblank units.
    #[serde(default = "default_atmosphere_wind_speed_q4")]
    pub atmosphere_wind_speed_q4: i32,
}

impl WorldGrid {
    /// Create an empty sparse grid.
    pub fn empty(width: u16, depth: u16, sector_size: i32) -> Self {
        let len = width as usize * depth as usize;
        Self {
            width,
            depth,
            sector_size,
            sectors: vec![None; len],
            origin: [0, 0],
            ambient_color: default_ambient_color(),
            fog_enabled: true,
            fog_color: default_fog_color(),
            fog_near: default_fog_near(),
            fog_far: default_fog_far(),
            atmosphere_enabled: default_atmosphere_enabled(),
            atmosphere_color: default_atmosphere_color(),
            atmosphere_density: default_atmosphere_density(),
            atmosphere_fall_speed_q4: default_atmosphere_fall_speed_q4(),
            atmosphere_wind_speed_q4: default_atmosphere_wind_speed_q4(),
        }
    }

    /// Create a rectangular room with floors and perimeter walls.
    pub fn stone_room(
        width: u16,
        depth: u16,
        sector_size: i32,
        floor_material: Option<ResourceId>,
        wall_material: Option<ResourceId>,
    ) -> Self {
        let mut grid = Self::empty(width, depth, sector_size);
        let wall_top = default_wall_height_for_sector_size(sector_size);
        for x in 0..width {
            for z in 0..depth {
                grid.set_floor(x, z, 0, floor_material);
                if z == depth.saturating_sub(1) {
                    grid.add_wall(x, z, GridDirection::North, 0, wall_top, wall_material);
                }
                if x == width.saturating_sub(1) {
                    grid.add_wall(x, z, GridDirection::East, 0, wall_top, wall_material);
                }
                if z == 0 {
                    grid.add_wall(x, z, GridDirection::South, 0, wall_top, wall_material);
                }
                if x == 0 {
                    grid.add_wall(x, z, GridDirection::West, 0, wall_top, wall_material);
                }
            }
        }
        grid
    }

    /// Flat sector index.
    pub fn sector_index(&self, x: u16, z: u16) -> Option<usize> {
        if x < self.width && z < self.depth {
            Some(x as usize * self.depth as usize + z as usize)
        } else {
            None
        }
    }

    /// Immutable sector.
    pub fn sector(&self, x: u16, z: u16) -> Option<&GridSector> {
        self.sector_index(x, z)
            .and_then(|index| self.sectors.get(index)?.as_ref())
    }

    /// Mutable sector. `None` when out-of-bounds OR the cell hasn't
    /// been authored yet (use `ensure_sector` to create-on-access).
    pub fn sector_mut(&mut self, x: u16, z: u16) -> Option<&mut GridSector> {
        self.sector_index(x, z)
            .and_then(move |index| self.sectors.get_mut(index)?.as_mut())
    }

    /// Mutable sector, creating it if needed.
    pub fn ensure_sector(&mut self, x: u16, z: u16) -> Option<&mut GridSector> {
        let index = self.sector_index(x, z)?;
        if self.sectors[index].is_none() {
            self.sectors[index] = Some(GridSector::empty());
        }
        self.sectors[index].as_mut()
    }

    /// Set or replace a floor.
    pub fn set_floor(&mut self, x: u16, z: u16, height: i32, material: Option<ResourceId>) {
        if let Some(sector) = self.ensure_sector(x, z) {
            sector.floor = Some(GridHorizontalFace::flat(height, material));
        }
    }

    /// Set or clear the floor link above one sector.
    pub fn set_floor_above(&mut self, x: u16, z: u16, link: Option<GridFloorLink>) {
        if let Some(sector) = self.ensure_sector(x, z) {
            sector.floor_above = link;
        }
    }

    /// Set or clear the floor link below one sector.
    pub fn set_floor_below(&mut self, x: u16, z: u16, link: Option<GridFloorLink>) {
        if let Some(sector) = self.ensure_sector(x, z) {
            sector.floor_below = link;
        }
    }

    /// Number of authored vertical floor links in this grid.
    pub fn floor_link_count(&self) -> usize {
        self.sectors
            .iter()
            .filter_map(Option::as_ref)
            .map(|sector| {
                usize::from(sector.floor_above.is_some())
                    + usize::from(sector.floor_below.is_some())
            })
            .sum()
    }

    /// Set or replace a floor, inheriting edge heights from touching
    /// floors. If exactly one flat edge is connected, the whole new
    /// floor adopts that height instead of only matching the shared edge.
    pub fn set_floor_aligned_to_neighbors(
        &mut self,
        x: u16,
        z: u16,
        height: i32,
        material: Option<ResourceId>,
    ) {
        let wcx = self.origin[0] + i32::from(x);
        let wcz = self.origin[1] + i32::from(z);
        let heights = self.floor_heights_aligned_to_neighbors_for_world_cell(wcx, wcz, height);
        if let Some(sector) = self.ensure_sector(x, z) {
            let mut floor = GridHorizontalFace::flat(height, material);
            floor.heights = heights;
            sector.floor = Some(floor);
        }
    }

    /// Candidate floor heights for editor placement by world-cell
    /// coordinate. The returned order is `[NW, NE, SE, SW]`.
    pub fn floor_heights_aligned_to_neighbors_for_world_cell(
        &self,
        wcx: i32,
        wcz: i32,
        height: i32,
    ) -> [i32; 4] {
        self.horizontal_heights_aligned_to_neighbor_faces_for_world_cell(
            wcx,
            wcz,
            HorizontalSurface::Floor,
            [height; 4],
        )
        .map(snap_height)
    }

    /// Set or replace a ceiling, inheriting edge heights from
    /// touching ceilings first and touching wall tops second. Wall
    /// tops win so a newly-painted ceiling sits on the surrounding
    /// authored wall geometry instead of cutting through it.
    pub fn set_ceiling_aligned_to_neighbors(
        &mut self,
        x: u16,
        z: u16,
        material: Option<ResourceId>,
    ) {
        let wcx = self.origin[0] + i32::from(x);
        let wcz = self.origin[1] + i32::from(z);
        let heights = self.ceiling_heights_aligned_to_neighbors_for_world_cell(wcx, wcz);
        let fallback_height = default_wall_height_for_sector_size(self.sector_size);
        if let Some(sector) = self.ensure_sector(x, z) {
            let mut ceiling = GridHorizontalFace::flat(fallback_height, material);
            ceiling.heights = heights;
            sector.ceiling = Some(ceiling);
        }
    }

    /// Candidate ceiling heights for editor placement. The returned
    /// order is `[NW, NE, SE, SW]`.
    pub fn ceiling_heights_aligned_to_neighbors(&self, x: u16, z: u16) -> [i32; 4] {
        let wcx = self.origin[0] + i32::from(x);
        let wcz = self.origin[1] + i32::from(z);
        self.ceiling_heights_aligned_to_neighbors_for_world_cell(wcx, wcz)
    }

    /// Candidate ceiling heights for editor placement by world-cell
    /// coordinate. Used by hover previews for cells that may not be
    /// allocated until the click auto-grows the grid.
    pub fn ceiling_heights_aligned_to_neighbors_for_world_cell(
        &self,
        wcx: i32,
        wcz: i32,
    ) -> [i32; 4] {
        let fallback_height = default_wall_height_for_sector_size(self.sector_size);
        let base_heights = self
            .world_cell_to_array(wcx, wcz)
            .and_then(|(sx, sz)| self.sector(sx, sz))
            .and_then(|sector| sector.ceiling.as_ref())
            .map(|ceiling| ceiling.heights)
            .unwrap_or([fallback_height; 4]);

        let mut heights = self.horizontal_heights_aligned_to_neighbor_faces_for_world_cell(
            wcx,
            wcz,
            HorizontalSurface::Ceiling,
            base_heights,
        );

        for direction in GridDirection::CARDINAL {
            if let Some(edge) =
                self.touching_wall_top_edge_heights_for_world_cell(wcx, wcz, direction)
            {
                set_horizontal_edge_heights(&mut heights, direction, edge);
            }
        }

        heights.map(snap_height)
    }

    fn horizontal_heights_aligned_to_neighbor_faces_for_world_cell(
        &self,
        wcx: i32,
        wcz: i32,
        surface: HorizontalSurface,
        fallback: [i32; 4],
    ) -> [i32; 4] {
        let mut heights = fallback;
        let mut only_edge: Option<[i32; 2]> = None;
        let mut edge_count = 0usize;

        for direction in GridDirection::CARDINAL {
            if let Some(edge) =
                self.neighbor_horizontal_edge_heights_for_world_cell(wcx, wcz, direction, surface)
            {
                set_horizontal_edge_heights(&mut heights, direction, edge);
                only_edge = Some(edge);
                edge_count += 1;
            }
        }

        match (edge_count, only_edge) {
            (1, Some([a, b])) if a == b => [a; 4],
            _ => heights,
        }
    }

    /// Add a wall to an edge.
    pub fn add_wall(
        &mut self,
        x: u16,
        z: u16,
        direction: GridDirection,
        bottom: i32,
        top: i32,
        material: Option<ResourceId>,
    ) {
        if let Some(sector) = self.ensure_sector(x, z) {
            sector
                .walls
                .get_mut(direction)
                .push(GridVerticalFace::flat(bottom, top, material));
        }
    }

    /// Add a wall whose bottom edge follows the floor edge under it
    /// and whose top edge follows the ceiling edge when present.
    /// Missing ceilings fall back to a two-sector wall span above
    /// each bottom endpoint.
    pub fn add_wall_aligned_to_surfaces(
        &mut self,
        x: u16,
        z: u16,
        direction: GridDirection,
        material: Option<ResourceId>,
    ) {
        let heights = self.wall_heights_aligned_to_surfaces(x, z, direction);
        if let Some(sector) = self.ensure_sector(x, z) {
            sector
                .walls
                .get_mut(direction)
                .push(GridVerticalFace::with_heights(heights, material));
        }
    }

    /// Add a wall on the selected edge. When that edge already has
    /// touching wall geometry, the new wall starts at the highest
    /// existing top edge and extends by one default wall height.
    /// Otherwise it uses the regular floor-to-ceiling placement.
    pub fn add_wall_above_stack_or_aligned(
        &mut self,
        x: u16,
        z: u16,
        direction: GridDirection,
        material: Option<ResourceId>,
    ) {
        let heights = self.wall_heights_above_stack_or_surfaces(x, z, direction);
        if let Some(sector) = self.ensure_sector(x, z) {
            sector
                .walls
                .get_mut(direction)
                .push(GridVerticalFace::with_heights(heights, material));
        }
    }

    /// Candidate wall heights for editor placement on a cardinal
    /// edge or diagonal. The returned order is `[BL, BR, TR, TL]`.
    pub fn wall_heights_aligned_to_surfaces(
        &self,
        x: u16,
        z: u16,
        direction: GridDirection,
    ) -> [i32; 4] {
        let bottom = self
            .floor_edge_heights_for_wall(x, z, direction)
            .unwrap_or([0, 0]);
        let top = self
            .ceiling_edge_heights_for_wall(x, z, direction)
            .unwrap_or_else(|| {
                let height = default_wall_height_for_sector_size(self.sector_size);
                [
                    bottom[0].saturating_add(height),
                    bottom[1].saturating_add(height),
                ]
            });
        [bottom[0], bottom[1], top[1], top[0]]
    }

    /// Candidate wall heights for placing the next wall in a stack
    /// at an in-grid cell. Falls back to surface-aligned placement
    /// when there is no existing wall on the touched edge.
    pub fn wall_heights_above_stack_or_surfaces(
        &self,
        x: u16,
        z: u16,
        direction: GridDirection,
    ) -> [i32; 4] {
        self.wall_heights_above_stack_or_surfaces_for_world_cell(
            self.origin[0].saturating_add(x as i32),
            self.origin[1].saturating_add(z as i32),
            direction,
        )
    }

    /// Same as [`Self::wall_heights_aligned_to_surfaces`], but
    /// addressed by world-cell coordinates so hover previews can
    /// match clicks that will auto-grow the grid on commit.
    pub fn wall_heights_aligned_to_surfaces_for_world_cell(
        &self,
        wcx: i32,
        wcz: i32,
        direction: GridDirection,
    ) -> [i32; 4] {
        let bottom = self
            .horizontal_edge_heights_for_world_wall(wcx, wcz, direction, HorizontalSurface::Floor)
            .unwrap_or([0, 0]);
        let top = self
            .horizontal_edge_heights_for_world_wall(wcx, wcz, direction, HorizontalSurface::Ceiling)
            .unwrap_or_else(|| {
                let height = default_wall_height_for_sector_size(self.sector_size);
                [
                    bottom[0].saturating_add(height),
                    bottom[1].saturating_add(height),
                ]
            });
        [bottom[0], bottom[1], top[1], top[0]]
    }

    /// Same as [`Self::wall_heights_above_stack_or_surfaces`], but
    /// addressed by world-cell coordinates so off-grid wall previews
    /// match auto-grown placement.
    pub fn wall_heights_above_stack_or_surfaces_for_world_cell(
        &self,
        wcx: i32,
        wcz: i32,
        direction: GridDirection,
    ) -> [i32; 4] {
        if let Some(bottom) =
            self.touching_wall_top_edge_heights_for_world_cell(wcx, wcz, direction)
        {
            let height = default_wall_height_for_sector_size(self.sector_size);
            let top = [
                bottom[0].saturating_add(height),
                bottom[1].saturating_add(height),
            ];
            return [bottom[0], bottom[1], top[1], top[0]];
        }
        self.wall_heights_aligned_to_surfaces_for_world_cell(wcx, wcz, direction)
    }

    fn floor_edge_heights_for_wall(
        &self,
        x: u16,
        z: u16,
        direction: GridDirection,
    ) -> Option<[i32; 2]> {
        self.horizontal_edge_heights_for_wall(x, z, direction, HorizontalSurface::Floor)
    }

    fn ceiling_edge_heights_for_wall(
        &self,
        x: u16,
        z: u16,
        direction: GridDirection,
    ) -> Option<[i32; 2]> {
        self.horizontal_edge_heights_for_wall(x, z, direction, HorizontalSurface::Ceiling)
    }

    fn neighbor_horizontal_edge_heights_for_world_cell(
        &self,
        wcx: i32,
        wcz: i32,
        direction: GridDirection,
        surface: HorizontalSurface,
    ) -> Option<[i32; 2]> {
        let (nwcx, nwcz, opposite) =
            Self::neighbor_world_cell_across_cardinal_edge(wcx, wcz, direction)?;
        let (sx, sz) = self.world_cell_to_array(nwcx, nwcz)?;
        let mut heights = self
            .sector(sx, sz)
            .and_then(|sector| surface.edge_heights(sector, opposite))?;
        heights.swap(0, 1);
        Some(heights)
    }

    fn touching_wall_top_edge_heights_for_world_cell(
        &self,
        wcx: i32,
        wcz: i32,
        direction: GridDirection,
    ) -> Option<[i32; 2]> {
        if let Some((sx, sz)) = self.world_cell_to_array(wcx, wcz) {
            if let Some(heights) = self
                .sector(sx, sz)
                .and_then(|sector| wall_top_edge_heights(sector.walls.get(direction)))
            {
                return Some(heights);
            }
        }

        let (nwcx, nwcz, opposite) =
            Self::neighbor_world_cell_across_cardinal_edge(wcx, wcz, direction)?;
        let (sx, sz) = self.world_cell_to_array(nwcx, nwcz)?;
        let mut heights = self
            .sector(sx, sz)
            .and_then(|sector| wall_top_edge_heights(sector.walls.get(opposite)))?;
        heights.swap(0, 1);
        Some(heights)
    }

    fn horizontal_edge_heights_for_wall(
        &self,
        x: u16,
        z: u16,
        direction: GridDirection,
        surface: HorizontalSurface,
    ) -> Option<[i32; 2]> {
        if let Some(heights) = self
            .sector(x, z)
            .and_then(|sector| surface.edge_heights(sector, direction))
        {
            return Some(heights);
        }

        let (nx, nz, opposite) = self.neighbor_across_cardinal_edge(x, z, direction)?;
        let mut heights = self
            .sector(nx, nz)
            .and_then(|sector| surface.edge_heights(sector, opposite))?;
        heights.swap(0, 1);
        Some(heights)
    }

    fn neighbor_across_cardinal_edge(
        &self,
        x: u16,
        z: u16,
        direction: GridDirection,
    ) -> Option<(u16, u16, GridDirection)> {
        let opposite = direction.opposite_cardinal()?;
        let (nx, nz) = match direction {
            GridDirection::North => (x, z.checked_add(1)?),
            GridDirection::East => (x.checked_add(1)?, z),
            GridDirection::South => (x, z.checked_sub(1)?),
            GridDirection::West => (x.checked_sub(1)?, z),
            GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => return None,
        };
        (nx < self.width && nz < self.depth).then_some((nx, nz, opposite))
    }

    fn horizontal_edge_heights_for_world_wall(
        &self,
        wcx: i32,
        wcz: i32,
        direction: GridDirection,
        surface: HorizontalSurface,
    ) -> Option<[i32; 2]> {
        if let Some((sx, sz)) = self.world_cell_to_array(wcx, wcz) {
            if let Some(heights) = self
                .sector(sx, sz)
                .and_then(|sector| surface.edge_heights(sector, direction))
            {
                return Some(heights);
            }
        }

        let (nwcx, nwcz, opposite) =
            Self::neighbor_world_cell_across_cardinal_edge(wcx, wcz, direction)?;
        let (sx, sz) = self.world_cell_to_array(nwcx, nwcz)?;
        let mut heights = self
            .sector(sx, sz)
            .and_then(|sector| surface.edge_heights(sector, opposite))?;
        heights.swap(0, 1);
        Some(heights)
    }

    /// Cook-time wall generated for a shared floor edge whose two
    /// sides do not meet. This closes vertical cracks in authored
    /// terrain without requiring artists to hand-place every step
    /// riser. Existing authored walls always win.
    pub fn floor_transition_wall_for_edge(
        &self,
        x: u16,
        z: u16,
        direction: GridDirection,
    ) -> Option<GridVerticalFace> {
        if !direction.is_cardinal() || self.physical_wall_authored(x, z, direction) {
            return None;
        }
        let sector = self.sector(x, z)?;
        let floor = sector.floor.as_ref()?;
        let current = HorizontalSurface::Floor.edge_heights(sector, direction)?;
        let (nx, nz, opposite) = self.neighbor_across_cardinal_edge(x, z, direction)?;
        let neighbour_sector = self.sector(nx, nz)?;
        let neighbour_floor = neighbour_sector.floor.as_ref()?;
        let mut neighbour = HorizontalSurface::Floor.edge_heights(neighbour_sector, opposite)?;
        neighbour.swap(0, 1);
        if current == neighbour {
            return None;
        }

        let bottom = [current[0].min(neighbour[0]), current[1].min(neighbour[1])];
        let top = [current[0].max(neighbour[0]), current[1].max(neighbour[1])];
        if bottom == top {
            return None;
        }
        Some(GridVerticalFace::with_heights(
            [bottom[0], bottom[1], top[1], top[0]],
            floor_transition_wall_material(floor, neighbour_floor, current, neighbour),
        ))
    }

    fn physical_wall_authored(&self, x: u16, z: u16, direction: GridDirection) -> bool {
        if self
            .sector(x, z)
            .is_some_and(|sector| !sector.walls.get(direction).is_empty())
        {
            return true;
        }
        let Some((nx, nz, opposite)) = self.neighbor_across_cardinal_edge(x, z, direction) else {
            return false;
        };
        self.sector(nx, nz)
            .is_some_and(|sector| !sector.walls.get(opposite).is_empty())
    }

    fn neighbor_world_cell_across_cardinal_edge(
        wcx: i32,
        wcz: i32,
        direction: GridDirection,
    ) -> Option<(i32, i32, GridDirection)> {
        let opposite = direction.opposite_cardinal()?;
        let cell = match direction {
            GridDirection::North => (wcx, wcz.saturating_add(1)),
            GridDirection::East => (wcx.saturating_add(1), wcz),
            GridDirection::South => (wcx, wcz.saturating_sub(1)),
            GridDirection::West => (wcx.saturating_sub(1), wcz),
            GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => return None,
        };
        Some((cell.0, cell.1, opposite))
    }

    /// Number of populated sectors.
    pub fn populated_sector_count(&self) -> usize {
        self.sectors
            .iter()
            .flatten()
            .filter(|sector| sector.has_geometry())
            .count()
    }

    /// Rectangle enclosing every sector that emits authored
    /// geometry. Empty allocated cells are capacity, not room
    /// footprint, so they do not influence bounds or streaming
    /// subdivision.
    pub fn authored_footprint(&self) -> Option<WorldGridFootprint> {
        let mut min_x = self.width;
        let mut min_z = self.depth;
        let mut max_x = 0u16;
        let mut max_z = 0u16;
        let mut found = false;
        for x in 0..self.width {
            for z in 0..self.depth {
                let Some(sector) = self.sector(x, z) else {
                    continue;
                };
                if !sector.has_geometry() {
                    continue;
                }
                found = true;
                min_x = min_x.min(x);
                min_z = min_z.min(z);
                max_x = max_x.max(x);
                max_z = max_z.max(z);
            }
        }
        found.then_some(WorldGridFootprint {
            x: min_x,
            z: min_z,
            width: max_x - min_x + 1,
            depth: max_z - min_z + 1,
        })
    }

    /// Budget for the authored footprint only. This is the number
    /// authors care about when sparse grid allocation has grown past
    /// the currently placed tiles.
    pub fn authored_budget(&self) -> WorldGridBudget {
        self.authored_footprint()
            .and_then(|f| self.budget_for_rect(f.x, f.z, f.width, f.depth))
            .unwrap_or_default()
    }

    /// Snapshot of the allocated grid rectangle + cooked-byte
    /// estimate. Use [`Self::authored_budget`] when empty capacity
    /// should not count as room footprint.
    pub fn budget(&self) -> WorldGridBudget {
        self.budget_for_rect(0, 0, self.width, self.depth)
            .unwrap_or_default()
    }

    /// Snapshot of one rectangular grid area. The rectangle is in
    /// array-sector coordinates, not world-origin-adjusted cells.
    /// Returns `None` for empty or out-of-bounds rectangles.
    pub fn budget_for_rect(
        &self,
        x: u16,
        z: u16,
        width: u16,
        depth: u16,
    ) -> Option<WorldGridBudget> {
        if width == 0 || depth == 0 {
            return None;
        }
        let x1 = x.checked_add(width)?;
        let z1 = z.checked_add(depth)?;
        if x1 > self.width || z1 > self.depth {
            return None;
        }
        let mut b = WorldGridBudget {
            width,
            depth,
            total_cells: (width as usize) * (depth as usize),
            ..Default::default()
        };
        for sx in x..x1 {
            for sz in z..z1 {
                let Some(sector) = self.sector(sx, sz) else {
                    continue;
                };
                if !sector.has_geometry() {
                    continue;
                }
                b.populated_cells += 1;
                if sector.floor.is_some() {
                    b.floors += 1;
                    if let Some(face) = sector.floor.as_ref() {
                        b.triangles += face_triangle_count(face);
                        if horizontal_face_needs_runtime_override(face) {
                            b.horizontal_overrides += 1;
                        }
                    }
                }
                if sector.ceiling.is_some() {
                    b.ceilings += 1;
                    if let Some(face) = sector.ceiling.as_ref() {
                        b.triangles += face_triangle_count(face);
                        if horizontal_face_needs_runtime_override(face) {
                            b.horizontal_overrides += 1;
                        }
                    }
                }
                for direction in GridDirection::ALL {
                    for wall in sector.walls.get(direction) {
                        let count = wall.autotile_segment_count(self.sector_size);
                        b.walls += count;
                        b.triangles += if wall.is_triangle() { 1 } else { count * 2 };
                    }
                }
                for direction in [GridDirection::East, GridDirection::North] {
                    if let Some(wall) = self.floor_transition_wall_for_edge(sx, sz, direction) {
                        let count = wall.autotile_segment_count(self.sector_size);
                        b.walls += count;
                        b.triangles += if wall.is_triangle() { 1 } else { count * 2 };
                    }
                }
            }
        }
        // Active wire layout (matches `psxed_format::world` records).
        // `.psxw` stores a sector record for every cell -- empty or
        // not -- so the byte count uses `total_cells`. Using
        // `populated_cells` here was the original bug: it under-
        // reported the wire size by one sector record per empty cell.
        // Target compact-format sizes for the planning estimate.
        // See `docs/world-format-roadmap.md`. Plain numeric
        // constants rather than struct sizes so this block doesn't
        // pretend a v2 format exists in code.
        b.psxw_bytes = ASSET_HEADER_BYTES
            + WORLD_HEADER_BYTES
            + b.total_cells * PSXW_SECTOR_BYTES
            + b.walls * PSXW_WALL_BYTES
            + b.horizontal_overrides * PSXW_HORIZONTAL_OVERRIDE_BYTES;
        if b.populated_cells > 0 {
            b.static_light_table_bytes = (b.total_cells * 2 + b.walls) * PSXW_SURFACE_LIGHT_BYTES;
        }
        b.psxw_static_lit_bytes = b.psxw_bytes + b.static_light_table_bytes;
        b.future_compact_estimated_bytes = ASSET_HEADER_BYTES
            + WORLD_HEADER_BYTES
            + b.total_cells * FUTURE_COMPACT_SECTOR_BYTES
            + b.walls * FUTURE_COMPACT_WALL_BYTES;
        Some(b)
    }

    /// World-space X coordinate of the left edge of column `sx`
    /// (array index, not world-cell index). Accounts for `origin`
    /// so the renderer and picking always agree on cell positions.
    pub fn cell_world_x(&self, sx: u16) -> i32 {
        (self.origin[0] + sx as i32) * self.sector_size
    }

    /// World-space Z coordinate of the low-Z edge of row `sz`.
    pub fn cell_world_z(&self, sz: u16) -> i32 {
        (self.origin[1] + sz as i32) * self.sector_size
    }

    /// World-space X/Z bounds of cell `(sx, sz)` in editor
    /// convention. `z0` is the low-Z / south edge and `z1` is
    /// the high-Z / north edge.
    pub fn cell_bounds_world(&self, sx: u16, sz: u16) -> GridCellBounds {
        let x0 = self.cell_world_x(sx);
        let z0 = self.cell_world_z(sz);
        GridCellBounds {
            x0,
            x1: x0 + self.sector_size,
            z0,
            z1: z0 + self.sector_size,
        }
    }

    /// World-space `(x, z)` centre of cell `(sx, sz)` in floating
    /// point -- handy for picking, edge inference, and entity
    /// snapping. Mirrors the renderer's cell positioning so all
    /// three pipelines agree on where each cell physically sits.
    pub fn cell_center_world(&self, sx: u16, sz: u16) -> [f32; 2] {
        let s = self.sector_size as f32;
        [
            (self.origin[0] as f32 + sx as f32 + 0.5) * s,
            (self.origin[1] as f32 + sz as f32 + 0.5) * s,
        ]
    }

    /// Geometric centre of the room in world-cell units. After a
    /// negative-side grow this is `(origin + half)` rather than
    /// just `half`, so callers stay correct without each
    /// re-deriving the offset.
    ///
    /// This is the **canonical** editor centre -- every coordinate
    /// helper that bridges editor-viewport units (sector-units,
    /// room-centre-relative) and world-cell / world-space units
    /// goes through this single source of truth.
    pub fn grid_center_cells(&self) -> [f32; 2] {
        [
            self.origin[0] as f32 + self.width as f32 * 0.5,
            self.origin[1] as f32 + self.depth as f32 * 0.5,
        ]
    }

    /// Convert editor-viewport coordinates (sector-units,
    /// room-centre-relative) to world-cell units. The viewport's
    /// `(0, 0)` is the room centre; world-cell `(0, 0)` is the
    /// runtime cell at the room's first array slot pre-grow.
    pub fn editor_to_world_cells(&self, editor: [f32; 2]) -> [f32; 2] {
        let center = self.grid_center_cells();
        [editor[0] + center[0], editor[1] + center[1]]
    }

    /// Inverse of [`Self::editor_to_world_cells`]. World coords
    /// (post-`/sector_size`) returned from a 3D ground-plane hit
    /// land back in the editor's sector-unit space ready to feed
    /// `world_cell_to_array` or stash on a node transform.
    pub fn world_cells_to_editor(&self, world_cells: [f32; 2]) -> [f32; 2] {
        let center = self.grid_center_cells();
        [world_cells[0] - center[0], world_cells[1] - center[1]]
    }

    /// Editor-viewport position → array `(sx, sz)`. Combines
    /// `editor_to_world_cells` + `floor` + `world_cell_to_array`
    /// in one step so callers don't repeat the conversion at
    /// each call site.
    pub fn editor_cells_to_array(&self, editor: [f32; 2]) -> Option<(u16, u16)> {
        let world = self.editor_to_world_cells(editor);
        let wcx = world[0].floor() as i32;
        let wcz = world[1].floor() as i32;
        self.world_cell_to_array(wcx, wcz)
    }

    /// Editor-viewport position → world-space `(x, 0, z)` in
    /// engine units (room-local, origin-aware). Used by the
    /// editor's 3D preview path which renders cells at
    /// `cell_world_x/z` so authored content keeps its visual
    /// position after a negative-side grow.
    pub fn editor_to_room_local(&self, editor: [f32; 2]) -> [f32; 3] {
        let world_cells = self.editor_to_world_cells(editor);
        let s = self.sector_size as f32;
        [world_cells[0] * s, 0.0, world_cells[1] * s]
    }

    /// Inverse of [`Self::editor_to_room_local`] -- world-space
    /// `(x, _, z)` → editor-viewport `(x, z)` (sector-units,
    /// room-centre-relative). The `y` component is dropped:
    /// cell positioning is purely XZ.
    pub fn room_local_to_editor(&self, room_local: [f32; 3]) -> [f32; 2] {
        let s = self.sector_size as f32;
        self.world_cells_to_editor([room_local[0] / s, room_local[2] / s])
    }

    /// Convert a world position to the world-cell coordinate
    /// (which can be negative). The world-cell is the same coord
    /// system the renderer uses; subtract `origin` to get the
    /// array index.
    pub fn world_x_to_cell(&self, world_x: f32) -> i32 {
        (world_x / self.sector_size as f32).floor() as i32
    }

    pub fn world_z_to_cell(&self, world_z: f32) -> i32 {
        (world_z / self.sector_size as f32).floor() as i32
    }

    /// Floor height under a room-local world-space X/Z point.
    /// Returns `None` when the point is outside the allocated grid
    /// or the addressed sector has no floor face.
    pub fn floor_height_at_room_local(&self, world_x: i32, world_z: i32) -> Option<i32> {
        let s = self.sector_size;
        if s <= 0 {
            return None;
        }
        let wcx = world_x.div_euclid(s);
        let wcz = world_z.div_euclid(s);
        let (sx, sz) = self.world_cell_to_array(wcx, wcz)?;
        let sector = self.sector(sx, sz)?;
        let floor = sector.floor.as_ref()?;
        let local_x = world_x.rem_euclid(s);
        let local_z = world_z.rem_euclid(s);
        Some(floor.height_at_local(local_x, local_z, s))
    }

    /// Translate a world-cell coordinate to its array index, or
    /// `None` if the cell isn't currently allocated.
    pub fn world_cell_to_array(&self, wcx: i32, wcz: i32) -> Option<(u16, u16)> {
        let ax = wcx.checked_sub(self.origin[0])?;
        let az = wcz.checked_sub(self.origin[1])?;
        if ax < 0 || az < 0 {
            return None;
        }
        let ax = ax as u32;
        let az = az as u32;
        if ax >= self.width as u32 || az >= self.depth as u32 {
            return None;
        }
        Some((ax as u16, az as u16))
    }

    /// Ensure the world-cell `(wcx, wcz)` is addressable. Grows
    /// the grid in `+X` / `+Z` and / or shifts existing sectors
    /// (with `origin` decrementing in lockstep) when growth is
    /// needed in `-X` / `-Z`. Existing cells keep the same world
    /// position throughout. Returns the resolved array index.
    pub fn extend_to_include(&mut self, wcx: i32, wcz: i32) -> (u16, u16) {
        let rel_x = wcx - self.origin[0];
        let rel_z = wcz - self.origin[1];
        let shift_x = (-rel_x).max(0) as u16;
        let shift_z = (-rel_z).max(0) as u16;
        // The new array width must hold both the shifted existing
        // data ([shift, shift + old_width)) AND the new cell (at
        // shift + max(rel, 0)). Same logic for depth.
        let new_cell_x = (rel_x.max(0) as u16) + shift_x;
        let new_cell_z = (rel_z.max(0) as u16) + shift_z;
        let new_w = (shift_x + self.width).max(new_cell_x + 1);
        let new_d = (shift_z + self.depth).max(new_cell_z + 1);
        if shift_x == 0 && shift_z == 0 && new_w == self.width && new_d == self.depth {
            return (rel_x as u16, rel_z as u16);
        }
        // Rebuild the sector array, shifting existing data by
        // (shift_x, shift_z) so its world position is preserved.
        let new_len = new_w as usize * new_d as usize;
        let mut new_sectors: Vec<Option<GridSector>> = vec![None; new_len];
        for x in 0..self.width {
            for z in 0..self.depth {
                let old_idx = x as usize * self.depth as usize + z as usize;
                let new_x = x as usize + shift_x as usize;
                let new_z = z as usize + shift_z as usize;
                if new_x < new_w as usize && new_z < new_d as usize {
                    let new_idx = new_x * new_d as usize + new_z;
                    new_sectors[new_idx] = self.sectors[old_idx].take();
                }
            }
        }
        self.width = new_w;
        self.depth = new_d;
        self.origin[0] -= shift_x as i32;
        self.origin[1] -= shift_z as i32;
        self.sectors = new_sectors;
        (
            (rel_x + shift_x as i32) as u16,
            (rel_z + shift_z as i32) as u16,
        )
    }

    /// Reshape the grid to `new_width × new_depth`.
    ///
    /// Sectors that lie inside both the old and new bounds keep
    /// their authored content; cells that were outside the old
    /// bounds (a grow operation) come up empty; cells outside the
    /// new bounds (a shrink) are dropped.
    ///
    /// No-op when the dims already match.
    pub fn resize(&mut self, new_width: u16, new_depth: u16) {
        if new_width == self.width && new_depth == self.depth {
            return;
        }
        let new_len = new_width as usize * new_depth as usize;
        let mut new_sectors: Vec<Option<GridSector>> = vec![None; new_len];
        let copy_w = self.width.min(new_width);
        let copy_d = self.depth.min(new_depth);
        for x in 0..copy_w {
            for z in 0..copy_d {
                let old_idx = x as usize * self.depth as usize + z as usize;
                let new_idx = x as usize * new_depth as usize + z as usize;
                new_sectors[new_idx] = self.sectors[old_idx].take();
            }
        }
        self.width = new_width;
        self.depth = new_depth;
        self.sectors = new_sectors;
    }

    /// Change this grid's sector size and scale engine-unit
    /// vertical geometry by the same ratio. X/Z authored positions
    /// are stored in sector units, so they inherit the new physical
    /// size through `sector_size`.
    pub fn rescale_sector_size(&mut self, new_sector_size: i32) {
        let new_sector_size = snap_world_sector_size(new_sector_size);
        let old_sector_size = self.sector_size.max(1);
        if old_sector_size == new_sector_size {
            self.sector_size = new_sector_size;
            self.snap_heights_to_quantum();
            return;
        }
        for sector in self.sectors.iter_mut().flatten() {
            if let Some(face) = &mut sector.floor {
                for h in &mut face.heights {
                    *h = snap_height(scale_i32_ratio(*h, old_sector_size, new_sector_size));
                }
                for idx in 0..2 {
                    if let Some(heights) = face.triangle_override_mut(idx).heights.as_mut() {
                        for h in heights {
                            *h = snap_height(scale_i32_ratio(*h, old_sector_size, new_sector_size));
                        }
                    }
                }
            }
            if let Some(face) = &mut sector.ceiling {
                for h in &mut face.heights {
                    *h = snap_height(scale_i32_ratio(*h, old_sector_size, new_sector_size));
                }
                for idx in 0..2 {
                    if let Some(heights) = face.triangle_override_mut(idx).heights.as_mut() {
                        for h in heights {
                            *h = snap_height(scale_i32_ratio(*h, old_sector_size, new_sector_size));
                        }
                    }
                }
            }
            for direction in GridDirection::ALL {
                for wall in sector.walls.get_mut(direction) {
                    for h in &mut wall.heights {
                        *h = snap_height(scale_i32_ratio(*h, old_sector_size, new_sector_size));
                    }
                }
            }
        }
        self.fog_near = scale_i32_ratio(self.fog_near, old_sector_size, new_sector_size).max(0);
        self.fog_far = scale_i32_ratio(self.fog_far, old_sector_size, new_sector_size)
            .max(self.fog_near + HEIGHT_QUANTUM);
        self.sector_size = new_sector_size;
    }

    /// Snap all authored vertical geometry to the cooker-supported
    /// height quantum. This is load/save normalization for stale or
    /// hand-edited project data; live editor controls call
    /// [`snap_height`] at the point of edit.
    pub fn snap_heights_to_quantum(&mut self) {
        for sector in self.sectors.iter_mut().flatten() {
            if let Some(face) = &mut sector.floor {
                for h in &mut face.heights {
                    *h = snap_height(*h);
                }
                for idx in 0..2 {
                    if let Some(heights) = face.triangle_override_mut(idx).heights.as_mut() {
                        for h in heights {
                            *h = snap_height(*h);
                        }
                    }
                }
            }
            if let Some(face) = &mut sector.ceiling {
                for h in &mut face.heights {
                    *h = snap_height(*h);
                }
                for idx in 0..2 {
                    if let Some(heights) = face.triangle_override_mut(idx).heights.as_mut() {
                        for h in heights {
                            *h = snap_height(*h);
                        }
                    }
                }
            }
            for direction in GridDirection::ALL {
                for wall in sector.walls.get_mut(direction) {
                    for h in &mut wall.heights {
                        *h = snap_height(*h);
                    }
                }
            }
        }
    }
}

/// One cooked animation clip referenced by a [`ModelResource`].
///
/// `psxanim_path` resolves with the same precedence rules as
/// [`ResourceData::Texture::psxt_path`]: absolute → project-relative →
/// workspace cwd-relative. Stored relative to the project when the
/// editor registers a bundle, so projects move freely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelAnimationClip {
    /// Display name surfaced in the inspector (clip dropdown,
    /// scrubber). Derived from the source filename when registered
    /// via a cooked bundle; user-editable.
    pub name: String,
    /// Path to the cooked `.psxanim` artifact.
    pub psxanim_path: String,
    /// Per-clip model placement controls used by editor preview and
    /// cooked runtime rendering.
    #[serde(default, skip_serializing_if = "AnimationClipCalibration::is_default")]
    pub calibration: AnimationClipCalibration,
}

/// Per-animation model placement controls.
///
/// These are deliberately stored on the clip, not on the character or
/// model renderer: different imported animations can have different
/// root conventions even when they target the same skeleton.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnimationClipCalibration {
    /// Render the clip in-place by cancelling root translation in
    /// model-local space. Controller code owns gameplay movement.
    #[serde(default = "default_true")]
    pub in_place: bool,
    /// Extra model-local pose translation in cooked pose units.
    #[serde(default)]
    pub offset: [i32; 3],
}

impl AnimationClipCalibration {
    pub const DEFAULT: Self = Self {
        in_place: true,
        offset: [0, 0, 0],
    };

    pub fn is_default(&self) -> bool {
        *self == Self::DEFAULT
    }
}

impl Default for AnimationClipCalibration {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Skeleton compatibility contract for skinned models and animation
/// clips.
///
/// The cooked `.psxanim` format only stores a joint count, so the
/// editor keeps the stronger authoring-side contract here: joint
/// count plus the cooked model parent table. Source importers can
/// extend this later with joint names and bind-pose hashes without
/// changing the runtime record layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkeletonResource {
    /// Number of joints in the skeleton.
    pub joint_count: u16,
    /// Parent index for each joint, or `None` for root joints.
    #[serde(default)]
    pub parents: Vec<Option<u16>>,
    /// Deterministic compatibility key. Current cooked assets use a
    /// parent-table signature; future importers should include joint
    /// names and bind pose in this value.
    #[serde(default)]
    pub signature: String,
    /// Human-readable note/source hint.
    #[serde(default)]
    pub note: String,
}

impl SkeletonResource {
    /// Build a skeleton descriptor from a cooked model.
    pub fn from_model(model: &psx_asset::Model<'_>) -> Self {
        let mut parents = Vec::with_capacity(model.joint_count() as usize);
        for index in 0..model.joint_count() {
            parents.push(model.joint(index).and_then(|joint| joint.parent()));
        }
        let signature = skeleton_signature(model.joint_count(), &parents);
        Self {
            joint_count: model.joint_count(),
            parents,
            signature,
            note: String::new(),
        }
    }

    /// True when an animation with `joint_count` can at least be
    /// safely sampled against this skeleton. This is the minimum
    /// cooked-format guarantee; exact skeleton signatures are checked
    /// when another skeleton resource is available.
    pub const fn accepts_joint_count(&self, joint_count: u16) -> bool {
        self.joint_count == joint_count
    }
}

fn skeleton_signature(joint_count: u16, parents: &[Option<u16>]) -> String {
    let mut out = format!("psx-parent-v1:{joint_count}:");
    for (index, parent) in parents.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        match parent {
            Some(parent) => out.push_str(&parent.to_string()),
            None => out.push_str("root"),
        }
    }
    out
}

/// Semantic role for an animation clip. This is editor metadata:
/// runtime still receives concrete clip indices after cooking.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnimationRole {
    /// No specific gameplay meaning yet.
    #[default]
    Generic,
    Idle,
    Walk,
    Run,
    Turn,
    Roll,
    Backstep,
    Attack,
    Hit,
    Death,
}

impl AnimationRole {
    pub const ALL: [Self; 10] = [
        Self::Generic,
        Self::Idle,
        Self::Walk,
        Self::Run,
        Self::Turn,
        Self::Roll,
        Self::Backstep,
        Self::Attack,
        Self::Hit,
        Self::Death,
    ];

    /// User-facing label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Generic => "Generic",
            Self::Idle => "Idle",
            Self::Walk => "Walk",
            Self::Run => "Run",
            Self::Turn => "Turn",
            Self::Roll => "Roll",
            Self::Backstep => "Backstep",
            Self::Attack => "Attack",
            Self::Hit => "Hit",
            Self::Death => "Death",
        }
    }

    /// Guess a role from a clip/resource name.
    pub fn guess_from_name(name: &str) -> Self {
        let name = name.to_ascii_lowercase();
        if name.contains("idle") {
            Self::Idle
        } else if name.contains("run") {
            Self::Run
        } else if name.contains("backstep")
            || name.contains("back_step")
            || name.contains("back step")
            || name.contains("step_back")
            || name.contains("step back")
        {
            Self::Backstep
        } else if name.contains("roll") || name.contains("dodge") {
            Self::Roll
        } else if name.contains("walk") {
            Self::Walk
        } else if name.contains("turn") {
            Self::Turn
        } else if name.contains("attack") || name.contains("combo") || name.contains("melee") {
            Self::Attack
        } else if name.contains("hit") || name.contains("reaction") {
            Self::Hit
        } else if name.contains("death") || name.contains("dead") {
            Self::Death
        } else {
            Self::Generic
        }
    }
}

/// Gameplay action slots that can be driven by animation clips.
///
/// This is distinct from [`AnimationRole`]: a clip's role describes
/// what the source appears to be, while a character action says how
/// the game will use it. Authors may bind any compatible clip to any
/// action.
pub const CHARACTER_ANIMATION_ACTION_COUNT: usize = psx_level::CHARACTER_ANIMATION_ACTION_COUNT;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CharacterAnimationAction {
    #[default]
    Idle,
    Walk,
    Run,
    Turn,
    Roll,
    Backstep,
    LightAttack,
    HeavyAttack,
    ComboAttack,
    Block,
    HitReact,
    Death,
}

impl CharacterAnimationAction {
    pub const ALL: [Self; CHARACTER_ANIMATION_ACTION_COUNT] = [
        Self::Idle,
        Self::Walk,
        Self::Run,
        Self::Turn,
        Self::Roll,
        Self::Backstep,
        Self::LightAttack,
        Self::HeavyAttack,
        Self::ComboAttack,
        Self::Block,
        Self::HitReact,
        Self::Death,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Walk => "Walk",
            Self::Run => "Run",
            Self::Turn => "Turn",
            Self::Roll => "Roll",
            Self::Backstep => "Backstep",
            Self::LightAttack => "Light Attack",
            Self::HeavyAttack => "Heavy Attack",
            Self::ComboAttack => "Combo Attack",
            Self::Block => "Block",
            Self::HitReact => "Hit React",
            Self::Death => "Death",
        }
    }

    pub const fn to_index(self) -> usize {
        match self {
            Self::Idle => 0,
            Self::Walk => 1,
            Self::Run => 2,
            Self::Turn => 3,
            Self::Roll => 4,
            Self::Backstep => 5,
            Self::LightAttack => 6,
            Self::HeavyAttack => 7,
            Self::ComboAttack => 8,
            Self::Block => 9,
            Self::HitReact => 10,
            Self::Death => 11,
        }
    }

    pub const fn role_hint(self) -> Option<AnimationRole> {
        match self {
            Self::Idle => Some(AnimationRole::Idle),
            Self::Walk => Some(AnimationRole::Walk),
            Self::Run => Some(AnimationRole::Run),
            Self::Turn => Some(AnimationRole::Turn),
            Self::Roll => Some(AnimationRole::Roll),
            Self::Backstep => Some(AnimationRole::Backstep),
            Self::LightAttack | Self::HeavyAttack | Self::ComboAttack | Self::Block => {
                Some(AnimationRole::Attack)
            }
            Self::HitReact => Some(AnimationRole::Hit),
            Self::Death => Some(AnimationRole::Death),
        }
    }

    pub const fn required_for_player(self) -> bool {
        matches!(self, Self::Idle | Self::Walk)
    }

    pub const fn loops_by_default(self) -> bool {
        matches!(
            self,
            Self::Idle | Self::Walk | Self::Run | Self::Turn | Self::Block
        )
    }

    pub fn guess_from_name(name: &str) -> Option<Self> {
        let name = name.to_ascii_lowercase();
        if name.contains("idle") {
            Some(Self::Idle)
        } else if name.contains("run") {
            Some(Self::Run)
        } else if name.contains("backstep")
            || name.contains("back_step")
            || name.contains("back step")
            || name.contains("step_back")
            || name.contains("step back")
        {
            Some(Self::Backstep)
        } else if name.contains("roll") || name.contains("dodge") {
            Some(Self::Roll)
        } else if name.contains("walk") {
            Some(Self::Walk)
        } else if name.contains("turn") {
            Some(Self::Turn)
        } else if name.contains("death") || name.contains("dead") {
            Some(Self::Death)
        } else if name.contains("hit") || name.contains("reaction") {
            Some(Self::HitReact)
        } else if name.contains("block") || name.contains("guard") {
            Some(Self::Block)
        } else if name.contains("combo") {
            Some(Self::ComboAttack)
        } else if name.contains("heavy") || name.contains("strong") {
            Some(Self::HeavyAttack)
        } else if name.contains("light") || name.contains("attack") || name.contains("melee") {
            Some(Self::LightAttack)
        } else {
            None
        }
    }
}

/// Resource-based action binding used by Animation Sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnimationActionBinding {
    pub action: CharacterAnimationAction,
    pub clip: ResourceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<CharacterActionOptions>,
}

/// Model-local fallback action binding used directly on Characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterActionClip {
    pub action: CharacterAnimationAction,
    pub clip: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<CharacterActionOptions>,
}

/// Per-action playback controls.
///
/// This deliberately belongs to the action binding, not the clip
/// resource: the same cooked animation can be used as a looping
/// locomotion fallback in one place and a one-shot action in another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterActionOptions {
    #[serde(default)]
    pub looping: bool,
    #[serde(default = "default_true")]
    pub in_place: bool,
}

impl CharacterActionOptions {
    pub const fn for_action(action: CharacterAnimationAction) -> Self {
        Self {
            looping: action.loops_by_default(),
            in_place: true,
        }
    }
}

/// Where an authoring-time animation candidate came from. The source
/// kind is editor metadata only; runtime receives already-cooked
/// `.psxanim` clips.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnimationSourceProvider {
    #[default]
    Unknown,
    Meshy,
    Mixamo,
    Synty,
    Other,
}

impl AnimationSourceProvider {
    pub const ALL: [Self; 5] = [
        Self::Unknown,
        Self::Meshy,
        Self::Mixamo,
        Self::Synty,
        Self::Other,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Meshy => "Meshy",
            Self::Mixamo => "Mixamo",
            Self::Synty => "Synty",
            Self::Other => "Other",
        }
    }

    pub fn guess_from_path(path: &str) -> Self {
        let lowered = path.to_ascii_lowercase();
        if lowered.contains("meshy") {
            Self::Meshy
        } else if lowered.contains("mixamo") || lowered.contains("standalone_fbx") {
            Self::Mixamo
        } else if lowered.contains("synty")
            || lowered.contains("sword_combat")
            || lowered.contains("sourcefiles/animations/polygon")
            || lowered.contains("sourcefiles/animations/sidekick")
        {
            Self::Synty
        } else {
            Self::Unknown
        }
    }
}

/// Authoring-time animation library entry. A source may be a raw FBX /
/// GLB clip, or a legacy cooked clip that has not yet been traced back
/// to its raw source. It is never consumed directly by the runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnimationSourceResource {
    /// Source file path. Prefer raw `.fbx` / `.glb` assets; legacy
    /// catalogued projects may point at an existing `.psxanim`.
    pub source_path: String,
    /// Clip/take name inside the source file.
    #[serde(default)]
    pub clip_name: String,
    /// Source provider hint used by the future retargeting pipeline.
    #[serde(default)]
    pub provider: AnimationSourceProvider,
    /// Optional source skeleton metadata when the importer knows it.
    #[serde(default)]
    pub skeleton: Option<ResourceId>,
    /// Optional target model when this source is known to be authored
    /// specifically for one Meshy character/export.
    #[serde(default)]
    pub target_model: Option<ResourceId>,
    /// Semantic role used for filtering and assignment.
    #[serde(default)]
    pub role: AnimationRole,
    /// Whether this source is expected to loop when used.
    #[serde(default = "default_true")]
    pub looping: bool,
    /// Searchable editor tags.
    #[serde(default)]
    pub tags: Vec<String>,
}

impl AnimationSourceResource {
    pub fn from_path(path: impl Into<String>, clip_name: impl Into<String>) -> Self {
        let source_path = path.into();
        let clip_name = clip_name.into();
        let role = AnimationRole::guess_from_name(if clip_name.is_empty() {
            &source_path
        } else {
            &clip_name
        });
        Self {
            provider: AnimationSourceProvider::guess_from_path(&source_path),
            source_path,
            clip_name,
            skeleton: None,
            target_model: None,
            role,
            looping: !matches!(
                role,
                AnimationRole::Roll
                    | AnimationRole::Backstep
                    | AnimationRole::Attack
                    | AnimationRole::Hit
                    | AnimationRole::Death
            ),
            tags: if matches!(role, AnimationRole::Generic) {
                Vec::new()
            } else {
                vec![role.label().to_ascii_lowercase()]
            },
        }
    }
}

/// How a cooked `.psxanim` was produced. This is editor metadata used
/// to avoid treating raw source-compatible clips as if they were
/// universally safe for every model on the same parent table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnimationClipBakeKind {
    /// Legacy or hand-authored resource. Kept playable for existing
    /// projects, but new imports should prefer a more specific value.
    #[default]
    LegacyShared,
    /// Cooked directly from animation data authored with the target
    /// model/export.
    ModelNative,
    /// Cooked from a source clip after retargeting to a target model.
    Retargeted,
}

impl AnimationClipBakeKind {
    pub const ALL: [Self; 3] = [Self::LegacyShared, Self::ModelNative, Self::Retargeted];

    pub const fn label(self) -> &'static str {
        match self {
            Self::LegacyShared => "Legacy/shared",
            Self::ModelNative => "Model native",
            Self::Retargeted => "Retargeted",
        }
    }
}

/// Standalone cooked animation clip. This is the runtime-ready result:
/// either model-native, retargeted to one target model, or legacy
/// skeleton-shared data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnimationClipResource {
    /// Path to the cooked `.psxanim` artifact.
    pub psxanim_path: String,
    /// Skeleton this clip targets.
    #[serde(default)]
    pub skeleton: Option<ResourceId>,
    /// Optional authoring source this cooked clip was baked from.
    #[serde(default)]
    pub source: Option<ResourceId>,
    /// Optional model this cooked clip was baked for. When present,
    /// `resolved_model_animation_clips` only exposes the clip to that
    /// exact model.
    #[serde(default)]
    pub target_model: Option<ResourceId>,
    /// Bake provenance. Runtime ignores this; editor tooling uses it
    /// to distinguish native Meshy clips from future retargeted Mixamo
    /// clips.
    #[serde(default)]
    pub bake: AnimationClipBakeKind,
    /// Semantic role used by auto-assignment and animation sets.
    #[serde(default)]
    pub role: AnimationRole,
    /// Whether gameplay should loop this clip by default.
    #[serde(default = "default_true")]
    pub looping: bool,
    /// Searchable editor tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Per-clip model placement controls used by editor preview and
    /// cooked runtime rendering.
    #[serde(default, skip_serializing_if = "AnimationClipCalibration::is_default")]
    pub calibration: AnimationClipCalibration,
}

impl AnimationClipResource {
    /// Mirror this resource into the legacy model-local clip shape.
    pub fn as_model_clip(&self, name: impl Into<String>) -> ModelAnimationClip {
        ModelAnimationClip {
            name: name.into(),
            psxanim_path: self.psxanim_path.clone(),
            calibration: self.calibration,
        }
    }
}

/// Reusable role mapping for one skeleton. Characters combine a
/// visual model with an Animation Set rather than raw clip indices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnimationSetResource {
    /// Skeleton every assigned clip must target.
    #[serde(default)]
    pub skeleton: Option<ResourceId>,
    #[serde(default)]
    pub idle_clip: Option<ResourceId>,
    #[serde(default)]
    pub walk_clip: Option<ResourceId>,
    #[serde(default)]
    pub run_clip: Option<ResourceId>,
    #[serde(default)]
    pub turn_clip: Option<ResourceId>,
    #[serde(default)]
    pub roll_clip: Option<ResourceId>,
    #[serde(default)]
    pub backstep_clip: Option<ResourceId>,
    /// Preferred action mapping. These bindings are used first
    /// when cooking and let authors assign any compatible clip to
    /// any gameplay action.
    #[serde(default)]
    pub action_clips: Vec<AnimationActionBinding>,
    /// Extra clips included with the set, such as attacks, hit
    /// reactions, death clips, emotes, and experiments.
    #[serde(default)]
    pub clips: Vec<ResourceId>,
}

impl AnimationSetResource {
    pub const fn defaults() -> Self {
        Self {
            skeleton: None,
            idle_clip: None,
            walk_clip: None,
            run_clip: None,
            turn_clip: None,
            roll_clip: None,
            backstep_clip: None,
            action_clips: Vec::new(),
            clips: Vec::new(),
        }
    }

    pub fn action_clip(&self, action: CharacterAnimationAction) -> Option<ResourceId> {
        self.action_clips
            .iter()
            .find_map(|binding| (binding.action == action).then_some(binding.clip))
            .or_else(|| action.role_hint().and_then(|role| self.role_clip(role)))
    }

    pub fn action_binding(
        &self,
        action: CharacterAnimationAction,
    ) -> Option<&AnimationActionBinding> {
        self.action_clips
            .iter()
            .find(|binding| binding.action == action)
    }

    pub fn set_action_clip(&mut self, action: CharacterAnimationAction, clip: Option<ResourceId>) {
        if let Some(role) = action.role_hint() {
            if let Some(slot) = self.role_clip_mut(role) {
                *slot = None;
            }
        }
        match clip {
            Some(clip) => {
                if let Some(binding) = self
                    .action_clips
                    .iter_mut()
                    .find(|binding| binding.action == action)
                {
                    binding.clip = clip;
                } else {
                    self.action_clips.push(AnimationActionBinding {
                        action,
                        clip,
                        options: None,
                    });
                }
            }
            None => self.action_clips.retain(|binding| binding.action != action),
        }
    }

    pub fn role_clip(&self, role: AnimationRole) -> Option<ResourceId> {
        match role {
            AnimationRole::Idle => self.idle_clip,
            AnimationRole::Walk => self.walk_clip,
            AnimationRole::Run => self.run_clip,
            AnimationRole::Turn => self.turn_clip,
            AnimationRole::Roll => self.roll_clip,
            AnimationRole::Backstep => self.backstep_clip,
            AnimationRole::Generic
            | AnimationRole::Attack
            | AnimationRole::Hit
            | AnimationRole::Death => None,
        }
    }

    pub fn role_clip_mut(&mut self, role: AnimationRole) -> Option<&mut Option<ResourceId>> {
        match role {
            AnimationRole::Idle => Some(&mut self.idle_clip),
            AnimationRole::Walk => Some(&mut self.walk_clip),
            AnimationRole::Run => Some(&mut self.run_clip),
            AnimationRole::Turn => Some(&mut self.turn_clip),
            AnimationRole::Roll => Some(&mut self.roll_clip),
            AnimationRole::Backstep => Some(&mut self.backstep_clip),
            AnimationRole::Generic
            | AnimationRole::Attack
            | AnimationRole::Hit
            | AnimationRole::Death => None,
        }
    }
}

impl Default for AnimationSetResource {
    fn default() -> Self {
        Self::defaults()
    }
}

/// Named model attachment point, usually bound to a skeleton
/// joint. Runtime composition is:
/// `entity transform × joint pose × socket local transform`.
///
/// Offsets are integer model/engine units and rotations are Q12
/// turn units (`4096 = 360°`) so project data can be cooked
/// directly for the PS1 without preserving floats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentSocket {
    /// User-facing socket name (`right_hand_grip`, `back_slot`, …).
    pub name: String,
    /// Joint index in the cooked `.psxmdl` skeleton.
    pub joint: u16,
    /// Local translation relative to the joint pose.
    #[serde(default)]
    pub translation: [i32; 3],
    /// Local Euler rotation in Q12 turns: X / Y / Z, 4096 per turn.
    #[serde(default)]
    pub rotation_q12: [i16; 3],
}

impl AttachmentSocket {
    /// Common right-hand default for humanoid rigs.
    pub fn right_hand_grip() -> Self {
        Self {
            name: default_character_socket(),
            joint: 0,
            translation: [0, 0, 0],
            rotation_q12: [0, 0, 0],
        }
    }
}

/// Pivot on a weapon model that should land on a character socket.
/// A sword normally uses `grip`; a shield might use `handle`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeaponGrip {
    /// User-facing grip/pivot name.
    pub name: String,
    /// Local translation inside the weapon model.
    #[serde(default)]
    pub translation: [i32; 3],
    /// Local Euler rotation in Q12 turns: X / Y / Z, 4096 per turn.
    #[serde(default)]
    pub rotation_q12: [i16; 3],
}

impl Default for WeaponGrip {
    fn default() -> Self {
        Self {
            name: default_weapon_grip(),
            translation: [0, 0, 0],
            rotation_q12: [0, 0, 0],
        }
    }
}

/// Weapon hit volume, stored relative to the weapon grip/pivot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WeaponHitShape {
    /// Oriented box hit volume. `half_extents` are local axes.
    Box {
        /// Local center relative to the weapon grip.
        center: [i32; 3],
        /// Half extents in engine/model units.
        half_extents: [u16; 3],
    },
    /// Capsule hit volume, useful for blades, clubs, and spears.
    Capsule {
        /// Local capsule start relative to the weapon grip.
        start: [i32; 3],
        /// Local capsule end relative to the weapon grip.
        end: [i32; 3],
        /// Capsule radius in engine/model units.
        radius: u16,
    },
}

impl Default for WeaponHitShape {
    fn default() -> Self {
        Self::Capsule {
            start: [0, 0, 0],
            end: [0, 512, 0],
            radius: 48,
        }
    }
}

/// One named active hitbox window for a weapon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeaponHitbox {
    /// User-facing hitbox name.
    pub name: String,
    /// Local hit volume.
    #[serde(default)]
    pub shape: WeaponHitShape,
    /// First animation frame where the hitbox is active.
    #[serde(default)]
    pub active_start_frame: u16,
    /// Last animation frame where the hitbox is active.
    #[serde(default)]
    pub active_end_frame: u16,
}

impl Default for WeaponHitbox {
    fn default() -> Self {
        Self {
            name: "Main Hit".to_string(),
            shape: WeaponHitShape::default(),
            active_start_frame: 0,
            active_end_frame: 0,
        }
    }
}

/// Gameplay weapon resource: model reference, grip/pivot, and
/// authored attack hit volumes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeaponResource {
    /// Visual model used for the weapon. `None` is allowed during
    /// authoring so hitboxes can be blocked in before art lands.
    #[serde(default)]
    pub model: Option<ResourceId>,
    /// Which character socket this weapon expects by default.
    #[serde(default = "default_character_socket")]
    pub default_character_socket: String,
    /// Weapon-local grip/pivot that aligns to the character socket.
    #[serde(default)]
    pub grip: WeaponGrip,
    /// Hit volumes authored relative to [`Self::grip`].
    #[serde(default)]
    pub hitboxes: Vec<WeaponHitbox>,
}

impl WeaponResource {
    /// Minimal editable weapon.
    pub fn defaults() -> Self {
        Self {
            model: None,
            default_character_socket: default_character_socket(),
            grip: WeaponGrip::default(),
            hitboxes: vec![WeaponHitbox::default()],
        }
    }
}

impl Default for WeaponResource {
    fn default() -> Self {
        Self::defaults()
    }
}

fn default_character_socket() -> String {
    "right_hand_grip".to_string()
}

fn default_weapon_grip() -> String {
    "grip".to_string()
}

/// Cooked PSX model bundle: a `.psxmdl` plus optional atlas
/// `.psxt` plus zero or more `.psxanim` clips.
///
/// All paths follow the project-relative resolution rule shared
/// with `Texture` resources. `clips` is ordered deterministically
/// (by file name at registration time); `default_clip` /
/// `preview_clip` index into that list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelResource {
    /// Path to the cooked `.psxmdl` artifact.
    pub model_path: String,
    /// Original GLB/glTF/FBX source used to cook this model, when
    /// known. The editor uses this to bake additional animation
    /// sources against the same skeleton.
    #[serde(default)]
    pub source_path: Option<String>,
    /// Optional atlas. Required for textured rendering at runtime;
    /// omitting is allowed for placeholder / debug bundles.
    #[serde(default)]
    pub texture_path: Option<String>,
    /// Skeleton this model was cooked against. Models can still
    /// carry legacy local clips for compatibility, but shared
    /// animation matching should use this resource id.
    #[serde(default)]
    pub skeleton: Option<ResourceId>,
    /// Cooked animation clips, sorted by file name. Empty for
    /// static models (rendered in bind pose).
    #[serde(default)]
    pub clips: Vec<ModelAnimationClip>,
    /// Index into `clips` used at runtime when no per-instance
    /// override is set. `None` means "first clip if any, else
    /// bind pose".
    #[serde(default)]
    pub default_clip: Option<u16>,
    /// Index into `clips` shown in the editor inspector preview.
    /// Falls back to `default_clip` when unset.
    #[serde(default)]
    pub preview_clip: Option<u16>,
    /// Suggested world-space height in engine units (mirrors the
    /// value the cooker stamped into the `.psxmdl` header). Used
    /// by the inspector for sanity checks and by the editor
    /// preview to size selection gizmos.
    #[serde(default = "default_model_world_height")]
    pub world_height: u16,
    /// Authored coarse collision radius in engine units. The
    /// runtime treats model actors as vertical cylinders for
    /// PS1-scale movement/collision.
    #[serde(default = "default_model_collision_radius")]
    pub collision_radius: u16,
    /// Authored bake-time scale in Q8 fixed point (`256 = 1.0`).
    /// Stored as integers so project data mirrors the PS1/runtime
    /// constraint; any application to mesh data must happen during
    /// cook/import, not as runtime floats.
    #[serde(default = "default_model_scale_q8")]
    pub scale_q8: [u16; 3],
    /// Named sockets used by equipment, VFX, and hitbox authoring.
    #[serde(default)]
    pub attachments: Vec<AttachmentSocket>,
}

const fn default_model_world_height() -> u16 {
    1024
}

const fn default_model_collision_radius() -> u16 {
    default_model_collision_radius_for_height(default_model_world_height())
}

pub const fn default_model_collision_radius_for_height(world_height: u16) -> u16 {
    let scaled = (world_height as u32 * 3) / 16;
    if scaled < 80 {
        80
    } else if scaled > 384 {
        384
    } else {
        scaled as u16
    }
}

impl ModelResource {
    /// Human-readable scale factor for one axis.
    pub fn scale_axis(&self, axis: usize) -> f32 {
        self.scale_q8
            .get(axis)
            .copied()
            .unwrap_or(MODEL_SCALE_ONE_Q8) as f32
            / MODEL_SCALE_ONE_Q8 as f32
    }

    /// Index of the clip the editor inspector should preview --
    /// `preview_clip` if set, else `default_clip`, else `None`.
    pub fn effective_preview_clip(&self) -> Option<u16> {
        self.preview_clip.or(self.default_clip)
    }

    /// Index of the clip a runtime instance with no override
    /// should play -- `default_clip` if set, else clip 0 if any
    /// clip exists, else `None`.
    pub fn effective_runtime_clip(&self) -> Option<u16> {
        self.default_clip
            .or_else(|| (!self.clips.is_empty()).then_some(0))
    }
}

/// Gameplay metadata layered on top of a Model. The Model owns
/// the `.psxmdl` / `.psxt` / `.psxanim` artifacts; the Character
/// names which clips fill the idle / walk / run / turn roles.
///
/// Authoring may leave the model unset (the resource still
/// validates to support partial setup); a Character assigned to
/// the player spawn must resolve to a Model with valid idle and
/// walk clips at cook time.
///
/// Engine units throughout -- same convention used by the rest
/// of the runtime (`sector_size = 1024`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterResource {
    /// Backing model. `None` is allowed during authoring;
    /// validated at cook time when assigned to the player.
    #[serde(default)]
    pub model: Option<ResourceId>,
    /// Preferred reusable animation set. When present, cook/preview
    /// resolve role clips from this set and fall back to the legacy
    /// per-model clip indices only for unset roles.
    #[serde(default)]
    pub animation_set: Option<ResourceId>,
    /// Index into the model's clip list -- played when the
    /// character has no movement input. Required for the player.
    #[serde(default)]
    pub idle_clip: Option<u16>,
    /// Index into the model's clip list -- played while walking.
    /// Required for the player.
    #[serde(default)]
    pub walk_clip: Option<u16>,
    /// Index into the model's clip list -- optional run clip.
    #[serde(default)]
    pub run_clip: Option<u16>,
    /// Index into the model's clip list -- optional turn clip.
    #[serde(default)]
    pub turn_clip: Option<u16>,
    /// Index into the model's clip list -- optional roll clip.
    #[serde(default)]
    pub roll_clip: Option<u16>,
    /// Index into the model's clip list -- optional backstep clip.
    #[serde(default)]
    pub backstep_clip: Option<u16>,
    /// Model-local fallback action bindings. Animation Set
    /// bindings are preferred when present.
    #[serde(default)]
    pub action_clips: Vec<CharacterActionClip>,
    /// Capsule radius (engine units). Used by collision +
    /// editor preview gizmo.
    pub radius: u16,
    /// Capsule height (engine units).
    pub height: u16,
    /// Forward walk speed in engine units per frame at 60 Hz.
    pub walk_speed: i32,
    /// Forward run speed in engine units per frame at 60 Hz.
    pub run_speed: i32,
    /// Yaw rate the controller applies when turning.
    pub turn_speed_degrees_per_second: u16,
    /// Maximum stamina. Uses the runtime's Q12-style stamina units.
    #[serde(default = "default_character_stamina_max_q12")]
    pub stamina_max_q12: i32,
    /// Minimum stamina required to start sprinting.
    #[serde(default = "default_character_sprint_min_q12")]
    pub sprint_min_q12: i32,
    /// Stamina drained per 60 Hz sprint frame.
    #[serde(default = "default_character_sprint_drain_q12")]
    pub sprint_drain_q12: i32,
    /// Stamina recovered per grounded non-sprint frame.
    #[serde(default = "default_character_stamina_recover_q12")]
    pub stamina_recover_q12: i32,
    /// Stamina spent to start a roll.
    #[serde(default = "default_character_roll_cost_q12")]
    pub roll_cost_q12: i32,
    /// Roll travel speed in engine units per 60 Hz frame.
    #[serde(default = "default_character_roll_speed")]
    pub roll_speed: i32,
    /// Frames where the roll keeps moving.
    #[serde(default = "default_character_roll_active_frames")]
    pub roll_active_frames: u8,
    /// Recovery frames after roll movement ends.
    #[serde(default = "default_character_roll_recovery_frames")]
    pub roll_recovery_frames: u8,
    /// Invulnerable frames from roll start.
    #[serde(default = "default_character_roll_invulnerable_frames")]
    pub roll_invulnerable_frames: u8,
    /// Stamina spent to start a backstep.
    #[serde(default = "default_character_backstep_cost_q12")]
    pub backstep_cost_q12: i32,
    /// Backstep travel speed in engine units per 60 Hz frame.
    #[serde(default = "default_character_backstep_speed")]
    pub backstep_speed: i32,
    /// Frames where the backstep keeps moving.
    #[serde(default = "default_character_backstep_active_frames")]
    pub backstep_active_frames: u8,
    /// Recovery frames after backstep movement ends.
    #[serde(default = "default_character_backstep_recovery_frames")]
    pub backstep_recovery_frames: u8,
    /// Invulnerable frames from backstep start.
    #[serde(default = "default_character_backstep_invulnerable_frames")]
    pub backstep_invulnerable_frames: u8,
    /// Distance the third-person camera trails the character.
    pub camera_distance: i32,
    /// Camera vertical offset above the character origin.
    pub camera_height: i32,
    /// Vertical offset of the camera's look-at target above
    /// the character origin (typically around the upper torso
    /// for comfortable third-person framing).
    pub camera_target_height: i32,
}

impl CharacterResource {
    /// Sensible defaults for a humanoid third-person character.
    /// Sized for the starter project's 1024-unit sector grid.
    pub const fn defaults() -> Self {
        Self {
            model: None,
            animation_set: None,
            idle_clip: None,
            walk_clip: None,
            run_clip: None,
            turn_clip: None,
            roll_clip: None,
            backstep_clip: None,
            action_clips: Vec::new(),
            radius: default_character_radius(),
            height: default_character_height(),
            walk_speed: default_character_walk_speed(),
            run_speed: default_character_run_speed(),
            turn_speed_degrees_per_second: default_character_turn_speed_degrees_per_second(),
            stamina_max_q12: default_character_stamina_max_q12(),
            sprint_min_q12: default_character_sprint_min_q12(),
            sprint_drain_q12: default_character_sprint_drain_q12(),
            stamina_recover_q12: default_character_stamina_recover_q12(),
            roll_cost_q12: default_character_roll_cost_q12(),
            roll_speed: default_character_roll_speed(),
            roll_active_frames: default_character_roll_active_frames(),
            roll_recovery_frames: default_character_roll_recovery_frames(),
            roll_invulnerable_frames: default_character_roll_invulnerable_frames(),
            backstep_cost_q12: default_character_backstep_cost_q12(),
            backstep_speed: default_character_backstep_speed(),
            backstep_active_frames: default_character_backstep_active_frames(),
            backstep_recovery_frames: default_character_backstep_recovery_frames(),
            backstep_invulnerable_frames: default_character_backstep_invulnerable_frames(),
            camera_distance: 6144,
            camera_height: 1280,
            camera_target_height: 640,
        }
    }

    pub fn action_clip(&self, action: CharacterAnimationAction) -> Option<u16> {
        self.action_clips
            .iter()
            .find_map(|binding| (binding.action == action).then_some(binding.clip))
            .or_else(|| match action {
                CharacterAnimationAction::Idle => self.idle_clip,
                CharacterAnimationAction::Walk => self.walk_clip,
                CharacterAnimationAction::Run => self.run_clip,
                CharacterAnimationAction::Turn => self.turn_clip,
                CharacterAnimationAction::Roll => self.roll_clip,
                CharacterAnimationAction::Backstep => self.backstep_clip,
                CharacterAnimationAction::LightAttack
                | CharacterAnimationAction::HeavyAttack
                | CharacterAnimationAction::ComboAttack
                | CharacterAnimationAction::Block
                | CharacterAnimationAction::HitReact
                | CharacterAnimationAction::Death => None,
            })
    }

    pub fn action_binding(&self, action: CharacterAnimationAction) -> Option<&CharacterActionClip> {
        self.action_clips
            .iter()
            .find(|binding| binding.action == action)
    }

    pub fn set_action_clip(&mut self, action: CharacterAnimationAction, clip: Option<u16>) {
        match action {
            CharacterAnimationAction::Idle => self.idle_clip = None,
            CharacterAnimationAction::Walk => self.walk_clip = None,
            CharacterAnimationAction::Run => self.run_clip = None,
            CharacterAnimationAction::Turn => self.turn_clip = None,
            CharacterAnimationAction::Roll => self.roll_clip = None,
            CharacterAnimationAction::Backstep => self.backstep_clip = None,
            CharacterAnimationAction::LightAttack
            | CharacterAnimationAction::HeavyAttack
            | CharacterAnimationAction::ComboAttack
            | CharacterAnimationAction::Block
            | CharacterAnimationAction::HitReact
            | CharacterAnimationAction::Death => {}
        }
        match clip {
            Some(clip) => {
                if let Some(binding) = self
                    .action_clips
                    .iter_mut()
                    .find(|binding| binding.action == action)
                {
                    binding.clip = clip;
                } else {
                    self.action_clips.push(CharacterActionClip {
                        action,
                        clip,
                        options: None,
                    });
                }
            }
            None => self.action_clips.retain(|binding| binding.action != action),
        }
    }
}

const fn default_character_stamina_max_q12() -> i32 {
    4096
}

const fn default_character_radius() -> u16 {
    192
}

const fn default_character_height() -> u16 {
    1024
}

const fn default_character_walk_speed() -> i32 {
    48
}

const fn default_character_run_speed() -> i32 {
    96
}

const fn default_character_turn_speed_degrees_per_second() -> u16 {
    180
}

const fn default_character_sprint_min_q12() -> i32 {
    384
}

const fn default_character_sprint_drain_q12() -> i32 {
    10
}

const fn default_character_stamina_recover_q12() -> i32 {
    36
}

const fn default_character_roll_cost_q12() -> i32 {
    768
}

const fn default_character_roll_speed() -> i32 {
    96
}

const fn default_character_roll_active_frames() -> u8 {
    14
}

const fn default_character_roll_recovery_frames() -> u8 {
    12
}

const fn default_character_roll_invulnerable_frames() -> u8 {
    10
}

const fn default_character_backstep_cost_q12() -> i32 {
    512
}

const fn default_character_backstep_speed() -> i32 {
    72
}

const fn default_character_backstep_active_frames() -> u8 {
    8
}

const fn default_character_backstep_recovery_frames() -> u8 {
    10
}

const fn default_character_backstep_invulnerable_frames() -> u8 {
    6
}

impl Default for CharacterResource {
    fn default() -> Self {
        Self::defaults()
    }
}

/// Tunable movement/collision settings authored on a
/// [`NodeKind::CharacterController`] component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterControllerSettings {
    /// Capsule radius (engine units).
    #[serde(default = "default_character_radius")]
    pub radius: u16,
    /// Capsule height (engine units).
    #[serde(default = "default_character_height")]
    pub height: u16,
    /// Forward walk speed in engine units per frame at 60 Hz.
    #[serde(default = "default_character_walk_speed")]
    pub walk_speed: i32,
    /// Forward run speed in engine units per frame at 60 Hz.
    #[serde(default = "default_character_run_speed")]
    pub run_speed: i32,
    /// Yaw rate the controller applies when turning.
    #[serde(default = "default_character_turn_speed_degrees_per_second")]
    pub turn_speed_degrees_per_second: u16,
    /// Maximum stamina. Uses the runtime's Q12-style stamina units.
    #[serde(default = "default_character_stamina_max_q12")]
    pub stamina_max_q12: i32,
    /// Minimum stamina required to start sprinting.
    #[serde(default = "default_character_sprint_min_q12")]
    pub sprint_min_q12: i32,
    /// Stamina drained per 60 Hz sprint frame.
    #[serde(default = "default_character_sprint_drain_q12")]
    pub sprint_drain_q12: i32,
    /// Stamina recovered per grounded non-sprint frame.
    #[serde(default = "default_character_stamina_recover_q12")]
    pub stamina_recover_q12: i32,
    /// Stamina spent to start a roll.
    #[serde(default = "default_character_roll_cost_q12")]
    pub roll_cost_q12: i32,
    /// Roll travel speed in engine units per 60 Hz frame.
    #[serde(default = "default_character_roll_speed")]
    pub roll_speed: i32,
    /// Frames where the roll keeps moving.
    #[serde(default = "default_character_roll_active_frames")]
    pub roll_active_frames: u8,
    /// Recovery frames after roll movement ends.
    #[serde(default = "default_character_roll_recovery_frames")]
    pub roll_recovery_frames: u8,
    /// Invulnerable frames from roll start.
    #[serde(default = "default_character_roll_invulnerable_frames")]
    pub roll_invulnerable_frames: u8,
    /// Stamina spent to start a backstep.
    #[serde(default = "default_character_backstep_cost_q12")]
    pub backstep_cost_q12: i32,
    /// Backstep travel speed in engine units per 60 Hz frame.
    #[serde(default = "default_character_backstep_speed")]
    pub backstep_speed: i32,
    /// Frames where the backstep keeps moving.
    #[serde(default = "default_character_backstep_active_frames")]
    pub backstep_active_frames: u8,
    /// Recovery frames after backstep movement ends.
    #[serde(default = "default_character_backstep_recovery_frames")]
    pub backstep_recovery_frames: u8,
    /// Invulnerable frames from backstep start.
    #[serde(default = "default_character_backstep_invulnerable_frames")]
    pub backstep_invulnerable_frames: u8,
}

impl CharacterControllerSettings {
    pub const fn defaults() -> Self {
        Self {
            radius: default_character_radius(),
            height: default_character_height(),
            walk_speed: default_character_walk_speed(),
            run_speed: default_character_run_speed(),
            turn_speed_degrees_per_second: default_character_turn_speed_degrees_per_second(),
            stamina_max_q12: default_character_stamina_max_q12(),
            sprint_min_q12: default_character_sprint_min_q12(),
            sprint_drain_q12: default_character_sprint_drain_q12(),
            stamina_recover_q12: default_character_stamina_recover_q12(),
            roll_cost_q12: default_character_roll_cost_q12(),
            roll_speed: default_character_roll_speed(),
            roll_active_frames: default_character_roll_active_frames(),
            roll_recovery_frames: default_character_roll_recovery_frames(),
            roll_invulnerable_frames: default_character_roll_invulnerable_frames(),
            backstep_cost_q12: default_character_backstep_cost_q12(),
            backstep_speed: default_character_backstep_speed(),
            backstep_active_frames: default_character_backstep_active_frames(),
            backstep_recovery_frames: default_character_backstep_recovery_frames(),
            backstep_invulnerable_frames: default_character_backstep_invulnerable_frames(),
        }
    }

    pub fn from_character(character: &CharacterResource) -> Self {
        Self {
            radius: character.radius,
            height: character.height,
            walk_speed: character.walk_speed,
            run_speed: character.run_speed,
            turn_speed_degrees_per_second: character.turn_speed_degrees_per_second,
            stamina_max_q12: character.stamina_max_q12,
            sprint_min_q12: character.sprint_min_q12,
            sprint_drain_q12: character.sprint_drain_q12,
            stamina_recover_q12: character.stamina_recover_q12,
            roll_cost_q12: character.roll_cost_q12,
            roll_speed: character.roll_speed,
            roll_active_frames: character.roll_active_frames,
            roll_recovery_frames: character.roll_recovery_frames,
            roll_invulnerable_frames: character.roll_invulnerable_frames,
            backstep_cost_q12: character.backstep_cost_q12,
            backstep_speed: character.backstep_speed,
            backstep_active_frames: character.backstep_active_frames,
            backstep_recovery_frames: character.backstep_recovery_frames,
            backstep_invulnerable_frames: character.backstep_invulnerable_frames,
        }
    }
}

impl Default for CharacterControllerSettings {
    fn default() -> Self {
        Self::defaults()
    }
}

/// Resource payloads available to editor scenes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceData {
    /// Cooked PSXT artifact reference.
    ///
    /// The editor and the runtime both consume the same `.psxt` blob
    /// -- the runtime via `include_bytes!` at compile time, the editor
    /// via `std::fs::read` at refresh time and `psx_asset::Texture::from_bytes`
    /// to extract pixel + CLUT bytes. PNG/JPG/BMP → PSXT cooking lives
    /// in `texture_import` and the `psxed tex` CLI; runtime paths still
    /// consume only cooked blobs.
    Texture {
        /// Path to the cooked `.psxt` artifact. Resolved at refresh
        /// time first as-is (absolute paths), then relative to the
        /// project file's directory, then relative to the workspace
        /// cwd. The starter project ships paths relative to the repo
        /// root so `cargo run -p frontend` from `/repos/psoxide` finds
        /// the canonical `assets/textures/*.psxt`.
        psxt_path: String,
    },
    /// Editor material.
    Material(MaterialResource),
    /// Cooked animated PSX model -- `.psxmdl` + optional `.psxt`
    /// atlas + animation clips. Instantiated in scenes by placing an
    /// [`NodeKind::Entity`] with a [`NodeKind::ModelRenderer`]
    /// component referencing this resource id.
    Model(ModelResource),
    /// Skeleton compatibility contract shared by models and
    /// standalone animation clips.
    Skeleton(SkeletonResource),
    /// Authoring-time animation library entry. Source clips are
    /// previewed / retargeted / baked by editor tooling; runtime uses
    /// [`ResourceData::AnimationClip`] only.
    AnimationSource(AnimationSourceResource),
    /// Standalone cooked animation clip bound to a skeleton.
    AnimationClip(AnimationClipResource),
    /// Reusable role mapping for characters on one skeleton.
    AnimationSet(AnimationSetResource),
    /// Legacy / generic source mesh path. Kept for backward
    /// compatibility; new authoring should use [`ResourceData::Model`].
    Mesh {
        /// Project-relative source path.
        source_path: String,
    },
    /// Nested room/prefab reference.
    Scene {
        /// Project-relative room/prefab path.
        source_path: String,
    },
    /// Script resource.
    Script {
        /// Project-relative script path.
        source_path: String,
    },
    /// Audio resource.
    Audio {
        /// Project-relative audio path.
        source_path: String,
    },
    /// Optional gameplay preset with model, animation, capsule, and
    /// camera defaults. Component-authored entities can override the
    /// visual model through Model Renderer, action clips through
    /// Animator, and movement tuning through Character Controller.
    Character(CharacterResource),
    /// Equipment/weapon authoring resource. A Weapon references a
    /// Model for visuals and owns grip + hitbox data for combat.
    Weapon(WeaponResource),
}

impl ResourceData {
    /// User-facing type label.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Texture { .. } => "Texture",
            Self::Material(_) => "Material",
            Self::Model(_) => "Model",
            Self::Skeleton(_) => "Skeleton",
            Self::AnimationSource(_) => "Animation Source",
            Self::AnimationClip(_) => "Animation Clip",
            Self::AnimationSet(_) => "Clip Role Map",
            Self::Mesh { .. } => "Mesh",
            Self::Scene { .. } => "Room",
            Self::Script { .. } => "Script",
            Self::Audio { .. } => "Audio",
            Self::Character(_) => "Character Profile",
            Self::Weapon(_) => "Weapon",
        }
    }
}

/// One named project resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resource {
    /// Stable resource id.
    pub id: ResourceId,
    /// Display name.
    pub name: String,
    /// Payload.
    pub data: ResourceData,
}

/// One animation clip that a model can play after resolving both
/// legacy model-local clips and standalone animation resources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModelAnimationClip {
    /// Display name for dropdowns/runtime manifests.
    pub name: String,
    /// Cooked `.psxanim` path.
    pub psxanim_path: String,
    /// Standalone animation resource id when this row came from the
    /// animation library. `None` means it came from
    /// `ModelResource::clips`.
    pub animation_resource: Option<ResourceId>,
    /// Model-local clip index when this row came from
    /// `ModelResource::clips`.
    pub model_clip_index: Option<usize>,
    /// Per-clip placement calibration.
    pub calibration: AnimationClipCalibration,
}

/// One backing-file move performed by a resource rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceFileRename {
    /// Previous stored project path.
    pub from: String,
    /// New stored project path.
    pub to: String,
}

/// One backing file deleted with a resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceFileDelete {
    /// Stored project-relative path that was deleted.
    pub path: String,
}

/// Summary returned after renaming a resource and any backing files
/// that are safe for the project to own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResourceRenameReport {
    /// Files that were physically moved and whose project paths were
    /// updated.
    pub renamed_files: Vec<ResourceFileRename>,
    /// Path fields that were left alone because they were empty,
    /// missing on disk, outside the project root, or otherwise not
    /// safe to move automatically.
    pub skipped_files: Vec<String>,
}

/// Summary returned after removing a resource from the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceDeleteReport {
    /// Resource removed from the project's resource table.
    pub removed: Resource,
    /// Number of project references cleared because they pointed at
    /// the removed resource.
    pub cleared_references: usize,
    /// Project-owned backing files physically removed from disk.
    pub deleted_files: Vec<ResourceFileDelete>,
    /// Path fields left alone because they were empty, missing,
    /// outside the project root, or otherwise not safe to delete.
    pub skipped_files: Vec<String>,
}

/// Failure modes for [`ProjectDocument::rename_resource_with_files`].
#[derive(Debug)]
pub enum ResourceRenameError {
    /// No resource with the requested id exists.
    MissingResource(ResourceId),
    /// Empty or whitespace-only names are refused.
    EmptyName,
    /// Two planned file moves would write the same destination.
    DuplicateTarget(PathBuf),
    /// A planned destination already exists.
    TargetExists(PathBuf),
    /// Filesystem operation failed.
    Io {
        /// Source path.
        from: PathBuf,
        /// Destination path.
        to: PathBuf,
        /// Error detail.
        detail: String,
    },
}

impl std::fmt::Display for ResourceRenameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingResource(id) => write!(f, "resource #{} does not exist", id.raw()),
            Self::EmptyName => write!(f, "resource name cannot be empty"),
            Self::DuplicateTarget(path) => {
                write!(f, "multiple files would rename to {}", path.display())
            }
            Self::TargetExists(path) => write!(f, "target already exists: {}", path.display()),
            Self::Io { from, to, detail } => write!(
                f,
                "failed to rename {} to {}: {detail}",
                from.display(),
                to.display()
            ),
        }
    }
}

impl std::error::Error for ResourceRenameError {}

/// Failure modes for [`ProjectDocument::delete_resource_with_files`].
#[derive(Debug)]
pub enum ResourceDeleteError {
    /// No resource with the requested id exists.
    MissingResource(ResourceId),
    /// Filesystem operation failed.
    Io {
        /// File path that could not be removed.
        path: PathBuf,
        /// Error detail.
        detail: String,
    },
}

impl std::fmt::Display for ResourceDeleteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingResource(id) => write!(f, "resource #{} does not exist", id.raw()),
            Self::Io { path, detail } => {
                write!(f, "failed to delete {}: {detail}", path.display())
            }
        }
    }
}

impl std::error::Error for ResourceDeleteError {}

/// Node type used by the editor scene tree.
///
/// Hierarchy convention for level authoring:
/// `World (scene root) -> Room (sector grid) -> portal/entity nodes`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NodeKind {
    /// Plain organisational node.
    Node,
    /// Spatial transform node.
    Node3D,
    /// Composed world object. The node owns transform/identity;
    /// behaviour is expressed by component-node children such as
    /// [`ModelRenderer`](Self::ModelRenderer),
    /// [`Animator`](Self::Animator), and
    /// [`Collider`](Self::Collider).
    Entity,
    /// World-root node for one authored world. Owns global settings
    /// inherited by descendant room grids.
    World {
        /// Shared sector size in engine units, snapped to
        /// [`WORLD_SECTOR_SIZE_QUANTUM`].
        #[serde(default = "default_world_sector_size")]
        sector_size: i32,
        /// Background sky drawn before room geometry.
        #[serde(default)]
        sky: SkySettings,
        /// Distant scenery ring drawn between sky and room geometry.
        #[serde(default)]
        far_vista: FarVistaSettings,
        /// Third-person camera defaults inherited by descendant rooms.
        #[serde(default)]
        camera: WorldCameraSettings,
        /// Runtime culling controls inherited by descendant rooms.
        #[serde(default)]
        culling: WorldCullingSettings,
        /// Cook-time streaming controls inherited by descendant rooms.
        #[serde(default)]
        streaming: WorldStreamingSettings,
    },
    /// One authored Tomb Raider-style room: a sector grid plus its
    /// child entities and portal links.
    #[serde(rename = "Room", alias = "Map")]
    Room {
        /// Authored grid-world payload.
        grid: WorldGrid,
    },
    /// Static or dynamic mesh / model instance.
    ///
    /// `mesh` references either a legacy [`ResourceData::Mesh`] or a
    /// cooked [`ResourceData::Model`]. When it points at a Model,
    /// `animation_clip` selects which clip plays -- an explicit
    /// `Some(idx)` overrides the model's `default_clip`; `None`
    /// inherits the model default. Instances of legacy meshes
    /// ignore this field.
    MeshInstance {
        /// Mesh / model resource.
        mesh: Option<ResourceId>,
        /// Material override (legacy mesh path; ignored for Model
        /// resources, which embed material data in the `.psxmdl`).
        material: Option<ResourceId>,
        /// Per-instance animation clip override.
        #[serde(default)]
        animation_clip: Option<u16>,
    },
    /// Flat material-backed image plane. The node transform marks
    /// the bottom-center anchor; yaw controls the static facing
    /// direction unless cylindrical billboarding is enabled.
    ImageProp {
        /// Material used by the quad.
        #[serde(default)]
        material: Option<ResourceId>,
        /// Authored width in engine/editor units.
        #[serde(default = "default_image_prop_size")]
        width: u16,
        /// Authored height in engine/editor units.
        #[serde(default = "default_image_prop_size")]
        height: u16,
        /// Rotate around Y every frame so the card faces the camera
        /// while staying upright.
        #[serde(default)]
        cylindrical_billboard: bool,
        /// Toggle the authored AABB collision box around the prop.
        /// Disabled by default so legacy props (and freshly placed
        /// ones) keep the "decorative-only" semantics they had
        /// before collision was opt-in.
        #[serde(default)]
        collision_enabled: bool,
        /// Full size (width / height / depth) of the AABB collision
        /// box in engine units, centered on the visible plane.
        /// Ignored when [`collision_enabled`](Self::ImageProp) is
        /// `false`, but kept around so toggling it back on restores
        /// the user's last size instead of snapping to a default.
        #[serde(default = "default_image_prop_collision_size")]
        collision_size: [u16; 3],
    },
    /// Material-backed editable hexahedron. The transform is a
    /// bottom-center anchor, `vertices` are local engine units from
    /// that anchor, and each face can bind its own material.
    BoxProp {
        /// Per-face material slots in [`BOX_PROP_FACE_NAMES`] order.
        #[serde(default = "default_box_prop_materials")]
        materials: [Option<ResourceId>; BOX_PROP_FACE_COUNT],
        /// Editable local vertices, bottom ring then top ring.
        #[serde(default = "default_box_prop_vertices")]
        vertices: [[i16; 3]; BOX_PROP_VERTEX_COUNT],
        /// Whether this prop blocks the character motor.
        #[serde(default = "default_true")]
        collision_enabled: bool,
        /// Authored break trigger bits from [`psx_level::box_prop_flags`].
        #[serde(default)]
        break_flags: u16,
    },
    /// Render a cooked [`ResourceData::Model`] from the transform
    /// on the nearest entity ancestor. This is the component form of
    /// the legacy [`MeshInstance`](Self::MeshInstance) node.
    ModelRenderer {
        /// Model resource.
        model: Option<ResourceId>,
        /// Optional material override for legacy/static paths.
        /// Cooked PSX models currently carry their own atlas and
        /// ignore this field.
        #[serde(default)]
        material: Option<ResourceId>,
        /// Render-only offset from the owning Entity root to the
        /// model origin, in entity-local engine units. This does
        /// not affect collision, camera, or movement.
        #[serde(default)]
        visual_offset: [i16; 3],
        /// Render-only uniform scale in Q8 fixed point (`256 =
        /// 1.0`). Use this for per-instance calibration; use the
        /// Model resource import scale for global asset fixes.
        #[serde(default = "default_model_renderer_visual_scale_q8")]
        visual_scale_q8: u16,
    },
    /// Animation component for a model-rendering entity. `clip`
    /// overrides the model default when set; `None` inherits the
    /// model's runtime default.
    Animator {
        /// Per-instance clip override.
        #[serde(default)]
        clip: Option<u16>,
        /// Gameplay action to model-local animation clip mapping.
        /// This is the authoritative authoring location for
        /// player/NPC action animation.
        #[serde(default)]
        action_clips: Vec<CharacterActionClip>,
        /// Whether this animation should run automatically in the
        /// editor/playtest runtime.
        #[serde(default = "default_true")]
        autoplay: bool,
    },
    /// Collision component. The first runtime pass only cooks room
    /// grid collision, but keeping authored collider data as a node
    /// makes entity/interactable/NPC architecture explicit now.
    Collider {
        /// Collision shape in engine/editor units.
        #[serde(default)]
        shape: ColliderShape,
        /// Solid colliders block movement; non-solid colliders are
        /// trigger volumes.
        #[serde(default = "default_true")]
        solid: bool,
    },
    /// Interactable component for props such as chests, doors, and
    /// levers. Runtime behaviour is not cooked yet; this is authoring
    /// structure for the upcoming object pass.
    Interactable {
        /// UI prompt or editor-facing affordance.
        #[serde(default)]
        prompt: String,
        /// Logical action id.
        #[serde(default)]
        action: String,
    },
    /// Character/controller component. It binds an entity to a reusable
    /// [`ResourceData::Character`] profile. When `player` is true this is
    /// the component-tree replacement for a legacy player
    /// [`SpawnPoint`](Self::SpawnPoint); non-player controllers cook as
    /// idle model instances until dedicated NPC runtime records exist.
    CharacterController {
        /// Character profile resource.
        #[serde(default)]
        character: Option<ResourceId>,
        /// Movement, stamina, evade, and coarse capsule tuning for this controller.
        #[serde(default)]
        settings: CharacterControllerSettings,
        /// Whether this controller drives the player.
        #[serde(default)]
        player: bool,
    },
    /// AI marker component for future NPC/enemy runtime records.
    AiController {
        /// Logical AI profile id.
        #[serde(default)]
        behavior: String,
    },
    /// Combat stat component for entity nodes.
    Combat {
        /// Team/faction label.
        #[serde(default)]
        faction: String,
        /// Hit points.
        #[serde(default = "default_combat_health")]
        health: u16,
    },
    /// Equipment component. The parent Entity supplies the animated
    /// character model; this component names the Weapon and which
    /// socket/grip pair should be composed.
    Equipment {
        /// Weapon resource.
        #[serde(default)]
        weapon: Option<ResourceId>,
        /// Character/model socket to follow.
        #[serde(default = "default_character_socket")]
        character_socket: String,
        /// Weapon-local grip/pivot to align to the character socket.
        #[serde(default = "default_weapon_grip")]
        weapon_grip: String,
    },
    /// Static point light.
    PointLight {
        /// RGB light colour.
        #[serde(default = "default_light_color")]
        color: [u8; 3],
        /// Light intensity multiplier.
        intensity: f32,
        /// Approximate editor/runtime radius in sectors.
        radius: f32,
    },
    /// Spawn marker.
    SpawnPoint {
        /// Whether this is the player spawn.
        player: bool,
        /// Character profile resource that drives this spawn. For the
        /// player spawn this picks the player's model + role
        /// clips + controller params. `None` lets the cook step
        /// auto-pick a Character when exactly one exists, or
        /// fail with a clear error otherwise. Non-player spawns
        /// currently ignore this field.
        #[serde(default)]
        character: Option<ResourceId>,
    },
    /// Trigger volume marker.
    Trigger {
        /// Logical trigger id.
        trigger_id: String,
    },
    /// Positional audio source.
    AudioSource {
        /// Audio resource.
        sound: Option<ResourceId>,
        /// Playback radius.
        radius: f32,
    },
    /// Manual streaming/visibility graph edge: the cooker snaps the marker
    /// to a grid edge and treats that edge as a room-to-room portal.
    Portal {
        /// Target room node by id, or `None` when not wired.
        target_room: Option<NodeId>,
        /// Entry-portal label on the target room.
        target_entry: String,
        /// Identifier this portal marker is known by in its source room.
        entry_name: String,
        /// Optional exact 3D portal plane imported from a Tomb
        /// Raider-style level file.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        geometry: Option<PortalGeometry>,
    },
}

impl NodeKind {
    /// Default scene-root World node.
    pub fn default_world() -> Self {
        Self::World {
            sector_size: DEFAULT_WORLD_SECTOR_SIZE,
            sky: SkySettings::default(),
            far_vista: FarVistaSettings::default(),
            camera: WorldCameraSettings::default(),
            culling: WorldCullingSettings::default(),
            streaming: WorldStreamingSettings::default(),
        }
    }

    /// User-facing label.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Node => "Node",
            Self::Node3D => "Node3D",
            Self::Entity => "Entity",
            Self::World { .. } => "World",
            Self::Room { .. } => "Room",
            Self::MeshInstance { .. } => "Mesh Instance",
            Self::ImageProp { .. } => "Image Prop",
            Self::BoxProp { .. } => "Box Prop",
            Self::ModelRenderer { .. } => "Model Renderer",
            Self::Animator { .. } => "Animator",
            Self::Collider { .. } => "Collider",
            Self::Interactable { .. } => "Interactable",
            Self::CharacterController { .. } => "Character Controller",
            Self::AiController { .. } => "AI Controller",
            Self::Combat { .. } => "Combat",
            Self::Equipment { .. } => "Equipment",
            Self::PointLight { .. } => "Point Light",
            Self::SpawnPoint { .. } => "Spawn Point",
            Self::Trigger { .. } => "Trigger",
            Self::AudioSource { .. } => "Audio Source",
            Self::Portal { .. } => "Portal",
        }
    }

    /// True for behaviour/component nodes that are intended to be
    /// children of an [`Entity`](Self::Entity) host rather than
    /// independent placed objects.
    pub const fn is_component(self: &Self) -> bool {
        matches!(
            self,
            Self::ModelRenderer { .. }
                | Self::Animator { .. }
                | Self::Collider { .. }
                | Self::Interactable { .. }
                | Self::CharacterController { .. }
                | Self::AiController { .. }
                | Self::Combat { .. }
                | Self::Equipment { .. }
        )
    }
}

/// Authored collision shape for component-node entities.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ColliderShape {
    /// Axis-aligned box, stored as half-extents.
    Box {
        /// Half extents in engine/editor units.
        half_extents: [u16; 3],
    },
    /// Sphere collider.
    Sphere {
        /// Radius in engine/editor units.
        radius: u16,
    },
    /// Upright capsule.
    Capsule {
        /// Radius in engine/editor units.
        radius: u16,
        /// Height in engine/editor units.
        height: u16,
    },
}

impl Default for ColliderShape {
    fn default() -> Self {
        Self::Box {
            half_extents: [256, 256, 256],
        }
    }
}

const fn default_true() -> bool {
    true
}

const fn default_combat_health() -> u16 {
    1
}

/// Explicit Tomb Raider-style portal rectangle.
///
/// Authored seam portals still use the marker transform and snap to
/// sector edges. Imported TR levels already carry the exact 3D
/// rectangle that connects two rooms, so keep that information on
/// the portal node instead of trying to rediscover it from a 2D grid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortalGeometry {
    /// Portal normal in editor/world coordinates.
    pub normal: [i32; 3],
    /// Portal corners in editor/world coordinates.
    pub vertices: [[i32; 3]; 4],
}

/// A scene-tree node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneNode {
    /// Stable node id.
    pub id: NodeId,
    /// Display name.
    pub name: String,
    /// Node type.
    pub kind: NodeKind,
    /// Local transform.
    pub transform: Transform3,
    /// Parent id, absent only for the scene root.
    pub parent: Option<NodeId>,
    /// Ordered child ids.
    pub children: Vec<NodeId>,
}

impl SceneNode {
    fn new(id: NodeId, parent: Option<NodeId>, name: impl Into<String>, kind: NodeKind) -> Self {
        Self {
            id,
            name: name.into(),
            kind,
            transform: Transform3::default(),
            parent,
            children: Vec::new(),
        }
    }
}

/// Owned row used by hierarchy UI without borrowing the scene.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRow {
    /// Node id.
    pub id: NodeId,
    /// Parent node id, or `None` for the scene root.
    pub parent: Option<NodeId>,
    /// Tree depth from root.
    pub depth: usize,
    /// Index of this node inside its parent's `children` list. Used
    /// by the editor's drag-drop machinery so a "drop above this row"
    /// gesture maps cleanly to `move_node(.., parent, sibling_index)`.
    pub sibling_index: usize,
    /// Display name.
    pub name: String,
    /// Node kind label.
    pub kind: &'static str,
    /// Number of direct children.
    pub child_count: usize,
}

/// One editor scene.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    /// Display name.
    pub name: String,
    /// World root node id.
    pub root: NodeId,
    next_node_id: u64,
    nodes: Vec<SceneNode>,
}

impl Scene {
    /// Create a scene with one root `World`.
    pub fn new(name: impl Into<String>) -> Self {
        let root = SceneNode::new(NodeId::ROOT, None, "World", NodeKind::default_world());
        Self {
            name: name.into(),
            root: NodeId::ROOT,
            next_node_id: NodeId::ROOT.raw() + 1,
            nodes: vec![root],
        }
    }

    /// Normalize legacy `Root -> World -> Room` scenes into the
    /// Tomb Raider-style `World(root) -> Room` hierarchy.
    pub fn normalize_world_root(&mut self) {
        let root_id = self.root;
        if self.node(root_id).is_none() {
            self.nodes.insert(
                0,
                SceneNode::new(root_id, None, "World", NodeKind::default_world()),
            );
        }

        let child_world = self.node(root_id).and_then(|root| {
            root.children.iter().copied().find(|id| {
                self.node(*id)
                    .is_some_and(|node| matches!(&node.kind, NodeKind::World { .. }))
            })
        });

        if self
            .node(root_id)
            .is_some_and(|root| matches!(&root.kind, NodeKind::World { .. }))
        {
            if let Some(root) = self.node_mut(root_id) {
                root.parent = None;
                if root.name == "Root" || root.name.is_empty() {
                    root.name = "World".to_string();
                }
            }
            return;
        }

        if let Some(world_id) = child_world {
            let Some(world_node) = self.node(world_id).cloned() else {
                return;
            };
            let mut merged_children = self
                .node(root_id)
                .map(|root| root.children.clone())
                .unwrap_or_default()
                .into_iter()
                .filter(|id| *id != world_id)
                .collect::<Vec<_>>();
            for child in world_node.children {
                if child != root_id && !merged_children.contains(&child) {
                    merged_children.push(child);
                }
            }
            for node in &mut self.nodes {
                if node.parent == Some(world_id) {
                    node.parent = Some(root_id);
                }
                node.children.retain(|child| *child != world_id);
            }
            if let Some(root) = self.node_mut(root_id) {
                root.name = if world_node.name.is_empty() || world_node.name == "Root" {
                    "World".to_string()
                } else {
                    world_node.name
                };
                root.kind = world_node.kind;
                root.parent = None;
                root.children = merged_children;
            }
            self.nodes.retain(|node| node.id != world_id);
        } else if let Some(root) = self.node_mut(root_id) {
            root.name = "World".to_string();
            root.kind = NodeKind::default_world();
            root.parent = None;
        }
    }

    /// All nodes in storage order.
    pub fn nodes(&self) -> &[SceneNode] {
        &self.nodes
    }

    /// Get a node.
    pub fn node(&self, id: NodeId) -> Option<&SceneNode> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// Get a mutable node.
    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut SceneNode> {
        self.nodes.iter_mut().find(|node| node.id == id)
    }

    /// Add a node under `parent`. Invalid parents fall back to the root.
    pub fn add_node(&mut self, parent: NodeId, name: impl Into<String>, kind: NodeKind) -> NodeId {
        let parent = if self.node(parent).is_some() {
            parent
        } else {
            self.root
        };
        let id = NodeId(self.next_node_id);
        self.next_node_id = self.next_node_id.saturating_add(1);
        self.nodes
            .push(SceneNode::new(id, Some(parent), name, kind));
        if let Some(parent_node) = self.node_mut(parent) {
            parent_node.children.push(id);
        }
        id
    }

    /// Remove a non-root node and its descendants.
    pub fn remove_node(&mut self, id: NodeId) -> bool {
        if id == self.root || self.node(id).is_none() {
            return false;
        }

        let mut doomed = Vec::new();
        self.collect_descendants(id, &mut doomed);
        doomed.push(id);

        for node in &mut self.nodes {
            node.children.retain(|child| !doomed.contains(child));
        }
        self.nodes.retain(|node| !doomed.contains(&node.id));
        true
    }

    /// `true` when `ancestor` appears anywhere on the parent chain of
    /// `id`. Includes `id` itself in the check, so callers using this
    /// for cycle detection don't need a separate equality test.
    pub fn is_descendant_of(&self, id: NodeId, ancestor: NodeId) -> bool {
        if id == ancestor {
            return true;
        }
        let mut current = self.node(id).and_then(|n| n.parent);
        while let Some(p) = current {
            if p == ancestor {
                return true;
            }
            current = self.node(p).and_then(|n| n.parent);
        }
        false
    }

    /// Move `id` under `new_parent` at `position` in its child list.
    ///
    /// Refuses (returns `false`) when:
    /// * `id` is the world root,
    /// * `id` or `new_parent` is missing,
    /// * `new_parent` is `id` or any of its descendants -- that would
    ///   form a cycle.
    ///
    /// `position` clamps to the destination's current child count.
    /// Reordering inside the same parent works because `id` is removed
    /// from the child list before `position` is clamped, so dropping
    /// at "the same slot" is a no-op without UI corner cases.
    pub fn move_node(&mut self, id: NodeId, new_parent: NodeId, position: usize) -> bool {
        if id == self.root {
            return false;
        }
        if self.node(id).is_none() || self.node(new_parent).is_none() {
            return false;
        }
        if self.is_descendant_of(new_parent, id) {
            return false;
        }

        let old_parent = self.node(id).and_then(|n| n.parent);
        if let Some(old) = old_parent {
            if let Some(parent) = self.node_mut(old) {
                parent.children.retain(|c| *c != id);
            }
        }
        if let Some(parent) = self.node_mut(new_parent) {
            let pos = position.min(parent.children.len());
            parent.children.insert(pos, id);
        }
        if let Some(node) = self.node_mut(id) {
            node.parent = Some(new_parent);
        }
        true
    }

    fn collect_descendants(&self, id: NodeId, out: &mut Vec<NodeId>) {
        if let Some(node) = self.node(id) {
            for child in &node.children {
                self.collect_descendants(*child, out);
                out.push(*child);
            }
        }
    }

    /// Sector size inherited by `id` from the nearest World ancestor.
    pub fn world_sector_size_for_node(&self, id: NodeId) -> Option<i32> {
        let mut current = Some(id);
        while let Some(node_id) = current {
            let node = self.node(node_id)?;
            if let NodeKind::World { sector_size, .. } = &node.kind {
                return Some(snap_world_sector_size(*sector_size));
            }
            current = node.parent;
        }
        None
    }

    /// Sky settings inherited by `id` from the nearest World ancestor.
    pub fn world_sky_for_node(&self, id: NodeId) -> Option<SkySettings> {
        let mut current = Some(id);
        while let Some(node_id) = current {
            let node = self.node(node_id)?;
            if let NodeKind::World { sky, .. } = &node.kind {
                return Some(*sky);
            }
            current = node.parent;
        }
        None
    }

    /// Far-vista settings inherited by `id` from the nearest World ancestor.
    pub fn world_far_vista_for_node(&self, id: NodeId) -> Option<FarVistaSettings> {
        let mut current = Some(id);
        while let Some(node_id) = current {
            let node = self.node(node_id)?;
            if let NodeKind::World { far_vista, .. } = &node.kind {
                return Some(*far_vista);
            }
            current = node.parent;
        }
        None
    }

    /// Third-person camera settings inherited by `id` from the nearest World ancestor.
    pub fn world_camera_for_node(&self, id: NodeId) -> Option<WorldCameraSettings> {
        let mut current = Some(id);
        while let Some(node_id) = current {
            let node = self.node(node_id)?;
            if let NodeKind::World { camera, .. } = &node.kind {
                return Some(camera.normalized());
            }
            current = node.parent;
        }
        None
    }

    /// Runtime culling settings inherited by `id` from the nearest World ancestor.
    pub fn world_culling_for_node(&self, id: NodeId) -> Option<WorldCullingSettings> {
        let mut current = Some(id);
        while let Some(node_id) = current {
            let node = self.node(node_id)?;
            if let NodeKind::World { culling, .. } = &node.kind {
                return Some(culling.normalized());
            }
            current = node.parent;
        }
        None
    }

    /// Streaming chunk settings inherited by `id` from the nearest World ancestor.
    pub fn world_streaming_for_node(&self, id: NodeId) -> Option<WorldStreamingSettings> {
        let mut current = Some(id);
        while let Some(node_id) = current {
            let node = self.node(node_id)?;
            if let NodeKind::World { streaming, .. } = &node.kind {
                return Some(streaming.normalized());
            }
            current = node.parent;
        }
        None
    }

    /// Rows in root-first depth-first order.
    pub fn hierarchy_rows(&self) -> Vec<NodeRow> {
        let mut rows = Vec::new();
        self.push_hierarchy_row(self.root, 0, &mut rows);
        rows
    }

    fn push_hierarchy_row(&self, id: NodeId, depth: usize, rows: &mut Vec<NodeRow>) {
        let Some(node) = self.node(id) else {
            return;
        };
        rows.push(NodeRow {
            id,
            parent: node.parent,
            depth,
            sibling_index: node
                .parent
                .and_then(|parent_id| self.node(parent_id))
                .and_then(|parent| parent.children.iter().position(|child| *child == id))
                .unwrap_or(0),
            name: node.name.clone(),
            kind: node.kind.label(),
            child_count: node.children.len(),
        });
        for child in &node.children {
            self.push_hierarchy_row(*child, depth + 1, rows);
        }
    }
}

/// Saved editor 3D camera mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum EditorCameraMode {
    /// Target/radius orbit camera.
    #[default]
    Orbit,
    /// Explicit-position fly camera.
    Free,
}

fn default_editor_camera_orbit_yaw_q12() -> u16 {
    256
}

fn default_editor_camera_orbit_pitch_q12() -> u16 {
    256
}

fn default_editor_camera_orbit_radius() -> i32 {
    6144
}

fn default_editor_camera_orbit_target() -> [i32; 3] {
    [0, 512, 0]
}

fn default_editor_camera_free_yaw_q12() -> u16 {
    default_editor_camera_orbit_yaw_q12()
}

fn default_editor_camera_free_pitch_q12() -> u16 {
    default_editor_camera_orbit_pitch_q12()
}

/// Editor-only 3D viewport camera state persisted with a project.
///
/// This is intentionally authoring metadata: cook/playtest paths
/// should not use it for runtime camera behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditorCameraState {
    #[serde(default)]
    pub mode: EditorCameraMode,
    #[serde(default = "default_editor_camera_orbit_yaw_q12")]
    pub orbit_yaw_q12: u16,
    #[serde(default = "default_editor_camera_orbit_pitch_q12")]
    pub orbit_pitch_q12: u16,
    #[serde(default = "default_editor_camera_orbit_radius")]
    pub orbit_radius: i32,
    #[serde(default = "default_editor_camera_orbit_target")]
    pub orbit_target: [i32; 3],
    #[serde(default = "default_editor_camera_free_yaw_q12")]
    pub free_yaw_q12: u16,
    #[serde(default = "default_editor_camera_free_pitch_q12")]
    pub free_pitch_q12: u16,
    #[serde(default)]
    pub free_position: [i32; 3],
    #[serde(default)]
    pub free_initialized: bool,
}

impl Default for EditorCameraState {
    fn default() -> Self {
        Self {
            mode: EditorCameraMode::Orbit,
            orbit_yaw_q12: default_editor_camera_orbit_yaw_q12(),
            orbit_pitch_q12: default_editor_camera_orbit_pitch_q12(),
            orbit_radius: default_editor_camera_orbit_radius(),
            orbit_target: default_editor_camera_orbit_target(),
            free_yaw_q12: default_editor_camera_free_yaw_q12(),
            free_pitch_q12: default_editor_camera_free_pitch_q12(),
            free_position: [0, 0, 0],
            free_initialized: false,
        }
    }
}

impl EditorCameraState {
    pub fn normalize(&mut self) {
        self.orbit_pitch_q12 = clamp_q12_pitch(self.orbit_pitch_q12);
        self.free_pitch_q12 = clamp_q12_pitch(self.free_pitch_q12);
        self.orbit_radius = self.orbit_radius.clamp(512, 262_144);
    }
}

fn clamp_q12_pitch(value: u16) -> u16 {
    let raw = (value & 0x0fff) as i32;
    let signed = if raw >= 2048 { raw - 4096 } else { raw };
    signed.clamp(-960, 960).rem_euclid(4096) as u16
}

/// Editor-only visibility preferences persisted with a project.
///
/// These fields affect authoring and debug overlays only; cooked
/// runtime output must not depend on them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditorVisibilityState {
    #[serde(default = "default_true")]
    pub show_grid: bool,
    #[serde(default = "default_true")]
    pub show_portals: bool,
    #[serde(default = "default_true")]
    pub show_lights: bool,
    #[serde(default = "default_true")]
    pub preview_fog: bool,
    #[serde(default = "default_true")]
    pub preview_backface_wireframe: bool,
    #[serde(default = "default_true")]
    pub preview_bounds: bool,
    #[serde(default = "default_true")]
    pub show_play_debug_overlays: bool,
    #[serde(default = "default_true")]
    pub show_play_debug_map: bool,
}

impl Default for EditorVisibilityState {
    fn default() -> Self {
        Self {
            show_grid: true,
            show_portals: true,
            show_lights: true,
            preview_fog: true,
            preview_backface_wireframe: true,
            preview_bounds: true,
            show_play_debug_overlays: true,
            show_play_debug_map: true,
        }
    }
}

/// Runtime depth sorting policy for cooked cached room geometry.
///
/// This affects embedded play and generated runtime manifests. The editor
/// preview remains the reference view, but the PS1 path needs explicit
/// tradeoffs between stable ordering and per-triangle work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeDepthSortMode {
    /// Use the legacy fixed cell depth key for every cached surface.
    FixedCell,
    /// Use per-triangle depth for sloped/high-span horizontal surfaces.
    Hybrid,
    /// Use per-triangle projected depth for every cached surface.
    PerTriangle,
}

impl RuntimeDepthSortMode {
    pub const ALL: [Self; 3] = [Self::Hybrid, Self::PerTriangle, Self::FixedCell];

    pub const fn label(self) -> &'static str {
        match self {
            Self::FixedCell => "Fixed cell",
            Self::Hybrid => "Hybrid",
            Self::PerTriangle => "Per triangle",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::FixedCell => "Fast legacy ordering. Can show overlap errors on ramps.",
            Self::Hybrid => "Uses per-triangle depth only where sloped floors need it.",
            Self::PerTriangle => "Most precise cached-room ordering. Costs more sort work.",
        }
    }

    pub const fn manifest_value(self) -> u8 {
        match self {
            Self::FixedCell => 0,
            Self::Hybrid => 1,
            Self::PerTriangle => 2,
        }
    }
}

impl Default for RuntimeDepthSortMode {
    fn default() -> Self {
        Self::Hybrid
    }
}

/// Default projected edge threshold for runtime room subdivision.
///
/// `0` disables visual subdivision and keeps splitting limited to PS1
/// hardware packet bounds. Lower positive values split more aggressively.
pub const DEFAULT_RUNTIME_TEXTURE_SPLIT_MAX_EDGE: u16 = 128;

const fn default_runtime_texture_split_max_edge() -> u16 {
    DEFAULT_RUNTIME_TEXTURE_SPLIT_MAX_EDGE
}

/// One editor project document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectDocument {
    /// Display name.
    pub name: String,
    /// Editor-only viewport camera state.
    #[serde(default)]
    pub editor_camera: EditorCameraState,
    /// Editor-only overlay visibility preferences.
    #[serde(default)]
    pub editor_visibility: EditorVisibilityState,
    /// Cooked playtest cached-room depth sorting mode.
    #[serde(default)]
    pub runtime_depth_sort_mode: RuntimeDepthSortMode,
    /// Projected edge threshold used to subdivide textured runtime room surfaces.
    #[serde(default = "default_runtime_texture_split_max_edge")]
    pub runtime_texture_split_max_edge: u16,
    /// Open scenes. The first scene is the active scene for now.
    pub scenes: Vec<Scene>,
    /// Project resources.
    pub resources: Vec<Resource>,
    next_resource_id: u64,
}

impl ProjectDocument {
    /// Create an empty project with one scene.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            editor_camera: EditorCameraState::default(),
            editor_visibility: EditorVisibilityState::default(),
            runtime_depth_sort_mode: RuntimeDepthSortMode::default(),
            runtime_texture_split_max_edge: DEFAULT_RUNTIME_TEXTURE_SPLIT_MAX_EDGE,
            scenes: vec![Scene::new("Main")],
            resources: Vec::new(),
            next_resource_id: 1,
        }
    }

    /// Deserialize the default project shipped at
    /// `editor/projects/default/project.ron`. The on-disk RON file
    /// is the single source of truth -- the editor reads the exact
    /// same bytes a `cargo run` would, so changes to the default
    /// project are git-trackable and don't require a rebuild.
    ///
    /// Panics only if the embedded RON drifts out of sync with the
    /// `ProjectDocument` schema; the `embedded_default_project_ron_deserializes`
    /// test guards the build-time invariant.
    pub fn starter() -> Self {
        Self::from_ron_str(DEFAULT_PROJECT_RON)
            .expect("editor/projects/default/project.ron is malformed")
    }

    /// Active scene.
    pub fn active_scene(&self) -> &Scene {
        &self.scenes[0]
    }

    /// Active scene, mutable.
    pub fn active_scene_mut(&mut self) -> &mut Scene {
        &mut self.scenes[0]
    }

    /// Add a resource and return its id.
    pub fn add_resource(&mut self, name: impl Into<String>, data: ResourceData) -> ResourceId {
        let id = ResourceId(self.next_resource_id);
        self.next_resource_id = self.next_resource_id.saturating_add(1);
        self.resources.push(Resource {
            id,
            name: name.into(),
            data,
        });
        id
    }

    /// Get a resource.
    pub fn resource(&self, id: ResourceId) -> Option<&Resource> {
        self.resources.iter().find(|resource| resource.id == id)
    }

    /// Get a mutable resource.
    pub fn resource_mut(&mut self, id: ResourceId) -> Option<&mut Resource> {
        self.resources.iter_mut().find(|resource| resource.id == id)
    }

    /// Return a resource display name.
    pub fn resource_name(&self, id: ResourceId) -> Option<&str> {
        self.resource(id).map(|resource| resource.name.as_str())
    }

    /// Resolve every animation a model can play. Legacy model-local
    /// clips are listed first so existing clip indices remain stable;
    /// target-specific cooked clips are preferred over generic
    /// skeleton-shared clips, de-duplicated by path.
    pub fn resolved_model_animation_clips(
        &self,
        model_id: ResourceId,
    ) -> Vec<ResolvedModelAnimationClip> {
        let Some(model) = self
            .resource(model_id)
            .and_then(|resource| match &resource.data {
                ResourceData::Model(model) => Some(model),
                _ => None,
            })
        else {
            return Vec::new();
        };

        let mut out = Vec::new();
        let mut seen_paths = HashSet::new();
        for (model_clip_index, clip) in model.clips.iter().enumerate() {
            if seen_paths.insert(clip.psxanim_path.clone()) {
                out.push(ResolvedModelAnimationClip {
                    name: clip.name.clone(),
                    psxanim_path: clip.psxanim_path.clone(),
                    animation_resource: None,
                    model_clip_index: Some(model_clip_index),
                    calibration: clip.calibration,
                });
            }
        }

        for target_required in [true, false] {
            for resource in &self.resources {
                let ResourceData::AnimationClip(clip) = &resource.data else {
                    continue;
                };
                let is_target_clip = clip.target_model == Some(model_id);
                if is_target_clip != target_required {
                    continue;
                }
                if model.skeleton.is_none() || clip.skeleton != model.skeleton {
                    continue;
                }
                if clip.target_model.is_some_and(|target| target != model_id) {
                    continue;
                }
                if seen_paths.insert(clip.psxanim_path.clone()) {
                    out.push(ResolvedModelAnimationClip {
                        name: resource.name.clone(),
                        psxanim_path: clip.psxanim_path.clone(),
                        animation_resource: Some(resource.id),
                        model_clip_index: None,
                        calibration: clip.calibration,
                    });
                }
            }
        }
        out
    }

    /// Resolve the model-local runtime index for a standalone
    /// animation resource after [`Self::resolved_model_animation_clips`]
    /// has appended compatible library clips.
    pub fn resolved_model_animation_index(
        &self,
        model_id: ResourceId,
        animation_id: ResourceId,
    ) -> Option<u16> {
        let resolved = self.resolved_model_animation_clips(model_id);
        if let Some(index) = resolved
            .iter()
            .position(|clip| clip.animation_resource == Some(animation_id))
        {
            return u16::try_from(index).ok();
        }

        let animation_path =
            self.resource(animation_id)
                .and_then(|resource| match &resource.data {
                    ResourceData::AnimationClip(clip) => Some(clip.psxanim_path.as_str()),
                    _ => None,
                })?;
        resolved
            .iter()
            .position(|clip| clip.psxanim_path == animation_path)
            .and_then(|index| u16::try_from(index).ok())
    }

    /// Count project references to `id` from scenes and from other
    /// resources. Backing-file paths are counted separately by the
    /// delete plan because they are owned by the resource itself.
    pub fn resource_reference_count(&self, id: ResourceId) -> usize {
        let mut count = 0;
        for resource in &self.resources {
            count += resource_data_reference_count(&resource.data, id);
        }
        for scene in &self.scenes {
            for node in scene.nodes() {
                count += node_kind_reference_count(&node.kind, id);
            }
        }
        count
    }

    /// Remove a resource from the project and clear references to it.
    pub fn delete_resource(&mut self, id: ResourceId) -> Option<ResourceDeleteReport> {
        let index = self
            .resources
            .iter()
            .position(|resource| resource.id == id)?;
        let removed = self.resources.remove(index);
        let cleared_references = self.clear_resource_references(id);
        Some(ResourceDeleteReport {
            removed,
            cleared_references,
            deleted_files: Vec::new(),
            skipped_files: Vec::new(),
        })
    }

    /// Remove a resource, delete its project-owned backing files, and
    /// clear references to it.
    ///
    /// Files are removed before project data is mutated. Only files
    /// that currently exist under `project_root` are deleted; missing
    /// or external paths are skipped and reported.
    pub fn delete_resource_with_files(
        &mut self,
        id: ResourceId,
        project_root: &Path,
    ) -> Result<ResourceDeleteReport, ResourceDeleteError> {
        let Some(index) = self.resources.iter().position(|resource| resource.id == id) else {
            return Err(ResourceDeleteError::MissingResource(id));
        };
        let plan = plan_resource_file_deletes(&self.resources[index], project_root);
        execute_resource_delete_plan(&plan, project_root)?;

        let mut report = self
            .delete_resource(id)
            .ok_or(ResourceDeleteError::MissingResource(id))?;
        report.deleted_files = plan
            .files
            .iter()
            .map(|op| ResourceFileDelete {
                path: op.stored.clone(),
            })
            .collect();
        report.skipped_files = plan.skipped;
        Ok(report)
    }

    /// Rename a resource and any project-owned backing files whose
    /// names are derived from the resource name.
    ///
    /// File moves are preflighted before project data is mutated:
    /// destinations must not already exist and duplicate destinations
    /// are refused. Only files that already exist under `project_root`
    /// are moved; missing paths and external absolute paths are
    /// preserved and reported as skipped.
    pub fn rename_resource_with_files(
        &mut self,
        id: ResourceId,
        new_name: &str,
        project_root: &Path,
    ) -> Result<ResourceRenameReport, ResourceRenameError> {
        let final_name = new_name.trim();
        if final_name.is_empty() {
            return Err(ResourceRenameError::EmptyName);
        }

        let Some(index) = self.resources.iter().position(|resource| resource.id == id) else {
            return Err(ResourceRenameError::MissingResource(id));
        };

        let resource = self.resources[index].clone();
        let safe_stem = resource_file_stem(final_name, resource_default_stem(&resource.data));
        let mut plan = ResourceRenamePlan::default();
        let mut data = resource.data.clone();

        match &mut data {
            ResourceData::Texture { psxt_path } => {
                plan_path_rename(psxt_path, &safe_stem, "psxt", project_root, &mut plan);
            }
            ResourceData::Model(model) => {
                plan_model_resource_rename(model, &safe_stem, project_root, &mut plan);
            }
            ResourceData::AnimationSource(source) => {
                let fallback_ext = resource_default_extension(&resource.data);
                plan_path_rename(
                    &mut source.source_path,
                    &safe_stem,
                    fallback_ext,
                    project_root,
                    &mut plan,
                );
            }
            ResourceData::AnimationClip(clip) => {
                plan_path_rename(
                    &mut clip.psxanim_path,
                    &safe_stem,
                    "psxanim",
                    project_root,
                    &mut plan,
                );
            }
            ResourceData::Mesh { source_path }
            | ResourceData::Scene { source_path }
            | ResourceData::Script { source_path }
            | ResourceData::Audio { source_path } => {
                let fallback_ext = resource_default_extension(&resource.data);
                plan_path_rename(
                    source_path,
                    &safe_stem,
                    fallback_ext,
                    project_root,
                    &mut plan,
                );
            }
            ResourceData::Material(_)
            | ResourceData::Skeleton(_)
            | ResourceData::AnimationSet(_)
            | ResourceData::Character(_)
            | ResourceData::Weapon(_) => {}
        }

        execute_resource_rename_plan(&plan)?;

        self.resources[index].name = final_name.to_string();
        self.resources[index].data = data;

        Ok(ResourceRenameReport {
            renamed_files: plan
                .ops
                .iter()
                .map(|op| ResourceFileRename {
                    from: op.from_stored.clone(),
                    to: op.to_stored.clone(),
                })
                .collect(),
            skipped_files: plan.skipped,
        })
    }

    fn clear_resource_references(&mut self, id: ResourceId) -> usize {
        let mut count = 0;
        for resource in &mut self.resources {
            count += clear_resource_data_references(&mut resource.data, id);
        }
        for scene in &mut self.scenes {
            for node in &mut scene.nodes {
                count += clear_node_kind_references(&mut node.kind, id);
            }
        }
        count
    }

    /// Material resources as `(id, name)` pairs for inspector combo boxes.
    pub fn material_options(&self) -> Vec<(ResourceId, String)> {
        self.resources
            .iter()
            .filter_map(|resource| match &resource.data {
                ResourceData::Material(_) => Some((resource.id, resource.name.clone())),
                _ => None,
            })
            .collect()
    }

    /// Serialize this project to human-readable RON.
    pub fn to_ron_string(&self) -> Result<String, ProjectIoError> {
        let config = PrettyConfig::new()
            .depth_limit(4)
            .separate_tuple_members(true)
            .enumerate_arrays(true);
        ron::ser::to_string_pretty(self, config).map_err(ProjectIoError::Serialize)
    }

    /// Deserialize a project from RON.
    pub fn from_ron_str(source: &str) -> Result<Self, ProjectIoError> {
        let mut project: Self = match ron::from_str(source) {
            Ok(project) => project,
            Err(first_error) => {
                let migrated = migrate_legacy_project_ron(source);
                if migrated == source {
                    return Err(ProjectIoError::Parse(first_error));
                }
                ron::from_str(&migrated).map_err(ProjectIoError::Parse)?
            }
        };
        project.normalize_loaded();
        Ok(project)
    }

    /// Save this project to a RON file, creating parent directories.
    pub fn save_to_path(&self, path: impl AsRef<Path>) -> Result<(), ProjectIoError> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut normalized = self.clone();
        normalized.normalize_loaded();
        std::fs::write(path, normalized.to_ron_string()?)?;
        Ok(())
    }

    /// Load a project from a RON file.
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, ProjectIoError> {
        let source = std::fs::read_to_string(path)?;
        Self::from_ron_str(&source)
    }

    /// Normalize legacy or hand-authored project data after load.
    pub fn normalize_loaded(&mut self) {
        self.editor_camera.normalize();
        for scene in &mut self.scenes {
            scene.normalize_world_root();
            for node in &mut scene.nodes {
                match &mut node.kind {
                    NodeKind::World {
                        sector_size,
                        sky,
                        far_vista,
                        camera,
                        culling,
                        streaming,
                    } => {
                        *sector_size = snap_world_sector_size(*sector_size);
                        sky.horizon_percent = sky.horizon_percent.clamp(5, 95);
                        sky.horizon_thickness_percent = sky.horizon_thickness_percent.clamp(0, 80);
                        sky.horizon_glow_percent = sky.horizon_glow_percent.clamp(0, 100);
                        sky.horizon_glow_yaw_degrees =
                            sky.horizon_glow_yaw_degrees.clamp(-180, 180);
                        sky.sun_yaw_degrees = sky.sun_yaw_degrees.clamp(-180, 180);
                        sky.sun_pitch_degrees = sky.sun_pitch_degrees.clamp(-30, 75);
                        sky.sun_size_percent = sky.sun_size_percent.clamp(1, 100);
                        sky.sun_glow_percent = sky.sun_glow_percent.clamp(0, 100);
                        sky.sun_glow_size_percent = sky.sun_glow_size_percent.clamp(0, 100);
                        sky.mountain_height_percent = sky.mountain_height_percent.clamp(0, 100);
                        sky.mountain_gap_percent = sky.mountain_gap_percent.clamp(0, 100);
                        sky.mountain_roughness_percent =
                            sky.mountain_roughness_percent.clamp(0, 100);
                        sky.mountain_layer_count = sky.mountain_layer_count.clamp(1, 3);
                        sky.cloud_layer.tile_count = sky.cloud_layer.tile_count.clamp(1, 16);
                        sky.skybox_columns = sky
                            .skybox_columns
                            .clamp(SKYBOX_COLUMNS_MIN, SKYBOX_COLUMNS_MAX);
                        sky.skybox_rows = sky.skybox_rows.clamp(SKYBOX_ROWS_MIN, SKYBOX_ROWS_MAX);
                        far_vista.radius = far_vista.radius.clamp(1_024, 65_535);
                        far_vista.height = far_vista.height.clamp(128, 32_768);
                        far_vista.vertical_offset =
                            far_vista.vertical_offset.clamp(-32_768, 32_768);
                        far_vista.segments = far_vista.segments.clamp(3, 16);
                        *camera = camera.normalized();
                        *culling = culling.normalized();
                        *streaming = streaming.normalized();
                    }
                    _ => {}
                }
            }
            let worlds: Vec<(NodeId, i32)> = scene
                .nodes()
                .iter()
                .filter_map(|node| match &node.kind {
                    NodeKind::World { sector_size, .. } => Some((node.id, *sector_size)),
                    _ => None,
                })
                .collect();
            for (world_id, sector_size) in worlds {
                apply_world_sector_size_to_descendants(
                    scene,
                    world_id,
                    sector_size,
                    sector_size,
                    false,
                );
            }
            let orphan_rooms: Vec<NodeId> = scene
                .nodes()
                .iter()
                .filter(|node| matches!(node.kind, NodeKind::Room { .. }))
                .filter(|node| scene.world_sector_size_for_node(node.id).is_none())
                .map(|node| node.id)
                .collect();
            for room_id in orphan_rooms {
                if let Some(node) = scene.node_mut(room_id) {
                    if let NodeKind::Room { grid } = &mut node.kind {
                        grid.rescale_sector_size(grid.sector_size);
                    }
                }
            }
        }
    }

    /// Sector size inherited by `node_id` from its nearest World
    /// ancestor, or the default when no World exists.
    pub fn world_sector_size_for_node(&self, node_id: NodeId) -> i32 {
        self.active_scene()
            .world_sector_size_for_node(node_id)
            .unwrap_or(DEFAULT_WORLD_SECTOR_SIZE)
    }

    /// Update a World node's sector size, snapping to 128-unit
    /// increments and rescaling descendant rooms/components.
    pub fn set_world_sector_size(&mut self, world_id: NodeId, requested: i32) -> Option<i32> {
        let scene = self.active_scene_mut();
        let new_size = snap_world_sector_size(requested);
        let old_size = {
            let world = scene.node_mut(world_id)?;
            let NodeKind::World { sector_size, .. } = &mut world.kind else {
                return None;
            };
            let old_size = snap_world_sector_size(*sector_size);
            *sector_size = new_size;
            old_size
        };
        apply_world_sector_size_to_descendants(
            scene,
            world_id,
            new_size,
            old_size,
            old_size != new_size,
        );
        Some(new_size)
    }
}

fn resource_data_reference_count(data: &ResourceData, id: ResourceId) -> usize {
    match data {
        ResourceData::Material(material) => option_resource_reference_count(material.texture, id),
        ResourceData::Model(model) => option_resource_reference_count(model.skeleton, id),
        ResourceData::AnimationSource(source) => {
            option_resource_reference_count(source.skeleton, id)
                + option_resource_reference_count(source.target_model, id)
        }
        ResourceData::AnimationClip(clip) => {
            option_resource_reference_count(clip.skeleton, id)
                + option_resource_reference_count(clip.source, id)
                + option_resource_reference_count(clip.target_model, id)
        }
        ResourceData::AnimationSet(set) => {
            option_resource_reference_count(set.skeleton, id)
                + option_resource_reference_count(set.idle_clip, id)
                + option_resource_reference_count(set.walk_clip, id)
                + option_resource_reference_count(set.run_clip, id)
                + option_resource_reference_count(set.turn_clip, id)
                + option_resource_reference_count(set.roll_clip, id)
                + option_resource_reference_count(set.backstep_clip, id)
                + set
                    .action_clips
                    .iter()
                    .filter(|binding| binding.clip == id)
                    .count()
                + set.clips.iter().filter(|clip_id| **clip_id == id).count()
        }
        ResourceData::Character(character) => {
            option_resource_reference_count(character.model, id)
                + option_resource_reference_count(character.animation_set, id)
        }
        ResourceData::Weapon(weapon) => option_resource_reference_count(weapon.model, id),
        ResourceData::Texture { .. }
        | ResourceData::Skeleton(_)
        | ResourceData::Mesh { .. }
        | ResourceData::Scene { .. }
        | ResourceData::Script { .. }
        | ResourceData::Audio { .. } => 0,
    }
}

fn clear_resource_data_references(data: &mut ResourceData, id: ResourceId) -> usize {
    match data {
        ResourceData::Material(material) => clear_option_resource(&mut material.texture, id),
        ResourceData::Model(model) => clear_option_resource(&mut model.skeleton, id),
        ResourceData::AnimationSource(source) => {
            clear_option_resource(&mut source.skeleton, id)
                + clear_option_resource(&mut source.target_model, id)
        }
        ResourceData::AnimationClip(clip) => {
            clear_option_resource(&mut clip.skeleton, id)
                + clear_option_resource(&mut clip.source, id)
                + clear_option_resource(&mut clip.target_model, id)
        }
        ResourceData::AnimationSet(set) => {
            let mut cleared = clear_option_resource(&mut set.skeleton, id)
                + clear_option_resource(&mut set.idle_clip, id)
                + clear_option_resource(&mut set.walk_clip, id)
                + clear_option_resource(&mut set.run_clip, id)
                + clear_option_resource(&mut set.turn_clip, id)
                + clear_option_resource(&mut set.roll_clip, id)
                + clear_option_resource(&mut set.backstep_clip, id);
            let before_actions = set.action_clips.len();
            set.action_clips.retain(|binding| binding.clip != id);
            cleared += before_actions - set.action_clips.len();
            let before = set.clips.len();
            set.clips.retain(|clip_id| *clip_id != id);
            cleared += before - set.clips.len();
            cleared
        }
        ResourceData::Character(character) => {
            let cleared_model = clear_option_resource(&mut character.model, id);
            let cleared_set = clear_option_resource(&mut character.animation_set, id);
            if cleared_model > 0 {
                character.idle_clip = None;
                character.walk_clip = None;
                character.run_clip = None;
                character.turn_clip = None;
                character.roll_clip = None;
                character.backstep_clip = None;
                character.action_clips.clear();
            }
            cleared_model + cleared_set
        }
        ResourceData::Weapon(weapon) => clear_option_resource(&mut weapon.model, id),
        ResourceData::Texture { .. }
        | ResourceData::Skeleton(_)
        | ResourceData::Mesh { .. }
        | ResourceData::Scene { .. }
        | ResourceData::Script { .. }
        | ResourceData::Audio { .. } => 0,
    }
}

fn node_kind_reference_count(kind: &NodeKind, id: ResourceId) -> usize {
    match kind {
        NodeKind::Room { grid } => grid_resource_reference_count(grid, id),
        NodeKind::MeshInstance { mesh, material, .. } => {
            option_resource_reference_count(*mesh, id)
                + option_resource_reference_count(*material, id)
        }
        NodeKind::ImageProp { material, .. } => option_resource_reference_count(*material, id),
        NodeKind::BoxProp { materials, .. } => materials
            .iter()
            .filter(|material| **material == Some(id))
            .count(),
        NodeKind::ModelRenderer {
            model, material, ..
        } => {
            option_resource_reference_count(*model, id)
                + option_resource_reference_count(*material, id)
        }
        NodeKind::CharacterController { character, .. } => {
            option_resource_reference_count(*character, id)
        }
        NodeKind::Equipment { weapon, .. } => option_resource_reference_count(*weapon, id),
        NodeKind::SpawnPoint { character, .. } => option_resource_reference_count(*character, id),
        NodeKind::AudioSource { sound, .. } => option_resource_reference_count(*sound, id),
        NodeKind::World { far_vista, .. } => far_vista_resource_reference_count(far_vista, id),
        NodeKind::Node
        | NodeKind::Node3D
        | NodeKind::Entity
        | NodeKind::Animator { .. }
        | NodeKind::Collider { .. }
        | NodeKind::Interactable { .. }
        | NodeKind::AiController { .. }
        | NodeKind::Combat { .. }
        | NodeKind::PointLight { .. }
        | NodeKind::Trigger { .. }
        | NodeKind::Portal { .. } => 0,
    }
}

fn far_vista_resource_reference_count(far_vista: &FarVistaSettings, id: ResourceId) -> usize {
    option_resource_reference_count(far_vista.texture, id)
        + far_vista
            .texture_panels
            .iter()
            .filter(|panel| **panel == Some(id))
            .count()
}

fn clear_far_vista_resource_references(far_vista: &mut FarVistaSettings, id: ResourceId) -> usize {
    let mut cleared = clear_option_resource(&mut far_vista.texture, id);
    for panel in &mut far_vista.texture_panels {
        cleared += clear_option_resource(panel, id);
    }
    cleared
}

fn clear_node_kind_references(kind: &mut NodeKind, id: ResourceId) -> usize {
    match kind {
        NodeKind::Room { grid } => clear_grid_resource_references(grid, id),
        NodeKind::MeshInstance { mesh, material, .. } => {
            clear_option_resource(mesh, id) + clear_option_resource(material, id)
        }
        NodeKind::ImageProp { material, .. } => clear_option_resource(material, id),
        NodeKind::BoxProp { materials, .. } => {
            let mut cleared = 0;
            for material in materials {
                cleared += clear_option_resource(material, id);
            }
            cleared
        }
        NodeKind::ModelRenderer {
            model, material, ..
        } => clear_option_resource(model, id) + clear_option_resource(material, id),
        NodeKind::CharacterController { character, .. } => clear_option_resource(character, id),
        NodeKind::Equipment { weapon, .. } => clear_option_resource(weapon, id),
        NodeKind::SpawnPoint { character, .. } => clear_option_resource(character, id),
        NodeKind::AudioSource { sound, .. } => clear_option_resource(sound, id),
        NodeKind::World { far_vista, .. } => clear_far_vista_resource_references(far_vista, id),
        NodeKind::Node
        | NodeKind::Node3D
        | NodeKind::Entity
        | NodeKind::Animator { .. }
        | NodeKind::Collider { .. }
        | NodeKind::Interactable { .. }
        | NodeKind::AiController { .. }
        | NodeKind::Combat { .. }
        | NodeKind::PointLight { .. }
        | NodeKind::Trigger { .. }
        | NodeKind::Portal { .. } => 0,
    }
}

fn grid_resource_reference_count(grid: &WorldGrid, id: ResourceId) -> usize {
    let mut count = 0;
    for sector in grid.sectors.iter().flatten() {
        if let Some(face) = &sector.floor {
            count += option_resource_reference_count(face.material, id);
        }
        if let Some(face) = &sector.ceiling {
            count += option_resource_reference_count(face.material, id);
        }
        for direction in GridDirection::ALL {
            for wall in sector.walls.get(direction) {
                count += option_resource_reference_count(wall.material, id);
            }
        }
    }
    count
}

fn clear_grid_resource_references(grid: &mut WorldGrid, id: ResourceId) -> usize {
    let mut count = 0;
    for sector in grid.sectors.iter_mut().flatten() {
        if let Some(face) = &mut sector.floor {
            count += clear_option_resource(&mut face.material, id);
        }
        if let Some(face) = &mut sector.ceiling {
            count += clear_option_resource(&mut face.material, id);
        }
        for direction in GridDirection::ALL {
            for wall in sector.walls.get_mut(direction) {
                count += clear_option_resource(&mut wall.material, id);
            }
        }
    }
    count
}

fn option_resource_reference_count(value: Option<ResourceId>, id: ResourceId) -> usize {
    usize::from(value == Some(id))
}

fn clear_option_resource(value: &mut Option<ResourceId>, id: ResourceId) -> usize {
    if *value == Some(id) {
        *value = None;
        1
    } else {
        0
    }
}

fn migrate_legacy_project_ron(source: &str) -> String {
    source
        .replace(
            "kind: World,",
            &format!("kind: World(sector_size: {}),", DEFAULT_WORLD_SECTOR_SIZE),
        )
        .replace("kind: Actor,", "kind: Entity,")
}

fn apply_world_sector_size_to_descendants(
    scene: &mut Scene,
    world_id: NodeId,
    sector_size: i32,
    old_sector_size: i32,
    rescale: bool,
) {
    let ids: Vec<NodeId> = scene
        .nodes()
        .iter()
        .filter(|node| scene.is_descendant_of(node.id, world_id))
        .map(|node| node.id)
        .collect();
    for id in ids {
        let Some(node) = scene.node_mut(id) else {
            continue;
        };
        match &mut node.kind {
            NodeKind::Room { grid } => {
                if rescale {
                    grid.rescale_sector_size(sector_size);
                } else {
                    grid.sector_size = snap_world_sector_size(sector_size);
                    grid.snap_heights_to_quantum();
                }
            }
            NodeKind::Collider { shape, .. } if rescale => {
                rescale_collider_shape(shape, old_sector_size, sector_size);
            }
            _ => {}
        }
    }
}

fn rescale_collider_shape(shape: &mut ColliderShape, from: i32, to: i32) {
    match shape {
        ColliderShape::Box { half_extents } => {
            for axis in half_extents {
                *axis = scale_u16_ratio(*axis, from, to);
            }
        }
        ColliderShape::Sphere { radius } => {
            *radius = scale_u16_ratio(*radius, from, to);
        }
        ColliderShape::Capsule { radius, height } => {
            *radius = scale_u16_ratio(*radius, from, to);
            *height = scale_u16_ratio(*height, from, to);
        }
    }
}

#[derive(Default)]
struct ResourceRenamePlan {
    ops: Vec<ResourcePathRename>,
    skipped: Vec<String>,
}

#[derive(Default)]
struct ResourceDeletePlan {
    files: Vec<ResourcePathDelete>,
    skipped: Vec<String>,
}

struct ResourcePathRename {
    from_abs: PathBuf,
    to_abs: PathBuf,
    from_stored: String,
    to_stored: String,
}

struct ResourcePathDelete {
    abs: PathBuf,
    stored: String,
}

fn plan_resource_file_deletes(resource: &Resource, project_root: &Path) -> ResourceDeletePlan {
    let mut plan = ResourceDeletePlan::default();
    match &resource.data {
        ResourceData::Texture { psxt_path } => {
            plan_path_delete(psxt_path, project_root, &mut plan);
        }
        ResourceData::Model(model) => {
            plan_path_delete(&model.model_path, project_root, &mut plan);
            if let Some(texture_path) = &model.texture_path {
                plan_path_delete(texture_path, project_root, &mut plan);
            }
            for clip in &model.clips {
                plan_path_delete(&clip.psxanim_path, project_root, &mut plan);
            }
        }
        ResourceData::AnimationClip(clip) => {
            plan_path_delete(&clip.psxanim_path, project_root, &mut plan);
        }
        ResourceData::AnimationSource(source) => {
            plan_path_delete(&source.source_path, project_root, &mut plan);
        }
        ResourceData::Mesh { source_path }
        | ResourceData::Scene { source_path }
        | ResourceData::Script { source_path }
        | ResourceData::Audio { source_path } => {
            plan_path_delete(source_path, project_root, &mut plan);
        }
        ResourceData::Material(_)
        | ResourceData::Skeleton(_)
        | ResourceData::AnimationSet(_)
        | ResourceData::Character(_)
        | ResourceData::Weapon(_) => {}
    }
    plan
}

fn plan_path_delete(stored: &str, project_root: &Path, plan: &mut ResourceDeletePlan) {
    let trimmed = stored.trim();
    if trimmed.is_empty() {
        return;
    }

    let abs = model_import::resolve_path(trimmed, Some(project_root));
    if !abs.is_file() {
        plan.skipped.push(trimmed.to_string());
        return;
    }
    if !path_is_project_owned(&abs, project_root) {
        plan.skipped.push(trimmed.to_string());
        return;
    }
    if plan.files.iter().any(|op| op.abs == abs) {
        return;
    }
    plan.files.push(ResourcePathDelete {
        stored: relativise_resource_path(&abs, project_root),
        abs,
    });
}

fn execute_resource_delete_plan(
    plan: &ResourceDeletePlan,
    project_root: &Path,
) -> Result<(), ResourceDeleteError> {
    for op in &plan.files {
        std::fs::remove_file(&op.abs).map_err(|error| ResourceDeleteError::Io {
            path: op.abs.clone(),
            detail: error.to_string(),
        })?;
    }
    for op in &plan.files {
        remove_empty_project_parents(op.abs.parent(), project_root);
    }
    Ok(())
}

fn remove_empty_project_parents(mut dir: Option<&Path>, project_root: &Path) {
    while let Some(current) = dir {
        if current == project_root {
            break;
        }
        if std::fs::remove_dir(current).is_err() {
            break;
        }
        dir = current.parent();
    }
}

fn plan_path_rename(
    stored: &mut String,
    safe_stem: &str,
    fallback_ext: &str,
    project_root: &Path,
    plan: &mut ResourceRenamePlan,
) {
    let original = stored.clone();
    let Some(op) = build_path_rename(&original, safe_stem, fallback_ext, project_root, plan) else {
        return;
    };
    *stored = op.to_stored.clone();
    plan.ops.push(op);
}

fn plan_model_resource_rename(
    model: &mut ModelResource,
    safe_stem: &str,
    project_root: &Path,
    plan: &mut ResourceRenamePlan,
) {
    let model_path = model.model_path.clone();
    let model_abs = model_import::resolve_path(&model_path, Some(project_root));
    let model_dir = model_abs.parent().map(Path::to_path_buf);
    let target_dir = model_dir
        .as_deref()
        .map(|dir| model_bundle_target_dir(dir, safe_stem, project_root));

    if let Some(op) = build_path_rename_in_dir(
        &model_path,
        safe_stem,
        "psxmdl",
        target_dir.as_deref(),
        project_root,
        plan,
    ) {
        model.model_path = op.to_stored.clone();
        plan.ops.push(op);
    }

    if let Some(texture_path) = &mut model.texture_path {
        let original = texture_path.clone();
        if let Some(op) = build_path_rename_in_dir(
            &original,
            safe_stem,
            "psxt",
            target_dir.as_deref(),
            project_root,
            plan,
        ) {
            *texture_path = op.to_stored.clone();
            plan.ops.push(op);
        }
    }

    let mut seen_clip_stems = HashSet::new();
    for (index, clip) in model.clips.iter_mut().enumerate() {
        let clip_suffix = resource_file_stem(&clip.name, "clip");
        let mut clip_stem = format!("{safe_stem}_{clip_suffix}");
        if !seen_clip_stems.insert(clip_stem.clone()) {
            clip_stem = format!("{safe_stem}_{index}_{clip_suffix}");
            seen_clip_stems.insert(clip_stem.clone());
        }
        let original = clip.psxanim_path.clone();
        if let Some(op) = build_path_rename_in_dir(
            &original,
            &clip_stem,
            "psxanim",
            target_dir.as_deref(),
            project_root,
            plan,
        ) {
            clip.psxanim_path = op.to_stored.clone();
            plan.ops.push(op);
        }
    }
}

fn build_path_rename(
    stored: &str,
    safe_stem: &str,
    fallback_ext: &str,
    project_root: &Path,
    plan: &mut ResourceRenamePlan,
) -> Option<ResourcePathRename> {
    build_path_rename_in_dir(stored, safe_stem, fallback_ext, None, project_root, plan)
}

fn build_path_rename_in_dir(
    stored: &str,
    safe_stem: &str,
    fallback_ext: &str,
    target_dir: Option<&Path>,
    project_root: &Path,
    plan: &mut ResourceRenamePlan,
) -> Option<ResourcePathRename> {
    let trimmed = stored.trim();
    if trimmed.is_empty() {
        return None;
    }

    let from_abs = model_import::resolve_path(trimmed, Some(project_root));
    if !from_abs.is_file() {
        plan.skipped.push(trimmed.to_string());
        return None;
    }
    if !path_is_project_owned(&from_abs, project_root) {
        plan.skipped.push(trimmed.to_string());
        return None;
    }

    let ext = from_abs
        .extension()
        .and_then(|ext| ext.to_str())
        .filter(|ext| !ext.is_empty())
        .unwrap_or(fallback_ext);
    let target_name = format!("{safe_stem}.{ext}");
    let to_abs = target_dir
        .map(|dir| dir.join(&target_name))
        .unwrap_or_else(|| from_abs.with_file_name(target_name));

    if from_abs == to_abs {
        return None;
    }

    Some(ResourcePathRename {
        from_abs,
        to_stored: relativise_resource_path(&to_abs, project_root),
        to_abs,
        from_stored: trimmed.to_string(),
    })
}

fn execute_resource_rename_plan(plan: &ResourceRenamePlan) -> Result<(), ResourceRenameError> {
    let mut targets = HashSet::new();
    for op in &plan.ops {
        if !targets.insert(op.to_abs.clone()) {
            return Err(ResourceRenameError::DuplicateTarget(op.to_abs.clone()));
        }
        if op.to_abs.exists() {
            return Err(ResourceRenameError::TargetExists(op.to_abs.clone()));
        }
    }

    let mut moved: Vec<&ResourcePathRename> = Vec::new();
    let mut created_dirs = Vec::new();
    for op in &plan.ops {
        if let Some(parent) = op.to_abs.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent).map_err(|error| ResourceRenameError::Io {
                    from: op.from_abs.clone(),
                    to: op.to_abs.clone(),
                    detail: error.to_string(),
                })?;
                created_dirs.push(parent.to_path_buf());
            }
        }
        if let Err(error) = std::fs::rename(&op.from_abs, &op.to_abs) {
            for done in moved.iter().rev() {
                let _ = std::fs::rename(&done.to_abs, &done.from_abs);
            }
            for dir in created_dirs.iter().rev() {
                let _ = std::fs::remove_dir(dir);
            }
            return Err(ResourceRenameError::Io {
                from: op.from_abs.clone(),
                to: op.to_abs.clone(),
                detail: error.to_string(),
            });
        }
        moved.push(op);
    }

    for op in &plan.ops {
        if let (Some(from_parent), Some(to_parent)) = (op.from_abs.parent(), op.to_abs.parent()) {
            if from_parent != to_parent {
                let _ = std::fs::remove_dir(from_parent);
            }
        }
    }

    Ok(())
}

fn model_bundle_target_dir(model_dir: &Path, safe_stem: &str, project_root: &Path) -> PathBuf {
    let Ok(relative) = model_dir.strip_prefix(project_root) else {
        return model_dir.to_path_buf();
    };
    let mut components = relative.components();
    let is_imported_bundle = matches!(
        (
            components.next().and_then(|c| c.as_os_str().to_str()),
            components.next().and_then(|c| c.as_os_str().to_str()),
            components.next(),
            components.next()
        ),
        (Some("assets"), Some("models"), Some(_), None)
    );
    if is_imported_bundle {
        project_root.join("assets").join("models").join(safe_stem)
    } else {
        model_dir.to_path_buf()
    }
}

fn path_is_project_owned(path: &Path, project_root: &Path) -> bool {
    path.strip_prefix(project_root).is_ok()
}

fn relativise_resource_path(path: &Path, project_root: &Path) -> String {
    path.strip_prefix(project_root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn resource_file_stem(name: &str, fallback: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_was_sep = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_sep = false;
        } else if !last_was_sep {
            out.push('_');
            last_was_sep = true;
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed
    }
}

const fn resource_default_stem(data: &ResourceData) -> &'static str {
    match data {
        ResourceData::Texture { .. } => "texture",
        ResourceData::Material(_) => "material",
        ResourceData::Model(_) => "model",
        ResourceData::Skeleton(_) => "skeleton",
        ResourceData::AnimationSource(_) => "animation_source",
        ResourceData::AnimationClip(_) => "animation",
        ResourceData::AnimationSet(_) => "animation_set",
        ResourceData::Weapon(_) => "weapon",
        ResourceData::Mesh { .. } => "mesh",
        ResourceData::Scene { .. } => "room",
        ResourceData::Script { .. } => "script",
        ResourceData::Audio { .. } => "audio",
        ResourceData::Character(_) => "character",
    }
}

const fn resource_default_extension(data: &ResourceData) -> &'static str {
    match data {
        ResourceData::Texture { .. } => "psxt",
        ResourceData::Material(_) => "mat",
        ResourceData::Model(_) => "psxmdl",
        ResourceData::Skeleton(_) => "skeleton",
        ResourceData::AnimationSource(_) => "animsrc",
        ResourceData::AnimationClip(_) => "psxanim",
        ResourceData::AnimationSet(_) => "animset",
        ResourceData::Weapon(_) => "weapon",
        ResourceData::Mesh { .. } => "psxmesh",
        ResourceData::Scene { .. } => "room",
        ResourceData::Script { .. } => "script",
        ResourceData::Audio { .. } => "vag",
        ResourceData::Character(_) => "char",
    }
}

impl Default for ProjectDocument {
    fn default() -> Self {
        Self::starter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizontal_face_height_samples_editor_corner_convention() {
        let mut floor = GridHorizontalFace::flat(0, None);
        floor.heights = [100, 200, 300, 400];

        assert_eq!(floor.height_at_local(0, 1024, 1024), 100);
        assert_eq!(floor.height_at_local(1024, 1024, 1024), 200);
        assert_eq!(floor.height_at_local(1024, 0, 1024), 300);
        assert_eq!(floor.height_at_local(0, 0, 1024), 400);
    }

    #[test]
    fn grid_floor_height_handles_negative_origin_cells() {
        let mut grid = WorldGrid::empty(1, 1, 1024);
        grid.origin = [-1, -1];
        grid.set_floor(0, 0, 256, None);

        assert_eq!(grid.floor_height_at_room_local(-512, -512), Some(256));
        assert_eq!(grid.floor_height_at_room_local(0, 0), None);
    }

    #[test]
    fn snap_height_rounds_to_nearest_quantum() {
        assert_eq!(HEIGHT_QUANTUM, 64);
        // Exact multiples are unchanged (positive + negative).
        assert_eq!(snap_height(0), 0);
        assert_eq!(snap_height(64), 64);
        assert_eq!(snap_height(1024), 1024);
        assert_eq!(snap_height(-64), -64);
        assert_eq!(snap_height(-1024), -1024);
        // Below half-quantum rounds down toward zero.
        assert_eq!(snap_height(31), 0);
        assert_eq!(snap_height(-31), 0);
        // At half-quantum (32), away-from-zero on both sides.
        assert_eq!(snap_height(32), 64);
        assert_eq!(snap_height(-32), -64);
        // Above half-quantum rounds up away from zero.
        assert_eq!(snap_height(33), 64);
        assert_eq!(snap_height(-33), -64);
        // Past one quantum the same rule applies -- round to the
        // nearest multiple.
        assert_eq!(snap_height(95), 64);
        assert_eq!(snap_height(96), 128);
        assert_eq!(snap_height(-95), -64);
        assert_eq!(snap_height(-96), -128);
    }

    #[test]
    fn character_resource_deserializes_without_new_motor_tuning_fields() {
        let ron = r#"(
            model: None,
            animation_set: None,
            idle_clip: None,
            walk_clip: None,
            run_clip: None,
            turn_clip: None,
            radius: 192,
            height: 1024,
            walk_speed: 48,
            run_speed: 96,
            turn_speed_degrees_per_second: 180,
            camera_distance: 6144,
            camera_height: 1280,
            camera_target_height: 640,
        )"#;
        let character: CharacterResource =
            ron::from_str(ron).expect("legacy character resource deserializes");

        assert_eq!(
            character.stamina_max_q12,
            default_character_stamina_max_q12()
        );
        assert_eq!(character.roll_speed, default_character_roll_speed());
        assert_eq!(
            character.backstep_invulnerable_frames,
            default_character_backstep_invulnerable_frames()
        );
    }

    #[test]
    fn sky_settings_resolve_clamps_subdivision_defaults() {
        let default_sky = SkySettings::default().resolved_for_room(false, [0, 0, 0]);
        assert_eq!(default_sky.skybox_columns, SKYBOX_COLUMNS_DEFAULT);
        assert_eq!(default_sky.skybox_rows, SKYBOX_ROWS_DEFAULT);
        assert_eq!(
            default_sky.horizon_glow_percent,
            default_sky_horizon_glow_percent()
        );
        assert_eq!(
            default_sky.horizon_glow_yaw_degrees,
            default_sky_horizon_glow_yaw_degrees()
        );
        assert_eq!(default_sky.sun_enabled, default_sky_sun_enabled());
        assert_eq!(default_sky.sun_color, default_sky_sun_color());
        assert_eq!(default_sky.sun_border_color, default_sky_sun_border_color());
        assert_eq!(default_sky.sun_yaw_degrees, default_sky_sun_yaw_degrees());
        assert_eq!(
            default_sky.sun_pitch_degrees,
            default_sky_sun_pitch_degrees()
        );
        assert_eq!(default_sky.sun_size_percent, default_sky_sun_size_percent());
        assert_eq!(default_sky.sun_glow_percent, default_sky_sun_glow_percent());
        assert_eq!(
            default_sky.sun_glow_size_percent,
            default_sky_sun_glow_size_percent()
        );
        assert_eq!(
            default_sky.mountain_height_percent,
            default_sky_mountain_height_percent()
        );
        assert_eq!(
            default_sky.mountain_top_color,
            default_sky_mountain_top_color()
        );
        assert_eq!(
            default_sky.mountain_base_color,
            default_sky_mountain_base_color()
        );
        assert_eq!(
            default_sky.mountain_gap_percent,
            default_sky_mountain_gap_percent()
        );
        assert_eq!(
            default_sky.mountain_roughness_percent,
            default_sky_mountain_roughness_percent()
        );
        assert_eq!(
            default_sky.mountain_layer_count,
            default_sky_mountain_layer_count()
        );

        let mut sky = SkySettings::default();
        sky.horizon_glow_percent = 240;
        sky.horizon_glow_yaw_degrees = 720;
        sky.sun_yaw_degrees = -720;
        sky.sun_pitch_degrees = 120;
        sky.sun_size_percent = 0;
        sky.sun_glow_percent = 240;
        sky.sun_glow_size_percent = 240;
        sky.mountain_height_percent = 240;
        sky.mountain_gap_percent = 240;
        sky.mountain_roughness_percent = 240;
        sky.mountain_layer_count = 9;
        sky.skybox_columns = 1;
        sky.skybox_rows = 99;
        let resolved = sky.resolved_for_room(false, [0, 0, 0]);
        assert_eq!(resolved.horizon_glow_percent, 100);
        assert_eq!(resolved.horizon_glow_yaw_degrees, 180);
        assert_eq!(resolved.sun_yaw_degrees, -180);
        assert_eq!(resolved.sun_pitch_degrees, 75);
        assert_eq!(resolved.sun_size_percent, 1);
        assert_eq!(resolved.sun_glow_percent, 100);
        assert_eq!(resolved.sun_glow_size_percent, 100);
        assert_eq!(
            resolved.mountain_height_percent,
            SKY_MOUNTAIN_HEIGHT_PERCENT_MAX
        );
        assert_eq!(resolved.mountain_gap_percent, 100);
        assert_eq!(resolved.mountain_roughness_percent, 100);
        assert_eq!(resolved.mountain_layer_count, 3);
        assert_eq!(resolved.skybox_columns, SKYBOX_COLUMNS_MIN);
        assert_eq!(resolved.skybox_rows, SKYBOX_ROWS_MAX);
    }

    #[test]
    fn sky_cyclorama_generation_is_cook_time_geometry() {
        let mut sky = SkySettings::default();
        sky.cloud_layer.enabled = true;
        sky.cloud_layer.density = 192;
        let resolved = sky.resolved_for_room(false, [0, 0, 0]);
        let quads = generate_sky_cyclorama(resolved);
        assert!(!quads.is_empty());
        assert!(quads.len() <= SKY_CYCLORAMA_QUAD_MAX);
        assert!(quads
            .iter()
            .any(|quad| quad.direction_q12[0] != quad.direction_q12[1]));

        let mut disabled = sky;
        disabled.mode = SkyMode::Off;
        assert!(generate_sky_cyclorama(disabled.resolved_for_room(false, [0, 0, 0])).is_empty());
    }

    #[test]
    fn dense_cyclorama_sky_stays_under_playtest_budget() {
        let mut sky = SkySettings::default();
        sky.top_color = [36, 36, 36];
        sky.horizon_color = [87, 34, 34];
        sky.lower_color = [0, 0, 0];
        sky.horizon_percent = 40;
        sky.horizon_thickness_percent = 0;
        sky.sun_enabled = true;
        sky.mountain_layer_count = 3;
        sky.skybox_columns = 12;
        sky.skybox_rows = 5;
        sky.cloud_layer.enabled = true;
        sky.cloud_layer.color = [155, 142, 140];
        sky.cloud_layer.density = 255;
        sky.cloud_layer.altitude = 5800;
        sky.cloud_layer.extent = 49_800;
        sky.cloud_layer.tile_count = 9;
        sky.cloud_layer.noise_seed = 0x5a7b_c91d;

        let quads = generate_sky_cyclorama(sky.resolved_for_room(false, [0, 0, 0]));

        // The runtime consumes a baked panorama; this guard keeps
        // cook/editor-preview source geometry from growing without
        // bound as the procedural sky gains detail.
        assert!(
            quads.len() <= 1050,
            "dense sky generated {} quads",
            quads.len()
        );
    }

    #[test]
    fn sky_cyclorama_sun_uses_faceted_polar_geometry() {
        let mut sky = SkySettings::default();
        sky.sun_enabled = true;
        sky.mountain_height_percent = 0;
        sky.top_color = [178, 178, 198];
        sky.horizon_color = [142, 108, 100];
        sky.lower_color = [80, 58, 70];
        sky.cloud_layer.enabled = false;

        let resolved = sky.resolved_for_room(false, [0, 0, 0]);
        let base_quads = resolved.skybox_columns as usize * resolved.skybox_rows as usize;
        let quads = generate_sky_cyclorama(resolved);
        let sun_quads = &quads[base_quads..];

        assert_eq!(sun_quads.len(), SKY_CYCLORAMA_SUN_QUAD_MAX);
        assert!(sun_quads.iter().any(|quad| {
            quad.direction_q12[2] == quad.direction_q12[3]
                && quad.direction_q12[0] != quad.direction_q12[2]
        }));
        assert!(sun_quads.iter().any(|quad| quad.rgb[0] != quad.rgb[1]));
    }

    #[test]
    fn normalize_loaded_snaps_room_heights_to_quantum() {
        let mut project = ProjectDocument::starter();
        let room_id = project
            .active_scene()
            .nodes()
            .iter()
            .find(|node| matches!(node.kind, NodeKind::Room { .. }))
            .map(|node| node.id)
            .unwrap();
        {
            let room = project.active_scene_mut().node_mut(room_id).unwrap();
            let NodeKind::Room { grid } = &mut room.kind else {
                panic!("expected room");
            };
            let sector = grid.ensure_sector(0, 0).unwrap();
            sector.floor = Some(GridHorizontalFace::flat(33, None));
            let walls = sector.walls.get_mut(GridDirection::West);
            walls.clear();
            walls.push(GridVerticalFace::with_heights([0, 0, 965, 802], None));
        }

        project.normalize_loaded();

        let room = project.active_scene().node(room_id).unwrap();
        let NodeKind::Room { grid } = &room.kind else {
            panic!("expected room");
        };
        let sector = grid.sector(0, 0).unwrap();
        assert_eq!(sector.floor.as_ref().unwrap().heights, [64, 64, 64, 64]);
        assert_eq!(
            sector.walls.get(GridDirection::West)[0].heights,
            [0, 0, 960, 832]
        );
    }

    #[test]
    fn snap_world_sector_size_quantizes_to_128_units() {
        assert_eq!(WORLD_SECTOR_SIZE_QUANTUM, 128);
        assert_eq!(snap_world_sector_size(1), 128);
        assert_eq!(snap_world_sector_size(127), 128);
        assert_eq!(snap_world_sector_size(191), 128);
        assert_eq!(snap_world_sector_size(192), 256);
        assert_eq!(snap_world_sector_size(1500), 1536);
        assert_eq!(
            snap_world_sector_size(MAX_WORLD_SECTOR_SIZE + 1),
            MAX_WORLD_SECTOR_SIZE
        );
    }

    #[test]
    fn world_camera_settings_default_normalize_and_inherit() {
        let mut project = ProjectDocument::starter();
        let scene = project.active_scene();
        let world_id = scene
            .nodes()
            .iter()
            .find(|node| matches!(node.kind, NodeKind::World { .. }))
            .map(|node| node.id)
            .expect("starter has world");
        let room_id = scene
            .nodes()
            .iter()
            .find(|node| matches!(node.kind, NodeKind::Room { .. }))
            .map(|node| node.id)
            .expect("starter has room");

        let inherited_camera = scene
            .nodes()
            .iter()
            .find_map(|node| match &node.kind {
                NodeKind::World { camera, .. } => Some(*camera),
                _ => None,
            })
            .expect("starter world has camera settings");
        assert_eq!(scene.world_camera_for_node(room_id), Some(inherited_camera));

        {
            let world = project.active_scene_mut().node_mut(world_id).unwrap();
            let NodeKind::World { camera, .. } = &mut world.kind else {
                panic!("expected world");
            };
            *camera = WorldCameraSettings {
                distance: 1,
                height: MAX_WORLD_CAMERA_HEIGHT + 1,
                target_height: -1,
                min_floor_clearance: MAX_WORLD_CAMERA_MIN_FLOOR_CLEARANCE + 1,
            };
        }

        project.normalize_loaded();

        assert_eq!(
            project.active_scene().world_camera_for_node(room_id),
            Some(WorldCameraSettings {
                distance: MIN_WORLD_CAMERA_DISTANCE,
                height: MAX_WORLD_CAMERA_HEIGHT,
                target_height: 0,
                min_floor_clearance: MAX_WORLD_CAMERA_MIN_FLOOR_CLEARANCE,
            })
        );
    }

    #[test]
    fn world_streaming_settings_separate_resident_and_visible_limits() {
        let settings = WorldStreamingSettings {
            resident_chunk_limit: 24,
            visible_chunk_limit: 8,
        }
        .normalized();

        assert_eq!(settings.resident_chunk_limit, 24);
        assert_eq!(settings.visible_chunk_limit, 8);
    }

    #[test]
    fn world_streaming_legacy_visible_limit_inherits_resident_limit() {
        let settings = WorldStreamingSettings {
            resident_chunk_limit: 18,
            visible_chunk_limit: 0,
        }
        .normalized();

        assert_eq!(settings.resident_chunk_limit, 18);
        assert_eq!(settings.visible_chunk_limit, 18);
    }

    #[test]
    fn changing_world_sector_size_rescales_descendant_room_and_colliders() {
        let mut project = ProjectDocument::new("test");
        let scene = project.active_scene_mut();
        let world = scene.add_node(
            scene.root,
            "World",
            NodeKind::World {
                sector_size: 1024,
                sky: SkySettings::default(),
                far_vista: FarVistaSettings::default(),
                camera: WorldCameraSettings::default(),
                culling: WorldCullingSettings::default(),
                streaming: WorldStreamingSettings::default(),
            },
        );
        let mut grid = WorldGrid::empty(1, 1, 1024);
        grid.set_floor(0, 0, 160, None);
        grid.add_wall(0, 0, GridDirection::North, 0, 1024, None);
        let room = scene.add_node(world, "Room", NodeKind::Room { grid });
        let entity = scene.add_node(room, "Entity", NodeKind::Entity);
        let collider = scene.add_node(
            entity,
            "Collider",
            NodeKind::Collider {
                shape: ColliderShape::Capsule {
                    radius: 128,
                    height: 1024,
                },
                solid: true,
            },
        );

        assert_eq!(project.set_world_sector_size(world, 1500), Some(1536));
        assert_eq!(project.world_sector_size_for_node(entity), 1536);

        let scene = project.active_scene();
        let NodeKind::Room { grid } = &scene.node(room).unwrap().kind else {
            panic!("expected Room");
        };
        assert_eq!(grid.sector_size, 1536);
        let sector = grid.sector(0, 0).unwrap();
        assert_eq!(sector.floor.as_ref().unwrap().heights, [256; 4]);
        assert_eq!(
            sector
                .walls
                .get(GridDirection::North)
                .first()
                .unwrap()
                .heights,
            [0, 0, 1536, 1536]
        );

        let NodeKind::Collider {
            shape: ColliderShape::Capsule { radius, height },
            ..
        } = &scene.node(collider).unwrap().kind
        else {
            panic!("expected capsule collider");
        };
        assert_eq!((*radius, *height), (192, 1536));
    }

    #[test]
    fn saving_normalizes_room_sector_size_to_world() {
        let mut project = ProjectDocument::starter();
        let scene = project.active_scene_mut();
        let room_id = scene
            .nodes()
            .iter()
            .find(|node| matches!(node.kind, NodeKind::Room { .. }))
            .map(|node| node.id)
            .unwrap();
        let NodeKind::Room { grid } = &mut scene.node_mut(room_id).unwrap().kind else {
            panic!("expected Room");
        };
        grid.sector_size = 2030;

        let dir = unique_temp_dir("normalize-room-sector-size");
        let path = dir.join("project.ron");
        project.save_to_path(&path).unwrap();

        let saved = std::fs::read_to_string(&path).unwrap();
        let expected_sector_size = project.world_sector_size_for_node(room_id);
        assert!(saved.contains(&format!("kind: World(sector_size: {expected_sector_size},")));
        assert!(saved.contains(&format!("sector_size: {expected_sector_size}")));
        assert!(!saved.contains("sector_size: 2030"));

        let loaded = ProjectDocument::load_from_path(&path).unwrap();
        let scene = loaded.active_scene();
        let NodeKind::Room { grid } = &scene.node(room_id).unwrap().kind else {
            panic!("expected Room");
        };
        assert_eq!(grid.sector_size, expected_sector_size);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn grid_direction_physical_edges_use_editor_z_convention() {
        assert_eq!(
            GridDirection::North.physical_edge(2, 3),
            Some(GridPhysicalEdge {
                x: 2,
                z: 4,
                axis: GridEdgeAxis::EastWest,
            })
        );
        assert_eq!(
            GridDirection::South.physical_edge(2, 3),
            Some(GridPhysicalEdge {
                x: 2,
                z: 3,
                axis: GridEdgeAxis::EastWest,
            })
        );
        assert_eq!(
            GridDirection::East.physical_edge(2, 3),
            Some(GridPhysicalEdge {
                x: 3,
                z: 3,
                axis: GridEdgeAxis::NorthSouth,
            })
        );
        assert_eq!(
            GridDirection::West.physical_edge(2, 3),
            Some(GridPhysicalEdge {
                x: 2,
                z: 3,
                axis: GridEdgeAxis::NorthSouth,
            })
        );
        assert_eq!(GridDirection::NorthWestSouthEast.physical_edge(2, 3), None);
    }

    #[test]
    fn cell_bounds_match_editor_corner_and_wall_convention() {
        let grid = WorldGrid::empty(2, 2, 1024);
        let bounds = grid.cell_bounds_world(1, 1);

        assert_eq!(bounds.horizontal_corner_xz(Corner::NW), [1024, 2048]);
        assert_eq!(bounds.horizontal_corner_xz(Corner::NE), [2048, 2048]);
        assert_eq!(bounds.horizontal_corner_xz(Corner::SE), [2048, 1024]);
        assert_eq!(bounds.horizontal_corner_xz(Corner::SW), [1024, 1024]);

        assert_eq!(
            bounds.wall_endpoints_xz(GridDirection::North),
            Some(([1024, 2048], [2048, 2048]))
        );
        assert_eq!(
            bounds.wall_endpoints_xz(GridDirection::South),
            Some(([2048, 1024], [1024, 1024]))
        );
        assert_eq!(
            bounds.wall_endpoints_xz(GridDirection::NorthWestSouthEast),
            Some(([1024, 2048], [2048, 1024]))
        );
        assert_eq!(
            bounds.wall_endpoints_xz(GridDirection::NorthEastSouthWest),
            Some(([2048, 2048], [1024, 1024]))
        );
    }

    #[test]
    fn wall_placement_aligns_bottom_edge_to_floor_vertices() {
        let mut grid = WorldGrid::empty(1, 1, 1024);
        let mut floor = GridHorizontalFace::flat(0, None);
        floor.heights = [128, 256, 384, 512];
        grid.ensure_sector(0, 0).unwrap().floor = Some(floor);

        grid.add_wall_aligned_to_surfaces(0, 0, GridDirection::North, None);

        let wall = grid
            .sector(0, 0)
            .unwrap()
            .walls
            .get(GridDirection::North)
            .first()
            .unwrap();
        assert_eq!(wall.heights, [128, 256, 2304, 2176]);
    }

    #[test]
    fn wall_placement_aligns_top_edge_to_ceiling_vertices() {
        let mut grid = WorldGrid::empty(1, 1, 1024);
        let mut floor = GridHorizontalFace::flat(0, None);
        floor.heights = [128, 256, 384, 512];
        let mut ceiling = GridHorizontalFace::flat(1024, None);
        ceiling.heights = [900, 1000, 1100, 1200];
        let sector = grid.ensure_sector(0, 0).unwrap();
        sector.floor = Some(floor);
        sector.ceiling = Some(ceiling);

        grid.add_wall_aligned_to_surfaces(0, 0, GridDirection::East, None);

        let wall = grid
            .sector(0, 0)
            .unwrap()
            .walls
            .get(GridDirection::East)
            .first()
            .unwrap();
        assert_eq!(wall.heights, [256, 384, 1100, 1000]);
    }

    #[test]
    fn diagonal_wall_placement_aligns_to_horizontal_diagonal_vertices() {
        let mut grid = WorldGrid::empty(1, 1, 1024);
        let mut floor = GridHorizontalFace::flat(0, None);
        floor.heights = [128, 256, 384, 512];
        let mut ceiling = GridHorizontalFace::flat(1024, None);
        ceiling.heights = [900, 1000, 1100, 1200];
        let sector = grid.ensure_sector(0, 0).unwrap();
        sector.floor = Some(floor);
        sector.ceiling = Some(ceiling);

        grid.add_wall_aligned_to_surfaces(0, 0, GridDirection::NorthWestSouthEast, None);
        grid.add_wall_aligned_to_surfaces(0, 0, GridDirection::NorthEastSouthWest, None);

        let sector = grid.sector(0, 0).unwrap();
        let nw_se = sector
            .walls
            .get(GridDirection::NorthWestSouthEast)
            .first()
            .unwrap();
        let ne_sw = sector
            .walls
            .get(GridDirection::NorthEastSouthWest)
            .first()
            .unwrap();
        assert_eq!(nw_se.heights, [128, 384, 1100, 900]);
        assert_eq!(ne_sw.heights, [256, 512, 1200, 1000]);
    }

    #[test]
    fn ceiling_placement_aligns_edge_to_touching_wall_top() {
        let mut grid = WorldGrid::empty(1, 1, 1024);
        grid.ensure_sector(0, 0)
            .unwrap()
            .walls
            .get_mut(GridDirection::North)
            .push(GridVerticalFace::with_heights([0, 0, 1472, 1344], None));

        grid.set_ceiling_aligned_to_neighbors(0, 0, None);

        let ceiling = grid.sector(0, 0).unwrap().ceiling.as_ref().unwrap();
        assert_eq!(ceiling.heights, [1344, 1472, 2048, 2048]);
    }

    #[test]
    fn floor_placement_with_one_flat_neighbor_uses_that_height_for_whole_face() {
        let mut grid = WorldGrid::empty(2, 1, 1024);
        grid.set_floor(0, 0, 384, None);

        grid.set_floor_aligned_to_neighbors(1, 0, 0, None);

        let floor = grid.sector(1, 0).unwrap().floor.as_ref().unwrap();
        assert_eq!(floor.heights, [384; 4]);
    }

    #[test]
    fn floor_preview_for_off_grid_cell_matches_flat_neighbor_height() {
        let mut grid = WorldGrid::empty(1, 1, 1024);
        grid.set_floor(0, 0, 384, None);

        let heights = grid.floor_heights_aligned_to_neighbors_for_world_cell(1, 0, 0);

        assert_eq!(heights, [384; 4]);
    }

    #[test]
    fn floor_placement_with_one_sloped_neighbor_keeps_only_the_shared_edge() {
        let mut grid = WorldGrid::empty(2, 1, 1024);
        let mut floor = GridHorizontalFace::flat(0, None);
        floor.heights = [128, 256, 384, 512];
        grid.ensure_sector(0, 0).unwrap().floor = Some(floor);

        grid.set_floor_aligned_to_neighbors(1, 0, 0, None);

        let floor = grid.sector(1, 0).unwrap().floor.as_ref().unwrap();
        assert_eq!(floor.heights, [256, 0, 0, 384]);
    }

    #[test]
    fn ceiling_placement_with_one_flat_neighbor_uses_that_height_for_whole_face() {
        let mut grid = WorldGrid::empty(2, 1, 1024);
        grid.ensure_sector(0, 0).unwrap().ceiling = Some(GridHorizontalFace::flat(1536, None));

        grid.set_ceiling_aligned_to_neighbors(1, 0, None);

        let ceiling = grid.sector(1, 0).unwrap().ceiling.as_ref().unwrap();
        assert_eq!(ceiling.heights, [1536; 4]);
    }

    #[test]
    fn ceiling_preview_for_off_grid_cell_matches_flat_neighbor_height() {
        let mut grid = WorldGrid::empty(1, 1, 1024);
        grid.ensure_sector(0, 0).unwrap().ceiling = Some(GridHorizontalFace::flat(1536, None));

        let heights = grid.ceiling_heights_aligned_to_neighbors_for_world_cell(1, 0);

        assert_eq!(heights, [1536; 4]);
    }

    #[test]
    fn ceiling_placement_aligns_edge_to_touching_neighbor_wall_top() {
        let mut grid = WorldGrid::empty(2, 1, 1024);
        grid.ensure_sector(0, 0)
            .unwrap()
            .walls
            .get_mut(GridDirection::East)
            .push(GridVerticalFace::with_heights([0, 0, 1600, 1536], None));

        grid.set_ceiling_aligned_to_neighbors(1, 0, None);

        let ceiling = grid.sector(1, 0).unwrap().ceiling.as_ref().unwrap();
        assert_eq!(ceiling.heights, [1536, 2048, 2048, 1600]);
    }

    #[test]
    fn ceiling_placement_aligns_edge_to_touching_neighbor_ceiling() {
        let mut grid = WorldGrid::empty(2, 1, 1024);
        let mut ceiling = GridHorizontalFace::flat(2048, None);
        ceiling.heights = [1024, 1152, 1280, 1408];
        grid.ensure_sector(0, 0).unwrap().ceiling = Some(ceiling);

        grid.set_ceiling_aligned_to_neighbors(1, 0, None);

        let ceiling = grid.sector(1, 0).unwrap().ceiling.as_ref().unwrap();
        assert_eq!(ceiling.heights, [1152, 2048, 2048, 1280]);
    }

    #[test]
    fn off_grid_wall_preview_samples_adjacent_floor_edge() {
        let mut grid = WorldGrid::empty(1, 1, 1024);
        let mut floor = GridHorizontalFace::flat(0, None);
        floor.heights = [128, 256, 384, 512];
        grid.ensure_sector(0, 0).unwrap().floor = Some(floor);

        let heights =
            grid.wall_heights_aligned_to_surfaces_for_world_cell(1, 0, GridDirection::West);

        assert_eq!(heights, [384, 256, 2304, 2432]);
    }

    #[test]
    fn wall_stack_placement_starts_above_highest_wall_top() {
        let mut grid = WorldGrid::empty(1, 1, 1024);
        grid.add_wall(0, 0, GridDirection::North, 0, 1024, None);

        let heights = grid.wall_heights_above_stack_or_surfaces(0, 0, GridDirection::North);

        assert_eq!(heights, [1024, 1024, 3072, 3072]);
    }

    #[test]
    fn wall_stack_placement_preserves_sloped_top_edge() {
        let mut grid = WorldGrid::empty(1, 1, 1024);
        grid.ensure_sector(0, 0)
            .unwrap()
            .walls
            .get_mut(GridDirection::North)
            .push(GridVerticalFace::with_heights([0, 0, 1408, 1152], None));

        let heights = grid.wall_heights_above_stack_or_surfaces(0, 0, GridDirection::North);

        assert_eq!(heights, [1152, 1408, 3456, 3200]);
    }

    #[test]
    fn stone_room_perimeter_uses_editor_direction_convention() {
        let grid = WorldGrid::stone_room(2, 3, 1024, None, None);
        let default_wall_height = default_wall_height_for_sector_size(1024);

        for x in 0..grid.width {
            assert!(!grid
                .sector(x, 0)
                .unwrap()
                .walls
                .get(GridDirection::South)
                .is_empty());
            assert!(grid
                .sector(x, 0)
                .unwrap()
                .walls
                .get(GridDirection::North)
                .is_empty());
            assert!(!grid
                .sector(x, grid.depth - 1)
                .unwrap()
                .walls
                .get(GridDirection::North)
                .is_empty());
            assert!(grid
                .sector(x, grid.depth - 1)
                .unwrap()
                .walls
                .get(GridDirection::South)
                .is_empty());
        }
        let south_wall = grid
            .sector(0, 0)
            .unwrap()
            .walls
            .get(GridDirection::South)
            .first()
            .unwrap();
        assert_eq!(
            south_wall.heights,
            [0, 0, default_wall_height, default_wall_height]
        );
    }

    #[test]
    fn editor_to_room_local_round_trip_origin_zero() {
        let grid = WorldGrid::stone_room(3, 3, 1024, None, None);
        for editor in [[0.0_f32, 0.0], [1.5, -0.25], [-1.4, 1.49]] {
            let world = grid.editor_to_room_local(editor);
            let back = grid.room_local_to_editor(world);
            assert!(
                (back[0] - editor[0]).abs() < 1e-3,
                "x: {editor:?} → {back:?}"
            );
            assert!(
                (back[1] - editor[1]).abs() < 1e-3,
                "z: {editor:?} → {back:?}"
            );
        }
    }

    #[test]
    fn editor_to_room_local_round_trip_negative_origin() {
        let mut grid = WorldGrid::stone_room(3, 3, 1024, None, None);
        // Force a -2/-3 origin via the public grow path so the
        // test shape matches what auto-grow actually produces.
        grid.extend_to_include(-2, -3);
        assert_eq!(grid.origin, [-2, -3]);

        for editor in [[0.0_f32, 0.0], [2.0, -1.25], [-3.5, 1.0]] {
            let world = grid.editor_to_room_local(editor);
            let back = grid.room_local_to_editor(world);
            assert!(
                (back[0] - editor[0]).abs() < 1e-3,
                "x: {editor:?} → {back:?}"
            );
            assert!(
                (back[1] - editor[1]).abs() < 1e-3,
                "z: {editor:?} → {back:?}"
            );
        }
    }

    #[test]
    fn editor_cells_to_array_resolves_to_correct_cell() {
        // Plain 3×3, origin [0, 0]: editor (0, 0) is room centre,
        // which falls inside cell (1, 1).
        let grid = WorldGrid::stone_room(3, 3, 1024, None, None);
        assert_eq!(grid.editor_cells_to_array([0.0, 0.0]), Some((1, 1)));
        assert_eq!(grid.editor_cells_to_array([-1.4, -1.4]), Some((0, 0)));
        assert_eq!(grid.editor_cells_to_array([1.4, 1.4]), Some((2, 2)));
        // Past the room edge: out of range.
        assert_eq!(grid.editor_cells_to_array([-2.0, 0.0]), None);
    }

    #[test]
    fn editor_cells_to_array_after_negative_grow_is_origin_aware() {
        // Negative-side grow: origin shifts but the previously-
        // existing cells must remain reachable from the same
        // editor coordinates. After `extend_to_include(-1, 0)` on a
        // 3×3 starter the room becomes width=4, depth=3, origin=[-1,0].
        // Old cell at world-cell (0, 0) is now at array (1, 0).
        let mut grid = WorldGrid::stone_room(3, 3, 1024, None, None);
        grid.extend_to_include(-1, 0);
        assert_eq!(grid.origin, [-1, 0]);
        assert_eq!(grid.width, 4);
        // grid_center_cells = [-1 + 2, 0 + 1.5] = [1.0, 1.5]; cell
        // (1, 0) has world-cell centre [0.5, 0.5], so editor centre
        // is [0.5 - 1.0, 0.5 - 1.5] = [-0.5, -1.0].
        assert_eq!(grid.editor_cells_to_array([-0.5, -1.0]), Some((1, 0)));
        // Newly-included cell at array (0, 0) -- world-cell (-1, 0),
        // editor centre [-0.5 - 1.0, -1.0] = [-1.5, -1.0].
        assert_eq!(grid.editor_cells_to_array([-1.5, -1.0]), Some((0, 0)));
    }

    #[test]
    fn cell_center_world_in_editor_units_matches_helper() {
        let mut grid = WorldGrid::stone_room(4, 5, 1024, None, None);
        grid.extend_to_include(-2, -1);
        let s = grid.sector_size as f32;
        for (sx, sz) in [(0u16, 0u16), (1, 2), (3, 4)] {
            let world_centre = grid.cell_center_world(sx, sz);
            let editor = grid.world_cells_to_editor([world_centre[0] / s, world_centre[1] / s]);
            // Same cell via editor_cells_to_array should round-trip.
            assert_eq!(grid.editor_cells_to_array(editor), Some((sx, sz)));
        }
    }

    #[test]
    fn authored_footprint_ignores_empty_allocation() {
        let mut grid = WorldGrid::empty(8, 6, 1024);
        let _ = grid.ensure_sector(0, 0);
        grid.set_floor(2, 1, 0, None);
        grid.add_wall(5, 4, GridDirection::North, 0, 1024, None);

        let footprint = grid.authored_footprint().expect("authored geometry");
        assert_eq!(
            footprint,
            WorldGridFootprint {
                x: 2,
                z: 1,
                width: 4,
                depth: 4,
            }
        );
        assert_eq!(grid.populated_sector_count(), 2);

        let budget = grid.authored_budget();
        assert_eq!(budget.width, 4);
        assert_eq!(budget.depth, 4);
        assert_eq!(budget.total_cells, 16);
        assert_eq!(budget.populated_cells, 2);
    }

    #[test]
    fn budget_empty_grid_reports_no_geometry() {
        let grid = WorldGrid::empty(3, 3, 1024);
        let b = grid.budget();
        assert_eq!(b.width, 3);
        assert_eq!(b.depth, 3);
        assert_eq!(b.total_cells, 9);
        assert_eq!(b.populated_cells, 0);
        assert_eq!(b.floors, 0);
        assert_eq!(b.ceilings, 0);
        assert_eq!(b.walls, 0);
        assert_eq!(b.triangles, 0);
        // AssetHeader + active WorldHeader + 9 sector records.
        // `.psxw` stores a record per cell whether populated or not.
        assert_eq!(
            b.psxw_bytes,
            12 + psxed_format::world::WorldHeader::SIZE
                + 9 * psxed_format::world::SectorRecord::SIZE
        );
        assert_eq!(b.static_light_table_bytes, 0);
        assert_eq!(b.psxw_static_lit_bytes, b.psxw_bytes);
        assert_eq!(
            b.future_compact_estimated_bytes,
            12 + psxed_format::world::WorldHeader::SIZE + 9 * 28
        );
        assert!(!b.over_budget());
        assert!(!b.static_lit_over_budget());
    }

    #[test]
    fn budget_starter_room_matches_authored_geometry() {
        let grid = WorldGrid::stone_room(3, 3, 1024, None, None);
        let b = grid.budget();
        assert_eq!(b.populated_cells, 9);
        assert_eq!(b.floors, 9);
        assert_eq!(b.ceilings, 0);
        // Perimeter only: 4 sides * 3 cells = 12 walls.
        assert_eq!(b.walls, 12);
        // 2 tris per face: 9 floors + 12 walls = 21 faces.
        assert_eq!(b.triangles, 42);
        // The future compact estimate should be strictly smaller
        // than the active format once any geometry exists.
        assert!(b.future_compact_estimated_bytes < b.psxw_bytes);
        assert_eq!(
            b.static_light_table_bytes,
            (9 * 2 + 12) * psxed_format::world::SurfaceLightRecord::SIZE
        );
        assert_eq!(
            b.psxw_static_lit_bytes,
            b.psxw_bytes + b.static_light_table_bytes
        );
        assert!(!b.over_budget());
        assert!(!b.static_lit_over_budget());
    }

    #[test]
    fn budget_counts_generated_floor_transition_walls() {
        let mut grid = WorldGrid::empty(2, 1, 1024);
        grid.set_floor(0, 0, 0, None);
        grid.set_floor(1, 0, 512, None);

        let b = grid.budget();

        assert_eq!(b.floors, 2);
        assert_eq!(b.walls, 1);
        assert_eq!(b.triangles, 6);
    }

    #[test]
    fn budget_max_dimension_grid_within_caps() {
        // Floors-only at MAX_ROOM_WIDTH × MAX_ROOM_DEPTH = 32 × 32.
        // Stresses the byte-cap path without going over MAX_ROOM_TRIANGLES.
        let mut grid = WorldGrid::empty(MAX_ROOM_WIDTH, MAX_ROOM_DEPTH, 1024);
        for x in 0..MAX_ROOM_WIDTH {
            for z in 0..MAX_ROOM_DEPTH {
                grid.set_floor(x, z, 0, None);
            }
        }
        let b = grid.budget();
        assert_eq!(b.populated_cells, 1024);
        assert_eq!(b.floors, 1024);
        assert_eq!(b.triangles, 2048);
        assert!(b.triangles <= MAX_ROOM_TRIANGLES);
        // Active format remains under the byte cap for floors-only;
        // the wall-stack-heavy worst case is what pushes rooms over.
        assert!(b.psxw_bytes <= MAX_ROOM_BYTES);
        assert!(b.psxw_static_lit_bytes > MAX_ROOM_BYTES);
        assert!(b.future_compact_estimated_bytes <= MAX_ROOM_BYTES);
        assert!(!b.over_budget());
        assert!(b.static_lit_over_budget());
    }

    #[test]
    fn budget_flags_oversized_room_dimensions() {
        // 64×16 fits the byte cap but blows past MAX_ROOM_WIDTH.
        // The old `over_budget` check only watched triangles +
        // bytes; this test pins the new width/depth check that
        // catches asymmetric over-sized rooms.
        let grid = WorldGrid::empty(MAX_ROOM_WIDTH * 2, MAX_ROOM_DEPTH / 2, 1024);
        let b = grid.budget();
        assert!(b.over_budget(), "{b:?}");
    }

    #[test]
    fn extend_to_include_grows_positively_without_shift() {
        let mut grid = WorldGrid::stone_room(3, 3, 1024, None, None);
        let baseline_floor_world = grid.cell_world_x(0); // 0
        let cell = grid.extend_to_include(5, 1);
        assert_eq!(cell, (5, 1));
        assert_eq!(grid.width, 6);
        assert_eq!(grid.depth, 3);
        assert_eq!(grid.origin, [0, 0]);
        // Old (0, 0) data still at array (0, 0), still at world 0.
        assert_eq!(grid.cell_world_x(0), baseline_floor_world);
        assert!(grid.sector(0, 0).is_some());
    }

    #[test]
    fn extend_to_include_grows_negatively_preserving_world_position() {
        let mut grid = WorldGrid::stone_room(3, 3, 1024, None, None);
        let cell = grid.extend_to_include(-2, 0);
        assert_eq!(cell, (0, 0));
        // Two new columns prepended in -X.
        assert_eq!(grid.width, 5);
        assert_eq!(grid.origin[0], -2);
        // Old (0, 0) data is now at array (2, 0), still at world 0.
        assert_eq!(grid.cell_world_x(2), 0);
        assert!(grid.sector(2, 0).is_some());
        // The newly-included cell at array (0, 0) is empty.
        assert!(grid.sector(0, 0).is_none());
    }

    #[test]
    fn embedded_default_project_ron_deserializes() {
        let project = ProjectDocument::from_ron_str(DEFAULT_PROJECT_RON).unwrap();
        assert!(project.resources.iter().any(|r| matches!(
            &r.data,
            ResourceData::Texture { psxt_path } if psxt_path.ends_with("block_1a.psxt")
        )));
        assert!(project.resources.iter().any(|r| matches!(
            &r.data,
            ResourceData::Texture { psxt_path } if psxt_path.ends_with("fence_1a.psxt")
        )));
        assert!(!project.resources.iter().any(|r| matches!(
            &r.data,
            ResourceData::Texture { psxt_path }
                if psxt_path.ends_with("floor.psxt")
                    || psxt_path.ends_with("brick-wall.psxt")
        )));
        // Starter now mirrors demo7: the active player is the
        // Crimson Cross Knight profile with its Meshy Gold animation
        // library and material atlas.
        let (character_id, character) = project
            .resources
            .iter()
            .find_map(|r| match &r.data {
                ResourceData::Character(c) if r.name == "Crimson Cross Knight Player" => {
                    Some((r.id, c))
                }
                _ => None,
            })
            .expect("starter player character resource missing");
        let model_id = character.model.expect("starter character has a model");
        let model = project
            .resource(model_id)
            .and_then(|resource| match &resource.data {
                ResourceData::Model(model) => Some(model),
                _ => None,
            })
            .expect("starter player model resource missing");
        assert!(model
            .model_path
            .ends_with("crimson_cross_knight/crimson_cross_knight.psxmdl"));
        assert!(model
            .texture_path
            .as_deref()
            .is_some_and(|path| path.ends_with("crimson_cross_knight.psxt")));
        assert!(model.skeleton.is_some());
        assert!(model.clips.len() >= 50);
        assert_eq!(model.default_clip, Some(22));
        assert_eq!(
            model.collision_radius,
            default_model_collision_radius_for_height(model.world_height)
        );
        let resolved_clips = project.resolved_model_animation_clips(model_id);
        assert!(resolved_clips.len() >= 50);
        assert!(resolved_clips
            .iter()
            .any(|clip| clip.name == "Meshy Gold / idle 03"));
        assert_eq!(model.scale_q8, [MODEL_SCALE_ONE_Q8; 3]);
        assert!(project.active_scene().nodes().iter().any(|node| matches!(
            &node.kind,
            NodeKind::CharacterController {
                player: true,
                character: Some(id),
                ..
            } if *id == character_id
        )));
    }

    #[test]
    fn legacy_world_and_actor_project_ron_migrates_to_world_sector_and_entity() {
        fn replace_first_world_payload(source: &str) -> String {
            let start = source
                .find("kind: World(")
                .expect("default fixture has a parameterised World kind");
            let payload_start = start + "kind: World".len();
            let mut depth = 0i32;
            for (offset, ch) in source[payload_start..].char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            let end = payload_start + offset + ch.len_utf8();
                            return format!("{}kind: World{}", &source[..start], &source[end..]);
                        }
                    }
                    _ => {}
                }
            }
            panic!("unterminated World payload");
        }

        let starter = ProjectDocument::from_ron_str(DEFAULT_PROJECT_RON).unwrap();
        assert!(starter
            .active_scene()
            .nodes()
            .iter()
            .any(|node| matches!(node.kind, NodeKind::World { .. })));
        let legacy = replace_first_world_payload(DEFAULT_PROJECT_RON).replacen(
            "kind: Entity,",
            "kind: Actor,",
            1,
        );

        let project = ProjectDocument::from_ron_str(&legacy).unwrap();
        let scene = project.active_scene();
        let root = scene.node(scene.root).expect("world root exists");
        assert!(matches!(root.kind, NodeKind::World { .. }));
        assert_eq!(root.name, "World");
        assert!(scene.nodes().iter().all(|node| node.name != "Root"));
        let world = scene
            .nodes()
            .iter()
            .find(|node| node.name == "World")
            .expect("starter world exists");
        assert!(matches!(
            &world.kind,
            NodeKind::World { sector_size, .. } if *sector_size == DEFAULT_WORLD_SECTOR_SIZE
        ));
        let migrated = scene
            .nodes()
            .iter()
            .find(|node| node.name == "Player")
            .expect("starter player entity exists");
        assert!(matches!(&migrated.kind, NodeKind::Entity));
    }

    #[test]
    fn starter_model_files_present_on_disk() {
        let root = default_project_dir();
        assert!(root
            .join("assets/models/crimson_cross_knight/crimson_cross_knight.psxmdl")
            .is_file());
        assert!(root
            .join("assets/models/crimson_cross_knight/crimson_cross_knight.psxt")
            .is_file());
        assert!(root
            .join("assets/models/crimson_cross_knight/crimson_cross_knight_armature_idle_03_baselayer.psxanim")
            .is_file());
    }

    #[test]
    fn projects_dir_resolves_to_real_directory() {
        assert!(projects_dir().is_dir(), "{}", projects_dir().display());
        assert!(default_project_dir().join("project.ron").is_file());
        assert!(default_project_dir()
            .join("assets/textures/block_1a.psxt")
            .is_file());
        assert!(default_project_dir()
            .join("assets/textures/fence_1a.psxt")
            .is_file());
        assert!(default_project_dir()
            .join("assets/models/crimson_cross_knight/crimson_cross_knight.psxmdl")
            .is_file());
        assert!(!default_project_dir()
            .join("assets/textures/floor.psxt")
            .exists());
        assert!(!default_project_dir()
            .join("assets/textures/brick-wall.psxt")
            .exists());
    }

    #[test]
    fn project_file_stem_is_filesystem_safe() {
        assert_eq!(
            project_file_stem("Stone Room: Vertical Slice!"),
            "stone_room_vertical_slice"
        );
        assert_eq!(project_file_stem("PSoXide 2"), "psoxide_2");
        assert_eq!(project_file_stem("..."), "project");
    }

    #[test]
    fn starter_project_has_scene_tree_and_resources() {
        let project = ProjectDocument::starter();

        assert_eq!(project.scenes.len(), 1);
        // Starter includes the demo7 room texture/material set plus gameplay
        // resources for the animated character and weapon path.
        assert!(project.resources.len() >= 10);
        assert!(project
            .active_scene()
            .hierarchy_rows()
            .iter()
            .any(|row| row.kind == "Room" && row.name == "Demo7 Map"));
        let grid = project
            .active_scene()
            .nodes()
            .iter()
            .find_map(|node| match &node.kind {
                NodeKind::Room { grid } => Some(grid),
                _ => None,
            })
            .expect("starter should contain a room node");
        assert!(grid.width > 0);
        assert!(grid.depth > 0);
        assert_eq!(
            grid.sectors.len(),
            grid.width as usize * grid.depth as usize
        );
        assert!(grid.populated_sector_count() > 0);
    }

    #[test]
    fn project_missing_point_light_color_and_room_ambient_uses_defaults() {
        let starter = ProjectDocument::from_ron_str(DEFAULT_PROJECT_RON).unwrap();
        let light = starter
            .active_scene()
            .nodes()
            .iter()
            .find_map(|node| match &node.kind {
                NodeKind::PointLight {
                    color,
                    intensity,
                    radius,
                } => Some((*color, *intensity, *radius)),
                _ => None,
            })
            .expect("starter has a light");
        let room = starter
            .active_scene()
            .nodes()
            .iter()
            .find_map(|node| match &node.kind {
                NodeKind::Room { grid } => Some(grid),
                _ => None,
            })
            .expect("starter has a room");
        let source = DEFAULT_PROJECT_RON
            .replace(
                &format!(
                    "kind: PointLight(color: ({}, {}, {}), intensity: {}, radius: {})",
                    light.0[0], light.0[1], light.0[2], light.1, light.2
                ),
                &format!(
                    "kind: PointLight(intensity: {}, radius: {})",
                    light.1, light.2
                ),
            )
            .replace(
                &format!(
                    ", ambient_color: ({}, {}, {})",
                    room.ambient_color[0], room.ambient_color[1], room.ambient_color[2]
                ),
                "",
            )
            .replace(
                &format!(", fog_near: {}, fog_far: {}", room.fog_near, room.fog_far),
                "",
            );

        let project = ProjectDocument::from_ron_str(&source).unwrap();
        let light_color = project
            .active_scene()
            .nodes()
            .iter()
            .find_map(|node| match &node.kind {
                NodeKind::PointLight { color, .. } => Some(*color),
                _ => None,
            })
            .expect("starter has a light");
        assert_eq!(light_color, default_light_color());

        let ambient = project
            .active_scene()
            .nodes()
            .iter()
            .find_map(|node| match &node.kind {
                NodeKind::Room { grid } => Some(grid.ambient_color),
                _ => None,
            })
            .expect("starter has a room");
        assert_eq!(ambient, default_ambient_color());

        let fog = project
            .active_scene()
            .nodes()
            .iter()
            .find_map(|node| match &node.kind {
                NodeKind::Room { grid } => Some((grid.fog_color, grid.fog_near, grid.fog_far)),
                _ => None,
            })
            .expect("starter has a room");
        assert_eq!(
            fog,
            (default_fog_color(), default_fog_near(), default_fog_far())
        );
    }

    #[test]
    fn adding_node_preserves_parent_child_relationship() {
        let mut scene = Scene::new("Test");

        let room = scene.add_node(
            scene.root,
            "Room",
            NodeKind::Room {
                grid: WorldGrid::empty(2, 2, 1024),
            },
        );
        let child = scene.add_node(
            room,
            "Spawn",
            NodeKind::SpawnPoint {
                player: true,
                character: None,
            },
        );

        assert_eq!(scene.node(child).and_then(|node| node.parent), Some(room));
        assert!(scene
            .node(room)
            .is_some_and(|node| node.children.contains(&child)));
    }

    #[test]
    fn removing_node_removes_descendants() {
        let mut scene = Scene::new("Test");
        let parent = scene.add_node(scene.root, "A", NodeKind::Node3D);
        let child = scene.add_node(parent, "B", NodeKind::Node3D);

        assert!(scene.remove_node(parent));
        assert!(scene.node(parent).is_none());
        assert!(scene.node(child).is_none());
        assert!(scene
            .node(scene.root)
            .is_some_and(|root| root.children.is_empty()));
    }

    #[test]
    fn move_node_reparents_and_reorders() {
        let mut scene = Scene::new("Test");
        let a = scene.add_node(scene.root, "A", NodeKind::Node3D);
        let b = scene.add_node(scene.root, "B", NodeKind::Node3D);
        let c = scene.add_node(a, "C", NodeKind::Node3D);

        // Reparent c from a to b at position 0.
        assert!(scene.move_node(c, b, 0));
        assert_eq!(scene.node(c).unwrap().parent, Some(b));
        assert!(scene.node(a).unwrap().children.is_empty());
        assert_eq!(scene.node(b).unwrap().children, vec![c]);

        // Reorder b before a at the root.
        assert!(scene.move_node(b, scene.root, 0));
        assert_eq!(scene.node(scene.root).unwrap().children, vec![b, a]);
    }

    #[test]
    fn move_node_rejects_cycles_and_root() {
        let mut scene = Scene::new("Test");
        let a = scene.add_node(scene.root, "A", NodeKind::Node3D);
        let b = scene.add_node(a, "B", NodeKind::Node3D);

        // Cannot reparent a node under itself.
        assert!(!scene.move_node(a, a, 0));
        // Cannot reparent an ancestor under its descendant.
        assert!(!scene.move_node(a, b, 0));
        // Cannot move the root.
        assert!(!scene.move_node(scene.root, a, 0));
    }

    #[test]
    fn project_roundtrips_through_ron_string() {
        let project = ProjectDocument::starter();
        let ron = project.to_ron_string().unwrap();

        assert!(ron.contains("Demo7 Map"));
        assert_eq!(ProjectDocument::from_ron_str(&ron).unwrap(), project);
    }

    #[test]
    fn editor_camera_roundtrips_through_ron_string() {
        let mut project = ProjectDocument::new("camera");
        project.editor_camera = EditorCameraState {
            mode: EditorCameraMode::Free,
            orbit_yaw_q12: 384,
            orbit_pitch_q12: 4096 - 128,
            orbit_radius: 8192,
            orbit_target: [1024, 512, -2048],
            free_yaw_q12: 1536,
            free_pitch_q12: 128,
            free_position: [-300, 700, 900],
            free_initialized: true,
        };
        let ron = project.to_ron_string().unwrap();

        assert!(ron.contains("editor_camera"));
        assert_eq!(ProjectDocument::from_ron_str(&ron).unwrap(), project);
    }

    #[test]
    fn editor_visibility_roundtrips_through_ron_string() {
        let mut project = ProjectDocument::new("visibility");
        project.editor_visibility = EditorVisibilityState {
            show_grid: false,
            show_portals: true,
            show_lights: false,
            preview_fog: false,
            preview_backface_wireframe: true,
            preview_bounds: false,
            show_play_debug_overlays: false,
            show_play_debug_map: true,
        };
        let ron = project.to_ron_string().unwrap();

        assert!(ron.contains("editor_visibility"));
        assert_eq!(ProjectDocument::from_ron_str(&ron).unwrap(), project);
    }

    #[test]
    fn runtime_depth_sort_mode_roundtrips_through_ron_string() {
        let mut project = ProjectDocument::new("depth-sort");
        project.runtime_depth_sort_mode = RuntimeDepthSortMode::PerTriangle;
        project.runtime_texture_split_max_edge = 96;
        let ron = project.to_ron_string().unwrap();

        assert!(ron.contains("runtime_depth_sort_mode"));
        assert!(ron.contains("runtime_texture_split_max_edge"));
        assert_eq!(ProjectDocument::from_ron_str(&ron).unwrap(), project);
    }

    #[test]
    fn model_resource_roundtrips_through_ron_string() {
        let mut project = ProjectDocument::starter();
        let id = project.add_resource(
            "TestModel",
            ResourceData::Model(ModelResource {
                model_path: "assets/models/x/x.psxmdl".to_string(),
                source_path: None,
                texture_path: Some("assets/models/x/x.psxt".to_string()),
                skeleton: None,
                clips: vec![
                    ModelAnimationClip {
                        name: "idle".to_string(),
                        psxanim_path: "assets/models/x/x_idle.psxanim".to_string(),
                        calibration: Default::default(),
                    },
                    ModelAnimationClip {
                        name: "walk".to_string(),
                        psxanim_path: "assets/models/x/x_walk.psxanim".to_string(),
                        calibration: AnimationClipCalibration {
                            in_place: false,
                            offset: [12, -8, 24],
                        },
                    },
                ],
                default_clip: Some(0),
                preview_clip: Some(1),
                world_height: 1280,
                collision_radius: 240,
                scale_q8: [
                    MODEL_SCALE_ONE_Q8,
                    MODEL_SCALE_ONE_Q8 * 2,
                    MODEL_SCALE_ONE_Q8,
                ],
                attachments: vec![AttachmentSocket {
                    name: "right_hand_grip".to_string(),
                    joint: 3,
                    translation: [16, 32, -8],
                    rotation_q12: [0, 1024, 0],
                }],
            }),
        );
        let ron = project.to_ron_string().unwrap();
        let restored = ProjectDocument::from_ron_str(&ron).unwrap();
        assert_eq!(restored, project);
        let resource = restored.resource(id).unwrap();
        match &resource.data {
            ResourceData::Model(m) => {
                assert_eq!(m.clips.len(), 2);
                assert_eq!(m.default_clip, Some(0));
                assert_eq!(m.preview_clip, Some(1));
                assert_eq!(m.clips[1].calibration.offset, [12, -8, 24]);
                assert!(!m.clips[1].calibration.in_place);
                assert_eq!(m.world_height, 1280);
                assert_eq!(m.collision_radius, 240);
                assert_eq!(
                    m.scale_q8,
                    [
                        MODEL_SCALE_ONE_Q8,
                        MODEL_SCALE_ONE_Q8 * 2,
                        MODEL_SCALE_ONE_Q8
                    ]
                );
                assert_eq!(m.effective_preview_clip(), Some(1));
                assert_eq!(m.effective_runtime_clip(), Some(0));
                assert_eq!(m.attachments.len(), 1);
                assert_eq!(m.attachments[0].joint, 3);
                assert_eq!(m.attachments[0].translation, [16, 32, -8]);
            }
            _ => panic!("expected Model"),
        }
    }

    #[test]
    fn animation_library_resources_roundtrip_and_resolve_by_path() {
        let mut project = ProjectDocument::new("Animation Test");
        let skeleton = project.add_resource(
            "Humanoid Skeleton",
            ResourceData::Skeleton(SkeletonResource {
                joint_count: 2,
                parents: vec![None, Some(0)],
                signature: "psx-parent-v1:2:root,0".to_string(),
                note: "test skeleton".to_string(),
            }),
        );
        let idle_animation = project.add_resource(
            "Idle",
            ResourceData::AnimationClip(AnimationClipResource {
                psxanim_path: "assets/animations/idle.psxanim".to_string(),
                skeleton: Some(skeleton),
                source: None,
                target_model: None,
                bake: AnimationClipBakeKind::LegacyShared,
                role: AnimationRole::Idle,
                looping: true,
                tags: vec!["idle".to_string()],
                calibration: Default::default(),
            }),
        );
        let set = project.add_resource(
            "Humanoid Set",
            ResourceData::AnimationSet(AnimationSetResource {
                skeleton: Some(skeleton),
                idle_clip: Some(idle_animation),
                walk_clip: None,
                run_clip: None,
                turn_clip: None,
                roll_clip: None,
                backstep_clip: None,
                action_clips: Vec::new(),
                clips: Vec::new(),
            }),
        );
        let model = project.add_resource(
            "Humanoid Model",
            ResourceData::Model(ModelResource {
                model_path: "assets/models/humanoid.psxmdl".to_string(),
                source_path: None,
                texture_path: Some("assets/models/humanoid.psxt".to_string()),
                skeleton: Some(skeleton),
                clips: vec![ModelAnimationClip {
                    name: "legacy idle".to_string(),
                    psxanim_path: "assets/animations/idle.psxanim".to_string(),
                    calibration: Default::default(),
                }],
                default_clip: Some(0),
                preview_clip: Some(0),
                world_height: 1024,
                collision_radius: default_model_collision_radius_for_height(1024),
                scale_q8: [MODEL_SCALE_ONE_Q8; 3],
                attachments: Vec::new(),
            }),
        );
        project.add_resource(
            "Character",
            ResourceData::Character(CharacterResource {
                model: Some(model),
                animation_set: Some(set),
                idle_clip: None,
                walk_clip: None,
                run_clip: None,
                turn_clip: None,
                ..CharacterResource::default()
            }),
        );

        let restored = ProjectDocument::from_ron_str(&project.to_ron_string().unwrap()).unwrap();
        assert_eq!(restored, project);
        assert_eq!(
            restored.resolved_model_animation_index(model, idle_animation),
            Some(0),
            "standalone clips matching legacy model-local paths resolve to the stable legacy index",
        );
    }

    #[test]
    fn animation_sources_and_target_specific_clips_roundtrip() {
        let mut project = ProjectDocument::new("Animation Source Test");
        let skeleton = project.add_resource(
            "Meshy Biped Skeleton",
            ResourceData::Skeleton(SkeletonResource {
                joint_count: 24,
                parents: vec![None],
                signature: "psx-parent-v1:24:root".to_string(),
                note: "test skeleton".to_string(),
            }),
        );
        let model_a = project.add_resource(
            "Knight",
            ResourceData::Model(ModelResource {
                model_path: "assets/models/knight/knight.psxmdl".to_string(),
                source_path: None,
                texture_path: None,
                skeleton: Some(skeleton),
                clips: Vec::new(),
                default_clip: None,
                preview_clip: None,
                world_height: 1024,
                collision_radius: default_model_collision_radius_for_height(1024),
                scale_q8: [MODEL_SCALE_ONE_Q8; 3],
                attachments: Vec::new(),
            }),
        );
        let model_b = project.add_resource(
            "Wraith",
            ResourceData::Model(ModelResource {
                model_path: "assets/models/wraith/wraith.psxmdl".to_string(),
                source_path: None,
                texture_path: None,
                skeleton: Some(skeleton),
                clips: Vec::new(),
                default_clip: None,
                preview_clip: None,
                world_height: 1024,
                collision_radius: default_model_collision_radius_for_height(1024),
                scale_q8: [MODEL_SCALE_ONE_Q8; 3],
                attachments: Vec::new(),
            }),
        );
        let source = project.add_resource(
            "Mixamo Roll",
            ResourceData::AnimationSource(AnimationSourceResource {
                source_path: "assets/animations/source/stand_to_roll.fbx".to_string(),
                clip_name: "Stand To Roll".to_string(),
                provider: AnimationSourceProvider::Mixamo,
                skeleton: Some(skeleton),
                target_model: None,
                role: AnimationRole::Generic,
                looping: false,
                tags: vec!["roll".to_string()],
            }),
        );
        let shared_walk = project.add_resource(
            "Shared Walk",
            ResourceData::AnimationClip(AnimationClipResource {
                psxanim_path: "assets/animations/shared_walk.psxanim".to_string(),
                skeleton: Some(skeleton),
                source: None,
                target_model: None,
                bake: AnimationClipBakeKind::LegacyShared,
                role: AnimationRole::Walk,
                looping: true,
                tags: vec!["walk".to_string()],
                calibration: Default::default(),
            }),
        );
        let baked_for_a = project.add_resource(
            "Knight Roll",
            ResourceData::AnimationClip(AnimationClipResource {
                psxanim_path: "assets/models/knight/knight_roll.psxanim".to_string(),
                skeleton: Some(skeleton),
                source: Some(source),
                target_model: Some(model_a),
                bake: AnimationClipBakeKind::Retargeted,
                role: AnimationRole::Generic,
                looping: false,
                tags: vec!["roll".to_string()],
                calibration: Default::default(),
            }),
        );
        project.add_resource(
            "Wraith Roll",
            ResourceData::AnimationClip(AnimationClipResource {
                psxanim_path: "assets/models/wraith/wraith_roll.psxanim".to_string(),
                skeleton: Some(skeleton),
                source: Some(source),
                target_model: Some(model_b),
                bake: AnimationClipBakeKind::Retargeted,
                role: AnimationRole::Generic,
                looping: false,
                tags: vec!["roll".to_string()],
                calibration: Default::default(),
            }),
        );

        let restored = ProjectDocument::from_ron_str(&project.to_ron_string().unwrap()).unwrap();
        assert_eq!(restored, project);
        let model_a_clips = restored.resolved_model_animation_clips(model_a);
        let baked_for_a_index = model_a_clips
            .iter()
            .position(|clip| clip.animation_resource == Some(baked_for_a))
            .expect("target-specific clip should resolve for its model");
        let shared_walk_index = model_a_clips
            .iter()
            .position(|clip| clip.animation_resource == Some(shared_walk))
            .expect("generic clip should still resolve for matching skeleton");
        assert!(
            baked_for_a_index < shared_walk_index,
            "target-specific clips should be offered before generic skeleton-shared clips",
        );
        assert!(!restored
            .resolved_model_animation_clips(model_b)
            .iter()
            .any(|clip| clip.animation_resource == Some(baked_for_a)));
        assert_eq!(restored.resource_reference_count(source), 2);
    }

    #[test]
    fn mesh_instance_with_animation_clip_roundtrips() {
        let mut project = ProjectDocument::starter();
        let scene = project.active_scene_mut();
        let room_id = scene
            .nodes()
            .iter()
            .find(|n| matches!(n.kind, NodeKind::Room { .. }))
            .map(|n| n.id)
            .unwrap();
        let model_resource_id = ResourceId(99);
        scene.add_node(
            room_id,
            "TestWraith",
            NodeKind::MeshInstance {
                mesh: Some(model_resource_id),
                material: None,
                animation_clip: Some(2),
            },
        );
        let ron = project.to_ron_string().unwrap();
        let restored = ProjectDocument::from_ron_str(&ron).unwrap();
        assert_eq!(restored, project);
        // Confirm the new field survives.
        let surviving = restored
            .active_scene()
            .nodes()
            .iter()
            .find(|n| n.name == "TestWraith")
            .unwrap();
        assert!(matches!(
            surviving.kind,
            NodeKind::MeshInstance {
                mesh: Some(_),
                animation_clip: Some(2),
                ..
            }
        ));
    }

    #[test]
    fn legacy_mesh_instance_without_animation_clip_loads() {
        // Synthesize the pre-extension MeshInstance shape -- `animation_clip`
        // missing -- and confirm `#[serde(default)]` lands `None`.
        let ron = r#"
            (
                name: "Legacy",
                next_resource_id: 1,
                resources: [],
                scenes: [
                    Scene(
                        name: "Demo",
                        next_node_id: 3,
                        root: NodeId(1),
                        nodes: [
                            (
                                id: NodeId(1),
                                name: "Root",
                                parent: None,
                                children: [NodeId(2)],
                                kind: Node3D,
                                transform: (translation: (0.0, 0.0, 0.0), rotation_degrees: (0.0, 0.0, 0.0), scale: (1.0, 1.0, 1.0)),
                            ),
                            (
                                id: NodeId(2),
                                name: "OldMesh",
                                parent: Some(NodeId(1)),
                                children: [],
                                kind: MeshInstance(mesh: None, material: None),
                                transform: (translation: (0.0, 0.0, 0.0), rotation_degrees: (0.0, 0.0, 0.0), scale: (1.0, 1.0, 1.0)),
                            ),
                        ],
                    ),
                ],
            )
        "#;
        let project = ProjectDocument::from_ron_str(ron).unwrap();
        let mesh = project
            .active_scene()
            .nodes()
            .iter()
            .find(|n| n.name == "OldMesh")
            .unwrap();
        assert!(matches!(
            mesh.kind,
            NodeKind::MeshInstance {
                mesh: None,
                material: None,
                animation_clip: None,
            }
        ));
    }

    #[test]
    fn project_saves_and_loads_from_disk() {
        let mut project = ProjectDocument::starter();
        project.name = "Disk Test".to_string();

        let dir = std::env::temp_dir().join(format!(
            "psxed-project-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("project.ron");

        project.save_to_path(&path).unwrap();
        assert_eq!(ProjectDocument::load_from_path(&path).unwrap(), project);

        let _ = std::fs::remove_dir_all(dir);
    }

    fn unique_temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "psxed-project-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn resource_rename_moves_project_owned_texture_file() {
        let root = unique_temp_dir("resource-rename-texture");
        let texture_dir = root.join("assets").join("textures");
        std::fs::create_dir_all(&texture_dir).unwrap();
        std::fs::write(texture_dir.join("floor.psxt"), b"texture").unwrap();

        let mut project = ProjectDocument::new("test");
        let id = project.add_resource(
            "Floor",
            ResourceData::Texture {
                psxt_path: "assets/textures/floor.psxt".to_string(),
            },
        );

        let report = project
            .rename_resource_with_files(id, "Stone Floor", &root)
            .unwrap();

        assert_eq!(project.resource_name(id), Some("Stone Floor"));
        let ResourceData::Texture { psxt_path } = &project.resource(id).unwrap().data else {
            panic!("expected texture");
        };
        assert_eq!(psxt_path, "assets/textures/stone_floor.psxt");
        assert!(!texture_dir.join("floor.psxt").exists());
        assert_eq!(
            std::fs::read(texture_dir.join("stone_floor.psxt")).unwrap(),
            b"texture"
        );
        assert_eq!(report.renamed_files.len(), 1);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn resource_rename_moves_imported_model_bundle_files() {
        let root = unique_temp_dir("resource-rename-model");
        let bundle_dir = root.join("assets").join("models").join("obsidian_wraith");
        std::fs::create_dir_all(&bundle_dir).unwrap();
        std::fs::write(bundle_dir.join("obsidian_wraith.psxmdl"), b"model").unwrap();
        std::fs::write(bundle_dir.join("obsidian_wraith.psxt"), b"atlas").unwrap();
        std::fs::write(bundle_dir.join("obsidian_wraith_idle.psxanim"), b"idle").unwrap();
        std::fs::write(bundle_dir.join("obsidian_wraith_walk.psxanim"), b"walk").unwrap();

        let mut project = ProjectDocument::new("test");
        let id = project.add_resource(
            "Obsidian Wraith",
            ResourceData::Model(ModelResource {
                model_path: "assets/models/obsidian_wraith/obsidian_wraith.psxmdl".to_string(),
                source_path: None,
                texture_path: Some(
                    "assets/models/obsidian_wraith/obsidian_wraith.psxt".to_string(),
                ),
                skeleton: None,
                clips: vec![
                    ModelAnimationClip {
                        name: "idle".to_string(),
                        psxanim_path: "assets/models/obsidian_wraith/obsidian_wraith_idle.psxanim"
                            .to_string(),
                        calibration: Default::default(),
                    },
                    ModelAnimationClip {
                        name: "walk".to_string(),
                        psxanim_path: "assets/models/obsidian_wraith/obsidian_wraith_walk.psxanim"
                            .to_string(),
                        calibration: Default::default(),
                    },
                ],
                default_clip: Some(0),
                preview_clip: Some(0),
                world_height: 1024,
                collision_radius: default_model_collision_radius_for_height(1024),
                scale_q8: [MODEL_SCALE_ONE_Q8; 3],
                attachments: Vec::new(),
            }),
        );

        let report = project
            .rename_resource_with_files(id, "Hooded Wretch", &root)
            .unwrap();

        let ResourceData::Model(model) = &project.resource(id).unwrap().data else {
            panic!("expected model");
        };
        assert_eq!(
            model.model_path,
            "assets/models/hooded_wretch/hooded_wretch.psxmdl"
        );
        assert_eq!(
            model.texture_path.as_deref(),
            Some("assets/models/hooded_wretch/hooded_wretch.psxt")
        );
        assert_eq!(
            model.clips[0].psxanim_path,
            "assets/models/hooded_wretch/hooded_wretch_idle.psxanim"
        );
        assert_eq!(
            model.clips[1].psxanim_path,
            "assets/models/hooded_wretch/hooded_wretch_walk.psxanim"
        );
        assert_eq!(report.renamed_files.len(), 4);
        assert!(!bundle_dir.exists());
        assert!(root
            .join("assets/models/hooded_wretch/hooded_wretch.psxmdl")
            .exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn resource_rename_refuses_existing_target_without_mutating_project() {
        let root = unique_temp_dir("resource-rename-target-exists");
        let texture_dir = root.join("assets").join("textures");
        std::fs::create_dir_all(&texture_dir).unwrap();
        std::fs::write(texture_dir.join("floor.psxt"), b"old").unwrap();
        std::fs::write(texture_dir.join("stone_floor.psxt"), b"target").unwrap();

        let mut project = ProjectDocument::new("test");
        let id = project.add_resource(
            "Floor",
            ResourceData::Texture {
                psxt_path: "assets/textures/floor.psxt".to_string(),
            },
        );

        let error = project
            .rename_resource_with_files(id, "Stone Floor", &root)
            .unwrap_err();

        assert!(matches!(error, ResourceRenameError::TargetExists(_)));
        assert_eq!(project.resource_name(id), Some("Floor"));
        let ResourceData::Texture { psxt_path } = &project.resource(id).unwrap().data else {
            panic!("expected texture");
        };
        assert_eq!(psxt_path, "assets/textures/floor.psxt");
        assert_eq!(
            std::fs::read(texture_dir.join("floor.psxt")).unwrap(),
            b"old"
        );
        assert_eq!(
            std::fs::read(texture_dir.join("stone_floor.psxt")).unwrap(),
            b"target"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn delete_resource_removes_entry_and_clears_references() {
        let root = unique_temp_dir("resource-delete");
        let texture_dir = root.join("assets").join("textures");
        std::fs::create_dir_all(&texture_dir).unwrap();
        std::fs::write(texture_dir.join("target.psxt"), b"texture").unwrap();

        let mut project = ProjectDocument::new("delete-resource");
        let target = project.add_resource(
            "Target",
            ResourceData::Texture {
                psxt_path: "assets/textures/target.psxt".to_string(),
            },
        );
        let material = project.add_resource(
            "Material",
            ResourceData::Material(MaterialResource::opaque(Some(target))),
        );
        let character = project.add_resource(
            "Character",
            ResourceData::Character(CharacterResource {
                model: Some(target),
                idle_clip: Some(0),
                walk_clip: Some(1),
                run_clip: Some(2),
                turn_clip: Some(3),
                ..CharacterResource::defaults()
            }),
        );
        let weapon = project.add_resource(
            "Weapon",
            ResourceData::Weapon(WeaponResource {
                model: Some(target),
                ..WeaponResource::default()
            }),
        );

        let scene = project.active_scene_mut();
        let mut grid = WorldGrid::empty(1, 1, 1024);
        grid.set_floor(0, 0, 0, Some(target));
        grid.add_wall(0, 0, GridDirection::North, 0, 1024, Some(target));
        let room = scene.add_node(scene.root, "Room", NodeKind::Room { grid });
        scene.add_node(
            room,
            "Mesh",
            NodeKind::MeshInstance {
                mesh: Some(target),
                material: Some(target),
                animation_clip: None,
            },
        );
        let entity = scene.add_node(room, "Entity", NodeKind::Entity);
        scene.add_node(
            entity,
            "Renderer",
            NodeKind::ModelRenderer {
                model: Some(target),
                material: Some(target),
                visual_offset: [0; 3],
                visual_scale_q8: MODEL_SCALE_ONE_Q8,
            },
        );
        scene.add_node(
            entity,
            "Controller",
            NodeKind::CharacterController {
                character: Some(target),
                settings: CharacterControllerSettings::default(),
                player: true,
            },
        );
        scene.add_node(
            entity,
            "Equipment",
            NodeKind::Equipment {
                weapon: Some(target),
                character_socket: "right_hand_grip".to_string(),
                weapon_grip: "grip".to_string(),
            },
        );
        scene.add_node(
            room,
            "Spawn",
            NodeKind::SpawnPoint {
                player: false,
                character: Some(target),
            },
        );
        scene.add_node(
            room,
            "Audio",
            NodeKind::AudioSource {
                sound: Some(target),
                radius: 1.0,
            },
        );

        assert_eq!(project.resource_reference_count(target), 13);
        let report = project
            .delete_resource_with_files(target, &root)
            .expect("resource exists");
        assert_eq!(report.removed.name, "Target");
        assert_eq!(report.cleared_references, 13);
        assert_eq!(
            report.deleted_files,
            vec![ResourceFileDelete {
                path: "assets/textures/target.psxt".to_string(),
            }]
        );
        assert!(report.skipped_files.is_empty());
        assert!(!texture_dir.join("target.psxt").exists());
        assert!(project.resource(target).is_none());
        assert_eq!(project.resource_name(material), Some("Material"));

        let ResourceData::Material(material_data) = &project.resource(material).unwrap().data
        else {
            panic!("expected material");
        };
        assert_eq!(material_data.texture, None);
        let ResourceData::Character(character_data) = &project.resource(character).unwrap().data
        else {
            panic!("expected character");
        };
        assert_eq!(character_data.model, None);
        assert_eq!(character_data.idle_clip, None);
        assert_eq!(character_data.walk_clip, None);
        assert_eq!(character_data.run_clip, None);
        assert_eq!(character_data.turn_clip, None);
        let ResourceData::Weapon(weapon_data) = &project.resource(weapon).unwrap().data else {
            panic!("expected weapon");
        };
        assert_eq!(weapon_data.model, None);

        for node in project.active_scene().nodes() {
            match &node.kind {
                NodeKind::Room { grid } => {
                    let sector = grid.sector(0, 0).unwrap();
                    assert_eq!(sector.floor.as_ref().unwrap().material, None);
                    assert_eq!(
                        sector
                            .walls
                            .get(GridDirection::North)
                            .first()
                            .unwrap()
                            .material,
                        None
                    );
                }
                NodeKind::MeshInstance { mesh, material, .. } => {
                    assert_eq!((*mesh, *material), (None, None));
                }
                NodeKind::ModelRenderer {
                    model, material, ..
                } => {
                    assert_eq!((*model, *material), (None, None));
                }
                NodeKind::CharacterController { character, .. }
                | NodeKind::SpawnPoint { character, .. } => {
                    assert_eq!(*character, None);
                }
                NodeKind::Equipment { weapon, .. } => {
                    assert_eq!(*weapon, None);
                }
                NodeKind::AudioSource { sound, .. } => {
                    assert_eq!(*sound, None);
                }
                _ => {}
            }
        }

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn corner_surviving_split_picks_diagonal_that_keeps_a_triangle() {
        // Drop NE → only the NW-SE diagonal keeps a triangle.
        // Drop NW → only the NE-SW diagonal keeps a triangle.
        assert_eq!(Corner::NE.surviving_split(), GridSplit::NorthWestSouthEast);
        assert_eq!(Corner::SW.surviving_split(), GridSplit::NorthWestSouthEast);
        assert_eq!(Corner::NW.surviving_split(), GridSplit::NorthEastSouthWest);
        assert_eq!(Corner::SE.surviving_split(), GridSplit::NorthEastSouthWest);
    }

    #[test]
    fn drop_corner_marks_face_as_triangle_and_flips_split() {
        let mut face = GridHorizontalFace::flat(0, None);
        face.split = GridSplit::NorthWestSouthEast; // would die if NW dropped
        face.drop_corner(Corner::NW);
        assert!(face.is_triangle());
        assert_eq!(face.dropped_corner, Some(Corner::NW));
        assert_eq!(face.split, GridSplit::NorthEastSouthWest);

        face.restore_corner();
        assert!(!face.is_triangle());
        assert_eq!(face.dropped_corner, None);
    }

    #[test]
    fn horizontal_triangle_overrides_inherit_until_set() {
        let parent = ResourceId(11);
        let triangle = ResourceId(12);
        let mut face = GridHorizontalFace::flat(0, Some(parent));
        face.uv.offset = [3, 4];
        face.walkable = true;

        assert_eq!(face.triangle_material(0), Some(parent));
        assert_eq!(face.triangle_uv(0), face.uv);
        assert!(face.triangle_walkable(0));

        let override_a = face.triangle_override_mut(0);
        override_a.material = Some(GridTriangleMaterialOverride::Resource(triangle));
        override_a.uv = Some(GridUvTransform {
            offset: [9, 10],
            span: [64, 32],
            rotation: GridUvRotation::Deg90,
            flip_u: true,
            flip_v: false,
        });
        override_a.walkable = Some(false);

        assert_eq!(face.triangle_material(0), Some(triangle));
        assert_eq!(face.triangle_material(1), Some(parent));
        assert_eq!(face.triangle_uv(0).offset, [9, 10]);
        assert!(!face.triangle_walkable(0));
        assert!(face.triangle_walkable(1));
    }

    #[test]
    fn drop_corner_on_wall_marks_triangle() {
        let mut wall = GridVerticalFace::flat(0, 64, None);
        wall.drop_corner(WallCorner::TL);
        assert!(wall.is_triangle());
        assert_eq!(wall.dropped_corner, Some(WallCorner::TL));
    }

    #[test]
    fn grid_uv_transform_rotates_quad_without_rebaking_texture() {
        let transform = GridUvTransform {
            offset: [0, 0],
            span: [0, 0],
            rotation: GridUvRotation::Deg90,
            flip_u: false,
            flip_v: false,
        };

        assert_eq!(
            transform.apply_to_quad([(0, 0), (64, 0), (64, 64), (0, 64)]),
            [(64, 0), (64, 64), (0, 64), (0, 0)]
        );
    }

    #[test]
    fn grid_uv_transform_rotates_quad_45_degrees_without_rebaking_texture() {
        let transform = GridUvTransform {
            offset: [0, 0],
            span: [0, 0],
            rotation: GridUvRotation::Deg45,
            flip_u: false,
            flip_v: false,
        };

        assert_eq!(
            transform.apply_to_quad([(0, 0), (64, 0), (64, 64), (0, 64)]),
            [(32, 0), (64, 32), (32, 64), (0, 32)]
        );
    }

    #[test]
    fn grid_uv_transform_rotates_quad_315_degrees_without_rebaking_texture() {
        let transform = GridUvTransform {
            offset: [0, 0],
            span: [0, 0],
            rotation: GridUvRotation::Deg315,
            flip_u: false,
            flip_v: false,
        };

        assert_eq!(
            transform.apply_to_quad([(0, 0), (64, 0), (64, 64), (0, 64)]),
            [(0, 32), (32, 0), (64, 32), (32, 64)]
        );
    }

    #[test]
    fn grid_uv_transform_flips_and_wraps_ps1_uv_offsets() {
        let transform = GridUvTransform {
            offset: [-8, 12],
            span: [0, 0],
            rotation: GridUvRotation::Deg0,
            flip_u: true,
            flip_v: false,
        };

        assert_eq!(
            transform.apply_to_quad([(0, 0), (64, 0), (64, 64), (0, 64)]),
            [(56, 12), (248, 12), (248, 76), (56, 76)]
        );
    }

    #[test]
    fn grid_uv_transform_scales_quad_span_without_rebaking_texture() {
        let transform = GridUvTransform {
            offset: [0, 0],
            span: [0, 32],
            rotation: GridUvRotation::Deg0,
            flip_u: false,
            flip_v: false,
        };

        assert_eq!(
            transform.apply_to_quad([(0, 64), (64, 64), (64, 0), (0, 0)]),
            [(0, 32), (64, 32), (64, 0), (0, 0)]
        );
    }

    #[test]
    fn wall_autotile_sets_double_height_v_span_without_changing_geometry() {
        let mut wall = GridVerticalFace::flat(0, 1536, None);
        let heights = wall.heights;

        let clamped = wall.autotile_uv(768);

        assert!(!clamped);
        assert_eq!(wall.heights, heights);
        assert_eq!(wall.uv.span, [0, 128]);
    }

    #[test]
    fn wall_autotile_uses_partial_v_span_for_short_wall() {
        let mut wall = GridVerticalFace::flat(0, 384, None);

        let clamped = wall.autotile_uv(768);

        assert!(!clamped);
        assert_eq!(wall.heights, [0, 0, 384, 384]);
        assert_eq!(wall.uv.span, [0, 32]);
    }

    #[test]
    fn wall_autotile_clamps_one_quad_to_ps1_uv_range() {
        let mut wall = GridVerticalFace::flat(0, 768 * 5, None);

        let clamped = wall.autotile_uv(768);

        assert!(clamped);
        assert_eq!(wall.heights, [0, 0, 3840, 3840]);
        assert_eq!(wall.uv.span, [0, 255]);
    }

    #[test]
    fn default_tall_wall_keeps_single_authored_uv_primitive() {
        let wall = GridVerticalFace::flat(0, 768 * 5, None);

        let segments = wall.split_into_autotile_segments(768);

        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].heights, [0, 0, 3840, 3840]);
        assert_eq!(segments[0].uv.span, [0, 0]);
    }

    #[test]
    fn wall_autotile_keeps_one_primitive_when_repeated_uvs_fit_packet() {
        let mut wall = GridVerticalFace::flat(0, 1536, None);
        wall.uv.offset[1] = -5;
        wall.autotile_uv(768);

        let segments = wall.split_into_autotile_segments(768);

        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].heights, [0, 0, 1536, 1536]);
        assert_eq!(segments[0].uv.span, [0, 128]);
        assert_eq!(segments[0].uv.offset[1], -5);
    }

    #[test]
    fn wall_autotile_segments_restore_clamped_tall_wall_density() {
        let mut wall = GridVerticalFace::flat(0, 768 * 5, None);
        wall.autotile_uv(768);

        let segments = wall.split_into_autotile_segments(768);

        assert_eq!(segments.len(), 5);
        assert!(segments.iter().all(|segment| segment.uv.span == [0, 0]));
        assert_eq!(segments[4].heights, [3072, 3072, 3840, 3840]);
    }

    #[test]
    fn wall_split_height_segments_keeps_uvs_and_sloped_edges_connected() {
        let mut wall = GridVerticalFace::flat(0, 1536, None);
        wall.heights = [0, 384, 1536, 1920];
        wall.uv.span = [12, 96];

        let segments = wall.split_into_height_segments(768);

        assert_eq!(segments.len(), 3);
        assert_eq!(
            [
                segments[0].heights[WallCorner::BL.idx()],
                segments[0].heights[WallCorner::BR.idx()],
            ],
            [0, 384]
        );
        assert_eq!(
            [
                segments[2].heights[WallCorner::TL.idx()],
                segments[2].heights[WallCorner::TR.idx()],
            ],
            [1920, 1536]
        );
        for pair in segments.windows(2) {
            assert_eq!(
                pair[0].heights[WallCorner::TL.idx()],
                pair[1].heights[WallCorner::BL.idx()]
            );
            assert_eq!(
                pair[0].heights[WallCorner::TR.idx()],
                pair[1].heights[WallCorner::BR.idx()]
            );
        }
        assert!(segments.iter().all(|segment| segment.uv.span == [12, 96]));
    }
}
