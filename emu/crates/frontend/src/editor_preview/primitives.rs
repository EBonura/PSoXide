//! Low-level projected-triangle submission: back-face culling, HW-safe
//! clamping/splitting, and ordering-table insertion for the PSX render path.

use super::*;

/// Two preview triangles produced by splitting one: each is a vertex
/// triple plus its UV triple and per-vertex colour triple.
type SplitPreviewTri = (
    [psx_gte::scene::Projected; 3],
    [(u8, u8); 3],
    [(u8, u8, u8); 3],
    [psx_gte::scene::Projected; 3],
    [(u8, u8); 3],
    [(u8, u8, u8); 3],
);

const PREVIEW_CLIP_EDGE_MARGIN: i32 = 16;
const PREVIEW_CLIP_ATTR_ONE: i64 = 1 << 16;
/// Keep the editor on the cooked PXBSP face contract. Exact clipping can add
/// at most one vertex per frustum plane.
pub(crate) const PREVIEW_FACE_VERTEX_CAP: usize = 39;
const PREVIEW_CLIP_VERTEX_CAP: usize = PREVIEW_FACE_VERTEX_CAP + 5;

/// Camera-space brush vertex carried through the editor's exact frustum clip.
/// UV and colour attributes stay Q16 until the clipped polygon is projected so
/// a near/side-plane intersection cannot tear a textured or Gouraud edge.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreviewClipVertex {
    pub(crate) view: psx_engine::ViewVertex,
    uv_q16: [i64; 2],
    color_q16: [i64; 3],
}

pub(crate) const EMPTY_PREVIEW_CLIP_VERTEX: PreviewClipVertex = PreviewClipVertex {
    view: psx_engine::ViewVertex::ZERO,
    uv_q16: [0; 2],
    color_q16: [0; 3],
};

pub(crate) struct PreviewClippedPolygon {
    vertices: [PreviewClipVertex; PREVIEW_CLIP_VERTEX_CAP],
    len: usize,
}

impl PreviewClippedPolygon {
    pub(crate) fn as_slice(&self) -> &[PreviewClipVertex] {
        &self.vertices[..self.len]
    }
}

impl PreviewClipVertex {
    pub(crate) fn new(view: psx_engine::ViewVertex, uv: [f64; 2], color: (u8, u8, u8)) -> Self {
        Self {
            view,
            uv_q16: [
                (uv[0] * PREVIEW_CLIP_ATTR_ONE as f64).round() as i64,
                (uv[1] * PREVIEW_CLIP_ATTR_ONE as f64).round() as i64,
            ],
            color_q16: [
                i64::from(color.0) << 16,
                i64::from(color.1) << 16,
                i64::from(color.2) << 16,
            ],
        }
    }

    pub(crate) fn projected(
        self,
        projection: psx_engine::WorldProjection,
    ) -> Option<(psx_gte::scene::Projected, (u8, u8), (u8, u8, u8))> {
        let projected = projection.project_view(self.view)?;
        let uv_component = |value: i64| ((value >> 16).rem_euclid(256)) as u8;
        let color_component = |value: i64| (value >> 16).clamp(0, 255) as u8;
        Some((
            preview_projected_from_engine(projected),
            (uv_component(self.uv_q16[0]), uv_component(self.uv_q16[1])),
            (
                color_component(self.color_q16[0]),
                color_component(self.color_q16[1]),
                color_component(self.color_q16[2]),
            ),
        ))
    }
}

