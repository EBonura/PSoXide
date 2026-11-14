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

Note (2026-11-14, the first console run of a disc built with `console-diag`): a test disc (hardware-tests v2.18, built from branches that had not landed) stopped on the stop panel at boot on a console (SCPH-9002): "PSXDIAG FAULT HALT ... CAUSE C000001C, EPC at the store loop of `psx_rt::tty::print`", exception code 7, a data bus error on the first byte stored to the Expansion 2 debug console (`0x1F802080`). Since 2026-11-03 (`psx-rt` 381be68a1) `tty` stores to that port directly, and since 2026-11-07 `hardware-tests-disc` is built with `console-diag`, which implied `host-tty`; no disc built after both had been run on a console. The entry state gives Expansion 2 128 bytes (`0x0007_0777`) and the port is at offset `0x80`, outside it. The emulator does not model the error, so every emulator run passed. From this commit `console-diag` does not imply `host-tty`, the stop panel draws on screen and needs no tty, and `make hardware-tests-disc` fails when the built executable can reach Expansion 2 beyond its first 128 bytes (`psoxide-hwtest scan-expansion2`). A run that needs the terminal (headless emulator runs read the `hardware-tests: rec` lines) uses a twin build with `HWTEST_FEATURES="console-diag psx-rt/host-tty"` (`make hardware-tests-disc HWTEST_FEATURES="console-diag psx-rt/host-tty" HWTEST_ALLOW_DEBUG_PORT=1`); the twin is never burned.

### v2.19 (2026-11-14, schema PX8)

MINOR. Records only retired, none redefined: a console passed step `CD STOP AT ACKNOWLEDGE` (SCPH-9002, 2026-11-14, two runs, see v2.14), so `SectorReader::stop` returns at the acknowledge, the wait for the second response is deleted, and the step goes with it. Retired record ids, not reused: `0x6F8`-`0x6FB`. The decoder keeps their names so a capture of v2.14 to v2.18 still reads. `SectorReader::stop_waiting`, which ran both sequences on one build for the step, is deleted; `src/cd_stop.rs` is gone and the step `PRESENT UNDER CD AND FLUSH`, which borrowed its reader, owns one. `PRESENT UNDER CD AND FLUSH` now ends its reads with the shipped `stop`, which no longer waits, so its time figures change and its sector, error and retry fields do not. Every other record measures what it did in v2.18.

### v2.18 (2026-11-14, schema PX8)

MINOR. Records only added: step `PRESENT UNDER CD AND FLUSH` (`src/present_path.rs`, CD area, records `0x974`-`0x976`), the one present path while the drive delivers sectors and across instruction-cache flushes: the two ways a direct GPU access or a cache flush racing a queued walk has frozen a console before. `0x974`: 24 reads of 1 to 8 sectors from `CDTEST.BIN` at scattered sectors with nothing presented, the control; fields: sectors read correctly, `errors << 8 \| retries`, 0. `0x975`: the same reads with a chain frame handed to the presenter between the sectors of each read (whenever its slot is free, so the thread and the VBlank handler kick while the drive streams) and a frame drawn through the port after each read; fields: sectors, `errors << 8 \| retries`, and `tears << 8 \| recoveries << 4 \| lost` (lost is 1 when a frame was not flipped exactly once or a part not kicked exactly once). `0x976`: 30 chain frames, each followed at once by an instruction-cache flush while it waits in the slot or walks, and a drawn frame after each, followed by another flush; fields: frames flipped, parts kicked, `tears << 8 \| recoveries << 4 \| faults`. Pass: `0x975` reads as many sectors as `0x974` with no more errors or retries and its third field 0; `0x976` flips every frame (twice its chain frames) and kicks every chain frame once, third field 0. Fail: a wrong or missing sector, a retry or error the control does not have, a tear, a recovered stall, or a fault: the present path disturbs the drive, or a flush races a walk, on this machine. Emulator (this suite plus this change, 150 pass and the two known fails 139 and 201): `0x974` 108, 0, 0; `0x975` 108, 0, 0 (88 frames, 64 chain parts, all flipped and kicked); `0x976` 60, 30, 0. Every other record measures what it did in v2.17.

### v2.17 (2026-11-14, schema PX8)

