use super::*;

pub(super) fn room_index_debug_mask(index: RoomIndex) -> RuntimeDebugMask {
    RuntimeDebugMask::from_room(index)
}

pub(super) use psx_game_runtime::room_streaming::emit_room_chunk_mask;

const DEBUG_LOG_LINE_CAP: usize = 256;
/// Master gate for the verbose portal-visibility snapshot log. Default off: the
/// snapshot emits many lines one byte at a time via `write_volatile` to the
/// trapped emulator log port, and every trapped byte costs the emulator
/// thousands of cycles, so a single snapshot smears ~1M guest cycles onto its
/// tick and reads as a frametime spike. Its `should_debug_log_*` predicate is
/// almost always true (some portal is always rejected), so it fired on a fixed
/// cooldown in normal runs. Keep false for play/perf; flip to true only when
/// debugging portal traversal.
pub(super) const PORTAL_VIS_DEBUG_LOGS: bool = false;
/// Log active-window reconcile anomalies (stale frees, failed or
/// skipped builds) with streaming state, for headless diagnosis.
pub(super) const RECONCILE_DEBUG_LOGS: bool = true;
pub(super) const PORTAL_VIS_DEBUG_LOG_COOLDOWN_TICKS: u8 = 120;
const PORTAL_VIS_DEBUG_VERBOSE_CLIPS: bool = false;
const PORTAL_VIS_DEBUG_LOG_MAX_FRUSTUMS: usize = 4;
const PORTAL_VIS_DEBUG_LOG_MAX_PORTALS: usize = 16;
pub(super) const POST_CROSS_RENDER_DEBUG_LOGS: bool = false;

struct DebugLogLine {
    bytes: [u8; DEBUG_LOG_LINE_CAP],
    len: usize,
}

impl DebugLogLine {
    fn new(prefix: &str) -> Self {
        let mut line = Self {
            bytes: [0; DEBUG_LOG_LINE_CAP],
            len: 0,
        };
        line.push_str(prefix);
        line
    }

    fn push_str(&mut self, text: &str) {
        for &byte in text.as_bytes() {
            self.push_byte(byte);
        }
    }

    fn push_byte(&mut self, byte: u8) {
        if self.len < self.bytes.len() {
            self.bytes[self.len] = byte;
            self.len += 1;
        }
    }

    fn push_u32(&mut self, value: u32) {
        let mut scratch = [0u8; 10];
        let mut remaining = value;
        let mut len = 0usize;
        loop {
            scratch[len] = b'0' + (remaining % 10) as u8;
            len += 1;
            remaining /= 10;
            if remaining == 0 {
                break;
            }
        }
        while len > 0 {
            len -= 1;
            self.push_byte(scratch[len]);
        }
    }

    fn push_i32(&mut self, value: i32) {
        if value < 0 {
            self.push_byte(b'-');
            self.push_u32(value.wrapping_neg() as u32);
        } else {
            self.push_u32(value as u32);
        }
    }

    fn push_room(&mut self, room: RoomIndex) {
        self.push_u32(room.raw() as u32);
    }

    fn push_bool(&mut self, value: bool) {
        self.push_byte(if value { b'1' } else { b'0' });
    }

    fn push_point(&mut self, point: RoomPoint) {
        self.push_byte(b'(');
        self.push_i32(point.x);
        self.push_byte(b',');
        self.push_i32(point.y);
        self.push_byte(b',');
        self.push_i32(point.z);
        self.push_byte(b')');
    }

    fn push_hex_u32_digits(&mut self, value: u32, pad_to_eight: bool) {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        if value == 0 && !pad_to_eight {
            self.push_byte(b'0');
            return;
        }
        let mut started = false;
        let mut shift = 28i32;
        while shift >= 0 {
            let nibble = ((value >> shift) & 0xF) as usize;
            if nibble != 0 || started || pad_to_eight || shift == 0 {
                started = true;
                self.push_byte(DIGITS[nibble]);
            }
            shift -= 4;
        }
    }

    fn push_hex_mask(&mut self, mask: RuntimeDebugMask) {
        self.push_str("0x");
        if mask.hi() != 0 {
            self.push_hex_u32_digits(mask.hi(), false);
            self.push_hex_u32_digits(mask.lo(), true);
        } else {
            self.push_hex_u32_digits(mask.lo(), false);
        }
    }

