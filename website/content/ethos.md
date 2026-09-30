+++
title = "About and licensing"
description = "Why PSoXide keeps developing for the original PlayStation: new games, ports, efficient reimplementations, shared tools and documented provenance."
[extra]
eyebrow = "About PSoXide"
+++

## What PSoXide is for

**What if we never stopped developing for the original PlayStation?**

Late in a console's commercial life, developers have had years to learn its
hardware. PSoXide asks what happens when that learning continues: new games,
shared tools and further experiments on the same machine.

I'm Manny, the developer behind Bonnie Studios. PSoXide brings together three
pursuits:

- **Create:** brand-new PS1 games using modern software engineering practices.
- **Port:** games brought to the console that were never released on it.
- **Optimize:** existing PS1 games reimplemented by rewriting their original source
  for greater efficiency.

The Rust SDK, engine, editor and emulator support this work. A useful discovery
in one project can become a tool for the next: PSXcel's on-screen keyboard, for
example, became the shared `psx-osk` SDK component.
[See the PSXcel development notes](https://github.com/EBonura/psxcel).

PSoXide builds on the work and knowledge shared by the PlayStation homebrew
community. The [projects page](@/projects/_index.md) records what you can play or
build today, alongside experiments that are still in development.

## Reimplementing existing games

The Optimize strand explores rewriting an existing PS1 game's source into a more
efficient implementation for the same hardware. The aim is to learn what a
different implementation can achieve, with changes to behaviour and presentation
recorded alongside performance and memory measurements.

This is a development direction. There is no released example of this work listed
on the site yet. Any future result needs a comparison against the original
implementation on equivalent scenes and hardware.

## How the work is developed

The tools support a repeatable cycle: build, run, inspect and test. Command-line
workflows and emulator debugging interfaces also let developers work with coding
assistants. The [development workflow](@/docs/development-workflow.md) explains how
to connect these steps.

I direct the architecture, review and integrate changes, and validate results
with automated tests and original-hardware checks. Read [how it is built](#how-it-s-built)
for the project's use of AI assistance and how provenance is tracked.

The project uses the following practices:

- **Hardware testing.** Emulator checks complement testing on original hardware; they don't replace it. When the emulator and a real PlayStation disagree, the PlayStation is right.
- **Recorded measurements.** The [comparison page](@/emulator/compare.md) ties its figures to recorded builds, explains the limits of the benchmark and separates measured results from work still in progress.
- **No firmware, no game data.** PSoXide doesn't need Sony's BIOS and can't load one. It does not distribute commercial disc images. Half-Life, Counter-Strike and Hollow Knight require your own game data; Quake uses the separately licensed shareware episode.
- **Source and provenance.** The SDK, emulator and editor source is public under GPL-2.0-or-later, with provenance written down. Some experiments have no public release yet.

## How it's built

PSoXide was developed with heavy use of AI coding assistants. A person directs the architecture, debugging and hardware verification; a large part of the code was written by an assistant under that direction, then reviewed and integrated.

That isn't a clean-room process. Language models are trained on large amounts of existing code, so code they write can carry influence from that training data that neither the tool nor the author can fully audit. Saying so is a disclosure, not a guarantee of clean-room provenance or non-infringement. The project's answer is to track provenance explicitly, file by file, as described in [downstream-licensing.md](https://github.com/EBonura/PSoXide/blob/main/docs/downstream-licensing.md) and in the sections below.

## Licensing at a glance

- PSoXide contains no Sony code or data and doesn't load a BIOS. Its own kernel boots discs.
- No commercial game, disc image or game data is distributed by the project, and the browser player includes only the public demo disc.
- Half-Life, Counter-Strike and Hollow Knight ports are source-only or private. You build them from a copy of the game you own.
- The Quake port ships the shareware episode together with its licence, which restricts screenshots, so this site shows none.
- Celeste Classic Collection is an unofficial fan port, credited to its creators.
- Music by Just Music and by magikAAAAArp is used with permission, for non-commercial releases only.
- The code is licensed GPL-2.0-or-later (the Quake port GPL-2.0-only).
- Much of the code was written with AI coding assistants under human direction. That isn't a clean-room process, so provenance is tracked file by file.

## No BIOS

DuckStation and Mednafen want a BIOS dumped from a real console, and PCSX-Redux offers its own open-source OpenBIOS as an alternative. PSoXide replaces the BIOS with a high-level emulation (HLE) of the kernel services games call, written in Rust. It boots commercial discs and homebrew alike, and since 25 September 2026 it has no way to load a BIOS at all: the settings, environment variable, command-line options and browser upload that used to take one have been removed.

The kernel is written from:

- public documentation (the facts in nocash's psx-spx, not its prose);
- the MIT-licensed OpenBIOS from PCSX-Redux, used as a specification rather than ported line by line;
- black-box observation of a real BIOS: register and memory values, call arguments, return values and timing.

The repository holds no Sony code or data: no BIOS bytes, disassembly, fonts, logos, boot sounds or kernel memory images, test fixtures included. Where each piece of behaviour came from is recorded in [hle-bios-provenance.md](https://github.com/EBonura/PSoXide-emulator/blob/main/docs/hle-bios-provenance.md), and the removal was audited in [firmware-cleanup.md](https://github.com/EBonura/PSoXide-emulator/blob/main/docs/firmware-cleanup.md).


## Code and licences

The SDK, emulator, editor and engine are licensed **GPL-2.0-or-later**, and so are most of the games. The Quake port is GPL-2.0-only, because it derives from id Software's GPL Quake source and the earlier QuakePSX port, both credited.

Parts of the emulator core are derived from PCSX-Redux (GPL-2.0-or-later), among them the event scheduler, DMA behaviour, SPU envelope tables and the CD-ROM command timing. Those files carry a provenance header naming PCSX-Redux and marking the points of correspondence. Other parts, such as the GTE, were written from hardware documentation and checked against a real console. Mednafen, DuckStation, the ps1-tests suite and the MiSTer core were used only as behavioural references or test tools; no code comes from them. The full picture is in [downstream-licensing.md](https://github.com/EBonura/PSoXide/blob/main/docs/downstream-licensing.md).

If you build a game on PSoXide and distribute it, the GPL applies to the PSoXide code you ship and to your changes to it. Your own art, music, levels and writing stay yours.


## Commercial games

PSoXide plays discs; it doesn't come with any. The project doesn't distribute commercial games, disc images or data taken from them, and the browser player on itch.io carries only the public PSoXide Demo Disc. The emulator's compatibility list names the games it was tested with and records how far each got. The games and any screenshots of them belong to their owners.


## Ports built from other people's games

Some PSoXide projects start from a game that belongs to someone else. Their distribution differs: some require your own installation, while Quake uses the shareware episode.

**Half-Life and Counter-Strike.** Noncommercial, source-only compatibility projects. The builder reads maps, models, textures, sounds and music from your own lawfully obtained installation and writes the converted data to local, ignored folders. No Valve assets and no disc images are distributed, and generated images must not be uploaded as releases. The Half-Life code was informed by Valve's public Half-Life SDK; Counter-Strike's gameplay numbers come from ReGameDLL_CS (MIT), with no code copied. Neither is affiliated with or endorsed by Valve. See [LICENSING.md](https://github.com/EBonura/hl-psx/blob/main/LICENSING.md).

**Hollow Knight.** An experiment that cooks rooms, sprites, text and audio from a local install. Those files belong to their owners, the project's licence gives no rights to them or to their converted form, and footage is public on Bonnie Studios' YouTube channel, but there is no public build.

**Quake.** The engine code is GPL. The game data is the Quake shareware episode, which comes with its own licence, SLICNSE.TXT. Every package that carries the Quake data includes that file, verbatim. The licence also restricts screenshots and other public display of the game, so this site doesn't show any.


**Celeste Classic Collection.** An unofficial, non-commercial fan port of the two PICO-8 games, Celeste (2016) and Celeste 2: Lani's Trek (2021) by Maddy Thorson and Noel Berry, with Celeste 2's music by Lena Raine. The levels, art and music are theirs. PICO-8 is by Lexaloffle Games. The port isn't affiliated with or endorsed by any of them.


## Music and other assets

- **Just Music:** four tracks, used in NitroXide and on the demo disc menu with the artist's permission. Non-commercial; don't redistribute the audio on its own.
- **Goncharov by magikAAAAArp:** used in GH-PSX and Magikarp Pong with the band's permission, and so is their album art.
- **Carmelo Miceli:** the Cortex Ignition menu and combat music, written for the game and used with the composer's permission.
- **Models, textures, sounds and fonts:** each has its source and licence recorded, for example the Torment Textures pack (with attribution) and the VT323 font (SIL Open Font License) in the SDK's [asset-provenance.md](https://github.com/EBonura/PSoXide/blob/main/docs/asset-provenance.md), and Kenney's CC0 interface sounds in the Celeste credits.


## Trademarks

PlayStation is a trademark of Sony Interactive Entertainment Inc. Half-Life and Counter-Strike are trademarks of Valve Corporation. Other game and product names belong to their owners and appear only to describe what a project is or what it's compatible with. None of these companies is affiliated with PSoXide or has endorsed it.

## Contact

If you own something that appears here and think it shouldn't, open an issue at [github.com/EBonura/PSoXide/issues](https://github.com/EBonura/PSoXide/issues) and it will be looked at.


## Not legal advice

This page explains how the project is licensed and what it does and doesn't distribute. It isn't legal advice and creates no warranty. For a decision about a specific use, especially a commercial one, ask a qualified lawyer.
