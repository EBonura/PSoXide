+++
title = "psx-rt"
description = "Runtime, boot and frame timing"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-rt` in every bare-metal program to supply the entry point, panic handling and runtime support. Keep `extern crate psx_rt;` in your executable so the runtime is linked. The loader enters `_start`, which clears `.bss`, initializes the optional heap and calls your `main`.

## How the crate is organized

`bios` and `tty` expose target-side kernel calls and debug text. `interrupts` owns interrupt and VBlank helpers, including queued display flips. `cache` handles instruction-cache maintenance; `scratchpad` provides regions and scoped scratchpad stacks. `heap` is available with `alloc`. Internal `mem` and `builtins` modules implement the memory routines and corrected software 64-bit arithmetic used on the target.

## Integration notes

The default build has no heap. Enable `alloc` only when your program needs allocation; its bump allocator is not a general reclaiming allocator. Use the top-level build commands so the linker script, load-delay hazard patcher and stack guard all run. Scratchpad stacks need the documented interrupt contract and a size proof. `scratchpad-stack-check` adds runtime checks; it does not replace the build-time guard.

Start with [hello-tri](@/docs/examples/hello-tri.md), then study [queued presentation](@/docs/examples/hello-present.md) and [scratchpad stacks](@/docs/examples/hello-spstack.md).

## API, dependencies and source structure

{{<sdk_crate name="psx-rt" />}}
