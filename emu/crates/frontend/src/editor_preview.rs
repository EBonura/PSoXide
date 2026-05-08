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

use std::collections::HashSet;
use std::sync::Mutex;

use emulator_core::gpu::GpuCmdLogEntry;
use psx_gpu::material::{BlendMode, TextureMaterial};
use psx_gpu::ot::OrderingTable;
use psx_gpu::prim::TriFlat;
use psx_gpu::prim::TriTextured;
use psx_gte::math::{Mat3I16, Vec3I16, Vec3I32};
use psx_gte::scene as gte_scene;

use psxed_project::playtest::playtest_streaming_chunk_config;
use psxed_project::streaming::plan_generated_chunks;
use psxed_project::{
    spatial, Corner, GridDirection, GridSplit, GridUvTransform, NodeId, NodeKind, ProjectDocument,
    ResourceData, ResourceId, Scene, SceneNode, Transform3, WallCorner, WorldGrid,
};

use crate::editor_textures::{EditorTextures, MaterialSlot};
use psxed_ui::{PaintCellPreviewKind, ViewportCameraState};

/// Maximum sectors we'll attempt to render in one preview pass.
/// 64×64 grid would already be enormous for PSX (~16 MiB cooked); a
/// 4096-cap caps the per-frame primitive count at a comfortable
/// number for the host renderer.
const TRI_CAP: usize = 4096;
/// Model scratch mirrors editor-playtest's runtime model caps so the
/// editor preview exercises the same overflow behavior.
const PREVIEW_MODEL_VERTEX_CAP: usize = 1024;
const PREVIEW_MODEL_COMMAND_CAP: usize = TRI_CAP;
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
const PREVIEW_GEOMETRY_SLOT_MAX: usize = OT_DEPTH - 2;
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
/// Keeping the array inline alongside the OT in the static fixes
/// that and matches PS1's flat 2 MB main RAM layout.
struct PreviewScratch {
    ot: OrderingTable<OT_DEPTH>,
    tris: [TriFlat; TRI_CAP],
    tex_tris: [TriTextured; TRI_CAP],
    model_vertices: [psx_engine::ProjectedVertex; PREVIEW_MODEL_VERTEX_CAP],
    model_joint_transforms: [psx_engine::JointViewTransform; PREVIEW_JOINT_CAP],
    /// `0` = next free slot in `tris` (flat-shaded);
    /// `tex_used` = next free slot in `tex_tris`.
    used: usize,
    tex_used: usize,
    /// Host-drawn overlay lines for editor affordances. These stay
    /// outside the GP0 command log so the UI can draw fractional,
    /// overlaid strokes that are not limited by PSX integer pixels.
    overlay_lines: Vec<psxed_ui::EditorViewportOverlayLine>,
    /// GP0(02h) fill-rectangle packet: 1 tag word + 3 data words
    /// (`opcode|color`, `pack_xy(x, y)`, `pack_xy(w, h)`). Must live
    /// in the same static as the OT for the same reason the prim
    /// arrays do -- `iter_packets` reconstructs full pointers from
    /// the OT's 24-bit chain encoding plus the OT struct's high
    /// address bits, so chained packets must share that 16 MB
    /// region.
    clear_packet: [u32; 4],
}

const EMPTY_TRI: TriFlat = TriFlat::new([(0, 0), (0, 0), (0, 0)], 0, 0, 0);
const EMPTY_TEX_TRI: TriTextured = TriTextured::new(
    [(0, 0), (0, 0), (0, 0)],
    [(0, 0), (0, 0), (0, 0)],
    0,
    0,
    (0x80, 0x80, 0x80),
);

static SCRATCH: Mutex<PreviewScratch> = Mutex::new(PreviewScratch {
    ot: OrderingTable::new(),
    tris: [EMPTY_TRI; TRI_CAP],
    tex_tris: [EMPTY_TEX_TRI; TRI_CAP],
    model_vertices: [psx_engine::ProjectedVertex::new(0, 0, 0); PREVIEW_MODEL_VERTEX_CAP],
    model_joint_transforms: [psx_engine::JointViewTransform::ZERO; PREVIEW_JOINT_CAP],
    used: 0,
    tex_used: 0,
    overlay_lines: Vec::new(),
    clear_packet: [0; 4],
});

/// Render data for one editable 3D preview frame.
pub struct EditorPreviewFrame {
    /// PSX-style command log for the scene itself.
    pub cmd_log: Vec<GpuCmdLogEntry>,
    /// Host UI overlay lines for editor-only affordances.
    pub overlay_lines: Vec<psxed_ui::EditorViewportOverlayLine>,
}

/// Build a fresh preview frame rendering the project's first Room from
/// `camera`'s orbit angles.
///
/// Returns an empty frame if the project has no Rooms -- the editor
/// renderer will then paint a black panel, which is the correct "no
/// scene to show" affordance.
#[allow(clippy::too_many_arguments)]
pub fn build_phase1_frame(
    project: &ProjectDocument,
    camera: ViewportCameraState,
    preview_fog: bool,
    preview_backface_wireframe: bool,
    hidden_scene_nodes: &HashSet<NodeId>,
    selected: psxed_project::NodeId,
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
    let Some((room_id, grid)) = first_visible_room_grid(project, hidden_scene_nodes) else {
        return EditorPreviewFrame {
            cmd_log: Vec::new(),
            overlay_lines: Vec::new(),
        };
    };

    let mut scratch = SCRATCH.lock().expect("editor preview scratch mutex");
    scratch.used = 0;
    scratch.tex_used = 0;
    scratch.overlay_lines.clear();
    scratch.ot.clear();

    push_clear(&mut scratch);
    let world_camera = setup_gte_for_camera(camera);
    let fog = PreviewFog::from_grid(grid, preview_fog);
    walk_room(
        project,
        room_id,
        grid,
        textures,
        world_camera,
        fog,
        preview_backface_wireframe,
        hidden_scene_nodes,
        &mut scratch,
    );
    push_streaming_chunk_boundaries(grid, &mut scratch);
    walk_entities(project, grid, hidden_scene_nodes, selected, &mut scratch);
    walk_light_gizmos(
        project,
        grid,
        hidden_scene_nodes,
        selected,
        hovered_entity_node,
        &mut scratch,
    );

    // Selection / hover / paint overlays drawn before models --
    // they project through the camera GTE matrix that
    // `setup_gte_for_camera` installed. Models render after,
    // overwriting per-joint GTE state. We re-install the
    // camera state below before drawing entity bounds so they
    // pick up the same camera basis instead of the last
    // model joint matrix.
    if selected_primitives.is_empty() {
        if let Some(selection) = selected_primitive {
            push_selection_outline(grid, selection, OutlineRole::Selected, &mut scratch);
        }
    } else {
        for selection in selected_primitives {
            push_selection_outline(grid, *selection, OutlineRole::Selected, &mut scratch);
        }
    }
    for face in selected_sector_faces {
        push_face_outline(grid, *face, FACE_OUTLINE_SELECTED, &mut scratch);
    }
    if let Some(selection) = hovered_primitive {
        if Some(selection) != selected_primitive && !selected_primitives.contains(&selection) {
            push_selection_outline(grid, selection, OutlineRole::Hover, &mut scratch);
        }
    }
    if let Some(preview) = paint_target_preview {
        push_paint_preview(grid, preview, &mut scratch);
    }

    walk_model_instances(
        project,
        room_id,
        grid,
        textures,
        assets,
        selected,
        &world_camera,
        fog,
        hidden_scene_nodes,
        &mut scratch,
    );

    // Re-prime the GTE with the camera matrix -- model
    // rendering left it set to the last joint's view, which
    // would project entity bound lines into junk.
    let _ = setup_gte_for_camera(camera);
    walk_entity_bounds(entity_bounds, selected, hovered_entity_node, &mut scratch);
    if let Some((center, half_extents)) = selected_bounds {
        push_aabb_wireframe(&mut scratch, center, half_extents, ENTITY_BOUND_SELECTED);
    }
    for selection in validation_issue_primitives {
        if selection.room() == room_id {
            push_selection_outline(grid, *selection, OutlineRole::Error, &mut scratch);
        }
    }

    // SAFETY: `scratch.tris` lives until end of this function (the
    // mutex guard keeps it alive); the OT chain pointers reference
    // packets inside that vec and are stable while the lock is held.
    let cmd_log = unsafe { psx_gpu_render::build_cmd_log(&scratch.ot) };
    EditorPreviewFrame {
        cmd_log,
        overlay_lines: scratch.overlay_lines.clone(),
    }
}

/// First Room that is not hidden by the editor Scene tree.
fn first_visible_room_grid<'a>(
    project: &'a ProjectDocument,
    hidden_scene_nodes: &HashSet<NodeId>,
) -> Option<(psxed_project::NodeId, &'a WorldGrid)> {
    let scene = project.active_scene();
    let room = scene.nodes().iter().find(|node| {
        matches!(node.kind, NodeKind::Room { .. })
            && !scene_node_hidden(scene, hidden_scene_nodes, node.id)
    })?;
    let NodeKind::Room { grid } = &room.kind else {
        return None;
    };
    Some((room.id, grid))
}

/// Configure the host-side GTE so subsequent `project_vertex` /
/// `project_triangle` calls produce screen-space coords for the
/// requested editor camera.
fn setup_gte_for_camera(camera: ViewportCameraState) -> psx_engine::WorldCamera {
    let cos_p = psx_gte::transform::cos_1_3_12(camera.pitch_q12) as i32;
    let sin_p = psx_gte::transform::sin_1_3_12(camera.pitch_q12) as i32;
    let cos_y = psx_gte::transform::cos_1_3_12(camera.yaw_q12) as i32;
    let sin_y = psx_gte::transform::sin_1_3_12(camera.yaw_q12) as i32;
    let anchor = camera.anchor_i32();
    let [cam_x, cam_y, cam_z] = camera.position_i32();

    // View rotation: world →camera. Built so that:
    //   row0 = right (= +X in camera space)
    //   row1 = -up   (PSX screen Y points down, so we flip)
    //   row2 = forward (= +Z in camera space; camera looks along view direction)
    // Matches `psx_engine::render3d::camera_gte_view_matrix`.
    let view = Mat3I16 {
        m: [
            [clamp_i16(cos_y), 0, clamp_i16(-sin_y)],
            [
                clamp_i16(-((sin_y * sin_p) >> 12)),
                clamp_i16(-cos_p),
                clamp_i16(-((cos_y * sin_p) >> 12)),
            ],
            [
                clamp_i16(-((sin_y * cos_p) >> 12)),
                clamp_i16(sin_p),
                clamp_i16(-((cos_y * cos_p) >> 12)),
            ],
        ],
    };

    // Vertex emit will subtract `anchor` from each world coord
    // (see `world_to_view`), so anything inside ±i16 of the
    // camera anchor is safe to GTE-project. Compose the GTE
    // translation around that anchor: view·(anchor - cam_world)
    // = view·(-cam_local) where cam_local lives entirely within
    // a small local range. Orbit anchors on its target; Free anchors
    // on its position, which keeps large authored rooms camera-local.
    let cam_local = [cam_x - anchor[0], cam_y - anchor[1], cam_z - anchor[2]];
    let tr = Vec3I32::new(
        -dot_view_world(view.m[0], cam_local),
        -dot_view_world(view.m[1], cam_local),
        -dot_view_world(view.m[2], cam_local),
    );

    set_view_anchor(anchor);
    gte_scene::load_rotation(&view);
    gte_scene::load_translation(tr);
    gte_scene::set_screen_offset(SCREEN_CX << 16, SCREEN_CY << 16);
    gte_scene::set_projection_plane(PROJ_H as u16);

    // Build a `WorldCamera` matching the same basis so the
    // engine model pass composes joint transforms against the
    // same view matrix the editor geometry just loaded.
    psx_engine::WorldCamera::from_basis(
        psx_engine::WorldProjection::new(SCREEN_CX as i16, SCREEN_CY as i16, PROJ_H, 32),
        psx_engine::WorldVertex::new(cam_x, cam_y, cam_z),
        psx_engine::Q12::from_raw(sin_y),
        psx_engine::Q12::from_raw(cos_y),
        psx_engine::Q12::from_raw(sin_p),
        psx_engine::Q12::from_raw(cos_p),
    )
}

/// Shared anchor that `world_to_view` subtracts from each vertex
/// before squashing to `i16`. Set per-frame by
/// `setup_gte_for_camera` to the camera anchor so the emitted
/// vertices stay anchor-relative -- the GTE absorbs the offset via
/// its translation register. Without this, a single 32-sector
/// room (32 × 1024 = 32 768) sits exactly on the i16 cliff.
static VIEW_ANCHOR: std::sync::Mutex<[i32; 3]> = std::sync::Mutex::new([0, 0, 0]);

fn set_view_anchor(anchor: [i32; 3]) {
    if let Ok(mut a) = VIEW_ANCHOR.lock() {
        *a = anchor;
    }
}

fn view_anchor() -> [i32; 3] {
    VIEW_ANCHOR.lock().map(|a| *a).unwrap_or([0, 0, 0])
}

/// `view_row · world_pos` with the >>12 the GTE does internally for
/// matrix * world products.
fn dot_view_world(row: [i16; 3], v: [i32; 3]) -> i32 {
    let a = (row[0] as i32).saturating_mul(v[0]);
    let b = (row[1] as i32).saturating_mul(v[1]);
    let c = (row[2] as i32).saturating_mul(v[2]);
    a.saturating_add(b).saturating_add(c) >> 12
}

