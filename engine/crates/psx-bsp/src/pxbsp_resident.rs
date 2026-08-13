//! Validated resident storage for cooked PXBSP maps.

use alloc::vec::Vec;
use core::fmt;

use psx_math::int32::mul_q12_i32;

use crate::collision::CollisionHull;
use crate::pxbsp::{
    decompress_leaf_row, PxbspEntityTable, PxbspEntityTableError, PxbspError, PxbspIndex,
    PxbspLumpKind, PxbspMaterial, PxbspMaterialError, PxbspVersion, PXBSP_LUMP_COUNT,
};
use crate::{
    BrushModel, ClipNode, CookedRecord, Face, Leaf, LumpRange, Node, Plane, ReadAt, RecordSlice,
    SliceReadError, SliceReader, Vec3I32, Vertex,
};

use crate::resident::MAX_RESIDENT_MAP_BYTES;

const RESIDENT_LUMPS: [PxbspLumpKind; 14] = [
    PxbspLumpKind::ModelData,
    PxbspLumpKind::Vertices,
    PxbspLumpKind::Planes,
    PxbspLumpKind::Materials,
    PxbspLumpKind::Faces,
    PxbspLumpKind::MarkSurfaces,
    PxbspLumpKind::Visibility,
    PxbspLumpKind::Leaves,
    PxbspLumpKind::Nodes,
    PxbspLumpKind::ClipNodes,
    PxbspLumpKind::Models,
    PxbspLumpKind::Strings,
    PxbspLumpKind::Entities,
    PxbspLumpKind::StreamingIndex,
];

/// Failure while reading or validating a resident PXBSP map.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PxbspMapLoadError<E> {
    Index(PxbspError<E>),
    Read(E),
    TooLarge { required: usize, capacity: usize },
    StaticLegacyVersion { found: u16 },
    LegacyRecord { kind: PxbspLumpKind, index: usize },
    BadVertexData,
    BadMaterial(usize, PxbspMaterialError),
    BadEntityTable(PxbspEntityTableError),
    BadFace(usize),
    BadMarkSurface(usize),
    BadLeaf(usize),
    BadNode(usize),
    BadClipNode(usize),
    BadBrushModel(usize),
    MissingWorldModel,
    BadEntity(usize),
}

impl<E: fmt::Display> fmt::Display for PxbspMapLoadError<E> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Index(error) => write!(output, "PXBSP index is invalid: {error}"),
            Self::Read(error) => write!(output, "PXBSP payload read failed: {error}"),
            Self::TooLarge { required, capacity } => write!(
                output,
                "resident PXBSP needs {required} bytes, capacity is {capacity} bytes"
            ),
            Self::StaticLegacyVersion { found } => write!(
                output,
                "static zero-copy PXBSP requires version 2, found legacy version {found}"
            ),
            Self::LegacyRecord { kind, index } => {
                write!(output, "legacy {kind:?} record {index} cannot be compacted")
            }
            Self::BadVertexData => output.write_str("vertex data is not four-byte aligned"),
            Self::BadMaterial(index, error) => {
                write!(output, "material {index} is invalid: {error:?}")
            }
            Self::BadEntityTable(error) => {
                write!(output, "PXBSP entity table is invalid: {error:?}")
            }
            Self::BadFace(index) => write!(output, "face {index} has an invalid reference"),
            Self::BadMarkSurface(index) => {
                write!(output, "mark surface {index} has an invalid face")
            }
            Self::BadLeaf(index) => write!(output, "leaf {index} has an invalid reference"),
            Self::BadNode(index) => write!(output, "node {index} has an invalid reference"),
            Self::BadClipNode(index) => {
                write!(output, "clip node {index} has an invalid reference")
            }
            Self::BadBrushModel(index) => {
                write!(output, "brush model {index} has an invalid reference")
            }
            Self::MissingWorldModel => output.write_str("PXBSP does not contain a world model"),
            Self::BadEntity(index) => write!(output, "entity {index} has an invalid leaf"),
        }
    }
}

/// Whole-map-resident PXBSP data with validated cross-lump references.
#[derive(Debug)]
pub struct PxbspResidentMap {
    map_id: Option<u32>,
    generation: u32,
    storage: PxbspResidentStorage,
    ranges: [LumpRange; PXBSP_LUMP_COUNT],
    source_ranges: [LumpRange; PXBSP_LUMP_COUNT],
    source_file_len: u32,
}

/// Backing bytes for a validated resident map.
///
/// Streaming callers retain the packed owned form. A map compiled directly
/// into the guest executable can instead be validated and viewed in place,
/// avoiding a second whole-world allocation from the PS1 bump heap.
#[derive(Debug)]
enum PxbspResidentStorage {
    Owned(Vec<u8>),
    Static(&'static [u8]),
}

impl PxbspResidentMap {
    /// Allocate the baseline whole-map resident budget.
    pub fn new() -> Self {
        Self::with_capacity(MAX_RESIDENT_MAP_BYTES)
    }

