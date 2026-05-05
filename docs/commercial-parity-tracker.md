# Commercial parity tracker

This is the game-by-game ledger for commercial PlayStation compatibility
work. It is intentionally parity-first: a title is not tracked as
"works" or "does not work"; it is tracked by the earliest observed point
where PSoXide stops matching the reference emulator, plus the evidence
needed to decide which emulation subsystem to improve next.

The reference oracle is the external PCSX-Redux build documented in
[`docs/redux-oracle.md`](redux-oracle.md). Test media must come from
legally owned discs or already-authorized preservation images.

## Parity workflow

Use `local_lockstep_sweep` as the default per-title workflow. It boots
the same disc in PSoXide and PCSX-Redux, records CPU-state checkpoints,
narrows the first coarse mismatch with an exact instruction window, and
optionally compares the final visible framebuffer byte-for-byte.

```bash
export PSOXIDE_BIOS="/Users/ebonura/Downloads/ps1 bios/SCPH1001.BIN"
export PSOXIDE_REDUX_BIN="/Users/ebonura/Desktop/repos/pcsx-redux/pcsx-redux"

cargo run --manifest-path emu/Cargo.toml \
  -p emulator-core \
  --example local_lockstep_sweep \
  --release -- \
  --root "/Users/ebonura/Downloads/ps1 games" \
  --steps 100000000 \
  --interval 10000 \
  --report-dir target/local-lockstep/top25-20260505
```

For a single game, pass one or more explicit discs:

```bash
cargo run --manifest-path emu/Cargo.toml \
  -p emulator-core \
  --example local_lockstep_sweep \
  --release -- \
  --disc "/absolute/path/to/Game.cue" \
  --steps 100000000 \
  --interval 10000 \
  --report-dir target/local-lockstep/game-name-20260505
```

Use `probe_local_games_boot` only as a loader and smoke triage tool. It
does not prove parity unless the result is later compared to Redux.

```bash
cargo run --manifest-path emu/Cargo.toml \
  --release \
  -p emulator-core \
  --example probe_local_games_boot -- \
  0 "/Users/ebonura/Downloads/ps1 games"
```

## Recording rules

Every row should eventually have:

- reference: Redux commit, BIOS image, disc path, image format;
- command: exact `local_lockstep_sweep` or route-specific command;
- checkpoint budget: steps, interval, visual comparison on/off;
- first matching checkpoint and first mismatching checkpoint;
- exact mismatch line from the generated per-game `SUMMARY.txt`;
- visual diff size, first pixel diff, and artifact directory if visual
  parity was enabled;
- subsystem hypothesis: CPU, DMA, CD-ROM, GPU, GTE, MDEC, SPU, SIO/pad,
  timing/scheduler, loader, or unknown;
- next probe that would either confirm or disprove that hypothesis.

Do not mark a game green from an interactive/manual run alone. Manual
screenshots and structural visual guards are useful evidence, but the
tracker state should remain "route works, parity not measured" until a
Redux-backed row exists.

## Status vocabulary

| Status | Meaning |
|---|---|
| Not local | The target is not available in the local legal test library. |
| Loader blocked | The image exists, but the loader cannot mount it yet. |
| Unswept | The image loads, but no Redux parity sweep has been recorded. |
| Swept to N | PSoXide matched Redux up to N user steps. |
| CPU break | CPU PC/register/COP2/tick parity diverged first. |
| Visual break | CPU checkpoints matched, but the visible display diverged. |
| Route works, parity not measured | A local functional route reaches a useful point, but no Redux comparison pins it. |
| Parity-pinned | A route has a frozen Redux-backed regression. |

## Shared findings

These are cross-cutting findings that may explain multiple game rows.
When a title first breaks at one of these points, keep it attached to
the shared issue instead of inventing a per-game bug.

