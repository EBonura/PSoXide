//! Editor-only viewport overlay drawing: selection, paint ghosts, bounds, and gizmos.

use super::*;

/// Draw a horizontal radius ring plus a bulb icon for every
/// PointLight in the scene. The bulb
/// replaces the old coloured square marker so lights read as editor
/// light gizmos rather than generic entities.
pub(super) fn walk_light_gizmos(
    project: &ProjectDocument,
    room_id: NodeId,
    grid: &WorldGrid,
    floor_index: usize,
    y_offset: i32,
    hidden_scene_nodes: &HashSet<NodeId>,
    selected: psxed_project::NodeId,
    hovered: Option<psxed_project::NodeId>,
    scratch: &mut PreviewScratch,
) {
    let scene = project.active_scene();
    for light in preview_lights(scene, hidden_scene_nodes) {
        if !is_descendant_of_room(scene, light.host_id, room_id) {
            continue;
        }
        if node_enclosing_floor(scene, light.host_id) != floor_index {
            continue;
        }
        let center = node_room_local_origin(grid, &light.transform);
        let center_world = [center.x, center.y + y_offset, center.z];
        let is_selected = preview_reference_selected(selected, light.host_id, None, None, None);
        let is_hovered = hovered
            .is_some_and(|id| preview_reference_selected(id, light.host_id, None, None, None));
        let style = if is_selected {
            FaceOutlineStyle {
                rgb: (0xFF, 0xE0, 0x80),
                thickness_px: EDITOR_PREVIEW_SELECTED_STROKE_WIDTH,
            }
        } else if is_hovered {
            FaceOutlineStyle {
                rgb: (0xFF, 0xF0, 0x90),
                thickness_px: EDITOR_PREVIEW_HOVER_STROKE_WIDTH,
            }
        } else {
            // Tint the unlit ring toward the authored colour
            // so multiple lights in a room read at a glance.
            FaceOutlineStyle {
                rgb: (
                    light.color[0].max(0x40),
                    light.color[1].max(0x40),
                    light.color[2].max(0x40),
                ),
                thickness_px: EDITOR_PREVIEW_HOVER_STROKE_WIDTH,
            }
        };
        // Light radius is authored in *sector units*; scale to
        // engine units so the ring matches the light's actual
        // attenuation footprint. The bulb remains visible even
        // for radius-zero lights.
        let radius_engine = spatial::light_radius_engine_units(grid, light.radius);
        if radius_engine > 0 {
            push_horizontal_ring(scratch, center_world, radius_engine, 16, style);
        }
        push_light_bulb_icon(
            scratch,
            center_world,
            (
                light.color[0].max(0x40),
                light.color[1].max(0x40),
                light.color[2].max(0x40),
            ),
            is_selected || is_hovered,
        );
    }
}

/// Ring + bulb gizmos for lights that live in world space (no enclosing
/// Section room), the BSP-scene counterpart of [`walk_light_gizmos`].
/// Positions are raw world units; the radius scales by the World sector
/// size, matching both the brush bake and the runtime light records.
pub(super) fn walk_roomless_light_gizmos(
    project: &ProjectDocument,
    hidden_scene_nodes: &HashSet<NodeId>,
    selected: psxed_project::NodeId,
    hovered: Option<psxed_project::NodeId>,
    scratch: &mut PreviewScratch,
) {
    let scene = project.active_scene();
    let radius_units = scene
        .world_sector_size_for_node(scene.root)
        .unwrap_or(1024)
        .max(1) as f32;
    for light in preview_lights(scene, hidden_scene_nodes) {
        if node_has_section_ancestor(scene, light.host_id) {
            continue;
        }
        let center_world = light.transform.translation.map(|value| value.round() as i32);
        let center_world = [center_world[0], center_world[1], center_world[2]];
        let is_selected = preview_reference_selected(selected, light.host_id, None, None, None);
        let is_hovered = hovered
            .is_some_and(|id| preview_reference_selected(id, light.host_id, None, None, None));
        let style = if is_selected {
            FaceOutlineStyle {
                rgb: (0xFF, 0xE0, 0x80),
                thickness_px: EDITOR_PREVIEW_SELECTED_STROKE_WIDTH,
            }
        } else if is_hovered {
            FaceOutlineStyle {
                rgb: (0xFF, 0xF0, 0x90),
                thickness_px: EDITOR_PREVIEW_HOVER_STROKE_WIDTH,
            }
        } else {
            FaceOutlineStyle {
                rgb: (
                    light.color[0].max(0x40),
                    light.color[1].max(0x40),
                    light.color[2].max(0x40),
                ),
                thickness_px: EDITOR_PREVIEW_HOVER_STROKE_WIDTH,
            }
        };
        let radius_engine = (light.radius * radius_units) as i32;
        if radius_engine > 0 {
            push_horizontal_ring(scratch, center_world, radius_engine, 16, style);
        }
        push_light_bulb_icon(
            scratch,
            center_world,
            (
                light.color[0].max(0x40),
                light.color[1].max(0x40),
                light.color[2].max(0x40),
            ),
            is_selected || is_hovered,
        );
    }
}

