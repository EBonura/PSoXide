use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrushGroupPick {
    Brush,
    Group(NodeId),
    Locked,
}

impl EditorWorkspace {
    pub(crate) fn handle_group_double_click_3d(
        &mut self,
        target: Option<Viewport3dPointerTarget>,
    ) -> bool {
        match target {
            Some(Viewport3dPointerTarget::Brush { brush, .. }) => {
                if let BrushGroupPick::Group(group) = self.brush_group_pick(brush) {
                    return self.open_group_for_editing(group);
                }
                false
            }
            Some(Viewport3dPointerTarget::Entity(hit)) => {
                if let BrushGroupPick::Group(group) = self.node_group_pick(hit.node) {
                    return self.open_group_for_editing(group);
                }
                false
            }
            Some(
                Viewport3dPointerTarget::Surface { .. }
                | Viewport3dPointerTarget::PrimitiveGizmo(_)
                | Viewport3dPointerTarget::NodeGizmo(_),
            )
            | None => self.close_open_group(),
        }
    }

    pub(crate) fn handle_group_double_click_2d(
        &mut self,
        world: [f32; 2],
        hits: &[ViewportHit],
    ) -> bool {
        if let Some(hit) = hits.iter().rev().find(|hit| hit.contains(world)) {
            if let BrushGroupPick::Group(group) = self.node_group_pick(hit.id) {
                return self.open_group_for_editing(group);
            }
            return false;
        }
        if let Some((brush, _)) = self.pick_brush_face_for_selection_at_2d(world) {
            if let BrushGroupPick::Group(group) = self.brush_group_pick(brush) {
                return self.open_group_for_editing(group);
            }
            return false;
        }
        self.close_open_group()
    }

    pub(crate) fn node_is_group(&self, id: NodeId) -> bool {
        self.project
            .active_scene()
            .node(id)
            .is_some_and(|node| matches!(node.kind, NodeKind::Group))
    }

    fn group_chain_from(&self, start: Option<NodeId>) -> Vec<NodeId> {
        let scene = self.project.active_scene();
        let mut chain = Vec::new();
        let mut current = start;
        let mut guard = 0usize;
        while let Some(id) = current {
            if guard >= scene.nodes().len() {
                break;
            }
            let Some(node) = scene.node(id) else {
                break;
            };
            if matches!(node.kind, NodeKind::Group) {
                chain.push(id);
            }
            current = node.parent;
            guard += 1;
        }
        chain
    }

    pub(crate) fn brush_group_chain(&self, brush: usize) -> Vec<NodeId> {
        let start = self
            .project
            .active_scene()
            .brushes
            .get(brush)
            .and_then(|brush| brush.group);
        self.group_chain_from(start)
    }

    pub(crate) fn node_group_chain(&self, node: NodeId) -> Vec<NodeId> {
        let start = self
            .project
            .active_scene()
            .node(node)
            .and_then(|node| node.parent);
        self.group_chain_from(start)
    }

    fn group_pick_for_chain(&self, chain: &[NodeId]) -> BrushGroupPick {
        match self.open_group {
            None => chain
                .last()
                .copied()
                .map_or(BrushGroupPick::Brush, BrushGroupPick::Group),
            Some(open) => {
                let Some(open_position) = chain.iter().position(|group| *group == open) else {
                    return BrushGroupPick::Locked;
                };
                if open_position == 0 {
                    BrushGroupPick::Brush
                } else {
                    // The highest still-closed group immediately below the
                    // current edit context owns the click.
                    BrushGroupPick::Group(chain[open_position - 1])
                }
            }
        }
    }

    pub(crate) fn brush_group_pick(&self, brush: usize) -> BrushGroupPick {
        self.group_pick_for_chain(&self.brush_group_chain(brush))
    }

    pub(crate) fn node_group_pick(&self, node: NodeId) -> BrushGroupPick {
        self.group_pick_for_chain(&self.node_group_chain(node))
    }