fn clamp_i16(value: i32) -> i16 {
    value.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

/// Walk every populated sector and emit triangles for floors,
/// ceilings, and the walls on each cardinal edge. Faces whose
/// material has a texture in the editor cache draw textured;
/// everything else falls back to flat shading. Light
/// accumulation happens per-face: the shade walks every
/// point light once, attenuates by distance to the face
/// centre, and modulates the base material colour.
fn walk_room(
    project: &ProjectDocument,
    room_id: psxed_project::NodeId,
    grid: &WorldGrid,
    textures: &EditorTextures,
    camera: psx_engine::WorldCamera,
    fog: PreviewFog,
    preview_backface_wireframe: bool,
    hidden_scene_nodes: &HashSet<NodeId>,
    scratch: &mut PreviewScratch,
) {
    let s = grid.sector_size;
    let lights = collect_preview_lights(project, room_id, grid, hidden_scene_nodes);
    let ambient = grid.ambient_color;
    for x in 0..grid.width {
        for z in 0..grid.depth {
            let Some(sector) = grid.sector(x, z) else {
                continue;
            };
            // Corner heights: [NW, NE, SE, SW] in `GridHorizontalFace`.
            // World coords with +X east, +Z south, +Y up.
            // `cell_world_x/z` add `grid.origin` so cells stay at the
            // same world position when the room grows in -X / -Z.
            let x0 = grid.cell_world_x(x);
            let x1 = x0 + s;
            let z0 = grid.cell_world_z(z);
            let z1 = z0 + s;

            if let Some(floor) = sector.floor.as_ref() {
                let center = horizontal_face_center([x0, x1, z0, z1], floor.heights);
                let shade_a = light_face(
                    face_shade(
                        project,
                        floor.triangle_material(0),
                        FALLBACK_FLOOR,
                        textures,
                    ),
                    center,
                    &lights,
                    ambient,
                );
                let shade_b = light_face(
                    face_shade(
                        project,
                        floor.triangle_material(1),
                        FALLBACK_FLOOR,
                        textures,
                    ),
                    center,
                    &lights,
                    ambient,
                );
                let shade_a = fog.apply_shade(shade_a, face_depth(camera, center));
                let shade_b = fog.apply_shade(shade_b, face_depth(camera, center));
                let face_ref = psxed_ui::FaceRef {
                    room: room_id,
                    sx: x,
                    sz: z,
                    kind: psxed_ui::FaceKind::Floor,
                };
                let emitted = push_horizontal_face(
                    scratch,
                    camera,
                    [x0, x1, z0, z1],
                    floor.heights,
                    floor.split,
                    floor.dropped_corner,
                    floor.triangle_uv(0),
                    shade_a,
                    floor.triangle_uv(1),
                    shade_b,
                    /* flip_winding */ false,
                );
                if !emitted && should_draw_culled_face_outline(preview_backface_wireframe, shade_a)
                {
                    push_culled_face_outline(grid, face_ref, shade_a, scratch);
                }
            }
            if let Some(ceiling) = sector.ceiling.as_ref() {
                let center = horizontal_face_center([x0, x1, z0, z1], ceiling.heights);
                let shade_a = light_face(
                    face_shade(
                        project,
                        ceiling.triangle_material(0),
                        FALLBACK_CEILING,
                        textures,
                    ),
                    center,
                    &lights,
                    ambient,
                );
                let shade_b = light_face(
                    face_shade(
                        project,
                        ceiling.triangle_material(1),
                        FALLBACK_CEILING,
                        textures,
                    ),
                    center,
                    &lights,
                    ambient,
                );
                let shade_a = fog.apply_shade(shade_a, face_depth(camera, center));
                let shade_b = fog.apply_shade(shade_b, face_depth(camera, center));
                let face_ref = psxed_ui::FaceRef {
                    room: room_id,
                    sx: x,
                    sz: z,
                    kind: psxed_ui::FaceKind::Ceiling,
                };
                let emitted = push_horizontal_face(
                    scratch,
                    camera,
                    [x0, x1, z0, z1],
                    ceiling.heights,
                    ceiling.split,
                    ceiling.dropped_corner,
                    ceiling.triangle_uv(0),
                    shade_a,
                    ceiling.triangle_uv(1),
                    shade_b,
                    // Ceiling normal points down; flipping the winding
                    // keeps backface-cullers happy and pins the inside
                    // surface as the visible side once we add culling.
                    /* flip_winding */
                    true,
                );
                if !emitted && should_draw_culled_face_outline(preview_backface_wireframe, shade_a)
                {
                    push_culled_face_outline(grid, face_ref, shade_a, scratch);
                }
            }
            for direction in GridDirection::ALL {
                let edge = WallEdge::from_direction(direction);
                for (stack_idx, face) in sector.walls.get(direction).iter().enumerate() {
                    let center = wall_face_center([x0, x1, z0, z1], edge, face.heights);
                    let shade = light_face(
                        face_shade(project, face.material, FALLBACK_WALL, textures),
                        center,
                        &lights,
                        ambient,
                    );
                    let shade = fog.apply_shade(shade, face_depth(camera, center));
                    let face_ref = psxed_ui::FaceRef {
                        room: room_id,
                        sx: x,
                        sz: z,
                        kind: psxed_ui::FaceKind::Wall {
                            dir: direction,
                            stack: stack_idx as u8,
                        },
                    };
                    let emitted = push_wall_face(
                        scratch,
                        camera,
                        [x0, x1, z0, z1],
                        edge,
                        face.heights,
                        face.dropped_corner,
                        face.uv,
                        shade,
                        [camera.position.x, camera.position.y, camera.position.z],
                    );
                    if !emitted
                        && should_draw_culled_face_outline(preview_backface_wireframe, shade)
                    {
                        push_culled_face_outline(grid, face_ref, shade, scratch);
                    }
                }
            }

            if scratch.used >= TRI_CAP || scratch.tex_used >= TRI_CAP {
                return;
            }
        }
    }
}

/// Per-face render description: either a texture sample with a
/// per-material tint, or a flat RGB. Resolved up-front so each
/// face's tri emit doesn't re-walk the resource table.
#[derive(Copy, Clone)]
enum FaceShade {
    Flat {
        rgb: (u8, u8, u8),
        sidedness: psxed_project::MaterialFaceSidedness,
    },
    Textured {
        slot: MaterialSlot,
        tint: (u8, u8, u8),
        sidedness: psxed_project::MaterialFaceSidedness,
    },
}

impl FaceShade {
    fn sidedness(self) -> psxed_project::MaterialFaceSidedness {
        match self {
            Self::Flat { sidedness, .. } | Self::Textured { sidedness, .. } => sidedness,
        }
    }

    fn with_sidedness(self, sidedness: psxed_project::MaterialFaceSidedness) -> Self {
        match self {
            Self::Flat { rgb, .. } => Self::Flat { rgb, sidedness },
            Self::Textured { slot, tint, .. } => Self::Textured {
                slot,
                tint,
                sidedness,
            },
        }
    }
}

fn face_shade(
    project: &ProjectDocument,
    material: Option<ResourceId>,
    fallback: (u8, u8, u8),
    textures: &EditorTextures,
) -> FaceShade {
    let tint = material_color(project, material, fallback);
    let sidedness = material_sidedness(project, material);
    if let Some(id) = material {
        if let Some(slot) = textures.slot(id) {
            return FaceShade::Textured {
                slot,
                tint: material_texture_tint(project, id),
                sidedness,
            };
        }
    }
    FaceShade::Flat {
        rgb: tint,
        sidedness,
    }
}

fn push_culled_face_outline(
    grid: &WorldGrid,
    face: psxed_ui::FaceRef,
    shade: FaceShade,
    scratch: &mut PreviewScratch,
) {
    if !should_draw_culled_face_outline(true, shade) {
        return;
    }
    push_face_outline(grid, face, FACE_OUTLINE_CULLED, scratch);
}

fn should_draw_culled_face_outline(preview_backface_wireframe: bool, shade: FaceShade) -> bool {
    preview_backface_wireframe
        && !matches!(
            shade.sidedness(),
            psxed_project::MaterialFaceSidedness::Both
        )
}

fn material_texture_tint(project: &ProjectDocument, material: ResourceId) -> (u8, u8, u8) {
    project
        .resource(material)
        .and_then(|resource| match &resource.data {
            ResourceData::Material(material) => Some(material.tint),
            _ => None,
        })
        .map(|[r, g, b]| (r, g, b))
        .unwrap_or((0x80, 0x80, 0x80))
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct PreviewFog {
    enabled: bool,
    rgb: (u8, u8, u8),
    near: i32,
    far: i32,
}

impl PreviewFog {
    fn from_grid(grid: &WorldGrid, preview_enabled: bool) -> Self {
        Self {
            enabled: preview_enabled && grid.fog_enabled,
            rgb: (grid.fog_color[0], grid.fog_color[1], grid.fog_color[2]),
            near: grid.fog_near,
            far: grid.fog_far,
        }
    }

    fn apply_shade(self, shade: FaceShade, depth: i32) -> FaceShade {
        match shade {
            FaceShade::Flat { rgb, sidedness } => FaceShade::Flat {
                rgb: self.apply_rgb(rgb, depth),
                sidedness,
            },
            FaceShade::Textured {
                slot,
                tint,
                sidedness,
            } => FaceShade::Textured {
                slot,
                tint: self.apply_rgb(tint, depth),
                sidedness,
            },
        }
    }

    fn apply_rgb(self, rgb: (u8, u8, u8), depth: i32) -> (u8, u8, u8) {
        if !self.enabled || self.far <= self.near || depth <= self.near {
            return rgb;
        }
        let weight =
            (((depth - self.near).saturating_mul(256)) / (self.far - self.near)).clamp(0, 256);
        let keep = 256 - weight;
        (
            fog_blend_channel(rgb.0, self.rgb.0, keep, weight),
            fog_blend_channel(rgb.1, self.rgb.1, keep, weight),
            fog_blend_channel(rgb.2, self.rgb.2, keep, weight),
        )
    }
}

fn fog_blend_channel(src: u8, fog: u8, keep: i32, weight: i32) -> u8 {
    (((src as i32) * keep + (fog as i32) * weight) / 256).clamp(0, 255) as u8
}

fn face_depth(camera: psx_engine::WorldCamera, center: [i32; 3]) -> i32 {
    camera
        .view_vertex(psx_engine::WorldVertex::new(
            center[0], center[1], center[2],
        ))
        .z
}

fn preview_vertices_in_front(camera: psx_engine::WorldCamera, verts: &[[i32; 3]]) -> bool {
    verts.iter().all(|v| {
        camera
            .view_vertex(psx_engine::WorldVertex::new(v[0], v[1], v[2]))
            .z
            >= camera.projection.near_z
    })
}

/// Walk every PointLight whose enclosing room is the active grid
/// and pre-multiply its
/// colour×intensity_q8. Lights authored outside any Room (no
/// enclosing parent) are skipped silently -- the cooker warns
/// about those, the preview just doesn't render them.
fn collect_preview_lights(
    project: &ProjectDocument,
    room_id: psxed_project::NodeId,
    grid: &WorldGrid,
    hidden_scene_nodes: &HashSet<NodeId>,
) -> Vec<psx_engine::PointLightSample> {
    let scene = project.active_scene();
    let mut out = Vec::new();
    for light in preview_lights(scene, hidden_scene_nodes) {
        // Filter by enclosing Room -- a light authored under
        // some other Room must not bleed into this one.
        if !is_descendant_of_room(scene, light.host_id, room_id) {
            continue;
        }
        push_preview_light_sample(
            &mut out,
            grid,
            &light.transform,
            light.color,
            light.intensity,
            light.radius,
        );
    }
    out
}

fn push_preview_light_sample(
    out: &mut Vec<psx_engine::PointLightSample>,
    grid: &WorldGrid,
    transform: &Transform3,
    color: [u8; 3],
    intensity: f32,
    radius: f32,
) {
    if radius <= 0.0 || !intensity.is_finite() || intensity < 0.0 {
        return;
    }
    let world = node_room_local_origin(grid, transform);
    // Editor `radius` is in sector units; convert to
    // engine units once here so the per-face attenuation
    // math stays in world space.
    let radius_engine = spatial::light_radius_engine_units(grid, radius);
    // Pre-multiply colour × intensity into u32 channels;
    // intensity scaled by 256 (Q8.8) keeps the per-face
    // accumulator in integer math.
    let intensity_q8 = (intensity * 256.0).clamp(0.0, u16::MAX as f32) as u32;
    out.push(psx_engine::PointLightSample::from_rgb_intensity(
        [world.x, world.y, world.z],
        radius_engine,
        psx_engine::Rgb8::from_array(color),
        psx_engine::Q8::from_raw(intensity_q8),
    ));
}

#[derive(Clone, Copy)]
struct PreviewLightMeta {
    host_id: NodeId,
    transform: Transform3,
    color: [u8; 3],
    intensity: f32,
    radius: f32,
}

fn preview_lights(scene: &Scene, hidden_scene_nodes: &HashSet<NodeId>) -> Vec<PreviewLightMeta> {
    let mut out = Vec::new();
    for node in scene.nodes() {
        if scene_node_hidden(scene, hidden_scene_nodes, node.id) {
            continue;
        }
        let NodeKind::PointLight {
            color,
            intensity,
            radius,
        } = &node.kind
        else {
            continue;
        };
        out.push(PreviewLightMeta {
            host_id: node.id,
            transform: node.transform,
            color: *color,
            intensity: *intensity,
            radius: *radius,
        });
    }
    out
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
    clip_override: Option<u16>,
    renderer_node: Option<NodeId>,
    animator_node: Option<NodeId>,
}

fn preview_model_reference(scene: &Scene, node: &SceneNode) -> Option<PreviewModelReference> {
    match &node.kind {
        NodeKind::MeshInstance {
            mesh: Some(model_id),
            animation_clip,
            ..
        } => Some(PreviewModelReference {
            model_id: *model_id,
            clip_override: *animation_clip,
            renderer_node: None,
            animator_node: None,
        }),
        NodeKind::Entity => {
            let mut renderer = None;
            let mut animator = None;
            for child in component_children(scene, node) {
                match &child.kind {
                    NodeKind::ModelRenderer {
                        model: Some(model_id),
                        ..
                    } if renderer.is_none() => {
                        renderer = Some((child.id, *model_id));
                    }
                    NodeKind::Animator { clip, .. } if animator.is_none() => {
                        animator = Some((child.id, *clip));
                    }
                    _ => {}
                }
            }
            renderer.map(|(renderer_node, model_id)| PreviewModelReference {
                model_id,
                clip_override: animator.and_then(|(_, clip)| clip),
                renderer_node: Some(renderer_node),
                animator_node: animator.map(|(node_id, _)| node_id),
            })
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
    controller_node: Option<NodeId>,
}

fn preview_player_reference(scene: &Scene, node: &SceneNode) -> Option<PreviewPlayerReference> {
    match &node.kind {
        NodeKind::SpawnPoint {
            player: true,
            character,
        } => Some(PreviewPlayerReference {
            character: *character,
            controller_node: None,
        }),
        NodeKind::Entity => component_children(scene, node).find_map(|child| {
            let NodeKind::CharacterController {
                character,
                player: true,
            } = &child.kind
            else {
                return None;
            };
            Some(PreviewPlayerReference {
                character: *character,
                controller_node: Some(child.id),
            })
        }),
        _ => None,
    }
}

fn preview_reference_selected(
    selected: NodeId,
    host_id: NodeId,
    component_a: Option<NodeId>,
    component_b: Option<NodeId>,
) -> bool {
    selected == host_id || component_a == Some(selected) || component_b == Some(selected)
}

fn preview_reference_hidden(
    scene: &Scene,
    hidden_scene_nodes: &HashSet<NodeId>,
    host_id: NodeId,
    component_a: Option<NodeId>,
    component_b: Option<NodeId>,
) -> bool {
    scene_node_hidden(scene, hidden_scene_nodes, host_id)
        || component_a.is_some_and(|id| scene_node_hidden(scene, hidden_scene_nodes, id))
        || component_b.is_some_and(|id| scene_node_hidden(scene, hidden_scene_nodes, id))
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

/// Centre of a horizontal face (floor / ceiling) -- average X /
/// Z of the bounds, mean of the four corner heights for Y.
fn horizontal_face_center(bounds: [i32; 4], heights: [i32; 4]) -> [i32; 3] {
    let [x0, x1, z0, z1] = bounds;
    let cx = (x0 + x1) / 2;
    let cz = (z0 + z1) / 2;
    let cy = (heights[0] as i64 + heights[1] as i64 + heights[2] as i64 + heights[3] as i64) / 4;
    [cx, cy as i32, cz]
}

/// Centre of a wall face -- midpoint of the wall's bottom edge
/// in X/Z, midpoint of the four corner heights for Y. Wall
/// edges run along one of the cell's cardinal or diagonal edges; the
/// `WallEdge` picks which.
fn wall_face_center(bounds: [i32; 4], edge: WallEdge, heights: [i32; 4]) -> [i32; 3] {
    let [x0, x1, z0, z1] = bounds;
    let (cx, cz) = match edge {
        WallEdge::North => ((x0 + x1) / 2, z1),
        WallEdge::East => (x1, (z0 + z1) / 2),
        WallEdge::South => ((x0 + x1) / 2, z0),
        WallEdge::West => (x0, (z0 + z1) / 2),
        WallEdge::NorthWestSouthEast | WallEdge::NorthEastSouthWest => {
            ((x0 + x1) / 2, (z0 + z1) / 2)
        }
    };
    let cy = (heights[0] as i64 + heights[1] as i64 + heights[2] as i64 + heights[3] as i64) / 4;
    [cx, cy as i32, cz]
}

/// Apply per-face lighting: ambient + linear-attenuation sum
/// of every point light whose radius covers `face_center`.
/// Final RGB clamps to 8 bits and modulates the input shade.
/// Lighting convention (PSX-neutral):
///
/// * `light_rgb` is in `0..=255` per channel.
/// * `128` = neutral -- material renders at its base brightness.
/// * `0`   = pitch black.
/// * `255` = saturated overbright (clamped at the modulate
///   step).
///
/// Both the editor preview and the runtime use this scale.
/// Final colour = `base * light_rgb / 128`, clamped to `255`.
fn light_face(
    base: FaceShade,
    face_center: [i32; 3],
    lights: &[psx_engine::PointLightSample],
    ambient: [u8; 3],
) -> FaceShade {
    let base_color = match base {
        FaceShade::Flat { rgb, .. } => rgb,
        FaceShade::Textured { tint, .. } => tint,
    };
    let (r, g, b) = psx_engine::shade_material_tint_with_lights(
        psx_engine::MaterialTint::from_tuple(base_color),
        face_center,
        psx_engine::Rgb8::from_array(ambient),
        lights.iter().copied(),
    )
    .to_tuple();
    match base {
        FaceShade::Flat { sidedness, .. } => FaceShade::Flat {
            rgb: (r, g, b),
            sidedness,
        },
        FaceShade::Textured {
            slot, sidedness, ..
        } => FaceShade::Textured {
            slot,
            tint: (r, g, b),
            sidedness,
        },
    }
}

/// Project the four corners of a sector-aligned horizontal face
/// and emit one or two triangles. `heights` is `[NW, NE, SE, SW]`.
/// `flip_winding=true` reverses the vertex order for ceilings.
/// `dropped_corner=Some(c)` makes the face a triangle: the half
/// containing `c` is skipped (`split` must already be on the
/// diagonal that keeps the other half alive -- `Corner::surviving_split`
/// enforces this at the data layer).
fn push_horizontal_face(
    scratch: &mut PreviewScratch,
    camera: psx_engine::WorldCamera,
    bounds: [i32; 4],
    heights: [i32; 4],
    split: GridSplit,
    dropped_corner: Option<psxed_project::Corner>,
    uv_transform_a: GridUvTransform,
    shade_a: FaceShade,
    uv_transform_b: GridUvTransform,
    shade_b: FaceShade,
    flip_winding: bool,
) -> bool {
    let [x0, x1, z0, z1] = bounds;
    let w_nw = [x0, heights[0], z1];
    let w_ne = [x1, heights[1], z1];
    let w_se = [x1, heights[2], z0];
    let w_sw = [x0, heights[3], z0];
    if !preview_vertices_in_front(camera, &[w_nw, w_ne, w_se, w_sw]) {
        return false;
    }
    let p_nw = gte_scene::project_vertex(world_to_view(w_nw));
    let p_ne = gte_scene::project_vertex(world_to_view(w_ne));
    let p_se = gte_scene::project_vertex(world_to_view(w_se));
    let p_sw = gte_scene::project_vertex(world_to_view(w_sw));
    let a_uvs = material_sized_uvs(
        shade_a,
        uv_transform_a.apply_to_quad(textured_base_uvs(shade_a, PREVIEW_FLOOR_UVS)),
    );
    let b_uvs = material_sized_uvs(
        shade_b,
        uv_transform_b.apply_to_quad(textured_base_uvs(shade_b, PREVIEW_FLOOR_UVS)),
    );

    let projected = [p_nw, p_ne, p_se, p_sw];
    let tri_a_corners = psxed_project::horizontal_triangle_corners(split, 0);
    let tri_b_corners = psxed_project::horizontal_triangle_corners(split, 1);
    let tri_a = (
        select_projected_corners(projected, tri_a_corners),
        select_uv_corners(a_uvs, tri_a_corners),
        tri_a_corners,
    );
    let tri_b = (
        select_projected_corners(projected, tri_b_corners),
        select_uv_corners(b_uvs, tri_b_corners),
        tri_b_corners,
    );

    let triangle_contains =
        |members: [Corner; 3], target: Corner| -> bool { members.contains(&target) };
    let emit_triangle = |scratch: &mut PreviewScratch,
                         verts: [psx_gte::scene::Projected; 3],
                         uvs: [(u8, u8); 3],
                         shade: FaceShade| {
        if flip_winding {
            // Ceilings: forward `[0, 1, 2]` walk (CW from above
            // = CCW from below) so the inward normal points down.
            emit_face_tri(scratch, verts, uvs, shade)
        } else {
            // Floors: reverse to `[0, 2, 1]` (CCW from above),
            // matching the legacy non-flip winding.
            emit_face_tri(
                scratch,
                [verts[0], verts[2], verts[1]],
                [uvs[0], uvs[2], uvs[1]],
                shade,
            )
        }
    };

    let skip_a = dropped_corner
        .map(|c| triangle_contains(tri_a.2, c))
        .unwrap_or(false);
    let skip_b = dropped_corner
        .map(|c| triangle_contains(tri_b.2, c))
        .unwrap_or(false);
    let mut emitted = false;
    if !skip_a {
        emitted |= emit_triangle(scratch, tri_a.0, tri_a.1, shade_a);
    }
    if !skip_b {
        emitted |= emit_triangle(scratch, tri_b.0, tri_b.1, shade_b);
    }
    emitted
}

/// Which edge of the sector this wall sits on. The renderer needs
/// the four corner positions in a consistent order so heights[bl,
/// br, tr, tl] line up with the right world-space corners.
#[derive(Copy, Clone, Debug)]
enum WallEdge {
    North,
    East,
    South,
    West,
    NorthWestSouthEast,
    NorthEastSouthWest,
}

impl WallEdge {
    const fn from_direction(direction: GridDirection) -> Self {
        match direction {
            GridDirection::North => Self::North,
            GridDirection::East => Self::East,
            GridDirection::South => Self::South,
            GridDirection::West => Self::West,
            GridDirection::NorthWestSouthEast => Self::NorthWestSouthEast,
            GridDirection::NorthEastSouthWest => Self::NorthEastSouthWest,
        }
    }
}

fn wall_side_visible(
    sidedness: psxed_project::MaterialFaceSidedness,
    bounds: [i32; 4],
    edge: WallEdge,
    camera_position: [i32; 3],
) -> bool {
    let sidedness = wall_material_sidedness(sidedness);
    let [x0, x1, z0, z1] = bounds;
    let [cam_x, _, cam_z] = camera_position;
    let inside_distance = match edge {
        WallEdge::North => z1.saturating_sub(cam_z),
        WallEdge::East => x1.saturating_sub(cam_x),
        WallEdge::South => cam_z.saturating_sub(z0),
        WallEdge::West => cam_x.saturating_sub(x0),
        WallEdge::NorthWestSouthEast | WallEdge::NorthEastSouthWest => return true,
    };
    match sidedness {
        psxed_project::MaterialFaceSidedness::Both => true,
        psxed_project::MaterialFaceSidedness::Back => inside_distance >= 0,
        psxed_project::MaterialFaceSidedness::Front => inside_distance <= 0,
    }
}

fn wall_material_sidedness(
    sidedness: psxed_project::MaterialFaceSidedness,
) -> psxed_project::MaterialFaceSidedness {
    match sidedness {
        psxed_project::MaterialFaceSidedness::Front => psxed_project::MaterialFaceSidedness::Back,
        psxed_project::MaterialFaceSidedness::Back => psxed_project::MaterialFaceSidedness::Front,
        psxed_project::MaterialFaceSidedness::Both => psxed_project::MaterialFaceSidedness::Both,
    }
}

/// Build the four world-space corners of a wall face on `edge`
/// and emit one or two triangles. `heights` is the
/// `GridVerticalFace` `[bl, br, tr, tl]` quad. `dropped_corner`
/// makes the face a triangle: BR / TL skip the second triangle
/// of the BL-TR diagonal split; BL / TR fall through to the
/// other diagonal.
fn push_wall_face(
    scratch: &mut PreviewScratch,
    camera: psx_engine::WorldCamera,
    bounds: [i32; 4],
    edge: WallEdge,
    heights: [i32; 4],
    dropped_corner: Option<psxed_project::WallCorner>,
    uv_transform: GridUvTransform,
    shade: FaceShade,
    camera_position: [i32; 3],
) -> bool {
    if !wall_side_visible(shade.sidedness(), bounds, edge, camera_position) {
        return false;
    }
    let render_shade = shade.with_sidedness(psxed_project::MaterialFaceSidedness::Both);
    let [x0, x1, z0, z1] = bounds;
    // For each edge, "left" and "right" are picked so an observer
    // standing inside the sector sees the wall the right way up.
    let (bl_xy, br_xy, tr_xy, tl_xy) = match edge {
        WallEdge::North => ((x0, z1), (x1, z1), (x1, z1), (x0, z1)),
        WallEdge::East => ((x1, z1), (x1, z0), (x1, z0), (x1, z1)),
        WallEdge::South => ((x1, z0), (x0, z0), (x0, z0), (x1, z0)),
        WallEdge::West => ((x0, z0), (x0, z1), (x0, z1), (x0, z0)),
        WallEdge::NorthWestSouthEast => ((x0, z1), (x1, z0), (x1, z0), (x0, z1)),
        WallEdge::NorthEastSouthWest => ((x1, z1), (x0, z0), (x0, z0), (x1, z1)),
    };
    let w_bl = [bl_xy.0, heights[0], bl_xy.1];
    let w_br = [br_xy.0, heights[1], br_xy.1];
    let w_tr = [tr_xy.0, heights[2], tr_xy.1];
    let w_tl = [tl_xy.0, heights[3], tl_xy.1];
    if !preview_vertices_in_front(camera, &[w_bl, w_br, w_tr, w_tl]) {
        return false;
    }
    let p_bl = gte_scene::project_vertex(world_to_view(w_bl));
    let p_br = gte_scene::project_vertex(world_to_view(w_br));
    let p_tr = gte_scene::project_vertex(world_to_view(w_tr));
    let p_tl = gte_scene::project_vertex(world_to_view(w_tl));
    let uvs = material_sized_uvs(
        shade,
        uv_transform.apply_to_quad(textured_base_uvs(shade, PREVIEW_WALL_UVS)),
    );

    let projected = [p_bl, p_br, p_tr, p_tl];
    let shape = dropped_corner.map(psxed_project::wall_shape_for_dropped_corner);
    let make_triangle = |members: [WallCorner; 3]| {
        let members = [members[0], members[2], members[1]];
        (
            select_projected_wall_corners(projected, members),
            select_uv_wall_corners(uvs, members),
            members,
        )
    };
    let (tri_a, tri_b) = if let Some(shape) = shape {
        let members = psxed_project::wall_shape_triangle_corners(shape).unwrap_or(
            psxed_project::wall_triangle_corners(GridSplit::NorthWestSouthEast, 0),
        );
        (make_triangle(members), make_triangle(members))
    } else {
        (
            make_triangle(psxed_project::wall_triangle_corners(
                GridSplit::NorthWestSouthEast,
                0,
            )),
            make_triangle(psxed_project::wall_triangle_corners(
                GridSplit::NorthWestSouthEast,
                1,
            )),
        )
    };

    let skip =
        |members: [WallCorner; 3]| -> bool { dropped_corner.is_some_and(|c| members.contains(&c)) };
    // Endpoint order keeps wall UVs upright. Winding is the
    // separate concern: the authored wall back side faces the
    // owning cell/interior, while wall materials swap Front/Back so
    // authors can use front-sided materials for interior walls.
    let flip_winding = !matches!(
        wall_material_sidedness(shade.sidedness()),
        psxed_project::MaterialFaceSidedness::Back
    );
    let emit_wall_triangle = |scratch: &mut PreviewScratch,
                              verts: [psx_gte::scene::Projected; 3],
                              uvs: [(u8, u8); 3]| {
        if flip_winding {
            emit_face_tri(
                scratch,
                [verts[0], verts[2], verts[1]],
                [uvs[0], uvs[2], uvs[1]],
                render_shade,
            )
        } else {
            emit_face_tri(scratch, verts, uvs, render_shade)
        }
    };
    let mut emitted = false;
    if shape.is_some() || !skip(tri_a.2) {
        emitted |= emit_wall_triangle(scratch, tri_a.0, tri_a.1);
    }
    if shape.is_none() && !skip(tri_b.2) {
        emitted |= emit_wall_triangle(scratch, tri_b.0, tri_b.1);
    }
    emitted
}

fn textured_base_uvs(shade: FaceShade, textured_uvs: [(u8, u8); 4]) -> [(u8, u8); 4] {
    if matches!(shade, FaceShade::Textured { .. }) {
        textured_uvs
    } else {
        [(0, 0); 4]
    }
}

fn material_sized_uvs(shade: FaceShade, uvs: [(u8, u8); 4]) -> [(u8, u8); 4] {
    match shade {
        FaceShade::Textured { slot, .. } => [
            material_sized_uv(slot, uvs[0]),
            material_sized_uv(slot, uvs[1]),
            material_sized_uv(slot, uvs[2]),
            material_sized_uv(slot, uvs[3]),
        ],
        FaceShade::Flat { .. } => uvs,
    }
}

fn material_sized_uv(slot: MaterialSlot, (u, v): (u8, u8)) -> (u8, u8) {
    (
        material_sized_uv_component(u, slot.texture_width),
        material_sized_uv_component(v, slot.texture_height),
    )
}

fn material_sized_uv_component(value: u8, size: u8) -> u8 {
    let size = if size == 0 || size > GRID_TILE_UV {
        GRID_TILE_UV
    } else {
        size
    };
    ((u16::from(value) * u16::from(size)) / u16::from(GRID_TILE_UV)).min(u16::from(u8::MAX)) as u8
}

fn select_projected_corners(
    projected: [psx_gte::scene::Projected; 4],
    corners: [Corner; 3],
) -> [psx_gte::scene::Projected; 3] {
    [
        projected[corners[0].idx()],
        projected[corners[1].idx()],
        projected[corners[2].idx()],
    ]
}

fn select_uv_corners(uvs: [(u8, u8); 4], corners: [Corner; 3]) -> [(u8, u8); 3] {
    [
        uvs[corners[0].idx()],
        uvs[corners[1].idx()],
        uvs[corners[2].idx()],
    ]
}

fn select_projected_wall_corners(
    projected: [psx_gte::scene::Projected; 4],
    corners: [WallCorner; 3],
) -> [psx_gte::scene::Projected; 3] {
    [
        projected[corners[0].idx()],
        projected[corners[1].idx()],
        projected[corners[2].idx()],
    ]
}

fn select_uv_wall_corners(uvs: [(u8, u8); 4], corners: [WallCorner; 3]) -> [(u8, u8); 3] {
    [
        uvs[corners[0].idx()],
        uvs[corners[1].idx()],
        uvs[corners[2].idx()],
    ]
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
    grid: &WorldGrid,
    hidden_scene_nodes: &HashSet<NodeId>,
    selected: psxed_project::NodeId,
    scratch: &mut PreviewScratch,
) {
    let scene = project.active_scene();
    for node in scene.nodes() {
        if scene_node_hidden(scene, hidden_scene_nodes, node.id) {
            continue;
        }
        // Skip nodes that the model-preview pass renders as real
        // textured characters/models. Without this guard they'd get
        // both a marker square and the real model on top of each other.
        if host_renders_as_preview_model(project, scene, node) {
            continue;
        }
        let Some(kind_color) = entity_marker_color(&node.kind) else {
            continue;
        };
        let entity_world = node_room_local_origin(grid, &node.transform);
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

/// Per-frame tick used to advance animation phase for the
/// editor's looping model preview. Bumped once per
/// `build_phase1_frame` call. PSX angle / phase math needs
/// monotonic ticks rather than wall-clock, and the editor
/// frame rate fluctuates on host -- so this is "preview
/// frames", not real-time. Good enough for inspector preview.
static PREVIEW_TICK: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

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
    /// Atlas slot in the editor's model atlas region.
    atlas: MaterialSlot,
    /// Render origin (room-local engine units). Model placement
    /// stays floor-anchored in `InstanceMeta`; this is lifted to
    /// the cooked model's centre before drawing.
    origin: psx_engine::WorldVertex,
    /// Y-axis rotation matrix derived from the node's yaw.
    instance_rotation: Mat3I16,
    /// Lit and fogged model texture tint, matching editor-playtest's
    /// single-material actor lighting path.
    tint: (u8, u8, u8),
}

/// Render every Model-backed legacy `MeshInstance` or component
/// `Entity` in the scene as a real textured animated model.
/// Mirrors the runtime path in `editor-playtest`: parse `.psxmdl`
/// + `.psxt` + `.psxanim`, upload atlas (lazily -- done by
/// `EditorTextures::refresh_models`), then submit the model through
/// `psx-engine`'s canonical model render pass.
///
/// Models with bad/missing data are skipped silently -- the
/// editor inspector + cook validation surface those errors
/// elsewhere; the preview just keeps drawing what it can.
fn walk_model_instances(
    project: &ProjectDocument,
    room_id: psxed_project::NodeId,
    grid: &WorldGrid,
    textures: &EditorTextures,
    assets: &crate::editor_assets::EditorAssets,
    selected: psxed_project::NodeId,
    camera: &psx_engine::WorldCamera,
    fog: PreviewFog,
    hidden_scene_nodes: &HashSet<NodeId>,
    scratch: &mut PreviewScratch,
) {
    // Bump the global preview tick once per frame so the
    // animation loops at a stable rate regardless of how many
    // instances we render.
    let tick = PREVIEW_TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

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
        let Some(reference) = preview_static_model_reference(scene, node) else {
            continue;
        };
        if preview_reference_hidden(
            scene,
            hidden_scene_nodes,
            node.id,
            reference.renderer_node,
            reference.animator_node,
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

        // Resolve clip: explicit instance override → preview clip → default.
        let clip_local = psxed_project::resolve::resolve_model_instance_preview_clip(
            model,
            reference.clip_override,
        )
        .unwrap_or(0);
        if (clip_local as usize)
            >= project
                .resolved_model_animation_clips(reference.model_id)
                .len()
        {
            continue;
        }

        // Model placements are floor anchors: X/Z follow the
        // authored node, Y is sampled from the floor under it.
        let origin = floor_anchored_node_room_local_origin(grid, &node.transform);

        let yaw_q12 = yaw_to_q12(node.transform.rotation_degrees[1]);
        let instance_rotation = yaw_rotation_q12(yaw_q12);

        instances_meta.push(InstanceMeta {
            mesh_id: reference.model_id,
            clip_local,
            origin,
            instance_rotation,
            atlas: atlas_slot,
            is_selected: preview_reference_selected(
                selected,
                node.id,
                reference.renderer_node,
                reference.animator_node,
            ),
            yaw_q12,
            collision_radius: model.collision_radius as i32,
            world_height: model.world_height as i32,
        });
    }

    // Player-spawn preview: render the player's character at
    // the spawn so level designers see where the player starts
    // *and* what they look like. Reuses the same model render
    // path -- no separate player renderer.
    walk_player_spawn_preview(
        project,
        grid,
        textures,
        hidden_scene_nodes,
        selected,
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
        let origin = floor_anchored_model_origin(meta.origin, meta.world_height);
        instances.push(PreviewModelInstance {
            model,
            animation,
            atlas: meta.atlas,
            origin,
            instance_rotation: meta.instance_rotation,
            tint: shade_model_tint(origin, *camera, fog, &lights, ambient),
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
    grid: &WorldGrid,
    textures: &EditorTextures,
    hidden_scene_nodes: &HashSet<NodeId>,
    selected: psxed_project::NodeId,
    instances_meta: &mut Vec<InstanceMeta>,
) {
    let scene = project.active_scene();
    for node in scene.nodes() {
        if instances_meta.len() >= MAX_PREVIEW_MODEL_INSTANCES {
            break;
        }
        let Some(reference) = preview_player_reference(scene, node) else {
            continue;
        };
        if preview_reference_hidden(
            scene,
            hidden_scene_nodes,
            node.id,
            reference.controller_node,
            None,
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
        let Some(model_id) = char_resource.model else {
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

        // Idle clip drives the preview loop -- the spec wants
        // designers to see "what would the player be doing
        // standing still here". Falls through to the model's
        // preview / default clip if the Character has no idle
        // assigned, so the surface still renders even when the
        // Character is mid-author.
        let clip_local = psxed_project::resolve::resolve_character_idle_preview_clip_for_model(
            project,
            char_resource,
            model_id,
            model,
        )
        .unwrap_or(0);
        if (clip_local as usize) >= project.resolved_model_animation_clips(model_id).len() {
            continue;
        }

        let origin = floor_anchored_node_room_local_origin(grid, &node.transform);
        let yaw_q12 = yaw_to_q12(node.transform.rotation_degrees[1]);
        let instance_rotation = yaw_rotation_q12(yaw_q12);

        instances_meta.push(InstanceMeta {
            mesh_id: model_id,
            clip_local,
            origin,
            instance_rotation,
            atlas: atlas_slot,
            // Host/controller node is selected, not the model --
            // but the preview gizmo still helps designers see
            // which spawn/controller they have selected.
            is_selected: preview_reference_selected(
                selected,
                node.id,
                reference.controller_node,
                None,
            ),
            yaw_q12,
            collision_radius: model.collision_radius as i32,
            world_height: model.world_height as i32,
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
    let s = psx_gte::transform::sin_1_3_12(meta.yaw_q12) as i32;
    let c = psx_gte::transform::cos_1_3_12(meta.yaw_q12) as i32;
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
    instance_rotation: Mat3I16,
    atlas: MaterialSlot,
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

fn model_origin_floor_lift(world_height: i32) -> i32 {
    // Imported model vertices are normalized around their bounds
    // centre, while editor placements describe the floor contact
    // point. The model path's projected Y convention needs the
    // render origin offset by +half height for that floor anchor.
    world_height.max(0) / 2
}

/// Convert editor-Y rotation in degrees to PSX angle units
/// (Q12, 4096 per turn). Matches the playtest writer's
/// `yaw_from_degrees`.
fn yaw_to_q12(degrees: f32) -> u16 {
    let normalised = degrees.rem_euclid(360.0);
    (normalised * (4096.0 / 360.0)) as i32 as u16
}

/// Y-axis rotation matrix in Q12. Mirrors `yaw_rotation_matrix`
/// in editor-playtest's runtime.
fn yaw_rotation_q12(yaw_q12: u16) -> Mat3I16 {
    let s = psx_gte::transform::sin_1_3_12(yaw_q12);
    let c = psx_gte::transform::cos_1_3_12(yaw_q12);
    Mat3I16 {
        m: [[c, 0, s], [0, 0x1000, 0], [-s, 0, c]],
    }
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
            &mut scratch.model_joint_transforms,
        ) {
            break;
        }
    }

    world.flush();
    scratch.tex_used = tex_start.saturating_add(triangles.len()).min(TRI_CAP);
}

#[allow(clippy::too_many_arguments)]
fn submit_preview_model_instance(
    world: &mut psx_engine::WorldRenderPass<'_, '_, OT_DEPTH>,
    triangles: &mut psx_engine::PrimitiveArena<'_, TriTextured>,
    camera: &psx_engine::WorldCamera,
    tick: u32,
    instance: &PreviewModelInstance<'_>,
    projected_vertices: &mut [psx_engine::ProjectedVertex],
    joint_view_transforms: &mut [psx_engine::JointViewTransform],
) -> bool {
    let frame_q12 = instance.animation.phase_at_tick_q12(tick, 60);
    let material = TextureMaterial::opaque(
        instance.atlas.clut_word,
        instance.atlas.tpage_word,
        instance.tint,
    );
    let options = preview_model_surface_options(material);

    let stats = world.submit_textured_model(
        triangles,
        instance.model,
        instance.animation,
        frame_q12,
        *camera,
        instance.origin,
        instance.instance_rotation,
        projected_vertices,
        joint_view_transforms,
        material,
        options,
    );

    stats.primitive_overflow || stats.command_overflow
}

fn preview_model_surface_options(material: TextureMaterial) -> psx_engine::WorldSurfaceOptions {
    psx_engine::WorldSurfaceOptions::new(
        psx_engine::DepthBand::new(PREVIEW_GEOMETRY_SLOT_MIN, PREVIEW_GEOMETRY_SLOT_MAX),
        psx_engine::DepthRange::new(
            (PREVIEW_GEOMETRY_SLOT_MIN as i32) << 6,
            (PREVIEW_GEOMETRY_SLOT_MAX as i32) << 6,
        ),
    )
    .with_depth_policy(psx_engine::DepthPolicy::Average)
    .with_cull_mode(psx_engine::CullMode::Back)
    .with_material_layer(material)
    .with_textured_triangle_splitting(false)
}

fn shade_model_tint(
    origin: psx_engine::WorldVertex,
    camera: psx_engine::WorldCamera,
    fog: PreviewFog,
    lights: &[psx_engine::PointLightSample],
    ambient: [u8; 3],
) -> (u8, u8, u8) {
    let lit = psx_engine::shade_material_tint_with_lights(
        psx_engine::MaterialTint::from_tuple((0x80, 0x80, 0x80)),
        [origin.x, origin.y, origin.z],
        psx_engine::Rgb8::from_array(ambient),
        lights.iter().copied(),
    )
    .to_tuple();
    fog.apply_rgb(lit, camera.view_vertex(origin).z)
}

/// Draw a horizontal radius ring plus a bulb icon for every
/// PointLight in the scene. The bulb
/// replaces the old coloured square marker so lights read as editor
/// light gizmos rather than generic entities.
fn walk_light_gizmos(
    project: &ProjectDocument,
    grid: &WorldGrid,
    hidden_scene_nodes: &HashSet<NodeId>,
    selected: psxed_project::NodeId,
    hovered: Option<psxed_project::NodeId>,
    scratch: &mut PreviewScratch,
) {
    let scene = project.active_scene();
    for light in preview_lights(scene, hidden_scene_nodes) {
        let center = node_room_local_origin(grid, &light.transform);
        let center_world = [center.x, center.y, center.z];
        let is_selected = preview_reference_selected(selected, light.host_id, None, None);
        let is_hovered =
            hovered.is_some_and(|id| preview_reference_selected(id, light.host_id, None, None));
        let style = if is_selected {
            FaceOutlineStyle {
                rgb: (0xFF, 0xE0, 0x80),
                thickness_px: EDITOR_PREVIEW_SELECTED_STROKE_WIDTH,
            }
        } else if is_hovered {
            FaceOutlineStyle {
                rgb: (0xFF, 0xF0, 0x90),
                thickness_px: EDITOR_PREVIEW_HOVER_STROKE_WIDTH,
            }
        } else {
            // Tint the unlit ring toward the authored colour
            // so multiple lights in a room read at a glance.
            FaceOutlineStyle {
                rgb: (
                    light.color[0].max(0x40),
                    light.color[1].max(0x40),
                    light.color[2].max(0x40),
                ),
                thickness_px: EDITOR_PREVIEW_HOVER_STROKE_WIDTH,
            }
        };
        // Light radius is authored in *sector units*; scale to
        // engine units so the ring matches the light's actual
        // attenuation footprint. The bulb remains visible even
        // for radius-zero lights.
        let radius_engine = spatial::light_radius_engine_units(grid, light.radius);
        if radius_engine > 0 {
            push_horizontal_ring(scratch, center_world, radius_engine, 16, style);
        }
        push_light_bulb_icon(
            scratch,
            center_world,
            (
                light.color[0].max(0x40),
                light.color[1].max(0x40),
                light.color[2].max(0x40),
            ),
            is_selected || is_hovered,
        );
    }
}

/// Wireframe AABB + facing arrow per selectable scene entity.
/// Bounds are gathered by `EditorWorkspace::collect_entity_bounds`
/// -- every entity-kind node carries an AABB the user can click to
/// select and drag to move. This pass renders the box wireframe for
/// non-light markers; lights use the bulb/radius gizmo above instead
/// of a generic cube.
///
/// Idle bounds draw thin and muted so they don't dominate the
/// viewport over the room they sit in. Hover and selected reuse
/// the room face palette for cross-tool consistency: yellow for
/// hover, cyan-bold for selected.
fn walk_entity_bounds(
    bounds: &[psxed_ui::EntityBounds],
    selected: psxed_project::NodeId,
    hovered: Option<psxed_project::NodeId>,
    scratch: &mut PreviewScratch,
) {
    for b in bounds {
        if matches!(b.kind, psxed_ui::EntityBoundKind::PointLight) {
            continue;
        }
        let is_selected = b.node == selected;
        let is_hovered = hovered == Some(b.node);
        let style = entity_bound_style(b.kind, is_selected, is_hovered);
        push_aabb_wireframe(scratch, b.center, b.half_extents, style);

        // Yaw arrow only for kinds with meaningful facing --
        // models and spawn points point at where they'll
        // render / face. Lights / triggers / portals / audio
        // are either omnidirectional or carry their own
        // direction gizmo elsewhere (light radius ring).
        if matches!(
            b.kind,
            psxed_ui::EntityBoundKind::Model
                | psxed_ui::EntityBoundKind::SpawnPoint
                | psxed_ui::EntityBoundKind::MeshFallback
        ) {
            push_facing_arrow(scratch, b.center, b.half_extents, b.yaw_degrees, style);
        }
    }
}

/// Pick the outline style for one bound. Selected wins over
/// hover; idle uses a muted kind-tinted thin line so multiple
/// boxes in a busy room read at a glance without dominating.
fn entity_bound_style(
    kind: psxed_ui::EntityBoundKind,
    selected: bool,
    hovered: bool,
) -> FaceOutlineStyle {
    if selected {
        return ENTITY_BOUND_SELECTED;
    }
    if hovered {
        return ENTITY_BOUND_HOVER;
    }
    let rgb = match kind {
        psxed_ui::EntityBoundKind::Model => (0xC0, 0xC8, 0xD0),
        psxed_ui::EntityBoundKind::MeshFallback => (0x90, 0x98, 0xA0),
        psxed_ui::EntityBoundKind::SpawnPoint => (0x60, 0xE0, 0x80),
        psxed_ui::EntityBoundKind::PointLight => (0xFF, 0xD8, 0x70),
        psxed_ui::EntityBoundKind::Trigger => (0xC8, 0x80, 0xE0),
        psxed_ui::EntityBoundKind::Portal => (0xFF, 0xB0, 0x60),
        psxed_ui::EntityBoundKind::AudioSource => (0x70, 0xD8, 0xC0),
    };
    FaceOutlineStyle {
        rgb,
        thickness_px: EDITOR_PREVIEW_HOVER_STROKE_WIDTH,
    }
}

/// Project the 8 corners of a world-space AABB and emit the 12
/// edges as `push_screen_line` segments. Coordinates are stored
/// `f32` in the bound; rounded to `i32` here because the GTE
/// shim wants integer world coords.
fn push_aabb_wireframe(
    scratch: &mut PreviewScratch,
    center: [f32; 3],
    half_extents: [f32; 3],
    style: FaceOutlineStyle,
) {
    let cx = center[0].round() as i32;
    let cy = center[1].round() as i32;
    let cz = center[2].round() as i32;
    let hx = half_extents[0].round() as i32;
    let hy = half_extents[1].round() as i32;
    let hz = half_extents[2].round() as i32;
    if hx <= 0 || hy <= 0 || hz <= 0 {
        return;
    }
    let lo = [cx - hx, cy - hy, cz - hz];
    let hi = [cx + hx, cy + hy, cz + hz];
    // Corner index encoding: bit0 = X (lo/hi), bit1 = Y, bit2 = Z.
    let corner = |i: usize| -> [i32; 3] {
        [
            if i & 1 != 0 { hi[0] } else { lo[0] },
            if i & 2 != 0 { hi[1] } else { lo[1] },
            if i & 4 != 0 { hi[2] } else { lo[2] },
        ]
    };
    let p: [_; 8] = std::array::from_fn(|i| gte_scene::project_vertex(world_to_view(corner(i))));
    // 12 edges of a box: 4 along X, 4 along Y, 4 along Z. Pairs
    // of corners that differ in exactly one bit.
    const EDGES: [(usize, usize); 12] = [
        (0, 1),
        (2, 3),
        (4, 5),
        (6, 7), // along X
        (0, 2),
        (1, 3),
        (4, 6),
        (5, 7), // along Y
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7), // along Z
    ];
    for (a, b) in EDGES {
        if p[a].sz == 0 || p[b].sz == 0 {
            continue;
        }
        push_screen_line(scratch, p[a], p[b], style);
    }
}

/// Draw a forward-pointing arrow from the bound centre out
/// past the front face, indicating where the entity faces.
/// Length scales with the bound's horizontal extent so big
/// models get a visible arrow and tiny markers don't grow
/// horns.
fn push_facing_arrow(
    scratch: &mut PreviewScratch,
    center: [f32; 3],
    half_extents: [f32; 3],
    yaw_degrees: f32,
    style: FaceOutlineStyle,
) {
    let yaw_q12 = yaw_to_q12(yaw_degrees);
    let s = psx_gte::transform::sin_1_3_12(yaw_q12) as i32;
    let c = psx_gte::transform::cos_1_3_12(yaw_q12) as i32;
    // Arrow length = bound's horizontal half-extent + a small
    // overshoot so the head sits clearly outside the box.
    let reach = (half_extents[0].max(half_extents[2]) * 1.5).max(96.0) as i32;
    let cx = center[0].round() as i32;
    let cy = center[1].round() as i32;
    let cz = center[2].round() as i32;
    let tip = [cx + ((s * reach) >> 12), cy, cz + ((c * reach) >> 12)];
    let p_origin = gte_scene::project_vertex(world_to_view([cx, cy, cz]));
    let p_tip = gte_scene::project_vertex(world_to_view(tip));
    if p_origin.sz != 0 && p_tip.sz != 0 {
        push_screen_line(scratch, p_origin, p_tip, style);
    }
}

/// Project a horizontal `segments`-sided polygon at world
/// `center` with `radius` into screen space and emit one
/// `push_screen_line` per edge. Used for light radius gizmos
/// and any future ground-plane affordances.
fn push_horizontal_ring(
    scratch: &mut PreviewScratch,
    center: [i32; 3],
    radius: i32,
    segments: u16,
    style: FaceOutlineStyle,
) {
    if segments < 3 || radius <= 0 {
        return;
    }
    let mut prev_world = [center[0] + radius, center[1], center[2]];
    let mut prev_proj = gte_scene::project_vertex(world_to_view(prev_world));
    for i in 1..=segments {
        // PSX trig uses 4096 units per turn; sample the unit
        // circle around the light origin once per segment.
        let angle_q12 = ((i as u32 * 4096) / segments as u32) as u16;
        let s = psx_gte::transform::sin_1_3_12(angle_q12) as i32;
        let c = psx_gte::transform::cos_1_3_12(angle_q12) as i32;
        let next_world = [
            center[0] + ((c * radius) >> 12),
            center[1],
            center[2] + ((s * radius) >> 12),
        ];
        let next_proj = gte_scene::project_vertex(world_to_view(next_world));
        if prev_proj.sz != 0 && next_proj.sz != 0 {
            push_screen_line(scratch, prev_proj, next_proj, style);
        }
        prev_world = next_world;
        prev_proj = next_proj;
    }
    let _ = prev_world; // silence the unused-final-assignment lint
}

fn push_light_bulb_icon(
    scratch: &mut PreviewScratch,
    center_world: [i32; 3],
    rgb: (u8, u8, u8),
    emphasized: bool,
) {
    let projected = gte_scene::project_vertex(world_to_view(center_world));
    if projected.sz == 0 {
        return;
    }
    let cx = projected.sx as f32;
    let cy = projected.sy as f32;
    let radius = if emphasized { 8.5 } else { 7.0 };
    let icon = FaceOutlineStyle {
        rgb,
        thickness_px: if emphasized { 1.0 } else { 0.75 },
    };
    let glass = egui::pos2(cx, cy - radius * 0.28);
    let segments = 16;
    for i in 0..segments {
        let a0 = i as f32 * std::f32::consts::TAU / segments as f32;
        let a1 = (i + 1) as f32 * std::f32::consts::TAU / segments as f32;
        push_overlay_line(
            scratch,
            egui::pos2(glass.x + a0.cos() * radius, glass.y + a0.sin() * radius),
            egui::pos2(glass.x + a1.cos() * radius, glass.y + a1.sin() * radius),
            icon,
        );
    }

    let neck_y = glass.y + radius * 0.72;
    let base_y = neck_y + radius * 0.62;
    let neck_half = radius * 0.46;
    let base_half = radius * 0.34;
    push_overlay_line(
        scratch,
        egui::pos2(cx - neck_half, neck_y),
        egui::pos2(cx - base_half, base_y),
        icon,
    );
    push_overlay_line(
        scratch,
        egui::pos2(cx + neck_half, neck_y),
        egui::pos2(cx + base_half, base_y),
        icon,
    );
    for step in [0.0, 0.28, 0.56] {
        let y = neck_y + radius * step;
        push_overlay_line(
            scratch,
            egui::pos2(cx - base_half, y),
            egui::pos2(cx + base_half, y),
            icon,
        );
    }

    let filament_y = glass.y + radius * 0.18;
    push_overlay_line(
        scratch,
        egui::pos2(cx - radius * 0.36, filament_y),
        egui::pos2(cx - radius * 0.12, filament_y + radius * 0.2),
        icon,
    );
    push_overlay_line(
        scratch,
        egui::pos2(cx - radius * 0.12, filament_y + radius * 0.2),
        egui::pos2(cx + radius * 0.12, filament_y + radius * 0.2),
        icon,
    );
    push_overlay_line(
        scratch,
        egui::pos2(cx + radius * 0.12, filament_y + radius * 0.2),
        egui::pos2(cx + radius * 0.36, filament_y),
        icon,
    );

    if emphasized {
        let halo = FaceOutlineStyle {
            rgb: (0xFF, 0xFF, 0xFF),
            thickness_px: 0.45,
        };
        let halo_radius = radius + 2.0;
        for i in 0..segments {
            let a0 = i as f32 * std::f32::consts::TAU / segments as f32;
            let a1 = (i + 1) as f32 * std::f32::consts::TAU / segments as f32;
            push_overlay_line(
                scratch,
                egui::pos2(
                    glass.x + a0.cos() * halo_radius,
                    glass.y + a0.sin() * halo_radius,
                ),
                egui::pos2(
                    glass.x + a1.cos() * halo_radius,
                    glass.y + a1.sin() * halo_radius,
                ),
                halo,
            );
        }
    }
}

fn synth(sx: i16, sy: i16, sz: u16) -> psx_gte::scene::Projected {
    psx_gte::scene::Projected { sx, sy, sz }
}

/// Marker colour per node kind, or `None` for nodes that aren't
/// placeable in 3D space (the World macro, the Room itself, plain
/// transform-only nodes).
fn entity_marker_color(kind: &NodeKind) -> Option<(u8, u8, u8)> {
    match kind {
        NodeKind::SpawnPoint { player: true, .. } => Some((0x60, 0xE0, 0x80)),
        NodeKind::SpawnPoint { player: false, .. } => Some((0x60, 0xB8, 0xF0)),
        NodeKind::MeshInstance { .. } => Some((0xC0, 0xC8, 0xD0)),
        NodeKind::Entity => Some((0xA0, 0xB0, 0xC0)),
        // Lights draw their own bulb icon + radius ring in
        // `walk_light_gizmos`; using the generic billboard square
        // makes them read like ordinary markers.
        NodeKind::PointLight { .. } => None,
        NodeKind::Trigger { .. } => Some((0xC8, 0x80, 0xE0)),
        NodeKind::Portal { .. } => Some((0xFF, 0xB0, 0x60)),
        NodeKind::AudioSource { .. } => Some((0x70, 0xD8, 0xC0)),
        NodeKind::ModelRenderer { .. }
        | NodeKind::Animator { .. }
        | NodeKind::Collider { .. }
        | NodeKind::Interactable { .. }
        | NodeKind::CharacterController { .. }
        | NodeKind::AiController { .. }
        | NodeKind::Combat { .. }
        | NodeKind::Equipment { .. }
        | NodeKind::Room { .. }
        | NodeKind::World { .. }
        | NodeKind::Node
        | NodeKind::Node3D => None,
    }
}

const FALLBACK_FLOOR: (u8, u8, u8) = (0xB0, 0xA0, 0x88);
const FALLBACK_WALL: (u8, u8, u8) = (0x88, 0x70, 0x58);
const FALLBACK_CEILING: (u8, u8, u8) = (0x60, 0x60, 0x70);

/// Pick the GP0 RGB triple to paint a face with.
///
/// Authored `MaterialResource::tint` defaults to PSX-neutral
/// `(0x80, 0x80, 0x80)` because that's the right value when sampling
/// a textured polygon (output = texel × tint / 128). For the editor's
/// pre-textured flat-shaded preview that means every face renders the
/// same dull grey -- useless for distinguishing materials. Mirror the
/// 2D viewport's approach: derive a colour from the material's name
/// so a project's "Floor Material" / "Brick Material" / "Glass" all
/// land at distinct, recognisable hues until real texturing arrives.
fn material_color(
    project: &ProjectDocument,
    material: Option<ResourceId>,
    fallback: (u8, u8, u8),
) -> (u8, u8, u8) {
    let Some(id) = material else {
        return fallback;
    };
    let Some(resource) = project.resource(id) else {
        return fallback;
    };
    let name = resource.name.to_ascii_lowercase();
    if name.contains("brick") {
        (0xC8, 0x70, 0x40)
    } else if name.contains("floor") || name.contains("stone") {
        (0xB6, 0xAC, 0x96)
    } else if name.contains("glass") {
        (0x70, 0xA8, 0xC0)
    } else if name.contains("wood") {
        (0x90, 0x60, 0x40)
    } else if name.contains("metal") {
        (0x90, 0x96, 0x9A)
    } else if let ResourceData::Material(mat) = &resource.data {
        // Author actually tinted the material away from neutral -- use
        // the tint directly. The mid-grey default falls through to
        // the role-specific fallback below.
        if mat.tint != [0x80, 0x80, 0x80] {
            let [r, g, b] = mat.tint;
            (r, g, b)
        } else {
            fallback
        }
    } else {
        fallback
    }
}

fn material_sidedness(
    project: &ProjectDocument,
    material: Option<ResourceId>,
) -> psxed_project::MaterialFaceSidedness {
    material
        .and_then(|id| project.resource(id))
        .and_then(|resource| match &resource.data {
            ResourceData::Material(material) => Some(material.sidedness()),
            _ => None,
        })
        .unwrap_or_default()
}

/// Squash a world-space i32 corner into the i16 the GTE V0 register
/// expects. Subtracts the per-frame view anchor (= camera target)
/// first so the emitted coord is anchor-relative. With sector_size
/// 1024, this gives ±32 sectors of headroom from the camera target
/// before clamp truncation kicks in -- comfortably the editor's
/// budget cap.
///
/// Debug builds assert; release silently saturates rather than
/// crashing the editor mid-paint.
fn world_to_view(world: [i32; 3]) -> Vec3I16 {
    let a = view_anchor();
    let lx = world[0] - a[0];
    let ly = world[1] - a[1];
    let lz = world[2] - a[2];
    debug_assert!(
        (i16::MIN as i32..=i16::MAX as i32).contains(&lx)
            && (i16::MIN as i32..=i16::MAX as i32).contains(&ly)
            && (i16::MIN as i32..=i16::MAX as i32).contains(&lz),
        "vertex {:?} (anchor-relative {:?}) overflows i16 — room too big or camera anchor wrong",
        world,
        [lx, ly, lz]
    );
    Vec3I16::new(clamp_i16(lx), clamp_i16(ly), clamp_i16(lz))
}

/// Render the paint-target ghost outline. Cell ghosts trace the
/// would-be cell surface; wall ghosts use
/// `push_face_outline` with a synthetic `FaceRef` whose world cell
/// might lie outside the current grid -- `push_face_outline`'s
/// missing-data fallback supplies default heights for the ghost
/// case. World-cell coords let both work for cells the grid
/// hasn't allocated yet; the outline appears exactly where the
/// auto-grow would create the cell on click.
fn push_paint_preview(
    grid: &WorldGrid,
    preview: psxed_ui::PaintTargetPreview,
    scratch: &mut PreviewScratch,
) {
    match preview {
        psxed_ui::PaintTargetPreview::Cell {
            world_cell_x,
            world_cell_z,
            kind,
        } => push_cell_ghost_outline(grid, world_cell_x, world_cell_z, kind, scratch),
        psxed_ui::PaintTargetPreview::Wall {
            world_cell_x,
            world_cell_z,
            dir,
            stack,
        } => {
            // Translate world cell → array (when in bounds) so
            // existing wall data is read for the outline; for
            // off-grid ghosts we pass a synthetic array index that
            // can't collide with any real wall and let
            // `push_face_outline` fall back to default heights.
            let (sx, sz) = grid
                .world_cell_to_array(world_cell_x, world_cell_z)
                .unwrap_or((u16::MAX, u16::MAX));
            // Fake a FaceRef. `room` field is unused by
            // push_face_outline; safe to fill with anything.
            let face = psxed_ui::FaceRef {
                room: psxed_project::NodeId::ROOT,
                sx,
                sz,
                kind: psxed_ui::FaceKind::Wall { dir, stack },
            };
            // For off-grid wall ghosts we have to project the
            // outline ourselves -- `push_face_outline` short-
            // circuits when sx/sz are out of grid bounds.
            if sx == u16::MAX || sz == u16::MAX {
                let heights = grid.wall_heights_aligned_to_surfaces_for_world_cell(
                    world_cell_x,
                    world_cell_z,
                    dir,
                );
                push_ghost_wall_outline(grid, world_cell_x, world_cell_z, dir, heights, scratch);
            } else {
                push_face_outline(grid, face, FACE_OUTLINE_WALL_PAINT, scratch);
            }
        }
    }
}

/// Outline a cell at world-cell `(wcx, wcz)`. Floor / ceiling paint
/// previews use the same candidate heights the click path will
/// commit; generic cell previews stay on the ground footprint.
fn push_cell_ghost_outline(
    grid: &WorldGrid,
    wcx: i32,
    wcz: i32,
    kind: PaintCellPreviewKind,
    scratch: &mut PreviewScratch,
) {
    let s = grid.sector_size;
    let x0 = wcx * s;
    let x1 = x0 + s;
    let z0 = wcz * s;
    let z1 = z0 + s;
    let (heights, style) = match kind {
        PaintCellPreviewKind::Ground => ([0; 4], FACE_OUTLINE_HOVER),
        PaintCellPreviewKind::Floor => (
            grid.floor_heights_aligned_to_neighbors_for_world_cell(wcx, wcz, 0),
            FACE_OUTLINE_FLOOR_PAINT,
        ),
        PaintCellPreviewKind::Ceiling => (
            grid.ceiling_heights_aligned_to_neighbors_for_world_cell(wcx, wcz),
            FACE_OUTLINE_CEILING_PAINT,
        ),
    };
    const LIFT: i32 = 4;
    let nw = gte_scene::project_vertex(world_to_view([x0, heights[Corner::NW.idx()] + LIFT, z1]));
    let ne = gte_scene::project_vertex(world_to_view([x1, heights[Corner::NE.idx()] + LIFT, z1]));
    let se = gte_scene::project_vertex(world_to_view([x1, heights[Corner::SE.idx()] + LIFT, z0]));
    let sw = gte_scene::project_vertex(world_to_view([x0, heights[Corner::SW.idx()] + LIFT, z0]));
    if [nw, ne, se, sw].iter().any(|p| p.sz == 0) {
        return;
    }
    for (a, b) in [(nw, ne), (ne, se), (se, sw), (sw, nw)] {
        push_screen_line(scratch, a, b, style);
    }
}

/// Outline a wall at world-cell `(wcx, wcz)` on edge `dir`. Used
/// when `push_face_outline`'s array-bound check rejects an off-grid
/// ghost so the user still sees where the wall will land.
fn push_ghost_wall_outline(
    grid: &WorldGrid,
    wcx: i32,
    wcz: i32,
    dir: GridDirection,
    heights: [i32; 4],
    scratch: &mut PreviewScratch,
) {
    let s = grid.sector_size;
    const LIFT: i32 = 4;
    let bounds = spatial::cell_bounds_from_world_cell(wcx, wcz, s);
    let Some(corners) = spatial::editor_wall_outline_corners(bounds, dir, heights, LIFT) else {
        return;
    };
    let projected: [psx_gte::scene::Projected; 4] = [
        gte_scene::project_vertex(world_to_view(corners[0])),
        gte_scene::project_vertex(world_to_view(corners[1])),
        gte_scene::project_vertex(world_to_view(corners[2])),
        gte_scene::project_vertex(world_to_view(corners[3])),
    ];
    if projected.iter().any(|p| p.sz == 0) {
        return;
    }
    for i in 0..4 {
        push_screen_line(
            scratch,
            projected[i],
            projected[(i + 1) % 4],
            FACE_OUTLINE_WALL_PAINT,
        );
    }
}

/// Hover and Selected outline styling. RGB plus screen-space line
/// thickness in pixels. Keep these light: they are editor affordances,
/// not scene geometry, and thick strokes obscure PS1-scale surfaces.
const FACE_OUTLINE_HOVER: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0xFF, 0xE0, 0x60),
    thickness_px: EDITOR_PREVIEW_HOVER_STROKE_WIDTH,
};
const FACE_OUTLINE_SELECTED: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0x60, 0xC8, 0xFF),
    thickness_px: EDITOR_PREVIEW_SELECTED_STROKE_WIDTH,
};
const FACE_OUTLINE_ERROR: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0xFF, 0x40, 0x40),
    thickness_px: 4.0,
};
const FACE_OUTLINE_CULLED: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0x88, 0xA0, 0xAE),
    thickness_px: 1.0,
};
const ENTITY_BOUND_HOVER: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0xFF, 0xE0, 0x60),
    thickness_px: EDITOR_PREVIEW_HOVER_STROKE_WIDTH,
};
const ENTITY_BOUND_SELECTED: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0x60, 0xC8, 0xFF),
    thickness_px: EDITOR_PREVIEW_SELECTED_STROKE_WIDTH,
};
/// PaintWall hover preview -- green for "this would be added /
/// replaced". Slightly stronger than hover, but still thin enough
/// to leave the underlying face readable.
const FACE_OUTLINE_WALL_PAINT: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0x60, 0xFF, 0x90),
    thickness_px: EDITOR_PREVIEW_PAINT_STROKE_WIDTH,
};
const FACE_OUTLINE_FLOOR_PAINT: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0x60, 0xFF, 0x90),
    thickness_px: EDITOR_PREVIEW_PAINT_STROKE_WIDTH,
};
const FACE_OUTLINE_CEILING_PAINT: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0x80, 0xC8, 0xFF),
    thickness_px: EDITOR_PREVIEW_PAINT_STROKE_WIDTH,
};
const STREAMING_CHUNK_BOUNDARY: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0x60, 0xFF, 0xC4),
    thickness_px: 2.0,
};

