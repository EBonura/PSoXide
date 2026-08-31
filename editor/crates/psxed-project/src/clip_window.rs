//! Which frames of an animation clip the project actually references.
//!
//! Narrowing an action's range in the Animation Studio writes `frame_start`
//! and `frame_end` onto the binding. The `.psxanim` keeps every frame it was
//! imported with and the cook embeds the whole file, so a clip authored down
//! to fifty frames still costs the guest its original length.
//!
//! This computes, per clip, the smallest frame window that still contains
//! everything anything references. The cook trims the pose table to that
//! window and rebases the frame indices it emits, so the runtime never sees
//! the difference.
//!
//! Two rules keep it safe:
//!
//! - a clip shared by several actions keeps the union of their windows, so one
//!   action can never cut frames another still plays;
//! - a single consumer left at full length pins the whole clip, because an
//!   unbounded window cannot be proven to end anywhere.

use std::collections::HashMap;

use crate::{
    CharacterAnimationAction, CombatCapsuleRole, ProjectDocument, ResourceData, ResourceId,
    ACTION_FRAME_END_FULL,
};

/// The frames of one clip the project still needs, inclusive on both ends.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ClipWindow {
    /// First referenced frame. The cook drops everything before this and
    /// subtracts it from every frame index it emits for the clip.
    pub start: u16,
    /// Last referenced frame, or `None` when a consumer plays to the clip's
    /// own end and the tail cannot be shortened.
    pub end: Option<u16>,
}

impl ClipWindow {
    /// Last frame to keep, given how long the clip actually is.
    pub fn end_within(&self, frame_count: u16) -> u16 {
        self.end
            .unwrap_or_else(|| frame_count.saturating_sub(1))
            .max(self.start)
    }

    /// Whether this window would drop anything from a clip of `frame_count`.
    pub fn trims(&self, frame_count: u16) -> bool {
        self.start > 0 || self.end_within(frame_count) + 1 < frame_count
    }
}

/// Widen `slot` to include frames `low..=high`, where `high` of `None` means
/// the consumer plays to the clip's own end.
fn widen(slot: &mut Option<(u16, Option<u16>)>, low: u16, high: Option<u16>) {
    match slot {
        None => *slot = Some((low, high)),
        Some((current_low, current_high)) => {
            *current_low = (*current_low).min(low);
            // Once anything runs to the end, the tail stays.
            *current_high = match (*current_high, high) {
                (None, _) | (_, None) => None,
                (Some(a), Some(b)) => Some(a.max(b)),
            };
        }
    }
}