    fn emit(&self) {
        telemetry::debug_line(&self.bytes[..self.len]);
    }
}

pub(super) fn debug_log_room_transition(
    previous_room: RoomIndex,
    next_room: RoomIndex,
    previous_local: RoomPoint,
    next_local: RoomPoint,
    global: RoomPoint,
    camera_before: RoomPoint,
    camera_after: RoomPoint,
) {
    if !POST_CROSS_RENDER_DEBUG_LOGS {
        return;
    }
    let mut line = DebugLogLine::new("room cross prev=");
    line.push_room(previous_room);
    line.push_str(" next=");
    line.push_room(next_room);
    line.push_str(" player_local=");
    line.push_point(previous_local);
    line.push_str(" -> ");
    line.push_point(next_local);
    line.push_str(" global=");
    line.push_point(global);
    line.push_str(" camera=");
    line.push_point(camera_before);
    line.push_str(" -> ");
    line.push_point(camera_after);
    line.emit();
}

pub(super) fn debug_log_room_window_after_cross(
    room: RoomIndex,
    visible_count: usize,
    frontier_count: usize,
    visible_mask: RuntimeDebugMask,
    active_mask: RuntimeDebugMask,
    drawable_mask: RuntimeDebugMask,
    loading_mask: RuntimeDebugMask,
    missing_mask: RuntimeDebugMask,
    build_failed_mask: RuntimeDebugMask,
    current_render_ready: bool,
    current_collision_ready: bool,
    portals_tested: u16,
    portals_accepted: u16,
) {
    if !POST_CROSS_RENDER_DEBUG_LOGS {
        return;
    }
    let mut line = DebugLogLine::new("room window room=");
    line.push_room(room);
    line.push_str(" visible=");
    line.push_u32(visible_count.min(u32::MAX as usize) as u32);
    line.push_str(" frontier=");
    line.push_u32(frontier_count.min(u32::MAX as usize) as u32);
    line.push_str(" tested=");
    line.push_u32(portals_tested as u32);
    line.push_str(" accepted=");
    line.push_u32(portals_accepted as u32);
    line.push_str(" vis=");
    line.push_hex_mask(visible_mask);
    line.push_str(" active=");
    line.push_hex_mask(active_mask);
    line.push_str(" draw=");
    line.push_hex_mask(drawable_mask);
    line.push_str(" loading=");
    line.push_hex_mask(loading_mask);
    line.push_str(" missing=");
    line.push_hex_mask(missing_mask);
    line.push_str(" build_fail=");
    line.push_hex_mask(build_failed_mask);
    line.push_str(" render=");
    line.push_bool(current_render_ready);
    line.push_str(" coll=");
    line.push_bool(current_collision_ready);
    line.emit();
}

fn portal_debug_mask_bit(index: usize) -> RuntimeDebugMask {
    RuntimeDebugMask::from_index(index)
}

fn portal_debug_decision_name(decision: PortalClipDebugDecision) -> &'static str {
    match decision {
        PortalClipDebugDecision::Accepted => "accepted",
        PortalClipDebugDecision::Backface => "backface",
        PortalClipDebugDecision::EmptyProjection => "empty",
        PortalClipDebugDecision::NoWindowOverlap => "no_window",
        PortalClipDebugDecision::Tiny => "tiny",
    }
}

fn portal_debug_plane_name(plane: PortalClipDebugPlane) -> &'static str {
    match plane {
        PortalClipDebugPlane::None => "none",
        PortalClipDebugPlane::Near => "near",
        PortalClipDebugPlane::Left => "left",
        PortalClipDebugPlane::Right => "right",
        PortalClipDebugPlane::Bottom => "bottom",
        PortalClipDebugPlane::Top => "top",
    }
}

fn push_portal_debug_rect(line: &mut DebugLogLine, rect: PortalClipDebugRect) {
    line.push_byte(b'[');
    line.push_i32(rect.left_tan_q12);
    line.push_byte(b',');
    line.push_i32(rect.right_tan_q12);
    line.push_byte(b',');
    line.push_i32(rect.min_y_tan_q12);
    line.push_byte(b',');
    line.push_i32(rect.max_y_tan_q12);
    line.push_byte(b']');
}

