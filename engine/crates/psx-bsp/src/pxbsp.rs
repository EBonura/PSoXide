//! Checked directory reader for PSoXide's versioned PXBSP container.

use core::fmt;

use crate::{CookedRecord, LumpRange, ReadAt, Vec3I16, Vec3I32};

/// `PXB%` in little-endian byte order.
pub const PXBSP_MAGIC: u32 = 0x2542_5850;
pub const PXBSP_VERSION_V1: u16 = 1;
/// Unsupported experimental compact layout with ten-byte plane records.
pub const PXBSP_VERSION_V2: u16 = 2;
pub const PXBSP_VERSION: u16 = 3;
pub const PXBSP_HEADER_BYTES: u32 = 8;
pub const PXBSP_DIRECTORY_ENTRY_BYTES: u32 = 12;
pub const PXBSP_LUMP_COUNT: usize = 16;
/// Maximum decompressed PVS row supported by the resident PS1 runtime.
pub const PXBSP_MAX_VISIBILITY_BYTES: usize = 1024;

/// Recognized PXBSP physical record layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PxbspVersion {
    V1,
    V3,
}

impl PxbspVersion {
    pub const fn wire(self) -> u16 {
        match self {
            Self::V1 => PXBSP_VERSION_V1,
            Self::V3 => PXBSP_VERSION,
        }
    }

    const fn from_wire(value: u16) -> Option<Self> {
        match value {
            PXBSP_VERSION_V1 => Some(Self::V1),
            PXBSP_VERSION => Some(Self::V3),
            _ => None,
        }
    }
}

/// Decompress one leaf's PVS row into caller-owned bytes.
///
/// The canonical row semantics shared by every resident map format: the row
/// spans `visible_leaves.div_ceil(8)` bytes, an oversize row or malformed
/// run-length data returns `None` without allocating, and success returns the
/// addressable visible-leaf count.
pub(crate) fn decompress_leaf_row(
    visibility: &[u8],
    offset: usize,
    visible_leaves: usize,
    output: &mut [u8],
) -> Option<usize> {
    let row_bytes = visible_leaves.div_ceil(8);
    if row_bytes > output.len() {
        return None;
    }
    output[..row_bytes].fill(0);
    decompress_visibility(visibility, offset, &mut output[..row_bytes]).then_some(visible_leaves)
}

pub(crate) fn decompress_visibility(input: &[u8], offset: usize, output: &mut [u8]) -> bool {
    let mut source = offset;
    let mut destination = 0usize;
    while destination < output.len() {
        let Some(&value) = input.get(source) else {
            return false;
        };
        source += 1;
        if value != 0 {
            output[destination] = value;
            destination += 1;
            continue;
        }
        let Some(&run) = input.get(source) else {
            return false;
        };
        source += 1;
        if run == 0 || destination + run as usize > output.len() {
            return false;
        }
        output[destination..destination + run as usize].fill(0);
        destination += run as usize;
    }
    true
}

/// One required PXBSP payload kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum PxbspLumpKind {
    TextureData = 0,
    SoundData = 1,
    ModelData = 2,
    Vertices = 3,
    Planes = 4,
    Materials = 5,
    Faces = 6,
    MarkSurfaces = 7,
    Visibility = 8,
    Leaves = 9,
    Nodes = 10,
    ClipNodes = 11,
    Models = 12,
    Strings = 13,
    Entities = 14,
    StreamingIndex = 15,
}

impl PxbspLumpKind {
    pub const ALL: [Self; PXBSP_LUMP_COUNT] = [
        Self::TextureData,
        Self::SoundData,
        Self::ModelData,
        Self::Vertices,
        Self::Planes,
        Self::Materials,
        Self::Faces,
        Self::MarkSurfaces,
        Self::Visibility,
        Self::Leaves,
        Self::Nodes,
        Self::ClipNodes,
        Self::Models,
        Self::Strings,
        Self::Entities,
        Self::StreamingIndex,
    ];

    pub const fn record_size(self, version: PxbspVersion) -> Option<u32> {
        match self {
            Self::Vertices => Some(12),
            Self::Planes => Some(match version {
                PxbspVersion::V1 => 14,
                PxbspVersion::V3 => crate::Plane::SIZE as u32,
            }),
            Self::Materials => Some(PxbspMaterial::SIZE as u32),
            Self::Faces => Some(match version {
                PxbspVersion::V1 => 14,
                PxbspVersion::V3 => 10,
            }),
            Self::MarkSurfaces => Some(2),
            Self::Leaves => Some(match version {
                PxbspVersion::V1 => 26,
                PxbspVersion::V3 => 10,
            }),
            Self::Nodes => Some(match version {
                PxbspVersion::V1 => 34,
                PxbspVersion::V3 => 6,
            }),
            Self::ClipNodes => Some(6),
            Self::Models => Some(match version {
                PxbspVersion::V1 => 32,
                PxbspVersion::V3 => crate::BrushModel::SIZE as u32,
            }),
            Self::Strings => Some(1),
            Self::TextureData
            | Self::SoundData
            | Self::ModelData
            | Self::Visibility
            | Self::Entities
            | Self::StreamingIndex => None,
        }
    }
}