MINOR. Records only added: step `PRESENT ONE PATH` (`src/present_path.rs`, records `0x970`-`0x973`), the one present path (`psx_rt::present`, `sdk/docs/PRESENT.md`) on the console's own GPU, DMA and VBlank timing. v2.15 is the pad handshake step and v2.16 the pad first poll step. 120 frames go through the presenter and psx-rt's VBlank handler with the cap lifted: frames of one chain, frames of two (`Part::More`, `Part::Final`), every fifth frame drawn through the GP0 port and handed over with `present_drawn`, under a CPU load that varies from frame to frame, so some frames are kicked by the thread at submit and some by the handler at an edge. Each frame paints a marker in its buffer in a colour of its own. A VBlank hook records the flips and kicks at each edge and the Timer 1 line of each flip's pass, and counts a tear when a frame started before the one before it was on screen or a walk ran while the display showed its buffer. `0x970`: frames flipped, parts kicked, `tears << 8 \| recoveries << 4 \| bad markers`; pass: 120 frames flipped, every part kicked once (128), the third field 0. Fail: a lost or doubled frame or part, a tear, a stall the presenter recovered, or a buffer whose marker is not its frame's: the handler or the thread-side kick is wrong on this machine. `0x971` (information): the earliest and latest Timer 1 line of a pass that flipped, and the most lines in a frame; on a console a spread of a few lines means every flip landed at the same point of the blank, a wide one that the handler ran late. The emulator does not catch Timer 1 up when it is read, so its value is not a beam position. `0x972` (information): parts the thread kicked, parts the handler kicked, edges on which a part waited. `0x973`: frames that waited past their edge, the most VBlanks from submit to flip, and the draw-done fallbacks the handler made; pass on a console: 0 late frames and 0 fallbacks. On an FPGA core that loses GP0(1Fh) the fallbacks count the frames shown without GPUSTAT bit 24 and each such frame shows one VBlank later, so late frames up to that count are expected there; any other late frame, or fallbacks on a console, is a fail. Emulator (this suite on SDK 34e7b36d plus this change, 150 pass and the two known fails 139 and 201): `0x970` 120, 128, 0; `0x971` 103, 237, 241; `0x972` 94, 34, 0; `0x973` 0, 2, 0. Every other record measures what it did in v2.16.

### v2.16 (2026-11-14, schema PX8)

MINOR. Records only added: step `PAD DIGITAL FIRST POLL` (`src/digital_first.rs`, SIO area, records `0x6FC`-`0x6FE`), how soon `psx_pad::Controllers` reads a DualShock from its first poll. PCSX-ReARMed's DualShock acknowledges the address byte and answers `0xFF` to the command byte until about 33 video frames have gone by, which is what an empty socket does on that port, and `Controllers` left a port that answered so alone for 59 polls: a game on `Controllers::digital()` ignored the pad for 120 polls (its first button read at the 121st; `Controllers::new()` the same), while the earlier poll found it at the 33rd. `Controllers` now polls a port that has not answered every time for its first 128 answers of "nobody there" (and again for 128 after a pad leaves), then one poll in 60 as before. No existing step would have shown this on a console: `PAD CONFIG UNDER IRQ` uses `Controllers::new()`, makes the pad digital and polls once, so it only sees the first answer. Each case is two rounds; a round puts the pad in the case's mode, makes a fresh driver and polls port 1 once a video frame, 130 times, noting the first poll that read a connected pad. `0x6FC`: `Controllers::digital()`, pad digital. `0x6FD`: `Controllers::digital()`, pad analog (it sends no handshake and reads what the pad answers). `0x6FE`: `Controllers::new()`, pad digital (the first poll runs the handshake). Pass (a DualShock in port 1): every round read from the first poll in all three cases (`rounds` `0x0002`, `first_poll_worst` 0), `id_and_mode` `0x4101` for `0x6FC` and `0x7302` for `0x6FD` and `0x6FE`. Fail: any round read late or never; `first_poll_worst` then says at which poll, and past 128 it is a pad that stays silent longer than the early probes cover. A digital-only pad passes `0x6FC` and `0x6FE` with `0x4101` and cannot enter analog mode (`rounds` bits 12-15) in `0x6FD`. Emulator (this suite on this change, `--dualshock-pad`, 150 pass and the two known fails 139 and 201): `0x6FC` `first_poll_worst` 0, `id_and_mode` `0x4101`, `rounds` `0x0002`; `0x6FD` 0, `0x7302`, `0x0002`; `0x6FE` 0, `0x7302`, `0x0002`. The emulator's DualShock answers from the first poll, so it cannot show the ReARMed case (a pad silent for its first 33 frames); on it the step passes with and without the fix. Every other record measures what it did in v2.15. The machine-code baseline is pinned (`docs/hardware-refs/hwtest-machine-code-v2.16.txt`).

### v2.15 (2026-11-13, schema PX8)

