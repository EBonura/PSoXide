# One present path

Every frame a program shows goes to the screen through `psx_rt::present` and psx-rt's VBlank
handler. There is no cargo feature and no second implementation: the handler, the guard and the
command recording are part of every build.

## What a frame is

| Frame | Call | What happens |
| --- | --- | --- |
| A DMA chain (an ordering table, recorded HUD, any linked list) | `Presenter::submit(&mut buffers, head, Part::Final)` | The presenter links the draw area, offset and clear of `buffers`' draw side in front, puts the chain in the slot and kicks it at once if the frame before is on screen and DMA channel 2 is idle; otherwise the handler kicks it at the first edge that allows it. The frame is shown at the first edge after its closing GP0(1Fh) has run, no sooner than the cap. |
| Several chains (two views, an overlay built later) | `submit(.., Part::More)` for each but the last, then `Part::Final` | Every part is kicked under the same condition; the last carries the display word. A `More` part holds no GP0(1Fh). |
| Drawing through the GP0 port | `Presenter::begin_frame(&buffers)`, draw, then `Presenter::present_drawn(&mut buffers)` | `begin_frame` waits until the frame before is on screen and points the draw area at the buffer to draw; `present_drawn` acknowledges GPUSTAT bit 24, sends the draw-done sequence, queues the flip and returns. |
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
blanking interval. When bit 24 never rises (an FPGA core that loses GP0(1Fh)) the handler flips at
the first edge where channel 2 is idle and GPUSTAT bits 26 and 28 stay up over a run of 64 reads,
and counts it
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

The handler's work per edge with nothing queued is three loads and three branches (one more than
before, the kick's slot test). A program that hands over chains or records pays one byte test on
every port write (the recording or the guard), as the present queue cost before; a program that
only draws through the port does not: the flags are plain statics only those paths ever set, so the
optimiser removes the test from its port writes. In the engine that is decided by
`Scene::PRESENT_QUEUE`, so an engine game that never uses the queue links none of the chain path.

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
  `signal_draw_done`: `presenter.begin_frame(&buffers)` before the frame's first draw and
  `presenter.present_drawn(&mut buffers)` after its last. The wait for the flip to land before the
  next clear, the timeout and the take-and-write recovery go: `begin_frame` waits, bounded, and the
  presenter recovers a stalled walk itself.
- A chain published with `publish_raw(head, display_of_the_frame_before)`: `submit(.., Part::Final)`,
  without the preamble (draw area, offset, clear), the display-word arithmetic and the explicit
  draw-done handling at the end of a run; the last frame is shown without a successor.
- `wait_vblank(); swap()`: `begin_frame` before drawing and `present_drawn` after (or
  `present_blocking` where the frame must be on screen before the program goes on).
- A split-screen frame that holds its flip by hand: two parts and `set_cap`.
- `set_min_flip_vblanks(n)`: `present::set_cap(PresentCap::every_vblanks(n))`.

## The direct-access guard on the console

Once a chain has been handed over, the first direct GPU access (a port write, a GPU DMA address
store) waits until the walk is done. On the console that wait is `psx_rt::present::wait_idle`
(symbol `__psx_rt_wait_idle`), which `psx_io::gpu` calls by a **direct call**, not through a
registered pointer: a call tree that reaches a direct write then holds no call through a register,
which the stack proofs of overlay and scratchpad-stack code (`stack-guard`) require. The host build
keeps a registered function (`set_direct_access_guard`, host only).

A program that never hands a chain over must not pay for that wait (`wait_idle`, the walk stop it
calls, the draw-done signal and its list are over a kilobyte). Two things make that true whatever the
program's LTO setting:

- `psx_io::gpu::handoff::run_armed_guard` calls the symbol `__psx_direct_access_guard`, which
  `sdk/psoxide.ld` defines: the address of `wait_idle` when `wait_idle` is in the image, else 0. The
  script finds out from the `.text.__psx_rt_wait_idle` window it places `wait_idle` in (empty when
  the function was dropped).
- `psx_io::gpu::arm_direct_access_guard` carries an `R_MIPS_NONE` relocation against
  `__psx_rt_wait_idle`: no bytes, and the linker's section collection treats the function as used
  wherever an arm is. Every arm (`Presenter::submit`, `publish_raw`) is therefore in a program that
  has the wait, and `run_armed_guard`, which runs only after an arm, never calls the 0.

Why not the cheaper-looking forms: a weak definition or a `PROVIDE` default does not help, because a
call that names the wait keeps it linked (section collection follows every relocation to a defined
symbol), and compile-time folding of the armed flag needs fat LTO (with thin LTO the flag is read
inline in other crates and cannot be proved constant). The window is the one thing the link can see
that depends on what the program links.

What this relies on: **a console program that links `psx-io` also links `psx-rt`**, which holds
`__psx_rt_wait_idle` and the whole present path (it is compiled unconditionally). A program that did
not would fail to link when it arms the guard, with an undefined symbol named
`__psx_rt_wait_idle`, which is the right failure. Calling `arm_direct_access_guard` from a program
that has no `psx-rt` present path is not supported.

### A program with its own linker script

- A script that is `sdk/psoxide.ld` with text replaced (a derived copy that adds placement lines before
  the `*(.text .text.*);` catch-all) and a second `-T` script of overlay sections (`INSERT BEFORE .text`
  or its own memory regions) need no change: the window and the definition are in the copy, and the
  overlay sections never match `.text.__psx_rt_wait_idle`.
- A script that **replaces** `psoxide.ld` (a chain-loader blob, a layer linked with C) has no
  `__psx_direct_access_guard`. A program of it that cannot arm the guard (fat LTO drops
  `run_armed_guard`, so it is not linked) links as before. One that can arm fails at link time with
  `undefined symbol: __psx_direct_access_guard`; it must add, before its `.text` catch-all and inside
  `.text`:

  ```
  . = ALIGN(4);
  __psx_guard_window_start = .;
  *(.text.__psx_rt_wait_idle)
  __psx_guard_window_end = .;
  ```

  and, after the `.text` output section:

  ```
  __psx_direct_access_guard = __psx_guard_window_end > __psx_guard_window_start ? __psx_guard_window_start : 0;
  ```

  Copying the second without the first leaves the symbol 0 in a program that arms. A link cannot
  assert against that: a script expression that names `__psx_rt_wait_idle` makes the linker keep it,
  which is what the window exists to avoid, and a link has no way to ask whether a program contains an
  arm. The stack proof (`stack-guard`) fails on the resulting `jal 0`, and a console build that gets there
  jumps to address 0 on the first direct write after a chain, so build such a program once with
  `guard_stack_gate.sh`'s recipe (or any direct write after a `Presenter::submit`) before shipping.
- The overlay packer ignores `R_MIPS_NONE` relocations, which the arm leaves in resident (or cold) code.

`tools/guard_stack_gate.sh` (part of `make lint`) keeps it true: it builds `hello-present`, which arms
the guard and writes directly, and runs `stack-guard` on the call tree rooted at a direct write
(`psx_io::gpu::handoff::write_command_slow`), failing if the tree calls through a register or does
not reach the guard's wait. A later change that put a pointer back fails there, in the SDK.
