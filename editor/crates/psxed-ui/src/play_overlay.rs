use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PlayDebugMapView {
    #[default]
    Rooms,
    Cells,
    Portals,
    Streaming,
}

impl PlayDebugMapView {
    pub(crate) const ALL: [Self; 4] = [Self::Rooms, Self::Cells, Self::Portals, Self::Streaming];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Rooms => "Rooms",
            Self::Cells => "Cells",
            Self::Portals => "Portals",
            Self::Streaming => "Streaming",
        }
    }

    pub(crate) fn parse(label: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|view| view.label().eq_ignore_ascii_case(label))
    }
}

pub(crate) fn human_bytes(n: u32) -> String {
    human_bytes_u64(n as u64)
}

pub(crate) fn human_bytes_u64(n: u64) -> String {
    if n < 1024 {
        format!("{} B", n)
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", (n as f64) / 1024.0)
    } else {
        format!("{:.1} MB", (n as f64) / (1024.0 * 1024.0))
    }
}

pub(crate) fn draw_play_overlay_icon_button(
    ui: &mut egui::Ui,
    rect: Rect,
    id_source: &'static str,
    icon: char,
    tooltip: &'static str,
    active: bool,
    enabled: bool,
    active_fill: Option<Color32>,
) -> bool {
    let response = ui
        .interact(
            rect,
            ui.id().with(("play_overlay_icon_button", id_source)),
            if enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        )
        .on_hover_text(tooltip);
    let hovered = response.hovered();
    let fill = if active {
        active_fill.unwrap_or(STUDIO_ACCENT_DIM)
    } else if hovered && enabled {
        Color32::from_rgba_unmultiplied(34, 48, 58, 232)
    } else if enabled {
        Color32::from_black_alpha(176)
    } else {
        Color32::from_rgba_unmultiplied(0, 0, 0, 112)
    };
    let stroke = if active {
        Stroke::new(1.0, STUDIO_ACCENT)
    } else if hovered && enabled {
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(210, 220, 235, 128))
    } else {
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(210, 220, 235, 84))
    };
    let icon_color = if !enabled {
        Color32::from_rgba_unmultiplied(142, 154, 168, 108)
    } else if hovered || active {
        Color32::WHITE
    } else {
        STUDIO_TEXT
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, fill);
    painter.rect_stroke(rect, 4.0, stroke, StrokeKind::Inside);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        icon.to_string(),
        icons::font(14.0),
        icon_color,
    );
    enabled && response.clicked()
}

#[derive(Clone, Copy)]
pub(crate) struct PlayChunkDebugMapCell {
    pub(crate) runtime_room_index: usize,
    pub(crate) project_room_id: NodeId,
    pub(crate) portal_room_index: usize,
    pub(crate) array_cell: [u16; 2],
    pub(crate) center: [f32; 2],
    pub(crate) half: [f32; 2],
    pub(crate) room_origin: [f32; 2],
    pub(crate) runtime_origin: [i32; 2],
    pub(crate) sector_size: f32,
    pub(crate) floor_index: usize,
    pub(crate) elevation: i32,
}

#[derive(Clone)]
pub(crate) struct PlayChunkDebugMapPortal {
    pub(crate) portal_index: usize,
    pub(crate) source_room_index: usize,
    pub(crate) destination_room_index: usize,
    pub(crate) a: [f32; 2],
    pub(crate) b: [f32; 2],
    pub(crate) vertices_world: [[i32; 3]; 4],
    pub(crate) direction: GridDirection,
    pub(crate) normal_world: [i16; 3],
    pub(crate) source_marker: Option<NodeId>,
    pub(crate) kind: u8,
    pub(crate) source_floor_index: usize,
    pub(crate) destination_floor_index: usize,
}

pub(crate) struct PlayChunkDebugMap {
    pub(crate) cells: Vec<PlayChunkDebugMapCell>,
    pub(crate) portals: Vec<PlayChunkDebugMapPortal>,
}

#[derive(Clone, Copy)]
pub(crate) struct PortalClipTraceEntry {
    pub(crate) portal_index: usize,
    pub(crate) parent: psx_level::portal_visibility::PortalFrustum,
    pub(crate) debug: psx_level::portal_visibility::PortalClipDebug,
    pub(crate) skipped_return: bool,
}

pub(crate) struct PortalClipTrace {
    pub(crate) camera: psx_level::portal_visibility::PortalVisibilityCamera,
    pub(crate) camera_global: [i32; 3],
    pub(crate) entries: Vec<PortalClipTraceEntry>,
}

impl PlayChunkDebugMap {
    pub(crate) fn runtime_room_count(&self) -> usize {
        self.cells
            .iter()
            .map(|cell| cell.runtime_room_index + 1)
            .max()
            .unwrap_or_default()
    }
}

