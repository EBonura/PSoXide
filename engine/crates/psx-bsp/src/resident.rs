//! Validated resident storage for cooked XBSP maps.
//!
//! Lifted from quake-psx `game/src/asset.rs` commit 83a6349, same GPL-2
//! authorship. Storage lookup, texture upload and episode selection remain
//! caller-owned; this module reads map data only through [`ReadAt`].

use alloc::vec::Vec;
use core::fmt;

use psx_math::int32::mul_q12_i32;

use crate::{
    AliasModelTable, BrushModel, ClipNode, CookedRecord, Face, Leaf, LumpKind, LumpRange,
    MapEntity, Node, Plane, PsbError, PsbIndex, PsbVersion, ReadAt, RecordSlice, TextureInfo,
    Vec3I32, Vertex, LUMP_COUNT,
};

/// Texture atlas location used by the original XBSP cooker.
pub const TEXTURE_VRAM_X: u16 = 320;
/// Width of one streamed texture row in 16-bit VRAM words.
pub const TEXTURE_VRAM_WIDTH: u16 = 640;
/// Maximum texture rows supported by the original XBSP atlas.
pub const TEXTURE_VRAM_MAX_ROWS: u16 = 512;
/// Bytes in one streamed texture row.
pub const TEXTURE_ROW_BYTES: usize = TEXTURE_VRAM_WIDTH as usize * 2;

/// Default resident-map arena budget used by quake-psx.
// ponytail: this 1.1 MB ceiling matches the first whole-map PS1 runtime;
// region paging moves face payloads out when authored worlds exceed it.
pub const MAX_RESIDENT_MAP_BYTES: usize = 1_100_000;

const RESIDENT_LUMPS: [LumpKind; 13] = [
    LumpKind::ModelData,
    LumpKind::Vertices,
    LumpKind::Planes,
    LumpKind::TextureInfo,
    LumpKind::Faces,
    LumpKind::MarkSurfaces,
    LumpKind::Visibility,
    LumpKind::Leaves,
    LumpKind::Nodes,
    LumpKind::ClipNodes,
    LumpKind::Models,
    LumpKind::Strings,
    LumpKind::Entities,
];

/// Failure while reading or validating a resident map.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MapLoadError<E> {
    Index(PsbError<E>),
    Read(E),
    TooLarge { required: usize, capacity: usize },
    LegacyRecord { kind: LumpKind, index: usize },
    BadTextureData,
    BadVertexData,
    BadAliasModels,
    BadFace(usize),
    BadMarkSurface(usize),
    BadLeaf(usize),
    BadNode(usize),
    BadClipNode(usize),
    BadBrushModel(usize),
    BadEntity(usize),
    MissingEntities,
}

impl<E: fmt::Display> fmt::Display for MapLoadError<E> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Index(error) => write!(output, "map index is invalid: {error}"),
            Self::Read(error) => write!(output, "map payload read failed: {error}"),
            Self::TooLarge { required, capacity } => write!(
                output,
                "resident map needs {required} bytes, capacity is {capacity} bytes"
            ),
            Self::LegacyRecord { kind, index } => {
                write!(output, "legacy {kind:?} record {index} cannot be compacted")
            }
            Self::BadTextureData => output.write_str("texture data has an invalid row count"),
            Self::BadVertexData => output.write_str("vertex data is not four-byte aligned"),
            Self::BadAliasModels => output.write_str("alias-model data is invalid"),
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
            Self::BadEntity(index) => write!(output, "entity {index} has an invalid reference"),
            Self::MissingEntities => {
                output.write_str("map does not contain world and player entities")
            }
        }
    }
}

/// Whole-map-resident XBSP data with validated cross-lump references.
pub struct ResidentMap {
    map_id: Option<u32>,
    generation: u32,
    bytes: Vec<u8>,
    ranges: [LumpRange; LUMP_COUNT],
    source_ranges: [LumpRange; LUMP_COUNT],
    source_file_len: u32,
}

impl ResidentMap {
    /// Allocate the original 1.1 MB resident-map budget.
    pub fn new() -> Self {
        Self::with_capacity(MAX_RESIDENT_MAP_BYTES)
    }

