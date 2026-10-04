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

## MDEC decode: lengths are checked, not trusted

`psx_fmv::mdec::decode` was safe and programmed DMA0 with `len / 32`
blocks: an 8-word slice gave 0 blocks, which the controller reads as
65,536, and a length that was not a whole number of blocks or did not fit
the decode command's 16-bit count was silently truncated.

`decode` and `decode_start` now return `Result<_, psx_fmv::rle::RleLengthError>`
and refuse an empty length, one that is not a multiple of 32 words, one
over `psx_fmv::rle::MAX_WORDS` (0xFFE0) and, for `decode_start`, one longer
than the slice; nothing reaches the MDEC then. `decode_frame` output always
passes. No game calls either function, so there is no forwarder.

## Polygon clip: a safe checked form and a provable unchecked one

`psx_math::attributed_clip::clip_convex_plane` was `unsafe` even with
`CHECK_CAPACITY = true`, and its docs called `source.len() + 1` output slots
sufficient. That holds only for a convex source and a plane whose `inside`
answer is stable per vertex; a zigzag source emits two vertices per source
vertex, so an unchecked caller that trusted the docs wrote past its buffer.

| Old | New |
| --- | --- |
| `unsafe { clip_convex_plane::<_, _, true>(src, dst, plane, t) }` | `clip_to_plane(src, dst, plane, t)` (safe, `Result<usize, ClipOverflow>`) |
| `unsafe { clip_convex_plane::<_, _, false>(src, dst, plane, t) }` | `unsafe { clip_to_plane_unchecked(src, dst, plane, t) }` |
| `clip_convex_plane_uninit::<_, _, true>` | `clip_to_plane_uninit` (safe) |
| `clip_convex_plane_uninit::<_, _, false>` | `clip_to_plane_uninit_unchecked` |

The unchecked forms require `2 * source.len()` slots, or `source.len() + 1`
for a convex source and a stable plane. The checked forms report an overflow
as `Err(ClipOverflow)` where the old checked form returned the partial count;
the deprecated forwarders keep the old result.

Callers of the old names: PSoXide-editor `psx-bsp/src/render.rs` (1,
unchecked), `psx-goldsrc/src/render.rs` (3, checked) and its legacy oracle
(3, checked); quake-psx `game/src/renderer.rs` (1, `_uninit`, checked);
voxide `game/src/main.rs` (1, checked).

## HMD8 body compaction: the caller's buffer by `&mut`

`Model::compact_visible_body_frames_raw(&self, data, ..)` rewrote the bytes
of the blob that `self` (a `&'static [u8]`) still read between the writes.
Every caller passed the model's own RAM, so a shared reference was live
across writes to its memory: undefined behaviour under both of Rust's
aliasing models, whatever the contract said.

`Model::compact_visible_bodies(blob: &mut [u8], visible_bodies: u8, remap: &mut [u16]) -> Option<usize>`
takes the buffer exclusively, reparses the layout from it and never holds a
`Model` while it writes. It is safe and returns the same bytes, remap table
and length as the old call (64 fixture configurations compared byte for
byte before the old body was replaced). The deprecated
`compact_visible_body_frames_raw` now forwards to it.

Callers: hl-psx `game/src/main.rs` (`md` parsed at line 4867, compacted at
4909) and cs-psx `game/src/main.rs` (4621, 4663), identical code. In both,
replace the `compact_visible_body_frames_raw` block with:

```rust
// `md` borrows these bytes; it is not used again from here.
let blob = core::slice::from_raw_parts_mut(buf_ptr.add(gw).cast::<u8>(), glen);
let remap = core::slice::from_raw_parts_mut(
    core::ptr::addr_of_mut!(MODEL_SCRATCH).cast::<u16>(),
    MAX_MODEL_VERTS,
);
if let Some(compact_kept) = Model::compact_visible_bodies(blob, visible_bodies, remap) {
    kept = compact_kept;
    // the face-index loop as before, reading `remap[i]` instead of
    // `core::ptr::read(remap.add(i))`
}
```

`md` must not be used after the call (it is not today). The face-index
loop may keep its raw reads; indexing the slice adds a bounds check per
face at load time only. `streamed_model_bytes_at` hands `md` the KSEG0 alias
of the same RAM (`canonical_ram_const`), so the two pointers differ in
address but not in memory: the rule above is what keeps it sound.
