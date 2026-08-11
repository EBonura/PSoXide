use super::*;

impl EditorWorkspace {
    /// Build the camera ray in world units for the given pointer
    /// position, or `None` if the pointer's outside the viewport.
    /// Shared by `pick_3d_world` (ray vs. ground plane) and
    /// `pick_face_at` (ray vs. every face triangle in the active
    /// room) so both agree on every axis convention.
    pub(crate) fn camera_ray_for_pointer(
        &self,
        rect: egui::Rect,
        pointer: egui::Pos2,
    ) -> Option<([f32; 3], [f32; 3])> {
        if !rect.contains(pointer) {
            return None;
        }
        let nx = (pointer.x - rect.center().x) / (rect.width() * 0.5);
        let ny = (pointer.y - rect.center().y) / (rect.height() * 0.5);
        Some(
            self.viewport_3d_camera()
                .ray_for_normalized_panel_point(nx, ny),
        )
    }

    pub(crate) fn clear_validation_issues(&mut self) {
        self.validation_issue_primitives.clear();
        self.validation_issue_rooms.clear();
    }

    /// Select the concrete authoring object attached to a typed cook
    /// diagnostic. Returns `false` when the target is stale so the caller can
    /// fall back to the legacy world-grid diagnostic mapper.
    pub(crate) fn focus_playtest_validation_target(
        &mut self,
        target: psxed_project::playtest::PlaytestValidationTarget,
    ) -> bool {
        use psxed_project::playtest::PlaytestValidationTarget;

        match target {
            PlaytestValidationTarget::Brush { brush, face } => {
                let Some(authored) = self.project.active_scene().brushes.get(brush) else {
                    return false;
                };
                let face = face.filter(|face| *face < authored.faces.len());
                self.active_tool = ViewTool::Brush;
                self.replace_brush_selection(brush, face);
                self.clear_node_selection_state();
                self.clear_resource_selection_state();
                self.clear_sector_selection();
                self.clear_primitive_selection_state();
                self.frame_viewport();
                true
            }
            PlaytestValidationTarget::Node(node) => {
                if self.project.active_scene().node(node).is_none() {
                    return false;
                }
                self.clear_brush_selection();
                self.replace_node_selection(node);
                self.clear_resource_selection_state();
                self.clear_sector_selection();
                self.clear_primitive_selection_state();
                self.frame_viewport();
                true
            }
            PlaytestValidationTarget::Resource(resource) => {
                if self.project.resource(resource).is_none() {
                    return false;
                }
                self.clear_brush_selection();
                self.replace_resource_selection(resource);
                self.clear_node_selection_state();
                self.clear_sector_selection();
                self.clear_primitive_selection_state();
                true
            }
        }
    }

    pub(crate) fn record_first_playtest_world_cook_issue(&mut self, project: &ProjectDocument) {
        let scene = project.active_scene();
        let mut room_nodes: Vec<_> = scene
            .nodes()
            .iter()
            .filter(|node| matches!(node.kind, NodeKind::Section { .. }))
            .collect();
        room_nodes.sort_by_key(|node| node.id.raw());

        for room_node in room_nodes {
            let NodeKind::Section { grid } = &room_node.kind else {
                continue;
            };
            if grid.populated_sector_count() == 0 {
                continue;
            }
            let plan = plan_portal_rooms(scene, room_node.id, grid, PortalRoomConfig::default());
            for portal_room in plan.rooms {
                let chunk_grid = extract_portal_room_grid(grid, &portal_room);
                match world_cook::cook_world_grid(project, &chunk_grid) {
                    Ok(cooked) => {
                        if let Err(error) = cooked.to_psxw_bytes() {
                            self.record_world_cook_error(
                                room_node.id,
                                &error,
                                portal_room.array_origin,
                            );
                            return;
                        }
                    }
                    Err(error) => {
                        self.record_world_cook_error(
                            room_node.id,
                            &error,
                            portal_room.array_origin,
                        );
                        return;
                    }
                }
            }
        }
    }

    pub(crate) fn record_world_cook_error(
        &mut self,
        room: NodeId,
        error: &WorldGridCookError,
        array_origin: [u16; 2],
    ) {
        let mapped = world_cook_error_primitives(room, error, array_origin);
        if mapped.is_empty() {
            self.validation_issue_rooms.insert(room);
            self.replace_node_selection(room);
            self.clear_sector_selection();
            self.clear_primitive_selection_state();
            self.frame_viewport();
        } else {
            for selection in &mapped {
                if !self.validation_issue_primitives.contains(selection) {
                    self.validation_issue_primitives.push(*selection);
                }
            }
            self.select_and_frame_validation_issue_primitives(&mapped);
        }
    }

    pub(crate) fn select_and_frame_validation_issue_primitives(
        &mut self,
        selections: &[Selection],
    ) {
        let mut selected = Vec::new();
        for &selection in selections {
            if !selected.contains(&selection) {
                selected.push(selection);
            }
        }
        let Some(first) = selected.first().copied() else {
            return;
        };

        self.replace_node_selection(first.room());
        self.clear_sector_selection();
        self.selection.selected_primitives = selected;
        self.selection.selected_primitive = Some(first);
        self.update_primitive_resource_selection();
        self.frame_viewport();
    }

    /// Map a `(face, world-hit)` pair from `pick_face_with_hit`
    /// to a `Selection`, refining to an edge or vertex of the
    /// face when `selection_mode` demands one. Local-UV math
    /// happens here; the picker's heavy lifting (ray vs every
    /// face) was already paid above.
    pub(crate) fn pick_primitive_from_hit(&self, face: FaceRef, hit: [f32; 3]) -> Selection {
        let triangle = if matches!(self.horizontal_edit_mode, HorizontalEditMode::Triangle) {
            self.horizontal_triangle_ref_at_hit(face, hit)
        } else {
            None
        };
        match self.selection_mode {
            SelectionMode::Face => triangle
                .map(Selection::Triangle)
                .unwrap_or(Selection::Face(face)),
            SelectionMode::Edge => triangle
                .and_then(|triangle| self.triangle_edge_at_hit(triangle, hit))
                .or_else(|| self.face_edge_at_hit(face, hit))
                .map(Selection::Edge)
                .unwrap_or(Selection::Face(face)),
            SelectionMode::Vertex => triangle
                .and_then(|triangle| self.triangle_vertex_at_hit(triangle, hit))
                .or_else(|| self.face_vertex_at_hit(face, hit))
                .map(Selection::Vertex)
                .unwrap_or(Selection::Face(face)),
        }
    }

    pub(crate) fn horizontal_triangle_ref_at_hit(
        &self,
        face: FaceRef,
        hit: [f32; 3],
    ) -> Option<HorizontalTriangleRef> {
        let (split, dropped) = self.horizontal_face_split_and_drop(face)?;
        let grid = self.room_grid_view(face.room)?;
        let bounds = grid.cell_bounds_world(face.sx, face.sz);
        let local_x = hit[0] - bounds.x0 as f32;
        let local_z = hit[2] - bounds.z0 as f32;
        let mut index =
            horizontal_triangle_index_at_local(local_x, local_z, grid.sector_size, split);
        let mut corners = horizontal_triangle_corners(split, index);
        if dropped.is_some_and(|corner| corners.contains(&corner)) {
            index = horizontal_triangle_other(index);
            corners = horizontal_triangle_corners(split, index);
        }
        let surface = match face.kind {
            FaceKind::Floor => HorizontalSurfaceKind::Floor,
            FaceKind::Ceiling => HorizontalSurfaceKind::Ceiling,
            FaceKind::Wall { .. } => return None,
        };
        Some(HorizontalTriangleRef {
            room: face.room,
            sx: face.sx,
            sz: face.sz,
            surface,
            index,
            corners,
        })
    }

    pub(crate) fn horizontal_triangle_refs_for_face(
        &self,
        face: FaceRef,
    ) -> Vec<HorizontalTriangleRef> {
        let Some((split, dropped)) = self.horizontal_face_split_and_drop(face) else {
            return Vec::new();
        };
        let surface = match face.kind {
            FaceKind::Floor => HorizontalSurfaceKind::Floor,
            FaceKind::Ceiling => HorizontalSurfaceKind::Ceiling,
            FaceKind::Wall { .. } => return Vec::new(),
        };
        [HorizontalTriangleIndex::A, HorizontalTriangleIndex::B]
            .into_iter()
            .filter_map(|index| {
                let corners = horizontal_triangle_corners(split, index);
                if dropped.is_some_and(|corner| corners.contains(&corner)) {
                    return None;
                }
                Some(HorizontalTriangleRef {
                    room: face.room,
                    sx: face.sx,
                    sz: face.sz,
                    surface,
                    index,
                    corners,
                })
            })
            .collect()
    }

    pub(crate) fn horizontal_face_split_and_drop(
        &self,
        face: FaceRef,
    ) -> Option<(GridSplit, Option<Corner>)> {
        let grid = self.room_grid_view(face.room)?;
        let sector = grid.sector(face.sx, face.sz)?;
        match face.kind {
            FaceKind::Floor => sector.floor.as_ref().map(|f| (f.split, f.dropped_corner)),
            FaceKind::Ceiling => sector.ceiling.as_ref().map(|c| (c.split, c.dropped_corner)),
            FaceKind::Wall { .. } => None,
        }
    }

    /// Closest edge of `face` to the world-space hit. Computes
    /// distance to each of the four perimeter line segments in
    /// 3D (so sloped floors / non-rectangular walls still pick
    /// the right edge) and returns the smallest.
    pub(crate) fn face_edge_at_hit(&self, face: FaceRef, hit: [f32; 3]) -> Option<EdgeRef> {
        let corners = self.face_world_corners(face)?;
        let edge_idx = closest_edge_idx(&corners, hit);
        let anchor = match face.kind {
            FaceKind::Floor => EdgeAnchor::Floor {
                sx: face.sx,
                sz: face.sz,
                dir: floor_edge_dir(edge_idx),
            },
            FaceKind::Ceiling => EdgeAnchor::Ceiling {
                sx: face.sx,
                sz: face.sz,
                dir: floor_edge_dir(edge_idx),
            },
            FaceKind::Wall { dir, stack } => EdgeAnchor::Wall {
                sx: face.sx,
                sz: face.sz,
                dir,
                stack,
                edge: wall_edge_idx(edge_idx),
            },
        };
        Some(EdgeRef {
            room: face.room,
            anchor,
        })
    }

    pub(crate) fn triangle_edge_at_hit(
        &self,
        triangle: HorizontalTriangleRef,
        hit: [f32; 3],
    ) -> Option<EdgeRef> {
        let corners = self.triangle_world_corners(triangle)?;
        let edge_idx = closest_edge_idx(&corners, hit);
        let a = triangle.corners[edge_idx];
        let b = triangle.corners[(edge_idx + 1) % 3];
        let dir = horizontal_edge_dir_from_corners(a, b)?;
        let anchor = match triangle.surface {
            HorizontalSurfaceKind::Floor => EdgeAnchor::Floor {
                sx: triangle.sx,
                sz: triangle.sz,
                dir,
            },
            HorizontalSurfaceKind::Ceiling => EdgeAnchor::Ceiling {
                sx: triangle.sx,
                sz: triangle.sz,
                dir,
            },
        };
        Some(EdgeRef {
            room: triangle.room,
            anchor,
        })
    }