    /// Allocate a caller-selected resident-map budget.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            map_id: None,
            generation: 0,
            storage: PxbspResidentStorage::Owned(Vec::with_capacity(capacity)),
            ranges: [LumpRange::EMPTY; PXBSP_LUMP_COUNT],
            source_ranges: [LumpRange::EMPTY; PXBSP_LUMP_COUNT],
            source_file_len: 0,
        }
    }

    /// Validate a PXBSP file that already has static guest lifetime.
    ///
    /// Lump ranges continue to address the source file directly. The caller
    /// must provide a four-byte-aligned base when the vertex lump is nonempty;
    /// generated `PXBSP_WORLD` assets satisfy that contract explicitly.
    pub fn from_static(
        map_id: u32,
        bytes: &'static [u8],
    ) -> Result<Self, PxbspMapLoadError<SliceReadError>> {
        let mut reader = SliceReader::new(bytes);
        let index = PxbspIndex::read(&mut reader).map_err(PxbspMapLoadError::Index)?;
        if index.version() != PxbspVersion::V2 {
            return Err(PxbspMapLoadError::StaticLegacyVersion {
                found: index.version().wire(),
            });
        }
        let mut map = Self {
            map_id: None,
            generation: 0,
            storage: PxbspResidentStorage::Static(bytes),
            ranges: [LumpRange::EMPTY; PXBSP_LUMP_COUNT],
            source_ranges: [LumpRange::EMPTY; PXBSP_LUMP_COUNT],
            source_file_len: index.file_len(),
        };
        for kind in PxbspLumpKind::ALL {
            let range = index.lump(kind);
            map.ranges[kind as usize] = range;
            map.source_ranges[kind as usize] = range;
        }
        if let Err(error) = map.validate_references() {
            map.clear_loaded_state();
            return Err(error);
        }
        map.map_id = Some(map_id);
        map.generation = 1;
        Ok(map)
    }

    /// Read and validate one map, associating caller-owned identity with it.
    pub fn load<R: ReadAt>(
        &mut self,
        map_id: u32,
        reader: &mut R,
    ) -> Result<(), PxbspMapLoadError<R::Error>> {
        self.prepare_owned_load();
        let index = PxbspIndex::read(reader).map_err(PxbspMapLoadError::Index)?;

        let total = RESIDENT_LUMPS.iter().try_fold(0usize, |total, &kind| {
            total
                .checked_add(3)
                .map(|value| value & !3)
                .and_then(|aligned| aligned.checked_add(resident_lump_len(&index, kind)))
        });
        let Some(total) = total else {
            return Err(PxbspMapLoadError::TooLarge {
                required: usize::MAX,
                capacity: self.storage_capacity(),
            });
        };
        if total > self.storage_capacity() {
            return Err(PxbspMapLoadError::TooLarge {
                required: total,
                capacity: self.storage_capacity(),
            });
        }

        self.owned_bytes_mut().resize(total, 0);
        let mut destination = 0usize;
        for kind in RESIDENT_LUMPS {
            destination = align_up_4(destination);
            let source = index.lump(kind);
            let resident_len = resident_lump_len(&index, kind);
            let end = destination + resident_len;
            if index.version() == PxbspVersion::V1 && is_compact_v2_lump(kind) {
                let legacy_size =
                    kind.record_size(PxbspVersion::V1)
                        .expect("compacted lump has fixed v1 records") as usize;
                let compact_size =
                    kind.record_size(PxbspVersion::V2)
                        .expect("compacted lump has fixed v2 records") as usize;
                for record_index in 0..source.len as usize / legacy_size {
                    let mut legacy = [0u8; 34];
                    let offset = source.offset + (record_index * legacy_size) as u32;
                    if let Err(error) = reader.read_exact_at(offset, &mut legacy[..legacy_size]) {
                        self.clear_loaded_state();
                        return Err(PxbspMapLoadError::Read(error));
                    }
                    let start = destination + record_index * compact_size;
                    if !compact_legacy_record(
                        kind,
                        &legacy[..legacy_size],
                        &mut self.owned_bytes_mut()[start..start + compact_size],
                    ) {
                        self.clear_loaded_state();
                        return Err(PxbspMapLoadError::LegacyRecord {
                            kind,
                            index: record_index,
                        });
                    }
                }
            } else if let Err(error) =
                reader.read_exact_at(source.offset, &mut self.owned_bytes_mut()[destination..end])
            {
                self.clear_loaded_state();
                return Err(PxbspMapLoadError::Read(error));
            }
            self.ranges[kind as usize] = LumpRange {
                offset: destination as u32,
                len: resident_len as u32,
            };
            destination = end;
        }

        for kind in PxbspLumpKind::ALL {
            self.source_ranges[kind as usize] = index.lump(kind);
        }
        self.source_file_len = index.file_len();
        if let Err(error) = self.validate_references() {
            self.clear_loaded_state();
            return Err(error);
        }
        self.map_id = Some(map_id);
        self.generation = self.generation.wrapping_add(1);
        Ok(())
    }

    /// Caller-owned identifier attached to the current map.
    pub const fn map_id(&self) -> Option<u32> {
        self.map_id
    }

    /// Changes after every successful load, including reloads of one ID.
    pub const fn generation(&self) -> u32 {
        self.generation
    }

    /// Original cooked file length before resident-lump packing.
    pub const fn source_file_len(&self) -> u32 {
        self.source_file_len
    }

    /// Bytes occupied by the active backing payload.
    ///
    /// Owned maps report the packed resident lumps. Static maps report the
    /// complete validated source file that remains embedded in place.
    pub fn resident_bytes(&self) -> usize {
        self.storage_bytes().len()
    }

    /// Source-file range for caller-owned texture and sound streaming.
    pub const fn source_lump(&self, kind: PxbspLumpKind) -> LumpRange {
        self.source_ranges[kind as usize]
    }

    pub fn vertices(&self) -> RecordSlice<'_, Vertex> {
        self.records(PxbspLumpKind::Vertices)
    }

    pub fn vertex_data(&self) -> &[u8] {
        self.lump_bytes(PxbspLumpKind::Vertices)
    }

    pub fn planes(&self) -> RecordSlice<'_, Plane> {
        self.records(PxbspLumpKind::Planes)
    }

    pub fn materials(&self) -> RecordSlice<'_, PxbspMaterial> {
        self.records(PxbspLumpKind::Materials)
    }

    pub fn faces(&self) -> RecordSlice<'_, Face> {
        self.records(PxbspLumpKind::Faces)
    }

    pub fn leaves(&self) -> RecordSlice<'_, Leaf> {
        self.records(PxbspLumpKind::Leaves)
    }

    pub fn nodes(&self) -> RecordSlice<'_, Node> {
        self.records(PxbspLumpKind::Nodes)
    }

    pub fn clip_nodes(&self) -> RecordSlice<'_, ClipNode> {
        self.records(PxbspLumpKind::ClipNodes)
    }

    pub fn brush_models(&self) -> RecordSlice<'_, BrushModel> {
        self.records(PxbspLumpKind::Models)
    }

    pub fn entities(&self) -> PxbspEntityTable<'_> {
        PxbspEntityTable::new(self.lump_bytes(PxbspLumpKind::Entities))
            .expect("validated PXBSP entity table")
    }

    pub fn mark_surfaces(&self) -> RecordSlice<'_, u16> {
        self.records(PxbspLumpKind::MarkSurfaces)
    }

    pub fn visibility(&self) -> &[u8] {
        self.lump_bytes(PxbspLumpKind::Visibility)
    }

    /// Decompress the PVS row for one validated non-solid runtime leaf.
    ///
    /// Returns the number of addressable empty leaves. Bit `n` in the
    /// caller-owned output corresponds to runtime leaf `n + 1`. Invalid,
    /// solid, unbounded, or malformed rows return `None` without allocating.
    pub fn leaf_visibility_into(&self, leaf_index: usize, output: &mut [u8]) -> Option<usize> {
        if leaf_index == 0 {
            return None;
        }
        let leaf = self.leaves().get(leaf_index)?;
        let offset = usize::try_from(leaf.visibility_offset).ok()?;
        let visible_leaves = usize::try_from(self.brush_models().get(0)?.visible_leaves).ok()?;
        decompress_leaf_row(self.visibility(), offset, visible_leaves, output)
    }

    /// Locate a Q20.12 point and decompress its PVS into caller-owned bytes.
    pub fn point_visibility_into(&self, point: Vec3I32, output: &mut [u8]) -> Option<usize> {
        self.leaf_visibility_into(self.point_leaf_index(point)?, output)
    }

    pub fn model_data(&self) -> &[u8] {
        self.lump_bytes(PxbspLumpKind::ModelData)
    }

    pub fn strings(&self) -> &[u8] {
        self.lump_bytes(PxbspLumpKind::Strings)
    }

    pub fn streaming_index(&self) -> &[u8] {
        self.lump_bytes(PxbspLumpKind::StreamingIndex)
    }

    /// Borrow one model-local point, player, or big clipnode hull.
    pub fn model_collision_hull(
        &self,
        model_index: usize,
        hull_index: usize,
    ) -> Option<CollisionHull<'_>> {
        let model = self.brush_models().get(model_index)?;
        let head_node = *model.head_nodes.get(hull_index.checked_add(1)?)?;
        Some(CollisionHull::new(
            self.planes(),
            self.clip_nodes(),
            head_node,
        ))
    }

    /// Borrow one checked zero-terminated value from the cooked string table.
    pub fn string_at(&self, offset: u32) -> Option<&[u8]> {
        let tail = self.strings().get(offset as usize..)?;
        let end = tail.iter().position(|&byte| byte == 0)?;
        Some(&tail[..end])
    }

    /// Locate a Q20.12 world point in the validated render BSP.
    pub fn point_leaf_index(&self, point: Vec3I32) -> Option<usize> {
        let world = self.brush_models().get(0)?;
        let mut node_index = world.head_nodes[0];
        loop {
            if node_index < 0 {
                return Some((-1i32 - node_index as i32) as usize);
            }
            let node = unsafe { self.nodes().get_unchecked(node_index as usize) };
            let plane = unsafe { self.planes().get_unchecked(node.plane as usize) };
            let dot = match plane.kind {
                0 => point.x,
                1 => point.y,
                2 => point.z,
                _ => mul_q12_i32(point.x, plane.normal.x as i32)
                    .saturating_add(mul_q12_i32(point.y, plane.normal.y as i32))
                    .saturating_add(mul_q12_i32(point.z, plane.normal.z as i32)),
            };
            node_index = node.children[(dot.saturating_sub(plane.distance) <= 0) as usize];
        }
    }

    fn clear_loaded_state(&mut self) {
        self.map_id = None;
        match &mut self.storage {
            PxbspResidentStorage::Owned(bytes) => bytes.clear(),
            PxbspResidentStorage::Static(_) => self.storage = PxbspResidentStorage::Static(&[]),
        }
        self.ranges = [LumpRange::EMPTY; PXBSP_LUMP_COUNT];
        self.source_ranges = [LumpRange::EMPTY; PXBSP_LUMP_COUNT];
        self.source_file_len = 0;
    }

    fn prepare_owned_load(&mut self) {
        let capacity = self.storage_capacity();
        match &mut self.storage {
            PxbspResidentStorage::Owned(bytes) => bytes.clear(),
            PxbspResidentStorage::Static(_) => {
                self.storage = PxbspResidentStorage::Owned(Vec::with_capacity(capacity));
            }
        }
        self.map_id = None;
        self.ranges = [LumpRange::EMPTY; PXBSP_LUMP_COUNT];
        self.source_ranges = [LumpRange::EMPTY; PXBSP_LUMP_COUNT];
        self.source_file_len = 0;
    }

    fn storage_capacity(&self) -> usize {
        match &self.storage {
            PxbspResidentStorage::Owned(bytes) => bytes.capacity(),
            PxbspResidentStorage::Static(bytes) => bytes.len(),
        }
    }

    fn storage_bytes(&self) -> &[u8] {
        match &self.storage {
            PxbspResidentStorage::Owned(bytes) => bytes,
            PxbspResidentStorage::Static(bytes) => bytes,
        }
    }

    fn owned_bytes_mut(&mut self) -> &mut Vec<u8> {
        match &mut self.storage {
            PxbspResidentStorage::Owned(bytes) => bytes,
            PxbspResidentStorage::Static(_) => unreachable!("prepared owned PXBSP load"),
        }
    }

    fn lump_bytes(&self, kind: PxbspLumpKind) -> &[u8] {
        let range = self.ranges[kind as usize];
        &self.storage_bytes()[range.offset as usize..range.end() as usize]
    }

    fn records<T: CookedRecord>(&self, kind: PxbspLumpKind) -> RecordSlice<'_, T> {
        RecordSlice::new(self.lump_bytes(kind)).expect("validated PXBSP record lump")
    }

    fn validate_references<E>(&self) -> Result<(), PxbspMapLoadError<E>> {
        if self.vertex_data().as_ptr() as usize & 3 != 0 {
            return Err(PxbspMapLoadError::BadVertexData);
        }
        let vertices = self.vertices();
        let planes = self.planes();
        let materials = self.materials();
        let faces = self.faces();
        let marks = self.mark_surfaces();
        let visibility = self.visibility();
        let leaves = self.leaves();
        let nodes = self.nodes();
        let clip_nodes = self.clip_nodes();

        for (index, material) in materials.iter().enumerate() {
            material
                .validate()
                .map_err(|error| PxbspMapLoadError::BadMaterial(index, error))?;
        }

        for (index, face) in faces.iter().enumerate() {
            let first = usize::try_from(face.first_vertex).ok();
            if face.plane as usize >= planes.len()
                || face.texture as usize >= materials.len()
                || face.vertex_count < 3
                || first.is_none()
                || first.unwrap().saturating_add(face.vertex_count as usize) > vertices.len()
                || face
                    .light_styles
                    .into_iter()
                    .any(|style| style as usize > 64)
            {
                return Err(PxbspMapLoadError::BadFace(index));
            }
        }
        for (index, face) in marks.iter().enumerate() {
            if face as usize >= faces.len() {
                return Err(PxbspMapLoadError::BadMarkSurface(index));
            }
        }
        for (index, leaf) in leaves.iter().enumerate() {
            let marks_end = leaf.first_mark_surface as usize + leaf.mark_surface_count as usize;
            let bad_visibility =
                leaf.visibility_offset >= 0 && leaf.visibility_offset as usize >= visibility.len();
            if !valid_leaf_contents(leaf.contents) || marks_end > marks.len() || bad_visibility {
                return Err(PxbspMapLoadError::BadLeaf(index));
            }
        }
        for (index, node) in nodes.iter().enumerate() {
            let bad_child = node.children.into_iter().any(|child| {
                if child >= 0 {
                    child as usize >= nodes.len()
                } else {
                    (-1i32 - child as i32) as usize >= leaves.len()
                }
            });
            if node.plane as usize >= planes.len() || bad_child {
                return Err(PxbspMapLoadError::BadNode(index));
            }
        }
        for (index, node) in clip_nodes.iter().enumerate() {
            let bad_child = node
                .children
                .into_iter()
                .any(|child| child >= 0 && child as usize >= clip_nodes.len());
            if node.plane < 0 || node.plane as usize >= planes.len() || bad_child {
                return Err(PxbspMapLoadError::BadClipNode(index));
            }
        }
        let models = self.brush_models();
        if models.is_empty() {
            return Err(PxbspMapLoadError::MissingWorldModel);
        }
        for (index, model) in models.iter().enumerate() {
            let faces_end = model.first_face as usize + model.face_count as usize;
            let bad_render_head =
                model.head_nodes[0] < 0 || model.head_nodes[0] as usize >= nodes.len();
            let bad_clip_head = model.head_nodes[1..]
                .iter()
                .any(|&head| head < 0 || head as usize >= clip_nodes.len());
            if faces_end > faces.len() || bad_render_head || bad_clip_head {
                return Err(PxbspMapLoadError::BadBrushModel(index));
            }
        }
        let entities = PxbspEntityTable::new(self.lump_bytes(PxbspLumpKind::Entities))
            .map_err(PxbspMapLoadError::BadEntityTable)?;
        for index in 0..entities.len() {
            let entity = entities.get(index).expect("entity index is in table");
            if entity.leaf as usize >= leaves.len() {
                return Err(PxbspMapLoadError::BadEntity(index));
            }
        }
        Ok(())
    }
}

