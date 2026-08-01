// SPDX-License-Identifier: GPL-2.0-or-later
#![cfg_attr(target_arch = "mips", feature(asm_experimental_arch))]
//! Runtime parsers for PSoXide cooked-asset blobs.
//!
//! Pairs with `editor/crates/psxed`, the host-side tool that
//! produces these files. Format structs live in `psxed-format`
//! (shared by both sides so drift is impossible).
//!
//! Usage pattern:
//!
//! ```ignore
//! // At compile time, embed the cooked blob into the MIPS binary.
//! static TEAPOT: &[u8] = include_bytes!("assets/teapot.psxm");
//!
//! // At runtime, parse into a typed view. Zero-copy -- the view
//! // just borrows slices into the original byte stream.
//! let mesh = psx_asset::Mesh::from_bytes(TEAPOT).expect("cooked mesh");
//! for tri_idx in 0..mesh.face_count() {
//!     let (ia, ib, ic) = mesh.face(tri_idx);
//!     let v0 = mesh.vertex(ia);
//!     let v1 = mesh.vertex(ib);
//!     let v2 = mesh.vertex(ic);
//!     let (r, g, b) = mesh.face_color(tri_idx).unwrap_or((128, 128, 128));
//!     // project + draw …
//! }
//! ```
//!
//! Design:
//!
//! - **Zero-copy**: `Mesh::from_bytes` borrows into the caller's
//!   byte slice. No allocation, no memcpy; the static blob stays
//!   where it was embedded.
//! - **`no_std`-clean**: all parsing is manual LE decode, no
//!   `std::io`.
//! - **Bounds-checked**: every accessor validates its index
//!   against the counts in the header. Malformed blobs produce
//!   `None` rather than panics at integer level, so consumers
//!   see errors at load time, not in the render loop.

#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

use psx_gte::math::{Vec3I16, Vec3I32};

const MESH_VERSION_U8_INDICES: u16 = 1;
const MESH_U8_INDEX_STRIDE: usize = 3;
const MESH_U16_INDEX_STRIDE: usize = 6;

/// Shared topology helpers for cooked world quads and wall shapes.
pub mod world_topology {
    pub use psxed_format::world::topology::{
        horizontal_triangle_at_local, split_triangle, split_triangles, triangle_contains_corner,
        wall_shape_for_dropped_corner, wall_shape_triangle, wall_shape_triangle_corners,
        SplitTriangles, TriangleCorners, HORIZONTAL_NE_SW_TRIANGLES, HORIZONTAL_NW_SE_TRIANGLES,
        SPLIT_ONE_THREE_TRIANGLES, SPLIT_ZERO_TWO_TRIANGLES, WHOLE_QUAD_TRIANGLE_INDEX,
    };
}

/// World split id from north-west to south-east.
pub const WORLD_SPLIT_NORTH_WEST_SOUTH_EAST: u8 = psxed_format::world::split::NORTH_WEST_SOUTH_EAST;

/// World split id from north-east to south-west.
pub const WORLD_SPLIT_NORTH_EAST_SOUTH_WEST: u8 = psxed_format::world::split::NORTH_EAST_SOUTH_WEST;

/// Full four-corner world-wall shape.
pub const WORLD_WALL_SHAPE_QUAD: u16 = psxed_format::world::wall_shape::QUAD;

/// World-wall shape with the bottom-left corner removed.
pub const WORLD_WALL_SHAPE_DROP_BOTTOM_LEFT: u16 =
    psxed_format::world::wall_shape::DROP_BOTTOM_LEFT;

/// World-wall shape with the bottom-right corner removed.
pub const WORLD_WALL_SHAPE_DROP_BOTTOM_RIGHT: u16 =
    psxed_format::world::wall_shape::DROP_BOTTOM_RIGHT;

/// World-wall shape with the top-right corner removed.
pub const WORLD_WALL_SHAPE_DROP_TOP_RIGHT: u16 = psxed_format::world::wall_shape::DROP_TOP_RIGHT;

/// World-wall shape with the top-left corner removed.
pub const WORLD_WALL_SHAPE_DROP_TOP_LEFT: u16 = psxed_format::world::wall_shape::DROP_TOP_LEFT;

/// Errors `Mesh::from_bytes` can return for a malformed blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// Blob shorter than the minimum header sizes.
    Truncated,
    /// `AssetHeader::magic` doesn't match the expected asset kind.
    WrongMagic,
    /// Format version newer than this parser supports.
    UnsupportedVersion(u16),
    /// Declared payload_len disagrees with the actual byte length.
    InvalidPayloadLen {
        /// Payload length declared in the common asset header.
        declared: u32,
        /// Actual payload byte count after the common asset header.
        actual: usize,
    },
    /// Vertex or face table wouldn't fit in the remaining payload.
    TableOverflow,
    /// World-grid header fields are inconsistent.
    InvalidWorldLayout,
    /// Model header/table fields are inconsistent.
    InvalidModelLayout,
    /// Animation header/table fields are inconsistent.
    InvalidAnimationLayout,
    /// Audio header/table fields are inconsistent.
    InvalidAudioLayout,
}

/// A parsed 3D mesh backed by slices into the caller's cooked blob.
///
/// Cheap to construct (just bounds-checks the header + computes
/// sub-slice offsets). Cheap to pass around -- table `&[u8]` slices,
/// an index stride, a flags `u16`, and the counts.
#[derive(Copy, Clone, Debug)]
pub struct Mesh<'a> {
    verts: &'a [u8],
    indices: &'a [u8],
    face_colors: Option<&'a [u8]>,
    vertex_normals: Option<&'a [u8]>,
    index_stride: u8,
    vert_count: u16,
    face_count: u16,
    flags: u16,
}

impl<'a> Mesh<'a> {
    /// Parse a cooked `.psxm` blob. Returns a `Mesh` view that
    /// borrows into `bytes`.
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, ParseError> {
        // AssetHeader.
        if bytes.len() < psxed_format::AssetHeader::SIZE {
            return Err(ParseError::Truncated);
        }
        let magic: [u8; 4] = [bytes[0], bytes[1], bytes[2], bytes[3]];
        if magic != psxed_format::mesh::MAGIC {
            return Err(ParseError::WrongMagic);
        }
        let version = u16::from_le_bytes([bytes[4], bytes[5]]);
        let index_stride = match version {
            MESH_VERSION_U8_INDICES => MESH_U8_INDEX_STRIDE,
            psxed_format::mesh::VERSION => MESH_U16_INDEX_STRIDE,
            _ => return Err(ParseError::UnsupportedVersion(version)),
        };
        let flags = u16::from_le_bytes([bytes[6], bytes[7]]);
        let payload_len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        let payload_start = psxed_format::AssetHeader::SIZE;
        let actual_payload = bytes.len().saturating_sub(payload_start);
        if (payload_len as usize) != actual_payload {
            return Err(ParseError::InvalidPayloadLen {
                declared: payload_len,
                actual: actual_payload,
            });
        }

        // MeshHeader.
        if actual_payload < psxed_format::mesh::MeshHeader::SIZE {
            return Err(ParseError::Truncated);
        }
        let mh = &bytes[payload_start..];
        let vert_count = u16::from_le_bytes([mh[0], mh[1]]);
        let face_count = u16::from_le_bytes([mh[2], mh[3]]);
        // Skip _reserved (4 bytes).

        // Slice the vertex + index + optional colour tables out.
        let mut off = payload_start + psxed_format::mesh::MeshHeader::SIZE;
        let vert_bytes = (vert_count as usize) * 6;
        if off + vert_bytes > bytes.len() {
            return Err(ParseError::TableOverflow);
        }
        let verts = &bytes[off..off + vert_bytes];
        off += vert_bytes;

        let index_bytes = (face_count as usize) * index_stride;
        if off + index_bytes > bytes.len() {
            return Err(ParseError::TableOverflow);
        }
        let indices = &bytes[off..off + index_bytes];
        off += index_bytes;

        let face_colors = if flags & psxed_format::mesh::flags::HAS_FACE_COLORS != 0 {
            let color_bytes = (face_count as usize) * 3;
            if off + color_bytes > bytes.len() {
                return Err(ParseError::TableOverflow);
            }
            let slice = &bytes[off..off + color_bytes];
            off += color_bytes;
            Some(slice)
        } else {
            None
        };

        let vertex_normals = if flags & psxed_format::mesh::flags::HAS_NORMALS != 0 {
            let normal_bytes = (vert_count as usize) * 6;
            if off + normal_bytes > bytes.len() {
                return Err(ParseError::TableOverflow);
            }
            let slice = &bytes[off..off + normal_bytes];
            Some(slice)
        } else {
            None
        };

        Ok(Self {
            verts,
            indices,
            face_colors,
            vertex_normals,
            index_stride: index_stride as u8,
            vert_count,
            face_count,
            flags,
        })
    }

    /// Vertex count.
    #[inline]
    pub fn vert_count(&self) -> u16 {
        self.vert_count
    }

    /// Triangle count.
    #[inline]
    pub fn face_count(&self) -> u16 {
        self.face_count
    }

    /// Mesh feature flags (see [`psxed_format::mesh::flags`]).
    #[inline]
    pub fn flags(&self) -> u16 {
        self.flags
    }

    /// Does the blob carry a face-colour table?
    #[inline]
    pub fn has_face_colors(&self) -> bool {
        self.flags & psxed_format::mesh::flags::HAS_FACE_COLORS != 0
    }

    /// Does the blob carry per-vertex normals? Required for any
    /// GTE- or CPU-lit rendering path.
    #[inline]
    pub fn has_normals(&self) -> bool {
        self.flags & psxed_format::mesh::flags::HAS_NORMALS != 0
    }

    /// Decode vertex `i` as a Q3.12 [`Vec3I16`]. Returns
    /// [`Vec3I16::ZERO`] if the index is out of range -- keeps
    /// the render path branch-free, callers who care can
    /// check against [`Self::vert_count`] first.
    #[inline]
    pub fn vertex(&self, i: u16) -> Vec3I16 {
        let idx = i as usize;
        if idx >= self.vert_count as usize {
            return Vec3I16::ZERO;
        }
        let base = idx * 6;
        let x = i16::from_le_bytes([self.verts[base], self.verts[base + 1]]);
        let y = i16::from_le_bytes([self.verts[base + 2], self.verts[base + 3]]);
        let z = i16::from_le_bytes([self.verts[base + 4], self.verts[base + 5]]);
        Vec3I16::new(x, y, z)
    }

    /// Triangle `i`'s three vertex indices. Returns `(0, 0, 0)`
    /// for an out-of-range index.
    #[inline]
    pub fn face(&self, i: u16) -> (u16, u16, u16) {
        let idx = i as usize;
        if idx >= self.face_count as usize {
            return (0, 0, 0);
        }
        let base = idx * self.index_stride as usize;
        if self.index_stride as usize == MESH_U16_INDEX_STRIDE {
            (
                u16::from_le_bytes([self.indices[base], self.indices[base + 1]]),
                u16::from_le_bytes([self.indices[base + 2], self.indices[base + 3]]),
                u16::from_le_bytes([self.indices[base + 4], self.indices[base + 5]]),
            )
        } else {
            (
                self.indices[base] as u16,
                self.indices[base + 1] as u16,
                self.indices[base + 2] as u16,
            )
        }
    }

    /// Triangle `i`'s flat colour, or `None` if the blob doesn't
    /// carry a face-colour table.
    #[inline]
    pub fn face_color(&self, i: u16) -> Option<(u8, u8, u8)> {
        let colors = self.face_colors?;
        let idx = i as usize;
        if idx >= self.face_count as usize {
            return None;
        }
        let base = idx * 3;
        Some((colors[base], colors[base + 1], colors[base + 2]))
    }

    /// Per-vertex Q3.12 normal. Returns `None` if the blob lacks
    /// a normal table (`HAS_NORMALS` flag clear) or `i` is out of
    /// range. Components are unit-length-ish (Q3.12 quantisation
    /// introduces sub-ULP error but the GTE lighting path is
    /// tolerant).
    #[inline]
    pub fn vertex_normal(&self, i: u16) -> Option<Vec3I16> {
        let normals = self.vertex_normals?;
        let idx = i as usize;
        if idx >= self.vert_count as usize {
            return None;
        }
        let base = idx * 6;
        let x = i16::from_le_bytes([normals[base], normals[base + 1]]);
        let y = i16::from_le_bytes([normals[base + 2], normals[base + 3]]);
        let z = i16::from_le_bytes([normals[base + 4], normals[base + 5]]);
        Some(Vec3I16::new(x, y, z))
    }
}