fn push_optional_portal_debug_rect(line: &mut DebugLogLine, rect: Option<PortalClipDebugRect>) {
    if let Some(rect) = rect {
        push_portal_debug_rect(line, rect);
    } else {
        line.push_byte(b'-');
    }
}

fn portal_debug_center(portal: psx_level::LevelRoomPortalRecord) -> RoomPoint {
    RoomPoint::new(
        (portal.vertex_x[0]
            .saturating_add(portal.vertex_x[1])
            .saturating_add(portal.vertex_x[2])
            .saturating_add(portal.vertex_x[3]))
            / 4,
        (portal.vertex_y[0]
            .saturating_add(portal.vertex_y[1])
            .saturating_add(portal.vertex_y[2])
            .saturating_add(portal.vertex_y[3]))
            / 4,
        (portal.vertex_z[0]
            .saturating_add(portal.vertex_z[1])
            .saturating_add(portal.vertex_z[2])
            .saturating_add(portal.vertex_z[3]))
            / 4,
    )
}

fn portal_debug_view_center(clip: PortalClipDebug) -> RoomPoint {
    let mut x = 0i32;
    let mut y = 0i32;
    let mut z = 0i32;
    let mut i = 0usize;
    while i < 4 {
        let vertex = clip.view_vertices[i];
        x = x.saturating_add(vertex.x);
        y = y.saturating_add(vertex.y);
        z = z.saturating_add(vertex.z);
        i += 1;
    }
    RoomPoint::new(x / 4, y / 4, z / 4)
}

fn debug_log_portal_visibility_summary(
    current_room: RoomIndex,
    player_room: RoomIndex,
    player_local: RoomPoint,
    player_global: RoomPoint,
    view: ActiveRoomView,
    camera: PortalVisibilityCamera,
    result: &RuntimePortalVisibility,
) {
    let mut line = DebugLogLine::new("portal vis pose room=");
    line.push_room(current_room);
    line.push_str(" player_room=");
    line.push_room(player_room);
    line.push_str(" player_local=");
    line.push_point(player_local);
    line.push_str(" player_global=");
    line.push_point(player_global);
    line.emit();

    let stats = result.stats;
    let mut line = DebugLogLine::new("portal vis camera local=");
    line.push_point(view.position);
    line.push_str(" global=");
    line.push_point(RoomPoint::new(camera.x, camera.y, camera.z));
    line.push_str(" sy/cy/sp/cp=(");
    line.push_i32(camera.sin_yaw_q12);
    line.push_byte(b',');
    line.push_i32(camera.cos_yaw_q12);
    line.push_byte(b',');
    line.push_i32(camera.sin_pitch_q12);
    line.push_byte(b',');
    line.push_i32(camera.cos_pitch_q12);
    line.push_str(") near/far=");
    line.push_i32(camera.near_z);
    line.push_byte(b'/');
    line.push_i32(camera.far_z);
    line.push_str(" fov=");
    line.push_i32(camera.half_fov_x_tan_q12);
    line.push_byte(b'/');
    line.push_i32(camera.half_fov_y_tan_q12);
    line.emit();

    let mut line = DebugLogLine::new("portal vis stats rooms/fr=");
    line.push_u32(result.room_count.min(u32::MAX as usize) as u32);
    line.push_byte(b'/');
    line.push_u32(result.frustum_count.min(u32::MAX as usize) as u32);
    line.push_str(" test/acc=");
    line.push_u32(stats.portals_tested as u32);
    line.push_byte(b'/');
    line.push_u32(stats.portals_accepted as u32);
    line.push_str(" rej b/f/t=");
    line.push_u32(stats.reject_backface as u32);
    line.push_byte(b'/');
    line.push_u32(stats.reject_frustum as u32);
    line.push_byte(b'/');
    line.push_u32(stats.reject_tiny as u32);
    line.push_str(" cap r/f/d=");
    line.push_u32(stats.cap_room as u32);
    line.push_byte(b'/');
    line.push_u32(stats.cap_frustum as u32);
    line.push_byte(b'/');
    line.push_u32(stats.cap_depth as u32);
    line.emit();

    let mut line = DebugLogLine::new("portal vis masks visible=");
    line.push_hex_mask(result.visible_room_mask());
    line.push_str(" tested=");
    line.push_hex_mask(stats.tested_room_mask);
    line.push_str(" accepted=");
    line.push_hex_mask(stats.accepted_room_mask);
    line.push_str(" rej_rooms=");
    line.push_hex_mask(stats.reject_frustum_room_mask);
    line.push_str(" rej_portals=");
    line.push_hex_mask(stats.reject_frustum_portal_mask);
    line.emit();
}

