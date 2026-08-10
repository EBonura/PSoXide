use super::*;

/// The node whose geometry is selected plus that selection's cell
/// coordinates, or the reason no single target could be resolved.
type GeometryCellTargets = Result<(NodeId, Vec<(u16, u16)>), &'static str>;

fn project_center_half_2d(
    view: OrthographicView,
    center: [f32; 3],
    half: [f32; 3],
) -> ([f32; 2], [f32; 2]) {
    (view.project_f32(center), view.project_f32(half))
}

impl EditorWorkspace {
    /// Stop a transient character action/movement preview when its Animator is
    /// edited. The transient preview carries its own clip override, so leaving
    /// it alive would mask a newly selected `Editor Clip` until the project was
    /// reopened (which happened to clear the preview state).
    pub(crate) fn reconcile_character_preview_after_node_kind_edit(
        &mut self,
        edited_node: NodeId,
        before: &NodeKind,
    ) {
        if !matches!(before, NodeKind::Animator { .. }) {
            return;
        }
        let scene = self.project.active_scene();
        let Some(node) = scene.node(edited_node) else {
            return;
        };
        if node.kind == *before || !matches!(node.kind, NodeKind::Animator { .. }) {
            return;
        }
        let Some(entity) = node.parent else {
            return;
        };
        if self
            .character_motion_preview
            .is_some_and(|preview| preview.entity == entity)
        {
            self.character_motion_preview = None;
        }
    }

    pub(crate) fn preview_character_action(
        &mut self,
        selected: NodeId,
        action: psxed_project::CharacterAnimationAction,
    ) -> bool {
        let (host, animator) = {
            let scene = self.project.active_scene();
            let Some(selected_node) = scene.node(selected) else {
                return false;
            };
            let host = if matches!(selected_node.kind, NodeKind::Entity) {
                selected
            } else if matches!(selected_node.kind, NodeKind::CharacterController { .. }) {
                let Some(parent) = selected_node.parent else {
                    self.status = "Character Controller has no owning Entity".to_string();
                    return false;
                };
                parent
            } else {
                return false;
            };
            let Some(host_node) = scene.node(host) else {
                return false;
            };
            let animator = host_node.children.iter().find_map(|child| {
                scene
                    .node(*child)
                    .filter(|node| matches!(node.kind, NodeKind::Animator { .. }))
                    .map(|node| node.id)
            });
            (host, animator)
        };

        let Some(animator) = animator else {
            self.status = "Add an Animator component to preview character actions".to_string();
            return false;
        };

        let local_clip =
            self.project
                .active_scene()
                .node(animator)
                .and_then(|node| match &node.kind {
                    NodeKind::Animator { action_clips, .. } => action_clips
                        .iter()
                        .find(|binding| binding.action == action)
                        .map(|binding| binding.clip),
                    _ => None,
                });
        let context = selected_animator_clip_context(&self.project, animator, &self.project_dir);
        let clip = local_clip.or_else(|| {
            context
                .as_ref()
                .and_then(|ctx| ctx.profile_action_clips[action.to_index()])
        });
        let Some(clip) = clip else {
            self.status = format!(
                "{} has no effective animation clip; bind one on Animator",
                action.label()
            );
            return false;
        };
        let clip_name = context
            .as_ref()
            .and_then(|ctx| ctx.clips.get(clip as usize))
            .cloned()
            .unwrap_or_else(|| format!("Clip {clip}"));

        self.character_motion_preview = Some(CharacterMotionPreviewState {
            entity: host,
            action,
            clip,
            started_at: Instant::now(),
        });
        self.status = format!("Previewing {} · {clip_name}", action.label());
        false
    }

    /// Single dispatch point for primary-button clicks on the viewport.
    pub(crate) fn handle_viewport_click(
        &mut self,
        world: [f32; 2],
        hits: &[ViewportHit],
        modifiers: egui::Modifiers,
    ) {
        if self.orthographic_view != OrthographicView::Top && self.active_tool != ViewTool::Brush {
            self.status = format!(
                "{} is a BSP brush view; use Top for room-grid and node tools",
                self.orthographic_view.label()
            );
            return;
        }
        match self.active_tool {
            ViewTool::Brush => {
                if modifiers.command {
                    let point = self.brush_snap_2d(world);
                    self.brush_clip_click(point);
                } else {
                    self.select_brush_at_2d(world);
                }
            }
            ViewTool::Select => {
                if let Some(hit) = hits.iter().rev().find(|hit| hit.contains(world)) {
                    if let Some(sector) = self
                        .world_to_sector(hit.id, world)
                        .map(|(sx, sz)| (hit.id, sx, sz))
                    {
                        self.select_sector(sector, modifiers);
                    } else {
                        let visible_order = self.scene_node_order();
                        self.apply_node_selection_modifiers(hit.id, modifiers, &visible_order);
                        self.clear_primitive_selection_state();
                        self.clear_sector_selection();
                    }
                } else {
                    self.clear_resource_selection_state();
                    self.clear_sector_selection();
                }
            }
            tool => {
                let Some(room_id) = self.active_room_id() else {
                    return;
                };
                let Some((x, z)) = self.world_to_sector(room_id, world) else {
                    self.clear_sector_selection();
                    return;
                };
                if self.portal_place_active()
                    || matches!(tool, ViewTool::PaintMaterial | ViewTool::Water)
                {
                    self.clear_sector_selection();
                    if matches!(tool, ViewTool::PaintMaterial | ViewTool::Water) {
                        self.clear_primitive_selection_state();
                    }
                } else {
                    self.selection.selected_sector = Some((x, z));
                    self.selection.selected_sectors.clear();
                    self.selection.selected_sectors.insert((room_id, x, z));
                }
                self.apply_paint(tool, room_id, x, z, world);
            }
        }
    }

    /// Apply a 2D-viewport click through the same logic as a 3D
    /// click. Old behaviour kept a separate `apply_paint` body
    /// here that diverged from the 3D `run_paint_action` (no
    /// origin awareness, no wall replacement, no `PlaceKind`
    /// dispatch). Now: lift the click into editor coords, pre-
    /// compute a `picked_face` for PaintWall when the inferred
    /// edge already has a wall stack, and hand off. PaintWall uses
    /// that face's direction to add the next stack entry.
    pub(crate) fn apply_paint(
        &mut self,
        tool: ViewTool,
        room_id: NodeId,
        sx: u16,
        sz: u16,
        world: [f32; 2],
    ) {
        // 2D `world` is already in editor sector-units (the 2D
        // viewport's native space, room-centre-relative around
        // `node_world(room)`). Convert through the canonical
        // helper to get a room-local 3D position the rest of the
        // paint flow can chew on. `editor_to_room_local` is
        // origin-aware, so this stays correct after a -X / -Z grow.
        let (hit_world, picked_face) = {
            let Some(room) = self.project.active_scene().node(room_id) else {
                return;
            };
            let room_center = node_world(room);
            let Some(grid) = self.room_grid_view(room_id) else {
                return;
            };
            let editor = [world[0] - room_center[0], world[1] - room_center[1]];
            let hit = grid.editor_to_room_local(editor);

            // For PaintWall: if the inferred edge already has at
            // least one wall, hand `run_paint_action` a `FaceRef`
            // pointing at the top of the stack so the new wall uses
            // that edge instead of re-inferring from the cell center.
            // Empty edge -> None -> normal append path.
            let face = if matches!(tool, ViewTool::PaintWall) {
                let centre = grid.cell_center_world(sx, sz);
                let dir = self
                    .wall_paint_shape
                    .direction(hit[0] - centre[0], hit[2] - centre[1]);
                grid.sector(sx, sz).and_then(|sector| {
                    let walls = sector.walls.get(dir);
                    let stack = walls.len().checked_sub(1)?;
                    Some(FaceRef {
                        room: room_id,
                        sx,
                        sz,
                        kind: FaceKind::Wall {
                            dir,
                            stack: stack as u8,
                        },
                    })
                })
            } else if matches!(tool, ViewTool::PaintMaterial) {
                grid.sector(sx, sz)
                    .and_then(|sector| sector.floor.as_ref())
                    .map(|_| FaceRef {
                        room: room_id,
                        sx,
                        sz,
                        kind: FaceKind::Floor,
                    })
            } else {
                None
            };
            (hit, face)
        };

        if tool == ViewTool::PaintMaterial && self.material_paint_sampling {
            let Some(face) = picked_face else {
                self.status = "Eyedropper needs an existing floor under the cursor".to_string();
                return;
            };
            self.sample_paint_material_from_face(face);
            return;
        }

        self.run_paint_action(tool, room_id, sx, sz, picked_face, hit_world);
    }

    pub(crate) fn has_geometry_selection(&self) -> bool {
        !self.selection.selected_sectors.is_empty()
            || !self.selected_primitive_targets().is_empty()
            || (self.active_room_id().is_some() && self.selection.selected_sector.is_some())
    }

    pub(crate) fn duplicate_current_selection(&mut self) {
        if self.floating_geometry.is_some() {
            self.status = "Place or cancel the duplicate preview first".to_string();
            return;
        }
        if self.has_geometry_selection() {
            self.begin_floating_geometry_duplicate();
        } else {
            self.duplicate_selected();
        }
    }

    pub(crate) fn selected_geometry_cell_targets(&self) -> GeometryCellTargets {
        let mut targets = Vec::new();
        if !self.selection.selected_sectors.is_empty() {
            targets.extend(
                self.selection
                    .selected_sectors
                    .iter()
                    .map(|(room, sx, sz)| (*room, *sx, *sz)),
            );
        } else {
            targets.extend(
                self.selected_primitive_targets()
                    .into_iter()
                    .map(|selection| {
                        let (room, sx, sz) = selection_sector(selection);
                        (room, sx, sz)
                    }),
            );
            if targets.is_empty() {
                if let (Some(room), Some((sx, sz))) =
                    (self.active_room_id(), self.selection.selected_sector)
                {
                    targets.push((room, sx, sz));
                }
            }
        }

        if targets.is_empty() {
            return Err("Select world geometry first");
        }
        targets.sort_by_key(|(room, sx, sz)| (room.raw(), *sx, *sz));
        targets.dedup();

        let room = targets[0].0;
        if targets.iter().any(|(candidate, _, _)| *candidate != room) {
            return Err("Select geometry from one room at a time");
        }

        Ok((
            room,
            targets.into_iter().map(|(_, sx, sz)| (sx, sz)).collect(),
        ))
    }

    pub(crate) fn copy_selected_geometry(&mut self) -> Option<GeometryClipboard> {
        if self.selection.selected_sectors.is_empty()
            && !self.selected_primitive_targets().is_empty()
        {
            return self.copy_selected_primitive_geometry();
        }

        self.copy_selected_geometry_cells()
    }

    pub(crate) fn copy_selected_primitive_geometry(&mut self) -> Option<GeometryClipboard> {
        let targets = self.selected_primitive_targets();
        let (clipboard, populated) = match self.primitive_geometry_clipboard_for_targets(&targets) {
            Ok(result) => result,
            Err(message) => {
                self.status = message.to_string();
                return None;
            }
        };
        self.status = if populated == 1 {
            "Copied 1 primitive".to_string()
        } else {
            format!("Copied {populated} primitives")
        };
        Some(clipboard)
    }

    pub(crate) fn primitive_geometry_clipboard_for_targets(
        &self,
        targets: &[Selection],
    ) -> Result<(GeometryClipboard, usize), &'static str> {
        let Some(first) = targets.first().copied() else {
            return Err("Select world geometry first");
        };
        let room = first.room();
        if targets.iter().any(|selection| selection.room() != room) {
            return Err("Select geometry from one room at a time");
        }

