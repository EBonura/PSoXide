//! Heap-free portal visibility traversal for cooked runtime rooms.
//!
//! The traversal clips each directed portal against the camera's current
//! screen-space portal window, then recurses with the clipped child window.
//! That mirrors the Tomb-style door traversal: a room is accepted because a
//! projected portal rectangle reaches it, not because the room's top-down
//! footprint intersects the camera cone.

use crate::{LevelRoomPortalRecord, LevelRoomRecord, RoomIndex, RuntimeDebugMask};

const INVALID_ROOM: RoomIndex = RoomIndex(u16::MAX);
const INVALID_PORTAL: u16 = u16::MAX;
const Q12_SHIFT: i32 = 12;
const Q12_ONE: i32 = 1 << Q12_SHIFT;
const SLOPE_LIMIT_Q12: i32 = 64 * 4096;
const PORTAL_SCREEN_PAD_Q12: i32 = 16;
const PORTAL_CLIP_VERTEX_CAPACITY: usize = 16;

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
}

impl PortalVisibilityCamera {
    /// Build portal traversal camera inputs.
    #[allow(clippy::too_many_arguments)]
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
        }
    }
}

/// World-space occupied bounds for a runtime room cell.
///
/// The portal traversal does not use these bounds for visibility; the type is
/// retained for callers that already collect room occupancy diagnostics.
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

/// One portal vertex after transforming it into camera/view space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortalClipDebugVertex {
    /// Horizontal view-space coordinate.
    pub x: i32,
    /// Vertical view-space coordinate.
    pub y: i32,
    /// Forward view-space depth.
    pub z: i32,
}

impl PortalClipDebugVertex {
    const fn from_view(vertex: PortalViewVertex) -> Self {
        Self {
            x: vertex.x,
            y: vertex.y,
            z: vertex.z,
        }
    }
}

/// Screen-space tangent window produced by portal clipping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortalClipDebugRect {
    /// Left horizontal tangent, Q12.
    pub left_tan_q12: i32,
    /// Right horizontal tangent, Q12.
    pub right_tan_q12: i32,
    /// Lower vertical tangent, Q12.
    pub min_y_tan_q12: i32,
    /// Upper vertical tangent, Q12.
    pub max_y_tan_q12: i32,
}

impl PortalClipDebugRect {
    const fn from_clip(clip: PortalClip) -> Self {
        Self {
            left_tan_q12: clip.left_tan_q12,
            right_tan_q12: clip.right_tan_q12,
            min_y_tan_q12: clip.min_y_tan_q12,
            max_y_tan_q12: clip.max_y_tan_q12,
        }
    }

    const fn from_frustum(frustum: PortalFrustum) -> Self {
        Self {
            left_tan_q12: frustum.left_tan_q12,
            right_tan_q12: frustum.right_tan_q12,
            min_y_tan_q12: frustum.min_y_tan_q12,
            max_y_tan_q12: frustum.max_y_tan_q12,
        }
    }
}

/// Portal clip plane that emptied the polygon first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalClipDebugPlane {
    /// The polygon survived every clip plane.
    None,
    /// Near-depth clipping emptied the polygon.
    Near,
    /// Left window clipping emptied the polygon.
    Left,
    /// Right window clipping emptied the polygon.
    Right,
    /// Bottom window clipping emptied the polygon.
    Bottom,
    /// Top window clipping emptied the polygon.
    Top,
}

/// Final runtime decision for a portal clip diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalClipDebugDecision {
    /// The portal produced a non-tiny child clip window.
    Accepted,
    /// The portal was rejected by the source-facing backface test.
    Backface,
    /// No projectable part of the portal was in front of the near plane.
    EmptyProjection,
    /// The projected portal did not overlap the inherited parent window.
    NoWindowOverlap,
    /// The portal overlapped but the resulting clip was below the minimum size.
    Tiny,
}

/// Exact intermediate values for one directed portal visibility test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortalClipDebug {
    /// Parent portal window inherited by this test.
    pub parent: PortalClipDebugRect,
    /// Parent window expanded by the small screen-space pad used before clipping.
    pub padded_parent: PortalClipDebugRect,
    /// Whether the directed portal faces the camera.
    pub front_faces_camera: bool,
    /// Portal vertices after view transform.
    pub view_vertices: [PortalClipDebugVertex; 4],
    /// Polygon vertex count after near clipping.
    pub near_count: u8,
    /// Polygon vertex count after left clipping.
    pub left_count: u8,
    /// Polygon vertex count after right clipping.
    pub right_count: u8,
    /// Polygon vertex count after bottom clipping.
    pub bottom_count: u8,
    /// Polygon vertex count after top clipping.
    pub top_count: u8,
    /// First clip plane that emptied the polygon, if any.
    pub first_empty_plane: PortalClipDebugPlane,
    /// Projected bounds before clamping to the inherited parent window.
    pub projected_bounds: Option<PortalClipDebugRect>,
    /// Bounds from the fully clipped polygon, clamped to the parent window.
    pub clipped_bounds: Option<PortalClipDebugRect>,
    /// Bounds from the projection fallback, clamped to the parent window.
    pub fallback_bounds: Option<PortalClipDebugRect>,
    /// Final child clip window used by traversal, if accepted before tiny rejection.
    pub result_bounds: Option<PortalClipDebugRect>,
    /// Whether the final child clip failed the minimum-size test.
    pub tiny: bool,
    /// Final visibility decision matching the runtime traversal.
    pub decision: PortalClipDebugDecision,
}

