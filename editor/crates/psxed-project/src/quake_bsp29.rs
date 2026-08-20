//! Geometry-only Quake BSP29 topology import.
//!
//! Full Quake maps already carry a high-quality render partition and PVS. The
//! editor still imports the original `.map` brushes as its editable authority;
//! this module optionally reuses only the compiled planes, polygons, nodes,
//! leaves and visibility for a practical PSX playtest cook. Texture, lighting,
//! entity and gameplay lumps are deliberately discarded.

use crate::brush_compile::pack_normalized_plane;
use crate::brush_collision_hulls::CompiledCollisionHulls;
use crate::brush_pack::PackedBspGeometry;
use crate::ResourceId;
use psx_bsp::{FACE_BACKSIDE, FACE_BAKED_LIGHT};
use std::fmt;

const BSP29_VERSION: i32 = 29;
const LUMP_COUNT: usize = 15;
const HEADER_BYTES: usize = 4 + LUMP_COUNT * 8;
const PLANES: usize = 1;
const VERTICES: usize = 3;
const VISIBILITY: usize = 4;
const NODES: usize = 5;
const FACES: usize = 7;
const LEAVES: usize = 10;
const MARK_SURFACES: usize = 11;
const EDGES: usize = 12;
const SURFACE_EDGES: usize = 13;
const MODELS: usize = 14;
const KEPT_LUMPS: [usize; 9] = [
    PLANES,
    VERTICES,
    VISIBILITY,
    NODES,
    FACES,
    LEAVES,
    MARK_SURFACES,
    EDGES,
    SURFACE_EDGES,
];

#[derive(Clone, Copy, Debug, Default)]
struct LumpRange {
    offset: usize,
    len: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuakeBsp29Error(pub String);

impl fmt::Display for QuakeBsp29Error {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(&self.0)
    }
}

impl std::error::Error for QuakeBsp29Error {}

/// Copy only the BSP29 topology/geometry/PVS lumps needed by
/// [`import_quake_bsp29_geometry`]. The result contains no Quake texture
/// names, texture pixels, lightmaps, entities, model assets or audio. The
/// compact BSP world-model bounds/head-node table is retained.
pub fn strip_quake_bsp29_geometry(bytes: &[u8]) -> Result<Vec<u8>, QuakeBsp29Error> {
    let lumps = parse_header(bytes)?;
    let mut keep = [false; LUMP_COUNT];
    for index in KEPT_LUMPS.into_iter().chain([MODELS]) {
        keep[index] = true;
    }
    let mut output = vec![0; HEADER_BYTES];
    output[..4].copy_from_slice(&BSP29_VERSION.to_le_bytes());
    for index in 0..LUMP_COUNT {
        while output.len() & 3 != 0 {
            output.push(0);
        }
        let offset = output.len();
        let len = if keep[index] { lumps[index].len } else { 0 };
        if keep[index] {
            output.extend_from_slice(lump(bytes, lumps[index]));
        }
        let header = 4 + index * 8;
        output[header..header + 4].copy_from_slice(&(offset as i32).to_le_bytes());
        output[header + 4..header + 8].copy_from_slice(&(len as i32).to_le_bytes());
    }
    Ok(output)
}

