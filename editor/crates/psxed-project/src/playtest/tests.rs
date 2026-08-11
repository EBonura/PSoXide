use super::*;
use crate::tests::unique_temp_dir;
use crate::{
    ArchPropGeometry, GridUvTransform, NodeKind, ProjectDocument, UiRect, ARCH_PROP_MATERIAL_COUNT,
};

fn starter_project_root() -> PathBuf {
    crate::default_project_dir()
}

fn test_wav_mono_44k() -> Vec<u8> {
    let sample_rate = 44_100u32;
    let samples = [0i16, 2048, -2048, 4096, -4096, 0, 1024, -1024];
    let data_bytes = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_bytes as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

#[test]
fn brush_cook_diagnostics_keep_a_typed_editor_focus_target() {
    let target =
        brush_world_validation_target(&crate::brush_world::BrushWorldCookError::InvalidBrush {
            brush: 7,
            face: Some(2),
        });
    assert_eq!(
        target,
        Some(PlaytestValidationTarget::Brush {
            brush: 7,
            face: Some(2),
        })
    );

    let node = NodeId(91);
    assert_eq!(
        brush_world_validation_target(
            &crate::brush_world::BrushWorldCookError::PlayerSpawnInSolid(node)
        ),
        Some(PlaytestValidationTarget::Node(node))
    );

    let resource = ResourceId(41);
    assert_eq!(
        brush_world_validation_target(&crate::brush_world::BrushWorldCookError::MissingMaterial(
            resource
        )),
        Some(PlaytestValidationTarget::Resource(resource))
    );

    let mut project = ProjectDocument::new("invalid brush package");
    let mut invalid = crate::brush::Brush::cuboid([0, 0, 0], [128, 128, 128]);
    invalid.faces.truncate(3);
    invalid.faces[0].points = [[0; 3]; 3];
    project.active_scene_mut().brushes.push(invalid);
    let (package, report) = build_package(&project, Path::new("."));
    assert!(package.is_none());
    assert_eq!(
        report.focus_target(),
        Some(PlaytestValidationTarget::Brush {
            brush: 0,
            face: Some(0),
        })
    );
    assert!(report.errors[0].contains("brush 0 has invalid face 0"));
}

/// The report keeps a target PER ERROR, not one for the whole report, and the
/// `focus_target` convenience still answers with the first focusable one.
/// `blaming` fills in only the errors that named nothing themselves.
#[test]
fn every_report_error_keeps_its_own_focus_target() {
    let node_a = crate::NodeId(7);
    let node_b = crate::NodeId(9);
    let resource = crate::ResourceId(3);

    let mut report = PlaytestValidationReport::default();
    report.error("no offender for this one");
    report.error_at(PlaytestValidationTarget::Node(node_a), "first offender");
    report.blaming(PlaytestValidationTarget::Node(node_b), |report| {
        report.error("raised by a helper that only knows a name");
        report.error_at(PlaytestValidationTarget::Resource(resource), "knows better");
    });

    let targets: Vec<_> = report.errors.iter().map(|error| error.target).collect();
    assert_eq!(
        targets,
        vec![
            None,
            Some(PlaytestValidationTarget::Node(node_a)),
            Some(PlaytestValidationTarget::Node(node_b)),
            Some(PlaytestValidationTarget::Resource(resource)),
        ],
        "blaming fills in untargeted errors and leaves precise ones alone"
    );
    assert_eq!(
        report.focus_target(),
        Some(PlaytestValidationTarget::Node(node_a)),
        "the convenience accessor skips the untargeted first error"
    );
    assert_eq!(
        report.error_messages().join("; "),
        "no offender for this one; first offender; \
raised by a helper that only knows a name; knows better"
    );
}

#[test]
fn tile_arch_cooks_surfaces_materials_and_segmented_collision() {
    let mut project = ProjectDocument::starter();
    let room_id = project
        .active_scene()
        .nodes()
        .iter()
        .find(|node| matches!(node.kind, NodeKind::Section { .. }))
        .map(|node| node.id)
        .expect("starter has a room");
    let material = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .map(|resource| resource.id)
        .expect("starter has a material");
    let arch_id = project.active_scene_mut().add_node(
        room_id,
        "Cook Test Arch",
        NodeKind::ArchProp {
            materials: [Some(material); ARCH_PROP_MATERIAL_COUNT],
            uvs: [GridUvTransform::IDENTITY; ARCH_PROP_MATERIAL_COUNT],
            geometry: ArchPropGeometry {
                filled_top: true,
                ..ArchPropGeometry::default()
            },
            collision_enabled: true,
        },
    );
    project
        .active_scene_mut()
        .node_mut(arch_id)
        .expect("arch exists")
        .transform
        .translation = [0.0, 0.0, 0.0];

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "{report:?}");
    let package = package.expect("arch project cooks");
    assert_eq!(package.arch_props.len(), 1);
    assert_eq!(package.arch_prop_surfaces.len(), 43);
    assert_eq!(package.arch_prop_collisions.len(), 8);
    assert_eq!(package.arch_props[0].surface_count, 43);
    assert_eq!(package.arch_props[0].collision_count, 8);
    let manifest = render_manifest_source(&package);
    assert!(manifest.contains("pub static ARCH_PROPS: &[LevelArchPropRecord]"));
    assert!(manifest.contains("pub static ARCH_PROP_SURFACES: &[LevelArchPropSurfaceRecord]"));
    assert!(manifest.contains("pub static ARCH_PROP_COLLISIONS: &[LevelArchPropCollisionRecord]"));
}

