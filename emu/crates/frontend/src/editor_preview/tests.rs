use super::{
    animated_material_quad_uvs, euler_rotation_q12, face_side_visible,
    horizontal_triangle_world_points, light_face, material_blend_mode, material_sized_uvs,
    material_texture_tint, node_room_local_origin, preview_lights, preview_model_reference,
    preview_player_reference, preview_projected_triangle_hw_safe, preview_scratch,
    preview_shadow_radius, preview_static_model_reference, preview_vertices_in_front, push_clear,
    push_tri, push_tri_colors, push_wall_face, room_depth_slot, rotate_image_prop_local,
    setup_gte_for_camera, shadow_depth_slot, should_draw_culled_face_outline,
    wall_material_sidedness_for_edge, wall_side_visible, yaw_rotation_q12, FaceShade, MaterialSlot,
    PreviewFog, WallEdge, GRID_TILE_UV, PREVIEW_FLOOR_UVS, PREVIEW_GEOMETRY_SLOT_MAX,
    PREVIEW_GEOMETRY_SLOT_MIN, PREVIEW_SHADOW_DEPTH_BIAS, PREVIEW_SHADOW_RADIUS_MAX,
    PREVIEW_SHADOW_RADIUS_MIN, PREVIEW_WALL_UVS,
};
use psx_engine::{Mat3I16, PointLightSample, WorldVertex};
use psx_gpu::material::BlendMode;
use psx_gte::scene::Projected;
use psxed_project::portal_rooms::PortalEdge;
use psxed_project::{
    Corner, GridDirection, GridSplit, GridUvTransform, GridVerticalFace, MaterialFaceSidedness,
    MaterialResource, NodeId, NodeKind, ProjectDocument, ResourceData, WorldGrid,
};
use psxed_ui::{ViewportCameraMode, ViewportCameraState};

fn flat(r: u8, g: u8, b: u8) -> FaceShade {
    FaceShade::Flat {
        rgb: (r, g, b),
        sidedness: psxed_project::MaterialFaceSidedness::Front,
    }
}

fn flat_sided(r: u8, g: u8, b: u8, sidedness: MaterialFaceSidedness) -> FaceShade {
    FaceShade::Flat {
        rgb: (r, g, b),
        sidedness,
    }
}

fn unpack(shade: FaceShade) -> (u8, u8, u8) {
    match shade {
        FaceShade::Flat { rgb, .. } => rgb,
        FaceShade::Textured { tint, .. } => tint,
    }
}

#[test]
fn editor_preview_uses_authored_average_blend_mode() {
    let mut project = ProjectDocument::new("blend-preview");
    let water = project.add_resource(
        "Water",
        ResourceData::Material(MaterialResource::translucent(
            None,
            psxed_project::PsxBlendMode::Average,
        )),
    );

    assert_eq!(
        material_blend_mode(&project, Some(water)),
        BlendMode::Average
    );
}

#[test]
fn editor_water_preview_uses_material_uv_motion() {
    let mut project = ProjectDocument::new("animated-water-preview");
    let mut material = MaterialResource::translucent(None, psxed_project::PsxBlendMode::Average);
    material.animation.mode = psxed_project::MaterialAnimationMode::UvScroll;
    material.animation.uv_scroll.enabled = true;
    material.animation.uv_scroll.speed_u_q8 = 8 * 256;
    material.animation.uv_scroll.speed_v_q8 = 4 * 256;
    let material_id = project.add_resource("Water", ResourceData::Material(material));
    let slot = MaterialSlot {
        tpage_word: 0,
        clut_word: 0,
        texture_window: psx_gpu::material::TextureWindow::NONE,
        texture_width: 64,
        texture_height: 64,
    };

    assert_eq!(
        animated_material_quad_uvs(&project, material_id, slot, 0)[0],
        (0, 0)
    );
    assert_eq!(
        animated_material_quad_uvs(&project, material_id, slot, 60)[0],
        (8, 4)
    );
}

fn address_window(addr: usize) -> usize {
    addr & !(super::OT_ADDRESS_WINDOW_BYTES - 1)
}

fn assert_same_ot_window<T>(label: &str, ot_window: usize, value: &T) {
    let start = value as *const T as usize;
    let end = start + core::mem::size_of::<T>().saturating_sub(1);
    assert_eq!(address_window(start), ot_window, "{label} start");
    assert_eq!(address_window(end), ot_window, "{label} end");
}

fn headless_preview_renderer() -> Option<psx_gpu_render::HwRenderer> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))?;
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("editor-preview-test-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
        },
        None,
    ))
    .ok()?;
    Some(psx_gpu_render::HwRenderer::new_headless(device, queue))
}

fn count_nonblack_rgba(rgba: &[u8]) -> usize {
    rgba.chunks_exact(4)
        .filter(|px| px[0] != 0 || px[1] != 0 || px[2] != 0)
        .count()
}

#[test]
fn preview_scratch_packet_arenas_share_one_ot_address_window() {
    let scratch = preview_scratch()
        .lock()
        .expect("editor preview scratch mutex");
    let ot_window = address_window(scratch.ot.submit_head() as usize);

    assert_same_ot_window("ordering table", ot_window, &scratch.ot);
    assert_same_ot_window("clear packet", ot_window, &scratch.clear_packet);
    assert_same_ot_window("sky quads", ot_window, &scratch.sky_quads);
    assert_same_ot_window("far vista quads", ot_window, &scratch.far_vista_quads);
    assert_same_ot_window("flat tris", ot_window, &scratch.tris);
    assert_same_ot_window("textured tris", ot_window, &scratch.tex_tris);
}

#[test]
fn preview_scratch_command_log_walk_terminates() {
    let mut scratch = preview_scratch()
        .lock()
        .expect("editor preview scratch mutex");
    scratch.used = 0;
    scratch.tex_used = 0;
    scratch.overlay_lines.clear();
    scratch.ot.clear();

    push_clear(&mut scratch, [0, 0, 0]);
    assert!(push_tri(
        &mut scratch,
        [
            Projected {
                sx: 0,
                sy: 0,
                sz: 256,
            },
            Projected {
                sx: 32,
                sy: 0,
                sz: 256,
            },
            Projected {
                sx: 0,
                sy: 32,
                sz: 256,
            },
        ],
        (255, 0, 0),
    ));

    let log = unsafe { psx_gpu_render::build_cmd_log(&scratch.ot) };
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].opcode, 0x02);
    // World triangles are Gouraud packets now (per-vertex baked light).
    assert_eq!(log[1].opcode, 0x30);
}

