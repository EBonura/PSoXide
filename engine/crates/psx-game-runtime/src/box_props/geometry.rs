use super::*;
use crate::image_props::{average4_i32, rotate_x_q12, rotate_y_q12, rotate_z_q12};

fn box_prop_face_point_q8(face: [WorldVertex; 4], u_q8: u16, v_q8: u16) -> WorldVertex {
    let left = lerp_world_vertex_q8(face[0], face[3], v_q8);
    let right = lerp_world_vertex_q8(face[1], face[2], v_q8);
    lerp_world_vertex_q8(left, right, u_q8)
}

pub(super) fn box_prop_quad_center(quad: [WorldVertex; 4]) -> WorldVertex {
    WorldVertex::new(
        average4_i32(quad[0].x, quad[1].x, quad[2].x, quad[3].x),
        average4_i32(quad[0].y, quad[1].y, quad[2].y, quad[3].y),
        average4_i32(quad[0].z, quad[1].z, quad[2].z, quad[3].z),
    )
}

pub(super) fn box_prop_face_color_at(
    prop: &LevelBoxPropRecord,
    face: usize,
    u_q8: u16,
    v_q8: u16,
) -> (u8, u8, u8) {
    let colors = prop.baked_vertex_rgb[face];
    let top = lerp_rgb_q8(colors[0], colors[1], u_q8);
    let bottom = lerp_rgb_q8(colors[3], colors[2], u_q8);
    lerp_rgb_q8(top, bottom, v_q8)
}

fn lerp_world_vertex_q8(a: WorldVertex, b: WorldVertex, t_q8: u16) -> WorldVertex {
    WorldVertex::new(
        lerp_i32_q8(a.x, b.x, t_q8),
        lerp_i32_q8(a.y, b.y, t_q8),
        lerp_i32_q8(a.z, b.z, t_q8),
    )
}

fn lerp_rgb_q8(a: (u8, u8, u8), b: (u8, u8, u8), t_q8: u16) -> (u8, u8, u8) {
    (
        lerp_i32_q8(a.0 as i32, b.0 as i32, t_q8) as u8,
        lerp_i32_q8(a.1 as i32, b.1 as i32, t_q8) as u8,
        lerp_i32_q8(a.2 as i32, b.2 as i32, t_q8) as u8,
    )
}

fn lerp_i32_q8(a: i32, b: i32, t_q8: u16) -> i32 {
    let t = t_q8.min(256) as i32;
    a.saturating_add(b.saturating_sub(a).saturating_mul(t) / 256)
}

pub(super) fn uv_from_q8(max: u8, t_q8: u16) -> u8 {
    ((max as u16).saturating_mul(t_q8.min(256)) >> 8) as u8
}

pub(super) fn shrink_world_vertex_around(
    vertex: WorldVertex,
    center: WorldVertex,
    scale_q8: i32,
) -> WorldVertex {
    WorldVertex::new(
        center.x.saturating_add(scale_q8_i32_signed(
            vertex.x.saturating_sub(center.x),
            scale_q8,
        )),
        center.y.saturating_add(scale_q8_i32_signed(
            vertex.y.saturating_sub(center.y),
            scale_q8,
        )),
        center.z.saturating_add(scale_q8_i32_signed(
            vertex.z.saturating_sub(center.z),
            scale_q8,
        )),
    )
}

fn world_vertex_delta(from: WorldVertex, to: WorldVertex) -> [i32; 3] {
    [
        to.x.saturating_sub(from.x),
        to.y.saturating_sub(from.y),
        to.z.saturating_sub(from.z),
    ]
}

pub(super) fn scale_world_delta_q8(delta: [i32; 3], scale_q8: i32) -> [i32; 3] {
    [
        scale_q8_i32_signed(delta[0], scale_q8),
        scale_q8_i32_signed(delta[1], scale_q8),
        scale_q8_i32_signed(delta[2], scale_q8),
    ]
}

pub(super) fn add_world_vertex_offset(vertex: WorldVertex, offset: [i32; 3]) -> WorldVertex {
    WorldVertex::new(
        vertex.x.saturating_add(offset[0]),
        vertex.y.saturating_add(offset[1]),
        vertex.z.saturating_add(offset[2]),
    )
}

pub(super) fn scale_q8_i32_signed(value: i32, scale_q8: i32) -> i32 {
    value.saturating_mul(scale_q8) / 256
}

