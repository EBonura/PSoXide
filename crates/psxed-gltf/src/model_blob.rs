//! Cooks decimated skinned-mesh source data into the runtime PSXM model blob.

use super::*;

pub(crate) fn cook_model_blob(
    source: &SkinnedSourceMesh,
    bounds: &ModelBounds,
    parents: &[Option<usize>],
    joints: &[usize],
    material_color: [u8; 4],
    texture_width: u16,
    texture_height: u16,
    local_to_world_q12: u16,
    double_sided: bool,
) -> Result<(Vec<u8>, usize, usize), Error> {
    let mut node_to_joint = vec![None; parents.len()];
    for (joint_index, node_index) in joints.iter().copied().enumerate() {
        node_to_joint[node_index] = Some(joint_index as u16);
    }

    let mut joint_records = Vec::new();
    for node_index in joints.iter().copied() {
        let parent = parents[node_index]
            .and_then(|parent| node_to_joint[parent])
            .unwrap_or(psxed_format::model::NO_JOINT);
        joint_records.push(parent);
    }

    let mut canonical_by_position: BTreeMap<[i16; 3], u16> = BTreeMap::new();
    let mut canonical_vertices: Vec<CookedModelVertex> = Vec::new();
    let mut vertex_face_joints: BTreeMap<u16, BTreeSet<u16>> = BTreeMap::new();

    // The base importer still collapses source materials into one atlas and
    // emits bank 0 for every face. A palette-bank post-process may replace that
    // atlas with up to four consecutive 16-entry CLUTs and append the VERSION 5
    // two-bit face-bank stream. Keeping the assignment separate from
    // `ModelPart::material_index` is intentional: parts are skeletal projection
    // ranges, and splitting them would duplicate vertex work.
    let mut grouped_faces: BTreeMap<u16, Vec<[CookedFaceCorner; 3]>> = BTreeMap::new();

    for face in &source.faces {
        let mut out_face = [CookedFaceCorner {
            vertex_index: 0,
            uv: (0, 0),
        }; 3];
        for (corner, out_corner) in out_face.iter_mut().enumerate() {
            let vertex = source.vertices[face.indices[corner]];
            let primary_joint = vertex.dominant_joint;
            let record = encode_model_vertex(vertex, primary_joint, bounds);
            let key = model_vertex_position_key(&record);
            let temp_index = if let Some(index) = canonical_by_position.get(&key) {
                *index
            } else {
                let index = ensure_u16("vertices", canonical_vertices.len())?;
                canonical_by_position.insert(key, index);
                canonical_vertices.push(CookedModelVertex {
                    source: vertex,
                    primary_joint,
                    record,
                });
                index
            };
            vertex_face_joints
                .entry(temp_index)
                .or_default()
                .insert(face.joint);
            *out_corner = CookedFaceCorner {
                vertex_index: temp_index,
                uv: (
                    uv_to_u8(vertex.uv[0], texture_width),
                    uv_to_u8(vertex.uv[1], texture_height),
                ),
            };
        }
        grouped_faces.entry(face.joint).or_default().push(out_face);
    }

    let face_joints: BTreeSet<u16> = grouped_faces.keys().copied().collect();
    for (index, vertex) in canonical_vertices.iter_mut().enumerate() {
        if face_joints.contains(&vertex.primary_joint) {
            continue;
        }
        let fallback = vertex_face_joints
            .get(&(index as u16))
            .and_then(|joints| joints.iter().next().copied());
        if let Some(primary_joint) = fallback {
            vertex.primary_joint = primary_joint;
            vertex.record = encode_model_vertex(vertex.source, primary_joint, bounds);
        }
    }

    let mut vertices_by_joint: BTreeMap<u16, Vec<u16>> = BTreeMap::new();
    for (index, vertex) in canonical_vertices.iter().enumerate() {
        vertices_by_joint
            .entry(vertex.primary_joint)
            .or_default()
            .push(ensure_u16("vertices", index)?);
    }

    let part_joints = face_joints;

    let mut temp_to_final = vec![u16::MAX; canonical_vertices.len()];
    let mut vertex_ranges: BTreeMap<u16, (u16, u16)> = BTreeMap::new();
    let mut cooked_vertices: Vec<[u8; psxed_format::model::VERTEX_RECORD_SIZE]> = Vec::new();
    let mut blend_skin = false;

    for joint in &part_joints {
        let first_vertex = ensure_u16("part first vertex", cooked_vertices.len())?;
        if let Some(indices) = vertices_by_joint.get(joint) {
            for temp_index in indices {
                let final_index = ensure_u16("vertices", cooked_vertices.len())?;
                temp_to_final[*temp_index as usize] = final_index;
                let record = canonical_vertices[*temp_index as usize].record;
                if record[7] != 0 {
                    blend_skin = true;
                }
                cooked_vertices.push(record);
            }
        }
        let vertex_count = ensure_u16(
            "part vertices",
            cooked_vertices.len() - first_vertex as usize,
        )?;
        vertex_ranges.insert(*joint, (first_vertex, vertex_count));
    }

    let mut part_records = Vec::new();
    let mut cooked_faces: Vec<[CookedFaceCorner; 3]> = Vec::new();

    for joint in part_joints {
        let (first_vertex, vertex_count) = vertex_ranges.get(&joint).copied().unwrap_or((0, 0));
        let first_face = cooked_faces.len();
        if let Some(faces) = grouped_faces.get(&joint) {
            for face in faces {
                let mut out_face = *face;
                for corner in &mut out_face {
                    let final_index = temp_to_final[corner.vertex_index as usize];
                    if final_index == u16::MAX {
                        return Err(Error::BadSkin("model face references an un-emitted vertex"));
                    }
                    corner.vertex_index = final_index;
                }
                cooked_faces.push(out_face);
            }
        }
        let face_count = cooked_faces.len() - first_face;
        part_records.push((
            joint,
            first_vertex,
            vertex_count,
            ensure_u16("part first face", first_face)?,
            ensure_u16("part faces", face_count)?,
            0u16,
        ));
    }

    ensure_u16("vertices", cooked_vertices.len())?;
    ensure_u16("faces", cooked_faces.len())?;
    ensure_u16("parts", part_records.len())?;

    let payload_len = psxed_format::model::ModelHeader::SIZE
        + joint_records.len() * psxed_format::model::JointRecord::SIZE
        + psxed_format::model::MaterialRecord::SIZE
        + part_records.len() * psxed_format::model::PartRecord::SIZE
        + cooked_vertices.len() * psxed_format::model::VERTEX_RECORD_SIZE
        + cooked_faces.len() * psxed_format::model::FACE_RECORD_SIZE;
    let mut out = Vec::with_capacity(psxed_format::AssetHeader::SIZE + payload_len);
    let mut model_flags =
        psxed_format::model::flags::HAS_UVS | psxed_format::model::flags::RIGID_SKINNED;
    if blend_skin {
        model_flags |= psxed_format::model::flags::BLEND_SKIN;
    }
    if double_sided {
        model_flags |= psxed_format::model::flags::DOUBLE_SIDED;
    }
    append_asset_header(
        &mut out,
        psxed_format::model::MAGIC,
        psxed_format::model::VERSION,
        model_flags,
        payload_len,
    )?;
    append_u16(&mut out, ensure_u16("joints", joint_records.len())?);
    append_u16(&mut out, ensure_u16("parts", part_records.len())?);
    append_u16(&mut out, ensure_u16("vertices", cooked_vertices.len())?);
    append_u16(&mut out, ensure_u16("faces", cooked_faces.len())?);
    append_u16(&mut out, 1);
    append_u16(&mut out, texture_width);
    append_u16(&mut out, texture_height);
    append_u16(&mut out, local_to_world_q12);

    for parent in joint_records {
        append_u16(&mut out, parent);
        append_u16(&mut out, 0);
    }
    append_u16(&mut out, 0);
    append_u16(&mut out, 0);
    out.extend_from_slice(&material_color);

    for (joint, first_vertex, vertex_count, first_face, face_count, material_index) in &part_records
    {
        append_u16(&mut out, *joint);
        append_u16(&mut out, *first_vertex);
        append_u16(&mut out, *vertex_count);
        append_u16(&mut out, *first_face);
        append_u16(&mut out, *face_count);
        append_u16(&mut out, *material_index);
        append_u32(&mut out, 0);
    }
    for vertex in &cooked_vertices {
        out.extend_from_slice(vertex);
    }
    for face in &cooked_faces {
        for corner in face {
            append_u16(&mut out, corner.vertex_index);
            out.push(corner.uv.0);
            out.push(corner.uv.1);
        }
    }

    Ok((out, cooked_vertices.len(), part_records.len()))
}

