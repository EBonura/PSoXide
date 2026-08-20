//! Generate an editable geometry-only E1M1 benchmark project.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use psxed_project::quake_bsp29::strip_quake_bsp29_geometry;
use psxed_project::quake_map_import::{import_quake_map_geometry_scaled, QUAKE_TO_EDITOR_SCALE};
use psxed_project::{
    CharacterAnimationAction, GeneratedMaterialTexture, MaterialResource, MaterialTextureMode,
    NodeId, NodeKind, ProjectDocument, ResourceData, ResourceId, Scene, Transform3,
};

const DEFAULT_OUTPUT_NAME: &str = "quake-e1m1-geometry";
const SOURCE_REPOSITORY: &str = "https://github.com/fzwoch/quake_map_source";
const SOURCE_REVISION: &str = "27abebaa3886bb0e3156cce3a604673d22b243f8";
const BSP_GEOMETRY_RELATIVE: &str = "assets/geometry/e1m1-topology.bsp29geom";
// A full 16x import exceeds the PXBSP 32,767-face limit after mandatory
// near-plane surface subdivision. E1M1 and both actors are uniformly reduced
// to one quarter of that canonical scale, preserving their proportions while
// keeping the complete level inside PSX coordinate and geometry budgets.
const E1M1_QUAKE_SCALE: i32 = 4;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let map_path = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("Usage: gen-quake-e1m1-project MAP_PATH [OUTPUT_DIR]"));
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output_dir = args.next().map(PathBuf::from).unwrap_or_else(|| {
        manifest_dir
            .join("..")
            .join("..")
            .join("projects")
            .join(DEFAULT_OUTPUT_NAME)
    });
    assert!(
        args.next().is_none(),
        "Usage: gen-quake-e1m1-project MAP_PATH [OUTPUT_DIR]"
    );

    let donor_dir = manifest_dir.join("../../projects/default");
    let donor_source =
        std::fs::read_to_string(donor_dir.join("project.ron")).expect("read default project donor");
    let mut project = ProjectDocument::from_ron_str(&donor_source).expect("parse default donor");
    let player_components = scaled_components(clone_components(&project, "Aletha"));
    let enemy_components = scaled_components(clone_components(&project, "Rust Mantis"));
    retain_locomotion_resources(&mut project);

    project.name = "Quake E1M1 Geometry Benchmark".to_string();
    project.set_world_format(psxed_project::ProjectWorldFormat::Bsp);
    project.bsp_cook_mode = psxed_project::brush_world::BrushWorldCookMode::Draft;
    project.bsp29_geometry_path = Some(BSP_GEOMETRY_RELATIVE.to_string());
    project.bsp29_geometry_scale = E1M1_QUAKE_SCALE;
    project.boot = psxed_project::BootTarget::Gameplay;
    let blank = ProjectDocument::new("blank scene");
    project.scenes = blank.scenes;
    project.ui_scenes = blank.ui_scenes;
    project.scene_states = blank.scene_states;
    project.options.clear();
    project.editor_visibility.show_grid = false;
    project.editor_visibility.preview_fog = false;
    project.editor_visibility.preview_bounds = false;
    project.editor_visibility.preview_backface_wireframe = false;
    project.editor_viewport.snap_units = 16 * E1M1_QUAKE_SCALE as u16;

    {
        let scene = project.active_scene_mut();
        let root = scene.root;
        if let NodeKind::World {
            sector_size,
            camera,
            culling,
            physics,
            ..
        } = &mut scene.node_mut(root).expect("world root").kind
        {
            *sector_size = 128 * E1M1_QUAKE_SCALE;
            scale_camera(camera);
            culling.draw_distance = scale_i32(culling.draw_distance).max(4096);
            physics.gravity_per_tick = scale_nonzero_i32(physics.gravity_per_tick);
        }
    }

    let mut grey = MaterialResource::opaque(None);
    grey.texture_mode = MaterialTextureMode::Generated;
    grey.generated = GeneratedMaterialTexture {
        size: 64,
        base_color: [78, 82, 88],
        noise_enabled: true,
        noise_color: [122, 128, 136],
        ..GeneratedMaterialTexture::default()
    };
    let grey = project.add_resource("E1M1 Geometry Grey", ResourceData::Material(grey));

    let map_source = std::fs::read_to_string(&map_path).expect("read E1M1 map source");
    let geometry = import_quake_map_geometry_scaled(&map_source, Some(grey), E1M1_QUAKE_SCALE)
        .unwrap_or_else(|error| panic!("E1M1 geometry import failed: {error}"));
    assert_eq!(
        geometry.stats.skipped_invalid_brushes, 0,
        "E1M1 import must not silently lose invalid brushes"
    );
    let mut player_start = geometry
        .player_start
        .expect("E1M1 map must contain info_player_start");
    // Quake's player origin is 24 units above the feet. PSoXide entity hosts
    // use a feet pivot, with one editor unit of lift to avoid exact contact.
    player_start[1] -= 24 * E1M1_QUAKE_SCALE;
    player_start[1] += 1;
    let enemy_start = [
        player_start[0],
        player_start[1],
        player_start[2] - 64 * E1M1_QUAKE_SCALE,
    ];

    project.editor_camera.orbit_target = [
        player_start[0],
        player_start[1] + 32 * E1M1_QUAKE_SCALE,
        player_start[2] - 32 * E1M1_QUAKE_SCALE,
    ];
    project.editor_camera.orbit_radius = 1024;
    project.editor_camera.orbit_yaw_q12 = 3584;
    project.editor_camera.orbit_pitch_q12 = 3584;
    project.editor_viewport.orthographic_focus =
        project.editor_camera.orbit_target.map(|v| v as f32);

    let stats = geometry.stats.clone();
    let scene = project.active_scene_mut();
    scene.brushes = geometry.brushes;
    add_host(
        scene,
        "Aletha",
        player_start.map(|value| value as f32),
        180.0,
        &player_components,
    );
    add_host(
        scene,
        "Rust Mantis",
        enemy_start.map(|value| value as f32),
        0.0,
        &enemy_components,
    );
    let light = scene.add_node(
        NodeId::ROOT,
        "Start Area Light",
        NodeKind::PointLight {
            color: [255, 238, 210],
            intensity: 1.2,
            radius: 2.5,
        },
    );
    scene.node_mut(light).expect("start light").transform = Transform3 {
        translation: [
            player_start[0] as f32,
            (player_start[1] + 96 * E1M1_QUAKE_SCALE) as f32,
            (player_start[2] - 32 * E1M1_QUAKE_SCALE) as f32,
        ],
        ..Transform3::default()
    };

    std::fs::create_dir_all(&output_dir).expect("create E1M1 project directory");
    copy_dir_recursive(&donor_dir.join("assets"), &output_dir.join("assets"));
    let bsp_path = map_path
        .parent()
        .expect("map source parent")
        .join("bsp")
        .join("e1m1.bsp");
    let stripped_bsp =
        strip_quake_bsp29_geometry(&std::fs::read(&bsp_path).unwrap_or_else(|error| {
            panic!("read compiled E1M1 BSP at {}: {error}", bsp_path.display())
        }))
        .unwrap_or_else(|error| panic!("strip E1M1 BSP topology: {error}"));
    let bsp_output = output_dir.join(BSP_GEOMETRY_RELATIVE);
    std::fs::create_dir_all(bsp_output.parent().expect("BSP output parent"))
        .expect("create BSP geometry directory");
    std::fs::write(&bsp_output, stripped_bsp).expect("write stripped BSP topology");
    let project_path = output_dir.join("project.ron");
    project
        .save_to_path(&project_path)
        .expect("save E1M1 project");
    let mut project_source = std::fs::read_to_string(&project_path).expect("read saved project");
    project_source.push('\n');
    std::fs::write(&project_path, project_source).expect("finish E1M1 project");
    write_provenance(&output_dir, &stats);
    if let Some(license) = map_path.parent().map(|parent| parent.join("COPYING")) {
        if license.is_file() {
            std::fs::copy(license, output_dir.join("SOURCE-COPYING")).expect("copy source license");
        }
    }

    println!(
        "[gen-quake-e1m1] wrote {}: {} brushes, {} faces, {} helper brushes skipped, {} non-geometry brushes skipped",
        output_dir.display(),
        stats.imported_brushes,
        stats.imported_faces,
        stats.skipped_helper_brushes,
        stats.skipped_non_geometry_brushes,
    );
}

