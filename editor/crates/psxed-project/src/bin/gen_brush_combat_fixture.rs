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
        player_components.iter().any(|(_, kind)| matches!(
            kind,
            NodeKind::Equipment {
                weapon: Some(_),
                ..
            }
        )),
        "sample player entity must carry armed Equipment"
    );
    assert!(
        enemy_components.iter().any(|(_, kind)| matches!(
            kind,
            NodeKind::Equipment {
                weapon: Some(_),
                ..
            }
        )),
        "sample enemy entity must carry armed Equipment"
    );

    // Replace the grid scenes with one fresh brush scene; the resource table
    // and its ids stay exactly as the sample verified them. The sample's UI
    // scenes (menus, music, SFX) are cortex presentation, not combat content,
    // so the fixture takes the plain defaults instead of copying megabytes of
    // unrelated audio.
    project.name = "Brush Combat Fixture".to_string();
    // The donor sample is a legacy grid project; this generator converts it,
    // so the format has to be restated. Without it the cook fails closed on
    // "grid project holding brushes", which is exactly the guard's job.
    project.set_world_format(psxed_project::ProjectWorldFormat::Bsp);
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

    // The same two-room structure as brush-first-playable, scaled fourfold so
    // the donor's authored 188/192-radius, 1024-high bodies genuinely fit the
    // world. The previous fixture copied the Cortex bodies into a 384-unit-high
    // interior and only passed because runtime collision always selected the
    // unrelated 16x56 debug hull.
    let static_boxes = [
        ([0, 0, 0], [4096, 256, 3072]),
        ([0, 1792, 0], [4096, 2048, 3072]),
        ([0, 256, 0], [256, 1792, 3072]),
        ([3840, 256, 0], [4096, 1792, 3072]),
        ([256, 256, 0], [3840, 1792, 256]),
        ([256, 256, 2816], [3840, 1792, 3072]),
        ([2016, 256, 256], [2080, 1792, 1280]),
        ([2016, 256, 1792], [2080, 1792, 2816]),
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
                open_offset: [0, 1536, 0],
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
        translation: [2048.0, 256.0, 1536.0],
        ..Transform3::default()
    };
    let mut door_brush = Brush::cuboid([2016, 256, 1280], [2080, 1024, 1792]);
    door_brush.mover = Some(door);
    paint(&mut door_brush, stone_material);
    scene.brushes.push(door_brush);

    for (name, translation, color) in [
        ("Left Lamp", [1024.0, 1280.0, 1536.0], [255, 160, 96]),
        ("Right Lamp", [3072.0, 1280.0, 1536.0], [96, 160, 255]),
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
            translation: [1024.0, 260.0, 1536.0],
            rotation_degrees: [0.0, 270.0, 0.0],
            ..Transform3::default()
        };
    } else {
        let scene = project.active_scene_mut();
        add_host(
            scene,
            "Player",
            [1024.0, 260.0, 1536.0],
            270.0,
            &player_components,
        );
        add_host(
            scene,
            "Mantis Enemy",
            [2300.0, 260.0, 1536.0],
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
/// hit breaks poise while the enemy still lives, a visible stagger), kill
/// with the first connecting heavy of the tail, and walk the cleared
/// doorway route to the far room. Poise break is strict
/// (`poise_damage > poise`), which is why equal-pool sequences like four
/// lights kill without ever staggering. Extra presses whiff harmlessly on
/// the corpse.
fn write_combat_tape(output_dir: &Path) {
    const UP: u16 = 1 << 4;
    const CROSS: u16 = 1 << 14;
    const R1: u16 = 1 << 11;
    const R2: u16 = 1 << 9;
    const L2: u16 = 1 << 8;
    // The fixture streams more assets than first-playable, so its loading
    // phase consumes ~150 tape frames before gameplay owns input. Every
    // action sits far after that; loading-frame samples are simply eaten.
    const FRAME_COUNT: usize = 1750;
    // Kill sequence: combo + heavy + combo (98 damage, 110 poise: a
    // survivable stagger), then a heavy tail whose first connection kills.
    // Tail spacing 37 is deliberately co-prime with the enemy's 45-tick
    // attack cadence so presses drift out of hit-stun phase lock instead of
    // colliding with it forever.
    const COMBO_PRESSES: [usize; 2] = [610, 750];
    const HEAVY_PRESSES: [usize; 7] = [680, 810, 847, 884, 921, 958, 995];
    const LIGHT_PRESSES: [usize; 0] = [];

    let mut tape = String::with_capacity(FRAME_COUNT * 32);
    writeln!(tape, "psoxide-tape,v2,clock=video_frame,start_poll=0").unwrap();
    writeln!(tape, "frame,buttons,right_x,right_y,left_x,left_y").unwrap();
    for frame in 0..FRAME_COUNT {
        let mut buttons = 0u16;
        if (240..650).contains(&frame) || (1200..1650).contains(&frame) {
            buttons |= UP;
        }
        if (430..434).contains(&frame) {
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
    write_door_occlusion_tape(output_dir);
}

/// Occlusion tape: walk to the closed door, never open it, and swing at it
/// while the enemy swings from the other side. World occlusion makes every
/// contact in both directions a miss, so the replay must end with zero
/// player melee hits and zero player hits taken while enemy attack enters
/// stays nonzero (it swung and was blocked).
fn write_door_occlusion_tape(output_dir: &Path) {
    const UP: u16 = 1 << 4;
    const R1: u16 = 1 << 11;
    const FRAME_COUNT: usize = 1200;
    const LIGHT_PRESSES: [usize; 3] = [720, 800, 880];

    let mut tape = String::with_capacity(FRAME_COUNT * 32);
    writeln!(tape, "psoxide-tape,v2,clock=video_frame,start_poll=0").unwrap();
    writeln!(tape, "frame,buttons,right_x,right_y,left_x,left_y").unwrap();
    for frame in 0..FRAME_COUNT {
        let mut buttons = 0u16;
        if (240..650).contains(&frame) {
            buttons |= UP;
        }
        if LIGHT_PRESSES
            .iter()
            .any(|press| (*press..press + 4).contains(&frame))
        {
            buttons |= R1;
        }
        writeln!(tape, "{frame},{buttons},128,128,128,128").unwrap();
    }
    std::fs::write(output_dir.join("door-blocks-damage.pxitape.csv"), tape)
        .expect("save door occlusion tape");
}
