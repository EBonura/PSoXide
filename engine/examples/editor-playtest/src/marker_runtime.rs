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
        let options = room_surface_options(room_record);
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

/// Floating enemy health bar tuning. Deliberately smaller than the player's
/// 82x2-in-an-88x8-shell gauge: this one reads at a glance while you are
/// looking at the enemy, not at the HUD, and it carries no text.
const ENEMY_BAR_HALF_WIDTH: i32 = 17;
const ENEMY_BAR_HEIGHT: i32 = 3;
/// Screen-space gap between the projected head point and the bar's underside.
const ENEMY_BAR_HEAD_GAP: i32 = 4;
/// Skip the bar once the actor is further away than this, so a distant
/// silhouette does not collect a bar that is wider than it is.
const ENEMY_BAR_RANGE: i32 = 2600;
/// Half of the gap between the two boxes. The player's shells are two SEPARATE
/// chamfered boxes with 12px of HUD between them, not one bar with a divider,
/// and the gap is a large part of why the pair reads as a pair. Scaled to this
/// gauge that is two pixels total, which is also the darkest possible seam:
/// nothing is drawn there at all.
const ENEMY_BAR_CENTRE_GAP: i32 = 1;
/// 1px border of each box, straight off the player's shell nodes. This is what
/// was missing when the bar first shipped: the border was a near-black `(9,5,7)`
/// ring, which over unlit world geometry reads as no edge at all, so the gauge
/// looked like a bare pair of fills instead of the player's chrome. The shells
/// carry `border_color` -> `border_gradient.to`, and their `flip_x` is the
/// OPPOSITE of the fills', so the border ramps dark at the outer end to bright
/// toward the centre while the fill inside it ramps the other way.
const ENEMY_BAR_HORIZON_EDGE_OUTER: (u8, u8, u8) = (107, 29, 23);
const ENEMY_BAR_HORIZON_EDGE_INNER: (u8, u8, u8) = (220, 85, 47);
const ENEMY_BAR_ZENITH_EDGE_OUTER: (u8, u8, u8) = (20, 69, 69);
const ENEMY_BAR_ZENITH_EDGE_INNER: (u8, u8, u8) = (76, 191, 172);
/// Chamfer of the shell's cut corners, in pixels. The player's own shells cut
/// 2 of their 8 rows at 45 degrees; this shell is 5 rows tall, so one row is
/// the same proportion and copying the constant outright would eat nearly half
/// the bar and turn a 36x5 gauge into a lozenge. The cut is two pixels WIDE for
/// that one row: a 45-degree one-pixel cut lands exactly on the rasterizer's
/// fill boundary at this size and survives only on the leading edges, which
/// renders the mirrored diagonal as two top cuts and a square bottom. The
/// shallower bevel clears the rounding on all four corners and suits a bar
/// seven times wider than it is tall.
const ENEMY_BAR_CORNER_CUT_X: i32 = 2;
const ENEMY_BAR_CORNER_CUT_Y: i32 = 1;
/// Horizon empty track, unchanged from the single-pool bar: brighter than the
/// HUD's own `(15, 4, 7)` because this gauge floats over unlit world geometry
/// rather than a black HUD plate.
const ENEMY_BAR_HORIZON_EMPTY: (u8, u8, u8) = (46, 15, 17);
/// Zenith empty track. The HUD's two backgrounds are `(15, 4, 7)` Horizon and
/// `(3, 11, 14)` Zenith; this is the second lifted by the same ~3x the Horizon
/// track already carried, so the halves sit at one luminance.
const ENEMY_BAR_ZENITH_EMPTY: (u8, u8, u8) = (10, 33, 40);
/// Horizon fill, authored HUD values verbatim: the node paints `from` at the
/// anchored (centre) end and `to` at the free end, so the ramp runs dark at
/// the centre to bright at the outer edge on both halves.
const ENEMY_BAR_HORIZON_INNER: (u8, u8, u8) = (158, 25, 31);
const ENEMY_BAR_HORIZON_OUTER: (u8, u8, u8) = (255, 113, 58);
/// Zenith fill, likewise straight off the HUD node.
const ENEMY_BAR_ZENITH_INNER: (u8, u8, u8) = (28, 112, 110);
const ENEMY_BAR_ZENITH_OUTER: (u8, u8, u8) = (108, 224, 198);
/// Floating enemy health bar, drawn as real world geometry.
///
/// Vertices are built at a CONSTANT pixel size around the projected anchor but
/// all share the anchor's depth, so the bar keeps a fixed on-screen size while
/// the ordering table sorts it against walls and props like any other surface.
/// That is the whole point of submitting it here instead of in the overlay
/// pass: PS1 has no depth buffer, so an overlay draw lands on the finished
/// frame and cannot be occluded by it.

