use super::*;

impl EditorWorkspace {
    pub(crate) fn replace_node_selection(&mut self, id: NodeId) {
        self.selection.selected_prefab = None;
        self.selection.selected_node = id;
        self.selection.selected_nodes.clear();
        self.selection.selected_nodes.insert(id);
        self.selection.node_selection_anchor = Some(id);
    }

    pub(crate) fn clear_node_selection_state(&mut self) {
        self.selection.selected_node = NodeId::ROOT;
        self.selection.selected_nodes.clear();
        self.selection.node_selection_anchor = None;
    }

    pub(crate) fn replace_resource_selection(&mut self, id: ResourceId) {
        self.selection.selected_prefab = None;
        self.selection.selected_resource = Some(id);
        self.selection.selected_resources.clear();
        self.selection.selected_resources.insert(id);
        self.selection.resource_selection_anchor = Some(id);
        self.resource_delete_confirm = None;
        if matches!(
            self.project.resource(id).map(|resource| &resource.data),
            Some(ResourceData::Material(_))
        ) {
            self.brush_material = Some(id);
        }
    }

    pub(crate) fn clear_resource_selection_state(&mut self) {
        self.selection.selected_resource = None;
        self.selection.selected_prefab = None;
        self.selection.selected_resources.clear();
        self.selection.resource_selection_anchor = None;
        self.resource_delete_confirm = None;
    }

    pub(crate) fn replace_primitive_selection(&mut self, selection: Selection) {
        self.selection.selected_prefab = None;
        self.selection.selected_primitive = Some(selection);
        self.selection.selected_primitives.clear();
        self.selection.selected_primitives.push(selection);
    }

    pub(crate) fn clear_primitive_selection_state(&mut self) {
        self.selection.clear_primitives();
    }

    pub(crate) fn replace_prefab_selection(&mut self, path: PathBuf) {
        let name = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        self.clear_resource_selection_state();
        self.clear_primitive_selection_state();
        self.selection.selected_prefab = Some(path);
        self.status = format!("Selected shared prefab '{name}'");
    }

    pub(crate) fn select_all_current_scope(&mut self) {
        if self.selection.selected_resource.is_some()
            || !self.selection.selected_resources.is_empty()
        {
            self.select_all_resources();
            return;
        }

        if (matches!(self.active_tool, ViewTool::Select)
            || self.selection.selected_primitive.is_some()
            || !self.selection.selected_primitives.is_empty())
            && self.select_all_primitives_in_active_room()
        {
            return;
        }

        self.select_all_scene_nodes();
    }

    pub(crate) fn select_all_scene_nodes(&mut self) {
        let ids: Vec<NodeId> = self
            .scene_node_order()
            .into_iter()
            .filter(|id| *id != NodeId::ROOT)
            .collect();
        if ids.is_empty() {
            self.status = "No scene nodes to select".to_string();
            return;
        }

        self.selection.selected_nodes = ids.iter().copied().collect();
        self.selection.selected_node = ids[0];
        self.selection.node_selection_anchor = Some(ids[0]);
        self.clear_resource_selection_state();
        self.clear_primitive_selection_state();
        self.clear_sector_selection();
        self.status = if ids.len() == 1 {
            "Selected 1 node".to_string()
        } else {
            format!("Selected {} nodes", ids.len())
        };
    }

    pub(crate) fn select_all_resources(&mut self) {
        let ids: Vec<ResourceId> = self
            .project
            .resources
            .iter()
            .map(|resource| resource.id)
            .collect();
        if ids.is_empty() {
            self.status = "No resources to select".to_string();
            return;
        }

        self.selection.selected_resources = ids.iter().copied().collect();
        self.selection.selected_resource = Some(ids[0]);
        self.selection.selected_prefab = None;
        self.selection.resource_selection_anchor = Some(ids[0]);
        self.resource_delete_confirm = None;
        self.clear_node_selection_state();
        self.clear_primitive_selection_state();
        self.clear_sector_selection();
        self.status = if ids.len() == 1 {
            "Selected 1 resource".to_string()
        } else {
            format!("Selected {} resources", ids.len())
        };
    }

    pub(crate) fn select_all_primitives_in_active_room(&mut self) -> bool {
        let Some(room) = self.active_room_id() else {
            self.status = "No active room to select".to_string();
            return false;
        };
        let selections = self.all_primitive_selections_in_room(room, self.selection_mode);
        if selections.is_empty() {
            self.status = format!("No {} primitives to select", self.selection_mode.label());
            return false;
        }

        self.selection.selected_primitives = selections;
        self.selection.selected_primitive = self.selection.selected_primitives.first().copied();
        self.clear_sector_selection();
        self.clear_node_selection_state();
        self.update_primitive_resource_selection();
        self.status = format!(
            "Selected {} {} primitives",
            self.selection.selected_primitives.len(),
            self.selection_mode.label()
        );
        true
    }

