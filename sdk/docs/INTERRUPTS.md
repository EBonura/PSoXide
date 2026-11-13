# Interrupts: one dispatcher

psx-rt's exception handler (`__psx_rt_exception_handler`) is the only handler in the
exception vector. It owns VBlank (the frame counter, the present queue, the queued GP1
word) and returns through the GTE-aware path. Anything else that wants code run from an
interrupt registers with `psx_rt::dispatch`; nothing writes the vector.

```rust
// once per VBlank, after psx-rt's own VBlank work, in registration order; stays for the program
psx_rt::dispatch::on_vblank(pump)?;                 // 4 slots
// one owner per other source
psx_rt::dispatch::attach(Source::Cdrom, handler);   // a table store and a mask bit
psx_rt::dispatch::detach(Source::Cdrom);
```

`Source` is `Cdrom`, `Dma`, `Timer1` or `Timer2`. The dispatcher acknowledges nothing: a
handler clears its own source in `I_STAT` at the point its device needs (the CD controller
has its own flags read first). Registering does not enable the source in `I_MASK`, the owner
does that when its hardware is ready. A source with nothing registered is psx-rt's stray: it is
acknowledged and counted, as before.

## What a handler may do

It runs with CPU interrupts off, on one private stack shared by every handler
(`dispatch::STACK_BYTES`, 512 bytes, the saved context in the top 112;
`dispatch::paint_stack()` once at start and `dispatch::stack_unused_bytes()` give a watermark),
after the interrupted code's caller-saved registers were saved once. It must not block, panic,
use the GTE (the interrupted code's GTE registers are not saved), touch `$26`/`$27`, or
acknowledge VBlank. It must finish well inside a frame: no other interrupt is taken until it
returns. Handlers never nest, so one stack serves all of them.

## How it is built

The dispatcher is one short assembly routine (`__psx_rt_dispatch_tramp`). It saves `$1`-`$15`,
`$24`, `$25`, `$31`, HI, LO and the three callee-saved registers it uses, on its own stack;
reads the pending word the exception handler stored and the registered mask; calls the VBlank
hooks (a zero-ended array) if bit 0 is due, then one handler per due source in `I_STAT` bit order
(a table indexed by bit); restores and returns to the handler's continuation (the VBlank path's
fault check, or straight to the GTE-aware return for any other interrupt). Registration stores
the table word first and then sets the mask bit, so the exception handler never calls an empty
slot; the mask bit's read-modify-write (`__psx_rt_dispatch_update`, 15 instructions) runs with
CPU interrupts off, because a handler may register or unregister too: psx-cdstream detaches
from its own interrupt when it closes the source. `dispatch::plan` is the same walk in Rust,
unit-tested on the host, and `hello-gteirq` runs the assembly for a set of pending masks and
compares it with `plan`.


Which handlers a dispatch calls is decided once, from the pending and registered bits as they
were when it started: a handler that detaches another source that was already due does not stop
that source's handler from being called in the same dispatch. A dispatch from the VBlank path
serves VBlank alone; a source that is pending too takes the exception again as soon as it
returns.

A fault inside a handler is fatal whatever `strict-faults` says: the fault's exception overwrites
EPC and `$26`/`$27`, which the interrupted code's return needs.

## Cost

Measured on the emulator with a timer stamp at the first instruction after the vector and at
psx-rt's `rfe`, and a second pair around the Rust handler call (the same probe in both builds),
against SDK main before the dispatcher. A CD interrupt's cost moves by a few hundred cycles with
where the code lands in the 4 KB instruction cache, so hello-cdstream was linked eight times in
each build with its CD handler moved through the cache in 512-byte steps:

| hello-cdstream, cycles | Before: min / median / max | After: min / median / max |
| --- | --- | --- |
| CD interrupt | 21,850.8 / 21,964.9 / 22,180.2 | 21,832.2 / 21,936.2 / 22,012.1 |
| of which the Rust handler | 21,327.3 / 21,410.6 / 21,546.5 | 21,332.7 / 21,405.4 / 21,474.3 |
| of which the rest | 504.3 / 536.3 / 663.1 | 499.1 / 526.3 / 579.0 |
| VBlank | 172.3 / 183.4 / 217.4 | 126.7 / 132.0 / 176.9 |

| Program, one link | Interrupt | Before | After |
| --- | --- | --- | --- |
| hello-tri (registers nothing) | VBlank | 113.6 | 124.7 |
| hello-cddrain (*) | VBlank | 168.6 | 125.8 |
| hello-cddrain (*) | CD | 23,873.6 | 23,631.7 |
| hello-gteirq (2 hooks, root counter 2 handler) | VBlank / root counter 2 | | 518.7 / 429.5 |

(*) Before: main with `install_vblank_counter` out of line as well.

With nothing registered the handler does five instructions more per VBlank (load the VBlank
vector, branch, and the two slots after them) and one more per other interrupt. The dispatcher's
code, stack and table are linked only into a program that registers something (the handler jumps
through `__psx_rt_irq.vector`, which the first registration sets). Sizes are in the CHANGELOG
entry.

## Order

VBlank hooks run in registration order, before any source handler; source handlers run in `I_STAT`
bit order: CD-ROM, DMA, root counter 1, root counter 2.
