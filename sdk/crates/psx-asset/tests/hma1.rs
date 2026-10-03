//! HMA1 validation check.
//!
//! The decoder reads keys without bounds checks, so `Model::new` must prove
//! every read in bounds for every playback position before it hands out a
//! model. These tests build a small blob by hand, decode it, and confirm that
//! truncation and damaged tables are rejected rather than trusted.

use psx_asset::hma1::{Aff, Model};

const N_BONES: usize = 2;
/// Segments of the keyed rate-0 track: keys at five source frames.
const SEGMENTS: u16 = 4;

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Byte offsets of the fields the damage tests poke.
struct Layout {
    parents: usize,
    clip_table: usize,
    segments: usize,
    rot_key_bytes: usize,
    /// One past the last byte any position can read.
    last_read: usize,
}

/// Two bones, one clip. Bone 0 is a root with a keyed 8-bit rotation track
/// on rate 0 and the bind translation; bone 1 is its child with a single
/// 8-bit rotation key and a per-clip translation.
fn build_blob() -> (Vec<u8>, Layout) {
    let mut out = Vec::new();
    put_u16(&mut out, N_BONES as u16);
    put_u16(&mut out, 1); // clips
    let parents = out.len();
    out.extend_from_slice(&[0xff, 0]);
    // bind translations, Q2
    for v in [0i16, 0, 0, 4, 8, 12] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    while out.len() % 4 != 0 {
        out.push(0);
    }
    let clip_table = out.len();
    let clip = clip_table + 4;
    out.extend_from_slice(&(clip as u32).to_le_bytes());

    // clip header: n_int, flags, qfmt, seg[7], factor_q15[7]
    put_u16(&mut out, SEGMENTS);
    out.extend_from_slice(&[0, 3]);
    let segments = out.len();
    put_u16(&mut out, SEGMENTS);
    for _ in 1..7 {
        put_u16(&mut out, 0);
    }
    for _ in 0..7 {
        put_u16(&mut out, 0x7fff); // ~1.0: one key per source frame
    }
    // modes: rotation rate in bits 0..2 (7 = one key), translation code in
    // bits 3..5 (7 = bind, 6 = one per-clip value), bit 6 = 12-bit keys
    out.extend_from_slice(&[7 << 3, 7 | (6 << 3)]);
    while out.len() % 2 != 0 {
        out.push(0);
    }

    // bone 0: i8 base, key bytes, pad, then SEGMENTS + 1 two-byte keys
    out.extend_from_slice(&[0, 0, 0, 127]);
    let rot_key_bytes = out.len();
    out.extend_from_slice(&[2, 0]);
    for k in 0..=SEGMENTS {
        out.extend_from_slice(&[k as u8 * 0x11, 0x00]);
    }
    let keyed_end = out.len();
    // bone 1: one 8-bit key (identity), then a per-clip translation
    out.extend_from_slice(&[0, 0, 0, 127]);
    for v in [16i16, 0, -16] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    let last_read = out.len();
    // The last keyed read of bone 0 is a word load at its last key.
    assert!(keyed_end + 2 <= last_read);
    // the cooker pads four readable bytes past the last key
    out.extend_from_slice(&[0; 4]);
    (
        out,
        Layout {
            parents,
            clip_table,
            segments,
            rot_key_bytes,
            last_read,
        },
    )
}

fn new(bytes: Vec<u8>) -> Option<Model> {
    Model::new(Box::leak(bytes.into_boxed_slice()))
}

#[test]
fn decodes_a_minimal_blob_at_any_position() {
    let model = new(build_blob().0).expect("well-formed blob");
    assert_eq!(model.n_bones(), N_BONES);
    assert_eq!(model.n_clips(), 1);
    assert_eq!(model.clip_intervals(0), SEGMENTS as u32);

    let mut out = [Aff::ZERO; N_BONES];
    // Past the end and past the last clip both clamp instead of reading on.
    for (clip, pos) in [(0, 0), (0, 300), (0, 4 * 256), (0, 100_000), (9, 50)] {
        model.decode(clip, pos, &mut out);
        // The child composes onto the root: its translation is the parent's
        // plus its own rotated offset, never garbage from past the blob.
        assert!(out[1].t.iter().all(|t| t.abs() < 64), "{:?}", out[1].t);
    }

    // A scratch buffer too small for the bones is left alone.
    let mut short = [Aff::ZERO; 1];
    model.decode(0, 0, &mut short);
    assert_eq!(short[0].t, [0; 3]);
}

#[test]
fn rejects_blobs_that_would_read_past_their_end() {
    let (blob, at) = build_blob();
    // Dropping the cooker's padding is fine as long as every read still fits.
    assert!(new(blob[..at.last_read].to_vec()).is_some());
    for cut in [
        at.last_read - 1,
        at.rot_key_bytes + 4,
        at.clip_table + 2,
        3,
        0,
    ] {
        assert!(new(blob[..cut].to_vec()).is_none(), "cut at {cut}");
    }

    // More segments than the track stores keys for.
    let mut long_track = blob.clone();
    long_track[at.segments..at.segments + 2].copy_from_slice(&200u16.to_le_bytes());
    assert!(new(long_track).is_none());

    // Wider keys than the 8-bit format packs.
    let mut wide_keys = blob.clone();
    wide_keys[at.rot_key_bytes] = 9;
    assert!(new(wide_keys).is_none());

    // A clip offset pointing outside the blob.
    let mut bad_clip = blob.clone();
    bad_clip[at.clip_table..at.clip_table + 4].copy_from_slice(&0xffff_fff0u32.to_le_bytes());
    assert!(new(bad_clip).is_none());

    // A child listed before its parent: the decoder composes parents first.
    let mut bad_parent = blob.clone();
    bad_parent[at.parents] = 1;
    assert!(new(bad_parent).is_none());

    // No clips at all.
    let mut no_clips = blob;
    no_clips[2..4].copy_from_slice(&0u16.to_le_bytes());
    assert!(new(no_clips).is_none());
}

/// Validation itself must never read out of bounds or panic, whatever the
/// bytes: flip every byte of a valid blob through a spread of values.
#[test]
fn validation_survives_arbitrary_damage() {
    let (blob, _) = build_blob();
    for i in 0..blob.len() {
        for v in [0u8, 1, 0x7f, 0x80, 0xfe, 0xff] {
            let mut damaged = blob.clone();
            damaged[i] = v;
            let _ = new(damaged);
        }
    }
}