/// World-material slot resolved through PSoXide's asset table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PxbspMaterial {
    pub texture_asset: u16,
    pub flags: u16,
    pub tint: [u8; 3],
    pub blend_mode: u8,
    pub animation_kind: u8,
    /// Kind-specific fixed payload. UV scroll and flipbook recipes fit here.
    pub animation_data: [u8; 7],
}

/// World-material face policy stored in [`PxbspMaterial::flags`].
pub mod material_flags {
    /// Bits reserved for face sidedness.
    pub const FACE_MASK: u16 = 0x0003;
    /// Draw the authored front face only.
    pub const FACE_FRONT: u16 = 0x0000;
    /// Draw the authored back face only.
    pub const FACE_BACK: u16 = 0x0001;
    /// Draw both sides of the authored face.
    pub const FACE_BOTH: u16 = 0x0002;
    /// All flags understood by PXBSP version one.
    pub const KNOWN: u16 = FACE_MASK;
}

/// PSoXide material blend codes stored in [`PxbspMaterial::blend_mode`].
pub mod material_blend {
    /// Opaque textured drawing.
    pub const OPAQUE: u8 = 0;
    /// Half-background plus half-foreground blending.
    pub const AVERAGE: u8 = 1;
    /// Add foreground to background.
    pub const ADD: u8 = 2;
    /// Subtract foreground from background.
    pub const SUBTRACT: u8 = 3;
    /// Add one quarter of the foreground to the background.
    pub const ADD_QUARTER: u8 = 4;
}

/// Decoded one-pass animation for a PXBSP material.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PxbspMaterialAnimation {
    Static,
    UvScroll {
        speed_u_q8: i16,
        speed_v_q8: i16,
        phase_u: u8,
        phase_v: u8,
    },
    Flipbook {
        columns: u8,
        rows: u8,
        frame_count: u8,
        ticks_per_frame: u8,
        phase: u8,
    },
}

/// Reason a packed material recipe cannot be submitted safely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PxbspMaterialError {
    /// The flags contain bits not assigned by this format version.
    UnknownFlags(u16),
    /// The sidedness field uses its reserved bit pattern.
    InvalidSidedness(u16),
    /// The blend mode is not a supported PlayStation GPU recipe.
    InvalidBlendMode(u8),
    /// The animation kind is not assigned by this format version.
    UnknownAnimation(u8),
    /// The selected animation kind has malformed parameters.
    InvalidAnimationPayload(u8),
}

impl CookedRecord for PxbspMaterial {
    const SIZE: usize = 16;

    fn decode(bytes: &[u8]) -> Self {
        Self {
            texture_asset: u16::from_le_bytes([bytes[0], bytes[1]]),
            flags: u16::from_le_bytes([bytes[2], bytes[3]]),
            tint: [bytes[4], bytes[5], bytes[6]],
            blend_mode: bytes[7],
            animation_kind: bytes[8],
            animation_data: bytes[9..16].try_into().unwrap(),
        }
    }
}

impl PxbspMaterial {
    /// Validate every versioned flag, blend, and animation field.
    pub fn validate(self) -> Result<(), PxbspMaterialError> {
        if self.flags & !material_flags::KNOWN != 0 {
            return Err(PxbspMaterialError::UnknownFlags(self.flags));
        }
        if self.flags & material_flags::FACE_MASK == material_flags::FACE_MASK {
            return Err(PxbspMaterialError::InvalidSidedness(self.flags));
        }
        if self.blend_mode > material_blend::ADD_QUARTER {
            return Err(PxbspMaterialError::InvalidBlendMode(self.blend_mode));
        }
        self.animation().map(|_| ())
    }

    /// Decode the fixed-size animation payload into its typed recipe.
    pub fn animation(self) -> Result<PxbspMaterialAnimation, PxbspMaterialError> {
        match self.animation_kind {
            material_animation::STATIC if self.animation_data == [0; 7] => {
                Ok(PxbspMaterialAnimation::Static)
            }
            material_animation::UV_SCROLL if self.animation_data[6] == 0 => {
                Ok(PxbspMaterialAnimation::UvScroll {
                    speed_u_q8: i16::from_le_bytes([
                        self.animation_data[0],
                        self.animation_data[1],
                    ]),
                    speed_v_q8: i16::from_le_bytes([
                        self.animation_data[2],
                        self.animation_data[3],
                    ]),
                    phase_u: self.animation_data[4],
                    phase_v: self.animation_data[5],
                })
            }
            material_animation::FLIPBOOK
                if self.animation_data[0] > 0
                    && self.animation_data[1] > 0
                    && self.animation_data[2] > 0
                    && self.animation_data[3] > 0
                    && self.animation_data[2]
                        <= self.animation_data[0].saturating_mul(self.animation_data[1])
                    && self.animation_data[5..] == [0; 2] =>
            {
                Ok(PxbspMaterialAnimation::Flipbook {
                    columns: self.animation_data[0],
                    rows: self.animation_data[1],
                    frame_count: self.animation_data[2],
                    ticks_per_frame: self.animation_data[3],
                    phase: self.animation_data[4],
                })
            }
            material_animation::STATIC
            | material_animation::UV_SCROLL
            | material_animation::FLIPBOOK => Err(PxbspMaterialError::InvalidAnimationPayload(
                self.animation_kind,
            )),
            other => Err(PxbspMaterialError::UnknownAnimation(other)),
        }
    }
}

