//! Backfill captured source bone names onto skeletons imported before
//! joint-name capture existed: re-cooks each Model's original source
//! (names only; cooked artifacts on disk are untouched) and writes the
//! names onto the model's skeleton when the joint counts line up.
//! Skeletons that already carry names are left alone, so a shared
//! skeleton is named by the first model that resolves.
//!
//! Usage:
//!   cargo run -p psxed-project --example backfill_joint_names -- <project.ron>

use std::path::PathBuf;

use psxed_project::model_import::{preview_model_with_animation_sources, resolve_path};
use psxed_project::{ProjectDocument, ResourceData, ResourceId};

fn main() {
    let project_path = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: backfill_joint_names <project.ron>"),
    );
    let root = project_path
        .parent()
        .expect("project root")
        .to_path_buf();
    let mut project = ProjectDocument::load_from_path(&project_path).expect("load project.ron");

    let backup = root.join("logs").join("project.ron.pre-jointnames.bak");
    if !backup.exists() {
        std::fs::create_dir_all(backup.parent().unwrap()).expect("logs dir");
        std::fs::copy(&project_path, &backup).expect("backup project.ron");
        println!("backup: {}", backup.display());
    }

    let candidates: Vec<(String, PathBuf, ResourceId)> = project
        .resources
        .iter()
        .filter_map(|resource| {
            let ResourceData::Model(model) = &resource.data else {
                return None;
            };
            let source = model.source_path.as_ref()?;
            let skeleton = model.skeleton?;
            Some((
                resource.name.clone(),
                resolve_path(source, Some(&root)),
                skeleton,
            ))
        })
        .collect();

    let mut updated = 0usize;
    for (name, source, skeleton_id) in candidates {
        let already_named = matches!(
            project.resource(skeleton_id).map(|resource| &resource.data),
            Some(ResourceData::Skeleton(skeleton)) if !skeleton.joint_names.is_empty()
        );
        if already_named {
            continue;
        }
        if !source.exists() {
            println!("{name}: source missing ({}), skipped", source.display());
            continue;
        }
        // Default config: names only depend on the collapse patterns,
        // which every import so far used at their defaults.
        let package = match preview_model_with_animation_sources(&source, &[], Default::default())
        {
            Ok(package) => package,
            Err(error) => {
                println!("{name}: cook failed ({error}), skipped");
                continue;
            }
        };
        let Some(resource) = project.resource_mut(skeleton_id) else {
            continue;
        };
        let ResourceData::Skeleton(skeleton) = &mut resource.data else {
            continue;
        };
        if skeleton.joint_count as usize != package.joint_names.len() {
            println!(
                "{name}: source cooked {} joints vs skeleton {} (import config differs?), skipped",
                package.joint_names.len(),
                skeleton.joint_count
            );
            continue;
        }
        skeleton.joint_names = package.joint_names;
        println!("{name}: named {} joints", skeleton.joint_count);
        updated += 1;
    }
    if updated > 0 {
        project.save_to_path(&project_path).expect("save project.ron");
        println!("saved {}", project_path.display());
    } else {
        println!("nothing to update");
    }
}
