use super::*;
use crate::GridVerticalFace;

#[test]
fn generated_room_cache_counts_match_runtime_builder() {
    let project = project_with_one_room();
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "{report:?}");
    let package = package.expect("package");
    let cache = package.room_surface_caches[0];
    assert!(cache.cell_vertex_count > 0);
    assert!(!package.room_cache_cell_vertices.is_empty());
    let room_record = &package.rooms[cache.room as usize];
    let room_asset = &package.assets[room_record.world_asset_index];
    let room = RuntimeRoom::from_bytes(&room_asset.bytes).expect("room parses");
    let materials =
        cache_materials_for_room(cache.room, &package.materials, &package.assets).unwrap();
    let mut cells = vec![CachedRoomCell::EMPTY; cache.cell_count as usize];
    let mut vertices = vec![WorldVertex::ZERO; cache.vertex_count as usize];
    let mut surfaces = vec![CachedRoomSurface::EMPTY; cache.surface_count as usize];
    let stats = cache_room_vertex_lit_surfaces(
        room.render(),
        &materials,
        &mut cells,
        &mut vertices,
        &mut surfaces,
    );
    assert!(!stats.overflow);
    assert_eq!(stats.cell_count, cache.cell_count as usize);
    assert_eq!(stats.vertex_count, cache.vertex_count as usize);
    assert_eq!(stats.surface_count, cache.surface_count as usize);
    assert_eq!(
        package.room_cache_cells[cache.cell_first as usize],
        playtest_cached_room_cell(
            cells[0],
            package.room_cache_cells[cache.cell_first as usize].vertex_first,
            package.room_cache_cells[cache.cell_first as usize].vertex_count,
        )
    );
    assert_eq!(
        package.room_cache_vertices[cache.vertex_first as usize],
        playtest_cached_room_vertex(vertices[0])
    );
    assert_eq!(
        package.room_cache_surfaces[cache.surface_first as usize],
        playtest_cached_room_surface(surfaces[0])
    );
}

#[test]
fn package_resolves_vertical_floor_links_to_runtime_rooms() {
    let mut project = project_with_one_room();
    let room_material = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .map(|resource| resource.id);
    let scene = project.active_scene_mut();
    let world_id = scene.root;
    let source_id = scene
        .nodes()
        .iter()
        .find(|node| node.name == "Room" && matches!(node.kind, NodeKind::Section { .. }))
        .map(|node| node.id)
        .expect("source room");
    let target_grid = WorldGrid::stone_room(
        1,
        1,
        crate::DEFAULT_WORLD_SECTOR_SIZE,
        room_material,
        room_material,
    );
    let target_id = scene.add_node(world_id, "Below", NodeKind::Section { grid: target_grid });
    let source = scene.node_mut(source_id).expect("source node");
    let NodeKind::Section { grid } = &mut source.kind else {
        panic!("source should be room");
    };
    grid.set_floor_below(0, 0, Some(crate::GridFloorLink::room(target_id)));

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "{report:?}");
    let package = package.expect("package");
    assert_eq!(package.room_floor_links.len(), 1);
    assert_eq!(package.room_floor_links[0].room, 0);
    assert_eq!(package.room_floor_links[0].x, 0);
    assert_eq!(package.room_floor_links[0].z, 0);
    assert_eq!(package.room_floor_links[0].above_room, None);
    assert_eq!(package.room_floor_links[0].below_room, Some(1));
}

