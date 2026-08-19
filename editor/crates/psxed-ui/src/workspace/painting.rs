use super::*;
use psxed_project::GridFloorLink;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayerDirection {
    Above,
    Below,
}

#[derive(Debug, Clone, Copy)]
struct LayerFootprintCell {
    world: (i32, i32),
    floor_material: Option<ResourceId>,
    ceiling_material: Option<ResourceId>,
    wall_material: Option<ResourceId>,
}

fn remap_deleted_floor_target(
    link: &mut Option<GridFloorLink>,
    target_room: NodeId,
    removed_floor: usize,
    replacement_floor: usize,
) {
    let Some(link) = link.as_mut() else {
        return;
    };
    if link.target_room != Some(target_room) {
        return;
    }
    let target = usize::from(link.target_floor);
    let remapped = if target > removed_floor {
        target - 1
    } else if target == removed_floor {
        replacement_floor
    } else {
        target
    };
    link.target_floor = u16::try_from(remapped).unwrap_or(u16::MAX);
}

const PAINT_NEIGHBOR_NORTH: u8 = 1 << 0;
const PAINT_NEIGHBOR_EAST: u8 = 1 << 1;
const PAINT_NEIGHBOR_SOUTH: u8 = 1 << 2;
const PAINT_NEIGHBOR_WEST: u8 = 1 << 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PaintWallFaceKey {
    sx: u16,
    sz: u16,
    dir: GridDirection,
    stack: usize,
}

fn cardinal_paint_neighbors(sx: u16, sz: u16, width: u16, depth: u16) -> Vec<(u8, u16, u16)> {
    let mut neighbors = Vec::with_capacity(4);
    if sz + 1 < depth {
        neighbors.push((PAINT_NEIGHBOR_NORTH, sx, sz + 1));
    }
    if sx + 1 < width {
        neighbors.push((PAINT_NEIGHBOR_EAST, sx + 1, sz));
    }
    if sz > 0 {
        neighbors.push((PAINT_NEIGHBOR_SOUTH, sx, sz - 1));
    }
    if sx > 0 {
        neighbors.push((PAINT_NEIGHBOR_WEST, sx - 1, sz));
    }
    neighbors
}

fn paint_context_material(
    project: &ProjectDocument,
    material: Option<ResourceId>,
) -> Option<ResourceId> {
    let material_id = material?;
    let resource = project.resource(material_id)?;
    if let Some(brush) = generated_paint_brush(project, material_id) {
        return Some(brush);
    }
    let ResourceData::Material(material) = &resource.data else {
        return Some(material_id);
    };
    if material.texture_mode == MaterialTextureMode::Transition {
        // Hand-authored transitions have no Paint-region metadata. Treat
        // Source B as their dominant/interior material for adjacency.
        material
            .transition
            .source_b
            .or(material.transition.source_a)
    } else {
        Some(material_id)
    }
}

fn generated_paint_brush(project: &ProjectDocument, material_id: ResourceId) -> Option<ResourceId> {
    let resource = project.resource(material_id)?;
    let raw = resource
        .name
        .strip_prefix(AUTO_PAINT_BLEND_PREFIX)?
        .split(':')
        .next()?
        .parse::<u64>()
        .ok()?;
    project
        .resources
        .iter()
        .find(|candidate| candidate.id.raw() == raw)
        .map(|candidate| candidate.id)
}

fn generated_paint_context(
    project: &ProjectDocument,
    material_id: ResourceId,
    brush: ResourceId,
) -> Option<ResourceId> {
    let resource = project.resource(material_id)?;
    let ResourceData::Material(material) = &resource.data else {
        return None;
    };
    (material.texture_mode == MaterialTextureMode::Transition)
        .then_some(material.transition)
        .into_iter()
        .flat_map(|transition| [transition.source_a, transition.source_b])
        .flatten()
        .find(|candidate| *candidate != brush)
}

fn push_material_candidate(
    candidates: &mut Vec<(ResourceId, u8)>,
    material: ResourceId,
    brush: ResourceId,
) {
    if material == brush {
        return;
    }
    if let Some((_, count)) = candidates.iter_mut().find(|(id, _)| *id == material) {
        *count = count.saturating_add(1);
    } else {
        candidates.push((material, 1));
    }
}

fn wall_face(grid: &WorldGrid, key: PaintWallFaceKey) -> Option<&GridVerticalFace> {
    grid.sector(key.sx, key.sz)?
        .walls
        .get(key.dir)
        .get(key.stack)
}

/// Adjacent wall quads in local texture space: top, right, bottom, left.
/// Diagonal walls currently have no stable cross-cell ownership convention,
/// so their transitions use the wall's current material as local context.
fn paint_wall_neighbors(grid: &WorldGrid, key: PaintWallFaceKey) -> Vec<(u8, PaintWallFaceKey)> {
    let mut neighbors = Vec::with_capacity(4);
    if key.stack + 1
        < grid
            .sector(key.sx, key.sz)
            .map(|sector| sector.walls.get(key.dir).len())
            .unwrap_or(0)
    {
        neighbors.push((
            PAINT_NEIGHBOR_NORTH,
            PaintWallFaceKey {
                stack: key.stack + 1,
                ..key
            },
        ));
    }
    if key.stack > 0 {
        neighbors.push((
            PAINT_NEIGHBOR_SOUTH,
            PaintWallFaceKey {
                stack: key.stack - 1,
                ..key
            },
        ));
    }

    let (left, right) = match key.dir {
        GridDirection::North => (
            key.sx.checked_sub(1).map(|sx| (sx, key.sz)),
            (key.sx + 1 < grid.width).then_some((key.sx + 1, key.sz)),
        ),
        GridDirection::South => (
            (key.sx + 1 < grid.width).then_some((key.sx + 1, key.sz)),
            key.sx.checked_sub(1).map(|sx| (sx, key.sz)),
        ),
        GridDirection::East => (
            (key.sz + 1 < grid.depth).then_some((key.sx, key.sz + 1)),
            key.sz.checked_sub(1).map(|sz| (key.sx, sz)),
        ),
        GridDirection::West => (
            key.sz.checked_sub(1).map(|sz| (key.sx, sz)),
            (key.sz + 1 < grid.depth).then_some((key.sx, key.sz + 1)),
        ),
        GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => (None, None),
    };
    for (bit, cell) in [(PAINT_NEIGHBOR_WEST, left), (PAINT_NEIGHBOR_EAST, right)] {
        let Some((sx, sz)) = cell else {
            continue;
        };
        let neighbor = PaintWallFaceKey { sx, sz, ..key };
        if wall_face(grid, neighbor).is_some() {
            neighbors.push((bit, neighbor));
        }
    }
    neighbors
}

fn paint_blend_recipe(
    own: ResourceId,
    other: ResourceId,
    connected_edges: u8,
    coverage_percent: u8,
    edge_detail: u8,
) -> TransitionMaterialTexture {
    let seed = (own.raw().wrapping_mul(0x9e37_79b9).rotate_left(7)
        ^ other.raw().wrapping_mul(0x85eb_ca6b)) as u32;
    let coverage = ((u16::from(coverage_percent.clamp(5, 95)) * 255 + 50) / 100) as u8;
    TransitionMaterialTexture {
        source_a: Some(other),
        source_b: Some(own),
        size: 64,
        coverage,
        shape: TransitionMaskShape::Connected,
        rotation_quarters: 0,
        flip_x: false,
        flip_y: false,
        edge_breakup: edge_detail.min(96),
        seed,
        connected_edges,
    }
}

/// Convert physical face edges into the texture edges sampled after the
/// face's authored UV transform. Paint masks are baked before that transform
/// is applied, so ignoring a 90-degree face rotation opens the blend on the
/// wrong pair of sides even though tile adjacency itself is correct.
fn paint_edges_in_transformed_uv_space(mut edges: u8, uv: GridUvTransform) -> u8 {
    if uv.flip_u {
        edges = swap_paint_edges(edges, PAINT_NEIGHBOR_EAST, PAINT_NEIGHBOR_WEST);
    }
    if uv.flip_v {
        edges = swap_paint_edges(edges, PAINT_NEIGHBOR_NORTH, PAINT_NEIGHBOR_SOUTH);
    }
    let quarter_turns = match uv.rotation {
        GridUvRotation::Deg90 => 1,
        GridUvRotation::Deg180 => 2,
        GridUvRotation::Deg270 => 3,
        // A cardinal connected mask cannot exactly follow a diagonal UV
        // rotation. Preserve the authored orientation for those uncommon
        // cases rather than silently snapping the whole texture.
        GridUvRotation::Deg0
        | GridUvRotation::Deg45
        | GridUvRotation::Deg135
        | GridUvRotation::Deg225
        | GridUvRotation::Deg315 => 0,
    };
    for _ in 0..quarter_turns {
        edges = ((edges & PAINT_NEIGHBOR_NORTH) << 1)
            | ((edges & PAINT_NEIGHBOR_EAST) << 1)
            | ((edges & PAINT_NEIGHBOR_SOUTH) << 1)
            | ((edges & PAINT_NEIGHBOR_WEST) >> 3);
    }
    edges
}

fn swap_paint_edges(edges: u8, a: u8, b: u8) -> u8 {
    let without_pair = edges & !(a | b);
    without_pair | if edges & a != 0 { b } else { 0 } | if edges & b != 0 { a } else { 0 }
}

fn layer_neighbor((x, z): (i32, i32), direction: GridDirection) -> Option<(i32, i32)> {
    match direction {
        GridDirection::North => Some((x, z.saturating_add(1))),
        GridDirection::East => Some((x.saturating_add(1), z)),
        GridDirection::South => Some((x, z.saturating_sub(1))),
        GridDirection::West => Some((x.saturating_sub(1), z)),
        GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => None,
    }
}

fn add_model_renderer_node(
    scene: &mut Scene,
    entity: NodeId,
    model_id: ResourceId,
    visual_scale_q8: u16,
    default_visual_yaw_q12: i16,
) -> NodeId {
    let renderer = scene.add_node(
        entity,
        "Model Renderer",
        NodeKind::ModelRenderer {
            model: Some(model_id),
            material: None,
            visual_offset: [0; 3],
            visual_scale_q8,
        },
    );
    if let Some(node) = scene.node_mut(renderer) {
        node.transform.rotation_degrees[1] = q12_turns_to_degrees(default_visual_yaw_q12 as i32);
    }
    renderer
}

fn default_model_visual_defaults(project: &ProjectDocument, model_id: ResourceId) -> (u16, i16) {
    project
        .resource(model_id)
        .and_then(|resource| match &resource.data {
            ResourceData::Model(model) => {
                Some((model.scale_q8[1].max(1), model.default_visual_yaw_q12))
            }
            _ => None,
        })
        .unwrap_or((psxed_project::MODEL_SCALE_ONE_Q8, 0))
}

impl EditorWorkspace {
    /// BSP scenes have no legacy `Section`/Room owner. Their world root is
    /// the correct parent for authored point entities because the PXBSP cook
    /// consumes those transforms directly in world units.
    pub(crate) fn bsp_authoring_root(&self) -> Option<NodeId> {
        let scene = self.project.active_scene();
        (!scene.brushes.is_empty()).then_some(scene.root)
    }

    /// `true` when Material Paint should address BSP brush faces instead of
    /// grid cells: a brush scene with no active grid Room. Grid painting is
    /// untouched, so a project with rooms keeps the cell lane.
    pub(crate) fn bsp_face_paint_active(&self) -> bool {
        self.active_tool == ViewTool::PaintMaterial
            && self.active_room_id().is_none()
            && self.bsp_authoring_root().is_some()
    }

    /// Material Paint against a BSP brush face: assign the picked material to
    /// the face under the pointer, or sample it when the eyedropper is armed.
    ///
    /// One undo step per gesture. `brush_face_paint_stroke` is cleared on the
    /// primary press (see `draw_viewport_3d_body`), so dragging across a whole
    /// wall coalesces into a single snapshot rather than one per face. This is
    /// deliberately stricter than the grid painter, which snapshots per cell.
    pub(crate) fn paint_bsp_brush_face(&mut self, rect: egui::Rect, pointer: egui::Pos2) {
        let Some((brush, face, _)) = self.pick_brush_face_nearest_for_selection_3d(rect, pointer)
        else {
            self.status = if self.material_paint_sampling {
                "Eyedropper needs a BSP brush face under the cursor".to_string()
            } else {
                "Material Paint needs a BSP brush face under the cursor".to_string()
            };
            return;
        };
        if self.material_paint_sampling {
            self.sample_material_from_brush_face(brush, face);
            return;
        }
        let Some(material) = self.paint_material_for("brick") else {
            self.status = "Material Paint needs a Material resource".to_string();
            return;
        };
        let Some(current) = self
            .project
            .active_scene()
            .brushes
            .get(brush)
            .and_then(|brush| brush.faces.get(face))
            .map(|face| face.material)
        else {
            return;
        };
        // Re-painting the same material is a no-op, which also keeps a drag
        // that dwells on one face from churning the document.
        if current == Some(material) {
            return;
        }
        if !self.brush_face_paint_stroke {
            self.push_undo();
            self.brush_face_paint_stroke = true;
        }
        self.project.active_scene_mut().brushes[brush].faces[face].material = Some(material);
        self.mark_dirty();
        let name = self
            .project
            .resource_name(material)
            .unwrap_or("(missing)")
            .to_string();
        self.status = format!("Painted {name} onto brush {} face {}", brush + 1, face + 1);
    }

    /// Eyedropper on a BSP brush face. Writes the picker AND the resource
    /// selection, so the very next paint uses the sampled material.
    fn sample_material_from_brush_face(&mut self, brush: usize, face: usize) {
        let Some(Some(material)) = self
            .project
            .active_scene()
            .brushes
            .get(brush)
            .and_then(|brush| brush.faces.get(face))
            .map(|face| face.material)
        else {
            self.status = format!("Brush {} face {} has no material", brush + 1, face + 1);
            return;
        };
        let name = self
            .project
            .resource_name(material)
            .unwrap_or("(missing)")
            .to_string();
        self.brush_material = Some(material);
        self.replace_resource_selection(material);
        self.material_paint_sampling = false;
        self.status = format!("Sampled {name} from brush {} face {}", brush + 1, face + 1);
        self.mark_shortcut_group_changed(ShortcutGroup::Tool);
    }