fn box_prop_vertices(prop: &LevelBoxPropRecord) -> [WorldVertex; psx_level::BOX_PROP_VERTEX_COUNT] {
    let mut out = [WorldVertex::new(0, 0, 0); psx_level::BOX_PROP_VERTEX_COUNT];
    let mut i = 0usize;
    while i < prop.vertices.len() {
        let local = prop.vertices[i];
        let rotated = rotate_z_q12(
            rotate_y_q12(
                rotate_x_q12(
                    [local[0] as i32, local[1] as i32, local[2] as i32],
                    prop.pitch as u16,
                ),
                prop.yaw as u16,
            ),
            prop.roll as u16,
        );
        out[i] = WorldVertex::new(
            prop.x.saturating_add(rotated[0]),
            prop.y.saturating_add(rotated[1]),
            prop.z.saturating_add(rotated[2]),
        );
        i += 1;
    }
    out
}

fn box_prop_faces(
    vertices: [WorldVertex; psx_level::BOX_PROP_VERTEX_COUNT],
) -> [[WorldVertex; 4]; psx_level::BOX_PROP_FACE_COUNT] {
    let mut out = [[WorldVertex::new(0, 0, 0); 4]; psx_level::BOX_PROP_FACE_COUNT];
    let mut face = 0usize;
    while face < psx_level::BOX_PROP_FACE_COUNT {
        let mut corner = 0usize;
        while corner < 4 {
            out[face][corner] = vertices[BOX_PROP_FACE_VERTEX_INDICES[face][corner]];
            corner += 1;
        }
        face += 1;
    }
    out
}

fn box_prop_face_runtime(face: [WorldVertex; 4]) -> BoxPropFaceRuntime {
    let abx = face[1].x.saturating_sub(face[0].x);
    let aby = face[1].y.saturating_sub(face[0].y);
    let abz = face[1].z.saturating_sub(face[0].z);
    let acx = face[2].x.saturating_sub(face[0].x);
    let acy = face[2].y.saturating_sub(face[0].y);
    let acz = face[2].z.saturating_sub(face[0].z);
    let nx = aby
        .saturating_mul(acz)
        .saturating_sub(abz.saturating_mul(acy))
        >> BOX_PROP_FACE_NORMAL_SHIFT;
    let ny = abz
        .saturating_mul(acx)
        .saturating_sub(abx.saturating_mul(acz))
        >> BOX_PROP_FACE_NORMAL_SHIFT;
    let nz = abx
        .saturating_mul(acy)
        .saturating_sub(aby.saturating_mul(acx))
        >> BOX_PROP_FACE_NORMAL_SHIFT;
    BoxPropFaceRuntime {
        vertices: face,
        center: WorldVertex::new(
            average4_i32(face[0].x, face[1].x, face[2].x, face[3].x),
            average4_i32(face[0].y, face[1].y, face[2].y, face[3].y),
            average4_i32(face[0].z, face[1].z, face[2].z, face[3].z),
        ),
        normal: [nx, ny, nz],
    }
}

pub(super) fn build_box_prop_runtime(prop: &LevelBoxPropRecord) -> BoxPropRuntime {
    let vertices = box_prop_vertices(prop);
    let raw_faces = box_prop_faces(vertices);
    let mut faces = [BoxPropFaceRuntime::EMPTY; psx_level::BOX_PROP_FACE_COUNT];
    let mut face = 0usize;
    while face < psx_level::BOX_PROP_FACE_COUNT {
        faces[face] = box_prop_face_runtime(raw_faces[face]);
        face += 1;
    }
    let (cull_center, cull_radius) = box_prop_cull_bounds(vertices);
    let break_shards = box_prop_break_shard_runtime(prop, raw_faces, cull_center);
    let aabb_min = RoomPoint::new(
        prop.collision_min[0],
        prop.collision_min[1],
        prop.collision_min[2],
    );
    let aabb_max = RoomPoint::new(
        prop.collision_max[0],
        prop.collision_max[1],
        prop.collision_max[2],
    );
    let floor_y = box_prop_floor_y(vertices);
    BoxPropRuntime {
        faces,
        break_shards,
        cull_center,
        cull_radius,
        floor_y,
        // Never let the baked ground sit above the box's own bottom (a box
        // rests on or above its floor); guards against a stale cook value.
        ground_y: prop.ground_y.min(floor_y),
        debris_bounds: box_prop_debris_bounds(vertices),
        aabb_min,
        aabb_max,
    }
}

