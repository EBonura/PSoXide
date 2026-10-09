+++
title = "Legal & licensing"
description = "Code licences, game-content rights, distribution policies, provenance and running burned discs on original hardware."
[extra]
eyebrow = "Project information"
+++

Last reviewed: **30 September 2026**.

PSoXide is an independent PlayStation development project. This page explains the
licences covering its code, the separate rights affecting game content, and what
each project distributes. Technical compatibility and public availability do not
establish permission to copy, adapt or redistribute third-party material.

## Code and licences

The SDK, emulator, editor and engine are released under **GPL-2.0-or-later**.
The Quake port is **GPL-2.0-only**. File-level notices and third-party licences
remain applicable; a repository’s top-level licence does not grant rights that
its contributors do not hold. Project-specific restrictions on third-party
content are separate from the GPL permissions for code.

- [SDK licence](https://github.com/EBonura/PSoXide/blob/main/LICENSE)
- [Emulator licence](https://github.com/EBonura/PSoXide-emulator/blob/main/LICENSE)
- [Editor and engine licence](https://github.com/EBonura/PSoXide-editor/blob/main/LICENSE)
- [Quake port licence](https://github.com/EBonura/quake-psx/blob/main/LICENSE)

Using a PSoXide tool does not automatically place everything it produces under
the GPL. However, distributing a program that incorporates or links PSoXide’s
GPL-covered runtime or SDK generally requires licensing the combined program
consistently with the GPL and providing its complete corresponding source,
including required build material. This is not limited to changes to PSoXide itself.

Separately licensed art, music and other independent content retain their own
terms. Permission to use assets bundled with a demonstration does not automatically
extend to your own release. The GPL permits commercial distribution of covered
code, subject to its conditions; that does not override restrictions on bundled
music, game data or other third-party material.

See the [GNU GPL FAQ](https://www.gnu.org/licenses/gpl-faq.html.en) for the
distinction between tool output, combined programs and independent content.

## Source for distributed builds

Source must correspond to the binary being distributed, including modifications
and the build scripts required by the applicable licence. A link to a moving
`main` branch alone does not identify that source. This applies to browser
WebAssembly downloads as well as native executables and code on disc images.

The [release source references](@/docs/release-sources.md) identify verified v0.40
revisions and outstanding source-availability checks.

The [demo-disc source repository](https://github.com/EBonura/PSoXide-demo-disc)
and [emulator source repository](https://github.com/EBonura/PSoXide-emulator)
contain the build tooling. Check the release’s source revision and component
pins, rather than assuming that today’s source matches an older download. If a
release does not identify its corresponding source, [report the release URL and
version](#contact) so the missing information can be addressed.

## Ports and reimplementations

Porting or rewriting a game does not remove the rights in its original code or
content. These projects require a suitable licence, permission or another
applicable legal basis. PSoXide’s own licence grants no rights to third-party games.

Requiring your own lawfully obtained copy avoids supplying the original game
data. It does not, by itself, grant permission to adapt or redistribute that data,
including converted files and completed disc images. Source availability,
noncommercial status, attribution and a rights holder’s silence do not by
themselves establish permission either.

Each project must distinguish its own code, source-informed adaptations and game
assets. Rewriting code in another language is not, by itself, evidence of an
independent implementation. See the UK Intellectual Property Office’s
[guidance on copyright permissions](https://www.gov.uk/using-somebody-elses-intellectual-property/copyright).

## Provenance

Development uses AI coding agents under human direction, review and testing.
The agents wrote nearly all of the code; [how PSoXide is built](@/how-its-built.md)
lists the rules they work to and the checks each change passes. Known upstream
derivations are credited in the source and provenance records. PSoXide does not
claim a clean-room development process or guarantee that provenance reviews
identify every third-party influence.

The emulator core's event scheduler, DMA, SIO0, SPU (voices, envelopes, noise
and reverb) and MDEC were rewritten from public hardware documentation (the
nocash PSX-SPX notes), console measurements and the public ps1-tests captures.
The high-level kernel emulation was written the same way (see
[No BIOS](#no-bios)). Some behaviour in the CPU, bus, video timing, GPU and
CD-ROM modules was first chosen to match PCSX-Redux (GPL-2.0-or-later) traces.
The source text is the project's own, but no document or console measurement
backs that behaviour. Each case is marked `gate-pinned` in the code and
listed in the emulator's
[provenance record](https://github.com/EBonura/PSoXide-emulator/blob/main/docs/PROVENANCE.md),
together with the few CD-ROM delay values still pinned to earlier choices. The
hardware renderer's texture filter is original work; two earlier filters that
were ports of third-party shader code have been removed.

Behavioural comparisons and similarity scans are evidence with a
defined scope, not proof of the absence of all derivation.

See the [licence audit](https://github.com/EBonura/PSoXide/blob/main/docs/license-audit.md)
and [HLE provenance record](https://github.com/EBonura/PSoXide-emulator/blob/main/docs/hle-bios-provenance.md).
Historical audit results describe the revisions and files they examined.

## No BIOS

The current PSoXide emulator implements kernel services through its own Rust
high-level emulation (HLE). It does not require or load a Sony BIOS image; the
external BIOS-loading paths were removed in September 2026. The SDK targets the
hardware without requiring Sony’s proprietary SDK.

The HLE provenance record says the kernel is written from public hardware
documentation (the nocash psx-spx notes) and from measurements: which kernel
functions games call, and timings taken on a console. It states that other kernel
implementations and SDKs (OpenBIOS, PCSX-Redux, nugget, psyqo and PSn00bSDK) are
not read while the kernel is written and that nothing is translated from them, and
that code once derived from one of them was deleted and written again in October
2026; the git history shows those rewrites as commits that start with "Rewrite".
The record's policy excludes
Sony BIOS bytes, disassembly, fonts, logos, boot sounds and kernel memory images
from committed implementation and fixtures. This describes the current
implementation and recorded checks, not an unlimited guarantee about every
historical copy, fork or artifact.

The [firmware-cleanup record](https://github.com/EBonura/PSoXide-emulator/blob/main/docs/firmware-cleanup.md)
documents the removal work. BIOS-free operation does not settle separate rights
in games, assets or console modifications.

## Commercial games

The emulator can open user-supplied software. The project does not distribute
retail commercial disc images; its browser player offers the public PSoXide Demo
Disc. That compilation includes third-party content with separate terms, so it
should not be treated as wholly project-owned or wholly GPL-licensed content.

Compatibility listings describe testing, not endorsement or permission to obtain
or share a game. Screenshots and video likewise do not grant rights in the works
shown. Use only material you are entitled to use for the intended purpose.

## Ports built from other people's games

### Half-Life

HL-PSX is an unofficial, noncommercial, source-only compatibility project. Its
Rust implementation was informed by Valve’s public Half-Life SDK, and its
provenance record identifies adaptations from GPL-licensed Quake code. Users
provide their own lawfully obtained Half-Life installation; project releases do
not include Valve game assets, converted asset packs or completed disc images.

Valve’s [Half-Life SDK licence](https://github.com/ValveSoftware/halflife/blob/master/LICENSE)
has its own scope and conditions. HL-PSX runs on a separate runtime; the project’s
distribution policy does not establish that Valve’s terms authorize every aspect
of this work. See [HL-PSX licensing](https://github.com/EBonura/hl-psx/blob/main/LICENSING.md)
and [source provenance](https://github.com/EBonura/hl-psx/blob/main/PROVENANCE.md).

### Hollow Knight

 An experiment that cooks rooms, sprites, text and audio from a local install. Those files belong to their owners, the project's licence gives no rights to them or to their converted form, and footage is public on Bonnie Studios' YouTube channel, but there is no public build.

### Quake

The port's engine code is GPL-2.0-only. Its current disc image includes converted
Quake shareware data and SLICNSE.TXT. That data has separate terms covering
modification, derivative works and distribution. See the [bundled shareware licence](https://github.com/EBonura/quake-psx/blob/main/release/SLICNSE.TXT)
and [port provenance](https://github.com/EBonura/quake-psx/blob/main/PROVENANCE.md).


### Celeste Classic Collection

 An unofficial, non-commercial fan port of the two PICO-8 games, Celeste (2016) and Celeste 2: Lani's Trek (2021) by Maddy Thorson and Noel Berry, with Celeste 2's music by Lena Raine. The levels, art and music are theirs. PICO-8 is by Lexaloffle Games. The port isn't affiliated with or endorsed by any of them. Its code licence does not grant rights in the original games’ assets.


## Music and other assets

- **Just Music:** four tracks, used in NitroXide and on the demo disc menu with the artist's permission. Non-commercial; don't redistribute the audio on its own.
- **Goncharov by magikAAAAArp:** used in Magikaaaaarp Pong with the band's permission, and so is their album art.
- **Carmelo Miceli:** the Cortex Ignition menu and combat music, written for the game and used with the composer's permission.
- **Models, textures, sounds and fonts:** sources and licences are tracked in project records, including unresolved provenance items. Examples include the Torment Textures pack (with attribution) and the VT323 font (SIL Open Font License) in the SDK's [asset-provenance.md](https://github.com/EBonura/PSoXide/blob/main/docs/asset-provenance.md), and Kenney's CC0 interface sounds in the Celeste credits.



## Running burned discs on original hardware

A standard, unmodified retail PlayStation cannot directly boot a burned PSoXide
disc. As with other burned PS1 discs, you need a modchipped console or another
compatible homebrew boot method.

PSoXide does not endorse console modchipping or provide instructions or assistance
for installing modchips. Modifying a console can permanently damage it or render
it unusable (“brick” it). Any modification is undertaken at your own risk.

This is a hardware requirement and risk notice, not permission to modify hardware
or bypass protections. Applicable law and third-party rights remain relevant.
The [browser player](https://bonnie-studios.itch.io/psoxide) requires no console
modification.

## Trademarks

PlayStation is a trademark of Sony Interactive Entertainment Inc. Half-Life is a
trademark of Valve Corporation. Other game and product names
belong to their respective owners and identify the works or compatibility being
discussed. PSoXide is not affiliated with, sponsored by or endorsed by Sony, Valve
or the other rights holders named here.

## Warranty and liability

Software is provided under its applicable licence, including that licence’s
warranty disclaimers and limitations of liability. This website does not add a
warranty or grant rights in third-party material. Nothing here excludes rights
or liabilities that applicable law does not allow to be excluded.

This page explains project policies and documented terms. It is not legal advice
or legal clearance for a particular use. For a decision about your own project,
especially commercial distribution or third-party adaptations, consult a qualified
IP lawyer.

## Contact

For a rights, attribution, licensing or source-availability concern, open an issue
at [EBonura/PSoXide](https://github.com/EBonura/PSoXide/issues). Identify the affected
page, file or release, the work concerned, and the basis of your concern. Where
relevant, include the applicable licence or a public source for the attribution.

GitHub issues are public. Do not post confidential correspondence, private personal
information, BIOS files or game assets. If supporting evidence is confidential,
state that a private follow-up is needed without posting the evidence itself.