/// The preview must light brush vertices with the exact Draft-bake
/// formula: a vertex under the light bakes brighter than one at the
/// rim, and the emitted packet carries those distinct colours.
#[test]
fn brush_vertex_lighting_matches_the_draft_bake() {
    let light = psxed_project::brush_light::BrushPointLight {
        position: [0.0, 400.0, 0.0],
        radius: 1200.0,
        intensity_q8: 256,
        color: [255, 255, 255],
    };
    let near = psxed_project::brush_light::lit_point_color(
        [0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [128; 3],
        [32; 3],
        std::slice::from_ref(&light),
        &[],
    );
    let far = psxed_project::brush_light::lit_point_color(
        [1150.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [128; 3],
        [32; 3],
        std::slice::from_ref(&light),
        &[],
    );
    assert!(
        near[0] > far[0] + 40,
        "lambert + falloff must grade across a face: near {near:?}, far {far:?}"
    );
    // A solid slab between the light and the point kills the light,
    // leaving the vertex on pure ambient: the preview's shadows.
    let blocker =
        psxed_project::brush_light::brush_occluder_planes(&[psxed_project::brush::Brush::cuboid(
            [-512, 180, -512],
            [512, 240, 512],
        )]);
    let shadowed = psxed_project::brush_light::lit_point_color(
        [0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [128; 3],
        [32; 3],
        std::slice::from_ref(&light),
        &blocker,
    );
    assert_eq!(
        shadowed,
        [32, 32, 32],
        "an occluded vertex must bake flat ambient"
    );

    let mut scratch = preview_scratch()
        .lock()
        .expect("editor preview scratch mutex");
    scratch.used = 0;
    scratch.tex_used = 0;
    scratch.overlay_lines.clear();
    scratch.ot.clear();
    let tri = [
        Projected {
            sx: 0,
            sy: 0,
            sz: 256,
        },
        Projected {
            sx: 32,
            sy: 0,
            sz: 256,
        },
        Projected {
            sx: 0,
            sy: 32,
            sz: 256,
        },
    ];
    let colors = [
        (near[0], near[1], near[2]),
        (far[0], far[1], far[2]),
        (far[0], far[1], far[2]),
    ];
    assert!(push_tri_colors(&mut scratch, tri, colors));
    let packet = &scratch.tris[0];
    assert_eq!(packet.color0_cmd & 0xff, u32::from(near[0]));
    assert_eq!(packet.color1 & 0xff, u32::from(far[0]));
}

#[test]
fn sample_project_preview_frame_contains_draw_commands() {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root");
    // The committed miniaturised sample, not a local working project:
    // `editor/projects/*` is gitignored, so a test keyed on one passes only
    // on the machine that authored it and fails everywhere else, which is
    // how these went red after cortex_ignition_v1 was renamed.
    let project_root = repo_root.join("editor/samples/cortex_v1");
    let project =
        ProjectDocument::load_from_path(project_root.join("project.ron")).expect("project loads");
    let mut textures = crate::editor_textures::EditorTextures::new();
    textures.refresh(&project, &project_root);
    textures.refresh_models(&project, &project_root);
    let mut assets = crate::editor_assets::EditorAssets::new();
    assets.refresh(&project, &project_root);

    let camera = ViewportCameraState {
        mode: ViewportCameraMode::Orbit,
        yaw_q12: 320,
        pitch_q12: 300,
        radius: 8192,
        target: [2048, 512, 2048],
        position: [0, 0, 0],
    };
    let empty_hidden = std::collections::HashSet::new();
    let frame = super::build_phase1_frame(
        &project,
        camera,
        true,
        true,
        true,
        true,
        true,
        true,
        &empty_hidden,
        None,
        0,
        NodeId::ROOT,
        None,
        None,
        None,
        &[],
        &[],
        None,
        &[],
        None,
        &[],
        None,
        &textures,
        &assets,
    );

    let draw_count = frame
        .cmd_log
        .iter()
        .filter(|entry| matches!(entry.opcode, 0x20..=0x7F))
        .count();
    let mut translator = psx_gpu_render::Translator::new();
    let translated = translator.translate(&frame.cmd_log);
    let nonblack_vertices = translated
        .vertices
        .iter()
        .filter(|v| v.color[0] != 0 || v.color[1] != 0 || v.color[2] != 0)
        .count();
    assert!(
        draw_count > 0,
        "sample project preview should emit draw commands; opcodes={:?}",
        frame
            .cmd_log
            .iter()
            .map(|entry| entry.opcode)
            .collect::<Vec<_>>()
    );
    assert!(
        translated.total() > 0,
        "sample project preview should translate to vertices"
    );
    assert!(
        nonblack_vertices > 0,
        "sample project preview should contain visible non-black vertices"
    );

    if let Some(mut renderer) = headless_preview_renderer() {
        assert!(renderer.set_internal_scale(2, None));
        let vram = textures.vram_words();
        renderer.render_frame(&emulator_core::Gpu::new(), &frame.cmd_log, vram);
        let scale = renderer.internal_scale();
        let (_, _, rgba) = renderer.read_subrect_rgba8(0, 0, 320 * scale, 240 * scale);
        assert!(
            count_nonblack_rgba(&rgba) > 0,
            "sample project preview should render non-black pixels"
        );
    }
}

#[test]
fn brush_preview_emits_solid_faces() {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root");
    let project_root = repo_root.join("editor/samples/cortex_v1");
    let mut project =
        ProjectDocument::load_from_path(project_root.join("project.ron")).expect("project loads");
    let mut textures = crate::editor_textures::EditorTextures::new();
    textures.refresh(&project, &project_root);
    textures.refresh_models(&project, &project_root);
    let mut assets = crate::editor_assets::EditorAssets::new();
    assets.refresh(&project, &project_root);

    let camera = ViewportCameraState {
        mode: ViewportCameraMode::Orbit,
        yaw_q12: 320,
        pitch_q12: 300,
        radius: 8192,
        target: [2048, 512, 2048],
        position: [0, 0, 0],
    };
    let empty_hidden = std::collections::HashSet::new();
    let build = |project: &ProjectDocument,
                 textures: &crate::editor_textures::EditorTextures,
                 assets: &crate::editor_assets::EditorAssets| {
        super::build_phase1_frame(
            project,
            camera,
            true,
            true,
            true,
            true,
            true,
            true,
            &empty_hidden,
            None,
            0,
            NodeId::ROOT,
            None,
            None,
            None,
            &[],
            &[],
            None,
            &[],
            None,
            &[],
            None,
            textures,
            assets,
        )
    };
    let draw_count = |frame: &super::EditorPreviewFrame| {
        frame
            .cmd_log
            .iter()
            .filter(|entry| matches!(entry.opcode, 0x20..=0x7F))
            .count()
    };

    let baseline = draw_count(&build(&project, &textures, &assets));

    // A brush square in front of the camera target must add solid faces.
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [1536, 0, 1536],
            [2560, 384, 2560],
        ));
    let with_brush = draw_count(&build(&project, &textures, &assets));
    assert!(
        with_brush > baseline,
        "brush must add draw commands: baseline={baseline} with_brush={with_brush}"
    );
}

#[test]
fn solved_brush_front_sidedness_culls_a_camera_inside_the_solid() {
    let mut project = ProjectDocument::new("brush winding cull");
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [0, 0, 0],
            [512, 512, 512],
        ));
    let empty = ProjectDocument::new("brush winding cull");
    let textures = crate::editor_textures::EditorTextures::new();
    let assets = crate::editor_assets::EditorAssets::new();
    let hidden = std::collections::HashSet::new();
    let build = |project: &ProjectDocument, position| {
        super::build_phase1_frame(
            project,
            ViewportCameraState {
                mode: ViewportCameraMode::Free,
                yaw_q12: 3072,
                pitch_q12: 0,
                radius: 512,
                target: [0; 3],
                position,
            },
            false,
            false,
            false,
            false,
            false,
            false,
            &hidden,
            None,
            0,
            NodeId::ROOT,
            None,
            None,
            None,
            &[],
            &[],
            None,
            &[],
            None,
            &[],
            None,
            &textures,
            &assets,
        )
    };
    let draws = |frame: &super::EditorPreviewFrame| {
        frame
            .cmd_log
            .iter()
            .filter(|entry| matches!(entry.opcode, 0x20..=0x7f))
            .count()
    };

    let empty_draws = draws(&build(&empty, [256, 256, 256]));
    assert_eq!(
        draws(&build(&project, [256, 256, 256])),
        empty_draws,
        "an interior camera must see only back faces of a closed solid"
    );
    assert!(
        draws(&build(&project, [-512, 256, 256])) > empty_draws,
        "an exterior camera looking toward the solid must see front faces"
    );
}

#[test]
fn bsp_preview_lights_use_world_units_and_sector_scaled_radius() {
    let mut project = ProjectDocument::new("bsp-light-samples");
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [0, 0, 0],
            [256, 256, 256],
        ));
    let scene = project.active_scene_mut();
    scene.add_node(
        psxed_project::NodeId::ROOT,
        "Light",
        psxed_project::NodeKind::PointLight {
            color: [255, 240, 200],
            intensity: 1.0,
            radius: 4.0,
        },
    );
    if let Some(node) = scene
        .nodes()
        .iter()
        .find(|node| matches!(node.kind, psxed_project::NodeKind::PointLight { .. }))
        .map(|node| node.id)
        .and_then(|id| scene.node_mut(id))
    {
        node.transform.translation = [3840.0, 1280.0, 21120.0];
    }
    let hidden = std::collections::HashSet::new();
    let samples = super::room_geometry::collect_bsp_preview_lights(&project, &hidden);
    assert_eq!(samples.len(), 1);
    // Position is the raw world translation; radius scales by the World
    // sector size (1024), matching the bake and runtime records.
    assert_eq!(samples[0].position, [3840, 1280, 21120]);
    assert_eq!(samples[0].radius, 4096);
}

