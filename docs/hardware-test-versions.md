# Hardware-test suite versions

A capture is only interpretable if you know what its record ids meant when it
was taken. This file is that record.

The **suite version** and the **transport schema** are different things. The
schema (PX5/PX6/PX7) says how bytes are laid out. The suite version says what a
record id *means*. A record id can be redefined while the byte layout stays
identical, which is exactly the case a schema version cannot catch, so both are
written into every payload.

## Bump rule

**MAJOR** when an existing record's meaning changes: a probe redefined, a clock
swapped, sampling semantics altered. Captures across a MAJOR boundary are **not
comparable**, and `hwtest-report.py` refuses the diff unless
`--allow-suite-mismatch` is passed.

**MINOR** when records are only added, or a bug is fixed that leaves every
existing record measuring the same thing. Shared records stay comparable and the
tool notes the difference without failing.

The version lives in `SUITE_VERSION_MAJOR` / `SUITE_VERSION_MINOR` in
`engine/examples/hardware-tests/src/main.rs`, with `SUITE_VERSION` as the
display string. Baseline files are named by version
(`docs/hardware-refs/px7-emulator-v<version>.txt`) because the version, not the
date, is what determines comparability; re-baselining the same version
overwrites rather than accumulating files, and the capture date is in the file
header.

## History

### v1.1 (2026-07-26, schema PX7)

Operator flow only; no record changed meaning, so v1.0 captures remain
comparable for every record. The guest binary changed though, and timing records
shift with code alignment, so the baseline was re-pinned.

- Boot runs the battery behind a visible progress bar, then lands on the capture
  pages by itself. Previously the battery ran with nothing on screen and progress
  went only to the debug TTY, which does not exist on a console: the operator saw
  a black screen for tens of seconds and reasonably concluded the disc was dead.
- A two-level main menu (START from anywhere, printed on every screen) reruns
  the startup tests or opens results, scans and probes. It replaces a flat mode
  ring that had to be cycled blindly, and every page fits on screen without
  scrolling. All 21 modes are reachable, and the audio readout is a listed row
  rather than only a hidden button.
- The audio readout is silent until SQUARE asks for it, and plays at a quarter
  scale rather than full. It used to start automatically at maximum volume.

### v1.0 (2026-07-26, schema PX7)

The suite became a measurement instrument rather than a conformance checker.
**Not comparable with v0.18**: sampling method changed, so every timing record
means something different.

Measurement method:
- Samples are taken with **interrupts masked**. Previously the only defence
  against an IRQ landing inside a measured window was that one of five repeats
  happened to escape it, so the old min/max gap reported "did an interrupt hit"
  rather than silicon jitter. This alone makes v0.18 timing incomparable.
- Records gained a **median**, so a spread caused by one stray event is
  distinguishable from a genuinely bimodal distribution.

New batteries:
- CD-ROM `0x90`-`0x9A` and CD-DA contention `0x9B`-`0x9E`, timed on Timer 1's
  HBlank clock and waiting for the drive's completion IRQ rather than its ack.
  The disc now carries a synthetic CD-DA track so contention can be probed.
- GPU fill rate and texture cache `0xA0`-`0xAF`.
- MDEC `0xB0`-`0xB5` (table loads, reset settle, decode-to-drained).
- SIO pad pacing `0xB6`-`0xB9`.
- Console/BIOS identity and 22 bit-exact raster hashes in precision `128`-`159`.

Transport:
- PX7: explicit per-record ids, so an unused slot is skippable and adding a
  probe cannot shift the meaning of later records. Five QR pages.
- Audio readout: the payload streams out of the SPU as binary FSK, looped in
  hardware, at four operator-selectable rates.

Tiering:
- Tier-2 probes (PA2-PA5) arm on menu entry instead of at boot. The disc
  previously booted into PA5, whose `spu::init()` ran before every capture.

### v0.18 and earlier (schema PX5/PX6)

Conformance battery plus CPU/GTE/DMA timing, sampled **without** interrupt
masking and reported as min/max only. Historic captures remain readable
(`hwtest-report.py` still parses PX5 and PX6) but their timing records must not
be diffed against v1.x.
