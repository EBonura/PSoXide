# Comicon performance and audio validation, 2026-09-04

Scope: Cortex Ignition 0.5 first, Quake-PSX second, HL-PSX third, and both
demo-disc layouts. Preserve visual quality, gameplay and audio contents.
Raw artifacts are in `/tmp/astra-perf-20260904`.

## Music correctness

`b1ee0fd5` fixes the rounded-per-tick CDDA ramp. A requested 120-tick fade
previously completed after 80 ticks at 80% volume, and after 40 ticks at
40%. The player now interpolates from its starting volume over the complete
duration, writes the SPU only when the integer volume changes, and releases
the drive on the final tick. A loading handoff also cancels a pending fade
stop, so it cannot suppress subsequent menu music.

Three new regression tests failed before the change and pass afterward;
all 411 engine library tests pass. The rebuilt shipping guest completed
the Cortex whole-level tape through menu, combat activation and combat
deactivation. CD commands select standalone menu track 3 and combat track 2.
No data read interrupts combat playback. The final capture remains live
gameplay.

## Cortex baseline and method

Fresh Cortex 0.5 baseline after the audio fix, `cd-stream-bench
lockstep-visuals`, tracked whole-level tape, stop at poll 5250. Two runs
agree exactly: 4,248,238,830 emulated bus cycles, 1,586,711,960 work
instructions, 2,620 presentations, display `0xfdd4a01402c63c96`, VRAM
`0x5d151bce1b4a5c55`. The baseline executable is
`1e77cab320ccf34c3bf2c4e044cfac7ea6b26b7663d03ad93a149ef6cda77334`.

Model projection/submission accounts for 18.54% of retired instructions.
Generated MIPS already loads packed XY and defers depths until after
backface rejection, so the suspected redundant face loads need no patch.
The model projection triple does use `lwl/lwr` pairs because decoded
`ModelVertex` records have only two-byte alignment. The alignment experiment
keeps the existing eight-byte record size and leaves cooked files unchanged.

The existing wide-arithmetic symbol gate reports a failure on the baseline
too. No new wide arithmetic is introduced here. Lockstep timing is an
emulator comparison, not a claim of sustained FPS on original hardware.

## Accepted Cortex alignment

`eee3aa93` gives decoded `ModelVertex` four-byte alignment while preserving
its eight-byte size. MIPS replaces paired unaligned XY loads with a single
word load. Two exact replay runs use 4,237,957,047 bus cycles (-0.242%) and
1,583,903,364 work instructions (-0.177%). Model geometry work falls by
2,339,934 instructions. Display, VRAM and 2,620 presentations match the
baseline exactly. BSS stays `0x80121800..0x801f3834`. All 35 asset tests and
411 engine tests pass. Candidate executable SHA-256:
`28853879f3412863092ccfcd27644e4416826e785d044252c22c80e22363ef13`.

A second candidate combined three projection writes under one slice check.
It increased cycles by 0.013% and work by 0.020%, with identical output;
it was reverted. No visual settings or pool sizes were reduced.

## Quake baseline

Pinned Quake `f036850` and SDK `c2c4b90d`: two identical fixed E1M1 route
runs, all 60 waypoints and the E1M2 transition complete. 1,682 presentations
over 2,264,959,278 bus cycles, 25.136 measured FPS. Keep the documented
0.122-FPS code-placement noise band when interpreting small changes.

The accepted Quake `c2329be` update pins SDK `b1ee0fd5`. Both fixed-route
replays complete all waypoints and the transition with identical display
`0x621bf7ee03f427a4`, VRAM `0x6c23b5e6511bc16e` and 1,682 presentations.
Measured cycles become 2,260,389,289 and FPS 25.187. This is below the
0.122-FPS layout-noise band, so it is not evidence of a significant FPS gain.
The shared packet linker does remove five instructions per link in generated
MIPS. Visual parity and all host test suites pass; see Quake
`docs/comicon-runtime-2026-09-04.md`.

## HL-PSX tram pass

At Manny's request, benchmark the full opening tram ride, using full shipping
arenas and the same runtime pin. An early depth-plus-packet gate was rejected:
240.350 to 240.846 instructions per soft-cell call, despite a tiny FPS rise.
The narrower depth-only gate preserves clipping and tessellation while
skipping LOD calculations after subdivision has already reached its limit.
It reduces instructions per call to 236.336 (-1.67%) and full-ride FPS changes
21.538 to 21.586. All five moving segments improve slightly; the slowest is
still 13.27 FPS and fifth-percentile windows remain 8.98 FPS. This is a small
optimization, not stable 20 FPS. Static RAM remains 1,993,492 bytes and text
shrinks four bytes. Final display/VRAM hashes match, intermediate screenshots
were inspected, 17 host tests pass and the MIPS patcher leaves zero hazards.
Details are in HL-PSX `docs/comicon-tram-2026-09-04.md`.

## Demo-disc audio method

An independent CUE/TOC audit checks every game's cumulative CDDA base,
every appended audio track's full sector span against its source image,
pregap lengths, all four raw launcher songs, shared GH-PSX/Arcade ownership,
and total track coverage. Absolute track numbers after Cortex change when
a track is added; correctness requires each game's base to move with them.

The rebuilt HL pressing passes all 35 audio tracks: launcher 2-5, Cortex
combat 6/menu 7, Half-Life 8-34, GH-PSX/Arcade 35, Hardware Tests 36.
VoXide, NitroXide, Celeste, PSXcel and Quake append no audio tracks.

Both standard and HL pressings build. The standard disc has 8 audio tracks:
launcher 2-5, Cortex combat 6/menu 7, GH-PSX/Arcade 8, Hardware Tests 9.
All audio spans, TOC bases and launcher songs match their sources. The two
Cortex tracks also match the source WAV PCM with only sector padding added.
The audit covers every game, including those sharing or appending no tracks.

Runtime/Quake SDK use `b1ee0fd5`; Cortex uses `eee3aa93`; Quake uses `c2329be`.
Both pressings pass all nine independent-program routes and deterministic
Quake chain-load checks. The release-critical Cortex, HL, Hardware Tests and
Quake battery passes; the final Cortex check sustains 466 gameplay frames.
See demo-disc `docs/comicon-audio-2026-09-04.md` for reproducible commands,
track-by-track evidence and final image receipts. Console testing is separate.