fn box_prop_break_shard_runtime(
    prop: &LevelBoxPropRecord,
    faces: [[WorldVertex; 4]; psx_level::BOX_PROP_FACE_COUNT],
    box_center: WorldVertex,
) -> [BoxPropBreakShardRuntime; BOX_PROP_BREAK_SHARD_COUNT] {
    let mut out = [BoxPropBreakShardRuntime::EMPTY; BOX_PROP_BREAK_SHARD_COUNT];
    let mut shard_index = 0usize;
    while shard_index < BOX_PROP_BREAK_SHARD_COUNT {
        let shard = BOX_PROP_BREAK_SHARDS[shard_index];
        let face = shard.face as usize;
        if face < psx_level::BOX_PROP_FACE_COUNT {
            let face_vertices = faces[face];
            let base_quad = [
                box_prop_face_point_q8(face_vertices, shard.u0_q8, shard.v0_q8),
                box_prop_face_point_q8(face_vertices, shard.u1_q8, shard.v0_q8),
                box_prop_face_point_q8(face_vertices, shard.u1_q8, shard.v1_q8),
                box_prop_face_point_q8(face_vertices, shard.u0_q8, shard.v1_q8),
            ];
            let face_center = box_prop_quad_center(face_vertices);
            out[shard_index] = BoxPropBreakShardRuntime {
                face: shard.face,
                base_quad,
                center: box_prop_quad_center(base_quad),
                edge_u: world_vertex_delta(face_vertices[0], face_vertices[1]),
                edge_v: world_vertex_delta(face_vertices[0], face_vertices[3]),
                face_delta: world_vertex_delta(box_center, face_center),
                colors: [
                    box_prop_face_color_at(prop, face, shard.u0_q8, shard.v0_q8),
                    box_prop_face_color_at(prop, face, shard.u1_q8, shard.v0_q8),
                    box_prop_face_color_at(prop, face, shard.u1_q8, shard.v1_q8),
                    box_prop_face_color_at(prop, face, shard.u0_q8, shard.v1_q8),
                ],
            };
        }
        shard_index += 1;
    }
    out
}

fn box_prop_cull_bounds(
    vertices: [WorldVertex; psx_level::BOX_PROP_VERTEX_COUNT],
) -> (WorldVertex, i32) {
    let mut min_x = vertices[0].x;
    let mut max_x = vertices[0].x;
    let mut min_y = vertices[0].y;
    let mut max_y = vertices[0].y;
    let mut min_z = vertices[0].z;
    let mut max_z = vertices[0].z;
    for vertex in vertices {
        min_x = min_x.min(vertex.x);
        max_x = max_x.max(vertex.x);
        min_y = min_y.min(vertex.y);
        max_y = max_y.max(vertex.y);
        min_z = min_z.min(vertex.z);
        max_z = max_z.max(vertex.z);
    }
    let center = WorldVertex::new(
        min_x.saturating_add(max_x) / 2,
        min_y.saturating_add(max_y) / 2,
        min_z.saturating_add(max_z) / 2,
    );
    let radius = abs_delta_i32(max_x, min_x)
        .saturating_add(abs_delta_i32(max_y, min_y))
        .saturating_add(abs_delta_i32(max_z, min_z))
        >> 1;
    (center, radius.max(32))
}

fn box_prop_floor_y(vertices: [WorldVertex; psx_level::BOX_PROP_VERTEX_COUNT]) -> i32 {
    let mut floor_y = vertices[0].y;
    for vertex in vertices {
        floor_y = floor_y.min(vertex.y);
    }
    floor_y
}

fn box_prop_debris_bounds(
    vertices: [WorldVertex; psx_level::BOX_PROP_VERTEX_COUNT],
) -> BoxPropDebrisBounds {
    let mut min_x = vertices[0].x;
    let mut max_x = vertices[0].x;
    let mut min_z = vertices[0].z;
    let mut max_z = vertices[0].z;
    for vertex in vertices {
        min_x = min_x.min(vertex.x);
        max_x = max_x.max(vertex.x);
        min_z = min_z.min(vertex.z);
        max_z = max_z.max(vertex.z);
    }
    BoxPropDebrisBounds {
        center_x: min_x.saturating_add(max_x) / 2,
        center_z: min_z.saturating_add(max_z) / 2,
        span_x: max_x.saturating_sub(min_x).max(64),
        span_z: max_z.saturating_sub(min_z).max(64),
    }
}

/// Whether two box AABBs overlap in the X/Z (floor) plane. Used to decide
/// if one box sits over another for stacked-support detection.
pub(super) fn box_prop_aabb_overlaps_xz(
    a_min: RoomPoint,
    a_max: RoomPoint,
    b_min: RoomPoint,
    b_max: RoomPoint,
) -> bool {
    a_min.x <= b_max.x && a_max.x >= b_min.x && a_min.z <= b_max.z && a_max.z >= b_min.z
}

/// Shift a box-face quad down (or up) by `dy` room units. Used to draw a
/// falling box at its current offset without rebuilding its runtime.
pub(super) fn box_prop_offset_quad_y(quad: [WorldVertex; 4], dy: i32) -> [WorldVertex; 4] {
    if dy == 0 {
        return quad;
    }
    let mut out = quad;
    for vertex in out.iter_mut() {
        *vertex = WorldVertex::new(vertex.x, vertex.y.saturating_add(dy), vertex.z);
    }
    out
}
