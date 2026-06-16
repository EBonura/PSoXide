//! Load a project.ron and print a resource summary (round-trip check).
use psxed_project::{ProjectDocument, ResourceData};

fn main() {
    let path = std::env::args().nth(1).expect("usage: load_check <project.ron>");
    let project = ProjectDocument::load_from_path(&path).expect("project failed to parse");
    let (mut models, mut chars, mut mats, mut clips, mut other) = (0, 0, 0, 0, 0);
    for r in &project.resources {
        match &r.data {
            ResourceData::Model(_) => { models += 1; println!("  Model     {}", r.name); }
            ResourceData::Character(_) => { chars += 1; println!("  Character {}", r.name); }
            ResourceData::Material(_) => mats += 1,
            ResourceData::AnimationClip(_) => clips += 1,
            _ => other += 1,
        }
    }
    println!("OK: {} resources ({} models, {} characters, {} materials, {} clips, {} other)",
        project.resources.len(), models, chars, mats, clips, other);
}
