+++
title = "psx-cdstream"
description = "Interrupt-driven CD-ROM streaming: queued reads, abort, chaining and an audio lease"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-cdstream` to read data from the disc while a game keeps running. You queue reads; the CD interrupt handler pops one sector per drive interrupt straight into your buffer; the main loop only submits, cancels and asks how far a read has got. For reads that happen once, at boot, the polled `SectorReader` in [psx-io](@/docs/crates/psx-io.md) is still the simpler tool.

## How the crate is organized

`Engine` is the whole state machine and is generic over `CdHw`, the controller as the machine sees it. On the console, `install` puts an exception wrapper in front of psx-rt's handler that calls one global engine once per CD interrupt, and the free functions (`submit`, `cancel`, `state`, `request_audio_lease`) reach it. On the host, the crate's tests drive the same engine against a scripted drive, so every phase, cancel point, error path, chaining rule and lease transition runs without hardware.

A `Request` names a run of sectors and the memory they go to. `Priority` orders waiting requests, `Ticket` names a submitted one, and `RequestState` and `Completion` say where it is and how it ended.

## Integration notes

Every transfer is Setloc, SeekL, Setmode (double speed), ReadN: the sequence the BIOS uses. A bare Setloc plus ReadN began delivering while the mechanism was still settling and corrupted long streams on a console. Sectors are popped by the CPU, because chopped CD DMA was unreliable on a console; that costs CPU time while a read runs, so read in bursts when there is work.

`cancel` only sets a flag. The handler drops the next sector and pauses the drive itself, because a Pause sent from the main loop while sectors were arriving lost its acknowledge on silicon. The request ends `Cancelled` with the number of sectors that had landed, and `Request::remaining_after` builds the request that continues it. `Request::with_resumes` makes the transport do that itself after a drive error.

Up to `QUEUE_DEPTH` requests wait behind the active one, most urgent `Priority` first. A waiting request that starts at the sector after the active one's last continues without a Pause or a seek, so a group of regions laid out in reading order costs one seek.

CD-DA and XA playback cannot share the drive with data reads. `request_audio_lease` stops the read in flight, waits for the drive to stop, closes the CD interrupt source and lets you collect the controller token with `take_audio_lease`; `release_audio_lease` takes it back and queued reads carry on. End audio with Pause, not Stop: after Stop the motor spins down and reads started in the next second or two failed on a console.

`install` takes the controller token that `SectorReader::release` returns, so the compiler keeps the polled reader and the interrupt transport from using the drive at once. `uninstall` hands the token back.

The counters (`StreamStats`) are published as the `PSX_CD_STATS` symbol for tools that read guest memory. The `trace` feature also records the last 128 controller events.

The state machine and the scripted drive are covered by host tests. The console wrapper and the register sequences are the same ones the earlier game-local transports shipped, but this crate has been run only in the emulator so far; a console run is still to do.

## Example

[hello-cdstream](@/docs/examples/hello-cdstream.md) streams a known file off a disc through the crate and checks every byte: single and chained reads, priority, abort and resume, a sustained stream, an audio lease and a failing read.

## API, dependencies and source structure

{{<sdk_crate name="psx-cdstream" />}}
