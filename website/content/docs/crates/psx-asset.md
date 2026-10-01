+++
title = "psx-asset"
description = "Reading cooked meshes, textures and audio"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-asset` to parse assets already converted into PSoXide's runtime formats. The editor and host tools produce those formats; game code reads them on the console.

## How the crate is organized

The root contains mesh, texture, audio and world-related views and topology helpers. `hmd8` covers the corresponding skeletal model format, and `hma1` handles animation data. Shared binary layout definitions come from `psxed-format` outside the SDK workspace.

## Integration notes

Parsers borrow their input bytes, so keep the underlying storage alive for as long as a view is used. Handle parse failures before rendering or playback. Reading an asset and uploading it to GPU or SPU memory are separate steps; the parsed view does not reserve those hardware resources. Pin the producer and consumer revisions together when format compatibility matters.

Read the complete [texture](@/docs/examples/hello-tex.md) and [audio](@/docs/examples/hello-audio.md) examples. They include cooked assets from repository paths; copying only `main.rs` into a new project will not include those files.

## API, dependencies and source structure

{{<sdk_crate name="psx-asset" />}}
