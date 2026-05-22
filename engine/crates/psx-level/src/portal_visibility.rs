//! Heap-free portal visibility traversal for cooked runtime rooms.
//!
//! The traversal clips each directed portal against the camera's current
//! screen-space portal window, then recurses with the clipped child window.
//! That mirrors the Tomb-style door traversal: a room is accepted because a
//! projected portal rectangle reaches it, not because the room's top-down
//! footprint intersects the camera cone.

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
    /// Camera pitch sine, Q12.
    pub sin_pitch_q12: i32,
    /// Camera pitch cosine, Q12.
    pub cos_pitch_q12: i32,
    /// Near plane in camera-space depth units.
    pub near_z: i32,
    /// Far plane in camera-space depth units.
    pub far_z: i32,
    /// Horizontal half field-of-view as `tan(angle)`, Q12.
    pub half_fov_x_tan_q12: i32,
    /// Vertical half field-of-view as `tan(angle)`, Q12.
    pub half_fov_y_tan_q12: i32,
    /// Minimum accepted clipped portal cone width, Q12 tangent units.
    pub min_portal_width_q12: i32,
    /// Recurse from accepted rooms with the full camera viewport instead of
    /// the clipped entry portal. Enable this for renderers that draw an
    /// accepted room whole rather than applying per-room viewport/scissor clips.
    pub whole_room_recursion: bool,
}

impl PortalVisibilityCamera {
    /// Build portal traversal camera inputs.
    pub const fn new(
        x: i32,
        y: i32,
        z: i32,
        sin_yaw_q12: i32,
        cos_yaw_q12: i32,
        sin_pitch_q12: i32,
        cos_pitch_q12: i32,
        near_z: i32,
        far_z: i32,
        half_fov_x_tan_q12: i32,
        half_fov_y_tan_q12: i32,
        min_portal_width_q12: i32,
    ) -> Self {
        Self {
            x,
            y,
            z,
            sin_yaw_q12,
            cos_yaw_q12,
            sin_pitch_q12,
            cos_pitch_q12,
            near_z,
            far_z,
            half_fov_x_tan_q12,
            half_fov_y_tan_q12,
            min_portal_width_q12,
            whole_room_recursion: false,
        }
    }

    /// Return a copy configured for whole-room recursive traversal.
    pub const fn with_whole_room_recursion(mut self, enabled: bool) -> Self {
        self.whole_room_recursion = enabled;
        self
    }
}

/// World-space occupied bounds for a runtime room cell used as a conservative
/// fallback when the renderer draws accepted rooms whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortalRoomBounds {
    /// Runtime room index these bounds belong to.
    pub room: RoomIndex,
    /// Minimum world X, inclusive.
    pub min_x: i32,
    /// Maximum world X, exclusive.
    pub max_x: i32,
    /// Minimum world Y.
    pub min_y: i32,
    /// Maximum world Y.
    pub max_y: i32,
    /// Minimum world Z, inclusive.
    pub min_z: i32,
    /// Maximum world Z, exclusive.
    pub max_z: i32,
}

impl PortalRoomBounds {
    /// Empty bounds slot.
    pub const EMPTY: Self = Self {
        room: INVALID_ROOM,
        min_x: 0,
        max_x: 0,
        min_y: 0,
        max_y: 0,
        min_z: 0,
        max_z: 0,
    };
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

/// One clipped portal viewport reaching a runtime room.
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
    /// Lower edge of the vertical clipped view, Q12 tangent units.
    pub min_y_tan_q12: i32,
    /// Upper edge of the vertical clipped view, Q12 tangent units.
    pub max_y_tan_q12: i32,
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
        min_y_tan_q12: 0,
        max_y_tan_q12: 0,
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
    /// Portals rejected by camera-plane/window clipping.
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
    /// Destination-room bitset for portals considered by the traversal.
    pub tested_room_mask: u64,
    /// Destination-room bitset for portals accepted by the traversal.
    pub accepted_room_mask: u64,
    /// Destination-room bitset for portals rejected by camera/window clipping.
    pub reject_frustum_room_mask: u64,
    /// Portals accepted by occupied-room-bounds fallback.
    pub bounds_fallbacks: u16,
    /// Destination-room bitset for occupied-room-bounds fallback accepts.
    pub bounds_fallback_room_mask: u64,
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
            tested_room_mask: 0,
            accepted_room_mask: 0,
            reject_frustum_room_mask: 0,
            bounds_fallbacks: 0,
            bounds_fallback_room_mask: 0,
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
                && existing.min_y_tan_q12 <= frustum.min_y_tan_q12
                && existing.max_y_tan_q12 >= frustum.max_y_tan_q12
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
    build_portal_visibility_with_room_bounds(
        rooms,
        portals,
        &[],
        current_room,
        camera,
        max_depth,
        out,
    );
}

