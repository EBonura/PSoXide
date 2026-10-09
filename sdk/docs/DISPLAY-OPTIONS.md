# Picture options: adopting `psx-display`

`psx-display` is the shared version of WipEout's BRIGHTNESS row (`DEFAULT` in
the middle, `DARKER n` to the left, `BRIGHTER n` to the right) and of its
SCREEN X / SCREEN Y rows (`CENTRE`, `LEFT n`, `RIGHT n`, `UP n`, `DOWN n`).
It holds the value types, the step and clamp rules, the row text, the save
byte and the way each one reaches the screen. It has no heap, no hardware
ownership and no cost per frame at `DEFAULT`.

## What a game gets

| Need | Call |
| --- | --- |
| The setting | `Brightness` (-5..=5, `DEFAULT` is 0), `ScreenOffset<RANGE>` (`RANGE` defaults to 16) |
| Left / right on the pad | `brightness.stepped(-1)`, `offset.stepped_x(1)`; clamped, never wraps |
| The text of the row | `brightness.label()`, `offset.label_x()`, `offset.label_y()`; `as_str()`, `as_bytes()` or `copy_into(&mut [u8; 12])` |
| Row text for a game's own range | `Brightness::label_for_level(level)` (no clamp) |
| Save | `brightness.to_byte()` / `Brightness::from_byte(b)`; `offset.to_bytes()` / `ScreenOffset::from_bytes([x, y])`. Two's complement, so a save from before the option reads as the default. Any byte decodes (clamped) |
| Put brightness on screen | `brightness.overlay(Resolution::R320X240)` gives `None` at `DEFAULT` and a `BrightnessOverlay` otherwise |
| Put the offset on screen | `offset.apply_to(display_config)`, then the GPU's display setter (a video-signal change, nothing per frame) |
| Brightness while a screen fades | `brightness.faded_overlay(resolution, fade)`, `fade` 128 being full strength |

### How brightness is applied

The GPU has no gamma, so the overlay is one semi-transparent grey rectangle
(GP0 62h, behind a GP0(E1h) draw-mode word) over the finished frame: `B - F`
to darken, `B + F` to brighten. The grey is 6 per step darker and 8 per step
brighter (brighter adds more because it lifts the blacks). `BrightnessOverlay`
implements `GpuPacket`, so it goes into an `OtFrame` (`add(0, &mut overlay)`)
or straight to the GPU (`Gpu::draw(&overlay)`); `words()` gives the four
payload words for a game that copies packets into its own arena.

Rules for the overlay:

- Link it into slot 0 **before** the HUD and menus are inserted there. Inserts
  prepend and slot 0 is walked last, so the overlay is the last node drawn and
  covers the HUD and menus too. A fade-to-black quad goes in before it.
