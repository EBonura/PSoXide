//! A parsed PSVG asset: validated views over the cooked bytes.

use core::mem::{align_of, size_of};

use crate::format::{
    blend_from_code, GroupRecord, Run, Vertex, HEADER_LEN, MAGIC, RUN_QUADS, RUN_TRIANGLES, VERSION,
};

/// Why [`VectorAsset::parse`] rejected a byte slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseError {
    /// Shorter than the header or than the lengths it declares.
    Truncated,
    /// The magic is not `"PSVG"`.
    BadMagic,
    /// A version this reader does not know.
    UnsupportedVersion(u16),
    /// The slice does not start on a 4-byte boundary; use `include_vector!`.
    Misaligned,
    /// A section offset is not a multiple of four or lies outside the asset.
    BadSection,
    /// A group record points outside its section.
    BadGroup,
    /// A triangle index is not smaller than its group's vertex count.
    BadIndex,
    /// A run has an unknown blend code or kind, or the runs do not add up to
    /// the group's triangle and quad counts.
    BadRun,
    /// A group name is not UTF-8 or lies outside the names section.
    BadName,
}

/// Handle to one group of an asset, from [`VectorAsset::group`] or
/// [`VectorAsset::groups`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct GroupId(pub(crate) u16);

impl GroupId {
    /// The group's position in cook order.
    #[inline]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Size facts about one group, for budgeting and culling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GroupInfo {
    /// Triangle primitives drawn by one `draw_group`.
    pub triangles: u16,
    /// Quad primitives drawn by one `draw_group` (each is two GPU triangles).
    pub quads: u16,
    /// Vertices transformed by one `draw_group`; the [`Scratch`](crate::Scratch)
    /// capacity must be at least this.
    pub vertices: u16,
    /// Bounding box `[min_x, min_y, max_x, max_y]` in Q4 pixels.
    pub bounds_q4: [i16; 4],
    /// Default pivot `[x, y]` in Q4 pixels.
    pub pivot_q4: [i16; 2],
}

/// A validated, zero-copy view of a cooked PSVG asset.
///
/// Parsing checks every offset, range and triangle index once, so the draw
/// paths can index without further bounds checks.
#[derive(Clone, Copy, Debug)]
pub struct VectorAsset<'a> {
    pub(crate) groups: &'a [GroupRecord],
    pub(crate) vertices: &'a [Vertex],
    pub(crate) indices: &'a [u16],
    pub(crate) runs: &'a [Run],
    names: &'a [u8],
    canvas_q4: [u16; 2],
}

fn read_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// View `count` records of `T` at byte offset `off`.
///
/// `T` must be a `repr(C)` plain-old-data type for which every bit pattern is
/// valid.
fn records<T>(bytes: &[u8], off: usize, count: usize) -> Result<&[T], ParseError> {
    let len = count
        .checked_mul(size_of::<T>())
        .ok_or(ParseError::BadSection)?;
    let end = off.checked_add(len).ok_or(ParseError::BadSection)?;
    if end > bytes.len() {
        return Err(ParseError::Truncated);
    }
    let ptr = bytes.as_ptr().wrapping_add(off);
    if ptr.align_offset(align_of::<T>()) != 0 {
        return Err(ParseError::Misaligned);
    }
    // SAFETY: `ptr..ptr + len` lies inside `bytes` (checked above), is aligned
    // for `T`, and `T` is a padding-free `repr(C)` integer record, so every bit
    // pattern is a valid value. The result borrows `bytes`.
    Ok(unsafe { core::slice::from_raw_parts(ptr.cast::<T>(), count) })
}

impl<'a> VectorAsset<'a> {
    /// Validate `bytes` and view it as an asset.
    ///
    /// `bytes` must start on a 4-byte boundary.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ParseError> {
        if bytes.len() < HEADER_LEN {
            return Err(ParseError::Truncated);
        }
        if bytes.as_ptr().align_offset(4) != 0 {
            return Err(ParseError::Misaligned);
        }
        if bytes[0..4] != MAGIC {
            return Err(ParseError::BadMagic);
        }
        let version = read_u16(bytes, 4);
        if version != VERSION {
            return Err(ParseError::UnsupportedVersion(version));
        }
        let group_count = read_u16(bytes, 6) as usize;
        let canvas_q4 = [read_u16(bytes, 8), read_u16(bytes, 10)];
        let vertex_count = read_u32(bytes, 12) as usize;
        let index_count = read_u32(bytes, 16) as usize;
        let run_count = read_u32(bytes, 20) as usize;
        let names_len = read_u32(bytes, 24) as usize;
        let off = |at: usize| read_u32(bytes, at) as usize;
        let (off_groups, off_vertices, off_indices, off_runs, off_names) =
            (off(28), off(32), off(36), off(40), off(44));
        if read_u32(bytes, 48) as usize != bytes.len() {
            return Err(ParseError::Truncated);
        }
        for section in [off_groups, off_vertices, off_indices, off_runs, off_names] {
            if !section.is_multiple_of(4) || section < HEADER_LEN || section > bytes.len() {
                return Err(ParseError::BadSection);
            }
        }
        let groups = records::<GroupRecord>(bytes, off_groups, group_count)?;
        let vertices = records::<Vertex>(bytes, off_vertices, vertex_count)?;
        let indices = records::<u16>(bytes, off_indices, index_count)?;
        let runs = records::<Run>(bytes, off_runs, run_count)?;
        let names = bytes
            .get(
                off_names
                    ..off_names
                        .checked_add(names_len)
                        .ok_or(ParseError::BadSection)?,
            )
            .ok_or(ParseError::Truncated)?;

