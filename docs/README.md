# `docs/` — design & reference notes

Narrative documentation: architecture deep-dives, hardware references, and
planning/roadmap notes. For an overview of the codebase itself, start from
the [root README](../README.md) and the per-area READMEs
([`sdk/`](../sdk), [`engine/`](../engine), [`editor/`](../editor),
[`emu/`](../emu)).

## Architecture

| Doc | Topic |
|-----|-------|
| [editor-architecture.md](editor-architecture.md) | Editor internals. |
| [engine-rearchitecture.md](engine-rearchitecture.md) | Engine design direction. |
| [frontend.md](frontend.md) | Emulator frontend architecture. |
| [world-grid-architecture.md](world-grid-architecture.md) | Room/sector grid model. |
| [level-residency.md](level-residency.md) | Streamed-room residency runtime. |
| [architecture-cleanup-roadmap.md](architecture-cleanup-roadmap.md) | In-progress structural cleanup. |

## Authoring & runtime

| Doc | Topic |
|-----|-------|
| [editor-model-authoring.md](editor-model-authoring.md) | Model import/authoring workflow. |
| [editor-lighting.md](editor-lighting.md) | Lighting authoring. |
| [editor-runtime-coordinates.md](editor-runtime-coordinates.md) | Editor ↔ runtime coordinate spaces. |
| [playable-character.md](playable-character.md) | Playable-character model. |
| [floors-plan.md](floors-plan.md) | Vertical levels within a room. |
| [vertical-rooms-investigation.md](vertical-rooms-investigation.md), [vertical-rooms-vr2-plan.md](vertical-rooms-vr2-plan.md) | Stacked-room support. |
| [game-states-plan.md](game-states-plan.md) | Game-state system plan. |

## Hardware reference

Per-subsystem behavioural notes backing the emulator: [`hardware-refs/`](hardware-refs) —
[gpu](hardware-refs/gpu.md), [spu](hardware-refs/spu.md), [dma](hardware-refs/dma.md),
[irq](hardware-refs/irq.md), [timers](hardware-refs/timers.md).

## Formats & roadmaps

| Doc | Topic |
|-----|-------|
| [world-format-roadmap.md](world-format-roadmap.md) | `.psxw` world format roadmap. |
| [milestones.md](milestones.md) | Milestone ladder. |

## Validation & provenance

| Doc | Topic |
|-----|-------|
| [redux-oracle.md](redux-oracle.md) | PCSX-Redux parity oracle. |
| [commercial-parity-tracker.md](commercial-parity-tracker.md) | Retail-disc compatibility status. |
| [asset-provenance.md](asset-provenance.md) | Asset and media provenance. |
| [license-audit.md](license-audit.md) | License and provenance audit. |
| [finalisation-log.md](finalisation-log.md) | Project finalisation log. |
