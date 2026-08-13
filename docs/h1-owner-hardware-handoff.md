# H1: original-PlayStation acceptance, owner handoff

Updated 2026-08-13 for the Comicon playable-beta pressing.

H1 is deliberately owner-run. Everything completed so far is host-test,
real-MIPS-build, or deterministic-emulator evidence. **None of it proves the
original PlayStation.** Silicon is the authority for BIOS boot, CD timing,
DMA interaction, GPU pacing, SPU envelopes, CD-DA byte order, controller
timing, stability, or frame rate.

Read this with:

- `docs/hardware-test-disc.md` for the on-disc measurement suite and QR
  capture transport;
- `docs/hardware-test-versions.md` for suite history;
- `docs/comicon-playable-beta-handoff-2026-08-13.md` for the complete software
  evidence boundary and deferred work.

## 1. Exact burn candidate

Burn the already-built default pressing at this exact path:

```text
/Users/ebonura/Downloads/ps1 games/PSoXide Demo Disc/PSoXide Demo Disc.cue
```

It is the ordinary eleven-program disc plus the launcher CREDITS card, for 12
visible entries. Quake 1.06 shareware is included by default as the last
program before Credits. Half-Life is absent; `HL=1` is the only optional
variant.

Verify the existing files before touching a blank:

```sh
cd "/Users/ebonura/Downloads/ps1 games/PSoXide Demo Disc"
shasum -a 256 \
  "PSoXide Demo Disc.cue" \
  "PSoXide Demo Disc.bin" \
  "PSoXide Demo Disc.quake-provenance.json"
```

Expected values:

| File | Bytes | SHA-256 |
| --- | ---: | --- |
| `PSoXide Demo Disc.cue` | 507 | `b349a5871697fe49e1a79ef62bd82e5c4167a2eb7a54c363770063fba57b52fb` |
| `PSoXide Demo Disc.bin` | 226,742,208 | `105de3bb062185fd1fa7bfaec7ec7fda30197255168bb8616228e02cb901d57e` |
| `PSoXide Demo Disc.quake-provenance.json` | 3,166 | `3decf160d9c442a4245b29380b850b204bc87877fc5a5aa3e1caf4988a6b74c8` |

The image is 96,404 whole 2,352-byte sectors, about 216.2 MiB, with one Mode
2 data track and seven CD-DA tracks. Stop if any hash differs. A different
artifact needs a new receipt, new hashes, and a fresh headless check; do not
silently treat it as this candidate.

The candidate and its local source context are:

| Component | Worktree / revision |
| --- | --- |
| Demo-disc source | `/Users/ebonura/Desktop/repos/psx-demo-disc`, `main` at `2061540e234da16fd7a8378b0fc61f5d810ddd68` |
| Quake source | `/Users/ebonura/Desktop/repos/quake-psx`, `codex/all-rust-quake` at `28507a6dd605730a43909d6b2258f081def68a79` |
| Shipping PSoXide engine/program pin | `/Users/ebonura/Desktop/repos/PSoXide-final-pin` at `79d51dd2f2fd78cfb8aa418e2ad123730f56ac3d` |
| Later editor/UX head | `/Users/ebonura/Desktop/repos/PSoXide`, `main` at functional/docs promotion base `b2db85c4b86c3c1ea9748a92d39c85dff8b7610d`, plus this docs-only descendant |

These are the promoted canonical local worktrees. No repository was pushed.
The preservation refs, stash objects and pre-promotion owner work are recorded
in section 12 of the main handoff.

The receipt itself binds the combined BIN/CUE, Quake source and artifact
hashes, Quake's PSoXide input pin and the ordinary-program PSoXide revision.
The demo-disc revision comes independently from the launcher identity embedded
in the BIN and the live local Git state; the later editor head is contextual
and is not a receipt field.

The later editor head is where level authoring continues. It is not the
PSoXide revision from which this already-pressed Cortex guest was built. The
disc deliberately keeps `editor/samples/cortex_v1`, the legacy grid project,
as the visible fallback. It does not contain the new BSP
`souls-bsp-vertical-slice`. Do not conflate testing the current editor with
testing the exact on-disc guest.

The launcher identifies itself as `v0.19-32-g2061540`; the Quake menu build
identifies itself as `q28507a6`. The provenance receipt records Quake EXE LBA
6601 and proves the 9,476 embedded Quake sectors match its pinned standalone
image apart from required MSF relocation.