/// Convert stripped BSP29 render topology to PSoXide's Y-up packed geometry.
/// Every face uses `material`; source texture and lightmap data are ignored.
pub fn import_quake_bsp29_geometry(
    bytes: &[u8],
    scale: f64,
    material: Option<ResourceId>,
) -> Result<PackedBspGeometry, QuakeBsp29Error> {
    if scale <= 0.0 {
        return Err(error("BSP29 import scale must be positive"));
    }
    let lumps = parse_header(bytes)?;
    require_aligned(lumps[PLANES].len, 20, "planes")?;
    require_aligned(lumps[VERTICES].len, 12, "vertices")?;
    require_aligned(lumps[NODES].len, 24, "nodes")?;
    require_aligned(lumps[FACES].len, 20, "faces")?;
    require_aligned(lumps[LEAVES].len, 28, "leaves")?;
    require_aligned(lumps[MARK_SURFACES].len, 2, "mark surfaces")?;
    require_aligned(lumps[EDGES].len, 4, "edges")?;
    require_aligned(lumps[SURFACE_EDGES].len, 4, "surface edges")?;
    require_aligned(lumps[MODELS].len, 64, "models")?;

    let source_planes = lump(bytes, lumps[PLANES]);
    let mut planes = Vec::with_capacity(source_planes.len() / 20 * 14);
    let mut plane_flipped = Vec::with_capacity(source_planes.len() / 20);
    let mut transformed_normals = Vec::with_capacity(source_planes.len() / 20);
    for (index, source) in source_planes.chunks_exact(20).enumerate() {
        let source_normal = [f32_at(source, 0)?, f32_at(source, 4)?, f32_at(source, 8)?];
        let mut normal = [
            f64::from(source_normal[0]),
            f64::from(source_normal[2]),
            -f64::from(source_normal[1]),
        ];
        let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
        if !length.is_finite() || length <= f64::EPSILON {
            return Err(error(format!("BSP29 plane {index} has an invalid normal")));
        }
        for value in &mut normal {
            *value /= length;
        }
        let distance = f64::from(f32_at(source, 12)?) * scale / length;
        let (record, flipped) = pack_normalized_plane(normal, distance)
            .ok_or_else(|| error(format!("BSP29 plane {index} cannot be packed")))?;
        planes.extend_from_slice(&record);
        plane_flipped.push(flipped);
        transformed_normals.push(normal);
    }

    let source_faces = lump(bytes, lumps[FACES]);
    let face_count = source_faces.len() / 20;
    if face_count > i16::MAX as usize {
        return Err(error("BSP29 face table exceeds PXBSP i16 limit"));
    }
    let mut vertices = Vec::new();
    let mut faces = Vec::with_capacity(face_count * 10);
    let mut face_source_centroids = Vec::with_capacity(face_count);
    for (face_index, source) in source_faces.chunks_exact(20).enumerate() {
        let plane_index = usize::from(u16_at(source, 0)?);
        let normal = *transformed_normals
            .get(plane_index)
            .ok_or_else(|| error(format!("BSP29 face {face_index} has an invalid plane")))?;
        let side = i16_at(source, 2)? != 0;
        let polygon_source = source_face_vertices(bytes, &lumps, source)?;
        if !(3..=39).contains(&polygon_source.len()) {
            return Err(error(format!(
                "BSP29 face {face_index} has unsupported vertex count {}",
                polygon_source.len()
            )));
        }
        let first_vertex = vertices.len() / 12;
        if first_vertex + polygon_source.len() > u16::MAX as usize {
            return Err(error("BSP29 face vertices exceed PXBSP u16 limit"));
        }
        let mut centroid = [0.0f64; 3];
        for source_vertex in &polygon_source {
            for axis in 0..3 {
                centroid[axis] += f64::from(source_vertex[axis]);
            }
            let position = transform_position(*source_vertex, scale)?;
            let uv = planar_uv(position, normal);
            for value in position {
                vertices.extend_from_slice(&value.to_le_bytes());
            }
            vertices.extend_from_slice(&uv);
            vertices.extend_from_slice(&0x00ff_ffffu32.to_le_bytes());
        }
        for value in &mut centroid {
            *value /= polygon_source.len() as f64;
        }
        face_source_centroids.push(centroid);

        faces.extend_from_slice(&(plane_index as u16).to_le_bytes());
        faces.extend_from_slice(&(first_vertex as u16).to_le_bytes());
        faces.extend_from_slice(&0u16.to_le_bytes());
        let backside = side ^ plane_flipped[plane_index];
        faces.push((FACE_BAKED_LIGHT | if backside { FACE_BACKSIDE } else { 0 }) as u8);
        faces.push(polygon_source.len() as u8);
        faces.extend_from_slice(&[0, 64]);
    }

    let source_nodes = lump(bytes, lumps[NODES]);
    let mut nodes = Vec::with_capacity(source_nodes.len() / 4);
    for (index, source) in source_nodes.chunks_exact(24).enumerate() {
        let plane = nonnegative_i32(i32_at(source, 0)?, "node plane")?;
        if plane >= plane_flipped.len() {
            return Err(error(format!("BSP29 node {index} has an invalid plane")));
        }
        let mut children = [i16_at(source, 4)?, i16_at(source, 6)?];
        if plane_flipped[plane] {
            children.swap(0, 1);
        }
        nodes.extend_from_slice(&(plane as u16).to_le_bytes());
        nodes.extend_from_slice(&children[0].to_le_bytes());
        nodes.extend_from_slice(&children[1].to_le_bytes());
    }

    let model = lump(bytes, lumps[MODELS])
        .get(..64)
        .ok_or_else(|| error("BSP29 has no world model"))?;
    let root_node =
        i16::try_from(i32_at(model, 36)?).map_err(|_| error("BSP29 world root exceeds i16"))?;
    if root_node < 0 {
        return Err(error("BSP29 world model has no render node"));
    }
    let visible_leaves = i16::try_from(i32_at(model, 52)?)
        .map_err(|_| error("BSP29 visible leaf count exceeds i16"))?;
    let world_first_face = nonnegative_i32(i32_at(model, 56)?, "world first face")?;
    let world_face_count = nonnegative_i32(i32_at(model, 60)?, "world face count")?;
    let world_face_end = world_first_face
        .checked_add(world_face_count)
        .ok_or_else(|| error("BSP29 world face range overflow"))?;
    if world_face_end > face_count {
        return Err(error("BSP29 world face range is out of bounds"));
    }

    let source_marks = lump(bytes, lumps[MARK_SURFACES]);
    let source_leaves = lump(bytes, lumps[LEAVES]);
    let mut leaf_marks = Vec::with_capacity(source_leaves.len() / 28);
    for (index, source) in source_leaves.chunks_exact(28).enumerate() {
        let first = usize::from(u16_at(source, 20)?);
        let count = usize::from(u16_at(source, 22)?);
        let end = first
            .checked_add(count)
            .ok_or_else(|| error("BSP29 leaf mark range overflow"))?;
        if end * 2 > source_marks.len() {
            return Err(error(format!("BSP29 leaf {index} marks are out of bounds")));
        }
        let mut marks = Vec::with_capacity(count);
        for mark in first..end {
            let face = u16_at(source_marks, mark * 2)?;
            if usize::from(face) >= face_count {
                return Err(error(format!("BSP29 leaf {index} marks an invalid face")));
            }
            marks.push(face);
        }
        leaf_marks.push(marks);
    }

    // Quake stores func_* geometry as additional models. The editor imports
    // those brushes as static geometry for this benchmark, so attach every
    // submodel face to the world leaf containing its centroid. The normal PVS
    // then exposes it from connected viewpoints without importing entities.
    for face in world_face_end..face_count {
        let leaf = source_point_leaf(bytes, &lumps, root_node, face_source_centroids[face])?;
        let marks = leaf_marks
            .get_mut(leaf)
            .ok_or_else(|| error("BSP29 submodel face resolved to an invalid leaf"))?;
        let face = face as u16;
        if !marks.contains(&face) {
            marks.push(face);
        }
    }

    let mut mark_surfaces = Vec::new();
    let mut leaves = Vec::with_capacity(source_leaves.len() / 2);
    for (index, source) in source_leaves.chunks_exact(28).enumerate() {
        let first = mark_surfaces.len() / 2;
        let marks = &leaf_marks[index];
        if first > u16::MAX as usize || marks.len() > u16::MAX as usize {
            return Err(error("BSP29 remapped leaf marks exceed u16"));
        }
        for mark in marks {
            mark_surfaces.extend_from_slice(&mark.to_le_bytes());
        }
        let contents = i32_at(source, 0)?;
        let contents = i8::try_from(contents)
            .map_err(|_| error(format!("BSP29 leaf {index} contents exceed i8")))?;
        leaves.push(contents as u8);
        leaves.push(0);
        leaves.extend_from_slice(&(marks.len() as u16).to_le_bytes());
        leaves.extend_from_slice(&i32_at(source, 4)?.to_le_bytes());
        leaves.extend_from_slice(&(first as u16).to_le_bytes());
        leaves.extend_from_slice(&[0, 0, 0, 64]);
    }

    let (mins, maxs) = transformed_model_bounds(model, scale)?;
    Ok(PackedBspGeometry {
        vertices,
        planes,
        faces,
        mark_surfaces,
        visibility: lump(bytes, lumps[VISIBILITY]).to_vec(),
        leaves,
        nodes,
        material_slots: vec![material],
        root_node,
        visible_leaves,
        mins,
        maxs,
    })
}

