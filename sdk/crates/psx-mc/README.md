# psx-mc

A PlayStation 1 memory-card driver for the PSoXide SDK: SIO0 transport, the
standard on-card filesystem format (interoperable with the console's card
manager), and optional save compression. `no_std`,
allocation-free.

## Layers

Three layers, each usable on its own:

1. **Transport**: [`Block`] exposes a card as 1024 raw 128-byte frames.
   `HardwareCard` drives a physical card over SIO0 (feature `hw`, default on);
   `RamCard` is a 128 KiB in-memory image for host tests and virtual saves.
2. **Filesystem**: `Card<B: Block>` implements the real PS1 directory: named
   files, block allocation with the link-chain, BIOS-visible title + icon
   frames, and per-frame XOR checksums. Saves appear in the console's memory-card
   manager like a retail game.
3. **Container**: `Card::write` wraps payloads in a small self-describing header
   so `Card::read` transparently handles plain and (feature `compress`) compressed
   saves.

## Quickstart

```rust
use psx_mc::{Card, HardwareCard, Slot};

let mut card = Card::new(HardwareCard::new(Slot::One));
if !card.is_formatted()? {
    card.format()?;
}
card.write("BESLES-00000MYGAME01", "MY GAME", &save_bytes)?;

let mut buf = [0u8; 8192];
let len = card.read("BESLES-00000MYGAME01", &mut buf)?;
```

Names are the BIOS file name (region+product code + label, ≤ 20 ASCII); the
title is the human-readable label the card manager shows (≤ 32 ASCII).

## Compression (feature `compress`)

```rust
let mut scratch = [0u8; 8192];
card.write_compressed("BESLES-00000MYGAME01", "MY GAME", &save_bytes, &mut scratch)?;
// read() auto-detects and decompresses; no flag needed.
```

A small LZSS codec, chosen because it is tiny and decodes using only the output
buffer as its history window, so a compressed read streams straight into the
caller's buffer with no scratch. If compression does not shrink the data it is
stored verbatim, so `write_compressed` never makes a save larger.

## Features

| feature | default | effect |
| --- | --- | --- |
| `hw` | yes | compile the on-device SIO0 `HardwareCard` (pulls `psx-io`) |
| `compress` | no | LZSS `write_compressed` + transparent decompression |

Host tests run the pure logic without hardware:

```sh
cargo test -p psx-mc --no-default-features --features compress
```

## Verification

- **Filesystem + compression**: 17 host unit tests against `RamCard` (CRUD,
  multi-block chains, overwrite/delete, capacity, checksums, image reopen,
  compressed round-trips).
- **SIO0 transport**: the `hello-memcard` example (`make hello-memcard`) runs
  format → write → read → verify (plain and compressed) against a real card and
  paints `ALL PASS`; verified green under the PSoXide emulator's card model.
- **On-silicon timing** is console-pending. The per-byte `/ACK` waits and the
  post-select setup delay are conservative defaults exposed as a runtime knob
  (`HardwareCard::with_timing`); tune `Timing::ack_spins` up first if a real
  card's write commit is slow.
