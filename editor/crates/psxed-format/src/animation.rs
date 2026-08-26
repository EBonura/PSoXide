//! On-disk layout for cooked rigid-skeletal animations (`.psxanim`).
//!
//! The animation format stores already-sampled fixed-point joint pose
//! matrices. Runtime code can index or cheaply interpolate between
//! sampled frames, fetch the joint record for each model part, and
//! submit transformed triangles without evaluating glTF channels or
//! quaternions on the PS1.
//!
//! # File layout
//!
//! ```text
//!   AssetHeader (12 bytes)
//!     magic       = b"PSXA"
//!     version     = VERSION_V1, VERSION, VERSION_V3, or VERSION_V4
//!     flags       = reserved
//!     payload_len = everything after this header
//!
//!   AnimationHeader (8 bytes)
//!     joint_count         u16
//!     frame_count         u16
//!     sample_rate_hz      u16
//!     translation_shift   u16   // v2+
//!
//!   Pose table: frame_count × joint_count × record_size
//!     v2: 24 bytes, i16[9] Q3.12 matrix + shifted i16[3] translation
//!     v3: 20 bytes, nine packed Q11 matrix elements + translation
//!     v4: 16 bytes, six packed Q11 elements + correction + translation
//! ```
//!
//! The pose matrix maps model-space vertices into the sampled animated
//! pose. Translation uses the same model-local unit scale as the
//! matching `.psxmdl` vertices, so local precision can be much denser
//! than world/grid precision. Version 4 reconstructs the third rigid-transform
//! basis vector from the first two and accepts the record only when its extra
//! matrix error is at most three Q12 units. Version 3 is the rigid fallback,
//! version 2 handles animated scale, and version 1 remains readable.

/// ASCII magic identifying the `.psxanim` animation format.
pub const MAGIC: [u8; 4] = *b"PSXA";

/// Legacy animation format revision.
pub const VERSION_V1: u16 = 1;

/// Flat compact animation revision, retained for animated scale.
pub const VERSION: u16 = 2;

/// Compact revision: rotation matrices packed as 12-bit Q11 codes
/// (nine elements in fourteen bytes), the encoding hl-psx ships on
/// silicon. Translation stays three shifted `i16`s, so a record is
/// twenty bytes instead of twenty-four.
pub const VERSION_V3: u16 = 3;

/// Dense rigid-pose revision. Two orthonormal basis vectors are stored as
/// six Q11 codes; the third is reconstructed with a fixed-point cross product
/// plus one compact correction. Shifted translations remain three `i16`s.
pub const VERSION_V4: u16 = 4;

/// Byte layout of the animation payload header.
#[repr(C, packed)]
#[derive(Copy, Clone, Debug)]
pub struct AnimationHeader {
    /// Number of joint pose records per frame.
    pub joint_count: u16,
    /// Number of sampled frames.
    pub frame_count: u16,
    /// Integer sample rate in Hz.
    pub sample_rate_hz: u16,
    /// Shared right shift applied to every stored translation in v2+.
    ///
    /// Runtime reconstructs model-local units with `stored << shift`.
    /// Version 1 files leave this as zero and store `i32` translations.
    pub _reserved: u16,
}

impl AnimationHeader {
    /// Size of the animation header in bytes (always 8).
    pub const SIZE: usize = 8;

    /// Build an animation header.
    pub const fn new(joint_count: u16, frame_count: u16, sample_rate_hz: u16) -> Self {
        Self::new_with_translation_shift(joint_count, frame_count, sample_rate_hz, 0)
    }

    /// Build an animation header with an explicit translation shift.
    pub const fn new_with_translation_shift(
        joint_count: u16,
        frame_count: u16,
        sample_rate_hz: u16,
        translation_shift: u16,
    ) -> Self {
        Self {
            joint_count,
            frame_count,
            sample_rate_hz,
            _reserved: translation_shift,
        }
    }

    /// Shared right shift for compact v2 translations.
    pub const fn translation_shift(&self) -> u16 {
        self._reserved
    }
}

/// Size of one compact v3 joint pose record in bytes.
pub const POSE_RECORD_SIZE_V3: usize = 20;

/// Size of one dense v4 joint pose record in bytes.
pub const POSE_RECORD_SIZE_V4: usize = 16;

/// Bytes of one v3 packed rotation block (nine 12-bit Q11 codes:
/// four 3-byte pairs, then one code in a trailing u16's low 12 bits).
pub const POSE_ROTATION_BLOCK_SIZE_V3: usize = 14;

/// Six Q11 codes in nine bytes plus one correction byte.
pub const POSE_ROTATION_BLOCK_SIZE_V4: usize = 10;

/// Maximum extra Q12 matrix-element error accepted by the v4 encoder over the
/// v3 Q11 representation. Three units are under one tenth of a degree and keep
/// the new quantisation in the same error class as Q11 itself.
pub const V4_MAX_RECONSTRUCTION_ERROR_Q12: i16 = 3;

