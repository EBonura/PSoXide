use super::indexed_cache::cached_surface_subdivision_options;
use super::*;
use crate::Angle;
use crate::PrimitiveArena;
use crate::{DepthBand, DepthRange, ProjectedVertex, WorldProjection, Q12};

/// Helper: the two indices both triangles in `[t0, t1]`
/// share form the diagonal of the split. Returned sorted
/// so test assertions are stable.
fn diagonal(triangles: [(usize, usize, usize); 2]) -> [usize; 2] {
    let [t0, t1] = triangles;
    let a = [t0.0, t0.1, t0.2];
    let b = [t1.0, t1.1, t1.2];
    let mut shared = [usize::MAX; 2];
    let mut n = 0;
    for &i in &a {
        if b.contains(&i) && n < 2 {
            shared[n] = i;
            n += 1;
        }
    }
    shared.sort();
    shared
}

#[test]
fn split_zero_uses_nw_se_diagonal() {
    // Standard split -- both triangles meet at corners 0
    // and 2, which is the diagonal `submit_textured_quad`
    // has always used.
    let triangles = split_triangles(SPLIT_NW_SE);
    assert_eq!(triangles[0], (0, 1, 2));
    assert_eq!(triangles[1], (0, 2, 3));
    assert_eq!(diagonal(triangles), [0, 2]);
}

#[test]
fn split_one_uses_ne_sw_diagonal() {
    // Alternate split -- the two triangles share corners
    // 1 (NE) and 3 (SW), which is the perpendicular
    // diagonal. This is the case the prior renderer got
    // wrong: it used the NW→SE diagonal regardless of
    // the cooked / collision split id.
    let triangles = split_triangles(SPLIT_NE_SW);
    assert_eq!(triangles[0], (0, 1, 3));
    assert_eq!(triangles[1], (1, 2, 3));
    assert_eq!(diagonal(triangles), [1, 3]);
}

#[test]
fn warmed_room_quad_defers_to_sector_scaled_tomb_raider_splitter() {
    let options = WorldSurfaceOptions::new(DepthBand::whole(), DepthRange::new(16, 25_000))
        .with_tomb_raider_subdivision_sector_size(1664);
    let near = [
        ProjectedVertex::new(0, 0, 3354),
        ProjectedVertex::new(64, 0, 3354),
        ProjectedVertex::new(0, 64, 3354),
        ProjectedVertex::new(64, 64, 3354),
    ];
    let far = [
        ProjectedVertex::new(0, 0, 9000),
        ProjectedVertex::new(64, 0, 9000),
        ProjectedVertex::new(0, 64, 9000),
        ProjectedVertex::new(64, 64, 9000),
    ];

    assert!(tomb_raider_warmed_quad_requires_dynamic_submit(
        &options, near
    ));
    assert!(!tomb_raider_warmed_quad_requires_dynamic_submit(
        &options, far
    ));
    assert!(tomb_raider_warmed_quad_requires_dynamic_submit(
        &options.with_tomb_raider_subdivision_debug_levels(true),
        far,
    ));
}

#[test]
fn floor_wall_subdivision_mask_keeps_ceilings_on_authored_path() {
    let options = WorldSurfaceOptions::new(DepthBand::whole(), DepthRange::new(16, 25_000))
        .with_tomb_raider_subdivision_sector_size(1664)
        .with_tomb_raider_subdivision_kinds(TombRaiderSubdivisionKindMask::FLOOR_WALL);
    let floor = cached_surface_subdivision_options(
        options,
        CachedRoomSubdivisionMode::All,
        WorldSurfaceKind::Floor,
        false,
        false,
    );
    let ceiling = cached_surface_subdivision_options(
        options,
        CachedRoomSubdivisionMode::All,
        WorldSurfaceKind::Ceiling,
        false,
        false,
    );
    let wall = cached_surface_subdivision_options(
        options,
        CachedRoomSubdivisionMode::All,
        WorldSurfaceKind::Wall {
            direction: DIR_NORTH,
        },
        false,
        false,
    );

    assert!(floor.tomb_raider_subdivision);
    assert!(!ceiling.tomb_raider_subdivision);
    assert!(wall.tomb_raider_subdivision);
}

#[test]
fn unknown_split_id_falls_back_to_nw_se() {
    // Future split-ids (e.g. quad subdivision) shouldn't
    // empty the room -- fall through to the standard
    // diagonal so the user sees something while the
    // schema catches up.
    for unknown in [2u8, 3, 9, 200] {
        assert_eq!(split_triangles(unknown), SPLIT_NW_SE_TRIANGLES);
    }
}

#[test]
fn merge_horizontal_triangle_surface_combines_matching_triangles() {
    let uvs = [(0, 0), (TILE_UV, 0), (TILE_UV, TILE_UV), (0, TILE_UV)];
    let face_heights = [0, 0, 0, 0];
    let heights = [
        triangle_heights_from_quad(face_heights, SPLIT_NW_SE, 0),
        triangle_heights_from_quad(face_heights, SPLIT_NW_SE, 1),
    ];
    assert_eq!(
        merge_horizontal_triangle_surface(
            [Some(3), Some(3)],
            [uvs, uvs],
            heights,
            face_heights,
            SPLIT_NW_SE,
        ),
        Some((3, uvs))
    );
}

#[test]
fn merge_horizontal_triangle_surface_preserves_real_splits() {
    let uvs = [(0, 0), (TILE_UV, 0), (TILE_UV, TILE_UV), (0, TILE_UV)];
    let shifted_uvs = [(0, 0), (32, 0), (32, TILE_UV), (0, TILE_UV)];
    let face_heights = [0, 0, 0, 0];
    let heights = [
        triangle_heights_from_quad(face_heights, SPLIT_NW_SE, 0),
        triangle_heights_from_quad(face_heights, SPLIT_NW_SE, 1),
    ];

    assert_eq!(
        merge_horizontal_triangle_surface(
            [Some(3), Some(4)],
            [uvs, uvs],
            heights,
            face_heights,
            SPLIT_NW_SE,
        ),
        None
    );
    assert_eq!(
        merge_horizontal_triangle_surface(
            [Some(3), Some(3)],
            [uvs, shifted_uvs],
            heights,
            face_heights,
            SPLIT_NW_SE,
        ),
        None
    );
    assert_eq!(
        merge_horizontal_triangle_surface(
            [Some(3), None],
            [uvs, uvs],
            heights,
            face_heights,
            SPLIT_NW_SE,
        ),
        None
    );
}

