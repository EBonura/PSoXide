use super::*;

pub(crate) fn dock_label_limit(depth: usize) -> usize {
    LEFT_DOCK_LABEL_CHARS
        .saturating_sub(depth.saturating_mul(2))
        .max(18)
}

pub(crate) fn compact_middle(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars || max_chars < 8 {
        return text.to_string();
    }

    let marker = "...";
    let room = max_chars.saturating_sub(marker.len());
    let head = room.saturating_mul(2) / 3;
    let tail = room.saturating_sub(head);
    let mut out = String::with_capacity(text.len().min(max_chars + marker.len()));
    out.extend(text.chars().take(head));
    out.push_str(marker);
    let mut suffix = text.chars().rev().take(tail).collect::<Vec<_>>();
    suffix.reverse();
    out.extend(suffix);
    out
}

fn tree_row_text_regions(
    rect: Rect,
    text_left: f32,
    show_detail: bool,
    has_child_count: bool,
) -> (f32, Option<Rect>) {
    let accessories_left = rect.right() - if has_child_count { 56.0 } else { 28.0 };
    let available = (accessories_left - text_left).max(0.0);
    if !show_detail || available < 116.0 {
        return (accessories_left.max(text_left), None);
    }

    let detail_width = (available * 0.45).clamp(56.0, 92.0);
    let detail_left = accessories_left - detail_width;
    let detail_rect = Rect::from_min_max(
        Pos2::new(detail_left, rect.top()),
        Pos2::new(accessories_left, rect.bottom()),
    );
    ((detail_left - 6.0).max(text_left), Some(detail_rect))
}

pub(crate) fn scene_node_label(scene: &psxed_project::Scene, id: NodeId) -> String {
    scene
        .node(id)
        .map(|node| node.name.clone())
        .unwrap_or_else(|| format!("#{}", id.raw()))
}

pub(crate) fn draw_room_connections_rows(
    ui: &mut egui::Ui,
    scene: &psxed_project::Scene,
    connections: &[RoomConnection],
    filter: &str,
    selected_node: NodeId,
    selected_nodes: &HashSet<NodeId>,
    selected: &mut Option<NodeId>,
    repair: &mut Option<NodeId>,
) {
    let visible = connections
        .iter()
        .filter(|connection| room_connection_matches_filter(scene, connection, filter))
        .collect::<Vec<_>>();
    let repair_count = connections
        .iter()
        .filter(|connection| connection.status.needs_repair())
        .count();
    let heading = if repair_count > 0 {
        format!(
            "Connections ({}, {} repair)",
            connections.len(),
            repair_count
        )
    } else {
        format!("Connections ({})", connections.len())
    };
    ui.horizontal(|ui| {
        ui.label(icons::text(icons::WAYPOINT, 13.0).color(STUDIO_TEXT_WEAK));
        ui.strong(heading);
    });
    if connections.is_empty() {
        ui.weak("No room portals yet.");
        return;
    }
    if visible.is_empty() {
        ui.weak("No connections match the filter.");
        return;
    }

    for connection in visible {
        draw_room_connection_row(
            ui,
            scene,
            connection,
            selected_node,
            selected_nodes,
            selected,
            repair,
        );
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum RoomFloorLinkDirection {
    Above,
    Below,
}

impl RoomFloorLinkDirection {
    const fn label(self) -> &'static str {
        match self {
            Self::Above => "above",
            Self::Below => "below",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RoomFloorLinkRow {
    pub(crate) source_room: NodeId,
    pub(crate) target_room: Option<NodeId>,
    pub(crate) target_floor: u16,
    pub(crate) x: u16,
    pub(crate) z: u16,
    pub(crate) direction: RoomFloorLinkDirection,
}

pub(crate) fn draw_room_floor_link_rows(
    ui: &mut egui::Ui,
    scene: &psxed_project::Scene,
    filter: &str,
) {
    let rows = collect_room_floor_link_rows(scene);
    let visible = rows
        .iter()
        .copied()
        .filter(|row| room_floor_link_matches_filter(scene, *row, filter))
        .collect::<Vec<_>>();
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(icons::text(icons::LAYERS, 13.0).color(STUDIO_TEXT_WEAK));
        ui.strong(format!("Floors ({})", rows.len()));
    });
    if rows.is_empty() {
        ui.weak("No vertical floor links yet.");
        return;
    }
    if visible.is_empty() {
        ui.weak("No floor links match the filter.");
        return;
    }

    let max_rows = 24usize;
    for row in visible.iter().take(max_rows) {
        draw_room_floor_link_row(ui, scene, *row);
    }
    if visible.len() > max_rows {
        ui.weak(format!("... {} more floor links", visible.len() - max_rows));
    }
}

pub(crate) fn collect_room_floor_link_rows(scene: &psxed_project::Scene) -> Vec<RoomFloorLinkRow> {
    let mut rows = Vec::new();
    for node in scene.nodes() {
        let NodeKind::Room { grid } = &node.kind else {
            continue;
        };
        for x in 0..grid.width {
            for z in 0..grid.depth {
                let Some(sector) = grid.sector(x, z) else {
                    continue;
                };
                if let Some(link) = sector.floor_above {
                    rows.push(RoomFloorLinkRow {
                        source_room: node.id,
                        target_room: link.target_room,
                        target_floor: link.target_floor,
                        x,
                        z,
                        direction: RoomFloorLinkDirection::Above,
                    });
                }
                if let Some(link) = sector.floor_below {
                    rows.push(RoomFloorLinkRow {
                        source_room: node.id,
                        target_room: link.target_room,
                        target_floor: link.target_floor,
                        x,
                        z,
                        direction: RoomFloorLinkDirection::Below,
                    });
                }
            }
        }
    }
    rows.sort_by_key(|row| {
        (
            row.source_room.raw(),
            row.target_room.map(NodeId::raw).unwrap_or(u64::MAX),
            row.x,
            row.z,
            match row.direction {
                RoomFloorLinkDirection::Above => 0u8,
                RoomFloorLinkDirection::Below => 1u8,
            },
        )
    });
    rows
}

pub(crate) fn draw_room_floor_link_row(
    ui: &mut egui::Ui,
    scene: &psxed_project::Scene,
    row: RoomFloorLinkRow,
) {
    let source = scene_node_label(scene, row.source_room);
    let target = row
        .target_room
        .map(|room| scene_node_label(scene, room))
        .unwrap_or_else(|| "missing room".to_string());
    ui.horizontal(|ui| {
        ui.add_space(16.0);
        ui.label(icons::text(icons::LAYERS, 11.0).color(STUDIO_TEXT_WEAK));
        ui.label(format!(
            "{} ({},{}) {} -> {} floor {}",
            compact_middle(&source, 24),
            row.x,
            row.z,
            row.direction.label(),
            compact_middle(&target, 24),
            row.target_floor
        ));
    });
}

pub(crate) fn room_floor_link_matches_filter(
    scene: &psxed_project::Scene,
    row: RoomFloorLinkRow,
    filter: &str,
) -> bool {
    if filter.is_empty() {
        return true;
    }
    let source = scene_node_label(scene, row.source_room).to_ascii_lowercase();
    let target = row
        .target_room
        .map(|room| scene_node_label(scene, room).to_ascii_lowercase())
        .unwrap_or_else(|| "missing room".to_string());
    source.contains(filter) || target.contains(filter) || row.direction.label().contains(filter)
}

pub(crate) fn draw_room_connection_row(
    ui: &mut egui::Ui,
    scene: &psxed_project::Scene,
    connection: &RoomConnection,
    selected_node: NodeId,
    selected_nodes: &HashSet<NodeId>,
    selected: &mut Option<NodeId>,
    repair: &mut Option<NodeId>,
) {
    let row_height = 24.0;
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), row_height), Sense::click());
    let painter = ui.painter_at(rect);
    let connection_selected = connection.contains_portal(selected_node)
        || selected_nodes
            .iter()
            .copied()
            .any(|node| connection.contains_portal(node));
    if connection_selected {
        painter.rect_filled(rect.shrink2(Vec2::new(0.0, 1.0)), 3.0, STUDIO_ACCENT_DIM);
    } else if response.hovered() {
        painter.rect_filled(
            rect.shrink2(Vec2::new(0.0, 1.0)),
            3.0,
            Color32::from_rgba_unmultiplied(42, 58, 70, 120),
        );
    }

    let status_color = room_connection_status_color(connection.status);
    let icon_pos = Pos2::new(rect.left() + 13.0, rect.center().y);
    painter.text(
        icon_pos,
        Align2::CENTER_CENTER,
        icons::WAYPOINT.to_string(),
        icons::font(14.0),
        status_color,
    );

    let label = room_connection_label(scene, connection);
    let status = connection.status.label();
    let label_left = rect.left() + 28.0;
    let label_right = (rect.right() - 118.0).max(label_left + 48.0);
    painter
        .with_clip_rect(Rect::from_min_max(
            Pos2::new(label_left, rect.top()),
            Pos2::new(label_right, rect.bottom()),
        ))
        .text(
            Pos2::new(label_left, rect.center().y),
            Align2::LEFT_CENTER,
            compact_middle(&label, 32),
            FontId::proportional(12.5),
            if connection_selected {
                Color32::WHITE
            } else {
                STUDIO_TEXT
            },
        );
    painter.text(
        Pos2::new(rect.right() - 112.0, rect.center().y),
        Align2::LEFT_CENTER,
        connection.kind.label(),
        FontId::proportional(10.5),
        STUDIO_TEXT_WEAK,
    );
    painter.text(
        Pos2::new(rect.right() - 40.0, rect.center().y),
        Align2::RIGHT_CENTER,
        status,
        FontId::proportional(10.5),
        status_color,
    );

    let response = response.on_hover_text(room_connection_tooltip(scene, connection));
    if response.clicked() {
        *selected = Some(connection.primary_portal());
    }
    response.context_menu(|ui| {
        if ui
            .button(icons::label(icons::FOCUS, "Select source"))
            .clicked()
        {
            *selected = Some(connection.a.portal);
            ui.close_menu();
        }
        if let Some(pair) = &connection.b {
            if ui
                .button(icons::label(icons::FOCUS, "Select reciprocal"))
                .clicked()
            {
                *selected = Some(pair.portal);
                ui.close_menu();
            }
        }
        if connection.status == RoomConnectionStatus::Unpaired
            && ui
                .button(icons::label(icons::PLUS, "Create reciprocal"))
                .clicked()
        {
            *repair = Some(connection.a.portal);
            ui.close_menu();
        }
    });
}