    pub(crate) fn all_primitive_selections_in_room(
        &self,
        room: NodeId,
        mode: SelectionMode,
    ) -> Vec<Selection> {
        let mut selections = Vec::new();
        for face in self.all_faces_in_room(room) {
            match mode {
                SelectionMode::Face => {
                    if matches!(self.horizontal_edit_mode, HorizontalEditMode::Triangle)
                        && matches!(face.kind, FaceKind::Floor | FaceKind::Ceiling)
                    {
                        for triangle in self.horizontal_triangle_refs_for_face(face) {
                            push_unique_selection(&mut selections, Selection::Triangle(triangle));
                        }
                    } else {
                        push_unique_selection(&mut selections, Selection::Face(face));
                    }
                }
                SelectionMode::Edge => {
                    if matches!(self.horizontal_edit_mode, HorizontalEditMode::Triangle)
                        && matches!(face.kind, FaceKind::Floor | FaceKind::Ceiling)
                    {
                        for triangle in self.horizontal_triangle_refs_for_face(face) {
                            for edge in triangle_edges(triangle) {
                                push_unique_selection(&mut selections, Selection::Edge(edge));
                            }
                        }
                    } else {
                        for edge in face_edges(face) {
                            push_unique_selection(&mut selections, Selection::Edge(edge));
                        }
                    }
                }
                SelectionMode::Vertex => {
                    if matches!(self.horizontal_edit_mode, HorizontalEditMode::Triangle)
                        && matches!(face.kind, FaceKind::Floor | FaceKind::Ceiling)
                    {
                        for triangle in self.horizontal_triangle_refs_for_face(face) {
                            for vertex in triangle_vertices(triangle) {
                                push_unique_selection(&mut selections, Selection::Vertex(vertex));
                            }
                        }
                    } else {
                        for vertex in face_vertices(face) {
                            push_unique_selection(&mut selections, Selection::Vertex(vertex));
                        }
                    }
                }
            }
        }
        selections
    }

    pub(crate) fn all_faces_in_room(&self, room: NodeId) -> Vec<FaceRef> {
        let Some(grid) = self.room_grid_view(room) else {
            return Vec::new();
        };

        let mut faces = Vec::new();
        for sx in 0..grid.width {
            for sz in 0..grid.depth {
                let Some(sector) = grid.sector(sx, sz) else {
                    continue;
                };
                if sector.floor.is_some() {
                    faces.push(FaceRef {
                        room,
                        sx,
                        sz,
                        kind: FaceKind::Floor,
                    });
                }
                if sector.ceiling.is_some() {
                    faces.push(FaceRef {
                        room,
                        sx,
                        sz,
                        kind: FaceKind::Ceiling,
                    });
                }
                for dir in GridDirection::ALL {
                    for (stack, _) in sector.walls.get(dir).iter().enumerate() {
                        let Ok(stack) = u8::try_from(stack) else {
                            continue;
                        };
                        faces.push(FaceRef {
                            room,
                            sx,
                            sz,
                            kind: FaceKind::Wall { dir, stack },
                        });
                    }
                }
            }
        }
        faces
    }

    pub(crate) fn selected_primitive_targets(&self) -> Vec<Selection> {
        if self.selection.selected_primitives.is_empty() {
            self.selection.selected_primitive.into_iter().collect()
        } else {
            self.selection.selected_primitives.clone()
        }
    }

    pub(crate) fn primitive_is_selected(&self, selection: Selection) -> bool {
        self.selection.selected_primitives.contains(&selection)
            || (self.selection.selected_primitives.is_empty()
                && self.selection.selected_primitive == Some(selection))
    }

    pub(crate) fn push_selected_primitive_unique(&mut self, selection: Selection) {
        if !self.selection.selected_primitives.contains(&selection) {
            self.selection.selected_primitives.push(selection);
        }
        self.selection.selected_primitive = Some(selection);
    }

    pub(crate) fn update_primitive_resource_selection(&mut self) {
        if self.selection.selected_primitives.len() == 1 {
            let resource = match self.selection.selected_primitives[0] {
                Selection::Face(face) => self.face_material(face),
                Selection::Triangle(triangle) => self.triangle_material(triangle),
                Selection::Edge(_) | Selection::Vertex(_) => None,
            };
            if let Some(id) = resource {
                self.replace_resource_selection(id);
                return;
            }
        }
        self.clear_resource_selection_state();
    }

    pub(crate) fn node_is_selected(&self, id: NodeId) -> bool {
        self.selection.selected_nodes.contains(&id)
            || (self.selection.selected_nodes.is_empty() && self.selection.selected_node == id)
    }

