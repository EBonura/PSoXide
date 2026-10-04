# The PSoXide naming convention

Every public name in the SDK follows one convention, built on the
[Rust API Guidelines](https://rust-lang.github.io/api-guidelines/naming.html)
and on what `core` and `std` already do. A PS1 programmer who knows Rust should
be able to guess a name; one who knows PsyQ or psx-spx should be able to search
for it.

[NAMING-RENAMES.md](NAMING-RENAMES.md) lists every item that predates this
convention, its new name and who applies the rename.

## Names say what the call does

A name describes the effect in plain words. It is never the name a historical
SDK gave the call, and never a hardware mnemonic or register name.

| Not this | This | Because |
| --- | --- | --- |
| `draw_sync()` | `wait_idle()` | PsyQ's `DrawSync` says nothing about waiting |
| `ops::rtps()` | `ops::project_single()` | `RTPS` is the GTE opcode mnemonic |
| `gpu::gpustat()` | `gpu::status()` | `GPUSTAT` is the register name |
| `cd::try_read_n()` | `cd::try_start_reading()` | `ReadN` is the drive command mnemonic |
| `Voice::key_on()` | `Voice::start()` | `KON` is the SPU register |

The old spelling is not lost: it becomes a search alias (see the last section).

## Casing (C-CASE)

`UpperCamelCase` for types and traits, `snake_case` for functions, methods,
modules, fields and macros, `SCREAMING_SNAKE_CASE` for constants and statics,
as rustc's own lints enforce.

## Acronyms are words

An acronym in a `CamelCase` name is capitalised like a word: `Gpu`, `Dma`,
`Cd`, `Spu`, `Mdec`, `Gte`, `Vram`, `Clut`, `Adsr` (std precedent:
`Utf8Error`, `TcpStream`, `IpAddr`). In `snake_case` it is lower case: `cd`,
`gpu_status`. `GPUStat`, `CDROMReader` and `SPUVoice` are all wrong; `Cd` is the
drive's name, so the module is `cd`, not `cdrom`.

## Conversions: `as_`, `to_`, `into_` (C-CONV)

- `as_*`: free, borrowed view or reinterpretation (`Color555::as_u16`,
  `str::as_bytes`).
- `to_*`: costs some work, leaves `self` usable (`RawPoll::to_state`,
  `f32::to_bits`).
- `into_*`: consumes `self` (`Card::into_inner`, `Vec::into_boxed_slice`).

## Getters have no `get_` (C-GETTER)

A getter is named after what it returns: `cd::status()`, not `get_stat()`;
`Model::vertex_count()`, not `get_vertex_count()`. The `get` prefix is reserved
for lookups that may fail, the way `slice::get` and `SlotCache::get` work.

## Constructors: `new`, `with_*`, `from_*` (C-CTOR)

- `new` builds the common case (`Voice::new`, `Vec::new`).
- `with_*` is a builder step or a constructor with one extra knob
  (`TextureMaterial::with_tint`, `Vec::with_capacity`).
- `from_*` converts from another representation (`Model::from_bytes`,
  `Duration::from_secs`). A constructor that computes from a quantity in
  another domain may say so with `for_*` (`Pitch::for_frequency`).

A parser that reads a blob is `from_bytes`, not `load` or `parse_blob`.

## Fallible variants: `try_*`

When an operation has a form that can fail or time out, the fallible one is
`try_*` and returns `Option` or `Result` (`cd::try_status`,
`gpu::try_wait_command_ready`), the way `TryFrom` and `RefCell::try_borrow`
work. The infallible name either panics, retries or waits without a bound,
and says so in its docs.

## `*_on`: the form that takes the device owner

A function that used to reach a device with no argument and now takes the
owner of that device (the token, or the driver that holds it) keeps its name
with `_on` appended: `poll_on(&mut port, socket)`, `tick_on(&mut cd, now)`,
`upload_on(&mut spu, sample)`. The old spelling is the deprecated forwarder
that steals the token for the call. A constructor does the same with
`on_port` (`HardwareCard::on_port(port, slot)`), and a type that can hold a
token instead of borrowing one takes `with_cd` (`SectorReader::with_cd(cd)`).
A method that already took `self` simply gains the owner as its receiver
(`cd.play_track(2)`), so it needs no suffix.

## `*_unchecked`: unsafe, skips a check the safe form makes

`Model::vertex_unchecked` is `vertex` without the bounds check, and is
`unsafe fn` with a `# Safety` section naming the check the caller now owns
(`slice::get_unchecked`, `str::from_utf8_unchecked`).

## `*_raw`: unsafe, low-level entry point

A function that hands hardware a raw pointer or skips the safe layer's
ownership rules is `unsafe fn` and ends in `_raw` (`submit_linked_list_raw`,
`fill_render_faces_split_raw`; std's `Box::from_raw`). The safe form has the
plain name.

## Iterators: `iter`, `iter_mut`, `into_iter` (C-ITER)

A collection's main iterator is `iter()`, `iter_mut()` or `into_iter()`. An
iterator over a secondary view is named after what it yields (`packets()`,
like `str::chars`), and its type is named the same in `CamelCase`
(`Packets`, like `Chars`).

## Predicates: `is_*`, `has_*`

A method that returns `bool` and changes nothing starts with `is_`, `has_`,
`are_` or `can_`: `is_busy`, `is_alive`, `has_floor_triangle`,
`are_interrupts_enabled`. Event queries that read a transition keep their
verb: `just_pressed`, `just_released`, `contains_ready`.

## One module per device

Each device has one module named after it, in lower case: `cd`, `gpu`, `gte`,
`spu`, `mdec`, `dma`, `irq`, `timers`. Everything that talks to that device
lives inside it, with submodules for sub-features (`cd::audio` for CD-DA
playback). No module is named after a port or a protocol mnemonic (`sio`
becomes `hardware` in psx-mc) or after a format abbreviation that shadows a
`core` module (`psx_fmv::str` becomes `stream`).

## Units in names where it matters

When a number's unit or fixed-point format is not obvious from the type, the
name carries it as a suffix:

- fixed point: `_q12`, `_q8`, `_q16` (`phase_step_q12`, `lerp_q12_i32`)
- sizes: `_bytes`, `_words`, `_halfwords`, `_texels` (`SECTOR_BYTES`,
  `NODE_PAYLOAD_WORDS`)
- time and rate: `_hz`, `_millis`, `_ticks`, `_vblanks`, `_spins`
  (`sample_rate_hz`, `relative_millis`)
- integer width when the call is about the width: `read_u32`, `write_u16`
  (byteorder's `read_u32`)

Counts end in `_count` (`vertex_count`, `clip_count`), never start with `n_`.
Lengths of collections are `len()`, as in std.

## Abbreviations

Spell words out unless the short form is the one std or the whole field uses
(`len`, `ptr`, `addr`, `rgb`, `uv`, `min`, `max`). Write `index`, not `idx`;
`diagnostics`, not `diag`; `command`, not `cmd`. One concept
has one spelling across the SDK: `vertex` and `triangle` everywhere (not
`vert`/`tri` in one crate and `vertex` in the next), `color` (American, as in
the rest of the SDK and in std).

## Register-level names live only in psx-hw

`psx-hw` is the hardware model shared by the SDK and the emulator: register
addresses, bit fields and command bytes. Only there may a name be a register
or field name (`I_STAT`, `CMD_GETSTAT`, `sio0::ctrl::TXEN`), because there the
register is the thing being named. Each one carries its psx-spx name as a
`#[doc(alias)]` where the spelling differs.

Everywhere else, code uses psx-hw's constants directly and wraps them in calls
named after what they do. No SDK crate outside psx-hw exports a register
address or register bit constant.

## PsyQ and psx-spx names are aliases

Every item that does what a PsyQ call, a psx-spx command or a GTE/SPU mnemonic
does carries that name as `#[doc(alias = "...")]`, so searching rustdoc for
`DrawSync`, `RTPS`, `Getstat` or `KON` lands on it. A historical name is never
an identifier.

## Renaming without breaking games

A rename never breaks a game that has not repinned:

1. The new name is the real item, with the doc aliases.
2. The old name stays as a thin `#[deprecated(note = "renamed to ...")]`
   wrapper (`#[inline(always)]` function, type alias or constant), so a repin
   compiles with warnings that point at the new name.
3. Once no game's main uses the old name, it is removed. [DEPRECATED-REMAINING.md](DEPRECATED-REMAINING.md) lists every item still deprecated and who calls it.

The forwarder takes the form the item allows:

- function or method: an `#[inline(always)]` wrapper (unsafe ones keep their
  `# Safety` section);
- type: a `pub type` alias; a unit struct also gets a `const` of the old
  name so `with_dma(words, GpuDma)` still compiles;
- enum variant: an associated `const` of the old name, which works in
  expressions and patterns;
- trait method: the new method is required and the old one becomes a
  provided method that calls it;
- module: a module of the old name whose every item is a deprecated
  forwarder (`#[deprecated]` on a module itself warns nobody);
- macro: a `#[macro_export]` macro of the old name that expands to the new
  one.

Public fields cannot be aliased; a field that breaks the convention gains an
accessor method under the new name and the field itself is marked
`#[deprecated]`. Fields of a type no game uses yet are renamed outright.