#[test]
fn floors_cook_to_stacked_rooms_with_auto_links() {
    let mut project = project_with_one_room();
    let room_material = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .map(|resource| resource.id);
    let baseline = build_package(&project, &starter_project_root())
        .0
        .expect("baseline package")
        .rooms
        .len();

    // Add a populated floor above the base, kept at its auto-stacked
    // elevation, fully overlapping the base footprint.
    {
        let scene = project.active_scene_mut();
        let room_id = scene
            .nodes()
            .iter()
            .find(|node| matches!(node.kind, NodeKind::Section { .. }))
            .map(|node| node.id)
            .expect("room node");
        let node = scene.node_mut(room_id).expect("room node");
        let NodeKind::Section { grid } = &mut node.kind else {
            panic!("expected a room");
        };
        let (w, d, s, origin) = (grid.width, grid.depth, grid.sector_size, grid.origin);
        // Punch a hole at a cell shared by both floors so the
        // hole-gated portal generator emits a vertical portal there:
        // floor 0 must have no ceiling and floor 1 no floor at (0,0).
        if let Some(sector) = grid.sector_mut(0, 0) {
            sector.ceiling = None;
        }
        grid.push_floor();
        let floor1 = grid.floor_mut(1).expect("floor 1");
        let elevation = floor1.elevation;
        *floor1 = WorldGrid::stone_room(w, d, s, room_material, room_material);
        floor1.origin = origin;
        floor1.elevation = elevation;
        // Open the floor-1 floor at the hole cell.
        if let Some(sector) = floor1.sector_mut(0, 0) {
            sector.floor = None;
        }
    }

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "{report:?}");
    let package = package.expect("package");
    // Floor 1 cooks as its own room chunk(s) on top of the base.
    assert!(
        package.rooms.len() > baseline,
        "the upper floor should add cooked rooms ({} vs {baseline})",
        package.rooms.len()
    );
    // At least one cooked room sits above Y=0 (the stacked floor).
    assert!(
        package.rooms.iter().any(|room| room.origin_y > 0),
        "an upper floor should cook at a stacked origin_y"
    );
    // Every X/Z-overlapping floor room references its counterpart even when
    // the vertical boundary is sealed. This keeps the lower geometry drawable
    // behind translucent upper surfaces.
    let lower = package
        .rooms
        .iter()
        .position(|room| room.origin_y == 0)
        .expect("base floor room");
    let upper = package
        .rooms
        .iter()
        .position(|room| room.origin_y > 0)
        .expect("upper floor room");
    let overlap_slice = |room_index: usize| {
        let room = &package.rooms[room_index];
        let first = room.overlapped_room_first as usize;
        let end = first + room.overlapped_room_count as usize;
        &package.room_overlapped_rooms[first..end]
    };
    assert!(overlap_slice(lower).contains(&(upper as u16)));
    assert!(overlap_slice(upper).contains(&(lower as u16)));
    // The floors are auto-wired with vertical room links.
    assert!(
        package
            .room_floor_links
            .iter()
            .any(|link| link.above_room.is_some() || link.below_room.is_some()),
        "consecutive floors should be auto-linked"
    );
    // ...and with vertical portal quads (kind=1) so the portal
    // clipper / portal view have geometry between the floors. Portals
    // are emitted only at actual holes (floor-1 floor open AND floor-0
    // ceiling open); we punched one at (0,0), so expect a reciprocal
    // up/down pair there with ±Y normals.
    let vertical: Vec<_> = package
        .room_portals
        .iter()
        .filter(|p| p.kind == 1)
        .collect();
    assert!(
        !vertical.is_empty(),
        "stacked floors should emit vertical portal quads"
    );
    assert!(
        vertical.iter().any(|p| p.normal == [0, 1, 0])
            && vertical.iter().any(|p| p.normal == [0, -1, 0]),
        "vertical portals should be reciprocal (both +Y and -Y normals): {vertical:?}"
    );
    // Each vertical portal is planar in Y (a horizontal quad at the
    // boundary elevation).
    for p in &vertical {
        let y = p.vertices[0][1];
        assert!(
            p.vertices.iter().all(|v| v[1] == y),
            "a vertical portal quad must be planar in Y: {:?}",
            p.vertices
        );
    }
}

#[test]
fn adjacent_floor_layers_cook_as_a_traversable_terrace_seam() {
    let mut project = project_with_one_room();
    let material = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .map(|resource| resource.id);

    let room_id = project
        .active_scene()
        .nodes()
        .iter()
        .find(|node| matches!(node.kind, NodeKind::Section { .. }))
        .map(|node| node.id)
        .expect("room node");
    let node = project
        .active_scene_mut()
        .node_mut(room_id)
        .expect("room node");
    node.transform.translation[1] = 0.0;
    let NodeKind::Section { grid } = &mut node.kind else {
        panic!("expected a room");
    };
    let sector_size = grid.sector_size;
    let mut lower = WorldGrid::empty(2, 1, sector_size);
    lower.set_floor(0, 0, 0, material);
    // This is a solid collision wall, but it ends exactly at the upper
    // walking surface. It is a low riser, not a room-sealing wall.
    lower
        .ensure_sector(0, 0)
        .expect("lower sector")
        .walls
        .east
        .push(GridVerticalFace::flat(0, 320, material));
    let mut upper = WorldGrid::empty(2, 1, sector_size);
    upper.elevation = 320;
    upper.set_floor(1, 0, 0, material);
    lower.floors_above.push(upper);
    *grid = lower;

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "{report:?}");
    let package = package.expect("package");
    assert_eq!(package.rooms.len(), 2, "one room per authored layer");
    assert!(
        package.room_floor_links.is_empty(),
        "the layers do not overlap"
    );

    let lateral: Vec<_> = package
        .room_portals
        .iter()
        .filter(|portal| portal.kind == 0)
        .collect();
    assert_eq!(lateral.len(), 2, "the terrace seam must be reciprocal");
    assert!(
        lateral.iter().any(|portal| portal.normal == [-1, 0, 0])
            && lateral.iter().any(|portal| portal.normal == [1, 0, 0]),
        "both sides of the seam must reach the other room: {lateral:?}"
    );
    for portal in lateral {
        assert_eq!(portal.vertices[0][0], sector_size);
        assert_eq!(portal.vertices[0][1], 320);
        assert!(portal.vertices[2][1] > portal.vertices[0][1]);
    }
    assert!(
        package.rooms.iter().all(|room| room.portal_count == 1),
        "each runtime room must expose its side of the terrace portal"
    );
}