| Finding | Evidence | Likely owner | Next action |
|---|---|---|---|
| BIOS Sony-logo display parity is byte-exact at 100M steps. | `docs/milestones.md` records Redux-visible hash `0xa3ac6881044333d0` for the no-disc path; 2026-05-05 sweeps below confirm zero visible-frame diff at 100M for all 11 local top-25 targets. | GPU, DMA, BIOS boot baseline | Keep as a cheap smoke guard before deeper per-title sweeps. |
| Historical instruction-record cache first diverges at step 19,474,544, but the commercial checkpoint sweep now stays aligned beyond that point. | `docs/milestones.md`; `probe_cycle_first_divergence`; parity cache under `target/parity-cache/`; 2026-05-05 checkpoint sweeps below. | DMA IRQ scheduling order, or an exact-record transient not visible in 10k checkpoint state. | Re-run exact tracing around 19,474,544 before treating this as an active commercial-game blocker. |
| Crash long-run display phase drifts from Redux even after the disc-check path is stable. | Milestone D notes: Crash 300M/900M reaches a different animation phase while static rendering is byte-exact. | Timing/scheduler, likely DMA IRQ cadence | Close or bound the timing drift before treating later Crash title/FMVs as renderer failures. |

## Latest sweep evidence

| Date | Scope | Command shape | Result | Evidence |
|---|---|---|---|---|
| 2026-05-05 | Full local library, 16 discovered sheets. | `local_lockstep_sweep --root ~/Downloads/ps1 games --steps 20000000 --interval 10000 --no-visual` | 15 mountable images matched Redux CPU checkpoints through 20M user steps. Tomb Raider was loader-blocked by missing ECM conversion. | `target/local-lockstep/local-20m-20260505/SUMMARY.txt` |
| 2026-05-05 | Top-25 local subset, 11 legally local targets. | `local_lockstep_sweep --disc ... --steps 50000000 --interval 10000 --no-visual` | All 11 matched Redux CPU checkpoints through 50M user steps. Framebuffer parity was intentionally skipped. | `target/local-lockstep/top25-local-50m-20260505/SUMMARY.txt` |
| 2026-05-05 | Top-25 local subset, 11 legally local targets. | `local_lockstep_sweep --disc ... --steps 100000000 --interval 10000` | All 11 matched Redux CPU checkpoints through 100M user steps and visible framebuffer parity at `640x478` with `diff=0/611840`. | `target/local-lockstep/crash-100m-visual-20260505/SUMMARY.txt`; `target/local-lockstep/top25-local-rest-100m-visual-20260505/SUMMARY.txt` |
| 2026-05-05 | Resident Evil 2 route toward "Original Game" / no-load path. | `local_lockstep_sweep --disc RE2.cue --steps 300000000 --interval 1000000 --no-visual --pad-pulses 0x0008@3150+30,0x4000@3250+20,0x0040@5120+12,0x0040@5160+12,0x4000@5200+30` | First route-level CPU checkpoint break is tick-only in `(266M, 267M]`: PC and GPR/COP2 state hash still match, but PSoXide is 402 cycles ahead of Redux. | `target/local-lockstep/re2-route-300m-20260505/SUMMARY.txt` |

Note: the 2026-05-05 sweep reports were generated before the harness
started printing visual `skip` explicitly. The command line is the
source of truth for those rows: framebuffer comparison was disabled.

## Top-25 parity ledger

