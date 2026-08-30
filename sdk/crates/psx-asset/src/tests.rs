use super::*;

#[test]
fn rejects_wrong_magic() {
    let bad = [b'N', b'O', b'P', b'E', 0, 0, 0, 0, 0, 0, 0, 0];
    assert!(matches!(
        Mesh::from_bytes(&bad),
        Err(ParseError::WrongMagic)
    ));
}

#[test]
fn rejects_truncated() {
    let too_short = [0u8; 4];
    assert!(matches!(
        Mesh::from_bytes(&too_short),
        Err(ParseError::Truncated)
    ));
}

#[test]
fn rejects_unsupported_version() {
    let mut bad = [0u8; 12];
    bad[0..4].copy_from_slice(&psxed_format::mesh::MAGIC);
    bad[4..6].copy_from_slice(&999u16.to_le_bytes());
    assert!(matches!(
        Mesh::from_bytes(&bad),
        Err(ParseError::UnsupportedVersion(999))
    ));
}

/// Pin the `.psxw` parser to the versions it intentionally
/// supports. Older revisions are legacy compatibility and the
/// format crate's VERSION is current;
/// any newer blob must be rejected.
#[test]
fn world_rejects_unknown_version() {
    let mut bad = [0u8; 12];
    bad[0..4].copy_from_slice(&psxed_format::world::MAGIC);
    let unknown = psxed_format::world::VERSION + 1;
    bad[4..6].copy_from_slice(&unknown.to_le_bytes());
    // Payload length 0 -- won't matter; version check fires first.
    bad[8..12].copy_from_slice(&0u32.to_le_bytes());
    assert!(matches!(
        World::from_bytes(&bad),
        Err(ParseError::UnsupportedVersion(version)) if version == unknown
    ));
}

/// Sizes the cooker / runtime have agreed on for world formats.
/// Drift would invalidate every committed `.psxw` blob, so
/// pin them at the format crate's records, not the wire.
#[test]
fn world_record_sizes_match_contract() {
    assert_eq!(WORLD_V3_HEADER_SIZE, 20);
    assert_eq!(psxed_format::world::WorldHeader::SIZE, 24);
    assert_eq!(psxed_format::world::QuadUvRecord::SIZE, 8);
    assert_eq!(WORLD_V4_HORIZONTAL_OVERRIDE_RECORD_SIZE, 24);
    assert_eq!(psxed_format::world::HorizontalOverrideRecord::SIZE, 48);
    assert_eq!(psxed_format::world::SurfaceLightRecord::SIZE, 12);
    assert_eq!(WORLD_V2_SECTOR_RECORD_SIZE, 60);
    assert_eq!(WORLD_V2_WALL_RECORD_SIZE, 32);
    assert_eq!(psxed_format::world::SectorRecord::SIZE, 60);
    assert_eq!(psxed_format::world::WallRecord::SIZE, 32);
}

#[test]
fn world_v1_synthesizes_default_uvs() {
    use psxed_format::world;

    const WORLD_V1_LEN: usize =
        psxed_format::AssetHeader::SIZE + WORLD_V3_HEADER_SIZE + WORLD_V1_SECTOR_RECORD_SIZE;
    let payload_len = (WORLD_V3_HEADER_SIZE + WORLD_V1_SECTOR_RECORD_SIZE) as u32;
    let mut buf = [0u8; WORLD_V1_LEN];
    buf[0..4].copy_from_slice(&world::MAGIC);
    buf[4..6].copy_from_slice(&world::VERSION_V1.to_le_bytes());
    buf[8..12].copy_from_slice(&payload_len.to_le_bytes());
    buf[12..14].copy_from_slice(&1u16.to_le_bytes());
    buf[14..16].copy_from_slice(&1u16.to_le_bytes());
    buf[16..20].copy_from_slice(&world::SECTOR_SIZE.to_le_bytes());
    buf[20..22].copy_from_slice(&1u16.to_le_bytes());

    let sector = psxed_format::AssetHeader::SIZE + WORLD_V3_HEADER_SIZE;
    buf[sector] = world::sector_flags::HAS_FLOOR;
    buf[sector + 4..sector + 6].copy_from_slice(&0u16.to_le_bytes());
    buf[sector + 6..sector + 8].copy_from_slice(&world::NO_MATERIAL.to_le_bytes());

    let world = World::from_bytes(&buf).expect("legacy world parses");
    let sector = world.sector(0, 0).unwrap();
    assert_eq!(sector.floor_uvs().corners(), psxed_format::world::FLOOR_UVS);
}

#[test]
fn texture_round_trip_4bpp() {
    use psxed_format::texture::Depth;
    // 12 AssetHeader + 16 TextureHeader + 8 pixels + 32 CLUT = 68 bytes.
    let pixel_bytes: u32 = 8;
    let clut_bytes: u32 = 32;
    let payload_len = 16 + pixel_bytes + clut_bytes;
    let mut buf = [0u8; 68];
    buf[0..4].copy_from_slice(b"PSXT");
    buf[4..6].copy_from_slice(&1u16.to_le_bytes());
    buf[6..8].copy_from_slice(&0u16.to_le_bytes());
    buf[8..12].copy_from_slice(&payload_len.to_le_bytes());
    // TextureHeader @ offset 12.
    buf[12] = 4; // depth
    buf[13] = 0;
    buf[14..16].copy_from_slice(&4u16.to_le_bytes()); // width
    buf[16..18].copy_from_slice(&4u16.to_le_bytes()); // height
    buf[18..20].copy_from_slice(&16u16.to_le_bytes()); // clut_entries
    buf[20..24].copy_from_slice(&pixel_bytes.to_le_bytes());
    buf[24..28].copy_from_slice(&clut_bytes.to_le_bytes());
    // 4 rows × 1 halfword = 4 halfwords = 8 bytes @ offset 28.
    for row in 0..4u16 {
        let off = 28 + (row as usize) * 2;
        buf[off..off + 2].copy_from_slice(&(row * 0x1111).to_le_bytes());
    }
    // 16 CLUT entries @ offset 36.
    for i in 0..16u16 {
        let off = 36 + (i as usize) * 2;
        buf[off..off + 2].copy_from_slice(&(i * 0x0123).to_le_bytes());
    }

    let t = Texture::from_bytes(&buf).expect("parse");
    assert_eq!(t.width(), 4);
    assert_eq!(t.height(), 4);
    assert_eq!(t.depth(), Depth::Bit4);
    assert_eq!(t.clut_entries(), 16);
    assert_eq!(t.halfwords_per_row(), 1);
    assert_eq!(t.pixel_bytes().len(), 8);
    assert_eq!(t.clut_bytes().len(), 32);
}

#[test]
fn texture_rejects_wrong_magic() {
    let bad = [b'N', b'O', b'P', b'E', 0, 0, 0, 0, 0, 0, 0, 0];
    assert!(matches!(
        Texture::from_bytes(&bad),
        Err(ParseError::WrongMagic)
    ));
}

#[test]
fn audio_round_trip_one_shot() {
    use psxed_format::audio;

    let adpcm = [0x08u8, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let payload_len = audio::AudioHeader::SIZE + adpcm.len();
    let mut buf = std::vec::Vec::new();
    buf.extend_from_slice(&audio::MAGIC);
    buf.extend_from_slice(&audio::VERSION.to_le_bytes());
    buf.extend_from_slice(&(audio::flags::MONO | audio::flags::ONE_SHOT).to_le_bytes());
    buf.extend_from_slice(&(payload_len as u32).to_le_bytes());
    buf.push(audio::CODEC_SPU_ADPCM);
    buf.push(1);
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&44_100u32.to_le_bytes());
    buf.extend_from_slice(&28u32.to_le_bytes());
    buf.extend_from_slice(&1u32.to_le_bytes());
    buf.extend_from_slice(&audio::AudioHeader::NO_LOOP.to_le_bytes());
    buf.extend_from_slice(&adpcm);

    let sample = Audio::from_bytes(&buf).expect("parse audio");
    assert_eq!(sample.sample_rate_hz(), 44_100);
    assert_eq!(sample.sample_count(), 28);
    assert_eq!(sample.adpcm_block_count(), 1);
    assert_eq!(sample.loop_start_block(), None);
    assert_eq!(sample.adpcm_bytes(), adpcm);
    assert!(sample.is_one_shot());
}

