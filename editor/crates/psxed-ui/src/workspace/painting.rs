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
    /// Character explicitly authored for the player role when there is one
    /// unambiguous choice. Enemy profiles never participate in this fallback,
    /// so adding the light/heavy catalogue cannot make a new player spawn
    /// ambiguous.
    pub(crate) fn default_player_character_resource(&self) -> Option<ResourceId> {
        let mut players = self.project.resources.iter().filter_map(|resource| {
            matches!(
                &resource.data,
                ResourceData::Character(character)
                    if character.spawn_role == psxed_project::CharacterSpawnRole::Player
            )
            .then_some(resource.id)
        });
        let player = players.next()?;
        players.next().is_none().then_some(player)
    }

    /// BSP scenes have no legacy `Section`/Room owner. Their world root is
    /// the correct parent for authored point entities because the PXBSP cook
    /// consumes those transforms directly in world units.
    pub(crate) fn bsp_authoring_root(&self) -> Option<NodeId> {
        Some(self.project.active_scene().root)
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
        self.paint_bsp_brush_face_target(brush, face);
    }

    pub(crate) fn paint_bsp_brush_face_target(&mut self, brush: usize, face: usize) {
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
        let before = self.project.active_scene().nodes().len();
        self.place_node_at_world_hit(root, hit);
        let placed = self.project.active_scene().nodes().len() > before;
        if placed {
            self.status = if matches!(self.place_kind, PlaceKind::PointOfInterest) {
                format!(
                    "Placed Point of Interest at {:.0},{:.0},{:.0} — edit Page 1 to replace the default message",
                    hit[0], hit[1], hit[2]
                )
            } else {
                format!(
                    "Placed {} at {:.0},{:.0},{:.0}",
                    self.place_kind.label(),
                    hit[0],
                    hit[1],
                    hit[2]
                )
            };
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

    pub(crate) fn drop_resource_2d(&mut self, resource_id: ResourceId, editor_world: [f32; 2]) {
        let Some(root) = self.bsp_authoring_root() else {
            self.status = "Drop needs a BSP brush world".to_string();
            return;
        };
        let Some(resource) = self.project.resource(resource_id) else {
            self.status = format!("Resource #{} no longer exists", resource_id.raw());
            return;
        };
        if matches!(resource.data, ResourceData::Material(_)) {
            let name = resource.name.clone();
            let Some((brush, face)) = self.pick_brush_face_for_selection_at_2d(editor_world) else {
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
            }
            self.mark_dirty();
            self.status = format!(
                "Assigned {name} to BSP brush {} face {}",
                brush + 1,
                face + 1
            );
            return;
        }
        let Some(surface_y) = self.bsp_upward_surface_y(editor_world, self.orthographic_focus[1])
        else {
            self.status = "Drop over an upward-facing BSP brush surface".to_string();
            return;
        };
        self.drop_resource_at_room_hit(
            resource_id,
            root,
            [editor_world[0], surface_y + 1.0, editor_world[1]],
            None,
        );
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
        _settings: CharacterControllerSettings,
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
                loadout: None,
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

    /// World-space sector size of the named Room, or `None` if the
    /// node isn't a Room.
    pub(crate) fn room_sector_size(&self, room_id: NodeId) -> Option<i32> {
        let node = self.project.active_scene().node(room_id)?;
        match &node.kind {
            NodeKind::Section { grid } => Some(grid.sector_size),
            _ => None,
        }
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

    /// Place the active entity kind at a BSP world-space hit.
    pub(crate) fn place_node_at_world_hit(&mut self, room_id: NodeId, hit_world: [f32; 3]) {
        let sector_size_i = self.room_sector_size(room_id).unwrap_or(1024);
        let translation = self.placement_translation_for_room_hit(room_id, hit_world);
        let kind = self.place_kind;
        if matches!(kind, PlaceKind::PlayerSpawn) && self.has_player_source() {
            self.status =
                    "Only one player source is allowed per world. Delete or demote the existing player first."
                        .to_string();
            return;
        }
        if matches!(kind, PlaceKind::PointOfInterest) {
            self.push_undo();
            let active_floor = self.active_floor;
            let scene = self.project.active_scene_mut();
            let entity = scene.add_node(room_id, "Point of Interest", NodeKind::Entity);
            if let Some(node) = scene.node_mut(entity) {
                node.transform.translation = translation;
                node.floor = active_floor;
            }
            let component = scene.add_node(
                entity,
                "Point of Interest",
                NodeKind::PointOfInterest {
                    pages: psxed_project::default_point_of_interest_pages(),
                    pages_it: Vec::new(),
                    prompt: psxed_project::default_point_of_interest_prompt(),
                    radius: psxed_project::default_point_of_interest_radius(),
                    marker_height: psxed_project::default_point_of_interest_marker_height(),
                    repeatable: true,
                    persistence_id: String::new(),
                    reward: None,
                    enabled: true,
                },
            );
            // The authored payload is the useful first inspector. Viewport
            // picking subsequently selects the Entity host for movement.
            self.replace_node_selection(component);
            self.clear_resource_selection_state();
            self.clear_primitive_selection_state();
            self.status =
                "Placed Point of Interest — edit Page 1 to replace the default message".to_string();
            self.mark_dirty();
            self.return_to_select_after_place();
            return;
        }
        let (default_name, node_kind): (String, NodeKind) = match kind {
            PlaceKind::PlayerSpawn => (
                "Player Spawn".to_string(),
                NodeKind::SpawnPoint {
                    player: true,
                    character: self.default_player_character_resource(),
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
                        let id = self
                            .create_model_entity_at_room_hit(room_id, model_id, &name, hit_world);
                        self.replace_node_selection(id);
                        self.clear_resource_selection_state();
                        self.clear_primitive_selection_state();
                        self.status = "Placed Prop".to_string();
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
                    if player && character.spawn_role == psxed_project::CharacterSpawnRole::Player {
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
                        "Placed Player Character".to_string()
                    } else {
                        "Placed Character".to_string()
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
                            destructible: None,
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
                        uvs: [GridUvTransform::IDENTITY; psxed_project::ARCH_PROP_MATERIAL_COUNT],
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
                if let Some(existing) = self.find_duplicate_particle_emitter(room_id, translation) {
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
            PlaceKind::Destructible => (
                "Destructible".to_string(),
                NodeKind::Destructible {
                    max_health: psxed_project::default_destructible_max_health(),
                    damage_affinity: psxed_project::DestructibleDamageAffinity::Both,
                    enabled: true,
                },
            ),
            PlaceKind::VitalityCircle => (
                "Horizon Vitality Circle".to_string(),
                NodeKind::VitalityCircle {
                    axis: psxed_project::VitalityCircleAxis::Horizon,
                    radius: psxed_project::default_vitality_circle_radius(),
                    refill_per_second: psxed_project::default_vitality_circle_refill_rate(),
                    drain_per_second: psxed_project::default_vitality_circle_drain_rate(),
                    enabled: true,
                },
            ),
            PlaceKind::PointOfInterest => unreachable!("handled above"),
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
        self.status = format!("Placed {}", kind.label());
        self.mark_dirty();
        self.return_to_select_after_place();
    }

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
            MaterialTarget::BrushFace { brush, face } => self
                .project
                .active_scene()
                .brushes
                .get(brush)
                .and_then(|brush| brush.faces.get(face))
                .and_then(|face| face.material),
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
        for target in self.selected_brush_material_targets() {
            push_unique_material_target(&mut targets, target);
        }
        targets
    }

    /// Material surfaces implied by the current brush selection. Explicit
    /// face elements remain precise for a single brush; selecting whole
    /// brushes means every face, and a multi-brush selection always behaves
    /// as one whole-object material target.
    pub(crate) fn selected_brush_material_targets(&self) -> Vec<MaterialTarget> {
        let brushes = self.selected_brush_set();
        if brushes.is_empty() {
            return Vec::new();
        }
        if self.brush_edit_mode == BrushEditMode::Face {
            return self
                .selected_brush_faces
                .iter()
                .copied()
                .filter(|(brush, face)| {
                    self.project
                        .active_scene()
                        .brushes
                        .get(*brush)
                        .and_then(|brush| brush.faces.get(*face))
                        .is_some()
                })
                .map(|(brush, face)| MaterialTarget::BrushFace { brush, face })
                .collect();
        }
        // Brush/Move mode is an object selection even though picking retains
        // the hit face for cycling and Inspector context. Never let that
        // implementation detail silently turn a brush material assignment
        // into a one-face edit.
        if brushes.len() == 1 && self.brush_edit_mode != BrushEditMode::Move {
            let brush = brushes[0];
            let mut faces: Vec<usize> = self
                .selected_brush_elements
                .iter()
                .filter_map(|element| match element {
                    BrushElement::Face(face) => Some(*face),
                    BrushElement::Edge(..) | BrushElement::Vertex(_) => None,
                })
                .collect();
            if faces.is_empty() {
                faces.extend(self.selected_brush_face);
            }
            if !faces.is_empty() {
                faces.sort_unstable();
                faces.dedup();
                return faces
                    .into_iter()
                    .filter(|face| {
                        self.project
                            .active_scene()
                            .brushes
                            .get(brush)
                            .and_then(|brush| brush.faces.get(*face))
                            .is_some()
                    })
                    .map(|face| MaterialTarget::BrushFace { brush, face })
                    .collect();
            }
        }
        let scene = self.project.active_scene();
        brushes
            .into_iter()
            .flat_map(|brush| {
                let face_count = scene
                    .brushes
                    .get(brush)
                    .map_or(0, |brush| brush.faces.len());
                (0..face_count).map(move |face| MaterialTarget::BrushFace { brush, face })
            })
            .collect()
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
            MaterialTarget::BrushFace { brush, face } => {
                let Some(face) = self
                    .project
                    .active_scene_mut()
                    .brushes
                    .get_mut(brush)
                    .and_then(|brush| brush.faces.get_mut(face))
                else {
                    return false;
                };
                if face.material == material {
                    return false;
                }
                face.material = material;
                true
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