pub(crate) fn room_connection_matches_filter(
    scene: &psxed_project::Scene,
    connection: &RoomConnection,
    filter: &str,
) -> bool {
    filter.is_empty()
        || room_connection_label(scene, connection)
            .to_ascii_lowercase()
            .contains(filter)
        || connection
            .status
            .label()
            .to_ascii_lowercase()
            .contains(filter)
        || connection
            .kind
            .label()
            .to_ascii_lowercase()
            .contains(filter)
}

pub(crate) fn room_connection_label(
    scene: &psxed_project::Scene,
    connection: &RoomConnection,
) -> String {
    let source = room_display_name(scene, connection.a.room);
    let target = connection
        .a
        .target_room
        .map(|room| room_display_name(scene, room))
        .unwrap_or_else(|| "(none)".to_string());
    let arrow = if connection.b.is_some() { "<->" } else { "->" };
    format!("{source} {arrow} {target}")
}

pub(crate) fn room_connection_tooltip(
    scene: &psxed_project::Scene,
    connection: &RoomConnection,
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        &mut out,
        "{}\n{}",
        room_connection_label(scene, connection),
        connection.status.label()
    );
    let _ = writeln!(
        &mut out,
        "Source: {}",
        portal_display_name(scene, connection.a.portal)
    );
    if let Some(pair) = &connection.b {
        let _ = writeln!(
            &mut out,
            "Reciprocal: {}",
            portal_display_name(scene, pair.portal)
        );
    }
    if !connection.alternatives.is_empty() {
        let _ = writeln!(
            &mut out,
            "Extra candidates: {}",
            connection.alternatives.len()
        );
    }
    out
}

pub(crate) const TREE_DND_INSERT_BAND_HEIGHT: f32 = 6.0;
pub(crate) const TREE_DND_AUTOSCROLL_EDGE: f32 = 38.0;
pub(crate) const TREE_DND_AUTOSCROLL_MARGIN: f32 = 18.0;
pub(crate) const TREE_DND_AUTOSCROLL_MAX_DELTA: f32 = 18.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TreeRowDropZone {
    Before,
    Inside,
}

pub(crate) fn tree_row_drop_zone(
    rect: Rect,
    pointer: Option<Pos2>,
    allow_before: bool,
) -> TreeRowDropZone {
    if allow_before
        && pointer.is_some_and(|pos| {
            rect.contains(pos) && pos.y <= rect.top() + TREE_DND_INSERT_BAND_HEIGHT
        })
    {
        TreeRowDropZone::Before
    } else {
        TreeRowDropZone::Inside
    }
}