fn clone_components(project: &ProjectDocument, host_name: &str) -> Vec<(String, NodeKind)> {
    let scene = project.active_scene();
    let host = scene
        .nodes()
        .iter()
        .find(|node| node.name == host_name && matches!(node.kind, NodeKind::Entity))
        .unwrap_or_else(|| panic!("default project has no '{host_name}' entity"));
    let components: Vec<_> = host
        .children
        .iter()
        .filter_map(|id| scene.node(*id))
        .filter(|node| node.kind.is_component() && !matches!(node.kind, NodeKind::Equipment { .. }))
        .map(|node| (node.name.clone(), node.kind.clone()))
        .collect();
    assert!(!components.is_empty(), "'{host_name}' has no components");
    components
}

fn retain_locomotion_resources(project: &mut ProjectDocument) {
    let animation_sets: Vec<ResourceId> = project
        .resources
        .iter_mut()
        .filter_map(|resource| match &mut resource.data {
            ResourceData::Character(character) => {
                character.combat_capsules.clear();
                character.animation_set
            }
            _ => None,
        })
        .collect();
    for resource in &mut project.resources {
        let ResourceData::AnimationSet(set) = &mut resource.data else {
            continue;
        };
        if !animation_sets.contains(&resource.id) {
            continue;
        }
        set.roll_clip = None;
        set.backstep_clip = None;
        set.action_clips.retain(|binding| {
            matches!(
                binding.action,
                CharacterAnimationAction::Idle
                    | CharacterAnimationAction::Walk
                    | CharacterAnimationAction::Run
                    | CharacterAnimationAction::Turn
            )
        });
        let mut keep = Vec::new();
        for clip in [set.idle_clip, set.walk_clip, set.run_clip, set.turn_clip]
            .into_iter()
            .flatten()
            .chain(set.action_clips.iter().map(|binding| binding.clip))
        {
            if !keep.contains(&clip) {
                keep.push(clip);
            }
        }
        set.clips.retain(|clip| keep.contains(clip));
    }
}

