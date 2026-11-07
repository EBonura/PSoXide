//! The PSVG writer. Host-side: the cooker and tests use it, the guest does not.

use alloc::string::String;
use alloc::vec::Vec;

use crate::format::{
    self, GroupRecord, Run, Vertex, HEADER_LEN, MAGIC, MAX_GROUP_TRIANGLES, MAX_GROUP_VERTICES,
    VERSION,
};

/// One group to write.
#[derive(Clone, Debug, Default)]
pub struct GroupSource {
    /// SVG id; at most 255 bytes.
    pub name: String,
    /// Vertices, Q4 pixels.
    pub vertices: Vec<Vertex>,
    /// Triangle primitives as indices into `vertices`.
    pub triangles: Vec<[u16; 3]>,
    /// Quad primitives as indices into `vertices`.
    pub quads: Vec<[u16; 4]>,
    /// Runs in paint order: `(blend code, kind, count)`. The runs of one kind
    /// consume `triangles` or `quads` front to back.
    pub runs: Vec<(u8, u8, u16)>,
    /// Pivot in Q4 pixels; the bounding-box centre when `None`.
    pub pivot: Option<[i16; 2]>,
}

/// Why [`encode`] refused its input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EncodeError {
    /// More than 65535 groups.
    TooManyGroups,
    /// A group has more vertices than `u16` indices address.
    TooManyVertices(String),
    /// A group has more triangles than its `u16` count holds.
    TooManyTriangles(String),
    /// A name is longer than 255 bytes, or the names section passed 64 KiB.
    NameTooLong(String),
    /// An index is not below the vertex count, or the runs do not cover the
    /// primitives.
    Inconsistent(String),
}

