//! Room-surface geometry, material shading, fog, and preview lighting.

use super::*;

/// Walk every populated sector and emit triangles for floors,
/// ceilings, and the walls on each cardinal edge. Faces whose
/// material has a texture in the editor cache draw textured;
/// everything else falls back to flat shading. Light
/// accumulation happens per-face: the shade walks every
/// point light once, attenuates by distance to the face
/// centre, and modulates the base material colour.
pub(super) fn walk_room(
    project: &ProjectDocument,
    room_id: psxed_project::NodeId,
    grid: &WorldGrid,
    y_offset: i32,
    textures: &EditorTextures,
    camera: psx_engine::WorldCamera,
    fog: PreviewFog,
    preview_backface_wireframe: bool,
    hidden_scene_nodes: &HashSet<NodeId>,
    scratch: &mut PreviewScratch,
) {
    let s = grid.sector_size;
    let lights = collect_preview_lights(project, room_id, grid, hidden_scene_nodes);
    let ambient = grid.ambient_color;
    // Stacked-floor render: shift this floor's faces to its real
    // elevation. Faces are stored floor-relative, so add the offset to
    // every height. Floor 0 / single-floor rooms pass 0 (unchanged).
    let off3 = |h: [i32; 3]| [h[0] + y_offset, h[1] + y_offset, h[2] + y_offset];
    let off4 = |h: [i32; 4]| {
        [
            h[0] + y_offset,
            h[1] + y_offset,
            h[2] + y_offset,
            h[3] + y_offset,
        ]
    };
    for x in 0..grid.width {
        for z in 0..grid.depth {
            let Some(sector) = grid.sector(x, z) else {
                continue;
            };
            // Corner heights: [NW, NE, SE, SW] in `GridHorizontalFace`.
            // World coords with +X east, +Z south, +Y up.
            // `cell_world_x/z` add `grid.origin` so cells stay at the
            // same world position when the room grows in -X / -Z.
            let x0 = grid.cell_world_x(x);
            let x1 = x0 + s;
            let z0 = grid.cell_world_z(z);
            let z1 = z0 + s;

            if let Some(floor) = sector.floor.as_ref() {
                let center = horizontal_face_center([x0, x1, z0, z1], off4(floor.heights));
                let shade_a = light_face(
                    face_shade(
                        project,
                        floor.triangle_material(0),
                        FALLBACK_FLOOR,
                        textures,
                    ),
                    center,
                    &lights,
                    ambient,
                );
                let shade_b = light_face(
                    face_shade(
                        project,
                        floor.triangle_material(1),
                        FALLBACK_FLOOR,
                        textures,
                    ),
                    center,
                    &lights,
                    ambient,
                );
                let shade_a = fog.apply_shade(shade_a, face_depth(camera, center));
                let shade_b = fog.apply_shade(shade_b, face_depth(camera, center));
                let face_ref = psxed_ui::FaceRef {
                    room: room_id,
                    sx: x,
                    sz: z,
                    kind: psxed_ui::FaceKind::Floor,
                };
                let emitted = push_horizontal_face(
                    scratch,
                    camera,
                    [x0, x1, z0, z1],
                    [
                        off3(floor.triangle_heights(0)),
                        off3(floor.triangle_heights(1)),
                    ],
                    floor.split,
                    floor.dropped_corner,
                    floor.triangle_uv(0),
                    shade_a,
                    floor.triangle_uv(1),
                    shade_b,
                    /* flip_winding */ false,
                );
                if !emitted && should_draw_culled_face_outline(preview_backface_wireframe, shade_a)
                {
                    push_culled_face_outline(grid, face_ref, shade_a, scratch);
                }
            }
            if let Some(ceiling) = sector.ceiling.as_ref() {
                let center = horizontal_face_center([x0, x1, z0, z1], off4(ceiling.heights));
                let shade_a = light_face(
                    face_shade(
                        project,
                        ceiling.triangle_material(0),
                        FALLBACK_CEILING,
                        textures,
                    ),
                    center,
                    &lights,
                    ambient,
                );
                let shade_b = light_face(
                    face_shade(
                        project,
                        ceiling.triangle_material(1),
                        FALLBACK_CEILING,
                        textures,
                    ),
                    center,
                    &lights,
                    ambient,
                );
                let shade_a = fog.apply_shade(shade_a, face_depth(camera, center));
                let shade_b = fog.apply_shade(shade_b, face_depth(camera, center));
                let face_ref = psxed_ui::FaceRef {
                    room: room_id,
                    sx: x,
                    sz: z,
                    kind: psxed_ui::FaceKind::Ceiling,
                };
                let emitted = push_horizontal_face(
                    scratch,
                    camera,
                    [x0, x1, z0, z1],
                    [
                        off3(ceiling.triangle_heights(0)),
                        off3(ceiling.triangle_heights(1)),
                    ],
                    ceiling.split,
                    ceiling.dropped_corner,
                    ceiling.triangle_uv(0),
                    shade_a,
                    ceiling.triangle_uv(1),
                    shade_b,
                    // Ceiling normal points down; flipping the winding
                    // keeps backface-cullers happy and pins the inside
                    // surface as the visible side once we add culling.
                    /* flip_winding */
                    true,
                );
                if !emitted && should_draw_culled_face_outline(preview_backface_wireframe, shade_a)
                {
                    push_culled_face_outline(grid, face_ref, shade_a, scratch);
                }
            }
            for direction in GridDirection::ALL {
                let edge = WallEdge::from_direction(direction);
                for (stack_idx, face) in sector.walls.get(direction).iter().enumerate() {
                    let center = wall_face_center([x0, x1, z0, z1], edge, off4(face.heights));
                    let shade = light_face(
                        face_shade(project, face.material, FALLBACK_WALL, textures),
                        center,
                        &lights,
                        ambient,
                    );
                    let shade = fog.apply_shade(shade, face_depth(camera, center));
                    let face_ref = psxed_ui::FaceRef {
                        room: room_id,
                        sx: x,
                        sz: z,
                        kind: psxed_ui::FaceKind::Wall {
                            dir: direction,
                            stack: stack_idx as u8,
                        },
                    };
                    let emitted = push_wall_face(
                        scratch,
                        camera,
                        [x0, x1, z0, z1],
                        edge,
                        off4(face.heights),
                        face.dropped_corner,
                        face.uv,
                        shade,
                        [camera.position.x, camera.position.y, camera.position.z],
                    );
                    if !emitted
                        && should_draw_culled_face_outline(preview_backface_wireframe, shade)
                    {
                        push_culled_face_outline(grid, face_ref, shade, scratch);
                    }
                }
            }

            if scratch.used >= TRI_CAP || scratch.tex_used >= TRI_CAP {
                return;
            }
        }
    }
}

