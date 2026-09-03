//! Generate the grounding side-view fixture: a flat slab, the Cortex 0.4
//! player and both enemies cloned node-for-node from the tech demo project,
//! a gameplay camera at floor height looking horizontally, and a reference
//! cube of known height beside each actor.
//!
//! With the camera on the floor plane the floor projects to one screen line
//! regardless of depth, and each cube's bottom and top edges give that line
//! and the pixel-per-unit scale at the actor's own depth, so a `--dump-draws`
//! replay measures how far every actor's lowest vertex sits above the floor
//! without assuming a camera model. The source project is read, never
//! written; the fixture shares its `assets/` through a symlink.

use std::path::{Path, PathBuf};

use psxed_project::brush::Brush;
use psxed_project::{
    MaterialResource, NodeId, NodeKind, ProjectDocument, ResourceData, ResourceId, Scene,
    Transform3,
};

/// Source entities, by name, in the tech demo scene.
const ACTORS: [&str; 3] = ["Aletha (Player)", "Intake Custodian", "Heavy Enemy"];
/// Editor units; the cook divides by 16 for engine units.
const SLAB_TOP: i32 = 64;
const CUBE_HEIGHT: i32 = 256;
const CUBE_SIZE: i32 = 64;
/// Thin along the view axis so every cube face sits at the actor's depth.
const CUBE_DEPTH: i32 = 16;
/// Actors along X, the camera looks along -Z from behind the player.
const ACTOR_X: [i32; 3] = [2048, 768, 3328];
const ACTOR_Z: i32 = 2048;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let source = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| project_relative("projects/cortex-ignition-tech-demo-0.4/project.ron"));
    let output_dir = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| project_relative("archive/fixtures/grounding-side-view"));
    // Optional yaw in degrees for every actor, so the camera settles at a
    // non-cardinal angle and any disagreement between the world and model
    // passes shows up as a sideways shift of the cubes against the actors.
    let yaw_degrees = args
        .next()
        .and_then(|value| value.to_str().and_then(|value| value.parse::<f32>().ok()))
        .unwrap_or(0.0);
    generate(&source, &output_dir, yaw_degrees);
}

fn project_relative(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join(relative)
}

