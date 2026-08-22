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

### v1.20 (2026-08-22, schema PX8)

Eight INFO-only RTPT hazard probes (`0xC0`-`0xC7`) settle the remaining
cross-engine scheduling disagreement without changing existing record meaning.
The first four compare the exact six-`MTC2` input load used by PSoXide and
hl-psx at +0/+1/+2/+4 instructions against a +64 settled reference. The second
four compare RTPT result reads at +0/+8/+16/+24 against a +64 reference. Every
measured sequence is one literal MIPS assembly block, with a distinct prior
triple left in both the input and output registers, so neither LLVM scheduling
nor an accidentally equal stale value can hide a hazard. These records remain
characterisation until a new console capture answers them.

### v1.19 (2026-08-07, schema PX8)

Three probes aimed at the last two conformance failures on silicon,
`0xA6`/`0xA7`, the SPU RAM round-trips. The v1.18 console capture had
already narrowed them: precision 036-038 show the readback is
self-consistent with the DMA timing override armed (single-block and
four-block hashes both 0x083B6E3D), and the boot-mode words show the
documented unstable shape, an 0xFFFF inserted at every DMA block start.
`0xA7`'s wrong readback is deterministic across two builds while
`0xA6`'s moves, which is what stale RAM under a write that never
arrived looks like.

- `0xBC` reads the same SPU RAM twice with nothing writing in between.
  A mismatch would mean the read path is unstable and nothing above it
  can be trusted.
- `0xBD` uploads the same 64 bytes by DMA and by the manual FIFO to two
  addresses and compares the two readbacks to EACH OTHER, so it tests
  the write paths without assuming the read is faithful.
- `0xBE` is the candidate fix: the same DMA upload with the transfer
  address written AFTER the mode is armed instead of before. If it
  passes on console while `0xA6` fails, the SDK's ordering is the bug
  and `psx_spu::upload_adpcm` gets the same swap.

This matters beyond the suite. psx-sfx uploads a 16-byte parking block
after every sample, and NitroXide's console recording had two voices
audibly looping theirs, which is what an upload that does not land
would sound like.

### v1.18 (2026-08-07, schema PX8)

Everything the v1.17 console capture settled, folded back in, plus the
font-atlas change that fixes the demo disc's corrupted 'f'.

- **Atlas cells are padded to a whole halfword.** `FontAtlas` laid 5-wide
  SPLEEN glyphs at 5-texel pitch, so every glyph began at an arbitrary
  nibble inside a 4bpp halfword. Cells are now `glyph_w` rounded up to 4
  texels, so each glyph starts on a halfword boundary. Nothing about the
  drawn output changes: `draw_text` still emits `glyph_w`-wide rects and
  the whole `0xB3`-`0xBA` family of hashes is byte-identical across the
  change, with only the atlas readback `0xBB` moving. Verified on the
  demo-disc launcher too: the description panel renders pixel-identical.
  8-wide fonts (BASIC) were already aligned and are untouched.

- **LZCR settle window widened to match silicon.** Conformance `0x79`
  (one nop between the LZCS write and the LZCR read) returns the PRIOR
  count on the reference console, while `0x7A`-`0x7D` (two or more nops)
  return the fresh one. The emulator settled one instruction early;
  `LZCR_RESULT_LATENCY` is now 3 and `0x79` expects the measured stale
  value rather than the ideal. This supersedes the 2026-07-15 SCPH-9902
  reading of the same window.
- **SPU key-on delay calibrated.** Every SB4 segment shows nine zero
  samples before the first envelope step, so the modelled start delay
  goes from 7 to 8 ticks.
- **The ring tap is confirmed PRE-volume.** VOICE3 ran at half volume
  against V1's quarter and both rings came back bit-identical, which a
  post-volume tap cannot produce. The probe's prose no longer calls this
  a claim.
- **Per-glyph text probes `0xB6`-`0xBA`.** `0xB3`-`0xB5` proved the
  demo-disc 'f' corruption lives in the glyph rect path (silicon's
  `0xB4` and `0xB5` agree, so the render-to-VRAM round trip is faithful),
  but an aggregate hash cannot say which glyph is wrong. These draw one
  glyph alone -- 'f' (reported bad), 'r' and 't' (same atlas row, same
  u&3==3 alignment, look fine on screen), 'o' (straddles a 16-texel
  boundary like 'f') -- plus 'f' drawn after 'r' to separate a cache
  aliasing fault from a glyph fault.

Battery is 187 cases: 138 pass, 1 fail (`0x8B`, the documented NCLIP
positive-winding gap), 48 info.

### v1.17 (2026-08-07, schema PX8)

The first version informed by a same-day console-vs-emulator diff of the
same suite build (v1.16 burn, QR decoded from video). Three kinds of
change, all conformance-semantics:

- SPU RAM round-trips `0xA6`/`0xA7` fixed: their oracle `spu_dma_read`
  read SPU RAM back with the memory controller's SPU DMA timing override
  (1F801014h bits 24-27) still at the BIOS boot value of zero, the
  documented-unstable mode whose FIFO-boundary corruption both silicon
  and the emulator faithfully produce. The read now arms the override
  and restores it after. `0xA7` additionally waits for the SPUSTAT mode
  mirror before pushing FIFO halfwords, drains before leaving
  Manual-Write, and parks TRANSFER_CTRL at NORMAL (0004h) instead of 0,
  which the SB2 finding showed poisons later sample-RAM access. These
  cases had never passed on silicon; they now pass in the emulator and
  the next burn arbitrates the console side.