    /// Closest corner of `face` to the world-space hit. Distance
    /// computed in world space against the four corner points.
    pub(crate) fn face_vertex_at_hit(&self, face: FaceRef, hit: [f32; 3]) -> Option<VertexRef> {
        let corners = self.face_world_corners(face)?;
        let corner_idx = closest_corner_idx(&corners, hit);
        let anchor = match face.kind {
            FaceKind::Floor => VertexAnchor::Floor {
                sx: face.sx,
                sz: face.sz,
                corner: floor_corner_idx(corner_idx),
            },
            FaceKind::Ceiling => VertexAnchor::Ceiling {
                sx: face.sx,
                sz: face.sz,
                corner: floor_corner_idx(corner_idx),
            },
            FaceKind::Wall { dir, stack } => VertexAnchor::Wall {
                sx: face.sx,
                sz: face.sz,
                dir,
                stack,
                corner: wall_corner_idx(corner_idx),
            },
        };
        Some(VertexRef {
            room: face.room,
            anchor,
        })
    }

    pub(crate) fn triangle_vertex_at_hit(
        &self,
        triangle: HorizontalTriangleRef,
        hit: [f32; 3],
    ) -> Option<VertexRef> {
        let corners = self.triangle_world_corners(triangle)?;
        let corner_idx = closest_corner_idx(&corners, hit);
        let corner = triangle.corners[corner_idx];
        let anchor = match triangle.surface {
            HorizontalSurfaceKind::Floor => VertexAnchor::Floor {
                sx: triangle.sx,
                sz: triangle.sz,
                corner,
            },
            HorizontalSurfaceKind::Ceiling => VertexAnchor::Ceiling {
                sx: triangle.sx,
                sz: triangle.sz,
                corner,
            },
        };
        Some(VertexRef {
            room: triangle.room,
            anchor,
        })
    }

    pub(crate) fn triangle_world_corners(
        &self,
        triangle: HorizontalTriangleRef,
    ) -> Option<[[f32; 3]; 3]> {
        let grid = self.room_grid_view(triangle.room)?;
        if triangle.sx >= grid.width || triangle.sz >= grid.depth {
            return None;
        }
        let sector = grid.sector(triangle.sx, triangle.sz)?;
        let face = match triangle.surface {
            HorizontalSurfaceKind::Floor => sector.floor.as_ref()?,
            HorizontalSurfaceKind::Ceiling => sector.ceiling.as_ref()?,
        };
        let bounds = grid.cell_bounds_world(triangle.sx, triangle.sz);
        Some(horizontal_triangle_world_corners(
            bounds,
            triangle.corners,
            face.triangle_heights(triangle.index.idx()),
        ))
    }

    /// Four world-space corners of `face` in canonical
    /// perimeter order -- `[NW, NE, SE, SW]` for floors / ceilings,
    /// `[BL, BR, TR, TL]` for walls. Returns `None` if the face
    /// no longer exists (cell out of bounds, geometry missing).
    pub(crate) fn face_world_corners(&self, face: FaceRef) -> Option<[[f32; 3]; 4]> {
        let grid = self.room_grid_view(face.room)?;
        if face.sx >= grid.width || face.sz >= grid.depth {
            return None;
        }
        let sector = grid.sector(face.sx, face.sz)?;
        let bounds = grid.cell_bounds_world(face.sx, face.sz);
        match face.kind {
            FaceKind::Floor => sector
                .floor
                .as_ref()
                .map(|f| horizontal_face_world_corners(bounds, f.heights)),
            FaceKind::Ceiling => sector
                .ceiling
                .as_ref()
                .map(|c| horizontal_face_world_corners(bounds, c.heights)),
            FaceKind::Wall { dir, stack } => {
                let wall = sector.walls.get(dir).get(stack as usize)?;
                wall_face_world_corners(bounds, dir, wall.heights)
            }
        }
    }

    /// Walk every floor / ceiling / wall in the active Room and
    /// return the closest face the camera ray hits. Mirrors the
    /// triangle layout `editor_preview` emits so what the user sees
    /// matches what gets picked. `None` when the pointer is off the
    /// panel or no face is along the ray.
    /// Closest floor / wall / ceiling the camera ray intersects,
    /// along with the world-space hit point. Paint dispatch reads
    /// the hit point to infer which edge of a floor cell the user
    /// clicked when the wall paint tool is active.
    pub(crate) fn pick_face_with_hit(
        &self,
        rect: egui::Rect,
        pointer: egui::Pos2,
    ) -> Option<(FaceRef, [f32; 3])> {
        let (origin, dir) = self.camera_ray_for_pointer(rect, pointer)?;
        let scene = self.project.active_scene();
        let room = scene.nodes().iter().find(|node| {
            matches!(node.kind, NodeKind::Section { .. })
                && !self.scene_node_effectively_hidden(node.id)
        })?;
        let room_id = room.id;
        // Read the active floor's grid, not the floor 0 destructure, so a
        // ray pick on an upper floor tests that floor's faces (the render
        // shows the active floor in place, so the ray must match it).
        let grid = self.room_grid_view(room_id)?;
        let mut best: Option<(FaceRef, f32)> = None;
        let mut consider = |face: FaceRef, t: f32| {
            if !t.is_finite() || t <= 0.0 {
                return;
            }
            if best.is_none_or(|(_, bt)| t < bt) {
                best = Some((face, t));
            }
        };

        for sx in 0..grid.width {
            for sz in 0..grid.depth {
                let Some(sector) = grid.sector(sx, sz) else {
                    continue;
                };
                let bounds = grid.cell_bounds_world(sx, sz);

                if let Some(floor) = &sector.floor {
                    let sidedness = material_sidedness(&self.project, floor.material);
                    let face = FaceRef {
                        room: room_id,
                        sx,
                        sz,
                        kind: FaceKind::Floor,
                    };
                    for index in [HorizontalTriangleIndex::A, HorizontalTriangleIndex::B] {
                        let corners = horizontal_triangle_corners(floor.split, index);
                        if floor.dropped_corner.is_some_and(|d| corners.contains(&d)) {
                            continue;
                        }
                        let [a, b, c] = horizontal_triangle_world_corners(
                            bounds,
                            corners,
                            floor.triangle_heights(index.idx()),
                        );
                        if let Some(t) = ray_triangle_sided(origin, dir, a, c, b, sidedness) {
                            consider(face, t);
                        }
                    }
                }
                if let Some(ceiling) = &sector.ceiling {
                    let sidedness = material_sidedness(&self.project, ceiling.material);
                    let face = FaceRef {
                        room: room_id,
                        sx,
                        sz,
                        kind: FaceKind::Ceiling,
                    };
                    for index in [HorizontalTriangleIndex::A, HorizontalTriangleIndex::B] {
                        let corners = horizontal_triangle_corners(ceiling.split, index);
                        if ceiling.dropped_corner.is_some_and(|d| corners.contains(&d)) {
                            continue;
                        }
                        let [a, b, c] = horizontal_triangle_world_corners(
                            bounds,
                            corners,
                            ceiling.triangle_heights(index.idx()),
                        );
                        if let Some(t) = ray_triangle_sided(origin, dir, a, b, c, sidedness) {
                            consider(face, t);
                        }
                    }
                }
                for dir_card in GridDirection::ALL {
                    for (stack_idx, wall) in sector.walls.get(dir_card).iter().enumerate() {
                        let sidedness = material_sidedness(&self.project, wall.material);
                        if !wall_side_visible_from_camera(sidedness, bounds, dir_card, origin) {
                            continue;
                        }
                        let Some([bl, br, tr, tl]) =
                            wall_face_world_corners(bounds, dir_card, wall.heights)
                        else {
                            continue;
                        };
                        let face = FaceRef {
                            room: room_id,
                            sx,
                            sz,
                            kind: FaceKind::Wall {
                                dir: dir_card,
                                stack: stack_idx as u8,
                            },
                        };
                        for (a, b, c, members) in
                            wall_triangles(bl, br, tr, tl, wall.dropped_corner)
                        {
                            if wall.dropped_corner.is_some_and(|d| members.contains(&d)) {
                                continue;
                            }
                            if let Some(t) = ray_triangle(origin, dir, a, b, c) {
                                consider(face, t);
                            }
                        }
                    }
                }
            }
        }

        best.map(|(face, t)| {
            let hit = [
                origin[0] + dir[0] * t,
                origin[1] + dir[1] * t,
                origin[2] + dir[2] * t,
            ];
            (face, hit)
        })
    }

    /// Project a pointer position inside the 3D viewport panel onto
    /// the active Room's ground plane and return the editor's
    /// "1 unit = 1 sector" world coordinates the 2D click handler
    /// already speaks.
    pub(crate) fn pick_3d_world(&self, rect: egui::Rect, pointer: egui::Pos2) -> Option<[f32; 2]> {
        let scene = self.project.active_scene();
        let room = scene.nodes().iter().find(|node| {
            matches!(node.kind, NodeKind::Section { .. })
                && !self.scene_node_effectively_hidden(node.id)
        })?;
        self.pick_3d_world_on_room_plane(rect, pointer, room.id, 0.0)
    }

    pub(crate) fn pick_3d_paint_world(
        &self,
        rect: egui::Rect,
        pointer: egui::Pos2,
        room_id: NodeId,
    ) -> Option<[f32; 2]> {
        match self.active_tool {
            ViewTool::PaintCeiling => self.pick_3d_ceiling_world(rect, pointer, room_id),
            _ => self.pick_3d_world_on_room_plane(rect, pointer, room_id, 0.0),
        }
    }

    pub(crate) fn pick_3d_ceiling_world(
        &self,
        rect: egui::Rect,
        pointer: egui::Pos2,
        room_id: NodeId,
    ) -> Option<[f32; 2]> {
        let grid = self.room_grid_view(room_id)?;
        let (origin, dir) = self.camera_ray_for_pointer(rect, pointer)?;
        let mut plane_y = grid.sector_size.saturating_mul(DEFAULT_WALL_HEIGHT_SECTORS) as f32;
        let mut hit = ray_intersects_horizontal_plane(origin, dir, plane_y)?;

        for _ in 0..3 {
            let wcx = grid.world_x_to_cell(hit[0]);
            let wcz = grid.world_z_to_cell(hit[2]);
            let heights = grid.ceiling_heights_aligned_to_neighbors_for_world_cell(wcx, wcz);
            let next_plane_y =
                heights.iter().map(|height| *height as f32).sum::<f32>() / heights.len() as f32;
            if (next_plane_y - plane_y).abs() < 1.0 {
                break;
            }
            plane_y = next_plane_y;
            hit = ray_intersects_horizontal_plane(origin, dir, plane_y)?;
        }

        Some(grid.room_local_to_editor(hit))
    }