pub mod material_animation {
    pub const STATIC: u8 = 0;
    pub const UV_SCROLL: u8 = 1;
    pub const FLIPBOOK: u8 = 2;
}

/// Stable PSoXide entity classes stored in [`PxbspEntity::class_id`].
pub mod entity_class {
    /// A translated brush submodel driven between closed and open endpoints.
    pub const BRUSH_DOOR: u16 = 1;
    /// Authored world-space entry point for the player character.
    pub const PLAYER_SPAWN: u16 = 2;
}

/// Common flags stored in [`PxbspEntity::flags`].
pub mod entity_flags {
    /// Entity participates in gameplay.
    pub const ENABLED: u16 = 1 << 0;
    /// A brush door begins at its open endpoint.
    pub const START_OPEN: u16 = 1 << 1;
    /// Flags assigned by PXBSP version one.
    pub const KNOWN: u16 = ENABLED | START_OPEN;
}

/// Fixed world-space base shared by every PSoXide entity class.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PxbspEntity {
    pub class_id: u16,
    pub flags: u16,
    /// Brush or skeletal model index, or `u16::MAX` when absent.
    pub model: u16,
    /// Runtime empty-leaf index used for PVS activation.
    pub leaf: u16,
    /// Q20.12 world position.
    pub origin: Vec3I32,
    /// Q0.12 turn angles.
    pub angles: Vec3I16,
    /// Byte offset from the entity table's payload start.
    pub payload_offset: u32,
    pub payload_size: u16,
}

impl CookedRecord for PxbspEntity {
    const SIZE: usize = 32;

    fn decode(bytes: &[u8]) -> Self {
        Self {
            class_id: u16::from_le_bytes([bytes[0], bytes[1]]),
            flags: u16::from_le_bytes([bytes[2], bytes[3]]),
            model: u16::from_le_bytes([bytes[4], bytes[5]]),
            leaf: u16::from_le_bytes([bytes[6], bytes[7]]),
            origin: Vec3I32 {
                x: i32::from_le_bytes(bytes[8..12].try_into().unwrap()),
                y: i32::from_le_bytes(bytes[12..16].try_into().unwrap()),
                z: i32::from_le_bytes(bytes[16..20].try_into().unwrap()),
            },
            angles: Vec3I16 {
                x: i16::from_le_bytes(bytes[20..22].try_into().unwrap()),
                y: i16::from_le_bytes(bytes[22..24].try_into().unwrap()),
                z: i16::from_le_bytes(bytes[24..26].try_into().unwrap()),
            },
            payload_offset: u32::from_le_bytes(bytes[26..30].try_into().unwrap()),
            payload_size: u16::from_le_bytes([bytes[30], bytes[31]]),
        }
    }
}

/// Fixed payload for [`entity_class::BRUSH_DOOR`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PxbspBrushDoor {
    /// Q20.12 translation from the closed endpoint to the open endpoint.
    pub open_offset: Vec3I32,
    /// Number of fixed 60 Hz simulation ticks between endpoints.
    pub travel_ticks: u16,
    reserved: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PxbspBrushDoorError {
    ZeroOpenOffset,
    ZeroTravelTicks,
    NonZeroReserved(u16),
}

impl PxbspBrushDoor {
    pub const fn new(open_offset: Vec3I32, travel_ticks: u16) -> Self {
        Self {
            open_offset,
            travel_ticks,
            reserved: 0,
        }
    }

    pub fn validate(self) -> Result<(), PxbspBrushDoorError> {
        if self.open_offset == (Vec3I32 { x: 0, y: 0, z: 0 }) {
            return Err(PxbspBrushDoorError::ZeroOpenOffset);
        }
        if self.travel_ticks == 0 {
            return Err(PxbspBrushDoorError::ZeroTravelTicks);
        }
        if self.reserved != 0 {
            return Err(PxbspBrushDoorError::NonZeroReserved(self.reserved));
        }
        Ok(())
    }

    pub fn to_le_bytes(self) -> [u8; Self::SIZE] {
        let mut bytes = [0; Self::SIZE];
        bytes[0..4].copy_from_slice(&self.open_offset.x.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.open_offset.y.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.open_offset.z.to_le_bytes());
        bytes[12..14].copy_from_slice(&self.travel_ticks.to_le_bytes());
        bytes[14..16].copy_from_slice(&self.reserved.to_le_bytes());
        bytes
    }
}

impl CookedRecord for PxbspBrushDoor {
    const SIZE: usize = 16;

