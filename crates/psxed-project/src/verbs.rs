//! The combat verb contract (`psx_level::combat_verbs`) over authored
//! project data: which verbs a Character binds through its Animation Set,
//! the same answer the cook gives. The editor's coverage table and the
//! `verb_coverage` example read it from here.

use psx_level::combat_verbs::VerbBindings;

use crate::{AnimationSetResource, CharacterAnimationAction, CharacterResource, ProjectDocument};

impl CharacterResource {
    /// The action this Character shoots from: the action its projectile
    /// emitter is authored on, else [`CharacterAnimationAction::RangedAttack`].
    pub fn shoot_action(&self) -> CharacterAnimationAction {
        self.combat_capsules
            .iter()
            .find_map(|volume| match volume.role {
                crate::CombatCapsuleRole::ProjectileEmitter { action, .. } => Some(action),
                _ => None,
            })
            .unwrap_or(CharacterAnimationAction::RangedAttack)
    }
}

/// What a Character binds, as the verb rules read it: a clip resolved from
/// its Animation Set exactly as the cook resolves one, and its shot.
#[derive(Clone, Copy)]
pub struct CharacterVerbBindings<'a> {
    project: &'a ProjectDocument,
    set: Option<&'a AnimationSetResource>,
    shoot: CharacterAnimationAction,
}

impl<'a> CharacterVerbBindings<'a> {
    /// The bindings of `character` in `project`.
    pub fn new(project: &'a ProjectDocument, character: &'a CharacterResource) -> Self {
        let set = character
            .animation_set
            .and_then(|id| project.resource(id))
            .and_then(|resource| match &resource.data {
                crate::ResourceData::AnimationSet(set) => Some(set),
                _ => None,
            });
        Self {
            project,
            set,
            shoot: character.shoot_action(),
        }
    }

    /// The Animation Set the bindings come from, if any.
    pub fn set(&self) -> Option<&'a AnimationSetResource> {
        self.set
    }
}

impl VerbBindings for CharacterVerbBindings<'_> {
    fn bound(&self, action: psx_level::CharacterAnimationAction) -> bool {
        self.set.is_some_and(|set| {
            crate::playtest::animation_set_action_clip(
                self.project,
                set,
                CharacterAnimationAction::from_level(action),
            )
            .is_some()
        })
    }

    fn shoot(&self) -> psx_level::CharacterAnimationAction {
        self.shoot.to_level()
    }
}
