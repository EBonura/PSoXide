# PSoXide ecosystem optimisation and release handoff

**Date:** 2026-08-23
**Scope:** Cortex Ignition on the PSoXide engine, `quake-psx`, `hl-psx`, the shared PSoXide SDK/emulator, and the combined demo disc
**Status:** authoritative handoff for the next optimisation agent
**Primary requirement:** improve real PlayStation performance without reducing visuals, gameplay, packet safety, RAM safety, or standalone/demo-disc compatibility

This document replaces conversational summaries from the 2026-08-20 through
2026-08-23 optimisation sessions. It records what was actually accepted, what
was rejected, the measurements behind those decisions, the release artifacts
that were validated, and the exact process a new agent should follow.

The short version is:

- Cortex gained substantial BSP and model hot-path improvements.
- Quake gained a robust RAM/streaming/stack repair and retained exact visuals,
  but the attempted fused renderer was slower and was removed.
- Half-Life gained a packet-capacity correctness repair with exact final visual
  equivalence, but no meaningful speed increase in the latest round.
- Shared profiling, exception, resource-lifetime, font/VRAM, and release
  provenance infrastructure is materially stronger.
- A deterministic combined disc containing all three current games exists and
  passes the emulator release gate.
- No final real-hardware acceptance has happened yet. Emulator success is not
  silicon proof.
- The live repositories are not clean. Do not benchmark, commit, merge, or
  repin them blindly. Use the frozen clean snapshots and receipt described
  below as the current evidence boundary.

---

## 1. Mission and non-negotiable constraints

The eventual outcome must be:

1. Cortex Ignition, Quake Shareware, and Half-Life all work as standalone
   discs.
2. All three chain-load correctly from one combined PSoXide demo disc.
3. Performance improves where a measured route shows a bottleneck.
4. Visual output does not downgrade. Missing geometry, altered topology,
   reduced affine correction, dropped packets, or a smaller draw distance do
   not count as optimisation.
5. Gameplay state, input timing, collision rounding, face winding, ordering,
   translucency, animation quality, and texture quality remain equivalent.
6. RAM, stack, packet, VRAM, CD streaming, and linker margins are explicit
   acceptance gates.
7. Original PlayStation hardware is the final authority. Emulator-only wins
   remain provisional when they touch GTE scheduling, GPU pacing, DMA, CD,
   scratchpad execution, or SPU behaviour.

Rules for every experiment:

- Freeze a fresh baseline and its hashes immediately before the candidate.
- Change one hypothesis at a time.
- Use the exact same source, cooked data, toolchain, frontend, route, and stop
  condition for baseline and candidate.
- Resolve hot addresses against the exact candidate map/disassembly.
- Measure a route-specific upper bound before implementing assembly.
- Reject and fully revert a candidate as soon as its gate fails.
- Do not call a fixed-route screenshot mismatch a visual regression until the
  two images have been aligned by guest frame, tape sample, or semantic state.
- Do not call a final-frame match proof of equivalence. Review checkpoints,
  packet counts, GPU streams, VRAM, and representative difficult geometry.
- Do not use host wall-clock time as guest performance truth.
- Do not use executable scratchpad code. Real PS1 instruction fetch from
  scratchpad is invalid even if an emulator happens to run it.
- Do not use `opt-level = "s"`; it is known to miscompile this guest target.
- Do not publish or burn a diagnostic build.

---

## 2. Repository and source-state boundary

### 2.1 Live worktrees: preserve, but do not treat as frozen baselines

| Repository | Live path | Live HEAD | Current live state |
|---|---|---|
| PSoXide | `/Users/ebonura/Desktop/repos/PSoXide` | `a8c69b296ee59fe3a744e1051a8ea2cc17be62a0` | 26 modified paths, no staged paths at handoff; diff SHA-256 `b9818ff94fa4da54acda1d51deb433cc825adc52ddb94e92d43a8b9c693eeff9` |
| quake-psx | `/Users/ebonura/Desktop/repos/quake-psx` | `c56c416aca42b9fc48256b639130b56772a69294` | 7 modified paths; diff SHA-256 `257514aef2f891540444c3babecc5283cebbe937057c0210e9a3b640e3d306f1` |
| hl-psx | `/Users/ebonura/Desktop/repos/hl-psx` | `8d9e4c50987be98d25b1b6f78e290e0e570bcb72` | `game/src/main.rs` modified; diff SHA-256 `ca460bb27f22bc84e530cf6e958a0aec85a0e4d0ffc7ca402412c00d134b0d23` |
| psx-demo-disc | `/Users/ebonura/Desktop/repos/psx-demo-disc` | `6a619c98c3bed0dd10b5d1d8356214afec92e0aa` | `Makefile` and `README.md` modified; label and release/checker tools untracked; tracked diff SHA-256 `a1124995f518d00cbc59edf9c491877e3211fa799164891296c9f7e9d81c7b21` |

The live PSoXide project also contains concurrent UI work named `HUD Shape
Lab`. That UI addition is not present in the clean release snapshot described
below. Preserve it. Do not overwrite `editor/projects/default/project.ron`
from the release snapshot.

### 2.2 Frozen clean revisions used for the validated release

The release receipt records these clean revisions as the source provenance for
the current validated artifacts. The temporary checkouts themselves were
deleted during the owner-requested cleanup on 2026-08-23; they are not current
working paths.

| Program | Clean revision | Notes |
|---|---|---|
| Cortex/PSoXide | `7b95d937e561dd585780bb7ee33b85cdcc3e8e4f` | `1e8cb8de` shared optimisation/resource snapshot plus safe Cortex camera start |
| Quake | `da3023ccbe0a4acd7164731d9788cb80a3d428b9` | accepted seven-file Quake repair plus final PSoXide pin `1e8cb8de` |
| Half-Life | `9ab1c8d495443160a126945c7f5acfbd8dd7229b` | accepted one-file actor packet repair, rebuilt against final PSoXide snapshot |

Important reconciliation facts:

- The 25 non-project files in PSoXide clean commit `1e8cb8de` are byte-exact
  with the corresponding live files at handoff.
- The live Cortex project differs from clean `7b95d937` only by later UI
  authoring in the `HUD Shape Lab`; the camera yaw repair is already in the
  live project.
- The seven live Quake files match the clean snapshot except
  `host/quake-build/main.rs`, where the live tree still declares old PSoXide
  revision `a33d7eb...` and the clean release snapshot declares
  `1e8cb8de...`.
- The live Half-Life `game/src/main.rs` is byte-exact with clean snapshot
  `9ab1c8d`.
- The clean snapshot revisions are receipt provenance, not evidence that the
  normal repository branches were pushed. Reconcile the validated live diffs
  into ordinary clean commits before rebuilding a release.

Never run `git reset`, `git checkout --`, or a broad clean operation in these
repositories. User and concurrent-agent work is present.

---

## 3. Current combined burn candidate

The emulator-validated combined image is:

`/Users/ebonura/Desktop/repos/psx-demo-disc/dist/hardware-candidate/PSoXide Demo Disc HL.cue`

| Artifact | SHA-256 | Size |
|---|---|---:|
| CUE | `a8d2a7b80c66665418644b14b5d8c98110429979b49b25e78a96b2a32be44e42` | 2,218 B |
| BIN | `17b751f3b3ce50cb35e00f1e32114326e82c6e287f4232d419aee991a4c1648b` | 715,024,464 B / 304,007 sectors |
| Release receipt | `ca23f1f875b147da3fc4927c9db09c1a6d79361ada624977a38aa1220c2bc6f5` | JSON |
| Frontend used by receipt | `f49b0f4bf285e192c3bd6445e07e30ce52a64b477c16266b3acbc1f23fbf879e` | 23,946,544 B |

The receipt is:

`/Users/ebonura/Desktop/repos/psx-demo-disc/dist/hardware-candidate/PSoXide Demo Disc HL.release-receipt.json`

It binds the combined image to clean source revisions and embedded payloads.
Key inputs:

| Entry | Source revision | Input BIN SHA-256 | Input EXE SHA-256 |
|---|---|---|---|
| Cortex Ignition | `7b95d937...` | `0bef480aa422033f4f18022c4ff57860f1906342d0d6a2c0b915964481b1eb29` | `edfe358d84717e12296802c7ce829d879002f799822f379e2182bdd09a43ca41` |
| Half-Life | `9ab1c8d...` | `b3126a2dadac04ae902e1bb484e9cc394c2bf0c607fcf56e80f0482b5384c7d3` | `80f044a964325f62180198702e26a64fd661013a754300b444f52f0e4be1f82a` |
| Quake Shareware | `da3023c...` | `dc2fb60a07234b54d217620f3928e596f347de2c5b38bded1e4893c507ca7c0c` | `4421733ef8f6b1e4f67ff011d24fde26a7b137c66f14be1e30c07a89c6fc5a0a` |

The Quake shareware PAK is separately pinned at SHA-256
`35a9c55e5e5a284a159ad2a62e0e8def23d829561fe2f54eb402dbc0a9a946af`.
Redistribution remains blocked pending separate legal/release approval.

### Combined-disc automated gate

All five release-critical targets were launched twice from the combined disc:

- Cortex Ignition
- Cortex Ignition Legacy
- Half-Life
- Hardware Tests
- Quake Shareware

For each target, route, CD, GPU, PC, and final display logs were byte-exact
between the two cold runs. Cortex also passed a sustained textured-gameplay
gate and a geometry-presence gate.

Corrected evidence is under:

`/Users/ebonura/Desktop/repos/psx-demo-disc/dist/hardware-candidate/chainload-evidence`

It contains 50 files. A sorted SHA-256 manifest-of-hashes evaluates to:

`dd30cdbeb3c49cfc26332591ab5cf2fcd957f98c4e6f9b4430650170e33eb397`

Run the receipt and combined gate again with:

```bash
cd /Users/ebonura/Desktop/repos/psx-demo-disc

python3 tools/release_receipt.py verify \
  --sealed \
  --receipt "dist/hardware-candidate/PSoXide Demo Disc HL.release-receipt.json"

# A fresh chain-load rerun uses a newly built canonical frontend; it is a new
# validation run, not the historical receipt frontend.
cargo build --manifest-path /Users/ebonura/Desktop/repos/PSoXide/Cargo.toml \
  -p frontend --release
EMPTY_EVIDENCE="$(mktemp -d /private/tmp/psoxide-release-recheck.XXXXXX)"
python3 tools/check_release_chainloads.py \
  --frontend /Users/ebonura/Desktop/repos/PSoXide/target/release/frontend \
  --cue "dist/hardware-candidate/PSoXide Demo Disc HL.cue" \
  --artifact-dir "$EMPTY_EVIDENCE"
```

The old visually invalid Cortex candidate was permanently deleted during the
2026-08-23 cleanup. `dist/hardware-candidate` is the only retained burn
candidate directory.

---

## 4. Shared architectural work that was accepted

### 4.1 Exact low-level profiling infrastructure

PSoXide now has opt-in, emulator-owned instrumentation for:

- disjoint CPU cycle classes;
- exact retired instruction classes;
- exact 16-byte PC-line execution counts;
- exact I-cache refill events including victim and incoming line;
- periodic PC, call-site, and window samples;
- rooted and full-run stack profiling;
- route, CD, GPU, semantic-counter, guest-hash, and visual-hash logging.

Key frontend flags:

```text
--cpu-cycle-profile-log
--instruction-class-log
--pc-line-log
--pc-line-start-route-tick
--icache-event-log
--icache-event-start-route-tick
--pc-sample-log
--pc-sample-callsite-log
--pc-sample-window-log
--pc-sample-window-ticks
--stack-profile-log
--stack-profile-root-pc
```

Exact temporal I-cache pairs are ranked with:

```bash
python3 /Users/ebonura/Desktop/repos/PSoXide/tools/pc_line_attribution.py \
  /path/to/pc-lines.csv /path/to/exact-build.map \
  --icache-events /path/to/icache-events.csv
```

Do not rank cache layout from set occupancy alone. Occupancy does not identify
the line that evicted another line. This mistake produced false priorities in
earlier work.

The instrumentation is disabled in ordinary play. A five-run host-only test
measured the optional event capture/drain at about +2.3% host wall time while
guest cycles, PC, route, and polls were identical. The disabled path neither
captures nor drains events.

### 4.2 Fatal exception handling

The emulator correctly raises instruction bus errors for scratchpad and
unmapped instruction fetch. The PS1 runtime now hard-stops:

- BREAK;
- instruction bus error;
- instruction-side address error where `BadVAddr == EPC`.

It does not advance EPC and resume through a fatal instruction exception.
Integration probes ran each fault for roughly 100,000 instructions and
observed exactly one captured fault followed by a stable self-loop. This is
important because `panic=immediate-abort` emits BREAK. Before this fix, a panic
could skip past BREAK and corrupt state instead of halting.

Executable scratchpad remains forbidden. The emulator accepting scratchpad
instruction fetch would be a bug, not a performance opportunity.

### 4.3 Inclusive shared screen bounds

The shared classic-affine screen rejection had used right/bottom bounds of
`screen_dimension - 2`. This dropped one legitimate Quake edge pixel. The
accepted bound is `screen_dimension - 1`, giving the inclusive 0..319 and
0..239 screen.

Quake proof:

- final world hash `0x951a75babd8f2904`;
- HUD hash `0x09e962893d496136`;
- display hash `0x42dbcf3360f22de8`;
- byte-exact display parity to the immediate pre-harmonisation parent;
- still 24,464 fewer packets, or -6.22%;
- still 32,560 fewer triangles, or -7.13%;
- zero packet overflow.

This is a correctness repair with retained culling, not a new speed claim.

### 4.4 Shared resource-lifetime and release infrastructure

Accepted manager-level improvements include:

- fail-closed release receipts binding source commits, artifacts, EXE payloads,
  and combined-disc LBA ranges;
- deterministic chain-load gates for all five important demo-disc programs;
- external PXBSP face-chain storage supplied by an audited runtime arena
  lifetime instead of duplicate heap buffers;
- compile-time runtime-arena union-size assertions;
- scene-aware font/VRAM acquisition returning a `#[must_use] bool` rather than
  silently leaving partial texture state;
- six authored UI fonts packed into two atlas pages rather than six pages;
- a visual Cortex geometry gate that rejects the old missing-wall screenshot.

The font repack uses 30,848 scratch halfwords versus 46,080 previously.
Glyphs and padding were exhaustively compared and are exact. The release MIPS
guest built successfully with six fonts.

For the PXBSP/runtime arena overlay, a fixed-path MIPS ELF comparison measured:

- loaded `.text`: 824,272 -> 634,012 bytes, -190,260;
- loaded `.data`: 1,096,752 -> 1,090,400 bytes, -6,352;
- `.bss`: unchanged at 73,088 bytes;
- avoided heap payload for two external chains: 9,772 bytes;
- combined loaded-plus-heap headroom improvement: 206,384 bytes.

This was primarily a RAM and code-shape improvement. Do not invent an FPS
claim from it.

---

## 5. Cortex/PSoXide: accepted performance work

### 5.1 E1M1 visible-face chain

Original renderer behaviour scanned all 5,724 E1M1 faces every frame after
building a PVS bitmap. At the captured stationary view:

- 86 PVS-visible leaves;
- 574 face references;
- 484 unique visible faces;
- 90 duplicate references;
- 5,240 faces were skip-only iterations every frame.

The accepted persistent visible list is sorted in original face order and is
rebuilt only when the PVS leaf changes. It reduces the loop from 5,724 to 484
entries, or 11.83 times fewer entries.

Measured A/B:

| Metric | Baseline | Visible chain | Delta |
|---|---:|---:|---:|
| render cycles / visual | 2,032,738.1 | 1,819,166.7 | -10.507% |
| PXBSP room cycles / hit | 1,424,779.6 | 1,209,761.4 | -15.091% |
| visual render task | 2,339,795.0 | 2,069,804.3 | -11.539% |
| gameplay flip interval | 5.052 route ticks | 4.474 | -11.44% |

World output stayed exact: every gameplay frame emitted 208 textured world
quads and identical world GPU cycles. Common guest frames and static BSP
pixels were exact.

### 5.2 Quake-style node bounds and face-range traversal

The next accepted step retained BSP29 node bounds and local face ranges in a
compact PXBSP node representation, then traversed visible/frustum-relevant
nodes instead of performing deep work on every PVS face.

Measured against the accepted visible-chain baseline:

| Metric | Visible-chain baseline | Node traversal | Delta |
|---|---:|---:|---:|
| render cycles / visual | 1,819,166.7 | 1,628,417.8 | -10.486% |
| room cycles / hit | 1,209,761.4 | 998,377.0 | -17.473% |
| room p95 | 1,208,068 | 988,374 | -18.186% |
| visual render task | baseline | -164,032.1 cycles | -7.925% |

Relative to the original full scan:

- render: -19.890%;
- room: -29.928%;
- visual render task: -18.550%;
- gameplay flip interval: -19.63%.

Cost: the first room render rebuilds node/PVS state and spikes once:

- first room: 1,435,862 -> 2,203,955 cycles, +53.49%;
- first render: 2,198,434 -> 2,976,188 cycles, +35.38%.

Steady-state benefit is decisive, so the traversal was retained. The cold
rebuild remains a follow-up: precompute during load or hide it before the first
presented gameplay frame.

### 5.3 Aligned Q11 animation decode

The existing 20-byte animation record and Q11 quality were preserved. The
fast path performs five aligned 32-bit loads and bit extraction; unaligned
records use the exact byte fallback.

Validation covered all 4,096 Q11 values in all nine lanes, plus mod-4 pointer
alignments 0, 1, 2, and 3.

E1M1 result:

| Metric | Baseline | Candidate | Delta |
|---|---:|---:|---:|
| decoder retired instructions | 1,435,828 | 1,174,215 | -18.2% |
| `textured_model_joints` cycles | 13,425,138 | 11,563,850 | -13.864% |
| active render cycles | 106,627,036 | 104,749,867 | -1.761% |

All 11,183 measured calls used the aligned path. All 120 visual hashes, all
guest hashes, 396 x 25 semantic counters, and the complete GPU stream were
exact. The executable shrank by 2,048 bytes and heap headroom improved by
2,048 bytes.

This optimisation applies immediately to Cortex/PSoXide. Quake alias models
use a different representation. Half-Life has the same Q11 concept in local
code, but no shared adapter was accepted in this round.

### 5.4 Scheduled zero-UV model-face walker

For the measured authored-zero-UV model route, a specialised flat walker fills
otherwise idle NCLIP hazard-gap instructions with independent palette and UV
bit extraction. It preserves the silicon-required gap before MAC0.

E1M1 lockstep result over the accepted Q11 build:

| Metric | Q11 baseline | Q11 + face walker | Delta |
|---|---:|---:|---:|
| model face stage | 19,713,711 | 18,331,431 | -7.01% |
| active render | 100,479,025 | 98,334,281 | -2.14% |
| visual task | 117,988,423 | 116,877,405 | -0.94% |

Shipping-cadence run:

- route CPU ticks: -3.88%;
- deadline misses: 56 -> 48;
- skipped VBlanks: 198 -> 189;
- face stage: -6.77%;
- active render: -1.36%.

Exact gates:

- 120/120 visual hashes;
- 4/4 guest hashes;
- 242 semantic rows;
- 116 cumulative GPU stream hashes;
- final 543,300 GPU words and hash `0x68b65ad4997cc584`;
- exact display, VRAM, and stack.

Static cost:

- specialised text +1,848 bytes;
- linker alignment consumes 2,048 bytes of heap headroom;
- no BSS or stack increase.

This scheduled GTE helper still requires real-console validation before it is
called silicon-proven.

### 5.5 Editor-side performance and visual correctness

The full E1M1 editor work also produced important authoring improvements:

- solved brush geometry cache shared by render, lighting, and grid overlay;
- exact near and side-plane clipping instead of dropping near-crossing
  triangles or clamping offscreen faces into wedges;
- fixed-capacity clipping storage to remove per-face temporary vectors;
- brush-surface grid overlay with a bounded 2,048-line global cap.

Release editor benchmark on the 1,213-brush project:

| Metric | Before/reference | Accepted | Delta |
|---|---:|---:|---:|
| grid overlay stage | 4.487 ms | 0.607 ms | -86.5% |
| grid allocations | 109,330 | 2,058 | -98.1% |
| grid requested bytes | 14.584 MB | 0.295 MB | -98.0% |
| full-frame allocation calls | 91,013 | 26,512 | -70.9% |
| full-frame requested bytes | 17.419 MB | 1.592 MB | -90.9% |