impl Playtest {
    /// Submit the locked target's health bar into the world ordering table.
    ///
    /// Two decisions are load-bearing here.
    ///
    /// FIRST, this is not an authored UI node. `psx_engine::ui::draw_scene`
    /// lays every node out from the cooked `&'static` pool and memoises the
    /// result across frames precisely because nothing in that pool moves. A
    /// world-anchored node whose rect changed every frame would invalidate that
    /// memo for the whole HUD and hand back the per-frame layout cost the UI
    /// pass was optimised away. Moving the bar into the world pass keeps it out
    /// of the UI pool entirely, so the memo is untouched.
    ///
    /// SECOND, it submits into the ordering table rather than drawing in the
    /// overlay pass, which is what makes walls occlude it. Two cheaper fixes
    /// were tried and measured first, and both failed: a segment trace from
    /// `camera.position` gated nothing at all, because the third-person boom
    /// routinely sits inside solid geometry and a trace starting in solid
    /// reports no hit; and a player-eye segment cost ~0.19% frame time while
    /// still drawing through the observed wall, because there the player had
    /// line of sight through a doorway and only the camera was blocked. Depth
    /// sorting is the only thing that answers the question actually being
    /// asked, which is whether the actor is visible in the rendered image.
    pub(super) fn draw_enemy_health_bar_world<T>(
        &self,
        camera: WorldCamera,
        options: WorldSurfaceOptions,
        packets: &mut T,
        world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
    ) -> usize
    where
        T: PrimitiveSink<TriGouraud>,
    {
        // Lock target only: the souls-like convention, and it means at most one
        // projection and a handful of packets however many actors a room holds.
        let Some(index) = self.lock_target else {
            return 0;
        };
        if !self.target_index_valid(index, ENEMY_BAR_RANGE) {
            return 0;
        }
        let Ok(instance) = u16::try_from(index) else {
            return 0;
        };
        let Some(entity) = game_entity_for_instance(instance) else {
            return 0;
        };
        let Some(record) = GAME_ENTITIES.get(entity) else {
            return 0;
        };
        let horizon = self.game_entities.health(entity);
        let zenith = self.game_entities.health_secondary(entity);
        // Both pools: an actor whose Horizon half is spent is still alive and
        // still needs a gauge. The death rule is "both empty", so the bar's
        // visibility rule has to be the same one.
        if (record.max_health == 0 && record.max_health_secondary == 0)
            || (horizon == 0 && zenith == 0)
        {
            return 0;
        }
        let Some(anchor) = self.enemy_health_bar_anchor(index) else {
            return 0;
        };
        let Some(center) = camera.project_world(anchor) else {
            return 0;
        };

        let cx = center.sx as i32;
        let cy = center.sy as i32;
        // Cheap screen reject. The projection already dropped anything behind
        // the near plane; this drops actors off the sides of the viewport.
        if cx < -ENEMY_BAR_HALF_WIDTH
            || cx > 320 + ENEMY_BAR_HALF_WIDTH
            || cy < -ENEMY_BAR_HEIGHT
            || cy > 240 + ENEMY_BAR_HEIGHT
        {
            return 0;
        }

        let left = cx - ENEMY_BAR_HALF_WIDTH;
        let right = cx + ENEMY_BAR_HALF_WIDTH;
        let bottom = cy - ENEMY_BAR_HEAD_GAP;
        let top = bottom - ENEMY_BAR_HEIGHT;
        let depth = center.sz;

        // `CullMode::None` because these are screen-space vertices whose winding
        // carries no facing information. A small negative bias keeps the bar
        // just in front of the actor it belongs to, so the actor's own head
        // cannot sort over its own bar.
        let bar_options = options
            .with_cull_mode(CullMode::None)
            .with_render_layer(WorldRenderLayer::Opaque)
            .with_depth_bias(-2);

        // The player's gauge, scaled down: TWO chamfered boxes with a gap
        // between them, Horizon left and Zenith right, each cut on diagonally
        // opposite corners and each flipped against the other so the pair reads
        // as symmetric. Both fills are anchored at their box's INNER edge, so
        // 0% is the centre and each pool grows outward to its own end.
        let mut submitted = 0usize;
        let h_inner = cx - ENEMY_BAR_CENTRE_GAP;
        let z_inner = cx + ENEMY_BAR_CENTRE_GAP;
        submitted += submit_enemy_bar_box(
            (left - 1, top - 1, h_inner, bottom + 1),
            depth,
            false,
            (ENEMY_BAR_HORIZON_EDGE_OUTER, ENEMY_BAR_HORIZON_EDGE_INNER),
            bar_options,
            packets,
            world,
        );
        submitted += submit_enemy_bar_box(
            (z_inner, top - 1, right + 1, bottom + 1),
            depth,
            true,
            (ENEMY_BAR_ZENITH_EDGE_INNER, ENEMY_BAR_ZENITH_EDGE_OUTER),
            bar_options,
            packets,
            world,
        );

        // Tracks and fills sit one pixel inside each box, so the border ring
        // survives whatever the pool does and the chamfer only ever eats border
        // pixels. That is what lets both stay plain rectangles.
        let h_track_inner = h_inner - 1;
        let z_track_inner = z_inner + 1;
        let track_span = h_track_inner - left;
        let mut quad = |rect: (i32, i32, i32, i32), lhs, rhs| {
            submit_enemy_bar_quad(rect, depth, lhs, rhs, bar_options, packets, world)
        };
        submitted += quad(
            (left, top, h_track_inner, bottom),
            ENEMY_BAR_HORIZON_EMPTY,
            ENEMY_BAR_HORIZON_EMPTY,
        );
        submitted += quad(
            (z_track_inner, top, right, bottom),
            ENEMY_BAR_ZENITH_EMPTY,
            ENEMY_BAR_ZENITH_EMPTY,
        );

        let fill_of = |current: u16, maximum: u16| {
            if maximum == 0 {
                0
            } else {
                (track_span * i32::from(current.min(maximum))) / i32::from(maximum)
            }
        };
        let horizon_fill = fill_of(horizon, record.max_health);
        let zenith_fill = fill_of(zenith, record.max_health_secondary);
        if horizon_fill > 0 {
            // Left box: the quad's left vertices are the OUTER end, so the
            // bright stop goes first to keep the HUD's dark-at-centre ramp.
            submitted += quad(
                (h_track_inner - horizon_fill, top, h_track_inner, bottom),
                ENEMY_BAR_HORIZON_OUTER,
                ENEMY_BAR_HORIZON_INNER,
            );
        }
        if zenith_fill > 0 {
            submitted += quad(
                (z_track_inner, top, z_track_inner + zenith_fill, bottom),
                ENEMY_BAR_ZENITH_INNER,
                ENEMY_BAR_ZENITH_OUTER,
            );
        }
        // No divider quad: the gap between the two boxes IS the separation, the
        // same way it is on the player's pair, and undrawn pixels read darker
        // than any colour a divider could have been. That also buys back the
        // two triangles the borders would otherwise have cost.
        submitted
    }