#[test]
fn audio_rejects_bad_block_count() {
    use psxed_format::audio;

    let mut buf = [0u8; psxed_format::AssetHeader::SIZE + audio::AudioHeader::SIZE];
    buf[0..4].copy_from_slice(&audio::MAGIC);
    buf[4..6].copy_from_slice(&audio::VERSION.to_le_bytes());
    buf[6..8].copy_from_slice(&(audio::flags::MONO | audio::flags::ONE_SHOT).to_le_bytes());
    buf[8..12].copy_from_slice(&(audio::AudioHeader::SIZE as u32).to_le_bytes());
    buf[12] = audio::CODEC_SPU_ADPCM;
    buf[13] = 1;
    buf[16..20].copy_from_slice(&44_100u32.to_le_bytes());
    buf[20..24].copy_from_slice(&28u32.to_le_bytes());
    buf[24..28].copy_from_slice(&1u32.to_le_bytes());
    buf[28..32].copy_from_slice(&audio::AudioHeader::NO_LOOP.to_le_bytes());

    assert!(matches!(
        Audio::from_bytes(&buf),
        Err(ParseError::InvalidAudioLayout)
    ));
}

#[test]
fn model_round_trip_minimal_textured_part() {
    use psxed_format::model;

    let payload_len = model::ModelHeader::SIZE
        + model::JointRecord::SIZE
        + model::MaterialRecord::SIZE
        + model::PartRecord::SIZE
        + 3 * model::VERTEX_RECORD_SIZE
        + model::FACE_RECORD_SIZE
        + model::face_palette_bank_bytes(1);
    let mut buf = std::vec::Vec::new();
    buf.extend_from_slice(&model::MAGIC);
    buf.extend_from_slice(&model::VERSION.to_le_bytes());
    buf.extend_from_slice(
        &(model::flags::HAS_UVS | model::flags::RIGID_SKINNED | model::flags::FACE_PALETTE_BANKS)
            .to_le_bytes(),
    );
    buf.extend_from_slice(&(payload_len as u32).to_le_bytes());

    buf.extend_from_slice(&1u16.to_le_bytes()); // joints
    buf.extend_from_slice(&1u16.to_le_bytes()); // parts
    buf.extend_from_slice(&3u16.to_le_bytes()); // vertices
    buf.extend_from_slice(&1u16.to_le_bytes()); // faces
    buf.extend_from_slice(&1u16.to_le_bytes()); // materials
    buf.extend_from_slice(&128u16.to_le_bytes());
    buf.extend_from_slice(&128u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());

    buf.extend_from_slice(&model::NO_JOINT.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&[255, 255, 255, 255]);
    for value in [0u16, 0, 3, 0, 1, 0] {
        buf.extend_from_slice(&value.to_le_bytes());
    }
    buf.extend_from_slice(&0u32.to_le_bytes());

    for (x, y) in [(0i16, 0i16), (4096, 0), (0, 4096)] {
        buf.extend_from_slice(&x.to_le_bytes());
        buf.extend_from_slice(&y.to_le_bytes());
        buf.extend_from_slice(&0i16.to_le_bytes());
        // joint1 sentinel + blend=0 means single-bone vertex.
        buf.push(psxed_format::model::NO_JOINT8);
        buf.push(0);
    }
    for (index, u, v) in [(0u16, 0u8, 0u8), (1, 127, 0), (2, 0, 127)] {
        buf.extend_from_slice(&index.to_le_bytes());
        buf.push(u);
        buf.push(v);
    }
    buf.push(3);

    let model = Model::from_bytes(&buf).expect("parse model");
    assert_eq!(model.joint_count(), 1);
    assert_eq!(model.part_count(), 1);
    assert_eq!(model.vertex_count(), 3);
    assert_eq!(model.face_count(), 1);
    assert_eq!(model.texture_width(), 128);
    assert_eq!(
        model.local_to_world_q12(),
        psxed_format::model::DEFAULT_LOCAL_TO_WORLD_Q12
    );
    assert_eq!(model.joint(0).unwrap().parent(), None);
    assert_eq!(
        model.material(0).unwrap().base_color(),
        [255, 255, 255, 255]
    );
    assert_eq!(model.part(0).unwrap().face_count(), 1);
    assert_eq!(model.vertex(1).unwrap().position, Vec3I16::new(4096, 0, 0));
    let face = model.face(0).unwrap();
    assert_eq!(face.corners[0].vertex_index, 0);
    assert_eq!(face.corners[1].vertex_index, 1);
    assert_eq!(face.corners[1].uv, (127, 0));
    assert_eq!(face.corners[2].vertex_index, 2);
    assert_eq!(face.corners[2].uv, (0, 127));
    assert_eq!(model.face_palette_bank(0), Some(3));
    assert_eq!(model.palette_bank_count(), 4);

    let mut legacy = buf.clone();
    legacy.pop();
    legacy[4..6].copy_from_slice(&model::LEGACY_VERSION.to_le_bytes());
    let legacy_flags = model::flags::HAS_UVS | model::flags::RIGID_SKINNED;
    legacy[6..8].copy_from_slice(&legacy_flags.to_le_bytes());
    legacy[8..12].copy_from_slice(&((payload_len - 1) as u32).to_le_bytes());
    let legacy = Model::from_bytes(&legacy).expect("parse legacy model");
    assert_eq!(legacy.face_palette_bank(0), Some(0));
    assert_eq!(legacy.palette_bank_count(), 1);
}

#[test]
fn animation_round_trip_pose_table() {
    use psxed_format::animation;

    let payload_len = animation::AnimationHeader::SIZE + 2 * animation::POSE_RECORD_SIZE;
    let mut buf = std::vec::Vec::new();
    buf.extend_from_slice(&animation::MAGIC);
    buf.extend_from_slice(&animation::VERSION.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&(payload_len as u32).to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes()); // joints
    buf.extend_from_slice(&2u16.to_le_bytes()); // frames
    buf.extend_from_slice(&15u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());

    for frame in 0..2i16 {
        for value in [4096i16, 0, 0, 0, 4096, 0, 0, 0, 4096] {
            buf.extend_from_slice(&value.to_le_bytes());
        }
        for value in [frame * 100, frame * 200, frame * 300] {
            buf.extend_from_slice(&value.to_le_bytes());
        }
    }

    let animation = Animation::from_bytes(&buf).expect("parse animation");
    assert_eq!(animation.joint_count(), 1);
    assert_eq!(animation.frame_count(), 2);
    assert_eq!(animation.sample_rate_hz(), 15);
    assert_eq!(animation.phase_step_q12(30), 0x0800);
    assert_eq!(animation.phase_at_tick_q12(2, 30), 0x1000);
    let pose = animation.pose(1, 0).unwrap();
    assert_eq!(pose.matrix[0][0], 4096);
    assert_eq!(pose.translation, Vec3I32::new(100, 200, 300));
    let sample = animation.looped_pose_sample_q12(0).unwrap();
    let gte_pose = sample.gte_pose(0).unwrap();
    assert_eq!(gte_pose.translation, Vec3I16::new(0, 0, 0));
    assert_eq!(gte_pose.translation_shift, 0);

    let mut unaligned = std::vec![0u8];
    unaligned.extend_from_slice(&buf);
    let unaligned_bytes = &unaligned[1..];
    assert_eq!(unaligned_bytes.as_ptr() as usize & 1, 1);
    let unaligned_animation = Animation::from_bytes(unaligned_bytes).expect("parse unaligned");
    assert_eq!(unaligned_animation.pose(1, 0), Some(pose));

    let mut halfword_aligned = std::vec![0u8; 2];
    halfword_aligned.extend_from_slice(&buf);
    let halfword_bytes = &halfword_aligned[2..];
    assert_eq!(halfword_bytes.as_ptr() as usize & 3, 2);
    let halfword_animation = Animation::from_bytes(halfword_bytes).expect("parse halfword-aligned");
    assert_eq!(halfword_animation.pose(1, 0), Some(pose));
}

