//! The lever-and-doors test level.
//!
//! Three rooms in a row along +X, each four sectors by three, joined by
//! doorways cut in their shared walls:
//!
//! ```text
//!          A (lever)               B                       C
//!   z=2  +-----------------+-----------------+-----------------+
//!        |                 |                 |    open gap     |   B|C, row 2: no door, so C is reachable
//!   z=1  | player   lever  |= gate door      |= shortcut door  |   A|B, row 1: opened by the lever
//!        |                 |                 |                 |   B|C, row 1: one-way, opens from C only
//!   z=0  +-----------------+-----------------+-----------------+
//! ```
//!
//! * `gate_ab` (A to B): a sliding door the lever in A opens. Persistent.
//! * `checkpoint` (room A, on the walk to the lever): a rest point, reachable by
//!   radius and line of sight like the levers.
//! * `gap` (B to C, row 2): an empty doorway, so C can be reached at all.
//! * `shortcut` (B to C, row 1): a one-way sliding door that refuses from the
//!   B side and opens from the C side, so a player walking back from C opens
//!   it and one coming from A is told it will not budge. Persistent.
//!
//! Headless check (the owner's PGO emulator, no window): cook the project, build
//! the guest and a disc with `mkisopsx`, then
//! `psoxide-emulator launch --path lever.cue --embedded-playtest --memcard card.mcd
//! --pad-pulses 0x0010@1500+75,0x4000@1620+8,0x0010@1800+260 --stop-at-poll 2300
//! --dump-ram ram.bin`: hold up for 75 ticks, press Cross at the lever, hold up
//! again. Without the Cross press the player stops against the closed gate at
//! x = 303 (engine units); with it the gate slides up and the player walks on
//! through room B. Appending `0x4000@2000+8` (Cross at the shortcut, from its
//! west side) shows its refusal message instead of opening it; walking round
//! through the gap (`0x2000@2070+8,0x0020@2110+26,0x0010@2160+25,0x0080@2210+27`)
//! and pushing at it from the east side (`0x0040@2260+40,0x4000@2320+8`) opens it.
//!
//! Built from the starter's base project (the Cortex player, camera, sky and
//! UI), so [`create_project`] needs the editor repository's
//! `projects/graybox-reach` as its base, like every starter.

use std::path::{Path, PathBuf};

use crate::{
    DoorDirection, DoorLock, DoorSide, DoorSlide, InteractableKind, LogicNodeKind, NodeId,
    NodeKind, ProjectDocument, ResourceData, ResourceId, SwitchBehavior,
};

use super::base_project::{link_assets, place_actors, spawn_point, strip_base, SpawnRequest};
use super::overrides::{Axis, EdgeKey, SEGMENT_CLICKS};
use super::room::GridRoom;
use super::starter::{StarterError, STARTER_BASE, STARTER_HEIGHT, STARTER_SCALE};
use super::surface::GridSurface;
use super::world::{GridWorld, SceneWorld};

/// Sectors along X of each room.
pub const ROOM_WIDTH: usize = 4;
/// Sectors along Z of each room.
pub const ROOM_DEPTH: usize = 3;
/// The three rooms' names, west to east.
pub const ROOM_NAMES: [&str; 3] = ["Room A", "Room B", "Room C"];
/// Name of the lever-opened door node (and of its Box Prop, suffixed `box`).
pub const GATE_DOOR: &str = "gate_ab";
/// Name of the one-way door node.
pub const SHORTCUT_DOOR: &str = "shortcut";
/// Name of the lever entity.
pub const LEVER: &str = "gate lever";
/// Name (and checkpoint id) of the checkpoint entity in room A.
pub const CHECKPOINT: &str = "gate checkpoint";
/// Name of the entity whose Switch targets [`SHORTCUT_DOOR`].
pub const SHORTCUT_BUTTON: &str = "shortcut button";

/// Door sliding time, 60 Hz ticks.
const SLIDE_TICKS: u16 = 60;
/// Half the thickness of a door slab, editor units.
const DOOR_HALF_THICKNESS: i16 = 64;
/// Half the width of a doorway, editor units (a sector is 1280).
const DOOR_HALF_WIDTH: i16 = 600;

