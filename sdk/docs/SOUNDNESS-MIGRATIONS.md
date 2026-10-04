# Soundness migrations

API changes made to close soundness holes, what replaces the old call, and
which games still call the old one. The old item stays as a deprecated
forwarder for one stage, so a repin compiles with a warning that names the
replacement; it is removed once no game's `main` calls it.

Caller counts are lines on each repo's local `main` as of 2026-10-04
(`git grep`).

## Command recording: `start_recording_raw` returns the recording

`psx_io::gpu::end_recording()` was safe and did not check that a recording
was open, so a second call wrote the end tag through the previous buffer's
pointer; a `RecordingPause` dropped after its recording ended reopened it.
Both reached freed memory from safe code (Miri: "pointer is dangling").

| Old | New |
| --- | --- |
| `unsafe { begin_recording_raw(buf, words) }` | `let recording = unsafe { start_recording_raw(buf, words) };` |
| `end_recording()` | `recording.end()` |

`ActiveRecording::end` consumes the guard, so a recording cannot be ended
twice; dropping the guard ends it too. A closed recording never reopens, and
the deprecated `end_recording` is a no-op (`Ok(None)`) when nothing is open.

Callers of the old pair: PSoXide-editor `engine/crates/psx-engine/src/present_queue.rs`
(2 `begin_recording_raw`, 2 `end_recording`). The change there is mechanical:

```rust
let recording = unsafe { psx_io::gpu::start_recording_raw(overlay, overlay_words) };
scene.render_overlay(ctx);
let recorded = recording.end();
```
