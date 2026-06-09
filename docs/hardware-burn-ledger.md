# Cortex Ignition V1 Hardware Burn Ledger

This ledger is the place where real PS1 CD-R observations become engineering
work. Do not leave a hardware finding only in chat or memory: each entry should
point to a visible checkpoint, a preburn/emulator gate, or a tracked
emulator/engine/SDK fix.

## Rules

1. Record the disc label, source commit, build command, burn speed/media, console
   model, video output, and exact visible result.
2. Preserve any screen color, text code, or hang point verbatim. Use
   `docs/hardware-visual-checkpoints.md` as the code/color map.
3. Convert the finding into one of:
   - a `hardware-boot-visual` checkpoint or text label;
   - a preburn/emulator assertion;
   - an emulator, engine, or SDK fix with a repro command.
4. Do not burn the next candidate while the bringup report has `WARN` or
   `MISSING` rows unless the exception is recorded here.

## Entries

| ID | Date | Disc / build | Hardware observation | Local / emulator evidence | Disposition |
| --- | --- | --- | --- | --- | --- |
| HWB-001 | pre-2026-06-08 | `demo10` / early `cortex_ignition_v1` color-checkpoint discs | Screen colors were used to locate boot progress on real hardware. Exact color/code sequence still needs backfill from notes/photos. | Current visible checkpoint sources: `engine/crates/psx-engine/src/app.rs`, `engine/crates/psx-engine/src/game_app.rs`. | Backfill exact color table, then keep stable names in the visual checkpoint map. |
| HWB-002 | pre-2026-06-08 | `demo10` / text-checkpoint discs | Text display was added after color-only discs to make the stop point identifiable on TV. Exact last visible text still needs backfill. | DuckStation BIOS TTY markers now assert `psx-rt`, editor init, scene init, and CD-DA startup markers. | Add any hardware-only last text code as a DuckStation/Redux expectation or a preburn assertion. |
| HWB-003 | 2026-06-02 evidence, still active on 2026-06-08 | Menu -> gameplay while menu CD-DA remains active | Gameplay showed sky/HUD only in the documented profile path; streamed room data did not become valid content. | `docs/demo10-low-level-hot-paths-2026-06-02.md` records `CD_ROOM_CHUNK_STATUS=5` (`STATUS_CD_ERROR`). `make cortex-ignition-v1-preburn-streaming-guard` now fails until profile rows prove room streaming while CD-DA evidence remains present. | Active blocker. Fix CD-DA/data ownership or route sequencing, then make the stream guard pass before burning. |
| HWB-004 | 2026-06-08 | `cortex_ignition_v1` external emulator matrix | Not a hardware burn. Added to prevent false confidence before the next CD-R. | RetroArch previously emitted `Firmware is missing: scph5501.bin`; the smoke now passes a BIOS-derived `system_directory` and writes a verified PNG screenshot. Mednafen and ares rows are marked `OBSERVED`, not semantic boot success. | Keep emulator comparison rows in the bringup report; promote any emulator disagreement into a focused probe. |
| HWB-005 | 2026-06-09 | `hardware-tests` GPU read-back battery (commit `66cd9af7`, 118 cases) burned at 10x | GPU CHECKS (photos IMG_6132-6135): every drawn TRIANGLE FAILs, every QUAD PASSes, raw VRAM fill PASSes, poly-too-large tri PASSes. 1st fail = flat triangle EXP 0x495AFB4D (emulator) / GOT 0x04121005 (hardware). | Quads are axis-aligned rects -> the 2-triangle split covers them exactly regardless of edge rule, so the divergence is the triangle EDGE-COVERAGE rule (~1px on diagonals), not gross geometry. `emu/crates/emulator-core/src/gpu/raster.rs` is "parity-matched against Redux" (header), i.e. tuned to PCSX-Redux, not silicon. | (1) Vertex explosion: GPU+DMA EXONERATED (a 1px edge diff can't fling a vertex); explosion is pose-specific skinning, reproduce the exact pose. (2) North-star rasterizer fix: needs a follow-up disc that dumps the covered-pixel BITMAP (not a hash) to reverse-engineer the PS1 edge rule, then rewrite raster.rs to match silicon. |
| HWB-006 | 2026-06-09 | `hardware-tests` silicon-rasterizer disc (commit `83424cf5`, GPU cases re-baked) burned at 10x | GPU CHECKS section is **ALL GREEN on real hardware** (photos IMG_6136-6139): all 12 triangle cases that were RED on HWB-005 now PASS -- flat/gouraud tri, edge/neg/wrap/bottom, textured-gouraud (player prim) direct + via OT/DMA + large-span, 8bpp CLUT, deep OT/DMA. ALL HARD FAILURES CLEAR. Global PASS 083 FAIL 009: the 9 fails are 8 known GTE result-read-latency cases (NCLIP MAC0 x4, LZCR x4; emulator fails them too via the latency model) + OP cross-product MAC1 (hardware 0xFFFFFBCE = -1074 vs spec -768; OP unused in cortex, game-irrelevant). MVMVA FC-bug MAC1/2/3 PASS (cv=FC fix confirmed on hw). | Rasterizer rewrite `2f4b0063`: center-sampled Mednafen/DuckStation DDA + determinant-plane interpolation in `gpu.rs` (`for_each_tri_pixel`), Redux scanline rasterizer retired. Verified in-emulator against all 12 recorded silicon hashes before burning (no-burn replay harness in `gpu/tests.rs`). | Triangle rasterization now matches silicon 100%. Resume the vertex-explosion hunt on the silicon-faithful emulator (re-test the player's stretch pose -- the rasterizer was the unmodeled downstream quirk class). OP MAC1 crackable later from 0xFFFFFBCE (pure accuracy). BURN GOTCHA: this drive EJECTS via software but cannot CLOSE -- `drutil tray close` is a no-op, the user must close manually; verify Space Used>0 only after a manual close. |

## Next Burn Template

| Field | Value |
| --- | --- |
| Entry ID | HWB-005 |
| Date / time |  |
| Git commit / diff summary |  |
| Disc label |  |
| Build command |  |
| Preburn report | `build/preburn/cortex_ignition_v1/BRINGUP_REPORT.md` |
| Burn media / speed / drive |  |
| Console model / BIOS / modchip |  |
| Video output / display |  |
| Last screen color |  |
| Last text checkpoint |  |
| Audio state | silent / CD-DA / SPU / unknown |
| Pad input route |  |
| Result |  |
| Required code change or gate |  |
