# psoxide-test

One test harness for every game on the SDK. A game declares what to build and check in
`psoxide-test.toml` at its repo root; the harness owns how: stage order, content receipts, the
emulator budget, pass and fail against the committed baseline (`psoxide-test.baseline.toml`), one
result file per run, the clean-clone proof and the install.

```
psoxide-test run [--repo DIR] [--tier quick|full] [--stage NAME]...
psoxide-test bless [--repo DIR]
psoxide-test lint [--repo DIR]
psoxide-test install [--repo DIR]
```

Stages, in this order for every game: `build` (the game's own commands, artifacts collected into
`out`), `size` (`psoxide_size::ram` of the link map: text, data, bss, free RAM), `host` (declared
host checks), `journeys` (the pinned `psoxide-gate`, one process per journey and hardware model,
longest first), `speed` (`psoxide-pgo measure` over poll windows at phase offsets), `cross` (the
smoke journey on PCSX-ReARMed and PCSX-Redux, RAM checkpoints only), `clean` (a fresh clone of the
pushed commit rebuilds the same bytes). `install` is its own command and installs through
`psx-library` only bytes a complete `--tier full` PASS judged.

`--tier quick` is the pre-check (build, size, the first window at offset 0, the `precheck`
journeys on `real`, the ReARMed smoke). Only `--tier full` can print `LANDING: COMPLETE`.

Every stage keys a receipt on the bytes it reads (artifacts, journey files, tapes, tool binaries)
and prints `RECEIPT HIT` instead of running again. Results land in
`<work>/<game>/results/<sha256>.json`, named by their content, with wall times beside them in
`<sha256>.timing.json`.

Journeys are the gate's own TOML. The harness refuses a journey without an explicit `pad`, keeps
the number of open-loop steps (fixed `wait`, frame-counted `hold`) as a ratchet in the baseline
(target 0; a new journey must have none), and replays the smoke subset (press until a RAM
condition, wait until one, check RAM at a named checkpoint) on the other emulators.

Machine-local paths and the budget live in the user config: see `config.example.toml`.