#[test]
#[ignore = "diagnostic: set PSXED_DIAG_PROJECT and run with --ignored --nocapture"]
fn diag_project_cook() {
    let root = std::env::var_os("PSXED_DIAG_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../projects/demo11"));
    let mut project =
        ProjectDocument::load_from_path(root.join("project.ron")).expect("load diagnostic project");
    project.normalize_loaded();

    // Authored side: room node, its floors, the player entity Y.
    let scene = project.active_scene();
    for node in scene.nodes() {
        if let NodeKind::Section { grid } = &node.kind {
            println!(
                "ROOM node id={} name={:?} sector_size={} base_elev={} floors={}",
                node.id.raw(),
                node.name,
                grid.sector_size,
                grid.elevation,
                grid.floor_count()
            );
            for i in 0..grid.floor_count() {
                if let Some(f) = grid.floor(i) {
                    println!(
                        "   floor {i}: elevation={} populated_cells={} dims={}x{} origin={:?}",
                        f.elevation,
                        f.populated_sector_count(),
                        f.width,
                        f.depth,
                        f.origin,
                    );
                }
            }
        }
    }
    for node in scene.nodes() {
        let is_player = matches!(&node.kind, NodeKind::SpawnPoint { player: true, .. })
            || scene.nodes().iter().any(|c| {
                c.parent == Some(node.id)
                    && matches!(&c.kind, NodeKind::CharacterController { player: true, .. })
            });
        if is_player {
            println!(
                "PLAYER node id={} name={:?} translation={:?}",
                node.id.raw(),
                node.name,
                node.transform.translation
            );
            // Physics check: where is the player relative to each
            // floor's surface at its cell?
            if let Some(room) = enclosing_room(scene, node) {
                if let NodeKind::Section { grid } = &room.kind {
                    let s = grid.sector_size;
                    let node_y_eng = (node.transform.translation[1] * s as f32) as i32;
                    let local = grid.editor_to_room_local([
                        node.transform.translation[0],
                        node.transform.translation[2],
                    ]);
                    println!(
                        "  player_y_engine={} (room-local cell from xz={:?})",
                        node_y_eng, local
                    );
                    for i in 0..grid.floor_count() {
                        if let Some(f) = grid.floor(i) {
                            let surf =
                                f.floor_height_at_room_local(local[0] as i32, local[2] as i32);
                            println!(
                                    "   floor {i}: base_elev={} surface_at_cell={:?} -> player is {} above base",
                                    f.elevation,
                                    surf,
                                    node_y_eng - f.elevation
                                );
                        }
                    }
                }
            }
        }
    }

    let (package, report) = build_package(&project, &root);
    println!("report.ok={} errors={:?}", report.is_ok(), report);
    let package = package.expect("diagnostic project cooked");

    println!("=== COOKED rooms ({}) ===", package.rooms.len());
    for (i, r) in package.rooms.iter().enumerate() {
        println!(
                "  room[{i}] name={:?} origin=({},{}) origin_y={} size={}x{} portal_first={} portal_count={}",
                r.name, r.origin_x, r.origin_z, r.origin_y, r.material_first, r.material_count,
                r.portal_first, r.portal_count
            );
    }
    println!("=== COOKED chunks ({}) ===", package.chunks.len());
    for (i, c) in package.chunks.iter().enumerate() {
        println!(
            "  chunk[{i}] room={} authored_room={} origin=({},{}) {}x{} neighbours={:?}",
            c.room, c.authored_room, c.origin_x, c.origin_z, c.width, c.depth, c.neighbours
        );
    }
    println!("=== room_portals ({}) ===", package.room_portals.len());
    for (i, p) in package.room_portals.iter().enumerate() {
        println!(
            "  portal[{i}] src={} dst={} kind={} normal={:?} verts={:?}",
            p.source_room, p.destination_room, p.kind, p.normal, p.vertices
        );
    }
    println!(
        "=== room_floor_links ({}) ===",
        package.room_floor_links.len()
    );
    for (i, l) in package.room_floor_links.iter().enumerate() {
        println!(
            "  link[{i}] room={} cell=({},{}) above={:?} below={:?}",
            l.room, l.x, l.z, l.above_room, l.below_room
        );
    }
    if let Some(spawn) = package.spawn {
        println!(
            "=== SPAWN room={} pos=({},{},{}) -> room origin_y={} ===",
            spawn.room,
            spawn.x,
            spawn.y,
            spawn.z,
            package
                .rooms
                .get(spawn.room as usize)
                .map(|r| r.origin_y)
                .unwrap_or(-1)
        );
    }
}