pub(crate) fn model_vertex_position_key(
    record: &[u8; psxed_format::model::VERTEX_RECORD_SIZE],
) -> [i16; 3] {
    [
        i16::from_le_bytes([record[0], record[1]]),
        i16::from_le_bytes([record[2], record[3]]),
        i16::from_le_bytes([record[4], record[5]]),
    ]
}

pub(crate) fn model_vertex_position_key_for_source(
    vertex: SourceVertex,
    bounds: &ModelBounds,
) -> [i16; 3] {
    let position = bounds.normalize_point(vertex.position);
    [
        q12_i16(position[0]),
        q12_i16(position[1]),
        q12_i16(position[2]),
    ]
}

pub(crate) fn encode_model_vertex(
    vertex: SourceVertex,
    primary_joint: u16,
    bounds: &ModelBounds,
) -> [u8; psxed_format::model::VERTEX_RECORD_SIZE] {
    let position = model_vertex_position_key_for_source(vertex, bounds);
    let (joint1_byte, blend_byte) = blend_slot_for_vertex(vertex, primary_joint);
    let mut out = [0u8; psxed_format::model::VERTEX_RECORD_SIZE];
    write_i16(&mut out, 0, position[0]);
    write_i16(&mut out, 2, position[1]);
    write_i16(&mut out, 4, position[2]);
    out[6] = joint1_byte;
    out[7] = blend_byte;
    out
}

