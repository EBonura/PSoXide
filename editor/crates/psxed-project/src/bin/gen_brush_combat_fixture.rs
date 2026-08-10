//! Generate the tracked BSP combat acceptance fixture.
//!
//! The fixture is the first-playable two-room brush world armed with the
//! verified combat content from the tracked `samples/cortex_v1` sample: the
//! player entity composition (renderer, animator, controller, camera, light
//! sword equipment) and the Mantis enemy composition (renderer, animator,
//! enemy controller, heavy sword equipment) are cloned from that sample by
//! node name, so the fixture cannot drift from the content the sample
//! verified. The enemy has no authored attack capsules on purpose; its
//! reciprocal damage uses the documented legacy-arc fallback.
//!
//! Asset selection is cook-driven exactly like `miniaturise-project`: the
//! fixture project is cooked against the sample's asset tree, and only the
//! source files that cook actually reached are copied next to the fixture.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use psxed_project::brush::Brush;
use psxed_project::{
    LogicNodeKind, MaterialResource, MaterialTextureMode, NodeId, NodeKind, ProjectDocument,
    ResourceData, Transform3,
};

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sample_root = manifest_dir.join("../../samples/cortex_v1");
    let output_dir = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("../../projects/brush-combat-fixture"));

    let sample_text = std::fs::read_to_string(sample_root.join("project.ron"))
        .expect("read tracked cortex_v1 sample");
    let mut project =
        ProjectDocument::from_ron_str(&sample_text).expect("parse tracked cortex_v1 sample");

    let player_components = clone_components(&project, "Aletha");
    let enemy_components = clone_components(&project, "Mantis Enemy");
    assert!(
        player_components
            .iter()
            .any(|(_, kind)| matches!(kind, NodeKind::Equipment { weapon: Some(_), .. })),
        "sample player entity must carry armed Equipment"
    );
    assert!(
        enemy_components
            .iter()
            .any(|(_, kind)| matches!(kind, NodeKind::Equipment { weapon: Some(_), .. })),
        "sample enemy entity must carry armed Equipment"
    );

    // Replace the grid scenes with one fresh brush scene; the resource table
    // and its ids stay exactly as the sample verified them. The sample's UI
    // scenes (menus, music, SFX) are cortex presentation, not combat content,
    // so the fixture takes the plain defaults instead of copying megabytes of
    // unrelated audio.
    project.name = "Brush Combat Fixture".to_string();
    let donor = ProjectDocument::new("scene donor");
    project.scenes = donor.scenes;
    project.ui_scenes = donor.ui_scenes;
    project.scene_states = donor.scene_states;
    let stone_material = {
        let mut material = MaterialResource::opaque(None);
        material.texture_mode = MaterialTextureMode::Generated;
        project.add_resource("Warm Stone", ResourceData::Material(material))
    };
    let scene = project.active_scene_mut();

    // Identical structure to the proven brush-first-playable world: two rooms
    // split by a doorway column pair and one raised lift door between them.
    let static_boxes = [
        ([0, 0, 0], [1024, 64, 768]),
        ([0, 448, 0], [1024, 512, 768]),
        ([0, 64, 0], [64, 448, 768]),
        ([960, 64, 0], [1024, 448, 768]),
        ([64, 64, 0], [960, 448, 64]),
        ([64, 64, 704], [960, 448, 768]),
        ([480, 64, 64], [544, 448, 320]),
        ([480, 64, 448], [544, 448, 704]),
    ];
    for (mins, maxs) in static_boxes {
        let mut brush = Brush::cuboid(mins, maxs);
        paint(&mut brush, stone_material);
        scene.brushes.push(brush);
    }

    let door = scene.add_node(
        NodeId::ROOT,
        "Lift Door",
        NodeKind::Logic {
            kind: LogicNodeKind::Door {
                box_prop: String::new(),
                start_open: false,
                open_offset: [0, 192, 0],
                travel_ticks: 60,
            },
            target: String::new(),
            killtarget: String::new(),
            master: String::new(),
            delay_ticks: 0,
            wait_ticks: 0,
            enabled: true,
        },
    );
    scene.node_mut(door).expect("door node").transform = Transform3 {
        translation: [512.0, 64.0, 384.0],
        ..Transform3::default()
    };
    let mut door_brush = Brush::cuboid([480, 64, 320], [544, 256, 448]);
    door_brush.mover = Some(door);
    paint(&mut door_brush, stone_material);
    scene.brushes.push(door_brush);

    for (name, translation, color) in [
        ("Left Lamp", [256.0, 320.0, 384.0], [255, 160, 96]),
        ("Right Lamp", [768.0, 320.0, 384.0], [96, 160, 255]),
    ] {
        let light = scene.add_node(
            NodeId::ROOT,
            name,
            NodeKind::PointLight {
                color,
                intensity: 1.0,
                radius: 1.0,
            },
        );
        scene.node_mut(light).expect("light node").transform = Transform3 {
            translation,
            ..Transform3::default()
        };
    }

    // Player in room one, facing the door (+X route like the walkthrough
    // tape); armed Mantis in room two, facing back toward the doorway.
    // GEN_BARE_WORLD=1 swaps both entities for a plain spawn, isolating
    // world rendering from the character/equipment path when debugging.
    if std::env::var_os("GEN_BARE_WORLD").is_some() {
        // No Character resources at all selects the BSP debug controller,
        // keeping the probe to world geometry plus the full resource table.
        project
            .resources
            .retain(|resource| !matches!(resource.data, ResourceData::Character(_)));
        let scene = project.active_scene_mut();
        let spawn = scene.add_node(
            NodeId::ROOT,
            "Player Spawn",
            NodeKind::SpawnPoint {
                player: true,
                character: None,
            },
        );
        scene.node_mut(spawn).expect("spawn node").transform = Transform3 {
            translation: [256.0, 65.0, 384.0],
            rotation_degrees: [0.0, 270.0, 0.0],
            ..Transform3::default()
        };
    } else {
        let scene = project.active_scene_mut();
        add_host(
            scene,
            "Player",
            [256.0, 65.0, 384.0],
            270.0,
            &player_components,
        );
        add_host(
            scene,
            "Mantis Enemy",
            [820.0, 65.0, 384.0],
            90.0,
            &enemy_components,
        );
    }

    // Cook against the sample's asset tree purely to learn what the fixture
    // reaches. A fixture that cannot cook must not be written at all.
    let (package, report) = psxed_project::playtest::build_package(&project, &sample_root);
    for error in &report.errors {
        eprintln!("[gen-combat-fixture] {error}");
    }
    let package = package.expect("combat fixture must cook against the sample assets");

    std::fs::create_dir_all(&output_dir).expect("create fixture directory");
    let used: BTreeSet<&str> = package
        .used_texture_paths
        .iter()
        .chain(package.used_model_paths.iter())
        .chain(package.used_ui_paths.iter())
        .map(String::as_str)
        .collect();
    let mut copied = 0usize;
    let mut missing = 0usize;
    for key in &used {
        // Generated-material keys are descriptors, and CD-DA keys can carry a
        // `#loop` fragment; both conventions follow miniaturise-project.
        if key.starts_with('@') {
            continue;
        }
        let path_part = key.split('#').next().unwrap_or(key);
        let source = if Path::new(path_part).is_absolute() {
            PathBuf::from(path_part)
        } else {
            sample_root.join(path_part)
        };
        let relative = match (source.canonicalize(), sample_root.canonicalize()) {
            (Ok(abs_source), Ok(abs_root)) => match abs_source.strip_prefix(&abs_root) {
                Ok(relative) => relative.to_path_buf(),
                Err(_) => {
                    eprintln!("[gen-combat-fixture] REFUSED '{path_part}': outside the sample");
                    missing += 1;
                    continue;
                }
            },
            _ => {
                if path_part.contains('.') {
                    eprintln!("[gen-combat-fixture] MISSING source for '{path_part}'");
                    missing += 1;
                }
                continue;
            }
        };
        let destination = output_dir.join(&relative);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).expect("create fixture asset directory");
        }
        std::fs::copy(&source, &destination).expect("copy reached fixture asset");
        copied += 1;
    }

    let project_path = output_dir.join("project.ron");
    project
        .save_to_path(&project_path)
        .expect("save combat fixture project");
    let mut project_source =
        std::fs::read_to_string(&project_path).expect("read generated combat fixture");
    project_source.push('\n');
    std::fs::write(&project_path, project_source).expect("finish combat fixture");
    write_combat_tape(&output_dir);

    println!(
        "[gen-combat-fixture] wrote {} ({copied} reached assets, {missing} unresolved)",
        output_dir.display()
    );
}

