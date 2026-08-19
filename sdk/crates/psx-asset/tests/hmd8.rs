//! HMD8 parser check.
//!
//! Builds a minimal blob by hand, reads it back, and confirms the loader
//! rejects a damaged header instead of trusting it. That guard is the whole
//! reason the reader is safe to point at streamed data: every per-field read
//! after `load` is unchecked, so validation is the only thing standing between
//! a short chunk and a wild dereference.
//!
//! The 12-bit rotation-code decode has its own unit tests inside the module.
//!
//! Real cooked chunks are Half-Life-derived, so none are committed here. Point
//! `HMD8_FIXTURE_DIR` at a directory of `.psxm` chunks to run the same
//! invariants over real data.

use psx_asset::hmd8::Model;

const HEADER: usize = 36;

const N_VERTS: usize = 4;
const N_TRIS: usize = 2;
const N_BONES: usize = 2;
const N_RANGES: usize = 2;
const N_FRAMES: usize = 2;
const N_CLIPS: usize = 1;

/// Q11 code for 1.0: the decoder maps the top code to a full Q12 unit.
const Q11_ONE: u16 = 2047;

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Nine 12-bit codes little-endian-packed into fourteen bytes, then the i16
/// translation. This is the 20-byte record both HMD8 and `.psxanim` v3 use.
fn identity_affine(translation: [i16; 3]) -> Vec<u8> {
    let codes = [Q11_ONE, 0, 0, 0, Q11_ONE, 0, 0, 0, Q11_ONE];
    let mut bits = [0u8; 14];
    for (i, code) in codes.iter().enumerate() {
        let start = i * 12;
        let value = u32::from(*code) << (start % 8);
        for byte in 0..3 {
            let index = start / 8 + byte;
            if index < bits.len() {
                bits[index] |= ((value >> (byte * 8)) & 0xff) as u8;
            }
        }
    }
    let mut out = bits.to_vec();
    for axis in translation {
        put_u16(&mut out, axis as u16);
    }
    out
}

fn build_blob() -> Vec<u8> {
    let ranges_off = HEADER + N_CLIPS * 4;
    let model_data_len = N_RANGES * 8 + N_VERTS * 6 + N_FRAMES * N_BONES * 20;

    let mut out = Vec::new();
    out.extend_from_slice(b"HMD8");
    put_u32(&mut out, N_VERTS as u32);
    put_u32(&mut out, N_TRIS as u32);
    put_u32(&mut out, 1); // low half textures, high half hitboxes
    put_u32(&mut out, N_FRAMES as u32);
    put_u32(&mut out, N_CLIPS as u32);
    put_u32(&mut out, model_data_len as u32);
    put_u16(&mut out, 4096); // local_to_world_q12, identity
    put_u16(&mut out, 0); // no optional sections
    put_u16(&mut out, N_BONES as u16);
    put_u16(&mut out, N_RANGES as u16);
    assert_eq!(out.len(), HEADER);

    // one clip covering both frames
    put_u16(&mut out, 0); // first frame
    put_u16(&mut out, N_FRAMES as u16); // frame count, low byte
    assert_eq!(out.len(), ranges_off);

    // two ranges, one per bone, splitting the vertices evenly
    for bone in 0..N_RANGES {
        put_u16(&mut out, (bone * 2) as u16); // first
        put_u16(&mut out, 2); // count
        put_u16(&mut out, bone as u16); // bone
        put_u16(&mut out, 0); // body mask + range flags
    }

    // bone-local vertices, distinct so a mis-strided read shows up
    for v in 0..N_VERTS {
        for axis in 0..3 {
            put_u16(&mut out, (v * 10 + axis) as u16);
        }
    }

    // identity pose for every bone of every frame, translation walking away
    // from the origin so an off-by-one frame index is visible
    for frame in 0..N_FRAMES {
        for bone in 0..N_BONES {
            let t = (frame * 100 + bone * 10) as i16;
            out.extend_from_slice(&identity_affine([t, t, t]));
        }
    }
    assert_eq!(out.len(), ranges_off + model_data_len);

    // triangles: indices then the packed UV/normal/mask tail
    for t in 0..N_TRIS {
        put_u16(&mut out, (t * 2) as u16);
        put_u16(&mut out, (t * 2 + 1) as u16);
        put_u16(&mut out, (t * 2) as u16);
        out.extend_from_slice(&[0u8; 10]);
    }
    out
}

fn load(bytes: Vec<u8>) -> Model {
    Model::load(Box::leak(bytes.into_boxed_slice()))
}

