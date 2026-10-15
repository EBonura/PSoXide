#[allow(dead_code)]
#[path = "fixtures/affine_surface_before.rs"]
mod before;
use psx_engine::affine_surface as after;
use psx_gte::{
    math::{Mat3I16, Vec3I32},
    scene,
};
fn configure() {
    scene::set_screen_offset(160 << 16, 120 << 16);
    scene::set_projection_plane(160);
    scene::set_average_z_weights(0x155, 0x100);
    scene::load_rotation(&Mat3I16::IDENTITY);
    scene::load_translation(Vec3I32::ZERO);
}
#[test]
fn packets_match_frozen_renderer_across_split_cull_and_depth_boundaries() {
    let depths = [
        -32768, -1, 0, 1, 15, 16, 339, 340, 679, 680, 681, 1359, 1360, 1361, 2047, 8191, 8192,
        32767,
    ];
    let mut seed = 0x92abcdefu32;
    for case in 0..4096usize {
        let count = 3 + case % 6;
        let mut old = vec![before::AffineVertex::default(); count + 6];
        let mut new = vec![after::AffineVertex::default(); count + 6];
        for i in 0..count {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let angle = (i as f64) * std::f64::consts::TAU / count as f64;
            let position = [
                ((angle.cos() * ((case % 1024) + 1) as f64) as i16)
                    .wrapping_add((case % 9) as i16 * 256 - 1024),
                ((angle.sin() * ((case % 512) + 1) as f64) as i16)
                    .wrapping_add((case % 7) as i16 * 256 - 768),
                depths[(case + i / 2) % depths.len()],
            ];
            let uv = [seed as u8, (seed >> 8) as u8];
            old[i] = before::AffineVertex {
                position,
                uv,
                color: seed,
                screen: [123, -456],
                depth: 0xdeadbeef,
            };
            new[i] = after::AffineVertex {
                position,
                uv,
                color: seed,
                screen: [123, -456],
                depth: 0xdeadbeef,
            };
        }
        let surface = before::AffineSurface {
            first_vertex: 0,
            vertex_count: count as u16,
            tpage: 0x105,
            clut: 0x1234,
            uv_offset: [case as u8, (case >> 3) as u8],
            compact: (case % 2) as u8,
            texture_window_word: 0xe2000000 | ((case as u32) & 0xfffff),
            color_command_word: 0x34000000 | ((case as u32 & 1) << 25),
        };
        let surface_new = after::AffineSurface {
            first_vertex: surface.first_vertex,
            vertex_count: surface.vertex_count,
            tpage: surface.tpage,
            clut: surface.clut,
            uv_offset: surface.uv_offset,
            compact: surface.compact,
            texture_window_word: surface.texture_window_word,
            color_command_word: surface.color_command_word,
        };
        let mut a = vec![0xaabbccddu32; 8192];
        let mut b = a.clone();
        configure();
        let ar = unsafe {
            before::submit_surface_batch(
                old.as_mut_ptr(),
                count,
                &surface,
                1,
                a.as_mut_ptr(),
                before::SurfaceProfile::PXBSP_THIRD_PERSON,
            )
        };
        configure();
        let br = unsafe {
            after::submit_surface_batch(
                new.as_mut_ptr(),
                count,
                &surface_new,
                1,
                b.as_mut_ptr(),
                after::SurfaceProfile::PXBSP_THIRD_PERSON,
            )
        };
        assert_eq!(ar.packets, br.packets, "case {case}");
        assert_eq!(ar.hardware_triangles, br.hardware_triangles, "case {case}");
        assert_eq!(
            unsafe { ar.next_packet.offset_from(a.as_ptr()) },
            unsafe { br.next_packet.offset_from(b.as_ptr()) },
            "case {case}"
        );
        assert_eq!(a, b, "packet order/words/guard case {case}");
        for i in 0..count + 6 {
            assert_eq!(
                (
                    old[i].position,
                    old[i].uv,
                    old[i].color,
                    old[i].screen,
                    old[i].depth
                ),
                (
                    new[i].position,
                    new[i].uv,
                    new[i].color,
                    new[i].screen,
                    new[i].depth
                ),
                "scratch case {case} vertex {i}"
            );
        }
    }
}