    fn decode(bytes: &[u8]) -> Self {
        Self {
            open_offset: Vec3I32 {
                x: i32::from_le_bytes(bytes[0..4].try_into().unwrap()),
                y: i32::from_le_bytes(bytes[4..8].try_into().unwrap()),
                z: i32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            },
            travel_ticks: u16::from_le_bytes([bytes[12], bytes[13]]),
            reserved: u16::from_le_bytes([bytes[14], bytes[15]]),
        }
    }
}

pub const PXBSP_ENTITY_TABLE_HEADER_BYTES: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PxbspEntityTableError {
    TooSmall,
    WrongRecordSize(u16),
    BadPayloadOffset(u32),
    TruncatedRecords,
    BadPayloadRange(usize),
}

/// Checked zero-copy view of the variable-payload entity lump.
#[derive(Clone, Copy)]
pub struct PxbspEntityTable<'a> {
    bytes: &'a [u8],
    count: usize,
    payload_offset: usize,
}

impl<'a> PxbspEntityTable<'a> {
    pub fn new(bytes: &'a [u8]) -> Result<Self, PxbspEntityTableError> {
        if bytes.len() < PXBSP_ENTITY_TABLE_HEADER_BYTES {
            return Err(PxbspEntityTableError::TooSmall);
        }
        let count = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
        let record_size = u16::from_le_bytes([bytes[2], bytes[3]]);
        if record_size as usize != PxbspEntity::SIZE {
            return Err(PxbspEntityTableError::WrongRecordSize(record_size));
        }
        let payload_offset = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let records_end = PXBSP_ENTITY_TABLE_HEADER_BYTES
            .checked_add(
                count
                    .checked_mul(PxbspEntity::SIZE)
                    .ok_or(PxbspEntityTableError::TruncatedRecords)?,
            )
            .ok_or(PxbspEntityTableError::TruncatedRecords)?;
        let expected_payload = (records_end + 3) & !3;
        if payload_offset != expected_payload || payload_offset > bytes.len() {
            return Err(PxbspEntityTableError::BadPayloadOffset(
                payload_offset as u32,
            ));
        }
        if records_end > bytes.len() {
            return Err(PxbspEntityTableError::TruncatedRecords);
        }
        let table = Self {
            bytes,
            count,
            payload_offset,
        };
        let payload_len = bytes.len() - payload_offset;
        for index in 0..count {
            let entity = table.get(index).expect("index is in table");
            let start = entity.payload_offset as usize;
            let end = start
                .checked_add(entity.payload_size as usize)
                .ok_or(PxbspEntityTableError::BadPayloadRange(index))?;
            if end > payload_len {
                return Err(PxbspEntityTableError::BadPayloadRange(index));
            }
        }
        Ok(table)
    }

    pub const fn len(self) -> usize {
        self.count
    }

    pub const fn is_empty(self) -> bool {
        self.count == 0
    }

    pub fn get(self, index: usize) -> Option<PxbspEntity> {
        if index >= self.count {
            return None;
        }
        let start = PXBSP_ENTITY_TABLE_HEADER_BYTES + index * PxbspEntity::SIZE;
        Some(PxbspEntity::decode(
            &self.bytes[start..start + PxbspEntity::SIZE],
        ))
    }

    pub fn payload(self, index: usize) -> Option<&'a [u8]> {
        let entity = self.get(index)?;
        let start = self.payload_offset + entity.payload_offset as usize;
        let end = start + entity.payload_size as usize;
        self.bytes.get(start..end)
    }

    /// Decode one fixed-size class payload without copying its table storage.
    pub fn payload_record<T: CookedRecord>(self, index: usize) -> Option<T> {
        let payload = self.payload(index)?;
        (payload.len() == T::SIZE).then(|| T::decode(payload))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PxbspError<E> {
    Read(E),
    TooSmall {
        len: u32,
    },
    BadMagic {
        found: u32,
    },
    BadVersion {
        found: u16,
    },
    BadLumpCount {
        found: u16,
    },
    Truncated {
        offset: u32,
        needed: u32,
        len: u32,
    },
    WrongLump {
        expected: PxbspLumpKind,
        found: u16,
    },
    UnsupportedFlags {
        kind: PxbspLumpKind,
        flags: u16,
    },
    MisalignedOffset {
        kind: PxbspLumpKind,
        offset: u32,
    },
    DirectoryOverlap {
        kind: PxbspLumpKind,
        offset: u32,
        directory_end: u32,
    },
    OutOfOrderLump {
        kind: PxbspLumpKind,
        offset: u32,
        previous_end: u32,
    },
    MisalignedLump {
        kind: PxbspLumpKind,
        size: u32,
        record_size: u32,
    },
    TrailingData {
        parsed: u32,
        len: u32,
    },
}

impl<E: fmt::Display> fmt::Display for PxbspError<E> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(error) => write!(output, "read failed: {error}"),
            Self::TooSmall { len } => write!(output, "PXBSP is only {len} bytes"),
            Self::BadMagic { found } => write!(output, "bad PXBSP magic {found:#010x}"),
            Self::BadVersion { found } => write!(output, "unsupported PXBSP version {found}"),
            Self::BadLumpCount { found } => write!(output, "PXBSP has {found} lump entries"),
            Self::Truncated {
                offset,
                needed,
                len,
            } => write!(
                output,
                "PXBSP range {offset}..{} exceeds file length {len}",
                offset.saturating_add(*needed)
            ),
            Self::WrongLump { expected, found } => {
                write!(output, "expected lump {expected:?}, found type {found}")
            }
            Self::UnsupportedFlags { kind, flags } => {
                write!(output, "lump {kind:?} has unsupported flags {flags:#06x}")
            }
            Self::MisalignedOffset { kind, offset } => {
                write!(
                    output,
                    "lump {kind:?} offset {offset} is not four-byte aligned"
                )
            }
            Self::DirectoryOverlap {
                kind,
                offset,
                directory_end,
            } => write!(
                output,
                "lump {kind:?} offset {offset} overlaps directory ending at {directory_end}"
            ),
            Self::OutOfOrderLump {
                kind,
                offset,
                previous_end,
            } => write!(
                output,
                "lump {kind:?} offset {offset} precedes prior lump end {previous_end}"
            ),
            Self::MisalignedLump {
                kind,
                size,
                record_size,
            } => write!(
                output,
                "lump {kind:?} size {size} is not a multiple of {record_size}"
            ),
            Self::TrailingData { parsed, len } => {
                write!(
                    output,
                    "PXBSP payload ended at {parsed}, file length is {len}"
                )
            }
        }
    }
}