#[derive(Copy, Clone)]
struct FaceOutlineStyle {
    rgb: (u8, u8, u8),
    thickness_px: f32,
}

/// Hover vs Selected -- outline style picker for the unified
/// selection dispatch. Hover uses the lighter yellow; selected
/// uses the bolder cyan. Same constants the original face-only
/// path consumed.
#[derive(Copy, Clone)]
enum OutlineRole {
    Hover,
    Selected,
    Error,
}

impl OutlineRole {
    fn face_style(self) -> FaceOutlineStyle {
        match self {
            Self::Hover => FACE_OUTLINE_HOVER,
            Self::Selected => FACE_OUTLINE_SELECTED,
            Self::Error => FACE_OUTLINE_ERROR,
        }
    }
}

/// Dispatch a `Selection` to the appropriate outline helper.
/// Each variant gets its own screen-space overlay: face → 4
/// edge lines, edge → 1 line, vertex → cross.
fn push_selection_outline(
    grid: &WorldGrid,
    selection: psxed_ui::Selection,
    role: OutlineRole,
    scratch: &mut PreviewScratch,
) {
    match selection {
        psxed_ui::Selection::Face(face) => {
            push_face_outline(grid, face, role.face_style(), scratch);
        }
        psxed_ui::Selection::Triangle(triangle) => {
            push_triangle_outline(grid, triangle, role.face_style(), scratch);
        }
        psxed_ui::Selection::Edge(edge) => {
            push_edge_outline(grid, edge, role.face_style(), scratch);
        }
        psxed_ui::Selection::Vertex(vertex) => {
            push_vertex_outline(grid, vertex, role.face_style(), scratch);
        }
    }
}

