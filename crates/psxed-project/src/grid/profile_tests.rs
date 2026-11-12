//! The standard world profile: every path that creates a world gives it the
//! profile, and a world stored without the settings still loads with them off.

use super::light::FacingShade;
use super::lighting::GlobalLight;
use super::logic_demo::lever_doors_world;
use super::starter::{
    attach_starter_world, create_project, into_starter, starter_world, STARTER_BASE,
};
use super::{GridWorld, SceneWorld, StreamRadii, WorldProfile};
use crate::ProjectDocument;

fn assert_standard(world: &GridWorld, path: &str) {
    let p = &WorldProfile::STANDARD;
    // Far panels are the one field a project path sets from its sky.
    let mut expected = p.stream;
    expected.flood.panels = world.stream.flood.panels;
    assert_eq!(world.shading, p.shading, "{path}: shading");
    assert_eq!(world.global_light, p.global_light, "{path}: global light");
    assert_eq!(world.stream, expected, "{path}: stream");
    assert!(world.stream.flood.on, "{path}: flood streaming is on");
    assert!(world.global_light.enabled, "{path}: global light is on");
    assert!(!world.shading.is_off(), "{path}: facing shade is on");
}

#[test]
fn the_profile_is_the_arch_study_look_on_the_citadel_stream() {
    let p = WorldProfile::STANDARD;
    assert_eq!(p.shading, FacingShade::STRONG);
    assert_eq!(
        (
            p.shading.up,
            p.shading.down,
            p.shading.side_x,
            p.shading.side_z
        ),
        (256, 168, 180, 124)
    );
    assert_eq!(
        p.global_light,
        GlobalLight {
            enabled: true,
            occlusion: 52,
            ..GlobalLight::DEFAULT
        }
    );
    let s = p.stream;
    assert_eq!(
        (s.draw_far, s.active_radius, s.lead_radius, s.ring),
        (12, 8, 4, true)
    );
    assert_eq!(
        (s.flood.on, s.flood.near_radius, s.flood.res_radius),
        (true, 12, 13)
    );
    assert!(s.flood.res_radius >= s.flood.near_radius);
}

#[test]
fn a_new_world_from_each_creation_path_has_the_profile() {
    assert_standard(&GridWorld::standard(), "GridWorld::standard");
    assert_standard(&starter_world(None, None, None), "starter_world");
    assert_standard(&lever_doors_world(None, None, None), "lever_doors_world");

    let mut attached = ProjectDocument::new("attach");
    attach_starter_world(&mut attached);
    assert_standard(
        attached.active_scene().world.grid().unwrap(),
        "attach_starter_world",
    );

    let starter = ProjectDocument::starter();
    assert_standard(
        starter.active_scene().world.grid().unwrap(),
        "ProjectDocument::starter",
    );
}

#[test]
fn the_new_project_flow_writes_a_world_with_the_profile() {
    let base = crate::projects_dir().join(STARTER_BASE);
    if !base.join("project.ron").is_file() {
        eprintln!("skipped: {} is missing", base.display());
        return;
    }
    let out = std::env::temp_dir().join(format!("psxed-profile-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    let path = create_project(&base, &out, "Profile Test").unwrap();
    let loaded = ProjectDocument::load_from_path(&path).unwrap();
    assert_standard(
        loaded.active_scene().world.grid().unwrap(),
        "create_project, reloaded",
    );

    let mut project = ProjectDocument::load_from_path(base.join("project.ron")).unwrap();
    into_starter(&mut project).unwrap();
    assert_standard(project.active_scene().world.grid().unwrap(), "into_starter");
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn the_empty_world_and_the_serde_defaults_stay_off() {
    let empty = GridWorld::new();
    assert!(empty.shading.is_off());
    assert!(!empty.global_light.enabled);
    assert_eq!(empty.stream, StreamRadii::DEFAULT);
    assert!(!empty.stream.flood.on);
}

#[test]
fn a_world_stored_without_the_settings_loads_with_them_off_and_round_trips() {
    let text = "(scale: (num: 5, den: 4), rooms: [])";
    let world: GridWorld = ron::from_str(text).unwrap();
    assert!(world.shading.is_off());
    assert_eq!(world.global_light, GlobalLight::DEFAULT);
    assert!(!world.global_light.enabled);
    assert_eq!(world.stream, StreamRadii::DEFAULT);
    assert!(!world.stream.flood.on);

    // Saving it again does not write the settings, so the file does not drift.
    let saved = ron::to_string(&world).unwrap();
    for key in ["shading", "global_light", "stream"] {
        assert!(!saved.contains(key), "{key} appeared in {saved}");
    }
    let again: GridWorld = ron::from_str(&saved).unwrap();
    assert_eq!(again, world);

    // A scene world holding it behaves the same.
    let scene: SceneWorld = ron::from_str(&format!("Grid({text})")).unwrap();
    assert_eq!(scene.grid().unwrap(), &world);

    // The profile does write, and reads back.
    let standard = GridWorld::standard();
    let back: GridWorld = ron::from_str(&ron::to_string(&standard).unwrap()).unwrap();
    assert_eq!(back, standard);
}

#[test]
fn a_new_project_with_the_profile_cooks() {
    let base = crate::projects_dir().join(STARTER_BASE);
    if !base.join("project.ron").is_file() {
        eprintln!("skipped: {} is missing", base.display());
        return;
    }
    let mut project = ProjectDocument::load_from_path(base.join("project.ron")).unwrap();
    into_starter(&mut project).unwrap();
    let (package, report) = crate::playtest::build_package(&project, &base);
    assert!(report.is_ok(), "starter cook: {:?}", report.errors);
    assert!(package.is_some());
}

#[test]
fn panels_follow_the_projects_sky() {
    use crate::SkyMode;
    let mut project = ProjectDocument::starter();
    let panels = |p: &ProjectDocument| p.active_scene().world.grid().unwrap().stream.flood.panels;
    let sky_mode = |p: &mut ProjectDocument, mode: SkyMode| {
        let scene = p.active_scene_mut();
        let root = scene.root;
        if let Some(node) = scene.node_mut(root) {
            if let crate::NodeKind::World { sky, .. } = &mut node.kind {
                sky.mode = mode;
            }
        }
    };
    sky_mode(&mut project, SkyMode::Cube);
    attach_starter_world(&mut project);
    assert_eq!(panels(&project), WorldProfile::PANELS);
    sky_mode(&mut project, SkyMode::Panorama);
    attach_starter_world(&mut project);
    assert_eq!(panels(&project), 0, "no cube sky, no panels");
    assert_standard(project.active_scene().world.grid().unwrap(), "no cube sky");
}
