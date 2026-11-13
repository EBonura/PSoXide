# `crates/` (shared PSX primitives)

Workspace-shared, `no_std`-compatible building blocks consumed across the
emulator, SDK, and host tooling. These have no UI and no target-specific
runtime; they are the lowest common layer the rest of the repo builds on.

| Crate | Purpose |
|-------|---------|
| [`psx-anim-cook`](psx-anim-cook) | Host-side HMA1 animation encoder: local-space quaternion tracks with per-bone key rates, chosen against a vertex-space error bound. Decoded on the PS1 by `psx_asset::hma1`. |
| [`psx-anim-tracks`](psx-anim-tracks) | Host encoder for keyed animation tracks, fitted against the runtime decoder and a vertex-space error bound. |
| [`psx-audio-cook`](psx-audio-cook) | Host-side SPU-ADPCM cooker for every game: band-limited resampling, Gaussian-interpolation pre-compensation, a trellis encoder evaluated against the exact SPU decoder, seamless block-aligned loops, and a distortion-aware rate allocator for SPU RAM budgets. Also encodes CD-ROM XA-ADPCM music (`xa-encode`, interleaved channels per file). |
| [`psx-seq-cook`](psx-seq-cook) | Host-side cook for `psx-seq` music: imports N64 libultra sequence and soundfont data from a ROM the player owns, converts samples to SPU-ADPCM, writes the song and bank containers, and renders a cooked song to WAV. |
| [`psx-disasm`](psx-disasm) | R3000 (MIPS I) decoder that prints GNU objdump's `mips:3000` syntax, so the post-link hazard checks (`tools/psoxide-hazard`) read an image without objdump. |
| [`psx-hw`](psx-hw) | PlayStation 1 hardware model: register addresses, bitfield layouts, and command packet formats. Shared by emulator and SDK. |
| [`psx-gate`](psx-gate) | The architecture gate's shared parts: the `sdk/profile.toml` reader, the feature check every SDK crate's build script runs, and the receipt check the disc builders run (sdk/docs/GATE.md). |
| [`psx-iso`](psx-iso) | BIN/CUE and ISO9660 parsing for PS1 disc images. Shared by emulator, SDK, and disc-builder. |
| [`psx-library`](psx-library) | Host-side install of a built disc into the games library: `Game Name (repo)` naming, atomic replace with digest check, safe removal of stale flat-named copies and a provenance record. Used by every game's build tool. |
| [`psx-trace`](psx-trace) | Instruction trace record format emitted by the emulator core (originally shared with the retired PCSX-Redux parity oracle). |
| [`psxed-format`](psxed-format) | Neutral cooked-asset formats shared by the SDK, editor cookers and engine. |
| [`psxed`](psxed) | Content-pipeline CLI: cooks OBJ meshes, textures and audio into the blobs the runtime embeds (`make assets`). |
| [`psxed-audio`](psxed-audio) | Host-side WAV to SPU ADPCM audio cooker used by the editor and by game cook tools. |
| [`psxed-gltf`](psxed-gltf) | glTF/GLB/FBX mesh importer that cooks scene meshes into PSXM and model blobs. |
| [`psxed-obj`](psxed-obj) | OBJ parser and vertex-cluster decimator emitting PSXM blobs. |
| [`psxed-project`](psxed-project) | The editor project model (scenes, nodes, resources, grid worlds) with the grid edit verbs and the cook that turns a project into the playtest runtime package (`cook-playtest`). Its tests read the editor repository's `projects/` and `assets/`, so they run there. |
| [`psxed-tex`](psxed-tex) | PNG/JPG to PSXT texture cooker: crop, resample, 4/8-bit CLUT quantisation, VRAM packing. |

These crates are members of the SDK repo-root host workspace alongside its
host tools. The device-side `sdk/` keeps its own workspace. The engine
(`engine/`, its own workspace) lives here too. The editor UI and the projects
live in [PSoXide-editor](https://github.com/EBonura/PSoXide-editor), and the emulator in
[PSoXide-emulator](https://github.com/EBonura/PSoXide-emulator); both import the shared
contracts from an exact SDK revision.

## See also

- [Root README](../README.md). Project overview, quick start, build targets.