/// A parsed textured 3D model backed by slices into the caller's
/// cooked `.psxmdl` blob.
///
/// This is intentionally a low-level view. It exposes the tables the
/// renderer needs -- joints, materials, rigid parts, vertices, and
/// faces -- without allocating or converting the whole model at load
/// time.
#[derive(Copy, Clone, Debug)]
pub struct Model<'a> {
    joints: &'a [u8],
    materials: &'a [u8],
    parts: &'a [u8],
    vertices: &'a [u8],
    faces: &'a [u8],
    joint_count: u16,
    material_count: u16,
    part_count: u16,
    vertex_count: u16,
    face_count: u16,
    texture_width: u16,
    texture_height: u16,
    local_to_world_q12: u16,
    flags: u16,
}

impl<'a> Model<'a> {
    /// Parse a cooked `.psxmdl` blob.
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, ParseError> {
        use psxed_format::model::{
            JointRecord, MaterialRecord, ModelHeader, PartRecord, FACE_RECORD_SIZE, MAGIC, VERSION,
            VERTEX_RECORD_SIZE,
        };

        if bytes.len() < psxed_format::AssetHeader::SIZE {
            return Err(ParseError::Truncated);
        }
        let magic = [bytes[0], bytes[1], bytes[2], bytes[3]];
        if magic != MAGIC {
            return Err(ParseError::WrongMagic);
        }
        let version = read_u16(bytes, 4);
        if version != VERSION {
            return Err(ParseError::UnsupportedVersion(version));
        }
        let flags = read_u16(bytes, 6);
        let payload_len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        let payload_start = psxed_format::AssetHeader::SIZE;
        let actual_payload = bytes.len().saturating_sub(payload_start);
        if payload_len as usize != actual_payload {
            return Err(ParseError::InvalidPayloadLen {
                declared: payload_len,
                actual: actual_payload,
            });
        }
        if actual_payload < ModelHeader::SIZE {
            return Err(ParseError::Truncated);
        }

        let mh = &bytes[payload_start..payload_start + ModelHeader::SIZE];
        let joint_count = read_u16(mh, 0);
        let part_count = read_u16(mh, 2);
        let vertex_count = read_u16(mh, 4);
        let face_count = read_u16(mh, 6);
        let material_count = read_u16(mh, 8);
        let texture_width = read_u16(mh, 10);
        let texture_height = read_u16(mh, 12);
        let local_to_world_q12 = read_u16(mh, 14);

        let mut off = payload_start + ModelHeader::SIZE;

        let joint_bytes = checked_table_bytes(joint_count, JointRecord::SIZE)?;
        let joints = take_table(bytes, &mut off, joint_bytes)?;

        let material_bytes = checked_table_bytes(material_count, MaterialRecord::SIZE)?;
        let materials = take_table(bytes, &mut off, material_bytes)?;

        let part_bytes = checked_table_bytes(part_count, PartRecord::SIZE)?;
        let parts = take_table(bytes, &mut off, part_bytes)?;

        let vertex_bytes = checked_table_bytes(vertex_count, VERTEX_RECORD_SIZE)?;
        let vertices = take_table(bytes, &mut off, vertex_bytes)?;

        let face_bytes = checked_table_bytes(face_count, FACE_RECORD_SIZE)?;
        let faces = take_table(bytes, &mut off, face_bytes)?;

        if off != bytes.len() {
            return Err(ParseError::InvalidModelLayout);
        }
        validate_model_parts(parts, joint_count, material_count, vertex_count, face_count)?;
        validate_model_faces(faces, vertex_count)?;

        Ok(Self {
            joints,
            materials,
            parts,
            vertices,
            faces,
            joint_count,
            material_count,
            part_count,
            vertex_count,
            face_count,
            texture_width,
            texture_height,
            local_to_world_q12,
            flags,
        })
    }

    /// Model feature flags (see [`psxed_format::model::flags`]).
    #[inline]
    pub fn flags(&self) -> u16 {
        self.flags
    }

    /// Whether this model should render double-sided (no backface
    /// culling). Set by the cooker for hollow / open-faced models.
    #[inline]
    pub fn double_sided(&self) -> bool {
        self.flags & psxed_format::model::flags::DOUBLE_SIDED != 0
    }

    /// Number of joint records.
    #[inline]
    pub fn joint_count(&self) -> u16 {
        self.joint_count
    }

    /// Number of material records.
    #[inline]
    pub fn material_count(&self) -> u16 {
        self.material_count
    }

    /// Number of rigid part records.
    #[inline]
    pub fn part_count(&self) -> u16 {
        self.part_count
    }

    /// Number of vertex records.
    #[inline]
    pub fn vertex_count(&self) -> u16 {
        self.vertex_count
    }

    /// Number of triangle records.
    #[inline]
    pub fn face_count(&self) -> u16 {
        self.face_count
    }

    /// Primary texture width in texels.
    #[inline]
    pub fn texture_width(&self) -> u16 {
        self.texture_width
    }

    /// Primary texture height in texels.
    #[inline]
    pub fn texture_height(&self) -> u16 {
        self.texture_height
    }

    /// Suggested uniform scale from model-local units to engine world units.
    ///
    /// `0x1000` is identity. Older blobs may store zero in the reserved
    /// header slot; those are treated as identity.
    #[inline]
    pub fn local_to_world_q12(&self) -> u16 {
        if self.local_to_world_q12 == 0 {
            psxed_format::model::DEFAULT_LOCAL_TO_WORLD_Q12
        } else {
            self.local_to_world_q12
        }
    }

    /// Joint record by index.
    #[inline]
    pub fn joint(&self, index: u16) -> Option<ModelJoint> {
        if index >= self.joint_count {
            return None;
        }
        let base = index as usize * psxed_format::model::JointRecord::SIZE;
        let bytes = self
            .joints
            .get(base..base + psxed_format::model::JointRecord::SIZE)?;
        Some(ModelJoint {
            parent: read_u16(bytes, 0),
        })
    }

    /// Material record by index.
    #[inline]
    pub fn material(&self, index: u16) -> Option<ModelMaterial> {
        if index >= self.material_count {
            return None;
        }
        let base = index as usize * psxed_format::model::MaterialRecord::SIZE;
        let bytes = self
            .materials
            .get(base..base + psxed_format::model::MaterialRecord::SIZE)?;
        Some(ModelMaterial {
            texture_index: read_u16(bytes, 0),
            flags: read_u16(bytes, 2),
            base_color: [bytes[4], bytes[5], bytes[6], bytes[7]],
        })
    }

    /// Rigid part record by index.
    #[inline]
    pub fn part(&self, index: u16) -> Option<ModelPart> {
        if index >= self.part_count {
            return None;
        }
        let base = index as usize * psxed_format::model::PartRecord::SIZE;
        let bytes = self
            .parts
            .get(base..base + psxed_format::model::PartRecord::SIZE)?;
        Some(ModelPart {
            joint_index: read_u16(bytes, 0),
            first_vertex: read_u16(bytes, 2),
            vertex_count: read_u16(bytes, 4),
            first_face: read_u16(bytes, 6),
            face_count: read_u16(bytes, 8),
            material_index: read_u16(bytes, 10),
        })
    }

    /// Vertex record by global vertex index.
    #[inline]
    pub fn vertex(&self, index: u16) -> Option<ModelVertex> {
        if index >= self.vertex_count {
            return None;
        }
        let base = index as usize * psxed_format::model::VERTEX_RECORD_SIZE;
        let bytes = self
            .vertices
            .get(base..base + psxed_format::model::VERTEX_RECORD_SIZE)?;
        Some(ModelVertex {
            position: Vec3I16::new(read_i16(bytes, 0), read_i16(bytes, 2), read_i16(bytes, 4)),
            joint1: bytes[6],
            blend: bytes[7],
        })
    }

    /// Textured triangle by global face index.
    #[inline]
    pub fn face(&self, index: u16) -> Option<ModelFace> {
        if index >= self.face_count {
            return None;
        }
        let base = index as usize * psxed_format::model::FACE_RECORD_SIZE;
        let bytes = self
            .faces
            .get(base..base + psxed_format::model::FACE_RECORD_SIZE)?;
        Some(ModelFace {
            corners: [
                ModelFaceCorner {
                    vertex_index: read_u16(bytes, 0),
                    uv: (bytes[2], bytes[3]),
                },
                ModelFaceCorner {
                    vertex_index: read_u16(bytes, 4),
                    uv: (bytes[6], bytes[7]),
                },
                ModelFaceCorner {
                    vertex_index: read_u16(bytes, 8),
                    uv: (bytes[10], bytes[11]),
                },
            ],
        })
    }
}

/// Decoded model joint record.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ModelJoint {
    parent: u16,
}

impl ModelJoint {
    /// Parent joint index, or `None` for a root joint.
    #[inline]
    pub fn parent(&self) -> Option<u16> {
        if self.parent == psxed_format::model::NO_JOINT {
            None
        } else {
            Some(self.parent)
        }
    }
}

/// Decoded model material record.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ModelMaterial {
    texture_index: u16,
    flags: u16,
    base_color: [u8; 4],
}

impl ModelMaterial {
    /// Texture slot index.
    #[inline]
    pub fn texture_index(&self) -> u16 {
        self.texture_index
    }

    /// Material flags.
    #[inline]
    pub fn flags(&self) -> u16 {
        self.flags
    }

    /// Base colour/tint as RGBA8.
    #[inline]
    pub fn base_color(&self) -> [u8; 4] {
        self.base_color
    }
}

/// Decoded rigid part record.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ModelPart {
    joint_index: u16,
    first_vertex: u16,
    vertex_count: u16,
    first_face: u16,
    face_count: u16,
    material_index: u16,
}

impl ModelPart {
    /// Empty part used for fixed static/runtime pools.
    pub const ZERO: Self = Self {
        joint_index: 0,
        first_vertex: 0,
        vertex_count: 0,
        first_face: 0,
        face_count: 0,
        material_index: 0,
    };

    /// Joint whose animation pose applies to this part.
    #[inline]
    pub fn joint_index(&self) -> u16 {
        self.joint_index
    }

    /// First global vertex owned by this part.
    #[inline]
    pub fn first_vertex(&self) -> u16 {
        self.first_vertex
    }

    /// Number of vertices owned by this part.
    #[inline]
    pub fn vertex_count(&self) -> u16 {
        self.vertex_count
    }

    /// First global triangle owned by this part.
    #[inline]
    pub fn first_face(&self) -> u16 {
        self.first_face
    }

    /// Number of triangles owned by this part.
    #[inline]
    pub fn face_count(&self) -> u16 {
        self.face_count
    }

    /// Material slot used by this part.
    #[inline]
    pub fn material_index(&self) -> u16 {
        self.material_index
    }
}

/// Sentinel value for [`ModelVertex::joint1`] meaning "no secondary
/// blend bone". Re-exported from `psxed_format::model` so runtime
/// code does not need to depend on the editor crate.
pub const NO_JOINT8: u8 = psxed_format::model::NO_JOINT8;

/// Decoded textured model vertex.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ModelVertex {
    /// Model-local position. The cooked importer may use a much denser
    /// scale here than world/grid units; use [`Model::local_to_world_q12`]
    /// when placing the model directly into world space.
    pub position: Vec3I16,
    /// Secondary blend joint, or [`NO_JOINT8`] when this vertex is
    /// single-bone.
    pub joint1: u8,
    /// Weight of `joint1` for view-space blending (0..=255). Zero
    /// signals the renderer to stay on the single-bone GTE fast path.
    pub blend: u8,
}

impl ModelVertex {
    /// Empty vertex record used by fixed-size runtime decode pools.
    pub const ZERO: Self = Self {
        position: Vec3I16::ZERO,
        joint1: NO_JOINT8,
        blend: 0,
    };

    /// `true` when this vertex needs the two-bone blend render path.
    #[inline]
    pub fn is_blend(&self) -> bool {
        self.blend != 0 && self.joint1 != NO_JOINT8
    }
}

/// One textured triangle corner in a cooked model face.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ModelFaceCorner {
    /// Global skinned vertex index.
    pub vertex_index: u16,
    /// 8-bit texture coordinate for this face corner.
    pub uv: (u8, u8),
}

