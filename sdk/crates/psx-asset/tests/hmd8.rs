//! HMD8 parser check.
//!
//! Builds a minimal blob by hand, reads it back, and confirms the loader
//! rejects a damaged header instead of trusting it. The hot accessors read
//! through offsets that validation proved, so a damaged chunk loads as the
//! null model; their bounds check is against the blob itself, which the
//! out-of-range tests below exercise.
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
    build_blob_with_vertices(N_VERTS)
}

/// The same model with `vertex_count` vertices (even), split between the two bones.
fn build_blob_with_vertices(vertex_count: usize) -> Vec<u8> {
    let ranges_off = HEADER + N_CLIPS * 4;
    let model_data_len = N_RANGES * 8 + vertex_count * 6 + N_FRAMES * N_BONES * 20;

    let mut out = Vec::new();
    out.extend_from_slice(b"HMD8");
    put_u32(&mut out, vertex_count as u32);
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
        put_u16(&mut out, (bone * vertex_count / 2) as u16); // first
        put_u16(&mut out, (vertex_count / 2) as u16); // count
        put_u16(&mut out, bone as u16); // bone
        put_u16(&mut out, 0); // body mask + range flags
    }

    // bone-local vertices, distinct so a mis-strided read shows up
    for v in 0..vertex_count {
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
    Model::from_bytes(Box::leak(bytes.into_boxed_slice()))
}

#[test]
fn parses_a_minimal_blob() {
    let model = load(build_blob());

    assert_eq!(model.vertex_count(), N_VERTS);
    assert_eq!(model.triangle_count(), N_TRIS);
    assert_eq!(model.bone_count(), N_BONES);
    assert_eq!(model.bone_range_count(), N_RANGES);
    assert_eq!(model.frame_count(), N_FRAMES);
    assert_eq!(model.local_to_world_q12(), 4096);
    assert_eq!(model.clip_len(0), N_FRAMES);

    // bone ranges partition the vertices; this is what lets a range pay one
    // matrix load, so a broken stride here silently skins to the wrong bone
    let mut covered = 0;
    for i in 0..model.bone_range_count() {
        let range = model.bone_range(i);
        assert_eq!(range.bone, i);
        assert_eq!(range.first, covered);
        covered += range.count;
    }
    assert_eq!(covered, N_VERTS);

    for v in 0..model.vertex_count() {
        let vert = model.vertex(v);
        assert_eq!(
            [vert.x, vert.y, vert.z],
            [(v * 10) as i16, (v * 10 + 1) as i16, (v * 10 + 2) as i16]
        );
    }

    for t in 0..model.triangle_count() {
        assert_eq!(
            model.triangle(t).idx,
            [(t * 2) as u16, (t * 2 + 1) as u16, (t * 2) as u16]
        );
    }
}

/// The checked accessors guard memory with the blob's own length, so an
/// index past the tables, the empty model, or a public count overwritten by
/// safe code all read inert values instead of outside the blob.
#[test]
fn accessors_stay_inside_the_blob() {
    let model = load(build_blob());
    for far in [N_VERTS * 1000, usize::MAX / 8] {
        let v = model.vertex(far);
        assert_eq!([v.x, v.y, v.z], [0, 0, 0]);
        let w = model.vertex_gte_words(far);
        assert_eq!((w.xy, w.z), (0, 0));
    }
    for far in [N_TRIS * 1000, usize::MAX / 32] {
        let tri = model.triangle(far);
        assert_eq!(tri.idx, [0; 3]);
        assert_eq!(tri.body_mask, 0, "an out-of-range face must be skipped");
        assert_eq!(model.triangle_uv_words(far), [0; 3]);
        assert_eq!(model.triangle_normal(far), [0; 3]);
    }

    // The public counts are plain fields; writing them must not widen what
    // the accessors read.
    let mut lying = model;
    #[allow(deprecated)] // overwrites the deprecated public counts on purpose
    {
        lying.n_tris = 1 << 20;
        lying.n_verts = 1 << 20;
        lying.n_ranges = 1 << 20;
        lying.n_bones = 1 << 20;
        lying.n_frames = 1 << 20;
        lying.n_clips = 1 << 20;
    }
    assert_eq!(lying.triangle(1 << 19).body_mask, 0);
    assert_eq!(lying.vertex(1 << 19).x, 0);
    let _ = lying.bone_range(1 << 19);
    let _ = lying.clip_len(1 << 19);
    let _ = lying.clip_frame(1 << 19, 3);
    let _ = lying
        .frame(1 << 19)
        .interpolate(lying.frame(0), 8)
        .bone(1 << 19, false, 0);

    let empty = Model::EMPTY;
    assert_eq!(empty.vertex(0).x, 0);
    assert_eq!(empty.triangle(0).body_mask, 0);
    assert_eq!(empty.bone_range(0).count, 0);
    assert_eq!(empty.hitbox(0).bone, 0);
    let pose = empty
        .frame(0)
        .interpolate(empty.frame(0), 0)
        .bone(0, false, 0);
    assert_eq!(pose.rotation.m[0][0], 4096, "empty model poses as identity");
}

