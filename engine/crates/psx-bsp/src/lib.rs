#![cfg_attr(target_arch = "mips", feature(asm_experimental_arch))]
#![no_std]

//! Checked, allocation-free readers for the XBSP cooked map format.
//!
//! Shared by the guest runtime, the editor cook and quake-psx so the wire
//! format has one Rust definition (docs/quake-bsp-migration-plan.md, P0).
//! Derived from quake-psx `crates/quake-formats` (GPL-2, same authorship;
//! synced to its commit 83a6349); the PXBSP extensions (material lump,
//! PSoXide entity records, streaming index) will grow here.
// ponytail: verbatim-copy provenance; field docs land as PXBSP diverges,
// then this allow goes away.
#![allow(missing_docs)]

extern crate alloc;

pub mod collision;
pub mod collision_provider;
pub mod destructible;
pub mod mover;
pub mod pxbsp;
pub mod pxbsp_resident;
pub mod render;
pub mod resident;
pub mod sky;
pub mod toolchain_probe;

use core::fmt;
use core::marker::PhantomData;

/// `PSX%` as emitted by the original Quake PSX map cooker.
pub const PSB_MAGIC: u32 = 0x2558_5350;
/// `PSX2` in little-endian byte order: unsupported experimental compact records.
///
/// This value is retained so callers can identify and reject the short-lived
/// Plane10 layout without ever interpreting it as the final compact format.
pub const PSB2_MAGIC: u32 = 0x3258_5350;
/// `PSX3` in little-endian byte order: the first compact structural records
/// (retained only to reject old files by name).
pub const PSB3_MAGIC: u32 = 0x3358_5350;
/// `PSX4` in little-endian byte order: compact records with indexed corners
/// but eight-bit leaf mark counts and no brush-model origin (retained only to
/// reject old files by name).
pub const PSB4_MAGIC: u32 = 0x3458_5350;
/// `PSX5` in little-endian byte order: compact records with indexed corners,
/// sixteen-bit leaf mark counts, wide visibility offsets and brush-model
/// origins, so one record set serves Quake's cooked maps and the editor's
/// brush worlds alike.
pub const PSB5_MAGIC: u32 = 0x3558_5350;
pub const PSB_HEADER_BYTES: u32 = 4;
pub const LUMP_HEADER_BYTES: u32 = 8;
pub const LUMP_COUNT: usize = 15;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PsbVersion {
    LegacyV1,
    IndexedV5,
}

impl PsbVersion {
    pub const fn magic(self) -> u32 {
        match self {
            Self::LegacyV1 => PSB_MAGIC,
            Self::IndexedV5 => PSB5_MAGIC,
        }
    }