/// Wireframe AABB + facing arrow per selectable scene entity.
/// Bounds are gathered by `EditorWorkspace::collect_entity_bounds`
/// -- every entity-kind node carries an AABB the user can click to
/// select and drag to move. Image props, box props, and portals render
/// their own authored frames/seams, so this pass leaves those out and
/// draws simple manipulation boxes for the remaining object-like nodes.
///
/// Idle bounds draw thin and muted so they don't dominate the
/// viewport over the room they sit in. Hover and selected reuse
/// the room face palette for cross-tool consistency: yellow for
/// hover, cyan-bold for selected.
pub(super) fn walk_entity_bounds(
    bounds: &[psxed_ui::EntityBounds],
    selected: psxed_project::NodeId,
    hovered: Option<psxed_project::NodeId>,
    scratch: &mut PreviewScratch,
) {
    for b in bounds {
        if matches!(
            b.kind,
            psxed_ui::EntityBoundKind::ImageProp
                | psxed_ui::EntityBoundKind::BoxProp
                | psxed_ui::EntityBoundKind::CylinderProp
                | psxed_ui::EntityBoundKind::Portal
        ) {
            continue;
        }
        let is_selected = b.node == selected;
        let is_hovered = hovered == Some(b.node);
        let style = entity_bound_style(b.kind, is_selected, is_hovered);
        push_aabb_wireframe(scratch, b.center, b.half_extents, style);

        // Yaw arrow only for kinds with meaningful facing --
        // models and spawn points point at where they'll
        // render / face. Lights and portals are either
        // omnidirectional or carry their own
        // direction gizmo elsewhere (light radius ring).
        if matches!(
            b.kind,
            psxed_ui::EntityBoundKind::Model
                | psxed_ui::EntityBoundKind::SpawnPoint
                | psxed_ui::EntityBoundKind::MeshFallback
        ) {
            push_facing_arrow(scratch, b.center, b.half_extents, b.yaw_degrees, style);
        }
    }
}

/// Pick the outline style for one bound. Selected wins over
/// hover; idle uses a muted kind-tinted thin line so multiple
/// boxes in a busy room read at a glance without dominating.
pub(super) fn entity_bound_style(
    kind: psxed_ui::EntityBoundKind,
    selected: bool,
    hovered: bool,
) -> FaceOutlineStyle {
    if selected {
        return ENTITY_BOUND_SELECTED;
    }
    if hovered {
        return ENTITY_BOUND_HOVER;
    }
    let rgb = match kind {
        psxed_ui::EntityBoundKind::Model => (0xC0, 0xC8, 0xD0),
        psxed_ui::EntityBoundKind::MeshFallback => (0x90, 0x98, 0xA0),
        psxed_ui::EntityBoundKind::ImageProp => (0xD0, 0xAA, 0x78),
        psxed_ui::EntityBoundKind::BoxProp => (0x87, 0xB4, 0xDC),
        psxed_ui::EntityBoundKind::CylinderProp => (0x78, 0xB8, 0xC8),
        psxed_ui::EntityBoundKind::ArchProp => (0xC0, 0xA0, 0x70),
        psxed_ui::EntityBoundKind::SpawnPoint => (0x60, 0xE0, 0x80),
        psxed_ui::EntityBoundKind::PointLight => (0xFF, 0xD8, 0x70),
        psxed_ui::EntityBoundKind::ParticleEmitter => (0x98, 0xD6, 0xE6),
        psxed_ui::EntityBoundKind::Portal => PORTAL_SEAM_STYLE.rgb,
        psxed_ui::EntityBoundKind::Logic => (0xC8, 0x8C, 0xE8),
    };
    FaceOutlineStyle {
        rgb,
        thickness_px: EDITOR_PREVIEW_HOVER_STROKE_WIDTH,
    }
}

/// Project the 8 corners of a world-space AABB and emit the 12
/// edges as `push_screen_line` segments. Coordinates are stored
/// `f32` in the bound; rounded to `i32` here because the GTE
/// shim wants integer world coords.
pub(super) fn push_aabb_wireframe(
    scratch: &mut PreviewScratch,
    center: [f32; 3],
    half_extents: [f32; 3],
    style: FaceOutlineStyle,
) {
    let cx = center[0].round() as i32;
    let cy = center[1].round() as i32;
    let cz = center[2].round() as i32;
    let hx = half_extents[0].round() as i32;
    let hy = half_extents[1].round() as i32;
    let hz = half_extents[2].round() as i32;
    if hx <= 0 || hy <= 0 || hz <= 0 {
        return;
    }
    let lo = [cx - hx, cy - hy, cz - hz];
    let hi = [cx + hx, cy + hy, cz + hz];
    // Corner index encoding: bit0 = X (lo/hi), bit1 = Y, bit2 = Z.
    let corner = |i: usize| -> [i32; 3] {
        [
            if i & 1 != 0 { hi[0] } else { lo[0] },
            if i & 2 != 0 { hi[1] } else { lo[1] },
            if i & 4 != 0 { hi[2] } else { lo[2] },
        ]
    };
    let corners: [_; 8] = std::array::from_fn(corner);
    push_world_box_wireframe(scratch, corners, style);
}

