//! Generate a temporary BSP project used by the headless liquid runtime gate.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use psxed_project::brush::{Brush, BrushContents};
use psxed_project::{
    MaterialResource, NodeId, NodeKind, ProjectDocument, ResourceData, Transform3,
};

const FLOOR_SOURCE: &str = "assets/textures/courtyard_cobbles.psxt";
const WALL_SOURCE: &str = "assets/textures/courtyard_brick.psxt";

#[derive(Debug, PartialEq, Eq)]
enum GeneratorAction {
    Help,
    Generate(PathBuf),
}

fn main() {
    let default_output = std::env::temp_dir().join("psoxide-brush-liquid-runtime");
    let output_dir = match parse_generator_args(std::env::args_os().skip(1), default_output)
        .unwrap_or_else(|error| panic!("{error}"))
    {
        GeneratorAction::Help => {
            println!("Usage: gen-brush-liquid-runtime [OUTPUT_DIR]");
            return;
        }
        GeneratorAction::Generate(output_dir) => output_dir,
    };
    generate(&output_dir);
}

fn generate(output_dir: &Path) {
    std::fs::create_dir_all(output_dir).expect("create liquid runtime project directory");
    copy_texture(output_dir, FLOOR_SOURCE);
    copy_texture(output_dir, WALL_SOURCE);

    let mut project = ProjectDocument::new("BSP Liquid Runtime Proof");
    project.editor_camera.orbit_target = [512, 160, 512];
    project.editor_camera.orbit_radius = 1500;
    let floor = project.add_resource(
        "Courtyard Cobbles",
        ResourceData::Material(MaterialResource::opaque(Some(FLOOR_SOURCE.to_string()))),
    );
    let walls = project.add_resource(
        "Courtyard Brick",
        ResourceData::Material(MaterialResource::opaque(Some(WALL_SOURCE.to_string()))),
    );

    let scene = project.active_scene_mut();
    let mut floor_brush = Brush::cuboid([0, 0, 0], [1024, 64, 1024]);
    paint(&mut floor_brush, floor);
    scene.brushes.push(floor_brush);
    for (mins, maxs) in [
        ([0, 64, 0], [64, 512, 1024]),
        ([960, 64, 0], [1024, 512, 1024]),
        ([64, 64, 0], [960, 512, 64]),
        ([64, 64, 960], [960, 512, 1024]),
    ] {
        let mut wall = Brush::cuboid(mins, maxs);
        paint(&mut wall, walls);
        scene.brushes.push(wall);
    }

    // Enclose the full fallback player hull at spawn so all three ordered
    // contents samples report lava. This volume is deliberately nonblocking.
    let mut lava = Brush::cuboid([128, 64, 128], [896, 384, 896]);
    lava.contents = BrushContents::Lava;
    paint(&mut lava, floor);
    scene.brushes.push(lava);

    let spawn = scene.add_node(
        NodeId::ROOT,
        "Player Spawn",
        NodeKind::SpawnPoint {
            player: true,
            character: None,
        },
    );
    scene.node_mut(spawn).expect("spawn node").transform = Transform3 {
        translation: [512.0, 65.0, 512.0],
        ..Transform3::default()
    };

    let project_path = output_dir.join("project.ron");
    project
        .save_to_path(&project_path)
        .expect("save liquid runtime project");
}

fn copy_texture(output_dir: &Path, relative: &str) {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("projects")
        .join("brush-open-courtyard")
        .join(relative);
    let destination = output_dir.join(relative);
    std::fs::create_dir_all(destination.parent().expect("texture parent"))
        .expect("create texture directory");
    std::fs::copy(&source, &destination).unwrap_or_else(|error| {
        panic!(
            "copy liquid proof texture {} -> {}: {error}",
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
        return Err("Usage: gen-brush-liquid-runtime [OUTPUT_DIR]".to_string());
    }
    if first == "--help" || first == "-h" {
        return Ok(GeneratorAction::Help);
    }
    Ok(GeneratorAction::Generate(PathBuf::from(first)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_bsp::pxbsp_resident::PxbspResidentMap;
    use psx_bsp::{SliceReader, Vec3I32};
    use psxed_project::brush_world::{
        compile_brush_world, BrushWorldCookMode, BrushWorldCookOptions,
    };

    #[test]
    fn generated_project_has_one_nonblocking_lava_volume_around_spawn() {
        let generated = std::env::temp_dir().join(format!(
            "psxed-liquid-runtime-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        generate(&generated);
        let project = ProjectDocument::load_from_path(&generated.join("project.ron")).unwrap();
        let scene = project.active_scene();
        assert_eq!(scene.brushes.len(), 6);
        assert_eq!(
            scene
                .brushes
                .iter()
                .filter(|brush| brush.contents == BrushContents::Lava)
                .count(),
            1
        );
        assert_eq!(
            scene
                .brushes
                .iter()
                .filter(|brush| brush.contents == BrushContents::Solid)
                .count(),
            5
        );

        let compiled = compile_brush_world(
            &project,
            BrushWorldCookOptions {
                project_root: &generated,
                mode: BrushWorldCookMode::Draft,
                ambient: [32; 3],
                texture_asset_base: 0,
            },
        )
        .expect("compile generated liquid proof");
        let mut map = PxbspResidentMap::with_capacity(compiled.pxbsp.bytes.len());
        map.load(0, &mut SliceReader::new(&compiled.pxbsp.bytes))
            .expect("load generated PXBSP");
        let hull = map.model_collision_hull(0, 0).expect("point hull");
        for y in [66, 93, 120] {
            assert_eq!(
                hull.point_contents(Vec3I32 {
                    x: 512 * 4096,
                    y: y * 4096,
                    z: 512 * 4096,
                }),
                Some(psx_bsp::collision::CONTENTS_LAVA),
                "spawn sample y={y}"
            );
        }
        let _ = std::fs::remove_dir_all(generated);
    }

    #[test]
    fn help_is_never_treated_as_an_output_directory() {
        let default = PathBuf::from("temporary-fixture");
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