Final measured full editor path was 6.718 ms median and 7.053 ms p95 on that
machine. The same-semantics uncached counterfactual was 10.598 ms, so the cache
cut median CPU cost by 36.6%.

The clipping fix restored missing editor walls/floors and was pixel-exact
between its allocating and fixed-stack implementations. Runtime PXBSP already
used polygon clipping and was not changed by that editor fix.

---

## 6. Quake: accepted repair and measured outcome

The current Quake integration initially did not boot. This was not an arena
overflow or a renderer crash.

### 6.1 Root causes

1. Renderer startup tried to allocate `1,325 * 48 = 63,600` bytes for the
   visible-face cache while only 40,764 heap bytes remained.
2. The Quake `run` stack had grown near its 32 KiB reserve.
3. During record-by-record PSB5 node expansion, a CPU-heavy ReadN stream could
   drop the sector crossing source offset 2,048. The next node decoded from
   the wrong bytes and `SharedResidentMap::load` returned `BadNode`.

### 6.2 Accepted repair

- Right-size both GPU packet arenas from 192 KiB to 128 KiB.
- Cache the compact Nodes lump in the existing 30 KiB streaming scratch,
  pause/close ReadN during CPU expansion, and reopen at ClipNodes.
- Reserve a Quake-local 52 KiB stack in the generated linker script.
- Make the shipping heap gate use the truthful new heap end.
- Add exact packet-arena high-water and overflow telemetry.
- Use the inclusive 319/239 shared screen bounds.

### 6.3 Final gates

- Shipping boot: live PC, 2,211 controller polls.
- Heap: 1,155,252 / 1,198,492 bytes used, 43,240 bytes free.
- Full-route stack: minimum SP `0x801f5f88`; 40,824 bytes below
  `STACK_INIT`; no-overlap margin 12,424 bytes.
- Episode 1 route: Start plus E1M1 through E1M8, 747 frames, 12 loads,
  all map and transition masks complete.
- Packet arena high water: 28,279 / 32,768 words, or 113,116 / 131,072
  bytes; 17,956-byte margin; zero overflow frames.
- Visual parity: exact world, HUD, and display hashes listed in section 4.3.

### 6.4 Performance conclusion

Final E1M1 cadence moved from 21.236 to 21.222 fps, a -0.014 fps difference.
The established layout noise band was about 0.122 fps. Therefore this round
did not establish a Quake speed improvement. It established a bootable,
memory-safe, stream-safe build while retaining the existing screen-outcode
culling win and exact visuals.

Do not describe the repair as a Quake FPS gain.

---

## 7. Half-Life: accepted correctness/capacity work

### 7.1 The real failure

The normal Hazard route was safe at the current packet limit, but map `c1a4i`
was not. With 1,469 fixed packet slots it reached zero free slots and rejected
183,482 actor model FT3 allocations in a bounded run. The central hanging
creature/tentacle was visibly truncated.

The drops were 100% flat `TriTextured` actor model faces, not world geometry,
underlay, FX, or room packets.

### 7.2 Accepted repair

- Emit actor FT3 packets into one contiguous tagged 9-word model stream rather
  than one 14-word fixed slot per triangle.
- Add a conservative common viewport half-space reject: reject only if all
  three vertices share the same outside half-space.
- Trim 37 old fixed slots so the code/data/BSS trade is RAM-neutral overall.

The packet stream preserves the same authored order and OT prepend semantics.

### 7.3 Final result

On `c1a4i`:

- zero model packet rejects;
- 349 submitted model triangles at the sampled peak;
- central missing model restored;
- fixed-slot-equivalent free margin 399 in the focused build.

On the full shipping Hazard route:

- all 12,265 controller polls consumed;
- zero packet drops and zero room allocator overflow;
- shipping packet floor at least 243 slots;
- full telemetry stack high water 18,388 bytes;
- final display and VRAM exact to the accepted control;
- no meaningful speed claim.

Final rebuild against PSoXide `1e8cb8de`, compared with the authoritative
accepted live artifact using the same final frontend and tape:

- 11,243 flips in both;
- cadence metric 46.810377 -> 46.807094, -0.0070%, effectively neutral;
- 42 of 47 route checkpoints byte-exact;
- five mismatches were bounded presentation-phase shifts;
- final display and full VRAM byte-exact;
- same maximum GPU words, commands, and draws;
- scoped `hl_psx::play` stack depth 8,408 bytes, leaving 24,336 bytes above
  the stack/heap floor.

This is a correctness and capacity fix, not an optimisation-speed win.

### 7.4 Current authoritative tape

The optimisation skill's older 24,943-sample tape is not the current release
route for this work. The current menu-compatible poll-bound tape is preserved
inside the canonical demo candidate:

`/Users/ebonura/Desktop/repos/psx-demo-disc/dist/hardware-candidate/tapes/hazard-current-menu.pxtape`

- format: `PXITAPE2`;
- samples: 12,265;
- starting controller poll: 52;
- SHA-256:
  `f814caa88918a862154d91a8c6e45682979ec52d95555760fd67d7d39532ff68`.

The older `hazard-t0a0.pxtape` with SHA `b9f0137b...` no longer selects the
correct menu entry after the menu-order merge.

Final visual/stack evidence:

- `/private/tmp/hl-hazard-visual-ab`
- `/private/tmp/hl-final-hazard-gate`
- final display PPM SHA-256
  `ec2fd9be071e2531f71ae6f71683e4d6461aec3151aebf5daffc23a64e23ecbf`
- final VRAM PPM SHA-256
  `4e3682b4114e70df15a2dbc08ef1db86ce7ead2d515cc030722dd87c695f2c4d`
- rooted stack CSV SHA-256
  `8d14b2ee48a48102b263b997c8a9bb9f304cc01155320ca88cacd2dd4448668b`

---

## 8. Rejected or inconclusive experiments: do not repeat unchanged

### 8.1 Cortex/PSoXide

| Candidate | Result | Decision |
|---|---|---|
| Specialise the offset/reflection layer like the zero-UV walker | Face stage +2.38%, active render +0.62%; GPU stream changed from 543,300 to 545,522 words and one visual checkpoint differed | Rejected and reverted |
| Remove the per-frame selected face list and scan the persistent PVS chain with state marks | Instructions +22.28%, visual task +40.15%, room +87.99%, triangle primitives +64.70%, visual divergence from frame 10 | Rejected and reverted |
| Whole-render scratchpad stack | Required roughly 16,136 bytes | Impossible on 1 KiB scratchpad |
| Executable scratchpad kernel | Emulator may permit it; PS1 instruction bus does not | Forbidden |
| Earlier phase-5 register/assembly projection route | Project substage faster, but net frame approximately zero and visual task +0.07% | Rejected |
| Pad-poll begin/finish overlap | Corrected upper bound only about 1.5% total; not implemented or hardware-gated | Open low-priority experiment, not an accepted win |
| Leaf-only scratch stack for blended model flush | Plausible 1-3% local bound, but no exact stack-stall proof and scratch lifetime needs audit | Not implemented |
| Double-sided Cortex BSP rendering | Restored the bad screenshot only because the camera was outside the room; would hide real winding/camera defects | Diagnostic only, not landed |

