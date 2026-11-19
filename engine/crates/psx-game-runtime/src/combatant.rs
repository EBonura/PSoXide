//! Combat state one fighter carries, the same type for the player and enemies.
//!
//! A [`Combatant`] is plain data plus thin rules over the shared leaf types:
//! health (one [`VitalityPool`] bar), posture ([`Poise`]), energy and
//! poise-break grace ([`CombatFlow`]) and the stance ([`CombatStance`]: side,
//! swap cooldown and swap clock). It holds no scene, no entity tables and no
//! AI.
//! Where today's player and enemy rules still differ, the difference is a
//! parameter of the call (a cooldown, a capacity, whether break grace
//! applies) rather than a branch on who is fighting.
//!
//! Invulnerability is one rule for every fighter, [`Combatant::invulnerable`]:
//! the protection the current action grants (an authored `Invulnerable`
//! combat window, else the dodge's i-frames) or the swap window. Nothing of it
//! is stored: the swap window is read from the stance clock and the action's
//! protection is read by the owner off its animation and motor when it asks,
//! so it can never go stale. Every hit resolver asks the defender through a
//! [`Defender`] before a hit may connect.
//!
//! Parry, crit and knockdown are the same rules for every fighter too. A hit
//! a fighter deals ([`CombatantId::WORLD`] never parries) landing in the
//! first [`CombatStats::parry_ticks`] of the defender's swap window is
//! parried ([`HitOut::parried`]): it deals nothing, refunds the defender
//! energy, clears its swap cooldown and arms a crit for its next attack. The
//! owner then staggers the attacker with [`Strike::parry`], through the same
//! [`Combatant::receive_hit`]. A crit ([`Combatant::crit_scaled`]) multiplies
//! the damage and poise of the next attack that lands (a swing) or is
//! released (a shot) within [`CombatStats::crit_window_ticks`]. A crit that
//! breaks posture knocks the fighter down for [`CombatStats::knockdown_ticks`]
//! ([`Combatant::knocked_down`]): it cannot act or move and it cannot be
//! broken again until it is up ([`DOWNED_PROTECTED`] says whether hits pass
//! through it meanwhile). Every other break, a parry's riposte included, is a
//! stagger: the owner plays the fighter's Stun clip through.
//!
//! Who may swap at all follows from the fighter's kits
//! ([`psx_level::combat_verbs::CombatKits`], derived from what it binds):
//! only a fighter with both the melee and the ranged kit swaps; one with a
//! single weapon holds that weapon's stance ([`starting_stance`]).
//!
//! Every hit, whatever dealt it, lands through [`Combatant::receive_hit`]. A
//! [`HitIn`] is the [`Strike`] (what the attacker sent) plus the defender's
//! [`CombatStats`] and [`PoiseRule`]. Damage and poise follow
//! one rule for every fighter, [`stance_scaled`]: a hit from the stance
//! opposite the defender's active one is multiplied by the defender's
//! opposite-stance multipliers, a matching hit is taken as authored.
//! Characters differ only in their stats. The damage comes off the one
//! health bar, which nothing refills during play: only a respawn, an arch or
//! a checkpoint restores it.

use psx_level::combat_verbs::CombatKits;
use psx_level::{combat_armour, CombatStats};

use crate::combat_flow::CombatFlow;
use crate::poise::Poise;
use crate::vitality::{CombatStance, VitalityChannelId, VitalityPool, VITALITY_Q12_ONE};

/// The one armour rule for every fighter: the current action's authored
/// `Armored` window (`authored`, None when its clip authors none) decides
/// when the character's stats take authored windows
/// ([`combat_armour::AUTHORED_WINDOW`]); otherwise a heavy attack armours its
/// fighter while its contact is live (`heavy_contact`, with
/// [`combat_armour::HEAVY_ATTACK`]). Armour doubles the poise capacity for a
/// hit ([`PoiseRule::armored`]).
// Asked once per landed hit: one out-of-line copy for every caller.
#[inline(never)]
#[cfg_attr(target_arch = "mips", optimize(size))]
pub fn armoured(stats: &CombatStats, authored: Option<bool>, heavy_contact: bool) -> bool {
    match authored {
        Some(open) if stats.armour & combat_armour::AUTHORED_WINDOW != 0 => open,
        _ => stats.armour & combat_armour::HEAVY_ATTACK != 0 && heavy_contact,
    }
}

/// Whether a knocked-down fighter is protected: hits pass through it until it
/// is up, as most of the reference games do for a downed fighter. The same
/// rule for every fighter; chosen by the duel harness (combat tuning,
/// change 1).
pub const DOWNED_PROTECTED: bool = true;

/// The duel harness's setting of [`DOWNED_PROTECTED`] (host builds with the
/// `harness` feature only), so a sweep can test both rules without a rebuild.
/// A game build never has it: there the constant is read directly.
#[cfg(feature = "harness")]
pub static DOWNED_PROTECTED_SETTING: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(DOWNED_PROTECTED);

/// The downed rule in force: [`DOWNED_PROTECTED`], or the harness's setting.
#[inline(always)]
fn downed_protected() -> bool {
    #[cfg(feature = "harness")]
    {
        DOWNED_PROTECTED_SETTING.load(core::sync::atomic::Ordering::Relaxed)
    }
    #[cfg(not(feature = "harness"))]
    {
        DOWNED_PROTECTED
    }
}

/// The one stance rule: health and posture damage a hit of `damage` and
/// `poise` deals to a defender with `stats`, `opposite` when the hit's
/// stance is not the defender's active one. Zero stays zero.
pub fn stance_scaled(stats: &CombatStats, opposite: bool, damage: u16, poise: u16) -> (u16, u16) {
    if !opposite {
        return (damage, poise);
    }
    (
        scale_q12(damage, stats.opposite_damage_q12),
        scale_q12(poise, stats.opposite_poise_q12),
    )
}

fn scale_q12(value: u16, q12: u16) -> u16 {
    (u32::from(value) * u32::from(q12) / u32::from(VITALITY_Q12_ONE)).min(u32::from(u16::MAX))
        as u16
}