    /// Allocate a caller-selected resident-map budget.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            map_id: None,
            generation: 0,
            bytes: Vec::with_capacity(capacity),
            ranges: [LumpRange::EMPTY; LUMP_COUNT],
            source_ranges: [LumpRange::EMPTY; LUMP_COUNT],
            source_file_len: 0,
        }
    }

    /// Read and validate one map, associating caller-owned identity with it.
    pub fn load<R: ReadAt>(
        &mut self,
        map_id: u32,
        reader: &mut R,
    ) -> Result<(), MapLoadError<R::Error>> {
        self.clear_loaded_state();
        let index = PsbIndex::read(reader).map_err(MapLoadError::Index)?;
        validate_texture_data(&index)?;

        let total = RESIDENT_LUMPS.iter().try_fold(0usize, |total, &kind| {
            total
                .checked_add(3)
                .map(|value| value & !3)
                .and_then(|aligned| aligned.checked_add(resident_lump_len(&index, kind)))
        });
        let Some(total) = total else {
            return Err(MapLoadError::TooLarge {
                required: usize::MAX,
                capacity: self.bytes.capacity(),
            });
        };
        if total > self.bytes.capacity() {
            return Err(MapLoadError::TooLarge {
                required: total,
                capacity: self.bytes.capacity(),
            });
        }

        self.bytes.resize(total, 0);
        let mut destination = 0usize;
        for kind in RESIDENT_LUMPS {
            destination = align_up_4(destination);
            let source = index.lump(kind);
            let resident_len = resident_lump_len(&index, kind);
            let end = destination + resident_len;
            if index.version() == PsbVersion::LegacyV1 && is_compact_lump(kind) {
                let legacy_size =
                    kind.record_size(PsbVersion::LegacyV1)
                        .expect("compacted lump has fixed v1 records") as usize;
                let compact_size =
                    kind.record_size(PsbVersion::CompactV3)
                        .expect("compacted lump has fixed v3 records") as usize;
                for record_index in 0..source.len as usize / legacy_size {
                    let mut legacy = [0u8; 34];
                    let offset = source.offset + (record_index * legacy_size) as u32;
                    if let Err(error) = reader.read_exact_at(offset, &mut legacy[..legacy_size]) {
                        self.clear_loaded_state();
                        return Err(MapLoadError::Read(error));
                    }
                    let start = destination + record_index * compact_size;
                    if !compact_legacy_record(
                        kind,
                        &legacy[..legacy_size],
                        &mut self.bytes[start..start + compact_size],
                    ) {
                        self.clear_loaded_state();
                        return Err(MapLoadError::LegacyRecord {
                            kind,
                            index: record_index,
                        });
                    }
                }
            } else if let Err(error) =
                reader.read_exact_at(source.offset, &mut self.bytes[destination..end])
            {
                self.clear_loaded_state();
                return Err(MapLoadError::Read(error));
            }
            self.ranges[kind as usize] = LumpRange {
                offset: destination as u32,
                len: resident_len as u32,
            };
            destination = end;
        }

        for kind in LumpKind::ALL {
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

    /// Source-file range for caller-owned streaming such as textures or sound.
    pub const fn source_lump(&self, kind: LumpKind) -> LumpRange {
        self.source_ranges[kind as usize]
    }

    /// Number of valid texture rows in the source texture-data lump.
    pub const fn texture_rows(&self) -> u16 {
        (self.source_ranges[LumpKind::TextureData as usize].len as usize / TEXTURE_ROW_BYTES) as u16
    }

    pub fn vertices(&self) -> RecordSlice<'_, Vertex> {
        self.records(LumpKind::Vertices)
    }

    pub fn vertex_data(&self) -> &[u8] {
        self.lump_bytes(LumpKind::Vertices)
    }

    pub fn planes(&self) -> RecordSlice<'_, Plane> {
        self.records(LumpKind::Planes)
    }

    pub fn textures(&self) -> RecordSlice<'_, TextureInfo> {
        self.records(LumpKind::TextureInfo)
    }

    pub fn faces(&self) -> RecordSlice<'_, Face> {
        self.records(LumpKind::Faces)
    }

    pub fn leaves(&self) -> RecordSlice<'_, Leaf> {
        self.records(LumpKind::Leaves)
    }

    pub fn nodes(&self) -> RecordSlice<'_, Node> {
        self.records(LumpKind::Nodes)
    }

    pub fn clip_nodes(&self) -> RecordSlice<'_, ClipNode> {
        self.records(LumpKind::ClipNodes)
    }

    pub fn brush_models(&self) -> RecordSlice<'_, BrushModel> {
        self.records(LumpKind::Models)
    }

    pub fn entities(&self) -> RecordSlice<'_, MapEntity> {
        self.records(LumpKind::Entities)
    }

    pub fn mark_surfaces(&self) -> RecordSlice<'_, u16> {
        self.records(LumpKind::MarkSurfaces)
    }

    pub fn visibility(&self) -> &[u8] {
        self.lump_bytes(LumpKind::Visibility)
    }

    /// Decompress the PVS row for one validated non-solid leaf.
    ///
    /// Returns the addressable visible-leaf count. Bit `n` in the
    /// caller-owned output corresponds to leaf `n + 1`. The solid leaf,
    /// leaves without a row (negative offset), oversize rows, and malformed
    /// run-length data return `None` without allocating. This is the same
    /// canonical row semantics as
    /// [`PxbspResidentMap::leaf_visibility_into`](crate::pxbsp_resident::PxbspResidentMap::leaf_visibility_into);
    /// callers should use it instead of decompressing the raw
    /// [`visibility`](Self::visibility) bytes themselves.
    pub fn leaf_visibility_into(&self, leaf_index: usize, output: &mut [u8]) -> Option<usize> {
        if leaf_index == 0 {
            return None;
        }
        let leaf = self.leaves().get(leaf_index)?;
        let offset = usize::try_from(leaf.visibility_offset).ok()?;
        let visible_leaves = usize::try_from(self.brush_models().get(0)?.visible_leaves).ok()?;
        crate::pxbsp::decompress_leaf_row(self.visibility(), offset, visible_leaves, output)
    }

    pub fn model_data(&self) -> &[u8] {
        self.lump_bytes(LumpKind::ModelData)
    }

    pub fn alias_models(&self) -> AliasModelTable<'_> {
        // `load` validates this immutable resident lump before publishing the
        // map. Rebuilding the borrowed view must not repeat the full audit.
        unsafe { AliasModelTable::from_validated(self.model_data()) }
    }

    pub fn strings(&self) -> &[u8] {
        self.lump_bytes(LumpKind::Strings)
    }

    /// Borrow one checked zero-terminated value from the cooked string table.
    pub fn string_at(&self, offset: u16) -> Option<&[u8]> {
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
        self.bytes.clear();
        self.ranges = [LumpRange::EMPTY; LUMP_COUNT];
        self.source_ranges = [LumpRange::EMPTY; LUMP_COUNT];
        self.source_file_len = 0;
    }

    fn lump_bytes(&self, kind: LumpKind) -> &[u8] {
        let range = self.ranges[kind as usize];
        &self.bytes[range.offset as usize..range.end() as usize]
    }

    fn records<T: CookedRecord>(&self, kind: LumpKind) -> RecordSlice<'_, T> {
        RecordSlice::new(self.lump_bytes(kind)).expect("validated record lump")
    }

    fn validate_references<E>(&self) -> Result<(), MapLoadError<E>> {
        let model_data = self.model_data();
        if model_data.as_ptr() as usize & 3 != 0 || AliasModelTable::new(model_data).is_err() {
            return Err(MapLoadError::BadAliasModels);
        }
        if self.vertex_data().as_ptr() as usize & 3 != 0 {
            return Err(MapLoadError::BadVertexData);
        }
        let vertices = self.vertices();
        let planes = self.planes();
        let textures = self.textures();
        let faces = self.faces();
        let marks = self.mark_surfaces();
        let visibility = self.visibility();
        let leaves = self.leaves();
        let nodes = self.nodes();
        let clip_nodes = self.clip_nodes();

        for (index, face) in faces.iter().enumerate() {
            let first = usize::try_from(face.first_vertex).ok();
            if face.plane < 0
                || face.plane as usize >= planes.len()
                || face.texture < 0
                || face.texture as usize >= textures.len()
                || face.vertex_count < 3
                || first.is_none()
                || first.unwrap().saturating_add(face.vertex_count as usize) > vertices.len()
                || face
                    .light_styles
                    .into_iter()
                    .any(|style| style as usize > 64)
            {
                return Err(MapLoadError::BadFace(index));
            }
        }
        for (index, face) in marks.iter().enumerate() {
            if face as usize >= faces.len() {
                return Err(MapLoadError::BadMarkSurface(index));
            }
        }
        for (index, leaf) in leaves.iter().enumerate() {
            let marks_end = leaf.first_mark_surface as usize + leaf.mark_surface_count as usize;
            let bad_visibility =
                leaf.visibility_offset >= 0 && leaf.visibility_offset as usize >= visibility.len();
            if !valid_leaf_contents(leaf.contents) || marks_end > marks.len() || bad_visibility {
                return Err(MapLoadError::BadLeaf(index));
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
                return Err(MapLoadError::BadNode(index));
            }
        }
        for (index, node) in clip_nodes.iter().enumerate() {
            let bad_child = node
                .children
                .into_iter()
                .any(|child| child >= 0 && child as usize >= clip_nodes.len());
            if node.plane < 0 || node.plane as usize >= planes.len() || bad_child {
                return Err(MapLoadError::BadClipNode(index));
            }
        }
        for (index, model) in self.brush_models().iter().enumerate() {
            let faces_end = model.first_face as usize + model.face_count as usize;
            let bad_render_head =
                model.head_nodes[0] < 0 || model.head_nodes[0] as usize >= nodes.len();
            let bad_clip_head = model.head_nodes[1..]
                .iter()
                .any(|&head| head < 0 || head as usize >= clip_nodes.len());
            if faces_end > faces.len() || bad_render_head || bad_clip_head {
                return Err(MapLoadError::BadBrushModel(index));
            }
        }
        let entities = self.entities();
        if entities.len() < 2 {
            return Err(MapLoadError::MissingEntities);
        }
        let brush_models = self.brush_models();
        for (index, entity) in entities.iter().enumerate() {
            let bad_model =
                entity.model < 0 && entity.model.saturating_neg() as usize >= brush_models.len();
            if bad_model || self.string_at(entity.string).is_none() {
                return Err(MapLoadError::BadEntity(index));
            }
        }
        Ok(())
    }
}