/// Encode one Q3.12 rotation element as a 12-bit Q11 code.
///
/// Decode is `code * 2`, except the reserved all-ones positive code
/// `0x7FF` which decodes to exactly 4096 (Q12 one). Regular codes top
/// out at 2046 (= 4092), so values in 4093..=4096 collapse to the
/// reserved code; everything else rounds to the nearest even Q12.
pub const fn encode_q11(q12: i16) -> u16 {
    if q12 >= 4095 {
        return 0x7FF;
    }
    let clamped = if q12 < -4096 { -4096 } else { q12 } as i32;
    // Round-to-nearest halving, then clamp inside the regular range.
    let mut code = (clamped + if clamped >= 0 { 1 } else { -1 }) / 2;
    if code > 2046 {
        code = 2046;
    }
    if code < -2048 {
        code = -2048;
    }
    (code as u16) & 0x0FFF
}

/// Decode one 12-bit Q11 code back to a Q3.12 element (reference
/// implementation; the runtime carries a branchless MIPS equivalent).
pub const fn decode_q11(code: u16) -> i16 {
    let signed = ((code << 4) as i16) >> 4;
    signed
        .wrapping_shl(1)
        .wrapping_add((((code & 0x0FFF) == 0x07FF) as i16) << 1)
}

/// Pack nine Q3.12 rotation elements into a v3 rotation block.
pub fn encode_rotation_q11(matrix: &[i16; 9], out: &mut [u8; POSE_ROTATION_BLOCK_SIZE_V3]) {
    let mut pair = 0;
    while pair < 4 {
        let a = encode_q11(matrix[pair * 2]) as u32;
        let b = encode_q11(matrix[pair * 2 + 1]) as u32;
        let packed = a | (b << 12);
        out[pair * 3] = (packed & 0xFF) as u8;
        out[pair * 3 + 1] = ((packed >> 8) & 0xFF) as u8;
        out[pair * 3 + 2] = ((packed >> 16) & 0xFF) as u8;
        pair += 1;
    }
    let last = encode_q11(matrix[8]);
    out[12] = (last & 0xFF) as u8;
    out[13] = ((last >> 8) & 0x0F) as u8;
}

/// Unpack a v3 rotation block back to nine Q3.12 elements (reference).
pub fn decode_rotation_q11(block: &[u8; POSE_ROTATION_BLOCK_SIZE_V3]) -> [i16; 9] {
    let mut out = [0i16; 9];
    let mut pair = 0;
    while pair < 4 {
        let packed = (block[pair * 3] as u32)
            | ((block[pair * 3 + 1] as u32) << 8)
            | ((block[pair * 3 + 2] as u32) << 16);
        out[pair * 2] = decode_q11((packed & 0x0FFF) as u16);
        out[pair * 2 + 1] = decode_q11(((packed >> 12) & 0x0FFF) as u16);
        pair += 1;
    }
    out[8] = decode_q11(((block[12] as u16) | ((block[13] as u16) << 8)) & 0x0FFF);
    out
}