#[test]
fn each_split_covers_every_corner() {
    // Sanity: every triangulation must reference all four
    // corners across its two triangles, otherwise the quad
    // has a hole.
    for split in [SPLIT_NW_SE, SPLIT_NE_SW] {
        let [t0, t1] = split_triangles(split);
        let mut seen = [false; 4];
        for i in [t0.0, t0.1, t0.2, t1.0, t1.1, t1.2] {
            seen[i] = true;
        }
        assert!(seen.iter().all(|&v| v), "split {split} misses a corner");
    }
}

#[test]
fn cardinal_wall_backs_face_their_owning_cell() {
    let projection = WorldProjection::new(160, 120, 200, 16);
    let y = 512;
    let center = WorldVertex::new(512, y, 512);
    let cases = [
        (
            DIR_NORTH,
            WorldCamera::from_basis(projection, center, Q12::ZERO, Q12::ONE, Q12::ZERO, Q12::ONE),
        ),
        (
            DIR_EAST,
            WorldCamera::from_basis(
                projection,
                center,
                Q12::NEG_ONE,
                Q12::ZERO,
                Q12::ZERO,
                Q12::ONE,
            ),
        ),
        (
            DIR_SOUTH,
            WorldCamera::from_basis(
                projection,
                center,
                Q12::ZERO,
                Q12::NEG_ONE,
                Q12::ZERO,
                Q12::ONE,
            ),
        ),
        (
            DIR_WEST,
            WorldCamera::from_basis(projection, center, Q12::ONE, Q12::ZERO, Q12::ZERO, Q12::ONE),
        ),
    ];

    for (direction, camera) in cases {
        let verts =
            wall_vertices(0, 0, 1024, direction, [0, 0, 1024, 1024]).expect("cardinal wall");
        let projected = camera
            .project_world_quad(verts)
            .expect("wall projects from owning cell");
        for (a, b, c) in SPLIT_NW_SE_TRIANGLES {
            assert!(
                projected_triangle_area(projected[a], projected[b], projected[c]) < 0,
                "direction {direction} wall back side should face owning cell"
            );
        }
    }
}

#[test]
fn diagonal_wall_vertices_use_runtime_corner_convention() {
    let nw_se = wall_vertices(0, 0, 1024, DIR_NORTH_WEST_SOUTH_EAST, [10, 20, 30, 40])
        .expect("nw-se diagonal wall");
    assert_eq!(nw_se[0], WorldVertex::new(0, 10, 0));
    assert_eq!(nw_se[1], WorldVertex::new(1024, 20, 1024));
    assert_eq!(nw_se[2], WorldVertex::new(1024, 30, 1024));
    assert_eq!(nw_se[3], WorldVertex::new(0, 40, 0));

    let ne_sw = wall_vertices(0, 0, 1024, DIR_NORTH_EAST_SOUTH_WEST, [50, 60, 70, 80])
        .expect("ne-sw diagonal wall");
    assert_eq!(ne_sw[0], WorldVertex::new(1024, 50, 0));
    assert_eq!(ne_sw[1], WorldVertex::new(0, 60, 1024));
    assert_eq!(ne_sw[2], WorldVertex::new(0, 70, 1024));
    assert_eq!(ne_sw[3], WorldVertex::new(1024, 80, 0));
}

#[test]
fn floors_face_playable_interior() {
    let projection = WorldProjection::new(160, 120, 200, 16);
    let camera = WorldCamera::orbit_yaw(
        projection,
        WorldVertex::new(512, 0, 512),
        1100,
        2048,
        Angle::ZERO,
    );
    let verts = [
        WorldVertex::new(0, 0, 0),
        WorldVertex::new(1024, 0, 0),
        WorldVertex::new(1024, 0, 1024),
        WorldVertex::new(0, 0, 1024),
    ];
    let projected = camera
        .project_world_quad(verts)
        .expect("floor projects from playable camera");

    for (a, b, c) in SPLIT_NW_SE_TRIANGLES {
        let area = projected_triangle_area(projected[a], projected[b], projected[c]);
        assert!(
            area > 0,
            "floor should not be culled from above: area={area} projected={projected:?}"
        );
    }
}

#[test]
fn wall_uvs_follow_physical_wall_corner_order() {
    assert_eq!(
        wall_uvs(),
        [(0, TILE_UV), (TILE_UV, TILE_UV), (TILE_UV, 0), (0, 0)]
    );
}

#[test]
fn wall_material_swaps_front_and_back_only() {
    let texture = TextureMaterial::opaque(0, 0, (128, 128, 128));
    assert_eq!(
        wall_material(WorldRenderMaterial::front(texture)).sidedness,
        SurfaceSidedness::Back
    );
    assert_eq!(
        wall_material(WorldRenderMaterial::back(texture)).sidedness,
        SurfaceSidedness::Front
    );
    assert_eq!(
        wall_material(WorldRenderMaterial::both(texture)).sidedness,
        SurfaceSidedness::Both
    );
}

/// Sector walls are solid geometry, so both faces are real surfaces. Culling
/// the interior face deleted any boundary wall for a player standing in the
/// cell that owns it, which is the only side of a room-bounding wall anyone can
/// stand on. `wall_material` still swaps the authored per-side texture, so the
/// two faces keep their own appearance; only the culling changes.
///
/// This is now the answer for a wall the cooker could NOT prove owner-facing,
/// hence `false` below. The proven case is covered by
/// `cache_build::wall_orientation_tests`.
#[test]
fn unproven_wall_materials_are_double_sided_in_every_direction() {
    let texture = TextureMaterial::opaque(0, 0, (128, 128, 128));
    for direction in [
        DIR_NORTH,
        DIR_EAST,
        DIR_SOUTH,
        DIR_WEST,
        DIR_NORTH_WEST_SOUTH_EAST,
        DIR_NORTH_EAST_SOUTH_WEST,
    ] {
        for material in [
            WorldRenderMaterial::front(texture),
            WorldRenderMaterial::back(texture),
            WorldRenderMaterial::both(texture),
        ] {
            assert_eq!(
                wall_material_for_direction(material, direction, false).sidedness,
                SurfaceSidedness::Both,
                "direction {direction} should render both wall faces"
            );
        }
    }
}

