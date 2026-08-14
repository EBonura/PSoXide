use super::*;

use psx_bsp::collision::TraceScratch;
use psx_bsp::collision_provider::{select_body_hull, PxbspCollisionModel, PxbspCollisionProvider};
use psx_bsp::mover::BrushDoorSet;
use psx_bsp::pxbsp_resident::PxbspResidentMap;
use psx_bsp::SliceReader;
use psx_engine::{
    commit_body_step_with_trace_provider, trace_collision, CollisionTraceQuery,
    CollisionTraceShape, RoomPoint,
};

const LOWER_FLOOR: i32 = 256;
const COURT_FLOOR: i32 = 2048;
const RELAY_FLOOR: i32 = 1536;
const PLAYER_RADIUS: i32 = 188;
const PLAYER_HEIGHT: i32 = 1024;
const ROUTE_STEP: i32 = 64;

fn collision_models(doors: &BrushDoorSet<3>) -> Vec<PxbspCollisionModel> {
    doors
        .iter()
        .map(|door| {
            PxbspCollisionModel::new(
                u16::try_from(door.model_index()).expect("PXBSP door model index fits u16"),
                door.transform(),
            )
        })
        .collect()
}

fn body_trace(
    map: &PxbspResidentMap,
    hull_index: usize,
    doors: &BrushDoorSet<3>,
    start: RoomPoint,
    end: RoomPoint,
) -> psx_engine::CollisionTrace {
    let models = collision_models(doors);
    let mut scratch = TraceScratch::new();
    let mut provider = PxbspCollisionProvider::new(
        map,
        hull_index,
        &models,
        CollisionTraceShape::Body {
            radius: PLAYER_RADIUS,
            height: PLAYER_HEIGHT,
        },
        &mut scratch,
    )
    .expect("resident Ashen Sanctum player-body provider");
    trace_collision(
        &mut provider,
        CollisionTraceQuery::body(start, end, PLAYER_RADIUS, PLAYER_HEIGHT),
    )
    .expect("Ashen Sanctum body trace")
}

fn body_step(
    map: &PxbspResidentMap,
    hull_index: usize,
    doors: &BrushDoorSet<3>,
    start: RoomPoint,
    dx: i32,
    dz: i32,
) -> psx_engine::BodyStep {
    let models = collision_models(doors);
    let mut scratch = TraceScratch::new();
    let mut provider = PxbspCollisionProvider::new(
        map,
        hull_index,
        &models,
        CollisionTraceShape::Body {
            radius: PLAYER_RADIUS,
            height: PLAYER_HEIGHT,
        },
        &mut scratch,
    )
    .expect("resident Ashen Sanctum player-body provider");
    commit_body_step_with_trace_provider(&mut provider, start, dx, dz, PLAYER_RADIUS, PLAYER_HEIGHT)
        .expect("Ashen Sanctum body step")
}

fn walk_axis_aligned(
    map: &PxbspResidentMap,
    hull_index: usize,
    doors: &BrushDoorSet<3>,
    label: &str,
    mut position: RoomPoint,
    target_x: i32,
    target_z: i32,
) -> RoomPoint {
    assert!(
        position.x == target_x || position.z == target_z,
        "{label}: route segment must be axis aligned: {position:?} -> ({target_x}, {target_z})"
    );
    for _ in 0..512 {
        let dx = (target_x - position.x).clamp(-ROUTE_STEP, ROUTE_STEP);
        let dz = (target_z - position.z).clamp(-ROUTE_STEP, ROUTE_STEP);
        if dx == 0 && dz == 0 {
            return position;
        }
        let step = body_step(map, hull_index, doors, position, dx, dz);
        if !step.moved {
            let target = RoomPoint::new(position.x + dx, position.y, position.z + dz);
            let direct = body_trace(map, hull_index, doors, position, target);
            let raised = position.with_y(position.y + 640);
            let lift = body_trace(map, hull_index, doors, position, raised);
            let raised_across = body_trace(map, hull_index, doors, raised, target.with_y(raised.y));
            panic!(
                "{label}: cooked player hull stopped at {position:?} before ({target_x}, {target_z}); step={step:?}; direct={direct:?}; lift={lift:?}; raised_across={raised_across:?}"
            );
        }
        let before = (target_x - position.x).abs() + (target_z - position.z).abs();
        let after = (target_x - step.position.x).abs() + (target_z - step.position.z).abs();
        assert!(
            after < before,
            "{label}: step failed to advance: {position:?} -> {:?}",
            step.position
        );
        position = step.position;
    }
    panic!("{label}: exceeded bounded route steps at {position:?}");
}

fn open_door(doors: &mut BrushDoorSet<3>, index: usize, label: &str) {
    let door = doors
        .get_mut(index)
        .unwrap_or_else(|| panic!("missing {label}"));
    door.set_open(true);
    for _ in 0..64 {
        door.tick();
    }
    assert!(door.fully_open(), "{label} did not reach its open endpoint");
}

fn assert_closed_then_open_door(
    map: &PxbspResidentMap,
    hull_index: usize,
    doors: &mut BrushDoorSet<3>,
    index: usize,
    label: &str,
    start: RoomPoint,
    end: RoomPoint,
) {
    let closed = body_trace(map, hull_index, doors, start, end);
    assert!(
        closed.hit(),
        "{label}: closed mover did not block {start:?} -> {end:?}"
    );
    open_door(doors, index, label);
    let open = body_trace(map, hull_index, doors, start, end);
    assert!(
        !open.hit(),
        "{label}: fully open mover did not clear {start:?} -> {end:?}: {open:?}"
    );
}