    pub(crate) fn brush_effectively_hidden(&self, brush: usize) -> bool {
        self.brush_group_chain(brush)
            .into_iter()
            .any(|group| self.scene_node_effectively_hidden(group))
    }

    pub(crate) fn brush_selected_through_group(&self, brush: usize) -> bool {
        self.brush_group_chain(brush)
            .into_iter()
            .any(|group| self.node_is_selected(group))
    }

    pub(crate) fn group_bounds_3d(&self, group: NodeId) -> Option<([f32; 3], [f32; 3])> {
        if !self.node_is_group(group) || self.scene_node_effectively_hidden(group) {
            return None;
        }
        let scene = self.project.active_scene();
        let mut bounds = None;
        for index in scene.brush_indices_in_group(group, true) {
            if self.brush_effectively_hidden(index) {
                continue;
            }
            let solved = scene.brushes[index].solve();
            if !solved.is_valid() {
                continue;
            }
            let center =
                std::array::from_fn(|axis| ((solved.min[axis] + solved.max[axis]) * 0.5) as f32);
            let half =
                std::array::from_fn(|axis| ((solved.max[axis] - solved.min[axis]) * 0.5) as f32);
            merge_bounds_3d(&mut bounds, center, half);
        }
        for entity in self.collect_entity_bounds(None) {
            if scene.is_descendant_of(entity.node, group) {
                merge_bounds_3d(&mut bounds, entity.center, entity.half_extents);
            }
        }
        bounds.map(bounds_3d_to_center_half)
    }

    pub(crate) fn open_group_for_editing(&mut self, group: NodeId) -> bool {
        if !self.node_is_group(group) {
            return false;
        }
        if let Some(open) = self.open_group {
            let scene = self.project.active_scene();
            if group != open && !scene.is_descendant_of(group, open) {
                self.status = "Close the current group before opening another branch".to_string();
                return false;
            }
        }
        self.open_group = Some(group);
        self.collapsed_scene_nodes.remove(&group);
        self.replace_node_selection(group);
        self.clear_brush_selection();
        self.status = self
            .project
            .active_scene()
            .node(group)
            .map(|node| format!("Opened group '{}' — objects outside are locked", node.name))
            .unwrap_or_else(|| "Opened group".to_string());
        true
    }

    pub(crate) fn close_open_group(&mut self) -> bool {
        let Some(current) = self.open_group else {
            return false;
        };
        let parent = self
            .project
            .active_scene()
            .node(current)
            .and_then(|node| node.parent);
        self.open_group = self.group_chain_from(parent).first().copied();
        self.replace_node_selection(current);
        self.clear_brush_selection();
        self.status = self.open_group.map_or_else(
            || "Closed group".to_string(),
            |group| {
                self.project
                    .active_scene()
                    .node(group)
                    .map(|node| format!("Returned to group '{}'", node.name))
                    .unwrap_or_else(|| "Returned to parent group".to_string())
            },
        );
        true
    }

    pub(crate) fn select_brush_with_group_semantics(
        &mut self,
        brush: usize,
        face: Option<usize>,
        modifiers: egui::Modifiers,
        double_clicked: bool,
    ) -> bool {
        match self.brush_group_pick(brush) {
            BrushGroupPick::Locked => {
                self.status = "Object is outside the open group".to_string();
                false
            }
            BrushGroupPick::Group(group) => {
                if double_clicked {
                    return self.open_group_for_editing(group);
                }
                let order = self.scene_node_order();
                self.apply_node_selection_modifiers(group, modifiers, &order);
                self.clear_brush_selection();
                self.clear_resource_selection_state();
                self.clear_primitive_selection_state();
                self.clear_sector_selection();
                true
            }
            BrushGroupPick::Brush => {
                self.clear_node_selection_state();
                self.clear_resource_selection_state();
                self.clear_primitive_selection_state();
                self.clear_sector_selection();
                if modifiers.shift || modifiers.command || modifiers.ctrl {
                    self.toggle_brush_selection(brush);
                } else {
                    self.replace_brush_selection(brush, face);
                }
                true
            }
        }
    }

