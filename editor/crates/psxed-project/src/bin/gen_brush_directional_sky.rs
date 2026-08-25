//! Generate the tracked six-face directional-sky validation fixture.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use image::RgbImage;
use psxed_format::texture::Depth;
use psxed_project::brush::Brush;
use psxed_project::{
    MaterialResource, NodeId, NodeKind, ProjectDocument, ResourceData, SkyMode, SkyVisibility,
    Transform3,
};

const FLOOR_SOURCE: &str = "courtyard_cobbles.psxt";
const FLOOR_RELATIVE: &str = "assets/textures/courtyard_cobbles.psxt";
const SKY_RELATIVE: &str = "assets/textures/directional_sunset.psxt";
const YAW_TAPE_RELATIVE: &str = "yaw-sweep.pxitape.csv";
const PITCH_TAPE_RELATIVE: &str = "pitch-sweep.pxitape.csv";
const SKY_ONLY_PROJECT_RELATIVE: &str = "sky-only.ron";
const SKY_SOURCE: &[u8] = include_bytes!("../../assets/sky/cortex_sunset_equirect_v1.png");
const FACE_WIDTH: usize = psx_bsp::sky::CUBE_SKY_FACE_WIDTH as usize;
const FACE_HEIGHT: usize = psx_bsp::sky::CUBE_SKY_FACE_HEIGHT as usize;
const ATLAS_WIDTH: usize = psx_bsp::sky::CUBE_SKY_ATLAS_SIZE[0] as usize;
const ATLAS_HEIGHT: usize = psx_bsp::sky::CUBE_SKY_ATLAS_SIZE[1] as usize;
const FACE_COUNT: usize = 6;
const FACE_PALETTE_COLORS: usize = 16;
const SOURCE_HORIZON_V: f32 = 0.64;

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
        .join("brush-directional-sky");
    let output_dir = match parse_generator_args(std::env::args_os().skip(1), default_output)
        .unwrap_or_else(|error| panic!("{error}"))
    {
        GeneratorAction::Help => {
            println!("Usage: gen-brush-directional-sky [OUTPUT_DIR]");
            return;
        }
        GeneratorAction::Generate(output_dir) => output_dir,
    };
    generate(&output_dir);
}

fn generate(output_dir: &Path) {
    std::fs::create_dir_all(output_dir).expect("create directional-sky fixture directory");
    copy_texture(output_dir, FLOOR_SOURCE, FLOOR_RELATIVE);
    write_directional_sky_texture(&output_dir.join(SKY_RELATIVE));
    write_yaw_sweep_tape(&output_dir.join(YAW_TAPE_RELATIVE));
    write_pitch_sweep_tape(&output_dir.join(PITCH_TAPE_RELATIVE));

    let mut project = ProjectDocument::new("Directional Cube Sky");
    project.editor_camera.orbit_yaw_q12 = 3584;
    project.editor_camera.orbit_pitch_q12 = 3584;
    project.editor_camera.orbit_target = [4096, 640, 4096];
    project.editor_camera.orbit_radius = 11_000;

    let floor = project.add_resource(
        "Courtyard Cobbles",
        ResourceData::Material(MaterialResource::opaque(Some(FLOOR_RELATIVE.to_string()))),
    );
    let mut directional_sky = MaterialResource::opaque(Some(SKY_RELATIVE.to_string()));
    directional_sky.sky_aperture = true;
    let sky = project.add_resource(
        "Directional Sunset Sky",
        ResourceData::Material(directional_sky),
    );

    let scene = project.active_scene_mut();
    let NodeKind::World { sky: world_sky, .. } =
        &mut scene.node_mut(NodeId::ROOT).expect("world root").kind
    else {
        panic!("fixture root must be a world");
    };
    world_sky.mode = SkyMode::Cube;
    world_sky.visibility = SkyVisibility::ThroughSkySurfaces;
    world_sky.texture = Some(sky);

    let outer = 8192;
    let wall = 128;
    let roof_y = 1536;
    let mut floor_brush = Brush::cuboid([0, 0, 0], [outer, wall, outer]);
    paint(&mut floor_brush, floor);
    scene.brushes.push(floor_brush);

    // Every interior wall and the roof are sky apertures. The player remains
    // in a sealed collision hull while a camera turn can expose every cube
    // direction without depending on an imported game project.
    for (mins, maxs) in [
        ([0, wall, 0], [wall, roof_y, outer]),
        ([outer - wall, wall, 0], [outer, roof_y, outer]),
        ([wall, wall, 0], [outer - wall, roof_y, wall]),
        ([wall, wall, outer - wall], [outer - wall, roof_y, outer]),
        (
            [wall, roof_y, wall],
            [outer - wall, roof_y + wall, outer - wall],
        ),
    ] {
        let mut brush = Brush::cuboid(mins, maxs);
        paint(&mut brush, sky);
        scene.brushes.push(brush);
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
        translation: [4096.0, 193.0, 4096.0],
        rotation_degrees: [0.0, 180.0, 0.0],
        ..Transform3::default()
    };

    save_project(&project, &output_dir.join("project.ron"));

    // A second cook uses the same sealed hull with its floor marked as sky.
    // It proves that the cube pass writes the entire display when no opaque
    // level polygon is visible, without weakening the main occlusion fixture.
    paint(
        project
            .active_scene_mut()
            .brushes
            .first_mut()
            .expect("floor brush"),
        sky,
    );
    save_project(&project, &output_dir.join(SKY_ONLY_PROJECT_RELATIVE));
}

