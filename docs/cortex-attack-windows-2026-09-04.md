# Cortex attack hit windows (2026-09-04)

Damage was registering at the start of the player's attacks instead of on
the swing. Measured on the whole-level tape (`--counter-log` now carries
`player_attack_starts_total` and `player_melee_hits_total`; the frame is
`player_anim_phase_q12 >> 12` plus the action's authored `frame_start`, see
below). First frame the damage registers, in editor-timeline frames:

| Attack | Swing (weapon trail) | Before | After |
|---|---|---|---|
| Horizon light | 48-68 | no hit (window 24-35 sat in the wind-up) | 52 |
| Horizon heavy | 55-76 | 57 | 57 |
| Zenith light | 28-47 | 15 (no hitbox, unarmed arc) | 37 |
| Zenith heavy | 54-76 | 18 (no hitbox, unarmed arc) | 54 |

Sheets: `cortex-attack-windows-2026-09-04/{light,heavy,zenith-light,zenith-heavy}.png`.

## What was wrong

- Aletha's `Light Sword Active` hitbox was authored at frames 24-35 while
  the Light action plays 26-83 and swings at 48-68: live from the first tick.
- The two Zenith attacks had no hitbox at all. With no authored hitbox for
  the action, `resolve_player_combat_capsules` returns `None` and the runtime
  falls back to the legacy arc; Aletha has no cooked weapon window, so the
  arc is active on every frame (`UNARMED.active_window == None`).
- The Heavy hitbox (55-76) was already on the trail.

Fix: light 48-68; new `Vert Light Sword Active` (28-47) and `Vert Heavy
Sword Active` (54-76) on the same joints/geometry as the Horizon swords.
All four windows equal the weapon-trail windows of the same action.

## Frame numbering, for the next person

The cook trims each action clip to its authored `frame_start..frame_end`
and rebases every hitbox window by `frame_start`, so a cooked
`CombatCapsuleRecord` and the runtime phase both count from the trimmed
clip's frame 0. The editor timeline shows source-clip frames. Editor frame =
runtime frame + `frame_start` (Light 26, Heavy 9, Zenith light 3, Zenith
heavy 6). The hit checks themselves are consistent; only the data was off.

## Cook warning

An attack action with authored hitboxes on *other* actions but none on its
own used to get the unarmed arc silently. The player-character cook now
warns per action ("action VertLightAttack has a clip but no hitbox or
projectile emitter -- the runtime falls back to the unarmed arc, which hits
on every frame"). Checked on the pre-fix data: both Zenith attacks warn; on
the fixed data nothing does. Characters with no hit volumes at all (the
Heavy Enemy) keep the arc without noise.