fn debug_log_portal_clip_summary_line(
    portal_index: usize,
    portal: psx_level::LevelRoomPortalRecord,
    parent: PortalFrustum,
    clip: PortalClipDebug,
    stats: psx_level::portal_visibility::PortalVisibilityStats,
) {
    let portal_bit = portal_debug_mask_bit(portal_index);
    let tested = !portal_bit.is_empty() && stats.tested_portal_mask.contains_index(portal_index);
    let accepted =
        !portal_bit.is_empty() && stats.accepted_portal_mask.contains_index(portal_index);
    let rejected = !portal_bit.is_empty()
        && stats
            .reject_frustum_portal_mask
            .contains_index(portal_index);

    let mut line = DebugLogLine::new("portal p summary idx=");
    line.push_u32(portal_index.min(u32::MAX as usize) as u32);
    line.push_str(" src=");
    line.push_room(portal.source_room);
    line.push_str(" dst=");
    line.push_room(portal.destination_room);
    line.push_str(" depth=");
    line.push_u32(parent.depth as u32);
    line.push_str(" decision=");
    line.push_str(portal_debug_decision_name(clip.decision));
    line.push_str(" empty=");
    line.push_str(portal_debug_plane_name(clip.first_empty_plane));
    line.push_str(" t/a/r=");
    line.push_bool(tested);
    line.push_byte(b'/');
    line.push_bool(accepted);
    line.push_byte(b'/');
    line.push_bool(rejected);
    line.push_str(" world=");
    line.push_point(portal_debug_center(portal));
    line.emit();

    let mut line = DebugLogLine::new("portal p view idx=");
    line.push_u32(portal_index.min(u32::MAX as usize) as u32);
    line.push_str(" center=");
    line.push_point(portal_debug_view_center(clip));
    line.push_str(" parent=");
    push_portal_debug_rect(&mut line, clip.parent);
    line.push_str(" proj=");
    push_optional_portal_debug_rect(&mut line, clip.projected_bounds);
    line.push_str(" result=");
    push_optional_portal_debug_rect(&mut line, clip.result_bounds);
    line.emit();
}

fn debug_log_portal_visible_rooms(result: &RuntimePortalVisibility) {
    let mut line = DebugLogLine::new("portal vis rooms=");
    let limit = result.room_count.min(MAX_ACTIVE_ROOMS);
    let mut i = 0usize;
    while i < limit {
        if i > 0 {
            line.push_byte(b',');
        }
        let room = result.rooms[i];
        line.push_room(room.room);
        line.push_byte(b':');
        line.push_u32(room.depth as u32);
        line.push_byte(b'/');
        line.push_u32(room.frustum_count as u32);
        i += 1;
    }
    line.emit();
}

fn debug_log_portal_visibility_source_portal_summaries(
    camera: PortalVisibilityCamera,
    result: &RuntimePortalVisibility,
) {
    let mut logged = 0usize;
    let frustum_limit = result
        .frustum_count
        .min(PORTAL_VIS_DEBUG_LOG_MAX_FRUSTUMS)
        .min(MAX_PORTAL_FRUSTUMS);
    let mut frustum_slot = 0usize;
    while frustum_slot < frustum_limit && logged < PORTAL_VIS_DEBUG_LOG_MAX_PORTALS {
        let frustum = result.frustums[frustum_slot];
        let Some(record) = ROOMS.get(frustum.room.to_usize()) else {
            frustum_slot += 1;
            continue;
        };
        let portal_first = record.portal_first as usize;
        let portal_end = portal_first.saturating_add(record.portal_count as usize);
        let mut portal_index = portal_first;
        while portal_index < portal_end.min(ROOM_PORTALS.len())
            && logged < PORTAL_VIS_DEBUG_LOG_MAX_PORTALS
        {
            let portal = ROOM_PORTALS[portal_index];
            if portal.source_room == frustum.room {
                let clip = debug_portal_clip(portal, camera, frustum);
                debug_log_portal_clip_summary_line(
                    portal_index,
                    portal,
                    frustum,
                    clip,
                    result.stats,
                );
                logged += 1;
            }
            portal_index += 1;
        }
        frustum_slot += 1;
    }
}

