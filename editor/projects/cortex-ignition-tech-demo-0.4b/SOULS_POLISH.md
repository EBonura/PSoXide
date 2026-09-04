# Cortex Ignition v0.4b — combat polish

Implemented on `codex/cortex-04b-souls-polish` in the isolated
`PSoXide-cortex-polish` worktree. The existing v0.4b map and industrial palette
remain the basis. The original working checkout belongs to the separate
performance task. Commit `42f493ea` records the inherited working baseline;
the subsequent feature commit contains this pass.

## Play and review

Open `baked/cortex_ignition_tech_demo_0_4b.cue` in the emulator, or open
`project.ron` in the editor. The CUE, BIN, EXE and symbol map are local build
outputs. The disc has the normal original spawn, WORLD.PAK, UI.PAK and both
music tracks. Diagnostic spawn/death probes are separate and are not shipped.

- [Native dash sequence](review/souls-polish/verified/latest-dash.png)
- [Native burst attack](review/souls-polish/verified/native-volley-contact.png)
- [Native heavy encounter, diagnostic spawn](review/souls-polish/verified/native-heavy.png)
- [Cooked heavy alert/turn reel](review/souls-polish/verified/cooked-heavy-contact.png)
- [Exact final-build receipt](review/souls-polish/verified/delivery-receipt.json)
- [Earlier approved fixes and their validation](RUNTIME_FIXES.md)

## What changed

- **Poise and commitment:** the approved baseline is retained: ordinary hits
  interrupt the player/light enemies; heavy enemies withstand a light hit but
  stagger from a heavy hit or short combo. Extra resistance only covers the active
  heavy attack window. Dodge invulnerability remains. Shared player/enemy swap
  delay is 900 ticks (15 seconds), including newly placed character defaults.
- **Enemy decisions:** existing distance-based melee/ranged choice, shared attack
  slots, explicit windup/recovery, and a timeout for obstructed pursuers are
  retained. The light enemy closes after its ranged opening instead of shooting
  forever. Aim stops tracking before release, and projectiles fly straight.
- **Attack buffering:** one late R1/R2 press can chain out of recovery for 12 ticks
  (0.2 seconds). New presses replace old ones. Dodge, successful stance swap,
  stagger and respawn clear the buffer; expired inputs cannot fire later.
- **Dash effects:** eight short-lived world-space samples add launch/settle rings
  and thin amber/teal streaks behind actual collision-resolved movement. Blocked
  dashes create no wake. Teleports/room changes clear it. Storage is under 300 bytes.
- **Hit feedback:** metal sparks distinguish ordinary hits, poise breaks and
  defeats, reusing the bounded impact pool instead of another allocation.
- **Ranged patterns:** the light enemy's generated charge volley releases two
  12-damage/25-poise bolts 36 ticks apart. One enemy in the paired encounter uses
  a longer tell and a single 28-damage/35-poise bolt. Both share one resident clip.
  Each authored emitter fires once per attack; skipped animation frames retain
  events, a full projectile pool retries, and stagger cancels unreleased shots.
- **Missing animation bindings:** generated heavy alert (1.4 seconds) and turning
  foot adjustment (1 second) replace idle fallbacks. Heavy first-notice delay is
  84 ticks to accommodate the alert. The walking pursuit is intentional.
- **Idle binding fix:** an explicit AnimationSet Idle action now wins over the
  legacy fallback, which could otherwise pick the new attack as idle.
- **Guidance and ending:** English/Italian pages explain swap delay, poise,
  chaining, ranged patterns and R3 recenter. Defeating every enabled enemy shows
  a localized Sector Cleared label; empty or truncated encounters cannot win.
- **Respawn fix:** clears old projectiles, impact effects, queued actions, retained
  poses and broken-stance flags, restores both pools and resets enemies. Currency,
  checkpoints and collected items persist.
- **Memory:** model scratch/cache sizes derive from cooked meshes rather than
  worst-case constants. This enables the added clips/effects within PS1 RAM.

The inherited camera changes smooth lock-on rotation, reduce close-contact orbit
flips and let R3 recenter when no target is available. Existing camera collision
sweeps, enemy footsteps/idle sounds, attack audio, POI depth and lighting fixes
remain included. See RUNTIME_FIXES.md for the detailed earlier audit.

## Animation provenance and rebuilding