/// In range, the unchecked forms read exactly what the checked ones do.
#[test]
fn unchecked_accessors_match_the_checked_ones() {
    let model = load(build_blob());
    for v in 0..model.vertex_count() {
        // SAFETY: `v` is below the loaded vertex count.
        let (a, b) = unsafe {
            (
                model.vertex_unchecked(v),
                model.vertex_gte_words_unchecked(v),
            )
        };
        let (c, d) = (model.vertex(v), model.vertex_gte_words(v));
        assert_eq!([a.x, a.y, a.z], [c.x, c.y, c.z]);
        assert_eq!((b.xy, b.z), (d.xy, d.z));
    }
    for t in 0..model.triangle_count() {
        // SAFETY: `t` is below the loaded triangle count.
        let (a, uv, n) = unsafe {
            (
                model.triangle_unchecked(t),
                model.triangle_uv_words_unchecked(t),
                model.triangle_normal_unchecked(t),
            )
        };
        assert_eq!(a.idx, model.triangle(t).idx);
        assert_eq!(uv, model.triangle_uv_words(t));
        assert_eq!(n, model.triangle_normal(t));
    }
}

#[test]
fn decodes_poses_and_interpolates_between_frames() {
    let model = load(build_blob());

    let a = model.frame(0);
    let b = model.frame(1);
    for bone in 0..model.bone_count() {
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
    assert_eq!(
        half.rotation.m[0][0], 4096,
        "identity must survive the blend"
    );
}

#[test]
fn rejects_damage_instead_of_trusting_it() {
    let empty = |bytes: Vec<u8>| {
        let model = load(bytes);
        assert_eq!(
            model.vertex_count(),
            0,
            "damaged blob must load as the null model"
        );
        assert_eq!(model.triangle_count(), 0);
        assert_eq!(model.bone_count(), 0);
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
    let model = Model::from_bytes_with_vertex_cap(Box::leak(build_blob().into_boxed_slice()), 2);
    assert_eq!(
        model.vertex_count(),
        0,
        "vertex cap must reject an oversized cook"
    );
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
    let mut with_tracks = 0;
    let mut soa_chunks = 0;
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
        let soa = u16::from_le_bytes([blob[30], blob[31]]) & FLAG_VERTEX_SOA != 0;
        let model = Model::from_bytes(leak_at(&blob, 0));
        assert!(model.bone_count() > 0, "{name} failed to load");

        let mut covered = 0;
        for i in 0..model.bone_range_count() {
            let range = model.bone_range(i);
            assert!(range.bone < model.bone_count(), "{name} range {i} bone");
            assert!(
                range.first + range.count <= model.vertex_count(),
                "{name} range {i}"
            );
            covered += range.count;
        }
        assert!(
            covered <= model.vertex_count(),
            "{name} ranges overlap the stream"
        );

        let words = model.vertex_words();
        if soa {
            soa_chunks += 1;
            assert_eq!(words.len(), model.vertex_count(), "{name} vertex words");
            for (v, w) in words.iter().enumerate() {
                let c = model.vertex_gte_words(v);
                assert_eq!((w.xy, w.z), (c.xy, c.z), "{name} vertex {v}");
            }
        }

        for clip in 0..model.clip_count() {
            let len = model.clip_len(clip);
            assert!(len > 0, "{name} clip {clip} empty");
            for local in 0..len {
                assert!(
                    model.clip_frame(clip, local) < model.frame_count(),
                    "{name} clip {clip} frame {local} out of range"
                );
            }
        }

        for t in 0..model.triangle_count() {
            for index in model.triangle(t).idx {
                assert!(
                    (index as usize) < model.vertex_count(),
                    "{name} tri {t} index"
                );
            }
        }
        if model.has_tracks() {
            let tracks = model.tracks().expect("validated tracks");
            let mut scratch = vec![psx_asset::hma1::Affine::ZERO; tracks.model.bone_count()];
            for clip in 0..tracks.model.clip_count() {
                let end = tracks.model.clip_intervals(clip) * 256;
                for pos in [0, end / 3, end, end + 999] {
                    tracks.model.decode(clip, pos, &mut scratch);
                }
            }
            with_tracks += 1;
        }
        checked += 1;
    }
    eprintln!("{checked} HMD8 chunks, {with_tracks} with HMA1 tracks, {soa_chunks} SoA");
    assert!(checked > 0, "no .psxm chunks in {dir}");
}

/// `HMD_FLAG_VERTEX_SOA`: the stream every current cook emits.
const FLAG_VERTEX_SOA: u16 = 1 << 7;

/// The same model as [`build_blob`] with its vertices in struct-of-arrays
/// form: every packed XY word, then every Z halfword. The header and clip
/// table end on a word boundary, so no padding precedes the ranges.
fn build_soa_blob() -> Vec<u8> {
    let mut out = build_blob();
    out[30..32].copy_from_slice(&FLAG_VERTEX_SOA.to_le_bytes());
    let vertices_off = HEADER + N_CLIPS * 4 + N_RANGES * 8;
    assert_eq!(vertices_off % 4, 0);
    let mut stream = Vec::new();
    for v in 0..N_VERTS {
        let (x, y) = ((v * 10) as u16, (v * 10 + 1) as u16);
        put_u32(&mut stream, u32::from(x) | (u32::from(y) << 16));
    }
    for v in 0..N_VERTS {
        put_u16(&mut stream, (v * 10 + 2) as u16);
    }
    out[vertices_off..vertices_off + N_VERTS * 6].copy_from_slice(&stream);
    out
}

/// Leak `blob` at an address `misalign` bytes past a four-byte boundary.
fn leak_at(blob: &[u8], misalign: usize) -> &'static [u8] {
    let buf: &'static mut [u8] = Box::leak(vec![0u8; blob.len() + 8].into_boxed_slice());
    let start = buf.as_ptr().align_offset(4) + misalign;
    buf[start..start + blob.len()].copy_from_slice(blob);
    &buf[start..start + blob.len()]
}