#[test]
fn legacy_textured_scene_is_fully_replaced_by_bsp_only_and_empty_scenes() {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let legacy_root = repo_root.join("editor/samples/cortex_v1");
    let legacy_project = ProjectDocument::load_from_path(legacy_root.join("project.ron"))
        .expect("load tracked legacy textured scene");
    let mut bsp_project = ProjectDocument::new("bsp-only-preview");
    bsp_project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [-512, 0, -512],
            [512, 512, 512],
        ));
    let empty_project = ProjectDocument::new("empty-preview");
    let mut textures = crate::editor_textures::EditorTextures::new();
    textures.refresh(&legacy_project, &legacy_root);
    textures.refresh_models(&legacy_project, &legacy_root);
    let mut assets = crate::editor_assets::EditorAssets::new();
    assets.refresh(&legacy_project, &legacy_root);
    let hidden = std::collections::HashSet::new();
    let camera = ViewportCameraState {
        mode: ViewportCameraMode::Orbit,
        yaw_q12: 512,
        pitch_q12: 320,
        radius: 2048,
        target: [0, 256, 0],
        position: [0; 3],
    };
    let build = |project: &ProjectDocument, camera| {
        super::build_phase1_frame(
            project,
            camera,
            false,
            false,
            false,
            false,
            false,
            false,
            &hidden,
            None,
            0,
            NodeId::ROOT,
            None,
            None,
            None,
            &[],
            &[],
            None,
            &[],
            None,
            &[],
            None,
            &textures,
            &assets,
        )
    };
    let legacy_camera = ViewportCameraState {
        mode: ViewportCameraMode::Orbit,
        yaw_q12: 320,
        pitch_q12: 300,
        radius: 8192,
        target: [2048, 512, 2048],
        position: [0; 3],
    };
    let legacy_frame = build(&legacy_project, legacy_camera);
    let bsp_frame = build(&bsp_project, camera);
    let empty_frame = build(&empty_project, camera);

    assert!(
        bsp_frame
            .cmd_log
            .iter()
            .any(|entry| matches!(entry.opcode, 0x20..=0x7f)),
        "a BSP-only project must submit brush draw commands"
    );
    assert!(
        empty_frame.cmd_log.iter().any(|entry| entry.opcode == 0x02),
        "an empty project must still clear the persistent preview target"
    );

    let Some(mut fresh_renderer) = headless_preview_renderer() else {
        panic!("no headless adapter");
    };
    let vram = textures.vram_words();
    fresh_renderer.render_frame(&emulator_core::Gpu::new(), &bsp_frame.cmd_log, vram);
    let scale = fresh_renderer.internal_scale();
    let (_, _, expected_bsp) = fresh_renderer.read_subrect_rgba8(0, 0, 320 * scale, 240 * scale);
    fresh_renderer.render_frame(&emulator_core::Gpu::new(), &empty_frame.cmd_log, vram);
    let (_, _, expected_empty) = fresh_renderer.read_subrect_rgba8(0, 0, 320 * scale, 240 * scale);

    let Some(mut reused_renderer) = headless_preview_renderer() else {
        panic!("no second headless adapter");
    };
    reused_renderer.render_frame(&emulator_core::Gpu::new(), &legacy_frame.cmd_log, vram);
    let (_, _, rendered_legacy) =
        reused_renderer.read_subrect_rgba8(0, 0, 320 * scale, 240 * scale);
    let legacy_pixels = rendered_legacy
        .chunks_exact(4)
        .zip(expected_empty.chunks_exact(4))
        .filter(|(legacy, empty)| legacy != empty)
        .count();
    assert!(
        legacy_pixels > 100,
        "legacy priming frame must visibly dirty the target, changed={legacy_pixels}"
    );
    reused_renderer.render_frame(&emulator_core::Gpu::new(), &bsp_frame.cmd_log, vram);
    let (_, _, bsp_after_legacy) =
        reused_renderer.read_subrect_rgba8(0, 0, 320 * scale, 240 * scale);
    assert_eq!(
        bsp_after_legacy, expected_bsp,
        "a BSP-only scene must replace every pixel of the prior legacy textured scene"
    );
    let changed_from_background = expected_bsp
        .chunks_exact(4)
        .zip(expected_empty.chunks_exact(4))
        .filter(|(bsp, empty)| bsp != empty)
        .count();
    assert!(
        changed_from_background > 100,
        "BSP frame must contain visible solid-face pixels, changed={changed_from_background}"
    );

    reused_renderer.render_frame(&emulator_core::Gpu::new(), &empty_frame.cmd_log, vram);
    let (_, _, empty_after_bsp) =
        reused_renderer.read_subrect_rgba8(0, 0, 320 * scale, 240 * scale);
    assert_eq!(
        empty_after_bsp, expected_empty,
        "opening an empty/BSP-less scene must not retain the old scene's pixels"
    );
}

#[test]
fn eroded_box_prop_preview_uses_generated_surface_mesh() {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root");
    let project_root = repo_root.join("editor/projects/default");
    let mut project = ProjectDocument::legacy_grid_starter();
    let material_id = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .expect("starter material")
        .id;
    let room = project
        .active_scene()
        .nodes()
        .iter()
        .find(|node| matches!(node.kind, NodeKind::Section { .. }))
        .expect("starter room");
    let room_id = room.id;
    let NodeKind::Section { grid } = &room.kind else {
        unreachable!();
    };
    let [target_x, _, target_z] = psxed_project::spatial::room_preview_center(grid);
    let prop_id = project.active_scene_mut().add_node(
        room_id,
        "Preview Broken Wall",
        NodeKind::BoxProp {
            materials: [Some(material_id); psxed_project::BOX_PROP_FACE_COUNT],
            uvs: [psxed_project::GridUvTransform::IDENTITY; psxed_project::BOX_PROP_FACE_COUNT],
            vertices: psxed_project::box_prop_vertices_for_size(512),
            collision_enabled: true,
            break_flags: 0,
            erosion: psxed_project::BoxPropErosion::default(),
        },
    );

    let mut textures = crate::editor_textures::EditorTextures::new();
    textures.refresh(&project, &project_root);
    let assets = crate::editor_assets::EditorAssets::new();
    let hidden = std::collections::HashSet::new();
    let camera = ViewportCameraState {
        mode: ViewportCameraMode::Orbit,
        yaw_q12: 512,
        pitch_q12: 256,
        radius: 2048,
        target: [target_x, 256, target_z],
        position: [0; 3],
    };
    let build_frame = |document: &ProjectDocument| {
        super::build_phase1_frame(
            document,
            camera,
            false,
            false,
            false,
            false,
            false,
            false,
            &hidden,
            Some(room_id),
            0,
            prop_id,
            None,
            None,
            None,
            &[],
            &[],
            None,
            &[],
            None,
            &[],
            None,
            &textures,
            &assets,
        )
    };
    let legacy_frame = build_frame(&project);

    let NodeKind::BoxProp { erosion, .. } = &mut project
        .active_scene_mut()
        .node_mut(prop_id)
        .expect("box prop")
        .kind
    else {
        unreachable!();
    };
    erosion.apply_broken_top_template();
    assert!(!psxed_project::generate_box_prop_erosion_quads(
        psxed_project::box_prop_vertices_for_size(512),
        *erosion,
    )
    .is_empty());
    let eroded_frame = build_frame(&project);

    let draw_count = |frame: &super::EditorPreviewFrame| {
        frame
            .cmd_log
            .iter()
            .filter(|entry| matches!(entry.opcode, 0x20..=0x7f))
            .count()
    };
    let legacy_draws = draw_count(&legacy_frame);
    let eroded_draws = draw_count(&eroded_frame);
    assert!(legacy_draws > 0);
    assert!(
        eroded_draws > legacy_draws,
        "generated low-poly surface must replace the six-face preview path; legacy={legacy_draws} eroded={eroded_draws}"
    );
}

#[test]
fn cylinder_prop_preview_renders_shared_generated_profile_headlessly() {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root");
    let project_root = repo_root.join("editor/projects/default");
    let mut project = ProjectDocument::legacy_grid_starter();
    let material_id = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .expect("starter material")
        .id;
    let room = project
        .active_scene()
        .nodes()
        .iter()
        .find(|node| matches!(node.kind, NodeKind::Section { .. }))
        .expect("starter room");
    let room_id = room.id;
    let NodeKind::Section { grid } = &room.kind else {
        unreachable!();
    };
    let [target_x, _, target_z] = psxed_project::spatial::room_preview_center(grid);
    let camera = ViewportCameraState {
        mode: ViewportCameraMode::Orbit,
        yaw_q12: 512,
        pitch_q12: 256,
        radius: 2048,
        target: [target_x, 512, target_z],
        position: [0; 3],
    };
    let hidden = std::collections::HashSet::new();
    let mut textures = crate::editor_textures::EditorTextures::new();
    textures.refresh(&project, &project_root);
    let assets = crate::editor_assets::EditorAssets::new();
    let build_frame = |document: &ProjectDocument, selected| {
        super::build_phase1_frame(
            document,
            camera,
            false,
            false,
            false,
            false,
            false,
            false,
            &hidden,
            Some(room_id),
            0,
            selected,
            None,
            None,
            None,
            &[],
            &[],
            None,
            &[],
            None,
            &[],
            None,
            &textures,
            &assets,
        )
    };
    let baseline = build_frame(&project, NodeId::ROOT);

    let mut geometry = psxed_project::CylinderPropGeometry {
        radius: [320, 320],
        height: 1024,
        ..Default::default()
    };
    geometry.base_bulge.enabled = true;
    geometry.top_bulge.enabled = true;
    geometry.broken_ends = psxed_project::CylinderBrokenEnds::Top;
    let expected_surfaces = psxed_project::generate_cylinder_prop_surfaces(geometry).len();
    let prop = project.active_scene_mut().add_node(
        room_id,
        "Preview Broken Column",
        NodeKind::CylinderProp {
            materials: [Some(material_id); psxed_project::CYLINDER_PROP_MATERIAL_COUNT],
            uvs: [psxed_project::GridUvTransform::IDENTITY;
                psxed_project::CYLINDER_PROP_MATERIAL_COUNT],
            geometry,
            collision_enabled: true,
        },
    );
    let frame = build_frame(&project, prop);
    let draw_count = |frame: &super::EditorPreviewFrame| {
        frame
            .cmd_log
            .iter()
            .filter(|entry| matches!(entry.opcode, 0x20..=0x7f))
            .count()
    };
    assert!(expected_surfaces > 18);
    assert!(
        draw_count(&frame) > draw_count(&baseline),
        "CylinderProp generated surfaces must reach the native headless preview"
    );
}