/// Decoded textured model face.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ModelFace {
    /// Three triangle corners in packet order.
    pub corners: [ModelFaceCorner; 3],
}

/// A parsed rigid-skeletal animation backed by slices into the
/// caller's cooked `.psxanim` blob.
#[derive(Copy, Clone, Debug)]
pub struct Animation<'a> {
    poses: &'a [u8],
    joint_count: u16,
    frame_count: u16,
    sample_rate_hz: u16,
    pose_record_size: usize,
    translation_shift: u8,
}

impl<'a> Animation<'a> {
    /// Parse a cooked `.psxanim` blob.
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, ParseError> {
        use psxed_format::animation::{
            AnimationHeader, MAGIC, POSE_RECORD_SIZE, POSE_RECORD_SIZE_V1, POSE_RECORD_SIZE_V3,
            VERSION, VERSION_V1, VERSION_V3,
        };

        if bytes.len() < psxed_format::AssetHeader::SIZE {
            return Err(ParseError::Truncated);
        }
        let magic = [bytes[0], bytes[1], bytes[2], bytes[3]];
        if magic != MAGIC {
            return Err(ParseError::WrongMagic);
        }
        let version = read_u16(bytes, 4);
        let pose_record_size = match version {
            VERSION => POSE_RECORD_SIZE,
            VERSION_V1 => POSE_RECORD_SIZE_V1,
            VERSION_V3 => POSE_RECORD_SIZE_V3,
            _ => {
                return Err(ParseError::UnsupportedVersion(version));
            }
        };
        let payload_len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        let payload_start = psxed_format::AssetHeader::SIZE;
        let actual_payload = bytes.len().saturating_sub(payload_start);
        if payload_len as usize != actual_payload {
            return Err(ParseError::InvalidPayloadLen {
                declared: payload_len,
                actual: actual_payload,
            });
        }
        if actual_payload < AnimationHeader::SIZE {
            return Err(ParseError::Truncated);
        }

        let ah = &bytes[payload_start..payload_start + AnimationHeader::SIZE];
        let joint_count = read_u16(ah, 0);
        let frame_count = read_u16(ah, 2);
        let sample_rate_hz = read_u16(ah, 4);
        let translation_shift = if version == VERSION || version == VERSION_V3 {
            let shift = read_u16(ah, 6);
            if shift > 15 {
                return Err(ParseError::InvalidAnimationLayout);
            }
            shift as u8
        } else {
            0
        };
        if joint_count == 0 || frame_count == 0 || sample_rate_hz == 0 {
            return Err(ParseError::InvalidAnimationLayout);
        }

        let mut off = payload_start + AnimationHeader::SIZE;
        let pose_count = (joint_count as usize)
            .checked_mul(frame_count as usize)
            .ok_or(ParseError::TableOverflow)?;
        let pose_bytes = pose_count
            .checked_mul(pose_record_size)
            .ok_or(ParseError::TableOverflow)?;
        let poses = take_table(bytes, &mut off, pose_bytes)?;
        if off != bytes.len() {
            return Err(ParseError::InvalidAnimationLayout);
        }

        Ok(Self {
            poses,
            joint_count,
            frame_count,
            sample_rate_hz,
            pose_record_size,
            translation_shift,
        })
    }

    /// Number of joint poses per frame.
    #[inline]
    pub fn joint_count(&self) -> u16 {
        self.joint_count
    }

    /// Number of sampled frames.
    #[inline]
    pub fn frame_count(&self) -> u16 {
        self.frame_count
    }

    /// Integer sample rate in Hz.
    #[inline]
    pub fn sample_rate_hz(&self) -> u16 {
        self.sample_rate_hz
    }

    /// Byte size of one stored pose record.
    #[inline]
    pub fn pose_record_size(&self) -> usize {
        self.pose_record_size
    }

    /// Shared right shift used by compact v2 stored translations.
    #[inline]
    pub fn translation_shift(&self) -> u8 {
        self.translation_shift
    }

    /// Q12 sampled-frame phase advance for one playback tick.
    ///
    /// `playback_hz` is the caller's update cadence. For example,
    /// a 15 Hz cooked clip played by a 30 Hz update loop advances by
    /// half a sampled frame per tick (`0x0800`).
    #[inline]
    pub fn phase_step_q12(&self, playback_hz: u16) -> u32 {
        ((self.sample_rate_hz as u32) << 12) / playback_hz.max(1) as u32
    }

    /// Convert a fixed-rate playback tick to a Q12 sampled-frame phase.
    ///
    /// This is a convenience for frame-locked demos. More advanced
    /// scenes can accumulate [`Animation::phase_step_q12`] themselves
    /// when playback speed, pausing, or elapsed-time correction matters.
    #[inline]
    pub fn phase_at_tick_q12(&self, playback_tick: u32, playback_hz: u16) -> u32 {
        playback_tick.wrapping_mul(self.phase_step_q12(playback_hz))
    }

    /// Like [`Animation::phase_at_tick_q12`] but scales the advance by a
    /// Q8 speed multiplier (`256 = 1.0x`): `< 256` plays slower, `> 256`
    /// faster. Scaling the per-tick step rather than the accumulated
    /// phase keeps looping wrap-around identical to unscaled playback.
    #[inline]
    pub fn phase_at_tick_scaled_q12(
        &self,
        playback_tick: u32,
        playback_hz: u16,
        speed_q8: u16,
    ) -> u32 {
        let scaled_step =
            // psx-numeric-allow-next-line: phase-step scale widens through u64; one multu, no 64-bit division
            ((self.phase_step_q12(playback_hz) as u64 * speed_q8 as u64) / 256) as u32;
        playback_tick.wrapping_mul(scaled_step)
    }

    /// Joint pose at `frame_index`, `joint_index`.
    pub fn pose(&self, frame_index: u16, joint_index: u16) -> Option<JointPose> {
        if frame_index >= self.frame_count || joint_index >= self.joint_count {
            return None;
        }
        let base = frame_index as usize * self.joint_count as usize * self.pose_record_size;
        // SAFETY: both indices were checked above. `from_bytes` validates
        // the complete frame/joint table before constructing `Animation`.
        Some(unsafe { self.pose_at_frame_offset_unchecked(base, joint_index) })
    }

    #[inline]
    unsafe fn pose_at_frame_offset_unchecked(
        &self,
        frame_offset: usize,
        joint_index: u16,
    ) -> JointPose {
        let base = frame_offset + joint_index as usize * self.pose_record_size;
        if self.poses.as_ptr() as usize & 1 != 0 {
            return unsafe { self.pose_at_byte_offset_unaligned(base) };
        }
        if self.pose_record_size == psxed_format::animation::POSE_RECORD_SIZE_V3 {
            return unsafe { self.pose_v3_unchecked(base) };
        }
        let matrix = unsafe { read_pose_matrix_aligned_unchecked(self.poses, base) };
        let off = 18;
        let translation = if self.pose_record_size == psxed_format::animation::POSE_RECORD_SIZE {
            Vec3I32::new(
                decode_packed_translation(
                    unsafe { read_i16_aligned_unchecked(self.poses, base + off) },
                    self.translation_shift,
                ),
                decode_packed_translation(
                    unsafe { read_i16_aligned_unchecked(self.poses, base + off + 2) },
                    self.translation_shift,
                ),
                decode_packed_translation(
                    unsafe { read_i16_aligned_unchecked(self.poses, base + off + 4) },
                    self.translation_shift,
                ),
            )
        } else {
            Vec3I32::new(
                unsafe { read_i32_unchecked(self.poses, base + off) },
                unsafe { read_i32_unchecked(self.poses, base + off + 4) },
                unsafe { read_i32_unchecked(self.poses, base + off + 8) },
            )
        };
        JointPose {
            matrix,
            translation,
        }
    }

    /// Decode one v3 (Q11-packed) record: rotation block at +0,
    /// shifted i16 translations at +14. Byte-wise loads throughout, so
    /// alignment is irrelevant.
    ///
    /// # Safety
    /// `base + 20` must be in bounds.
    unsafe fn pose_v3_unchecked(&self, base: usize) -> JointPose {
        let matrix = unsafe { read_pose_matrix_q11_unchecked(self.poses, base) };
        let off = psxed_format::animation::POSE_ROTATION_BLOCK_SIZE_V3;
        let translation = Vec3I32::new(
            decode_packed_translation(
                unsafe { read_i16_unchecked(self.poses, base + off) },
                self.translation_shift,
            ),
            decode_packed_translation(
                unsafe { read_i16_unchecked(self.poses, base + off + 2) },
                self.translation_shift,
            ),
            decode_packed_translation(
                unsafe { read_i16_unchecked(self.poses, base + off + 4) },
                self.translation_shift,
            ),
        );
        JointPose {
            matrix,
            translation,
        }
    }

    #[inline(never)]
    unsafe fn pose_at_byte_offset_unaligned(&self, base: usize) -> JointPose {
        if self.pose_record_size == psxed_format::animation::POSE_RECORD_SIZE_V3 {
            return unsafe { self.pose_v3_unchecked(base) };
        }
        let matrix = unsafe { read_pose_matrix_unchecked(self.poses, base) };
        let off = 18;
        let translation = if self.pose_record_size == psxed_format::animation::POSE_RECORD_SIZE {
            Vec3I32::new(
                decode_packed_translation(
                    unsafe { read_i16_unchecked(self.poses, base + off) },
                    self.translation_shift,
                ),
                decode_packed_translation(
                    unsafe { read_i16_unchecked(self.poses, base + off + 2) },
                    self.translation_shift,
                ),
                decode_packed_translation(
                    unsafe { read_i16_unchecked(self.poses, base + off + 4) },
                    self.translation_shift,
                ),
            )
        } else {
            Vec3I32::new(
                unsafe { read_i32_unchecked(self.poses, base + off) },
                unsafe { read_i32_unchecked(self.poses, base + off + 4) },
                unsafe { read_i32_unchecked(self.poses, base + off + 8) },
            )
        };
        JointPose {
            matrix,
            translation,
        }
    }

    #[inline]
    fn packed_pose_at_frame_offset(
        &self,
        frame_offset: usize,
        joint_index: u16,
    ) -> Option<GteJointPose> {
        let v3 = self.pose_record_size == psxed_format::animation::POSE_RECORD_SIZE_V3;
        if joint_index >= self.joint_count
            || !(v3 || self.pose_record_size == psxed_format::animation::POSE_RECORD_SIZE)
        {
            return None;
        }
        let base = frame_offset.checked_add(joint_index as usize * self.pose_record_size)?;
        let bytes = self.poses.get(base..base + self.pose_record_size)?;
        if v3 {
            // SAFETY: the slice covers the whole 20-byte record.
            let matrix = unsafe { read_pose_matrix_q11_unchecked(bytes, 0) };
            let off = psxed_format::animation::POSE_ROTATION_BLOCK_SIZE_V3;
            return Some(GteJointPose {
                matrix,
                translation: Vec3I16::new(
                    read_i16(bytes, off),
                    read_i16(bytes, off + 2),
                    read_i16(bytes, off + 4),
                ),
                translation_shift: self.translation_shift,
            });
        }
        Some(GteJointPose {
            matrix: read_pose_matrix(bytes),
            translation: Vec3I16::new(
                read_i16(bytes, 18),
                read_i16(bytes, 20),
                read_i16(bytes, 22),
            ),
            translation_shift: self.translation_shift,
        })
    }

    /// Interpolated looping joint pose at a Q12 fixed-point frame phase.
    ///
    /// `frame_q12` uses sampled animation frames as the integer unit
    /// and 12 fractional bits. For example, `1 << 12` samples frame
    /// 1 exactly, while `1 << 11` samples halfway between frames 0
    /// and 1. The cooker writes endpoint-inclusive clips, so looping
    /// playback treats the final stored frame as the duplicate of
    /// frame 0 and blends `frame_count - 2` back to frame 0.
    pub fn pose_looped_q12(&self, frame_q12: u32, joint_index: u16) -> Option<JointPose> {
        self.looped_pose_sample_q12(frame_q12)?.pose(joint_index)
    }

