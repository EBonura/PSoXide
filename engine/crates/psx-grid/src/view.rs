//! Zero-copy readers over the PGL level core and PGRM room chunks.
//!
//! `parse` checks the header, section bounds, alignment and layout hash in
//! constant time and builds typed slices over the bytes. `validate_full`
//! walks every index and exists for the host, the editor and debug guests;
//! the cook proves the same facts when it writes.

use crate::format::*;

/// Why a blob was refused.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FormatError {
    /// Shorter than its own header or a section.
    Truncated,
    /// Wrong magic.
    BadMagic,
    /// Version outside what this reader understands.
    BadVersion,
    /// Layout hash differs: the file was written against another layout.
    BadLayout,
    /// A table does not start on its alignment.
    Misaligned,
    /// A section or count is inconsistent with the header.
    Inconsistent,
    /// An index points outside its table.
    IndexRange,
}

/// Cast `count` records of `T` at `offset`, checking bounds and alignment.
fn table<T: Copy>(bytes: &[u8], offset: usize, count: usize) -> Result<&[T], FormatError> {
    let size = core::mem::size_of::<T>();
    let length = count.checked_mul(size).ok_or(FormatError::Truncated)?;
    let end = offset.checked_add(length).ok_or(FormatError::Truncated)?;
    if end > bytes.len() {
        return Err(FormatError::Truncated);
    }
    let pointer = bytes[offset..].as_ptr();
    if !(pointer as usize).is_multiple_of(core::mem::align_of::<T>()) {
        return Err(FormatError::Misaligned);
    }
    // SAFETY: bounds and alignment were checked above, the records are plain
    // integers with no invalid bit patterns, and the target is little endian.
    Ok(unsafe { core::slice::from_raw_parts(pointer.cast::<T>(), count) })
}

fn header<T: Copy>(bytes: &[u8]) -> Result<&T, FormatError> {
    Ok(&table::<T>(bytes, 0, 1)?[0])
}

/// The level core: directory, portals and materials.
#[derive(Copy, Clone)]
pub struct LevelView<'a> {
    pub header: &'a LevelHeader,
    pub rooms: &'a [RoomRecord],
    pub portals: &'a [Portal],
    pub materials: &'a [Material],
}

impl<'a> LevelView<'a> {
    /// Parse a level core.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, FormatError> {
        let head = header::<LevelHeader>(bytes)?;
        if head.magic != LEVEL_MAGIC {
            return Err(FormatError::BadMagic);
        }
        if head.version < head.min_reader || head.min_reader > VERSION || head.version < MIN_READER
        {
            return Err(FormatError::BadVersion);
        }
        if head.layout_hash != LAYOUT_HASH {
            return Err(FormatError::BadLayout);
        }
        if head.sector_units == 0 || head.sector_units > 255 {
            return Err(FormatError::Inconsistent);
        }
        let mut offset = core::mem::size_of::<LevelHeader>();
        let rooms = table::<RoomRecord>(bytes, offset, usize::from(head.room_count))?;
        offset += core::mem::size_of_val(rooms);
        let portals = table::<Portal>(bytes, offset, usize::from(head.portal_count))?;
        offset += core::mem::size_of_val(portals);
        let materials = table::<Material>(bytes, offset, usize::from(head.material_count))?;
        Ok(Self {
            header: head,
            rooms,
            portals,
            materials,
        })
    }

    /// Runtime units per sector edge.
    pub fn sector_units(&self) -> u32 {
        u32::from(self.header.sector_units)
    }

    /// Portals leaving `room`.
    pub fn room_portals(&self, room: usize) -> &'a [Portal] {
        let Some(record) = self.rooms.get(room) else {
            return &[];
        };
        let first = usize::from(record.portal_first);
        let end = first + usize::from(record.portal_count);
        self.portals.get(first..end).unwrap_or(&[])
    }

    /// Walk every index. Host and debug use.
    pub fn validate_full(&self) -> Result<(), FormatError> {
        for room in self.rooms {
            let end = usize::from(room.portal_first) + usize::from(room.portal_count);
            if end > self.portals.len() || room.width == 0 || room.depth == 0 {
                return Err(FormatError::IndexRange);
            }
        }
        for portal in self.portals {
            if usize::from(portal.dest) >= self.rooms.len() {
                return Err(FormatError::IndexRange);
            }
        }
        Ok(())
    }
}