#[test]
fn parses_a_minimal_blob() {
    let model = load(build_blob());

    assert_eq!(model.n_verts, N_VERTS);
    assert_eq!(model.n_tris, N_TRIS);
    assert_eq!(model.n_bones, N_BONES);
    assert_eq!(model.n_ranges, N_RANGES);
    assert_eq!(model.n_frames, N_FRAMES);
    assert_eq!(model.local_to_world_q12(), 4096);
    assert_eq!(model.clip_len(0), N_FRAMES);

    // bone ranges partition the vertices; this is what lets a range pay one
    // matrix load, so a broken stride here silently skins to the wrong bone
    let mut covered = 0;
    for i in 0..model.n_ranges {
        let range = model.range(i);
        assert_eq!(range.bone, i);
        assert_eq!(range.first, covered);
        covered += range.count;
    }
    assert_eq!(covered, N_VERTS);

    for v in 0..model.n_verts {
        let vert = model.vert(v);
        assert_eq!(
            [vert.x, vert.y, vert.z],
            [(v * 10) as i16, (v * 10 + 1) as i16, (v * 10 + 2) as i16]
        );
    }

    for t in 0..model.n_tris {
        assert_eq!(model.tri(t).idx, [(t * 2) as u16, (t * 2 + 1) as u16, (t * 2) as u16]);
    }
}

#[test]
fn decodes_poses_and_interpolates_between_frames() {
    let model = load(build_blob());

    let a = model.frame(0);
    let b = model.frame(1);
    for bone in 0..model.n_bones {
        let pose = a.interpolate(a, 0).bone(bone, false, 0);
        assert_eq!(pose.rotation.m[0][0], 4096);
        assert_eq!(pose.rotation.m[1][1], 4096);
        assert_eq!(pose.rotation.m[2][2], 4096);
        assert_eq!(pose.rotation.m[0][1], 0);
        assert_eq!(pose.translation.x, (bone * 10) as i16);
    }

    // halfway between frame 0 (t = 0) and frame 1 (t = 100) for bone 0
    let half = a.interpolate(b, 8).bone(0, false, 0);
    assert_eq!(half.translation.x, 50);
    assert_eq!(half.rotation.m[0][0], 4096, "identity must survive the blend");
}

#[test]
fn rejects_damage_instead_of_trusting_it() {
    let empty = |bytes: Vec<u8>| {
        let model = load(bytes);
        assert_eq!(model.n_verts, 0, "damaged blob must load as the null model");
        assert_eq!(model.n_tris, 0);
        assert_eq!(model.n_bones, 0);
    };

    empty(b"HMD8".to_vec()); // shorter than the header
    empty(build_blob()[..8].to_vec());

    let mut wrong_magic = build_blob();
    wrong_magic[0] = b'X';
    empty(wrong_magic);

    // a chunk that streamed in short: the counts are fine, the bytes are not
    let full = build_blob();
    empty(full[..full.len() - 1].to_vec());

    // a range pointing past the vertex stream, which is the read that would go
    // wild if the loader took the header at its word
    let mut bad_range = build_blob();
    let ranges_off = HEADER + N_CLIPS * 4;
    bad_range[ranges_off..ranges_off + 2].copy_from_slice(&u16::MAX.to_le_bytes());
    empty(bad_range);

    // a bone index outside the pose stream
    let mut bad_bone = build_blob();
    bad_bone[ranges_off + 4..ranges_off + 6].copy_from_slice(&99u16.to_le_bytes());
    empty(bad_bone);

    // an unsupported model-space scale, which would misplace every vertex
    let mut bad_scale = build_blob();
    bad_scale[28..30].copy_from_slice(&777u16.to_le_bytes());
    empty(bad_scale);

    // more vertices than the caller's arena can hold
    let model = Model::load_with_vertex_cap(Box::leak(build_blob().into_boxed_slice()), 2);
    assert_eq!(model.n_verts, 0, "vertex cap must reject an oversized cook");
}

/// Run the same invariants over real cooked chunks, which are not committed:
///
/// ```text
/// HMD8_FIXTURE_DIR=~/path/to/modelpack cargo test -p psx-asset --test hmd8
/// ```
#[test]
fn real_chunks_hold_their_invariants() {
    let Ok(dir) = std::env::var("HMD8_FIXTURE_DIR") else {
        return;
    };

    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("fixture dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_none_or(|e| e != "psxm") {
            continue;
        }
        let bytes = std::fs::read(&path).expect("chunk");
        // Models ship inside an HMRG container (magic, geometry length, blob),
        // but a model pack also carries texture chunks under the same
        // extension, so select on the inner magic rather than the filename.
        let blob = if bytes.starts_with(b"HMRG") {
            bytes[8..].to_vec()
        } else {
            bytes
        };
        if !blob.starts_with(b"HMD8") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let model = load(blob);
        assert!(model.n_bones > 0, "{name} failed to load");

        let mut covered = 0;
        for i in 0..model.n_ranges {
            let range = model.range(i);
            assert!(range.bone < model.n_bones, "{name} range {i} bone");
            assert!(range.first + range.count <= model.n_verts, "{name} range {i}");
            covered += range.count;
        }
        assert!(covered <= model.n_verts, "{name} ranges overlap the stream");

        for clip in 0..model.n_clips {
            let len = model.clip_len(clip);
            assert!(len > 0, "{name} clip {clip} empty");
            for local in 0..len {
                assert!(
                    model.clip_frame(clip, local) < model.n_frames,
                    "{name} clip {clip} frame {local} out of range"
                );
            }
        }

        for t in 0..model.n_tris {
            for index in model.tri(t).idx {
                assert!((index as usize) < model.n_verts, "{name} tri {t} index");
            }
        }
        checked += 1;
    }
    assert!(checked > 0, "no .psxm chunks in {dir}");
}
