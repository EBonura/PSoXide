# Cortex 0.4 points of interest: tutorial audit and item beacons

Date: 2026-09-04. Project: `editor/projects/cortex-ignition-tech-demo-0.4`.

## What is placed today

Six Points of Interest, all in room 0. World units, then what the beacon does.

| Node | Position (x, y, z) | Prompt | Pages | Reward |
|---|---|---|---|---|
| 162 | (-10624, 512, -20608) | READ | "Circle: Sprint" | none |
| 166 | (-6144, 512, -20864) | READ | "R1/R2: Attack" | none |
| 164 | (-5504, 1792, -13440) | READ | "Triangle: Swap stance" | none |
| 117 | (384, 1792, -12672) | TAKE | "RUPTURE FILAMENT RECOVERED." | Rupture Filament |
| 119 | (-5376, 1536, -5120) | TAKE | "ZENITH CONDENSER RECOVERED." | Zenith Condenser |
| 133 | (10752, 2816, -26880) | TAKE | "INERTIAL SHELL RECOVERED." | Inertial Shell |

Player spawn is (-10752, 953, -26112). The first enemy (Intake Custodian) stands
at (18688, 3584, -19456); the other two Custodians at (29440, 256, -512) and
(20992, -1024, -10240); the Heavy Enemy at (46080, -1024, -10752).

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

## Verdict and proposed beacon set

The three placed beacons cover dash, attack, and swap. They miss evade,
lock-on, the stance damage rule, and sockets. Asylum-style: one idea per
beacon, on the path, before the thing it prepares you for.

Suggested pages, in walking order from spawn. Copy is a suggestion; the cook
validates line wrapping against the 244 px Archive panel, so keep each page
to two short lines.

1. Keep at (-10624, 512, -20608), replace "Circle: Sprint" with
   `["Circle: tap to evade,", "hold to sprint."]` (two pages or one wrapped page).
2. Keep at (-6144, 512, -20864), replace "R1/R2: Attack" with
   `["R1: light attack.", "R2: heavy attack."]`.
3. Keep at (-5504, 1792, -13440), replace "Triangle: Swap stance" with
   `["Triangle: swap stance.", "The resting pool recovers."]`.
4. NEW, between the swap beacon and the first Custodian (around x 8000..14000
   on the way to (18688, 3584, -19456)):
   `["R3: lock on."]`.
5. NEW, in sight of the first Custodian:
   `["Enemies guard their active stance.", "Strike the other one."]`.
6. NEW, just past the Rupture Filament at (384, 1792, -12672):
   `["Socket modules from the", "inventory."]` The button that opens the
   inventory is driven by the UI scene, so confirm the label before authoring.

Stamina (sprint and evade cost it) is worth a seventh beacon only if
playtesters run dry; the bar is on the HUD.

Style: the item pages shout in caps ("RUPTURE FILAMENT RECOVERED.") while the
tutorials are sentence case. Pick one. Item pickups already get the
"ITEM ACQUIRED - name" panel after the page closes, so the page itself could
be flavour ("A filament, still warm.") rather than a restatement.

## Telling message beacons from item beacons

The record already knows: `InteractableRecord::reward_resource` is
`POI_REWARD_NONE` for a message. Two changes, both keyed on that:

1. Prompt verb. Item beacons now say `X - TAKE`, messages `X - READ`. Data only
   (the `prompt` field on the three item POIs).
2. Beacon colour. `marker_runtime::ArchiveBeaconKind` re-hues the ember palette
   to cyan for item beacons through `archive_beacon_tint`; message beacons are
   untouched. The editor viewport preview (`editor_preview/poi.rs`) applies
   the same tint so the two match in the editor. Depleted beacons dim as
   before in both hues.

Screenshots from the whole-level tape (`editor/archive/fixtures/cortex-0.4/whole-level.pxtape`)
on a disc built from the live project, in `cortex-poi-audit-2026-09-04/`:

- `message-beacon-read.png` (+ `-zoom`): a tutorial beacon in the spawn
  hallway, ember red, `X - READ` prompt.
- `item-beacon-cyan.png` (+ `-zoom`): the Rupture Filament beacon seen from
  the doorway of the blue-lit room, cyan body and frame.