/// Clip a convex brush polygon against the same near and guarded screen-edge
/// planes used by the runtime PXBSP renderer. The previous editor path asked
/// `project_world_quad` to project each fan triangle and therefore dropped a
/// partially visible triangle when any corner crossed the near plane. It also
/// clamped wholly off-screen triangles into the GPU guard band, producing
/// screen-sized wedges and exhausting the preview packet arena on E1M1.
pub(crate) fn clip_preview_brush_polygon(
    projection: psx_engine::WorldProjection,
    vertices: &[PreviewClipVertex],
) -> PreviewClippedPolygon {
    let mut clipped = PreviewClippedPolygon {
        vertices: [EMPTY_PREVIEW_CLIP_VERTEX; PREVIEW_CLIP_VERTEX_CAP],
        len: 0,
    };
    if vertices.len() < 3 || vertices.len() > PREVIEW_FACE_VERTEX_CAP {
        return clipped;
    }
    clipped.vertices[..vertices.len()].copy_from_slice(vertices);
    clipped.len = vertices.len();
    let mut output = [EMPTY_PREVIEW_CLIP_VERTEX; PREVIEW_CLIP_VERTEX_CAP];
    for plane in 0..5 {
        if clipped.len < 3 {
            clipped.len = 0;
            return clipped;
        }
        let mut output_len = 0;
        let input = &clipped.vertices[..clipped.len];
        let mut previous = input[input.len() - 1];
        let mut previous_distance = preview_clip_distance(projection, plane, previous.view);
        for &current in input {
            let current_distance = preview_clip_distance(projection, plane, current.view);
            if (previous_distance >= 0) != (current_distance >= 0) {
                output[output_len] = interpolate_preview_clip_vertex(
                    previous,
                    current,
                    previous_distance,
                    current_distance,
                );
                output_len += 1;
            }
            if current_distance >= 0 {
                output[output_len] = current;
                output_len += 1;
            }
            previous = current;
            previous_distance = current_distance;
        }
        clipped.vertices[..output_len].copy_from_slice(&output[..output_len]);
        clipped.len = output_len;
    }
    if clipped.len < 3 {
        clipped.len = 0;
    }
    clipped
}

/// One depth key for every triangle produced from the same clipped surface.
/// Keeping a fan atomic prevents an unrelated polygon from being submitted
/// between its halves merely because their centroids have different depths.
pub(crate) fn clipped_surface_depth_slot(vertices: &[PreviewClipVertex]) -> usize {
    if vertices.is_empty() {
        return room_depth_slot(0);
    }
    let avg_sz = vertices
        .iter()
        .map(|vertex| vertex.view.z.clamp(0, i32::from(u16::MAX)) as u64)
        .sum::<u64>()
        / vertices.len() as u64;
    room_depth_slot(avg_sz as u32)
}

/// Clip one world-space editor guide segment to the preview frustum before
/// projecting it. Projecting the endpoints independently is not sufficient:
/// a radius ring or bound can straddle the camera plane while both projected
/// endpoints saturate to valid-looking i16 coordinates, turning a short guide
/// edge into a screen-wide slash.
pub(crate) fn clip_preview_world_segment(
    camera: psx_engine::WorldCamera,
    a: [i32; 3],
    b: [i32; 3],
) -> Option<[psx_gte::scene::Projected; 2]> {
    let world = |point: [i32; 3]| psx_engine::WorldVertex::new(point[0], point[1], point[2]);
    let mut a = camera.view_vertex(world(a));
    let mut b = camera.view_vertex(world(b));
    for plane in 0..5 {
        let a_distance = preview_clip_distance(camera.projection, plane, a);
        let b_distance = preview_clip_distance(camera.projection, plane, b);
        match (a_distance >= 0, b_distance >= 0) {
            (false, false) => return None,
            (true, true) => {}
            (false, true) => {
                a = interpolate_preview_clip_vertex(
                    PreviewClipVertex::new(a, [0.0; 2], (0, 0, 0)),
                    PreviewClipVertex::new(b, [0.0; 2], (0, 0, 0)),
                    a_distance,
                    b_distance,
                )
                .view;
            }
            (true, false) => {
                b = interpolate_preview_clip_vertex(
                    PreviewClipVertex::new(a, [0.0; 2], (0, 0, 0)),
                    PreviewClipVertex::new(b, [0.0; 2], (0, 0, 0)),
                    a_distance,
                    b_distance,
                )
                .view;
            }
        }
    }
    Some([
        preview_projected_from_engine(camera.projection.project_view(a)?),
        preview_projected_from_engine(camera.projection.project_view(b)?),
    ])
}