/// Import render topology plus a compact point-collision tree from the same
/// released BSP. The editable `.map` brushes remain the authoring source, but
/// rebuilding collision as a linear chain of every brush makes a full Quake
/// level unusably expensive on PS1. Quake's render BSP already classifies
/// every solid leaf, so its node tree is also an exact point hull. PSoXide's
/// three collision slots share that tree for this geometry-only benchmark.
///
/// This deliberately provides point collision, not Quake's stock 32x56 body
/// hull: the imported project uses PSoXide-sized actors and the source hull's
/// expansion would be many times too large after coordinate scaling.
pub fn import_quake_bsp29_world(
    bytes: &[u8],
    scale: f64,
    material: Option<ResourceId>,
) -> Result<(PackedBspGeometry, CompiledCollisionHulls), QuakeBsp29Error> {
    let geometry = import_quake_bsp29_geometry(bytes, scale, material)?;
    let lumps = parse_header(bytes)?;
    let source_planes = lump(bytes, lumps[PLANES]);
    let source_nodes = lump(bytes, lumps[NODES]);
    let source_leaves = lump(bytes, lumps[LEAVES]);
    let mut plane_flipped = Vec::with_capacity(source_planes.len() / 20);
    for (index, source) in source_planes.chunks_exact(20).enumerate() {
        let source_normal = [f32_at(source, 0)?, f32_at(source, 4)?, f32_at(source, 8)?];
        let mut normal = [
            f64::from(source_normal[0]),
            f64::from(source_normal[2]),
            -f64::from(source_normal[1]),
        ];
        let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
        if !length.is_finite() || length <= f64::EPSILON {
            return Err(error(format!("BSP29 plane {index} has an invalid normal")));
        }
        for value in &mut normal {
            *value /= length;
        }
        let distance = f64::from(f32_at(source, 12)?) * scale / length;
        let (_, flipped) = pack_normalized_plane(normal, distance)
            .ok_or_else(|| error(format!("BSP29 plane {index} cannot be packed")))?;
        plane_flipped.push(flipped);
    }

    let collision_child = |child: i16| -> Result<i16, QuakeBsp29Error> {
        if child >= 0 {
            return Ok(child);
        }
        let leaf = usize::try_from(-1 - i32::from(child))
            .map_err(|_| error("BSP29 collision leaf index overflow"))?;
        let source = record(source_leaves, leaf, 28, "leaf")?;
        i16::try_from(i32_at(source, 0)?)
            .map_err(|_| error(format!("BSP29 leaf {leaf} contents exceed i16")))
    };
    let mut clipnodes = Vec::with_capacity(source_nodes.len() / 4);
    for (index, source) in source_nodes.chunks_exact(24).enumerate() {
        let plane = nonnegative_i32(i32_at(source, 0)?, "node plane")?;
        if plane >= plane_flipped.len() {
            return Err(error(format!("BSP29 node {index} has an invalid plane")));
        }
        let mut children = [
            collision_child(i16_at(source, 4)?)?,
            collision_child(i16_at(source, 6)?)?,
        ];
        if plane_flipped[plane] {
            children.swap(0, 1);
        }
        clipnodes.extend_from_slice(&(plane as i16).to_le_bytes());
        clipnodes.extend_from_slice(&children[0].to_le_bytes());
        clipnodes.extend_from_slice(&children[1].to_le_bytes());
    }
    let root = geometry.root_node;
    let collision = CompiledCollisionHulls {
        planes: geometry.planes.clone(),
        clipnodes,
        head_nodes: vec![root, root, root],
    };
    Ok((geometry, collision))
}