fn save_project(project: &ProjectDocument, destination: &Path) {
    project
        .save_to_path(destination)
        .expect("save directional-sky fixture");
    let mut source = std::fs::read_to_string(destination).expect("read generated fixture");
    source.push('\n');
    std::fs::write(destination, source).expect("finish directional-sky fixture");
}

fn write_directional_sky_texture(destination: &Path) {
    let source = image::load_from_memory(SKY_SOURCE)
        .expect("decode authored directional-sky source")
        .to_rgb8();
    let faces = (0..FACE_COUNT)
        .map(|face| sample_cube_face(&source, face))
        .collect::<Vec<_>>();

    // One authored palette is duplicated into all six CLUT slots. Keeping the
    // palette identical lets reconciled edge indices remain identical across
    // face boundaries. Avoiding ordered dither also prevents a pattern from
    // restarting on every face and revealing those boundaries in motion.
    let palette_source = faces
        .iter()
        .flat_map(|face| face.iter().copied())
        .collect::<Vec<_>>();
    let (mut sky_palette, _) = psxed_tex::quantize_rgb(&palette_source, FACE_PALETTE_COLORS)
        .expect("quantize authored sky palette");
    pad_palette(&mut sky_palette, FACE_PALETTE_COLORS);

    let mut pixels = vec![0u8; ATLAS_WIDTH * ATLAS_HEIGHT];
    let mut palette_rows = Vec::with_capacity(FACE_COUNT);
    for (face, face_pixels) in faces.iter().enumerate() {
        for y in 0..FACE_HEIGHT {
            let row = y * ATLAS_WIDTH;
            for x in 0..FACE_WIDTH {
                let source = face_pixels[y * FACE_WIDTH + x];
                pixels[row + face * FACE_WIDTH + x] = nearest_palette(source, &sky_palette);
            }
        }
        palette_rows.push(sky_palette.clone());
    }
    reconcile_polar_face_edges(&mut pixels);

    let bytes = psxed_tex::encode_indexed_psxt_with_clut_rows(
        ATLAS_WIDTH as u16,
        ATLAS_HEIGHT as u16,
        Depth::Bit4,
        &pixels,
        &palette_rows,
        false,
    )
    .expect("encode directional-sky PSXT");
    std::fs::create_dir_all(destination.parent().expect("sky texture parent"))
        .expect("create sky texture directory");
    std::fs::write(destination, bytes).expect("write directional-sky PSXT");
}