/// One portal-clipped room beyond the currently accepted set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortalFrontierRoom {
    /// Room that passed portal clipping but was not accepted because traversal
    /// depth or fixed-pool capacity stopped expansion.
    pub room: RoomIndex,
    /// Accepted source room that owns the frontier portal.
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
    pub tested_room_mask: RuntimeDebugMask,
    /// Destination-room bitset for portals accepted by the traversal.
    pub accepted_room_mask: RuntimeDebugMask,
    /// Destination-room bitset for portals rejected by camera/window clipping.
    pub reject_frustum_room_mask: RuntimeDebugMask,
    /// Directed portal-record bitset for portals considered by the traversal.
    pub tested_portal_mask: RuntimeDebugMask,
    /// Directed portal-record bitset for portals accepted by the traversal.
    pub accepted_portal_mask: RuntimeDebugMask,
    /// Directed portal-record bitset for portals rejected by camera/window clipping.
    pub reject_frustum_portal_mask: RuntimeDebugMask,
    /// Deprecated: occupied-room bounds no longer rescue rejected portals.
    pub bounds_fallbacks: u16,
    /// Deprecated: always zero while portal visibility is surface-clipped.
    pub bounds_fallback_room_mask: RuntimeDebugMask,
    /// Deprecated: always zero while portal visibility is surface-clipped.
    pub bounds_fallback_portal_mask: RuntimeDebugMask,
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
            tested_room_mask: RuntimeDebugMask::EMPTY,
            accepted_room_mask: RuntimeDebugMask::EMPTY,
            reject_frustum_room_mask: RuntimeDebugMask::EMPTY,
            tested_portal_mask: RuntimeDebugMask::EMPTY,
            accepted_portal_mask: RuntimeDebugMask::EMPTY,
            reject_frustum_portal_mask: RuntimeDebugMask::EMPTY,
            bounds_fallbacks: 0,
            bounds_fallback_room_mask: RuntimeDebugMask::EMPTY,
            bounds_fallback_portal_mask: RuntimeDebugMask::EMPTY,
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
    pub fn visible_room_mask(&self) -> RuntimeDebugMask {
        let mut mask = RuntimeDebugMask::EMPTY;
        let mut i = 0usize;
        while i < self.room_count.min(MAX_ROOMS) {
            mask.insert_room(self.rooms[i].room);
            i += 1;
        }
        mask
    }

    /// Bit mask of frontier runtime rooms for debug telemetry.
    pub fn frontier_room_mask(&self) -> RuntimeDebugMask {
        let mut mask = RuntimeDebugMask::EMPTY;
        let mut i = 0usize;
        while i < self.frontier_count.min(MAX_FRONTIER) {
            mask.insert_room(self.frontier_rooms[i].room);
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

/// Build the portal-visible room set.
///
/// `room_bounds` is accepted for compatibility with callers that already
/// collect room occupancy diagnostics. Visibility itself follows the
/// Tomb-style rule: only the directed portal surface can open the next room.
pub fn build_portal_visibility_with_room_bounds<
    const MAX_ROOMS: usize,
    const MAX_FRUSTUMS: usize,
    const MAX_FRONTIER: usize,
>(
    rooms: &[LevelRoomRecord],
    portals: &[LevelRoomPortalRecord],
    _room_bounds: &[PortalRoomBounds],
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
            collect_frontier_from_capped_frustum(
                rooms,
                portals,
                current_room,
                camera,
                frustum,
                out,
            );
            continue;
        }
        let record = rooms[frustum.room.to_usize()];
        let portal_start = record.portal_first as usize;
        let portal_end = portal_start.saturating_add(record.portal_count as usize);
        let mut portal_index = portal_start;
        while portal_index < portal_end.min(portals.len()) {
            let current_portal_index = portal_index;
            let portal = portals[portal_index];
            portal_index += 1;
            if portal.source_room != frustum.room {
                continue;
            }
            let portal_mask = portal_mask_bit(current_portal_index);
            out.stats.portals_tested = out.stats.portals_tested.saturating_add(1);
            out.stats
                .tested_room_mask
                .insert_room(portal.destination_room);
            out.stats.tested_portal_mask |= portal_mask;
            if portal.destination_room == current_room
                || portal.destination_room == frustum.source_room
            {
                continue;
            }
            if !portal_front_faces_camera(portal, camera) {
                out.stats.reject_backface = out.stats.reject_backface.saturating_add(1);
                continue;
            }
            let Some(child_clip) = clipped_portal_clip(portal, camera, frustum) else {
                out.stats.reject_frustum = out.stats.reject_frustum.saturating_add(1);
                out.stats
                    .reject_frustum_room_mask
                    .insert_room(portal.destination_room);
                out.stats.reject_frustum_portal_mask |= portal_mask;
                continue;
            };
            let child_clip = if portal_clip_is_tiny(child_clip, camera.min_portal_width_q12.max(0))
            {
                out.stats.reject_tiny = out.stats.reject_tiny.saturating_add(1);
                continue;
            } else {
                child_clip
            };
            let child_depth = frustum.depth.saturating_add(1);
            let child = PortalFrustum {
                room: portal.destination_room,
                source_room: portal.source_room,
                source_portal: current_portal_index.min(u16::MAX as usize) as u16,
                depth: child_depth,
                left_tan_q12: child_clip.left_tan_q12,
                right_tan_q12: child_clip.right_tan_q12,
                min_y_tan_q12: child_clip.min_y_tan_q12,
                max_y_tan_q12: child_clip.max_y_tan_q12,
            };
            if out.contains_redundant_frustum(child) {
                continue;
            }
            if out.frustum_count >= MAX_FRUSTUMS {
                out.stats.cap_frustum = out.stats.cap_frustum.saturating_add(1);
                out.push_frontier(portal.destination_room, portal.source_room);
                continue;
            }
            let Some(room_slot) = out.push_visible_room(portal.destination_room, child_depth)
            else {
                out.push_frontier(portal.destination_room, portal.source_room);
                continue;
            };
            if out.push_frustum(room_slot, child) {
                out.stats.portals_accepted = out.stats.portals_accepted.saturating_add(1);
                out.stats
                    .accepted_room_mask
                    .insert_room(portal.destination_room);
                out.stats.accepted_portal_mask |= portal_mask;
                out.stats.max_depth = out.stats.max_depth.max(child_depth);
            }
        }
    }
}