Used the already-installed free local MoMask model; no paid service or new weights.
Three light candidates used seed 260904; candidate 2 supplied the selected body
motion. Upward-aiming and spinning alternatives were rejected. The refinement
blends the original forward cannon pose in world space and adds two recoil beats.
The cannon is the rig's RightArm. Two heavy candidates used seed 260905; selected
sections were trimmed and retimed. Actual generated lengths differed from the
prompt's requested lengths, so the retained BVHs are the source of truth.

Prompts, source BVHs, candidate GLBs, review renders and editable refinement scenes
are under `review/souls-polish/`. Accepted source GLBs are under
`source_assets/animations/`. Blender refinement scripts are in `tools/` and use
project-relative paths. Run with `blender --background --factory-startup --python`.

Cooking asserts model/skeleton compatibility: light 22 joints, 451 vertices, scale
113; heavy 27 joints, 618 vertices, scale 200. Parent hierarchy matches and source
coordinates differ by at most one local quantization unit. Additional animation
bounds are disabled. The heavy must use the retained original Idle 2 reference;
the walking-pack idle produces a different quantization frame and was rejected.
Cooked additions are 17,268 bytes (volley), 7,796 (alert), and 5,636 (turn).

From the repository root, the checked-in cooked assets can be rebuilt with:

```sh
cargo run -p psxed-project --example cook_cortex_motion -- editor/projects/cortex-ignition-tech-demo-0.4b
cargo run -p psxed-project --example cook_cortex_heavy_motion -- editor/projects/cortex-ignition-tech-demo-0.4b
sh editor/projects/cortex-ignition-tech-demo-0.4b/tools/build_review_disc.sh
```

The disc builder cooks project data, builds mkisopsx, uses a disposable guest source
closure and the repository's canonical staged build/hazard patcher, then scans the
result. It requires the repository Rust toolchain, Python 3, rsync and MIPS binutils.
`cd-stream-bench` enables the runtime's persistent streamed-model loading;
`emulator-telemetry` provides review diagnostics. Neither changes the map spawn.

## Validation and limits

- 202 runtime, 444 project and 57 playtest tests pass. Includes independent volley
  latches, interrupted/stale attacks, skipped-frame events, bounded dash storage,
  buffer expiry, explicit idle binding, mesh capacities, ending and respawn reset.
  Full playtest tests used the cooked Cortex generated fixtures.
- Final normal disc: zero scanned R3000 load-delay hazards; structural check passes
  with WORLD.PAK, UI.PAK and two audio tracks. EXE SHA-256:
  `b3e20142b248fe0ec94b5a71b67da5effbbcc1efa6eeef759c01f1f505979d2c`.
- Final native replay: 1,850 pad polls, zero guest faults, 3,008 bytes heap headroom.
  Presses at polls 1,620/1,690 start attacks at frames 1,619/1,697; the second starts
  after release. The early press at 1,705 expires without a third attack.
- Burst observation: pairs at 2,105/2,141, 2,339/2,375 and 3,235/3,271, each 36 ticks
  apart. These earlier captures use the same final volley asset and emitter data.
- Heavy diagnostic spawn: intact native model/motion, projectile release and
  repeated player poise breaks. Cooked alert/turn reels inspected independently.
- Separate native respawn probe: charged enemy release and injected death with a
  live shot at frame 821; real respawn reports empty projectile pool, full vitality
  and cleared broken flags at 881. Charged enemy fires again at 1,035. Probe runs
  to 2,200 polls with zero guest faults. Its exact test-only patch is retained in
  `verified/respawn-probe.patch`, outside all compiled project source.

This is emulator and source/test evidence, not physical-console certification or
an exhaustive manual full-level playthrough. Heap headroom remains tight; further
resident assets need a fresh memory check. No claim of a guaranteed hardware frame
rate is made. Combat feel and camera tuning should receive a hands-on playthrough.

## Design references

The [official DS3 manual](https://www.fromsoftware.jp/manual/darksouls3/win/basic.html)
and [HUD guide](https://www.fromsoftware.jp/manual/darksouls3/ps4/screen.html)
support clear resources, checkpoints and combat commitment. [Bloodborne's combat
introduction](https://blog.playstation.com/2014/08/13/bloodborne-on-ps4-new-combat-details/)
and [official survival guidance](https://blog.playstation.com/2015/03/23/bloodborne-24-tips-for-survival/)
informed readable tells and recoveries. These are design references, not a claim
of reproducing proprietary algorithms. The demo already has dual vitality and
passive regeneration; an extra stamina/rally economy was not added without a
clear role in that system.
