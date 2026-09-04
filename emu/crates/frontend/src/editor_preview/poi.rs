//! Depth-sorted Point of Interest beacons for the editor preview.
//!
//! POIs used to be painted by egui after the preview texture. That made the
//! complete beacon visible through every BSP surface. Keep the authoring
//! colors and steady rotation, but submit the shell as ordinary world
//! triangles so the preview ordering table gives brushes normal occlusion.

use super::*;

const POI_FACE_VERTICES: usize = 6;
const POI_FACE_TRIANGLES: [[usize; 3]; 4] = [[0, 1, 2], [0, 2, 3], [0, 3, 4], [0, 4, 5]];
const POI_ROTATION_Q12_PER_TICK: u32 = 9;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PoiPreviewStyle {
    face: (u8, u8, u8),
    side: (u8, u8, u8),
}

pub(super) fn walk_point_of_interest_beacons(
    project: &ProjectDocument,
    hidden_scene_nodes: &HashSet<NodeId>,
    selected: NodeId,
    hovered: Option<NodeId>,
    entity_bounds: &[psxed_ui::EntityBounds],
    tick: u32,
    scratch: &mut PreviewScratch,
) {
    let scene = project.active_scene();
    for bounds in entity_bounds
        .iter()
        .filter(|bounds| bounds.kind == psxed_ui::EntityBoundKind::PointOfInterest)
    {
        let Some(host) = scene.node(bounds.node) else {
            continue;
        };
        let Some((component, pages, enabled, item)) = host.children.iter().find_map(|child| {
            let component = scene.node(*child)?;
            match &component.kind {
                NodeKind::PointOfInterest {
                    pages,
                    enabled,
                    reward,
                    ..
                } => Some((component.id, pages.as_slice(), *enabled, reward.is_some())),
                _ => None,
            }
        }) else {
            continue;
        };
        if preview_reference_hidden(
            scene,
            hidden_scene_nodes,
            bounds.node,
            Some(component),
            None,
            None,
        ) {
            continue;
        }

        let selected =
            preview_reference_selected(selected, bounds.node, Some(component), None, None);
        let hovered = hovered.is_some_and(|hovered| {
            preview_reference_selected(hovered, bounds.node, Some(component), None, None)
        });
        let incomplete = pages.is_empty() || pages.iter().any(|page| page.trim().is_empty());
        let style = poi_preview_style(enabled, incomplete, selected, hovered).tinted(item);
        push_point_of_interest_beacon(bounds, tick, style, scratch);
    }
}

impl PoiPreviewStyle {
    /// Item beacons re-hue the ember palette to cyan, matching the in-game
    /// renderer (`marker_runtime::archive_beacon_tint`).
    fn tinted(self, item: bool) -> Self {
        if !item {
            return self;
        }
        let cyan = |(r, _g, b): (u8, u8, u8)| (b, ((u16::from(r) * 3) / 4) as u8, r);
        Self {
            face: cyan(self.face),
            side: cyan(self.side),
        }
    }
}

fn poi_preview_style(
    enabled: bool,
    incomplete: bool,
    selected: bool,
    hovered: bool,
) -> PoiPreviewStyle {
    if !enabled {
        return PoiPreviewStyle {
            face: (45, 18, 18),
            side: (31, 12, 12),
        };
    }
    if incomplete {
        return PoiPreviewStyle {
            face: (144, 78, 20),
            side: (78, 39, 10),
        };
    }
    if selected {
        return PoiPreviewStyle {
            face: (220, 58, 39),
            side: (105, 15, 11),
        };
    }
    if hovered {
        return PoiPreviewStyle {
            face: (188, 45, 31),
            side: (92, 14, 10),
        };
    }
    PoiPreviewStyle {
        face: (105, 15, 11),
        side: (58, 9, 7),
    }
}

fn push_point_of_interest_beacon(
    bounds: &psxed_ui::EntityBounds,
    tick: u32,
    style: PoiPreviewStyle,
    scratch: &mut PreviewScratch,
) {
    let height = (bounds.half_extents[1] * 2.0).max(64.0).round() as i32;
    let half = (height / 2).max(3);
    let cut = ((height * 11) / 50).clamp(2, half.saturating_sub(1));
    let half_depth = ((height * 2) / 25).max(4);
    let ground_y = (bounds.center[1] - bounds.half_extents[1]).round() as i32;
    let center = [
        bounds.center[0].round() as i32,
        ground_y.saturating_add(half),
        bounds.center[2].round() as i32,
    ];
    let authored_yaw = yaw_to_q12(bounds.yaw_degrees);
    let yaw = authored_yaw.wrapping_add(tick.wrapping_mul(POI_ROTATION_Q12_PER_TICK) as u16);
    let local = [
        [-half + cut, half],
        [half, half],
        [half, -half + cut],
        [half - cut, -half],
        [-half, -half],
        [-half, half - cut],
    ];
    let front_world = poi_face_world_points(center, yaw, half_depth, local);
    let back_world = poi_face_world_points(center, yaw, -half_depth, local);
    let Some(front) = project_poi_face(front_world) else {
        return;
    };
    let Some(back) = project_poi_face(back_world) else {
        return;
    };

    for edge in 0..POI_FACE_VERTICES {
        let next = (edge + 1) % POI_FACE_VERTICES;
        push_poi_triangle(scratch, [front[edge], front[next], back[next]], style.side);
        push_poi_triangle(scratch, [front[edge], back[next], back[edge]], style.side);
    }

    let visible_face = if poi_face_depth(front) <= poi_face_depth(back) {
        front
    } else {
        back
    };
    for triangle in POI_FACE_TRIANGLES {
        push_poi_triangle(
            scratch,
            [
                visible_face[triangle[0]],
                visible_face[triangle[1]],
                visible_face[triangle[2]],
            ],
            style.face,
        );
    }
}

