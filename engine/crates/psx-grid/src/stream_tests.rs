//! Load and evict rooms while the world is drawn, walked and asked about
//! entities: nothing may break, and what is gone must behave as a solid
//! box, not a hole.

use crate::activity::RoomActivity;
use crate::format::*;
use crate::render::{FrustumPlanes, GridMaterial, GridRenderer, RoomCamera, ViewProjection};
use crate::resident::ResidentRooms;
use crate::view::{LevelView, RoomView};
use crate::write::{aligned_copy, words_as_bytes, LevelCore, RoomChunk};
use alloc::vec;
use alloc::vec::Vec;
use psx_gte::math::{Mat3I16, Vec3I32 as GteVec3I32};
use psx_gte::scene;

const S: i32 = 80;
const ROOMS: usize = 6;

/// One sector with a wall facing -X on its east edge (room-local `x = 80`),
/// drawn with one quad, plus a floor so a body can stand in it.
fn chunk() -> Vec<u8> {
    let vtx = |z: i16, y: i16| Vtx {
        x: 80,
        y,
        z,
        pad: 0,
    };
    RoomChunk {
        width: 1,
        depth: 1,
        min_y: 0,
        max_y: 80,
        sectors: vec![Sector {
            floor: [0; 4],
            ceil: [80; 4],
            flags: SECTOR_HAS_FLOOR | SECTOR_FLOOR_WALK_A | SECTOR_FLOOR_WALK_B | SECTOR_FLAT_FLOOR,
            ext: NO_EXT,
        }],
        vertices: vec![vtx(0, 0), vtx(80, 0), vtx(80, 80), vtx(0, 80)],
        quads: vec![Quad {
            v: [0, 1, 2, 3],
            color: [0x00_80_80_80; 4],
            uv: [0, 63, 63 | (63 << 8), 63 << 8],
            d: -80,
            n: [-127, 0, 0],
            material: 0,
            flags: QUAD_AXIS | QUAD_AXIS_NEG,
            ..Quad::default()
        }],
        cells: vec![RCell {
            first: 0,
            count: 1,
            flags: 0,
            min_y: 0,
            max_y: 80,
        }],
        material_use: vec![0],
        ..RoomChunk::default()
    }
    .to_bytes()
}

/// A corridor of `ROOMS` one sector rooms along X, every neighbour pair
/// joined by a portal both ways.
fn core(draw_far: u8) -> Vec<u8> {
    let mut rooms = Vec::new();
    let mut portals = Vec::new();
    for i in 0..ROOMS {
        let first = portals.len() as u16;
        let wall = |at: i16, dest: usize| Portal {
            dest: dest as u16,
            flags: PORTAL_ALWAYS_OPEN,
            v: [[at, 0, 0], [at, 0, 80], [at, 80, 80], [at, 80, 0]],
            normal: [0; 3],
            reserved: 0,
        };
        if i > 0 {
            portals.push(wall(0, i - 1));
        }
        if i + 1 < ROOMS {
            portals.push(wall(80, i + 1));
        }
        rooms.push(RoomRecord {
            origin: [i as i32 * S, 0, 0],
            width: 1,
            depth: 1,
            portal_first: first,
            portal_count: portals.len() as u16 - first,
            min_y: 0,
            max_y: 80,
            ..RoomRecord::default()
        });
    }
    LevelCore {
        occupancy: Vec::new(),
        sector_units: S as u16,
        draw_far,
        active_radius: 2,
        lead_radius: 1,
        rooms,
        portals,
        materials: vec![Material::default()],
        maxima: RenderMaxima {
            vertices: 4,
            cells: 1,
            quads: 1,
            ..RenderMaxima::default()
        },
        ..LevelCore::default()
    }
    .to_bytes()
}

fn material() -> GridMaterial {
    GridMaterial {
        resident: true,
        tpage: 1,
        ..GridMaterial::default()
    }
}