fn debug_log_portal_clip_line(
    root_room: RoomIndex,
    portal_index: usize,
    parent: PortalFrustum,
    portal: psx_level::LevelRoomPortalRecord,
    clip: PortalClipDebug,
    stats: psx_level::portal_visibility::PortalVisibilityStats,
) {
    let portal_bit = portal_debug_mask_bit(portal_index);
    let tested = !portal_bit.is_empty() && stats.tested_portal_mask.contains_index(portal_index);
    let accepted =
        !portal_bit.is_empty() && stats.accepted_portal_mask.contains_index(portal_index);
    let rejected = !portal_bit.is_empty()
        && stats
            .reject_frustum_portal_mask
            .contains_index(portal_index);
    let skip_backlink =
        portal.destination_room == root_room || portal.destination_room == parent.source_room;

    let mut line = DebugLogLine::new("portal p idx=");
    line.push_u32(portal_index.min(u32::MAX as usize) as u32);
    line.push_str(" src=");
    line.push_room(portal.source_room);
    line.push_str(" dst=");
    line.push_room(portal.destination_room);
    line.push_str(" depth=");
    line.push_u32(parent.depth as u32);
    line.push_str(" decision=");
    line.push_str(portal_debug_decision_name(clip.decision));
    line.push_str(" flags t/a/r/skip=");
    line.push_bool(tested);
    line.push_byte(b'/');
    line.push_bool(accepted);
    line.push_byte(b'/');
    line.push_bool(rejected);
    line.push_byte(b'/');
    line.push_bool(skip_backlink);
    line.push_str(" front=");
    line.push_bool(clip.front_faces_camera);
    line.emit();

    let mut line = DebugLogLine::new("portal p counts idx=");
    line.push_u32(portal_index.min(u32::MAX as usize) as u32);
    line.push_str(" n/l/r/b/t=");
    line.push_u32(clip.near_count as u32);
    line.push_byte(b'/');
    line.push_u32(clip.left_count as u32);
    line.push_byte(b'/');
    line.push_u32(clip.right_count as u32);
    line.push_byte(b'/');
    line.push_u32(clip.bottom_count as u32);
    line.push_byte(b'/');
    line.push_u32(clip.top_count as u32);
    line.push_str(" empty=");
    line.push_str(portal_debug_plane_name(clip.first_empty_plane));
    line.push_str(" tiny=");
    line.push_bool(clip.tiny);
    line.push_str(" normal=(");
    line.push_i32(portal.normal_x as i32);
    line.push_byte(b',');
    line.push_i32(portal.normal_y as i32);
    line.push_byte(b',');
    line.push_i32(portal.normal_z as i32);
    line.push_byte(b')');
    line.emit();

    let mut line = DebugLogLine::new("portal p geom idx=");
    line.push_u32(portal_index.min(u32::MAX as usize) as u32);
    let mut i = 0usize;
    while i < 4 {
        line.push_str(" v");
        line.push_u32(i as u32);
        line.push_byte(b'=');
        line.push_point(RoomPoint::new(
            portal.vertex_x[i],
            portal.vertex_y[i],
            portal.vertex_z[i],
        ));
        i += 1;
    }
    line.emit();

    let mut line = DebugLogLine::new("portal p view idx=");
    line.push_u32(portal_index.min(u32::MAX as usize) as u32);
    let mut i = 0usize;
    while i < 4 {
        line.push_str(" v");
        line.push_u32(i as u32);
        line.push_byte(b'=');
        let vertex = clip.view_vertices[i];
        line.push_point(RoomPoint::new(vertex.x, vertex.y, vertex.z));
        i += 1;
    }
    line.emit();

    let mut line = DebugLogLine::new("portal p clip idx=");
    line.push_u32(portal_index.min(u32::MAX as usize) as u32);
    line.push_str(" parent=");
    push_portal_debug_rect(&mut line, clip.parent);
    line.push_str(" proj=");
    push_optional_portal_debug_rect(&mut line, clip.projected_bounds);
    line.push_str(" clipped=");
    push_optional_portal_debug_rect(&mut line, clip.clipped_bounds);
    line.push_str(" result=");
    push_optional_portal_debug_rect(&mut line, clip.result_bounds);
    line.emit();
}

