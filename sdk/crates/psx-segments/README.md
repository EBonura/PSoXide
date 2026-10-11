# Segment select

`psx-segments` supplies bounded selection and a versioned launch control
request. Games draw its rows in their existing Debug page or a hidden title
combo. Keep this page outside the normal menu flow.

A game defines one table with stable identifiers, display names, load targets,
spawn descriptions and full game-owned progression state. `Verified` means
that state was decoded from a retained normal-play save, with an actual capture
reference, SHA-256, save version and playthrough receipt. The game must check
that evidence matches the state before declaring it verified. This crate checks
metadata syntax; it does not read files or prove a capture's origin.

Use `Unverified` with the actual missing-evidence reason for every remaining
row. Such rows can offer a separately labeled diagnostic reset. That callback
receives only the load target and spawn description, and cannot pass for an
independent progression launch or promote coverage. Reuse the existing reset
behavior; never manufacture equipment to populate a captured-state entry.

The table also supplies identifiers and availability for coverage and journey
exports. Display row order is not the wire identifier. Duplicate identifiers
reject the entire table before dispatch.

`LaunchRequest` has three little-endian 32-bit fields when transported as
bytes: version, segment identifier and mode. The zero request is idle.
Playthrough mode requires verified state and valid evidence metadata;
diagnostic mode requires the row's explicit diagnostic availability. This
control channel is distinct from the game's versioned observed gate-state
block. The game owns the mailbox symbol, endian decoding, atomic publication,
volatile reads and clearing. Clear it before calling `consume` on a local copy.
`consume` clears that copy before validating or calling either game callback,
returns an explicit result and never replays a failed request.

Selection has no font, renderer, controller polling, runtime heap or game-state
storage. Use `selected` and `visible_range` with the game's existing shell and
list drawing primitives. Apply the borrowed captured state through the same
restore path used by normal save loading.
