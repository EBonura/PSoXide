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
    if !face_side_visible(shade.sidedness(), p) {
        return false;
    }
    match shade {
        FaceShade::Flat { .. } => push_tri_colors(scratch, p, colors),
        FaceShade::Textured {
            slot, blend_mode, ..
        } => push_tex_tri_colors(scratch, p, uvs, slot, colors, blend_mode),
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

/// Compose a [`TriTexturedGouraud`] sampling `slot`'s tpage / CLUT,
/// stash it in the static `tex_tris` arena, and chain it into the OT.
///
/// The per-vertex colour modulates the texel: PSX hardware computes
/// `output = texel * color / 0x80`, so `(0x80, 0x80, 0x80)` is a
/// pass-through and `(0xFF, 0x60, 0x40)` saturates a grey texel
/// toward terracotta. Textured preview uses the authored material
/// tint so it matches the cooked runtime path; flat fallback still
/// uses material-name colours to keep untextured faces readable.
pub(crate) fn push_tex_tri(
    scratch: &mut PreviewScratch,
    p: [psx_gte::scene::Projected; 3],
    uvs: [(u8, u8); 3],
    slot: MaterialSlot,
    tint: (u8, u8, u8),
    blend_mode: BlendMode,
) -> bool {
    push_tex_tri_colors(scratch, p, uvs, slot, [tint; 3], blend_mode)
}

pub(crate) fn push_tex_tri_colors(
    scratch: &mut PreviewScratch,
    p: [psx_gte::scene::Projected; 3],
    uvs: [(u8, u8); 3],
    slot: MaterialSlot,
    colors: [(u8, u8, u8); 3],
    blend_mode: BlendMode,
) -> bool {
    let avg_sz = projected_avg_sz(p);
    push_textured_material_tri(
        scratch,
        p,
        uvs,
        colors,
        preview_texture_material(slot, colors[0], blend_mode),
        room_depth_slot(avg_sz),
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
        scratch
            .ot
            .insert(slot_idx, packet_ptr.cast::<u32>(), TriTexturedGouraud::WORDS);
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
    push_tri_split(scratch, p, colors, 0)
}

pub(crate) fn push_tri_split(
    scratch: &mut PreviewScratch,
    p: [psx_gte::scene::Projected; 3],
    colors: [(u8, u8, u8); 3],
    depth: u8,
) -> bool {
    let p = clamp_preview_projected_triangle(p);
    if !preview_projected_triangle_hw_safe(p) {
        if depth >= MAX_PREVIEW_HW_SPLIT_DEPTH {
            return false;
        }
        let (a, _, a_colors, b, _, b_colors) =
            split_preview_projected_triangle(p, [(0, 0); 3], colors);
        let left = push_tri_split(scratch, a, a_colors, depth + 1);
        let right = push_tri_split(scratch, b, b_colors, depth + 1);
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
    // Map sz (Q0, range up to ~32K for our scenes) into the OT
    // depth band. Smaller sz = closer = drawn last, so map to a
    // lower OT slot index. We reserve slot OT_DEPTH-1 for the
    // per-frame fill-rect clear and slot 0 for the hover overlay
    // (drawn last so it tops everything), so geometry rides the
    // 1..OT_DEPTH-1 band exclusively.
    let slot = room_depth_slot(projected_avg_sz(p));
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
    ((avg_sz as usize) >> 6).clamp(PREVIEW_GEOMETRY_SLOT_MIN, PREVIEW_GEOMETRY_SLOT_MAX)
}