fn poi_face_world_points(
    center: [i32; 3],
    yaw: u16,
    depth: i32,
    local: [[i32; 2]; POI_FACE_VERTICES],
) -> [[i32; 3]; POI_FACE_VERTICES] {
    let sin = sin_q12_turn(yaw);
    let cos = cos_q12_turn(yaw);
    local.map(|[x, y]| {
        [
            center[0].saturating_add(
                x.saturating_mul(cos)
                    .saturating_add(depth.saturating_mul(sin))
                    >> 12,
            ),
            center[1].saturating_add(y),
            center[2].saturating_add(
                depth
                    .saturating_mul(cos)
                    .saturating_sub(x.saturating_mul(sin))
                    >> 12,
            ),
        ]
    })
}

fn project_poi_face(
    world: [[i32; 3]; POI_FACE_VERTICES],
) -> Option<[psx_gte::scene::Projected; POI_FACE_VERTICES]> {
    let projected = world.map(|point| gte_scene::project_vertex(world_to_view(point)));
    projected
        .iter()
        .all(|point| point.sz != 0)
        .then_some(projected)
}

fn poi_face_depth(points: [psx_gte::scene::Projected; POI_FACE_VERTICES]) -> u32 {
    points.iter().map(|point| u32::from(point.sz)).sum::<u32>() / POI_FACE_VERTICES as u32
}

fn push_poi_triangle(
    scratch: &mut PreviewScratch,
    projected: [psx_gte::scene::Projected; 3],
    color: (u8, u8, u8),
) {
    let slot = preview_geometry_depth_slot(projected_avg_sz(projected));
    let _ = push_tri_colors_at_slot(scratch, projected, [color; 3], slot);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poi_beacon_face_keeps_the_approved_opposing_corner_cuts() {
        let face = poi_face_world_points(
            [0, 32, 0],
            0,
            4,
            [
                [-18, 32],
                [32, 32],
                [32, -18],
                [18, -32],
                [-32, -32],
                [-32, 18],
            ],
        );
        assert_eq!(face[0], [-18, 64, 4]);
        assert_eq!(face[2], [32, 14, 4]);
        assert_eq!(face[4], [-32, 0, 4]);
    }

    #[test]
    fn poi_beacon_authoring_states_remain_visually_distinct() {
        let idle = poi_preview_style(true, false, false, false);
        let selected = poi_preview_style(true, false, true, false);
        let incomplete = poi_preview_style(true, true, false, false);
        let disabled = poi_preview_style(false, false, false, false);
        assert_ne!(idle, selected);
        assert_ne!(idle, incomplete);
        assert_ne!(idle, disabled);
        assert!(u16::from(selected.face.0) > u16::from(idle.face.0));
    }

    #[test]
    fn poi_beacon_is_submitted_as_depth_sorted_world_geometry() {
        let mut project = ProjectDocument::new("depth-sorted POI preview");
        let root = project.active_scene().root;
        let host = project
            .active_scene_mut()
            .add_node(root, "Archive Beacon", NodeKind::Entity);
        project.active_scene_mut().add_node(
            host,
            "Point of Interest",
            NodeKind::PointOfInterest {
                pages: vec!["Recovered protocol.".to_owned()],
                prompt: "READ".to_owned(),
                radius: 576,
                marker_height: 192,
                repeatable: false,
                persistence_id: String::new(),
                reward: None,
                enabled: true,
            },
        );
        let bounds = [psxed_ui::EntityBounds {
            node: host,
            room: None,
            kind: psxed_ui::EntityBoundKind::PointOfInterest,
            center: [0.0, 96.0, 0.0],
            half_extents: [96.0; 3],
            yaw_degrees: 0.0,
        }];
        let mut scratch = new_preview_scratch();
        let _ = setup_gte_for_camera(ViewportCameraState {
            mode: psxed_ui::ViewportCameraMode::Orbit,
            yaw_q12: 0,
            pitch_q12: 0,
            radius: 512,
            target: [0; 3],
            position: [0; 3],
        });

        walk_point_of_interest_beacons(
            &project,
            &HashSet::new(),
            NodeId::ROOT,
            None,
            &bounds,
            0,
            &mut scratch,
        );

        assert!(scratch.used >= POI_FACE_TRIANGLES.len());
        assert!(
            scratch.overlay_lines.is_empty(),
            "POI visuals must stay in the ordering table, never in the host overlay"
        );
    }
}