    /// World point the bar hangs from: the actor's head rather than the torso
    /// point [`Playtest::target_indicator_position`] uses for the lock reticle,
    /// so the two never sit on top of each other.
    fn enemy_health_bar_anchor(&self, index: usize) -> Option<RoomPoint> {
        let target = MODEL_INSTANCES.get(index)?;
        let position = self.target_position(index)?;
        let height = MODELS
            .get(target.model.to_usize())
            .map(|model| model.world_height as i32)
            .unwrap_or(1024);
        Some(RoomPoint::new(
            position.x,
            // Full height clears the head; the extra quarter keeps the bar off
            // it as the actor's animation bobs.
            position.y.saturating_add(height + (height >> 2)),
            position.z,
        ))
    }
}

/// Submit one of the gauge's two chamfered boxes, as a bordered hexagon.
///
/// `mirrored` false cuts the top-left and bottom-right corners; true cuts
/// top-right and bottom-left. DIAGONALLY OPPOSITE corners, never all four:
/// that is what gives the player's shells their slanted-parallelogram read
/// rather than a generic bevel, and feeding the Horizon box `false` and the
/// Zenith box `true` reproduces the `flip_x` that mirrors the player's pair
/// about the centre. Cutting all four corners, or cutting the two halves the
/// same way, both read lopsided or generic at this size.
///
/// The two bevels are NOT the same slope, and that is deliberate. The
/// rasterizer resolves each scanline's span from the polygon edges at the row's
/// TOP y, so a bevel one row deep only ever shortens the row it starts on: at
/// the top that is the first drawn row, but at the bottom it is the row BELOW
/// the last drawn one, and the cut vanishes. Measured on a 4x dump, not
/// assumed. The bottom bevel therefore starts two rows early and runs twice as
/// far out, which puts the same [`ENEMY_BAR_CORNER_CUT_X`] pixels of cut on the
/// last drawn row for the same four triangles.
fn submit_enemy_bar_box<T>(
    rect: (i32, i32, i32, i32),
    depth: i32,
    mirrored: bool,
    edge: ((u8, u8, u8), (u8, u8, u8)),
    options: WorldSurfaceOptions,
    packets: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> usize
where
    T: PrimitiveSink<TriGouraud>,
{
    let (x0, y0, x1, y1) = rect;
    let cut = ENEMY_BAR_CORNER_CUT_X;
    let corners: [(i32, i32); 6] = if mirrored {
        [
            (x0, y0),
            (x1 - cut, y0),
            (x1, y0 + ENEMY_BAR_CORNER_CUT_Y),
            (x1, y1),
            (x0 + 2 * cut, y1),
            (x0, y1 - 2 * ENEMY_BAR_CORNER_CUT_Y),
        ]
    } else {
        [
            (x0 + cut, y0),
            (x1, y0),
            (x1, y1 - 2 * ENEMY_BAR_CORNER_CUT_Y),
            (x1 - 2 * cut, y1),
            (x0, y1),
            (x0, y0 + ENEMY_BAR_CORNER_CUT_Y),
        ]
    };
    // Horizontal border gradient across the box, exactly as the shell nodes
    // paint theirs. Interpolating per vertex costs nothing on a Gouraud
    // primitive, so the ramp is free.
    let (edge_left, edge_right) = edge;
    let width = (x1 - x0).max(1);
    let vertex = |(x, y): (i32, i32)| {
        let t = ((x - x0).clamp(0, width) * 256) / width;
        let mix = |a: u8, b: u8| {
            ((i32::from(a) * (256 - t) + i32::from(b) * t) / 256).clamp(0, 255) as u8
        };
        projected_lit(
            ProjectedVertex::new(clamp_i16(x), clamp_i16(y), depth),
            (
                mix(edge_left.0, edge_right.0),
                mix(edge_left.1, edge_right.1),
                mix(edge_left.2, edge_right.2),
            ),
        )
    };
    // Convex, so a fan from the first corner is a valid triangulation.
    let hub = vertex(corners[0]);
    let mut submitted = 0u16;
    for edge in 1..corners.len() - 1 {
        submitted += world
            .submit_gouraud_triangle(
                packets,
                [hub, vertex(corners[edge]), vertex(corners[edge + 1])],
                options,
            )
            .submitted_triangles;
    }
    usize::from(submitted)
}

/// Submit one screen-space rectangle at a single depth, as two triangles.
///
/// `left`/`right` are the horizontal gradient stops the HUD's own bar nodes
/// carry; pass the same colour twice for a flat quad. Gouraud interpolation is
/// already paid for by the primitive type, so the ramp is free.
fn submit_enemy_bar_quad<T>(
    rect: (i32, i32, i32, i32),
    depth: i32,
    left: (u8, u8, u8),
    right: (u8, u8, u8),
    options: WorldSurfaceOptions,
    packets: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> usize
where
    T: PrimitiveSink<TriGouraud>,
{
    let (x0, y0, x1, y1) = rect;
    let vertex = |x: i32, y: i32, color: (u8, u8, u8)| {
        projected_lit(
            ProjectedVertex::new(clamp_i16(x), clamp_i16(y), depth),
            color,
        )
    };
    let tl = vertex(x0, y0, left);
    let tr = vertex(x1, y0, right);
    let bl = vertex(x0, y1, left);
    let br = vertex(x1, y1, right);
    let mut submitted = 0u16;
    submitted += world
        .submit_gouraud_triangle(packets, [tl, tr, bl], options)
        .submitted_triangles;
    submitted += world
        .submit_gouraud_triangle(packets, [tr, br, bl], options)
        .submitted_triangles;
    usize::from(submitted)
}