pub(super) fn push_world_box_wireframe(
    scratch: &mut PreviewScratch,
    corners: [[i32; 3]; 8],
    style: FaceOutlineStyle,
) {
    let p = corners.map(|corner| gte_scene::project_vertex(world_to_view(corner)));
    // 12 edges of a box: 4 along X, 4 along Y, 4 along Z. Pairs
    // of corners that differ in exactly one bit.
    const EDGES: [(usize, usize); 12] = [
        (0, 1),
        (2, 3),
        (4, 5),
        (6, 7), // along X
        (0, 2),
        (1, 3),
        (4, 6),
        (5, 7), // along Y
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7), // along Z
    ];
    for (a, b) in EDGES {
        if p[a].sz == 0 || p[b].sz == 0 {
            continue;
        }
        push_screen_line(scratch, p[a], p[b], style);
    }
}

pub(super) fn selected_node_is_image_prop(project: &ProjectDocument, selected: NodeId) -> bool {
    project.active_scene().node(selected).is_some_and(|node| {
        matches!(
            node.kind,
            NodeKind::ImageProp { .. } | NodeKind::BoxProp { .. } | NodeKind::CylinderProp { .. }
        )
    })
}

/// Draw a forward-pointing arrow from the bound centre out
/// past the front face, indicating where the entity faces.
/// Length scales with the bound's horizontal extent so big
/// models get a visible arrow and tiny markers don't grow
/// horns.
pub(super) fn push_facing_arrow(
    scratch: &mut PreviewScratch,
    center: [f32; 3],
    half_extents: [f32; 3],
    yaw_degrees: f32,
    style: FaceOutlineStyle,
) {
    let yaw_q12 = yaw_to_q12(yaw_degrees);
    let s = sin_q12_turn(yaw_q12);
    let c = cos_q12_turn(yaw_q12);
    // Arrow length = bound's horizontal half-extent + a small
    // overshoot so the head sits clearly outside the box.
    let reach = (half_extents[0].max(half_extents[2]) * 1.5).max(96.0) as i32;
    let cx = center[0].round() as i32;
    let cy = center[1].round() as i32;
    let cz = center[2].round() as i32;
    let tip = [cx + ((s * reach) >> 12), cy, cz + ((c * reach) >> 12)];
    let p_origin = gte_scene::project_vertex(world_to_view([cx, cy, cz]));
    let p_tip = gte_scene::project_vertex(world_to_view(tip));
    if p_origin.sz != 0 && p_tip.sz != 0 {
        push_screen_line(scratch, p_origin, p_tip, style);
    }
}

/// Project a horizontal `segments`-sided polygon at world
/// `center` with `radius` into screen space and emit one
/// `push_screen_line` per edge. Used for light radius gizmos
/// and any future ground-plane affordances.
pub(super) fn push_horizontal_ring(
    scratch: &mut PreviewScratch,
    center: [i32; 3],
    radius: i32,
    segments: u16,
    style: FaceOutlineStyle,
) {
    if segments < 3 || radius <= 0 {
        return;
    }
    let mut prev_world = [center[0] + radius, center[1], center[2]];
    let mut prev_proj = gte_scene::project_vertex(world_to_view(prev_world));
    for i in 1..=segments {
        // Authored editor angles use 4096 units per turn; sample
        // the unit circle around the light origin once per segment.
        let angle_q12 = ((i as u32 * 4096) / segments as u32) as u16;
        let s = sin_q12_turn(angle_q12);
        let c = cos_q12_turn(angle_q12);
        let next_world = [
            center[0] + ((c * radius) >> 12),
            center[1],
            center[2] + ((s * radius) >> 12),
        ];
        let next_proj = gte_scene::project_vertex(world_to_view(next_world));
        if prev_proj.sz != 0 && next_proj.sz != 0 {
            push_screen_line(scratch, prev_proj, next_proj, style);
        }
        prev_world = next_world;
        prev_proj = next_proj;
    }
    let _ = prev_world; // silence the unused-final-assignment lint
}

