//! Editor 3D viewport for BSP-authored projects.
//!
//! The preview compiles the authored CSG brushes, renders the resulting BSP
//! surfaces through the PSX-style ordering-table path, and then draws the
//! project's models, lights, particles, sky, and editor overlays. The camera,
//! projection, packet limits, and material handling intentionally mirror the
//! playtest renderer so the editor is a useful approximation of console output.
//!
//! Scene geometry stays on the PSX-style path. Editor-only
//! affordances such as bounds, selection, and paint previews are
//! returned as host-drawn overlay lines so they can use fractional UI
//! strokes without PSX integer-pixel limitations.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use emulator_core::gpu::GpuCmdLogEntry;
use psx_engine::{PrimitivePacketArena, PrimitivePacketScratch, PRIMITIVE_PACKET_SLOT_WORDS};
use psx_gpu::material::{BlendMode, TextureMaterial};
use psx_gpu::ot::OrderingTable;
use psx_gpu::prim::QuadFlat;
use psx_gpu::prim::QuadGouraud;
use psx_gpu::prim::QuadTexturedMaterial;
use psx_gpu::prim::TriGouraud;
use psx_gpu::prim::TriTextured;
use psx_gpu::prim::TriTexturedGouraud;
use psx_gte::math::{Mat3I16, Vec3I16, Vec3I32};
use psx_gte::scene as gte_scene;

use psxed_project::brush::BRUSH_UV_UNITS_PER_TEXEL;
use psxed_project::{
    NodeId, NodeKind, ProjectDocument, ResourceData, ResourceId, Scene, SceneNode, SkyMode,
    SkyVisibility, Transform3,
};

use crate::editor_textures::{EditorTextures, MaterialSlot};
use psxed_ui::ViewportCameraState;

mod backdrop;
mod bsp_support;
mod camera;
mod overlays;
mod particles;
mod primitives;

use backdrop::*;
use bsp_support::*;
use camera::*;
use overlays::*;
use particles::*;
use primitives::*;

/// Maximum sectors we'll attempt to render in one preview pass.
/// 64×64 grid would already be enormous for PSX (~16 MiB cooked); a
/// 4096-cap caps the per-frame primitive count at a comfortable
/// number for the host renderer.
const TRI_CAP: usize = 4096;
const SKY_QUAD_CAP: usize = psxed_project::SKY_CYCLORAMA_QUAD_MAX;
const PROJECTED_SKY_PACKET_WORDS: usize =
    if psx_bsp::sky::VIEW_RAY_SKY_PACKET_WORDS > psx_bsp::sky::VIEW_RAY_CUBE_SKY_PACKET_WORDS {
        psx_bsp::sky::VIEW_RAY_SKY_PACKET_WORDS
    } else {
        psx_bsp::sky::VIEW_RAY_CUBE_SKY_PACKET_WORDS
    };
const PROJECTED_SKY_PACKET_SLOTS: usize =
    PROJECTED_SKY_PACKET_WORDS.div_ceil(PRIMITIVE_PACKET_SLOT_WORDS);
const FAR_VISTA_QUAD_CAP: usize = 16;
/// Model scratch mirrors editor-playtest's runtime model caps so the
/// editor preview exercises the same overflow behavior.
const PREVIEW_MODEL_VERTEX_CAP: usize = 1024;
const PREVIEW_MODEL_SOURCE_VERTEX_CAP: usize = PREVIEW_MODEL_VERTEX_CAP;
const PREVIEW_MODEL_FACE_CAP: usize = 2048;
const PREVIEW_MODEL_PART_CAP: usize = 128;
const PREVIEW_MODEL_COMMAND_CAP: usize = TRI_CAP;
const PREVIEW_PARTICLE_DRAW_CAP: usize = 48;
const PREVIEW_PARTICLE_MIN_SCREEN_SIZE: i16 = 2;
const PREVIEW_PARTICLE_MAX_SCREEN_SIZE: i16 = 16;
const PREVIEW_PARTICLE_TEXEL_U: u8 = 64;
const PREVIEW_PARTICLE_TEXEL_V: u8 = 0;
/// Cap on placed model-rendering nodes the editor preview will
/// render in one frame. Excess instances skip silently (the
/// manifest hasn't filtered them) -- keeps a runaway scene from
/// busting the per-frame budget.
const MAX_PREVIEW_MODEL_INSTANCES: usize = 8;
/// Cap on joints any one previewed model can carry. Matches
/// the runtime `JOINT_CAP` so a model that renders in
/// editor-playtest also renders here.
const PREVIEW_JOINT_CAP: usize = 32;
/// Ordering-table depth -- tradeoff between Z resolution and the
/// per-frame chain-walk cost. Brush-heavy close views need finer buckets than
/// the original 64-world-unit bands or adjacent trims can swap order.
const OT_DEPTH: usize = 4096;
const PREVIEW_GEOMETRY_SLOT_MIN: usize = 1;
const PREVIEW_CLEAR_SLOT: usize = OT_DEPTH - 1;
const PREVIEW_SKY_SLOT: usize = OT_DEPTH - 2;
const PREVIEW_FAR_VISTA_SLOT: usize = OT_DEPTH - 3;
const PREVIEW_GEOMETRY_SLOT_MAX: usize = OT_DEPTH - 4;
const OT_ADDRESS_WINDOW_BYTES: usize = 0x0100_0000;
const PREVIEW_SHADOW_DEPTH_BIAS: u32 = 128;
const PREVIEW_SHADOW_FLOOR_LIFT: i32 = 4;
const PREVIEW_SHADOW_RADIUS_SCALE_NUM: i32 = 5;
const PREVIEW_SHADOW_RADIUS_SCALE_DEN: i32 = 4;
const PREVIEW_SHADOW_RADIUS_MIN: i32 = 160;
const PREVIEW_SHADOW_RADIUS_MAX: i32 = 320;
const PREVIEW_SHADOW_UV_MAX: u8 = 63;

/// Default screen geometry -- matches the PSX 320×240 framebuffer the
/// editor's HwRenderer is sized to display.
const SCREEN_W: i32 = 320;
const SCREEN_H: i32 = 240;
const SCREEN_CX: i32 = SCREEN_W / 2;
const SCREEN_CY: i32 = SCREEN_H / 2;
/// Projection-plane distance (focal length). Bigger = narrower FOV.
const PROJ_H: i32 = 320;
const NEAR_Z: i32 = 64;
const PSX_VERTEX_MIN: i16 = -1024;
const PSX_VERTEX_MAX: i16 = 1023;
const PSX_TRI_MAX_DX: i32 = 1023;
const PSX_TRI_MAX_DY: i32 = 511;
const MAX_PREVIEW_HW_SPLIT_DEPTH: u8 = 5;
const EDITOR_PREVIEW_HOVER_STROKE_WIDTH: f32 = 1.5;
const EDITOR_PREVIEW_SELECTED_STROKE_WIDTH: f32 = 3.0;

/// Per-frame scratch -- primitives **and** OT must live in the same
/// memory region. `OrderingTable` stores 24-bit chain pointers (the
/// PS1 DMA encoding); `iter_packets` reconstructs full addresses by
/// OR-ing the OT slot's high 40 bits over the 24-bit chain entries.
/// That only works if every chained primitive sits in the same 16 MB
/// window as the OT itself -- heap-allocated `Vec<TriFlat>` lives in
/// a totally separate region on host and segfaults on dereference.
/// Keeping the array inline alongside the OT is necessary but not
/// sufficient on macOS: ASLR can place the static near the end of a
/// 16 MB window, so later arrays cross into the next window and the
/// 24-bit tag can no longer be reconstructed from the OT's high bits.
/// The 16 MB-aligned heap allocation below makes the whole scratch
/// arena live in one reconstructable address window and matches PS1's
/// flat 2 MB main RAM layout closely enough for host preview.
#[repr(C, align(16777216))]
struct PreviewScratch {
    ot: OrderingTable<OT_DEPTH>,
    projected_sky_packets: PrimitivePacketScratch<PROJECTED_SKY_PACKET_SLOTS>,
    sky_quads: [QuadGouraud; SKY_QUAD_CAP],
    far_vista_quads: [QuadFlat; FAR_VISTA_QUAD_CAP],
    tris: [TriGouraud; TRI_CAP],
    tex_tris: [TriTexturedGouraud; TRI_CAP],
    /// Engine-side model instances render through
    /// `psx_engine::PrimitiveArena<TriTextured>` (the runtime model
    /// path emits flat-textured packets), so they keep their own
    /// pool now that world geometry rides Gouraud packets.
    model_tex_tris: [TriTextured; TRI_CAP],
    particle_quads: [QuadTexturedMaterial; PREVIEW_PARTICLE_DRAW_CAP],
    model_vertices: [psx_engine::ProjectedVertex; PREVIEW_MODEL_VERTEX_CAP],
    model_faces: [psx_engine::TexturedModelRenderFace; PREVIEW_MODEL_FACE_CAP],
    model_parts: [psx_asset::ModelPart; PREVIEW_MODEL_PART_CAP],
    model_source_vertices: [psx_asset::ModelVertex; PREVIEW_MODEL_SOURCE_VERTEX_CAP],
    model_joint_transforms: [psx_engine::JointViewTransform; PREVIEW_JOINT_CAP],
    /// `0` = next free slot in `tris` (flat-shaded);
    /// `tex_used` = next free slot in `tex_tris`.
    used: usize,
    sky_used: usize,
    far_vista_used: usize,
    tex_used: usize,
    model_tex_used: usize,
    particle_used: usize,
    /// Host-drawn overlay lines for editor affordances. These stay
    /// outside the GP0 command log so the UI can draw fractional,
    /// overlaid strokes that are not limited by PSX integer pixels.
    overlay_lines: Vec<psxed_ui::EditorViewportOverlayLine>,
    /// GP0(02h) fill-rectangle packet: 1 tag word + 3 data words
    /// (`opcode|color`, `pack_xy(x, y)`, `pack_xy(w, h)`). Must live
    /// in the same aligned arena as the OT for the same reason the
    /// prim arrays do -- `iter_packets` reconstructs full pointers
    /// from the OT's 24-bit chain encoding plus the OT struct's high
    /// address bits, so chained packets must share that 16 MB region.
    clear_packet: [u32; 4],
}

const _: () = assert!(
    core::mem::size_of::<PreviewScratch>() <= OT_ADDRESS_WINDOW_BYTES,
    "editor preview scratch must fit in one 24-bit OT address window"
);

const EMPTY_TRI: TriGouraud =
    TriGouraud::new([(0, 0), (0, 0), (0, 0)], [(0, 0, 0), (0, 0, 0), (0, 0, 0)]);
