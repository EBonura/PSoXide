# Measuring a guest performance change

Proving a change is faster is easy. Proving it draws the same thing is where
this project has repeatedly lost time, so the order below matters. Written
after the 17 September hl-psx pass, where two of the three conclusions I
reached by inspection turned out to be wrong and only measurement caught them.

Companion: [ps1-performance-research-2026-09-17.md](ps1-performance-research-2026-09-17.md)
for what the console says each technique costs.

## 1. Check the CPU is the wall at all

`--gpu-frame-stats-log` records `gpu_cycles` per route tick. Sum it over the
gameplay window and divide by the route log's `bus_cycle_delta` over the same
ticks. hl-psx on the chapter-two tape: the GPU is busy 31% of wall, so there
is about 3x of headroom and every worthwhile lever is CPU-side. Near 1 and the
CPU work is irrelevant; the answer is fewer or cheaper primitives.

## 2. Know the noise floor before believing a delta

The same commit built from two different checkout paths differs by about 0.4%
in rendered FPS, because the path changes the crate disambiguator and so the
entire code layout. Compare only builds made in one directory, and treat any
sub-1% delta as unproven without an instruction and stall breakdown.

## 3. Final-frame hashes do not gate an hl-psx change

`decoupled-present` samples the camera at render time, so any change in speed
moves the final image legitimately. The coupled build (`--no-default-features`)
still free-runs vblanks and is not exempt. Two builds differing only in the
delay-slot flags, which are provably behaviour-preserving, end the tape on
different hashes.

## 4. Find a speed-insensitive checkpoint before comparing frames

Build the same source twice, the second time with a no-op counter added: a
static plus an `#[inline(always)]` increment on a hot path. It is
behaviourally identical and measurably slower. Compare the two at a given
`--stop-at-poll`.

Where they agree byte for byte, that checkpoint does not depend on execution
speed, and a difference there from a real change is a real change. Where they
disagree, the checkpoint is useless and says nothing about correctness.

On the hl-psx chapter-two tape, poll 1500 is speed-insensitive and poll 4500
is not. Without this step every frame comparison is unfalsifiable: a
difference can always be blamed on cadence, and an agreement can always be
luck.

## 5. Separate a changed decision from a changed value

Sum `textured_tris`, `textured_quads` and `commands` from the GPU frame stats
up to the checkpoint.

- Different counts: a culling or subdivision decision moved.
- Same counts, different pixels: arithmetic moved.

That single number halves the code you have to read. In the vertex-narrowing
experiment it showed 2% more textured triangles, which pointed straight at the
affine subdivision metric rather than at the colour maths.

## 6. Gate simulation separately from rendering

Build both sides with `HLPSX_LINK_MAP`, dump RAM at tape end with
`--dump-ram`, and compare every same-named data symbol through the two link
maps. Gameplay statics must match exactly. Packets, projection scratch,
ordering tables and frame tokens are expected to differ and carry no
information.

Watch for LLVM shrinking a `static mut bool` into `.bss` with inverted sense;
the map shows it as `NAME (.0)` and zero there means the initial value.

## 7. Two traps that cost real time on 17 September

**Divide samples by call count before believing a hot site.** A `memcmp` site
carrying 0.38% of samples looked worth fixing. It sits behind a once-per-frame
call, which is about ten thousand calls across the whole route and cannot
matter; the site actually taking the time ran 1.4M times. Matching a plausible
shape in the disassembly is not identification. The rewrite measured 0.14%,
which is noise.

**After narrowing an integer field, grep for `+`, `-` and `*` on it.**
`u8 + u8` is valid Rust that wraps silently in release builds, so
`(a.rgb.0 + b.rgb.0) / 2` compiles and returns garbage for any sum above 255.
The compiler reports every type mismatch and none of these. This one corrupted
every subdivision midpoint colour and UV while still building and still
running.

## 8. Profile-guided builds

`psoxide-pgo` (in the SDK) turns the emulator's exact instruction counts into
an LLVM sample profile, so a guest can be profile-guided with no
instrumentation of its own. Build with `-Cdebuginfo=1
-Zdebug-info-for-profiling -Cstrip=none`, link a DWARF ELF twin by dropping
`--oformat=binary`, replay a representative tape with `--pc-sample-log
--pc-sample-instructions 61`, convert, then rebuild with
`-Zprofile-sample-use`.

hl-psx measured +6% over the delay-slot flags alone on the trained route and
+4% on a route the profile never saw, almost all of it from I-cache refill
stalls falling by a fifth against a 4 KB direct-mapped cache. Its
`cargo run --release -- pgo --tape PATH` runs the whole loop in four minutes.

Two things to know. Profile symbol names carry crate hashes that depend on the
checkout path, so a profile belongs to the directory that produced it and is
regenerated rather than committed. And `--config
target.<triple>.rustflags=[...]` appends to the flags already in
`.cargo/config.toml`, where an exported `RUSTFLAGS` would replace them.