#[test]
fn layered_pvs_can_leave_a_room_and_reenter_behind_a_shallow_recess() {
    let mut project = project_with_one_room();
    let material = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .map(|resource| resource.id);
    let room_id = project
        .active_scene()
        .nodes()
        .iter()
        .find(|node| matches!(node.kind, NodeKind::Section { .. }))
        .map(|node| node.id)
        .expect("room node");
    let node = project.active_scene_mut().node_mut(room_id).expect("room");
    node.transform.translation[1] = 0.0;
    let NodeKind::Section { grid } = &mut node.kind else {
        panic!("expected room grid");
    };
    let sector_size = grid.sector_size;
    let mut lower = WorldGrid::empty(3, 1, sector_size);
    lower.set_floor(1, 0, 0, material);
    let mut upper = WorldGrid::empty(3, 1, sector_size);
    upper.elevation = 320;
    upper.set_floor(0, 0, 0, material);
    upper.set_floor(2, 0, 0, material);
    lower.floors_above.push(upper);
    *grid = lower;

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "{report:?}");
    let package = package.expect("package");
    let upper_room = package
        .rooms
        .iter()
        .position(|room| room.origin_y == 320)
        .expect("upper runtime room") as u16;
    let visibility = package
        .room_visibility
        .iter()
        .find(|visibility| visibility.room == upper_room)
        .expect("upper room visibility");
    assert_eq!(visibility.cell_count, 2);
    let pvs = package.visibility_pvs[visibility.pvs_first as usize];
    let bits = &package.visibility_pvs_bits
        [pvs.byte_first as usize..pvs.byte_first as usize + pvs.byte_count as usize];
    assert_ne!(
        bits[0] & 0b10,
        0,
        "the far upper cell must remain visible through upper -> recess -> upper"
    );

    let debug = build_debug_topology(&project);
    assert_eq!(
        debug.cells.len(),
        package.visibility_cells.len(),
        "the editor diagnostic must use the same layered cells as the cook"
    );
    assert_eq!(
        debug.portals.len(),
        package.room_portals.len(),
        "the editor diagnostic must include generated terrace portals"
    );
    assert!(debug.cells.iter().any(|cell| cell.floor_index == 1));
    assert!(debug
        .portals
        .iter()
        .any(|portal| portal.portal.source_room != portal.portal.destination_room));
}