/// Clone the component-children kinds of the named sample host entity.
fn clone_components(project: &ProjectDocument, host_name: &str) -> Vec<(String, NodeKind)> {
    let scene = project.active_scene();
    let host = scene
        .nodes()
        .iter()
        .find(|node| node.name == host_name && matches!(node.kind, NodeKind::Entity))
        .unwrap_or_else(|| panic!("sample scene has no '{host_name}' entity"));
    let components: Vec<(String, NodeKind)> = host
        .children
        .iter()
        .filter_map(|id| scene.node(*id))
        .filter(|node| node.kind.is_component())
        // The sample's camera distances are tuned for cortex room scale and
        // would place the eye outside this small sealed world (a solid leaf,
        // so PVS would draw nothing). The BSP default camera fits the box.
        .filter(|node| !matches!(node.kind, NodeKind::Camera { .. }))
        .map(|node| (node.name.clone(), node.kind.clone()))
        .collect();
    assert!(
        !components.is_empty(),
        "sample entity '{host_name}' has no component children"
    );
    components
}

/// Add one Entity host plus its cloned component children to the scene.
fn add_host(
    scene: &mut psxed_project::Scene,
    name: &str,
    translation: [f32; 3],
    yaw_degrees: f32,
    components: &[(String, NodeKind)],
) {
    let host = scene.add_node(NodeId::ROOT, name, NodeKind::Entity);
    scene.node_mut(host).expect("host node").transform = Transform3 {
        translation,
        rotation_degrees: [0.0, yaw_degrees, 0.0],
        ..Transform3::default()
    };
    for (component_name, kind) in components {
        scene.add_node(host, component_name, kind.clone());
    }
}

