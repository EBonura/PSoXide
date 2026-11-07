# HWTEST v2.10 against v2.9, row by row

The same headless run of the suite, on both emulator targets (`real` and
`--hardware mister`), for three builds: v2.9 as shipped (editor
`d3119871`, SDK `838c09fb`), an intermediate **I1** (the v2.9 sources
converted to the current SDK's device-owner API, nothing else; editor
`0244bce2` plus the token edits) and v2.10. All three booted with the same
`hardware-tests-disc` layout (console-diag, `CDTEST.BIN` 400 sectors) and
the pad pulse the Makefile uses. Reports come from `psoxide-hwtest report`;
the gate output is its own `--baseline ... --layout-immune-timing-only` drift
list (conformance observations, timing minima of the layout-immune records,
precision values, failures).

## What changed, in order of how much it explains

1. **The emulator did not change anything.** The v2.9 EXE run on the 11-03
   emulator binary (the `emu-1f-quirk` lane build, `dce58e70` on main
   `656cbed8`) and on main's `bb788f5b` gives byte-identical reports on both
   targets (every row, every verdict). So no row below moved because of an
   emulator change.
2. **The SDK pin moved from `838c09fb` to `bfb464405`.** Already I1, which
   differs from v2.9 only by the SDK and the API conversion, moves 437 timing
   rows and 61 gate items on `real`. v2.10 against I1 moves 27 gate items
   more (the new steps shift where the later steps start).
3. **The suite additions** (below) add 117 records and one case and move the
   start phase of everything after the GPU area.
4. **No verdict changed.** Every case that passed, failed or was informational
   in v2.9 has the same status in v2.10 on both targets. `real`: two fail
   (139 `NCLIP` scene C, 201 `GTE vs IRQ ... RTPS intact`), as on the 11-03
   v2.9 runs. `mister`: five fail (139, 201, 204, 205, 227), as recorded.
   New case 234 (`list upload`) passes on both.

Nothing is a real regression. Every moved value falls in one of the causes
below.

## Causes

Each row was bisected to a step (v2.9 to I1 is the SDK pin, I1 to v2.10 is the
suite). Naming the SDK commit behind an SDK-step row follows the commit's
stated effect; the individual SDK commits were not rebuilt one by one.

**A. The SDK's new entry state** (`5b1f2b8d6`, `d16e06a37`, `875111021`,
`5b0d3f433`). The runtime no longer trusts what the BIOS left: it writes the
memory controller's access timings itself and zeroes the GTE. The emulator's
HLE hand-over left `COMMON_DELAY` at `0x132C` and `CD_DELAY` at `0x20943`; the
SDK now writes the retail values `0x1325` and `0x20843` (visible in
`memory_control` of every v2.10 capture; I1 shows the same). On silicon the
BIOS already wrote those, so this change moves the emulator's numbers toward
the console's, not the console's numbers.
* `FC warm_spustat_half_read` min 1790 to 1214 and `FD warm_spu_half_write`
  894 to 510 (both targets): SPU register accesses pay the common delay (consistent with the memory-control values, not separately isolated).
  Every SPU and CD register timing record moves the same way (tolerated, not
  layout-immune: the `0x300`-`0x3xx` CD and `0x7xx` families).
* `CC`/`CD` unaligned load/store minima (-1 and -4 cycles, `real`), `C8`
  `warm_ram_loads_back_to_back` (-8, `mister`) and `128` (-8): the same delay
  change reaching the RAM-side probes through the bus recovery.
* GTE zeroed at entry changes the starting `FLAG`/data state of the GTE
  interrupt cases: `real` 200 (`0x88` to `0x7F`), 201 (`0x88` to `0x7F`, still
  FAIL, it failed in v2.4 and every baseline since), 202 (`0x4B` to `0x51`);
  `mister` the same three (`0x7E`/`0x52` to `0x7F`/`0x51`).

**B. The shared draw-done node** (`942453a3d`, probable: the rows moved in I1, where only the SDK differs): the SDK's closing sequence is a
13-word fill node instead of one `GP0(1Fh)`. Every list walk ends with it, so
the paced GPU list batches `0x100`-`0x114` are +9 clocks each at the median
(the same +9 on every row, `real`), and the 16-word-node texture state
rows `0x200`-`0x223` move by +2 or -11 (the node alignment against the CLUT
and page change). The list cases `219`-`226`, `231` (empty/cheap/expensive/
packed list, "CPU during walk") are walk-length observations of the same
lists and move by 20 to 70 clocks in 620,000 (0.01 percent): 219 `0x97630` to
`0x97657`, 220 `0xA17A0` to `0xA17BE`, 221-226 likewise, 231 `0x97604` to
`0x9760C`. `real` only; the `mister` cases 211, 213-215, 217-219, 221-223,
225, 226, 231, 233 are the same effect on that target.