pub(super) fn push_light_bulb_icon(
    scratch: &mut PreviewScratch,
    center_world: [i32; 3],
    rgb: (u8, u8, u8),
    emphasized: bool,
) {
    let projected = gte_scene::project_vertex(world_to_view(center_world));
    if projected.sz == 0 {
        return;
    }
    let cx = projected.sx as f32;
    let cy = projected.sy as f32;
    let radius = if emphasized { 8.5 } else { 7.0 };
    let icon = FaceOutlineStyle {
        rgb,
        thickness_px: if emphasized { 1.0 } else { 0.75 },
    };
    let glass = egui::pos2(cx, cy - radius * 0.28);
    let segments = 16;
    for i in 0..segments {
        let a0 = i as f32 * std::f32::consts::TAU / segments as f32;
        let a1 = (i + 1) as f32 * std::f32::consts::TAU / segments as f32;
        push_overlay_line(
            scratch,
            egui::pos2(glass.x + a0.cos() * radius, glass.y + a0.sin() * radius),
            egui::pos2(glass.x + a1.cos() * radius, glass.y + a1.sin() * radius),
            icon,
        );
    }

    let neck_y = glass.y + radius * 0.72;
    let base_y = neck_y + radius * 0.62;
    let neck_half = radius * 0.46;
    let base_half = radius * 0.34;
    push_overlay_line(
        scratch,
        egui::pos2(cx - neck_half, neck_y),
        egui::pos2(cx - base_half, base_y),
        icon,
    );
    push_overlay_line(
        scratch,
        egui::pos2(cx + neck_half, neck_y),
        egui::pos2(cx + base_half, base_y),
        icon,
    );
    for step in [0.0, 0.28, 0.56] {
        let y = neck_y + radius * step;
        push_overlay_line(
            scratch,
            egui::pos2(cx - base_half, y),
            egui::pos2(cx + base_half, y),
            icon,
        );
    }

    let filament_y = glass.y + radius * 0.18;
    push_overlay_line(
        scratch,
        egui::pos2(cx - radius * 0.36, filament_y),
        egui::pos2(cx - radius * 0.12, filament_y + radius * 0.2),
        icon,
    );
    push_overlay_line(
        scratch,
        egui::pos2(cx - radius * 0.12, filament_y + radius * 0.2),
        egui::pos2(cx + radius * 0.12, filament_y + radius * 0.2),
        icon,
    );
    push_overlay_line(
        scratch,
        egui::pos2(cx + radius * 0.12, filament_y + radius * 0.2),
        egui::pos2(cx + radius * 0.36, filament_y),
        icon,
    );

    if emphasized {
        let halo = FaceOutlineStyle {
            rgb: (0xFF, 0xFF, 0xFF),
            thickness_px: 0.45,
        };
        let halo_radius = radius + 2.0;
        for i in 0..segments {
            let a0 = i as f32 * std::f32::consts::TAU / segments as f32;
            let a1 = (i + 1) as f32 * std::f32::consts::TAU / segments as f32;
            push_overlay_line(
                scratch,
                egui::pos2(
                    glass.x + a0.cos() * halo_radius,
                    glass.y + a0.sin() * halo_radius,
                ),
                egui::pos2(
                    glass.x + a1.cos() * halo_radius,
                    glass.y + a1.sin() * halo_radius,
                ),
                halo,
            );
        }
    }
}

pub(super) fn synth(sx: i16, sy: i16, sz: u16) -> psx_gte::scene::Projected {
    psx_gte::scene::Projected { sx, sy, sz }
}

/// Render the paint-target ghost outline. Cell ghosts trace the
/// would-be cell surface; wall ghosts use
/// `push_face_outline` with a synthetic `FaceRef` whose world cell
/// might lie outside the current grid. Missing wall-stack previews
/// project explicit candidate heights so the outline appears where
/// the auto-grow or next-stack click will create the wall.
pub(super) fn push_paint_preview(
    grid: &WorldGrid,
    preview: psxed_ui::PaintTargetPreview,
    scratch: &mut PreviewScratch,
) {
    match preview {
        psxed_ui::PaintTargetPreview::Cell {
            world_cell_x,
            world_cell_z,
            kind,
        } => push_cell_ghost_outline(grid, world_cell_x, world_cell_z, kind, scratch),
        psxed_ui::PaintTargetPreview::Wall {
            world_cell_x,
            world_cell_z,
            dir,
            stack,
        } => {
            // Translate world cell -> array when in bounds so an
            // existing wall stack can still use the regular face
            // outline path.
            let (sx, sz) = grid
                .world_cell_to_array(world_cell_x, world_cell_z)
                .unwrap_or((u16::MAX, u16::MAX));
            // Fake a FaceRef. `room` field is unused by
            // push_face_outline; safe to fill with anything.
            let face = psxed_ui::FaceRef {
                room: psxed_project::NodeId::ROOT,
                sx,
                sz,
                kind: psxed_ui::FaceKind::Wall { dir, stack },
            };
            // For off-grid or next-stack wall ghosts we have to
            // project the outline ourselves. `push_face_outline`
            // either short-circuits when sx/sz are out of grid
            // bounds or falls back to floor-to-ceiling placement
            // for missing stack indices.
            let existing_stack = grid
                .sector(sx, sz)
                .is_some_and(|sector| (stack as usize) < sector.walls.get(dir).len());
            if sx == u16::MAX || sz == u16::MAX || !existing_stack {
                let heights = grid.wall_heights_above_stack_or_surfaces_for_world_cell(
                    world_cell_x,
                    world_cell_z,
                    dir,
                );
                push_ghost_wall_outline(grid, world_cell_x, world_cell_z, dir, heights, scratch);
            } else {
                push_face_outline(grid, face, FACE_OUTLINE_WALL_PAINT, scratch);
            }
        }
        psxed_ui::PaintTargetPreview::PortalEdge {
            world_cell_x,
            world_cell_z,
            dir,
            valid,
        } => {
            let style = if valid {
                PORTAL_SEAM_STYLE
            } else {
                PORTAL_SEAM_INVALID_STYLE
            };
            if valid {
                if let Some((sx, sz)) = grid.world_cell_to_array(world_cell_x, world_cell_z) {
                    if let Some(edge) = canonical_portal_edge_for_array_cell(sx, sz, dir) {
                        let seam = portal_seam_edges_for_edge(grid, edge);
                        if !seam.is_empty() {
                            push_portal_seam_edges(grid, seam, style, scratch);
                            return;
                        }
                    }
                }
            }
            push_portal_edge_wall_outline(grid, world_cell_x, world_cell_z, dir, style, scratch);
        }
    }
}

