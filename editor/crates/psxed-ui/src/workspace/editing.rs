use super::*;

impl EditorWorkspace {
    /// Press on a primitive: select it AND arm a drag. The
    /// drag itself doesn't apply any height change yet -- that
    /// happens in `update_primitive_drag` once the pointer
    /// actually moves. A pure click (no movement) flows through
    /// `commit_face_selection` and never touches `primitive_drag`.
    pub(crate) fn begin_primitive_drag(&mut self, modifiers: egui::Modifiers) {
        let Some((_, targets)) = self.prepare_primitive_drag_targets(modifiers) else {
            return;
        };
        if targets.is_empty() {
            return;
        }
        // Resolve the vertices the drag will translate and snapshot
        // their pre-drag Ys. Welded mode fans each seed out to all
        // coincident face-corners; detached mode keeps just the
        // selected face's corner records.
        let vertices = self.drag_vertices_for_targets(&targets);
        if vertices.is_empty() {
            return;
        }
        self.interaction = Interaction::PrimitiveHeight(PrimitiveDrag {
            targets,
            vertices,
            accumulated_pixel_dy: 0.0,
            snapshot_pushed: false,
        });
    }

    pub(crate) fn prepare_primitive_drag_targets(
        &mut self,
        modifiers: egui::Modifiers,
    ) -> Option<(Selection, Vec<Selection>)> {
        let target = self.selection.hovered_primitive?;
        let already_selected = self.primitive_is_selected(target)
            || self.floor_face_sector_is_selected(target).is_some();
        if !already_selected {
            if modifiers.shift || modifiers.command || modifiers.ctrl {
                self.apply_primitive_selection_modifiers(target, modifiers);
            } else {
                self.replace_primitive_selection(target);
                self.clear_node_selection_state();
                self.clear_sector_selection();
                self.update_primitive_resource_selection();
            }
        } else {
            self.selection.selected_primitive = Some(target);
        }

        let targets = self.primitive_drag_targets(target);
        Some((target, targets))
    }

    pub(crate) fn begin_primitive_pointer_drag(
        &mut self,
        rect: egui::Rect,
        pointer: egui::Pos2,
        modifiers: egui::Modifiers,
    ) {
        if modifiers.alt || !self.begin_primitive_grid_drag(rect, pointer, modifiers) {
            self.begin_primitive_drag(modifiers);
        }
    }

    pub(crate) fn begin_primitive_grid_drag(
        &mut self,
        rect: egui::Rect,
        pointer: egui::Pos2,
        modifiers: egui::Modifiers,
    ) -> bool {
        let Some((target, targets)) = self.prepare_primitive_drag_targets(modifiers) else {
            return false;
        };
        if targets.is_empty() {
            return false;
        }
        let room = target.room();
        let Some(grid) = self.room_grid_view(room) else {
            return false;
        };
        let (_, sx, sz) = selection_sector(target);
        let target_cell = [grid.origin[0] + sx as i32, grid.origin[1] + sz as i32];
        let start_cell = self
            .pointer_world_cell_for_room(rect, pointer, room)
            .unwrap_or(target_cell);
        let Ok((clipboard, _)) = self.primitive_geometry_clipboard_for_targets(&targets) else {
            return false;
        };
        self.interaction = Interaction::PrimitiveGrid(PrimitiveGridDrag {
            base_project: self.project.clone(),
            base_dirty: self.dirty,
            room,
            targets,
            source_origin: clipboard.source_origin,
            start_cell,
            current_delta: [0, 0],
            cells: clipboard.cells,
        });
        true
    }

    /// One drag-frame: accumulate mouse-Y travel, convert to a
    /// world-Y delta (snap-aware), and apply to every captured
    /// physical vertex.
    pub(crate) fn update_primitive_drag(&mut self, dy_pixels: f32) {
        if dy_pixels.abs() < f32::EPSILON {
            return;
        }
        // Pixels per HEIGHT_QUANTUM step -- drag 8 px to advance
        // one quantum. With HEIGHT_QUANTUM = 64 and a 1024-unit
        // sector, one full sector of height takes 128 pixels of
        // mouse travel -- comfortable for the orbit-cam panel.
        const PIXELS_PER_QUANTUM: f32 = 8.0;
        let Some(drag) = self.interaction.primitive_drag_mut() else {
            return;
        };
        // Screen +Y is down; world +Y is up -- invert.
        drag.accumulated_pixel_dy -= dy_pixels;
        let total_quanta = (drag.accumulated_pixel_dy / PIXELS_PER_QUANTUM).round() as i32;
        let world_delta = total_quanta * HEIGHT_QUANTUM;
        // No-op until the drag has crossed a quantum.
        if world_delta == 0 && !drag.snapshot_pushed {
            return;
        }
        // Lazy undo snapshot -- captures pre-drag state once,
        // never on a press-without-movement.
        if !drag.snapshot_pushed {
            drag.snapshot_pushed = true;
            self.push_undo();
        }
        // Re-borrow after the push_undo(&mut self) call.
        let Some(drag) = self.interaction.primitive_drag() else {
            return;
        };
        // Compute every (vertex, new_y) BEFORE entering the
        // mutable scene borrow so the apply step is one tight
        // loop without re-borrowing.
        let updates: Vec<(NodeId, PhysicalVertex, i32)> = drag
            .vertices
            .iter()
            .map(|entry| {
                let new_y = snap_height(entry.pre_drag_y + world_delta);
                (entry.room, entry.vertex.clone(), new_y)
            })
            .collect();
        for (room, vertex, new_y) in updates {
            let Some(grid) = self.room_floor_grid_mut(room) else {
                continue;
            };
            apply_vertex_height(grid, &vertex, new_y);
        }
        self.mark_dirty();
    }

    /// Drag released. Just clears the stroke; the heights are
    /// already committed.
    pub(crate) fn end_primitive_drag(&mut self) {
        if let Some(drag) = self.interaction.take_primitive_drag() {
            if drag.snapshot_pushed {
                let label = if drag.targets.len() == 1 {
                    describe_selection(drag.targets[0])
                } else {
                    format!("{} primitives", drag.targets.len())
                };
                self.status = format!(
                    "Translated {} ({} face-corners followed)",
                    label,
                    drag.vertices
                        .iter()
                        .map(|v| v.vertex.members.len())
                        .sum::<usize>(),
                );
            }
        }
    }

    pub(crate) fn update_primitive_grid_drag(&mut self, rect: egui::Rect, pointer: egui::Pos2) {
        let Some(drag) = self.interaction.primitive_grid_drag() else {
            return;
        };
        let Some(current_cell) = self.pointer_world_cell_for_room(rect, pointer, drag.room) else {
            return;
        };
        let delta = [
            current_cell[0] - drag.start_cell[0],
            current_cell[1] - drag.start_cell[1],
        ];
        if delta == drag.current_delta {
            return;
        }
        if let Some(drag) = self.interaction.primitive_grid_drag_mut() {
            drag.current_delta = delta;
        }
        self.apply_primitive_grid_drag_preview();
    }

    pub(crate) fn apply_primitive_grid_drag_preview(&mut self) {
        let Some(drag) = self.interaction.primitive_grid_drag().cloned() else {
            return;
        };

        self.project = drag.base_project.clone();
        self.dirty = drag.base_dirty;
        if drag.current_delta == [0, 0] {
            self.select_geometry_primitives(drag.room, drag.targets);
            return;
        }

        remove_primitive_faces_from_project(&mut self.project, &drag.targets, self.active_floor);
        let mut selected_primitives = Vec::new();
        let active_floor = self.active_floor;
        {
            let scene = self.project.active_scene_mut();
            let Some(node) = scene.node(drag.room) else {
                self.interaction = Interaction::Idle;
                self.status = "Move target room no longer exists".to_string();
                return;
            };
            let NodeKind::Section { .. } = &node.kind else {
                self.interaction = Interaction::Idle;
                self.status = "Move target is not a Section".to_string();
                return;
            };
            for cell in &drag.cells {
                let _ = extend_room_grid_to_include_preserving_child_positions(
                    scene,
                    drag.room,
                    drag.source_origin[0] + drag.current_delta[0] + cell.offset[0],
                    drag.source_origin[1] + drag.current_delta[1] + cell.offset[1],
                    active_floor,
                );
            }
            let Some(node) = scene.node_mut(drag.room) else {
                self.interaction = Interaction::Idle;
                self.status = "Move target room no longer exists".to_string();
                return;
            };
            let NodeKind::Section { grid } = &mut node.kind else {
                self.interaction = Interaction::Idle;
                self.status = "Move target is not a Section".to_string();
                return;
            };
            let floor_idx = active_floor.min(grid.floor_count().saturating_sub(1));
            let grid = grid
                .floor_mut(floor_idx)
                .expect("floor index clamped to range");
            for cell in drag.cells {
                let wcx = drag.source_origin[0] + drag.current_delta[0] + cell.offset[0];
                let wcz = drag.source_origin[1] + drag.current_delta[1] + cell.offset[1];
                let Some((sx, sz)) = grid.world_cell_to_array(wcx, wcz) else {
                    continue;
                };
                let Some(index) = grid.sector_index(sx, sz) else {
                    continue;
                };
                let Some(fragment) = cell.sector else {
                    continue;
                };
                let target = grid.sectors[index].get_or_insert_with(GridSector::empty);
                merge_primitive_fragment(
                    target,
                    fragment,
                    drag.room,
                    sx,
                    sz,
                    &mut selected_primitives,
                );
            }
        }
        self.select_geometry_primitives(drag.room, selected_primitives);
    }

    pub(crate) fn pointer_world_cell_for_room(
        &self,
        rect: egui::Rect,
        pointer: egui::Pos2,
        room: NodeId,
    ) -> Option<[i32; 2]> {
        let grid = self
            .interaction
            .primitive_grid_drag()
            .filter(|drag| drag.room == room)
            .and_then(|drag| drag.base_project.active_scene().node(room))
            .and_then(|node| match &node.kind {
                NodeKind::Section { grid } => Some(grid),
                _ => None,
            })
            .or_else(|| self.room_grid_view(room))?;
        let (origin, dir) = self.camera_ray_for_pointer(rect, pointer)?;
        let hit = ray_intersects_horizontal_plane(origin, dir, 0.0)?;
        Some([grid.world_x_to_cell(hit[0]), grid.world_z_to_cell(hit[2])])
    }

    pub(crate) fn end_primitive_grid_drag(&mut self) {
        let Some(drag) = self.interaction.take_primitive_grid_drag() else {
            return;
        };
        if drag.current_delta == [0, 0] {
            self.project = drag.base_project;
            self.dirty = drag.base_dirty;
            self.select_geometry_primitives(drag.room, drag.targets);
            return;
        }

        let moved = drag.targets.len();
        let delta = drag.current_delta;
        self.history.record(drag.base_project);
        self.status = if moved == 1 {
            format!("Moved 1 primitive by {},{} cells", delta[0], delta[1])
        } else {
            format!(
                "Moved {moved} primitives by {},{} cells",
                delta[0], delta[1]
            )
        };
        self.mark_dirty();
    }