fn debug_log_portal_visibility_source_portals(
    root_room: RoomIndex,
    camera: PortalVisibilityCamera,
    result: &RuntimePortalVisibility,
) {
    let mut logged = 0usize;
    let frustum_limit = result
        .frustum_count
        .min(PORTAL_VIS_DEBUG_LOG_MAX_FRUSTUMS)
        .min(MAX_PORTAL_FRUSTUMS);
    let mut frustum_slot = 0usize;
    while frustum_slot < frustum_limit && logged < PORTAL_VIS_DEBUG_LOG_MAX_PORTALS {
        let frustum = result.frustums[frustum_slot];
        let Some(record) = ROOMS.get(frustum.room.to_usize()) else {
            frustum_slot += 1;
            continue;
        };
        let portal_first = record.portal_first as usize;
        let portal_end = portal_first.saturating_add(record.portal_count as usize);
        let mut portal_index = portal_first;
        while portal_index < portal_end.min(ROOM_PORTALS.len())
            && logged < PORTAL_VIS_DEBUG_LOG_MAX_PORTALS
        {
            let portal = ROOM_PORTALS[portal_index];
            if portal.source_room == frustum.room {
                let clip = debug_portal_clip(portal, camera, frustum);
                debug_log_portal_clip_line(
                    root_room,
                    portal_index,
                    frustum,
                    portal,
                    clip,
                    result.stats,
                );
                logged += 1;
            }
            portal_index += 1;
        }
        frustum_slot += 1;
    }
}

pub(super) fn should_debug_log_portal_visibility(
    current_record: &LevelRoomRecord,
    result: &RuntimePortalVisibility,
) -> bool {
    let stats = result.stats;
    stats.reject_backface != 0
        || stats.reject_frustum != 0
        || stats.reject_tiny != 0
        || stats.cap_room != 0
        || stats.cap_frustum != 0
        || stats.cap_depth != 0
        || (current_record.portal_count != 0 && current_record.portal_count <= 4)
}

pub(super) fn debug_log_portal_visibility_snapshot(
    current_room: RoomIndex,
    current_record: &LevelRoomRecord,
    player_room: RoomIndex,
    player_local: RoomPoint,
    player_global: RoomPoint,
    view: ActiveRoomView,
    camera: PortalVisibilityCamera,
    result: &RuntimePortalVisibility,
) {
    if !should_debug_log_portal_visibility(current_record, result) {
        return;
    }
    debug_log_portal_visibility_summary(
        current_room,
        player_room,
        player_local,
        player_global,
        view,
        camera,
        result,
    );
    debug_log_portal_visible_rooms(result);
    debug_log_portal_visibility_source_portal_summaries(camera, result);
    if PORTAL_VIS_DEBUG_VERBOSE_CLIPS {
        debug_log_portal_visibility_source_portals(current_room, camera, result);
    }
}

fn active_room_cache_status_debug_code(status: ActiveRoomCacheStatus) -> u32 {
    match status {
        ActiveRoomCacheStatus::Ready => 0,
        ActiveRoomCacheStatus::NotBuilt => 1,
        ActiveRoomCacheStatus::Overflow => 2,
        ActiveRoomCacheStatus::Empty => 3,
    }
}

pub(super) fn debug_log_post_cross_render_start(
    room: RoomIndex,
    camera: WorldCamera,
    visible_mask: RuntimeDebugMask,
    active_mask: RuntimeDebugMask,
    current_collision_ready: bool,
) {
    let mut line = DebugLogLine::new("render start room=");
    line.push_room(room);
    line.push_str(" cam=");
    line.push_point(RoomPoint::new(
        camera.position.x,
        camera.position.y,
        camera.position.z,
    ));
    line.push_str(" vis=");
    line.push_hex_mask(visible_mask);
    line.push_str(" active=");
    line.push_hex_mask(active_mask);
    line.push_str(" coll=");
    line.push_bool(current_collision_ready);
    line.emit();
}