fn sample_cube_face(source: &RgbImage, face: usize) -> Vec<[u8; 3]> {
    let mut pixels = Vec::with_capacity(FACE_WIDTH * FACE_HEIGHT);
    for y in 0..FACE_HEIGHT {
        let v = y as f32 * 2.0 / (FACE_HEIGHT - 1) as f32 - 1.0;
        for x in 0..FACE_WIDTH {
            let u = x as f32 * 2.0 / (FACE_WIDTH - 1) as f32 - 1.0;
            pixels.push(sample_equirectangular(source, cube_direction(face, u, v)));
        }
    }
    pixels
}

fn cube_direction(face: usize, u: f32, v: f32) -> [f32; 3] {
    match face {
        0 => [1.0, -v, -u],
        1 => [-1.0, -v, u],
        2 => [u, 1.0, v],
        3 => [u, -1.0, -v],
        4 => [u, -v, 1.0],
        5 => [-u, -v, -1.0],
        _ => unreachable!("cube face index"),
    }
}

fn sample_equirectangular(source: &RgbImage, direction: [f32; 3]) -> [u8; 3] {
    let length =
        (direction[0] * direction[0] + direction[1] * direction[1] + direction[2] * direction[2])
            .sqrt()
            .max(f32::EPSILON);
    let longitude = direction[0].atan2(direction[2]);
    let latitude = (direction[1] / length).clamp(-1.0, 1.0).asin();
    let width = source.width() as f32;
    let height = source.height() as f32;
    let source_x = (longitude / std::f32::consts::TAU + 0.5) * width - 0.5;
    let environment_v = 0.5 - latitude / std::f32::consts::PI;
    let source_v = if environment_v <= 0.5 {
        environment_v * SOURCE_HORIZON_V * 2.0
    } else {
        SOURCE_HORIZON_V + (environment_v - 0.5) * (1.0 - SOURCE_HORIZON_V) * 2.0
    };
    let source_y = source_v * height - 0.5;
    let sampled = bilinear_wrapped(source, source_x, source_y);
    let horizon_weight = (1.0 - (environment_v - 0.5).abs() / 0.035).clamp(0.0, 1.0);
    let mut color = blend_rgb(sampled, [52, 38, 58], horizon_weight);

    // Preserve a readable fixed sun after the source is reduced to a
    // 16-colour face palette. The direction matches the authored
    // source sun, so this reinforces it rather than adding a second light.
    let normal = direction.map(|component| component / length);
    let sun_direction = [-0.65f32, 0.25, 0.72];
    let sun_dot =
        normal[0] * sun_direction[0] + normal[1] * sun_direction[1] + normal[2] * sun_direction[2];
    let glow = ((sun_dot - 0.995) / 0.004).clamp(0.0, 1.0) * 0.55;
    color = blend_rgb(color, [255, 176, 116], glow);
    let core = ((sun_dot - 0.999) / 0.0007).clamp(0.0, 1.0);
    blend_rgb(color, [255, 238, 204], core)
}