MINOR. Records only added: step `PAD CONFIG UNDER IRQ` (`src/config_irq.rs`, SIO area, records `0x6F5`-`0x6F7`), three cases for a pad left digital by an interrupted analog handshake. `psx_pad::Controllers` sends the handshake (enter configuration, set analog and lock, exit) at a pad's first poll, with CPU interrupts on in most games, reads none of the answers and repeats no packet. Three games that carried console-found protection against a pad left digital for the session (the handshake run before interrupts were enabled, a request repeated while the pad read digital) lost it when they moved onto `Controllers`. The history is in `docs/PAD.md`: the failures were seen on the earlier synchronous drivers (VoXide 2026-07-06, Quake before 2026-08-25), never on a console with today's ACK-paced one, which no console has run. `Controllers` now masks CPU interrupts from the first byte to the last of every configuration packet (`Transport::mask_interrupts`) and asks again, at most three times in all, a pad that answered a packet in full (only a DualShock does) and still reports digital, unknown or configuration mode. The step is the console's answer to whether the mechanism is real and whether the protection holds. Each case is fifteen rounds; a round puts the pad in digital mode, unlocked, makes a fresh `Controllers` and polls port 1 once. The handshake is `Controllers`' own code on the real port; the transport under it records the bytes and, in `0x6F5` and `0x6F6`, starts root counter 1 on the system clock just before the byte after the command `0x44` is written, so that its interrupt lands 1,000 cycles later. The handler (`psx_rt::dispatch`) spins 1,600 cycles of root counter 0 and returns. The pad's `/ACK` rises 1,608 to 1,686 cycles after a byte's write and lasts about 92 (`0x610`-`0x618`, v2.8 console capture), so the handler runs from about 1,100 to 2,700 after the write, over the whole pulse with 500 cycles of margin before it and 900 after: the worst case of an interrupt over the pulse, where a VBlank handler would meet it only some of the time. `0x6F5`: nothing is masked (`Transport::mask_interrupts` left as a model has it), so the handler hides the pulse. `0x6F6`: `Controllers` as shipped, the interrupt waits and the handler runs after the packet. `0x6F7`: no forced interrupt, the plain first-poll handshake with VBlank interrupts on, as every game starts. Pass (a DualShock in port 1): `0x6F7` and `0x6F6` fifteen rounds analog, one set-analog packet a round, ID `0x73` straight after (`bytes_seen` `0x73F3`, `rounds` `0xF00F`, `packets` bits 4-7 one, bits 8-11 nine, bits 12-15 fifteen), and `0x6F6` with one handler run a round. `0x6F5` is a measurement with two readings: the packet is lost (`bytes_seen` `0x41F3`, three bytes written, a second packet, fifteen rounds analog in the end) and the mechanism is real, or it is not (`0x73F3`, nine bytes, one packet) and an interrupt over the pulse does not hide it on this console, in which case the masking is insurance only. Fail: any round of `0x6F6` or `0x6F7` ending digital or needing a second packet; then the masking does not protect the packet, or the console's pad does not answer a handshake the way the driver expects, and the retry is all that stands. A digital-only pad ends every round digital (`rounds` `0xF0F0`, ID `0x41`) in all three cases; the step is for a DualShock. Emulator (this suite on SDK 3b7ef1256 plus this change, DualShock in port 1, 150 pass and the two known fails 139 and 201): `0x6F5` rounds `0xF00F`, `bytes_seen` `0x41F3`, `packets` `0x0321` (one handler run, two packets, three bytes of the first, none analog at once); `0x6F6` `0xF00F`, `0x73F3`, `0xF911`; `0x6F7` `0xF00F`, `0x73F3`, `0xF910`. Every other record measures what it did in v2.14. The machine-code baseline is pinned (`docs/hardware-refs/hwtest-machine-code-v2.15.txt`, drift 0 against v2.14).

### v2.14 (2026-11-13, schema PX8)

MINOR. Records only added: step `CD STOP AT ACKNOWLEDGE` (`src/cd_stop.rs`, records `0x6F8`-`0x6FB`), four pass-or-fail cases for `psx_io::cd::reader::SectorReader::stop`, which sends Pause and then waits 16,384 spins (about 296,000 clocks) for a second response that comes 1.2 to 3.9 million clocks after the acknowledge, so the wait always runs out and a load of many short reads pays about 8.9 ms a read. `STOP_AT_ACKNOWLEDGE` (off) makes `stop` return at the acknowledge; the next read's Setloc then reaches a drive still completing the Pause, and only a GetStat is known to cancel the second response there. Each case is forty reads of `CDTEST.BIN` at scattered sectors, every sector checked against the pattern, a failed read tried once more after a proved stop. `0x6F8`: reads of 1 to 8 sectors, `stop` waiting as shipped. `0x6F9`: the same reads, `stop` returning at the acknowledge. `0x6FA`, `0x6FB`: one-sector reads, waiting and returning early. Pass: `0x6F9` and `0x6FB` read as many sectors correctly as `0x6F8` and `0x6FA` (all of them) with no errors and no retries, and take less time, by about 8.9 ms a read (40 reads, about 360 ms; the time field is in units of 16 HBlanks, about a millisecond). Fail: any error, retry or wrong sector in `0x6F9` or `0x6FB` that `0x6F8` and `0x6FA` do not have. After a pass `STOP_AT_ACKNOWLEDGE` and the wait go; after a fail the early return goes. Emulator (this suite on SDK 51f1943ad plus this change, 150 pass and the two known fails): `0x6F8` 180 sectors, no error or retry, 101,744 HBlanks; `0x6F9` the same, 98,997 (4.4 ms a read less); `0x6FA` 40 sectors, 90,523; `0x6FB` 85,332 (8.3 ms a read less). Every other record measures what it did in v2.13. Console (SCPH-9002, 2026-11-14, hardware-tests v2.18 on two runs): pass. Sectors, errors and retries (`0x6F8` to `0x6FB`) 180/0/0, 180/0/0, 40/0/0, 40/0/0 in both runs; time in units of 16 HBlanks, run 1: 7,511, 6,725, 6,183, 6,077; run 2: 6,925, 5,557, 5,536, 5,103. The early return was faster in each pair by 106 to 1,368 units; the spread between runs is as large as the saving. Retired in v2.19.

