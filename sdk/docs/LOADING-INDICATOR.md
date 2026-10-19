# Loading indicator: adopting `psx-loading`

A load that blocks for seconds (a chain-load, a pack read, a sound bank
streamed to the SPU, a memory card) leaves the picture frozen, and a frozen
picture reads as a hang. `psx-loading` is the shared fix, and every PSoXide game
uses the same one: the **corner widget**, a small black square in the
bottom-right corner of the picture with a little animation in it and no text.
The corner and the box are the same everywhere; the animation inside is the
game's own (its colours, or art it draws). The widget is drawn as plain GP0
rectangles, so it needs no texture, no atlas and no heap.

## What a game gets

| Need | Call |
| --- | --- |
| The corner widget | `Style::new(&font, "", ink, Rgb(0, 0, 0)).with_tail(tail).scaled(2).in_corner(32, 320, 240, 12)`. `const fn`s, so a `const STYLE` works |
| The animation | `Tail::Spinner { lit, unlit }` (the 2x3 dot spinner in the game's colours), or `Tail::Art { width, height, frames, draw }` for art the game draws itself with `psx_loading::rect` or any GP0 packet |
| A running indicator | `Indicator::new(STYLE)`; `begin(tick)` restarts it |
| Keep it moving | `indicator.poll(&mut Gp0, tick)` between the steps of a blocking load. `tick` advances once per VBlank. It draws only when the animation stepped, and never waits |
| Draw one frame yourself | `indicator.draw(&mut Gp0, style.look(tick))` (unconditional) |
| GPU state | `psx_loading::prepare(&mut Gp0)` puts the mask bit and draw mode at their defaults. The drawing area and offset are yours: they decide which buffer the box lands in |
| Test | implement `Sink` on a `Vec<Vec<u32>>` and read the packets |

`Gp0` (guest only) writes each packet with one bounded `GPUSTAT` ready wait,
then every word back to back. That is the pacing the silicon needs; a host sink
records the packets instead. A label with stepping dots (`Tail::Dots`) is there
for a program that wants words, and every part of the look (font, scale, inks,
plate, rule, position) can be set.

## Where to poll from

Nothing in the crate runs in an interrupt. The widget redraws from the caller's
own blocking loop, so a draw never overlaps the caller's other GPU traffic and
needs no locking against the VBlank handler or the present queue. A load that
reads sectors calls back between them, so the widget steps every few
milliseconds. The hooks the SDK and the shared engine provide:

- **Any polled disc load** (`psx_io::cd::reader::set_idle_hook`). The SDK's
  polled sector reader calls the hook from every spin of every wait it makes
  (a seek, the next sector, a command's acknowledge), so one hook animates every
  load built on it, whichever crate issues it: pack table scans, chunk reads and
  streamed assets included. Install it when the load starts, remove it (`None`)
  when it ends. The hook runs thousands of times a second, so it checks the
  VBlank count and returns at once unless the animation steps.
- **A chain-load** (`psx-chainloader`, editor engine). Give the
  `Presentation` a `tick` method: it runs after every payload sector, once per
  spin of a seek wait and every 4 KiB of the checksum pass, with interrupts
  masked. Count VBlank edges with `paint::take_vblank()` and poll the widget
  with that count. The disc's intro programs do this.
- **Between steps.** For work with no sector loop (a memory card read, a decode),
  call `poll` once per step.

A stretch with no hook (a seek of a few hundred milliseconds, a card write) holds
the last picture; the animation resumes at the next call.

## Look

The spinner steps every two VBlanks and the box is redrawn only then, so a load
costs a few hundred GPU words thirty times a second. The widget sits low in the
picture: the beam reaches the bottom of the frame about 12 ms after the VBlank
edge, well after a redraw of this size finishes, so it does not tear. Keep it
inside the TV's safe area (12 pixels from each edge in the shipped look).

## Adoption

| Program | Animation | Poll from |
| --- | --- | --- |
| HL (intro program, then the game) | The orange dot spinner (the HL menu's own), amber `lit`, dark `unlit`. The intro draws the widget from the end of the movie through the chain-load of `HLGAME.EXE`; the game carries on with the same widget through its boot reads, the menu-to-game load and every changelevel. The game's earlier text strip, its menu spinner and its black loading card are gone | The intro: `Presentation::tick`. The game: `set_idle_hook` |
| WipEout | The same spinner in WipEout's red | `set_idle_hook`, from the end of the intro movie until the menus take the screen. In progress: the integration builds (EBonura/wipeout-psx `feat/boot-loading-2026-10-18`) but has not been seen working on screen yet |
| HK | not adopted yet. The intro program (`hk-polish`) chain-loads the game through `psx-chainloader`: implement `Presentation::tick` as the HL intro does (`intro/src/main.rs` in hl-psx), with HK's own colours or `Tail::Art` | |

## Notes for adopters

- Pick the buffer. The rectangles go wherever the drawing area and offset point.
  A program that double-buffers sets them to the buffer being shown, draws, and
  puts them back (HL's `draw_loading_strip`).
- Erase is built in: each draw first fills the box with `ground`. A game that
  wants the picture to show through does not use a plate; it clears the region
  itself and calls `invalidate()` so the next poll draws again.
- The `Gp0` sink goes through `psx_io::gpu::write_command`, so with the
  `present-queue` feature it goes into an open recording like any other
  command. Poll from a blocking load, not from inside a recorded frame.
