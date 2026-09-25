//! Generate a temporary BSP project used by the headless liquid runtime gate.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use psxed_project::brush::{Brush, BrushContents};
use psxed_project::{
    MaterialResource, NodeId, NodeKind, ProjectDocument, ResourceData, Transform3,
};

const FLOOR_SOURCE: &str = "assets/textures/courtyard_cobbles.psxt";
const WALL_SOURCE: &str = "assets/textures/courtyard_brick.psxt";
/// Authored corridor length along Z.
const ROOM_DEPTH: i32 = 2048;
/// The lava pool runs from the -Z wall to here; everything past it is the
/// dry landing. The characterless player walks -Z under forward input.
const LAVA_END_Z: i32 = 1024;
/// Spawn on the landing, clear of the pool by more than the fallback
/// player hull (Quake's 16-unit radius, 256 authored).
const SPAWN_Z: f32 = 1664.0;
/// Wall top. Above the 640-unit step-up, so forward input cannot climb out.
const WALL_TOP: i32 = 1088;

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
    project.editor_camera.orbit_target = [512, 160, ROOM_DEPTH / 2];
    project.editor_camera.orbit_radius = 2500;
    let floor = project.add_resource(
        "Courtyard Cobbles",
        ResourceData::Material(MaterialResource::opaque(Some(FLOOR_SOURCE.to_string()))),
    );
    let walls = project.add_resource(
        "Courtyard Brick",
        ResourceData::Material(MaterialResource::opaque(Some(WALL_SOURCE.to_string()))),
    );

    let scene = project.active_scene_mut();
    // One corridor, ROOM_DEPTH long in +Z: a dry landing where the player
    // spawns and respawns, then the lava pool up to the far wall.
    let mut floor_brush = Brush::cuboid([0, 0, 0], [1024, 64, ROOM_DEPTH]);
    paint(&mut floor_brush, floor);
    scene.brushes.push(floor_brush);
    for (mins, maxs) in [
        ([0, 64, 0], [64, WALL_TOP, ROOM_DEPTH]),
        ([960, 64, 0], [1024, WALL_TOP, ROOM_DEPTH]),
        ([64, 64, 0], [960, WALL_TOP, 64]),
        ([64, 64, ROOM_DEPTH - 64], [960, WALL_TOP, ROOM_DEPTH]),
    ] {
        let mut wall = Brush::cuboid(mins, maxs);
        paint(&mut wall, walls);
        scene.brushes.push(wall);
    }

    // The pool fills the -Z half of the corridor up to the wall, so a
    // player walked forward into it stays inside once the input stops and
    // all three ordered contents samples report lava. The volume is
    // deliberately nonblocking. The spawn stands on the dry landing: the
    // respawn after the lava death must leave the player out of the pool,
    // so the gate proves exactly one death rather than a death loop.
    let mut lava = Brush::cuboid([128, 64, 64], [896, 384, LAVA_END_Z]);
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
        translation: [512.0, 65.0, SPAWN_Z],
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
        .join("archive")
        .join("fixtures")
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
    fn generated_project_spawns_on_dry_land_facing_a_nonblocking_lava_pool() {
        let generated = std::env::temp_dir().join(format!(
            "psxed-liquid-runtime-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        generate(&generated);
        let mut project = ProjectDocument::load_from_path(generated.join("project.ron")).unwrap();
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

        psxed_project::units::scale_project_to_engine_units(&mut project);
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
        let engine = |authored: i32| authored / psxed_project::units::WORLD_UNIT_DIVISOR * 4096;
        let spawn_z = engine(SPAWN_Z as i32);
        let pool_z = engine((64 + LAVA_END_Z) / 2);
        for y in [5, 6, 7] {
            let sample = |z| {
                hull.point_contents(Vec3I32 {
                    x: 32 * 4096,
                    y: y * 4096,
                    z,
                })
            };
            assert_ne!(
                sample(spawn_z),
                Some(psx_bsp::collision::CONTENTS_LAVA),
                "spawn sample y={y} must be dry: the respawn lands here"
            );
            assert_eq!(
                sample(pool_z),
                Some(psx_bsp::collision::CONTENTS_LAVA),
                "pool sample y={y}"
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