fn visibility_test_cell(x: u16, z: u16, blocker_mask: u8) -> PlaytestVisibilityCell {
    PlaytestVisibilityCell {
        room: 0,
        x,
        z,
        min_y: 0,
        max_y: crate::DEFAULT_WORLD_SECTOR_SIZE,
        portal_mask: 0,
        blocker_mask,
        cache_cell_index: u16::MAX,
        flags: visibility_cell_flags::HAS_GEOMETRY,
    }
}

fn project_with_one_room() -> ProjectDocument {
    let mut project = ProjectDocument::starter();
    let room_material = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .map(|resource| resource.id);
    let (world_kind, room_grid, player_name, player_kind, player_children, light_template) = {
        let scene = project.active_scene();
        let world = scene
            .nodes()
            .iter()
            .find(|node| matches!(node.kind, NodeKind::World { .. }))
            .expect("starter must contain a World");
        let room = scene
            .nodes()
            .iter()
            .find(|node| matches!(node.kind, NodeKind::Section { .. }))
            .expect("starter must contain a Room");
        let NodeKind::Section { grid } = &room.kind else {
            unreachable!();
        };
        let player = scene
            .nodes()
            .iter()
            .find(|node| is_player_spawn_node(scene, node))
            .expect("starter must contain a player spawn entity");
        let children: Vec<_> = player
            .children
            .iter()
            .filter_map(|id| {
                let child = scene.node(*id)?;
                Some((child.name.clone(), child.kind.clone(), child.transform))
            })
            .collect();
        // The minimal starter ships no lights, so synthesize the test
        // fixture light the light-cook tests build on when absent.
        let light = scene
            .nodes()
            .iter()
            .find(|node| matches!(node.kind, NodeKind::PointLight { .. }))
            .map(|node| (node.name.clone(), node.kind.clone()))
            .or_else(|| {
                Some((
                    "Preview Light".to_string(),
                    NodeKind::PointLight {
                        color: [255, 244, 214],
                        intensity: 1.25,
                        radius: 3.0,
                    },
                ))
            });
        (
            world.kind.clone(),
            grid.clone(),
            player.name.clone(),
            player.kind.clone(),
            children,
            light,
        )
    };

    let mut grid = WorldGrid::stone_room(1, 1, room_grid.sector_size, room_material, room_material);
    grid.ambient_color = room_grid.ambient_color;
    grid.fog_enabled = room_grid.fog_enabled;
    grid.fog_color = room_grid.fog_color;
    grid.fog_near = room_grid.fog_near;
    grid.fog_far = room_grid.fog_far;
    grid.atmosphere_enabled = room_grid.atmosphere_enabled;
    grid.atmosphere_color = room_grid.atmosphere_color;
    grid.atmosphere_density = room_grid.atmosphere_density;
    grid.atmosphere_fall_speed_q4 = room_grid.atmosphere_fall_speed_q4;
    grid.atmosphere_wind_speed_q4 = room_grid.atmosphere_wind_speed_q4;

    let mut scene = crate::Scene::new("Main");
    let world_id = scene.root;
    if let Some(world) = scene.node_mut(world_id) {
        world.name = "World".to_string();
        world.kind = world_kind;
    }
    let room_id = scene.add_node(world_id, "Room", NodeKind::Section { grid });
    if let Some((name, kind)) = light_template {
        let light_id = scene.add_node(room_id, name, kind);
        if let Some(light) = scene.node_mut(light_id) {
            light.transform.translation = [0.0, 1.5, 0.0];
        }
    }
    let player_id = scene.add_node(room_id, player_name, player_kind);
    if let Some(player) = scene.node_mut(player_id) {
        player.transform.translation = [0.0, 0.0, 0.0];
        player.transform.rotation_degrees = [0.0, 0.0, 0.0];
    }
    for (name, kind, transform) in player_children {
        let child_id = scene.add_node(player_id, name, kind);
        if let Some(child) = scene.node_mut(child_id) {
            child.transform = transform;
        }
    }
    project.scenes[0] = scene;

    let scene = project.active_scene();
    let has_room = scene
        .nodes()
        .iter()
        .any(|n| matches!(n.kind, NodeKind::Section { .. }));
    let has_player_spawn = scene.nodes().iter().any(|n| is_player_spawn_node(scene, n));
    assert!(has_room, "starter must contain a Room");
    assert!(
        has_player_spawn,
        "starter must contain a player spawn entity"
    );
    project
}