/// Validated random-access ranges for one PXBSP file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PxbspIndex {
    version: PxbspVersion,
    file_len: u32,
    lumps: [LumpRange; PXBSP_LUMP_COUNT],
}

impl PxbspIndex {
    pub fn read<R: ReadAt>(reader: &mut R) -> Result<Self, PxbspError<R::Error>> {
        let file_len = reader.len();
        if file_len < PXBSP_HEADER_BYTES {
            return Err(PxbspError::TooSmall { len: file_len });
        }
        let mut header = [0u8; PXBSP_HEADER_BYTES as usize];
        reader
            .read_exact_at(0, &mut header)
            .map_err(PxbspError::Read)?;
        let magic = u32::from_le_bytes(header[0..4].try_into().unwrap());
        if magic != PXBSP_MAGIC {
            return Err(PxbspError::BadMagic { found: magic });
        }
        let found_version = u16::from_le_bytes(header[4..6].try_into().unwrap());
        let version = PxbspVersion::from_wire(found_version).ok_or(PxbspError::BadVersion {
            found: found_version,
        })?;
        let lump_count = u16::from_le_bytes(header[6..8].try_into().unwrap());
        if lump_count as usize != PXBSP_LUMP_COUNT {
            return Err(PxbspError::BadLumpCount { found: lump_count });
        }

        let directory_bytes = PXBSP_DIRECTORY_ENTRY_BYTES * PXBSP_LUMP_COUNT as u32;
        let directory_end = PXBSP_HEADER_BYTES + directory_bytes;
        checked_range(PXBSP_HEADER_BYTES, directory_bytes, file_len)?;
        let mut lumps = [LumpRange::EMPTY; PXBSP_LUMP_COUNT];
        let mut previous_end = directory_end;
        let mut entry = [0u8; PXBSP_DIRECTORY_ENTRY_BYTES as usize];
        for (index, expected) in PxbspLumpKind::ALL.into_iter().enumerate() {
            let entry_offset = PXBSP_HEADER_BYTES + index as u32 * PXBSP_DIRECTORY_ENTRY_BYTES;
            reader
                .read_exact_at(entry_offset, &mut entry)
                .map_err(PxbspError::Read)?;
            let found = u16::from_le_bytes(entry[0..2].try_into().unwrap());
            if found != expected as u16 {
                return Err(PxbspError::WrongLump { expected, found });
            }
            let flags = u16::from_le_bytes(entry[2..4].try_into().unwrap());
            if flags != 0 {
                return Err(PxbspError::UnsupportedFlags {
                    kind: expected,
                    flags,
                });
            }
            let offset = u32::from_le_bytes(entry[4..8].try_into().unwrap());
            let len = u32::from_le_bytes(entry[8..12].try_into().unwrap());
            if offset & 3 != 0 {
                return Err(PxbspError::MisalignedOffset {
                    kind: expected,
                    offset,
                });
            }
            if offset < directory_end {
                return Err(PxbspError::DirectoryOverlap {
                    kind: expected,
                    offset,
                    directory_end,
                });
            }
            if offset < previous_end {
                return Err(PxbspError::OutOfOrderLump {
                    kind: expected,
                    offset,
                    previous_end,
                });
            }
            if let Some(record_size) = expected.record_size(version) {
                if len % record_size != 0 {
                    return Err(PxbspError::MisalignedLump {
                        kind: expected,
                        size: len,
                        record_size,
                    });
                }
            }
            checked_range(offset, len, file_len)?;
            lumps[index] = LumpRange { offset, len };
            previous_end = offset + len;
        }
        if previous_end != file_len {
            return Err(PxbspError::TrailingData {
                parsed: previous_end,
                len: file_len,
            });
        }
        Ok(Self {
            version,
            file_len,
            lumps,
        })
    }

