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
comparable**, and `psoxide-hwtest report` refuses the diff unless
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

### v2.4 (2026-10-09, schema PX8)

MINOR: five new measurements the emulator work asked for, a pad question, and a self-check on the record list. No existing record changes what it means. The SDK pin is now `1832928d0` (the pad integration branch with the five review fixes); the emulator pin is `af6747947`. Steps: 66 (61 before), records: about 1,000 (788), `TIMING_RECORD_COUNT` 1,100.

- 480i draw rule (`src/field_rule.rs`, step `480I DRAW RULE`, the last safe step of the run, `0x800`-`0x852`): in GP1(08h) 480-line interlaced, with GP0(E1h) bit 10 clear, which rows of a 64 x 16 scratch area do a fill, a rectangle, two triangles, a textured quad and a line leave untouched, with GPUSTAT bit 31 at 0 and at 1; and does a VRAM copy or an upload skip them. Also with bit 10 set, with the scratch area outside the displayed rectangle, in 480 progressive, 240 interlaced and 24bpp modes, with a texpage word that sets bit 10, with the draw-area top and the draw offset on an odd row, with the display start on an odd row, a fill issued at once after VBlank, the clocks of a 256 x 240 fill and rectangle with the rule on and off, and the same commands with the display blanked. Every record carries its own descriptor.
- GPU list stopped mid-node and block-mode DMA into a full FIFO (`src/dma_edge.rs`, step `GPU DMA EDGE CASES`, `0x860`-`0x873`): a large rectangle, then an upload node of 48 words and a second upload node; CHCR START is cleared 6,000, 20,000 and 60,000 clocks after the kick (and not at all as the control); the words that landed say how much of the node reached the GPU and whether the walk went on. A request-mode transfer of 64 and 256 words (blocks of 16, and 8) behind the same rectangle, with and without it, counts the words that landed and the clocks it took.
- Memory-card protocol (`src/card_proto.rs`, step `CARD SECTOR PROTOCOL`, `0x880`-`0x8BF`): per slot, a 140-byte sector read four times with every byte's `/ACK` rise, width and arrival, the checksum and the terminator, the replies; with L1 and R1 held, a 138-byte write of the same bytes to the same sector, twice. An empty slot records the unanswered first byte.
- Whole-workload calibration (`src/workload.rs`, step `WORKLOAD CALIBRATION`, `0x8C0`-`0x8C5`): a fixed scene of 256 textured triangles from a linked list and a fixed block of CPU work, timed alone, together, and in VBlank-locked frame loops, on the system clock extended to 32 bits.
- Texture UV windows (`src/workload.rs`, step `TEXTURE UV WINDOWS`, `0x8E0`-`0x92B`): sixteen textured triangles per row with the UVs swept over a 256 x 256 page at 4, 8 and 15 bits (spans 8 to 255 texels, origins moved, the texture window shrunk to 8 to 64 texels); each row is a timing record and a record that says what the row was.
- Pads (`rumble.rs`, `0x940`-`0x962`): the motor step asks which pad is in the port (CROSS SCPH-1200, CIRCLE SCPH-110, SQUARE another, TRIANGLE none), runs the battery, then asks the operator to swap the pad and name the new one; an answer runs the whole battery again into a second set of records (the first set's ids plus `0x1E0`). No answer to either question is recorded and the run goes on.
- Card write safety (review fixes): the sector read and written is frame 40 (an unused frame of block 0), never the header; the write is gated on all four reads being good with matching checksums, no error flag and identical data, with the reason recorded in `0x89D`/`0x8BD` bits 1 to 5. The QR checkpoint pages are now armed by START+R2 at the start, apart from the L1+R1 card-write chord (which also arms the older pad-engine card-lease write-back, about sixty frames of block 0 rewritten with the bytes just read). `GPU DMA EDGE CASES` runs after `WORKLOAD CALIBRATION` and `TEXTURE UV WINDOWS`. The pad prompts fit the screen.
- CD chain variants (`0x500`-`0x507`): the third field was the drive-state word shifted down by 8, 24 bits wide, so it read 0xFFFF in every capture. It is now the index/status register and the IRQ flag (16 bits) and the CD status byte rides in bits 8 to 15 of the first field. The only existing records whose values change meaning; the old third field carried nothing.
- Record integrity (`0x41E`, `0x41F`): before the capture is encoded the run counts record ids pushed twice, fields clamped to 16 bits and records that did not fit, and names the first and last clamped id and the first duplicate (`0x41F`); the host verdict names a non-zero count. `hwtest-report.py --v24` prints the new measurements and checks that each record describes the case it should.

### v2.3 (2026-10-09, schema PX8)

MINOR: harness fixes for what the v2.2 console run showed. Records are added; three existing ones change what they report, named below (`0x767`/`0x768`, the first field of `0x41C`, and the handoff flag bit 1), because the old values were measuring the harness rather than the machine.

- Motor questions (`rumble.rs`): the v2.2 answers looked shifted by one prompt (a CROSS landed at once on `LARGE 0`). Each question now stays up for a second with the pad ignored, then waits for every button to be let go, then takes the next press (four seconds for both). A button still held after that is a no answer, never an answer. New records `0x771`-`0x773` (the raw button word of each answer, then a mask of questions whose buttons were never let go) and `0x774`-`0x776` (the frame each answer arrived on, counted from the question appearing, then a mask of questions that saw a button down during the first second). Also written, as `FFFF`, when no pad takes the config packets.
- Poll cost (`0x767`/`0x768`): now the cycles from the first byte written to the last byte received. v2.2 recorded the transaction total, which is the last byte's unanswered wait (15.9k idle and with motors on, the harness's own timeout).
- Handoff baseline (`0x41C`): the interrupt mask is compared on its eleven source bits (0x7FF) on both sides. The v2.2 console read the baseline with the upper half set, the later reads with it clear, so every area was flagged 0x3D and the summary said to look at record `410`. The first field of `0x41C` is the masked baseline; the full 32-bit words are in `0x41D` (baseline low half, baseline high half, the last handoff's high half), and the per-area mask in `0x410`-`0x419` is masked too.
- Summary screen: the RUN ID on the cover is the id the page headers carry, fixed when the capture is encoded. v2.2 mixed the vblank count into it every time it was drawn.
- SPU RAM precondition (`run.rs`): SPU RAM is not cleared at power on and a previous program's data stays through a disc swap. At the start of the SPU area the run now zeroes SPU RAM from 0x1010 up with the DMA upload and records three regions before (`0x777`) and after (`0x778`), each folded to 16 bits. The conformance cases that write and read back (`0xBE`, `0xBF`) and SB1/SB2 report what they read, and for a write that does not land that is whatever was there: v2.2 moved `0xBE`/`0xBF` (38FD746D, B0526890; v1.24, v1.28 and v2.1 had DB9A456D, 53C8E737) and records `431`, `481`, `493`. They should now agree from run to run. The emulator starts with zeroed SPU RAM, so its values do not move.

### v2.2 (2026-10-09, schema PX8)

MINOR: records are only added; every existing record measures what it did, except where the first item below stops two probes from measuring a runaway. Pins the SDK pad integration branch (`integ/pad-2026-10-08`) and drives the motors through its motor API again.

- DMA BCR (`perf_probes.rs`, `list_busy_probes.rs`, `gpu_probes.rs`): the overlap probes set BCR once and then kicked the channel twice per call and many calls per record. Silicon counts a block-mode BCR down as the blocks go, so every kick after the first started with a count of 0, which the controller runs as 65,536 blocks; that is the v2.1 hang in `DMA VERSUS CPU LOADS` (SPU and GPU block channels never idle, BCR high half 0xFF06 and 0xFEEE, MADR hundreds of blocks past the source). Every probe now writes BCR inside the pass, ahead of the timed span, and the linked-list kicks rewrite `size_words(0)` before each kick. One deliberate case, `0x7A0`-`0x7A2`, kicks the SPU channel again without writing BCR and records the count after the first transfer, what the second did and how far it got before the counted bound stopped it.
- DualShock motors (`rumble.rs`): the config packets are a frame apart, Enter Config is tried up to four times (v2.1: id 0xFF, no 0x5A on the console), and its reply bytes, the attempts and the model query's reply are recorded (`0x769`-`0x76C`, `0x76F`-`0x770`) and shown on the screen. The operator part uses `psx_pad::enable_rumble_on` and `poll_rumble_on`.
- Pad identity (`0x76D`-`0x76E`): a plain poll's id, 0x5A, button and stick bytes per port, on the screen for a second and a half, so a film says which pad it was.
- Handoff baseline (`0x41C`): the IRQ mask and DPCR the handoffs are compared with (v2.1 reported flag 0x3D in all ten areas with no baseline to read it against).
- Card frame reads (`0x6D0`-`0x6DB`): for each of the four mix passes the first failed frame read, its kind, the SDK transport fault and the exchange it happened at, and the first ten response bytes the SDK kept (v2.1: 3 of 24 and 2 of 24 frames failed with protocol errors under GPU load). The reads themselves are unchanged.

### v2.1 (2026-10-09, schema PX8)

Fix for a silicon hang in `DMA VERSUS CPU LOADS` (area 9, step 56): `dma_overlap_probe!` waited on CHCR bit 24 with no bound, so a channel that never cleared START froze the disc.

- Every device wait is now counter-bounded. DMA probes: 4,000 iterations before the kick and 20,000 after (about 20 cycles each, so roughly 40x the longest legitimate SPU transfer). SPUSTAT mode match: 100,000. `dma_matrix`: 50,000. CD and cdstream: 60,000,000 / 30,000,000 spins. MDEC: 200,000. List-busy: 2,000,000.
- A timeout is a measurement. The channel is stopped (CHCR=0, SPUCNT mode 0, GP1 direction 0) and `0x780-0x787` record flags (bit0 busy before kick, bit1 busy after loop, bit3 timed out), CHCR high half, device status, MADR and BCR. `0x788` records the SPUSTAT-match wait before an SPU kick. `0x790-0x799` record CD/MDEC DMA state.
- The record id is drawn on screen before each record starts, so a hang names the probe.
- Hold SELECT at any time to skip the current step (waits scale to 1 iteration).
- Hold L1+R1 at boot to also show a QR checkpoint page at the end of each area (opt-in, also enables card write-back).
- Emulator fault injection: `PSOXIDE_WEDGE_DMA=<channel mask>` leaves CHCR START latched; a run with SPU and OTC wedged (0x50) completes and records timeouts.

### v2.0 (2026-10-08, schema PX8)

MAJOR: the suite is one linear run (`src/run.rs`, [hardware-test-disc.md](hardware-test-disc.md)), ten areas in a fixed order with a reset at the start of each and a handoff record at the end. Every menu screen that was a separate probe is now a step of the run, except the controller test and the memory-card diagnostic, which need a person. What was removed and where each thing went is [hardware-test-v2-removed.md](hardware-test-v2-removed.md). Shared record ids keep their meaning, so a v1.28 silicon capture still compares record by record (`make hwtest-compare` does it), but a record's value can move because the state it starts from is now defined (a reset per area) and the order is different.

New in the run:

* Area handoff records `410`-`419`, final silence proof `41A` (flags `7F` silent), run info `41B`; boot snapshot `400`-`404`.
* Folded probes (records unchanged): SB1 `430`-`436`, SB2 `440`-`486`, SB4 `490`-`49E`, PA4 `4A0`-`4AD` (two variants), PA1 `510`-`514`, CL2 `500`-`507`, the XA loop, kernel timing, display widths, 480i interlace, the three CD stream cases. A short MDEC decode check, `420`-`426`, replaces the MDEC diagnostic and the 75 s FMV stream playback.
* Measurements that exist because the emulator could not settle them (records and fields in [hardware-test-disc.md](hardware-test-disc.md)):
  * Controller port: select-to-first-byte in cycles (`600`-`601`); `/ACK` latency and width per byte of a nine-byte pad poll and a four-byte card command, per port (`610`-`628`, `6C0`-`6CB`, answers `630`-`635`); pad and card taking turns, idle and under GPU load (`640`-`64F`); the SDK pad engine's setup sweep, Ack and Timed pacing, card lease and load behaviour with the generator's own interrupt state (`660`-`693`, `720`-`749`); a hot-plug window (`638`-`63B`); and the DualShock motors, config packets, mapping, poll cost and an operator yes or no per motor level (`760`-`768`).
  * The emulator timing audit's list M1 to M11, to its definitions: GTE early reads and `swc2`/`cfc2`/`lwc2` (`192`-`195`), MMIO read costs (`1A0`-`1A3`), scratchpad sub-word (`1A8`-`1AD`), the isolated cache (`1B0`-`1B5`), the CPU/DMA matrix with real CD (channel 3) and MDEC (channels 0 and 1) transfers (`700`-`709`, `1E0`-`1E7`, `140`-`145`), cold code after the evictor (`1B8`-`1BC`), BIOS ROM and expansion loads (`1C0`-`1CB`), texture state (`200`-`223`), the multiply/divide unit (`1D0`-`1D8`), and polled-counter tick loss (`6B0`-`6B7`, with `650`-`652` for Timer 1 itself). GTE latency of 22 commands read back with `mfc2` and back to back is `150`-`191`.
* `tools/hwtest-report.py --compare`, `--check-silence` and `--emulator-baseline`; `tools/hwtest-video-qr.py` groups pages by run id; `make hwtest-run`, `hwtest-diff`, `hwtest-compare`, `hwtest-baseline`.

Bug fixed on the way: **SB2 never ran `begin_step` for segment 0 in v1.28 and earlier**, so its tables were never uploaded and the tone records of silicon captures taken before v2.0 (the SB2 tone rows) measured whatever the SPU RAM already held. They are not comparable with v2.0's.

The end-of-run noise Manny heard at the end of v1.28 was the payload-as-audio FSK tone that every capture started; the generator is deleted.

Pins: SDK 73ab7ef7 (main; carries `psx-pad`'s `irq-engine`, used by the pad-engine steps) and emulator c743674 (CD timing calibrated to the v1.28 silicon capture, SWC2 interlock, Timer 1 polling fix). The emulator run is 61 steps, 13 QR pages and 728 records (the page cap is 24 and the record slots 800); its conformance count is 149 pass, 2 fail, 83 info: cases 139 (`NCLIP controlled scene-C +2`) and 201 (`GTE vs IRQ, return to EPC: RTPS intact`), both failing in the v1.25 emulator baseline too (see below), so they are known emulator gaps and not an effect of the new order. On silicon (v1.28) 139 passes with a settled reference that is itself partial (`0xF3A`) and 201 fails.

### v1.28 (2026-10-08, schema PX8)

Three `CD STREAM` cases join `CONSOLE TESTS`, with `CD STREAM, ALL THREE` to run them in turn (`src/cdstream_cases.rs`; details in [hardware-test-disc.md](hardware-test-disc.md)). They measure the SDK's streaming transport, `psx-cdstream`, which the polled CD records never run, for milestone M0 of [streaming-design-2026-10-08.md](streaming-design-2026-10-08.md):

* `CD STREAM COST`: a sustained read through the transport at double and single speed. Sectors a second, foreground CPU lost to the sector pops (microseconds per sector and share), longest handler call (Timer 2), interrupts per sector, first-sector time, chained and discarded sectors.
* `CD-DA HANDOFF`: lease request to granted while a read runs; Pause to idle after CD-DA (`T_pause`); first data sector after audio; Play after SetLoc to PLAYING and whether the position continued from the saved one. Then a read that starts while the tone plays, with and without the transport's recovery Pause, judged from the drive's PLAYING bit, GetlocP advancing and the SPU CD-input capture buffer sampled during the read, so no listening is needed.
* `CD MOTOR`: a read after a Pause and a wait of 0, 5 and 15 s; a read right after Stop; a read once the motor has stopped.

New timing-block records, present only once a case has run: `2F0`-`2F7` cost, `300`-`30B` hand-off, `310`-`315` motor (layouts in each function's doc and `tools/hwtest-report.py` `cdstream_rows`). `TIMING_RECORD_COUNT` grows to 380. No existing record changed meaning, hence MINOR.

Three things changed under the suite and are worth knowing when comparing captures:

* **CDTEST shrank from 500 to 460 sectors** (it went from 600 to 500 earlier): the boot EXE needs room for the transport. `CDTEST.BIN` now starts at LBA 564 instead of 524, so `cd_chain_probe.rs`, `lever_probes.rs` and the new cases name 564; `MOVIE.STR`, `HWSONGS.XA` and the CD-DA track keep their LBAs. The EXE is 508 of 542 sectors. Records `90`-`C7` read at LBA 424 and are untouched.
* **The SDK pin moves to 2be27c140**, a commit on the SDK branch `feat/cdstream-probe` (not on SDK main), because that is where `psx-cdstream` lives. The pin is 57 commits past the previous one (38ba81f0e); `fmv_diag.rs` only needed its MDEC setup functions to take the new `&mut Mdec` driver argument. That SDK deletes `OrderingTable::iter_packets`, which the pinned emulator's `psx-gpu-render` still calls, so the emulator pin has to move (to d44685a or later) before the editor frontend builds against it.
* The machine-code baseline for v1.28 is pinned; shared timing records may move with the new code layout and SDK, as at every bump.

### v1.27 (2026-10-04, schema PX8)

`CONSOLE TESTS (V1.27)` joins the main menu, after TARGETED PROBES, as a page of four cases for one console session (`src/console_tests.rs`, `kernel_timing.rs`, `display_widths.rs`, `xa_loop.rs`; details in [hardware-test-disc.md](hardware-test-disc.md)):

* `KERNEL TIMING (BIOS)`: EnterCriticalSection and ExitCriticalSection (SYSCALL 1 and 2), and a VBlank interrupt round trip, in system-clock cycles from root counter 2, with the exception vector the BIOS left (snapshotted in `main`) put back for the measurement. The runtime's own VBlank handler is measured for reference.
* `DISPLAY WIDTHS`: an edge-marker pattern at 256, 320, 368, 384, 512 and 640 pixels through `psx_gpu::display::DisplayConfig` (368 and 384 add raw GP1 writes, which `DisplayConfig` has no preset for).
* `480I INTERLACE`: a 640x480 interlaced pattern through `DisplayConfig::R640X480`, with GPUSTAT bits and field-parity changes counted at VBlank.
* `XA MUSIC LOOP`: `HWSONGS.XA` (generated tones) looped through `psx_io::cd::xa::Player`, with the restart gap per loop and whether GetlocP keeps updating.

The disc gains `HWSONGS.XA` after `MOVIE.STR` (468 sectors), which moves the CD-DA track outward by that much. `CDTEST.BIN` and `MOVIE.STR` keep their LBAs. The SDK is pinned at ae6e1ef10. The boot EXE grows to 493 of its 502 sectors.

New timing-block records, present only once a case has run, as for the FMV test: `2C0`-`2C5` kernel timing, `2D0`-`2D5` one per display width, `2D6`-`2D7` interlace, `2E0`-`2E3` XA loop (layouts in each module's `records`, decoded by ``psoxide-hwtest report`` `console_rows`). `TIMING_RECORD_COUNT` grows to 354. No existing record changed meaning, hence MINOR.

### v1.26 (2026-09-26, schema PX8)

v1.25's FMV test never started on a PAL SCPH-9002: the SDK player's DMA0
upload of the MDEC tables did not finish ("mdec tables", record `1F5`), while
PSoXide and a SuperStation One played the movie. v1.26 adds `MDEC
DIAGNOSTIC` as MAIN MENU's second-to-last row (two UP from row 0; `FMV STREAM
TEST` stays last) and runs the same diagnostic first when FMV STREAM TEST
finds none (`src/fmv_diag.rs`):

* Six MDEC setup sequences, eight runs each (runs 1, 3, 5, 7 from an idle MDEC,
  runs 2, 4, 6, 8 reset in the middle of a table command, run 1 of A from
  whatever the console held), each followed by a one-macroblock DMA0/DMA1 probe decode:
  A the v1.25 driver verbatim (then a late enable write, to test whether the
  first was lost), B settle on not-busy then enable, C a fixed delay, D
  CPU-written tables, E PSn00bSDK's order, F the SDK driver on this build.
* MDEC1 traced over time after a reset from idle, from busy, and from busy
  with the enable written straight after (reset latency, timer-stamped).
* A one-frame control: the movie's first frame decoded by CPU writes and
  reads, and again over DMA0/DMA1; equal word sums mean both paths agree.
  PSoXide only decodes the first macroblock on the CPU path, so the CPU half
  fails in the emulator; it is aimed at the console.

FMV STREAM TEST then plays a 15-second cut (1,965 video sectors) behind each
sequence that worked, the chosen one first (the SDK driver when it held every
run). The pass criteria apply to the cut. Every result goes on screen and into
the capture, which is now encoded after either row even without a timing scan,
and the QR pages open straight after. New timing-block ids, all packed
halfword streams rather than timings (layout in `fmv_diag::records`, decoded
by ``psoxide-hwtest report``): `200`-`25D` per sequence, `260` overview,
`270`-`287` playbacks, `290`-`29E` reset traces, `2A0`-`2A2` the control
decode. `1F0`-`1F5` keep their meaning and describe the first playback.
TIMING_RECORD_COUNT grows to 336 and CAPTURE_PAGE_MAX to 12.

No existing record changed meaning, hence MINOR.

### v1.25 (2026-09-25, schema PX8)

The FMV console test joins the disc as MAIN MENU's last row, `FMV STREAM
TEST`, so one burn carries it with everything else. The player is the SDK's
`hello-fmv`, run as a library, with its overlay, summary screen and pass
criteria unchanged; the disc gains `MOVIE.STR` (raw CD-XA sectors, `mkisopsx
--xa-file`) after CDTEST.BIN, which moves the CD-DA track outward. Its result
travels in the capture as six new timing-block records, `1F0`-`1F5`, present
only once the test has run (see [hardware-test-disc.md](hardware-test-disc.md)).

No existing record changed meaning, hence MINOR. Every probe's machine code is
identical to v1.24 (`hwtest-verify-code`, drift 0), and every conformance
verdict matches the same source built without the FMV test, captured on the
same emulator. Against the v1.24 baseline one case moves: `0xC9` now fails,
because the emulator has since modelled an interrupt on a GTE command running
it twice, as the v1.24 console showed; the v1.24 image fails it the same way on
today's emulator. `0x9B` is not comparable with v1.24 (the CD-DA track moved).

### v1.24 (2026-09-23, schema PX8)

Console gates for three performance levers that measured well in the emulator
and each rest on something only silicon can answer (`src/lever_probes.rs`,
described in [hardware-test-disc.md](hardware-test-disc.md)). Eleven
conformance cases, `0xC8`-`0xD2`, appended to the battery (indices 200-210, so
RESUME FROM TEST at 200 runs these, the list-busy cases below and the timing
scan), and four timing records, `137`-`13A`, in a new `LEVERS` table that runs
with every timing scan. Then, before any burn, 23 more cases, `0xD3`-`0xE9`
(indices 211-233, `src/list_busy_probes.rs`), for the lever those three do not
settle:

* `0xC8`-`0xCB`, GTE vs IRQ: whether an interrupt taken on a GTE command runs
  it twice when the handler returns to EPC (psx-rt's behaviour), and whether
  psx-spx's fix (step EPC over a GTE command) removes that without losing one.
* `0xCC`-`0xCF`, the present queue: quake-psx a29ca4d's VBlank-driven flip and
  DMA kick for 120 frames of uneven GPU load, checking at every flip that
  GPUSTAT bit 28 did not report idle before the previous chain's GP0(1Fh).
* `0xD0`-`0xD2` and `137`-`13A`, the scratchpad stack: hello-spstack's workload
  on a scratchpad stack (the SDK trampoline vendored, since the pinned SDK
  predates it) against the RAM stack, under Timer 2 and VBlank interrupts, GPU
  and SPU DMA, a CD read stream and pad polling; and one `level2` call timed on
  each stack, idle and during a linked-list DMA.
* `0xD3`-`0xE9`, list busy: how long GPU DMA channel 2 stays busy (CHCR bit
  24) on a linked list whose nodes draw. Four lists ending in GP0(1Fh) (16
  empty nodes; 16 tiny Gouraud triangles; the same 16 packets as half-screen
  triangles; those four to a 24-word node), each stamped from the kick at
  CHCR clear, GP0(1Fh), and GPUSTAT bits 28 and 26 settling; whether the
  packed list draws the same pixels (the only PASS/FAIL case); and a fixed
  loop of ALU ops, RAM loads or scratchpad loads counted through the walk
  against the same loop idle. The emulator's default model clears CHCR on a
  word-count formula and its FIFO model when the drawing nearly drains, and
  an hl-psx optimisation is worth +21% under one and nothing under the other.

No existing record changed meaning, hence MINOR. The conformance capture diffs
clean against the v1.23 emulator baseline (`drift=0`, the only failure still
`0x8B`). Two harness changes came with it: `CAPTURE_PAGE_MAX` goes from 9 to
10, because the worst-case payload with every case failing no longer fits nine
pages (the emulator's full characterisation capture grows from five pages to
six), and `HWTEST_STEPS` from 400M to 480M, because the headless conformance
capture now completes between 420M and 430M instructions. `psoxide-hwtest report`
names the new records and no longer indexes past the end of a baseline with
fewer cases. The list-busy cases take the worst-case payload to 9.54 pages of
the 10, leave the emulator's full characterisation capture at six pages and the
headless conformance capture still complete by 430M instructions; the
conformance baseline was re-pinned after they were added (`drift=0` against the
pin without them), and `psoxide-hwtest report` prints them as a labelled
`list_busy` table.

### v1.23 (2026-09-17, schema PX8 with the TIMING_EXT block)

Follows the first v1.22 console captures. Record ids widen to sixteen bits;
ids from `0x100` travel in a new last block, so every earlier offset and every
archived capture is untouched. `0x100`-`0x114` are GPU batches submitted as a
DMA list that ends in a GP0(1Fh) interrupt request, replacing the v1.22
sweep's GPU records (`0xBA`-`0xBF`, `0x3B`, `0x38`, `0xCB`, `0xF6`), whose
unpaced GP0 writes overflowed the FIFO on silicon. `0x120`-`0x136` fill in the
shapes v1.22 measured one point of: the multiply interlock, the load shadow,
the write queue, whether a GTE read waits for its command, and the I-cache
alias pair from a cached caller. No surviving record changed meaning.

### v1.22 (2026-09-17, schema PX8)

The performance sweep: 59 more records pricing techniques rather than checking
behaviour, listed in [hardware-test-disc.md](hardware-test-disc.md). Warm GTE
command latencies and the GTE gap sweep, coprocessor register moves, a lerp on
the CPU against GPF, multiply behind multiply, unaligned and narrow data
accesses, the write queue and load scheduling, loading the next RTPT's inputs
behind the current one, I/O port costs, empty ordering-table slots and whether
the CPU runs during a linked-list DMA (with and without RAM loads), GPU cases
the fill battery left out, triangle setup, CLUT reload and letterboxing, and two more
register A/B pairs (`RAM_SIZE` bit 7 on a cold sweep of code that loads data;
a shorter SPU bus read delay).

They run from two TARGETED PROBES rows, `PERF SWEEP (SAFE)` and `PERF A/B (MAY
HANG)`, not with the standing battery, whose records are unchanged. Every
record id from `00` to `FE` is now allocated; the next probe needs a second id
byte or a retired id, which is a schema question.

Also: SB2 mirrors its payload to the TTY, SB1 and CL2 no longer overprint the
harness header, and the probes share one copy of their payload plumbing.

### v1.21 (2026-09-17, schema PX8)

Performance probes, all under a warm harness (`src/perf_probes.rs`, described
in [hardware-test-disc.md](hardware-test-disc.md)). Records `0x72`-`0x8D` run
with every timing scan: warm twins of the core CPU records, the MULT/DIV gap
sweep, MULT operand dependence, and the I-cache 4 KiB alias pair. Records
`0xDC`-`0xEC` flip `RAM_SIZE` bit 7 and cache-control bits 13-17 around a fixed
workload; they can hang a console, so they run only from `TARGETED PROBES >
PERF A/B (MAY HANG)`.

Why a warm harness: the older CPU records carry a layout-dependent I-cache
refill tax (a commit that touched no probe moved 104 of 151 emulator minima),
so they are not comparable across builds. No existing record changed meaning,
hence MINOR.

Also in this version, none of it changing a record: probe markers became
unique `ori`-to-zero words and the machine-code audit discovers them from the
image (the six GTE command probes are audited for the first time; the 19
previously pinned digests are unchanged); the timing block sends only filled
records; the memory-control block gains `RAM_SIZE` and cache control; and the
timing sampler is one shared body, which took the EXE from 1,003,520 bytes to 751,616 with the new probes included.

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
NCLIP). `psoxide-hwtest report` reads PX8 and the archived px7 references, and its
regression gate names a failure that appeared and one that stopped
reproducing rather than only a moved digest.

Two repairs landed after the bump (2026-08-06): the audio link had been
silently dead since this commit because the capture handed it the whole
worst-case buffer instead of the encoded slice, and its FSK frame no longer
fit SPU RAM; and `psoxide-hwtest audio-decode --emit-pages` still emitted a PX7
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

### v1.7 (2026-07-31, schema PX7)

The CD-DA contention pair (`0x9B`/`0x9C`) normalises with PAUSE instead of
STOP. The 2026-07-31 console run proved STOP plus respin grinds the mechanism
for minutes right after the contention read; with the motor kept up, `0x9C`
measures the read it always claimed to measure instead of a spin-up. Also: the
scan draws the in-flight record id as bit-cells, and START skips mid-record.

### v1.6 (2026-07-28, schema PX7)

Menu-first boot and the shared memory-card hardware diagnostic. The record
schema is unchanged, but linking the diagnostic moves timing code, so
machine-code and emulator timing baselines are pinned to this binary rather
than compared byte-for-byte with v1.5.

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
(`psoxide-hwtest report` still parses PX5 and PX6) but their timing records must not
be diffed against v1.x.