fn paint(brush: &mut Brush, material: psxed_project::ResourceId) {
    for face in &mut brush.faces {
        face.material = Some(material);
    }
}

/// One deterministic 60 Hz pad tape telling the complete checkpoint story:
/// walk to the door and open it, meet the arriving enemy, then land
/// combo + heavy + combo (98 damage, 110 poise on the 100 pool: the third
/// hit breaks poise while the enemy still lives, a visible stagger), finish
/// with a light hit, and walk the cleared doorway route to the far room.
/// Poise break is strict (`poise_damage > poise`), which is why equal-pool
/// sequences like four lights kill without ever staggering. Extra presses
/// whiff harmlessly on the corpse.
fn write_combat_tape(output_dir: &Path) {
    const UP: u16 = 1 << 4;
    const CROSS: u16 = 1 << 14;
    const R1: u16 = 1 << 11;
    const R2: u16 = 1 << 9;
    const L2: u16 = 1 << 8;
    // The fixture streams more assets than first-playable, so its loading
    // phase consumes ~150 tape frames before gameplay owns input. Every
    // action sits far after that; loading-frame samples are simply eaten.
    const FRAME_COUNT: usize = 1250;
    const COMBO_PRESSES: [usize; 2] = [420, 560];
    const HEAVY_PRESSES: [usize; 1] = [490];
    const LIGHT_PRESSES: [usize; 4] = [630, 690, 750, 810];

    let mut tape = String::with_capacity(FRAME_COUNT * 32);
    writeln!(tape, "psoxide-tape,v2,clock=video_frame,start_poll=0").unwrap();
    writeln!(tape, "frame,buttons,right_x,right_y,left_x,left_y").unwrap();
    for frame in 0..FRAME_COUNT {
        let mut buttons = 0u16;
        if (240..400).contains(&frame) || (870..1150).contains(&frame) {
            buttons |= UP;
        }
        if (266..270).contains(&frame) {
            buttons |= CROSS;
        }
        if COMBO_PRESSES
            .iter()
            .any(|press| (*press..press + 4).contains(&frame))
        {
            buttons |= L2;
        }
        if HEAVY_PRESSES
            .iter()
            .any(|press| (*press..press + 4).contains(&frame))
        {
            buttons |= R2;
        }
        if LIGHT_PRESSES
            .iter()
            .any(|press| (*press..press + 4).contains(&frame))
        {
            buttons |= R1;
        }
        writeln!(tape, "{frame},{buttons},128,128,128,128").unwrap();
    }
    std::fs::write(output_dir.join("combat-checkpoint.pxitape.csv"), tape)
        .expect("save combat fixture tape");
}
