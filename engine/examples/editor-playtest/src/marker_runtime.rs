use super::*;

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

/// Recommended world-space height for a compact Archive Beacon. The helper
/// accepts an override so authored POIs remain free to scale the glyph.
pub(super) const ARCHIVE_BEACON_DEFAULT_HEIGHT: i32 = 24;
const ARCHIVE_BEACON_MIN_HEIGHT: i32 = 8;
const ARCHIVE_BEACON_MAX_HEIGHT: i32 = 48;
const ARCHIVE_BEACON_ACTIVE_ROTATION_FRAMES: u32 = 180;
const ARCHIVE_BEACON_INTERACTABLE_ROTATION_FRAMES: u32 = 120;
/// Rendering-only state for the Archive Beacon glyph. Gameplay persistence,
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

/// Draw a PS1-native high-tech Archive Beacon with no dedicated texture.
///
/// Two crossed, chamfered vertical panels are projected from the world anchor
/// and submitted in the overlay pass. This keeps the interaction glyph legible
/// in packet-heavy BSP scenes without allocating a texture or CLUT. `position`
/// is the glyph centre; `height` is the glyph's full world-space height.
pub(super) fn draw_archive_beacon_overlay(
    position: RoomPoint,
    yaw: Angle,
    height: i32,
    state: ArchiveBeaconVisualState,
    elapsed_tick: SimTick,
    camera: WorldCamera,
) {
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
    let (bright_tint, dark_tint, blend) = archive_beacon_palette(state, pulse);
    draw_archive_beacon_panel_overlay(
        position,
        panel_yaw,
        height,
        bright_tint,
        dark_tint,
        blend,
        matches!(state, ArchiveBeaconVisualState::Interactable),
        camera,
    );
    draw_archive_beacon_panel_overlay(
        position,
        panel_yaw.add(Angle::QUARTER),
        height,
        bright_tint,
        dark_tint,
        blend,
        matches!(state, ArchiveBeaconVisualState::Interactable),
        camera,
    );
}

fn draw_archive_beacon_panel_overlay(
    position: RoomPoint,
    yaw: Angle,
    height: i32,
    bright_tint: (u8, u8, u8),
    dark_tint: (u8, u8, u8),
    blend: BlendMode,
    fallback_to_interaction_anchor: bool,
    camera: WorldCamera,
) {
    let center = match camera.project_world(position) {
        Some(projected)
            if (-16..=336).contains(&projected.sx) && (-16..=256).contains(&projected.sy) =>
        {
            projected
        }
        None if fallback_to_interaction_anchor => ProjectedVertex {
            sx: 160,
            sy: 76,
            sz: 1,
        },
        Some(_) if fallback_to_interaction_anchor => ProjectedVertex {
            sx: 160,
            sy: 76,
            sz: 1,
        },
        _ => return,
    };
    let projected = archive_beacon_screen_vertices(center, yaw, height);

    // Four triangles preserve the six-point TL/BR chamfer silhouette. The
    // brighter upper pair and darker lower pair provide a tiny vertical red
    // gradient without spending a Gouraud arena or a new texture.
    const FACES: [[usize; 3]; 4] = [[0, 1, 2], [0, 2, 3], [0, 3, 4], [0, 4, 5]];
    let mut face = 0usize;
    while face < FACES.len() {
        let indices = FACES[face];
        let tint = if face < 2 { bright_tint } else { dark_tint };
        draw_tri_flat_blended(
            [
                projected[indices[0]],
                projected[indices[1]],
                projected[indices[2]],
            ],
            tint.0,
            tint.1,
            tint.2,
            blend,
        );
        face += 1;
    }
}

fn archive_beacon_screen_vertices(
    center: ProjectedVertex,
    yaw: Angle,
    height: i32,
) -> [(i16, i16); 6] {
    let height = height.clamp(ARCHIVE_BEACON_MIN_HEIGHT, ARCHIVE_BEACON_MAX_HEIGHT);
    let half_height = (height / 2).clamp(6, 24);
    let half_width = (height / 3).clamp(4, 16);
    let cut = (height / 6).clamp(2, 8);
    let local = [
        (-half_width + cut, half_height),
        (half_width, half_height),
        (half_width, -half_height + cut),
        (half_width - cut, -half_height),
        (-half_width, -half_height),
        (-half_width, half_height - cut),
    ];
    let sin = yaw.sin_q12();
    let cos = yaw.cos_q12();
    let mut vertices = [(0i16, 0i16); 6];
    let mut index = 0usize;
    while index < local.len() {
        let (x, y) = local[index];
        let rx = (x.saturating_mul(cos).saturating_sub(y.saturating_mul(sin))) >> 12;
        let ry = (x.saturating_mul(sin).saturating_add(y.saturating_mul(cos))) >> 12;
        vertices[index] = (
            clamp_i16(i32::from(center.sx).saturating_add(rx)),
            clamp_i16(i32::from(center.sy).saturating_add(ry)),
        );
        index += 1;
    }
    vertices
}

fn archive_beacon_pulse(rotation: Angle, state: ArchiveBeaconVisualState) -> u8 {
    if matches!(state, ArchiveBeaconVisualState::Depleted) {
        return 0;
    }
    (((rotation.sin_q12().saturating_add(4096)) * 255) / 8192).clamp(0, 255) as u8
}

fn archive_beacon_palette(
    state: ArchiveBeaconVisualState,
    pulse: u8,
) -> ((u8, u8, u8), (u8, u8, u8), BlendMode) {
    match state {
        ArchiveBeaconVisualState::Active => (
            (
                86u8.saturating_add(pulse / 6),
                14u8.saturating_add(pulse / 16),
                10u8.saturating_add(pulse / 22),
            ),
            (54, 8, 7),
            BlendMode::AddQuarter,
        ),
        ArchiveBeaconVisualState::Interactable => (
            (
                188u8.saturating_add(pulse / 4),
                18u8.saturating_add(pulse / 12),
                14u8.saturating_add(pulse / 16),
            ),
            (126, 8, 7),
            // Average remains legible over both black floors and bright
            // player/enemy silhouettes; additive red disappears on white.
            BlendMode::Average,
        ),
        ArchiveBeaconVisualState::Depleted => ((36, 7, 6), (22, 4, 4), BlendMode::Average),
    }
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
