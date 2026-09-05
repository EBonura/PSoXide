# `docs/` (design & reference notes)

Narrative documentation: architecture deep-dives, hardware references, and
planning/roadmap notes. For an overview of the codebase itself, start from
the [root README](../README.md) and the per-area READMEs
([`sdk/`](../sdk), [`engine/`](../engine), [`editor/`](../editor),
[`emu/`](../emu)).

## Start here

- [Build and contribute](../CONTRIBUTING.md)
- [Repository architecture and crate boundaries](repository-architecture.md)
- [SDK extraction proposal](sdk-separation.md)
- [Demo-disc consumer matrix and migration checks](demo-disc-dependencies.md)
- [Current repository review](repository-review-2026-09-05.md)
- [Cortex Ignition 0.4b](../editor/projects/cortex-ignition-tech-demo-0.4b/)

Dated investigations and acceptance reports describe the revision they tested.
They are historical evidence, not the current release status or setup guide.

## Architecture

| Doc | Topic |
|-----|-------|
| [editor-architecture.md](editor-architecture.md) | Editor internals. |
| [frontend.md](frontend.md) | Emulator frontend architecture. |
| [world-grid-architecture.md](world-grid-architecture.md) | Room/sector grid model. |
| [level-residency.md](level-residency.md) | Streamed-room residency runtime. |
| [demo10-low-level-hot-paths-2026-06-02.md](demo10-low-level-hot-paths-2026-06-02.md) | Demo10 guest-cycle baseline for low-level optimization work. |

## Authoring & runtime

| Doc | Topic |
|-----|-------|
| [editor-model-authoring.md](editor-model-authoring.md) | Model import/authoring workflow. |
| [editor-lighting.md](editor-lighting.md) | Lighting authoring. |
| [editor-runtime-coordinates.md](editor-runtime-coordinates.md) | Editor ↔ runtime coordinate spaces. |
| [playable-character.md](playable-character.md) | Playable-character model. |
| [floors-plan.md](floors-plan.md) | Vertical levels within a room. |
| [floors-editor-architecture.md](floors-editor-architecture.md) | Editor-side floors design. |
| [playtest-profiling.md](playtest-profiling.md) | Playtest capture, headless replay, per-vblank profiling. |
| [classic-affine-rendering-2026-08-08.md](classic-affine-rendering-2026-08-08.md) | Reusable camera-space subdivision and compact packet path for near-perspective-correct PS1 texturing. |
| [downstream-projects.md](downstream-projects.md) | Canonical structure for game repos built on PSoXide. |
| [game-states-plan.md](game-states-plan.md) | Game-state system plan. |
| [multi-gameplay-scenes.md](multi-gameplay-scenes.md) | Design note: more than one gameplay Scene in GameFlow. |
| [gte-camera-kit.md](gte-camera-kit.md) | Design note: shared GTE camera/projection kit. |
| [shared-engine-standardisation-2026-08-22.md](shared-engine-standardisation-2026-08-22.md) | Guarded convergence plan for PSoXide, Quake-PSX and HL-PSX. |

## Hardware reference

Per-subsystem behavioural notes backing the emulator, in [`hardware-refs/`](hardware-refs):
[gpu](hardware-refs/gpu.md), [spu](hardware-refs/spu.md), [dma](hardware-refs/dma.md),
[irq](hardware-refs/irq.md), [timers](hardware-refs/timers.md).

## Formats & roadmaps

| Doc | Topic |
|-----|-------|
| [world-format-roadmap.md](world-format-roadmap.md) | `.psxw` world format roadmap. |

## Validation & provenance

| Doc | Topic |
|-----|-------|
| [comicon-playable-beta-handoff-2026-08-13.md](comicon-playable-beta-handoff-2026-08-13.md) | Historical August playable-beta freeze and validation evidence. |
| [souls-slice-acceptance.md](souls-slice-acceptance.md) | Owner acceptance pass on the tracked souls slice. |
| [fresh-project-workflow-checklist.md](fresh-project-workflow-checklist.md) | Owner acceptance pass on a newly created project. |
| [asset-provenance.md](asset-provenance.md) | Asset and media provenance. |
| [license-audit.md](license-audit.md) | License and provenance audit. |
| [finalisation-log.md](finalisation-log.md) | Project finalisation log. |