fn preview_clip_distance(
    projection: psx_engine::WorldProjection,
    plane: usize,
    view: psx_engine::ViewVertex,
) -> i64 {
    let x_limit = SCREEN_CX + PREVIEW_CLIP_EDGE_MARGIN;
    let y_limit = SCREEN_CY + PREVIEW_CLIP_EDGE_MARGIN;
    let focal = projection.focal_length.max(1);
    match plane {
        0 => i64::from(view.z) - i64::from(projection.near_z),
        1 => i64::from(x_limit) * i64::from(view.z) + i64::from(focal) * i64::from(view.x),
        2 => i64::from(x_limit) * i64::from(view.z) - i64::from(focal) * i64::from(view.x),
        3 => i64::from(y_limit) * i64::from(view.z) + i64::from(focal) * i64::from(view.y),
        _ => i64::from(y_limit) * i64::from(view.z) - i64::from(focal) * i64::from(view.y),
    }
}

/// Conservative brush-level frustum test. A brush is rejected only when all
/// eight corners of its world-space AABB are outside the same clip plane, so a
/// visible face can never be lost. This avoids projecting and clipping every
/// subdivided face in distant E1M1 brushes during camera navigation.
pub(crate) fn preview_brush_bounds_visible(
    camera: psx_engine::WorldCamera,
    bounds: PreviewBrushBounds,
) -> bool {
    // Fixed-point world-to-view uses two rounded Q12 stages. Keep several
    // screen pixels of distance slack so a rounding difference at an AABB
    // interior point cannot turn a boundary-touching brush into a rejection.
    const CULL_DISTANCE_MARGIN: i64 = 1024;
    let coordinate = |value: f64, round_down: bool| -> Option<i32> {
        if !value.is_finite() || value < f64::from(i32::MIN) || value > f64::from(i32::MAX) {
            return None;
        }
        Some(if round_down {
            value.floor() as i32
        } else {
            value.ceil() as i32
        })
    };
    let (Some(min_x), Some(min_y), Some(min_z)) = (
        coordinate(bounds.min[0], true),
        coordinate(bounds.min[1], true),
        coordinate(bounds.min[2], true),
    ) else {
        return true;
    };
    let (Some(max_x), Some(max_y), Some(max_z)) = (
        coordinate(bounds.max[0], false),
        coordinate(bounds.max[1], false),
        coordinate(bounds.max[2], false),
    ) else {
        return true;
    };
    let min = [min_x, min_y, min_z];
    let max = [max_x, max_y, max_z];
    let corners = core::array::from_fn::<_, 8, _>(|corner| {
        camera.view_vertex(psx_engine::WorldVertex::new(
            if corner & 1 == 0 { min[0] } else { max[0] },
            if corner & 2 == 0 { min[1] } else { max[1] },
            if corner & 4 == 0 { min[2] } else { max[2] },
        ))
    });
    (0..5).all(|plane| {
        corners.iter().any(|corner| {
            preview_clip_distance(camera.projection, plane, *corner) >= -CULL_DISTANCE_MARGIN
        })
    })
}