pub(crate) fn collect_portal_clip_trace(
    map: &PlayChunkDebugMap,
    metrics: EditorPlaytestMetrics,
    current_room: Option<usize>,
) -> Option<PortalClipTrace> {
    let current_room = current_room?;
    let (camera, camera_global) = portal_clip_debug_camera(map, metrics, current_room)?;
    let root = psx_level::portal_visibility::PortalFrustum {
        room: psx_level::RoomIndex(current_room.min(u16::MAX as usize) as u16),
        source_room: psx_level::RoomIndex(u16::MAX),
        source_portal: u16::MAX,
        depth: 0,
        left_tan_q12: -camera.half_fov_x_tan_q12,
        right_tan_q12: camera.half_fov_x_tan_q12,
        min_y_tan_q12: -camera.half_fov_y_tan_q12,
        max_y_tan_q12: camera.half_fov_y_tan_q12,
    };
    let mut frustums = vec![root];
    let mut entries = Vec::new();
    let mut cursor = 0usize;
    while cursor < frustums.len() {
        let parent = frustums[cursor];
        cursor += 1;
        if parent.depth >= PLAY_PORTAL_DEBUG_MAX_DEPTH {
            continue;
        }
        for portal in map
            .portals
            .iter()
            .filter(|portal| portal.source_room_index == parent.room.to_usize())
        {
            let record = portal_level_record(portal);
            let debug = psx_level::portal_visibility::debug_portal_clip(record, camera, parent);
            let destination =
                psx_level::RoomIndex(portal.destination_room_index.min(u16::MAX as usize) as u16);
            let skipped_return =
                portal.destination_room_index == current_room || destination == parent.source_room;
            entries.push(PortalClipTraceEntry {
                portal_index: portal.portal_index,
                parent,
                debug,
                skipped_return,
            });
            if skipped_return
                || debug.decision != psx_level::portal_visibility::PortalClipDebugDecision::Accepted
            {
                continue;
            }
            let Some(bounds) = debug.result_bounds else {
                continue;
            };
            let child = psx_level::portal_visibility::PortalFrustum {
                room: destination,
                source_room: parent.room,
                source_portal: portal.portal_index.min(u16::MAX as usize) as u16,
                depth: parent.depth.saturating_add(1),
                left_tan_q12: bounds.left_tan_q12,
                right_tan_q12: bounds.right_tan_q12,
                min_y_tan_q12: bounds.min_y_tan_q12,
                max_y_tan_q12: bounds.max_y_tan_q12,
            };
            if !portal_debug_trace_contains_redundant_frustum(&frustums, child) {
                frustums.push(child);
            }
        }
    }
    Some(PortalClipTrace {
        camera,
        camera_global,
        entries,
    })
}

pub(crate) fn portal_clip_debug_camera(
    map: &PlayChunkDebugMap,
    metrics: EditorPlaytestMetrics,
    current_room: usize,
) -> Option<(
    psx_level::portal_visibility::PortalVisibilityCamera,
    [i32; 3],
)> {
    if !metrics.camera_view_basis_valid
        || (!metrics.camera_map_valid && !metrics.camera_global_valid)
    {
        return None;
    }
    if !metrics.camera_global_valid
        && (metrics.camera_local_y < -900_000 || metrics.camera_view_cos_pitch_q12 <= 0)
    {
        return None;
    }
    let (global_x, global_y, global_z) = if metrics.camera_global_valid {
        (
            metrics.camera_global_x,
            metrics.camera_global_y,
            metrics.camera_global_z,
        )
    } else {
        let room_cell = map
            .cells
            .iter()
            .find(|cell| cell.runtime_room_index == current_room)?;
        let sector_size = room_cell.sector_size.max(1.0);
        (
            room_cell.runtime_origin[0]
                .saturating_mul(sector_size.round().max(1.0) as i32)
                .saturating_add(metrics.camera_local_x),
            metrics.camera_local_y,
            room_cell.runtime_origin[1]
                .saturating_mul(sector_size.round().max(1.0) as i32)
                .saturating_add(metrics.camera_local_z),
        )
    };
    let half_fov_x = (PLAY_PORTAL_DEBUG_SCREEN_CX * 4096 / PLAY_PORTAL_DEBUG_FOCAL.max(1)).max(1);
    let half_fov_y = (PLAY_PORTAL_DEBUG_SCREEN_CY * 4096 / PLAY_PORTAL_DEBUG_FOCAL.max(1)).max(1);
    let camera = psx_level::portal_visibility::PortalVisibilityCamera::new(
        global_x,
        global_y,
        global_z,
        metrics.camera_view_sin_yaw_q12,
        metrics.camera_view_cos_yaw_q12,
        metrics.camera_view_sin_pitch_q12,
        metrics.camera_view_cos_pitch_q12,
        PLAY_PORTAL_DEBUG_NEAR_Z,
        PLAY_PORTAL_DEBUG_FAR_Z,
        half_fov_x,
        half_fov_y,
        PLAY_PORTAL_DEBUG_MIN_WIDTH_Q12,
    );
    Some((camera, [global_x, global_y, global_z]))
}

pub(crate) fn portal_level_record(
    portal: &PlayChunkDebugMapPortal,
) -> psx_level::LevelRoomPortalRecord {
    psx_level::LevelRoomPortalRecord {
        source_room: psx_level::RoomIndex(portal.source_room_index.min(u16::MAX as usize) as u16),
        destination_room: psx_level::RoomIndex(
            portal.destination_room_index.min(u16::MAX as usize) as u16,
        ),
        kind: portal.kind,
        normal_x: portal.normal_world[0],
        normal_y: portal.normal_world[1],
        normal_z: portal.normal_world[2],
        vertex_x: [
            portal.vertices_world[0][0],
            portal.vertices_world[1][0],
            portal.vertices_world[2][0],
            portal.vertices_world[3][0],
        ],
        vertex_y: [
            portal.vertices_world[0][1],
            portal.vertices_world[1][1],
            portal.vertices_world[2][1],
            portal.vertices_world[3][1],
        ],
        vertex_z: [
            portal.vertices_world[0][2],
            portal.vertices_world[1][2],
            portal.vertices_world[2][2],
            portal.vertices_world[3][2],
        ],
    }
}

pub(crate) fn portal_debug_trace_contains_redundant_frustum(
    frustums: &[psx_level::portal_visibility::PortalFrustum],
    frustum: psx_level::portal_visibility::PortalFrustum,
) -> bool {
    frustums.iter().any(|existing| {
        existing.room == frustum.room
            && existing.source_room == frustum.source_room
            && existing.source_portal == frustum.source_portal
            && existing.depth <= frustum.depth
            && existing.left_tan_q12 <= frustum.left_tan_q12
            && existing.right_tan_q12 >= frustum.right_tan_q12
            && existing.min_y_tan_q12 <= frustum.min_y_tan_q12
            && existing.max_y_tan_q12 >= frustum.max_y_tan_q12
    })
}

