# Two-trigger axis combat: L2 vertical, R2 horizontal, 3 charge levels

Proposal. Bloodborne-framed: no blocking, dodge is the defensive verb, aggression
is the reward loop. **R2 = WIDE, L2 = DEEP.** Every rule is that one difference
applied elsewhere; nothing may exist outside this table.

| System | Horizontal (R2) | Vertical (L2) |
| --- | --- | --- |
| Shape | Wide, shallow | Deep, narrow |
| Player attack | Hits several, buys space | Hits one, buys time and damage |
| Gauge / break | FOOTING: knocked down, you stay mobile | STUN: rooted burst window |
| Resource it pays | Stamina | Health |
| Dismembers | Legs, makes a crawler | Head, makes a berserker |
| Immunity created | Crawler cannot be swept | Berserker cannot be stunned |
| Charge 1 / 2 / 3 | Slash / sweep / **cross slice** | Chop / overhead / **double attack** |
| Counter-hit on `Windup` | Displaces out of range | Interrupts the attack |
| Enemy attack of this shape | Wide sweep | Deep chop |
| ...dodge that answers it | **Back**, leave the radius | **Aside**, leave the line |
| ...counter it sets up | Your **vertical** | Your **horizontal** |
| Boss (tall) | Sweep legs, always available | Chop head, only after a knockdown |

Enemy body decides which axis *works*; your depleted resource decides which you
*need*. When they disagree, solve the room instead of the enemy.

## Already exists, do not rebuild

| Piece | Where |
| --- | --- |
| `button::L2` / `R2`; bindings with no charge or buffering | `psx-pad/lib.rs:71`, `runtime_config.rs:157`, `playtest_update.rs:533` |
| Tap-vs-hold precedent (CIRCLE tap roll / hold run) | `main.rs:936`, `EVADE_RUN_HOLD_VBLANKS = 8` |
| Quickstep, directional dodges, per-character i-frames | `character.rs:59-62,163`, `entities.rs:1125` |
| Enemy arc: `reach` + `half_angle` + point-blank override | `combat.rs:324`, `arc_hits_circle:481` |
| 6 action slots (`Alt*` = "heavy weapon class"), clip fallbacks | `psx-level/lib.rs:3143`, `character.rs:347` |
| Capsule hitboxes with frame windows, multi-target arc sweep | `combat.rs`, `entities.rs:729` |
| Poise, stagger, health, `Windup`/`Recover` states | `entities.rs:42,429,661` |
| Per-entity collision cylinders (downed stops being a wall) | `playtest_update.rs:601` |
| `ModelPart` ranges (no visibility mask yet) | `psx-asset/lib.rs:672` |
| `EquipmentRecord`: one weapon+socket each, multiple allowed | `psx-level/lib.rs:3001` |

**Missing:** charge input machine, second gauge, per-attack `half_angle`,
resource economy, part visibility, clips.

## Rules

**Input.** Digital triggers, 60 Hz, so charge is hold-ticks (`L2 = 12`, `L3 =
30`, feel knobs). Fire on press, branch on release, else level 1 pays the tap as
latency. Buffer presses during dodge recovery. Charging is rooted with rotation
allowed (verified DS behaviour); dodge out before the swing, no cancel after.

**Breaks must stay asymmetric** or it is two stuns with different animations:

| | FOOTING (R2) | STUN (L2) |
| --- | --- | --- |
| Buys / role | Space and tempo, opener | Damage, closer |
| Damage while broken | Reduced, chip only | Multiplied |
| Targets / duration | Everything in arc, ~2-3 s | One, 45 ticks |
| Ends early if hit | **Yes**, so cashing costs tempo | No |
| Blocks your movement | **No**, cylinder dropped | Yes |
| Wake-up | Aggressive get-up, greedy punish is a trap | Plain recovery |

Sweep a tall enemy down and its head enters vertical reach: the axes chain. **No
hit zones**, the axis picks the gauge directly.