### v2.13 (2026-11-13, schema PX8)

MINOR. Records only added: step `SIO0 SELECT EARLY` (`src/sio_timing.rs`, records `0x6F0`-`0x6F4`), five pass-or-fail cases on port 1 for the pad poll that clocks its bytes on a select the poll before left open (`psx_pad::Controllers`). Each case polls eight times, one poll when the port is empty. `0x6F0`: the sequence before (select, 1,024 status reads, the bytes), the control. `0x6F1`: the select opened a video frame before the bytes, as shipped. `0x6F2`: half a second before. `0x6F3`: the bytes at once, so the setup time comes from root counter 0 alone. `0x6F4`: a memory-card frame read on slot 1 between the open select and the bytes. Pass: every case clean on every poll (`polls` low nibble equals the tried nibble); `0x6F1` to `0x6F3` with the select still in `CTRL` on every poll, `0x6F4` with it on none (the card released it, so the driver selects again) and the card read whenever a card is in slot 1; `0x6F1` and `0x6F2` about 7,000 cycles cheaper than `0x6F0`; `0x6F3` and `0x6F4` within a few hundred cycles of `0x6F0` (`0x6F3` takes its setup time from root counter 0 alone, so a figure thousands of cycles lower means the counter runs fast or the wait was skipped, and one thousands higher that the counter stood still and the bounded loop ended the wait). Emulator, this suite on SDK f6d77523 plus this change (DualShock in port 1, card in slot 1): `0x6F0` 8 of 8 clean, 22,983 cycles; `0x6F1` 8 of 8 clean, select kept 8 times, 15,834; `0x6F2` the same, 15,836; `0x6F3` the same, 22,941; `0x6F4` 8 of 8 clean, select kept 0 times, 8 card frames read, 23,081. Every other record measures what it did in v2.12.

Note (2026-11-14, from suite v2.16): every case of this step sends its bytes with CPU interrupts masked (`Transport::mask_interrupts`, the pair `psx_pad::Controllers` brackets a configuration packet with). The select and the wait before the bytes stay outside the masked part, so VBlanks still run while a select is held; only the exchange is protected. Why: once the present path's VBlank handler is linked, the third poll of `0x6F2` can lose the `/ACK` of its fifth byte to a VBlank (the vblank counter moved once inside that exchange, the poll took the ACK timeout, `polls` read `0x0887`). The eight polls of that case start at different phases against the VBlank, so one can land over the pad's `/ACK` pulse; a pause of 20,000 cycles before the step moved the phases and gave `0x0888`. That is the phase of an interrupt, not the select, and without the mask the case could not answer its question. The medians of `0x6F0` to `0x6F4` move by 11 to 35 cycles against the unmasked step; no other record changes. No record field is free for a count of exchanges that saw a VBlank, so none is recorded.

### v2.12 (2026-11-13, schema PX8)

MINOR. Records only retired, none redefined: the interrupt-driven pad engine is deleted from the SDK (the synchronous driver is the one driver for every game), so the steps that exercised it are gone. Removed steps: `ENGINE SETUP SWEEP`, `ENGINE ACK PACING`, `ENGINE TIMED PACING`, `ENGINE AND CARD LEASE`, `ENGINE UNDER LOAD` and `PAD HOT-PLUG WINDOW` (the hot-plug window watched the ports through the engine), module `src/pad_engine.rs`. Retired record ids, not reused: `0x638`-`0x63B`, `0x660`-`0x666`, `0x670`-`0x674`, `0x678`-`0x67C`, `0x690`-`0x694`, `0x720`-`0x749`. The decoder no longer names them. Every other record measures what it did in v2.11, so shared records stay comparable; the v2.11 emulator and machine-code baselines are orphaned until regenerated, as for any bump. The skip key (SELECT) is now always read from the port directly.

### v2.11 (2026-11-13, schema PX8)

MINOR. Records only added. Step `SIO0 LATCH AND ARM` (SIO area, after `SIO0 CARD ACK TIMING`; `src/sio_timing.rs`, records `0x6E0`-`0x6E3`) asks the two questions the shared pad driver's latch path rests on: whether `STAT` bit 9 sets with `CTRL` bit 12 clear, and whether one `CTRL` write (arm plus ACK) after an acknowledged byte disturbs the rest of the packet. Neither passes or fails; a port with nothing in it reads zero tries. The v2.10 emulator and machine-code baselines are orphaned until regenerated.

### v2.10 (2026-11-07, schema PX8)

