# Migrating to psx-gpu's `Gpu` driver

This SDK stage gives the GPU a driver that owns it. Everything a game calls
today still compiles: the old names are deprecated forwarders into the same
code, so a repin brings warnings, not errors. They are removed once no game's
main uses them.

The one rule behind every change: a call that writes GP0 or GP1, or starts
DMA channel 2, needs `&mut Gpu` (or the `GpuDma` token it wraps). Reading
status (`psx_gpu::is_draw_done`, `psx_gpu::recovery_stats`) needs nothing.

## Getting a `Gpu`

```rust
use psx_gpu::display::{DisplayConfig, DoubleBuffer, Resolution, VideoMode};
use psx_gpu::Gpu;

let peripherals = psx_rt::Peripherals::take().expect("peripherals are taken once");
let display = DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240);
let mut gpu = Gpu::new(peripherals.gpu_dma, display); // was psx_gpu::init
let mut buffers = DoubleBuffer::new(Resolution::R320X240); // was FrameBuffer::new(320, 240)
```

Pass `&mut Gpu` down to code that draws. Code that holds the token elsewhere
(a `FramePair`, an in-flight frame handed back by `InFlight::wait`, an
`OrderedCommandStream` after `flush`) reaches the methods through
`Gpu::from_dma_mut(&mut dma)`; frame submission takes `gpu.dma_mut()`.

## Replacements

| Old | New |
| --- | --- |
| `init(mode, res)` | `Gpu::new(dma, DisplayConfig::new(mode, res))` |
| `set_display_offset`, `set_screen_h_offset`, `set_screen_v_offset` | `gpu.set_display(display.with_offset((dx, dy)))` |
| `psx_gpu::VideoMode`, `psx_gpu::Resolution` | `psx_gpu::display::{VideoMode, Resolution}` (fields now `width()`, `height()`) |
| `wait_idle()`, `draw_sync()` | `gpu.wait_idle()` |
| `arm_draw_done()`, `signal_draw_done()` | `gpu.arm_draw_done()`, `gpu.signal_draw_done()` |
| `submit_static(&mut dma, chain)` | `gpu.submit_static(chain)` |
| `set_draw_area(x0, y0, x1, y1)` | `gpu.set_draw_area((x0, y0), (x1, y1))` |
| `set_draw_offset(x, y)` | `gpu.set_draw_offset((x, y))` |
| `set_mask_mode(set, check)` | `gpu.set_mask_mode(MaskMode::SET_ON_DRAW \| MaskMode::CHECK_BEFORE_DRAW)` |
| `fill_rect(x, y, w, h, r, g, b)` | `gpu.draw(&FillRect::new((x, y), (w, h), (r, g, b)))` |
| `draw_tri_flat`, `draw_tri_gouraud`, `draw_quad_flat`, `draw_line_mono` | `gpu.draw(&TriFlat::new(..))`, `TriGouraud`, `QuadFlat`, `LineMono` |
| `draw_line_gouraud(x0, y0, c0, x1, y1, c1)` | `gpu.draw(&LineGouraud::new((x0, y0), c0, (x1, y1), c1))` |
| `draw_rect_flat(x, y, w, h, r, g, b)` | `gpu.draw(&QuadFlat::rect((x, y), (w, h), (r, g, b)))` |
| `draw_*_blended(.., mode)` | `gpu.set_draw_mode(TextureMaterial::blended(0, 0, color, mode))` then `gpu.draw(&packet.translucent())` |
| `draw_quad_textured`, `draw_quad_textured_material` | `gpu.draw(&QuadTexturedMaterial::with_material(..))` |
| `draw_tri_textured_material` | `gpu.draw(&TriTextured::with_material(..))` |
| `draw_quad_textured_gouraud{,_material}` | `gpu.draw(&QuadTexturedGouraud::with_material(..))` |
| `draw_sprite_material(..)` | `gpu.set_draw_mode(material)` then `gpu.draw(&Sprite::with_material(..))` |
| `TextureMaterial::apply_draw_mode()`, `TextureWindow::apply()` | `gpu.set_draw_mode(material)`, `gpu.set_texture_window(window)` |
| `framebuf::FrameBuffer` | `display::DoubleBuffer`; `swap`, `begin_swap`, `apply_draw_target`, `clear` take `&mut Gpu`; `buffer_y(fb.drawing)` is `draw_origin().1` |
| `submit_linked_list_async_raw(head)`, `submit_linked_list_raw(head)` | `chain::submit_async_raw(dma, head)`, `chain::submit_raw(dma, head)` |
| `submit_linked_list_wait()` | `chain::wait(dma)` |
| `OrderingTable::insert*` | the same raw adds on `OtFrame` (`add_raw`, `add_raw_unchecked`, ...), through `frame()` or `resume_frame()` |
| `with_staged_slot_prepacked_unchecked` (unsafe) | `with_staged_slot_prepacked_colors` (safe) |
| `with_material_packet_texcoords` | `with_material` (same packet) |
| `OrderedCommandStream::new(words)` | `OrderedCommandStream::with_dma(words, gpu_dma)` |
| `configure_scanline_timer` and the Timer 1 helpers | `psx_io::timers`, `psx_rt::interrupts` |

