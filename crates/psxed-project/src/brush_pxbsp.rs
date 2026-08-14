//! Final PXBSP assembly for compiled brush-world records.

use crate::brush_collision_hulls::CompiledCollisionHulls;
use crate::brush_pack::PackedBspGeometry;
use crate::ResourceId;

use psx_bsp::pxbsp::{
    PxbspEntity, PxbspLumpKind, PxbspMaterial, PxbspMaterialError, PXBSP_DIRECTORY_ENTRY_BYTES,
    PXBSP_ENTITY_TABLE_HEADER_BYTES, PXBSP_HEADER_BYTES, PXBSP_LUMP_COUNT, PXBSP_MAGIC,
    PXBSP_VERSION,
};
use psx_bsp::CookedRecord;

const WORLD_COLLISION_HULLS: usize = 3;
const VERTEX_BYTES: usize = 12;
const PLANE_BYTES: usize = 14;
const FACE_BYTES: usize = 14;
const MARK_SURFACE_BYTES: usize = 2;
const LEAF_BYTES: usize = 26;
const NODE_BYTES: usize = 34;
const CLIPNODE_BYTES: usize = 6;

/// Entity base plus its class-specific, bounded payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PxbspEntityInput {
    pub entity: PxbspEntity,
    pub payload: Vec<u8>,
}

/// Non-geometry payloads resolved by the editor's asset and entity cook.
pub struct PxbspMapPayloads<'a> {
    /// Material records in first-seen slot order across world then submodels.
    pub materials: &'a [PxbspMaterial],
    pub entities: &'a [PxbspEntityInput],
    pub texture_data: &'a [u8],
    pub sound_data: &'a [u8],
    pub model_data: &'a [u8],
    pub strings: &'a [u8],
    pub streaming_index: &'a [u8],
}