impl Default for ResidentMap {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_texture_data<E>(index: &PsbIndex) -> Result<(), MapLoadError<E>> {
    let texture = index.lump(LumpKind::TextureData);
    if texture.len == 0 || !(texture.len as usize).is_multiple_of(TEXTURE_ROW_BYTES) {
        return Err(MapLoadError::BadTextureData);
    }
    let rows = texture.len as usize / TEXTURE_ROW_BYTES;
    if rows > TEXTURE_VRAM_MAX_ROWS as usize {
        return Err(MapLoadError::BadTextureData);
    }
    Ok(())
}

const fn align_up_4(value: usize) -> usize {
    (value + 3) & !3
}

const fn is_compact_lump(kind: LumpKind) -> bool {
    matches!(
        kind,
        LumpKind::Planes | LumpKind::Faces | LumpKind::Leaves | LumpKind::Nodes | LumpKind::Models
    )
}

fn resident_lump_len(index: &PsbIndex, kind: LumpKind) -> usize {
    let source = index.lump(kind).len as usize;
    if index.version() != PsbVersion::LegacyV1 || !is_compact_lump(kind) {
        return source;
    }
    let legacy = kind
        .record_size(PsbVersion::LegacyV1)
        .expect("compacted lump has a legacy record size") as usize;
    let compact = kind
        .record_size(PsbVersion::CompactV3)
        .expect("compacted lump has a compact record size") as usize;
    source / legacy * compact
}

fn compact_legacy_record(kind: LumpKind, source: &[u8], output: &mut [u8]) -> bool {
    match kind {
        LumpKind::Planes => output.copy_from_slice(&source[..Plane::SIZE]),
        LumpKind::Faces => {
            let plane = i16::from_le_bytes(source[0..2].try_into().unwrap());
            let flags = u16::from_le_bytes(source[2..4].try_into().unwrap());
            let first = i32::from_le_bytes(source[4..8].try_into().unwrap());
            let count = i16::from_le_bytes(source[8..10].try_into().unwrap());
            let texture = i16::from_le_bytes(source[10..12].try_into().unwrap());
            let (Ok(plane), Ok(flags), Ok(first), Ok(count), Ok(texture)) = (
                u16::try_from(plane),
                u8::try_from(flags),
                u16::try_from(first),
                u8::try_from(count),
                u16::try_from(texture),
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
        LumpKind::Leaves => {
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
        LumpKind::Nodes => output.copy_from_slice(&source[..6]),
        LumpKind::Models => {
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
mod tests {
    use super::*;
    use crate::Vec3I16;

    #[test]
    fn legacy_structural_records_compact_without_semantic_drift() {
        let mut plane = [0u8; 14];
        plane[0..2].copy_from_slice(&4096i16.to_le_bytes());
        plane[6..10].copy_from_slice(&(64i32 << 12).to_le_bytes());
        let mut compact = [0u8; Plane::SIZE];
        assert!(compact_legacy_record(
            LumpKind::Planes,
            &plane,
            &mut compact
        ));
        assert_eq!(Plane::decode(&compact).kind, 0);
        assert_eq!(Plane::decode(&compact).distance, 64 << 12);

        let mut face = [0u8; 14];
        face[0..2].copy_from_slice(&7i16.to_le_bytes());
        face[2..4].copy_from_slice(&5u16.to_le_bytes());
        face[4..8].copy_from_slice(&1234i32.to_le_bytes());
        face[8..10].copy_from_slice(&24i16.to_le_bytes());
        face[10..12].copy_from_slice(&72i16.to_le_bytes());
        face[12..14].copy_from_slice(&[3, 64]);
        let mut compact = [0u8; Face::SIZE];
        assert!(compact_legacy_record(LumpKind::Faces, &face, &mut compact));
        assert_eq!(
            Face::decode(&compact),
            Face {
                plane: 7,
                flags: 5,
                first_vertex: 1234,
                vertex_count: 24,
                texture: 72,
                light_styles: [3, 64],
            }
        );

        let mut leaf = [0u8; 26];
        leaf[0..2].copy_from_slice(&(-3i16).to_le_bytes());
        leaf[2..6].copy_from_slice(&40823i32.to_le_bytes());
        leaf[18..20].copy_from_slice(&99u16.to_le_bytes());
        leaf[20..22].copy_from_slice(&78u16.to_le_bytes());
        leaf[22..26].copy_from_slice(&[1, 2, 3, 4]);
        let mut compact = [0u8; Leaf::SIZE];
        assert!(compact_legacy_record(LumpKind::Leaves, &leaf, &mut compact));
        let leaf = Leaf::decode(&compact);
        assert_eq!(leaf.contents, -3);
        assert_eq!(leaf.visibility_offset, 40823);
        assert_eq!(leaf.mins, Vec3I16::default());
        assert_eq!(leaf.maxs, Vec3I16::default());
        assert_eq!(leaf.first_mark_surface, 99);
        assert_eq!(leaf.mark_surface_count, 78);
        assert_eq!(leaf.lightmap, [1, 2]);
        assert_eq!(leaf.light_styles, [3, 4]);

        let mut node = [0u8; 34];
        node[0..2].copy_from_slice(&7u16.to_le_bytes());
        node[2..4].copy_from_slice(&(-2i16).to_le_bytes());
        node[4..6].copy_from_slice(&3i16.to_le_bytes());
        node[6..34].fill(0x5a);
        let mut compact = [0u8; Node::SIZE];
        assert!(compact_legacy_record(LumpKind::Nodes, &node, &mut compact));
        let node = Node::decode(&compact);
        assert_eq!(node.plane, 7);
        assert_eq!(node.children, [-2, 3]);
        assert_eq!(node.mins, Vec3I16::default());
        assert_eq!(node.maxs, Vec3I16::default());
        assert_eq!(node.surface_mins, Vec3I16::default());
        assert_eq!(node.surface_maxs, Vec3I16::default());
        assert_eq!(node.first_face, 0);
        assert_eq!(node.face_count, 0);

        let mut model = [0u8; 32];
        model[0..12].fill(1);
        model[12..18].fill(0x5a);
        model[18..32].fill(2);
        let mut compact = [0u8; BrushModel::SIZE];
        assert!(compact_legacy_record(
            LumpKind::Models,
            &model,
            &mut compact
        ));
        let model = BrushModel::decode(&compact);
        assert_eq!(model.origin, Vec3I16::default());
    }

    #[test]
    fn legacy_structural_compaction_rejects_unrepresentable_values() {
        let mut face = [0u8; 14];
        face[0..2].copy_from_slice(&(-1i16).to_le_bytes());
        face[8..10].copy_from_slice(&3i16.to_le_bytes());
        assert!(!compact_legacy_record(
            LumpKind::Faces,
            &face,
            &mut [0; Face::SIZE],
        ));

        let mut leaf = [0u8; 26];
        leaf[0..2].copy_from_slice(&(-1i16).to_le_bytes());
        leaf[2..6].copy_from_slice(&(u16::MAX as i32).to_le_bytes());
        assert!(!compact_legacy_record(
            LumpKind::Leaves,
            &leaf,
            &mut [0; Leaf::SIZE],
        ));
    }
}