/// The stance a fresh life starts in, from the fighter's kits
/// ([`CombatKits`]): Zenith for a fighter with the cannon and no blade, the
/// blade otherwise. A fighter that cannot swap ([`CombatKits::can_swap`])
/// holds this stance for good.
pub const fn starting_stance(kits: CombatKits) -> VitalityChannelId {
    if kits.can_shoot() && !kits.can_melee() {
        VitalityChannelId::Two
    } else {
        VitalityChannelId::One
    }
}

/// Who dealt a hit: an entity slot, the player, or the world.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CombatantId(pub u16);

impl CombatantId {
    /// The player character.
    pub const PLAYER: Self = Self(u16::MAX - 1);
    /// No fighter: hazards, liquids and other environment damage.
    pub const WORLD: Self = Self(u16::MAX);

    /// The entity in table slot `index`.
    pub const fn entity(index: usize) -> Self {
        Self(index as u16)
    }
}

/// What kind of source dealt a hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitKind {
    /// A melee swing (its authored capsules).
    Melee,
    /// A projectile impact.
    Projectile,
    /// A hazard volume or a harmful liquid.
    Hazard,
    /// Anything else the world deals.
    Environment,
    /// The riposte of a parry on the attacker: posture only, and its break
    /// is a stagger, never a knockdown.
    Parry,
}

/// The attacker's half of a hit: who, how, on which channel and how hard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Strike {
    /// Who dealt it.
    pub attacker: CombatantId,
    /// What dealt it.
    pub kind: HitKind,
    /// The attack's vitality channel (stance colour).
    pub channel: VitalityChannelId,
    /// Health damage before the defender's routing scales it.
    pub damage: u16,
    /// Posture damage before the defender's routing scales it.
    pub poise: u16,
    /// A crit (the swing or volley a parry armed): its posture damage lands
    /// even on a fighter staggered by that parry's riposte, inside the
    /// riposte's break grace, and its break is the only knockdown.
    pub crit: bool,
}

impl Strike {
    /// The riposte a fighter that parried (`by`) sends its attacker: a
    /// posture-only hit on the attacker's own `channel` (so no stance
    /// multiplier applies) big enough to break any posture outside break
    /// grace, armour included.
    pub const fn parry(by: CombatantId, channel: VitalityChannelId) -> Self {
        Self {
            attacker: by,
            kind: HitKind::Parry,
            channel,
            damage: 0,
            poise: u16::MAX,
            crit: false,
        }
    }

    /// Whether a fighter dealt this strike as an attack, so it can be
    /// parried: a swing or a shot by anyone but the world.
    pub fn parryable(&self) -> bool {
        self.attacker != CombatantId::WORLD
            && matches!(self.kind, HitKind::Melee | HitKind::Projectile)
    }
}

/// The defender's posture state for one hit. The capacity is
/// [`CombatStats::poise`], and break grace always applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoiseRule {
    /// Armour doubles the capacity for this hit.
    pub armored: bool,
    /// The defender cannot be broken right now (already in a break).
    pub blocked: bool,
}

/// Everything [`Combatant::receive_hit`] needs: the strike and the
/// defender's stats and posture state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HitIn {
    /// The attacker's half.
    pub strike: Strike,
    /// The defender's numbers.
    pub stats: CombatStats,
    /// The defender's posture state.
    pub poise: PoiseRule,
}

/// The protection a fighter's current action grants, read by its owner off
/// the action's animation and motor at the moment a hit resolver asks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActionProtection {
    /// The action's authored `Invulnerable` combat window at this frame:
    /// `Some(open)` when the action authors that channel, `None` when it does
    /// not. An authored window replaces the dodge's i-frames.
    pub authored: Option<bool>,
    /// The motor's dodge (roll or backstep) i-frames.
    pub dodge: bool,
}

impl ActionProtection {
    /// No action protection: no authored window, no dodge. What a fighter
    /// without combat windows or a dodge (every enemy today) always has.
    pub const NONE: Self = Self {
        authored: None,
        dodge: false,
    };

    /// Whether the action alone protects the fighter.
    pub const fn active(self) -> bool {
        match self.authored {
            Some(open) => open,
            None => self.dodge,
        }
    }
}

/// A fighter as the target of a hit: its combat state, how many ticks a swap
/// protects it (its character's `roll_invulnerable_frames`, data), how many
/// of those parry (its [`CombatStats::parry_ticks`]) and what its current
/// action grants. Hit resolvers ask [`Self::passes_through`].
#[derive(Clone, Copy, Debug)]
pub struct Defender<'a> {
    /// The target's combat state.
    pub combatant: &'a Combatant,
    /// Ticks a swap protects this fighter.
    pub swap_invulnerable_ticks: u8,
    /// Ticks from the swap during which a fighter's hit is parried.
    pub parry_ticks: u16,
    /// What the target's current action grants.
    pub action: ActionProtection,
}

impl Defender<'static> {
    /// A target that is never invulnerable: a settled fighter with no swap
    /// window and no action protection.
    pub const OPEN: Self = Self {
        combatant: &Combatant::EMPTY,
        swap_invulnerable_ticks: 0,
        parry_ticks: 0,
        action: ActionProtection::NONE,
    };
}

impl Defender<'_> {
    /// Whether a hit on this target passes through it, see
    /// [`Combatant::invulnerable`].
    pub fn invulnerable(&self) -> bool {
        self.combatant
            .invulnerable(self.swap_invulnerable_ticks, self.action)
    }

    /// Whether a fighter's hit on this target would be parried this tick,
    /// see [`Combatant::parrying`].
    pub fn parries(&self) -> bool {
        self.combatant.parrying(self.parry_ticks)
    }

    /// Whether a fighter's hit passes through this target without landing:
    /// invulnerable and not parrying. A hit inside the parry window lands
    /// and [`Combatant::receive_hit`] turns it into a parry.
    pub fn passes_through(&self) -> bool {
        !self.parries() && self.invulnerable()
    }
}

