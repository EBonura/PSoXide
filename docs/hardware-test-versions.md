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

### v1.5 (2026-07-26, schema PX7)

Two sweeps, added to settle findings the first complete capture raised but could
not answer. Records only added, so v1.4 captures stay comparable.

- **Seek sweep** (`0xC0`-`0xC5`) fills in 2, 4, 8, 32, 64 and 256 sectors between
  the four original distances, which came back non-monotonic (+128 measured 361
  ms against +512 at 192 ms) and defeated both a linear and a square-root fit.
  Ten distances make an outlier visible as an outlier rather than as the shape
  of the curve.
- **Backward seeks** (`0xC6`, `0xC7`) at 64 and 256 sectors. Every existing seek
  record approaches from below, so a direction asymmetry would be invisible.
- **SIO setup-delay sweep** (`0xD0`-`0xDB`), twelve delays from 0 to 1536. The
  console answered at setup 0, gave no reply at 128, and answered again at 384;
  a threshold cannot be read off that, and this is the SCPH-1200 pad problem
  stated as a measurement.

Record slots raised 144 to 176, which still fits five QR pages (3,820 of 4,140
characters).

### v1.4 (2026-07-26, schema PX7)

The capture is frozen when it is taken. **This is the fix that makes multi-page
QR capture possible at all.**

Paging previously called `encode_capture`, which rebuilt the entire payload from
current state. Some observations are live: `results[PAD_POLL_TEST_INDEX]` is
refreshed from the controller every frame in `update`. So page 1's QR encoded
one payload, page 5's encoded another, and the whole-binary CRC stored in page 5
described only page 5's version. Every page decoded cleanly and reconstruction
always failed, which is exactly what three console captures did.

Paging now re-renders the QR from the frozen payload (`PhotoCapture::render_page`)
and only a genuinely new measurement re-encodes. Verified headlessly by paging
with pad pulses and reconstructing from pages rendered at different times: CRC
valid.

### v1.3 (2026-07-26, schema PX7)

Audio readout holds its level for the whole payload. No record changed meaning;
baseline re-pinned because the binary changed.

v1.2 keyed the readout voice with `Adsr::passthrough()`, an all-zero ADSR. Zero
means sustain level 0, so on hardware the envelope decays to silence shortly
after key-on: a console recording carried about 3 seconds of a 13.6 second
payload, twice, and neither repetition was complete. PSoXide holds the level
indefinitely for the same configuration, so no amount of emulator testing could
have shown it. See `emulator-accuracy-from-silicon.md`.

The disc now uses `Adsr::sample()`: instant attack, sustain level maximum.

### v1.2 (2026-07-26, schema PX7)

Audio readout on by default. No record changed meaning; the baseline was
re-pinned because the guest binary changed.

Off-by-default cost a console session. The operator has no reason to know a
silent disc is withholding the payload, and with the readout off the only route
left was the QR pages, which then lost a symbol: four of five decoded from the
recording and page 4 was unreadable in every frame it appeared in. One missing
symbol costs the whole capture. The original complaint was the VOLUME, which
v1.1 already fixed, not the readout existing.

SQUARE still steps rate and off, for dropping to a slower rate when a chain
cannot decode the fastest.

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