    pub(crate) fn primitive_selection_room(&self, targets: &[Selection]) -> Option<NodeId> {
        let room = targets.first()?.room();
        targets
            .iter()
            .all(|selection| selection.room() == room)
            .then_some(room)
    }

    pub(crate) fn primitive_selection_pivot(&self, targets: &[Selection]) -> Option<[f32; 3]> {
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        let mut any = false;
        for &selection in targets {
            for point in self.selection_world_points(selection)? {
                for axis in 0..3 {
                    min[axis] = min[axis].min(point[axis]);
                    max[axis] = max[axis].max(point[axis]);
                }
                any = true;
            }
        }
        any.then_some([
            (min[0] + max[0]) * 0.5,
            (min[1] + max[1]) * 0.5,
            (min[2] + max[2]) * 0.5,
        ])
    }

    pub(crate) fn primitive_gizmo_screen_axes(&self, rect: Rect) -> Vec<PrimitiveGizmoScreenAxis> {
        let targets = self.selected_primitive_targets();
        let Some(room) = self.primitive_selection_room(&targets) else {
            return Vec::new();
        };
        let Some(grid) = self.room_grid_view(room) else {
            return Vec::new();
        };
        let Some(pivot) = self.primitive_selection_pivot(&targets) else {
            return Vec::new();
        };
        let camera = self.viewport_3d_camera();
        let Some(start) = project_world_to_viewport_screen(camera, rect, pivot) else {
            return Vec::new();
        };
        let sector_size = grid.sector_size.max(1);
        [
            PrimitiveGizmoAxis::X,
            PrimitiveGizmoAxis::Y,
            PrimitiveGizmoAxis::Z,
        ]
        .into_iter()
        .filter_map(|axis| {
            let delta = axis.world_delta(sector_size);
            let end_world = [
                pivot[0] + delta[0],
                pivot[1] + delta[1],
                pivot[2] + delta[2],
            ];
            let end = project_world_to_viewport_screen(camera, rect, end_world)?;
            ((end - start).length_sq() >= 64.0).then_some(PrimitiveGizmoScreenAxis {
                axis,
                start,
                end,
            })
        })
        .collect()
    }

    pub(crate) fn pick_primitive_gizmo_axis(
        &self,
        rect: Rect,
        pointer: Pos2,
    ) -> Option<PrimitiveGizmoAxis> {
        self.primitive_gizmo_screen_axes(rect)
            .into_iter()
            .filter_map(|screen_axis| {
                let distance = distance_to_segment_2d(pointer, screen_axis.start, screen_axis.end)
                    .min((pointer - screen_axis.end).length());
                (distance <= GIZMO_AXIS_PICK_RADIUS).then_some((distance, screen_axis.axis))
            })
            .min_by(|(a, _), (b, _)| a.total_cmp(b))
            .map(|(_, axis)| axis)
    }

    pub(crate) fn draw_primitive_gizmo(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        hovered_axis: Option<PrimitiveGizmoAxis>,
    ) {
        if self.transform_gizmo_mode != TransformGizmoMode::Move {
            return;
        }
        let axes = self.primitive_gizmo_screen_axes(rect);
        if axes.is_empty() {
            return;
        }
        let active_axis = self
            .interaction
            .primitive_gizmo_drag()
            .map(|drag| drag.axis);
        painter.circle_filled(axes[0].start, 4.0, Color32::from_rgb(235, 242, 248));
        for screen_axis in axes {
            let highlighted =
                active_axis == Some(screen_axis.axis) || hovered_axis == Some(screen_axis.axis);
            let color = gizmo_axis_color(screen_axis.axis, highlighted);
            let stroke_width = gizmo_axis_stroke_width(highlighted);
            painter.line_segment(
                [screen_axis.start, screen_axis.end],
                Stroke::new(stroke_width, color),
            );
            painter.circle_filled(
                screen_axis.end,
                gizmo_axis_handle_radius(highlighted),
                color,
            );
            let label_offset = (screen_axis.end - screen_axis.start).normalized() * 12.0;
            painter.text(
                screen_axis.end + label_offset,
                Align2::CENTER_CENTER,
                screen_axis.axis.label(),
                FontId::monospace(12.0),
                color,
            );
        }
    }

    /// World directions of the gizmo handle axes, as matrix columns.
    ///
    /// Identity in Global space; the active (first) target's authored
    /// rotation in Local space. Scale handles always follow the object
    /// regardless of the toggle: they edit object dimensions, so
    /// world-aligned handles on a rotated prop would point away from
    /// the extent they resize.
    pub(crate) fn node_gizmo_basis(&self, targets: &[NodeId]) -> [[f32; 3]; 3] {
        const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let object_aligned = match self.transform_gizmo_mode {
            TransformGizmoMode::Scale => true,
            TransformGizmoMode::Move | TransformGizmoMode::Rotate => {
                self.gizmo_space == GizmoSpace::Local
            }
        };
        if !object_aligned {
            return IDENTITY;
        }
        let scene = self.project.active_scene();
        targets
            .iter()
            .find_map(|id| scene.node(*id))
            .map(|node| euler_degrees_to_matrix(node.transform.rotation_degrees))
            .unwrap_or(IDENTITY)
    }

    pub(crate) fn node_gizmo_screen_axes(&self, rect: Rect) -> Vec<PrimitiveGizmoScreenAxis> {
        let targets = self.selected_node_gizmo_targets();
        if targets.is_empty() {
            return Vec::new();
        }
        let Some((pivot, _)) = self.node_gizmo_bounds_3d(&targets) else {
            return Vec::new();
        };
        let camera = self.viewport_3d_camera();
        let Some(start) = project_world_to_viewport_screen(camera, rect, pivot) else {
            return Vec::new();
        };
        let axis_len = self.node_gizmo_axis_world_length(&targets) as f32;
        let basis = self.node_gizmo_basis(&targets);
        [
            PrimitiveGizmoAxis::X,
            PrimitiveGizmoAxis::Y,
            PrimitiveGizmoAxis::Z,
        ]
        .into_iter()
        .filter_map(|axis| {
            let dir = basis_column(&basis, axis.index());
            let end_world = [
                pivot[0] + dir[0] * axis_len,
                pivot[1] + dir[1] * axis_len,
                pivot[2] + dir[2] * axis_len,
            ];
            let end = project_world_to_viewport_screen(camera, rect, end_world)?;
            ((end - start).length_sq() >= 64.0).then_some(PrimitiveGizmoScreenAxis {
                axis,
                start,
                end,
            })
        })
        .collect()
    }

