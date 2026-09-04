//! Generate the tracked Null Choir dark-science-fiction BSP project.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use psxed_project::brush::{Brush, BrushContents, Plane, BRUSH_UV_UNITS_PER_TEXEL};
use psxed_project::{
    MaterialResource, NodeId, NodeKind, ParticleEmitterSettings, ProjectDocument, ResourceData,
    ResourceId, SkyMode, SkyVisibility, Transform3,
};
use psxed_tex::{convert, Config, CropMode, PsxtDepth, Resampler};

const PROJECT_NAME: &str = "The Null Choir";
const TEXTURE_SIZE: u16 = 64;
const GRAND_SCALE: i32 = 2;
const GRAND_PIVOT: [i32; 3] = [4096, 0, 1024];
const SKY_SOURCE_RELATIVE: &str = "source_assets/sky/null_choir_eclipse_equirect_v1.png";
const SKY_TEXTURE_RELATIVE: &str = "assets/textures/sky/null_choir_eclipse_cube_4bpp.psxt";

const TEXTURES: [(&str, &str); 12] = [
    (
        "source_assets/textures/null_choir_bulkhead_v3.png",
        "assets/textures/null_choir_bulkhead_v3.psxt",
    ),
    (
        "source_assets/textures/null_choir_deck_v3.png",
        "assets/textures/null_choir_deck_v3.psxt",
    ),
    (
        "source_assets/textures/null_choir_rib_v3.png",
        "assets/textures/null_choir_rib_v3.psxt",
    ),
    (
        "source_assets/textures/null_choir_core_v3.png",
        "assets/textures/null_choir_core_v3.psxt",
    ),
    (
        "source_assets/textures/null_choir_wall_base_v3.png",
        "assets/textures/null_choir_wall_base_v3.psxt",
    ),
    (
        "source_assets/textures/null_choir_beam_face_v3.png",
        "assets/textures/null_choir_beam_face_v3.psxt",
    ),
    (
        "source_assets/textures/null_choir_beam_joint_v3.png",
        "assets/textures/null_choir_beam_joint_v3.psxt",
    ),
    (
        "source_assets/textures/null_choir_deck_edge_v3.png",
        "assets/textures/null_choir_deck_edge_v3.psxt",
    ),
    (
        "source_assets/textures/null_choir_ceiling_vent_v3.png",
        "assets/textures/null_choir_ceiling_vent_v3.psxt",
    ),
    (
        "source_assets/textures/null_choir_trench_liner_v3.png",
        "assets/textures/null_choir_trench_liner_v3.psxt",
    ),
    (
        "source_assets/textures/null_choir_hazard_inset_v3.png",
        "assets/textures/null_choir_hazard_inset_v3.psxt",
    ),
    (
        "source_assets/textures/null_choir_service_panel_v3.png",
        "assets/textures/null_choir_service_panel_v3.psxt",
    ),
];

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
        .join("null-choir");
    let output_dir = match parse_generator_args(std::env::args_os().skip(1), default_output)
        .unwrap_or_else(|error| panic!("{error}"))
    {
        GeneratorAction::Help => {
            println!("Usage: gen-null-choir-project [OUTPUT_DIR]");
            return;
        }
        GeneratorAction::Generate(output_dir) => output_dir,
    };
    generate(&output_dir);
}

fn generate(output_dir: &Path) {
    std::fs::create_dir_all(output_dir).expect("create Null Choir project directory");
    cook_textures(output_dir);
    cook_cube_sky(output_dir);

    let mut project = ProjectDocument::new(PROJECT_NAME);
    // The expanded multi-loop district benefits from separator-flow PVS and
    // shadowed lighting; Draft's conservative fast visibility exceeds the
    // compact PS1 visibility table for this topology.
    project.bsp_cook_mode = psxed_project::brush_world::BrushWorldCookMode::Release;
    // Concept-art view: inside the now cathedral-scale nave, wide enough to
    // reveal the bridge, coolant trench, and core without orbiting above the roof.
    project.editor_camera.orbit_yaw_q12 = 2048;
    project.editor_camera.orbit_pitch_q12 = 40;
    project.editor_camera.orbit_target = grand_point([4096, 900, 9800]);
    project.editor_camera.orbit_radius = 8400;

    let bulkhead = add_material(
        &mut project,
        "Choir Bulkhead",
        TEXTURES[0].1,
        [0x98, 0xa0, 0xa8],
    );
    let deck = add_material(
        &mut project,
        "Choir Deck Grating",
        TEXTURES[1].1,
        [0x98, 0x90, 0x88],
    );
    let rib = add_material(
        &mut project,
        "Choir Emergency Rib",
        TEXTURES[2].1,
        [0xa0, 0x88, 0x88],
    );
    let core = add_material(
        &mut project,
        "Choir Signal Core",
        TEXTURES[3].1,
        [0x88, 0xa8, 0xb8],
    );
    let wall_base = add_material(
        &mut project,
        "Choir Wall Plinth",
        TEXTURES[4].1,
        [0x90, 0x90, 0x90],
    );
    let beam_face = add_material(
        &mut project,
        "Choir Layered Beam",
        TEXTURES[5].1,
        [0x88, 0x84, 0x84],
    );
    let beam_joint = add_material(
        &mut project,
        "Choir Beam Knee",
        TEXTURES[6].1,
        [0x90, 0x80, 0x7c],
    );
    let deck_edge = add_material(
        &mut project,
        "Choir Deck Edge",
        TEXTURES[7].1,
        [0x88, 0x90, 0x94],
    );
    let ceiling_vent = add_material(
        &mut project,
        "Choir Ceiling Vent",
        TEXTURES[8].1,
        [0x78, 0x78, 0x78],
    );
    let trench_liner = add_material(
        &mut project,
        "Choir Trench Liner",
        TEXTURES[9].1,
        [0x70, 0x90, 0x98],
    );
    let hazard_inset = add_material(
        &mut project,
        "Choir Hazard Inset",
        TEXTURES[10].1,
        [0xa0, 0x78, 0x74],
    );
    let service_panel = add_material(
        &mut project,
        "Choir Service Panel",
        TEXTURES[11].1,
        [0x80, 0x88, 0x8c],
    );
    let sky_aperture = add_cube_sky_material(&mut project);
    configure_world_sky(&mut project, sky_aperture);

    let scene = project.active_scene_mut();
    let airlock = scene.add_node(NodeId::ROOT, "01 - Listening Airlock", NodeKind::Group);
    let throat = scene.add_node(NodeId::ROOT, "02 - Processional Throat", NodeKind::Group);
    let chamber = scene.add_node(NodeId::ROOT, "03 - Sunken Signal Chamber", NodeKind::Group);
    let core_group = scene.add_node(NodeId::ROOT, "04 - Choir Core", NodeKind::Group);
    let cloister = scene.add_node(NodeId::ROOT, "05 - Echo Cloister", NodeKind::Group);
    let ossuary = scene.add_node(NodeId::ROOT, "06 - Relay Ossuary", NodeKind::Group);
    let return_spine = scene.add_node(NodeId::ROOT, "07 - Return Spine", NodeKind::Group);
    let transfer = scene.add_node(NodeId::ROOT, "08 - Canticle Transfer", NodeKind::Group);
    let engine = scene.add_node(NodeId::ROOT, "09 - Dissonance Engine", NodeKind::Group);
    let starwell = scene.add_node(NodeId::ROOT, "10 - Starwell Approach", NodeKind::Group);
    let sky_court = scene.add_node(NodeId::ROOT, "11 - Null Sky Court", NodeKind::Group);
    let antenna = scene.add_node(NodeId::ROOT, "12 - Antenna Chapel", NodeKind::Group);
    let service_return = scene.add_node(NodeId::ROOT, "13 - Deep Service Return", NodeKind::Group);
    let lighting = scene.add_node(NodeId::ROOT, "14 - Emergency Lighting", NodeKind::Group);

    author_airlock(scene, airlock, bulkhead, deck, rib);
    author_processional_throat(scene, throat, bulkhead, deck, rib);
    author_signal_chamber(scene, chamber, bulkhead, deck, rib, core);
    author_core(scene, core_group, bulkhead, deck, rib, core);
    author_echo_annex(
        scene,
        cloister,
        ossuary,
        return_spine,
        bulkhead,
        deck,
        rib,
        core,
    );
    author_outer_district(
        scene,
        transfer,
        engine,
        starwell,
        sky_court,
        antenna,
        service_return,
        bulkhead,
        deck,
        rib,
        core,
        wall_base,
        beam_face,
        beam_joint,
        deck_edge,
        ceiling_vent,
        trench_liner,
        hazard_inset,
        service_panel,
        sky_aperture,
    );
    author_contextual_detail(
        scene,
        airlock,
        throat,
        chamber,
        core_group,
        cloister,
        ossuary,
        return_spine,
        wall_base,
        beam_face,
        beam_joint,
        deck_edge,
        ceiling_vent,
        trench_liner,
        hazard_inset,
        service_panel,
    );
    author_lighting(scene, lighting);
    author_player_and_lore(scene, airlock, core_group);
    author_archive_lore(scene, ossuary);
    make_architecture_grand(scene);

    let project_path = output_dir.join("project.ron");
    project
        .save_to_path(&project_path)
        .expect("save Null Choir project");
    let mut source = std::fs::read_to_string(&project_path).expect("read generated project");
    source.push('\n');
    std::fs::write(&project_path, source).expect("finish generated project");
    write_player_route_tape(output_dir);

    println!(
        "generated {} ({} brushes, {} scene nodes, {} resources)",
        project_path.display(),
        project.active_scene().brushes.len(),
        project.active_scene().nodes().len(),
        project.resources.len(),
    );
}

