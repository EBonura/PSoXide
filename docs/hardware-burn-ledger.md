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