fn push_triangle_outline(
    grid: &WorldGrid,
    triangle: psxed_ui::HorizontalTriangleRef,
    style: FaceOutlineStyle,
    scratch: &mut PreviewScratch,
) {
    let [c0, c1, c2] = triangle.corners;
    let Some(w0) = psxed_ui::face_corner_world(grid, triangle.face_corner(c0)) else {
        return;
    };
    let Some(w1) = psxed_ui::face_corner_world(grid, triangle.face_corner(c1)) else {
        return;
    };
    let Some(w2) = psxed_ui::face_corner_world(grid, triangle.face_corner(c2)) else {
        return;
    };
    let projected = [
        gte_scene::project_vertex(world_to_view(w0)),
        gte_scene::project_vertex(world_to_view(w1)),
        gte_scene::project_vertex(world_to_view(w2)),
    ];
    if projected.iter().any(|p| p.sz == 0) {
        return;
    }
    for i in 0..3 {
        push_screen_line(scratch, projected[i], projected[(i + 1) % 3], style);
    }
}

fn push_streaming_chunk_boundaries(grid: &WorldGrid, scratch: &mut PreviewScratch) {
    let plan = plan_generated_chunks(grid, playtest_streaming_chunk_config());
    if plan.chunk_count() <= 1 {
        return;
    }
    let s = grid.sector_size;
    let y = 10;
    for chunk in plan.chunks {
        let x0 = chunk.world_origin[0] * s;
        let z0 = chunk.world_origin[1] * s;
        let x1 = x0 + chunk.size[0] as i32 * s;
        let z1 = z0 + chunk.size[1] as i32 * s;
        let projected: [psx_gte::scene::Projected; 4] = [
            gte_scene::project_vertex(world_to_view([x0, y, z1])),
            gte_scene::project_vertex(world_to_view([x1, y, z1])),
            gte_scene::project_vertex(world_to_view([x1, y, z0])),
            gte_scene::project_vertex(world_to_view([x0, y, z0])),
        ];
        if projected.iter().any(|p| p.sz == 0) {
            continue;
        }
        for i in 0..4 {
            push_screen_line(
                scratch,
                projected[i],
                projected[(i + 1) % 4],
                STREAMING_CHUNK_BOUNDARY,
            );
        }
    }
}