#[test]
fn material_texture_size_projects_default_uvs_once() {
    let material = WorldRenderMaterial::front(TextureMaterial::opaque(0, 0, (128, 128, 128)))
        .with_texture_size(32, 32);
    assert_eq!(
        material_uvs(
            material,
            [(0, 0), (TILE_UV, 0), (TILE_UV, TILE_UV), (0, TILE_UV)]
        ),
        [(0, 0), (32, 0), (32, 32), (0, 32)]
    );
}

#[test]
fn material_texture_size_preserves_authored_repeat_count() {
    let material = WorldRenderMaterial::front(TextureMaterial::opaque(0, 0, (128, 128, 128)))
        .with_texture_size(32, 64);
    assert_eq!(
        material_uvs(material, [(0, 0), (128, 0), (128, TILE_UV), (0, TILE_UV)]),
        [(0, 0), (64, 0), (64, TILE_UV), (0, TILE_UV)]
    );
}

#[test]
fn generated_cache_records_reconstruct_cached_samples() {
    let vertices = [
        WorldVertex::new(0, 10, 0),
        WorldVertex::new(1024, 20, 0),
        WorldVertex::new(1024, 30, 1024),
        WorldVertex::new(0, 40, 1024),
    ];
    let vertex_records = [
        LevelCachedRoomVertexRecord {
            x: vertices[0].x,
            y: vertices[0].y,
            z: vertices[0].z,
        },
        LevelCachedRoomVertexRecord {
            x: vertices[1].x,
            y: vertices[1].y,
            z: vertices[1].z,
        },
        LevelCachedRoomVertexRecord {
            x: vertices[2].x,
            y: vertices[2].y,
            z: vertices[2].z,
        },
        LevelCachedRoomVertexRecord {
            x: vertices[3].x,
            y: vertices[3].y,
            z: vertices[3].z,
        },
    ];
    assert_eq!(
        cached_room_vertices_from_level_records(&vertex_records),
        &vertices
    );

    let cell_records = [LevelCachedRoomCellRecord {
        x: 3,
        z: 4,
        min_y: 10,
        max_y: 40,
        visibility_center: [512, 25, 512],
        visibility_radius: 1040,
        surface_first: 7,
        surface_count: 1,
        vertex_first: 2,
        vertex_count: 4,
    }];
    let cells = cached_room_cells_from_level_records(&cell_records);
    assert_eq!(cells[0].x, 3);
    assert_eq!(cells[0].visibility_center, [512, 25, 512]);
    assert_eq!(cells[0].surface_first, 7);
    assert_eq!(cells[0].vertex_first, 2);

    let baked = [(1, 2, 3), (4, 5, 6), (7, 8, 9), (10, 11, 12)];
    let surface = CachedRoomSurface::new(
        5,
        [0, 1, 2, 3],
        [(0, 0), (32, 0), (32, 64), (0, 64)],
        WorldSurfaceSample {
            kind: WorldSurfaceKind::Wall {
                direction: DIR_EAST,
            },
            sx: 3,
            sz: 4,
            center: RoomPoint::ZERO,
            baked_vertex_rgb: Some(baked),
            ordinal: 9,
        },
        SPLIT_NE_SW,
        1,
    );
    let surface_records = [LevelCachedRoomSurfaceRecord {
        material_slot: surface.material_slot,
        vertex_indices: surface.vertex_indices,
        sample_sx: surface.sample_sx,
        sample_sz: surface.sample_sz,
        sample_ordinal: surface.sample_ordinal,
        uv_words: surface.uv_words,
        baked_vertex_rgb: surface.baked_vertex_rgb,
        kind_flags: surface.kind_flags,
        wall_direction: surface.wall_direction,
        split: surface.split,
        triangle_index: surface.triangle_index,
    }];
    let surfaces = cached_room_surfaces_from_level_records(&surface_records);
    assert_eq!(surfaces[0], surface);
    assert_eq!(surfaces[0].uvs(), [(0, 0), (32, 0), (32, 64), (0, 64)]);
    let sample = surfaces[0].sample_with_center(vertices, true);
    assert_eq!(
        sample.kind,
        WorldSurfaceKind::Wall {
            direction: DIR_EAST
        }
    );
    assert_eq!(sample.sx, 3);
    assert_eq!(sample.sz, 4);
    assert_eq!(sample.ordinal, 9);
    assert_eq!(sample.baked_vertex_rgb, Some(baked));
    assert_eq!(
        sample.center,
        cached_surface_center(vertices, SPLIT_NE_SW, 1)
    );
}

#[test]
fn room_quad_prewarm_builds_baked_packet_payload_in_split_order() {
    let colors = [(1, 2, 3), (4, 5, 6), (7, 8, 9), (10, 11, 12)];
    let surface = CachedRoomSurface::new(
        0,
        [0, 1, 2, 3],
        [(1, 2), (3, 4), (5, 6), (7, 8)],
        WorldSurfaceSample {
            kind: WorldSurfaceKind::Floor,
            sx: 0,
            sz: 0,
            center: RoomPoint::ZERO,
            baked_vertex_rgb: Some(colors),
            ordinal: 0,
        },
        SPLIT_NE_SW,
        WHOLE_QUAD_TRIANGLE_INDEX,
    );
    let split_triangle = CachedRoomSurface::new(
        0,
        [0, 1, 2, 3],
        [(1, 2), (3, 4), (5, 6), (7, 8)],
        WorldSurfaceSample {
            kind: WorldSurfaceKind::Floor,
            sx: 0,
            sz: 0,
            center: RoomPoint::ZERO,
            baked_vertex_rgb: Some(colors),
            ordinal: 0,
        },
        SPLIT_NE_SW,
        0,
    );
    let material = WorldRenderMaterial::front(TextureMaterial::opaque(3, 4, (0x80, 0x80, 0x80)));
    let mut quads = [QuadTexturedGouraud::EMPTY, QuadTexturedGouraud::EMPTY];
    let mut valid = [0x55, 0x55];

    let warmed = prewarm_indexed_cached_room_quads(
        &[surface, split_triangle],
        &[material],
        &mut quads,
        &mut valid,
    );

    assert_eq!(warmed, 1);
    assert_eq!(valid[0], 0x80 | 0x02);
    assert_eq!(valid[1], 0);
    // NE-SW packet order is 0,1,3,2 for UVs and baked colours alike.
    assert_eq!(quads[0].uv0_clut as u16, surface.uv_words[0]);
    assert_eq!(quads[0].uv1_tpage as u16, surface.uv_words[1]);
    assert_eq!(quads[0].uv2 as u16, surface.uv_words[3]);
    assert_eq!(quads[0].uv3 as u16, surface.uv_words[2]);
    assert_eq!(quads[0].color0_cmd & 0x00ff_ffff, 0x0003_0201);
    assert_eq!(quads[0].color1, 0x0006_0504);
    assert_eq!(quads[0].color2, 0x000c_0b0a);
    assert_eq!(quads[0].color3, 0x0009_0807);
}

