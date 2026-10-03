# Naming convention

The SDK has three layers, and each takes its names from a different source.
New items follow this file; existing items that don't are listed at the end
so they can be renamed through deprecation rather than in one breaking pass.

## Raw layer: psx-spx register names

Crates and modules that expose hardware registers one to one (`psx-io`,
`psx-hw`, the register constants in the other crates) use the names in
[psx-spx](https://psx-spx.consoledev.net/), converted to Rust case:
`GPUSTAT` becomes `gpustat()`, `D2_MADR` becomes `madr(Channel::Gpu)`,
`I_STAT` becomes `I_STAT` as a constant. Bit fields keep the psx-spx field
name (`CHCR_START`, not `CHCR_GO`).

Anything at this layer that makes hardware touch caller-chosen RAM is an
`unsafe fn` with a `# Safety` section, whatever its name.

## Command and instruction wrappers: psx-spx mnemonics

A function that issues exactly one GPU command, GTE instruction, CD-ROM
command or SPU register write is named after the psx-spx mnemonic in
snake_case: `rtps`, `nclip`, `avsz3`, `mvmva_rt_v0_tr_sf1` for GTE
instructions; `get_stat`, `get_loc_p`, `try_set_loc_lba`, `try_read_n` for
the CD-ROM commands `Getstat`, `GetlocP`, `Setloc`, `ReadN`; `key_on` and
`key_off` for the SPU's `KON`/`KOFF`; `fill_rect` for GP0(02h). The doc
comment cites the command number (`GP0(E6h)`, `GP1(05h)`).

## Safe layer: Rust API guidelines

Everything above the wrappers (`psx-gpu`'s frame and stream types,
`psx-vram`, `psx-asset`, `psx-pad`, `psx-mc` and the rest) follows the
[Rust API guidelines](https://rust-lang.github.io/api-guidelines/naming.html):
snake_case functions that say what they do, `CamelCase` types with acronyms
as words (`Clut`, `VramRect`), no `get_` prefix on getters, `as_`/`to_`/`into_`
by cost, `try_` for the fallible form of an infallible-looking call,
`_unchecked` for the variant that skips a check the safe one makes, `_raw`
for the variant that takes a pointer instead of a reference. American
spelling (`color`).

## PsyQ names are aliases, never identifiers

PsyQ (and PSn00bSDK, which copies it) is what most PS1 programmers know, so
every item that does what a PsyQ call does carries its name as
`#[doc(alias = "...")]`. Searching the rustdoc for `DrawSync` finds
`psx_gpu::draw_sync`. No public identifier is spelled the PsyQ way, and a
PsyQ name never decides a Rust name.

Aliases in place:

| PsyQ | SDK |
| --- | --- |
| `DrawSync` | `psx_gpu::draw_sync`, `OrderedCommandStream::draw_sync` |
| `VSync` | `psx_rt::interrupts::wait_vblank`, `psx_rt::interrupts::vblank_count` (`VSync(-1)`) |
| `ClearOTagR` | `psx_gpu::ot::OrderingTable::clear`, `psx_io::dma::clear_ordering_table` |
| `DrawOTag` | `psx_gpu::ot::OrderingTable::submit` |
| `ResetGraph` | `psx_gpu::init` |
| `LoadImage` | `psx_vram::upload_words` |
| `FlushCache` | `psx_rt::cache::flush_i_cache` |
| `SpuInit` | `psx_spu::init` |
| `PadRead` | `psx_pad::poll_port1` |
| `SetRotMatrix` | `psx_gte::scene::load_rotation` |
| `SetTransMatrix` | `psx_gte::scene::load_translation` |
| `SetLightMatrix` | `psx_gte::scene::load_light_matrix` |
| `SetColorMatrix` | `psx_gte::scene::load_light_colour_matrix` |
| `SetBackColor` | `psx_gte::scene::load_background_colour` |
| `SetFarColor` | `psx_gte::scene::load_far_colour` |
| `SetGeomOffset` | `psx_gte::scene::set_screen_offset` |
| `SetGeomScreen` | `psx_gte::scene::set_projection_plane` |
| `RotTransPers` | `psx_gte::scene::project_vertex` |
| `RotTransPers3` | `psx_gte::scene::project_triangle` |
| `RotTrans` | `psx_gte::scene::transform_vertex` |

## Renames this convention implies

Not done in this pass. Each goes on the deprecation train: add the new name,
mark the old one `#[deprecated(note = "renamed to ...")]` with a doc alias
for the old spelling, and remove it once no game's main uses it.

| Today | Proposed | Why |
| --- | --- | --- |
| `psx_gpu::draw_sync` | `psx_gpu::wait_idle` | PsyQ name; it waits for GPU-DMA and the GPU to go idle. |
| `OrderedCommandStream::draw_sync` | `OrderedCommandStream::flush` | It submits, waits and resets the buffer, which is what `flush` means in Rust. |
| `psx_gpu::vsync` | remove | Already deprecated: it does not sync to the display. |
| `psx_gte::scene::load_background_colour` | `load_background_color` | Spelling: the rest of the SDK says `color`. |
| `psx_gte::scene::load_far_colour` | `load_far_color` | Same. |
| `psx_gte::scene::load_light_colour_matrix` | `load_light_color_matrix` | Same. |
| `psx_gpu::submit_linked_list{,_async}` | `*_raw` (unsafe) plus a safe form | Takes a raw pointer; being reworked on `soundness/dma-gpu`. |
| `psx_io::dma::{set_madr, set_bcr_*, set_chcr}` | `dma::raw::*` (unsafe), `dma::start` | Raw layer rule above; same branch. |

Names checked and left alone because they already follow the convention:
the GTE instruction wrappers in `psx_gte::ops`, the CD-ROM command wrappers
in `psx_io::cdrom` (`get_stat` is the snake_case of `Getstat`, not a getter),
the SPU `key_on`/`key_off`, and every public type (no all-caps acronyms).