pub(crate) fn tree_drag_autoscroll_delta(viewport: Rect, pointer: Pos2) -> f32 {
    let active = Rect::from_min_max(
        Pos2::new(viewport.left(), viewport.top() - TREE_DND_AUTOSCROLL_MARGIN),
        Pos2::new(
            viewport.right(),
            viewport.bottom() + TREE_DND_AUTOSCROLL_MARGIN,
        ),
    );
    if !active.contains(pointer) {
        return 0.0;
    }

    let edge = TREE_DND_AUTOSCROLL_EDGE
        .min(viewport.height() * 0.5)
        .max(1.0);
    if pointer.y < viewport.top() + edge {
        let strength = ((viewport.top() + edge - pointer.y) / edge).clamp(0.0, 1.0);
        TREE_DND_AUTOSCROLL_MAX_DELTA * strength
    } else if pointer.y > viewport.bottom() - edge {
        let strength = ((pointer.y - (viewport.bottom() - edge)) / edge).clamp(0.0, 1.0);
        -TREE_DND_AUTOSCROLL_MAX_DELTA * strength
    } else {
        0.0
    }
}

pub(crate) fn autoscroll_tree_drag<Payload>(ui: &mut egui::Ui)
where
    Payload: 'static + Send + Sync,
{
    if !egui::DragAndDrop::has_payload_of_type::<Payload>(ui.ctx()) {
        return;
    }
    let Some(pointer) = ui.ctx().input(|input| input.pointer.interact_pos()) else {
        return;
    };
    let delta = tree_drag_autoscroll_delta(ui.clip_rect(), pointer);
    if delta.abs() > f32::EPSILON {
        ui.scroll_with_delta(Vec2::new(0.0, delta));
        ui.ctx().request_repaint();
    }
}

pub(crate) fn move_scene_nodes_as_group(
    scene: &mut Scene,
    sources: &[NodeId],
    target_parent: NodeId,
    position: usize,
) -> usize {
    if scene.node(target_parent).is_none() {
        return 0;
    }

    let mut seen = HashSet::new();
    let sources: Vec<NodeId> = sources
        .iter()
        .copied()
        .filter(|id| *id != scene.root && scene.node(*id).is_some() && seen.insert(*id))
        .collect();
    if sources.is_empty() {
        return 0;
    }

    let source_set: HashSet<NodeId> = sources.iter().copied().collect();
    if source_set.contains(&target_parent) {
        return 0;
    }

    let adjusted_position = scene
        .node(target_parent)
        .map(|parent| {
            let removed_before = parent
                .children
                .iter()
                .take(position)
                .filter(|child| source_set.contains(child))
                .count();
            position.saturating_sub(removed_before)
        })
        .unwrap_or(position);

    let mut parent_ids = Vec::new();
    for source in &sources {
        if let Some(parent) = scene.node(*source).and_then(|node| node.parent) {
            if !parent_ids.contains(&parent) {
                parent_ids.push(parent);
            }
        }
    }
    if !parent_ids.contains(&target_parent) {
        parent_ids.push(target_parent);
    }

    for parent in parent_ids {
        if let Some(node) = scene.node_mut(parent) {
            node.children.retain(|child| !source_set.contains(child));
        }
    }

    let moved = if let Some(parent) = scene.node_mut(target_parent) {
        let insert_at = adjusted_position.min(parent.children.len());
        for (offset, source) in sources.iter().enumerate() {
            parent.children.insert(insert_at + offset, *source);
        }
        sources.len()
    } else {
        0
    };

    for source in sources.iter().take(moved) {
        if let Some(node) = scene.node_mut(*source) {
            node.parent = Some(target_parent);
        }
    }

    moved
}