#[test]
fn sample_aletha_crystal_preview_uses_its_generated_texture() {
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root");
    // The committed miniaturised sample, not a local working project:
    // `editor/projects/*` is gitignored, so a test keyed on one passes only
    // on the machine that authored it and fails everywhere else, which is
    // how these went red after cortex_ignition_v1 was renamed.
    let project_root = repo_root.join("editor/samples/cortex_v1");
    let project =
        ProjectDocument::load_from_path(project_root.join("project.ron")).expect("project loads");
    let player_crystal = project
        .resources
        .iter()
        .find(|resource| resource.name == "Aletha Crystal")
        .expect("Aletha Crystal material exists");
    let mut textures = crate::editor_textures::EditorTextures::new();
    textures.refresh(&project, &project_root);

    assert!(
        textures.slot(player_crystal.id).is_some(),
        "generated Player Crystal texture should be uploaded to editor VRAM"
    );
    assert!(
        super::preview_model_material_override(&project, &textures, Some(player_crystal.id))
            .and_then(|material| material.texture)
            .is_some(),
        "model preview should use Player Crystal's generated texture instead of its checkerboard atlas"
    );

    // Exercise the complete editor render path without opening a window. Frame
    // the authored player, render once with Player Crystal and once with the
    // material deliberately changed to atlas fallback, then require the GPU
    // output to differ. This catches a preview regression even when both the
    // generated texture upload and the model atlas upload succeed independently.
    let scene = project.active_scene();
    let room = scene
        .nodes()
        .iter()
        .find(|node| node.name == "Demo7 Map" && matches!(node.kind, NodeKind::Section { .. }))
        .expect("Demo7 Map room exists");
    let NodeKind::Section { grid } = &room.kind else {
        unreachable!();
    };
    let player = scene
        .nodes()
        .iter()
        .find(|node| {
            node.name == "Aletha" && super::preview_player_reference(scene, node).is_some()
        })
        .expect("authored player exists");
    let reference = super::preview_player_reference(scene, player).expect("player reference");
    let character_id = super::resolve_player_spawn_character(&project, reference.character)
        .expect("player character resolves");
    let character = project
        .resource(character_id)
        .and_then(|resource| match &resource.data {
            ResourceData::Character(character) => Some(character),
            _ => None,
        })
        .expect("player Character resource");
    let model_id = reference
        .model_override
        .or(character.model)
        .expect("player model resolves");
    let placement = super::floor_anchored_node_room_local_origin(grid, &player.transform);
    let player_rotation = super::yaw_rotation_q12(
        super::yaw_to_q12(player.transform.rotation_degrees[1])
            .wrapping_add(reference.visual_yaw_q12),
    );
    let mut assets = crate::editor_assets::EditorAssets::new();
    assets.refresh(&project, &project_root);
    // Mirror the production origin exactly: the lift comes from the cooked
    // mesh's own bind pose, not the authored world height, and travels through
    // the same local-to-world the mesh is drawn with.
    let mesh_bytes = assets
        .mesh_bytes(model_id)
        .expect("player mesh is cooked")
        .to_vec();
    let mesh = psx_asset::Model::from_bytes(&mesh_bytes).expect("player mesh parses");
    let render_origin = super::visual_model_origin(
        placement,
        mesh.bind_pose_floor_lift(),
        super::preview_local_to_world(&mesh, reference.visual_scale_q8),
        reference.visual_offset,
        player_rotation,
    );
    let camera = ViewportCameraState {
        mode: ViewportCameraMode::Orbit,
        yaw_q12: 512,
        pitch_q12: 256,
        radius: 2200,
        target: [render_origin.x, render_origin.y, render_origin.z],
        position: [0; 3],
    };
    let empty_hidden = std::collections::HashSet::new();
    let build_frame =
        |document: &ProjectDocument, texture_cache: &crate::editor_textures::EditorTextures| {
            super::build_phase1_frame(
                document,
                camera,
                false,
                false,
                false,
                false,
                false,
                false,
                &empty_hidden,
                Some(room.id),
                0,
                NodeId::ROOT,
                None,
                None,
                None,
                &[],
                &[],
                None,
                &[],
                None,
                &[],
                None,
                texture_cache,
                &assets,
            )
        };
    let generated_frame = build_frame(&project, &textures);

    let mut atlas_fallback_project = project.clone();
    let fallback_material = atlas_fallback_project
        .resource_mut(player_crystal.id)
        .and_then(|resource| match &mut resource.data {
            ResourceData::Material(material) => Some(material),
            _ => None,
        })
        .expect("mutable Player Crystal material");
    fallback_material.texture_mode = psxed_project::MaterialTextureMode::SimpleImage;
    fallback_material.psxt_path = None;
    let mut fallback_textures = crate::editor_textures::EditorTextures::new();
    fallback_textures.refresh(&atlas_fallback_project, &project_root);
    fallback_textures.refresh_models(&atlas_fallback_project, &project_root);
    let fallback_frame = build_frame(&atlas_fallback_project, &fallback_textures);

    if let Some(mut renderer) = headless_preview_renderer() {
        let render = |renderer: &mut psx_gpu_render::HwRenderer,
                      frame: &super::EditorPreviewFrame,
                      texture_cache: &crate::editor_textures::EditorTextures| {
            renderer.render_frame(
                &emulator_core::Gpu::new(),
                &frame.cmd_log,
                texture_cache.vram_words(),
            );
            let scale = renderer.internal_scale();
            renderer
                .read_subrect_rgba8(0, 0, 320 * scale, 240 * scale)
                .2
        };
        let generated_rgba = render(&mut renderer, &generated_frame, &textures);
        let fallback_rgba = render(&mut renderer, &fallback_frame, &fallback_textures);
        let changed_pixels = generated_rgba
            .chunks_exact(4)
            .zip(fallback_rgba.chunks_exact(4))
            .filter(|(generated, fallback)| generated != fallback)
            .count();
        assert!(
            changed_pixels > 100,
            "headless editor frame should visibly change when Player Crystal falls back to the checkerboard atlas; changed pixels={changed_pixels}"
        );
    }
}

fn fog(rgb: (u8, u8, u8), near: i32, far: i32) -> PreviewFog {
    PreviewFog {
        enabled: true,
        rgb,
        near,
        far,
    }
}

#[test]
fn image_prop_overlay_rotation_applies_roll() {
    let rotated = rotate_image_prop_local([0, 512, 0], [0, 0, 1024]);
    assert_eq!(rotated, [-512, 0, 0]);
}

#[test]
fn image_prop_overlay_rotation_applies_yaw_q12_quarter_turn() {
    let rotated = rotate_image_prop_local([512, 0, 0], [0, 1024, 0]);
    assert_eq!(rotated, [0, 0, -512]);
}

#[test]
fn model_preview_yaw_matrix_uses_q12_quarter_turn() {
    let rotation = yaw_rotation_q12(1024);
    assert_eq!(rotation.m, [[0, 0, 4096], [0, 4096, 0], [-4096, 0, 0]]);
}

#[test]
fn model_preview_euler_matrix_applies_pitch_and_roll() {
    // Yaw-only inputs keep the exact single-axis build.
    assert_eq!(euler_rotation_q12(0, 1024, 0).m, yaw_rotation_q12(1024).m);
    // A quarter-turn pitch must move the model's local Y toward
    // world Z (column 1 of Rx(90 deg)); the old yaw-only preview
    // rendered this as identity, leaving pitched props upright.
    let pitched = euler_rotation_q12(1024, 0, 0);
    let col_y = [pitched.m[0][1], pitched.m[1][1], pitched.m[2][1]];
    assert_eq!(col_y, [0, 0, 4096]);
    // And the composition order matches the runtime's
    // `euler_q12_rotation` (Rz * Ry * Rx).
    let composed = euler_rotation_q12(512, 1024, 256);
    let rx = Mat3I16::rotate_x(psx_engine::Angle::from_q12(512).rotate_y_arg());
    let ry = Mat3I16::rotate_y(psx_engine::Angle::from_q12(1024).rotate_y_arg());
    let rz = Mat3I16::rotate_z(psx_engine::Angle::from_q12(256).rotate_y_arg());
    assert_eq!(composed.m, rz.mul(&ry).mul(&rx).m);
}

fn projected(sx: i16, sy: i16) -> Projected {
    Projected { sx, sy, sz: 100 }
}

