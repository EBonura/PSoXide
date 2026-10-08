+++
title = "About PSoXide"
description = "Why PSoXide keeps developing for the original PlayStation: new games, ports, efficient reimplementations, shared tools and documented provenance."
[extra]
eyebrow = "About PSoXide"
+++

## What PSoXide is for

**What if we never stopped developing for the original PlayStation?**

Late in a console's commercial life, developers have had years to learn its
hardware. I want to see what happens when that learning continues: new games,
shared tools and further experiments on the same machine.

I'm Manny, the developer behind Bonnie Studios. PSoXide has three strands:

- **Create:** brand-new PS1 games using modern software engineering practices.
- **Port:** games brought to the console that were never released on it.
- **Optimize:** existing PS1 games reimplemented by rewriting their original source
  for greater efficiency.

The Rust SDK, engine, editor and emulator support this work. Work that proves
useful in one project gets reused in the next: PSXcel's on-screen keyboard, for
example, is now the shared `psx-osk` SDK component.
[See the PSXcel development notes](https://github.com/EBonura/psxcel).

PSoXide builds on the work and knowledge shared by the PlayStation homebrew
community. The [projects page](@/projects/_index.md) records what you can play or
build today, alongside experiments that are still in development.

## Reimplementing existing games

Porting or rewriting a game does not remove the rights in its original code or
content. These projects require a suitable licence, permission or another
applicable legal basis. PSoXide’s own licence grants no rights to third-party
games. See [Legal & licensing](@/legal.md#ports-and-reimplementations).

The Optimize strand rewrites an existing PS1 game's source into a more
efficient implementation for the same hardware, to learn what a different
implementation can achieve. Changes to behaviour and presentation are recorded
alongside performance and memory measurements.

The first project in this strand, WipEout, is in progress: the 1995 PlayStation
game rebuilt for the same console, labelled "Rebuilt with PSoXide". Nothing is
released yet, and any result will need a comparison against the original on
equivalent scenes and hardware.

## How the work is developed

The tools support a repeatable cycle: build, run, inspect and test. Command-line
workflows and emulator debugging interfaces also let developers work with coding
assistants. The [development workflow](@/docs/development-workflow.md) explains how
to connect these steps. The [development methodology](@/docs/development-methodology.md)
shows how measurements from real consoles improve the emulator and SDK.

PSoXide is largely written with agentic coding. I direct the architecture, review
and integrate changes, and validate results with automated tests and
original-hardware checks. [How PSoXide is built](@/how-its-built.md) lists the
rules the agents work to and the checks every change passes. [The provenance disclosure](@/legal.md#provenance)
covers the project's use of AI assistance and how provenance is tracked.

- **Hardware testing.** Emulator checks complement testing on original hardware; they don't replace it. When the emulator and a real PlayStation disagree, the PlayStation is right.
- **Recorded measurements.** The [comparison page](@/emulator/compare.md) ties its figures to recorded builds, explains the limits of the benchmark and separates measured results from work still in progress.
- **Firmware and game data.** PSoXide doesn't need Sony's BIOS and can't load one. It doesn't distribute commercial disc images. Half-Life and Hollow Knight require your own game data; Quake uses the shareware episode's data. See [project-specific terms](@/legal.md#ports-built-from-other-people-s-games).
- **Source and provenance.** The SDK, emulator and editor source is public under GPL-2.0-or-later, with provenance written down. Some experiments have no public release yet.

## Licensing and rights

The [Legal & licensing page](@/legal.md) records the project’s licences,
distribution policies, provenance, hardware warning and rights-contact process.

<span id="how-it-s-built"></span>

[Development and provenance →](@/legal.md#provenance)

<span id="licensing-at-a-glance"></span>

[Licensing at a glance →](@/legal.md#code-and-licences)

<span id="the-short-version"></span>

[Licensing summary →](@/legal.md#code-and-licences)

<span id="no-bios"></span>

[No BIOS →](@/legal.md#no-bios)

<span id="code-and-licences"></span>

[Code and licences →](@/legal.md#code-and-licences)

<span id="commercial-games"></span>

[Commercial games →](@/legal.md#commercial-games)

<span id="ports-built-from-other-people-s-games"></span>

[Ports built from other people’s games →](@/legal.md#ports-built-from-other-people-s-games)

<span id="music-and-other-assets"></span>

[Music and other assets →](@/legal.md#music-and-other-assets)

<span id="trademarks"></span>

[Trademarks →](@/legal.md#trademarks)

<span id="contact"></span>

[Contact →](@/legal.md#contact)

<span id="not-legal-advice"></span>

[Legal information →](@/legal.md#warranty-and-liability)
