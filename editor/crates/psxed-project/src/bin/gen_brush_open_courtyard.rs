//! Generate the tracked roofless BSP template copied by New Project.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use psxed_project::brush::Brush;
use psxed_project::{
    MaterialResource, NodeId, NodeKind, ProjectDocument, ResourceData, Transform3,
};

const FLOOR_SOURCE: &str = "cobbles_1a.psxt";
const WALL_SOURCE: &str = "brick_1a.psxt";
const FLOOR_RELATIVE: &str = "assets/textures/courtyard_cobbles.psxt";
const WALL_RELATIVE: &str = "assets/textures/courtyard_brick.psxt";

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
        .join("brush-open-courtyard");
    let output_dir = match parse_generator_args(std::env::args_os().skip(1), default_output)
        .unwrap_or_else(|error| panic!("{error}"))
    {
        GeneratorAction::Help => {
            println!("Usage: gen-brush-open-courtyard [OUTPUT_DIR]");
            return;
        }
        GeneratorAction::Generate(output_dir) => output_dir,
    };
    generate(&output_dir);
}

fn generate(output_dir: &Path) {
    std::fs::create_dir_all(output_dir).expect("create open courtyard project directory");
    copy_texture(output_dir, FLOOR_SOURCE, FLOOR_RELATIVE);
    copy_texture(output_dir, WALL_SOURCE, WALL_RELATIVE);

    let mut project = ProjectDocument::new("Open Courtyard Starter");
    project.editor_camera.orbit_yaw_q12 = 3584;
    project.editor_camera.orbit_pitch_q12 = 3712;
    project.editor_camera.orbit_target = [2112, 160, 2112];
    project.editor_camera.orbit_radius = 4800;
    let floor = project.add_resource(
        "Courtyard Cobbles",
        ResourceData::Material(MaterialResource::opaque(Some(FLOOR_RELATIVE.to_string()))),
    );
    let walls = project.add_resource(
        "Courtyard Brick",
        ResourceData::Material(MaterialResource::opaque(Some(WALL_RELATIVE.to_string()))),
    );

    let scene = project.active_scene_mut();
    // The four walls enclose an exact 4096 x 4096 usable interior. Their
    // 768-unit height gives a clear blockout silhouette without enclosing the
    // sky. There is deliberately no ceiling brush.
    let mut floor_brush = Brush::cuboid([0, 0, 0], [4224, 64, 4224]);
    paint(&mut floor_brush, floor);
    scene.brushes.push(floor_brush);
    for (mins, maxs) in [
        ([0, 64, 0], [64, 832, 4224]),
        ([4160, 64, 0], [4224, 832, 4224]),
        ([64, 64, 0], [4160, 832, 64]),
        ([64, 64, 4160], [4160, 832, 4224]),
    ] {
        let mut wall = Brush::cuboid(mins, maxs);
        paint(&mut wall, walls);
        scene.brushes.push(wall);
    }

    let spawn = scene.add_node(
        NodeId::ROOT,
        "Player Spawn",
        NodeKind::SpawnPoint {
            player: true,
            character: None,
        },
    );
    scene.node_mut(spawn).expect("spawn node").transform = Transform3 {
        translation: [2112.0, 65.0, 3200.0],
        rotation_degrees: [0.0, 180.0, 0.0],
        ..Transform3::default()
    };

    for (name, translation, color) in [
        (
            "Warm Blockout Light",
            [1408.0, 600.0, 2112.0],
            [255, 196, 144],
        ),
        (
            "Cool Blockout Light",
            [2816.0, 600.0, 2112.0],
            [144, 196, 255],
        ),
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
        .expect("save open courtyard starter");
    let mut source = std::fs::read_to_string(&project_path).expect("read generated courtyard");
    source.push('\n');
    std::fs::write(&project_path, source).expect("finish open courtyard project");
}

fn copy_texture(output_dir: &Path, source_name: &str, destination_relative: &str) {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("projects")
        .join("default")
        .join("assets")
        .join("textures")
        .join(source_name);
    let destination = output_dir.join(destination_relative);
    std::fs::create_dir_all(destination.parent().expect("texture parent"))
        .expect("create texture directory");
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

fn parse_generator_args(
    args: impl IntoIterator<Item = OsString>,
    default_output: PathBuf,
) -> Result<GeneratorAction, String> {
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Ok(GeneratorAction::Generate(default_output));
    };
    if args.next().is_some() {
        return Err("Usage: gen-brush-open-courtyard [OUTPUT_DIR]".to_string());
    }
    if first == "--help" || first == "-h" {
        return Ok(GeneratorAction::Help);
    }
    Ok(GeneratorAction::Generate(PathBuf::from(first)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracked_fixture_is_byte_identical_to_generator_output() {
        let generated = std::env::temp_dir().join(format!(
            "psxed-open-courtyard-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        generate(&generated);
        let tracked = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("projects")
            .join("brush-open-courtyard");
        for relative in [FLOOR_RELATIVE, WALL_RELATIVE, "project.ron"] {
            assert_eq!(
                std::fs::read(generated.join(relative)).unwrap(),
                std::fs::read(tracked.join(relative)).unwrap(),
                "tracked {relative} drifted from generator"
            );
        }
        let _ = std::fs::remove_dir_all(generated);
    }

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
}
