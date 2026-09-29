+++
title = "Build and run your first PS1 program"
description = "Clone the SDK, build a triangle into a bootable BIN/CUE and run it in the PSoXide emulator."
weight = 1
[extra]
kind = "Walkthrough"
eyebrow = "Walkthrough · SDK"
+++

By the end of this page you'll have `hello-tri`, the smallest PSoXide program, running as a real PlayStation disc image. It clears the screen to dark blue and draws one Gouraud-shaded triangle that bounces a little each frame, so you can see the render loop is alive. No editor is involved: this is the bare-metal SDK on its own.

{{<figure src="img/shots/hello-tri.png" alt="hello-tri running: a red, green and blue shaded triangle on a dark blue background" native={true} width={320} height={240} caption="hello-tri at the PlayStation's native 320×240." />}}

## 1. Install the tools

You need three things on your machine:

- **Rust through [rustup](https://rustup.rs).** You don't pick the version yourself. The repository's `rust-toolchain.toml` pins the nightly it needs (the `mipsel-sony-psx` target and `build-std` are nightly-only) and rustup installs it with `rust-src` and the other components. If yours doesn't do that on first use, run `rustup toolchain install` inside the checkout.
- **A C/C++ build toolchain** for your host (Xcode command line tools on macOS, `build-essential` or similar on Linux).
- **Python 3 and `mipsel-none-elf-objdump`.** The instruction-hazard check uses them. It disassembles the finished executable to catch MIPS load-delay hazards before they reach a console.

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
The SDK's build turns on LLVM's wider delay-slot search, then runs `tools/hazard_patch.py` over the linked program and rescans it. Any load left in a delay slot whose result is used too early gets rerouted, because the R3000 won't stall for you. `tools/stack_guard.py` also proves every scratchpad stack fits its region.
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

In **Settings**, choose your games directory and select the SDK's `build/examples/mipsel-sony-psx/release/` folder. In **Games**, refresh the library if needed and select `hello-tri`. Expand any collapsed folder with a click or Enter. The triangle should appear on a dark blue background.

For an automated check instead, run this from the SDK checkout:

```sh
make run-tri FRONTEND=/absolute/path/to/PSoXide-emulator/target/release/frontend
```

`run-tri` rebuilds the disc if needed and uses `frontend launch`, which runs **headlessly**: it prints the final emulator state and does not open a window. You don't need a BIOS in either mode.

The CUE works in other PlayStation emulators too, and you can burn the image to a CD-R and boot it on a console that runs burned discs.

## What the program does

Here's the heart of `sdk/examples/hello-tri/src/main.rs`, trimmed of comments:

```rust
#![no_std]
#![no_main]

extern crate psx_rt; // brings in _start, the panic handler and the heap

use psx_gpu::{self as gpu, framebuf::FrameBuffer, Resolution, VideoMode};

#[no_mangle]
fn main() {
    gpu::init(VideoMode::Ntsc, Resolution::R320X240);

    // Two buffers: draw into one while the TV shows the other.
    let mut fb = FrameBuffer::new(320, 240);
    gpu::set_draw_area(0, 0, 319, 239);
    gpu::set_draw_offset(0, 0);

    let mut frame: u16 = 0;
    loop {
        fb.clear(0, 0, 64);

        let wobble = (((frame % 60) as i16) - 30).abs();
        let verts = [(160, 40 + wobble), (60, 200 - wobble), (260, 200 - wobble)];
        gpu::draw_tri_gouraud(verts, [(255, 64, 64), (64, 255, 64), (64, 64, 255)]);

        gpu::draw_sync();
        psx_rt::interrupts::wait_vblank();
        fb.swap();

        frame = frame.wrapping_add(1);
    }
}
```

There's no operating system underneath. `psx_rt` provides `_start`, clears BSS and calls `main`. From there it's you and the hardware: set the video mode, then loop forever. Clear the back buffer, send one triangle to the GPU, wait for the GPU to finish and for vertical blank, and swap buffers. Double buffering keeps the displayed frame separate from the one being drawn.

## Where to go next

Build any other example the same way with `make disc EXAMPLE=<name>`:

| Example | Shows |
|---|---|
| `hello-input` | Controller polling through `psx-pad` |
| `hello-tex` | Textured primitives and a CLUT upload |
| `hello-ot` | Depth sorting with an ordering table |
| `hello-gte` | Transforms on the GTE, the PS1's geometry coprocessor |
| `hello-audio` | Playing a voice on the SPU |

Examples that use CD audio or a `WORLD.PAK` need their own pack inputs; the generic `disc` target only makes a data-only image. The full list is in the [SDK's README](https://github.com/EBonura/PSoXide/blob/main/sdk/README.md).

{% <callout kind="warn" title="Pre-1.0"> %}
The SDK is pre-1.0. Formats and APIs still change, and downstream games pin an exact revision. Emulator checks don't replace testing on original hardware.
{% </callout> %}
