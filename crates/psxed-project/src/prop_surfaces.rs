//! World-space surfaces of Box, Cylinder and Image props, built by the code
//! the cook uses, so the editor preview draws the geometry that ships.
//!
//! The cook bakes these surfaces (or, for an uneroded Box Prop, the six cage
//! faces the runtime rebuilds from the record) at engine scale; the preview
//! calls the same functions at authored scale.

use crate::spatial::rotate_euler_local_q12;
use crate::{
    BoxPropErosion, CylinderPropGeometry, GridUvTransform, BOX_PROP_FACE_COUNT,
    BOX_PROP_FACE_VERTEX_INDICES, BOX_PROP_VERTEX_COUNT,
};

/// One flat prop surface in world units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropSurface {
    /// Corners in outward winding. Only the first `vertex_count` are used.
    pub vertices: [[i32; 3]; 4],
    pub vertex_count: u8,
    /// Where each corner sits inside its material slot's UV quad, in Q0.8
    /// (`0..=255`). [`bake_prop_uv`] turns it into texels.
    pub uv_q8: [[u8; 2]; 4],
    /// Box face (`BOX_PROP_FACE_NAMES` order) or Cylinder material slot.
    pub slot: u8,
}

/// Q0.8 corners of a whole face, in face-vertex order.
pub const PROP_FACE_UV_Q8: [[u8; 2]; 4] = [[0, 0], [255, 0], [255, 255], [0, 255]];

/// A prop-local vertex placed in the world with the authored Euler rotation
/// (Q12 angles, pitch then yaw then roll), as the cook and runtime place it.
pub fn prop_local_to_world(
    pos: [i32; 3],
    local: [i16; 3],
    pitch: i16,
    yaw: i16,
    roll: i16,
) -> [i32; 3] {
    let rotated = rotate_euler_local_q12(
        [
            i32::from(local[0]),
            i32::from(local[1]),
            i32::from(local[2]),
        ],
        pitch as u16,
        yaw as u16,
        roll as u16,
    );
    [
        pos[0].saturating_add(rotated[0]),
        pos[1].saturating_add(rotated[1]),
        pos[2].saturating_add(rotated[2]),
    ]
}

/// A Box Prop's surfaces: the eroded quads when erosion is on, otherwise the
/// six cage faces the runtime draws from the cooked record.
pub fn box_prop_world_surfaces(
    pos: [i32; 3],
    pitch: i16,
    yaw: i16,
    roll: i16,
    vertices: [[i16; 3]; BOX_PROP_VERTEX_COUNT],
    erosion: BoxPropErosion,
) -> Vec<PropSurface> {
    let place = |local: [i16; 3]| prop_local_to_world(pos, local, pitch, yaw, roll);
    let eroded = crate::generate_box_prop_erosion_quads(vertices, erosion);
    if !eroded.is_empty() {
        return eroded
            .into_iter()
            .map(|quad| PropSurface {
                vertices: quad.vertices.map(place),
                vertex_count: 4,
                uv_q8: quad.uv_q8,
                slot: quad.source_face.min(BOX_PROP_FACE_COUNT as u8 - 1),
            })
            .collect();
    }
    (0..BOX_PROP_FACE_COUNT)
        .map(|face| PropSurface {
            vertices: BOX_PROP_FACE_VERTEX_INDICES[face].map(|index| place(vertices[index])),
            vertex_count: 4,
            uv_q8: PROP_FACE_UV_Q8,
            slot: face as u8,
        })
        .collect()
}

/// A Cylinder Prop's generated surfaces in world units.
pub fn cylinder_prop_world_surfaces(
    pos: [i32; 3],
    pitch: i16,
    yaw: i16,
    roll: i16,
    geometry: CylinderPropGeometry,
) -> Vec<PropSurface> {
    crate::generate_cylinder_prop_surfaces(geometry)
        .into_iter()
        .map(|surface| PropSurface {
            vertices: surface
                .vertices
                .map(|local| prop_local_to_world(pos, local, pitch, yaw, roll)),
            vertex_count: surface.vertex_count.clamp(3, 4),
            uv_q8: surface.uv_q8,
            slot: surface.material_slot,
        })
        .collect()
}

/// An Image Prop's card, bottom-centre anchored, in the runtime's corner
/// order (top-left, top-right, bottom-right, bottom-left). A cylindrical
/// billboard turns to face a camera with the given yaw (Q12 sine and
/// cosine), as the runtime does every frame.
pub fn image_prop_world_quad(
    pos: [i32; 3],
    width: u16,
    height: u16,
    pitch: i16,
    yaw: i16,
    roll: i16,
    billboard_camera_yaw_sin_cos_q12: Option<(i32, i32)>,
) -> [[i32; 3]; 4] {
    let half_width = i32::from(width.max(1)) >> 1;
    let top = i32::from(height.max(1));
    if let Some((sin, cos)) = billboard_camera_yaw_sin_cos_q12 {
        let right_x = (half_width * cos) >> 12;
        let right_z = -((half_width * sin) >> 12);
        let top_y = pos[1].saturating_add(top);
        return [
            [pos[0] - right_x, top_y, pos[2] - right_z],
            [pos[0] + right_x, top_y, pos[2] + right_z],
            [pos[0] + right_x, pos[1], pos[2] + right_z],
            [pos[0] - right_x, pos[1], pos[2] - right_z],
        ];
    }
    let clamp16 = |value: i32| value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
    [
        [-half_width, top, 0],
        [half_width, top, 0],
        [half_width, 0, 0],
        [-half_width, 0, 0],
    ]
    .map(|local| prop_local_to_world(pos, local.map(clamp16), pitch, yaw, roll))
}

/// The texel quad a slot's UV transform selects in a texture of this size:
/// the corners the cook bakes into the prop record.
pub fn prop_uv_corners(
    uv: GridUvTransform,
    texture_width: u16,
    texture_height: u16,
) -> [(u8, u8); 4] {
    let u_max = texture_width.saturating_sub(1).min(255) as u8;
    let v_max = texture_height.saturating_sub(1).min(255) as u8;
    uv.apply_to_quad([(0, 0), (u_max, 0), (u_max, v_max), (0, v_max)])
}

/// Bilinear position `uv_q8` inside the texel quad `corners`, as the cook
/// bakes generated prop surfaces.
pub fn bake_prop_uv(corners: [(u8, u8); 4], uv_q8: [u8; 2]) -> [u8; 2] {
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
    [interpolate(0), interpolate(1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_face_corners_bake_to_the_slot_quad_exactly() {
        let corners = [(3, 5), (60, 7), (61, 30), (2, 29)];
        for (index, corner) in PROP_FACE_UV_Q8.iter().enumerate() {
            let [u, v] = bake_prop_uv(corners, *corner);
            assert_eq!((u, v), corners[index]);
        }
    }

    #[test]
    fn an_uneroded_box_is_its_six_rotated_cage_faces() {
        let vertices = crate::box_prop_vertices_for_size(256);
        let surfaces = box_prop_world_surfaces(
            [1000, 0, 2000],
            0,
            0,
            0,
            vertices,
            BoxPropErosion::default(),
        );
        assert_eq!(surfaces.len(), BOX_PROP_FACE_COUNT);
        for (face, surface) in surfaces.iter().enumerate() {
            for (corner, world) in surface.vertices.iter().enumerate() {
                let local = vertices[BOX_PROP_FACE_VERTEX_INDICES[face][corner]];
                assert_eq!(
                    *world,
                    [
                        1000 + i32::from(local[0]),
                        i32::from(local[1]),
                        2000 + i32::from(local[2])
                    ]
                );
            }
        }
    }
}