#[test]
fn entities_bind_to_their_explicit_floor() {
    // Two-floor room; a spawn marker on each floor, distinguished by
    // the explicit `SceneNode::floor` field (NOT by Y -- the authored
    // standing height is a placement default and can't select a
    // floor). The cook must bind each marker to the runtime room for
    // its own floor (distinct origin_y).
    let mut project = project_with_one_room();
    let room_material = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .map(|resource| resource.id);

    let room_id = {
        let scene = project.active_scene_mut();
        let room_id = scene
            .nodes()
            .iter()
            .find(|node| matches!(node.kind, NodeKind::Section { .. }))
            .map(|node| node.id)
            .expect("room node");
        let node = scene.node_mut(room_id).expect("room node");
        let NodeKind::Section { grid } = &mut node.kind else {
            panic!("expected a room");
        };
        let (w, d, s, origin) = (grid.width, grid.depth, grid.sector_size, grid.origin);
        grid.push_floor();
        let floor1 = grid.floor_mut(1).expect("floor 1");
        let elevation = floor1.elevation;
        *floor1 = WorldGrid::stone_room(w, d, s, room_material, room_material);
        floor1.origin = origin;
        floor1.elevation = elevation;
        room_id
    };

    // Two markers at the SAME transform; only the explicit floor
    // differs. This proves binding is by `floor`, not Y.
    let scene = project.active_scene_mut();
    let ground = scene.add_node(
        room_id,
        "Ground Marker",
        NodeKind::SpawnPoint {
            player: false,
            character: None,
        },
    );
    let ground_node = scene.node_mut(ground).unwrap();
    ground_node.transform.translation = [0.0, 0.0, 0.0];
    ground_node.floor = 0;
    let upper = scene.add_node(
        room_id,
        "Upper Marker",
        NodeKind::SpawnPoint {
            player: false,
            character: None,
        },
    );
    let upper_node = scene.node_mut(upper).unwrap();
    upper_node.transform.translation = [0.0, 0.0, 0.0];
    upper_node.floor = 1;

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "{report:?}");
    let package = package.expect("package");

    // Both markers cooked; each binds to a room whose origin_y matches
    // its floor. The base marker -> origin_y 0; the upper -> origin_y > 0.
    let origin_y_of = |room_index: u16| package.rooms[room_index as usize].origin_y;
    let marker_origin_ys: Vec<i32> = package
        .entities
        .iter()
        .filter(|e| matches!(e.kind, PlaytestEntityKind::Marker))
        .map(|e| origin_y_of(e.room))
        .collect();
    assert!(
        marker_origin_ys.contains(&0),
        "the ground marker should bind to floor 0 (origin_y 0): {marker_origin_ys:?}"
    );
    assert!(
        marker_origin_ys.iter().any(|y| *y > 0),
        "the upper marker should bind to the stacked floor (origin_y > 0): {marker_origin_ys:?}"
    );
}

#[test]
fn room_vertical_placement_flows_from_transform_into_origin_y() {
    // Ground placement: a room left at the default transform Y
    // cooks to origin_y == 0, preserving today's behaviour.
    let project = project_with_one_room();
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "{report:?}");
    let package = package.expect("package");
    assert_eq!(package.rooms[0].origin_y, 0);

    // Raised placement: authoring translation[1] = 2 sectors must
    // cook to engine units (2 * sector_size). The cook reads the
    // Room node transform, so the authored Y reaches the record
    // even though the per-chunk grid does not carry elevation yet.
    let mut project = project_with_one_room();
    let scene = project.active_scene_mut();
    let room_id = scene
        .nodes()
        .iter()
        .find(|node| matches!(node.kind, NodeKind::Section { .. }))
        .map(|node| node.id)
        .expect("room node");
    scene.node_mut(room_id).expect("room").transform.translation[1] = 2.0;
    let (raised, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "{report:?}");
    let raised = raised.expect("package");
    // 2 authored sectors converted to engine units. Derived from
    // the room's own sector_size so the test holds whatever the
    // starter project uses.
    assert_eq!(raised.rooms[0].origin_y, 2 * raised.rooms[0].sector_size);
    assert!(raised.rooms[0].sector_size > 0);
}

#[test]
fn visibility_pvs_adds_one_cell_boundary_shell() {
    let width = 1;
    let radius = DEFAULT_PLAYTEST_VISIBILITY_CELL_RADIUS;
    let depth = radius + 6;
    let mut cells: Vec<PlaytestVisibilityCell> =
        (0..depth).map(|z| visibility_test_cell(0, z, 0)).collect();
    let index_by_coord = visibility_index_by_coord(width, depth, &cells);
    assign_visibility_portals(width, depth, &index_by_coord, &mut cells);

    let visible = visibility_indices_for_anchor(0, width, depth, &cells, &index_by_coord, radius);

    assert_eq!(visible.len(), radius as usize + 2);
    assert!(visible.contains(&0));
    assert!(visible.contains(&(radius as usize)));
    assert!(visible.contains(&(radius as usize + 1)));
    assert!(!visible.contains(&(radius as usize + 2)));
}

