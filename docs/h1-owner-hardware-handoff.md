# H1: original-PlayStation acceptance, owner handoff

This document exists because H1 cannot be completed by an agent. Every claim
in this campaign is host-test, real-MIPS-build, or deterministic-emulator
evidence. **None of it is hardware evidence.** Emulator agreement cannot
prove GPU pacing, DMA behaviour, CD seek and settle, SPU envelopes, CD-DA,
controller timing, or a real 30 FPS. Only the console can.

Read this together with `docs/hardware-test-disc.md` (the on-disc battery and
its QR capture transport) and `docs/hardware-test-versions.md`.

## 1. What to burn

The combined opt-in pressing is produced in the demo-disc worktree:

```sh
cd /Users/ebonura/Desktop/repos/psx-demo-disc-quake-shareware
make quake-disc
```

That target refuses to build against missing, dirty, stale, or mismatched
inputs; it emits `dist/<disc>.cue`, `dist/<disc>.bin`, and the receipt
`dist/<disc>.quake-provenance.json`. Confirm the receipt names the exact
PSoXide and Quake revisions you intend to test before burning, and keep it:
it is the only durable link between a physical disc and the source that made
it. The PAK, the Quake disc image, and the combined image stay uncommitted.

## 2. Burn procedure (the canonical protocol, do not improvise)

Every step below earned its place by ruining a disc.

1. Verify the blank first. `drutil -drive 1 status` must show
   `Space Used: 00:00:00` and full Space Free.
2. Burn from the image's own directory, sleep-guarded, at 10x, with the
   close and verify flags explicit:

```sh
cd /Users/ebonura/Desktop/repos/psx-demo-disc-quake-shareware/dist && caffeinate -i -s drutil -drive 1 burn -notest -noappendable -verify -speed 10 <disc>.cue
```

   `caffeinate` is mandatory (a burn suspended by sleep resumes on wake,
   prints "Burn completed", and produces an unreadable disc).
   `-noappendable` is mandatory (a session left open reads as an empty disc
   on the PS1 and cannot be closed after the fact).
3. Confirm "Burn completed" in the output.
4. Read back in the burner BEFORE going to the console: reinsert, wait about
   20 seconds, confirm `drutil status` shows Space Used near the image size,
   `Space Free: 00:00:00` (closed), and that the data track mounts. Byte
   compare at least one file against the build.

Failure signatures: pristine blank means it never wrote; "No Media Inserted"
forever means the session never closed (unrecoverable); Mac reads it but the
PS1 boots erratically means a weak burn, so reburn the identical image on a
different blank. The target console is modchipped, so the mkiso "no system
area" warning is expected and not a defect.

## 3. Console checklist

Work down this list and record what you see. A failure at any step is more
valuable than a pass, so write down the exact symptom rather than a verdict.

**Boot and shell**

- [ ] Cold boot from power-off reaches the carousel.
- [ ] Every carousel entry is present with the expected count and text.
- [ ] Controller input works in the shell; pause and resume behave.

**PSoXide souls entry**

- [ ] Chain-loads and reaches gameplay.
- [ ] The authored level renders: brush world, lights, liquid surface, props.
- [ ] Movement and camera collide correctly; the door opens and blocks.
- [ ] The equipped weapon is attached and animates with the body.
- [ ] Combat connects, staggers, and kills the enemy.
- [ ] Lava damages and kills; respawn returns to the checkpoint, not spawn.
- [ ] Return to the carousel and chain-load it a SECOND time; it behaves the
      same.

**Quake entry**

- [ ] Chain-loads and reaches Start.
- [ ] A representative map change works.
- [ ] Worst-case combat runs without visible stalls.
- [ ] Water and hazards behave; damage and death behave.
- [ ] Chthon and the episode ending behave (when Q4 has landed).
- [ ] Intermission and return to Start behave (when Q4 has landed).
- [ ] Boot Quake a SECOND time from the carousel and confirm it reaches the
      same authored checkpoint.

**Silicon-specific observations (the reason this step exists)**

- [ ] GPU: tearing, pacing, dropped frames, geometry or texture corruption.
- [ ] DMA and CD: seek behaviour, streamed reads, any stall or hang.
- [ ] SPU: one-shot sounds, spatial loops, voice lifetime, CD-DA if used.
- [ ] Long run: leave the worst workload running and note stability and
      cadence over several minutes.

**Hardware battery**

- [ ] Run the on-disc `HARDWARE TESTS` entry, select `RUN ALL TESTS +
      CAPTURE`, and photograph the QR pages per `docs/hardware-test-disc.md`.
- [ ] Decode and diff against the current baseline (`make hwtest-diff`).

## 4. Expected evidence to bring back

For the campaign to record a hardware result, the following is enough:

- the receipt JSON of the exact disc you burned;
- a note per checklist line: pass, or the precise symptom;
- decoded hardware-battery output and its diff against the baseline;
- for any performance claim, a measured cadence from the console rather than
  an impression, using the on-disc FPS overlay where available;
- photographs or capture-card stills for anything visual that disagrees with
  the emulator.

## 5. Rules when the console disagrees with the emulator

Silicon wins. Do not change game behaviour to match the emulator when the
console disagrees. Record confirmed emulator gaps in the established
hardware documentation (`docs/emulator-accuracy-from-silicon.md`) so the
divergence is tracked rather than silently absorbed. A console failure sends
the work back to the owning work package, not to a workaround in the release
layer.

## 6. Status

H1 is OPEN. No console run has occurred in this campaign. Nothing in the
handoff, the validation logs, or the milestone names should be read as a
hardware claim until this checklist comes back filled in.