pub(crate) fn draw_ui_node_row(
    ui: &mut egui::Ui,
    row: &UiNodeRow,
    scene_id: UiSceneId,
    selected: bool,
    directly_hidden: bool,
    effectively_hidden: bool,
    can_paste: bool,
    actions: &mut Vec<UiTreeAction>,
) {
    let row_height = 24.0;

    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), row_height),
        Sense::click_and_drag(),
    );
    let painter = ui.painter_at(rect);
    let hovered = response.hovered();

    paint_list_row(&painter, rect, selected, hovered);

    let indent = row.depth as f32 * 16.0;
    let content_left = rect.left() + 4.0 + indent;
    if row.depth > 0 {
        let line_x = rect.left() + 10.0 + (row.depth.saturating_sub(1) as f32 * 16.0);
        painter.line_segment(
            [
                Pos2::new(line_x, rect.top()),
                Pos2::new(line_x, rect.bottom()),
            ],
            Stroke::new(1.0, STUDIO_TREE_GUIDE),
        );
    }

    let icon_rect = Rect::from_min_size(
        Pos2::new(content_left + 14.0, rect.center().y - 8.0),
        Vec2::splat(16.0),
    );
    painter.text(
        icon_rect.center(),
        Align2::CENTER_CENTER,
        ui_node_kind_icon(row.kind).to_string(),
        icons::font(15.0),
        if effectively_hidden {
            Color32::from_rgb(82, 94, 106)
        } else if selected {
            STUDIO_ACCENT
        } else {
            Color32::from_rgb(160, 174, 188)
        },
    );

    let text_color = if effectively_hidden {
        Color32::from_rgb(96, 110, 124)
    } else if selected {
        Color32::WHITE
    } else {
        STUDIO_TEXT
    };
    let text_left = icon_rect.right() + 7.0;
    let label = compact_middle(&row.name, dock_label_limit(row.depth));
    let (name_clip_right, detail_rect) = tree_row_text_regions(
        rect,
        text_left,
        row.id != UiNodeId::ROOT,
        row.child_count > 0,
    );
    painter
        .with_clip_rect(Rect::from_min_max(
            Pos2::new(text_left, rect.top()),
            Pos2::new(name_clip_right, rect.bottom()),
        ))
        .text(
            Pos2::new(text_left, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(13.0),
            text_color,
        );

    if let Some(detail_rect) = detail_rect {
        let detail = row
            .tag
            .as_ref()
            .map(|tag| format!("{}  #{}", row.kind, compact_middle(tag, 18)))
            .unwrap_or_else(|| row.kind.to_string());
        painter.with_clip_rect(detail_rect).text(
            Pos2::new(detail_rect.left(), rect.center().y),
            Align2::LEFT_CENTER,
            compact_middle(&detail, 18),
            FontId::proportional(11.0),
            if effectively_hidden {
                Color32::from_rgb(84, 96, 108)
            } else if selected {
                STUDIO_TEXT
            } else {
                STUDIO_TEXT_WEAK
            },
        );
    }

    if row.child_count > 0 {
        let pill = Rect::from_min_size(
            Pos2::new(rect.right() - 50.0, rect.center().y - 8.0),
            Vec2::new(24.0, 16.0),
        );
        painter.rect_filled(pill, 8.0, Color32::from_rgba_unmultiplied(9, 14, 18, 138));
        painter.text(
            pill.center(),
            Align2::CENTER_CENTER,
            row.child_count.to_string(),
            FontId::monospace(10.0),
            STUDIO_TEXT_WEAK,
        );
    }

    let eye_rect = Rect::from_min_size(
        Pos2::new(rect.right() - 22.0, rect.center().y - 6.0),
        Vec2::new(14.0, 12.0),
    );
    let eye_response = ui
        .interact(
            eye_rect.expand2(Vec2::new(5.0, 4.0)),
            ui.id().with(("ui_tree_visibility", scene_id, row.id)),
            Sense::click(),
        )
        .on_hover_text(if directly_hidden {
            "Show UI node"
        } else {
            "Hide UI node"
        });
    painter.text(
        eye_rect.center(),
        Align2::CENTER_CENTER,
        if effectively_hidden {
            icons::EYE_OFF
        } else {
            icons::EYE
        }
        .to_string(),
        icons::font(12.0),
        if eye_response.hovered() {
            Color32::WHITE
        } else if effectively_hidden {
            Color32::from_rgb(82, 92, 102)
        } else if selected || hovered {
            Color32::from_rgb(184, 205, 218)
        } else {
            Color32::from_rgb(88, 102, 116)
        },
    );

    if row.id != UiNodeId::ROOT && response.dragged() {
        response.dnd_set_drag_payload::<UiNodeId>(row.id);
        let pointer_pos = ui
            .ctx()
            .input(|i| i.pointer.interact_pos())
            .unwrap_or_else(|| rect.center());
        ui.painter().text(
            pointer_pos + Vec2::new(12.0, 0.0),
            Align2::LEFT_CENTER,
            ui_label_for_drag(row),
            FontId::proportional(12.0),
            STUDIO_ACCENT,
        );
    }

    let pointer_pos = ui.ctx().input(|input| input.pointer.interact_pos());
    let drop_zone = tree_row_drop_zone(rect, pointer_pos, row.id != UiNodeId::ROOT);

    if let Some(payload) = response.dnd_hover_payload::<UiNodeId>() {
        if *payload != row.id {
            match drop_zone {
                TreeRowDropZone::Before => {
                    ui.painter().line_segment(
                        [
                            Pos2::new(rect.left() + 4.0, rect.top() + 1.0),
                            Pos2::new(rect.right() - 4.0, rect.top() + 1.0),
                        ],
                        Stroke::new(EDITOR_OUTLINE_STROKE_WIDTH, EDITOR_OUTLINE_ACCENT),
                    );
                }
                TreeRowDropZone::Inside => {
                    ui.painter().rect_stroke(
                        rect.shrink2(Vec2::new(2.0, 1.0)),
                        3.0,
                        Stroke::new(EDITOR_OUTLINE_STROKE_WIDTH, EDITOR_OUTLINE_ACCENT),
                        StrokeKind::Inside,
                    );
                }
            }
        }
    }
    if let Some(payload) = response.dnd_release_payload::<UiNodeId>() {
        if *payload != row.id {
            match drop_zone {
                TreeRowDropZone::Before => actions.push(UiTreeAction::Reparent {
                    source: *payload,
                    target_parent: row.parent.unwrap_or(UiNodeId::ROOT),
                    position: row.sibling_index,
                }),
                TreeRowDropZone::Inside => actions.push(UiTreeAction::Reparent {
                    source: *payload,
                    target_parent: row.id,
                    position: row.child_count,
                }),
            }
        }
    }

    if eye_response.clicked() {
        actions.push(UiTreeAction::ToggleVisibility(row.id));
    }
    let icon_clicked = eye_response.clicked();
    if response.clicked() && !icon_clicked {
        actions.push(UiTreeAction::Select(row.id));
    }

    response.context_menu(|ui| {
        if row.id != UiNodeId::ROOT && ui.button(icons::label(icons::COPY, "Copy")).clicked() {
            actions.push(UiTreeAction::Copy(row.id));
            ui.close_menu();
        }
        if ui
            .add_enabled(can_paste, egui::Button::new("Paste Child"))
            .clicked()
        {
            actions.push(UiTreeAction::PasteInto(row.id));
            ui.close_menu();
        }
        ui.separator();
        ui.menu_button(icons::label(icons::PLUS, "Add Child"), |ui| {
            for (label, kind) in default_addable_ui_kinds() {
                if ui.button(label).clicked() {
                    actions.push(UiTreeAction::AddChild {
                        parent: row.id,
                        kind,
                        name: label,
                    });
                    ui.close_menu();
                }
            }
        });
        if row.id != UiNodeId::ROOT {
            ui.separator();
            if ui.button(icons::label(icons::TRASH, "Delete")).clicked() {
                actions.push(UiTreeAction::Delete(row.id));
                ui.close_menu();
            }
        }
    });
}

pub(crate) fn room_connection_status_color(status: RoomConnectionStatus) -> Color32 {
    match status {
        RoomConnectionStatus::Paired => Color32::from_rgb(110, 218, 148),
        RoomConnectionStatus::Unassigned => STUDIO_TEXT_WEAK,
        RoomConnectionStatus::Unpaired => Color32::from_rgb(238, 176, 78),
        RoomConnectionStatus::MissingTarget | RoomConnectionStatus::Ambiguous => {
            Color32::from_rgb(238, 96, 112)
        }
    }
}

pub(crate) fn draw_portal_connection_inspector(
    ui: &mut egui::Ui,
    scene: &psxed_project::Scene,
    connection: &RoomConnection,
) -> Option<NodeId> {
    let mut nav = None;
    egui::CollapsingHeader::new(icons::label(icons::WAYPOINT, "Connection"))
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Route");
                ui.label(room_connection_label(scene, connection));
            });
            ui.horizontal(|ui| {
                ui.label("Status");
                ui.colored_label(
                    room_connection_status_color(connection.status),
                    connection.status.label(),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Kind");
                ui.label(connection.kind.label());
            });
            ui.horizontal(|ui| {
                if ui.button(icons::label(icons::FOCUS, "Source")).clicked() {
                    nav = Some(connection.a.portal);
                }
                if let Some(pair) = &connection.b {
                    if ui
                        .button(icons::label(icons::FOCUS, "Reciprocal"))
                        .clicked()
                    {
                        nav = Some(pair.portal);
                    }
                }
            });
            if connection.status == RoomConnectionStatus::Unpaired {
                ui.weak("This portal has no reciprocal endpoint in the target room.");
            } else if connection.status == RoomConnectionStatus::Ambiguous {
                ui.weak(format!(
                    "{} extra reciprocal candidate(s) found.",
                    connection.alternatives.len()
                ));
            }
        });
    nav
}

pub(crate) fn room_display_name(scene: &psxed_project::Scene, room: NodeId) -> String {
    scene
        .node(room)
        .map(|node| node.name.clone())
        .unwrap_or_else(|| format!("Room #{}", room.raw()))
}

pub(crate) fn portal_display_name(scene: &psxed_project::Scene, portal: NodeId) -> String {
    scene
        .node(portal)
        .map(|node| format!("{} #{}", node.name, node.id.raw()))
        .unwrap_or_else(|| format!("Portal #{}", portal.raw()))
}

