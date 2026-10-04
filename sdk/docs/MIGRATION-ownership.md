# Migrating to the device owners

This SDK stage makes `psx_rt::Peripherals` mean something. Until now only
`GpuDma` was taken by anything (psx-gpu's `Gpu` owns it); the CD, the
controller port, the SPU's and MDEC's DMA channels and the ordering-table
clear had tokens that nothing required. Now every driver that programs one of
those devices takes its token, so the borrow checker sees two drivers that
would fight over it.

Everything a game calls today still compiles. The old free functions are
deprecated forwarders that steal the token for the call, so a repin brings
warnings, not errors. They are removed once no game's main uses them
([NAMING.md](NAMING.md), "Renaming without breaking games"). One naming rule
is new: a function that existed without a device argument and now takes the
owner gets the suffix `_on` (`poll_on`, `tick_on`, `upload_on`), because Rust
cannot overload `poll()` with `poll(&mut port)`.

Caller counts below are lines of working-tree `*.rs` under each repo, as of
2026-10-04, excluding `target`, vendored and preserved copies, tests, and the
editor's own copy of the SDK. The patterns are in the heading of each section.

## Getting the tokens

```rust
let p = psx_rt::Peripherals::take().expect("peripherals are taken once");
let mut gpu = psx_gpu::Gpu::new(p.gpu_dma, display);
let mut port = p.controller_port;          // pads and memory cards borrow it
let mut cd = p.cd;                         // or hand it to a SectorReader / Player
let mut spu = psx_spu::Spu::new(p.spu_dma);
let mut mdec = psx_fmv::mdec::Mdec::new(p.mdec_dma);
```

[`psx_io::periph`](../crates/psx-io/src/periph.rs) lists who owns each token
and how others borrow it. A token that an owner holds comes back with its
`release()`.

## CD-ROM (`Cd`)

Every command and register step in `psx_io::cd` is a method on `&mut Cd`:

| Old | New |
| --- | --- |
| `cd::command(c, p)`, `command_within`, `try_command` | `cd.command(c, p)`, `cd.command_within(..)`, `cd.try_command(..)` |
| `cd::status`, `try_status`, `set_mode`, `try_set_mode`, `try_set_target_lba`, `try_start_reading` | the same names as methods |
| `cd::play_track`, `try_play_track`, `pause`, `try_pause`, `try_pause_until_complete`, `stop`, `try_stop`, `stop_and_settle` | the same names as methods |
| `cd::mute`, `unmute`, `try_mute`, `try_unmute`, `play_position`, `try_play_position`, `try_command_until_complete`, `set_audio_mixer` | the same names as methods |
| `cd::irq_flag_value`, `acknowledge_irq`, `discard_response`, `dispatch_command`, `restore_irq_output`, `poll_data_sector`, `try_wait_data_sector` | the same names as methods |
| `psx_io::cdrom::*` (the module that was already deprecated) | forwards to the methods |
| `PlaybackStarter::tick(now, track)` | `tick_on(&mut cd, now, track)` |
| `PlaybackClock::tick(now)` | `tick_on(&mut cd, now)` |

`cd::bin_to_bcd`, `bcd_to_bin`, `lba_to_bcd_msf` and the `Response`,
`PlayPosition` types do not touch the device and stay free.

`xa::Player` already took `Cd`; it now drives the drive through it (it used to
hold the token and ignore it).

### One driver for data sectors: `SectorReader` moves to psx-io

`psx_pack::cd::SectorReader` was a second driver for the controller with its
own register map. It is now `psx_io::cd::reader::SectorReader`, holds the `Cd`
token, and drives the controller through the same register steps as every
other CD user. `psx_pack::cd::SectorReader` stays as a plain re-export for one
stage (a re-export cannot carry a deprecation). `psx-pack` keeps `find_entry`,
`load_chunk` and `load_chunk_decompressed`.

| Old | New |
| --- | --- |
| `SectorReader::new()` (const, usable in a `static`) | `SectorReader::with_cd(cd)` (const too) |
| `unsafe { reader.prepare() }`, `start_read`, `read_sector`, `try_read_sector`, `stop`, `set_filter`, `unmute`, `prepare_mode`, `prepare_single_speed`, `start_read_seek_first` | the same methods, now safe; an `unsafe` block around one draws an unused-unsafe warning |
| (none) | `reader.release()` gives the token back; `reader.cd_mut()` lends it for a command outside the reader's sequences |

Behaviour changes a repin sees:

- **`I_MASK` is restored.** `prepare` used to rewrite `I_MASK` to VBlank-only
  and leave it that way for good. The reader now keeps the caller's mask and
  `stop()` puts it back, after its last acknowledge. `prepare` and each
  `start_read` set the VBlank-only mask again, so every
  `prepare`/`start_read` to `stop` span is bracketed. A game that relied on
  the mask staying VBlank-only after a load now keeps its own mask; nitroxide's
  `load_arena_texture` already saved and restored it by hand and can drop that.
- **No DMA channel 3 enable.** `prepare` wrote DPCR to enable channel 3 for a
  path that reads sectors by CPU. That write is gone; nothing in the SDK uses
  channel 3.
- **`dispatch_command`'s doc** said it returned `bool`; it returns
  `Option<u8>`. The behaviour did not change.

### Call sites

CD free functions (`cdrom::try_*`, `cd::*` calls; constants like
`cdrom::MODE_CDDA` are not counted): wipeout-psx 4
(`game/src/gameplay.rs`), nitroxide 8 (`game/src/music.rs`), voxide 0,
hk-psx 30 (`game/src/{music,audio_probe,cd_stream}.rs`), hl-psx 12
(`game/src/main.rs`), cs-psx 12 (`game/src/cd_irq.rs`), quake-psx 4
(`game/src/music.rs`), the editor engine 68
(`engine/examples/hardware-tests/src/{main,audio_probe,lever_probes}.rs`,
`engine/crates/psx-engine/src/game_app.rs`,
`engine/examples/game-magikaaaaaarp-pong/src/main.rs`).

CD-DA starter and clock `tick` calls: wipeout-psx 1, nitroxide 1, quake-psx 1,
the editor engine 5 (`engine/crates/psx-engine/src/scheduler.rs`,
`engine/examples/editor-playtest/src/playtest_update.rs`).

`SectorReader`: nitroxide 3 (`game/src/assets.rs`), voxide 2
(`game/src/sfx.rs`), hk-psx 6 (`game/src/{disc,cd_stream}.rs`), hl-psx 2 and
cs-psx 3 (`game/src/cdstream.rs`, plus `cd_irq.rs`), quake-psx 4
(`game/src/{platform,music}.rs`), the editor engine 28
(`engine/crates/psx-chainloader/src/runtime.rs`,
`engine/examples/hardware-tests/src/{cd_chain_probe,lever_probes,fmv_test}.rs`).
hl-psx and cs-psx hand `SectorReader::new()` to `CachedStreamer::new` inside a
`static mut` initialiser, which has no token to give. Those two sites are the
one that cannot be a one-line change: make the static an `Option` filled once
from `Peripherals` (`hello-pack` does this for its reader), or keep
`SectorReader::new()` for the stage the deprecation allows.

## Controller port (`ControllerPort`): one SIO0 transport

A pad and a memory card share SIO0, so they now share one transport,
`psx_io::controller_port::Transport` (implemented by `ControllerPort`): select
a socket, clock a byte and wait for the device's `/ACK` pulse, release. The
pad's ACK-paced timing, tuned on an original SCPH-1200, a clone and an
SCPH-110, is the reference; `psx-mc` uses the same exchange.

The pad and card entry points take `&mut` of the token (generically, any
`Transport`, which is how host tests run the real code against a controller or
card model):

| Old | New |
| --- | --- |
| `poll_port1()`, `poll_port2()` | `poll_on(&mut port, Port::One / Port::Two)` |
| `poll_port1_raw(pacing)`, `poll_port2_raw` | `poll_raw_on(&mut port, socket, pacing)` |
| `poll_port1_diagnostics(setup, gap)`, `poll_port1_diag` | `poll_diagnostics_on(&mut port, socket, setup, gap)` |
| `enable_analog_port1()`, `enable_analog_port2()` | `enable_analog_on(&mut port, socket)` |
| `require_analog_port1()`, `require_analog_port2()` | `require_analog_on(&mut port, socket)` |
| `reader.poll()` (`PadReader`) | `reader.poll_on(&mut port)` |
| `HardwareCard::new(slot)`, `with_timing(slot, timing)` | `HardwareCard::on_port(port, slot)`, `on_port_with_timing(port, slot, timing)` |
| `psx_settings::save_slot_one(..)`, `load_slot_one(..)`, `format_and_save_slot_one(..)` | `save_to_slot_one(&mut port, ..)`, `load_from_slot_one(&mut port, ..)`, `format_and_save_to_slot_one(&mut port, ..)` |

`HardwareCard<P = ControllerPort>` holds the port as the token itself (so
`Card<HardwareCard>`, the type games already write, still names it) or as
`&mut ControllerPort` when the program keeps polling pads between saves.
`release()` takes the port back; `controller_port()` lends it between frame
transactions. `psx_mc::Slot` is now an alias of `psx_io::controller_port::Port`
(`Slot::One` still works).

Behaviour change a repin sees, and the one that needs a console: **the memory
card used to arm the `/ACK` interrupt in CTRL and wait on the STAT latch; it
now watches the live `/ACK` level like the pad does**, and a failed frame ends
with a UART reset. The card keeps its own, much longer `/ACK` budget
(`Timing::CARD`, 200,000 spins against the pad's 2,048) for the flash commit.
The emulator reads the same card bytes either way. Its headless scan ran one or
two steps behind `main` between ticks 100 and 500 (the new pacing waits for the
`/ACK` release before every byte) and level with it from tick 600; the card
image after the guarded write is byte-identical. Nothing here is proven on
silicon.

Call sites, pads (`poll_port*`, `*_analog_port*`, `.poll()`; the `.poll()`
pattern also matches other types, so read these as an upper bound): wipeout-psx
1 (`game/src/main.rs`), nitroxide 4 (`game/src/main.rs`), voxide 11
(`game/src/{main,tunelab}.rs`), hk-psx 5 (`game/src/{menu,input}.rs`, tests),
hl-psx 14 (`game/src/{main,menu}.rs`), cs-psx 16 (`game/src/{main,menu}.rs`),
quake-psx 6 (`game/src/{input,intro,quake}.rs`), the editor engine 32
(`engine/crates/psx-engine/src/app.rs`,
`engine/examples/hardware-tests/src/{main,controller_test,fmv_test,lever_probes}.rs`).

Call sites, cards and settings (`HardwareCard::new`, `*_slot_one`): nitroxide 2
(`game/src/main.rs`), voxide 6 (`game/src/{save,main}.rs`), hk-psx 3
(`game/src/save.rs`), hl-psx 1 (`game/src/save.rs`), the editor engine 10
(`engine/examples/game-{pong,magikaaaaaarp-pong,invaders,breakout}/src/main.rs`,
`engine/examples/editor-playtest/src/playtest_runtime.rs`). psxcel
(`game/src/main.rs`, 2 `HardwareCard::new`) is outside the list above but also
calls it.

## SPU (`SpuDma`)

`psx_spu::Spu` owns `SpuDma`. The voice, volume and reverb registers stay free
functions (writing one cannot touch RAM); what needs the owner is what copies
memory into sound RAM.

| Old | New |
| --- | --- |
| `psx_spu::init()` | `Spu::new(spu_dma)` (the same reset, then the silence block) |
| `psx_spu::upload_adpcm(dest, bytes)` | `spu.upload_adpcm(dest, bytes)` |
| `psx_sfx::Bank::upload(psau)` | `bank.upload_on(&mut spu, psau)` |

The DMA transfer itself is now `SpuDma::write_blocks(words, block_words)` in
psx-io: it checks the shape (non-empty, whole blocks, a 16-bit block count),
borrows the slice until the channel finishes or is aborted, and psx-spu no
longer has an unsafe DMA start.

Call sites (`spu::init()`, `upload_adpcm`, `Bank::new`/`bank.upload`):
wipeout-psx 3 (`game/src/gameplay.rs`), nitroxide 3 (`game/src/audio.rs`),
voxide 4 (`game/src/sfx.rs`), hk-psx 11
(`game/src/{audio,ambience,scene_sfx,runner_audio,geo_audio,focus_audio,audio_stream}.rs`),
hl-psx 0, cs-psx 0, quake-psx 4 (`game/src/{quake,audio}.rs`), the editor
engine 65 (mostly `engine/examples/hardware-tests/src/*_probe.rs` and
`engine/crates/psx-goldsrc/src/hsfx.rs`).

## MDEC (`MdecDma`)

`psx_fmv::mdec::Mdec` owns the two MDEC DMA channels. `reset`, `load_tables`,
`load_tables_cpu`, `write_command`, `read_data`, `decode_start`, `decode`,
`read_column` and `decode_finish` are methods; the free functions are
deprecated forwarders, and `mdec::status()` stays free. `decode` hands the
driver back to its column closure (`|mdec| mdec.read_column(..)`), which is
where `read_column` now lives. The module no longer needs the MIPS cfg.

Call sites: only the editor engine's `engine/examples/hardware-tests/src/fmv_diag.rs`
(2 direct calls) and, through `hello-fmv`, its `Options { setup }`,
`frame_control(dma_setup)` and `mdec_setup`. Those three now take or receive
`&mut Mdec`, so `fmv_diag.rs`'s `setup_fn` and `hello_fmv::mdec_setup`
references need the new signature on repin.

## Ordering-table clear (`OrderingTableClearDma`)

`dma::clear_ordering_table(buf)` is `OrderingTableClearDma::clear_table(&mut
self, buf)`. psx-gpu's `OrderingTable::clear_with_dma` already took the token
and now runs the clear through it. Call sites: the editor engine's
`engine/examples/hardware-tests/src/main.rs` (1).

## DPCR updates are atomic with respect to the VBlank handler (io-05)

`dma::enable_channel` read, ORed and wrote DPCR with interrupts on; the
present queue's VBlank handler sets the GPU channel's bit in the same
register. It now runs the update inside `psx_io::irq::without_interrupts`. The
SR read-modify-write moved from psx-rt into `psx_io::irq`
(`disable_cpu_interrupts`, `restore_cpu_interrupts`); psx-rt's critical
sections use the same pair. Code size grows by the six instructions of the
mask where `enable_channel` is inlined (hello-ot gains ten lines of
disassembly), so an example that calls it is no longer byte-identical.

## psx-asset: HMA1 key widths

The decoder derives a field width from the key-byte count in the blob
(`(1 << (2 * key_bytes)) - 1`). `Model::new` already rejects a count above
four, so the shift cannot overflow for any model it hands out; the test now
checks every count from 0 to 255 and the decode site has a debug assertion
that names the check.

## Not migrated yet

- `hello-fmv` is migrated for the CD, SPU and MDEC; its GPU token is stolen at
  the entry of `run_with`, as before, because the hardware-test suite runs it
  after other diagnostics that took the set already.
- Pads and cards need a token where games poll from deep inside their loops
  (hl-psx and cs-psx poll from `main.rs`). Carry `ControllerPort` in the game
  state beside the `Gpu`.
- `psx-vram`, `psx-font`, `psx-osk` and `psx-fx` still write GP0 without
  `&mut Gpu` (see [EXEMPLAR.md](EXEMPLAR.md)).
- The voice and reverb registers of the SPU, the root counters and the
  interrupt controller have no owner; they cannot move memory.

## What to test on a console

None of this is proven on silicon. In the headless emulator every SDK example
runs frame for frame equal to `main` (`hello-input`, `hello-cdda`, `hello-xa`,
`hello-pack`, `hello-fmv` without its movie and the rest), except the
`hello-memcard` scan progress described above; its write, readback and
cold-boot readback pass and leave the same card image. But the emulator answers
`/ACK` and the CD instantly. On a console, in this order:

1. **hello-memcard**, an official card and a third-party card, slot 1: the
   scan finishes with zero read errors and the same hash on the second pass; the
   guarded write (L1+R1, Cross) passes write and readback; power-cycle and
   cold-boot readback passes. This is the one that exercises the new card
   pacing. Watch the transport page for `AckTimeout` or `AckReleaseTimeout`
   (the 200,000-spin card budget is a guess from the old default).
2. **hello-memcard with no card** and **a pad on the same boot**: the card
   reports `NoCard`, the pad still reads buttons afterwards (the failed frame
   now ends with a UART reset), and an unplugged then replugged card is picked
   up.
3. **hello-input** on an SCPH-1200, a clone and an SCPH-110: unchanged
   production path, expected identical, analog lock still takes.
4. **A game's save and load** (nitroxide settings, voxide save menu) on a
   real card, to cover the settings forwarders and `Card<HardwareCard>`.
5. **hello-pack** with the `WORLD.PAK` fixture. The generator
   `tools/hello_pack_fixture.py` is not in this checkout; rebuild the pack from
   86 chunk files named `chunk_0000.bin` to `chunk_0085.bin` (ids 0 to 83
   sixteen bytes each, 84 the raw pattern, 85 the HLZC-framed run pattern,
   both as the example computes them) with `mkisopsx --world-pack-extra-dir`.
   Expect ALL PASS including the new `IRQ MASK RESTORED` line, then a
   chain-loader or game chunk load with a timer or pad IRQ enabled, to see
   that source still fires after the load.
6. **hello-cdda** and **hello-xa**: CD-DA starts and pauses, XA streams and
   loops, the `Player` now driving the drive through its token.
7. **The demo-disc chain loader** (single speed, `start_read_seek_first`),
   because it is the strictest user of the reader: every payload's RAM
   checksum against the disc build.
8. **hello-fmv** with a movie (sector reader, XA, the `Mdec` driver) and
   **hello-audio** (SPU DMA upload with the masked DPCR update).