/// Per-face render description: either a texture sample with a
/// per-material tint, or a flat RGB. Resolved up-front so each
/// face's tri emit doesn't re-walk the resource table.
#[derive(Copy, Clone)]
pub(super) enum FaceShade {
    Flat {
        rgb: (u8, u8, u8),
        sidedness: psxed_project::MaterialFaceSidedness,
    },
    Textured {
        slot: MaterialSlot,
        tint: (u8, u8, u8),
        blend_mode: BlendMode,
        sidedness: psxed_project::MaterialFaceSidedness,
    },
}

impl FaceShade {
    pub(super) fn sidedness(self) -> psxed_project::MaterialFaceSidedness {
        match self {
            Self::Flat { sidedness, .. } | Self::Textured { sidedness, .. } => sidedness,
        }
    }

    pub(super) fn with_sidedness(self, sidedness: psxed_project::MaterialFaceSidedness) -> Self {
        match self {
            Self::Flat { rgb, .. } => Self::Flat { rgb, sidedness },
            Self::Textured {
                slot,
                tint,
                blend_mode,
                ..
            } => Self::Textured {
                slot,
                tint,
                blend_mode,
                sidedness,
            },
        }
    }
}

pub(super) fn face_shade(
    project: &ProjectDocument,
    material: Option<ResourceId>,
    fallback: (u8, u8, u8),
    textures: &EditorTextures,
) -> FaceShade {
    let tint = material_color(project, material, fallback);
    let sidedness = material_sidedness(project, material);
    if let Some(id) = material {
        if let Some(slot) = textures.slot(id) {
            return FaceShade::Textured {
                slot,
                tint: material_texture_tint(project, id),
                blend_mode: material_blend_mode(project, Some(id)),
                sidedness,
            };
        }
    }
    FaceShade::Flat {
        rgb: tint,
        sidedness,
    }
}

pub(super) fn push_culled_face_outline(
    grid: &WorldGrid,
    face: psxed_ui::FaceRef,
    shade: FaceShade,
    scratch: &mut PreviewScratch,
) {
    if !should_draw_culled_face_outline(true, shade) {
        return;
    }
    push_face_outline(grid, face, FACE_OUTLINE_CULLED, scratch);
}

pub(super) fn should_draw_culled_face_outline(
    preview_backface_wireframe: bool,
    shade: FaceShade,
) -> bool {
    preview_backface_wireframe
        && !matches!(
            shade.sidedness(),
            psxed_project::MaterialFaceSidedness::Both
        )
}

pub(super) fn material_texture_tint(
    project: &ProjectDocument,
    material: ResourceId,
) -> (u8, u8, u8) {
    project
        .resource(material)
        .and_then(|resource| match &resource.data {
            ResourceData::Material(material) => Some(material.tint),
            _ => None,
        })
        .map(|[r, g, b]| (r, g, b))
        .unwrap_or((0x80, 0x80, 0x80))
}

pub(super) fn material_blend_mode(
    project: &ProjectDocument,
    material: Option<ResourceId>,
) -> BlendMode {
    material
        .and_then(|id| project.resource(id))
        .and_then(|resource| match &resource.data {
            ResourceData::Material(material) => Some(psx_blend_mode(material.blend_mode)),
            _ => None,
        })
        .unwrap_or(BlendMode::Opaque)
}