/// One thick screen-space line between the edge's two world
/// endpoints. Lifted slightly off the surface (same `LIFT` as
/// `push_face_outline`) so it doesn't z-fight the geometry it
/// outlines.
fn push_edge_outline(
    grid: &WorldGrid,
    edge: psxed_ui::EdgeRef,
    style: FaceOutlineStyle,
    scratch: &mut PreviewScratch,
) {
    let Some((a, b)) = edge_world_endpoints(grid, edge) else {
        return;
    };
    let projected_a = gte_scene::project_vertex(world_to_view(a));
    let projected_b = gte_scene::project_vertex(world_to_view(b));
    if projected_a.sz == 0 || projected_b.sz == 0 {
        return;
    }
    push_screen_line(scratch, projected_a, projected_b, style);
}

/// Small screen-space cross at the vertex's world position.
/// The cross is drawn as four short line segments offset along
/// world axes so its on-screen size scales naturally with
/// distance -- close vertices read clearly, far ones don't
/// dominate the viewport.
fn push_vertex_outline(
    grid: &WorldGrid,
    vertex: psxed_ui::VertexRef,
    style: FaceOutlineStyle,
    scratch: &mut PreviewScratch,
) {
    let Some(world) = vertex_world_position(grid, vertex) else {
        return;
    };
    // Half-extent of the cross in world units. ~32 reads as a
    // few px in the viewport at orbit distances we use.
    const ARM: i32 = 32;
    let arms = [
        (
            [world[0] - ARM, world[1], world[2]],
            [world[0] + ARM, world[1], world[2]],
        ),
        (
            [world[0], world[1] - ARM, world[2]],
            [world[0], world[1] + ARM, world[2]],
        ),
        (
            [world[0], world[1], world[2] - ARM],
            [world[0], world[1], world[2] + ARM],
        ),
    ];
    for (a, b) in arms {
        let pa = gte_scene::project_vertex(world_to_view(a));
        let pb = gte_scene::project_vertex(world_to_view(b));
        if pa.sz == 0 || pb.sz == 0 {
            continue;
        }
        push_screen_line(scratch, pa, pb, style);
    }
}