/// Build the portal-visible room set with occupied-cell bounds fallback.
pub fn build_portal_visibility_with_room_bounds<
    const MAX_ROOMS: usize,
    const MAX_FRUSTUMS: usize,
    const MAX_FRONTIER: usize,
>(
    rooms: &[LevelRoomRecord],
    portals: &[LevelRoomPortalRecord],
    room_bounds: &[PortalRoomBounds],
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
    let half_fov_x = camera.half_fov_x_tan_q12.max(1);
    let half_fov_y = camera.half_fov_y_tan_q12.max(1);
    let root = PortalFrustum {
        room: current_room,
        source_room: INVALID_ROOM,
        source_portal: INVALID_PORTAL,
        depth: 0,
        left_tan_q12: -half_fov_x,
        right_tan_q12: half_fov_x,
        min_y_tan_q12: -half_fov_y,
        max_y_tan_q12: half_fov_y,
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
            out.stats.tested_room_mask |= room_mask_bit(portal.destination_room);
            if portal.destination_room == current_room
                || portal.destination_room == frustum.source_room
            {
                continue;
            }
            if !portal_front_faces_camera(portal, camera) {
                out.stats.reject_backface = out.stats.reject_backface.saturating_add(1);
                continue;
            }
            let child_clip = match clipped_portal_clip(portal, camera, frustum) {
                Some(child_clip) => child_clip,
                None => {
                    if camera.whole_room_recursion
                        && room_bounds_intersects_camera_window(
                            portal.destination_room,
                            room_bounds,
                            camera,
                            frustum,
                        )
                    {
                        out.stats.bounds_fallbacks = out.stats.bounds_fallbacks.saturating_add(1);
                        out.stats.bounds_fallback_room_mask |=
                            room_mask_bit(portal.destination_room);
                        PortalClip::full_camera(camera)
                    } else {
                        out.stats.reject_frustum = out.stats.reject_frustum.saturating_add(1);
                        out.stats.reject_frustum_room_mask |=
                            room_mask_bit(portal.destination_room);
                        continue;
                    }
                }
            };
            if portal_clip_is_tiny(child_clip, camera.min_portal_width_q12.max(0)) {
                out.stats.reject_tiny = out.stats.reject_tiny.saturating_add(1);
                continue;
            }
            let child_depth = frustum.depth.saturating_add(1);
            let traversal_clip = if camera.whole_room_recursion {
                PortalClip::full_camera(camera)
            } else {
                child_clip
            };
            let child = PortalFrustum {
                room: portal.destination_room,
                source_room: portal.source_room,
                source_portal: portal_index.saturating_sub(1).min(u16::MAX as usize) as u16,
                depth: child_depth,
                left_tan_q12: traversal_clip.left_tan_q12,
                right_tan_q12: traversal_clip.right_tan_q12,
                min_y_tan_q12: traversal_clip.min_y_tan_q12,
                max_y_tan_q12: traversal_clip.max_y_tan_q12,
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
                out.stats.accepted_room_mask |= room_mask_bit(portal.destination_room);
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
struct PortalClip {
    left_tan_q12: i32,
    right_tan_q12: i32,
    min_y_tan_q12: i32,
    max_y_tan_q12: i32,
}

impl PortalClip {
    fn full_camera(camera: PortalVisibilityCamera) -> Self {
        let half_fov_x = camera.half_fov_x_tan_q12.max(1);
        let half_fov_y = camera.half_fov_y_tan_q12.max(1);
        Self {
            left_tan_q12: -half_fov_x,
            right_tan_q12: half_fov_x,
            min_y_tan_q12: -half_fov_y,
            max_y_tan_q12: half_fov_y,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PortalViewVertex {
    x: i64,
    y: i64,
    z: i64,
}

impl PortalViewVertex {
    const ZERO: Self = Self { x: 0, y: 0, z: 0 };
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

fn clipped_portal_clip(
    portal: LevelRoomPortalRecord,
    camera: PortalVisibilityCamera,
    parent: PortalFrustum,
) -> Option<PortalClip> {
    let mut vertices = [PortalViewVertex::ZERO; 4];
    let mut bounds = PortalClipBounds::EMPTY;
    let camera_plane = 1i64;
    let mut behind_count = 0u8;

    let mut i = 0usize;
    while i < 4 {
        let vertex = portal_view_vertex(portal, camera, i);
        vertices[i] = vertex;
        if vertex.z > 0 {
            bounds.include_vertex(vertex);
        } else if vertex.z <= 0 {
            behind_count = behind_count.saturating_add(1);
        }
        i += 1;
    }

    if behind_count == 4 {
        return None;
    }

    let mut edge = 0usize;
    while edge < 4 {
        let next = (edge + 1) & 3;
        include_camera_plane_crossing(
            &mut bounds,
            vertices[edge],
            vertices[next],
            camera.half_fov_x_tan_q12.max(1),
            camera.half_fov_y_tan_q12.max(1),
        );
        include_depth_crossing(&mut bounds, vertices[edge], vertices[next], camera_plane);
        edge += 1;
    }

    if !bounds.valid() {
        return None;
    }
    let left = bounds.x.min_q12.max(parent.left_tan_q12);
    let right = bounds.x.max_q12.min(parent.right_tan_q12);
    let min_y = bounds.y.min_q12.max(parent.min_y_tan_q12);
    let max_y = bounds.y.max_q12.min(parent.max_y_tan_q12);
    (left <= right && min_y <= max_y).then_some(PortalClip {
        left_tan_q12: left,
        right_tan_q12: right,
        min_y_tan_q12: min_y,
        max_y_tan_q12: max_y,
    })
}

fn room_bounds_intersects_camera_window(
    room: RoomIndex,
    bounds: &[PortalRoomBounds],
    camera: PortalVisibilityCamera,
    parent: PortalFrustum,
) -> bool {
    let mut i = 0usize;
    while i < bounds.len() {
        let cell = bounds[i];
        if cell.room == room && one_bounds_intersects_camera_window(cell, camera, parent) {
            return true;
        }
        i += 1;
    }
    false
}

fn one_bounds_intersects_camera_window(
    bounds: PortalRoomBounds,
    camera: PortalVisibilityCamera,
    parent: PortalFrustum,
) -> bool {
    if bounds.max_x <= bounds.min_x || bounds.max_y <= bounds.min_y || bounds.max_z <= bounds.min_z
    {
        return false;
    }

    let mut any_in_front = false;
    let mut all_right = true;
    let mut all_left = true;
    let mut all_above = true;
    let mut all_below = true;
    for x in [bounds.min_x, bounds.max_x] {
        for y in [bounds.min_y, bounds.max_y] {
            for z in [bounds.min_z, bounds.max_z] {
                let view = world_view_vertex(x, y, z, camera);
                if view.z <= 0 {
                    all_right = false;
                    all_left = false;
                    all_above = false;
                    all_below = false;
                    continue;
                }
                any_in_front = true;
                let x_slope = clamp_i64_to_i32(
                    view.x.saturating_mul(Q12_ONE) / view.z,
                    -SLOPE_LIMIT_Q12,
                    SLOPE_LIMIT_Q12,
                );
                let y_slope = clamp_i64_to_i32(
                    view.y.saturating_mul(Q12_ONE) / view.z,
                    -SLOPE_LIMIT_Q12,
                    SLOPE_LIMIT_Q12,
                );
                if x_slope <= parent.right_tan_q12 {
                    all_right = false;
                }
                if x_slope >= parent.left_tan_q12 {
                    all_left = false;
                }
                if y_slope <= parent.max_y_tan_q12 {
                    all_above = false;
                }
                if y_slope >= parent.min_y_tan_q12 {
                    all_below = false;
                }
            }
        }
    }
    any_in_front && !(all_right || all_left || all_above || all_below)
}

fn portal_view_vertex(
    portal: LevelRoomPortalRecord,
    camera: PortalVisibilityCamera,
    index: usize,
) -> PortalViewVertex {
    let dx = (portal.vertex_x[index] as i64).saturating_sub(camera.x as i64);
    let dy = (portal.vertex_y[index] as i64).saturating_sub(camera.y as i64);
    let dz = (portal.vertex_z[index] as i64).saturating_sub(camera.z as i64);
    world_view_delta(dx, dy, dz, camera)
}

fn world_view_vertex(x: i32, y: i32, z: i32, camera: PortalVisibilityCamera) -> PortalViewVertex {
    let dx = (x as i64).saturating_sub(camera.x as i64);
    let dy = (y as i64).saturating_sub(camera.y as i64);
    let dz = (z as i64).saturating_sub(camera.z as i64);
    world_view_delta(dx, dy, dz, camera)
}

fn world_view_delta(dx: i64, dy: i64, dz: i64, camera: PortalVisibilityCamera) -> PortalViewVertex {
    let sin_yaw = camera.sin_yaw_q12 as i64;
    let cos_yaw = camera.cos_yaw_q12 as i64;
    let sin_pitch = camera.sin_pitch_q12 as i64;
    let cos_pitch = camera.cos_pitch_q12 as i64;
    let x1 = dx
        .saturating_mul(cos_yaw)
        .saturating_sub(dz.saturating_mul(sin_yaw))
        >> Q12_SHIFT;
    let z1 = dx
        .saturating_mul(-sin_yaw)
        .saturating_sub(dz.saturating_mul(cos_yaw))
        >> Q12_SHIFT;
    let y2 = dy
        .saturating_mul(cos_pitch)
        .saturating_sub(z1.saturating_mul(sin_pitch))
        >> Q12_SHIFT;
    let z2 = dy
        .saturating_mul(sin_pitch)
        .saturating_add(z1.saturating_mul(cos_pitch))
        >> Q12_SHIFT;

    PortalViewVertex {
        x: x1,
        y: y2,
        z: z2,
    }
}

fn include_depth_crossing(
    bounds: &mut PortalClipBounds,
    a: PortalViewVertex,
    b: PortalViewVertex,
    clip_depth: i64,
) {
    let crosses =
        (a.z < clip_depth && b.z >= clip_depth) || (b.z < clip_depth && a.z >= clip_depth);
    if !crosses {
        return;
    }
    let denom = b.z.saturating_sub(a.z);
    if denom == 0 {
        return;
    }
    let num = clip_depth.saturating_sub(a.z);
    let x =
        a.x.saturating_add(b.x.saturating_sub(a.x).saturating_mul(num) / denom);
    let y =
        a.y.saturating_add(b.y.saturating_sub(a.y).saturating_mul(num) / denom);
    bounds.include_vertex(PortalViewVertex {
        x,
        y,
        z: clip_depth.max(1),
    });
}

fn include_camera_plane_crossing(
    bounds: &mut PortalClipBounds,
    a: PortalViewVertex,
    b: PortalViewVertex,
    half_fov_x_tan_q12: i32,
    half_fov_y_tan_q12: i32,
) {
    if !((a.z <= 0 && b.z > 0) || (b.z <= 0 && a.z > 0)) {
        return;
    }

    if a.x < 0 && b.x < 0 {
        bounds.include_x_min(-half_fov_x_tan_q12);
    } else if a.x > 0 && b.x > 0 {
        bounds.include_x_max(half_fov_x_tan_q12);
    } else {
        bounds.include_x_range(-half_fov_x_tan_q12, half_fov_x_tan_q12);
    }

    if a.y < 0 && b.y < 0 {
        bounds.include_y_min(-half_fov_y_tan_q12);
    } else if a.y > 0 && b.y > 0 {
        bounds.include_y_max(half_fov_y_tan_q12);
    } else {
        bounds.include_y_range(-half_fov_y_tan_q12, half_fov_y_tan_q12);
    }
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
        self.include_value(slope);
    }

    fn include_value(&mut self, slope: i32) {
        if !self.valid {
            self.min_q12 = slope;
            self.max_q12 = slope;
            self.valid = true;
        } else {
            self.min_q12 = self.min_q12.min(slope);
            self.max_q12 = self.max_q12.max(slope);
        }
    }

    fn include_min(&mut self, slope: i32) {
        if !self.valid {
            self.min_q12 = slope;
            self.max_q12 = slope;
            self.valid = true;
        } else {
            self.min_q12 = self.min_q12.min(slope);
        }
    }

    fn include_max(&mut self, slope: i32) {
        if !self.valid {
            self.min_q12 = slope;
            self.max_q12 = slope;
            self.valid = true;
        } else {
            self.max_q12 = self.max_q12.max(slope);
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PortalClipBounds {
    x: PortalSlopeInterval,
    y: PortalSlopeInterval,
}

impl PortalClipBounds {
    const EMPTY: Self = Self {
        x: PortalSlopeInterval::EMPTY,
        y: PortalSlopeInterval::EMPTY,
    };

    fn include_vertex(&mut self, vertex: PortalViewVertex) {
        self.x.include_slope(vertex.x, vertex.z);
        self.y.include_slope(vertex.y, vertex.z);
    }

    fn include_x_min(&mut self, value: i32) {
        self.x.include_min(value);
    }

    fn include_x_max(&mut self, value: i32) {
        self.x.include_max(value);
    }

    fn include_x_range(&mut self, min: i32, max: i32) {
        self.x.include_value(min);
        self.x.include_value(max);
    }

    fn include_y_min(&mut self, value: i32) {
        self.y.include_min(value);
    }

    fn include_y_max(&mut self, value: i32) {
        self.y.include_max(value);
    }

    fn include_y_range(&mut self, min: i32, max: i32) {
        self.y.include_value(min);
        self.y.include_value(max);
    }

    fn valid(self) -> bool {
        self.x.valid && self.y.valid
    }
}

fn portal_clip_is_tiny(clip: PortalClip, min_size_q12: i32) -> bool {
    if min_size_q12 <= 0 {
        return false;
    }
    let width = clip.right_tan_q12.saturating_sub(clip.left_tan_q12);
    let height = clip.max_y_tan_q12.saturating_sub(clip.min_y_tan_q12);
    width < min_size_q12 && height < min_size_q12
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
        wall_portal_with_y(source, destination, x0, x1, z, 0, 2048)
    }

    const fn wall_portal_with_y(
        source: u16,
        destination: u16,
        x0: i32,
        x1: i32,
        z: i32,
        y0: i32,
        y1: i32,
    ) -> LevelRoomPortalRecord {
        LevelRoomPortalRecord {
            source_room: RoomIndex(source),
            destination_room: RoomIndex(destination),
            kind: 0,
            normal_x: 0,
            normal_y: 0,
            normal_z: -1,
            vertex_x: [x0, x1, x1, x0],
            vertex_y: [y0, y0, y1, y1],
            vertex_z: [z, z, z, z],
        }
    }

    fn forward_camera(z: i32) -> PortalVisibilityCamera {
        PortalVisibilityCamera::new(0, 1024, z, 0, -4096, 0, 4096, 64, 16_384, 4096, 3072, 4)
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
        assert_eq!(out.stats.tested_room_mask, 0b11);
        assert_eq!(out.stats.accepted_room_mask, 0b10);
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

    #[test]
    fn rejects_portal_outside_vertical_camera_view() {
        let rooms = [room(0, 0, 1), room(1, 1, 0)];
        let portals = [wall_portal(0, 1, -1024, 1024, 4096)];
        let camera =
            PortalVisibilityCamera::new(0, 8192, 0, 0, -4096, 0, 4096, 64, 16_384, 4096, 1024, 4);
        let mut out = PortalVisibilityResult::<8, 16, 8>::EMPTY;

        build_portal_visibility(&rooms, &portals, RoomIndex(0), camera, 4, &mut out);

        assert_eq!(out.room_count, 1);
        assert_eq!(out.stats.reject_frustum, 1);
        assert_eq!(out.stats.reject_frustum_room_mask, 0b10);
    }

    #[test]
    fn accepts_portal_closer_than_render_near_plane() {
        let rooms = [room(0, 0, 1), room(1, 1, 0)];
        let portals = [wall_portal(0, 1, -1024, 1024, 32)];
        let mut out = PortalVisibilityResult::<8, 16, 8>::EMPTY;

        build_portal_visibility(
            &rooms,
            &portals,
            RoomIndex(0),
            forward_camera(0),
            4,
            &mut out,
        );

        assert_eq!(out.visible_room_mask(), 0b11);
        assert_eq!(out.stats.portals_accepted, 1);
    }

    #[test]
    fn accepts_projected_portal_beyond_render_far_plane() {
        let rooms = [room(0, 0, 1), room(1, 1, 0)];
        let portals = [wall_portal(0, 1, -1024, 1024, 8192)];
        let camera =
            PortalVisibilityCamera::new(0, 1024, 0, 0, -4096, 0, 4096, 64, 2048, 4096, 3072, 4);
        let mut out = PortalVisibilityResult::<8, 16, 8>::EMPTY;

        build_portal_visibility(&rooms, &portals, RoomIndex(0), camera, 4, &mut out);

        assert_eq!(out.visible_room_mask(), 0b11);
        assert_eq!(out.stats.portals_accepted, 1);
    }

    #[test]
    fn recursive_portal_visibility_uses_parent_vertical_clip() {
        let rooms = [room(0, 0, 1), room(1, 1, 1), room(2, 2, 0)];
        let portals = [
            wall_portal_with_y(0, 1, -1024, 1024, 4096, 0, 1024),
            wall_portal_with_y(1, 2, -1024, 1024, 8192, 4096, 5120),
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

        assert_eq!(out.visible_room_mask(), 0b11);
        assert_eq!(out.frontier_room_mask(), 0b100);
        assert_eq!(out.stats.portals_accepted, 1);
        assert_eq!(out.stats.reject_frustum, 1);
    }

    #[test]
    fn whole_room_recursion_keeps_portals_visible_in_whole_room_renderer() {
        let rooms = [room(0, 0, 1), room(1, 1, 1), room(2, 2, 0)];
        let portals = [
            wall_portal(0, 1, -3800, -3000, 4096),
            wall_portal(1, 2, 3000, 3800, 8192),
        ];
        let camera = forward_camera(0).with_whole_room_recursion(true);
        let mut out = PortalVisibilityResult::<8, 16, 8>::EMPTY;

        build_portal_visibility(&rooms, &portals, RoomIndex(0), camera, 4, &mut out);

        assert_eq!(out.visible_room_mask(), 0b111);
        assert_eq!(out.stats.portals_accepted, 2);
        assert_eq!(out.stats.reject_frustum, 0);
    }

    #[test]
    fn whole_room_renderer_can_fallback_to_occupied_room_bounds() {
        let rooms = [room(0, 0, 1), room(1, 1, 0)];
        let portals = [wall_portal(0, 1, 6000, 7000, 4096)];
        let bounds = [PortalRoomBounds {
            room: RoomIndex(1),
            min_x: -512,
            max_x: 512,
            min_y: 0,
            max_y: 2048,
            min_z: 4096,
            max_z: 6144,
        }];
        let camera = forward_camera(0).with_whole_room_recursion(true);
        let mut out = PortalVisibilityResult::<8, 16, 8>::EMPTY;

        build_portal_visibility_with_room_bounds(
            &rooms,
            &portals,
            &bounds,
            RoomIndex(0),
            camera,
            4,
            &mut out,
        );

        assert_eq!(out.visible_room_mask(), 0b11);
        assert_eq!(out.stats.portals_accepted, 1);
        assert_eq!(out.stats.reject_frustum, 0);
        assert_eq!(out.stats.bounds_fallbacks, 1);
        assert_eq!(out.stats.bounds_fallback_room_mask, 0b10);
    }
}
