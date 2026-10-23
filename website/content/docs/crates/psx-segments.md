+++
title = "psx-segments"
description = "Segment selection and launch requests for a game's debug page"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-segments` when a game needs a list of places to start from, for testing or for coverage runs, without every game reinventing the table and its rules. It supplies bounded selection and a versioned launch request. The game draws the rows itself, in its existing debug page or behind a hidden title combo, and keeps the page outside the normal menu flow.

## How the crate is organized

A game defines one table of `Segment` rows. Each row has a stable identifier, a display name, a load target, a spawn description and a `Progression`. `Progression::Verified` carries the full game-owned state decoded from a retained normal-play save, together with a `Provenance` record: the save version, the capture reference, its SHA-256 and a playthrough receipt. `Progression::Unverified` carries the reason the evidence is missing. `LaunchRequest` is the control message, `validate` and `consume` turn it into a `Launch`, and `Selection` moves a cursor through the rows.

## Integration notes

The crate checks that the metadata is well formed. It does not read files or prove where a capture came from, so the game must check that the evidence matches the state before it calls a row verified. Every other row is `Unverified`, with the actual reason. Such rows can offer a separately labeled diagnostic reset, whose callback receives only the load target and spawn description. It cannot pass for an independent progression launch and does not count toward coverage.

A `LaunchRequest` is three little-endian 32-bit fields when sent as bytes: version, segment identifier and mode. The zero request means idle. Playthrough mode needs a verified row with valid evidence, and diagnostic mode needs the row's explicit diagnostic availability. Display order is not the wire identifier, and a table with duplicate identifiers is rejected whole before anything is dispatched. The game owns the mailbox symbol, the byte decoding, atomic publication, volatile reads and clearing. Clear the mailbox, then call `consume` on a local copy. `consume` clears that copy before it validates or calls either game callback, returns an explicit result and never replays a failed request.

Selection has no font, renderer, controller polling, heap or game-state storage. Use `Selection::next`, `previous` and `visible_range` with the game's own list drawing, and apply a borrowed captured state through the same restore path that normal save loading uses.

There is no standalone `psx-segments` example in the SDK set.

## API, dependencies and source structure

{{<sdk_crate name="psx-segments" />}}