fn pack_q11_codes(codes: [u16; 9]) -> [u8; 14] {
    let mut packed = [0u8; 14];
    let mut lane = 0usize;
    while lane < codes.len() {
        let code = codes[lane] & 0x0fff;
        let bit_offset = lane * 12;
        let mut bit = 0usize;
        while bit < 12 {
            if code & (1 << bit) != 0 {
                let output_bit = bit_offset + bit;
                packed[output_bit >> 3] |= 1 << (output_bit & 7);
            }
            bit += 1;
        }
        lane += 1;
    }
    packed
}

#[test]
fn animation_v3_word_decoder_matches_byte_decoder_for_every_q11_code_and_lane() {
    #[repr(C, align(4))]
    struct WordAlignedRecord([u8; psxed_format::animation::POSE_RECORD_SIZE_V3]);

    let translations = [-32768i16, 0x1234, 32767];
    let mut lane = 0usize;
    while lane < 9 {
        let mut code = 0u16;
        while code < 0x1000 {
            let mut codes = [0u16; 9];
            let mut other = 0usize;
            while other < codes.len() {
                codes[other] = ((other as u16 * 0x19d) ^ 0x5a5) & 0x0fff;
                other += 1;
            }
            codes[lane] = code;

            let mut record = WordAlignedRecord([0; psxed_format::animation::POSE_RECORD_SIZE_V3]);
            record.0[..psxed_format::animation::POSE_ROTATION_BLOCK_SIZE_V3]
                .copy_from_slice(&pack_q11_codes(codes));
            let mut translation_offset = psxed_format::animation::POSE_ROTATION_BLOCK_SIZE_V3;
            for translation in translations {
                record.0[translation_offset..translation_offset + 2]
                    .copy_from_slice(&translation.to_le_bytes());
                translation_offset += 2;
            }

            let expected_matrix = unsafe { read_pose_matrix_q11_unchecked(&record.0, 0) };
            let (matrix, translation) =
                unsafe { read_pose_v3_word_aligned_unchecked(&record.0, 0) };
            assert_eq!(matrix, expected_matrix, "lane={lane} code=0x{code:03x}");
            assert_eq!(
                translation,
                Vec3I16::new(translations[0], translations[1], translations[2]),
                "lane={lane} code=0x{code:03x}"
            );
            code += 1;
        }
        lane += 1;
    }
}