fn scaled_components(components: Vec<(String, NodeKind)>) -> Vec<(String, NodeKind)> {
    components
        .into_iter()
        .map(|(name, kind)| (name, scale_component(kind)))
        .collect()
}

fn scale_component(kind: NodeKind) -> NodeKind {
    match kind {
        NodeKind::ModelRenderer {
            model,
            material,
            mut visual_offset,
            visual_scale_q8,
        } => {
            for value in &mut visual_offset {
                *value = scale_i32(i32::from(*value)) as i16;
            }
            NodeKind::ModelRenderer {
                model,
                material,
                visual_offset,
                visual_scale_q8: ((u32::from(visual_scale_q8) * E1M1_QUAKE_SCALE as u32
                    + (QUAKE_TO_EDITOR_SCALE as u32 / 2))
                    / QUAKE_TO_EDITOR_SCALE as u32)
                    .max(1) as u16,
            }
        }
        NodeKind::CharacterController {
            character,
            mut settings,
            player,
        } => {
            if let Some(settings) = &mut settings {
                settings.radius = scale_u16(settings.radius);
                settings.height = scale_u16(settings.height);
                settings.walk_speed = scale_nonzero_i32(settings.walk_speed);
                settings.run_speed = scale_nonzero_i32(settings.run_speed);
                settings.roll_speed = scale_nonzero_i32(settings.roll_speed);
                settings.backstep_speed = scale_nonzero_i32(settings.backstep_speed);
                if let Some(enemy) = &mut settings.enemy {
                    enemy.aggro_radius = scale_u16(enemy.aggro_radius);
                    enemy.patrol_offset = enemy.patrol_offset.map(scale_i32);
                    enemy.preferred_distance = scale_u16(enemy.preferred_distance);
                    enemy.spacing_tolerance = scale_u16(enemy.spacing_tolerance);
                }
            }
            NodeKind::CharacterController {
                character,
                settings,
                player,
            }
        }
        NodeKind::Camera { mut settings } => {
            scale_camera(&mut settings);
            NodeKind::Camera { settings }
        }
        other => other,
    }
}