        for group in groups {
            Self::validate_group(group, vertices, indices, runs, names)?;
        }
        Ok(Self {
            groups,
            vertices,
            indices,
            runs,
            names,
            canvas_q4,
        })
    }

    fn validate_group(
        group: &GroupRecord,
        vertices: &[Vertex],
        indices: &[u16],
        runs: &[Run],
        names: &[u8],
    ) -> Result<(), ParseError> {
        let vertex_start = group.vertex_start as usize;
        let vertex_count = group.vertex_count as usize;
        let index_start = group.index_start as usize;
        let index_len = group.tri_count as usize * 3 + group.quad_count as usize * 4;
        let run_start = group.run_start as usize;
        let run_count = group.run_count as usize;
        if vertex_start
            .checked_add(vertex_count)
            .is_none_or(|e| e > vertices.len())
            || index_start
                .checked_add(index_len)
                .is_none_or(|e| e > indices.len())
            || run_start
                .checked_add(run_count)
                .is_none_or(|e| e > runs.len())
        {
            return Err(ParseError::BadGroup);
        }
        if indices[index_start..index_start + index_len]
            .iter()
            .any(|&i| i as usize >= vertex_count)
        {
            return Err(ParseError::BadIndex);
        }
        let (mut run_tris, mut run_quads) = (0usize, 0usize);
        for run in &runs[run_start..run_start + run_count] {
            if blend_from_code(run.blend).is_none() {
                return Err(ParseError::BadRun);
            }
            match run.kind {
                RUN_TRIANGLES => run_tris += run.count as usize,
                RUN_QUADS => run_quads += run.count as usize,
                _ => return Err(ParseError::BadRun),
            }
        }
        if run_tris != group.tri_count as usize || run_quads != group.quad_count as usize {
            return Err(ParseError::BadRun);
        }
        let name_start = group.name_off as usize;
        let name = names
            .get(name_start..name_start + group.name_len as usize)
            .ok_or(ParseError::BadName)?;
        core::str::from_utf8(name).map_err(|_| ParseError::BadName)?;
        Ok(())
    }

    /// Number of groups.
    #[inline]
    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    /// Canvas size `[width, height]` in Q4 pixels, from the SVG's size.
    #[inline]
    pub fn canvas_q4(&self) -> [u16; 2] {
        self.canvas_q4
    }

    /// Every group, in cook (document) order.
    pub fn groups(&self) -> impl Iterator<Item = GroupId> {
        (0..self.groups.len() as u16).map(GroupId)
    }

    /// The group with SVG id `name`.
    ///
    /// A linear scan over a handful of short names; look groups up once at load
    /// and keep the [`GroupId`]s.
    pub fn group(&self, name: &str) -> Option<GroupId> {
        self.groups().find(|&id| self.group_name(id) == name)
    }

    /// The SVG id of `group`; empty for shapes that sat outside any id.
    pub fn group_name(&self, group: GroupId) -> &'a str {
        let record = &self.groups[group.index()];
        let start = record.name_off as usize;
        // Parse validated the range and the UTF-8, so this cannot fail; the
        // fallback keeps the function total without a panic path.
        core::str::from_utf8(&self.names[start..start + record.name_len as usize]).unwrap_or("")
    }

    /// Size facts for `group`.
    pub fn group_info(&self, group: GroupId) -> GroupInfo {
        let record = &self.groups[group.index()];
        GroupInfo {
            triangles: record.tri_count,
            quads: record.quad_count,
            vertices: record.vertex_count,
            bounds_q4: record.bounds,
            pivot_q4: record.pivot,
        }
    }

    /// The most vertices any one group transforms: the [`Scratch`](crate::Scratch)
    /// capacity that fits every group of this asset.
    pub fn max_group_vertices(&self) -> usize {
        self.groups
            .iter()
            .map(|g| g.vertex_count as usize)
            .max()
            .unwrap_or(0)
    }

    /// Total GPU triangles over all groups (a quad counts as two).
    pub fn triangle_count(&self) -> usize {
        self.groups
            .iter()
            .map(|g| g.tri_count as usize + 2 * g.quad_count as usize)
            .sum()
    }

    /// The indices of `group`: three per triangle and four per quad, in paint order.
    pub(crate) fn group_indices(&self, group: &GroupRecord) -> &'a [u16] {
        let start = group.index_start as usize;
        let len = group.tri_count as usize * 3 + group.quad_count as usize * 4;
        &self.indices[start..start + len]
    }

    pub(crate) fn group_vertices(&self, group: &GroupRecord) -> &'a [Vertex] {
        let start = group.vertex_start as usize;
        &self.vertices[start..start + group.vertex_count as usize]
    }

    pub(crate) fn group_runs(&self, group: &GroupRecord) -> &'a [Run] {
        let start = group.run_start as usize;
        &self.runs[start..start + group.run_count as usize]
    }
}