pub(crate) fn portal_clip_debug_rect_text(
    rect: Option<psx_level::portal_visibility::PortalClipDebugRect>,
) -> String {
    rect.map(|rect| {
        format!(
            "x=[{},{}] y=[{},{}]",
            rect.left_tan_q12, rect.right_tan_q12, rect.min_y_tan_q12, rect.max_y_tan_q12
        )
    })
    .unwrap_or_else(|| "none".to_owned())
}

pub(crate) fn portal_clip_debug_vertices_text(
    vertices: [psx_level::portal_visibility::PortalClipDebugVertex; 4],
) -> String {
    format!(
        "[({}, {}, {}), ({}, {}, {}), ({}, {}, {}), ({}, {}, {})]",
        vertices[0].x,
        vertices[0].y,
        vertices[0].z,
        vertices[1].x,
        vertices[1].y,
        vertices[1].z,
        vertices[2].x,
        vertices[2].y,
        vertices[2].z,
        vertices[3].x,
        vertices[3].y,
        vertices[3].z
    )
}

pub(crate) fn debug_parent_portal_label(portal: u16) -> String {
    if portal == u16::MAX {
        "root".to_owned()
    } else {
        format!("#{portal}")
    }
}

pub(crate) fn draw_play_chunk_debug_map(
    painter: &egui::Painter,
    viewport_rect: Rect,
    project: &ProjectDocument,
    metrics: EditorPlaytestMetrics,
    view: PlayDebugMapView,
) {
    let map = collect_play_chunk_debug_map(project);
    if map.cells.is_empty() {
        return;
    }

    let map_size = Vec2::new(410.0, 300.0);
    let mut map_rect = Rect::from_min_size(
        Pos2::new(
            viewport_rect.right() - map_size.x - 8.0,
            viewport_rect.top() + 44.0,
        ),
        map_size,
    );
    if map_rect.left() < viewport_rect.left() + 270.0 {
        map_rect = Rect::from_min_size(
            Pos2::new(
                viewport_rect.right() - map_size.x - 8.0,
                viewport_rect.bottom() - map_size.y - 8.0,
            ),
            map_size,
        );
    }

    let mut raw_min_x = f32::INFINITY;
    let mut raw_max_x = f32::NEG_INFINITY;
    let mut min_z = f32::INFINITY;
    let mut max_z = f32::NEG_INFINITY;
    for cell in &map.cells {
        raw_min_x = raw_min_x.min(cell.center[0] - cell.half[0]);
        raw_max_x = raw_max_x.max(cell.center[0] + cell.half[0]);
        min_z = min_z.min(cell.center[1] - cell.half[1]);
        max_z = max_z.max(cell.center[1] + cell.half[1]);
    }
    if !raw_min_x.is_finite() || !min_z.is_finite() {
        return;
    }
    // Explode authored floors into side-by-side lanes. The old overlay folded
    // every layer onto the same XZ footprint, producing the misleading stack
    // of boxes all labelled room 0 seen in the layer regression captures.
    let layer_stride = (raw_max_x - raw_min_x).max(1.0) + 1.5;
    let display_x = |x: f32, floor_index: usize| x + floor_index as f32 * layer_stride;
    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    for cell in &map.cells {
        min_x = min_x.min(display_x(cell.center[0] - cell.half[0], cell.floor_index));
        max_x = max_x.max(display_x(cell.center[0] + cell.half[0], cell.floor_index));
    }

    painter.rect_filled(map_rect, 4.0, Color32::from_black_alpha(176));
    painter.rect_stroke(
        map_rect,
        4.0,
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(210, 220, 235, 68)),
        StrokeKind::Inside,
    );
    painter.text(
        map_rect.left_top() + Vec2::new(8.0, 6.0),
        Align2::LEFT_TOP,
        format!("Room topology · {}", view.label()),
        FontId::monospace(11.0),
        STUDIO_TEXT,
    );
    // Live streaming dashboard, read alongside the coloured rooms: the resident
    // slot pool and its pressure, then the correctness faults. This makes the
    // map a self-contained view of the streaming system.
    let slot_limit = metrics.stream_slot_limit.max(1);
    let pool_color = if metrics.stream_protected_full > 0 {
        // Over budget: a visible room could not be granted a slot.
        Color32::from_rgb(255, 120, 120)
    } else if metrics.chunk_loaded >= slot_limit {
        // Pool full: every new load now costs an eviction.
        Color32::from_rgb(255, 200, 96)
    } else {
        STUDIO_TEXT_WEAK
    };
    painter.text(
        map_rect.left_top() + Vec2::new(8.0, 21.0),
        Align2::LEFT_TOP,
        format!(
            "rooms {}  layers {}  portals {}   pool {}/{}",
            map.runtime_room_count(),
            map.cells
                .iter()
                .map(|cell| cell.floor_index + 1)
                .max()
                .unwrap_or(1),
            map.portals.len(),
            metrics.chunk_loaded,
            metrics.stream_slot_limit
        ),
        FontId::monospace(9.0),
        pool_color,
    );
    let fault_total = metrics.portal_missing_resident
        + metrics.portal_build_failed
        + metrics.stream_failed
        + metrics.stream_protected_full
        + metrics.room_visibility_fallback_draws;
    let fault_color = if fault_total > 0 {
        Color32::from_rgb(255, 120, 120)
    } else {
        STUDIO_TEXT_WEAK
    };
    painter.text(
        map_rect.left_top() + Vec2::new(8.0, 33.0),
        Align2::LEFT_TOP,
        format!(
            "vis {}  drawn {}  load {}  miss {}  pvs-fallback {}",
            metrics.portal_visible_rooms,
            metrics.chunk_drawn_mask.count_ones(),
            metrics.stream_pending,
            metrics.portal_missing_resident,
            metrics.room_visibility_fallback_draws
        ),
        FontId::monospace(9.0),
        fault_color,
    );
    // VRAM room-texture residency overflow: the silent missing-texture signal.
    // `vdrop` is the symptom (materials left untextured); the rest attribute it to
    // the binding cap (slot table / room window / CLUT / upload queue).
    let vram_total = metrics.vram_texture_drops
        + metrics.vram_caps_full[0]
        + metrics.vram_caps_full[1]
        + metrics.vram_caps_full[2]
        + metrics.vram_caps_full[3]
        + metrics.room_material_slot_overflow;
    let vram_color = if vram_total > 0 {
        Color32::from_rgb(255, 120, 120)
    } else {
        STUDIO_TEXT_WEAK
    };
    if vram_total > 0 {
        painter.text(
            map_rect.right_top() + Vec2::new(-8.0, 6.0),
            Align2::RIGHT_TOP,
            format!("VRAM fault {vram_total}"),
            FontId::monospace(9.0),
            vram_color,
        );
    }

    let plot = Rect::from_min_max(
        map_rect.left_top() + Vec2::new(8.0, 62.0),
        map_rect.right_bottom() - Vec2::new(8.0, 78.0),
    );
    let world_w = (max_x - min_x).max(1.0);
    let world_h = (max_z - min_z).max(1.0);
    let scale = (plot.width() / world_w).min(plot.height() / world_h);
    let content_w = world_w * scale;
    let content_h = world_h * scale;
    let origin = Pos2::new(
        plot.left() + (plot.width() - content_w) * 0.5,
        plot.top() + (plot.height() - content_h) * 0.5,
    );
    let map_x = |x: f32| origin.x + (x - min_x) * scale;
    let map_z = |z: f32| origin.y + (z - min_z) * scale;

    let mut layer_labels = std::collections::BTreeMap::new();
    for cell in &map.cells {
        layer_labels
            .entry(cell.floor_index)
            .or_insert((cell.elevation, cell.center[0] - cell.half[0]));
    }
    for (floor_index, (elevation, lane_x)) in layer_labels {
        painter.text(
            Pos2::new(map_x(display_x(lane_x, floor_index)), plot.top() - 11.0),
            Align2::LEFT_BOTTOM,
            format!("L{} · y {}", floor_index + 1, elevation),
            FontId::monospace(8.5),
            STUDIO_TEXT_WEAK,
        );
    }

    // Each room is shaded by its innermost membership in the three rings of the
    // streaming model, read as a heat ramp out from the player:
    //   COLLISION ring  (red)         = the current room, where the motor runs.
    //   VISIBILITY ring (green/amber) = portal-visible rooms, built and drawn.
    //   STREAMING ring  (blue/slate)  = resident prefetch buffer + in-flight loads.
    // Faults overlay the rings: a visible room that is not resident ("missing")
    // or resident-but-unbuilt ("build fail") is a correctness problem and shows
    // hot pink/red. Rooms outside every ring stay unfilled.
    for cell in &map.cells {
        let bit = debug_chunk_bit(cell.runtime_room_index);
        let is_current = metrics.player_map_valid
            && cell.runtime_room_index == metrics.player_room_index as usize;
        let loaded = bit != 0 && metrics.chunk_loaded_mask & bit != 0;
        let loading = bit != 0 && metrics.chunk_loading_mask & bit != 0;
        let active = bit != 0 && metrics.chunk_active_mask & bit != 0;
        let visible = bit != 0 && metrics.portal_visible_mask & bit != 0;
        let frontier = bit != 0 && metrics.portal_frontier_mask & bit != 0;
        let missing = bit != 0 && metrics.portal_missing_mask & bit != 0;
        let build_failed = bit != 0 && metrics.portal_build_failed_mask & bit != 0;
        let drawn = bit != 0 && metrics.chunk_drawn_mask & bit != 0;
        let rect = Rect::from_min_max(
            Pos2::new(
                map_x(display_x(cell.center[0] - cell.half[0], cell.floor_index)),
                map_z(cell.center[1] - cell.half[1]),
            ),
            Pos2::new(
                map_x(display_x(cell.center[0] + cell.half[0], cell.floor_index)),
                map_z(cell.center[1] + cell.half[1]),
            ),
        )
        .shrink(0.75);
        let (fill, stroke) = if view != PlayDebugMapView::Streaming {
            let hue = (cell.runtime_room_index as u8).wrapping_mul(37);
            let base = Color32::from_rgb(
                66u8.saturating_add(hue / 5),
                98u8.saturating_add(hue / 7),
                132u8.saturating_add(hue / 9),
            );
            let fill_alpha = match view {
                PlayDebugMapView::Rooms => 92,
                PlayDebugMapView::Cells => 38,
                PlayDebugMapView::Portals => 20,
                PlayDebugMapView::Streaming => unreachable!(),
            };
            (
                Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), fill_alpha),
                Stroke::new(
                    if is_current { 2.0 } else { 1.0 },
                    if is_current {
                        Color32::from_rgb(255, 138, 96)
                    } else {
                        base
                    },
                ),
            )
        } else if is_current {
            // Collision ring: the room the player occupies.
            (
                Color32::from_rgba_unmultiplied(240, 96, 64, 150),
                Stroke::new(2.2, Color32::from_rgb(255, 138, 96)),
            )
        } else if build_failed {
            // Fault: visible + resident but its surface cache would not build.
            (
                Color32::from_rgba_unmultiplied(232, 76, 196, 112),
                Stroke::new(1.8, Color32::from_rgb(255, 92, 214)),
            )
        } else if missing {
            // Fault: visible but neither resident nor loading.
            (
                Color32::from_rgba_unmultiplied(210, 40, 60, 120),
                Stroke::new(1.8, Color32::from_rgb(245, 70, 90)),
            )
        } else if drawn {
            // Visibility ring: rendered this frame.
            (
                Color32::from_rgba_unmultiplied(42, 214, 124, 150),
                Stroke::new(1.6, Color32::from_rgb(72, 255, 152)),
            )
        } else if visible {
            // Visibility ring: portal-accepted, not yet drawn.
            (
                Color32::from_rgba_unmultiplied(244, 170, 48, 96),
                Stroke::new(1.7, Color32::from_rgb(255, 184, 58)),
            )
        } else if loading {
            // Streaming ring: load in flight.
            (
                Color32::from_rgba_unmultiplied(72, 150, 255, 100),
                Stroke::new(1.8, Color32::from_rgb(110, 188, 255)),
            )
        } else if active {
            // Streaming ring: built and ready (surface cache staged) but not
            // visible -- the warm prefetch that makes the next crossing instant.
            (
                Color32::from_rgba_unmultiplied(96, 150, 190, 92),
                Stroke::new(1.4, Color32::from_rgb(140, 190, 224)),
            )
        } else if loaded {
            // Streaming ring: resident bytes only, surface cache not built yet
            // -- the cold prefetch buffer.
            (
                Color32::from_rgba_unmultiplied(84, 104, 144, 52),
                Stroke::new(1.2, Color32::from_rgb(116, 140, 180)),
            )
        } else if frontier {
            // Beyond the rings: portal-traversal depth/capacity frontier.
            (
                Color32::from_rgba_unmultiplied(0, 0, 0, 0),
                Stroke::new(1.4, Color32::from_rgb(150, 120, 210)),
            )
        } else {
            // Outside every ring.
            (
                Color32::from_rgba_unmultiplied(0, 0, 0, 0),
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(210, 220, 235, 90)),
            )
        };
        if fill.a() > 0 {
            painter.rect_filled(rect, 0.0, fill);
        }
        painter.rect_stroke(rect, 0.0, stroke, StrokeKind::Inside);
        if view == PlayDebugMapView::Cells && rect.width() >= 8.0 && rect.height() >= 8.0 {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                format!("{},{}", cell.array_cell[0], cell.array_cell[1]),
                FontId::monospace(5.5),
                STUDIO_TEXT_WEAK,
            );
        }
    }

    // Label each runtime room once at its centroid instead of repeating the
    // same index in every cell. This keeps dense room grids readable.
    let mut room_centroids: HashMap<usize, ([f32; 2], usize, usize)> = HashMap::new();
    for cell in &map.cells {
        let entry = room_centroids.entry(cell.runtime_room_index).or_insert((
            [0.0, 0.0],
            0,
            cell.floor_index,
        ));
        entry.0[0] += cell.center[0];
        entry.0[1] += cell.center[1];
        entry.1 += 1;
    }
    for (room_index, (sum, count, floor_index)) in room_centroids {
        if view == PlayDebugMapView::Cells {
            continue;
        }
        let count = count.max(1) as f32;
        let pos = Pos2::new(
            map_x(display_x(sum[0] / count, floor_index)),
            map_z(sum[1] / count),
        );
        painter.circle_filled(pos, 7.0, Color32::from_black_alpha(190));
        painter.text(
            pos,
            Align2::CENTER_CENTER,
            room_index.to_string(),
            FontId::monospace(8.5),
            Color32::WHITE,
        );
    }

    let clipped = painter.with_clip_rect(plot);
    for portal in &map.portals {
        let source_bit = debug_chunk_bit(portal.source_room_index);
        let dest_bit = debug_chunk_bit(portal.destination_room_index);
        let portal_bit = debug_chunk_bit(portal.portal_index);
        let source_visible = source_bit != 0 && metrics.portal_visible_mask & source_bit != 0;
        let dest_frontier = dest_bit != 0 && metrics.portal_frontier_mask & dest_bit != 0;
        let dest_loading = dest_bit != 0 && metrics.chunk_loading_mask & dest_bit != 0;
        let dest_build_failed = dest_bit != 0 && metrics.portal_build_failed_mask & dest_bit != 0;
        let portal_accepted =
            portal_bit != 0 && metrics.portal_accepted_portal_mask & portal_bit != 0;
        let portal_rejected =
            portal_bit != 0 && metrics.portal_reject_frustum_portal_mask & portal_bit != 0;
        let color = if source_visible && portal_accepted {
            Color32::from_rgb(255, 190, 72)
        } else if source_visible && dest_build_failed {
            Color32::from_rgb(255, 92, 214)
        } else if source_visible && dest_loading {
            Color32::from_rgb(96, 178, 255)
        } else if source_visible && portal_rejected {
            Color32::from_rgb(255, 142, 48)
        } else if source_visible && dest_frontier {
            Color32::from_rgb(172, 128, 255)
        } else {
            Color32::from_rgba_unmultiplied(210, 220, 235, 112)
        };
        let (a, b) = if portal.source_floor_index == portal.destination_floor_index {
            directed_portal_map_segment(
                Pos2::new(
                    map_x(display_x(portal.a[0], portal.source_floor_index)),
                    map_z(portal.a[1]),
                ),
                Pos2::new(
                    map_x(display_x(portal.b[0], portal.source_floor_index)),
                    map_z(portal.b[1]),
                ),
                portal.source_room_index,
                portal.destination_room_index,
            )
        } else {
            let midpoint = [
                (portal.a[0] + portal.b[0]) * 0.5,
                (portal.a[1] + portal.b[1]) * 0.5,
            ];
            (
                Pos2::new(
                    map_x(display_x(midpoint[0], portal.source_floor_index)),
                    map_z(midpoint[1]),
                ),
                Pos2::new(
                    map_x(display_x(midpoint[0], portal.destination_floor_index)),
                    map_z(midpoint[1]),
                ),
            )
        };
        clipped.line_segment(
            [a, b],
            Stroke::new(
                if view == PlayDebugMapView::Portals {
                    2.8
                } else {
                    2.0
                },
                color,
            ),
        );
        if source_visible && portal_rejected && !portal_accepted {
            draw_rejected_portal_marker(&clipped, a, b);
        }
        if view == PlayDebugMapView::Portals
            || source_visible && (portal_accepted || portal_rejected)
        {
            let label = if portal_rejected && !portal_accepted {
                format!("#{} R", portal.portal_index)
            } else {
                format!(
                    "#{} {}→{}",
                    portal.portal_index, portal.source_room_index, portal.destination_room_index
                )
            };
            clipped.text(
                a.lerp(b, 0.5) + Vec2::new(0.0, -7.0),
                Align2::CENTER_CENTER,
                label,
                FontId::monospace(8.5),
                color,
            );
        }
    }

    if metrics.player_map_valid {
        if let Some(cell) = map
            .cells
            .iter()
            .find(|cell| cell.runtime_room_index == metrics.player_room_index as usize)
        {
            let sector_size = cell.sector_size.max(1.0);
            let local_to_map_pos = |local_x: i32, local_z: i32| {
                let x = cell.room_origin[0] + local_x as f32 / sector_size;
                let z = cell.room_origin[1] + local_z as f32 / sector_size;
                Pos2::new(map_x(display_x(x, cell.floor_index)), map_z(z))
            };
            let global_to_map_pos = |cell: &PlayChunkDebugMapCell, global_x: i32, global_z: i32| {
                let sector_size = cell.sector_size.max(1.0);
                let x = cell.room_origin[0] + global_x as f32 / sector_size
                    - cell.runtime_origin[0] as f32;
                let z = cell.room_origin[1] + global_z as f32 / sector_size
                    - cell.runtime_origin[1] as f32;
                Pos2::new(map_x(display_x(x, cell.floor_index)), map_z(z))
            };
            let player_pos = local_to_map_pos(metrics.player_local_x, metrics.player_local_z);
            let camera_pos = if metrics.camera_global_valid {
                let camera_cell = map
                    .cells
                    .iter()
                    .find(|cell| {
                        cell.runtime_room_index == metrics.portal_current_room_index as usize
                    })
                    .unwrap_or(cell);
                global_to_map_pos(
                    camera_cell,
                    metrics.camera_global_x,
                    metrics.camera_global_z,
                )
            } else if metrics.camera_map_valid {
                local_to_map_pos(metrics.camera_local_x, metrics.camera_local_z)
            } else {
                player_pos
            };
            if player_pos.x.is_finite()
                && player_pos.y.is_finite()
                && camera_pos.x.is_finite()
                && camera_pos.y.is_finite()
            {
                let yaw = metrics.player_view_yaw_q12 as f32 * std::f32::consts::TAU / 4096.0;
                let forward = if metrics.camera_view_basis_valid {
                    let basis = Vec2::new(
                        -(metrics.camera_view_sin_yaw_q12 as f32) / 4096.0,
                        -(metrics.camera_view_cos_yaw_q12 as f32) / 4096.0,
                    );
                    let len = basis.length();
                    if len > 0.001 {
                        basis / len
                    } else {
                        Vec2::new(-yaw.sin(), -yaw.cos())
                    }
                } else {
                    Vec2::new(-yaw.sin(), -yaw.cos())
                };
                let cone_len = (content_w * content_w + content_h * content_h)
                    .sqrt()
                    .max(42.0);
                let cone_half_angle = (160.0_f32 / 320.0_f32).atan();
                let left = rotate_vec2(forward, -cone_half_angle) * cone_len;
                let right = rotate_vec2(forward, cone_half_angle) * cone_len;
                let clipped = painter.with_clip_rect(plot);
                if (metrics.camera_map_valid || metrics.camera_global_valid)
                    && player_pos.distance(camera_pos) > 2.0
                {
                    clipped.line_segment(
                        [player_pos, camera_pos],
                        Stroke::new(1.0, Color32::from_rgba_unmultiplied(232, 240, 252, 160)),
                    );
                }
                clipped.add(egui::Shape::convex_polygon(
                    vec![camera_pos, camera_pos + left, camera_pos + right],
                    Color32::from_rgba_unmultiplied(235, 240, 248, 44),
                    Stroke::new(1.2, Color32::from_rgb(232, 240, 252)),
                ));
                clipped.line_segment(
                    [camera_pos, camera_pos + forward * (cone_len * 0.82)],
                    Stroke::new(1.2, Color32::from_rgb(218, 244, 255)),
                );
                clipped.circle_filled(player_pos, 2.6, Color32::from_rgb(72, 255, 152));
                clipped.circle_stroke(player_pos, 2.6, Stroke::new(1.0, Color32::BLACK));
                clipped.circle_filled(camera_pos, 3.5, Color32::from_rgb(245, 250, 255));
                clipped.circle_stroke(camera_pos, 3.5, Stroke::new(1.0, Color32::BLACK));
            }
        }
    }

    // Three-ring legend: one row per ring (collision / visibility / streaming),
    // tagged with the ring name, plus a faults row for the correctness signals.
    let legend_x = map_rect.left() + 8.0;
    let legend_bottom = map_rect.bottom();
    let swatch_x = legend_x + 52.0;
    let ring_tag = |y_off: f32, text: &str| {
        painter.text(
            Pos2::new(legend_x, legend_bottom + y_off - 1.0),
            Align2::LEFT_TOP,
            text,
            FontId::monospace(9.0),
            STUDIO_TEXT,
        );
    };
    // Collision ring: the current room.
    ring_tag(-70.0, "collide");
    draw_chunk_map_legend_item(
        painter,
        Pos2::new(swatch_x, legend_bottom - 70.0),
        Color32::from_rgba_unmultiplied(240, 96, 64, 150),
        Color32::from_rgb(255, 138, 96),
        "current",
    );
    // Visibility ring: built + drawn portal-visible rooms.
    ring_tag(-53.0, "visible");
    draw_chunk_map_legend_item(
        painter,
        Pos2::new(swatch_x, legend_bottom - 53.0),
        Color32::from_rgba_unmultiplied(42, 214, 124, 150),
        Color32::from_rgb(72, 255, 152),
        "drawn",
    );
    draw_chunk_map_legend_item(
        painter,
        Pos2::new(swatch_x + 78.0, legend_bottom - 53.0),
        Color32::from_rgba_unmultiplied(244, 170, 48, 96),
        Color32::from_rgb(255, 184, 58),
        "accepted",
    );
    // Portal-traversal frontier: reached but cut at the depth/capacity edge.
    draw_chunk_map_legend_item(
        painter,
        Pos2::new(swatch_x + 166.0, legend_bottom - 53.0),
        Color32::from_rgba_unmultiplied(0, 0, 0, 0),
        Color32::from_rgb(150, 120, 210),
        "frontier",
    );
    // Streaming ring: in-flight load, built/ready prefetch, resident bytes only.
    ring_tag(-36.0, "stream");
    draw_chunk_map_legend_item(
        painter,
        Pos2::new(swatch_x, legend_bottom - 36.0),
        Color32::from_rgba_unmultiplied(72, 150, 255, 100),
        Color32::from_rgb(110, 188, 255),
        "loading",
    );
    draw_chunk_map_legend_item(
        painter,
        Pos2::new(swatch_x + 70.0, legend_bottom - 36.0),
        Color32::from_rgba_unmultiplied(96, 150, 190, 92),
        Color32::from_rgb(140, 190, 224),
        "built",
    );
    draw_chunk_map_legend_item(
        painter,
        Pos2::new(swatch_x + 136.0, legend_bottom - 36.0),
        Color32::from_rgba_unmultiplied(84, 104, 144, 52),
        Color32::from_rgb(116, 140, 180),
        "resident",
    );
    // Faults: visible rooms that should be resident + built but are not.
    ring_tag(-19.0, "fault");
    draw_chunk_map_legend_item(
        painter,
        Pos2::new(swatch_x, legend_bottom - 19.0),
        Color32::from_rgba_unmultiplied(210, 40, 60, 120),
        Color32::from_rgb(245, 70, 90),
        "missing",
    );
    draw_chunk_map_legend_item(
        painter,
        Pos2::new(swatch_x + 82.0, legend_bottom - 19.0),
        Color32::from_rgba_unmultiplied(232, 76, 196, 112),
        Color32::from_rgb(255, 92, 214),
        "build fail",
    );
}

