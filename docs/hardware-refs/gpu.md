# GPU

## MMIO layout

Only two 32-bit ports, multiplexed for both command submission and
status readback:

| Address | R | W |
|---|---|---|
| `0x1F80_1810` | `GPUREAD` (returns VRAM→CPU transfer data or response) | `GP0` (drawing commands) |
| `0x1F80_1814` | `GPUSTAT` (status register) | `GP1` (display control commands) |

Commands are identified by the upper byte. GP0 opcodes cover
drawing primitives, draw-mode settings, and VRAM transfers. GP1
opcodes cover display configuration and DMA direction.

## GPUSTAT bits

| Bit | Meaning |
|---|---|
| 3:0 | Texture page X base |
| 4 | Texture page Y base |
| 6:5 | Semi-transparency mode |
| 8:7 | Texture colour depth (4/8/15 bpp) |
| 9 | Dither enable |
| 10 | Drawing to display enable |
| 11 | Set mask bit when drawing |
| 12 | Draw pixels with mask bit set |
| 13 | Interlace field |
| 14 | "Reverseflag" |
| 15 | Upper texture-page Y bit (gated by GP1(09h) 2 MiB addressing) |
| 16 | Horizontal resolution (320..=640) second half |
| 18:17 | Horizontal resolution (256/320/512/640) first half |
| 19 | Vertical resolution (240/480 interlaced) |
| 20 | Video mode (0 = NTSC, 1 = PAL) |
| 21 | Display area colour depth (15/24 bpp) |
| 22 | Vertical interlace |
| 23 | Display disable |
| 24 | Interrupt request |
| 25 | DMA / data request (computed, see below) |
| 26 | Ready to receive command word |
| 27 | Ready to send VRAM to CPU |
| 28 | Ready to receive DMA block |
| 30:29 | DMA direction (0=Off / 1=FIFO / 2=CPU→GPU / 3=GPU→CPU) |
| 31 | Interlace / even-odd line flag (toggles at VBlank) |

Bits 26/27/28 are the "ready" flags BIOS + games spin on before
sending commands. PSoXide drives them from its source-sensitive GPU
execution backlog; CPU stores also receive the silicon-measured stall
without incorrectly clearing DMA-block-ready bit 28.

Bit 25 is **computed per read** from the DMA-direction bits:
- Off: 0
- FIFO: 1 (always -- FIFO can always accept)
- CPU→GPU: copy of bit 28
- GPU→CPU: copy of bit 27

Bit 31 toggles on every VBlank in a real GPU; BIOS polling loops
wait for it to flip. We'll flip it when the cycle model drives
scan-out.

## GP0 command space (high-byte dispatch)

| Range | Kind |
|---|---|
| `0x00` | NOP / various misc |
| `0x01` | Clear cache |
| `0x02` | Fill rectangle (solid colour) |
| `0x20..=0x3F` | Triangles (flat / shaded × textured / untextured × 3-vert / 4-vert) |
| `0x40..=0x5F` | Lines |
| `0x60..=0x7F` | Rectangles |
| `0x80..=0x9F` | VRAM-to-VRAM blit |
| `0xA0..=0xBF` | CPU-to-VRAM transfer |
| `0xC0..=0xDF` | VRAM-to-CPU transfer |
| `0xE0..=0xFF` | Draw-mode settings (texture window, draw area, offset, …) |

Most drawing commands are packet-shaped: a primary word with the
opcode + colour, followed by 2–12 additional words (vertices,
UVs, extra colours). A full GP0 decoder implements a FIFO state
machine: after the primary word the GPU knows how many more words
to expect and accumulates them before acting. Our Phase 2h impl
accepts and discards the primary word only -- full FIFO arrives
when we need to render.

## GP1 command space

| Opcode | Action |
|---|---|
| `0x00` | Reset GPU -- clears everything, restores defaults |
| `0x01` | Reset command buffer |
| `0x02` | Acknowledge GPU interrupt |
| `0x03` | Display enable (bit 0: 0 = on, 1 = off) |
| `0x04` | DMA direction (bits 1:0) |
| `0x05` | Start of display area in VRAM |
| `0x06` | Horizontal display range |
| `0x07` | Vertical display range |
| `0x08` | Display mode (resolution, refresh rate, colour depth) |
| `0x09` | Allow 2 MiB VRAM addressing / upper texture-page Y bit |
| `0x10` | Get GPU info (various sub-opcodes) |

## Rust shape

```rust
pub struct Gpu {
    pub vram: Vram,
    status:   GpuStatus, // private, dispatched through GP1 writes
}

impl Gpu {
    pub fn read32(&self, phys: u32) -> Option<u32>;   // returns None off-port
    pub fn write32(&mut self, phys: u32, value: u32) -> bool; // true if handled
}

const GP0_ADDR: u32 = 0x1F80_1810;
const GP1_ADDR: u32 = 0x1F80_1814;
```

`read32`/`write32` return `Option` / `bool` (not `contains`-style
pre-check) so the bus dispatch reads

```rust
if let Some(v) = self.gpu.read32(phys) { return v; }
// ... next region
```

which keeps the happy path branchless and lets the GPU own the
address match.

## What's missing

Everything the original milestone-A list named (command FIFO, triangle /
line / rect / fill rasterizers, texture cache + CLUT lookup, VRAM
transfers, scan-out, VBlank + interlace, GPU IRQ, DMA ch2 linked-list
walker) has since shipped in the software GPU.

The hardware renderer (`psx-gpu-render`) draws GP0 line/polyline
primitives (0x40..0x5F) too, as of the emu-gpu-lines work: the shared
interpreter decodes the whole family (the CPU GPU appends polyline
continuation words to the cmd_log entry, terminator excluded) and the
translator expands each segment into a one-PSX-pixel quad band,
endpoint-inclusive and connected at any slope, upscaling with the
internal-resolution target. `--dump-hw` frames therefore include
lines. The band is center-sampled rather than Bresenham-stepped, so
arbitrary-slope segments may round individual steps one minor-axis
pixel away from the CPU walk (endpoints and h/v/45-degree lines land
exactly); this CPU rasterizer stays the pixel-exact oracle. The
accurate compute backend still skips lines and counts them as
unhandled.

## References

- Nocash PSX-SPX -- "GPU"
- PCSX-Redux `src/core/gpu.cc` + `src/gpu/soft/` -- our primary reference oracle
- PSoXide-2 `emulator/gpu/` -- still worth reading for rasterizer design
- `emulator_core::gpu` -- our impl