- The draw area and offset must cover the whole frame when it runs. A split
  screen or a viewport widens them first (WipEout's `full_screen_env`).
- It leaves GP0(E1h) in its own semi-transparent mode with dithering off. It is
  the last node of a frame, so the next frame's first packets set their own.
- A game that draws its HUD immediate after the table (NitroXide) draws the
  overlay immediate after the HUD instead of linking it.

A game that already has a palette or CLUT route to brightness (Quake, HL) keeps
it. It is real gamma, costs nothing per pixel, and an overlay would add a
packet per frame there. Those games use the crate for the row text, the
centred reading and the save byte only.

## Differences from the WipEout copy

The gain table and the packet words are WipEout's, step for step (the host
tests rebuild its formula and its words and compare all ten non-default
steps). One word differs: GP0(E1h) bit 10 ("drawing to the display area
allowed") is set here and clear in WipEout. The GPU ignores it for a
progressive frame, and a 480-line interlaced frame needs it to draw into the
field being shown. The save bytes are WipEout's, so an existing WipEout save
reads unchanged.

## Finding: the darker steps are coarser than they read

The GPU blends in five bits per channel and a rectangle's grey is its top five
bits. Seen through that, the table is:

| Step | 1 | 2 | 3 | 4 | 5 |
| --- | --- | --- | --- | --- | --- |
| DARKER (grey 6, 12, 18, 24, 30) | 0 | 1 | 2 | 3 | 3 |
| BRIGHTER (grey 8, 16, 24, 32, 40) | 1 | 2 | 3 | 4 | 5 |

So DARKER 1 draws nothing and DARKER 4 and DARKER 5 are the same picture. This
is source-inspected on the emulator's blend and colour conversion
(`gpu/blend.rs`), not tested on a console, and the unit test
`the_gpu_sees_five_bit_channels` pins it. The fix, if Manny wants it, is a
darker grey of 8 per step; it changes how WipEout's existing DARKER steps look,
so it is his call, and it is one constant.

## Per game

Paths are in each game's own repository. WipEout's work is on branch
`menu/options-page-2026-10-08` (not on its main) and Hollow Knight's brightness
is on branch `focus-vfx` (main has none); both would be adopted on top of those.

**WipEout.** `game/src/options.rs` has the three rows (ids 28 to 30) with their
own `toward` and `text` helpers: the `value` closures become
`p.brightness.label().copy_into(buf)`, the `step` closures
`p.brightness = p.brightness.stepped(d)`. `progress.rs` fields `brightness`,
`screen_x`, `screen_y` (`i8`) become `Brightness` and `ScreenOffset`, and
`BRIGHT_STEPS`, `SCREEN_RANGE` go. `saves.rs` (`DISPLAY_AT`) calls
`to_byte`/`from_byte`/`to_bytes`/`from_bytes`; the bytes do not change.
`render.rs` loses `BRIGHT_UP`, `BRIGHT_DOWN` and the body of
`fn brightness::<SPLIT>`: it keeps the split-screen draw-area widening and
copies `overlay.words()` into slot 0. `screen.rs::apply_display` becomes
`ScreenOffset::apply_to` plus the display setter.

**Hollow Knight.** `game/src/display.rs` is replaced whole. It builds a 9-word
Gouraud quad with a constant grey; the shared 4-word rectangle is the same
grey and the same blend. `menu_state.rs` rows 3 to 5 print `+3` / `-2` through
`signed()`: they print the labels instead (`menu.rs` line 100). `render.rs`
(the `display::append` call) links `overlay` after the fade quad and before the
HUD. The title screens' `draw_direct(gain)` becomes
`brightness.faded_overlay(resolution, gain)` and `Gpu::draw`; the fade rule is
HK's, tested. HK keeps brightness out of its save today ("a display setting
belongs to the television"); the shared byte makes saving it a one-line choice,
left to Manny.

**Quake.** The GAMMA row (`crates/quake-core/src/menu.rs`, row 3, labels
`"1"` to `"8"`) becomes BRIGHTNESS and reads
`Brightness::label_for_level(index as i8 - DEFAULT_BRIGHTNESS as i8)`: the
eight palette rows read `DARKER 1`, `DEFAULT`, `BRIGHTER 1` to `BRIGHTER 6`.
`DEFAULT` is the shipped default (row index 1, the tuned lift), not the
cooker's neutral power, so the centre tells players where Quake's intended
look is. Build the eight labels once in a `static [Label; 8]` so `as_str()`
returns the `&'static str` that `MenuRow::valued` takes. The CLUT-row
application (`renderer.rs::set_brightness_level`) and the stepping code stay.

**NitroXide.** `game/src/main.rs`: `SettingsRow` has `Arena`, `Sound`,
`Music`, `Track`, `Back`; add `Brightness` after `Music`, a line in
`settings_rows()` and in `draw_settings` (label and value are drawn
separately already, so the value is `label.as_str()` from a local `Label`).
The pause list also reaches Settings. The byte needs a home: the profile
saved through `psx_settings` has a `brightness` field, but it is 0 to 100 and
`sanitize` clamps it, so a step stored there would be clamped. Add a field to
the settings record in its next version (see the last section). Its overlay HUD is drawn
immediate after the table is submitted (see `draw_now_playing`), so the
brightness overlay is drawn immediate after that, with the full-screen draw
area restored.

**VoXide.** `game/src/main.rs`: `SETTING_NAMES` / `SETTING_ROWS` (5 rows,
about line 4024) feed both the main menu's settings page and the in-game
OPTIONS list (`OPTIONS`, `OPT_SETTINGS`, about line 10923); adding
`"BRIGHTNESS"` once adds it to both. `setting_value` writes into a
`[u8; 4]`; it becomes a `[u8; 12]` so it can carry the label. The overlay is
linked at slot 0 before the HUD; the byte shares NitroXide's record question,
since VoXide persists through the same `psx_settings` profile. VoXide's own
`brightness` comments (day and night lerp) are unrelated.

**Celeste.** One change in `shared/src/pause.rs` serves `celeste`, `celeste2`
and `celeste-collection`. The pause rows are SFX, Music, Fly (debug), Pixel,
Screen, Borders, Quit: add BRIGHTNESS after Borders and before Quit
(`row_count` 6/7 becomes 7/8, a `brightness_row()` beside `borders_row()`, a
branch in the left/right handler next to the volume rows, a line in the draw
pass). The setting lives with the other player options in `shared/src/save.rs`
(one more byte of the payload). The overlay is drawn in `backend.rs` right
before `submit()`, so the pause panel itself is dimmed too, which shows the
effect live. The game's "Screen" row is a pixel-scale mode, not an overscan
position, so it is not the SCREEN X/Y option.

**HL.** `game/src/menu.rs` `OPT_LABELS` already has Screen X, Screen Y and
Brightness (`OPT_BRIGHTNESS = 6`), shared with the in-game pause menu, but
`draw_options_menu` prints the numbers (`i32_dec`). It prints
`label_x()`, `label_y()` and `label_for_level(level - DEFAULT_BRIGHTNESS)`
instead, and `settings.rs` `SCREEN_X/Y` (range 24) become a
`ScreenOffset::<24>`. The brightness application (the per-map light palette
curve, `BRIGHTNESS_MIX`) stays: it costs nothing per frame and HL has no room
for an extra packet or table. `DEFAULT` is the shipped index 1; the cooked
lighting (mix 0, index 4) reads `BRIGHTER 3`. HL is at its `.bss` limit, so
measure the added code. `Label` is 13 bytes; a table of eight is 104.

**Cortex.** The option is already data-driven: `runtime_config.rs`
`BRIGHTNESS_OPTION_ID = 6`, six levels, default level 6, applied by
`overlay.rs::draw_brightness_overlay` (two flat triangles and two draw-mode
sets) from `render_post_process`. Cut over to a `-5..=5` option with
`Brightness::new(value)` and a single `gpu.draw(&overlay)`. On the GPU's five
bits the old six levels (grey -24, -14, -6, 0, +6, +14) are exactly
`DARKER 4`, `DARKER 2`, `DARKER 1`, `DEFAULT`, `DEFAULT`, `BRIGHTER 1`, so
every look it can show today is reachable, and its shipped level 6 is
`BRIGHTER 1` (set the option's default to 1 to keep the current first-boot
picture). What is missing is a UI node that prints text for an option's value:
the front-end Settings scene only has Slider, Bar and Label nodes for
integer options (`LevelUiNodeKind`), so it needs a stepper-label node that
calls `label()`. SCREEN X/Y are options 1 and 2, applied in
`playtest_scene.rs::apply_options` with a `-128..=127` clamp; move to
`ScreenOffset` and decide the range (16 matches the others).

## Should SCREEN X/Y come along?

Yes. Four games already carry their own copy (WipEout and Hollow Knight at
plus or minus 16, HL at 24, Cortex at 127) with slightly different clamps and
wording, and the write path is already in the SDK
(`DisplayConfig::with_offset`). What the games repeat is the value, the labels
and the byte, which `ScreenOffset` now holds. The `RANGE` parameter keeps HL's
24 without a behaviour change. Quake, NitroXide, VoXide and Celeste have no
overscan control, and a CRT is where this matters, so they should get it with
the same adoption step. It is a recommendation; nothing in the crate forces a
game to take both options.

## Open points for Manny

- Whether to fix the coarse darker steps (grey 8 per step). It changes
  WipEout's look.
- `psx-settings::Profile.brightness` (0 to 100, default 75) is read by no
  renderer and is clamped by `sanitize`, so it cannot hold a signed step. The
  clean fix is a record version that replaces it with a `Brightness` byte and
  adds the two offset bytes, with a decode that maps the old field to the
  default. That is a format change for every game that saves a profile
  (NitroXide, VoXide), so it is a separate change.
- Quake and HL read `DEFAULT` at their shipped palette row, not at the neutral
  curve. This keeps `DEFAULT` meaning "the intended look"; say if the neutral
  row should be the centre instead.