fn edge_world_endpoints(grid: &WorldGrid, edge: psxed_ui::EdgeRef) -> Option<([i32; 3], [i32; 3])> {
    use psxed_ui::{EdgeAnchor, FaceCornerRef};
    let (a, b) = match edge.anchor {
        EdgeAnchor::Floor { sx, sz, dir } => (
            FaceCornerRef::Floor {
                sx,
                sz,
                corner: floor_edge_a(dir),
            },
            FaceCornerRef::Floor {
                sx,
                sz,
                corner: floor_edge_b(dir),
            },
        ),
        EdgeAnchor::Ceiling { sx, sz, dir } => (
            FaceCornerRef::Ceiling {
                sx,
                sz,
                corner: floor_edge_a(dir),
            },
            FaceCornerRef::Ceiling {
                sx,
                sz,
                corner: floor_edge_b(dir),
            },
        ),
        EdgeAnchor::Wall {
            sx,
            sz,
            dir,
            stack,
            edge: e,
        } => (
            FaceCornerRef::Wall {
                sx,
                sz,
                dir,
                stack,
                corner: wall_edge_a(e),
            },
            FaceCornerRef::Wall {
                sx,
                sz,
                dir,
                stack,
                corner: wall_edge_b(e),
            },
        ),
    };
    Some((
        psxed_ui::face_corner_world(grid, a)?,
        psxed_ui::face_corner_world(grid, b)?,
    ))
}

