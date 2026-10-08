# Public demo-disc inventory

Verified on 29 September 2026 against the public **v0.40** image in the local
game library and its release receipt. The BIN SHA-256 is
`d1e16f5deca68351b8361ab61bc7db90298497dc93817e72f8d7fe5be6657940`.
No disc image or game data is included in this website repository.

## Evidence

- `PSoXide Demo Disc v0.40.bin` matches the SHA-256 in
  `PSoXide Demo Disc v0.40.release-receipt.json`.
- Decoding its `PSXDEMO4` table using the demo-disc repository's
  `tools/check_release_chainloads.py:parse_toc` gives the eight programs and
  versions recorded in `demo-disc.json`. Every program and version matches the
  release receipt. The visible menu also contains Credits.
- PSoXide Arcade's embedded image begins at sector 1855. Decoding the table at
  sector 22 relative to that image gives Breakout, Space Invaders and
  Magikaaaaarp Pong, plus Credits. None of these games is hidden.
- Celeste Collection 0.2.5 is a single executable containing both games. Its
  receipt pins source revision `76bb26b8779f2df2c91b9d8dfeec05f91f7e829b` in
  [celeste-collection-psx](https://github.com/EBonura/celeste-collection-psx).
  At that revision, `games/celeste-collection/src/main.rs` dispatches menu
  choices 0 and 1 to `celeste::run()` and `celeste2::run()`.

The release contains eight outer programs, including two collections. Their
five games are shown separately on the website so readers can find them. Do
not count those five as five additional outer-menu entries.

The site lists seven of the eight: one early-alpha program is deliberately
omitted because it is being withdrawn (Manny, 2026-10-08).

## Differences from older descriptions

The [download page](https://bonnie-studios.itch.io/psoxide-demo-disc) serves
v0.40 but its descriptive list still includes Hardware Tests and an earlier
carousel order. The main demo-disc checkout's README and deployment script
also describe an earlier layout. The v0.40 release receipt uses the separate
`release/lineup-v0.40.json` build recipe.

Hardware Tests is absent from this public image and its receipt. Half-Life
and Hollow Knight are absent too. Optional local editions and
standalone programs must not acquire a public demo-disc badge merely because
the build system supports them.

## Updating the inventory

1. Identify the current public release and its exact BIN/CUE and receipt.
2. Verify the image hash against that receipt.
3. Decode the visible outer menu and compare every program/version with the
   receipt. Decode nested collections or inspect their pinned source as needed.
4. Update `demo-disc.json`, this evidence note and the affected descriptions.
5. Confirm every inventory slug and collection-game slug resolves to a project
   card, and that every included program displays a demo-disc badge.

The Projects template derives inclusion badges and the contents list from this
inventory. Screenshot provenance is recorded separately in
`media-provenance.toml`: the Arcade and Hardware Tests images are existing emulator captures. The
Celeste selector and both games were recaptured from the standalone collection
in the game library. None is presented as original-console evidence.