fn pad4(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn bounds_of(vertices: &[Vertex]) -> [i16; 4] {
    let mut b = [i16::MAX, i16::MAX, i16::MIN, i16::MIN];
    for v in vertices {
        b = [b[0].min(v.x), b[1].min(v.y), b[2].max(v.x), b[3].max(v.y)];
    }
    if vertices.is_empty() {
        [0; 4]
    } else {
        b
    }
}

/// Serialise `groups` into a PSVG image. The output is a pure function of the
/// input, byte for byte.
pub fn encode(canvas_q4: [u16; 2], groups: &[GroupSource]) -> Result<Vec<u8>, EncodeError> {
    if groups.len() > u16::MAX as usize {
        return Err(EncodeError::TooManyGroups);
    }
    let mut records = Vec::with_capacity(groups.len());
    let mut vertices: Vec<Vertex> = Vec::new();
    let mut indices: Vec<u16> = Vec::new();
    let mut runs: Vec<Run> = Vec::new();
    let mut names: Vec<u8> = Vec::new();
    for group in groups {
        if group.vertices.len() > MAX_GROUP_VERTICES {
            return Err(EncodeError::TooManyVertices(group.name.clone()));
        }
        if group.triangles.len() > MAX_GROUP_TRIANGLES || group.quads.len() > MAX_GROUP_TRIANGLES {
            return Err(EncodeError::TooManyTriangles(group.name.clone()));
        }
        if group.name.len() > u8::MAX as usize || names.len() > u16::MAX as usize {
            return Err(EncodeError::NameTooLong(group.name.clone()));
        }
        let covered = |kind: u8| -> usize {
            group
                .runs
                .iter()
                .filter(|r| r.1 == kind)
                .map(|r| r.2 as usize)
                .sum()
        };
        let n = group.vertices.len();
        let in_range = group.triangles.iter().flatten().all(|&i| (i as usize) < n)
            && group.quads.iter().flatten().all(|&i| (i as usize) < n);
        if covered(format::RUN_TRIANGLES) != group.triangles.len()
            || covered(format::RUN_QUADS) != group.quads.len()
            || group.runs.iter().any(|r| r.1 > format::RUN_QUADS)
            || !in_range
        {
            return Err(EncodeError::Inconsistent(group.name.clone()));
        }
        let bounds = bounds_of(&group.vertices);
        let pivot = group.pivot.unwrap_or([
            ((bounds[0] as i32 + bounds[2] as i32) / 2) as i16,
            ((bounds[1] as i32 + bounds[3] as i32) / 2) as i16,
        ]);
        records.push(GroupRecord {
            name_off: names.len() as u16,
            name_len: group.name.len() as u8,
            flags: 0,
            vertex_start: vertices.len() as u32,
            vertex_count: group.vertices.len() as u16,
            tri_count: group.triangles.len() as u16,
            index_start: indices.len() as u32,
            run_start: runs.len() as u32,
            run_count: group.runs.len() as u16,
            quad_count: group.quads.len() as u16,
            bounds,
            pivot,
            reserved2: 0,
        });
        names.extend_from_slice(group.name.as_bytes());
        vertices.extend_from_slice(&group.vertices);
        // Indices follow paint order: each run takes its primitives from the
        // front of the matching list.
        let (mut next_tri, mut next_quad) = (0usize, 0usize);
        for &(_, kind, count) in &group.runs {
            let count = count as usize;
            if kind == format::RUN_TRIANGLES {
                indices.extend(
                    group.triangles[next_tri..next_tri + count]
                        .iter()
                        .flatten()
                        .copied(),
                );
                next_tri += count;
            } else {
                indices.extend(
                    group.quads[next_quad..next_quad + count]
                        .iter()
                        .flatten()
                        .copied(),
                );
                next_quad += count;
            }
        }
        runs.extend(
            group
                .runs
                .iter()
                .map(|&(blend, kind, count)| Run { blend, kind, count }),
        );
    }

    let mut out = Vec::new();
    out.extend_from_slice(&[0u8; HEADER_LEN]);
    let off_groups = out.len();
    for g in &records {
        put_u16(&mut out, g.name_off);
        out.push(g.name_len);
        out.push(g.flags);
        put_u32(&mut out, g.vertex_start);
        put_u16(&mut out, g.vertex_count);
        put_u16(&mut out, g.tri_count);
        put_u32(&mut out, g.index_start);
        put_u32(&mut out, g.run_start);
        put_u16(&mut out, g.run_count);
        put_u16(&mut out, g.quad_count);
        for b in g.bounds {
            out.extend_from_slice(&b.to_le_bytes());
        }
        for p in g.pivot {
            out.extend_from_slice(&p.to_le_bytes());
        }
        put_u32(&mut out, g.reserved2);
    }
    pad4(&mut out);
    let off_vertices = out.len();
    for v in &vertices {
        out.extend_from_slice(&v.x.to_le_bytes());
        out.extend_from_slice(&v.y.to_le_bytes());
        put_u32(&mut out, v.color);
    }
    let off_indices = out.len();
    for &i in &indices {
        put_u16(&mut out, i);
    }
    pad4(&mut out);
    let off_runs = out.len();
    for r in &runs {
        out.push(r.blend);
        out.push(r.kind);
        put_u16(&mut out, r.count);
    }
    let off_names = out.len();
    out.extend_from_slice(&names);
    pad4(&mut out);

    let total = out.len();
    out[0..4].copy_from_slice(&MAGIC);
    out[4..6].copy_from_slice(&VERSION.to_le_bytes());
    out[6..8].copy_from_slice(&(groups.len() as u16).to_le_bytes());
    out[8..10].copy_from_slice(&canvas_q4[0].to_le_bytes());
    out[10..12].copy_from_slice(&canvas_q4[1].to_le_bytes());
    let words = [
        vertices.len(),
        indices.len(),
        runs.len(),
        names.len(),
        off_groups,
        off_vertices,
        off_indices,
        off_runs,
        off_names,
        total,
        0,
    ];
    for (i, w) in words.iter().enumerate() {
        out[12 + 4 * i..16 + 4 * i].copy_from_slice(&(*w as u32).to_le_bytes());
    }
    debug_assert_eq!(format::HEADER_LEN, 56);
    Ok(out)
}