/// Draw every candidate room from `eye` (world, looking along +X) and return
/// the packets and the rooms that were drawn.
fn draw(
    level: &LevelView<'_>,
    resident: &ResidentRooms<'_>,
    activity: &mut RoomActivity,
    renderer: &mut GridRenderer,
    eye: [i32; 3],
) -> (u32, Vec<usize>) {
    let mut candidates = Vec::new();
    let room = resident.collision().locate(0, eye).map(|l| l.room);
    activity.draw_rooms(level, resident, room, eye, &mut candidates);
    let projection = ViewProjection {
        focal_length: 160,
        ..ViewProjection::DEFAULT
    };
    let mut packets = 0;
    let mut drawn = Vec::new();
    for &index in &candidates {
        let Some(slot) = resident.get(index) else {
            continue;
        };
        let Some(view) = slot.view() else {
            continue;
        };
        if !slot.materials_ready {
            continue;
        }
        let origin = level.rooms[index].origin;
        let local = [eye[0] - origin[0], eye[1] - origin[1], eye[2] - origin[2]];
        psx_gte::host::reset();
        scene::set_screen_offset(160 << 16, 120 << 16);
        scene::set_projection_plane(160);
        scene::set_average_z_weights(0x155, 0x100);
        let rotated = crate::render::load_view_rotation(
            GteVec3I32::new(local[0] << 12, local[1] << 12, local[2] << 12),
            Mat3I16::IDENTITY,
        );
        let frustum = FrustumPlanes::from_view(&rotated.rotation, rotated.translation, projection);
        let camera = RoomCamera {
            frustum: &frustum,
            view: &rotated,
            camera: local,
            screen: [320, 240],
            focal_length: 160,
            sector: S,
        };
        let mut out = vec![0u32; 4096];
        let frame = renderer.draw_room(&view, &camera, &slot.materials, &mut out, false);
        packets += frame.packets;
        if frame.packets > 0 {
            drawn.push(index);
        }
    }
    (packets, drawn)
}

#[test]
fn an_evicted_rooms_material_buffer_serves_the_next_room_inserted() {
    let core_words = aligned_copy(&core(0));
    let level = LevelView::parse(words_as_bytes(&core_words, core_words.len() * 4)).unwrap();
    let chunks: Vec<Vec<u32>> = (0..ROOMS).map(|_| aligned_copy(&chunk())).collect();
    let view = |i: usize| RoomView::parse(words_as_bytes(&chunks[i], chunks[i].len() * 4)).unwrap();
    let mut resident = ResidentRooms::with_capacity(level, 2).unwrap();
    resident.reserve_rooms(2);
    resident.insert(0, view(0)).unwrap();
    resident.get_mut(0).unwrap().materials = vec![material(); 6];
    let buffer = resident.get(0).unwrap().materials.as_ptr();
    let removed = resident.remove(0).unwrap();
    assert!(
        removed.materials.is_empty(),
        "the buffer stays with the set"
    );
    // The next room, whichever it is, takes the buffer back, emptied.
    resident.insert(1, view(1)).unwrap();
    let slot = resident.get_mut(1).unwrap();
    assert!(slot.materials.is_empty() && slot.materials.capacity() >= 6);
    slot.materials.push(material());
    assert_eq!(slot.materials.as_ptr(), buffer);
}

