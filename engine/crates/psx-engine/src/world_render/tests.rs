use super::*;
use crate::Angle;
use crate::PrimitiveArena;
use crate::{ProjectedVertex, WorldProjection, Q12};

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

#[test]
fn diagonal_wall_materials_are_forced_double_sided() {
    let texture = TextureMaterial::opaque(0, 0, (128, 128, 128));
    assert_eq!(
        wall_material_for_direction(WorldRenderMaterial::front(texture), DIR_NORTH).sidedness,
        SurfaceSidedness::Back
    );
    assert_eq!(
        wall_material_for_direction(
            WorldRenderMaterial::front(texture),
            DIR_NORTH_WEST_SOUTH_EAST
        )
        .sidedness,
        SurfaceSidedness::Both
    );
    assert_eq!(
        wall_material_for_direction(
            WorldRenderMaterial::back(texture),
            DIR_NORTH_EAST_SOUTH_WEST
        )
        .sidedness,
        SurfaceSidedness::Both
    );
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
        &camera,
        options,
        CachedRoomDepthMode::FixedCell,
        CachedRoomSubdivisionMode::All,
        &visible_cells,
        0,
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
        &camera,
        WorldSurfaceOptions::new(crate::DepthBand::whole(), crate::DepthRange::new(0, 4096)),
        CachedRoomDepthMode::PerTriangle,
        CachedRoomSubdivisionMode::All,
        &[GridVisibleCell::new(0, 0, 0, 128)],
        0,
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