fn generate(source: &Path, output_dir: &Path, yaw_degrees: f32) {
    let mut project = ProjectDocument::load_from_path(source).expect("load source project");
    let source_scene = project.active_scene().clone();
    let floor_material = first_textured_material(&project);

    let mut scene = Scene::new("Grounding Side View");
    let mut slab = Brush::cuboid([0, 0, 0], [4096, SLAB_TOP, 4096]);
    paint(&mut slab, floor_material);
    scene.brushes.push(slab);

    for (index, name) in ACTORS.iter().enumerate() {
        let entity = source_scene
            .nodes()
            .iter()
            .find(|node| node.name == *name && matches!(node.kind, NodeKind::Entity))
            .unwrap_or_else(|| panic!("source entity {name}"));
        let new_entity = scene.add_node(NodeId::ROOT, entity.name.clone(), NodeKind::Entity);
        scene.node_mut(new_entity).expect("entity").transform = Transform3 {
            translation: [ACTOR_X[index] as f32, (SLAB_TOP + 1) as f32, ACTOR_Z as f32],
            rotation_degrees: [0.0, yaw_degrees, 0.0],
            ..Transform3::default()
        };
        for child_id in &entity.children {
            let child = source_scene.node(*child_id).expect("entity child");
            let mut kind = child.kind.clone();
            match &mut kind {
                NodeKind::Camera { settings } => {
                    settings.distance = 4000;
                    settings.height = 0;
                    settings.target_height = 0;
                    settings.lock_rise_percent = 0;
                    settings.min_floor_clearance = 0;
                }
                NodeKind::CharacterController {
                    settings: Some(settings),
                    ..
                } => {
                    if let Some(enemy) = settings.enemy.as_mut() {
                        enemy.aggro_radius = 1;
                        enemy.patrol_offset = [0, 0, 0];
                    }
                }
                _ => {}
            }
            let new_child = scene.add_node(new_entity, child.name.clone(), kind);
            scene.node_mut(new_child).expect("child").transform = child.transform.clone();
        }

        let cube_x = ACTOR_X[index] + 384;
        let mut cube = Brush::cuboid(
            [cube_x, SLAB_TOP, ACTOR_Z - CUBE_DEPTH / 2],
            [
                cube_x + CUBE_SIZE,
                SLAB_TOP + CUBE_HEIGHT,
                ACTOR_Z + CUBE_DEPTH / 2,
            ],
        );
        paint(&mut cube, floor_material);
        scene.brushes.push(cube);
    }

    // One Point of Interest beacon beside the player, cloned node-for-node,
    // with its own reference cube: the beacon is runtime geometry projected on
    // the CPU, so it measures the world pass against that path as well.
    if let Some(poi) = source_scene.nodes().iter().find(|node| {
        matches!(node.kind, NodeKind::Entity)
            && node.children.iter().any(|child| {
                matches!(
                    source_scene.node(*child).map(|n| &n.kind),
                    Some(NodeKind::PointOfInterest { .. })
                )
            })
    }) {
        let poi_x = 1408;
        let new_entity = scene.add_node(NodeId::ROOT, poi.name.clone(), NodeKind::Entity);
        scene.node_mut(new_entity).expect("poi entity").transform = Transform3 {
            translation: [poi_x as f32, (SLAB_TOP + 1) as f32, ACTOR_Z as f32],
            rotation_degrees: [0.0, yaw_degrees, 0.0],
            ..Transform3::default()
        };
        for child_id in &poi.children {
            let child = source_scene.node(*child_id).expect("poi child");
            let new_child = scene.add_node(new_entity, child.name.clone(), child.kind.clone());
            scene.node_mut(new_child).expect("poi child").transform = child.transform.clone();
        }
        let cube_x = poi_x + 192;
        let mut cube = Brush::cuboid(
            [cube_x, SLAB_TOP, ACTOR_Z - CUBE_DEPTH / 2],
            [
                cube_x + CUBE_SIZE,
                SLAB_TOP + CUBE_HEIGHT,
                ACTOR_Z + CUBE_DEPTH / 2,
            ],
        );
        paint(&mut cube, floor_material);
        scene.brushes.push(cube);
    }

    // One Point of Interest beacon beside the player, cloned node-for-node,
    // with its own reference cube: the beacon is runtime geometry projected on
    // the CPU, so it measures the world pass against that path as well.
    if let Some(poi) = source_scene.nodes().iter().find(|node| {
        matches!(node.kind, NodeKind::Entity)
            && node.children.iter().any(|child| {
                matches!(
                    source_scene.node(*child).map(|n| &n.kind),
                    Some(NodeKind::PointOfInterest { .. })
                )
            })
    }) {
        let poi_x = 1408;
        let new_entity = scene.add_node(NodeId::ROOT, poi.name.clone(), NodeKind::Entity);
        scene.node_mut(new_entity).expect("poi entity").transform = Transform3 {
            translation: [poi_x as f32, (SLAB_TOP + 1) as f32, ACTOR_Z as f32],
            rotation_degrees: [0.0, yaw_degrees, 0.0],
            ..Transform3::default()
        };
        for child_id in &poi.children {
            let child = source_scene.node(*child_id).expect("poi child");
            let new_child = scene.add_node(new_entity, child.name.clone(), child.kind.clone());
            scene.node_mut(new_child).expect("poi child").transform = child.transform.clone();
        }
        let cube_x = poi_x + 192;
        let mut cube = Brush::cuboid(
            [cube_x, SLAB_TOP, ACTOR_Z - CUBE_DEPTH / 2],
            [cube_x + CUBE_SIZE, SLAB_TOP + CUBE_HEIGHT, ACTOR_Z + CUBE_DEPTH / 2],
        );
        paint(&mut cube, floor_material);
        scene.brushes.push(cube);
    }

    // One Point of Interest beacon beside the player, cloned node-for-node,
    // with its own reference cube: the beacon is runtime geometry projected on
    // the CPU, so it measures the world pass against that path as well.
    if let Some(poi) = source_scene.nodes().iter().find(|node| {
        matches!(node.kind, NodeKind::Entity)
            && node.children.iter().any(|child| {
                matches!(
                    source_scene.node(*child).map(|n| &n.kind),
                    Some(NodeKind::PointOfInterest { .. })
                )
            })
    }) {
        let poi_x = 1408;
        let new_entity = scene.add_node(NodeId::ROOT, poi.name.clone(), NodeKind::Entity);
        scene.node_mut(new_entity).expect("poi entity").transform = Transform3 {
            translation: [poi_x as f32, (SLAB_TOP + 1) as f32, ACTOR_Z as f32],
            rotation_degrees: [0.0, yaw_degrees, 0.0],
            ..Transform3::default()
        };
        for child_id in &poi.children {
            let child = source_scene.node(*child_id).expect("poi child");
            let new_child = scene.add_node(new_entity, child.name.clone(), child.kind.clone());
            scene.node_mut(new_child).expect("poi child").transform = child.transform.clone();
        }
        let cube_x = poi_x + 192;
        let mut cube = Brush::cuboid(
            [cube_x, SLAB_TOP, ACTOR_Z - CUBE_DEPTH / 2],
            [cube_x + CUBE_SIZE, SLAB_TOP + CUBE_HEIGHT, ACTOR_Z + CUBE_DEPTH / 2],
        );
        paint(&mut cube, floor_material);
        scene.brushes.push(cube);
    }

    project.scenes = vec![scene];
    project.name = "Grounding Side View".to_string();

    std::fs::create_dir_all(output_dir).expect("create fixture directory");
    let assets_link = output_dir.join("assets");
    if !assets_link.exists() {
        let source_assets = source.parent().expect("source project dir").join("assets");
        let relative = pathdiff(&source_assets, output_dir);
        std::os::unix::fs::symlink(&relative, &assets_link).expect("link shared assets");
    }
    project
        .save_to_path(output_dir.join("project.ron"))
        .expect("save grounding fixture");
}

fn first_textured_material(project: &ProjectDocument) -> ResourceId {
    project
        .resources
        .iter()
        .find(|resource| match &resource.data {
            ResourceData::Material(MaterialResource {
                psxt_path: Some(path),
                ..
            }) => !path.contains("/sky/") && !path.contains("menu"),
            _ => false,
        })
        .map(|resource| resource.id)
        .expect("a textured material in the source project")
}

fn paint(brush: &mut Brush, material: ResourceId) {
    for face in &mut brush.faces {
        face.material = Some(material);
    }
}

/// `target` relative to `base`, both taken as directories under the same root.
fn pathdiff(target: &Path, base: &Path) -> PathBuf {
    let target = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    let base = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());
    let target: Vec<_> = target.components().collect();
    let base: Vec<_> = base.components().collect();
    let common = target
        .iter()
        .zip(base.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let mut out = PathBuf::new();
    for _ in common..base.len() {
        out.push("..");
    }
    for component in &target[common..] {
        out.push(component.as_os_str());
    }
    out
}