pub(super) fn debug_log_post_cross_render_room(
    slot: usize,
    active: ActiveRuntimeRoom,
    draws: bool,
) {
    let cache = active.surface_cache;
    let mut line = DebugLogLine::new("render room slot=");
    line.push_u32(slot.min(u32::MAX as usize) as u32);
    line.push_str(" room=");
    line.push_room(active.index);
    line.push_str(" stream=");
    line.push_u32(active.stream_slot as u32);
    line.push_str(" off=(");
    line.push_i32(active.offset_x);
    line.push_byte(b',');
    line.push_i32(active.offset_z);
    line.push_byte(b')');
    line.push_str(" draw=");
    line.push_bool(draws);
    line.push_str(" cache=");
    line.push_bool(cache.ready);
    line.push_str(" st=");
    line.push_u32(active_room_cache_status_debug_code(cache.status));
    line.push_str(" cells=");
    line.push_u32(cache.cell_count.min(u32::MAX as usize) as u32);
    line.push_str(" verts=");
    line.push_u32(cache.vertex_count.min(u32::MAX as usize) as u32);
    line.push_str(" surf=");
    line.push_u32(cache.surface_count.min(u32::MAX as usize) as u32);
    line.push_str(" amb=(");
    line.push_u32(active.ambient_rgb[0] as u32);
    line.push_byte(b',');
    line.push_u32(active.ambient_rgb[1] as u32);
    line.push_byte(b',');
    line.push_u32(active.ambient_rgb[2] as u32);
    line.push_byte(b')');
    line.push_str(" rr=");
    line.push_bool(active.render_room.is_some());
    line.push_str(" slices=");
    line.push_bool(room_surface_cache_slices(active.index, cache).is_some());
    line.emit();
}

pub(super) fn debug_log_post_cross_render_end(
    room: RoomIndex,
    active_mask: RuntimeDebugMask,
    drawn_mask: RuntimeDebugMask,
    primitive_count: usize,
    primitive_remaining: usize,
    world_commands: usize,
) {
    let mut line = DebugLogLine::new("render end room=");
    line.push_room(room);
    line.push_str(" active=");
    line.push_hex_mask(active_mask);
    line.push_str(" drawn=");
    line.push_hex_mask(drawn_mask);
    line.push_str(" prim=");
    line.push_u32(primitive_count.min(u32::MAX as usize) as u32);
    line.push_str(" rem=");
    line.push_u32(primitive_remaining.min(u32::MAX as usize) as u32);
    line.push_str(" cmd=");
    line.push_u32(world_commands.min(u32::MAX as usize) as u32);
    line.emit();
}

#[cfg(feature = "cd-stream-bench")]
pub(super) fn debug_log_stream_plan<const N: usize>(label: &str, plan: &RoomStreamLoadPlan<N>) {
    let mut line = DebugLogLine::new(label);
    line.push_str(" count=");
    line.push_u32(plan.count.min(u32::MAX as usize) as u32);
    line.push_str(" rooms=");
    let limit = plan.count.min(N).min(STREAMED_ROOM_SLOT_COUNT);
    let mut i = 0usize;
    while i < limit {
        if i > 0 {
            line.push_byte(b',');
        }
        line.push_room(plan.rooms[i]);
        line.push_byte(b'@');
        line.push_u32(plan.slots[i].min(u32::MAX as usize) as u32);
        i += 1;
    }
    line.emit();
}

#[cfg(feature = "cd-stream-bench")]
pub(super) fn debug_log_stream_entry(
    label: &str,
    room: RoomIndex,
    slot: usize,
    byte_count: usize,
    status: u32,
) {
    let mut line = DebugLogLine::new(label);
    line.push_str(" room=");
    line.push_room(room);
    line.push_str(" slot=");
    line.push_u32(slot.min(u32::MAX as usize) as u32);
    line.push_str(" bytes=");
    line.push_u32(byte_count.min(u32::MAX as usize) as u32);
    line.push_str(" status=");
    line.push_u32(status);
    line.emit();
}

fn encode_debug_map_position(value: i32) -> u32 {
    let encoded = value.saturating_add(DEBUG_MAP_POSITION_BIAS);
    if encoded < 0 {
        0
    } else {
        encoded as u32
    }
}

fn encode_debug_q12_basis(value: i32) -> u32 {
    value.saturating_add(4096).clamp(0, 8192) as u32
}