fn parse_header(bytes: &[u8]) -> Result<[LumpRange; LUMP_COUNT], QuakeBsp29Error> {
    if bytes.len() < HEADER_BYTES {
        return Err(error("truncated BSP29 header"));
    }
    if i32_at(bytes, 0)? != BSP29_VERSION {
        return Err(error("unsupported Quake BSP version"));
    }
    let mut lumps = [LumpRange::default(); LUMP_COUNT];
    for (index, destination) in lumps.iter_mut().enumerate() {
        let offset = nonnegative_i32(i32_at(bytes, 4 + index * 8)?, "lump offset")?;
        let len = nonnegative_i32(i32_at(bytes, 8 + index * 8)?, "lump length")?;
        let end = offset
            .checked_add(len)
            .ok_or_else(|| error("BSP29 lump range overflow"))?;
        if end > bytes.len() {
            return Err(error(format!("BSP29 lump {index} is out of bounds")));
        }
        *destination = LumpRange { offset, len };
    }
    Ok(lumps)
}

fn lump(bytes: &[u8], range: LumpRange) -> &[u8] {
    &bytes[range.offset..range.offset + range.len]
}

fn require_aligned(len: usize, size: usize, label: &str) -> Result<(), QuakeBsp29Error> {
    if len % size == 0 {
        Ok(())
    } else {
        Err(error(format!(
            "BSP29 {label} lump is not {size}-byte aligned"
        )))
    }
}