/// Outline a cell at world-cell `(wcx, wcz)`. Floor / ceiling paint
/// previews use the same candidate heights the click path will
/// commit; generic cell previews stay on the ground footprint.
pub(super) fn push_cell_ghost_outline(
    grid: &WorldGrid,
    wcx: i32,
    wcz: i32,
    kind: PaintCellPreviewKind,
    scratch: &mut PreviewScratch,
) {
    let s = grid.sector_size;
    let x0 = wcx * s;
    let x1 = x0 + s;
    let z0 = wcz * s;
    let z1 = z0 + s;
    let (heights, style) = match kind {
        PaintCellPreviewKind::Ground => ([0; 4], FACE_OUTLINE_HOVER),
        PaintCellPreviewKind::Floor => (
            grid.floor_heights_aligned_to_neighbors_for_world_cell(wcx, wcz, 0),
            FACE_OUTLINE_FLOOR_PAINT,
        ),
        PaintCellPreviewKind::Ceiling => (
            grid.ceiling_heights_aligned_to_neighbors_for_world_cell(wcx, wcz),
            FACE_OUTLINE_CEILING_PAINT,
        ),
    };
    const LIFT: i32 = 4;
    let nw = gte_scene::project_vertex(world_to_view([x0, heights[Corner::NW.idx()] + LIFT, z1]));
    let ne = gte_scene::project_vertex(world_to_view([x1, heights[Corner::NE.idx()] + LIFT, z1]));
    let se = gte_scene::project_vertex(world_to_view([x1, heights[Corner::SE.idx()] + LIFT, z0]));
    let sw = gte_scene::project_vertex(world_to_view([x0, heights[Corner::SW.idx()] + LIFT, z0]));
    if [nw, ne, se, sw].iter().any(|p| p.sz == 0) {
        return;
    }
    for (a, b) in [(nw, ne), (ne, se), (se, sw), (sw, nw)] {
        push_screen_line(scratch, a, b, style);
    }
}

/// Outline a wall at world-cell `(wcx, wcz)` on edge `dir`. Used
/// when `push_face_outline`'s array-bound check rejects an off-grid
/// ghost so the user still sees where the wall will land.
pub(super) fn push_ghost_wall_outline(
    grid: &WorldGrid,
    wcx: i32,
    wcz: i32,
    dir: GridDirection,
    heights: [i32; 4],
    scratch: &mut PreviewScratch,
) {
    let s = grid.sector_size;
    const LIFT: i32 = 4;
    let bounds = spatial::cell_bounds_from_world_cell(wcx, wcz, s);
    let Some(corners) = spatial::editor_wall_outline_corners(bounds, dir, heights, LIFT) else {
        return;
    };
    let projected: [psx_gte::scene::Projected; 4] = [
        gte_scene::project_vertex(world_to_view(corners[0])),
        gte_scene::project_vertex(world_to_view(corners[1])),
        gte_scene::project_vertex(world_to_view(corners[2])),
        gte_scene::project_vertex(world_to_view(corners[3])),
    ];
    if projected.iter().any(|p| p.sz == 0) {
        return;
    }
    for i in 0..4 {
        push_screen_line(
            scratch,
            projected[i],
            projected[(i + 1) % 4],
            FACE_OUTLINE_WALL_PAINT,
        );
    }
}

/// Hover and Selected outline styling. RGB plus screen-space line
/// thickness in pixels. Keep these light: they are editor affordances,
/// not scene geometry, and thick strokes obscure PS1-scale surfaces.
pub(super) const FACE_OUTLINE_HOVER: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0xFF, 0xE0, 0x60),
    thickness_px: EDITOR_PREVIEW_HOVER_STROKE_WIDTH,
};
pub(super) const FACE_OUTLINE_SELECTED: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0x60, 0xC8, 0xFF),
    thickness_px: EDITOR_PREVIEW_SELECTED_STROKE_WIDTH,
};
pub(super) const FACE_OUTLINE_ERROR: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0xFF, 0x40, 0x40),
    thickness_px: 4.0,
};
pub(super) const FACE_OUTLINE_CULLED: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0x88, 0xA0, 0xAE),
    thickness_px: 1.0,
};
pub(super) const ENTITY_BOUND_HOVER: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0xFF, 0xE0, 0x60),
    thickness_px: EDITOR_PREVIEW_HOVER_STROKE_WIDTH,
};
pub(super) const ENTITY_BOUND_SELECTED: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0x60, 0xC8, 0xFF),
    thickness_px: EDITOR_PREVIEW_SELECTED_STROKE_WIDTH,
};
/// Wireframe outline drawn around an image prop's authored
/// collision AABB. Distinct warm orange to read as "collision /
/// physics" against the cooler selection / hover palette.
pub(super) const IMAGE_PROP_COLLISION_BOX: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0xFF, 0xA0, 0x40),
    thickness_px: 1.5,
};
/// PaintWall hover preview -- green for "this would be added /
/// replaced". Slightly stronger than hover, but still thin enough
/// to leave the underlying face readable.
pub(super) const FACE_OUTLINE_WALL_PAINT: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0x60, 0xFF, 0x90),
    thickness_px: EDITOR_PREVIEW_PAINT_STROKE_WIDTH,
};
pub(super) const FACE_OUTLINE_FLOOR_PAINT: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0x60, 0xFF, 0x90),
    thickness_px: EDITOR_PREVIEW_PAINT_STROKE_WIDTH,
};
pub(super) const FACE_OUTLINE_CEILING_PAINT: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0x80, 0xC8, 0xFF),
    thickness_px: EDITOR_PREVIEW_PAINT_STROKE_WIDTH,
};
pub(super) const PORTAL_SEAM_STYLE: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0xFF, 0x48, 0xD6),
    thickness_px: 3.0,
};
pub(super) const PORTAL_SEAM_INVALID_STYLE: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0xFF, 0x40, 0x60),
    thickness_px: 2.0,
};
pub(super) const STREAMING_CHUNK_BOUNDARY: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0x60, 0xFF, 0xC4),
    thickness_px: 2.0,
};

