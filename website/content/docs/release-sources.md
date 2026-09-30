+++
title = "Release source references"
description = "Verified source revisions for the public Demo Disc v0.40, with source-availability checks and their limits."
weight = 30
[extra]
kind = "Reference"
+++

Checked on **30 September 2026**. These references come from the public Demo Disc
v0.40 component receipt, matched to BIN SHA-256
`d1e16f5deca68351b8361ab61bc7db90298497dc93817e72f8d7fe5be6657940`.
They identify recorded source revisions, not a certification that every release
package satisfies every licence requirement.

## Demo Disc v0.40

| Component | Recorded source |
| --- | --- |
| Disc build | [a8cf0a8244](https://github.com/EBonura/PSoXide-demo-disc/tree/a8cf0a824469d7d6a9a7f78198e8953748f64d37) |
| Launcher editor component | [6eebb2d866](https://github.com/EBonura/PSoXide-editor/tree/6eebb2d8667cfa9a5b65cb7aeeeabcff189fe78e) |
| Build-time emulator component | [035bf6f31f](https://github.com/EBonura/PSoXide-emulator/tree/035bf6f31faba1b920b334f71d403627480f832a) |
| Launcher SDK component | [6da88d92de](https://github.com/EBonura/PSoXide/tree/6da88d92de183cb5fd092be181bdbc5b81868480) |
| Celeste Classic Collection | [76bb26b877](https://github.com/EBonura/celeste-collection-psx/tree/76bb26b8779f2df2c91b9d8dfeec05f91f7e829b) |
| Cortex Ignition | [1bb7a44c2c](https://github.com/EBonura/PSoXide-editor/tree/1bb7a44c2c40d8811a6a6613f610bff6aa24fab6) |
| NitroXide | [4eaf4c0f37](https://github.com/EBonura/nitroxide/tree/4eaf4c0f37b6db6c26c2ffb7d4afd36a66c60d9d) |
| PSXcel | [c08096c14d](https://github.com/EBonura/psxcel/tree/c08096c14dfc23ea29fc561c5fda4f7073132f9f) |
| Quake port | [c58e2dfdfe](https://github.com/EBonura/quake-psx/tree/c58e2dfdfe2f8774ba6b0d1ba9151205df39e553) |
| VoXide | [a5d408b6d4](https://github.com/EBonura/voxide/tree/a5d408b6d4c3b03e5615aa4dbd587ae3ddd7f270) |

The [disc build recipe](https://github.com/EBonura/PSoXide-demo-disc/blob/a8cf0a824469d7d6a9a7f78198e8953748f64d37/release/lineup-v0.40.json)
records the individual program builds. The launcher’s component pins and a game’s
own pins can differ; use the recipe and that program’s build instructions together.

The recorded GH-PSX and PSoXide Arcade repository revisions were not anonymously
accessible during this check. A separate corresponding-source package for those
entries has not been verified. This is an outstanding source-availability check;
the table above is not a complete source package for the disc.

## Browser player and other downloads

The browser player is deployed separately. Its current WebAssembly artifact has
not been matched to an exact source revision in this website review. The emulator
revision above belongs to the disc’s build receipt; it must not be taken as the
revision of the live browser player. Standalone game downloads also need their
own release-to-source mapping.

Use the release’s matching source and build instructions, not an arbitrary current
branch. [Report a missing source reference](@/legal.md#contact) with the download
URL and version. See [Legal & licensing](@/legal.md#source-for-distributed-builds)
for the distinction between source availability and third-party content rights.