fn find_material(project: &ProjectDocument, names: &[&str]) -> Option<ResourceId> {
    names.iter().find_map(|name| {
        project
            .resources
            .iter()
            .find(|r| r.name == *name && matches!(r.data, ResourceData::Material(_)))
            .map(|r| r.id)
    })
}

/// The three rooms: flat floors and ceilings, derived closure walls, with a
/// doorway (a hidden, non-solid wall segment on each side of the shared
/// edge) at the rows the doors and the gap use.
#[must_use]
pub fn lever_doors_world(
    floor: Option<ResourceId>,
    wall: Option<ResourceId>,
    ceiling: Option<ResourceId>,
) -> GridWorld {
    let mut rooms = Vec::new();
    for (index, name) in ROOM_NAMES.iter().enumerate() {
        let mut room = GridRoom::new_derived(*name, ROOM_WIDTH, ROOM_DEPTH);
        room.origin = [(index * ROOM_WIDTH) as i32, 0];
        room.defaults.wall_material = wall;
        for z in 0..ROOM_DEPTH {
            for x in 0..ROOM_WIDTH {
                let Some(sector) = room.sector_or_insert(x, z) else {
                    continue;
                };
                sector.set_ground_floor(Some(GridSurface::flat_auto(0, floor)));
                sector.set_ground_ceiling(Some(GridSurface::flat_auto(STARTER_HEIGHT, ceiling)));
            }
        }
        rooms.push(room);
    }
    // Doorways: A|B at row 1, B|C at rows 1 (the shortcut) and 2 (the gap).
    let clicks = SEGMENT_CLICKS as i32;
    let width = ROOM_WIDTH as i32;
    let doorways = [(1, 1), (2, 1), (2, 2)];
    for (boundary, row) in doorways {
        let key = EdgeKey {
            x: boundary * width * clicks,
            z: row * clicks,
            axis: Axis::Z,
            len: SEGMENT_CLICKS,
        };
        // Both neighbours own a closure wall on the shared edge.
        for side in [boundary as usize - 1, boundary as usize] {
            rooms[side].edge_overrides.edit(key, |edge| {
                edge.hidden = true;
                edge.solid = Some(false);
            });
        }
    }
    GridWorld {
        scale: STARTER_SCALE,
        texels_per_sector: super::units::SectorTexels::DEFAULT,
        rooms,
        shading: super::light::FacingShade::SUBTLE,
        global_light: super::lighting::GlobalLight::NEW_WORLD,
        stream: super::world::StreamRadii::DEFAULT,
        mesh_assets: Vec::new(),
        level_names: Default::default(),
        palette_pins: Vec::new(),
    }
}

/// Vertices of a door slab: `half_width` along Z, `half_thickness` along X,
/// `height` tall, bottom ring first.
fn slab_vertices(half_thickness: i16, half_width: i16, height: i16) -> [[i16; 3]; 8] {
    let (t, w, h) = (half_thickness, half_width, height);
    [
        [-t, 0, -w],
        [t, 0, -w],
        [t, 0, w],
        [-t, 0, w],
        [-t, h, -w],
        [t, h, -w],
        [t, h, w],
        [-t, h, w],
    ]
}