    /// Place the active `PlaceKind` on an upward-facing BSP brush surface.
    /// The one-unit lift mirrors the tracked first-playable spawn and keeps
    /// floor-anchored actors out of the solid boundary. Point lights receive
    /// a useful room-height default and remain freely editable afterwards.
    pub(crate) fn place_bsp_on_brush_face(
        &mut self,
        brush_index: usize,
        face_index: usize,
        mut hit: [f32; 3],
    ) -> bool {
        let upward = self
            .project
            .active_scene()
            .brushes
            .get(brush_index)
            .and_then(|brush| brush.faces.get(face_index))
            .and_then(|face| psxed_project::brush::Plane::from_points(face.points))
            .is_some_and(|plane| {
                plane.normal[1] > 0
                    && plane.normal[1].abs() >= plane.normal[0].abs()
                    && plane.normal[1].abs() >= plane.normal[2].abs()
            });
        if !upward {
            self.status = "Place needs an upward-facing BSP surface".to_string();
            return false;
        }
        hit[1] += self.bsp_place_height_offset();
        self.place_bsp_at_world_hit(hit)
    }

    /// Top-view BSP placement chooses the upward face at the clicked X/Z
    /// closest to the shared orthographic Y focus. This makes the initial
    /// focus (`Y=0`) land on a room floor instead of the exterior roof while
    /// still letting authors target upper floors by moving the shared focus.
    pub(crate) fn place_bsp_from_top(&mut self, world: [f32; 2]) -> bool {
        let Some(surface_y) = self.bsp_upward_surface_y(world, self.orthographic_focus[1]) else {
            self.status = "Place over an upward-facing BSP brush surface".to_string();
            return false;
        };
        self.place_bsp_at_world_hit([
            world[0],
            surface_y + self.bsp_place_height_offset(),
            world[1],
        ])
    }

    /// Vertical lift applied to a BSP placement anchor.
    ///
    /// Floor-anchored actors get one unit so they never start inside the solid
    /// boundary, and point lights get a useful room-height default. Logic
    /// nodes get NOTHING: a Trigger Volume's AABB grows upward from its anchor
    /// while the character motor stands its feet exactly on the floor plane,
    /// so a lifted anchor produces a volume that starts one unit above the
    /// player and can never fire.
    fn bsp_place_height_offset(&self) -> f32 {
        match self.place_kind {
            PlaceKind::PointLightMarker => 256.0,
            PlaceKind::Logic => 0.0,
            _ => 1.0,
        }
    }

    fn place_bsp_at_world_hit(&mut self, hit: [f32; 3]) -> bool {
        let Some(root) = self.bsp_authoring_root() else {
            self.status = "Place needs a BSP brush world".to_string();
            return false;
        };
        if self.place_kind == PlaceKind::Portal {
            self.status =
                "Portals are generated by the BSP compiler; no marker is needed".to_string();
            return false;
        }
        let before = self.project.active_scene().nodes().len();
        self.run_paint_action(ViewTool::Place, root, 0, 0, None, hit);
        let placed = self.project.active_scene().nodes().len() > before;
        if placed {
            self.status = format!(
                "Placed {} at {:.0},{:.0},{:.0}",
                self.place_kind.label(),
                hit[0],
                hit[1],
                hit[2]
            );
        }
        placed
    }

    pub(crate) fn bsp_upward_surface_y(&self, world: [f32; 2], focus_y: f32) -> Option<f32> {
        let point = world.map(f64::from);
        let mut best: Option<(f64, f64)> = None;
        for brush in &self.project.active_scene().brushes {
            let solved = brush.solve();
            if !solved.is_valid() {
                continue;
            }
            for (face_index, polygon) in solved.polygons.iter().enumerate() {
                let Some(polygon) = polygon else { continue };
                let Some(face) = brush.faces.get(face_index) else {
                    continue;
                };
                let Some(plane) = psxed_project::brush::Plane::from_points(face.points) else {
                    continue;
                };
                if plane.normal[1] <= 0
                    || plane.normal[1].abs() < plane.normal[0].abs()
                    || plane.normal[1].abs() < plane.normal[2].abs()
                {
                    continue;
                }
                let projected = polygon
                    .verts
                    .iter()
                    .map(|vertex| [vertex[0], vertex[2]])
                    .collect::<Vec<_>>();
                if !point_in_convex_xz(point, &projected) {
                    continue;
                }
                let normal = plane.normal.map(|component| component as f64);
                let y =
                    (plane.dist as f64 - normal[0] * point[0] - normal[2] * point[1]) / normal[1];
                let distance = (y - f64::from(focus_y)).abs();
                if best.is_none_or(|(best_distance, best_y)| {
                    distance < best_distance || (distance == best_distance && y < best_y)
                }) {
                    best = Some((distance, y));
                }
            }
        }
        best.map(|(_, y)| y as f32)
    }

    /// 3D paint / move click handler. `face_hit` is the ray-test
    /// result (`pick_face_with_hit`) and `fallback_hit` is the
    /// tool-specific plane pick for cases where the face hit should
    /// not own the action. Ceiling paint intentionally ignores floor
    /// and wall face hits so it can target the ceiling plane.
    pub(crate) fn dispatch_paint_3d(
        &mut self,
        face_hit: Option<(FaceRef, [f32; 3])>,
        fallback_hit: Option<[f32; 2]>,
    ) {
        let Some(room_id) = self.active_room_id() else {
            return;
        };
        let paint_tool = matches!(
            self.active_tool,
            ViewTool::PaintFloor
                | ViewTool::PaintWall
                | ViewTool::PaintCeiling
                | ViewTool::PaintMaterial
                | ViewTool::Water
                | ViewTool::Erase
                | ViewTool::Place
        );
        let portal_tool = self.portal_place_active();
        let face_hit = self.face_hit_for_paint_tool(face_hit);
        if self.active_tool == ViewTool::PaintMaterial && face_hit.is_none() {
            self.status = if self.material_paint_sampling {
                "Eyedropper needs an existing surface under the cursor".to_string()
            } else {
                "Material Paint needs an existing surface under the cursor".to_string()
            };
            return;
        }
        if self.active_tool == ViewTool::PaintMaterial && self.material_paint_sampling {
            if let Some((face, _)) = face_hit {
                self.sample_paint_material_from_face(face);
            }
            return;
        }
        let (cell, hit_world) = match face_hit {
            Some((face, hit)) => ((face.sx, face.sz), hit),
            None => {
                let Some(world) = fallback_hit else {
                    return;
                };
                // Click outside the existing grid? Auto-grow on
                // paint/place clicks so the user can extend a room
                // by stamping a floor in empty space -- Sims-style.
                // Move just bails (it never made sense for it to
                // grow the room).
                let cell = if paint_tool && !portal_tool && self.active_tool != ViewTool::Water {
                    self.ensure_cell_in_grid(room_id, world)
                } else {
                    self.world_to_sector(room_id, world)
                };
                let Some((sx, sz)) = cell else {
                    return;
                };
                let raw_hit = self.editor_world_to_world3(room_id, world);
                ((sx, sz), raw_hit)
            }
        };
        let (sx, sz) = cell;
        let stamp = self.paint_stamp_for(room_id, sx, sz, face_hit, hit_world);
        if self.last_paint_stamp == Some(stamp) {
            return;
        }
        self.last_paint_stamp = Some(stamp);
        if portal_tool {
            self.clear_sector_selection();
        } else if matches!(self.active_tool, ViewTool::PaintMaterial | ViewTool::Water) {
            // Paint hover/click feedback is not selection. Keep the current
            // resource choice, but never create a tile/face selection.
            self.clear_sector_selection();
            self.clear_primitive_selection_state();
        } else {
            self.selection.selected_sector = Some((sx, sz));
        }
        let tool = self.active_tool;
        self.run_paint_action(tool, room_id, sx, sz, face_hit.map(|(f, _)| f), hit_world)
    }

    /// Drop a resource into a BSP scene: models/characters/weapons land on
    /// the upward brush face under the pointer (like the Place tool);
    /// materials assign to whichever brush face is hit.
    pub(crate) fn drop_resource_bsp_3d(
        &mut self,
        resource_id: ResourceId,
        rect: egui::Rect,
        pointer: egui::Pos2,
    ) {
        let Some(root) = self.bsp_authoring_root() else {
            self.status = "Drop needs a BSP brush world".to_string();
            return;
        };
        let Some(resource) = self.project.resource(resource_id) else {
            self.status = format!("Resource #{} no longer exists", resource_id.raw());
            return;
        };
        if matches!(resource.data, psxed_project::ResourceData::Material(_)) {
            let name = resource.name.clone();
            let Some((brush, face, _)) = self.pick_brush_face_with_hit(rect, pointer) else {
                self.status = "Drop the material onto a brush face".to_string();
                return;
            };
            self.push_undo();
            if let Some(face_data) = self
                .project
                .active_scene_mut()
                .brushes
                .get_mut(brush)
                .and_then(|brush| brush.faces.get_mut(face))
            {
                face_data.material = Some(resource_id);
                self.replace_brush_selection(brush, Some(face));
                self.clear_node_selection_state();
                self.status = format!("Assigned {} to brush {} face {}", name, brush + 1, face);
                self.mark_dirty();
            }
            return;
        }
        let Some((brush, face, mut hit)) = self.pick_brush_face_with_hit(rect, pointer) else {
            self.status = "Drop onto an upward-facing BSP brush surface".to_string();
            return;
        };
        let upward = self
            .project
            .active_scene()
            .brushes
            .get(brush)
            .and_then(|brush| brush.faces.get(face))
            .and_then(|face| psxed_project::brush::Plane::from_points(face.points))
            .is_some_and(|plane| {
                plane.normal[1] > 0
                    && plane.normal[1].abs() >= plane.normal[0].abs()
                    && plane.normal[1].abs() >= plane.normal[2].abs()
            });
        if !upward {
            self.status = "Drop onto an upward-facing BSP brush surface".to_string();
            return;
        }
        // Same one-unit floor clearance the Place tool applies.
        hit[1] += 1.0;
        self.drop_resource_at_room_hit(resource_id, root, hit, None);
    }

    pub(crate) fn drop_resource_3d(
        &mut self,
        resource_id: ResourceId,
        face_hit: Option<(FaceRef, [f32; 3])>,
        ground_hit: Option<[f32; 2]>,
    ) {
        if let Some((face, hit_world)) = face_hit {
            self.drop_resource_at_room_hit(resource_id, face.room, hit_world, Some(face));
            return;
        }

        let Some(room_id) = self.active_room_id() else {
            self.status = "Drop needs an active Room".to_string();
            return;
        };
        let Some(editor_world) = ground_hit else {
            self.status = "Drop onto the room floor or an existing face".to_string();
            return;
        };
        let Some((_sx, _sz)) = self.ensure_cell_in_grid(room_id, editor_world) else {
            return;
        };
        let hit_world = self.editor_world_to_world3(room_id, editor_world);
        self.drop_resource_at_room_hit(resource_id, room_id, hit_world, None);
    }

    pub(crate) fn drop_resource_2d(&mut self, resource_id: ResourceId, editor_world: [f32; 2]) {
        let Some(room_id) = self.active_room_id() else {
            self.status = "Drop needs an active Room".to_string();
            return;
        };
        let Some((_sx, _sz)) = self.ensure_cell_in_grid(room_id, editor_world) else {
            return;
        };
        let hit_world = self.editor_world_to_world3(room_id, editor_world);
        self.drop_resource_at_room_hit(resource_id, room_id, hit_world, None);
    }

    pub(crate) fn drop_resource_at_room_hit(
        &mut self,
        resource_id: ResourceId,
        room_id: NodeId,
        hit_world: [f32; 3],
        face: Option<FaceRef>,
    ) {
        let Some(resource) = self.project.resource(resource_id).cloned() else {
            self.status = format!("Resource #{} no longer exists", resource_id.raw());
            return;
        };

        match resource.data {
            ResourceData::Model(_) => {
                let translation = self.placement_translation_for_room_hit(room_id, hit_world);
                if let Some(existing) =
                    self.find_duplicate_model_entity(room_id, resource_id, translation)
                {
                    self.reject_duplicate_placement(existing, "Prop");
                    return;
                }
                self.push_undo();
                let node = self.create_model_entity_at_room_hit(
                    room_id,
                    resource_id,
                    &resource.name,
                    hit_world,
                );
                self.replace_node_selection(node);
                self.clear_resource_selection_state();
                self.clear_primitive_selection_state();
                self.status = format!("Created Prop Entity from model {}", resource.name);
                self.mark_dirty();
            }
            ResourceData::Character(character) => {
                let translation = self.placement_translation_for_room_hit(room_id, hit_world);
                if let Some(existing) =
                    self.find_duplicate_character_entity(room_id, resource_id, translation)
                {
                    self.reject_duplicate_placement(existing, "Character");
                    return;
                }
                self.push_undo();
                let player = match character.spawn_role {
                    psxed_project::CharacterSpawnRole::Auto => !self.has_player_source(),
                    psxed_project::CharacterSpawnRole::Player => true,
                    psxed_project::CharacterSpawnRole::Enemy => false,
                };
                if player && character.spawn_role == psxed_project::CharacterSpawnRole::Player {
                    self.demote_player_sources_except(None);
                }
                let idle_clip = self.resolve_character_idle_preview_clip(&character);
                let settings = CharacterControllerSettings::from_character(&character);
                let camera_settings = character.camera_settings();
                let node = self.create_character_entity_at_room_hit(
                    room_id,
                    resource_id,
                    &resource.name,
                    character.model,
                    character.material,
                    idle_clip,
                    settings,
                    player,
                    camera_settings,
                    hit_world,
                );
                self.replace_node_selection(node);
                self.clear_resource_selection_state();
                self.clear_primitive_selection_state();
                self.status = if player {
                    format!(
                        "Created Player Character Entity from profile {}",
                        resource.name
                    )
                } else {
                    format!("Created Character Entity from profile {}", resource.name)
                };
                self.mark_dirty();
            }
            ResourceData::Weapon(weapon) => {
                let translation = self.placement_translation_for_room_hit(room_id, hit_world);
                if let Some(existing) =
                    self.find_duplicate_weapon_entity(room_id, resource_id, translation)
                {
                    self.reject_duplicate_placement(existing, "Weapon");
                    return;
                }
                self.push_undo();
                let node = self.create_weapon_entity_at_room_hit(
                    room_id,
                    resource_id,
                    &resource.name,
                    weapon.model,
                    weapon.default_character_socket.as_str(),
                    weapon.grip.name.as_str(),
                    hit_world,
                );
                self.replace_node_selection(node);
                self.clear_resource_selection_state();
                self.selection.selected_primitive = None;
                self.status = format!("Created Weapon Entity from resource {}", resource.name);
                self.mark_dirty();
            }
            ResourceData::Material(_) => {
                let Some(face) = face else {
                    self.status = "Drop Material onto an existing face".to_string();
                    return;
                };
                if self.assign_face_material(face, Some(resource_id)) {
                    self.replace_resource_selection(resource_id);
                    if self.active_tool != ViewTool::PaintMaterial {
                        self.replace_primitive_selection(Selection::Face(face));
                    }
                    self.status = format!("Assigned {} to {}", resource.name, describe_face(face));
                }
            }
            _ => {
                self.status = format!(
                    "Drag Model, Character Profile, or Weapon resources into the scene; {} is not placeable",
                    resource.data.label()
                );
            }
        }
    }