const EMPTY_FAR_VISTA_QUAD: QuadFlat = QuadFlat::new([(0, 0), (0, 0), (0, 0), (0, 0)], 0, 0, 0);
const EMPTY_SKY_QUAD: QuadGouraud = QuadGouraud::new(
    [(0, 0), (0, 0), (0, 0), (0, 0)],
    [(0, 0, 0), (0, 0, 0), (0, 0, 0), (0, 0, 0)],
);
const EMPTY_MODEL_TEX_TRI: TriTextured = TriTextured::new(
    [(0, 0), (0, 0), (0, 0)],
    [(0, 0), (0, 0), (0, 0)],
    0,
    0,
    (0x80, 0x80, 0x80),
);
const EMPTY_TEX_TRI: TriTexturedGouraud = TriTexturedGouraud::with_material(
    [(0, 0), (0, 0), (0, 0)],
    [(0, 0), (0, 0), (0, 0)],
    [(0x80, 0x80, 0x80); 3],
    TextureMaterial::opaque(0, 0, (0x80, 0x80, 0x80)),
);
const EMPTY_PARTICLE_QUAD: QuadTexturedMaterial = QuadTexturedMaterial::with_material(
    [(0, 0), (0, 0), (0, 0), (0, 0)],
    [(0, 0), (0, 0), (0, 0), (0, 0)],
    TextureMaterial::opaque(0, 0, (0, 0, 0)),
);

static SCRATCH: OnceLock<Mutex<Box<PreviewScratch>>> = OnceLock::new();

fn preview_scratch() -> &'static Mutex<Box<PreviewScratch>> {
    SCRATCH.get_or_init(|| Mutex::new(new_preview_scratch()))
}

fn new_preview_scratch() -> Box<PreviewScratch> {
    let mut scratch = Box::<PreviewScratch>::new_uninit();
    let ptr = scratch.as_mut_ptr();
    // SAFETY: every field is written exactly once before `assume_init`.
    // `Box::<PreviewScratch>::new_uninit()` asks the allocator for
    // the large alignment that static Mach-O sections did not
    // reliably preserve, so release builds can still reconstruct
    // 24-bit OT links from the OT's address window.
    unsafe {
        std::ptr::addr_of_mut!((*ptr).ot).write(OrderingTable::new());
        std::ptr::addr_of_mut!((*ptr).projected_sky_packets).write(PrimitivePacketScratch::ZERO);
        std::ptr::addr_of_mut!((*ptr).sky_quads).write([EMPTY_SKY_QUAD; SKY_QUAD_CAP]);
        std::ptr::addr_of_mut!((*ptr).far_vista_quads)
            .write([EMPTY_FAR_VISTA_QUAD; FAR_VISTA_QUAD_CAP]);
        std::ptr::addr_of_mut!((*ptr).tris).write([EMPTY_TRI; TRI_CAP]);
        std::ptr::addr_of_mut!((*ptr).tex_tris).write([EMPTY_TEX_TRI; TRI_CAP]);
        std::ptr::addr_of_mut!((*ptr).model_tex_tris).write([EMPTY_MODEL_TEX_TRI; TRI_CAP]);
        std::ptr::addr_of_mut!((*ptr).particle_quads)
            .write([EMPTY_PARTICLE_QUAD; PREVIEW_PARTICLE_DRAW_CAP]);
        std::ptr::addr_of_mut!((*ptr).model_vertices)
            .write([psx_engine::ProjectedVertex::new(0, 0, 0); PREVIEW_MODEL_VERTEX_CAP]);
        std::ptr::addr_of_mut!((*ptr).model_faces)
            .write([psx_engine::TexturedModelRenderFace::ZERO; PREVIEW_MODEL_FACE_CAP]);
        std::ptr::addr_of_mut!((*ptr).model_parts)
            .write([psx_asset::ModelPart::ZERO; PREVIEW_MODEL_PART_CAP]);
        std::ptr::addr_of_mut!((*ptr).model_source_vertices)
            .write([psx_asset::ModelVertex::ZERO; PREVIEW_MODEL_SOURCE_VERTEX_CAP]);
        std::ptr::addr_of_mut!((*ptr).model_joint_transforms)
            .write([psx_engine::JointViewTransform::ZERO; PREVIEW_JOINT_CAP]);
        std::ptr::addr_of_mut!((*ptr).used).write(0);
        std::ptr::addr_of_mut!((*ptr).sky_used).write(0);
        std::ptr::addr_of_mut!((*ptr).far_vista_used).write(0);
        std::ptr::addr_of_mut!((*ptr).tex_used).write(0);
        std::ptr::addr_of_mut!((*ptr).model_tex_used).write(0);
        std::ptr::addr_of_mut!((*ptr).particle_used).write(0);
        std::ptr::addr_of_mut!((*ptr).overlay_lines).write(Vec::new());
        std::ptr::addr_of_mut!((*ptr).clear_packet).write([0; 4]);
        scratch.assume_init()
    }
}

impl PreviewScratch {
    fn geometry_full(&self) -> bool {
        self.used >= TRI_CAP || self.tex_used >= TRI_CAP
    }
}

/// Render data for one editable 3D preview frame.
#[derive(Default)]
pub struct EditorPreviewFrame {
    /// PSX-style command log for the scene itself.
    pub cmd_log: Vec<GpuCmdLogEntry>,
    /// Host UI overlay lines for editor-only affordances.
    pub overlay_lines: Vec<psxed_ui::EditorViewportOverlayLine>,
}

/// Add the BSP brush-surface grid ahead of stronger selection/gizmo lines.
/// Kept separate from the PSX-style command log because this is an editor
/// readability aid with translucent, sub-pixel host strokes.
pub(crate) fn prepend_bsp_surface_grid_overlay(
    project: &ProjectDocument,
    camera: ViewportCameraState,
    grid_units: u16,
    hidden_scene_nodes: &HashSet<NodeId>,
    overlay_lines: &mut Vec<psxed_ui::EditorViewportOverlayLine>,
) {
    let world_camera = setup_gte_for_camera(camera);
    overlays::prepend_bsp_surface_grid_overlay(
        project,
        world_camera,
        grid_units,
        hidden_scene_nodes,
        overlay_lines,
    );
}

/// Draw the Quake pointfile route from the last successful BSP cook. The
/// bright host stroke is split against the cached solid-brush geometry so
/// walls hide it even though it is not part of the PSX command stream.
pub(crate) fn append_bsp_leak_path_overlay(
    project: &ProjectDocument,
    camera: ViewportCameraState,
    leak_path: &[[i32; 3]],
    likely_opening: &[[i32; 3]],
    hidden_scene_nodes: &HashSet<NodeId>,
    overlay_lines: &mut Vec<psxed_ui::EditorViewportOverlayLine>,
) {
    let world_camera = setup_gte_for_camera(camera);
    overlays::append_bsp_leak_path_overlay(
        project,
        world_camera,
        leak_path,
        likely_opening,
        hidden_scene_nodes,
        overlay_lines,
    );
}

/// Build a fresh BSP preview frame from `camera`'s orbit angles.
///
/// Even an empty scene emits its clear/sky frame so a project switch cannot
/// leave the persistent preview target showing pixels from the old scene.
#[allow(clippy::too_many_arguments)]
pub fn build_phase1_frame(
    project: &ProjectDocument,
    camera: ViewportCameraState,
    preview_bounds: bool,
    show_lights: bool,
    hidden_scene_nodes: &HashSet<NodeId>,
    selected: psxed_project::NodeId,
    character_motion: Option<psxed_ui::EditorCharacterMotionPreview>,
    selected_bounds: Option<([f32; 3], [f32; 3])>,
    entity_bounds: &[psxed_ui::EntityBounds],
    hovered_entity_node: Option<psxed_project::NodeId>,
    textures: &EditorTextures,
    assets: &crate::editor_assets::EditorAssets,
) -> EditorPreviewFrame {
    build_phase1_frame_reusing(
        project,
        camera,
        preview_bounds,
        show_lights,
        hidden_scene_nodes,
        selected,
        character_motion,
        selected_bounds,
        entity_bounds,
        hovered_entity_node,
        textures,
        assets,
        EditorPreviewFrame::default(),
    )
}

/// Build a preview frame while retaining the output vectors from a previous
/// frame. The live editor uses this path to avoid reallocating the command log
/// and overlay storage during camera navigation.
#[allow(clippy::too_many_arguments)]
pub fn build_phase1_frame_reusing(
    project: &ProjectDocument,
    camera: ViewportCameraState,
    preview_bounds: bool,
    show_lights: bool,
    hidden_scene_nodes: &HashSet<NodeId>,
    selected: psxed_project::NodeId,
    character_motion: Option<psxed_ui::EditorCharacterMotionPreview>,
    selected_bounds: Option<([f32; 3], [f32; 3])>,
    entity_bounds: &[psxed_ui::EntityBounds],
    hovered_entity_node: Option<psxed_project::NodeId>,
    textures: &EditorTextures,
    assets: &crate::editor_assets::EditorAssets,
    mut output: EditorPreviewFrame,
) -> EditorPreviewFrame {
    let preview_context_node = project.active_scene().root;

    let mut scratch = preview_scratch()
        .lock()
        .expect("editor preview scratch mutex");
    scratch.used = 0;
    scratch.sky_used = 0;
    scratch.far_vista_used = 0;
    scratch.tex_used = 0;
    scratch.model_tex_used = 0;
    scratch.particle_used = 0;
    scratch.overlay_lines.clear();
    scratch.ot.clear();

    let world_camera = setup_gte_for_camera(camera);
    let resolved_sky = project
        .active_scene()
        .world_sky_for_node(preview_context_node)
        .unwrap_or_default()
        .resolved_for_room(false, [0; 3]);
    let preview_tick = preview_elapsed_vblanks();
    let visible_sky_aperture = visible_sky_aperture(project, world_camera, hidden_scene_nodes);
    let sky_visible = resolved_sky.visibility == SkyVisibility::Always || visible_sky_aperture;
    push_clear(&mut scratch, resolved_sky.lower_color);
    if sky_visible {
        match resolved_sky.mode {
            SkyMode::Panorama => push_cyclorama(&mut scratch, resolved_sky, world_camera),
            SkyMode::QuakeLayered | SkyMode::Cube => push_projected_scene_sky(
                &mut scratch,
                resolved_sky.mode,
                preview_view_rotation(camera),
                preview_tick,
                textures,
            ),
            SkyMode::Off => {}
        }
    }
    let resolved_far_vista = project
        .active_scene()
        .world_far_vista_for_node(preview_context_node)
        .unwrap_or_default()
        .resolved_for_room(false, [0; 3]);
    push_far_vista_ring(
        &mut scratch,
        camera,
        world_camera,
        resolved_far_vista,
        textures,
    );
    // World-space brushes render once, against the camera GTE state
    // installed above (rooms and their local offsets do not apply).
    walk_brushes(
        project,
        textures,
        world_camera,
        hidden_scene_nodes,
        &mut scratch,
    );
    // World-space light gizmos (BSP scenes): rooms never iterate here, so
    // roomless lights draw their ring + bulb right after the brushes,
    // while the camera GTE state is still installed.
    if show_lights {
        walk_roomless_light_gizmos(
            project,
            world_camera,
            hidden_scene_nodes,
            selected,
            hovered_entity_node,
            &mut scratch,
        );
    }
    walk_bsp_particle_emitters(
        project,
        hidden_scene_nodes,
        textures.particle_slot(),
        preview_tick,
        &mut scratch,
    );
    // World-space (BSP) entity models: prime the camera GTE first (the
    // room loop leaves per-room state), draw, then fall through to the
    // shared re-prime below for the bounds pass.
    let _ = setup_gte_for_camera(camera);
    walk_bsp_model_instances(
        project,
        textures,
        assets,
        selected,
        &world_camera,
        hidden_scene_nodes,
        preview_tick,
        character_motion,
        &mut scratch,
    );

    // Re-prime the GTE with the camera matrix -- model
    // rendering left it set to the last joint's view, which
    // would project entity bound lines into junk.
    let _ = setup_gte_for_camera(camera);
    if preview_bounds {
        walk_entity_bounds(entity_bounds, selected, hovered_entity_node, &mut scratch);
    }
    if let Some((center, half_extents)) = selected_bounds {
        if !selected_node_is_image_prop(project, selected) {
            push_aabb_wireframe(&mut scratch, center, half_extents, ENTITY_BOUND_SELECTED);
        }
    }
    // SAFETY: the mutex guard keeps the preview packet arenas alive
    // while the OT is walked. `PreviewScratch` is 16 MB-aligned so the
    // OT's 24-bit packet links reconstruct addresses inside the same
    // host address window.
    // SAFETY: the mutex guard keeps every OT packet alive while it is walked.
    unsafe { psx_gpu_render::build_cmd_log_into(&scratch.ot, &mut output.cmd_log) };
    output.overlay_lines.clear();
    output
        .overlay_lines
        .extend_from_slice(&scratch.overlay_lines);
    output
}