- History-dependent GTE cases reclassified from conformance to INFO:
  scenes `0x50`-`0x52`, immediate-read OP `0x5C`/`0x61`, the settle
  probes `0x7E`-`0x86`, the magnitude ladder `0x87`-`0x89`, and
  controlled scene-B `0x8A`. Their compiled or settled-reference
  expectations are snapshots of one binary's GTE history (the v1.16
  console run measured 0xFFFF752C where the calibration-era binary
  measured 0x2764 on the same machine), so pass/fail carried no signal.
  The raw values still travel; `hwtest-silicon` diffs them host-side.
  Controlled scene-C `0x8B` stays conformance: silicon computes the full
  cross in both phases, so reference==probe is a real invariant there.
- Two constants re-pinned to measured silicon: OTC variants `0x0B`
  0x3F -> 0x33 (start-only kick does not complete on this console;
  SCPH-9902's 0x3F kept on record as per-model variance) and NCLIP
  winding `0x16` 0x0F -> 0x0E (positive-winding MAC0 reads 0 on
  silicon, stable across three burns).

Conformance totals move accordingly; the regenerated
`px8-emulator-v1.17.txt` baseline is the number that counts.

v1.17 adds three text-pipeline replica cases (`0xB3`-`0xB5`) for the
demo-disc "lowercase f renders as a bare crossbar" console bug: SPLEEN 5x8
uploaded at the launcher's exact geometry (4bpp atlas at 448,0, CLUT at
416,256), then the glyph pass into the 15bpp cache rect at (512,0)
raster-hashed (`0xB3`), the blit of that cache hashed (`0xB4`), and the
same text drawn directly hashed (`0xB5`). Expected values are pinned from
the emulator, where the blit round trip is pixel-exact (B4 == B5 ==
0x3B208994); on console, whichever case fails names the corrupt stage and
its observed hash is the finding.

v1.17 adds two CLUT-cache conformance cases (`0xB1`/`0xB2`, console-proven
by proxy through the demo-disc shot panel): the CLUT cache must NOT reload
when palette data is rewritten in place under an unchanged clut word, and
the 240-entry 8bpp line must survive an interleaved 4bpp draw with a
different clut word (per-line reload tracking, not a shared register).
The emulator's CLUT cache was split into the two PSX-SPX lines to match;
that is what let the demo-disc shot-colour bug reproduce headlessly.

v1.17 also adds four NCLIP-mechanism discriminators (`0xAD`-`0xB0`,
cases 174-177, all characterisation): the scene-C replica written via
SXYP (does the commit hazard track the write port?), eighth- and
sixteenth-scale magnitude rungs (where does the partial-sum regime
begin?), and a Timer 2 bracket around nclip+immediate-mfc2 (does the
documented CPU read interlock exist on this silicon at all? psx-spx and
DuckStation say reads stall until completion; the measured settle
partials suggest otherwise). None has ever run on a console; the next
burn gives them their first silicon answers, which is the missing
context for closing 0x8B properly.

### v1.16 (2026-08-06, schema PX8)

The memory-card test goes behind a consent screen. It has had limited
testing on real hardware, it reads and writes the operator's card, and
its menu row used to say SAFE, which was a promise this project cannot
make. The row now says AT OWN RISK; selecting it shows a full-screen
warning and nothing touches the card, reads included, until CIRCLE
accepts. CIRCLE rather than CROSS, so the press that selected the menu
row cannot bounce through the gate; TRIANGLE or START backs out, and
re-entering always re-asks. Verified both ways in the emulator with
route screenshots: the warning holds indefinitely without consent, and
the scan starts only after it.

No measurement changed: the v1.16 emulator capture is identical to
v1.15's (24 of 173 fail, drift=0 cross-version). Records untouched;
minor because the linked EXE changes. All three baselines regenerated.

### v1.15 (2026-08-06, schema PX8)

SB4, the capture-ring readback: the first digital tap on a voice's decoded
output. The SPU writes voice 1's and voice 3's post-envelope samples into
two 512-sample rings at SPU RAM 0x800/0xC00; the probe syncs on SPUSTAT
bit 11, keys a voice on the half-flag edge, and DMA-reads the keyed half
while the writer is in the other. Five segments: SQUARE (decode and
key-on latency), IMPULSE at half pitch (the interpolation kernel, sample
by sample), ENVRAMP (per-tick envelope stepping under a slow linear
attack), NOISE (the LFSR sequence), VOICE3 (the other ring). Per segment
the SB4 payload carries SPUSTAT, late ENVX, first-nonzero index, a CRC-32
of the 256-sample half, and the first 32 raw samples; the whole payload
also mirrors to the TTY so a headless emulator run needs no QR.

The emulator's first capture already earns the probe's keep: its
interpolation kernel reads out as 185/2131/6977/10046/6936/2103/180, its
ENVRAMP window is at full amplitude from sample 0 (hash identical to
SQUARE, so the linear attack is not applied per tick where the ring can
see it), and its NOISE segment is the constant 0x3F8B rather than an LFSR
sequence. Silicon values are pending a burn; the payload is deterministic
across emulator boots, so `sb4-emulator-*` baselines are meaningful.

PX8 records unchanged; the battery is untouched (24 of 173 fail, same as
v1.14). Machine-code and emulator baselines regenerated for v1.15.

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