fn collect_frontier_from_capped_frustum<
    const MAX_ROOMS: usize,
    const MAX_FRUSTUMS: usize,
    const MAX_FRONTIER: usize,
>(
    rooms: &[LevelRoomRecord],
    portals: &[LevelRoomPortalRecord],
    current_room: RoomIndex,
    camera: PortalVisibilityCamera,
    frustum: PortalFrustum,
    out: &mut PortalVisibilityResult<MAX_ROOMS, MAX_FRUSTUMS, MAX_FRONTIER>,
) {
    if frustum.room.to_usize() >= rooms.len() {
        return;
    }
    let record = rooms[frustum.room.to_usize()];
    let portal_start = record.portal_first as usize;
    let portal_end = portal_start.saturating_add(record.portal_count as usize);
    let mut portal_index = portal_start;
    while portal_index < portal_end.min(portals.len()) {
        let portal = portals[portal_index];
        if portal.source_room == frustum.room
            && portal.destination_room != current_room
            && portal.destination_room != frustum.source_room
            && portal_passes_camera_window(portal, camera, frustum)
        {
            out.push_frontier(portal.destination_room, portal.source_room);
        }
        portal_index += 1;
    }
}

fn portal_passes_camera_window(
    portal: LevelRoomPortalRecord,
    camera: PortalVisibilityCamera,
    frustum: PortalFrustum,
) -> bool {
    if !portal_front_faces_camera(portal, camera) {
        return false;
    }
    let Some(child_clip) = clipped_portal_clip(portal, camera, frustum) else {
        return false;
    };
    !portal_clip_is_tiny(child_clip, camera.min_portal_width_q12.max(0))
}

/// Return exact intermediate clip values for one directed portal test.
pub fn debug_portal_clip(
    portal: LevelRoomPortalRecord,
    camera: PortalVisibilityCamera,
    parent: PortalFrustum,
) -> PortalClipDebug {
    let mut vertices = [PortalViewVertex::ZERO; PORTAL_CLIP_VERTEX_CAPACITY];
    let mut scratch = [PortalViewVertex::ZERO; PORTAL_CLIP_VERTEX_CAPACITY];
    let mut view_vertices = [PortalClipDebugVertex { x: 0, y: 0, z: 0 }; 4];
    let mut i = 0usize;
    while i < 4 {
        let vertex = portal_view_vertex(portal, camera, i);
        vertices[i] = vertex;
        view_vertices[i] = PortalClipDebugVertex::from_view(vertex);
        i += 1;
    }

    let clip_left = parent.left_tan_q12.saturating_sub(PORTAL_SCREEN_PAD_Q12);
    let clip_right = parent.right_tan_q12.saturating_add(PORTAL_SCREEN_PAD_Q12);
    let clip_bottom = parent.min_y_tan_q12.saturating_sub(PORTAL_SCREEN_PAD_Q12);
    let clip_top = parent.max_y_tan_q12.saturating_add(PORTAL_SCREEN_PAD_Q12);
    let padded_parent = PortalClipDebugRect {
        left_tan_q12: clip_left,
        right_tan_q12: clip_right,
        min_y_tan_q12: clip_bottom,
        max_y_tan_q12: clip_top,
    };

    let mut first_empty_plane = PortalClipDebugPlane::None;
    let mut count = 4usize;
    count = clip_portal_polygon_against_plane(
        &mut vertices,
        &mut scratch,
        count,
        PortalClipPlane::Near(1),
    );
    let near_count = count.min(u8::MAX as usize) as u8;
    if count == 0 {
        first_empty_plane = PortalClipDebugPlane::Near;
    }
    count = clip_portal_polygon_against_plane(
        &mut vertices,
        &mut scratch,
        count,
        PortalClipPlane::Left(clip_left),
    );
    let left_count = count.min(u8::MAX as usize) as u8;
    if count == 0 && first_empty_plane == PortalClipDebugPlane::None {
        first_empty_plane = PortalClipDebugPlane::Left;
    }
    count = clip_portal_polygon_against_plane(
        &mut vertices,
        &mut scratch,
        count,
        PortalClipPlane::Right(clip_right),
    );
    let right_count = count.min(u8::MAX as usize) as u8;
    if count == 0 && first_empty_plane == PortalClipDebugPlane::None {
        first_empty_plane = PortalClipDebugPlane::Right;
    }
    count = clip_portal_polygon_against_plane(
        &mut vertices,
        &mut scratch,
        count,
        PortalClipPlane::Bottom(clip_bottom),
    );
    let bottom_count = count.min(u8::MAX as usize) as u8;
    if count == 0 && first_empty_plane == PortalClipDebugPlane::None {
        first_empty_plane = PortalClipDebugPlane::Bottom;
    }
    count = clip_portal_polygon_against_plane(
        &mut vertices,
        &mut scratch,
        count,
        PortalClipPlane::Top(clip_top),
    );
    let top_count = count.min(u8::MAX as usize) as u8;
    if count == 0 && first_empty_plane == PortalClipDebugPlane::None {
        first_empty_plane = PortalClipDebugPlane::Top;
    }

    let clipped_bounds = clipped_portal_surface_polygon_clip(portal, camera, parent)
        .map(PortalClipDebugRect::from_clip);
    let projected_bounds =
        projected_portal_surface_bounds(portal, camera).map(PortalClipDebugRect::from_clip);
    let fallback_bounds = projected_portal_surface_bounds_clip(portal, camera, parent)
        .map(PortalClipDebugRect::from_clip);
    let result_bounds = fallback_bounds;
    let tiny = result_bounds
        .map(|clip| {
            portal_clip_is_tiny(
                PortalClip {
                    left_tan_q12: clip.left_tan_q12,
                    right_tan_q12: clip.right_tan_q12,
                    min_y_tan_q12: clip.min_y_tan_q12,
                    max_y_tan_q12: clip.max_y_tan_q12,
                },
                camera.min_portal_width_q12.max(0),
            )
        })
        .unwrap_or(false);
    let front_faces_camera = portal_front_faces_camera(portal, camera);
    let decision = if !front_faces_camera {
        PortalClipDebugDecision::Backface
    } else if result_bounds.is_none() && projected_bounds.is_none() {
        PortalClipDebugDecision::EmptyProjection
    } else if result_bounds.is_none() {
        PortalClipDebugDecision::NoWindowOverlap
    } else if tiny {
        PortalClipDebugDecision::Tiny
    } else {
        PortalClipDebugDecision::Accepted
    };

    PortalClipDebug {
        parent: PortalClipDebugRect::from_frustum(parent),
        padded_parent,
        front_faces_camera,
        view_vertices,
        near_count,
        left_count,
        right_count,
        bottom_count,
        top_count,
        first_empty_plane,
        projected_bounds,
        clipped_bounds,
        fallback_bounds,
        result_bounds,
        tiny,
        decision,
    }
}

