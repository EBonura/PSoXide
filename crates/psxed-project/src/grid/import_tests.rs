//! End-to-end tests of the bonnie-32 project import.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use super::bonnie_project::{import_project, ImportError, ImportOptions, FALLBACK_MATERIAL};
use super::limits;
use crate::{NodeKind, ProjectDocument, ResourceData};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "psxed-grid-import-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_png(path: &Path, width: u32, height: u32) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut image = image::RgbaImage::new(width, height);
    for (x, y, p) in image.enumerate_pixels_mut() {
        *p = image::Rgba([(x * 4) as u8, (y * 4) as u8, 90, 255]);
    }
    image.save(path).unwrap();
}

/// A 6x6 room with textured floors, one wall, one empty texture reference
/// and one texture whose file is missing. No player height, no lights.
fn level_text() -> String {
    let mut text = String::from(
        "(rooms: [(position: (x: 0.0, y: 0.0, z: 0.0), width: 6, depth: 6, ambient: 0.5, sectors: [",
    );
    for x in 0..6 {
        text.push('[');
        for z in 0..6 {
            let texture = match (x, z) {
                (0, 0) => "(pack: \"\", name: \"\")",
                (1, 1) => "(pack: \"a\", name: \"Gone\")",
                _ => "(pack: \"a\", name: \"Brick\")",
            };
            let wall = if (x, z) == (2, 2) {
                format!(
                    ", walls_north: [(heights: (0.0, 0.0, 1024.0, 1024.0), texture: {texture})]"
                )
            } else {
                String::new()
            };
            text.push_str(&format!(
                "Some((floor: Some((heights: (0.0, 0.0, 0.0, 0.0), texture: {texture})){wall})),"
            ));
        }
        text.push_str("],");
    }
    text.push_str("])])");
    text
}

fn setup(scratch: &Scratch) -> ImportOptions {
    let base = scratch.path("base");
    std::fs::create_dir_all(base.join("assets")).unwrap();
    let mut project = ProjectDocument::new("base");
    project
        .active_scene_mut()
        .add_node(crate::NodeId::ROOT, "Stray Prop", NodeKind::Node3D);
    project.save_to_path(base.join("project.ron")).unwrap();
    write_png(&scratch.path("packs/a/Brick.PNG"), 64, 64);
    ImportOptions {
        base_dir: base,
        packs_dir: scratch.path("packs"),
        out_dir: scratch.path("out"),
        name: "grid test".into(),
        scale: None,
        spawn: None,
        yaw_degrees: 90.0,
        enemies: 0,
    }
}

#[test]
fn a_level_becomes_a_loadable_grid_project() {
    let scratch = Scratch::new();
    let options = setup(&scratch);
    let report = import_project(level_text().as_bytes(), &options).expect("import");

    assert_eq!(report.stats.floors, 36);
    assert_eq!(report.stats.walls, 1);
    assert_eq!(report.stats.generated_lights, 1);
    assert_eq!(report.faces, 37);
    assert_eq!(report.scale, super::units::GridScale::DEFAULT);

    let project = ProjectDocument::load_from_path(&report.project_path).expect("load");
    assert_eq!(project.name, "grid test");
    let scene = project.active_scene();
    let world = scene.world.grid().expect("grid world");
    assert!(limits::validate(world).is_empty());
    assert!(scene.brushes.is_empty());
    // The base's stray node is gone; the generated light and a spawn are
    // ordinary nodes.
    assert!(!scene.nodes().iter().any(|n| n.name == "Stray Prop"));
    assert_eq!(
        scene
            .nodes()
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::PointLight { .. }))
            .count(),
        1
    );
    let spawn = scene
        .nodes()
        .iter()
        .find(|n| matches!(n.kind, NodeKind::SpawnPoint { player: true, .. }))
        .expect("player spawn");
    // Central floor cell of a 6x6 room: (2,2) or its neighbours, 1344 per
    // sector, centred, 64 above a floor at height 0.
    assert!((2688..=5376).contains(&(spawn.transform.translation[0] as i32)));
    assert_eq!(spawn.transform.translation[1] as i32, 64);
    assert_eq!(spawn.transform.rotation_degrees[1], 90.0);
    // The base's authored views are dropped so the editor frames the world.
    assert_eq!(project.editor_camera, crate::EditorCameraState::default());
    assert_eq!(
        project.editor_viewport,
        crate::EditorViewportState::default()
    );
}