        let Some(grid) = self.room_grid_view(room) else {
            return Err("Selected room no longer exists");
        };

        let mut min_x = i32::MAX;
        let mut min_z = i32::MAX;
        let mut max_x = i32::MIN;
        let mut max_z = i32::MIN;
        let mut staged: Vec<([i32; 2], GridSector)> = Vec::new();
        let mut populated = 0usize;
        for &selection in targets {
            let Some(fragment) = sector_fragment_for_selection(grid, selection) else {
                continue;
            };
            let (_, sx, sz) = selection_sector(selection);
            let world = [grid.origin[0] + sx as i32, grid.origin[1] + sz as i32];
            min_x = min_x.min(world[0]);
            min_z = min_z.min(world[1]);
            max_x = max_x.max(world[0]);
            max_z = max_z.max(world[1]);
            if let Some((_, cell)) = staged
                .iter_mut()
                .find(|(candidate_world, _)| *candidate_world == world)
            {
                merge_clipboard_fragment(cell, fragment);
            } else {
                staged.push((world, fragment));
            }
            populated += 1;
        }

        if populated == 0 {
            return Err("Selected primitives are empty");
        }

        let width = max_x - min_x + 1;
        let height = max_z - min_z + 1;
        let clipboard = GeometryClipboard {
            mode: GeometryClipboardMode::MergePrimitives,
            source_room: room,
            source_origin: [min_x, min_z],
            next_paste_origin: [max_x + 1, min_z],
            width,
            height,
            cells: staged
                .into_iter()
                .map(|(world, sector)| GeometryClipboardCell {
                    offset: [world[0] - min_x, world[1] - min_z],
                    sector: Some(sector),
                })
                .collect(),
            extra_floors: Vec::new(),
            lights: Vec::new(),
        };
        Ok((clipboard, populated))
    }

    pub(crate) fn copy_selected_geometry_cells(&mut self) -> Option<GeometryClipboard> {
        let (room, cells) = match self.selected_geometry_cell_targets() {
            Ok(targets) => targets,
            Err(message) => {
                self.status = message.to_string();
                return None;
            }
        };
        let Some(grid) = self.room_grid_view(room) else {
            self.status = "Selected room no longer exists".to_string();
            return None;
        };

        let mut min_x = i32::MAX;
        let mut min_z = i32::MAX;
        let mut max_x = i32::MIN;
        let mut max_z = i32::MIN;
        let mut staged = Vec::new();
        for (sx, sz) in cells {
            let world = [grid.origin[0] + sx as i32, grid.origin[1] + sz as i32];
            min_x = min_x.min(world[0]);
            min_z = min_z.min(world[1]);
            max_x = max_x.max(world[0]);
            max_z = max_z.max(world[1]);
            staged.push((world, grid.sector(sx, sz).cloned()));
        }

        let populated = staged
            .iter()
            .filter(|(_, sector)| sector.as_ref().is_some_and(GridSector::has_geometry))
            .count();
        if populated == 0 {
            self.status = "Selected cells are empty".to_string();
            return None;
        }

        let width = max_x - min_x + 1;
        let height = max_z - min_z + 1;
        let clipboard = GeometryClipboard {
            mode: GeometryClipboardMode::ReplaceCells,
            source_room: room,
            source_origin: [min_x, min_z],
            next_paste_origin: [max_x + 1, min_z],
            width,
            height,
            cells: staged
                .into_iter()
                .map(|(world, sector)| GeometryClipboardCell {
                    offset: [world[0] - min_x, world[1] - min_z],
                    sector,
                })
                .collect(),
            extra_floors: Vec::new(),
            lights: Vec::new(),
        };
        self.status = if populated == 1 {
            "Copied 1 world cell".to_string()
        } else {
            format!("Copied {populated} world cells")
        };
        Some(clipboard)
    }

    pub(crate) fn begin_floating_geometry_duplicate(&mut self) {
        let Some(clipboard) = self.copy_selected_geometry() else {
            return;
        };
        self.begin_floating_geometry(clipboard, "Duplicating world geometry beside source");
    }

    /// Enter the floating preview loop with `clipboard`. Shared by Duplicate
    /// and by stamping a prefab: the two differ only in where the cells came
    /// from and what the status line calls the operation.
    fn begin_floating_geometry(&mut self, clipboard: GeometryClipboard, lead: &str) {
        let Some(room) = self.paste_target_room(&clipboard) else {
            self.status = "No section to duplicate into".to_string();
            return;
        };
        self.floating_geometry = Some(FloatingGeometryPlacement {
            base_project: self.project.clone(),
            base_dirty: self.dirty,
            mode: clipboard.mode,
            room,
            origin: clipboard.next_paste_origin,
            width: clipboard.width,
            height: clipboard.height,
            rotation_quarters: 0,
            flip_x: false,
            flip_z: false,
            pointer_anchor_origin: None,
            pointer_anchor_placement_origin: clipboard.next_paste_origin,
            selected_cells: Vec::new(),
            selected_primitives: Vec::new(),
            cells: clipboard.cells,
            extra_floors: clipboard.extra_floors,
            lights: clipboard.lights,
            seam_walls_stripped: 0,
            elevation_offset: 0,
        });
        self.apply_floating_geometry_preview();
        self.status = format!(
            "{lead} - move cursor, R rotates, F flips, Shift+F flips vertically, PgUp/PgDn \
             raises and lowers, click places, Esc cancels"
        );
    }

    /// Tools > Prefabs. Saving names the piece; stamping picks one off disk
    /// and hands it to the same preview loop Duplicate uses, so rotate / flip /
    /// place stay exactly as they are.
    pub(crate) fn draw_prefab_menu(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Name");
            ui.text_edit_singleline(&mut self.prefab_name);
        });
        let can_save = self.has_geometry_selection() && !self.prefab_name.trim().is_empty();
        if ui
            .add_enabled(can_save, egui::Button::new("Save Selection as Prefab"))
            .on_disabled_hover_text("Select world geometry and type a name")
            .clicked()
        {
            let name = std::mem::take(&mut self.prefab_name);
            self.save_selection_as_prefab(&name);
            ui.close_menu();
        }
        if ui.button("Refresh Library").clicked() {
            self.status = match self.refresh_prefab_library() {
                Ok(count) => format!("Refreshed shared prefab library - {count} pieces"),
                Err(error) => format!("Could not refresh prefab library: {error}"),
            };
            ui.close_menu();
        }
        ui.separator();
        if self.prefab_library.is_empty() {
            ui.weak("No prefabs saved yet");
            return;
        }
        let mut stamp = None;
        for entry in &self.prefab_library {
            if ui
                .add_enabled(entry.prefab.is_some(), egui::Button::new(&entry.name))
                .on_disabled_hover_text(
                    entry
                        .load_error
                        .as_deref()
                        .unwrap_or("Prefab could not be loaded"),
                )
                .clicked()
            {
                stamp = Some(entry.path.clone());
            }
        }
        if let Some(path) = stamp {
            self.stamp_prefab(&path);
            ui.close_menu();
        }
    }

    /// Capture the current geometry selection as a named prefab.
    pub(crate) fn capture_selection_as_prefab(
        &mut self,
        name: &str,
    ) -> Option<psxed_project::Prefab> {
        let name = name.trim();
        if name.is_empty() {
            self.status = "Name the prefab first".to_string();
            return None;
        }
        let clipboard = self.copy_selected_geometry()?;
        let room = clipboard.source_room;
        let sector_size = self
            .room_grid_view(room)
            .map_or(DEFAULT_WORLD_SECTOR_SIZE, |grid| grid.sector_size);
        let base_floor = self.active_floor;

        // The selection is made on one floor, so a multi-floor piece is
        // captured as that footprint taken through the whole stack above it.
        // Asking the user to select the same shape floor by floor would be the
        // clicking this feature exists to remove.
        let mut floors = vec![psxed_project::PrefabFloor {
            relative_elevation: 0,
            cells: clipboard.cells.clone(),
        }];
        let (world_cells, base_elevation, floor_count) = {
            let Some(NodeKind::Section { grid }) = self
                .project
                .active_scene()
                .node(room)
                .map(|node| &node.kind)
            else {
                return None;
            };
            let base = grid.floor(base_floor)?;
            // `source_origin` is already in world cells, and each cell offset
            // is measured from it, so the world address is just their sum.
            // Floors are free grids with their own `origin`, so world cells are
            // the only address that means the same thing on all of them.
            let world_cells: Vec<[i32; 2]> = clipboard
                .cells
                .iter()
                .map(|cell| {
                    [
                        clipboard.source_origin[0] + cell.offset[0],
                        clipboard.source_origin[1] + cell.offset[1],
                    ]
                })
                .collect();
            (world_cells, base.elevation, grid.floor_count())
        };
        for floor_index in (base_floor + 1)..floor_count {
            let Some(NodeKind::Section { grid }) = self
                .project
                .active_scene()
                .node(room)
                .map(|node| &node.kind)
            else {
                break;
            };
            let Some(floor) = grid.floor(floor_index) else {
                break;
            };
            let cells: Vec<psxed_project::PrefabCell> = clipboard
                .cells
                .iter()
                .zip(&world_cells)
                .map(|(cell, world)| psxed_project::PrefabCell {
                    offset: cell.offset,
                    sector: floor
                        .world_cell_to_array(world[0], world[1])
                        .and_then(|(sx, sz)| floor.sector(sx, sz))
                        .cloned(),
                })
                .collect();
            // A stack that runs out of authored geometry stops the piece there
            // rather than padding it with empty floors.
            if cells.iter().all(|cell| cell.sector.is_none()) {
                break;
            }
            floors.push(psxed_project::PrefabFloor {
                relative_elevation: floor.elevation.saturating_sub(base_elevation),
                cells,
            });
        }

        Some(psxed_project::Prefab::capture(
            name,
            sector_size,
            clipboard.width,
            clipboard.height,
            clipboard.mode == GeometryClipboardMode::MergePrimitives,
            floors,
            room,
            base_floor,
            &self.project,
        ))
    }

    /// Write the current geometry selection to `editor/prefabs/<name>.ron`.
    pub(crate) fn save_selection_as_prefab(&mut self, name: &str) {
        let Some(prefab) = self.capture_selection_as_prefab(name) else {
            return;
        };
        let path = psxed_project::prefab_path(&prefab.name);
        self.status = match prefab.save_to_path(&path) {
            Ok(()) => {
                let refresh_note = self
                    .refresh_prefab_library()
                    .err()
                    .map(|error| format!("; library refresh failed: {error}"))
                    .unwrap_or_default();
                format!("Saved prefab to {}{refresh_note}", path.display())
            }
            Err(error) => format!("Could not save prefab: {error}"),
        };
    }

    /// Load a prefab and enter the floating preview loop with it. Materials
    /// are rebound to this project by name; anything the project does not have
    /// is cleared and counted so the status line can say so.
    pub(crate) fn stamp_prefab(&mut self, path: &Path) {
        let prefab = match psxed_project::Prefab::load_from_path(path) {
            Ok(prefab) => prefab,
            Err(error) => {
                self.status = format!("Could not load prefab: {error}");
                return;
            }
        };
        let Some(room) = self.active_room_id() else {
            self.status = "Select a section to stamp into".to_string();
            return;
        };
        let grid = self.room_grid_view(room);
        let origin = grid
            .as_ref()
            .map(|grid| match self.selection.selected_sector {
                Some((sx, sz)) => [grid.origin[0] + sx as i32, grid.origin[1] + sz as i32],
                None => grid.origin,
            })
            .unwrap_or([0, 0]);
        self.begin_prefab_stamp(prefab, room, origin);
    }

    pub(crate) fn drop_prefab_2d(&mut self, path: &Path, editor_world: [f32; 2]) {
        let Some(room) = self.active_room_id() else {
            self.status = "Drop the prefab over a section".to_string();
            return;
        };
        let Some(origin) = self.floating_origin_from_2d_world(room, editor_world) else {
            self.status = "Drop the prefab over a section".to_string();
            return;
        };
        self.stamp_prefab_at(path, room, origin);
    }

    pub(crate) fn drop_prefab_3d(
        &mut self,
        path: &Path,
        face_hit: Option<(FaceRef, [f32; 3])>,
        ground_hit: Option<[f32; 2]>,
    ) {
        let Some(room) = face_hit
            .map(|(face, _)| face.room)
            .or_else(|| self.active_room_id())
        else {
            self.status = "Drop the prefab onto a section surface".to_string();
            return;
        };
        let origin = face_hit
            .and_then(|(face, _)| {
                self.room_grid_view(face.room).map(|grid| {
                    [
                        grid.origin[0] + face.sx as i32,
                        grid.origin[1] + face.sz as i32,
                    ]
                })
            })
            .or_else(|| self.floating_origin_from_3d_hover(room, None, ground_hit));
        let Some(origin) = origin else {
            self.status = "Drop the prefab onto a section surface".to_string();
            return;
        };
        self.stamp_prefab_at(path, room, origin);
    }

    fn stamp_prefab_at(&mut self, path: &Path, room: NodeId, origin: [i32; 2]) {
        let prefab = match psxed_project::Prefab::load_from_path(path) {
            Ok(prefab) => prefab,
            Err(error) => {
                self.status = format!("Could not load prefab: {error}");
                return;
            }
        };
        self.begin_prefab_stamp(prefab, room, origin);
    }

    fn begin_prefab_stamp(
        &mut self,
        prefab: psxed_project::Prefab,
        room: NodeId,
        origin: [i32; 2],
    ) {
        let (mut floors, unbound) = prefab.bound_floors(&self.project, room, self.active_floor);
        if floors.is_empty() {
            self.status = format!("Prefab '{}' has no floors", prefab.name);
            return;
        }
        let cells = floors.remove(0).cells;
        let extra_floors = floors;
        let grid = self.room_grid_view(room);
        let sector_size = grid.map_or(DEFAULT_WORLD_SECTOR_SIZE, |grid| grid.sector_size);

        let mut notes = Vec::new();
        if sector_size != prefab.sector_size {
            notes.push(format!(
                "authored at sector size {} not {sector_size}, heights will not match",
                prefab.sector_size
            ));
        }
        if unbound > 0 {
            notes.push(format!("{unbound} material references cleared"));
        }
        if !extra_floors.is_empty() {
            notes.push(format!("{} floors", extra_floors.len() + 1));
        }
        let lead = if notes.is_empty() {
            format!("Stamping '{}'", prefab.name)
        } else {
            format!("Stamping '{}' ({})", prefab.name, notes.join("; "))
        };

        let clipboard = GeometryClipboard {
            mode: if prefab.merge_primitives {
                GeometryClipboardMode::MergePrimitives
            } else {
                GeometryClipboardMode::ReplaceCells
            },
            source_room: room,
            source_origin: origin,
            next_paste_origin: origin,
            width: prefab.width,
            height: prefab.height,
            cells,
            extra_floors,
            lights: prefab.lights.clone(),
        };
        self.begin_floating_geometry(clipboard, &lead);
    }

    pub(crate) fn paste_target_room(&self, clipboard: &GeometryClipboard) -> Option<NodeId> {
        self.active_room_id().or_else(|| {
            self.project
                .active_scene()
                .node(clipboard.source_room)
                .and_then(|node| matches!(node.kind, NodeKind::Section { .. }).then_some(node.id))
        })
    }

    pub(crate) fn rotate_current_selection_90(&mut self) {
        if self.floating_geometry.is_some() {
            self.rotate_floating_geometry_cw();
        } else if self.has_geometry_selection() {
            self.rotate_selected_geometry_cw();
        } else {
            self.rotate_selected_yaw_90();
        }
    }

    pub(crate) fn update_floating_geometry_origin(&mut self, origin: [i32; 2]) -> bool {
        let Some(preview) = self.floating_geometry.as_mut() else {
            return false;
        };
        if preview.origin == origin {
            return true;
        }
        preview.origin = origin;
        self.apply_floating_geometry_preview();
        true
    }

    /// Feed a pointer-derived grid origin into floating placement. The first
    /// observed cell is only an anchor: duplicate commands can originate from
    /// a shortcut, toolbar, or tree menu while the mouse is elsewhere, and
    /// immediately snapping to that stale position makes the copy appear to
    /// vanish. Later pointer cells move the preview by their delta from this
    /// anchor, preserving the adjacent starting placement even when the cursor
    /// began on the other side of a large room.
    pub(crate) fn track_floating_geometry_pointer_origin(&mut self, origin: [i32; 2]) -> bool {
        let Some(preview) = self.floating_geometry.as_mut() else {
            return false;
        };
        let Some(anchor) = preview.pointer_anchor_origin else {
            preview.pointer_anchor_origin = Some(origin);
            preview.pointer_anchor_placement_origin = preview.origin;
            return true;
        };
        let placement_anchor = preview.pointer_anchor_placement_origin;
        let target = [
            placement_anchor[0].saturating_add(origin[0].saturating_sub(anchor[0])),
            placement_anchor[1].saturating_add(origin[1].saturating_sub(anchor[1])),
        ];
        self.update_floating_geometry_origin(target)
    }

    pub(crate) fn rotate_floating_geometry_cw(&mut self) {
        let Some(preview) = self.floating_geometry.as_mut() else {
            return;
        };
        preview.rotation_quarters = (preview.rotation_quarters + 1) % 4;
        self.apply_floating_geometry_preview();
        self.status = "Rotated duplicate preview 90°".to_string();
    }

    pub(crate) fn flip_floating_geometry_x(&mut self) {
        let Some(preview) = self.floating_geometry.as_mut() else {
            return;
        };
        preview.flip_x = !preview.flip_x;
        self.apply_floating_geometry_preview();
        self.status = "Flipped duplicate preview horizontally".to_string();
    }

    /// Raise or lower the floating placement by `steps` height quanta.
    ///
    /// Snapped to [`HEIGHT_QUANTUM`] because the cooker rejects any authored
    /// height that is not a multiple of it, and a free-running offset would
    /// turn a stamp into a build failure the user cannot see.
    pub(crate) fn nudge_floating_geometry_elevation(&mut self, steps: i32) {
        let Some(preview) = self.floating_geometry.as_mut() else {
            return;
        };
        preview.elevation_offset = preview
            .elevation_offset
            .saturating_add(steps.saturating_mul(HEIGHT_QUANTUM));
        let offset = preview.elevation_offset;
        self.apply_floating_geometry_preview();
        self.status = format!("Placement raised {offset} units from its authored height");
    }

    pub(crate) fn flip_floating_geometry_z(&mut self) {
        let Some(preview) = self.floating_geometry.as_mut() else {
            return;
        };
        preview.flip_z = !preview.flip_z;
        self.apply_floating_geometry_preview();
        self.status = "Flipped duplicate preview vertically".to_string();
    }

    pub(crate) fn commit_floating_geometry(&mut self) -> bool {
        let Some(preview) = self.floating_geometry.take() else {
            return false;
        };
        // Lights go in before anything consumes `preview`, and before the undo
        // snapshot is recorded: `base_project` is the pre-placement state, so
        // recording it after is still correct and one Escape still undoes the
        // whole stamp, lights included.
        let lit = self.place_floating_lights(&preview);
        self.history.record(preview.base_project);
        match preview.mode {
            GeometryClipboardMode::ReplaceCells => {
                self.select_geometry_cells(preview.room, preview.selected_cells);
            }
            GeometryClipboardMode::MergePrimitives => {
                self.select_geometry_primitives(preview.room, preview.selected_primitives);
            }
        }
        let mut notes = Vec::new();
        match preview.seam_walls_stripped {
            0 => {}
            1 => notes.push("1 wall dropped onto a seam a neighbour owns".to_string()),
            n => notes.push(format!("{n} walls dropped onto seams neighbours own")),
        }
        match self.portal_rooms_over_budget(preview.room) {
            0 => {}
            1 => notes
                .push("1 runtime room past a hard cap - author a Portal to split it".to_string()),
            n => notes.push(format!(
                "{n} runtime rooms past a hard cap - author Portals to split them"
            )),
        }
        match lit {
            0 => {}
            1 => notes.push("1 light placed".to_string()),
            n => notes.push(format!("{n} lights placed")),
        }
        self.status = if notes.is_empty() {
            "Placed world geometry".to_string()
        } else {
            format!("Placed world geometry - {}", notes.join("; "))
        };
        self.mark_dirty();
        true
    }

    /// Derived runtime rooms in `room` that still bust a hard cap.
    ///
    /// The authored grid is one contiguous room; the runtime splits it only at
    /// authored `Portal` nodes, and the planner deliberately will not invent a
    /// seam for size (`portal_rooms.rs:3-5`). Stamping grows the grid freely,
    /// so a piece placed past `MAX_ROOM_WIDTH` becomes a build failure that
    /// otherwise only surfaces on Play, an hour of clicking later. Counting
    /// here says it at the moment the placement causes it.
    ///
    /// ponytail: the plan, not a cook. `over_budget` comes off the budget
    /// estimate, which is a sector walk; cooking each derived room the way the
    /// Play path does would be far too slow to run on a click.
    fn portal_rooms_over_budget(&self, room: NodeId) -> usize {
        let scene = self.project.active_scene();
        let Some(NodeKind::Section { grid }) = scene.node(room).map(|node| &node.kind) else {
            return 0;
        };
        plan_portal_rooms(scene, room, grid, PortalRoomConfig::default())
            .rooms
            .iter()
            .filter(|derived| derived.over_budget)
            .count()
    }

    /// Materialise the piece's lights as child nodes of the destination room.
    ///
    /// Runs on commit rather than on every preview pass, because the preview
    /// rebuilds the project from its base snapshot each frame and would either
    /// discard them or stack duplicates. Offsets go through the same transform
    /// the cells take, so a rotated piece keeps its light in the same corner.
    fn place_floating_lights(&mut self, preview: &FloatingGeometryPlacement) -> usize {
        if preview.lights.is_empty() {
            return 0;
        }
        // Reuse the geometry transform verbatim by feeding it sectorless cells:
        // any divergence here would drift a rotated piece's light off-centre.
        let carrier: Vec<GeometryClipboardCell> = preview
            .lights
            .iter()
            .map(|light| GeometryClipboardCell {
                offset: light.cell,
                sector: None,
            })
            .collect();
        let placed = transformed_geometry_cells(
            &carrier,
            preview.width,
            preview.height,
            preview.rotation_quarters,
            preview.flip_x,
            preview.flip_z,
        );

        let Some(grid) = self.room_grid_view(preview.room) else {
            return 0;
        };
        let sector_size = grid.sector_size.max(1) as f32;
        let lift = preview.elevation_offset as f32 / sector_size;
        let spawn: Vec<(String, NodeKind, [f32; 3])> = preview
            .lights
            .iter()
            .zip(&placed)
            .map(|(light, (offset, _))| {
                let editor = grid.world_cells_to_editor([
                    (preview.origin[0] + offset[0]) as f32 + 0.5,
                    (preview.origin[1] + offset[1]) as f32 + 0.5,
                ]);
                (
                    "Prefab Light".to_string(),
                    NodeKind::PointLight {
                        color: light.color,
                        intensity: light.intensity,
                        radius: light.radius,
                    },
                    [editor[0], light.height_sectors + lift, editor[1]],
                )
            })
            .collect();

        let scene = self.project.active_scene_mut();
        let mut count = 0;
        for (name, kind, translation) in spawn {
            let id = scene.add_node(preview.room, &name, kind);
            if let Some(node) = scene.node_mut(id) {
                node.transform.translation = translation;
            }
            count += 1;
        }
        count
    }

    pub(crate) fn cancel_floating_geometry(&mut self) -> bool {
        let Some(preview) = self.floating_geometry.take() else {
            return false;
        };
        self.project = preview.base_project;
        self.dirty = preview.base_dirty;
        self.clear_sector_selection();
        self.clear_primitive_selection_state();
        self.status = "Cancelled duplicate".to_string();
        true
    }

    /// Write one floor of a floating placement, growing the floor stack and the
    /// grid footprint to fit. Returns how many walls the seam pass dropped.
    fn place_floating_floor(
        &mut self,
        preview: &FloatingGeometryPlacement,
        target_floor: usize,
        base_floor: usize,
        relative_elevation: i32,
        cells: Vec<([i32; 2], Option<GridSector>)>,
        selected_cells: &mut Vec<(u16, u16)>,
        selected_primitives: &mut Vec<Selection>,
    ) -> Result<usize, &'static str> {
        {
            let scene = self.project.active_scene_mut();
            let Some(node) = scene.node_mut(preview.room) else {
                return Err("Duplicate target section no longer exists");
            };
            let NodeKind::Section { grid } = &mut node.kind else {
                return Err("Duplicate target is not a Section");
            };
            // Grow the stack so an upper floor of the piece has somewhere to
            // land. A floor this stamp creates takes the piece's own spacing;
            // one that already existed keeps its authored elevation, because
            // moving it would drag the geometry already sitting on it.
            while grid.floor_count() <= target_floor {
                let base_elevation = grid.floor(base_floor).map(|floor| floor.elevation);
                let created = grid.push_floor();
                if created == target_floor && relative_elevation != 0 {
                    if let (Some(base_elevation), Some(floor)) =
                        (base_elevation, grid.floor_mut(created))
                    {
                        floor.elevation = base_elevation.saturating_add(relative_elevation);
                    }
                }
            }
            for (offset, _) in &cells {
                let _ = extend_room_grid_to_include_preserving_child_positions(
                    scene,
                    preview.room,
                    preview.origin[0] + offset[0],
                    preview.origin[1] + offset[1],
                    target_floor,
                );
            }
        }

        let scene = self.project.active_scene_mut();
        let Some(node) = scene.node_mut(preview.room) else {
            return Err("Duplicate target section no longer exists");
        };
        let NodeKind::Section { grid } = &mut node.kind else {
            return Err("Duplicate target is not a Section");
        };
        let floor_idx = target_floor.min(grid.floor_count().saturating_sub(1));
        let grid = grid
            .floor_mut(floor_idx)
            .expect("floor index clamped to range");
        for (offset, sector) in cells {
            let wcx = preview.origin[0] + offset[0];
            let wcz = preview.origin[1] + offset[1];
            let Some((sx, sz)) = grid.world_cell_to_array(wcx, wcz) else {
                continue;
            };
            let Some(index) = grid.sector_index(sx, sz) else {
                continue;
            };
            match preview.mode {
                GeometryClipboardMode::ReplaceCells => {
                    grid.sectors[index] = sector;
                    selected_cells.push((sx, sz));
                }
                GeometryClipboardMode::MergePrimitives => {
                    let Some(fragment) = sector else {
                        continue;
                    };
                    let target = grid.sectors[index].get_or_insert_with(GridSector::empty);
                    merge_primitive_fragment(
                        target,
                        fragment,
                        preview.room,
                        sx,
                        sz,
                        selected_primitives,
                    );
                }
            }
        }
        // Placing a piece against existing geometry hands the cooker two claims
        // on one physical edge, which it rejects outright. The incoming wall
        // loses. ponytail: cells only -- MergePrimitives grafts individual faces
        // onto a sector that keeps its own walls, so it never authors a whole
        // perimeter to collide.
        Ok(match preview.mode {
            GeometryClipboardMode::ReplaceCells => grid.strip_seam_walls(selected_cells),
            GeometryClipboardMode::MergePrimitives => 0,
        })
    }

    pub(crate) fn apply_floating_geometry_preview(&mut self) {
        let Some(preview) = self.floating_geometry.clone() else {
            return;
        };
        self.project = preview.base_project.clone();
        self.dirty = preview.base_dirty;

        let mut cells = transformed_geometry_cells(
            &preview.cells,
            preview.width,
            preview.height,
            preview.rotation_quarters,
            preview.flip_x,
            preview.flip_z,
        );
        for sector in cells.iter_mut().filter_map(|(_, sector)| sector.as_mut()) {
            sector.offset_heights(preview.elevation_offset);
        }
        let mut selected_cells = Vec::new();
        let mut selected_primitives = Vec::new();
        let mut seam_walls_stripped = 0;
        let active_floor = self.active_floor;

        // Batch 0 is the active floor. A multi-floor prefab adds one batch per
        // floor above it, each carrying the same rotation, flips and lift.
        let mut batches = vec![(0usize, 0i32, cells)];
        for (index, floor) in preview.extra_floors.iter().enumerate() {
            let mut above = transformed_geometry_cells(
                &floor.cells,
                preview.width,
                preview.height,
                preview.rotation_quarters,
                preview.flip_x,
                preview.flip_z,
            );
            for sector in above.iter_mut().filter_map(|(_, sector)| sector.as_mut()) {
                sector.offset_heights(preview.elevation_offset);
            }
            batches.push((index + 1, floor.relative_elevation, above));
        }

        for (floor_delta, relative_elevation, cells) in batches {
            let target_floor = active_floor + floor_delta;
            let mut floor_cells = Vec::new();
            let mut floor_primitives = Vec::new();
            match self.place_floating_floor(
                &preview,
                target_floor,
                active_floor,
                relative_elevation,
                cells,
                &mut floor_cells,
                &mut floor_primitives,
            ) {
                Ok(stripped) => seam_walls_stripped += stripped,
                Err(message) => {
                    self.floating_geometry = None;
                    self.status = message.to_string();
                    return;
                }
            }
            // Selection tracks the floor the user is authoring on; geometry
            // written to the floors above it is placed but not selected.
            if floor_delta == 0 {
                selected_cells = floor_cells;
                selected_primitives = floor_primitives;
            }
        }
        if let Some(active_preview) = self.floating_geometry.as_mut() {
            active_preview.selected_cells = selected_cells.clone();
            active_preview.selected_primitives = selected_primitives.clone();
            active_preview.seam_walls_stripped = seam_walls_stripped;
        }
        match preview.mode {
            GeometryClipboardMode::ReplaceCells => {
                self.select_geometry_cells(preview.room, selected_cells);
            }
            GeometryClipboardMode::MergePrimitives => {
                self.select_geometry_primitives(preview.room, selected_primitives);
            }
        }
    }

    pub(crate) fn floating_origin_from_2d_world(
        &self,
        room: NodeId,
        world: [f32; 2],
    ) -> Option<[i32; 2]> {
        let scene = self
            .floating_geometry
            .as_ref()
            .map(|preview| preview.base_project.active_scene())
            .unwrap_or_else(|| self.project.active_scene());
        let node = scene.node(room)?;
        let NodeKind::Section { grid } = &node.kind else {
            return None;
        };
        let center = node_world(node);
        let editor = [world[0] - center[0], world[1] - center[1]];
        let world_cells = grid.editor_to_world_cells(editor);
        Some([world_cells[0].floor() as i32, world_cells[1].floor() as i32])
    }

    pub(crate) fn floating_origin_from_3d_hover(
        &self,
        room: NodeId,
        face_hit: Option<(FaceRef, [f32; 3])>,
        ground_hit: Option<[f32; 2]>,
    ) -> Option<[i32; 2]> {
        // Anchor on the ground-plane projection only, never on
        // `face_hit`. `face_hit` comes from `pick_face_with_hit`, which
        // ray-tests the baked preview in `self.project`; feeding the
        // preview's own faces back as the anchor makes the origin flip
        // to whatever cell the cursor's nearest preview surface is in.
        // `ground_hit` is a pure camera-ray/plane intersection
        // (`pick_3d_world`), independent of scene geometry.
        let _ = face_hit;
        let editor = ground_hit?;
        // Convert with the SAME grid the pick used. `ground_hit` was
        // produced by `pick_3d_world_on_room_plane` via
        // `WorldGrid::room_local_to_editor`, which subtracts
        // `grid_center_cells()` of the *current* (`self.project`) grid.
        // `editor_to_world_cells` re-adds the center, so it must read the
        // same grid or the two centers cancel incorrectly. During a
        // floating placement `self.project` may have been auto-grown,
        // shifting its center: reading the clean base grid here would
        // leave a constant offset between the two centers, the origin
        // lands a cell over, the preview re-grows, the center shifts
        // again, and the wireframe oscillates between two cells frame to
        // frame. Using `self.project`'s grid makes the center cancel so
        // `world_cells` is the true absolute cell under the cursor,
        // regardless of how the preview grew.
        let grid = self.room_grid_view(room)?;
        let world_cells = grid.editor_to_world_cells(editor);
        Some([world_cells[0].floor() as i32, world_cells[1].floor() as i32])
    }

    pub(crate) fn rotate_selected_geometry_cw(&mut self) {
        let (room, cells) = match self.selected_geometry_cell_targets() {
            Ok(targets) => targets,
            Err(message) => {
                self.status = message.to_string();
                return;
            }
        };
        let Some(grid) = self.room_grid_view(room) else {
            self.status = "Selected room no longer exists".to_string();
            return;
        };

        let mut min_x = i32::MAX;
        let mut min_z = i32::MAX;
        let mut max_x = i32::MIN;
        let mut staged = Vec::new();
        for (sx, sz) in cells {
            let world = [grid.origin[0] + sx as i32, grid.origin[1] + sz as i32];
            min_x = min_x.min(world[0]);
            min_z = min_z.min(world[1]);
            max_x = max_x.max(world[0]);
            staged.push((world, grid.sector(sx, sz).cloned()));
        }

        let populated = staged
            .iter()
            .filter(|(_, sector)| sector.as_ref().is_some_and(GridSector::has_geometry))
            .count();
        if populated == 0 {
            self.status = "Selected cells are empty".to_string();
            return;
        }

        let width = max_x - min_x + 1;
        let rotated: Vec<([i32; 2], Option<GridSector>)> = staged
            .iter()
            .map(|(world, sector)| {
                let local_x = world[0] - min_x;
                let local_z = world[1] - min_z;
                (
                    [min_x + local_z, min_z + width - 1 - local_x],
                    sector.as_ref().map(rotate_sector_cw),
                )
            })
            .collect();

        self.push_undo();
        let active_floor = self.active_floor;
        let mut selected = Vec::new();
        {
            let scene = self.project.active_scene_mut();
            let Some(node) = scene.node(room) else {
                self.status = "Selected room no longer exists".to_string();
                return;
            };
            let NodeKind::Section { .. } = &node.kind else {
                self.status = "Selected target is not a Room".to_string();
                return;
            };

            for (world, _) in &rotated {
                let _ = extend_room_grid_to_include_preserving_child_positions(
                    scene,
                    room,
                    world[0],
                    world[1],
                    active_floor,
                );
            }
            let Some(node) = scene.node_mut(room) else {
                self.status = "Selected room no longer exists".to_string();
                return;
            };
            let NodeKind::Section { grid } = &mut node.kind else {
                self.status = "Selected target is not a Room".to_string();
                return;
            };
            let floor_idx = active_floor.min(grid.floor_count().saturating_sub(1));
            let grid = grid
                .floor_mut(floor_idx)
                .expect("floor index clamped to range");
            for (world, _) in &staged {
                if let Some((sx, sz)) = grid.world_cell_to_array(world[0], world[1]) {
                    if let Some(index) = grid.sector_index(sx, sz) {
                        grid.sectors[index] = None;
                    }
                }
            }
            for (world, sector) in rotated {
                let Some((sx, sz)) = grid.world_cell_to_array(world[0], world[1]) else {
                    continue;
                };
                let Some(index) = grid.sector_index(sx, sz) else {
                    continue;
                };
                grid.sectors[index] = sector;
                selected.push((sx, sz));
            }
        }

        self.select_geometry_cells(room, selected.clone());
        self.status = if populated == 1 {
            "Rotated 1 world cell 90°".to_string()
        } else {
            format!("Rotated {populated} world cells 90°")
        };
        self.mark_dirty();
    }

    pub(crate) fn select_geometry_cells(&mut self, room: NodeId, cells: Vec<(u16, u16)>) {
        self.replace_node_selection(room);
        self.clear_resource_selection_state();
        self.clear_primitive_selection_state();
        self.selection.selected_sectors = cells
            .iter()
            .map(|(sx, sz)| (room, *sx, *sz))
            .collect::<HashSet<_>>();
        self.selection.selected_sector = cells.first().copied();
        self.selection.sector_selection_anchor = cells.first().map(|(sx, sz)| (room, *sx, *sz));
        self.interaction.take_box_select_2d();
    }

    pub(crate) fn select_geometry_primitives(&mut self, room: NodeId, selections: Vec<Selection>) {
        self.replace_node_selection(room);
        self.clear_resource_selection_state();
        self.clear_primitive_selection_state();
        self.clear_sector_selection();
        for selection in selections {
            self.push_selected_primitive_unique(selection);
        }
        self.interaction.take_box_select_2d();
        self.update_primitive_resource_selection();
    }

    pub(crate) fn add_child(&mut self, mut kind: NodeKind, name: &str) {
        let parent = self.selection.selected_node;
        if kind.is_component() {
            let scene = self.project.active_scene();
            let Some(host) = scene.node(parent) else {
                self.status = format!("Cannot add {name}: no selected host node");
                return;
            };
            if !component_can_be_added_to_host(&host.kind, &kind, scene, parent) {
                self.status = format!("Cannot add {name} to {}", host.name);
                return;
            }
        }
        self.push_undo();
        let first_material = self.first_material();
        if let NodeKind::Section { grid } = &mut kind {
            *grid = starter_room_grid(
                self.project.world_sector_size_for_node(parent),
                first_material,
            );
        }
        let id = self
            .project
            .active_scene_mut()
            .add_node(parent, name.to_string(), kind);
        self.replace_node_selection(id);
        self.clear_resource_selection_state();
        self.clear_primitive_selection_state();
        self.clear_sector_selection();
        self.status = format!("Added {name}");
        self.mark_dirty();
    }

    pub(crate) fn add_ui_child(&mut self, kind: UiNodeKind, name: &str) {
        self.push_undo();
        let parent = self.selection.selected_ui_node;
        let Some(scene) = self.current_ui_scene_mut() else {
            self.status = "No UI scene available".to_string();
            return;
        };
        let id = scene.add_node(parent, name.to_string(), kind);
        self.selection.selected_ui_node = id;
        self.clear_resource_selection_state();
        self.clear_primitive_selection_state();
        self.clear_sector_selection();
        self.status = format!("Added UI {name}");
        self.mark_dirty();
    }

    pub(crate) fn create_reciprocal_portal(&mut self, source_portal: NodeId) {
        let Some(connection) = connection_for_portal(self.project.active_scene(), source_portal)
        else {
            self.status = "Could not find portal connection".to_string();
            return;
        };
        let source = if connection.a.portal == source_portal {
            connection.a
        } else if let Some(pair) = connection.b {
            pair
        } else {
            connection.a
        };
        if source.target_room.is_none() {
            self.status = "Assign a target room before creating a reciprocal portal".to_string();
            return;
        }
        if connection.status != RoomConnectionStatus::Unpaired {
            self.status = format!("Connection is {}", connection.status.label());
            return;
        }

        let target_room = source.target_room.expect("checked target room");
        let scene = self.project.active_scene();
        let Some(source_node) = scene.node(source.portal) else {
            self.status = "Source portal node is missing".to_string();
            return;
        };
        let Some(target_room_node) = scene.node(target_room) else {
            self.status = "Target room is missing".to_string();
            return;
        };
        if !matches!(target_room_node.kind, NodeKind::Section { .. }) {
            self.status = "Target node is not a room".to_string();
            return;
        }

        let source_world = portal_marker_world_2d(scene, source_node);
        let target_center = node_world(target_room_node);
        let reciprocal_translation = [
            source_world[0] - target_center[0],
            source_node.transform.translation[1],
            source_world[1] - target_center[1],
        ];
        let source_name = source_node.name.clone();
        let source_entry = match &source_node.kind {
            NodeKind::Portal { entry_name, .. } => entry_name.clone(),
            _ => String::new(),
        };
        let source_room_name = room_display_name(scene, source.room);
        let target_room_name = room_display_name(scene, target_room);
        let geometry = source.geometry.as_ref().map(inverted_portal_geometry);

        self.push_undo();
        let scene = self.project.active_scene_mut();
        let reciprocal = scene.add_node(
            target_room,
            format!("Portal {target_room_name} -> {source_room_name}"),
            NodeKind::Portal {
                target_room: Some(source.room),
                target_entry: source_entry,
                entry_name: format!("{}_reciprocal", source_name.trim().replace(' ', "_")),
                geometry,
            },
        );
        if let Some(node) = scene.node_mut(reciprocal) {
            node.transform.translation = reciprocal_translation;
        }
        self.replace_node_selection(reciprocal);
        self.clear_resource_selection_state();
        self.clear_primitive_selection_state();
        self.clear_sector_selection();
        self.status = "Created reciprocal portal".to_string();
        self.mark_dirty();
    }

    pub(crate) fn duplicate_selected(&mut self) {
        let selected = self.selected_node_ids_in_hierarchy();
        if selected.is_empty() {
            return;
        }
        self.push_undo();
        let mut duplicated = Vec::new();
        for selected in selected {
            let Some(source) = self.project.active_scene().node(selected).cloned() else {
                continue;
            };
            let parent = source.parent.unwrap_or(NodeId::ROOT);
            let id = self.project.active_scene_mut().add_node(
                parent,
                format!("{} Copy", source.name),
                source.kind,
            );
            if let Some(node) = self.project.active_scene_mut().node_mut(id) {
                node.transform = source.transform;
            }
            duplicated.push(id);
        }
        if duplicated.is_empty() {
            return;
        }
        self.selection.selected_nodes = duplicated.iter().copied().collect();
        self.selection.selected_node = duplicated[0];
        self.selection.node_selection_anchor = duplicated.last().copied();
        self.clear_resource_selection_state();
        self.clear_primitive_selection_state();
        self.clear_sector_selection();
        self.status = if duplicated.len() == 1 {
            "Duplicated node".to_string()
        } else {
            format!("Duplicated {} nodes", duplicated.len())
        };
        self.mark_dirty();
    }

    pub(crate) fn delete_selected(&mut self) {
        let selected = self.selected_node_ids_in_hierarchy();
        if selected.is_empty() {
            return;
        }
        self.push_undo();
        let mut removed = 0usize;
        for id in selected.iter().rev() {
            if self.project.active_scene_mut().remove_node(*id) {
                removed += 1;
            }
        }
        if removed > 0 {
            self.clear_node_selection_state();
            self.clear_resource_selection_state();
            self.clear_primitive_selection_state();
            self.clear_sector_selection();
            self.status = if removed == 1 {
                "Deleted node".to_string()
            } else {
                format!("Deleted {removed} nodes")
            };
            self.mark_dirty();
        }
    }

    pub(crate) fn draw_component_authoring_panel(
        &mut self,
        ui: &mut egui::Ui,
        selected: NodeId,
        character_options: &[(ResourceId, String)],
        nav_target: &mut Option<ResourceId>,
        preview_action: &mut Option<psxed_project::CharacterAnimationAction>,
    ) -> bool {
        let scene = self.project.active_scene();
        let Some(node) = scene.node(selected) else {
            return false;
        };

        let is_host = matches!(node.kind, NodeKind::Entity);
        let is_component = node.kind.is_component();
        if !is_host && !is_component {
            return false;
        }

        if is_component {
            let parent = node
                .parent
                .and_then(|parent| scene.node(parent).map(|node| (node.id, node.name.clone())));
            egui::CollapsingHeader::new(icons::label(icons::LAYERS, "Relationship"))
                .default_open(false)
                .show(ui, |ui| {
                    if let Some((parent_id, parent_name)) = &parent {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Host").color(STUDIO_TEXT_WEAK));
                            if ui.button(parent_name).clicked() {
                                self.replace_node_selection(*parent_id);
                                self.clear_resource_selection_state();
                                self.clear_primitive_selection_state();
                                self.clear_sector_selection();
                            }
                        });
                    } else {
                        ui.weak("Component has no host parent.");
                    }
                });
            return false;
        }

        let host_kind = node.kind.clone();
        let components: Vec<(NodeId, String, &'static str)> = node
            .children
            .iter()
            .filter_map(|id| scene.node(*id))
            .filter(|child| child.kind.is_component())
            .map(|child| (child.id, child.name.clone(), child.kind.label()))
            .collect();
        let existing: Vec<&NodeKind> = node
            .children
            .iter()
            .filter_map(|id| scene.node(*id))
            .filter(|child| child.kind.is_component())
            .map(|child| &child.kind)
            .collect();
        let addable = addable_component_templates(&host_kind, &existing);

        let mut add_component = None;
        let mut select_component = None;
        let mut promoted_controller = None;
        let mut changed = false;
        egui::CollapsingHeader::new(icons::label(icons::LAYERS, "Components"))
            .default_open(true)
            .show(ui, |ui| {
                if components.is_empty() {
                    ui.weak("No components attached.");
                } else {
                    for (id, name, kind) in &components {
                        let is_character_controller = *kind == "Character Controller";
                        let title = if name == kind {
                            name.clone()
                        } else {
                            format!("{name} · {kind}")
                        };
                        inspector_section(
                            ui,
                            ("entity-component", selected.raw(), id.raw()),
                            node_lucide_icon(kind, false),
                            &title,
                            is_character_controller,
                            |ui| {
                                if is_character_controller {
                                    let (component_changed, became_player) = {
                                        let Some(node) =
                                            self.project.active_scene_mut().node_mut(*id)
                                        else {
                                            ui.colored_label(
                                                Color32::from_rgb(220, 120, 100),
                                                "Component no longer exists.",
                                            );
                                            return;
                                        };
                                        let NodeKind::CharacterController {
                                            character,
                                            settings,
                                            player,
                                        } = &mut node.kind
                                        else {
                                            return;
                                        };
                                        let was_player = *player;
                                        let edited = ui
                                            .push_id(
                                                ("inline-character-controller", id.raw()),
                                                |ui| {
                                                    draw_character_controller_editor(
                                                        ui,
                                                        character,
                                                        settings,
                                                        player,
                                                        character_options,
                                                        nav_target,
                                                        preview_action,
                                                    )
                                                },
                                            )
                                            .inner;
                                        (edited, !was_player && *player)
                                    };
                                    changed |= component_changed;
                                    if became_player {
                                        promoted_controller = Some(*id);
                                    }
                                } else {
                                    ui.label(
                                        RichText::new(
                                            "This component keeps its dedicated settings editor.",
                                        )
                                        .color(STUDIO_TEXT_WEAK)
                                        .small(),
                                    );
                                }
                                if ui
                                    .button(icons::label(icons::POINTER, "Open full settings"))
                                    .on_hover_text("Select this component in the Scene Graph")
                                    .clicked()
                                {
                                    select_component = Some(*id);
                                }
                            },
                        );
                    }
                }

                ui.separator();
                ui.menu_button(icons::label(icons::PLUS, "Add Component"), |ui| {
                    if addable.is_empty() {
                        ui.weak("All singleton components are already present.");
                    }
                    for (label, kind) in &addable {
                        if ui.button(*label).clicked() {
                            add_component = Some((*label, kind.clone()));
                            ui.close_menu();
                        }
                    }
                });
            });

        if let Some(id) = select_component {
            self.replace_node_selection(id);
            self.clear_resource_selection_state();
            self.clear_primitive_selection_state();
            self.clear_sector_selection();
        }
        if let Some((label, kind)) = add_component {
            self.add_component_to_host(selected, label, kind);
        }
        if let Some(controller) = promoted_controller {
            self.demote_player_sources_except(Some(controller));
        }
        changed
    }

    #[cfg(test)]
    pub(crate) fn set_character_controller_player_controlled(
        &mut self,
        controller: NodeId,
        player: bool,
    ) {
        let Some(current) =
            self.project
                .active_scene()
                .node(controller)
                .and_then(|node| match &node.kind {
                    NodeKind::CharacterController { player, .. } => Some(*player),
                    _ => None,
                })
        else {
            self.status = "Selected component is not a Character Controller".to_string();
            return;
        };
        if current == player {
            return;
        }

        self.push_undo();
        if player {
            self.demote_player_sources_except(Some(controller));
        }
        let Some(node) = self.project.active_scene_mut().node_mut(controller) else {
            self.status = "Character Controller no longer exists".to_string();
            return;
        };
        let NodeKind::CharacterController {
            player: current, ..
        } = &mut node.kind
        else {
            self.status = "Selected component is not a Character Controller".to_string();
            return;
        };
        *current = player;
        self.status = if player {
            "Marked Character Controller as player controlled".to_string()
        } else {
            "Cleared player control from Character Controller".to_string()
        };
        self.mark_dirty();
    }

    pub(crate) fn add_component_to_host(
        &mut self,
        host: NodeId,
        label: &'static str,
        kind: NodeKind,
    ) -> Option<NodeId> {
        if !kind.is_component() {
            self.status = "Only component nodes can be added as components".to_string();
            return None;
        }
        let scene = self.project.active_scene();
        let Some(host_node) = scene.node(host) else {
            self.status = "Component host no longer exists".to_string();
            return None;
        };
        if !matches!(host_node.kind, NodeKind::Entity) {
            self.status = "Components can only be added to Entity nodes".to_string();
            return None;
        }
        if !component_can_be_added_to_host(&host_node.kind, &kind, scene, host) {
            self.status = format!("{label} is already present or invalid for this host");
            return None;
        }

        self.push_undo();
        let id = self
            .project
            .active_scene_mut()
            .add_node(host, label.to_string(), kind);
        self.replace_node_selection(id);
        self.clear_resource_selection_state();
        self.clear_primitive_selection_state();
        self.clear_sector_selection();
        self.status = format!("Added {label} component");
        self.mark_dirty();
        Some(id)
    }

    pub(crate) fn delete_selected_sectors(&mut self) {
        let targets: Vec<SectorSelection> =
            self.selection.selected_sectors.iter().copied().collect();
        if targets.is_empty() {
            return;
        }

        let first_room = targets[0].0;
        let target_room = targets
            .iter()
            .all(|(room, _, _)| *room == first_room)
            .then_some(first_room);
        let deleted_floor = self.active_floor;

        self.push_undo();
        let mut removed = 0usize;
        for (room, sx, sz) in targets {
            let Some(grid) = self.room_floor_grid_mut(room) else {
                continue;
            };
            let Some(index) = grid.sector_index(sx, sz) else {
                continue;
            };
            if grid.sectors[index].take().is_some() {
                removed += 1;
            }
        }

        self.clear_sector_selection();
        self.clear_primitive_selection_state();
        if removed > 0 {
            let removed_layer =
                target_room.and_then(|room| self.remove_empty_layer_in_room(room, deleted_floor));
            self.status = if let Some((replacement, count)) = removed_layer {
                format!(
                    "Deleted {removed} tile{} and empty layer {}; now editing layer {} of {}",
                    if removed == 1 { "" } else { "s" },
                    deleted_floor + 1,
                    replacement + 1,
                    count
                )
            } else if removed == 1 {
                "Deleted tile".to_string()
            } else {
                format!("Deleted {removed} tiles")
            };
            self.mark_dirty();
        } else {
            self.status = "No selected tiles had geometry".to_string();
        }
    }

    /// Delete dispatch for the active selection:
    /// - Face   → remove the face from its sector.
    /// - Edge   → remove the face that owns the edge.
    /// - Vertex → drop the corner on the seed face, turning it
    ///   into a triangle (split is auto-flipped to the surviving
    ///   diagonal). The other coincident face-corners are left
    ///   untouched.
    pub(crate) fn delete_selected_primitives(&mut self) {
        let targets = self.selected_primitive_targets();
        if targets.is_empty() {
            return;
        }
        self.push_undo();

        let mut removed = 0usize;
        let mut triangulated = 0usize;
        let mut first_label = None;
        for selection in targets {
            match self.delete_primitive_no_undo(selection) {
                DeleteOutcome::Removed(label) => {
                    removed += 1;
                    first_label.get_or_insert(label);
                }
                DeleteOutcome::Triangulated(label) => {
                    triangulated += 1;
                    first_label.get_or_insert(label);
                }
                DeleteOutcome::Missing => {}
            }
        }

        let changed = removed + triangulated;
        if changed == 0 {
            self.status = "Nothing to delete".to_string();
            return;
        }

        self.clear_primitive_selection_state();
        self.selection.hovered_primitive = None;
        self.status = if changed == 1 {
            if removed == 1 {
                format!("Deleted {}", first_label.unwrap_or("primitive"))
            } else {
                format!("Dropped {}", first_label.unwrap_or("primitive"))
            }
        } else {
            format!("Deleted {changed} primitives")
        };
        self.mark_dirty();
    }

    pub(crate) fn delete_primitive_no_undo(&mut self, selection: Selection) -> DeleteOutcome {
        match selection {
            Selection::Face(face) => self.remove_face_no_undo(face),
            Selection::Triangle(triangle) => self.remove_triangle_no_undo(triangle),
            Selection::Edge(edge) => edge_owning_face_ref(edge)
                .map(|face| self.remove_face_no_undo(face))
                .unwrap_or(DeleteOutcome::Missing),
            Selection::Vertex(vertex) => self.drop_vertex_no_undo(vertex),
        }
    }

    pub(crate) fn remove_triangle_no_undo(
        &mut self,
        triangle: HorizontalTriangleRef,
    ) -> DeleteOutcome {
        let Some(grid) = self.room_floor_grid_mut(triangle.room) else {
            return DeleteOutcome::Missing;
        };
        let Some(sector) = grid.sector_mut(triangle.sx, triangle.sz) else {
            return DeleteOutcome::Missing;
        };
        match triangle.surface {
            HorizontalSurfaceKind::Floor => {
                let Some(face) = sector.floor.as_mut() else {
                    return DeleteOutcome::Missing;
                };
                if face.dropped_corner.is_some() {
                    sector.floor = None;
                    DeleteOutcome::Removed("floor")
                } else {
                    let corner = horizontal_triangle_delete_corner(face.split, triangle.index);
                    face.drop_corner(corner);
                    DeleteOutcome::Triangulated("floor triangle")
                }
            }
            HorizontalSurfaceKind::Ceiling => {
                let Some(face) = sector.ceiling.as_mut() else {
                    return DeleteOutcome::Missing;
                };
                if face.dropped_corner.is_some() {
                    sector.ceiling = None;
                    DeleteOutcome::Removed("ceiling")
                } else {
                    let corner = horizontal_triangle_delete_corner(face.split, triangle.index);
                    face.drop_corner(corner);
                    DeleteOutcome::Triangulated("ceiling triangle")
                }
            }
        }
    }

    /// Detach a face from its sector. Floors / ceilings clear the
    /// `Option<>`; walls splice the entry out of the per-direction
    /// `Vec`. Returns `Removed` on success so the caller can update
    /// status / clear the selection.
    pub(crate) fn remove_face_no_undo(&mut self, face: FaceRef) -> DeleteOutcome {
        let Some(grid) = self.room_floor_grid_mut(face.room) else {
            return DeleteOutcome::Missing;
        };
        let Some(sector) = grid.sector_mut(face.sx, face.sz) else {
            return DeleteOutcome::Missing;
        };
        let removed = match face.kind {
            FaceKind::Floor => sector.floor.take().is_some(),
            FaceKind::Ceiling => sector.ceiling.take().is_some(),
            FaceKind::Wall { dir, stack } => {
                let walls = sector.walls.get_mut(dir);
                if (stack as usize) < walls.len() {
                    walls.remove(stack as usize);
                    true
                } else {
                    false
                }
            }
        };
        if removed {
            DeleteOutcome::Removed(describe_face_kind(face.kind))
        } else {
            DeleteOutcome::Missing
        }
    }

    /// Drop a corner from the vertex's seed face. Floors / ceilings
    /// gain a `dropped_corner` and have their split forced to the
    /// surviving diagonal. Walls do the same with `WallCorner`.
    pub(crate) fn drop_vertex_no_undo(&mut self, vertex: VertexRef) -> DeleteOutcome {
        let Some(grid) = self.room_floor_grid_mut(vertex.room) else {
            return DeleteOutcome::Missing;
        };
        let (sx, sz) = match vertex.anchor {
            VertexAnchor::Floor { sx, sz, .. }
            | VertexAnchor::Ceiling { sx, sz, .. }
            | VertexAnchor::Wall { sx, sz, .. } => (sx, sz),
        };
        let Some(sector) = grid.sector_mut(sx, sz) else {
            return DeleteOutcome::Missing;
        };
        match vertex.anchor {
            VertexAnchor::Floor { corner, .. } => {
                let Some(floor) = sector.floor.as_mut() else {
                    return DeleteOutcome::Missing;
                };
                floor.drop_corner(corner);
                DeleteOutcome::Triangulated("floor corner")
            }
            VertexAnchor::Ceiling { corner, .. } => {
                let Some(ceiling) = sector.ceiling.as_mut() else {
                    return DeleteOutcome::Missing;
                };
                ceiling.drop_corner(corner);
                DeleteOutcome::Triangulated("ceiling corner")
            }
            VertexAnchor::Wall {
                dir, stack, corner, ..
            } => {
                let walls = sector.walls.get_mut(dir);
                let Some(wall) = walls.get_mut(stack as usize) else {
                    return DeleteOutcome::Missing;
                };
                wall.drop_corner(corner);
                DeleteOutcome::Triangulated("wall corner")
            }
        }
    }

    pub(crate) fn open_new_project_dialog(&mut self) {
        self.modal = Modal::NewProject {
            name: String::new(),
            error: None,
        };
    }

    pub(crate) fn open_texture_import_dialog(&mut self) {
        self.texture_import_dialog.open = true;
        self.texture_import_dialog.status = None;
        self.retire_texture_import_preview();
    }

    pub(crate) fn open_model_import_dialog(&mut self) {
        self.model_import_dialog.open = true;
        self.model_import_dialog.status = None;
        self.retire_model_import_preview();
        self.model_import_dialog.selected_clip = 0;
    }

    pub(crate) fn catalogue_animation_source_folder(&mut self) {
        let mut dialog = rfd::FileDialog::new().set_title("Choose animation source folder");
        if self.project_dir.is_dir() {
            dialog = dialog.set_directory(&self.project_dir);
        }
        let Some(path) = dialog.pick_folder() else {
            return;
        };
        self.catalogue_animation_source_path(&path);
    }

    pub(crate) fn catalogue_animation_source_zip(&mut self) {
        let mut dialog = rfd::FileDialog::new()
            .set_title("Choose animation source zip")
            .add_filter("Animation source zip", &["zip"]);
        if self.project_dir.is_dir() {
            dialog = dialog.set_directory(&self.project_dir);
        }
        let Some(path) = dialog.pick_file() else {
            return;
        };
        self.catalogue_animation_source_path(&path);
    }

    pub(crate) fn catalogue_animation_source_path(&mut self, path: &Path) {
        match catalogue_animation_sources_from_path(&mut self.project, &self.project_dir, path) {
            Ok(report) => {
                self.status = format!(
                    "Catalogued animation sources: {} found, {} added, {} updated",
                    report.source_candidates, report.sources_added, report.sources_updated
                );
                if report.changed() {
                    self.mark_dirty();
                }
            }
            Err(error) => {
                self.status = format!("Animation source catalogue failed: {error}");
            }
        }
    }

    pub(crate) fn handle_animation_viewer_action(
        &mut self,
        action: model_animation_viewer::AnimationViewerAction,
    ) {
        match action {
            model_animation_viewer::AnimationViewerAction::BakeSourceForModel {
                model_id,
                source_id,
            } => {
                let Some((model_source, world_height)) =
                    self.project.resource(model_id).and_then(|resource| {
                        let ResourceData::Model(model) = &resource.data else {
                            return None;
                        };
                        Some((model.source_path.clone(), model.world_height))
                    })
                else {
                    self.status = format!("Model #{} is not available", model_id.raw());
                    return;
                };
                let Some(model_source) = model_source.filter(|path| !path.trim().is_empty()) else {
                    self.status = format!(
                        "Model #{} has no source path. Set it in the Model inspector or reimport the model.",
                        model_id.raw()
                    );
                    return;
                };
                let Some(animation_source) =
                    self.project.resource(source_id).and_then(|resource| {
                        let ResourceData::AnimationSource(source) = &resource.data else {
                            return None;
                        };
                        Some(source.source_path.clone())
                    })
                else {
                    self.status = format!("Animation source #{} is not available", source_id.raw());
                    return;
                };

                let temp_dir = match make_animation_bake_temp_dir() {
                    Ok(path) => path,
                    Err(error) => {
                        self.status = format!("Animation bake failed: {error}");
                        return;
                    }
                };
                let result = (|| {
                    let model_source_path = materialize_authoring_source_path(
                        &model_source,
                        &self.project_dir,
                        &temp_dir,
                    )?;
                    let animation_source_path = materialize_authoring_source_path(
                        &animation_source,
                        &self.project_dir,
                        &temp_dir,
                    )?;
                    let config = psxed_project::model_import::RigidModelConfig {
                        world_height,
                        extra_animations_affect_bounds: false,
                        ..Default::default()
                    };
                    psxed_project::model_import::bake_animation_source_for_model(
                        &mut self.project,
                        model_id,
                        source_id,
                        &model_source_path,
                        &animation_source_path,
                        &self.project_dir,
                        config,
                    )
                    .map_err(|error| error.to_string())
                })();
                let _ = std::fs::remove_dir_all(&temp_dir);
                match result {
                    Ok(clip_id) => {
                        self.animation_viewer.focus_resource(&self.project, clip_id);
                        self.animation_viewer_preview_texture = None;
                        self.mark_dirty();
                        self.status = format!("Baked animation clip #{}", clip_id.raw());
                    }
                    Err(error) => {
                        self.status = format!("Animation bake failed: {error}");
                    }
                }
            }
            model_animation_viewer::AnimationViewerAction::ProjectChanged => {
                self.mark_dirty();
            }
        }
    }

    pub(crate) fn mark_dirty(&mut self) {
        self.dirty = true;
        self.clear_validation_issues();
    }

    pub(crate) fn commit_resource_rename(&mut self, id: ResourceId, name: String) {
        let Some(current_name) = self.project.resource_name(id).map(str::to_string) else {
            self.resource_renaming = None;
            self.status = format!("Resource #{} no longer exists", id.raw());
            return;
        };

        let final_name = name.trim();
        if final_name.is_empty() {
            self.resource_renaming = Some((id, current_name));
            self.status = "Resource name cannot be empty".to_string();
            return;
        }
        if final_name == current_name {
            self.resource_renaming = Some((id, current_name));
            return;
        }

        let before = self.project.clone();
        match self
            .project
            .rename_resource_with_files(id, final_name, &self.project_dir)
        {
            Ok(report) => {
                if report.renamed_files.is_empty() {
                    self.history.record(before);
                } else {
                    self.history.clear();
                }
                self.resource_renaming = Some((id, final_name.to_string()));
                self.mark_dirty();

                let moved = report.renamed_files.len();
                let skipped = report.skipped_files.len();
                self.status = match (moved, skipped) {
                    (0, 0) => format!("Renamed {final_name}"),
                    (m, 0) => format!("Renamed {final_name}; moved {m} file(s)"),
                    (0, s) => format!("Renamed {final_name}; skipped {s} file path(s)"),
                    (m, s) => {
                        format!("Renamed {final_name}; moved {m} file(s), skipped {s} path(s)")
                    }
                };
            }
            Err(error) => {
                self.resource_renaming = Some((id, current_name));
                self.status = format!("Rename failed: {error}");
            }
        }
    }

    /// Delete every EXACT duplicate wall segment in the active scene.
    ///
    /// A duplicate is byte-identical to one already on the same edge: same
    /// heights, material, UV transform, solidity and dropped corner. Two of
    /// them occupy one plane, so the second can never be seen and costs a full
    /// room surface to draw. The cooker only rejects the different case, where
    /// two neighbouring sectors each claim one physical edge; a list that
    /// repeats itself passes straight through into the room cache. cortex_v3
    /// carried 30 across 185 authored segments.
    ///
    /// Counts first so the status line can say nothing was found without
    /// spending an undo step on a no-op.
    pub(crate) fn remove_duplicate_walls(&mut self) {
        let room_ids: Vec<_> = self
            .project
            .active_scene()
            .nodes()
            .iter()
            .filter(|node| matches!(node.kind, NodeKind::Section { .. }))
            .map(|node| node.id)
            .collect();

        let mut found = 0usize;
        for &id in &room_ids {
            if let Some(node) = self.project.active_scene().node(id) {
                if let NodeKind::Section { grid } = &node.kind {
                    found += grid.duplicate_wall_count_all_floors();
                }
            }
        }
        if found == 0 {
            self.status = "No duplicate walls found".to_string();
            return;
        }

        self.push_undo();
        let mut removed = 0usize;
        let mut rooms_touched = 0usize;
        for &id in &room_ids {
            let Some(node) = self.project.active_scene_mut().node_mut(id) else {
                continue;
            };
            let NodeKind::Section { grid } = &mut node.kind else {
                continue;
            };
            let n = grid.dedupe_duplicate_walls_all_floors();
            if n > 0 {
                removed += n;
                rooms_touched += 1;
            }
        }
        self.status = format!(
            "Removed {removed} duplicate wall segment{} from {rooms_touched} room{}",
            if removed == 1 { "" } else { "s" },
            if rooms_touched == 1 { "" } else { "s" },
        );
        self.mark_dirty();
    }

    /// Snapshot the current project before a discrete mutation.
    /// Call once per user action -- paint click, place, add/delete
    /// node, etc -- so each undo step matches one author intent.
    pub(crate) fn push_undo(&mut self) {
        self.history.record(self.project.clone());
    }

    /// Drop an inspector coalescing token once its pointer drag or focused
    /// keyboard edit has ended. A new control then starts a fresh undo step.
    pub(crate) fn prepare_inspector_undo_frame(&mut self, ctx: &egui::Context) {
        self.prepare_inspector_undo(InspectorUndoInput::from_context(ctx));
    }

    pub(crate) fn prepare_inspector_undo(&mut self, input: InspectorUndoInput) {
        let Some(transaction) = self.inspector_undo_transaction.as_mut() else {
            return;
        };
        if input.pointer_down {
            if input.focused_widget.is_some() {
                transaction.focused_widget = input.focused_widget;
            }
            return;
        }
        if !input.wants_keyboard || input.focused_widget != transaction.focused_widget {
            self.inspector_undo_transaction = None;
        }
    }

    /// Record the pre-draw document when the Inspector mutated project data.
    /// Nested actions that explicitly touched history win: this preserves the
    /// non-undoable contract for filesystem-backed resource operations.
    pub(crate) fn finish_inspector_undo_frame(
        &mut self,
        before: ProjectDocument,
        history_epoch_before: u64,
        ctx: &egui::Context,
    ) {
        self.finish_inspector_undo(
            before,
            history_epoch_before,
            InspectorUndoInput::from_context(ctx),
        );
    }

    pub(crate) fn finish_inspector_undo(
        &mut self,
        before: ProjectDocument,
        history_epoch_before: u64,
        input: InspectorUndoInput,
    ) {
        if self.project == before {
            return;
        }
        if self.history.epoch() != history_epoch_before {
            self.inspector_undo_transaction = None;
            return;
        }

        // Focus may have moved while the Inspector was drawn, so close a stale
        // text transaction before deciding whether this edit needs a snapshot.
        self.prepare_inspector_undo(input);
        if self.inspector_undo_transaction.is_none() {
            self.history.record(before);
        }

        self.inspector_undo_transaction = if input.pointer_down || input.wants_keyboard {
            Some(InspectorUndoTransaction {
                focused_widget: input.focused_widget,
            })
        } else {
            None
        };
    }

    /// Pop the most recent snapshot back into `project`.
    pub(crate) fn do_undo(&mut self) {
        self.inspector_undo_transaction = None;
        if let Some(prev) = self.history.undo(self.project.clone()) {
            self.project = prev;
            self.clear_resource_selection_state();
            self.resource_renaming = None;
            self.clear_sector_selection();
            self.reconcile_selection_after_document_change();
            self.status = "Undo".to_string();
            self.mark_dirty();
        } else {
            self.status = "Nothing to undo".to_string();
        }
    }

    pub(crate) fn do_redo(&mut self) {
        self.inspector_undo_transaction = None;
        if let Some(next) = self.history.redo(self.project.clone()) {
            self.project = next;
            self.clear_resource_selection_state();
            self.resource_renaming = None;
            self.clear_sector_selection();
            self.reconcile_selection_after_document_change();
            self.status = "Redo".to_string();
            self.mark_dirty();
        } else {
            self.status = "Nothing to redo".to_string();
        }
    }

    pub(crate) fn frame_viewport(&mut self) {
        if !self.view_2d {
            if let Some((center, half)) = self.current_frame_bounds_3d() {
                self.frame_3d_bounds(center, half);
                self.persist_editor_camera_state();
                self.status = "Framed selection".to_string();
            } else {
                self.status = "Nothing to frame".to_string();
            }
            return;
        }

        let Some((center, half)) = self.current_frame_bounds_2d() else {
            self.orthographic_focus = [0.0; 3];
            self.viewport_zoom = DEFAULT_VIEWPORT_ZOOM;
            self.status = "Reset viewport frame".to_string();
            return;
        };
        let content = [(half[0] * 2.0).max(1.0), (half[1] * 2.0).max(1.0)];
        let viewport = [
            self.last_viewport_size.x.max(320.0),
            self.last_viewport_size.y.max(240.0),
        ];
        let zoom_x = viewport[0] * 0.72 / content[0];
        let zoom_y = viewport[1] * 0.72 / content[1];
        self.viewport_zoom = zoom_x
            .min(zoom_y)
            .clamp(MIN_VIEWPORT_ZOOM, MAX_VIEWPORT_ZOOM);
        self.orthographic_focus = self
            .orthographic_view
            .with_projected_focus(self.orthographic_focus, center);
        self.status = "Framed selection".to_string();
    }

    /// Fit 3D bounds without changing the current viewing direction. Orbit
    /// mode moves its target and dolly distance; free mode moves the camera
    /// backward along its own look vector. In both cases `.` therefore frames
    /// the selection instead of merely repointing at it from an arbitrary
    /// distance.
    pub(crate) fn frame_3d_bounds(&mut self, center: [f32; 3], half: [f32; 3]) {
        let target = center.map(round_to_i32);
        let radius = frame_radius_for_3d_bounds(half);
        self.camera_rig.target = target;
        self.camera_rig.radius = radius;

        if self.camera_rig.mode == ViewportCameraMode::Free {
            let forward =
                camera_forward_from_angles(self.camera_rig.free_yaw, self.camera_rig.free_pitch);
            self.camera_rig.free_position = [
                round_to_i32(target[0] as f32 - forward[0] * radius as f32),
                round_to_i32(target[1] as f32 - forward[1] * radius as f32),
                round_to_i32(target[2] as f32 - forward[2] * radius as f32),
            ];
            self.camera_rig.free_initialized = true;
        }
    }

    pub(crate) fn current_frame_bounds_3d(&self) -> Option<([f32; 3], [f32; 3])> {
        self.selected_frame_bounds_3d().or_else(|| {
            self.active_room_id()
                .and_then(|room_id| self.room_bounds_3d(room_id))
        })
    }

    pub(crate) fn selected_frame_bounds_3d(&self) -> Option<([f32; 3], [f32; 3])> {
        if self.active_tool == ViewTool::Brush {
            if let Some(bounds) = self.selected_brush_frame_bounds_3d() {
                return Some(bounds);
            }
        }

        let mut bounds: Option<(f32, f32, f32, f32, f32, f32)> = None;
        for &(room, sx, sz) in &self.selection.selected_sectors {
            if let Some((center, half)) = self.sector_bounds_3d(room, sx, sz) {
                merge_bounds_3d(&mut bounds, center, half);
            }
        }
        if let Some(bounds) = bounds {
            return Some(bounds_3d_to_center_half(bounds));
        }

        let primitive_targets = self.selected_primitive_targets();
        if primitive_targets.len() > 1 {
            let mut bounds = None;
            for selection in primitive_targets {
                if let Some((center, half)) = self.selection_bounds_3d(selection) {
                    merge_bounds_3d(&mut bounds, center, half);
                }
            }
            if let Some(bounds) = bounds {
                return Some(bounds_3d_to_center_half(bounds));
            }
        } else if let Some(selection) = self.selection.selected_primitive {
            return self.selection_bounds_3d(selection);
        }

        if let Some((sx, sz)) = self.selection.selected_sector {
            if let Some(room) = self.active_room_id() {
                return self.sector_bounds_3d(room, sx, sz);
            }
        }

        let selected_nodes = self.selected_node_ids_in_hierarchy();
        if selected_nodes.len() > 1 {
            let mut bounds = None;
            for id in selected_nodes {
                if let Some((center, half)) = self.node_frame_bounds_3d(id) {
                    merge_bounds_3d(&mut bounds, center, half);
                }
            }
            if let Some(bounds) = bounds {
                return Some(bounds_3d_to_center_half(bounds));
            }
        }

        if let Some(bounds) = self.node_frame_bounds_3d(self.selection.selected_node) {
            return Some(bounds);
        }
        None
    }

    fn selected_brush_frame_bounds_3d(&self) -> Option<([f32; 3], [f32; 3])> {
        let brush = self
            .project
            .active_scene()
            .brushes
            .get(self.selected_brush?)?;
        let solved = brush.solve();
        if !solved.is_valid()
            || !solved.min.into_iter().all(f64::is_finite)
            || !solved.max.into_iter().all(f64::is_finite)
        {
            return None;
        }
        let mut center = [0.0; 3];
        let mut half = [0.0; 3];
        for axis in 0..3 {
            center[axis] = ((solved.min[axis] + solved.max[axis]) * 0.5) as f32;
            half[axis] = ((solved.max[axis] - solved.min[axis]) * 0.5) as f32;
        }
        Some((center, half))
    }

    pub(crate) fn selection_bounds_3d(&self, selection: Selection) -> Option<([f32; 3], [f32; 3])> {
        let grid = self.room_grid_view(selection.room())?;
        let mut bounds: Option<(f32, f32, f32, f32, f32, f32)> = None;
        for seed in drag_corner_seeds(selection)? {
            let world = face_corner_world(grid, seed)?;
            merge_bounds_3d(
                &mut bounds,
                [world[0] as f32, world[1] as f32, world[2] as f32],
                [0.0, 0.0, 0.0],
            );
        }
        bounds.map(bounds_3d_to_center_half)
    }

    pub(crate) fn current_frame_bounds_2d(&self) -> Option<([f32; 2], [f32; 2])> {
        if self.active_tool == ViewTool::Brush {
            if let Some((center, half)) = self.selected_brush_frame_bounds_3d() {
                return Some(project_center_half_2d(self.orthographic_view, center, half));
            }
        }

        // Legacy room-grid/node authoring remains a Top-view workflow. In
        // Front and Side, frame all BSP brushes when no brush is selected so
        // the alternate views never reinterpret grid XZ coordinates as XY.
        if self.orthographic_view != OrthographicView::Top {
            let mut bounds = None;
            for brush in &self.project.active_scene().brushes {
                let solved = brush.solve();
                if !solved.is_valid() {
                    continue;
                }
                let min = self.orthographic_view.project_f64(solved.min);
                let max = self.orthographic_view.project_f64(solved.max);
                merge_bounds(
                    &mut bounds,
                    [
                        ((min[0] + max[0]) * 0.5) as f32,
                        ((min[1] + max[1]) * 0.5) as f32,
                    ],
                    [
                        ((max[0] - min[0]) * 0.5) as f32,
                        ((max[1] - min[1]) * 0.5) as f32,
                    ],
                );
            }
            return bounds.map(bounds_to_center_half);
        }

        let mut bounds: Option<(f32, f32, f32, f32)> = None;
        for &(room, sx, sz) in &self.selection.selected_sectors {
            if let Some((center, half)) = self.sector_bounds_2d(room, sx, sz) {
                merge_bounds(&mut bounds, center, half);
            }
        }
        if let Some(bounds) = bounds {
            return Some(bounds_to_center_half(bounds));
        }

        let primitive_targets = self.selected_primitive_targets();
        if !primitive_targets.is_empty() {
            for selection in primitive_targets {
                let (room, sx, sz) = selection_sector(selection);
                if let Some((center, half)) = self.sector_bounds_2d(room, sx, sz) {
                    merge_bounds(&mut bounds, center, half);
                }
            }
            if let Some(bounds) = bounds {
                return Some(bounds_to_center_half(bounds));
            }
        }

        if let Some((sx, sz)) = self.selection.selected_sector {
            if let Some(room) = self.active_room_id() {
                return self.sector_bounds_2d(room, sx, sz);
            }
        }

        let selected_nodes = self.selected_node_ids_in_hierarchy();
        if selected_nodes.len() > 1 {
            let mut bounds = None;
            for id in selected_nodes {
                if let Some((center, half)) = self.node_frame_bounds_2d(id) {
                    merge_bounds(&mut bounds, center, half);
                }
            }
            if let Some(bounds) = bounds {
                return Some(bounds_to_center_half(bounds));
            }
        }

        self.node_frame_bounds_2d(self.selection.selected_node)
    }

    pub(crate) fn sector_bounds_2d(
        &self,
        room: NodeId,
        sx: u16,
        sz: u16,
    ) -> Option<([f32; 2], [f32; 2])> {
        let node = self.project.active_scene().node(room)?;
        let center = node_world(node);
        let grid = self.room_grid_view(room)?;
        if sx >= grid.width || sz >= grid.depth {
            return None;
        }
        let local = grid_cell_editor_center(grid, sx, sz);
        Some(([center[0] + local[0], center[1] + local[1]], [0.5, 0.5]))
    }

    pub(crate) fn drag_selected_node(&mut self, screen_delta: Vec2) {
        let selected = self.selected_node_ids_in_hierarchy();
        if selected.is_empty() || screen_delta == Vec2::ZERO {
            return;
        }

        let world_delta = [
            screen_delta.x / self.viewport_zoom,
            -screen_delta.y / self.viewport_zoom,
        ];
        let targets = {
            let scene = self.project.active_scene();
            selected
                .into_iter()
                .map(|id| (id, node_enclosing_sector_size(scene, id)))
                .collect::<Vec<_>>()
        };
        let mut moved = Vec::new();
        for (id, sector_size) in targets {
            if let Some(node) = self.project.active_scene_mut().node_mut(id) {
                node.transform.translation[0] += world_delta[0];
                node.transform.translation[2] += world_delta[1];
                if matches!(
                    node.kind,
                    NodeKind::Entity
                        | NodeKind::PointLight { .. }
                        | NodeKind::ImageProp { .. }
                        | NodeKind::BoxProp { .. }
                        | NodeKind::CylinderProp { .. }
                ) {
                    node.transform.translation[0] = snap_node_transform_component_to_world_step(
                        node.transform.translation[0],
                        sector_size,
                    );
                    node.transform.translation[2] = snap_node_transform_component_to_world_step(
                        node.transform.translation[2],
                        sector_size,
                    );
                }
                moved.push(node.name.clone());
            }
        }

        match moved.as_slice() {
            [] => {}
            [name] => {
                self.status = format!("Moved {name}");
                self.mark_dirty();
            }
            _ => {
                self.status = format!("Moved {} nodes", moved.len());
                self.mark_dirty();
            }
        }
    }
}