/// Threshold below which a secondary bone's relative weight is dropped.
///
/// `weight1 / (weight0 + weight1)` smaller than this contributes a
/// blend so subtle the runtime CPU path costs more than the visual
/// difference -- better to stay on the single-bone GTE fast path.
pub(crate) const BLEND_DROP_THRESHOLD: f32 = 0.04;

/// Pick the secondary bone + blend byte for a vertex, given the part
/// it ended up in.
///
/// `joint0` is implicit at runtime (it is the part's bone), so we only
/// store `joint1` and a relative weight in 0..=255. VERSION 4 stores
/// each skinned point once under its dominant bone, so the secondary
/// bone is simply the highest remaining influence after
/// `primary_joint`.
pub(crate) fn blend_slot_for_vertex(vertex: SourceVertex, primary_joint: u16) -> (u8, u8) {
    let mut weight0 = 0.0f32;
    let mut weight1 = 0.0f32;
    let mut joint1: Option<u16> = None;

    if vertex.dominant_joint != primary_joint && vertex.weights[0] > 0.0 {
        joint1 = Some(vertex.dominant_joint);
    }

    for i in 0..4 {
        let w = vertex.weights[i].max(0.0);
        if w == 0.0 {
            continue;
        }
        let j = vertex.joints[i];
        if j == primary_joint {
            weight0 += w;
        } else if Some(j) == joint1 {
            weight1 += w;
        } else if joint1.is_none() || w > weight1 {
            joint1 = Some(j);
            weight1 = w;
        }
    }

    let Some(j1) = joint1 else {
        return (psxed_format::model::NO_JOINT8, 0);
    };
    let total = weight0 + weight1;
    if total <= 0.0 {
        return (psxed_format::model::NO_JOINT8, 0);
    }
    let blend = weight1 / total;
    if blend < BLEND_DROP_THRESHOLD {
        return (psxed_format::model::NO_JOINT8, 0);
    }

    let blend_byte = (blend * 255.0).round().clamp(0.0, 255.0) as u8;
    if blend_byte == 0 {
        return (psxed_format::model::NO_JOINT8, 0);
    }
    let joint1_byte = if j1 < 255 {
        j1 as u8
    } else {
        psxed_format::model::NO_JOINT8
    };
    if joint1_byte == psxed_format::model::NO_JOINT8 {
        return (psxed_format::model::NO_JOINT8, 0);
    }
    (joint1_byte, blend_byte)
}