#[test]
fn room_quad_prewarm_skips_dynamic_and_translucent_materials() {
    let surface = CachedRoomSurface::new(
        0,
        [0, 1, 2, 3],
        [(0, 0), (64, 0), (64, 64), (0, 64)],
        WorldSurfaceSample {
            kind: WorldSurfaceKind::Floor,
            sx: 0,
            sz: 0,
            center: RoomPoint::ZERO,
            baked_vertex_rgb: Some([(128, 128, 128); 4]),
            ordinal: 0,
        },
        SPLIT_NW_SE,
        WHOLE_QUAD_TRIANGLE_INDEX,
    );
    let dynamic = WorldRenderMaterial::front(TextureMaterial::opaque(0, 0, (128, 128, 128)))
        .with_animation(WorldMaterialAnimation::UvScroll {
            speed_u_q8: 256,
            speed_v_q8: 0,
            phase_u: 0,
            phase_v: 0,
        });
    let translucent = WorldRenderMaterial::front(TextureMaterial::blended(
        0,
        0,
        (128, 128, 128),
        psx_gpu::material::BlendMode::Average,
    ));
    for material in [dynamic, translucent] {
        let mut quad = [QuadTexturedGouraud::EMPTY];
        let mut valid = [0x55];
        assert_eq!(
            prewarm_indexed_cached_room_quads(&[surface], &[material], &mut quad, &mut valid),
            0
        );
        assert_eq!(valid, [0]);
    }
}

#[test]
fn warmed_quad_culls_in_authored_order_before_gpu_packet_reordering() {
    // Clockwise/back-facing NW-SE quad. Reordering this into GP0(3Ch) packet
    // order flips only one of the packet triangles, so culling the packet
    // vertices would incorrectly retain the authored back face.
    let projected = [
        ProjectedVertex::new(0, 0, 100),
        ProjectedVertex::new(0, 10, 100),
        ProjectedVertex::new(10, 10, 100),
        ProjectedVertex::new(10, 0, 100),
    ];
    assert!(encoded_warmed_room_quad_backface_culled(projected, 0x80));
    assert!(!encoded_warmed_room_quad_backface_culled(
        projected,
        0x80 | 0x04,
    ));
}

#[test]
fn cached_cell_visibility_sphere_tightly_and_conservatively_bounds_cell_aabb() {
    let (flat_center, flat_radius) = cell_visibility_bounds(0, 0, 1664, 1152, 1152);
    assert_eq!(flat_center, WorldVertex::new(832, 1152, 832));
    assert_eq!(flat_radius, 1177);
    assert!(
        flat_radius < 2496,
        "old loose radius should stay eliminated"
    );
    assert!(flat_radius.saturating_mul(flat_radius) >= 832 * 832 * 2);

    let (odd_center, odd_radius) = cell_visibility_bounds(0, 0, 5, -5, 0);
    assert_eq!(odd_center, WorldVertex::new(2, -2, 2));
    // Integer midpoint truncation makes the far corner delta (3, 3, 3).
    assert!(odd_radius.saturating_mul(odd_radius) >= 3 * 3 * 3);
    assert_eq!(odd_radius, 6);
}

#[test]
fn cell_frustum_sphere_test_includes_plane_normal_support() {
    let camera = WorldCamera::from_basis(
        WorldProjection::new(160, 120, 200, 40),
        WorldVertex::new(0, 0, 0),
        Q12::ZERO,
        Q12::ONE,
        Q12::ZERO,
        Q12::ONE,
    );
    let options = WorldSurfaceOptions::new(
        crate::DepthBand::whole(),
        crate::DepthRange::new(40, 10_000),
    );
    let frustum = CellFrustum::new(&camera, options, 0);

    // At z=1000 the right edge is x=800. A radius-100 sphere centered at
    // x=900 still intersects the side plane; `radius*focal` alone incorrectly
    // rejected it, while the full plane-normal support must retain it.
    assert!(frustum.sphere_visible(ViewVertex::new(900, 0, 1000), 100));
    assert!(!frustum.sphere_visible(ViewVertex::new(1100, 0, 1000), 100));
}

#[test]
fn cell_frustum_aabb_rejects_flat_cell_retained_by_bounding_sphere() {
    let camera = WorldCamera::from_basis(
        WorldProjection::new(160, 120, 200, 40),
        WorldVertex::ZERO,
        Q12::ZERO,
        Q12::ONE,
        Q12::ZERO,
        Q12::ONE,
    );
    let options = WorldSurfaceOptions::new(
        crate::DepthBand::whole(),
        crate::DepthRange::new(40, 10_000),
    );
    let frustum = CellFrustum::new(&camera, options, 0);
    let view = ViewVertex::new(990, 0, 1_000);

    // A sphere enclosing the 200x20x200 cell intersects the right plane, but
    // the actual world-axis-aligned cell box is wholly outside it. The cached
    // all-cells path can therefore skip projection and surface traversal
    // without losing any potentially visible geometry.
    assert!(frustum.sphere_visible_no_far(view, 142));
    assert!(!frustum.cell_aabb_visible(view, 100, 10, 100));
}

