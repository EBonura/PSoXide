//! Generate the tracked Quake-style layered-sky aperture fixture.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use image::imageops::FilterType;
use psxed_format::texture::Depth;
use psxed_project::brush::Brush;
use psxed_project::{
    MaterialResource, NodeId, NodeKind, ProjectDocument, ResourceData, SkyMode, SkyVisibility,
    Transform3,
};

const FLOOR_SOURCE: &str = "courtyard_cobbles.psxt";
const WALL_SOURCE: &str = "courtyard_brick.psxt";
const FLOOR_RELATIVE: &str = "assets/textures/courtyard_cobbles.psxt";
const WALL_RELATIVE: &str = "assets/textures/courtyard_brick.psxt";
const SKY_RELATIVE: &str = "assets/textures/layered_sky.psxt";
const SKY_SOURCE: &[u8] = include_bytes!("../../assets/sky/cortex_sunset_clouds_v2.png");

#[derive(Debug, PartialEq, Eq)]
enum GeneratorAction {
    Help,
    Generate(PathBuf),
}

fn main() {
    let default_output = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("archive")
        .join("fixtures")
        .join("brush-layered-sky");
    let output_dir = match parse_generator_args(std::env::args_os().skip(1), default_output)
        .unwrap_or_else(|error| panic!("{error}"))
    {
        GeneratorAction::Help => {
            println!("Usage: gen-brush-layered-sky [OUTPUT_DIR]");
            return;
        }
        GeneratorAction::Generate(output_dir) => output_dir,
    };
    generate(&output_dir);
}

fn generate(output_dir: &Path) {
    std::fs::create_dir_all(output_dir).expect("create layered-sky fixture directory");
    copy_texture(output_dir, FLOOR_SOURCE, FLOOR_RELATIVE);
    copy_texture(output_dir, WALL_SOURCE, WALL_RELATIVE);
    write_layered_sky_texture(&output_dir.join(SKY_RELATIVE));

    let mut project = ProjectDocument::new("Layered Sky Aperture");
    project.editor_camera.orbit_yaw_q12 = 3584;
    project.editor_camera.orbit_pitch_q12 = 3584;
    project.editor_camera.orbit_target = [4096, 640, 4096];
    project.editor_camera.orbit_radius = 11_000;

    let floor = project.add_resource(
        "Courtyard Cobbles",
        ResourceData::Material(MaterialResource::opaque(Some(FLOOR_RELATIVE.to_string()))),
    );
    let walls = project.add_resource(
        "Courtyard Brick",
        ResourceData::Material(MaterialResource::opaque(Some(WALL_RELATIVE.to_string()))),
    );
    let mut layered_sky = MaterialResource::opaque(Some(SKY_RELATIVE.to_string()));
    layered_sky.sky_aperture = true;
    let sky = project.add_resource("Layered Sky", ResourceData::Material(layered_sky));

    let scene = project.active_scene_mut();
    let NodeKind::World { sky: world_sky, .. } =
        &mut scene.node_mut(NodeId::ROOT).expect("world root").kind
    else {
        panic!("fixture root must be a world");
    };
    world_sky.mode = SkyMode::QuakeLayered;
    world_sky.visibility = SkyVisibility::ThroughSkySurfaces;
    world_sky.texture = Some(sky);

    let outer = 8192;
    let wall = 128;
    let roof_y = 1536;
    let mut floor_brush = Brush::cuboid([0, 0, 0], [outer, wall, outer]);
    paint(&mut floor_brush, floor);
    scene.brushes.push(floor_brush);
    for (mins, maxs, material) in [
        ([0, wall, 0], [wall, roof_y, outer], walls),
        ([outer - wall, wall, 0], [outer, roof_y, outer], walls),
        ([wall, wall, 0], [outer - wall, roof_y, wall], sky),
        (
            [wall, wall, outer - wall],
            [outer - wall, roof_y, outer],
            sky,
        ),
    ] {
        let mut brush = Brush::cuboid(mins, maxs);
        paint(&mut brush, material);
        scene.brushes.push(brush);
    }

    // Sky brushes remain solid collision. Their visible faces emit no world
    // polygon and instead select the bounded camera-relative sky lattice.
    let mut roof = Brush::cuboid(
        [wall, roof_y, wall],
        [outer - wall, roof_y + wall, outer - wall],
    );
    paint(&mut roof, sky);
    scene.brushes.push(roof);

    let spawn = scene.add_node(
        NodeId::ROOT,
        "Player Spawn",
        NodeKind::SpawnPoint {
            player: true,
            character: None,
        },
    );
    scene.node_mut(spawn).expect("spawn node").transform = Transform3 {
        translation: [4096.0, 193.0, 4096.0],
        rotation_degrees: [0.0, 180.0, 0.0],
        ..Transform3::default()
    };

    let project_path = output_dir.join("project.ron");
    project
        .save_to_path(&project_path)
        .expect("save layered-sky fixture");
    let mut source = std::fs::read_to_string(&project_path).expect("read generated fixture");
    source.push('\n');
    std::fs::write(&project_path, source).expect("finish layered-sky fixture");
}