fn scale_camera(camera: &mut psxed_project::WorldCameraSettings) {
    camera.distance = scale_nonzero_i32(camera.distance);
    camera.height = scale_i32(camera.height);
    camera.target_height = scale_i32(camera.target_height);
    camera.min_floor_clearance = scale_i32(camera.min_floor_clearance);
}

fn scale_i32(value: i32) -> i32 {
    value.saturating_mul(E1M1_QUAKE_SCALE) / QUAKE_TO_EDITOR_SCALE
}

fn scale_nonzero_i32(value: i32) -> i32 {
    let scaled = scale_i32(value);
    if value > 0 {
        scaled.max(1)
    } else if value < 0 {
        scaled.min(-1)
    } else {
        0
    }
}

fn scale_u16(value: u16) -> u16 {
    scale_nonzero_i32(i32::from(value)) as u16
}

fn add_host(
    scene: &mut Scene,
    name: &str,
    translation: [f32; 3],
    yaw_degrees: f32,
    components: &[(String, NodeKind)],
) {
    let host = scene.add_node(NodeId::ROOT, name, NodeKind::Entity);
    scene.node_mut(host).expect("entity host").transform = Transform3 {
        translation,
        rotation_degrees: [0.0, yaw_degrees, 0.0],
        ..Transform3::default()
    };
    for (component_name, kind) in components {
        scene.add_node(host, component_name, kind.clone());
    }
}

fn copy_dir_recursive(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).expect("create asset directory");
    for entry in std::fs::read_dir(source).expect("read donor assets") {
        let entry = entry.expect("read donor asset entry");
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("read donor asset type").is_dir() {
            copy_dir_recursive(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("copy donor asset");
        }
    }
}

fn write_provenance(
    output_dir: &Path,
    stats: &psxed_project::quake_map_import::QuakeMapImportStats,
) {
    let mut text = String::new();
    writeln!(text, "# E1M1 geometry source").unwrap();
    writeln!(text).unwrap();
    writeln!(
        text,
        "This project contains brush-plane geometry derived from the original Quake E1M1 map source released by John Romero in October 2006."
    )
    .unwrap();
    writeln!(text).unwrap();
    writeln!(text, "- Source: {SOURCE_REPOSITORY}").unwrap();
    writeln!(text, "- Source revision: `{SOURCE_REVISION}`").unwrap();
    writeln!(text, "- License: GPL-2.0, reproduced in `SOURCE-COPYING`").unwrap();
    writeln!(text, "- Imported brushes: {}", stats.imported_brushes).unwrap();
    writeln!(text, "- Imported faces: {}", stats.imported_faces).unwrap();
    writeln!(text, "- Quake coordinate scale: {E1M1_QUAKE_SCALE}x").unwrap();
    writeln!(
        text,
        "- Actor visual/controller scale: 0.25x canonical PSoXide"
    )
    .unwrap();
    writeln!(
        text,
        "- Runtime BSP topology/PVS: textureless geometry-only derivative of the released `bsp/e1m1.bsp`"
    )
    .unwrap();
    writeln!(text).unwrap();
    writeln!(
        text,
        "No Quake texture names, texture pixels, lightmaps, gameplay entities, triggers, monsters, items, model assets, or audio are included. PSoXide's generated grey material is assigned to every imported face. The stripped BSP sidecar retains only planes, vertices, faces, nodes, leaves, mark-surfaces, edges, visibility, and world-model bounds needed to reuse E1M1's partition/PVS."
    )
    .unwrap();
    std::fs::write(output_dir.join("SOURCE.md"), text).expect("write source provenance");
}