/// What one [`Combatant::receive_hit`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HitOut {
    /// Health actually removed.
    pub damage_applied: u16,
    /// The health bar is empty after the hit.
    pub defeated: bool,
    /// This hit emptied the health bar. Posture is left alone when it does.
    pub killed: bool,
    /// The hit broke posture.
    pub poise_broken: bool,
    /// The break knocked the fighter down (a crit's break, while the stats
    /// allow knockdown); any other break is a stagger.
    pub knocked_down: bool,
    /// The defender parried: nothing landed, the defender gained its
    /// refund, its swap cooldown and a crit. The owner staggers the
    /// attacker with [`Strike::parry`].
    pub parried: bool,
}

/// What a fighter's body does after a landed hit. Every fighter reacts by
/// the one rule ([`Posture::reaction`]); only how a body plays it differs
/// (an entity's state, the player's animation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reaction {
    /// An ordinary swing's hit reaction (the Hit React clip).
    Flinch,
    /// A posture break (the stagger clip).
    Stagger,
    /// A posture break by a parry's riposte: the stagger, with the Parried
    /// clip over it.
    Parried,
    /// A crit's posture break: down for [`CombatStats::knockdown_ticks`].
    Knockdown,
}

impl Reaction {
    /// The verb every body plays this reaction with, through its Character's
    /// fallback chain ([`psx_level::combat_verbs`]): a Parried with no
    /// Parried clip plays the Stun, a Stun with none the Hit React.
    pub const fn verb(self) -> psx_level::CharacterAnimationAction {
        use psx_level::CharacterAnimationAction as Action;
        match self {
            Self::Flinch => Action::HitReact,
            Self::Stagger => Action::Stun,
            Self::Parried => Action::Parried,
            Self::Knockdown => Action::Knockdown,
        }
    }

    /// The action a body that binds the actions `bound` says shows for this
    /// reaction: its verb, else the first bound stand-in of the verb's chain
    /// (Stun, then Hit React, as `combat_verbs::resolve` walks it; pinned by
    /// a test); with none bound, the verb (a riposte's, the stagger's), over
    /// the rest pose. Spelled out rather than calling the generic resolver:
    /// the reaction chains are this short, and the guest copy of the general
    /// walk costs about 0.8 KB.
    pub fn shown(
        self,
        bound: impl Fn(psx_level::CharacterAnimationAction) -> bool,
    ) -> psx_level::CharacterAnimationAction {
        use psx_level::CharacterAnimationAction as Action;
        let verb = self.verb();
        if bound(verb) {
            verb
        } else if self != Self::Flinch && bound(Action::Stun) {
            Action::Stun
        } else if bound(Action::HitReact) {
            Action::HitReact
        } else if self == Self::Parried {
            Action::Stun
        } else {
            verb
        }
    }
}

/// The defender's side of one hit apart from its numbers: armour now, the
/// reaction holding it, and whether its body binds a flinch (Hit React)
/// clip.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Posture {
    /// Its current action armours it.
    pub armored: bool,
    /// The reaction it is in, if any.
    pub reacting: Option<Reaction>,
    /// It binds a Hit React clip, so an ordinary swing can flinch it.
    pub flinches: bool,
}

impl Posture {
    /// The posture state for [`Combatant::receive_hit`]: a break reaction
    /// (stagger, riposte stagger, knockdown) cannot be broken again.
    pub fn rule(self) -> PoiseRule {
        PoiseRule {
            armored: self.armored,
            blocked: matches!(
                self.reacting,
                Some(Reaction::Stagger | Reaction::Parried | Reaction::Knockdown)
            ),
        }
    }

    /// The one reaction rule, for every fighter: a crit's break knocks
    /// down, any other break staggers (a riposte's with the Parried clip),
    /// and an ordinary swing flinches a fighter that binds a Hit React clip
    /// unless armour holds it or a reaction already does (a flinch never
    /// restarts itself). A kill or a parry is no reaction.
    pub fn reaction(self, kind: HitKind, out: &HitOut) -> Option<Reaction> {
        if out.killed || out.parried {
            None
        } else if out.knocked_down {
            Some(Reaction::Knockdown)
        } else if out.poise_broken {
            Some(if kind == HitKind::Parry {
                Reaction::Parried
            } else {
                Reaction::Stagger
            })
        } else if kind == HitKind::Melee
            && !self.armored
            && self.flinches
            && self.reacting.is_none()
        {
            Some(Reaction::Flinch)
        } else {
            None
        }
    }
}

/// Health, posture, energy and stance of one fighter.
///
/// `Copy` and `const`-constructible so entity tables can hold an array of
/// them in zero-initialised scene storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Combatant {
    /// The one health bar.
    pub health: VitalityPool,
    /// Accumulated posture damage.
    pub poise: Poise,
    /// Energy, poise-break grace and the ranged/melee resource cycle.
    pub flow: CombatFlow,
    /// Active side, swap cooldown and swap clock.
    pub stance: CombatStance,
    /// Ticks left down after a poise break; zero when standing.
    pub knockdown: u16,
    /// Ticks left in which an attack started crits; zero when none is
    /// armed. It counts down only while the fighter is free to act (not
    /// during its swap animation), so every fighter gets the same usable
    /// window whatever its swap length.
    pub crit: u16,
    /// The attack under way started while a crit was armed: it is the crit
    /// strike or crit shot until it lands or releases
    /// ([`Self::start_attack`], [`Self::spend_crit`]).
    pub crit_attack: bool,
}

impl Combatant {
    /// An empty bar on side One with no timers running. Not a live fighter
    /// until [`Self::reset`] or [`Self::respawn`] gives it health.
    pub const EMPTY: Self = Self {
        health: VitalityPool::at(0, 0),
        poise: Poise::EMPTY,
        flow: CombatFlow::FULL,
        stance: CombatStance::new(VitalityChannelId::One),
        knockdown: 0,
        crit: 0,
        crit_attack: false,
    };