MINOR. The suite that is burned and put on the SuperStation card (v2.9, the BIOS-free linear run) and the v1.27 tree that sat on editor main are one tree again, on editor main, and every v1.26/v1.27 measurement either runs in it or is named as covered. [hardware-test-v1-to-v2.10.md](hardware-test-v1-to-v2.10.md) is the mapping, feature by feature. Added to v2.9:

* `MDEC SETUP DIAGNOSTIC` (GPU area, after `MDEC DECODE`; `src/mdec_diag.rs`, records `0x230`-`0x2AA`): the v1.26 diagnostic. Six MDEC setup sequences (the v1.25 driver, settle then enable, fixed delay, CPU tables, PSn00bSDK's order, the SDK driver), eight runs each, a probe decode after each; MDEC1 traced after a reset from idle, from busy and from busy with the enable written straight after; a one-macroblock control decode over CPU writes and reads against DMA. The v1.26 record ids `0x200`-`0x2A2` collide with the v2 texture-state block (`0x200`-`0x223`), so the layout moved to `0x230`-`0x2AA` (the playback block is gone, see the mapping). Every wait is spin-bounded. The decoder is `psoxide-hwtest report` (`mdec_diag`, `mdec_reset_trace`, `mdec_control` rows).
* `GPU BATCHES` gains the six grid-line E2 texture-window cases, `0x146`-`0x14B` (the v1.27 tree numbered them `0x140`-`0x145`, which the v2 CPU/DMA matrix owns; they never ran on a console under the old ids).
* Conformance case `0xEA` (index 234, `src/list_upload_probe.rs`): GP0(A0h) rows in 16-word list nodes read back word for word. It runs in the GPU area. `TEST_COUNT` 235.
* `CD DATA VERSUS AUDIO ROUTE` keeps everything the v1.x CD/SPU audio probe (PA1) recorded per stage that v2.0 dropped: SPUCNT, SPUSTAT, the CD volumes, the CD status and request flag, and for each capture half the FNV hash, the non-zero sample count and the summed magnitude. Records `0x520 + 8 * stage + k`.
* The device owners (`Cd`, DMA 6, the memory-card diagnostic) are taken by `steal` in `src/probe_tokens.rs`, as v1.27 does since 4fcfc51ae: the SDK removed the free forwarders the v2.9 sources still called. The host-only fallbacks of the RTPT probes use `write_data!`, `read_data!` and `project_triple` (ea367f697).
* `SUITE_VERSION_MINOR` says 10. v2.9 shipped the display string `HWTEST v2.9` with the minor constant still 8, so every v2.9 capture reports itself as v2.8 in its header; the header of a v2.10 capture is right.
* `TIMING_RECORD_COUNT` 1700 (v2.9: 1500) and `CAPTURE_PAGE_MAX` 32 (28). A plain-disc run goes from 1,215 records to 1,332 (19 to 20 QR pages); the radius disc adds the sweep's `0xB00`-`0xBF7`, which still fits.
* The disc is always the console-diag build with `CDTEST.BIN` at 400 sectors (`make hardware-tests-disc`). The BIOS-free boot EXE is 577 sectors, past the 542 that a 460-sector `CDTEST.BIN` leaves; every v2.9 burn and test image already used this layout.

* `GPU DONE SIGNAL ONLY` (MAIN MENU row 1, `src/gpu_done.rs`, from the FPGA lane): does GP0(1Fh) raise GPUSTAT bit 24 and I_STAT bit 1 on a core that loses it. Three result pages (A: 18 signal rows, B: 11 candidate closing sequences, C: 18 fill and shipped-node rows), 24 attempts each; it is not part of the linear run and adds no records. On `real` every row is 24/24; on `mister` the shipped rows `SHIP DMA IDLE`, `SHIP DMA BIG` and `SHIP PORT RAW` are 24/24. The emulator baseline was refreshed with it (the module moves the layout and the phase of everything after it).

Pins: SDK `bfb464405` (SDK main), emulator `bb788f5b` (emulator main), the editor main lock. v2.9 was built on SDK `838c09fb`. What that move changes in the records is in [the v2.9 to v2.10 comparison](hardware-test-v2.10-vs-v2.9.md): the SDK's new entry sets the bus timings itself (COMMON_DELAY `0x132C` to `0x1325`, CD_DELAY `0x20943` to `0x20843`, the retail values) where the emulator's HLE entry left others, which moves every SPU and CD register timing record; the shared draw-done node adds 9 clocks to a list walk; the rest is phase. No conformance verdict changed on either target (real: cases 139 and 201 fail, as in v2.9; MiSTer: 139, 201, 204, 205, 227).

### List-upload case (2026-10-22, v1.27 tree; in the suite since v2.10)

One conformance case, `0xEA` (index 234, `src/list_upload_probe.rs`), joins the DMA group without a version bump (a case added at the end moves no existing id). It uploads four 384 halfword rows as GP0(A0h) image transfers inside a channel 2 linked list, each transfer chopped into nodes of at most 16 payload words and the list ended by a texture-cache flush node, the way the grid runtime's sky stream sends them. A poison fill goes first, the block is read back with GP0(C0h) and compared word for word. The observed value is the count of differing words, plus flags for a stuck channel (bit 16), a GPU that never reports ready (bit 17) and a short readback (bit 18); every wait is bounded. Headless in the emulator with `PSOXIDE_CHECK_GPU_LL_FIFO=1` it passes, and with the kick removed it fails with all 768 words wrong. Not run on a console.

The suite's own build also needed a repair: `psx-engine` reaches `psx-level`, whose wire module uses `alloc`, so `make hardware-tests` (and the lint step for it) now builds with `-Zbuild-std=core,alloc` and the crate links psx-rt's bump allocator. The boot EXE stays under its sector budget.

To run just this case headless: from the main menu go UP three rows to RESUME FROM TEST, press R1 23 times and RIGHT four times (start index 234), then CROSS. The case prints as `case 234` in the TTY report before the long scans start.

### Grid-line E2 probe (2026-10-17, v1.27 tree; in the suite since v2.10 under new record ids)

This is not a suite release. A real v1.28 exists (console run 2026-10-08) and a newer consolidated suite lives on another branch line, so this tree keeps the version it started from (v1.27) and carries the probe as extra steps. Six GPU batches price GP0(E2), the texture window the grid renderer repeats textures through (`0x140`-`0x145`, `src/gpu_probes.rs`). Every grid packet carries its own E2 word in front of the polygon, so the cases read in pairs: a reset window carried in each packet against none (`0x140` against `0x103`), an active 64 texel window against none at the same UV span (`0x141` against `0x10D`), one 32 texel window every packet against two alternating (`0x142` against `0x143`, the cost of changing window between neighbours, a possible texture-cache flush), a reset alternating with a window (`0x144`, the stream a level of plain and repeating quads makes) and a window repeated four times across a primitive (`0x145`). The timing capacity grows by six slots. Not run on a console; do not burn a disc from this tree as a release. Port the six cases to the consolidated suite and number them there.
### v2.8 (2026-10-19, schema PX8)

MINOR: two steps added and one menu row, to answer two console questions with one burn: does this console's laser and media fail at the outer radius of a full 80-minute CD-R, and at what GPU linked-list node size does silicon start losing data.

* `CD RADIUS SWEEP` (Drive area, after CD MOTOR; `src/radius.rs`, records `0xB00`-`0xBF7`). Needs the radius disc (`make hwtest-radius-disc`): the image plus `RADIUS.DAT`, deterministic filler (`src/filler.rs`, written by `psoxide-hwtest radius-fill`) that takes the data track to about 78 minutes. It reads 64-sector groups at the file's first sector, every fourth minute of disc time and every 20 seconds of the last two minutes, three ways each (the SDK `SectorReader` at 2x, raw controller commands at 2x and at 1x), verifying every word, and counts delivered and matching sectors, INT5 errors (first error code), timeouts, retries and the time. Every wait is bounded; three dead positions in a row end the sweep; SELECT skips the rest. On a plain disc without the file the step says so and records nothing else.
* `GPU LIST NODE SIZES` (GPU area, after GPU DMA EDGE CASES; `src/ll_sweep.rs`, records `0xC00`-`0xC47`). One list per node size (16, 20, 24, 28, 32, 36, 41, 48, 64 words), with the GPU busy on two large triangles and idle: flat rectangles in the node, a closing GP0(1Fh) node. Records whether the GP0(1Fh) interrupt arrived (bounded wait), the clocks it took, whether the channel finished, where its address stopped, and which rectangles drew, read back from VRAM. The GPU is recovered after every trial.
* Both tables are drawn row by row while the step runs and kept as text pages between the cover and the QR pages of the capture (they repeat with them), so a photo of the screen is the answer. `psoxide-hwtest diag-report <pages>` prints both from a capture and recomputes every group's checksum.
* MAIN MENU row 2, `RADIUS + LIST NODE SWEEP ONLY`, runs just those two steps (the radius sweep first) and shows the capture. Row 0 stays the full run.
* The suite builds with the SDK console stop panel (`--features console-diag`; psx-rt registers, wait site, CD and GPU counters on a fault, panic or 15 s stall) for the radius disc. The panel's 24 sectors push the boot EXE past what 460 sectors of CDTEST.BIN allow, so that build gives CDTEST.BIN 400 sectors (`CDTEST_SECTORS`/`CDTEST_LBA` in `main.rs`; the plain build is unchanged, LBA 564). `bounds::record_start` and `begin_step` tell the panel's watchdog the suite is alive. The panel's hooks also change the SDK paths' timing slightly: do not diff the diag build's timing records against the plain build.
* On the radius disc the CD-DA track is appended after the filler, so it sits at the outer edge: the CD-DA records of that disc are outer-radius measurements, with full-stroke seeks between the data and the tone.
* `TIMING_RECORD_COUNT` 1500 and `CAPTURE_PAGE_MAX` 28. Pins: SDK `e50f4859` (`diag/stall-limit-2026-10-19`: SDK main `b5365b18`, which contains `bbddb44b`, plus the stop panel from `diag/console-2026-10-18` and `psx_rt::diag::set_stall_limit`), emulator unchanged (`6edde550`). The suite raises the panel's stall limit to 20,000 VBlanks (about 5.5 minutes): with the default 15 s the panel took the screen during the silent steps (the 180 s XA clock case issues no GP1 write, GPU kick or CD command the watchdog counts) and reset the GPU mid-measurement. Faults and panics still show at once.
* No existing record changed meaning.

### v2.7 (2026-10-16, schema PX8)

MINOR: one step added, `CD PAUSE SECOND RESPONSE`, the last step of the CD area, records `0x340`-`0x3B8` (`src/cd_pause.rs`, layout in its header and in [hardware-test-disc.md](hardware-test-disc.md)). It times the CD controller's second response (INT2) to Pause, in system clocks from the command byte, for an idle drive and for a data read at single and double speed, with the head settled in a read, at its first sector, mid-seek and after a long seek (three trials each), then watches GetStat commands sent between Pause's two responses. The wait is bounded at 8,000,000 clocks. Nothing existing changes meaning. `TIMING_RECORD_COUNT` rises from 1100 to 1300: the v2.6 run takes about 1,090 records, this step adds 79, and 1100 dropped 68 of them (the third field of `0x41E`) in the first headless run.

This version also consolidates the suite's history onto one branch. v1.28 (CD STREAM), v2.0 to v2.4 (the single ordered run, rumble, 480i, FIFO and card steps), v2.5 (cached store and load rows), the CLUT address rewrite (`CLUT ADDRESS REWRITE`, GPU area) and v2.6 (XA) had only ever lived on separate branches and were never on main, which still said v1.27. Record ids are unchanged from those builds, so the silicon captures taken on them (v1.28, v2.1, v2.2, v2.4) stay comparable record for record. Pins: SDK `bbddb44b` (`feat/pad-engine-land-2026-10-16`, SDK main plus the pad engine and DualShock motors; v2.2 to v2.6 were built against `integ/pad-2026-10-08`, which this branch contains plus the card-lease ACK-latch fix) and the emulator `6edde550` (editor main's). The host tools are the Rust `psoxide-hwtest`; the Python report, silence check, baseline writer and machine-code verifier of those branches are not restored.