#[derive(Copy, Clone)]
pub(super) struct FaceOutlineStyle {
    pub(super) rgb: (u8, u8, u8),
    pub(super) thickness_px: f32,
}

/// Hover vs Selected -- outline style picker for the unified
/// selection dispatch. Hover uses the lighter yellow; selected
/// uses the bolder cyan. Same constants the original face-only
/// path consumed.
#[derive(Copy, Clone)]
pub(super) enum OutlineRole {
    Hover,
    Selected,
    Error,
}

impl OutlineRole {
    pub(super) fn face_style(self) -> FaceOutlineStyle {
        match self {
            Self::Hover => FACE_OUTLINE_HOVER,
            Self::Selected => FACE_OUTLINE_SELECTED,
            Self::Error => FACE_OUTLINE_ERROR,
        }
    }
}

/// Dispatch a `Selection` to the appropriate outline helper.
/// Each variant gets its own screen-space overlay: face → 4
/// edge lines, edge → 1 line, vertex → cross.
pub(super) fn push_selection_outline(
    grid: &WorldGrid,
    selection: psxed_ui::Selection,
    role: OutlineRole,
    scratch: &mut PreviewScratch,
) {
    match selection {
        psxed_ui::Selection::Face(face) => {
            push_face_outline(grid, face, role.face_style(), scratch);
        }
        psxed_ui::Selection::Triangle(triangle) => {
            push_triangle_outline(grid, triangle, role.face_style(), scratch);
        }
        psxed_ui::Selection::Edge(edge) => {
            push_edge_outline(grid, edge, role.face_style(), scratch);
        }
        psxed_ui::Selection::Vertex(vertex) => {
            push_vertex_outline(grid, vertex, role.face_style(), scratch);
        }
    }
}

pub(super) fn push_triangle_outline(
    grid: &WorldGrid,
    triangle: psxed_ui::HorizontalTriangleRef,
    style: FaceOutlineStyle,
    scratch: &mut PreviewScratch,
) {
    let [c0, c1, c2] = triangle.corners;
    let Some(w0) = psxed_ui::face_corner_world(grid, triangle.face_corner(c0)) else {
        return;
    };
    let Some(w1) = psxed_ui::face_corner_world(grid, triangle.face_corner(c1)) else {
        return;
    };
    let Some(w2) = psxed_ui::face_corner_world(grid, triangle.face_corner(c2)) else {
        return;
    };
    let projected = [
        gte_scene::project_vertex(world_to_view(w0)),
        gte_scene::project_vertex(world_to_view(w1)),
        gte_scene::project_vertex(world_to_view(w2)),
    ];
    if projected.iter().any(|p| p.sz == 0) {
        return;
    }
    for i in 0..3 {
        push_screen_line(scratch, projected[i], projected[(i + 1) % 3], style);
    }
}

pub(super) fn push_streaming_chunk_boundaries(
    project: &ProjectDocument,
    room_id: NodeId,
    grid: &WorldGrid,
    scratch: &mut PreviewScratch,
) {
    let plan = plan_portal_rooms(
        project.active_scene(),
        room_id,
        grid,
        PortalRoomConfig::default(),
    );
    if plan.room_count() <= 1 {
        return;
    }
    let s = grid.sector_size;
    let y = 10;
    for room in plan.rooms {
        let x0 = room.world_origin[0] * s;
        let z0 = room.world_origin[1] * s;
        let x1 = x0 + room.size[0] as i32 * s;
        let z1 = z0 + room.size[1] as i32 * s;
        let projected: [psx_gte::scene::Projected; 4] = [
            gte_scene::project_vertex(world_to_view([x0, y, z1])),
            gte_scene::project_vertex(world_to_view([x1, y, z1])),
            gte_scene::project_vertex(world_to_view([x1, y, z0])),
            gte_scene::project_vertex(world_to_view([x0, y, z0])),
        ];
        if projected.iter().any(|p| p.sz == 0) {
            continue;
        }
        for i in 0..4 {
            push_screen_line(
                scratch,
                projected[i],
                projected[(i + 1) % 4],
                STREAMING_CHUNK_BOUNDARY,
            );
        }
    }
}

/// One thick screen-space line between the edge's two world
/// endpoints. Lifted slightly off the surface (same `LIFT` as
/// `push_face_outline`) so it doesn't z-fight the geometry it
/// outlines.
pub(super) fn push_edge_outline(
    grid: &WorldGrid,
    edge: psxed_ui::EdgeRef,
    style: FaceOutlineStyle,
    scratch: &mut PreviewScratch,
) {
    let Some((a, b)) = edge_world_endpoints(grid, edge) else {
        return;
    };
    let projected_a = gte_scene::project_vertex(world_to_view(a));
    let projected_b = gte_scene::project_vertex(world_to_view(b));
    if projected_a.sz == 0 || projected_b.sz == 0 {
        return;
    }
    push_screen_line(scratch, projected_a, projected_b, style);
}

