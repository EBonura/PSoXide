+++
title = "psx-seq"
description = "Sequenced music on the SPU from a script interpreter"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-seq` to play music that is a program, not a recording. It runs the sequence bytecode that some Nintendo 64 games use: a sequence script starts up to sixteen channel scripts, each channel starts up to four layer scripts, and the layers hold the notes. Scripts branch, loop, call, and read and write IO ports that the game reads and writes too, so a tune can follow the player: mute a channel, change with the time of day, or fade a part in with distance. That is why the bytecode runs at play time instead of being flattened to a list of notes.

## How the crate is organized

`Sequencer<P, N>` runs `P` players, which are independent songs such as background music and a fanfare over it. They share `N` SPU voices and compete for them by note priority. `Sequencer::new` takes a mask of the voices it may use, with exactly `N` bits set. The sequencer is several kilobytes, so a game that keeps one in a `static` builds it in place with `init_at`. `format` defines the two containers: a song (`PSQ1`), which is the bytecode behind a short header, and a bank (`PSQB`), which carries instrument and drum tables, envelopes, a sample table and the SPU-ADPCM data in one block that goes to sound memory in a single transfer. `NoteHandle` addresses a note the game starts itself. `tables` holds the lookup tables the interpreter uses. The engine is `no_std` and uses no heap, floating point or 64-bit arithmetic.

## Integration notes

Sound leaves through `psx_sfx::hw::Hw`, the same seam the [psx-sfx](@/docs/crates/psx-sfx.md) mixer uses, so one hardware implementation serves both. On a host, `Recorder` captures every register write in order, which is how the engine is tested.

The engine advances in updates at `UPDATE_HZ`, which is 180 per second. The caller owns the clock: `pump` takes the number of updates that have elapsed on whatever clock the game has, such as a timer interrupt or a counter polled from a wait loop, and runs the ones that are due. After a long stall it runs at most `MAX_CATCH_UP` and drops the rest, so a slow frame sounds like a gap in the music instead of a burst of notes.

Load a bank with `set_bank`, passing the sound-memory address its samples were uploaded to, then `start` a song on a player. The song is patched in place by some sequences, so it needs a mutable buffer in RAM. `set_player_io` and `set_channel_io` write the ports the scripts read, `set_channel_mute` and `fade_to` shape the mix, and `stop` takes a fade length. Gains are Q15, pitch multipliers Q16 and envelopes Q24. The original squares most volume terms (velocity, channel volume, envelope level), and the crate keeps that curve because it shapes the dynamics. `unsupported_ops` counts bytecode the interpreter skipped, which is worth checking when a new song sounds wrong.

The [hello-seq example](@/docs/examples/hello-seq.md) plays a cooked song and reports what the sequencer costs per update. It needs a song and bank cooked with `psx-seq-cook`, which are not committed.

## API, dependencies and source structure

{{<sdk_crate name="psx-seq" />}}
