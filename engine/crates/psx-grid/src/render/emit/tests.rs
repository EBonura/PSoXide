//! Packets of repeating and plain quads, on the host GTE: a `REPEAT` quad
//! carries its material's window with raw UVs on the straight path and as a
//! windowed packet on the near-plane path, a plain quad adds the texture
//! origin and resets the window.

use super::*;
use crate::render::projection::{load_view_rotation, ViewProjection};
use crate::view::RoomView;
use crate::write::{aligned_copy, words_as_bytes, RoomChunk};
use alloc::vec;
use alloc::vec::Vec;
use psx_gte::math::{Mat3I16, Vec3I32 as GteVec3I32};

const WINDOW: u32 = 0xE200_0000 | 0b11000 | (0b11000 << 5) | (16 << 10) | (8 << 15);
const ORIGIN: u16 = 0x4080;

fn material() -> GridMaterial {
    GridMaterial {
        tpage: 0x1234,
        clut: 0x5678,
        uv_origin: ORIGIN,
        command_bits: 0,
        resident: true,
        window_word: WINDOW,
    }
}

/// One cell with a wall on the plane `x = 80`, facing -X: a plain quad
/// first, then a repeating one with the given raw UV corner `far`.
fn chunk(far: u8) -> Vec<u8> {
    let wall = |first: usize| {
        let mut quad = Quad {
            v: [0, 1, 2, 3],
            color: [0x00_80_80_80; 4],
            d: -80,
            n: [-127, 0, 0],
            material: 0,
            flags: QUAD_AXIS | QUAD_AXIS_NEG | (0 << QUAD_AXIS_SHIFT),
            ..Quad::default()
        };
        if first == 1 {
            quad.flags2 = QUAD2_REPEAT;
            quad.uv = [
                0,
                u16::from(far),
                u16::from(far) | (u16::from(far) << 8),
                u16::from(far) << 8,
            ];
        } else {
            quad.uv = [0, 63, 63 | (63 << 8), 63 << 8];
        }
        quad
    };
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
            flags: SECTOR_HAS_FLOOR,
            ext: NO_EXT,
            ..Sector::default()
        }],
        vertices: vec![vtx(0, 0), vtx(80, 0), vtx(80, 80), vtx(0, 80)],
        quads: vec![wall(0), wall(1)],
        cells: vec![RCell {
            first: 0,
            count: 2,
            flags: 0,
            min_y: 0,
            max_y: 80,
        }],
        material_use: vec![0],
        ..RoomChunk::default()
    }
    .to_bytes()
}

/// Draw the chunk from `eye_x` (room-local, looking along +X) and return
/// the packet words.
fn draw(far: u8, eye_x: i32) -> (GridFrame, Vec<u32>) {
    let words = aligned_copy(&chunk(far));
    let bytes = words_as_bytes(&words, words.len() * 4);
    let room = RoomView::parse(bytes).expect("room parses");
    room.validate_full(1).expect("room validates");
    psx_gte::host::reset();
    scene::set_screen_offset(160 << 16, 120 << 16);
    scene::set_projection_plane(160);
    scene::set_average_z_weights(0x155, 0x100);
    let eye = [eye_x, 40, 40];
    let view = load_view_rotation(
        GteVec3I32::new(eye[0] << 12, eye[1] << 12, eye[2] << 12),
        Mat3I16::IDENTITY,
    );
    let projection = ViewProjection {
        focal_length: 160,
        ..ViewProjection::DEFAULT
    };
    let frustum = FrustumPlanes::from_view(&view.rotation, view.translation, projection);
    let camera = RoomCamera {
        frustum: &frustum,
        view: &view,
        camera: eye,
        screen: [320, 240],
        focal_length: 160,
        sector: 80,
    };
    let mut renderer = GridRenderer::for_rooms(core::iter::once(&room));
    let mut out = vec![0u32; 4096];
    let frame = renderer.draw_room(&room, &camera, &[material()], &mut out, false);
    out.truncate(frame.packet_words);
    (frame, out)
}

/// Packets of a stream as `(tag, data words)`; `tag >> 24` is the data count.
fn packets(stream: &[u32]) -> Vec<&[u32]> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < stream.len() {
        let size = (stream[at] >> 24) as usize;
        out.push(&stream[at..at + 1 + size]);
        at += 1 + size;
    }
    out
}

#[test]
fn a_repeat_quad_carries_the_window_and_raw_uvs_and_a_plain_quad_resets_it() {
    let (frame, stream) = draw(255, -120);
    assert_eq!(frame.packets, 2, "{frame:?}");
    let packets = packets(&stream);
    assert_eq!(packets.len(), 2);
    let reset = TextureWindow::NONE.word();
    // Both quads sit on one plane and one depth; take them by their window.
    let plain = packets
        .iter()
        .find(|p| p[1] == reset)
        .expect("a plain packet");
    let repeat = packets
        .iter()
        .find(|p| p[1] == WINDOW)
        .expect("a repeating packet");
    // Words 4, 7, 10, 13 hold the UVs (corner 1, 0, 2, 3 order) low half.
    let uv = |p: &[u32], at: usize| (p[at] & 0xffff) as u16;
    // Plain: origin added to 63 (u) and 63 | 63 << 8.
    assert_eq!(uv(plain, 4), 63u16.wrapping_add(ORIGIN));
    assert_eq!(uv(plain, 7), 0u16.wrapping_add(ORIGIN));
    // Repeating: raw, nothing added.
    assert_eq!(uv(repeat, 4), 255);
    assert_eq!(uv(repeat, 7), 0);
    assert_eq!(uv(repeat, 10), 255 | (255 << 8));
    assert_eq!(uv(repeat, 13), 255 << 8);
    // Same clut and tpage words either way.
    assert_eq!(repeat[4] >> 16, 0x5678);
    assert_eq!(repeat[7] >> 16, 0x1234);
    assert_eq!(repeat.len(), plain.len(), "no packet grows");
}

#[test]
fn a_repeat_quad_on_a_material_without_a_window_is_skipped_not_drawn_wrong() {
    let words = aligned_copy(&chunk(255));
    let bytes = words_as_bytes(&words, words.len() * 4);
    let room = RoomView::parse(bytes).unwrap();
    assert!(GridMaterial::default()
        .addressing(&room.render.quads[1], 0)
        .is_none());
    let plain = GridMaterial::default().addressing(&room.render.quads[0], 7);
    assert_eq!(plain, Some((0, 7)));
}

#[test]
fn near_plane_repeat_quads_take_the_windowed_packet_shape() {
    // The wall is 10 units ahead of the eye, inside the zone the straight
    // path cannot project exactly, so the quads take the shared path.
    let (frame, stream) = draw(255, 70);
    assert!(frame.robust_quads >= 1, "{frame:?}");
    let packets = packets(&stream);
    let reset = TextureWindow::NONE.word();
    let windowed = |p: &[u32]| p.contains(&WINDOW);
    let repeating: Vec<_> = packets.iter().filter(|p| windowed(p)).collect();
    assert!(!repeating.is_empty(), "no packet carries the window");
    for p in &repeating {
        // The windowed shape: window word in front, reset behind.
        assert_eq!(p[1], WINDOW);
        assert_eq!(*p.last().unwrap(), reset);
    }
    // No plain packet carries the window.
    assert!(packets.iter().any(|p| !windowed(p)));
}