    /// Six object-aligned resize anchors, one at the centre of each Box
    /// Prop face. The short stem points outwards and supplies an
    /// unambiguous drag direction even when a face is seen obliquely.
    pub(crate) fn box_prop_face_screen_handles(&self, rect: Rect) -> Vec<BoxPropFaceScreenHandle> {
        if self.transform_gizmo_mode != TransformGizmoMode::Scale {
            return Vec::new();
        }
        let targets = self.selected_node_gizmo_targets();
        let [node_id] = targets.as_slice() else {
            return Vec::new();
        };
        let scene = self.project.active_scene();
        let Some(node) = scene.node(*node_id) else {
            return Vec::new();
        };
        let NodeKind::BoxProp { vertices, .. } = &node.kind else {
            return Vec::new();
        };
        let Some(room_id) = enclosing_room_id(scene, *node_id) else {
            return Vec::new();
        };
        let Some(room) = scene.node(room_id) else {
            return Vec::new();
        };
        let NodeKind::Section { grid } = &room.kind else {
            return Vec::new();
        };
        let floor = psxed_project::floor_view::node_floor(scene, *node_id);
        let Some(node_grid) = grid.floor(floor) else {
            return Vec::new();
        };
        let Some(y_offset) = psxed_project::floor_view::node_draw_offset(
            scene,
            room_id,
            self.active_floor,
            *node_id,
        ) else {
            return Vec::new();
        };
        let mut origin =
            psxed_project::spatial::node_preview_origin_f32(node_grid, &node.transform);
        origin[1] += y_offset as f32;
        let basis = euler_degrees_to_matrix(node.transform.rotation_degrees);
        let camera = self.viewport_3d_camera();
        let stem_length = (node_grid.sector_size.max(1) as f32 * 0.18).max(64.0);
        const FACE_NORMALS: [[f32; 3]; psxed_project::BOX_PROP_FACE_COUNT] = [
            [0.0, 0.0, -1.0],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [-1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
        ];

        psxed_project::BOX_PROP_FACE_VERTEX_INDICES
            .iter()
            .enumerate()
            .filter_map(|(face, indices)| {
                let mut local_center = [0.0; 3];
                for index in indices {
                    for axis in 0..3 {
                        local_center[axis] += f32::from(vertices[*index][axis]) * 0.25;
                    }
                }
                let rotated_center = rotate_vector_by_matrix(&basis, local_center);
                let center_world = [
                    origin[0] + rotated_center[0],
                    origin[1] + rotated_center[1],
                    origin[2] + rotated_center[2],
                ];
                let normal = rotate_vector_by_matrix(&basis, FACE_NORMALS[face]);
                let end_world = [
                    center_world[0] + normal[0] * stem_length,
                    center_world[1] + normal[1] * stem_length,
                    center_world[2] + normal[2] * stem_length,
                ];
                let center = project_world_to_viewport_screen(camera, rect, center_world)?;
                let end = project_world_to_viewport_screen(camera, rect, end_world)?;
                ((end - center).length_sq() >= 16.0).then_some(BoxPropFaceScreenHandle {
                    face: face as u8,
                    center,
                    end,
                })
            })
            .collect()
    }

    pub(crate) fn node_gizmo_screen_planes(&self, rect: Rect) -> Vec<NodeGizmoScreenPlane> {
        if self.transform_gizmo_mode != TransformGizmoMode::Move {
            return Vec::new();
        }
        // Plane handles drag along axis-aligned world planes and snap
        // per world component; in Local space the axis handles carry
        // the rotated directions and the planes are hidden rather than
        // drawn misaligned with them.
        if self.gizmo_space == GizmoSpace::Local {
            return Vec::new();
        }
        let targets = self.selected_node_gizmo_targets();
        if targets.is_empty() {
            return Vec::new();
        }
        let Some((pivot, _)) = self.node_gizmo_bounds_3d(&targets) else {
            return Vec::new();
        };
        let camera = self.viewport_3d_camera();
        let axis_len = self.node_gizmo_axis_world_length(&targets) as f32;
        let near = axis_len * 0.18;
        let far = axis_len * 0.44;

        NodeGizmoPlane::ALL
            .into_iter()
            .filter_map(|plane| {
                let [a, b] = plane.axes();
                let a_delta = a.world_delta(1);
                let b_delta = b.world_delta(1);
                let corner_world = |a_scale: f32, b_scale: f32| {
                    [
                        pivot[0] + a_delta[0] * a_scale + b_delta[0] * b_scale,
                        pivot[1] + a_delta[1] * a_scale + b_delta[1] * b_scale,
                        pivot[2] + a_delta[2] * a_scale + b_delta[2] * b_scale,
                    ]
                };
                let corners = [
                    project_world_to_viewport_screen(camera, rect, corner_world(near, near))?,
                    project_world_to_viewport_screen(camera, rect, corner_world(far, near))?,
                    project_world_to_viewport_screen(camera, rect, corner_world(far, far))?,
                    project_world_to_viewport_screen(camera, rect, corner_world(near, far))?,
                ];
                (polygon_area_2d(&corners).abs() >= 24.0)
                    .then_some(NodeGizmoScreenPlane { plane, corners })
            })
            .collect()
    }

    pub(crate) fn selected_node_gizmo_targets(&self) -> Vec<NodeId> {
        self.selected_node_ids_in_hierarchy()
            .into_iter()
            .filter(|id| self.node_supports_transform_gizmo(*id, self.transform_gizmo_mode))
            .collect()
    }

    pub(crate) fn node_supports_transform_gizmo(
        &self,
        id: NodeId,
        mode: TransformGizmoMode,
    ) -> bool {
        if self.scene_node_effectively_hidden(id) {
            return false;
        }
        self.project
            .active_scene()
            .node(id)
            .is_some_and(|node| node_kind_supports_transform_gizmo(&node.kind, mode))
    }

    pub(crate) fn node_gizmo_bounds_3d(&self, targets: &[NodeId]) -> Option<([f32; 3], [f32; 3])> {
        let mut bounds = None;
        for id in targets {
            if let Some((center, half)) = self.node_frame_bounds_3d(*id) {
                merge_bounds_3d(&mut bounds, center, half);
            }
        }
        bounds.map(bounds_3d_to_center_half)
    }

    pub(crate) fn node_gizmo_axis_world_length(&self, targets: &[NodeId]) -> i32 {
        let scene = self.project.active_scene();
        for id in targets {
            let Some(room_id) = enclosing_room_id(scene, *id) else {
                continue;
            };
            let Some(room) = scene.node(room_id) else {
                continue;
            };
            let NodeKind::Section { grid } = &room.kind else {
                continue;
            };
            return grid.sector_size.max(1);
        }
        DEFAULT_WORLD_SECTOR_SIZE
    }

    pub(crate) fn node_rotation_gizmo_screen_ring_for_axis(
        &self,
        rect: Rect,
        axis: PrimitiveGizmoAxis,
    ) -> Option<NodeRotationGizmoScreenRing> {
        let targets = self.selected_node_gizmo_targets();
        if targets.is_empty() {
            return None;
        }
        let (pivot, half) = self.node_gizmo_bounds_3d(&targets)?;
        let camera = self.viewport_3d_camera();
        let center = project_world_to_viewport_screen(camera, rect, pivot)?;
        let base_radius = self.node_gizmo_axis_world_length(&targets) as f32 * 0.65;
        let bound_radius = half[0].max(half[1]).max(half[2]) * 1.35;
        let radius = base_radius.max(bound_radius).max(128.0);
        // Ring lies in the plane perpendicular to `axis`, spanned by
        // the other two basis columns in the cyclic order that makes a
        // positive rotation about `axis` carry `u` toward `v`. Ring
        // points are therefore ordered so a positive world rotation
        // advances them, which is what the drag's winding test reads.
        let basis = self.node_gizmo_basis(&targets);
        let u = basis_column(&basis, (axis.index() + 1) % 3);
        let v = basis_column(&basis, (axis.index() + 2) % 3);
        let mut points = Vec::with_capacity(49);
        for step in 0..=48 {
            let angle = step as f32 / 48.0 * std::f32::consts::TAU;
            let (sin, cos) = angle.sin_cos();
            let world = [
                pivot[0] + (u[0] * cos + v[0] * sin) * radius,
                pivot[1] + (u[1] * cos + v[1] * sin) * radius,
                pivot[2] + (u[2] * cos + v[2] * sin) * radius,
            ];
            if let Some(screen) = project_world_to_viewport_screen(camera, rect, world) {
                points.push(screen);
            }
        }
        (points.len() >= 8).then_some(NodeRotationGizmoScreenRing {
            axis,
            center,
            points,
        })
    }

    pub(crate) fn node_rotation_gizmo_screen_rings(
        &self,
        rect: Rect,
    ) -> Vec<NodeRotationGizmoScreenRing> {
        self.selected_node_rotation_axes()
            .into_iter()
            .filter_map(|axis| self.node_rotation_gizmo_screen_ring_for_axis(rect, axis))
            .collect()
    }

    pub(crate) fn selected_node_rotation_axes(&self) -> Vec<PrimitiveGizmoAxis> {
        let scene = self.project.active_scene();
        let targets = self.selected_node_gizmo_targets();
        if targets.is_empty() {
            return Vec::new();
        }
        let all_axes = [
            PrimitiveGizmoAxis::X,
            PrimitiveGizmoAxis::Y,
            PrimitiveGizmoAxis::Z,
        ];
        let mut axes: Vec<PrimitiveGizmoAxis> = all_axes.to_vec();
        for id in &targets {
            let Some(node) = scene.node(*id) else {
                continue;
            };
            let supported = node_rotation_axes(&node.kind);
            axes.retain(|axis| supported.contains(axis));
        }
        axes
    }

    pub(crate) fn pick_node_gizmo_handle(
        &self,
        rect: Rect,
        pointer: Pos2,
    ) -> Option<NodeGizmoHandle> {
        if self.transform_gizmo_mode == TransformGizmoMode::Rotate {
            return self
                .node_rotation_gizmo_screen_rings(rect)
                .into_iter()
                .filter_map(|ring| {
                    ring.points
                        .windows(2)
                        .map(|pair| distance_to_segment_2d(pointer, pair[0], pair[1]))
                        .min_by(|a, b| a.total_cmp(b))
                        .map(|distance| (distance, ring.axis))
                })
                .filter(|(distance, _)| *distance <= GIZMO_ROTATION_PICK_RADIUS)
                .min_by(|(a, _), (b, _)| a.total_cmp(b))
                .map(|(_, axis)| NodeGizmoHandle::Axis(axis));
        }
        if self.transform_gizmo_mode == TransformGizmoMode::Scale {
            if let Some((_, handle)) = self
                .box_prop_face_screen_handles(rect)
                .into_iter()
                .map(|handle| ((pointer - handle.end).length(), handle))
                .filter(|(distance, _)| *distance <= GIZMO_AXIS_PICK_RADIUS)
                .min_by(|(a, _), (b, _)| a.total_cmp(b))
            {
                return Some(NodeGizmoHandle::BoxFace(handle.face));
            }
        }
        // Axes and move planes overlap on screen: each plane handle is an
        // inner quad spanning two axes, so a cursor inside a plane is also
        // within the axis pick radius of both of that plane's axes.
        // Gather every in-tolerance handle and choose the globally closest
        // instead of returning the first kind checked. The old code
        // early-returned on axes, so a click inside the plane grabbed an
        // axis -- and when the plane was foreshortened while zoomed out,
        // that miss read as "grabbed the tile behind it". Ties go to the
        // plane: a plane hit reports distance 0 by polygon containment, so
        // an equal distance means the cursor is inside the quad, where the
        // user is aiming at the plane rather than its bounding axes.
        let mut best: Option<(f32, u8, NodeGizmoHandle)> = None;
        let mut consider = |distance: f32, plane_tiebreak: u8, handle: NodeGizmoHandle| {
            let better = match best {
                Some((best_distance, best_tiebreak, _)) => {
                    distance < best_distance
                        || (distance == best_distance && plane_tiebreak > best_tiebreak)
                }
                None => true,
            };
            if better {
                best = Some((distance, plane_tiebreak, handle));
            }
        };
        for screen_axis in self.node_gizmo_screen_axes(rect) {
            let distance = distance_to_segment_2d(pointer, screen_axis.start, screen_axis.end)
                .min((pointer - screen_axis.end).length());
            if distance <= GIZMO_AXIS_PICK_RADIUS {
                consider(distance, 0, NodeGizmoHandle::Axis(screen_axis.axis));
            }
        }
        if self.transform_gizmo_mode == TransformGizmoMode::Move {
            let axes = self.node_gizmo_screen_axes(rect);
            let pivot = axes.first().map(|axis| axis.start);
            for screen_plane in self.node_gizmo_screen_planes(rect) {
                // The plane is drawn as a small square inset into the
                // corner between its two axes, but the user reads the whole
                // corner as the handle. So the plane is grabbable in two
                // tiers, both reported at distance-to-quad so a genuinely
                // nearby axis (added above within its 10px radius) still
                // wins and the plane only claims interior the axes leave:
                //   1. inside the quad (distance 0) or within the usual
                //      few-px tolerance of it -- the tight, primary target;
                //   2. anywhere inside the footprint triangle (pivot + the
                //      two axis endpoints) -- fills the wedge of bare tile
                //      that used to sit between the inset quad and the axes
                //      and caused zoomed-out clicks to fall through to the
                //      floor.
                let quad_distance = if point_in_polygon_2d(pointer, &screen_plane.corners) {
                    0.0
                } else {
                    distance_to_polygon_edges_2d(pointer, &screen_plane.corners)
                };
                if quad_distance <= GIZMO_PLANE_PICK_RADIUS {
                    consider(quad_distance, 1, NodeGizmoHandle::Plane(screen_plane.plane));
                    continue;
                }
                if let Some(pivot) = pivot {
                    let [axis_a, axis_b] = screen_plane.plane.axes();
                    let end_a = axes.iter().find(|axis| axis.axis == axis_a).map(|a| a.end);
                    let end_b = axes.iter().find(|axis| axis.axis == axis_b).map(|a| a.end);
                    if let (Some(end_a), Some(end_b)) = (end_a, end_b) {
                        if point_in_polygon_2d(pointer, &[pivot, end_a, end_b]) {
                            consider(quad_distance, 1, NodeGizmoHandle::Plane(screen_plane.plane));
                        }
                    }
                }
            }
        }
        best.map(|(_, _, handle)| handle)
    }

    pub(crate) fn resolve_viewport_3d_pointer_target(
        &self,
        rect: Rect,
        pointer: Pos2,
        room_filter: Option<NodeId>,
        select_pick_enabled: bool,
    ) -> Option<Viewport3dPointerTarget> {
        if select_pick_enabled {
            if self.transform_gizmo_mode == TransformGizmoMode::Move {
                if let Some(axis) = self.pick_primitive_gizmo_axis(rect, pointer) {
                    return Some(Viewport3dPointerTarget::PrimitiveGizmo(axis));
                }
            }
            if let Some(handle) = self.pick_node_gizmo_handle(rect, pointer) {
                return Some(Viewport3dPointerTarget::NodeGizmo(handle));
            }
        }

        let surface = self
            .pick_face_with_hit(rect, pointer)
            .and_then(|(face, hit)| {
                let (origin, _) = self.camera_ray_for_pointer(rect, pointer)?;
                Some((
                    Viewport3dPointerTarget::Surface {
                        face,
                        hit,
                        selection: self.pick_primitive_from_hit(face, hit),
                    },
                    distance3_f32(origin, hit),
                ))
            });
        if !select_pick_enabled {
            return surface.map(|(target, _)| target);
        }

        let brush = self
            .pick_brush_face_nearest_for_selection_3d(rect, pointer)
            .and_then(|(brush, face, hit)| {
                let (origin, _) = self.camera_ray_for_pointer(rect, pointer)?;
                Some((
                    Viewport3dPointerTarget::Brush { brush, face },
                    distance3_f32(origin, hit),
                ))
            });

        // Compare all authored geometry in one distance domain. Preserve the
        // legacy tie rule where entities consume a click exactly on a surface.
        let mut best = surface;
        if let Some((target, distance)) = brush {
            if best.is_none_or(|(_, best_distance)| distance < best_distance) {
                best = Some((target, distance));
            }
        }
        if let Some(entity) = self.pick_entity_bound(rect, pointer, room_filter) {
            if best.is_none_or(|(_, best_distance)| entity.distance <= best_distance) {
                best = Some((Viewport3dPointerTarget::Entity(entity), entity.distance));
            }
        }
        best.map(|(target, _)| target)
    }

    pub(crate) fn draw_node_gizmo(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        hovered_handle: Option<NodeGizmoHandle>,
    ) {
        if self.transform_gizmo_mode == TransformGizmoMode::Rotate {
            let rings = self.node_rotation_gizmo_screen_rings(rect);
            if rings.is_empty() {
                return;
            }
            let active_axis = self
                .interaction
                .node_gizmo_drag()
                .and_then(|drag| drag.handle.axis());
            painter.circle_filled(rings[0].center, 4.0, Color32::from_rgb(235, 242, 248));
            for ring in &rings {
                let highlighted = active_axis == Some(ring.axis)
                    || hovered_handle == Some(NodeGizmoHandle::Axis(ring.axis));
                let color = gizmo_axis_color(ring.axis, highlighted);
                let stroke_width = gizmo_axis_stroke_width(highlighted);
                for pair in ring.points.windows(2) {
                    painter.line_segment([pair[0], pair[1]], Stroke::new(stroke_width, color));
                }
                if let Some(label_pos) = ring.points.first().copied() {
                    painter.circle_filled(label_pos, gizmo_axis_handle_radius(highlighted), color);
                    painter.text(
                        label_pos + Vec2::new(14.0, 0.0),
                        Align2::CENTER_CENTER,
                        ring.axis.label(),
                        FontId::monospace(12.0),
                        color,
                    );
                }
            }
            if let Some(drag) = self
                .interaction
                .node_gizmo_drag()
                .filter(|drag| drag.mode == TransformGizmoMode::Rotate)
            {
                let snap = if drag.group_brushes.is_empty() {
                    1
                } else {
                    BRUSH_ROTATION_SNAP_DEGREES
                };
                paint_rotation_readout(painter, rings[0].center, drag.current_steps, snap);
            }
            return;
        }
        if self.transform_gizmo_mode == TransformGizmoMode::Scale {
            let handles = self.box_prop_face_screen_handles(rect);
            if !handles.is_empty() {
                let active_handle = self.interaction.node_gizmo_drag().map(|drag| drag.handle);
                for handle in handles {
                    let face_handle = NodeGizmoHandle::BoxFace(handle.face);
                    let highlighted =
                        active_handle == Some(face_handle) || hovered_handle == Some(face_handle);
                    let axis = box_prop_face_axis(handle.face);
                    let color = gizmo_axis_color(axis, highlighted);
                    painter.line_segment(
                        [handle.center, handle.end],
                        Stroke::new(gizmo_axis_stroke_width(highlighted), color),
                    );
                    painter.circle_filled(
                        handle.end,
                        gizmo_axis_handle_radius(highlighted) + 1.5,
                        color,
                    );
                    painter.circle_stroke(
                        handle.end,
                        gizmo_axis_handle_radius(highlighted) + 3.5,
                        Stroke::new(1.0, Color32::from_white_alpha(150)),
                    );
                }
                return;
            }
        }
        let axes = self.node_gizmo_screen_axes(rect);
        if axes.is_empty() {
            return;
        }
        let active_handle = self.interaction.node_gizmo_drag().map(|drag| drag.handle);
        painter.circle_filled(axes[0].start, 4.0, Color32::from_rgb(235, 242, 248));
        if self.transform_gizmo_mode == TransformGizmoMode::Move {
            for screen_plane in self.node_gizmo_screen_planes(rect) {
                let highlighted = active_handle == Some(NodeGizmoHandle::Plane(screen_plane.plane))
                    || hovered_handle == Some(NodeGizmoHandle::Plane(screen_plane.plane));
                let color = gizmo_highlight_color(screen_plane.plane.color(), highlighted);
                let fill_alpha = if highlighted { 128 } else { 58 };
                let stroke_width = if highlighted { 3.0 } else { 1.5 };
                painter.add(egui::Shape::convex_polygon(
                    screen_plane.corners.to_vec(),
                    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), fill_alpha),
                    Stroke::new(stroke_width, color),
                ));
            }
        }
        for screen_axis in axes {
            let highlighted = active_handle == Some(NodeGizmoHandle::Axis(screen_axis.axis))
                || hovered_handle == Some(NodeGizmoHandle::Axis(screen_axis.axis));
            let color = gizmo_axis_color(screen_axis.axis, highlighted);
            let stroke_width = gizmo_axis_stroke_width(highlighted);
            painter.line_segment(
                [screen_axis.start, screen_axis.end],
                Stroke::new(stroke_width, color),
            );
            painter.circle_filled(
                screen_axis.end,
                gizmo_axis_handle_radius(highlighted),
                color,
            );
            let label_offset = (screen_axis.end - screen_axis.start).normalized() * 12.0;
            painter.text(
                screen_axis.end + label_offset,
                Align2::CENTER_CENTER,
                screen_axis.axis.label(),
                FontId::monospace(12.0),
                color,
            );
        }
    }

    pub(crate) fn begin_primitive_gizmo_drag(
        &mut self,
        axis: PrimitiveGizmoAxis,
        rect: Rect,
        pointer: Pos2,
    ) -> bool {
        let targets = self.selected_primitive_targets();
        if targets.is_empty() {
            return false;
        }
        let Some(room) = self.primitive_selection_room(&targets) else {
            self.status = "Move one room's geometry at a time".to_string();
            return false;
        };
        let Some(screen_axis) = self
            .primitive_gizmo_screen_axes(rect)
            .into_iter()
            .find(|candidate| candidate.axis == axis)
        else {
            return false;
        };
        let screen_axis_delta = screen_axis.end - screen_axis.start;
        if screen_axis_delta.length_sq() < 64.0 {
            return false;
        }

        let mut y_vertices = Vec::new();
        let mut grid_drag = None;
        match axis {
            PrimitiveGizmoAxis::Y => {
                y_vertices = self.drag_vertices_for_targets(&targets);
                if y_vertices.is_empty() {
                    return false;
                }
            }
            PrimitiveGizmoAxis::X | PrimitiveGizmoAxis::Z => {
                let Ok((clipboard, _)) = self.primitive_geometry_clipboard_for_targets(&targets)
                else {
                    self.status = "Selected primitives are empty".to_string();
                    return false;
                };
                grid_drag = Some(PrimitiveGizmoGridDrag {
                    base_project: self.project.clone(),
                    base_dirty: self.dirty,
                    room,
                    targets: targets.clone(),
                    source_origin: clipboard.source_origin,
                    current_delta: [0, 0],
                    cells: clipboard.cells,
                });
            }
        }

        self.interaction = Interaction::PrimitiveGizmo(PrimitiveGizmoDrag {
            axis,
            start_pointer: pointer,
            screen_axis: screen_axis_delta,
            targets,
            y_vertices,
            grid: grid_drag,
            current_steps: 0,
            snapshot_pushed: false,
        });
        true
    }

    pub(crate) fn update_primitive_gizmo_drag(&mut self, pointer: Pos2) {
        let Some(drag) = self.interaction.primitive_gizmo_drag() else {
            return;
        };
        let axis_len_sq = drag.screen_axis.length_sq();
        if axis_len_sq < f32::EPSILON {
            return;
        }
        let pointer_delta = pointer - drag.start_pointer;
        let steps = match drag.axis {
            PrimitiveGizmoAxis::Y => {
                const PIXELS_PER_QUANTUM: f32 = 4.0;
                let unit = drag.screen_axis / axis_len_sq.sqrt();
                (pointer_delta.dot(unit) / PIXELS_PER_QUANTUM).round() as i32
            }
            PrimitiveGizmoAxis::X | PrimitiveGizmoAxis::Z => {
                (pointer_delta.dot(drag.screen_axis) * 2.0 / axis_len_sq).round() as i32
            }
        };
        if steps == drag.current_steps {
            return;
        }
        let axis = drag.axis;
        if let Some(drag) = self.interaction.primitive_gizmo_drag_mut() {
            drag.current_steps = steps;
            if let Some(grid) = drag.grid.as_mut() {
                grid.current_delta = axis.cell_delta(steps);
            }
        }
        match axis {
            PrimitiveGizmoAxis::Y => self.apply_primitive_gizmo_y_drag(),
            PrimitiveGizmoAxis::X | PrimitiveGizmoAxis::Z => {
                self.apply_primitive_gizmo_grid_preview();
            }
        }
    }

    pub(crate) fn apply_primitive_gizmo_y_drag(&mut self) {
        let Some(drag) = self.interaction.primitive_gizmo_drag() else {
            return;
        };
        let world_delta = drag.current_steps * HEIGHT_QUANTUM;
        if world_delta == 0 && !drag.snapshot_pushed {
            return;
        }
        if !drag.snapshot_pushed {
            if let Some(drag) = self.interaction.primitive_gizmo_drag_mut() {
                drag.snapshot_pushed = true;
            }
            self.push_undo();
        }
        let Some(drag) = self.interaction.primitive_gizmo_drag() else {
            return;
        };
        let updates: Vec<(NodeId, PhysicalVertex, i32)> = drag
            .y_vertices
            .iter()
            .map(|entry| {
                (
                    entry.room,
                    entry.vertex.clone(),
                    snap_height(entry.pre_drag_y + world_delta),
                )
            })
            .collect();
        for (room, vertex, new_y) in updates {
            let Some(grid) = self.room_floor_grid_mut(room) else {
                continue;
            };
            apply_vertex_height(grid, &vertex, new_y);
        }
        self.mark_dirty();
    }

    pub(crate) fn apply_primitive_gizmo_grid_preview(&mut self) {
        let Some(grid_drag) = self
            .interaction
            .primitive_gizmo_drag()
            .and_then(|drag| drag.grid.clone())
        else {
            return;
        };

        self.project = grid_drag.base_project.clone();
        self.dirty = grid_drag.base_dirty;
        if grid_drag.current_delta == [0, 0] {
            self.select_geometry_primitives(grid_drag.room, grid_drag.targets);
            return;
        }

        remove_primitive_faces_from_project(
            &mut self.project,
            &grid_drag.targets,
            self.active_floor,
        );
        let mut selected_primitives = Vec::new();
        let active_floor = self.active_floor;
        {
            let scene = self.project.active_scene_mut();
            let Some(node) = scene.node(grid_drag.room) else {
                self.interaction = Interaction::Idle;
                self.status = "Move target room no longer exists".to_string();
                return;
            };
            let NodeKind::Section { .. } = &node.kind else {
                self.interaction = Interaction::Idle;
                self.status = "Move target is not a Section".to_string();
                return;
            };
            for cell in &grid_drag.cells {
                let _ = extend_room_grid_to_include_preserving_child_positions(
                    scene,
                    grid_drag.room,
                    grid_drag.source_origin[0] + grid_drag.current_delta[0] + cell.offset[0],
                    grid_drag.source_origin[1] + grid_drag.current_delta[1] + cell.offset[1],
                    active_floor,
                );
            }
            let Some(node) = scene.node_mut(grid_drag.room) else {
                self.interaction = Interaction::Idle;
                self.status = "Move target room no longer exists".to_string();
                return;
            };
            let NodeKind::Section { grid } = &mut node.kind else {
                self.interaction = Interaction::Idle;
                self.status = "Move target is not a Section".to_string();
                return;
            };
            let floor_idx = active_floor.min(grid.floor_count().saturating_sub(1));
            let grid = grid
                .floor_mut(floor_idx)
                .expect("floor index clamped to range");
            for cell in grid_drag.cells {
                let wcx = grid_drag.source_origin[0] + grid_drag.current_delta[0] + cell.offset[0];
                let wcz = grid_drag.source_origin[1] + grid_drag.current_delta[1] + cell.offset[1];
                let Some((sx, sz)) = grid.world_cell_to_array(wcx, wcz) else {
                    continue;
                };
                let Some(index) = grid.sector_index(sx, sz) else {
                    continue;
                };
                let Some(fragment) = cell.sector else {
                    continue;
                };
                let target = grid.sectors[index].get_or_insert_with(GridSector::empty);
                merge_primitive_fragment(
                    target,
                    fragment,
                    grid_drag.room,
                    sx,
                    sz,
                    &mut selected_primitives,
                );
            }
        }
        self.select_geometry_primitives(grid_drag.room, selected_primitives);
    }

    pub(crate) fn end_primitive_gizmo_drag(&mut self) {
        let Some(drag) = self.interaction.take_primitive_gizmo_drag() else {
            return;
        };
        if let Some(grid_drag) = drag.grid {
            if grid_drag.current_delta == [0, 0] {
                self.project = grid_drag.base_project;
                self.dirty = grid_drag.base_dirty;
                self.select_geometry_primitives(grid_drag.room, grid_drag.targets);
                return;
            }
            let moved = grid_drag.targets.len();
            let delta = grid_drag.current_delta;
            self.history.record(grid_drag.base_project);
            self.status = if moved == 1 {
                format!("Moved 1 primitive by {},{} cells", delta[0], delta[1])
            } else {
                format!(
                    "Moved {moved} primitives by {},{} cells",
                    delta[0], delta[1]
                )
            };
            self.mark_dirty();
        } else if drag.snapshot_pushed {
            let moved = drag.targets.len();
            let delta = drag.current_steps * HEIGHT_QUANTUM;
            self.status = if moved == 1 {
                format!("Moved 1 primitive by {delta} height units")
            } else {
                format!("Moved {moved} primitives by {delta} height units")
            };
        }
    }

    #[cfg(test)]
    pub(crate) fn begin_node_gizmo_drag(
        &mut self,
        axis: PrimitiveGizmoAxis,
        rect: Rect,
        pointer: Pos2,
    ) -> bool {
        self.begin_node_gizmo_handle_drag(NodeGizmoHandle::Axis(axis), rect, pointer)
    }

    pub(crate) fn begin_node_gizmo_handle_drag(
        &mut self,
        handle: NodeGizmoHandle,
        rect: Rect,
        pointer: Pos2,
    ) -> bool {
        let mode = self.transform_gizmo_mode;
        let ids = self.selected_node_gizmo_targets();
        if ids.is_empty() {
            return false;
        }
        if mode != TransformGizmoMode::Move
            && !matches!(
                (mode, handle),
                (TransformGizmoMode::Rotate, NodeGizmoHandle::Axis(_))
                    | (TransformGizmoMode::Scale, NodeGizmoHandle::Axis(_))
                    | (TransformGizmoMode::Scale, NodeGizmoHandle::BoxFace(_))
            )
        {
            return false;
        }

        let start_plane_hit =
            if let (TransformGizmoMode::Move, NodeGizmoHandle::Plane(plane)) = (mode, handle) {
                let Some((pivot, _)) = self.node_gizmo_bounds_3d(&ids) else {
                    return false;
                };
                let Some((origin, dir)) = self.camera_ray_for_pointer(rect, pointer) else {
                    return false;
                };
                let normal = plane.normal_axis();
                ray_intersects_axis_aligned_plane(origin, dir, normal, pivot[normal.index()])
            } else {
                None
            };
        if matches!(
            (mode, handle),
            (TransformGizmoMode::Move, NodeGizmoHandle::Plane(_))
        ) && start_plane_hit.is_none()
        {
            return false;
        }

        let mut rotate_state: Option<NodeGizmoRotateDrag> = None;
        let screen_axis_delta = match (mode, handle) {
            (TransformGizmoMode::Rotate, NodeGizmoHandle::Axis(axis)) => {
                // Rotation tracks the angle the pointer sweeps around
                // the projected pivot rather than linear motion along
                // a fixed screen direction, so the ring can be grabbed
                // anywhere and circled from any camera angle.
                let Some(ring) = self.node_rotation_gizmo_screen_ring_for_axis(rect, axis) else {
                    return false;
                };
                let grab = pointer - ring.center;
                if grab.length_sq() < 64.0 {
                    return false;
                }
                let winding = ring_screen_winding(&ring);
                if winding == 0.0 {
                    return false;
                }
                rotate_state = Some(NodeGizmoRotateDrag {
                    center: ring.center,
                    winding,
                    last_angle: grab.y.atan2(grab.x),
                    accumulated: 0.0,
                    space: self.gizmo_space.rotation_space(),
                });
                // Unused by the angular path; non-degenerate so the
                // shared length check below stays inert.
                Vec2::new(64.0, 0.0)
            }
            (_, NodeGizmoHandle::Axis(axis)) => {
                let Some(screen_axis) = self
                    .node_gizmo_screen_axes(rect)
                    .into_iter()
                    .find(|candidate| candidate.axis == axis)
                else {
                    return false;
                };
                screen_axis.end - screen_axis.start
            }
            (TransformGizmoMode::Move, NodeGizmoHandle::Plane(plane)) => {
                let Some(screen_plane) = self
                    .node_gizmo_screen_planes(rect)
                    .into_iter()
                    .find(|candidate| candidate.plane == plane)
                else {
                    return false;
                };
                screen_plane.corners[2] - screen_plane.corners[0]
            }
            (TransformGizmoMode::Scale, NodeGizmoHandle::BoxFace(face)) => {
                let Some(handle) = self
                    .box_prop_face_screen_handles(rect)
                    .into_iter()
                    .find(|candidate| candidate.face == face)
                else {
                    return false;
                };
                handle.end - handle.center
            }
            (_, NodeGizmoHandle::Plane(_) | NodeGizmoHandle::BoxFace(_)) => return false,
        };
        if screen_axis_delta.length_sq() < 64.0 {
            return false;
        }
        let move_axis_world = match (mode, handle) {
            (TransformGizmoMode::Move, NodeGizmoHandle::Axis(axis)) => {
                basis_column(&self.node_gizmo_basis(&ids), axis.index())
            }
            _ => [0.0; 3],
        };

        let scene = self.project.active_scene();
        let group_roots: Vec<NodeId> = ids
            .iter()
            .copied()
            .filter(|id| {
                scene
                    .node(*id)
                    .is_some_and(|node| matches!(node.kind, NodeKind::Group))
            })
            .collect();
        let group_pivot = (!group_roots.is_empty())
            .then(|| {
                self.node_gizmo_bounds_3d(&ids)
                    .map(|(pivot, _)| pivot.map(f64::from))
            })
            .flatten();
        let group_count = group_roots.len();
        let mut target_ids = ids;
        if mode == TransformGizmoMode::Move {
            for node in scene.nodes() {
                if !matches!(node.kind, NodeKind::Group)
                    && group_roots
                        .iter()
                        .any(|group| scene.is_descendant_of(node.id, *group))
                    && node_kind_supports_transform_gizmo(&node.kind, mode)
                    && !target_ids.contains(&node.id)
                {
                    target_ids.push(node.id);
                }
            }
        }
        let targets: Vec<NodeGizmoTarget> = target_ids
            .into_iter()
            .filter(|id| {
                !scene
                    .node(*id)
                    .is_some_and(|node| matches!(node.kind, NodeKind::Group))
            })
            .filter_map(|id| {
                scene.node(id).map(|node| NodeGizmoTarget {
                    node: id,
                    start_translation: node.transform.translation,
                    start_rotation_degrees: node.transform.rotation_degrees,
                    start_image_prop_size: match &node.kind {
                        NodeKind::ImageProp { width, height, .. } => Some([*width, *height]),
                        _ => None,
                    },
                    start_box_prop_vertices: match &node.kind {
                        NodeKind::BoxProp { vertices, .. } => Some(*vertices),
                        _ => None,
                    },
                    start_cylinder_prop_geometry: match &node.kind {
                        NodeKind::CylinderProp { geometry, .. } => Some(*geometry),
                        _ => None,
                    },
                    start_arch_prop_geometry: match &node.kind {
                        NodeKind::ArchProp { geometry, .. } => Some(*geometry),
                        _ => None,
                    },
                    sector_size: node_translation_sector_size(&self.project, id),
                })
            })
            .collect();
        let mut group_brush_indices = Vec::new();
        for group in &group_roots {
            for index in scene.brush_indices_in_group(*group, true) {
                if !group_brush_indices.contains(&index) {
                    group_brush_indices.push(index);
                }
            }
        }
        let group_brushes: Vec<GroupBrushGizmoTarget> = group_brush_indices
            .into_iter()
            .filter_map(|index| {
                scene
                    .brushes
                    .get(index)
                    .cloned()
                    .map(|start| GroupBrushGizmoTarget { index, start })
            })
            .collect();
        if targets.is_empty() && group_brushes.is_empty() {
            return false;
        }

        self.interaction = Interaction::NodeGizmo(NodeGizmoDrag {
            mode,
            handle,
            start_pointer: pointer,
            screen_axis: screen_axis_delta,
            start_plane_hit,
            current_plane_delta_world: [0.0, 0.0, 0.0],
            move_axis_world,
            rotate: rotate_state,
            targets,
            group_brushes,
            group_pivot,
            group_count,
            current_steps: 0,
            snapshot_pushed: false,
            free: false,
        });
        true
    }

    pub(crate) fn update_node_gizmo_drag(&mut self, rect: Rect, pointer: Pos2, free: bool) {
        if let Some(drag) = self.interaction.node_gizmo_drag_mut() {
            drag.free = free;
        }
        let Some(drag) = self.interaction.node_gizmo_drag() else {
            return;
        };
        if let (TransformGizmoMode::Move, NodeGizmoHandle::Plane(plane)) = (drag.mode, drag.handle)
        {
            let Some(start_hit) = drag.start_plane_hit else {
                return;
            };
            let Some((origin, dir)) = self.camera_ray_for_pointer(rect, pointer) else {
                return;
            };
            let normal = plane.normal_axis();
            let Some(hit) =
                ray_intersects_axis_aligned_plane(origin, dir, normal, start_hit[normal.index()])
            else {
                return;
            };
            let [a, b] = plane.axes();
            let mut delta = [0.0, 0.0, 0.0];
            delta[a.index()] = hit[a.index()] - start_hit[a.index()];
            delta[b.index()] = hit[b.index()] - start_hit[b.index()];
            if vec3_nearly_equal(delta, drag.current_plane_delta_world) {
                return;
            }
            if let Some(drag) = self.interaction.node_gizmo_drag_mut() {
                drag.current_plane_delta_world = delta;
            }
            self.apply_node_gizmo_drag();
            return;
        }

        if let Some(rotate) = drag.rotate {
            // Angular tracking: one step per degree the pointer sweeps
            // around the projected pivot. Unwrapping per update lets a
            // drag wind through multiple revolutions.
            let current_steps = drag.current_steps;
            let rotates_brush_group = !drag.group_brushes.is_empty();
            let offset = pointer - rotate.center;
            if offset.length_sq() < 16.0 {
                return;
            }
            let angle = offset.y.atan2(offset.x);
            let accumulated = rotate.accumulated + wrap_angle_radians(angle - rotate.last_angle);
            if let Some(drag) = self.interaction.node_gizmo_drag_mut() {
                if let Some(rotate) = drag.rotate.as_mut() {
                    rotate.last_angle = angle;
                    rotate.accumulated = accumulated;
                }
            }
            let raw_degrees = f64::from(rotate.winding * accumulated.to_degrees());
            let steps = if rotates_brush_group {
                snap_brush_rotation_degrees(raw_degrees)
            } else {
                raw_degrees.round() as i32
            };
            if steps == current_steps {
                return;
            }
            if let Some(drag) = self.interaction.node_gizmo_drag_mut() {
                drag.current_steps = steps;
            }
            self.apply_node_gizmo_drag();
            return;
        }

        let axis_len_sq = drag.screen_axis.length_sq();
        if axis_len_sq < f32::EPSILON {
            return;
        }
        let pixels_per_step = match drag.mode {
            TransformGizmoMode::Move => 4.0,
            TransformGizmoMode::Scale => 8.0,
            // Rotate never reaches the linear path; it tracks the
            // swept pointer angle above.
            TransformGizmoMode::Rotate => return,
        };
        let pointer_delta = pointer - drag.start_pointer;
        let unit = drag.screen_axis / axis_len_sq.sqrt();
        let steps = (pointer_delta.dot(unit) / pixels_per_step).round() as i32;
        if steps == drag.current_steps {
            return;
        }
        if let Some(drag) = self.interaction.node_gizmo_drag_mut() {
            drag.current_steps = steps;
        }
        self.apply_node_gizmo_drag();
    }

    pub(crate) fn apply_node_gizmo_drag(&mut self) {
        let Some(drag) = self.interaction.node_gizmo_drag() else {
            return;
        };
        if !node_gizmo_drag_has_motion(drag) && !drag.snapshot_pushed {
            return;
        }
        let snapshot_pushed = drag.snapshot_pushed;
        let handle = drag.handle;
        let steps = drag.current_steps;
        let plane_delta_world = drag.current_plane_delta_world;
        // World-unit nodes (sector_size == 1, i.e. BSP scenes) move on the
        // brush grid; Shift (free) drops to single-unit precision.
        let free = drag.free;
        let world_quantum = if free {
            1
        } else {
            i32::from(self.snap_units.max(1))
        };
        let move_axis_world = drag.move_axis_world;
        let rotation_space = drag
            .rotate
            .map(|rotate| rotate.space)
            .unwrap_or(RotationSpace::Global);
        let mode = drag.mode;
        let targets = drag.targets.clone();
        let group_brushes = drag.group_brushes.clone();
        let group_has_brushes = !group_brushes.is_empty();
        let group_pivot = drag.group_pivot;
        let brush_delta = if mode == TransformGizmoMode::Move {
            match handle {
                NodeGizmoHandle::Axis(_) => std::array::from_fn(|axis| {
                    (move_axis_world[axis] * steps as f32 * world_quantum as f32).round() as i32
                }),
                NodeGizmoHandle::Plane(plane) => {
                    let mut delta = [0; 3];
                    for axis in plane.axes() {
                        let index = axis.index();
                        delta[index] = (plane_delta_world[index] / world_quantum as f32).round()
                            as i32
                            * world_quantum;
                    }
                    delta
                }
                NodeGizmoHandle::BoxFace(_) => [0; 3],
            }
        } else {
            [0; 3]
        };
        let group_brush_previews = if mode == TransformGizmoMode::Move {
            None
        } else if let (Some(pivot), NodeGizmoHandle::Axis(axis)) = (group_pivot, handle) {
            if mode == TransformGizmoMode::Rotate
                && steps.rem_euclid(BRUSH_ROTATION_SNAP_DEGREES) != 0
            {
                self.status = format!(
                    "Rotate {steps}° rejected: brush groups snap every {}°",
                    BRUSH_ROTATION_SNAP_DEGREES
                );
                return;
            }
            let map = group_brush_transform_map(mode, axis, steps);
            let snap_step = i32::from(self.snap_units.max(1));
            let mut previews = Vec::with_capacity(group_brushes.len());
            for target in &group_brushes {
                let mut brush = target.start.clone();
                let faces: Vec<usize> = (0..brush.faces.len()).collect();
                if brush.transform_selected_snapped(&faces, &[], pivot, map, 0.5, snap_step) == 0
                    || !brush.is_pickable()
                {
                    self.status = if mode == TransformGizmoMode::Rotate {
                        format!(
                            "Rotate {steps}° rejected: result would not form a valid solid on Grid {}",
                            self.snap_units
                        )
                    } else {
                        format!("Scale rejected on Grid {}", self.snap_units)
                    };
                    return;
                }
                if mode == TransformGizmoMode::Rotate {
                    let Some(snapped) = brush.snapped_solved_to_grid(snap_step) else {
                        self.status = format!(
                            "Rotate {steps}° rejected: result cannot be re-snapped as a valid solid on Grid {}",
                            self.snap_units
                        );
                        return;
                    };
                    brush = snapped;
                }
                previews.push((target.index, brush));
            }
            Some(previews)
        } else {
            None
        };
        if !snapshot_pushed {
            if let Some(drag) = self.interaction.node_gizmo_drag_mut() {
                drag.snapshot_pushed = true;
            }
            self.push_undo();
        }
        let texture_lock = self.brush_texture_lock;
        let scene = self.project.active_scene_mut();
        for target in targets {
            let Some(node) = scene.node_mut(target.node) else {
                continue;
            };
            match mode {
                TransformGizmoMode::Move => match handle {
                    NodeGizmoHandle::Axis(_) => {
                        node.transform.translation = node_gizmo_translation(
                            node,
                            target.start_translation,
                            move_axis_world,
                            steps,
                            target.sector_size,
                            world_quantum,
                        );
                    }
                    NodeGizmoHandle::Plane(plane) => {
                        node.transform.translation = node_gizmo_plane_translation(
                            node,
                            target.start_translation,
                            plane,
                            plane_delta_world,
                            target.sector_size,
                            world_quantum,
                        );
                    }
                    NodeGizmoHandle::BoxFace(_) => {}
                },
                TransformGizmoMode::Rotate => {
                    if let NodeGizmoHandle::Axis(axis) = handle {
                        node.transform.rotation_degrees = node_gizmo_rotation(
                            node,
                            target.start_rotation_degrees,
                            axis,
                            steps,
                            rotation_space,
                        );
                    }
                }
                TransformGizmoMode::Scale => match handle {
                    NodeGizmoHandle::Axis(axis) => apply_node_gizmo_scale(
                        node,
                        target.start_image_prop_size,
                        target.start_box_prop_vertices,
                        target.start_cylinder_prop_geometry,
                        target.start_arch_prop_geometry,
                        axis,
                        steps,
                    ),
                    NodeGizmoHandle::BoxFace(face) => apply_box_prop_face_gizmo_resize(
                        node,
                        target.start_translation,
                        target.start_box_prop_vertices,
                        face,
                        steps,
                        target.sector_size,
                    ),
                    NodeGizmoHandle::Plane(_) => {}
                },
            }
            if let NodeKind::ArchProp { geometry, .. } = &node.kind {
                snap_arch_prop_transform(&mut node.transform, *geometry, target.sector_size);
            }
        }
        if mode == TransformGizmoMode::Move {
            for target in group_brushes {
                let mut brush = target.start;
                if texture_lock {
                    brush.translate_with_uv_lock(
                        brush_delta,
                        psxed_project::brush::BRUSH_UV_UNITS_PER_TEXEL,
                    );
                } else {
                    brush.translate(brush_delta);
                }
                if let Some(destination) = scene.brushes.get_mut(target.index) {
                    *destination = brush;
                }
            }
        } else if let Some(previews) = group_brush_previews {
            for (index, brush) in previews {
                if let Some(destination) = scene.brushes.get_mut(index) {
                    *destination = brush;
                }
            }
        }
        self.mark_dirty();
        if mode == TransformGizmoMode::Rotate {
            let snap = if group_has_brushes {
                BRUSH_ROTATION_SNAP_DEGREES
            } else {
                1
            };
            self.status = format!("Rotate {steps:+}° on {} (snap {snap}°)", handle.label());
        }
    }

    pub(crate) fn end_node_gizmo_drag(&mut self) {
        let Some(drag) = self.interaction.take_node_gizmo_drag() else {
            return;
        };
        if !drag.snapshot_pushed {
            return;
        }
        let handle = drag.handle.label();
        let moved = drag.targets.len() + drag.group_count;
        let action = match drag.mode {
            TransformGizmoMode::Move => "Moved",
            TransformGizmoMode::Rotate => "Rotated",
            TransformGizmoMode::Scale => "Scaled",
        };
        let amount = if drag.mode == TransformGizmoMode::Rotate {
            let snap = if drag.group_brushes.is_empty() {
                1
            } else {
                BRUSH_ROTATION_SNAP_DEGREES
            };
            format!(" {}° (snap {snap}°)", drag.current_steps)
        } else {
            String::new()
        };
        self.status = if moved == 1 {
            format!("{action} 1 node{amount} on {handle}")
        } else {
            format!("{action} {moved} nodes{amount} on {handle}")
        };
    }

    pub(crate) fn primitive_drag_targets(&self, target: Selection) -> Vec<Selection> {
        if self.floor_face_sector_is_selected(target).is_some() {
            return self
                .selected_sector_faces()
                .into_iter()
                .map(Selection::Face)
                .collect();
        }
        if self.primitive_is_selected(target) {
            self.selected_primitive_targets()
        } else {
            vec![target]
        }
    }

    pub(crate) fn drag_vertices_for_targets(&self, targets: &[Selection]) -> Vec<DragVertex> {
        let mut vertices = Vec::new();
        for target in targets {
            let Some(grid) = self.room_grid_view(target.room()) else {
                continue;
            };
            let Some(seeds) = drag_corner_seeds(*target) else {
                continue;
            };
            for seed in seeds {
                let Some(vertex) = vertex_for_seed(grid, seed, self.vertex_connectivity) else {
                    continue;
                };
                if vertices
                    .iter()
                    .any(|entry: &DragVertex| entry.room == target.room() && entry.vertex == vertex)
                {
                    continue;
                }
                vertices.push(DragVertex {
                    room: target.room(),
                    pre_drag_y: vertex.world[1],
                    vertex,
                });
            }
        }
        vertices
    }

    /// Promote the hovered primitive to a selection. When the
    /// hover is a face, also pre-load `selected_resource` with
    /// its material so the resource panel surfaces it without a
    /// second click. Edge / vertex modes don't pre-load -- the
    /// inspector renders directly from the selection.
    /// Promote `node` to the active selected node, clearing
    /// any grid primitive selection. Mirrors `commit_face_selection`
    /// for entity bounds -- keeps the inspector and scene tree
    /// in sync with the viewport click.
    pub(crate) fn commit_node_selection(&mut self, node: NodeId) {
        self.replace_node_selection(node);
        self.clear_primitive_selection_state();
        self.clear_resource_selection_state();
        self.clear_sector_selection();
        self.clear_brush_selection();
        let scene = self.project.active_scene();
        if let Some(n) = scene.node(node) {
            self.status = format!("Selected {} '{}'", n.kind.label(), n.name);
        } else {
            self.status = format!("Selected node #{}", node.raw());
        }
    }

    pub(crate) fn commit_face_selection(&mut self, modifiers: egui::Modifiers) {
        match self.selection.hovered_primitive {
            Some(selection) => {
                self.apply_primitive_selection_modifiers(selection, modifiers);
            }
            None => {
                // Clicked empty space: clear every selection domain.
                self.clear_primitive_selection_state();
                self.clear_resource_selection_state();
                self.clear_sector_selection();
                self.clear_brush_selection();
                self.clear_node_selection_state();
                self.status = "Cleared selection".to_string();
            }
        }
    }

    pub(crate) fn floor_face_sector_is_selected(
        &self,
        selection: Selection,
    ) -> Option<SectorSelection> {
        let Selection::Face(face) = selection else {
            return None;
        };
        if !matches!(face.kind, FaceKind::Floor) {
            return None;
        }
        let sector = (face.room, face.sx, face.sz);
        self.selection
            .selected_sectors
            .contains(&sector)
            .then_some(sector)
    }

    pub(crate) fn select_wall_face_span(
        &mut self,
        selection: Selection,
        modifiers: egui::Modifiers,
    ) -> bool {
        let Selection::Face(current) = selection else {
            return false;
        };
        let FaceKind::Wall { dir, stack } = current.kind else {
            return false;
        };
        let Some(anchor) = self.wall_face_selection_anchor() else {
            return false;
        };
        let FaceKind::Wall {
            dir: anchor_dir,
            stack: anchor_stack,
        } = anchor.kind
        else {
            return false;
        };
        if anchor.room != current.room || anchor_dir != dir || anchor_stack != stack {
            return false;
        }

        let Some((min_x, max_x, min_z, max_z)) = wall_span_bounds(anchor, current, dir) else {
            return false;
        };
        let selections =
            self.existing_wall_span_faces(current.room, dir, stack, min_x, max_x, min_z, max_z);
        if selections.is_empty() {
            return false;
        }

        let additive = modifiers.command || modifiers.ctrl;
        self.clear_sector_selection();
        self.clear_node_selection_state();
        if !additive {
            self.selection.selected_primitives.clear();
        }
        for span_selection in selections {
            self.push_selected_primitive_unique(span_selection);
        }
        self.selection.selected_primitive = Some(selection);
        self.update_primitive_resource_selection();
        self.status = match self.selection.selected_primitives.len() {
            0 => "Cleared primitive selection".to_string(),
            1 => format!("Selected {}", describe_selection(selection)),
            count => format!("Selected {count} walls"),
        };
        true
    }

    pub(crate) fn select_horizontal_face_rect(
        &mut self,
        selection: Selection,
        modifiers: egui::Modifiers,
    ) -> bool {
        let Selection::Face(current) = selection else {
            return false;
        };
        if !matches!(current.kind, FaceKind::Floor | FaceKind::Ceiling) {
            return false;
        }
        let Some(anchor) = self.horizontal_face_selection_anchor(current.kind) else {
            return false;
        };
        if anchor.room != current.room {
            return false;
        }

        let min_x = anchor.sx.min(current.sx);
        let max_x = anchor.sx.max(current.sx);
        let min_z = anchor.sz.min(current.sz);
        let max_z = anchor.sz.max(current.sz);
        let selections = self.existing_horizontal_rect_faces(
            current.room,
            current.kind,
            min_x,
            max_x,
            min_z,
            max_z,
        );
        if selections.is_empty() {
            return false;
        }

        let additive = modifiers.command || modifiers.ctrl;
        self.clear_sector_selection();
        self.clear_node_selection_state();
        if !additive {
            self.selection.selected_primitives.clear();
        }
        for rect_selection in selections {
            self.push_selected_primitive_unique(rect_selection);
        }
        self.selection.selected_primitive = Some(selection);
        self.update_primitive_resource_selection();
        self.status = match self.selection.selected_primitives.len() {
            0 => "Cleared primitive selection".to_string(),
            1 => format!("Selected {}", describe_selection(selection)),
            count => format!("Selected {count} {}", horizontal_face_plural(current.kind)),
        };
        true
    }

    pub(crate) fn horizontal_face_selection_anchor(&self, kind: FaceKind) -> Option<FaceRef> {
        self.selection
            .selected_primitives
            .iter()
            .copied()
            .find_map(|selection| selection_horizontal_face(selection, kind))
            .or_else(|| {
                self.selection
                    .selected_primitive
                    .and_then(|selection| selection_horizontal_face(selection, kind))
            })
    }

    pub(crate) fn existing_horizontal_rect_faces(
        &self,
        room: NodeId,
        kind: FaceKind,
        min_x: u16,
        max_x: u16,
        min_z: u16,
        max_z: u16,
    ) -> Vec<Selection> {
        let Some(grid) = self.room_grid_view(room) else {
            return Vec::new();
        };
        let mut selections = Vec::new();
        for sx in min_x..=max_x {
            for sz in min_z..=max_z {
                let has_face = grid.sector(sx, sz).is_some_and(|sector| match kind {
                    FaceKind::Floor => sector.floor.is_some(),
                    FaceKind::Ceiling => sector.ceiling.is_some(),
                    FaceKind::Wall { .. } => false,
                });
                if has_face {
                    selections.push(Selection::Face(FaceRef { room, sx, sz, kind }));
                }
            }
        }
        selections
    }

    pub(crate) fn wall_face_selection_anchor(&self) -> Option<FaceRef> {
        self.selection
            .selected_primitives
            .iter()
            .copied()
            .find_map(selection_wall_face)
            .or_else(|| {
                self.selection
                    .selected_primitive
                    .and_then(selection_wall_face)
            })
    }

    pub(crate) fn existing_wall_span_faces(
        &self,
        room: NodeId,
        dir: GridDirection,
        stack: u8,
        min_x: u16,
        max_x: u16,
        min_z: u16,
        max_z: u16,
    ) -> Vec<Selection> {
        let Some(grid) = self.room_grid_view(room) else {
            return Vec::new();
        };
        let mut selections = Vec::new();
        for sx in min_x..=max_x {
            for sz in min_z..=max_z {
                let has_wall = grid
                    .sector(sx, sz)
                    .is_some_and(|sector| sector.walls.get(dir).get(stack as usize).is_some());
                if has_wall {
                    selections.push(Selection::Face(FaceRef {
                        room,
                        sx,
                        sz,
                        kind: FaceKind::Wall { dir, stack },
                    }));
                }
            }
        }
        selections
    }

    pub(crate) fn select_edge_path(
        &mut self,
        selection: Selection,
        modifiers: egui::Modifiers,
    ) -> bool {
        let Selection::Edge(current) = selection else {
            return false;
        };
        let Some(anchor) = self.edge_selection_anchor() else {
            return false;
        };
        let Some(path) = self.edge_path_between(anchor, current) else {
            return false;
        };
        if path.is_empty() {
            return false;
        }

        let additive = modifiers.command || modifiers.ctrl;
        self.clear_sector_selection();
        self.clear_node_selection_state();
        if !additive {
            self.selection.selected_primitives.clear();
        }
        for edge in path {
            self.push_selected_primitive_unique(Selection::Edge(edge));
        }
        self.selection.selected_primitive = Some(selection);
        self.update_primitive_resource_selection();
        self.status = match self.selection.selected_primitives.len() {
            0 => "Cleared primitive selection".to_string(),
            1 => format!("Selected {}", describe_selection(selection)),
            count => format!("Selected {count} edges"),
        };
        true
    }

    pub(crate) fn edge_selection_anchor(&self) -> Option<EdgeRef> {
        self.selection
            .selected_primitives
            .iter()
            .copied()
            .find_map(selection_edge)
            .or_else(|| self.selection.selected_primitive.and_then(selection_edge))
    }

    pub(crate) fn edge_path_between(
        &self,
        anchor: EdgeRef,
        current: EdgeRef,
    ) -> Option<Vec<EdgeRef>> {
        if anchor.room != current.room {
            return None;
        }
        let kind = edge_path_kind(anchor);
        if edge_path_kind(current) != kind {
            return None;
        }
        let grid = self.room_grid_view(anchor.room)?;
        let mut candidates = Vec::new();
        for selection in self.all_primitive_selections_in_room(anchor.room, SelectionMode::Edge) {
            let Some(edge) = selection_edge(selection) else {
                continue;
            };
            if edge_path_kind(edge) != kind {
                continue;
            }
            let Some(segment) = edge_world_segment(grid, edge) else {
                continue;
            };
            candidates.push((edge, segment));
        }

        let start = candidates.iter().position(|(edge, _)| *edge == anchor)?;
        let end = candidates.iter().position(|(edge, _)| *edge == current)?;
        if start == end {
            return Some(vec![current]);
        }

        let mut visited = vec![false; candidates.len()];
        let mut previous: Vec<Option<usize>> = vec![None; candidates.len()];
        let mut queue = VecDeque::new();
        visited[start] = true;
        queue.push_back(start);

        while let Some(index) = queue.pop_front() {
            if index == end {
                break;
            }
            for next in 0..candidates.len() {
                if visited[next] {
                    continue;
                }
                if !edge_segments_touch(candidates[index].1, candidates[next].1) {
                    continue;
                }
                visited[next] = true;
                previous[next] = Some(index);
                queue.push_back(next);
            }
        }

        if !visited[end] {
            return None;
        }
        let mut path = Vec::new();
        let mut index = end;
        loop {
            path.push(candidates[index].0);
            if index == start {
                break;
            }
            index = previous[index]?;
        }
        path.reverse();
        Some(path)
    }

    pub(crate) fn apply_primitive_selection_modifiers(
        &mut self,
        selection: Selection,
        modifiers: egui::Modifiers,
    ) {
        let toggle = modifiers.command || modifiers.ctrl;
        if modifiers.shift && self.select_horizontal_face_rect(selection, modifiers) {
            return;
        }
        if modifiers.shift && self.select_wall_face_span(selection, modifiers) {
            return;
        }
        if modifiers.shift && self.select_edge_path(selection, modifiers) {
            return;
        }

        self.clear_sector_selection();
        self.clear_node_selection_state();
        if modifiers.shift {
            if self.selection.selected_primitives.is_empty() {
                if let Some(current) = self.selection.selected_primitive {
                    self.selection.selected_primitives.push(current);
                }
            }
            self.push_selected_primitive_unique(selection);
        } else if toggle {
            if self.selection.selected_primitives.is_empty() {
                if let Some(current) = self.selection.selected_primitive {
                    self.selection.selected_primitives.push(current);
                }
            }
            if let Some(index) = self
                .selection
                .selected_primitives
                .iter()
                .position(|candidate| *candidate == selection)
            {
                self.selection.selected_primitives.remove(index);
                self.selection.selected_primitive =
                    self.selection.selected_primitives.last().copied();
            } else {
                self.push_selected_primitive_unique(selection);
            }
        } else {
            self.replace_primitive_selection(selection);
        }

        if self.selection.selected_primitives.is_empty() {
            self.clear_primitive_selection_state();
            self.clear_resource_selection_state();
            self.status = "Cleared primitive selection".to_string();
            return;
        }

        self.update_primitive_resource_selection();
        self.status = match self.selection.selected_primitives.len() {
            0 => "Cleared primitive selection".to_string(),
            1 => format!(
                "Selected {}",
                describe_selection(self.selection.selected_primitives[0])
            ),
            count => format!("Selected {count} primitives"),
        };
    }

    fn selected_floor_snap_entities(&self) -> Vec<NodeId> {
        let scene = self.project.active_scene();
        let mut entities = Vec::new();
        for selected in self.selected_node_ids_in_hierarchy() {
            let Some(entity) = owning_entity_id(scene, selected) else {
                continue;
            };
            if !entities.contains(&entity) {
                entities.push(entity);
            }
        }
        entities
    }

    pub(crate) fn can_snap_selected_entities_to_floor(&self) -> bool {
        !self.selected_floor_snap_entities().is_empty()
    }

    /// Exact supporting surface beneath an Entity floor anchor.
    ///
    /// Authored Entity transforms are raw world units and already represent
    /// the character/controller foot point. A short upward probe allowance
    /// also recovers an entity that is intersecting its floor by less than one
    /// height quantum, while remaining below any usable character ceiling.
    fn entity_floor_height(&self, entity: NodeId) -> Option<f32> {
        let scene = self.project.active_scene();
        let node = scene.node(entity)?;
        let [x, y, z] = node.transform.translation;
        if !x.is_finite() || !y.is_finite() || !z.is_finite() {
            return None;
        }

        let probe_y = f64::from(y) + f64::from(HEIGHT_QUANTUM);
        let origin = [f64::from(x), probe_y, f64::from(z)];
        let mut best: Option<f64> = None;
        for brush in &scene.brushes {
            if !brush.contents.is_solid() || brush.mover == Some(entity) {
                continue;
            }
            let Some((distance, face_index)) = brush.raycast(origin, [0.0, -1.0, 0.0]) else {
                continue;
            };
            let Some(face) = brush.faces.get(face_index) else {
                continue;
            };
            let Some(plane) = psxed_project::brush::Plane::from_points(face.points) else {
                continue;
            };
            // A downward ray can only use an outward-upward face as a floor.
            // This rejects vertical walls and the underside of solid ceilings.
            if plane.normal[1] <= 0 {
                continue;
            }
            let floor_y = probe_y - distance;
            if floor_y.is_finite() && best.is_none_or(|current| floor_y > current) {
                best = Some(floor_y);
            }
        }

        // Legacy grid rooms share the same floor-anchor contract. Prefer the
        // highest candidate when a project mixes room cells with BSP brushes.
        if let Some(room) = enclosing_room_id(scene, entity) {
            if let Some(grid) = self.room_grid_view(room) {
                if let Some(floor_y) =
                    grid.floor_height_at_room_local(x.round() as i32, z.round() as i32)
                {
                    let floor_y = f64::from(floor_y);
                    if floor_y <= probe_y && best.is_none_or(|current| floor_y > current) {
                        best = Some(floor_y);
                    }
                }
            }
        }

        best.map(|height| height as f32)
    }

    /// Move every selected Entity root to the exact supporting surface below.
    /// Child-component selections are promoted to their owning Entity; each
    /// Entity is moved once and the whole operation is one undo step.
    pub(crate) fn snap_selected_entities_to_floor(&mut self) -> bool {
        let entities = self.selected_floor_snap_entities();
        if entities.is_empty() {
            self.status = "Select an Entity or one of its components to snap to floor".to_string();
            return false;
        }

        let mut placements = Vec::new();
        let mut missing = 0usize;
        let mut already_grounded = 0usize;
        for entity in entities {
            let Some(floor_y) = self.entity_floor_height(entity) else {
                missing += 1;
                continue;
            };
            let Some(node) = self.project.active_scene().node(entity) else {
                missing += 1;
                continue;
            };
            if (node.transform.translation[1] - floor_y).abs() <= 0.001 {
                already_grounded += 1;
                continue;
            }
            placements.push((entity, floor_y));
        }

        if placements.is_empty() {
            self.status = if already_grounded > 0 && missing == 0 {
                if already_grounded == 1 {
                    "Entity is already on the floor".to_string()
                } else {
                    format!("All {already_grounded} entities are already on the floor")
                }
            } else {
                "No floor found beneath the selected entity".to_string()
            };
            return false;
        }

        self.push_undo();
        let moved = placements.len();
        for (entity, floor_y) in placements {
            if let Some(node) = self.project.active_scene_mut().node_mut(entity) {
                node.transform.translation[1] = floor_y;
            }
        }
        self.status = match (moved, missing) {
            (1, 0) => "Snapped Entity to floor".to_string(),
            (1, missing) => {
                format!("Snapped Entity to floor; {missing} had no floor beneath it")
            }
            (moved, 0) => format!("Snapped {moved} entities to floor"),
            (moved, missing) => {
                format!("Snapped {moved} entities to floor; {missing} had no floor beneath them")
            }
        };
        self.mark_dirty();
        true
    }
}