pub(super) fn emit_player_map_debug(
    room: RoomIndex,
    position: RoomPoint,
    facing_yaw_q12: u16,
    camera_position: RoomPoint,
    camera_global: RoomPoint,
    view_yaw_q12: u16,
    view_sin_yaw_q12: i32,
    view_cos_yaw_q12: i32,
    view_sin_pitch_q12: i32,
    view_cos_pitch_q12: i32,
) {
    telemetry::counter(
        telemetry::counter::ROOM_PLAYER_ROOM_INDEX,
        room.raw() as u32,
    );
    telemetry::counter(
        telemetry::counter::ROOM_PLAYER_LOCAL_X_BIASED,
        encode_debug_map_position(position.x),
    );
    telemetry::counter(
        telemetry::counter::ROOM_PLAYER_LOCAL_Z_BIASED,
        encode_debug_map_position(position.z),
    );
    telemetry::counter(
        telemetry::counter::ROOM_PLAYER_LOCAL_Y_BIASED,
        encode_debug_map_position(position.y),
    );
    telemetry::counter(
        telemetry::counter::ROOM_PLAYER_VIEW_YAW_Q12,
        view_yaw_q12 as u32,
    );
    telemetry::counter(
        telemetry::counter::PLAYER_FACING_YAW_Q12,
        facing_yaw_q12 as u32,
    );
    telemetry::counter(
        telemetry::counter::ROOM_CAMERA_LOCAL_X_BIASED,
        encode_debug_map_position(camera_position.x),
    );
    telemetry::counter(
        telemetry::counter::ROOM_CAMERA_LOCAL_Y_BIASED,
        encode_debug_map_position(camera_position.y),
    );
    telemetry::counter(
        telemetry::counter::ROOM_CAMERA_LOCAL_Z_BIASED,
        encode_debug_map_position(camera_position.z),
    );
    telemetry::counter(
        telemetry::counter::ROOM_CAMERA_GLOBAL_X_BIASED,
        encode_debug_map_position(camera_global.x),
    );
    telemetry::counter(
        telemetry::counter::ROOM_CAMERA_GLOBAL_Y_BIASED,
        encode_debug_map_position(camera_global.y),
    );
    telemetry::counter(
        telemetry::counter::ROOM_CAMERA_GLOBAL_Z_BIASED,
        encode_debug_map_position(camera_global.z),
    );
    telemetry::counter(
        telemetry::counter::ROOM_CAMERA_VIEW_SIN_YAW_Q12_BIASED,
        encode_debug_q12_basis(view_sin_yaw_q12),
    );
    telemetry::counter(
        telemetry::counter::ROOM_CAMERA_VIEW_COS_YAW_Q12_BIASED,
        encode_debug_q12_basis(view_cos_yaw_q12),
    );
    telemetry::counter(
        telemetry::counter::ROOM_CAMERA_VIEW_SIN_PITCH_Q12_BIASED,
        encode_debug_q12_basis(view_sin_pitch_q12),
    );
    telemetry::counter(
        telemetry::counter::ROOM_CAMERA_VIEW_COS_PITCH_Q12_BIASED,
        encode_debug_q12_basis(view_cos_pitch_q12),
    );
}

pub(super) fn debug_log_reconcile_room(
    label: &str,
    room: RoomIndex,
    stream_slot: u16,
    resident: bool,
    loading: bool,
) {
    let mut line = DebugLogLine::new("recon ");
    line.push_str(label);
    line.push_str(" room=");
    line.push_room(room);
    line.push_str(" slot=");
    line.push_u32(stream_slot as u32);
    line.push_str(" res=");
    line.push_bool(resident);
    line.push_str(" load=");
    line.push_bool(loading);
    line.emit();
}

pub(super) fn debug_log_reconcile_pass(
    current: RoomIndex,
    desired_count: usize,
    built: usize,
    freed: usize,
    converged: bool,
    window_mask: RuntimeDebugMask,
) {
    let mut line = DebugLogLine::new("recon pass cur=");
    line.push_room(current);
    line.push_str(" want=");
    line.push_u32(desired_count as u32);
    line.push_str(" built=");
    line.push_u32(built as u32);
    line.push_str(" freed=");
    line.push_u32(freed as u32);
    line.push_str(" conv=");
    line.push_bool(converged);
    line.push_str(" win=");
    line.push_hex_mask(window_mask);
    line.emit();
}
