+++
title = "Build, run, inspect and test"
description = "A repeatable workflow for PlayStation development, with tools developers and coding assistants can use."
weight = 20
[extra]
kind = "Workflow"
+++

PSoXide's tools support short development cycles: make a change, build it, run it,
inspect the result and decide what to try next. That approach supports original
games, ports and experiments in rewriting existing PS1 games more efficiently.

## Build a known version

Start with [your first PS1 program](@/docs/first-ps1-program.md), or follow the
build instructions for the project you want to work on. Keep its source revision,
tool versions and component revisions with your results so another run can use
the same inputs.

The repositories document their own build and check commands:

- [SDK and examples](https://github.com/EBonura/PSoXide)
- [Editor, engine and Cortex Ignition](https://github.com/EBonura/PSoXide-editor)
- [Emulator and headless verification](https://github.com/EBonura/PSoXide-emulator)

## Run and inspect

The emulator has desktop and browser frontends, plus a command-line mode for
recorded runs. For example, with a built native emulator:

```sh
./target/release/frontend launch \
  --path /path/to/game.cue \
  --steps 8000000 --dump-hash
```

Replace the path with your disc's CUE file and keep its BIN files alongside it.
This bounded run is a starting point for inspection, not a complete gameplay test.
For the current options, see the emulator repository's build and run instructions.

Recorded routes can capture emulated cycles and display flips using `--route-log`.
Compare the same workload before and after a change; a different route, build or
renderer can change what the numbers mean. The site's
[recorded comparisons](@/emulator/compare.md) show how results are tied to their
test conditions.

## Work with coding assistants

Command-line builds, tests and inspection tools can be used by a developer or a
coding assistant. The native emulator's optional `mcp` feature enables its
debugging server, giving compatible assistants a way to inspect execution while
working on the code. Check the emulator repository for the interfaces supported
by your build; this native integration is separate from the browser player.

Keep the cycle grounded in observations: describe a problem, reproduce it, inspect
the result, make a change and rerun the relevant checks. Review the code and the
evidence together. [How PSoXide is developed](@/ethos.md#how-the-work-is-developed)
explains the project's use of AI assistance and developer review.

## Verify on the console

Emulator checks help find problems and make them repeatable. Test relevant changes
on original hardware too, especially when they affect timing, rendering, audio or
disc access. Record the console model, game build, scene and known limitations.

For an efficiency claim, compare frame times and memory use on equivalent work.
Keep gameplay and presentation changes visible in the comparison. A target is not
a measured result, and one successful route is not a full-game verification.