    pub(crate) fn resource_is_selected(&self, id: ResourceId) -> bool {
        self.selection.selected_resources.contains(&id)
            || (self.selection.selected_resources.is_empty()
                && self.selection.selected_resource == Some(id))
    }

    pub(crate) fn apply_node_selection_modifiers(
        &mut self,
        id: NodeId,
        modifiers: egui::Modifiers,
        visible_order: &[NodeId],
    ) {
        let toggle = modifiers.command || modifiers.ctrl;
        self.selection
            .apply_node_modifiers(id, modifiers.shift, toggle, visible_order);
        self.clear_sector_selection();

        let count = self.selection.selected_nodes.len();
        let scene = self.project.active_scene();
        if count > 1 {
            self.status = format!("Selected {count} nodes");
        } else if let Some(n) = scene.node(self.selection.selected_node) {
            self.status = format!("Selected {} '{}'", n.kind.label(), n.name);
        } else {
            self.status = "Cleared node selection".to_string();
        }
    }

    pub(crate) fn apply_resource_selection_modifiers(
        &mut self,
        id: ResourceId,
        modifiers: egui::Modifiers,
        visible_order: &[ResourceId],
    ) {
        let toggle = modifiers.command || modifiers.ctrl;
        self.selection
            .apply_resource_modifiers(id, modifiers.shift, toggle, visible_order);
        self.clear_sector_selection();
        self.resource_delete_confirm = None;

        let count = self.selection.selected_resources.len();
        if count <= 1 {
            if let Some(selected) = self.selection.selected_resource {
                if matches!(
                    self.project
                        .resource(selected)
                        .map(|resource| &resource.data),
                    Some(ResourceData::Material(_))
                ) {
                    self.brush_material = Some(selected);
                }
            }
        }
        if count > 1 {
            self.status = format!("Selected {count} resources");
        } else if let Some(id) = self.selection.selected_resource {
            if let Some(name) = self.project.resource_name(id) {
                self.status = format!("Selected {name}");
            }
        } else {
            self.status = "Cleared resource selection".to_string();
        }
    }

    pub(crate) fn selected_resource_ids_in_project_order(&self) -> Vec<ResourceId> {
        let mut selected = self.selection.selected_resources.clone();
        if selected.is_empty() {
            if let Some(id) = self.selection.selected_resource {
                selected.insert(id);
            }
        }
        self.project
            .resources
            .iter()
            .map(|resource| resource.id)
            .filter(|id| selected.contains(id))
            .collect()
    }

    pub(crate) fn resource_delete_targets(&self, fallback: ResourceId) -> Vec<ResourceId> {
        let selected = self.selected_resource_ids_in_project_order();
        if selected.is_empty() && self.project.resource(fallback).is_some() {
            vec![fallback]
        } else {
            selected
        }
    }

    pub(crate) fn begin_resource_delete_confirmation(&mut self) {
        let targets = self
            .selection
            .selected_resource
            .map(|id| self.resource_delete_targets(id))
            .unwrap_or_else(|| self.selected_resource_ids_in_project_order());
        if targets.is_empty() {
            self.status = "No resource selected".to_string();
            self.resource_delete_confirm = None;
            return;
        }
        self.status = if targets.len() == 1 {
            "Confirm resource deletion in the inspector".to_string()
        } else {
            format!(
                "Confirm deletion of {} resources in the inspector",
                targets.len()
            )
        };
        self.resource_delete_confirm = Some(targets);
    }

    pub(crate) fn draw_resource_delete_controls(
        &mut self,
        ui: &mut egui::Ui,
        fallback: ResourceId,
    ) -> bool {
        let targets = self.resource_delete_targets(fallback);
        if targets.is_empty() {
            return false;
        }

        let label = self.resource_delete_label(&targets);
        let reference_count: usize = targets
            .iter()
            .map(|id| self.project.resource_reference_count(*id))
            .sum();
        let confirming = self
            .resource_delete_confirm
            .as_ref()
            .is_some_and(|pending| pending.as_slice() == targets.as_slice());

        ui.separator();
        if confirming {
            let mut confirmed = false;
            let mut cancelled = false;
            section_frame().show(ui, |ui| {
                ui.label(
                    RichText::new(format!("Delete {label}?"))
                        .strong()
                        .color(Color32::from_rgb(255, 190, 150)),
                );
                ui.label(
                    RichText::new(
                        "This removes the resource and deletes its project-owned backing files.",
                    )
                    .color(STUDIO_TEXT_WEAK)
                    .small(),
                );
                if reference_count > 0 {
                    ui.label(
                        RichText::new(format!(
                            "{reference_count} project reference(s) will be cleared."
                        ))
                        .color(Color32::from_rgb(255, 218, 150))
                        .small(),
                    );
                }
                ui.horizontal(|ui| {
                    if ui
                        .button(icons::label(icons::TRASH, "Confirm Delete"))
                        .clicked()
                    {
                        confirmed = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancelled = true;
                    }
                });
            });
            if confirmed {
                self.delete_resources(&targets);
                return true;
            }
            if cancelled {
                self.resource_delete_confirm = None;
                self.status = "Resource deletion cancelled".to_string();
            }
        } else if ui
            .button(icons::label(
                icons::TRASH,
                if targets.len() == 1 {
                    "Delete Resource"
                } else {
                    "Delete Resources"
                },
            ))
            .clicked()
        {
            self.resource_delete_confirm = Some(targets);
            self.status = format!("Confirm deletion of {label}");
        }
        false
    }