/// Compute the referenced window of every clip the project binds to an action.
///
/// A clip absent from the result is never bound to an action and is left
/// alone; a clip mapped to `None` is needed whole.
pub fn clip_windows(project: &ProjectDocument) -> HashMap<ResourceId, ClipWindow> {
    let mut windows: HashMap<ResourceId, Option<(u16, Option<u16>)>> = HashMap::new();

    for resource in &project.resources {
        let ResourceData::AnimationSet(set) = &resource.data else {
            continue;
        };

        // Action index to clip, so the per-action authored beats below can find
        // the clip they index into. CharacterAnimationAction is not hashable.
        let action_clip: Vec<(usize, ResourceId)> = set
            .action_clips
            .iter()
            .map(|binding| (binding.action.to_index(), binding.clip))
            .collect();
        let clip_for = |action: CharacterAnimationAction| -> Option<ResourceId> {
            action_clip
                .iter()
                .find(|(index, _)| *index == action.to_index())
                .map(|(_, clip)| *clip)
        };

        for binding in &set.action_clips {
            let slot = windows.entry(binding.clip).or_default();
            let Some(options) = binding.options else {
                // No options at all means the whole clip plays.
                widen(slot, 0, None);
                continue;
            };
            // The root push is authored in the same frame space as the action,
            // but only when it is a real window. A push end left at
            // ACTION_FRAME_END_FULL means no push was authored at all, and
            // reading it as "runs to the clip's end" would unpin the tail of
            // every clip that simply has no root motion.
            let pushes = options.push_frame_end != ACTION_FRAME_END_FULL;
            let low = if pushes {
                options.frame_start.min(options.push_frame_start)
            } else {
                options.frame_start
            };
            let high = if options.frame_end == ACTION_FRAME_END_FULL {
                None
            } else if pushes {
                Some(options.frame_end.max(options.push_frame_end).max(low))
            } else {
                Some(options.frame_end.max(low))
            };
            widen(slot, low, high);
        }

        // Weapon visibility beats and sword trails index the action's clip.
        for track in &set.weapon_appearance_tracks {
            let Some(clip) = clip_for(track.action) else {
                continue;
            };
            let mut low = track.fully_visible_frame.min(track.hidden_frame);
            let mut high = track
                .fully_visible_frame
                .max(track.hidden_frame)
                .saturating_add(track.transition_frames);
            if let Some(trail) = &track.trail {
                low = low.min(trail.start_frame);
                high = high.max(trail.end_frame.saturating_add(trail.history_frames));
            }
            widen(windows.entry(clip).or_default(), low, Some(high));
        }

        // Hitboxes and emitters live on the characters using this set.
        for other in &project.resources {
            let ResourceData::Character(character) = &other.data else {
                continue;
            };
            if character.animation_set != Some(resource.id) {
                continue;
            }
            for volume in &character.combat_capsules {
                let (action, low, high) = match volume.role {
                    CombatCapsuleRole::Hurtbox => continue,
                    CombatCapsuleRole::Hitbox {
                        action,
                        active_start_frame,
                        active_end_frame,
                        ..
                    } => (action, active_start_frame, active_end_frame),
                    CombatCapsuleRole::ProjectileEmitter {
                        action,
                        charge_start_frame,
                        active_start_frame,
                        active_end_frame,
                        ..
                    } => (
                        action,
                        charge_start_frame.min(active_start_frame),
                        active_end_frame,
                    ),
                };
                let Some(clip) = clip_for(action) else {
                    continue;
                };
                widen(windows.entry(clip).or_default(), low, Some(high.max(low)));
            }
        }
    }

    windows
        .into_iter()
        .filter_map(|(clip, slot)| {
            let (start, end) = slot?;
            Some((clip, ClipWindow { start, end }))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_covers_every_authored_beat() {
        let mut slot = None;
        widen(&mut slot, 10, Some(20));
        widen(&mut slot, 4, Some(12));
        assert_eq!(slot, Some((4, Some(20))), "the union, not the last writer");
    }

    #[test]
    fn playing_to_the_end_pins_the_tail_but_not_the_head() {
        // This is Aletha's idle: authored to start at 84 and run to the clip's
        // own end. Frames 0..83 never play and are exactly what is worth
        // dropping, so a full end must not pin the whole clip.
        let mut slot = None;
        widen(&mut slot, 84, None);
        assert_eq!(slot, Some((84, None)));

        let window = ClipWindow {
            start: 84,
            end: None,
        };
        assert_eq!(window.end_within(170), 169, "the tail runs to the last frame");
        assert!(window.trims(170), "but the first 84 frames still go");
    }

    #[test]
    fn the_head_is_the_earliest_frame_any_consumer_needs() {
        let mut slot = None;
        widen(&mut slot, 84, None);
        widen(&mut slot, 12, Some(30));
        assert_eq!(
            slot,
            Some((12, None)),
            "an earlier consumer pulls the head back and the open end stays open"
        );
    }

    #[test]
    fn a_window_over_the_whole_clip_trims_nothing() {
        let window = ClipWindow {
            start: 0,
            end: None,
        };
        assert!(!window.trims(170));
        assert_eq!(window.end_within(170), 169);
    }
}

/// Per-clip trim decided once for a cook, and the frame offset each clip's
/// authored indices must be shifted by.
///
/// The cook trims a clip's pose table to its window and then emits every frame
/// index for that clip minus the window start, so the runtime plays a shorter
/// clip with indices that still line up. Keeping the offset behind one lookup
/// means an emit site can never forget to rebase and silently mistime combat.
#[derive(Debug, Default, Clone)]
pub struct ClipTrim {
    windows: HashMap<ResourceId, ClipWindow>,
}

impl ClipTrim {
    /// Decide the trim for every clip in a project.
    pub fn for_project(project: &ProjectDocument) -> Self {
        Self {
            windows: clip_windows(project),
        }
    }

    /// A trim that changes nothing, for callers that cook without a project.
    pub fn disabled() -> Self {
        Self::default()
    }

    /// The window a clip is trimmed to, if it is trimmed at all.
    pub fn window(&self, clip: ResourceId) -> Option<ClipWindow> {
        self.windows.get(&clip).copied()
    }

    /// Frames dropped from the front of `clip`, which every frame index
    /// authored against it must be reduced by.
    pub fn offset(&self, clip: ResourceId) -> u16 {
        self.window(clip).map(|window| window.start).unwrap_or(0)
    }

    /// Rebase one authored frame index into the trimmed clip's frame space.
    pub fn rebase(&self, clip: ResourceId, frame: u16) -> u16 {
        frame.saturating_sub(self.offset(clip))
    }

    /// The clip an action plays, so an emit site can find the right offset.
    pub fn action_clip(
        project: &ProjectDocument,
        animation_set: Option<ResourceId>,
        action: CharacterAnimationAction,
    ) -> Option<ResourceId> {
        let ResourceData::AnimationSet(set) = &project.resource(animation_set?)?.data else {
            return None;
        };
        set.action_clips
            .iter()
            .find(|binding| binding.action.to_index() == action.to_index())
            .map(|binding| binding.clip)
    }
}

impl ClipTrim {
    /// Return a copy of `project` with every authored frame index moved into
    /// the trimmed clips' frame space.
    ///
    /// Rebasing the document once, before cooking, keeps the trim out of the
    /// cook's combat-timing paths entirely: every emit site keeps reading the
    /// field it always read, and none of them can forget to subtract. The only
    /// other change a cook needs is to trim the clip's pose table to the
    /// matching window.
    ///
    /// [`ACTION_FRAME_END_FULL`] is a sentinel, not a frame, and is preserved.
    /// Such a binding pins its clip anyway, so its offset is zero regardless.
    pub fn rebase_project(&self, project: &ProjectDocument) -> ProjectDocument {
        let mut rebased = project.clone();
        // Resolve each character's action clips against the original document
        // so the lookup cannot see half-rebased data.
        let sets: Vec<(ResourceId, Option<ResourceId>)> = project
            .resources
            .iter()
            .filter_map(|resource| match &resource.data {
                ResourceData::Character(character) => {
                    Some((resource.id, character.animation_set))
                }
                _ => None,
            })
            .collect();

        for resource in &mut rebased.resources {
            match &mut resource.data {
                ResourceData::AnimationSet(set) => {
                    let clips: Vec<ResourceId> =
                        set.action_clips.iter().map(|b| b.clip).collect();
                    let actions: Vec<usize> = set
                        .action_clips
                        .iter()
                        .map(|b| b.action.to_index())
                        .collect();
                    for (binding, clip) in set.action_clips.iter_mut().zip(clips.iter()) {
                        let offset = self.offset(*clip);
                        if offset == 0 {
                            continue;
                        }
                        if let Some(options) = binding.options.as_mut() {
                            options.frame_start = options.frame_start.saturating_sub(offset);
                            if options.frame_end != ACTION_FRAME_END_FULL {
                                options.frame_end = options.frame_end.saturating_sub(offset);
                            }
                            options.push_frame_start =
                                options.push_frame_start.saturating_sub(offset);
                            if options.push_frame_end != ACTION_FRAME_END_FULL {
                                options.push_frame_end =
                                    options.push_frame_end.saturating_sub(offset);
                            }
                        }
                    }
                    for track in set.weapon_appearance_tracks.iter_mut() {
                        let Some(position) = actions
                            .iter()
                            .position(|index| *index == track.action.to_index())
                        else {
                            continue;
                        };
                        let offset = self.offset(clips[position]);
                        if offset == 0 {
                            continue;
                        }
                        track.fully_visible_frame =
                            track.fully_visible_frame.saturating_sub(offset);
                        track.hidden_frame = track.hidden_frame.saturating_sub(offset);
                        if let Some(trail) = track.trail.as_mut() {
                            trail.start_frame = trail.start_frame.saturating_sub(offset);
                            trail.end_frame = trail.end_frame.saturating_sub(offset);
                        }
                    }
                }
                ResourceData::Character(character) => {
                    let animation_set = sets
                        .iter()
                        .find(|(id, _)| *id == resource.id)
                        .and_then(|(_, set)| *set);
                    for volume in character.combat_capsules.iter_mut() {
                        let action = match volume.role {
                            CombatCapsuleRole::Hurtbox => continue,
                            CombatCapsuleRole::Hitbox { action, .. }
                            | CombatCapsuleRole::ProjectileEmitter { action, .. } => action,
                        };
                        let Some(clip) = Self::action_clip(project, animation_set, action) else {
                            continue;
                        };
                        let offset = self.offset(clip);
                        if offset == 0 {
                            continue;
                        }
                        match &mut volume.role {
                            CombatCapsuleRole::Hurtbox => {}
                            CombatCapsuleRole::Hitbox {
                                active_start_frame,
                                active_end_frame,
                                ..
                            } => {
                                *active_start_frame = active_start_frame.saturating_sub(offset);
                                *active_end_frame = active_end_frame.saturating_sub(offset);
                            }
                            CombatCapsuleRole::ProjectileEmitter {
                                charge_start_frame,
                                active_start_frame,
                                active_end_frame,
                                ..
                            } => {
                                *charge_start_frame = charge_start_frame.saturating_sub(offset);
                                *active_start_frame = active_start_frame.saturating_sub(offset);
                                *active_end_frame = active_end_frame.saturating_sub(offset);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        rebased
    }
}