    pub(crate) fn select_node_with_group_semantics(
        &mut self,
        node: NodeId,
        modifiers: egui::Modifiers,
        double_clicked: bool,
    ) -> bool {
        match self.node_group_pick(node) {
            BrushGroupPick::Locked => {
                self.status = "Object is outside the open group".to_string();
                false
            }
            BrushGroupPick::Group(group) => {
                if double_clicked {
                    return self.open_group_for_editing(group);
                }
                let order = self.scene_node_order();
                self.apply_node_selection_modifiers(group, modifiers, &order);
                self.clear_brush_selection();
                true
            }
            BrushGroupPick::Brush => {
                let order = self.scene_node_order();
                self.apply_node_selection_modifiers(node, modifiers, &order);
                self.clear_brush_selection();
                true
            }
        }
    }

    fn group_parent_for_new_group(&self) -> NodeId {
        if let Some(open) = self.open_group.filter(|id| self.node_is_group(*id)) {
            return open;
        }
        let scene = self.project.active_scene();
        let node_roots = self.selected_node_ids_in_hierarchy();
        let mut parents = node_roots
            .iter()
            .filter_map(|id| scene.node(*id).and_then(|node| node.parent));
        let first = parents.next();
        if first.is_some() && parents.all(|parent| Some(parent) == first) {
            return first.unwrap_or(scene.root);
        }
        let brush_groups: HashSet<Option<NodeId>> = self
            .selected_brush_set()
            .into_iter()
            .filter_map(|index| scene.brushes.get(index).map(|brush| brush.group))
            .collect();
        if brush_groups.len() == 1 {
            return brush_groups
                .into_iter()
                .next()
                .flatten()
                .unwrap_or(scene.root);
        }
        scene.root
    }

    pub(crate) fn group_current_selection(&mut self) -> bool {
        let brushes = self.selected_brush_set();
        let nodes = self.selected_node_ids_in_hierarchy();
        if brushes.is_empty() && nodes.is_empty() {
            self.status = "Select brushes or scene objects to group".to_string();
            return false;
        }
        let parent = self.group_parent_for_new_group();
        self.push_undo();
        let group = self
            .project
            .active_scene_mut()
            .add_node(parent, "Group", NodeKind::Group);
        for index in &brushes {
            if let Some(brush) = self.project.active_scene_mut().brushes.get_mut(*index) {
                brush.group = Some(group);
            }
        }
        for node in &nodes {
            let position = self
                .project
                .active_scene()
                .node(group)
                .map_or(0, |group| group.children.len());
            self.project
                .active_scene_mut()
                .move_node(*node, group, position);
        }
        self.clear_brush_selection();
        self.replace_node_selection(group);
        self.renaming = Some((group, "Group".to_string()));
        self.pending_rename_focus = true;
        self.status = format!(
            "Grouped {} object{}",
            brushes.len() + nodes.len(),
            if brushes.len() + nodes.len() == 1 {
                ""
            } else {
                "s"
            }
        );
        self.mark_dirty();
        true
    }