fn interpolate_preview_clip_vertex(
    a: PreviewClipVertex,
    b: PreviewClipVertex,
    a_distance: i64,
    b_distance: i64,
) -> PreviewClipVertex {
    let denominator = a_distance - b_distance;
    debug_assert_ne!(denominator, 0);
    let numerator = a_distance << 16;
    let mut t_q16 = numerator / denominator;
    // When A is outside, round the intersection toward the inside B endpoint.
    // Together with truncating the component interpolation toward A, this
    // keeps the quantized point on the accepted side of the plane.
    if a_distance < 0 && numerator % denominator != 0 {
        t_q16 += 1;
    }
    let t_q16 = t_q16.clamp(0, PREVIEW_CLIP_ATTR_ONE);
    let lerp = |x: i64, y: i64| x + ((y - x) * t_q16) / PREVIEW_CLIP_ATTR_ONE;
    PreviewClipVertex {
        view: psx_engine::ViewVertex::new(
            lerp(i64::from(a.view.x), i64::from(b.view.x))
                .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
            lerp(i64::from(a.view.y), i64::from(b.view.y))
                .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
            lerp(i64::from(a.view.z), i64::from(b.view.z))
                .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        ),
        uv_q16: [
            lerp(a.uv_q16[0], b.uv_q16[0]),
            lerp(a.uv_q16[1], b.uv_q16[1]),
        ],
        color_q16: [
            lerp(a.color_q16[0], b.color_q16[0]),
            lerp(a.color_q16[1], b.color_q16[1]),
            lerp(a.color_q16[2], b.color_q16[2]),
        ],
    }
}

/// Per-face emit with one uniform colour: routes to the Gouraud or
/// textured pool based on `shade`, packing UVs only when textured.
pub(crate) fn emit_face_tri(
    scratch: &mut PreviewScratch,
    p: [psx_gte::scene::Projected; 3],
    uvs: [(u8, u8); 3],
    shade: FaceShade,
) -> bool {
    let colors = match shade {
        FaceShade::Flat { rgb, .. } => [rgb; 3],
        FaceShade::Textured { tint, .. } => [tint; 3],
    };
    emit_face_tri_lit(scratch, p, uvs, shade, colors)
}

/// Per-face emit with per-vertex colours (the baked-lighting path):
/// the packets are Gouraud, so lit brush faces interpolate exactly
/// like the cooked game instead of showing flat triangle plates.
pub(crate) fn emit_face_tri_lit(
    scratch: &mut PreviewScratch,
    p: [psx_gte::scene::Projected; 3],
    uvs: [(u8, u8); 3],
    shade: FaceShade,
    colors: [(u8, u8, u8); 3],
) -> bool {
    emit_face_tri_lit_at_slot(
        scratch,
        p,
        uvs,
        shade,
        colors,
        room_depth_slot(projected_avg_sz(p)),
    )
}

/// Emit one face triangle at a surface-owned OT slot. BSP brush polygons are
/// often fans of several triangles; sorting every triangle by its own centroid
/// lets another wall land between the halves of one planar face, producing the
/// conspicuous diagonal wedges seen around arches and pillars.
pub(crate) fn emit_face_tri_lit_at_slot(
    scratch: &mut PreviewScratch,
    p: [psx_gte::scene::Projected; 3],
    uvs: [(u8, u8); 3],
    shade: FaceShade,
    colors: [(u8, u8, u8); 3],
    slot_idx: usize,
) -> bool {
    if !face_side_visible(shade.sidedness(), p) {
        return false;
    }
    match shade {
        FaceShade::Flat { .. } => push_tri_colors_at_slot(scratch, p, colors, slot_idx),
        FaceShade::Textured {
            slot, blend_mode, ..
        } => push_textured_material_tri(
            scratch,
            p,
            uvs,
            colors,
            preview_texture_material(slot, colors[0], blend_mode),
            slot_idx,
        ),
    }
}

pub(crate) fn face_side_visible(
    sidedness: psxed_project::MaterialFaceSidedness,
    p: [psx_gte::scene::Projected; 3],
) -> bool {
    let area = projected_area(p);
    match sidedness {
        psxed_project::MaterialFaceSidedness::Front => area > 0,
        psxed_project::MaterialFaceSidedness::Back => area < 0,
        psxed_project::MaterialFaceSidedness::Both => area != 0,
    }
}

pub(crate) fn projected_area(p: [psx_gte::scene::Projected; 3]) -> i32 {
    let ax = (p[1].sx as i32) - (p[0].sx as i32);
    let ay = (p[1].sy as i32) - (p[0].sy as i32);
    let bx = (p[2].sx as i32) - (p[0].sx as i32);
    let by = (p[2].sy as i32) - (p[0].sy as i32);
    ax * by - ay * bx
}