#[test]
fn textured_preview_uses_authored_material_tint() {
    let mut project = ProjectDocument::new("test");
    let mut material = MaterialResource::opaque(Some("brick.psxt".to_string()));
    material.tint = [0x60, 0x70, 0x90];
    let material = project.add_resource("Brick Wall", ResourceData::Material(material));

    assert_eq!(
        material_texture_tint(&project, material),
        (0x60, 0x70, 0x90)
    );
}

#[test]
fn wall_preview_uvs_use_runtime_grid_tile_span() {
    let transform = GridUvTransform {
        span: [0, 128],
        ..GridUvTransform::IDENTITY
    };

    assert_eq!(
        transform.apply_to_quad(PREVIEW_WALL_UVS),
        [(0, 128), (64, 128), (64, 0), (0, 0)]
    );
}

#[test]
fn horizontal_preview_points_use_triangle_local_heights() {
    let corners = psxed_project::horizontal_triangle_corners(GridSplit::NorthWestSouthEast, 0);
    assert_eq!(corners, [Corner::NW, Corner::NE, Corner::SE]);
    assert_eq!(
        horizontal_triangle_world_points([0, 1024, 0, 1024], corners, [64, 128, 192]),
        [[0, 64, 1024], [1024, 128, 1024], [1024, 192, 0]]
    );
}

#[test]
fn portal_seam_runs_coalesce_connected_edges() {
    let runs = super::portal_seam_runs(vec![
        PortalEdge {
            x: 2,
            z: 0,
            direction: GridDirection::North,
        },
        PortalEdge {
            x: 0,
            z: 0,
            direction: GridDirection::North,
        },
        PortalEdge {
            x: 1,
            z: 0,
            direction: GridDirection::North,
        },
        PortalEdge {
            x: 4,
            z: 2,
            direction: GridDirection::East,
        },
        PortalEdge {
            x: 4,
            z: 3,
            direction: GridDirection::East,
        },
    ]);

    assert_eq!(
        runs,
        vec![
            super::PortalSeamRun {
                start: PortalEdge {
                    x: 0,
                    z: 0,
                    direction: GridDirection::North,
                },
                len: 3,
            },
            super::PortalSeamRun {
                start: PortalEdge {
                    x: 4,
                    z: 2,
                    direction: GridDirection::East,
                },
                len: 2,
            },
        ]
    );
}

#[test]
fn portal_edge_height_span_uses_real_adjacent_geometry() {
    let mut grid = WorldGrid::empty(2, 1, 2048);
    grid.set_floor(0, 0, -128, None);
    grid.set_floor(1, 0, 64, None);
    grid.ensure_sector(1, 0)
        .unwrap()
        .walls
        .east
        .push(GridVerticalFace::flat(0, 3328, None));

    let corners = super::portal_edge_wall_corners_for_world_cell(&grid, 0, 0, GridDirection::East)
        .expect("portal wall corners");

    assert_eq!(corners[0][1], -104);
    assert_eq!(corners[1][1], -104);
    assert_eq!(corners[2][1], 3328);
    assert_eq!(corners[3][1], 3328);
}