impl Default for PxbspResidentMap {
    fn default() -> Self {
        Self::new()
    }
}

const fn align_up_4(value: usize) -> usize {
    (value + 3) & !3
}

const fn is_compact_v2_lump(kind: PxbspLumpKind) -> bool {
    matches!(
        kind,
        PxbspLumpKind::Planes
            | PxbspLumpKind::Faces
            | PxbspLumpKind::Leaves
            | PxbspLumpKind::Nodes
            | PxbspLumpKind::Models
    )
}

fn resident_lump_len(index: &PxbspIndex, kind: PxbspLumpKind) -> usize {
    let source = index.lump(kind).len as usize;
    if index.version() != PxbspVersion::V1 || !is_compact_v2_lump(kind) {
        return source;
    }
    let legacy = kind
        .record_size(PxbspVersion::V1)
        .expect("compacted lump has a legacy record size") as usize;
    let compact = kind
        .record_size(PxbspVersion::V2)
        .expect("compacted lump has a compact record size") as usize;
    source / legacy * compact
}

fn compact_legacy_record(kind: PxbspLumpKind, source: &[u8], output: &mut [u8]) -> bool {
    match kind {
        PxbspLumpKind::Planes => {
            output.copy_from_slice(&source[..10]);
        }
        PxbspLumpKind::Faces => {
            let plane = i16::from_le_bytes(source[0..2].try_into().unwrap());
            let first = i32::from_le_bytes(source[4..8].try_into().unwrap());
            let texture = i16::from_le_bytes(source[10..12].try_into().unwrap());
            let flags = u16::from_le_bytes(source[2..4].try_into().unwrap());
            let count = i16::from_le_bytes(source[8..10].try_into().unwrap());
            let (Ok(plane), Ok(first), Ok(texture), Ok(flags), Ok(count)) = (
                u16::try_from(plane),
                u16::try_from(first),
                u16::try_from(texture),
                u8::try_from(flags),
                u8::try_from(count),
            ) else {
                return false;
            };
            output[0..2].copy_from_slice(&plane.to_le_bytes());
            output[2..4].copy_from_slice(&first.to_le_bytes());
            output[4..6].copy_from_slice(&texture.to_le_bytes());
            output[6] = flags;
            output[7] = count;
            output[8..10].copy_from_slice(&source[12..14]);
        }
        PxbspLumpKind::Leaves => {
            let contents = i16::from_le_bytes(source[0..2].try_into().unwrap());
            let visibility = i32::from_le_bytes(source[2..6].try_into().unwrap());
            let count = u16::from_le_bytes(source[20..22].try_into().unwrap());
            let (Ok(contents), Some(visibility), Ok(count)) = (
                i8::try_from(contents),
                if visibility == -1 {
                    Some(u16::MAX)
                } else {
                    u16::try_from(visibility)
                        .ok()
                        .filter(|&offset| offset != u16::MAX)
                },
                u8::try_from(count),
            ) else {
                return false;
            };
            output[0] = contents as u8;
            output[1] = count;
            output[2..4].copy_from_slice(&visibility.to_le_bytes());
            output[4..6].copy_from_slice(&source[18..20]);
            output[6..10].copy_from_slice(&source[22..26]);
        }
        PxbspLumpKind::Nodes => output.copy_from_slice(&source[..6]),
        PxbspLumpKind::Models => {
            output[..12].copy_from_slice(&source[..12]);
            output[12..26].copy_from_slice(&source[18..32]);
        }
        _ => return false,
    }
    true
}

