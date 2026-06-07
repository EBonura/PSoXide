use super::*;

/// World layer attached to an authored screen state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SceneWorldLayer {
    /// No 3D/gameplay layer; the state is UI-only.
    #[default]
    None,
    /// Run the gameplay/level world layer.
    Gameplay,
}

impl SceneWorldLayer {
    /// Human-readable label for editor controls.
    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Gameplay => "Gameplay",
        }
    }
}

/// One authored screen/game state. This is the scene-arranger object:
/// it references a world layer and an optional UI scene overlay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSceneState {
    /// Stable state id. Assigned during normalization for legacy data.
    #[serde(default)]
    pub id: SceneStateId,
    /// Display name.
    pub name: String,
    /// Optional 3D/gameplay layer.
    #[serde(default)]
    pub world: SceneWorldLayer,
    /// Optional UI scene drawn over the world.
    #[serde(default)]
    pub ui_scene: Option<UiSceneId>,
    /// Whether UI navigation/buttons capture input while this state is active.
    #[serde(default = "default_true")]
    pub ui_input: bool,
    /// Whether the world layer is paused while the UI overlay is active.
    #[serde(default)]
    pub pause_world: bool,
}

impl ProjectSceneState {
    /// UI-only state backed by one authored UI scene.
    pub fn ui_only(name: impl Into<String>, ui_scene: UiSceneId) -> Self {
        Self {
            id: SceneStateId::UNASSIGNED,
            name: name.into(),
            world: SceneWorldLayer::None,
            ui_scene: Some(ui_scene),
            ui_input: true,
            pause_world: false,
        }
    }

    /// Gameplay/world state with an optional UI overlay.
    pub fn gameplay(name: impl Into<String>, ui_scene: Option<UiSceneId>) -> Self {
        Self {
            id: SceneStateId::UNASSIGNED,
            name: name.into(),
            world: SceneWorldLayer::Gameplay,
            ui_scene,
            ui_input: false,
            pause_world: false,
        }
    }
}

pub(crate) fn default_scene_states_for_ui_scenes(ui_scenes: &[UiScene]) -> Vec<ProjectSceneState> {
    let mut states: Vec<ProjectSceneState> = ui_scenes
        .iter()
        .map(|scene| ProjectSceneState::ui_only(scene.name.clone(), scene.id))
        .collect();
    states.push(ProjectSceneState::gameplay("Gameplay", None));
    assign_scene_state_ids_for_slice(&mut states);
    states
}