fn source_face_vertices(
    bytes: &[u8],
    lumps: &[LumpRange; LUMP_COUNT],
    face: &[u8],
) -> Result<Vec<[f32; 3]>, QuakeBsp29Error> {
    let first = nonnegative_i32(i32_at(face, 4)?, "face first edge")?;
    let count = usize::from(u16_at(face, 8)?);
    let surface_edges = lump(bytes, lumps[SURFACE_EDGES]);
    let edges = lump(bytes, lumps[EDGES]);
    let vertices = lump(bytes, lumps[VERTICES]);
    let mut output = Vec::with_capacity(count);
    for offset in 0..count {
        let edge = i32_at(surface_edges, (first + offset) * 4)?;
        let edge_index = edge.unsigned_abs() as usize;
        let source_edge = record(edges, edge_index, 4, "edge")?;
        let vertex_index = usize::from(if edge >= 0 {
            u16_at(source_edge, 0)?
        } else {
            u16_at(source_edge, 2)?
        });
        let source_vertex = record(vertices, vertex_index, 12, "vertex")?;
        output.push([
            f32_at(source_vertex, 0)?,
            f32_at(source_vertex, 4)?,
            f32_at(source_vertex, 8)?,
        ]);
    }
    Ok(output)
}

fn source_point_leaf(
    bytes: &[u8],
    lumps: &[LumpRange; LUMP_COUNT],
    root: i16,
    point: [f64; 3],
) -> Result<usize, QuakeBsp29Error> {
    let nodes = lump(bytes, lumps[NODES]);
    let planes = lump(bytes, lumps[PLANES]);
    let mut child = root;
    while child >= 0 {
        let node = record(nodes, child as usize, 24, "node")?;
        let plane = record(
            planes,
            nonnegative_i32(i32_at(node, 0)?, "node plane")?,
            20,
            "plane",
        )?;
        let normal = [
            f64::from(f32_at(plane, 0)?),
            f64::from(f32_at(plane, 4)?),
            f64::from(f32_at(plane, 8)?),
        ];
        let distance = f64::from(f32_at(plane, 12)?);
        let side = normal[0] * point[0] + normal[1] * point[1] + normal[2] * point[2] < distance;
        child = i16_at(node, 4 + usize::from(side) * 2)?;
    }
    Ok((-1 - i32::from(child)) as usize)
}

