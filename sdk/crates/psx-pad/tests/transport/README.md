# Controller transport regression

Run from the SDK repository root with Python 3 and rustc:

```sh
python3 sdk/crates/psx-pad/tests/transport/check.py sdk/crates/psx-pad/src/lib.rs /tmp/psx-pad-transport
```

The harness compiles the actual driver and tracker source, replacing only hardware register access with deterministic schedules. Abstract MMIO ticks model RX completion followed by ACK assertion/release; they are not calibrated SCPH-110 timing. No unexpected Select input or corrupt reply bytes are injected. The tests verify no next-byte write before readiness, complete-packet failure/retry behavior, final-byte handling, current-ID packet lengths, absence and reconnection. Results and source hashes are written under the supplied output directory.

An optional `--input-source /path/to/celeste/shared/src/input.rs` also compiles the actual collection input filter (only adapting crate paths). Its additional cases check real Select input, preserving the last clean state during invalid polls, and a later fresh Cross edge.

These contracts establish a protocol fix, not proof that physical navigation is fixed. The candidate still requires the reported SCPH-110 analog-toggle route on a console.