#[derive(Debug, Clone, Copy)]
struct PortalClip {
    left_tan_q12: i32,
    right_tan_q12: i32,
    min_y_tan_q12: i32,
    max_y_tan_q12: i32,
}

#[derive(Debug, Clone, Copy)]
struct PortalViewVertex {
    x: i32,
    y: i32,
    z: i32,
}

impl PortalViewVertex {
    const ZERO: Self = Self { x: 0, y: 0, z: 0 };
}

fn portal_front_faces_camera(
    portal: LevelRoomPortalRecord,
    camera: PortalVisibilityCamera,
) -> bool {
    let dx = camera.x.saturating_sub(portal.vertex_x[0]);
    let dy = camera.y.saturating_sub(portal.vertex_y[0]);
    let dz = camera.z.saturating_sub(portal.vertex_z[0]);
    let dot = dx
        .saturating_mul(portal.normal_x as i32)
        .saturating_add(dy.saturating_mul(portal.normal_y as i32))
        .saturating_add(dz.saturating_mul(portal.normal_z as i32));
    dot >= 0
}

fn clipped_portal_clip(
    portal: LevelRoomPortalRecord,
    camera: PortalVisibilityCamera,
    parent: PortalFrustum,
) -> Option<PortalClip> {
    projected_portal_surface_bounds_clip(portal, camera, parent)
}

fn clipped_portal_surface_polygon_clip(
    portal: LevelRoomPortalRecord,
    camera: PortalVisibilityCamera,
    parent: PortalFrustum,
) -> Option<PortalClip> {
    let mut vertices = [PortalViewVertex::ZERO; PORTAL_CLIP_VERTEX_CAPACITY];
    let mut scratch = [PortalViewVertex::ZERO; PORTAL_CLIP_VERTEX_CAPACITY];
    let mut i = 0usize;
    while i < 4 {
        vertices[i] = portal_view_vertex(portal, camera, i);
        i += 1;
    }
    let mut count = 4usize;
    count = clip_portal_polygon_against_plane(
        &mut vertices,
        &mut scratch,
        count,
        PortalClipPlane::Near(1),
    );
    let clip_left = parent.left_tan_q12.saturating_sub(PORTAL_SCREEN_PAD_Q12);
    let clip_right = parent.right_tan_q12.saturating_add(PORTAL_SCREEN_PAD_Q12);
    let clip_bottom = parent.min_y_tan_q12.saturating_sub(PORTAL_SCREEN_PAD_Q12);
    let clip_top = parent.max_y_tan_q12.saturating_add(PORTAL_SCREEN_PAD_Q12);
    count = clip_portal_polygon_against_plane(
        &mut vertices,
        &mut scratch,
        count,
        PortalClipPlane::Left(clip_left),
    );
    count = clip_portal_polygon_against_plane(
        &mut vertices,
        &mut scratch,
        count,
        PortalClipPlane::Right(clip_right),
    );
    count = clip_portal_polygon_against_plane(
        &mut vertices,
        &mut scratch,
        count,
        PortalClipPlane::Bottom(clip_bottom),
    );
    count = clip_portal_polygon_against_plane(
        &mut vertices,
        &mut scratch,
        count,
        PortalClipPlane::Top(clip_top),
    );
    if count == 0 {
        return None;
    }

    let mut bounds = PortalClipBounds::EMPTY;
    i = 0;
    while i < count {
        bounds.include_vertex(vertices[i]);
        i += 1;
    }

    if !bounds.valid() {
        return None;
    }
    let left = bounds
        .x
        .min_q12
        .saturating_sub(PORTAL_SCREEN_PAD_Q12)
        .max(parent.left_tan_q12);
    let right = bounds
        .x
        .max_q12
        .saturating_add(PORTAL_SCREEN_PAD_Q12)
        .min(parent.right_tan_q12);
    let min_y = bounds
        .y
        .min_q12
        .saturating_sub(PORTAL_SCREEN_PAD_Q12)
        .max(parent.min_y_tan_q12);
    let max_y = bounds
        .y
        .max_q12
        .saturating_add(PORTAL_SCREEN_PAD_Q12)
        .min(parent.max_y_tan_q12);
    if left <= right && min_y <= max_y {
        return Some(PortalClip {
            left_tan_q12: left,
            right_tan_q12: right,
            min_y_tan_q12: min_y,
            max_y_tan_q12: max_y,
        });
    }
    None
}