/// Small screen-space cross at the vertex's world position.
/// The cross is drawn as four short line segments offset along
/// world axes so its on-screen size scales naturally with
/// distance -- close vertices read clearly, far ones don't
/// dominate the viewport.
pub(super) fn push_vertex_outline(
    grid: &WorldGrid,
    vertex: psxed_ui::VertexRef,
    style: FaceOutlineStyle,
    scratch: &mut PreviewScratch,
) {
    let Some(world) = vertex_world_position(grid, vertex) else {
        return;
    };
    // Half-extent of the cross in world units. ~32 reads as a
    // few px in the viewport at orbit distances we use.
    const ARM: i32 = 32;
    let arms = [
        (
            [world[0] - ARM, world[1], world[2]],
            [world[0] + ARM, world[1], world[2]],
        ),
        (
            [world[0], world[1] - ARM, world[2]],
            [world[0], world[1] + ARM, world[2]],
        ),
        (
            [world[0], world[1], world[2] - ARM],
            [world[0], world[1], world[2] + ARM],
        ),
    ];
    for (a, b) in arms {
        let pa = gte_scene::project_vertex(world_to_view(a));
        let pb = gte_scene::project_vertex(world_to_view(b));
        if pa.sz == 0 || pb.sz == 0 {
            continue;
        }
        push_screen_line(scratch, pa, pb, style);
    }
}

pub(super) fn edge_world_endpoints(
    grid: &WorldGrid,
    edge: psxed_ui::EdgeRef,
) -> Option<([i32; 3], [i32; 3])> {
    use psxed_ui::{EdgeAnchor, FaceCornerRef};
    let (a, b) = match edge.anchor {
        EdgeAnchor::Floor { sx, sz, dir } => (
            FaceCornerRef::Floor {
                sx,
                sz,
                corner: floor_edge_a(dir),
            },
            FaceCornerRef::Floor {
                sx,
                sz,
                corner: floor_edge_b(dir),
            },
        ),
        EdgeAnchor::Ceiling { sx, sz, dir } => (
            FaceCornerRef::Ceiling {
                sx,
                sz,
                corner: floor_edge_a(dir),
            },
            FaceCornerRef::Ceiling {
                sx,
                sz,
                corner: floor_edge_b(dir),
            },
        ),
        EdgeAnchor::Wall {
            sx,
            sz,
            dir,
            stack,
            edge: e,
        } => (
            FaceCornerRef::Wall {
                sx,
                sz,
                dir,
                stack,
                corner: wall_edge_a(e),
            },
            FaceCornerRef::Wall {
                sx,
                sz,
                dir,
                stack,
                corner: wall_edge_b(e),
            },
        ),
    };
    Some((
        psxed_ui::face_corner_world(grid, a)?,
        psxed_ui::face_corner_world(grid, b)?,
    ))
}

pub(super) fn vertex_world_position(
    grid: &WorldGrid,
    vertex: psxed_ui::VertexRef,
) -> Option<[i32; 3]> {
    psxed_ui::face_corner_world(grid, vertex.anchor.as_face_corner())
}

pub(super) const fn floor_edge_a(dir: GridDirection) -> psxed_ui::Corner {
    match dir {
        GridDirection::North => psxed_ui::Corner::NW,
        GridDirection::East => psxed_ui::Corner::NE,
        GridDirection::South => psxed_ui::Corner::SE,
        GridDirection::West => psxed_ui::Corner::SW,
        GridDirection::NorthWestSouthEast => psxed_ui::Corner::NW,
        GridDirection::NorthEastSouthWest => psxed_ui::Corner::NE,
    }
}

pub(super) const fn floor_edge_b(dir: GridDirection) -> psxed_ui::Corner {
    match dir {
        GridDirection::North => psxed_ui::Corner::NE,
        GridDirection::East => psxed_ui::Corner::SE,
        GridDirection::South => psxed_ui::Corner::SW,
        GridDirection::West => psxed_ui::Corner::NW,
        GridDirection::NorthWestSouthEast => psxed_ui::Corner::SE,
        GridDirection::NorthEastSouthWest => psxed_ui::Corner::SW,
    }
}

pub(super) const fn wall_edge_a(edge: psxed_ui::WallEdge) -> psxed_ui::WallCorner {
    match edge {
        psxed_ui::WallEdge::Bottom => psxed_ui::WallCorner::BL,
        psxed_ui::WallEdge::Right => psxed_ui::WallCorner::BR,
        psxed_ui::WallEdge::Top => psxed_ui::WallCorner::TR,
        psxed_ui::WallEdge::Left => psxed_ui::WallCorner::TL,
    }
}

pub(super) const fn wall_edge_b(edge: psxed_ui::WallEdge) -> psxed_ui::WallCorner {
    match edge {
        psxed_ui::WallEdge::Bottom => psxed_ui::WallCorner::BR,
        psxed_ui::WallEdge::Right => psxed_ui::WallCorner::TR,
        psxed_ui::WallEdge::Top => psxed_ui::WallCorner::TL,
        psxed_ui::WallEdge::Left => psxed_ui::WallCorner::BL,
    }
}

