//! The combat verb contract: one fixed list of what a fighter can do,
//! grouped into kits, shared by the player and every enemy.
//!
//! Player and enemies run the same combat rules and the AI is only a
//! controller, so every character answers to the same verbs. A verb is a
//! [`CharacterAnimationAction`] slot; a [`CombatKit`] groups the verbs that
//! belong together (the blade's light and heavy strike, the Zenith cannon's
//! aim and shot and its ranged walk, ...).
//!
//! What a character may do follows from the kits it binds, in one place,
//! [`CombatKits`]: it melees only with the melee kit, shoots only with the
//! ranged kit, dodges only with the mobility kit, and swaps stance only with
//! both the melee and the ranged kit. The cook, the editor and both runtimes
//! (player and enemy) call this module rather than keep their own rules.
//!
//! A verb a character does not bind resolves through its documented
//! [`fallback_chain`]: the first bound action of the chain plays instead.
//! When the chain runs out, a movement or reaction verb holds the
//! character's rest pose ([`CharacterAnimationAction::Idle`]); an action
//! verb (an attack, the shot, a dodge) is refused by its kit rule, so it
//! never plays some unrelated clip.

use crate::CharacterAnimationAction;

type Action = CharacterAnimationAction;

/// A group of verbs a character binds together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombatKit {
    /// Locomotion, reactions and death: every character, always expected.
    Core,
    /// The blade: light and heavy strikes.
    Melee,
    /// The Zenith cannon: aim, shot and the ranged walk.
    Ranged,
    /// The stance swap and the two parry ripostes. Expected exactly when the
    /// character can swap ([`CombatKits::can_swap`]).
    Swap,
    /// Roll, backstep and the locked-on dashes.
    Mobility,
}

impl CombatKit {
    /// Every kit, in display order.
    pub const ALL: [Self; 5] = [
        Self::Core,
        Self::Melee,
        Self::Ranged,
        Self::Swap,
        Self::Mobility,
    ];

    /// Short display name.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Core => "Core",
            Self::Melee => "Melee",
            Self::Ranged => "Ranged",
            Self::Swap => "Swap",
            Self::Mobility => "Mobility",
        }
    }
}

/// One verb of the contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CombatVerb {
    /// The action slot that carries the verb's clip.
    pub action: Action,
    /// The kit the verb belongs to.
    pub kit: CombatKit,
    /// An extra verb a kit may carry: shown with the kit, never reported
    /// missing (the Zenith-axis melee strikes, which only some characters
    /// author).
    pub optional: bool,
}

const fn verb(action: Action, kit: CombatKit) -> CombatVerb {
    CombatVerb {
        action,
        kit,
        optional: false,
    }
}

const fn extra(action: Action, kit: CombatKit) -> CombatVerb {
    CombatVerb {
        action,
        kit,
        optional: true,
    }
}

/// Number of verbs in [`COMBAT_VERBS`].
pub const COMBAT_VERB_COUNT: usize = 29;

/// The verb list, grouped by kit in [`CombatKit::ALL`] order.
///
/// [`CharacterAnimationAction::RangedAttack`] is the shot. A character whose
/// projectile is authored on another action shoots from that one instead
/// ([`VerbBindings::shoot`]).
pub const COMBAT_VERBS: [CombatVerb; COMBAT_VERB_COUNT] = [
    verb(Action::Idle, CombatKit::Core),
    verb(Action::Walk, CombatKit::Core),
    verb(Action::Run, CombatKit::Core),
    verb(Action::StrafeLeft, CombatKit::Core),
    verb(Action::StrafeRight, CombatKit::Core),
    verb(Action::WalkBackward, CombatKit::Core),
    verb(Action::Turn, CombatKit::Core),
    verb(Action::HitReact, CombatKit::Core),
    verb(Action::Stun, CombatKit::Core),
    verb(Action::Knockdown, CombatKit::Core),
    verb(Action::GetUp, CombatKit::Core),
    verb(Action::Death, CombatKit::Core),
    verb(Action::LightAttack, CombatKit::Melee),
    verb(Action::HeavyAttack, CombatKit::Melee),
    extra(Action::VertLightAttack, CombatKit::Melee),
    extra(Action::VertHeavyAttack, CombatKit::Melee),
    verb(Action::RangedAim, CombatKit::Ranged),
    verb(Action::RangedAttack, CombatKit::Ranged),
    verb(Action::RangedWalk, CombatKit::Ranged),
    verb(Action::RangedBackward, CombatKit::Ranged),
    verb(Action::RangedLeft, CombatKit::Ranged),
    verb(Action::RangedRight, CombatKit::Ranged),
    verb(Action::StanceSwap, CombatKit::Swap),
    verb(Action::CritStrike, CombatKit::Swap),
    verb(Action::CritShot, CombatKit::Swap),
    verb(Action::Roll, CombatKit::Mobility),
    verb(Action::Backstep, CombatKit::Mobility),
    verb(Action::DashLeft, CombatKit::Mobility),
    verb(Action::DashRight, CombatKit::Mobility),
];