fn is_player_spawn_node(scene: &crate::Scene, node: &SceneNode) -> bool {
    match &node.kind {
        NodeKind::SpawnPoint { player: true, .. } => true,
        NodeKind::Entity => node.children.iter().any(|id| {
            scene.node(*id).is_some_and(|child| {
                matches!(
                    child.kind,
                    NodeKind::CharacterController { player: true, .. }
                )
            })
        }),
        _ => false,
    }
}

fn player_spawn_node_id(project: &ProjectDocument) -> NodeId {
    let scene = project.active_scene();
    scene
        .nodes()
        .iter()
        .find(|node| is_player_spawn_node(scene, node))
        .expect("starter has a player spawn entity")
        .id
}

fn player_controller_component_id(project: &ProjectDocument) -> NodeId {
    let scene = project.active_scene();
    scene
        .nodes()
        .iter()
        .find(|node| {
            matches!(
                node.kind,
                NodeKind::CharacterController { player: true, .. }
            )
        })
        .expect("starter has a player CharacterController")
        .id
}

fn player_character_resource_id(project: &ProjectDocument) -> ResourceId {
    let scene = project.active_scene();
    scene
        .nodes()
        .iter()
        .find_map(|node| match &node.kind {
            NodeKind::CharacterController {
                player: true,
                character: Some(character),
                ..
            } => Some(*character),
            _ => None,
        })
        .expect("starter has an assigned player Character")
}

fn player_model_resource_id(project: &ProjectDocument) -> ResourceId {
    let character_id = player_character_resource_id(project);
    project
        .resource(character_id)
        .and_then(|resource| match &resource.data {
            ResourceData::Character(character) => character.model,
            _ => None,
        })
        .expect("starter player Character has a Model")
}