    /// A fresh life for `stats`: health full, posture clear, energy full,
    /// side One with no cooldown and no swap showing; a fighter whose `kits`
    /// cannot swap holds its [`starting_stance`] instead.
    pub fn reset(&mut self, stats: &CombatStats, kits: CombatKits) {
        self.stance = CombatStance::new(VitalityChannelId::One);
        self.respawn(stats, kits);
    }

    /// Like [`Self::reset`] but keeps the stance side and its cooldown, only
    /// ending any swap presentation. This is what an entity slot does when
    /// it spawns. A zero maximum gives an empty bar, as the cook allows. A
    /// fighter whose `kits` cannot swap holds its [`starting_stance`] (a
    /// ranged-only one starts in Zenith), whoever controls it.
    pub fn respawn(&mut self, stats: &CombatStats, kits: CombatKits) {
        self.respawn_numbers(stats);
        if !kits.can_swap() {
            self.stance.hold(starting_stance(kits));
        }
    }

    fn respawn_numbers(&mut self, stats: &CombatStats) {
        self.health = VitalityPool::at(stats.max_health, stats.max_health);
        self.poise = Poise::EMPTY;
        self.flow = CombatFlow::full(stats);
        self.stance.settle();
        self.knockdown = 0;
        self.crit = 0;
        self.crit_attack = false;
    }

    /// Whether a hit on `channel` comes from the stance opposite this
    /// fighter's active one.
    pub fn opposes(&self, channel: VitalityChannelId) -> bool {
        channel != self.stance.active()
    }

    /// Current health.
    pub fn health(&self) -> u16 {
        self.health.current()
    }

    /// Whether the health bar is empty.
    pub fn defeated(&self) -> bool {
        self.health.is_empty()
    }

    /// Restore up to `health` points and `energy` energy (to the stats'
    /// ceiling), as an arch or a checkpoint does. Returns the health
    /// actually restored.
    pub fn restore(&mut self, health: u16, energy: u16, stats: &CombatStats) -> u16 {
        self.flow.gain_energy(energy, stats);
        self.health.heal(health)
    }

    /// One tick of the arch: the air allowance, and while hanging on with
    /// allowance left and not firing, one point of energy and one of health
    /// at [`crate::combat_flow::FLOAT_GAIN_PER_SECOND`]. True when the
    /// allowance ran out and the arch lets go.
    pub fn air_tick(
        &mut self,
        attached: bool,
        grounded: bool,
        firing: bool,
        stats: &CombatStats,
    ) -> bool {
        let tick = self.flow.air_tick(attached, grounded, firing, stats);
        if tick.restored {
            self.health.heal(1);
        }
        tick.exhausted
    }

    /// Advance posture recovery, the energy timers, the knockdown and the
    /// crit window by `delta` ticks. Break grace only drains once
    /// `recovering` (a hit reaction) is over; the crit window holds while
    /// `swapping` (the fighter's swap animation is still playing).
    pub fn tick(&mut self, delta: u16, recovering: bool, swapping: bool) {
        self.poise.tick(delta);
        self.flow.tick(delta, recovering);
        self.knockdown = self.knockdown.saturating_sub(delta);
        if !swapping {
            self.crit = self.crit.saturating_sub(delta);
        }
    }

    /// An attack (a strike or a volley) starts: it carries a crit when one
    /// is armed now, through to its hit or release, however long that takes.
    pub fn start_attack(&mut self) {
        self.crit_attack = self.crit != 0;
    }

    /// Whether the fighter is down after a poise break: it cannot act or
    /// move until [`CombatStats::knockdown_ticks`] have passed.
    pub fn knocked_down(&self) -> bool {
        self.knockdown != 0
    }

    /// Whether a fighter's hit would be parried now: the latest swap began
    /// no more than `parry_ticks` ago. Zero turns parrying off.
    pub fn parrying(&self, parry_ticks: u16) -> bool {
        parry_ticks != 0 && self.in_swap_window(parry_ticks)
    }

    /// Damage and poise of an attack this fighter deals, through its crit:
    /// multiplied by `stats`' crit multipliers when the attack started with
    /// a crit armed ([`Self::start_attack`]), unchanged otherwise.
    /// [`Self::spend_crit`] once the attack lands or is released.
    pub fn crit_scaled(&self, stats: &CombatStats, damage: u16, poise: u16) -> (u16, u16) {
        if !self.crit_attack {
            return (damage, poise);
        }
        (
            scale_q12(damage, stats.crit_damage_q12),
            scale_q12(poise, stats.crit_poise_q12),
        )
    }

    /// Use up the crit the attack under way carries; true when it carried
    /// one. The parry's window closes with it: one crit per parry.
    pub fn spend_crit(&mut self) -> bool {
        let armed = self.crit_attack;
        if armed {
            self.crit = 0;
            self.crit_attack = false;
        }
        armed
    }

    /// Advance the swap cooldown and swap clock by `delta` ticks.
    pub fn advance_stance(&mut self, delta: u16) {
        self.stance.cool_down(delta);
        self.stance.advance_swap(delta);
    }

    /// Voluntary swap arming `cooldown_ticks` of cooldown. False while the
    /// cooldown is running.
    pub fn request_swap(&mut self, cooldown_ticks: u16) -> bool {
        self.stance.try_swap(cooldown_ticks)
    }

    /// Whether the latest swap began no more than `frames` ticks ago.
    pub fn in_swap_window(&self, frames: u16) -> bool {
        self.stance.swap_elapsed_ticks() <= frames
    }

    /// The one i-frame rule: a hit passes through this fighter while its
    /// current `action` protects it (an authored `Invulnerable` window, else
    /// the dodge), while it is inside the swap window of
    /// `swap_invulnerable_ticks`, or while it is down when
    /// [`DOWNED_PROTECTED`] holds. A fighter that never swaps never enters the
    /// swap window. A hit that passes through does nothing: it neither lands
    /// nor uses up the attack.
    pub fn invulnerable(&self, swap_invulnerable_ticks: u8, action: ActionProtection) -> bool {
        action.active()
            || self.in_swap_window(u16::from(swap_invulnerable_ticks))
            || (downed_protected() && self.knocked_down())
    }