#[test]
fn textures_resolve_per_pack_with_fallbacks_for_the_odd_ones() {
    let scratch = Scratch::new();
    let options = setup(&scratch);
    let report = import_project(level_text().as_bytes(), &options).unwrap();

    // Brick was found through the upper-case extension; the empty reference
    // and the missing file both use the checkerboard.
    assert_eq!(report.materials, 2);
    assert_eq!(report.substituted_textures.len(), 1);
    assert!(report.substituted_textures[0].is_empty());
    assert_eq!(report.missing_textures.len(), 1);
    assert_eq!(report.missing_textures[0].name, "Gone");

    let project = ProjectDocument::load_from_path(&report.project_path).unwrap();
    let names: Vec<&str> = project
        .resources
        .iter()
        .filter(|r| matches!(r.data, ResourceData::Material(_)))
        .map(|r| r.name.as_str())
        .collect();
    assert!(
        names.contains(&"Brick") && names.contains(&FALLBACK_MATERIAL),
        "{names:?}"
    );
    for resource in &project.resources {
        if let ResourceData::Material(material) = &resource.data {
            let path = material.psxt_path.as_deref().expect("converted texture");
            assert!(options.out_dir.join(path).is_file(), "{path}");
        }
    }
    // The base's assets are reachable through the output folder.
    assert!(options.out_dir.join("assets").is_dir());
    // Floors in the corner cells use the fallback material.
    let world = project.active_scene().world.grid().unwrap();
    let fallback = project
        .resources
        .iter()
        .find(|r| r.name == FALLBACK_MATERIAL)
        .unwrap()
        .id;
    let corner = world.rooms[0].sector(0, 0).unwrap().floor.as_ref().unwrap();
    assert_eq!(corner.tri(0).material, Some(fallback));
}

#[test]
fn an_explicit_spawn_overrides_the_default_and_must_have_a_floor() {
    let scratch = Scratch::new();
    let mut options = setup(&scratch);
    options.spawn = Some(super::bonnie_project::SpawnRequest {
        room: 0,
        cell: [5, 4],
    });
    let report = import_project(level_text().as_bytes(), &options).unwrap();
    assert_eq!(report.spawn, [5 * 1280 + 640, 64, 4 * 1280 + 640]);

    options.spawn = Some(super::bonnie_project::SpawnRequest {
        room: 3,
        cell: [0, 0],
    });
    assert!(matches!(
        import_project(level_text().as_bytes(), &options),
        Err(ImportError::BadSpawn(_))
    ));
}

#[test]
fn a_level_over_the_caps_is_refused_before_anything_is_saved() {
    let scratch = Scratch::new();
    let options = setup(&scratch);
    let wide = limits::MAX_ROOM_SIDE + 1;
    let mut text = format!(
        "(rooms: [(position: (x: 0.0, y: 0.0, z: 0.0), width: {wide}, depth: 1, sectors: ["
    );
    for _ in 0..wide {
        text.push_str("[None],");
    }
    text.push_str("])])");
    assert!(matches!(
        import_project(text.as_bytes(), &options),
        Err(ImportError::Invalid(_))
    ));
    assert!(!options.out_dir.join("project.ron").exists());
}

/// The real sample levels, when the bonnie-32 checkout is present.
#[test]
fn the_sample_levels_import_when_bonnie_is_checked_out() {
    let repo = Path::new("/Users/ebonura/Desktop/repos/bonnie-32/assets/samples");
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../projects/graybox-reach");
    if !repo.is_dir() || !base.join("project.ron").is_file() {
        eprintln!("bonnie-32 samples not present, skipping");
        return;
    }
    for level in [
        "Cathedral",
        "level_008",
        "Dungeon",
        "Cave",
        "Sewers",
        "West",
    ] {
        let scratch = Scratch::new();
        let options = ImportOptions {
            base_dir: base.clone(),
            packs_dir: repo.join("texture-packs"),
            out_dir: scratch.path("out"),
            name: level.into(),
            scale: None,
            spawn: None,
            yaw_degrees: 0.0,
            enemies: 0,
        };
        let bytes = std::fs::read(repo.join("levels").join(format!("{level}.ron"))).unwrap();
        let report = import_project(&bytes, &options).unwrap_or_else(|e| panic!("{level}: {e}"));
        assert!(report.faces > 50, "{level}: {} faces", report.faces);
        assert!(
            report.stats.level_lights + report.stats.generated_lights > 0,
            "{level}"
        );
        let project = ProjectDocument::load_from_path(&report.project_path).unwrap();
        let world = project.active_scene().world.grid().unwrap();
        assert!(!limits::has_errors(&limits::validate(world)), "{level}");
    }
}