pub(crate) fn collect_play_chunk_debug_map(project: &ProjectDocument) -> PlayChunkDebugMap {
    let topology = psxed_project::playtest::build_debug_topology(project);
    let scene = project.active_scene();
    let cells: Vec<_> = topology
        .cells
        .iter()
        .filter_map(|cell| {
            let project_room_id = scene
                .nodes()
                .iter()
                .find(|node| node.id.raw() == u64::from(cell.authored_room))?
                .id;
            Some(PlayChunkDebugMapCell {
                runtime_room_index: cell.runtime_room_index,
                project_room_id,
                portal_room_index: cell.portal_room_index,
                array_cell: cell.array_cell,
                center: cell.center,
                half: cell.half,
                room_origin: cell.room_origin,
                runtime_origin: cell.runtime_origin,
                sector_size: cell.sector_size.max(1) as f32,
                floor_index: cell.floor_index,
                elevation: cell.elevation,
            })
        })
        .collect();
    let portals = topology
        .portals
        .iter()
        .filter_map(|topology_portal| {
            let portal = topology_portal.portal;
            let source = cells
                .iter()
                .find(|cell| cell.runtime_room_index == portal.source_room as usize)?;
            let destination = cells
                .iter()
                .find(|cell| cell.runtime_room_index == portal.destination_room as usize)?;
            let to_map = |cell: &PlayChunkDebugMapCell, vertex: [i32; 3]| {
                [
                    cell.room_origin[0] + vertex[0] as f32 / cell.sector_size
                        - cell.runtime_origin[0] as f32,
                    cell.room_origin[1] + vertex[2] as f32 / cell.sector_size
                        - cell.runtime_origin[1] as f32,
                ]
            };
            Some(PlayChunkDebugMapPortal {
                portal_index: topology_portal.portal_index,
                source_room_index: portal.source_room as usize,
                destination_room_index: portal.destination_room as usize,
                a: to_map(source, portal.vertices[0]),
                b: to_map(source, portal.vertices[1]),
                vertices_world: portal.vertices,
                direction: debug_portal_direction(portal.normal),
                normal_world: portal.normal,
                source_marker: None,
                kind: portal.kind,
                source_floor_index: source.floor_index,
                destination_floor_index: destination.floor_index,
            })
        })
        .collect();
    PlayChunkDebugMap { cells, portals }
}

