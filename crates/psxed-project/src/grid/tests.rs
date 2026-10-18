//! Document-level tests: how the grid world sits inside a project file.

use std::path::{Path, PathBuf};

use super::room::GridRoom;
use super::surface::{GridSurface, GridWall};
use super::world::{GridWorld, SceneWorld};
use crate::ProjectDocument;

fn project_files() -> Vec<PathBuf> {
    let editor = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for group in ["projects", "archive/fixtures"] {
        let Ok(entries) = std::fs::read_dir(editor.join(group)) else {
            continue;
        };
        for entry in entries.flatten() {
            let candidate = entry.path().join("project.ron");
            if candidate.is_file() {
                files.push(candidate);
            }
        }
    }
    files.sort();
    files
}

fn small_world() -> GridWorld {
    let mut room = GridRoom::new("hall", 2, 2);
    room.origin = [-1, 3];
    room.y_offset = 256;
    let a = room.sector_or_insert(0, 0).unwrap();
    a.set_ground_floor(Some(GridSurface::flat(0, None)));
    a.north.push(GridWall::span(0, 1024, None));
    let b = room.sector_or_insert(1, 1).unwrap();
    let mut slope = GridSurface::flat(0, None);
    slope.heights = [0, 0, 256, 256];
    b.set_ground_floor(Some(slope));
    GridWorld {
        rooms: vec![room],
        ..GridWorld::new()
    }
}

#[test]
fn shipped_projects_are_grid_worlds_and_round_trip() {
    let files = project_files();
    assert!(
        files.len() >= 4,
        "expected the tracked projects, found {files:?}"
    );
    let mut checked = 0;
    for path in files {
        let project = ProjectDocument::load_from_path(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        for scene in &project.scenes {
            assert!(
                scene.world.grid().is_some(),
                "{} scene {:?} did not load as a grid world",
                path.display(),
                scene.name
            );
        }
        let saved = project.to_ron_string().unwrap();
        let reloaded = ProjectDocument::from_ron_str(&saved).unwrap();
        assert_eq!(
            reloaded,
            project,
            "{} changed in a round trip",
            path.display()
        );
        assert_eq!(
            reloaded.to_ron_string().unwrap(),
            saved,
            "{} does not reach a fixed point",
            path.display()
        );
        checked += 1;
    }
    assert!(checked >= 4, "only {checked} projects were checked");
}

#[test]
fn a_scene_without_the_field_loads_as_brushes() {
    let project = ProjectDocument::new("old");
    let text = project.to_ron_string().unwrap();
    assert!(!text.contains("world: Brushes") && !text.contains("world: Grid"));
    let loaded = ProjectDocument::from_ron_str(&text).unwrap();
    assert!(loaded.active_scene().world.is_empty());
}

#[test]
fn a_grid_scene_round_trips_and_names_its_kind() {
    let mut project = ProjectDocument::new("grid");
    project.active_scene_mut().world = SceneWorld::Grid(small_world());
    let text = project.to_ron_string().unwrap();
    assert!(text.contains("world: Grid(("), "grid world is written");
    let loaded = ProjectDocument::from_ron_str(&text).unwrap();
    assert_eq!(loaded, project);
    assert_eq!(
        loaded.active_scene().world.grid().unwrap().rooms[0].sector_count(),
        2
    );
    // Saving and loading again changes nothing.
    assert_eq!(loaded.to_ron_string().unwrap(), text);
}

#[test]
fn a_grid_project_does_not_trip_the_legacy_grid_guard() {
    let mut project = ProjectDocument::new("grid");
    project.active_scene_mut().world = SceneWorld::Grid(small_world());
    let text = project.to_ron_string().unwrap();
    for variant in ["Section(", "Room(", "Map(", "WaterVolume(", "Portal("] {
        assert!(!text.contains(&format!("kind: {variant}")), "{variant}");
    }
}

#[test]
fn default_values_stay_out_of_the_file() {
    let text = ron::to_string(&small_world()).unwrap();
    for absent in ["fog", "walkable", "solid", "uv", "heights_b", "tri_b"] {
        assert!(!text.contains(absent), "{absent} was written: {text}");
    }
    let sparse = r#"(rooms: [(width: 1, depth: 1, sectors: [Some((floor: Some((heights: (0, 0, 0, 0)))))])])"#;
    let world: GridWorld = ron::from_str(sparse).unwrap();
    assert_eq!(world.scale, super::units::GridScale::DEFAULT);
    let floor = world.rooms[0].sector(0, 0).unwrap().ground_floor().unwrap();
    assert!(floor.walkable);
    assert_eq!(floor.tri(0).material, None);
}

#[test]
fn planes_round_trip_in_a_project_and_old_rooms_load_without_them() {
    use super::plane::{GridPlane, PlaneCollision, PlaneUvMode};
    let mut world = small_world();
    assert!(!ron::to_string(&world).unwrap().contains("planes"));
    let mut plane = GridPlane::new([512, 256, 2048], [2048, 1024]);
    plane.rotation = [30, 90, 0];
    plane.collision = PlaneCollision::Walkable;
    plane.uv_mode = PlaneUvMode::Stretch;
    plane.double_sided = true;
    plane.fullbright = true;
    world.rooms[0].planes.push(plane.clone());
    let mut project = ProjectDocument::new("planes");
    project.active_scene_mut().world = SceneWorld::Grid(world);
    let text = project.to_ron_string().unwrap();
    let loaded = ProjectDocument::from_ron_str(&text).unwrap();
    assert_eq!(loaded, project);
    assert_eq!(
        loaded.active_scene().world.grid().unwrap().rooms[0].planes,
        vec![plane]
    );
    assert_eq!(loaded.to_ron_string().unwrap(), text);
}

#[test]
fn trimming_a_room_keeps_the_footprint_of_its_planes() {
    use super::plane::GridPlane;
    let mut room = GridRoom::new("r", 6, 6);
    room.sector_or_insert(1, 1)
        .unwrap()
        .set_ground_floor(Some(GridSurface::flat(0, None)));
    // A plane over sectors 3 to 4, 4 to 5.
    room.planes
        .push(GridPlane::new([4096, 0, 5120], [2048, 2048]));
    room.trim_empty();
    let (lo, hi) = room.planes[0].cell_span();
    assert!(
        room.local(lo).is_some() && room.local(hi).is_some(),
        "{room:?}"
    );
    assert_eq!((room.origin, room.width, room.depth), ([1, 1], 4, 5));
}

#[test]
fn legacy_levels_load_with_every_diagonal_locked_and_no_rule_set() {
    for name in [
        "grid-cathedral",
        "grid-level008",
        "grid-dungeon",
        "grid-cave",
        "grid-sewers",
        "grid-west",
    ] {
        let path = crate::projects_dir().join(name).join("project.ron");
        let project = ProjectDocument::load_from_path(path).expect("a reference project loads");
        let world = project.active_scene().world.grid().expect("a grid project");
        for room in &world.rooms {
            assert!(room.defaults.rules.is_default(), "{name}");
            for (_, _, sector) in room.iter() {
                for surface in [sector.ground_floor(), sector.ground_ceiling()]
                    .into_iter()
                    .flatten()
                {
                    assert!(
                        !surface.auto_split && !surface.paint_locked,
                        "{name}: a legacy split is deliberate and stays"
                    );
                }
            }
        }
    }
}
