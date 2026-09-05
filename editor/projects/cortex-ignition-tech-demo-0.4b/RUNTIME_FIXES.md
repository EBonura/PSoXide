# Runtime fixes — 2026-09-04

The v0.4 map and all eight POI placements remain unchanged. Changes apply to the v0.4b project and the shared cooker/runtime.

## Music

Restored `menu2.wav` to all three menu Music nodes and `cortex_boss.wav` to a Combat-triggered HUD node, preserving the original volumes and music slider. Recovered the two exact WAV files from the tracked v0.5 revision without restoring that deleted project. Both cooked CD-DA payloads match the source WAV PCM. Native replay requests track 3 in the menu and track 2 when combat engages, then reports combat ending.

## Attack sound

The attack event, cue, WAV and cooked sample were present. The percussive SPU envelope had already decayed to zero by the swing waveform's audible onset at 30–40 ms. Gameplay samples now use `Adsr::sample_one_shot`: sustained full level through the waveform, fast exponential release, and the existing silence-block repeat address. This preserves the authored waveform without the indefinite release of `Adsr::sample`.

A stationary native replay triggers four attacks, including both stances. Before the fix, their audio windows were silent. Afterward, each window peaks at 12,293/32,767 with 15,894 nonzero stereo samples. `review/runtime-fixes/native-attack.wav` is an actual emulator mix capture, not the source WAV. The regression test also verifies envelope level at 40 ms and release to silence.

## Lighting

The project already used Release shadow baking. After scaling authored sector size 1,024 to 64 engine units, `world_sector_size_for_node` snapped it back to the authoring minimum, 128. This doubled baked and dynamic light radii and affected actor/marker clearance. The accessor now preserves normalized engine units; authoring/load normalization still performs editor snapping.

All 8,928 drawable BSP vertices match the authored-space light evaluation exactly after accounting for their surface normals and material tints. This verifies light reach, color and occlusion at baked vertices; it does not claim pixel-identical editor and native images, which use different camera/mesh rendering paths.

## POI ordering

Message and collectible panels now use the same half-sector floor clearance as actors, replacing the marker-only two-sector offset. Their outline retains that clearance with a two-unit local offset. A regression tests a marker behind a player in both BSP and grid depth policies. `review/runtime-fixes/marker-behind-player.png` shows a native diagnostic spawn aligned with the first marker: the shaft is occluded by the body. Only the temporary probe changed the player spawn. The additive light shaft uses the same world options as the panel.

## Validation

- 442 project library tests pass, including the scaled light-reach regression.
- 414 engine library tests pass.
- SPU delayed-onset/release regression passes; POI depth regression passes.
- Release frontend and native guest build successfully.
- Native whole-level tape reaches poll 5,250; separate audio replay verifies four swings.
- Disc structural check passes: boot files, WORLD.PAK, UI.PAK, two audio tracks.
- Verification logs, mixed audio, captures and SHA-256 receipt: `review/runtime-fixes/`.

Physical-console audio release and a manual complete combat playthrough remain untested. No geometry or texture redesign was performed.

## Gameplay and presentation follow-up

- Dismiss icon and localized text align to the message panel's bottom right, with two additional pixels of bottom clearance.
- Player stance swap cooldown is 900 simulation ticks (15 seconds), up from 180 (3 seconds). The HUD reads the same authored value.
- Lock target height now includes the rendered instance scale; Cortex enemies are approximately 1.63x their base model height. The marker also shifts four screen pixels left and two up, and all its primitives use native 50% average blending.
- Added original 8 kHz mono mechanical footsteps (120 ms) and idle servo vocalizations (300 ms), reproducible with `tools/generate_enemy_sfx.py`. Footsteps require actual movement and an animation foot-down phase. Idle cues occur every six seconds, staggered by actor. Both are restricted to nearby living enemies in the player's room; the shared event mask coalesces simultaneous cues. Volumes are 48% and 36% respectively. These are proximity-gated cues, without stereo positional attenuation.
- Hybrid enemies were using projectile maximum range as their desired spacing. During cooldowns this caused retreat to a firing ring instead of advancing to the authored preferred distance. The melee entry threshold also excluded the outer spacing tolerance, allowing enemies to stop just outside melee. Both now use the same preferred band. Recovery no longer repeats the initial acquisition reaction delay. Attack cooldowns and already committed attacks remain intact.
- The localized-text validation accepts authored newlines as layout controls while continuing to reject unsupported glyph bytes.

Validation: 191 game-runtime tests, 414 engine tests, 442 project tests and all 54 playtest tests pass. The new AI regression uses Cortex's cooked distances/timings and verifies ranged opening followed by melee against a stationary player at both one- and two-tick NPC cadences. Enemy audio cadence is also tested at both rates. Native replay reaches poll 5,250; disc structural validation passes and the executable has zero scanned load-delay hazards. Captures, build/test logs, replay input and disc checksum are in `review/gameplay-polish/`. Console verification remains outstanding.

## Combat state machine, poise and camera

The approved poise baseline is implemented in the rebuilt v0.4b disc.

### Shared swap delay and enemy decisions

Found a follow-up bug in the earlier cooldown change: `init_gameplay` loaded the authored stance configuration and then overwrote it with `DEFAULT`. The effective player cooldown was therefore still 30 ticks, despite the authored value and previous note above saying 900. Initialization now applies defaults before reading the controller. Enemies receive the same final player cooldown after every spawn and respawn: 900 simulation ticks, or 15 seconds. Their guard changes remain a recovery-state transition with a visible 12-tick tell; repeated recovery transitions cannot bypass the cooldown. The four enemy controllers also store 900 for consistent authoring.