pub(crate) fn preview_projected_triangle_hw_safe(p: [psx_gte::scene::Projected; 3]) -> bool {
    let min_x = p[0].sx.min(p[1].sx).min(p[2].sx);
    let max_x = p[0].sx.max(p[1].sx).max(p[2].sx);
    let min_y = p[0].sy.min(p[1].sy).min(p[2].sy);
    let max_y = p[0].sy.max(p[1].sy).max(p[2].sy);
    min_x >= PSX_VERTEX_MIN
        && max_x <= PSX_VERTEX_MAX
        && min_y >= PSX_VERTEX_MIN
        && max_y <= PSX_VERTEX_MAX
        && ((max_x as i32) - (min_x as i32)) <= PSX_TRI_MAX_DX
        && ((max_y as i32) - (min_y as i32)) <= PSX_TRI_MAX_DY
}

pub(crate) fn clamp_preview_projected_triangle(
    p: [psx_gte::scene::Projected; 3],
) -> [psx_gte::scene::Projected; 3] {
    [
        clamp_preview_projected(p[0]),
        clamp_preview_projected(p[1]),
        clamp_preview_projected(p[2]),
    ]
}

pub(crate) fn clamp_preview_projected(p: psx_gte::scene::Projected) -> psx_gte::scene::Projected {
    psx_gte::scene::Projected {
        sx: p.sx.clamp(PSX_VERTEX_MIN, PSX_VERTEX_MAX),
        sy: p.sy.clamp(PSX_VERTEX_MIN, PSX_VERTEX_MAX),
        sz: p.sz,
    }
}

pub(crate) fn split_preview_projected_triangle(
    p: [psx_gte::scene::Projected; 3],
    uvs: [(u8, u8); 3],
    colors: [(u8, u8, u8); 3],
) -> SplitPreviewTri {
    match largest_preview_projected_edge(p) {
        0 => {
            let mid = midpoint_projected(p[0], p[1]);
            let uv = midpoint_uv(uvs[0], uvs[1]);
            let color = midpoint_color(colors[0], colors[1]);
            (
                [p[0], mid, p[2]],
                [uvs[0], uv, uvs[2]],
                [colors[0], color, colors[2]],
                [mid, p[1], p[2]],
                [uv, uvs[1], uvs[2]],
                [color, colors[1], colors[2]],
            )
        }
        1 => {
            let mid = midpoint_projected(p[1], p[2]);
            let uv = midpoint_uv(uvs[1], uvs[2]);
            let color = midpoint_color(colors[1], colors[2]);
            (
                [p[0], p[1], mid],
                [uvs[0], uvs[1], uv],
                [colors[0], colors[1], color],
                [p[0], mid, p[2]],
                [uvs[0], uv, uvs[2]],
                [colors[0], color, colors[2]],
            )
        }
        _ => {
            let mid = midpoint_projected(p[2], p[0]);
            let uv = midpoint_uv(uvs[2], uvs[0]);
            let color = midpoint_color(colors[2], colors[0]);
            (
                [p[0], p[1], mid],
                [uvs[0], uvs[1], uv],
                [colors[0], colors[1], color],
                [mid, p[1], p[2]],
                [uv, uvs[1], uvs[2]],
                [color, colors[1], colors[2]],
            )
        }
    }
}

pub(crate) fn largest_preview_projected_edge(p: [psx_gte::scene::Projected; 3]) -> usize {
    let mut edge = 0;
    let mut score = preview_edge_split_score(p[0], p[1]);
    let score_1 = preview_edge_split_score(p[1], p[2]);
    if score_1 > score {
        edge = 1;
        score = score_1;
    }
    let score_2 = preview_edge_split_score(p[2], p[0]);
    if score_2 > score {
        edge = 2;
    }
    edge
}