#[test]
fn visibility_pvs_keeps_blocked_boundary_shell_without_traversing() {
    let width = 2;
    let depth = 1;
    let mut cells = vec![
        visibility_test_cell(0, 0, visibility_edge_flags::EAST),
        visibility_test_cell(1, 0, visibility_edge_flags::WEST),
    ];
    let index_by_coord = visibility_index_by_coord(width, depth, &cells);
    assign_visibility_portals(width, depth, &index_by_coord, &mut cells);

    let visible = visibility_indices_for_anchor(
        0,
        width,
        depth,
        &cells,
        &index_by_coord,
        DEFAULT_PLAYTEST_VISIBILITY_CELL_RADIUS,
    );

    assert_eq!(visible, vec![1, 0]);
}

#[test]
fn visibility_pvs_reuses_identical_bitsets() {
    let width = 2;
    let depth = 1;
    let mut cells = vec![visibility_test_cell(0, 0, 0), visibility_test_cell(1, 0, 0)];
    let index_by_coord = visibility_index_by_coord(width, depth, &cells);
    assign_visibility_portals(width, depth, &index_by_coord, &mut cells);
    let mut pvs = Vec::new();
    let mut bits = Vec::new();

    append_visibility_pvs(
        width,
        depth,
        &cells,
        &index_by_coord,
        DEFAULT_PLAYTEST_VISIBILITY_CELL_RADIUS,
        &mut pvs,
        &mut bits,
    );

    assert_eq!(pvs.len(), 2);
    assert_eq!(bits.len(), 1);
    assert_eq!(pvs[0].byte_first, pvs[1].byte_first);
    assert_eq!(pvs[0].byte_count, 1);
    assert_eq!(bits[0], 0b0000_0011);
}

#[test]
fn oversized_authored_room_fails_without_manual_split() {
    let mut project = project_with_one_room();
    let floor_material = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .expect("starter has a room material")
        .id;
    let room_id = {
        let scene = project.active_scene();
        scene
            .nodes()
            .iter()
            .find(|n| matches!(n.kind, NodeKind::Section { .. }))
            .expect("starter has a room")
            .id
    };
    if let Some(room) = project.active_scene_mut().node_mut(room_id) {
        let NodeKind::Section { grid } = &mut room.kind else {
            panic!("starter room is a room");
        };
        *grid = crate::WorldGrid::empty(
            1,
            crate::MAX_ROOM_DEPTH + 8,
            crate::DEFAULT_WORLD_SECTOR_SIZE,
        );
        for z in 0..grid.depth {
            grid.set_floor(0, z, 0, Some(floor_material));
        }
    }
    let spawn_id = player_spawn_node_id(&project);
    if let Some(spawn) = project.active_scene_mut().node_mut(spawn_id) {
        spawn.transform.translation = [0.0, 0.0, 0.0];
    }

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(package.is_none());
    assert!(!report.is_ok());
    assert!(
        report
            .errors
            .iter()
            .any(|error| error.contains("runtime cap")),
        "errors: {:?}",
        report.errors
    );
}

#[test]
fn portal_room_cook_emits_directed_room_portals() {
    let mut project = project_with_one_room();
    let floor_material = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .expect("starter has a room material")
        .id;
    let room_id = project
        .active_scene()
        .nodes()
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Section { .. }))
        .expect("starter has a room")
        .id;
    if let Some(room) = project.active_scene_mut().node_mut(room_id) {
        let NodeKind::Section { grid } = &mut room.kind else {
            panic!("starter room is a room");
        };
        *grid = crate::WorldGrid::stone_room(
            2,
            1,
            crate::DEFAULT_WORLD_SECTOR_SIZE,
            Some(floor_material),
            Some(floor_material),
        );
    }
    let portal_id = project.active_scene_mut().add_node(
        room_id,
        "Portal",
        NodeKind::Portal {
            target_room: None,
            target_entry: String::new(),
            entry_name: String::new(),
            geometry: None,
        },
    );
    if let Some(portal) = project.active_scene_mut().node_mut(portal_id) {
        portal.transform.translation = [0.0, 0.0, 0.0];
    }
    let spawn_id = player_spawn_node_id(&project);
    if let Some(spawn) = project.active_scene_mut().node_mut(spawn_id) {
        spawn.transform.translation = [-0.25, 0.0, 0.0];
    }

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("package returned on ok report");
    assert_eq!(package.rooms.len(), 2);
    assert_eq!(package.room_portals.len(), 2);
    assert_eq!(package.rooms[0].portal_first, 0);
    assert_eq!(package.rooms[0].portal_count, 1);
    assert_eq!(package.rooms[1].portal_first, 1);
    assert_eq!(package.rooms[1].portal_count, 1);
    assert_eq!(package.room_portals[0].source_room, 0);
    assert_eq!(package.room_portals[0].destination_room, 1);
    assert_eq!(package.room_portals[0].normal, [-1, 0, 0]);
    let src = render_manifest_source(&package);
    assert!(src.contains(
        "pub static ROOM_PORTALS: &[LevelRoomPortalRecord] = &[\n    LevelRoomPortalRecord"
    ));
}