    const fn from_magic(magic: u32) -> Option<Self> {
        match magic {
            PSB_MAGIC => Some(Self::LegacyV1),
            PSB5_MAGIC => Some(Self::IndexedV5),
            _ => None,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum LumpKind {
    TextureData = 0,
    SoundData = 1,
    ModelData = 2,
    Vertices = 3,
    Planes = 4,
    TextureInfo = 5,
    Faces = 6,
    MarkSurfaces = 7,
    Visibility = 8,
    Leaves = 9,
    Nodes = 10,
    ClipNodes = 11,
    Models = 12,
    Strings = 13,
    Entities = 14,
}

impl LumpKind {
    pub const ALL: [Self; LUMP_COUNT] = [
        Self::TextureData,
        Self::SoundData,
        Self::ModelData,
        Self::Vertices,
        Self::Planes,
        Self::TextureInfo,
        Self::Faces,
        Self::MarkSurfaces,
        Self::Visibility,
        Self::Leaves,
        Self::Nodes,
        Self::ClipNodes,
        Self::Models,
        Self::Strings,
        Self::Entities,
    ];

    pub const fn record_size(self, version: PsbVersion) -> Option<u32> {
        match self {
            Self::TextureData | Self::SoundData | Self::ModelData | Self::Visibility => None,
            Self::Vertices => match version {
                PsbVersion::IndexedV5 => None,
                _ => Some(12),
            },
            Self::Planes => Some(match version {
                PsbVersion::LegacyV1 => 14,
                PsbVersion::IndexedV5 => Plane::SIZE as u32,
            }),
            Self::TextureInfo => Some(14),
            Self::Faces => Some(match version {
                PsbVersion::LegacyV1 => 14,
                PsbVersion::IndexedV5 => Face::SIZE as u32,
            }),
            Self::MarkSurfaces => Some(2),
            Self::Leaves => Some(match version {
                PsbVersion::LegacyV1 => 26,
                PsbVersion::IndexedV5 => Leaf::SIZE as u32,
            }),
            Self::Nodes => Some(match version {
                PsbVersion::LegacyV1 => 34,
                // PSB5 predates resident render bounds and stores only the
                // splitter plane plus two children. `ResidentMap` expands
                // those records into the current semantic node layout.
                PsbVersion::IndexedV5 => 6,
            }),
            Self::ClipNodes => Some(6),
            Self::Models => Some(match version {
                PsbVersion::LegacyV1 => 32,
                PsbVersion::IndexedV5 => BrushModel::SIZE as u32,
            }),
            Self::Strings => Some(1),
            Self::Entities => Some(50),
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct LumpRange {
    pub offset: u32,
    pub len: u32,
}

impl LumpRange {
    pub const EMPTY: Self = Self { offset: 0, len: 0 };

    pub const fn end(self) -> u32 {
        self.offset + self.len
    }

    pub const fn record_count(self, kind: LumpKind, version: PsbVersion) -> Option<u32> {
        match kind.record_size(version) {
            Some(size) => Some(self.len / size),
            None => None,
        }
    }
}

/// Minimal random-access input used by both CD streaming and host files.
pub trait ReadAt {
    /// Backend-specific read failure.
    type Error;

    /// Total readable byte length.
    fn len(&self) -> u32;

    /// Whether the input contains no bytes.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Fill `output` from the exact byte offset or return a backend error.
    fn read_exact_at(&mut self, offset: u32, output: &mut [u8]) -> Result<(), Self::Error>;
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SliceReadError;

impl fmt::Display for SliceReadError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("slice range is out of bounds")
    }
}

pub struct SliceReader<'a> {
    bytes: &'a [u8],
}

impl<'a> SliceReader<'a> {
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }
}

impl ReadAt for SliceReader<'_> {
    type Error = SliceReadError;

    fn len(&self) -> u32 {
        self.bytes.len().min(u32::MAX as usize) as u32
    }

    fn read_exact_at(&mut self, offset: u32, output: &mut [u8]) -> Result<(), Self::Error> {
        let start = offset as usize;
        let end = start.checked_add(output.len()).ok_or(SliceReadError)?;
        output.copy_from_slice(self.bytes.get(start..end).ok_or(SliceReadError)?);
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PsbError<E> {
    Read(E),
    TooSmall {
        len: u32,
    },
    BadMagic {
        found: u32,
    },
    Truncated {
        offset: u32,
        needed: u32,
        len: u32,
    },
    WrongLump {
        expected: LumpKind,
        found: i32,
    },
    NegativeLumpSize {
        kind: LumpKind,
        size: i32,
    },
    MisalignedLump {
        kind: LumpKind,
        size: u32,
        record_size: u32,
    },
    TrailingData {
        parsed: u32,
        len: u32,
    },
}

impl<E: fmt::Display> fmt::Display for PsbError<E> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(error) => write!(output, "read failed: {error}"),
            Self::TooSmall { len } => write!(output, "PSB is only {len} bytes"),
            Self::BadMagic { found } => write!(output, "bad PSB magic {found:#010x}"),
            Self::Truncated {
                offset,
                needed,
                len,
            } => write!(
                output,
                "PSB range {offset}..{} exceeds file length {len}",
                offset.saturating_add(*needed)
            ),
            Self::WrongLump { expected, found } => {
                write!(output, "expected lump {expected:?}, found type {found}")
            }
            Self::NegativeLumpSize { kind, size } => {
                write!(output, "lump {kind:?} has negative size {size}")
            }
            Self::MisalignedLump {
                kind,
                size,
                record_size,
            } => write!(
                output,
                "lump {kind:?} size {size} is not a multiple of {record_size}"
            ),
            Self::TrailingData { parsed, len } => {
                write!(output, "PSB ended at {parsed}, but file length is {len}")
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PsbIndex {
    version: PsbVersion,
    file_len: u32,
    lumps: [LumpRange; LUMP_COUNT],
}

impl PsbIndex {
    pub fn read<R: ReadAt>(reader: &mut R) -> Result<Self, PsbError<R::Error>> {
        let file_len = reader.len();
        if file_len < PSB_HEADER_BYTES {
            return Err(PsbError::TooSmall { len: file_len });
        }

        let mut word = [0u8; 4];
        reader.read_exact_at(0, &mut word).map_err(PsbError::Read)?;
        let magic = u32::from_le_bytes(word);
        let version = PsbVersion::from_magic(magic).ok_or(PsbError::BadMagic { found: magic })?;

        let mut lumps = [LumpRange::EMPTY; LUMP_COUNT];
        let mut cursor = PSB_HEADER_BYTES;
        let mut header = [0u8; LUMP_HEADER_BYTES as usize];
        for (index, expected) in LumpKind::ALL.into_iter().enumerate() {
            checked_range(cursor, LUMP_HEADER_BYTES, file_len)?;
            reader
                .read_exact_at(cursor, &mut header)
                .map_err(PsbError::Read)?;
            let found = i32::from_le_bytes(header[0..4].try_into().unwrap());
            if found != expected as i32 {
                return Err(PsbError::WrongLump { expected, found });
            }
            let signed_size = i32::from_le_bytes(header[4..8].try_into().unwrap());
            if signed_size < 0 {
                return Err(PsbError::NegativeLumpSize {
                    kind: expected,
                    size: signed_size,
                });
            }
            let size = signed_size as u32;
            if let Some(record_size) = expected.record_size(version) {
                if !size.is_multiple_of(record_size) {
                    return Err(PsbError::MisalignedLump {
                        kind: expected,
                        size,
                        record_size,
                    });
                }
            }
            let data_offset = cursor + LUMP_HEADER_BYTES;
            checked_range(data_offset, size, file_len)?;
            lumps[index] = LumpRange {
                offset: data_offset,
                len: size,
            };
            cursor = data_offset + size;
        }

        if cursor != file_len {
            return Err(PsbError::TrailingData {
                parsed: cursor,
                len: file_len,
            });
        }
        Ok(Self {
            version,
            file_len,
            lumps,
        })
    }

    pub const fn version(&self) -> PsbVersion {
        self.version
    }

    pub const fn magic(&self) -> u32 {
        self.version.magic()
    }

    pub const fn file_len(&self) -> u32 {
        self.file_len
    }

    pub const fn lump(&self, kind: LumpKind) -> LumpRange {
        self.lumps[kind as usize]
    }
}

fn checked_range<E>(offset: u32, needed: u32, len: u32) -> Result<(), PsbError<E>> {
    if offset > len || needed > len - offset {
        Err(PsbError::Truncated {
            offset,
            needed,
            len,
        })
    } else {
        Ok(())
    }
}

pub const TEXTURE_SPECIAL: u8 = 1;
pub const TEXTURE_LIQUID: u8 = 2;
pub const TEXTURE_SKY: u8 = 4;
pub const TEXTURE_INVISIBLE: u8 = 8;
pub const TEXTURE_ANIMATED: u8 = 16;
pub const TEXTURE_LARGE: u8 = 32;
pub const TEXTURE_NULL: u8 = 0x80;

pub const FACE_BACKSIDE: u16 = 1;
pub const FACE_BAKED_UV: u16 = 2;
pub const FACE_BAKED_LIGHT: u16 = 4;
/// PXBSP face override that renders both authored sides regardless of the
/// shared material's normal sidedness policy.
pub const FACE_TWO_SIDED: u16 = 8;
/// Every cooked UV lies inside one copy of the face texture. The runtime may
/// add the texture's page origin and use compact packets without GP0(E2).
pub const FACE_PAGE_LOCAL_UV: u16 = 16;

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Vec2U8 {
    pub x: u8,
    pub y: u8,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Vec2I16 {
    pub x: i16,
    pub y: i16,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Vec3I16 {
    pub x: i16,
    pub y: i16,
    pub z: i16,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Vec3I32 {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

pub trait CookedRecord: Copy {
    const SIZE: usize;
    fn decode(bytes: &[u8]) -> Self;
}

impl CookedRecord for u16 {
    const SIZE: usize = 2;

    fn decode(bytes: &[u8]) -> Self {
        u16::from_le_bytes([bytes[0], bytes[1]])
    }
}

#[derive(Copy, Clone)]
pub struct RecordSlice<'a, T> {
    bytes: &'a [u8],
    marker: PhantomData<T>,
}

impl<'a, T: CookedRecord> RecordSlice<'a, T> {
    pub fn new(bytes: &'a [u8]) -> Option<Self> {
        if !bytes.len().is_multiple_of(T::SIZE) {
            return None;
        }
        Some(Self {
            bytes,
            marker: PhantomData,
        })
    }

    pub const fn len(self) -> usize {
        self.bytes.len() / T::SIZE
    }

    pub const fn is_empty(self) -> bool {
        self.bytes.is_empty()
    }

    /// Return the validated wire bytes backing this record view.
    ///
    /// This does not imply that every cooked record is native-layout. Callers
    /// may only cast records whose type explicitly pins its endian-independent
    /// wire layout and alignment.
    pub const fn as_bytes(self) -> &'a [u8] {
        self.bytes
    }

    pub fn get(self, index: usize) -> Option<T> {
        let start = index.checked_mul(T::SIZE)?;
        self.bytes.get(start..start + T::SIZE)?;
        Some(unsafe { self.get_unchecked(index) })
    }

    /// Raw bytes of one record, bounds-checked.
    pub fn record_bytes(self, index: usize) -> Option<&'a [u8]> {
        let start = index.checked_mul(T::SIZE)?;
        self.bytes.get(start..start + T::SIZE)
    }

    /// Decode one record after its index has been validated by the caller.
    ///
    /// # Safety
    /// `index` must be less than [`Self::len`].
    #[inline(always)]
    pub unsafe fn get_unchecked(self, index: usize) -> T {
        let start = index * T::SIZE;
        let bytes = unsafe { core::slice::from_raw_parts(self.bytes.as_ptr().add(start), T::SIZE) };
        T::decode(bytes)
    }

    pub fn iter(self) -> RecordIter<'a, T> {
        RecordIter {
            records: self,
            index: 0,
        }
    }
}

pub struct RecordIter<'a, T> {
    records: RecordSlice<'a, T>,
    index: usize,
}

impl<T: CookedRecord> Iterator for RecordIter<'_, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        let value = self.records.get(self.index)?;
        self.index += 1;
        Some(value)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.records.len().saturating_sub(self.index);
        (remaining, Some(remaining))
    }
}

impl<T: CookedRecord> ExactSizeIterator for RecordIter<'_, T> {}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Vertex {
    pub position: Vec3I16,
    pub texture: Vec2U8,
    /// Either two light-style contributions or a baked packet RGB word.
    pub light: u32,
}

impl Vertex {
    pub const fn light_contributions(self) -> [u8; 2] {
        [self.light as u8, (self.light >> 8) as u8]
    }
}

impl CookedRecord for Vertex {
    const SIZE: usize = 12;

