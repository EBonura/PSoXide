//! Generate the tracked two-room brush Play acceptance map.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use psxed_project::brush::Brush;
use psxed_project::{
    LogicNodeKind, MaterialResource, NodeId, NodeKind, ProjectDocument, ResourceData, Transform3,
};

const STARTER_STONE_SOURCE: &str = "delven_07_stonebrk4a_q0.psxt";
const STARTER_STONE_RELATIVE: &str = "assets/textures/starter_stone_brick.psxt";

#[derive(Debug, PartialEq, Eq)]
enum GeneratorAction {
    Help,
    Generate(PathBuf),
}

fn main() {
    let default_output = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("projects")
        .join("brush-first-playable");
    let output_dir = match parse_generator_args(std::env::args_os().skip(1), default_output)
        .unwrap_or_else(|error| panic!("{error}"))
    {
        GeneratorAction::Help => {
            println!("Usage: gen-brush-first-playable [OUTPUT_DIR]");
            return;
        }
        GeneratorAction::Generate(output_dir) => output_dir,
    };
    std::fs::create_dir_all(&output_dir).expect("create brush project directory");

    let mut project = ProjectDocument::new("Brush First Playable");
    // Start inside the left room, looking down toward the door so the first
    // view includes the floor, both side-wall strips, and the closed door.
    // This is deliberately authored rather than bounds-framed: an exterior
    // fit hides the editable interior behind the ceiling and outer walls.
    project.editor_camera.orbit_yaw_q12 = 3072;
    project.editor_camera.orbit_pitch_q12 = 3665;
    project.editor_camera.orbit_target = [512, 64, 384];
    project.editor_camera.orbit_radius = 550;
    copy_starter_stone(&output_dir);
    let stone_material = MaterialResource::opaque(Some(STARTER_STONE_RELATIVE.to_string()));
    let stone = project.add_resource(
        "Starter Stone Brick",
        ResourceData::Material(stone_material),
    );
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

fn parse_generator_args(
    args: impl IntoIterator<Item = OsString>,
    default_output: PathBuf,
) -> Result<GeneratorAction, String> {
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Ok(GeneratorAction::Generate(default_output));
    };
    if args.next().is_some() {
        return Err("Usage: gen-brush-first-playable [OUTPUT_DIR]".to_string());
    }
    if first == "--help" || first == "-h" {
        return Ok(GeneratorAction::Help);
    }
    Ok(GeneratorAction::Generate(PathBuf::from(first)))
}

fn copy_starter_stone(output_dir: &Path) {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("projects")
        .join("default")
        .join("assets")
        .join("textures")
        .join(STARTER_STONE_SOURCE);
    let destination = output_dir.join(STARTER_STONE_RELATIVE);
    std::fs::create_dir_all(destination.parent().expect("starter texture parent"))
        .expect("create starter texture directory");
    std::fs::copy(&source, &destination).unwrap_or_else(|error| {
        panic!(
            "copy starter texture {} -> {}: {error}",
            source.display(),
            destination.display()
        )
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_is_never_treated_as_an_output_directory() {
        let default = PathBuf::from("tracked-fixture");
        assert_eq!(
            parse_generator_args([OsString::from("--help")], default.clone()).unwrap(),
            GeneratorAction::Help
        );
        assert_eq!(
            parse_generator_args([OsString::from("-h")], default).unwrap(),
            GeneratorAction::Help
        );
    }

    #[test]
    fn output_directory_is_optional_and_unique() {
        let default = PathBuf::from("tracked-fixture");
        assert_eq!(
            parse_generator_args([], default.clone()).unwrap(),
            GeneratorAction::Generate(default)
        );
        assert_eq!(
            parse_generator_args([OsString::from("generated")], PathBuf::new()).unwrap(),
            GeneratorAction::Generate(PathBuf::from("generated"))
        );
        assert!(parse_generator_args(
            [OsString::from("one"), OsString::from("two")],
            PathBuf::new()
        )
        .is_err());
    }
}
