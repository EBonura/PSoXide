//! Generate the tracked two-room brush Play acceptance map.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use psxed_project::brush::Brush;
use psxed_project::{
    LogicNodeKind, MaterialResource, MaterialTextureMode, NodeId, NodeKind, ProjectDocument,
    ResourceData, Transform3,
};

fn main() {
    let output_dir = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("projects")
                .join("brush-first-playable")
        });
    std::fs::create_dir_all(&output_dir).expect("create brush project directory");

    let mut project = ProjectDocument::new("Brush First Playable");
    let mut stone_material = MaterialResource::opaque(None);
    stone_material.texture_mode = MaterialTextureMode::Generated;
    let stone = project.add_resource("Warm Stone", ResourceData::Material(stone_material));
    let scene = project.active_scene_mut();

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
        paint(&mut brush, stone);
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
    paint(&mut door_brush, stone);
    scene.brushes.push(door_brush);

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
        // The normal playtest maps forward input through its camera yaw. Use
        // the quarter-turn that makes the canonical UP route advance toward
        // +X, without a BSP-only input convention.
        rotation_degrees: [0.0, 270.0, 0.0],
        ..Transform3::default()
    };

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

    let project_path = output_dir.join("project.ron");
    project
        .save_to_path(&project_path)
        .expect("save brush first playable");
    let mut project_source =
        std::fs::read_to_string(&project_path).expect("read generated brush first playable");
    project_source.push('\n');
    std::fs::write(&project_path, project_source).expect("finish brush first playable");
    write_walkthrough_tape(&output_dir);
}

fn paint(brush: &mut Brush, material: psxed_project::ResourceId) {
    for face in &mut brush.faces {
        face.material = Some(material);
    }
}

fn write_walkthrough_tape(output_dir: &Path) {
    const UP: u16 = 1 << 4;
    const CROSS: u16 = 1 << 14;
    const FRAME_COUNT: usize = 220;

    let mut tape = String::with_capacity(FRAME_COUNT * 32);
    writeln!(tape, "psoxide-tape,v2,clock=video_frame,start_poll=0").unwrap();
    writeln!(tape, "frame,buttons,right_x,right_y,left_x,left_y").unwrap();
    for frame in 0..FRAME_COUNT {
        // Normal Play has a short cold-load phase before gameplay owns input.
        // Keep that pre-roll explicit, then reproduce the original 150-tick
        // walk and fire use after 24 gameplay-relative movement samples.
        let mut buttons = if (30..180).contains(&frame) { UP } else { 0 };
        if (54..58).contains(&frame) {
            buttons |= CROSS;
        }
        writeln!(tape, "{frame},{buttons},128,128,128,128").unwrap();
    }
    std::fs::write(output_dir.join("walk-through-door.pxitape.csv"), tape)
        .expect("save brush walkthrough tape");
}