The final combined cue also chain-loaded the legacy Cortex fallback twice in
the headless emulator with identical route, CD-command and PC-sample logs.
That closes the software chain-load question, but it is still an HLE first-EXE
test rather than BIOS, physical-disc or silicon evidence.

## 2. CD-DA byte order: prove it before burning

This cue stores the data and audio tracks in one `BINARY` file. The `.cdda`
sources and the combined BIN contain little-endian PCM. With this TSSTcorp
SN-208AB drive, cdrdao's non-raw `generic-mmc` path must therefore use
`--swap`. Omitting it produces high-volume static while leaving the data
track apparently healthy.

For this exact candidate, Track 02 starts at cue sector 16,205
(`03:36:05`). The following compares one raw sector ten seconds into that
track with the same sector of its source. Silent success and exit status zero
are the expected result:

```sh
cmp \
  <(dd if="/Users/ebonura/Downloads/ps1 games/PSoXide Demo Disc/PSoXide Demo Disc.bin" \
      bs=2352 skip=16955 count=1 2>/dev/null) \
  <(dd if="/Users/ebonura/Desktop/repos/psx-demo-disc/audio/knuckle-dust.cdda" \
      bs=2352 skip=750 count=1 2>/dev/null)
echo $?
```

This comparison was green for the named artifact. If the cue is rebuilt,
derive the new Track 02 `INDEX 01` sector from that cue instead of copying the
old skip value. Compare at least ten seconds into the track because its head
is silent.

Do not take track numbers from the demo repository's `audio/README.md` for
this pressing. Its 32-35 labels describe an older/larger layout; the final
default cue itself is authoritative and places these songs at Tracks 02-05.

Do not use `cdrdao show-data` as an audio oracle. It starts at byte zero of
the combined file, which is the Mode 2 data track, and can make sector sync
bytes look like swapped audio samples.

## 3. Canonical cdrdao burn procedure

The measured primary path for the TSSTcorp SN-208AB USB burner is cdrdao's
non-raw driver with `--swap`. It does not need `sudo`.

1. Attach the burner and identify it:

   ```sh
   cdrdao scanbus
   ```

   The last measured address was `0,0,0`. If `scanbus` reports a different
   address, substitute that exact address below.

2. Insert a blank CD-R and confirm cdrdao reports it as empty:

   ```sh
   cdrdao disk-info --device 0,0,0 --driver generic-mmc
   ```

3. Re-run the three SHA-256 checks and the raw Track 02 comparison above.

4. Burn from the cue's own directory so its relative BIN path resolves:

   ```sh
   cd "/Users/ebonura/Downloads/ps1 games/PSoXide Demo Disc"
   caffeinate -i -s cdrdao write \
     --device 0,0,0 \
     --driver generic-mmc \
     --swap \
     --speed 10 \
     -n \
     --eject \
     "PSoXide Demo Disc.cue"
   ```

5. Wait for cdrdao to complete and eject. Do not interrupt it because the
   terminal appears quiet; a backgrounded cdrdao may buffer all output until
   completion. Reinsert the disc and run `cdrdao disk-info` again. It must no
   longer report an empty blank.

6. On the console, listen to at least one launcher music track before testing
   games. Music, not loud broadband static, is the byte-order acceptance.

Never use `generic-mmc` without `--swap` for this image. Never combine
`--swap` with `generic-mmc-raw`. A raw-DAO fallback is only for an owner-chosen
retry with elevated real-time privileges:

```sh
sudo cdrdao write --device 0,0,0 --driver generic-mmc-raw \
  --speed 10 -n --eject "PSoXide Demo Disc.cue"
```

On this drive, `generic-mmc-raw` without `sudo` has previously completed power
calibration and then failed while writing the lead-in. A failed raw attempt
left the CD-R empty, but verify that with `cdrdao disk-info`; never assume the
blank is reusable. The non-raw `generic-mmc --swap` command is the primary
procedure.

If a post-burn macOS audio rip is used as a second oracle, remember that its
AIFF-C `sowt` payload is little-endian despite the AIFF name: a raw musical
needle matching the original `.cdda` means the burn is correct; only a
byte-swapped needle matching means the burn is wrong.

## 4. Cold-boot and carousel acceptance

Record console model, modchip/boot method, burner, media brand, burn date,
and the three candidate hashes. Test from a true power-off, not only reset.