The full persistent-chain scan failure is especially important. Do not revive
it as a memory fix. It was both slower and visually different even though the
cooked packs were identical.

### 8.2 Quake

| Candidate | Result | Decision |
|---|---|---|
| Fused Quake-local classic-affine projected packet writer | Exact visuals, but payload +20,480 bytes, gameplay cycles +1.139%, I-cache stalls +21.614%, tail stalls +27% | Rejected and fully removed |
| 160 KiB GPU arenas | Only 3,152 heap bytes free, below 8 KiB floor | Rejected |
| 48 KiB stack reserve | Only 8,128-byte measured margin, 64 bytes below the required 8 KiB safety rule under the exact accounting used | Rejected |
| Static/in-place `EntityScene` and cold/steady split | Extra lifetime complexity; insufficient coherent stack/heap trade | Rejected |
| `panic=immediate-abort` as the RAM repair | Saved 12,288 bytes but was insufficient alone and removed diagnostics; fatal BREAK semantics first had to be fixed | Not stacked |
| More scratchpad render data | Existing Quake batch consumes 1,020 / 1,024 bytes | No capacity |
| Generic Quake I-cache layout shuffle | Historical signal only around 0.5-0.6%, below better targets | Deprioritised |

### 8.3 Half-Life

| Candidate | Result | Decision |
|---|---|---|
| Exact-word packet arena across SDK | Only 42 extra equivalent slots beyond the local actor repair; unnecessary cross-game risk | Rejected/deferred |
| Actor packet stream without common screen rejection | Still dropped packets at the c1a4i peak | Incomplete; do not use alone |
| Exact I-cache placement for strongest temporal pair | Only 0.217% bounded refill-stall reduction | Rejected |
| Duplicated-cell replacement-only cooked surface | Removed 80,500 bytes of legacy loops but remained +101,980 bytes / +18.78% because of duplicated cell vertices and 24-byte commands; panicked at guest frame 26 | Rejected and removed |
| Global-vertex aligned 20-byte command | Exactly 20 bytes replaced exactly four 5-byte records across all 103 maps; fleet delta exactly zero | Stop/go failed; no runtime slice built |
| Additive/source world pipeline | Historical full route -95 flips / -0.93% | Do not revive as positive evidence |
| Shared attributed clipping/projection/outcode adapter | Final display/VRAM exact, but flips 13,793 -> 13,767, route FPS -0.27%, median window 18.596 -> 18.370 | Rejected for HL |
| Coverage-buffer occlusion | Fill cost exceeded the two recovered culls; net about -2.3% | Rejected |
| Small source-addressed GT4 cache | Full tape -0.27% despite exact visuals | Rejected |
| Pre-baked per-patch GPU packet expansion | Worst room has no room for the roughly 147 KiB additive payload | RAM-infeasible without replacing other authoritative state |

The lesson from Half-Life is not that cooked draw surfaces are a bad target.
The rejected layouts were additive or failed to beat the existing 20-byte
payload. A viable design must average under 20 bytes per eligible patch or
eliminate another authoritative resident record at the same time.

### 8.4 Shared-code overreach

Do not force all three games through one hot renderer merely because their
lineage is related. They have different exact contracts:

- Half-Life uses canonical Q12 edge rounding and GoldSrc-specific topology.
- Quake caches transformed depth and uses rounded Q12 interpolation.
- PSoXide PXBSP clips attributed world polygons, currently using Q16-style
  fractions and exact plane signs.

Share data contracts, packet interfaces, profilers, resource managers,
exception behaviour, and carefully parameterised kernels. Keep game-specific
hot loops where a shared abstraction regresses I-cache locality or changes
rounding. The rejected HL shared adapter is direct evidence.

---

## 9. The 64-bit arithmetic question

Do not adopt a simplistic rule that every source-level `i64` is forbidden.
The real rule is:

- no compiler-emitted 64-bit divide/modulo in a hot path;
- no generic compiler scaffolding when exact sign/range can be computed with a
  bounded 32-bit or hand-scheduled HI/LO kernel;
- preserve the exact geometric sign and rounding contract.

PSoXide's BSP plane-distance sign path already uses a specialised MIPS kernel
that accumulates the exact 64-bit result in HI:LO with carries and schedules
the small multiply operand correctly. That avoids generic `i64` scaffolding.

Any attempt to narrow it to 32 bits must first prove cooker bounds for every
map and every transformed coordinate, then run exact face admission and visual
gates. Overflow-induced sign changes are missing-geometry bugs, not acceptable
performance trade-offs.

---

## 10. Current screenshots and visual evidence

- Corrected Cortex combined-disc frame:
  `/Users/ebonura/Desktop/repos/psx-demo-disc/dist/hardware-candidate/cortex-combined.png`
- Old invalid Cortex frame:
  `/private/tmp/psoxide-cortex-final.Vkkk3f/standalone-gameplay-pulses/display.png`
- Quake definitive visual:
  `/private/tmp/quake-visual-definitive-inclusive.png`
- Half-Life chain-load gameplay:
  `/private/tmp/hlpsx-chainload-gameplay/display.png`
- Half-Life final Hazard display:
  `/private/tmp/hl-final-hazard-gate/final-display.png`

The original Cortex frame was genuinely missing walls. The BSP geometry was
present, but the authored player/camera yaw put the third-person camera through
an unfinished leaking side of the level. One-sided wall faces were then
correctly backface-culled. A double-sided diagnostic restored them but was not
the right fix.

The accepted project fix rotates the starting yaw to 90 degrees. A new visual
release gate examines upper-playfield horizontal RGB edge density:

- old broken frame: 92 permille, rejected;
- corrected frame: 551 permille, accepted;
- required minimum: 250 permille.

The map still emits a real BSP leak from the active player-connected region.
The safe camera start prevents the launch screenshot from exiting through it,
but the map should still be sealed as level-authoring work.

---

## 11. How to continue: first actions for a new agent

### Phase 0: preserve evidence and re-establish a clean baseline

1. Read this document completely.
2. Read:
   - `/Users/ebonura/Desktop/repos/PSoXide/docs/playtest-profiling.md`
   - `/Users/ebonura/Desktop/repos/PSoXide/docs/perf-30fps.md`
   - `/Users/ebonura/Desktop/repos/hl-psx/docs/ps1-throughput-lessons.md`
   - `/Users/ebonura/.codex/skills/optimize-hl-psx/references/findings.md`
   - `/Users/ebonura/.codex/skills/optimize-hl-psx/references/tooling.md`
3. Record `git status --short --branch`, `git diff --stat`, HEAD, and a SHA-256
   of each live diff in all four repositories.