    /// Precomputed looping frame pair for sampling multiple joints at
    /// the same animation phase.
    ///
    /// Renderers should prefer this when walking every joint in a
    /// model: the expensive loop-frame modulo and alpha calculation
    /// happens once per draw instead of once per joint.
    pub fn looped_pose_sample_q12(&self, frame_q12: u32) -> Option<AnimationPoseSample<'a>> {
        if self.frame_count == 0 {
            return None;
        }

        let cycle_frames = self.frame_count.saturating_sub(1).max(1);
        let base_frame = ((frame_q12 >> 12) % cycle_frames as u32) as u16;
        let next_frame = if cycle_frames <= 1 || base_frame + 1 >= cycle_frames {
            0
        } else {
            base_frame + 1
        };
        Some(AnimationPoseSample {
            animation: *self,
            base_frame,
            next_frame,
            base_frame_offset: base_frame as usize
                * self.joint_count as usize
                * self.pose_record_size,
            next_frame_offset: next_frame as usize
                * self.joint_count as usize
                * self.pose_record_size,
            alpha_q12: (frame_q12 & 0x0fff) as u16,
        })
    }
}

/// Reusable frame-pair sampler for one animation phase.
#[derive(Copy, Clone, Debug)]
pub struct AnimationPoseSample<'a> {
    animation: Animation<'a>,
    base_frame: u16,
    next_frame: u16,
    base_frame_offset: usize,
    next_frame_offset: usize,
    alpha_q12: u16,
}

impl AnimationPoseSample<'_> {
    /// Joint pose at this sample's precomputed looping phase.
    #[inline]
    pub fn pose(&self, joint_index: u16) -> Option<JointPose> {
        if joint_index >= self.animation.joint_count {
            return None;
        }
        if self.alpha_q12 == 0 || self.base_frame == self.next_frame {
            // SAFETY: the joint index was checked above and the frame offset
            // was computed from a validated animation frame.
            return Some(unsafe {
                self.animation
                    .pose_at_frame_offset_unchecked(self.base_frame_offset, joint_index)
            });
        }

        // SAFETY: the joint index was checked above and both frame offsets
        // were computed from validated animation frames.
        let a = unsafe {
            self.animation
                .pose_at_frame_offset_unchecked(self.base_frame_offset, joint_index)
        };
        let b = unsafe {
            self.animation
                .pose_at_frame_offset_unchecked(self.next_frame_offset, joint_index)
        };
        Some(lerp_pose_q12(a, b, self.alpha_q12))
    }

    /// Packed GTE-friendly joint pose at this sample's phase.
    ///
    /// This is available for current v2 `.psxanim` files. Legacy v1
    /// files return `None` and callers can fall back to [`Self::pose`].
    #[inline]
    pub fn gte_pose(&self, joint_index: u16) -> Option<GteJointPose> {
        if self.alpha_q12 == 0 || self.base_frame == self.next_frame {
            return self
                .animation
                .packed_pose_at_frame_offset(self.base_frame_offset, joint_index);
        }

        let a = self
            .animation
            .packed_pose_at_frame_offset(self.base_frame_offset, joint_index)?;
        let b = self
            .animation
            .packed_pose_at_frame_offset(self.next_frame_offset, joint_index)?;
        Some(lerp_gte_pose_q12(a, b, self.alpha_q12))
    }
}

/// Crossfade source: a second animation sample blended toward a
/// primary pose.
///
/// Used for clip-transition crossfades: the outgoing clip's frozen
/// sample sits here while the incoming clip plays as the primary.
/// `alpha_q12` is the Q12 weight of the PRIMARY pose (0 shows this
/// source, `1 << 12` shows the primary alone).
///
/// The blend is a linear matrix lerp, not a rotation-correct slerp:
/// mid-blend joints shrink slightly when the two poses differ a lot.
/// Short crossfade windows keep that invisible; renormalize later if
/// a long blend ever makes it visible.
#[derive(Copy, Clone, Debug)]
pub struct ModelPoseBlend<'a> {
    /// Outgoing pose sample (frame pair + intra-frame alpha).
    pub sample: AnimationPoseSample<'a>,
    /// Q12 weight of the primary pose this source blends toward.
    pub alpha_q12: u16,
}

impl ModelPoseBlend<'_> {
    /// Blend this source's joint pose toward `primary`.
    ///
    /// Falls back to `primary` unchanged when the joint is missing
    /// from the outgoing clip (mismatched rigs never corrupt poses).
    #[inline]
    pub fn blend_toward(&self, primary: JointPose, joint_index: u16) -> JointPose {
        if self.alpha_q12 >= 1 << 12 {
            return primary;
        }
        match self.sample.pose(joint_index) {
            Some(from) => lerp_pose_q12(from, primary, self.alpha_q12),
            None => primary,
        }
    }
}

/// Decoded joint pose matrix.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct JointPose {
    /// Q3.12 column-major 3×3 transform.
    pub matrix: [[i16; 3]; 3],
    /// Q3.12 translation vector.
    pub translation: Vec3I32,
}

/// Compact joint pose that can feed GTE vector inputs directly.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct GteJointPose {
    /// Q3.12 column-major 3×3 transform.
    pub matrix: [[i16; 3]; 3],
    /// Packed model-local translation vector.
    pub translation: Vec3I16,
    /// Shared left shift used to reconstruct model-local units.
    pub translation_shift: u8,
}

/// A parsed `.psau` SPU audio sample backed by the caller's cooked blob.
#[derive(Copy, Clone, Debug)]
pub struct Audio<'a> {
    adpcm: &'a [u8],
    flags: u16,
    sample_rate_hz: u32,
    sample_count: u32,
    adpcm_block_count: u32,
    loop_start_block: u32,
}

impl<'a> Audio<'a> {
    /// Parse a cooked `.psau` blob.
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, ParseError> {
        use psxed_format::audio::{self, AudioHeader};

        if bytes.len() < psxed_format::AssetHeader::SIZE {
            return Err(ParseError::Truncated);
        }
        let magic = [bytes[0], bytes[1], bytes[2], bytes[3]];
        if magic != audio::MAGIC {
            return Err(ParseError::WrongMagic);
        }
        let version = read_u16(bytes, 4);
        if version != audio::VERSION {
            return Err(ParseError::UnsupportedVersion(version));
        }
        let flags = read_u16(bytes, 6);
        let payload_len = read_u32(bytes, 8);
        let payload_start = psxed_format::AssetHeader::SIZE;
        let actual_payload = bytes.len().saturating_sub(payload_start);
        if payload_len as usize != actual_payload {
            return Err(ParseError::InvalidPayloadLen {
                declared: payload_len,
                actual: actual_payload,
            });
        }
        if actual_payload < AudioHeader::SIZE {
            return Err(ParseError::Truncated);
        }

        let ah = &bytes[payload_start..payload_start + AudioHeader::SIZE];
        let codec = ah[0];
        let channel_count = ah[1];
        let sample_rate_hz = read_u32(ah, 4);
        let sample_count = read_u32(ah, 8);
        let adpcm_block_count = read_u32(ah, 12);
        let loop_start_block = read_u32(ah, 16);
        let decoded_capacity = adpcm_block_count
            .checked_mul(28)
            .ok_or(ParseError::TableOverflow)?;
        let min_samples_for_blocks = adpcm_block_count.saturating_sub(1) * 28 + 1;
        if codec != audio::CODEC_SPU_ADPCM
            || channel_count != 1
            || flags & audio::flags::MONO == 0
            || flags & audio::flags::ONE_SHOT == 0
            || sample_rate_hz == 0
            || sample_count == 0
            || adpcm_block_count == 0
            || sample_count > decoded_capacity
            || sample_count < min_samples_for_blocks
            || loop_start_block != AudioHeader::NO_LOOP
        {
            return Err(ParseError::InvalidAudioLayout);
        }

        let adpcm_bytes = (adpcm_block_count as usize)
            .checked_mul(16)
            .ok_or(ParseError::TableOverflow)?;
        let adpcm_start = payload_start + AudioHeader::SIZE;
        let adpcm_end = adpcm_start
            .checked_add(adpcm_bytes)
            .ok_or(ParseError::TableOverflow)?;
        if adpcm_end != bytes.len() {
            return Err(ParseError::InvalidAudioLayout);
        }

        Ok(Self {
            adpcm: &bytes[adpcm_start..adpcm_end],
            flags,
            sample_rate_hz,
            sample_count,
            adpcm_block_count,
            loop_start_block,
        })
    }

    /// Playback sample rate in Hz.
    #[inline]
    pub fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    /// Audible mono sample count before encoder padding.
    #[inline]
    pub fn sample_count(&self) -> u32 {
        self.sample_count
    }

    /// Number of 16-byte SPU ADPCM blocks.
    #[inline]
    pub fn adpcm_block_count(&self) -> u32 {
        self.adpcm_block_count
    }

    /// Loop-start block, or `None` for one-shot samples.
    #[inline]
    pub fn loop_start_block(&self) -> Option<u32> {
        if self.loop_start_block == psxed_format::audio::AudioHeader::NO_LOOP {
            None
        } else {
            Some(self.loop_start_block)
        }
    }

    /// Raw SPU ADPCM bytes ready for upload.
    #[inline]
    pub fn adpcm_bytes(&self) -> &'a [u8] {
        self.adpcm
    }

    /// Whether this sample is a non-looping one-shot.
    #[inline]
    pub fn is_one_shot(&self) -> bool {
        self.flags & psxed_format::audio::flags::ONE_SHOT != 0
    }
}