pub(crate) fn inverted_portal_geometry(
    geometry: &psxed_project::PortalGeometry,
) -> psxed_project::PortalGeometry {
    let mut out = geometry.clone();
    out.normal = [
        out.normal[0].saturating_neg(),
        out.normal[1].saturating_neg(),
        out.normal[2].saturating_neg(),
    ];
    out
}

pub(crate) fn draw_scene_node_row(
    ui: &mut egui::Ui,
    row: &NodeRow,
    selected: bool,
    collapsed: bool,
    directly_hidden: bool,
    effectively_hidden: bool,
    renaming: &mut Option<(NodeId, String)>,
    pending_focus: &mut bool,
    actions: &mut Vec<TreeAction>,
) {
    let row_height = 24.0;

    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), row_height),
        Sense::click_and_drag(),
    );
    let painter = ui.painter_at(rect);
    let hovered = response.hovered();
    let display_kind = scene_tree_kind_label(row.kind);

    paint_list_row(&painter, rect, selected, hovered);

    let indent = row.depth as f32 * 16.0;
    let content_left = rect.left() + 4.0 + indent;
    if row.depth > 0 {
        let line_x = rect.left() + 10.0 + (row.depth.saturating_sub(1) as f32 * 16.0);
        painter.line_segment(
            [
                Pos2::new(line_x, rect.top()),
                Pos2::new(line_x, rect.bottom()),
            ],
            Stroke::new(1.0, STUDIO_TREE_GUIDE),
        );
    }

    let chevron_rect = Rect::from_min_size(
        Pos2::new(content_left, rect.center().y - 5.0),
        Vec2::splat(10.0),
    );
    let chevron_response = if row.child_count > 0 {
        ui.interact(
            chevron_rect.expand(4.0),
            ui.id().with(("scene_tree_expand", row.id)),
            Sense::click(),
        )
        .on_hover_text(if collapsed { "Expand" } else { "Collapse" })
    } else {
        response.clone()
    };
    if row.child_count > 0 {
        painter.text(
            chevron_rect.center(),
            Align2::CENTER_CENTER,
            if collapsed {
                icons::CHEVRON_RIGHT
            } else {
                icons::CHEVRON_DOWN
            }
            .to_string(),
            icons::font(12.0),
            if chevron_response.hovered() {
                Color32::WHITE
            } else {
                Color32::from_rgb(160, 174, 188)
            },
        );
    }

    let icon_rect = Rect::from_min_size(
        Pos2::new(content_left + 14.0, rect.center().y - 8.0),
        Vec2::splat(16.0),
    );
    painter.text(
        icon_rect.center(),
        Align2::CENTER_CENTER,
        node_lucide_icon(display_kind, row.id == NodeId::ROOT).to_string(),
        icons::font(15.0),
        node_lucide_color(display_kind, row.id == NodeId::ROOT, selected),
    );

    let text_color = if effectively_hidden {
        Color32::from_rgb(96, 110, 124)
    } else if selected {
        Color32::WHITE
    } else {
        STUDIO_TEXT
    };
    let in_rename = matches!(renaming, Some((id, _)) if *id == row.id);
    let label = row.name.clone();
    let display_label = compact_middle(&label, dock_label_limit(row.depth));
    let response = if !in_rename && display_label != label {
        response.on_hover_text(label.clone())
    } else {
        response
    };
    let text_left = icon_rect.right() + 7.0;
    let text_pos = Pos2::new(text_left, rect.center().y);
    let (name_clip_right, detail_rect) = tree_row_text_regions(
        rect,
        text_left,
        row.id != NodeId::ROOT,
        row.child_count > 0 && row.id != NodeId::ROOT,
    );

    if in_rename {
        let edit_rect = Rect::from_min_size(
            Pos2::new(text_left, rect.center().y - 10.0),
            Vec2::new(rect.right() - text_left - 56.0, 20.0),
        );
        if let Some((_, buffer)) = renaming.as_mut() {
            let edit_response = ui.put(
                edit_rect,
                egui::TextEdit::singleline(buffer)
                    .desired_width(edit_rect.width())
                    .margin(egui::Vec2::new(2.0, 1.0)),
            );
            if *pending_focus {
                edit_response.request_focus();
                *pending_focus = false;
            }
            let lost_focus = edit_response.lost_focus();
            let pressed_enter = lost_focus && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let pressed_esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if pressed_esc {
                actions.push(TreeAction::CancelRename);
            } else if pressed_enter || lost_focus {
                actions.push(TreeAction::CommitRename(row.id, buffer.clone()));
            }
        }
    } else {
        painter
            .with_clip_rect(Rect::from_min_max(
                Pos2::new(text_left, rect.top()),
                Pos2::new(name_clip_right, rect.bottom()),
            ))
            .text(
                text_pos,
                Align2::LEFT_CENTER,
                display_label,
                FontId::proportional(13.0),
                text_color,
            );
    }

    if !in_rename {
        if let Some(detail_rect) = detail_rect {
            painter.with_clip_rect(detail_rect).text(
                Pos2::new(detail_rect.left(), rect.center().y),
                Align2::LEFT_CENTER,
                compact_middle(display_kind, 18),
                FontId::proportional(11.0),
                if effectively_hidden {
                    Color32::from_rgb(84, 96, 108)
                } else if selected {
                    STUDIO_TEXT
                } else {
                    STUDIO_TEXT_WEAK
                },
            );
        }
    }

    if row.child_count > 0 && row.id != NodeId::ROOT {
        let pill = Rect::from_min_size(
            Pos2::new(rect.right() - 50.0, rect.center().y - 8.0),
            Vec2::new(24.0, 16.0),
        );
        painter.rect_filled(pill, 8.0, Color32::from_rgba_unmultiplied(9, 14, 18, 138));
        painter.text(
            pill.center(),
            Align2::CENTER_CENTER,
            row.child_count.to_string(),
            FontId::monospace(10.0),
            STUDIO_TEXT_WEAK,
        );
    }

    let eye_rect = Rect::from_min_size(
        Pos2::new(rect.right() - 22.0, rect.center().y - 6.0),
        Vec2::new(14.0, 12.0),
    );
    let eye_response = ui
        .interact(
            eye_rect.expand2(Vec2::new(5.0, 4.0)),
            ui.id().with(("scene_tree_visibility", row.id)),
            Sense::click(),
        )
        .on_hover_text(if directly_hidden {
            "Show node"
        } else {
            "Hide node"
        });
    painter.text(
        eye_rect.center(),
        Align2::CENTER_CENTER,
        if effectively_hidden {
            icons::EYE_OFF
        } else {
            icons::EYE
        }
        .to_string(),
        icons::font(12.0),
        if eye_response.hovered() {
            Color32::WHITE
        } else if effectively_hidden {
            Color32::from_rgb(82, 92, 102)
        } else if selected || hovered {
            Color32::from_rgb(184, 205, 218)
        } else {
            Color32::from_rgb(88, 102, 116)
        },
    );

    if in_rename {
        return;
    }

    if chevron_response.clicked() && row.child_count > 0 {
        actions.push(TreeAction::ToggleExpanded(row.id));
    }
    if eye_response.clicked() {
        actions.push(TreeAction::ToggleVisibility(row.id));
    }
    let icon_clicked =
        (row.child_count > 0 && chevron_response.clicked()) || eye_response.clicked();

    // Drag source: only descendants of the World root can be dragged.
    if row.id != NodeId::ROOT && response.dragged() {
        response.dnd_set_drag_payload::<NodeId>(row.id);
        let label_text = label_for_drag(row);
        let pointer_pos = ui
            .ctx()
            .input(|i| i.pointer.interact_pos())
            .unwrap_or_else(|| rect.center());
        ui.painter().text(
            pointer_pos + Vec2::new(12.0, 0.0),
            Align2::LEFT_CENTER,
            label_text,
            FontId::proportional(12.0),
            STUDIO_ACCENT,
        );
    }

    // Drop on row body → reparent as last child. Highlight while
    // hovered so the user knows where the drop will land.
    let pointer_pos = ui.ctx().input(|input| input.pointer.interact_pos());
    let drop_zone = tree_row_drop_zone(rect, pointer_pos, row.id != NodeId::ROOT);

    if let Some(payload) = response.dnd_hover_payload::<NodeId>() {
        if *payload != row.id {
            match drop_zone {
                TreeRowDropZone::Before => {
                    ui.painter().line_segment(
                        [
                            Pos2::new(rect.left() + 4.0, rect.top() + 1.0),
                            Pos2::new(rect.right() - 4.0, rect.top() + 1.0),
                        ],
                        Stroke::new(EDITOR_OUTLINE_STROKE_WIDTH, EDITOR_OUTLINE_ACCENT),
                    );
                }
                TreeRowDropZone::Inside => {
                    ui.painter().rect_stroke(
                        rect.shrink2(Vec2::new(2.0, 1.0)),
                        3.0,
                        Stroke::new(EDITOR_OUTLINE_STROKE_WIDTH, EDITOR_OUTLINE_ACCENT),
                        StrokeKind::Inside,
                    );
                }
            }
        }
    }
    if let Some(payload) = response.dnd_release_payload::<NodeId>() {
        if *payload != row.id {
            match drop_zone {
                TreeRowDropZone::Before => actions.push(TreeAction::Reparent {
                    source: *payload,
                    target_parent: row.parent.unwrap_or(NodeId::ROOT),
                    position: row.sibling_index,
                }),
                TreeRowDropZone::Inside => actions.push(TreeAction::Reparent {
                    source: *payload,
                    target_parent: row.id,
                    position: row.child_count,
                }),
            }
        }
    }

    if response.clicked() && !icon_clicked {
        let modifiers = ui.input(|input| input.modifiers);
        actions.push(TreeAction::Select {
            id: row.id,
            modifiers,
        });
    }
    if response.double_clicked() && !icon_clicked && row.id != NodeId::ROOT {
        actions.push(TreeAction::BeginRename(row.id));
    }

    if row.id != NodeId::ROOT {
        response.context_menu(|ui| {
            ui.menu_button(icons::label(icons::PLUS, "Add Child"), |ui| {
                for (label, kind) in scene_graph_addable_kinds_for_host_label(row.kind) {
                    if ui.button(label).clicked() {
                        actions.push(TreeAction::AddChild {
                            parent: row.id,
                            kind,
                            name: label,
                        });
                        ui.close_menu();
                    }
                }
            });
            if ui.button(icons::label(icons::PALETTE, "Rename")).clicked() {
                actions.push(TreeAction::BeginRename(row.id));
                ui.close_menu();
            }
            if ui.button(icons::label(icons::COPY, "Duplicate")).clicked() {
                actions.push(TreeAction::Duplicate(row.id));
                ui.close_menu();
            }
            ui.separator();
            if ui.button(icons::label(icons::TRASH, "Delete")).clicked() {
                actions.push(TreeAction::Delete(row.id));
                ui.close_menu();
            }
        });
    } else {
        // The root is the scene graph; add structural nodes here and use the
        // toolbar Add menu for placed runtime objects.
        response.context_menu(|ui| {
            ui.menu_button(icons::label(icons::PLUS, "Add Child"), |ui| {
                for (label, kind) in scene_graph_addable_kinds() {
                    if ui.button(label).clicked() {
                        actions.push(TreeAction::AddChild {
                            parent: row.id,
                            kind,
                            name: label,
                        });
                        ui.close_menu();
                    }
                }
            });
        });
    }
}

