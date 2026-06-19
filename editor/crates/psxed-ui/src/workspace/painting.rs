use super::*;

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
                | ViewTool::Erase
                | ViewTool::Place
        );
        let portal_tool = self.portal_place_active();
        let face_hit = self.face_hit_for_paint_tool(face_hit);
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
                let cell = if paint_tool && !portal_tool {
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
        } else {
            self.selection.selected_sector = Some((sx, sz));
        }
        let tool = self.active_tool;
        self.run_paint_action(tool, room_id, sx, sz, face_hit.map(|(f, _)| f), hit_world)
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
                let player = !self.has_player_source();
                let idle_clip = self.resolve_character_idle_preview_clip(&character);
                let settings = CharacterControllerSettings::from_character(&character);
                let node = self.create_character_entity_at_room_hit(
                    room_id,
                    resource_id,
                    &resource.name,
                    character.model,
                    idle_clip,
                    settings,
                    player,
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
                    self.replace_primitive_selection(Selection::Face(face));
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
        idle_clip: Option<u16>,
        settings: CharacterControllerSettings,
        player: bool,
        hit_world: [f32; 3],
    ) -> NodeId {
        let translation = self.placement_translation_for_room_hit(room_id, hit_world);
        let active_floor = self.active_floor;
        let scene = self.project.active_scene_mut();
        let entity = scene.add_node(room_id, name.to_string(), NodeKind::Entity);
        if let Some(node) = scene.node_mut(entity) {
            node.transform.translation = translation;
            // Record the placed floor (0 = ground) so the cook binds the
            // entity to the right runtime room; Y can't select the floor.
            node.floor = active_floor;
        }
        if let Some(model_id) = model_id {
            scene.add_node(
                entity,
                "Model Renderer",
                NodeKind::ModelRenderer {
                    model: Some(model_id),
                    material: None,
                    visual_offset: [0; 3],
                    visual_scale_q8: psxed_project::MODEL_SCALE_ONE_Q8,
                },
            );
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
                settings,
                player,
            },
        );
        if player {
            scene.add_node(
                entity,
                "Camera",
                NodeKind::Camera {
                    settings: WorldCameraSettings::default(),
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
        material_id: ResourceId,
        translation: [f32; 3],
    ) -> Option<NodeId> {
        self.find_duplicate_room_child(room_id, translation, |_, node| {
            matches!(
                node.kind,
                NodeKind::BoxProp { materials, .. }
                    if materials.iter().any(|material| *material == Some(material_id))
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
        character
            .model
            .and_then(|model_id| {
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
        let NodeKind::Room { grid } = &node.kind else {
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
            NodeKind::Room { grid } => Some(grid.sector_size),
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
            NodeKind::Room { grid } => {
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
                .find(|node| matches!(node.kind, NodeKind::Room { .. }))
                .map(|node| node.id)
        })
    }

    /// Base (floor 0) grid for a room, unrouted by `active_floor`. Use
    /// this for whole-room queries like the floor count; use
    /// [`Self::room_grid_view`] for active-floor reads.
    pub(crate) fn room_base_grid(&self, room_id: NodeId) -> Option<&WorldGrid> {
        match &self.project.active_scene().node(room_id)?.kind {
            NodeKind::Room { grid } => Some(grid),
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
            NodeKind::Room { grid } => {
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
        let pushed = {
            let Some(node) = self.project.active_scene_mut().node_mut(room_id) else {
                return;
            };
            let NodeKind::Room { grid } = &mut node.kind else {
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
                        let player = !self.has_player_source();
                        let idle_clip = self.resolve_character_idle_preview_clip(&character);
                        let settings = CharacterControllerSettings::from_character(&character);
                        self.push_undo();
                        let id = self.create_character_entity_at_room_hit(
                            room_id,
                            character_id,
                            &name,
                            character.model,
                            idle_clip,
                            settings,
                            player,
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
                PlaceKind::BoxProp => match self.resolve_place_image_prop_material() {
                    Ok((material_id, name)) => {
                        let size = image_prop_default_size_for_sector(sector_size_i);
                        if let Some(existing) =
                            self.find_duplicate_box_prop(room_id, material_id, translation)
                        {
                            self.reject_duplicate_placement(existing, "Box Prop");
                            return;
                        }
                        (
                            format!("{name} Box"),
                            NodeKind::BoxProp {
                                materials: [Some(material_id); psxed_project::BOX_PROP_FACE_COUNT],
                                vertices: psxed_project::box_prop_vertices_for_size(size),
                                collision_enabled: true,
                                break_flags: 0,
                            },
                        )
                    }
                    Err(message) => {
                        self.status = message;
                        return;
                    }
                },
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

        // Snapshot for undo BEFORE mutating. Each non-Place tool
        // shares the same snapshot point.
        self.push_undo();
        let wall_paint_shape = self.wall_paint_shape;
        let active_floor = self.active_floor;
        let scene = self.project.active_scene_mut();
        let Some(room) = scene.node_mut(room_id) else {
            return;
        };
        let NodeKind::Room { grid } = &mut room.kind else {
            return;
        };
        // Paint into the active floor's grid (floor 0 = base). Clamp so a
        // stale index can't panic; the index is always in range here.
        let floor_idx = active_floor.min(grid.floor_count().saturating_sub(1));
        let grid = grid
            .floor_mut(floor_idx)
            .expect("floor index clamped to range");
        let status = match tool {
            ViewTool::PaintFloor => {
                grid.set_floor_aligned_to_neighbors(sx, sz, 0, floor_mat);
                format!("Painted floor at {sx},{sz}")
            }
            ViewTool::PaintCeiling => {
                grid.set_ceiling_aligned_to_neighbors(sx, sz, floor_mat);
                format!("Painted ceiling at {sx},{sz}")
            }
            ViewTool::PaintWall => {
                // A wall pick supplies the exact edge to stack on.
                // Floor / ceiling / empty picks infer the edge from
                // the click position relative to the cell centre.
                let dir = if let Some(FaceRef {
                    kind: FaceKind::Wall { dir, .. },
                    ..
                }) = picked_face
                {
                    dir
                } else {
                    wall_paint_shape
                        .direction(hit_world[0] - cell_center[0], hit_world[2] - cell_center[1])
                };
                let stack = grid
                    .sector(sx, sz)
                    .map(|sector| sector.walls.get(dir).len())
                    .unwrap_or(0);
                grid.add_wall_above_stack_or_aligned(sx, sz, dir, wall_mat);
                if stack == 0 {
                    format!("Added {} wall at {sx},{sz}", direction_label(dir))
                } else {
                    format!(
                        "Added {} wall #{stack} on top at {sx},{sz}",
                        direction_label(dir)
                    )
                }
            }
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
            (1, 0) => "Material already assigned to selected Box Prop".to_string(),
            (1, _) => "Assigned material to Box Prop".to_string(),
            (_, 0) => "Material already assigned to selected Box Props".to_string(),
            (total, updated) if total == updated => {
                format!("Assigned material to {updated} selected Box Props")
            }
            (total, updated) => {
                format!("Assigned material to {updated}/{total} selected Box Props")
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
                self.project
                    .active_scene()
                    .node(*id)
                    .is_some_and(|node| matches!(node.kind, NodeKind::BoxProp { .. }))
            })
            .collect()
    }

    pub(crate) fn box_prop_materials_differ(&self, id: NodeId, material: ResourceId) -> bool {
        self.project
            .active_scene()
            .node(id)
            .and_then(|node| match &node.kind {
                NodeKind::BoxProp { materials, .. } => Some(materials),
                _ => None,
            })
            .is_some_and(|materials| materials.iter().any(|slot| *slot != Some(material)))
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
            let NodeKind::BoxProp { materials, .. } = &mut node.kind else {
                continue;
            };
            if materials.iter().all(|slot| *slot == Some(material)) {
                continue;
            }
            *materials = [Some(material); psxed_project::BOX_PROP_FACE_COUNT];
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
        let Some(sector) = grid.sector_mut(face.sx, face.sz) else {
            return false;
        };
        match face.kind {
            FaceKind::Floor => sector
                .floor
                .as_mut()
                .map(|f| f.material = material)
                .is_some(),
            FaceKind::Ceiling => sector
                .ceiling
                .as_mut()
                .map(|c| c.material = material)
                .is_some(),
            FaceKind::Wall { dir, stack } => sector
                .walls
                .get_mut(dir)
                .get_mut(stack as usize)
                .map(|w| w.material = material)
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