- [ ] Cold boot through the real BIOS reaches the launcher.
- [ ] Launcher version reads `v0.19-32-g2061540`.
- [ ] Twelve visible entries are present: Cortex Ignition, Voxide,
      NitroXide, Celeste Collection, PSXcel, GH-PSX, Breakout, Space
      Invaders, Magikaaaaarp Pong, Hardware Tests, Quake Shareware, and
      Credits.
- [ ] Left/right navigation, Cross selection, Up/Down language toggle, and
      L1/R1 music switching respond correctly.
- [ ] Launcher CD-DA is clean music with no byte-swapped static, looping,
      seeking, or long hang.
- [ ] Launch one small non-streaming title and reboot, proving the
      shell and ordinary chain-loader work independently of the two candidates
      below.

The menu test is not cosmetic. Watch for tearing, delayed panel drawing,
palette corruption, missing glyphs, input lag, and music/drive contention.

## 5. PSoXide fallback acceptance: Cortex Ignition

The on-disc entry is the owner's original legacy grid-based souls-like tech
demo, not Quake content and not the new BSP slice. It stays on the disc as the
known fallback until the replacement tech demo is ready. Test this exact guest
before judging the later editor.

- [ ] Select `CORTEX IGNITION`; the loader completes and gameplay appears.
- [ ] The legacy grid world, materials, lighting, player, Rust Mantis and
      attached weapons render without corruption.
- [ ] Left-stick movement and right-stick camera feel responsive, with stable
      collision through the part of the grid level exercised.
- [ ] The player and Mantis skeletal animations remain coherent and attached
      weapons follow their authored sockets.
- [ ] Exercise the combat actions that the legacy build exposes. Record what
      is actually present rather than expecting the newer BSP slice's exact
      combat/checkpoint script.
- [ ] Listen for one-shot and looping SFX problems: endless repeats, missing
      release, distortion, or voices that never stop.
- [ ] Reboot to the launcher and chain-load Cortex a second time. Repeat the
      same movement, camera, animation and available-combat checks.

Do not require BSP geometry, the new slice's door, `SYNC RELAY` checkpoint,
lava, death/reset sequence, or BSP project telemetry from this fallback. Those
belong to `souls-bsp-vertical-slice`, which is not pressed on this candidate.

Do not turn this into a claim that the latest native editor UI passed on
hardware. Editor selection, brush manipulation, UV controls, cooking, and
Rebuild & Play are a separate desktop-owner acceptance against canonical
`/Users/ebonura/Desktop/repos/PSoXide`.

## 6. Quake Shareware acceptance

Quake is a playable-beta candidate. All nine shareware maps cook and load in
software evidence, but this campaign has not certified every normal and
secret route start-to-finish. The purpose of this console run is to find the
first real blocker, not to manufacture a completion claim.

- [ ] Select `QUAKE SHAREWARE`; the loader completes and Quake Start appears
      with build identity `q28507a6`.
- [ ] Choose New Game and Easy. Start's slipgate reaches E1M1.
- [ ] Verify left stick/D-pad movement, right-stick look, R2 fire, Cross jump,
      Square use, Triangle/Circle weapon cycling, and Start pause.
- [ ] In E1M1, verify collision, steps, doors/buttons, pickups, combat,
      damage/death, and the map exit.
- [ ] In E1M2, exercise the lift, shootable button, bridge, silver key/target,
      locks, floorplate, and exit. This is the strongest durable ordinary-input
      route beyond E1M1.
- [ ] Exercise representative moving platforms, rider carrying, teleports,
      water/hazards, key locks, monster combat, weapon switching, ammo and
      armor across later maps.
- [ ] Reach E1M7 if practical and verify the Chthon/lightning sequence and
      intermission. If secret-episode completion will be claimed, take E1M4's
      secret exit and test E1M8 too.
- [ ] Watch for actual hard blockers: an impassable mover, non-firing target,
      unusable shootable button, bad body block, map-load failure, crash, or
      permanent CD stall. Record the map, location, prior actions, and exact
      symptom.
- [ ] Record frame pacing during busy combat and wide views. “Playable” is the
      current requirement; sustained 30 FPS has not been claimed.
- [ ] Reboot to the launcher and chain-load Quake a second time through Start
      into E1M1.

Quake CD music, save/continue, registered-game content, perfect presentation,
and exhaustive parity are deferred. Their absence is not a disc failure.
Quake SFX, input, gameplay and data streaming are expected to work.

## 7. Silicon-specific observations

For both games and the launcher, explicitly record:

- [ ] GPU: tearing, pacing, dropped frames, palette errors, geometry/texture
      corruption, or stale frames.