fn write_player_route_tape(output_dir: &Path) {
    const UP: u16 = 1 << 4;
    const RIGHT: u16 = 1 << 5;
    const DOWN: u16 = 1 << 6;
    const LEFT: u16 = 1 << 7;
    const L1: u16 = 1 << 10;
    const CROSS: u16 = 1 << 14;
    const SQUARE: u16 = 1 << 15;

    #[derive(Clone, Copy)]
    struct Sample {
        buttons: u16,
        right_x: u8,
        right_y: u8,
        left_x: u8,
        left_y: u8,
    }

    const NEUTRAL: Sample = Sample {
        buttons: 0,
        right_x: 128,
        right_y: 128,
        left_x: 128,
        left_y: 128,
    };
    let buttons = |mask| Sample {
        buttons: mask,
        ..NEUTRAL
    };
    let route = |direction| buttons(SQUARE | direction);
    let route_look_up = |direction| Sample {
        buttons: SQUARE | direction,
        right_y: 0,
        ..NEUTRAL
    };
    let mut samples = Vec::new();
    let mut hold = |count: usize, sample: Sample| {
        samples.extend(std::iter::repeat_n(sample, count));
    };

    // Cold boot/menu confirmation beats, matching the editor Play disc's
    // deterministic route clock.
    hold(120, NEUTRAL);
    hold(4, buttons(CROSS));
    hold(76, NEUTRAL);
    hold(4, buttons(CROSS));
    hold(96, NEUTRAL);
    hold(4, buttons(CROSS));
    hold(116, NEUTRAL);
    hold(4, buttons(CROSS));
    hold(120, NEUTRAL);

    // Airlock -> throat -> signal chamber crossing. The follow camera begins
    // on orbit yaw 2048, so camera-forward is authored +Z.
    hold(231, route(UP));
    hold(20, NEUTRAL);

    // Look east and cross from the core bridge into the Echo Cloister (+X).
    hold(6, route(RIGHT));
    hold(32, buttons(L1));
    hold(76, route(RIGHT));
    hold(20, NEUTRAL);

    // North through the narrow cloister threshold (+Z), then east along its
    // long gallery to the Relay Ossuary.
    hold(6, route(UP));
    hold(32, buttons(L1));
    hold(6, route(UP));
    hold(6, route(RIGHT));
    hold(32, buttons(L1));
    hold(92, route(RIGHT));
    hold(25, NEUTRAL);

    // Align with the new east aperture, then take the long axial route through
    // the Canticle Transfer, Dissonance Engine, Starwell, and Null Sky Court.
    hold(6, route(UP));
    hold(24, buttons(L1));
    hold(12, route(UP));
    hold(8, NEUTRAL);
    hold(6, route(RIGHT));
    hold(24, buttons(L1));
    // Deliberately touch the chapel's east wall, then back up to the center of
    // its undercroft doorway. Using the wall as a deterministic stop keeps the
    // long tape stable even if acceleration tuning changes slightly.
    hold(250, route(RIGHT));
    // Tilt up on entering the exterior court. Manual pitch persists until the
    // chapel recenter, keeping the eclipse panorama above the causeway in view.
    hold(8, route_look_up(RIGHT));
    hold(342, route(RIGHT));
    hold(12, NEUTRAL);
    hold(6, route(LEFT));
    hold(24, buttons(L1));
    hold(40, route(LEFT));

    // Descend into the Deep Service undercroft.
    hold(6, route(UP));
    hold(24, buttons(L1));
    hold(111, route(UP));
    hold(16, NEUTRAL);

    // Cross the complete undercroft to its west wall, center on the engine
    // doorway, then retrace the transfer to the Relay Ossuary.
    // First step away from the southern rib line so the westward run cannot
    // snag on a decorative buttress after using the far wall as a stop.
    hold(6, route(DOWN));
    hold(24, buttons(L1));
    hold(12, route(DOWN));
    hold(6, route(LEFT));
    hold(24, buttons(L1));
    hold(400, route(LEFT));
    hold(12, NEUTRAL);
    hold(6, route(RIGHT));
    hold(24, buttons(L1));
    hold(6, route(DOWN));
    hold(24, buttons(L1));
    hold(96, route(DOWN));
    hold(6, route(LEFT));
    hold(24, buttons(L1));
    hold(168, route(LEFT));
    hold(8, NEUTRAL);
    hold(6, route(UP));
    hold(24, buttons(L1));
    hold(36, route(UP));
    hold(20, NEUTRAL);

    // Turn west and traverse the complete Return Spine back toward the nave.
    hold(6, route(LEFT));
    hold(32, buttons(L1));
    hold(66, route(LEFT));
    hold(25, NEUTRAL);
    hold(88, route(LEFT));
    hold(60, NEUTRAL);

    let mut csv = String::from(
        "psoxide-tape,v2,clock=video_frame,start_poll=0\n\
         frame,buttons,right_x,right_y,left_x,left_y\n",
    );
    for (frame, sample) in samples.iter().enumerate() {
        writeln!(
            csv,
            "{frame},{},{},{},{},{}",
            sample.buttons, sample.right_x, sample.right_y, sample.left_x, sample.left_y
        )
        .expect("write Null Choir tape row");
    }
    let path = output_dir.join("captures/null_choir_player_route.csv");
    std::fs::create_dir_all(path.parent().expect("route tape parent"))
        .expect("create route tape directory");
    std::fs::write(path, csv).expect("write Null Choir player route tape");
}

fn author_airlock(
    scene: &mut psxed_project::Scene,
    group: NodeId,
    bulkhead: ResourceId,
    deck: ResourceId,
    rib: ResourceId,
) {
    // A low, compressed starting room with a single framed exit.
    push_box(scene, group, deck, [2816, 0, 256], [5376, 128, 2304], 2048);
    push_box(
        scene,
        group,
        bulkhead,
        [2816, 2048, 256],
        [5376, 2176, 2304],
        1024,
    );
    push_box(
        scene,
        group,
        bulkhead,
        [2816, 128, 256],
        [2944, 2048, 2304],
        1024,
    );
    push_box(
        scene,
        group,
        bulkhead,
        [5248, 128, 256],
        [5376, 2048, 2304],
        1024,
    );
    push_box(
        scene,
        group,
        bulkhead,
        [2944, 128, 256],
        [5248, 2048, 384],
        1024,
    );
    push_box(
        scene,
        group,
        bulkhead,
        [2944, 128, 2176],
        [3584, 2048, 2304],
        1024,
    );
    push_box(
        scene,
        group,
        bulkhead,
        [4608, 128, 2176],
        [5248, 2048, 2304],
        1024,
    );
    push_box(
        scene,
        group,
        rib,
        [3584, 1664, 2176],
        [4608, 2048, 2304],
        512,
    );
    for x in [3072, 4864] {
        push_box(scene, group, rib, [x, 128, 384], [x + 256, 1920, 640], 512);
    }
}

fn author_processional_throat(
    scene: &mut psxed_project::Scene,
    group: NodeId,
    bulkhead: ResourceId,
    deck: ResourceId,
    rib: ResourceId,
) {
    push_box(scene, group, deck, [3584, 0, 2048], [4608, 128, 6272], 2048);
    push_box(
        scene,
        group,
        bulkhead,
        [3456, 128, 2048],
        [3584, 2048, 6272],
        1024,
    );
    push_box(
        scene,
        group,
        bulkhead,
        [4608, 128, 2048],
        [4736, 2048, 6272],
        1024,
    );
    push_box(
        scene,
        group,
        bulkhead,
        [3456, 2048, 2048],
        [4736, 2176, 6272],
        1024,
    );

    for z in [2560, 3328, 4096, 4864, 5632] {
        push_box(scene, group, rib, [3584, 128, z], [3712, 1792, z + 96], 512);
        push_box(scene, group, rib, [4480, 128, z], [4608, 1792, z + 96], 512);
        push_box(
            scene,
            group,
            rib,
            [3584, 1792, z],
            [4608, 2048, z + 96],
            512,
        );
    }
}

fn author_signal_chamber(
    scene: &mut psxed_project::Scene,
    group: NodeId,
    bulkhead: ResourceId,
    deck: ResourceId,
    rib: ResourceId,
    core: ResourceId,
) {
    // Outer shell. The walls extend down to the trench floor so the BSP stays sealed.
    push_box(
        scene,
        group,
        bulkhead,
        [896, -512, 6144],
        [1024, 3072, 11392],
        1024,
    );
    // The east wall carries two full-height apertures into the side loop.
    for (min, max) in [
        ([7168, -512, 6144], [7296, 3072, 8448]),
        ([7168, 2048, 8448], [7296, 3072, 9216]),
        ([7168, -512, 9216], [7296, 3072, 10240]),
        ([7168, 2048, 10240], [7296, 3072, 10880]),
        ([7168, -512, 10880], [7296, 3072, 11392]),
    ] {
        push_box(scene, group, bulkhead, min, max, 1024);
    }
    push_box(
        scene,
        group,
        bulkhead,
        [1024, -512, 11264],
        [7168, 3072, 11392],
        1024,
    );
    push_box(
        scene,
        group,
        bulkhead,
        [1024, 128, 6144],
        [3584, 3072, 6272],
        1024,
    );
    push_box(
        scene,
        group,
        bulkhead,
        [4608, 128, 6144],
        [7168, 3072, 6272],
        1024,
    );
    push_box(
        scene,
        group,
        rib,
        [3584, 2048, 6144],
        [4608, 3072, 6272],
        512,
    );
    push_box(
        scene,
        group,
        bulkhead,
        [1024, 3072, 6144],
        [7168, 3200, 11264],
        1024,
    );

    // The chamber floor is deliberately broken around a deep coolant trench.
    for (min, max) in [
        ([1024, -512, 6144], [7168, 128, 7424]),
        ([1024, -512, 7424], [2560, 128, 10240]),
        ([5632, -512, 7424], [7168, 128, 10240]),
        ([1024, -512, 10240], [7168, 128, 11264]),
    ] {
        push_box(scene, group, deck, min, max, 2048);
    }
    push_box(
        scene,
        group,
        bulkhead,
        [2048, -640, 7424],
        [6144, -512, 10240],
        1024,
    );

    push_box_with_contents(
        scene,
        group,
        core,
        [2048, -512, 7424],
        [6144, -256, 10240],
        1024,
        BrushContents::Slime,
    );

    // A narrow axial bridge and broader crossing platform form the playable silhouette.
    push_box(scene, group, rib, [3712, 0, 7424], [4480, 128, 9728], 512);
    push_box(scene, group, rib, [2048, 0, 8320], [6144, 128, 8832], 512);
    for x in [3712, 4352] {
        for (z0, z1) in [(7424, 8320), (8832, 9728)] {
            push_box(
                scene,
                group,
                bulkhead,
                [x, 128, z0],
                [x + 128, 384, z1],
                1024,
            );
        }
    }

    // Repeating buttresses make the large rectangular shell read as a chapel nave.
    for z in [6656, 7808, 8960, 10112] {
        push_box(
            scene,
            group,
            rib,
            [1024, 128, z],
            [1280, 2560, z + 256],
            512,
        );
        push_box(
            scene,
            group,
            rib,
            [6912, 128, z],
            [7168, 2560, z + 256],
            512,
        );
    }
    for z in [7040, 8192, 9344, 10496] {
        push_box(
            scene,
            group,
            rib,
            [1280, 2816, z],
            [6912, 3072, z + 192],
            512,
        );
    }

    // Six small resonators repeat the cyan language before the central core.
    for (x, z) in [
        (1472, 7168),
        (1472, 9088),
        (1472, 10496),
        (6720, 7168),
        (6720, 9088),
        (6720, 10496),
    ] {
        push_octagonal_prism(scene, group, core, [x, z], 144, [128, 1408], 512);
    }
}