#[inline]
fn lerp_pose_q12(a: JointPose, b: JointPose, alpha_q12: u16) -> JointPose {
    let mut matrix = [[0i16; 3]; 3];
    let mut col = 0;
    while col < 3 {
        let mut row = 0;
        while row < 3 {
            matrix[col][row] = lerp_i16_q12(a.matrix[col][row], b.matrix[col][row], alpha_q12);
            row += 1;
        }
        col += 1;
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

#[inline]
fn lerp_gte_pose_q12(a: GteJointPose, b: GteJointPose, alpha_q12: u16) -> GteJointPose {
    let mut matrix = [[0i16; 3]; 3];
    let mut col = 0;
    while col < 3 {
        let mut row = 0;
        while row < 3 {
            matrix[col][row] = lerp_i16_q12(a.matrix[col][row], b.matrix[col][row], alpha_q12);
            row += 1;
        }
        col += 1;
    }

    GteJointPose {
        matrix,
        translation: Vec3I16::new(
            lerp_i16_q12(a.translation.x, b.translation.x, alpha_q12),
            lerp_i16_q12(a.translation.y, b.translation.y, alpha_q12),
            lerp_i16_q12(a.translation.z, b.translation.z, alpha_q12),
        ),
        translation_shift: a.translation_shift,
    }
}

#[inline]
/// Decode one 12-bit Q11 rotation code to a Q3.12 element.
///
/// Ported from hl-psx's silicon-proven decoder: value = code * 2, with
/// the reserved positive-max code 0x7FF decoding to exactly 4096. On
/// MIPS the branchless six-instruction form avoids the select branch
/// LLVM would otherwise emit.
#[inline(always)]
fn decode_q11_element(raw: u16) -> i16 {
    #[cfg(target_arch = "mips")]
    unsafe {
        let decoded: u32;
        core::arch::asm!(
            "xori {scratch}, {decoded}, 0x07ff",
            "sltiu {scratch}, {scratch}, 1",
            "sll {decoded}, {decoded}, 20",
            "sra {decoded}, {decoded}, 19",
            "sll {scratch}, {scratch}, 1",
            "addu {decoded}, {decoded}, {scratch}",
            decoded = inlateout(reg) raw as u32 => decoded,
            scratch = lateout(reg) _,
            options(nomem, nostack, preserves_flags),
        );
        return decoded as i16;
    }

    #[cfg(not(target_arch = "mips"))]
    {
        let signed = ((raw << 4) as i16) >> 4;
        signed
            .wrapping_shl(1)
            .wrapping_add((((raw & 0x0FFF) == 0x07FF) as i16) << 1)
    }
}

/// Decode a v3 packed rotation block (nine 12-bit Q11 codes in
/// fourteen bytes) into the row-major pose matrix. Byte-wise loads:
/// the block is not alignment-guaranteed and the R3000 has no D-cache
/// penalty for it.
///
/// # Safety
/// `offset + 14` must be in bounds (v3 records are 20 bytes, so any
/// in-bounds record satisfies this).
#[inline]
unsafe fn read_pose_matrix_q11_unchecked(bytes: &[u8], offset: usize) -> [[i16; 3]; 3] {
    let mut flat = [0i16; 9];
    let mut pair = 0;
    while pair < 4 {
        let o = offset + pair * 3;
        let packed = unsafe {
            (bytes.as_ptr().add(o).read() as u32)
                | ((bytes.as_ptr().add(o + 1).read() as u32) << 8)
                | ((bytes.as_ptr().add(o + 2).read() as u32) << 16)
        };
        flat[pair * 2] = decode_q11_element((packed & 0x0FFF) as u16);
        flat[pair * 2 + 1] = decode_q11_element(((packed >> 12) & 0x0FFF) as u16);
        pair += 1;
    }
    let last = unsafe {
        (bytes.as_ptr().add(offset + 12).read() as u16)
            | ((bytes.as_ptr().add(offset + 13).read() as u16) << 8)
    };
    flat[8] = decode_q11_element(last & 0x0FFF);
    [
        [flat[0], flat[1], flat[2]],
        [flat[3], flat[4], flat[5]],
        [flat[6], flat[7], flat[8]],
    ]
}

fn read_pose_matrix(bytes: &[u8]) -> [[i16; 3]; 3] {
    [
        [read_i16(bytes, 0), read_i16(bytes, 2), read_i16(bytes, 4)],
        [read_i16(bytes, 6), read_i16(bytes, 8), read_i16(bytes, 10)],
        [
            read_i16(bytes, 12),
            read_i16(bytes, 14),
            read_i16(bytes, 16),
        ],
    ]
}

#[inline]
unsafe fn read_pose_matrix_unchecked(bytes: &[u8], offset: usize) -> [[i16; 3]; 3] {
    [
        [
            unsafe { read_i16_unchecked(bytes, offset) },
            unsafe { read_i16_unchecked(bytes, offset + 2) },
            unsafe { read_i16_unchecked(bytes, offset + 4) },
        ],
        [
            unsafe { read_i16_unchecked(bytes, offset + 6) },
            unsafe { read_i16_unchecked(bytes, offset + 8) },
            unsafe { read_i16_unchecked(bytes, offset + 10) },
        ],
        [
            unsafe { read_i16_unchecked(bytes, offset + 12) },
            unsafe { read_i16_unchecked(bytes, offset + 14) },
            unsafe { read_i16_unchecked(bytes, offset + 16) },
        ],
    ]
}

#[inline]
unsafe fn read_pose_matrix_aligned_unchecked(bytes: &[u8], offset: usize) -> [[i16; 3]; 3] {
    [
        [
            unsafe { read_i16_aligned_unchecked(bytes, offset) },
            unsafe { read_i16_aligned_unchecked(bytes, offset + 2) },
            unsafe { read_i16_aligned_unchecked(bytes, offset + 4) },
        ],
        [
            unsafe { read_i16_aligned_unchecked(bytes, offset + 6) },
            unsafe { read_i16_aligned_unchecked(bytes, offset + 8) },
            unsafe { read_i16_aligned_unchecked(bytes, offset + 10) },
        ],
        [
            unsafe { read_i16_aligned_unchecked(bytes, offset + 12) },
            unsafe { read_i16_aligned_unchecked(bytes, offset + 14) },
            unsafe { read_i16_aligned_unchecked(bytes, offset + 16) },
        ],
    ]
}

#[inline]
fn decode_packed_translation(value: i16, shift: u8) -> i32 {
    debug_assert!(shift <= 15);
    (value as i32) * (1i32 << shift)
}

#[inline]
fn lerp_i16_q12(a: i16, b: i16, alpha_q12: u16) -> i16 {
    debug_assert!(alpha_q12 < 4096);
    let value = a as i32 + (((b as i32 - a as i32) * alpha_q12 as i32) >> 12);
    value as i16
}

#[inline]
fn lerp_i32_q12(a: i32, b: i32, alpha_q12: u16) -> i32 {
    let delta = b.saturating_sub(a);
    a.saturating_add(scale_i32_q12(delta, alpha_q12 as i32))
}

#[inline]
fn scale_i32_q12(value: i32, scale_q12: i32) -> i32 {
    let whole = value >> 12;
    let frac = value - (whole << 12);
    whole.saturating_mul(scale_q12) + ((frac * scale_q12) >> 12)
}

/// A parsed 2D texture backed by slices into the caller's cooked
/// blob. Pixel data is already packed into the halfword-nibble
/// layout the PSX GPU reads; the CLUT (if any) is a slice of
/// RGB555 halfwords.
///
/// Construct by `Texture::from_bytes(&blob)`; upload to VRAM via
/// [`Texture::upload`], which returns the matching `Tpage` + `Clut`
/// handles ready to feed into primitive constructors.
#[derive(Copy, Clone, Debug)]
pub struct Texture<'a> {
    /// Packed pixel halfwords -- 4 texels per u16 at 4bpp,
    /// 2 at 8bpp, 1 Color555 at 15bpp.
    pixel_data: &'a [u8],
    /// CLUT halfwords, or empty for 15bpp.
    clut_data: &'a [u8],
    width_px: u16,
    height_px: u16,
    depth: psxed_format::texture::Depth,
    clut_entries: u16,
    flags: u16,
}

impl<'a> Texture<'a> {
    /// Parse a cooked `.psxt` blob. Returns a `Texture` view that
    /// borrows into `bytes`. Cheap -- header decode + two slice
    /// computations.
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, ParseError> {
        use psxed_format::texture::{Depth, TextureHeader, MAGIC, VERSION};

        // AssetHeader.
        if bytes.len() < psxed_format::AssetHeader::SIZE {
            return Err(ParseError::Truncated);
        }
        let magic = [bytes[0], bytes[1], bytes[2], bytes[3]];
        if magic != MAGIC {
            return Err(ParseError::WrongMagic);
        }
        let version = u16::from_le_bytes([bytes[4], bytes[5]]);
        if version != VERSION {
            return Err(ParseError::UnsupportedVersion(version));
        }
        let flags = u16::from_le_bytes([bytes[6], bytes[7]]);
        let payload_len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        let payload_start = psxed_format::AssetHeader::SIZE;
        let actual_payload = bytes.len().saturating_sub(payload_start);
        if (payload_len as usize) != actual_payload {
            return Err(ParseError::InvalidPayloadLen {
                declared: payload_len,
                actual: actual_payload,
            });
        }

        // TextureHeader.
        if actual_payload < TextureHeader::SIZE {
            return Err(ParseError::Truncated);
        }
        let th = &bytes[payload_start..];
        let depth = Depth::from_byte(th[0]).ok_or(ParseError::TableOverflow)?;
        // th[1] is _pad; skip.
        let width_px = u16::from_le_bytes([th[2], th[3]]);
        let height_px = u16::from_le_bytes([th[4], th[5]]);
        let clut_entries = u16::from_le_bytes([th[6], th[7]]);
        let pixel_bytes = u32::from_le_bytes([th[8], th[9], th[10], th[11]]) as usize;
        let clut_bytes = u32::from_le_bytes([th[12], th[13], th[14], th[15]]) as usize;

        let mut off = payload_start + TextureHeader::SIZE;
        if off + pixel_bytes > bytes.len() {
            return Err(ParseError::TableOverflow);
        }
        let pixel_data = &bytes[off..off + pixel_bytes];
        off += pixel_bytes;

        if off + clut_bytes > bytes.len() {
            return Err(ParseError::TableOverflow);
        }
        let clut_data = &bytes[off..off + clut_bytes];

        Ok(Self {
            pixel_data,
            clut_data,
            width_px,
            height_px,
            depth,
            clut_entries,
            flags,
        })
    }

    /// Width in texels.
    #[inline]
    pub fn width(&self) -> u16 {
        self.width_px
    }

    /// Height in texels.
    #[inline]
    pub fn height(&self) -> u16 {
        self.height_px
    }

    /// Colour depth.
    #[inline]
    pub fn depth(&self) -> psxed_format::texture::Depth {
        self.depth
    }

    /// Number of CLUT entries in the blob. Normal textures use 16
    /// for 4bpp, 256 for 8bpp, or 0 for 15bpp; specialized 4bpp
    /// assets may concatenate multiple 16-entry CLUT rows.
    #[inline]
    pub fn clut_entries(&self) -> u16 {
        self.clut_entries
    }

    /// Texture feature flags from the shared asset header.
    #[inline]
    pub fn flags(&self) -> u16 {
        self.flags
    }

    /// True when indexed palette entry 0 should remain transparent.
    #[inline]
    pub fn index_zero_transparent(&self) -> bool {
        self.flags & psxed_format::texture::flags::INDEX_ZERO_TRANSPARENT != 0
    }

    /// Raw packed pixel halfwords, as bytes. Suitable for
    /// halfword-level DMA upload; caller pairs this with a `VramRect`
    /// describing the *halfword footprint*, not the texel width.
    #[inline]
    pub fn pixel_bytes(&self) -> &'a [u8] {
        self.pixel_data
    }

    /// Raw CLUT halfwords, as bytes. Empty for 15bpp.
    #[inline]
    pub fn clut_bytes(&self) -> &'a [u8] {
        self.clut_data
    }

    /// Halfwords-per-row at this texture's depth (rows padded up to
    /// a full halfword). Needed when computing the VRAM rect for
    /// upload: VRAM measures in halfwords regardless of the texel
    /// depth the GPU will fetch them at.
    #[inline]
    pub fn halfwords_per_row(&self) -> u16 {
        psxed_format::texture::TextureHeader::halfwords_per_row(self.depth, self.width_px)
    }
}

/// A parsed `.psxw` grid-world backed by slices into the cooked blob.
///
/// This is the binary runtime format; engine code wraps it as
/// `psx_engine::RuntimeRoom`. It keeps parsing zero-copy and lets engine
/// code pull sectors and walls by value as needed.
#[derive(Copy, Clone, Debug)]
pub struct World<'a> {
    sectors: &'a [u8],
    walls: &'a [u8],
    horizontal_overrides: &'a [u8],
    surface_lights: &'a [u8],
    sector_record_size: usize,
    wall_record_size: usize,
    horizontal_override_record_size: usize,
    width: u16,
    depth: u16,
    sector_size: i32,
    material_count: u16,
    wall_count: u16,
    horizontal_override_count: u16,
    surface_light_count: u16,
    ambient_color: [u8; 3],
    flags: u8,
    static_vertex_lighting: bool,
}

/// Four PS1 UV coordinates for one world quad.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct WorldQuadUvs {
    corners: [(u8, u8); 4],
}

impl WorldQuadUvs {
    /// Build a quad UV record from face-corner coordinates.
    pub const fn new(corners: [(u8, u8); 4]) -> Self {
        Self { corners }
    }

    /// Return UVs as face-corner coordinates.
    pub const fn corners(self) -> [(u8, u8); 4] {
        self.corners
    }
}

/// Four RGB vertex colours for one world quad.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct WorldSurfaceLight {
    vertex_rgb: [[u8; 3]; 4],
}

impl WorldSurfaceLight {
    /// Full-bright neutral lighting for legacy/unlit rooms.
    pub const fn white() -> Self {
        Self {
            vertex_rgb: [[255, 255, 255]; 4],
        }
    }

    /// Build from per-corner RGB values.
    pub const fn new(vertex_rgb: [[u8; 3]; 4]) -> Self {
        Self { vertex_rgb }
    }

    /// Return RGB values in face-corner order.
    pub const fn vertex_rgb(self) -> [[u8; 3]; 4] {
        self.vertex_rgb
    }
}

const WORLD_V1_SECTOR_RECORD_SIZE: usize = 44;
const WORLD_V1_WALL_RECORD_SIZE: usize = 24;
const WORLD_V2_SECTOR_RECORD_SIZE: usize = 60;
const WORLD_V2_WALL_RECORD_SIZE: usize = 32;
const WORLD_V3_HEADER_SIZE: usize = 20;
const WORLD_V3_SECTOR_RECORD_SIZE: usize = 60;
const WORLD_V3_WALL_RECORD_SIZE: usize = 32;
const WORLD_V4_HORIZONTAL_OVERRIDE_RECORD_SIZE: usize = 24;