- [ ] CD/DMA: seek stalls, repeated retries, streaming hangs, delayed map
      loads, or audio/data contention.
- [ ] SPU/CD-DA: byte-swapped static, one-shots that loop forever, stuck
      voices, missing release, broken spatial loops, or track seek failures.
- [ ] Input: missed/latched buttons, bad analog centering, or timing-sensitive
      actions that differ after reset.
- [ ] Stability: leave the heaviest repeatable workload running for several
      minutes and note cadence, thermals/drive behavior, and any crash.

Capture video when possible. Emulator counters cannot substantiate a console
FPS claim; use an on-disc overlay where available or count delivered frames
from the console capture.

## 8. On-disc hardware-test battery

The combined disc already includes the PSoXide hardware suite as `HARDWARE
TESTS`; no second burn is needed.

- [ ] Reboot, select `HARDWARE TESTS`, and wait through the chain-load.
- [ ] Run `RUN ALL TESTS + CAPTURE`.
- [ ] Photograph or film every QR page according to
      `docs/hardware-test-disc.md`.
- [ ] Do not run the memory-card diagnostic unless you accept its explicit
      card-write risk and consent flow.
- [ ] Decode the capture with `tools/hwtest-video-qr.py`.
- [ ] Compare the decoded payload against the emulator measurement:

  ```sh
  cd /Users/ebonura/Desktop/repos/PSoXide-final-pin
  make hwtest-silicon SILICON=/absolute/path/to/decoded-payload.txt
  ```

Run the full-characterisation capture when INFO observations are required;
the conformance capture carries failing records rather than every INFO probe.

## 9. Evidence to return

The minimum useful hardware report is:

- the three exact artifact hashes and the provenance JSON;
- console, modchip/boot method, burner, media, burn command and date;
- one line per checklist item: PASS, NOT RUN, or the exact symptom;
- video timestamps or stills for visual/performance disagreements;
- decoded hardware-suite payload and `hwtest-silicon` diff;
- for a Quake blocker: map, coordinates/landmark, preceding actions, input,
  whether it reproduces after a cold boot, and whether other maps still load;
- for an audio failure: whether it affects launcher CD-DA, game SFX, or both.

Keep the receipt with the physical disc. It is the durable link between the
burn and the source/artifacts that produced it.

## 10. If hardware disagrees

Silicon wins. Do not change game behavior to imitate the emulator. First
separate content, disc-layout, drive/media, and engine causes. If necessary,
burn an unchanged known-good control image on the same media spindle; this
console has shown outer-radius read sensitivity before.

Record confirmed emulator gaps in `docs/emulator-accuracy-from-silicon.md`.
Send game/runtime defects to their owning repository and disc-layout or loader
defects to the demo-disc repository. Do not hide a hardware failure with a
release-layer workaround.

## 11. Rebuilding only if the candidate changes

The current artifact is already built and headlessly checked. If it must be
regenerated, use the exact clean inputs:

```sh
cd /Users/ebonura/Desktop/repos/psx-demo-disc
make disc
make check
make quake-headless-check \
  FRONTEND=/Users/ebonura/Desktop/repos/psx-demo-disc/games/PSoXide/target/release/frontend
```

The default `QUAKE_SRC` resolves to canonical sibling
`/Users/ebonura/Desktop/repos/quake-psx`; no temporary worktree override is
needed. The canonical rebuild, repository check and deterministic Quake
headless gate were green for the exact candidate in section 1. The legacy
Cortex fallback also produced two byte-identical passes with the telemetry
recorded in the main handoff.

Use the demo repository's exact-pinned `games/PSoXide` submodule for a full
disc build. Do not pass the detached `PSoXide-final-pin` worktree as
`PSOXIDE`: the current hydration recipe can self-target that external
checkout while staging ordinary programs. That worktree had to be restored at
`79d51dd2` after this trap was observed.

Then recalculate every combined-disc hash, inspect the new receipt, rerun the
Track 02 byte comparison using the new cue's index, and update this handoff
before burning. The headless gate uses an HLE first-EXE path and the pressed
launcher/loader/Quake payload; it remains emulator evidence, not BIOS or
silicon evidence.

## 12. Status

H1 is **OPEN**. No original-PlayStation result exists for this exact candidate
until the owner returns the completed checklist and hardware-test capture.
The software freeze is a burn candidate; it is not hardware-certified and it
does not certify the entire Quake episode start-to-finish.