fn projected_portal_surface_bounds_clip(
    portal: LevelRoomPortalRecord,
    camera: PortalVisibilityCamera,
    parent: PortalFrustum,
) -> Option<PortalClip> {
    let bounds = projected_portal_surface_bounds(portal, camera)?;
    let left = bounds.left_tan_q12.max(parent.left_tan_q12);
    let right = bounds.right_tan_q12.min(parent.right_tan_q12);
    let min_y = bounds.min_y_tan_q12.max(parent.min_y_tan_q12);
    let max_y = bounds.max_y_tan_q12.min(parent.max_y_tan_q12);
    (left <= right && min_y <= max_y).then_some(PortalClip {
        left_tan_q12: left,
        right_tan_q12: right,
        min_y_tan_q12: min_y,
        max_y_tan_q12: max_y,
    })
}

fn projected_portal_surface_bounds(
    portal: LevelRoomPortalRecord,
    camera: PortalVisibilityCamera,
) -> Option<PortalClip> {
    let near = 1;
    let mut vertices = [PortalViewVertex::ZERO; 4];
    let mut bounds = PortalClipBounds::EMPTY;
    let mut i = 0usize;
    while i < 4 {
        vertices[i] = portal_view_vertex(portal, camera, i);
        if vertices[i].z >= near {
            bounds.include_vertex(vertices[i]);
        }
        i += 1;
    }

    i = 0;
    while i < 4 {
        let a = vertices[i];
        let b = vertices[(i + 1) & 3];
        let distance_a = a.z.saturating_sub(near);
        let distance_b = b.z.saturating_sub(near);
        if (distance_a < 0 && distance_b > 0) || (distance_a > 0 && distance_b < 0) {
            bounds.include_vertex(portal_clip_intersection(a, b, distance_a, distance_b));
        }
        i += 1;
    }

    if !bounds.valid() {
        return None;
    }
    Some(PortalClip {
        left_tan_q12: bounds.x.min_q12.saturating_sub(PORTAL_SCREEN_PAD_Q12),
        right_tan_q12: bounds.x.max_q12.saturating_add(PORTAL_SCREEN_PAD_Q12),
        min_y_tan_q12: bounds.y.min_q12.saturating_sub(PORTAL_SCREEN_PAD_Q12),
        max_y_tan_q12: bounds.y.max_q12.saturating_add(PORTAL_SCREEN_PAD_Q12),
    })
}

#[derive(Debug, Clone, Copy)]
enum PortalClipPlane {
    Near(i32),
    Left(i32),
    Right(i32),
    Bottom(i32),
    Top(i32),
}

fn clip_portal_polygon_against_plane(
    vertices: &mut [PortalViewVertex; PORTAL_CLIP_VERTEX_CAPACITY],
    scratch: &mut [PortalViewVertex; PORTAL_CLIP_VERTEX_CAPACITY],
    count: usize,
    plane: PortalClipPlane,
) -> usize {
    if count == 0 {
        return 0;
    }

    let mut out_count = 0usize;
    let mut previous = vertices[count - 1];
    let mut previous_distance = portal_clip_plane_distance(previous, plane);
    let mut previous_inside = previous_distance >= 0;
    let mut i = 0usize;
    while i < count {
        let current = vertices[i];
        let current_distance = portal_clip_plane_distance(current, plane);
        let current_inside = current_distance >= 0;
        if current_inside != previous_inside {
            push_clipped_portal_vertex(
                scratch,
                &mut out_count,
                portal_clip_intersection(previous, current, previous_distance, current_distance),
            );
        }
        if current_inside {
            push_clipped_portal_vertex(scratch, &mut out_count, current);
        }
        previous = current;
        previous_distance = current_distance;
        previous_inside = current_inside;
        i += 1;
    }

    i = 0;
    while i < out_count {
        vertices[i] = scratch[i];
        i += 1;
    }
    out_count
}

fn push_clipped_portal_vertex(
    vertices: &mut [PortalViewVertex; PORTAL_CLIP_VERTEX_CAPACITY],
    count: &mut usize,
    vertex: PortalViewVertex,
) {
    if *count < PORTAL_CLIP_VERTEX_CAPACITY {
        vertices[*count] = vertex;
        *count += 1;
    }
}

fn portal_clip_intersection(
    a: PortalViewVertex,
    b: PortalViewVertex,
    distance_a: i32,
    distance_b: i32,
) -> PortalViewVertex {
    let denom = distance_a.saturating_sub(distance_b);
    if denom == 0 {
        return a;
    }
    PortalViewVertex {
        x: interpolate_portal_clip_axis(a.x, b.x, distance_a, denom),
        y: interpolate_portal_clip_axis(a.y, b.y, distance_a, denom),
        z: interpolate_portal_clip_axis(a.z, b.z, distance_a, denom),
    }
}