fn debug_portal_direction(normal: [i16; 3]) -> GridDirection {
    match normal {
        [0, 0, -1] => GridDirection::North,
        [-1, 0, 0] => GridDirection::East,
        [0, 0, 1] => GridDirection::South,
        [1, 0, 0] => GridDirection::West,
        _ => GridDirection::NorthWestSouthEast,
    }
}

pub(crate) fn debug_chunk_bit(index: usize) -> u64 {
    if index < u64::BITS as usize {
        1u64 << index
    } else {
        0
    }
}

pub(crate) fn debug_room_flags(metrics: EditorPlaytestMetrics, index: usize) -> String {
    let bit = debug_chunk_bit(index);
    let flag = |mask: u64| bit != 0 && mask & bit != 0;
    format!(
        "loaded={} loading={} active={} drawn={} visible={} frontier={} missing={} build_failed={} tested={} accepted={} reject_frustum={} bounds_fb={}",
        flag(metrics.chunk_loaded_mask),
        flag(metrics.chunk_loading_mask),
        flag(metrics.chunk_active_mask),
        flag(metrics.chunk_drawn_mask),
        flag(metrics.portal_visible_mask),
        flag(metrics.portal_frontier_mask),
        flag(metrics.portal_missing_mask),
        flag(metrics.portal_build_failed_mask),
        flag(metrics.portal_tested_mask),
        flag(metrics.portal_accepted_mask),
        flag(metrics.portal_reject_frustum_mask),
        flag(metrics.portal_bounds_fallback_mask)
    )
}