impl<'a> World<'a> {
    /// Parse a cooked `.psxw` blob.
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, ParseError> {
        use psxed_format::world::{
            WorldHeader, MAGIC, VERSION, VERSION_V1, VERSION_V2, VERSION_V3, VERSION_V4,
        };

        if bytes.len() < psxed_format::AssetHeader::SIZE {
            return Err(ParseError::Truncated);
        }
        let magic = [bytes[0], bytes[1], bytes[2], bytes[3]];
        if magic != MAGIC {
            return Err(ParseError::WrongMagic);
        }
        let version = u16::from_le_bytes([bytes[4], bytes[5]]);
        if version != VERSION
            && version != VERSION_V3
            && version != VERSION_V4
            && version != VERSION_V2
            && version != VERSION_V1
        {
            return Err(ParseError::UnsupportedVersion(version));
        }
        let (world_header_size, sector_record_size, wall_record_size) = match version {
            VERSION_V1 => (
                WORLD_V3_HEADER_SIZE,
                WORLD_V1_SECTOR_RECORD_SIZE,
                WORLD_V1_WALL_RECORD_SIZE,
            ),
            VERSION_V2 => (
                WORLD_V3_HEADER_SIZE,
                WORLD_V2_SECTOR_RECORD_SIZE,
                WORLD_V2_WALL_RECORD_SIZE,
            ),
            VERSION_V3 => (
                WORLD_V3_HEADER_SIZE,
                WORLD_V3_SECTOR_RECORD_SIZE,
                WORLD_V3_WALL_RECORD_SIZE,
            ),
            VERSION_V4 => (
                WorldHeader::SIZE,
                psxed_format::world::SectorRecord::SIZE,
                psxed_format::world::WallRecord::SIZE,
            ),
            _ => (
                WorldHeader::SIZE,
                psxed_format::world::SectorRecord::SIZE,
                psxed_format::world::WallRecord::SIZE,
            ),
        };
        let payload_len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        let payload_start = psxed_format::AssetHeader::SIZE;
        let actual_payload = bytes.len().saturating_sub(payload_start);
        if payload_len as usize != actual_payload {
            return Err(ParseError::InvalidPayloadLen {
                declared: payload_len,
                actual: actual_payload,
            });
        }
        if actual_payload < world_header_size {
            return Err(ParseError::Truncated);
        }

        let wh = &bytes[payload_start..payload_start + world_header_size];
        let width = read_u16(wh, 0);
        let depth = read_u16(wh, 2);
        let sector_size = read_i32(wh, 4);
        let sector_count = read_u16(wh, 8);
        let material_count = read_u16(wh, 10);
        let wall_count = read_u16(wh, 12);
        let ambient_color = [wh[14], wh[15], wh[16]];
        let flags = wh[17];
        let surface_light_count =
            if version == VERSION || version == VERSION_V4 || version == VERSION_V3 {
                read_u16(wh, 18)
            } else {
                0
            };
        let horizontal_override_count = if version == VERSION || version == VERSION_V4 {
            read_u16(wh, 20)
        } else {
            0
        };
        let static_vertex_lighting =
            (version == VERSION || version == VERSION_V4 || version == VERSION_V3)
                && flags & psxed_format::world::world_flags::STATIC_VERTEX_LIGHTING != 0;

        let expected_sectors = (width as usize)
            .checked_mul(depth as usize)
            .ok_or(ParseError::InvalidWorldLayout)?;
        if sector_count as usize != expected_sectors {
            return Err(ParseError::InvalidWorldLayout);
        }
        if static_vertex_lighting {
            let expected_surface_lights = sector_count
                .checked_mul(2)
                .and_then(|count| count.checked_add(wall_count))
                .ok_or(ParseError::InvalidWorldLayout)?;
            if surface_light_count != expected_surface_lights {
                return Err(ParseError::InvalidWorldLayout);
            }
        } else if surface_light_count != 0 {
            return Err(ParseError::InvalidWorldLayout);
        }

        let mut off = payload_start + world_header_size;
        let sector_bytes = (sector_count as usize)
            .checked_mul(sector_record_size)
            .ok_or(ParseError::TableOverflow)?;
        if off + sector_bytes > bytes.len() {
            return Err(ParseError::TableOverflow);
        }
        let sectors = &bytes[off..off + sector_bytes];
        off += sector_bytes;

        let wall_bytes = (wall_count as usize)
            .checked_mul(wall_record_size)
            .ok_or(ParseError::TableOverflow)?;
        if off + wall_bytes > bytes.len() {
            return Err(ParseError::TableOverflow);
        }
        let walls = &bytes[off..off + wall_bytes];
        off += wall_bytes;
        let horizontal_override_record_size = if version == VERSION_V4 {
            WORLD_V4_HORIZONTAL_OVERRIDE_RECORD_SIZE
        } else {
            psxed_format::world::HorizontalOverrideRecord::SIZE
        };
        let horizontal_override_bytes = (horizontal_override_count as usize)
            .checked_mul(horizontal_override_record_size)
            .ok_or(ParseError::TableOverflow)?;
        if off + horizontal_override_bytes > bytes.len() {
            return Err(ParseError::TableOverflow);
        }
        let horizontal_overrides = &bytes[off..off + horizontal_override_bytes];
        off += horizontal_override_bytes;
        let surface_light_bytes = (surface_light_count as usize)
            .checked_mul(psxed_format::world::SurfaceLightRecord::SIZE)
            .ok_or(ParseError::TableOverflow)?;
        if off + surface_light_bytes > bytes.len() {
            return Err(ParseError::TableOverflow);
        }
        let surface_lights = &bytes[off..off + surface_light_bytes];
        off += surface_light_bytes;
        if off != bytes.len() {
            return Err(ParseError::InvalidWorldLayout);
        }

        validate_sector_wall_ranges(sectors, sector_record_size, wall_count)?;
        validate_horizontal_overrides(
            horizontal_overrides,
            horizontal_override_count,
            sector_count,
            horizontal_override_record_size,
        )?;

        Ok(Self {
            sectors,
            walls,
            horizontal_overrides,
            surface_lights,
            sector_record_size,
            wall_record_size,
            horizontal_override_record_size,
            width,
            depth,
            sector_size,
            material_count,
            wall_count,
            horizontal_override_count,
            surface_light_count,
            ambient_color,
            flags,
            static_vertex_lighting,
        })
    }

    /// Width in grid sectors.
    #[inline]
    pub fn width(&self) -> u16 {
        self.width
    }

    /// Depth in grid sectors.
    #[inline]
    pub fn depth(&self) -> u16 {
        self.depth
    }

    /// Engine units per sector.
    #[inline]
    pub fn sector_size(&self) -> i32 {
        self.sector_size
    }

    /// Number of material slots referenced by the world.
    #[inline]
    pub fn material_count(&self) -> u16 {
        self.material_count
    }

    /// Number of wall records in the world.
    #[inline]
    pub fn wall_count(&self) -> u16 {
        self.wall_count
    }

    /// Number of horizontal override records in the world.
    #[inline]
    pub fn horizontal_override_count(&self) -> u16 {
        self.horizontal_override_count
    }

    /// Number of appended static surface-light records.
    #[inline]
    pub fn surface_light_count(&self) -> u16 {
        self.surface_light_count
    }

    /// Ambient RGB color.
    #[inline]
    pub fn ambient_color(&self) -> [u8; 3] {
        self.ambient_color
    }

    /// Whether fog/depth cue is enabled for this world.
    #[inline]
    pub fn fog_enabled(&self) -> bool {
        self.flags & psxed_format::world::world_flags::FOG_ENABLED != 0
    }

    /// Whether face records carry baked static vertex lighting.
    #[inline]
    pub fn static_vertex_lighting(&self) -> bool {
        self.static_vertex_lighting
    }

    /// Sector at a coordinate, returning `None` for empty cells or out of range.
    pub fn sector(&self, x: u16, z: u16) -> Option<WorldSector> {
        if x >= self.width || z >= self.depth {
            return None;
        }
        let index = x as usize * self.depth as usize + z as usize;
        let sector = self.sector_record(index)?;
        if sector.has_geometry() {
            Some(sector)
        } else {
            None
        }
    }

    /// Sector at a coordinate without applying horizontal override
    /// side-table records. Collision and wall-only queries use this
    /// to avoid scanning render-only per-triangle override data.
    pub fn sector_without_horizontal_overrides(&self, x: u16, z: u16) -> Option<WorldSector> {
        if x >= self.width || z >= self.depth {
            return None;
        }
        let index = x as usize * self.depth as usize + z as usize;
        let sector = self.sector_record_without_horizontal_overrides(index)?;
        if sector.has_geometry() {
            Some(sector)
        } else {
            None
        }
    }

    /// Minimal collision-probe sector header at a coordinate. This
    /// skips horizontal override decoding and only exposes floor
    /// presence plus the wall range.
    pub fn sector_collision_probe(&self, x: u16, z: u16) -> Option<WorldSectorCollisionProbe> {
        if x >= self.width || z >= self.depth {
            return None;
        }
        let index = x as usize * self.depth as usize + z as usize;
        let sector = self.sector_collision_probe_record(index)?;
        if sector.has_geometry() {
            Some(sector)
        } else {
            None
        }
    }

    /// Decode only the selected floor triangle needed by a collision query.
    ///
    /// Unlike [`Self::sector`], this does not materialize render materials,
    /// UVs, ceiling data, or the unselected triangle.
    pub fn sector_floor_collision(
        &self,
        x: u16,
        z: u16,
        local_x: i32,
        local_z: i32,
        sector_size: i32,
    ) -> Option<WorldSectorFloorCollision> {
        if x >= self.width || z >= self.depth || sector_size <= 0 {
            return None;
        }
        let index = x as usize * self.depth as usize + z as usize;
        let size = self.sector_record_size;
        let base = index.checked_mul(size)?;
        let end = base.checked_add(size)?;
        let bytes = self.sectors.get(base..end)?;
        let flags = bytes[0];
        if flags & psxed_format::world::sector_flags::HAS_FLOOR == 0 {
            return None;
        }

        let split = bytes[1];
        let triangle =
            world_topology::horizontal_triangle_at_local(split, local_x, local_z, sector_size);
        let default_flags = default_horizontal_flags(
            true,
            flags & psxed_format::world::sector_flags::FLOOR_WALKABLE != 0,
        );
        let sector_index = u16::try_from(index).ok()?;
        let (triangle_flags, override_heights) = self
            .horizontal_collision_override(
                sector_index,
                psxed_format::world::horizontal_surface::FLOOR,
                triangle,
            )
            .unwrap_or((default_flags, None));
        if !horizontal_triangle_present(triangle_flags, triangle) {
            return None;
        }

        let floor_heights = read_i32x4(bytes, 12);
        let triangle_heights = override_heights
            .unwrap_or_else(|| horizontal_triangle_heights(floor_heights, split, triangle));
        Some(WorldSectorFloorCollision {
            split,
            triangle: triangle as u8,
            walkable: horizontal_triangle_walkable(triangle_flags, triangle),
            floor_heights,
            triangle_heights,
        })
    }

    /// Sector record by flat `[x * depth + z]` index, including empty cells.
    pub fn sector_record(&self, index: usize) -> Option<WorldSector> {
        let size = self.sector_record_size;
        let base = index.checked_mul(size)?;
        let end = base.checked_add(size)?;
        let bytes = self.sectors.get(base..end)?;
        let sector_index = if index <= u16::MAX as usize {
            index as u16
        } else {
            return None;
        };
        let floor_override =
            self.horizontal_override(sector_index, psxed_format::world::horizontal_surface::FLOOR);
        let ceiling_override = self.horizontal_override(
            sector_index,
            psxed_format::world::horizontal_surface::CEILING,
        );
        Some(WorldSector::decode(bytes, floor_override, ceiling_override))
    }

    fn sector_record_without_horizontal_overrides(&self, index: usize) -> Option<WorldSector> {
        let size = self.sector_record_size;
        let base = index.checked_mul(size)?;
        let end = base.checked_add(size)?;
        let bytes = self.sectors.get(base..end)?;
        if index > u16::MAX as usize {
            return None;
        }
        Some(WorldSector::decode(bytes, None, None))
    }

    fn sector_collision_probe_record(&self, index: usize) -> Option<WorldSectorCollisionProbe> {
        let size = self.sector_record_size;
        let base = index.checked_mul(size)?;
        let end = base.checked_add(size)?;
        let bytes = self.sectors.get(base..end)?;
        if index > u16::MAX as usize {
            return None;
        }
        Some(WorldSectorCollisionProbe {
            flags: bytes[0],
            first_wall: read_u16(bytes, 8),
            wall_count: read_u16(bytes, 10),
        })
    }