/// Turn `project` into the lever-and-doors project in place: the base's
/// level goes, its player stays, the world becomes the three rooms and the
/// gameplay nodes are placed.
///
/// # Errors
///
/// Fails when the first room has no floor to spawn the player on.
pub fn into_lever_doors(project: &mut ProjectDocument) -> Result<(), StarterError> {
    strip_base(project, 0);
    super::graybox_set::add_to(project);
    let ground = find_material(project, &["Graybox / Ground 64"]);
    let walls = find_material(project, &["Graybox / Original 64"]).or(ground);
    let roof = find_material(project, &["Graybox / Cliff 64"]).or(ground);
    let door_material = find_material(project, &["Graybox / Cliff 64", "Graybox / Original 64"]);
    let world = lever_doors_world(ground, walls, roof);

    // The player starts in room A, west end of the middle row.
    let spawn = SpawnRequest {
        room: 0,
        cell: [0, 1],
    };
    let mut at = spawn_point(&world, spawn)
        .ok_or_else(|| StarterError::Io("the lever room has no floor".to_string()))?;
    // Walking "forward" from here drifts a little south (the camera trails the
    // player's turn), so start a third of a sector north of the doorway's
    // middle and arrive at it.
    at[2] -= 384;
    let sector = world.scale.sector_editor_units();
    // Facing +X (east), toward the door: a node yaw of 270 degrees faces east
    // (90 faces west).
    place_actors(project, &world, at, 270.0, sector);

    let scene = project.active_scene_mut();
    let unit = scene
        .world_sector_size_for_node(NodeId::ROOT)
        .unwrap_or(crate::DEFAULT_WORLD_SECTOR_SIZE)
        .max(1) as f32;
    let at_cell = |room: usize, x: f32, z: f32| -> [f32; 3] {
        let origin = (room * ROOM_WIDTH) as f32;
        [(origin + x) * sector as f32, 0.0, z * sector as f32]
    };

    // One lamp per room.
    for (room, room_name) in ROOM_NAMES.iter().enumerate() {
        let lamp = scene.add_node(
            NodeId::ROOT,
            format!("Light {room_name}"),
            NodeKind::PointLight {
                color: [255, 214, 160],
                intensity: 1.4,
                radius: 4.5 * sector as f32 / unit,
            },
        );
        if let Some(node) = scene.node_mut(lamp) {
            let mut at = at_cell(room, ROOM_WIDTH as f32 / 2.0, ROOM_DEPTH as f32 / 2.0);
            at[1] = world.scale.to_editor(STARTER_HEIGHT * 3 / 4) as f32;
            node.transform.translation = at;
        }
    }

    // Doors: a Box Prop slab in the doorway and the Door node that drives it.
    let height = world.scale.to_editor(STARTER_HEIGHT) as i16;
    let slab = |scene: &mut crate::Scene, name: &str, at: [f32; 3]| {
        let id = scene.add_node(
            NodeId::ROOT,
            name,
            NodeKind::BoxProp {
                materials: [door_material; crate::BOX_PROP_FACE_COUNT],
                uvs: crate::default_box_prop_uvs(),
                vertices: slab_vertices(DOOR_HALF_THICKNESS, DOOR_HALF_WIDTH, height),
                collision_enabled: true,
                break_flags: 0,
                erosion: Default::default(),
            },
        );
        if let Some(node) = scene.node_mut(id) {
            node.transform.translation = at;
        }
    };
    let door_node =
        |scene: &mut crate::Scene, name: &str, box_prop: String, lock: DoorLock, refusal: &str| {
            scene.add_node(
                NodeId::ROOT,
                name,
                NodeKind::Logic {
                    kind: LogicNodeKind::Door {
                        box_prop,
                        start_open: false,
                        slide: Some(DoorSlide {
                            direction: DoorDirection::Up,
                            distance: height as u16,
                            ticks: SLIDE_TICKS,
                        }),
                        lock,
                        refusal: refusal.to_string(),
                    },
                    target: String::new(),
                    killtarget: String::new(),
                    master: String::new(),
                    delay_ticks: 0,
                    wait_ticks: 0,
                    enabled: true,
                    persistent: true,
                },
            )
        };
    // x is the shared edge, z the middle of the doorway's row.
    let gate_at = [
        (ROOM_WIDTH) as f32 * sector as f32,
        0.0,
        1.5 * sector as f32,
    ];
    let shortcut_at = [
        (2 * ROOM_WIDTH) as f32 * sector as f32,
        0.0,
        1.5 * sector as f32,
    ];
    slab(scene, &format!("{GATE_DOOR} box"), gate_at);
    door_node(
        scene,
        GATE_DOOR,
        format!("{GATE_DOOR} box"),
        DoorLock::Unlocked,
        "",
    );
    slab(scene, &format!("{SHORTCUT_DOOR} box"), shortcut_at);
    door_node(
        scene,
        SHORTCUT_DOOR,
        format!("{SHORTCUT_DOOR} box"),
        DoorLock::OneWay {
            refuse_from: DoorSide::West,
        },
        "It will not open from this side.",
    );

    // Interactables: an Entity with an Interactable component.
    let entity = |scene: &mut crate::Scene,
                  name: &str,
                  at: [f32; 3],
                  kind: InteractableKind,
                  prompt: &str,
                  radius: u16| {
        let id = scene.add_node(NodeId::ROOT, name, NodeKind::Entity);
        if let Some(node) = scene.node_mut(id) {
            node.transform.translation = at;
        }
        scene.add_node(
            id,
            "Interactable",
            NodeKind::Interactable {
                kind,
                prompt: prompt.to_string(),
                radius,
                enabled: true,
            },
        );
    };
    // A checkpoint, in room A on the player's walk to the lever.
    entity(
        scene,
        CHECKPOINT,
        at_cell(0, 1.4, 1.2),
        InteractableKind::Checkpoint {
            checkpoint_id: CHECKPOINT.to_string(),
            title: "Checkpoint".to_string(),
            body: "Rested.".to_string(),
        },
        "REST",
        640,
    );
    // The lever, in room A.
    entity(
        scene,
        LEVER,
        at_cell(0, 2.5, 1.5),
        InteractableKind::Switch {
            target: GATE_DOOR.to_string(),
            behavior: SwitchBehavior::OneShot,
            unlocks: false,
            persistent: true,
        },
        "PULL LEVER",
        640,
    );
    // The shortcut's button stands on the door's plane so one press radius
    // reaches both sides; the door decides which side is let through.
    entity(
        scene,
        SHORTCUT_BUTTON,
        [shortcut_at[0], 0.0, shortcut_at[2] - 320.0],
        InteractableKind::Switch {
            target: SHORTCUT_DOOR.to_string(),
            behavior: SwitchBehavior::Toggle,
            unlocks: false,
            persistent: true,
        },
        "PUSH DOOR",
        640,
    );

    scene.world = SceneWorld::Grid(world);
    project.editor_camera = crate::EditorCameraState::default();
    project.editor_viewport = crate::EditorViewportState::default();
    Ok(())
}