pub(crate) fn cook_base_color_texture(
    mesh: &gltf::Mesh<'_>,
    buffers: &[gltf::buffer::Data],
    cfg: &RigidModelConfig,
) -> Result<Option<Vec<u8>>, Error> {
    for primitive in mesh.primitives() {
        let Some(info) = primitive
            .material()
            .pbr_metallic_roughness()
            .base_color_texture()
        else {
            continue;
        };
        let source = info.texture().source();
        let Source::View { view, .. } = source.source() else {
            return Err(Error::UnsupportedImageSource);
        };
        let buffer = &buffers[view.buffer().index()].0;
        let start = view.offset();
        let end = start + view.length();
        let bytes = buffer
            .get(start..end)
            .ok_or(Error::UnsupportedImageSource)?;
        let tex_cfg = model_texture_config(cfg);
        return psxed_tex::convert(bytes, &tex_cfg)
            .map(Some)
            .map_err(Error::TextureCook);
    }
    Ok(None)
}

/// Cooked fallback atlas for models that ship without a texture image.
/// A neutral grey checker (rather than a flat fill) so the model's UVs
/// give it readable surface detail until the real atlas arrives.
pub(crate) fn cook_solid_base_color_texture(
    _color: [u8; 4],
    cfg: &RigidModelConfig,
) -> Result<Vec<u8>, Error> {
    let bmp = checker_skin_bmp(
        cfg.texture_width.max(1) as u32,
        cfg.texture_height.max(1) as u32,
    );
    psxed_tex::convert(&bmp, &model_texture_config(cfg)).map_err(Error::TextureCook)
}

/// Neutral two-tone grey checkerboard BMP, used as the no-texture
/// fallback skin. 16x16 texel cells read clearly at 128x128.
pub(crate) fn checker_skin_bmp(width: u32, height: u32) -> Vec<u8> {
    let cell = 16u32;
    let row_stride = (width * 3).div_ceil(4) * 4;
    let pixel_bytes = row_stride * height;
    let file_size = 14 + 40 + pixel_bytes;
    let mut out = Vec::with_capacity(file_size as usize);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&file_size.to_le_bytes());
    out.extend_from_slice(&[0u8; 4]);
    out.extend_from_slice(&(14u32 + 40u32).to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(height as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&pixel_bytes.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    let pad = (row_stride - width * 3) as usize;
    for y in 0..height {
        for x in 0..width {
            let on = ((x / cell) + (y / cell)).is_multiple_of(2);
            let g = if on { 190u8 } else { 110u8 };
            out.push(g);
            out.push(g);
            out.push(g);
        }
        out.extend(std::iter::repeat_n(0u8, pad));
    }
    out
}

pub(crate) fn model_texture_config(cfg: &RigidModelConfig) -> psxed_tex::Config {
    psxed_tex::Config {
        width: cfg.texture_width,
        height: cfg.texture_height,
        depth: cfg.texture_depth,
        crop: psxed_tex::CropMode::None,
        resampler: psxed_tex::Resampler::Lanczos3,
        transparent_index_zero: true,
        clut_rows: 1,
    }
}

pub(crate) fn first_material_base_color(mesh: &gltf::Mesh<'_>) -> [u8; 4] {
    if let Some(primitive) = mesh.primitives().next() {
        let color = primitive
            .material()
            .pbr_metallic_roughness()
            .base_color_factor();
        [
            linear_to_u8(color[0]),
            linear_to_u8(color[1]),
            linear_to_u8(color[2]),
            linear_to_u8(color[3]),
        ]
    } else {
        [255, 255, 255, 255]
    }
}
