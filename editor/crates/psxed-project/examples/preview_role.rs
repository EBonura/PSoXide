//! Point the AnimationSet's idle slot at an existing clip by name, so the
//! standing headless render shows that clip. Fast: no re-import.
//!   cargo run -p psxed-project --example preview_role -- <project.ron> <clip name>

use std::path::PathBuf;
use psxed_project::{ProjectDocument, ResourceData};

fn main() {
    let project = PathBuf::from(std::env::args().nth(1).expect("usage: <project.ron> <clip>"));
    let want = std::env::args().nth(2).expect("clip name");
    let mut doc = ProjectDocument::load_from_path(&project).unwrap();
    let clip = doc
        .resources
        .iter()
        .find(|r| matches!(r.data, ResourceData::AnimationClip(_)) && r.name == want)
        .map(|r| r.id)
        .unwrap_or_else(|| panic!("no clip named {want}"));
    for r in &mut doc.resources {
        if let ResourceData::AnimationSet(set) = &mut r.data {
            set.idle_clip = Some(clip);
            set.walk_clip = Some(clip);
        }
    }
    doc.save_to_path(&project).unwrap();
    println!("idle/walk slot -> {want} ({clip:?})");
}
