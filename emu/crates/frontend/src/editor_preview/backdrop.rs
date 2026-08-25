//! Frame clear, sky cyclorama, and far-vista backdrop submission.

use super::*;

/// Stamp a GP0(02h) fill-rectangle into `scratch.clear_packet` and
/// link it into the back-most OT slot so it runs first when DMA
/// walks the chain -- which is the same pattern PS1 software uses to
/// "clear" the framebuffer at the start of every frame, since the
/// HwRenderer (faithfully) preserves VRAM across frames the way real
/// hardware does.
pub(crate) fn push_clear(scratch: &mut PreviewScratch, color: [u8; 3]) {
    // PSX VRAM coords; the editor's HwRenderer renders the same
    // 320×240 sub-rect that the runtime frame-buffer would land in.
    let color_word =
        0x0200_0000_u32 | color[0] as u32 | ((color[1] as u32) << 8) | ((color[2] as u32) << 16);
    let xy_word = 0u32; // top-left at (0, 0)
    let wh_word = ((240u32) << 16) | 320u32; // pack_xy(320, 240)
                                             // word[0] is rewritten by `OrderingTable::insert` with the
                                             // chain tag -- leave it at 0 here.
    scratch.clear_packet[1] = color_word;
    scratch.clear_packet[2] = xy_word;
    scratch.clear_packet[3] = wh_word;
    let ptr: *mut u32 = scratch.clear_packet.as_mut_ptr();
    unsafe {
        scratch.ot.insert(PREVIEW_CLEAR_SLOT, ptr, 3);
    }
}

pub(crate) fn push_cyclorama(
    scratch: &mut PreviewScratch,
    sky: psxed_project::ResolvedSkySettings,
    camera: psx_engine::WorldCamera,
) {
    if !sky.enabled {
        return;
    }
    for quad in psxed_project::generate_sky_cyclorama(sky).into_iter().rev() {
        let Some(projected) = preview_project_cyclorama_quad(quad.direction_q12, camera) else {
            continue;
        };
        if preview_cyclorama_quad_outside_screen(projected) {
            continue;
        }
        push_sky_quad_corners(scratch, projected, quad.rgb);
    }
}

pub(crate) fn push_projected_scene_sky(
    scratch: &mut PreviewScratch,
    mode: SkyMode,
    view_rotation: Mat3I16,
    material_tick: u32,
    textures: &EditorTextures,
) {
    let Some(slot) = textures.scene_sky_slot().filter(|slot| slot.mode == mode) else {
        return;
    };
    let word_capacity = match mode {
        SkyMode::QuakeLayered => psx_bsp::sky::VIEW_RAY_SKY_PACKET_WORDS,
        SkyMode::Cube => psx_bsp::sky::VIEW_RAY_CUBE_SKY_PACKET_WORDS,
        SkyMode::Off | SkyMode::Panorama => return,
    };
    let stream = {
        let mut arena = PrimitivePacketArena::new(&mut scratch.projected_sky_packets);
        let Some(mut reservation) = arena.reserve_packet_words(word_capacity) else {
            return;
        };
        let first = reservation.words_mut().as_mut_ptr();
        let submitted = unsafe {
            match mode {
                SkyMode::QuakeLayered => psx_bsp::sky::submit_view_ray_layered_sky_to_slot(
                    slot.tpage_word,
                    slot.clut_word,
                    [0, 0],
                    [128, 128],
                    view_rotation,
                    [SCREEN_W as i16, SCREEN_H as i16],
                    [SCREEN_CX as i16, SCREEN_CY as i16],
                    PROJ_H as i16,
                    material_tick,
                    PREVIEW_SKY_SLOT as u16,
                    first,
                ),
                SkyMode::Cube => psx_bsp::sky::submit_view_ray_cube_sky_to_slot(
                    slot.tpage_word,
                    slot.clut_word,
                    view_rotation,
                    [SCREEN_W as i16, SCREEN_H as i16],
                    [SCREEN_CX as i16, SCREEN_CY as i16],
                    PROJ_H as i16,
                    PREVIEW_SKY_SLOT as u16,
                    first,
                ),
                SkyMode::Off | SkyMode::Panorama => unreachable!(),
            }
        };
        let words = unsafe { submitted.next_packet.offset_from(first) }.max(0) as usize;
        let Some(stream) = reservation.commit(words, submitted.packets as usize) else {
            return;
        };
        stream
    };
    let mut ot = psx_engine::OtFrame::resume(&mut scratch.ot);
    unsafe {
        ot.add_committed_tagged_packet_stream_unchecked(stream);
    }
}