    /// Take one hit: health and posture scaled by [`stance_scaled`], health
    /// off the one bar, then posture against [`CombatStats::poise`]. This is
    /// the only way damage reaches a fighter.
    ///
    /// A fighter's swing or shot inside the parry window
    /// ([`CombatStats::parry_ticks`]) is parried instead: nothing lands, the
    /// defender gains [`CombatStats::parry_refund`] energy, its swap cooldown
    /// clears and its next attack within [`CombatStats::crit_window_ticks`]
    /// crits. A crit's posture break knocks the fighter down for
    /// [`CombatStats::knockdown_ticks`]; every other break staggers. A
    /// fighter that is down cannot be broken.
    ///
    /// A hit that kills leaves posture alone. A zero-damage hit (poise only)
    /// never touches the bar.
    pub fn receive_hit(&mut self, hit: HitIn) -> HitOut {
        let strike = hit.strike;
        if strike.parryable() && self.parrying(hit.stats.parry_ticks) {
            self.flow.gain_energy(hit.stats.parry_refund, &hit.stats);
            self.stance.clear_cooldown();
            self.crit = hit.stats.crit_window_ticks;
            return HitOut {
                parried: true,
                ..HitOut::default()
            };
        }
        let was_defeated = self.defeated();
        let (damage, poise) = stance_scaled(
            &hit.stats,
            self.opposes(strike.channel),
            strike.damage,
            strike.poise,
        );
        let damage_applied = self.health.apply_damage(damage);
        let defeated = self.defeated();
        let killed = !was_defeated && defeated;
        // A crit ignores the stagger and the break grace of the riposte that
        // armed it (a knockdown still blocks it): otherwise the riposte's own
        // break swallows every crit's posture damage.
        let blocked = (hit.poise.blocked && !strike.crit) || self.knocked_down();
        let poise_broken = !killed
            && self.poise_hit(
                poise,
                hit.stats.poise,
                hit.poise.armored,
                blocked,
                strike.crit,
            );
        let knocked_down = poise_broken && strike.crit && hit.stats.knockdown_ticks != 0;
        if knocked_down {
            self.knockdown = hit.stats.knockdown_ticks;
        }
        HitOut {
            damage_applied,
            defeated,
            killed,
            poise_broken,
            knocked_down,
            parried: false,
        }
    }