fn author_core(
    scene: &mut psxed_project::Scene,
    group: NodeId,
    bulkhead: ResourceId,
    deck: ResourceId,
    rib: ResourceId,
    core: ResourceId,
) {
    // Two shallow steps lift the player from the bridge onto the core dais.
    push_box(scene, group, rib, [3328, 0, 9600], [4864, 256, 9856], 512);
    push_box(
        scene,
        group,
        deck,
        [2944, -512, 9856],
        [5248, 384, 11264],
        2048,
    );
    push_box(
        scene,
        group,
        rib,
        [3072, 384, 9984],
        [3328, 2432, 11008],
        512,
    );
    push_box(
        scene,
        group,
        rib,
        [4864, 384, 9984],
        [5120, 2432, 11008],
        512,
    );
    push_octagonal_prism(scene, group, core, [4096, 10496], 512, [384, 2560], 1024);

    // A dark crown and four braces keep the bright core from becoming a flat billboard.
    push_box(
        scene,
        group,
        bulkhead,
        [3584, 2432, 9984],
        [4608, 2688, 11008],
        1024,
    );
    for (x0, x1, z0, z1) in [
        (3328, 3584, 10112, 10368),
        (4608, 4864, 10112, 10368),
        (3328, 3584, 10624, 10880),
        (4608, 4864, 10624, 10880),
    ] {
        push_box(scene, group, rib, [x0, 384, z0], [x1, 1792, z1], 512);
    }
}

#[allow(clippy::too_many_arguments)]
fn author_echo_annex(
    scene: &mut psxed_project::Scene,
    cloister: NodeId,
    ossuary: NodeId,
    return_spine: NodeId,
    bulkhead: ResourceId,
    deck: ResourceId,
    rib: ResourceId,
    core: ResourceId,
) {
    // The first east aperture leaves the nave at the crosswalk and compresses
    // back down to the throat's claustrophobic scale.
    push_box(
        scene,
        cloister,
        deck,
        [7168, 0, 8448],
        [9600, 128, 9216],
        2048,
    );
    push_box(
        scene,
        cloister,
        bulkhead,
        [7168, 2048, 8320],
        [9600, 2176, 9344],
        1024,
    );
    push_box(
        scene,
        cloister,
        bulkhead,
        [7168, 128, 8320],
        [9600, 2048, 8448],
        1024,
    );
    push_box(
        scene,
        cloister,
        bulkhead,
        [7168, 128, 9216],
        [9600, 2048, 9344],
        1024,
    );
    for x in [7552, 8192, 8832] {
        push_box(
            scene,
            cloister,
            rib,
            [x, 128, 8448],
            [x + 96, 1792, 8576],
            512,
        );
        push_box(
            scene,
            cloister,
            rib,
            [x, 128, 9088],
            [x + 96, 1792, 9216],
            512,
        );
        push_box(
            scene,
            cloister,
            rib,
            [x, 1792, 8448],
            [x + 96, 2048, 9216],
            512,
        );
    }

    // A taller auxiliary chamber turns the material language into a forest
    // of resonators. Its two west apertures make the whole annex a loop.
    push_box(
        scene,
        ossuary,
        deck,
        [9472, 0, 7424],
        [12672, 128, 11264],
        2048,
    );
    push_box(
        scene,
        ossuary,
        bulkhead,
        [9472, 2688, 7424],
        [12672, 2816, 11264],
        1024,
    );
    // The east wall is split around the Canticle Transfer doorway into the
    // new outer district.
    for (min, max) in [
        ([12544, 128, 7424], [12672, 2688, 8960]),
        ([12544, 2048, 8960], [12672, 2688, 9856]),
        ([12544, 128, 9856], [12672, 2688, 11264]),
    ] {
        push_box(scene, ossuary, bulkhead, min, max, 1024);
    }
    push_box(
        scene,
        ossuary,
        bulkhead,
        [9472, 128, 7424],
        [12672, 2688, 7552],
        1024,
    );
    push_box(
        scene,
        ossuary,
        bulkhead,
        [9472, 128, 11136],
        [12672, 2688, 11264],
        1024,
    );
    for (min, max) in [
        ([9472, 128, 7552], [9600, 2688, 8448]),
        ([9472, 2048, 8448], [9600, 2688, 9216]),
        ([9472, 128, 9216], [9600, 2688, 10240]),
        ([9472, 2048, 10240], [9600, 2688, 10880]),
        ([9472, 128, 10880], [9600, 2688, 11136]),
    ] {
        push_box(scene, ossuary, bulkhead, min, max, 1024);
    }

    // The tuning plinth is deliberately squat enough to see over from either
    // doorway, while the surrounding resonators create changing silhouettes.
    push_box(
        scene,
        ossuary,
        rib,
        [10368, 0, 7680],
        [11776, 256, 8832],
        512,
    );
    push_octagonal_prism(scene, ossuary, core, [11072, 8256], 384, [256, 1536], 1024);
    for (x, z) in [
        (10240, 7808),
        (11072, 7808),
        (11904, 7808),
        (11904, 11008),
        (11072, 11008),
        (10240, 11008),
    ] {
        push_octagonal_prism(scene, ossuary, core, [x, z], 128, [128, 1280], 512);
    }
    for x in [9856, 10880, 11904] {
        push_box(
            scene,
            ossuary,
            rib,
            [x, 128, 7552],
            [x + 192, 2304, 7808],
            512,
        );
        push_box(
            scene,
            ossuary,
            rib,
            [x, 128, 10880],
            [x + 192, 2304, 11136],
            512,
        );
        push_box(
            scene,
            ossuary,
            rib,
            [x, 2432, 7808],
            [x + 192, 2688, 10880],
            512,
        );
    }

    // The return spine exits beside the core dais, rewarding exploration with
    // a changed angle on the hero chamber rather than a dead-end backtrack.
    push_box(
        scene,
        return_spine,
        deck,
        [7168, 0, 10240],
        [9600, 128, 10880],
        2048,
    );
    push_box(
        scene,
        return_spine,
        bulkhead,
        [7168, 2048, 10112],
        [9600, 2176, 11008],
        1024,
    );
    push_box(
        scene,
        return_spine,
        bulkhead,
        [7168, 128, 10112],
        [9600, 2048, 10240],
        1024,
    );
    push_box(
        scene,
        return_spine,
        bulkhead,
        [7168, 128, 10880],
        [9600, 2048, 11008],
        1024,
    );
    for x in [7552, 8192, 8832] {
        push_box(
            scene,
            return_spine,
            rib,
            [x, 128, 10240],
            [x + 96, 1792, 10368],
            512,
        );
        push_box(
            scene,
            return_spine,
            rib,
            [x, 128, 10752],
            [x + 96, 1792, 10880],
            512,
        );
        push_box(
            scene,
            return_spine,
            rib,
            [x, 1792, 10240],
            [x + 96, 2048, 10880],
            512,
        );
    }
}

