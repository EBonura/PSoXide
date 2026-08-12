# Convergence continuation packet

Last refreshed: 2026-08-12. Read with `docs/finish-line-plan-2026-08-11.md`,
its binding amendment `docs/finish-line-scope-correction-2026-08-11.md`, the
merge procedure in `docs/convergence-merge-train-2026-08-11.md`, the evidence
history in `docs/quake-psoxide-convergence-handoff.md` sections 0.19 to 0.23,
and `docs/convergence-discrepancy-report-2026-08-11.md`.

**Challenge this document rather than inheriting it.** Every number came from
a run recorded here, but re-derive whatever you depend on. Two of the most
important findings this campaign produced were audits refuting a previous
worker's confident diagnosis.

## 1. Exact heads

| Lane | Worktree | Branch | Head | State |
|---|---|---|---|---|
| PSoXide integration | `PSoXide-convergence` | `codex/quake-psoxide-convergence` | `0c0b35d9` | one dirty file: the owner's camera edit in `editor/projects/brush-first-playable/project.ron`. PRESERVE. Fully gated green. |
| Quake integration | `quake-psx-convergence` | `codex/quake-convergence` | `9e6e5ba` | a repair worker is active in this tree; 3 gates red (below) |
| Demo disc | `psx-demo-disc-quake-shareware` | `codex/quake-shareware-demo-disc` | `dc9e41a` | clean; Quake now DEFAULT; still on old pins |
| Quake source pin | `PSoXide-rc1-pin` | detached | `f9f83c35` | clean; every Quake command needs `--psoxide` pointing here |

PSoXide is an **automated Comicon candidate**, not "done": P2, P3 and the
composed souls demonstration are green under automated evidence, while the
owner's native-GUI acceptance and all hardware acceptance remain open. It
already contains `main` (merge `dac903f3`), so merge-train step 2 is DONE,
and it is the candidate for the final pin once Quake stabilises.

P6 microsection streaming stays DEFERRED, and the disclosure stays exact:
**Comicon BSP demo uses measured whole-map residency; P6 microsection
streaming remains deferred.** The residency claim rests on the measured
budgets in handoff section 0.22 (static footprint 1,640,536 of 1,998,848,
17.9 percent free), not on the PXBSP file size, and two figures in it are
explicitly unmeasured: VRAM page occupancy/fragmentation, and runtime peak
stack usage.

D1 is **NOT complete**. The default-inclusion policy is correct and merged,
but the disc still pins OLD Quake and PSoXide revisions. It finalises only
after repinning both live final revisions, rebuilding every disc mode,
rerunning the two-pass deterministic headless checks, remeasuring capacity,
and regenerating hashes and provenance.

The suspected MIPS aggregate-return ABI problem is a **HYPOTHESIS, not a
proven root cause**. It must be proved with a guest-side boundary
diagnostic or a disassembly, or eliminated by converting the risky
by-value aggregate returns to caller-owned output parameters, and then
authored `func_train` movement must be proved on the real MIPS guest: a
host-side leg length is not evidence that the guest moves the train.

Any gate result quoted in this document is evidence **for the head it was
run at**. The twelve green gates recorded at `bb8aa9a` are historical and
are not proof for any later head; the complete matrix must be re-run at the
final head after all merges.

## 2. What is green, and at what evidence level

Levels: L1 host tests, L2 real MIPS build, L3 two deterministic image-free
replays from an exact-source frontend, L4 original hardware. **No L4 evidence
exists anywhere in this campaign.**

PSoXide at `0c0b35d9`, all green:

```text
L1  psxed-ui 397, psxed-project 499, psx-bsp 71,
    psx-game-runtime 92, psx-engine 318
L3  make combat-checkpoint          melee 4, stagger 1, death 1, taken 3,
                                    vram 0xdef0e1275bff08b2
L3  make editor-blank-playtest-check
L3  make editor-bsp-liquid-check    12 lava events, respawn at frame 181
L3  make editor-souls-bsp-check     the composed demonstration gate:
      hits 4, stagger 1, kill 1, taken 4, checkpoint 1, door 1, lava 6,
      death 1, attachments 2, pvs 2910, vram 0xbab02327df64003d,
      IDENTICAL across two guest layouts (988 and 989 visual frames)
    make runtime-numeric-guard      ok, 219 files
```

Quake at `9e6e5ba`: check, map-regress, e1m1-chain-regress, systems-regress,
combat-regress, monster-regress, arsenal-regress, ambient-regress PASS;
quake-core 114 tests, host 35, real MIPS guest builds.

## 3. Open work, in dependency order

### Quake: three red gates (a repair worker is on them)

All three come from the SAME class of cause and that is itself the finding:
two branches green in isolation changed navigation and timing when combined.

1. `start-route-regress` times out at (508, 1638, 46) with 3 of 7 mechanisms.
   Start gained genuinely solid episode/boss gates at the same moment
   monsters started blocking bodies. Note the earlier fix in this area
   (skip a brush entity whose hull cannot be evaluated rather than vetoing
   the world trace) SURVIVED the merge and is verified present.