const fn valid_leaf_contents(contents: i16) -> bool {
    matches!(contents, -6..=-1)
}

#[cfg(test)]
pub(crate) mod tests {
    use alloc::vec;

    use super::*;
    use crate::pxbsp::{
        PxbspEntity, PXBSP_DIRECTORY_ENTRY_BYTES, PXBSP_ENTITY_TABLE_HEADER_BYTES,
        PXBSP_HEADER_BYTES, PXBSP_MAGIC, PXBSP_VERSION,
    };
    use crate::SliceReader;

    fn push_i16(output: &mut Vec<u8>, value: i16) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u16(output: &mut Vec<u8>, value: u16) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn push_i32(output: &mut Vec<u8>, value: i32) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    fn pack_leaf(contents: i16, visibility: i32, first_mark: u16, mark_count: u16) -> Vec<u8> {
        let mut output = Vec::with_capacity(Leaf::SIZE);
        output.push(contents as i8 as u8);
        output.push(mark_count as u8);
        push_u16(
            &mut output,
            if visibility < 0 {
                u16::MAX
            } else {
                visibility as u16
            },
        );
        push_u16(&mut output, first_mark);
        output.extend_from_slice(&[0; 4]);
        output
    }

    fn pack_entity_table(leaf: Option<u16>) -> Vec<u8> {
        let count = usize::from(leaf.is_some());
        let payload_offset = PXBSP_ENTITY_TABLE_HEADER_BYTES + count * PxbspEntity::SIZE;
        let mut output = vec![0; payload_offset];
        output[0..2].copy_from_slice(&(count as u16).to_le_bytes());
        output[2..4].copy_from_slice(&(PxbspEntity::SIZE as u16).to_le_bytes());
        output[4..8].copy_from_slice(&(payload_offset as u32).to_le_bytes());
        if let Some(leaf) = leaf {
            output[8..10].copy_from_slice(&1u16.to_le_bytes());
            output[14..16].copy_from_slice(&leaf.to_le_bytes());
        }
        output
    }