    /// Wall record by global wall index.
    pub fn wall(&self, index: u16) -> Option<WorldWall> {
        if index >= self.wall_count {
            return None;
        }
        let size = self.wall_record_size;
        let base = index as usize * size;
        let end = base.checked_add(size)?;
        let bytes = self.walls.get(base..end)?;
        Some(WorldWall::decode(bytes))
    }

    /// Wall record by sector-local wall index.
    pub fn sector_wall(&self, sector: WorldSector, local_index: u16) -> Option<WorldWall> {
        if local_index >= sector.wall_count {
            return None;
        }
        self.wall(sector.first_wall.checked_add(local_index)?)
    }

    /// Static surface-light record by direct table index.
    pub fn surface_light(&self, index: u16) -> Option<WorldSurfaceLight> {
        if !self.static_vertex_lighting || index >= self.surface_light_count {
            return None;
        }
        let size = psxed_format::world::SurfaceLightRecord::SIZE;
        let base = index as usize * size;
        let end = base.checked_add(size)?;
        let bytes = self.surface_lights.get(base..end)?;
        Some(read_world_surface_light(bytes, 0))
    }

    fn horizontal_override(
        &self,
        sector_index: u16,
        surface: u8,
    ) -> Option<WorldHorizontalOverride> {
        let size = self.horizontal_override_record_size;
        let mut index = 0usize;
        while index < self.horizontal_override_count as usize {
            let base = index.checked_mul(size)?;
            let bytes = self.horizontal_overrides.get(base..base + size)?;
            if read_u16(bytes, 0) == sector_index && bytes[2] == surface {
                return Some(read_world_horizontal_override(bytes));
            }
            index += 1;
        }
        None
    }

    fn horizontal_collision_override(
        &self,
        sector_index: u16,
        surface: u8,
        triangle: usize,
    ) -> Option<(u8, Option<[i32; 3]>)> {
        let size = self.horizontal_override_record_size;
        let mut index = 0usize;
        while index < self.horizontal_override_count as usize {
            let base = index.checked_mul(size)?;
            let bytes = self.horizontal_overrides.get(base..base + size)?;
            if read_u16(bytes, 0) == sector_index && bytes[2] == surface {
                let heights = (bytes.len() >= psxed_format::world::HorizontalOverrideRecord::SIZE)
                    .then(|| {
                        let offset = 24 + triangle.min(1) * 12;
                        [
                            read_i32(bytes, offset),
                            read_i32(bytes, offset + 4),
                            read_i32(bytes, offset + 8),
                        ]
                    });
                return Some((bytes[3], heights));
            }
            index += 1;
        }
        None
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct WorldHorizontalOverride {
    flags: u8,
    materials: [u16; 2],
    uvs: [WorldQuadUvs; 2],
    heights: Option<[[i32; 3]; 2]>,
}

/// Minimal sector data for camera/wall collision probes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct WorldSectorCollisionProbe {
    flags: u8,
    first_wall: u16,
    wall_count: u16,
}

/// Minimal decoded floor triangle used by character and camera collision.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct WorldSectorFloorCollision {
    split: u8,
    triangle: u8,
    walkable: bool,
    floor_heights: [i32; 4],
    triangle_heights: [i32; 3],
}

impl WorldSectorFloorCollision {
    /// Floor diagonal split id.
    #[inline]
    pub fn split(self) -> u8 {
        self.split
    }

    /// Selected triangle index.
    #[inline]
    pub fn triangle(self) -> usize {
        self.triangle as usize
    }

    /// Whether the selected floor triangle is walkable.
    #[inline]
    pub fn walkable(self) -> bool {
        self.walkable
    }

    /// Floor corner heights `[NW, NE, SE, SW]`.
    #[inline]
    pub fn floor_heights(self) -> [i32; 4] {
        self.floor_heights
    }

    /// Selected triangle heights in triangle-corner order.
    #[inline]
    pub fn triangle_heights(self) -> [i32; 3] {
        self.triangle_heights
    }
}

impl WorldSectorCollisionProbe {
    /// True if this sector contains any floor, ceiling, or wall data.
    #[inline]
    pub fn has_geometry(self) -> bool {
        self.has_floor() || self.has_ceiling() || self.wall_count != 0
    }

    /// True if this sector has a floor surface.
    #[inline]
    pub fn has_floor(self) -> bool {
        self.flags & psxed_format::world::sector_flags::HAS_FLOOR != 0
    }

    /// True if this sector has a ceiling surface.
    #[inline]
    pub fn has_ceiling(self) -> bool {
        self.flags & psxed_format::world::sector_flags::HAS_CEILING != 0
    }

    /// First global wall index for this sector.
    #[inline]
    pub fn first_wall(self) -> u16 {
        self.first_wall
    }

    /// Number of walls belonging to this sector.
    #[inline]
    pub fn wall_count(self) -> u16 {
        self.wall_count
    }
}

/// One decoded world sector record.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct WorldSector {
    flags: u8,
    floor_split: u8,
    ceiling_split: u8,
    floor_material: u16,
    ceiling_material: u16,
    floor_triangle_flags: u8,
    ceiling_triangle_flags: u8,
    floor_triangle_materials: [u16; 2],
    ceiling_triangle_materials: [u16; 2],
    floor_triangle_heights: [[i32; 3]; 2],
    ceiling_triangle_heights: [[i32; 3]; 2],
    first_wall: u16,
    wall_count: u16,
    floor_heights: [i32; 4],
    ceiling_heights: [i32; 4],
    floor_uvs: WorldQuadUvs,
    ceiling_uvs: WorldQuadUvs,
    floor_triangle_uvs: [WorldQuadUvs; 2],
    ceiling_triangle_uvs: [WorldQuadUvs; 2],
}

impl WorldSector {
    fn decode(
        bytes: &[u8],
        floor_override: Option<WorldHorizontalOverride>,
        ceiling_override: Option<WorldHorizontalOverride>,
    ) -> Self {
        let has_v2_uvs = bytes.len() >= WORLD_V2_SECTOR_RECORD_SIZE;
        let flags = bytes[0];
        let floor_material = read_u16(bytes, 4);
        let ceiling_material = read_u16(bytes, 6);
        let floor_heights = read_i32x4(bytes, 12);
        let ceiling_heights = read_i32x4(bytes, 28);
        let floor_uvs = if has_v2_uvs {
            read_world_uvs(bytes, 44)
        } else {
            WorldQuadUvs::new(psxed_format::world::FLOOR_UVS)
        };
        let ceiling_uvs = if has_v2_uvs {
            read_world_uvs(bytes, 52)
        } else {
            WorldQuadUvs::new(psxed_format::world::FLOOR_UVS)
        };
        let floor_default_flags = default_horizontal_flags(
            flags & psxed_format::world::sector_flags::HAS_FLOOR != 0,
            flags & psxed_format::world::sector_flags::FLOOR_WALKABLE != 0,
        );
        let ceiling_default_flags = default_horizontal_flags(
            flags & psxed_format::world::sector_flags::HAS_CEILING != 0,
            flags & psxed_format::world::sector_flags::CEILING_WALKABLE != 0,
        );
        let floor_triangle_flags =
            floor_override.map_or(floor_default_flags, |override_data| override_data.flags);
        let ceiling_triangle_flags =
            ceiling_override.map_or(ceiling_default_flags, |override_data| override_data.flags);
        let floor_triangle_materials = floor_override
            .map_or([floor_material, floor_material], |override_data| {
                override_data.materials
            });
        let ceiling_triangle_materials = ceiling_override
            .map_or([ceiling_material, ceiling_material], |override_data| {
                override_data.materials
            });
        let floor_triangle_uvs =
            floor_override.map_or([floor_uvs, floor_uvs], |override_data| override_data.uvs);
        let ceiling_triangle_uvs = ceiling_override
            .map_or([ceiling_uvs, ceiling_uvs], |override_data| {
                override_data.uvs
            });
        let floor_triangle_heights = floor_override
            .and_then(|override_data| override_data.heights)
            .unwrap_or_else(|| {
                [
                    horizontal_triangle_heights(floor_heights, bytes[1], 0),
                    horizontal_triangle_heights(floor_heights, bytes[1], 1),
                ]
            });
        let ceiling_triangle_heights = ceiling_override
            .and_then(|override_data| override_data.heights)
            .unwrap_or_else(|| {
                [
                    horizontal_triangle_heights(ceiling_heights, bytes[2], 0),
                    horizontal_triangle_heights(ceiling_heights, bytes[2], 1),
                ]
            });
        Self {
            flags,
            floor_split: bytes[1],
            ceiling_split: bytes[2],
            floor_material,
            ceiling_material,
            floor_triangle_flags,
            ceiling_triangle_flags,
            floor_triangle_materials,
            ceiling_triangle_materials,
            floor_triangle_heights,
            ceiling_triangle_heights,
            first_wall: read_u16(bytes, 8),
            wall_count: read_u16(bytes, 10),
            floor_heights,
            ceiling_heights,
            floor_uvs,
            ceiling_uvs,
            floor_triangle_uvs,
            ceiling_triangle_uvs,
        }
    }

    /// True if this sector has any floor, ceiling, or wall geometry.
    #[inline]
    pub fn has_geometry(&self) -> bool {
        self.has_floor() || self.has_ceiling() || self.wall_count != 0
    }

    /// True if this sector has a floor face.
    #[inline]
    pub fn has_floor(&self) -> bool {
        self.flags & psxed_format::world::sector_flags::HAS_FLOOR != 0
            && (self.floor_triangle_present(0) || self.floor_triangle_present(1))
    }

    /// True if this sector has a ceiling face.
    #[inline]
    pub fn has_ceiling(&self) -> bool {
        self.flags & psxed_format::world::sector_flags::HAS_CEILING != 0
            && (self.ceiling_triangle_present(0) || self.ceiling_triangle_present(1))
    }

    /// True if the floor face is walkable.
    #[inline]
    pub fn floor_walkable(&self) -> bool {
        self.floor_triangle_walkable(0) || self.floor_triangle_walkable(1)
    }

    /// Floor diagonal split id.
    #[inline]
    pub fn floor_split(&self) -> u8 {
        self.floor_split
    }

    /// Floor material slot.
    #[inline]
    pub fn floor_material(&self) -> Option<u16> {
        self.floor_triangle_material(0)
            .or_else(|| self.floor_triangle_material(1))
            .or_else(|| material_or_none(self.floor_material))
    }

    /// Floor corner heights `[NW, NE, SE, SW]`.
    #[inline]
    pub fn floor_heights(&self) -> [i32; 4] {
        self.floor_heights
    }

    /// Floor UVs `[NW, NE, SE, SW]`.
    #[inline]
    pub fn floor_uvs(&self) -> WorldQuadUvs {
        self.floor_uvs
    }

    /// `true` if a floor split triangle is present.
    #[inline]
    pub fn floor_triangle_present(&self, index: usize) -> bool {
        horizontal_triangle_present(self.floor_triangle_flags, index)
    }

    /// Floor split-triangle material slot.
    #[inline]
    pub fn floor_triangle_material(&self, index: usize) -> Option<u16> {
        if self.floor_triangle_present(index) {
            material_or_none(self.floor_triangle_materials[index.min(1)])
        } else {
            None
        }
    }

    /// Floor split-triangle UVs in face-corner order.
    #[inline]
    pub fn floor_triangle_uvs(&self, index: usize) -> WorldQuadUvs {
        self.floor_triangle_uvs[index.min(1)]
    }

    /// Floor split-triangle heights in that triangle's corner order.
    #[inline]
    pub fn floor_triangle_heights(&self, index: usize) -> [i32; 3] {
        self.floor_triangle_heights[index.min(1)]
    }

    /// `true` if a floor split triangle is walkable.
    #[inline]
    pub fn floor_triangle_walkable(&self, index: usize) -> bool {
        self.floor_triangle_present(index)
            && horizontal_triangle_walkable(self.floor_triangle_flags, index)
    }

    /// Ceiling diagonal split id.
    #[inline]
    pub fn ceiling_split(&self) -> u8 {
        self.ceiling_split
    }