pub(crate) fn preview_project_cyclorama_quad(
    dirs: [[i16; 3]; 4],
    camera: psx_engine::WorldCamera,
) -> Option<[(i16, i16); 4]> {
    Some([
        preview_project_cyclorama_direction(dirs[0], camera)?,
        preview_project_cyclorama_direction(dirs[1], camera)?,
        preview_project_cyclorama_direction(dirs[2], camera)?,
        preview_project_cyclorama_direction(dirs[3], camera)?,
    ])
}

pub(crate) fn preview_project_cyclorama_direction(
    dir: [i16; 3],
    camera: psx_engine::WorldCamera,
) -> Option<(i16, i16)> {
    let x = i32::from(dir[0]);
    let y = i32::from(dir[1]);
    let z = i32::from(dir[2]);
    let sin_yaw = camera.sin_yaw.raw();
    let cos_yaw = camera.cos_yaw.raw();
    let sin_pitch = camera.sin_pitch.raw();
    let cos_pitch = camera.cos_pitch.raw();
    let x1 = mul_q12_i32(x, cos_yaw) - mul_q12_i32(z, sin_yaw);
    let z1 = -mul_q12_i32(x, sin_yaw) - mul_q12_i32(z, cos_yaw);
    let y2 = mul_q12_i32(y, cos_pitch) - mul_q12_i32(z1, sin_pitch);
    let z2 = mul_q12_i32(y, sin_pitch) + mul_q12_i32(z1, cos_pitch);
    if z2 <= NEAR_Z {
        return None;
    }
    let sx = SCREEN_CX + (x1 * PROJ_H) / z2;
    let sy = SCREEN_CY - (y2 * PROJ_H) / z2;
    Some((sx.clamp(-512, 831) as i16, sy.clamp(-256, 495) as i16))
}

pub(crate) fn preview_cyclorama_quad_outside_screen(points: [(i16, i16); 4]) -> bool {
    let min_x = points.iter().map(|p| p.0).min().unwrap_or(0);
    let max_x = points.iter().map(|p| p.0).max().unwrap_or(0);
    let min_y = points.iter().map(|p| p.1).min().unwrap_or(0);
    let max_y = points.iter().map(|p| p.1).max().unwrap_or(0);
    max_x < 0 || min_x >= SCREEN_W as i16 || max_y < 0 || min_y >= SCREEN_H as i16
}

pub(crate) fn push_sky_quad_corners(
    scratch: &mut PreviewScratch,
    points: [(i16, i16); 4],
    colors: [[u8; 3]; 4],
) {
    if scratch.sky_used >= scratch.sky_quads.len() {
        return;
    }
    let quad = QuadGouraud::new(
        points,
        [
            (colors[0][0], colors[0][1], colors[0][2]),
            (colors[1][0], colors[1][1], colors[1][2]),
            (colors[2][0], colors[2][1], colors[2][2]),
            (colors[3][0], colors[3][1], colors[3][2]),
        ],
    );
    let idx = scratch.sky_used;
    scratch.sky_quads[idx] = quad;
    scratch.sky_used += 1;
    let ptr: *mut QuadGouraud = &mut scratch.sky_quads[idx];
    unsafe {
        scratch
            .ot
            .insert(PREVIEW_SKY_SLOT, ptr.cast::<u32>(), QuadGouraud::WORDS);
    }
}

pub(crate) fn push_far_vista_ring(
    scratch: &mut PreviewScratch,
    camera: ViewportCameraState,
    world_camera: psx_engine::WorldCamera,
    vista: psxed_project::ResolvedFarVistaSettings,
    textures: &EditorTextures,
) {
    if !vista.enabled {
        return;
    }
    let [cam_x, cam_y, cam_z] = camera.position_i32();
    let segments = vista.segments.clamp(3, FAR_VISTA_QUAD_CAP as u8);
    let radius = vista.radius as f32;
    let base = (vista.rotation_degrees as f32).to_radians();
    let step = std::f32::consts::TAU / segments as f32;
    let y0 = cam_y.saturating_add(vista.vertical_offset);
    let y1 = y0.saturating_add(vista.height);
    for segment in 0..segments {
        let a0 = base + step * segment as f32;
        let a1 = base + step * (segment as f32 + 1.0);
        let x0 = cam_x.saturating_add((a0.sin() * radius).round() as i32);
        let z0 = cam_z.saturating_add((a0.cos() * radius).round() as i32);
        let x1 = cam_x.saturating_add((a1.sin() * radius).round() as i32);
        let z1 = cam_z.saturating_add((a1.cos() * radius).round() as i32);
        let verts = [[x0, y1, z0], [x1, y1, z1], [x1, y0, z1], [x0, y0, z0]];
        if !preview_vertices_in_front(world_camera, &verts) {
            continue;
        }
        let projected = verts.map(|vertex| gte_scene::project_vertex(world_to_view(vertex)));
        if projected.iter().any(|point| point.sz == 0) {
            continue;
        }
        if let Some((slot, texture_width, texture_height)) =
            far_vista_texture_slot(vista, segment, segments, textures)
        {
            push_far_vista_textured_quad(
                scratch,
                projected,
                slot,
                texture_width,
                texture_height,
                vista.tint,
            );
        } else {
            push_far_vista_quad(
                scratch,
                [
                    (projected[0].sx, projected[0].sy),
                    (projected[1].sx, projected[1].sy),
                    (projected[3].sx, projected[3].sy),
                    (projected[2].sx, projected[2].sy),
                ],
                vista.tint,
            );
        }
    }
}