pub(crate) fn q12_degrees(angle: u16) -> f32 {
    angle as f32 * 360.0 / 4096.0
}

pub(crate) fn rotate_vec2(v: Vec2, radians: f32) -> Vec2 {
    let (sin, cos) = radians.sin_cos();
    Vec2::new(v.x * cos - v.y * sin, v.x * sin + v.y * cos)
}

pub(crate) fn directed_portal_map_segment(
    a: Pos2,
    b: Pos2,
    source_room_index: usize,
    destination_room_index: usize,
) -> (Pos2, Pos2) {
    let edge = b - a;
    let len = (edge.x * edge.x + edge.y * edge.y).sqrt();
    if len <= 0.001 {
        return (a, b);
    }
    let side = if source_room_index <= destination_room_index {
        1.0
    } else {
        -1.0
    };
    let offset = Vec2::new(-edge.y / len, edge.x / len) * (1.6 * side);
    (a + offset, b + offset)
}

pub(crate) fn draw_rejected_portal_marker(painter: &egui::Painter, a: Pos2, b: Pos2) {
    let edge = b - a;
    let len = (edge.x * edge.x + edge.y * edge.y).sqrt();
    if len <= 0.001 {
        return;
    }
    let tangent = edge / len;
    let normal = Vec2::new(-tangent.y, tangent.x);
    let center = a + edge * 0.5;
    let half_a = tangent * 4.5 + normal * 4.5;
    let half_b = tangent * 4.5 - normal * 4.5;
    painter.line_segment(
        [center - half_a, center + half_a],
        Stroke::new(2.4, Color32::from_rgba_unmultiplied(20, 14, 10, 170)),
    );
    painter.line_segment(
        [center - half_a, center + half_a],
        Stroke::new(1.2, Color32::from_rgb(255, 238, 204)),
    );
    painter.line_segment(
        [center - half_b, center + half_b],
        Stroke::new(2.4, Color32::from_rgba_unmultiplied(20, 14, 10, 170)),
    );
    painter.line_segment(
        [center - half_b, center + half_b],
        Stroke::new(1.2, Color32::from_rgb(255, 238, 204)),
    );
}

