use super::*;
use psx_engine::{CullMode, WorldRenderLayer};
use psx_gpu::prim::{LineMono, TriGouraud};
use psx_gte::lighting::ProjectedLit;

const TARGET_LOCK_OUTER: i32 = 25;
const TARGET_LOCK_INNER: i32 = 13;
const TARGET_LOCK_TRI_HALF_WIDTH: i32 = 8;
const TARGET_LOCK_RED: (u8, u8, u8) = (225, 18, 24);
const TARGET_LOCK_ROTATION_FRAMES: u32 = 360;

/// Marker visualization tuning. Markers are debug stubs -- keep
/// them visible at orbit-camera scales without dominating the
/// scene.
const MARKER_HALF: i32 = 6;
const MARKER_LIFT: i32 = MARKER_HALF;
const MARKER_TINT: (u8, u8, u8) = (0xff, 0xa8, 0x40);

const ARCHIVE_BEACON_MIN_HEIGHT: i32 = 6;
const ARCHIVE_BEACON_MAX_HEIGHT: i32 = 32;
const ARCHIVE_BEACON_ACTIVE_ROTATION_FRAMES: u32 = 180;
const ARCHIVE_BEACON_INTERACTABLE_ROTATION_FRAMES: u32 = 120;
const ARCHIVE_BEACON_FACE_VERTICES: usize = 6;
const ARCHIVE_BEACON_FACE_TRIANGLES: [[usize; 3]; 4] = [[0, 1, 2], [0, 2, 3], [0, 3, 4], [0, 4, 5]];

/// Rendering-only state for the Archive Beacon prop. Gameplay persistence,
/// reward state, and range selection stay outside the marker renderer.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum ArchiveBeaconVisualState {
    /// Available but outside interaction range: slow rotation and ember pulse.
    Active,
    /// Nearest in-range POI: faster rotation and a brighter pulse.
    Interactable,
    /// Non-repeatable/depleted POI: dim and stationary, as requested.
    Depleted,
}

impl Playtest {
    /// Submit every visible Archive Beacon in the current room into the same
    /// world ordering table as BSP/grid surfaces and actors. Unlike the former
    /// overlay marker, walls and props can now correctly occlude the beacon.
    pub(super) fn draw_archive_beacons_world<T>(
        &self,
        camera: WorldCamera,
        elapsed_tick: SimTick,
        world_object_visibility: WorldObjectVisibility,
        packets: &mut T,
        world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
    ) -> usize
    where
        T: PrimitiveSink<TriGouraud> + PrimitiveSink<LineMono>,
    {
        let Some(room_record) = ROOMS.get(self.room_index.to_usize()) else {
            return 0;
        };
        let options = if self.bsp.is_some() {
            pxbsp_surface_options(room_record)
        } else {
            room_surface_options(room_record)
        };
        let open_poi = self
            .poi_messages
            .active()
            .and_then(|message| match message.source() {
                psx_game_runtime::poi::MessageSource::PointOfInterest(index) => {
                    Some(usize::from(index))
                }
                psx_game_runtime::poi::MessageSource::World => None,
            });
        let mut submitted = 0usize;
        for (index, interactable) in INTERACTABLES.iter().enumerate() {
            if interactable.kind != InteractableKind::PointOfInterest
                || interactable.room != self.room_index
                || !interactable_is_active(interactable)
                || open_poi == Some(index)
                || !world_object_visibility.typed_visible(
                    WORLD_OBJECTS,
                    psx_level::world_object_kind::POINT_OF_INTEREST_BEACON,
                    index,
                )
            {
                continue;
            }
            let state = if self.point_of_interest_depleted(interactable) {
                ArchiveBeaconVisualState::Depleted
            } else if self.active_interactable == Some(index) {
                ArchiveBeaconVisualState::Interactable
            } else {
                ArchiveBeaconVisualState::Active
            };
            submitted += draw_archive_beacon_world(
                RoomPoint::new(interactable.x, interactable.y, interactable.z),
                Angle::from_q12(interactable.yaw as u16),
                interactable.marker_height,
                state,
                elapsed_tick,
                camera,
                options,
                packets,
                world,
            );
        }
        submitted
    }
}

