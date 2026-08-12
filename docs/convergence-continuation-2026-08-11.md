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
| PSoXide integration | `PSoXide-convergence` | `codex/quake-psoxide-convergence` | `41875824` | one dirty file: the owner's camera edit in `editor/projects/brush-first-playable/project.ron`. PRESERVE. |
| Quake integration | `quake-psx-convergence` | `codex/quake-convergence` | `7a4b0a4` | clean; FULL 14-gate matrix green at this head (see below); a worker is extending it on `codex/quake-q6-routes` |
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

PSoXide at `0c0b35d9`, all green (re-verify at the current head):

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

Quake at `7a4b0a4`, the FULL matrix green and verified at that exact head:
check, map-regress, start-route-regress, e1m1-chain-regress,
systems-regress, combat-regress, monster-regress, bestiary-regress,
arsenal-regress, audio-regress, ambient-regress, survival-regress,
episode1-regress, disc. Tests: root 42, quake-core 181 + 12 + 2 + 3. Real
MIPS build green. Earlier matrices at `bb8aa9a` and `6abc54c` are
historical and prove nothing about this head.

Landmarks reached at this head: **Chthon is killed with ordinary pad
input** (`episode_state=0x3fff`, two byte-identical runs) by taking the
rune, watching him rise, walking the arena, riding the map's own lift 176
units to the button ring, and driving `event_lightning`, which is the only
kill there is since he is immune to every weapon. Mover rider carrying is
implemented over the abstract collision provider and applies to plats,
trains AND doors, with the pusher removed from the composed hull exactly
as the original sets `pusher->v.solid = SOLID_NOT`. A blocked pusher rolls
back together with everything it moved. Two real bugs fell out of that
work: `func_plat` travel was `size_z + 8` where Quake is `size_z - 8`, and
lifts were started by a 60-unit proximity field instead of Quake's
`plat_spawn_inside_trigger` volume, which on E1M7 sent the lift away
before the player could board and then crushed them under it.

## 3. Open work, in dependency order

### Quake: gates repaired, then extended (all HISTORICAL until re-run)

The three gates that broke when the bestiary and entity-systems branches
merged were repaired at cause, not by re-tuning routes: the merged guest
image had outgrown the PS1 heap (the resident-map arena was still PSoXide's
generic 1.1 MB while the largest cooked map needs 988,918 bytes, so the
guest died in `handle_alloc_error` before its first gameplay frame, and the
arena is now Quake policy sized with a build-time check), and a resting
contact was being read as a trap in `fly_move`. The audio gate was
re-anchored to the event instead of an absolute wall-clock window.

The freeze fix then went further: `restore_trace_invariants` re-imposes
`SV_RecursiveHullCheck`'s own rule at the PRODUCER, and `trapped()` is now
bare `all_solid`, exactly what `SV_FlyMove` tests. The two-flag workaround
is gone.

### Quake: func_train, and what was refuted

The guest computes about 300x too many ticks for an authored leg. My
earlier cooker hypothesis (a doubly-Q12-scaled `mins`) was REFUTED with
direct evidence: running the shipping `QuakeTrain` against the real cooked
lumps for all nine maps gives correct legs everywhere (12 trains, 1,586 leg
samples, longest 87 ticks). The arithmetic and the cooked data are both
fine; the guest's view of the inputs is not. A regression test pins
authored leg timing. The MIPS aggregate-return ABI story is a HYPOTHESIS
and must be proved or eliminated (see section 1). **Trains are not
working** until guest-side movement is proved on the real MIPS guest.

### Quake: Q4 partially delivered

Delivered: the intermission (authored `info_intermission` camera, level
title, kill and secret counters, episode-complete headline), the episode
completion state, and an `episode1-regress` action with two-run agreement.
Two real defects were found while verifying rather than assuming:
`item_sigil` never ran its target dispatch, which made the WHOLE Chthon
encounter unreachable through the authored chain, and `monster_death_use`
was absent, so his death could never reach the relay driving his exit
doors.

NOT delivered: per-map authored routes for Start and E1M1 through E1M8,
the normal and secret full-episode routes, presentation beyond the
intermission (no particles, sprites, explosion or muzzle feedback, screen
flashes, and light styles are still frozen at constant 256), and a real
performance profile. Whole-run averages that include disc stalls are NOT a
frame-rate claim and were correctly refused as such.

Chthon could not be killed because `func_plat` did not carry riders: his
`event_lightning` button sits on a ring at z 208, the arena floor is
z 0..63, and the two walkable sets are disjoint with the plat as the only
access. Rider carrying is being implemented; its blocked-push rollback
semantics and its rider scope both need the corrections in section 1.

Also open: `bestiary-regress` gates 2 of 7 monsters (Ogre, Knight). Zombie,
Wizard, Shambler, Demon and Chthon are implemented and unit-tested with no
authored-map death gate, and Zombie and Shambler cannot be gated by a
walk-up route with the starting arsenal.

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