/// A second explorable district beyond the Relay Ossuary. Two long transfer
/// passages, two machinery chambers, an outdoor sky court, and a deep service
/// loop roughly double the route length without turning the expansion into one
/// undifferentiated hall.
#[allow(clippy::too_many_arguments)]
fn author_outer_district(
    scene: &mut psxed_project::Scene,
    transfer: NodeId,
    engine: NodeId,
    starwell: NodeId,
    sky_court: NodeId,
    antenna: NodeId,
    service_return: NodeId,
    bulkhead: ResourceId,
    deck: ResourceId,
    rib: ResourceId,
    core: ResourceId,
    wall_base: ResourceId,
    beam_face: ResourceId,
    beam_joint: ResourceId,
    deck_edge: ResourceId,
    ceiling_vent: ResourceId,
    trench_liner: ResourceId,
    hazard_inset: ResourceId,
    service_panel: ResourceId,
    sky_aperture: ResourceId,
) {
    // Canticle Transfer: a long, compressed decompression gallery leaving the
    // ossuary through its new east aperture.
    push_box(
        scene,
        transfer,
        deck,
        [12544, 0, 8960],
        [15488, 128, 9856],
        512,
    );
    push_box(
        scene,
        transfer,
        bulkhead,
        [12544, 2304, 8960],
        [15488, 2432, 9856],
        512,
    );
    for (min, max) in [
        ([12544, 128, 8960], [15488, 2304, 9088]),
        ([12544, 128, 9728], [15488, 2304, 9856]),
    ] {
        push_box(scene, transfer, bulkhead, min, max, 512);
    }
    for (min, max) in [
        ([12544, 128, 9088], [15488, 512, 9152]),
        ([12544, 128, 9664], [15488, 512, 9728]),
    ] {
        push_box(scene, transfer, wall_base, min, max, 512);
    }
    for x in [13184, 13952, 14720] {
        push_box(
            scene,
            transfer,
            rib,
            [x, 128, 9088],
            [x + 128, 2048, 9216],
            512,
        );
        push_box(
            scene,
            transfer,
            rib,
            [x, 128, 9600],
            [x + 128, 2048, 9728],
            512,
        );
        push_box(
            scene,
            transfer,
            beam_face,
            [x + 24, 384, 9216],
            [x + 104, 1664, 9280],
            512,
        );
        push_box(
            scene,
            transfer,
            beam_joint,
            [x, 1792, 9216],
            [x + 128, 2048, 9600],
            512,
        );
    }
    for x in [12736, 13760, 14784] {
        push_box(
            scene,
            transfer,
            ceiling_vent,
            [x, 2240, 9216],
            [x + 512, 2304, 9600],
            512,
        );
    }
    // Dissonance Engine: a tall intermediate room with two exits. The south
    // aperture starts the deep return loop; the east aperture continues to the
    // sky court.
    push_box(
        scene,
        engine,
        deck,
        [15360, 0, 7680],
        [18560, 128, 11264],
        512,
    );
    push_box(
        scene,
        engine,
        bulkhead,
        [15360, 2816, 7680],
        [18560, 2944, 11264],
        512,
    );
    push_box(
        scene,
        engine,
        bulkhead,
        [15360, 128, 7680],
        [18560, 2816, 7808],
        512,
    );
    for (min, max) in [
        ([15360, 128, 7808], [15488, 2816, 8960]),
        ([15360, 2048, 8960], [15488, 2816, 9856]),
        ([15360, 128, 9856], [15488, 2816, 11136]),
        ([18432, 128, 7808], [18560, 2816, 8960]),
        ([18432, 2048, 8960], [18560, 2816, 9856]),
        ([18432, 128, 9856], [18560, 2816, 11136]),
        ([15360, 128, 11136], [16896, 2816, 11264]),
        ([16896, 2048, 11136], [17792, 2816, 11264]),
        ([17792, 128, 11136], [18560, 2816, 11264]),
    ] {
        push_box(scene, engine, bulkhead, min, max, 512);
    }
    for (min, max) in [
        ([15488, 128, 7808], [18432, 512, 7872]),
        ([15488, 128, 11072], [16896, 512, 11136]),
        ([17792, 128, 11072], [18432, 512, 11136]),
        ([15488, 128, 7872], [15552, 512, 8960]),
        ([15488, 128, 9856], [15552, 512, 11072]),
        ([18368, 128, 7872], [18432, 512, 8960]),
        ([18368, 128, 9856], [18432, 512, 11072]),
    ] {
        push_box(scene, engine, wall_base, min, max, 512);
    }
    push_octagonal_prism(scene, engine, core, [16960, 8448], 448, [128, 2048], 512);
    push_octagonal_prism(scene, engine, rib, [16960, 8448], 704, [128, 320], 512);
    for (x, z) in [(15872, 8192), (18048, 8192), (15872, 10496), (18048, 10496)] {
        push_octagonal_prism(scene, engine, core, [x, z], 128, [128, 1280], 512);
    }
    for x in [15872, 18048] {
        push_box(
            scene,
            engine,
            service_panel,
            [x - 256, 704, 7808],
            [x + 256, 1792, 7872],
            512,
        );
        push_box(
            scene,
            engine,
            ceiling_vent,
            [x - 384, 2752, 8576],
            [x + 384, 2816, 10112],
            512,
        );
    }

    // Starwell Approach: a short threshold that makes the exterior reveal a
    // deliberate transition instead of exposing the sky directly from the
    // machinery room.
    push_box(
        scene,
        starwell,
        deck,
        [18432, 0, 8960],
        [19968, 128, 9856],
        512,
    );
    push_box(
        scene,
        starwell,
        bulkhead,
        [18432, 2304, 8960],
        [19968, 2432, 9856],
        512,
    );
    for (min, max) in [
        ([18432, 128, 8960], [19968, 2304, 9088]),
        ([18432, 128, 9728], [19968, 2304, 9856]),
        ([18432, 128, 9088], [19968, 512, 9152]),
        ([18432, 128, 9664], [19968, 512, 9728]),
    ] {
        let material = if min[2] == 9088 || min[2] == 9664 {
            wall_base
        } else {
            bulkhead
        };
        push_box(scene, starwell, material, min, max, 512);
    }
    for x in [18752, 19392] {
        push_box(
            scene,
            starwell,
            rib,
            [x, 128, 9088],
            [x + 128, 2048, 9216],
            512,
        );
        push_box(
            scene,
            starwell,
            rib,
            [x, 128, 9600],
            [x + 128, 2048, 9728],
            512,
        );
        push_box(
            scene,
            starwell,
            hazard_inset,
            [x + 24, 704, 9216],
            [x + 104, 1152, 9280],
            512,
        );
    }

    // Null Sky Court: the "open" space is still a sealed BSP hull. Invisible
    // sky-aperture brushes form its high walls and roof, so the world sky is
    // revealed without leaking the compiler's outside void into the level.
    push_box(
        scene,
        sky_court,
        deck,
        [19840, 0, 6400],
        [25216, 128, 12416],
        512,
    );
    for (min, max) in [
        ([19840, 128, 6400], [19968, 1152, 8960]),
        ([19840, 128, 9856], [19968, 1152, 12416]),
        ([25088, 128, 6400], [25216, 1152, 8960]),
        ([25088, 128, 9856], [25216, 1152, 12416]),
        ([19968, 128, 6400], [25088, 1152, 6528]),
        ([19968, 128, 12288], [25088, 1152, 12416]),
    ] {
        push_box(scene, sky_court, bulkhead, min, max, 512);
    }
    for (min, max) in [
        ([19840, 1152, 6400], [19968, 3200, 8960]),
        ([19840, 2304, 8960], [19968, 3200, 9856]),
        ([19840, 1152, 9856], [19968, 3200, 12416]),
        ([25088, 1152, 6400], [25216, 3200, 8960]),
        ([25088, 2304, 8960], [25216, 3200, 9856]),
        ([25088, 1152, 9856], [25216, 3200, 12416]),
        ([19968, 1152, 6400], [25088, 3200, 6528]),
        ([19968, 1152, 12288], [25088, 3200, 12416]),
        ([19968, 3072, 6528], [25088, 3200, 12288]),
    ] {
        push_box(scene, sky_court, sky_aperture, min, max, 512);
    }

    // A broad axial causeway and low perimeter curb give the exterior its own
    // ground plane while leaving most of the horizon unobstructed.
    for (min, max) in [
        ([19968, 128, 9152], [25088, 256, 9216]),
        ([19968, 128, 9600], [25088, 256, 9664]),
    ] {
        push_box(scene, sky_court, deck_edge, min, max, 512);
    }
    for (min, max) in [
        ([19968, 128, 6528], [25088, 512, 6592]),
        ([19968, 128, 12224], [25088, 512, 12288]),
        ([19968, 128, 6592], [20032, 512, 8960]),
        ([19968, 128, 9856], [20032, 512, 12224]),
        ([25024, 128, 6592], [25088, 512, 8960]),
        ([25024, 128, 9856], [25088, 512, 12224]),
    ] {
        push_box(scene, sky_court, wall_base, min, max, 512);
    }
    for (x, z) in [(20736, 7424), (24320, 7424), (20736, 11392), (24320, 11392)] {
        push_octagonal_prism(scene, sky_court, core, [x, z], 160, [128, 1536], 512);
        push_octagonal_prism(scene, sky_court, beam_joint, [x, z], 256, [128, 256], 512);
    }
    // The eclipse receiver sits off the main causeway so the full west-east
    // traversal remains collision-clear.
    push_octagonal_prism(scene, sky_court, rib, [22528, 7936], 640, [128, 384], 512);
    push_octagonal_prism(scene, sky_court, core, [22528, 7936], 352, [384, 2432], 512);
    for (x0, x1, z0, z1) in [
        (21632, 22016, 7040, 7424),
        (23040, 23424, 7040, 7424),
        (21632, 22016, 8448, 8832),
        (23040, 23424, 8448, 8832),
    ] {
        push_box(
            scene,
            sky_court,
            beam_face,
            [x0, 128, z0],
            [x1, 1152, z1],
            512,
        );
    }

    // Antenna Chapel: an enclosed terminus on the far side of the court with a
    // second door into the service loop.
    push_box(
        scene,
        antenna,
        deck,
        [25088, 0, 7680],
        [28160, 128, 11264],
        512,
    );
    push_box(
        scene,
        antenna,
        bulkhead,
        [25088, 2816, 7680],
        [28160, 2944, 11264],
        512,
    );
    push_box(
        scene,
        antenna,
        bulkhead,
        [25088, 128, 7680],
        [28160, 2816, 7808],
        512,
    );
    push_box(
        scene,
        antenna,
        bulkhead,
        [28032, 128, 7808],
        [28160, 2816, 11136],
        512,
    );
    for (min, max) in [
        ([25088, 128, 7808], [25216, 2816, 8960]),
        ([25088, 2304, 8960], [25216, 2816, 9856]),
        ([25088, 128, 9856], [25216, 2816, 11136]),
        ([25088, 128, 11136], [26368, 2816, 11264]),
        ([26368, 2048, 11136], [27264, 2816, 11264]),
        ([27264, 128, 11136], [28160, 2816, 11264]),
    ] {
        push_box(scene, antenna, bulkhead, min, max, 512);
    }
    for (min, max) in [
        ([25216, 128, 7808], [28032, 512, 7872]),
        ([25216, 128, 11072], [26368, 512, 11136]),
        ([27264, 128, 11072], [28032, 512, 11136]),
        ([27968, 128, 7872], [28032, 512, 11072]),
    ] {
        push_box(scene, antenna, wall_base, min, max, 512);
    }
    // Keep the axial route collision-clear while retaining a substantial,
    // layered receiver silhouette at the chapel's north side.
    push_octagonal_prism(scene, antenna, rib, [26752, 8192], 512, [128, 384], 512);
    push_octagonal_prism(scene, antenna, core, [26752, 8192], 320, [384, 2432], 512);
    for (x, z) in [(25600, 8192), (27648, 8192), (25600, 10496), (27648, 10496)] {
        push_octagonal_prism(scene, antenna, core, [x, z], 128, [128, 1344], 512);
    }
    for x in [25600, 27648] {
        push_box(
            scene,
            antenna,
            service_panel,
            [x - 256, 768, 7808],
            [x + 256, 1792, 7872],
            512,
        );
        push_box(
            scene,
            antenna,
            ceiling_vent,
            [x - 384, 2752, 8576],
            [x + 384, 2816, 10112],
            512,
        );
    }

    // Deep Service Return: a broad undercroft joins the two southern doorways.
    // Making this one coherent room avoids collision seams at corridor height
    // changes and gives the expansion another distinct spatial beat.
    push_box(
        scene,
        service_return,
        deck,
        [16896, 0, 11136],
        [27264, 128, 13280],
        512,
    );
    push_box(
        scene,
        service_return,
        bulkhead,
        [16896, 2048, 11136],
        [27264, 2176, 13280],
        512,
    );
    for (min, max) in [
        ([17792, 128, 11136], [26368, 2048, 11264]),
        ([16896, 128, 11264], [17024, 2048, 13280]),
        ([27136, 128, 11264], [27264, 2048, 13280]),
        ([17024, 128, 13152], [27136, 2048, 13280]),
    ] {
        push_box(scene, service_return, bulkhead, min, max, 512);
    }
    for (min, max) in [
        ([17792, 128, 11264], [26368, 512, 11328]),
        ([17024, 128, 13088], [27136, 512, 13152]),
        ([17024, 128, 11328], [17088, 512, 13088]),
        ([27072, 128, 11328], [27136, 512, 13088]),
    ] {
        push_box(scene, service_return, wall_base, min, max, 512);
    }
    for x in (17920..26368).step_by(1536) {
        push_box(
            scene,
            service_return,
            rib,
            [x, 128, 11264],
            [x + 128, 1792, 11392],
            512,
        );
        push_box(
            scene,
            service_return,
            rib,
            [x, 128, 13024],
            [x + 128, 1792, 13152],
            512,
        );
        push_box(
            scene,
            service_return,
            hazard_inset,
            [x + 24, 704, 11392],
            [x + 104, 1088, 11456],
            512,
        );
    }
    for x in (17408..26624).step_by(1536) {
        push_box(
            scene,
            service_return,
            ceiling_vent,
            [x, 1984, 11904],
            [x + 768, 2048, 12416],
            512,
        );
    }

    // Cyan drainage channels run along the north edge, safely away from the
    // central player route through the undercroft.
    for (x0, x1) in [(18432, 20480), (22016, 24064), (25600, 26624)] {
        push_box(
            scene,
            service_return,
            trench_liner,
            [x0, 128, 11456],
            [x1, 192, 11520],
            512,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn author_contextual_detail(
    scene: &mut psxed_project::Scene,
    airlock: NodeId,
    throat: NodeId,
    chamber: NodeId,
    core_group: NodeId,
    cloister: NodeId,
    ossuary: NodeId,
    return_spine: NodeId,
    wall_base: ResourceId,
    beam_face: ResourceId,
    beam_joint: ResourceId,
    deck_edge: ResourceId,
    ceiling_vent: ResourceId,
    trench_liner: ResourceId,
    hazard_inset: ResourceId,
    service_panel: ResourceId,
) {
    // The airlock teaches the material grammar: calm upper wall modules sit
    // on a vented plinth, while a recessed ceiling cassette aims at the exit.
    for (min, max) in [
        ([2944, 128, 384], [3008, 512, 2176]),
        ([5184, 128, 384], [5248, 512, 2176]),
        ([3008, 128, 384], [5184, 512, 448]),
    ] {
        push_box(scene, airlock, wall_base, min, max, 1024);
    }
    push_box(
        scene,
        airlock,
        service_panel,
        [3328, 640, 384],
        [4864, 1536, 448],
        1024,
    );
    push_box(
        scene,
        airlock,
        ceiling_vent,
        [3328, 1984, 640],
        [4864, 2048, 1920],
        1024,
    );

    // Each nave rib is now a layered assembly: column face, red inset,
    // diagonal knee, and a separate vent cassette between frames.
    for z in [2560, 3328, 4096, 4864, 5632] {
        for (min, max) in [
            ([3712, 256, z + 16], [3760, 1536, z + 80]),
            ([4432, 256, z + 16], [4480, 1536, z + 80]),
        ] {
            push_box(scene, throat, beam_face, min, max, 1024);
        }
        for (min, max) in [
            ([3760, 640, z], [3808, 1024, z + 96]),
            ([4384, 640, z], [4432, 1024, z + 96]),
        ] {
            push_box(scene, throat, hazard_inset, min, max, 1024);
        }
        push_prism(
            scene,
            throat,
            beam_joint,
            &[[3712, 1408], [3840, 1408], [4096, 1792], [3968, 1792]],
            [0, 1],
            2,
            [z, z + 96],
            1024,
        );
        push_prism(
            scene,
            throat,
            beam_joint,
            &[[4352, 1408], [4480, 1408], [4224, 1792], [4096, 1792]],
            [0, 1],
            2,
            [z, z + 96],
            1024,
        );
    }
    for (min, max) in [
        ([3584, 128, 2304], [3648, 512, 6144]),
        ([4544, 128, 2304], [4608, 512, 6144]),
    ] {
        push_box(scene, throat, wall_base, min, max, 1024);
    }
    for z in [2816, 3584, 4352, 5120] {
        push_box(
            scene,
            throat,
            ceiling_vent,
            [3840, 1984, z],
            [4352, 2048, z + 384],
            1024,
        );
    }

    // A continuous plinth makes the chamber walls visually land on the deck.
    for (min, max) in [
        ([1024, 128, 6336], [1088, 512, 11264]),
        ([1024, 128, 6272], [3584, 512, 6336]),
        ([4608, 128, 6272], [7168, 512, 6336]),
        ([1024, 128, 11200], [7168, 512, 11264]),
        ([7104, 128, 6272], [7168, 512, 8448]),
        ([7104, 128, 9216], [7168, 512, 10240]),
        ([7104, 128, 10880], [7168, 512, 11264]),
    ] {
        push_box(scene, chamber, wall_base, min, max, 1024);
    }

    // Four interrupted curb runs terminate the deck cleanly at the coolant
    // void while preserving both bridge crossings.
    for (min, max) in [
        ([2496, 0, 7424], [2560, 256, 8256]),
        ([2496, 0, 8896], [2560, 256, 10240]),
        ([5632, 0, 7424], [5696, 256, 8256]),
        ([5632, 0, 8896], [5696, 256, 10240]),
        ([2560, 0, 7360], [3648, 256, 7424]),
        ([4544, 0, 7360], [5632, 256, 7424]),
        ([2560, 0, 10240], [3648, 256, 10304]),
        ([4544, 0, 10240], [5632, 256, 10304]),
    ] {
        push_box(scene, chamber, deck_edge, min, max, 1024);
    }
    for (min, max) in [
        ([2560, -512, 7424], [2624, 0, 10240]),
        ([5568, -512, 7424], [5632, 0, 10240]),
        ([2624, -512, 7424], [5568, 0, 7488]),
        ([2624, -512, 10176], [5568, 0, 10240]),
    ] {
        push_box(scene, chamber, trench_liner, min, max, 1024);
    }

    // Layer the nave buttresses instead of leaving them as monolithic boxes.
    for z in [6656, 7808, 8960, 10112] {
        push_box(
            scene,
            chamber,
            beam_face,
            [1280, 384, z + 32],
            [1344, 2304, z + 224],
            1024,
        );
        push_box(
            scene,
            chamber,
            beam_face,
            [6848, 384, z + 32],
            [6912, 2304, z + 224],
            1024,
        );
    }
    for (x0, x1) in [(1536, 2816), (5376, 6656)] {
        for (z0, z1) in [(6848, 7616), (9152, 9920)] {
            push_box(
                scene,
                chamber,
                service_panel,
                [x0, 768, z0],
                [x1, 1792, z1],
                1024,
            );
        }
    }
    for (x0, x1, z0, z1) in [
        (1536, 3072, 7040, 8192),
        (5120, 6656, 7040, 8192),
        (1536, 3072, 9344, 10496),
        (5120, 6656, 9344, 10496),
    ] {
        push_box(
            scene,
            chamber,
            ceiling_vent,
            [x0, 3008, z0],
            [x1, 3072, z1],
            1024,
        );
    }

    // The core dais gets its own edge and joint language instead of borrowing
    // the generic hazard stripe everywhere.
    push_box(
        scene,
        core_group,
        deck_edge,
        [2944, 384, 9856],
        [5248, 512, 9984],
        1024,
    );
    for (min, max) in [
        ([3328, 512, 10048], [3392, 2176, 10944]),
        ([4800, 512, 10048], [4864, 2176, 10944]),
    ] {
        push_box(scene, core_group, beam_face, min, max, 1024);
    }
    for (x0, x1) in [(3392, 3648), (4544, 4800)] {
        push_box(
            scene,
            core_group,
            beam_joint,
            [x0, 1792, 10240],
            [x1, 2432, 10752],
            1024,
        );
    }

    // Annex passages use the same lower-wall datum and ceiling modules, which
    // makes the loop feel constructed as part of the same facility.
    for (min, max) in [
        ([7168, 128, 8448], [9600, 512, 8512]),
        ([7168, 128, 9152], [9600, 512, 9216]),
    ] {
        push_box(scene, cloister, wall_base, min, max, 1024);
    }
    for x in [7552, 8192, 8832] {
        push_box(
            scene,
            cloister,
            beam_face,
            [x + 16, 384, 8576],
            [x + 80, 1536, 8640],
            1024,
        );
        push_box(
            scene,
            cloister,
            hazard_inset,
            [x + 16, 704, 9088],
            [x + 80, 1088, 9152],
            1024,
        );
    }
    for x in [7424, 8320, 9216] {
        push_box(
            scene,
            cloister,
            ceiling_vent,
            [x, 1984, 8640],
            [x + 512, 2048, 9024],
            1024,
        );
    }

    for (min, max) in [
        ([9536, 128, 7552], [12544, 512, 7616]),
        ([9536, 128, 11072], [12544, 512, 11136]),
        ([12480, 128, 7616], [12544, 512, 8960]),
        ([12480, 128, 9856], [12544, 512, 11072]),
    ] {
        push_box(scene, ossuary, wall_base, min, max, 1024);
    }
    for (x, z) in [(9856, 7808), (12032, 7808), (9856, 10624), (12032, 10624)] {
        push_box(
            scene,
            ossuary,
            service_panel,
            [x - 256, 640, z],
            [x + 256, 1792, z + 64],
            1024,
        );
    }
    for (x0, x1) in [(9856, 10752), (11264, 12032)] {
        push_box(
            scene,
            ossuary,
            ceiling_vent,
            [x0, 2624, 8704],
            [x1, 2688, 9984],
            1024,
        );
    }

    for (min, max) in [
        ([7168, 128, 10240], [9600, 512, 10304]),
        ([7168, 128, 10816], [9600, 512, 10880]),
    ] {
        push_box(scene, return_spine, wall_base, min, max, 1024);
    }
    for x in [7552, 8192, 8832] {
        push_box(
            scene,
            return_spine,
            beam_face,
            [x + 16, 384, 10368],
            [x + 80, 1536, 10432],
            1024,
        );
    }
}

fn author_lighting(scene: &mut psxed_project::Scene, group: NodeId) {
    add_light(
        scene,
        group,
        "Airlock Blood Lamp",
        [4096.0, 1536.0, 768.0],
        [255, 54, 42],
        0.8,
        2.5,
    );
    for (index, z) in [2816.0, 3840.0, 4864.0, 5888.0].into_iter().enumerate() {
        add_light(
            scene,
            group,
            &format!("Throat Pulse {:02}", index + 1),
            [4096.0, 1664.0, z],
            if index % 2 == 0 {
                [164, 34, 30]
            } else {
                [32, 96, 116]
            },
            0.55,
            1.75,
        );
    }
    for (index, (x, z)) in [
        (1536.0, 6912.0),
        (6656.0, 6912.0),
        (1536.0, 9472.0),
        (6656.0, 9472.0),
    ]
    .into_iter()
    .enumerate()
    {
        add_light(
            scene,
            group,
            &format!("Nave Emergency {:02}", index + 1),
            [x, 2048.0, z],
            [192, 38, 32],
            0.65,
            2.6,
        );
    }
    add_light(
        scene,
        group,
        "Core Lower Glow",
        [4096.0, 768.0, 10112.0],
        [30, 176, 224],
        1.2,
        3.5,
    );
    add_light(
        scene,
        group,
        "Core Crown Glow",
        [4096.0, 2304.0, 10496.0],
        [48, 208, 255],
        1.0,
        3.0,
    );
    for (index, (x, z, color)) in [
        (7808.0, 8832.0, [176, 34, 30]),
        (8960.0, 8832.0, [36, 112, 136]),
        (10112.0, 8064.0, [176, 34, 30]),
        (12032.0, 8064.0, [176, 34, 30]),
        (11072.0, 8256.0, [40, 184, 220]),
        (10112.0, 10624.0, [176, 34, 30]),
        (12032.0, 10624.0, [176, 34, 30]),
        (8320.0, 10560.0, [36, 112, 136]),
    ]
    .into_iter()
    .enumerate()
    {
        add_light(
            scene,
            group,
            &format!("Annex Pulse {:02}", index + 1),
            [x, 1664.0, z],
            color,
            if index == 4 { 1.0 } else { 0.6 },
            if index == 4 { 3.0 } else { 2.2 },
        );
    }
    for (index, (x, y, z, color, intensity, radius)) in [
        (14016.0, 1664.0, 9408.0, [176, 34, 30], 0.6, 2.2),
        (16960.0, 2176.0, 8448.0, [36, 168, 208], 1.0, 3.0),
        (19200.0, 1664.0, 9408.0, [176, 34, 30], 0.65, 2.2),
        (20736.0, 1024.0, 7424.0, [28, 128, 160], 0.65, 2.8),
        (24320.0, 1024.0, 11392.0, [176, 34, 30], 0.55, 2.6),
        (22528.0, 1792.0, 7936.0, [44, 184, 216], 0.9, 3.4),
        (26752.0, 2176.0, 8320.0, [48, 196, 232], 1.05, 3.2),
        (17408.0, 1536.0, 12896.0, [176, 34, 30], 0.55, 2.0),
        (20480.0, 1536.0, 12896.0, [32, 112, 136], 0.5, 2.0),
        (23552.0, 1536.0, 12896.0, [176, 34, 30], 0.55, 2.0),
        (26624.0, 1536.0, 12896.0, [32, 112, 136], 0.5, 2.0),
    ]
    .into_iter()
    .enumerate()
    {
        add_light(
            scene,
            group,
            &format!("Outer District Static {:02}", index + 1),
            [x, y, z],
            color,
            intensity,
            radius,
        );
    }
    add_particle_emitter(scene, group, "Trench Vapour West", [2816.0, -192.0, 8448.0]);
    add_particle_emitter(scene, group, "Trench Vapour East", [5376.0, -192.0, 9472.0]);
    add_particle_emitter(scene, group, "Ossuary Static", [11072.0, 320.0, 8256.0]);
    add_particle_emitter(scene, group, "Sky Court Static", [22528.0, 512.0, 7936.0]);
}

fn author_player_and_lore(scene: &mut psxed_project::Scene, airlock: NodeId, core: NodeId) {
    let spawn = scene.add_node(
        airlock,
        "Player Spawn",
        NodeKind::SpawnPoint {
            player: true,
            character: None,
        },
    );
    scene.node_mut(spawn).expect("player spawn").transform = Transform3 {
        translation: [4096.0, 129.0, 1024.0],
        rotation_degrees: [0.0, 180.0, 0.0],
        ..Transform3::default()
    };

    let terminal = scene.add_node(core, "Last Canticle Terminal", NodeKind::Entity);
    scene.node_mut(terminal).expect("terminal entity").transform = Transform3 {
        translation: [4096.0, 385.0, 9728.0],
        ..Transform3::default()
    };
    scene.add_node(
        terminal,
        "Read Last Canticle",
        NodeKind::PointOfInterest {
            pages: vec![
                "LISTENING ARRAY 07\nFINAL CANTICLE".to_string(),
                "No signal crossed the dark.\nThe dark answered anyway.".to_string(),
                "The choir is not transmitting.".to_string(),
                "It remembers the voices\nthat built it.".to_string(),
            ],
            pages_it: Vec::new(),
            prompt: "X - LISTEN".to_string(),
            radius: 704,
            marker_height: 224,
            repeatable: true,
            persistence_id: "null_choir_last_canticle".to_string(),
            reward: None,
            enabled: true,
        },
    );
}

fn author_archive_lore(scene: &mut psxed_project::Scene, parent: NodeId) {
    let archive = scene.add_node(parent, "Relay Ossuary Archive", NodeKind::Entity);
    scene.node_mut(archive).expect("archive entity").transform = Transform3 {
        translation: [12160.0, 129.0, 10368.0],
        rotation_degrees: [0.0, 270.0, 0.0],
        ..Transform3::default()
    };
    scene.add_node(
        archive,
        "Read Relay Archive",
        NodeKind::PointOfInterest {
            pages: vec![
                "RELAY OSSUARY\nMAINTENANCE RECORD".to_string(),
                "Seven operators entered.\nEight voices answered.".to_string(),
                "Do not synchronize the array.\nLet each relay drift alone.".to_string(),
            ],
            pages_it: Vec::new(),
            prompt: "X - READ".to_string(),
            radius: 640,
            marker_height: 208,
            repeatable: true,
            persistence_id: "null_choir_relay_archive".to_string(),
            reward: None,
            enabled: true,
        },
    );
}

fn grand_point(point: [i32; 3]) -> [i32; 3] {
    std::array::from_fn(|axis| GRAND_PIVOT[axis] + (point[axis] - GRAND_PIVOT[axis]) * GRAND_SCALE)
}

fn grand_translation(translation: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|axis| {
        let pivot = GRAND_PIVOT[axis] as f32;
        pivot + (translation[axis] - pivot) * GRAND_SCALE as f32
    })
}

/// Expand the finished blockout around the player spawn into a megastructure.
///
/// Face scale grows with the geometry, preserving both square world-space
/// texels and the original repeat count. That makes the architecture larger
/// relative to the player without turning its material panels into long UV
/// gradients or multiplying the BSP's authored brush count.
fn make_architecture_grand(scene: &mut psxed_project::Scene) {
    for brush in &mut scene.brushes {
        for face in &mut brush.faces {
            for point in &mut face.points {
                *point = grand_point(*point);
            }
            face.uv.scale_q8 = face
                .uv
                .scale_q8
                .map(|scale| (i32::from(scale) * GRAND_SCALE).clamp(1, i32::from(i16::MAX)) as i16);
        }
    }

    let node_ids = scene.nodes().iter().map(|node| node.id).collect::<Vec<_>>();
    for id in node_ids {
        let node = scene.node_mut(id).expect("grand-scale scene node");
        let owns_world_position = matches!(
            node.kind,
            NodeKind::Entity
                | NodeKind::SpawnPoint { .. }
                | NodeKind::PointLight { .. }
                | NodeKind::ParticleEmitter { .. }
        );
        if owns_world_position {
            node.transform.translation = grand_translation(node.transform.translation);
        }

        match &mut node.kind {
            NodeKind::PointOfInterest {
                radius,
                marker_height,
                ..
            } => {
                *radius = radius.saturating_mul(GRAND_SCALE as u16);
                *marker_height = marker_height.saturating_mul(GRAND_SCALE as u16);
            }
            NodeKind::PointLight { radius, .. } => *radius *= GRAND_SCALE as f32,
            NodeKind::ParticleEmitter { settings } => {
                settings.spawn_radius = settings.spawn_radius.saturating_mul(GRAND_SCALE as u16);
                settings.start_size = settings.start_size.saturating_mul(GRAND_SCALE as u16);
                settings.end_size = settings.end_size.saturating_mul(GRAND_SCALE as u16);
            }
            _ => {}
        }
    }
}

fn configure_world_sky(project: &mut ProjectDocument, sky_material: ResourceId) {
    let scene = project.active_scene_mut();
    let NodeKind::World { sky, .. } = &mut scene
        .node_mut(NodeId::ROOT)
        .expect("Null Choir world root")
        .kind
    else {
        panic!("Null Choir scene root must be a World");
    };
    sky.mode = SkyMode::Cube;
    sky.visibility = SkyVisibility::ThroughSkySurfaces;
    sky.texture = Some(sky_material);
}

fn add_cube_sky_material(project: &mut ProjectDocument) -> ResourceId {
    let mut material = MaterialResource::opaque(Some(SKY_TEXTURE_RELATIVE.to_string()));
    material.sky_aperture = true;
    material.tint = [255, 255, 255];
    project.add_resource(
        "Null Choir Eclipse Cube Sky",
        ResourceData::Material(material),
    )
}

fn cook_cube_sky(output_dir: &Path) {
    let source_path = output_dir.join(SKY_SOURCE_RELATIVE);
    let source = std::fs::read(&source_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", source_path.display()));
    let cooked = psxed_project::sky_texture::cook_equirectangular_cube_sky(&source)
        .unwrap_or_else(|error| panic!("cook {}: {error}", source_path.display()));
    let destination = output_dir.join(SKY_TEXTURE_RELATIVE);
    std::fs::create_dir_all(destination.parent().expect("cube-sky texture parent"))
        .expect("create cube-sky texture directory");
    std::fs::write(&destination, cooked)
        .unwrap_or_else(|error| panic!("write {}: {error}", destination.display()));
}

fn add_material(
    project: &mut ProjectDocument,
    name: &str,
    relative_path: &str,
    tint: [u8; 3],
) -> ResourceId {
    let mut material = MaterialResource::opaque(Some(relative_path.to_string()));
    material.tint = tint;
    project.add_resource(name, ResourceData::Material(material))
}

fn push_box(
    scene: &mut psxed_project::Scene,
    group: NodeId,
    material: ResourceId,
    min: [i32; 3],
    max: [i32; 3],
    tile_density_q8: i16,
) {
    push_box_with_contents(
        scene,
        group,
        material,
        min,
        max,
        tile_density_q8,
        BrushContents::Solid,
    );
}

fn push_box_with_contents(
    scene: &mut psxed_project::Scene,
    group: NodeId,
    material: ResourceId,
    min: [i32; 3],
    max: [i32; 3],
    tile_density_q8: i16,
    contents: BrushContents,
) {
    // A packed PS1 vertex has an 8-bit UV. Keeping each authored panel to
    // one texture repeat prevents a large face from crossing the 256 wrap
    // and rasterizing as one long, backwards gradient. CSG removes the
    // internal faces between these adjacent cells, so this changes render
    // tessellation without changing the solid.
    let panel_span = uv_safe_panel_span(tile_density_q8);
    let xs = panel_cuts(min[0], max[0], panel_span);
    let ys = panel_cuts(min[1], max[1], panel_span);
    let zs = panel_cuts(min[2], max[2], panel_span);
    for x in xs.windows(2) {
        for y in ys.windows(2) {
            for z in zs.windows(2) {
                let mut brush = Brush::cuboid([x[0], y[0], z[0]], [x[1], y[1], z[1]]);
                brush.contents = contents;
                brush.group = Some(group);
                paint(&mut brush, material, tile_density_q8);
                scene.brushes.push(brush);
            }
        }
    }
}

fn push_octagonal_prism(
    scene: &mut psxed_project::Scene,
    group: NodeId,
    material: ResourceId,
    center: [i32; 2],
    radius: i32,
    y: [i32; 2],
    tile_density_q8: i16,
) {
    let diagonal = radius * 3 / 5;
    let [cx, cz] = center;
    let polygon = [
        [cx - diagonal, cz - radius],
        [cx + diagonal, cz - radius],
        [cx + radius, cz - diagonal],
        [cx + radius, cz + diagonal],
        [cx + diagonal, cz + radius],
        [cx - diagonal, cz + radius],
        [cx - radius, cz + diagonal],
        [cx - radius, cz - diagonal],
    ];
    for panel_y in panel_cuts(y[0], y[1], uv_safe_panel_span(tile_density_q8)).windows(2) {
        let mut brush = Brush::convex_prism(&polygon, [0, 2], 1, [panel_y[0], panel_y[1]])
            .expect("octagonal prism");
        brush.group = Some(group);
        paint(&mut brush, material, tile_density_q8);
        scene.brushes.push(brush);
    }
}

#[allow(clippy::too_many_arguments)]
fn push_prism(
    scene: &mut psxed_project::Scene,
    group: NodeId,
    material: ResourceId,
    polygon: &[[i32; 2]],
    plane_axes: [usize; 2],
    depth_axis: usize,
    depth: [i32; 2],
    tile_density_q8: i16,
) {
    for panel_depth in
        panel_cuts(depth[0], depth[1], uv_safe_panel_span(tile_density_q8)).windows(2)
    {
        let mut brush = Brush::convex_prism(
            polygon,
            plane_axes,
            depth_axis,
            [panel_depth[0], panel_depth[1]],
        )
        .expect("contextual convex prism");
        brush.group = Some(group);
        paint(&mut brush, material, tile_density_q8);
        scene.brushes.push(brush);
    }
}

fn panel_cuts(min: i32, max: i32, maximum_span: i32) -> Vec<i32> {
    let mut cuts = vec![min];
    let mut cursor = min;
    while max - cursor > maximum_span {
        cursor += maximum_span;
        cuts.push(cursor);
    }
    cuts.push(max);
    cuts
}

fn authored_face_scale(tile_density_q8: i16) -> i16 {
    let density = i32::from(tile_density_q8.max(1));
    ((1024 * 256) / density).clamp(256, 512) as i16
}

fn uv_safe_panel_span(tile_density_q8: i16) -> i32 {
    let scale_q8 = i32::from(authored_face_scale(tile_density_q8));
    (i32::from(TEXTURE_SIZE) * BRUSH_UV_UNITS_PER_TEXEL as i32 * scale_q8) / 256
}

/// Paint using relative texel density, not raw [`psxed_project::FaceUv`] scale.
///
/// The original blockout passed these values straight through as face scale,
/// where larger values make a texture physically larger. That stretched one
/// texture over most of a room. Treating 512 as the baseline density makes the
/// call sites read naturally instead: 1024 is twice the detail and 2048 is four
/// times the detail. The 64 px v3 sources use twice the previous face scale so
/// a single cropped module retains its established physical size.
fn paint(brush: &mut Brush, material: ResourceId, tile_density_q8: i16) {
    let scale_q8 = authored_face_scale(tile_density_q8);
    for face in &mut brush.faces {
        face.material = Some(material);
        let plane = Plane::from_points(face.points).expect("painted brush face plane");
        face.uv.scale_q8 = square_texel_scale(&plane, scale_q8);
    }
}

/// Paraxial projection drops the dominant normal axis. On an angled face,
/// moving one projected unit can therefore travel more than one world unit
/// along the surface. Divide that UV axis by the tangent's length so equal
/// texture steps cover equal physical distances in both directions.
fn square_texel_scale(plane: &Plane, base_scale_q8: i16) -> [i16; 2] {
    let normal = plane.normal.map(|component| component as f64);
    let abs = normal.map(f64::abs);
    let (dropped, projected) = if abs[1] >= abs[0] && abs[1] >= abs[2] {
        (1, [0, 2])
    } else if abs[0] >= abs[2] {
        (0, [2, 1])
    } else {
        (2, [0, 1])
    };
    projected.map(|axis| {
        let slope = normal[axis] / normal[dropped];
        let tangent_length = (1.0 + slope * slope).sqrt();
        (f64::from(base_scale_q8) / tangent_length)
            .round()
            .clamp(1.0, f64::from(i16::MAX)) as i16
    })
}

fn add_light(
    scene: &mut psxed_project::Scene,
    parent: NodeId,
    name: &str,
    translation: [f32; 3],
    color: [u8; 3],
    intensity: f32,
    radius: f32,
) {
    let light = scene.add_node(
        parent,
        name,
        NodeKind::PointLight {
            color,
            intensity,
            radius,
        },
    );
    scene.node_mut(light).expect("point light").transform = Transform3 {
        translation,
        ..Transform3::default()
    };
}

fn add_particle_emitter(
    scene: &mut psxed_project::Scene,
    parent: NodeId,
    name: &str,
    translation: [f32; 3],
) {
    let emitter = scene.add_node(
        parent,
        name,
        NodeKind::ParticleEmitter {
            settings: ParticleEmitterSettings {
                max_particles: 16,
                spawn_rate_q8: 3 * 256,
                lifetime_frames: 72,
                start_size: 32,
                end_size: 128,
                start_color: [54, 174, 208],
                end_color: [8, 28, 36],
                base_velocity_q4: [0, 12, 0],
                random_velocity_q4: [8, 4, 8],
                spawn_radius: 48,
                ..ParticleEmitterSettings::default()
            },
        },
    );
    scene.node_mut(emitter).expect("particle emitter").transform = Transform3 {
        translation,
        ..Transform3::default()
    };
}

fn cook_textures(output_dir: &Path) {
    for (source_relative, cooked_relative) in TEXTURES {
        let source = output_dir.join(source_relative);
        let destination = output_dir.join(cooked_relative);
        let source_bytes = std::fs::read(&source)
            .unwrap_or_else(|error| panic!("read texture source {}: {error}", source.display()));
        let bytes = convert(
            &source_bytes,
            &Config {
                width: TEXTURE_SIZE,
                height: TEXTURE_SIZE,
                depth: PsxtDepth::Bit4,
                crop: CropMode::CentreSquare,
                resampler: Resampler::Lanczos3,
                transparent_index_zero: false,
                clut_rows: 1,
            },
        )
        .unwrap_or_else(|error| panic!("cook texture {}: {error}", source.display()));
        std::fs::create_dir_all(destination.parent().expect("texture destination parent"))
            .expect("create cooked texture directory");
        std::fs::write(&destination, bytes)
            .unwrap_or_else(|error| panic!("write texture {}: {error}", destination.display()));
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
        return Err("Usage: gen-null-choir-project [OUTPUT_DIR]".to_string());
    }
    if first == "--help" || first == "-h" {
        return Ok(GeneratorAction::Help);
    }
    Ok(GeneratorAction::Generate(PathBuf::from(first)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_bsp::collision::TraceScratch;
    use psx_bsp::collision_provider::{
        select_body_hull, PxbspCollisionModel, PxbspCollisionProvider,
    };
    use psx_bsp::pxbsp_resident::PxbspResidentMap;
    use psx_bsp::SliceReader;
    use psx_engine::{trace_collision, CollisionTraceQuery, CollisionTraceShape, RoomPoint};

    const fn engine(authored: i32) -> i32 {
        let divisor = psxed_project::units::WORLD_UNIT_DIVISOR;
        (authored + divisor / 2) / divisor
    }

    fn route_point([x, y, z]: [i32; 3]) -> RoomPoint {
        let [x, y, z] = grand_point([x, y, z]);
        RoomPoint::new(engine(x), engine(y), engine(z))
    }

    fn prove_cooked_player_route(world: &psxed_project::playtest::PlaytestPxbspWorld) {
        let mut map = PxbspResidentMap::with_capacity(world.bytes.len());
        map.load(0, &mut SliceReader::new(&world.bytes))
            .expect("load Null Choir resident PXBSP");
        let player_body = world.body_hulls[0];
        let hull = select_body_hull(&world.body_hulls, player_body.radius, player_body.height)
            .expect("select Null Choir player hull");
        let models: [PxbspCollisionModel; 0] = [];

        let route = [
            [4096, 129, 1024],
            [4096, 129, 7040],
            [4096, 129, 8320],
            [4096, 129, 9472],
            [4096, 129, 8576],
            [6500, 129, 8576],
            [6500, 129, 8832],
            [8000, 129, 8832],
            [9856, 129, 8832],
            [9856, 129, 9344],
            [12032, 129, 9344],
            [19200, 129, 9344],
            [22400, 129, 9344],
            [26752, 129, 9344],
            [26752, 129, 12896],
            [22000, 129, 12896],
            [17400, 129, 12896],
            [17400, 129, 9344],
            [12032, 129, 9344],
            [12032, 129, 10496],
            [9856, 129, 10496],
            [9856, 129, 10560],
            [8000, 129, 10560],
            [7040, 129, 10560],
        ];

        for (index, segment) in route.windows(2).enumerate() {
            let start = route_point(segment[0]);
            let end = route_point(segment[1]);
            let mut scratch = TraceScratch::new();
            let mut provider = PxbspCollisionProvider::new(
                &map,
                hull,
                &models,
                CollisionTraceShape::Body {
                    radius: player_body.radius,
                    height: player_body.height,
                },
                &mut scratch,
            )
            .expect("Null Choir collision provider");
            let trace = trace_collision(
                &mut provider,
                CollisionTraceQuery::body(start, end, player_body.radius, player_body.height),
            )
            .expect("Null Choir route trace");
            assert!(
                !trace.start_solid && !trace.all_solid && !trace.hit(),
                "route segment {index} blocked: {:?} -> {:?}: {trace:?}",
                segment[0],
                segment[1]
            );
        }
    }

    #[test]
    fn default_output_targets_the_tracked_editor_project() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("projects")
            .join("null-choir");
        assert!(path
            .canonicalize()
            .expect("canonical tracked project path")
            .ends_with("editor/projects/null-choir"));
    }

    #[test]
    fn parser_accepts_an_explicit_output_directory() {
        assert_eq!(
            parse_generator_args([OsString::from("somewhere")], PathBuf::from("default")),
            Ok(GeneratorAction::Generate(PathBuf::from("somewhere")))
        );
    }

    #[test]
    fn angled_faces_keep_square_world_space_texels() {
        let polygon = [[-307, -512], [307, -512], [512, -307], [512, 307]];
        let mut brush = Brush::convex_prism(&polygon, [0, 2], 1, [0, 1024])
            .expect("representative angled prism");
        let mut project = ProjectDocument::new("UV metric test");
        let material = project.add_resource(
            "Test",
            ResourceData::Material(MaterialResource::opaque(None)),
        );
        paint(&mut brush, material, 2048);

        for face in &brush.faces {
            let plane = Plane::from_points(face.points).expect("face plane");
            let normal = plane.normal.map(|component| component as f64);
            let abs = normal.map(f64::abs);
            let (dropped, projected) = if abs[1] >= abs[0] && abs[1] >= abs[2] {
                (1, [0, 2])
            } else if abs[0] >= abs[2] {
                (0, [2, 1])
            } else {
                (2, [0, 1])
            };
            let world_per_texel = projected.map(|axis| {
                let slope = normal[axis] / normal[dropped];
                let world_per_projected_unit = (1.0 + slope * slope).sqrt();
                let projected_texels_per_unit =
                    face.uv.apply_linear([1.0 / BRUSH_UV_UNITS_PER_TEXEL, 0.0])[0].abs();
                let projected_texels_per_unit = if axis == projected[0] {
                    projected_texels_per_unit
                } else {
                    face.uv.apply_linear([0.0, 1.0 / BRUSH_UV_UNITS_PER_TEXEL])[1].abs()
                };
                world_per_projected_unit / projected_texels_per_unit
            });
            let ratio = world_per_texel[0] / world_per_texel[1];
            assert!(
                (ratio - 1.0).abs() < 0.01,
                "non-square texels on {:?}: {world_per_texel:?}",
                face.points
            );
        }
    }

    #[test]
    fn tracked_project_loads_and_cooks_as_pxbsp() {
        let project_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("projects")
            .join("null-choir");
        let project = ProjectDocument::load_from_path(project_dir.join("project.ron"))
            .expect("load tracked Null Choir project");
        let leak = psxed_project::brush_world::diagnose_brush_world_leak(project.clone())
            .expect("diagnose tracked Null Choir BSP hull");
        assert!(
            leak.is_empty(),
            "Null Choir BSP leak path: {:?}; likely opening: {:?}",
            leak.path,
            leak.likely_opening
        );
        let (package, report) = psxed_project::playtest::build_package(&project, &project_dir);
        assert!(report.is_ok(), "cook errors: {:?}", report.errors);
        let package = package.expect("Null Choir playtest package");
        let world = match &package.world_geometry {
            psxed_project::playtest::PlaytestWorldGeometry::Pxbsp(world) => world,
            other => panic!("expected Null Choir PXBSP, got {other:?}"),
        };
        prove_cooked_player_route(world);
        // The playtest package may also contain its built-in fallback texture;
        // every authored texture is verified individually below.
        assert!(package.texture_asset_count() >= TEXTURES.len());
        for (_, cooked_relative) in TEXTURES {
            let cooked_path = project_dir.join(cooked_relative);
            let bytes = std::fs::read(&cooked_path)
                .unwrap_or_else(|error| panic!("read {}: {error}", cooked_path.display()));
            assert_eq!(&bytes[..4], b"PSXT", "{} magic", cooked_path.display());
            let width = u16::from_le_bytes([bytes[14], bytes[15]]);
            let height = u16::from_le_bytes([bytes[16], bytes[17]]);
            assert_eq!(
                [width, height],
                [TEXTURE_SIZE, TEXTURE_SIZE],
                "{} dimensions",
                cooked_path.display()
            );
        }
        let scene = project.active_scene();
        let face_scales: Vec<i16> = scene
            .brushes
            .iter()
            .flat_map(|brush| brush.faces.iter())
            .flat_map(|face| face.uv.scale_q8)
            .collect();
        assert!(
            face_scales.iter().all(|scale| (360..=1024).contains(scale)),
            "Null Choir UV scales escaped the authored texel-density range"
        );
        assert!(
            face_scales.contains(&512) && face_scales.contains(&1024),
            "Null Choir should retain both architectural and fine-detail UV densities"
        );
        for (brush_index, brush) in scene.brushes.iter().enumerate() {
            let solved = brush.solve();
            for (face_index, polygon) in solved.polygons.iter().enumerate() {
                let Some(polygon) = polygon else { continue };
                let face = &brush.faces[face_index];
                let plane = Plane::from_points(face.points).expect("authored face plane");
                let mut min = [f64::MAX; 2];
                let mut max = [f64::MIN; 2];
                for &vertex in &polygon.verts {
                    let raw = psxed_project::brush::paraxial_uv(&plane, vertex);
                    let uv = face.uv.apply([
                        raw[0] / BRUSH_UV_UNITS_PER_TEXEL,
                        raw[1] / BRUSH_UV_UNITS_PER_TEXEL,
                    ]);
                    for axis in 0..2 {
                        min[axis] = min[axis].min(uv[axis]);
                        max[axis] = max[axis].max(uv[axis]);
                    }
                }
                let span = [max[0] - min[0], max[1] - min[1]];
                assert!(
                    span[0] <= f64::from(TEXTURE_SIZE) + 0.01
                        && span[1] <= f64::from(TEXTURE_SIZE) + 0.01,
                    "brush {brush_index} face {face_index} crosses a texture repeat: {span:?}"
                );
            }
        }
        assert!(
            scene.brushes.len() >= 128,
            "brushes: {}",
            scene.brushes.len()
        );
        assert_eq!(
            scene
                .nodes()
                .iter()
                .filter(|node| matches!(node.kind, NodeKind::PointOfInterest { .. }))
                .count(),
            2
        );
        assert_eq!(
            scene
                .nodes()
                .iter()
                .filter(|node| matches!(node.kind, NodeKind::ParticleEmitter { .. }))
                .count(),
            4
        );
        assert!(
            scene
                .brushes
                .iter()
                .any(|brush| brush.contents == BrushContents::Slime),
            "the coolant trench should retain its slime volume"
        );
    }
}