    pub(crate) fn placement_translation_for_room_hit(
        &self,
        room_id: NodeId,
        hit_world: [f32; 3],
    ) -> [f32; 3] {
        let editor = self
            .room_grid_view(room_id)
            .map(|grid| grid.room_local_to_editor(hit_world))
            .unwrap_or([hit_world[0], hit_world[2]]);
        // Preserve the picked surface height instead of pinning placed
        // content to the room floor. `hit_world` is room-local engine
        // units; `translation[1]` is authored in sectors (the same
        // convention `node_preview_origin` reads back as
        // `translation[1] * sector_size`), so divide the hit Y by the
        // sector size. A floor-plane pick reports `hit_world[1] == 0`,
        // which keeps ground placement identical to the old behaviour;
        // clicking a raised floor now drops the node onto that level,
        // the first lever a user reaches for when stacking rooms.
        let sector_size = self.room_sector_size(room_id).unwrap_or(1) as f32;
        let y = if sector_size > 0.0 {
            hit_world[1] / sector_size
        } else {
            0.0
        };
        [editor[0], y, editor[1]]
    }

    pub(crate) fn create_model_entity_at_room_hit(
        &mut self,
        room_id: NodeId,
        model_id: ResourceId,
        name: &str,
        hit_world: [f32; 3],
    ) -> NodeId {
        let translation = self.placement_translation_for_room_hit(room_id, hit_world);
        let active_floor = self.active_floor;
        let (visual_scale_q8, default_visual_yaw_q12) =
            default_model_visual_defaults(&self.project, model_id);
        let scene = self.project.active_scene_mut();
        let entity = scene.add_node(room_id, name.to_string(), NodeKind::Entity);
        if let Some(node) = scene.node_mut(entity) {
            node.transform.translation = translation;
            // Record the placed floor (0 = ground) so the cook binds the
            // entity to the right runtime room; Y can't select the floor.
            node.floor = active_floor;
        }
        add_model_renderer_node(
            scene,
            entity,
            model_id,
            visual_scale_q8,
            default_visual_yaw_q12,
        );
        scene.add_node(
            entity,
            "Animator",
            NodeKind::Animator {
                clip: None,
                action_clips: Vec::new(),
                autoplay: true,
                pose_frame: 0,
            },
        );
        entity
    }

