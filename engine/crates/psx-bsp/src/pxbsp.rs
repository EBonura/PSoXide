//! Checked directory reader for PSoXide's versioned PXBSP container.

use core::fmt;

use crate::{LumpRange, ReadAt};

/// `PXB%` in little-endian byte order.
pub const PXBSP_MAGIC: u32 = 0x2542_5850;
pub const PXBSP_VERSION: u16 = 1;
pub const PXBSP_HEADER_BYTES: u32 = 8;
pub const PXBSP_DIRECTORY_ENTRY_BYTES: u32 = 12;
pub const PXBSP_LUMP_COUNT: usize = 16;

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

    pub const fn record_size(self) -> Option<u32> {
        match self {
            Self::Vertices => Some(12),
            Self::Planes => Some(14),
            Self::Faces => Some(14),
            Self::MarkSurfaces => Some(2),
            Self::Leaves => Some(26),
            Self::Nodes => Some(34),
            Self::ClipNodes => Some(6),
            Self::Models => Some(32),
            Self::Strings => Some(1),
            Self::TextureData
            | Self::SoundData
            | Self::ModelData
            | Self::Materials
            | Self::Visibility
            | Self::Entities
            | Self::StreamingIndex => None,
        }
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
        let version = u16::from_le_bytes(header[4..6].try_into().unwrap());
        if version != PXBSP_VERSION {
            return Err(PxbspError::BadVersion { found: version });
        }
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
            if let Some(record_size) = expected.record_size() {
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
        Ok(Self { file_len, lumps })
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
        let payload_sizes = [0usize, 0, 0, 12, 14, 0, 14, 2, 1, 26, 34, 6, 32, 1, 0, 0];
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
        assert_eq!(index.lump(PxbspLumpKind::Vertices).len, 12);
        assert_eq!(index.lump(PxbspLumpKind::Nodes).len, 34);
    }

    #[test]
    fn rejects_unknown_version_and_lump_count() {
        let mut bytes = valid_file();
        bytes[4..6].copy_from_slice(&2u16.to_le_bytes());
        assert!(matches!(
            PxbspIndex::read(&mut SliceReader::new(&bytes)),
            Err(PxbspError::BadVersion { found: 2 })
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
}