#[test]
fn manual_portal_rooms_emit_warm_residency_hints() {
    let mut project = project_with_one_room();
    let floor_material = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .expect("starter has a room material")
        .id;
    let material = project
        .resource_mut(floor_material)
        .expect("starter room material remains addressable");
    let ResourceData::Material(material) = &mut material.data else {
        panic!("starter room material is a material");
    };
    material.texture_mode = crate::MaterialTextureMode::ReflectiveProbe;
    let room_id = {
        let scene = project.active_scene();
        scene
            .nodes()
            .iter()
            .find(|n| matches!(n.kind, NodeKind::Section { .. }))
            .expect("starter has a room")
            .id
    };
    if let Some(room) = project.active_scene_mut().node_mut(room_id) {
        let NodeKind::Section { grid } = &mut room.kind else {
            panic!("starter room is a room");
        };
        *grid = crate::WorldGrid::stone_room(
            2,
            1,
            crate::DEFAULT_WORLD_SECTOR_SIZE,
            Some(floor_material),
            Some(floor_material),
        );
    }
    let portal_id = project.active_scene_mut().add_node(
        room_id,
        "Portal",
        NodeKind::Portal {
            target_room: None,
            target_entry: String::new(),
            entry_name: String::new(),
            geometry: None,
        },
    );
    if let Some(portal) = project.active_scene_mut().node_mut(portal_id) {
        portal.transform.translation = [0.0, 0.0, 0.0];
    }
    let spawn_id = player_spawn_node_id(&project);
    if let Some(spawn) = project.active_scene_mut().node_mut(spawn_id) {
        spawn.transform.translation = [-0.25, 0.0, 0.0];
    }

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("cooks");
    let room_0_probe = package.rooms[0]
        .reflection_probe_asset_index
        .expect("room 0 probe is cooked");
    let room_1_probe = package.rooms[1]
        .reflection_probe_asset_index
        .expect("room 1 probe is cooked");
    assert_ne!(room_0_probe, room_1_probe);
    let src = render_manifest_source(&package);

    let warm_ram_line = src
        .lines()
        .find(|line| line.contains("pub static ROOM_0_WARM_RAM"))
        .expect("room 0 warm RAM static emitted");
    assert!(
        warm_ram_line.contains("AssetId("),
        "room 0 should warm at least one neighbouring room asset: {warm_ram_line}"
    );
    assert!(src.contains("warm_ram: ROOM_0_WARM_RAM"));
    assert!(src.contains("warm_vram: ROOM_0_WARM_VRAM"));
    let room_0_required_vram = src
        .lines()
        .find(|line| line.contains("pub static ROOM_0_REQUIRED_VRAM"))
        .expect("room 0 required VRAM static emitted");
    assert!(room_0_required_vram.contains(&format!("AssetId({room_0_probe})")));
    let room_0_warm_vram = src
        .lines()
        .find(|line| line.contains("pub static ROOM_0_WARM_VRAM"))
        .expect("room 0 warm VRAM static emitted");
    assert!(room_0_warm_vram.contains(&format!("AssetId({room_1_probe})")));
    let room_1_warm_vram = src
        .lines()
        .find(|line| line.contains("pub static ROOM_1_WARM_VRAM"))
        .expect("room 1 warm VRAM static emitted");
    assert!(room_1_warm_vram.contains(&format!("AssetId({room_0_probe})")));
}

#[test]
fn reflective_second_layer_cooks_room_probe_residency() {
    let mut project = project_with_one_room();
    let material = project
        .resources
        .iter_mut()
        .find_map(|resource| match &mut resource.data {
            ResourceData::Material(material) => Some(material),
            _ => None,
        })
        .expect("starter has a material");
    let layer = crate::ModelSecondaryLayer {
        texture_mode: crate::MaterialTextureMode::ReflectiveProbe,
        ..Default::default()
    };
    material.secondary_layer = Some(layer);

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    assert!(
        package.expect("cooks").rooms[0]
            .reflection_probe_asset_index
            .is_some(),
        "layer 2 probe must request the same streamed room probe as layer 1"
    );
}
