//! Editor 3D viewport -- Phase 1 sector renderer.
//!
//! Walks the editor's active Room and feeds the editor-owned
//! [`HwRenderer`](psx_gpu_render::HwRenderer) the same way runtime
//! PS1 code does:
//!
//! 1. Configure the GTE for the editor camera (RT / TR / OFX / OFY / H).
//! 2. For every populated sector with a floor, project the four
//!    corners through the host GTE shim ([`psx_gte::scene::project_vertex`]).
//! 3. Emit two `TriFlat` packets per floor, coloured from the
//!    sector's material base colour.
//! 4. Insert each packet into an `OrderingTable` keyed on average
//!    depth.
//! 5. Walk the OT via `iter_packets`, build a `GpuCmdLogEntry` log,
//!    hand it to `psx-gpu-render::HwRenderer::render_frame`.
//!
//! Scene geometry stays on the PSX-style path. Editor-only
//! affordances such as bounds, selection, and paint previews are
//! returned as host-drawn overlay lines so they can use fractional UI
//! strokes without PSX integer-pixel limitations.

use std::collections::{HashSet, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use emulator_core::gpu::GpuCmdLogEntry;
use psx_gpu::material::{BlendMode, TextureMaterial};
use psx_gpu::ot::OrderingTable;
use psx_gpu::prim::QuadFlat;
use psx_gpu::prim::QuadGouraud;
use psx_gpu::prim::QuadTexturedMaterial;
use psx_gpu::prim::TriFlat;
use psx_gpu::prim::TriTextured;
use psx_gte::math::{Mat3I16, Vec3I16, Vec3I32};
use psx_gte::scene as gte_scene;

use psxed_project::floor_view;
use psxed_project::portal_rooms::{
    plan_portal_rooms, portal_seam_edges_for_edge, portal_seam_edges_for_node, PortalEdge,
    PortalRoomConfig,
};
use psxed_project::{
    spatial, Corner, GridDirection, GridSector, GridSplit, GridUvTransform, NodeId, NodeKind,
    ProjectDocument, ResourceData, ResourceId, Scene, SceneNode, Transform3, WallCorner, WorldGrid,
};

use crate::editor_textures::{EditorTextures, MaterialSlot};
use psxed_ui::{PaintCellPreviewKind, ViewportCameraState};

mod backdrop;
mod camera;
mod overlays;
mod particles;
mod portals;
mod primitives;
mod room_geometry;
mod visible_rooms;

use backdrop::*;
use camera::*;
use overlays::*;
use particles::*;
use portals::*;
use primitives::*;
use room_geometry::*;
use visible_rooms::*;

/// Maximum sectors we'll attempt to render in one preview pass.
/// 64×64 grid would already be enormous for PSX (~16 MiB cooked); a
/// 4096-cap caps the per-frame primitive count at a comfortable
/// number for the host renderer.
const TRI_CAP: usize = 4096;
const EDITOR_PREVIEW_PORTAL_DEPTH: usize = 3;
const EDITOR_PREVIEW_MAX_ROOMS: usize = 16;
const SKY_QUAD_CAP: usize = psxed_project::SKY_CYCLORAMA_QUAD_MAX;
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
/// per-frame chain-walk cost. 256 slots is plenty for an orbit-camera
/// view where the front-to-back range is a small multiple of the
/// sector size.
const OT_DEPTH: usize = 256;
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
const GRID_TILE_UV: u8 = 64;
const PREVIEW_FLOOR_UVS: [(u8, u8); 4] = [
    (0, 0),
    (GRID_TILE_UV, 0),
    (GRID_TILE_UV, GRID_TILE_UV),
    (0, GRID_TILE_UV),
];
const PREVIEW_WALL_UVS: [(u8, u8); 4] = [
    (0, GRID_TILE_UV),
    (GRID_TILE_UV, GRID_TILE_UV),
    (GRID_TILE_UV, 0),
    (0, 0),
];
const BOX_PROP_FACE_VERTEX_INDICES: [[usize; 4]; psxed_project::BOX_PROP_FACE_COUNT] = [
    [4, 5, 1, 0],
    [5, 6, 2, 1],
    [6, 7, 3, 2],
    [7, 4, 0, 3],
    [7, 6, 5, 4],
    [0, 1, 2, 3],
];
const BOX_PROP_EDGE_VERTEX_INDICES: [(usize, usize); 12] = [
    (0, 1),
    (1, 2),
    (2, 3),
    (3, 0),
    (4, 5),
    (5, 6),
    (6, 7),
    (7, 4),
    (0, 4),
    (1, 5),
    (2, 6),
    (3, 7),
];
const EDITOR_PREVIEW_HOVER_STROKE_WIDTH: f32 = 1.5;
const EDITOR_PREVIEW_SELECTED_STROKE_WIDTH: f32 = 3.0;
const EDITOR_PREVIEW_PAINT_STROKE_WIDTH: f32 = 2.0;

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
    sky_quads: [QuadGouraud; SKY_QUAD_CAP],
    far_vista_quads: [QuadFlat; FAR_VISTA_QUAD_CAP],
    tris: [TriFlat; TRI_CAP],
    tex_tris: [TriTextured; TRI_CAP],
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

const EMPTY_TRI: TriFlat = TriFlat::new([(0, 0), (0, 0), (0, 0)], 0, 0, 0);
const EMPTY_FAR_VISTA_QUAD: QuadFlat = QuadFlat::new([(0, 0), (0, 0), (0, 0), (0, 0)], 0, 0, 0);
const EMPTY_SKY_QUAD: QuadGouraud = QuadGouraud::new(
    [(0, 0), (0, 0), (0, 0), (0, 0)],
    [(0, 0, 0), (0, 0, 0), (0, 0, 0), (0, 0, 0)],
);
const EMPTY_TEX_TRI: TriTextured = TriTextured::new(
    [(0, 0), (0, 0), (0, 0)],
    [(0, 0), (0, 0), (0, 0)],
    0,
    0,
    (0x80, 0x80, 0x80),
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
        std::ptr::addr_of_mut!((*ptr).sky_quads).write([EMPTY_SKY_QUAD; SKY_QUAD_CAP]);
        std::ptr::addr_of_mut!((*ptr).far_vista_quads)
            .write([EMPTY_FAR_VISTA_QUAD; FAR_VISTA_QUAD_CAP]);
        std::ptr::addr_of_mut!((*ptr).tris).write([EMPTY_TRI; TRI_CAP]);
        std::ptr::addr_of_mut!((*ptr).tex_tris).write([EMPTY_TEX_TRI; TRI_CAP]);
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
pub struct EditorPreviewFrame {
    /// PSX-style command log for the scene itself.
    pub cmd_log: Vec<GpuCmdLogEntry>,
    /// Host UI overlay lines for editor-only affordances.
    pub overlay_lines: Vec<psxed_ui::EditorViewportOverlayLine>,
}

/// Build a fresh preview frame rendering the active editor room window
/// from `camera`'s orbit angles.
///
/// BSP brushes are world-space geometry and do not require a legacy Room.
/// Even an empty scene emits its clear/sky frame so a project switch cannot
/// leave the persistent preview target showing pixels from the old scene.
#[allow(clippy::too_many_arguments)]
pub fn build_phase1_frame(
    project: &ProjectDocument,
    camera: ViewportCameraState,
    preview_fog: bool,
    preview_backface_wireframe: bool,
    preview_bounds: bool,
    show_grid: bool,
    show_portals: bool,
    show_lights: bool,
    hidden_scene_nodes: &HashSet<NodeId>,
    active_room: Option<psxed_project::NodeId>,
    active_floor: usize,
    selected: psxed_project::NodeId,
    character_motion: Option<psxed_ui::EditorCharacterMotionPreview>,
    hovered_primitive: Option<psxed_ui::Selection>,
    selected_primitive: Option<psxed_ui::Selection>,
    selected_primitives: &[psxed_ui::Selection],
    validation_issue_primitives: &[psxed_ui::Selection],
    selected_bounds: Option<([f32; 3], [f32; 3])>,
    selected_sector_faces: &[psxed_ui::FaceRef],
    paint_target_preview: Option<psxed_ui::PaintTargetPreview>,
    entity_bounds: &[psxed_ui::EntityBounds],
    hovered_entity_node: Option<psxed_project::NodeId>,
    textures: &EditorTextures,
    assets: &crate::editor_assets::EditorAssets,
) -> EditorPreviewFrame {
    let visible_rooms = preview_room_grids(
        project,
        hidden_scene_nodes,
        active_room,
        active_floor,
        selected,
        selected_primitive,
        selected_primitives,
        validation_issue_primitives,
    );
    let first_room = visible_rooms.first();
    let preview_context_node = first_room
        .map(|entry| entry.room)
        .unwrap_or_else(|| project.active_scene().root);
    let preview_context_fog = first_room
        .map(|entry| (entry.grid.fog_enabled && preview_fog, entry.grid.fog_color))
        .unwrap_or((false, [0; 3]));

    let mut scratch = preview_scratch()
        .lock()
        .expect("editor preview scratch mutex");
    scratch.used = 0;
    scratch.sky_used = 0;
    scratch.far_vista_used = 0;
    scratch.tex_used = 0;
    scratch.particle_used = 0;
    scratch.overlay_lines.clear();
    scratch.ot.clear();

    let world_camera = setup_gte_for_camera(camera);
    let resolved_sky = project
        .active_scene()
        .world_sky_for_node(preview_context_node)
        .unwrap_or_default()
        .resolved_for_room(preview_context_fog.0, preview_context_fog.1);
    push_clear(&mut scratch, resolved_sky.lower_color);
    push_cyclorama(&mut scratch, resolved_sky, world_camera);
    let resolved_far_vista = project
        .active_scene()
        .world_far_vista_for_node(preview_context_node)
        .unwrap_or_default()
        .resolved_for_room(preview_context_fog.0, preview_context_fog.1);
    push_far_vista_ring(
        &mut scratch,
        camera,
        world_camera,
        resolved_far_vista,
        textures,
    );
    let preview_tick = preview_elapsed_vblanks();
    // World-space brushes render once, against the camera GTE state
    // installed above (rooms and their local offsets do not apply).
    walk_brushes(project, textures, world_camera, &mut scratch);
    for floor_entry in &visible_rooms {
        if scratch.geometry_full() {
            break;
        }
        let room_id = floor_entry.room;
        let grid = floor_entry.grid;
        let y_offset = floor_entry.y_offset;
        let floor_index = floor_entry.floor_index;
        let fog = PreviewFog::from_grid(grid, preview_fog);
        walk_room(
            project,
            room_id,
            grid,
            y_offset,
            textures,
            world_camera,
            fog,
            preview_backface_wireframe,
            hidden_scene_nodes,
            &mut scratch,
        );
        walk_image_props(
            project,
            room_id,
            grid,
            floor_index,
            y_offset,
            textures,
            world_camera,
            hidden_scene_nodes,
            selected,
            hovered_entity_node,
            preview_bounds,
            &mut scratch,
        );
        walk_box_props(
            project,
            room_id,
            grid,
            floor_index,
            y_offset,
            textures,
            world_camera,
            fog,
            hidden_scene_nodes,
            selected,
            hovered_entity_node,
            preview_bounds,
            &mut scratch,
        );
        walk_cylinder_props(
            project,
            room_id,
            grid,
            floor_index,
            y_offset,
            textures,
            world_camera,
            fog,
            hidden_scene_nodes,
            selected,
            hovered_entity_node,
            preview_bounds,
            &mut scratch,
        );
        walk_arch_props(
            project,
            room_id,
            grid,
            floor_index,
            y_offset,
            textures,
            world_camera,
            fog,
            hidden_scene_nodes,
            selected,
            hovered_entity_node,
            preview_bounds,
            &mut scratch,
        );
        walk_water_volumes(
            project,
            room_id,
            grid,
            floor_index,
            y_offset,
            textures,
            world_camera,
            fog,
            hidden_scene_nodes,
            selected,
            preview_tick,
            &mut scratch,
        );
        if show_grid {
            push_streaming_chunk_boundaries(project, room_id, grid, &mut scratch);
        }
        walk_entities(
            project,
            room_id,
            grid,
            floor_index,
            y_offset,
            textures,
            hidden_scene_nodes,
            selected,
            preview_tick,
            &mut scratch,
        );
        if show_lights {
            walk_light_gizmos(
                project,
                room_id,
                grid,
                floor_index,
                y_offset,
                hidden_scene_nodes,
                selected,
                hovered_entity_node,
                &mut scratch,
            );
        }
        if show_portals {
            walk_portal_seams(
                project,
                room_id,
                grid,
                hidden_scene_nodes,
                selected,
                hovered_entity_node,
                &mut scratch,
            );
        }

        // Selection / hover / paint overlays drawn before models --
        // they project through the camera GTE matrix that
        // `setup_gte_for_camera` installed. Models render after,
        // overwriting per-joint GTE state. We re-install the
        // camera state after each room before drawing more editor
        // geometry.
        //
        // Only the active floor draws edit overlays: selection / hover /
        // paint all target the floor being authored, and the overlay
        // helpers project against floor-local face heights, so drawing
        // them on a stacked floor (offset in Y) would detach the outline
        // from its geometry. For non-floored rooms `active` is false on
        // the single base entry, so gate on "active OR no offset".
        let edit_overlays = floor_entry.active || y_offset == 0;
        if edit_overlays {
            if selected_primitives.is_empty() {
                if let Some(selection) =
                    selected_primitive.filter(|selection| selection.room() == room_id)
                {
                    push_selection_outline(grid, selection, OutlineRole::Selected, &mut scratch);
                }
            } else {
                for selection in selected_primitives {
                    if selection.room() == room_id {
                        push_selection_outline(
                            grid,
                            *selection,
                            OutlineRole::Selected,
                            &mut scratch,
                        );
                    }
                }
            }
            for face in selected_sector_faces {
                if face.room == room_id {
                    push_face_outline(grid, *face, FACE_OUTLINE_SELECTED, &mut scratch);
                }
            }
            if let Some(selection) = hovered_primitive {
                if selection.room() == room_id
                    && Some(selection) != selected_primitive
                    && !selected_primitives.contains(&selection)
                {
                    push_selection_outline(grid, selection, OutlineRole::Hover, &mut scratch);
                }
            }
            if room_id == preview_context_node {
                if let Some(preview) = paint_target_preview {
                    push_paint_preview(grid, preview, &mut scratch);
                }
            }
        }

        walk_model_instances(
            project,
            room_id,
            grid,
            floor_index,
            y_offset,
            textures,
            assets,
            selected,
            &world_camera,
            fog,
            hidden_scene_nodes,
            preview_tick,
            character_motion,
            &mut scratch,
        );

        let _ = setup_gte_for_camera(camera);
    }

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
    for selection in validation_issue_primitives {
        for floor_entry in &visible_rooms {
            if selection.room() == floor_entry.room {
                push_selection_outline(
                    floor_entry.grid,
                    *selection,
                    OutlineRole::Error,
                    &mut scratch,
                );
                break;
            }
        }
    }

    // SAFETY: the mutex guard keeps the preview packet arenas alive
    // while the OT is walked. `PreviewScratch` is 16 MB-aligned so the
    // OT's 24-bit packet links reconstruct addresses inside the same
    // host address window.
    let cmd_log = unsafe { psx_gpu_render::build_cmd_log(&scratch.ot) };
    EditorPreviewFrame {
        cmd_log,
        overlay_lines: scratch.overlay_lines.clone(),
    }
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
                    NodeKind::Animator { clip, autoplay, .. } if animator.is_none() => {
                        animator = Some((child.id, *clip, *autoplay));
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
                        clip_override: animator.and_then(|(_, clip, _)| clip),
                        autoplay: animator.is_none_or(|(_, _, autoplay)| autoplay),
                        renderer_node: Some(renderer_node),
                        animator_node: animator.map(|(node_id, _, _)| node_id),
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

fn preview_static_model_reference(
    scene: &Scene,
    node: &SceneNode,
) -> Option<PreviewModelReference> {
    // Match the playtest cooker: a player-controlled Entity's
    // ModelRenderer is consumed by the CharacterController path,
    // not emitted as a second static model at the same transform.
    if matches!(node.kind, NodeKind::Entity) && preview_player_reference(scene, node).is_some() {
        return None;
    }
    preview_model_reference(scene, node)
}

#[derive(Clone, Copy)]
struct PreviewPlayerReference {
    character: Option<ResourceId>,
    model_override: Option<ResourceId>,
    material_override: Option<ResourceId>,
    clip_override: Option<u16>,
    controller_node: Option<NodeId>,
    renderer_node: Option<NodeId>,
    animator_node: Option<NodeId>,
    autoplay: bool,
    visual_offset: [i16; 3],
    visual_yaw_q12: u16,
    visual_scale_q8: u16,
}

fn preview_player_reference(scene: &Scene, node: &SceneNode) -> Option<PreviewPlayerReference> {
    match &node.kind {
        NodeKind::SpawnPoint {
            player: true,
            character,
        } => Some(PreviewPlayerReference {
            character: *character,
            model_override: None,
            material_override: None,
            clip_override: None,
            controller_node: None,
            renderer_node: None,
            animator_node: None,
            autoplay: true,
            visual_offset: [0; 3],
            visual_yaw_q12: 0,
            visual_scale_q8: psxed_project::MODEL_SCALE_ONE_Q8,
        }),
        NodeKind::Entity => {
            let mut controller = None;
            let mut renderer = None;
            let mut animator = None;
            for child in component_children(scene, node) {
                match &child.kind {
                    NodeKind::CharacterController {
                        character,
                        player: true,
                        ..
                    } if controller.is_none() => {
                        controller = Some((child.id, *character));
                    }
                    NodeKind::ModelRenderer {
                        model,
                        material,
                        visual_offset,
                        visual_scale_q8,
                    } if renderer.is_none() => {
                        renderer = Some((
                            child.id,
                            *model,
                            *material,
                            *visual_offset,
                            yaw_to_q12(child.transform.rotation_degrees[1]),
                            *visual_scale_q8,
                        ));
                    }
                    NodeKind::Animator { clip, autoplay, .. } if animator.is_none() => {
                        animator = Some((child.id, *clip, *autoplay));
                    }
                    _ => {}
                }
            }
            controller.map(|(controller_node, character)| {
                let (
                    renderer_node,
                    model_override,
                    material_override,
                    visual_offset,
                    visual_yaw_q12,
                    visual_scale_q8,
                ) = renderer
                    .map(|(node, model, material, offset, yaw, scale)| {
                        (Some(node), model, material, offset, yaw, scale)
                    })
                    .unwrap_or((
                        None,
                        None,
                        None,
                        [0; 3],
                        0,
                        psxed_project::MODEL_SCALE_ONE_Q8,
                    ));
                PreviewPlayerReference {
                    character,
                    model_override,
                    material_override,
                    clip_override: animator.and_then(|(_, clip, _)| clip),
                    controller_node: Some(controller_node),
                    renderer_node,
                    animator_node: animator.map(|(node_id, _, _)| node_id),
                    autoplay: animator.is_none_or(|(_, _, autoplay)| autoplay),
                    visual_offset,
                    visual_yaw_q12,
                    visual_scale_q8,
                }
            })
        }
        _ => None,
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

fn host_renders_as_preview_model(
    project: &ProjectDocument,
    scene: &Scene,
    node: &SceneNode,
) -> bool {
    if let Some(reference) = preview_static_model_reference(scene, node) {
        return project
            .resource(reference.model_id)
            .is_some_and(|resource| matches!(&resource.data, ResourceData::Model(_)));
    }
    if let Some(reference) = preview_player_reference(scene, node) {
        let Some(character_id) = resolve_player_spawn_character(project, reference.character)
        else {
            return false;
        };
        let Some(character_resource) = project.resource(character_id) else {
            return false;
        };
        let ResourceData::Character(character) = &character_resource.data else {
            return false;
        };
        let Some(model_id) = character.model else {
            return false;
        };
        return project
            .resource(model_id)
            .is_some_and(|resource| matches!(&resource.data, ResourceData::Model(_)));
    }
    false
}

/// Walk parent links from `node_id` looking for `room_id`.
/// `true` if `room_id` itself is on the chain. Used to confine
/// per-room lights to the room they were authored under.
/// Which floor a node belongs to, for the editor preview. A placed
/// entity carries its floor on `SceneNode::floor`; child components
/// (ModelRenderer, etc.) inherit it. Walk the ancestor chain up to the
/// room and take the max `floor` seen (children default to 0). Mirrors
/// how the cook binds a node to its floor, so the editor draws each
/// entity once, on the floor it cooks to.
fn node_enclosing_floor(scene: &psxed_project::Scene, node_id: psxed_project::NodeId) -> usize {
    // Single source of truth lives in psxed-project so the render pass
    // (here) and the selection/pick pass (psxed-ui) resolve a node's
    // floor identically.
    psxed_project::floor_view::node_floor(scene, node_id)
}

fn is_descendant_of_room(
    scene: &psxed_project::Scene,
    node_id: psxed_project::NodeId,
    room_id: psxed_project::NodeId,
) -> bool {
    let mut current = Some(node_id);
    while let Some(id) = current {
        if id == room_id {
            return true;
        }
        current = scene.node(id).and_then(|n| n.parent);
    }
    false
}

/// Walk every placeable child node and stamp a small screen-space
/// marker so the user can see where they sit inside the room.
///
/// The room geometry uses the GTE-projected world coords; markers
/// project the same way so they read as "here is this thing in the
/// world", but the corners are drawn at fixed pixel offsets around
/// the projected centre -- a billboarded square that doesn't shrink
/// with distance, the way Godot's editor sprites work.
fn walk_entities(
    project: &ProjectDocument,
    room_id: NodeId,
    grid: &WorldGrid,
    floor_index: usize,
    y_offset: i32,
    textures: &EditorTextures,
    hidden_scene_nodes: &HashSet<NodeId>,
    selected: psxed_project::NodeId,
    preview_tick: u32,
    scratch: &mut PreviewScratch,
) {
    let scene = project.active_scene();
    for node in scene.nodes() {
        if scene_node_hidden(scene, hidden_scene_nodes, node.id)
            || !is_descendant_of_room(scene, node.id, room_id)
        {
            continue;
        }
        // One entity, one floor: draw it only on its own floor entry, so
        // a stacked room doesn't draw every entity once per floor.
        if node_enclosing_floor(scene, node.id) != floor_index {
            continue;
        }
        // Skip nodes that the model-preview pass renders as real
        // textured characters/models. Without this guard they'd get
        // both a marker square and the real model on top of each other.
        if host_renders_as_preview_model(project, scene, node) {
            continue;
        }
        // Anchor the marker to the floor surface, matching how the real
        // model renders (`walk_model_instances` uses the floor-anchored
        // origin) and how the cook places the entity. The raw
        // `translation.y` is a placement default (e.g. the 2.89-sector
        // standing height) and would float the marker far above the
        // floor, disagreeing with the model on top of it. `y_offset`
        // lifts it to its floor's real elevation in the stacked render.
        let mut entity_world = floor_anchored_node_room_local_origin(grid, &node.transform);
        entity_world.y += y_offset;
        if let NodeKind::ParticleEmitter { settings } = &node.kind {
            push_particle_emitter_preview(
                settings,
                entity_world,
                textures.particle_slot(),
                preview_tick,
                scratch,
            );
        }
        let Some(kind_color) = entity_marker_color(&node.kind) else {
            continue;
        };
        let projected = gte_scene::project_vertex(world_to_view([
            entity_world.x,
            entity_world.y,
            entity_world.z,
        ]));
        if projected.sz == 0 {
            continue;
        }

        let is_selected = node.id == selected;
        let half = if is_selected { 9 } else { 6 };
        let (mut r, mut g, mut b) = kind_color;
        if is_selected {
            // Brighten selected markers so they stand out on top of
            // any colour scheme.
            r = r.saturating_add(0x40);
            g = g.saturating_add(0x40);
            b = b.saturating_add(0x40);
        }

        let cx = projected.sx;
        let cy = projected.sy;
        let p_tl = synth(cx - half, cy - half, projected.sz);
        let p_tr = synth(cx + half, cy - half, projected.sz);
        let p_br = synth(cx + half, cy + half, projected.sz);
        let p_bl = synth(cx - half, cy + half, projected.sz);
        push_tri(scratch, [p_tl, p_bl, p_tr], (r, g, b));
        push_tri(scratch, [p_tr, p_bl, p_br], (r, g, b));

        if is_selected {
            // Outline ring for selected entity: four thin tris
            // forming an offset square one pixel beyond the marker.
            let ring = half + 2;
            let outline = (0xFF, 0xFF, 0xFF);
            let r_tl = synth(cx - ring, cy - ring, projected.sz);
            let r_tr = synth(cx + ring, cy - ring, projected.sz);
            let r_br = synth(cx + ring, cy + ring, projected.sz);
            let r_bl = synth(cx - ring, cy + ring, projected.sz);
            push_tri(scratch, [r_tl, p_tl, r_tr], outline);
            push_tri(scratch, [r_tr, p_tl, p_tr], outline);
            push_tri(scratch, [r_tr, p_tr, r_br], outline);
            push_tri(scratch, [r_br, p_tr, p_br], outline);
            push_tri(scratch, [r_br, p_br, r_bl], outline);
            push_tri(scratch, [r_bl, p_br, p_bl], outline);
            push_tri(scratch, [r_bl, p_bl, r_tl], outline);
            push_tri(scratch, [r_tl, p_bl, p_tl], outline);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_image_props(
    project: &ProjectDocument,
    room_id: psxed_project::NodeId,
    grid: &WorldGrid,
    floor_index: usize,
    y_offset: i32,
    textures: &EditorTextures,
    camera: psx_engine::WorldCamera,
    hidden_scene_nodes: &HashSet<NodeId>,
    selected: NodeId,
    hovered: Option<NodeId>,
    preview_bounds: bool,
    scratch: &mut PreviewScratch,
) {
    let scene = project.active_scene();
    let lights = collect_preview_lights(project, room_id, grid, hidden_scene_nodes);
    for node in scene.nodes() {
        if scene_node_hidden(scene, hidden_scene_nodes, node.id)
            || !is_descendant_of_room(scene, node.id, room_id)
        {
            continue;
        }
        if node_enclosing_floor(scene, node.id) != floor_index {
            continue;
        }
        let NodeKind::ImageProp {
            material,
            width,
            height,
            cylindrical_billboard,
            collision_enabled,
            collision_size,
        } = &node.kind
        else {
            continue;
        };
        let Some(material_id) = *material else {
            continue;
        };
        let Some(slot) = textures.slot(material_id) else {
            continue;
        };
        let tint = project
            .resource(material_id)
            .and_then(|resource| match &resource.data {
                ResourceData::Material(material) => Some(material.tint),
                _ => None,
            })
            .unwrap_or([0x80, 0x80, 0x80]);
        let mut origin = node_room_local_origin(grid, &node.transform);
        origin.y += y_offset;
        let verts = image_prop_vertices(
            origin,
            *width,
            *height,
            node.transform.rotation_degrees,
            *cylindrical_billboard,
            camera,
        );
        let Some(projected) = camera.project_world_quad(verts) else {
            continue;
        };
        let p = projected.map(preview_projected_from_engine);
        let u_max = slot.texture_width.saturating_sub(1);
        let v_max = slot.texture_height.saturating_sub(1);
        let uvs = [(0, 0), (u_max, 0), (u_max, v_max), (0, v_max)];
        let lit_tint = preview_lit_image_prop_tint(tint, verts, &lights, grid.ambient_color);
        let material = preview_texture_material(
            slot,
            lit_tint,
            material_blend_mode(project, Some(material_id)),
        );
        let _ = push_textured_material_tri(
            scratch,
            [p[0], p[1], p[2]],
            [uvs[0], uvs[1], uvs[2]],
            material,
            room_depth_slot(projected_avg_sz([p[0], p[1], p[2]])),
        );
        let _ = push_textured_material_tri(
            scratch,
            [p[0], p[2], p[3]],
            [uvs[0], uvs[2], uvs[3]],
            material,
            room_depth_slot(projected_avg_sz([p[0], p[2], p[3]])),
        );
        let is_selected = node.id == selected;
        let is_hovered = hovered == Some(node.id);
        if preview_bounds {
            push_world_quad_wireframe(
                scratch,
                verts,
                entity_bound_style(
                    psxed_ui::EntityBoundKind::ImageProp,
                    is_selected,
                    is_hovered,
                ),
            );
            if *collision_enabled {
                push_image_prop_collision_wireframe(
                    scratch,
                    origin,
                    *height,
                    *collision_size,
                    node.transform.rotation_degrees,
                    *cylindrical_billboard,
                    IMAGE_PROP_COLLISION_BOX,
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
use psxed_project::brush::BRUSH_UV_UNITS_PER_TEXEL;

/// Flat fallback tint for unmaterialed brush faces: neutral greys varied
/// per face so adjacent faces stay distinguishable.
fn brush_fallback_color(face: usize) -> (u8, u8, u8) {
    let shade = 0x6c + ((face as u8) % 3) * 0x14;
    (shade, shade, shade)
}

/// World-space brush solids: each solved face polygon fans into
/// triangles with paraxial world-aligned UVs. Unlit and fog-free in v1
/// (brushes are not room-bound). Solved brush polygons retain their authored
/// outward winding, so normal material sidedness must remain intact: forcing
/// both sides makes the hidden exterior planes of hollow-room slabs occlude
/// an interior editing camera.
fn walk_brushes(
    project: &ProjectDocument,
    textures: &EditorTextures,
    camera: psx_engine::WorldCamera,
    scratch: &mut PreviewScratch,
) {
    use psxed_project::brush::{paraxial_uv, Plane};
    for brush in &project.active_scene().brushes {
        if scratch.geometry_full() {
            break;
        }
        let solved = brush.solve();
        for (face_index, polygon) in solved.polygons.iter().enumerate() {
            let Some(polygon) = polygon else { continue };
            let face = &brush.faces[face_index];
            let Some(plane) = Plane::from_points(face.points) else {
                continue;
            };
            let shade = face_shade(
                project,
                face.material,
                brush_fallback_color(face_index),
                textures,
            );
            let verts = &polygon.verts;
            for i in 1..verts.len().saturating_sub(1) {
                let tri = [verts[0], verts[i], verts[i + 1]];
                let world = tri.map(|v| {
                    psx_engine::WorldVertex::new(
                        v[0].round() as i32,
                        v[1].round() as i32,
                        v[2].round() as i32,
                    )
                });
                let Some(projected) =
                    camera.project_world_quad([world[0], world[1], world[2], world[2]])
                else {
                    continue;
                };
                let p =
                    [projected[0], projected[1], projected[2]].map(preview_projected_from_engine);
                let uvs = match shade {
                    FaceShade::Textured { .. } => tri.map(|v| {
                        let raw = paraxial_uv(&plane, v);
                        let uv = face.uv.apply([
                            raw[0] / BRUSH_UV_UNITS_PER_TEXEL,
                            raw[1] / BRUSH_UV_UNITS_PER_TEXEL,
                        ]);
                        (uv[0].rem_euclid(256.0) as u8, uv[1].rem_euclid(256.0) as u8)
                    }),
                    FaceShade::Flat { .. } => [(0, 0); 3],
                };
                let _ = emit_face_tri(scratch, p, uvs, shade);
            }
        }
    }
}

fn walk_box_props(
    project: &ProjectDocument,
    room_id: psxed_project::NodeId,
    grid: &WorldGrid,
    floor_index: usize,
    y_offset: i32,
    textures: &EditorTextures,
    camera: psx_engine::WorldCamera,
    fog: PreviewFog,
    hidden_scene_nodes: &HashSet<NodeId>,
    selected: NodeId,
    hovered: Option<NodeId>,
    preview_bounds: bool,
    scratch: &mut PreviewScratch,
) {
    let scene = project.active_scene();
    let lights = collect_preview_lights(project, room_id, grid, hidden_scene_nodes);
    for node in scene.nodes() {
        if scene_node_hidden(scene, hidden_scene_nodes, node.id)
            || !is_descendant_of_room(scene, node.id, room_id)
        {
            continue;
        }
        if node_enclosing_floor(scene, node.id) != floor_index {
            continue;
        }
        let NodeKind::BoxProp {
            materials,
            uvs: face_uvs,
            vertices,
            collision_enabled: _,
            break_flags: _,
            erosion,
        } = &node.kind
        else {
            continue;
        };
        let mut origin = node_room_local_origin(grid, &node.transform);
        origin.y += y_offset;
        let world_vertices = box_prop_vertices(origin, *vertices, node.transform.rotation_degrees);
        let generated = psxed_project::generate_box_prop_erosion_quads(*vertices, *erosion);
        let generated_quads: Vec<(usize, [psx_engine::WorldVertex; 4], [[u8; 2]; 4])> = generated
            .iter()
            .map(|quad| {
                (
                    usize::from(quad.source_face),
                    box_prop_world_quad(origin, quad.vertices, node.transform.rotation_degrees),
                    quad.uv_q8,
                )
            })
            .collect();
        let face_count = if generated_quads.is_empty() {
            psxed_project::BOX_PROP_FACE_COUNT
        } else {
            generated_quads.len()
        };
        for rendered_face in 0..face_count {
            let (face, face_vertices, generated_uvs) = if generated_quads.is_empty() {
                (
                    rendered_face,
                    box_prop_face_vertices(world_vertices, rendered_face),
                    None,
                )
            } else {
                let (face, vertices, uv_q8) = generated_quads[rendered_face];
                (face, vertices, Some(uv_q8))
            };
            let Some(material_id) = materials[face] else {
                continue;
            };
            let Some(projected) = camera.project_world_quad(face_vertices) else {
                continue;
            };
            let p = projected.map(preview_projected_from_engine);
            let shade = face_shade(
                project,
                Some(material_id),
                box_prop_fallback_color(face),
                textures,
            )
            // Box props are standalone closed primitives. The
            // runtime renders them uncullled, and the editor
            // preview should match that rather than inheriting
            // one-sided room-face material semantics.
            .with_sidedness(psxed_project::MaterialFaceSidedness::Both);
            let face_center = average_world_quad(face_vertices);
            let depth = face_depth(camera, face_center);
            let shade = fog.apply_shade(
                light_face(shade, face_center, &lights, grid.ambient_color),
                depth,
            );
            let uvs = match shade {
                FaceShade::Textured { slot, .. } => {
                    let u_max = slot.texture_width.saturating_sub(1);
                    let v_max = slot.texture_height.saturating_sub(1);
                    let corners = face_uvs[face].apply_to_quad([
                        (0, 0),
                        (u_max, 0),
                        (u_max, v_max),
                        (0, v_max),
                    ]);
                    generated_uvs
                        .map(|uvs| uvs.map(|uv| box_prop_face_uv_at(corners, uv)))
                        .unwrap_or(corners)
                }
                FaceShade::Flat { .. } => PREVIEW_FLOOR_UVS,
            };
            emit_box_prop_face(scratch, p, uvs, shade);
        }

        let is_selected = node.id == selected;
        let is_hovered = hovered == Some(node.id);
        if preview_bounds || is_selected || is_hovered {
            push_box_prop_wireframe(
                scratch,
                world_vertices,
                entity_bound_style(psxed_ui::EntityBoundKind::BoxProp, is_selected, is_hovered),
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_cylinder_props(
    project: &ProjectDocument,
    room_id: psxed_project::NodeId,
    grid: &WorldGrid,
    floor_index: usize,
    y_offset: i32,
    textures: &EditorTextures,
    camera: psx_engine::WorldCamera,
    fog: PreviewFog,
    hidden_scene_nodes: &HashSet<NodeId>,
    selected: NodeId,
    hovered: Option<NodeId>,
    preview_bounds: bool,
    scratch: &mut PreviewScratch,
) {
    let scene = project.active_scene();
    let lights = collect_preview_lights(project, room_id, grid, hidden_scene_nodes);
    for node in scene.nodes() {
        if scene_node_hidden(scene, hidden_scene_nodes, node.id)
            || !is_descendant_of_room(scene, node.id, room_id)
            || node_enclosing_floor(scene, node.id) != floor_index
        {
            continue;
        }
        let NodeKind::CylinderProp {
            materials,
            uvs,
            geometry,
            collision_enabled: _,
        } = &node.kind
        else {
            continue;
        };
        let mut origin = node_room_local_origin(grid, &node.transform);
        origin.y += y_offset;
        let generated = psxed_project::generate_cylinder_prop_surfaces(*geometry);
        let is_selected = node.id == selected;
        let is_hovered = hovered == Some(node.id);
        let outline = entity_bound_style(
            psxed_ui::EntityBoundKind::CylinderProp,
            is_selected,
            is_hovered,
        );
        for surface in generated {
            let slot_index = usize::from(surface.material_slot)
                .min(psxed_project::CYLINDER_PROP_MATERIAL_COUNT - 1);
            let world_vertices =
                box_prop_world_quad(origin, surface.vertices, node.transform.rotation_degrees);
            let Some(projected) = camera.project_world_quad(world_vertices) else {
                continue;
            };
            let p = projected.map(preview_projected_from_engine);
            if let Some(material_id) = materials[slot_index] {
                let shade = face_shade(
                    project,
                    Some(material_id),
                    cylinder_prop_fallback_color(slot_index),
                    textures,
                )
                .with_sidedness(psxed_project::MaterialFaceSidedness::Both);
                let center = average_world_quad(world_vertices);
                let depth = face_depth(camera, center);
                let shade = fog.apply_shade(
                    light_face(shade, center, &lights, grid.ambient_color),
                    depth,
                );
                let surface_uvs = match shade {
                    FaceShade::Textured { slot, .. } => {
                        let u_max = slot.texture_width.saturating_sub(1);
                        let v_max = slot.texture_height.saturating_sub(1);
                        let corners = uvs[slot_index].apply_to_quad([
                            (0, 0),
                            (u_max, 0),
                            (u_max, v_max),
                            (0, v_max),
                        ]);
                        surface.uv_q8.map(|uv| box_prop_face_uv_at(corners, uv))
                    }
                    FaceShade::Flat { .. } => PREVIEW_FLOOR_UVS,
                };
                if surface.vertex_count == 3 {
                    let _ = emit_face_tri(
                        scratch,
                        [p[0], p[1], p[2]],
                        [surface_uvs[0], surface_uvs[1], surface_uvs[2]],
                        shade,
                    );
                } else {
                    emit_box_prop_face(scratch, p, surface_uvs, shade);
                }
            }
            if preview_bounds || is_selected || is_hovered {
                let count = usize::from(surface.vertex_count.clamp(3, 4));
                for index in 0..count {
                    let next = (index + 1) % count;
                    if p[index].sz != 0 && p[next].sz != 0 {
                        push_screen_line(scratch, p[index], p[next], outline);
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_arch_props(
    project: &ProjectDocument,
    room_id: psxed_project::NodeId,
    grid: &WorldGrid,
    floor_index: usize,
    y_offset: i32,
    textures: &EditorTextures,
    camera: psx_engine::WorldCamera,
    fog: PreviewFog,
    hidden_scene_nodes: &HashSet<NodeId>,
    selected: NodeId,
    hovered: Option<NodeId>,
    preview_bounds: bool,
    scratch: &mut PreviewScratch,
) {
    let scene = project.active_scene();
    let lights = collect_preview_lights(project, room_id, grid, hidden_scene_nodes);
    for node in scene.nodes() {
        if scene_node_hidden(scene, hidden_scene_nodes, node.id)
            || !is_descendant_of_room(scene, node.id, room_id)
            || node_enclosing_floor(scene, node.id) != floor_index
        {
            continue;
        }
        let NodeKind::ArchProp {
            materials,
            uvs,
            geometry,
            collision_enabled: _,
        } = &node.kind
        else {
            continue;
        };
        let mut origin = node_room_local_origin(grid, &node.transform);
        origin.y += y_offset;
        let generated =
            psxed_project::generate_arch_prop_surfaces(*geometry, grid.sector_size.max(1));
        let is_selected = node.id == selected;
        let is_hovered = hovered == Some(node.id);
        let outline =
            entity_bound_style(psxed_ui::EntityBoundKind::ArchProp, is_selected, is_hovered);
        for surface in generated {
            let slot_index =
                usize::from(surface.material_slot).min(psxed_project::ARCH_PROP_MATERIAL_COUNT - 1);
            let world_vertices =
                box_prop_world_quad(origin, surface.vertices, node.transform.rotation_degrees);
            let Some(projected) = camera.project_world_quad(world_vertices) else {
                continue;
            };
            let p = projected.map(preview_projected_from_engine);
            if let Some(material_id) = materials[slot_index] {
                let shade = face_shade(
                    project,
                    Some(material_id),
                    arch_prop_fallback_color(slot_index),
                    textures,
                )
                .with_sidedness(psxed_project::MaterialFaceSidedness::Both);
                let center = average_world_quad(world_vertices);
                let depth = face_depth(camera, center);
                let shade = fog.apply_shade(
                    light_face(shade, center, &lights, grid.ambient_color),
                    depth,
                );
                let surface_uvs = match shade {
                    FaceShade::Textured { slot, .. } => {
                        let u_max = slot.texture_width.saturating_sub(1);
                        let v_max = slot.texture_height.saturating_sub(1);
                        let corners = uvs[slot_index].apply_to_quad([
                            (0, 0),
                            (u_max, 0),
                            (u_max, v_max),
                            (0, v_max),
                        ]);
                        surface.uv_q8.map(|uv| box_prop_face_uv_at(corners, uv))
                    }
                    FaceShade::Flat { .. } => PREVIEW_FLOOR_UVS,
                };
                emit_box_prop_face(scratch, p, surface_uvs, shade);
            }
            if preview_bounds || is_selected || is_hovered {
                for index in 0..4 {
                    let next = (index + 1) % 4;
                    if p[index].sz != 0 && p[next].sz != 0 {
                        push_screen_line(scratch, p[index], p[next], outline);
                    }
                }
            }
        }
    }
}

fn box_prop_face_uv_at(corners: [(u8, u8); 4], uv_q8: [u8; 2]) -> (u8, u8) {
    let u = u32::from(uv_q8[0]);
    let v = u32::from(uv_q8[1]);
    let inv_u = 255 - u;
    let inv_v = 255 - v;
    let interpolate = |axis: usize| {
        let values = if axis == 0 {
            [
                u32::from(corners[0].0),
                u32::from(corners[1].0),
                u32::from(corners[2].0),
                u32::from(corners[3].0),
            ]
        } else {
            [
                u32::from(corners[0].1),
                u32::from(corners[1].1),
                u32::from(corners[2].1),
                u32::from(corners[3].1),
            ]
        };
        let top = values[0] * inv_u + values[1] * u;
        let bottom = values[3] * inv_u + values[2] * u;
        ((top * inv_v + bottom * v + 32_512) / 65_025).min(255) as u8
    };
    (interpolate(0), interpolate(1))
}

#[allow(clippy::too_many_arguments)]
fn walk_water_volumes(
    project: &ProjectDocument,
    room_id: NodeId,
    grid: &WorldGrid,
    floor_index: usize,
    y_offset: i32,
    textures: &EditorTextures,
    camera: psx_engine::WorldCamera,
    fog: PreviewFog,
    hidden_scene_nodes: &HashSet<NodeId>,
    selected: NodeId,
    preview_tick: u32,
    scratch: &mut PreviewScratch,
) {
    let scene = project.active_scene();
    let lights = collect_preview_lights(project, room_id, grid, hidden_scene_nodes);
    for node in scene.nodes() {
        if scene_node_hidden(scene, hidden_scene_nodes, node.id)
            || !is_descendant_of_room(scene, node.id, room_id)
            || node_enclosing_floor(scene, node.id) != floor_index
        {
            continue;
        }
        let NodeKind::WaterVolume {
            material,
            cells,
            settings,
        } = &node.kind
        else {
            continue;
        };
        let Some(material_id) = *material else {
            continue;
        };
        let sector_size = grid.sector_size;
        for cell in cells {
            let Some((array_x, array_z)) = grid.world_cell_to_array(cell.x, cell.z) else {
                continue;
            };
            let x0 = cell.x.saturating_mul(sector_size);
            let z0 = cell.z.saturating_mul(sector_size);
            let x1 = x0.saturating_add(sector_size);
            let z1 = z0.saturating_add(sector_size);
            let floor = grid
                .sector(array_x, array_z)
                .and_then(|sector| sector.floor.as_ref());
            let floor_y = floor.map(|floor| floor.lowest_height()).unwrap_or(0);
            let y = floor_y
                .saturating_add(i32::from(settings.height_above_floor))
                .saturating_add(y_offset);
            // Match the top face winding used by BoxProp so editor and
            // generated runtime surface packets share culling/orientation.
            let verts = [
                psx_engine::WorldVertex::new(x0, y, z1),
                psx_engine::WorldVertex::new(x1, y, z1),
                psx_engine::WorldVertex::new(x1, y, z0),
                psx_engine::WorldVertex::new(x0, y, z0),
            ];
            let Some(projected) = camera.project_world_quad(verts) else {
                continue;
            };
            let p = projected.map(preview_projected_from_engine);
            let center = average_world_quad(verts);
            let depth = face_depth(camera, center);
            let shade = face_shade(project, Some(material_id), (52, 92, 112), textures)
                .with_sidedness(psxed_project::MaterialFaceSidedness::Both);
            let shade = fog.apply_shade(
                light_face(shade, center, &lights, grid.ambient_color),
                depth,
            );
            let uvs = match shade {
                FaceShade::Textured { slot, .. } => {
                    animated_material_quad_uvs(project, material_id, slot, preview_tick)
                }
                FaceShade::Flat { .. } => PREVIEW_FLOOR_UVS,
            };
            emit_box_prop_face(scratch, p, uvs, shade);
            if node.id == selected {
                let style = if settings.height_above_floor >= settings.lethal_depth {
                    FACE_OUTLINE_ERROR
                } else {
                    FACE_OUTLINE_SELECTED
                };
                push_world_quad_wireframe(scratch, verts, style);
                if let Some(floor) = floor {
                    let bottom = [
                        psx_engine::WorldVertex::new(
                            x0,
                            floor.heights[0].saturating_add(y_offset),
                            z1,
                        ),
                        psx_engine::WorldVertex::new(
                            x1,
                            floor.heights[1].saturating_add(y_offset),
                            z1,
                        ),
                        psx_engine::WorldVertex::new(
                            x1,
                            floor.heights[2].saturating_add(y_offset),
                            z0,
                        ),
                        psx_engine::WorldVertex::new(
                            x0,
                            floor.heights[3].saturating_add(y_offset),
                            z0,
                        ),
                    ];
                    push_world_quad_wireframe(scratch, bottom, style);
                    for index in 0..4 {
                        let projected_top = gte_scene::project_vertex(world_to_view([
                            verts[index].x,
                            verts[index].y,
                            verts[index].z,
                        ]));
                        let projected_bottom = gte_scene::project_vertex(world_to_view([
                            bottom[index].x,
                            bottom[index].y,
                            bottom[index].z,
                        ]));
                        if projected_top.sz != 0 && projected_bottom.sz != 0 {
                            push_screen_line(scratch, projected_top, projected_bottom, style);
                        }
                    }
                }
            }
        }
    }
}

fn emit_box_prop_face(
    scratch: &mut PreviewScratch,
    p: [psx_gte::scene::Projected; 4],
    uvs: [(u8, u8); 4],
    shade: FaceShade,
) {
    let _ = emit_face_tri(scratch, [p[0], p[1], p[2]], [uvs[0], uvs[1], uvs[2]], shade);
    let _ = emit_face_tri(scratch, [p[0], p[2], p[3]], [uvs[0], uvs[2], uvs[3]], shade);
}

fn box_prop_vertices(
    origin: psx_engine::WorldVertex,
    vertices: [[i16; 3]; psxed_project::BOX_PROP_VERTEX_COUNT],
    rotation_degrees: [f32; 3],
) -> [psx_engine::WorldVertex; psxed_project::BOX_PROP_VERTEX_COUNT] {
    let rotation = [
        yaw_to_q12(rotation_degrees[0]),
        yaw_to_q12(rotation_degrees[1]),
        yaw_to_q12(rotation_degrees[2]),
    ];
    let mut out = [psx_engine::WorldVertex::new(0, 0, 0); psxed_project::BOX_PROP_VERTEX_COUNT];
    for (i, local) in vertices.iter().enumerate() {
        let rotated = rotate_image_prop_local(
            [local[0] as i32, local[1] as i32, local[2] as i32],
            rotation,
        );
        out[i] = psx_engine::WorldVertex::new(
            origin.x.saturating_add(rotated[0]),
            origin.y.saturating_add(rotated[1]),
            origin.z.saturating_add(rotated[2]),
        );
    }
    out
}

fn box_prop_world_quad(
    origin: psx_engine::WorldVertex,
    vertices: [[i16; 3]; 4],
    rotation_degrees: [f32; 3],
) -> [psx_engine::WorldVertex; 4] {
    let rotation = [
        yaw_to_q12(rotation_degrees[0]),
        yaw_to_q12(rotation_degrees[1]),
        yaw_to_q12(rotation_degrees[2]),
    ];
    vertices.map(|local| {
        let rotated = rotate_image_prop_local(
            [
                i32::from(local[0]),
                i32::from(local[1]),
                i32::from(local[2]),
            ],
            rotation,
        );
        psx_engine::WorldVertex::new(
            origin.x.saturating_add(rotated[0]),
            origin.y.saturating_add(rotated[1]),
            origin.z.saturating_add(rotated[2]),
        )
    })
}

fn box_prop_face_vertices(
    vertices: [psx_engine::WorldVertex; psxed_project::BOX_PROP_VERTEX_COUNT],
    face: usize,
) -> [psx_engine::WorldVertex; 4] {
    let indices = BOX_PROP_FACE_VERTEX_INDICES[face];
    [
        vertices[indices[0]],
        vertices[indices[1]],
        vertices[indices[2]],
        vertices[indices[3]],
    ]
}

fn average_world_quad(verts: [psx_engine::WorldVertex; 4]) -> [i32; 3] {
    [
        average_i32_4(verts[0].x, verts[1].x, verts[2].x, verts[3].x),
        average_i32_4(verts[0].y, verts[1].y, verts[2].y, verts[3].y),
        average_i32_4(verts[0].z, verts[1].z, verts[2].z, verts[3].z),
    ]
}

fn box_prop_fallback_color(face: usize) -> (u8, u8, u8) {
    const COLORS: [(u8, u8, u8); psxed_project::BOX_PROP_FACE_COUNT] = [
        (0x87, 0xB4, 0xDC),
        (0x78, 0xC0, 0xA0),
        (0xD0, 0xAA, 0x78),
        (0xB0, 0x98, 0xD0),
        (0xC0, 0xC8, 0xD0),
        (0x90, 0x98, 0xA0),
    ];
    COLORS[face]
}

fn cylinder_prop_fallback_color(slot: usize) -> (u8, u8, u8) {
    const COLORS: [(u8, u8, u8); psxed_project::CYLINDER_PROP_MATERIAL_COUNT] = [
        (0x88, 0xA8, 0xB8),
        (0xB8, 0xC0, 0xC8),
        (0x70, 0x78, 0x80),
        (0x90, 0x78, 0x68),
    ];
    COLORS[slot]
}

fn arch_prop_fallback_color(slot: usize) -> (u8, u8, u8) {
    const COLORS: [(u8, u8, u8); psxed_project::ARCH_PROP_MATERIAL_COUNT] = [
        (0xB0, 0x98, 0x78),
        (0x88, 0x78, 0x68),
        (0xC0, 0xAC, 0x88),
        (0x78, 0x68, 0x58),
    ];
    COLORS[slot]
}

fn push_box_prop_wireframe(
    scratch: &mut PreviewScratch,
    vertices: [psx_engine::WorldVertex; psxed_project::BOX_PROP_VERTEX_COUNT],
    style: FaceOutlineStyle,
) {
    let projected = vertices.map(|v| gte_scene::project_vertex(world_to_view([v.x, v.y, v.z])));
    for (a, b) in BOX_PROP_EDGE_VERTEX_INDICES {
        if projected[a].sz == 0 || projected[b].sz == 0 {
            continue;
        }
        push_screen_line(scratch, projected[a], projected[b], style);
    }
}

fn preview_lit_image_prop_tint(
    tint: [u8; 3],
    verts: [psx_engine::WorldVertex; 4],
    lights: &[psx_engine::PointLightSample],
    ambient: [u8; 3],
) -> (u8, u8, u8) {
    let center = [
        average_i32_4(verts[0].x, verts[1].x, verts[2].x, verts[3].x),
        average_i32_4(verts[0].y, verts[1].y, verts[2].y, verts[3].y),
        average_i32_4(verts[0].z, verts[1].z, verts[2].z, verts[3].z),
    ];
    psx_engine::shade_material_tint_with_lights(
        psx_engine::MaterialTint::from_tuple((tint[0], tint[1], tint[2])),
        center,
        psx_engine::Rgb8::from_array(ambient),
        lights.iter().copied(),
    )
    .to_tuple()
}

fn average_i32_4(a: i32, b: i32, c: i32, d: i32) -> i32 {
    ((a as i64 + b as i64 + c as i64 + d as i64) / 4).clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

fn image_prop_vertices(
    origin: psx_engine::WorldVertex,
    width: u16,
    height: u16,
    rotation_degrees: [f32; 3],
    cylindrical_billboard: bool,
    camera: psx_engine::WorldCamera,
) -> [psx_engine::WorldVertex; 4] {
    // Cylindrical billboarding overrides any authored rotation: the
    // card always faces the camera and stays upright, so pitch / roll
    // are ignored.
    if cylindrical_billboard {
        let sin_yaw = camera.sin_yaw.raw();
        let cos_yaw = camera.cos_yaw.raw();
        let half_width = (width as i32) / 2;
        let right_x = mul_q12_i32(half_width, cos_yaw);
        let right_z = -mul_q12_i32(half_width, sin_yaw);
        let top_y = origin.y.saturating_add(height as i32);
        return [
            psx_engine::WorldVertex::new(origin.x - right_x, top_y, origin.z - right_z),
            psx_engine::WorldVertex::new(origin.x + right_x, top_y, origin.z + right_z),
            psx_engine::WorldVertex::new(origin.x + right_x, origin.y, origin.z + right_z),
            psx_engine::WorldVertex::new(origin.x - right_x, origin.y, origin.z - right_z),
        ];
    }
    let half_width = (width as i32) / 2;
    let h = height as i32;
    // Local quad anchored at bottom-center, lying in the X-Y plane,
    // facing +Z before rotation. Top edge runs from (-w/2, h, 0) to
    // (+w/2, h, 0); bottom edge runs along Y = 0.
    let locals: [[i32; 3]; 4] = [
        [-half_width, h, 0],
        [half_width, h, 0],
        [half_width, 0, 0],
        [-half_width, 0, 0],
    ];
    let pitch_q12 = yaw_to_q12(rotation_degrees[0]);
    let yaw_q12 = yaw_to_q12(rotation_degrees[1]);
    let roll_q12 = yaw_to_q12(rotation_degrees[2]);
    let mut out = [psx_engine::WorldVertex::new(0, 0, 0); 4];
    for (i, local) in locals.iter().enumerate() {
        let rotated = rotate_image_prop_local(*local, [pitch_q12, yaw_q12, roll_q12]);
        out[i] = psx_engine::WorldVertex::new(
            origin.x.saturating_add(rotated[0]),
            origin.y.saturating_add(rotated[1]),
            origin.z.saturating_add(rotated[2]),
        );
    }
    out
}

fn rotate_image_prop_local(v: [i32; 3], rotation_q12: [u16; 3]) -> [i32; 3] {
    // X (pitch) -> Y (yaw) -> Z (roll), shared with the cooker so the editor
    // card, selection outline, cooked record, and runtime draw path agree.
    psxed_project::spatial::rotate_euler_local_q12(
        v,
        rotation_q12[0],
        rotation_q12[1],
        rotation_q12[2],
    )
}

fn push_world_quad_wireframe(
    scratch: &mut PreviewScratch,
    verts: [psx_engine::WorldVertex; 4],
    style: FaceOutlineStyle,
) {
    let projected = verts.map(|v| gte_scene::project_vertex(world_to_view([v.x, v.y, v.z])));
    if projected.iter().any(|p| p.sz == 0) {
        return;
    }
    for (a, b) in [(0, 1), (1, 2), (2, 3), (3, 0)] {
        push_screen_line(scratch, projected[a], projected[b], style);
    }
}

fn push_image_prop_collision_wireframe(
    scratch: &mut PreviewScratch,
    origin: psx_engine::WorldVertex,
    visual_height: u16,
    collision_size: [u16; 3],
    rotation_degrees: [f32; 3],
    cylindrical_billboard: bool,
    style: FaceOutlineStyle,
) {
    let half = [
        ((collision_size[0] as i32) / 2).max(1),
        ((collision_size[1] as i32) / 2).max(1),
        ((collision_size[2] as i32) / 2).max(1),
    ];
    let center_y = (visual_height as i32) / 2;
    if cylindrical_billboard {
        let center = [
            origin.x as f32,
            (origin.y + center_y) as f32,
            origin.z as f32,
        ];
        push_aabb_wireframe(
            scratch,
            center,
            [half[0] as f32, half[1] as f32, half[2] as f32],
            style,
        );
        return;
    }

    let rotation = [
        yaw_to_q12(rotation_degrees[0]),
        yaw_to_q12(rotation_degrees[1]),
        yaw_to_q12(rotation_degrees[2]),
    ];
    let mut verts = [[0, 0, 0]; 8];
    let mut i = 0usize;
    for z in [-half[2], half[2]] {
        for y in [center_y - half[1], center_y + half[1]] {
            for x in [-half[0], half[0]] {
                let rotated = rotate_image_prop_local([x, y, z], rotation);
                verts[i] = [
                    origin.x.saturating_add(rotated[0]),
                    origin.y.saturating_add(rotated[1]),
                    origin.z.saturating_add(rotated[2]),
                ];
                i += 1;
            }
        }
    }
    push_world_box_wireframe(scratch, verts, style);
}

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
    /// Render origin (room-local engine units). Model placement
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
    /// animation phase. Disabled instances hold frame zero.
    autoplay: bool,
}

/// Render every Model-backed legacy `MeshInstance` or component
/// `Entity` in the scene as a real textured animated model.
/// Mirrors the runtime path in `editor-playtest`: parse the
/// `.psxmdl` + `.psxt` + `.psxanim` set, upload atlas (lazily, done by
/// `EditorTextures::refresh_models`), then submit the model through
/// `psx-engine`'s canonical model render pass.
///
/// Models with bad/missing data are skipped silently -- the
/// editor inspector + cook validation surface those errors
/// elsewhere; the preview just keeps drawing what it can.
#[allow(clippy::too_many_arguments)]
fn walk_model_instances(
    project: &ProjectDocument,
    room_id: psxed_project::NodeId,
    grid: &WorldGrid,
    floor_index: usize,
    y_offset: i32,
    textures: &EditorTextures,
    assets: &crate::editor_assets::EditorAssets,
    selected: psxed_project::NodeId,
    camera: &psx_engine::WorldCamera,
    fog: PreviewFog,
    hidden_scene_nodes: &HashSet<NodeId>,
    tick: u32,
    character_motion: Option<psxed_ui::EditorCharacterMotionPreview>,
    scratch: &mut PreviewScratch,
) {
    // The persistent `EditorAssets` cache owns mesh + animation
    // bytes. We only borrow into it here; nothing in this loop
    // touches the filesystem. Per-instance state (which clip is
    // active, where it lives in the world) lives in
    // `instances_meta`.
    let scene = project.active_scene();
    let lights = collect_preview_lights(project, room_id, grid, hidden_scene_nodes);
    let ambient = grid.ambient_color;
    let mut instances_meta: Vec<InstanceMeta> = Vec::new();

    for node in scene.nodes() {
        if instances_meta.len() >= MAX_PREVIEW_MODEL_INSTANCES {
            break;
        }
        if !is_descendant_of_room(scene, node.id, room_id) {
            continue;
        }
        if node_enclosing_floor(scene, node.id) != floor_index {
            continue;
        }
        let Some(reference) = preview_static_model_reference(scene, node) else {
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
        // Atlas required -- runtime contract.
        if model.texture_path.is_none() {
            continue;
        }
        // Atlas slot must already be uploaded (refresh_models
        // ran earlier in the frame). Skip if not -- lets the
        // user know visually that the atlas is broken.
        let Some(atlas_slot) = textures.model_atlas_slot(reference.model_id) else {
            continue;
        };

        // Geometry-only models: preview the instance's clip override,
        // else the first skeleton-scoped clip.
        let preview_transform = character_motion.filter(|preview| preview.entity == node.id);
        let clip_local = preview_transform
            .map(|preview| preview.clip)
            .or(reference.clip_override)
            .unwrap_or(0);
        if (clip_local as usize)
            >= project
                .resolved_model_animation_clips(reference.model_id)
                .len()
        {
            continue;
        }

        // Model placements are floor anchors: X/Z follow the
        // authored node, Y is sampled from the floor under it, then
        // lifted to this floor's real elevation in the stacked render.
        let mut origin = floor_anchored_node_room_local_origin(grid, &node.transform);
        origin.y += y_offset;

        let yaw_q12 = apply_character_motion_preview(
            node.id,
            &mut origin,
            yaw_to_q12(node.transform.rotation_degrees[1]),
            character_motion,
        );
        // Match the cooked record: entity pitch/roll plus the combined
        // entity + renderer yaw, composed exactly as the runtime does.
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
            yaw_q12,
            collision_radius: model.collision_radius as i32,
            world_height: model.world_height as i32,
            visual_offset: reference.visual_offset,
            visual_scale_q8: reference.visual_scale_q8,
        });
    }

    // Player-spawn preview: render the player's character at
    // the spawn so level designers see where the player starts
    // *and* what they look like. Reuses the same model render
    // path -- no separate player renderer.
    walk_player_spawn_preview(
        project,
        room_id,
        grid,
        floor_index,
        y_offset,
        textures,
        hidden_scene_nodes,
        selected,
        character_motion,
        &mut instances_meta,
    );

    // Resolve parsed model + animation per instance straight
    // out of the cache. Each meta carries its own
    // `(mesh_id, clip_local)` pair so two instances of the
    // same model with different clips resolve to two different
    // animation entries -- fixes the prior shared-buffer bug
    // where whichever clip got loaded first won.
    let mut instances: Vec<PreviewModelInstance> = Vec::new();
    for meta in &instances_meta {
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
            meta.world_height,
            meta.visual_offset,
            meta.visual_scale_q8,
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
            tint: shade_model_tint(origin, *camera, fog, &lights, ambient, base_tint),
            blend_mode: material_override
                .map(|material| material.blend_mode)
                .unwrap_or(BlendMode::Opaque),
            secondary_layer: material_override.and_then(|material| {
                material.secondary_layer.map(|mut layer| {
                    layer.tint =
                        shade_model_tint(origin, *camera, fog, &lights, ambient, layer.tint);
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
        });
    }

    let shadow_slot = textures.shadow_slot();
    for meta in &instances_meta {
        draw_model_shadow(meta, shadow_slot, *camera, scratch);
    }

    // Gizmos first while GTE still holds the camera matrix --
    // the engine model pass overrides rotation/translation
    // per joint so any project_vertex after a model render uses
    // joint-space, not world-space.
    for meta in &instances_meta {
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
fn walk_player_spawn_preview(
    project: &ProjectDocument,
    room_id: psxed_project::NodeId,
    grid: &WorldGrid,
    floor_index: usize,
    y_offset: i32,
    textures: &EditorTextures,
    hidden_scene_nodes: &HashSet<NodeId>,
    selected: psxed_project::NodeId,
    character_motion: Option<psxed_ui::EditorCharacterMotionPreview>,
    instances_meta: &mut Vec<InstanceMeta>,
) {
    let scene = project.active_scene();
    for node in scene.nodes() {
        if instances_meta.len() >= MAX_PREVIEW_MODEL_INSTANCES {
            break;
        }
        if !is_descendant_of_room(scene, node.id, room_id) {
            continue;
        }
        // Player model belongs to one floor like any other entity, draw
        // it only on its own floor entry (this path is separate from
        // `walk_model_instances`, which skips player-controlled entities).
        if node_enclosing_floor(scene, node.id) != floor_index {
            continue;
        }
        let Some(reference) = preview_player_reference(scene, node) else {
            continue;
        };
        if preview_reference_hidden(
            scene,
            hidden_scene_nodes,
            node.id,
            reference.controller_node,
            reference.renderer_node,
            reference.animator_node,
        ) {
            continue;
        }
        let Some(character_id) = resolve_player_spawn_character(project, reference.character)
        else {
            continue;
        };
        let Some(character_resource) = project.resource(character_id) else {
            continue;
        };
        let ResourceData::Character(char_resource) = &character_resource.data else {
            continue;
        };
        let Some(model_id) = reference.model_override.or(char_resource.model) else {
            continue;
        };
        let Some(model_resource) = project.resource(model_id) else {
            continue;
        };
        let ResourceData::Model(model) = &model_resource.data else {
            continue;
        };
        if model.texture_path.is_none() {
            continue;
        }
        let Some(atlas_slot) = textures.model_atlas_slot(model_id) else {
            continue;
        };

        // Animator clip drives the editor viewport when authored.
        // Otherwise fall back to the player's idle action, then the
        // model preview/default clip so partial characters still draw.
        let preview_transform = character_motion.filter(|preview| preview.entity == node.id);
        let clip_local = preview_transform
            .map(|preview| preview.clip)
            .or(reference.clip_override)
            .or_else(|| {
                psxed_project::resolve::resolve_character_idle_preview_clip_for_model(
                    project,
                    char_resource,
                    model_id,
                    model,
                )
            })
            .unwrap_or(0);
        if (clip_local as usize) >= project.resolved_model_animation_clips(model_id).len() {
            continue;
        }

        let mut origin = floor_anchored_node_room_local_origin(grid, &node.transform);
        origin.y += y_offset;
        let yaw_q12 = apply_character_motion_preview(
            node.id,
            &mut origin,
            yaw_to_q12(node.transform.rotation_degrees[1]),
            character_motion,
        );
        let model_rotation = yaw_rotation_q12(yaw_q12.wrapping_add(reference.visual_yaw_q12));

        instances_meta.push(InstanceMeta {
            mesh_id: model_id,
            clip_local,
            origin,
            model_rotation,
            atlas: atlas_slot,
            material_override: reference.material_override,
            // Host/controller node is selected, not the model --
            // but the preview gizmo still helps designers see
            // which spawn/controller they have selected.
            is_selected: preview_reference_selected(
                selected,
                node.id,
                reference.controller_node,
                reference.renderer_node,
                reference.animator_node,
            ),
            autoplay: preview_transform.is_some() || reference.autoplay,
            yaw_q12,
            collision_radius: model.collision_radius as i32,
            world_height: model.world_height as i32,
            visual_offset: reference.visual_offset,
            visual_scale_q8: reference.visual_scale_q8,
        });
    }
}

/// Resolve a Player Spawn's character reference, applying the
/// "auto-pick the only one" rule when no explicit character is
/// set. `None` means the editor preview can't render a player
/// model -- typically because the project has zero or multiple
/// Characters and the spawn is mid-author.
fn resolve_player_spawn_character(
    project: &ProjectDocument,
    explicit: Option<ResourceId>,
) -> Option<ResourceId> {
    psxed_project::resolve::resolve_spawn_character(project, explicit)
        .ok()
        .map(|resolved| resolved.id)
}

fn node_room_local_origin(
    grid: &WorldGrid,
    transform: &psxed_project::Transform3,
) -> psx_engine::WorldVertex {
    let [x, y, z] = spatial::node_preview_origin(grid, transform);
    psx_engine::WorldVertex::new(x, y, z)
}

fn floor_anchored_node_room_local_origin(
    grid: &WorldGrid,
    transform: &psxed_project::Transform3,
) -> psx_engine::WorldVertex {
    let [x, y, z] = spatial::floor_anchored_node_preview_origin(grid, transform);
    psx_engine::WorldVertex::new(x, y, z)
}

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

fn floor_anchored_model_origin(
    origin: psx_engine::WorldVertex,
    world_height: i32,
) -> psx_engine::WorldVertex {
    psx_engine::WorldVertex::new(
        origin.x,
        origin
            .y
            .saturating_add(model_origin_floor_lift(world_height)),
        origin.z,
    )
}

fn visual_model_origin(
    origin: psx_engine::WorldVertex,
    world_height: i32,
    visual_offset: [i16; 3],
    _visual_scale_q8: u16,
    instance_rotation: Mat3I16,
) -> psx_engine::WorldVertex {
    let origin = floor_anchored_model_origin(origin, world_height);
    let offset = rotate_visual_offset(instance_rotation, visual_offset);
    psx_engine::WorldVertex::new(
        origin.x.saturating_add(offset[0]),
        origin.y.saturating_add(offset[1]),
        origin.z.saturating_add(offset[2]),
    )
}

fn model_origin_floor_lift(world_height: i32) -> i32 {
    // Imported model vertices are normalized around their bounds
    // centre, while editor placements describe the floor contact
    // point. The model path's projected Y convention needs the
    // render origin offset by +half height for that floor anchor.
    world_height.max(0) / 2
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
    if instances.is_empty() || scratch.tex_used >= TRI_CAP {
        return;
    }

    let tex_start = scratch.tex_used;
    let mut triangles = psx_engine::PrimitiveArena::new(&mut scratch.tex_tris[tex_start..]);
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
    scratch.tex_used = tex_start.saturating_add(triangles.len()).min(TRI_CAP);
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
    let frame_q12 = if instance.autoplay {
        instance.animation.phase_at_tick_q12(tick, 60)
    } else {
        0
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
        face_pool[i] = psx_engine::TexturedModelRenderFace::new(
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
            (PREVIEW_GEOMETRY_SLOT_MIN as i32) << 6,
            (PREVIEW_GEOMETRY_SLOT_MAX as i32) << 6,
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
        blend_mode: room_geometry::psx_blend_mode(material.blend_mode),
        tint: (material.tint[0], material.tint[1], material.tint[2]),
        secondary_layer: material.enabled_secondary_layer().and_then(|layer| {
            Some(PreviewModelSecondaryLayer {
                texture: textures.secondary_slot(material_id)?,
                blend_mode: room_geometry::psx_blend_mode(layer.blend_mode),
                tint: (layer.tint[0], layer.tint[1], layer.tint[2]),
                motion: layer.motion,
            })
        }),
        face_sidedness: material.sidedness(),
    })
}

/// Marker colour per node kind, or `None` for nodes that aren't
/// placeable in 3D space (the World macro, the Room itself, plain
/// transform-only nodes).
fn entity_marker_color(kind: &NodeKind) -> Option<(u8, u8, u8)> {
    match kind {
        NodeKind::SpawnPoint { player: true, .. } => Some((0x60, 0xE0, 0x80)),
        NodeKind::SpawnPoint { player: false, .. } => Some((0x60, 0xB8, 0xF0)),
        NodeKind::MeshInstance { .. } => Some((0xC0, 0xC8, 0xD0)),
        NodeKind::ImageProp { .. } => Some((0xD0, 0xAA, 0x78)),
        // Box props render as real textured geometry in
        // `walk_box_props`; don't cover them with a 2D marker.
        NodeKind::BoxProp { .. }
        | NodeKind::CylinderProp { .. }
        | NodeKind::ArchProp { .. }
        | NodeKind::WaterVolume { .. } => None,
        NodeKind::Entity => Some((0xA0, 0xB0, 0xC0)),
        // Lights draw their own bulb icon + radius ring in
        // `walk_light_gizmos`; using the generic billboard square
        // makes them read like ordinary markers.
        NodeKind::PointLight { .. } => None,
        NodeKind::ParticleEmitter { .. } => Some((0x98, 0xD6, 0xE6)),
        NodeKind::Portal { .. } => None,
        // Logic nodes read through their selection bound (the
        // trigger-extent box); a billboard marker would double up.
        NodeKind::Logic { .. } => Some((0xC8, 0x8C, 0xE8)),
        NodeKind::ModelRenderer { .. }
        | NodeKind::Animator { .. }
        | NodeKind::Collider { .. }
        | NodeKind::CharacterController { .. }
        | NodeKind::Camera { .. }
        | NodeKind::Equipment { .. }
        | NodeKind::Interactable { .. }
        | NodeKind::PhysicsBody { .. }
        | NodeKind::Section { .. }
        | NodeKind::World { .. }
        | NodeKind::Node
        | NodeKind::Node3D => None,
    }
}

#[cfg(test)]
mod tests;