#[test]
fn animation_v3_aligned_and_unaligned_public_paths_are_identical() {
    use psxed_format::animation;

    const FRAMES: usize = 3;
    const BYTES: usize = psxed_format::AssetHeader::SIZE
        + animation::AnimationHeader::SIZE
        + FRAMES * animation::POSE_RECORD_SIZE_V3;
    #[repr(C, align(4))]
    struct WordAlignedBlob([u8; BYTES]);

    let mut blob = WordAlignedBlob([0; BYTES]);
    blob.0[0..4].copy_from_slice(&animation::MAGIC);
    blob.0[4..6].copy_from_slice(&animation::VERSION_V3.to_le_bytes());
    let payload_len = BYTES - psxed_format::AssetHeader::SIZE;
    blob.0[8..12].copy_from_slice(&(payload_len as u32).to_le_bytes());
    blob.0[12..14].copy_from_slice(&1u16.to_le_bytes());
    blob.0[14..16].copy_from_slice(&(FRAMES as u16).to_le_bytes());
    blob.0[16..18].copy_from_slice(&15u16.to_le_bytes());
    blob.0[18..20].copy_from_slice(&2u16.to_le_bytes());

    for frame in 0..FRAMES {
        let codes = if frame == 1 {
            [
                0x200, 0x111, 0xeee, 0x7ff, 0x800, 0x001, 0xfff, 0x333, 0xabc,
            ]
        } else {
            [0x7ff, 0, 0, 0, 0x7ff, 0, 0, 0, 0x7ff]
        };
        let base = 20 + frame * animation::POSE_RECORD_SIZE_V3;
        blob.0[base..base + animation::POSE_ROTATION_BLOCK_SIZE_V3]
            .copy_from_slice(&pack_q11_codes(codes));
        for (axis, value) in [frame as i16 * 10, frame as i16 * -20, frame as i16 * 30]
            .into_iter()
            .enumerate()
        {
            let offset = base + animation::POSE_ROTATION_BLOCK_SIZE_V3 + axis * 2;
            blob.0[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
    }

    let aligned = Animation::from_bytes(&blob.0).expect("aligned v3 animation parses");
    assert_eq!(aligned.poses.as_ptr() as usize & 3, 0);

    for desired_alignment in [1usize, 2, 3] {
        let mut storage = std::vec![0u8; BYTES + 3];
        let base_alignment = storage.as_ptr() as usize & 3;
        let prefix = (desired_alignment + 4 - base_alignment) & 3;
        storage[prefix..prefix + BYTES].copy_from_slice(&blob.0);
        let bytes = &storage[prefix..prefix + BYTES];
        assert_eq!(bytes.as_ptr() as usize & 3, desired_alignment);
        let unaligned = Animation::from_bytes(bytes).expect("unaligned v3 animation parses");
        for phase in [0, 0x400, 0x800, 0x1000, 0x1800] {
            let aligned_sample = aligned.looped_pose_sample_q12(phase).unwrap();
            let unaligned_sample = unaligned.looped_pose_sample_q12(phase).unwrap();
            assert_eq!(aligned_sample.pose(0), unaligned_sample.pose(0));
            assert_eq!(aligned_sample.gte_pose(0), unaligned_sample.gte_pose(0));
        }
    }
}

#[test]
fn animation_v4_aligned_and_unaligned_public_paths_are_identical() {
    use psxed_format::animation;

    const FRAMES: usize = 3;
    const BYTES: usize = psxed_format::AssetHeader::SIZE
        + animation::AnimationHeader::SIZE
        + FRAMES * animation::POSE_RECORD_SIZE_V4;
    #[repr(C, align(4))]
    struct WordAlignedBlob([u8; BYTES]);

    let mut blob = WordAlignedBlob([0; BYTES]);
    blob.0[0..4].copy_from_slice(&animation::MAGIC);
    blob.0[4..6].copy_from_slice(&animation::VERSION_V4.to_le_bytes());
    let payload_len = BYTES - psxed_format::AssetHeader::SIZE;
    blob.0[8..12].copy_from_slice(&(payload_len as u32).to_le_bytes());
    blob.0[12..14].copy_from_slice(&1u16.to_le_bytes());
    blob.0[14..16].copy_from_slice(&(FRAMES as u16).to_le_bytes());
    blob.0[16..18].copy_from_slice(&15u16.to_le_bytes());
    blob.0[18..20].copy_from_slice(&2u16.to_le_bytes());

    let matrices = [
        [4096, 0, 0, 0, 4096, 0, 0, 0, 4096],
        [3850, 1212, 704, -1294, 3862, 432, -536, -628, 4012],
        [4096, 0, 0, 0, 4096, 0, 0, 0, 4096],
    ];
    for (frame, matrix) in matrices.into_iter().enumerate() {
        let base = 20 + frame * animation::POSE_RECORD_SIZE_V4;
        let mut block = [0u8; animation::POSE_ROTATION_BLOCK_SIZE_V4];
        assert!(animation::encode_rotation_q11_cross(&matrix, &mut block));
        blob.0[base..base + animation::POSE_ROTATION_BLOCK_SIZE_V4].copy_from_slice(&block);
        for (axis, value) in [frame as i16 * 10, frame as i16 * -20, frame as i16 * 30]
            .into_iter()
            .enumerate()
        {
            let offset = base + animation::POSE_ROTATION_BLOCK_SIZE_V4 + axis * 2;
            blob.0[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
    }

    let aligned = Animation::from_bytes(&blob.0).expect("aligned v4 animation parses");
    assert_eq!(aligned.poses.as_ptr() as usize & 3, 0);

    for desired_alignment in [1usize, 2, 3] {
        let mut storage = std::vec![0u8; BYTES + 3];
        let base_alignment = storage.as_ptr() as usize & 3;
        let prefix = (desired_alignment + 4 - base_alignment) & 3;
        storage[prefix..prefix + BYTES].copy_from_slice(&blob.0);
        let bytes = &storage[prefix..prefix + BYTES];
        assert_eq!(bytes.as_ptr() as usize & 3, desired_alignment);
        let unaligned = Animation::from_bytes(bytes).expect("unaligned v4 animation parses");
        for phase in [0, 0x400, 0x800, 0x1000, 0x1800] {
            let aligned_sample = aligned.looped_pose_sample_q12(phase).unwrap();
            let unaligned_sample = unaligned.looped_pose_sample_q12(phase).unwrap();
            assert_eq!(aligned_sample.pose(0), unaligned_sample.pose(0));
            assert_eq!(aligned_sample.gte_pose(0), unaligned_sample.gte_pose(0));
        }
    }
}

#[test]
fn animation_looped_pose_interpolates_q12_phase() {
    use psxed_format::animation;

    let payload_len = animation::AnimationHeader::SIZE + 3 * animation::POSE_RECORD_SIZE;
    let mut buf = std::vec::Vec::new();
    buf.extend_from_slice(&animation::MAGIC);
    buf.extend_from_slice(&animation::VERSION.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&(payload_len as u32).to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes()); // joints
    buf.extend_from_slice(&3u16.to_le_bytes()); // frames: first, middle, duplicate first
    buf.extend_from_slice(&15u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());

    for (m00, tx, ty, tz) in [
        (4096i16, 0i16, 0i16, 0i16),
        (2048i16, 100i16, -100i16, 50i16),
        (4096i16, 0i16, 0i16, 0i16),
    ] {
        for value in [m00, 0, 0, 0, 4096, 0, 0, 0, 4096] {
            buf.extend_from_slice(&value.to_le_bytes());
        }
        for value in [tx, ty, tz] {
            buf.extend_from_slice(&value.to_le_bytes());
        }
    }

    let animation = Animation::from_bytes(&buf).expect("parse animation");
    let halfway = animation.pose_looped_q12(0x0800, 0).unwrap();
    assert_eq!(halfway.matrix[0][0], 3072);
    assert_eq!(halfway.translation, Vec3I32::new(50, -50, 25));
    let sample = animation.looped_pose_sample_q12(0x0800).unwrap();
    assert_eq!(sample.pose(0), Some(halfway));
    assert_eq!(
        sample.gte_pose(0).unwrap().translation,
        Vec3I16::new(50, -50, 25)
    );

    let wrapped = animation.pose_looped_q12(0x1800, 0).unwrap();
    assert_eq!(wrapped.matrix[0][0], 3072);
    assert_eq!(wrapped.translation, Vec3I32::new(50, -50, 25));
}

#[test]
fn i16_animation_lerp_stays_between_extreme_endpoints() {
    let values = [i16::MIN, -30_000, -1, 0, 1, 30_000, i16::MAX];
    let alphas = [0, 1, 63, 1_024, 2_047, 2_048, 2_049, 4_094, 4_095];
    for &a in &values {
        for &b in &values {
            for &alpha in &alphas {
                let actual = super::lerp_i16_q12(a, b, alpha);
                assert!(actual >= a.min(b) && actual <= a.max(b));
                let expected = (a as i32 + (((b as i32 - a as i32) * alpha as i32) >> 12)) as i16;
                assert_eq!(actual, expected);
            }
        }
    }
}

#[test]
fn packed_animation_translation_decode_fits_i32_for_every_shift() {
    for shift in 0..=15 {
        let scale = 1i32 << shift;
        for value in [i16::MIN, -1, 0, 1, i16::MAX] {
            assert_eq!(
                super::decode_packed_translation(value, shift),
                i32::from(value) * scale
            );
        }
    }
}

#[test]
fn world_round_trip_1x1_with_wall() {
    use psxed_format::world;

    const SURFACE_LIGHT_RECORD_COUNT: usize = 3;
    const WORLD_ROUND_TRIP_LEN: usize = psxed_format::AssetHeader::SIZE
        + psxed_format::world::WorldHeader::SIZE
        + psxed_format::world::SectorRecord::SIZE
        + psxed_format::world::WallRecord::SIZE
        + SURFACE_LIGHT_RECORD_COUNT * psxed_format::world::SurfaceLightRecord::SIZE;
    let payload_len = (world::WorldHeader::SIZE
        + world::SectorRecord::SIZE
        + world::WallRecord::SIZE
        + SURFACE_LIGHT_RECORD_COUNT * world::SurfaceLightRecord::SIZE)
        as u32;
    let mut buf = [0u8; WORLD_ROUND_TRIP_LEN];
    buf[0..4].copy_from_slice(&world::MAGIC);
    buf[4..6].copy_from_slice(&world::VERSION.to_le_bytes());
    buf[6..8].copy_from_slice(&0u16.to_le_bytes());
    buf[8..12].copy_from_slice(&payload_len.to_le_bytes());

    buf[12..14].copy_from_slice(&1u16.to_le_bytes()); // width
    buf[14..16].copy_from_slice(&1u16.to_le_bytes()); // depth
    buf[16..20].copy_from_slice(&world::SECTOR_SIZE.to_le_bytes());
    buf[20..22].copy_from_slice(&1u16.to_le_bytes()); // sectors
    buf[22..24].copy_from_slice(&2u16.to_le_bytes()); // materials
    buf[24..26].copy_from_slice(&1u16.to_le_bytes()); // walls
    buf[26..29].copy_from_slice(&[32, 32, 40]);
    buf[29] = world::world_flags::FOG_ENABLED | world::world_flags::STATIC_VERTEX_LIGHTING;
    buf[30..32].copy_from_slice(&(SURFACE_LIGHT_RECORD_COUNT as u16).to_le_bytes());

    let sector = 12 + world::WorldHeader::SIZE;
    buf[sector] = world::sector_flags::HAS_FLOOR | world::sector_flags::FLOOR_WALKABLE;
    buf[sector + 1] = world::split::NORTH_WEST_SOUTH_EAST;
    buf[sector + 4..sector + 6].copy_from_slice(&0u16.to_le_bytes());
    buf[sector + 6..sector + 8].copy_from_slice(&world::NO_MATERIAL.to_le_bytes());
    buf[sector + 8..sector + 10].copy_from_slice(&0u16.to_le_bytes());
    buf[sector + 10..sector + 12].copy_from_slice(&1u16.to_le_bytes());
    for (i, (u, v)) in world::FLOOR_UVS.iter().copied().enumerate() {
        buf[sector + 44 + i * 2] = u;
        buf[sector + 45 + i * 2] = v;
    }
    let wall = sector + world::SectorRecord::SIZE;
    buf[wall] = world::direction::NORTH;
    buf[wall + 1] = world::wall_flags::SOLID;
    buf[wall + 4..wall + 6].copy_from_slice(&1u16.to_le_bytes());
    buf[wall + 8..wall + 12].copy_from_slice(&0i32.to_le_bytes());
    buf[wall + 12..wall + 16].copy_from_slice(&0i32.to_le_bytes());
    buf[wall + 16..wall + 20].copy_from_slice(&1024i32.to_le_bytes());
    buf[wall + 20..wall + 24].copy_from_slice(&1024i32.to_le_bytes());
    for (i, (u, v)) in world::WALL_UVS.iter().copied().enumerate() {
        buf[wall + 24 + i * 2] = u;
        buf[wall + 25 + i * 2] = v;
    }
    let floor_light = [[10, 20, 30], [40, 50, 60], [70, 80, 90], [100, 110, 120]];
    let ceiling_light = [[1, 2, 3], [4, 5, 6], [7, 8, 9], [10, 11, 12]];
    let wall_light = [[12, 22, 32], [42, 52, 62], [72, 82, 92], [102, 112, 122]];
    let lights = wall + world::WallRecord::SIZE;
    for (record_index, light) in [floor_light, ceiling_light, wall_light].iter().enumerate() {
        for (corner_index, rgb) in light.iter().enumerate() {
            let off = lights + record_index * world::SurfaceLightRecord::SIZE + corner_index * 3;
            buf[off..off + 3].copy_from_slice(rgb);
        }
    }

    let world = World::from_bytes(&buf).expect("parse world");
    assert_eq!(world.width(), 1);
    assert_eq!(world.depth(), 1);
    assert_eq!(world.sector_size(), psxed_format::world::SECTOR_SIZE);
    assert_eq!(world.material_count(), 2);
    assert_eq!(world.wall_count(), 1);
    assert_eq!(
        world.surface_light_count(),
        SURFACE_LIGHT_RECORD_COUNT as u16
    );
    assert_eq!(world.ambient_color(), [32, 32, 40]);
    assert!(world.fog_enabled());
    assert!(world.static_vertex_lighting());

    let sector = world.sector(0, 0).unwrap();
    assert!(sector.has_floor());
    assert!(sector.floor_walkable());
    assert_eq!(sector.floor_material(), Some(0));
    assert_eq!(sector.ceiling_material(), None);
    assert_eq!(sector.wall_count(), 1);
    assert_eq!(sector.floor_uvs().corners(), world::FLOOR_UVS);
    assert_eq!(world.surface_light(0).unwrap().vertex_rgb(), floor_light);
    assert_eq!(world.surface_light(1).unwrap().vertex_rgb(), ceiling_light);

    let wall = world.sector_wall(sector, 0).unwrap();
    assert_eq!(wall.direction(), psxed_format::world::direction::NORTH);
    assert!(wall.solid());
    assert_eq!(wall.material(), 1);
    assert_eq!(wall.heights(), [0, 0, 1024, 1024]);
    assert_eq!(wall.uvs().corners(), world::WALL_UVS);
    assert_eq!(world.surface_light(2).unwrap().vertex_rgb(), wall_light);
}

#[test]
fn floor_collision_decode_matches_full_sector_with_override() {
    use psxed_format::world;

    const LEN: usize = psxed_format::AssetHeader::SIZE
        + world::WorldHeader::SIZE
        + world::SectorRecord::SIZE
        + world::HorizontalOverrideRecord::SIZE;
    let mut buf = [0u8; LEN];
    buf[0..4].copy_from_slice(&world::MAGIC);
    buf[4..6].copy_from_slice(&world::VERSION.to_le_bytes());
    buf[8..12].copy_from_slice(&((LEN - psxed_format::AssetHeader::SIZE) as u32).to_le_bytes());
    buf[12..14].copy_from_slice(&1u16.to_le_bytes());
    buf[14..16].copy_from_slice(&1u16.to_le_bytes());
    buf[16..20].copy_from_slice(&world::SECTOR_SIZE.to_le_bytes());
    buf[20..22].copy_from_slice(&1u16.to_le_bytes());
    buf[32..34].copy_from_slice(&1u16.to_le_bytes());

    let sector_offset = psxed_format::AssetHeader::SIZE + world::WorldHeader::SIZE;
    buf[sector_offset] = world::sector_flags::HAS_FLOOR | world::sector_flags::FLOOR_WALKABLE;
    buf[sector_offset + 1] = world::split::NORTH_WEST_SOUTH_EAST;
    for (index, height) in [10i32, 20, 30, 40].into_iter().enumerate() {
        let offset = sector_offset + 12 + index * 4;
        buf[offset..offset + 4].copy_from_slice(&height.to_le_bytes());
    }

    let override_offset = sector_offset + world::SectorRecord::SIZE;
    buf[override_offset + 2] = world::horizontal_surface::FLOOR;
    buf[override_offset + 3] =
        world::horizontal_flags::TRI_A_PRESENT | world::horizontal_flags::TRI_A_WALKABLE;
    for (index, height) in [101i32, 102, 103, 201, 202, 203].into_iter().enumerate() {
        let offset = override_offset + 24 + index * 4;
        buf[offset..offset + 4].copy_from_slice(&height.to_le_bytes());
    }

    let world = World::from_bytes(&buf).expect("parse overridden world");
    let full = world.sector(0, 0).expect("sector");
    for (local_x, local_z) in [
        (0, 0),
        (world::SECTOR_SIZE, 0),
        (0, world::SECTOR_SIZE),
        (world::SECTOR_SIZE, world::SECTOR_SIZE),
    ] {
        let triangle = world_topology::horizontal_triangle_at_local(
            full.floor_split(),
            local_x,
            local_z,
            world::SECTOR_SIZE,
        );
        let reduced = world.sector_floor_collision(0, 0, local_x, local_z, world::SECTOR_SIZE);
        if full.floor_triangle_present(triangle) {
            let reduced = reduced.expect("present triangle");
            assert_eq!(reduced.split(), full.floor_split());
            assert_eq!(reduced.triangle(), triangle);
            assert_eq!(reduced.walkable(), full.floor_triangle_walkable(triangle));
            assert_eq!(reduced.floor_heights(), full.floor_heights());
            assert_eq!(
                reduced.triangle_heights(),
                full.floor_triangle_heights(triangle)
            );
        } else {
            assert!(reduced.is_none());
        }
    }
}

#[test]
fn world_rejects_bad_sector_count() {
    use psxed_format::world;

    let mut buf = [0u8; 12 + psxed_format::world::WorldHeader::SIZE];
    buf[0..4].copy_from_slice(&world::MAGIC);
    buf[4..6].copy_from_slice(&world::VERSION.to_le_bytes());
    buf[8..12].copy_from_slice(&(world::WorldHeader::SIZE as u32).to_le_bytes());
    buf[12..14].copy_from_slice(&1u16.to_le_bytes());
    buf[14..16].copy_from_slice(&1u16.to_le_bytes());
    buf[16..20].copy_from_slice(&world::SECTOR_SIZE.to_le_bytes());
    buf[20..22].copy_from_slice(&2u16.to_le_bytes());

    assert!(matches!(
        World::from_bytes(&buf),
        Err(ParseError::InvalidWorldLayout)
    ));
}

#[test]
fn world_rejects_wall_range_outside_table() {
    use psxed_format::world;

    let payload_len = (world::WorldHeader::SIZE + world::SectorRecord::SIZE) as u32;
    let mut buf = [0u8; psxed_format::AssetHeader::SIZE
        + psxed_format::world::WorldHeader::SIZE
        + psxed_format::world::SectorRecord::SIZE];
    buf[0..4].copy_from_slice(&world::MAGIC);
    buf[4..6].copy_from_slice(&world::VERSION.to_le_bytes());
    buf[8..12].copy_from_slice(&payload_len.to_le_bytes());
    buf[12..14].copy_from_slice(&1u16.to_le_bytes());
    buf[14..16].copy_from_slice(&1u16.to_le_bytes());
    buf[16..20].copy_from_slice(&world::SECTOR_SIZE.to_le_bytes());
    buf[20..22].copy_from_slice(&1u16.to_le_bytes());

    let sector = 12 + world::WorldHeader::SIZE;
    buf[sector + 10..sector + 12].copy_from_slice(&1u16.to_le_bytes());

    assert!(matches!(
        World::from_bytes(&buf),
        Err(ParseError::InvalidWorldLayout)
    ));
}

#[test]
fn parses_legacy_v1_cooked_blob() {
    // Construct a proper v1 blob programmatically: 2 verts, 0 faces.
    // (0 faces is legal; we just want to check header parses.)
    let mut buf: [u8; 12 + 8 + 2 * 6] = [0; 32];
    buf[0..4].copy_from_slice(&psxed_format::mesh::MAGIC);
    buf[4..6].copy_from_slice(&1u16.to_le_bytes());
    buf[6..8].copy_from_slice(&0u16.to_le_bytes());
    buf[8..12].copy_from_slice(&((8 + 2 * 6) as u32).to_le_bytes());
    buf[12..14].copy_from_slice(&2u16.to_le_bytes());
    buf[14..16].copy_from_slice(&0u16.to_le_bytes());
    // Reserved already zero.
    // Vert 0 = (0x0100, 0, 0) -- X at offset 20..22.
    buf[20..22].copy_from_slice(&0x0100_i16.to_le_bytes());
    // Vert 1 = (0, 0x0200, 0) -- Y at offset 28..30
    // (vert 1 starts at 26, Y is offset +2 into the vert).
    buf[28..30].copy_from_slice(&0x0200_i16.to_le_bytes());

    let m = Mesh::from_bytes(&buf).expect("parse");
    assert_eq!(m.vert_count(), 2);
    assert_eq!(m.face_count(), 0);
    let v0 = m.vertex(0);
    assert_eq!(v0.x, 0x0100);
    let v1 = m.vertex(1);
    assert_eq!(v1.y, 0x0200);
}

#[test]
fn parses_v2_u16_indices() {
    const VERTS: usize = 260;
    const FACES: usize = 1;
    const PAYLOAD_LEN: usize = psxed_format::mesh::MeshHeader::SIZE + VERTS * 6 + FACES * 6;
    const INDEX_OFFSET: usize =
        psxed_format::AssetHeader::SIZE + psxed_format::mesh::MeshHeader::SIZE + VERTS * 6;

    let mut buf = [0u8; psxed_format::AssetHeader::SIZE + PAYLOAD_LEN];
    buf[0..4].copy_from_slice(&psxed_format::mesh::MAGIC);
    buf[4..6].copy_from_slice(&psxed_format::mesh::VERSION.to_le_bytes());
    buf[6..8].copy_from_slice(&0u16.to_le_bytes());
    buf[8..12].copy_from_slice(&(PAYLOAD_LEN as u32).to_le_bytes());
    buf[12..14].copy_from_slice(&(VERTS as u16).to_le_bytes());
    buf[14..16].copy_from_slice(&(FACES as u16).to_le_bytes());

    buf[INDEX_OFFSET..INDEX_OFFSET + 2].copy_from_slice(&0u16.to_le_bytes());
    buf[INDEX_OFFSET + 2..INDEX_OFFSET + 4].copy_from_slice(&255u16.to_le_bytes());
    buf[INDEX_OFFSET + 4..INDEX_OFFSET + 6].copy_from_slice(&259u16.to_le_bytes());

    let mesh = Mesh::from_bytes(&buf).expect("parse v2 mesh");
    assert_eq!(mesh.vert_count(), VERTS as u16);
    assert_eq!(mesh.face(0), (0, 255, 259));
}

/// Build a minimal v2 animation: `frames` frames, one joint, each
/// frame's matrix diagonal and translation stamped with `values[f]`.
fn blend_test_animation(values: &[i16]) -> std::vec::Vec<u8> {
    use psxed_format::animation::{AnimationHeader, MAGIC, POSE_RECORD_SIZE, VERSION};
    let payload_len = AnimationHeader::SIZE + values.len() * POSE_RECORD_SIZE;
    let mut out = std::vec::Vec::new();
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(payload_len as u32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // joints
    out.extend_from_slice(&(values.len() as u16).to_le_bytes()); // frames
    out.extend_from_slice(&30u16.to_le_bytes()); // sample rate
    out.extend_from_slice(&0u16.to_le_bytes()); // translation shift
    for &value in values {
        let matrix = [value, 0, 0, 0, value, 0, 0, 0, value];
        for element in matrix {
            out.extend_from_slice(&element.to_le_bytes());
        }
        for translation in [value, value, value] {
            out.extend_from_slice(&translation.to_le_bytes());
        }
    }
    out
}

#[test]
fn model_pose_blend_endpoints_and_midpoint() {
    let from_bytes_a = blend_test_animation(&[1000, 1000]);
    let from_bytes_b = blend_test_animation(&[3000, 3000]);
    let outgoing = Animation::from_bytes(&from_bytes_a).expect("outgoing parses");
    let incoming = Animation::from_bytes(&from_bytes_b).expect("incoming parses");
    let outgoing_sample = outgoing.looped_pose_sample_q12(0).expect("outgoing sample");
    let primary = incoming
        .looped_pose_sample_q12(0)
        .and_then(|sample| sample.pose(0))
        .expect("incoming pose");

    // Alpha 0 shows the outgoing pose.
    let blend = ModelPoseBlend {
        sample: outgoing_sample,
        alpha_q12: 0,
    };
    assert_eq!(blend.blend_toward(primary, 0).matrix[0][0], 1000);

    // Saturated alpha passes the primary through untouched.
    let blend = ModelPoseBlend {
        sample: outgoing_sample,
        alpha_q12: 1 << 12,
    };
    assert_eq!(blend.blend_toward(primary, 0), primary);

    // Midpoint lands halfway, matrix and translation alike.
    let blend = ModelPoseBlend {
        sample: outgoing_sample,
        alpha_q12: 1 << 11,
    };
    let mid = blend.blend_toward(primary, 0);
    assert_eq!(mid.matrix[0][0], 2000);
    assert_eq!(mid.translation.x, 2000);

    // A joint missing from the outgoing clip degrades to the primary.
    let blend = ModelPoseBlend {
        sample: outgoing_sample,
        alpha_q12: 1 << 11,
    };
    assert_eq!(blend.blend_toward(primary, 7), primary);
}

/// The v4 decode exactly as it stood before the interpolation fast path was
/// added, kept as the oracle those changes are checked against.
///
/// Every function here is a verbatim copy of the code it replaced. Nothing in
/// the crate calls it; it exists so the sweeps below can assert that the faster
/// form is not merely close but bit-identical, over the whole domain each piece
/// can reach. A pose that is off by one Q12 unit deforms a character subtly
/// enough to survive a screenshot, so "looks right" is not evidence here.
mod pre_change {
    use super::super::{JointPose, Vec3I16, Vec3I32};

    pub fn decode_q11_element(raw: u16) -> i16 {
        let signed = ((raw << 4) as i16) >> 4;
        signed
            .wrapping_shl(1)
            .wrapping_add((((raw & 0x0FFF) == 0x07FF) as i16) << 1)
    }

    pub fn cross_component_q12(a: i16, b: i16, c: i16, d: i16) -> i16 {
        let value = a as i32 * b as i32 - c as i32 * d as i32;
        let rounded = if value >= 0 {
            value.saturating_add(1 << 11) >> 12
        } else {
            -((value.saturating_neg().saturating_add(1 << 11)) >> 12)
        };
        rounded.clamp(i16::MIN as i32, i16::MAX as i32) as i16
    }

    pub fn reconstruct_third_basis_q12(first: [i16; 3], second: [i16; 3]) -> [i16; 3] {
        [
            cross_component_q12(first[1], second[2], first[2], second[1]),
            cross_component_q12(first[2], second[0], first[0], second[2]),
            cross_component_q12(first[0], second[1], first[1], second[0]),
        ]
    }

    pub fn apply_v4_basis_correction(mut third: [i16; 3], correction_byte: u8) -> [i16; 3] {
        let axis = usize::from(correction_byte & 0x03);
        if axis < 3 {
            let correction = (((correction_byte >> 2) << 2) as i8 >> 2) as i16;
            third[axis] = third[axis].saturating_add(correction);
        }
        for component in &mut third {
            *component = (*component).clamp(-4096, 4096);
        }
        third
    }

    /// The whole 16-byte v4 record, byte addressed so alignment cannot matter.
    pub fn read_v4_record(record: &[u8]) -> ([[i16; 3]; 3], Vec3I16) {
        let mut flat = [0i16; 6];
        for pair in 0..3 {
            let o = pair * 3;
            let packed = (record[o] as u32) | ((record[o + 1] as u32) << 8)
                | ((record[o + 2] as u32) << 16);
            flat[pair * 2] = decode_q11_element((packed & 0x0fff) as u16);
            flat[pair * 2 + 1] = decode_q11_element(((packed >> 12) & 0x0fff) as u16);
        }
        let first = [flat[0], flat[1], flat[2]];
        let second = [flat[3], flat[4], flat[5]];
        let third =
            apply_v4_basis_correction(reconstruct_third_basis_q12(first, second), record[9]);
        let translation = Vec3I16::new(
            i16::from_le_bytes([record[10], record[11]]),
            i16::from_le_bytes([record[12], record[13]]),
            i16::from_le_bytes([record[14], record[15]]),
        );
        ([first, second, third], translation)
    }

    pub fn decode_packed_translation(value: i16, shift: u8) -> i32 {
        (value as i32) * (1i32 << shift)
    }

    pub fn scale_i32_q12(value: i32, scale_q12: i32) -> i32 {
        let whole = value >> 12;
        let frac = value - (whole << 12);
        whole.saturating_mul(scale_q12) + ((frac * scale_q12) >> 12)
    }

    pub fn lerp_i32_q12(a: i32, b: i32, alpha_q12: u16) -> i32 {
        let delta = b.saturating_sub(a);
        a.saturating_add(scale_i32_q12(delta, alpha_q12 as i32))
    }

    pub fn lerp_i16_q12(a: i16, b: i16, alpha_q12: u16) -> i16 {
        (a as i32 + (((b as i32 - a as i32) * alpha_q12 as i32) >> 12)) as i16
    }

    pub fn joint_pose(record: &[u8], shift: u8) -> JointPose {
        let (matrix, packed) = read_v4_record(record);
        JointPose {
            matrix,
            translation: Vec3I32::new(
                decode_packed_translation(packed.x, shift),
                decode_packed_translation(packed.y, shift),
                decode_packed_translation(packed.z, shift),
            ),
        }
    }

    /// `AnimationPoseSample::pose` as it behaved before the change: decode both
    /// frames independently, then lerp the two `JointPose` values.
    pub fn interpolated(a: &[u8], b: &[u8], shift: u8, alpha_q12: u16) -> JointPose {
        let a = joint_pose(a, shift);
        let b = joint_pose(b, shift);
        let mut matrix = [[0i16; 3]; 3];
        for col in 0..3 {
            for row in 0..3 {
                matrix[col][row] = lerp_i16_q12(a.matrix[col][row], b.matrix[col][row], alpha_q12);
            }
        }
        JointPose {
            matrix,
            translation: Vec3I32::new(
                lerp_i32_q12(a.translation.x, b.translation.x, alpha_q12),
                lerp_i32_q12(a.translation.y, b.translation.y, alpha_q12),
                lerp_i32_q12(a.translation.z, b.translation.z, alpha_q12),
            ),
        }
    }
}

/// Deterministic xorshift, so a failure reproduces exactly.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
}

#[test]
fn wide_q11_decode_equals_narrow_for_every_code() {
    for raw in 0..=0x0fffu16 {
        let narrow = super::decode_q11_element(raw);
        assert_eq!(narrow, pre_change::decode_q11_element(raw), "code {raw:#05x}");
        assert_eq!(
            super::decode_q11_element_wide(raw),
            narrow as i32,
            "code {raw:#05x} widened"
        );
        // The reconstruction and the interpolation both rely on this bound to
        // drop their saturating arithmetic.
        assert!((-4096..=4096).contains(&super::decode_q11_element_wide(raw)));
    }
}

#[test]
fn cross_rounding_matches_the_saturating_form_across_its_whole_domain() {
    // `cross_round_q12` only ever sees `a*b - c*d` over decoded Q11 elements,
    // so sweep every value that expression can produce.
    let mut value = -super::V4_CROSS_LIMIT;
    while value <= super::V4_CROSS_LIMIT {
        let expected = if value >= 0 {
            value.saturating_add(1 << 11) >> 12
        } else {
            -((value.saturating_neg().saturating_add(1 << 11)) >> 12)
        };
        assert_eq!(super::cross_round_q12(value), expected, "value {value}");
        value += 1;
    }
}

#[test]
fn third_basis_reconstruction_matches_the_array_form() {
    let interesting: [i16; 9] = [-4096, -4095, -2048, -1, 0, 1, 2048, 4094, 4096];
    let mut rng = Rng(0x5eed_1234);
    let mut checked = 0u64;

    // Every corner of the element range against every correction byte.
    for &f0 in &interesting {
        for &s2 in &interesting {
            for correction_byte in 0..=255u8 {
                let first = [f0, interesting[(correction_byte as usize) % 9], -s2];
                let second = [s2, f0, interesting[(f0.unsigned_abs() as usize) % 9]];
                let expected = pre_change::apply_v4_basis_correction(
                    pre_change::reconstruct_third_basis_q12(first, second),
                    correction_byte,
                );
                let actual = super::v4_third_basis_q12(
                    [first[0] as i32, first[1] as i32, first[2] as i32],
                    [second[0] as i32, second[1] as i32, second[2] as i32],
                    correction_byte,
                );
                assert_eq!(
                    [actual[0] as i16, actual[1] as i16, actual[2] as i16],
                    expected,
                    "first {first:?} second {second:?} correction {correction_byte:#04x}"
                );
                checked += 1;
            }
        }
    }

    // Plus a wide random sweep over the full decoded-element range.
    for _ in 0..200_000 {
        let mut element = || {
            super::decode_q11_element((rng.next() & 0x0fff) as u16)
        };
        let first = [element(), element(), element()];
        let second = [element(), element(), element()];
        let correction_byte = (rng.next() & 0xff) as u8;
        let expected = pre_change::apply_v4_basis_correction(
            pre_change::reconstruct_third_basis_q12(first, second),
            correction_byte,
        );
        let actual = super::v4_third_basis_q12(
            [first[0] as i32, first[1] as i32, first[2] as i32],
            [second[0] as i32, second[1] as i32, second[2] as i32],
            correction_byte,
        );
        assert_eq!(
            [actual[0] as i16, actual[1] as i16, actual[2] as i16],
            expected,
            "first {first:?} second {second:?} correction {correction_byte:#04x}"
        );
        checked += 1;
    }
    assert!(checked > 200_000);
}

#[test]
fn wide_element_lerp_matches_the_i16_lerp() {
    let alphas = [0u16, 1, 2, 2047, 2048, 2049, 4093, 4094, 4095];
    let mut a = -4096i32;
    while a <= 4096 {
        let mut b = -4096i32;
        while b <= 4096 {
            for &alpha in &alphas {
                assert_eq!(
                    super::lerp_q12_wide_to_i16(a, b, alpha),
                    pre_change::lerp_i16_q12(a as i16, b as i16, alpha),
                    "a {a} b {b} alpha {alpha}"
                );
            }
            b += 37;
        }
        a += 31;
    }
}

#[test]
fn packed_translation_lerp_matches_the_saturating_form() {
    let endpoints: [i16; 11] = [
        i16::MIN,
        i16::MIN + 1,
        -30_000,
        -4096,
        -1,
        0,
        1,
        4096,
        30_000,
        i16::MAX - 1,
        i16::MAX,
    ];
    let alphas = [0u16, 1, 2, 1023, 2047, 2048, 2049, 4093, 4094, 4095];
    for shift in 0..=15u8 {
        for &a in &endpoints {
            for &b in &endpoints {
                for &alpha in &alphas {
                    let expected = pre_change::lerp_i32_q12(
                        pre_change::decode_packed_translation(a, shift),
                        pre_change::decode_packed_translation(b, shift),
                        alpha,
                    );
                    assert_eq!(
                        super::lerp_packed_translation_q12(a, b, shift, alpha),
                        expected,
                        "a {a} b {b} shift {shift} alpha {alpha}"
                    );
                }
            }
        }
    }

    let mut rng = Rng(0x1234_5eed);
    for _ in 0..400_000 {
        let a = (rng.next() & 0xffff) as u16 as i16;
        let b = (rng.next() & 0xffff) as u16 as i16;
        let shift = (rng.next() % 16) as u8;
        let alpha = (rng.next() & 0x0fff) as u16;
        let expected = pre_change::lerp_i32_q12(
            pre_change::decode_packed_translation(a, shift),
            pre_change::decode_packed_translation(b, shift),
            alpha,
        );
        assert_eq!(
            super::lerp_packed_translation_q12(a, b, shift, alpha),
            expected,
            "a {a} b {b} shift {shift} alpha {alpha}"
        );
    }
}

/// Build a word-aligned v4 clip whose pose records are the given bytes.
fn v4_clip(joint_count: u16, frame_count: u16, shift: u16, records: &[u8]) -> std::vec::Vec<u8> {
    use psxed_format::animation;
    let payload_len = animation::AnimationHeader::SIZE + records.len();
    let mut blob = std::vec::Vec::new();
    blob.extend_from_slice(&animation::MAGIC);
    blob.extend_from_slice(&animation::VERSION_V4.to_le_bytes());
    blob.extend_from_slice(&0u16.to_le_bytes());
    blob.extend_from_slice(&(payload_len as u32).to_le_bytes());
    blob.extend_from_slice(&joint_count.to_le_bytes());
    blob.extend_from_slice(&frame_count.to_le_bytes());
    blob.extend_from_slice(&15u16.to_le_bytes());
    blob.extend_from_slice(&shift.to_le_bytes());
    blob.extend_from_slice(records);
    blob
}

#[test]
fn v4_interpolated_pose_is_bit_identical_to_the_pre_change_decode() {
    use psxed_format::animation::POSE_RECORD_SIZE_V4 as REC;

    const JOINTS: u16 = 6;
    const FRAMES: u16 = 5;
    let mut rng = Rng(0xc0ff_ee11);
    let mut compared = 0u64;

    for shift in [0u16, 1, 4, 9, 15] {
        let mut records =
            std::vec![0u8; JOINTS as usize * FRAMES as usize * REC];
        for byte in records.iter_mut() {
            *byte = (rng.next() >> 7) as u8;
        }
        let blob = v4_clip(JOINTS, FRAMES, shift, &records);
        let animation = Animation::from_bytes(&blob).expect("v4 clip parses");
        assert_eq!(animation.poses.as_ptr() as usize & 3, 0);

        // Every joint, every looping frame pair, and alphas covering the
        // zero-alpha fast path, both ends and the interior.
        for whole in 0..u32::from(FRAMES) + 2 {
            for frac in [0u32, 1, 2, 1365, 2048, 2731, 4093, 4094, 4095] {
                let phase = (whole << 12) | frac;
                let sample = animation.looped_pose_sample_q12(phase).unwrap();
                for joint in 0..JOINTS {
                    let stride = joint as usize * REC;
                    let a = &records[sample.base_frame_offset + stride..][..REC];
                    let b = &records[sample.next_frame_offset + stride..][..REC];
                    let expected = if sample.alpha_q12 == 0
                        || sample.base_frame == sample.next_frame
                    {
                        pre_change::joint_pose(a, shift as u8)
                    } else {
                        pre_change::interpolated(a, b, shift as u8, sample.alpha_q12)
                    };
                    assert_eq!(
                        sample.pose(joint),
                        Some(expected),
                        "shift {shift} phase {phase:#x} joint {joint}"
                    );
                    // `pose_looped_q12` is the same decode through the other
                    // public entry point.
                    assert_eq!(animation.pose_looped_q12(phase, joint), Some(expected));
                    compared += 1;
                }
            }
        }
    }
    assert!(compared > 1_000);
}

#[test]
fn v4_fast_path_and_unaligned_pool_agree_on_random_records() {
    use psxed_format::animation::POSE_RECORD_SIZE_V4 as REC;

    // The fast path is taken only for a word-aligned pool. Copying the same
    // clip to every other alignment exercises the generic fallback and proves
    // the two produce identical poses.
    const JOINTS: u16 = 3;
    const FRAMES: u16 = 4;
    let mut rng = Rng(0x0bad_f00d);
    let mut records = std::vec![0u8; JOINTS as usize * FRAMES as usize * REC];
    for byte in records.iter_mut() {
        *byte = (rng.next() >> 11) as u8;
    }
    let blob = v4_clip(JOINTS, FRAMES, 7, &records);
    let aligned = Animation::from_bytes(&blob).expect("aligned clip parses");
    assert_eq!(aligned.poses.as_ptr() as usize & 3, 0);

    for skew in 1..4usize {
        let mut storage = std::vec![0u8; blob.len() + 4];
        let prefix = (skew + 4 - (storage.as_ptr() as usize & 3)) & 3;
        storage[prefix..prefix + blob.len()].copy_from_slice(&blob);
        let bytes = &storage[prefix..prefix + blob.len()];
        let skewed = Animation::from_bytes(bytes).expect("skewed clip parses");
        for whole in 0..u32::from(FRAMES) + 1 {
            for frac in [0u32, 1, 999, 2048, 4095] {
                let phase = (whole << 12) | frac;
                let a = aligned.looped_pose_sample_q12(phase).unwrap();
                let b = skewed.looped_pose_sample_q12(phase).unwrap();
                for joint in 0..JOINTS {
                    assert_eq!(a.pose(joint), b.pose(joint), "phase {phase:#x} joint {joint}");
                }
            }
        }
    }
}