#[test]
fn portal_cell_window_union_keeps_every_admitting_path() {
    let left = PortalCellWindow::new(-4096, -1024, -2048, 2048);
    let right = PortalCellWindow::new(1024, 4096, -1024, 3072);
    assert_eq!(
        left.union(right),
        PortalCellWindow::new(-4096, 4096, -2048, 3072)
    );

    let camera = WorldCamera::from_basis(
        WorldProjection::new(160, 120, 200, 40),
        WorldVertex::ZERO,
        Q12::ZERO,
        Q12::ONE,
        Q12::ZERO,
        Q12::ONE,
    );
    let options = WorldSurfaceOptions::new(
        crate::DepthBand::whole(),
        crate::DepthRange::new(40, 10_000),
    );
    let frustum = CellFrustum::new(&camera, options, 0);
    let narrow_left = PortalCellWindow::new(-4096, -2048, -2048, 2048);
    let narrow_right = PortalCellWindow::new(2048, 4096, -2048, 2048);
    let right_path_cell = ViewVertex::new(750, 0, 1_000);

    assert!(!frustum.cell_aabb_intersects_portal_window(
        right_path_cell,
        100,
        100,
        100,
        narrow_left,
    ));
    assert!(frustum.cell_aabb_intersects_portal_window(
        right_path_cell,
        100,
        100,
        100,
        narrow_left.union(narrow_right),
    ));
}

#[test]
fn cell_frustum_aabb_fast_paths_match_widened_reference() {
    let camera = WorldCamera::from_basis(
        WorldProjection::new(160, 120, 320, 40),
        WorldVertex::ZERO,
        Q12::from_raw(2896),
        Q12::from_raw(2896),
        Q12::from_raw(-1567),
        Q12::from_raw(3785),
    );
    let options = WorldSurfaceOptions::new(
        crate::DepthBand::whole(),
        crate::DepthRange::new(40, 1_000_000),
    );
    let frustum = CellFrustum::new(&camera, options, 96);
    let reference = |view: ViewVertex, half_x: i32, half_y: i32, half_z: i32| {
        let half_x = half_x.max(0);
        let half_y = half_y.max(0);
        let half_z = half_z.max(0);
        let extent_x = cell_aabb_extent_wide(frustum.view_abs[0], half_x, half_y, half_z);
        let extent_y = cell_aabb_extent_wide(frustum.view_abs[1], half_x, half_y, half_z);
        let extent_z = cell_aabb_extent_wide(frustum.view_abs[2], half_x, half_y, half_z);
        if view.z < frustum.near.saturating_sub(extent_z)
            || view.z > frustum.far.saturating_add(extent_z)
        {
            return false;
        }
        cell_aabb_lateral_visible_wide(
            view,
            view.z.max(frustum.near),
            extent_x,
            extent_y,
            extent_z,
            frustum.focal,
            frustum.half_w,
            frustum.half_h,
        )
    };

    let views = [
        ViewVertex::new(0, 0, 40),
        ViewVertex::new(900, -300, 1_000),
        ViewVertex::new(174_000, 174_000, 174_000),
        ViewVertex::new(174_001, -174_001, 174_001),
        ViewVertex::new(2_000_000, 500_000, 900_000),
    ];
    let half_extents = [
        (0, 0, 0),
        (832, 576, 832),
        (174_000, 174_000, 174_000),
        (174_001, 174_001, 174_001),
        (i32::MAX, 2_000_000, 1_000_000),
        (-1, -20, -300),
    ];
    for view in views {
        for (half_x, half_y, half_z) in half_extents {
            assert_eq!(
                frustum.cell_aabb_visible(view, half_x, half_y, half_z),
                reference(view, half_x, half_y, half_z),
                "view={view:?}, half=({half_x},{half_y},{half_z})"
            );
        }
    }
}

#[test]
fn floor_depth_uses_farthest_projected_depth() {
    const ZERO: TriTextured = TriTextured::new(
        [(0, 0), (0, 0), (0, 0)],
        [(0, 0), (0, 0), (0, 0)],
        0,
        0,
        (0, 0, 0),
    );
    let mut ot_storage = psx_gpu::ot::OrderingTable::<8>::new();
    let mut ot = crate::OtFrame::begin(&mut ot_storage);
    let mut triangle_storage = [const { ZERO }; 4];
    let mut triangles = PrimitiveArena::new(&mut triangle_storage);
    let mut commands = [crate::WorldTriCommand::EMPTY; 4];
    let mut pass = WorldRenderPass::new(&mut ot, &mut commands);

    let projection = WorldProjection::new(160, 120, 200, 16);
    let camera = WorldCamera::orbit_yaw(
        projection,
        WorldVertex::new(512, 0, 512),
        1100,
        2048,
        Angle::ZERO,
    );
    let options =
        WorldSurfaceOptions::new(crate::DepthBand::whole(), crate::DepthRange::new(0, 4096));
    emit_floor(
        0,
        0,
        1024,
        [0, 0, 0, 0],
        SPLIT_NW_SE,
        [(0, 0), (TILE_UV, 0), (TILE_UV, TILE_UV), (0, TILE_UV)],
        WorldRenderMaterial::front(TextureMaterial::opaque(0, 0, (128, 128, 128))),
        &camera,
        options,
        &mut triangles,
        &mut pass,
    );
    assert_eq!(pass.command_len(), 2);
    drop(pass);

    let projected = camera
        .project_world_quad([
            WorldVertex::new(0, 0, 0),
            WorldVertex::new(1024, 0, 0),
            WorldVertex::new(1024, 0, 1024),
            WorldVertex::new(0, 0, 1024),
        ])
        .expect("floor projects from playable camera");
    let [(a, b, c), (d, e, f)] = SPLIT_NW_SE_TRIANGLES;
    assert_eq!(
        commands[0].depth_raw(),
        max3(projected[a].sz, projected[b].sz, projected[c].sz) + HORIZONTAL_DEPTH_BIAS
    );
    assert_eq!(
        commands[1].depth_raw(),
        max3(projected[d].sz, projected[e].sz, projected[f].sz) + HORIZONTAL_DEPTH_BIAS
    );
}