/// Drive the same expanded player hull the runtime selects through every
/// mandatory Ashen Sanctum route beat. This deliberately operates on the
/// packed resident PXBSP and translated mover hulls, after the editor cook;
/// authored boxes or coordinate adjacency alone cannot satisfy it.
fn prove_cooked_player_hull_route(world: &psxed_project::playtest::PlaytestPxbspWorld) {
    let mut map = PxbspResidentMap::with_capacity(world.bytes.len());
    map.load(0, &mut SliceReader::new(&world.bytes))
        .expect("load cooked Ashen Sanctum PXBSP");
    let hull_index = select_body_hull(&world.body_hulls, PLAYER_RADIUS, PLAYER_HEIGHT)
        .expect("cooked player body hull");
    assert_eq!(
        hull_index, 1,
        "Aletha should use the tight cooked body hull"
    );

    let mut doors = BrushDoorSet::<3>::default();
    doors
        .init_from_map(&map)
        .expect("three Ashen Sanctum doors");
    assert_eq!(doors.len(), 3);
    let cooked_models = doors
        .iter()
        .map(|door| u16::try_from(door.model_index()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        cooked_models,
        world
            .movers
            .iter()
            .map(|mover| mover.model_index)
            .collect::<Vec<_>>(),
        "door runtime order must match the cooked mover contract"
    );

    let mut position = RoomPoint::new(3840, LOWER_FLOOR + 1, 21376);
    let spawn = body_trace(&map, hull_index, &doors, position, position);
    assert!(
        !spawn.start_solid && !spawn.all_solid,
        "Aletha starts inside cooked collision: {spawn:?}"
    );

    // Cell, intake, flooded junction and the entire four-riser ascent.
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "cell into intake",
        position,
        3840,
        13440,
    );
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "flooded junction traverse",
        position,
        5248,
        13440,
    );
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "four-riser ascent",
        position,
        5248,
        9280,
    );
    assert_eq!(position.y, COURT_FLOOR, "lower ascent reaches court height");
    // Visit the first checkpoint landmark, then prove the first gate blocks
    // in its closed state and clears at its translated open endpoint.
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "leave lower stair landing toward relay",
        position,
        3584,
        9280,
    );
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "arrival court approach",
        position,
        3584,
        10496,
    );
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "Courtyard Relay",
        position,
        3584,
        10496,
    );
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "return to the stair landing",
        position,
        3584,
        9280,
    );
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "cross the upper stair landing",
        position,
        7800,
        9280,
    );
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "First Warden Gate approach",
        position,
        7800,
        10496,
    );
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "First Warden Gate centreline",
        position,
        7800,
        10880,
    );
    assert_closed_then_open_door(
        &map,
        hull_index,
        &mut doors,
        0,
        "First Warden Gate",
        RoomPoint::new(7800, COURT_FLOOR + 1, 10880),
        RoomPoint::new(8700, COURT_FLOOR + 1, 10880),
    );
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "enter Warden Court",
        position,
        9000,
        10880,
    );

    // The side opening drops 512 units, its split wall admits the relay hall,
    // and the optional shortcut must itself block/clear before the motor
    // climbs the 512-unit return into the arrival court and walks back down.
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "arena side opening",
        position,
        11200,
        10880,
    );
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "side escape drop",
        position,
        11200,
        7040,
    );
    assert_eq!(position.y, RELAY_FLOOR, "side escape reaches relay height");
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "side escape wall aperture",
        position,
        8192,
        7040,
    );
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "Sanctum Relay",
        position,
        8192,
        6912,
    );
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "shortcut approach",
        position,
        7168,
        6912,
    );
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "shortcut threshold",
        position,
        7168,
        8400,
    );
    assert_closed_then_open_door(
        &map,
        hull_index,
        &mut doors,
        1,
        "Sanctum Shortcut",
        RoomPoint::new(7168, COURT_FLOOR + 1, 8400),
        RoomPoint::new(7168, COURT_FLOOR + 1, 9100),
    );
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "shortcut climb into arrival court",
        position,
        7168,
        9200,
    );
    assert_eq!(position.y, COURT_FLOOR, "shortcut climbs to arrival court");
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "shortcut return to relay hall",
        position,
        7168,
        8400,
    );
    assert_eq!(position.y, RELAY_FLOOR, "shortcut returns to relay height");

    // Equipment hall, all five upper risers, the aligned rail opening and the
    // four supported 512-unit drops back into the same physical arena.
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "return from shortcut",
        position,
        7168,
        6912,
    );
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "relay hall centreline",
        position,
        8192,
        6912,
    );
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "equipment hall",
        position,
        16500,
        6912,
    );
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "five-riser ascent",
        position,
        19000,
        6912,
    );
    assert_eq!(position.y, 4096, "upper ascent reaches rampart height");
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "upper rail opening",
        position,
        19000,
        7600,
    );
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "upper rampart",
        position,
        12672,
        7600,
    );
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "four descending ledges",
        position,
        12672,
        11000,
    );
    assert_eq!(
        position.y, COURT_FLOOR,
        "upper route returns to same arena floor"
    );

    // The far gate has the same closed/open collision contract, followed by
    // four 256-unit cliff stages to the final authored vista.
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "far gate centreline",
        position,
        12672,
        11904,
    );
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "Far Warden Gate approach",
        position,
        14800,
        11904,
    );
    assert_closed_then_open_door(
        &map,
        hull_index,
        &mut doors,
        2,
        "Far Warden Gate",
        RoomPoint::new(14800, COURT_FLOOR + 1, 11904),
        RoomPoint::new(15800, COURT_FLOOR + 1, 11904),
    );
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "leave Warden Court",
        position,
        16000,
        11904,
    );
    position = walk_axis_aligned(
        &map,
        hull_index,
        &doors,
        "rising cliff vista",
        position,
        21000,
        11904,
    );
    assert_eq!(position.y, 2816, "cliff route reaches its fourth stage");
}