pub(crate) fn reserve_remaining_panel_space(ui: &mut egui::Ui) {
    let remaining = ui.available_size();
    if remaining.x > 0.0 || remaining.y > 0.0 {
        ui.allocate_space(remaining);
    }
}

pub(crate) fn fixed_panel_content<R>(
    ui: &mut egui::Ui,
    id_salt: &'static str,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let size = ui.available_size_before_wrap();
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(id_salt)
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.expand_to_include_rect(rect);
    child.set_clip_rect(rect);
    add_contents(&mut child)
}

pub(crate) fn range_between<T: Copy + Eq>(order: &[T], a: T, b: T) -> Option<Vec<T>> {
    let ai = order.iter().position(|id| *id == a)?;
    let bi = order.iter().position(|id| *id == b)?;
    let (start, end) = if ai <= bi { (ai, bi) } else { (bi, ai) };
    Some(order[start..=end].to_vec())
}

pub(crate) fn scene_node_hidden(
    scene: &psxed_project::Scene,
    hidden_scene_nodes: &HashSet<NodeId>,
    id: NodeId,
) -> bool {
    let mut current = Some(id);
    while let Some(node_id) = current {
        if hidden_scene_nodes.contains(&node_id) {
            return true;
        }
        current = scene.node(node_id).and_then(|node| node.parent);
    }
    false
}

pub(crate) fn ui_node_hidden(
    scene: &psxed_project::UiScene,
    hidden_ui_nodes: &HashSet<(UiSceneId, UiNodeId)>,
    id: UiNodeId,
) -> bool {
    let mut current = Some(id);
    while let Some(node_id) = current {
        if hidden_ui_nodes.contains(&(scene.id, node_id)) {
            return true;
        }
        current = scene.node(node_id).and_then(|node| node.parent);
    }
    false
}

pub(crate) fn scene_tree_row_matches_filter(row: &NodeRow, filter: &str) -> bool {
    let display_kind = scene_tree_kind_label(row.kind);
    filter.is_empty()
        || row.name.to_ascii_lowercase().contains(filter)
        || row.kind.to_ascii_lowercase().contains(filter)
        || display_kind.to_ascii_lowercase().contains(filter)
}