#[test]
fn cached_full_ceiling_faces_playable_interior() {
    let mut ot_storage = psx_gpu::ot::OrderingTable::<8>::new();
    let mut ot = crate::OtFrame::begin(&mut ot_storage);
    let mut packet_scratch = crate::PrimitivePacketScratch::<4>::ZERO;
    let mut triangles = crate::PrimitivePacketArena::new(&mut packet_scratch);
    let mut commands = [crate::WorldTriCommand::EMPTY; 4];
    let mut pass = WorldRenderPass::new(&mut ot, &mut commands);

    let projection = WorldProjection::new(160, 120, 200, 16);
    let camera = WorldCamera::orbit_yaw(
        projection,
        WorldVertex::new(512, 1024, 512),
        0,
        2048,
        Angle::ZERO,
    );
    let options =
        WorldSurfaceOptions::new(crate::DepthBand::whole(), crate::DepthRange::new(0, 4096))
            .with_textured_triangle_splitting(false);
    let uvs = [(0, 0), (TILE_UV, 0), (TILE_UV, TILE_UV), (0, TILE_UV)];
    let vertices = horizontal_vertices(0, 0, 1024, [1024, 1024, 1024, 1024]);
    let cells = [CachedRoomCell::new(0, 0, 1024, 1024, 1024, 0, 1, 0, 4)];
    let surface = CachedRoomSurface::new(
        0,
        [0, 1, 2, 3],
        uvs,
        WorldSurfaceSample {
            kind: WorldSurfaceKind::Ceiling,
            sx: 0,
            sz: 0,
            center: horizontal_face_center(0, 0, 1024, [1024, 1024, 1024, 1024]),
            baked_vertex_rgb: None,
            ordinal: 0,
        },
        SPLIT_NW_SE,
        WHOLE_QUAD_TRIANGLE_INDEX,
    );
    let surfaces = [surface];
    let visible_cells = [GridVisibleCell::new(0, 0, 1024, 1024)];
    let cell_vertices = [0u16, 1, 2, 3];
    let mut projected_indices = [0u16; 4];
    let mut projected = [ProjectedVertex::new(0, 0, 0); 4];
    // Simulate arbitrary bytes left by the menu/gameplay RAM overlay. The
    // renderer must initialize only its tiny seen-bit prefix and never trust
    // persistent per-vertex readiness state.
    let mut projected_depths = [i32::MAX; 4];
    let mut accepted_cell_indices = [0u16; 1];
    let mut accepted_cell_depths = [0; 1];

    let stats = draw_indexed_cached_room_vertex_lit_visible_cells(
        &cells,
        &cell_vertices,
        &vertices,
        &surfaces,
        &mut projected_indices,
        &mut projected,
        &mut projected_depths,
        &mut accepted_cell_indices,
        &mut accepted_cell_depths,
        1,
        1024,
        &[WorldRenderMaterial::front(TextureMaterial::opaque(
            0,
            0,
            (128, 128, 128),
        ))],
        &NoWorldSurfaceLighting,
        false,
        &camera,
        options,
        CachedRoomDepthMode::FixedCell,
        CachedRoomSubdivisionMode::All,
        &visible_cells,
        0,
        None,
        None,
        &mut triangles,
        &mut pass,
    );
    assert_eq!(stats.surfaces_considered, 1);
    assert_eq!(projected_indices, [0, 1, 2, 3]);
    assert!(projected_depths.iter().all(|&depth| depth != i32::MAX));
    // A hardware-safe ceiling keeps the compact GP0(3Ch) quad path.
    assert_eq!(pass.command_len(), 1);
    drop(pass);

    let expected_depth = tile_camera_depth(&camera, visible_cells[0], 1024) + HORIZONTAL_DEPTH_BIAS;
    assert_eq!(commands[0].depth_raw(), expected_depth);
}

#[test]
fn cached_surface_crossing_near_plane_keeps_visible_half() {
    let mut ot_storage = psx_gpu::ot::OrderingTable::<8>::new();
    let mut ot = crate::OtFrame::begin(&mut ot_storage);
    let mut packet_scratch = crate::PrimitivePacketScratch::<16>::ZERO;
    let mut triangles = crate::PrimitivePacketArena::new(&mut packet_scratch);
    let mut commands = [crate::WorldTriCommand::EMPTY; 16];
    let mut pass = WorldRenderPass::new(&mut ot, &mut commands);
    let projection = WorldProjection::new(160, 120, 200, 40);
    let camera = WorldCamera::from_basis(
        projection,
        WorldVertex::new(0, 0, 0),
        crate::Q12::ZERO,
        crate::Q12::ONE,
        crate::Q12::ZERO,
        crate::Q12::ONE,
    );
    let vertices = [
        WorldVertex::new(-50, 0, 10),
        WorldVertex::new(50, 0, 10),
        WorldVertex::new(50, 0, -100),
        WorldVertex::new(-50, 0, -100),
    ];
    let cells = [CachedRoomCell::new(0, 0, 0, 0, 128, 0, 1, 0, 4)];
    let surface = CachedRoomSurface::new(
        0,
        [0, 1, 2, 3],
        [(0, 0), (TILE_UV, 0), (TILE_UV, TILE_UV), (0, TILE_UV)],
        WorldSurfaceSample {
            kind: WorldSurfaceKind::Floor,
            sx: 0,
            sz: 0,
            center: RoomPoint::new(0, 0, -45),
            baked_vertex_rgb: None,
            ordinal: 0,
        },
        SPLIT_NW_SE,
        WHOLE_QUAD_TRIANGLE_INDEX,
    );
    let mut projected_indices = [0u16; 4];
    let mut projected = [ProjectedVertex::default(); 4];
    let mut projected_depths = [0; 4];
    let mut accepted_cell_indices = [0u16; 1];
    let mut accepted_cell_depths = [0; 1];

    let stats = draw_indexed_cached_room_vertex_lit_visible_cells(
        &cells,
        &[0, 1, 2, 3],
        &vertices,
        &[surface],
        &mut projected_indices,
        &mut projected,
        &mut projected_depths,
        &mut accepted_cell_indices,
        &mut accepted_cell_depths,
        1,
        1024,
        &[WorldRenderMaterial::both(TextureMaterial::opaque(
            0,
            0,
            (128, 128, 128),
        ))],
        &NoWorldSurfaceLighting,
        false,
        &camera,
        WorldSurfaceOptions::new(crate::DepthBand::whole(), crate::DepthRange::new(0, 4096)),
        CachedRoomDepthMode::PerTriangle,
        CachedRoomSubdivisionMode::All,
        &[GridVisibleCell::new(0, 0, 0, 128)],
        0,
        None,
        None,
        &mut triangles,
        &mut pass,
    );

    assert_eq!(stats.surfaces_considered, 1);
    assert!(projected.iter().any(|vertex| !vertex.is_valid()));
    assert!(pass.command_len() > 0);
}

