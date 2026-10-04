# Correctness fixes that change behaviour

Bugs fixed in work package 5 whose fix a game can notice: an API that now
refuses input it used to accept, a value that now means something else, or a
call that used to hang and now returns. Each entry says what changed, which
games call the affected API, and what (if anything) to do on repin. Where an
old signature had to go, it stays as a deprecated forwarder for one stage.

Caller counts are lines on each repo's local `main` as of 2026-10-04
(`grep` over `*.rs`, excluding `target`, preserved and vendored copies).

## psx-mc: replacing a save no longer deletes it first (mc-01)

`Card::write`, `write_with_icon` and `write_compressed` freed the old
same-name file before they knew the new one fitted, so a `NoSpace` result
(or a card pulled mid-save) left no save at all. They now write the new file
into blocks that are free, put its directory entries down last-first so the
file only appears once its chain is complete, and release the old file after
that.

Behaviour change: the card needs room for both copies at once. A write that
does not fit returns `Error::NoSpace` and leaves the card untouched, where it
used to replace the old save in place. A game that keeps one save on a card
it also fills with other saves has to ask the player to free blocks first.

Callers: voxide `game/src/save.rs` (2 `write`), hk-psx `game/src/save.rs` (1;
its own two-copy journal already covers the torn-write case, and it now
also survives a refused overwrite), hl-psx `game/src/save.rs` (1), psxcel
`game/src/main.rs` (1 `write_compressed`), PSoXide-editor
`engine/examples/editor-playtest` (via `psx_settings`).

## psx-mc: directory checksums are verified on every read (mc-03)

`write_dir` has always kept each directory entry's XOR checksum, but only
the optional `validate_filesystem` read it back. `find`, `chain`, `list`,
`free_blocks`, `read`, `write` and `delete` trusted any entry with a
plausible state byte, so one flipped link byte could make `delete` free
another save's blocks or `write` allocate over them.

Every directory read now compares the stored checksum and returns
`Error::Corrupt` on a mismatch. There is no switch to turn it off. Cards
written by the BIOS, by other tools that follow the format and by this crate
carry valid checksums; a card whose entries do not is damaged, and a game
that meets `Corrupt` from `list` or `read` should offer the same recovery it
offers for a failed `validate_filesystem`. No code change is needed on
repin for a game that already handles `Error::Corrupt`.

## psx-mc: a card pulled during a compressed load reports the transport error (mc-04)

`Card::read` of a compressed save turned `NoCard`, `Protocol` and
`BadChecksum` from the card into "end of input" and then reported
`Error::Compression`, so a pulled card read as a codec bug. It now returns
the first transport error. A game that mapped `Compression` to "save is
damaged" and anything else to "card problem" gets the right message without
a change.

## psx-spu: `Volume` encodes the 15-bit level the SPU reads (spu-01)

`Volume` is written to the SPU as `value as u16`. Bit 15 of a volume
register is the sweep-mode select (psx-spx, "SPU Voice Volume"), so every
negative `Volume` switched the voice or the main output into sweep mode
instead of inverting the phase, and `Volume::linear(2, 1)` computed `0x7FFE`,
which the register reads as -2. The register write now limits the level to
`-0x4000..=0x3FFF` and stores it as a 15-bit two's complement, so bit 15 is
always clear and a negative `Volume` really inverts the phase.
`Volume::linear` saturates at `Volume::MAX` once `num >= den` (a zero `den`
included, which used to divide by zero).

The field stays public, so `Volume(x)` still compiles; only the encoding at
the register changed. A game that passed a negative `Volume` and got a
sweep now gets an inverted voice, and one that passed more than `0x3FFF` now
gets `0x3FFF`. Callers of the constructor or `Volume::linear` need no edit:
wipeout-psx `game/src/gameplay.rs` (3), nitroxide `game/src/audio.rs` (4),
voxide `game/src/sfx.rs` (5), hl-psx and cs-psx `game/src/settings.rs`
(1 each, `0x3FFF * s / VOL_MAX`), oot-psx (3), quake-psx `game/src/audio.rs`
(3 `linear`, all with `num <= den`), hk-psx (audio tests mock their own
`Volume`).

## psx-rt: `wait_vblank()` no longer replaces a game's exception vector (rt-01)

`wait_vblank()` is safe, and on first use it installed psx-rt's handler over
whatever was at `0x8000_0080`, then reset `I_MASK` to VBlank only. A game
that had put its own handler there (hk-psx and cs-psx wrap psx-rt's to
service CD interrupts) and called `wait_vblank()` before
`install_vblank_counter()` lost its handler without a word.