fn write_layered_sky_texture(destination: &Path) {
    const LAYER: usize = 128;
    const WIDTH: usize = LAYER * 2;
    const HEIGHT: usize = LAYER;
    let source = image::load_from_memory(SKY_SOURCE)
        .expect("decode authored layered-sky source")
        .to_rgba8();
    let tile = image::imageops::resize(&source, LAYER as u32, LAYER as u32, FilterType::Lanczos3);

    // Quake's atlas is two adjacent square layers. The right half is the
    // opaque, slowly scrolling cloud field. The left half is a faster masked
    // cloud layer; index zero reveals the background. Derive that mask from
    // the darkest 42% of an offset copy so both layers share the same authored
    // colour language without visibly tracking one another.
    let mut luminance = tile
        .pixels()
        .map(|pixel| rgb_luminance([pixel[0], pixel[1], pixel[2]]))
        .collect::<Vec<_>>();
    luminance.sort_unstable();
    let mask_cutoff = luminance[luminance.len() * 42 / 100];
    let mut rgba = vec![[0u8; 4]; WIDTH * HEIGHT];
    for y in 0..HEIGHT {
        for x in 0..LAYER {
            let background = tile.get_pixel(x as u32, y as u32);
            rgba[y * WIDTH + LAYER + x] = [background[0], background[1], background[2], 255];

            let foreground = tile.get_pixel(((x + 19) % LAYER) as u32, ((y + 11) % LAYER) as u32);
            let rgb = [foreground[0], foreground[1], foreground[2]];
            let alpha = if rgb_luminance(rgb) <= mask_cutoff {
                255
            } else {
                0
            };
            rgba[y * WIDTH + x] = [
                rgb[0].saturating_mul(3) / 4,
                rgb[1].saturating_mul(3) / 4,
                rgb[2].saturating_mul(7) / 8,
                alpha,
            ];
        }
    }
    let (palette, pixels) = psxed_tex::quantize_rgba_with_transparent_zero(&rgba, 16)
        .expect("quantize authored layered sky");
    let bytes = psxed_tex::encode_indexed_psxt(
        WIDTH as u16,
        HEIGHT as u16,
        Depth::Bit4,
        &pixels,
        &palette,
        true,
    )
    .expect("encode layered-sky PSXT");
    std::fs::create_dir_all(destination.parent().expect("sky texture parent"))
        .expect("create sky texture directory");
    std::fs::write(destination, bytes).expect("write layered-sky PSXT");
}

fn rgb_luminance(rgb: [u8; 3]) -> u16 {
    (u16::from(rgb[0]) * 54 + u16::from(rgb[1]) * 183 + u16::from(rgb[2]) * 19) >> 8
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
            "copy fixture texture {} -> {}: {error}",
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
        return Err("Usage: gen-brush-layered-sky [OUTPUT_DIR]".to_string());
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
            "psxed-layered-sky-{}-{}",
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
            .join("archive")
            .join("fixtures")
            .join("brush-layered-sky");
        for relative in [FLOOR_RELATIVE, WALL_RELATIVE, SKY_RELATIVE, "project.ron"] {
            assert_eq!(
                std::fs::read(generated.join(relative)).unwrap(),
                std::fs::read(tracked.join(relative)).unwrap(),
                "tracked {relative} drifted from generator"
            );
        }
        let _ = std::fs::remove_dir_all(generated);
    }

    #[test]
    fn sky_atlas_preserves_a_masked_foreground_and_opaque_background() {
        let generated =
            std::env::temp_dir().join(format!("psxed-layered-sky-texture-{}", std::process::id()));
        write_layered_sky_texture(&generated);
        let bytes = std::fs::read(&generated).unwrap();
        let texture = psx_asset::Texture::from_bytes(&bytes).unwrap();
        assert_eq!([texture.width(), texture.height()], [256, 128]);
        assert_eq!(texture.depth(), Depth::Bit4);
        assert!(texture.index_zero_transparent());
        let pixels = texture.pixel_bytes();
        assert!(pixels.iter().any(|byte| byte & 0x0f == 0));
        let _ = std::fs::remove_file(generated);
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
