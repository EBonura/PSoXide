#![no_std]

//! Checked, allocation-free readers for the XBSP cooked map format.
//!
//! Shared by the guest runtime, the editor cook and quake-psx so the wire
//! format has one Rust definition (docs/quake-bsp-migration-plan.md, P0).
//! Derived from quake-psx `crates/quake-formats` (GPL-2, same authorship);
//! the PXBSP extensions (material lump, PSoXide entity records, streaming
//! index) will grow here.
// ponytail: verbatim-copy provenance; field docs land as PXBSP diverges,
// then this allow goes away.
#![allow(missing_docs)]

pub mod collision;

use core::fmt;
use core::marker::PhantomData;

/// `PSX%` as emitted by the original Quake PSX map cooker.
pub const PSB_MAGIC: u32 = 0x2558_5350;
pub const PSB_HEADER_BYTES: u32 = 4;
pub const LUMP_HEADER_BYTES: u32 = 8;
pub const LUMP_COUNT: usize = 15;

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

    pub const fn record_size(self) -> Option<u32> {
        match self {
            Self::TextureData | Self::SoundData | Self::ModelData | Self::Visibility => None,
            Self::Vertices => Some(12),
            Self::Planes => Some(14),
            Self::TextureInfo => Some(14),
            Self::Faces => Some(14),
            Self::MarkSurfaces => Some(2),
            Self::Leaves => Some(26),
            Self::Nodes => Some(34),
            Self::ClipNodes => Some(6),
            Self::Models => Some(32),
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

    pub const fn record_count(self, kind: LumpKind) -> Option<u32> {
        match kind.record_size() {
            Some(size) => Some(self.len / size),
            None => None,
        }
    }
}

/// Minimal random-access input used by both CD streaming and host files.
pub trait ReadAt {
    type Error;

    fn len(&self) -> u32;
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
        if magic != PSB_MAGIC {
            return Err(PsbError::BadMagic { found: magic });
        }

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
            if let Some(record_size) = expected.record_size() {
                if size % record_size != 0 {
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
        Ok(Self { file_len, lumps })
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
        if bytes.len() % T::SIZE != 0 {
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

    pub fn get(self, index: usize) -> Option<T> {
        let start = index.checked_mul(T::SIZE)?;
        Some(T::decode(self.bytes.get(start..start + T::SIZE)?))
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

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Plane {
    /// Q3.12 unit normal.
    pub normal: Vec3I16,
    /// Q20.12 distance from the origin.
    pub distance: i32,
    pub kind: i32,
}

impl CookedRecord for Plane {
    const SIZE: usize = 14;

    fn decode(bytes: &[u8]) -> Self {
        Self {
            normal: vec3_i16(bytes, 0),
            distance: i32_at(bytes, 6),
            kind: i32_at(bytes, 10),
        }
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
    pub plane: i16,
    pub flags: u16,
    pub first_vertex: i32,
    pub vertex_count: i16,
    pub texture: i16,
    pub light_styles: [u8; 2],
}

impl CookedRecord for Face {
    const SIZE: usize = 14;

    fn decode(bytes: &[u8]) -> Self {
        Self {
            plane: i16_at(bytes, 0),
            flags: u16_at(bytes, 2),
            first_vertex: i32_at(bytes, 4),
            vertex_count: i16_at(bytes, 8),
            texture: i16_at(bytes, 10),
            light_styles: [bytes[12], bytes[13]],
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Leaf {
    pub contents: i16,
    pub visibility_offset: i32,
    pub mins: Vec3I16,
    pub maxs: Vec3I16,
    pub first_mark_surface: u16,
    pub mark_surface_count: u16,
    pub lightmap: [u8; 2],
    pub light_styles: [u8; 2],
}

impl CookedRecord for Leaf {
    const SIZE: usize = 26;

    fn decode(bytes: &[u8]) -> Self {
        Self {
            contents: i16_at(bytes, 0),
            visibility_offset: i32_at(bytes, 2),
            mins: vec3_i16(bytes, 6),
            maxs: vec3_i16(bytes, 12),
            first_mark_surface: u16_at(bytes, 18),
            mark_surface_count: u16_at(bytes, 20),
            lightmap: [bytes[22], bytes[23]],
            light_styles: [bytes[24], bytes[25]],
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Node {
    pub plane: u16,
    /// Negative values encode `-(leaf + 1)`.
    pub children: [i16; 2],
    pub mins: Vec3I16,
    pub maxs: Vec3I16,
    pub surface_mins: Vec3I16,
    pub surface_maxs: Vec3I16,
    pub first_face: u16,
    pub face_count: u16,
}

impl CookedRecord for Node {
    const SIZE: usize = 34;

    fn decode(bytes: &[u8]) -> Self {
        Self {
            plane: u16_at(bytes, 0),
            children: [i16_at(bytes, 2), i16_at(bytes, 4)],
            mins: vec3_i16(bytes, 6),
            maxs: vec3_i16(bytes, 12),
            surface_mins: vec3_i16(bytes, 18),
            surface_maxs: vec3_i16(bytes, 24),
            first_face: u16_at(bytes, 30),
            face_count: u16_at(bytes, 32),
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ClipNode {
    pub plane: i16,
    pub children: [i16; 2],
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
    pub origin: Vec3I16,
    pub head_nodes: [i16; 4],
    pub visible_leaves: i16,
    pub first_face: u16,
    pub face_count: u16,
}

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
        let mut bytes = Vec::from(PSB_MAGIC.to_le_bytes());
        for kind in LumpKind::ALL {
            bytes.extend_from_slice(&(kind as i32).to_le_bytes());
            bytes.extend_from_slice(&0i32.to_le_bytes());
        }
        Bytes(bytes)
    }

    #[test]
    fn indexes_all_lumps_without_allocating_map_data() {
        let mut bytes = valid_file();
        let index = PsbIndex::read(&mut bytes).unwrap();
        assert_eq!(index.file_len(), 4 + LUMP_COUNT as u32 * 8);
        assert_eq!(index.lump(LumpKind::TextureData).offset, 12);
        assert_eq!(index.lump(LumpKind::Entities).len, 0);
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
        bytes[6..8].copy_from_slice(&(-64i16).to_le_bytes());
        bytes[8..10].copy_from_slice(&32i16.to_le_bytes());
        bytes[10..12].copy_from_slice(&96i16.to_le_bytes());
        bytes[30..32].copy_from_slice(&120u16.to_le_bytes());
        bytes[32..34].copy_from_slice(&9u16.to_le_bytes());

        let node = RecordSlice::<Node>::new(&bytes).unwrap().get(0).unwrap();
        assert_eq!(node.plane, 7);
        assert_eq!(node.children, [3, -5]);
        assert_eq!(
            node.mins,
            Vec3I16 {
                x: -64,
                y: 32,
                z: 96
            }
        );
        assert_eq!(node.first_face, 120);
        assert_eq!(node.face_count, 9);
    }

    #[test]
    fn rejects_partial_record_slices() {
        assert!(RecordSlice::<MapEntity>::new(&[0; MapEntity::SIZE - 1]).is_none());
    }
}