    pub(crate) fn resource_delete_label(&self, ids: &[ResourceId]) -> String {
        match ids {
            [id] => self
                .project
                .resource_name(*id)
                .map(|name| format!("'{name}'"))
                .unwrap_or_else(|| format!("resource #{}", id.raw())),
            _ => format!("{} resources", ids.len()),
        }
    }

    pub(crate) fn delete_resources(&mut self, ids: &[ResourceId]) {
        let targets: Vec<ResourceId> = ids
            .iter()
            .copied()
            .filter(|id| self.project.resource(*id).is_some())
            .collect();
        if targets.is_empty() {
            self.resource_delete_confirm = None;
            self.status = "No matching resources to delete".to_string();
            return;
        }

        let before = self.project.clone();
        let mut removed = 0usize;
        let mut cleared_references = 0usize;
        let mut deleted_files = 0usize;
        let mut skipped_files = 0usize;
        let mut removed_names = Vec::new();
        let mut failed = None;
        for id in targets {
            if self.brush_material == Some(id) {
                self.brush_material = None;
            }
            self.remove_texture_thumb(id);
            match self
                .project
                .delete_resource_with_files(id, &self.project_dir)
            {
                Ok(report) => {
                    removed += 1;
                    cleared_references += report.cleared_references;
                    deleted_files += report.deleted_files.len();
                    skipped_files += report.skipped_files.len();
                    removed_names.push(report.removed.name);
                }
                Err(error) => {
                    failed = Some(error.to_string());
                    break;
                }
            }
        }

        self.clear_resource_selection_state();
        self.resource_renaming = None;
        self.reconcile_selection_after_document_change();
        if removed > 0 {
            if deleted_files > 0 {
                self.history.clear();
            } else {
                self.history.record(before);
            }
            self.mark_dirty();
            let mut status = if removed == 1 {
                let name = removed_names
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "resource".to_string());
                if cleared_references > 0 {
                    format!("Deleted {name}; cleared {cleared_references} reference(s)")
                } else {
                    format!("Deleted {name}")
                }
            } else if cleared_references > 0 {
                format!("Deleted {removed} resources; cleared {cleared_references} reference(s)")
            } else {
                format!("Deleted {removed} resources")
            };
            if deleted_files > 0 {
                status.push_str(&format!("; deleted {deleted_files} file(s)"));
            }
            if skipped_files > 0 {
                status.push_str(&format!("; skipped {skipped_files} file path(s)"));
            }
            if let Some(error) = failed {
                status.push_str(&format!("; stopped: {error}"));
            }
            self.status = status;
        } else if let Some(error) = failed {
            self.status = format!("Delete failed: {error}");
        }
    }

    pub(crate) fn scene_node_order(&self) -> Vec<NodeId> {
        self.project
            .active_scene()
            .hierarchy_rows()
            .into_iter()
            .map(|row| row.id)
            .collect()
    }

    pub(crate) fn scene_node_effectively_hidden(&self, id: NodeId) -> bool {
        scene_node_hidden(self.project.active_scene(), &self.hidden_scene_nodes, id)
    }

    pub(crate) fn selected_node_ids_in_hierarchy(&self) -> Vec<NodeId> {
        let mut selected = self.selection.selected_nodes.clone();
        if self.selection.selected_node != NodeId::ROOT {
            selected.insert(self.selection.selected_node);
        }
        self.project
            .active_scene()
            .hierarchy_rows()
            .into_iter()
            .map(|row| row.id)
            .filter(|id| *id != NodeId::ROOT && selected.contains(id))
            .collect()
    }

    pub(crate) fn scene_tree_drag_sources(&self, source: NodeId) -> Vec<NodeId> {
        if !self.selection.selected_nodes.contains(&source) {
            return vec![source];
        }

        let scene = self.project.active_scene();
        let mut selected = self.selection.selected_nodes.clone();
        if self.selection.selected_node != NodeId::ROOT {
            selected.insert(self.selection.selected_node);
        }
        let ordered: Vec<NodeId> = scene
            .hierarchy_rows()
            .into_iter()
            .map(|row| row.id)
            .filter(|id| *id != NodeId::ROOT && selected.contains(id))
            .collect();

        let roots: Vec<NodeId> = ordered
            .iter()
            .copied()
            .filter(|id| {
                !ordered
                    .iter()
                    .any(|other| *other != *id && scene.is_descendant_of(*id, *other))
            })
            .collect();
        if roots.is_empty() {
            vec![source]
        } else {
            roots
        }
    }

    pub(crate) fn scene_tree_reparent_is_valid(
        &self,
        sources: &[NodeId],
        target_parent: NodeId,
    ) -> bool {
        let scene = self.project.active_scene();
        if scene.node(target_parent).is_none() {
            return false;
        }
        sources.iter().all(|source| {
            *source != NodeId::ROOT
                && scene.node(*source).is_some()
                && *source != target_parent
                && !scene.is_descendant_of(target_parent, *source)
        })
    }

    pub(crate) fn node_frame_bounds_3d(&self, id: NodeId) -> Option<([f32; 3], [f32; 3])> {
        if self.scene_node_effectively_hidden(id) {
            return None;
        }
        let scene = self.project.active_scene();
        let node = scene.node(id)?;
        if matches!(node.kind, NodeKind::Section { .. }) {
            return self.room_bounds_3d(node.id);
        }

        let entity_bounds = self.collect_entity_bounds(None);
        let mut current = Some(node.id);
        while let Some(id) = current {
            if let Some(bounds) = entity_bounds.iter().find(|b| b.node == id) {
                return Some((bounds.center, bounds.half_extents));
            }
            current = scene.node(id).and_then(|n| n.parent);
        }
        None
    }

    pub(crate) fn node_frame_bounds_2d(&self, id: NodeId) -> Option<([f32; 2], [f32; 2])> {
        if self.scene_node_effectively_hidden(id) {
            return None;
        }
        let scene = self.project.active_scene();
        let node = scene.node(id)?;
        match &node.kind {
            NodeKind::Section { grid } => {
                let (local_center, half) = grid_authored_editor_center_half(grid)?;
                let center = node_world(node);
                Some((
                    [center[0] + local_center[0], center[1] + local_center[1]],
                    half,
                ))
            }
            _ => Some((node_world(node), [0.75, 0.75])),
        }
    }

    pub(crate) fn retain_hidden_ui_nodes_for_project(&mut self) {
        let valid_ui_nodes: HashSet<(UiSceneId, UiNodeId)> = self
            .project
            .ui_scenes
            .iter()
            .flat_map(|scene| {
                let scene_id = scene.id;
                scene.nodes().iter().map(move |node| (scene_id, node.id))
            })
            .collect();
        self.hidden_ui_nodes
            .retain(|key| valid_ui_nodes.contains(key));
    }

    pub(crate) fn reconcile_selection_after_document_change(&mut self) {
        self.reconcile_brush_selection();
        let valid_nodes: HashSet<NodeId> = self
            .project
            .active_scene()
            .nodes()
            .iter()
            .map(|node| node.id)
            .collect();
        self.selection
            .selected_nodes
            .retain(|id| valid_nodes.contains(id));
        self.collapsed_scene_nodes
            .retain(|id| valid_nodes.contains(id));
        self.hidden_scene_nodes
            .retain(|id| valid_nodes.contains(id));
        if self
            .selection
            .node_selection_anchor
            .is_some_and(|id| !valid_nodes.contains(&id))
        {
            self.selection.node_selection_anchor = None;
        }
        if self.selection.selected_node != NodeId::ROOT
            && !valid_nodes.contains(&self.selection.selected_node)
        {
            self.selection.selected_node =
                first_in_order(&self.scene_node_order(), &self.selection.selected_nodes)
                    .unwrap_or(NodeId::ROOT);
        }

        let valid_resources: HashSet<ResourceId> = self
            .project
            .resources
            .iter()
            .map(|resource| resource.id)
            .collect();
        self.selection
            .selected_resources
            .retain(|id| valid_resources.contains(id));
        if self
            .selection
            .resource_selection_anchor
            .is_some_and(|id| !valid_resources.contains(&id))
        {
            self.selection.resource_selection_anchor = None;
        }
        if self
            .selection
            .selected_resource
            .is_some_and(|id| !valid_resources.contains(&id))
        {
            self.selection.selected_resource = self
                .project
                .resources
                .iter()
                .map(|resource| resource.id)
                .find(|id| self.selection.selected_resources.contains(id));
        }
        if let Some(ids) = &mut self.resource_delete_confirm {
            ids.retain(|id| valid_resources.contains(id));
            if ids.is_empty() {
                self.resource_delete_confirm = None;
            }
        }

        // The UI-scene list can shrink across undo/redo (a created or
        // duplicated scene is rolled back). Clamp the active index and
        // drop any pending scene-strip rename / delete-confirm so they
        // never point past the end.
        let ui_scene_count = self.project.ui_scenes.len();
        if ui_scene_count == 0 {
            self.active_ui_scene_index = 0;
        } else if self.active_ui_scene_index >= ui_scene_count {
            self.active_ui_scene_index = ui_scene_count - 1;
        }
        if self
            .ui_scene_renaming
            .as_ref()
            .is_some_and(|(index, _)| *index >= ui_scene_count)
        {
            self.ui_scene_renaming = None;
            self.ui_scene_rename_focus_pending = false;
        }
        if self
            .ui_scene_delete_confirm
            .is_some_and(|index| index >= ui_scene_count)
        {
            self.ui_scene_delete_confirm = None;
        }
        let scene_state_count = self.project.scene_states.len();
        if scene_state_count == 0 {
            self.active_scene_state_index = 0;
        } else if self.active_scene_state_index >= scene_state_count {
            self.active_scene_state_index = scene_state_count - 1;
        }
        self.retain_hidden_ui_nodes_for_project();
        // The selected UI node belongs to whatever scene is now active;
        // a stale id from a rolled-back scene snaps back to the canvas.
        let ui_root = self
            .current_ui_scene()
            .map(|scene| scene.root)
            .unwrap_or(UiNodeId::ROOT);
        if self
            .current_ui_scene()
            .is_none_or(|scene| scene.node(self.selection.selected_ui_node).is_none())
        {
            self.selection.selected_ui_node = ui_root;
        }

        // Layer creation is undoable. If undo removes the active top layer,
        // keep the authoring index inside the restored room instead of
        // carrying a stale value into the next Up/Down action.
        self.active_floor = self
            .floors_target_room()
            .and_then(|room| self.room_base_grid(room))
            .map(|grid| self.active_floor.min(grid.floor_count().saturating_sub(1)))
            .unwrap_or(0);
    }

    pub(crate) fn clear_sector_selection(&mut self) {
        self.selection.selected_sector = None;
        self.selection.selected_sectors.clear();
        self.selection.sector_selection_anchor = None;
        self.interaction.take_box_select_2d();
    }

    pub(crate) fn select_sector(&mut self, selection: SectorSelection, modifiers: egui::Modifiers) {
        let toggle = modifiers.command || modifiers.ctrl;
        if modifiers.shift {
            let anchor = self.selection.sector_selection_anchor.unwrap_or(selection);
            self.select_sector_rect(anchor, selection, toggle);
            return;
        }

        if !toggle {
            self.selection.selected_sectors.clear();
        }
        if toggle && self.selection.selected_sectors.remove(&selection) {
            self.selection.selected_sector = self
                .selection
                .selected_sectors
                .iter()
                .next()
                .map(|(_, sx, sz)| (*sx, *sz));
        } else {
            self.selection.selected_sectors.insert(selection);
            self.selection.selected_sector = Some((selection.1, selection.2));
        }
        self.selection.sector_selection_anchor = Some(selection);
        self.replace_node_selection(selection.0);
        self.clear_resource_selection_state();
        self.clear_primitive_selection_state();
        self.status = match self.selection.selected_sectors.len() {
            0 => "Cleared tile selection".to_string(),
            1 => format!("Selected sector {},{}", selection.1, selection.2),
            count => format!("Selected {count} sectors"),
        };
    }

    pub(crate) fn select_sector_rect(
        &mut self,
        anchor: SectorSelection,
        current: SectorSelection,
        additive: bool,
    ) {
        if anchor.0 != current.0 {
            return;
        }
        if !additive {
            self.selection.selected_sectors.clear();
        }
        let min_x = anchor.1.min(current.1);
        let max_x = anchor.1.max(current.1);
        let min_z = anchor.2.min(current.2);
        let max_z = anchor.2.max(current.2);
        for sx in min_x..=max_x {
            for sz in min_z..=max_z {
                self.selection.selected_sectors.insert((anchor.0, sx, sz));
            }
        }
        self.selection.sector_selection_anchor = Some(anchor);
        self.replace_node_selection(anchor.0);
        self.clear_resource_selection_state();
        self.clear_primitive_selection_state();
        self.selection.selected_sector = Some((current.1, current.2));
        self.status = format!("Selected {} sectors", self.selection.selected_sectors.len());
    }

    pub(crate) fn begin_viewport_box_select(
        &mut self,
        start: Pos2,
        room: Option<NodeId>,
        modifiers: egui::Modifiers,
    ) {
        let additive = modifiers.shift || modifiers.command || modifiers.ctrl;
        self.interaction = Interaction::BoxSelect2d(ViewportBoxSelect {
            start,
            current: start,
            room,
            additive,
            base_sectors: if additive {
                self.selection.selected_sectors.clone()
            } else {
                HashSet::new()
            },
        });
        if !additive {
            self.selection.selected_sectors.clear();
            self.selection.selected_sector = None;
            self.selection.sector_selection_anchor = None;
        }
        self.clear_resource_selection_state();
        self.clear_primitive_selection_state();
    }

    pub(crate) fn update_viewport_box_select(
        &mut self,
        current: Pos2,
        transform: ViewportTransform,
    ) -> bool {
        let Some(drag) = self.interaction.box_select_2d_mut() else {
            return false;
        };
        drag.current = current;
        let rect = drag.rect();
        let room = drag.room;
        let additive = drag.additive;
        let base_sectors = drag.base_sectors.clone();
        self.select_sectors_in_screen_rect(transform, rect, room, additive, &base_sectors);
        true
    }

    pub(crate) fn viewport_box_select_rect(&self) -> Option<Rect> {
        self.interaction
            .box_select_2d()
            .map(ViewportBoxSelect::rect)
    }

    pub(crate) fn begin_viewport_3d_box_select(
        &mut self,
        start: Pos2,
        room: Option<NodeId>,
        modifiers: egui::Modifiers,
    ) {
        let additive = modifiers.shift || modifiers.command || modifiers.ctrl;
        self.interaction = Interaction::BoxSelect3d(Viewport3dBoxSelect {
            start,
            current: start,
            room,
            additive,
            base_primitives: if additive {
                self.selected_primitive_targets()
            } else {
                Vec::new()
            },
        });
        if !additive {
            self.selection.selected_primitive = None;
            self.selection.selected_primitives.clear();
        }
        self.selection.selected_sector = None;
        self.selection.selected_sectors.clear();
        self.selection.sector_selection_anchor = None;
        self.clear_resource_selection_state();
    }

    pub(crate) fn update_viewport_3d_box_select(&mut self, current: Pos2, viewport: Rect) -> bool {
        let Some(drag) = self.interaction.box_select_3d_mut() else {
            return false;
        };
        drag.current = current;
        let rect = drag.rect();
        let room = drag.room;
        let additive = drag.additive;
        let base_primitives = drag.base_primitives.clone();
        self.select_primitives_in_viewport_3d_rect(
            viewport,
            rect,
            room,
            additive,
            &base_primitives,
        );
        true
    }

    pub(crate) fn end_viewport_3d_box_select(&mut self) {
        self.interaction.take_box_select_3d();
    }

    pub(crate) fn viewport_3d_box_select_rect(&self) -> Option<Rect> {
        self.interaction
            .box_select_3d()
            .map(Viewport3dBoxSelect::rect)
    }

    pub(crate) fn select_primitives_in_viewport_3d_rect(
        &mut self,
        viewport: Rect,
        rect: Rect,
        room_filter: Option<NodeId>,
        additive: bool,
        base_primitives: &[Selection],
    ) {
        let camera = self.viewport_3d_camera();
        let room_ids: Vec<NodeId> = self
            .project
            .active_scene()
            .nodes()
            .iter()
            .filter_map(|node| {
                if room_filter.is_some_and(|room| room != node.id) {
                    return None;
                }
                if self.scene_node_effectively_hidden(node.id) {
                    return None;
                }
                matches!(node.kind, NodeKind::Section { .. }).then_some(node.id)
            })
            .collect();

        let mut selected = if additive {
            base_primitives.to_vec()
        } else {
            Vec::new()
        };
        for room in room_ids {
            for selection in self.all_primitive_selections_in_room(room, self.selection_mode) {
                let Some(bounds) = self.selection_screen_bounds(selection, camera, viewport) else {
                    continue;
                };
                if bounds.intersects(rect) {
                    push_unique_selection(&mut selected, selection);
                }
            }
        }

        self.selection.selected_primitives = selected;
        self.selection.selected_primitive = self.selection.selected_primitives.last().copied();
        self.selection.selected_sector = None;
        self.selection.selected_sectors.clear();
        self.selection.sector_selection_anchor = None;
        self.clear_resource_selection_state();
        self.update_primitive_resource_selection();

        let selected_room = self
            .selection
            .selected_primitives
            .first()
            .map(Selection::room);
        if selected_room.is_some()
            && self
                .selection
                .selected_primitives
                .iter()
                .all(|selection| Some(selection.room()) == selected_room)
        {
            if let Some(room) = selected_room {
                self.replace_node_selection(room);
            }
        } else if self.selection.selected_primitives.is_empty() {
            self.clear_node_selection_state();
        }

        self.status = match self.selection.selected_primitives.len() {
            0 => "Cleared primitive selection".to_string(),
            1 => format!(
                "Selected {}",
                describe_selection(self.selection.selected_primitives[0])
            ),
            count => format!("Selected {count} primitives"),
        };
    }

    pub(crate) fn selection_screen_bounds(
        &self,
        selection: Selection,
        camera: ViewportCameraState,
        viewport: Rect,
    ) -> Option<Rect> {
        let points = self.selection_world_points(selection)?;
        let mut projected = Vec::with_capacity(points.len());
        for point in points {
            projected.push(project_world_to_viewport_screen(camera, viewport, point)?);
        }
        let mut bounds = Rect::from_points(&projected);
        if matches!(selection, Selection::Edge(_) | Selection::Vertex(_)) {
            bounds = bounds.expand(4.0);
        }
        Some(bounds)
    }

    pub(crate) fn selection_world_points(&self, selection: Selection) -> Option<Vec<[f32; 3]>> {
        match selection {
            Selection::Face(face) => Some(self.face_world_corners(face)?.to_vec()),
            Selection::Triangle(triangle) => Some(self.triangle_world_corners(triangle)?.to_vec()),
            Selection::Edge(edge) => {
                let grid = self.room_grid_view(edge.room)?;
                let (a, b) = edge_endpoint_corners(edge);
                let a = face_corner_world(grid, a)?.map(|value| value as f32);
                let b = face_corner_world(grid, b)?.map(|value| value as f32);
                Some(vec![a, b])
            }
            Selection::Vertex(vertex) => {
                let grid = self.room_grid_view(vertex.room)?;
                let point = face_corner_world(grid, vertex.anchor.as_face_corner())?
                    .map(|value| value as f32);
                Some(vec![point])
            }
        }
    }

    pub(crate) fn select_sectors_in_screen_rect(
        &mut self,
        transform: ViewportTransform,
        rect: Rect,
        room_filter: Option<NodeId>,
        additive: bool,
        base_sectors: &HashSet<SectorSelection>,
    ) {
        let active_floor = self.active_floor;
        let scene = self.project.active_scene();
        let mut selected = if additive {
            base_sectors.clone()
        } else {
            HashSet::new()
        };
        for node in scene.nodes() {
            if room_filter.is_some_and(|room| room != node.id) {
                continue;
            }
            if self.scene_node_effectively_hidden(node.id) {
                continue;
            }
            let NodeKind::Section { grid } = &node.kind else {
                continue;
            };
            let idx = active_floor.min(grid.floor_count().saturating_sub(1));
            let Some(grid) = grid.floor(idx) else {
                continue;
            };
            let node_center = node_world(node);
            for sx in 0..grid.width {
                for sz in 0..grid.depth {
                    let Some(sector) = grid.sector(sx, sz) else {
                        continue;
                    };
                    if !sector.has_geometry() {
                        continue;
                    }
                    let local_tile_center = grid_cell_editor_center(grid, sx, sz);
                    let tile_center = [
                        node_center[0] + local_tile_center[0],
                        node_center[1] + local_tile_center[1],
                    ];
                    let tile_rect = transform.world_rect_to_screen(tile_center, [0.5, 0.5]);
                    if tile_rect.intersects(rect) {
                        let selection = (node.id, sx, sz);
                        selected.insert(selection);
                    }
                }
            }
        }

        let mut selected_ordered: Vec<_> = selected.iter().copied().collect();
        selected_ordered.sort_by_key(|(room, sx, sz)| (room.raw(), *sx, *sz));
        self.selection.selected_sectors = selected;
        self.selection.selected_sector = selected_ordered.first().map(|(_, sx, sz)| (*sx, *sz));
        self.selection.sector_selection_anchor = selected_ordered.first().copied();
        self.clear_resource_selection_state();
        self.clear_primitive_selection_state();

        let selected_room = selected_ordered.first().map(|(room, _, _)| *room);
        if selected_room.is_some()
            && selected_ordered
                .iter()
                .all(|(room, _, _)| Some(*room) == selected_room)
        {
            if let Some(room) = selected_room {
                self.replace_node_selection(room);
            }
        } else if selected_ordered.is_empty() {
            self.clear_node_selection_state();
        }

        self.status = match self.selection.selected_sectors.len() {
            0 => "Cleared tile selection".to_string(),
            1 => "Selected 1 sector".to_string(),
            count => format!("Selected {count} sectors"),
        };
    }
}