fn vertex_world_position(grid: &WorldGrid, vertex: psxed_ui::VertexRef) -> Option<[i32; 3]> {
    psxed_ui::face_corner_world(grid, vertex.anchor.as_face_corner())
}

const fn floor_edge_a(dir: GridDirection) -> psxed_ui::Corner {
    match dir {
        GridDirection::North => psxed_ui::Corner::NW,
        GridDirection::East => psxed_ui::Corner::NE,
        GridDirection::South => psxed_ui::Corner::SE,
        GridDirection::West => psxed_ui::Corner::SW,
        GridDirection::NorthWestSouthEast => psxed_ui::Corner::NW,
        GridDirection::NorthEastSouthWest => psxed_ui::Corner::NE,
    }
}

const fn floor_edge_b(dir: GridDirection) -> psxed_ui::Corner {
    match dir {
        GridDirection::North => psxed_ui::Corner::NE,
        GridDirection::East => psxed_ui::Corner::SE,
        GridDirection::South => psxed_ui::Corner::SW,
        GridDirection::West => psxed_ui::Corner::NW,
        GridDirection::NorthWestSouthEast => psxed_ui::Corner::SE,
        GridDirection::NorthEastSouthWest => psxed_ui::Corner::SW,
    }
}

const fn wall_edge_a(edge: psxed_ui::WallEdge) -> psxed_ui::WallCorner {
    match edge {
        psxed_ui::WallEdge::Bottom => psxed_ui::WallCorner::BL,
        psxed_ui::WallEdge::Right => psxed_ui::WallCorner::BR,
        psxed_ui::WallEdge::Top => psxed_ui::WallCorner::TR,
        psxed_ui::WallEdge::Left => psxed_ui::WallCorner::TL,
    }
}

const fn wall_edge_b(edge: psxed_ui::WallEdge) -> psxed_ui::WallCorner {
    match edge {
        psxed_ui::WallEdge::Bottom => psxed_ui::WallCorner::BR,
        psxed_ui::WallEdge::Right => psxed_ui::WallCorner::TR,
        psxed_ui::WallEdge::Top => psxed_ui::WallCorner::TL,
        psxed_ui::WallEdge::Left => psxed_ui::WallCorner::BL,
    }
}

/// Stamp four short, screen-space-thick line segments along the
/// edges of a picked face. Drawing in screen space (after GTE
/// projection) keeps the outline a constant pixel weight regardless
/// of perspective, which matches Godot / Unity's "selection halo"
/// look. Lines pinned to OT slot 0 so they paint on top of every
/// floor / wall / ceiling.
fn push_face_outline(
    grid: &WorldGrid,
    face: psxed_ui::FaceRef,
    style: FaceOutlineStyle,
    scratch: &mut PreviewScratch,
) {
    if face.sx >= grid.width || face.sz >= grid.depth {
        return;
    }
    let sector = grid.sector(face.sx, face.sz);
    let s = grid.sector_size;
    let x0 = grid.cell_world_x(face.sx);
    let x1 = x0 + s;
    let z0 = grid.cell_world_z(face.sz);
    let z1 = z0 + s;
    // Lift a hair off the surface so the outline doesn't z-fight
    // the face it's marking. Sloped floors keep their relative
    // outline position because we lift each corner by the same
    // amount along the local up axis.
    const LIFT: i32 = 4;
    let corners = match face.kind {
        psxed_ui::FaceKind::Floor => sector.and_then(|s| s.floor.as_ref()).map(|f| {
            let h = f.heights;
            [
                [x0, h[0] + LIFT, z1],
                [x1, h[1] + LIFT, z1],
                [x1, h[2] + LIFT, z0],
                [x0, h[3] + LIFT, z0],
            ]
        }),
        psxed_ui::FaceKind::Ceiling => sector.and_then(|s| s.ceiling.as_ref()).map(|c| {
            let h = c.heights;
            [
                [x0, h[0] - LIFT, z1],
                [x1, h[1] - LIFT, z1],
                [x1, h[2] - LIFT, z0],
                [x0, h[3] - LIFT, z0],
            ]
        }),
        psxed_ui::FaceKind::Wall { dir, stack } => {
            let h = sector
                .and_then(|s| s.walls.get(dir).get(stack as usize))
                .map(|wall| wall.heights)
                .unwrap_or_else(|| grid.wall_heights_aligned_to_surfaces(face.sx, face.sz, dir));
            // Inset along the wall's inward normal so the outline
            // sits inside the room rather than z-fighting the
            // wall surface when viewed from inside.
            spatial::editor_wall_outline_corners(
                grid.cell_bounds_world(face.sx, face.sz),
                dir,
                h,
                LIFT,
            )
        }
    };
    let Some(corners) = corners else { return };
    let projected: [psx_gte::scene::Projected; 4] = [
        gte_scene::project_vertex(world_to_view(corners[0])),
        gte_scene::project_vertex(world_to_view(corners[1])),
        gte_scene::project_vertex(world_to_view(corners[2])),
        gte_scene::project_vertex(world_to_view(corners[3])),
    ];
    // Skip outlines whose corners didn't project -- `project_vertex`
    // returns `sz == 0` for behind-camera or near-plane-clipped
    // points, which would produce nonsense screen lines.
    if projected.iter().any(|p| p.sz == 0) {
        return;
    }
    for i in 0..4 {
        let a = projected[i];
        let b = projected[(i + 1) % 4];
        push_screen_line(scratch, a, b, style);
    }
    let split = match face.kind {
        psxed_ui::FaceKind::Floor => sector.and_then(|s| s.floor.as_ref()).map(|face| face.split),
        psxed_ui::FaceKind::Ceiling => sector
            .and_then(|s| s.ceiling.as_ref())
            .map(|face| face.split),
        psxed_ui::FaceKind::Wall { .. } => None,
    };
    if let Some(split) = split {
        let (a, b) = match split {
            GridSplit::NorthWestSouthEast => (projected[0], projected[2]),
            GridSplit::NorthEastSouthWest => (projected[1], projected[3]),
        };
        push_screen_line(scratch, a, b, style);
    }
}

/// Queue one host-drawn overlay segment between two screen-projected
/// vertices. Unlike the scene command log, this is painted by egui on
/// top of the preview texture, so fractional widths and normal UI
/// compositing work as expected.
fn push_screen_line(
    scratch: &mut PreviewScratch,
    a: psx_gte::scene::Projected,
    b: psx_gte::scene::Projected,
    style: FaceOutlineStyle,
) {
    push_overlay_line(
        scratch,
        egui::pos2(a.sx as f32, a.sy as f32),
        egui::pos2(b.sx as f32, b.sy as f32),
        style,
    );
}