**Defence.** I-frames whiff any swing, but `arc_hits_circle` also fails on
geometry, so the shape decides the escape: a wide sweep covers the sides
(**timing** test), a deep chop is thin (**positioning** test). Survival is
timing, tempo is positioning: the correct dodge leaves you where your counter
lands. That replaces blocking. Needs `GAME_ENTITY_ATTACK_HALF_ANGLE` moved from a
constant onto the attack; keep `ARC_POINT_BLANK`. **I-frame generosity is the
most important feel value in the system**: too generous and the axis stops
mattering.

**Charge is a weapon loadout**, not a power meter: L1 small, L2 big, L3 both
hands. The tell is the silhouette, and reach changes per level so releasing wrong
whiffs. Two PLAYER `EquipmentRecord`s already render two weapons; `combat.rs`
resolving the arc from *the first* one must become level-selected.

**Dismemberment is the enemy variety** (roster is one enemy plus one boss): one
model yields intact / crawler / berserker, authored by the player's own damage,
and losing a part kills that axis so spamming self-defeats. Geometry is nearly
free (`ModelPart` + per-instance bitmask). **Animation is the real cost**, so
prototype with placeholders (crawler = downed pose shuffling, berserker =
existing attack at random headings) before authoring anything.

**Economy: DOOM push-forward, no glory kills.** A broken enemy leaks resource
into whoever keeps hitting it (STUN pays health, FOOTING stamina, at `apply_hit`).
**Cut passive regen** or none of it matters. With no fodder: every enemy is a
two-tap battery whose taps dismemberment closes permanently, a kill always pays a
trickle (anti-deadlock), and the tall boss always pays stamina but gates health
behind a knockdown. `Block` stays unused; enemies threaten with spacing and
telegraphed wind-ups.

## Milestones

| # | Milestone | Gate |
| --- | --- | --- |
| 1 | Charge input machine on existing actions | Host test: hold ticks map to levels, incl. exact threshold |
| 2 | Dodge-cancel and buffering | Host test: fires once at dodge end, never mid-i-frames |
| 3 | Rooted charge with rotation | Headless capture: facing changes, position does not |
| 4 | Two gauges, two breaks | Extend poise tests: both breaks, shorten-on-hit, cylinder dropped |
| 5 | Per-attack `half_angle` | Host test: lateral defender hit by wide, missed by narrow, both hit point-blank |
| 6 | Dodge profit + i-frame tuning | Tape: correct dodge counters, panicked one only survives |
| 7 | Counter-hit and economy | Host tests: health only from STUN, stamina only from FOOTING, no regen above floor |
| 8 | Dismemberment, placeholder clips | Tape: forced axis switch is fun. **If not, stop, cost was nothing** |
| 9-11 | Weapon per level, real clips, hardware | Multi-frame dumps; then trigger and i-frame feel on a real pad |

1-8 need no art and answer every design question. Nothing here touches the ~10k
cycle combat line; the only real cost is clip RAM.

**Open:** six attacks do not fit the existing 3+3 (add explicit actions, keep
`Alt*` = big weapon, one-way door); L2+R2 reserved; L3 auto-fire or hold; economy
numbers and stamina floor; i-frame tightness; weapon-spec selection per level;
dismemberment for the player?; clip RAM budget.

## Rejected, do not re-propose

Guard axis / stance matching (user rejected: dodging, not defence reading) ·
per-enemy resistance stats (invisible at 320x240) · hit zones as a prerequisite
(the axis already picks the gauge) · boss gauges per body region (hit zones
renamed, off-axis) · Bloodborne rally (reactive, replaced by the DOOM economy) ·
glory kills (per-enemy clip RAM) · fodder as the resource bank (roster has none)
· height dimension (no jump or crouch) · swing vs world collision (a whole
feature) · **the clash, deferred not dead** (re-imports axis matching, needs
per-axis reactions).

Sources: [Bloodborne combat](https://bloodborne.wiki.fextralife.com/Combat),
[DS3 Charge](https://darksouls3.wiki.fextralife.com/Charge),
[Doom Eternal resources](https://help.bethesda.net/app/answers/detail/a_id/49879/~/why-do-i-keep-running-out-of-health-and-ammo-in-doom-eternal).