pub(crate) fn assign_scene_state_ids_for_slice(states: &mut [ProjectSceneState]) {
    let mut next = SceneStateId::FIRST.raw();
    for state in states.iter() {
        if state.id != SceneStateId::UNASSIGNED {
            next = next.max(state.id.raw().saturating_add(1));
        }
    }
    let mut seen: HashSet<SceneStateId> = HashSet::new();
    for state in states {
        if state.id == SceneStateId::UNASSIGNED || !seen.insert(state.id) {
            state.id = SceneStateId(next);
            seen.insert(state.id);
            next = next.saturating_add(1);
        }
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

pub(crate) const fn default_image_prop_size() -> u16 {
    DEFAULT_IMAGE_PROP_SIZE
}

/// Default authored collision-box full-size (width / height / depth)
/// for an image prop. Sized to match `DEFAULT_IMAGE_PROP_SIZE` so a
/// fresh prop with collision toggled on has a sensible cube around
/// its visible plane.
pub(crate) const fn default_image_prop_collision_size() -> [u16; 3] {
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

pub(crate) const fn default_box_prop_materials() -> [Option<ResourceId>; BOX_PROP_FACE_COUNT] {
    [None; BOX_PROP_FACE_COUNT]
}

pub(crate) const fn default_box_prop_vertices() -> [[i16; 3]; BOX_PROP_VERTEX_COUNT] {
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
pub(crate) struct UvBounds {
    min_u: i32,
    max_u: i32,
    min_v: i32,
    max_v: i32,
}

pub(crate) fn uv_bounds(uvs: [(u8, u8); 4]) -> UvBounds {
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

pub(crate) fn scale_rounded(value: i32, numerator: i32, denominator: i32) -> i32 {
    if denominator == 0 {
        0
    } else {
        (value.saturating_mul(numerator) + denominator / 2) / denominator
    }
}

pub(crate) fn signed_div_round(value: i32, denominator: i32) -> i32 {
    if denominator == 0 {
        0
    } else if value >= 0 {
        (value + denominator / 2) / denominator
    } else {
        (value - denominator / 2) / denominator
    }
}

pub(crate) fn rotate_uv_diagonal_fit(
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

pub(crate) fn wrap_uv(value: i32) -> u8 {
    value.rem_euclid(256) as u8
}

pub(crate) fn wrap_tiled_uv_offset_i16(value: i64) -> i16 {
    value.rem_euclid(i64::from(psxed_format::world::TILE_UV)) as i16
}

pub(crate) const fn is_default_uv_span(span: &[u16; 2]) -> bool {
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

pub(crate) fn horizontal_triangle_index_at_local(
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

pub(crate) fn runtime_horizontal_triangle_heights(
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

pub(crate) fn quad_heights_for_triangle(
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

pub(crate) fn height_at_local_for_split(
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

pub(crate) fn mul_sector_i32(delta: i32, amount: i32, sector_size: i32) -> i32 {
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

pub(crate) fn uv_span_for_world_span(world_span: i32, sector_size: i32) -> (u16, bool) {
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

pub(crate) fn stored_uv_span(span: u16) -> u16 {
    if span == u16::from(psxed_format::world::TILE_UV) {
        0
    } else {
        span
    }
}

pub(crate) fn lerp_i32_ratio(a: i32, b: i32, numerator: i32, denominator: i32) -> i32 {
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

pub(crate) fn div_round_i64(numerator: i64, denominator: i64) -> i64 {
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
pub(crate) enum HorizontalSurface {
    Floor,
    Ceiling,
}

impl HorizontalSurface {
    pub(crate) fn edge_heights(
        self,
        sector: &GridSector,
        direction: GridDirection,
    ) -> Option<[i32; 2]> {
        let heights = match self {
            Self::Floor => sector.floor.as_ref()?.heights,
            Self::Ceiling => sector.ceiling.as_ref()?.heights,
        };
        horizontal_edge_heights_for_wall(heights, direction)
    }
}

pub(crate) fn horizontal_edge_heights_for_wall(
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

pub(crate) fn set_horizontal_edge_heights(
    heights: &mut [i32; 4],
    direction: GridDirection,
    edge: [i32; 2],
) {
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

pub(crate) fn wall_top_edge_heights(walls: &[GridVerticalFace]) -> Option<[i32; 2]> {
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

pub(crate) fn floor_transition_wall_material(
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
/// Default camera position follow lag shift. Lower values move faster.
pub const DEFAULT_WORLD_CAMERA_POSITION_LAG_SHIFT: u8 = 2;
/// Default camera focus follow lag shift. Lower values move faster.
pub const DEFAULT_WORLD_CAMERA_FOCUS_LAG_SHIFT: u8 = 2;
/// Default camera boom-distance recovery lag shift. Lower values move faster.
pub const DEFAULT_WORLD_CAMERA_DISTANCE_LAG_SHIFT: u8 = 3;
/// Minimum authored third-person camera distance.
pub const MIN_WORLD_CAMERA_DISTANCE: i32 = 384;
/// Maximum authored third-person camera distance.
pub const MAX_WORLD_CAMERA_DISTANCE: i32 = 16_384;
/// Maximum authored camera vertical offset.
pub const MAX_WORLD_CAMERA_HEIGHT: i32 = 16_384;
/// Maximum authored minimum floor clearance for the third-person camera.
pub const MAX_WORLD_CAMERA_MIN_FLOOR_CLEARANCE: i32 = 4_096;
/// Maximum authored camera follow lag shift.
pub const MAX_WORLD_CAMERA_LAG_SHIFT: u8 = 6;
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

pub(crate) fn default_world_sector_size() -> i32 {
    DEFAULT_WORLD_SECTOR_SIZE
}

pub(crate) const fn is_zero_u16(value: &u16) -> bool {
    *value == 0
}

pub(crate) fn default_world_camera_distance() -> i32 {
    DEFAULT_WORLD_CAMERA_DISTANCE
}

pub(crate) fn default_world_camera_height() -> i32 {
    DEFAULT_WORLD_CAMERA_HEIGHT
}

pub(crate) fn default_world_camera_target_height() -> i32 {
    DEFAULT_WORLD_CAMERA_TARGET_HEIGHT
}

pub(crate) fn default_world_camera_min_floor_clearance() -> i32 {
    DEFAULT_WORLD_CAMERA_MIN_FLOOR_CLEARANCE
}

pub(crate) fn default_world_camera_position_lag_shift() -> u8 {
    DEFAULT_WORLD_CAMERA_POSITION_LAG_SHIFT
}

pub(crate) fn default_world_camera_focus_lag_shift() -> u8 {
    DEFAULT_WORLD_CAMERA_FOCUS_LAG_SHIFT
}

pub(crate) fn default_world_camera_distance_lag_shift() -> u8 {
    DEFAULT_WORLD_CAMERA_DISTANCE_LAG_SHIFT
}

pub(crate) fn default_wall_height_for_sector_size(sector_size: i32) -> i32 {
    sector_size.saturating_mul(DEFAULT_WALL_HEIGHT_SECTORS)
}

pub(crate) fn default_model_scale_q8() -> [u16; 3] {
    [MODEL_SCALE_ONE_Q8; 3]
}

pub(crate) fn default_model_renderer_visual_scale_q8() -> u16 {
    MODEL_SCALE_ONE_Q8
}

pub(crate) fn scale_i32_ratio(value: i32, from: i32, to: i32) -> i32 {
    if from <= 0 || from == to {
        return value;
    }
    (((value as i64) * (to as i64) + (from as i64 / 2)) / (from as i64)) as i32
}

pub(crate) fn scale_u16_ratio(value: u16, from: i32, to: i32) -> u16 {
    scale_i32_ratio(value as i32, from, to).clamp(0, u16::MAX as i32) as u16
}