    pub const fn version(&self) -> PxbspVersion {
        self.version
    }

    pub const fn file_len(&self) -> u32 {
        self.file_len
    }

    pub const fn lump(&self, kind: PxbspLumpKind) -> LumpRange {
        self.lumps[kind as usize]
    }
}

fn checked_range<E>(offset: u32, needed: u32, len: u32) -> Result<(), PxbspError<E>> {
    if offset > len || needed > len - offset {
        Err(PxbspError::Truncated {
            offset,
            needed,
            len,
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;
    use crate::SliceReader;

    fn aligned(value: usize) -> usize {
        (value + 3) & !3
    }

    fn valid_file() -> Vec<u8> {
        let directory_end =
            PXBSP_HEADER_BYTES as usize + PXBSP_DIRECTORY_ENTRY_BYTES as usize * PXBSP_LUMP_COUNT;
        let payload_sizes = [0usize, 0, 0, 12, 14, 0, 10, 2, 1, 10, 6, 6, 26, 1, 0, 0];
        let mut offsets = [0usize; PXBSP_LUMP_COUNT];
        let mut cursor = aligned(directory_end);
        for (index, size) in payload_sizes.into_iter().enumerate() {
            offsets[index] = cursor;
            cursor = aligned(cursor + size);
        }
        let last = PXBSP_LUMP_COUNT - 1;
        let file_len = offsets[last] + payload_sizes[last];
        let mut bytes = vec![0; file_len];
        bytes[0..4].copy_from_slice(&PXBSP_MAGIC.to_le_bytes());
        bytes[4..6].copy_from_slice(&PXBSP_VERSION.to_le_bytes());
        bytes[6..8].copy_from_slice(&(PXBSP_LUMP_COUNT as u16).to_le_bytes());
        for (index, kind) in PxbspLumpKind::ALL.into_iter().enumerate() {
            let start = PXBSP_HEADER_BYTES as usize + index * PXBSP_DIRECTORY_ENTRY_BYTES as usize;
            bytes[start..start + 2].copy_from_slice(&(kind as u16).to_le_bytes());
            bytes[start + 4..start + 8].copy_from_slice(&(offsets[index] as u32).to_le_bytes());
            bytes[start + 8..start + 12]
                .copy_from_slice(&(payload_sizes[index] as u32).to_le_bytes());
        }
        bytes
    }

    #[test]
    fn reads_versioned_aligned_lump_directory() {
        let bytes = valid_file();
        let index = PxbspIndex::read(&mut SliceReader::new(&bytes)).expect("index");
        assert_eq!(index.file_len(), bytes.len() as u32);
        assert_eq!(index.version(), PxbspVersion::V3);
        assert_eq!(index.lump(PxbspLumpKind::Vertices).len, 12);
        assert_eq!(index.lump(PxbspLumpKind::Nodes).len, 6);
        assert_eq!(PxbspLumpKind::Planes.record_size(index.version()), Some(14));
        assert_eq!(PxbspLumpKind::Faces.record_size(index.version()), Some(10));
        assert_eq!(PxbspLumpKind::Leaves.record_size(index.version()), Some(10));
        assert_eq!(PxbspLumpKind::Models.record_size(index.version()), Some(26));
    }

    #[test]
    fn rejects_unknown_version_and_lump_count() {
        let mut bytes = valid_file();
        bytes[4..6].copy_from_slice(&4u16.to_le_bytes());
        assert!(matches!(
            PxbspIndex::read(&mut SliceReader::new(&bytes)),
            Err(PxbspError::BadVersion { found: 4 })
        ));
        bytes[4..6].copy_from_slice(&PXBSP_VERSION.to_le_bytes());
        bytes[6..8].copy_from_slice(&15u16.to_le_bytes());
        assert!(matches!(
            PxbspIndex::read(&mut SliceReader::new(&bytes)),
            Err(PxbspError::BadLumpCount { found: 15 })
        ));
    }

    #[test]
    fn rejects_overlapping_and_misaligned_payloads() {
        let mut bytes = valid_file();
        let vertices = PXBSP_HEADER_BYTES as usize
            + PxbspLumpKind::Vertices as usize * PXBSP_DIRECTORY_ENTRY_BYTES as usize;
        bytes[vertices + 4..vertices + 8].copy_from_slice(&9u32.to_le_bytes());
        assert!(matches!(
            PxbspIndex::read(&mut SliceReader::new(&bytes)),
            Err(PxbspError::MisalignedOffset { .. })
        ));
        bytes[vertices + 4..vertices + 8].copy_from_slice(&PXBSP_HEADER_BYTES.to_le_bytes());
        assert!(matches!(
            PxbspIndex::read(&mut SliceReader::new(&bytes)),
            Err(PxbspError::DirectoryOverlap { .. })
        ));
    }

    #[test]
    fn rejects_record_size_mismatch() {
        let mut bytes = valid_file();
        let faces = PXBSP_HEADER_BYTES as usize
            + PxbspLumpKind::Faces as usize * PXBSP_DIRECTORY_ENTRY_BYTES as usize;
        bytes[faces + 8..faces + 12].copy_from_slice(&13u32.to_le_bytes());
        assert!(matches!(
            PxbspIndex::read(&mut SliceReader::new(&bytes)),
            Err(PxbspError::MisalignedLump {
                kind: PxbspLumpKind::Faces,
                ..
            })
        ));
    }

    #[test]
    fn rejects_experimental_v2_before_reading_ambiguous_records() {
        let mut bytes = valid_file();
        let planes = PXBSP_HEADER_BYTES as usize
            + PxbspLumpKind::Planes as usize * PXBSP_DIRECTORY_ENTRY_BYTES as usize;
        // Seventy is divisible by both the experimental 10-byte and final
        // 14-byte plane strides. The version rejects it before that
        // structurally ambiguous payload can be interpreted.
        bytes[planes + 8..planes + 12].copy_from_slice(&70u32.to_le_bytes());
        bytes[4..6].copy_from_slice(&PXBSP_VERSION_V2.to_le_bytes());
        assert!(matches!(
            PxbspIndex::read(&mut SliceReader::new(&bytes)),
            Err(PxbspError::BadVersion { found: PXBSP_VERSION_V2 })
        ));
    }

    #[test]
    fn decodes_psoxide_material_slot() {
        let bytes = [
            0x34,
            0x12,
            0x02,
            0x00,
            96,
            112,
            128,
            3,
            material_animation::UV_SCROLL,
            0xfe,
            0xff,
            0x20,
            0x00,
            7,
            9,
            0,
        ];
        assert_eq!(
            PxbspMaterial::decode(&bytes),
            PxbspMaterial {
                texture_asset: 0x1234,
                flags: 2,
                tint: [96, 112, 128],
                blend_mode: 3,
                animation_kind: material_animation::UV_SCROLL,
                animation_data: [0xfe, 0xff, 0x20, 0x00, 7, 9, 0],
            }
        );
    }

    #[test]
    fn decodes_material_animation_payloads() {
        let scroll = PxbspMaterial {
            animation_kind: material_animation::UV_SCROLL,
            animation_data: [0x00, 0x02, 0x00, 0xff, 7, 9, 0],
            ..PxbspMaterial::default()
        };
        assert_eq!(
            scroll.animation(),
            Ok(PxbspMaterialAnimation::UvScroll {
                speed_u_q8: 512,
                speed_v_q8: -256,
                phase_u: 7,
                phase_v: 9,
            })
        );
        let flipbook = PxbspMaterial {
            animation_kind: material_animation::FLIPBOOK,
            animation_data: [4, 2, 7, 3, 5, 0, 0],
            ..PxbspMaterial::default()
        };
        assert_eq!(
            flipbook.animation(),
            Ok(PxbspMaterialAnimation::Flipbook {
                columns: 4,
                rows: 2,
                frame_count: 7,
                ticks_per_frame: 3,
                phase: 5,
            })
        );
    }

    #[test]
    fn rejects_reserved_material_codes_and_payloads() {
        let bad_sidedness = PxbspMaterial {
            flags: material_flags::FACE_MASK,
            ..PxbspMaterial::default()
        };
        assert_eq!(
            bad_sidedness.validate(),
            Err(PxbspMaterialError::InvalidSidedness(
                material_flags::FACE_MASK
            ))
        );
        let bad_blend = PxbspMaterial {
            blend_mode: 5,
            ..PxbspMaterial::default()
        };
        assert_eq!(
            bad_blend.validate(),
            Err(PxbspMaterialError::InvalidBlendMode(5))
        );
        let bad_flipbook = PxbspMaterial {
            animation_kind: material_animation::FLIPBOOK,
            animation_data: [2, 2, 5, 1, 0, 0, 0],
            ..PxbspMaterial::default()
        };
        assert_eq!(
            bad_flipbook.validate(),
            Err(PxbspMaterialError::InvalidAnimationPayload(
                material_animation::FLIPBOOK
            ))
        );
    }

    fn entity_table(payload_size: u16) -> Vec<u8> {
        let payload_offset = PXBSP_ENTITY_TABLE_HEADER_BYTES + PxbspEntity::SIZE;
        let mut bytes = vec![0; payload_offset + 4];
        bytes[0..2].copy_from_slice(&1u16.to_le_bytes());
        bytes[2..4].copy_from_slice(&(PxbspEntity::SIZE as u16).to_le_bytes());
        bytes[4..8].copy_from_slice(&(payload_offset as u32).to_le_bytes());
        let record = PXBSP_ENTITY_TABLE_HEADER_BYTES;
        bytes[record..record + 2].copy_from_slice(&7u16.to_le_bytes());
        bytes[record + 4..record + 6].copy_from_slice(&3u16.to_le_bytes());
        bytes[record + 6..record + 8].copy_from_slice(&11u16.to_le_bytes());
        bytes[record + 8..record + 12].copy_from_slice(&4096i32.to_le_bytes());
        bytes[record + 12..record + 16].copy_from_slice(&8192i32.to_le_bytes());
        bytes[record + 16..record + 20].copy_from_slice(&(-4096i32).to_le_bytes());
        bytes[record + 22..record + 24].copy_from_slice(&1024i16.to_le_bytes());
        bytes[record + 30..record + 32].copy_from_slice(&payload_size.to_le_bytes());
        bytes[payload_offset..].copy_from_slice(&[1, 2, 3, 4]);
        bytes
    }

    #[test]
    fn validates_world_entity_records_and_payloads() {
        let bytes = entity_table(4);
        let table = PxbspEntityTable::new(&bytes).expect("entity table");
        let entity = table.get(0).expect("entity");
        assert_eq!(entity.class_id, 7);
        assert_eq!(entity.model, 3);
        assert_eq!(entity.leaf, 11);
        assert_eq!(
            entity.origin,
            Vec3I32 {
                x: 4096,
                y: 8192,
                z: -4096
            }
        );
        assert_eq!(entity.angles.y, 1024);
        assert_eq!(table.payload(0), Some(&[1, 2, 3, 4][..]));
    }

    #[test]
    fn rejects_entity_payload_outside_lump() {
        let bytes = entity_table(5);
        assert!(matches!(
            PxbspEntityTable::new(&bytes),
            Err(PxbspEntityTableError::BadPayloadRange(0))
        ));
    }

    #[test]
    fn brush_door_payload_round_trips_through_entity_table() {
        let door = PxbspBrushDoor::new(
            Vec3I32 {
                x: 0,
                y: 128 * 4096,
                z: -32 * 4096,
            },
            45,
        );
        let payload = door.to_le_bytes();
        let payload_offset = PXBSP_ENTITY_TABLE_HEADER_BYTES + PxbspEntity::SIZE;
        let mut bytes = vec![0; payload_offset + payload.len()];
        bytes[0..2].copy_from_slice(&1u16.to_le_bytes());
        bytes[2..4].copy_from_slice(&(PxbspEntity::SIZE as u16).to_le_bytes());
        bytes[4..8].copy_from_slice(&(payload_offset as u32).to_le_bytes());
        let record = PXBSP_ENTITY_TABLE_HEADER_BYTES;
        bytes[record..record + 2].copy_from_slice(&entity_class::BRUSH_DOOR.to_le_bytes());
        bytes[record + 2..record + 4]
            .copy_from_slice(&(entity_flags::ENABLED | entity_flags::START_OPEN).to_le_bytes());
        bytes[record + 4..record + 6].copy_from_slice(&1u16.to_le_bytes());
        bytes[record + 30..record + 32].copy_from_slice(&(payload.len() as u16).to_le_bytes());
        bytes[payload_offset..].copy_from_slice(&payload);

        let table = PxbspEntityTable::new(&bytes).expect("entity table");
        let entity = table.get(0).expect("door entity");
        assert_eq!(entity.class_id, entity_class::BRUSH_DOOR);
        assert_eq!(entity.model, 1);
        assert_eq!(
            entity.flags,
            entity_flags::ENABLED | entity_flags::START_OPEN
        );
        let decoded = table
            .payload_record::<PxbspBrushDoor>(0)
            .expect("door payload");
        assert_eq!(decoded, door);
        assert_eq!(decoded.validate(), Ok(()));
    }

    #[test]
    fn leaf_row_decompression_is_bounded_and_fail_closed() {
        // 10 visible leaves -> 2-byte row. Literal byte then a zero-run of 1.
        let visibility = [0x2a, 0x00, 0x01];
        let mut output = [0xffu8; 4];
        assert_eq!(
            decompress_leaf_row(&visibility, 0, 10, &mut output),
            Some(10)
        );
        assert_eq!(output[..2], [0x2a, 0x00]);

        // An oversize row must fail without touching the output.
        let mut untouched = [0xa5u8; 1];
        assert_eq!(
            decompress_leaf_row(&visibility, 0, 10, &mut untouched),
            None
        );
        assert_eq!(untouched, [0xa5]);

        // Truncated run-length data fails after zeroing only the row span.
        let mut truncated = [0xffu8; 2];
        assert_eq!(decompress_leaf_row(&[0x00], 0, 10, &mut truncated), None);

        // A zero-leaf world decompresses an empty row successfully.
        assert_eq!(decompress_leaf_row(&[], 0, 0, &mut []), Some(0));
    }

    #[test]
    fn brush_door_payload_rejects_motionless_recipes() {
        assert_eq!(
            PxbspBrushDoor::new(Vec3I32 { x: 0, y: 0, z: 0 }, 30).validate(),
            Err(PxbspBrushDoorError::ZeroOpenOffset)
        );
        assert_eq!(
            PxbspBrushDoor::new(
                Vec3I32 {
                    x: 0,
                    y: 4096,
                    z: 0
                },
                0
            )
            .validate(),
            Err(PxbspBrushDoorError::ZeroTravelTicks)
        );
    }
}