    pub(crate) fn valid_lumps() -> [Vec<u8>; PXBSP_LUMP_COUNT] {
        let mut lumps: [Vec<u8>; PXBSP_LUMP_COUNT] = core::array::from_fn(|_| Vec::new());

        let mut vertices = Vec::new();
        for position in [[1i16, 0, 0], [1, 1, 0], [1, 0, 1]] {
            for component in position {
                push_i16(&mut vertices, component);
            }
            vertices.extend_from_slice(&[0, 0, 128, 128, 128, 0]);
        }
        lumps[PxbspLumpKind::Vertices as usize] = vertices;

        let mut plane = Vec::new();
        push_i16(&mut plane, 4096);
        push_i16(&mut plane, 0);
        push_i16(&mut plane, 0);
        push_i32(&mut plane, 0);
        lumps[PxbspLumpKind::Planes as usize] = plane;
        lumps[PxbspLumpKind::Materials as usize] = vec![0; PxbspMaterial::SIZE];

        let mut face = Vec::new();
        push_u16(&mut face, 0);
        push_u16(&mut face, 0);
        push_u16(&mut face, 0);
        face.extend_from_slice(&[0, 3]);
        face.extend_from_slice(&[0, 0]);
        lumps[PxbspLumpKind::Faces as usize] = face;
        lumps[PxbspLumpKind::MarkSurfaces as usize] = 0u16.to_le_bytes().to_vec();
        lumps[PxbspLumpKind::Visibility as usize] = vec![1];

        let mut leaves = pack_leaf(-2, -1, 0, 0);
        leaves.extend_from_slice(&pack_leaf(-1, 0, 0, 1));
        lumps[PxbspLumpKind::Leaves as usize] = leaves;

        let mut node = Vec::with_capacity(Node::SIZE);
        push_u16(&mut node, 0);
        push_i16(&mut node, -2);
        push_i16(&mut node, -1);
        lumps[PxbspLumpKind::Nodes as usize] = node;

        let mut clipnode = Vec::with_capacity(ClipNode::SIZE);
        push_i16(&mut clipnode, 0);
        push_i16(&mut clipnode, -1);
        push_i16(&mut clipnode, -2);
        lumps[PxbspLumpKind::ClipNodes as usize] = clipnode;

        let mut model = Vec::with_capacity(BrushModel::SIZE);
        for _ in 0..6 {
            push_i16(&mut model, 0);
        }
        for _ in 0..4 {
            push_i16(&mut model, 0);
        }
        push_i16(&mut model, 1);
        push_u16(&mut model, 0);
        push_u16(&mut model, 1);
        lumps[PxbspLumpKind::Models as usize] = model;
        lumps[PxbspLumpKind::Strings as usize] = b"world\0".to_vec();
        lumps[PxbspLumpKind::Entities as usize] = pack_entity_table(Some(1));
        lumps
    }

