# Hardware-test suite versions

A capture is only interpretable if you know what its record ids meant when it
was taken. This file is that record.

The **suite version** and the **transport schema** are different things. The
schema (PX5/PX6/PX7/PX8) says how bytes are laid out. The suite version says
what a record id *means*. A record id can be redefined while the byte layout
stays identical, which is exactly the case a schema version cannot catch, so
both are written into every payload.

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
(`docs/hardware-refs/px8-emulator-v<version>.txt` and
`docs/hardware-refs/hwtest-machine-code-v<version>.txt`) because the version,
not the date, is what determines comparability; re-baselining the same version
overwrites rather than accumulating files, and the capture date is in the file
header. A version bump orphans both baselines until they are regenerated, and
`make hwtest-verify-code` now says so instead of crashing; v1.9 through v1.13
shipped without either, which is why no machine-code baseline exists for them.

## History

### v1.14 (2026-08-05, schema PX8)

The transport reworked around what a capture is for. Every block after the
header is optional behind a flags byte: a routine run emits verdicts plus one
record per FAILING case (383 bytes, a single QR at 173 cases), while the new
FULL CHARACTERISATION CAPTURE menu row emits every block PX7 carried, in PX7's
field order, so archived `px7-*` references still describe the same run a full
PX8 does. Failure records name their case by `TestSpec::id` rather than array
position (ids checked unique at compile time), header counts widened to u16,
pages counted from the payload, and QR symbols size themselves to it.

No record redefined, so minor: v1.8 PX7 captures remain comparable, and the
emulator baseline is unchanged from v1.8 (24 of 173 fail, 20 of them GTE
NCLIP). `hwtest-report.py` reads PX8 and the archived px7 references, and its
regression gate names a failure that appeared and one that stopped
reproducing rather than only a moved digest.

Two repairs landed after the bump (2026-08-06): the audio link had been
silently dead since this commit because the capture handed it the whole
worst-case buffer instead of the encoded slice, and its FSK frame no longer
fit SPU RAM; and `hwtest-audio-decode.py --emit-pages` still emitted a PX7
page prefix. Both fixed; `docs/hardware-refs/hwtest-machine-code-v1.14.txt`
was generated against the repaired EXE, so the pristine 2026-08-05 build
differs from it in total word count (probe spans are identical).

### v1.13 (2026-08-04, schema PX7)

The tone ladder gains three segments (the commit history calls this stretch
SB3; captures still carry the `SB2/` payload prefix), each a bug that shipped
and that nothing measured: KEYVOL (does key-on restore a voice silenced by a volume
write, the v0.11 mute-blip), RETRIG (re-key every 12 frames, the carousel
lean), XFERLIVE (upload to far SPU RAM mid-playback, the hl-psx/VoXide
streaming shape). SB2's QR moves from version 19 to 21 (667 of 711 bytes),
still behind the const asserts. Records only added.

### v1.12 (2026-08-04, schema PX7)

SB2's ladder gains the termination pair, the measurement SB2 could not make:
the repeat register read correct on 2026-08-03 while voices audibly ran past
their END block, because it says where the hardware WOULD jump, not whether it
did. PARKED and UNPARKED play identical four-block one-shots, one followed by
a self-looping silent park block, one by a loud neighbour; ENDXBIT keys voice
1 to catch a stale flag; ENVZERO reads the envelope late. These segments
report ENDX (`psx-spu` gains `voices_ended`/`clear_ended` for it) instead of
an early sample. Records only added.

### v1.11 (2026-08-03, schema PX7)

The 2026-08-03 console run drew QR ENCODE FAILED: SMALLTAB pushed SB2's
payload from 491 to 519 characters against version 17's 504. SB2's QR moves
to version 19, its size derives from the version number, and two const
asserts (capacity, screen height) make an oversized payload a build error
rather than a wasted burn.

This bump also rolls up the SB2 refinement stretch that shipped under the
v1.10 label: REPEXPL (does silicon latch the loop-start flag at all), the
64-block table (transfer size as the remaining suspect), SMALLTAB (the
32-byte control), the transfer-type-NORMAL fix, and the readable-screen
reorder. The SB2 segment list changed under one version label during
2026-08-02/03, so SB2 captures from those days must be identified by burn
date, not by version. That is exactly the failure this file exists to
prevent; bump on payload change, even mid-investigation.

### v1.10 (2026-08-02, schema PX7)

SB2, the SPU diagnostic, for the shape where Celeste's wavetables and
VoXide's bank are wrong on console while CD-DA is fine. Pass 1 uploads a
self-locating pattern and reads it back over seven upload/readback routes so
a bad upload, a bad readback, and an unstable reader are told apart; on the
emulator every word read back wrong, so pass 1 is documented as a comparison
instrument, not a verdict. Pass 2 plays a synthesised square table at known
pitches so an OBS capture measures the speaker while the QR carries the
registers. Fitting it cost CDTEST 600 to 500 sectors (opt-level changes
either crashed rustc or built a binary that never reached its menu). PX7
records unchanged.

### v1.9 (2026-08-02, schema PX7)

SB1, the UI sample end/loop probe, for the launcher browse blip that repeats
aggressively on console while every emulator plays it once. A silent audit of
the shipped ui_beep's terminator flags plus an SPU RAM readback, then four
keyed stages (the launcher path, a retrigger mash, a key-off, the percussive
preset) tracing envelope and ENDX at eight checkpoints each. Emulator
baseline: END+mute on the final block only, envelope 7FFF then 0, ENDX inside
8 frames. PX7 records unchanged.

### v1.8 (2026-08-02, schema PX7)

Added an operator-facing controller diagnostic without changing any PX7 record:

- A root-menu `CONTROLLER TEST (P1 + P2)` entry polls both controller ports
  live and identifies each as empty, digital, analog, config, or unknown.
- Every button has a persistent per-port marker: yellow while held, green once
  observed, and grey until tested. Digital pads correctly require 14 buttons;
  analog pads additionally require L3 and R3.
- Both analog sticks show raw 0-255 coordinates and live position plots. After
  the sticks remain still for 30 frames, the test samples 90 frames and reports
  the largest offset from the hardware centre value `0x80`: 0-8 pass, 9-16
  warning, and greater than 16 fail. Moving either stick automatically restarts
  the sample.
- START remains testable. Holding START+SELECT for 45 frames on either port
  returns to the menu.

The conformance schema and record meanings are unchanged. The minor version was
bumped because adding the screen changes the linked executable against which
machine-code and emulator timing baselines are pinned. This also corrects the
previous payload/display mismatch: the v1.7 display string shipped while the
two payload version bytes still encoded v1.6.

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