#[test]
fn rooms_load_and_evict_without_breaking_draw_collision_or_entities() {
    let core_words = aligned_copy(&core(0));
    let level = LevelView::parse(words_as_bytes(&core_words, core_words.len() * 4)).unwrap();
    level.validate_full().unwrap();
    let chunks: Vec<Vec<u32>> = (0..ROOMS).map(|_| aligned_copy(&chunk())).collect();
    let view = |i: usize| RoomView::parse(words_as_bytes(&chunks[i], chunks[i].len() * 4)).unwrap();
    let mut resident = ResidentRooms::new(level).unwrap();
    let bind = |resident: &mut ResidentRooms<'_>, room: usize| {
        let slot = resident.get_mut(room).unwrap();
        slot.materials = vec![material()];
        slot.materials_ready = true;
    };
    for room in 0..ROOMS {
        resident.insert(room, view(room)).unwrap();
        bind(&mut resident, room);
    }
    let mut activity = RoomActivity::new(&level);
    let mut renderer = GridRenderer::with_maxima(&level.maxima());
    let eye = [10, 40, 40];
    activity.track_player(&level, 0, eye);

    // Everything resident: every room's wall is in front of the eye.
    let (all, drawn) = draw(&level, &resident, &mut activity, &mut renderer, eye);
    assert_eq!(drawn.len(), ROOMS, "{drawn:?}");
    assert!(all as usize >= ROOMS);
    let point_in = |room: usize| [room as i32 * S + 40, 20, 40];
    let active = |resident: &ResidentRooms<'_>, activity: &RoomActivity, room: usize| {
        let at = point_in(room);
        resident
            .collision()
            .locate(0, at)
            .is_some_and(|l| activity.room_active(resident, l.room))
    };
    // Activation radius 2 sectors: rooms 0, 1 and 2 hold active entities.
    assert!(active(&resident, &activity, 1) && active(&resident, &activity, 2));
    assert!(!active(&resident, &activity, 4));
    assert!(!resident.collision().body_blocked(point_in(2), 12, 48));

    // Evict room 2. It is not drawn, not active, and solid, not a hole.
    let epoch = resident.epoch();
    resident.remove(2).unwrap();
    assert_ne!(resident.epoch(), epoch);
    let (fewer, drawn) = draw(&level, &resident, &mut activity, &mut renderer, eye);
    assert_eq!(drawn.len(), ROOMS - 1);
    assert!(!drawn.contains(&2));
    assert!(fewer < all);
    assert!(!active(&resident, &activity, 2));
    assert!(resident.collision().body_blocked(point_in(2), 12, 48));
    // The walk still passes through the missing room: room 3 keeps its
    // distance, so it is still drawn and active-by-distance rules stand.
    assert!(drawn.contains(&3));

    // Evict the whole far end, then bring everything back at new addresses.
    for room in 3..ROOMS {
        resident.remove(room).unwrap();
    }
    let (near, drawn) = draw(&level, &resident, &mut activity, &mut renderer, eye);
    assert_eq!(drawn, vec![0, 1]);
    assert!(near < fewer);
    let moved: Vec<Vec<u32>> = (0..ROOMS).map(|_| aligned_copy(&chunk())).collect();
    for (room, words) in moved.iter().enumerate().skip(2) {
        let view = RoomView::parse(words_as_bytes(words, words.len() * 4)).unwrap();
        // `moved` outlives `resident`: it is dropped after it below.
        resident.insert(room, view).unwrap();
        bind(&mut resident, room);
    }
    let (again, drawn) = draw(&level, &resident, &mut activity, &mut renderer, eye);
    assert_eq!(drawn.len(), ROOMS);
    assert_eq!(again, all, "the world draws exactly as before the eviction");
    assert!(active(&resident, &activity, 2));
    assert!(!resident.collision().body_blocked(point_in(2), 12, 48));

    // A room that has landed but has no bound materials is skipped, not drawn.
    resident.get_mut(1).unwrap().materials_ready = false;
    let (_, drawn) = draw(&level, &resident, &mut activity, &mut renderer, eye);
    assert!(!drawn.contains(&1) && drawn.contains(&0));
    drop(resident);
    drop(moved);
}

#[test]
fn the_draw_distance_bounds_the_rooms_that_are_drawn() {
    // Draw distance of 3 sectors: walk distance from an eye in room 0 to
    // room 3 is about 2.5 sectors, to room 5 about 4.5.
    let core_words = aligned_copy(&core(3));
    let level = LevelView::parse(words_as_bytes(&core_words, core_words.len() * 4)).unwrap();
    let chunks: Vec<Vec<u32>> = (0..ROOMS).map(|_| aligned_copy(&chunk())).collect();
    let mut resident = ResidentRooms::new(level).unwrap();
    for (room, words) in chunks.iter().enumerate() {
        let v = RoomView::parse(words_as_bytes(words, words.len() * 4)).unwrap();
        resident.insert(room, v).unwrap();
        let slot = resident.get_mut(room).unwrap();
        slot.materials = vec![material()];
        slot.materials_ready = true;
    }
    let mut activity = RoomActivity::new(&level);
    let mut renderer = GridRenderer::with_maxima(&level.maxima());
    let near =
        |activity: &mut RoomActivity, resident: &ResidentRooms<'_>, room: usize, eye: [i32; 3]| {
            let mut rooms = Vec::new();
            activity.draw_rooms(&level, resident, Some(room), eye, &mut rooms);
            rooms.sort_unstable();
            rooms
        };
    // Room 3 is about 2.5 sectors of walking away, room 5 about 4.5.
    let rooms = near(&mut activity, &resident, 0, [10, 40, 40]);
    assert!(rooms.contains(&3) && !rooms.contains(&5), "{rooms:?}");
    let (_, drawn) = draw(
        &level,
        &resident,
        &mut activity,
        &mut renderer,
        [10, 40, 40],
    );
    assert!(drawn.contains(&3) && !drawn.contains(&5), "{drawn:?}");
    // With the eye in room 3 the far room 5 is near and room 0 is still in
    // reach behind it (behind the eye its wall faces away, so it is not drawn).
    let rooms = near(&mut activity, &resident, 3, [3 * S + 10, 40, 40]);
    assert_eq!(rooms, vec![0, 1, 2, 3, 4, 5]);
    assert_eq!(activity.draw_radius(), 3 * S);
    // From room 0 the far end is out of range even though every room is
    // resident: the bound, not residency, keeps it out of the draw set.
    let rooms = near(&mut activity, &resident, 0, [10, 40, 40]);
    assert!(!rooms.contains(&4) || !rooms.contains(&5), "{rooms:?}");
}