    pub(crate) fn write_file(lumps: &[Vec<u8>; PXBSP_LUMP_COUNT]) -> Vec<u8> {
        write_file_version(lumps, PXBSP_VERSION)
    }

    fn write_file_version(lumps: &[Vec<u8>; PXBSP_LUMP_COUNT], version: u16) -> Vec<u8> {
        let directory_end =
            PXBSP_HEADER_BYTES as usize + PXBSP_DIRECTORY_ENTRY_BYTES as usize * PXBSP_LUMP_COUNT;
        let mut output = vec![0; directory_end];
        output[0..4].copy_from_slice(&PXBSP_MAGIC.to_le_bytes());
        output[4..6].copy_from_slice(&version.to_le_bytes());
        output[6..8].copy_from_slice(&(PXBSP_LUMP_COUNT as u16).to_le_bytes());
        for (index, kind) in PxbspLumpKind::ALL.into_iter().enumerate() {
            output.resize(align_up_4(output.len()), 0);
            let offset = output.len();
            output.extend_from_slice(&lumps[index]);
            let entry = PXBSP_HEADER_BYTES as usize + index * PXBSP_DIRECTORY_ENTRY_BYTES as usize;
            output[entry..entry + 2].copy_from_slice(&(kind as u16).to_le_bytes());
            output[entry + 4..entry + 8].copy_from_slice(&(offset as u32).to_le_bytes());
            output[entry + 8..entry + 12]
                .copy_from_slice(&(lumps[index].len() as u32).to_le_bytes());
        }
        output
    }

    fn legacy_lumps() -> [Vec<u8>; PXBSP_LUMP_COUNT] {
        let compact = valid_lumps();
        let mut legacy = compact.clone();

        legacy[PxbspLumpKind::Planes as usize].clear();
        for plane in compact[PxbspLumpKind::Planes as usize].chunks_exact(Plane::SIZE) {
            legacy[PxbspLumpKind::Planes as usize].extend_from_slice(plane);
            legacy[PxbspLumpKind::Planes as usize].extend_from_slice(&0i32.to_le_bytes());
        }

        legacy[PxbspLumpKind::Faces as usize].clear();
        for face in compact[PxbspLumpKind::Faces as usize].chunks_exact(Face::SIZE) {
            let output = &mut legacy[PxbspLumpKind::Faces as usize];
            output.extend_from_slice(&face[0..2]);
            output.extend_from_slice(&(face[6] as u16).to_le_bytes());
            output.extend_from_slice(
                &(u16::from_le_bytes(face[2..4].try_into().unwrap()) as i32).to_le_bytes(),
            );
            output.extend_from_slice(&(face[7] as i16).to_le_bytes());
            output.extend_from_slice(&face[4..6]);
            output.extend_from_slice(&face[8..10]);
        }

        legacy[PxbspLumpKind::Leaves as usize].clear();
        for leaf in compact[PxbspLumpKind::Leaves as usize].chunks_exact(Leaf::SIZE) {
            let output = &mut legacy[PxbspLumpKind::Leaves as usize];
            output.extend_from_slice(&(leaf[0] as i8 as i16).to_le_bytes());
            let visibility = match u16::from_le_bytes(leaf[2..4].try_into().unwrap()) {
                u16::MAX => -1,
                offset => offset as i32,
            };
            output.extend_from_slice(&visibility.to_le_bytes());
            output.extend_from_slice(&[0; 12]);
            output.extend_from_slice(&leaf[4..6]);
            output.extend_from_slice(&(leaf[1] as u16).to_le_bytes());
            output.extend_from_slice(&leaf[6..10]);
        }

        legacy[PxbspLumpKind::Nodes as usize].clear();
        for node in compact[PxbspLumpKind::Nodes as usize].chunks_exact(Node::SIZE) {
            legacy[PxbspLumpKind::Nodes as usize].extend_from_slice(node);
            legacy[PxbspLumpKind::Nodes as usize].extend_from_slice(&[0; 28]);
        }

        legacy[PxbspLumpKind::Models as usize].clear();
        for model in compact[PxbspLumpKind::Models as usize].chunks_exact(BrushModel::SIZE) {
            legacy[PxbspLumpKind::Models as usize].extend_from_slice(&model[..12]);
            legacy[PxbspLumpKind::Models as usize].extend_from_slice(&[0; 6]);
            legacy[PxbspLumpKind::Models as usize].extend_from_slice(&model[12..26]);
        }
        legacy
    }