pub(crate) fn scene_tree_display_rows<'a>(
    rows: &'a [NodeRow],
    filter: &str,
    collapsed_scene_nodes: &HashSet<NodeId>,
) -> Vec<&'a NodeRow> {
    if !filter.is_empty() {
        return rows
            .iter()
            .filter(|row| scene_tree_row_matches_filter(row, filter))
            .collect();
    }

    let mut collapsed_depth = None;
    let mut display_rows = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(depth) = collapsed_depth {
            if row.depth > depth {
                continue;
            }
            collapsed_depth = None;
        }

        display_rows.push(row);
        if row.child_count > 0 && collapsed_scene_nodes.contains(&row.id) {
            collapsed_depth = Some(row.depth);
        }
    }
    display_rows
}

pub(crate) fn first_in_order<T: Copy + Eq + std::hash::Hash>(
    order: &[T],
    selected: &HashSet<T>,
) -> Option<T> {
    order.iter().copied().find(|id| selected.contains(id))
}

/// Shared Shift-range / toggle / replace multi-selection over an ordered list.
///
/// Mutates `set` and `anchor` in place and returns the new primary selection:
/// the clicked `id` if it survived, otherwise the first still-selected item in
/// `order`. Used by both scene-node and resource selection so the branching
/// logic lives once. `shift_anchor_fallback` is the anchor to range from when
/// none is set (the current primary for nodes, the clicked id for resources).
pub(crate) fn apply_range_modifiers<T: Copy + Eq + std::hash::Hash>(
    set: &mut HashSet<T>,
    anchor: &mut Option<T>,
    id: T,
    shift: bool,
    toggle: bool,
    order: &[T],
    shift_anchor_fallback: T,
) -> Option<T> {
    if shift {
        let from = anchor.unwrap_or(shift_anchor_fallback);
        let range = range_between(order, from, id).unwrap_or_else(|| vec![id]);
        if !toggle {
            set.clear();
        }
        set.extend(range);
        anchor.get_or_insert(from);
    } else if toggle {
        if !set.remove(&id) {
            set.insert(id);
        }
        *anchor = Some(id);
    } else {
        set.clear();
        set.insert(id);
        *anchor = Some(id);
    }
    set.contains(&id)
        .then_some(id)
        .or_else(|| first_in_order(order, set))
}

pub(crate) fn constrain_resizable_dock_content(ui: &mut egui::Ui, width: f32) {
    ui.set_width(width);
    ui.set_max_width(width);
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
    ui.spacing_mut().text_edit_width = (width - 72.0).clamp(24.0, 280.0);
}

pub(crate) fn max_resizable_side_dock_width(
    ctx: &egui::Context,
    reserve_opposite_dock: bool,
) -> f32 {
    let opposite = if reserve_opposite_dock {
        RESIZABLE_DOCK_MIN_WIDTH
    } else {
        0.0
    };
    (ctx.available_rect().width() - CENTRAL_WORKSPACE_MIN_WIDTH - opposite)
        .max(RESIZABLE_DOCK_MIN_WIDTH)
}

pub(crate) fn max_resizable_bottom_dock_height(ctx: &egui::Context) -> f32 {
    (ctx.available_rect().height() - CENTRAL_WORKSPACE_MIN_HEIGHT).max(CONTENT_BROWSER_MIN_HEIGHT)
}

/// Friendly label for the drag-tooltip preview.
pub(crate) fn label_for_drag(row: &NodeRow) -> String {
    if row.name.is_empty() {
        scene_tree_kind_label(row.kind).to_string()
    } else {
        row.name.clone()
    }
}

pub(crate) fn ui_label_for_drag(row: &UiNodeRow) -> String {
    let base = if row.name.is_empty() {
        row.kind.to_string()
    } else {
        row.name.clone()
    };
    if let Some(tag) = row.tag.as_ref().filter(|tag| !tag.is_empty()) {
        format!("{base}  #{tag}")
    } else {
        base
    }
}