    fn decode(bytes: &[u8]) -> Self {
        Self {
            position: vec3_i16(bytes, 0),
            texture: Vec2U8 {
                x: bytes[6],
                y: bytes[7],
            },
            light: u32_at(bytes, 8),
        }
    }
}

/// `IVX1` header stored at the front of a PSB4 vertex lump.
pub const INDEXED_VERTEX_MAGIC: u32 = 0x3158_5649;
/// Bytes in the fixed PSB4 indexed-vertex header.
pub const INDEXED_VERTEX_HEADER_BYTES: usize = 8;

/// One face corner referencing a shared position in a PSB4 vertex lump.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct IndexedVertexCorner {
    /// Index into the position array following all corner records.
    pub position_index: u16,
    /// Material-relative or baked atlas UV.
    pub texture: [u8; 2],
    /// Two light contributions in the low bytes or baked RGB.
    pub light: u32,
}

/// One exact quantized position shared by PSB4 corners.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct IndexedVertexPosition {
    /// Signed world/model coordinates in cooked Quake units.
    pub position: [i16; 3],
}

const _: [(); 8] = [(); core::mem::size_of::<IndexedVertexCorner>()];
const _: [(); 6] = [(); core::mem::size_of::<IndexedVertexPosition>()];

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Plane {
    /// Q3.12 unit normal.
    pub normal: Vec3I16,
    /// Q20.12 distance from the origin.
    pub distance: i32,
    /// Cooker-authored axial fast-path class. Kept at its legacy semantic and
    /// physical width because exact real-MIPS A/B evidence proves that
    /// reclassifying the quantized normal is not visually equivalent.
    pub kind: i32,
}

impl CookedRecord for Plane {
    // Exact real-MIPS A/B evidence requires the cooker-authored classification
    // for visual parity; reclassifying the quantized normal dropped geometry.
    const SIZE: usize = 14;

    fn decode(bytes: &[u8]) -> Self {
        Self {
            normal: vec3_i16(bytes, 0),
            distance: i32_at(bytes, 6),
            kind: i32_at(bytes, 10),
        }
    }
}

/// Native PXBSP plane record, matching Quake-PSX's directly addressable
/// `mplane_t`: quantized normal, authored axial class, cached normal sign
/// bits, then an aligned Q20.12 distance.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct CompactPlane {
    pub normal: Vec3I16,
    pub kind: u8,
    pub sign_bits: u8,
    pub distance: i32,
}

const _: [(); 12] = [(); core::mem::size_of::<CompactPlane>()];
const _: [(); 4] = [(); core::mem::align_of::<CompactPlane>()];

impl CompactPlane {
    pub const fn decoded(self) -> Plane {
        Plane {
            normal: self.normal,
            distance: self.distance,
            kind: self.kind as i32,
        }
    }
}

impl CookedRecord for CompactPlane {
    const SIZE: usize = 12;

    fn decode(bytes: &[u8]) -> Self {
        Self {
            normal: vec3_i16(bytes, 0),
            kind: bytes[6],
            sign_bits: bytes[7],
            distance: i32_at(bytes, 8),
        }
    }
}

impl<'a> RecordSlice<'a, CompactPlane> {
    /// Borrow current PXBSP plane records without reconstructing them at each
    /// collision or render-tree step.
    #[cfg(target_endian = "little")]
    pub fn as_native_compact_planes(self) -> Option<&'a [CompactPlane]> {
        let bytes = self.as_bytes();
        if bytes
            .as_ptr()
            .align_offset(core::mem::align_of::<CompactPlane>())
            != 0
        {
            return None;
        }
        Some(unsafe {
            core::slice::from_raw_parts(bytes.as_ptr().cast::<CompactPlane>(), self.len())
        })
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct TextureInfo {
    pub atlas: Vec2U8,
    pub size: Vec2I16,
    pub texture_page: u16,
    pub flags: u8,
    pub animation_total: i8,
    pub animation_min: i8,
    pub animation_max: i8,
    pub animation_next: i8,
    pub animation_alt: i8,
}

impl CookedRecord for TextureInfo {
    const SIZE: usize = 14;

