# Miri

`make miri` runs every SDK crate's host tests, and psx-hw's, under Miri with
Stacked Borrows and again with Tree Borrows, on the nightly pinned in
`rust-toolchain.toml`; it needs `rustup component add miri`. CI runs it as
the `miri` job. Nothing is expected to fail: a Miri report is a bug.

Two flags apply to both models:

- `-Zmiri-permissive-provenance`: a DMA link is a 24-bit integer on the
  console, so the ordering-table and chain code turns addresses into
  integers and back on purpose.
- `-Zmiri-ignore-leaks`: tests leak boxes on purpose to get the
  `&'static [u8]` blobs the asset parsers take. A leak is not undefined
  behaviour.

## Skipped under Miri

Exhaustive sweeps (every Q11 code, every shift, hundreds of thousands of
random oracle inputs) take minutes to tens of minutes each when interpreted.
They carry `#[cfg_attr(miri, ignore = "...")]`, so Miri reports them as
ignored and `make test` still runs them natively. They check arithmetic over
a domain; the memory accesses they share are covered by smaller tests that
Miri does run.

Add the attribute only to a sweep whose memory behaviour another test
already covers, and keep the reason string.
