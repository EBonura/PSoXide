+++
title = "Build and run your first PS1 program"
description = "Clone the SDK, build a triangle into a bootable BIN/CUE and run it in the PSoXide emulator."
weight = 1
[extra]
kind = "Walkthrough"
eyebrow = "Walkthrough · SDK"
+++

By the end of this page you'll have `hello-tri`, a minimal PSoXide program, running as a real PlayStation disc image. It clears the screen to dark blue and draws one triangle with its vertex colours blended across it (Gouraud shading) that bounces a little each frame, so you can see the render loop is alive. No editor is involved: this is the bare-metal SDK on its own.

{{<example_player name="hello-tri" />}}

## 1. Install the tools

You need two things on your machine:

- **Rust through [rustup](https://rustup.rs).** You don't pick the version yourself. The repository's `rust-toolchain.toml` pins the nightly it needs (the `mipsel-sony-psx` target and `build-std` are nightly-only) and rustup installs it with `rust-src` and the other components. If yours doesn't do that on first use, run `rustup toolchain install` inside the checkout.
- **A C/C++ build toolchain** for your host (Xcode command line tools on macOS, `build-essential` or similar on Linux).

## 2. Clone the SDK and check it builds

```sh
git clone https://github.com/EBonura/PSoXide.git
cd PSoXide
make check
make test
```

`make check` type-checks both workspaces: the host one (tools, shared formats, the disc packer) and the `sdk/` device one that targets the PlayStation. `make test` runs the host tests, including the hazard tools and the stack guard.

## 3. Build the disc image

```sh
make hello-tri-disc
```

This compiles `sdk/examples/hello-tri` for `mipsel-sony-psx`, links it into a PS-X EXE, then masters a CD image around it. You get three files in `build/examples/mipsel-sony-psx/release/`:

- `hello-tri.exe`, the PlayStation executable
- `hello-tri.bin` and `hello-tri.cue`, the disc image

Keep the BIN and the CUE together. The CUE is the file you open; it points at the BIN.

{% <callout title="What the build does to your code"> %}
The PlayStation's CPU (a MIPS R3000) doesn't wait for a value loaded from memory to arrive: the instruction straight after a load still sees the old register contents. Compilers normally schedule around this, but not always. After linking, `hazard-patch` (from `tools/psoxide-hazard`) finds any instruction that reads a loaded value too early, reroutes it, and scans the program again. `stack-guard` then checks that every stack placed in the CPU's small, fast scratchpad RAM fits its space.
{% </callout> %}

## 4. Get the emulator

The emulator lives in its own repository. Build it once:

```sh
git clone https://github.com/EBonura/PSoXide-emulator.git
cd PSoXide-emulator
make bootstrap
make build
```

`make bootstrap` fetches the exact SDK revision the emulator is pinned to. On Ubuntu, install `pkg-config libasound2-dev libudev-dev libxkbcommon-dev` first. The result is `./target/release/frontend`.

## 5. Run it

To see the triangle, start the desktop app from the emulator checkout:

```sh
./target/release/frontend --windowed
```

Open the **Library** menu and choose **Choose games folder**, then select the SDK's `build/examples/mipsel-sony-psx/release/` folder. Back in **Library**, use **Refresh library** if `hello-tri` isn't listed yet, then select it. Expand any collapsed folder with a click or Enter. The triangle should appear on a dark blue background.

For an automated check instead, run this from the SDK checkout:

```sh
make run-tri FRONTEND=/absolute/path/to/PSoXide-emulator/target/release/frontend
```

`run-tri` rebuilds the disc if needed and uses `frontend launch`, which runs **headlessly**: it prints the final emulator state and does not open a window. You don't need a BIOS in either mode.

The CUE works in other PlayStation emulators too. You can also burn the image to a CD-R, but booting it on a console needs a modchipped console or another homebrew boot method. PSoXide does not endorse modchipping or provide installation help, and modifications can permanently damage or brick hardware. Read the [hardware warning](@/legal.md#running-burned-discs-on-original-hardware) first.

## What the program does

Here's `sdk/examples/hello-tri/src/main.rs` with most comments removed:

```rust
#![no_std]
#![no_main]

extern crate psx_rt; // keeps _start, the panic handler and (if enabled) the heap

use psx_gpu::display::{DisplayConfig, DoubleBuffer, Resolution, VideoMode};
use psx_gpu::prim::TriGouraud;
use psx_gpu::Gpu;
use psx_rt::tty;

#[no_mangle]
fn main() {
    tty::println("hello-tri: booted via HLE BIOS");

    let Some(peripherals) = psx_rt::Peripherals::take() else {
        return;
    };
    let mut gpu = Gpu::new(
        peripherals.gpu_dma,
        DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240),
    );

    // Two buffers: draw into one while the TV shows the other.
    let mut fb = DoubleBuffer::new(Resolution::R320X240);
    gpu.set_draw_area((0, 0), (319, 239));
    gpu.set_draw_offset((0, 0));

    tty::println("hello-tri: entering render loop");
    let mut frame: u16 = 0;
    loop {
        fb.clear(&mut gpu, (0, 0, 64));

        let wobble = (((frame % 60) as i16) - 30).abs();
        let verts = [(160, 40 + wobble), (60, 200 - wobble), (260, 200 - wobble)];
        gpu.draw(&TriGouraud::new(
            verts,
            [(255, 64, 64), (64, 255, 64), (64, 64, 255)],
        ));

        gpu.wait_idle();
        psx_rt::interrupts::wait_vblank();
        fb.swap(&mut gpu);

        frame = frame.wrapping_add(1);
    }
}
```

There's no operating system underneath. `psx_rt` provides `_start`, zeroes the program's uninitialised globals and calls `main`. `tty::println` writes to the kernel's debug text output, which the emulator shows in its log; it doesn't appear on screen.

Hardware access goes through owned tokens. `Peripherals::take()` hands out the set once and returns `None` after that, and `Gpu::new` consumes the GPU's DMA token, so only one piece of code can drive the GPU at a time. From there it's you and the hardware: set the video mode, then loop forever. Clear the back buffer, send one triangle to the GPU, wait for the GPU to finish and for vertical blank, and swap buffers. Double buffering keeps the displayed frame separate from the one being drawn.

## Where to go next

Build any other example the same way with `make disc EXAMPLE=<name>`:

| Example | Shows |
|---|---|
| `hello-input` | Controller polling through `psx-pad`'s `PadReader` |
| `hello-tex` | Textured sprites using a colour palette (CLUT) uploaded to video memory |
| `hello-ot` | Depth sorting with an ordering table |
| `hello-gte` | Transforms on the GTE, the PS1's geometry coprocessor |
| `hello-audio` | Playing sound effects on the SPU, the PS1's sound chip |

Examples that use CD audio or a `WORLD.PAK` need their own pack inputs; the generic `disc` target only makes a data-only image. The full list is in the [SDK's README](https://github.com/EBonura/PSoXide/blob/main/sdk/README.md).

{% <callout kind="warn" title="Pre-1.0"> %}
The SDK is pre-1.0. Formats and APIs still change, and downstream games pin an exact revision. Emulator checks don't replace testing on original hardware.
{% </callout> %}

## Running on original hardware

A standard, unmodified retail PlayStation cannot directly boot a burned PSoXide
disc. You need a modchipped console or another compatible homebrew boot method.
PSoXide does not endorse modchipping or provide installation instructions or help.
Modifications can permanently damage or brick your console and are undertaken at
your own risk. Read the [hardware warning](@/legal.md#running-burned-discs-on-original-hardware).

Before distributing a program built with the SDK, read the
[code and content licensing requirements](@/legal.md#code-and-licences).