    fn decode(bytes: &[u8]) -> Self {
        Self {
            atlas: Vec2U8 {
                x: bytes[0],
                y: bytes[1],
            },
            size: Vec2I16 {
                x: i16_at(bytes, 2),
                y: i16_at(bytes, 4),
            },
            texture_page: u16_at(bytes, 6),
            flags: bytes[8],
            animation_total: bytes[9] as i8,
            animation_min: bytes[10] as i8,
            animation_max: bytes[11] as i8,
            animation_next: bytes[12] as i8,
            animation_alt: bytes[13] as i8,
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Face {
    /// Non-negative plane index. Compact v3 stores this as `u16`, while the
    /// semantic type stays legacy-shaped for the MIPS aggregate-return ABI.
    pub plane: i16,
    pub flags: u16,
    pub first_vertex: i32,
    pub vertex_count: i16,
    /// Non-negative texture/material index; see [`Self::plane`].
    pub texture: i16,
    pub light_styles: [u8; 2],
}

impl CookedRecord for Face {
    const SIZE: usize = 10;

    fn decode(bytes: &[u8]) -> Self {
        Self {
            plane: u16_at(bytes, 0) as i16,
            first_vertex: u16_at(bytes, 2) as i32,
            texture: u16_at(bytes, 4) as i16,
            flags: bytes[6] as u16,
            vertex_count: bytes[7] as i16,
            light_styles: [bytes[8], bytes[9]],
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Leaf {
    pub contents: i16,
    pub visibility_offset: i32,
    /// Legacy semantic bounds retained for the proven MIPS ABI. The compact
    /// record omits these unused fields and decodes them as zero.
    pub mins: Vec3I16,
    pub maxs: Vec3I16,
    pub first_mark_surface: u16,
    pub mark_surface_count: u16,
    pub lightmap: [u8; 2],
    pub light_styles: [u8; 2],
}

/// Compact leaf: contents i8, pad, mark count u16, visibility offset i32
/// (`-1` = none), first mark u16, lightmap, light styles. Wide enough for the
/// editor's exact per-leaf marks and multi-page visibility lumps.
impl CookedRecord for Leaf {
    const SIZE: usize = 14;

    fn decode(bytes: &[u8]) -> Self {
        Self {
            contents: bytes[0] as i8 as i16,
            mark_surface_count: u16_at(bytes, 2),
            visibility_offset: i32_at(bytes, 4),
            mins: Vec3I16::default(),
            maxs: Vec3I16::default(),
            first_mark_surface: u16_at(bytes, 8),
            lightmap: [bytes[10], bytes[11]],
            light_styles: [bytes[12], bytes[13]],
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Node {
    pub plane: u16,
    /// Negative values encode `-(leaf + 1)`.
    pub children: [i16; 2],
    /// Conservative Y-up render bounds in the model's local coordinate
    /// space. PXBSP v5 stores these explicitly for Quake-style node culling.
    pub mins: Vec3I16,
    pub maxs: Vec3I16,
    pub surface_mins: Vec3I16,
    pub surface_maxs: Vec3I16,
    pub first_face: u16,
    pub face_count: u16,
}

/// World units represented by one signed PXBSP v5 node-bound code. Values
/// outside the finite i8 range expand to the corresponding i16 extreme, so
/// large maps remain conservatively culled while ordinary brush worlds keep
/// bounds tight enough for hierarchical frustum classification.
pub const NODE_BOUND_GRID: i16 = 8;
const NODE_BOUND_GRID_SHIFT: u32 = NODE_BOUND_GRID.trailing_zeros();

/// Quantize a node minimum outward toward negative infinity.
pub const fn encode_node_bound_min(value: i16) -> i8 {
    let units = (value as i32) >> NODE_BOUND_GRID_SHIFT;
    if units < i8::MIN as i32 {
        i8::MIN
    } else {
        units as i8
    }
}

/// Quantize a node maximum outward toward positive infinity.
pub const fn encode_node_bound_max(value: i16) -> i8 {
    let units = ((value as i32) + (NODE_BOUND_GRID as i32 - 1)) >> NODE_BOUND_GRID_SHIFT;
    if units > i8::MAX as i32 {
        i8::MAX
    } else {
        units as i8
    }
}

/// Expand a quantized node minimum into model-local world units.
pub const fn decode_node_bound_min(code: i8) -> i16 {
    if code == i8::MIN {
        i16::MIN
    } else {
        (code as i16) << NODE_BOUND_GRID_SHIFT
    }
}

/// Expand a quantized node maximum into model-local world units.
pub const fn decode_node_bound_max(code: i8) -> i16 {
    if code == i8::MAX {
        i16::MAX
    } else {
        (code as i16) << NODE_BOUND_GRID_SHIFT
    }
}

impl CookedRecord for Node {
    /// PXBSP v5 node: plane/children (6), quantized bounds (6), face range (4).
    const SIZE: usize = 16;

    fn decode(bytes: &[u8]) -> Self {
        Self {
            plane: u16_at(bytes, 0),
            children: [i16_at(bytes, 2), i16_at(bytes, 4)],
            mins: Vec3I16 {
                x: decode_node_bound_min(bytes[6] as i8),
                y: decode_node_bound_min(bytes[7] as i8),
                z: decode_node_bound_min(bytes[8] as i8),
            },
            maxs: Vec3I16 {
                x: decode_node_bound_max(bytes[9] as i8),
                y: decode_node_bound_max(bytes[10] as i8),
                z: decode_node_bound_max(bytes[11] as i8),
            },
            surface_mins: Vec3I16::default(),
            surface_maxs: Vec3I16::default(),
            first_face: u16_at(bytes, 12),
            face_count: u16_at(bytes, 14),
        }
    }
}

/// Native view of the PXBSP v5 render-tree node wire record.
///
/// The public [`Node`] retains its proven legacy aggregate layout for MIPS
/// callers. Traversal-only consumers can borrow this narrow representation
/// directly after resident-map validation.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct CompactNode {
    pub plane: u16,
    pub children: [i16; 2],
    /// Signed eight-world-unit grid codes; use the node-bound decode helpers.
    pub mins: [i8; 3],
    pub maxs: [i8; 3],
    pub first_face: u16,
    pub face_count: u16,
}

const _: [(); 16] = [(); core::mem::size_of::<CompactNode>()];
const _: [(); 2] = [(); core::mem::align_of::<CompactNode>()];

impl<'a> RecordSlice<'a, Node> {
    /// Borrow validated v5 node records without reconstructing the larger
    /// legacy semantic aggregate on every tree step.
    #[cfg(target_endian = "little")]
    pub fn as_native_compact_nodes(self) -> Option<&'a [CompactNode]> {
        let bytes = self.as_bytes();
        if bytes
            .as_ptr()
            .align_offset(core::mem::align_of::<CompactNode>())
            != 0
        {
            return None;
        }
        Some(unsafe {
            core::slice::from_raw_parts(bytes.as_ptr().cast::<CompactNode>(), self.len())
        })
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ClipNode {
    pub plane: i16,
    pub children: [i16; 2],
}

const _: [(); 6] = [(); core::mem::size_of::<ClipNode>()];
const _: [(); 2] = [(); core::mem::align_of::<ClipNode>()];

impl<'a> RecordSlice<'a, ClipNode> {
    /// Borrow little-endian clip-node wire records as their identical native
    /// representation. Resident BSP lump packing guarantees at least 4-byte
    /// alignment; this method still checks alignment so independent callers
    /// fail closed rather than creating a misaligned slice.
    #[cfg(target_endian = "little")]
    pub fn as_native_clip_nodes(self) -> Option<&'a [ClipNode]> {
        let bytes = self.as_bytes();
        if bytes.is_empty() {
            return Some(&[]);
        }
        if bytes
            .as_ptr()
            .align_offset(core::mem::align_of::<ClipNode>())
            != 0
        {
            return None;
        }
        Some(unsafe { core::slice::from_raw_parts(bytes.as_ptr().cast::<ClipNode>(), self.len()) })
    }
}

impl CookedRecord for ClipNode {
    const SIZE: usize = 6;

    fn decode(bytes: &[u8]) -> Self {
        Self {
            plane: i16_at(bytes, 0),
            children: [i16_at(bytes, 2), i16_at(bytes, 4)],
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct BrushModel {
    pub mins: Vec3I16,
    pub maxs: Vec3I16,
    /// Legacy semantic origin retained for the proven MIPS ABI. Entity/mover
    /// transforms own placement, so compact v3 decodes this unused field as
    /// zero rather than storing it.
    pub origin: Vec3I16,
    pub head_nodes: [i16; 4],
    pub visible_leaves: i16,
    pub first_face: u16,
    pub face_count: u16,
}

/// Brush model: the legacy 32-byte layout, origin included (Quake writes
/// zero, the editor's doors and lifts carry theirs).
impl CookedRecord for BrushModel {
    const SIZE: usize = 32;

    fn decode(bytes: &[u8]) -> Self {
        Self {
            mins: vec3_i16(bytes, 0),
            maxs: vec3_i16(bytes, 6),
            origin: vec3_i16(bytes, 12),
            head_nodes: [
                i16_at(bytes, 18),
                i16_at(bytes, 20),
                i16_at(bytes, 22),
                i16_at(bytes, 24),
            ],
            visible_leaves: i16_at(bytes, 26),
            first_face: u16_at(bytes, 28),
            face_count: u16_at(bytes, 30),
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct MapEntity {
    pub class_name: u8,
    pub noise: i8,
    pub spawn_flags: u16,
    pub model: i16,
    pub health: i16,
    pub damage: i16,
    pub speed: i16,
    pub count: i16,
    pub height: i16,
    pub target: u16,
    pub kill_target: u16,
    pub target_name: u16,
    pub string: u16,
    pub wait: i32,
    pub delay: i32,
    /// Q3.12 turns/degrees as cooked by the original converter.
    pub angles: Vec3I16,
    /// Q20.12 world position.
    pub origin: Vec3I32,
}

impl CookedRecord for MapEntity {
    const SIZE: usize = 50;

    fn decode(bytes: &[u8]) -> Self {
        Self {
            class_name: bytes[0],
            noise: bytes[1] as i8,
            spawn_flags: u16_at(bytes, 2),
            model: i16_at(bytes, 4),
            health: i16_at(bytes, 6),
            damage: i16_at(bytes, 8),
            speed: i16_at(bytes, 10),
            count: i16_at(bytes, 12),
            height: i16_at(bytes, 14),
            target: u16_at(bytes, 16),
            kill_target: u16_at(bytes, 18),
            target_name: u16_at(bytes, 20),
            string: u16_at(bytes, 22),
            wait: i32_at(bytes, 24),
            delay: i32_at(bytes, 28),
            angles: vec3_i16(bytes, 32),
            origin: Vec3I32 {
                x: i32_at(bytes, 38),
                y: i32_at(bytes, 42),
                z: i32_at(bytes, 46),
            },
        }
    }
}

/// One entry in the cooked map-local SPU sound table.
///
/// The address is absolute within the PlayStation's 512 KiB of SPU RAM. The
/// ADPCM payload which follows the table is laid out from address `0x1100`.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct SoundEffect {
    pub id: i16,
    pub frames: u16,
    pub spu_address: u32,
}

/// Maximum number of retained alias-style models in one cooked map.
pub const MAX_ALIAS_MODELS: usize = 128;
/// Maximum number of animation frames in one retained alias-style model.
pub const MAX_ALIAS_FRAMES: usize = 256;
/// Maximum number of vertices in one retained alias-style model.
pub const MAX_ALIAS_VERTICES: usize = 512;
/// Maximum number of triangles in one retained alias-style model.
pub const MAX_ALIAS_TRIANGLES: usize = 1024;
/// Maximum number of skins in one retained alias-style model.
pub const MAX_ALIAS_SKINS: usize = 3;

/// One texture atlas binding in a cooked alias-model header.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct AliasModelSkin {
    pub texture_page: u16,
    pub base: Vec2U8,
}

/// Fixed-size header for one cooked alias-style model.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct AliasModelHeader {
    pub model_type: u8,
    pub flags: u8,
    pub id: i16,
    pub frame_count: u16,
    pub vertex_count: u16,
    pub triangle_count: u16,
    pub skin_count: u16,
    /// Per-axis Q3.12 scale applied to the compact byte vertices.
    pub scale: Vec3I16,
    /// Integer model-space offset applied before world translation.
    pub offset: Vec3I16,
    /// Q20.12 bounds across every retained frame.
    pub mins: Vec3I32,
    /// Q20.12 bounds across every retained frame.
    pub maxs: Vec3I32,
    pub skins: [AliasModelSkin; MAX_ALIAS_SKINS],
    /// Byte offset from the model payload to the first skin's triangles.
    pub triangle_offset: u32,
    /// Byte offset from the model payload to the first frame's vertices.
    pub frame_offset: u32,
}

impl CookedRecord for AliasModelHeader {
    const SIZE: usize = 68;

    fn decode(bytes: &[u8]) -> Self {
        Self {
            model_type: bytes[0],
            flags: bytes[1],
            id: i16_at(bytes, 2),
            frame_count: u16_at(bytes, 4),
            vertex_count: u16_at(bytes, 6),
            triangle_count: u16_at(bytes, 8),
            skin_count: u16_at(bytes, 10),
            scale: vec3_i16(bytes, 12),
            offset: vec3_i16(bytes, 18),
            mins: vec3_i32(bytes, 24),
            maxs: vec3_i32(bytes, 36),
            skins: [
                AliasModelSkin {
                    texture_page: u16_at(bytes, 48),
                    base: Vec2U8 {
                        x: bytes[50],
                        y: bytes[51],
                    },
                },
                AliasModelSkin {
                    texture_page: u16_at(bytes, 52),
                    base: Vec2U8 {
                        x: bytes[54],
                        y: bytes[55],
                    },
                },
                AliasModelSkin {
                    texture_page: u16_at(bytes, 56),
                    base: Vec2U8 {
                        x: bytes[58],
                        y: bytes[59],
                    },
                },
            ],
            triangle_offset: u32_at(bytes, 60),
            frame_offset: u32_at(bytes, 64),
        }
    }
}

/// One cooked indexed alias-model triangle.
///
/// Each corner keeps its texture coordinate in the low half and its byte
/// offset into the eight-byte projected-vertex cache in the high half.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct AliasModelTriangle {
    pub corners: [u32; 3],
}

impl CookedRecord for AliasModelTriangle {
    const SIZE: usize = 12;

    fn decode(bytes: &[u8]) -> Self {
        Self {
            corners: [u32_at(bytes, 0), u32_at(bytes, 4), u32_at(bytes, 8)],
        }
    }
}

/// One compact position in a cooked alias-model animation frame.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct AliasModelVertex {
    pub position: [u8; 3],
}

impl CookedRecord for AliasModelVertex {
    const SIZE: usize = 3;

    fn decode(bytes: &[u8]) -> Self {
        Self {
            position: [bytes[0], bytes[1], bytes[2]],
        }
    }
}

/// Validation failure in a cooked alias-model table.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AliasModelError {
    TooSmall,
    BadCount(u32),
    TruncatedHeaders,
    BadType {
        model: usize,
        found: u8,
    },
    BadId {
        model: usize,
        id: i16,
    },
    DuplicateId {
        model: usize,
        id: i16,
    },
    BadFrameCount {
        model: usize,
        count: u16,
    },
    BadVertexCount {
        model: usize,
        count: u16,
    },
    BadTriangleCount {
        model: usize,
        count: u16,
    },
    BadSkinCount {
        model: usize,
        count: u16,
    },
    MisalignedTriangles {
        model: usize,
        offset: u32,
    },
    OutOfOrderData {
        model: usize,
    },
    BadTriangleRange {
        model: usize,
    },
    BadFrameLayout {
        model: usize,
    },
    BadFrameRange {
        model: usize,
    },
    NonZeroPadding {
        model: usize,
    },
    BadProjectedOffset {
        model: usize,
        triangle: usize,
        corner: usize,
        offset: u16,
    },
    TrailingData,
}

impl fmt::Display for AliasModelError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::TooSmall => output.write_str("alias-model table is smaller than its count"),
            Self::BadCount(count) => write!(output, "alias-model count {count} is out of range"),
            Self::TruncatedHeaders => output.write_str("alias-model headers are truncated"),
            Self::BadType { model, found } => {
                write!(output, "alias model {model} has unsupported type {found}")
            }
            Self::BadId { model, id } => write!(output, "alias model {model} has bad ID {id}"),
            Self::DuplicateId { model, id } => {
                write!(output, "alias model {model} duplicates ID {id}")
            }
            Self::BadFrameCount { model, count } => {
                write!(output, "alias model {model} has {count} frames")
            }
            Self::BadVertexCount { model, count } => {
                write!(output, "alias model {model} has {count} vertices")
            }
            Self::BadTriangleCount { model, count } => {
                write!(output, "alias model {model} has {count} triangles")
            }
            Self::BadSkinCount { model, count } => {
                write!(output, "alias model {model} has {count} skins")
            }
            Self::MisalignedTriangles { model, offset } => write!(
                output,
                "alias model {model} triangle offset {offset} is not four-byte aligned"
            ),
            Self::OutOfOrderData { model } => {
                write!(output, "alias model {model} data overlaps the preceding model")
            }
            Self::BadTriangleRange { model } => {
                write!(output, "alias model {model} triangle range is invalid")
            }
            Self::BadFrameLayout { model } => {
                write!(output, "alias model {model} frame data does not follow its triangles")
            }
            Self::BadFrameRange { model } => {
                write!(output, "alias model {model} frame range is invalid")
            }
            Self::NonZeroPadding { model } => {
                write!(output, "alias model {model} has nonzero alignment padding")
            }
            Self::BadProjectedOffset {
                model,
                triangle,
                corner,
                offset,
            } => write!(
                output,
                "alias model {model} triangle {triangle} corner {corner} has bad projected offset {offset}"
            ),
            Self::TrailingData => output.write_str("alias-model table has trailing data"),
        }
    }
}

/// Checked, allocation-free view of one map's retained alias-model table.
#[derive(Copy, Clone)]
pub struct AliasModelTable<'a> {
    bytes: &'a [u8],
    count: usize,
    payload_offset: usize,
}

impl<'a> AliasModelTable<'a> {
    pub fn new(bytes: &'a [u8]) -> Result<Self, AliasModelError> {
        if bytes.len() < 4 {
            return Err(AliasModelError::TooSmall);
        }
        let raw_count = u32_at(bytes, 0);
        let count = raw_count as usize;
        if count == 0 || count > MAX_ALIAS_MODELS {
            return Err(AliasModelError::BadCount(raw_count));
        }
        let header_bytes = count
            .checked_mul(AliasModelHeader::SIZE)
            .ok_or(AliasModelError::TruncatedHeaders)?;
        let payload_offset = 4usize
            .checked_add(header_bytes)
            .ok_or(AliasModelError::TruncatedHeaders)?;
        if payload_offset > bytes.len() {
            return Err(AliasModelError::TruncatedHeaders);
        }

        let table = Self {
            bytes,
            count,
            payload_offset,
        };
        let payload = &bytes[payload_offset..];
        let mut previous_end = 0usize;
        for model_index in 0..count {
            let header = table.header_at(model_index);
            if header.model_type != 1 {
                return Err(AliasModelError::BadType {
                    model: model_index,
                    found: header.model_type,
                });
            }
            if header.id <= 0 || header.id as usize >= MAX_ALIAS_MODELS {
                return Err(AliasModelError::BadId {
                    model: model_index,
                    id: header.id,
                });
            }
            for preceding in 0..model_index {
                if table.header_at(preceding).id == header.id {
                    return Err(AliasModelError::DuplicateId {
                        model: model_index,
                        id: header.id,
                    });
                }
            }
            if header.frame_count == 0 || header.frame_count as usize > MAX_ALIAS_FRAMES {
                return Err(AliasModelError::BadFrameCount {
                    model: model_index,
                    count: header.frame_count,
                });
            }
            if header.vertex_count == 0 || header.vertex_count as usize > MAX_ALIAS_VERTICES {
                return Err(AliasModelError::BadVertexCount {
                    model: model_index,
                    count: header.vertex_count,
                });
            }
            if header.triangle_count == 0 || header.triangle_count as usize > MAX_ALIAS_TRIANGLES {
                return Err(AliasModelError::BadTriangleCount {
                    model: model_index,
                    count: header.triangle_count,
                });
            }
            if header.skin_count == 0 || header.skin_count as usize > MAX_ALIAS_SKINS {
                return Err(AliasModelError::BadSkinCount {
                    model: model_index,
                    count: header.skin_count,
                });
            }

            let triangle_start = header.triangle_offset as usize;
            if triangle_start & 3 != 0 {
                return Err(AliasModelError::MisalignedTriangles {
                    model: model_index,
                    offset: header.triangle_offset,
                });
            }
            if triangle_start < previous_end {
                return Err(AliasModelError::OutOfOrderData { model: model_index });
            }
            if payload
                .get(previous_end..triangle_start)
                .ok_or(AliasModelError::BadTriangleRange { model: model_index })?
                .iter()
                .any(|&byte| byte != 0)
            {
                return Err(AliasModelError::NonZeroPadding { model: model_index });
            }
            let triangles_per_skin = (header.triangle_count as usize)
                .checked_mul(AliasModelTriangle::SIZE)
                .ok_or(AliasModelError::BadTriangleRange { model: model_index })?;
            let triangle_bytes = triangles_per_skin
                .checked_mul(header.skin_count as usize)
                .ok_or(AliasModelError::BadTriangleRange { model: model_index })?;
            let triangle_end = triangle_start
                .checked_add(triangle_bytes)
                .ok_or(AliasModelError::BadTriangleRange { model: model_index })?;
            if triangle_end > payload.len() {
                return Err(AliasModelError::BadTriangleRange { model: model_index });
            }
            if header.frame_offset as usize != triangle_end {
                return Err(AliasModelError::BadFrameLayout { model: model_index });
            }
            let frame_bytes = (header.frame_count as usize)
                .checked_mul(header.vertex_count as usize)
                .and_then(|count| count.checked_mul(AliasModelVertex::SIZE))
                .ok_or(AliasModelError::BadFrameRange { model: model_index })?;
            let frame_end = triangle_end
                .checked_add(frame_bytes)
                .ok_or(AliasModelError::BadFrameRange { model: model_index })?;
            if frame_end > payload.len() {
                return Err(AliasModelError::BadFrameRange { model: model_index });
            }

            let total_triangles = header.triangle_count as usize * header.skin_count as usize;
            for triangle in 0..total_triangles {
                let start = triangle_start + triangle * AliasModelTriangle::SIZE;
                let decoded =
                    AliasModelTriangle::decode(&payload[start..start + AliasModelTriangle::SIZE]);
                for (corner, packed) in decoded.corners.into_iter().enumerate() {
                    let offset = (packed >> 16) as u16;
                    if offset & 7 != 0 || offset as usize >= header.vertex_count as usize * 8 {
                        return Err(AliasModelError::BadProjectedOffset {
                            model: model_index,
                            triangle,
                            corner,
                            offset,
                        });
                    }
                }
            }
            previous_end = frame_end;
        }
        if previous_end != payload.len() {
            return Err(AliasModelError::TrailingData);
        }
        Ok(table)
    }

    /// Rebuild a lightweight table view after [`Self::new`] has validated the
    /// same immutable byte slice.
    ///
    /// This avoids repeating the complete model, frame, and triangle audit in
    /// a render loop. Map-loading code must retain ownership of `bytes` and
    /// must not mutate it between validation and this call.
    ///
    /// # Safety
    /// `bytes` must be the unchanged slice previously accepted by
    /// [`Self::new`].
    pub unsafe fn from_validated(bytes: &'a [u8]) -> Self {
        let count = u32_at(bytes, 0) as usize;
        Self {
            bytes,
            count,
            payload_offset: 4 + count * AliasModelHeader::SIZE,
        }
    }

    pub const fn len(self) -> usize {
        self.count
    }

    pub const fn is_empty(self) -> bool {
        self.count == 0
    }

    pub fn get(self, id: i16) -> Option<AliasModelView<'a>> {
        (0..self.count)
            .find(|&index| self.header_at(index).id == id)
            .map(|index| self.model_at(index).expect("model index came from table"))
    }

    pub fn model_at(self, index: usize) -> Option<AliasModelView<'a>> {
        (index < self.count).then(|| AliasModelView {
            payload: &self.bytes[self.payload_offset..],
            header: self.header_at(index),
        })
    }

    fn header_at(self, index: usize) -> AliasModelHeader {
        let start = 4 + index * AliasModelHeader::SIZE;
        AliasModelHeader::decode(&self.bytes[start..start + AliasModelHeader::SIZE])
    }
}

/// Checked slices for one retained alias-style model.
#[derive(Copy, Clone)]
pub struct AliasModelView<'a> {
    payload: &'a [u8],
    header: AliasModelHeader,
}

impl<'a> AliasModelView<'a> {
    pub const fn header(self) -> AliasModelHeader {
        self.header
    }