**C. Phase.** Several precision values and informational cases read a free
running quantity (the GPU field bit, a timer, the OTC channel's busy flag,
SPUSTAT's buffer half) at a fixed point of the run. Anything that changes
how long the run took to get there changes them, and the SDK entry and the new
steps both do. They are not properties of the hardware being measured.
* GPU status words in the precision block (`043`-`060`, `136`): `0xD6022405`
  to `0x56022405` and its siblings. Only bit 31 (the interlace field bit) and
  the DMA-direction bits differ; the video mode bits `022405` do not.
* `timer_*` precision (`062`, `063`, `065`, `068`, `069`), `spu_*_stop_mode_polls`
  (`018`, `035`), OTC (`otc_chcr_read0`, `otc_madr_after`, `otc_last_word`,
  `106`, `112`, `116`): counters and a busy flag read at a different instant.
  `otc_chcr_read0` flips between `0x11000002` (busy) and `0x00000002` (done)
  from build to build for this reason; `otc_madr_after` and `otc_last_word` are
  where the OTC walk had got to.
* Cases `10`, `14`, `15`, `32`, `38`, `40`, `46`, `49`, `51`, `52`, `176`,
  `209`, `210`, `230` (GPU IRQ in I_STAT, timer 1/2 free run, timer 0 tick
  counts, the DMA direction latch, NCLIP bracket, scratchpad-stack
  background counts, RAM-load iterations during a walk). All INFO or PASS
  before and after; the observed number is a count of something that happened
  in a fixed window.
* `mister` only: case 23 (SPUSTAT readable, `0x800` to `0`, the capture-buffer
  half bit).

**D. Layout.** The EXE is 577 sectors against 547 and every address moved.
The machine code of all 218 measured instruction blocks is byte-identical
(`verify-machine-code`: the same digests in v2.9, I1 and v2.10), so no probe
changed; the cold-cache and alias-sensitive records (`1C icache_cold_4k`,
`42`-`45`, `1B0`-`1BC`, `135`, `137`-`13A`) shift by a few clocks. They are
not layout-immune records and the gate tolerates them (248 on `real`, 190 on
`mister`); the layout-immune ones are the items under A.

**E. New steps.** New records:
`0x146`-`0x14B` (E2), `0x230`-`0x2AA` (MDEC diagnostic), `0x520`-`0x547`
(PA1 detail), the case `0xEA`. Nothing existing was removed.

## The gate output, item by item (`real`)

The `--layout-immune-timing-only` drift list of v2.10 against v2.9 has 58
entries. Their causes, by the groups above:

| entries | cause |
|---|---|
| `new failure 0x00c9` and `0x00c9 no longer reproduces` | A: the failure record is case 201, same case, the observed pair changed with the zeroed GTE |
| cases 10, 14, 15, 38, 40, 46, 49, 51, 52, 176, 209, 230 | C |
| cases 200, 201, 202 | A (GTE zeroed) |
| cases 219-226, 231 | B |
| timing `FC`, `FD` | A (bus delay) |
| timing `CC`, `CD` | A |
| precision 018, 035, 062, 063, 065, 068, 069 | C |
| precision 043-060, 136 | C (GPU field bit) |
| precision 112, 116 | C (OTC walk position) |

I1 against v2.9 shows 61 entries; v2.10 against I1 shows 27. The two lists
overlap because phase values flip back and forth between builds; none of them
is stable across the three.

## MiSTer target

The same gate (v2.10 against v2.9 on `mister`) lists 44 entries: failure
`0x00c9` as above, 26 case observations (the `real` cases plus 211-218, 233 and
23), timing `C8`, `FC`, `FD`, `128`, and 12 precision values; 190 timing rows
are layout-tolerated. Causes are the same A, B, C. The three extra failures of
the `mister` model against `real` (204 and 205, the present-queue cases, and 227,
`packed list draws the same pixels`: the cases the lost `GP0(1Fh)` interrupt of
that core breaks) are unchanged, as expected from the model.

## What a person comparing with a console capture should know

* The SPU and CD register timing records of a v2.10 emulator run now agree
  better with a console capture than v2.9's did, because the emulator's HLE
  entry no longer decides the bus timings.
* The records that differ by a handful of clocks between builds (group D) are
  exactly the ones `make hwtest-diff` treats as layout-sensitive; compare them
  across builds only with the layout-immune filter.
* A v2.10 capture's header says `v2.10` (v2.9's said `v2.8`).
