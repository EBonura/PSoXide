//! Build a probe project for measuring what an enemy costs: the lever-and-doors
//! level with no enemy, one light enemy, one heavy enemy, or both, placed by
//! character name from the base project's Character resources.
//!
//! ```text
//! cargo run -p psxed-project --example enemy_probe -- \
//!     <out-dir> <base-dir> <none|light|heavy|both>
//! ```
//!
//! The enemies are placed with no per-placement settings, so each takes what
//! its Character resource says.

use std::path::PathBuf;

use psxed_project::grid::logic_demo::into_lever_doors;
use psxed_project::{NodeId, NodeKind, ProjectDocument, ResourceData, ResourceId};

fn character(project: &ProjectDocument, name: &str) -> Option<ResourceId> {
    project
        .resources
        .iter()
        .find(|r| r.name == name && matches!(r.data, ResourceData::Character(_)))
        .map(|r| r.id)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [out, base, mode] = args.as_slice() else {
        eprintln!("usage: enemy_probe <out-dir> <base-dir> <none|light|heavy|both>");
        std::process::exit(2);
    };
    let (out, base) = (PathBuf::from(out), PathBuf::from(base));
    let mut project =
        ProjectDocument::load_from_path(base.join("project.ron")).expect("load the base project");
    let light = character(&project, "Light Enemy").expect("a Light Enemy character");
    let heavy = character(&project, "Heavy Enemy").expect("a Heavy Enemy character");
    into_lever_doors(&mut project).expect("build the level");

    let wanted: Vec<(&str, ResourceId)> = match mode.as_str() {
        "none" => vec![],
        "light" => vec![("Light Enemy", light)],
        "heavy" => vec![("Heavy Enemy", heavy)],
        "both" => vec![("Light Enemy", light), ("Heavy Enemy", heavy)],
        other => panic!("unknown mode {other}"),
    };
    let scene = project.active_scene_mut();
    let player = scene
        .nodes()
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Entity) && n.name.contains("Player"))
        .map(|n| n.transform.translation)
        .unwrap_or([320.0, 64.0, 1920.0]);
    for (index, (name, id)) in wanted.iter().enumerate() {
        let host = scene.add_node(NodeId::ROOT, format!("{name} probe"), NodeKind::Entity);
        if let Some(node) = scene.node_mut(host) {
            node.transform.translation = [
                player[0] + 3.0 * 1280.0,
                player[1],
                player[2] + index as f32 * 640.0,
            ];
            node.transform.rotation_degrees = [0.0, 90.0, 0.0];
        }
        scene.add_node(
            host,
            "Character Controller",
            NodeKind::CharacterController {
                character: Some(*id),
                loadout: None,
                settings: None,
                player: false,
            },
        );
    }
    std::fs::create_dir_all(&out).expect("create out");
    let link = out.join("assets");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(std::fs::canonicalize(&base).unwrap().join("assets"), &link)
        .expect("link assets");
    project.name = format!("Enemy probe {mode}");
    project
        .save_to_path(out.join("project.ron"))
        .expect("save project");
    println!("{}", out.join("project.ron").display());
}
