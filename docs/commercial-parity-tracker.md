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
optionally compares the final visible framebuffer byte-for-byte. The
harness compares Redux checkpoints as they arrive and terminates the
Redux run at the first mismatch, so long route sweeps should now produce
bounded first-break evidence instead of waiting for the full route.

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
  --redux-timeout-secs 60 \
  --redux-wall-timeout-secs 900 \
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

Use `commercial_route_matrix` as the route-progress ratchet. It BIOS
boots local images, applies the per-game route script or generic triage
script, classifies the blocker bucket, writes `SUMMARY.md` plus
`matrix.csv`, and prints the exact `local_lockstep_sweep` command needed
to pin the route against Redux.

```bash
cargo run --manifest-path emu/Cargo.toml \
  -p emulator-core \
  --example commercial_route_matrix \
  --release -- \
  --root "/Users/ebonura/Downloads/ps1 games" \
  --steps 300000000 \
  --report-dir target/commercial-route-matrix/local-300m
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

## Playable ratchet

A title is not playable until an automated BIOS-boot route:

- reaches an actual controllable gameplay state;
- accepts input in that state;
- renders a coherent framebuffer;
- survives a fixed soak window without panic or hang;
- has the first route parity break recorded against Redux.

`commercial_route_matrix` may report `playable-candidate` when structural
guards look promising, but that still is not playable. Promote it only
after a game-specific route guard and Redux-backed parity row exist.

## Status vocabulary

| Status | Meaning |
|---|---|
| Not local | The target is not available in the local legal test library. |
| Loader blocked | The image exists, but the loader cannot mount it yet. |
| Unswept | The image loads, but no Redux parity sweep has been recorded. |
| Swept to N | PSoXide matched Redux up to N user steps. |
| CPU break | CPU PC/register/COP2/tick parity diverged first. |
| Visual break | CPU checkpoints matched, but the visible display diverged. |
| Commercial route blocked | A real game route hits a visible blocker before a useful playable/menu route, regardless of BIOS or direct-EXE entry path. |
| Route works, parity not measured | A local functional route reaches a useful point, but no Redux comparison pins it. |
| Parity-pinned | A route has a frozen Redux-backed regression. |

## Shared findings

These are cross-cutting findings that may explain multiple game rows.
When a title first breaks at one of these points, keep it attached to
the shared issue instead of inventing a per-game bug.

| Finding | Evidence | Likely owner | Next action |
|---|---|---|---|
| BIOS Sony-logo display parity is byte-exact at 100M steps. | `docs/milestones.md` records Redux-visible hash `0xa3ac6881044333d0` for the no-disc path; 2026-05-05 sweeps below confirm zero visible-frame diff at 100M for all 11 local top-25 targets. | GPU, DMA, BIOS boot baseline | Keep as a cheap smoke guard before deeper per-title sweeps. |
| The 100M top-25 sweeps validate the cold BIOS boot baseline only; they do not prove menu or gameplay support. | Route-progress probes on 2026-05-05: CTR remains on the SCEA splash through 300M in both BIOS and direct-EXE modes; Metal Slug X displays `NO METAL SLUG X DATA DETECTED. DATA LOAD CANCELED.` without route input, but the route matrix can move it to a loading screen at 300M; Marvel vs. Capcom reaches the Capcom movie/logo path but is not gameplay-validated. | CD-ROM, DMA/timing, GPU, scheduler, loader state | Use `commercial_route_matrix` first, then promote each passing route into a Redux-backed `local_lockstep_sweep`. |
| Historical instruction-record cache first diverges at step 19,474,544, but the commercial checkpoint sweep now stays aligned beyond that point. | `docs/milestones.md`; `probe_cycle_first_divergence`; parity cache under `target/parity-cache/`; 2026-05-05 checkpoint sweeps below. | DMA IRQ scheduling order, or an exact-record transient not visible in 10k checkpoint state. | Re-run exact tracing around 19,474,544 before treating this as an active commercial-game blocker. |
| Crash long-run display phase drifts from Redux even after the disc-check path is stable. | Milestone D notes: Crash 300M/900M reaches a different animation phase while static rendering is byte-exact. | Timing/scheduler, likely DMA IRQ cadence | Close or bound the timing drift before treating later Crash title/FMVs as renderer failures. |

## Latest sweep evidence

| Date | Scope | Command shape | Result | Evidence |
|---|---|---|---|---|
| 2026-05-05 | Full local library, 16 discovered sheets. | `local_lockstep_sweep --root ~/Downloads/ps1 games --steps 20000000 --interval 10000 --no-visual` | 15 mountable images matched Redux CPU checkpoints through 20M user steps. Tomb Raider was loader-blocked by missing ECM conversion. | `target/local-lockstep/local-20m-20260505/SUMMARY.txt` |
| 2026-05-05 | Top-25 local subset, 11 legally local targets. | `local_lockstep_sweep --disc ... --steps 50000000 --interval 10000 --no-visual` | All 11 matched Redux CPU checkpoints through 50M user steps. Framebuffer parity was intentionally skipped. | `target/local-lockstep/top25-local-50m-20260505/SUMMARY.txt` |
| 2026-05-05 | Top-25 local subset, 11 legally local targets. | `local_lockstep_sweep --disc ... --steps 100000000 --interval 10000` | All 11 matched Redux CPU checkpoints through 100M user steps and BIOS/Sony-logo visible framebuffer parity at `640x478` with `diff=0/611840`. This is not gameplay validation. | `target/local-lockstep/crash-100m-visual-20260505/SUMMARY.txt`; `target/local-lockstep/top25-local-rest-100m-visual-20260505/SUMMARY.txt` |
| 2026-05-05 | Resident Evil 2 route toward "Original Game" / no-load path. | `local_lockstep_sweep --disc RE2.cue --steps 300000000 --interval 1000000 --no-visual --pad-pulses 0x0008@3150+30,0x4000@3250+20,0x0040@5120+12,0x0040@5160+12,0x4000@5200+30` | First route-level CPU checkpoint break is tick-only in `(266M, 267M]`: PC and GPR/COP2 state hash still match, but PSoXide is 402 cycles ahead of Redux. | `target/local-lockstep/re2-route-300m-20260505/SUMMARY.txt` |
| 2026-05-05 | Route-progress spot checks for CTR, Marvel vs. Capcom, and Metal Slug X. | `probe_fmv_path ...` and `probe_fmv_path --fastboot ...`; reproduced by ignored tests in `emu/crates/emulator-core/tests/commercial_disc_progress.rs`. | CTR is blocked on the SCEA splash through 300M in BIOS and direct-EXE modes (`0xbfb9bb04fb7042d8`); Metal Slug X reports no game data by 300M in BIOS mode and by 100M in direct-EXE mode (`0x09369767b12fc5f2`); Marvel vs. Capcom reaches the Capcom movie/logo path but no gameplay route is pinned. | Local repro logs/screenshots; use `commercial_disc_progress` as the red guard. |
| 2026-05-05 | Route matrix canaries for CTR and Metal Slug X. | `commercial_route_matrix --disc CTR.cue --disc MetalSlugX.cue --steps 300000000 --report-dir target/commercial-route-matrix/canaries-20260505` | CTR remains `boot/license` at the SCEA splash (`0xbfb9bb04fb7042d8`). Metal Slug X becomes `route-progress` with generic input, reaches a loading screen (`0x36cb4b8cb6c42d59`), and still needs gameplay confirmation plus Redux parity. | `target/commercial-route-matrix/canaries-20260505/SUMMARY.md`; `target/commercial-route-matrix/canaries-20260505/matrix.csv` |
| 2026-05-05 | Full local route matrix, 16 discovered sheets. | `commercial_route_matrix --root ~/Downloads/ps1 games --steps 300000000 --wall-timeout-secs 120 --report-dir target/commercial-route-matrix/local-300m-20260505` | No title is playable yet. Buckets: `render/gpu=4`, `fmv/mdec=4`, `unknown=3`, `boot/license=2`, `route-progress=1`, `menu-input=1`, `loader=1`. Every row includes the next `local_lockstep_sweep` parity command. | `target/commercial-route-matrix/local-300m-20260505/SUMMARY.md`; `target/commercial-route-matrix/local-300m-20260505/matrix.csv` |
| 2026-05-05 | CTR routed parity harness smoke. | `local_lockstep_sweep --disc CTR.cue --steps 10000000 --interval 1000000 --no-visual --pad-pulses ...` followed by streaming-harness regression smokes with `--redux-timeout-secs 60 --redux-wall-timeout-secs 120`. | CTR route CPU state matches Redux through 10M routed user steps. This does not reach gameplay; it validates the route/parity plumbing before longer SCEA-splash first-break sweeps. | `target/local-lockstep/ctr-smoke-10m-20260505/SUMMARY.txt`; `target/local-lockstep/ctr-stream-wall-smoke-1m-20260505/SUMMARY.txt` |
| 2026-05-06 | Resident Evil 2 exact route drift probe. | `probe_raw_irq_trace 266946809 RE2.cue` plus dense `local_lockstep_sweep --steps 267500000 --interval 50000 --no-visual --pad-pulses ...` | The old `(266M, 267M]` tick-only drift is fixed. The next first break is now exact: checkpoint window `(267150000, 267200000]`, exact step `267175364`, ours `tick=608965378`, Redux `tick=608965346`, same `pc=0x8008602c` and instruction. Local folded-step evidence shows a VBlank IRQ entry with `I_STAT=0x001`, `raw_isr=10362`, delta `22849`, and `2123` memory-access cycles. | `target/re2-diagnostics/re2-local-fold-after-no-dsr-timeout.trace`; `target/re2-diagnostics/re2-redux-fold.trace`; `target/re2-diagnostics/re2-local-vblank-267175364.trace`; `target/local-lockstep/re2-route-267m-single-after-sio-20260506/`; `target/local-lockstep/re2-route-267_5m-50k-after-sio-20260506/` |
| 2026-05-06 | CTR and Metal Slug X canaries after SIO IRQ fix. | `commercial_route_matrix --disc CTR.cue --disc MetalSlugX.cue --steps 300000000 --report-dir target/commercial-route-matrix/canaries-after-sio-20260506` | No route promotion. CTR remains `boot/license` at the SCEA splash (`0xbfb9bb04fb7042d8`). Metal Slug X remains `route-progress` at the loading-screen state (`0x36cb4b8cb6c42d59`) and still needs gameplay confirmation plus Redux parity. | `target/commercial-route-matrix/canaries-after-sio-20260506/SUMMARY.md`; `target/commercial-route-matrix/canaries-after-sio-20260506/matrix.csv` |
| 2026-05-06 | Metal Slug X target switch. | `commercial_route_matrix --disc MetalSlugX.cue --steps 530000000 --dump-visible`, `probe_fmv_path` with extended START/CROSS/DOWN pulses, and `local_lockstep_sweep` routed parity runs. | Stock route advances beyond the 300M loading-screen state to a 530M black visible frame: `render/gpu`, `display_hash=0x1100fdb97cd50325`, `pc=0x8006e5dc`, `vblank=2147`, CD data IRQs `955`, pad polls `1139`, MDEC MB `0`. Extended input changes the route after 400M and drives heavy GPU DMA, but after 500M host samples are dominated by `Gpu::paint_rect` semi-transparent monochrome rectangles. The Redux pad oracle now matches the same routed input through 100M checkpoints after moving pad-mask recomputation to VBlank edges. | `target/commercial-route-matrix/metal-slug-x-530m-after-rect-span-20260506/`; `target/metal-slug-x-500m-long-pulses.ppm`; `target/metal-slug-x-530m-after-rect-span.sample.txt`; `target/local-lockstep/metal-slug-x-route-100m-after-pad-oracle-20260506/` |

Note: the 2026-05-05 sweep reports were generated before the harness
started printing visual `skip` explicitly. The command line is the
source of truth for those rows: framebuffer comparison was disabled.

## Top-25 parity ledger

| # | Game | Local image | Current parity state | First break or blocker | Next parity action |
|---|---|---|---|---|---|
| 1 | Crash Bandicoot | `Crash Bandicoot (USA).cue` | Swept to 100M CPU checkpoints with BIOS/Sony-logo visible framebuffer parity, `640x478`, `diff=0/611840`. Milestone-D canary still has later visual-phase drift noted in `docs/milestones.md`. | No CPU or BIOS-logo visual break through 100M. Later Crash title/FMVs remain unpinned. | Route toward title/FMVs and record the first non-logo parity break. |
| 2 | Tekken 3 | `Tekken 3 (USA).cue` | Swept to 100M CPU checkpoints with BIOS/Sony-logo visible framebuffer parity, `640x478`, `diff=0/611840`. Structural visual guards cover mode select, VS portraits, and fight screens, but they are not Redux rows. | No CPU or BIOS-logo visual break through 100M. | Promote one existing visual guard window into a Redux route now that the boot baseline is clean. |
| 3 | Marvel vs. Capcom: Clash of Super Heroes | `Marvel vs. Capcom - Clash of Super Heroes (USA).cue` | Swept to 100M CPU checkpoints with BIOS/Sony-logo visible framebuffer parity, `640x478`, `diff=0/611840`. The 300M route matrix classifies the generic fighter route as `fmv/mdec`; the dumped frame is movie/effect content, not gameplay. | No CPU or BIOS-logo visual break through 100M. Gameplay support is unpinned. | Add a real menu/fight route with explicit gameplay guards, then record its first parity break. |
| 4 | CTR: Crash Team Racing | `CTR - Crash Team Racing (USA).cue` | Commercial route blocked: stuck on the SCEA splash through 300M steps in BIOS and direct-EXE modes (`display_hash=0xbfb9bb04fb7042d8`). Cold BIOS sweep still matches Redux through 100M BIOS/Sony-logo parity. | The route does not reach title/menu/race; no gameplay route is validated. | Inspect CD-ROM/DMA/GPU state around the post-license SCEA splash, then promote a race-start route into Redux parity. |
| 5 | Gran Turismo 2 | `Gran Turismo 2 (USA) (Arcade Mode) (Rev 1).cue` | Swept to 100M CPU checkpoints with BIOS/Sony-logo visible framebuffer parity, `640x478`, `diff=0/611840`. Milestone-K stretch target. | No CPU or BIOS-logo visual break through 100M. | Plan a menu/race route and record its first parity break. |
| 6 | Metal Gear Solid | `Metal Gear Solid (USA) (Disc 1) (Rev 1).cue` | Swept to 100M CPU checkpoints with BIOS/Sony-logo visible framebuffer parity, `640x478`, `diff=0/611840`. Milestone-G target. | No CPU or BIOS-logo visual break through 100M. | Route to first complex MDEC sequence and compare against Redux there. |
| 7 | Metal Slug X | `Metal Slug X (USA).cue` | Route-progress/render blocked: without route input it reaches `NO METAL SLUG X DATA DETECTED. DATA LOAD CANCELED.`; with generic route input the matrix reaches a loading screen at 300M (`display_hash=0x36cb4b8cb6c42d59`) and then a black 320x240 visible frame at 530M (`display_hash=0x1100fdb97cd50325`). Routed BIOS parity now matches Redux through 100M checkpoints. | Data detection can be passed, but gameplay is not confirmed. The current local blocker is black-frame/render progress after 530M plus renderer throughput under heavy semi-transparent rectangle traffic. The next parity target is the 300M/530M route window, not oracle throughput. | Pin the 300M/530M route against Redux, add a Metal Slug X-specific gameplay guard, and inspect why the visible display start alternates between VRAM pages while the 530M visible page is black. |
| 8 | Resident Evil 2: Dual Shock Ver. | `Resident Evil 2 - Dual Shock Ver. (USA) (Disc 1).cue` | Swept to 100M CPU checkpoints with BIOS/Sony-logo visible framebuffer parity, `640x478`, `diff=0/611840`. Local cold-boot route reaches the first playable room at 2.2B user steps. The previously pinned `(266M, 267M]` timing drift is fixed in both exact folded-step tracing and a normal 267M routed Redux checkpoint. | Old break was SIO/controller IRQ timing during a BIOS SPU-DMA ISR. New post-fix first break is exact at step `267175364`: ours `tick=608965378`, Redux `tick=608965346`, same `pc=0x8008602c` and instruction. Local raw folded step enters VBlank IRQ with `I_STAT=0x001`, runs `10362` raw ISR instructions, and ends 32 cycles ahead of Redux. | Capture the equivalent Redux raw VBlank PC/memory trace, explain the +32-cycle delta, then extend toward the 2.2B first-room route and add a RE2-specific gameplay guard. |
| 9 | Silent Hill | Not local | Not local. | No legal local image available for parity work. | Add only after legal local media exists. |
| 10 | Final Fantasy VII | Not local | Not local. | No legal local image available for parity work. | Add only after legal local media exists. |
| 11 | Yu-Gi-Oh! Forbidden Memories | Not local | Not local. | No legal local image available for parity work. | Add only after legal local media exists. |
| 12 | Spider-Man | `Spider-Man (USA).cue` | Swept to 100M CPU checkpoints with BIOS/Sony-logo visible framebuffer parity, `640x478`, `diff=0/611840`. | No CPU or BIOS-logo visual break through 100M. | Add a gameplay route and record its first parity break. |
| 13 | Medal of Honor | Not local | Not local. | No legal local image available for parity work. | Add only after legal local media exists. |
| 14 | Castlevania: Symphony of the Night | Not local | Not local. | No legal local image available for parity work. | Add only after legal local media exists. |
| 15 | Resident Evil 3: Nemesis | `Resident Evil 3 - Nemesis (USA).cue` | Swept to 100M CPU checkpoints with BIOS/Sony-logo visible framebuffer parity, `640x478`, `diff=0/611840`. | No CPU or BIOS-logo visual break through 100M. | Add a route through Capcom's movie/background path and record its first parity break. |
| 16 | Tony Hawk's Pro Skater 2 | Not local | Not local. | No legal local image available for parity work. | Add only after legal local media exists. |
| 17 | Street Fighter Collection / Alpha 2 Gold | `Street Fighter Collection - Street Fighter Alpha 2 Gold (USA) (Disc 2).cue` | Swept to 100M CPU checkpoints with BIOS/Sony-logo visible framebuffer parity, `640x478`, `diff=0/611840`. | No CPU or BIOS-logo visual break through 100M. | Add a fight route and record its first parity break. |
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
Skipped by the old pad-routed exact-trace guard. The harness now
fast-forwards pad-routed exact probes with progress reporting and should
be able to refine this window while the pad schedule is inactive.

Visual diff:
Skipped. A separate local `probe_fmv_path` run with the same route reaches the first playable room at 2.2B user steps.

Artifacts:
`target/local-lockstep/re2-route-300m-20260505/`

Subsystem hypothesis:
Timing/scheduler drift, likely CD-ROM/DMA cadence during RE2 executable startup or first MDEC stream setup. This is not yet a renderer failure: the first pinned mismatch has identical PC and GPR/COP2 state hash.

Next probe:
Superseded by the 2026-05-06 exact SIO fix below.

### 2026-05-06 - Resident Evil 2 SIO IRQ timing drift fixed at exact step

Disc:
`/Users/ebonura/Downloads/ps1 games/Resident Evil 2 - Dual Shock Ver. (USA) (Disc 1)/Resident Evil 2 - Dual Shock Ver. (USA) (Disc 1).cue`

BIOS:
`/Users/ebonura/Downloads/ps1 bios/SCPH1001.BIN`

Redux:
`/Users/ebonura/Desktop/repos/pcsx-redux/pcsx-redux`

Commands:
`probe_raw_irq_trace 266946809 RE2.cue`

`probe_redux_raw_step 266946809 RE2.cue` with the Redux C++ trace hook writing `target/re2-diagnostics/re2-redux-fold.trace`

Steps / interval / visual:
Exact folded user step after 266,946,809 completed user steps. Visual skipped.

Last matching raw instruction:
Local and Redux match instruction-for-instruction across the captured ISR window through Redux trace line 10,292.

First old raw divergence:
Before the fix, local branched on `I_STAT` bit 7 too early in the BIOS handler around `pc=0x80096ba8`, then raised extra SIO/controller interrupts while RE2 probed port 2.

Fix:
SIO scheduled events are delivered from the branch-boundary/post-op drain, and missing devices now return `0xff` without scheduling a no-device ACK/DSR IRQ. This mirrors Redux's port-2 no-controller behavior during RE2's BIOS pad poll.

Post-fix exact result:
Local finishes the folded ISR at `cycles=608395751`, delta `22909`, `pc=0x00001ea8`, `I_STAT=0x000`, matching the Redux tick that previously exposed the 402-cycle drift.

Route sweep status:
`local_lockstep_sweep --disc RE2.cue --steps 267000000 --interval 267000000 --exact-window 0 --no-visual --pad-pulses ... --redux-wall-timeout-secs 1800` matches Redux at 267M. The original `(266M, 267M]` route checkpoint break is fixed.

Follow-up route status:
`local_lockstep_sweep --disc RE2.cue --steps 270000000 --interval 10000000 --exact-window 10000 --no-visual --pad-pulses ... --redux-wall-timeout-secs 1800` reached checkpoint 27/27 and stopped on a coarse 270M mismatch. That run's exact refinement was manually stopped because the 10M interval only exact-traces the first 10k steps after 260M. A single 268M checkpoint then failed, while 267M passes. A 267.5M checkpoint failed tick-only, so the active first-break window became `(267M, 267.5M]`.

267.5M mismatch:
ours `{tick:609759007 pc:0x80086060 state:f8a29d610771d2d1}`; Redux `{tick:609754322 pc:0x80086060 state:f8a29d610771d2d1}`. This is tick-only again, with PSoXide 4,685 cycles ahead.

Dense sweep:
`local_lockstep_sweep --disc RE2.cue --steps 267500000 --interval 50000 --exact-window 50000 --no-visual --pad-pulses ... --redux-timeout-secs 1800 --redux-wall-timeout-secs 3600` narrows the first new break to checkpoint window `(267150000, 267200000]`.

Exact new mismatch:
`step 267175364`; ours `tick=608965378 pc=0x8008602c instr=0x00000000`; Redux `tick=608965346 pc=0x8008602c instr=0x00000000`. The PC and instruction still match, and the first reported delta is tick-only: PSoXide is 32 cycles ahead.

Local VBlank folded-step probe:
`probe_raw_irq_trace 267175363 RE2.cue` with the same pad pulses starts at `cycles=608942529`, `I_STAT=0x001`, `I_MASK=0x00d`, and enters the IRQ vector from `pc=0x8008602c`. Local finishes at `cycles=608965378`, `pc=0x8008605c`, `I_STAT=0x000`, with `raw_isr=10362`, delta `22849`, and `mem_access_cycles=2123`. The hot path is the BIOS `0x80096b30..0x80096bac` polling loop; top counts are 117/116 visits. Redux folded delta for the same step is `22817`, so the unexplained delta is exactly 32 cycles.

268M mismatch:
ours `{tick:610971271 pc:0x8008602c state:3826149412b538c4}`; Redux `{tick:610957346 pc:0x8008602c state:8cc64b3fdcce25ad}`. The PC still matches, but the state hash now differs, so this is a new post-SIO-fix divergence rather than the original tick-only drift.

Canary status:
CTR and Metal Slug X were rerun with `commercial_route_matrix` at 300M. CTR remains stuck on SCEA splash. Metal Slug X remains route-progress/loading-screen only.

Artifacts:
`target/re2-diagnostics/re2-local-fold-after-no-dsr-timeout.trace`

`target/re2-diagnostics/re2-redux-fold.trace`

`target/local-lockstep/re2-route-300m-after-sio-20260506/`

`target/local-lockstep/re2-route-267m-single-after-sio-20260506/`

`target/local-lockstep/re2-route-267_5m-single-after-sio-20260506/`

`target/local-lockstep/re2-route-267_5m-50k-after-sio-20260506/`

`target/re2-diagnostics/re2-local-vblank-267175364.trace`

`target/local-lockstep/re2-route-268m-single-after-sio-20260506/`

`target/commercial-route-matrix/canaries-after-sio-20260506/`

Subsystem hypothesis:
The fixed break is SIO/scheduler IRQ semantics, not CD-ROM or GPU. The next pinned RE2 break is VBlank/timer/CPU memory timing inside a long BIOS IRQ polling path; it is not currently a CD-ROM data-delivery break. The shared CTR/Metal Slug X canary blocker may still be CD-ROM/DMA/scheduler timing, but it did not move after the SIO fix.

Next probe:
Capture the equivalent Redux raw VBlank trace for cycles `608942529..608965346` and compare PC counts plus memory-cycle accounting against `target/re2-diagnostics/re2-local-vblank-267175364.trace`. Do not change CD-ROM/DMA cadence for this RE2 break until the 32-cycle VBlank delta is explained.

### 2026-05-06 - Metal Slug X route/render target

Disc:
`/Users/ebonura/Downloads/ps1 games/Metal Slug X (USA)/Metal Slug X (USA).cue`

BIOS:
`/Users/ebonura/Downloads/ps1 bios/SCPH1001.BIN`

Redux:
`/Users/ebonura/Desktop/repos/pcsx-redux/pcsx-redux`

Commands:
`commercial_route_matrix --disc MetalSlugX.cue --steps 530000000 --wall-timeout-secs 120 --dump-visible --report-dir target/commercial-route-matrix/metal-slug-x-530m-after-rect-span-20260506`

`probe_fmv_path 500000000 MetalSlugX.cue` and `probe_fmv_path 530000000 MetalSlugX.cue` with 150 extended START/CROSS/DOWN pulses through vblank 3332.

`local_lockstep_sweep --disc MetalSlugX.cue --steps 1000000 --interval 1000000 --exact-window 0 --no-visual`

`local_lockstep_sweep --disc MetalSlugX.cue --steps 100000000 --interval 25000000 --exact-window 0 --no-visual --pad-pulses ...`

Local route result:
The stock 30-pulse route passes the earlier data-detection failure and reaches the 300M loading-screen state (`display_hash=0x36cb4b8cb6c42d59`). Extending the same stock route to 530M reaches a new black visible frame classified as `render/gpu`: `display_hash=0x1100fdb97cd50325`, display `320x240`, `pc=0x8006e5dc`, cycles `1212263862`, vblank `2147`, CD data IRQs `955`, sector events `955`, FIFO pops `1963508`, pad polls `1139`, memcard commands `77`, and MDEC macroblocks `0`.

Visible evidence:
`target/commercial-route-matrix/metal-slug-x-530m-after-rect-span-20260506/Metal_Slug_X__USA_/metal-slug-x-data.ppm` is a fully black visible frame. The display mode history shows the game alternating display starts between `(0,0)` and `(0,240)`, so the next render probe should inspect both VRAM pages and the GP1 display-start cadence before assuming the content itself was not rendered.

Follow-up page evidence:
`probe_fmv_path 400000000` with full VRAM and visible dumps shows real visible content at 400M: display page `(0,240)` has `24804/76800` nonblack pixels and 175 distinct sampled colours. `probe_fmv_path 530000000` shows both configured display pages `(0,0)` and `(0,240)` completely black, while offscreen VRAM regions at `x=320` and `x=512` still contain texture/content data. Recent GP0 logs confirm this is not a display-start decode error: the game explicitly issues repeated `GP0 0x02` clears for both 320x240 buffers after reprogramming draw areas.

550M boundary:
The stock route completes at 545M (`display_hash=0x1100fdb97cd50325`, GPU DMA `1773`) but does not reach the 550M checkpoint in a useful wall-clock budget. Host samples are dominated by `Gpu::paint_rect`. A gated mono-rect trace identifies the hot packet as repeated `GP0 0x62` variable-size subtractive rectangles: `cmd=0x62dfdfdf`, `pos=0xff80ff60`, `size=0x01000140`, clipped to `(8,8) 304x224`, colour `0x6f7b`, mode `Sub`. The same packet repeats thousands of times after the 525M checkpoint. CD-ROM delivery is not advancing in this window (`955` data IRQs, `read_lba=2926`, FIFO pops unchanged), so the route is likely sitting in a render/loading wait loop rather than progressing toward gameplay.

Extended-input route result:
A longer pulse schedule changes the route after 400M: hashes advance through `0xc7841dc90e810978`, `0xaa651a5fdbd301c1`, `0x9d8612f8339fb5af`, and `0x8ced4c5a4f1797ac` by 475M, with GPU DMA increasing from 892 to 1355. By 500M the route is still black on the visible page but has more CD traffic (`979` data IRQs) and much heavier GP0 traffic. Host samples after 500M are dominated by `Gpu::paint_rect` called from GP0 writes, especially semi-transparent monochrome rectangle traffic.

Code change:
The monochrome rectangle raster path now writes clipped rows directly through `Vram::words_mut()` when pixel-owner tracing is disabled, and uses `Vram::fill_rect_unwrapped()` for opaque unmasked spans. This preserves blending/mask behavior while avoiding wrapped per-pixel `get_pixel`/`set_pixel` calls. It does not change the 530M stock route classification or visible output.

Redux parity status:
The no-pad 25M Redux control passes quickly, proving the disc and base oracle still work. After moving `run_checkpoint_pad` mask recomputation from every instruction to VBlank edges, the stock 30-pulse routed run now matches Redux through 25M and 100M checkpoints (`interval=25M`). No Metal Slug X route parity break is pinned yet.

Artifacts:
`target/commercial-route-matrix/metal-slug-x-530m-after-rect-span-20260506/`

`target/metal-slug-x-500m-long-pulses.ppm`

`target/metal-slug-x-530m-after-rect-span.sample.txt`

`target/local-lockstep/metal-slug-x-smoke-1m-20260506/`

`target/local-lockstep/metal-slug-x-route-25m-20260506/`

`target/local-lockstep/metal-slug-x-route-25m-after-pad-oracle-20260506/`

`target/local-lockstep/metal-slug-x-route-100m-after-pad-oracle-20260506/`

Subsystem hypothesis:
Local game progress is no longer blocked at data detection. The active local blocker is render/GPU/display-page behavior after the loading path, with a secondary harness-throughput issue under heavy rectangle traffic. The route parity harness is usable again through 100M, so the next useful parity target is the 300M loading-screen window.

Next probe:
Run the routed parity sweep to 300M, then compare the 525M-550M wait loop against Redux: if Redux keeps receiving CD sectors or exits the subtractive-rect loop, the owner is likely CD-ROM/DMA/scheduler. If Redux repeats the same loop, route input/game-specific menu state is the next suspect.

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