pub(crate) fn scene_tree_kind_label(kind: &'static str) -> &'static str {
    kind
}

/// Structural scene-graph entries for global "Add Child" menus.
///
/// Runtime objects are placed through the toolbar Add/Place menu so a click
/// can resolve room context, resources, floor anchoring, and dedupe rules.
pub(crate) fn scene_graph_addable_kinds() -> [(&'static str, NodeKind); 3] {
    [
        (
            "Room",
            NodeKind::Room {
                grid: WorldGrid::empty(3, 3, 1024),
            },
        ),
        ("Entity", NodeKind::Entity),
        ("Folder", NodeKind::Node),
    ]
}

/// Scene-graph entries for a specific row's "Add Child" menu.
///
/// Entity components are real child nodes, so expose them beside the
/// structural nodes when adding directly under an Entity.
pub(crate) fn scene_graph_addable_kinds_for_host_label(
    host_kind: &str,
) -> Vec<(&'static str, NodeKind)> {
    let mut addable = scene_graph_addable_kinds().into_iter().collect::<Vec<_>>();
    if host_kind == NodeKind::Entity.label() {
        addable.extend(component_templates_for_host(&NodeKind::Entity));
    }
    addable
}

/// Default UI node templates for the UI tree "Add" menu.
pub(crate) fn default_addable_ui_kinds() -> Vec<(&'static str, UiNodeKind)> {
    vec![
        (
            "Group",
            UiNodeKind::Group {
                rect: UiRect::new(0, 0, 64, 32),
            },
        ),
        (
            "Rect",
            UiNodeKind::Rect {
                rect: UiRect::new(24, 24, 80, 24),
                color: [32, 36, 48],
                gradient: None,
            },
        ),
        (
            "Label",
            UiNodeKind::Label {
                rect: UiRect::new(24, 24, 96, 12),
                text: "Label".to_string(),
                random_message: false,
                messages: Vec::new(),
                tag: String::new(),
                align: UiTextAlign::Left,
                wrap: false,
                font: UiFontChoice::Basic,
                font_scale: default_ui_font_scale(),
                letter_spacing: default_ui_letter_spacing(),
                color: [220, 226, 240],
                gradient: None,
                effect: UiImageEffect::None,
            },
        ),
        (
            "Image",
            UiNodeKind::Image {
                rect: UiRect::new(24, 24, 64, 64),
                texture: None,
                tint: [128, 128, 128],
                effect: UiImageEffect::None,
            },
        ),
        (
            "Bar",
            UiNodeKind::Bar {
                rect: UiRect::new(24, 42, 96, 8),
                value: UiValueBinding::ConstantQ12(4096),
                max: UiValueBinding::ConstantQ12(4096),
                fill: [72, 136, 96],
                fill_gradient: None,
                background: [30, 26, 28],
                background_gradient: None,
            },
        ),
        (
            "Button",
            UiNodeKind::Button {
                rect: UiRect::new(24, 24, 96, 18),
                label: "Button".to_string(),
                align: UiTextAlign::Center,
                font: UiFontChoice::Basic,
                font_scale: default_ui_font_scale(),
                letter_spacing: default_ui_letter_spacing(),
                color: [52, 60, 80],
                background_gradient: None,
                text_color: [236, 240, 248],
                text_gradient: None,
                transparent: false,
                action: UiAction::Back,
                sfx: UiSfxBindings::default(),
            },
        ),
        (
            "Slider",
            UiNodeKind::Slider {
                rect: UiRect::new(24, 48, 96, 8),
                option: OptionId::default(),
                track: [30, 34, 44],
                track_gradient: None,
                fill: [80, 132, 180],
                fill_gradient: None,
                knob: [210, 218, 232],
                knob_gradient: None,
                sfx: UiSfxBindings::default(),
            },
        ),
        (
            "Music",
            UiNodeKind::Music {
                wav_path: String::new(),
                volume: 25,
                volume_option: None,
                playback_speed_q12: psxed_project::UI_MUSIC_PLAYBACK_SPEED_UNITY_Q12,
                loop_track: false,
            },
        ),
    ]
}

pub(crate) fn ui_node_kind_icon(kind: &str) -> char {
    match kind {
        "Canvas" => icons::SQUARE,
        "Group" => icons::LAYERS,
        "Rect" => icons::SQUARE,
        "Label" => icons::FILE,
        "Image" => icons::PALETTE,
        "Bar" => icons::BLEND,
        "Button" => icons::POINTER,
        "Slider" => icons::WAYPOINT,
        "Music" => icons::AUDIO_LINES,
        _ => icons::CIRCLE_DOT,
    }
}

pub(crate) fn ui_scene_canvas_size(scene: &psxed_project::UiScene) -> (u16, u16) {
    scene
        .node(scene.root)
        .and_then(|node| match &node.kind {
            UiNodeKind::Canvas { width, height } => Some(((*width).max(1), (*height).max(1))),
            _ => None,
        })
        .unwrap_or((320, 240))
}

pub(crate) fn ui_scene_has_animated_image_effect(
    scene: &psxed_project::UiScene,
    hidden_ui_nodes: &HashSet<(UiSceneId, UiNodeId)>,
) -> bool {
    scene.hierarchy_node_ids().into_iter().any(|id| {
        !ui_node_hidden(scene, hidden_ui_nodes, id)
            && scene.node(id).is_some_and(|node| {
                matches!(
                    &node.kind,
                    UiNodeKind::Image {
                        effect,
                        ..
                    } if *effect != UiImageEffect::None
                )
            })
    })
}

pub(crate) fn starter_room_grid(sector_size: i32, material: Option<ResourceId>) -> WorldGrid {
    let mut grid = WorldGrid::empty(3, 3, sector_size);
    for x in 0..3 {
        for z in 0..3 {
            grid.set_floor(x, z, 0, material);
        }
    }
    grid
}

pub(crate) fn addable_component_templates(
    host_kind: &NodeKind,
    existing: &[&NodeKind],
) -> Vec<(&'static str, NodeKind)> {
    component_templates_for_host(host_kind)
        .into_iter()
        .filter(|(_, candidate)| {
            component_can_be_added(candidate, existing)
                && component_is_valid_for_host(host_kind, candidate)
        })
        .collect()
}

pub(crate) fn component_templates_for_host(host_kind: &NodeKind) -> Vec<(&'static str, NodeKind)> {
    if !matches!(host_kind, NodeKind::Entity) {
        return Vec::new();
    }
    vec![
        (
            "Model Renderer",
            NodeKind::ModelRenderer {
                model: None,
                material: None,
                visual_offset: [0; 3],
                visual_scale_q8: psxed_project::MODEL_SCALE_ONE_Q8,
            },
        ),
        (
            "Animator",
            NodeKind::Animator {
                clip: None,
                action_clips: Vec::new(),
                autoplay: true,
                pose_frame: 0,
            },
        ),
        (
            "Character Controller",
            NodeKind::CharacterController {
                character: None,
                settings: CharacterControllerSettings::default(),
                player: false,
            },
        ),
        (
            "Camera",
            NodeKind::Camera {
                settings: WorldCameraSettings::default(),
            },
        ),
        (
            "Equipment",
            NodeKind::Equipment {
                weapon: None,
                character_socket: "right_hand_grip".to_string(),
                weapon_grip: "grip".to_string(),
            },
        ),
        (
            "Physics Body",
            NodeKind::PhysicsBody {
                settings: PhysicsBodySettings::default(),
            },
        ),
        (
            "Interactable",
            NodeKind::Interactable {
                kind: InteractableKind::default(),
                prompt: "READ ECHO".to_string(),
                radius: 96,
                enabled: true,
            },
        ),
    ]
}

pub(crate) fn component_can_be_added_to_host(
    host_kind: &NodeKind,
    candidate: &NodeKind,
    scene: &psxed_project::Scene,
    host: NodeId,
) -> bool {
    let existing: Vec<&NodeKind> = scene
        .node(host)
        .into_iter()
        .flat_map(|host| host.children.iter())
        .filter_map(|id| scene.node(*id))
        .filter(|child| child.kind.is_component())
        .map(|child| &child.kind)
        .collect();
    component_is_valid_for_host(host_kind, candidate)
        && component_can_be_added(candidate, &existing)
}

pub(crate) const fn component_is_valid_for_host(
    host_kind: &NodeKind,
    component: &NodeKind,
) -> bool {
    if !component.is_component() {
        return false;
    }
    matches!(host_kind, NodeKind::Entity)
}

pub(crate) fn component_can_be_added(candidate: &NodeKind, existing: &[&NodeKind]) -> bool {
    if component_allows_multiple(candidate) {
        return true;
    }
    let Some(candidate_slot) = component_slot(candidate) else {
        return true;
    };
    !existing
        .iter()
        .filter_map(|component| component_slot(component))
        .any(|slot| slot == candidate_slot)
}

pub(crate) const fn component_allows_multiple(kind: &NodeKind) -> bool {
    matches!(kind, NodeKind::Collider { .. })
}

pub(crate) const fn component_slot(kind: &NodeKind) -> Option<&'static str> {
    match kind {
        NodeKind::ModelRenderer { .. } => Some("ModelRenderer"),
        NodeKind::Animator { .. } => Some("Animator"),
        NodeKind::Collider { .. } => Some("Collider"),
        NodeKind::CharacterController { .. } => Some("CharacterController"),
        NodeKind::Camera { .. } => Some("Camera"),
        NodeKind::Equipment { .. } => Some("Equipment"),
        NodeKind::PhysicsBody { .. } => Some("PhysicsBody"),
        NodeKind::Interactable { .. } => Some("Interactable"),
        _ => None,
    }
}