`_start` now records the vector word the program booted with. The lazy
install happens only while the vector still holds that word or already
jumps to psx-rt's handler; otherwise `wait_vblank()` leaves the vector
alone and waits for the count a chained psx-rt handler advances. Programs
that call `install_vblank_counter()` first (every game below, and the
common route) are unchanged, as are the SDK examples that rely on the lazy
install.

A game with its own handler that never calls `install_vblank_counter()`
and whose handler does not chain to psx-rt's will now wait on a counter
nothing advances; the wait is bounded (see rt-08 below). Such a game should
call `install_vblank_counter()` before it installs its handler. Games that
call `install_vblank_counter()` and then wrap: wipeout-psx, voxide, hk-psx,
hl-psx, cs-psx, quake-psx, oot-psx. Games that call `wait_vblank()` with no
install of their own: nitroxide `game/src/draw.rs` (1, after the engine's
install).

## psx-rt: the exception handler acknowledges stray interrupts (rt-05)

psx-rt's handler owns VBlank and acknowledged only that. Any other source
enabled in `I_MASK` returned without an acknowledge, so the same interrupt
re-entered at once and the console hung. `psx_io::irq::set_mask` is safe, so
`irq::set_mask(1 << VBLANK | 1 << TMR2)` after `install_vblank_counter()` was
enough (a headless run of exactly that stops at the first line and never
reaches its VBlank wait).