### v2.6 (2026-10-15, schema PX8)

MINOR: one step added, `XA LATENCY, SWITCH, CLOCK`, right after `XA MUSIC LOOP` in the CD area, with records `0x320`-`0x32E` (`src/xa_measure.rs`, layouts in its header and in `hwtest-report.py` `xa_measure_rows`). Nothing existing changes meaning. The SDK pin stays `1832928d0`: a bump to the XA clock and resume branch (`feat/xa-clock-resume-2026-10-15`) breaks `rumble.rs` and `controller_test.rs` (the pad API moved), so the XA resume is sent by hand with the same commands as `Player::resume` there, and the song clock is not run; move to the real calls when the pin moves. Every wait counts loop iterations (SELECT skips), never a timer.

What it measures, all heard through the SPU CD capture buffer (the pad song, channel 0, and the whistle, channel 3, tell themselves apart by zero crossings), so nobody has to listen:

- XA start latency: `Player::play` from a paused drive to the head seen in the file and to sound, five times (`0x320`-`0x322`).
- XA channel switch: a Setfilter alone, mid-stream, to the other song, six times, and the same switch by starting the other song from the top, three times (`0x323`-`0x325`).
- After a 64-sector data read at double speed: XA resume (Demute, Setmode, Setfilter, Setloc to the head GetlocP gave at the pause, ReadS), a fresh XA start and CD-DA Play, each three times, timed from the last data sector to sound, with the read's own time and where the resumed song continued (`0x326`-`0x32B`).
- XA clock drift: `HWCLOCK.XA`, one 190 s song (new, 14,250 sectors, 33 MB, file number 2), plays for 180 s at a low volume while the head is polled twice a second. Head progress minus system-clock time at 60, 120 and 180 s (Timer 2 read in a tight loop, wraps placed by Timer 1) says how far the drive's sector clock and the console's clock drift apart; also the head positions seen, the longest stall and the system clocks per HBlank line over the run (`0x32C`-`0x32E`). The step adds about four minutes to the run.