fn resource_id(
    workspace: &EditorWorkspace,
    name: &str,
    wants: fn(&ResourceData) -> bool,
) -> ResourceId {
    workspace
        .project()
        .resources
        .iter()
        .find(|resource| resource.name == name && wants(&resource.data))
        .unwrap_or_else(|| panic!("missing resource '{name}'"))
        .id
}

fn add_texture_material(
    workspace: &mut EditorWorkspace,
    name: &str,
    source_name: &str,
    target_name: &str,
) -> ResourceId {
    let source = psxed_project::legacy_grid_starter_dir()
        .join("assets/textures")
        .join(source_name);
    assert!(
        source.is_file(),
        "missing texture donor {}",
        source.display()
    );
    let relative = format!("assets/textures/{target_name}");
    let target = workspace.project_dir.join(&relative);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::copy(&source, &target).unwrap();
    workspace.project.add_resource(
        name,
        ResourceData::Material(MaterialResource::opaque(Some(relative))),
    )
}

fn author_box(
    workspace: &mut EditorWorkspace,
    material: ResourceId,
    mins: [i32; 3],
    maxs: [i32; 3],
) -> usize {
    assert!(
        mins.iter().zip(maxs).all(|(min, max)| *min < max),
        "invalid brush {mins:?} -> {maxs:?}"
    );
    workspace.brush_material = Some(material);
    workspace.orthographic_focus[1] = mins[1] as f32;
    workspace.begin_brush_drag_2d([mins[0] as f32, mins[2] as f32]);
    workspace.update_brush_drag_2d([maxs[0] as f32, maxs[2] as f32]);
    workspace.commit_brush_drag();
    let created = workspace.selected_brush.expect("committed brush");
    assert!(workspace.set_selected_brush_size([
        maxs[0] - mins[0],
        maxs[1] - mins[1],
        maxs[2] - mins[2],
    ]));
    created
}

fn place_character(
    workspace: &mut EditorWorkspace,
    profile: ResourceId,
    name: &str,
    position: [i32; 3],
    yaw: f32,
    visual_scale_q8: u16,
) -> NodeId {
    workspace.orthographic_focus[1] = position[1] as f32;
    workspace.set_active_tool_cycle_value((ViewTool::Place, Some(PlaceKind::Character)));
    workspace.replace_resource_selection(profile);
    assert!(workspace.place_bsp_from_top([position[0] as f32, position[2] as f32,]));
    let entity = workspace.selected_node_id();
    workspace.push_undo();
    let scene = workspace.project.active_scene_mut();
    let children = scene
        .node(entity)
        .expect("placed character")
        .children
        .clone();
    {
        let node = scene.node_mut(entity).expect("placed character");
        node.name = name.to_string();
        node.transform.rotation_degrees = [0.0, yaw, 0.0];
    }
    let renderer = children
        .iter()
        .copied()
        .find(|id| {
            matches!(
                scene.node(*id).map(|child| &child.kind),
                Some(NodeKind::ModelRenderer { .. })
            )
        })
        .expect("placed character renderer");
    if let Some(NodeKind::ModelRenderer {
        visual_scale_q8: scale,
        visual_offset,
        ..
    }) = scene.node_mut(renderer).map(|node| &mut node.kind)
    {
        *scale = visual_scale_q8;
        *visual_offset = [0, 1, 0];
    }
    workspace.mark_dirty();
    entity
}

fn attach_weapon(workspace: &mut EditorWorkspace, entity: NodeId, weapon: ResourceId) {
    workspace.replace_node_selection(entity);
    workspace.add_child(
        NodeKind::Equipment {
            weapon: None,
            character_socket: "right_hand_grip".to_string(),
            weapon_grip: "grip".to_string(),
        },
        "Equipment",
    );
    let equipment = workspace.selected_node_id();
    workspace.push_undo();
    if let Some(NodeKind::Equipment {
        weapon: weapon_slot,
        ..
    }) = workspace
        .project
        .active_scene_mut()
        .node_mut(equipment)
        .map(|node| &mut node.kind)
    {
        *weapon_slot = Some(weapon);
    }
    workspace.mark_dirty();
}