/// The collision block of a room chunk, usable before the render block has
/// landed.
#[derive(Copy, Clone)]
pub struct CollisionView<'a> {
    pub header: &'a RoomHeader,
    pub sectors: &'a [Sector],
    pub exts: &'a [SectorExt],
    pub walls: &'a [ColWall],
    pub splitbs: &'a [SplitB],
    pub col_planes: &'a [ColPlane],
    pub liquids: &'a [Liquid],
    pub movers: &'a [Mover],
    pub mover_verts: &'a [u16],
}

/// The render block of a room chunk.
#[derive(Copy, Clone)]
pub struct RenderView<'a> {
    pub material_use: &'a [u8],
    pub vertices: &'a [Vtx],
    pub quads: &'a [Quad],
    pub cells: &'a [RCell],
    pub lod_blocks: &'a [LodBlock],
    pub lod_quads: &'a [Quad],
    pub plane_refs: &'a [PlaneRef],
}

/// A whole room chunk.
#[derive(Copy, Clone)]
pub struct RoomView<'a> {
    pub col: CollisionView<'a>,
    pub render: RenderView<'a>,
}

fn room_header(bytes: &[u8]) -> Result<&RoomHeader, FormatError> {
    let head = header::<RoomHeader>(bytes)?;
    if head.magic != ROOM_MAGIC {
        return Err(FormatError::BadMagic);
    }
    if head.version != VERSION {
        return Err(FormatError::BadVersion);
    }
    if head.width == 0 || head.depth == 0 {
        return Err(FormatError::Inconsistent);
    }
    Ok(head)
}

fn section<'a, T: Copy>(
    bytes: &'a [u8],
    head: &RoomHeader,
    id: usize,
) -> Result<&'a [T], FormatError> {
    let entry = head.sections[id];
    if !entry.offset.is_multiple_of(4) {
        return Err(FormatError::Misaligned);
    }
    table::<T>(bytes, entry.offset as usize, entry.count as usize)
}

impl<'a> CollisionView<'a> {
    /// Parse the collision block. `bytes` need only cover the first
    /// `render_offset` bytes of the chunk.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, FormatError> {
        let head = room_header(bytes)?;
        let cells = usize::from(head.width) * usize::from(head.depth);
        let sectors = section::<Sector>(bytes, head, section::SECTORS)?;
        if sectors.len() != cells
            || (head.render_offset as usize) < core::mem::size_of::<RoomHeader>()
        {
            return Err(FormatError::Inconsistent);
        }
        if bytes.len() < head.render_offset as usize {
            return Err(FormatError::Truncated);
        }
        Ok(Self {
            header: head,
            sectors,
            exts: section(bytes, head, section::SECTOR_EXTS)?,
            walls: section(bytes, head, section::WALLS)?,
            splitbs: section(bytes, head, section::SPLITBS)?,
            col_planes: section(bytes, head, section::COL_PLANES)?,
            liquids: section(bytes, head, section::LIQUIDS)?,
            movers: section(bytes, head, section::MOVERS)?,
            mover_verts: section(bytes, head, section::MOVER_VERTS)?,
        })
    }

    /// Sectors along X.
    pub fn width(&self) -> usize {
        usize::from(self.header.width)
    }

    /// Sectors along Z.
    pub fn depth(&self) -> usize {
        usize::from(self.header.depth)
    }

    /// Extension record of `sector`, if it has one.
    #[inline(always)]
    pub fn ext(&self, sector: &Sector) -> Option<&'a SectorExt> {
        if sector.ext == NO_EXT {
            None
        } else {
            self.exts.get(usize::from(sector.ext))
        }
    }

    /// Wall pieces of a sector.
    #[inline(always)]
    pub fn sector_walls(&self, ext: &SectorExt) -> &'a [ColWall] {
        let first = usize::from(ext.wall_first);
        self.walls
            .get(first..first + usize::from(ext.wall_count))
            .unwrap_or(&[])
    }

    /// Collision planes of a sector.
    #[inline(always)]
    pub fn sector_planes(&self, ext: &SectorExt) -> &'a [ColPlane] {
        let first = usize::from(ext.plane_first);
        self.col_planes
            .get(first..first + usize::from(ext.plane_count))
            .unwrap_or(&[])
    }

    /// Second-triangle heights of a split-level sector.
    #[inline(always)]
    pub fn split_b(&self, ext: &SectorExt) -> Option<&'a SplitB> {
        if ext.splitb == NO_EXT {
            None
        } else {
            self.splitbs.get(usize::from(ext.splitb))
        }
    }

    /// Walk every collision index.
    pub fn validate_full(&self) -> Result<(), FormatError> {
        for sector in self.sectors {
            if sector.ext != NO_EXT && usize::from(sector.ext) >= self.exts.len() {
                return Err(FormatError::IndexRange);
            }
        }
        for ext in self.exts {
            if usize::from(ext.wall_first) + usize::from(ext.wall_count) > self.walls.len()
                || usize::from(ext.plane_first) + usize::from(ext.plane_count)
                    > self.col_planes.len()
                || (ext.splitb != NO_EXT && usize::from(ext.splitb) >= self.splitbs.len())
            {
                return Err(FormatError::IndexRange);
            }
        }
        for wall in self.walls {
            if wall.edge > edge::NESW {
                return Err(FormatError::IndexRange);
            }
        }
        for plane in self.col_planes {
            if !(plane_kind::PLATFORM..=plane_kind::BLOCKER).contains(&plane.kind) {
                return Err(FormatError::IndexRange);
            }
        }
        for liquid in self.liquids {
            if usize::from(liquid.x1) >= self.width()
                || usize::from(liquid.z1) >= self.depth()
                || liquid.x0 > liquid.x1
                || liquid.z0 > liquid.z1
            {
                return Err(FormatError::IndexRange);
            }
        }
        Ok(())
    }
}