fn transform_position(source: [f32; 3], scale: f64) -> Result<[i16; 3], QuakeBsp29Error> {
    let transformed = [source[0], source[2], -source[1]];
    let mut output = [0i16; 3];
    for axis in 0..3 {
        let value = f64::from(transformed[axis]) * scale;
        if !value.is_finite()
            || value.round() < f64::from(i16::MIN)
            || value.round() > f64::from(i16::MAX)
        {
            return Err(error(format!(
                "BSP29 vertex axis {axis} is out of i16 range"
            )));
        }
        output[axis] = value.round() as i16;
    }
    Ok(output)
}

fn planar_uv(position: [i16; 3], normal: [f64; 3]) -> [u8; 2] {
    let axis = (0..3)
        .max_by(|left, right| normal[*left].abs().total_cmp(&normal[*right].abs()))
        .unwrap_or(1);
    let coordinates = match axis {
        0 => [position[2], position[1]],
        1 => [position[0], position[2]],
        _ => [position[0], position[1]],
    };
    coordinates.map(|value| (i32::from(value) / 16).rem_euclid(256) as u8)
}

fn transformed_model_bounds(
    model: &[u8],
    scale: f64,
) -> Result<([i16; 3], [i16; 3]), QuakeBsp29Error> {
    let source_min = [f32_at(model, 0)?, f32_at(model, 4)?, f32_at(model, 8)?];
    let source_max = [f32_at(model, 12)?, f32_at(model, 16)?, f32_at(model, 20)?];
    let transformed = [
        transform_position([source_min[0], source_min[1], source_min[2]], scale)?,
        transform_position([source_max[0], source_max[1], source_max[2]], scale)?,
    ];
    let mins = [
        transformed[0][0].min(transformed[1][0]),
        transformed[0][1].min(transformed[1][1]),
        transformed[0][2].min(transformed[1][2]),
    ];
    let maxs = [
        transformed[0][0].max(transformed[1][0]),
        transformed[0][1].max(transformed[1][1]),
        transformed[0][2].max(transformed[1][2]),
    ];
    Ok((mins, maxs))
}

fn record<'a>(
    bytes: &'a [u8],
    index: usize,
    size: usize,
    label: &str,
) -> Result<&'a [u8], QuakeBsp29Error> {
    let offset = index
        .checked_mul(size)
        .ok_or_else(|| error(format!("BSP29 {label} offset overflow")))?;
    bytes
        .get(offset..offset + size)
        .ok_or_else(|| error(format!("BSP29 {label} index is out of bounds")))
}

fn i16_at(bytes: &[u8], offset: usize) -> Result<i16, QuakeBsp29Error> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| error("truncated BSP29 i16"))?;
    Ok(i16::from_le_bytes(value.try_into().unwrap()))
}

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16, QuakeBsp29Error> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| error("truncated BSP29 u16"))?;
    Ok(u16::from_le_bytes(value.try_into().unwrap()))
}

fn i32_at(bytes: &[u8], offset: usize) -> Result<i32, QuakeBsp29Error> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| error("truncated BSP29 i32"))?;
    Ok(i32::from_le_bytes(value.try_into().unwrap()))
}

fn f32_at(bytes: &[u8], offset: usize) -> Result<f32, QuakeBsp29Error> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| error("truncated BSP29 f32"))?;
    Ok(f32::from_le_bytes(value.try_into().unwrap()))
}

fn nonnegative_i32(value: i32, label: &str) -> Result<usize, QuakeBsp29Error> {
    usize::try_from(value).map_err(|_| error(format!("BSP29 {label} is negative")))
}

fn error(message: impl Into<String>) -> QuakeBsp29Error {
    QuakeBsp29Error(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_truncated_and_wrong_version_files() {
        assert!(import_quake_bsp29_geometry(&[], 4.0, None).is_err());
        let mut header = vec![0; HEADER_BYTES];
        header[..4].copy_from_slice(&28i32.to_le_bytes());
        assert!(strip_quake_bsp29_geometry(&header).is_err());
    }
}
