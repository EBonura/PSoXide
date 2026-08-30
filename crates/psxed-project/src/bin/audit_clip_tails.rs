//! Report animation frames that ship but are never referenced.
//!
//! ```sh
//! cargo run -p psxed-project --bin audit_clip_tails -- <project.ron>
//! ```
//!
//! Narrowing an action's range in the Animation Studio writes `frame_start` and
//! `frame_end` onto the binding in `project.ron`. The `.psxanim` keeps every
//! frame it was imported with, and the cook embeds the whole file, so a clip
//! authored down to fifty frames still costs the guest its original length.
//!
//! This reports the tail past the last frame anything references. Trimming a
//! tail needs no index rebasing, because every authored frame number stays
//! where it is; trimming a head would shift all of them and is deliberately
//! not considered here.
//!
//! A clip is only reported when *every* consumer states an explicit bound. One
//! binding left at full length means the whole clip is required.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use psxed_project::{
    CombatCapsuleRole, ProjectDocument, ResourceData, ResourceId, ACTION_FRAME_END_FULL,
};

/// Highest frame index a clip must keep, or `None` when something needs it whole.
type Requirement = Option<u16>;

fn require(current: &mut HashMap<ResourceId, Requirement>, clip: ResourceId, frame: Option<u16>) {
    let slot = current.entry(clip).or_insert(Some(0));
    match (slot.as_mut(), frame) {
        // Once any consumer needs the full clip the answer cannot go back.
        (_, None) => *slot = None,
        (None, _) => {}
        (Some(highest), Some(frame)) => *highest = (*highest).max(frame),
    }
}

fn main() {
    let path = PathBuf::from(std::env::args().nth(1).expect("project.ron"));
    let root = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let project = ProjectDocument::load_from_path(&path).expect("load project");

    let mut needed: HashMap<ResourceId, Requirement> = HashMap::new();

    for resource in &project.resources {
        let ResourceData::AnimationSet(set) = &resource.data else {
            continue;
        };

        // Every action binding: its own window, plus the root-push window,
        // which is authored in the same frame space.
        let mut action_clip: Vec<(usize, ResourceId)> = Vec::new();
        for binding in &set.action_clips {
            action_clip.push((binding.action.to_index(), binding.clip));
            let bound = binding.options.map(|options| {
                if options.frame_end == ACTION_FRAME_END_FULL {
                    None
                } else {
                    Some(options.frame_end.max(options.push_frame_end.min(u16::MAX - 1)))
                }
            });
            require(&mut needed, binding.clip, bound.flatten());
            if bound.is_none() {
                require(&mut needed, binding.clip, None);
            }
        }

        // Weapon appearance beats and sword trails are authored per action and
        // index the same clip.
        for track in &set.weapon_appearance_tracks {
            let Some(&(_, clip)) = action_clip
                .iter()
                .find(|(index, _)| *index == track.action.to_index())
            else {
                continue;
            };
            let mut highest = track.hidden_frame.max(track.fully_visible_frame);
            highest = highest.saturating_add(track.transition_frames);
            if let Some(trail) = &track.trail {
                highest = highest.max(trail.end_frame.saturating_add(trail.history_frames));
            }
            require(&mut needed, clip, Some(highest));
        }

        // Hitboxes and projectile emitters live on the characters that use the
        // set, and their active windows are frame indices into the action clip.
        for character in &project.resources {
            let ResourceData::Character(character) = &character.data else {
                continue;
            };
            if character.animation_set != Some(resource.id) {
                continue;
            }
            for capsule in &character.combat_capsules {
                let (action, last) = match capsule.role {
                    CombatCapsuleRole::Hurtbox => continue,
                    CombatCapsuleRole::Hitbox {
                        action,
                        active_end_frame,
                        ..
                    } => (action, active_end_frame),
                    CombatCapsuleRole::ProjectileEmitter {
                        action,
                        active_end_frame,
                        ..
                    } => (action, active_end_frame),
                };
                if let Some(&(_, clip)) = action_clip
                    .iter()
                    .find(|(index, _)| *index == action.to_index())
                {
                    require(&mut needed, clip, Some(last));
                }
            }
        }
    }

    let mut rows = Vec::new();
    let (mut savings, mut whole) = (0usize, 0usize);
    for (clip, requirement) in &needed {
        let Some(resource) = project.resource(*clip) else {
            continue;
        };
        let ResourceData::AnimationClip(clip_resource) = &resource.data else {
            continue;
        };
        let Ok(bytes) = std::fs::read(root.join(&clip_resource.psxanim_path)) else {
            continue;
        };
        if bytes.len() < 20 || bytes[..4] != *b"PSXA" {
            continue;
        }
        let joints = u16::from_le_bytes([bytes[12], bytes[13]]) as usize;
        let frames = u16::from_le_bytes([bytes[14], bytes[15]]) as usize;
        let Some(highest) = requirement else {
            whole += 1;
            continue;
        };
        let keep = (*highest as usize).saturating_add(1).min(frames);
        if keep >= frames {
            continue;
        }
        let record = if u16::from_le_bytes([bytes[4], bytes[5]]) == 4 {
            16
        } else {
            20
        };
        let saved = (frames - keep) * joints * record;
        savings += saved;
        rows.push((saved, resource.name.clone(), frames, keep));
    }

    rows.sort_by(|a, b| b.0.cmp(&a.0));
    println!("{:>8}  {:>12}  clip", "bytes", "frames");
    for (saved, name, frames, keep) in &rows {
        println!("{saved:>8}  {frames:>5} -> {keep:<4}  {name}");
    }
    println!(
        "\n{} clip(s) trimmable, {savings} bytes ({:.1} KiB); {whole} clip(s) needed whole",
        rows.len(),
        savings as f64 / 1024.0,
    );
}
