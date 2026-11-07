#![no_std]

//! Cooker/runtime contracts for classic PS1 draw surfaces.
//!
//! These records intentionally do not prescribe BSP visibility, lightmap
//! semantics, material animation, or packet ordering. Quake, GoldSrc and
//! PXBSP keep those policies in adapters while sharing one validated wire
//! layout for ordinary convex surfaces and one packet-ready quad command.

/// Runtime bounds retained with a PVS-visible surface.
///
/// Compact map records save resident RAM, but repeatedly decoding and
/// bounding the same visible polygons wastes CPU. Quake-PSX, PXBSP, and
/// GoldSrc adapters can keep this small semantic record while retaining
/// their own face, plane, material, and lighting representations.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct RetainedSurfaceBounds {
    /// Adapter-owned source surface index.
    pub surface_index: u16,
    /// Inclusive world- or model-space minimum corner.
    pub mins: [i16; 3],
    /// Inclusive world- or model-space maximum corner.
    pub maxs: [i16; 3],
}

const _: [(); 14] = [(); core::mem::size_of::<RetainedSurfaceBounds>()];

impl RetainedSurfaceBounds {
    /// Link-time-zero value for fixed runtime arenas.
    pub const ZERO: Self = Self {
        surface_index: 0,
        mins: [0; 3],
        maxs: [0; 3],
    };
}

/// Compact convex-surface header used by Quake-PSX and PXBSP.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct CookedDrawSurface {
    /// Index of the supporting BSP plane.
    pub plane: u16,
    /// First corner in the map's contiguous corner array.
    pub first_corner: u16,
    /// Material or texture-table index.
    pub material: u16,
    /// Game-specific surface flags retained by the adapter.
    pub flags: u8,
    /// Number of corners in this convex polygon.
    pub corner_count: u8,
    /// Primary and secondary authored light-style identifiers.
    pub light_styles: [u8; 2],
}

const _: [(); CookedDrawSurface::SIZE] = [(); core::mem::size_of::<CookedDrawSurface>()];

impl CookedDrawSurface {
    /// Encoded byte size of one compact surface record.
    pub const SIZE: usize = 10;

    /// Decode one little-endian compact surface record.
    #[inline]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            plane: u16::from_le_bytes([bytes[0], bytes[1]]),
            first_corner: u16::from_le_bytes([bytes[2], bytes[3]]),
            material: u16::from_le_bytes([bytes[4], bytes[5]]),
            flags: bytes[6],
            corner_count: bytes[7],
            light_styles: [bytes[8], bytes[9]],
        })
    }

    /// Encode one little-endian compact surface record.
    #[inline]
    pub const fn encode(self) -> [u8; Self::SIZE] {
        let plane = self.plane.to_le_bytes();
        let first_corner = self.first_corner.to_le_bytes();
        let material = self.material.to_le_bytes();
        [
            plane[0],
            plane[1],
            first_corner[0],
            first_corner[1],
            material[0],
            material[1],
            self.flags,
            self.corner_count,
            self.light_styles[0],
            self.light_styles[1],
        ]
    }

    /// Whether the record contains enough corners to form a polygon.
    #[inline]
    pub const fn is_convex_polygon(self) -> bool {
        self.corner_count >= 3
    }
}

/// Packet-ready four-corner command used by the retained world pipeline.
///
/// Positions are batch-local u8 indices. UV and light attributes remain per
/// corner so the same position can participate in material seams. The final
/// source identifiers are cold-path ownership/fallback keys and need not be
/// consumed by the ordinary packet emitter.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct CookedDrawSurfaceCommand {
    /// Batch-local position indices for the four corners.
    pub vertex: [u8; 4],
    /// Per-corner light indices or weights.
    pub light: [u8; 4],
    /// Packed per-corner texture coordinates.
    pub uv: [u16; 4],
    /// Material or texture-table index.
    pub material: u8,
    /// Game-specific packet/surface flags.
    pub flags: u8,
    /// Edge mask used by subdivision and crack-avoidance policy.
    pub blocked_edges: u8,
    /// Adapter-owned subdivision policy identifier.
    pub subdivision_policy: u8,
    /// Source patch or first-corner identifier used by fallback paths.
    pub source_patch: u16,
    /// Source face identifier used by ownership and diagnostics.
    pub source_face: u16,
}

impl CookedDrawSurfaceCommand {
    /// Encoded byte size of one packet-ready command.
    pub const SIZE: usize = 24;

    /// Decode one little-endian packet-ready command.
    #[inline]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            vertex: [bytes[0], bytes[1], bytes[2], bytes[3]],
            light: [bytes[4], bytes[5], bytes[6], bytes[7]],
            uv: [
                u16::from_le_bytes([bytes[8], bytes[9]]),
                u16::from_le_bytes([bytes[10], bytes[11]]),
                u16::from_le_bytes([bytes[12], bytes[13]]),
                u16::from_le_bytes([bytes[14], bytes[15]]),
            ],
            material: bytes[16],
            flags: bytes[17],
            blocked_edges: bytes[18],
            subdivision_policy: bytes[19],
            source_patch: u16::from_le_bytes([bytes[20], bytes[21]]),
            source_face: u16::from_le_bytes([bytes[22], bytes[23]]),
        })
    }

    /// Encode one little-endian packet-ready command.
    #[inline]
    pub const fn encode(self) -> [u8; Self::SIZE] {
        let uv0 = self.uv[0].to_le_bytes();
        let uv1 = self.uv[1].to_le_bytes();
        let uv2 = self.uv[2].to_le_bytes();
        let uv3 = self.uv[3].to_le_bytes();
        let source_patch = self.source_patch.to_le_bytes();
        let source_face = self.source_face.to_le_bytes();
        [
            self.vertex[0],
            self.vertex[1],
            self.vertex[2],
            self.vertex[3],
            self.light[0],
            self.light[1],
            self.light[2],
            self.light[3],
            uv0[0],
            uv0[1],
            uv1[0],
            uv1[1],
            uv2[0],
            uv2[1],
            uv3[0],
            uv3[1],
            self.material,
            self.flags,
            self.blocked_edges,
            self.subdivision_policy,
            source_patch[0],
            source_patch[1],
            source_face[0],
            source_face[1],
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_surface_wire_order_is_pinned() {
        let surface = CookedDrawSurface {
            plane: 0x1234,
            first_corner: 0x2345,
            material: 0x3456,
            flags: 5,
            corner_count: 24,
            light_styles: [7, 8],
        };
        assert_eq!(
            surface.encode(),
            [0x34, 0x12, 0x45, 0x23, 0x56, 0x34, 5, 24, 7, 8]
        );
        assert_eq!(CookedDrawSurface::decode(&surface.encode()), Some(surface));
    }

    #[test]
    fn packet_ready_command_round_trips() {
        let command = CookedDrawSurfaceCommand {
            vertex: [1, 2, 3, 4],
            light: [5, 6, 7, 8],
            uv: [0x1009, 0x1211, 0x1413, 0x1615],
            material: 23,
            flags: 24,
            blocked_edges: 25,
            subdivision_policy: 26,
            source_patch: 0x1c1b,
            source_face: 0x1e1d,
        };
        assert_eq!(
            CookedDrawSurfaceCommand::decode(&command.encode()),
            Some(command)
        );
    }
}