    fn load(bytes: &[u8]) -> Result<PxbspResidentMap, PxbspMapLoadError<crate::SliceReadError>> {
        let mut map = PxbspResidentMap::with_capacity(bytes.len());
        map.load(17, &mut SliceReader::new(bytes))?;
        Ok(map)
    }

    fn leak_aligned(bytes: Vec<u8>) -> &'static [u8] {
        let byte_len = bytes.len();
        let mut words = vec![0u32; byte_len.div_ceil(core::mem::size_of::<u32>())];
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), words.as_mut_ptr().cast(), byte_len);
        }
        let words = words.leak();
        unsafe { core::slice::from_raw_parts(words.as_ptr().cast(), byte_len) }
    }

    fn leak_misaligned(bytes: Vec<u8>) -> &'static [u8] {
        let byte_len = bytes.len();
        let mut words = vec![0u32; (byte_len + 1).div_ceil(core::mem::size_of::<u32>())];
        let output = unsafe { words.as_mut_ptr().cast::<u8>().add(1) };
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), output, byte_len);
        }
        let words = words.leak();
        unsafe { core::slice::from_raw_parts(words.as_ptr().cast::<u8>().add(1), byte_len) }
    }

    #[test]
    fn loads_checked_pxbsp_record_views() {
        let bytes = write_file(&valid_lumps());
        let map = load(&bytes).expect("resident map");
        assert_eq!(map.map_id(), Some(17));
        assert_eq!(map.generation(), 1);
        assert_eq!(map.source_file_len(), bytes.len() as u32);
        assert_eq!(map.vertices().len(), 3);
        assert_eq!(map.materials().len(), 1);
        assert_eq!(map.faces().len(), 1);
        assert_eq!(map.entities().get(0).expect("entity").leaf, 1);
        assert_eq!(map.string_at(0), Some(&b"world"[..]));
        assert!(map.resident_bytes() < bytes.len());
        assert_eq!(map.source_lump(PxbspLumpKind::TextureData).len, 0);
    }

    #[test]
    fn owned_load_transcodes_legacy_v1_records_to_compact_views() {
        let bytes = write_file_version(&legacy_lumps(), crate::pxbsp::PXBSP_VERSION_V1);
        let map = load(&bytes).expect("legacy resident map");
        assert_eq!(map.planes().len(), 1);
        assert_eq!(map.faces().get(0).expect("face").vertex_count, 3);
        assert_eq!(map.leaves().get(1).expect("leaf").visibility_offset, 0);
        assert_eq!(map.nodes().get(0).expect("node").children, [-2, -1]);
        assert_eq!(map.brush_models().len(), 1);
        assert!(map.resident_bytes() < bytes.len());
    }

    #[test]
    fn static_zero_copy_rejects_legacy_v1_records_clearly() {
        let bytes = leak_aligned(write_file_version(
            &legacy_lumps(),
            crate::pxbsp::PXBSP_VERSION_V1,
        ));
        assert!(matches!(
            PxbspResidentMap::from_static(1, bytes),
            Err(PxbspMapLoadError::StaticLegacyVersion { found: 1 })
        ));
    }

    #[test]
    fn legacy_compaction_rejects_values_outside_compact_contract() {
        let mut output = [0u8; Face::SIZE];
        let mut face = [0u8; 14];
        face[0..2].copy_from_slice(&(-1i16).to_le_bytes());
        face[8..10].copy_from_slice(&3i16.to_le_bytes());
        assert!(!compact_legacy_record(
            PxbspLumpKind::Faces,
            &face,
            &mut output,
        ));

        let mut output = [0u8; Leaf::SIZE];
        let mut leaf = [0u8; 26];
        leaf[0..2].copy_from_slice(&(-1i16).to_le_bytes());
        leaf[2..6].copy_from_slice(&(u16::MAX as i32).to_le_bytes());
        assert!(!compact_legacy_record(
            PxbspLumpKind::Leaves,
            &leaf,
            &mut output,
        ));
        leaf[2..6].copy_from_slice(&0i32.to_le_bytes());
        leaf[20..22].copy_from_slice(&(u8::MAX as u16 + 1).to_le_bytes());
        assert!(!compact_legacy_record(
            PxbspLumpKind::Leaves,
            &leaf,
            &mut output,
        ));
    }

    #[test]
    fn validates_static_pxbsp_without_copying_its_lumps() {
        let bytes = leak_aligned(write_file(&valid_lumps()));
        let map = PxbspResidentMap::from_static(23, bytes).expect("static resident map");
        let vertex_source = map.source_lump(PxbspLumpKind::Vertices);

        assert_eq!(map.map_id(), Some(23));
        assert_eq!(map.generation(), 1);
        assert_eq!(map.source_file_len(), bytes.len() as u32);
        assert_eq!(map.resident_bytes(), bytes.len());
        assert_eq!(map.vertices().len(), 3);
        assert_eq!(
            map.vertex_data().as_ptr(),
            unsafe { bytes.as_ptr().add(vertex_source.offset as usize) },
            "static map must view the embedded source bytes directly"
        );
    }

    #[test]
    fn rejects_invalid_static_pxbsp_before_publishing_it() {
        let mut lumps = valid_lumps();
        lumps[PxbspLumpKind::Materials as usize][7] = 9;
        let bytes = leak_aligned(write_file(&lumps));
        assert_eq!(
            PxbspResidentMap::from_static(23, bytes).expect_err("invalid static map"),
            PxbspMapLoadError::BadMaterial(0, PxbspMaterialError::InvalidBlendMode(9))
        );
    }

    #[test]
    fn rejects_static_pxbsp_with_a_misaligned_base() {
        let bytes = leak_misaligned(write_file(&valid_lumps()));
        assert_eq!(
            PxbspResidentMap::from_static(23, bytes).expect_err("misaligned static map"),
            PxbspMapLoadError::BadVertexData
        );
    }

    #[test]
    fn locates_points_in_validated_render_tree() {
        let map = load(&write_file(&valid_lumps())).expect("resident map");
        assert_eq!(
            map.point_leaf_index(Vec3I32 {
                x: 4096,
                y: 0,
                z: 0,
            }),
            Some(1)
        );
        assert_eq!(
            map.point_leaf_index(Vec3I32 {
                x: -4096,
                y: 0,
                z: 0,
            }),
            Some(0)
        );
    }

    #[test]
    fn decompresses_point_pvs_into_caller_owned_bytes() {
        let map = load(&write_file(&valid_lumps())).expect("resident map");
        let mut visibility = [0xa5; 1];
        assert_eq!(
            map.point_visibility_into(
                Vec3I32 {
                    x: 4096,
                    y: 0,
                    z: 0,
                },
                &mut visibility,
            ),
            Some(1)
        );
        assert_eq!(visibility, [1]);
        assert_eq!(
            map.point_visibility_into(
                Vec3I32 {
                    x: -4096,
                    y: 0,
                    z: 0,
                },
                &mut visibility,
            ),
            None
        );
        assert_eq!(map.leaf_visibility_into(1, &mut []), None);
    }

    #[test]
    fn exposes_each_model_collision_hull_by_body_size() {
        let map = load(&write_file(&valid_lumps())).expect("resident map");
        let point_hull = map.model_collision_hull(0, 0).expect("point hull");
        assert_eq!(
            point_hull.point_contents(Vec3I32 {
                x: 4096,
                y: 0,
                z: 0,
            }),
            Some(crate::collision::CONTENTS_EMPTY)
        );
        assert!(map.model_collision_hull(0, 3).is_none());
        assert!(map.model_collision_hull(1, 0).is_none());
    }

    #[test]
    fn rejects_face_with_missing_material() {
        let mut lumps = valid_lumps();
        lumps[PxbspLumpKind::Materials as usize].clear();
        let error = load(&write_file(&lumps)).expect_err("bad map");
        assert_eq!(error, PxbspMapLoadError::BadFace(0));
    }

    #[test]
    fn rejects_invalid_material_recipe() {
        let mut lumps = valid_lumps();
        lumps[PxbspLumpKind::Materials as usize][7] = 9;
        let error = load(&write_file(&lumps)).expect_err("bad map");
        assert_eq!(
            error,
            PxbspMapLoadError::BadMaterial(0, PxbspMaterialError::InvalidBlendMode(9))
        );
    }

    #[test]
    fn rejects_entity_linked_outside_leaf_table() {
        let mut lumps = valid_lumps();
        lumps[PxbspLumpKind::Entities as usize] = pack_entity_table(Some(7));
        let error = load(&write_file(&lumps)).expect_err("bad map");
        assert_eq!(error, PxbspMapLoadError::BadEntity(0));
    }

    #[test]
    fn rejects_invalid_variable_entity_payload_table() {
        let mut lumps = valid_lumps();
        lumps[PxbspLumpKind::Entities as usize][4..8].copy_from_slice(&0u32.to_le_bytes());
        let error = load(&write_file(&lumps)).expect_err("bad map");
        assert_eq!(
            error,
            PxbspMapLoadError::BadEntityTable(PxbspEntityTableError::BadPayloadOffset(0))
        );
    }

    #[test]
    fn failed_reload_clears_published_map_state() {
        let bytes = write_file(&valid_lumps());
        let mut map = PxbspResidentMap::with_capacity(bytes.len());
        map.load(17, &mut SliceReader::new(&bytes))
            .expect("first load");
        let generation = map.generation();

        let mut lumps = valid_lumps();
        lumps[PxbspLumpKind::Models as usize].clear();
        let bad = write_file(&lumps);
        assert_eq!(
            map.load(18, &mut SliceReader::new(&bad)),
            Err(PxbspMapLoadError::MissingWorldModel)
        );
        assert_eq!(map.map_id(), None);
        assert_eq!(map.generation(), generation);
        assert_eq!(map.source_file_len(), 0);
    }

    #[test]
    fn enforces_caller_resident_budget() {
        let bytes = write_file(&valid_lumps());
        let mut map = PxbspResidentMap::with_capacity(1);
        let error = map
            .load(17, &mut SliceReader::new(&bytes))
            .expect_err("too large");
        assert!(matches!(error, PxbspMapLoadError::TooLarge { .. }));
    }
}