fn portal_clip_plane_distance(vertex: PortalViewVertex, plane: PortalClipPlane) -> i32 {
    match plane {
        PortalClipPlane::Near(z) => vertex.z.saturating_sub(z),
        PortalClipPlane::Left(left_tan_q12) => {
            left_plane_distance(vertex.x, vertex.z, left_tan_q12)
        }
        PortalClipPlane::Right(right_tan_q12) => {
            right_plane_distance(vertex.x, vertex.z, right_tan_q12)
        }
        PortalClipPlane::Bottom(min_y_tan_q12) => {
            left_plane_distance(vertex.y, vertex.z, min_y_tan_q12)
        }
        PortalClipPlane::Top(max_y_tan_q12) => {
            right_plane_distance(vertex.y, vertex.z, max_y_tan_q12)
        }
    }
}

fn left_plane_distance(lateral: i32, depth: i32, left_tan_q12: i32) -> i32 {
    lateral
        .saturating_mul(Q12_ONE)
        .saturating_sub(left_tan_q12.saturating_mul(depth))
}

fn right_plane_distance(lateral: i32, depth: i32, right_tan_q12: i32) -> i32 {
    right_tan_q12
        .saturating_mul(depth)
        .saturating_sub(lateral.saturating_mul(Q12_ONE))
}

fn interpolate_portal_clip_axis(a: i32, b: i32, numerator: i32, denominator: i32) -> i32 {
    a.saturating_add(mul_div_i32(b.saturating_sub(a), numerator, denominator))
}

fn portal_view_vertex(
    portal: LevelRoomPortalRecord,
    camera: PortalVisibilityCamera,
    index: usize,
) -> PortalViewVertex {
    let dx = portal.vertex_x[index].saturating_sub(camera.x);
    let dy = portal.vertex_y[index].saturating_sub(camera.y);
    let dz = portal.vertex_z[index].saturating_sub(camera.z);
    world_view_delta(dx, dy, dz, camera)
}

