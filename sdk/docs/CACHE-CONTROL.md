# Cache control and DMA: why the SDK owns the register

Changing the instruction-cache control register (`0xFFFE0130`), or isolating the cache with
COP0 Status.IsC, while any DMA channel is moving data can make the CPU's next instruction
fetch receive a word of the DMA's traffic. On a console this killed Quake; the emulator
cannot reproduce it, so the SDK makes the mistake impossible to write instead of hoping to
notice it.

Rule: never touch the cache control register while any DMA channel runs. Nobody outside
`psx_rt::cache` touches it at all.

## What happened

The Quake bisect disc of 2026-10-22 ran seven variants on a console. Every flush of the
instruction cache in the overlay path (two per tick, mid-frame, with the previous frame's GPU
list still being walked by DMA channel 2) was a chance to fail.

| Variant | Flush | DMA 2 during the flush | Result |
| --- | --- | --- | --- |
| A, as shipped | SDK routine | live (present queue) | froze silently, 2.3 s into the first map |
| B, blocking present | SDK routine | live (the walk is still started asynchronously) | froze |
| C, strict VRAM uploads | SDK routine | live | froze |
| D, no overlays | none | n/a | ran to the end of the recording |
| E, no scratchpad stacks | SDK routine | live | froze |
| F, BIOS `A(44h)` | BIOS FlushCache | live | fault panel: bus error on instruction fetch |
| G, idle wait before the flush | SDK routine | idle | ran to the end of the recording |

Variant F's panel pinned the fault to the first fetch after the cache control write: PC
`B428AD34`, which is what a `j` with the word `090A2B4D` jumps to. Read as a GPU list header
that word is a count-9 textured quad whose next pointer is `0A2B4D`, against DMA 2's address
register `000A2B4C`. The word the CPU fetched was the one the DMA was reading. The same fault
inside the SDK's own routine is silent, because it is taken with IsC set and the handler's
loads and stores then hit the isolated cache; only F faulted before IsC.

## How the SDK enforces it

Three layers, each catching what the one above cannot.

1. **Types.** `psx_rt::cache::with_dma_quiesced(owners, |q| { ... q.flush_icache() ... })`.
   The gate masks interrupts and waits, bounded and counted, until DMA channels 0 to 6 are all
   idle (the VBlank handler's present-queue kick is covered by the same check: it starts
   channel 2). It waits with interrupts on, masks, checks again, and backs out and waits again
   if a queued frame was kicked in between. Only then does it hand out the `Quiesced` token,
   the only way to reach a flush. The token holds `&mut` of every DMA owner token
   (`GpuDma`, `OrderingTableClearDma`, `MdecDma`, `SpuDma`, `Cd`), so starting a transfer
   from the closure through an SDK driver does not compile (there is a `compile_fail`
   doctest). `flush_instruction_cache()` and `flush_instruction_cache_range()` are the same
   gate with a closure that only flushes, so they need no tokens.
2. **Link time.** `cache-gate` (`tools/psoxide-hazard`, run by `psoxide-pgo` after every
   example link next to the hazard scan) disassembles the final image's `.text` and fails the
   build when a function outside `__psx_rt_flush_i_cache`, `__psx_rt_flush_i_cache_range` and
   `__psx_diag_fault_tramp` stores to the cache control register or sets IsC. It follows
   `lui`/`ori`/`addiu`/`addu`/`or`/`and`/`xor`/`sll` through registers, so a hand-built
   address or a copied register is caught; reading the register (the stop-panel does) is not.
   A host test also scans every SDK crate and fails if any source file other than
   `psx-rt/src/cache.rs` names the register or IsC. The register's address stays in `psx-hw`
   as a hardware map entry for the emulator.
3. **Emulator.** The emulator's tripwire hard-fails a cache control write that changes bits
   13 to 16, 3 or 7 while a DMA channel is busy, so the headless gates turn red on exactly the
   variants that failed on the console (tracked in the emulator repository).

No BIOS flush wrapper exists in the SDK (`A(44h)` is not reachable; the only BIOS call left is
`A(3Ch) putchar`). A new service that flushes internally (a BIOS exec or load call) belongs
behind the `Quiesced` token too.

A wait that runs out is never silent: it is counted (`flush_timeouts` in `psx_io::diag`,
shown on the stop-panel with `console-diag`) and the flush still happens, because stale
instructions are the worse failure and a channel wedged for that long is already a fault.

## Cost, and the real fix for hot paths

The gate costs a few control-register reads when the bus is idle. When a walk is live it
waits for it: the CPU gives up the overlap with the GPU for that frame. A path that flushes
every tick (Quake's overlay admission: two flushes per tick; OoT's leases: a range flush on
bind and on drop) therefore serialises with the GPU once per frame. That is correct and
slower; the fix is to stop flushing per tick, not to weaken the gate. `psx-residency` does that with
resident jump veneers (`PackedVeneer`, `Admission::invoke_veneered`, `overlay-compose --veneers`):
a lease stores the module entry into a data word and back, and only the image install flushes.

## On the console stop-panel

With `console-diag` the exception handler clears SR.IsC first (a fault inside a flush now
reaches the panel), the fault trampoline records the cache control register as the fault found
it and puts the running setting back before drawing, and the panel shows:

* `WILD PC pc J word MADR dma2`: the program counter is outside any code; `J` is the word that
  would jump there. A word that reads as a GPU list header, next to DMA 2's address, is the
  signature above (`hello-diag` with `HELLO_DIAG_MODE=wild` shows it);
* `FLUSH N .. WAIT .. RETRY .. TMO .. SPIN ..`: flushes, those that found a channel busy, kick
  retries, wait timeouts, most busy polls;
* `ENTRY CC .. SR .. GS ..` and `ENTRY BUSY mask CHCR2 .. FAULT CC .. ISC n`: the cache control
  register, status register and GPUSTAT as the last flush found them, the busy channels, the
  register as the fault found it and whether the fault came with the cache isolated.

## Migration

`flush_instruction_cache` and `flush_instruction_cache_range` keep their names and
signatures, so no caller changes. What changes is behaviour: they wait for DMA. Code that
wants to do more inside the quiet window (copy code, then flush, with nothing able to start a
transfer between) uses `with_dma_quiesced`; `Peripherals::dma_owners` borrows the token set
for a program that kept its `Peripherals` whole, and a program that gave the tokens to
drivers lends them back through `Gpu::dma_mut` and `SectorReader::cd_mut`.

The chain loader's own flush routine (a copy of the same sequence) is outside this crate and
needs the same wait before its cache control write; the link gate will flag it when it adopts
the gate.
