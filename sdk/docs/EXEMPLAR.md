# psx-gpu as the pattern for SDK crates

psx-gpu is the crate the others are brought in line with. This note lists
what it does and why, so a driver for the CD drive, the SPU, the MDEC or
the controller port can follow it without reading psx-gpu's source first.
The rules come from the Rust API Guidelines, the Embedded Rust Book
(singletons, typestate, DMA), the Rustonomicon and Effective Rust, and the
shapes follow embassy and rp-hal drivers; [NAMING.md](NAMING.md) is the
naming authority.

## Layers

| Layer | Crate or module | Rule |
| --- | --- | --- |
| Hardware model | `psx-hw` | Register addresses, bit layouts and command-word encoders, shared with the emulator. Nothing else declares a register value. |
| MMIO | `psx-io` | One volatile access per call, plus the ownership tokens. |
| Raw driver layer | `psx_gpu::chain`, `OtFrame::add_raw*` | `unsafe fn` wherever hardware reads or writes memory the caller names. Each has a `# Safety` section that says exactly what the caller proves. |
| Safe layer | `psx_gpu::frame`, `ordered`, `prim` | Lifetimes and owned `'static` storage prove the raw layer's contract. No safe function hands hardware an address the types do not vouch for. |
| Driver | `psx_gpu::Gpu` | The value that owns the device. Every write is a method on it. |

A reader should be able to tell which layer a function is in from its
signature alone: `unsafe` and a pointer means raw; a borrow or an owned
`'static` buffer means safe; `&mut Gpu` means it touches the device.

## A driver owns its device

```rust
#[repr(transparent)]
pub struct Gpu(GpuDma); // the psx-io token, zero-sized
```

- `Gpu::new(token, config)` takes the token by value and sets the device up;
  `release(self)` gives it back. Same shape as embassy's
  `Uart::new(p.UART0, ..)` and rp-hal's `free()`.
- Every operation that writes the device is a method taking `&mut self`.
  The borrow checker then rejects two users of one command stream; psx-gpu
  has a `compile_fail` doctest drawing inside `submit_with`'s overlap
  closure.
- Reads that cannot disturb the device (status, counters) stay free
  functions. A token buys exclusivity, and a read does not need it.
- `Gpu::from_dma_mut(&mut token)` lends the methods to code that holds the
  token inside something else (a `FramePair`, a stream after `flush`), and
  `dma_mut()` lends the token to APIs that only start DMA. Both keep the
  borrow, so nothing escapes it.
- Tokens are logic guards, not memory-safety guards. Soundness never
  depends on who holds a token: `steal()` is `unsafe` only in the "you are
  breaking an ownership invariant" sense, and every memory argument is made
  with lifetimes instead.
- Being zero-sized and `repr(transparent)`, the driver costs nothing: the
  examples built byte-identical or frame-identical against the free
  functions it replaced.

Ownership is complete inside psx-gpu only for the GPU: psx-vram, psx-font,
psx-osk and psx-fx still write GP0 without the token, and psx-io's port writes
are safe free functions anyone can call; moving them onto `&mut Gpu` (or the
token) is still to do.

The other devices follow the same shape, with the token itself or a small
driver as the owner (see [MIGRATION-ownership.md](MIGRATION-ownership.md)):
`Cd` is the CD-ROM driver (`SectorReader` and `xa::Player` hold it while they
exist), `ControllerPort` is the one SIO0 transport that `psx-pad` polls and
`psx-mc::HardwareCard` clock bytes through, `psx_spu::Spu` owns `SpuDma`,
`psx_fmv::mdec::Mdec` owns `MdecDma`, and `OrderingTableClearDma` runs the
ordering-table clear. Where a call needs the device only to read a status
register it stays a free function.

## DMA: prove the buffer outlives the transfer

The DMA controller reads and writes RAM with no regard for borrows, so the
type system has to end every borrow after the transfer ends. psx-gpu uses
three shapes, one per lifetime of the transfer:

1. **Blocking.** `OtFrame::submit(self, dma)` kicks and waits before it
   returns. Packets added to the frame are borrowed for `'f`, the frame's
   borrow of the table, so none can be dropped, moved or written meanwhile.
2. **Scoped overlap.** `submit_with(dma, || cpu_work())` kicks, runs the
   closure, then waits on every path out of it, including unwinding. The
   closure cannot reach the packets or the token: they are still borrowed.
3. **Across calls.** `FrameStorage::draw_async(&'static mut self, token, ..)`
   returns an `InFlight` that owns the storage and the token until
   `wait(self)` hands both back, as rp-hal's `Transfer::wait` does.
   Forgetting the `InFlight` leaks the storage; it can never be reused under
   the walk. Unsafe code never relies on a destructor running (Rustonomicon,
   "Leaking"), so this shape needs no `Drop` to be sound.