/// The verb entry for `action`, when the action is part of the contract.
pub fn combat_verb(action: Action) -> Option<CombatVerb> {
    COMBAT_VERBS.iter().copied().find(|v| v.action == action)
}

/// The documented fallback chain for `action`, in order: when the action
/// has no clip, the first bound action of this list plays in its place.
///
/// An empty chain means there is no stand-in. A movement or reaction verb
/// then holds the rest pose (Idle); an action verb is refused by its kit.
/// [`CharacterAnimationAction::RangedAttack`] in a chain means the
/// character's shot ([`VerbBindings::shoot`]).
///
/// The chains, one line each:
/// - Run, WalkBackward, StrafeLeft, StrafeRight: Walk.
/// - Stun: HitReact. Knockdown: Stun, then HitReact.
/// - GetUp, StanceSwap: none (the knockdown or swap just ends, no clip).
/// - LightAttack: the legacy ComboAttack slot (and it the light). HeavyAttack
///   and CritStrike: LightAttack, then ComboAttack.
/// - CritShot: the shot.
/// - RangedWalk: Walk. RangedBackward: WalkBackward, then Walk. RangedLeft:
///   StrafeLeft, then Walk. RangedRight: StrafeRight, then Walk.
/// - Roll: Backstep. Backstep: Roll. DashLeft, DashRight: Roll, then
///   Backstep.
/// - Alt attacks: the primary light/heavy/combo. HitReactAlt: HitReact.
///   StunRecovery: Stun. Walk and run windups and winddowns: their cruise
///   (the mirrored winddown tries the plain winddown first).
/// - Everything else (Idle, Walk, Turn, HitReact, Death, Intro, the Zenith
///   melee strikes, RangedAim, the shot, Block, the hook and arch actions):
///   none.
pub const fn fallback_chain(action: Action) -> &'static [Action] {
    match action {
        Action::Run | Action::WalkBackward | Action::StrafeLeft | Action::StrafeRight => {
            &[Action::Walk]
        }
        Action::Stun => &[Action::HitReact],
        Action::Knockdown => &[Action::Stun, Action::HitReact],
        Action::LightAttack => &[Action::ComboAttack],
        Action::HeavyAttack | Action::CritStrike => &[Action::LightAttack, Action::ComboAttack],
        Action::ComboAttack => &[Action::LightAttack],
        Action::CritShot => &[Action::RangedAttack],
        Action::RangedWalk => &[Action::Walk],
        Action::RangedBackward => &[Action::WalkBackward, Action::Walk],
        Action::RangedLeft => &[Action::StrafeLeft, Action::Walk],
        Action::RangedRight => &[Action::StrafeRight, Action::Walk],
        Action::Roll => &[Action::Backstep],
        Action::Backstep => &[Action::Roll],
        Action::DashLeft | Action::DashRight => &[Action::Roll, Action::Backstep],
        Action::AltLightAttack => &[Action::LightAttack, Action::ComboAttack],
        Action::AltHeavyAttack => &[Action::HeavyAttack, Action::LightAttack],
        Action::AltComboAttack => &[Action::ComboAttack, Action::LightAttack],
        Action::HitReactAlt => &[Action::HitReact],
        Action::StunRecovery => &[Action::Stun],
        Action::WalkWindup | Action::WalkWinddown => &[Action::Walk],
        Action::WalkWinddownAlt => &[Action::WalkWinddown, Action::Walk],
        Action::RunWindup | Action::RunWinddown => &[Action::Run, Action::Walk],
        Action::RunWinddownAlt => &[Action::RunWinddown, Action::Run, Action::Walk],
        Action::Idle
        | Action::Walk
        | Action::Turn
        | Action::Block
        | Action::HitReact
        | Action::Death
        | Action::Intro
        | Action::VertLightAttack
        | Action::VertHeavyAttack
        | Action::VertComboAttack
        | Action::RangedAttack
        | Action::RangedAim
        | Action::GetUp
        | Action::StanceSwap
        | Action::HookLaunch
        | Action::ArchPerch
        | Action::Fall
        | Action::Land => &[],
    }
}