#[test]
fn soa_vertex_words_cover_exactly_the_loaded_stream() {
    let model = Model::from_bytes(leak_at(&build_soa_blob(), 0));
    assert_eq!(model.vertex_count(), N_VERTS);
    let words = model.vertex_words();
    assert_eq!(words.len(), model.vertex_count());
    for v in 0..N_VERTS {
        let (a, b) = (words.get(v).expect("in range"), model.vertex_gte_words(v));
        assert_eq!((a.xy, a.z), (b.xy, b.z));
        let (p, q) = (a.position(), model.vertex(v));
        assert_eq!([p.x, p.y, p.z], [q.x, q.y, q.z]);
        assert_eq!(
            [p.x, p.y, p.z],
            [(v * 10) as i16, (v * 10 + 1) as i16, (v * 10 + 2) as i16]
        );
    }
    assert!(words.get(N_VERTS).is_none());

    // Groups of three then the remainder visit every vertex once, in order.
    let (groups, rest) = words.triples();
    let mut seen: Vec<u32> = groups.flatten().map(|w| w.xy).collect();
    assert_eq!(seen.len(), N_VERTS / 3 * 3);
    assert_eq!(rest.len(), N_VERTS % 3);
    seen.extend(rest.iter().map(|w| w.xy));
    let all: Vec<u32> = words.iter().map(|w| w.xy).collect();
    assert_eq!(seen, all);

    // Every bone range slices cleanly; a range past the stream does not.
    for i in 0..model.bone_range_count() {
        let range = model.bone_range(i);
        let part = words
            .slice(range.first..range.first + range.count)
            .expect("range in stream");
        assert_eq!(part.len(), range.count);
        assert_eq!(
            part.get(0).map(|w| w.xy),
            words.get(range.first).map(|w| w.xy)
        );
    }
    assert!(words.slice(0..N_VERTS + 1).is_none());
    assert!(words.slice(N_VERTS..N_VERTS).is_some_and(|w| w.is_empty()));
}

/// The view's length comes from the offsets `from_bytes` validated, not from
/// the public count safe code can overwrite.
#[test]
fn soa_vertex_words_ignore_an_overwritten_count() {
    let mut lying = Model::from_bytes(leak_at(&build_soa_blob(), 0));
    #[allow(deprecated)] // overwrites the deprecated public count on purpose
    {
        lying.n_verts = 1 << 20;
    }
    assert_eq!(lying.vertex_words().len(), N_VERTS);
}