pub(crate) fn preview_edge_split_score(
    a: psx_gte::scene::Projected,
    b: psx_gte::scene::Projected,
) -> i32 {
    let dx = ((a.sx as i32) - (b.sx as i32)).abs();
    let dy = ((a.sy as i32) - (b.sy as i32)).abs();
    dx.max(dy.saturating_mul(2))
}

pub(crate) fn midpoint_projected(
    a: psx_gte::scene::Projected,
    b: psx_gte::scene::Projected,
) -> psx_gte::scene::Projected {
    psx_gte::scene::Projected {
        sx: midpoint_i16(a.sx, b.sx),
        sy: midpoint_i16(a.sy, b.sy),
        sz: (((a.sz as u32) + (b.sz as u32)) / 2) as u16,
    }
}

pub(crate) fn midpoint_i16(a: i16, b: i16) -> i16 {
    (a as i32 + ((b as i32) - (a as i32)) / 2) as i16
}

pub(crate) fn midpoint_uv(a: (u8, u8), b: (u8, u8)) -> (u8, u8) {
    (
        (((a.0 as u16) + (b.0 as u16)) / 2) as u8,
        (((a.1 as u16) + (b.1 as u16)) / 2) as u8,
    )
}

pub(crate) fn midpoint_color(a: (u8, u8, u8), b: (u8, u8, u8)) -> (u8, u8, u8) {
    (
        (((a.0 as u16) + (b.0 as u16)) / 2) as u8,
        (((a.1 as u16) + (b.1 as u16)) / 2) as u8,
        (((a.2 as u16) + (b.2 as u16)) / 2) as u8,
    )
}

pub(crate) fn preview_texture_material(
    slot: MaterialSlot,
    tint: (u8, u8, u8),
    blend_mode: BlendMode,
) -> TextureMaterial {
    let material = if blend_mode.is_translucent() {
        TextureMaterial::blended(slot.clut_word, slot.tpage_word, tint, blend_mode)
    } else {
        TextureMaterial::opaque(slot.clut_word, slot.tpage_word, tint)
    };
    material.with_texture_window(slot.texture_window)
}

pub(crate) fn push_shadow_tex_tri(
    scratch: &mut PreviewScratch,
    p: [psx_gte::scene::Projected; 3],
    uvs: [(u8, u8); 3],
    slot: MaterialSlot,
) -> bool {
    push_textured_material_tri(
        scratch,
        p,
        uvs,
        [(0x80, 0x80, 0x80); 3],
        TextureMaterial::blended(
            slot.clut_word,
            slot.tpage_word,
            (0x80, 0x80, 0x80),
            BlendMode::Average,
        )
        .with_raw_texture(true),
        shadow_depth_slot(projected_avg_sz(p)),
    )
}

pub(crate) fn push_textured_material_tri(
    scratch: &mut PreviewScratch,
    p: [psx_gte::scene::Projected; 3],
    uvs: [(u8, u8); 3],
    colors: [(u8, u8, u8); 3],
    material: TextureMaterial,
    slot_idx: usize,
) -> bool {
    push_textured_material_tri_split(scratch, p, uvs, colors, material, slot_idx, 0)
}

pub(crate) fn push_textured_material_tri_split(
    scratch: &mut PreviewScratch,
    p: [psx_gte::scene::Projected; 3],
    uvs: [(u8, u8); 3],
    colors: [(u8, u8, u8); 3],
    material: TextureMaterial,
    slot_idx: usize,
    depth: u8,
) -> bool {
    let p = clamp_preview_projected_triangle(p);
    if !preview_projected_triangle_hw_safe(p) {
        if depth >= MAX_PREVIEW_HW_SPLIT_DEPTH {
            return false;
        }
        let (a, a_uvs, a_colors, b, b_uvs, b_colors) =
            split_preview_projected_triangle(p, uvs, colors);
        let left = push_textured_material_tri_split(
            scratch,
            a,
            a_uvs,
            a_colors,
            material,
            slot_idx,
            depth + 1,
        );
        let right = push_textured_material_tri_split(
            scratch,
            b,
            b_uvs,
            b_colors,
            material,
            slot_idx,
            depth + 1,
        );
        return left || right;
    }
    if scratch.tex_used >= TRI_CAP {
        return false;
    }
    let idx = scratch.tex_used;
    scratch.tex_tris[idx] = TriTexturedGouraud::with_material(
        [(p[0].sx, p[0].sy), (p[1].sx, p[1].sy), (p[2].sx, p[2].sy)],
        uvs,
        colors,
        material,
    );
    let packet_ptr: *mut TriTexturedGouraud = &mut scratch.tex_tris[idx];
    unsafe {
        scratch.ot.insert(
            slot_idx,
            packet_ptr.cast::<u32>(),
            TriTexturedGouraud::WORDS,
        );
    }
    scratch.tex_used = idx + 1;
    true
}