The disc build gains `HWCLOCK.XA` (`make hardware-tests-disc`, `PSOXIDE_SDK` as for `HWSONGS.XA`). `TIMING_RECORD_COUNT` is unchanged.

### v2.5 (2026-10-11, schema PX8)

MINOR: twelve cached-RAM store/load timing rows and one address readback row were added to `EXTENDED PROBES AND SHAPES`. Existing rows, step order and capture format are unchanged. Records `0x8D0`-`0x8DB` are the four T00/T10/T01/T11 shapes at literal gaps of 0, 2 and 6 nops, with sixteen successive A/B word pairs per row. Each has eight trailing nops and a pointer advance. The warm harness masks IRQs, times the second pass and records min/median/max of five samples. `0x8DC` records zeroed A count, unchanged sentinel B count and whether the last load returned the final B sentinel: 16, 16, 1 on a clean run. The load witness is copied after the timer read, outside the measured interval. Linked instruction markers `0x220`-`0x22B` audit the exact opcodes and spans. The interaction per pair is `(T11 - T10 - T01 + T00) / 16`, calculated separately at each gap from unrounded clocks. These measurements distinguish a store-to-different-address-load interaction from ordinary store and load cost; they do not assume a bus delay constant.

The 0/2/6 rows answer an open question raised by v2.4's mixed CPU calibration `0x8C1`. No v2.5 silicon result is claimed by this source addition. The disc must be run on hardware and in the emulator using the same BIN/EXE before changing the CPU timing model.