fn bilinear_wrapped(source: &RgbImage, x: f32, y: f32) -> [u8; 3] {
    let width = source.width() as i32;
    let height = source.height() as i32;
    let x0 = x.floor() as i32;
    let y0 = (y.floor() as i32).clamp(0, height - 1);
    let x1 = x0 + 1;
    let y1 = (y0 + 1).clamp(0, height - 1);
    let fx = x - x.floor();
    let fy = y - y.floor();
    let wrap_x = |value: i32| value.rem_euclid(width) as u32;
    let sample = |sx: i32, sy: i32| source.get_pixel(wrap_x(sx), sy as u32).0;
    let p00 = sample(x0, y0);
    let p10 = sample(x1, y0);
    let p01 = sample(x0, y1);
    let p11 = sample(x1, y1);
    let mut output = [0u8; 3];
    for channel in 0..3 {
        let top = p00[channel] as f32 * (1.0 - fx) + p10[channel] as f32 * fx;
        let bottom = p01[channel] as f32 * (1.0 - fx) + p11[channel] as f32 * fx;
        output[channel] = (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8;
    }
    output
}

fn blend_rgb(a: [u8; 3], b: [u8; 3], weight: f32) -> [u8; 3] {
    let weight = weight.clamp(0.0, 1.0);
    core::array::from_fn(|channel| {
        (a[channel] as f32 * (1.0 - weight) + b[channel] as f32 * weight)
            .round()
            .clamp(0.0, 255.0) as u8
    })
}

fn pad_palette(palette: &mut Vec<[u8; 3]>, length: usize) {
    let fill = palette.last().copied().unwrap_or([0, 0, 0]);
    palette.resize(length, fill);
}

fn nearest_palette(rgb: [u8; 3], palette: &[[u8; 3]]) -> u8 {
    let mut best_index = 0usize;
    let mut best_distance = u32::MAX;
    for (index, color) in palette.iter().enumerate() {
        let dr = i32::from(rgb[0]) - i32::from(color[0]);
        let dg = i32::from(rgb[1]) - i32::from(color[1]);
        let db = i32::from(rgb[2]) - i32::from(color[2]);
        let distance = (dr * dr + dg * dg + db * db) as u32;
        if distance < best_distance {
            best_index = index;
            best_distance = distance;
        }
    }
    best_index as u8
}

fn atlas_index(face: usize, x: usize, y: usize) -> usize {
    y * ATLAS_WIDTH + face * FACE_WIDTH + x
}

fn reconcile_polar_face_edges(pixels: &mut [u8]) {
    // At a polar/side join, copy the matching side texel into the polar edge so
    // nearest-neighbour samples cannot quantise to different palette indices.
    for y in 0..FACE_HEIGHT {
        let side_x = (y * (FACE_WIDTH - 1) + (FACE_HEIGHT - 1) / 2) / (FACE_HEIGHT - 1);
        let reversed = FACE_WIDTH - 1 - side_x;
        for (to, from) in [
            ((2, 0, y), (1, side_x, 0)),
            ((2, FACE_WIDTH - 1, y), (0, reversed, 0)),
            ((3, 0, y), (1, reversed, FACE_HEIGHT - 1)),
            ((3, FACE_WIDTH - 1, y), (0, side_x, FACE_HEIGHT - 1)),
        ] {
            pixels[atlas_index(to.0, to.1, to.2)] = pixels[atlas_index(from.0, from.1, from.2)];
        }
    }
}

fn write_yaw_sweep_tape(destination: &Path) {
    let mut tape = std::fs::File::create(destination).expect("create directional-sky yaw tape");
    writeln!(tape, "psoxide-tape,v2,clock=video_frame,start_poll=0").unwrap();
    writeln!(tape, "frame,buttons,right_x,right_y,left_x,left_y").unwrap();
    for frame in 0..440u16 {
        // A moderate stick value makes one complete turn after the boot
        // settles. Slow face-boundary crossings reveal seam shimmer.
        let right_x = if (40..400).contains(&frame) { 176 } else { 128 };
        writeln!(tape, "{frame},0,{right_x},128,128,128").unwrap();
    }
}

fn write_pitch_sweep_tape(destination: &Path) {
    let mut tape = std::fs::File::create(destination).expect("create directional-sky pitch tape");
    writeln!(tape, "psoxide-tape,v2,clock=video_frame,start_poll=0").unwrap();
    writeln!(tape, "frame,buttons,right_x,right_y,left_x,left_y").unwrap();
    for frame in 0..440u16 {
        // Exercise both polar faces slowly enough to inspect their joins with
        // all four lateral faces at deterministic visual checkpoints.
        let right_y = if (40..170).contains(&frame) {
            80
        } else if (220..400).contains(&frame) {
            176
        } else {
            128
        };
        writeln!(tape, "{frame},0,128,{right_y},128,128").unwrap();
    }
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
        return Err("Usage: gen-brush-directional-sky [OUTPUT_DIR]".to_string());
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
            "psxed-directional-sky-{}-{}",
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
            .join("brush-directional-sky");
        for relative in [
            FLOOR_RELATIVE,
            SKY_RELATIVE,
            YAW_TAPE_RELATIVE,
            PITCH_TAPE_RELATIVE,
            SKY_ONLY_PROJECT_RELATIVE,
            "project.ron",
        ] {
            assert_eq!(
                std::fs::read(generated.join(relative)).unwrap(),
                std::fs::read(tracked.join(relative)).unwrap(),
                "tracked {relative} drifted from generator"
            );
        }
        let _ = std::fs::remove_dir_all(generated);
    }

    #[test]
    fn scenic_atlas_has_six_opaque_face_palettes() {
        let generated = std::env::temp_dir().join(format!(
            "psxed-directional-sky-texture-{}",
            std::process::id()
        ));
        write_directional_sky_texture(&generated);
        let bytes = std::fs::read(&generated).unwrap();
        let texture = psx_asset::Texture::from_bytes(&bytes).unwrap();
        assert_eq!(
            [texture.width(), texture.height()],
            [ATLAS_WIDTH as u16, ATLAS_HEIGHT as u16]
        );
        assert_eq!(texture.depth(), Depth::Bit4);
        assert_eq!(texture.clut_entries(), 96);
        assert_eq!(texture.clut_bytes().len(), 96 * 2);
        assert!(!texture.index_zero_transparent());
        for face in 1..FACE_COUNT {
            assert_eq!(
                &texture.clut_bytes()[..FACE_PALETTE_COLORS * 2],
                &texture.clut_bytes()
                    [face * FACE_PALETTE_COLORS * 2..(face + 1) * FACE_PALETTE_COLORS * 2]
            );
        }
        let index_at = |face: usize, x: usize, y: usize| {
            let atlas_x = face * FACE_WIDTH + x;
            let atlas_y = y;
            let byte = texture.pixel_bytes()[atlas_y * (ATLAS_WIDTH / 2) + atlas_x / 2];
            if atlas_x & 1 == 0 {
                byte & 0x0f
            } else {
                byte >> 4
            }
        };
        for y in 0..FACE_HEIGHT {
            assert_eq!(index_at(4, FACE_WIDTH - 1, y), index_at(0, 0, y));
            assert_eq!(index_at(0, FACE_WIDTH - 1, y), index_at(5, 0, y));
            assert_eq!(index_at(5, FACE_WIDTH - 1, y), index_at(1, 0, y));
            assert_eq!(index_at(1, FACE_WIDTH - 1, y), index_at(4, 0, y));
        }
        for x in 0..FACE_WIDTH {
            let reversed = FACE_WIDTH - 1 - x;
            assert_eq!(index_at(2, x, 0), index_at(5, reversed, 0));
            assert_eq!(index_at(2, x, FACE_HEIGHT - 1), index_at(4, x, 0));
            assert_eq!(index_at(3, x, 0), index_at(4, x, FACE_HEIGHT - 1));
            assert_eq!(
                index_at(3, x, FACE_HEIGHT - 1),
                index_at(5, reversed, FACE_HEIGHT - 1)
            );
        }
        for y in 0..FACE_HEIGHT {
            let source_x = (y * (FACE_WIDTH - 1) + (FACE_HEIGHT - 1) / 2) / (FACE_HEIGHT - 1);
            let reversed = FACE_WIDTH - 1 - source_x;
            assert_eq!(
                index_at(2, 0, y),
                index_at(1, source_x, 0),
                "+Y/-X seam at y={y}, adjacent x={source_x}"
            );
            assert_eq!(index_at(2, FACE_WIDTH - 1, y), index_at(0, reversed, 0));
            assert_eq!(index_at(3, 0, y), index_at(1, reversed, FACE_HEIGHT - 1));
            assert_eq!(
                index_at(3, FACE_WIDTH - 1, y),
                index_at(0, source_x, FACE_HEIGHT - 1)
            );
        }
        for face in 0..FACE_COUNT {
            let x = face * FACE_WIDTH + FACE_WIDTH / 2;
            let y = FACE_HEIGHT / 2;
            let byte = texture.pixel_bytes()[y * (ATLAS_WIDTH / 2) + x / 2];
            let index = if x & 1 == 0 { byte & 0x0f } else { byte >> 4 };
            assert!(index < 16);
        }
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
