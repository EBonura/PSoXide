//! Leaf numeric helpers and little-endian blob writers shared across the importer.

use super::*;

pub(crate) fn append_asset_header(
    out: &mut Vec<u8>,
    magic: [u8; 4],
    version: u16,
    flags: u16,
    payload_len: usize,
) -> Result<(), Error> {
    if payload_len > u32::MAX as usize {
        return Err(Error::TooMany {
            kind: "payload bytes",
            count: payload_len,
            max: u32::MAX as usize,
        });
    }
    out.extend_from_slice(&magic);
    append_u16(out, version);
    append_u16(out, flags);
    append_u32(out, payload_len as u32);
    Ok(())
}

pub(crate) fn append_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

pub(crate) fn append_i16(out: &mut Vec<u8>, value: i16) {
    out.extend_from_slice(&value.to_le_bytes());
}

pub(crate) fn append_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

pub(crate) fn write_i16(out: &mut [u8], offset: usize, value: i16) {
    out[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

pub(crate) fn ensure_u16(kind: &'static str, count: usize) -> Result<u16, Error> {
    if count > u16::MAX as usize {
        Err(Error::TooMany {
            kind,
            count,
            max: u16::MAX as usize,
        })
    } else {
        Ok(count as u16)
    }
}

pub(crate) fn q12_i16(value: f32) -> i16 {
    (value * 4096.0)
        .round()
        .clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

pub(crate) fn q12_i32(value: f32) -> i32 {
    (value * 4096.0)
        .round()
        .clamp(i32::MIN as f32, i32::MAX as f32) as i32
}

pub(crate) fn uv_to_u8(value: f32, size: u16) -> u8 {
    let max_coord = size.saturating_sub(1).min(255) as f32;
    (value.clamp(0.0, 1.0) * max_coord)
        .round()
        .clamp(0.0, 255.0) as u8
}

pub(crate) fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

pub(crate) fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub(crate) fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub(crate) fn length_sq3(v: [f32; 3]) -> f32 {
    v[0] * v[0] + v[1] * v[1] + v[2] * v[2]
}

pub(crate) fn vec3_close(a: [f32; 3], b: [f32; 3], epsilon: f32) -> bool {
    (a[0] - b[0]).abs() <= epsilon
        && (a[1] - b[1]).abs() <= epsilon
        && (a[2] - b[2]).abs() <= epsilon
}

pub(crate) fn quat_close_same_orientation(a: [f32; 4], b: [f32; 4], min_abs_dot: f32) -> bool {
    let a = normalize4(a);
    let b = normalize4(b);
    let dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    dot.abs() >= min_abs_dot
}

pub(crate) fn nlerp_quat(mut a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    if dot < 0.0 {
        a = [-a[0], -a[1], -a[2], -a[3]];
    }
    normalize4([
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
        a[3] + (b[3] - a[3]) * t,
    ])
}

pub(crate) fn quat_mul(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let [ax, ay, az, aw] = normalize4(a);
    let [bx, by, bz, bw] = normalize4(b);
    normalize4([
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    ])
}

pub(crate) fn quat_inverse(q: [f32; 4]) -> [f32; 4] {
    let [x, y, z, w] = normalize4(q);
    [-x, -y, -z, w]
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const fn identity_quat() -> [f32; 4] {
    [0.0, 0.0, 0.0, 1.0]
}

pub(crate) fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len <= 0.000001 {
        [0.0, 1.0, 0.0]
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}

pub(crate) fn normalize4(v: [f32; 4]) -> [f32; 4] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2] + v[3] * v[3]).sqrt();
    if len <= 0.000001 {
        [0.0, 0.0, 0.0, 1.0]
    } else {
        [v[0] / len, v[1] / len, v[2] / len, v[3] / len]
    }
}