### v2.4 (2026-10-09, schema PX8)

MINOR: five new measurements the emulator work asked for, a pad question, and a self-check on the record list. No existing record changes what it means. The SDK pin is now `1832928d0` (the pad integration branch with the five review fixes); the emulator pin is `af6747947`. Steps: 66 (61 before), records: about 1,000 (788), `TIMING_RECORD_COUNT` 1,100.

- 480i draw rule (`src/field_rule.rs`, step `480I DRAW RULE`, the last safe step of the run, `0x800`-`0x852`): in GP1(08h) 480-line interlaced, with GP0(E1h) bit 10 clear, which rows of a 64 x 16 scratch area do a fill, a rectangle, two triangles, a textured quad and a line leave untouched, with GPUSTAT bit 31 at 0 and at 1; and does a VRAM copy or an upload skip them. Also with bit 10 set, with the scratch area outside the displayed rectangle, in 480 progressive, 240 interlaced and 24bpp modes, with a texpage word that sets bit 10, with the draw-area top and the draw offset on an odd row, with the display start on an odd row, a fill issued at once after VBlank, the clocks of a 256 x 240 fill and rectangle with the rule on and off, and the same commands with the display blanked. Every record carries its own descriptor.
- GPU list stopped mid-node and block-mode DMA into a full FIFO (`src/dma_edge.rs`, step `GPU DMA EDGE CASES`, `0x860`-`0x873`): a large rectangle, then an upload node of 48 words and a second upload node; CHCR START is cleared 6,000, 20,000 and 60,000 clocks after the kick (and not at all as the control); the words that landed say how much of the node reached the GPU and whether the walk went on. A request-mode transfer of 64 and 256 words (blocks of 16, and 8) behind the same rectangle, with and without it, counts the words that landed and the clocks it took.
- Memory-card protocol (`src/card_proto.rs`, step `CARD SECTOR PROTOCOL`, `0x880`-`0x8BF`): per slot, a 140-byte sector read four times with every byte's `/ACK` rise, width and arrival, the checksum and the terminator, the replies; with L1 and R1 held, a 138-byte write of the same bytes to the same sector, twice. An empty slot records the unanswered first byte.
- Whole-workload calibration (`src/workload.rs`, step `WORKLOAD CALIBRATION`, `0x8C0`-`0x8C5`): a fixed scene of 256 textured triangles from a linked list and a fixed block of CPU work, timed alone, together, and in VBlank-locked frame loops, on the system clock extended to 32 bits.
- Texture UV windows (`src/workload.rs`, step `TEXTURE UV WINDOWS`, `0x8E0`-`0x92B`): sixteen textured triangles per row with the UVs swept over a 256 x 256 page at 4, 8 and 15 bits (spans 8 to 255 texels, origins moved, the texture window shrunk to 8 to 64 texels); each row is a timing record and a record that says what the row was.
- Pads (`rumble.rs`, `0x940`-`0x962`): the motor step asks which pad is in the port (CROSS SCPH-1200, CIRCLE SCPH-110, SQUARE another, TRIANGLE none), runs the battery, then asks the operator to swap the pad and name the new one; an answer runs the whole battery again into a second set of records (the first set's ids plus `0x1E0`). No answer to either question is recorded and the run goes on.
- Card write safety (review fixes): the sector read and written is frame 56 (an unused frame of block 0: psx-spx has 36 to 55 as broken-sector replacement data and 56 to 62 unused), never the header; the write is gated on all four reads being good with matching checksums, no error flag and identical data, with the reason recorded in `0x89D`/`0x8BD` bits 1 to 5. The QR checkpoint pages are now armed by START+R2 at the start, apart from the L1+R1 card-write chord (which also arms the older pad-engine card-lease write-back, about sixty frames of block 0 rewritten with the bytes just read). `GPU DMA EDGE CASES` runs after `WORKLOAD CALIBRATION` and `TEXTURE UV WINDOWS`. The pad prompts fit the screen.
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