/// Draw one compact, PS1-native Archive Beacon as real world geometry.
///
/// The authored floor point owns a short central spindle. Above it sits a
/// shallow square extrusion with the same opposing TL/BR cuts as the Archive
/// message panel. The whole shell is ember red, including its edge-on faces.
/// Only the nearer face receives a perimeter so rear line packets cannot sort
/// across the front face while the prop rotates on PS1's painter renderer.
pub(super) fn draw_archive_beacon_world<T>(
    position: RoomPoint,
    yaw: Angle,
    cooked_scale: u16,
    state: ArchiveBeaconVisualState,
    elapsed_tick: SimTick,
    camera: WorldCamera,
    options: WorldSurfaceOptions,
    packets: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> usize
where
    T: PrimitiveSink<TriGouraud> + PrimitiveSink<LineMono>,
{
    let rotation = match state {
        ArchiveBeaconVisualState::Active => {
            Angle::per_frames(ARCHIVE_BEACON_ACTIVE_ROTATION_FRAMES).mul_tick(elapsed_tick)
        }
        ArchiveBeaconVisualState::Interactable => {
            Angle::per_frames(ARCHIVE_BEACON_INTERACTABLE_ROTATION_FRAMES).mul_tick(elapsed_tick)
        }
        ArchiveBeaconVisualState::Depleted => Angle::ZERO,
    };
    let panel_yaw = yaw.add(rotation);
    let pulse = archive_beacon_pulse(rotation, state);
    let height = archive_beacon_world_height(cooked_scale);
    let half = (height / 2).max(3);
    let cut = (height / 4).clamp(2, 8);
    let half_depth = (height / 12).clamp(1, 3);
    let pivot_height = (height / 6).clamp(1, 4);
    let pivot_half = (height / 10).clamp(1, 3);
    let center_y = -pivot_height - half;

    let outer_local = archive_beacon_face_points(half, cut);
    let outer_front_world =
        archive_beacon_face_world_points(position, panel_yaw, center_y, -half_depth, outer_local);
    let outer_back_world =
        archive_beacon_face_world_points(position, panel_yaw, center_y, half_depth, outer_local);

    let Some(outer_front) = project_archive_beacon_points(camera, outer_front_world) else {
        return 0;
    };
    let Some(outer_back) = project_archive_beacon_points(camera, outer_back_world) else {
        return 0;
    };
    if archive_beacon_offscreen(&outer_front) && archive_beacon_offscreen(&outer_back) {
        return 0;
    }

    let body_options = options
        .with_cull_mode(CullMode::None)
        .with_render_layer(WorldRenderLayer::Opaque);
    let line_options = body_options.with_depth_bias(-2);
    let face_colors = archive_beacon_face_colors(state, pulse, outer_local, half);
    let frame_color = archive_beacon_frame_color(state, pulse);
    let visible_face =
        if archive_beacon_face_depth(&outer_front) <= archive_beacon_face_depth(&outer_back) {
            outer_front
        } else {
            outer_back
        };
    let mut submitted = 0usize;

    submitted +=
        submit_archive_beacon_face(visible_face, face_colors, body_options, packets, world);
    submitted += submit_archive_beacon_sides(
        outer_front,
        outer_back,
        state,
        pulse,
        body_options,
        packets,
        world,
    );
    submitted +=
        submit_archive_beacon_perimeter(visible_face, frame_color, line_options, packets, world);
    submitted += submit_archive_beacon_pivot(
        position,
        panel_yaw,
        pivot_height,
        pivot_half,
        camera,
        body_options,
        packets,
        world,
    );
    submitted
}

/// Cooking maps the default authored scale of 192 to 12 engine units. Keep
/// that value literal in world space: it produces the approved shin-height
/// square rather than inflating it into the former 24-pixel overlay glyph.
pub(super) const fn archive_beacon_world_height(cooked_scale: u16) -> i32 {
    let scaled = cooked_scale as i32;
    if scaled < ARCHIVE_BEACON_MIN_HEIGHT {
        ARCHIVE_BEACON_MIN_HEIGHT
    } else if scaled > ARCHIVE_BEACON_MAX_HEIGHT {
        ARCHIVE_BEACON_MAX_HEIGHT
    } else {
        scaled
    }
}

const _: () = assert!(archive_beacon_world_height(12) == 12);

const fn archive_beacon_face_points(half: i32, cut: i32) -> [(i32, i32); 6] {
    [
        (-half + cut, -half),
        (half, -half),
        (half, half - cut),
        (half - cut, half),
        (-half, half),
        (-half, -half + cut),
    ]
}

fn archive_beacon_face_world_points(
    position: RoomPoint,
    yaw: Angle,
    center_y: i32,
    local_z: i32,
    local: [(i32, i32); ARCHIVE_BEACON_FACE_VERTICES],
) -> [WorldVertex; ARCHIVE_BEACON_FACE_VERTICES] {
    let mut out = [WorldVertex::new(0, 0, 0); ARCHIVE_BEACON_FACE_VERTICES];
    let mut index = 0usize;
    while index < ARCHIVE_BEACON_FACE_VERTICES {
        out[index] = archive_beacon_world_point(
            position,
            yaw,
            local[index].0,
            center_y.saturating_add(local[index].1),
            local_z,
        );
        index += 1;
    }
    out
}

fn archive_beacon_world_point(
    position: RoomPoint,
    yaw: Angle,
    local_x: i32,
    local_y: i32,
    local_z: i32,
) -> WorldVertex {
    let sin = yaw.sin_q12();
    let cos = yaw.cos_q12();
    let world_x = position.x.saturating_add(
        (local_x
            .saturating_mul(cos)
            .saturating_add(local_z.saturating_mul(sin)))
            >> 12,
    );
    let world_z = position.z.saturating_add(
        (local_z
            .saturating_mul(cos)
            .saturating_sub(local_x.saturating_mul(sin)))
            >> 12,
    );
    WorldVertex::new(world_x, position.y.saturating_add(local_y), world_z)
}

fn project_archive_beacon_points<const N: usize>(
    camera: WorldCamera,
    points: [WorldVertex; N],
) -> Option<[ProjectedVertex; N]> {
    let mut out = [ProjectedVertex::INVALID; N];
    let mut index = 0usize;
    while index < N {
        out[index] = camera.project_world(points[index])?;
        index += 1;
    }
    Some(out)
}

fn archive_beacon_offscreen(points: &[ProjectedVertex; ARCHIVE_BEACON_FACE_VERTICES]) -> bool {
    points.iter().all(|point| point.sx < -16)
        || points.iter().all(|point| point.sx > 336)
        || points.iter().all(|point| point.sy < -16)
        || points.iter().all(|point| point.sy > 256)
}

fn archive_beacon_face_depth(points: &[ProjectedVertex; ARCHIVE_BEACON_FACE_VERTICES]) -> i32 {
    let mut sum = 0i32;
    let mut index = 0usize;
    while index < points.len() {
        sum = sum.saturating_add(points[index].sz);
        index += 1;
    }
    sum / ARCHIVE_BEACON_FACE_VERTICES as i32
}

fn archive_beacon_pulse(rotation: Angle, state: ArchiveBeaconVisualState) -> u8 {
    if matches!(state, ArchiveBeaconVisualState::Depleted) {
        return 0;
    }
    (((rotation.sin_q12().saturating_add(4096)) * 255) / 8192).clamp(0, 255) as u8
}

fn archive_beacon_face_colors(
    state: ArchiveBeaconVisualState,
    pulse: u8,
    local: [(i32, i32); ARCHIVE_BEACON_FACE_VERTICES],
    half: i32,
) -> [(u8, u8, u8); ARCHIVE_BEACON_FACE_VERTICES] {
    let mut colors = [(0u8, 0u8, 0u8); ARCHIVE_BEACON_FACE_VERTICES];
    let band_x = (i32::from(pulse).saturating_sub(128)).saturating_mul(half) / 128;
    let (base, strength) = match state {
        ArchiveBeaconVisualState::Active => (34i32, 52i32),
        ArchiveBeaconVisualState::Interactable => (58, 116),
        ArchiveBeaconVisualState::Depleted => (15, 0),
    };
    let span = half.saturating_mul(2).max(1);
    let mut index = 0usize;
    while index < ARCHIVE_BEACON_FACE_VERTICES {
        let distance = local[index].0.saturating_sub(band_x).abs().min(span);
        let glow = strength.saturating_mul(span.saturating_sub(distance)) / span;
        let red = base.saturating_add(glow).clamp(0, 255) as u8;
        colors[index] = (red, (red / 10).max(3), (red / 14).max(3));
        index += 1;
    }
    colors
}

fn archive_beacon_frame_color(state: ArchiveBeaconVisualState, pulse: u8) -> (u8, u8, u8) {
    match state {
        ArchiveBeaconVisualState::Active => (
            132u8.saturating_add(pulse / 4),
            15u8.saturating_add(pulse / 24),
            10,
        ),
        ArchiveBeaconVisualState::Interactable => (238, 38, 22),
        ArchiveBeaconVisualState::Depleted => (52, 11, 8),
    }
}

fn projected_lit(projected: ProjectedVertex, color: (u8, u8, u8)) -> ProjectedLit {
    ProjectedLit {
        sx: projected.sx,
        sy: projected.sy,
        sz: projected.sz.clamp(1, u16::MAX as i32) as u16,
        r: color.0,
        g: color.1,
        b: color.2,
    }
}

fn submit_archive_beacon_face<T>(
    projected: [ProjectedVertex; ARCHIVE_BEACON_FACE_VERTICES],
    colors: [(u8, u8, u8); ARCHIVE_BEACON_FACE_VERTICES],
    options: WorldSurfaceOptions,
    packets: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> usize
where
    T: PrimitiveSink<TriGouraud>,
{
    let mut submitted = 0u16;
    let mut index = 0usize;
    while index < ARCHIVE_BEACON_FACE_TRIANGLES.len() {
        let face = ARCHIVE_BEACON_FACE_TRIANGLES[index];
        submitted += world
            .submit_gouraud_triangle(
                packets,
                [
                    projected_lit(projected[face[0]], colors[face[0]]),
                    projected_lit(projected[face[1]], colors[face[1]]),
                    projected_lit(projected[face[2]], colors[face[2]]),
                ],
                options,
            )
            .submitted_triangles;
        index += 1;
    }
    usize::from(submitted)
}

fn submit_archive_beacon_sides<T>(
    front: [ProjectedVertex; ARCHIVE_BEACON_FACE_VERTICES],
    back: [ProjectedVertex; ARCHIVE_BEACON_FACE_VERTICES],
    state: ArchiveBeaconVisualState,
    pulse: u8,
    options: WorldSurfaceOptions,
    packets: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> usize
where
    T: PrimitiveSink<TriGouraud>,
{
    let mut submitted = 0u16;
    let mut edge = 0usize;
    while edge < ARCHIVE_BEACON_FACE_VERTICES {
        let next = (edge + 1) % ARCHIVE_BEACON_FACE_VERTICES;
        let color = archive_beacon_side_color(state, pulse, edge);
        submitted += world
            .submit_gouraud_triangle(
                packets,
                [
                    projected_lit(front[edge], color),
                    projected_lit(front[next], color),
                    projected_lit(back[next], color),
                ],
                options,
            )
            .submitted_triangles;
        submitted += world
            .submit_gouraud_triangle(
                packets,
                [
                    projected_lit(front[edge], color),
                    projected_lit(back[next], color),
                    projected_lit(back[edge], color),
                ],
                options,
            )
            .submitted_triangles;
        edge += 1;
    }
    usize::from(submitted)
}

fn archive_beacon_side_color(
    state: ArchiveBeaconVisualState,
    pulse: u8,
    edge: usize,
) -> (u8, u8, u8) {
    let base = match state {
        ArchiveBeaconVisualState::Active => 38u8.saturating_add(pulse / 12),
        ArchiveBeaconVisualState::Interactable => 72u8.saturating_add(pulse / 8),
        ArchiveBeaconVisualState::Depleted => 18,
    };
    let red = if edge < 2 {
        base.saturating_add(18)
    } else if edge < 4 {
        base
    } else {
        base.saturating_sub(10)
    };
    (red, (red / 11).max(3), (red / 16).max(2))
}

fn submit_archive_beacon_perimeter<T>(
    projected: [ProjectedVertex; ARCHIVE_BEACON_FACE_VERTICES],
    color: (u8, u8, u8),
    options: WorldSurfaceOptions,
    packets: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> usize
where
    T: PrimitiveSink<LineMono>,
{
    let mut submitted = 0u16;
    let mut edge = 0usize;
    while edge < ARCHIVE_BEACON_FACE_VERTICES {
        let next = (edge + 1) % ARCHIVE_BEACON_FACE_VERTICES;
        submitted += world
            .submit_projected_line(packets, [projected[edge], projected[next]], color, options)
            .submitted_triangles;
        edge += 1;
    }
    usize::from(submitted)
}

#[allow(clippy::too_many_arguments)]
fn submit_archive_beacon_pivot<T>(
    position: RoomPoint,
    yaw: Angle,
    height: i32,
    half: i32,
    camera: WorldCamera,
    options: WorldSurfaceOptions,
    packets: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> usize
where
    T: PrimitiveSink<TriGouraud>,
{
    let local = [
        (-half, -height, -half),
        (half, -height, -half),
        (half, -height, half),
        (-half, -height, half),
        (-half, 0, -half),
        (half, 0, -half),
        (half, 0, half),
        (-half, 0, half),
    ];
    let mut projected = [ProjectedVertex::INVALID; 8];
    let mut index = 0usize;
    while index < local.len() {
        let world_point = archive_beacon_world_point(
            position,
            yaw,
            local[index].0,
            local[index].1,
            local[index].2,
        );
        let Some(point) = camera.project_world(world_point) else {
            return 0;
        };
        projected[index] = point;
        index += 1;
    }
    const SIDES: [[usize; 4]; 4] = [[0, 1, 5, 4], [1, 2, 6, 5], [2, 3, 7, 6], [3, 0, 4, 7]];
    let colors = [(25, 16, 14), (14, 10, 9), (8, 7, 7), (18, 12, 10)];
    let mut submitted = 0u16;
    let mut side = 0usize;
    while side < SIDES.len() {
        let quad = SIDES[side];
        let color = colors[side];
        submitted += world
            .submit_gouraud_triangle(
                packets,
                [
                    projected_lit(projected[quad[0]], color),
                    projected_lit(projected[quad[1]], color),
                    projected_lit(projected[quad[2]], color),
                ],
                options,
            )
            .submitted_triangles;
        submitted += world
            .submit_gouraud_triangle(
                packets,
                [
                    projected_lit(projected[quad[0]], color),
                    projected_lit(projected[quad[2]], color),
                    projected_lit(projected[quad[3]], color),
                ],
                options,
            )
            .submitted_triangles;
        side += 1;
    }
    usize::from(submitted)
}

/// Draw one tinted cube per generated entity record. Cubes
/// reuse the room's first material with an override tint so
/// markers stand out from the surrounding geometry without
/// needing a dedicated texture upload.
pub(super) fn draw_entity_markers(
    entities: &[EntityRecord],
    current_room: RoomIndex,
    materials: &[WorldRenderMaterial],
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    if entities.is_empty() || materials.is_empty() {
        return;
    }
    // Reuse the room's first material so we don't need a
    // dedicated marker texture. Tint override picks up the
    // existing CLUT + tpage but recolours.
    let material = materials[0].texture.with_tint(MARKER_TINT);
    let opts = options.with_material_layer(material);
    const UVS: [(u8, u8); 4] = [(0, 0), (64, 0), (64, 64), (0, 64)];

    for entity in entities {
        if entity.room != current_room {
            continue;
        }
        let cx = entity.x;
        let cy = entity.y - MARKER_LIFT - MARKER_HALF;
        let cz = entity.z;
        let h = MARKER_HALF;

        let top = [
            WorldVertex::new(cx - h, cy - h, cz - h),
            WorldVertex::new(cx + h, cy - h, cz - h),
            WorldVertex::new(cx + h, cy - h, cz + h),
            WorldVertex::new(cx - h, cy - h, cz + h),
        ];
        let bottom = [
            WorldVertex::new(cx - h, cy + h, cz + h),
            WorldVertex::new(cx + h, cy + h, cz + h),
            WorldVertex::new(cx + h, cy + h, cz - h),
            WorldVertex::new(cx - h, cy + h, cz - h),
        ];
        let north = [
            WorldVertex::new(cx - h, cy - h, cz - h),
            WorldVertex::new(cx + h, cy - h, cz - h),
            WorldVertex::new(cx + h, cy + h, cz - h),
            WorldVertex::new(cx - h, cy + h, cz - h),
        ];
        let south = [
            WorldVertex::new(cx + h, cy - h, cz + h),
            WorldVertex::new(cx - h, cy - h, cz + h),
            WorldVertex::new(cx - h, cy + h, cz + h),
            WorldVertex::new(cx + h, cy + h, cz + h),
        ];
        let east = [
            WorldVertex::new(cx + h, cy - h, cz - h),
            WorldVertex::new(cx + h, cy - h, cz + h),
            WorldVertex::new(cx + h, cy + h, cz + h),
            WorldVertex::new(cx + h, cy + h, cz - h),
        ];
        let west = [
            WorldVertex::new(cx - h, cy - h, cz + h),
            WorldVertex::new(cx - h, cy - h, cz - h),
            WorldVertex::new(cx - h, cy + h, cz - h),
            WorldVertex::new(cx - h, cy + h, cz + h),
        ];

        for face in [top, bottom, north, south, east, west] {
            if let Some(projected) = camera.project_world_quad(face) {
                let _ = world.submit_textured_quad(triangles, projected, UVS, material, opts);
            }
        }
    }
}

pub(super) fn draw_lock_target_indicator(
    target: RoomPoint,
    camera: WorldCamera,
    elapsed_tick: SimTick,
) {
    let Some(center) = camera.project_world(target) else {
        return;
    };

    let outer = TARGET_LOCK_OUTER;
    let inner = TARGET_LOCK_INNER;
    let half_width = TARGET_LOCK_TRI_HALF_WIDTH;
    let angle = Angle::per_frames(TARGET_LOCK_ROTATION_FRAMES).mul_tick(elapsed_tick);
    let triangles = [
        [
            target_screen_vertex(center, 0, -inner, angle),
            target_screen_vertex(center, -half_width, -outer, angle),
            target_screen_vertex(center, half_width, -outer, angle),
        ],
        [
            target_screen_vertex(center, 0, inner, angle),
            target_screen_vertex(center, half_width, outer, angle),
            target_screen_vertex(center, -half_width, outer, angle),
        ],
        [
            target_screen_vertex(center, -inner, 0, angle),
            target_screen_vertex(center, -outer, half_width, angle),
            target_screen_vertex(center, -outer, -half_width, angle),
        ],
        [
            target_screen_vertex(center, inner, 0, angle),
            target_screen_vertex(center, outer, -half_width, angle),
            target_screen_vertex(center, outer, half_width, angle),
        ],
    ];

    for triangle in triangles {
        draw_tri_flat_blended(
            triangle,
            TARGET_LOCK_RED.0,
            TARGET_LOCK_RED.1,
            TARGET_LOCK_RED.2,
            BlendMode::Average,
        );
    }
}

fn target_screen_vertex(center: ProjectedVertex, ox: i32, oy: i32, angle: Angle) -> (i16, i16) {
    let sin = angle.sin_q12();
    let cos = angle.cos_q12();
    let rx = ((ox.saturating_mul(cos)).saturating_sub(oy.saturating_mul(sin))) >> 12;
    let ry = ((ox.saturating_mul(sin)).saturating_add(oy.saturating_mul(cos))) >> 12;
    (
        clamp_i16((center.sx as i32).saturating_add(rx)),
        clamp_i16((center.sy as i32).saturating_add(ry)),
    )
}
