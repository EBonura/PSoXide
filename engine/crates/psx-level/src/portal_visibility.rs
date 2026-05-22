//! Heap-free portal visibility traversal for cooked runtime rooms.
//!
//! The traversal is intentionally conservative: portals are clipped in the
//! horizontal camera cone and rooms are submitted whole once accepted. This
//! matches the current renderer, which can overdraw visible room payloads while
//! streaming keeps the current visible set resident.

use crate::{LevelRoomPortalRecord, LevelRoomRecord, RoomIndex};

const INVALID_ROOM: RoomIndex = RoomIndex(u16::MAX);
const INVALID_PORTAL: u16 = u16::MAX;
const Q12_SHIFT: i32 = 12;
const Q12_ONE: i64 = 1 << Q12_SHIFT;
const SLOPE_LIMIT_Q12: i32 = 64 * 4096;

/// Camera inputs needed by the portal traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortalVisibilityCamera {
    /// Camera X in global room/world units.
    pub x: i32,
    /// Camera Y in global room/world units.
    pub y: i32,
    /// Camera Z in global room/world units.
    pub z: i32,
    /// Camera yaw sine, Q12.
    pub sin_yaw_q12: i32,
    /// Camera yaw cosine, Q12.
    pub cos_yaw_q12: i32,
    /// Near plane in camera-space depth units.
    pub near_z: i32,
    /// Far plane in camera-space depth units.
    pub far_z: i32,
    /// Horizontal half field-of-view as `tan(angle)`, Q12.
    pub half_fov_tan_q12: i32,
    /// Minimum accepted clipped portal cone width, Q12 tangent units.
    pub min_portal_width_q12: i32,
}

impl PortalVisibilityCamera {
    /// Build portal traversal camera inputs.
    pub const fn new(
        x: i32,
        y: i32,
        z: i32,
        sin_yaw_q12: i32,
        cos_yaw_q12: i32,
        near_z: i32,
        far_z: i32,
        half_fov_tan_q12: i32,
        min_portal_width_q12: i32,
    ) -> Self {
        Self {
            x,
            y,
            z,
            sin_yaw_q12,
            cos_yaw_q12,
            near_z,
            far_z,
            half_fov_tan_q12,
            min_portal_width_q12,
        }
    }
}

/// One accepted runtime room in portal traversal order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortalVisibleRoom {
    /// Runtime room index.
    pub room: RoomIndex,
    /// First frustum in [`PortalVisibilityResult::frustums`] for this room.
    pub frustum_first: u16,
    /// Number of accepted frustums for this room.
    pub frustum_count: u8,
    /// Portal depth from the current room.
    pub depth: u8,
}

impl PortalVisibleRoom {
    /// Empty visible-room slot.
    pub const EMPTY: Self = Self {
        room: INVALID_ROOM,
        frustum_first: 0,
        frustum_count: 0,
        depth: 0,
    };
}

/// One clipped horizontal portal cone reaching a runtime room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortalFrustum {
    /// Room reached by this frustum.
    pub room: RoomIndex,
    /// Room that sourced the portal, or `RoomIndex(u16::MAX)` for the root.
    pub source_room: RoomIndex,
    /// Directed portal record index, or `u16::MAX` for the root frustum.
    pub source_portal: u16,
    /// Portal depth from the current room.
    pub depth: u8,
    /// Left edge of the horizontal clipped cone, Q12 tangent units.
    pub left_tan_q12: i32,
    /// Right edge of the horizontal clipped cone, Q12 tangent units.
    pub right_tan_q12: i32,
}

impl PortalFrustum {
    /// Empty frustum slot.
    pub const EMPTY: Self = Self {
        room: INVALID_ROOM,
        source_room: INVALID_ROOM,
        source_portal: INVALID_PORTAL,
        depth: 0,
        left_tan_q12: 0,
        right_tan_q12: 0,
    };
}

/// One adjacent room beyond the currently accepted portal-visible set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortalFrontierRoom {
    /// Room that is one portal beyond the visible set.
    pub room: RoomIndex,
    /// Visible source room that owns the frontier portal.
    pub source_room: RoomIndex,
}

impl PortalFrontierRoom {
    /// Empty frontier slot.
    pub const EMPTY: Self = Self {
        room: INVALID_ROOM,
        source_room: INVALID_ROOM,
    };
}