    pub(crate) fn ungroup_selected_groups(&mut self) -> bool {
        let mut groups: Vec<NodeId> = self
            .selected_node_ids_in_hierarchy()
            .into_iter()
            .filter(|id| self.node_is_group(*id))
            .collect();
        if groups.is_empty() {
            self.status = "Select one or more groups to ungroup".to_string();
            return false;
        }
        // Deepest first makes a parent+child multi-selection deterministic.
        groups.reverse();
        self.push_undo();
        let mut moved_brushes = Vec::new();
        let mut moved_nodes = Vec::new();
        for group in &groups {
            let (parent, children, sibling_index) = {
                let scene = self.project.active_scene();
                let Some(node) = scene.node(*group) else {
                    continue;
                };
                let parent = node.parent.unwrap_or(scene.root);
                let sibling_index = scene
                    .node(parent)
                    .and_then(|parent| parent.children.iter().position(|child| *child == *group))
                    .unwrap_or(0);
                (parent, node.children.clone(), sibling_index)
            };
            let parent_group = self.node_is_group(parent).then_some(parent);
            for (index, brush) in self
                .project
                .active_scene_mut()
                .brushes
                .iter_mut()
                .enumerate()
            {
                if brush.group == Some(*group) {
                    brush.group = parent_group;
                    moved_brushes.push(index);
                }
            }
            for (offset, child) in children.iter().copied().enumerate() {
                if self
                    .project
                    .active_scene_mut()
                    .move_node(child, parent, sibling_index + offset)
                {
                    moved_nodes.push(child);
                }
            }
            self.project.active_scene_mut().remove_node(*group);
            if self.open_group == Some(*group) {
                self.open_group = parent_group;
            }
        }
        if !moved_brushes.is_empty() {
            moved_brushes.sort_unstable();
            moved_brushes.dedup();
            self.selected_brush = moved_brushes.first().copied();
            self.selected_brushes = moved_brushes;
            self.selected_brush_face = None;
            self.selected_brush_faces.clear();
            self.selected_brush_elements.clear();
            self.clear_node_selection_state();
        } else if !moved_nodes.is_empty() {
            self.selection.selected_node = moved_nodes[0];
            self.selection.selected_nodes = moved_nodes.iter().copied().collect();
            self.selection.node_selection_anchor = moved_nodes.last().copied();
        } else {
            self.clear_node_selection_state();
        }
        self.status = format!(
            "Ungrouped {} group{}",
            groups.len(),
            if groups.len() == 1 { "" } else { "s" }
        );
        self.mark_dirty();
        true
    }

    pub(crate) fn merge_selected_groups(&mut self) -> bool {
        let groups: Vec<NodeId> = self
            .selected_node_ids_in_hierarchy()
            .into_iter()
            .filter(|id| self.node_is_group(*id))
            .collect();
        if groups.len() < 2 {
            self.status = "Select at least two groups to merge".to_string();
            return false;
        }
        let target = groups[0];
        self.push_undo();
        for source in groups.iter().copied().skip(1) {
            let children = self
                .project
                .active_scene()
                .node(source)
                .map(|node| node.children.clone())
                .unwrap_or_default();
            for brush in &mut self.project.active_scene_mut().brushes {
                if brush.group == Some(source) {
                    brush.group = Some(target);
                }
            }
            for child in children {
                let position = self
                    .project
                    .active_scene()
                    .node(target)
                    .map_or(0, |group| group.children.len());
                self.project
                    .active_scene_mut()
                    .move_node(child, target, position);
            }
            self.project.active_scene_mut().remove_node(source);
        }
        self.replace_node_selection(target);
        self.status = format!("Merged {} groups", groups.len());
        self.mark_dirty();
        true
    }