fn add_door(
    workspace: &mut EditorWorkspace,
    root: NodeId,
    name: &str,
    brush: usize,
    anchor: [i32; 3],
    open_offset: [i16; 3],
) -> NodeId {
    workspace.replace_node_selection(root);
    workspace.add_child(NodeKind::Entity, name);
    let door = workspace.selected_node_id();
    workspace.push_undo();
    if let Some(node) = workspace.project.active_scene_mut().node_mut(door) {
        node.name = name.to_string();
        node.transform.translation = [anchor[0] as f32, anchor[1] as f32, anchor[2] as f32];
        node.kind = NodeKind::Logic {
            kind: psxed_project::LogicNodeKind::Door {
                box_prop: String::new(),
                start_open: false,
                open_offset,
                travel_ticks: 45,
            },
            target: String::new(),
            killtarget: String::new(),
            master: String::new(),
            delay_ticks: 0,
            wait_ticks: 0,
            enabled: true,
        };
    }
    workspace.mark_dirty();
    workspace.replace_brush_selection(brush, None);
    workspace.set_selected_brush_mover(Some(door));
    assert_eq!(
        workspace.project().active_scene().brushes[brush].mover,
        Some(door)
    );
    door
}

fn add_checkpoint(
    workspace: &mut EditorWorkspace,
    root: NodeId,
    name: &str,
    title: &str,
    position: [i32; 3],
) -> NodeId {
    workspace.replace_node_selection(root);
    workspace.add_child(NodeKind::Entity, name);
    let entity = workspace.selected_node_id();
    workspace.push_undo();
    if let Some(node) = workspace.project.active_scene_mut().node_mut(entity) {
        node.name = name.to_string();
        node.transform.translation = [position[0] as f32, position[1] as f32, position[2] as f32];
    }
    workspace.mark_dirty();
    workspace.add_child(
        NodeKind::Interactable {
            kind: psxed_project::InteractableKind::Checkpoint {
                checkpoint_id: String::new(),
                title: title.to_string(),
                body: "Memory synchronized.".to_string(),
            },
            prompt: "SYNCHRONIZE".to_string(),
            radius: 160,
            enabled: true,
        },
        "Checkpoint",
    );
    workspace.mark_dirty();
    entity
}

fn add_checkpoint_trigger(
    workspace: &mut EditorWorkspace,
    root: NodeId,
    name: &str,
    target: &str,
    position: [i32; 3],
    size: [u16; 3],
) -> NodeId {
    workspace.replace_node_selection(root);
    workspace.add_child(NodeKind::Entity, name);
    let trigger = workspace.selected_node_id();
    workspace.push_undo();
    if let Some(node) = workspace.project.active_scene_mut().node_mut(trigger) {
        node.name = name.to_string();
        node.transform.translation = [position[0] as f32, position[1] as f32, position[2] as f32];
        node.kind = NodeKind::Logic {
            kind: psxed_project::LogicNodeKind::TriggerVolume { size },
            target: target.to_string(),
            killtarget: String::new(),
            master: String::new(),
            delay_ticks: 0,
            wait_ticks: -1,
            enabled: true,
        };
    }
    workspace.mark_dirty();
    trigger
}

fn add_multisource(
    workspace: &mut EditorWorkspace,
    root: NodeId,
    name: &str,
    required: u16,
) -> NodeId {
    workspace.replace_node_selection(root);
    workspace.add_child(NodeKind::Entity, name);
    let multisource = workspace.selected_node_id();
    workspace.push_undo();
    if let Some(node) = workspace.project.active_scene_mut().node_mut(multisource) {
        node.name = name.to_string();
        node.kind = NodeKind::Logic {
            kind: psxed_project::LogicNodeKind::Multisource { required },
            target: String::new(),
            killtarget: String::new(),
            master: String::new(),
            delay_ticks: 0,
            wait_ticks: 0,
            enabled: true,
        };
    }
    workspace.mark_dirty();
    multisource
}