/// Per-traversal portal visibility diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PortalVisibilityStats {
    /// Directed portals tested.
    pub portals_tested: u16,
    /// Directed portals accepted into the traversal.
    pub portals_accepted: u16,
    /// Portals rejected because the camera was behind the source-facing plane.
    pub reject_backface: u16,
    /// Portals rejected by near/far/cone clipping.
    pub reject_frustum: u16,
    /// Portals rejected because the clipped cone was too narrow.
    pub reject_tiny: u16,
    /// Visible-room pool capacity hits.
    pub cap_room: u16,
    /// Frustum pool capacity hits.
    pub cap_frustum: u16,
    /// Maximum traversal depth hits.
    pub cap_depth: u16,
    /// Deepest accepted portal depth.
    pub max_depth: u8,
}

/// Fixed-pool output from a portal visibility traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortalVisibilityResult<
    const MAX_ROOMS: usize,
    const MAX_FRUSTUMS: usize,
    const MAX_FRONTIER: usize,
> {
    /// Accepted rooms in traversal order.
    pub rooms: [PortalVisibleRoom; MAX_ROOMS],
    /// Number of accepted rooms.
    pub room_count: usize,
    /// Accepted frustums in breadth-first traversal order.
    pub frustums: [PortalFrustum; MAX_FRUSTUMS],
    /// Number of accepted frustums.
    pub frustum_count: usize,
    /// Adjacent rooms one portal beyond the accepted visible set.
    pub frontier_rooms: [PortalFrontierRoom; MAX_FRONTIER],
    /// Number of frontier rooms.
    pub frontier_count: usize,
    /// Traversal diagnostics.
    pub stats: PortalVisibilityStats,
}

