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
after the interrupted code's caller-saved registers and `$28` were saved once. It must not block, panic,
use the GTE (the interrupted code's GTE registers are not saved), touch `$26`/`$27`, or
acknowledge VBlank. It must finish well inside a frame: no other interrupt is taken until it
returns. Handlers never nest, so one stack serves all of them.

## How it is built

The dispatcher is one short assembly routine (`__psx_rt_dispatch_tramp`). It saves `$1`-`$15`,
`$24`, `$25`, `$28`, `$31`, HI, LO and the three callee-saved registers it uses, on its own stack;
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

## What is saved, and why `$28`

A handler is a compiled Rust function, so it may write any register the compiler lets a function write
without saving it. That set was read off the compiler's output, not off the o32 table: a scan of a
linked game image (1,600 functions) for every register a function writes and does not store to its frame.
`$1`-`$15`, `$24`, `$25` and `$31` are written freely; `$16`-`$23` and `$30` never without a save in the
function (the callee-saved set, saved by the handler's own code); `$26` and `$27` only by psx-rt's own
assembly; **and `$28`, the global pointer, by 107 of them with no save at all**: this toolchain allocates
it as a general register. The dispatcher therefore saves every register the compiler may leave changed:
`$1`-`$15`, `$24`, `$25`, `$28`, `$31`, HI and LO; `$16`-`$18` it uses itself and saves; `$19`-`$23` and `$30`
reach a handler's own prologue unchanged. `$26`/`$27` belong to the exception path. Interrupted code that held a
live value in `$28` (the hot loops of a game do) would otherwise resume with a handler's leftover. The
two wrappers the dispatcher replaced saved `$28` for this reason; the dispatcher's first version did not, which
`hello-gteirq` now checks (`make hello-gteirq-gate`): the foreground holds a known value in `$28` while a
VBlank hook and a root counter handler both overwrite it.

Where an exception enters compiled code, and what is saved there: the dispatcher (above); psx-rt's exception
handler itself, which is assembly using `$26` and `$27` only (VBlank work, the present kick, the fault paths
and the stray count enter no compiled code); the masked update routine `__psx_rt_dispatch_update`
(assembly, entered as a function, `$2`, `$3`, `$5`, `$8`, `$9` only); the `console-diag` stop panel, which
saves every register including `$28` before it calls its Rust watchdog and fault routines; the legacy
`claim_interrupt_sources` and `declare_stack_safe_handler`, which only store words and enter no code.

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

The save and restore of `$28` cost two instructions and about 8 cycles per dispatched interrupt (the routine is 8 bytes
larger). With nothing registered the handler does five instructions more per VBlank (load the VBlank
vector, branch, and the two slots after them) and one more per other interrupt. The dispatcher's
code, stack and table are linked only into a program that registers something (the handler jumps
through `__psx_rt_irq.vector`, which the first registration sets). Sizes are in the CHANGELOG
entry.

## Order

VBlank hooks run in registration order, before any source handler; source handlers run in `I_STAT`
bit order: CD-ROM, DMA, root counter 1, root counter 2.