The handler now acknowledges an interrupt that has an enabled source
pending and no VBlank pending, records the source bits and counts it
(`interrupts::stray_interrupt_count()`, `last_stray_interrupt_sources()`).
It deliberately does nothing while a VBlank is pending: a game's own handler
chains to psx-rt's for VBlank, and a source that handler services itself
(hk-psx and cs-psx's CD interrupt) must stay in `I_STAT` until it does. A
game that relied on a handler of its own for such a source is unaffected; a
game that polls `I_STAT` for a source it also enabled in `I_MASK` never
worked and now loses the flag to the acknowledge, which the stray count
shows. The fast path (a VBlank) is byte for byte the same code; the new
block is reached only when no VBlank is pending.

## psx-rt: VBlank and present-queue waits give up instead of hanging (rt-08)

`wait_vblank()` and `present::{wait_slot_empty, wait_arena_free, wait_idle}`
looped without a bound that survives a missing handler: they counted VBlanks
that only psx-rt's handler produces. Inside a `critical_section::with`
(interrupts masked) or before `install_vblank_counter()` they never
returned; a headless guest that calls `wait_vblank()` in a critical section
stops there on main.

They now stop after about four million reads of their condition. The signature
is unchanged. `wait_vblank()` returns and asserts in a debug build; the
present waits assert in a debug build and, in `wait_idle`, stop a walk that
never finished by hand, as the VBlank stall path already did. New
`interrupts::try_wait_vblank(spins) -> bool` is the bounded form that
reports it. A wait that completes is unchanged: the added cost is one
counter increment per spin pass.

A call site that really did wait inside a critical section was hung before
and now falls through; none of the games below does (the waits all run in the
main loop with interrupts on): nitroxide, voxide, hk-psx, hl-psx, cs-psx,
quake-psx, oot-psx.

## psx-io: blocking CD commands time out with a typed error (io-02)

`cd::command` and the blocking forms built on it (`status`, `set_mode`,
`unmute`, `mute`, `play_track`, `pause`, `stop`, `play_position`) waited for
the acknowledge in an unbounded loop, and the parameter-FIFO wait ignored its
own timeout and wrote the byte anyway. A drive that never answered hung the
caller; a drive error came back as an ordinary response.

| Old | New |
| --- | --- |
| `cd::command(c, p) -> Response` (and the forms above) | `-> Result<Response, CdError>`, `CdError::{Timeout, DriveError}`, each wait bounded by `cd::DEFAULT_COMMAND_SPINS` |
| (none) | `cd::command_within(c, p, spins) -> Result<Response, CdError>` |
| `cd::try_command(c, p, spins) -> Option<Response>` | unchanged; `None` still covers both errors, `command_within` tells them apart |

A parameter FIFO that never frees a slot now returns `Timeout` without
writing the parameter or the command. A drive error (INT5) leaves the
controller cleaned up (drained, acknowledged, IRQ enable restored); a
timeout leaves CD IRQ output masked, as `try_command` already did. The
`cd::audio` default budget is the same constant.

The deprecated `psx_io::cdrom` forwarders keep their `Response` return and
hand back `Response::empty()` on an error. No game calls the blocking forms
(they use `try_*`); the one caller in the tree is the `hello-cdda` example,
updated to ignore the result as it always did.

## psx-settings: saving never formats a card (settings-01)

`save_slot_one` formatted any card whose frame 0 did not start with `MC`,
which rewrites the directory and drops every other save on it, from a helper
named "save settings". `settings::save` and `save_slot_one` now return
`CardError::Card(psx_mc::Error::NotFormatted)` and leave the card untouched.
Formatting is `format_and_save` / `format_and_save_slot_one`, for after the
player has said yes to "format this card?". A formatted card is saved onto
as before.

Callers of `save_slot_one` that relied on the silent format and so now see
`NotFormatted` on a blank card: nitroxide `game/src/main.rs` (1), voxide
`game/src/main.rs` (1), PSoXide-editor `engine/examples/game-invaders` and
`game-pong` (1 each). Each should show a prompt and call
`format_and_save_slot_one` on yes. `load_slot_one` is unchanged.

## psx-cache: a late load cannot fill another key's slot (cache-01)

`SlotCache::reserve` returned a bare slot index and `mark_ready(slot, value)`
stored into any occupied slot. A load that finished after its key was
evicted, and its slot given to another key, wrote its value under that other
key: `reserve(1)`, `evict(1)`, `reserve(2)` (same slot), `mark_ready(slot,
value_for_1)` left key 2 `Ready` with key 1's value. Asynchronous loads
(a CD read finishing frames later) are exactly when that happens.

| Old | New |
| --- | --- |
| `reserve(key) -> Option<usize>` | `begin_load(key) -> Option<Reservation>` (`Reservation::slot()`, `key()`) |
| `mark_ready(slot, value)` | `finish_load(reservation, value) -> Result<(), Stale>` |

`finish_load` stores only if the reservation's key still owns the slot and
returns `Stale` otherwise. The old pair stays, deprecated and unchanged, for
one stage. Caller: hk-psx `shared/hk-cache/src/lib.rs` (1 `reserve`, 1
`mark_ready`, the latter in the same call so it cannot go stale today; move
it anyway when a load can span frames).

## psx-fx: slow particles drift (fx-01)

`ParticlePool::update` moved a particle by `vx / 16` on integer pixels and
dropped the remainder every frame, although the docs promise Q4.4
sub-pixel velocity. A particle with `|vx| < 16` never moved, and every `vx`
in `16..=31` moved exactly one pixel a frame, so about half of a burst with
`velocity_range = 32` had no horizontal motion.

`Particle` gains `x_frac` and `y_frac` (sixteenths of a pixel) and `update`
carries the remainder, so the distance after `n` frames is
`floor(n * v / 16)`. Particles move differently from the previous stage:
slow ones now drift and fast ones move their true speed, so a burst is wider
and the picture of a frame with live particles changes. Anything that pins
frame hashes of a scene with `psx-fx` particles (hl-psx and cs-psx impact
sparks) needs its pins refreshed on repin.

`Particle` has two new public fields: a struct literal needs
`..Particle::empty()` or the two zeroes. No game outside the SDK builds one
(hk-psx and quake-psx have their own particle types); callers of
`ParticlePool` (cs-psx 1, hl-psx 1 `render_into_ot` plus pool calls) need no
edit.

## psx-fx: `LcgRng::signed` is symmetric and cannot overflow (fx-02)

`signed(range)` computed `(raw - 16) * range / 16` in `i16`. Above
`range = 2047` the product overflowed (a panic in a host debug build, a
flipped sign on the console), and the result spanned `[-range, 15 * range /
16]`, not the documented `[-range, +range]`: at `range = 40` the maximum was
37, so every burst drifted toward negative x and y.

It now maps the five bits onto thirty-two evenly spaced values from
`-range` to `+range` in 32-bit arithmetic. The values differ from the
previous stage for every range, so a seeded burst lands elsewhere and
frame hashes of scenes that draw `signed` output (particle bursts) change.
Callers: cs-psx and hl-psx, through `ParticlePool::spawn_burst` (their
view shake and impact streams use `next()`, which is unchanged); voxide
`game/src/mob.rs` uses `next_mixed`, also unchanged. Refresh pins on repin.