4. Do not clean the live trees.
5. Preserve the three clean snapshots and the 12,265-poll Half-Life tape
   somewhere durable before `/private/tmp` is pruned.
6. Verify the release receipt and rerun the five-entry combined gate.
7. Run the current standalone acceptance route for each game once before
   changing any source.
8. Only then create isolated baseline/candidate worktrees and private target,
   cook, guest-stage, and artifact directories.

### Phase 1: build one comparable low-level profile for each game

For each game capture, from the same final frontend:

- route log;
- GPU frame stats;
- CPU cycle classes;
- instruction classes;
- exact PC lines;
- exact I-cache events for the slow gameplay window;
- PC samples and call sites;
- rooted and full stack profile;
- display and full VRAM;
- guest/visual hashes where available;
- link map, EXE hash, CUE/BIN hash, source commit, diff hash, and cooked-data
  hash.

Do not compare an instruction histogram from one game to a stage profile from
another. Build a per-game table with the same denominators.

### Phase 2: choose the next target by measured upper bound

Rank candidates by:

1. share of the worst representative gameplay window;
2. cycles that can actually be removed, not merely moved;
3. RAM/code/stack cost;
4. visual and hardware risk;
5. how many games can use the exact implementation without an adapter tax.

Implement only the top candidate whose realistic saving can materially move
the target window.

---

## 12. Ranked next optimisation targets

These are hypotheses, not accepted work. Re-profile first because final code
layout changed.

### Target A: Cortex register-resident CPU-blended vertex kernel

Prior survey evidence identified the CPU-blended player/model path as the
largest Cortex instruction-level target: roughly 64% of player vertices and
about 95,000 of approximately 105,000 project-stage cycles in the older
capture. The failed batching route round-tripped through RAM.

The next experiment should keep the entire blend/project sequence in GPR/GTE
registers and use spare GTE matrix/color slots only within an audited phase.
It must not create a main-RAM staging array.

Gate:

- exact animation pose and projected-vertex results;
- exact model packets and GPU stream;
- exact display/VRAM across E1M1 and Cortex Ignition Tech Demo 0.1;
- MIPS disassembly proves fewer RAM loads/stores and no new GTE hazard;
- real-console hardware-test/capture before shipping.

### Target B: Quake collision trace kernel

Historical attribution placed trace work near a realistic 4-8% opportunity,
larger than Quake's measured I-cache layout noise. Flatten the exact hot hull
trace only after a final-route window confirms it.

Gate:

- bit-exact Quake hull, plane, fraction, and rounding results;
- Start plus all Episode 1 route probes;
- collision/contact trace corpus;
- no tunnelling, stair, liquid, trigger, or moving-brush regression;
- exact visuals and packet counts;
- code/I-cache change measured, not assumed.

### Target C: Quake alias-vertex packed-format audit

The alias RTPT loop historically assembled 3-byte vertices from byte loads.
Test a cooker/runtime representation that replaces byte assembly with aligned
word/halfword loads without increasing total resident model RAM.

Do not make the record larger unless another resident representation shrinks
by at least the same amount. Quake's batch scratchpad is already 1,020 / 1,024
bytes, so this is not a scratchpad proposal.

### Target D: Half-Life existing ordinary-quad assembly tightening

Do not build another additive cooked-surface system first. Start inside the
existing flat `__hlpsx_walk_ordinary_quads` path. The earlier survey identified
the guard/clamp chain as a possible local reduction from roughly 48 to roughly
12 instructions per ordinary quad.

Gate on the full 12,265-poll menu-compatible Hazard tape, including the slow
open-room windows, not only the train or one savestate. Preserve exact
GoldSrc/HL edge rounding and topology.

### Target E: replacement-only Half-Life draw-surface contract under 20 bytes

This is the next architectural target only if a concrete format beats the
storage gate before runtime implementation.

Requirements:

- average command below 20 bytes, or delete another authoritative resident
  record so net room size falls;
- no duplicated cell vertices;
- FaceRec/PVS/material/plane ownership is either retained without duplication
  or replaced by a provably smaller authoritative structure;
- brush-entity fallback is explicit;
- all 103 maps cook and the worst map gains RAM headroom;
- short vertical slice passes before a full route.

### Target F: cold first-frame PXBSP rebuild

Move or amortise Cortex node/PVS postorder construction into loading or before
the first presented gameplay frame. The goal is to retain the -29.9% steady
room gain while removing the +53.5% first-room spike.

This should be low visual risk but must preserve loading progress, scene entry,
and chain-load timing.

### Target G: cross-game manager standardisation

Prefer shared, efficient managers for repeated failure classes:

- release/source/artifact provenance;
- scene asset residency and replacement, including UI, world, model, sky,
  audio, and font lifetimes;
- packet arena reservation/high-water/drop reporting;
- stack and heap safety receipts;
- fatal exception capture;
- screen/outcode contracts;
- common low-level profiling schemas;
- cooker format versioning and source/runtime compatibility checks.

Do not standardise game-specific hot loops until an adapter-free A/B proves
neutral or faster in every consumer.

### Lower-priority measured ideas

- Cortex pad poll begin/finish overlap: at most about 1.5% total, hardware
  controller/memory-card gated.
- Cortex leaf-only scratchpad stack: plausible local 1-3%, but lifetime and
  exact stack-stall evidence missing.
- `$gp` small-data experiment: only after exact dynamic LUI/address counts and
  full relocation/startup audit.
- panic immediate abort in Quake/HL: capacity tool only after diagnostics and
  BREAK handling are accepted; not a performance target by itself.
- cache placement: only exact temporal pairs, never static occupancy.

---

## 13. Benchmark recipes

### 13.1 Cortex/E1M1 telemetry build

Use isolated source and generated roots. The historical exact build was:

```bash
cd /path/to/isolated/PSoXide
make build-editor-playtest \
  EDITOR_PLAYTEST_FEATURES="cd-stream-bench emulator-telemetry"
```

The historical pack recipe was:

```bash
cd tools/mkisopsx
cargo run --release -- \
  --exe ../../build/examples/mipsel-sony-psx/release/editor-playtest.exe \
  --out ../../build/examples/mipsel-sony-psx/release/editor-playtest.bin \
  --volume PSOXIDE \
  --cdtest-sectors 32 \
  --world-pack-rooms-dir ../../engine/examples/editor-playtest/generated/stream_chunks \
  --world-pack-order-file ../../engine/examples/editor-playtest/generated/world_pack_order.txt \
  --ui-pack-dir ../../engine/examples/editor-playtest/generated/ui_stream_chunks \
  --ui-pack-order-file ../../engine/examples/editor-playtest/generated/ui_pack_order.txt \
  --cdda-track-list ../../engine/examples/editor-playtest/generated/cdda_tracks.txt
```

Never omit the UI pack arguments. That creates an apparent streaming hang
because sectors requested by the runtime were never written.

Use `frontend launch` with the low-level flags from section 4.1, plus:

```text
--profile-log
--counter-log
--route-log
--gpu-frame-stats-log
--route-screenshot-dir
--route-screenshot-interval
--dump-display
--dump-vram
--dump-hash
--dump-guest-profile
```

On non-telemetry shipping builds, `--guest-frames` and
`--guest-visual-frames` do not stop the run. Use an instruction bound and
controller route, or rebuild with `emulator-telemetry`.

### 13.2 Quake gates

From a clean Quake tree whose declared PSoXide revision matches a clean
PSoXide checkout:

```bash
cd /path/to/clean/quake-psx

cargo run --release --manifest-path host/quake-build/Cargo.toml -- ship-boot
cargo run --release --manifest-path host/quake-build/Cargo.toml -- visual-parity-regress
cargo run --release --manifest-path host/quake-build/Cargo.toml -- regress
```

Also run the focused E1M1 benchmark/regression action appropriate to the
hypothesis, such as `e1m1-chain-bench`, and compare two deterministic runs.

Acceptance requires:

- shipping boot and heap floor;
- full stack margin;
- fixed-camera exact visual hash;
- full Episode 1 route;
- packet high water and zero overflow;
- deterministic E1M1 route/performance;
- exact source/artifact provenance.

### 13.3 Half-Life full route

Use the clean final CUE:

`/private/tmp/hl-psx-final-release-source/dist/hl-psx.cue`

and the current menu-compatible tape:

`/private/tmp/hlpsx-perf-audit.CxhqSI/hazard-current-menu.pxtape`

Example direct route:

```bash
OUT="$(mktemp -d /private/tmp/hl-hazard-next.XXXXXX)"
/private/tmp/psoxide-final-release-source/target/release/frontend launch \
  --path /private/tmp/hl-psx-final-release-source/dist/hl-psx.cue \
  --input-tape /private/tmp/hlpsx-perf-audit.CxhqSI/hazard-current-menu.pxtape \
  --steps 5910000000 \
  --route-log "$OUT/route.csv" \
  --gpu-frame-stats-log "$OUT/gpu.csv" \
  --cpu-cycle-profile-log "$OUT/cpu-cycle.csv" \
  --pc-sample-log "$OUT/pc.csv" \
  --pc-sample-callsite-log "$OUT/pc-callsite.csv" \
  --pc-sample-window-log "$OUT/pc-window.csv" \
  --pc-sample-window-ticks 300 \
  --route-screenshot-dir "$OUT/checkpoints" \
  --route-screenshot-interval 300 \
  --dump-display "$OUT/final-display.ppm" \
  --dump-vram "$OUT/final-vram.ppm" \
  --dump-hash
```

For low-level work add instruction classes, PC lines, I-cache events, and an
exact-root stack profile. Resolve every address against the candidate map.

The run must consume sample 12,264 and finish with a live gameplay PC. Compare
the worst representative gameplay windows, not menu/loading aggregate FPS.

### 13.4 Combined release gate

Once standalone gates pass and artifacts are repinned:

```bash
cd /Users/ebonura/Desktop/repos/psx-demo-disc
make release-headless-check
```

This target is intended to rebuild the private HL pressing and run the
receipt/chain-load verifier. It must fail closed on dirty sources, stale
stamps, or mismatched pins. Do not weaken that behaviour to accommodate a
dirty experiment.

---

## 14. Acceptance matrix for every new candidate

| Gate | Cortex | Quake | Half-Life | Combined disc |
|---|---|---|---|---|
| Clean source and exact diff hash | Required | Required | Required | Required |
| Fresh cook where format changed | Required | Required | Required; run assets before disc | N/A |
| Link/RAM report | Required | Required | Required | Receipt checks inputs |
| Full stack profile | Required | Required | Required | Target PC range |
| Packet high water and zero drops | Required | Required | Required | Route/GPU logs |
| Full representative route | Arena/E1M1 | Episode 1 | 12,265-poll Hazard | Five targets twice |
| Guest/semantic equality | Required | Probe state required | Required where available | Deterministic logs |
| GPU stream/count equality | Required unless explained | Required | Required unless correctness repair | Deterministic logs |
| Display and VRAM | Required | Required | Required | Final PPM deterministic |
| Difficult visual review | BSP edges, actors, HUD | world, water, sky, HUD | grates, water, actors, near edges, HUD | Cortex geometry gate |
| Original PS1 verification | Before shipping low-level GTE/DMA work | Before final release | Before final release | Final burn |

For a performance claim, report:

- exact baseline and candidate identities;
- target window and why it matters;
- cycles, instructions, CPI, I-cache, RAM loads/stores, and GTE stalls;
- rendered flips and route/bus denominators;
- GPU primitives and timing;
- RAM, text, data, BSS, heap, stack, and packet margins;
- visual/semantic equivalence evidence;
- whether the result is emulator-only or hardware-confirmed.

---

## 15. Real-hardware verification

The current combined image is ready for an owner-run hardware pass, not yet
hardware-approved.

For the TSSTcorp SN-208AB drive, the established audio-safe burn command is:

```bash
cdrdao write \
  --device 0,0,0 \
  --driver generic-mmc \
  --swap \
  --speed 10 \
  -n \
  --eject \
  "PSoXide Demo Disc HL.cue"
```

Do not use `generic-mmc` without `--swap`; it byteswaps CD-DA into noise. Do
not combine `--swap` with `generic-mmc-raw`.

Hardware pass checklist:

1. Cold boot launcher.
2. Launch Cortex, verify completed room geometry, Aletha, HUD, textures,
   animation, camera, and no panic.
3. Launch Quake, enter gameplay, check input, water/sky, world edges, actors,
   and sustained play.
4. Launch Half-Life, start the train/Hazard route, inspect the restored
   c1a4i actor-heavy case if reachable, grates, water, HUD, and loading.
5. Return to launcher between programs if supported; exercise at least two
   successive chain-loads.
6. Launch Hardware Tests and record the characterisation output.
7. Record CRT video from power-on and retain audio.
8. Treat any DMA, CD, GTE, GPU, or SPU difference from the emulator as a model
   finding. Do not tune the games to an emulator-only behaviour.

---

## 16. Paste-ready prompt for a brand-new optimisation agent

Copy everything inside the block below into a fresh task. Attach this document
or provide its absolute path.