/// Total transform applied to every brush in a selected Group. Rebuilding
/// from each drag-start brush prevents incremental rounding drift.
fn group_brush_transform_map(
    mode: TransformGizmoMode,
    axis: PrimitiveGizmoAxis,
    steps: i32,
) -> [[f64; 3]; 3] {
    let mut map = [[0.0; 3]; 3];
    match mode {
        TransformGizmoMode::Rotate => {
            let (sin, cos) = f64::from(steps).to_radians().sin_cos();
            let a = axis.index();
            let (u, v) = ((a + 1) % 3, (a + 2) % 3);
            map[a][a] = 1.0;
            map[u][u] = cos;
            map[u][v] = -sin;
            map[v][u] = sin;
            map[v][v] = cos;
        }
        TransformGizmoMode::Scale => {
            let factor = (1.0 + f64::from(steps) * 0.05).clamp(0.05, 16.0);
            for (index, row) in map.iter_mut().enumerate() {
                row[index] = if index == axis.index() { factor } else { 1.0 };
            }
        }
        TransformGizmoMode::Move => {
            for (index, row) in map.iter_mut().enumerate() {
                row[index] = 1.0;
            }
        }
    }
    map
}

/// World direction of basis column `index` (the gizmo handle axis).
fn basis_column(basis: &[[f32; 3]; 3], index: usize) -> [f32; 3] {
    [basis[0][index], basis[1][index], basis[2][index]]
}

/// Screen winding of a projected rotation ring: `+1.0` when the ring's
/// increasing-angle order (a positive world rotation) advances the
/// pointer polar angle `atan2(dy, dx)` in viewport coordinates, `-1.0`
/// when it runs the other way, `0.0` for a degenerate (edge-on) ring.
fn ring_screen_winding(ring: &NodeRotationGizmoScreenRing) -> f32 {
    let mut sum = 0.0f32;
    for pair in ring.points.windows(2) {
        let a = pair[0] - ring.center;
        let b = pair[1] - ring.center;
        sum += a.x * b.y - a.y * b.x;
    }
    if sum.abs() < 1.0 {
        0.0
    } else {
        sum.signum()
    }
}

/// Wrap an angle difference into `(-PI, PI]` so per-frame pointer
/// deltas accumulate across the atan2 seam without 2*PI jumps.
pub(crate) fn wrap_angle_radians(delta: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    let mut delta = delta;
    while delta > PI {
        delta -= TAU;
    }
    while delta <= -PI {
        delta += TAU;
    }
    delta
}