fn world_view_delta(dx: i32, dy: i32, dz: i32, camera: PortalVisibilityCamera) -> PortalViewVertex {
    let sin_yaw = camera.sin_yaw_q12;
    let cos_yaw = camera.cos_yaw_q12;
    let sin_pitch = camera.sin_pitch_q12;
    let cos_pitch = camera.cos_pitch_q12;
    let x1 = mul_q12_i32(dx, cos_yaw).saturating_sub(mul_q12_i32(dz, sin_yaw));
    let z1 = mul_q12_i32(dx, -sin_yaw).saturating_sub(mul_q12_i32(dz, cos_yaw));
    let y2 = mul_q12_i32(dy, cos_pitch).saturating_sub(mul_q12_i32(z1, sin_pitch));
    let z2 = mul_q12_i32(dy, sin_pitch).saturating_add(mul_q12_i32(z1, cos_pitch));

    PortalViewVertex {
        x: x1,
        y: y2,
        z: z2,
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

    fn include_slope(&mut self, lateral: i32, depth: i32) {
        if depth <= 0 {
            return;
        }
        let slope = clamp_slope_q12(div_q12_i32(lateral, depth));
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

fn portal_mask_bit(portal_index: usize) -> RuntimeDebugMask {
    RuntimeDebugMask::from_index(portal_index)
}

fn clamp_slope_q12(value: i32) -> i32 {
    value.clamp(-SLOPE_LIMIT_Q12, SLOPE_LIMIT_Q12)
}

fn mul_q12_i32(value: i32, q12: i32) -> i32 {
    let whole = (value >> Q12_SHIFT).saturating_mul(q12);
    let frac = ((value & (Q12_ONE - 1)).saturating_mul(q12)) >> Q12_SHIFT;
    whole.saturating_add(frac)
}

fn div_q12_i32(numerator: i32, denominator: i32) -> i32 {
    if denominator == 0 {
        return if numerator < 0 { i32::MIN } else { i32::MAX };
    }
    let whole = (numerator / denominator).saturating_mul(Q12_ONE);
    let remainder = numerator % denominator;
    whole.saturating_add(remainder.saturating_mul(Q12_ONE) / denominator)
}

fn mul_div_i32(value: i32, numerator: i32, denominator: i32) -> i32 {
    if denominator == 0 {
        return 0;
    }
    let whole = (value / denominator).saturating_mul(numerator);
    let remainder = value % denominator;
    whole.saturating_add(remainder.saturating_mul(numerator) / denominator)
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

    fn yawed_camera(z: i32, sin_yaw_q12: i32, cos_yaw_q12: i32) -> PortalVisibilityCamera {
        PortalVisibilityCamera::new(
            0,
            1024,
            z,
            sin_yaw_q12,
            cos_yaw_q12,
            0,
            4096,
            64,
            16_384,
            4096,
            3072,
            4,
        )
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
        assert_eq!(out.stats.tested_portal_mask, 0b11);
        assert_eq!(out.stats.accepted_portal_mask, 0b01);
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
    fn frontier_ignores_clip_rejected_adjacent_rooms() {
        let rooms = [room(0, 0, 2), room(1, 2, 0), room(2, 2, 0)];
        let portals = [
            wall_portal(0, 1, -1024, 1024, 4096),
            wall_portal(0, 2, 6000, 7000, 4096),
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
        assert_eq!(out.frontier_count, 0);
        assert_eq!(out.stats.reject_frustum_room_mask, 0b100);
        assert_eq!(out.stats.reject_frustum_portal_mask, 0b10);
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
    fn accepts_portal_touching_view_edge_with_tomb_pixel_padding() {
        let rooms = [room(0, 0, 1), room(1, 1, 0)];
        let portals = [wall_portal(0, 1, 4104, 4112, 4096)];
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
        assert_eq!(out.stats.reject_frustum, 0);
    }

    #[test]
    fn accepts_off_axis_portal_when_any_surface_strip_is_in_view() {
        let rooms = [room(0, 0, 1), room(1, 1, 0)];
        let portals = [wall_portal(0, 1, 3500, 7000, 4096)];
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
        assert_eq!(out.stats.reject_frustum, 0);
    }

    #[test]
    fn accepts_partially_visible_portal_with_center_outside_view() {
        let rooms = [room(0, 0, 1), room(1, 1, 0)];
        let portals = [wall_portal(0, 1, 3900, 7000, 4096)];
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
        assert_eq!(out.stats.reject_frustum, 0);
    }

    #[test]
    fn accepts_pitched_third_person_portal_surface_strip() {
        let rooms = [room(0, 0, 1), room(1, 1, 0)];
        let portals = [LevelRoomPortalRecord {
            source_room: RoomIndex(0),
            destination_room: RoomIndex(1),
            kind: 0,
            normal_x: -1,
            normal_y: 0,
            normal_z: 0,
            vertex_x: [6144, 6144, 6144, 6144],
            vertex_y: [0, 0, 4096, 4096],
            vertex_z: [-4096, -6144, -6144, -4096],
        }];
        let camera = PortalVisibilityCamera::new(
            4608, 3108, -6290, 0, -4096, -2103, 3514, 64, 16_384, 2048, 1536, 4,
        );
        let mut out = PortalVisibilityResult::<8, 16, 8>::EMPTY;

        build_portal_visibility(&rooms, &portals, RoomIndex(0), camera, 4, &mut out);

        assert_eq!(out.visible_room_mask(), 0b11);
        assert_eq!(out.stats.portals_accepted, 1);
        assert_eq!(out.stats.reject_frustum, 0);
    }

    #[test]
    fn accepts_demo7_second_hop_portal_from_captured_camera() {
        let rooms = [
            room(0, 0, 0),
            room(1, 0, 0),
            room(2, 0, 0),
            room(3, 4, 2),
            room(4, 0, 0),
            room(5, 9, 2),
            room(6, 0, 0),
        ];
        let dummy = wall_portal(0, 0, 0, 0, 0);
        let portals = [
            dummy,
            dummy,
            dummy,
            dummy,
            LevelRoomPortalRecord {
                source_room: RoomIndex(3),
                destination_room: RoomIndex(5),
                kind: 0,
                normal_x: -1,
                normal_y: 0,
                normal_z: 0,
                vertex_x: [-2048, -2048, -2048, -2048],
                vertex_y: [0, 0, 4160, 4160],
                vertex_z: [-4096, -10240, -10240, -4096],
            },
            LevelRoomPortalRecord {
                source_room: RoomIndex(3),
                destination_room: RoomIndex(2),
                kind: 0,
                normal_x: 0,
                normal_y: 0,
                normal_z: 1,
                vertex_x: [-10240, -6144, -6144, -10240],
                vertex_y: [0, 0, 2176, 2176],
                vertex_z: [-12288, -12288, -12288, -12288],
            },
            dummy,
            dummy,
            dummy,
            LevelRoomPortalRecord {
                source_room: RoomIndex(5),
                destination_room: RoomIndex(6),
                kind: 0,
                normal_x: 0,
                normal_y: 0,
                normal_z: -1,
                vertex_x: [0, 2048, 2048, 0],
                vertex_y: [0, 0, 2176, 2176],
                vertex_z: [-4096, -4096, -4096, -4096],
            },
            LevelRoomPortalRecord {
                source_room: RoomIndex(5),
                destination_room: RoomIndex(3),
                kind: 0,
                normal_x: 1,
                normal_y: 0,
                normal_z: 0,
                vertex_x: [-2048, -2048, -2048, -2048],
                vertex_y: [0, 0, 4160, 4160],
                vertex_z: [-4096, -10240, -10240, -4096],
            },
        ];
        let camera = PortalVisibilityCamera::new(
            -5615, 1638, -8915, -3573, -2003, -680, 4039, 64, 16_384, 2048, 1536, 4,
        );
        let mut out = PortalVisibilityResult::<8, 16, 8>::EMPTY;

        build_portal_visibility(&rooms, &portals, RoomIndex(3), camera, 8, &mut out);

        assert!(out.contains_room(RoomIndex(5)));
        assert!(out.contains_room(RoomIndex(6)));
        assert_ne!(out.stats.accepted_portal_mask & (1 << 9), 0);
        assert_eq!(out.stats.reject_frustum_portal_mask & (1 << 9), 0);
    }

    #[test]
    fn rejects_portal_just_outside_horizontal_view() {
        let rooms = [room(0, 0, 1), room(1, 1, 0)];
        let portals = [wall_portal(0, 1, 4200, 7000, 4096)];
        let mut out = PortalVisibilityResult::<8, 16, 8>::EMPTY;

        build_portal_visibility(
            &rooms,
            &portals,
            RoomIndex(0),
            forward_camera(0),
            4,
            &mut out,
        );

        assert_eq!(out.visible_room_mask(), 0b1);
        assert_eq!(out.stats.portals_accepted, 0);
        assert_eq!(out.stats.reject_frustum, 1);
    }

    #[test]
    fn rejects_portal_when_camera_turns_away() {
        let rooms = [room(0, 0, 1), room(1, 1, 0)];
        let portals = [wall_portal(0, 1, -1024, 1024, 4096)];
        let camera = yawed_camera(0, 0, 4096);
        let mut out = PortalVisibilityResult::<8, 16, 8>::EMPTY;

        build_portal_visibility(&rooms, &portals, RoomIndex(0), camera, 4, &mut out);

        assert_eq!(out.visible_room_mask(), 0b1);
        assert_eq!(out.stats.portals_accepted, 0);
        assert_eq!(out.stats.reject_frustum, 1);
    }

    #[test]
    fn rejects_portal_when_whole_room_camera_turns_sideways() {
        let rooms = [room(0, 0, 1), room(1, 1, 0)];
        let portals = [wall_portal(0, 1, -1024, 1024, 4096)];
        let camera = yawed_camera(0, 4096, 0);
        let mut out = PortalVisibilityResult::<8, 16, 8>::EMPTY;

        build_portal_visibility(&rooms, &portals, RoomIndex(0), camera, 4, &mut out);

        assert_eq!(out.visible_room_mask(), 0b1);
        assert_eq!(out.stats.portals_accepted, 0);
        assert_eq!(out.stats.reject_frustum, 1);
    }

    #[test]
    fn rejects_portal_surface_outside_vertical_view_even_if_seam_is_visible() {
        let rooms = [room(0, 0, 1), room(1, 1, 0)];
        let portals = [wall_portal_with_y(0, 1, 3900, 7000, 4096, 8192, 9216)];
        let camera = forward_camera(0);
        let mut out = PortalVisibilityResult::<8, 16, 8>::EMPTY;

        build_portal_visibility(&rooms, &portals, RoomIndex(0), camera, 4, &mut out);

        assert_eq!(out.visible_room_mask(), 0b1);
        assert_eq!(out.stats.portals_accepted, 0);
        assert_eq!(out.stats.reject_frustum, 1);
    }

    #[test]
    fn rejects_portal_surface_outside_horizontal_view_even_if_room_bounds_are_visible() {
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
        let camera = forward_camera(0);
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

        assert_eq!(out.visible_room_mask(), 0b1);
        assert_eq!(out.stats.portals_accepted, 0);
        assert_eq!(out.stats.reject_frustum, 1);
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
        assert_eq!(out.frontier_room_mask(), 0);
        assert_eq!(out.stats.portals_accepted, 1);
        assert_eq!(out.stats.reject_frustum, 1);
        assert_eq!(out.stats.reject_frustum_room_mask, 0b100);
        assert_eq!(out.stats.reject_frustum_portal_mask, 0b10);
    }

    #[test]
    fn recursive_portal_visibility_keeps_parent_horizontal_clip() {
        let rooms = [room(0, 0, 1), room(1, 1, 1), room(2, 2, 0)];
        let portals = [
            wall_portal(0, 1, -3800, -3000, 4096),
            wall_portal(1, 2, 3000, 3800, 8192),
        ];
        let camera = forward_camera(0);
        let mut out = PortalVisibilityResult::<8, 16, 8>::EMPTY;

        build_portal_visibility(&rooms, &portals, RoomIndex(0), camera, 4, &mut out);

        assert_eq!(out.visible_room_mask(), 0b11);
        assert_eq!(out.stats.portals_accepted, 1);
        assert_eq!(out.stats.reject_frustum, 1);
        assert_eq!(out.stats.reject_frustum_room_mask, 0b100);
        assert_eq!(out.stats.reject_frustum_portal_mask, 0b10);
    }

    #[test]
    fn room_bounds_do_not_rescue_offscreen_portal() {
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
        let camera = forward_camera(0);
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

        assert_eq!(out.visible_room_mask(), 0b1);
        assert_eq!(out.stats.portals_accepted, 0);
        assert_eq!(out.stats.reject_frustum, 1);
        assert_eq!(out.stats.bounds_fallbacks, 0);
    }

    #[test]
    fn room_bounds_do_not_rescue_offscreen_portal_on_first_hop() {
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
        let camera = forward_camera(0);
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

        assert_eq!(out.visible_room_mask(), 0b1);
        assert_eq!(out.stats.portals_accepted, 0);
        assert_eq!(out.stats.reject_frustum, 1);
        assert_eq!(out.stats.bounds_fallbacks, 0);
        assert_eq!(out.stats.bounds_fallback_room_mask, 0);
        assert_eq!(out.stats.bounds_fallback_portal_mask, 0);
    }

    #[test]
    fn room_bounds_do_not_rescue_offscreen_portal_after_first_hop() {
        let rooms = [room(0, 0, 1), room(1, 1, 1), room(2, 2, 0)];
        let portals = [north_portal(0, 1, -1), wall_portal(1, 2, 6000, 7000, 4096)];
        let bounds = [PortalRoomBounds {
            room: RoomIndex(2),
            min_x: -512,
            max_x: 512,
            min_y: 0,
            max_y: 2048,
            min_z: 4096,
            max_z: 6144,
        }];
        let camera = forward_camera(0);
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
        assert_eq!(out.stats.reject_frustum, 1);
        assert_eq!(out.stats.bounds_fallbacks, 0);
        assert_eq!(out.stats.bounds_fallback_room_mask, 0);
        assert_eq!(out.stats.bounds_fallback_portal_mask, 0);
    }
}