#[test]
fn hybrid_depth_uses_triangle_depth_for_sloped_horizontal_surfaces() {
    let projected = [
        ProjectedVertex::new(0, 0, 1024),
        ProjectedVertex::new(64, 0, 1056),
        ProjectedVertex::new(64, 64, 1088),
        ProjectedVertex::new(0, 64, 1040),
    ];
    let surface = CachedRoomSurface::new(
        0,
        [0, 1, 2, 3],
        [(0, 0), (64, 0), (64, 64), (0, 64)],
        WorldSurfaceSample {
            kind: WorldSurfaceKind::Floor,
            sx: 0,
            sz: 0,
            center: RoomPoint::ZERO,
            baked_vertex_rgb: None,
            ordinal: 0,
        },
        SPLIT_NW_SE,
        WHOLE_QUAD_TRIANGLE_INDEX,
    );
    let sloped_surface = surface.with_horizontal_non_flat(true);

    assert!(!cached_surface_uses_triangle_depth(
        CachedRoomDepthMode::Hybrid,
        WorldSurfaceKind::Floor,
        surface,
        projected,
    ));
    assert!(cached_surface_uses_triangle_depth(
        CachedRoomDepthMode::Hybrid,
        WorldSurfaceKind::Floor,
        sloped_surface,
        projected,
    ));
    assert!(cached_surface_uses_triangle_depth(
        CachedRoomDepthMode::PerTriangle,
        WorldSurfaceKind::Wall {
            direction: DIR_EAST,
        },
        surface,
        projected,
    ));
    assert!(!cached_surface_uses_triangle_depth(
        CachedRoomDepthMode::Hybrid,
        WorldSurfaceKind::Wall {
            direction: DIR_EAST,
        },
        surface,
        [
            ProjectedVertex::new(0, 0, 1024),
            ProjectedVertex::new(64, 0, 2048),
            ProjectedVertex::new(64, 64, 2112),
            ProjectedVertex::new(0, 64, 1088),
        ],
    ));
    assert!(cached_surface_uses_triangle_depth(
        CachedRoomDepthMode::HybridWalls,
        WorldSurfaceKind::Wall {
            direction: DIR_EAST,
        },
        surface,
        [
            ProjectedVertex::new(0, 0, 1024),
            ProjectedVertex::new(64, 0, 2048),
            ProjectedVertex::new(64, 64, 2112),
            ProjectedVertex::new(0, 64, 1088),
        ],
    ));
}

#[test]
fn horizontal_face_center_uses_cell_midpoint_and_average_height() {
    assert_eq!(
        horizontal_face_center(2, 3, 1024, [0, 512, 1024, 512]),
        RoomPoint::new(2560, 512, 3584)
    );
}

#[test]
fn grid_visible_cell_camera_depth_fits_existing_padding() {
    assert_eq!(core::mem::size_of::<GridVisibleCell>(), 16);
}

#[test]
fn world_uv_scroll_uses_video_rate_and_wraps_inside_the_texture_window() {
    let animation = WorldMaterialAnimation::UvScroll {
        speed_u_q8: 2 * 256,
        speed_v_q8: -2 * 256,
        phase_u: 250,
        phase_v: 250,
    };
    assert_eq!(animation.uv_offset(30, 60, 64, 64), (59, 57));
    assert_eq!(animation.uv_offset(25, 50, 64, 64), (59, 57));
    assert_eq!(animation.uv_offset(30, 60, 32, 16), (27, 9));
}

#[test]
fn world_flipbook_offsets_into_one_resident_atlas() {
    let animation = WorldMaterialAnimation::Flipbook {
        columns: 4,
        frame_count: 6,
        ticks_per_frame: 2,
        phase: 1,
    };
    assert_eq!(animation.uv_offset(0, 60, 16, 16), (16, 0));
    assert_eq!(animation.uv_offset(6, 60, 16, 16), (0, 16));
    assert_eq!(animation.uv_offset(10, 60, 16, 16), (0, 0));
}

#[test]
fn wall_face_center_uses_emitted_runtime_wall_geometry() {
    assert_eq!(
        wall_face_center(
            0,
            0,
            1024,
            DIR_EAST,
            [0, 0, 1024, 1024],
            psx_asset::WORLD_WALL_SHAPE_QUAD
        ),
        Some(RoomPoint::new(1024, 512, 512))
    );
    assert_eq!(
        wall_face_center(
            0,
            0,
            1024,
            DIR_NORTH,
            [0, 0, 1024, 1024],
            psx_asset::WORLD_WALL_SHAPE_QUAD
        ),
        Some(RoomPoint::new(512, 512, 0))
    );
    assert_eq!(
        wall_face_center(
            0,
            0,
            1024,
            DIR_NORTH,
            [0, 0, 1024, 1024],
            psx_asset::WORLD_WALL_SHAPE_DROP_TOP_RIGHT
        ),
        Some(RoomPoint::new(341, 341, 0))
    );
}

fn projected_triangle_area(a: ProjectedVertex, b: ProjectedVertex, c: ProjectedVertex) -> i32 {
    let ax = (b.sx as i32) - (a.sx as i32);
    let ay = (b.sy as i32) - (a.sy as i32);
    let bx = (c.sx as i32) - (a.sx as i32);
    let by = (c.sy as i32) - (a.sy as i32);
    ax * by - ay * bx
}

const fn max3(a: i32, b: i32, c: i32) -> i32 {
    let ab = if a > b { a } else { b };
    if ab > c {
        ab
    } else {
        c
    }
}