| # | Game | Local image | Current parity state | First break or blocker | Next parity action |
|---|---|---|---|---|---|
| 1 | Crash Bandicoot | `Crash Bandicoot (USA).cue` | Swept to 100M CPU checkpoints with visible framebuffer parity, `640x478`, `diff=0/611840`. Milestone-D canary still has later visual-phase drift noted in `docs/milestones.md`. | No CPU or BIOS-logo visual break through 100M. Later Crash title/FMVs remain unpinned. | Route toward title/FMVs and record the first non-logo parity break. |
| 2 | Tekken 3 | `Tekken 3 (USA).cue` | Swept to 100M CPU checkpoints with visible framebuffer parity, `640x478`, `diff=0/611840`. Structural visual guards cover mode select, VS portraits, and fight screens, but they are not Redux rows. | No CPU or BIOS-logo visual break through 100M. | Promote one existing visual guard window into a Redux route now that the boot baseline is clean. |
| 3 | Marvel vs. Capcom: Clash of Super Heroes | `Marvel vs. Capcom - Clash of Super Heroes (USA).cue` | Swept to 100M CPU checkpoints with visible framebuffer parity, `640x478`, `diff=0/611840`. | No CPU or BIOS-logo visual break through 100M. | Add a menu/fight route and record its first parity break. |
| 4 | CTR: Crash Team Racing | `CTR - Crash Team Racing (USA).cue` | Swept to 100M CPU checkpoints with visible framebuffer parity, `640x478`, `diff=0/611840`. | No CPU or BIOS-logo visual break through 100M. | Add a race-start route and record its first parity break. |
| 5 | Gran Turismo 2 | `Gran Turismo 2 (USA) (Arcade Mode) (Rev 1).cue` | Swept to 100M CPU checkpoints with visible framebuffer parity, `640x478`, `diff=0/611840`. Milestone-K stretch target. | No CPU or BIOS-logo visual break through 100M. | Plan a menu/race route and record its first parity break. |
| 6 | Metal Gear Solid | `Metal Gear Solid (USA) (Disc 1) (Rev 1).cue` | Swept to 100M CPU checkpoints with visible framebuffer parity, `640x478`, `diff=0/611840`. Milestone-G target. | No CPU or BIOS-logo visual break through 100M. | Route to first complex MDEC sequence and compare against Redux there. |
| 7 | Metal Slug X | `Metal Slug X (USA).cue` | Swept to 100M CPU checkpoints with visible framebuffer parity, `640x478`, `diff=0/611840`. | No CPU or BIOS-logo visual break through 100M. | Add a gameplay route and record its first parity break. |
| 8 | Resident Evil 2: Dual Shock Ver. | `Resident Evil 2 - Dual Shock Ver. (USA) (Disc 1).cue` | Swept to 100M CPU checkpoints with visible framebuffer parity, `640x478`, `diff=0/611840`. Local cold-boot route reaches the first playable room at 2.2B user steps. Redux-backed route sweep is pinned to a 300M timing break before route input becomes active. | CPU/timing break in `(266M, 267M]`: ours `{tick:608542101 pc:0x8008605c state:64e648f7c3560511}`; Redux `{tick:608541699 pc:0x8008605c state:64e648f7c3560511}`. Visual skipped. | Refine the pre-input window with an exact no-pad trace, then inspect CD-ROM/DMA scheduler timing around RE2 executable startup and first MDEC stream setup. |
| 9 | Silent Hill | Not local | Not local. | No legal local image available for parity work. | Add only after legal local media exists. |
| 10 | Final Fantasy VII | Not local | Not local. | No legal local image available for parity work. | Add only after legal local media exists. |
| 11 | Yu-Gi-Oh! Forbidden Memories | Not local | Not local. | No legal local image available for parity work. | Add only after legal local media exists. |
| 12 | Spider-Man | `Spider-Man (USA).cue` | Swept to 100M CPU checkpoints with visible framebuffer parity, `640x478`, `diff=0/611840`. | No CPU or BIOS-logo visual break through 100M. | Add a gameplay route and record its first parity break. |
| 13 | Medal of Honor | Not local | Not local. | No legal local image available for parity work. | Add only after legal local media exists. |
| 14 | Castlevania: Symphony of the Night | Not local | Not local. | No legal local image available for parity work. | Add only after legal local media exists. |
| 15 | Resident Evil 3: Nemesis | `Resident Evil 3 - Nemesis (USA).cue` | Swept to 100M CPU checkpoints with visible framebuffer parity, `640x478`, `diff=0/611840`. | No CPU or BIOS-logo visual break through 100M. | Add a route through Capcom's movie/background path and record its first parity break. |
| 16 | Tony Hawk's Pro Skater 2 | Not local | Not local. | No legal local image available for parity work. | Add only after legal local media exists. |
| 17 | Street Fighter Collection / Alpha 2 Gold | `Street Fighter Collection - Street Fighter Alpha 2 Gold (USA) (Disc 2).cue` | Swept to 100M CPU checkpoints with visible framebuffer parity, `640x478`, `diff=0/611840`. | No CPU or BIOS-logo visual break through 100M. | Add a fight route and record its first parity break. |
| 18 | Need for Speed III: Hot Pursuit | Not local | Not local. | No legal local image available for parity work. | Add only after legal local media exists. |
| 19 | Disney's Tarzan | Not local | Not local. | No legal local image available for parity work. | Add only after legal local media exists. |
| 20 | Mortal Kombat 4 | Not local | Not local. | No legal local image available for parity work. | Add only after legal local media exists. |
| 21 | Jackie Chan Stuntmaster | Not local | Not local. | No legal local image available for parity work. | Add only after legal local media exists. |
| 22 | Harry Potter and the Sorcerer's Stone | Not local | Not local. | No legal local image available for parity work. | Add only after legal local media exists. |
| 23 | Digimon World | Not local | Not local. | No legal local image available for parity work. | Add only after legal local media exists. |
| 24 | Crash Bandicoot: Warped | Not local | Not local. | No legal local image available for parity work. | Add only after legal local media exists. |
| 25 | Mega Man X4 | Not local | Not local. | No legal local image available for parity work. | Add only after legal local media exists. |

