# Convergence adversarial audit: discrepancy report

Date: 2026-08-11. Auditor: the integration session that produced PSoXide
`codex/quake-psoxide-convergence` HEAD `c683ba1e` (code) plus the documentation
commits above it, and quake-psx `codex/quake-convergence` HEAD `2189bb3`.
Scope: the section-17 mandate of `docs/quake-psoxide-convergence-handoff.md`.
Each entry: claim, evidence checked, outcome, impact, corrective action, and
whether a milestone label was downgraded.

## 1. Handoff agent statuses (section 9)

Claim: reciprocal-pose combat was "about 15 percent" implemented; Draft/Release
Play still needed UI, New Project, and freshness wiring.

Evidence: the dirty worktree diff at `PSoXide-reciprocal-pose-combat` was about
95 percent complete (one compile error, zero tests); `603126dc` on
`codex/bsp-draft-release-play` already contained the UI, New Project, and
freshness wiring plus tests.

Outcome: contradicted (stale in the conservative direction).
Impact: planning only. Corrective action: statuses re-derived from worktrees
before merging; none of the section-9 numbers were trusted. No downgrade.

## 2. Authored attack windows (cortex weapon content)

Claim: the migrated player attack capsules and windows were "verified" content.

Evidence: the windows were authored for the legacy distance/angle arc, which
tests no vertical or blade geometry. A headless blade-trajectory probe against
the combat fixture enemy showed the light blade first crosses a target in
front around frames 13-14 while the authored window was 3-6 (the overhead
windup), so every capsule-resolved swing whiffed. Measured contact frames:
light 13-14, heavy 11-13, combo 12-14/18-22/24-25.

Outcome: contradicted for capsule-accurate combat; the data was faithfully
migrated but geometrically untested.
Impact: Level B was unreachable until corrected. Corrective action:
`711db669` retunes the windows to the measured strike sweeps (light 12-15,
heavy 11-14, combo 12-25) in the tracked sample; the fixture replay then
proved 4 single-connection hits, 1 poise-break stagger, and 1 death.
Downgrade: "verified sword/socket/capsule content" now reads "verified as
migrated data; strike windows measured and retuned on 2026-08-11".

## 3. Occlusion in combat volume tests

Claim (implicit in "a live BSP enemy blocks the player and is blocked by the
BSP world"): combat contact respects world geometry.

Evidence: in an early fixture replay whose tape never opened the door, the
enemy landed arc-fallback hits on the player through the closed door brush
(both actors pressed against opposite faces, gap under the arc reach). The
authored-capsule path also performs no world-occlusion test; only geometric
capsule overlap gates it.

Outcome: confirmed limitation, previously undocumented. The initial reading
that the enemy had walked through the closed mover was wrong; the NPC trace
provider composes the same static-world, transformed-mover, and cylinder
stack as the player (`bsp_runtime.rs::commit_body_step`), and the giant-scale
first fixture frames were misread.
Impact: melee can connect through thin occluders in both the legacy arc and
the capsule path. Corrective action: documented here and in the handoff;
fixing it is a design decision (occlusion trace per contact) left to the
owner. No milestone downgrade; movement blocking itself held.

## 4. Quake gate greenness (handoff section 8)

Claim: core, parity, cooker, tooling, MIPS, all-map, and a shipping capture
were previously green on the Quake convergence branch.

Evidence: rerunning from a fresh hydration of the pinned converged revision,
every gate passed except `audio-regress`, which failed with a silent shot.
Diagnosis from `--route-log port1_polls` and audio captures: the gate script
predated the attract-menu boot that `76cfd76` wired in. Three compounding
harness assumptions had rotted: the cold boot polls no pad until about route
tick 2,030 (the old fire press at 2,000 died on dead input); the shipping
image now needs a NEW GAME accept at all; and the NEW GAME reload keeps
polling dead until about tick 3,220, swallowing naive retimings. SDK and
emulator audio in the pin range were byte-identical (7 telemetry lines), and
the direct-boot ambient image played both the ambient loop and a probed R2
shot correctly, exonerating engine, SDK, emulator, and game audio code.

Outcome: the prior green audio claim was true only for the pre-menu image;
against the menu-era image it was stale. The gate itself, not the game, was
broken.
Impact: a false red that could have been misread as an SDK regression.
Corrective action: `4a72c47` retimes the gate around the measured poll
windows; it now passes through the real menu flow (accept 2,200, fire 3,400,
shot 38,940 active frames, silent tail). Downgrade: "previously green"
labels for the audio gate now carry the image-era caveat.

## 5. Cortex budget numbers (handoff section 12)

Claim: packet envelope 2,015 against 1,536, and a 54,096-byte MIPS/RAM
overflow.

Evidence: the tracked sample's cook reproduces envelope 2,015. The RAM
overflow does not reproduce for the tracked sample; its MIPS image links.
The 54,096-byte figure belonged to the full untracked donor project.

Outcome: packet claim confirmed; RAM claim clarified as donor-only.
Corrective action: `c683ba1e` derives the runtime packet arena per project
from the cooked envelope (floor 1,536, ceiling 4,096); cortex derives 2,048,
the warning is gone, and the link is the RAM proof. No visual change; brush
projects keep byte-identical output (same executable SHA-256 before and
after). No downgrade.

## 6. Baseline determinism and performance

Claim (sections 10.2/10.3): first-playable replay hashes and per-frame costs.

Evidence: at final code HEAD, the walk-through-door replay reproduces the
checkpoint hashes bit-identically (VRAM `0x5f0c6df40cf31729`, display
`0x9eef0b6f25eb5c11`) through five merges. Update cost 117,655 cycles/tick
(baseline 117,437, +0.19 percent), render 89,354 cycles/visual frame
(baseline 89,348), and the same single known deadline-miss event.

Outcome: confirmed. The `/tmp` replay evidence directories named in section
21 were lost to a disk-full event before this session; the bit-identical
reproduction at HEAD supersedes them.

## 7. Duplicate-authority sweep (17.2)

Evidence: `rg` sweeps at final HEAD. Render-time pose computation outside the
retained-snapshot module in editor-playtest: none. Legacy-arc callers:
exactly one enemy fallback site (gated on `FallbackRequired`) and one player
fallback site (gated on the authored action being absent), plus the crate
definitions and tests. Grid-vs-BSP selection: single presence-based
discriminator chain documented in `docs/legacy-grid-boundary.md`, which also
records the real duplications that remain by design (grid cook is a live
dependency of every BSP cook via the synthetic 1x1 grid room; NPC hull index
is hard-coded to 1; BoxProp/ArchProp AABBs and water are absent from the BSP
trace stack).

Outcome: no undocumented duplicate authorities found; the documented parity
gaps block any grid-retirement claim and are owner decisions.

## 8. Untested items carried forward

- The `i32::MIN` weapon-origin spawn transient (handoff 7.1) was not
  re-tested this session.
- No original-hardware battery ran; every result here is emulator evidence.
- The from-scratch editor workflow test by a non-implementer (section 13,
  item 18) requires the owner.
- Enemy authored attack volumes still do not exist (no Mantis attack clip);
  enemy damage remains the documented legacy-arc fallback by design.