/// Exploratory harness: submit one cached wall quad standing `distance` in
/// front of the camera and report what the room renderer emitted.
fn wall_submission_probe(distance: i32, half_extent: i32) -> (usize, GridVisibilityStats) {
    wall_submission_probe_pooled(distance, half_extent, false)
}

/// As above, but `warmed` routes the surface through the prebuilt room-quad
/// pool, which is what the shipping runtime does.
fn wall_submission_probe_pooled(
    distance: i32,
    half_extent: i32,
    warmed: bool,
) -> (usize, GridVisibilityStats) {
    let mut ot_storage = psx_gpu::ot::OrderingTable::<64>::new();
    let mut ot = crate::OtFrame::begin(&mut ot_storage);
    let mut packet_scratch = crate::PrimitivePacketScratch::<256>::ZERO;
    let mut triangles = crate::PrimitivePacketArena::new(&mut packet_scratch);
    let mut commands = [crate::WorldTriCommand::EMPTY; 256];
    let mut pass = WorldRenderPass::new(&mut ot, &mut commands);
    let projection = WorldProjection::new(160, 120, 200, 16);
    let camera = WorldCamera::from_basis(
        projection,
        WorldVertex::new(0, 0, 0),
        Q12::ZERO,
        Q12::ONE,
        Q12::ZERO,
        Q12::ONE,
    );
    // Vertical wall plane facing the camera, which looks down -Z.
    let z = -distance;
    let vertices = [
        WorldVertex::new(-half_extent, half_extent, z),
        WorldVertex::new(half_extent, half_extent, z),
        WorldVertex::new(half_extent, -half_extent, z),
        WorldVertex::new(-half_extent, -half_extent, z),
    ];
    let cells = [CachedRoomCell::new(
        0,
        0,
        0,
        -half_extent,
        half_extent,
        0,
        1,
        0,
        4,
    )];
    let surface = CachedRoomSurface::new(
        0,
        [0, 1, 2, 3],
        [(0, 0), (TILE_UV, 0), (TILE_UV, TILE_UV), (0, TILE_UV)],
        WorldSurfaceSample {
            kind: WorldSurfaceKind::Wall {
                direction: DIR_NORTH,
            },
            sx: 0,
            sz: 0,
            center: RoomPoint::new(0, 0, z),
            baked_vertex_rgb: None,
            ordinal: 0,
        },
        SPLIT_NW_SE,
        WHOLE_QUAD_TRIANGLE_INDEX,
    );
    let mut projected_indices = [0u16; 4];
    let mut projected = [ProjectedVertex::default(); 4];
    let mut projected_depths = [0; 4];
    let mut accepted_cell_indices = [0u16; 1];
    let mut accepted_cell_depths = [0; 1];
    let mut prebuilt_quads = [QuadTexturedGouraud::EMPTY; 8];
    let mut prebuilt_valid = [0u8; 8];

    let stats = draw_indexed_cached_room_vertex_lit_visible_cells(
        &cells,
        &[0, 1, 2, 3],
        &vertices,
        &[surface],
        &mut projected_indices,
        &mut projected,
        &mut projected_depths,
        &mut accepted_cell_indices,
        &mut accepted_cell_depths,
        1,
        1664,
        &[WorldRenderMaterial::both(TextureMaterial::opaque(
            0,
            0,
            (128, 128, 128),
        ))],
        &NoWorldSurfaceLighting,
        false,
        &camera,
        WorldSurfaceOptions::new(DepthBand::whole(), DepthRange::new(16, 25_000))
            .with_tomb_raider_subdivision_sector_size(1664)
            .with_tomb_raider_subdivision_max_levels(1)
            .with_tomb_raider_subdivision_kinds(TombRaiderSubdivisionKindMask::FLOOR_WALL),
        CachedRoomDepthMode::Hybrid,
        CachedRoomSubdivisionMode::All,
        &[GridVisibleCell::new(0, 0, -half_extent, half_extent)],
        0,
        None,
        warmed.then(|| (&mut prebuilt_quads[..], &mut prebuilt_valid[..])),
        &mut triangles,
        &mut pass,
    );
    (pass.command_len(), stats)
}

/// Walls must take the same Tomb Raider subdivision schedule as floors, and
/// must keep rendering when the camera is close enough that the projected quad
/// blows past the PS1 hardware extent. Both were reported broken in cortex_v1;
/// this pins the room-renderer half of that report.
#[test]
fn wall_surfaces_subdivide_inside_the_band_and_survive_close_range() {
    let far_depth = 1664 * 5;
    let underdraw_depth = far_depth / 2;
    let emitted = |distance| wall_submission_probe(distance, 832).0;

    // Beyond the subdivision band the wall stays one authored quad.
    assert_eq!(emitted(far_depth + 1_000), 1);
    // Inside the band it becomes four generated leaves, plus the authored
    // crack-cover polygon while the root is still past the underdraw depth.
    assert_eq!(emitted(far_depth - 1_000), 5);
    assert_eq!(emitted(underdraw_depth - 1_000), 4);
    // Close range must keep emitting geometry rather than dropping the wall.
    for distance in [400, 200, 100, 50] {
        assert!(
            emitted(distance) >= 4,
            "wall vanished at distance {distance}"
        );
    }
}

/// The shipping runtime submits room quads through the prebuilt pool. That path
/// decides whether it can skip the hardware splitter from
/// `ProjectedQuadMetrics::hardware_extent_safe`, which tests only the projected
/// SPAN. Screen coordinates leave the GTE already saturated to signed 11 bits,
/// so a wall close enough to project off-screen collapses onto the saturation
/// boundary and its span shrinks back into range. The quad then looks "safe",
/// skips the splitter, and is emitted as a degenerate primitive covering
/// nothing: the cortex_v1 disappearing-wall report.
#[test]
fn warmed_wall_quads_keep_splitting_at_point_blank_range() {
    for distance in [400, 200, 100, 50] {
        let dynamic = wall_submission_probe_pooled(distance, 832, false).0;
        let warmed = wall_submission_probe_pooled(distance, 832, true).0;
        assert!(
            warmed >= 4,
            "warmed wall collapsed at distance {distance}: {warmed} commands (dynamic path emits {dynamic})"
        );
    }
}