/// Create the lever-and-doors project at `out_dir` from the base project in
/// `base_dir`, named `name`. Returns the written `project.ron`.
///
/// # Errors
///
/// Fails when the base project does not load or a file cannot be written.
pub fn create_project(
    base_dir: &Path,
    out_dir: &Path,
    name: &str,
) -> Result<PathBuf, StarterError> {
    let mut project = ProjectDocument::load_from_path(base_dir.join("project.ron"))
        .map_err(|e| StarterError::Base(e.to_string()))?;
    into_lever_doors(&mut project)?;
    project.name = name.to_string();
    std::fs::create_dir_all(out_dir)
        .map_err(|e| StarterError::Io(format!("create {}: {e}", out_dir.display())))?;
    link_assets(base_dir, out_dir)
        .map_err(|e| StarterError::Io(format!("link the base assets: {e}")))?;
    let path = out_dir.join("project.ron");
    project
        .save_to_path(&path)
        .map_err(|e| StarterError::Io(e.to_string()))?;
    Ok(path)
}

/// The base project directory under `projects_dir`.
#[must_use]
pub fn base_dir(projects_dir: &Path) -> PathBuf {
    projects_dir.join(STARTER_BASE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::limits;

    #[test]
    fn three_rooms_in_a_row_with_three_doorways_validate() {
        let world = lever_doors_world(None, None, None);
        assert!(!limits::has_errors(&limits::validate(&world)));
        assert_eq!(world.rooms.len(), 3);
        let origins: Vec<[i32; 2]> = world.rooms.iter().map(|r| r.origin).collect();
        assert_eq!(origins, [[0, 0], [4, 0], [8, 0]]);
        // The A|B edge has one opening on each side; B|C has two.
        assert_eq!(world.rooms[0].edge_overrides.len(), 1);
        assert_eq!(world.rooms[1].edge_overrides.len(), 3);
        assert_eq!(world.rooms[2].edge_overrides.len(), 2);
    }

    /// The lever level made beside its base (so shared asset paths resolve),
    /// cooked, and its logic records read back. Needs the editor repository's
    /// `projects/` (point `PSOXIDE_PROJECTS_DIR` at a copy of it), like every
    /// starter-project test.
    #[test]
    #[ignore = "needs projects/graybox-reach and projects/default: set PSOXIDE_PROJECTS_DIR"]
    fn the_lever_level_cooks_with_its_logic_wired() {
        use psx_level::{door_flags, logic_flags, logic_kind, LOGIC_LINK_NONE};
        let projects = crate::projects_dir();
        let out = projects.join(format!("test-lever-doors-{}", std::process::id()));
        let path = create_project(&base_dir(&projects), &out, "Lever doors")
            .expect("the lever project is created");
        let project = ProjectDocument::load_from_path(&path).expect("it loads");
        let (package, report) = crate::playtest::build_package(&project, &out);
        let _ = std::fs::remove_dir_all(&out);
        assert!(report.is_ok(), "{:?}", report.error_messages());
        let package = package.expect("a package");

        let by_kind = |kind| {
            package
                .logic
                .iter()
                .enumerate()
                .filter(move |(_, r)| r.kind == kind)
                .collect::<Vec<_>>()
        };
        let doors = by_kind(logic_kind::DOOR);
        let switches = by_kind(logic_kind::SWITCH);
        assert_eq!((doors.len(), switches.len()), (2, 2));

        // Every door slides up one room height over the authored ticks, is
        // linked to its box, takes the box's position for its plane and is
        // saved.
        for (_, door) in &doors {
            assert_eq!(door.spawnflags & door_flags::DIR_MASK, door_flags::DIR_UP);
            assert_eq!((door.arg0, door.arg1), (160, 60), "2560 editor units / 16");
            assert_ne!(door.link, LOGIC_LINK_NONE);
            assert_ne!(door.flags & logic_flags::PERSISTENT, 0);
        }
        let (_, gate) = doors[0];
        let (_, shortcut) = doors[1];
        assert_eq!((gate.x, gate.z), (320, 120), "the A|B doorway, row 1");
        assert_eq!(
            (shortcut.x, shortcut.z),
            (640, 120),
            "the B|C doorway, row 1"
        );
        assert_eq!(
            gate.spawnflags & !door_flags::DIR_MASK,
            0,
            "the gate is plain"
        );
        assert_eq!(
            shortcut.spawnflags >> door_flags::REFUSE_SHIFT,
            door_flags::REFUSE_WEST,
            "refuses the B side, opens from C"
        );
        let refusal = &package.interactable_messages[usize::from(shortcut.message)];
        assert_eq!(refusal.body, "It will not open from this side.");

        // The levers fire the doors by name, are saved, and the gate lever
        // retires after one pull.
        let (_, gate_lever) = switches
            .iter()
            .find(|(_, r)| r.target == gate.targetname)
            .expect("a lever fires the gate");
        assert_eq!(gate_lever.wait_ticks, -1);
        assert!(switches
            .iter()
            .any(|(_, r)| r.target == shortcut.targetname));
        for (_, lever) in &switches {
            assert_ne!(lever.flags & logic_flags::PERSISTENT, 0);
            assert_eq!(lever.message, psx_level::INTERACTABLE_MESSAGE_NONE);
        }

        // Four persistent records, four distinct save slots.
        let slots: std::collections::BTreeSet<u16> = package
            .logic
            .iter()
            .zip(&package.logic_ids)
            .filter(|(r, _)| r.flags & logic_flags::PERSISTENT != 0)
            .map(|(_, id)| psx_level::persistent_flag_slot(psx_level::StableId(*id)))
            .collect();
        assert_eq!(slots.len(), 4);
        assert!(slots
            .iter()
            .all(|slot| usize::from(*slot) < psx_level::POI_PERSISTENT_FLAG_CAPACITY));
    }

    #[test]
    fn the_slab_spans_the_doorway() {
        let v = slab_vertices(64, 600, 2560);
        assert_eq!(v[0], [-64, 0, -600]);
        assert_eq!(v[6], [64, 2560, 600]);
    }
}