    pub fn triangles(self, skin: usize) -> Option<RecordSlice<'a, AliasModelTriangle>> {
        RecordSlice::new(self.triangle_bytes(skin)?)
    }

    pub fn triangle_bytes(self, skin: usize) -> Option<&'a [u8]> {
        if skin >= self.header.skin_count as usize {
            return None;
        }
        let skin_bytes = self.header.triangle_count as usize * AliasModelTriangle::SIZE;
        let start = self.header.triangle_offset as usize + skin * skin_bytes;
        self.payload.get(start..start + skin_bytes)
    }

    pub fn frame_vertices(self, frame: usize) -> Option<RecordSlice<'a, AliasModelVertex>> {
        RecordSlice::new(self.frame_bytes(frame)?)
    }

    pub fn frame_bytes(self, frame: usize) -> Option<&'a [u8]> {
        if frame >= self.header.frame_count as usize {
            return None;
        }
        let frame_bytes = self.header.vertex_count as usize * AliasModelVertex::SIZE;
        let start = self.header.frame_offset as usize + frame * frame_bytes;
        self.payload.get(start..start + frame_bytes)
    }
}

impl CookedRecord for SoundEffect {
    const SIZE: usize = 8;

    fn decode(bytes: &[u8]) -> Self {
        Self {
            id: i16_at(bytes, 0),
            frames: u16_at(bytes, 2),
            spu_address: u32_at(bytes, 4),
        }
    }
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn i16_at(bytes: &[u8], offset: usize) -> i16 {
    u16_at(bytes, offset) as i16
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn i32_at(bytes: &[u8], offset: usize) -> i32 {
    u32_at(bytes, offset) as i32
}

fn vec3_i16(bytes: &[u8], offset: usize) -> Vec3I16 {
    Vec3I16 {
        x: i16_at(bytes, offset),
        y: i16_at(bytes, offset + 2),
        z: i16_at(bytes, offset + 4),
    }
}

fn vec3_i32(bytes: &[u8], offset: usize) -> Vec3I32 {
    Vec3I32 {
        x: i32_at(bytes, offset),
        y: i32_at(bytes, offset + 4),
        z: i32_at(bytes, offset + 8),
    }
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    struct Bytes(Vec<u8>);

    impl ReadAt for Bytes {
        type Error = ();

        fn len(&self) -> u32 {
            self.0.len() as u32
        }

        fn read_exact_at(&mut self, offset: u32, output: &mut [u8]) -> Result<(), Self::Error> {
            let start = offset as usize;
            output.copy_from_slice(self.0.get(start..start + output.len()).ok_or(())?);
            Ok(())
        }
    }

    fn valid_file() -> Bytes {
        valid_file_with_magic(PSB_MAGIC)
    }

    fn valid_file_with_magic(magic: u32) -> Bytes {
        let mut bytes = Vec::from(magic.to_le_bytes());
        for kind in LumpKind::ALL {
            bytes.extend_from_slice(&(kind as i32).to_le_bytes());
            bytes.extend_from_slice(&0i32.to_le_bytes());
        }
        Bytes(bytes)
    }

    fn one_alias_model() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&[1, 8]);
        bytes.extend_from_slice(&0x49i16.to_le_bytes());
        for value in [1u16, 3, 1, 1] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        for value in [4096i16, 4096, 4096, -4, 2, 7] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        for value in [
            -4i32 << 12,
            2i32 << 12,
            7i32 << 12,
            252i32 << 12,
            257i32 << 12,
            262i32 << 12,
        ] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&0x1234u16.to_le_bytes());
        bytes.extend_from_slice(&[17, 29]);
        bytes.extend_from_slice(&[0; 8]);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&(AliasModelTriangle::SIZE as u32).to_le_bytes());
        assert_eq!(bytes.len(), 4 + AliasModelHeader::SIZE);

        for (uv, projected_offset) in [(0x0201u16, 0u16), (0x0403, 8), (0x0605, 16)] {
            bytes.extend_from_slice(&((projected_offset as u32) << 16 | uv as u32).to_le_bytes());
        }
        bytes.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9]);
        bytes
    }

    #[test]
    fn indexes_all_lumps_without_allocating_map_data() {
        let mut bytes = valid_file();
        let index = PsbIndex::read(&mut bytes).unwrap();
        assert_eq!(index.file_len(), 4 + LUMP_COUNT as u32 * 8);
        assert_eq!(index.lump(LumpKind::TextureData).offset, 12);
        assert_eq!(index.lump(LumpKind::Entities).len, 0);
        assert_eq!(index.version(), PsbVersion::LegacyV1);
        assert_eq!(index.magic(), PSB_MAGIC);
    }

    #[test]
    fn recognizes_compact_v5_magic_and_record_contract() {
        let mut bytes = valid_file_with_magic(PSB5_MAGIC);
        let index = PsbIndex::read(&mut bytes).expect("compact index");
        assert_eq!(index.version(), PsbVersion::IndexedV5);
        assert_eq!(index.magic(), PSB5_MAGIC);
        assert_eq!(LumpKind::Planes.record_size(index.version()), Some(14));
        assert_eq!(LumpKind::Faces.record_size(index.version()), Some(10));
        assert_eq!(LumpKind::Leaves.record_size(index.version()), Some(14));
        assert_eq!(LumpKind::Nodes.record_size(index.version()), Some(6));
        assert_eq!(LumpKind::Models.record_size(index.version()), Some(32));
        assert_eq!(LumpKind::Vertices.record_size(index.version()), None);
    }

    #[test]
    fn rejects_the_retired_compact_magics() {
        for magic in [PSB2_MAGIC, PSB3_MAGIC, PSB4_MAGIC] {
            let mut bytes = valid_file_with_magic(magic);
            assert!(PsbIndex::read(&mut bytes).is_err(), "{magic:#x}");
        }
    }

    #[test]
    fn rejects_experimental_psb2_before_reading_ambiguous_records() {
        let mut bytes = Vec::from(PSB2_MAGIC.to_le_bytes());
        for kind in LumpKind::ALL {
            bytes.extend_from_slice(&(kind as i32).to_le_bytes());
            // Seventy is divisible by both the experimental 10-byte and final
            // 14-byte plane strides. The magic rejects it before that
            // structurally ambiguous payload can be interpreted.
            let payload_len = if kind == LumpKind::Planes { 70 } else { 0 };
            bytes.extend_from_slice(&(payload_len as i32).to_le_bytes());
            bytes.resize(bytes.len() + payload_len, 0);
        }
        assert!(matches!(
            PsbIndex::read(&mut Bytes(bytes)),
            Err(PsbError::BadMagic { found: PSB2_MAGIC })
        ));
    }

    #[test]
    fn compact_wire_records_keep_the_proven_legacy_semantic_abi() {
        assert_eq!(Plane::SIZE, 14);
        assert_eq!(Face::SIZE, 10);
        assert_eq!(Leaf::SIZE, 14);
        assert_eq!(Node::SIZE, 16);
        assert_eq!(BrushModel::SIZE, 32);

        // These are semantic Rust values returned by RecordSlice::get on the
        // MIPS guest, not their compact wire strides. Keep the layouts pinned
        // independently so future physical-format work cannot silently alter
        // aggregate-return codegen again.
        assert_eq!(core::mem::size_of::<Plane>(), 16);
        assert_eq!(core::mem::align_of::<Plane>(), 4);
        assert_eq!(core::mem::size_of::<Face>(), 16);
        assert_eq!(core::mem::align_of::<Face>(), 4);
        assert_eq!(core::mem::size_of::<Leaf>(), 28);
        assert_eq!(core::mem::align_of::<Leaf>(), 4);
        assert_eq!(core::mem::size_of::<Node>(), 34);
        assert_eq!(core::mem::align_of::<Node>(), 2);
        assert_eq!(core::mem::size_of::<ClipNode>(), 6);
        assert_eq!(core::mem::align_of::<ClipNode>(), 2);
        assert_eq!(core::mem::size_of::<BrushModel>(), 32);
        assert_eq!(core::mem::align_of::<BrushModel>(), 2);

        let leaf = Leaf::decode(&[
            -1i8 as u8, 0, 0x00, 0x01, 0xff, 0xff, 0xff, 0xff, 0x34, 0x12, 5, 6, 7, 8,
        ]);
        assert_eq!(leaf.contents, -1);
        assert_eq!(leaf.mark_surface_count, 256);
        assert_eq!(leaf.visibility_offset, -1);
        assert_eq!(leaf.first_mark_surface, 0x1234);
        assert_eq!(leaf.lightmap, [5, 6]);
        assert_eq!(leaf.light_styles, [7, 8]);
        assert_eq!(leaf.mins, Vec3I16::default());
        assert_eq!(leaf.maxs, Vec3I16::default());
        let node = Node::decode(&[0; Node::SIZE]);
        assert_eq!(node.mins, Vec3I16::default());
        assert_eq!(node.maxs, Vec3I16::default());
        assert_eq!(node.surface_mins, Vec3I16::default());
        assert_eq!(node.surface_maxs, Vec3I16::default());
        assert_eq!(node.first_face, 0);
        assert_eq!(node.face_count, 0);
        let mut model_bytes = [0u8; BrushModel::SIZE];
        model_bytes[12..14].copy_from_slice(&128i16.to_le_bytes());
        model_bytes[14..16].copy_from_slice(&16i16.to_le_bytes());
        model_bytes[16..18].copy_from_slice(&96i16.to_le_bytes());
        model_bytes[30..32].copy_from_slice(&7u16.to_le_bytes());
        let model = BrushModel::decode(&model_bytes);
        assert_eq!(
            model.origin,
            Vec3I16 {
                x: 128,
                y: 16,
                z: 96
            }
        );
        assert_eq!(model.face_count, 7);
    }

    #[test]
    fn decodes_sound_records_without_native_layout_assumptions() {
        let bytes = [0xCD, 0x00, 97, 0, 0x00, 0x11, 0x00, 0x00];
        assert_eq!(
            SoundEffect::decode(&bytes),
            SoundEffect {
                id: 0xCD,
                frames: 97,
                spu_address: 0x1100,
            }
        );
    }

    #[test]
    fn rejects_out_of_order_lumps() {
        let mut bytes = valid_file();
        bytes.0[4..8].copy_from_slice(&1i32.to_le_bytes());
        assert_eq!(
            PsbIndex::read(&mut bytes),
            Err(PsbError::WrongLump {
                expected: LumpKind::TextureData,
                found: 1,
            })
        );
    }

    #[test]
    fn rejects_record_lumps_with_partial_records() {
        let mut bytes = valid_file();
        let vertex_header = 4 + LumpKind::Vertices as usize * 8;
        bytes.0[vertex_header + 4..vertex_header + 8].copy_from_slice(&1i32.to_le_bytes());
        bytes.0.insert(vertex_header + 8, 0);
        assert_eq!(
            PsbIndex::read(&mut bytes),
            Err(PsbError::MisalignedLump {
                kind: LumpKind::Vertices,
                size: 1,
                record_size: 12,
            })
        );
    }

    #[test]
    fn rejects_truncated_payloads_before_reading_them() {
        let mut bytes = valid_file();
        bytes.0[8..12].copy_from_slice(&4096i32.to_le_bytes());
        assert!(matches!(
            PsbIndex::read(&mut bytes),
            Err(PsbError::Truncated { .. })
        ));
    }

    #[test]
    fn decodes_packed_node_without_native_layout_assumptions() {
        let mut bytes = [0u8; Node::SIZE];
        bytes[0..2].copy_from_slice(&7u16.to_le_bytes());
        bytes[2..4].copy_from_slice(&3i16.to_le_bytes());
        bytes[4..6].copy_from_slice(&(-5i16).to_le_bytes());
        bytes[6..9].copy_from_slice(&[-128i8 as u8, -2i8 as u8, 3]);
        bytes[9..12].copy_from_slice(&[0, 4, 127]);
        bytes[12..14].copy_from_slice(&11u16.to_le_bytes());
        bytes[14..16].copy_from_slice(&3u16.to_le_bytes());

        let node = RecordSlice::<Node>::new(&bytes).unwrap().get(0).unwrap();
        assert_eq!(node.plane, 7);
        assert_eq!(node.children, [3, -5]);
        assert_eq!(
            node.mins,
            Vec3I16 {
                x: i16::MIN,
                y: -16,
                z: 24
            }
        );
        assert_eq!(
            node.maxs,
            Vec3I16 {
                x: 0,
                y: 32,
                z: i16::MAX
            }
        );
        assert_eq!(node.first_face, 11);
        assert_eq!(node.face_count, 3);
    }

    #[test]
    fn node_bound_quantization_is_conservative_across_the_i16_domain() {
        for value in [
            i16::MIN,
            -32_767,
            -1_025,
            -1_024,
            -1_017,
            -1_016,
            -257,
            -256,
            -255,
            -9,
            -8,
            -7,
            -1,
            0,
            1,
            7,
            8,
            9,
            255,
            256,
            257,
            1_008,
            1_009,
            1_016,
            32_512,
            32_513,
            i16::MAX,
        ] {
            let decoded_min = decode_node_bound_min(encode_node_bound_min(value));
            let decoded_max = decode_node_bound_max(encode_node_bound_max(value));
            assert!(decoded_min <= value, "min {decoded_min} excludes {value}");
            assert!(decoded_max >= value, "max {decoded_max} excludes {value}");
        }
        assert_eq!(decode_node_bound_min(i8::MIN), i16::MIN);
        assert_eq!(decode_node_bound_max(i8::MAX), i16::MAX);
    }

    #[test]
    fn borrows_aligned_clip_nodes_without_decoding_or_copying() {
        #[repr(align(2))]
        struct Aligned([u8; ClipNode::SIZE]);

        let mut bytes = Aligned([0; ClipNode::SIZE]);
        bytes.0[0..2].copy_from_slice(&7i16.to_le_bytes());
        bytes.0[2..4].copy_from_slice(&3i16.to_le_bytes());
        bytes.0[4..6].copy_from_slice(&(-5i16).to_le_bytes());
        let packed = RecordSlice::<ClipNode>::new(&bytes.0).unwrap();
        let native = packed.as_native_clip_nodes().unwrap();
        assert_eq!(
            native,
            &[ClipNode {
                plane: 7,
                children: [3, -5]
            }]
        );
        assert_eq!(native.as_ptr().cast::<u8>(), bytes.0.as_ptr());
    }

    #[test]
    fn rejects_partial_record_slices() {
        assert!(RecordSlice::<MapEntity>::new(&[0; MapEntity::SIZE - 1]).is_none());
    }

    #[test]
    fn indexes_checked_alias_model_slices_without_copying() {
        let bytes = one_alias_model();
        let table = AliasModelTable::new(&bytes).unwrap();
        assert_eq!(table.len(), 1);
        let model = table.get(0x49).unwrap();
        let header = model.header();
        assert_eq!(header.flags, 8);
        assert_eq!(header.vertex_count, 3);
        assert_eq!(header.skins[0].texture_page, 0x1234);
        assert_eq!(header.skins[0].base, Vec2U8 { x: 17, y: 29 });
        assert_eq!(
            model.triangles(0).unwrap().get(0).unwrap().corners,
            [0x0000_0201, 0x0008_0403, 0x0010_0605]
        );
        assert_eq!(
            model.frame_vertices(0).unwrap().get(2).unwrap().position,
            [7, 8, 9]
        );
        assert!(model.triangles(1).is_none());
        assert!(model.frame_vertices(1).is_none());
    }

    #[test]
    fn rejects_alias_faces_with_invalid_projected_offsets() {
        let mut bytes = one_alias_model();
        let face = 4 + AliasModelHeader::SIZE;
        bytes[face + 2..face + 4].copy_from_slice(&7u16.to_le_bytes());
        assert_eq!(
            AliasModelTable::new(&bytes).err(),
            Some(AliasModelError::BadProjectedOffset {
                model: 0,
                triangle: 0,
                corner: 0,
                offset: 7,
            })
        );
    }

    #[test]
    fn rejects_alias_frame_offsets_that_do_not_follow_skin_triangles() {
        let mut bytes = one_alias_model();
        let frame_offset = 4 + 64;
        bytes[frame_offset..frame_offset + 4].copy_from_slice(&16u32.to_le_bytes());
        assert_eq!(
            AliasModelTable::new(&bytes).err(),
            Some(AliasModelError::BadFrameLayout { model: 0 })
        );
    }
}