impl<'a> RoomView<'a> {
    /// Parse a whole chunk.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, FormatError> {
        let col = CollisionView::parse(bytes)?;
        let head = col.header;
        if (head.byte_len as usize) > bytes.len() || head.render_offset > head.byte_len {
            return Err(FormatError::Truncated);
        }
        let render = RenderView {
            material_use: section(bytes, head, section::MATERIAL_USE)?,
            vertices: section(bytes, head, section::VERTICES)?,
            quads: section(bytes, head, section::QUADS)?,
            cells: section(bytes, head, section::CELLS)?,
            lod_blocks: section(bytes, head, section::LOD_BLOCKS)?,
            lod_quads: section(bytes, head, section::LOD_QUADS)?,
            plane_refs: section(bytes, head, section::PLANE_REFS)?,
        };
        if render.cells.len() != col.sectors.len()
            || render.quads.len() + render.lod_quads.len() > usize::from(u16::MAX)
        {
            return Err(FormatError::Inconsistent);
        }
        Ok(Self { col, render })
    }

    /// Walk every index in both blocks. `material_count` is the level's.
    pub fn validate_full(&self, material_count: usize) -> Result<(), FormatError> {
        self.col.validate_full()?;
        let r = &self.render;
        for cell in r.cells {
            if usize::from(cell.first) + usize::from(cell.count) > r.quads.len() {
                return Err(FormatError::IndexRange);
            }
        }
        for quad in r.quads.iter().chain(r.lod_quads) {
            if quad.v.iter().any(|&v| usize::from(v) >= r.vertices.len())
                || usize::from(quad.material) >= material_count
                || (quad.flags & QUAD_AXIS != 0 && (quad.flags >> QUAD_AXIS_SHIFT) & 3 > 2)
            {
                return Err(FormatError::IndexRange);
            }
        }
        for block in r.lod_blocks {
            if usize::from(block.first) + usize::from(block.count) > r.lod_quads.len() {
                return Err(FormatError::IndexRange);
            }
        }
        for reference in r.plane_refs {
            if usize::from(reference.quad) + usize::from(reference.count) > r.quads.len() {
                return Err(FormatError::IndexRange);
            }
        }
        if r.material_use
            .iter()
            .any(|&m| usize::from(m) >= material_count)
        {
            return Err(FormatError::IndexRange);
        }
        Ok(())
    }
}