/// What a character binds, as the verb rules need it.
pub trait VerbBindings {
    /// Whether `action` has an authored clip of its own.
    fn bound(&self, action: Action) -> bool;

    /// The action the character shoots from: [`Action::RangedAttack`]
    /// unless its projectile is authored on another action.
    fn shoot(&self) -> Action {
        Action::RangedAttack
    }
}

/// Bindings as a plain predicate, shooting from [`Action::RangedAttack`].
pub struct BoundBy<F: Fn(Action) -> bool>(pub F);

impl<F: Fn(Action) -> bool> VerbBindings for BoundBy<F> {
    fn bound(&self, action: Action) -> bool {
        (self.0)(action)
    }
}

/// Bindings as a predicate plus the action the projectile is authored on.
pub struct BoundWithShot<F: Fn(Action) -> bool> {
    /// Whether an action has a clip of its own.
    pub bound: F,
    /// The action the character shoots from.
    pub shoot: Action,
}

impl<F: Fn(Action) -> bool> VerbBindings for BoundWithShot<F> {
    fn bound(&self, action: Action) -> bool {
        (self.bound)(action)
    }

    fn shoot(&self) -> Action {
        self.shoot
    }
}

/// The slot a verb names, with the shot mapped to the character's own.
fn slot(bindings: &impl VerbBindings, action: Action) -> Action {
    if matches!(action, Action::RangedAttack) {
        bindings.shoot()
    } else {
        action
    }
}

/// The action whose clip plays for `action`: the action itself when bound,
/// else the first bound action of its [`fallback_chain`], else `None` (rest
/// pose, or a refused action).
pub fn resolve(bindings: &impl VerbBindings, action: Action) -> Option<Action> {
    let own = slot(bindings, action);
    if bindings.bound(own) {
        return Some(own);
    }
    fallback_chain(action)
        .iter()
        .map(|&step| slot(bindings, step))
        .find(|&step| bindings.bound(step))
}

/// How one verb is covered by a character's bindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerbCoverage {
    /// The verb has its own clip (for the shot: on the character's shooting
    /// action, which the value names).
    Bound(Action),
    /// No clip of its own; this action's clip plays instead.
    Fallback(Action),
    /// No clip of its own and no stand-in in its chain.
    Missing,
}

/// Coverage of `action` under `bindings`.
pub fn coverage(bindings: &impl VerbBindings, action: Action) -> VerbCoverage {
    let own = slot(bindings, action);
    match resolve(bindings, action) {
        Some(found) if found == own => VerbCoverage::Bound(found),
        Some(found) => VerbCoverage::Fallback(found),
        None => VerbCoverage::Missing,
    }
}

/// The kits a character binds, and so what it may do.
///
/// A kit is present when one of its key verbs has a clip of its own (or, for
/// the blade, through the legacy combo slot): the melee kit with a light or
/// heavy strike, the ranged kit with the shot, the mobility kit with a roll,
/// backstep or dash. Core is always present; the swap kit is present exactly
/// when both the melee and the ranged kit are.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CombatKits(u8);

impl CombatKits {
    /// The blade.
    pub const MELEE: u8 = 1 << 0;
    /// The Zenith cannon.
    pub const RANGED: u8 = 1 << 1;
    /// Roll, backstep or dash.
    pub const MOBILITY: u8 = 1 << 2;

    /// No kit beyond core.
    pub const NONE: Self = Self(0);

    /// The kits from raw [`Self::MELEE`] / [`Self::RANGED`] /
    /// [`Self::MOBILITY`] bits.
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits & (Self::MELEE | Self::RANGED | Self::MOBILITY))
    }

    /// The raw kit bits.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Derive the kits from what a character binds.
    pub fn from_bindings(bindings: &impl VerbBindings) -> Self {
        let mut bits = 0;
        if resolve(bindings, Action::LightAttack).is_some()
            || resolve(bindings, Action::HeavyAttack).is_some()
        {
            bits |= Self::MELEE;
        }
        if bindings.bound(bindings.shoot()) {
            bits |= Self::RANGED;
        }
        if [
            Action::Roll,
            Action::Backstep,
            Action::DashLeft,
            Action::DashRight,
        ]
        .into_iter()
        .any(|action| bindings.bound(action))
        {
            bits |= Self::MOBILITY;
        }
        Self(bits)
    }

    /// Whether the character may strike with the blade.
    pub const fn can_melee(self) -> bool {
        self.0 & Self::MELEE != 0
    }

    /// Whether the character may shoot.
    pub const fn can_shoot(self) -> bool {
        self.0 & Self::RANGED != 0
    }

    /// Whether the character may roll, backstep or dash.
    pub const fn can_dodge(self) -> bool {
        self.0 & Self::MOBILITY != 0
    }

    /// Whether the character may swap stance: only with both the blade and
    /// the cannon. One with a single weapon holds that weapon's stance.
    pub const fn can_swap(self) -> bool {
        self.can_melee() && self.can_shoot()
    }

    /// Whether the character has `kit`.
    pub const fn has(self, kit: CombatKit) -> bool {
        match kit {
            CombatKit::Core => true,
            CombatKit::Melee => self.can_melee(),
            CombatKit::Ranged => self.can_shoot(),
            CombatKit::Swap => self.can_swap(),
            CombatKit::Mobility => self.can_dodge(),
        }
    }
}