    /// Apply `amount` posture damage against `capacity`; true on a break.
    ///
    /// `blocked` refuses the hit outright (the fighter is already in a
    /// break reaction). The break-grace window of [`CombatFlow`] applies: a
    /// hit inside it cannot break, and a break opens a new one.
    fn poise_hit(
        &mut self,
        amount: u16,
        capacity: u16,
        armored: bool,
        blocked: bool,
        crit: bool,
    ) -> bool {
        if blocked || (!crit && !self.flow.can_interrupt()) {
            return false;
        }
        let broken = self.poise.hit(amount, capacity, armored);
        if broken {
            self.flow.broke();
        }
        broken
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_reaction_shows_what_the_verb_chain_resolves_to() {
        use psx_level::combat_verbs::{resolve, BoundBy};
        use psx_level::CharacterAnimationAction as Action;
        let actions = [
            Action::HitReact,
            Action::Stun,
            Action::Parried,
            Action::Knockdown,
        ];
        for set in 0u8..16 {
            let bound = |action: Action| {
                actions
                    .iter()
                    .position(|&a| a == action)
                    .is_some_and(|i| set & (1 << i) != 0)
            };
            for reaction in [
                super::Reaction::Flinch,
                super::Reaction::Stagger,
                super::Reaction::Parried,
                super::Reaction::Knockdown,
            ] {
                let resolved = resolve(&BoundBy(bound), reaction.verb());
                let shown = reaction.shown(bound);
                match resolved {
                    Some(action) => assert_eq!(shown, action, "{reaction:?} {set:04b}"),
                    None => assert!(!bound(shown), "{reaction:?} {set:04b}"),
                }
            }
        }
    }

    use super::*;
    use crate::combat_flow::BREAK_GRACE_TICKS;

    /// 200 health, poise 60, the starting multipliers and energy.
    const STATS: CombatStats = CombatStats::DEFAULT;

    fn fighter() -> Combatant {
        let mut c = Combatant::EMPTY;
        c.reset(
            &STATS,
            CombatKits::from_bits(CombatKits::MELEE | CombatKits::RANGED),
        );
        c
    }

    #[test]
    fn reset_is_a_fresh_life_and_respawn_keeps_the_side() {
        let mut c = fighter();
        assert_eq!(c.health(), 200);
        assert!(c.request_swap(30));
        c.health.apply_damage(150);
        c.flow.spend_shot(&STATS);
        let small = CombatStats {
            max_health: 80,
            energy_max: 40,
            ..STATS
        };
        c.respawn(
            &small,
            CombatKits::from_bits(CombatKits::MELEE | CombatKits::RANGED),
        );
        assert_eq!(c.health(), 80);
        assert_eq!(c.flow, CombatFlow::full(&small));
        assert_eq!(c.flow.energy, 40, "energy starts at the stats' ceiling");
        assert_eq!(c.stance.active(), VitalityChannelId::Two);
        assert_eq!(c.stance.swap_cooldown(), 30);
        assert_eq!(c.stance.swap_elapsed_ticks(), u16::MAX);
        c.reset(
            &STATS,
            CombatKits::from_bits(CombatKits::MELEE | CombatKits::RANGED),
        );
        assert_eq!(c, fighter());
    }

    #[test]
    fn swap_window_counts_from_the_swap() {
        let mut c = fighter();
        assert!(!c.in_swap_window(8));
        assert!(c.request_swap(30));
        assert!(!c.request_swap(30), "cooldown refuses a second swap");
        c.advance_stance(8);
        assert!(c.in_swap_window(8));
        c.advance_stance(1);
        assert!(!c.in_swap_window(8));
        assert_eq!(c.stance.swap_cooldown(), 21);
    }

    #[test]
    fn poise_grace_applies_to_every_fighter() {
        let mut c = fighter();
        assert!(c.poise_hit(60, 60, false, false, false));
        assert!(!c.poise_hit(60, 60, false, false, false), "inside grace");
        c.tick(BREAK_GRACE_TICKS, false, false);
        assert!(c.poise_hit(60, 60, false, false, false));
        c.tick(BREAK_GRACE_TICKS, false, false);
        assert!(!c.poise_hit(60, 60, false, true, false), "blocked");
    }

    const DODGING: ActionProtection = ActionProtection {
        authored: None,
        dodge: true,
    };

    #[test]
    fn the_swap_window_protects_any_fighter_for_its_own_tick_count() {
        // The player and an enemy hold the same Combatant; only the count,
        // each character's own data, differs.
        for ticks in [10u8, 6] {
            let mut c = fighter();
            assert!(!c.invulnerable(ticks, ActionProtection::NONE), "settled");
            assert!(c.request_swap(30));
            for elapsed in 0..=u16::from(ticks) {
                assert!(c.invulnerable(ticks, ActionProtection::NONE), "{elapsed}");
                c.advance_stance(1);
            }
            assert!(!c.invulnerable(ticks, ActionProtection::NONE));
        }
    }

    #[test]
    fn a_fighter_that_never_swaps_is_always_hittable() {
        let mut c = fighter();
        for _ in 0..200 {
            assert!(!c.invulnerable(10, ActionProtection::NONE));
            c.advance_stance(1);
            c.tick(1, false, false);
        }
        assert!(!Defender::OPEN.invulnerable());
    }

    #[test]
    fn an_authored_window_replaces_the_dodge_on_every_fighter() {
        let c = fighter();
        let open = ActionProtection {
            authored: Some(true),
            dodge: false,
        };
        let closed = ActionProtection {
            authored: Some(false),
            dodge: true,
        };
        assert!(c.invulnerable(10, open), "an open window protects");
        assert!(c.invulnerable(10, DODGING), "no window: the dodge decides");
        assert!(
            !c.invulnerable(10, closed),
            "a closed window ends the dodge"
        );
        // A closed window does not end the swap window.
        let mut swapping = fighter();
        assert!(swapping.request_swap(30));
        assert!(swapping.invulnerable(10, closed));
        let target = Defender {
            combatant: &swapping,
            swap_invulnerable_ticks: 0,
            parry_ticks: 0,
            action: open,
        };
        assert!(target.invulnerable());
    }

    fn strike(channel: VitalityChannelId, damage: u16, poise: u16) -> Strike {
        Strike {
            attacker: CombatantId::entity(3),
            kind: HitKind::Melee,
            channel,
            damage,
            poise,
            crit: false,
        }
    }

    const UNARMORED: PoiseRule = PoiseRule {
        armored: false,
        blocked: false,
    };

    fn hit(strike: Strike) -> HitIn {
        HitIn {
            strike,
            stats: STATS,
            poise: UNARMORED,
        }
    }

    #[test]
    fn one_stance_rule_scales_only_opposite_hits() {
        assert_eq!(stance_scaled(&STATS, false, 32, 48), (32, 48));
        assert_eq!(stance_scaled(&STATS, true, 32, 48), (40, 60));
        assert_eq!(stance_scaled(&STATS, true, 0, 0), (0, 0));
        let custom = CombatStats {
            opposite_damage_q12: 8192,
            opposite_poise_q12: 2048,
            ..STATS
        };
        assert_eq!(stance_scaled(&custom, true, 32, 48), (64, 24));
    }

    #[test]
    fn the_one_bar_takes_the_scaled_hit() {
        let mut c = fighter();
        // Matching stance (side One is active): taken as authored.
        let out = c.receive_hit(hit(strike(VitalityChannelId::One, 32, 48)));
        assert_eq!(out.damage_applied, 32);
        assert!(!out.poise_broken, "48 of 60");
        assert_eq!(c.health(), 168);
        // Opposite stance: 1.25x, off the same bar.
        let out = c.receive_hit(hit(strike(VitalityChannelId::Two, 96, 0)));
        assert_eq!(out.damage_applied, 120);
        assert_eq!(c.health(), 48);
        assert!(!out.defeated);
        let out = c.receive_hit(hit(strike(VitalityChannelId::One, 48, 60)));
        assert!(out.killed && out.defeated);
        assert!(!out.poise_broken, "a killing blow leaves posture alone");
    }

    #[test]
    fn an_opposite_hit_breaks_posture_sooner() {
        let mut matching = fighter();
        let out = matching.receive_hit(hit(strike(VitalityChannelId::One, 0, 50)));
        assert!(!out.poise_broken, "50 of 60");
        let mut opposite = fighter();
        let out = opposite.receive_hit(hit(strike(VitalityChannelId::Two, 0, 50)));
        assert!(out.poise_broken, "50 x 1.25 = 62 of 60");
    }

    #[test]
    fn a_hit_bigger_than_the_bar_is_lethal() {
        let mut c = fighter();
        let out = c.receive_hit(hit(Strike {
            kind: HitKind::Hazard,
            ..strike(VitalityChannelId::One, 1000, 0)
        }));
        assert_eq!(c.health(), 0);
        assert_eq!(out.damage_applied, 200);
        assert!(out.defeated && out.killed);
        let again = c.receive_hit(hit(strike(VitalityChannelId::One, 5, 0)));
        assert!(
            again.defeated && !again.killed,
            "only the emptying hit kills"
        );
    }

    #[test]
    fn a_poise_only_hit_leaves_the_bar_alone() {
        let mut c = fighter();
        let out = c.receive_hit(hit(strike(VitalityChannelId::Two, 0, 60)));
        assert_eq!(c.health(), 200);
        assert_eq!(out.damage_applied, 0);
        assert!(out.poise_broken);
    }

    #[test]
    fn restoring_tops_up_health_and_energy_to_their_ceilings() {
        let mut c = fighter();
        c.health.apply_damage(150);
        assert!(c.flow.spend_shot(&STATS));
        assert_eq!(c.restore(30, 5, &STATS), 30);
        assert_eq!((c.health(), c.flow.energy), (80, 85));
        assert_eq!(c.restore(500, 500, &STATS), 120);
        assert_eq!((c.health(), c.flow.energy), (200, 100));
    }

    #[test]
    fn an_arch_restores_health_and_energy_on_one_beat() {
        let mut c = fighter();
        c.health.apply_damage(100);
        c.flow.energy = 0;
        for _ in 0..60 {
            assert!(!c.air_tick(true, false, false, &STATS));
        }
        assert_eq!((c.health(), c.flow.energy), (120, 20));
        for _ in 0..60 {
            c.air_tick(true, false, true, &STATS);
        }
        assert_eq!((c.health(), c.flow.energy), (120, 20), "firing pauses both");
    }

    fn swapped() -> Combatant {
        let mut c = fighter();
        assert!(c.request_swap(30));
        c.advance_stance(1);
        c
    }

    #[test]
    fn a_fighters_hit_early_in_the_swap_is_parried_with_all_four_effects() {
        let mut c = swapped();
        c.flow.energy = 0;
        let out = c.receive_hit(hit(strike(VitalityChannelId::One, 50, 50)));
        assert!(out.parried);
        assert_eq!((out.damage_applied, out.poise_broken), (0, false));
        assert_eq!(c.health(), 200);
        assert_eq!(c.flow.energy, STATS.parry_refund);
        assert_eq!(c.stance.swap_cooldown(), 0);
        assert_eq!(c.crit, STATS.crit_window_ticks);
        // The riposte the owner sends back breaks any posture as a stagger.
        let mut attacker = fighter();
        let riposte = attacker.receive_hit(HitIn {
            poise: PoiseRule {
                armored: true,
                blocked: false,
            },
            ..hit(Strike::parry(CombatantId::PLAYER, VitalityChannelId::One))
        });
        assert!(riposte.poise_broken && !riposte.knocked_down && !riposte.parried);
    }

    #[test]
    fn past_the_parry_window_or_from_the_world_nothing_is_parried() {
        let mut c = swapped();
        c.advance_stance(STATS.parry_ticks);
        assert!(
            !c.receive_hit(hit(strike(VitalityChannelId::One, 5, 0)))
                .parried
        );
        let mut c = swapped();
        let world = Strike {
            attacker: CombatantId::WORLD,
            ..strike(VitalityChannelId::One, 5, 0)
        };
        assert!(!c.receive_hit(hit(world)).parried);
        let mut off = swapped();
        let no_parry = HitIn {
            stats: CombatStats {
                parry_ticks: 0,
                ..STATS
            },
            ..hit(strike(VitalityChannelId::One, 5, 0))
        };
        assert!(
            !off.receive_hit(no_parry).parried,
            "zero turns parrying off"
        );
    }

    #[test]
    fn a_crit_scales_an_attack_started_inside_the_window_until_spent() {
        let mut c = fighter();
        assert_eq!(c.crit_scaled(&STATS, 10, 3), (10, 3));
        c.crit = STATS.crit_window_ticks;
        // Armed but no attack started yet: nothing scales.
        assert_eq!(c.crit_scaled(&STATS, 10, 3), (10, 3));
        c.start_attack();
        assert_eq!(c.crit_scaled(&STATS, 10, 3), (20, 6));
        // The attack keeps its crit after the window runs out.
        for _ in 0..STATS.crit_window_ticks {
            c.tick(1, false, false);
        }
        assert_eq!(c.crit, 0);
        assert_eq!(c.crit_scaled(&STATS, 10, 3), (20, 6));
        assert!(c.spend_crit());
        assert!(!c.spend_crit());
        assert_eq!(c.crit_scaled(&STATS, 10, 3), (10, 3));
        // An attack started after the window ran out does not crit.
        c.crit = 2;
        c.tick(1, false, false);
        assert_ne!(c.crit, 0);
        c.tick(1, false, false);
        assert_eq!(c.crit, 0);
        c.start_attack();
        assert!(!c.crit_attack);
    }

    #[test]
    fn the_crit_window_waits_for_the_swap_animation() {
        let mut c = fighter();
        c.crit = STATS.crit_window_ticks;
        // A 72-tick swap and a 12-tick swap leave the same usable window.
        for _ in 0..72 {
            c.tick(1, false, true);
        }
        assert_eq!(c.crit, STATS.crit_window_ticks);
        c.tick(1, false, false);
        assert_eq!(c.crit, STATS.crit_window_ticks - 1);
    }

    #[test]
    fn only_a_crit_break_knocks_down_and_a_downed_fighter_cannot_break_again() {
        let mut c = fighter();
        let out = c.receive_hit(hit(strike(VitalityChannelId::One, 0, 60)));
        assert!(
            out.poise_broken && !out.knocked_down,
            "an ordinary break staggers"
        );
        assert_eq!(c.knockdown, 0);
        c.tick(BREAK_GRACE_TICKS, false, false);
        let crit = Strike {
            crit: true,
            ..strike(VitalityChannelId::One, 0, 60)
        };
        let out = c.receive_hit(hit(crit));
        assert!(
            out.poise_broken && out.knocked_down,
            "a crit break knocks down"
        );
        assert_eq!(c.knockdown, STATS.knockdown_ticks);
        c.tick(BREAK_GRACE_TICKS, false, false);
        c.tick(BREAK_GRACE_TICKS, false, false);
        c.knockdown = 5;
        let out = c.receive_hit(hit(Strike {
            damage: 7,
            poise: 1000,
            ..crit
        }));
        assert_eq!(out.damage_applied, 7);
        assert!(!out.poise_broken, "down: no second break");
        c.tick(5, false, false);
        assert!(!c.knocked_down());
        let mut off = fighter();
        let out = off.receive_hit(HitIn {
            stats: CombatStats {
                knockdown_ticks: 0,
                ..STATS
            },
            ..hit(crit)
        });
        assert!(
            out.poise_broken && !out.knocked_down,
            "zero turns knockdown off"
        );
    }

    #[test]
    fn one_armour_rule_authored_window_first_then_heavy_contact() {
        let both = CombatStats::DEFAULT;
        assert_eq!(
            both.armour,
            combat_armour::HEAVY_ATTACK | combat_armour::AUTHORED_WINDOW
        );
        assert!(armoured(&both, Some(true), false));
        assert!(!armoured(&both, Some(false), true), "the window decides");
        assert!(
            armoured(&both, None, true),
            "no window: the heavy's contact"
        );
        assert!(!armoured(&both, None, false));
        let heavy_only = CombatStats {
            armour: combat_armour::HEAVY_ATTACK,
            ..both
        };
        assert!(armoured(&heavy_only, Some(false), true));
        let none = CombatStats { armour: 0, ..both };
        assert!(!armoured(&none, Some(true), true));
    }

    #[test]
    fn a_downed_fighter_is_protected_by_the_one_downed_rule() {
        let mut c = fighter();
        assert!(!c.invulnerable(10, ActionProtection::NONE));
        c.knockdown = 3;
        assert_eq!(c.invulnerable(10, ActionProtection::NONE), DOWNED_PROTECTED);
        c.tick(3, false, false);
        assert!(!c.invulnerable(10, ActionProtection::NONE), "up again");
    }

    #[test]
    fn only_a_fighter_with_both_kits_swaps() {
        let both = CombatKits::from_bits(CombatKits::MELEE | CombatKits::RANGED);
        let melee = CombatKits::from_bits(CombatKits::MELEE);
        let ranged = CombatKits::from_bits(CombatKits::RANGED);
        assert!(both.can_swap() && !melee.can_swap() && !ranged.can_swap());
        assert_eq!(starting_stance(melee), VitalityChannelId::One);
        assert_eq!(starting_stance(both), VitalityChannelId::One);
        assert_eq!(starting_stance(ranged), VitalityChannelId::Two);
        assert_eq!(starting_stance(CombatKits::NONE), VitalityChannelId::One);
    }

    #[test]
    fn a_fighter_stays_small() {
        // Every entity slot holds one: 32 B before parry, crit and knockdown,
        // 36 before the crit latch.
        assert_eq!(core::mem::size_of::<Combatant>(), 38);
    }

    /// A crit follows the parry that armed it, so it lands on a fighter
    /// staggered by that parry's riposte (poise blocked, break grace open).
    /// Its posture damage still counts and the break is a knockdown; the
    /// same strike without the crit does nothing to posture.
    #[test]
    fn a_crit_breaks_a_riposted_fighter_into_a_knockdown() {
        let staggered = PoiseRule {
            armored: false,
            blocked: true,
        };
        let riposted = || {
            let mut c = fighter();
            let out = c.receive_hit(hit(Strike::parry(
                CombatantId::entity(3),
                VitalityChannelId::One,
            )));
            assert!(
                out.poise_broken && !out.knocked_down,
                "the riposte staggers"
            );
            c
        };
        let blow = strike(VitalityChannelId::One, 10, STATS.poise);
        let mut plain = riposted();
        let out = plain.receive_hit(HitIn {
            poise: staggered,
            ..hit(blow)
        });
        assert!(!out.poise_broken && !out.knocked_down);
        assert_eq!(out.damage_applied, 10, "damage lands either way");
        let mut critted = riposted();
        let out = critted.receive_hit(HitIn {
            poise: staggered,
            ..hit(Strike { crit: true, ..blow })
        });
        assert!(out.poise_broken && out.knocked_down);
        assert_eq!(critted.knockdown, STATS.knockdown_ticks);
        // A knockdown still blocks it: no chain of crit knockdowns.
        let again = critted.receive_hit(hit(Strike { crit: true, ..blow }));
        assert!(!again.poise_broken);
    }

    #[test]
    fn one_reaction_rule_for_every_fighter() {
        let open = Posture {
            flinches: true,
            ..Posture::default()
        };
        let hit = HitOut::default();
        let broke = HitOut {
            poise_broken: true,
            ..HitOut::default()
        };
        let down = HitOut {
            knocked_down: true,
            ..broke
        };
        // An ordinary swing flinches; armour, a missing clip, a bolt or a
        // reaction already playing holds it.
        assert_eq!(open.reaction(HitKind::Melee, &hit), Some(Reaction::Flinch));
        let armored = Posture {
            armored: true,
            ..open
        };
        assert_eq!(armored.reaction(HitKind::Melee, &hit), None);
        let no_clip = Posture {
            flinches: false,
            ..open
        };
        assert_eq!(no_clip.reaction(HitKind::Melee, &hit), None);
        for kind in [HitKind::Projectile, HitKind::Hazard, HitKind::Parry] {
            assert_eq!(open.reaction(kind, &hit), None);
        }
        let flinching = Posture {
            reacting: Some(Reaction::Flinch),
            ..open
        };
        assert_eq!(flinching.reaction(HitKind::Melee, &hit), None);
        // A break staggers through armour and over a flinch; a riposte's
        // break plays the Parried stagger; a crit's break knocks down.
        assert_eq!(
            armored.reaction(HitKind::Melee, &broke),
            Some(Reaction::Stagger)
        );
        assert_eq!(
            flinching.reaction(HitKind::Melee, &broke),
            Some(Reaction::Stagger)
        );
        assert_eq!(
            open.reaction(HitKind::Parry, &broke),
            Some(Reaction::Parried)
        );
        assert_eq!(
            open.reaction(HitKind::Melee, &down),
            Some(Reaction::Knockdown)
        );
        // A kill or a parry is no reaction.
        let killed = HitOut {
            killed: true,
            ..HitOut::default()
        };
        assert_eq!(open.reaction(HitKind::Melee, &killed), None);
        // Only a break reaction refuses another break.
        assert!(!flinching.rule().blocked);
        for reacting in [Reaction::Stagger, Reaction::Parried, Reaction::Knockdown] {
            let held = Posture {
                reacting: Some(reacting),
                ..open
            };
            assert!(held.rule().blocked);
            assert_eq!(held.reaction(HitKind::Melee, &hit), None);
        }
    }
}