```text
You are taking over low-level performance and release work across the PSoXide
PS1 ecosystem. Read this handoff completely before acting:

/Users/ebonura/Desktop/repos/PSoXide/docs/optimization-handoff-2026-08-23.md

Your goal is to improve Cortex Ignition, quake-psx, and hl-psx on the original
PlayStation with zero visual downgrade, while keeping each standalone disc and
the combined demo disc working. The combined image must remain fail-closed and
source/artifact-provenanced. Real PS1 hardware is final truth.

Critical state:

- The live repositories are dirty and contain user/concurrent work. Do not
  clean, reset, checkout, or commit mixed changes.
- The validated clean sources are temporary snapshots:
  - PSoXide/Cortex: /private/tmp/psoxide-final-release-source at 7b95d937...
  - Quake: /private/tmp/quake-final-release-clean/repo at da3023c...
  - Half-Life: /private/tmp/hl-psx-final-release-source at 9ab1c8d...
- The current combined candidate is:
  /Users/ebonura/Desktop/repos/psx-demo-disc/dist/hardware-candidate/PSoXide Demo Disc HL.cue
- Its CUE SHA is a8d2a7b80c66665418644b14b5d8c98110429979b49b25e78a96b2a32be44e42.
- Its BIN SHA is 17b751f3b3ce50cb35e00f1e32114326e82c6e287f4232d419aee991a4c1648b.
- Verify its release receipt before doing anything.
- The current Half-Life acceptance tape is the 12,265-poll PXITAPE2 file at
  /private/tmp/hlpsx-perf-audit.CxhqSI/hazard-current-menu.pxtape,
  SHA f814caa88918a862154d91a8c6e45682979ec52d95555760fd67d7d39532ff68.
  Preserve it before temp cleanup.

First actions:

1. Record status, HEAD, diff stats, and diff hashes for PSoXide, quake-psx,
   hl-psx, and psx-demo-disc. Preserve all WIP.
2. Verify the combined release receipt and rerun every combined target twice
   using tools/check_release_chainloads.py.
3. Rerun standalone frozen baselines for Cortex, Quake, and Half-Life.
4. Build one comparable low-level profile for each game using CPU-cycle,
   instruction-class, exact PC-line, exact I-cache-event, stack, route, GPU,
   semantic, display, and VRAM logs.
5. Resolve every hot PC against the exact matching link map/disassembly.
6. Rank opportunities by the worst representative gameplay window and a
   realistic removable-cycle upper bound.
7. Propose exactly one next candidate, its invariants, expected ceiling, RAM
   cost, and complete acceptance gate before editing.
8. Implement in an isolated candidate tree with private target/cook/stage
   roots. Do not contaminate the live trees or generated assets.
9. Run the complete game route, not a short scene only. Keep or fully revert
   the candidate based on measured evidence.

Known accepted work you must preserve:

- Cortex E1M1 visible-face list and Quake-style BSP node traversal.
- Cortex aligned Q11 decoder and scheduled zero-UV face walker.
- Inclusive 319/239 screen outcodes.
- External PXBSP face-chain lifetime overlay.
- Six-font-to-two-page VRAM manager with observable failure.
- Exact I-cache/instruction/cycle/stack profiler and fatal exception hard-stop.
- Quake 128 KiB packet arenas, 52 KiB stack, sector-safe Nodes transcode, and
  arena/heap/stack gates.
- Half-Life actor model packet stream plus conservative common screen reject
  and 37-slot RAM-neutral trim.
- Fail-closed demo-disc receipt and deterministic five-target chainload gate.

Do not retry unchanged:

- executable scratchpad;
- Cortex offset-layer face specialisation;
- Cortex persistent-PVS-chain scan without the selected frame list;
- Quake fused classic-affine writer;
- Quake 160 KiB arenas or 48 KiB stack;
- Half-Life exact I-cache placement based on the already measured pair;
- Half-Life duplicated-cell replacement or storage-neutral 20-byte command;
- additive Half-Life source-world pipeline;
- the rejected shared Half-Life projection/outcode adapter;
- coverage buffer or small GT4 cache;
- a generic shared hot renderer that changes a game's rounding or I-cache
  shape.

Prioritise after remeasurement:

1. Cortex register-resident CPU-blended vertex kernel.
2. Quake collision trace hot kernel.
3. Quake aligned alias-vertex representation if RAM-neutral.
4. Tighten the existing Half-Life ordinary-quad asm walker.
5. Replacement-only Half-Life draw records only if average storage is under
   20 bytes or another authoritative record disappears.
6. Move Cortex's cold first-PVS/node rebuild into load time.
7. Continue shared manager/provenance/profile work where it removes repeated
   failure classes without taxing hot loops.

Do not claim 30 fps or stable 20 fps from an aggregate average. Report slow
gameplay windows, exact denominators, packet/RAM safety, and visual equivalence.
Do not commit, push, repin, publish, or burn until the user explicitly asks and
all automated gates pass.
```

---

## 17. Final state statement

The present work is a solid foundation, not the end of the 30 fps effort:

- Cortex has real measured CPU wins and stronger memory management.
- Quake is again bootable and safe with exact visuals, but its latest round is
  performance-neutral.
- Half-Life no longer drops the actor-heavy packet stream, but its latest
  round is performance-neutral.
- The three programs and the hardware suite chain-load deterministically from
  the same combined image in the emulator.
- The release receipt and geometry gate prevent the specific stale-artifact
  and missing-Cortex-geometry failures that occurred during this session.
- The source changes still need clean integration into the normal repositories
  and pins.
- The combined candidate still needs a real-console burn and verification.

The next agent should begin from measurements, not from a promise to unify all
three renderers. The most reusable path is one shared, exact, efficient set of
resource/provenance/profiling/packet contracts with game-specific inner loops
when that is what R3000 I-cache locality and exact rounding require.

---

## 18. Addendum, 2026-08-23 (Phase 0 rerun)

Phase 0 steps 1-6 were rerun on the live machine:

- Live HEADs and diff SHA-256 for all four repositories matched section 2.1
  exactly. No staged changes.
- The release receipt verified (CUE/BIN/receipt SHA-256 unchanged).
- The five-target chain-load gate passed twice and its 50 evidence files were
  byte-identical to the original session's evidence.
- The section 2.2 reconciliation claims were confirmed (25 PSoXide files
  exact, HL `main.rs` exact, Quake differs only in the `host/quake-build/main.rs`
  pin line).

On the owner's explicit instruction, every auxiliary worktree and every
project-specific `/private/tmp` artifact was permanently removed after this
rerun. The duplicate `dist/release-evidence-2026-08-23` directory and the old
invalid Cortex burn candidate were also removed.

The only durable release material is now inside the canonical demo repository:

- corrected burn candidate:
  `/Users/ebonura/Desktop/repos/psx-demo-disc/dist/hardware-candidate/`;
- ten deterministic chain-load runs (50 files):
  `hardware-candidate/chainload-evidence/`;
- current 12,265-sample Hazard tape:
  `hardware-candidate/tapes/hazard-current-menu.pxtape`;
- release receipt and Quake provenance JSON beside the CUE/BIN.

All earlier `/private/tmp` paths in this document are historical evidence
labels only and must not be treated as existing files. There is now exactly one
registered worktree for each canonical repository: PSoXide, quake-psx,
hl-psx, and psx-demo-disc. Before the next release rebuild, convert the
validated live diffs into ordinary clean commits and regenerate any standalone
profiling artifact from those canonical repositories.

The section 3 evidence manifest hash `dd30cdbe...` could not be reproduced
from the 50 files by any obvious sorted-hash recipe; the recipe was never
recorded. Rely on the per-file byte identity between the two gate runs instead.