pub(crate) fn far_vista_texture_slot(
    vista: psxed_project::ResolvedFarVistaSettings,
    segment: u8,
    segments: u8,
    textures: &EditorTextures,
) -> Option<(MaterialSlot, u8, u8)> {
    let texture_id = far_vista_texture_for_segment(vista, segment, segments)?;
    let slot = textures.slot(texture_id)?;
    Some((slot, slot.texture_width, slot.texture_height))
}

pub(crate) fn far_vista_texture_for_segment(
    vista: psxed_project::ResolvedFarVistaSettings,
    segment: u8,
    segments: u8,
) -> Option<ResourceId> {
    let assigned_panels = vista.texture_panels.iter().any(Option::is_some);
    if !assigned_panels {
        return vista.texture;
    }
    let panel_count = active_far_vista_panel_count(&vista.texture_panels, segments);
    if panel_count == 0 {
        return None;
    }
    let panel_index = ((segment as usize) * panel_count / (segments as usize).max(1))
        .min(panel_count.saturating_sub(1));
    vista.texture_panels[panel_index]
}

pub(crate) fn active_far_vista_panel_count(
    texture_panels: &[Option<ResourceId>; psxed_project::FAR_VISTA_TEXTURE_PANEL_COUNT],
    segments: u8,
) -> usize {
    texture_panels
        .iter()
        .rposition(Option::is_some)
        .map(|index| index + 1)
        .unwrap_or(0)
        .min(segments as usize)
        .min(psxed_project::FAR_VISTA_TEXTURE_PANEL_COUNT)
}

pub(crate) fn push_far_vista_textured_quad(
    scratch: &mut PreviewScratch,
    projected: [psx_gte::scene::Projected; 4],
    slot: MaterialSlot,
    texture_width: u8,
    texture_height: u8,
    tint: [u8; 3],
) {
    let material = preview_texture_material(slot, (tint[0], tint[1], tint[2]), BlendMode::Opaque);
    let uvs = [
        (0, 0),
        (texture_width.saturating_sub(1), 0),
        (
            texture_width.saturating_sub(1),
            texture_height.saturating_sub(1),
        ),
        (0, texture_height.saturating_sub(1)),
    ];
    let vista_colors = [(tint[0], tint[1], tint[2]); 3];
    let _ = push_textured_material_tri(
        scratch,
        [projected[0], projected[1], projected[2]],
        [uvs[0], uvs[1], uvs[2]],
        vista_colors,
        material,
        PREVIEW_FAR_VISTA_SLOT,
    );
    let _ = push_textured_material_tri(
        scratch,
        [projected[0], projected[2], projected[3]],
        [uvs[0], uvs[2], uvs[3]],
        vista_colors,
        material,
        PREVIEW_FAR_VISTA_SLOT,
    );
}

pub(crate) fn push_far_vista_quad(
    scratch: &mut PreviewScratch,
    verts: [(i16, i16); 4],
    color: [u8; 3],
) {
    if scratch.far_vista_used >= scratch.far_vista_quads.len() {
        return;
    }
    let idx = scratch.far_vista_used;
    scratch.far_vista_quads[idx] = QuadFlat::new(verts, color[0], color[1], color[2]);
    scratch.far_vista_used += 1;
    let ptr: *mut QuadFlat = &mut scratch.far_vista_quads[idx];
    unsafe {
        scratch
            .ot
            .insert(PREVIEW_FAR_VISTA_SLOT, ptr.cast::<u32>(), QuadFlat::WORDS);
    }
}
