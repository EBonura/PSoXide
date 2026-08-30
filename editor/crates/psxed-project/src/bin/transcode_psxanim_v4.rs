//! Rewrite v3 `.psxanim` clips as v4 in place.
//!
//! ```sh
//! cargo run -p psxed-project --bin transcode_psxanim_v4 -- <project dir> [--apply]
//! ```
//!
//! The cooker already prefers v4 and only falls back to v3 when
//! [`encode_rotation_q11_cross`] cannot reconstruct a record's third basis
//! vector. Clips copied between projects predate that path and keep a v3 body
//! forever, because nothing re-cooks an asset that already exists. This walks a
//! project's clips and re-encodes the ones that would have been v4 had they
//! been cooked today, saving four bytes per joint per frame.
//!
//! A clip is only rewritten when every one of its records encodes, so a file is
//! never left half-converted. Without `--apply` this reports and writes
//! nothing.

use std::path::{Path, PathBuf};

use psxed_format::animation::{
    decode_rotation_q11, decode_rotation_q11_cross, encode_rotation_q11_cross, AnimationHeader,
    MAGIC, POSE_RECORD_SIZE_V3, POSE_RECORD_SIZE_V4, POSE_ROTATION_BLOCK_SIZE_V3,
    POSE_ROTATION_BLOCK_SIZE_V4, VERSION_V3, VERSION_V4,
};

const ASSET_HEADER_SIZE: usize = 12;

fn read_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

/// Largest per-component rotation error observed while re-encoding, in Q12.
/// The v4 contract allows three; anything above that means the conversion
/// changed the pose and the clip must keep its v3 body.
const MAX_ACCEPTED_ERROR_Q12: i32 = 3;

/// Re-encode one clip, or explain why it stays as it is.
///
/// Returns the rewritten bytes and the worst rotation error the conversion
/// introduced, so the caller can refuse a clip that would visibly move.
fn transcode(bytes: &[u8]) -> Result<(Vec<u8>, i32), String> {
    if bytes.len() < ASSET_HEADER_SIZE + AnimationHeader::SIZE || bytes[..4] != MAGIC {
        return Err("not a .psxanim".to_string());
    }
    if read_u16(bytes, 4) != VERSION_V3 {
        return Err("not v3".to_string());
    }
    let joints = read_u16(bytes, ASSET_HEADER_SIZE) as usize;
    let frames = read_u16(bytes, ASSET_HEADER_SIZE + 2) as usize;
    let records = joints.saturating_mul(frames);
    let first = ASSET_HEADER_SIZE + AnimationHeader::SIZE;
    if bytes.len() < first + records * POSE_RECORD_SIZE_V3 {
        return Err("truncated pose table".to_string());
    }

    let mut worst_error = 0i32;
    let mut poses = Vec::with_capacity(records * POSE_RECORD_SIZE_V4);
    for index in 0..records {
        let at = first + index * POSE_RECORD_SIZE_V3;
        let mut block = [0u8; POSE_ROTATION_BLOCK_SIZE_V3];
        block.copy_from_slice(&bytes[at..at + POSE_ROTATION_BLOCK_SIZE_V3]);
        let matrix = decode_rotation_q11(&block);

        let mut dense = [0u8; POSE_ROTATION_BLOCK_SIZE_V4];
        if !encode_rotation_q11_cross(&matrix, &mut dense) {
            return Err(format!("record {index} does not fit v4"));
        }
        // Decode what the runtime will actually read and compare it against the
        // v3 pose. Encoding reporting success is not the same as the pose
        // surviving, and these are authored assets being overwritten in place.
        let round_tripped = decode_rotation_q11_cross(&dense);
        for element in 0..9 {
            let error = (round_tripped[element] as i32 - matrix[element] as i32).abs();
            if error > worst_error {
                worst_error = error;
            }
        }
        if worst_error > MAX_ACCEPTED_ERROR_Q12 {
            return Err(format!(
                "record {index} round-trips {worst_error} Q12 units off, over the {MAX_ACCEPTED_ERROR_Q12} allowed"
            ));
        }
        poses.extend_from_slice(&dense);
        // Translation is the record tail in both revisions, three shifted i16s.
        let translation = at + POSE_ROTATION_BLOCK_SIZE_V3;
        poses.extend_from_slice(&bytes[translation..translation + 6]);
    }

    let payload_len = AnimationHeader::SIZE + poses.len();
    let mut out = Vec::with_capacity(ASSET_HEADER_SIZE + payload_len);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION_V4.to_le_bytes());
    out.extend_from_slice(&read_u16(bytes, 6).to_le_bytes());
    out.extend_from_slice(&(payload_len as u32).to_le_bytes());
    // The animation header is revision independent: joints, frames, rate and
    // the translation shift all carry over untouched.
    out.extend_from_slice(&bytes[ASSET_HEADER_SIZE..first]);
    out.extend_from_slice(&poses);
    Ok((out, worst_error))
}

fn collect(dir: &Path, into: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, into);
        } else if path.extension().is_some_and(|ext| ext == "psxanim") {
            into.push(path);
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().expect("project directory"));
    let apply = args.any(|arg| arg == "--apply");

    let mut clips = Vec::new();
    collect(&root, &mut clips);
    clips.sort();

    let (mut converted, mut saved, mut skipped) = (0usize, 0i64, Vec::new());
    let mut worst_overall = 0i32;
    for clip in &clips {
        let Ok(bytes) = std::fs::read(clip) else {
            continue;
        };
        match transcode(&bytes) {
            Ok((out, worst)) => {
                worst_overall = worst_overall.max(worst);
                let delta = bytes.len() as i64 - out.len() as i64;
                converted += 1;
                saved += delta;
                println!(
                    "  {:>7} -> {:>7}  (-{:>6})  {}",
                    bytes.len(),
                    out.len(),
                    delta,
                    clip.strip_prefix(&root).unwrap_or(clip).display()
                );
                if apply {
                    std::fs::write(clip, &out).expect("write clip");
                }
            }
            Err(reason) if reason == "not v3" || reason == "not a .psxanim" => {}
            Err(reason) => skipped.push((clip.clone(), reason)),
        }
    }
    println!(
        "\n{} clip(s) scanned, {converted} converted, {saved} bytes saved ({:.1} KiB), worst rotation error {worst_overall} of {MAX_ACCEPTED_ERROR_Q12} Q12 units{}",
        clips.len(),
        saved as f64 / 1024.0,
        if apply { "" } else { "  [dry run, pass --apply]" }
    );
    for (clip, reason) in &skipped {
        println!("  kept as v3: {} ({reason})", clip.display());
    }
}
