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
    encode_node_bound_max, encode_node_bound_min, BrushModel, ClipNode, CompactNode, CompactPlane,
    CookedRecord, Face, Leaf, LumpRange, Node, ReadAt, RecordSlice, SliceReadError, SliceReader,
    Vec3I32, Vertex,
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
    BadPlane(usize),
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
                "static zero-copy PXBSP requires version {}, found legacy version {found}",
                crate::pxbsp::PXBSP_VERSION
            ),
            Self::LegacyRecord { kind, index } => {
                write!(output, "legacy {kind:?} record {index} cannot be compacted")
            }
            Self::BadVertexData => output.write_str("vertex data is not four-byte aligned"),
            Self::BadPlane(index) => write!(output, "plane {index} is invalid"),
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
        if index.version() != PxbspVersion::V6 {
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
            if requires_transcode(index.version(), kind) {
                let source_size = kind
                    .record_size(index.version())
                    .expect("transcoded lump has fixed source records")
                    as usize;
                let resident_size = kind
                    .record_size(PxbspVersion::V6)
                    .expect("transcoded lump has fixed resident records")
                    as usize;
                for record_index in 0..source.len as usize / source_size {
                    let mut legacy = [0u8; 34];
                    let offset = source.offset + (record_index * source_size) as u32;
                    if let Err(error) = reader.read_exact_at(offset, &mut legacy[..source_size]) {
                        self.clear_loaded_state();
                        return Err(PxbspMapLoadError::Read(error));
                    }
                    let start = destination + record_index * resident_size;
                    if !transcode_record(
                        kind,
                        index.version(),
                        &legacy[..source_size],
                        &mut self.owned_bytes_mut()[start..start + resident_size],
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

    pub fn planes(&self) -> &[CompactPlane] {
        let records: RecordSlice<'_, CompactPlane> = self.records(PxbspLumpKind::Planes);
        if records.is_empty() {
            return &[];
        }
        records
            .as_native_compact_planes()
            .expect("validated native PXBSP plane alignment")
    }

    /// Decode one face record with the aligned halfword loads the PXBSP lump
    /// layout guarantees.
    ///
    /// The ten-byte face record holds three `u16` fields at offsets zero, two
    /// and four. Reached through a `&[u8]` their alignment is unknown, so the
    /// MIPS guest assembles each one from two `lbu` and a shift-or: six loads
    /// where three would do, on the hottest record in the renderer. Every
    /// resident lump sits at a four-byte-aligned offset and the stride is
    /// even, so every record base is two-byte aligned; `validate_references`
    /// rejects a map where that does not hold.
    ///
    /// # Safety
    ///
    /// `index` must be less than `self.faces().len()`.
    #[cfg(target_endian = "little")]
    #[inline(always)]
    pub unsafe fn face_at_unchecked(&self, index: usize) -> Face {
        unsafe {
            let base = self
                .lump_bytes(PxbspLumpKind::Faces)
                .as_ptr()
                .add(index * Face::SIZE);
            debug_assert_eq!(base as usize & 1, 0);
            Face {
                plane: core::ptr::read(base.cast::<u16>()) as i16,
                first_vertex: i32::from(core::ptr::read(base.add(2).cast::<u16>())),
                texture: core::ptr::read(base.add(4).cast::<u16>()) as i16,
                flags: u16::from(core::ptr::read(base.add(6))),
                vertex_count: i16::from(core::ptr::read(base.add(7))),
                light_styles: [core::ptr::read(base.add(8)), core::ptr::read(base.add(9))],
            }
        }
    }

    /// Borrow one face record in place. The hot face loop reads two or three
    /// fields per face; decoding the whole record into a [`Face`] made the
    /// compiler keep a ten-byte copy on the stack and reload it, which on a
    /// cacheless R3000 is six RAM loads and nine stores per face before any
    /// field is used. Each accessor here is one load at the moment of use.
    ///
    /// # Safety
    ///
    /// `index` must be less than `self.faces().len()`.
    #[cfg(target_endian = "little")]
    #[inline(always)]
    pub unsafe fn face_ref_unchecked(&self, index: usize) -> FaceRef {
        unsafe {
            let base = self
                .lump_bytes(PxbspLumpKind::Faces)
                .as_ptr()
                .add(index * Face::SIZE);
            debug_assert_eq!(base as usize & 1, 0);
            FaceRef { base }
        }
    }

    /// Borrow the render-tree nodes in their wire layout.
    ///
    /// [`Self::nodes`] reconstructs the larger semantic [`Node`] aggregate,
    /// which costs sixteen byte loads and their shift-and-or assembly on every
    /// tree step even when the step reads only a plane index and one child.
    /// Every resident lump is placed at a four-byte-aligned offset and
    /// `validate_references` rejects a map whose node lump is not compatible
    /// with `CompactNode`, so this view is always available.
    pub fn compact_nodes(&self) -> &[CompactNode] {
        let records: RecordSlice<'_, Node> = self.records(PxbspLumpKind::Nodes);
        if records.is_empty() {
            return &[];
        }
        records
            .as_native_compact_nodes()
            .expect("validated native PXBSP node alignment")
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

    /// Borrow the leaf mark-surface list as the `u16` array it already is.
    ///
    /// Read through `RecordSlice` each entry costs two `lbu` and a shift-or
    /// because the slice cannot promise alignment; the visibility marking and
    /// the render traversal walk this list several hundred entries deep every
    /// frame. `validate_references` rejects a map whose mark lump is not
    /// two-byte aligned, which no cooked file is.
    #[cfg(target_endian = "little")]
    pub fn mark_surfaces_native(&self) -> &[u16] {
        let bytes = self.lump_bytes(PxbspLumpKind::MarkSurfaces);
        debug_assert_eq!(bytes.as_ptr() as usize & 1, 0);
        // SAFETY: the lump length is a multiple of the two-byte record size
        // (checked by `RecordSlice::new` at every accessor) and the base is
        // two-byte aligned (checked at load).
        unsafe { core::slice::from_raw_parts(bytes.as_ptr().cast::<u16>(), bytes.len() / 2) }
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
        let visible_leaves = usize::try_from(self.world_visible_leaves()?).ok()?;
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

    /// One brush model's head node for the given hull slot, without
    /// reconstructing the rest of the 32-byte record.
    ///
    /// Every collision entry point resolves a hull before it can trace, and
    /// the only field it needs is this one halfword. Decoding the whole model
    /// there rebuilt bounds, origin, the other three head nodes and the face
    /// range as well, which measured 115 guest instructions and a matching
    /// pile of uncached loads per lookup. The offset mirrors
    /// `<BrushModel as CookedRecord>::decode`, exactly as
    /// `PlaneRecords::distance` mirrors the plane decoder for the same reason.
    fn model_head_node(&self, model_index: usize, slot: usize) -> Option<i16> {
        let bytes = self.brush_models().record_bytes(model_index)?;
        let offset = 18 + slot.checked_mul(2)?;
        let low = *bytes.get(offset)?;
        let high = *bytes.get(offset + 1)?;
        Some(i16::from_le_bytes([low, high]))
    }

    /// World model's render-BSP head node, without reconstructing the rest of
    /// the 32-byte record.
    ///
    /// Same reason as [`Self::model_head_node`]: `point_leaf_index` and
    /// `aabb_touches_visible_leaf` each opened `brush_models().get(0)` purely
    /// to read `head_nodes[0]`, which rebuilt bounds, origin, the other three
    /// head nodes, the visible-leaf count and the face range on every call.
    /// Those two run several times per simulation tick and once per queried
    /// bounds, and the decode measured 8.1M cycles over the benchmark window.
    fn world_head_node(&self) -> Option<i16> {
        self.model_head_node(0, 0)
    }

    /// World model's addressable empty-leaf count, read as one halfword. The
    /// offset mirrors `<BrushModel as CookedRecord>::decode`.
    fn world_visible_leaves(&self) -> Option<i16> {
        let bytes = self.brush_models().record_bytes(0)?;
        let low = *bytes.get(26)?;
        let high = *bytes.get(27)?;
        Some(i16::from_le_bytes([low, high]))
    }

    /// Borrow one model-local point, player, or big clipnode hull.
    pub fn model_collision_hull(
        &self,
        model_index: usize,
        hull_index: usize,
    ) -> Option<CollisionHull<'_>> {
        if hull_index == 0 {
            // Quake hull 0: point traces walk the render BSP (balanced,
            // leaf contents from the leaf records) instead of the cooked
            // per-brush clipnode chain.
            // SAFETY: validate_references range-checked every node's plane,
            // children and leaf children and every model head node at load.
            return Some(unsafe {
                CollisionHull::from_render_bsp(
                    self.planes(),
                    self.nodes(),
                    self.leaves(),
                    self.model_head_node(model_index, 0)?,
                )
            });
        }
        let slot = hull_index.checked_add(1)?;
        if slot >= 4 {
            return None;
        }
        let head_node = self.model_head_node(model_index, slot)?;
        let records = self.clip_nodes();
        // SAFETY: validate_references rejects a non-empty clip-node lump whose
        // address is not compatible with ClipNode. Owned resident lumps are
        // also placed at four-byte-aligned offsets and never move after load.
        let nodes = unsafe {
            core::slice::from_raw_parts(
                records.as_bytes().as_ptr().cast::<ClipNode>(),
                records.len(),
            )
        };
        // SAFETY: validate_references range-checked every clip node's plane
        // and children and every model head node at load.
        Some(unsafe { CollisionHull::from_native_clip_nodes(self.planes(), nodes, head_node) })
    }

    /// Borrow one checked zero-terminated value from the cooked string table.
    pub fn string_at(&self, offset: u32) -> Option<&[u8]> {
        let tail = self.strings().get(offset as usize..)?;
        let end = tail.iter().position(|&byte| byte == 0)?;
        Some(&tail[..end])
    }

    /// Locate a Q20.12 world point in the validated render BSP.
    pub fn point_leaf_index(&self, point: Vec3I32) -> Option<usize> {
        let mut node_index = self.world_head_node()?;
        loop {
            if node_index < 0 {
                return Some((-1i32 - node_index as i32) as usize);
            }
            let node = unsafe { self.compact_nodes().get_unchecked(node_index as usize) };
            let plane = unsafe { self.planes().get_unchecked(node.plane as usize) };
            let dot = match plane.kind {
                0 => point.x,
                1 => point.y,
                2 => point.z,
                _ => mul_q12_i32(point.x, plane.normal.x as i32)
                    .saturating_add(mul_q12_i32(point.y, plane.normal.y as i32))
                    .saturating_add(mul_q12_i32(point.z, plane.normal.z as i32)),
            };
            // A point exactly on the plane takes the FRONT child, which is
            // Quake's SV_HullPointContents rule and what the collision hull's
            // point_contents_from already does. Sending the tie to the back
            // child instead walks into the solid side and resolves leaf 0, the
            // outside-world leaf: standing exactly on a floor plane then culls
            // every face and shows the sky through the ground.
            node_index = node.children[(dot.saturating_sub(plane.distance) < 0) as usize];
        }
    }

    /// Return whether an axis-aligned Q20.12 box touches any leaf selected by
    /// a decompressed world PVS row.
    ///
    /// This is the Quake `SV_LinkEdict`/efrag visibility rule without a
    /// retained linked list: a dynamic entity remains active and drawable if
    /// any part of its bounds reaches a visible leaf, even when its origin is
    /// across a BSP plane. The traversal allocates nothing. An unusually deep
    /// tree which exhausts the fixed traversal stack fails open so visibility
    /// optimisation can never make an entity disappear.
    pub fn aabb_touches_visible_leaf(
        &self,
        mins: Vec3I32,
        maxs: Vec3I32,
        visibility: &[u8],
        visible_leaves: usize,
    ) -> bool {
        const NODE_STACK_CAPACITY: usize = 64;

        if mins.x > maxs.x
            || mins.y > maxs.y
            || mins.z > maxs.z
            || visible_leaves > visibility.len().saturating_mul(8)
        {
            return false;
        }
        let Some(head_node) = self.world_head_node() else {
            return false;
        };
        let mut stack = [0i16; NODE_STACK_CAPACITY];
        stack[0] = head_node;
        let mut stack_len = 1usize;
        while stack_len != 0 {
            stack_len -= 1;
            let node_index = stack[stack_len];
            if node_index < 0 {
                let leaf = (-1i32 - i32::from(node_index)) as usize;
                if leaf == 0 || leaf > visible_leaves {
                    continue;
                }
                let visible_index = leaf - 1;
                if visibility[visible_index >> 3] & (1 << (visible_index & 7)) != 0 {
                    return true;
                }
                continue;
            }

            let node = unsafe { self.compact_nodes().get_unchecked(node_index as usize) };
            let plane = unsafe { self.planes().get_unchecked(node.plane as usize) };
            let (minimum_dot, maximum_dot) = aabb_plane_dot_range(mins, maxs, *plane);
            let minimum_distance = minimum_dot.saturating_sub(plane.distance);
            let maximum_distance = maximum_dot.saturating_sub(plane.distance);
            if minimum_distance > 0 {
                stack[stack_len] = node.children[0];
                stack_len += 1;
            } else if maximum_distance <= 0 {
                stack[stack_len] = node.children[1];
                stack_len += 1;
            } else {
                if stack_len + 2 > stack.len() {
                    return true;
                }
                stack[stack_len] = node.children[1];
                stack[stack_len + 1] = node.children[0];
                stack_len += 2;
            }
        }
        false
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
        let vertex_count = self.vertex_data().len() / Vertex::SIZE;
        let plane_records: RecordSlice<'_, CompactPlane> = self.records(PxbspLumpKind::Planes);
        if !plane_records.is_empty() && plane_records.as_native_compact_planes().is_none() {
            return Err(PxbspMapLoadError::BadPlane(0));
        }
        // Same contract as the planes above, so the traversals can read the
        // wire node record in place. The cooker writes every lump at a
        // four-byte-aligned file offset and the owned loader re-aligns to
        // four, so this rejects only a hand-built or truncated file.
        let node_records: RecordSlice<'_, Node> = self.records(PxbspLumpKind::Nodes);
        if !node_records.is_empty() && node_records.as_native_compact_nodes().is_none() {
            return Err(PxbspMapLoadError::BadNode(0));
        }
        // `face_at_unchecked` reads the record's three `u16` fields with
        // aligned halfword loads, which needs a two-byte-aligned lump base.
        if self.lump_bytes(PxbspLumpKind::Faces).as_ptr() as usize & 1 != 0 {
            return Err(PxbspMapLoadError::BadFace(0));
        }
        // Same for the mark-surface list, which `mark_surfaces_native`
        // borrows as a plain `&[u16]`.
        if self.lump_bytes(PxbspLumpKind::MarkSurfaces).as_ptr() as usize & 1 != 0 {
            return Err(PxbspMapLoadError::BadMarkSurface(0));
        }
        let planes = self.planes();
        let materials = self.materials();
        let faces = self.faces();
        let marks = self.mark_surfaces();
        let visibility = self.visibility();
        let leaves = self.leaves();
        let nodes = self.nodes();
        let clip_nodes = self.clip_nodes();
        if !clip_nodes.is_empty()
            && clip_nodes.as_bytes().as_ptr() as usize & (core::mem::align_of::<ClipNode>() - 1)
                != 0
        {
            return Err(PxbspMapLoadError::BadClipNode(0));
        }

        for (index, material) in materials.iter().enumerate() {
            material
                .validate()
                .map_err(|error| PxbspMapLoadError::BadMaterial(index, error))?;
        }

        for (index, face) in faces.iter().enumerate() {
            let first = usize::try_from(face.first_vertex).ok();
            if face.plane < 0
                || face.plane as usize >= planes.len()
                || face.texture < 0
                || face.texture as usize >= materials.len()
                || face.vertex_count < 3
                || first.is_none()
                || first.unwrap().saturating_add(face.vertex_count as usize) > vertex_count
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
            let bad_bounds =
                node.mins.x > node.maxs.x || node.mins.y > node.maxs.y || node.mins.z > node.maxs.z;
            let bad_face_range = usize::from(node.first_face)
                .saturating_add(usize::from(node.face_count))
                > faces.len();
            if node.plane as usize >= planes.len() || bad_child || bad_bounds || bad_face_range {
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

fn aabb_plane_dot_range(mins: Vec3I32, maxs: Vec3I32, plane: CompactPlane) -> (i32, i32) {
    match plane.kind {
        0 => (mins.x, maxs.x),
        1 => (mins.y, maxs.y),
        2 => (mins.z, maxs.z),
        _ => {
            let (minimum, maximum) = match plane.sign_bits & 7 {
                0 => (mins, maxs),
                1 => (
                    Vec3I32 {
                        x: maxs.x,
                        y: mins.y,
                        z: mins.z,
                    },
                    Vec3I32 {
                        x: mins.x,
                        y: maxs.y,
                        z: maxs.z,
                    },
                ),
                2 => (
                    Vec3I32 {
                        x: mins.x,
                        y: maxs.y,
                        z: mins.z,
                    },
                    Vec3I32 {
                        x: maxs.x,
                        y: mins.y,
                        z: maxs.z,
                    },
                ),
                3 => (
                    Vec3I32 {
                        x: maxs.x,
                        y: maxs.y,
                        z: mins.z,
                    },
                    Vec3I32 {
                        x: mins.x,
                        y: mins.y,
                        z: maxs.z,
                    },
                ),
                4 => (
                    Vec3I32 {
                        x: mins.x,
                        y: mins.y,
                        z: maxs.z,
                    },
                    Vec3I32 {
                        x: maxs.x,
                        y: maxs.y,
                        z: mins.z,
                    },
                ),
                5 => (
                    Vec3I32 {
                        x: maxs.x,
                        y: mins.y,
                        z: maxs.z,
                    },
                    Vec3I32 {
                        x: mins.x,
                        y: maxs.y,
                        z: mins.z,
                    },
                ),
                6 => (
                    Vec3I32 {
                        x: mins.x,
                        y: maxs.y,
                        z: maxs.z,
                    },
                    Vec3I32 {
                        x: maxs.x,
                        y: mins.y,
                        z: mins.z,
                    },
                ),
                _ => (maxs, mins),
            };
            (
                mul_q12_i32(minimum.x, i32::from(plane.normal.x))
                    .saturating_add(mul_q12_i32(minimum.y, i32::from(plane.normal.y)))
                    .saturating_add(mul_q12_i32(minimum.z, i32::from(plane.normal.z))),
                mul_q12_i32(maximum.x, i32::from(plane.normal.x))
                    .saturating_add(mul_q12_i32(maximum.y, i32::from(plane.normal.y)))
                    .saturating_add(mul_q12_i32(maximum.z, i32::from(plane.normal.z))),
            )
        }
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

const fn is_compact_lump(kind: PxbspLumpKind) -> bool {
    matches!(
        kind,
        PxbspLumpKind::Planes
            | PxbspLumpKind::Faces
            | PxbspLumpKind::Leaves
            | PxbspLumpKind::Nodes
            | PxbspLumpKind::Models
    )
}

const fn requires_transcode(version: PxbspVersion, kind: PxbspLumpKind) -> bool {
    (matches!(
        version,
        PxbspVersion::V1 | PxbspVersion::V4 | PxbspVersion::V5
    ) && matches!(kind, PxbspLumpKind::Planes))
        || (matches!(version, PxbspVersion::V1) && is_compact_lump(kind))
        || (matches!(version, PxbspVersion::V4) && matches!(kind, PxbspLumpKind::Nodes))
}

fn resident_lump_len(index: &PxbspIndex, kind: PxbspLumpKind) -> usize {
    let source = index.lump(kind).len as usize;
    if !requires_transcode(index.version(), kind) {
        return source;
    }
    let source_size = kind
        .record_size(index.version())
        .expect("transcoded lump has a source record size") as usize;
    let resident_size = kind
        .record_size(PxbspVersion::V6)
        .expect("transcoded lump has a resident record size") as usize;
    source / source_size * resident_size
}

fn transcode_record(
    kind: PxbspLumpKind,
    version: PxbspVersion,
    source: &[u8],
    output: &mut [u8],
) -> bool {
    match kind {
        PxbspLumpKind::Planes => {
            let kind = i32::from_le_bytes(source[10..14].try_into().unwrap());
            let Ok(kind) = u8::try_from(kind) else {
                return false;
            };
            output[0..6].copy_from_slice(&source[0..6]);
            output[6] = kind;
            let mut sign_bits = 0u8;
            for axis in 0..3 {
                let normal = i16::from_le_bytes(source[axis * 2..axis * 2 + 2].try_into().unwrap());
                if normal < 0 {
                    sign_bits |= 1 << axis;
                }
            }
            output[7] = sign_bits;
            output[8..12].copy_from_slice(&source[6..10]);
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
            // Legacy: contents i16, visibility i32, mins/maxs, first mark
            // u16 @18, count u16 @20, lightmap/styles @22..26.
            let contents = i16::from_le_bytes(source[0..2].try_into().unwrap());
            let Ok(contents) = i8::try_from(contents) else {
                return false;
            };
            output[0] = contents as u8;
            output[1] = 0;
            output[2..4].copy_from_slice(&source[20..22]);
            output[4..8].copy_from_slice(&source[2..6]);
            output[8..10].copy_from_slice(&source[18..20]);
            output[10..14].copy_from_slice(&source[22..26]);
        }
        PxbspLumpKind::Nodes => match version {
            PxbspVersion::V1 => {
                output[..6].copy_from_slice(&source[..6]);
                for axis in 0..3 {
                    let min =
                        i16::from_le_bytes(source[6 + axis * 2..8 + axis * 2].try_into().unwrap());
                    let max = i16::from_le_bytes(
                        source[12 + axis * 2..14 + axis * 2].try_into().unwrap(),
                    );
                    output[6 + axis] = encode_node_bound_min(min) as u8;
                    output[9 + axis] = encode_node_bound_max(max) as u8;
                }
                output[12..16].copy_from_slice(&source[30..34]);
            }
            PxbspVersion::V4 => {
                output[..6].copy_from_slice(source);
                output[6..].fill(0);
            }
            PxbspVersion::V5 | PxbspVersion::V6 => return false,
        },
        PxbspLumpKind::Models => output.copy_from_slice(&source[..32]),
        _ => return false,
    }
    true
}

#[cfg(test)]
fn compact_legacy_record(kind: PxbspLumpKind, source: &[u8], output: &mut [u8]) -> bool {
    transcode_record(kind, PxbspVersion::V1, source, output)
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
        PXBSP_HEADER_BYTES, PXBSP_MAGIC,
    };
    use crate::SliceReader;
    use crate::Vec3I16;

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
        output.push(0);
        push_u16(&mut output, mark_count);
        push_i32(&mut output, visibility);
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
        plane.extend_from_slice(&[0, 0]);
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
        for value in [-8, -4, -2] {
            node.push(encode_node_bound_min(value) as u8);
        }
        for value in [16, 12, 10] {
            node.push(encode_node_bound_max(value) as u8);
        }
        push_u16(&mut node, 0);
        push_u16(&mut node, 1);
        lumps[PxbspLumpKind::Nodes as usize] = node;

        let mut clipnode = Vec::with_capacity(ClipNode::SIZE);
        push_i16(&mut clipnode, 0);
        push_i16(&mut clipnode, -1);
        push_i16(&mut clipnode, -2);
        lumps[PxbspLumpKind::ClipNodes as usize] = clipnode;

        let mut model = Vec::with_capacity(BrushModel::SIZE);
        // mins, maxs, origin, then the four head nodes.
        for _ in 0..9 {
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
        // Most resident/renderer fixtures below author the legacy word-corner
        // payload directly. Keep them as explicit v6 compatibility coverage;
        // v7 indexed loading has its own format-specific regression.
        write_file_version(lumps, crate::pxbsp::PXBSP_VERSION_V6)
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
        for plane in compact[PxbspLumpKind::Planes as usize].chunks_exact(CompactPlane::SIZE) {
            let output = &mut legacy[PxbspLumpKind::Planes as usize];
            output.extend_from_slice(&plane[0..6]);
            output.extend_from_slice(&plane[8..12]);
            push_i32(output, i32::from(plane[6]));
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
            output.extend_from_slice(&leaf[4..8]);
            output.extend_from_slice(&[0; 12]);
            output.extend_from_slice(&leaf[8..10]);
            output.extend_from_slice(&leaf[2..4]);
            output.extend_from_slice(&leaf[10..14]);
        }

        legacy[PxbspLumpKind::Nodes as usize].clear();
        for node in compact[PxbspLumpKind::Nodes as usize].chunks_exact(Node::SIZE) {
            let output = &mut legacy[PxbspLumpKind::Nodes as usize];
            let decoded = Node::decode(node);
            output.extend_from_slice(&node[..6]);
            for value in [decoded.mins.x, decoded.mins.y, decoded.mins.z] {
                push_i16(output, value);
            }
            for value in [decoded.maxs.x, decoded.maxs.y, decoded.maxs.z] {
                push_i16(output, value);
            }
            output.extend_from_slice(&[0; 12]);
            output.extend_from_slice(&node[12..16]);
        }

        legacy[PxbspLumpKind::Models as usize].clear();
        for model in compact[PxbspLumpKind::Models as usize].chunks_exact(BrushModel::SIZE) {
            legacy[PxbspLumpKind::Models as usize].extend_from_slice(model);
        }
        legacy
    }

    fn v4_lumps() -> [Vec<u8>; PXBSP_LUMP_COUNT] {
        let mut legacy = v5_lumps();
        legacy[PxbspLumpKind::Nodes as usize].truncate(6);
        legacy
    }

    fn v5_lumps() -> [Vec<u8>; PXBSP_LUMP_COUNT] {
        let mut legacy = valid_lumps();
        let compact_planes = core::mem::take(&mut legacy[PxbspLumpKind::Planes as usize]);
        for plane in compact_planes.chunks_exact(CompactPlane::SIZE) {
            let output = &mut legacy[PxbspLumpKind::Planes as usize];
            output.extend_from_slice(&plane[0..6]);
            output.extend_from_slice(&plane[8..12]);
            push_i32(output, i32::from(plane[6]));
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
        assert_eq!(
            map.planes()[0],
            CompactPlane {
                normal: Vec3I16 {
                    x: 4096,
                    y: 0,
                    z: 0
                },
                kind: 0,
                sign_bits: 0,
                distance: 0,
            }
        );
        assert_eq!(map.materials().len(), 1);
        assert_eq!(map.faces().len(), 1);
        assert_eq!(map.entities().get(0).expect("entity").leaf, 1);
        assert_eq!(map.string_at(0), Some(&b"world"[..]));
        assert!(map.resident_bytes() < bytes.len());
        assert_eq!(map.source_lump(PxbspLumpKind::TextureData).len, 0);
    }

    #[test]
    fn compact_plane_sign_bits_select_exact_aabb_extrema() {
        let mins = Vec3I32 {
            x: -9 * 4096,
            y: -5 * 4096,
            z: -2 * 4096,
        };
        let maxs = Vec3I32 {
            x: 7 * 4096,
            y: 11 * 4096,
            z: 13 * 4096,
        };
        for sign_bits in 0..8u8 {
            let component = |axis: u32| -> i16 {
                if sign_bits & (1u8 << axis) == 0 {
                    1024
                } else {
                    -1024
                }
            };
            let plane = CompactPlane {
                normal: Vec3I16 {
                    x: component(0),
                    y: component(1),
                    z: component(2),
                },
                kind: 3,
                sign_bits,
                distance: 0,
            };
            let actual = aabb_plane_dot_range(mins, maxs, plane);
            let mut expected = (i32::MAX, i32::MIN);
            for x in [mins.x, maxs.x] {
                for y in [mins.y, maxs.y] {
                    for z in [mins.z, maxs.z] {
                        let dot = mul_q12_i32(x, i32::from(plane.normal.x))
                            .saturating_add(mul_q12_i32(y, i32::from(plane.normal.y)))
                            .saturating_add(mul_q12_i32(z, i32::from(plane.normal.z)));
                        expected.0 = expected.0.min(dot);
                        expected.1 = expected.1.max(dot);
                    }
                }
            }
            assert_eq!(actual, expected, "sign bits {sign_bits}");
        }
    }

    #[test]
    fn the_native_mark_surface_view_agrees_with_the_record_slice() {
        for bytes in [
            write_file(&valid_lumps()),
            write_file_version(&legacy_lumps(), crate::pxbsp::PXBSP_VERSION_V1),
        ] {
            let map = load(&bytes).expect("resident map");
            let records = map.mark_surfaces();
            let native = map.mark_surfaces_native();
            assert_eq!(native.len(), records.len());
            assert!(!native.is_empty(), "fixture has no mark surfaces");
            for index in 0..records.len() {
                assert_eq!(
                    native[index],
                    records.get(index).expect("mark"),
                    "mark {index}"
                );
            }
        }
    }

    #[test]
    fn the_aligned_face_read_agrees_with_the_record_decode() {
        // `face_at_unchecked` swaps six byte loads for three halfword loads.
        // Every field, on both the native and the transcoded legacy layout,
        // has to come back exactly as `Face::decode` produced it.
        for bytes in [
            write_file(&valid_lumps()),
            write_file_version(&legacy_lumps(), crate::pxbsp::PXBSP_VERSION_V1),
        ] {
            let map = load(&bytes).expect("resident map");
            let count = map.faces().len();
            assert!(count > 0, "fixture has no faces to compare");
            for index in 0..count {
                assert_eq!(
                    unsafe { map.face_at_unchecked(index) },
                    map.faces().get(index).expect("face"),
                    "face {index}"
                );
            }
        }
    }

    #[test]
    fn the_compact_node_view_agrees_with_the_semantic_node_decode() {
        // `compact_nodes` reads the wire record in place instead of
        // reconstructing `Node`. Every field the traversals use has to come
        // back identical, bounds included once the grid codes are expanded.
        for bytes in [
            write_file(&valid_lumps()),
            write_file_version(&legacy_lumps(), crate::pxbsp::PXBSP_VERSION_V1),
        ] {
            let map = load(&bytes).expect("resident map");
            let compact = map.compact_nodes();
            assert_eq!(compact.len(), map.nodes().len());
            assert!(!compact.is_empty(), "fixture has no nodes to compare");
            for (index, native) in compact.iter().enumerate() {
                let node = map.nodes().get(index).expect("node");
                assert_eq!(native.plane, node.plane, "node {index} plane");
                assert_eq!(native.children, node.children, "node {index} children");
                assert_eq!(
                    native.first_face, node.first_face,
                    "node {index} first face"
                );
                assert_eq!(
                    native.face_count, node.face_count,
                    "node {index} face count"
                );
                assert_eq!(
                    Vec3I16 {
                        x: crate::decode_node_bound_min(native.mins[0]),
                        y: crate::decode_node_bound_min(native.mins[1]),
                        z: crate::decode_node_bound_min(native.mins[2]),
                    },
                    node.mins,
                    "node {index} mins"
                );
                assert_eq!(
                    Vec3I16 {
                        x: crate::decode_node_bound_max(native.maxs[0]),
                        y: crate::decode_node_bound_max(native.maxs[1]),
                        z: crate::decode_node_bound_max(native.maxs[2]),
                    },
                    node.maxs,
                    "node {index} maxs"
                );
            }
        }
    }

    #[test]
    fn owned_load_transcodes_legacy_v1_records_without_losing_node_ranges() {
        let bytes = write_file_version(&legacy_lumps(), crate::pxbsp::PXBSP_VERSION_V1);
        let map = load(&bytes).expect("legacy resident map");
        assert_eq!(map.planes().len(), 1);
        assert_eq!(map.faces().get(0).expect("face").vertex_count, 3);
        assert_eq!(map.leaves().get(1).expect("leaf").visibility_offset, 0);
        let leaf = map.leaves().get(1).expect("leaf");
        assert_eq!(leaf.mins, Vec3I16::default());
        assert_eq!(leaf.maxs, Vec3I16::default());
        let node = map.nodes().get(0).expect("node");
        assert_eq!(node.children, [-2, -1]);
        assert_eq!(
            node.mins,
            Vec3I16 {
                x: -32,
                y: -32,
                z: -32
            }
        );
        assert_eq!(
            node.maxs,
            Vec3I16 {
                x: 32,
                y: 32,
                z: 32
            }
        );
        assert_eq!(node.surface_mins, Vec3I16::default());
        assert_eq!(node.surface_maxs, Vec3I16::default());
        assert_eq!(node.first_face, 0);
        assert_eq!(node.face_count, 1);
        let model = map.brush_models().get(0).expect("model");
        assert_eq!(model.origin, Vec3I16::default());
        assert!(map.resident_bytes() < bytes.len());
    }

    #[test]
    fn owned_load_transcodes_v5_planes_to_native_layout() {
        let bytes = write_file_version(&v5_lumps(), crate::pxbsp::PXBSP_VERSION_V5);
        let map = load(&bytes).expect("v5 resident map");
        assert_eq!(
            map.planes()[0],
            CompactPlane {
                normal: Vec3I16 {
                    x: 4096,
                    y: 0,
                    z: 0
                },
                kind: 0,
                sign_bits: 0,
                distance: 0,
            }
        );
        assert!(map.resident_bytes() < bytes.len());
    }

    #[test]
    fn owned_load_expands_v4_nodes_with_zeroed_missing_metadata() {
        let bytes = write_file_version(&v4_lumps(), crate::pxbsp::PXBSP_VERSION_V4);
        let map = load(&bytes).expect("v4 resident map");
        let node = map.nodes().get(0).expect("node");
        assert_eq!(node.children, [-2, -1]);
        assert_eq!(node.mins, Vec3I16::default());
        assert_eq!(node.maxs, Vec3I16::default());
        assert_eq!(node.first_face, 0);
        assert_eq!(node.face_count, 0);
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

        // Wide visibility offsets and mark counts past 255 now transcode.
        let mut output = [0u8; Leaf::SIZE];
        let mut leaf = [0u8; 26];
        leaf[0..2].copy_from_slice(&(-1i16).to_le_bytes());
        leaf[2..6].copy_from_slice(&(u16::MAX as i32).to_le_bytes());
        leaf[20..22].copy_from_slice(&(u8::MAX as u16 + 1).to_le_bytes());
        assert!(compact_legacy_record(
            PxbspLumpKind::Leaves,
            &leaf,
            &mut output,
        ));
        let decoded = Leaf::decode(&output);
        assert_eq!(decoded.visibility_offset, u16::MAX as i32);
        assert_eq!(decoded.mark_surface_count, 256);
        // Contents outside i8 stay unrepresentable.
        leaf[0..2].copy_from_slice(&(-200i16).to_le_bytes());
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
    fn a_point_exactly_on_a_plane_takes_the_front_child() {
        // Quake's SV_HullPointContents sends only d < 0 to the back child, and
        // CollisionHull::point_contents_from already matches that. This lookup
        // used <= 0, so a point resting exactly on a plane walked into the
        // solid side and resolved leaf 0, the outside-world leaf.
        //
        // Standing on a floor is exactly that tie. The collision hull reported
        // empty while the render tree reported outside the world, so every face
        // was culled and the sky brush showed through the ground. Measured on
        // the shipping cortex 0.3 map, eight of 405 sampled positions hit it,
        // across two different floor heights.
        let map = load(&write_file(&valid_lumps())).expect("resident map");
        assert_eq!(
            map.point_leaf_index(Vec3I32 { x: 0, y: 0, z: 0 }),
            Some(1),
            "on-plane point must resolve to the same side the collision hull does"
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
    fn entity_bounds_link_to_every_touched_render_leaf() {
        let map = load(&write_file(&valid_lumps())).expect("resident map");
        let visible_front_leaf = [1u8];

        // The representative origin is behind the x=0 splitter in the solid
        // leaf, but the complete actor bounds cross into visible leaf one.
        let mins = Vec3I32 {
            x: -4096,
            y: -4096,
            z: -4096,
        };
        let maxs = Vec3I32 {
            x: 4096,
            y: 4096,
            z: 4096,
        };
        assert_eq!(map.point_leaf_index(mins), Some(0));
        assert!(map.aabb_touches_visible_leaf(mins, maxs, &visible_front_leaf, 1));

        let wholly_behind = Vec3I32 {
            x: -1,
            y: 4096,
            z: 4096,
        };
        assert!(!map.aabb_touches_visible_leaf(mins, wholly_behind, &visible_front_leaf, 1,));
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

    /// `model_head_node` reads one halfword at an offset it owns rather than
    /// rebuilding the record, so the offset has to stay tied to the decoder.
    #[test]
    fn lean_head_node_read_matches_the_brush_model_decoder() {
        let mut bytes = [0u8; BrushModel::SIZE];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = (index as u8) ^ 0x5a;
        }
        let decoded = BrushModel::decode(&bytes);
        for slot in 0..decoded.head_nodes.len() {
            let offset = 18 + slot * 2;
            assert_eq!(
                decoded.head_nodes[slot],
                i16::from_le_bytes([bytes[offset], bytes[offset + 1]]),
                "head node slot {slot} moved away from offset {offset}"
            );
        }
    }

    #[test]
    fn rejects_face_with_missing_material() {
        let mut lumps = valid_lumps();
        lumps[PxbspLumpKind::Materials as usize].clear();
        let error = load(&write_file(&lumps)).expect_err("bad map");
        assert_eq!(error, PxbspMapLoadError::BadFace(0));
    }

    #[test]
    fn rejects_node_bounds_and_face_ranges_outside_the_render_tables() {
        let mut lumps = valid_lumps();
        lumps[PxbspLumpKind::Nodes as usize][12..14].copy_from_slice(&1u16.to_le_bytes());
        let error = load(&write_file(&lumps)).expect_err("bad node face range");
        assert_eq!(error, PxbspMapLoadError::BadNode(0));

        let mut lumps = valid_lumps();
        lumps[PxbspLumpKind::Nodes as usize][6] = 1;
        lumps[PxbspLumpKind::Nodes as usize][9] = 0;
        let error = load(&write_file(&lumps)).expect_err("bad node bounds");
        assert_eq!(error, PxbspMapLoadError::BadNode(0));
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

/// A face record borrowed in place; see [`PxbspResidentMap::face_ref_unchecked`].
#[cfg(target_endian = "little")]
#[derive(Clone, Copy)]
pub struct FaceRef {
    base: *const u8,
}

#[cfg(target_endian = "little")]
impl FaceRef {
    #[inline(always)]
    pub fn plane(self) -> usize {
        unsafe { core::ptr::read(self.base.cast::<u16>()) as usize }
    }

    #[inline(always)]
    pub fn first_vertex(self) -> usize {
        unsafe { core::ptr::read(self.base.add(2).cast::<u16>()) as usize }
    }

    #[inline(always)]
    pub fn texture(self) -> usize {
        unsafe { core::ptr::read(self.base.add(4).cast::<u16>()) as usize }
    }

    #[inline(always)]
    pub fn flags(self) -> u16 {
        unsafe { u16::from(core::ptr::read(self.base.add(6))) }
    }

    #[inline(always)]
    pub fn vertex_count(self) -> usize {
        unsafe { core::ptr::read(self.base.add(7)) as usize }
    }

    #[inline(always)]
    pub fn light_styles(self) -> [u8; 2] {
        unsafe {
            [
                core::ptr::read(self.base.add(8)),
                core::ptr::read(self.base.add(9)),
            ]
        }
    }
}
