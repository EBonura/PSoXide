# One present path

Every frame a program shows goes to the screen through `psx_rt::present` and psx-rt's VBlank
handler. There is no cargo feature and no second implementation: the handler, the guard and the
command recording are part of every build.

## What a frame is

| Frame | Call | What happens |
| --- | --- | --- |
| A DMA chain (an ordering table, recorded HUD, any linked list) | `Presenter::submit(&mut buffers, head, Part::Final)` | The presenter links the draw area, offset and clear of `buffers`' draw side in front, puts the chain in the slot and kicks it at once if the frame before is on screen and DMA channel 2 is idle; otherwise the handler kicks it at the first edge that allows it. The frame is shown at the first edge after its closing GP0(1Fh) has run, no sooner than the cap. |
| Several chains (two views, an overlay built later) | `submit(.., Part::More)` for each but the last, then `Part::Final` | Every part is kicked under the same condition; the last carries the display word. A `More` part holds no GP0(1Fh). |
| Drawing through the GP0 port | draw, then `Presenter::present_drawn(&mut buffers)` | Acknowledges GPUSTAT bit 24, sends the draw-done sequence, queues the flip and returns. The next direct access waits in the guard until the flip has landed and then points the draw area at the other buffer. |
| A loading screen, the last frame before a long blocking read | `Presenter::present_blocking(&mut buffers)` | `present_drawn`, then waits until the frame is on screen. |
| A buffer filled outside the frame path (a movie frame) | `present::show(origin)` | As `present_drawn` with an explicit origin and no buffer swap. |
| Display enable at a gate | `present::set_display_enabled(on)` | Applied at the next edge with bit 24 up, never held by the cap. |

Per frame, a chain program does:

```rust
present::wait_arena_free();          // the frame two back no longer walks this side's memory
// build the ordering table into this side's arena, ending on DRAW_DONE_NODE
unsafe { presenter.submit(&mut buffers, table.submit_head(), Part::Final) };
```

`Presenter::new(clear)` once, after `install_vblank_counter`, with the GPU idle. The cap is
`present::set_cap` ([PRESENT-CAP.md](PRESENT-CAP.md)). Waiting work can run while a part waits:
`while present::is_slot_full() { work(); present::kick(); }`.

## How the handler runs it

At each VBlank edge the handler applies the flip word if bit 24 is up and the cap's hold is zero
(GP1(05h), then the hold and the interval histogram), then runs the kick: if a part waits, the flip
word is zero and channel 2 is idle, it acknowledges bit 24, starts the walk, moves the part's
display word to the flip word and empties the slot. The kick is one assembler macro assembled twice:
into the handler, and into `__psx_rt_present_kick`, which the thread calls with CPU interrupts
masked (at submit and in every wait). The flip is the handler's alone, so it always lands in the
blanking interval. When bit 24 never rises (an FPGA core that loses GP0(1Fh)) the handler flips once
channel 2 and the GPU have been idle for two edges in a row and counts it
(`interrupts::draw_done_fallback_count`).

Which word each side writes, and why a VBlank between any two thread instructions cannot lose a
frame, kick twice, show a buffer still being drawn, draw into the buffer on screen, or leave a part
nobody kicks, is written in `psx-rt/src/present.rs` ("Interrupt safety"). The host tests run the
thread-side code against a model of the handler and the GPU with a VBlank, a walk's end or a draw's
end injected before each of its shared-word accesses, and pairs of them, and check those five
statements (`present_model.rs`). `make hello-present-queue-gate` runs the real handler in the
emulator at 60, 30 and 20 fps and under a varying load, records each frame's kick and flip edge
by edge, and fails on a tear, a lost or doubled kick, a flip sooner than the cap, or a finished
frame that waited past its edge; it runs again on the FPGA core's model with bit 24 never raised.

## Cost

The handler's work per edge with nothing queued is two loads and two branches, as before. Every
port write tests one byte (the recording or the guard), and every GPU DMA start does the same.

## Deprecated

Kept, with the same behaviour, until the last program has moved, and listed in
`sdk/profile.toml`'s `[deprecated]` so a program still calling them needs an exception:
`interrupts::queue_display_control_at_vblank`, `is_display_control_queued`,
`take_queued_display_control`, `set_min_flip_vblanks`, `min_flip_vblanks` and `present::publish_raw`;
`present::start` carries the Rust deprecation only (the gate matches by name, and `start` is everywhere). The `present-queue` features of psx-io, psx-rt and psx-engine have no effect
and remain only so an old manifest resolves. A direct flip (`DoubleBuffer::swap`,
`Gpu::set_display_start`) still waits for the cap before it writes GP1(05h).

## Moving a program onto it

- A frame flipped with `queue_display_control_at_vblank` after `arm_draw_done` and
  `signal_draw_done`: `presenter.present_drawn(&mut buffers)`. The wait for the flip to land before
  the next clear, the timeout and the take-and-write recovery go: the guard waits, bounded, and the
  presenter recovers a stalled walk itself.
- A chain published with `publish_raw(head, display_of_the_frame_before)`: `submit(.., Part::Final)`,
  without the preamble (draw area, offset, clear), the display-word arithmetic and the explicit
  draw-done handling at the end of a run; the last frame is shown without a successor.
- `wait_vblank(); swap()`: `present_drawn` (or `present_blocking` where the frame must be on screen
  before the program goes on).
- A split-screen frame that holds its flip by hand: two parts and `set_cap`.
- `set_min_flip_vblanks(n)`: `present::set_cap(PresentCap::every_vblanks(n))`.
