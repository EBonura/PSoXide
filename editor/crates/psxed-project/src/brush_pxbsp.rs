//! Final PXBSP assembly for compiled brush-world records.

use crate::brush_collision_hulls::CompiledCollisionHulls;
use crate::brush_pack::PackedBspGeometry;

use psx_bsp::pxbsp::{
    PxbspEntity, PxbspLumpKind, PxbspMaterial, PXBSP_DIRECTORY_ENTRY_BYTES,
    PXBSP_ENTITY_TABLE_HEADER_BYTES, PXBSP_HEADER_BYTES, PXBSP_LUMP_COUNT, PXBSP_MAGIC,
    PXBSP_VERSION,
};
use psx_bsp::CookedRecord;

const WORLD_COLLISION_HULLS: usize = 3;

/// Entity base plus its class-specific, bounded payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PxbspEntityInput {
    pub entity: PxbspEntity,
    pub payload: Vec<u8>,
}

/// Non-geometry payloads resolved by the editor's asset and entity cook.
pub struct PxbspMapPayloads<'a> {
    /// Material records in `PackedBspGeometry::material_slots` order.
    pub materials: &'a [PxbspMaterial],
    pub entities: &'a [PxbspEntityInput],
    pub texture_data: &'a [u8],
    pub sound_data: &'a [u8],
    pub model_data: &'a [u8],
    pub strings: &'a [u8],
    pub streaming_index: &'a [u8],
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
    MisalignedRecords(&'static str),
    LimitExceeded {
        kind: &'static str,
        count: usize,
        max: usize,
    },
}

/// Merge shared planes, pack the world model, and write all PXBSP lumps.
pub fn build_pxbsp(
    mut geometry: PackedBspGeometry,
    collision: CompiledCollisionHulls,
    payloads: PxbspMapPayloads<'_>,
) -> Result<CompiledPxbsp, PxbspBuildError> {
    if payloads.materials.len() != geometry.material_slots.len() {
        return Err(PxbspBuildError::MaterialCount {
            expected: geometry.material_slots.len(),
            found: payloads.materials.len(),
        });
    }
    if collision.head_nodes.len() != WORLD_COLLISION_HULLS {
        return Err(PxbspBuildError::CollisionHullCount {
            expected: WORLD_COLLISION_HULLS,
            found: collision.head_nodes.len(),
        });
    }
    let clipnodes = merge_collision_planes(&mut geometry.planes, &collision)?;
    let models = pack_world_model(&geometry, &collision.head_nodes);
    let materials = pack_materials(payloads.materials);
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

fn merge_collision_planes(
    render_planes: &mut Vec<u8>,
    collision: &CompiledCollisionHulls,
) -> Result<Vec<u8>, PxbspBuildError> {
    if !render_planes.len().is_multiple_of(14) {
        return Err(PxbspBuildError::MisalignedRecords("render planes"));
    }
    if !collision.planes.len().is_multiple_of(14) {
        return Err(PxbspBuildError::MisalignedRecords("collision planes"));
    }
    if !collision.clipnodes.len().is_multiple_of(6) {
        return Err(PxbspBuildError::MisalignedRecords("clipnodes"));
    }
    let mut remap = Vec::with_capacity(collision.planes.len() / 14);
    for plane in collision.planes.chunks_exact(14) {
        let index = render_planes
            .chunks_exact(14)
            .position(|existing| existing == plane)
            .unwrap_or_else(|| {
                let index = render_planes.len() / 14;
                render_planes.extend_from_slice(plane);
                index
            });
        limit("planes", index + 1, i16::MAX as usize + 1)?;
        remap.push(index as i16);
    }
    let mut output = Vec::with_capacity(collision.clipnodes.len());
    for node in collision.clipnodes.chunks_exact(6) {
        let plane = i16::from_le_bytes([node[0], node[1]]);
        let mapped = plane
            .try_into()
            .ok()
            .and_then(|index: usize| remap.get(index).copied())
            .ok_or(PxbspBuildError::MisalignedRecords("clipnode plane"))?;
        output.extend_from_slice(&mapped.to_le_bytes());
        output.extend_from_slice(&node[2..6]);
    }
    Ok(output)
}

fn pack_world_model(geometry: &PackedBspGeometry, collision_heads: &[i16]) -> Vec<u8> {
    let mut output = Vec::with_capacity(32);
    pack_vec3_i16(&mut output, geometry.mins);
    pack_vec3_i16(&mut output, geometry.maxs);
    pack_vec3_i16(&mut output, [0; 3]);
    push_i16(&mut output, geometry.root_node);
    for &head in collision_heads {
        push_i16(&mut output, head);
    }
    push_i16(&mut output, geometry.visible_leaves);
    push_u16(&mut output, 0);
    push_u16(&mut output, (geometry.faces.len() / 14) as u16);
    output
}

fn pack_materials(materials: &[PxbspMaterial]) -> Vec<u8> {
    let mut output = Vec::with_capacity(materials.len() * PxbspMaterial::SIZE);
    for material in materials {
        push_u16(&mut output, material.texture_asset);
        push_u16(&mut output, material.flags);
        output.extend_from_slice(&material.tint);
        output.push(material.blend_mode);
        output.push(material.animation_kind);
        output.extend_from_slice(&material.animation_data);
    }
    output
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
    use psx_bsp::collision::CollisionHull;
    use psx_bsp::pxbsp::{PxbspEntityTable, PxbspIndex};
    use psx_bsp::{BrushModel, ClipNode, Plane, RecordSlice, SliceReader, Vec3I16, Vec3I32};

    const PLAYER: CollisionHullBounds = CollisionHullBounds {
        mins: [-16, 0, -16],
        maxs: [16, 56, 16],
    };
    const BIG: CollisionHullBounds = CollisionHullBounds {
        mins: [-32, 0, -32],
        maxs: [32, 96, 32],
    };

    fn compiled_room() -> CompiledPxbsp {
        let brushes = Brush::cuboid([0, 0, 0], [1024, 512, 1024])
            .hollow(64)
            .expect("room");
        let surfaces = compile_csg_surfaces(&brushes);
        let mut bsp = build_surface_bsp(&surfaces);
        let portals = portalize_surface_bsp(&bsp);
        classify_bsp_leaves(&mut bsp, &portals, &brushes);
        let geometry =
            pack_bsp_geometry(&bsp, &portals, BspLighting::Fullbright).expect("geometry");
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
        let trace = hull
            .trace(
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
            )
            .expect("trace");
        let end_y = trace.end.y as f64 / 4096.0;
        assert!((63.0..64.1).contains(&end_y));
    }

    #[test]
    fn complete_map_build_is_deterministic() {
        assert_eq!(compiled_room(), compiled_room());
    }
}