#[test]
fn malformed_soa_blobs_load_with_no_vertex_words() {
    let full = build_soa_blob();
    let short = &full[..full.len() - 1];
    let mut bad_range = full.clone();
    let ranges_off = HEADER + N_CLIPS * 4;
    bad_range[ranges_off..ranges_off + 2].copy_from_slice(&u16::MAX.to_le_bytes());
    for blob in [short, &bad_range[..]] {
        let model = Model::from_bytes(leak_at(blob, 0));
        assert_eq!(
            model.vertex_count(),
            0,
            "damaged blob must load as the null model"
        );
        assert!(model.vertex_words().is_empty());
    }
    assert!(Model::EMPTY.vertex_words().is_empty());
}

/// A SoA blob whose vertex stream does not start on a word boundary is
/// rejected at load, so the view never casts a misaligned address.
#[test]
fn unaligned_soa_blob_is_rejected_not_read() {
    let blob = build_soa_blob();
    for misalign in 1..4 {
        let model = Model::from_bytes(leak_at(&blob, misalign));
        assert_eq!(model.vertex_count(), 0, "misaligned by {misalign}");
        assert!(model.vertex_words().is_empty(), "misaligned by {misalign}");
    }
}

/// An interleaved (pre-SoA) cook still loads; it has no word view and reads
/// through the per-index accessors.
#[test]
fn interleaved_blob_has_no_vertex_words() {
    let model = load(build_blob());
    assert_eq!(model.vertex_count(), N_VERTS);
    assert!(model.vertex_words().is_empty());
}

/// The packed render-face stream stores three 10-bit vertex indices
/// (`RENDER_FACE_INDEX_MASK`), so the default vertex guard must not admit a
/// model with more vertices than that can address: its faces would be packed
/// with the high bits masked off and point at the wrong vertices.
#[test]
fn the_default_vertex_guard_matches_what_the_render_faces_can_address() {
    use psx_asset::hmd8::{DEFAULT_MAX_VERTICES, RENDER_FACE_INDEX_MASK};
    let addressable = RENDER_FACE_INDEX_MASK as usize + 1;
    assert_eq!(addressable, 1024);
    assert_eq!(DEFAULT_MAX_VERTICES, addressable);

    assert_eq!(
        load(build_blob_with_vertices(addressable)).vertex_count(),
        addressable
    );
    for too_many in [addressable + 2, 2048, 4096] {
        assert_eq!(
            load(build_blob_with_vertices(too_many)).vertex_count(),
            0,
            "{too_many} vertices"
        );
    }
}

/// The minimal blob with a per-frame time table and one clip record whose
/// frame range is `first` and `count` (the 8-bit count field).
fn build_timed_blob(first: u16, count: u8) -> Vec<u8> {
    let mut blob = build_blob();
    // header flags at byte 30: HMD_FLAG_FRAME_TIMES is bit 5
    let flags = u16::from_le_bytes([blob[30], blob[31]]) | (1 << 5);
    blob[30..32].copy_from_slice(&flags.to_le_bytes());
    // the clip record follows the header; its count is the low byte of the second halfword
    blob[HEADER..HEADER + 2].copy_from_slice(&first.to_le_bytes());
    blob[HEADER + 2] = count;
    // one time byte per frame sits between the clip table and the ranges
    let times_at = HEADER + N_CLIPS * 4;
    for (i, time) in [0u8, 255].into_iter().enumerate() {
        blob.insert(times_at + i, time);
    }
    blob
}

#[test]
fn a_clip_longer_than_the_frame_table_cannot_read_past_the_time_table() {
    // The clip claims 255 frames starting at frame 1 of 2, so the lookup
    // `frame_times[first + local]` ran up to 254 bytes past the 2-entry table
    // and, in this small blob, off the end of it.
    let model = load(build_timed_blob(1, 255));
    assert_eq!(model.frame_count(), N_FRAMES);
    for elapsed in 0..=100 {
        for (a, b, frac) in [
            model.looped_clip_phase(0, 100, elapsed),
            model.one_shot_clip_phase(0, 100, elapsed),
        ] {
            assert!(a < N_FRAMES && b < N_FRAMES, "{a} {b}");
            assert!(frac < 16);
        }
    }
}

#[test]
fn a_well_formed_timed_clip_still_interpolates() {
    let model = load(build_timed_blob(0, 2));
    // Frame times 0 and 255 over a 100-unit clip: halfway is about frame 0
    // blending toward frame 1, and the end of a one-shot is frame 1.
    let (a, b, _) = model.one_shot_clip_phase(0, 100, 50);
    assert_eq!((a, b), (0, 1));
    let (a, _, _) = model.one_shot_clip_phase(0, 100, 100);
    assert_eq!(a, 1);
}
