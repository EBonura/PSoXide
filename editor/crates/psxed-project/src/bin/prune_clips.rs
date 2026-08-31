//! Remove named animation clips, their action bindings and their set
//! membership, then save the project.
//!
//! ```sh
//! cargo run -p psxed-project --bin prune_clips -- <project.ron> <clip name>... [--apply]
//! ```
//!
//! Round-tripping through `ProjectDocument` rather than editing the RON text
//! keeps every untouched field, including the level's brushes, exactly as the
//! editor wrote it.
use psxed_project::{ProjectDocument, ResourceData};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let apply = args.iter().any(|a| a == "--apply");
    args.retain(|a| a != "--apply");
    let path = args.remove(0);
    let wanted: Vec<String> = args;

    let mut project = ProjectDocument::load_from_path(&path).expect("load project");

    let doomed: Vec<_> = project
        .resources
        .iter()
        .filter(|r| matches!(r.data, ResourceData::AnimationClip(_)) && wanted.contains(&r.name))
        .map(|r| (r.id, r.name.clone()))
        .collect();
    for (id, name) in &doomed {
        println!("  removing clip {id:?}  {name}");
    }
    let ids: Vec<_> = doomed.iter().map(|(id, _)| *id).collect();

    let (mut unbound, mut orphaned) = (0, 0);
    for resource in &mut project.resources {
        match &mut resource.data {
            ResourceData::AnimationSet(set) => {
                let before = set.action_clips.len();
                set.action_clips.retain(|b| !ids.contains(&b.clip));
                unbound += before - set.action_clips.len();
                set.clips.retain(|c| !ids.contains(c));
                // A weapon appearance track is authored against an action. Once
                // that action has no clip the cook rejects the set outright, so
                // the track goes with the binding that gave it meaning.
                let live: Vec<usize> = set
                    .action_clips
                    .iter()
                    .map(|b| b.action.to_index())
                    .collect();
                let before_tracks = set.weapon_appearance_tracks.len();
                set.weapon_appearance_tracks
                    .retain(|t| live.contains(&t.action.to_index()));
                orphaned += before_tracks - set.weapon_appearance_tracks.len();
                if let Some(clip) = set.idle_clip.filter(|c| ids.contains(c)) {
                    panic!("refusing to strip the idle clip {clip:?}");
                }
            }
            ResourceData::Model(_) | ResourceData::Character(_) => {}
            _ => {}
        }
    }
    project.resources.retain(|r| !ids.contains(&r.id));

    println!(
        "\n{} clip(s) removed, {unbound} action binding(s) and {orphaned} orphaned weapon track(s) dropped{}",
        doomed.len(),
        if apply { "" } else { "  [dry run, pass --apply]" }
    );
    if apply {
        project.save_to_path(&path).expect("save project");
    }
}