/// Compose a [`TriGouraud`] from three projected vertices, store it
/// in the next slot of the static `tris` array, and link it into the
/// OT keyed on average screen-space depth.
pub(crate) fn push_tri(
    scratch: &mut PreviewScratch,
    p: [psx_gte::scene::Projected; 3],
    rgb: (u8, u8, u8),
) -> bool {
    push_tri_colors(scratch, p, [rgb; 3])
}

pub(crate) fn push_tri_colors(
    scratch: &mut PreviewScratch,
    p: [psx_gte::scene::Projected; 3],
    colors: [(u8, u8, u8); 3],
) -> bool {
    push_tri_colors_at_slot(scratch, p, colors, room_depth_slot(projected_avg_sz(p)))
}

pub(crate) fn push_tri_colors_at_slot(
    scratch: &mut PreviewScratch,
    p: [psx_gte::scene::Projected; 3],
    colors: [(u8, u8, u8); 3],
    slot: usize,
) -> bool {
    push_tri_split(scratch, p, colors, slot, 0)
}

pub(crate) fn push_tri_split(
    scratch: &mut PreviewScratch,
    p: [psx_gte::scene::Projected; 3],
    colors: [(u8, u8, u8); 3],
    slot: usize,
    depth: u8,
) -> bool {
    let p = clamp_preview_projected_triangle(p);
    if !preview_projected_triangle_hw_safe(p) {
        if depth >= MAX_PREVIEW_HW_SPLIT_DEPTH {
            return false;
        }
        let (a, _, a_colors, b, _, b_colors) =
            split_preview_projected_triangle(p, [(0, 0); 3], colors);
        let left = push_tri_split(scratch, a, a_colors, slot, depth + 1);
        let right = push_tri_split(scratch, b, b_colors, slot, depth + 1);
        return left || right;
    }
    if scratch.used >= TRI_CAP {
        return false;
    }
    let idx = scratch.used;
    scratch.tris[idx] = TriGouraud::new(
        [(p[0].sx, p[0].sy), (p[1].sx, p[1].sy), (p[2].sx, p[2].sy)],
        colors,
    );
    scratch.used = idx + 1;
    let packet_ptr: *mut TriGouraud = &mut scratch.tris[idx];
    unsafe {
        scratch
            .ot
            .insert(slot, packet_ptr.cast::<u32>(), TriGouraud::WORDS);
    }
    true
}

pub(crate) fn projected_avg_sz(p: [psx_gte::scene::Projected; 3]) -> u32 {
    (p[0].sz as u32 + p[1].sz as u32 + p[2].sz as u32) / 3
}

pub(crate) fn room_depth_slot(avg_sz: u32) -> usize {
    preview_geometry_depth_slot(avg_sz)
}

pub(crate) fn shadow_depth_slot(avg_sz: u32) -> usize {
    preview_geometry_depth_slot(avg_sz.saturating_sub(PREVIEW_SHADOW_DEPTH_BIAS))
}

pub(crate) fn preview_geometry_depth_slot(avg_sz: u32) -> usize {
    ((avg_sz as usize) >> 2).clamp(PREVIEW_GEOMETRY_SLOT_MIN, PREVIEW_GEOMETRY_SLOT_MAX)
}