#[inline]
fn round_q24_to_q12(value: i32) -> i16 {
    let rounded = if value >= 0 {
        value.saturating_add(1 << 11) >> 12
    } else {
        -value
            .saturating_neg()
            .saturating_add(1 << 11)
            .wrapping_shr(12)
    };
    rounded.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

#[inline]
fn cross_q12(a: [i16; 3], b: [i16; 3]) -> [i16; 3] {
    [
        round_q24_to_q12(a[1] as i32 * b[2] as i32 - a[2] as i32 * b[1] as i32),
        round_q24_to_q12(a[2] as i32 * b[0] as i32 - a[0] as i32 * b[2] as i32),
        round_q24_to_q12(a[0] as i32 * b[1] as i32 - a[1] as i32 * b[0] as i32),
    ]
}

/// Encode a v4 rotation block. Returns false when the third basis vector
/// cannot be reconstructed within [`V4_MAX_RECONSTRUCTION_ERROR_Q12`]; callers
/// should retain the v3 record for that clip in that case.
pub fn encode_rotation_q11_cross(
    matrix: &[i16; 9],
    out: &mut [u8; POSE_ROTATION_BLOCK_SIZE_V4],
) -> bool {
    let mut quantized = [0i16; 9];
    let mut index = 0usize;
    while index < 9 {
        quantized[index] = decode_q11(encode_q11(matrix[index]));
        index += 1;
    }

    let mut pair = 0usize;
    while pair < 3 {
        let a = encode_q11(matrix[pair * 2]) as u32;
        let b = encode_q11(matrix[pair * 2 + 1]) as u32;
        let packed = a | (b << 12);
        out[pair * 3] = (packed & 0xff) as u8;
        out[pair * 3 + 1] = ((packed >> 8) & 0xff) as u8;
        out[pair * 3 + 2] = ((packed >> 16) & 0xff) as u8;
        pair += 1;
    }

    let reconstructed = cross_q12(
        [quantized[0], quantized[1], quantized[2]],
        [quantized[3], quantized[4], quantized[5]],
    );
    let residual = [
        quantized[6] as i32 - reconstructed[0] as i32,
        quantized[7] as i32 - reconstructed[1] as i32,
        quantized[8] as i32 - reconstructed[2] as i32,
    ];
    let axis = (0..3)
        .max_by_key(|&axis| residual[axis].unsigned_abs())
        .unwrap_or(0);
    let correction = residual[axis];
    if !(-32..=31).contains(&correction) {
        return false;
    }
    let correction_code = (correction as i8 as u8) & 0x3f;
    out[9] = (correction_code << 2) | axis as u8;

    let mut decoded = reconstructed;
    decoded[axis] = decoded[axis]
        .saturating_add(correction as i16)
        .clamp(-4096, 4096);
    for component in &mut decoded {
        *component = (*component).clamp(-4096, 4096);
    }
    (0..3).all(|component| {
        (decoded[component] as i32 - quantized[component + 6] as i32).abs()
            <= V4_MAX_RECONSTRUCTION_ERROR_Q12 as i32
    })
}

/// Decode a v4 rotation block into the same matrix ordering used by v3.
pub fn decode_rotation_q11_cross(block: &[u8; POSE_ROTATION_BLOCK_SIZE_V4]) -> [i16; 9] {
    let mut flat = [0i16; 9];
    let mut pair = 0usize;
    while pair < 3 {
        let packed = (block[pair * 3] as u32)
            | ((block[pair * 3 + 1] as u32) << 8)
            | ((block[pair * 3 + 2] as u32) << 16);
        flat[pair * 2] = decode_q11((packed & 0x0fff) as u16);
        flat[pair * 2 + 1] = decode_q11(((packed >> 12) & 0x0fff) as u16);
        pair += 1;
    }
    let third = cross_q12([flat[0], flat[1], flat[2]], [flat[3], flat[4], flat[5]]);
    flat[6..9].copy_from_slice(&third);
    let axis = usize::from(block[9] & 0x03);
    if axis < 3 {
        let correction = (((block[9] >> 2) << 2) as i8 >> 2) as i16;
        flat[6 + axis] = flat[6 + axis].saturating_add(correction);
    }
    for component in &mut flat[6..9] {
        *component = (*component).clamp(-4096, 4096);
    }
    flat
}

/// Size of one legacy v1 joint pose record in bytes.
pub const POSE_RECORD_SIZE_V1: usize = 30;

/// Size of one v2 flat compact joint pose record in bytes.
pub const POSE_RECORD_SIZE: usize = 24;

#[cfg(test)]
mod q11_tests {
    use super::*;

    /// Every Q3.12 element round-trips within the Q11 step (2), and the
    /// two exact identities the runtime depends on hold: zero is zero
    /// and Q12 one (4096) survives via the reserved code.
    #[test]
    fn q11_round_trip_stays_within_one_step() {
        assert_eq!(decode_q11(encode_q11(0)), 0);
        assert_eq!(decode_q11(encode_q11(4096)), 4096);
        assert_eq!(decode_q11(encode_q11(-4096)), -4096);
        let mut worst = 0i32;
        let mut value = -4096i16;
        while value <= 4096 {
            let decoded = decode_q11(encode_q11(value)) as i32;
            worst = worst.max((decoded - value as i32).abs());
            value += 1;
        }
        assert!(worst <= 2, "worst q11 round-trip error {worst}");
    }

    /// Packing nine elements and unpacking them is the identity over
    /// the code space, including the reserved encoding.
    #[test]
    fn rotation_block_round_trips() {
        let matrix: [i16; 9] = [4096, -4096, 0, 2048, -2048, 1234, -1235, 4094, 3];
        let mut block = [0u8; POSE_ROTATION_BLOCK_SIZE_V3];
        encode_rotation_q11(&matrix, &mut block);
        let decoded = decode_rotation_q11(&block);
        for (i, (&want, &got)) in matrix.iter().zip(decoded.iter()).enumerate() {
            let err = (got as i32 - want as i32).abs();
            assert!(err <= 2, "element {i}: {want} -> {got}");
        }
    }

    #[test]
    fn dense_rotation_stays_within_its_q12_error_gate() {
        let matrix: [i16; 9] = [3850, 1212, 704, -1294, 3862, 432, -536, -628, 4012];
        let mut dense = [0u8; POSE_ROTATION_BLOCK_SIZE_V4];
        assert!(encode_rotation_q11_cross(&matrix, &mut dense));
        let decoded = decode_rotation_q11_cross(&dense);
        for (want, got) in matrix
            .map(|value| decode_q11(encode_q11(value)))
            .into_iter()
            .zip(decoded)
        {
            assert!(
                (i32::from(want) - i32::from(got)).abs()
                    <= i32::from(V4_MAX_RECONSTRUCTION_ERROR_Q12)
            );
        }
    }

    #[test]
    fn dense_identity_remains_exact() {
        let matrix = [4096, 0, 0, 0, 4096, 0, 0, 0, 4096];
        let mut dense = [0u8; POSE_ROTATION_BLOCK_SIZE_V4];
        assert!(encode_rotation_q11_cross(&matrix, &mut dense));
        assert_eq!(decode_rotation_q11_cross(&dense), matrix);
    }
}