`FramePair` builds the usual two-buffer ping-pong from shape 3, and
`release()` hands everything back. Closures that build a frame are
`for<'f>` and receive `&mut OtFrame<'f>`, which is invariant in `'f`, so a
stack packet cannot be added to a frame that outlives the closure.

Compiler barriers sit around every kick and wait (`dma::compiler_barrier`):
stores to a buffer land before the start, and reuse after the wait cannot
move before the completion read.

## `unsafe` means undefined behaviour, nothing else

- A function is `unsafe` only if misuse can cause UB. Constructors that can
  at worst produce a wrong GP0 word are safe, with `debug_assert!` on their
  preconditions (`with_staged_slot_prepacked_colors`).
- `_raw` marks an `unsafe` low-level entry point; `_unchecked` marks the
  `unsafe` twin of a safe call minus one named check. The unchecked version
  really skips the check (`get_unchecked_mut`), behind a `debug_assert!`.
- A safe API that trusts an implementation takes an `unsafe trait`
  (`GpuPacket`, `CommandStreamDma`), the Rustonomicon's `UnsafeOrd` shape.
  A trait only the crate may implement is sealed (`StaticChain`).
- Every `unsafe` block carries a `// SAFETY:` comment that names the
  invariant, and every `unsafe fn` a `# Safety` section.
- Addresses handed to hardware are exposed explicitly
  (`expose_provenance`), and read back with `with_exposed_provenance`. A
  pointer the hardware walks from is derived from the whole buffer, never
  from one element. `make miri` runs the host tests under Stacked Borrows
  and Tree Borrows; strict provenance cannot pass by design, since a DMA
  link is a 24-bit integer on the console.

## One encoder per wire format

The packet structs in `prim` are the GP0 wire format (`repr(C)`, tag word
first). `Gpu::draw(&packet)` sends one now, `OtFrame::add(z, &mut packet)`
links it into a frame, so a primitive is encoded once whichever way it
reaches the GPU. Command words come from `psx-hw`'s builders and constants,
never from literals in the driver. The deprecated free functions forward to
the packets, and host tests check each packet against the words the old
function wrote.

## Values instead of flags

- Settings that travel together are one value: `DisplayConfig` carries the
  video standard, the resolution and the picture offset, so moving the
  picture is one field and one call, not a repeat of `init`'s arguments.
- A type that only some values make sense for exposes only those:
  `Resolution` is its presets, with private fields, so a 480-line mode
  without interlace cannot be asked for.
- Flag sets are `bitflags` (`MaskMode`), not `bool` pairs.
- Builders add one property without new scalars: `packet.translucent()`.
- End state for constructors: `#[repr(transparent)]` newtypes for colours,
  points, sizes and the CLUT and texture-page words, replacing tuples and
  loose `u8`/`u16`. That is the next planned stage; until then new APIs take
  tuples and add no loose-scalar parameters.

## Errors and recovery

Guest errors are small `Copy` types or `Option`; no formatting, no heap.
Every hardware wait is bounded and recovers (abort the walk, reset the
command buffer), and the recoveries are counted rather than silent:
`psx_gpu::recovery_stats()`. A panic is reserved for a bug the caller can
fix (a node longer than the FIFO, a double buffer that does not fit VRAM).

## Changing a public API

Follow NAMING.md's forwarder rules. psx-gpu keeps its forwarders in one
private `compat` module re-exported at the old paths, so the real modules
read as the current API. Each forwarder calls the code the new API runs,
not a copy. A re-export cannot carry a deprecation, so a moved type keeps a
plain re-export at its old path for one stage, noted where it is declared.
Every stage ships a migration note with per-game call-site counts
([MIGRATION-psx-gpu.md](MIGRATION-psx-gpu.md)).

## Evidence a change carries

- Host tests that compare against an oracle (the old encoder, psx-spx
  values, a captured frame), never a restatement of the code.
- `compile_fail` doctests for each misuse the types reject.
- `make miri` for code that hands addresses to hardware.
- Guest gates: every SDK example byte-identical, or frame-identical by
  route-tick screenshots where code moved; hot-path changes also run the
  Quake e1m1-chain-bench and NitroXide train comparison.

## Checklist for the next crate

1. Find every function that makes hardware touch caller memory: `unsafe`,
   `_raw`, `# Safety`, then a safe wrapper over borrows or owned `'static`
   storage.
2. Find every function that writes the device: move it onto a driver type
   that owns the crate's token.
3. Replace literals with `psx-hw` constants; move missing ones there.
4. Turn argument groups into values, flag pairs into `bitflags`, and
   invalid states into types that cannot hold them.
5. Count silent recoveries.
6. Deprecate with forwarders, write the migration note, and gate it.