fn demote_player_spawns(project: &mut ProjectDocument) {
    let scene = project.active_scene_mut();
    let ids: Vec<NodeId> = scene
        .nodes()
        .iter()
        .filter(|node| {
            matches!(
                node.kind,
                NodeKind::SpawnPoint { player: true, .. }
                    | NodeKind::CharacterController { player: true, .. }
            )
        })
        .map(|node| node.id)
        .collect();
    for id in ids {
        let Some(node) = scene.node_mut(id) else {
            continue;
        };
        match &mut node.kind {
            NodeKind::SpawnPoint { player, character } if *player => {
                *player = false;
                *character = None;
            }
            NodeKind::CharacterController {
                player, character, ..
            } if *player => {
                *player = false;
                *character = None;
            }
            _ => {}
        }
    }
}

fn starter_light_color(project: &ProjectDocument) -> [u8; 3] {
    project
        .active_scene()
        .nodes()
        .iter()
        .find_map(|node| match &node.kind {
            NodeKind::PointLight { color, .. } => Some(*color),
            _ => None,
        })
        .expect("starter contains one light")
}

fn starter_light_intensity_q8(project: &ProjectDocument) -> u16 {
    let intensity = project
        .active_scene()
        .nodes()
        .iter()
        .find_map(|node| match &node.kind {
            NodeKind::PointLight { intensity, .. } => Some(*intensity),
            _ => None,
        })
        .expect("starter contains one light");
    (intensity * 256.0).clamp(0.0, u16::MAX as f32) as u16
}

/// Insert the fixture preview light the light-cook tests exercise; the
/// minimal starter ships without lights.
fn insert_preview_light(project: &mut ProjectDocument) -> NodeId {
    let (room_id, over_middle_tile) = project
        .active_scene()
        .nodes()
        .iter()
        .find_map(|n| match &n.kind {
            NodeKind::Section { grid } => {
                let e =
                    grid.world_cells_to_editor([grid.width as f32 / 2.0, grid.depth as f32 / 2.0]);
                Some((n.id, [e[0], 1.5, e[1]]))
            }
            _ => None,
        })
        .expect("starter has a room");
    let scene = project.active_scene_mut();
    let id = scene.add_node(
        room_id,
        "Preview Light",
        NodeKind::PointLight {
            color: [255, 244, 214],
            intensity: 1.25,
            radius: 3.0,
        },
    );
    if let Some(light) = scene.node_mut(id) {
        light.transform.translation = over_middle_tile;
    }
    id
}

fn starter_light_ids(project: &ProjectDocument) -> Vec<NodeId> {
    project
        .active_scene()
        .nodes()
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::PointLight { .. }))
        .map(|n| n.id)
        .collect()
}

fn remove_model_renderer_components(project: &mut ProjectDocument) {
    let scene = project.active_scene_mut();
    let ids: Vec<NodeId> = scene
        .nodes()
        .iter()
        .filter(|node| {
            matches!(
                node.kind,
                NodeKind::ModelRenderer { model: Some(_), .. } | NodeKind::Animator { .. }
            )
        })
        .map(|node| node.id)
        .collect();
    for id in ids {
        scene.remove_node(id);
    }
}

fn set_first_model_instance_clip(project: &mut ProjectDocument, clip_index: u16) {
    let model_id = player_model_resource_id(project);
    let scene = project.active_scene_mut();
    let ids: Vec<NodeId> = scene
        .nodes()
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::MeshInstance { .. }))
        .map(|node| node.id)
        .collect();
    for id in ids {
        let Some(node) = scene.node_mut(id) else {
            continue;
        };
        if let NodeKind::MeshInstance { animation_clip, .. } = &mut node.kind {
            *animation_clip = Some(clip_index);
            return;
        }
    }
    let room_id = scene
        .nodes()
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Section { .. }))
        .map(|n| n.id)
        .expect("starter has Room");
    scene.add_node(
        room_id,
        "Invalid Clip Model",
        NodeKind::MeshInstance {
            mesh: Some(model_id),
            material: None,
            animation_clip: Some(clip_index),
        },
    );
}

mod assets_validation;
mod lights_components;
mod logic_entities;
mod models_entities;
mod player_character;
mod rooms_visibility;
mod ui_options;
mod water;