    pub(crate) fn prune_empty_groups(&mut self) {
        loop {
            let scene = self.project.active_scene();
            let empty = scene.nodes().iter().find_map(|node| {
                (matches!(node.kind, NodeKind::Group)
                    && node.children.is_empty()
                    && !scene
                        .brushes
                        .iter()
                        .any(|brush| brush.group == Some(node.id)))
                .then_some(node.id)
            });
            let Some(group) = empty else {
                break;
            };
            if self.open_group == Some(group) {
                self.open_group = None;
            }
            self.project.active_scene_mut().remove_node(group);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use psxed_project::brush::Brush;

    fn grouped_workspace() -> (EditorWorkspace, NodeId, NodeId, NodeId) {
        let mut project = ProjectDocument::new("brush groups");
        let root = project.active_scene().root;
        let outer = project
            .active_scene_mut()
            .add_node(root, "Outer", NodeKind::Group);
        let inner = project
            .active_scene_mut()
            .add_node(outer, "Inner", NodeKind::Group);
        let outside = project
            .active_scene_mut()
            .add_node(root, "Outside", NodeKind::Group);
        for (group, min) in [(Some(outer), 0), (Some(inner), 256), (Some(outside), 512)] {
            let mut brush = Brush::cuboid([min, 0, 0], [min + 128, 128, 128]);
            brush.group = group;
            project.active_scene_mut().brushes.push(brush);
        }
        (
            EditorWorkspace::with_project(std::env::temp_dir(), project),
            outer,
            inner,
            outside,
        )
    }

    #[test]
    fn closed_and_open_groups_promote_or_lock_brush_picks() {
        let (mut workspace, outer, inner, _) = grouped_workspace();

        assert_eq!(workspace.brush_group_pick(0), BrushGroupPick::Group(outer));
        assert_eq!(workspace.brush_group_pick(1), BrushGroupPick::Group(outer));
        assert!(workspace.open_group_for_editing(outer));
        assert_eq!(workspace.brush_group_pick(0), BrushGroupPick::Brush);
        assert_eq!(workspace.brush_group_pick(1), BrushGroupPick::Group(inner));
        assert_eq!(workspace.brush_group_pick(2), BrushGroupPick::Locked);
        assert!(workspace.open_group_for_editing(inner));
        assert_eq!(workspace.brush_group_pick(1), BrushGroupPick::Brush);
        assert!(workspace.close_open_group());
        assert_eq!(workspace.open_group, Some(outer));
    }

    #[test]
    fn duplicating_group_copies_subtree_brushes_and_entities() {
        let (mut workspace, outer, inner, _) = grouped_workspace();
        let child = workspace
            .project
            .active_scene_mut()
            .add_node(inner, "Enemy", NodeKind::Entity);
        workspace.replace_node_selection(outer);

        workspace.duplicate_current_selection();

        let copy = workspace.selection.selected_node;
        assert_ne!(copy, outer);
        assert_eq!(
            workspace
                .project
                .active_scene()
                .node(copy)
                .map(|node| node.name.as_str()),
            Some("Outer Copy")
        );
        assert_eq!(
            workspace
                .project
                .active_scene()
                .brush_indices_in_group(copy, true)
                .len(),
            2
        );
        let copied_enemy = workspace
            .project
            .active_scene()
            .nodes()
            .iter()
            .find(|node| node.name == "Enemy" && node.id != child)
            .expect("entity descendant copied");
        assert!(workspace
            .project
            .active_scene()
            .is_descendant_of(copied_enemy.id, copy));
    }

    #[test]
    fn brush_group_clipboard_remaps_ids_across_projects() {
        let (mut source, outer, _, _) = grouped_workspace();
        source.replace_node_selection(outer);
        assert!(source.copy_current_geometry());
        let clipboard = source
            .portable_geometry_clipboard
            .clone()
            .expect("portable clipboard");

        let mut destination = EditorWorkspace::with_project(
            std::env::temp_dir(),
            ProjectDocument::new("destination"),
        );
        destination.portable_geometry_clipboard = Some(clipboard);
        assert!(destination.paste_current_geometry());

        let pasted_root = destination.selection.selected_node;
        assert!(destination.node_is_group(pasted_root));
        assert_eq!(
            destination
                .project
                .active_scene()
                .brush_indices_in_group(pasted_root, true)
                .len(),
            2
        );
        assert!(destination
            .project
            .active_scene()
            .brush_indices_in_group(pasted_root, true)
            .into_iter()
            .all(|index| destination.project.active_scene().brushes[index]
                .group
                .is_some_and(|group| destination.node_is_group(group))));
    }

    #[test]
    fn hidden_parent_group_hides_nested_brushes() {
        let (mut workspace, outer, _, _) = grouped_workspace();
        workspace.hidden_scene_nodes.insert(outer);
        assert!(workspace.brush_effectively_hidden(0));
        assert!(workspace.brush_effectively_hidden(1));
        assert!(!workspace.brush_effectively_hidden(2));
    }

    #[test]
    fn selected_group_highlights_all_descendant_brushes() {
        let (mut workspace, outer, inner, _) = grouped_workspace();
        workspace.replace_node_selection(outer);
        assert!(workspace.brush_selected_through_group(0));
        assert!(workspace.brush_selected_through_group(1));
        assert!(!workspace.brush_selected_through_group(2));

        workspace.replace_node_selection(inner);
        assert!(!workspace.brush_selected_through_group(0));
        assert!(workspace.brush_selected_through_group(1));
    }

    #[test]
    fn group_move_applies_one_snapped_delta_to_brushes_and_entities() {
        let (mut workspace, _, inner, _) = grouped_workspace();
        let entity =
            workspace
                .project
                .active_scene_mut()
                .add_node(inner, "Enemy", NodeKind::Entity);
        workspace.snap_units = 16;
        workspace.interaction = Interaction::NodeGizmo(NodeGizmoDrag {
            mode: TransformGizmoMode::Move,
            handle: NodeGizmoHandle::Axis(PrimitiveGizmoAxis::X),
            start_pointer: Pos2::ZERO,
            screen_axis: Vec2::X,
            start_plane_hit: None,
            current_plane_delta_world: [0.0; 3],
            move_axis_world: [1.0, 0.0, 0.0],
            rotate: None,
            targets: vec![NodeGizmoTarget {
                node: entity,
                start_translation: [0.0; 3],
                start_rotation_degrees: [0.0; 3],
                start_image_prop_size: None,
                start_box_prop_vertices: None,
                start_cylinder_prop_geometry: None,
                start_arch_prop_geometry: None,
                sector_size: 1,
            }],
            group_brushes: vec![GroupBrushGizmoTarget {
                index: 1,
                start: workspace.project.active_scene().brushes[1].clone(),
            }],
            group_pivot: Some([320.0, 64.0, 64.0]),
            group_count: 1,
            current_steps: 2,
            snapshot_pushed: false,
            free: false,
        });

        workspace.apply_node_gizmo_drag();

        assert_eq!(
            workspace
                .project
                .active_scene()
                .node(entity)
                .expect("entity")
                .transform
                .translation,
            [32.0, 0.0, 0.0]
        );
        let solved = workspace.project.active_scene().brushes[1].solve();
        assert_eq!(solved.min, [288.0, 0.0, 0.0]);
        assert_eq!(solved.max, [416.0, 128.0, 128.0]);
        workspace.do_undo();
        assert_eq!(
            workspace
                .project
                .active_scene()
                .node(entity)
                .expect("entity after undo")
                .transform
                .translation,
            [0.0; 3]
        );
        assert_eq!(
            workspace.project.active_scene().brushes[1].solve().min,
            [256.0, 0.0, 0.0]
        );
    }

    #[test]
    fn group_rotate_uses_one_shared_pivot_for_owned_brushes() {
        let (mut workspace, _, inner, _) = grouped_workspace();
        workspace.project.active_scene_mut().brushes[1] =
            Brush::cuboid([256, 0, 0], [384, 128, 64]);
        workspace.replace_node_selection(inner);
        assert!(workspace.node_supports_transform_gizmo(inner, TransformGizmoMode::Rotate));
        let start = workspace.project.active_scene().brushes[1].clone();
        workspace.interaction = Interaction::NodeGizmo(NodeGizmoDrag {
            mode: TransformGizmoMode::Rotate,
            handle: NodeGizmoHandle::Axis(PrimitiveGizmoAxis::Y),
            start_pointer: Pos2::ZERO,
            screen_axis: Vec2::X,
            start_plane_hit: None,
            current_plane_delta_world: [0.0; 3],
            move_axis_world: [0.0; 3],
            rotate: None,
            targets: Vec::new(),
            group_brushes: vec![GroupBrushGizmoTarget { index: 1, start }],
            group_pivot: Some([320.0, 64.0, 32.0]),
            group_count: 1,
            current_steps: 90,
            snapshot_pushed: false,
            free: false,
        });

        workspace.apply_node_gizmo_drag();

        let solved = workspace.project.active_scene().brushes[1].solve();
        assert_eq!(solved.min, [288.0, 0.0, -32.0]);
        assert_eq!(solved.max, [352.0, 128.0, 96.0]);
    }

    #[test]
    fn group_rotate_resnaps_the_result_without_lowering_the_active_grid() {
        let (mut workspace, _, inner, _) = grouped_workspace();
        workspace.snap_units = 128;
        workspace.project.active_scene_mut().brushes[1] =
            Brush::cuboid([256, 0, 0], [384, 128, 64]);
        workspace.replace_node_selection(inner);
        let start = workspace.project.active_scene().brushes[1].clone();
        workspace.interaction = Interaction::NodeGizmo(NodeGizmoDrag {
            mode: TransformGizmoMode::Rotate,
            handle: NodeGizmoHandle::Axis(PrimitiveGizmoAxis::Y),
            start_pointer: Pos2::ZERO,
            screen_axis: Vec2::X,
            start_plane_hit: None,
            current_plane_delta_world: [0.0; 3],
            move_axis_world: [0.0; 3],
            rotate: None,
            targets: Vec::new(),
            group_brushes: vec![GroupBrushGizmoTarget {
                index: 1,
                start: start.clone(),
            }],
            group_pivot: Some([320.0, 64.0, 32.0]),
            group_count: 1,
            current_steps: 90,
            snapshot_pushed: false,
            free: false,
        });

        workspace.apply_node_gizmo_drag();

        let rotated = &workspace.project.active_scene().brushes[1];
        assert_ne!(rotated, &start);
        assert!(rotated.is_pickable());
        assert!(rotated.solved_vertices_on_grid(128, 0.01));
        assert_eq!(workspace.snap_units, 128);
        assert!(
            workspace
                .interaction
                .node_gizmo_drag()
                .expect("drag remains active")
                .snapshot_pushed,
            "an accepted preview creates one undo step"
        );
    }

    #[test]
    fn group_scale_uses_one_shared_pivot_for_owned_brushes() {
        let (mut workspace, _, inner, _) = grouped_workspace();
        workspace.project.active_scene_mut().brushes[1] =
            Brush::cuboid([256, 0, 0], [384, 128, 64]);
        workspace.replace_node_selection(inner);
        assert!(workspace.node_supports_transform_gizmo(inner, TransformGizmoMode::Scale));
        let start = workspace.project.active_scene().brushes[1].clone();
        workspace.interaction = Interaction::NodeGizmo(NodeGizmoDrag {
            mode: TransformGizmoMode::Scale,
            handle: NodeGizmoHandle::Axis(PrimitiveGizmoAxis::X),
            start_pointer: Pos2::ZERO,
            screen_axis: Vec2::X,
            start_plane_hit: None,
            current_plane_delta_world: [0.0; 3],
            move_axis_world: [0.0; 3],
            rotate: None,
            targets: Vec::new(),
            group_brushes: vec![GroupBrushGizmoTarget { index: 1, start }],
            group_pivot: Some([320.0, 64.0, 32.0]),
            group_count: 1,
            current_steps: 10,
            snapshot_pushed: false,
            free: false,
        });

        workspace.apply_node_gizmo_drag();

        let solved = workspace.project.active_scene().brushes[1].solve();
        assert_eq!(solved.min, [224.0, 0.0, 0.0]);
        assert_eq!(solved.max, [416.0, 128.0, 64.0]);
    }
}
