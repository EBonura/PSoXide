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