/// Authors an original PSoXide BSP homage to the compact looping tutorial
/// grammar of Northern Undead Asylum. It intentionally reuses one roofless
/// arena from its ground approach and upper gallery, but copies no Dark Souls
/// geometry, assets, names, encounters, or presentation.
#[test]
fn ashen_sanctum_project_is_authored_through_production_commands() {
    let mut workspace =
        EditorWorkspace::open_directory(psxed_project::default_project_dir()).unwrap();
    let name = format!(
        "Ashen Sanctum Authoring {} {}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let project_dir = psxed_project::projects_dir().join(psxed_project::project_file_stem(&name));
    let _scratch = ScratchProjectDir::new(project_dir.clone());
    let cook_a = test_temp_dir("ashen-sanctum-cook-a");
    let cook_b = test_temp_dir("ashen-sanctum-cook-b");
    let _ = std::fs::remove_dir_all(&cook_a);
    let _ = std::fs::remove_dir_all(&cook_b);

    workspace.create_and_open_project(&name).unwrap();
    let root = workspace.project().active_scene().root;
    workspace.push_undo();
    let target_dir = workspace.project_dir.clone();
    sync_starter_character_catalogue(&mut workspace.project, &target_dir)
        .expect("starter character sync");
    workspace.mark_dirty();

    let aletha = resource_id(&workspace, "Aletha", |data| {
        matches!(data, ResourceData::Character(_))
    });
    let mantis = resource_id(&workspace, "Rust Mantis Enemy", |data| {
        matches!(data, ResourceData::Character(_))
    });
    let sword_light = resource_id(&workspace, "Sword1 Light", |data| {
        matches!(data, ResourceData::Weapon(_))
    });
    let sword_heavy = resource_id(&workspace, "Sword1 Heavy", |data| {
        matches!(data, ResourceData::Weapon(_))
    });

    let slate = add_texture_material(
        &mut workspace,
        "Sanctum Slate",
        "delven_02_slateflr1b_q2.psxt",
        "sanctum_slate.psxt",
    );
    let stone = add_texture_material(
        &mut workspace,
        "Sanctum Masonry",
        "delven_07_stonebrk4a_q0.psxt",
        "sanctum_masonry.psxt",
    );
    let trim = add_texture_material(
        &mut workspace,
        "Sanctum Trim",
        "delven_23_stonetrm1b_q2.psxt",
        "sanctum_trim.psxt",
    );
    let door_material = add_texture_material(
        &mut workspace,
        "Sanctum Gate",
        "bigdoor_1a.psxt",
        "sanctum_gate.psxt",
    );
    let metal = add_texture_material(
        &mut workspace,
        "Sanctum Metal",
        "metal_1a.psxt",
        "sanctum_metal.psxt",
    );

    // Remove the copied courtyard using the same selection/delete paths used
    // by the native scene tree and brush viewport.
    let children = workspace
        .project()
        .active_scene()
        .node(root)
        .expect("world root")
        .children
        .clone();
    for child in children {
        workspace.replace_node_selection(child);
        workspace.delete_selected();
    }
    while !workspace.project().active_scene().brushes.is_empty() {
        workspace.selected_brush = Some(0);
        workspace.delete_selected_brushes();
    }

    workspace.set_orthographic_view(OrthographicView::Top);
    workspace.set_active_tool_cycle_value((ViewTool::Brush, None));

    // Cinder Cell and the long lower intake passage.
    author_box(&mut workspace, slate, [2048, 0, 19456], [5632, 256, 22528]);
    author_box(
        &mut workspace,
        stone,
        [2048, 1792, 19456],
        [5632, 2048, 22528],
    );
    author_box(
        &mut workspace,
        stone,
        [2048, 256, 19456],
        [2176, 1792, 22528],
    );
    author_box(
        &mut workspace,
        stone,
        [5504, 256, 19456],
        [5632, 1792, 22528],
    );
    author_box(
        &mut workspace,
        stone,
        [2176, 256, 22400],
        [5504, 1792, 22528],
    );
    author_box(
        &mut workspace,
        stone,
        [2176, 256, 19456],
        [3072, 1792, 19584],
    );
    author_box(
        &mut workspace,
        stone,
        [4480, 256, 19456],
        [5504, 1792, 19584],
    );
    author_box(&mut workspace, slate, [3072, 0, 14592], [4480, 256, 19456]);
    author_box(
        &mut workspace,
        stone,
        [3072, 1792, 14592],
        [4480, 2048, 19456],
    );
    author_box(
        &mut workspace,
        stone,
        [2944, 256, 14592],
        [3072, 1792, 19456],
    );
    author_box(
        &mut workspace,
        stone,
        [4480, 256, 14592],
        [4608, 1792, 19456],
    );

    // Flooded junction. Water is a distinct non-solid authored contents
    // volume over the slate floor, not a disguised legacy-grid sector.
    author_box(&mut workspace, slate, [2176, 0, 12288], [6144, 256, 14592]);
    author_box(
        &mut workspace,
        stone,
        [2176, 2048, 12288],
        [6144, 2304, 14592],
    );
    author_box(
        &mut workspace,
        stone,
        [2048, 256, 12288],
        [2176, 2048, 14592],
    );
    author_box(
        &mut workspace,
        stone,
        [6016, 256, 12288],
        [6144, 2048, 14592],
    );
    author_box(
        &mut workspace,
        stone,
        [2176, 256, 14464],
        [3072, 2048, 14592],
    );
    author_box(
        &mut workspace,
        stone,
        [4480, 256, 14464],
        [6016, 2048, 14592],
    );
    let water = author_box(
        &mut workspace,
        metal,
        [2304, 256, 12544],
        [5888, 384, 14336],
    );
    workspace.replace_brush_selection(water, None);
    workspace.set_selected_brush_contents(psxed_project::brush::BrushContents::Water);

    // Four broad risers replace the source inspiration's ladder. Every rise
    // is 448 units, below the proven 640-unit motor step envelope.
    for (z_min, z_max, top) in [
        (11392, 12288, 704),
        (10496, 11392, 1152),
        (9600, 10496, 1600),
        (8704, 9600, 2048),
    ] {
        // The final landing overlaps the east court slab. A merely butted
        // seam leaves an expanded player hull colliding with the slab edge.
        let x_max = if top == 2048 { 6272 } else { 5888 };
        author_box(&mut workspace, trim, [4608, 0, z_min], [x_max, top, z_max]);
    }
    author_box(
        &mut workspace,
        stone,
        [4480, 256, 9600],
        [4608, 3584, 12288],
    );
    author_box(
        &mut workspace,
        stone,
        [5888, 256, 9600],
        [6016, 3584, 12288],
    );

    // Roofless arrival court. Its east opening frames the reused arena; its
    // north opening becomes the later relay shortcut.
    // Keep the stair lane open instead of burying its upper risers beneath a
    // single courtyard slab. The final riser is already the centre landing.
    author_box(
        &mut workspace,
        slate,
        [2048, 1792, 8704],
        [4608, 2048, 12288],
    );
    author_box(
        &mut workspace,
        slate,
        [4608, 1792, 8704],
        [8192, 2048, 9216],
    );
    author_box(
        &mut workspace,
        slate,
        [5888, 1792, 9216],
        [8192, 2048, 12288],
    );
    author_box(
        &mut workspace,
        stone,
        [2048, 2048, 8704],
        [2176, 4608, 12288],
    );
    author_box(
        &mut workspace,
        stone,
        [2176, 2048, 12160],
        [4608, 4608, 12288],
    );
    author_box(
        &mut workspace,
        stone,
        [5888, 2048, 12160],
        [8192, 4608, 12288],
    );
    author_box(
        &mut workspace,
        stone,
        [2176, 2048, 8704],
        [4608, 4608, 8832],
    );
    author_box(
        &mut workspace,
        stone,
        [5888, 2048, 8704],
        [6400, 4608, 8832],
    );
    author_box(
        &mut workspace,
        stone,
        [7936, 2048, 8704],
        [8192, 4608, 8832],
    );

    // The central roofless Warden Court is intentionally the same physical
    // space on the first intimidating approach and the later upper return.
    author_box(
        &mut workspace,
        slate,
        [8192, 1792, 8192],
        [15360, 2048, 15360],
    );
    author_box(
        &mut workspace,
        stone,
        [8192, 2048, 8192],
        [8320, 6144, 10240],
    );
    author_box(
        &mut workspace,
        stone,
        [8192, 2048, 11520],
        [8320, 6144, 15360],
    );
    author_box(
        &mut workspace,
        stone,
        [8192, 2048, 15232],
        [15360, 6144, 15360],
    );
    author_box(
        &mut workspace,
        stone,
        [8192, 2048, 8192],
        [10624, 6144, 8320],
    );
    author_box(
        &mut workspace,
        stone,
        [13568, 2048, 8192],
        [15360, 6144, 8320],
    );
    author_box(
        &mut workspace,
        stone,
        [15232, 2048, 8192],
        [15360, 6144, 11264],
    );
    author_box(
        &mut workspace,
        stone,
        [15232, 2048, 12544],
        [15360, 6144, 15360],
    );
    let arena_gate = author_box(
        &mut workspace,
        door_material,
        [8192, 2048, 10240],
        [8320, 3328, 11520],
    );

    // Side escape and lower relay/equipment loop. The 512-unit descent is
    // deliberate and remains within the proven motor envelope.
    author_box(
        &mut workspace,
        slate,
        [10624, 1280, 6144],
        [11776, 1536, 8320],
    );
    // Join the 512-unit arena drop with a real intermediate landing. The
    // overlap turns two disconnected slab edges into two 256-unit steps for
    // the expanded player hull, while preserving the lowered relay floor.
    author_box(
        &mut workspace,
        trim,
        [10624, 1280, 8064],
        [11776, 1792, 8512],
    );
    // Split the west wall around a full-height aperture into the relay hall.
    author_box(
        &mut workspace,
        stone,
        [10496, 1536, 6144],
        [10624, 3584, 6656],
    );
    author_box(
        &mut workspace,
        stone,
        [10496, 1536, 7424],
        [10624, 3584, 8192],
    );
    author_box(
        &mut workspace,
        stone,
        [11776, 1536, 7168],
        [11904, 3584, 7424],
    );
    author_box(
        &mut workspace,
        stone,
        [10624, 3584, 6144],
        [11776, 3840, 8192],
    );
    author_box(
        &mut workspace,
        slate,
        [6144, 1280, 6144],
        [10624, 1536, 8832],
    );
    author_box(
        &mut workspace,
        stone,
        [6144, 1536, 6144],
        [6272, 3328, 8192],
    );
    author_box(
        &mut workspace,
        stone,
        [6144, 1536, 6144],
        [10624, 3328, 6272],
    );
    author_box(
        &mut workspace,
        stone,
        [6144, 3072, 6272],
        [10624, 3328, 8192],
    );
    author_box(
        &mut workspace,
        stone,
        [6144, 1536, 8064],
        [6400, 3328, 8832],
    );
    author_box(
        &mut workspace,
        stone,
        [7936, 1536, 8064],
        [10624, 3328, 8832],
    );
    let shortcut_gate = author_box(
        &mut workspace,
        door_material,
        [6400, 2048, 8704],
        [7936, 3328, 8832],
    );

    author_box(
        &mut workspace,
        slate,
        [11776, 1280, 6144],
        [16896, 1536, 7168],
    );
    author_box(
        &mut workspace,
        stone,
        [11776, 1536, 6016],
        [16896, 3328, 6144],
    );
    author_box(
        &mut workspace,
        stone,
        [11776, 1536, 7168],
        [16896, 3328, 7296],
    );
    author_box(
        &mut workspace,
        stone,
        [11776, 3072, 6144],
        [16000, 3328, 7168],
    );
    // Five 512-unit risers lead to the upper rampart. No ladder, jump, key,
    // boulder physics, or scripted wall break is required.
    for (x_min, x_max, top) in [
        (16896, 17408, 2048),
        (17408, 17920, 2560),
        (17920, 18432, 3072),
        (18432, 18944, 3584),
        (18944, 19456, 4096),
    ] {
        // Overlap the final landing into the rampart. Butted coplanar slabs
        // retain a clip-brush seam for an expanded body hull.
        let z_max = if top == 4096 { 7552 } else { 7168 };
        author_box(&mut workspace, trim, [x_min, 0, 6144], [x_max, top, z_max]);
    }
    author_box(
        &mut workspace,
        stone,
        [16896, 1536, 6016],
        [19456, 5120, 6144],
    );
    author_box(
        &mut workspace,
        stone,
        [16896, 1536, 7168],
        [18176, 5120, 7296],
    );

    // Upper return and descending ledges into the same arena. Each descent is
    // only 512 units, producing the plunge composition without relying on a
    // plunge-attack or fall-damage mechanic.
    author_box(
        &mut workspace,
        slate,
        [11776, 3840, 7168],
        [19456, 4096, 8320],
    );
    author_box(
        &mut workspace,
        stone,
        [11776, 4096, 7040],
        [16896, 4864, 7168],
    );
    author_box(
        &mut workspace,
        stone,
        [19456, 4096, 7168],
        [19584, 4864, 8320],
    );
    author_box(
        &mut workspace,
        trim,
        [11776, 3840, 8192],
        [13568, 4096, 8960],
    );
    author_box(
        &mut workspace,
        trim,
        [11776, 3328, 8960],
        [13568, 3584, 9472],
    );
    author_box(
        &mut workspace,
        trim,
        [11776, 2816, 9472],
        [13568, 3072, 9984],
    );
    author_box(
        &mut workspace,
        trim,
        [11776, 2304, 9984],
        [13568, 2560, 10496],
    );

    // Far gate and a roofless, gently rising cliff path. It deliberately ends
    // in an authored vista, not a copied crow/cutscene or level transition.
    let final_gate = author_box(
        &mut workspace,
        door_material,
        [15232, 2048, 11264],
        [15360, 3328, 12544],
    );
    for step in 0..4 {
        let x_min = 15360 + step * 1536;
        let top = 2048 + step * 256;
        author_box(
            &mut workspace,
            slate,
            [x_min, 1792, 10752],
            [x_min + 1536, top, 13056],
        );
    }
    author_box(
        &mut workspace,
        stone,
        [15360, 2048, 10624],
        [21504, 3072, 10752],
    );
    // Leave the far and north edges open to the authored panorama. The route
    // finishes safely on the broad final shelf; the missing parapet/end cap
    // is deliberate cliff composition, not an unsealed room accident.

    let _arena_door = add_door(
        &mut workspace,
        root,
        "First Warden Gate",
        arena_gate,
        [8256, COURT_FLOOR, 10880],
        [0, 2048, 0],
    );
    let _shortcut = add_door(
        &mut workspace,
        root,
        "Sanctum Shortcut",
        shortcut_gate,
        [7168, COURT_FLOOR, 8768],
        [0, 2048, 0],
    );
    let final_door = add_door(
        &mut workspace,
        root,
        "Far Warden Gate",
        final_gate,
        [15296, COURT_FLOOR, 11904],
        [0, 2048, 0],
    );
    let upper_route_master = "Upper Route Cleared";
    let _upper_route_gate = add_multisource(&mut workspace, root, upper_route_master, 1);
    let _upper_route_trigger = add_checkpoint_trigger(
        &mut workspace,
        root,
        "Upper Return Threshold",
        upper_route_master,
        [18176, 4096, 7744],
        [1536, 1024, 1024],
    );
    workspace.push_undo();
    if let Some(NodeKind::Logic { master, .. }) = workspace
        .project
        .active_scene_mut()
        .node_mut(final_door)
        .map(|node| &mut node.kind)
    {
        *master = upper_route_master.to_string();
    }
    workspace.mark_dirty();

    let courtyard_relay = add_checkpoint(
        &mut workspace,
        root,
        "Courtyard Relay",
        "COURTYARD RELAY",
        [3584, COURT_FLOOR + 1, 10496],
    );
    let _courtyard_trigger = add_checkpoint_trigger(
        &mut workspace,
        root,
        "Courtyard Memory Field",
        "Courtyard Relay",
        [3584, COURT_FLOOR, 10496],
        [512, 768, 512],
    );
    let sanctum_relay = add_checkpoint(
        &mut workspace,
        root,
        "Sanctum Relay",
        "SANCTUM RELAY",
        [8192, RELAY_FLOOR + 1, 6912],
    );
    let _relay_trigger = add_checkpoint_trigger(
        &mut workspace,
        root,
        "Relay Approach",
        "Sanctum Relay",
        [8192, RELAY_FLOOR, 6912],
        [512, 768, 512],
    );
    assert_ne!(courtyard_relay, sanctum_relay);

    let player = place_character(
        &mut workspace,
        aletha,
        "Aletha",
        [3840, LOWER_FLOOR, 21376],
        180.0,
        360,
    );
    let lower_guard = place_character(
        &mut workspace,
        mantis,
        "Intake Custodian",
        [3840, LOWER_FLOOR, 15744],
        180.0,
        448,
    );
    let gallery_guard = place_character(
        &mut workspace,
        mantis,
        "Gallery Custodian",
        [15872, RELAY_FLOOR, 6656],
        270.0,
        448,
    );
    let warden = place_character(
        &mut workspace,
        mantis,
        "Warden Prototype",
        [12672, COURT_FLOOR, 12416],
        270.0,
        640,
    );
    attach_weapon(&mut workspace, player, sword_light);
    for enemy in [lower_guard, gallery_guard, warden] {
        attach_weapon(&mut workspace, enemy, sword_heavy);
    }

    // A restrained lighting pass marks route beats without pretending to be
    // final light art. Open courts remain primarily sky-lit in presentation.
    for [x, y, z] in [
        [3840, 1280, 21120],
        [3840, 1280, 15360],
        [3840, 3072, 10496],
        [8192, 2560, 6912],
        [14592, 2560, 6656],
        [18176, 4608, 6656],
        [12672, 3584, 12416],
        [19968, 3584, 11904],
    ] {
        workspace.orthographic_focus[1] = y as f32;
        workspace.set_active_tool_cycle_value((ViewTool::Place, Some(PlaceKind::PointLightMarker)));
        assert!(workspace.place_bsp_from_top([x as f32, z as f32]));
        let light = workspace.selected_node_id();
        workspace
            .project
            .active_scene_mut()
            .node_mut(light)
            .expect("placed point light")
            .transform
            .translation = [x as f32, y as f32, z as f32];
        workspace.mark_dirty();
    }

    // Open the tracked project on an immediately legible authored overview,
    // rather than inheriting the tiny starter courtyard's camera target.
    workspace.project.editor_camera.orbit_yaw_q12 = 3584;
    workspace.project.editor_camera.orbit_pitch_q12 = 3712;
    workspace.project.editor_camera.orbit_target = [11_000, 2_200, 13_000];
    workspace.project.editor_camera.orbit_radius = 22_000;
    workspace.apply_project_editor_camera();
    workspace.mark_dirty();

    workspace.save_if_dirty().expect("persist Ashen Sanctum");
    let reopened = EditorWorkspace::open_directory(&project_dir).expect("reopen Ashen Sanctum");
    assert!(reopened.has_player_source());
    assert_eq!(
        reopened.project().world_format(),
        psxed_project::ProjectWorldFormat::Bsp
    );
    let scene = reopened.project().active_scene();
    assert!(
        scene.brushes.len() >= 65,
        "brush count {}",
        scene.brushes.len()
    );
    assert_eq!(
        scene
            .brushes
            .iter()
            .filter(|brush| brush.contents == psxed_project::brush::BrushContents::Water)
            .count(),
        1
    );
    assert_eq!(
        scene
            .brushes
            .iter()
            .filter(|brush| brush.mover.is_some())
            .count(),
        3
    );
    let node_count = |wants: fn(&NodeKind) -> bool| {
        scene
            .nodes()
            .iter()
            .filter(|node| wants(&node.kind))
            .count()
    };
    assert_eq!(
        node_count(|kind| matches!(
            kind,
            NodeKind::Equipment {
                weapon: Some(_),
                ..
            }
        )),
        4
    );
    assert_eq!(
        node_count(|kind| matches!(
            kind,
            NodeKind::Interactable {
                kind: psxed_project::InteractableKind::Checkpoint { .. },
                ..
            }
        )),
        2
    );

    let mut reopened = reopened;
    reopened
        .cook_playtest_to_dir(&cook_a)
        .expect("cook Ashen Sanctum");
    reopened
        .cook_playtest_to_dir(&cook_b)
        .expect("deterministic Ashen Sanctum recook");
    for filename in [
        psxed_project::brush_playtest::BRUSH_WORLD_FILENAME,
        psxed_project::playtest::COOKED_MANIFEST_FILENAME,
    ] {
        let a = std::fs::read(cook_a.join(filename)).unwrap();
        let b = std::fs::read(cook_b.join(filename)).unwrap();
        assert_eq!(a, b, "{filename} drifted across unchanged recook");
    }
    let pxbsp = std::fs::read(cook_a.join(psxed_project::brush_playtest::BRUSH_WORLD_FILENAME))
        .expect("read Ashen Sanctum PXBSP");
    assert!(!pxbsp.is_empty());
    assert!(
        pxbsp.len() < 1_100_000,
        "PXBSP {} exceeds resident budget",
        pxbsp.len()
    );

    let document = ProjectDocument::load_from_path(project_dir.join("project.ron")).unwrap();
    let (package, report) = psxed_project::playtest::build_package(&document, &project_dir);
    assert!(report.is_ok(), "{}", report.error_messages().join("; "));
    let package = package.expect("Ashen Sanctum package");
    let psxed_project::playtest::PlaytestWorldGeometry::Pxbsp(ref world) = package.world_geometry
    else {
        panic!("Ashen Sanctum must cook as PXBSP");
    };
    assert_eq!(world.movers.len(), 3);
    assert_eq!(package.game_entities.len(), 3);
    assert_eq!(package.equipment.len(), 4);
    assert_eq!(package.interactables.len(), 2);
    prove_cooked_player_hull_route(world);

    if let Some(export_dir) = std::env::var_os("PSOXIDE_ASHEN_SANCTUM_PROJECT_OUT") {
        let export_dir = PathBuf::from(export_dir);
        assert!(
            !export_dir.join("project.ron").exists(),
            "Ashen Sanctum export already has project.ron: {}",
            export_dir.display()
        );
        copy_dir_recursive(&project_dir, &export_dir).expect("export Ashen Sanctum project");
        let project_file = export_dir.join("project.ron");
        let mut exported = ProjectDocument::load_from_path(&project_file).expect("load export");
        exported.name = "Ashen Sanctum Tech Demo".to_string();
        exported
            .save_to_path(&project_file)
            .expect("stabilise export name");
        println!("Ashen Sanctum project: {}", project_file.display());
    }

    let _ = std::fs::remove_dir_all(project_dir);
    let _ = std::fs::remove_dir_all(cook_a);
    let _ = std::fs::remove_dir_all(cook_b);
}