fn component_children<'a>(
    scene: &'a Scene,
    host: &'a SceneNode,
) -> impl Iterator<Item = &'a SceneNode> + 'a {
    host.children.iter().filter_map(|id| scene.node(*id))
}

#[derive(Clone, Copy)]
struct PreviewModelReference {
    model_id: ResourceId,
    material_override: Option<ResourceId>,
    clip_override: Option<u16>,
    autoplay: bool,
    /// Frame the animator is parked on when `autoplay` is false. The cook and
    /// the inspector both honour this; the viewport used to pin frame 0, so
    /// scrubbing a pose in the inspector changed nothing on screen.
    pose_frame: u16,
    renderer_node: Option<NodeId>,
    animator_node: Option<NodeId>,
    visual_offset: [i16; 3],
    visual_yaw_q12: u16,
    visual_scale_q8: u16,
}

fn preview_model_reference(scene: &Scene, node: &SceneNode) -> Option<PreviewModelReference> {
    match &node.kind {
        NodeKind::MeshInstance {
            mesh: Some(model_id),
            material,
            animation_clip,
        } => Some(PreviewModelReference {
            model_id: *model_id,
            material_override: *material,
            clip_override: *animation_clip,
            autoplay: true,
            pose_frame: 0,
            renderer_node: None,
            animator_node: None,
            visual_offset: [0; 3],
            visual_yaw_q12: 0,
            visual_scale_q8: psxed_project::MODEL_SCALE_ONE_Q8,
        }),
        NodeKind::Entity => {
            let mut renderer = None;
            let mut animator = None;
            for child in component_children(scene, node) {
                match &child.kind {
                    NodeKind::ModelRenderer {
                        model: Some(model_id),
                        material,
                        visual_offset,
                        visual_scale_q8,
                    } if renderer.is_none() => {
                        renderer = Some((
                            child.id,
                            *model_id,
                            *material,
                            *visual_offset,
                            yaw_to_q12(child.transform.rotation_degrees[1]),
                            *visual_scale_q8,
                        ));
                    }
                    NodeKind::Animator {
                        clip,
                        autoplay,
                        pose_frame,
                        ..
                    } if animator.is_none() => {
                        animator = Some((child.id, *clip, *autoplay, *pose_frame));
                    }
                    _ => {}
                }
            }
            renderer.map(
                |(
                    renderer_node,
                    model_id,
                    material_override,
                    visual_offset,
                    visual_yaw_q12,
                    visual_scale_q8,
                )| {
                    PreviewModelReference {
                        model_id,
                        material_override,
                        clip_override: animator.and_then(|(_, clip, _, _)| clip),
                        autoplay: animator.is_none_or(|(_, _, autoplay, _)| autoplay),
                        pose_frame: animator.map_or(0, |(_, _, _, frame)| frame),
                        renderer_node: Some(renderer_node),
                        animator_node: animator.map(|(node_id, _, _, _)| node_id),
                        visual_offset,
                        visual_yaw_q12,
                        visual_scale_q8,
                    }
                },
            )
        }
        _ => None,
    }
}

fn valid_preview_clip(requested: u16, clip_count: usize) -> Option<u16> {
    if clip_count == 0 {
        None
    } else if usize::from(requested) < clip_count {
        Some(requested)
    } else {
        // Resource pruning and animation-set edits can leave an Animator's
        // saved local index stale. Keep the model visible on its first valid
        // clip instead of silently dropping the whole entity from the editor.
        Some(0)
    }
}

fn preview_reference_selected(
    selected: NodeId,
    host_id: NodeId,
    component_a: Option<NodeId>,
    component_b: Option<NodeId>,
    component_c: Option<NodeId>,
) -> bool {
    selected == host_id
        || component_a == Some(selected)
        || component_b == Some(selected)
        || component_c == Some(selected)
}

fn preview_reference_hidden(
    scene: &Scene,
    hidden_scene_nodes: &HashSet<NodeId>,
    host_id: NodeId,
    component_a: Option<NodeId>,
    component_b: Option<NodeId>,
    component_c: Option<NodeId>,
) -> bool {
    scene_node_hidden(scene, hidden_scene_nodes, host_id)
        || component_a.is_some_and(|id| scene_node_hidden(scene, hidden_scene_nodes, id))
        || component_b.is_some_and(|id| scene_node_hidden(scene, hidden_scene_nodes, id))
        || component_c.is_some_and(|id| scene_node_hidden(scene, hidden_scene_nodes, id))
}

fn scene_node_hidden(scene: &Scene, hidden_scene_nodes: &HashSet<NodeId>, id: NodeId) -> bool {
    let mut current = Some(id);
    while let Some(node_id) = current {
        if hidden_scene_nodes.contains(&node_id) {
            return true;
        }
        current = scene.node(node_id).and_then(|node| node.parent);
    }
    false
}

fn brush_group_hidden(
    scene: &Scene,
    hidden_scene_nodes: &HashSet<NodeId>,
    brush: &psxed_project::brush::Brush,
) -> bool {
    brush
        .group
        .is_some_and(|group| scene_node_hidden(scene, hidden_scene_nodes, group))
}

fn brush_fallback_color(face: usize) -> (u8, u8, u8) {
    let shade = 0x6c + ((face as u8) % 3) * 0x14;
    (shade, shade, shade)
}

/// Ambient term for world-space (BSP) preview lighting, matching the
/// Release bake contract (`PXBSP_AMBIENT_RGB` in the cooked manifest).
const PXBSP_PREVIEW_AMBIENT: [u8; 3] = [32; 3];

#[derive(Debug, Clone, Copy)]
pub(crate) struct PreviewBrushBounds {
    min: [f64; 3],
    max: [f64; 3],
}

struct PreviewSolvedBrush {
    all_planes: Vec<psxed_project::brush::Plane>,
    normalized_planes: Vec<([f64; 3], f64)>,
    bounds: PreviewBrushBounds,
    pickable: bool,
}

/// Camera-independent convex solve output. Camera navigation, material/UV
/// edits, and overlay visibility do not change this geometry, while solving a
/// full imported map allocates and clips thousands of temporary windings.
/// Hashing the authored integer plane points is much cheaper and captures
/// every input consumed by `Brush::solve`.
struct PreviewSolvedBrushCache {
    key: Option<u64>,
    brushes: Vec<PreviewSolvedBrush>,
}

static SOLVED_BRUSH_CACHE: OnceLock<Mutex<PreviewSolvedBrushCache>> = OnceLock::new();

/// Exterior brush-union surface used by the material preview. Raw editable
/// brushes may overlap, but submitting every authored face independently is
/// not renderable with a PS1 painter's algorithm: two faces can exchange
/// front/back order inside one triangle. The cook already solves that with
/// union CSG, so the editor caches the same split exterior polygons. Raw
/// solved brush planes remain separately cached for lighting occlusion tests.
struct PreviewCsgSurface {
    surface: psxed_project::brush_compile::CompiledSurface,
    bounds: PreviewBrushBounds,
}

struct PreviewCsgCache {
    key: Option<u64>,
    surfaces: Vec<PreviewCsgSurface>,
}

static CSG_SURFACE_CACHE: OnceLock<Mutex<PreviewCsgCache>> = OnceLock::new();

