+++
title = "hello-pack"
description = "Reading and decompressing WORLD.PAK"
[extra]
kind = "Complete example"
eyebrow = "SDK example · full source"
+++

## What this program does

Find entries that cross sector boundaries, load raw and compressed chunks, verify their checksums and compare the decoded payloads against deterministic patterns. The program expects a specific fixture layout.

## Build and run

Use the [documented SDK checkout and tool setup](@/docs/sdk.md#build-the-documented-revision). Run these commands from the repository root.

You can build the executable:

```sh
make example EXAMPLE=hello-pack
```

**Fixture limitation in this revision:** the source mentions `make hello-pack-disc` and `tools/hello_pack_fixture.py`, but neither is present in this SDK checkout. A generic `make disc` image will not pass this diagnostic.

The expected pack has 86 entries (IDs 0–85), with the boundary-straddling entry 84 holding a 3,000-byte raw pattern and entry 85 holding a 5,000-byte compressible pattern. The exact patterns are in the source below. A matching fixture must be supplied before running the test. Use this page as a complete source reference for the reader; do not interpret a missing-pack failure as an emulator defect.

For original hardware, read the [burned-disc requirements and warning](@/legal.md#running-burned-discs-on-original-hardware).

## Complete source

{{<sdk_example_source name="hello-pack" />}}