pub(super) fn psx_blend_mode(mode: psxed_project::PsxBlendMode) -> BlendMode {
    match mode {
        psxed_project::PsxBlendMode::Opaque => BlendMode::Opaque,
        psxed_project::PsxBlendMode::Average => BlendMode::Average,
        psxed_project::PsxBlendMode::Add => BlendMode::Add,
        psxed_project::PsxBlendMode::Subtract => BlendMode::Subtract,
        psxed_project::PsxBlendMode::AddQuarter => BlendMode::AddQuarter,
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) struct PreviewFog {
    pub(super) enabled: bool,
    pub(super) rgb: (u8, u8, u8),
    pub(super) near: i32,
    pub(super) far: i32,
}

impl PreviewFog {
    pub(super) fn from_grid(grid: &WorldGrid, preview_enabled: bool) -> Self {
        Self {
            enabled: preview_enabled && grid.fog_enabled,
            rgb: (grid.fog_color[0], grid.fog_color[1], grid.fog_color[2]),
            near: grid.fog_near,
            far: grid.fog_far,
        }
    }

    pub(super) fn apply_shade(self, shade: FaceShade, depth: i32) -> FaceShade {
        match shade {
            FaceShade::Flat { rgb, sidedness } => FaceShade::Flat {
                rgb: self.apply_rgb(rgb, depth),
                sidedness,
            },
            FaceShade::Textured {
                slot,
                tint,
                blend_mode,
                sidedness,
            } => FaceShade::Textured {
                slot,
                tint: self.apply_rgb(tint, depth),
                blend_mode,
                sidedness,
            },
        }
    }

    pub(super) fn apply_rgb(self, rgb: (u8, u8, u8), depth: i32) -> (u8, u8, u8) {
        if !self.enabled || self.far <= self.near || depth <= self.near {
            return rgb;
        }
        let weight =
            (((depth - self.near).saturating_mul(256)) / (self.far - self.near)).clamp(0, 256);
        let keep = 256 - weight;
        (
            fog_blend_channel(rgb.0, self.rgb.0, keep, weight),
            fog_blend_channel(rgb.1, self.rgb.1, keep, weight),
            fog_blend_channel(rgb.2, self.rgb.2, keep, weight),
        )
    }
}

pub(super) fn fog_blend_channel(src: u8, fog: u8, keep: i32, weight: i32) -> u8 {
    (((src as i32) * keep + (fog as i32) * weight) / 256).clamp(0, 255) as u8
}

pub(super) fn face_depth(camera: psx_engine::WorldCamera, center: [i32; 3]) -> i32 {
    camera
        .view_vertex(psx_engine::WorldVertex::new(
            center[0], center[1], center[2],
        ))
        .z
}

pub(super) fn preview_vertices_in_front(
    camera: psx_engine::WorldCamera,
    verts: &[[i32; 3]],
) -> bool {
    verts.iter().all(|v| {
        camera
            .view_vertex(psx_engine::WorldVertex::new(v[0], v[1], v[2]))
            .z
            >= camera.projection.near_z
    })
}

/// Walk every PointLight whose enclosing room is the active grid
/// and pre-multiply its
/// colour×intensity_q8. Lights authored outside any Room (no
/// enclosing parent) are skipped silently -- the cooker warns
/// about those, the preview just doesn't render them.
pub(super) fn collect_preview_lights(
    project: &ProjectDocument,
    room_id: psxed_project::NodeId,
    grid: &WorldGrid,
    hidden_scene_nodes: &HashSet<NodeId>,
) -> Vec<psx_engine::PointLightSample> {
    let scene = project.active_scene();
    let mut out = Vec::new();
    for light in preview_lights(scene, hidden_scene_nodes) {
        // Filter by enclosing Room -- a light authored under
        // some other Room must not bleed into this one.
        if !is_descendant_of_room(scene, light.host_id, room_id) {
            continue;
        }
        push_preview_light_sample(
            &mut out,
            grid,
            &light.transform,
            light.color,
            light.intensity,
            light.radius,
        );
    }
    out
}

/// Scene-wide light samples for world-space (BSP) geometry: no room
/// filter, positions are raw world units, and the radius scales by the
/// World sector size, matching the brush bake's `scene_lights`.
/// Scene lights in the cook's bake form (f64 world units), so the
/// preview's per-vertex brush lighting runs the exact Draft-bake
/// math instead of an integer approximation of it.
pub(super) fn collect_bsp_preview_bake_lights(
    project: &ProjectDocument,
    hidden_scene_nodes: &HashSet<NodeId>,
) -> Vec<psxed_project::brush_light::BrushPointLight> {
    let scene = project.active_scene();
    let radius_units = scene
        .world_sector_size_for_node(scene.root)
        .unwrap_or(1024)
        .max(1) as f64;
    let mut out = Vec::new();
    for light in preview_lights(scene, hidden_scene_nodes) {
        if light.radius <= 0.0 || !light.intensity.is_finite() || light.intensity < 0.0 {
            continue;
        }
        out.push(psxed_project::brush_light::BrushPointLight {
            position: light.transform.translation.map(f64::from),
            radius: f64::from(light.radius) * radius_units,
            intensity_q8: (light.intensity * 256.0).clamp(0.0, f32::from(u16::MAX)) as u16,
            color: light.color,
        });
    }
    out
}

pub(super) fn collect_bsp_preview_lights(
    project: &ProjectDocument,
    hidden_scene_nodes: &HashSet<NodeId>,
) -> Vec<psx_engine::PointLightSample> {
    let scene = project.active_scene();
    let radius_units = scene
        .world_sector_size_for_node(scene.root)
        .unwrap_or(1024)
        .max(1) as f32;
    let mut out = Vec::new();
    for light in preview_lights(scene, hidden_scene_nodes) {
        if light.radius <= 0.0 || !light.intensity.is_finite() || light.intensity < 0.0 {
            continue;
        }
        let world = light
            .transform
            .translation
            .map(|value| value.round() as i32);
        let intensity_q8 = (light.intensity * 256.0).clamp(0.0, u16::MAX as f32) as u32;
        out.push(psx_engine::PointLightSample::from_rgb_intensity(
            [world[0], world[1], world[2]],
            (light.radius * radius_units) as i32,
            psx_engine::Rgb8::from_array(light.color),
            psx_engine::Q8::from_raw(intensity_q8),
        ));
    }
    out
}

pub(super) fn push_preview_light_sample(
    out: &mut Vec<psx_engine::PointLightSample>,
    grid: &WorldGrid,
    transform: &Transform3,
    color: [u8; 3],
    intensity: f32,
    radius: f32,
) {
    if radius <= 0.0 || !intensity.is_finite() || intensity < 0.0 {
        return;
    }
    let world = node_room_local_origin(grid, transform);
    // Editor `radius` is in sector units; convert to
    // engine units once here so the per-face attenuation
    // math stays in world space.
    let radius_engine = spatial::light_radius_engine_units(grid, radius);
    // Pre-multiply colour × intensity into u32 channels;
    // intensity scaled by 256 (Q8.8) keeps the per-face
    // accumulator in integer math.
    let intensity_q8 = (intensity * 256.0).clamp(0.0, u16::MAX as f32) as u32;
    out.push(psx_engine::PointLightSample::from_rgb_intensity(
        [world.x, world.y, world.z],
        radius_engine,
        psx_engine::Rgb8::from_array(color),
        psx_engine::Q8::from_raw(intensity_q8),
    ));
}

#[derive(Clone, Copy)]
pub(super) struct PreviewLightMeta {
    pub(super) host_id: NodeId,
    pub(super) transform: Transform3,
    pub(super) color: [u8; 3],
    pub(super) intensity: f32,
    pub(super) radius: f32,
}

pub(super) fn preview_lights(
    scene: &Scene,
    hidden_scene_nodes: &HashSet<NodeId>,
) -> Vec<PreviewLightMeta> {
    let mut out = Vec::new();
    for node in scene.nodes() {
        if scene_node_hidden(scene, hidden_scene_nodes, node.id) {
            continue;
        }
        let NodeKind::PointLight {
            color,
            intensity,
            radius,
        } = &node.kind
        else {
            continue;
        };
        out.push(PreviewLightMeta {
            host_id: node.id,
            transform: node.transform,
            color: *color,
            intensity: *intensity,
            radius: *radius,
        });
    }
    out
}

/// Centre of a horizontal face (floor / ceiling) -- average X /
/// Z of the bounds, mean of the four corner heights for Y.
pub(super) fn horizontal_face_center(bounds: [i32; 4], heights: [i32; 4]) -> [i32; 3] {
    let [x0, x1, z0, z1] = bounds;
    let cx = (x0 + x1) / 2;
    let cz = (z0 + z1) / 2;
    let cy = (heights[0] as i64 + heights[1] as i64 + heights[2] as i64 + heights[3] as i64) / 4;
    [cx, cy as i32, cz]
}

/// Centre of a wall face -- midpoint of the wall's bottom edge
/// in X/Z, midpoint of the four corner heights for Y. Wall
/// edges run along one of the cell's cardinal or diagonal edges; the
/// `WallEdge` picks which.
pub(super) fn wall_face_center(bounds: [i32; 4], edge: WallEdge, heights: [i32; 4]) -> [i32; 3] {
    let [x0, x1, z0, z1] = bounds;
    let (cx, cz) = match edge {
        WallEdge::North => ((x0 + x1) / 2, z1),
        WallEdge::East => (x1, (z0 + z1) / 2),
        WallEdge::South => ((x0 + x1) / 2, z0),
        WallEdge::West => (x0, (z0 + z1) / 2),
        WallEdge::NorthWestSouthEast | WallEdge::NorthEastSouthWest => {
            ((x0 + x1) / 2, (z0 + z1) / 2)
        }
    };
    let cy = (heights[0] as i64 + heights[1] as i64 + heights[2] as i64 + heights[3] as i64) / 4;
    [cx, cy as i32, cz]
}

/// Apply per-face lighting: ambient + linear-attenuation sum
/// of every point light whose radius covers `face_center`.
/// Final RGB clamps to 8 bits and modulates the input shade.
/// Lighting convention (PSX-neutral):
///
/// * `light_rgb` is in `0..=255` per channel.
/// * `128` = neutral -- material renders at its base brightness.
/// * `0`   = pitch black.
/// * `255` = saturated overbright (clamped at the modulate
///   step).
///
/// Both the editor preview and the runtime use this scale.
/// Final colour = `base * light_rgb / 128`, clamped to `255`.
pub(super) fn light_face(
    base: FaceShade,
    face_center: [i32; 3],
    lights: &[psx_engine::PointLightSample],
    ambient: [u8; 3],
) -> FaceShade {
    let base_color = match base {
        FaceShade::Flat { rgb, .. } => rgb,
        FaceShade::Textured { tint, .. } => tint,
    };
    let (r, g, b) = psx_engine::shade_material_tint_with_lights(
        psx_engine::MaterialTint::from_tuple(base_color),
        face_center,
        psx_engine::Rgb8::from_array(ambient),
        lights.iter().copied(),
    )
    .to_tuple();
    match base {
        FaceShade::Flat { sidedness, .. } => FaceShade::Flat {
            rgb: (r, g, b),
            sidedness,
        },
        FaceShade::Textured {
            slot,
            blend_mode,
            sidedness,
            ..
        } => FaceShade::Textured {
            slot,
            tint: (r, g, b),
            blend_mode,
            sidedness,
        },
    }
}

/// Project the two triangles of a sector-aligned horizontal face
/// and emit one or two triangles. `triangle_heights` are in each
/// triangle's split-corner order.
/// `flip_winding=true` reverses the vertex order for ceilings.
/// `dropped_corner=Some(c)` makes the face a triangle: the half
/// containing `c` is skipped (`split` must already be on the
/// diagonal that keeps the other half alive -- `Corner::surviving_split`
/// enforces this at the data layer).
#[allow(clippy::too_many_arguments)]
pub(super) fn push_horizontal_face(
    scratch: &mut PreviewScratch,
    camera: psx_engine::WorldCamera,
    bounds: [i32; 4],
    triangle_heights: [[i32; 3]; 2],
    split: GridSplit,
    dropped_corner: Option<psxed_project::Corner>,
    uv_transform_a: GridUvTransform,
    shade_a: FaceShade,
    uv_transform_b: GridUvTransform,
    shade_b: FaceShade,
    flip_winding: bool,
) -> bool {
    let a_uvs = material_sized_uvs(
        shade_a,
        uv_transform_a.apply_to_quad(textured_base_uvs(shade_a, PREVIEW_FLOOR_UVS)),
    );
    let b_uvs = material_sized_uvs(
        shade_b,
        uv_transform_b.apply_to_quad(textured_base_uvs(shade_b, PREVIEW_FLOOR_UVS)),
    );

    let tri_a_corners = psxed_project::horizontal_triangle_corners(split, 0);
    let tri_b_corners = psxed_project::horizontal_triangle_corners(split, 1);
    let tri_a_world = horizontal_triangle_world_points(bounds, tri_a_corners, triangle_heights[0]);
    let tri_b_world = horizontal_triangle_world_points(bounds, tri_b_corners, triangle_heights[1]);
    let tri_a = (
        tri_a_world,
        select_uv_corners(a_uvs, tri_a_corners),
        tri_a_corners,
    );
    let tri_b = (
        tri_b_world,
        select_uv_corners(b_uvs, tri_b_corners),
        tri_b_corners,
    );

    let triangle_contains =
        |members: [Corner; 3], target: Corner| -> bool { members.contains(&target) };
    let emit_triangle = |scratch: &mut PreviewScratch,
                         verts: [[i32; 3]; 3],
                         uvs: [(u8, u8); 3],
                         shade: FaceShade| {
        if !preview_vertices_in_front(camera, &verts) {
            return false;
        }
        let verts = [
            gte_scene::project_vertex(world_to_view(verts[0])),
            gte_scene::project_vertex(world_to_view(verts[1])),
            gte_scene::project_vertex(world_to_view(verts[2])),
        ];
        if flip_winding {
            // Ceilings: forward `[0, 1, 2]` walk (CW from above
            // = CCW from below) so the inward normal points down.
            emit_face_tri(scratch, verts, uvs, shade)
        } else {
            // Floors: reverse to `[0, 2, 1]` (CCW from above),
            // matching the legacy non-flip winding.
            emit_face_tri(
                scratch,
                [verts[0], verts[2], verts[1]],
                [uvs[0], uvs[2], uvs[1]],
                shade,
            )
        }
    };

    let skip_a = dropped_corner
        .map(|c| triangle_contains(tri_a.2, c))
        .unwrap_or(false);
    let skip_b = dropped_corner
        .map(|c| triangle_contains(tri_b.2, c))
        .unwrap_or(false);
    let mut emitted = false;
    if !skip_a {
        emitted |= emit_triangle(scratch, tri_a.0, tri_a.1, shade_a);
    }
    if !skip_b {
        emitted |= emit_triangle(scratch, tri_b.0, tri_b.1, shade_b);
    }
    emitted
}

pub(super) fn horizontal_triangle_world_points(
    bounds: [i32; 4],
    corners: [Corner; 3],
    heights: [i32; 3],
) -> [[i32; 3]; 3] {
    [
        horizontal_corner_world_point(bounds, corners[0], heights[0]),
        horizontal_corner_world_point(bounds, corners[1], heights[1]),
        horizontal_corner_world_point(bounds, corners[2], heights[2]),
    ]
}

pub(super) fn horizontal_corner_world_point(
    bounds: [i32; 4],
    corner: Corner,
    height: i32,
) -> [i32; 3] {
    let [x0, x1, z0, z1] = bounds;
    match corner {
        Corner::NW => [x0, height, z1],
        Corner::NE => [x1, height, z1],
        Corner::SE => [x1, height, z0],
        Corner::SW => [x0, height, z0],
    }
}

/// Which edge of the sector this wall sits on. The renderer needs
/// the four corner positions in a consistent order so heights[bl,
/// br, tr, tl] line up with the right world-space corners.
#[derive(Copy, Clone, Debug)]
pub(super) enum WallEdge {
    North,
    East,
    South,
    West,
    NorthWestSouthEast,
    NorthEastSouthWest,
}

impl WallEdge {
    const fn from_direction(direction: GridDirection) -> Self {
        match direction {
            GridDirection::North => Self::North,
            GridDirection::East => Self::East,
            GridDirection::South => Self::South,
            GridDirection::West => Self::West,
            GridDirection::NorthWestSouthEast => Self::NorthWestSouthEast,
            GridDirection::NorthEastSouthWest => Self::NorthEastSouthWest,
        }
    }
}

pub(super) fn wall_side_visible(
    sidedness: psxed_project::MaterialFaceSidedness,
    bounds: [i32; 4],
    edge: WallEdge,
    camera_position: [i32; 3],
) -> bool {
    let sidedness = wall_material_sidedness_for_edge(sidedness, edge);
    let [x0, x1, z0, z1] = bounds;
    let [cam_x, _, cam_z] = camera_position;
    let inside_distance = match edge {
        WallEdge::North => z1.saturating_sub(cam_z),
        WallEdge::East => x1.saturating_sub(cam_x),
        WallEdge::South => cam_z.saturating_sub(z0),
        WallEdge::West => cam_x.saturating_sub(x0),
        WallEdge::NorthWestSouthEast | WallEdge::NorthEastSouthWest => 0,
    };
    match sidedness {
        psxed_project::MaterialFaceSidedness::Both => true,
        psxed_project::MaterialFaceSidedness::Back => inside_distance >= 0,
        psxed_project::MaterialFaceSidedness::Front => inside_distance <= 0,
    }
}

pub(super) fn wall_material_sidedness(
    sidedness: psxed_project::MaterialFaceSidedness,
) -> psxed_project::MaterialFaceSidedness {
    match sidedness {
        psxed_project::MaterialFaceSidedness::Front => psxed_project::MaterialFaceSidedness::Back,
        psxed_project::MaterialFaceSidedness::Back => psxed_project::MaterialFaceSidedness::Front,
        psxed_project::MaterialFaceSidedness::Both => psxed_project::MaterialFaceSidedness::Both,
    }
}

pub(super) fn wall_material_sidedness_for_edge(
    sidedness: psxed_project::MaterialFaceSidedness,
    edge: WallEdge,
) -> psxed_project::MaterialFaceSidedness {
    match edge {
        WallEdge::NorthWestSouthEast | WallEdge::NorthEastSouthWest => {
            psxed_project::MaterialFaceSidedness::Both
        }
        _ => wall_material_sidedness(sidedness),
    }
}

/// Build the four world-space corners of a wall face on `edge`
/// and emit one or two triangles. `heights` is the
/// `GridVerticalFace` `[bl, br, tr, tl]` quad. `dropped_corner`
/// makes the face a triangle: BR / TL skip the second triangle
/// of the BL-TR diagonal split; BL / TR fall through to the
/// other diagonal.
pub(super) fn push_wall_face(
    scratch: &mut PreviewScratch,
    camera: psx_engine::WorldCamera,
    bounds: [i32; 4],
    edge: WallEdge,
    heights: [i32; 4],
    dropped_corner: Option<psxed_project::WallCorner>,
    uv_transform: GridUvTransform,
    shade: FaceShade,
    camera_position: [i32; 3],
) -> bool {
    if !wall_side_visible(shade.sidedness(), bounds, edge, camera_position) {
        return false;
    }
    let render_shade = shade.with_sidedness(psxed_project::MaterialFaceSidedness::Both);
    let [x0, x1, z0, z1] = bounds;
    // For each edge, "left" and "right" are picked so an observer
    // standing inside the sector sees the wall the right way up.
    let (bl_xy, br_xy, tr_xy, tl_xy) = match edge {
        WallEdge::North => ((x0, z1), (x1, z1), (x1, z1), (x0, z1)),
        WallEdge::East => ((x1, z1), (x1, z0), (x1, z0), (x1, z1)),
        WallEdge::South => ((x1, z0), (x0, z0), (x0, z0), (x1, z0)),
        WallEdge::West => ((x0, z0), (x0, z1), (x0, z1), (x0, z0)),
        WallEdge::NorthWestSouthEast => ((x0, z1), (x1, z0), (x1, z0), (x0, z1)),
        WallEdge::NorthEastSouthWest => ((x1, z1), (x0, z0), (x0, z0), (x1, z1)),
    };
    let w_bl = [bl_xy.0, heights[0], bl_xy.1];
    let w_br = [br_xy.0, heights[1], br_xy.1];
    let w_tr = [tr_xy.0, heights[2], tr_xy.1];
    let w_tl = [tl_xy.0, heights[3], tl_xy.1];
    if !preview_vertices_in_front(camera, &[w_bl, w_br, w_tr, w_tl]) {
        return false;
    }
    let p_bl = gte_scene::project_vertex(world_to_view(w_bl));
    let p_br = gte_scene::project_vertex(world_to_view(w_br));
    let p_tr = gte_scene::project_vertex(world_to_view(w_tr));
    let p_tl = gte_scene::project_vertex(world_to_view(w_tl));
    let uvs = material_sized_uvs(
        shade,
        uv_transform.apply_to_quad(textured_base_uvs(shade, PREVIEW_WALL_UVS)),
    );

    let projected = [p_bl, p_br, p_tr, p_tl];
    let shape = dropped_corner.map(psxed_project::wall_shape_for_dropped_corner);
    let make_triangle = |members: [WallCorner; 3]| {
        let members = [members[0], members[2], members[1]];
        (
            select_projected_wall_corners(projected, members),
            select_uv_wall_corners(uvs, members),
            members,
        )
    };
    let (tri_a, tri_b) = if let Some(shape) = shape {
        let members = psxed_project::wall_shape_triangle_corners(shape).unwrap_or(
            psxed_project::wall_triangle_corners(GridSplit::NorthWestSouthEast, 0),
        );
        (make_triangle(members), make_triangle(members))
    } else {
        (
            make_triangle(psxed_project::wall_triangle_corners(
                GridSplit::NorthWestSouthEast,
                0,
            )),
            make_triangle(psxed_project::wall_triangle_corners(
                GridSplit::NorthWestSouthEast,
                1,
            )),
        )
    };

    let skip =
        |members: [WallCorner; 3]| -> bool { dropped_corner.is_some_and(|c| members.contains(&c)) };
    // Endpoint order keeps wall UVs upright. Winding is the
    // separate concern: the authored wall back side faces the
    // owning cell/interior, while wall materials swap Front/Back so
    // authors can use front-sided materials for interior walls.
    let flip_winding = !matches!(
        wall_material_sidedness_for_edge(shade.sidedness(), edge),
        psxed_project::MaterialFaceSidedness::Back
    );
    let emit_wall_triangle = |scratch: &mut PreviewScratch,
                              verts: [psx_gte::scene::Projected; 3],
                              uvs: [(u8, u8); 3]| {
        if flip_winding {
            emit_face_tri(
                scratch,
                [verts[0], verts[2], verts[1]],
                [uvs[0], uvs[2], uvs[1]],
                render_shade,
            )
        } else {
            emit_face_tri(scratch, verts, uvs, render_shade)
        }
    };
    let mut emitted = false;
    if shape.is_some() || !skip(tri_a.2) {
        emitted |= emit_wall_triangle(scratch, tri_a.0, tri_a.1);
    }
    if shape.is_none() && !skip(tri_b.2) {
        emitted |= emit_wall_triangle(scratch, tri_b.0, tri_b.1);
    }
    emitted
}

pub(super) fn textured_base_uvs(shade: FaceShade, textured_uvs: [(u8, u8); 4]) -> [(u8, u8); 4] {
    if matches!(shade, FaceShade::Textured { .. }) {
        textured_uvs
    } else {
        [(0, 0); 4]
    }
}

pub(super) fn material_sized_uvs(shade: FaceShade, uvs: [(u8, u8); 4]) -> [(u8, u8); 4] {
    match shade {
        FaceShade::Textured { slot, .. } => [
            material_sized_uv(slot, uvs[0]),
            material_sized_uv(slot, uvs[1]),
            material_sized_uv(slot, uvs[2]),
            material_sized_uv(slot, uvs[3]),
        ],
        FaceShade::Flat { .. } => uvs,
    }
}

pub(super) fn material_sized_uv(slot: MaterialSlot, (u, v): (u8, u8)) -> (u8, u8) {
    (
        material_sized_uv_component(u, slot.texture_width),
        material_sized_uv_component(v, slot.texture_height),
    )
}

pub(super) fn material_sized_uv_component(value: u8, size: u8) -> u8 {
    let size = if size == 0 || size > GRID_TILE_UV {
        GRID_TILE_UV
    } else {
        size
    };
    ((u16::from(value) * u16::from(size)) / u16::from(GRID_TILE_UV)).min(u16::from(u8::MAX)) as u8
}

/// Resolve a full material texture pass for editor-only prop/water geometry.
/// Room geometry has its own authored UV transforms; standalone surfaces use
/// this helper so their preview still consumes the same animation recipe as
/// the native runtime.
pub(super) fn animated_material_quad_uvs(
    project: &ProjectDocument,
    material_id: ResourceId,
    slot: MaterialSlot,
    tick: u32,
) -> [(u8, u8); 4] {
    let animation = project
        .resource(material_id)
        .and_then(|resource| match &resource.data {
            ResourceData::Material(material) => Some(material.animation),
            _ => None,
        })
        .unwrap_or_default();
    let (frame_width, frame_height, animation) = match animation.mode {
        psxed_project::MaterialAnimationMode::Static => (
            slot.texture_width,
            slot.texture_height,
            psx_engine::WorldMaterialAnimation::Static,
        ),
        psxed_project::MaterialAnimationMode::UvScroll if animation.uv_scroll.enabled => (
            slot.texture_width,
            slot.texture_height,
            psx_engine::WorldMaterialAnimation::UvScroll {
                speed_u_q8: animation.uv_scroll.speed_u_q8,
                speed_v_q8: animation.uv_scroll.speed_v_q8,
                phase_u: animation.uv_scroll.phase_u,
                phase_v: animation.uv_scroll.phase_v,
            },
        ),
        psxed_project::MaterialAnimationMode::UvScroll => (
            slot.texture_width,
            slot.texture_height,
            psx_engine::WorldMaterialAnimation::Static,
        ),
        psxed_project::MaterialAnimationMode::Flipbook => {
            let flipbook = animation.flipbook.normalized();
            (
                (slot.texture_width / flipbook.columns).max(1),
                (slot.texture_height / flipbook.rows).max(1),
                psx_engine::WorldMaterialAnimation::Flipbook {
                    columns: flipbook.columns,
                    frame_count: flipbook.frame_count,
                    ticks_per_frame: flipbook.ticks_per_frame,
                    phase: flipbook.phase,
                },
            )
        }
    };
    let (offset_u, offset_v) = animation.uv_offset(tick, 60, frame_width, frame_height);
    let u_max = frame_width.saturating_sub(1);
    let v_max = frame_height.saturating_sub(1);
    [
        (offset_u, offset_v),
        (u_max.wrapping_add(offset_u), offset_v),
        (u_max.wrapping_add(offset_u), v_max.wrapping_add(offset_v)),
        (offset_u, v_max.wrapping_add(offset_v)),
    ]
}

pub(super) fn select_uv_corners(uvs: [(u8, u8); 4], corners: [Corner; 3]) -> [(u8, u8); 3] {
    [
        uvs[corners[0].idx()],
        uvs[corners[1].idx()],
        uvs[corners[2].idx()],
    ]
}

pub(super) fn select_projected_wall_corners(
    projected: [psx_gte::scene::Projected; 4],
    corners: [WallCorner; 3],
) -> [psx_gte::scene::Projected; 3] {
    [
        projected[corners[0].idx()],
        projected[corners[1].idx()],
        projected[corners[2].idx()],
    ]
}

pub(super) fn select_uv_wall_corners(
    uvs: [(u8, u8); 4],
    corners: [WallCorner; 3],
) -> [(u8, u8); 3] {
    [
        uvs[corners[0].idx()],
        uvs[corners[1].idx()],
        uvs[corners[2].idx()],
    ]
}

pub(super) const FALLBACK_FLOOR: (u8, u8, u8) = (0xB0, 0xA0, 0x88);
pub(super) const FALLBACK_WALL: (u8, u8, u8) = (0x88, 0x70, 0x58);
pub(super) const FALLBACK_CEILING: (u8, u8, u8) = (0x60, 0x60, 0x70);

/// Pick the GP0 RGB triple to paint a face with.
///
/// Authored `MaterialResource::tint` defaults to PSX-neutral
/// `(0x80, 0x80, 0x80)` because that's the right value when sampling
/// a textured polygon (output = texel × tint / 128). For the editor's
/// pre-textured flat-shaded preview that means every face renders the
/// same dull grey -- useless for distinguishing materials. Mirror the
/// 2D viewport's approach: derive a colour from the material's name
/// so a project's "Floor Material" / "Brick Material" / "Glass" all
/// land at distinct, recognisable hues until real texturing arrives.
pub(super) fn material_color(
    project: &ProjectDocument,
    material: Option<ResourceId>,
    fallback: (u8, u8, u8),
) -> (u8, u8, u8) {
    let Some(id) = material else {
        return fallback;
    };
    let Some(resource) = project.resource(id) else {
        return fallback;
    };
    let name = resource.name.as_bytes();
    let contains = |needle: &[u8]| {
        name.windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
    };
    if contains(b"brick") {
        (0xC8, 0x70, 0x40)
    } else if contains(b"floor") || contains(b"stone") {
        (0xB6, 0xAC, 0x96)
    } else if contains(b"glass") {
        (0x70, 0xA8, 0xC0)
    } else if contains(b"wood") {
        (0x90, 0x60, 0x40)
    } else if contains(b"metal") {
        (0x90, 0x96, 0x9A)
    } else if let ResourceData::Material(mat) = &resource.data {
        // Author actually tinted the material away from neutral -- use
        // the tint directly. The mid-grey default falls through to
        // the role-specific fallback below.
        if mat.tint != [0x80, 0x80, 0x80] {
            let [r, g, b] = mat.tint;
            (r, g, b)
        } else {
            fallback
        }
    } else {
        fallback
    }
}

pub(super) fn material_sidedness(
    project: &ProjectDocument,
    material: Option<ResourceId>,
) -> psxed_project::MaterialFaceSidedness {
    material
        .and_then(|id| project.resource(id))
        .and_then(|resource| match &resource.data {
            ResourceData::Material(material) => Some(material.sidedness()),
            _ => None,
        })
        .unwrap_or_default()
}