    pub(crate) fn create_weapon_entity_at_room_hit(
        &mut self,
        room_id: NodeId,
        weapon_id: ResourceId,
        name: &str,
        model_id: Option<ResourceId>,
        character_socket: &str,
        weapon_grip: &str,
        hit_world: [f32; 3],
    ) -> NodeId {
        let translation = self.placement_translation_for_room_hit(room_id, hit_world);
        let active_floor = self.active_floor;
        let model_visual_defaults =
            model_id.map(|model_id| default_model_visual_defaults(&self.project, model_id));
        let scene = self.project.active_scene_mut();
        let entity = scene.add_node(room_id, name.to_string(), NodeKind::Entity);
        if let Some(node) = scene.node_mut(entity) {
            node.transform.translation = translation;
            // Record the placed floor (0 = ground) so the cook binds the
            // entity to the right runtime room; Y can't select the floor.
            node.floor = active_floor;
        }
        if let Some(model_id) = model_id {
            let (visual_scale_q8, default_visual_yaw_q12) =
                model_visual_defaults.unwrap_or((psxed_project::MODEL_SCALE_ONE_Q8, 0));
            add_model_renderer_node(
                scene,
                entity,
                model_id,
                visual_scale_q8,
                default_visual_yaw_q12,
            );
        }
        scene.add_node(
            entity,
            "Equipment",
            NodeKind::Equipment {
                weapon: Some(weapon_id),
                character_socket: character_socket.to_string(),
                weapon_grip: weapon_grip.to_string(),
            },
        );
        entity
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create_character_entity_at_room_hit(
        &mut self,
        room_id: NodeId,
        character_id: ResourceId,
        name: &str,
        model_id: Option<ResourceId>,
        material_id: Option<ResourceId>,
        idle_clip: Option<u16>,
        settings: CharacterControllerSettings,
        player: bool,
        camera_settings: WorldCameraSettings,
        hit_world: [f32; 3],
    ) -> NodeId {
        let translation = self.placement_translation_for_room_hit(room_id, hit_world);
        let active_floor = self.active_floor;
        let model_visual_defaults =
            model_id.map(|model_id| default_model_visual_defaults(&self.project, model_id));
        let scene = self.project.active_scene_mut();
        let entity = scene.add_node(room_id, name.to_string(), NodeKind::Entity);
        if let Some(node) = scene.node_mut(entity) {
            node.transform.translation = translation;
            // Record the placed floor (0 = ground) so the cook binds the
            // entity to the right runtime room; Y can't select the floor.
            node.floor = active_floor;
        }
        if let Some(model_id) = model_id {
            let (visual_scale_q8, default_visual_yaw_q12) =
                model_visual_defaults.unwrap_or((psxed_project::MODEL_SCALE_ONE_Q8, 0));
            let renderer = add_model_renderer_node(
                scene,
                entity,
                model_id,
                visual_scale_q8,
                default_visual_yaw_q12,
            );
            if let Some(node) = scene.node_mut(renderer) {
                if let NodeKind::ModelRenderer { material, .. } = &mut node.kind {
                    *material = material_id;
                }
            }
            scene.add_node(
                entity,
                "Animator",
                NodeKind::Animator {
                    clip: idle_clip,
                    action_clips: Vec::new(),
                    autoplay: true,
                    pose_frame: 0,
                },
            );
        }
        scene.add_node(
            entity,
            "Character Controller",
            NodeKind::CharacterController {
                character: Some(character_id),
                // No override: the placement inherits the Character, so later
                // edits to the type reach it. `settings` is materialised only
                // when this placement is actually tuned away from its type.
                settings: None,
                player,
            },
        );
        if player {
            scene.add_node(
                entity,
                "Camera",
                NodeKind::Camera {
                    settings: camera_settings,
                },
            );
        }
        entity
    }

    pub(crate) fn find_duplicate_model_entity(
        &self,
        room_id: NodeId,
        model_id: ResourceId,
        translation: [f32; 3],
    ) -> Option<NodeId> {
        self.find_duplicate_room_child(room_id, translation, |scene, node| {
            matches!(node.kind, NodeKind::Entity)
                && entity_model_resource_id(scene, node) == Some(model_id)
                && entity_character_component_resource_id(scene, node).is_none()
                && entity_weapon_resource_id(scene, node).is_none()
        })
    }

    pub(crate) fn find_duplicate_character_entity(
        &self,
        room_id: NodeId,
        character_id: ResourceId,
        translation: [f32; 3],
    ) -> Option<NodeId> {
        self.find_duplicate_room_child(room_id, translation, |scene, node| {
            matches!(node.kind, NodeKind::Entity)
                && entity_character_component_resource_id(scene, node) == Some(character_id)
        })
    }

    pub(crate) fn find_duplicate_weapon_entity(
        &self,
        room_id: NodeId,
        weapon_id: ResourceId,
        translation: [f32; 3],
    ) -> Option<NodeId> {
        self.find_duplicate_room_child(room_id, translation, |scene, node| {
            matches!(node.kind, NodeKind::Entity)
                && entity_weapon_resource_id(scene, node) == Some(weapon_id)
        })
    }

    pub(crate) fn find_duplicate_image_prop(
        &self,
        room_id: NodeId,
        material_id: ResourceId,
        translation: [f32; 3],
    ) -> Option<NodeId> {
        self.find_duplicate_room_child(room_id, translation, |_, node| {
            matches!(
                node.kind,
                NodeKind::ImageProp {
                    material: Some(id),
                    ..
                } if id == material_id
            )
        })
    }

    pub(crate) fn find_duplicate_box_prop(
        &self,
        room_id: NodeId,
        material_id: Option<ResourceId>,
        translation: [f32; 3],
    ) -> Option<NodeId> {
        self.find_duplicate_room_child(room_id, translation, |_, node| {
            matches!(
                node.kind,
                NodeKind::BoxProp { materials, .. }
                    if material_id.map_or_else(
                        || materials.iter().all(Option::is_none),
                        |material| materials.contains(&Some(material)),
                    )
            )
        })
    }

    pub(crate) fn find_duplicate_cylinder_prop(
        &self,
        room_id: NodeId,
        material_id: Option<ResourceId>,
        translation: [f32; 3],
    ) -> Option<NodeId> {
        self.find_duplicate_room_child(room_id, translation, |_, node| {
            matches!(
                node.kind,
                NodeKind::CylinderProp { materials, .. }
                    if material_id.map_or_else(
                        || materials.iter().all(Option::is_none),
                        |material| materials.contains(&Some(material)),
                    )
            )
        })
    }

    pub(crate) fn find_duplicate_spawn_marker(
        &self,
        room_id: NodeId,
        player: bool,
        translation: [f32; 3],
    ) -> Option<NodeId> {
        self.find_duplicate_room_child(room_id, translation, |_, node| {
            matches!(
                node.kind,
                NodeKind::SpawnPoint {
                    player: node_player,
                    character: None,
                } if node_player == player
            )
        })
    }

    pub(crate) fn find_duplicate_point_light(
        &self,
        room_id: NodeId,
        translation: [f32; 3],
    ) -> Option<NodeId> {
        self.find_duplicate_room_child(room_id, translation, |_, node| {
            matches!(node.kind, NodeKind::PointLight { .. })
        })
    }

    pub(crate) fn find_duplicate_particle_emitter(
        &self,
        room_id: NodeId,
        translation: [f32; 3],
    ) -> Option<NodeId> {
        self.find_duplicate_room_child(room_id, translation, |_, node| {
            matches!(node.kind, NodeKind::ParticleEmitter { .. })
        })
    }

    pub(crate) fn find_duplicate_portal(
        &self,
        room_id: NodeId,
        translation: [f32; 3],
    ) -> Option<NodeId> {
        self.find_duplicate_room_child(room_id, translation, |_, node| {
            matches!(node.kind, NodeKind::Portal { .. })
        })
    }

    pub(crate) fn find_duplicate_room_child<F>(
        &self,
        room_id: NodeId,
        translation: [f32; 3],
        mut matches_placement: F,
    ) -> Option<NodeId>
    where
        F: FnMut(&Scene, &SceneNode) -> bool,
    {
        let scene = self.project.active_scene();
        let room = scene.node(room_id)?;
        room.children.iter().copied().find(|child_id| {
            scene.node(*child_id).is_some_and(|node| {
                translations_match(node.transform.translation, translation)
                    && matches_placement(scene, node)
            })
        })
    }

    pub(crate) fn reject_duplicate_placement(&mut self, existing: NodeId, label: &str) {
        self.replace_node_selection(existing);
        self.clear_resource_selection_state();
        self.clear_primitive_selection_state();
        self.status = format!("{label} already exists at this position");
    }

    pub(crate) fn resolve_character_idle_preview_clip(
        &self,
        character: &psxed_project::CharacterResource,
    ) -> Option<u16> {
        character.model.and_then(|model_id| {
            self.project.resource(model_id).and_then(|resource| {
                let ResourceData::Model(model) = &resource.data else {
                    return None;
                };
                psxed_project::resolve::resolve_character_idle_preview_clip_for_model(
                    &self.project,
                    character,
                    model_id,
                    model,
                )
            })
        })
    }

    /// Build the dedupe key for the next paint dispatch. PaintWall
    /// records the targeted edge so dragging across edges of the
    /// same cell stamps each one (different stamps), but dwelling
    /// on the same edge during drag dedupes even though each commit
    /// creates a new stack index.
    /// Other tools key on cell + tool only -- drag-restamping a
    /// floor with the same material is a no-op anyway.
    pub(crate) fn paint_stamp_for(
        &self,
        room_id: NodeId,
        sx: u16,
        sz: u16,
        face_hit: Option<(FaceRef, [f32; 3])>,
        hit_world: [f32; 3],
    ) -> PaintStamp {
        let triangle = self
            .horizontal_paint_triangle_target(self.active_tool, room_id, sx, sz, hit_world)
            .map(|triangle| triangle.index);
        let edge = if self.portal_place_active() {
            Some(self.portal_place_direction)
        } else if matches!(self.active_tool, ViewTool::PaintWall) {
            match face_hit {
                Some((
                    FaceRef {
                        kind: FaceKind::Wall { dir, .. },
                        ..
                    },
                    _,
                )) => Some(dir),
                _ => {
                    let center = self
                        .room_grid_view(room_id)
                        .map(|grid| grid.cell_center_world(sx, sz))
                        .unwrap_or([0.0, 0.0]);
                    let dir = self
                        .wall_paint_shape
                        .direction(hit_world[0] - center[0], hit_world[2] - center[1]);
                    Some(dir)
                }
            }
        } else {
            None
        };
        PaintStamp {
            room: room_id,
            sx,
            sz,
            tool: self.active_tool,
            triangle,
            edge,
            stack: None,
        }
    }

    /// Resolve the cell `world` lands in, growing the room's grid
    /// in any direction if the click falls beyond the current
    /// footprint. Negative-side growth re-anchors via
    /// `WorldGrid::origin` so existing geometry keeps its world
    /// position. Returns `None` only when the requested cell sits
    /// past the safety cap.
    pub(crate) fn ensure_cell_in_grid(
        &mut self,
        room_id: NodeId,
        world: [f32; 2],
    ) -> Option<(u16, u16)> {
        const AUTO_GROW_LIMIT: u16 = 64;
        if let Some(cell) = self.world_to_sector(room_id, world) {
            return Some(cell);
        }
        let grid = self.room_grid_view(room_id)?;
        // `world` here is already in editor-cell units (sector-units,
        // room-centre-relative -- the 2D viewport's native space).
        // Route through the canonical helper so this stays exactly
        // inverse to `world_to_sector`'s lookup.
        let editor_to_world = grid.editor_to_world_cells(world);
        let wcx = editor_to_world[0].floor() as i32;
        let wcz = editor_to_world[1].floor() as i32;
        // Cap the request before mutating so a wild click can't
        // explode the sector vec. The cap covers the post-grow
        // dimensions in either direction.
        let projected_w = grid.width as i32
            + (wcx - grid.origin[0] - grid.width as i32 + 1).max(0)
            + (grid.origin[0] - wcx).max(0);
        let projected_d = grid.depth as i32
            + (wcz - grid.origin[1] - grid.depth as i32 + 1).max(0)
            + (grid.origin[1] - wcz).max(0);
        if projected_w as u32 > AUTO_GROW_LIMIT as u32
            || projected_d as u32 > AUTO_GROW_LIMIT as u32
        {
            self.status =
                format!("Auto-grow capped at {AUTO_GROW_LIMIT} - resize the grid manually");
            return None;
        }
        self.push_undo();
        let active_floor = self.active_floor;
        let scene = self.project.active_scene_mut();
        let cell = extend_room_grid_to_include_preserving_child_positions(
            scene,
            room_id,
            wcx,
            wcz,
            active_floor,
        )?;
        let node = scene.node(room_id)?;
        let NodeKind::Section { grid } = &node.kind else {
            return None;
        };
        let grid = grid.floor(active_floor).unwrap_or(grid);
        self.status = format!(
            "Grew grid to {}×{} (origin {},{})",
            grid.width, grid.depth, grid.origin[0], grid.origin[1]
        );
        self.mark_dirty();
        Some(cell)
    }

    /// World-space sector size of the named Room, or `None` if the
    /// node isn't a Room.
    pub(crate) fn room_sector_size(&self, room_id: NodeId) -> Option<i32> {
        let node = self.project.active_scene().node(room_id)?;
        match &node.kind {
            NodeKind::Section { grid } => Some(grid.sector_size),
            _ => None,
        }
    }

    /// Convert an editor (sector-units, room-centre-relative) hit
    /// position to a raw world `[x, 0, z]` triple. Thin shim over
    /// `WorldGrid::editor_to_room_local` so `pick_3d_world` and this
    /// stay exact inverses by construction.
    pub(crate) fn editor_world_to_world3(&self, room_id: NodeId, editor: [f32; 2]) -> [f32; 3] {
        self.room_grid_view(room_id)
            .map(|grid| grid.editor_to_room_local(editor))
            .unwrap_or([0.0, 0.0, 0.0])
    }

    /// Borrow the named Room's grid for the duration of `&self`,
    /// or `None` if the node isn't a Room. Avoids the
    /// `node.kind` matching dance at every cell-coord call site.
    pub(crate) fn room_grid_view(&self, room_id: NodeId) -> Option<&WorldGrid> {
        let node = self.project.active_scene().node(room_id)?;
        match &node.kind {
            NodeKind::Section { grid } => {
                // Route every editor read (render overlays, hit-test,
                // selection, paint preview) to the active floor so the
                // Room workspace edits the floor the user is on. Floor 0
                // is the base grid; clamp keeps a room with fewer floors
                // (or a stale index after switching rooms) valid.
                let floor = self.active_floor.min(grid.floor_count().saturating_sub(1));
                grid.floor(floor)
            }
            _ => None,
        }
    }

    /// Active floor index the Room workspace is authoring (0 = base).
    pub fn active_floor(&self) -> usize {
        self.active_floor
    }

    /// The room the floor stepper targets: the active room, else the
    /// first room in the scene.
    pub(crate) fn floors_target_room(&self) -> Option<NodeId> {
        self.active_room_id().or_else(|| {
            self.project
                .active_scene()
                .nodes()
                .iter()
                .find(|node| matches!(node.kind, NodeKind::Section { .. }))
                .map(|node| node.id)
        })
    }

    /// Base (floor 0) grid for a room, unrouted by `active_floor`. Use
    /// this for whole-room queries like the floor count; use
    /// [`Self::room_grid_view`] for active-floor reads.
    pub(crate) fn room_base_grid(&self, room_id: NodeId) -> Option<&WorldGrid> {
        match &self.project.active_scene().node(room_id)?.kind {
            NodeKind::Section { grid } => Some(grid),
            _ => None,
        }
    }

    /// Active floor's grid for a room, mutable. Routes every floor-aware
    /// edit (paint, height drag, material, vertex, erase, resize, grow)
    /// to the floor the user is on. Floor 0 is the base grid; the index
    /// is clamped against the room's floor count so a stale value after
    /// switching rooms can't go out of range.
    pub(crate) fn room_floor_grid_mut(&mut self, room: NodeId) -> Option<&mut WorldGrid> {
        let active_floor = self.active_floor;
        match &mut self.project.active_scene_mut().node_mut(room)?.kind {
            NodeKind::Section { grid } => {
                let idx = active_floor.min(grid.floor_count().saturating_sub(1));
                grid.floor_mut(idx)
            }
            _ => None,
        }
    }

    /// Step up one floor, adding a new empty floor above the top when
    /// already on the highest floor.
    pub(crate) fn floor_up(&mut self) {
        let Some(room_id) = self.floors_target_room() else {
            return;
        };
        let active_floor = self.active_floor;
        let before = self.project.clone();
        let pushed = {
            let Some(node) = self.project.active_scene_mut().node_mut(room_id) else {
                return;
            };
            let NodeKind::Section { grid } = &mut node.kind else {
                return;
            };
            if active_floor + 1 >= grid.floor_count() {
                grid.push_floor();
                true
            } else {
                false
            }
        };
        self.active_floor = active_floor + 1;
        if pushed {
            self.history.record(before);
            self.mark_dirty();
            self.status = format!("Added floor {} (paint to build it)", self.active_floor + 1);
        } else {
            self.status = format!("Floor {}", self.active_floor + 1);
        }
    }

    /// Step down one floor (no-op on the base floor).
    pub(crate) fn floor_down(&mut self) {
        if self.active_floor > 0 {
            self.active_floor -= 1;
            self.status = format!("Floor {}", self.active_floor + 1);
        }
    }

    /// Whether the active layer has no tile geometry and another layer can
    /// replace it. Layer-owned nodes are preserved and remapped by
    /// [`Self::delete_active_empty_layer`].
    pub(crate) fn can_delete_active_empty_layer(&self) -> bool {
        let Some(room) = self.floors_target_room() else {
            return false;
        };
        let Some(grid) = self.room_base_grid(room) else {
            return false;
        };
        let active = self.active_floor.min(grid.floor_count().saturating_sub(1));
        grid.floor_count() > 1
            && grid
                .floor(active)
                .is_some_and(|floor| floor.populated_sector_count() == 0)
    }

    /// Remove an empty layer and repair every floor-indexed reference without
    /// creating an undo entry. Callers own undo, selection, status, and dirty
    /// state so this can be folded into a larger authoring action such as
    /// deleting the last tiles on a layer.
    pub(crate) fn remove_empty_layer_in_room(
        &mut self,
        room: NodeId,
        floor_index: usize,
    ) -> Option<(usize, usize)> {
        let (replacement, new_floor_count) = {
            let scene = self.project.active_scene_mut();
            let room_node = scene.node_mut(room)?;
            let NodeKind::Section { grid } = &mut room_node.kind else {
                return None;
            };
            if grid.floor_count() <= 1
                || grid
                    .floor(floor_index)
                    .is_none_or(|floor| floor.populated_sector_count() != 0)
            {
                return None;
            }

            let room_origin_y = room_node.transform.translation[1] * grid.sector_size.max(1) as f32;
            let elevation_delta = grid.remove_empty_floor(floor_index)?;
            if floor_index == 0 {
                room_node.transform.translation[1] =
                    (room_origin_y + elevation_delta as f32) / grid.sector_size.max(1) as f32;
            }
            let new_floor_count = grid.floor_count();
            (
                floor_index.min(new_floor_count.saturating_sub(1)),
                new_floor_count,
            )
        };

        let scene = self.project.active_scene_mut();
        let descendant_ids: Vec<NodeId> = scene
            .nodes()
            .iter()
            .filter(|node| node.id != room && scene.is_descendant_of(node.id, room))
            .map(|node| node.id)
            .collect();
        for id in descendant_ids {
            if let Some(node) = scene.node_mut(id) {
                node.floor = if node.floor > floor_index {
                    node.floor - 1
                } else if node.floor == floor_index {
                    replacement
                } else {
                    node.floor
                };
            }
        }

        let room_ids: Vec<NodeId> = scene
            .nodes()
            .iter()
            .filter(|node| matches!(node.kind, NodeKind::Section { .. }))
            .map(|node| node.id)
            .collect();
        for room_id in room_ids {
            let Some(node) = scene.node_mut(room_id) else {
                continue;
            };
            let NodeKind::Section { grid } = &mut node.kind else {
                continue;
            };
            for current_floor in 0..grid.floor_count() {
                let Some(floor) = grid.floor_mut(current_floor) else {
                    continue;
                };
                for sector in floor.sectors.iter_mut().flatten() {
                    remap_deleted_floor_target(
                        &mut sector.floor_above,
                        room,
                        floor_index,
                        replacement,
                    );
                    remap_deleted_floor_target(
                        &mut sector.floor_below,
                        room,
                        floor_index,
                        replacement,
                    );
                }
            }
        }

        self.active_floor = replacement;
        Some((replacement, new_floor_count))
    }

    /// Delete the active empty layer as one undoable edit. Removing the base
    /// promotes layer two and offsets the Room node so the promoted geometry
    /// does not move in world space. Nodes and authored floor-link targets are
    /// compacted to the surviving layer indices instead of being discarded.
    pub(crate) fn delete_active_empty_layer(&mut self) {
        let Some(room) = self.floors_target_room() else {
            self.status = "Layer deletion needs an active Room".to_string();
            return;
        };
        let Some(base) = self.room_base_grid(room) else {
            return;
        };
        let active = self.active_floor.min(base.floor_count().saturating_sub(1));
        if base.floor_count() <= 1 {
            self.status = "A room must keep at least one layer".to_string();
            return;
        }
        if base
            .floor(active)
            .is_none_or(|floor| floor.populated_sector_count() != 0)
        {
            self.status = "Delete this layer's tile geometry first".to_string();
            return;
        }

        let before = self.project.clone();
        let Some((replacement, new_floor_count)) = self.remove_empty_layer_in_room(room, active)
        else {
            return;
        };
        self.clear_sector_selection();
        self.clear_primitive_selection_state();
        self.history.record(before);
        self.mark_dirty();
        self.status = format!(
            "Deleted empty layer {}; now editing layer {} of {}",
            active + 1,
            replacement + 1,
            new_floor_count
        );
    }

    fn selected_layer_footprint(&self, room: NodeId) -> Vec<LayerFootprintCell> {
        let Some(base) = self.room_base_grid(room) else {
            return Vec::new();
        };
        let floor_index = self.active_floor.min(base.floor_count().saturating_sub(1));
        let Some(grid) = base.floor(floor_index) else {
            return Vec::new();
        };

        let mut cells = Vec::new();
        for &(selected_room, sx, sz) in &self.selection.selected_sectors {
            if selected_room == room && !cells.contains(&(sx, sz)) {
                cells.push((sx, sz));
            }
        }
        for selection in self.selected_primitive_targets() {
            let (selected_room, sx, sz) = selection_sector(selection);
            if selected_room == room && !cells.contains(&(sx, sz)) {
                cells.push((sx, sz));
            }
        }
        if cells.is_empty() {
            if let Some((sx, sz)) = self.selection.selected_sector {
                cells.push((sx, sz));
            }
        }
        cells.sort_unstable();

        cells
            .into_iter()
            .filter(|&(sx, sz)| sx < grid.width && sz < grid.depth)
            .map(|(sx, sz)| {
                let sector = grid.sector(sx, sz);
                let wall_material = sector.and_then(|sector| {
                    GridDirection::CARDINAL.iter().find_map(|&direction| {
                        sector
                            .walls
                            .get(direction)
                            .iter()
                            .find_map(|wall| wall.material)
                    })
                });
                LayerFootprintCell {
                    world: (
                        grid.origin[0].saturating_add(i32::from(sx)),
                        grid.origin[1].saturating_add(i32::from(sz)),
                    ),
                    floor_material: sector
                        .and_then(|sector| sector.floor.as_ref())
                        .and_then(|face| face.material),
                    ceiling_material: sector
                        .and_then(|sector| sector.ceiling.as_ref())
                        .and_then(|face| face.material),
                    wall_material,
                }
            })
            .collect()
    }

    pub(crate) fn can_author_selected_layer_footprint(&self) -> bool {
        self.floors_target_room()
            .is_some_and(|room| !self.selected_layer_footprint(room).is_empty())
    }

    pub(crate) fn extrude_selected_layer_above(&mut self, open_boundary: bool) {
        self.extrude_selected_layer(LayerDirection::Above, open_boundary);
    }

    pub(crate) fn extrude_selected_layer_below(&mut self, open_boundary: bool) {
        self.extrude_selected_layer(LayerDirection::Below, open_boundary);
    }

    fn extrude_selected_layer(&mut self, direction: LayerDirection, open_boundary: bool) {
        let Some(room) = self.floors_target_room() else {
            self.status = "Layer extrusion needs an active Room".to_string();
            return;
        };
        let footprint = self.selected_layer_footprint(room);
        if footprint.is_empty() {
            self.status = "Select one or more sectors or faces to extrude".to_string();
            return;
        }

        let before = self.project.clone();
        let source_floor = self.active_floor;
        let mut changed = false;
        let target_floor = {
            let scene = self.project.active_scene_mut();
            let Some(room_node) = scene.node_mut(room) else {
                return;
            };
            let NodeKind::Section { grid } = &mut room_node.kind else {
                return;
            };
            let source_floor = source_floor.min(grid.floor_count().saturating_sub(1));
            match direction {
                LayerDirection::Above => {
                    let target = source_floor.saturating_add(1);
                    if target >= grid.floor_count() {
                        grid.push_floor();
                        changed = true;
                    }
                    target
                }
                LayerDirection::Below if source_floor > 0 => source_floor - 1,
                LayerDirection::Below => {
                    grid.push_floor_below();
                    room_node.transform.translation[1] -= DEFAULT_WALL_HEIGHT_SECTORS as f32;
                    changed = true;
                    0
                }
            }
        };

        // Prepending a base floor changes every old floor index. Shift all
        // descendants so their inherited layer remains the same, while the
        // room translation above keeps their world-space height unchanged.
        if direction == LayerDirection::Below && source_floor == 0 {
            let scene = self.project.active_scene_mut();
            let ids: Vec<NodeId> = scene
                .nodes()
                .iter()
                .filter(|node| node.id != room && scene.is_descendant_of(node.id, room))
                .map(|node| node.id)
                .collect();
            for id in ids {
                if let Some(node) = scene.node_mut(id) {
                    node.floor = node.floor.saturating_add(1);
                }
            }
        }

        let default_floor_material = self.paint_material_for("floor");
        let default_wall_material = self.paint_material_for("brick").or(default_floor_material);
        let footprint_world: HashSet<(i32, i32)> =
            footprint.iter().map(|cell| cell.world).collect();
        let layer_height = {
            let scene = self.project.active_scene_mut();
            let Some(room_node) = scene.node_mut(room) else {
                return;
            };
            let NodeKind::Section { grid } = &mut room_node.kind else {
                return;
            };
            let Some(target) = grid.floor_mut(target_floor) else {
                return;
            };
            for cell in &footprint {
                target.extend_to_include(cell.world.0, cell.world.1);
            }
            let layer_height = target
                .sector_size
                .saturating_mul(DEFAULT_WALL_HEIGHT_SECTORS);
            for cell in &footprint {
                let Some((sx, sz)) = target.world_cell_to_array(cell.world.0, cell.world.1) else {
                    continue;
                };
                let floor_material = cell.floor_material.or(default_floor_material);
                let ceiling_material = cell
                    .ceiling_material
                    .or(cell.floor_material)
                    .or(default_floor_material);
                let wall_material = cell.wall_material.or(default_wall_material);
                let Some(sector) = target.ensure_sector(sx, sz) else {
                    continue;
                };
                if sector.floor.is_none() {
                    sector.floor = Some(GridHorizontalFace::flat(0, floor_material));
                    changed = true;
                }
                if sector.ceiling.is_none() {
                    sector.ceiling = Some(GridHorizontalFace::flat(layer_height, ceiling_material));
                    changed = true;
                }
                for direction in GridDirection::CARDINAL {
                    let is_perimeter = layer_neighbor(cell.world, direction)
                        .is_none_or(|neighbor| !footprint_world.contains(&neighbor));
                    if is_perimeter && sector.walls.get(direction).is_empty() {
                        sector.walls.get_mut(direction).push(GridVerticalFace::flat(
                            0,
                            layer_height,
                            wall_material,
                        ));
                        changed = true;
                    }
                }
            }
            layer_height
        };

        let (lower_floor, upper_floor) = match direction {
            LayerDirection::Above => (source_floor, target_floor),
            LayerDirection::Below if source_floor > 0 => (target_floor, source_floor),
            LayerDirection::Below => (target_floor, source_floor + 1),
        };
        if open_boundary {
            changed |= self.set_layer_boundary_open_no_undo(
                room,
                lower_floor,
                upper_floor,
                &footprint_world,
                true,
                default_floor_material,
                layer_height,
            ) > 0;
        }

        let selected_target_cells: HashSet<(NodeId, u16, u16)> = self
            .room_base_grid(room)
            .and_then(|base| base.floor(target_floor))
            .map(|target| {
                footprint
                    .iter()
                    .filter_map(|cell| {
                        target
                            .world_cell_to_array(cell.world.0, cell.world.1)
                            .map(|(sx, sz)| (room, sx, sz))
                    })
                    .collect()
            })
            .unwrap_or_default();

        if changed {
            self.history.record(before);
            self.active_floor = target_floor;
            self.selection.selected_sectors = selected_target_cells;
            self.selection.selected_sector = self
                .selection
                .selected_sectors
                .iter()
                .next()
                .map(|(_, sx, sz)| (*sx, *sz));
            self.selection.sector_selection_anchor =
                self.selection.selected_sectors.iter().next().copied();
            self.replace_node_selection(room);
            self.clear_resource_selection_state();
            self.clear_primitive_selection_state();
            self.mark_dirty();
            let side = if direction == LayerDirection::Above {
                "above"
            } else {
                "below"
            };
            let connection = if open_boundary { "open" } else { "solid" };
            self.status = format!(
                "Extruded {} sector{} {side} ({connection})",
                footprint.len(),
                if footprint.len() == 1 { "" } else { "s" }
            );
        } else {
            self.status = "The target layer already contains that solid footprint".to_string();
        }
    }

    pub(crate) fn set_selected_slab_above(&mut self, open: bool) {
        self.set_selected_slab(LayerDirection::Above, open);
    }

    pub(crate) fn set_selected_slab_below(&mut self, open: bool) {
        self.set_selected_slab(LayerDirection::Below, open);
    }

    fn set_selected_slab(&mut self, direction: LayerDirection, open: bool) {
        let Some(room) = self.floors_target_room() else {
            return;
        };
        let footprint = self.selected_layer_footprint(room);
        if footprint.is_empty() {
            self.status = "Select one or more sectors or faces first".to_string();
            return;
        }
        let Some(base) = self.room_base_grid(room) else {
            return;
        };
        let active = self.active_floor.min(base.floor_count().saturating_sub(1));
        let pair = match direction {
            LayerDirection::Above if active + 1 < base.floor_count() => (active, active + 1),
            LayerDirection::Below if active > 0 => (active - 1, active),
            LayerDirection::Above => {
                self.status = "There is no layer above this one".to_string();
                return;
            }
            LayerDirection::Below => {
                self.status = "There is no layer below this one".to_string();
                return;
            }
        };
        let cells: HashSet<(i32, i32)> = footprint.iter().map(|cell| cell.world).collect();
        let material = self.paint_material_for("floor");
        let layer_height = base
            .floor(pair.0)
            .map(|grid| grid.sector_size.saturating_mul(DEFAULT_WALL_HEIGHT_SECTORS))
            .unwrap_or(DEFAULT_WORLD_SECTOR_SIZE.saturating_mul(DEFAULT_WALL_HEIGHT_SECTORS));
        let before = self.project.clone();
        let changed = self.set_layer_boundary_open_no_undo(
            room,
            pair.0,
            pair.1,
            &cells,
            open,
            material,
            layer_height,
        );
        if changed == 0 {
            self.status = if open {
                "Selected boundary is already open".to_string()
            } else {
                "Selected boundary is already sealed".to_string()
            };
            return;
        }
        self.history.record(before);
        self.mark_dirty();
        self.status = format!(
            "{} {} vertical surface{}",
            if open { "Opened" } else { "Sealed" },
            changed,
            if changed == 1 { "" } else { "s" }
        );
    }

    fn set_layer_boundary_open_no_undo(
        &mut self,
        room: NodeId,
        lower_floor: usize,
        upper_floor: usize,
        cells: &HashSet<(i32, i32)>,
        open: bool,
        material: Option<ResourceId>,
        layer_height: i32,
    ) -> usize {
        let mut changed = 0usize;
        let scene = self.project.active_scene_mut();
        let Some(room_node) = scene.node_mut(room) else {
            return 0;
        };
        let NodeKind::Section { grid } = &mut room_node.kind else {
            return 0;
        };

        if let Some(lower) = grid.floor_mut(lower_floor) {
            for &(wcx, wcz) in cells {
                let Some((sx, sz)) = lower.world_cell_to_array(wcx, wcz) else {
                    continue;
                };
                let Some(sector) = lower.ensure_sector(sx, sz) else {
                    continue;
                };
                if open {
                    changed += usize::from(sector.ceiling.take().is_some());
                } else if sector.ceiling.is_none() {
                    sector.ceiling = Some(GridHorizontalFace::flat(layer_height, material));
                    changed += 1;
                }
            }
        }
        if let Some(upper) = grid.floor_mut(upper_floor) {
            for &(wcx, wcz) in cells {
                let Some((sx, sz)) = upper.world_cell_to_array(wcx, wcz) else {
                    continue;
                };
                let Some(sector) = upper.ensure_sector(sx, sz) else {
                    continue;
                };
                if open {
                    changed += usize::from(sector.floor.take().is_some());
                } else if sector.floor.is_none() {
                    sector.floor = Some(GridHorizontalFace::flat(0, material));
                    changed += 1;
                }
            }
        }
        changed
    }

    fn resolve_paint_blend_transition(
        &mut self,
        recipe: TransitionMaterialTexture,
        brush: ResourceId,
        context: ResourceId,
    ) -> (ResourceId, bool) {
        if let Some(existing) = self.project.resources.iter().find(|resource| {
            resource.name.starts_with(AUTO_PAINT_BLEND_PREFIX)
                && matches!(
                    &resource.data,
                    ResourceData::Material(material)
                        if material.texture_mode == MaterialTextureMode::Transition
                            && material.transition == recipe
                )
        }) {
            return (existing.id, false);
        }

        let name = format!(
            "{AUTO_PAINT_BLEND_PREFIX}{}:{}:{:02x}:{}:{}",
            brush.raw(),
            context.raw(),
            recipe.connected_edges,
            recipe.coverage,
            recipe.edge_breakup,
        );
        let mut material = MaterialResource::opaque(None);
        material.texture_mode = MaterialTextureMode::Transition;
        material.transition = recipe;
        let id = self
            .project
            .add_resource(name, ResourceData::Material(material));
        (id, true)
    }

    fn material_paint_neighbors(&self, face: FaceRef) -> Vec<(u8, FaceRef)> {
        let Some(grid) = self.room_grid_view(face.room) else {
            return Vec::new();
        };
        match face.kind {
            FaceKind::Floor | FaceKind::Ceiling => {
                let kind = face.kind;
                cardinal_paint_neighbors(face.sx, face.sz, grid.width, grid.depth)
                    .into_iter()
                    .filter_map(|(bit, sx, sz)| {
                        let exists = grid.sector(sx, sz).is_some_and(|sector| match kind {
                            FaceKind::Floor => sector.floor.is_some(),
                            FaceKind::Ceiling => sector.ceiling.is_some(),
                            FaceKind::Wall { .. } => false,
                        });
                        exists.then_some((
                            bit,
                            FaceRef {
                                room: face.room,
                                sx,
                                sz,
                                kind,
                            },
                        ))
                    })
                    .collect()
            }
            FaceKind::Wall { dir, stack } => {
                let key = PaintWallFaceKey {
                    sx: face.sx,
                    sz: face.sz,
                    dir,
                    stack: usize::from(stack),
                };
                paint_wall_neighbors(grid, key)
                    .into_iter()
                    .map(|(bit, neighbor)| {
                        (
                            bit,
                            FaceRef {
                                room: face.room,
                                sx: neighbor.sx,
                                sz: neighbor.sz,
                                kind: FaceKind::Wall {
                                    dir: neighbor.dir,
                                    stack: neighbor.stack as u8,
                                },
                            },
                        )
                    })
                    .collect()
            }
        }
    }

    fn face_uv_transform(&self, face: FaceRef) -> Option<GridUvTransform> {
        let grid = self.room_grid_view(face.room)?;
        let sector = grid.sector(face.sx, face.sz)?;
        match face.kind {
            FaceKind::Floor => sector.floor.as_ref().map(|face| face.uv),
            FaceKind::Ceiling => sector.ceiling.as_ref().map(|face| face.uv),
            FaceKind::Wall { dir, stack } => sector
                .walls
                .get(dir)
                .get(stack as usize)
                .map(|face| face.uv),
        }
    }

    /// Sample the material a designer considers painted on `face`. Generated
    /// connected blends are implementation resources, so unwrap those back to
    /// their source brush; ordinary and hand-authored transition materials are
    /// selected directly.
    pub(crate) fn sample_paint_material_from_face(&mut self, face: FaceRef) {
        let Some(applied) = self.face_material(face) else {
            self.status = format!("{} has no material to sample", describe_face(face));
            return;
        };
        let sampled = generated_paint_brush(&self.project, applied).unwrap_or(applied);
        if !matches!(
            self.project
                .resource(sampled)
                .map(|resource| &resource.data),
            Some(ResourceData::Material(_))
        ) {
            self.status = format!("{} does not resolve to a Material", describe_face(face));
            return;
        }
        let name = self
            .project
            .resource_name(sampled)
            .unwrap_or("(missing)")
            .to_string();
        self.replace_resource_selection(sampled);
        self.material_paint_sampling = false;
        self.status = format!("Sampled {name} from {}", describe_face(face));
        self.mark_shortcut_group_changed(ShortcutGroup::Tool);
    }

    /// Build the transition recipe for one painted face. Neighbours provide
    /// read-only context and connection bits; only the caller's face is
    /// assigned. Already-painted neighbours may later have their own recipes
    /// refreshed so the shared edge becomes continuous.
    fn material_paint_blend_recipe(
        &self,
        face: FaceRef,
        brush: ResourceId,
    ) -> Option<(TransitionMaterialTexture, ResourceId)> {
        let mut candidates: Vec<(ResourceId, u8)> = Vec::new();
        let mut connected_edges = 0u8;
        for (bit, neighbor) in self.material_paint_neighbors(face) {
            let Some(material) =
                paint_context_material(&self.project, self.face_material(neighbor))
            else {
                continue;
            };
            if material == brush {
                connected_edges |= bit;
            } else {
                push_material_candidate(&mut candidates, material, brush);
            }
        }

        if candidates.is_empty() {
            if let Some(current) = self.face_material(face) {
                if let Some(context) = generated_paint_context(&self.project, current, brush) {
                    push_material_candidate(&mut candidates, context, brush);
                } else if let Some(material) = paint_context_material(&self.project, Some(current))
                {
                    push_material_candidate(&mut candidates, material, brush);
                }
            }
        }
        candidates.sort_by(|(id_a, count_a), (id_b, count_b)| {
            count_b
                .cmp(count_a)
                .then_with(|| id_a.raw().cmp(&id_b.raw()))
        });
        let other = candidates.first()?.0;
        let connected_edges = self
            .face_uv_transform(face)
            .map(|uv| paint_edges_in_transformed_uv_space(connected_edges, uv))
            .unwrap_or(connected_edges);
        Some((
            paint_blend_recipe(
                brush,
                other,
                connected_edges,
                self.material_paint_blend_coverage_percent,
                self.material_paint_blend_edge_detail,
            ),
            other,
        ))
    }

    fn rebuild_paint_blend_face_no_undo(
        &mut self,
        face: FaceRef,
        brush: ResourceId,
    ) -> (bool, bool) {
        let Some((recipe, context)) = self.material_paint_blend_recipe(face, brush) else {
            return (false, false);
        };
        let (material, created) = self.resolve_paint_blend_transition(recipe, brush, context);
        (
            self.assign_face_material_no_undo(face, Some(material)),
            created,
        )
    }

    fn paint_horizontal_exact_no_undo(
        &mut self,
        room_id: NodeId,
        surface: HorizontalSurfaceKind,
        sx: u16,
        sz: u16,
        material: Option<ResourceId>,
    ) -> usize {
        let Some(grid) = self.room_floor_grid_mut(room_id) else {
            return 0;
        };
        match surface {
            HorizontalSurfaceKind::Floor => {
                grid.set_floor_aligned_to_neighbors(sx, sz, 0, material);
            }
            HorizontalSurfaceKind::Ceiling => {
                grid.set_ceiling_aligned_to_neighbors(sx, sz, material);
            }
        }
        1
    }

    /// Apply one paint / erase / place action to `(sx, sz)` in
    /// `room_id`. `picked_face` is set when a face was directly
    /// ray-picked (lets us remove a specific wall stack instead of
    /// the whole sector for Erase). `hit_world` is the world-space
    /// click position for tools that need the in-cell offset
    /// (PaintWall picks the edge from `(dx, dz)` against the cell
    /// centre).
    pub(crate) fn run_paint_action(
        &mut self,
        tool: ViewTool,
        room_id: NodeId,
        sx: u16,
        sz: u16,
        picked_face: Option<FaceRef>,
        hit_world: [f32; 3],
    ) {
        let floor_mat = self.paint_material_for("floor");
        let wall_mat = self.paint_material_for("brick").or(floor_mat);
        let sector_size_i = self.room_sector_size(room_id).unwrap_or(1024);
        let sector_size = sector_size_i as f32;
        let cell_center = self
            .room_grid_view(room_id)
            .map(|grid| grid.cell_center_world(sx, sz))
            .unwrap_or([
                (sx as f32 + 0.5) * sector_size,
                (sz as f32 + 0.5) * sector_size,
            ]);

        if matches!(tool, ViewTool::Place) && matches!(self.place_kind, PlaceKind::Portal) {
            self.place_portal_marker(room_id, sx, sz);
            return;
        }

        if matches!(tool, ViewTool::Place) {
            let translation = self.placement_translation_for_room_hit(room_id, hit_world);
            let kind = self.place_kind;
            if matches!(kind, PlaceKind::PlayerSpawn) && self.has_player_source() {
                self.status =
                    "Only one player source is allowed per world. Delete or demote the existing player first."
                        .to_string();
                return;
            }
            let (default_name, node_kind): (String, NodeKind) = match kind {
                PlaceKind::PlayerSpawn => (
                    "Player Spawn".to_string(),
                    NodeKind::SpawnPoint {
                        player: true,
                        character: None,
                    },
                ),
                PlaceKind::SpawnMarker => {
                    if let Some(existing) =
                        self.find_duplicate_spawn_marker(room_id, false, translation)
                    {
                        self.reject_duplicate_placement(existing, "Spawn");
                        return;
                    }
                    (
                        "Spawn".to_string(),
                        NodeKind::SpawnPoint {
                            player: false,
                            character: None,
                        },
                    )
                }
                PlaceKind::ModelInstance => {
                    // Resolve which Model resource to bind. Order:
                    // (a) user has a Model selected in the resource
                    //     panel -- use it; (b) exactly one Model
                    //     resource exists project-wide -- auto-pick;
                    //     (c) refuse with an actionable status.
                    match self.resolve_place_model_resource() {
                        Ok((model_id, name)) => {
                            if let Some(existing) =
                                self.find_duplicate_model_entity(room_id, model_id, translation)
                            {
                                self.reject_duplicate_placement(existing, "Prop");
                                return;
                            }
                            self.push_undo();
                            let id = self.create_model_entity_at_room_hit(
                                room_id, model_id, &name, hit_world,
                            );
                            self.replace_node_selection(id);
                            self.clear_resource_selection_state();
                            self.clear_primitive_selection_state();
                            self.status = format!("Placed Prop at {sx},{sz}");
                            self.mark_dirty();
                            self.return_to_select_after_place();
                            return;
                        }
                        Err(message) => {
                            self.status = message;
                            return;
                        }
                    }
                }
                PlaceKind::Character => match self.resolve_place_character_resource() {
                    Ok((character_id, name, character)) => {
                        if let Some(existing) =
                            self.find_duplicate_character_entity(room_id, character_id, translation)
                        {
                            self.reject_duplicate_placement(existing, "Character");
                            return;
                        }
                        let player = match character.spawn_role {
                            psxed_project::CharacterSpawnRole::Auto => !self.has_player_source(),
                            psxed_project::CharacterSpawnRole::Player => true,
                            psxed_project::CharacterSpawnRole::Enemy => false,
                        };
                        let idle_clip = self.resolve_character_idle_preview_clip(&character);
                        let settings = CharacterControllerSettings::from_character(&character);
                        let camera_settings = character.camera_settings();
                        self.push_undo();
                        if player
                            && character.spawn_role == psxed_project::CharacterSpawnRole::Player
                        {
                            self.demote_player_sources_except(None);
                        }
                        let id = self.create_character_entity_at_room_hit(
                            room_id,
                            character_id,
                            &name,
                            character.model,
                            character.material,
                            idle_clip,
                            settings,
                            player,
                            camera_settings,
                            hit_world,
                        );
                        self.replace_node_selection(id);
                        self.clear_resource_selection_state();
                        self.clear_primitive_selection_state();
                        self.status = if player {
                            format!("Placed Player Character at {sx},{sz}")
                        } else {
                            format!("Placed Character at {sx},{sz}")
                        };
                        self.mark_dirty();
                        self.return_to_select_after_place();
                        return;
                    }
                    Err(message) => {
                        self.status = message;
                        return;
                    }
                },
                PlaceKind::ImageProp => match self.resolve_place_image_prop_material() {
                    Ok((material_id, name)) => {
                        let size = image_prop_default_size_for_sector(sector_size_i);
                        if let Some(existing) =
                            self.find_duplicate_image_prop(room_id, material_id, translation)
                        {
                            self.reject_duplicate_placement(existing, "Image Prop");
                            return;
                        }
                        (
                            format!("{name} Image"),
                            NodeKind::ImageProp {
                                material: Some(material_id),
                                width: size,
                                height: size,
                                cylindrical_billboard: false,
                                collision_enabled: false,
                                collision_size: [size, size, size],
                            },
                        )
                    }
                    Err(message) => {
                        self.status = message;
                        return;
                    }
                },
                PlaceKind::BoxProp => {
                    let material = self.resolve_place_box_prop_material();
                    let material_id = material.as_ref().map(|(id, _)| *id);
                    let size = image_prop_default_size_for_sector(sector_size_i);
                    if let Some(existing) =
                        self.find_duplicate_box_prop(room_id, material_id, translation)
                    {
                        self.reject_duplicate_placement(existing, "Box Prop");
                        return;
                    }
                    let name = material
                        .as_ref()
                        .map(|(_, name)| format!("{name} Box"))
                        .unwrap_or_else(|| "Box Prop".to_string());
                    (
                        name,
                        NodeKind::BoxProp {
                            materials: [material_id; psxed_project::BOX_PROP_FACE_COUNT],
                            uvs: [GridUvTransform::IDENTITY; psxed_project::BOX_PROP_FACE_COUNT],
                            vertices: psxed_project::box_prop_vertices_for_size(size),
                            collision_enabled: true,
                            break_flags: 0,
                            erosion: psxed_project::BoxPropErosion::default(),
                        },
                    )
                }
                PlaceKind::CylinderProp => {
                    let material = self.resolve_place_box_prop_material();
                    let material_id = material.as_ref().map(|(id, _)| *id);
                    let size = image_prop_default_size_for_sector(sector_size_i);
                    if let Some(existing) =
                        self.find_duplicate_cylinder_prop(room_id, material_id, translation)
                    {
                        self.reject_duplicate_placement(existing, "Cylinder Prop");
                        return;
                    }
                    let name = material
                        .as_ref()
                        .map(|(_, name)| format!("{name} Column"))
                        .unwrap_or_else(|| "Cylinder Prop".to_string());
                    let geometry = psxed_project::CylinderPropGeometry {
                        radius: [size / 2, size / 2],
                        height: size.saturating_mul(2),
                        ..Default::default()
                    };
                    (
                        name,
                        NodeKind::CylinderProp {
                            materials: [material_id; psxed_project::CYLINDER_PROP_MATERIAL_COUNT],
                            uvs: [GridUvTransform::IDENTITY;
                                psxed_project::CYLINDER_PROP_MATERIAL_COUNT],
                            geometry,
                            collision_enabled: true,
                        },
                    )
                }
                PlaceKind::ArchProp => {
                    let material = self.resolve_place_box_prop_material();
                    let material_id = material.as_ref().map(|(id, _)| *id);
                    let name = material
                        .as_ref()
                        .map(|(_, name)| format!("{name} Arch"))
                        .unwrap_or_else(|| "Arch Prop".to_string());
                    (
                        name,
                        NodeKind::ArchProp {
                            materials: [material_id; psxed_project::ARCH_PROP_MATERIAL_COUNT],
                            uvs: [GridUvTransform::IDENTITY;
                                psxed_project::ARCH_PROP_MATERIAL_COUNT],
                            geometry: psxed_project::ArchPropGeometry::default(),
                            collision_enabled: false,
                        },
                    )
                }
                PlaceKind::PointLightMarker => {
                    if let Some(existing) = self.find_duplicate_point_light(room_id, translation) {
                        self.reject_duplicate_placement(existing, "Point Light");
                        return;
                    }
                    (
                        "Point Light".to_string(),
                        NodeKind::PointLight {
                            color: [255, 240, 200],
                            intensity: 1.0,
                            // Sectors. Matches the Add Child default
                            // and covers a typical 4×4 sector room.
                            radius: 4.0,
                        },
                    )
                }
                PlaceKind::ParticleEmitter => {
                    if let Some(existing) =
                        self.find_duplicate_particle_emitter(room_id, translation)
                    {
                        self.reject_duplicate_placement(existing, "Particle Emitter");
                        return;
                    }
                    (
                        "Particle Emitter".to_string(),
                        NodeKind::ParticleEmitter {
                            settings: ParticleEmitterSettings::default(),
                        },
                    )
                }
                PlaceKind::Logic => (
                    // Default to a trigger volume; the inspector's
                    // kind picker switches it to relay/multisource/
                    // door. Rename the node to give the record its
                    // targetname.
                    "Trigger Volume".to_string(),
                    NodeKind::Logic {
                        kind: psxed_project::LogicNodeKind::default(),
                        target: String::new(),
                        killtarget: String::new(),
                        master: String::new(),
                        delay_ticks: 0,
                        // Fire once, then retire (hl's `wait -1`). A wait-0
                        // volume re-activates on EVERY tick the player stands
                        // inside it, which soft-locks any overlay it opens.
                        // Authored projects keep whatever they already store.
                        wait_ticks: -1,
                        enabled: true,
                    },
                ),
                PlaceKind::Portal => return,
            };
            self.push_undo();
            let active_floor = self.active_floor;
            let id = self
                .project
                .active_scene_mut()
                .add_node(room_id, default_name, node_kind);
            if let Some(node) = self.project.active_scene_mut().node_mut(id) {
                node.transform.translation = translation;
                let arch_geometry = match &node.kind {
                    NodeKind::ArchProp { geometry, .. } => Some(*geometry),
                    _ => None,
                };
                if let Some(geometry) = arch_geometry {
                    crate::inspector_transform_node::snap_arch_prop_transform(
                        &mut node.transform,
                        geometry,
                        sector_size_i,
                    );
                }
                // Record the floor this was placed on (0 = ground). The
                // cook binds the node to this floor's runtime room; Y is
                // a placement default and can't select the floor.
                node.floor = active_floor;
            }
            self.replace_node_selection(id);
            self.clear_resource_selection_state();
            self.clear_primitive_selection_state();
            self.status = format!("Placed {} at {sx},{sz}", kind.label());
            self.mark_dirty();
            self.return_to_select_after_place();
            return;
        }

        if tool == ViewTool::Water {
            self.paint_water_cell(room_id, sx, sz);
            return;
        }

        if tool == ViewTool::PaintMaterial {
            let Some(face) = picked_face else {
                self.status =
                    "Material Paint needs an existing surface under the cursor".to_string();
                return;
            };
            let Some(brush) = floor_mat else {
                self.status = "Select a Material before painting".to_string();
                return;
            };
            self.push_undo();
            let mut changed = self.assign_face_material_no_undo(face, Some(brush));
            let mut created = false;
            let mut blended = false;
            if self.material_paint_blend {
                // Only the clicked face receives the new logical material.
                // Refresh adjacent generated Paint faces so their connection
                // masks open along the new shared edge; unpainted neighbours
                // are never assigned or otherwise mutated.
                let mut refresh = vec![(face, brush)];
                for (_, neighbor) in self.material_paint_neighbors(face) {
                    let Some(material) = self.face_material(neighbor) else {
                        continue;
                    };
                    let Some(neighbor_brush) = generated_paint_brush(&self.project, material)
                    else {
                        continue;
                    };
                    refresh.push((neighbor, neighbor_brush));
                }
                for (painted_face, painted_brush) in refresh {
                    let (face_changed, resource_created) =
                        self.rebuild_paint_blend_face_no_undo(painted_face, painted_brush);
                    changed |= face_changed;
                    created |= resource_created;
                    blended |= painted_face == face && (face_changed || resource_created);
                }
            }
            if changed || created {
                self.mark_dirty();
            }
            self.status = if blended {
                format!("Blended material onto {} only", describe_face(face))
            } else {
                format!("Painted material onto {} only", describe_face(face))
            };
            return;
        }

        if let Some(triangle) =
            self.horizontal_paint_triangle_target(tool, room_id, sx, sz, hit_world)
        {
            let already_assigned = self.triangle_material(triangle) == floor_mat;
            self.select_painted_triangle(triangle);
            if already_assigned {
                self.status = format!(
                    "{} already uses selected material",
                    describe_triangle(triangle)
                );
                return;
            }
            self.push_undo();
            if self.assign_triangle_material_no_undo(triangle, floor_mat) {
                self.mark_dirty();
            }
            self.select_painted_triangle(triangle);
            self.status = format!("Painted {}", describe_triangle(triangle));
            return;
        }

        if matches!(tool, ViewTool::PaintFloor | ViewTool::PaintCeiling) {
            let surface = if tool == ViewTool::PaintFloor {
                HorizontalSurfaceKind::Floor
            } else {
                HorizontalSurfaceKind::Ceiling
            };
            self.push_undo();
            let changed = self.paint_horizontal_exact_no_undo(room_id, surface, sx, sz, floor_mat);
            if changed > 0 {
                self.mark_dirty();
            }
            let surface_name = if surface == HorizontalSurfaceKind::Floor {
                "floor"
            } else {
                "ceiling"
            };
            self.status = format!("Created {surface_name} at {sx},{sz}");
            return;
        }

        if tool == ViewTool::PaintWall {
            let dir = if let Some(FaceRef {
                kind: FaceKind::Wall { dir, .. },
                ..
            }) = picked_face
            {
                dir
            } else {
                self.wall_paint_shape
                    .direction(hit_world[0] - cell_center[0], hit_world[2] - cell_center[1])
            };
            self.push_undo();
            let Some(grid) = self.room_floor_grid_mut(room_id) else {
                return;
            };
            let stack = grid
                .sector(sx, sz)
                .map(|sector| sector.walls.get(dir).len())
                .unwrap_or(0);
            grid.add_wall_above_stack_or_aligned(sx, sz, dir, wall_mat);
            if wall_mat.is_some() {
                let sector_size = grid.sector_size;
                if let Some(wall) = grid
                    .sector_mut(sx, sz)
                    .and_then(|sector| sector.walls.get_mut(dir).last_mut())
                {
                    wall.autotile_uv(sector_size);
                }
            }
            self.mark_dirty();
            self.status = if stack == 0 {
                format!("Added {} wall at {sx},{sz}", direction_label(dir))
            } else {
                format!("Added {} wall #{stack} at {sx},{sz}", direction_label(dir))
            };
            return;
        }

        // Snapshot for undo BEFORE mutating. Each non-Place tool
        // shares the same snapshot point.
        self.push_undo();
        let active_floor = self.active_floor;
        let scene = self.project.active_scene_mut();
        let Some(room) = scene.node_mut(room_id) else {
            return;
        };
        let NodeKind::Section { grid } = &mut room.kind else {
            return;
        };
        // Paint into the active floor's grid (floor 0 = base). Clamp so a
        // stale index can't panic; the index is always in range here.
        let floor_idx = active_floor.min(grid.floor_count().saturating_sub(1));
        let grid = grid
            .floor_mut(floor_idx)
            .expect("floor index clamped to range");
        let status = match tool {
            ViewTool::Erase => {
                // Per-face Erase: a wall ray-pick drops just that
                // wall stack entry; floor/ceiling/no-pick clears
                // the whole sector (mirrors the 2D paint pass).
                match picked_face {
                    Some(FaceRef {
                        kind: FaceKind::Wall { dir, stack },
                        ..
                    }) => {
                        if let Some(sector) = grid.sector_mut(sx, sz) {
                            let walls = sector.walls.get_mut(dir);
                            if (stack as usize) < walls.len() {
                                walls.remove(stack as usize);
                            }
                        }
                        format!("Removed wall at {sx},{sz}")
                    }
                    _ => {
                        if let Some(index) = grid.sector_index(sx, sz) {
                            grid.sectors[index] = None;
                        }
                        format!("Erased sector {sx},{sz}")
                    }
                }
            }
            _ => return,
        };
        self.dirty = true;
        self.status = status;
    }

    fn paint_water_cell(&mut self, room_id: NodeId, sx: u16, sz: u16) {
        let active_floor = self.active_floor;
        let Some((world_cell, default_height)) = self.room_grid_view(room_id).and_then(|grid| {
            let sector = grid.sector(sx, sz)?;
            let floor = sector.floor.as_ref()?;
            let clicked_floor = floor.lowest_height();
            let world_x = grid.origin[0] + i32::from(sx);
            let world_z = grid.origin[1] + i32::from(sz);
            let mut rim = clicked_floor;
            for dz in -1..=1 {
                for dx in -1..=1 {
                    if dx == 0 && dz == 0 {
                        continue;
                    }
                    let Some((nx, nz)) = grid.world_cell_to_array(world_x + dx, world_z + dz)
                    else {
                        continue;
                    };
                    if let Some(neighbor_floor) =
                        grid.sector(nx, nz).and_then(|sector| sector.floor.as_ref())
                    {
                        rim = rim.max(neighbor_floor.lowest_height());
                    }
                }
            }
            let height = if rim > clicked_floor {
                u16::try_from(rim.saturating_sub(clicked_floor)).unwrap_or(u16::MAX)
            } else {
                64
            };
            Some((WaterVolumeCell::new(world_x, world_z), height.max(1)))
        }) else {
            self.status = "Water needs an existing floor under the cursor".to_string();
            return;
        };

        let scene = self.project.active_scene();
        let selected_volume = (self.selection.selected_node != NodeId::ROOT)
            .then_some(self.selection.selected_node)
            .filter(|id| scene.is_descendant_of(*id, room_id))
            .filter(|id| {
                scene.node(*id).is_some_and(|node| {
                    node.floor == active_floor && matches!(node.kind, NodeKind::WaterVolume { .. })
                })
            });

        if self.water_tool_mode == WaterToolMode::Select {
            let selected = self
                .project
                .active_scene()
                .nodes()
                .iter()
                .find(|node| {
                    node.floor == active_floor
                        && !self.scene_node_effectively_hidden(node.id)
                        && self
                            .project
                            .active_scene()
                            .is_descendant_of(node.id, room_id)
                        && matches!(
                            &node.kind,
                            NodeKind::WaterVolume { cells, .. }
                                if cells.contains(&world_cell)
                        )
                })
                .map(|node| (node.id, node.name.clone()));
            if let Some((id, name)) = selected {
                self.replace_node_selection(id);
                // Water painting commonly starts from a material selected in the
                // Resources dock. Once the author explicitly selects a volume,
                // the node must become the Inspector target instead of leaving
                // that stale resource selection in front of it.
                self.clear_resource_selection_state();
                self.clear_primitive_selection_state();
                self.clear_sector_selection();
                self.status = format!("Selected {name} at {},{}", world_cell.x, world_cell.z);
            } else {
                self.clear_node_selection_state();
                self.clear_resource_selection_state();
                self.clear_primitive_selection_state();
                self.clear_sector_selection();
                self.status = format!("No water at {},{}", world_cell.x, world_cell.z);
            }
            return;
        }

        self.push_undo();
        if self.water_tool_mode == WaterToolMode::Erase {
            let mut changed = false;
            let ids: Vec<NodeId> = self
                .project
                .active_scene()
                .nodes()
                .iter()
                .filter(|node| node.floor == active_floor)
                .filter(|node| {
                    self.project
                        .active_scene()
                        .is_descendant_of(node.id, room_id)
                })
                .filter(|node| matches!(node.kind, NodeKind::WaterVolume { .. }))
                .map(|node| node.id)
                .collect();
            let scene = self.project.active_scene_mut();
            for id in ids {
                if let Some(NodeKind::WaterVolume { cells, .. }) =
                    scene.node_mut(id).map(|node| &mut node.kind)
                {
                    let old_len = cells.len();
                    cells.retain(|cell| *cell != world_cell);
                    changed |= cells.len() != old_len;
                }
            }
            if changed {
                self.mark_dirty();
                self.status = format!("Removed water at {},{}", world_cell.x, world_cell.z);
            } else {
                self.status = "No water in this cell".to_string();
            }
            return;
        }

        let material = self.selected_material_resource().or(self.brush_material);
        let volume_id = if let Some(id) = selected_volume {
            id
        } else {
            let settings = WaterVolumeSettings {
                height_above_floor: default_height,
                ..Default::default()
            };
            let id = self.project.active_scene_mut().add_node(
                room_id,
                "Water Volume",
                NodeKind::WaterVolume {
                    material,
                    cells: Vec::new(),
                    settings,
                },
            );
            if let Some(node) = self.project.active_scene_mut().node_mut(id) {
                node.floor = active_floor;
            }
            id
        };

        // A cell belongs to one water volume on a floor. Painting it into a
        // selected volume transfers ownership so cook-time behavior is never
        // ambiguous.
        let ids: Vec<NodeId> = self
            .project
            .active_scene()
            .nodes()
            .iter()
            .filter(|node| node.id != volume_id && node.floor == active_floor)
            .filter(|node| {
                self.project
                    .active_scene()
                    .is_descendant_of(node.id, room_id)
            })
            .filter(|node| matches!(node.kind, NodeKind::WaterVolume { .. }))
            .map(|node| node.id)
            .collect();
        let scene = self.project.active_scene_mut();
        for id in ids {
            if let Some(NodeKind::WaterVolume { cells, .. }) =
                scene.node_mut(id).map(|node| &mut node.kind)
            {
                cells.retain(|cell| *cell != world_cell);
            }
        }
        if let Some(NodeKind::WaterVolume {
            material: volume_material,
            cells,
            ..
        }) = scene.node_mut(volume_id).map(|node| &mut node.kind)
        {
            if volume_material.is_none() {
                *volume_material = material;
            }
            if !cells.contains(&world_cell) {
                cells.push(world_cell);
                cells.sort_by_key(|cell| (cell.x, cell.z));
            }
        }
        self.replace_node_selection(volume_id);
        // Painting selects the owning WaterVolume so its material and gameplay
        // settings are immediately editable in the Inspector. The brush keeps
        // its material independently, so clearing the Resources selection does
        // not interrupt subsequent paint strokes.
        self.clear_resource_selection_state();
        self.clear_primitive_selection_state();
        self.clear_sector_selection();
        self.mark_dirty();
        self.status = format!("Painted water at {},{}", world_cell.x, world_cell.z);
    }

    pub(crate) fn horizontal_paint_triangle_target(
        &self,
        tool: ViewTool,
        room_id: NodeId,
        sx: u16,
        sz: u16,
        hit_world: [f32; 3],
    ) -> Option<HorizontalTriangleRef> {
        if !matches!(self.horizontal_edit_mode, HorizontalEditMode::Triangle) {
            return None;
        }
        let kind = match tool {
            ViewTool::PaintFloor => FaceKind::Floor,
            ViewTool::PaintCeiling => FaceKind::Ceiling,
            _ => return None,
        };
        self.horizontal_triangle_ref_at_hit(
            FaceRef {
                room: room_id,
                sx,
                sz,
                kind,
            },
            hit_world,
        )
    }

    pub(crate) fn select_painted_triangle(&mut self, triangle: HorizontalTriangleRef) {
        self.replace_node_selection(triangle.room);
        self.clear_sector_selection();
        self.replace_primitive_selection(Selection::Triangle(triangle));
        self.update_primitive_resource_selection();
    }

    pub(crate) fn place_portal_marker(&mut self, room_id: NodeId, sx: u16, sz: u16) {
        let dir = self.portal_place_direction;
        let Some((editor, entry_name)) = self.room_grid_view(room_id).and_then(|grid| {
            if !portal_edge_valid_for_array_cell(grid, sx, sz, dir) {
                return None;
            }
            let editor = portal_edge_midpoint_editor(grid, sx, sz, dir);
            let entry_name = format!(
                "portal_{}_{}_{}",
                sx,
                sz,
                direction_label(dir).to_ascii_lowercase()
            );
            Some((editor, entry_name))
        }) else {
            self.status = format!(
                "Portal needs populated sectors on both sides of the {} edge",
                direction_label(dir)
            );
            return;
        };

        let translation = [editor[0], 0.0, editor[1]];
        if let Some(existing) = self.find_duplicate_portal(room_id, translation) {
            self.reject_duplicate_placement(existing, "Portal");
            self.status = format!(
                "Portal already exists on {} edge at {sx},{sz}",
                direction_label(dir)
            );
            return;
        }

        self.push_undo();
        let id = self.project.active_scene_mut().add_node(
            room_id,
            format!("Portal {}", direction_label(dir)),
            NodeKind::Portal {
                target_room: None,
                target_entry: String::new(),
                entry_name,
                geometry: None,
            },
        );
        if let Some(node) = self.project.active_scene_mut().node_mut(id) {
            node.transform.translation = translation;
        }
        self.replace_node_selection(id);
        self.clear_resource_selection_state();
        self.clear_primitive_selection_state();
        self.status = format!(
            "Placed Portal on {} edge at {sx},{sz}",
            direction_label(dir)
        );
        self.mark_dirty();
        self.return_to_select_after_place();
    }

    /// Material id currently applied to `face`, or `None` if the
    /// face is unassigned / its referent went away.
    pub(crate) fn face_material(&self, face: FaceRef) -> Option<ResourceId> {
        let grid = self.room_grid_view(face.room)?;
        let sector = grid.sector(face.sx, face.sz)?;
        match face.kind {
            FaceKind::Floor => sector.floor.as_ref().and_then(|f| f.material),
            FaceKind::Ceiling => sector.ceiling.as_ref().and_then(|c| c.material),
            FaceKind::Wall { dir, stack } => sector
                .walls
                .get(dir)
                .get(stack as usize)
                .and_then(|w| w.material),
        }
    }

    pub(crate) fn triangle_material(&self, triangle: HorizontalTriangleRef) -> Option<ResourceId> {
        let face = triangle.parent_face();
        let grid = self.room_grid_view(face.room)?;
        let sector = grid.sector(face.sx, face.sz)?;
        let index = triangle.index.idx();
        match triangle.surface {
            HorizontalSurfaceKind::Floor => sector.floor.as_ref()?.triangle_material(index),
            HorizontalSurfaceKind::Ceiling => sector.ceiling.as_ref()?.triangle_material(index),
        }
    }

    pub(crate) fn material_target_value(&self, target: MaterialTarget) -> Option<ResourceId> {
        match target {
            MaterialTarget::Face(face) => self.face_material(face),
            MaterialTarget::Triangle(triangle) => self.triangle_material(triangle),
        }
    }

    /// Reassign `face`'s material in-place. Marks the project
    /// dirty if the field actually moved. Used by drag/drop flows
    /// and by the resource-card click path for single-face edits.
    pub(crate) fn assign_face_material(
        &mut self,
        face: FaceRef,
        material: Option<ResourceId>,
    ) -> bool {
        if self.face_material(face) == material {
            return false;
        }
        self.push_undo();
        let updated = self.assign_face_material_no_undo(face, material);
        if updated {
            self.mark_dirty();
        }
        updated
    }

    /// Reassign every selected face in one undo step. Edges and
    /// vertices are intentionally ignored here: materials bind to
    /// actual face surfaces, while those modes edit topology/height.
    pub(crate) fn assign_selected_faces_material(&mut self, material: Option<ResourceId>) -> usize {
        let targets = self.selected_material_targets();
        if targets.is_empty() {
            return 0;
        }
        let needs_update = targets
            .iter()
            .any(|target| self.material_target_value(*target) != material);
        if !needs_update {
            return 0;
        }
        self.push_undo();
        let mut updated = 0usize;
        for target in targets {
            if self.assign_material_target_no_undo(target, material) {
                updated += 1;
            }
        }
        if updated > 0 {
            self.mark_dirty();
        }
        updated
    }

    pub(crate) fn apply_selected_box_prop_resource_click(&mut self, click: ResourceClick) -> bool {
        let plain_click =
            !click.modifiers.shift && !click.modifiers.ctrl && !click.modifiers.command;
        if !plain_click {
            return false;
        }
        let Some(assignment) = self.assign_selected_box_props_resource(click.id) else {
            return false;
        };
        self.status = match (assignment.targets, assignment.updated) {
            (1, 0) => "Material already assigned to selected Prop".to_string(),
            (1, _) => "Assigned material to Prop".to_string(),
            (_, 0) => "Material already assigned to selected Props".to_string(),
            (total, updated) if total == updated => {
                format!("Assigned material to {updated} selected Props")
            }
            (total, updated) => {
                format!("Assigned material to {updated}/{total} selected Props")
            }
        };
        self.clear_resource_selection_state();
        true
    }

    pub(crate) fn assign_selected_box_props_resource(
        &mut self,
        resource_id: ResourceId,
    ) -> Option<BoxPropMaterialAssignment> {
        let targets = self.selected_box_prop_nodes();
        let target_count = targets.len();
        if target_count == 0 {
            return None;
        }
        let material = match self.project.resource(resource_id).map(|r| &r.data) {
            Some(ResourceData::Material(_)) => resource_id,
            _ => return None,
        };
        let needs_update = targets
            .iter()
            .any(|id| self.box_prop_materials_differ(*id, material));
        if !needs_update {
            return Some(BoxPropMaterialAssignment {
                material,
                targets: target_count,
                updated: 0,
            });
        }

        self.push_undo();
        let updated = self.assign_box_prop_nodes_material_no_undo(&targets, material);
        if updated > 0 {
            self.mark_dirty();
        }
        Some(BoxPropMaterialAssignment {
            material,
            targets: target_count,
            updated,
        })
    }

    pub(crate) fn selected_box_prop_nodes(&self) -> Vec<NodeId> {
        self.selected_node_ids_in_hierarchy()
            .into_iter()
            .filter(|id| {
                self.project.active_scene().node(*id).is_some_and(|node| {
                    matches!(
                        node.kind,
                        NodeKind::BoxProp { .. } | NodeKind::CylinderProp { .. }
                    )
                })
            })
            .collect()
    }

    pub(crate) fn box_prop_materials_differ(&self, id: NodeId, material: ResourceId) -> bool {
        self.project
            .active_scene()
            .node(id)
            .is_some_and(|node| match &node.kind {
                NodeKind::BoxProp { materials, .. } => {
                    materials.iter().any(|slot| *slot != Some(material))
                }
                NodeKind::CylinderProp { materials, .. } => {
                    materials.iter().any(|slot| *slot != Some(material))
                }
                _ => false,
            })
    }

    pub(crate) fn assign_box_prop_nodes_material_no_undo(
        &mut self,
        targets: &[NodeId],
        material: ResourceId,
    ) -> usize {
        let scene = self.project.active_scene_mut();
        let mut updated = 0usize;
        for id in targets {
            let Some(node) = scene.node_mut(*id) else {
                continue;
            };
            match &mut node.kind {
                NodeKind::BoxProp { materials, .. } => {
                    if materials.iter().all(|slot| *slot == Some(material)) {
                        continue;
                    }
                    *materials = [Some(material); psxed_project::BOX_PROP_FACE_COUNT];
                }
                NodeKind::CylinderProp { materials, .. } => {
                    if materials.iter().all(|slot| *slot == Some(material)) {
                        continue;
                    }
                    *materials = [Some(material); psxed_project::CYLINDER_PROP_MATERIAL_COUNT];
                }
                _ => continue,
            }
            updated += 1;
        }
        updated
    }

    pub(crate) fn selected_material_targets(&self) -> Vec<MaterialTarget> {
        let mut targets = Vec::new();
        for face in self.selected_sector_faces() {
            push_unique_material_target(&mut targets, MaterialTarget::Face(face));
        }
        for selection in self.selected_primitive_targets() {
            match selection {
                Selection::Face(face) => {
                    push_unique_material_target(&mut targets, MaterialTarget::Face(face));
                }
                Selection::Triangle(triangle) => {
                    push_unique_material_target(&mut targets, MaterialTarget::Triangle(triangle));
                }
                Selection::Edge(_) | Selection::Vertex(_) => {}
            }
        }
        targets
    }

    /// Apply only the UV fields changed in the active face inspector to every
    /// selected face. Keeping this field-wise is important: rotating a batch
    /// must not also replace offsets, spans, or flips that differ per face.
    ///
    /// The inspector transaction owns undo/dirty bookkeeping; this helper is
    /// deliberately mutation-only so the whole batch remains one undo step.
    pub(crate) fn apply_selected_face_uv_change_no_undo(
        &mut self,
        active: FaceRef,
        edit: GridUvTransformEdit,
        authored: GridUvTransform,
    ) -> (usize, usize) {
        if !edit.changed() {
            return (0, 0);
        }

        let mut targets = self.selected_sector_faces();
        for selection in self.selected_primitive_targets() {
            let Selection::Face(face) = selection else {
                continue;
            };
            if !targets.contains(&face) {
                targets.push(face);
            }
        }
        if !targets.contains(&active) {
            targets.push(active);
        }

        let mut affected = 0;
        let mut updated = 0;
        for face in targets {
            let Some(grid) = self.room_floor_grid_mut(face.room) else {
                continue;
            };
            let Some(sector) = grid.sector_mut(face.sx, face.sz) else {
                continue;
            };
            let uv = match face.kind {
                FaceKind::Floor => sector.floor.as_mut().map(|face| &mut face.uv),
                FaceKind::Ceiling => sector.ceiling.as_mut().map(|face| &mut face.uv),
                FaceKind::Wall { dir, stack } => sector
                    .walls
                    .get_mut(dir)
                    .get_mut(stack as usize)
                    .map(|face| &mut face.uv),
            };
            let Some(uv) = uv else {
                continue;
            };
            let before = *uv;
            edit.apply(uv, authored);
            affected += 1;
            updated += usize::from(*uv != before);
        }
        (affected, updated)
    }

    #[cfg(test)]
    pub(crate) fn selected_face_targets(&self) -> Vec<FaceRef> {
        let mut faces = Vec::new();
        for face in self.selected_sector_faces() {
            if !faces.contains(&face) {
                faces.push(face);
            }
        }
        for selection in self.selected_primitive_targets() {
            let face = match selection {
                Selection::Face(face) => face,
                Selection::Triangle(triangle) => triangle.parent_face(),
                Selection::Edge(_) | Selection::Vertex(_) => continue,
            };
            if !faces.contains(&face) {
                faces.push(face);
            }
        }
        faces
    }

    pub(crate) fn selected_sector_wall_faces(&self) -> Vec<FaceRef> {
        self.selected_sector_faces()
            .into_iter()
            .filter(|face| matches!(face.kind, FaceKind::Wall { .. }))
            .collect()
    }

    pub(crate) fn autotile_selected_sector_walls(&mut self) -> usize {
        let selected_tiles = self.selection.selected_sectors.len();
        if selected_tiles == 0 {
            self.status = "No selected tiles to autotile".to_string();
            return 0;
        }

        let targets = self.selected_sector_wall_faces();
        if targets.is_empty() {
            self.status = "No selected tiles have walls to autotile".to_string();
            return 0;
        }

        let mut visited = 0usize;
        let mut updated = 0usize;
        let mut clamped = 0usize;
        for face in targets {
            let Some((changed, was_clamped)) = self.autotile_wall_face_no_undo(face) else {
                continue;
            };
            visited += 1;
            if changed {
                updated += 1;
            }
            if was_clamped {
                clamped += 1;
            }
        }

        if updated > 0 {
            self.mark_dirty();
        }
        self.status = autotile_selection_status(selected_tiles, visited, updated, clamped);
        updated
    }

    pub(crate) fn autotile_wall_face_no_undo(&mut self, face: FaceRef) -> Option<(bool, bool)> {
        let FaceKind::Wall { dir, stack } = face.kind else {
            return None;
        };
        let grid = self.room_floor_grid_mut(face.room)?;
        let sector_size = grid.sector_size;
        let sector = grid.sector_mut(face.sx, face.sz)?;
        let wall = sector.walls.get_mut(dir).get_mut(stack as usize)?;
        let before = wall.uv;
        let clamped = wall.autotile_uv(sector_size);
        Some((wall.uv != before, clamped))
    }

    pub(crate) fn assign_face_material_no_undo(
        &mut self,
        face: FaceRef,
        material: Option<ResourceId>,
    ) -> bool {
        if self.face_material(face) == material {
            return false;
        }
        let Some(grid) = self.room_floor_grid_mut(face.room) else {
            return false;
        };
        let sector_size = grid.sector_size;
        let Some(sector) = grid.sector_mut(face.sx, face.sz) else {
            return false;
        };
        match face.kind {
            FaceKind::Floor => sector
                .floor
                .as_mut()
                .map(|f| {
                    f.material = material;
                })
                .is_some(),
            FaceKind::Ceiling => sector
                .ceiling
                .as_mut()
                .map(|c| {
                    c.material = material;
                })
                .is_some(),
            FaceKind::Wall { dir, stack } => sector
                .walls
                .get_mut(dir)
                .get_mut(stack as usize)
                .map(|w| {
                    w.material = material;
                    // Applying a wall texture gets the same sensible UV
                    // density as the Inspector's Autotile action. Rotation,
                    // flips, and offset remain authored; only the span is
                    // normalized to the wall's world height.
                    if material.is_some() {
                        w.autotile_uv(sector_size);
                    }
                })
                .is_some(),
        }
    }

    pub(crate) fn assign_material_target_no_undo(
        &mut self,
        target: MaterialTarget,
        material: Option<ResourceId>,
    ) -> bool {
        match target {
            MaterialTarget::Face(face) => self.assign_face_material_no_undo(face, material),
            MaterialTarget::Triangle(triangle) => {
                self.assign_triangle_material_no_undo(triangle, material)
            }
        }
    }

    pub(crate) fn assign_triangle_material_no_undo(
        &mut self,
        triangle: HorizontalTriangleRef,
        material: Option<ResourceId>,
    ) -> bool {
        if self.triangle_material(triangle) == material {
            return false;
        }
        let Some(grid) = self.room_floor_grid_mut(triangle.room) else {
            return false;
        };
        let Some(sector) = grid.sector_mut(triangle.sx, triangle.sz) else {
            return false;
        };
        let face = match triangle.surface {
            HorizontalSurfaceKind::Floor => sector.floor.as_mut(),
            HorizontalSurfaceKind::Ceiling => sector.ceiling.as_mut(),
        };
        let Some(face) = face else {
            return false;
        };
        let parent_material = face.material;
        let override_material = if material == parent_material {
            None
        } else {
            Some(GridTriangleMaterialOverride::from_material(material))
        };
        let target = face.triangle_override_mut(triangle.index.idx());
        if target.material == override_material {
            return false;
        }
        target.material = override_material;
        true
    }
}

fn point_in_convex_xz(point: [f64; 2], polygon: &[[f64; 2]]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let mut winding = 0i8;
    let mut has_area = false;
    for index in 0..polygon.len() {
        let a = polygon[index];
        let b = polygon[(index + 1) % polygon.len()];
        let cross = (b[0] - a[0]) * (point[1] - a[1]) - (b[1] - a[1]) * (point[0] - a[0]);
        if cross.abs() <= 1.0e-7 {
            continue;
        }
        has_area = true;
        let sign = if cross > 0.0 { 1 } else { -1 };
        if winding != 0 && winding != sign {
            return false;
        }
        winding = sign;
    }
    has_area
}