The combat director retains one committed attacker at a time, fair waiting priority, distance-based melee/ranged selection and explicit windup/recovery windows. An enemy that spends 120 ticks chasing with the attack slot now yields it and waits at least 60 ticks before retrying, so an obstructed pursuer cannot monopolize the group. No player button reading or post-release retargeting is introduced.

### Poise and interruption

| Actor | Poise | Ordinary light hit (25) | Heavy hit (50) |
| --- | ---: | --- | --- |
| Player | 20 | Interrupts | Interrupts |
| Light enemy | 25 | Interrupts | Interrupts |
| Heavy enemy | 50 | Two hits within the recovery interval | Interrupts |

Accumulated poise damage clears after 120 quiet ticks (two seconds). Reaching the threshold breaks poise; the previous strict greater-than comparison could let an exact-threshold hit pass. Extra resistance doubles the threshold only during the player's authored heavy hitbox-active frames, or the enemy heavy attack's active state. Windup and recovery remain vulnerable, and this resistance is finite. Dodge invulnerability remains intact.

A player poise break cancels the current motor action and buffered evade, immediately changes to the existing HitReact animation and briefly locks input. Stamina, position and vertical motion are preserved. The authored player reaction plays at 4x speed; reaction duration is clamped to 18–48 ticks. Enemy stagger lasts 32 ticks with the reaction clip at 3x speed. Enemy stagger/death invalidates retained attack tokens and releases the group attack slot, preventing a cancelled swing from connecting later. Existing projectiles already in flight remain independent.

### Ranged facing and camera

The sideways-shot problem combined a committed body bearing with aiming at the player's latest position when the projectile spawned. Enemies now turn at a bounded rate during early ranged windup, stop tracking for the final six ticks, and launch along that committed body bearing. Elevation is sampled during the tell. Projectile velocity remains fixed after launch; the projectile system does not home.

Lock-on camera alignment is less abrupt (64 instead of 128 angle units per update). A small close-contact bearing dead zone prevents a target crossing through the player from forcing a 180-degree orbit flip. R3 with no available target now starts a smooth recenter that completes after releasing the button; manual camera input cancels it. Existing lock/unlock behavior and collision sweeps remain. These choices use the control and combat references in the [official DS3 manual](https://www.fromsoftware.jp/manual/darksouls3/ps4/operation.html) and [PlayStation's Bloodborne guidance](https://blog.playstation.com/2015/03/23/bloodborne-24-tips-for-survival/), rather than claiming an exact reproduction of their camera algorithms.

### Native validation and memory

The first native build exposed a heap exhaustion during loading. Runtime entity, model, instance and clip arrays now size themselves to the cooked tables, with a sentinel slot for the empty editor manifest. This level reserves four entity/instance slots, five model slots and 48 clip slots. No authored content was removed. The final stationary replay reports zero guest faults and 1,536 bytes of remaining heap after gameplay initialization; memory remains tight on the 2 MB target.

- 197 game-runtime, 416 engine, 54 playtest and 442 project library tests pass. New regressions cover poise thresholds/reset/finite armor, cancellation of retained attacks, cooldowns at both NPC cadences, blocked-owner fairness, committed ranged aiming, motor interruption and camera behavior.
- Native cooldown tape attempts swaps at polls 1,000, 1,200, 1,905 and 2,050. Only the first and third succeed, at guest frames 999 and 1,904 (905 ticks apart).
- The existing route tape completes to poll 5,251, with three enemy poise breaks, one player poise break and a projectile release. Menu/combat music requests are still present. Captures show gameplay, combat and the first defeated enemy; this is not a manual complete-level playthrough.
- Mean rendering cost is 1,064,780 cycles against the 1,128,960-cycle budget. The replay still has 580 late visual frames, so this does not establish a locked frame rate.
- The final disc passes the structural check with WORLD.PAK, UI.PAK and both CD-DA tracks; the executable scan reports zero load-delay hazards.

Build logs, tests, native captures, cooldown tape and checksums are in `review/combat-state-machine/`. Physical-console validation and subjective combat/camera tuning remain outstanding.

## Menu music replacement, 5 September 2026

Imported the updated `Cortex Intro.wav` delivery as `assets/audio/cdda/menu2.wav`.
All three existing menu Music nodes keep their path, volume, loop and trigger
settings. Combat music is unchanged. The new file contains 2,607,580 stereo
frames of 44.1 kHz, 16-bit PCM, exactly the previous sample count (4,435 padded
CD audio sectors). This replacement does not change the audio-track layout.

Source WAV SHA256: `a5355af0d22dfe89e0d267a63f23178053b6a3a61fbf4a372e7c277749acbce1`.
PCM SHA256: `13de7c3e60ba3e26bd6b60f11ea9f2bd24276d446d1bc0b75b93322233229fbd`.
The earlier runtime-fixes receipt records the previous music; rebuild cooked
discs to include this replacement.

## Combat music replacement, 5 September 2026

Imported the updated `Cortex Boss (1).wav` delivery as
`assets/audio/cdda/cortex_boss.wav`. The Combat-triggered Music node keeps its
existing volume, slider, loop and playback settings. The updated menu mix is
unchanged. The new combat track contains 3,207,273 stereo frames of 44.1 kHz,
16-bit PCM (72.727279 seconds), replacing 3,399,710 frames (77.090930 seconds).
Its padded CD audio length falls from 5,782 to 5,455 sectors. Subsequent audio
offsets must be regenerated by a fresh disc build; do not reuse the old CUE.

Source WAV SHA256: `18e07a90fa5f674fdfcd92c835a7606b0deead4346d4f3aeb526fc98cb84ce93`.
PCM SHA256: `d06ed3e35fc8b5740df94b3d770f22f84a180f86cd512d1addf2a731b5e71f89`.
Earlier runtime-fixes and disc receipts predate this replacement.
