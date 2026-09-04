# Cortex 0.4 points of interest: tutorial audit and item beacons

Date: 2026-09-04. Project: `editor/projects/cortex-ignition-tech-demo-0.4`.

## What is placed today (after this pass)

Eight Points of Interest, all in room 0. World units, then what the beacon does.
The five tutorial beacons sit clustered by the spawn hallway for Manny to drag
into place in the editor; the two new ones (167/169) were added next to the
existing three.

| Node | Position (x, y, z) | Prompt | Page | Reward |
|---|---|---|---|---|
| 162 | (-10624, 512, -20608) | READ | Hold Circle: sprint. | none |
| 168 (new) | (-9088, 512, -20608) | READ | Tap Circle: evade. Rolling is invulnerable for a moment. | none |
| 170 (new) | (-7680, 512, -20864) | READ | R3: lock on. Press again to release. | none |
| 166 | (-6144, 512, -20864) | READ | R1: light attack. R2: heavy attack. | none |
| 164 | (-5504, 1792, -13440) | READ | Triangle: swap stance. The resting pool recovers. | none |
| 117 | (384, 1792, -12672) | TAKE | RUPTURE FILAMENT RECOVERED. | Rupture Filament |
| 119 | (-5376, 1536, -5120) | TAKE | ZENITH CONDENSER RECOVERED. | Zenith Condenser |
| 133 | (10752, 2816, -26880) | TAKE | INERTIAL SHELL RECOVERED. | Inertial Shell |

Player spawn is (-10752, 953, -26112). The first enemy (Intake Custodian) stands
at (18688, 3584, -19456); the other two Custodians at (29440, 256, -512) and
(20992, -1024, -10240); the Heavy Enemy at (46080, -1024, -10752).

The sockets tutorial is not a beacon. It is a one-time runtime hint that opens
right after the first item pickup's "ITEM ACQUIRED" panel closes (see below).

## Controls the 0.4 runtime actually has

Read from `engine/examples/editor-playtest/src/runtime_config.rs`,
`playtest_update.rs` and `main.rs` on main (03338512 and later).

| Input | Effect |
|---|---|
| Left stick | Move (walk, run past the sprint threshold) |
| Right stick | Camera |
| Circle tap (released within 8 vblanks) | Evade: roll with a stick direction, backstep without |
| Circle hold | Sprint, drains stamina |
| R1 / R2 | Light / heavy attack of the ACTIVE stance |
| Triangle | Swap stance. Only the active pool takes damage and only the resting pool recovers. Refused during the swap cooldown and while the target pool is broken. |
| R3 | Lock on / lock off |
| Cross | Interact: read a beacon, take an item, advance a message page |
| Select | Free orbit camera (debug) |

L1/L2 no longer attack (commit 11d40d32, on main). The live checkout on
`fix/cortex-boss-theme` predates that commit, which is why they still fire there.

Combat rule that matters for the tutorial: an attack drains the enemy pool of
its own stance (Horizon attacks hit Horizon health, Zenith attacks hit Zenith
health). Hitting the stance the enemy is currently IN is guarded (x0.5),
hitting the other one is opposed (x1.5). An enemy falls when both pools are
empty. This is the mechanic nothing in the level explains yet.

## Dark Souls (Undead Asylum) reference

The verbatim message list was not retrievable today: Fextralife pages return
403 to non-browser fetches, the Fandom page returned 402, and wikidot has the
site locked. The concept list below is from memory of the PS3 build, so treat
wording as approximate. The structure (developer messages in the cell
hallway teaching controls, one in the shield cell teaching equipment, one at
the fog-gate stairs teaching the plunge) is confirmed by the Fextralife
walkthrough.