2. `bestiary-regress` loads E1M2 but never reaches E1M4.
3. `audio-regress` tail window. Do NOT widen it again: it asserts silence in
   an ABSOLUTE wall-clock window while guest pacing is content-dependent, so
   every gameplay addition shifts it. Re-anchor it to an event.

### Quake: func_train legs about 300x too long on the guest

Host tests give 87 ticks for an E1M5 leg; the guest computes 27,804, exactly
`isqrt_i32(i32::MAX) * 60 / 100`, so the squared length saturated.
`travel_ticks` shifts right by 12 first, so saturation needs a component near
46,340 units (about 1.9e8 in Q12), roughly 4096x too large: the signature of
a value Q12-scaled twice. `MapEntity::origin` is verified already Q20.12 and
`BrushModel::mins` is declared raw i16, so the open suspect is what the
COOKER writes into a train submodel's mins. **Trains are not working.**

### Quake: Q2c lane (worker active, branch `codex/quake-q2c-survival`)

Player freeze investigation (the player stops dead at fixed coordinates,
bit-identical across runs with different input, which points at the motor
never running rather than a wall), the `survival-regress` route, the audio
re-anchoring, and `SetChangeParms` level-change carry-over.

### Quake: Q4 NOT STARTED

Per-map routes for Start and E1M1-E1M8, the normal and secret routes (the
secret exit is **E1M4 to E1M8**, and E1M8 returns to E1M5; E1M7 returns to
Start), Chthon end to end, intermission and the episode ending, plus
sprites, particles, explosions, flashes, light styles, and the 30 fps
performance telemetry. This is the largest remaining package.

Also open from the monster lane: `bestiary-regress` gates 2 of 7 monsters
(Ogre, Knight). Zombie, Wizard, Shambler, Demon and Chthon are implemented
and unit-tested but have no authored-map death gate. Zombie and Shambler
cannot be gated by a walk-up route with the starting arsenal.

### PSoXide: final pin

Nothing is blocking. When Quake stabilises, freeze the pin, update Quake's
`PSOXIDE_REV` and dependency, re-hydrate, and rerun.

### I1, D1, R1

I1 can now rely on canonical guest staging (a two-checkout byte-identity
proof exists for PSoXide, and Quake already had one). D1's plumbing is DONE
and merged: Quake shareware is a DEFAULT program, `make quake-repin` prints
the six pin values from a built tree, and the headless gate proves two
deterministic chain-loads plus the menu entry. It still runs against OLD
pins, so it must be rerun after the repin, and the two FNV pins inside
`check_quake_headless.py` recomputed then. Measured capacity: default disc
96,393 sectors, HL variant 300,881 of 359,999, so Half-Life plus Quake fits
with 59,118 sectors spare and nothing had to be dropped.

### H1 stays with the owner

`docs/h1-owner-hardware-handoff.md` has the burn command, the console
checklist, and the expected evidence. No agent may claim hardware success.

## 4. Findings a successor must not lose

**Replay tapes must be on the guest's pad-poll clock.** A `video_frame` tape
is applied on the emulator's route-tick clock while the guest samples the pad
once per simulation tick; the phase drifts with guest execution cost, so the
same tape delivered 70 ticks of movement on one build and 71 on another and
changed the gameplay outcome. Both PSoXide gates now reject non-poll tapes.
The editor records on the video-frame clock, so any re-recorded tape must be
converted (`--input-tape-transcribe`).

**Melee was already tick-authoritative.** The suspicion that it was
render-rate dependent was refuted by reading and by measurement. Do not
"fix" it again.

**The MIPS guest is now checkout-path-reproducible** via
`tools/build_guest_staged.sh`; `PSOXIDE_GUEST_STAGE=0` restores the old
behaviour for comparison.

**Replay gates must rebuild the frontend** from the source under test. A
stale frontend reads newer telemetry ids as `unknown`.

**Git stashes are repository-wide, not worktree-local.** A push in one
worktree and a pop in another moves the work between trees. Use a patch file
or a temporary commit instead.

**Shareware Episode 1 authors ZERO** Enforcer, Fish, Hell Knight, Shalrath
and Tarbaby. They are registered content and are correctly excluded.

## 5. Falsification targets for the next session

- The three red Quake gates: confirm the repair fixed the CAUSE and did not
  re-tune a route around a real bug.
- `PLAYER_WEAPON_ATTACHMENTS` is inferred from equipment draw counts rather
  than from socket resolution. Try to make it lie: unequip mid-life, kill the
  player during a swing, respawn with a broken socket reference.
- The residency claim: it rests on a link map and a cook budget line, and two
  numbers (VRAM fragmentation, peak stack) are explicitly unmeasured. If the
  demonstration grows, re-measure rather than assuming the headroom holds.
- The demo-disc headless gate dropped several absolute pins (cycles, PC,
  route ticks, pad polls, CD commands, log digests) because they moved on
  every commit to that repo. Confirm what remains still fails when Quake is
  genuinely broken.