## Extra local images

| Game | Local image | Current parity state | First break or blocker | Next parity action |
|---|---|---|---|---|
| Celeste Classic PSX | `Celeste Classic PSX (Homebrew).cue` | Swept to 20M CPU checkpoints; framebuffer parity skipped. Local homebrew image, not part of the commercial top-25 set. | No CPU checkpoint break through 20M. | Use as an optional lightweight route after commercial boot parity stabilizes. |
| Tomb Raider | `Tomb Raider (USA) (Greatest Hits).ccd` with `.img.ecm` | Loader blocked until ECM conversion is available. | Needs `unecm`, `ecm-uncompress`, or `PSOXIDE_UNECM` external converter. | Install or configure ECM converter, verify CCD mount, then sweep baseline parity. |
| WipEout | `WipEout (Europe) (v1.1).cue` | Swept to 20M CPU checkpoints with the default SCPH1001 baseline; framebuffer parity skipped. | No CPU checkpoint break through 20M in that baseline. PAL BIOS parity remains pending. | Run baseline sweep with the correct PAL BIOS when doing region-specific parity. |
| WipEout 2097 | `WipEout 2097 (Europe).cue` | Swept to 20M CPU checkpoints with the default SCPH1001 baseline; framebuffer parity skipped. Milestone-J target. | No CPU checkpoint break through 20M in that baseline. PAL BIOS parity remains pending. | Use `SCPH5502.BIN` for PAL timing sweep and record the first exact mismatch. |
| WipEout 3: Special Edition | `WipEout 3 - Special Edition (Europe) (En,Fr,De,Es,It).cue` | Swept to 20M CPU checkpoints with the default SCPH1001 baseline; framebuffer parity skipped. | No CPU checkpoint break through 20M in that baseline. PAL BIOS parity remains pending. | Sweep after WipEout 2097 gives the primary PAL finding. |

## Finding template

Append dated findings here when a sweep produces new evidence.

### 2026-05-05 - Resident Evil 2 route timing drift

Disc:
`/Users/ebonura/Downloads/ps1 games/Resident Evil 2 - Dual Shock Ver. (USA) (Disc 1)/Resident Evil 2 - Dual Shock Ver. (USA) (Disc 1).cue`

BIOS:
`/Users/ebonura/Downloads/ps1 bios/SCPH1001.BIN`

Redux:
`/Users/ebonura/Desktop/repos/pcsx-redux/pcsx-redux`

Command:
`local_lockstep_sweep --disc RE2.cue --steps 300000000 --interval 1000000 --no-visual --pad-pulses 0x0008@3150+30,0x4000@3250+20,0x0040@5120+12,0x0040@5160+12,0x4000@5200+30`

Steps / interval / visual:
300,000,000 / 1,000,000 / skipped.

Last matching checkpoint:
266,000,000 user steps.

First coarse mismatch:
`(266000000, 267000000]`; ours `{tick:608542101 pc:0x8008605c state:64e648f7c3560511}`; Redux `{tick:608541699 pc:0x8008605c state:64e648f7c3560511}`.

Exact mismatch:
Skipped by the current pad-routed exact-trace guard. The mismatch occurs before the first scheduled route input, so the next pass should refine this as a no-pad exact window or teach the harness to exact-trace pad routes while the pad schedule is inactive.

Visual diff:
Skipped. A separate local `probe_fmv_path` run with the same route reaches the first playable room at 2.2B user steps.

Artifacts:
`target/local-lockstep/re2-route-300m-20260505/`

Subsystem hypothesis:
Timing/scheduler drift, likely CD-ROM/DMA cadence during RE2 executable startup or first MDEC stream setup. This is not yet a renderer failure: the first pinned mismatch has identical PC and GPR/COP2 state hash.

Next probe:
Refine `(266M, 267M]` with exact instruction tracing, then correlate the first tick delta with CD-ROM IRQ counts, DMA start/finish cadence, and MDEC command submission.

```text
YYYY-MM-DD - Game title
Disc:
BIOS:
Redux:
Command:
Steps / interval / visual:
Last matching checkpoint:
First coarse mismatch:
Exact mismatch:
Visual diff:
Artifacts:
Subsystem hypothesis:
Next probe:
```