The blended free functions also rewrote GP0(E1h) to texture page (0, 0) on
every call; with the driver that write is the explicit `set_draw_mode`, so a
game that draws textured rectangles after a blended primitive can see it.

## Present queue without `unsafe`

A game that publishes frames with `psx_rt::present::publish_raw` (quake,
the editor engine) can move to `psx_gpu::present::PresentPair` with the
`present-queue` feature: two `PresentStorage`s, a `DoubleBuffer` and the
`Gpu` go in, and each frame is `pair.present(clear_color, |frame, packets| ..)`.
The pair adds the draw target, the clear and the closing GP0(1Fh) itself.
A frame that needs a recorded HUD still uses the raw protocol for now
(`hello-present-queue` shows it).

## Changes a repin sees without touching code

- **480-line modes** set the interlace bit and program one field of
  scanlines; before, a 480-line `Resolution` showed 240 lines.
- **Display window width** follows each width's dot clock (psx-spx, GPU
  Timings): 256, 512 and 640-pixel modes get their full picture, and a
  picture offset moves by whole pixels of the current width. 320-pixel
  modes are unchanged. alttp-psx (256 pixels) is the one game this moves.
- **`wait_idle`** also waits for a frame handed to `psx_rt::present` when
  the `present-queue` feature is on.
- **Recovered hangs are counted**: `psx_gpu::recovery_stats()`.
- **Removed outright** (no user in any game's main): the
  `ot-window-insert-coalescing` feature and `coalesce_scoped_texture_windows`,
  `ordered::GpuChannel` and its alias `ordered::GpuDma`, `Resolution`'s public
  fields.
- **Signatures**: `OrderingTable::clear_with_dma` takes the
  `OrderingTableClearDma` token (no caller); `packets()` returns `Packets<'_>`,
  borrowing the table it walks; `OrderedCommandStream::flush` returns the
  idle transport.

## Affected call sites

Counted on each game's local `main` when this stage was written (wipeout
09-30, nitroxide, voxide, hl and the editor 10-03, hk 10-02, cs 09-24,
quake 10-01). Upper bounds: a line can match a game's own item of the same
name, and doc comments count.

| Area | wipeout | nitroxide | voxide | hk | hl | cs | quake | editor |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Immediate drawing (`draw_*`, `fill_rect`) | 0 | 12 | 33 | 15 | 34 | 44 | 10 | 48 |
| Display setup (`init`, draw area/offset, offsets) | 1 | 1 | 3 | 2 | 10 | 16 | 3 | 31 |
| Sync (`wait_idle`, draw-done, `submit_static`) | 1 | 0 | 4 | 12 | 12 | 9 | 9 | 16 |
| `FrameBuffer` | 0 | 0 | 13 | 25 | 18 | 16 | 7 | 17 |
| Raw submission (`submit_linked_list*`) | 0 | 1 | 1 | 2 | 4 | 5 | 4 | 6 |
| Table methods (`insert*`, `add`, `submit_async`) | 2 | 6 | 4 | 19 | 8 | 10 | 4 | 1 |
| Packet constructors and material helpers | 0 | 2 | 0 | 0 | 0 | 0 | 1 | 21 |
| Timer 1 helpers | 0 | 0 | 1 | 0 | 1 | 1 | 0 | 3 |

pico8-psx (not in the table) calls `OrderedCommandStream::new` once.