pub(crate) fn draw_chunk_map_legend_item(
    painter: &egui::Painter,
    pos: Pos2,
    fill: Color32,
    stroke: Color32,
    label: &str,
) {
    let swatch = Rect::from_min_size(pos, Vec2::new(9.0, 9.0));
    if fill.a() > 0 {
        painter.rect_filled(swatch, 0.0, fill);
    }
    painter.rect_stroke(swatch, 0.0, Stroke::new(1.0, stroke), StrokeKind::Inside);
    painter.text(
        pos + Vec2::new(13.0, -1.0),
        Align2::LEFT_TOP,
        label,
        FontId::monospace(10.0),
        STUDIO_TEXT_WEAK,
    );
}

pub(crate) fn play_frame_rate_from_ms(frame_ms: f32) -> f32 {
    if frame_ms > 0.0 && frame_ms.is_finite() {
        (1000.0 / frame_ms).clamp(0.0, PLAY_FRAME_TARGET_FPS)
    } else {
        0.0
    }
}

pub(crate) fn draw_play_frame_rate_chart(
    painter: &egui::Painter,
    rect: Rect,
    samples: &VecDeque<f32>,
) {
    painter.rect_filled(rect, 3.0, Color32::from_black_alpha(88));
    painter.rect_stroke(
        rect,
        3.0,
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(180, 200, 220, 64)),
        StrokeKind::Inside,
    );
    let plot = rect.shrink2(Vec2::new(2.0, 3.0));
    let target_y = plot.top();
    painter.line_segment(
        [
            Pos2::new(plot.left(), target_y),
            Pos2::new(plot.right(), target_y),
        ],
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(235, 240, 248, 126)),
    );

    if samples.len() < 2 {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "VIS fps",
            FontId::monospace(9.0),
            STUDIO_TEXT_WEAK,
        );
        return;
    }

    let step = plot.width() / (samples.len().saturating_sub(1) as f32).max(1.0);
    let mut fps_points = Vec::with_capacity(samples.len());
    let fill = Color32::from_rgba_unmultiplied(42, 214, 124, 38);
    for (index, frame_ms) in samples.iter().copied().enumerate() {
        let rate = play_frame_rate_from_ms(frame_ms);
        let t = (rate / PLAY_FRAME_TARGET_FPS).clamp(0.0, 1.0);
        let point = Pos2::new(
            plot.left() + step * index as f32,
            plot.bottom() - plot.height() * t,
        );
        let x0 = if index == 0 {
            plot.left()
        } else {
            point.x - step * 0.5
        };
        let x1 = if index + 1 == samples.len() {
            plot.right()
        } else {
            point.x + step * 0.5
        };
        painter.rect_filled(
            Rect::from_min_max(Pos2::new(x0, point.y), Pos2::new(x1, plot.bottom())),
            0.0,
            fill,
        );
        fps_points.push(point);
    }
    painter.add(egui::Shape::line(
        fps_points,
        Stroke::new(1.2, Color32::from_rgba_unmultiplied(150, 238, 172, 184)),
    ));
}

pub(crate) fn draw_play_metric_line(
    painter: &egui::Painter,
    x: f32,
    y: &mut f32,
    text: &str,
    color: Color32,
) {
    painter.text(
        Pos2::new(x, *y),
        Align2::LEFT_TOP,
        text,
        FontId::monospace(11.0),
        color,
    );
    *y += 13.0;
}
