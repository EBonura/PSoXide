+++
title = "How PSoXide is built"
description = "The rules the AI coding agents work to, the checks every change has to pass, and where the provenance records live."
[extra]
eyebrow = "About PSoXide · Largely written with agentic coding"
+++

**PSoXide and its games are largely written with agentic coding.** I direct the AI coding agents. I decide what gets built, set the rules they work to and review their changes. Then I test the work in two places: PSoXide's emulator, which profiles every cycle, and a real PlayStation, which shows where the emulator is wrong.

## What I do

The agents don't pick the projects, the architecture or the priorities. I do, and I set the rules in the next section. Nothing lands without my review, commit messages included.

The emulator replays a bug the same way every time, which a console can't. When the two disagree, the console wins and I fix the emulator to match: its CD timing and its MDEC transfer speed both come from measurements on my own consoles.

## The rules the agents work to

- **Provenance.** Where behaviour comes from public hardware documentation, console measurements or another project, the provenance records say so, and known derivations are credited in the source. I don't claim that the whole project is clean-room. An AI model is trained on existing code, so AI-written code can carry influences that nobody can fully audit.
- **32-bit integer code on the console.** Code that runs on the PlayStation uses fixed-point arithmetic and bounded memory, because the console has no floating-point unit.
- **No visual trade-offs for speed without approval.** The agents don't cut draw distance, detail or effects to reach a frame rate unless I have seen before and after frames and agreed.
- **Plain commit messages.** A commit message describes the change. AI authorship is disclosed here, in each project's README and on each release page, and not in commit trailers.

## What every change has to pass

The checks differ by project. Each project runs some or all of these:

- **Emulator tests.** The emulator core has its own test suite, and it runs the public [ps1-tests](https://github.com/JaCzekanski/ps1-tests) cases (58 of 61 self-checking cases pass, and that number is a gate).
- **A 20-game compatibility run.** The emulator boots 20 commercial titles without a BIOS, and a change that alters the recorded display hashes needs an explanation. All 20 reach the in-game tier in headless runs. No person has played them through and rated them playable yet. The [comparison page](@/emulator/compare.md) lists the results in its compatibility section.
- **Frame, hash and state comparisons.** The same inputs replayed before and after a change must produce the same game state. Where a change is meant to alter the picture, I compare frames.
- **Comparison with the original game.** For ports and rebuilds, behaviour is checked against the original: every Celeste room and Celeste 2 level against the PICO-8 carts, Half-Life against the Xash3D engine as a behavioural reference, and WipEout against ship traces recorded from its own disc.
- **Build gates for the console's limits.** A stack-depth check proves that the scratchpad call trees fit, a link-map scan flags 64-bit helper calls, and a game that outgrows the console's RAM does not link.
- **Console tests.** Hardware tests run on my own consoles, with results captured and compared with the emulator. The [development methodology](@/docs/development-methodology.md) shows one measured example.

A passing check is evidence for the workload it ran. Some releases go out tested in the emulator only, and their changelogs say so.

## Where the provenance records live

- [Legal & licensing: provenance](@/legal.md#provenance) covers the whole project.
- SDK: [asset provenance](https://github.com/EBonura/PSoXide/blob/main/docs/asset-provenance.md), [downstream licensing](https://github.com/EBonura/PSoXide/blob/main/docs/downstream-licensing.md) and the [licence audit](https://github.com/EBonura/PSoXide/blob/main/docs/license-audit.md).
- Emulator: [PROVENANCE.md](https://github.com/EBonura/PSoXide-emulator/blob/main/docs/PROVENANCE.md) and the [HLE provenance record](https://github.com/EBonura/PSoXide-emulator/blob/main/docs/hle-bios-provenance.md).
- Games: [Quake](https://github.com/EBonura/quake-psx/blob/main/PROVENANCE.md), [Half-Life](https://github.com/EBonura/hl-psx/blob/main/PROVENANCE.md), [PSoXide Arcade](https://github.com/EBonura/psoxide-arcade/blob/main/THIRD_PARTY.md), [VoXide](https://github.com/EBonura/voxide/blob/main/assets/pack/CREDITS.md) and the editor's [asset provenance](https://github.com/EBonura/PSoXide-editor/blob/main/docs/asset-provenance.md) for Cortex Ignition.

## Corrections

If you think a claim here is wrong, a provenance record is missing or a game isn't doing what its page says, [open an issue](https://github.com/EBonura/PSoXide/issues). Say which page or file you mean and what you found. The [contact process](@/legal.md#contact) in the legal notice has the details.