/// Stamp four short, screen-space-thick line segments along the
/// edges of a picked face. Drawing in screen space (after GTE
/// projection) keeps the outline a constant pixel weight regardless
/// of perspective, which matches Godot / Unity's "selection halo"
/// look. Lines pinned to OT slot 0 so they paint on top of every
/// floor / wall / ceiling.
pub(super) fn push_face_outline(
    grid: &WorldGrid,
    face: psxed_ui::FaceRef,
    style: FaceOutlineStyle,
    scratch: &mut PreviewScratch,
) {
    if face.sx >= grid.width || face.sz >= grid.depth {
        return;
    }
    let sector = grid.sector(face.sx, face.sz);
    let s = grid.sector_size;
    let x0 = grid.cell_world_x(face.sx);
    let x1 = x0 + s;
    let z0 = grid.cell_world_z(face.sz);
    let z1 = z0 + s;
    // Lift a hair off the surface so the outline doesn't z-fight
    // the face it's marking. Sloped floors keep their relative
    // outline position because we lift each corner by the same
    // amount along the local up axis.
    const LIFT: i32 = 4;
    let corners = match face.kind {
        psxed_ui::FaceKind::Floor => sector.and_then(|s| s.floor.as_ref()).map(|f| {
            let h = f.heights;
            [
                [x0, h[0] + LIFT, z1],
                [x1, h[1] + LIFT, z1],
                [x1, h[2] + LIFT, z0],
                [x0, h[3] + LIFT, z0],
            ]
        }),
        psxed_ui::FaceKind::Ceiling => sector.and_then(|s| s.ceiling.as_ref()).map(|c| {
            let h = c.heights;
            [
                [x0, h[0] - LIFT, z1],
                [x1, h[1] - LIFT, z1],
                [x1, h[2] - LIFT, z0],
                [x0, h[3] - LIFT, z0],
            ]
        }),
        psxed_ui::FaceKind::Wall { dir, stack } => {
            let h = sector
                .and_then(|s| s.walls.get(dir).get(stack as usize))
                .map(|wall| wall.heights)
                .unwrap_or_else(|| grid.wall_heights_aligned_to_surfaces(face.sx, face.sz, dir));
            // Inset along the wall's inward normal so the outline
            // sits inside the room rather than z-fighting the
            // wall surface when viewed from inside.
            spatial::editor_wall_outline_corners(
                grid.cell_bounds_world(face.sx, face.sz),
                dir,
                h,
                LIFT,
            )
        }
    };
    let Some(corners) = corners else { return };
    let projected: [psx_gte::scene::Projected; 4] = [
        gte_scene::project_vertex(world_to_view(corners[0])),
        gte_scene::project_vertex(world_to_view(corners[1])),
        gte_scene::project_vertex(world_to_view(corners[2])),
        gte_scene::project_vertex(world_to_view(corners[3])),
    ];
    // Skip outlines whose corners didn't project -- `project_vertex`
    // returns `sz == 0` for behind-camera or near-plane-clipped
    // points, which would produce nonsense screen lines.
    if projected.iter().any(|p| p.sz == 0) {
        return;
    }
    for i in 0..4 {
        let a = projected[i];
        let b = projected[(i + 1) % 4];
        push_screen_line(scratch, a, b, style);
    }
    let split = match face.kind {
        psxed_ui::FaceKind::Floor => sector.and_then(|s| s.floor.as_ref()).map(|face| face.split),
        psxed_ui::FaceKind::Ceiling => sector
            .and_then(|s| s.ceiling.as_ref())
            .map(|face| face.split),
        psxed_ui::FaceKind::Wall { .. } => None,
    };
    if let Some(split) = split {
        let (a, b) = match split {
            GridSplit::NorthWestSouthEast => (projected[0], projected[2]),
            GridSplit::NorthEastSouthWest => (projected[1], projected[3]),
        };
        push_screen_line(scratch, a, b, style);
    }
}

/// Queue one host-drawn overlay segment between two screen-projected
/// vertices. Unlike the scene command log, this is painted by egui on
/// top of the preview texture, so fractional widths and normal UI
/// compositing work as expected.
pub(super) fn push_screen_line(
    scratch: &mut PreviewScratch,
    a: psx_gte::scene::Projected,
    b: psx_gte::scene::Projected,
    style: FaceOutlineStyle,
) {
    push_overlay_line(
        scratch,
        egui::pos2(a.sx as f32, a.sy as f32),
        egui::pos2(b.sx as f32, b.sy as f32),
        style,
    );
}

pub(super) fn push_overlay_line(
    scratch: &mut PreviewScratch,
    a: egui::Pos2,
    b: egui::Pos2,
    style: FaceOutlineStyle,
) {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 0.5 {
        return;
    }
    scratch
        .overlay_lines
        .push(psxed_ui::EditorViewportOverlayLine::new(
            a,
            b,
            bright_overlay_color(style.rgb),
            style.thickness_px,
        ));
}

pub(super) fn bright_overlay_color(rgb: (u8, u8, u8)) -> egui::Color32 {
    let lift = |channel: u8| -> u8 {
        let c = channel as u16;
        (c + ((255 - c) * 3 / 5)).min(255) as u8
    };
    egui::Color32::from_rgb(lift(rgb.0), lift(rgb.1), lift(rgb.2))
}