#[test]
fn visible_room_grids_keeps_all_non_hidden_rooms() {
    let mut project = ProjectDocument::new("test");
    let scene = project.active_scene_mut();
    let room_a = scene.add_node(
        scene.root,
        "Room A",
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let room_b = scene.add_node(
        scene.root,
        "Room B",
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );

    let mut hidden = std::collections::HashSet::new();
    let visible = super::visible_room_grids(&project, &hidden)
        .into_iter()
        .map(|(id, _)| id)
        .collect::<Vec<_>>();
    assert_eq!(visible, vec![room_a, room_b]);

    hidden.insert(room_a);
    let visible = super::visible_room_grids(&project, &hidden)
        .into_iter()
        .map(|(id, _)| id)
        .collect::<Vec<_>>();
    assert_eq!(visible, vec![room_b]);
}

#[test]
fn preview_room_grids_walks_bounded_portal_neighborhood() {
    let mut project = ProjectDocument::new("test");
    let scene = project.active_scene_mut();
    let room_a = scene.add_node(
        scene.root,
        "Room A",
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let room_b = scene.add_node(
        scene.root,
        "Room B",
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let room_c = scene.add_node(
        scene.root,
        "Room C",
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let room_d = scene.add_node(
        scene.root,
        "Room D",
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let room_e = scene.add_node(
        scene.root,
        "Room E",
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    let room_far = scene.add_node(
        scene.root,
        "Room Far",
        NodeKind::Section {
            grid: WorldGrid::empty(1, 1, 1024),
        },
    );
    for (source, target) in [
        (room_a, room_b),
        (room_b, room_c),
        (room_c, room_d),
        (room_d, room_e),
    ] {
        scene.add_node(
            source,
            "Portal",
            NodeKind::Portal {
                target_room: Some(target),
                target_entry: String::new(),
                entry_name: String::new(),
                geometry: None,
            },
        );
    }

    let hidden = std::collections::HashSet::new();
    let visible = super::preview_room_grids(
        &project,
        &hidden,
        Some(room_a),
        0,
        NodeId::ROOT,
        None,
        &[],
        &[],
    )
    .into_iter()
    .map(|entry| entry.room)
    .collect::<Vec<_>>();

    assert_eq!(visible, vec![room_a, room_b, room_c, room_d]);
    assert!(!visible.contains(&room_e));
    assert!(!visible.contains(&room_far));
}

/// Sims-style floor view: the active floor is the working plane at
/// y_offset 0 (so the ray pick, which tests the active floor's
/// floor-local faces, aligns with what's drawn), floors BELOW render
/// descending (negative offset), and floors ABOVE are hidden. This is
/// what keeps selection on the floor you're editing.
#[test]
fn preview_room_grids_shows_active_floor_and_below_only() {
    let mut project = ProjectDocument::new("test");
    let scene = project.active_scene_mut();
    let mut grid = WorldGrid::empty(1, 1, 1024);
    grid.push_floor(); // floor 1
    grid.push_floor(); // floor 2
    let room = scene.add_node(scene.root, "Stacked", NodeKind::Section { grid });

    let hidden = std::collections::HashSet::new();
    // Active floor = 1 (middle of three).
    let floors = super::preview_room_grids(
        &project,
        &hidden,
        Some(room),
        1,
        NodeId::ROOT,
        None,
        &[],
        &[],
    );
    let entries: Vec<(usize, i32, bool)> = floors
        .iter()
        .filter(|f| f.room == room)
        .map(|f| (f.floor_index, f.y_offset, f.active))
        .collect();

    // Floors 0 and 1 only (the active floor and the one below); floor
    // 2 (above) is hidden.
    assert_eq!(entries.len(), 2, "active + below only: {entries:?}");
    assert!(
        entries
            .iter()
            .any(|&(i, off, act)| i == 1 && off == 0 && act),
        "active floor 1 at y_offset 0: {entries:?}"
    );
    let below = entries
        .iter()
        .find(|&&(i, _, _)| i == 0)
        .expect("floor 0 below");
    assert!(
        below.1 < 0,
        "floor below descends (negative y_offset): {below:?}"
    );
    assert!(
        !entries.iter().any(|&(i, _, _)| i == 2),
        "floor above hidden"
    );
}

#[test]
fn component_model_reference_reads_renderer_and_animator_children() {
    let mut project = ProjectDocument::new("test");
    let model_id = project.add_resource(
        "Dummy",
        ResourceData::Texture {
            psxt_path: "dummy.psxt".to_string(),
        },
    );
    let material_id = project.add_resource(
        "Crystal",
        ResourceData::Material(psxed_project::MaterialResource::translucent(
            None,
            psxed_project::PsxBlendMode::Average,
        )),
    );
    let scene = project.active_scene_mut();
    let actor = scene.add_node(scene.root, "Enemy", NodeKind::Entity);
    let renderer = scene.add_node(
        actor,
        "Model Renderer",
        NodeKind::ModelRenderer {
            model: Some(model_id),
            material: Some(material_id),
            visual_offset: [0; 3],
            visual_scale_q8: psxed_project::MODEL_SCALE_ONE_Q8,
        },
    );
    let animator = scene.add_node(
        actor,
        "Animator",
        NodeKind::Animator {
            clip: Some(3),
            action_clips: Vec::new(),
            autoplay: true,
            pose_frame: 0,
        },
    );

    let scene = project.active_scene();
    let reference = preview_model_reference(scene, scene.node(actor).unwrap()).unwrap();

    assert_eq!(reference.model_id, model_id);
    assert_eq!(reference.material_override, Some(material_id));
    assert_eq!(reference.clip_override, Some(3));
    assert!(reference.autoplay);
    assert_eq!(reference.renderer_node, Some(renderer));
    assert_eq!(reference.animator_node, Some(animator));
}

#[test]
fn component_player_reference_reads_controller_renderer_and_animator_children() {
    let mut project = ProjectDocument::new("test");
    let character_id = project.add_resource(
        "Dummy",
        ResourceData::Texture {
            psxt_path: "dummy.psxt".to_string(),
        },
    );
    let material_id = project.add_resource(
        "Crystal",
        ResourceData::Material(psxed_project::MaterialResource::translucent(
            None,
            psxed_project::PsxBlendMode::Average,
        )),
    );
    let scene = project.active_scene_mut();
    let actor = scene.add_node(scene.root, "Player", NodeKind::Entity);
    let controller = scene.add_node(
        actor,
        "Character Controller",
        NodeKind::CharacterController {
            character: Some(character_id),
            player: true,
            settings: Default::default(),
        },
    );
    let renderer = scene.add_node(
        actor,
        "Model Renderer",
        NodeKind::ModelRenderer {
            model: None,
            material: Some(material_id),
            visual_offset: [0; 3],
            visual_scale_q8: psxed_project::MODEL_SCALE_ONE_Q8,
        },
    );
    let animator = scene.add_node(
        actor,
        "Animator",
        NodeKind::Animator {
            clip: Some(2),
            action_clips: Vec::new(),
            autoplay: false,
            pose_frame: 0,
        },
    );

    let scene = project.active_scene();
    let reference = preview_player_reference(scene, scene.node(actor).unwrap()).unwrap();

    assert_eq!(reference.character, Some(character_id));
    assert_eq!(reference.material_override, Some(material_id));
    assert_eq!(reference.controller_node, Some(controller));
    assert_eq!(reference.renderer_node, Some(renderer));
    assert_eq!(reference.animator_node, Some(animator));
    assert_eq!(reference.clip_override, Some(2));
    assert!(!reference.autoplay);
}

#[test]
fn player_controlled_entity_does_not_static_preview_model_renderer() {
    let mut project = ProjectDocument::new("test");
    let model_id = project.add_resource(
        "Dummy Model",
        ResourceData::Texture {
            psxt_path: "dummy.psxt".to_string(),
        },
    );
    let character_id = project.add_resource(
        "Dummy Character",
        ResourceData::Texture {
            psxt_path: "dummy-character.psxt".to_string(),
        },
    );
    let scene = project.active_scene_mut();
    let actor = scene.add_node(scene.root, "Player", NodeKind::Entity);
    scene.add_node(
        actor,
        "Model Renderer",
        NodeKind::ModelRenderer {
            model: Some(model_id),
            material: None,
            visual_offset: [0; 3],
            visual_scale_q8: psxed_project::MODEL_SCALE_ONE_Q8,
        },
    );
    scene.add_node(
        actor,
        "Character Controller",
        NodeKind::CharacterController {
            character: Some(character_id),
            player: true,
            settings: Default::default(),
        },
    );

    let scene = project.active_scene();
    let actor_node = scene.node(actor).unwrap();
    assert!(
        preview_model_reference(scene, actor_node).is_some(),
        "the raw renderer reference is still present"
    );
    assert!(
        preview_static_model_reference(scene, actor_node).is_none(),
        "player-controlled renderers are drawn by the player preview path"
    );
}

#[test]
fn point_light_uses_own_transform() {
    let mut project = ProjectDocument::new("test");
    let scene = project.active_scene_mut();
    let host = scene.add_node(scene.root, "Lamp", NodeKind::Entity);
    scene.node_mut(host).unwrap().transform.translation = [2.0, 0.5, 3.0];
    let light = scene.add_node(
        scene.root,
        "Point Light",
        NodeKind::PointLight {
            color: [1, 2, 3],
            intensity: 0.75,
            radius: 4.0,
        },
    );
    scene.node_mut(light).unwrap().transform.translation = [99.0, 99.0, 99.0];

    let hidden = std::collections::HashSet::new();
    let lights = preview_lights(project.active_scene(), &hidden);

    assert_eq!(lights.len(), 1);
    assert_eq!(lights[0].host_id, light);
    assert_eq!(lights[0].transform.translation, [99.0, 99.0, 99.0]);
    assert_eq!(lights[0].color, [1, 2, 3]);
    assert_eq!(lights[0].intensity, 0.75);
    assert_eq!(lights[0].radius, 4.0);
}

#[test]
fn face_sidedness_matches_runtime_winding_convention() {
    let front = [projected(0, 0), projected(10, 0), projected(0, 10)];
    let back = [front[0], front[2], front[1]];

    assert!(face_side_visible(MaterialFaceSidedness::Front, front));
    assert!(!face_side_visible(MaterialFaceSidedness::Front, back));
    assert!(!face_side_visible(MaterialFaceSidedness::Back, front));
    assert!(face_side_visible(MaterialFaceSidedness::Back, back));
    assert!(face_side_visible(MaterialFaceSidedness::Both, front));
    assert!(face_side_visible(MaterialFaceSidedness::Both, back));
}

#[test]
fn preview_rejects_triangles_outside_psx_packet_extent() {
    assert!(preview_projected_triangle_hw_safe([
        projected(0, 0),
        projected(64, 0),
        projected(0, 64),
    ]));
    assert!(!preview_projected_triangle_hw_safe([
        projected(-1024, 0),
        projected(1023, 0),
        projected(0, 16),
    ]));
    assert!(!preview_projected_triangle_hw_safe([
        projected(0, -512),
        projected(16, 512),
        projected(0, 0),
    ]));
}

#[test]
fn editor_cardinal_wall_front_material_renders_from_owning_cell() {
    let cases = [
        (WallEdge::North, [512, 512, 512], 2048, [512, 512, 1536], 0),
        (
            WallEdge::East,
            [512, 512, 512],
            3072,
            [1536, 512, 512],
            1024,
        ),
        (WallEdge::South, [512, 512, 512], 0, [512, 512, -512], 2048),
        (
            WallEdge::West,
            [512, 512, 512],
            1024,
            [-512, 512, 512],
            3072,
        ),
    ];

    for (edge, inside_pos, inside_yaw, outside_pos, outside_yaw) in cases {
        assert!(
            wall_face_emits_from_camera(edge, inside_pos, inside_yaw, MaterialFaceSidedness::Front),
            "{edge:?} wall front material should render from inside the owning cell"
        );
        assert!(
            !wall_face_emits_from_camera(edge, inside_pos, inside_yaw, MaterialFaceSidedness::Back),
            "{edge:?} wall back material should not render from inside the owning cell"
        );
        assert!(
            !wall_face_emits_from_camera(
                edge,
                outside_pos,
                outside_yaw,
                MaterialFaceSidedness::Front
            ),
            "{edge:?} wall front material should not render from outside the owning cell"
        );
        assert!(
            wall_face_emits_from_camera(
                edge,
                outside_pos,
                outside_yaw,
                MaterialFaceSidedness::Back
            ),
            "{edge:?} wall back material should render from outside the owning cell"
        );
    }
}

#[test]
fn editor_diagonal_wall_materials_are_forced_double_sided() {
    for edge in [WallEdge::NorthWestSouthEast, WallEdge::NorthEastSouthWest] {
        assert_eq!(
            wall_material_sidedness_for_edge(MaterialFaceSidedness::Front, edge),
            MaterialFaceSidedness::Both
        );
        assert_eq!(
            wall_material_sidedness_for_edge(MaterialFaceSidedness::Back, edge),
            MaterialFaceSidedness::Both
        );
        assert!(wall_side_visible(
            MaterialFaceSidedness::Front,
            [0, 1024, 0, 1024],
            edge,
            [256, 512, 256]
        ));
        assert!(wall_side_visible(
            MaterialFaceSidedness::Back,
            [0, 1024, 0, 1024],
            edge,
            [768, 512, 768]
        ));
    }
}

fn wall_face_emits_from_camera(
    edge: WallEdge,
    position: [i32; 3],
    yaw_q12: u16,
    sidedness: MaterialFaceSidedness,
) -> bool {
    let camera = setup_gte_for_camera(ViewportCameraState {
        mode: ViewportCameraMode::Free,
        yaw_q12,
        pitch_q12: 0,
        radius: 1024,
        target: [512, 512, 512],
        position,
    });
    let mut scratch = preview_scratch()
        .lock()
        .expect("editor preview scratch mutex");
    scratch.used = 0;
    scratch.tex_used = 0;
    scratch.overlay_lines.clear();
    scratch.ot.clear();

    push_wall_face(
        &mut scratch,
        camera,
        [0, 1024, 0, 1024],
        edge,
        [384, 384, 640, 640],
        None,
        GridUvTransform::default(),
        flat_sided(128, 128, 128, sidedness),
        position,
    );

    scratch.used > 0 || scratch.tex_used > 0
}

#[test]
fn preview_near_guard_rejects_vertices_behind_camera() {
    let camera = setup_gte_for_camera(ViewportCameraState {
        mode: ViewportCameraMode::Free,
        yaw_q12: 0,
        pitch_q12: 0,
        radius: 1024,
        target: [0, 0, 0],
        position: [0, 0, 0],
    });

    assert!(preview_vertices_in_front(
        camera,
        &[[0, 0, -64], [16, 0, -64]]
    ));
    assert!(!preview_vertices_in_front(
        camera,
        &[[0, 0, -64], [0, 0, 16]]
    ));
}

#[test]
fn culled_room_face_outline_respects_preview_toggle() {
    assert!(should_draw_culled_face_outline(
        true,
        flat_sided(128, 128, 128, MaterialFaceSidedness::Front)
    ));
    assert!(should_draw_culled_face_outline(
        true,
        flat_sided(128, 128, 128, MaterialFaceSidedness::Back)
    ));
    assert!(!should_draw_culled_face_outline(
        false,
        flat_sided(128, 128, 128, MaterialFaceSidedness::Back)
    ));
    assert!(!should_draw_culled_face_outline(
        true,
        flat_sided(128, 128, 128, MaterialFaceSidedness::Both)
    ));
}

#[test]
fn preview_depth_slots_share_world_geometry_band() {
    assert_eq!(room_depth_slot(0), PREVIEW_GEOMETRY_SLOT_MIN);
    assert_eq!(room_depth_slot(u32::MAX), PREVIEW_GEOMETRY_SLOT_MAX);
    assert!(shadow_depth_slot(2048) < room_depth_slot(2048));
    assert_eq!(shadow_depth_slot(0), PREVIEW_GEOMETRY_SLOT_MIN);
    assert_eq!(PREVIEW_SHADOW_DEPTH_BIAS, 128);
}

#[test]
fn preview_shadow_radius_matches_runtime_scale() {
    assert_eq!(preview_shadow_radius(1), PREVIEW_SHADOW_RADIUS_MIN);
    assert_eq!(preview_shadow_radius(2048), PREVIEW_SHADOW_RADIUS_MAX);
    assert_eq!(preview_shadow_radius(200), 250);
}

#[test]
fn node_room_local_origin_matches_origin_aware_grid_conversion() {
    let mut grid = WorldGrid::stone_room(4, 7, 1024, None, None);
    grid.origin = [-1, -3];
    let translation = [1.0, 0.25, 0.85];
    let transform = psxed_project::Transform3 {
        translation,
        ..psxed_project::Transform3::default()
    };

    let origin = node_room_local_origin(&grid, &transform);
    let expected = grid.editor_to_room_local([translation[0], translation[2]]);

    assert_eq!(origin.x, expected[0] as i32);
    assert_eq!(origin.y, 256);
    assert_eq!(origin.z, expected[2] as i32);
    assert_ne!(
        (origin.x, origin.z),
        (
            ((translation[0] + grid.width as f32 * 0.5) * grid.sector_size as f32) as i32,
            ((translation[2] + grid.depth as f32 * 0.5) * grid.sector_size as f32) as i32,
        ),
        "regression guard: old half-grid-only conversion ignores grid.origin"
    );
}

// These two replace a pair that pinned `floor_anchored_model_origin`, which
// lifted a model by HALF its authored world height. That function was deleted
// on purpose: the half-height lift was the actor-grounding float, and models
// now sit on their bind pose's own origin-to-feet distance instead. The cases
// worth keeping are the same two, aimed at the rule that replaced it.

#[test]
fn visual_model_origin_lifts_by_the_bind_pose_floor_lift() {
    let origin = super::visual_model_origin(
        WorldVertex::new(10, 0, 20),
        512,
        psx_engine::LocalToWorldScale::IDENTITY,
        [0; 3],
        Mat3I16::IDENTITY,
    );
    assert_eq!(origin, WorldVertex::new(10, 512, 20));
}

#[test]
fn visual_model_origin_ignores_a_negative_floor_lift() {
    // A mesh whose lowest vertex sits above its origin must not be pushed
    // INTO the floor.
    let origin = super::visual_model_origin(
        WorldVertex::new(10, 32, 20),
        -128,
        psx_engine::LocalToWorldScale::IDENTITY,
        [0; 3],
        Mat3I16::IDENTITY,
    );
    assert_eq!(origin, WorldVertex::new(10, 32, 20));
}

#[test]
fn visual_model_origin_scales_the_lift_with_the_mesh() {
    // The lift is in MODEL units, so it has to travel through the same
    // local-to-world the mesh is drawn with. Applying it unscaled is what
    // floats a shrunken actor off the floor.
    let origin = super::visual_model_origin(
        WorldVertex::new(0, 0, 0),
        512,
        psx_engine::LocalToWorldScale::from_q12(2048),
        [0; 3],
        Mat3I16::IDENTITY,
    );
    assert_eq!(origin, WorldVertex::new(0, 256, 0));
}

#[test]
fn preview_fog_blends_after_near_plane() {
    let fog = fog((10, 20, 30), 100, 300);

    assert_eq!(fog.apply_rgb((110, 120, 130), 100), (110, 120, 130));
    assert_eq!(fog.apply_rgb((110, 120, 130), 200), (60, 70, 80));
    assert_eq!(fog.apply_rgb((110, 120, 130), 300), (10, 20, 30));
    assert_eq!(fog.apply_rgb((110, 120, 130), 900), (10, 20, 30));
}

#[test]
fn preview_fog_applies_to_flat_and_textured_tints() {
    let fog = fog((0, 0, 0), 0, 256);
    let flat = fog.apply_shade(flat(128, 64, 32), 128);
    let textured = fog.apply_shade(
        FaceShade::Textured {
            slot: MaterialSlot {
                tpage_word: 0,
                clut_word: 0,
                texture_window: psx_gpu::material::TextureWindow::NONE,
                texture_width: 64,
                texture_height: 64,
            },
            tint: (128, 64, 32),
            blend_mode: BlendMode::Opaque,
            sidedness: psxed_project::MaterialFaceSidedness::Front,
        },
        128,
    );

    assert_eq!(unpack(flat), (64, 32, 16));
    assert_eq!(unpack(textured), (64, 32, 16));
}

#[test]
fn material_sized_uvs_stretch_32px_texture_once_by_default() {
    let shade = FaceShade::Textured {
        slot: MaterialSlot {
            tpage_word: 0,
            clut_word: 0,
            texture_window: psx_gpu::material::TextureWindow::NONE,
            texture_width: 32,
            texture_height: 32,
        },
        tint: (128, 128, 128),
        blend_mode: BlendMode::Opaque,
        sidedness: psxed_project::MaterialFaceSidedness::Both,
    };
    assert_eq!(
        material_sized_uvs(shade, PREVIEW_FLOOR_UVS),
        [(0, 0), (32, 0), (32, 32), (0, 32)]
    );
}

#[test]
fn material_sized_uvs_preserve_authored_repeat_count() {
    let shade = FaceShade::Textured {
        slot: MaterialSlot {
            tpage_word: 0,
            clut_word: 0,
            texture_window: psx_gpu::material::TextureWindow::NONE,
            texture_width: 32,
            texture_height: 64,
        },
        tint: (128, 128, 128),
        blend_mode: BlendMode::Opaque,
        sidedness: psxed_project::MaterialFaceSidedness::Both,
    };
    assert_eq!(
        material_sized_uvs(
            shade,
            [(0, 0), (128, 0), (128, GRID_TILE_UV), (0, GRID_TILE_UV)]
        ),
        [(0, 0), (64, 0), (64, GRID_TILE_UV), (0, GRID_TILE_UV)]
    );
}

#[test]
fn light_face_no_lights_ambient_32_is_not_white() {
    // Regression: pre-fix the `ambient * 256` bug saturated
    // every face to 255. With the new convention an unlit
    // face at ambient 32 should render at ~32, not white.
    let base = flat(128, 128, 128);
    let lit = light_face(base, [0, 0, 0], &[], [32, 32, 32]);
    let (r, g, b) = unpack(lit);
    assert!(r < 64 && g < 64 && b < 64, "got ({r}, {g}, {b})");
}

#[test]
fn light_face_ambient_128_is_neutral() {
    // 128 ambient is the neutral PSX-tint value; an unlit
    // 128-base material should land back at exactly 128.
    let lit = light_face(flat(128, 128, 128), [0, 0, 0], &[], [128, 128, 128]);
    assert_eq!(unpack(lit), (128, 128, 128));
}

#[test]
fn light_face_zero_ambient_zero_lights_black() {
    let lit = light_face(flat(255, 255, 255), [0, 0, 0], &[], [0, 0, 0]);
    assert_eq!(unpack(lit), (0, 0, 0));
}

#[test]
fn light_face_point_light_inside_radius_brightens() {
    // White light at the face centre with neutral base
    // should land at saturating-bright since contribution
    // (255 × 256 × 256) / 65536 = 255 dominates ambient.
    let light = PointLightSample::from_color_intensity_q8([0, 0, 0], 100, [255, 255, 255], 256);
    let lit = light_face(flat(128, 128, 128), [0, 0, 0], &[light], [32, 32, 32]);
    let (r, g, b) = unpack(lit);
    assert!(r > 200 && g > 200 && b > 200, "got ({r}, {g}, {b})");
}

#[test]
fn light_face_point_light_outside_radius_zero() {
    // Place the face well outside the light's radius; the
    // contribution must be exactly zero. Output should
    // match the no-lights case.
    let light = PointLightSample::from_color_intensity_q8([0, 0, 0], 100, [255, 255, 255], 256);
    let lit = light_face(flat(128, 128, 128), [10000, 0, 0], &[light], [32, 32, 32]);
    let baseline = light_face(flat(128, 128, 128), [10000, 0, 0], &[], [32, 32, 32]);
    assert_eq!(unpack(lit), unpack(baseline));
}

#[test]
fn light_face_two_lights_accumulate_and_clamp() {
    let l = PointLightSample::from_color_intensity_q8([0, 0, 0], 100, [255, 255, 255], 256);
    let lit = light_face(flat(255, 255, 255), [0, 0, 0], &[l, l], [128, 128, 128]);
    let (r, g, b) = unpack(lit);
    // Even with two saturating lights, output never
    // exceeds 255 per channel.
    assert_eq!((r, g, b), (255, 255, 255));
}

// Temporary end-to-end repro for the flipped-normal report: import the
// suspect GLB for real, place it in a scratch scene next to the
// known-good obsidian wraith, render through the actual editor scene
// path, and dump the frame for visual inspection.
// Run: cargo test -p frontend diagnose_static_glb_scene -- --ignored --nocapture
#[test]
#[ignore]
fn diagnose_static_glb_scene_render() {
    use psxed_project::WorldGrid;
    let glb = std::path::PathBuf::from(
        std::env::var("DIAG_GLB")
            .unwrap_or_else(|_| "/Users/ebonura/Downloads/ps1_clean_power_barricade.glb".into()),
    );
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root");
    let root = std::env::temp_dir().join("psoxide-diag-scene");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("scratch root");

    let mut project = ProjectDocument::new("diag");
    let room = project.active_scene_mut().add_node(
        NodeId::ROOT,
        "Room",
        NodeKind::Section {
            grid: WorldGrid::stone_room(6, 6, 1024, None, None),
        },
    );
    let model_id = psxed_project::model_import::import_model_with_animation_sources(
        &mut project,
        &glb,
        &[],
        "diag_prop",
        &root,
        psxed_project::model_import::RigidModelConfig::default(),
    )
    .expect("glb imports");

    // Known-good baseline: the tracked wraith, absolute paths so the
    // scratch project resolves them.
    let wraith_dir = repo_root.join("assets/models/obsidian_wraith");
    let wraith_id = project.add_resource(
        "Wraith",
        ResourceData::Model(psxed_project::ModelResource {
            model_path: wraith_dir
                .join("obsidian_wraith.psxmdl")
                .to_string_lossy()
                .into_owned(),
            source_path: None,
            texture_path: Some(
                wraith_dir
                    .join("obsidian_wraith.psxt")
                    .to_string_lossy()
                    .into_owned(),
            ),
            skeleton: None,
            world_height: 1024,
            collision_radius: 192,
            scale_q8: [psxed_project::MODEL_SCALE_ONE_Q8; 3],
            default_visual_yaw_q12: 0,
            attachments: Vec::new(),
        }),
    );

    for (name, id, x) in [("Prop", model_id, -0.8f32), ("Wraith", wraith_id, 0.8f32)] {
        let scene = project.active_scene_mut();
        let entity = scene.add_node(room, name, NodeKind::Entity);
        if let Some(node) = scene.node_mut(entity) {
            node.transform.translation = [x, 0.0, 0.0];
        }
        scene.add_node(
            entity,
            "Model Renderer",
            NodeKind::ModelRenderer {
                model: Some(id),
                material: None,
                visual_offset: [0; 3],
                visual_scale_q8: psxed_project::MODEL_SCALE_ONE_Q8,
            },
        );
    }

    let mut textures = crate::editor_textures::EditorTextures::new();
    textures.refresh(&project, &root);
    textures.refresh_models(&project, &root);
    let mut assets = crate::editor_assets::EditorAssets::new();
    assets.refresh(&project, &root);

    let camera = ViewportCameraState {
        mode: ViewportCameraMode::Orbit,
        yaw_q12: 600,
        pitch_q12: 350,
        radius: 1400,
        target: [2253, 256, 3072],
        position: [0, 0, 0],
    };
    let empty_hidden = std::collections::HashSet::new();
    let frame = super::build_phase1_frame(
        &project,
        camera,
        true,
        true,
        true,
        true,
        true,
        true,
        &empty_hidden,
        None,
        0,
        NodeId::ROOT,
        None,
        None,
        None,
        &[],
        &[],
        None,
        &[],
        None,
        &[],
        None,
        &textures,
        &assets,
    );
    let Some(mut renderer) = headless_preview_renderer() else {
        panic!("no headless adapter");
    };
    assert!(renderer.set_internal_scale(2, None));
    let vram = textures.vram_words();
    renderer.render_frame(&emulator_core::Gpu::new(), &frame.cmd_log, vram);
    let scale = renderer.internal_scale();
    let (w, h, rgba) = renderer.read_subrect_rgba8(0, 0, 320 * scale, 240 * scale);
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    for px in rgba.chunks_exact(4) {
        out.extend_from_slice(&px[..3]);
    }
    std::fs::write("/tmp/diag_scene.ppm", out).expect("write ppm");
    let mut histogram = std::collections::BTreeMap::new();
    for entry in frame.cmd_log.iter() {
        *histogram.entry(entry.opcode).or_insert(0usize) += 1;
    }
    println!("opcode histogram: {histogram:x?}");
    println!("wrote /tmp/diag_scene.ppm ({w}x{h})");
}
#[test]
fn character_motion_preview_overrides_only_the_target_render_transform() {
    let mut project = ProjectDocument::new("motion-preview-targets");
    let target = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Target", NodeKind::Entity);
    let other = project
        .active_scene_mut()
        .add_node(NodeId::ROOT, "Other", NodeKind::Entity);
    let preview = psxed_ui::EditorCharacterMotionPreview {
        entity: target,
        origin: [1200, 64, -800],
        yaw_q12: 1024,
        clip: 7,
    };
    let mut target_origin = psx_engine::WorldVertex::new(0, 0, 0);
    let yaw = super::apply_character_motion_preview(target, &mut target_origin, 128, Some(preview));
    assert_eq!(
        [target_origin.x, target_origin.y, target_origin.z],
        preview.origin
    );
    assert_eq!(yaw, preview.yaw_q12);

    let mut other_origin = psx_engine::WorldVertex::new(7, 8, 9);
    let yaw = super::apply_character_motion_preview(other, &mut other_origin, 256, Some(preview));
    assert_eq!([other_origin.x, other_origin.y, other_origin.z], [7, 8, 9]);
    assert_eq!(yaw, 256);
}

#[test]
fn lit_preview_key_tracks_exactly_its_inputs() {
    let mut project = ProjectDocument::new("lit-key");
    project
        .active_scene_mut()
        .brushes
        .push(psxed_project::brush::Brush::cuboid(
            [0, 0, 0],
            [512, 256, 512],
        ));
    let scene = project.active_scene_mut();
    scene.add_node(
        psxed_project::NodeId::ROOT,
        "Light",
        psxed_project::NodeKind::PointLight {
            color: [255, 255, 255],
            intensity: 1.0,
            radius: 2.0,
        },
    );
    let textures = crate::editor_textures::EditorTextures::new();
    let hidden = std::collections::HashSet::new();
    let lights = super::room_geometry::collect_bsp_preview_bake_lights(&project, &hidden);
    let base = super::lit_preview_key(&project, &textures, &lights);
    assert_eq!(
        base,
        super::lit_preview_key(&project, &textures, &lights),
        "same inputs must reuse the cached bake"
    );
    // A moved light invalidates.
    let mut moved = lights.clone();
    moved[0].position[0] += 64.0;
    assert_ne!(base, super::lit_preview_key(&project, &textures, &moved));
    // A moved brush corner invalidates.
    let mut edited = project.clone();
    edited.active_scene_mut().brushes[0].faces[0].points[0][0] += 16;
    assert_ne!(base, super::lit_preview_key(&edited, &textures, &lights));
    // A face UV change does NOT: it never feeds the bake.
    let mut uv_only = project.clone();
    uv_only.active_scene_mut().brushes[0].faces[0]
        .uv
        .offset_texels = [7, 3];
    assert_eq!(base, super::lit_preview_key(&uv_only, &textures, &lights));
}