    /// Ceiling material slot.
    #[inline]
    pub fn ceiling_material(&self) -> Option<u16> {
        self.ceiling_triangle_material(0)
            .or_else(|| self.ceiling_triangle_material(1))
            .or_else(|| material_or_none(self.ceiling_material))
    }

    /// Ceiling corner heights `[NW, NE, SE, SW]`.
    #[inline]
    pub fn ceiling_heights(&self) -> [i32; 4] {
        self.ceiling_heights
    }

    /// Ceiling UVs `[NW, NE, SE, SW]`.
    #[inline]
    pub fn ceiling_uvs(&self) -> WorldQuadUvs {
        self.ceiling_uvs
    }

    /// `true` if a ceiling split triangle is present.
    #[inline]
    pub fn ceiling_triangle_present(&self, index: usize) -> bool {
        horizontal_triangle_present(self.ceiling_triangle_flags, index)
    }

    /// Ceiling split-triangle material slot.
    #[inline]
    pub fn ceiling_triangle_material(&self, index: usize) -> Option<u16> {
        if self.ceiling_triangle_present(index) {
            material_or_none(self.ceiling_triangle_materials[index.min(1)])
        } else {
            None
        }
    }

    /// Ceiling split-triangle UVs in face-corner order.
    #[inline]
    pub fn ceiling_triangle_uvs(&self, index: usize) -> WorldQuadUvs {
        self.ceiling_triangle_uvs[index.min(1)]
    }

    /// Ceiling split-triangle heights in that triangle's corner order.
    #[inline]
    pub fn ceiling_triangle_heights(&self, index: usize) -> [i32; 3] {
        self.ceiling_triangle_heights[index.min(1)]
    }

    /// `true` if a ceiling split triangle is walkable.
    #[inline]
    pub fn ceiling_triangle_walkable(&self, index: usize) -> bool {
        self.ceiling_triangle_present(index)
            && horizontal_triangle_walkable(self.ceiling_triangle_flags, index)
    }

    /// First global wall index for this sector.
    #[inline]
    pub fn first_wall(&self) -> u16 {
        self.first_wall
    }

    /// Number of walls belonging to this sector.
    #[inline]
    pub fn wall_count(&self) -> u16 {
        self.wall_count
    }
}

/// One decoded world wall record.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct WorldWall {
    direction: u8,
    flags: u8,
    material: u16,
    shape: u16,
    heights: [i32; 4],
    uvs: WorldQuadUvs,
}

impl WorldWall {
    fn decode(bytes: &[u8]) -> Self {
        let has_v2_uvs = bytes.len() >= WORLD_V2_WALL_RECORD_SIZE;
        Self {
            direction: bytes[0],
            flags: bytes[1],
            material: read_u16(bytes, 4),
            shape: if has_v2_uvs {
                read_u16(bytes, 6)
            } else {
                psxed_format::world::wall_shape::QUAD
            },
            heights: read_i32x4(bytes, 8),
            uvs: if has_v2_uvs {
                read_world_uvs(bytes, 24)
            } else {
                WorldQuadUvs::new(psxed_format::world::WALL_UVS)
            },
        }
    }

    /// Direction id, see `psxed_format::world::direction`.
    #[inline]
    pub fn direction(&self) -> u8 {
        self.direction
    }

    /// Whether this wall blocks collision.
    #[inline]
    pub fn solid(&self) -> bool {
        self.flags & psxed_format::world::wall_flags::SOLID != 0
    }

    /// Material slot.
    #[inline]
    pub fn material(&self) -> u16 {
        self.material
    }

    /// Wall shape id, see `psxed_format::world::wall_shape`.
    #[inline]
    pub fn shape(&self) -> u16 {
        self.shape
    }

    /// Wall heights `[bottom-left, bottom-right, top-right, top-left]`.
    #[inline]
    pub fn heights(&self) -> [i32; 4] {
        self.heights
    }

    /// Wall UVs `[bottom-left, bottom-right, top-right, top-left]`.
    #[inline]
    pub fn uvs(&self) -> WorldQuadUvs {
        self.uvs
    }
}

#[inline]
fn material_or_none(material: u16) -> Option<u16> {
    if material == psxed_format::world::NO_MATERIAL {
        None
    } else {
        Some(material)
    }
}

const fn default_horizontal_flags(present: bool, walkable: bool) -> u8 {
    if !present {
        0
    } else {
        let mut flags = psxed_format::world::horizontal_flags::TRI_A_PRESENT
            | psxed_format::world::horizontal_flags::TRI_B_PRESENT;
        if walkable {
            flags |= psxed_format::world::horizontal_flags::TRI_A_WALKABLE
                | psxed_format::world::horizontal_flags::TRI_B_WALKABLE;
        }
        flags
    }
}

const fn horizontal_triangle_present(flags: u8, index: usize) -> bool {
    let bit = if index == 0 {
        psxed_format::world::horizontal_flags::TRI_A_PRESENT
    } else {
        psxed_format::world::horizontal_flags::TRI_B_PRESENT
    };
    flags & bit != 0
}

const fn horizontal_triangle_walkable(flags: u8, index: usize) -> bool {
    let bit = if index == 0 {
        psxed_format::world::horizontal_flags::TRI_A_WALKABLE
    } else {
        psxed_format::world::horizontal_flags::TRI_B_WALKABLE
    };
    flags & bit != 0
}

fn horizontal_triangle_heights(heights: [i32; 4], split: u8, index: usize) -> [i32; 3] {
    let corners = psxed_format::world::topology::split_triangle(split, index);
    [
        heights[corners[0]],
        heights[corners[1]],
        heights[corners[2]],
    ]
}

fn checked_table_bytes(count: u16, stride: usize) -> Result<usize, ParseError> {
    (count as usize)
        .checked_mul(stride)
        .ok_or(ParseError::TableOverflow)
}

fn take_table<'a>(bytes: &'a [u8], offset: &mut usize, len: usize) -> Result<&'a [u8], ParseError> {
    let end = offset.checked_add(len).ok_or(ParseError::TableOverflow)?;
    if end > bytes.len() {
        return Err(ParseError::TableOverflow);
    }
    let table = &bytes[*offset..end];
    *offset = end;
    Ok(table)
}

fn validate_model_parts(
    parts: &[u8],
    joint_count: u16,
    material_count: u16,
    vertex_count: u16,
    face_count: u16,
) -> Result<(), ParseError> {
    let size = psxed_format::model::PartRecord::SIZE;
    let count = parts.len() / size;
    for index in 0..count {
        let base = index * size;
        let joint_index = read_u16(parts, base);
        let first_vertex = read_u16(parts, base + 2);
        let part_vertex_count = read_u16(parts, base + 4);
        let first_face = read_u16(parts, base + 6);
        let part_face_count = read_u16(parts, base + 8);
        let material_index = read_u16(parts, base + 10);

        if joint_index != psxed_format::model::NO_JOINT && joint_index >= joint_count {
            return Err(ParseError::InvalidModelLayout);
        }
        if material_index >= material_count {
            return Err(ParseError::InvalidModelLayout);
        }
        let Some(vertex_end) = first_vertex.checked_add(part_vertex_count) else {
            return Err(ParseError::InvalidModelLayout);
        };
        if vertex_end > vertex_count {
            return Err(ParseError::InvalidModelLayout);
        }
        let Some(face_end) = first_face.checked_add(part_face_count) else {
            return Err(ParseError::InvalidModelLayout);
        };
        if face_end > face_count {
            return Err(ParseError::InvalidModelLayout);
        }
    }
    Ok(())
}

fn validate_model_faces(faces: &[u8], vertex_count: u16) -> Result<(), ParseError> {
    let size = psxed_format::model::FACE_RECORD_SIZE;
    let count = faces.len() / size;
    for index in 0..count {
        let base = index * size;
        let a = read_u16(faces, base);
        let b = read_u16(faces, base + 4);
        let c = read_u16(faces, base + 8);
        if a >= vertex_count || b >= vertex_count || c >= vertex_count {
            return Err(ParseError::InvalidModelLayout);
        }
    }
    Ok(())
}

#[inline]
fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

#[inline]
fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

#[inline]
fn read_i16(bytes: &[u8], offset: usize) -> i16 {
    i16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

#[inline]
unsafe fn read_i16_unchecked(bytes: &[u8], offset: usize) -> i16 {
    i16::from_le_bytes([unsafe { *bytes.get_unchecked(offset) }, unsafe {
        *bytes.get_unchecked(offset + 1)
    }])
}

#[inline]
unsafe fn read_i16_aligned_unchecked(bytes: &[u8], offset: usize) -> i16 {
    debug_assert_eq!((unsafe { bytes.as_ptr().add(offset) } as usize) & 1, 0);
    i16::from_le(unsafe { bytes.as_ptr().add(offset).cast::<i16>().read() })
}

#[inline]
fn read_i32(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

#[inline]
unsafe fn read_i32_unchecked(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes([
        unsafe { *bytes.get_unchecked(offset) },
        unsafe { *bytes.get_unchecked(offset + 1) },
        unsafe { *bytes.get_unchecked(offset + 2) },
        unsafe { *bytes.get_unchecked(offset + 3) },
    ])
}

#[inline]
fn read_i32x4(bytes: &[u8], offset: usize) -> [i32; 4] {
    [
        read_i32(bytes, offset),
        read_i32(bytes, offset + 4),
        read_i32(bytes, offset + 8),
        read_i32(bytes, offset + 12),
    ]
}

#[inline]
fn read_world_uvs(bytes: &[u8], offset: usize) -> WorldQuadUvs {
    WorldQuadUvs::new([
        (bytes[offset], bytes[offset + 1]),
        (bytes[offset + 2], bytes[offset + 3]),
        (bytes[offset + 4], bytes[offset + 5]),
        (bytes[offset + 6], bytes[offset + 7]),
    ])
}

#[inline]
fn read_world_horizontal_override(bytes: &[u8]) -> WorldHorizontalOverride {
    WorldHorizontalOverride {
        flags: bytes[3],
        materials: [read_u16(bytes, 4), read_u16(bytes, 6)],
        uvs: [read_world_uvs(bytes, 8), read_world_uvs(bytes, 16)],
        heights: (bytes.len() >= psxed_format::world::HorizontalOverrideRecord::SIZE).then(|| {
            [
                [
                    read_i32(bytes, 24),
                    read_i32(bytes, 28),
                    read_i32(bytes, 32),
                ],
                [
                    read_i32(bytes, 36),
                    read_i32(bytes, 40),
                    read_i32(bytes, 44),
                ],
            ]
        }),
    }
}

#[inline]
fn read_world_surface_light(bytes: &[u8], offset: usize) -> WorldSurfaceLight {
    WorldSurfaceLight::new([
        [bytes[offset], bytes[offset + 1], bytes[offset + 2]],
        [bytes[offset + 3], bytes[offset + 4], bytes[offset + 5]],
        [bytes[offset + 6], bytes[offset + 7], bytes[offset + 8]],
        [bytes[offset + 9], bytes[offset + 10], bytes[offset + 11]],
    ])
}

fn validate_horizontal_overrides(
    overrides: &[u8],
    override_count: u16,
    sector_count: u16,
    record_size: usize,
) -> Result<(), ParseError> {
    let size = record_size;
    if overrides.len() != override_count as usize * size {
        return Err(ParseError::InvalidWorldLayout);
    }
    for index in 0..override_count as usize {
        let base = index * size;
        let sector_index = read_u16(overrides, base);
        let surface = overrides[base + 2];
        if sector_index >= sector_count {
            return Err(ParseError::InvalidWorldLayout);
        }
        if surface != psxed_format::world::horizontal_surface::FLOOR
            && surface != psxed_format::world::horizontal_surface::CEILING
        {
            return Err(ParseError::InvalidWorldLayout);
        }
    }
    Ok(())
}

fn validate_sector_wall_ranges(
    sectors: &[u8],
    sector_record_size: usize,
    wall_count: u16,
) -> Result<(), ParseError> {
    let size = sector_record_size;
    let count = sectors.len() / size;
    for index in 0..count {
        let base = index * size;
        let first_wall = read_u16(sectors, base + 8);
        let sector_wall_count = read_u16(sectors, base + 10);
        let Some(end) = first_wall.checked_add(sector_wall_count) else {
            return Err(ParseError::InvalidWorldLayout);
        };
        if end > wall_count {
            return Err(ParseError::InvalidWorldLayout);
        }
    }
    Ok(())
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;