/// One independently transformed brush BSP after model-local compilation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PxbspSubmodel {
    pub geometry: PackedBspGeometry,
    pub collision: CompiledCollisionHulls,
    pub origin: [i16; 3],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledPxbsp {
    pub bytes: Vec<u8>,
    pub resident_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PxbspBuildError {
    MaterialCount {
        expected: usize,
        found: usize,
    },
    CollisionHullCount {
        expected: usize,
        found: usize,
    },
    InvalidMaterial {
        index: usize,
        error: PxbspMaterialError,
    },
    InvalidReference(&'static str),
    MisalignedRecords(&'static str),
    LimitExceeded {
        kind: &'static str,
        count: usize,
        max: usize,
    },
}

/// Merge shared planes, pack the world model, and write all PXBSP lumps.
pub fn build_pxbsp(
    geometry: PackedBspGeometry,
    collision: CompiledCollisionHulls,
    payloads: PxbspMapPayloads<'_>,
) -> Result<CompiledPxbsp, PxbspBuildError> {
    build_pxbsp_with_submodels(geometry, collision, Vec::new(), payloads)
}

/// Merge model-local BSP tables, pack model 0 plus mover submodels, and write PXBSP.
pub fn build_pxbsp_with_submodels(
    mut geometry: PackedBspGeometry,
    collision: CompiledCollisionHulls,
    submodels: Vec<PxbspSubmodel>,
    payloads: PxbspMapPayloads<'_>,
) -> Result<CompiledPxbsp, PxbspBuildError> {
    validate_geometry_records(&geometry)?;
    validate_collision_records(&collision)?;
    validate_hull_count(&collision)?;
    for submodel in &submodels {
        validate_geometry_records(&submodel.geometry)?;
        validate_collision_records(&submodel.collision)?;
        validate_hull_count(&submodel.collision)?;
    }

    let expected_materials = pxbsp_material_slots(&geometry, &submodels);
    limit("materials", expected_materials.len(), i16::MAX as usize + 1)?;
    if payloads.materials.len() != expected_materials.len() {
        return Err(PxbspBuildError::MaterialCount {
            expected: expected_materials.len(),
            found: payloads.materials.len(),
        });
    }
    geometry.material_slots = expected_materials;

    let mut clipnodes = Vec::new();
    let world_collision_heads = append_collision(&mut geometry.planes, &mut clipnodes, &collision)?;
    let mut models = pack_brush_model(
        &geometry,
        [0; 3],
        geometry.root_node,
        &world_collision_heads,
        0,
    )?;
    for submodel in submodels {
        let model = append_geometry(&mut geometry, submodel.geometry)?;
        let collision_heads =
            append_collision(&mut geometry.planes, &mut clipnodes, &submodel.collision)?;
        models.extend_from_slice(&pack_brush_model(
            &model,
            submodel.origin,
            model.root_node,
            &collision_heads,
            model.first_face,
        )?);
    }
    let materials = pack_materials(payloads.materials)?;
    let entities = pack_entities(payloads.entities)?;

    let mut lumps: [Vec<u8>; PXBSP_LUMP_COUNT] = core::array::from_fn(|_| Vec::new());
    lumps[PxbspLumpKind::TextureData as usize].extend_from_slice(payloads.texture_data);
    lumps[PxbspLumpKind::SoundData as usize].extend_from_slice(payloads.sound_data);
    lumps[PxbspLumpKind::ModelData as usize].extend_from_slice(payloads.model_data);
    lumps[PxbspLumpKind::Vertices as usize] = geometry.vertices;
    lumps[PxbspLumpKind::Planes as usize] = geometry.planes;
    lumps[PxbspLumpKind::Materials as usize] = materials;
    lumps[PxbspLumpKind::Faces as usize] = geometry.faces;
    lumps[PxbspLumpKind::MarkSurfaces as usize] = geometry.mark_surfaces;
    lumps[PxbspLumpKind::Visibility as usize] = geometry.visibility;
    lumps[PxbspLumpKind::Leaves as usize] = geometry.leaves;
    lumps[PxbspLumpKind::Nodes as usize] = geometry.nodes;
    lumps[PxbspLumpKind::ClipNodes as usize] = clipnodes;
    lumps[PxbspLumpKind::Models as usize] = models;
    lumps[PxbspLumpKind::Strings as usize].extend_from_slice(payloads.strings);
    lumps[PxbspLumpKind::Entities as usize] = entities;
    lumps[PxbspLumpKind::StreamingIndex as usize].extend_from_slice(payloads.streaming_index);

    let resident_bytes = [
        PxbspLumpKind::ModelData,
        PxbspLumpKind::Vertices,
        PxbspLumpKind::Planes,
        PxbspLumpKind::Materials,
        PxbspLumpKind::Faces,
        PxbspLumpKind::MarkSurfaces,
        PxbspLumpKind::Visibility,
        PxbspLumpKind::Leaves,
        PxbspLumpKind::Nodes,
        PxbspLumpKind::ClipNodes,
        PxbspLumpKind::Models,
        PxbspLumpKind::Strings,
        PxbspLumpKind::Entities,
        PxbspLumpKind::StreamingIndex,
    ]
    .into_iter()
    .map(|kind| lumps[kind as usize].len())
    .sum();
    Ok(CompiledPxbsp {
        bytes: write_pxbsp(&lumps)?,
        resident_bytes,
    })
}

#[derive(Clone, Copy)]
struct AppendedGeometry {
    root_node: i16,
    visible_leaves: i16,
    mins: [i16; 3],
    maxs: [i16; 3],
    first_face: usize,
    face_count: usize,
}

trait GeometryModel {
    fn mins(&self) -> [i16; 3];
    fn maxs(&self) -> [i16; 3];
    fn visible_leaves(&self) -> i16;
    fn face_count(&self) -> usize;
}

impl GeometryModel for PackedBspGeometry {
    fn mins(&self) -> [i16; 3] {
        self.mins
    }

    fn maxs(&self) -> [i16; 3] {
        self.maxs
    }

    fn visible_leaves(&self) -> i16 {
        self.visible_leaves
    }

    fn face_count(&self) -> usize {
        self.faces.len() / FACE_BYTES
    }
}

impl GeometryModel for AppendedGeometry {
    fn mins(&self) -> [i16; 3] {
        self.mins
    }

    fn maxs(&self) -> [i16; 3] {
        self.maxs
    }

    fn visible_leaves(&self) -> i16 {
        self.visible_leaves
    }

    fn face_count(&self) -> usize {
        self.face_count
    }
}

/// Return the material resolution order consumed by a multi-model PXBSP cook.
pub fn pxbsp_material_slots(
    world: &PackedBspGeometry,
    submodels: &[PxbspSubmodel],
) -> Vec<Option<ResourceId>> {
    let mut slots = world.material_slots.clone();
    for submodel in submodels {
        for &slot in &submodel.geometry.material_slots {
            if !slots.contains(&slot) {
                slots.push(slot);
            }
        }
    }
    slots
}

fn validate_hull_count(collision: &CompiledCollisionHulls) -> Result<(), PxbspBuildError> {
    if collision.head_nodes.len() != WORLD_COLLISION_HULLS {
        Err(PxbspBuildError::CollisionHullCount {
            expected: WORLD_COLLISION_HULLS,
            found: collision.head_nodes.len(),
        })
    } else {
        Ok(())
    }
}

fn validate_geometry_records(geometry: &PackedBspGeometry) -> Result<(), PxbspBuildError> {
    for (bytes, size, name) in [
        (geometry.vertices.len(), VERTEX_BYTES, "vertices"),
        (geometry.planes.len(), PLANE_BYTES, "render planes"),
        (geometry.faces.len(), FACE_BYTES, "faces"),
        (
            geometry.mark_surfaces.len(),
            MARK_SURFACE_BYTES,
            "mark surfaces",
        ),
        (geometry.leaves.len(), LEAF_BYTES, "leaves"),
        (geometry.nodes.len(), NODE_BYTES, "nodes"),
    ] {
        if !bytes.is_multiple_of(size) {
            return Err(PxbspBuildError::MisalignedRecords(name));
        }
    }
    Ok(())
}

fn validate_collision_records(collision: &CompiledCollisionHulls) -> Result<(), PxbspBuildError> {
    if !collision.planes.len().is_multiple_of(PLANE_BYTES) {
        return Err(PxbspBuildError::MisalignedRecords("collision planes"));
    }
    if !collision.clipnodes.len().is_multiple_of(CLIPNODE_BYTES) {
        return Err(PxbspBuildError::MisalignedRecords("clipnodes"));
    }
    Ok(())
}

fn append_geometry(
    output: &mut PackedBspGeometry,
    input: PackedBspGeometry,
) -> Result<AppendedGeometry, PxbspBuildError> {
    let vertex_base = output.vertices.len() / VERTEX_BYTES;
    let face_base = output.faces.len() / FACE_BYTES;
    let mark_base = output.mark_surfaces.len() / MARK_SURFACE_BYTES;
    let visibility_base = output.visibility.len();
    let leaf_base = output.leaves.len() / LEAF_BYTES;
    let node_base = output.nodes.len() / NODE_BYTES;

    let plane_remap = append_unique_records(
        &mut output.planes,
        &input.planes,
        PLANE_BYTES,
        "planes",
        i16::MAX as usize + 1,
    )?;
    let material_remap: Vec<_> = input
        .material_slots
        .iter()
        .map(|slot| {
            output
                .material_slots
                .iter()
                .position(|candidate| candidate == slot)
                .ok_or(PxbspBuildError::InvalidReference("face material"))
        })
        .collect::<Result<_, _>>()?;
    limit(
        "materials",
        output.material_slots.len(),
        i16::MAX as usize + 1,
    )?;

    let vertex_count = input.vertices.len() / VERTEX_BYTES;
    limit(
        "vertices",
        vertex_base.saturating_add(vertex_count),
        i32::MAX as usize,
    )?;
    output.vertices.extend_from_slice(&input.vertices);

    let face_count = input.faces.len() / FACE_BYTES;
    limit(
        "faces",
        face_base.saturating_add(face_count),
        u16::MAX as usize + 1,
    )?;
    for face in input.faces.chunks_exact(FACE_BYTES) {
        let plane = read_i16(face, 0);
        let plane = remap_signed_index(plane, &plane_remap, "face plane")?;
        let first_vertex = read_i32(face, 4);
        let first_vertex = usize::try_from(first_vertex)
            .ok()
            .and_then(|value| value.checked_add(vertex_base))
            .and_then(|value| i32::try_from(value).ok())
            .ok_or(PxbspBuildError::InvalidReference("face vertex"))?;
        let material = read_i16(face, 10);
        let material = remap_signed_index(material, &material_remap, "face material")?;
        let mut remapped = face.to_vec();
        remapped[0..2].copy_from_slice(&plane.to_le_bytes());
        remapped[4..8].copy_from_slice(&first_vertex.to_le_bytes());
        remapped[10..12].copy_from_slice(&material.to_le_bytes());
        output.faces.extend_from_slice(&remapped);
    }

    let mark_count = input.mark_surfaces.len() / MARK_SURFACE_BYTES;
    limit(
        "mark surfaces",
        mark_base.saturating_add(mark_count),
        u16::MAX as usize + 1,
    )?;
    for mark in input.mark_surfaces.chunks_exact(MARK_SURFACE_BYTES) {
        let face = read_u16(mark, 0) as usize;
        if face >= face_count {
            return Err(PxbspBuildError::InvalidReference("mark surface face"));
        }
        let remapped = face_base
            .checked_add(face)
            .and_then(|value| u16::try_from(value).ok())
            .ok_or(PxbspBuildError::InvalidReference("mark surface face"))?;
        push_u16(&mut output.mark_surfaces, remapped);
    }

    let visibility_end = visibility_base.checked_add(input.visibility.len()).ok_or(
        PxbspBuildError::LimitExceeded {
            kind: "visibility",
            count: usize::MAX,
            max: i32::MAX as usize,
        },
    )?;
    limit("visibility", visibility_end, i32::MAX as usize)?;
    output.visibility.extend_from_slice(&input.visibility);

    let leaf_count = input.leaves.len() / LEAF_BYTES;
    limit(
        "leaves",
        leaf_base.saturating_add(leaf_count),
        i16::MAX as usize + 1,
    )?;
    for leaf in input.leaves.chunks_exact(LEAF_BYTES) {
        let visibility = read_i32(leaf, 2);
        let visibility = if visibility < 0 {
            visibility
        } else {
            usize::try_from(visibility)
                .ok()
                .filter(|&value| value < input.visibility.len())
                .and_then(|value| value.checked_add(visibility_base))
                .and_then(|value| i32::try_from(value).ok())
                .ok_or(PxbspBuildError::InvalidReference("leaf visibility"))?
        };
        let first_mark = read_u16(leaf, 18) as usize;
        let mark_count = read_u16(leaf, 20) as usize;
        if first_mark.saturating_add(mark_count) > input.mark_surfaces.len() / MARK_SURFACE_BYTES {
            return Err(PxbspBuildError::InvalidReference("leaf mark surfaces"));
        }
        let first_mark = mark_base
            .checked_add(first_mark)
            .and_then(|value| u16::try_from(value).ok())
            .ok_or(PxbspBuildError::InvalidReference("leaf mark surfaces"))?;
        let mut remapped = leaf.to_vec();
        remapped[2..6].copy_from_slice(&visibility.to_le_bytes());
        remapped[18..20].copy_from_slice(&first_mark.to_le_bytes());
        output.leaves.extend_from_slice(&remapped);
    }

    let node_count = input.nodes.len() / NODE_BYTES;
    limit(
        "nodes",
        node_base.saturating_add(node_count),
        i16::MAX as usize + 1,
    )?;
    for node in input.nodes.chunks_exact(NODE_BYTES) {
        let plane = read_u16(node, 0) as usize;
        let plane = plane_remap
            .get(plane)
            .copied()
            .and_then(|value| u16::try_from(value).ok())
            .ok_or(PxbspBuildError::InvalidReference("node plane"))?;
        let front = remap_render_child(
            read_i16(node, 2),
            node_base,
            leaf_base,
            node_count,
            leaf_count,
        )?;
        let back = remap_render_child(
            read_i16(node, 4),
            node_base,
            leaf_base,
            node_count,
            leaf_count,
        )?;
        let first_face = read_u16(node, 30) as usize;
        let local_face_count = read_u16(node, 32) as usize;
        if first_face.saturating_add(local_face_count) > face_count {
            return Err(PxbspBuildError::InvalidReference("node faces"));
        }
        let first_face = face_base
            .checked_add(first_face)
            .and_then(|value| u16::try_from(value).ok())
            .ok_or(PxbspBuildError::InvalidReference("node faces"))?;
        let mut remapped = node.to_vec();
        remapped[0..2].copy_from_slice(&plane.to_le_bytes());
        remapped[2..4].copy_from_slice(&front.to_le_bytes());
        remapped[4..6].copy_from_slice(&back.to_le_bytes());
        remapped[30..32].copy_from_slice(&first_face.to_le_bytes());
        output.nodes.extend_from_slice(&remapped);
    }

    let root_node = usize::try_from(input.root_node)
        .ok()
        .filter(|&value| value < node_count)
        .and_then(|value| value.checked_add(node_base))
        .and_then(|value| i16::try_from(value).ok())
        .ok_or(PxbspBuildError::InvalidReference("model render head"))?;
    Ok(AppendedGeometry {
        root_node,
        visible_leaves: input.visible_leaves,
        mins: input.mins,
        maxs: input.maxs,
        first_face: face_base,
        face_count,
    })
}

fn append_unique_records(
    output: &mut Vec<u8>,
    input: &[u8],
    record_bytes: usize,
    kind: &'static str,
    max: usize,
) -> Result<Vec<usize>, PxbspBuildError> {
    let mut remap = Vec::with_capacity(input.len() / record_bytes);
    for record in input.chunks_exact(record_bytes) {
        let index = output
            .chunks_exact(record_bytes)
            .position(|existing| existing == record)
            .unwrap_or_else(|| {
                let index = output.len() / record_bytes;
                output.extend_from_slice(record);
                index
            });
        limit(kind, index + 1, max)?;
        remap.push(index);
    }
    Ok(remap)
}

fn remap_signed_index(
    index: i16,
    remap: &[usize],
    kind: &'static str,
) -> Result<i16, PxbspBuildError> {
    usize::try_from(index)
        .ok()
        .and_then(|index| remap.get(index).copied())
        .and_then(|index| i16::try_from(index).ok())
        .ok_or(PxbspBuildError::InvalidReference(kind))
}

fn remap_render_child(
    child: i16,
    node_base: usize,
    leaf_base: usize,
    node_count: usize,
    leaf_count: usize,
) -> Result<i16, PxbspBuildError> {
    if child >= 0 {
        let child = child as usize;
        if child >= node_count {
            return Err(PxbspBuildError::InvalidReference("node child"));
        }
        node_base
            .checked_add(child)
            .and_then(|value| i16::try_from(value).ok())
            .ok_or(PxbspBuildError::InvalidReference("node child"))
    } else {
        let child = (-1i32 - child as i32) as usize;
        if child >= leaf_count {
            return Err(PxbspBuildError::InvalidReference("node leaf"));
        }
        let child = leaf_base
            .checked_add(child)
            .ok_or(PxbspBuildError::InvalidReference("node leaf"))?;
        i16::try_from(-1i32 - child as i32)
            .map_err(|_| PxbspBuildError::InvalidReference("node leaf"))
    }
}

fn remap_clip_child(
    child: i16,
    node_base: usize,
    node_count: usize,
) -> Result<i16, PxbspBuildError> {
    if child < 0 {
        return Ok(child);
    }
    let child = child as usize;
    if child >= node_count {
        return Err(PxbspBuildError::InvalidReference("clipnode child"));
    }
    node_base
        .checked_add(child)
        .and_then(|value| i16::try_from(value).ok())
        .ok_or(PxbspBuildError::InvalidReference("clipnode child"))
}

fn remap_clip_head(head: i16, node_base: usize, node_count: usize) -> Result<i16, PxbspBuildError> {
    if head < 0 || head as usize >= node_count {
        return Err(PxbspBuildError::InvalidReference("model clip head"));
    }
    node_base
        .checked_add(head as usize)
        .and_then(|value| i16::try_from(value).ok())
        .ok_or(PxbspBuildError::InvalidReference("model clip head"))
}

fn append_collision(
    render_planes: &mut Vec<u8>,
    output: &mut Vec<u8>,
    collision: &CompiledCollisionHulls,
) -> Result<Vec<i16>, PxbspBuildError> {
    let remap = append_unique_records(
        render_planes,
        &collision.planes,
        PLANE_BYTES,
        "planes",
        i16::MAX as usize + 1,
    )?;
    let node_base = output.len() / CLIPNODE_BYTES;
    let node_count = collision.clipnodes.len() / CLIPNODE_BYTES;
    limit(
        "clipnodes",
        node_base.saturating_add(node_count),
        i16::MAX as usize + 1,
    )?;
    for node in collision.clipnodes.chunks_exact(CLIPNODE_BYTES) {
        let plane = remap_signed_index(read_i16(node, 0), &remap, "clipnode plane")?;
        push_i16(output, plane);
        for offset in [2, 4] {
            push_i16(
                output,
                remap_clip_child(read_i16(node, offset), node_base, node_count)?,
            );
        }
    }
    collision
        .head_nodes
        .iter()
        .copied()
        .map(|head| remap_clip_head(head, node_base, node_count))
        .collect()
}

fn pack_brush_model(
    geometry: &impl GeometryModel,
    origin: [i16; 3],
    render_head: i16,
    collision_heads: &[i16],
    first_face: usize,
) -> Result<Vec<u8>, PxbspBuildError> {
    let face_count = geometry.face_count();
    limit("model first face", first_face, u16::MAX as usize)?;
    limit("model faces", face_count, u16::MAX as usize)?;
    let mut output = Vec::with_capacity(32);
    pack_vec3_i16(&mut output, geometry.mins());
    pack_vec3_i16(&mut output, geometry.maxs());
    pack_vec3_i16(&mut output, origin);
    push_i16(&mut output, render_head);
    for &head in collision_heads {
        push_i16(&mut output, head);
    }
    push_i16(&mut output, geometry.visible_leaves());
    push_u16(&mut output, first_face as u16);
    push_u16(&mut output, face_count as u16);
    Ok(output)
}

fn pack_materials(materials: &[PxbspMaterial]) -> Result<Vec<u8>, PxbspBuildError> {
    let mut output = Vec::with_capacity(materials.len() * PxbspMaterial::SIZE);
    for (index, material) in materials.iter().enumerate() {
        material
            .validate()
            .map_err(|error| PxbspBuildError::InvalidMaterial { index, error })?;
        push_u16(&mut output, material.texture_asset);
        push_u16(&mut output, material.flags);
        output.extend_from_slice(&material.tint);
        output.push(material.blend_mode);
        output.push(material.animation_kind);
        output.extend_from_slice(&material.animation_data);
    }
    Ok(output)
}

fn pack_entities(entities: &[PxbspEntityInput]) -> Result<Vec<u8>, PxbspBuildError> {
    limit("entities", entities.len(), u16::MAX as usize)?;
    let record_bytes =
        entities
            .len()
            .checked_mul(PxbspEntity::SIZE)
            .ok_or(PxbspBuildError::LimitExceeded {
                kind: "entity records",
                count: usize::MAX,
                max: u32::MAX as usize,
            })?;
    let records_end = PXBSP_ENTITY_TABLE_HEADER_BYTES
        .checked_add(record_bytes)
        .ok_or(PxbspBuildError::LimitExceeded {
            kind: "entity table",
            count: usize::MAX,
            max: u32::MAX as usize,
        })?;
    let payload_start = align_up_4(records_end);
    limit("entity table", payload_start, u32::MAX as usize)?;
    let payload_bytes = entities.iter().try_fold(0usize, |total, entity| {
        total.checked_add(entity.payload.len())
    });
    let Some(payload_bytes) = payload_bytes else {
        return Err(PxbspBuildError::LimitExceeded {
            kind: "entity payload",
            count: usize::MAX,
            max: u32::MAX as usize,
        });
    };
    limit("entity payload", payload_bytes, u32::MAX as usize)?;
    let entity_lump_bytes =
        payload_start
            .checked_add(payload_bytes)
            .ok_or(PxbspBuildError::LimitExceeded {
                kind: "entity lump",
                count: usize::MAX,
                max: u32::MAX as usize,
            })?;
    limit("entity lump", entity_lump_bytes, u32::MAX as usize)?;
    let mut output = vec![0; payload_start];
    output[0..2].copy_from_slice(&(entities.len() as u16).to_le_bytes());
    output[2..4].copy_from_slice(&(PxbspEntity::SIZE as u16).to_le_bytes());
    output[4..8].copy_from_slice(&(payload_start as u32).to_le_bytes());
    let mut payload_offset = 0usize;
    for (index, input) in entities.iter().enumerate() {
        limit(
            "entity payload record",
            input.payload.len(),
            u16::MAX as usize,
        )?;
        let start = PXBSP_ENTITY_TABLE_HEADER_BYTES + index * PxbspEntity::SIZE;
        let mut entity = input.entity;
        entity.payload_offset = payload_offset as u32;
        entity.payload_size = input.payload.len() as u16;
        pack_entity_record(&mut output[start..start + PxbspEntity::SIZE], entity);
        payload_offset += input.payload.len();
        output.extend_from_slice(&input.payload);
    }
    Ok(output)
}

fn pack_entity_record(output: &mut [u8], entity: PxbspEntity) {
    output[0..2].copy_from_slice(&entity.class_id.to_le_bytes());
    output[2..4].copy_from_slice(&entity.flags.to_le_bytes());
    output[4..6].copy_from_slice(&entity.model.to_le_bytes());
    output[6..8].copy_from_slice(&entity.leaf.to_le_bytes());
    output[8..12].copy_from_slice(&entity.origin.x.to_le_bytes());
    output[12..16].copy_from_slice(&entity.origin.y.to_le_bytes());
    output[16..20].copy_from_slice(&entity.origin.z.to_le_bytes());
    output[20..22].copy_from_slice(&entity.angles.x.to_le_bytes());
    output[22..24].copy_from_slice(&entity.angles.y.to_le_bytes());
    output[24..26].copy_from_slice(&entity.angles.z.to_le_bytes());
    output[26..30].copy_from_slice(&entity.payload_offset.to_le_bytes());
    output[30..32].copy_from_slice(&entity.payload_size.to_le_bytes());
}

fn write_pxbsp(lumps: &[Vec<u8>; PXBSP_LUMP_COUNT]) -> Result<Vec<u8>, PxbspBuildError> {
    let directory_end =
        PXBSP_HEADER_BYTES as usize + PXBSP_DIRECTORY_ENTRY_BYTES as usize * PXBSP_LUMP_COUNT;
    let mut output = vec![0; directory_end];
    output[0..4].copy_from_slice(&PXBSP_MAGIC.to_le_bytes());
    output[4..6].copy_from_slice(&PXBSP_VERSION.to_le_bytes());
    output[6..8].copy_from_slice(&(PXBSP_LUMP_COUNT as u16).to_le_bytes());
    for (index, kind) in PxbspLumpKind::ALL.into_iter().enumerate() {
        let aligned = align_up_4(output.len());
        output.resize(aligned, 0);
        let offset = output.len();
        let bytes = &lumps[index];
        limit("PXBSP offset", offset, u32::MAX as usize)?;
        limit("PXBSP lump", bytes.len(), u32::MAX as usize)?;
        output.extend_from_slice(bytes);
        let entry = PXBSP_HEADER_BYTES as usize + index * PXBSP_DIRECTORY_ENTRY_BYTES as usize;
        output[entry..entry + 2].copy_from_slice(&(kind as u16).to_le_bytes());
        output[entry + 4..entry + 8].copy_from_slice(&(offset as u32).to_le_bytes());
        output[entry + 8..entry + 12].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
    }
    limit("PXBSP file", output.len(), u32::MAX as usize)?;
    Ok(output)
}

fn limit(kind: &'static str, count: usize, max: usize) -> Result<(), PxbspBuildError> {
    if count > max {
        Err(PxbspBuildError::LimitExceeded { kind, count, max })
    } else {
        Ok(())
    }
}

const fn align_up_4(value: usize) -> usize {
    (value + 3) & !3
}

fn pack_vec3_i16(output: &mut Vec<u8>, value: [i16; 3]) {
    for component in value {
        push_i16(output, component);
    }
}

fn read_i16(bytes: &[u8], offset: usize) -> i16 {
    i16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_i32(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn push_i16(output: &mut Vec<u8>, value: i16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::Brush;
    use crate::brush_collision_hulls::{compile_collision_hulls, CollisionHullBounds};
    use crate::brush_compile::{build_surface_bsp, compile_csg_surfaces};
    use crate::brush_pack::{pack_bsp_geometry, BspLighting};
    use crate::brush_portal::{classify_bsp_leaves, portalize_surface_bsp};
    use psx_bsp::collision::{CollisionHull, Trace, TraceScratch};
    use psx_bsp::pxbsp::{PxbspEntityTable, PxbspIndex};
    use psx_bsp::pxbsp_resident::PxbspResidentMap;
    use psx_bsp::{BrushModel, ClipNode, Plane, RecordSlice, SliceReader, Vec3I16, Vec3I32};

    const PLAYER: CollisionHullBounds = CollisionHullBounds {
        mins: [-16, 0, -16],
        maxs: [16, 56, 16],
    };
    const BIG: CollisionHullBounds = CollisionHullBounds {
        mins: [-32, 0, -32],
        maxs: [32, 96, 32],
    };

    fn trace(hull: &CollisionHull<'_>, start: Vec3I32, end: Vec3I32) -> Trace {
        let mut output = Trace::default();
        assert!(hull.trace_into(&start, &end, &mut TraceScratch::new(), &mut output,));
        output
    }

    fn compiled_room() -> CompiledPxbsp {
        let brushes = Brush::cuboid([0, 0, 0], [1024, 512, 1024])
            .hollow(64)
            .expect("room");
        let surfaces = compile_csg_surfaces(&brushes);
        let mut bsp = build_surface_bsp(&surfaces);
        let portals = portalize_surface_bsp(&bsp);
        classify_bsp_leaves(&mut bsp, &portals, &brushes);
        let geometry =
            pack_bsp_geometry(&bsp, &portals, BspLighting::Fullbright, &Default::default())
                .expect("geometry");
        let collision =
            compile_collision_hulls(&brushes, &[CollisionHullBounds::POINT, PLAYER, BIG])
                .expect("collision");
        let materials = [PxbspMaterial {
            texture_asset: 7,
            tint: [128; 3],
            ..PxbspMaterial::default()
        }];
        let entities = [PxbspEntityInput {
            entity: PxbspEntity {
                class_id: 1,
                model: u16::MAX,
                leaf: 1,
                origin: Vec3I32 {
                    x: 512 * 4096,
                    y: 64 * 4096,
                    z: 512 * 4096,
                },
                angles: Vec3I16::default(),
                ..PxbspEntity::default()
            },
            payload: vec![60, 0],
        }];
        build_pxbsp(
            geometry,
            collision,
            PxbspMapPayloads {
                materials: &materials,
                entities: &entities,
                texture_data: &[],
                sound_data: &[],
                model_data: &[],
                strings: b"world\0player\0",
                streaming_index: &[],
            },
        )
        .expect("PXBSP")
    }

    fn compiled_geometry(brushes: &[Brush]) -> (PackedBspGeometry, CompiledCollisionHulls) {
        let surfaces = compile_csg_surfaces(brushes);
        let mut bsp = build_surface_bsp(&surfaces);
        let portals = portalize_surface_bsp(&bsp);
        classify_bsp_leaves(&mut bsp, &portals, brushes);
        let geometry =
            pack_bsp_geometry(&bsp, &portals, BspLighting::Fullbright, &Default::default())
                .expect("geometry");
        let collision =
            compile_collision_hulls(brushes, &[CollisionHullBounds::POINT, PLAYER, BIG])
                .expect("collision");
        (geometry, collision)
    }

    #[test]
    fn complete_map_round_trips_shared_pxbsp_index() {
        let compiled = compiled_room();
        let mut reader = SliceReader::new(&compiled.bytes);
        let index = PxbspIndex::read(&mut reader).expect("index");
        assert_eq!(index.file_len(), compiled.bytes.len() as u32);
        assert!(index.lump(PxbspLumpKind::Vertices).len > 0);
        assert!(index.lump(PxbspLumpKind::ClipNodes).len > 0);
        assert_eq!(index.lump(PxbspLumpKind::Materials).len, 16);
        let entities = index.lump(PxbspLumpKind::Entities);
        let table = PxbspEntityTable::new(
            &compiled.bytes[entities.offset as usize..entities.end() as usize],
        )
        .expect("entities");
        assert_eq!(table.len(), 1);
        assert_eq!(table.payload(0), Some(&[60, 0][..]));
    }

    #[test]
    fn world_model_points_at_render_and_three_collision_hulls() {
        let compiled = compiled_room();
        let index = PxbspIndex::read(&mut SliceReader::new(&compiled.bytes)).expect("index");
        let range = index.lump(PxbspLumpKind::Models);
        let models = RecordSlice::<BrushModel>::new(
            &compiled.bytes[range.offset as usize..range.end() as usize],
        )
        .expect("models");
        let world = models.get(0).expect("world");
        assert!(world.head_nodes.into_iter().all(|head| head >= 0));
        assert!(world.visible_leaves > 0);
        assert!(world.face_count > 0);
    }

    #[test]
    fn merged_player_hull_traces_room_floor() {
        let compiled = compiled_room();
        let index = PxbspIndex::read(&mut SliceReader::new(&compiled.bytes)).expect("index");
        let bytes = |kind| {
            let range = index.lump(kind);
            &compiled.bytes[range.offset as usize..range.end() as usize]
        };
        let world = RecordSlice::<BrushModel>::new(bytes(PxbspLumpKind::Models))
            .expect("models")
            .get(0)
            .expect("world");
        let hull = CollisionHull::new(
            RecordSlice::<Plane>::new(bytes(PxbspLumpKind::Planes)).expect("planes"),
            RecordSlice::<ClipNode>::new(bytes(PxbspLumpKind::ClipNodes)).expect("clipnodes"),
            world.head_nodes[2],
        );
        let trace = trace(
            &hull,
            Vec3I32 {
                x: 512 * 4096,
                y: 256 * 4096,
                z: 512 * 4096,
            },
            Vec3I32 {
                x: 512 * 4096,
                y: -64 * 4096,
                z: 512 * 4096,
            },
        );
        let end_y = trace.end.y as f64 / 4096.0;
        assert!((63.0..64.1).contains(&end_y));
    }

    #[test]
    fn complete_map_build_is_deterministic() {
        assert_eq!(compiled_room(), compiled_room());
    }

    #[test]
    fn submodel_tables_are_remapped_and_validate_as_one_resident_map() {
        let world_brushes = Brush::cuboid([0, 0, 0], [1024, 512, 1024])
            .hollow(64)
            .expect("room");
        let (world_geometry, world_collision) = compiled_geometry(&world_brushes);
        let world_face_count = world_geometry.faces.len() / FACE_BYTES;
        let world_node_count = world_geometry.nodes.len() / NODE_BYTES;
        let world_leaf_count = world_geometry.leaves.len() / LEAF_BYTES;
        let mut door_brush = Brush::cuboid([-32, 0, -8], [32, 96, 8]);
        for face in &mut door_brush.faces {
            face.material = Some(ResourceId(91));
        }
        let door_brushes = [door_brush];
        let (door_geometry, door_collision) = compiled_geometry(&door_brushes);
        let door_face_count = door_geometry.faces.len() / FACE_BYTES;
        let materials = [
            PxbspMaterial {
                texture_asset: 7,
                ..PxbspMaterial::default()
            },
            PxbspMaterial {
                texture_asset: 9,
                ..PxbspMaterial::default()
            },
        ];

        let compiled = build_pxbsp_with_submodels(
            world_geometry,
            world_collision,
            vec![PxbspSubmodel {
                geometry: door_geometry,
                collision: door_collision,
                origin: [512, 64, 512],
            }],
            PxbspMapPayloads {
                materials: &materials,
                entities: &[],
                texture_data: &[],
                sound_data: &[],
                model_data: &[],
                strings: &[],
                streaming_index: &[],
            },
        )
        .expect("PXBSP with door");

        let mut map = PxbspResidentMap::with_capacity(compiled.bytes.len());
        map.load(5, &mut SliceReader::new(&compiled.bytes))
            .expect("resident map validates all remapped references");
        let models = map.brush_models();
        assert_eq!(models.len(), 2);
        let door = models.get(1).expect("door");
        assert_eq!(
            door.origin,
            Vec3I16 {
                x: 512,
                y: 64,
                z: 512
            }
        );
        assert_eq!(door.first_face as usize, world_face_count);
        assert_eq!(door.face_count as usize, door_face_count);
        assert!(door.head_nodes[0] as usize >= world_node_count);
        assert_eq!(map.materials().len(), 2);
        assert!(map
            .faces()
            .iter()
            .skip(door.first_face as usize)
            .take(door.face_count as usize)
            .all(|face| face.texture == 1));
        assert!(map.nodes().iter().skip(world_node_count).any(|node| {
            node.children
                .into_iter()
                .any(|child| child < 0 && (-1i32 - child as i32) as usize >= world_leaf_count)
        }));

        let door_hull = CollisionHull::new(map.planes(), map.clip_nodes(), door.head_nodes[1]);
        let trace = trace(
            &door_hull,
            Vec3I32 {
                x: 0,
                y: 48 * 4096,
                z: 32 * 4096,
            },
            Vec3I32 {
                x: 0,
                y: 48 * 4096,
                z: -32 * 4096,
            },
        );
        let end_z = trace.end.z as f64 / 4096.0;
        assert!((7.9..8.1).contains(&end_z), "end z was {end_z}");
    }

    #[test]
    fn invalid_material_recipe_is_rejected_before_write() {
        let brushes = Brush::cuboid([0, 0, 0], [1024, 512, 1024])
            .hollow(64)
            .expect("room");
        let surfaces = compile_csg_surfaces(&brushes);
        let mut bsp = build_surface_bsp(&surfaces);
        let portals = portalize_surface_bsp(&bsp);
        classify_bsp_leaves(&mut bsp, &portals, &brushes);
        let geometry =
            pack_bsp_geometry(&bsp, &portals, BspLighting::Fullbright, &Default::default())
                .expect("geometry");
        let collision =
            compile_collision_hulls(&brushes, &[CollisionHullBounds::POINT, PLAYER, BIG])
                .expect("collision");
        let materials = [PxbspMaterial {
            blend_mode: 9,
            ..PxbspMaterial::default()
        }];
        let error = build_pxbsp(
            geometry,
            collision,
            PxbspMapPayloads {
                materials: &materials,
                entities: &[],
                texture_data: &[],
                sound_data: &[],
                model_data: &[],
                strings: &[],
                streaming_index: &[],
            },
        )
        .expect_err("invalid material");
        assert_eq!(
            error,
            PxbspBuildError::InvalidMaterial {
                index: 0,
                error: PxbspMaterialError::InvalidBlendMode(9),
            }
        );
    }
}