| Asylum message teaches | Cortex 0.4 equivalent | Beacon today |
|---|---|---|
| Move (left stick), camera (right stick) | same | none (universal, skip) |
| Dash: hold Circle + stick | Circle hold: sprint | "Circle: Sprint" |
| Roll: Circle + stick, Backstep: Circle | Circle tap: evade | MISSING |
| Lock on: R3 | R3 | MISSING |
| Attack: R1, Strong attack: R2 | R1 / R2 in the active stance | "R1/R2: Attack" |
| Guard: L1, Parry: L2 | no block or parry | n/a |
| Use item: Square (Estus) | healing is the stance swap (resting pool recovers) | "Triangle: Swap stance" says what, not why |
| Two-hand: Triangle | n/a | n/a |
| Kick / jump attack | n/a | n/a |
| Equip at the menu (shield cell) | module sockets menu after the first pickup | MISSING |
| Plunging attack (fog gate stairs) | n/a | n/a |
| Rest at the bonfire | no checkpoint in 0.4 | n/a |
| Read messages: Cross | first beacon teaches it by existing | fine |

## Verdict and what was done

The three placed beacons covered dash, attack, and swap. They missed evade,
lock-on, and sockets. Manny's calls, 2026-09-04: reword the three, add evade
and lock-on as beacons, and deliver the sockets hint as a post-pickup message
instead of a beacon. Not added (available if wanted): a stance-damage-rule
beacon ("Enemies guard their active stance. Strike the other one."), a
both-pools beacon, a broken-pool beacon, and a stamina beacon.

Style note: the item pages shout in caps while the tutorials are sentence
case. The post-pickup hint follows the item/system caps.

## Telling message beacons from item beacons

The record already knows: `InteractableRecord::reward_resource` is
`POI_REWARD_NONE` for a message. A first attempt tinted item beacons cyan; in
this teal level that was invisible (Manny caught it before it shipped: never
push a visual change without showing frames first). What landed instead,
Souls style:

1. Prompt verb. Item beacons say `X - TAKE`, messages `X - READ`. Data only
   (the `prompt` field on the three item POIs).
2. Light shaft. Every live beacon raises two additive Gouraud quads from the
   floor under the panel: a wide soft column and a narrow bright core, the
   beacon's own hue at the base fading to black at the top (invisible under
   additive blending), six panel heights tall, screen-facing with the width
   taken from the projected panel. Ember red for messages, gold for items.
   It pulses with the panel and brightens in range. `marker_runtime.rs`:
   `ArchiveBeaconKind`, `archive_beacon_tint`, `submit_archive_beacon_shaft`.
3. Item panels are gold-tinted through the same tint function; the editor
   viewport preview (`editor_preview/poi.rs`) applies the same gold so the
   two match in the editor (no shaft in the preview).
4. Depleted beacons (item taken, one-shot message read) keep the dim, still
   panel and lose the shaft. Repeatable messages keep theirs.

Cost: two blended quads per visible live beacon.

## Post-pickup sockets hint

After the first item pickup's "ITEM ACQUIRED - name" panel is dismissed, the
runtime opens `SOCKET RECOVERED MODULES / FROM THE INVENTORY.` through the
legacy `message_overlay` path (a runtime `&'static str`, no cooked page, no
save flag). `Playtest::socket_hint_shown` makes it once per launch. The Cross
press that closed the acquired panel is not allowed to dismiss the hint in the
same tick (`poi_interaction_consumed` guard in `playtest_update.rs`). Like any
legacy overlay it pauses gameplay until dismissed with Cross or Circle.

## Evidence

Frames from `editor/archive/fixtures/cortex-0.4/whole-level.pxtape` on a disc
built from the live project, plus a variant of that tape steered into the
Zenith Condenser's radius (D-pad right for 40 polls, then Cross presses):

- `message-beacon-read.png`: tutorial beacon in the spawn hallway, red shaft,
  `X - READ`.
- `item-beacon-shaft.png`: the Rupture Filament beacon from the doorway of the
  blue-lit room, gold shaft.
- `item-acquired.png`, `socket-hint.png`: the pickup chain on the Zenith
  Condenser, then the one-time hint.
- `item-beacon-depleted.png`: the same beacon after the pickup, no shaft.

Tape gotcha found on the way: polls per tick depend on the build's frame
time, so a sample index maps to a different route tick on every build. Read
the route log's `tape_frame` column for the build under test before patching
samples; the offset here was 38 samples between two builds of the same level.