fn push_overlay_line(
    scratch: &mut PreviewScratch,
    a: egui::Pos2,
    b: egui::Pos2,
    style: FaceOutlineStyle,
) {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 0.5 {
        return;
    }
    scratch
        .overlay_lines
        .push(psxed_ui::EditorViewportOverlayLine::new(
            a,
            b,
            bright_overlay_color(style.rgb),
            style.thickness_px,
        ));
}

fn bright_overlay_color(rgb: (u8, u8, u8)) -> egui::Color32 {
    let lift = |channel: u8| -> u8 {
        let c = channel as u16;
        (c + ((255 - c) * 3 / 5)).min(255) as u8
    };
    egui::Color32::from_rgb(lift(rgb.0), lift(rgb.1), lift(rgb.2))
}

/// Stamp a GP0(02h) fill-rectangle into `scratch.clear_packet` and
/// link it into the back-most OT slot so it runs first when DMA
/// walks the chain -- which is the same pattern PS1 software uses to
/// "clear" the framebuffer at the start of every frame, since the
/// HwRenderer (faithfully) preserves VRAM across frames the way real
/// hardware does.
fn push_clear(scratch: &mut PreviewScratch) {
    // PSX VRAM coords; the editor's HwRenderer renders the same
    // 320×240 sub-rect that the runtime frame-buffer would land in.
    let color_word = 0x0200_0000_u32; // opcode 0x02, RGB = 0 (black)
    let xy_word = 0u32; // top-left at (0, 0)
    let wh_word = ((240u32) << 16) | 320u32; // pack_xy(320, 240)
                                             // word[0] is rewritten by `OrderingTable::insert` with the
                                             // chain tag -- leave it at 0 here.
    scratch.clear_packet[1] = color_word;
    scratch.clear_packet[2] = xy_word;
    scratch.clear_packet[3] = wh_word;
    let ptr: *mut u32 = scratch.clear_packet.as_mut_ptr();
    unsafe {
        scratch.ot.insert(OT_DEPTH - 1, ptr, 3);
    }
}

/// Per-face emit: routes to the flat or textured pool based on
/// `shade`, packing UVs only when textured.
fn emit_face_tri(
    scratch: &mut PreviewScratch,
    p: [psx_gte::scene::Projected; 3],
    uvs: [(u8, u8); 3],
    shade: FaceShade,
) -> bool {
    if !face_side_visible(shade.sidedness(), p) {
        return false;
    }
    match shade {
        FaceShade::Flat { rgb, .. } => push_tri(scratch, p, rgb),
        FaceShade::Textured { slot, tint, .. } => push_tex_tri(scratch, p, uvs, slot, tint),
    }
}

fn face_side_visible(
    sidedness: psxed_project::MaterialFaceSidedness,
    p: [psx_gte::scene::Projected; 3],
) -> bool {
    let area = projected_area(p);
    match sidedness {
        psxed_project::MaterialFaceSidedness::Front => area > 0,
        psxed_project::MaterialFaceSidedness::Back => area < 0,
        psxed_project::MaterialFaceSidedness::Both => area != 0,
    }
}

fn projected_area(p: [psx_gte::scene::Projected; 3]) -> i32 {
    let ax = (p[1].sx as i32) - (p[0].sx as i32);
    let ay = (p[1].sy as i32) - (p[0].sy as i32);
    let bx = (p[2].sx as i32) - (p[0].sx as i32);
    let by = (p[2].sy as i32) - (p[0].sy as i32);
    ax * by - ay * bx
}

/// Compose a [`TriTextured`] sampling `slot`'s tpage / CLUT, stash
/// it in the static `tex_tris` arena, and chain it into the OT.
///
/// `tint` modulates the texel: PSX hardware computes
/// `output = texel * tint / 0x80`, so `(0x80, 0x80, 0x80)` is a
/// pass-through and `(0xFF, 0x60, 0x40)` saturates a grey texel
/// toward terracotta. Textured preview uses the authored material
/// tint so it matches the cooked runtime path; flat fallback still
/// uses material-name colours to keep untextured faces readable.
fn push_tex_tri(
    scratch: &mut PreviewScratch,
    p: [psx_gte::scene::Projected; 3],
    uvs: [(u8, u8); 3],
    slot: MaterialSlot,
    tint: (u8, u8, u8),
) -> bool {
    let avg_sz = projected_avg_sz(p);
    push_textured_material_tri(
        scratch,
        p,
        uvs,
        TextureMaterial::opaque(slot.clut_word, slot.tpage_word, tint)
            .with_texture_window(slot.texture_window),
        room_depth_slot(avg_sz),
    )
}

fn push_shadow_tex_tri(
    scratch: &mut PreviewScratch,
    p: [psx_gte::scene::Projected; 3],
    uvs: [(u8, u8); 3],
    slot: MaterialSlot,
) -> bool {
    push_textured_material_tri(
        scratch,
        p,
        uvs,
        TextureMaterial::blended(
            slot.clut_word,
            slot.tpage_word,
            (0x80, 0x80, 0x80),
            BlendMode::Average,
        )
        .with_raw_texture(true),
        shadow_depth_slot(projected_avg_sz(p)),
    )
}

fn push_textured_material_tri(
    scratch: &mut PreviewScratch,
    p: [psx_gte::scene::Projected; 3],
    uvs: [(u8, u8); 3],
    material: TextureMaterial,
    slot_idx: usize,
) -> bool {
    if scratch.tex_used >= TRI_CAP {
        return false;
    }
    let idx = scratch.tex_used;
    scratch.tex_tris[idx] = TriTextured::with_material(
        [(p[0].sx, p[0].sy), (p[1].sx, p[1].sy), (p[2].sx, p[2].sy)],
        uvs,
        material,
    );
    let packet_ptr: *mut TriTextured = &mut scratch.tex_tris[idx];
    unsafe {
        scratch
            .ot
            .insert(slot_idx, packet_ptr.cast::<u32>(), TriTextured::WORDS);
    }
    scratch.tex_used = idx + 1;
    true
}

/// Compose a [`TriFlat`] from three projected vertices, store it in
/// the next slot of the static `tris` array, and link it into the
/// OT keyed on average screen-space depth.
fn push_tri(
    scratch: &mut PreviewScratch,
    p: [psx_gte::scene::Projected; 3],
    rgb: (u8, u8, u8),
) -> bool {
    if scratch.used >= TRI_CAP {
        return false;
    }
    let idx = scratch.used;
    scratch.tris[idx] = TriFlat::new(
        [(p[0].sx, p[0].sy), (p[1].sx, p[1].sy), (p[2].sx, p[2].sy)],
        rgb.0,
        rgb.1,
        rgb.2,
    );
    scratch.used = idx + 1;
    // Map sz (Q0, range up to ~32K for our scenes) into the OT
    // depth band. Smaller sz = closer = drawn last, so map to a
    // lower OT slot index. We reserve slot OT_DEPTH-1 for the
    // per-frame fill-rect clear and slot 0 for the hover overlay
    // (drawn last so it tops everything), so geometry rides the
    // 1..OT_DEPTH-1 band exclusively.
    let slot = room_depth_slot(projected_avg_sz(p));
    let packet_ptr: *mut TriFlat = &mut scratch.tris[idx];
    unsafe {
        scratch
            .ot
            .insert(slot, packet_ptr.cast::<u32>(), TriFlat::WORDS);
    }
    true
}

fn projected_avg_sz(p: [psx_gte::scene::Projected; 3]) -> u32 {
    (p[0].sz as u32 + p[1].sz as u32 + p[2].sz as u32) / 3
}

fn room_depth_slot(avg_sz: u32) -> usize {
    preview_geometry_depth_slot(avg_sz)
}

fn shadow_depth_slot(avg_sz: u32) -> usize {
    preview_geometry_depth_slot(avg_sz.saturating_sub(PREVIEW_SHADOW_DEPTH_BIAS))
}

fn preview_geometry_depth_slot(avg_sz: u32) -> usize {
    ((avg_sz as usize) >> 6).clamp(PREVIEW_GEOMETRY_SLOT_MIN, PREVIEW_GEOMETRY_SLOT_MAX)
}

#[cfg(test)]
mod tests {
    use super::{
        face_side_visible, floor_anchored_model_origin, light_face, material_sized_uvs,
        material_texture_tint, node_room_local_origin, preview_lights, preview_model_reference,
        preview_player_reference, preview_shadow_radius, preview_static_model_reference,
        preview_vertices_in_front, push_wall_face, room_depth_slot, setup_gte_for_camera,
        shadow_depth_slot, should_draw_culled_face_outline, FaceShade, MaterialSlot, PreviewFog,
        WallEdge, GRID_TILE_UV, PREVIEW_FLOOR_UVS, PREVIEW_GEOMETRY_SLOT_MAX,
        PREVIEW_GEOMETRY_SLOT_MIN, PREVIEW_SHADOW_DEPTH_BIAS, PREVIEW_SHADOW_RADIUS_MAX,
        PREVIEW_SHADOW_RADIUS_MIN, PREVIEW_WALL_UVS, SCRATCH,
    };
    use psx_engine::{PointLightSample, WorldVertex};
    use psx_gte::scene::Projected;
    use psxed_project::{
        GridUvTransform, MaterialFaceSidedness, MaterialResource, NodeKind, ProjectDocument,
        ResourceData, WorldGrid,
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

    fn fog(rgb: (u8, u8, u8), near: i32, far: i32) -> PreviewFog {
        PreviewFog {
            enabled: true,
            rgb,
            near,
            far,
        }
    }

    fn projected(sx: i16, sy: i16) -> Projected {
        Projected { sx, sy, sz: 100 }
    }

    #[test]
    fn textured_preview_uses_authored_material_tint() {
        let mut project = ProjectDocument::new("test");
        let texture = project.add_resource(
            "Brick Texture",
            ResourceData::Texture {
                psxt_path: "brick.psxt".to_string(),
            },
        );
        let mut material = MaterialResource::opaque(Some(texture));
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
    fn component_model_reference_reads_renderer_and_animator_children() {
        let mut project = ProjectDocument::new("test");
        let model_id = project.add_resource(
            "Dummy",
            ResourceData::Texture {
                psxt_path: "dummy.psxt".to_string(),
            },
        );
        let scene = project.active_scene_mut();
        let actor = scene.add_node(scene.root, "Enemy", NodeKind::Entity);
        let renderer = scene.add_node(
            actor,
            "Model Renderer",
            NodeKind::ModelRenderer {
                model: Some(model_id),
                material: None,
            },
        );
        let animator = scene.add_node(
            actor,
            "Animator",
            NodeKind::Animator {
                clip: Some(3),
                autoplay: true,
            },
        );

        let scene = project.active_scene();
        let reference = preview_model_reference(scene, scene.node(actor).unwrap()).unwrap();

        assert_eq!(reference.model_id, model_id);
        assert_eq!(reference.clip_override, Some(3));
        assert_eq!(reference.renderer_node, Some(renderer));
        assert_eq!(reference.animator_node, Some(animator));
    }

    #[test]
    fn component_player_reference_reads_controller_child() {
        let mut project = ProjectDocument::new("test");
        let character_id = project.add_resource(
            "Dummy",
            ResourceData::Texture {
                psxt_path: "dummy.psxt".to_string(),
            },
        );
        let scene = project.active_scene_mut();
        let actor = scene.add_node(scene.root, "Player", NodeKind::Entity);
        let controller = scene.add_node(
            actor,
            "Character Controller",
            NodeKind::CharacterController {
                character: Some(character_id),
                player: true,
            },
        );

        let scene = project.active_scene();
        let reference = preview_player_reference(scene, scene.node(actor).unwrap()).unwrap();

        assert_eq!(reference.character, Some(character_id));
        assert_eq!(reference.controller_node, Some(controller));
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
            },
        );
        scene.add_node(
            actor,
            "Character Controller",
            NodeKind::CharacterController {
                character: Some(character_id),
                player: true,
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
    fn editor_cardinal_wall_front_material_renders_from_owning_cell() {
        let cases = [
            (WallEdge::North, [512, 512, 512], 128, [512, 512, 1536], 0),
            (WallEdge::East, [512, 512, 512], 192, [1536, 512, 512], 64),
            (WallEdge::South, [512, 512, 512], 0, [512, 512, -512], 128),
            (WallEdge::West, [512, 512, 512], 64, [-512, 512, 512], 192),
        ];

        for (edge, inside_pos, inside_yaw, outside_pos, outside_yaw) in cases {
            assert!(
                wall_face_emits_from_camera(
                    edge,
                    inside_pos,
                    inside_yaw,
                    MaterialFaceSidedness::Front
                ),
                "{edge:?} wall front material should render from inside the owning cell"
            );
            assert!(
                !wall_face_emits_from_camera(
                    edge,
                    inside_pos,
                    inside_yaw,
                    MaterialFaceSidedness::Back
                ),
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
        let mut scratch = SCRATCH.lock().expect("editor preview scratch mutex");
        scratch.used = 0;
        scratch.tex_used = 0;
        scratch.overlay_lines.clear();
        scratch.ot.clear();

        push_wall_face(
            &mut scratch,
            camera,
            [0, 1024, 0, 1024],
            edge,
            [0, 0, 1024, 1024],
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

    #[test]
    fn floor_anchored_model_origin_offsets_by_half_world_height() {
        let origin = floor_anchored_model_origin(WorldVertex::new(10, 0, 20), 1024);
        assert_eq!(origin, WorldVertex::new(10, 512, 20));
    }

    #[test]
    fn floor_anchored_model_origin_ignores_negative_height() {
        let origin = floor_anchored_model_origin(WorldVertex::new(10, 32, 20), -128);
        assert_eq!(origin, WorldVertex::new(10, 32, 20));
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
}