fn solved_brush_geometry_key(project: &ProjectDocument) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let brushes = &project.active_scene().brushes;
    brushes.len().hash(&mut hasher);
    for brush in brushes {
        brush.faces.len().hash(&mut hasher);
        for face in &brush.faces {
            face.points.hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn csg_surface_key(project: &ProjectDocument, hidden_scene_nodes: &HashSet<NodeId>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let scene = project.active_scene();
    scene.brushes.len().hash(&mut hasher);
    for brush in &scene.brushes {
        brush_group_hidden(scene, hidden_scene_nodes, brush).hash(&mut hasher);
        brush.contents.label().hash(&mut hasher);
        brush.mover.map(NodeId::raw).hash(&mut hasher);
        brush.faces.len().hash(&mut hasher);
        for face in &brush.faces {
            face.points.hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn surface_bounds(vertices: &[[f64; 3]]) -> PreviewBrushBounds {
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for vertex in vertices {
        for axis in 0..3 {
            min[axis] = min[axis].min(vertex[axis]);
            max[axis] = max[axis].max(vertex[axis]);
        }
    }
    PreviewBrushBounds { min, max }
}

fn rebuild_csg_surfaces(
    project: &ProjectDocument,
    hidden_scene_nodes: &HashSet<NodeId>,
) -> Vec<PreviewCsgSurface> {
    use psxed_project::brush::BRUSH_EDIT_EXTENT_LIMIT;
    let scene = project.active_scene();
    type PreviewBrushGroup = (Option<NodeId>, Vec<(usize, psxed_project::brush::Brush)>);
    let mut groups: Vec<PreviewBrushGroup> = Vec::new();
    for (source_index, brush) in scene.brushes.iter().enumerate() {
        if brush_group_hidden(scene, hidden_scene_nodes, brush) {
            continue;
        }
        let solved = brush.solve();
        if !solved.is_valid() || !solved.within_extent(BRUSH_EDIT_EXTENT_LIMIT) {
            continue;
        }
        let group = if let Some(group) = groups.iter_mut().find(|entry| entry.0 == brush.mover) {
            group
        } else {
            groups.push((brush.mover, Vec::new()));
            groups.last_mut().expect("just pushed CSG group")
        };
        group.1.push((source_index, brush.clone()));
    }

    let mut output = Vec::new();
    for (_, brushes) in groups {
        let source_indices: Vec<_> = brushes.iter().map(|entry| entry.0).collect();
        let local_brushes: Vec<_> = brushes.into_iter().map(|entry| entry.1).collect();
        for mut surface in psxed_project::brush_compile::compile_csg_surfaces(&local_brushes) {
            let Some(&source_brush) = source_indices.get(surface.source_brush) else {
                continue;
            };
            surface.source_brush = source_brush;
            output.push(PreviewCsgSurface {
                bounds: surface_bounds(&surface.vertices),
                surface,
            });
        }
    }
    output
}

fn with_cached_csg_surfaces<R>(
    project: &ProjectDocument,
    hidden_scene_nodes: &HashSet<NodeId>,
    visit: impl FnOnce(&[PreviewCsgSurface]) -> R,
) -> R {
    let key = csg_surface_key(project, hidden_scene_nodes);
    let cache = CSG_SURFACE_CACHE.get_or_init(|| {
        Mutex::new(PreviewCsgCache {
            key: None,
            surfaces: Vec::new(),
        })
    });
    let mut cache = cache.lock().expect("preview CSG surface cache");
    if cache.key != Some(key) {
        cache.surfaces = rebuild_csg_surfaces(project, hidden_scene_nodes);
        cache.key = Some(key);
    }
    visit(&cache.surfaces)
}

fn rebuild_solved_brushes(project: &ProjectDocument) -> Vec<PreviewSolvedBrush> {
    use psxed_project::brush::{Plane, BRUSH_EDIT_EXTENT_LIMIT};
    project
        .active_scene()
        .brushes
        .iter()
        .map(|brush| {
            let solved = brush.solve();
            let pickable = solved.is_valid() && solved.within_extent(BRUSH_EDIT_EXTENT_LIMIT);
            let all_planes = brush
                .faces
                .iter()
                .filter_map(|face| Plane::from_points(face.points))
                .collect::<Vec<_>>();
            let normalized_planes = all_planes
                .iter()
                .copied()
                .map(psxed_project::brush_compile::normalized_plane)
                .collect();
            PreviewSolvedBrush {
                all_planes,
                normalized_planes,
                bounds: PreviewBrushBounds {
                    min: solved.min,
                    max: solved.max,
                },
                pickable,
            }
        })
        .collect()
}

fn with_cached_solved_brushes<R>(
    project: &ProjectDocument,
    visit: impl FnOnce(&[PreviewSolvedBrush]) -> R,
) -> R {
    let key = solved_brush_geometry_key(project);
    let cache = SOLVED_BRUSH_CACHE.get_or_init(|| {
        Mutex::new(PreviewSolvedBrushCache {
            key: None,
            brushes: Vec::new(),
        })
    });
    let mut cache = cache.lock().expect("preview solved brush cache");
    if cache.key != Some(key) {
        cache.brushes = rebuild_solved_brushes(project);
        cache.key = Some(key);
    }
    visit(&cache.brushes)
}

/// Subdivided + shadow-baked output for one exterior CSG surface.
struct PreviewLitSurface {
    source_brush: usize,
    source_face: usize,
    plane: psxed_project::brush::Plane,
    bounds: PreviewBrushBounds,
    /// `(patch polygon, per-vertex baked colour)` pairs.
    patches: Vec<PreviewLitPatch>,
}

type PreviewLitPatch = (Vec<[f64; 3]>, Vec<(u8, u8, u8)>);

/// Lit brush geometry reused across frames: solving, subdividing and
/// shadow-baking the whole scene costs milliseconds, and it only
/// changes when a brush, light, or material tint does. The key hashes
/// exactly those inputs; camera motion and everything else re-uses
/// the bake and pays only projection.
struct PreviewLitCache {
    key: Option<u64>,
    surfaces: Vec<PreviewLitSurface>,
}

static LIT_CACHE: OnceLock<Mutex<PreviewLitCache>> = OnceLock::new();

/// Everything the lit-brush bake reads, folded into one key: brush
/// geometry and materials, the per-face tint the shade resolves to,
/// and the (hidden-filtered) light set. Face UV transforms are
/// deliberately absent: they steer texels, never colours or patches.
fn lit_preview_key(
    project: &ProjectDocument,
    textures: &EditorTextures,
    lights: &[psxed_project::brush_light::BrushPointLight],
    hidden_scene_nodes: &HashSet<NodeId>,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for brush in &project.active_scene().brushes {
        brush_group_hidden(project.active_scene(), hidden_scene_nodes, brush).hash(&mut hasher);
        brush.contents.label().hash(&mut hasher);
        brush.mover.map(NodeId::raw).hash(&mut hasher);
        brush.faces.len().hash(&mut hasher);
        for (face_index, face) in brush.faces.iter().enumerate() {
            face.points.hash(&mut hasher);
            face.material.map(|id| id.raw()).hash(&mut hasher);
            face.material
                .and_then(|id| project.resource(id))
                .is_some_and(|resource| {
                    matches!(&resource.data, ResourceData::Material(material) if material.sky_aperture)
                })
                .hash(&mut hasher);
            let shade = face_shade(
                project,
                face.material,
                brush_fallback_color(face_index),
                textures,
            );
            let tint = match shade {
                FaceShade::Flat { rgb, .. } => rgb,
                FaceShade::Textured { tint, .. } => tint,
            };
            [tint.0, tint.1, tint.2].hash(&mut hasher);
        }
    }
    for light in lights {
        light.position.map(f64::to_bits).hash(&mut hasher);
        light.radius.to_bits().hash(&mut hasher);
        light.intensity_q8.hash(&mut hasher);
        light.color.hash(&mut hasher);
    }
    PXBSP_PREVIEW_AMBIENT.hash(&mut hasher);
    hasher.finish()
}

fn rebuild_lit_surfaces(
    project: &ProjectDocument,
    textures: &EditorTextures,
    lights: &[psxed_project::brush_light::BrushPointLight],
    hidden_scene_nodes: &HashSet<NodeId>,
) -> Vec<PreviewLitSurface> {
    // Shadow occluders mirror the cook's set (every solid brush), with
    // one editor-only guard: mid-edit damaged/unbounded brushes are
    // skipped, otherwise an infinite wedge occludes every segment and
    // blacks out the room while you drag.
    with_cached_solved_brushes(project, |solved_brushes| {
        let brushes = &project.active_scene().brushes;
        let occluders: Vec<Vec<psxed_project::brush::Plane>> = brushes
            .iter()
            .zip(solved_brushes)
            .filter(|(brush, solved)| brush.contents.is_solid() && solved.pickable)
            .map(|(_, solved)| solved.all_planes.clone())
            .collect();
        with_cached_csg_surfaces(project, hidden_scene_nodes, |surfaces| {
            surfaces
                .iter()
                .filter_map(|cached| {
                    let surface = &cached.surface;
                    let brush = brushes.get(surface.source_brush)?;
                    let face = brush.faces.get(surface.source_face)?;
                    if face.material.is_some_and(|material| {
                        project.resource(material).is_some_and(|resource| {
                            matches!(&resource.data, ResourceData::Material(material) if material.sky_aperture)
                        })
                    }) {
                        return None;
                    }
                    let (normal, _) = psxed_project::brush_compile::normalized_plane(surface.plane);
                    let shade = face_shade(
                        project,
                        face.material,
                        brush_fallback_color(surface.source_face),
                        textures,
                    );
                    let tint = match shade {
                        FaceShade::Flat { rgb, .. } => [rgb.0, rgb.1, rgb.2],
                        FaceShade::Textured { tint, .. } => [tint.0, tint.1, tint.2],
                    };
                    // The cook subdivides EVERY face to the qbsp-parity
                    // extent cap (lights or not); the preview must render
                    // the SAME patches or mid-face light features fall
                    // between the authored corners and vanish, and the
                    // preview==cook invariant breaks.
                    let patches = psxed_project::brush_compile::subdivide_polygon_for_lighting(
                        surface.vertices.clone(),
                        psxed_project::brush_compile::SURFACE_EXTENT_UNITS,
                        &[([0.0; 3], f64::INFINITY)],
                    )
                    .into_iter()
                    .map(|verts| {
                        let colors = verts
                            .iter()
                            .map(|&vertex| {
                                let color = psxed_project::brush_light::lit_point_color(
                                    vertex,
                                    normal,
                                    tint,
                                    PXBSP_PREVIEW_AMBIENT,
                                    lights,
                                    &occluders,
                                );
                                (color[0], color[1], color[2])
                            })
                            .collect();
                        (verts, colors)
                    })
                    .collect();
                    Some(PreviewLitSurface {
                        source_brush: surface.source_brush,
                        source_face: surface.source_face,
                        plane: surface.plane,
                        bounds: cached.bounds,
                        patches,
                    })
                })
                .collect()
        })
    })
}

/// One brush patch through projection and submission. `colors` is the
/// per-vertex bake (lit path) or `None` for the historic uniform
/// shade.
#[allow(clippy::too_many_arguments)]
fn emit_brush_patch(
    scratch: &mut PreviewScratch,
    camera: psx_engine::WorldCamera,
    plane: &psxed_project::brush::Plane,
    face_uv: &psxed_project::brush::FaceUv,
    shade: FaceShade,
    verts: &[[f64; 3]],
    colors: Option<&[(u8, u8, u8)]>,
) {
    use psxed_project::brush::paraxial_uv;
    // Defense against unbounded/corrupt brushes (infinite wedges solve to
    // base-winding coordinates): anything beyond the renderer's safe range
    // would overflow the fixed-point camera rotate.
    const PREVIEW_COORD_LIMIT: f64 = 500_000.0;
    if verts.iter().any(|vertex| {
        vertex
            .iter()
            .any(|coordinate| !coordinate.is_finite() || coordinate.abs() > PREVIEW_COORD_LIMIT)
    }) {
        return;
    }

    // Texel UVs for the whole patch, rebased by whole texture repeats
    // so no triangle straddles the u8 wrap: a straddling triangle
    // samples a wrapped (backwards) gradient, and with light-driven
    // subdivision the straddle set changed whenever a light moved,
    // which read as the texture changing. Power-of-two repeat sizes
    // make the rebase sampling-identical; spans too wide for the u8
    // window keep the historic wrap.
    if verts.len() > PREVIEW_FACE_VERTEX_CAP {
        return;
    }
    let mut patch_uvs = [[0.0; 2]; PREVIEW_FACE_VERTEX_CAP];
    if let FaceShade::Textured { slot, .. } = shade {
        for (uv, &vertex) in patch_uvs[..verts.len()].iter_mut().zip(verts) {
            let raw = paraxial_uv(plane, vertex);
            *uv = face_uv.apply([
                raw[0] / BRUSH_UV_UNITS_PER_TEXEL,
                raw[1] / BRUSH_UV_UNITS_PER_TEXEL,
            ]);
        }
        psxed_project::brush::rebase_texel_uvs(
            &mut patch_uvs[..verts.len()],
            [
                f64::from(slot.texture_width.max(8)),
                f64::from(slot.texture_height.max(8)),
            ],
        );
    }

    let default_color = match shade {
        FaceShade::Flat { rgb, .. } => rgb,
        FaceShade::Textured { tint, .. } => tint,
    };
    let mut clip_vertices = [EMPTY_PREVIEW_CLIP_VERTEX; PREVIEW_FACE_VERTEX_CAP];
    for (index, vertex) in verts.iter().enumerate() {
        let world = psx_engine::WorldVertex::new(
            vertex[0].round() as i32,
            vertex[1].round() as i32,
            vertex[2].round() as i32,
        );
        clip_vertices[index] = PreviewClipVertex::new(
            camera.view_vertex(world),
            patch_uvs[index],
            colors.map_or(default_color, |colors| colors[index]),
        );
    }
    let clipped = clip_preview_brush_polygon(camera.projection, &clip_vertices[..verts.len()]);
    let clipped = clipped.as_slice();
    let surface_slot = clipped_surface_depth_slot(clipped);
    let Some(anchor) = clipped
        .first()
        .and_then(|vertex| vertex.projected(camera.projection))
    else {
        return;
    };
    let Some(mut previous) = clipped
        .get(1)
        .and_then(|vertex| vertex.projected(camera.projection))
    else {
        return;
    };
    for vertex in &clipped[2..] {
        let Some(current) = vertex.projected(camera.projection) else {
            return;
        };
        let p = [anchor.0, previous.0, current.0];
        let uvs = [anchor.1, previous.1, current.1];
        let tri_colors = match colors {
            Some(_) => [anchor.2, previous.2, current.2],
            None => {
                let color = match shade {
                    FaceShade::Flat { rgb, .. } => rgb,
                    FaceShade::Textured { tint, .. } => tint,
                };
                [color; 3]
            }
        };
        let _ = emit_face_tri_lit_at_slot(scratch, p, uvs, shade, tri_colors, surface_slot);
        previous = current;
    }
}

fn visible_sky_aperture(
    project: &ProjectDocument,
    camera: psx_engine::WorldCamera,
    hidden_scene_nodes: &HashSet<NodeId>,
) -> bool {
    let scene = project.active_scene();
    with_cached_csg_surfaces(project, hidden_scene_nodes, |surfaces| {
        surfaces.iter().any(|cached| {
            if !preview_brush_bounds_visible(camera, cached.bounds) {
                return false;
            }
            let surface = &cached.surface;
            let Some(face) = scene
                .brushes
                .get(surface.source_brush)
                .and_then(|brush| brush.faces.get(surface.source_face))
            else {
                return false;
            };
            let aperture = face.material.is_some_and(|material| {
                project.resource(material).is_some_and(|resource| {
                    matches!(&resource.data, ResourceData::Material(material) if material.sky_aperture)
                })
            });
            aperture && preview_brush_polygon_visible(camera, &surface.vertices)
        })
    })
}

fn preview_brush_polygon_visible(camera: psx_engine::WorldCamera, vertices: &[[f64; 3]]) -> bool {
    const PREVIEW_COORD_LIMIT: f64 = 500_000.0;
    if vertices.len() < 3
        || vertices.len() > PREVIEW_FACE_VERTEX_CAP
        || vertices.iter().any(|vertex| {
            vertex
                .iter()
                .any(|coordinate| !coordinate.is_finite() || coordinate.abs() > PREVIEW_COORD_LIMIT)
        })
    {
        return false;
    }
    let mut clip_vertices = [EMPTY_PREVIEW_CLIP_VERTEX; PREVIEW_FACE_VERTEX_CAP];
    for (slot, vertex) in clip_vertices.iter_mut().zip(vertices) {
        *slot = PreviewClipVertex::new(
            camera.view_vertex(psx_engine::WorldVertex::new(
                vertex[0].round() as i32,
                vertex[1].round() as i32,
                vertex[2].round() as i32,
            )),
            [0.0; 2],
            (0, 0, 0),
        );
    }
    let clipped = clip_preview_brush_polygon(camera.projection, &clip_vertices[..vertices.len()]);
    if clipped.as_slice().len() < 3 {
        return false;
    }
    let mut min_x = i16::MAX;
    let mut max_x = i16::MIN;
    let mut min_y = i16::MAX;
    let mut max_y = i16::MIN;
    for vertex in clipped.as_slice() {
        let Some((projected, _, _)) = vertex.projected(camera.projection) else {
            return false;
        };
        let x = projected.sx;
        let y = projected.sy;
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    max_x >= 0 && min_x < SCREEN_W as i16 && max_y >= 0 && min_y < SCREEN_H as i16
}

fn walk_brushes(
    project: &ProjectDocument,
    textures: &EditorTextures,
    camera: psx_engine::WorldCamera,
    hidden_scene_nodes: &HashSet<NodeId>,
    scratch: &mut PreviewScratch,
) {
    walk_brushes_with_culling(project, textures, camera, hidden_scene_nodes, scratch, true);
}

fn walk_brushes_with_culling(
    project: &ProjectDocument,
    textures: &EditorTextures,
    camera: psx_engine::WorldCamera,
    hidden_scene_nodes: &HashSet<NodeId>,
    scratch: &mut PreviewScratch,
    cull_brush_bounds: bool,
) {
    // Live light preview: per-VERTEX analytic point lighting with the
    // exact Release-bake formula (lambert + linear falloff, shadow
    // segments against solid brushes, tint-modulated) and its ambient
    // contract (PXBSP_AMBIENT_RGB = [32; 3]). With zero lights the
    // historic unlit shading is kept, so lightless maps don't go dark.
    let lights = collect_bsp_preview_bake_lights(project, hidden_scene_nodes);
    let scene = project.active_scene();
    if lights.is_empty() {
        with_cached_csg_surfaces(project, hidden_scene_nodes, |surfaces| {
            for cached in surfaces {
                if scratch.geometry_full() {
                    break;
                }
                if cull_brush_bounds && !preview_brush_bounds_visible(camera, cached.bounds) {
                    continue;
                }
                let surface = &cached.surface;
                let Some(brush) = scene.brushes.get(surface.source_brush) else {
                    continue;
                };
                let Some(face) = brush.faces.get(surface.source_face) else {
                    continue;
                };
                if face.material.is_some_and(|material| {
                    project.resource(material).is_some_and(|resource| {
                        matches!(&resource.data, ResourceData::Material(material) if material.sky_aperture)
                    })
                }) {
                    continue;
                }
                let shade = face_shade(
                    project,
                    face.material,
                    brush_fallback_color(surface.source_face),
                    textures,
                );
                emit_brush_patch(
                    scratch,
                    camera,
                    &surface.plane,
                    &face.uv,
                    shade,
                    &surface.vertices,
                    None,
                );
            }
        });
        return;
    }
    let key = lit_preview_key(project, textures, &lights, hidden_scene_nodes);
    let cache = LIT_CACHE.get_or_init(|| {
        Mutex::new(PreviewLitCache {
            key: None,
            surfaces: Vec::new(),
        })
    });
    let mut cache = cache.lock().expect("preview lit cache");
    if cache.key != Some(key) {
        cache.surfaces = rebuild_lit_surfaces(project, textures, &lights, hidden_scene_nodes);
        cache.key = Some(key);
    }
    for lit_surface in &cache.surfaces {
        if scratch.geometry_full() {
            break;
        }
        if cull_brush_bounds && !preview_brush_bounds_visible(camera, lit_surface.bounds) {
            continue;
        }
        let Some(brush) = scene.brushes.get(lit_surface.source_brush) else {
            continue;
        };
        let Some(face) = brush.faces.get(lit_surface.source_face) else {
            continue;
        };
        let shade = face_shade(
            project,
            face.material,
            brush_fallback_color(lit_surface.source_face),
            textures,
        );
        for (verts, colors) in &lit_surface.patches {
            emit_brush_patch(
                scratch,
                camera,
                &lit_surface.plane,
                &face.uv,
                shade,
                verts,
                Some(colors),
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]

fn preview_projected_from_engine(
    projected: psx_engine::ProjectedVertex,
) -> psx_gte::scene::Projected {
    psx_gte::scene::Projected {
        sx: projected.sx,
        sy: projected.sy,
        sz: projected.sz.clamp(0, u16::MAX as i32) as u16,
    }
}

/// Per-frame tick used to advance animation phase for the
/// editor's looping model preview. Bumped once per
/// `build_phase1_frame` call. PSX angle / phase math needs
/// monotonic ticks rather than wall-clock, and the editor
/// frame rate fluctuates on host -- so this is "preview
/// frames", not real-time. Good enough for inspector preview.
static PREVIEW_START: OnceLock<Instant> = OnceLock::new();

fn preview_elapsed_vblanks() -> u32 {
    const VBLANKS_PER_SECOND: u128 = 60;
    let start = PREVIEW_START.get_or_init(Instant::now);
    let ticks = start
        .elapsed()
        .as_micros()
        .saturating_mul(VBLANKS_PER_SECOND)
        / 1_000_000;
    ticks.min(u128::from(u32::MAX)) as u32
}

/// One placed model the preview pass should render. Resolved
/// once per call to `walk_model_instances` so the per-instance
/// loop only does projection + emit work.
struct PreviewModelInstance<'a> {
    /// Cached parsed model. Owns no allocation; references
    /// bytes the caller keeps alive.
    model: psx_asset::Model<'a>,
    /// Cached parsed animation clip. Resolved through the
    /// preview / default clip rule.
    animation: psx_asset::Animation<'a>,
    /// Model atlas or replacement-material texture slot.
    texture: MaterialSlot,
    /// Render origin (world units). Model placement
    /// stays floor-anchored in `InstanceMeta`; this is lifted to
    /// the cooked model's centre before drawing.
    origin: psx_engine::WorldVertex,
    /// Y-axis rotation matrix used for mesh vertices.
    model_rotation: Mat3I16,
    /// Render-only scale from the ModelRenderer component.
    visual_scale_q8: u16,
    /// Lit and fogged model texture tint, matching editor-playtest's
    /// single-material actor lighting path.
    tint: (u8, u8, u8),
    /// PS1 optical blend selected by the authored material.
    blend_mode: BlendMode,
    /// Optional independently blended second texture pass.
    secondary_layer: Option<PreviewModelSecondaryLayer>,
    /// Authored model face-sidedness after material override.
    face_sidedness: psxed_project::MaterialFaceSidedness,
    /// Whether the editor preview should advance this instance's
    /// animation phase. Disabled instances hold `pose_frame`.
    autoplay: bool,
    /// Frame held when `autoplay` is false.
    pose_frame: u16,
}

fn walk_bsp_model_instances(
    project: &ProjectDocument,
    textures: &EditorTextures,
    assets: &crate::editor_assets::EditorAssets,
    selected: psxed_project::NodeId,
    camera: &psx_engine::WorldCamera,
    hidden_scene_nodes: &HashSet<NodeId>,
    tick: u32,
    character_motion: Option<psxed_ui::EditorCharacterMotionPreview>,
    scratch: &mut PreviewScratch,
) {
    let scene = project.active_scene();
    let lights = collect_bsp_preview_lights(project, hidden_scene_nodes);
    let fog = PreviewFog;
    let mut instances_meta: Vec<InstanceMeta> = Vec::new();
    for node in scene.nodes() {
        if instances_meta.len() >= MAX_PREVIEW_MODEL_INSTANCES {
            break;
        }
        let Some(reference) = preview_model_reference(scene, node) else {
            continue;
        };
        if preview_reference_hidden(
            scene,
            hidden_scene_nodes,
            node.id,
            reference.renderer_node,
            reference.animator_node,
            None,
        ) {
            continue;
        }
        let Some(model_resource) = project.resource(reference.model_id) else {
            continue;
        };
        let ResourceData::Model(model) = &model_resource.data else {
            continue;
        };
        if model.texture_path.is_none() {
            continue;
        }
        let Some(atlas_slot) = textures.model_atlas_slot(reference.model_id) else {
            continue;
        };
        let preview_transform = character_motion.filter(|preview| preview.entity == node.id);
        let requested_clip = preview_transform
            .map(|preview| preview.clip)
            .or(reference.clip_override)
            .unwrap_or(0);
        let Some(clip_local) = valid_preview_clip(
            requested_clip,
            project
                .resolved_model_animation_clips(reference.model_id)
                .len(),
        ) else {
            continue;
        };
        let translation = node.transform.translation;
        let mut origin = psx_engine::WorldVertex::new(
            translation[0].round() as i32,
            translation[1].round() as i32,
            translation[2].round() as i32,
        );
        let yaw_q12 = apply_character_motion_preview(
            node.id,
            &mut origin,
            yaw_to_q12(node.transform.rotation_degrees[1]),
            character_motion,
        );
        let model_rotation = euler_rotation_q12(
            yaw_to_q12(node.transform.rotation_degrees[0]),
            yaw_q12.wrapping_add(reference.visual_yaw_q12),
            yaw_to_q12(node.transform.rotation_degrees[2]),
        );
        instances_meta.push(InstanceMeta {
            mesh_id: reference.model_id,
            clip_local,
            origin,
            model_rotation,
            atlas: atlas_slot,
            material_override: reference.material_override,
            is_selected: preview_reference_selected(
                selected,
                node.id,
                reference.renderer_node,
                reference.animator_node,
                None,
            ),
            autoplay: preview_transform.is_some() || reference.autoplay,
            pose_frame: reference.pose_frame,
            yaw_q12,
            collision_radius: model.collision_radius as i32,
            world_height: model.world_height as i32,
            visual_offset: reference.visual_offset,
            visual_scale_q8: reference.visual_scale_q8,
        });
    }
    if instances_meta.is_empty() {
        return;
    }
    resolve_and_draw_model_instances(
        project,
        textures,
        assets,
        camera,
        fog,
        &lights,
        PXBSP_PREVIEW_AMBIENT,
        tick,
        &instances_meta,
        scratch,
    );
}

/// Shared tail of the model walkers: resolve cached mesh/clip bytes for
/// each gathered `InstanceMeta`, shade, and draw shadows, gizmos, and the
/// instances themselves. Requires the GTE to hold the camera matrix on
/// entry (the model pass leaves joint-space state behind).
#[allow(clippy::too_many_arguments)]
fn resolve_and_draw_model_instances(
    project: &ProjectDocument,
    textures: &EditorTextures,
    assets: &crate::editor_assets::EditorAssets,
    camera: &psx_engine::WorldCamera,
    fog: PreviewFog,
    lights: &[psx_engine::PointLightSample],
    ambient: [u8; 3],
    tick: u32,
    instances_meta: &[InstanceMeta],
    scratch: &mut PreviewScratch,
) {
    // Resolve parsed model + animation per instance straight
    // out of the cache. Each meta carries its own
    // `(mesh_id, clip_local)` pair so two instances of the
    // same model with different clips resolve to two different
    // animation entries -- fixes the prior shared-buffer bug
    // where whichever clip got loaded first won.
    let mut instances: Vec<PreviewModelInstance> = Vec::new();
    for meta in instances_meta {
        let Some(mesh_bytes) = assets.mesh_bytes(meta.mesh_id) else {
            continue;
        };
        let Some(anim_bytes) = assets.clip_bytes(meta.mesh_id, meta.clip_local) else {
            continue;
        };
        let Ok(model) = psx_asset::Model::from_bytes(mesh_bytes) else {
            continue;
        };
        let Ok(animation) = psx_asset::Animation::from_bytes(anim_bytes) else {
            continue;
        };
        let origin = visual_model_origin(
            meta.origin,
            model.bind_pose_floor_lift(),
            preview_local_to_world(&model, meta.visual_scale_q8),
            meta.visual_offset,
            meta.model_rotation,
        );
        let material_override =
            preview_model_material_override(project, textures, meta.material_override);
        let texture = material_override
            .and_then(|material| material.texture)
            .unwrap_or(meta.atlas);
        let base_tint = material_override
            .map(|material| material.tint)
            .unwrap_or((0x80, 0x80, 0x80));
        instances.push(PreviewModelInstance {
            model,
            animation,
            texture,
            origin,
            model_rotation: meta.model_rotation,
            visual_scale_q8: meta.visual_scale_q8,
            tint: shade_model_tint(origin, *camera, fog, lights, ambient, base_tint),
            blend_mode: material_override
                .map(|material| material.blend_mode)
                .unwrap_or(BlendMode::Opaque),
            secondary_layer: material_override.and_then(|material| {
                material.secondary_layer.map(|mut layer| {
                    layer.tint =
                        shade_model_tint(origin, *camera, fog, lights, ambient, layer.tint);
                    layer
                })
            }),
            face_sidedness: material_override
                .map(|material| material.face_sidedness)
                .unwrap_or_else(|| {
                    if model.double_sided() {
                        psxed_project::MaterialFaceSidedness::Both
                    } else {
                        psxed_project::MaterialFaceSidedness::Front
                    }
                }),
            autoplay: meta.autoplay,
            pose_frame: meta.pose_frame,
        });
    }

    let shadow_slot = textures.shadow_slot();
    for meta in instances_meta {
        draw_model_shadow(meta, shadow_slot, *camera, scratch);
    }

    // Gizmos first while GTE still holds the camera matrix --
    // the engine model pass overrides rotation/translation
    // per joint so any project_vertex after a model render uses
    // joint-space, not world-space.
    for meta in instances_meta {
        if meta.is_selected {
            draw_model_selection_gizmo(meta, scratch);
        }
    }
    draw_preview_model_instances(camera, tick, &instances, scratch);
}

/// For every legacy Player Spawn or component player controller,
/// resolve its `character` link to a Model + idle clip and queue
/// an `InstanceMeta` so the same render path placed model instances
/// follow renders the character at the spawn. `(mesh_id, clip_local)`
/// is the cache key -- different player idle clips and different
/// placed-instance clips each resolve to their own animation entry.
///
/// Resolution rule mirrors the cooker:
/// 1. Explicit `character` assignment wins.
/// 2. If unset and exactly one Character resource exists,
///    auto-pick it.
/// 3. Otherwise skip the preview (the cook step's validation
///    will surface the missing character).

fn apply_character_motion_preview(
    entity: NodeId,
    origin: &mut psx_engine::WorldVertex,
    authored_yaw_q12: u16,
    preview: Option<psxed_ui::EditorCharacterMotionPreview>,
) -> u16 {
    let Some(preview) = preview.filter(|preview| preview.entity == entity) else {
        return authored_yaw_q12;
    };
    *origin = psx_engine::WorldVertex::new(preview.origin[0], preview.origin[1], preview.origin[2]);
    preview.yaw_q12
}

/// Selection gizmo for a placed model: a cyan vertical line
/// at the origin (visible against any backdrop) plus a yellow
/// forward arrow showing the yaw direction. The model itself
/// draws underneath the gizmo via the OT depth slot system.
///
/// Restores the camera GTE rotation/translation before
/// projecting because the engine model pass left the
/// GTE primed with the *last part's* joint transform.
fn draw_model_selection_gizmo(meta: &InstanceMeta, scratch: &mut PreviewScratch) {
    // Re-prime the GTE with the camera transform -- model
    // rendering left it set to the last joint's view.
    // `world_to_view` already does the anchor subtract so we
    // just need rotation+translation back to camera basis.
    // Cheap: re-derive from VIEW_ANCHOR + the existing camera
    // matrix is harder than just calling project_vertex with
    // the camera setup. Skip the explicit restore and use
    // the existing set_view_anchor → world_to_view pipeline
    // by projecting via gte_scene::project_vertex with the
    // camera matrix re-loaded explicitly.
    //
    // Pragmatic shortcut: emit screen-space lines built from
    // worldspace endpoints projected with `gte_scene::project_vertex`
    // after we restore the camera transform via setup_gte_for_camera.
    // We don't have access to the camera state here, so the gizmo
    // routes through the same world_to_view + project_vertex path
    // the room geometry uses *before* model rendering kicks in.
    // To make this work we run gizmo emit *before* model render
    // in the caller; for now route it through and accept that
    // gizmos may use the last-joint transform if rendered after
    // the model. We'll fix ordering in the caller.

    let height = meta.world_height.max(256);
    let origin_w = [meta.origin.x, meta.origin.y, meta.origin.z];
    let top_w = [meta.origin.x, meta.origin.y - height, meta.origin.z];
    let mid_w = [meta.origin.x, meta.origin.y - height / 4, meta.origin.z];
    let len = (height / 3).max(128);
    let s = sin_q12_turn(meta.yaw_q12);
    let c = cos_q12_turn(meta.yaw_q12);
    let forward_w = [
        meta.origin.x + ((s * len) >> 12),
        meta.origin.y - height / 4,
        meta.origin.z + ((c * len) >> 12),
    ];

    let origin_p = gte_scene::project_vertex(world_to_view(origin_w));
    let top_p = gte_scene::project_vertex(world_to_view(top_w));
    let mid_p = gte_scene::project_vertex(world_to_view(mid_w));
    let forward_p = gte_scene::project_vertex(world_to_view(forward_w));

    let cyan = FaceOutlineStyle {
        rgb: (0x40, 0xC8, 0xE8),
        thickness_px: EDITOR_PREVIEW_SELECTED_STROKE_WIDTH,
    };
    let yellow = FaceOutlineStyle {
        rgb: (0xF0, 0xC8, 0x40),
        thickness_px: EDITOR_PREVIEW_SELECTED_STROKE_WIDTH,
    };
    if origin_p.sz != 0 && top_p.sz != 0 {
        push_screen_line(scratch, origin_p, top_p, cyan);
    }
    if mid_p.sz != 0 && forward_p.sz != 0 {
        push_screen_line(scratch, mid_p, forward_p, yellow);
    }
}

fn draw_model_shadow(
    meta: &InstanceMeta,
    slot: MaterialSlot,
    camera: psx_engine::WorldCamera,
    scratch: &mut PreviewScratch,
) {
    let radius = preview_shadow_radius(meta.collision_radius);
    if radius <= 0 {
        return;
    }

    let x = meta.origin.x;
    let y = meta.origin.y.saturating_add(PREVIEW_SHADOW_FLOOR_LIFT);
    let z = meta.origin.z;
    let verts = [
        [x.saturating_sub(radius), y, z.saturating_sub(radius)],
        [x.saturating_add(radius), y, z.saturating_sub(radius)],
        [x.saturating_add(radius), y, z.saturating_add(radius)],
        [x.saturating_sub(radius), y, z.saturating_add(radius)],
    ];
    if !preview_vertices_in_front(camera, &verts) {
        return;
    }
    let projected = [
        gte_scene::project_vertex(world_to_view(verts[0])),
        gte_scene::project_vertex(world_to_view(verts[1])),
        gte_scene::project_vertex(world_to_view(verts[2])),
        gte_scene::project_vertex(world_to_view(verts[3])),
    ];
    if projected.iter().any(|p| p.sz == 0) {
        return;
    }

    const UVS: [(u8, u8); 4] = [
        (0, 0),
        (PREVIEW_SHADOW_UV_MAX, 0),
        (PREVIEW_SHADOW_UV_MAX, PREVIEW_SHADOW_UV_MAX),
        (0, PREVIEW_SHADOW_UV_MAX),
    ];
    push_shadow_tex_tri(
        scratch,
        [projected[0], projected[1], projected[2]],
        [UVS[0], UVS[1], UVS[2]],
        slot,
    );
    push_shadow_tex_tri(
        scratch,
        [projected[0], projected[2], projected[3]],
        [UVS[0], UVS[2], UVS[3]],
        slot,
    );
}

fn preview_shadow_radius(base_radius: i32) -> i32 {
    base_radius
        .saturating_mul(PREVIEW_SHADOW_RADIUS_SCALE_NUM)
        .checked_div(PREVIEW_SHADOW_RADIUS_SCALE_DEN)
        .unwrap_or(base_radius)
        .clamp(PREVIEW_SHADOW_RADIUS_MIN, PREVIEW_SHADOW_RADIUS_MAX)
}

struct InstanceMeta {
    mesh_id: ResourceId,
    /// Clip index inside the model's clip list. Two instances
    /// of the same model with different clip overrides carry
    /// different `clip_local` values, which keys the
    /// `EditorAssets::clip_bytes` lookup so each instance's
    /// animation lands separately.
    clip_local: u16,
    origin: psx_engine::WorldVertex,
    model_rotation: Mat3I16,
    atlas: MaterialSlot,
    /// Material resource selected by the ModelRenderer, if any.
    material_override: Option<ResourceId>,
    /// `true` when the placed instance is the currently
    /// selected scene node. Drives the selection gizmo.
    is_selected: bool,
    /// Yaw in PSX angle units, retained for the facing arrow.
    yaw_q12: u16,
    /// Frame to hold when `autoplay` is false.
    pose_frame: u16,
    /// Ground-contact radius used for the editor shadow decal.
    collision_radius: i32,
    /// Approximate world-space height for the facing arrow's
    /// vertical extent. Lifted from `ModelResource::world_height`.
    world_height: i32,
    /// Render-only calibration copied from ModelRenderer.
    visual_offset: [i16; 3],
    visual_scale_q8: u16,
    autoplay: bool,
}

/// The mesh's composed local-to-world scale, matching the runtime's
/// `visual_model_local_to_world`.
fn preview_local_to_world(
    model: &psx_asset::Model<'_>,
    visual_scale_q8: u16,
) -> psx_engine::LocalToWorldScale {
    let q12 = (model.local_to_world_q12() as u32 * visual_scale_q8.max(1) as u32
        + (psxed_project::MODEL_SCALE_ONE_Q8 as u32 / 2))
        / psxed_project::MODEL_SCALE_ONE_Q8 as u32;
    psx_engine::LocalToWorldScale::from_q12(q12.clamp(1, u16::MAX as u32) as u16)
}

fn visual_model_origin(
    origin: psx_engine::WorldVertex,
    floor_lift: i32,
    local_to_world: psx_engine::LocalToWorldScale,
    visual_offset: [i16; 3],
    instance_rotation: Mat3I16,
) -> psx_engine::WorldVertex {
    // Same rule as the runtime: the origin sits the bind pose's
    // origin-to-feet distance (model units) above the floor point, scaled
    // with the SAME local-to-world the mesh is drawn with.
    let origin = psx_engine::WorldVertex::new(
        origin.x,
        origin
            .y
            .saturating_add(local_to_world.apply(floor_lift.max(0))),
        origin.z,
    );
    let offset = rotate_visual_offset(instance_rotation, visual_offset);
    psx_engine::WorldVertex::new(
        origin.x.saturating_add(offset[0]),
        origin.y.saturating_add(offset[1]),
        origin.z.saturating_add(offset[2]),
    )
}

fn rotate_visual_offset(rotation: Mat3I16, offset: [i16; 3]) -> [i32; 3] {
    let offset = [offset[0] as i32, offset[1] as i32, offset[2] as i32];
    let row = |r: [i16; 3]| -> i32 {
        let x = (r[0] as i32).saturating_mul(offset[0]);
        let y = (r[1] as i32).saturating_mul(offset[1]);
        let z = (r[2] as i32).saturating_mul(offset[2]);
        x.saturating_add(y).saturating_add(z) >> 12
    };
    [row(rotation.m[0]), row(rotation.m[1]), row(rotation.m[2])]
}

/// Convert editor-Y rotation in degrees to PSX angle units (Q12, 4096 per
/// turn). Delegates to the shared converter so preview and the playtest
/// writer can't diverge.
fn yaw_to_q12(degrees: f32) -> u16 {
    psxed_project::spatial::euler_degrees_to_q12(degrees)
}

/// Y-axis rotation matrix in Q12. Mirrors `yaw_rotation_matrix`
/// in editor-playtest's runtime.
fn yaw_rotation_q12(yaw_q12: u16) -> Mat3I16 {
    let s = clamp_i16(sin_q12_turn(yaw_q12));
    let c = clamp_i16(cos_q12_turn(yaw_q12));
    Mat3I16 {
        m: [[c, 0, s], [0, 0x1000, 0], [-s, 0, c]],
    }
}

/// Full instance rotation `Rz(roll) * Ry(yaw) * Rx(pitch)` in Q12.
/// Mirrors the runtime's `euler_q12_rotation` so a pitched/rolled
/// model prop previews exactly as it ships; keeps the cheaper
/// single-axis build for the common upright case.
fn euler_rotation_q12(pitch_q12: u16, yaw_q12: u16, roll_q12: u16) -> Mat3I16 {
    if pitch_q12 == 0 && roll_q12 == 0 {
        return yaw_rotation_q12(yaw_q12);
    }
    let rx = Mat3I16::rotate_x(psx_engine::Angle::from_q12(pitch_q12).rotate_y_arg());
    let ry = Mat3I16::rotate_y(psx_engine::Angle::from_q12(yaw_q12).rotate_y_arg());
    let rz = Mat3I16::rotate_z(psx_engine::Angle::from_q12(roll_q12).rotate_y_arg());
    rz.mul(&ry).mul(&rx)
}

/// Submit all preview models through the same engine model pass used
/// by editor-playtest. The editor keeps its own entry point and OT
/// lifetime, but model projection, culling, UV handling, and packet
/// emission now live behind `psx-engine`.
///
/// IMPORTANT: this clobbers the GTE rotation/translation
/// registers, so any caller relying on the camera-target
/// transform set by `setup_gte_for_camera` must restore it before
/// projecting non-model geometry.
fn draw_preview_model_instances(
    camera: &psx_engine::WorldCamera,
    tick: u32,
    instances: &[PreviewModelInstance<'_>],
    scratch: &mut PreviewScratch,
) {
    // BSP/brush geometry and animated models have independent packet arenas.
    // A dense BSP can legitimately fill `tex_tris` while `model_tex_tris` is
    // still empty; checking the world counter here used to hide every model
    // preview in a full Quake level.
    if instances.is_empty() || !preview_model_triangle_capacity_available(scratch.model_tex_used) {
        return;
    }

    let tex_start = scratch.model_tex_used;
    let mut triangles = psx_engine::PrimitiveArena::new(&mut scratch.model_tex_tris[tex_start..]);
    let mut model_commands = [psx_engine::WorldTriCommand::EMPTY; PREVIEW_MODEL_COMMAND_CAP];
    let mut ot = psx_engine::OtFrame::resume(&mut scratch.ot);
    let mut world = psx_engine::WorldRenderPass::new_deferred_sorted(&mut ot, &mut model_commands);

    for instance in instances {
        if submit_preview_model_instance(
            &mut world,
            &mut triangles,
            camera,
            tick,
            instance,
            &mut scratch.model_vertices,
            &mut scratch.model_faces,
            &mut scratch.model_parts,
            &mut scratch.model_source_vertices,
            &mut scratch.model_joint_transforms,
        ) {
            break;
        }
    }

    world.flush();
    scratch.model_tex_used = tex_start.saturating_add(triangles.len()).min(TRI_CAP);
}

fn submit_preview_model_instance(
    world: &mut psx_engine::WorldRenderPass<'_, '_, OT_DEPTH>,
    triangles: &mut psx_engine::PrimitiveArena<'_, TriTextured>,
    camera: &psx_engine::WorldCamera,
    tick: u32,
    instance: &PreviewModelInstance<'_>,
    projected_vertices: &mut [psx_engine::ProjectedVertex],
    face_pool: &mut [psx_engine::TexturedModelRenderFace],
    part_pool: &mut [psx_asset::ModelPart],
    source_vertex_pool: &mut [psx_asset::ModelVertex],
    joint_view_transforms: &mut [psx_engine::JointViewTransform],
) -> bool {
    // Parked animators hold their authored pose frame. Same convention as the
    // runtime's model instances: whole frames in the high bits of a Q12 phase,
    // clamped so an authored frame past the end of a shorter clip still draws.
    let frame_q12 = if instance.autoplay {
        instance.animation.phase_at_tick_q12(tick, 60)
    } else {
        (instance
            .pose_frame
            .min(instance.animation.frame_count().saturating_sub(1)) as u32)
            << 12
    };
    let material = TextureMaterial::opaque(
        instance.texture.clut_word,
        instance.texture.tpage_word,
        instance.tint,
    )
    .with_texture_window(instance.texture.texture_window)
    .with_blend_mode(instance.blend_mode);
    let options = preview_model_surface_options(material, instance.face_sidedness);
    let Some((geometry, faces)) = predecode_preview_model_geometry_faces(
        instance.model,
        face_pool,
        part_pool,
        source_vertex_pool,
    ) else {
        return false;
    };

    let secondary_layer = instance.secondary_layer.map(|layer| {
        let layer_material = TextureMaterial::blended(
            layer.texture.clut_word,
            layer.texture.tpage_word,
            layer.tint,
            layer.blend_mode,
        )
        .with_texture_window(layer.texture.texture_window);
        let [u, v] = layer.motion.offset_at_tick(tick, 60);
        psx_engine::TexturedModelLayer::new(layer_material)
            .with_uv_offset(psx_engine::ModelUvOffset::new(u, v))
    });
    let stats = world.submit_textured_model_predecoded_geometry_faces_layered(
        triangles,
        instance.model,
        instance.animation,
        frame_q12,
        *camera,
        instance.origin,
        instance.model_rotation,
        preview_model_local_to_world(instance.model, instance.visual_scale_q8),
        psx_engine::ModelPoseTranslation::ZERO,
        projected_vertices,
        joint_view_transforms,
        material,
        secondary_layer,
        options,
        faces,
        geometry,
        None,
    );
    stats.primitive_overflow || stats.command_overflow || stats.vertex_overflow
}

fn preview_model_triangle_capacity_available(model_tex_used: usize) -> bool {
    model_tex_used < TRI_CAP
}

fn preview_model_local_to_world(
    model: psx_asset::Model<'_>,
    visual_scale_q8: u16,
) -> psx_engine::LocalToWorldScale {
    let q12 = (model.local_to_world_q12() as u32)
        .saturating_mul(visual_scale_q8.max(1) as u32)
        .saturating_add((psxed_project::MODEL_SCALE_ONE_Q8 / 2) as u32)
        / psxed_project::MODEL_SCALE_ONE_Q8 as u32;
    psx_engine::LocalToWorldScale::from_q12(q12.clamp(1, u16::MAX as u32) as u16)
}

fn predecode_preview_model_geometry_faces<'a>(
    model: psx_asset::Model<'_>,
    face_pool: &'a mut [psx_engine::TexturedModelRenderFace],
    part_pool: &'a mut [psx_asset::ModelPart],
    vertex_pool: &'a mut [psx_asset::ModelVertex],
) -> Option<(
    psx_engine::TexturedModelGeometry<'a>,
    &'a [psx_engine::TexturedModelRenderFace],
)> {
    let part_count = model.part_count() as usize;
    let vertex_count = model.vertex_count() as usize;
    let face_count = model.face_count() as usize;
    if part_pool.len() < part_count
        || vertex_pool.len() < vertex_count
        || face_pool.len() < face_count
    {
        return None;
    }

    let mut i = 0usize;
    while i < part_count {
        part_pool[i] = model.part(i as u16)?;
        i += 1;
    }
    i = 0;
    while i < vertex_count {
        vertex_pool[i] = model.vertex(i as u16)?;
        i += 1;
    }

    let (max_u, max_v) = preview_model_uv_limits(model);
    i = 0;
    while i < face_count {
        let face = model.face(i as u16)?;
        face_pool[i] = psx_engine::TexturedModelRenderFace::new_with_palette_bank(
            [
                face.corners[0].vertex_index,
                face.corners[1].vertex_index,
                face.corners[2].vertex_index,
            ],
            [
                clamp_preview_model_uv(face.corners[0].uv, max_u, max_v),
                clamp_preview_model_uv(face.corners[1].uv, max_u, max_v),
                clamp_preview_model_uv(face.corners[2].uv, max_u, max_v),
            ],
            model.face_palette_bank(i as u16)?,
        );
        i += 1;
    }

    Some((
        psx_engine::TexturedModelGeometry::new(
            &part_pool[..part_count],
            &vertex_pool[..vertex_count],
        ),
        &face_pool[..face_count],
    ))
}

fn preview_model_uv_limits(model: psx_asset::Model<'_>) -> (u8, u8) {
    (
        preview_model_uv_max(model.texture_width()),
        preview_model_uv_max(model.texture_height()),
    )
}

fn preview_model_uv_max(size: u16) -> u8 {
    size.saturating_sub(1).min(u16::from(u8::MAX)) as u8
}

fn clamp_preview_model_uv(uv: (u8, u8), max_u: u8, max_v: u8) -> (u8, u8) {
    (uv.0.min(max_u), uv.1.min(max_v))
}

fn preview_model_surface_options(
    material: TextureMaterial,
    face_sidedness: psxed_project::MaterialFaceSidedness,
) -> psx_engine::WorldSurfaceOptions {
    let cull_mode = match face_sidedness {
        psxed_project::MaterialFaceSidedness::Front => psx_engine::CullMode::Back,
        psxed_project::MaterialFaceSidedness::Back => psx_engine::CullMode::Front,
        psxed_project::MaterialFaceSidedness::Both => psx_engine::CullMode::None,
    };
    psx_engine::WorldSurfaceOptions::new(
        psx_engine::DepthBand::new(PREVIEW_GEOMETRY_SLOT_MIN, PREVIEW_GEOMETRY_SLOT_MAX),
        psx_engine::DepthRange::new(
            (PREVIEW_GEOMETRY_SLOT_MIN as i32) << 2,
            (PREVIEW_GEOMETRY_SLOT_MAX as i32) << 2,
        ),
    )
    .with_depth_policy(psx_engine::DepthPolicy::Average)
    .with_cull_mode(cull_mode)
    .with_material_layer(material)
    .with_textured_triangle_splitting(false)
}

fn shade_model_tint(
    origin: psx_engine::WorldVertex,
    camera: psx_engine::WorldCamera,
    fog: PreviewFog,
    lights: &[psx_engine::PointLightSample],
    ambient: [u8; 3],
    base_tint: (u8, u8, u8),
) -> (u8, u8, u8) {
    let lit = psx_engine::shade_material_tint_with_lights(
        psx_engine::MaterialTint::from_tuple(base_tint),
        [origin.x, origin.y, origin.z],
        psx_engine::Rgb8::from_array(ambient),
        lights.iter().copied(),
    )
    .to_tuple();
    fog.apply_rgb(lit, camera.view_vertex(origin).z)
}

#[derive(Clone, Copy)]
struct PreviewModelMaterialOverride {
    texture: Option<MaterialSlot>,
    blend_mode: BlendMode,
    tint: (u8, u8, u8),
    secondary_layer: Option<PreviewModelSecondaryLayer>,
    face_sidedness: psxed_project::MaterialFaceSidedness,
}

#[derive(Clone, Copy)]
struct PreviewModelSecondaryLayer {
    texture: MaterialSlot,
    blend_mode: BlendMode,
    tint: (u8, u8, u8),
    motion: psxed_project::MaterialUvMotion,
}

fn preview_model_material_override(
    project: &ProjectDocument,
    textures: &EditorTextures,
    material_id: Option<ResourceId>,
) -> Option<PreviewModelMaterialOverride> {
    let material_id = material_id?;
    let material = project
        .resource(material_id)
        .and_then(|resource| match &resource.data {
            ResourceData::Material(material) => Some(material),
            _ => None,
        })?;
    let replacement_texture = match material.texture_mode {
        // An image-less simple material deliberately inherits the model atlas.
        psxed_project::MaterialTextureMode::SimpleImage => material
            .psxt_path
            .as_deref()
            .is_some_and(|path| !path.trim().is_empty())
            .then(|| textures.slot(material_id))
            .flatten(),
        // Generated and probe materials have no `psxt_path`: their preview
        // texture is baked directly into the editor texture cache.
        psxed_project::MaterialTextureMode::Generated
        | psxed_project::MaterialTextureMode::Transition
        | psxed_project::MaterialTextureMode::ReflectiveProbe => textures.slot(material_id),
    };
    Some(PreviewModelMaterialOverride {
        texture: replacement_texture,
        blend_mode: psx_blend_mode(material.blend_mode),
        tint: (material.tint[0], material.tint[1], material.tint[2]),
        secondary_layer: material.enabled_secondary_layer().and_then(|layer| {
            Some(PreviewModelSecondaryLayer {
                texture: textures.secondary_slot(material_id)?,
                blend_mode: psx_blend_mode(layer.blend_mode),
                tint: (layer.tint[0], layer.tint[1], layer.tint[2]),
                motion: layer.motion,
            })
        }),
        face_sidedness: material.sidedness(),
    })
}