    pub(crate) fn pick_3d_world_on_room_plane(
        &self,
        rect: egui::Rect,
        pointer: egui::Pos2,
        room_id: NodeId,
        plane_y: f32,
    ) -> Option<[f32; 2]> {
        let grid = self.room_grid_view(room_id)?;
        let (origin, dir) = self.camera_ray_for_pointer(rect, pointer)?;
        let hit = ray_intersects_horizontal_plane(origin, dir, plane_y)?;
        // `WorldGrid::room_local_to_editor` is the canonical inverse
        // of `editor_to_room_local` and accounts for `origin`, so
        // picking stays correct after a negative-side grow.
        Some(grid.room_local_to_editor(hit))
    }

    /// Top-level keyboard shortcut handler. Cleared via `consume_*`
    /// so child widgets never see the same chord.
    pub(crate) fn handle_global_shortcuts(
        &mut self,
        ctx: &egui::Context,
        playtest_status: EditorPlaytestStatus,
    ) {
        let consume_save = consume_command_shortcut(ctx, egui::Key::S);
        let consume_new = consume_command_shortcut(ctx, egui::Key::N);
        let consume_build = consume_command_shortcut(ctx, egui::Key::B);
        let consume_play = consume_command_shortcut(ctx, egui::Key::Enter);
        let consume_redo = consume_command_shift_shortcut(ctx, egui::Key::Z);
        let consume_undo = consume_command_shortcut(ctx, egui::Key::Z);
        let focus_taken = ctx.memory(|m| m.focused().is_some());
        let consume_ui_copy = !focus_taken
            && self.active_workspace == WorkspaceView::Ui
            && consume_command_shortcut(ctx, egui::Key::C);
        let consume_ui_paste = !focus_taken
            && self.active_workspace == WorkspaceView::Ui
            && consume_command_shortcut(ctx, egui::Key::V);
        let consume_duplicate = if focus_taken || self.active_workspace == WorkspaceView::Ui {
            false
        } else {
            consume_command_shortcut(ctx, egui::Key::D)
        };
        if consume_save {
            self.save_project_from_ui();
        }
        if consume_new {
            self.open_new_project_dialog();
        }
        if consume_build {
            self.pending_playtest_request = Some(EditorPlaytestRequest::BuildProject);
        }
        if consume_play {
            self.request_play_or_rebuild(playtest_status);
        }
        if consume_redo {
            if self.floating_geometry.is_some() {
                self.cancel_floating_geometry();
            } else {
                self.do_redo();
            }
        } else if consume_undo {
            if self.floating_geometry.is_some() {
                self.cancel_floating_geometry();
            } else {
                self.do_undo();
            }
        }
        if !focus_taken
            && self.floating_geometry.is_none()
            && self.active_workspace != WorkspaceView::Ui
            && consume_command_shortcut(ctx, egui::Key::A)
        {
            self.select_all_current_scope();
        }
        if consume_duplicate {
            self.duplicate_current_selection();
        }
        if consume_ui_copy {
            self.copy_selected_ui_node();
        }
        if consume_ui_paste {
            self.paste_ui_node();
        }
        self.handle_toolbar_group_shortcuts(ctx);

        // F2 / Delete only fire when no widget owns focus -- so they
        // don't fight TextEdit content while the user is typing.
        let modifiers = ctx.input(|i| i.modifiers);
        if bare_shortcuts_available(focus_taken, modifiers) {
            if self.active_workspace == WorkspaceView::Ui {
                self.handle_ui_workspace_shortcuts(ctx, modifiers);
                return;
            }
            let rot = ctx.input_mut(|i| i.key_pressed(egui::Key::R));
            if rot && self.renaming.is_none() {
                if self.portal_place_active() {
                    self.rotate_portal_place_direction();
                } else {
                    self.rotate_current_selection_90();
                }
            }
            let flip = ctx.input_mut(|i| i.key_pressed(egui::Key::F));
            if flip && self.floating_geometry.is_some() {
                if modifiers.shift {
                    self.flip_floating_geometry_z();
                } else {
                    self.flip_floating_geometry_x();
                }
            }
            if self.floating_geometry.is_some() {
                // Heights are absolute, so a piece authored at ground level
                // has to be liftable onto a terrace before it is placed.
                let raise = ctx.input_mut(|i| i.key_pressed(egui::Key::PageUp));
                let lower = ctx.input_mut(|i| i.key_pressed(egui::Key::PageDown));
                if raise != lower {
                    self.nudge_floating_geometry_elevation(if raise { 1 } else { -1 });
                }
            }
            let escape = ctx.input_mut(|i| i.key_pressed(egui::Key::Escape));
            if escape && self.floating_geometry.is_some() {
                self.cancel_floating_geometry();
            }
            let frame = ctx.input_mut(|i| i.key_pressed(egui::Key::Period));
            if frame {
                match self.active_workspace {
                    WorkspaceView::Room => self.frame_viewport(),
                    WorkspaceView::Animation => {
                        self.animation_viewer.frame_preview();
                        self.status = "Framed animation preview".to_string();
                    }
                    // The UI workspace handles its own shortcuts above, and
                    // the Material workspace has no movable preview camera.
                    WorkspaceView::Ui | WorkspaceView::Material => {}
                }
            }
            if self.floating_geometry.is_some() {
                return;
            }
            let f2 = ctx.input_mut(|i| i.key_pressed(egui::Key::F2));
            if f2 && self.selection.selected_node != NodeId::ROOT {
                self.apply_tree_action(TreeAction::BeginRename(self.selection.selected_node), &[]);
            }
            let del = ctx.input_mut(|i| {
                i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)
            });
            if del && self.renaming.is_none() {
                if !self.selection.selected_sectors.is_empty() {
                    self.delete_selected_sectors();
                } else if !self.selected_primitive_targets().is_empty() {
                    self.delete_selected_primitives();
                } else if self.selection.selected_resource.is_some() {
                    self.begin_resource_delete_confirmation();
                } else if self.selection.selected_node != NodeId::ROOT {
                    self.apply_tree_action(TreeAction::Delete(self.selection.selected_node), &[]);
                }
            }
        }
    }

    pub(crate) fn handle_ui_workspace_shortcuts(
        &mut self,
        ctx: &egui::Context,
        modifiers: egui::Modifiers,
    ) -> bool {
        let step = if modifiers.shift { 8 } else { 1 };
        let delta = ctx.input_mut(|input| {
            let mut dx = 0;
            let mut dy = 0;
            if input.key_pressed(egui::Key::ArrowLeft) {
                dx -= step;
            }
            if input.key_pressed(egui::Key::ArrowRight) {
                dx += step;
            }
            if input.key_pressed(egui::Key::ArrowUp) {
                dy -= step;
            }
            if input.key_pressed(egui::Key::ArrowDown) {
                dy += step;
            }
            [dx, dy]
        });
        if delta != [0, 0] {
            self.nudge_selected_ui_node(delta[0], delta[1]);
            return true;
        }

        let del = ctx.input_mut(|input| {
            input.key_pressed(egui::Key::Delete) || input.key_pressed(egui::Key::Backspace)
        });
        if del {
            self.delete_selected_ui_node();
            return true;
        }
        false
    }

    pub(crate) fn handle_toolbar_group_shortcuts(&mut self, ctx: &egui::Context) {
        if let Some(reverse) = consume_command_cycle_shortcut(ctx, egui::Key::Num1) {
            self.cycle_workspace_group(reverse);
        }
        if self.active_workspace == WorkspaceView::Ui {
            if let Some(reverse) = consume_command_cycle_shortcut(ctx, egui::Key::Num2) {
                self.cycle_ui_transform_group(reverse);
            }
            return;
        }
        if self.active_workspace != WorkspaceView::Room {
            return;
        }
        if let Some(reverse) = consume_command_cycle_shortcut(ctx, egui::Key::Num2) {
            self.cycle_tool_group(reverse);
        }
        if let Some(reverse) = consume_command_cycle_shortcut(ctx, egui::Key::Num3) {
            self.cycle_transform_group(reverse);
        }
        if let Some(reverse) = consume_command_cycle_shortcut(ctx, egui::Key::Num4) {
            self.cycle_selection_group(reverse);
        }
        if let Some(reverse) = consume_command_cycle_shortcut(ctx, egui::Key::Num5) {
            self.cycle_horizontal_edit_group(reverse);
        }
        if let Some(reverse) = consume_command_cycle_shortcut(ctx, egui::Key::Num6) {
            self.cycle_vertex_connectivity_group(reverse);
        }
        if let Some(reverse) = consume_command_cycle_shortcut(ctx, egui::Key::Num7) {
            self.cycle_visibility_group(reverse);
        }
        if let Some(reverse) = consume_command_cycle_shortcut(ctx, egui::Key::Num8) {
            self.cycle_camera_group(reverse);
        }
        if let Some(reverse) = consume_command_cycle_shortcut(ctx, egui::Key::Num9) {
            self.cycle_view_dimension_group(reverse);
        }
    }

    pub(crate) fn cycle_workspace_group(&mut self, reverse: bool) {
        const VALUES: &[WorkspaceView] = &[
            WorkspaceView::Room,
            WorkspaceView::Ui,
            WorkspaceView::Animation,
            WorkspaceView::Material,
        ];
        self.active_workspace = cycle_value(VALUES, self.active_workspace, reverse);
        self.status = format!("Workspace: {}", self.active_workspace.label());
        self.mark_shortcut_group_changed(ShortcutGroup::Workspace);
    }

    pub(crate) fn cycle_tool_group(&mut self, reverse: bool) {
        const ALL_VALUES: &[(ViewTool, Option<PlaceKind>)] = &[
            (ViewTool::Select, None),
            (ViewTool::PaintMaterial, None),
            (ViewTool::Water, None),
            (ViewTool::PaintFloor, None),
            (ViewTool::PaintWall, None),
            (ViewTool::PaintCeiling, None),
            (ViewTool::Erase, None),
            (ViewTool::Place, Some(PlaceKind::PlayerSpawn)),
            (ViewTool::Place, Some(PlaceKind::SpawnMarker)),
            (ViewTool::Place, Some(PlaceKind::ModelInstance)),
            (ViewTool::Place, Some(PlaceKind::Character)),
            (ViewTool::Place, Some(PlaceKind::ImageProp)),
            (ViewTool::Place, Some(PlaceKind::BoxProp)),
            (ViewTool::Place, Some(PlaceKind::CylinderProp)),
            (ViewTool::Place, Some(PlaceKind::PointLightMarker)),
            (ViewTool::Place, Some(PlaceKind::ParticleEmitter)),
            (ViewTool::Place, Some(PlaceKind::Portal)),
        ];
        const BSP_VALUES: &[(ViewTool, Option<PlaceKind>)] = &[
            (ViewTool::Select, None),
            // Same cycle slot as the grid project's Paint, but it addresses
            // BSP brush faces instead of grid cells (`bsp_face_paint_active`).
            (ViewTool::PaintMaterial, None),
            (ViewTool::Brush, None),
            (ViewTool::Place, Some(PlaceKind::PlayerSpawn)),
            (ViewTool::Place, Some(PlaceKind::SpawnMarker)),
            (ViewTool::Place, Some(PlaceKind::ModelInstance)),
            (ViewTool::Place, Some(PlaceKind::Character)),
            (ViewTool::Place, Some(PlaceKind::ImageProp)),
            (ViewTool::Place, Some(PlaceKind::BoxProp)),
            (ViewTool::Place, Some(PlaceKind::CylinderProp)),
            (ViewTool::Place, Some(PlaceKind::ArchProp)),
            (ViewTool::Place, Some(PlaceKind::PointLightMarker)),
            (ViewTool::Place, Some(PlaceKind::ParticleEmitter)),
            (ViewTool::Place, Some(PlaceKind::Logic)),
        ];
        const BRUSH_ONLY: &[(ViewTool, Option<PlaceKind>)] =
            &[(ViewTool::Select, None), (ViewTool::Brush, None)];
        let values = if self.active_room_id().is_some() {
            ALL_VALUES
        } else if self.bsp_authoring_root().is_some() {
            BSP_VALUES
        } else {
            BRUSH_ONLY
        };
        let current = self.active_tool_cycle_value();
        let next = cycle_value(values, current, reverse);
        self.set_active_tool_cycle_value(next);
    }

    pub(crate) fn active_tool_cycle_value(&self) -> (ViewTool, Option<PlaceKind>) {
        if self.active_tool == ViewTool::Place {
            (ViewTool::Place, Some(self.place_kind))
        } else {
            (self.active_tool, None)
        }
    }

    pub(crate) fn set_active_tool_cycle_value(&mut self, value: (ViewTool, Option<PlaceKind>)) {
        let changed =
            self.active_tool_cycle_value() != value || self.active_workspace != WorkspaceView::Room;
        self.active_workspace = WorkspaceView::Room;
        let (tool, place_kind) = value;
        self.active_tool = tool;
        if tool != ViewTool::PaintMaterial {
            self.material_paint_sampling = false;
        }
        if let Some(place_kind) = place_kind {
            self.place_kind = place_kind;
        }
        if self.place_kind == PlaceKind::Portal {
            self.clear_sector_selection();
            self.clear_primitive_selection_state();
            self.selection.hovered_primitive = None;
        }
        if matches!(tool, ViewTool::PaintMaterial | ViewTool::Water) {
            self.clear_sector_selection();
            self.clear_primitive_selection_state();
            self.selection.hovered_primitive = None;
            if matches!(
                self.interaction,
                Interaction::PrimitiveGizmo(_) | Interaction::NodeGizmo(_) | Interaction::Node(_)
            ) {
                self.interaction = Interaction::Idle;
            }
            let material = self
                .selected_material_resource()
                .or(self.brush_material)
                .or_else(|| self.first_material());
            if let Some(material) = material {
                self.brush_material = Some(material);
                self.replace_resource_selection(material);
            }
        }
        self.status = if tool == ViewTool::Place {
            format!("Tool: {}", self.place_kind.label())
        } else {
            format!("Tool: {}", tool.label())
        };
        if changed {
            self.mark_shortcut_group_changed(ShortcutGroup::Tool);
        }
    }

    pub(crate) fn return_to_select_after_place(&mut self) {
        if self.active_tool == ViewTool::Place {
            self.active_tool = ViewTool::Select;
            self.mark_shortcut_group_changed(ShortcutGroup::Tool);
        }
    }

    pub(crate) fn cycle_transform_group(&mut self, reverse: bool) {
        const VALUES: &[TransformGizmoMode] = &[
            TransformGizmoMode::Move,
            TransformGizmoMode::Rotate,
            TransformGizmoMode::Scale,
        ];
        self.set_transform_gizmo_mode(cycle_value(VALUES, self.transform_gizmo_mode, reverse));
    }

    pub(crate) fn cycle_ui_transform_group(&mut self, reverse: bool) {
        const VALUES: &[UiTransformMode] = &[UiTransformMode::Move, UiTransformMode::Rotate];
        self.ui_transform_mode = cycle_value(VALUES, self.ui_transform_mode, reverse);
        self.status = format!("UI transform: {}", self.ui_transform_mode.label());
        self.mark_shortcut_group_changed(ShortcutGroup::Transform);
    }

    pub(crate) fn set_transform_gizmo_mode(&mut self, mode: TransformGizmoMode) {
        if self.transform_gizmo_mode == mode {
            return;
        }
        self.transform_gizmo_mode = mode;
        // Switching gizmo mode cancels an in-flight transform stroke
        // (gizmo / node-gizmo / node drag), but must leave an unrelated
        // marquee or UI-canvas stroke alone.
        if matches!(
            self.interaction,
            Interaction::PrimitiveGizmo(_) | Interaction::NodeGizmo(_) | Interaction::Node(_)
        ) {
            self.interaction = Interaction::Idle;
        }
        self.status = format!("Transform: {}", mode.label());
        self.mark_shortcut_group_changed(ShortcutGroup::Transform);
    }

    pub(crate) fn cycle_selection_group(&mut self, reverse: bool) {
        const VALUES: &[SelectionMode] = &[
            SelectionMode::Face,
            SelectionMode::Edge,
            SelectionMode::Vertex,
        ];
        self.set_selection_mode(cycle_value(VALUES, self.selection_mode, reverse));
    }

    pub(crate) fn cycle_horizontal_edit_group(&mut self, reverse: bool) {
        const VALUES: &[HorizontalEditMode] =
            &[HorizontalEditMode::Quad, HorizontalEditMode::Triangle];
        self.set_horizontal_edit_mode(cycle_value(VALUES, self.horizontal_edit_mode, reverse));
    }

    pub(crate) fn cycle_vertex_connectivity_group(&mut self, reverse: bool) {
        const VALUES: &[VertexConnectivity] =
            &[VertexConnectivity::Welded, VertexConnectivity::Detached];
        self.set_vertex_connectivity(cycle_value(VALUES, self.vertex_connectivity, reverse));
    }

    pub(crate) fn cycle_visibility_group(&mut self, reverse: bool) {
        let all_visible = !self.editor_visibility_has_hidden_items();
        let show_all = reverse || !all_visible;
        self.show_grid = show_all;
        self.show_portals = show_all;
        self.show_lights = show_all;
        self.preview_fog = show_all;
        self.preview_backface_wireframe = show_all;
        self.preview_bounds = show_all;
        self.persist_editor_visibility_state();
        self.status = if show_all {
            "Visibility: all shown".to_string()
        } else {
            "Visibility: all hidden".to_string()
        };
        self.mark_shortcut_group_changed(ShortcutGroup::Visibility);
    }

    pub(crate) fn cycle_camera_group(&mut self, reverse: bool) {
        const VALUES: &[ViewportCameraMode] =
            &[ViewportCameraMode::Orbit, ViewportCameraMode::Free];
        let mode = cycle_value(VALUES, self.camera_rig.mode, reverse);
        self.set_viewport_3d_camera_mode(mode);
        self.status = match mode {
            ViewportCameraMode::Orbit => "Camera: Orbit".to_string(),
            ViewportCameraMode::Free => "Camera: Free".to_string(),
        };
    }

    pub(crate) fn cycle_view_dimension_group(&mut self, reverse: bool) {
        let current = if self.view_2d {
            match self.orthographic_view {
                OrthographicView::Top => 1,
                OrthographicView::Front => 2,
                OrthographicView::Side => 3,
            }
        } else {
            0
        };
        let next = if reverse {
            (current + 3) % 4
        } else {
            (current + 1) % 4
        };
        match next {
            0 => {
                self.view_2d = false;
                self.status = "Viewport: 3D".to_string();
            }
            1 => self.set_orthographic_view(OrthographicView::Top),
            2 => self.set_orthographic_view(OrthographicView::Front),
            3 => self.set_orthographic_view(OrthographicView::Side),
            _ => unreachable!(),
        }
        self.mark_shortcut_group_changed(ShortcutGroup::Viewport);
    }

    pub(crate) fn set_orthographic_view(&mut self, view: OrthographicView) {
        let entering_orthographic = !self.view_2d;
        if entering_orthographic || self.orthographic_view != view {
            self.cancel_brush_gestures();
        }
        self.view_2d = true;
        self.orthographic_view = view;
        if entering_orthographic {
            self.frame_bsp_viewport_if_uninitialized();
        }
        self.status = format!("Viewport: {}", view.label());
        self.mark_shortcut_group_changed(ShortcutGroup::Viewport);
    }

    /// Switch the Select tool's primitive mode. Tries to adapt
    /// the existing selection to the new mode (a face → its NW
    /// corner, a vertex → its parent face) so the user doesn't
    /// lose their place. Falls back to clearing if the current
    /// selection has no natural counterpart.
    pub(crate) fn set_selection_mode(&mut self, mode: SelectionMode) {
        if self.selection_mode == mode {
            return;
        }
        self.selection_mode = mode;
        let active = self
            .selection
            .selected_primitive
            .and_then(|selection| Self::selection_as_mode(selection, mode));
        let mut converted = Vec::new();
        for selection in self.selected_primitive_targets() {
            let Some(selection) = Self::selection_as_mode(selection, mode) else {
                continue;
            };
            if !converted.contains(&selection) {
                converted.push(selection);
            }
        }
        self.selection.selected_primitives = converted;
        self.selection.selected_primitive =
            active.or_else(|| self.selection.selected_primitives.first().copied());
        // Clear the hover too -- its mode is the old one, and
        // the next mouse-move re-pick will repopulate under the
        // new mode anyway.
        self.selection.hovered_primitive = None;
        self.status = format!("Selection mode: {}", mode.label());
        self.mark_shortcut_group_changed(ShortcutGroup::Selection);
    }

    pub(crate) fn set_horizontal_edit_mode(&mut self, mode: HorizontalEditMode) {
        if self.horizontal_edit_mode == mode {
            return;
        }
        self.horizontal_edit_mode = mode;
        let active = self
            .selection
            .selected_primitive
            .and_then(|selection| self.selection_as_horizontal_mode(selection, mode));
        let mut converted = Vec::new();
        for selection in self.selected_primitive_targets() {
            let Some(selection) = self.selection_as_horizontal_mode(selection, mode) else {
                continue;
            };
            if !converted.contains(&selection) {
                converted.push(selection);
            }
        }
        self.selection.selected_primitives = converted;
        self.selection.selected_primitive =
            active.or_else(|| self.selection.selected_primitives.first().copied());
        self.selection.hovered_primitive = None;
        self.status = format!("Surface edit: {}", mode.label());
        self.mark_shortcut_group_changed(ShortcutGroup::Surface);
    }

    pub(crate) fn set_vertex_connectivity(&mut self, mode: VertexConnectivity) {
        if self.vertex_connectivity == mode {
            return;
        }
        self.vertex_connectivity = mode;
        self.status = format!("Vertex edits: {}", mode.label());
        self.mark_shortcut_group_changed(ShortcutGroup::Vertex);
    }

    pub(crate) fn selection_as_horizontal_mode(
        &self,
        selection: Selection,
        mode: HorizontalEditMode,
    ) -> Option<Selection> {
        match (selection, mode) {
            (Selection::Triangle(triangle), HorizontalEditMode::Quad) => {
                Some(Selection::Face(triangle.parent_face()))
            }
            (Selection::Face(face), HorizontalEditMode::Triangle)
                if matches!(face.kind, FaceKind::Floor | FaceKind::Ceiling) =>
            {
                self.horizontal_triangle_refs_for_face(face)
                    .first()
                    .copied()
                    .map(Selection::Triangle)
            }
            (selection, _) => Some(selection),
        }
    }

    pub(crate) fn selection_as_mode(
        selection: Selection,
        mode: SelectionMode,
    ) -> Option<Selection> {
        match (selection, mode) {
            (Selection::Face(face), SelectionMode::Face) => Some(Selection::Face(face)),
            (Selection::Face(face), SelectionMode::Edge) => {
                Some(Selection::Edge(face_first_edge(face)))
            }
            (Selection::Face(face), SelectionMode::Vertex) => {
                Some(Selection::Vertex(face_first_vertex(face)))
            }
            (Selection::Triangle(triangle), SelectionMode::Face) => {
                Some(Selection::Triangle(triangle))
            }
            (Selection::Triangle(triangle), SelectionMode::Edge) => {
                Some(Selection::Edge(triangle_first_edge(triangle)))
            }
            (Selection::Triangle(triangle), SelectionMode::Vertex) => {
                Some(Selection::Vertex(triangle_first_vertex(triangle)))
            }
            (Selection::Edge(edge), SelectionMode::Face) => {
                edge_owning_face_ref(edge).map(Selection::Face)
            }
            (Selection::Edge(edge), SelectionMode::Vertex) => {
                Some(Selection::Vertex(edge_first_vertex(edge)))
            }
            (Selection::Vertex(vertex), SelectionMode::Face) => {
                vertex_owning_face_ref(vertex).map(Selection::Face)
            }
            (Selection::Vertex(vertex), SelectionMode::Edge) => {
                Some(Selection::Edge(vertex_first_edge(vertex)))
            }
            (selection, mode) if Self::matches_mode(selection, mode) => Some(selection),
            _ => None,
        }
    }

    pub(crate) fn matches_mode(selection: Selection, mode: SelectionMode) -> bool {
        matches!(
            (selection, mode),
            (Selection::Face(_), SelectionMode::Face)
                | (Selection::Triangle(_), SelectionMode::Face)
                | (Selection::Edge(_), SelectionMode::Edge)
                | (Selection::Vertex(_), SelectionMode::Vertex)
        )
    }

    /// Snap the selected node's Y-rotation up by 90°. No-op on
    /// macro / structural nodes (World, Room, plain
    /// transform-only nodes) since they have no in-world heading.
    /// Entity hosts, the legacy `MeshInstance` card, and directional
    /// markers (spawn / portal) are rotatable.
    pub(crate) fn rotate_selected_yaw_90(&mut self) {
        let id = self.selection.selected_node;
        if id == NodeId::ROOT {
            return;
        }
        let scene = self.project.active_scene();
        let Some(node) = scene.node(id) else { return };
        let rotatable = matches!(
            node.kind,
            NodeKind::Entity
                | NodeKind::MeshInstance { .. }
                | NodeKind::ImageProp { .. }
                | NodeKind::BoxProp { .. }
                | NodeKind::CylinderProp { .. }
                | NodeKind::SpawnPoint { .. }
                | NodeKind::Portal { .. }
        );
        if !rotatable {
            return;
        }
        self.push_undo();
        if let Some(node) = self.project.active_scene_mut().node_mut(id) {
            let next = (node.transform.rotation_degrees[1] + 90.0) % 360.0;
            node.transform.rotation_degrees[1] = next;
            self.status = format!("Rotated {} to {}°", node.name, next as i32);
        }
        self.mark_dirty();
    }

    /// Apply one scene-tree action collected from a row.
    pub(crate) fn apply_tree_action(&mut self, action: TreeAction, visible_order: &[NodeId]) {
        match action {
            TreeAction::Select { id, modifiers } => {
                self.apply_node_selection_modifiers(id, modifiers, visible_order);
                self.renaming = None;
                // No-op when `id` isn't a Room -- keeps the camera
                // put while the user clicks through entity nodes.
                self.frame_3d_on_room(self.selection.selected_node);
                self.persist_editor_camera_state();
            }
            TreeAction::BeginRename(id) => {
                if let Some(node) = self.project.active_scene().node(id) {
                    let name = node.name.clone();
                    self.commit_node_selection(id);
                    self.renaming = Some((id, name));
                    self.pending_rename_focus = true;
                }
            }
            TreeAction::CommitRename(id, name) => {
                let trimmed = name.trim();
                let final_name = if trimmed.is_empty() {
                    self.project
                        .active_scene()
                        .node(id)
                        .map(|node| node.name.clone())
                        .unwrap_or_default()
                } else {
                    trimmed.to_string()
                };
                let original = self
                    .project
                    .active_scene()
                    .node(id)
                    .map(|node| node.name.clone());
                if original.as_deref() != Some(final_name.as_str()) {
                    self.push_undo();
                    if let Some(node) = self.project.active_scene_mut().node_mut(id) {
                        node.name = final_name.clone();
                    }
                    self.status = format!("Renamed {final_name}");
                    self.mark_dirty();
                }
                self.renaming = None;
            }
            TreeAction::CancelRename => {
                self.renaming = None;
            }
            TreeAction::Delete(id) => {
                if !self.node_is_selected(id) {
                    self.replace_node_selection(id);
                }
                self.delete_selected();
                self.renaming = None;
            }
            TreeAction::Duplicate(id) => {
                if !self.node_is_selected(id) {
                    self.replace_node_selection(id);
                }
                self.duplicate_selected();
                self.renaming = None;
            }
            TreeAction::AddChild { parent, kind, name } => {
                self.replace_node_selection(parent);
                self.add_child(kind, name);
            }
            TreeAction::ToggleExpanded(id) => {
                if self.collapsed_scene_nodes.remove(&id) {
                    self.status = self
                        .project
                        .active_scene()
                        .node(id)
                        .map(|node| format!("Expanded {}", node.name))
                        .unwrap_or_else(|| "Expanded node".to_string());
                } else {
                    self.collapsed_scene_nodes.insert(id);
                    self.status = self
                        .project
                        .active_scene()
                        .node(id)
                        .map(|node| format!("Collapsed {}", node.name))
                        .unwrap_or_else(|| "Collapsed node".to_string());
                }
                self.renaming = None;
            }
            TreeAction::ToggleVisibility(id) => {
                if self.hidden_scene_nodes.remove(&id) {
                    self.status = self
                        .project
                        .active_scene()
                        .node(id)
                        .map(|node| format!("Showing {}", node.name))
                        .unwrap_or_else(|| "Showing node".to_string());
                } else {
                    self.hidden_scene_nodes.insert(id);
                    self.status = self
                        .project
                        .active_scene()
                        .node(id)
                        .map(|node| format!("Hiding {}", node.name))
                        .unwrap_or_else(|| "Hiding node".to_string());
                }
                if self.selection.hovered_entity_node == Some(id) {
                    self.selection.hovered_entity_node = None;
                }
                self.renaming = None;
            }
            TreeAction::Reparent {
                source,
                target_parent,
                position,
            } => {
                let sources = self.scene_tree_drag_sources(source);
                if sources.is_empty() {
                    return;
                }
                if !self.scene_tree_reparent_is_valid(&sources, target_parent) {
                    self.status = "Cannot reparent: would create a cycle".to_string();
                    return;
                }
                self.push_undo();
                let moved = move_scene_nodes_as_group(
                    self.project.active_scene_mut(),
                    &sources,
                    target_parent,
                    position,
                );
                if moved > 0 {
                    if moved == 1 {
                        self.replace_node_selection(sources[0]);
                    } else {
                        self.selection.selected_node = if sources.contains(&source) {
                            source
                        } else {
                            sources[0]
                        };
                        self.selection.selected_nodes = sources.iter().copied().collect();
                        self.selection.node_selection_anchor = Some(self.selection.selected_node);
                    }
                    self.clear_resource_selection_state();
                    self.clear_primitive_selection_state();
                    self.clear_sector_selection();
                    self.status = if moved == 1 {
                        "Moved node".to_string()
                    } else {
                        format!("Moved {moved} nodes")
                    };
                    self.mark_dirty();
                }
            }
        }
    }

    pub(crate) fn apply_ui_tree_action(&mut self, action: UiTreeAction) {
        match action {
            UiTreeAction::Select(id) => {
                self.select_ui_node(id);
            }
            UiTreeAction::Copy(id) => {
                self.copy_ui_node(id);
            }
            UiTreeAction::PasteInto(id) => {
                self.paste_ui_node_under(id);
            }
            UiTreeAction::Delete(id) => {
                self.selection.selected_ui_node = id;
                self.delete_selected_ui_node();
            }
            UiTreeAction::ToggleVisibility(id) => {
                let Some((scene_id, node_name)) = self
                    .current_ui_scene()
                    .map(|scene| (scene.id, scene.node(id).map(|node| node.name.clone())))
                else {
                    return;
                };
                let key = (scene_id, id);
                if self.hidden_ui_nodes.remove(&key) {
                    self.status = node_name
                        .map(|name| format!("Showing UI {name}"))
                        .unwrap_or_else(|| "Showing UI node".to_string());
                } else {
                    self.hidden_ui_nodes.insert(key);
                    self.status = node_name
                        .map(|name| format!("Hiding UI {name}"))
                        .unwrap_or_else(|| "Hiding UI node".to_string());
                }
                self.interaction.take_ui_canvas_drag();
            }
            UiTreeAction::AddChild { parent, kind, name } => {
                self.selection.selected_ui_node = parent;
                self.add_ui_child(kind, name);
            }
            UiTreeAction::Reparent {
                source,
                target_parent,
                position,
            } => {
                if source == target_parent {
                    return;
                }
                let Some(scene) = self.current_ui_scene() else {
                    return;
                };
                if scene.is_descendant_of(target_parent, source) {
                    self.status = "Cannot reparent UI node: would create a cycle".to_string();
                    return;
                }
                self.push_undo();
                let Some(scene) = self.current_ui_scene_mut() else {
                    return;
                };
                if scene.move_node(source, target_parent, position) {
                    self.select_ui_node(source);
                    self.interaction.take_ui_canvas_drag();
                    self.status = "Moved UI node".to_string();
                    self.mark_dirty();
                }
            }
        }
    }

    /// Set the project's boot target and mark it dirty so the next cook (and
    /// the saved project) picks it up.
    pub(crate) fn set_boot_target(&mut self, target: BootTarget) {
        if self.project.boot != target {
            self.project.boot = target;
            self.mark_dirty();
        }
    }

    /// Human-readable name of the current boot target for tooltips: "Gameplay"
    /// or the bound UI scene's name (falling back to "Gameplay" if the bound
    /// scene was deleted).
    pub(crate) fn boot_target_label(&self) -> String {
        match self.project.boot {
            BootTarget::Gameplay => "Gameplay".to_string(),
            BootTarget::SceneState(state_id) => self
                .project
                .scene_states
                .iter()
                .find(|state| state.id == state_id)
                .map(|state| state.name.clone())
                .unwrap_or_else(|| "Gameplay".to_string()),
            BootTarget::UiScene(scene_id) => self
                .project
                .ui_scenes
                .iter()
                .find(|scene| scene.id == scene_id)
                .map(|scene| scene.name.clone())
                .unwrap_or_else(|| "Gameplay".to_string()),
        }
    }

    pub(crate) fn request_play_or_rebuild(&mut self, playtest_status: EditorPlaytestStatus) {
        self.resources_open = true;
        self.content_browser_view = ContentBrowserView::Debug;
        self.pending_playtest_request = Some(if playtest_status.is_active() {
            EditorPlaytestRequest::Rebuild
        } else {
            EditorPlaytestRequest::Play
        });
    }

    pub(crate) fn draw_action_bar(
        &mut self,
        ctx: &egui::Context,
        playtest_status: EditorPlaytestStatus,
        play_metrics: Option<EditorPlaytestMetrics>,
    ) {
        let action_bar_height = action_bar_height_for_status(&self.status);
        let status_strip_height = (action_bar_height - 12.0).max(38.0);
        egui::TopBottomPanel::top("psxed_action_bar")
            .exact_height(action_bar_height)
            .frame(top_bar_frame())
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    self.draw_project_identity(ui);
                    ui.add_space(6.0);
                    self.draw_main_menus(ctx, ui, playtest_status);
                    ui.add_space(8.0);
                    self.draw_build_play_controls(ui, playtest_status, play_metrics);
                    let remaining = ui.available_width();
                    if remaining >= 80.0 {
                        ui.add_space(8.0);
                        ui.allocate_ui_with_layout(
                            Vec2::new(ui.available_width().max(1.0), status_strip_height),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                self.draw_build_status_strip(ui, playtest_status);
                            },
                        );
                    }
                });
            });
    }

    pub(crate) fn draw_project_identity(&mut self, ui: &mut egui::Ui) {
        let logo_texture = self.psoxide_logo_texture_id(ui.ctx());
        egui::Frame::new()
            .fill(Color32::TRANSPARENT)
            .corner_radius(egui::CornerRadius::same(6))
            .inner_margin(egui::Margin::symmetric(2, 2))
            .show(ui, |ui| {
                if let Some(texture_id) = logo_texture {
                    ui.add(egui::Image::new((texture_id, Vec2::splat(28.0))));
                } else {
                    ui.label(icons::text(icons::BOX, 18.0).color(STUDIO_ACCENT));
                }
            })
            .response
            .on_hover_text("PSoXide");

        ui.add_space(2.0);

        if self.project_name_editing {
            let edit_id = ui.id().with("project_name_inline_edit");
            let response = ui.add_sized(
                Vec2::new(154.0, 23.0),
                egui::TextEdit::singleline(&mut self.project.name)
                    .hint_text("Project name")
                    .font(egui::TextStyle::Button)
                    .id(edit_id),
            );
            if self.project_name_focus_pending {
                response.request_focus();
                self.project_name_focus_pending = false;
            }
            if response.changed() {
                self.mark_dirty();
            }
            let finish_edit = response.lost_focus()
                || (response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)));
            if finish_edit {
                self.project_name_editing = false;
            }
            if self.dirty {
                ui.label(RichText::new("*").strong().color(STUDIO_ACCENT));
            }
            return;
        }

        let display_name = if self.project.name.trim().is_empty() {
            "Untitled"
        } else {
            self.project.name.trim()
        };
        ui.label(RichText::new(display_name).strong().size(15.0));
        if self.dirty {
            ui.label(RichText::new("*").strong().color(STUDIO_ACCENT));
        }
        if ui
            .add_sized(
                Vec2::new(24.0, 23.0),
                egui::Button::new(icons::text(icons::PEN_LINE, 13.0)),
            )
            .on_hover_text("Edit project name")
            .clicked()
        {
            self.project_name_editing = true;
            self.project_name_focus_pending = true;
        }
    }

    pub(crate) fn psoxide_logo_texture_id(
        &mut self,
        ctx: &egui::Context,
    ) -> Option<egui::TextureId> {
        if self.psoxide_logo_texture.is_none() {
            let logo = decode_embedded_png(PSOXIDE_APP_ICON_PNG)?;
            self.psoxide_logo_texture =
                Some(ctx.load_texture("psoxide-app-icon", logo, egui::TextureOptions::LINEAR));
        }
        self.psoxide_logo_texture.as_ref().map(|handle| handle.id())
    }

    /// Lazily rasterize the on-device UI bitmap fonts into egui textures and
    /// return their handles. Uses NEAREST sampling so the crisp source pixels
    /// survive any preview scale.
    pub(crate) fn ui_font_textures(&mut self, ctx: &egui::Context) -> Vec<egui::TextureHandle> {
        UI_FONT_CHOICES
            .iter()
            .copied()
            .map(|font| self.ui_font_texture(ctx, font))
            .collect()
    }

    pub(crate) fn ui_font_texture(
        &mut self,
        ctx: &egui::Context,
        font: UiFontChoice,
    ) -> egui::TextureHandle {
        let spec = ui_preview_font_spec(font);
        if self.ui_font_textures.len() < UI_FONT_COUNT {
            self.ui_font_textures.resize_with(UI_FONT_COUNT, || None);
        }
        self.ui_font_textures[spec.texture_index]
            .get_or_insert_with(|| {
                ctx.load_texture(
                    format!("psx-ui-font-{}", font.slug()),
                    rasterize_ui_font_atlas(font),
                    egui::TextureOptions::NEAREST,
                )
            })
            .clone()
    }

    pub(crate) fn draw_main_menus(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        playtest_status: EditorPlaytestStatus,
    ) {
        ui.menu_button("File", |ui| {
            if ui
                .button(menu_label("New Project...", &command_shortcut_text("N")))
                .clicked()
            {
                self.open_new_project_dialog();
                ui.close_menu();
            }
            ui.menu_button(icons::label(icons::FOLDER, "Project"), |ui| {
                self.draw_project_switch_menu(ui);
            });
            let can_delete_project = !self.current_project_is_bundled();
            let delete_response =
                ui.add_enabled(can_delete_project, egui::Button::new("Delete Project..."));
            let delete_clicked = delete_response.clicked();
            if !can_delete_project {
                delete_response.on_hover_text("Bundled starter projects cannot be deleted");
            }
            if delete_clicked {
                self.modal = Modal::DeleteProject { error: None };
                ui.close_menu();
            }
            ui.separator();
            if ui
                .button(menu_label("Save", &command_shortcut_text("S")))
                .clicked()
            {
                self.save_project_from_ui();
                ui.close_menu();
            }
            if ui.button("Reload").clicked() {
                self.reload();
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Quit").clicked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
        ui.menu_button("Edit", |ui| {
            let can_node_delete = self.selection.selected_node != NodeId::ROOT;
            let has_geometry_selection = self.has_geometry_selection();
            if ui
                .button(menu_label(
                    "Duplicate Selection",
                    &command_shortcut_text("D"),
                ))
                .clicked()
            {
                self.duplicate_current_selection();
                ui.close_menu();
            }
            ui.separator();
            if ui
                .add_enabled(
                    has_geometry_selection,
                    egui::Button::new("Rotate World Geometry 90°"),
                )
                .clicked()
            {
                self.rotate_current_selection_90();
                ui.close_menu();
            }
            ui.separator();
            if ui
                .add_enabled(can_node_delete, egui::Button::new("Delete Selection"))
                .clicked()
            {
                self.delete_selected();
                ui.close_menu();
            }
        });
        ui.menu_button("View", |ui| {
            if ui
                .checkbox(&mut self.left_dock_open, "World and files")
                .clicked()
            {
                ui.close_menu();
            }
            if ui.checkbox(&mut self.resources_open, "Resources").clicked() {
                ui.close_menu();
            }
            if ui.checkbox(&mut self.inspector_open, "Inspector").clicked() {
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Frame Selection").clicked() {
                self.frame_viewport();
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Room Workspace").clicked() {
                self.active_workspace = WorkspaceView::Room;
                ui.close_menu();
            }
            if ui.button("UI Workspace").clicked() {
                self.active_workspace = WorkspaceView::Ui;
                ui.close_menu();
            }
            if ui.button("Animation Viewer").clicked() {
                self.open_animation_viewer_for_current_selection();
                ui.close_menu();
            }
            if ui.button("Material Lab").clicked() {
                self.active_workspace = WorkspaceView::Material;
                ui.close_menu();
            }
        });
        ui.menu_button("Tools", |ui| {
            if ui
                .button("Remove Duplicate Walls")
                .on_hover_text(
                    "Delete every wall segment that is byte-identical to another on the same \
                     edge. Two of them sit in one plane, so the second can never be seen and \
                     still costs a full room surface to draw. Undoable.",
                )
                .clicked()
            {
                self.remove_duplicate_walls();
                ui.close_menu();
            }
            ui.menu_button("Prefabs", |ui| self.draw_prefab_menu(ui));
            ui.separator();
            if ui.button("Build Project").clicked() {
                self.pending_playtest_request = Some(EditorPlaytestRequest::BuildProject);
                ui.close_menu();
            }
            let play_label = if playtest_status.is_active() {
                "Rebuild and Play"
            } else {
                "Play"
            };
            if ui.button(play_label).clicked() {
                self.request_play_or_rebuild(playtest_status);
                ui.close_menu();
            }
            if playtest_status.is_active() {
                ui.separator();
                if ui.button("Stop Embedded Play").clicked() {
                    self.pending_playtest_request = Some(EditorPlaytestRequest::Stop);
                    ui.close_menu();
                }
            }
        });
        ui.menu_button("Help", |ui| {
            ui.label(RichText::new("PSoXide Editor").strong());
            ui.weak("Build cooks assets and compiles the PS1 runtime.");
            ui.weak("Play builds and runs inside the viewport.");
        });
    }

    pub(crate) fn draw_build_play_controls(
        &mut self,
        ui: &mut egui::Ui,
        playtest_status: EditorPlaytestStatus,
        play_metrics: Option<EditorPlaytestMetrics>,
    ) {
        if ui
            .button(icons::label(icons::BOX, "Build"))
            .on_hover_text(format!(
                "Cook assets, build the runtime EXE, and export it into the launcher Projects list. Shortcut: {}.",
                command_shortcut_text("B")
            ))
            .clicked()
        {
            self.pending_playtest_request = Some(EditorPlaytestRequest::BuildProject);
        }

        let playtest_active = playtest_status.is_active();
        let play_label = if playtest_active {
            "Rebuild & Play"
        } else {
            "Play"
        };
        // Play is a split button: the main face runs the project; the attached
        // chevron opens persistent boot/cook settings plus the latest host-side
        // PSX envelope, without starting the game. Zero item spacing inside
        // this group makes the two faces read as one control.
        let mut set_boot: Option<BootTarget> = None;
        let mut set_cook_mode = None;
        let mut focus_budget_target = None;
        let current_boot = self.project.boot;
        let current_cook_mode = self.project.bsp_cook_mode;
        let boot_label = self.boot_target_label();
        let budget = self.last_playtest_budget.clone().unwrap_or_else(|| {
            psxed_project::playtest::estimate_playtest_budgets(&self.project, &self.project_dir)
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            if ui
                .button(icons::label(icons::PLAY, play_label))
                .on_hover_text(format!(
                    "{} BSP cook, build, and run inside the 3D viewport. Boots at: {}. Shortcut: {}.",
                    current_cook_mode.label(),
                    boot_label,
                    command_shortcut_text("Enter")
                ))
                .clicked()
            {
                self.request_play_or_rebuild(playtest_status);
            }
            ui.menu_button(icons::text(icons::CHEVRON_DOWN, 12.0), |ui| {
                ui.label(RichText::new("Boot at").color(STUDIO_TEXT_WEAK).small());
                if ui
                    .selectable_label(current_boot == BootTarget::Gameplay, "Gameplay")
                    .clicked()
                {
                    set_boot = Some(BootTarget::Gameplay);
                    ui.close_menu();
                }
                ui.separator();
                ui.label(RichText::new("Screen States").color(STUDIO_TEXT_WEAK).small());
                for state in &self.project.scene_states {
                    let target = BootTarget::SceneState(state.id);
                    if ui
                        .selectable_label(current_boot == target, &state.name)
                        .clicked()
                    {
                        set_boot = Some(target);
                        ui.close_menu();
                    }
                }
                ui.separator();
                ui.label(RichText::new("UI Scenes").color(STUDIO_TEXT_WEAK).small());
                for scene in &self.project.ui_scenes {
                    let target = BootTarget::UiScene(scene.id);
                    if ui
                        .selectable_label(current_boot == target, &scene.name)
                        .clicked()
                    {
                        set_boot = Some(target);
                        ui.close_menu();
                    }
                }
                ui.separator();
                ui.label(RichText::new("BSP cook").color(STUDIO_TEXT_WEAK).small());
                for mode in psxed_project::brush_world::BrushWorldCookMode::ALL {
                    if ui
                        .selectable_label(current_cook_mode == mode, mode.label())
                        .on_hover_text(mode.description())
                        .clicked()
                    {
                        set_cook_mode = Some(mode);
                    }
                }
                ui.label(
                    RichText::new(current_cook_mode.description())
                        .color(STUDIO_TEXT_WEAK)
                        .small(),
                );
                ui.separator();
                let stage = match budget.stage {
                    psxed_project::playtest::PlaytestBudgetStage::AuthoredEstimate => {
                        "PSX budget estimate (before cook)"
                    }
                    psxed_project::playtest::PlaytestBudgetStage::Cooked => {
                        "PSX budget (last cook)"
                    }
                };
                ui.label(RichText::new(stage).color(STUDIO_TEXT_WEAK).small());
                ui.monospace(format!("BSP       {}", human_bytes_u64(budget.bsp_bytes as u64)));
                ui.monospace(format!(
                    "PVS       {} · row {}/{} B",
                    human_bytes_u64(budget.pvs_bytes as u64),
                    budget.pvs_row_bytes,
                    psxed_project::playtest::PLAYTEST_PVS_ROW_LIMIT_BYTES,
                ));
                ui.monospace(format!(
                    "Lighting  {}",
                    human_bytes_u64(budget.light_bytes as u64)
                ));
                ui.monospace(format!(
                    "Textures  {}",
                    human_bytes_u64(budget.texture_bytes as u64)
                ));
                ui.monospace(format!(
                    "RAM       {}/{} · {}/{} slots",
                    human_bytes_u64(budget.ram_bytes as u64),
                    human_bytes_u64(
                        psxed_project::playtest::PLAYTEST_RAM_PHYSICAL_BYTES as u64
                    ),
                    budget.ram_asset_slots,
                    psxed_project::playtest::PLAYTEST_RAM_ASSET_SLOT_LIMIT,
                ));
                ui.monospace(format!(
                    "VRAM      {}/{} · {}/{} slots",
                    human_bytes_u64(budget.vram_bytes as u64),
                    human_bytes_u64(
                        psxed_project::playtest::PLAYTEST_VRAM_PHYSICAL_BYTES as u64
                    ),
                    budget.vram_asset_slots,
                    psxed_project::playtest::PLAYTEST_VRAM_ASSET_SLOT_LIMIT,
                ));
                ui.monospace(format!(
                    "Packets   {}/{}",
                    budget.packet_count, budget.packet_limit,
                ));
                if let Some(issue) = budget.first_actionable_issue() {
                    ui.separator();
                    ui.label(RichText::new(issue.message()).color(STUDIO_ERROR));
                    if let Some(target) = issue.target {
                        if ui.button("Focus budget offender").clicked() {
                            focus_budget_target = Some(target);
                            ui.close_menu();
                        }
                    }
                } else {
                    ui.weak("No host envelope exceeded; MIPS link/replay remain final gates.");
                }
            })
            .response
            .on_hover_text("Choose boot target and persistent BSP cook quality; inspect pre/post-cook PSX envelopes without starting Play.");
        });
        if let Some(target) = set_boot {
            self.set_boot_target(target);
        }
        if let Some(mode) = set_cook_mode {
            self.set_bsp_cook_mode(mode);
        }
        if let Some(target) = focus_budget_target {
            let _ = self.focus_playtest_validation_target(target);
        }

        if playtest_active
            && ui
                .button(icons::label(icons::TRASH, "Stop"))
                .on_hover_text("Stop embedded play mode and return the viewport to editing.")
                .clicked()
        {
            self.pending_playtest_request = Some(EditorPlaytestRequest::Stop);
        }

        if ui
            .button(icons::label(icons::FOCUS, "Debug"))
            .on_hover_text("Append camera, player, room, and portal visibility diagnostics to logs/editor_debug.log.")
            .clicked()
        {
            self.capture_debug_snapshot(play_metrics);
        }
    }

    pub(crate) fn capture_debug_snapshot(&mut self, play_metrics: Option<EditorPlaytestMetrics>) {
        let path = self.debug_log_path();
        match self.write_debug_snapshot(&path, play_metrics) {
            Ok(()) => {
                let label = path
                    .strip_prefix(&self.project_dir)
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|_| path.display().to_string());
                self.status = format!("Debug snapshot appended to {label}");
            }
            Err(error) => {
                self.status = format!("Debug snapshot failed: {error}");
            }
        }
    }

    pub(crate) fn debug_log_path(&self) -> PathBuf {
        self.project_dir.join("logs").join("editor_debug.log")
    }

    pub(crate) fn write_debug_snapshot(
        &self,
        path: &Path,
        play_metrics: Option<EditorPlaytestMetrics>,
    ) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("create {}: {error}", parent.display()))?;
        }
        let mut text = String::new();
        self.append_debug_snapshot_text(&mut text, play_metrics);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|error| format!("open {}: {error}", path.display()))?;
        std::io::Write::write_all(&mut file, text.as_bytes())
            .map_err(|error| format!("write {}: {error}", path.display()))?;
        Ok(())
    }

    pub(crate) fn append_debug_snapshot_text(
        &self,
        out: &mut String,
        play_metrics: Option<EditorPlaytestMetrics>,
    ) {
        let unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        let editor_camera = self.viewport_3d_camera();
        let editor_basis = editor_camera.basis();
        let editor_position = editor_camera.position_i32();
        let _ = writeln!(out, "---- PSoXide editor debug snapshot ----");
        let _ = writeln!(out, "unix_ms: {unix_ms}");
        let _ = writeln!(out, "project: {}", self.project.name);
        let _ = writeln!(out, "project_dir: {}", self.project_dir.display());
        let _ = writeln!(out, "status: {}", self.status);
        let _ = writeln!(
            out,
            "editor_camera: mode={:?} pos={:?} target={:?} yaw_q12={} yaw_deg={:.2} pitch_q12={} pitch_deg={:.2} radius={}",
            editor_camera.mode,
            editor_position,
            editor_camera.target,
            editor_camera.yaw_q12,
            q12_degrees(editor_camera.yaw_q12),
            editor_camera.pitch_q12,
            q12_degrees(editor_camera.pitch_q12),
            editor_camera.radius
        );
        let _ = writeln!(
            out,
            "editor_camera_basis: forward={:?} right={:?} up={:?}",
            editor_basis.forward, editor_basis.right, editor_basis.up
        );
        match play_metrics {
            Some(metrics) => self.append_play_metrics_debug_snapshot(out, metrics),
            None => {
                let _ = writeln!(out, "play_metrics: unavailable");
            }
        }
        let _ = writeln!(out);
    }

    pub(crate) fn append_play_metrics_debug_snapshot(
        &self,
        out: &mut String,
        metrics: EditorPlaytestMetrics,
    ) {
        let _ = writeln!(
            out,
            "runtime_player: valid={} room_index={} local=({}, {}) yaw_q12={} yaw_deg={:.2}",
            metrics.player_map_valid,
            metrics.player_room_index,
            metrics.player_local_x,
            metrics.player_local_z,
            metrics.player_view_yaw_q12,
            q12_degrees(metrics.player_view_yaw_q12)
        );
        let camera_forward = if metrics.camera_view_basis_valid {
            let x = -(metrics.camera_view_sin_yaw_q12 as f32) / 4096.0;
            let z = -(metrics.camera_view_cos_yaw_q12 as f32) / 4096.0;
            Some([x, z])
        } else {
            None
        };
        let _ = writeln!(
            out,
            "runtime_camera: map_valid={} local=({}, {}, {}) global_valid={} global=({}, {}, {}) visibility_room={} basis_valid={} yaw_sin_q12={} yaw_cos_q12={} pitch_sin_q12={} pitch_cos_q12={} forward_xz={:?}",
            metrics.camera_map_valid,
            metrics.camera_local_x,
            metrics.camera_local_y,
            metrics.camera_local_z,
            metrics.camera_global_valid,
            metrics.camera_global_x,
            metrics.camera_global_y,
            metrics.camera_global_z,
            metrics.portal_current_room_index,
            metrics.camera_view_basis_valid,
            metrics.camera_view_sin_yaw_q12,
            metrics.camera_view_cos_yaw_q12,
            metrics.camera_view_sin_pitch_q12,
            metrics.camera_view_cos_pitch_q12,
            camera_forward
        );
        let _ = writeln!(
            out,
            "scheduler_tasks: fixed_avg_ms={:.3} fixed_max_ms={:.3} visual_avg_ms={:.3} visual_max_ms={:.3}",
            metrics.fixed_update_task_ms,
            metrics.fixed_update_task_max_ms,
            metrics.visual_render_task_ms,
            metrics.visual_render_task_max_ms
        );
        let _ = writeln!(
            out,
            "portal_counts: visible={} frontier={} missing_resident={} build_failed={} tests={} accepts={} bounds_fb={} rejects_b/f/t={:?} caps_r/f/d={:?}",
            metrics.portal_visible_rooms,
            metrics.portal_frontier_rooms,
            metrics.portal_missing_resident,
            metrics.portal_build_failed,
            metrics.portal_tests,
            metrics.portal_accepts,
            metrics.portal_bounds_fallbacks,
            metrics.portal_rejects,
            metrics.portal_caps
        );
        let _ = writeln!(
            out,
            "stream_counts: visible_chunks={} loaded={} candidates={} built={} cache_skips={} priorities_c/v/f={:?} req={} miss={} prefetch={} evict={} slot_limit={} pending={} failed={} protected_full={}",
            metrics.chunk_visible,
            metrics.chunk_loaded,
            metrics.chunk_candidates,
            metrics.chunk_built,
            metrics.chunk_cache_skips,
            metrics.stream_priorities,
            metrics.stream_requests,
            metrics.stream_misses,
            metrics.stream_prefetches,
            metrics.stream_evictions,
            metrics.stream_slot_limit,
            metrics.stream_pending,
            metrics.stream_failed,
            metrics.stream_protected_full
        );
        let _ = writeln!(
            out,
            "masks: loaded={:#018x} loading={:#018x} active={:#018x} drawn={:#018x} visible={:#018x} frontier={:#018x} missing={:#018x} build_failed={:#018x}",
            metrics.chunk_loaded_mask,
            metrics.chunk_loading_mask,
            metrics.chunk_active_mask,
            metrics.chunk_drawn_mask,
            metrics.portal_visible_mask,
            metrics.portal_frontier_mask,
            metrics.portal_missing_mask,
            metrics.portal_build_failed_mask
        );
        let _ = writeln!(
            out,
            "portal_masks: room_tested={:#018x} room_accepted={:#018x} room_reject_frustum={:#018x} room_bounds_fb={:#018x} portal_tested={:#018x} portal_accepted={:#018x} portal_reject_frustum={:#018x} portal_bounds_fb={:#018x}",
            metrics.portal_tested_mask,
            metrics.portal_accepted_mask,
            metrics.portal_reject_frustum_mask,
            metrics.portal_bounds_fallback_mask,
            metrics.portal_tested_portal_mask,
            metrics.portal_accepted_portal_mask,
            metrics.portal_reject_frustum_portal_mask,
            metrics.portal_bounds_fallback_portal_mask
        );
        self.append_portal_map_debug_snapshot(out, metrics);
    }

    pub(crate) fn append_portal_map_debug_snapshot(
        &self,
        out: &mut String,
        metrics: EditorPlaytestMetrics,
    ) {
        let map = collect_play_chunk_debug_map(&self.project);
        let player_room = metrics
            .player_map_valid
            .then_some(metrics.player_room_index as usize);
        let current_room = if metrics.camera_global_valid {
            Some(metrics.portal_current_room_index as usize)
        } else {
            player_room
        };
        let trace = collect_portal_clip_trace(&map, metrics, current_room);
        let _ = writeln!(
            out,
            "portal_map: runtime_rooms={} directed_portals={} player_runtime_room={:?} visibility_runtime_room={:?}",
            map.runtime_room_count(),
            map.portals.len(),
            player_room,
            current_room
        );
        if let Some(trace) = &trace {
            let _ = writeln!(
                out,
                "portal_clip_trace: camera_global={:?} entries={} half_fov_q12=({}, {}) near={} far={} max_depth={} min_width={}",
                trace.camera_global,
                trace.entries.len(),
                trace.camera.half_fov_x_tan_q12,
                trace.camera.half_fov_y_tan_q12,
                trace.camera.near_z,
                trace.camera.far_z,
                PLAY_PORTAL_DEBUG_MAX_DEPTH,
                trace.camera.min_portal_width_q12
            );
        } else {
            let _ = writeln!(out, "portal_clip_trace: unavailable");
        }
        if let Some(room_index) = current_room {
            self.append_runtime_room_debug_snapshot(out, &map, metrics, room_index, "current_room");
        }
        if let Some(player_room) = player_room {
            if Some(player_room) != current_room {
                self.append_runtime_room_debug_snapshot(
                    out,
                    &map,
                    metrics,
                    player_room,
                    "player_room",
                );
            }
        }
        let connected: Vec<_> = map
            .portals
            .iter()
            .filter(|portal| {
                current_room.is_none_or(|room| {
                    portal.source_room_index == room || portal.destination_room_index == room
                })
            })
            .collect();
        let _ = writeln!(out, "connected_portals: count={}", connected.len());
        let connected_indices: HashSet<usize> =
            connected.iter().map(|portal| portal.portal_index).collect();
        for portal in connected {
            self.append_portal_debug_snapshot(out, &map, metrics, portal, trace.as_ref());
        }
        let traversal_portals: Vec<_> = map
            .portals
            .iter()
            .filter(|portal| {
                if connected_indices.contains(&portal.portal_index) {
                    return false;
                }
                let source_bit = debug_chunk_bit(portal.source_room_index);
                let portal_bit = debug_chunk_bit(portal.portal_index);
                source_bit != 0
                    && portal_bit != 0
                    && metrics.portal_visible_mask & source_bit != 0
                    && metrics.portal_tested_portal_mask & portal_bit != 0
            })
            .collect();
        let _ = writeln!(
            out,
            "visible_source_traversal_portals: count={}",
            traversal_portals.len()
        );
        for portal in traversal_portals {
            self.append_portal_debug_snapshot(out, &map, metrics, portal, trace.as_ref());
        }
    }

    pub(crate) fn append_runtime_room_debug_snapshot(
        &self,
        out: &mut String,
        map: &PlayChunkDebugMap,
        metrics: EditorPlaytestMetrics,
        room_index: usize,
        label: &str,
    ) {
        let cells: Vec<_> = map
            .cells
            .iter()
            .filter(|cell| cell.runtime_room_index == room_index)
            .collect();
        let Some(first) = cells.first() else {
            let _ = writeln!(out, "{label}: runtime_room={room_index} missing_from_map");
            return;
        };
        let room_name = self
            .project
            .active_scene()
            .node(first.project_room_id)
            .map(|node| node.name.as_str())
            .unwrap_or("<missing room node>");
        let _ = writeln!(
            out,
            "{label}: runtime_room={} project_room=#{} '{}' portal_room={} flags=[{}] cell_count={}",
            room_index,
            first.project_room_id.raw(),
            room_name,
            first.portal_room_index,
            debug_room_flags(metrics, room_index),
            cells.len()
        );
        for cell in cells {
            let _ = writeln!(
                out,
                "  cell: array={:?} center={:?} half={:?} map_origin={:?} runtime_origin={:?} sector_size={}",
                cell.array_cell,
                cell.center,
                cell.half,
                cell.room_origin,
                cell.runtime_origin,
                cell.sector_size
            );
        }
    }

    pub(crate) fn append_portal_debug_snapshot(
        &self,
        out: &mut String,
        map: &PlayChunkDebugMap,
        metrics: EditorPlaytestMetrics,
        portal: &PlayChunkDebugMapPortal,
        trace: Option<&PortalClipTrace>,
    ) {
        let portal_bit = debug_chunk_bit(portal.portal_index);
        let tested = portal_bit != 0 && metrics.portal_tested_portal_mask & portal_bit != 0;
        let accepted = portal_bit != 0 && metrics.portal_accepted_portal_mask & portal_bit != 0;
        let rejected =
            portal_bit != 0 && metrics.portal_reject_frustum_portal_mask & portal_bit != 0;
        let bounds_fb =
            portal_bit != 0 && metrics.portal_bounds_fallback_portal_mask & portal_bit != 0;
        let _ = writeln!(
            out,
            "portal #{}: {} -> {} dir={:?} normal={:?} marker={:?} status tested={} accepted={} reject_frustum={} bounds_fb={} map_a={:?} map_b={:?} world={:?}",
            portal.portal_index,
            portal.source_room_index,
            portal.destination_room_index,
            portal.direction,
            portal.normal_world,
            portal.source_marker.map(NodeId::raw),
            tested,
            accepted,
            rejected,
            bounds_fb,
            portal.a,
            portal.b,
            portal.vertices_world
        );
        self.append_portal_clip_debug_snapshot(out, portal, trace);
        self.append_runtime_room_debug_snapshot(
            out,
            map,
            metrics,
            portal.source_room_index,
            "  source",
        );
        self.append_runtime_room_debug_snapshot(
            out,
            map,
            metrics,
            portal.destination_room_index,
            "  destination",
        );
    }

    pub(crate) fn append_portal_clip_debug_snapshot(
        &self,
        out: &mut String,
        portal: &PlayChunkDebugMapPortal,
        trace: Option<&PortalClipTrace>,
    ) {
        let Some(trace) = trace else {
            let _ = writeln!(out, "  clip: unavailable");
            return;
        };
        let mut matches = 0usize;
        for entry in trace
            .entries
            .iter()
            .filter(|entry| entry.portal_index == portal.portal_index)
        {
            matches += 1;
            let debug = entry.debug;
            let _ = writeln!(
                out,
                "  clip[{}]: parent_room={} parent_portal={} depth={} skipped_return={} decision={:?} front={} first_empty={:?} counts_n/l/r/b/t={}/{}/{}/{}/{} tiny={} parent={} padded={} projected={} clipped={} fallback={} result={}",
                matches - 1,
                entry.parent.room.raw(),
                debug_parent_portal_label(entry.parent.source_portal),
                entry.parent.depth,
                entry.skipped_return,
                debug.decision,
                debug.front_faces_camera,
                debug.first_empty_plane,
                debug.near_count,
                debug.left_count,
                debug.right_count,
                debug.bottom_count,
                debug.top_count,
                debug.tiny,
                portal_clip_debug_rect_text(Some(debug.parent)),
                portal_clip_debug_rect_text(Some(debug.padded_parent)),
                portal_clip_debug_rect_text(debug.projected_bounds),
                portal_clip_debug_rect_text(debug.clipped_bounds),
                portal_clip_debug_rect_text(debug.fallback_bounds),
                portal_clip_debug_rect_text(debug.result_bounds),
            );
            let _ = writeln!(
                out,
                "    view_vertices: {}",
                portal_clip_debug_vertices_text(debug.view_vertices)
            );
        }
        if matches == 0 {
            let _ = writeln!(out, "  clip: no traversal entry for this portal");
        }
    }
}