impl<const MAX_ROOMS: usize, const MAX_FRUSTUMS: usize, const MAX_FRONTIER: usize>
    PortalVisibilityResult<MAX_ROOMS, MAX_FRUSTUMS, MAX_FRONTIER>
{
    /// Empty traversal result.
    pub const EMPTY: Self = Self {
        rooms: [PortalVisibleRoom::EMPTY; MAX_ROOMS],
        room_count: 0,
        frustums: [PortalFrustum::EMPTY; MAX_FRUSTUMS],
        frustum_count: 0,
        frontier_rooms: [PortalFrontierRoom::EMPTY; MAX_FRONTIER],
        frontier_count: 0,
        stats: PortalVisibilityStats {
            portals_tested: 0,
            portals_accepted: 0,
            reject_backface: 0,
            reject_frustum: 0,
            reject_tiny: 0,
            cap_room: 0,
            cap_frustum: 0,
            cap_depth: 0,
            max_depth: 0,
        },
    };

    /// Reset the result for reuse.
    pub fn clear(&mut self) {
        *self = Self::EMPTY;
    }

    /// True when `room` was accepted as portal-visible.
    pub fn contains_room(&self, room: RoomIndex) -> bool {
        self.room_position(room).is_some()
    }

    /// Find a visible room's traversal-order slot.
    pub fn room_position(&self, room: RoomIndex) -> Option<usize> {
        let mut i = 0usize;
        while i < self.room_count.min(MAX_ROOMS) {
            if self.rooms[i].room == room {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    /// Bit mask of visible runtime rooms for debug telemetry.
    pub fn visible_room_mask(&self) -> u64 {
        let mut mask = 0u64;
        let mut i = 0usize;
        while i < self.room_count.min(MAX_ROOMS) {
            mask |= room_mask_bit(self.rooms[i].room);
            i += 1;
        }
        mask
    }

    /// Bit mask of frontier runtime rooms for debug telemetry.
    pub fn frontier_room_mask(&self) -> u64 {
        let mut mask = 0u64;
        let mut i = 0usize;
        while i < self.frontier_count.min(MAX_FRONTIER) {
            mask |= room_mask_bit(self.frontier_rooms[i].room);
            i += 1;
        }
        mask
    }

    fn push_visible_room(&mut self, room: RoomIndex, depth: u8) -> Option<usize> {
        if room == INVALID_ROOM {
            return None;
        }
        if let Some(slot) = self.room_position(room) {
            if depth < self.rooms[slot].depth {
                self.rooms[slot].depth = depth;
            }
            return Some(slot);
        }
        if self.room_count >= MAX_ROOMS {
            self.stats.cap_room = self.stats.cap_room.saturating_add(1);
            return None;
        }
        let slot = self.room_count;
        self.rooms[slot] = PortalVisibleRoom {
            room,
            frustum_first: self.frustum_count.min(u16::MAX as usize) as u16,
            frustum_count: 0,
            depth,
        };
        self.room_count += 1;
        Some(slot)
    }

    fn push_frustum(&mut self, room_slot: usize, frustum: PortalFrustum) -> bool {
        if self.frustum_count >= MAX_FRUSTUMS {
            self.stats.cap_frustum = self.stats.cap_frustum.saturating_add(1);
            return false;
        }
        self.frustums[self.frustum_count] = frustum;
        self.frustum_count += 1;
        if let Some(room) = self.rooms.get_mut(room_slot) {
            if room.frustum_count == 0 {
                room.frustum_first = (self.frustum_count - 1).min(u16::MAX as usize) as u16;
            }
            room.frustum_count = room.frustum_count.saturating_add(1);
        }
        true
    }

    fn contains_redundant_frustum(&self, frustum: PortalFrustum) -> bool {
        let mut i = 0usize;
        while i < self.frustum_count.min(MAX_FRUSTUMS) {
            let existing = self.frustums[i];
            if existing.room == frustum.room
                && existing.source_room == frustum.source_room
                && existing.source_portal == frustum.source_portal
                && existing.depth <= frustum.depth
                && existing.left_tan_q12 <= frustum.left_tan_q12
                && existing.right_tan_q12 >= frustum.right_tan_q12
            {
                return true;
            }
            i += 1;
        }
        false
    }

    fn push_frontier(&mut self, room: RoomIndex, source_room: RoomIndex) {
        if room == INVALID_ROOM || self.contains_room(room) {
            return;
        }
        let mut i = 0usize;
        while i < self.frontier_count.min(MAX_FRONTIER) {
            if self.frontier_rooms[i].room == room {
                return;
            }
            i += 1;
        }
        if self.frontier_count >= MAX_FRONTIER {
            return;
        }
        self.frontier_rooms[self.frontier_count] = PortalFrontierRoom { room, source_room };
        self.frontier_count += 1;
    }
}

/// Build the portal-visible room set from `current_room`.
pub fn build_portal_visibility<
    const MAX_ROOMS: usize,
    const MAX_FRUSTUMS: usize,
    const MAX_FRONTIER: usize,
>(
    rooms: &[LevelRoomRecord],
    portals: &[LevelRoomPortalRecord],
    current_room: RoomIndex,
    camera: PortalVisibilityCamera,
    max_depth: u8,
    out: &mut PortalVisibilityResult<MAX_ROOMS, MAX_FRUSTUMS, MAX_FRONTIER>,
) {
    out.clear();
    if current_room.to_usize() >= rooms.len() {
        return;
    }

    let Some(root_slot) = out.push_visible_room(current_room, 0) else {
        return;
    };
    let half_fov = camera.half_fov_tan_q12.max(1);
    let root = PortalFrustum {
        room: current_room,
        source_room: INVALID_ROOM,
        source_portal: INVALID_PORTAL,
        depth: 0,
        left_tan_q12: -half_fov,
        right_tan_q12: half_fov,
    };
    if !out.push_frustum(root_slot, root) {
        return;
    }

    let mut cursor = 0usize;
    while cursor < out.frustum_count.min(MAX_FRUSTUMS) {
        let frustum = out.frustums[cursor];
        cursor += 1;
        if frustum.room.to_usize() >= rooms.len() {
            continue;
        }
        if frustum.depth >= max_depth {
            out.stats.cap_depth = out.stats.cap_depth.saturating_add(1);
            continue;
        }
        let record = rooms[frustum.room.to_usize()];
        let portal_start = record.portal_first as usize;
        let portal_end = portal_start.saturating_add(record.portal_count as usize);
        let mut portal_index = portal_start;
        while portal_index < portal_end.min(portals.len()) {
            let portal = portals[portal_index];
            portal_index += 1;
            if portal.source_room != frustum.room {
                continue;
            }
            out.stats.portals_tested = out.stats.portals_tested.saturating_add(1);
            if portal.destination_room == current_room
                || portal.destination_room == frustum.source_room
            {
                continue;
            }
            if !portal_front_faces_camera(portal, camera) {
                out.stats.reject_backface = out.stats.reject_backface.saturating_add(1);
                continue;
            }
            let Some(child_cone) = clipped_portal_cone(portal, camera, frustum) else {
                out.stats.reject_frustum = out.stats.reject_frustum.saturating_add(1);
                continue;
            };
            if child_cone
                .right_tan_q12
                .saturating_sub(child_cone.left_tan_q12)
                < camera.min_portal_width_q12.max(0)
            {
                out.stats.reject_tiny = out.stats.reject_tiny.saturating_add(1);
                continue;
            }
            let child_depth = frustum.depth.saturating_add(1);
            let child = PortalFrustum {
                room: portal.destination_room,
                source_room: portal.source_room,
                source_portal: portal_index.saturating_sub(1).min(u16::MAX as usize) as u16,
                depth: child_depth,
                left_tan_q12: child_cone.left_tan_q12,
                right_tan_q12: child_cone.right_tan_q12,
            };
            if out.contains_redundant_frustum(child) {
                continue;
            }
            let Some(room_slot) = out.push_visible_room(portal.destination_room, child_depth)
            else {
                continue;
            };
            if out.push_frustum(room_slot, child) {
                out.stats.portals_accepted = out.stats.portals_accepted.saturating_add(1);
                out.stats.max_depth = out.stats.max_depth.max(child_depth);
            }
        }
    }

    build_frontier(rooms, portals, out);
}

fn build_frontier<const MAX_ROOMS: usize, const MAX_FRUSTUMS: usize, const MAX_FRONTIER: usize>(
    rooms: &[LevelRoomRecord],
    portals: &[LevelRoomPortalRecord],
    out: &mut PortalVisibilityResult<MAX_ROOMS, MAX_FRUSTUMS, MAX_FRONTIER>,
) {
    let mut room_slot = 0usize;
    while room_slot < out.room_count.min(MAX_ROOMS) {
        let source_room = out.rooms[room_slot].room;
        if source_room.to_usize() >= rooms.len() {
            room_slot += 1;
            continue;
        }
        let record = rooms[source_room.to_usize()];
        let portal_start = record.portal_first as usize;
        let portal_end = portal_start.saturating_add(record.portal_count as usize);
        let mut portal_index = portal_start;
        while portal_index < portal_end.min(portals.len()) {
            let portal = portals[portal_index];
            if portal.source_room == source_room {
                out.push_frontier(portal.destination_room, source_room);
            }
            portal_index += 1;
        }
        room_slot += 1;
    }
}

#[derive(Debug, Clone, Copy)]
struct PortalCone {
    left_tan_q12: i32,
    right_tan_q12: i32,
}

fn portal_front_faces_camera(
    portal: LevelRoomPortalRecord,
    camera: PortalVisibilityCamera,
) -> bool {
    let dx = (camera.x as i64).saturating_sub(portal.vertex_x[0] as i64);
    let dy = (camera.y as i64).saturating_sub(portal.vertex_y[0] as i64);
    let dz = (camera.z as i64).saturating_sub(portal.vertex_z[0] as i64);
    let dot = dx
        .saturating_mul(portal.normal_x as i64)
        .saturating_add(dy.saturating_mul(portal.normal_y as i64))
        .saturating_add(dz.saturating_mul(portal.normal_z as i64));
    dot >= 0
}

fn clipped_portal_cone(
    portal: LevelRoomPortalRecord,
    camera: PortalVisibilityCamera,
    parent: PortalFrustum,
) -> Option<PortalCone> {
    let mut depths = [0i64; 4];
    let mut laterals = [0i64; 4];
    let mut min_depth = i64::MAX;
    let mut max_depth = i64::MIN;
    let mut interval = PortalSlopeInterval::EMPTY;
    let near = camera.near_z.max(1) as i64;
    let far = camera.far_z.max(camera.near_z.max(1)) as i64;

    let mut i = 0usize;
    while i < 4 {
        let dx = (portal.vertex_x[i] as i64).saturating_sub(camera.x as i64);
        let dz = (portal.vertex_z[i] as i64).saturating_sub(camera.z as i64);
        let depth = dx
            .saturating_mul(-(camera.sin_yaw_q12 as i64))
            .saturating_add(dz.saturating_mul(-(camera.cos_yaw_q12 as i64)))
            >> Q12_SHIFT;
        let lateral = dx
            .saturating_mul(camera.cos_yaw_q12 as i64)
            .saturating_sub(dz.saturating_mul(camera.sin_yaw_q12 as i64))
            >> Q12_SHIFT;
        depths[i] = depth;
        laterals[i] = lateral;
        min_depth = min_depth.min(depth);
        max_depth = max_depth.max(depth);
        if depth >= near && depth <= far {
            interval.include_slope(lateral, depth);
        }
        i += 1;
    }

    if max_depth < near || min_depth > far {
        return None;
    }

    let mut edge = 0usize;
    while edge < 4 {
        let next = (edge + 1) & 3;
        include_depth_crossing(
            &mut interval,
            laterals[edge],
            depths[edge],
            laterals[next],
            depths[next],
            near,
        );
        include_depth_crossing(
            &mut interval,
            laterals[edge],
            depths[edge],
            laterals[next],
            depths[next],
            far,
        );
        edge += 1;
    }

    if !interval.valid {
        return None;
    }
    let left = interval.min_q12.max(parent.left_tan_q12);
    let right = interval.max_q12.min(parent.right_tan_q12);
    (left <= right).then_some(PortalCone {
        left_tan_q12: left,
        right_tan_q12: right,
    })
}

fn include_depth_crossing(
    interval: &mut PortalSlopeInterval,
    lateral_a: i64,
    depth_a: i64,
    lateral_b: i64,
    depth_b: i64,
    clip_depth: i64,
) {
    let crosses = (depth_a < clip_depth && depth_b >= clip_depth)
        || (depth_b < clip_depth && depth_a >= clip_depth);
    if !crosses {
        return;
    }
    let denom = depth_b.saturating_sub(depth_a);
    if denom == 0 {
        return;
    }
    let num = clip_depth.saturating_sub(depth_a);
    let lateral =
        lateral_a.saturating_add(lateral_b.saturating_sub(lateral_a).saturating_mul(num) / denom);
    interval.include_slope(lateral, clip_depth.max(1));
}

#[derive(Debug, Clone, Copy)]
struct PortalSlopeInterval {
    min_q12: i32,
    max_q12: i32,
    valid: bool,
}

impl PortalSlopeInterval {
    const EMPTY: Self = Self {
        min_q12: i32::MAX,
        max_q12: i32::MIN,
        valid: false,
    };

    fn include_slope(&mut self, lateral: i64, depth: i64) {
        if depth <= 0 {
            return;
        }
        let raw = lateral.saturating_mul(Q12_ONE) / depth;
        let slope = clamp_i64_to_i32(raw, -SLOPE_LIMIT_Q12, SLOPE_LIMIT_Q12);
        if !self.valid {
            self.min_q12 = slope;
            self.max_q12 = slope;
            self.valid = true;
        } else {
            self.min_q12 = self.min_q12.min(slope);
            self.max_q12 = self.max_q12.max(slope);
        }
    }
}

fn room_mask_bit(room: RoomIndex) -> u64 {
    let raw = room.to_usize();
    if raw < u64::BITS as usize {
        1u64 << raw
    } else {
        0
    }
}

fn clamp_i64_to_i32(value: i64, min: i32, max: i32) -> i32 {
    value.clamp(min as i64, max as i64) as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AssetId, LevelCameraRecord, LevelFarVistaRecord, LevelSkyRecord, MaterialIndex, RoomIndex,
    };

    const fn room(index: u16, portal_first: u16, portal_count: u8) -> LevelRoomRecord {
        LevelRoomRecord {
            name: "",
            world_asset: AssetId(index),
            origin_x: 0,
            origin_z: 0,
            sector_size: 2048,
            draw_distance: 16_384,
            chunk_activation_radius_sectors: 8,
            visibility_radius: 1,
            resident_chunk_limit: 8,
            visible_chunk_limit: 8,
            material_first: MaterialIndex(0),
            material_count: 0,
            portal_first,
            portal_count,
            near_room_first: 0,
            near_room_count: 0,
            overlapped_room_first: 0,
            overlapped_room_count: 0,
            fog_rgb: [0, 0, 0],
            fog_near: 0,
            fog_far: 0,
            atmosphere_rgb: [0, 0, 0],
            atmosphere_density: 0,
            atmosphere_fall_speed_q4: 0,
            atmosphere_wind_speed_q4: 0,
            sky: LevelSkyRecord::DEFAULT,
            far_vista: LevelFarVistaRecord::DEFAULT,
            camera: LevelCameraRecord::DEFAULT,
            flags: 0,
        }
    }

    const fn north_portal(source: u16, destination: u16, normal_z: i16) -> LevelRoomPortalRecord {
        LevelRoomPortalRecord {
            source_room: RoomIndex(source),
            destination_room: RoomIndex(destination),
            kind: 0,
            normal_x: 0,
            normal_y: 0,
            normal_z,
            vertex_x: [-1024, 1024, 1024, -1024],
            vertex_y: [0, 0, 2048, 2048],
            vertex_z: [4096, 4096, 4096, 4096],
        }
    }

    const fn wall_portal(
        source: u16,
        destination: u16,
        x0: i32,
        x1: i32,
        z: i32,
    ) -> LevelRoomPortalRecord {
        LevelRoomPortalRecord {
            source_room: RoomIndex(source),
            destination_room: RoomIndex(destination),
            kind: 0,
            normal_x: 0,
            normal_y: 0,
            normal_z: -1,
            vertex_x: [x0, x1, x1, x0],
            vertex_y: [0, 0, 2048, 2048],
            vertex_z: [z, z, z, z],
        }
    }

    fn forward_camera(z: i32) -> PortalVisibilityCamera {
        PortalVisibilityCamera::new(0, 1024, z, 0, -4096, 64, 16_384, 4096, 4)
    }

    #[test]
    fn accepts_front_facing_portal_in_camera_cone() {
        let rooms = [room(0, 0, 1), room(1, 1, 1)];
        let portals = [north_portal(0, 1, -1), north_portal(1, 0, 1)];
        let mut out = PortalVisibilityResult::<8, 16, 8>::EMPTY;

        build_portal_visibility(
            &rooms,
            &portals,
            RoomIndex(0),
            forward_camera(0),
            4,
            &mut out,
        );

        assert_eq!(out.room_count, 2);
        assert_eq!(out.rooms[0].room, RoomIndex(0));
        assert_eq!(out.rooms[1].room, RoomIndex(1));
        assert_eq!(out.stats.portals_tested, 2);
        assert_eq!(out.stats.portals_accepted, 1);
        assert_eq!(out.visible_room_mask(), 0b11);
    }

    #[test]
    fn rejects_portal_behind_camera() {
        let rooms = [room(0, 0, 1), room(1, 1, 0)];
        let portals = [north_portal(0, 1, -1)];
        let mut out = PortalVisibilityResult::<8, 16, 8>::EMPTY;

        build_portal_visibility(
            &rooms,
            &portals,
            RoomIndex(0),
            forward_camera(8192),
            4,
            &mut out,
        );

        assert_eq!(out.room_count, 1);
        assert_eq!(out.stats.reject_backface, 1);
    }

    #[test]
    fn records_frontier_rooms_beyond_visible_set() {
        let rooms = [room(0, 0, 1), room(1, 1, 1), room(2, 2, 0)];
        let portals = [
            north_portal(0, 1, -1),
            LevelRoomPortalRecord {
                source_room: RoomIndex(1),
                destination_room: RoomIndex(2),
                vertex_z: [8192, 8192, 8192, 8192],
                ..north_portal(1, 2, -1)
            },
        ];
        let mut out = PortalVisibilityResult::<2, 2, 8>::EMPTY;

        build_portal_visibility(
            &rooms,
            &portals,
            RoomIndex(0),
            forward_camera(0),
            1,
            &mut out,
        );

        assert_eq!(out.room_count, 2);
        assert_eq!(out.frontier_count, 1);
        assert_eq!(out.frontier_rooms[0].room, RoomIndex(2));
        assert_eq!(out.frontier_room_mask(), 0b100);
    }

    #[test]
    fn traverses_multiple_portal_frustums_into_same_room() {
        let rooms = [room(0, 0, 2), room(1, 2, 2), room(2, 4, 0), room(3, 4, 0)];
        let portals = [
            wall_portal(0, 1, -3000, -2000, 4096),
            wall_portal(0, 1, 2000, 3000, 4096),
            wall_portal(1, 2, -6000, -4000, 8192),
            wall_portal(1, 3, 4000, 6000, 8192),
        ];
        let mut out = PortalVisibilityResult::<8, 16, 8>::EMPTY;

        build_portal_visibility(
            &rooms,
            &portals,
            RoomIndex(0),
            forward_camera(0),
            4,
            &mut out,
        );

        assert_eq!(out.visible_room_mask(), 0b1111);
        assert_eq!(out.room_count, 4);
        assert_eq!(out.rooms[1].room, RoomIndex(1));
        assert_eq!(out.rooms[1].frustum_count, 2);
        assert_eq!(out.frustum_count, 5);
        assert_eq!(out.stats.portals_accepted, 4);
    }
}