/// The verbs of kits the character has but does not bind itself, the list
/// the cook warns about and the authoring to-do list is made of. Optional
/// verbs never count. `visit` receives each missing verb with its coverage
/// (a stand-in, or none).
pub fn for_each_gap(bindings: &impl VerbBindings, mut visit: impl FnMut(CombatVerb, VerbCoverage)) {
    let kits = CombatKits::from_bindings(bindings);
    for verb in COMBAT_VERBS {
        if verb.optional || !kits.has(verb.kit) {
            continue;
        }
        let cover = coverage(bindings, verb.action);
        if !matches!(cover, VerbCoverage::Bound(_)) {
            visit(verb, cover);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bound_set(actions: &'static [Action]) -> BoundBy<impl Fn(Action) -> bool> {
        BoundBy(move |a| actions.contains(&a))
    }

    #[test]
    fn verb_list_is_grouped_and_unique() {
        let mut seen = [false; crate::CHARACTER_ANIMATION_ACTION_COUNT];
        for verb in COMBAT_VERBS {
            assert!(
                !seen[verb.action.to_index()],
                "{:?} listed twice",
                verb.action
            );
            seen[verb.action.to_index()] = true;
        }
        for kit in CombatKit::ALL {
            assert!(COMBAT_VERBS.iter().any(|v| v.kit == kit && !v.optional));
        }
    }

    #[test]
    fn kits_follow_bindings() {
        let melee_only = bound_set(&[Action::Idle, Action::LightAttack]);
        let kits = CombatKits::from_bindings(&melee_only);
        assert!(kits.can_melee() && !kits.can_shoot() && !kits.can_swap() && !kits.can_dodge());

        let both = bound_set(&[Action::HeavyAttack, Action::RangedAttack, Action::Roll]);
        let kits = CombatKits::from_bindings(&both);
        assert!(kits.can_melee() && kits.can_shoot() && kits.can_swap() && kits.can_dodge());
        assert!(kits.has(CombatKit::Swap) && kits.has(CombatKit::Core));

        let ranged_only = bound_set(&[Action::RangedAttack, Action::Backstep]);
        let kits = CombatKits::from_bindings(&ranged_only);
        assert!(!kits.can_melee() && kits.can_shoot() && !kits.can_swap() && kits.can_dodge());

        // The legacy combo slot still arms the blade; a Zenith strike alone
        // does not.
        assert!(CombatKits::from_bindings(&bound_set(&[Action::ComboAttack])).can_melee());
        assert!(!CombatKits::from_bindings(&bound_set(&[Action::VertLightAttack])).can_melee());
        assert!(CombatKits::from_bindings(&bound_set(&[Action::DashLeft])).can_dodge());
        assert_eq!(
            CombatKits::from_bindings(&bound_set(&[Action::Idle, Action::Walk])),
            CombatKits::NONE
        );
    }

    #[test]
    fn an_enemy_record_carries_the_cooked_kits() {
        use crate::game_entity_flags as flags;
        let kits = crate::game_entity_combat_kits(flags::ENABLED | flags::MELEE_ATTACK);
        assert!(kits.can_melee() && !kits.can_shoot() && !kits.can_dodge());
        let kits = crate::game_entity_combat_kits(
            flags::MELEE_ATTACK | flags::RANGED_ATTACK | flags::MOBILITY,
        );
        assert!(kits.can_swap() && kits.can_dodge());
        // No kit flag: no kit (no legacy "no projectile means melee").
        assert_eq!(
            crate::game_entity_combat_kits(flags::ENABLED),
            CombatKits::NONE
        );
    }

    #[test]
    fn a_projectile_on_another_action_is_the_shot() {
        let bindings = BoundWithShot {
            bound: |a| matches!(a, Action::LightAttack | Action::VertLightAttack),
            shoot: Action::VertLightAttack,
        };
        let kits = CombatKits::from_bindings(&bindings);
        assert!(kits.can_swap());
        assert_eq!(
            coverage(&bindings, Action::RangedAttack),
            VerbCoverage::Bound(Action::VertLightAttack)
        );
        assert_eq!(
            coverage(&bindings, Action::CritShot),
            VerbCoverage::Fallback(Action::VertLightAttack)
        );
    }

    #[test]
    fn every_fallback_chain_resolves_in_order() {
        use Action as A;
        // Each case: (verb, bound set, expected stand-in).
        let cases: [(A, &'static [A], Option<A>); 24] = [
            (A::Run, &[A::Walk], Some(A::Walk)),
            (A::WalkBackward, &[A::Walk], Some(A::Walk)),
            (A::StrafeLeft, &[A::Walk], Some(A::Walk)),
            (A::StrafeRight, &[], None),
            (A::Stun, &[A::HitReact], Some(A::HitReact)),
            (A::Knockdown, &[A::Stun, A::HitReact], Some(A::Stun)),
            (A::Knockdown, &[A::HitReact], Some(A::HitReact)),
            (A::Knockdown, &[], None),
            (A::GetUp, &[A::Stun, A::Idle], None),
            (A::StanceSwap, &[A::Idle, A::Walk], None),
            (A::LightAttack, &[A::ComboAttack], Some(A::ComboAttack)),
            (
                A::HeavyAttack,
                &[A::LightAttack, A::ComboAttack],
                Some(A::LightAttack),
            ),
            (A::CritStrike, &[A::LightAttack], Some(A::LightAttack)),
            (A::CritStrike, &[A::ComboAttack], Some(A::ComboAttack)),
            (A::CritShot, &[A::RangedAttack], Some(A::RangedAttack)),
            (A::CritShot, &[A::LightAttack], None),
            (A::RangedWalk, &[A::Walk], Some(A::Walk)),
            (
                A::RangedBackward,
                &[A::WalkBackward, A::Walk],
                Some(A::WalkBackward),
            ),
            (A::RangedLeft, &[A::Walk], Some(A::Walk)),
            (A::RangedRight, &[A::StrafeRight], Some(A::StrafeRight)),
            (A::Roll, &[A::Backstep], Some(A::Backstep)),
            (A::Backstep, &[A::Roll], Some(A::Roll)),
            (A::DashLeft, &[A::Backstep], Some(A::Backstep)),
            (A::DashRight, &[A::Roll, A::Backstep], Some(A::Roll)),
        ];
        for (action, bound, expected) in cases {
            assert_eq!(
                resolve(&bound_set(bound), action),
                expected,
                "{action:?} with {bound:?}"
            );
        }
        // No chain ever lands on the rest pose or an unrelated clip: the
        // only stand-ins are listed actions.
        for action in Action::ALL {
            assert!(!fallback_chain(action).contains(&action));
            assert!(!fallback_chain(action).contains(&Action::Idle));
        }
        // A bound verb always plays itself.
        for action in Action::ALL {
            let all = BoundBy(|_| true);
            assert_eq!(resolve(&all, action), Some(action));
        }
    }

    #[test]
    fn gaps_list_only_missing_verbs_of_present_kits() {
        // Blade light only: core gaps plus the heavy (standing in with the
        // light); no ranged, swap or mobility gaps.
        let set = bound_set(&[Action::Idle, Action::Walk, Action::LightAttack]);
        let mut gaps = [None; COMBAT_VERB_COUNT];
        let mut count = 0;
        for_each_gap(&set, |verb, cover| {
            gaps[count] = Some((verb.action, cover));
            count += 1;
        });
        let gaps = &gaps[..count];
        assert!(gaps.contains(&Some((
            Action::HeavyAttack,
            VerbCoverage::Fallback(Action::LightAttack)
        ))));
        assert!(gaps.contains(&Some((Action::GetUp, VerbCoverage::Missing))));
        assert!(gaps.iter().flatten().all(|(action, _)| combat_verb(*action)
            .is_some_and(|v| matches!(v.kit, CombatKit::Core | CombatKit::Melee))));
        assert!(!gaps
            .iter()
            .flatten()
            .any(|(a, _)| *a == Action::VertLightAttack));
    }
}
