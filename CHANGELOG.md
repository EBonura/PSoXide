# Changelog

## Unreleased

No program depends on a BIOS any more, not a call into one and not the state one leaves behind (Manny's rule:
every program works on every Sony BIOS and on OpenBIOS).

- GP0(1Fh) is no longer sent alone (FPGA PSX cores lose it): `psx_hw::gpu::gp0::DRAW_DONE_SEQUENCE` is one 13-word
  node, GP0(1Fh) and a 16 x 8 fill of a block at the corner of VRAM (`DRAW_DONE_SCRATCH`, 1008,504, never handed out
  by `psx_vram::VramAllocator`; `psx_io::gpu::set_draw_done_scratch` moves it for a game that keeps data there),
  three times. MiSTer's PSX core raises GPUSTAT bit 24 and I_STAT bit 1 only for a request popped on an odd clk2x
  edge; an idle GPU pops on an even one every time, a request queued behind a fill lands 3 + 4 * 8 edges after it
  starts, an odd edge, whatever came before. `psx_gpu::chain::DRAW_DONE_NODE` is that node (its list lives in
  `psx_io::gpu`), and `Gpu::signal_draw_done` / `psx_io::gpu::signal_draw_done` kick it through DMA channel 2 with
  bounded waits (recorded as words while a present-queue recording is open) instead of writing one word through
  the port. psx-rt's VBlank handler also releases a queued display word or a published present-queue chain when
  GPUSTAT bit 24 is down but DMA 2 is idle and bits 26 and 28 were up on the previous edge as well
  (`psx_rt::interrupts::draw_done_fallback_count` counts it; 0 on a PS1): a lost request costs one VBlank instead
  of a timeout. Silicon ignores the repeats once bit 24 is set.
- `psx_rt::tty` is off unless the `host-tty` feature is on (`console-diag` turns it on): `print`, `println` and
  `print_hex_u32` compile to nothing without it, and with it each byte is one store to the Expansion 2 debug
  console (`psx_hw::memory::expansion2::DEBUG_CONSOLE`), where PCSX-Redux, OpenBIOS and our emulator print it.
  `psx_rt::bios` and its `A(3Ch)` trampoline are gone, so the SDK makes no BIOS call at all. A game whose gate
  reads the terminal asks for `host-tty` in that build; the SDK examples do. `boot-trace` only prints with
  `host-tty` as well.
- `psx_rt::entry::set_entry_state` writes one documented state before `main`: cop0 SR (CU2 only, interrupts off),
  `I_MASK` and `I_STAT`, every DMA channel quiesced then `DPCR` (`psx_hw::dma::DPCR_ENTRY`, the controller's own
  priority ladder, nothing enabled) and `DICR`, the root counters' modes, SIO0, the SPU (voices off, reverb and
  modulation off, main volume at its maximum, CD input volume silent and CD audio off, enabled and unmuted) and the
  cache control register. `_start` calls it, and the demo disc's chain loader calls it between programs. A Sony
  console was measured handing over `DPCR` 0x9099, not 0x07654321, and other BIOS versions and OpenBIOS differ again.
- `entry` also writes the memory controller's seven access-timing registers (`psx_hw::memory::timing::ENTRY`: the
  CD-ROM, SPU, expansion, BIOS ROM and common delays, which BIOS versions and OpenBIOS set differently) and zeroes
  every GTE register; the census keeps the timing words the BIOS left.
- `boot-trace` now implies `host-tty`. Feature `entry-census` keeps the registers the program found at start
  (`psx_rt::entry::inherited`) for the hardware tests to report; off by default.
- `SectorReader::prepare` no longer trusts the drive to be idle on its first call: it asks GetStat and stops a
  drive that is reading, seeking or playing with the proven Pause (`pause::settle_drive`). An idle drive gets no
  Pause. `hello-cdpause` takes the drive over from a running read and from a Pause in flight.

Added `psx_rt::scratchpad_window` (feature `scratchpad-window`, off by default, programs that leave it off are
byte-identical): a stack window on the scratchpad for whole call trees with hop-back to the RAM stack at guarded
callees (`window_fn!`), and `window-gate` in psoxide-hazard, run by psoxide-pgo after every link, which proves from
the final image that each guard's room covers its target's static depth, that level-1 roots fit the window, that
nothing bypasses a stub and that the walker finds no indirect call, `$sp` realignment or recursion. `stack-guard`
now treats a `jal` to the function's own range as recursion. `hello-spstack` runs a recursion deeper than the
window under VBlank interrupts. Known limits are listed in the module docs.

Removed the deprecated forwarders that need no device owner, so a repin that
still uses an old name stops compiling at that name. Every removed item has a
replacement of the same shape; `sdk/docs/NAMING-RENAMES.md` maps old to new
and `sdk/docs/DEPRECATED-REMAINING.md` lists what is still deprecated and who
calls it.

- psx-asset: `hmd8::Model::n_verts` and `n_tris` (the fields are private; use
  `vertex_count()` and `triangle_count()`), `Model::vert` and `tri_uv_words`
  (`vertex`, `triangle_uv_words`), `Mesh::vert_count`, and
  `WorldSector::floor_walkable`, `floor_triangle_present`,
  `floor_triangle_walkable`, `ceiling_triangle_present` and
  `WorldSectorFloorCollision::walkable` (the `is_` and `has_` names).
- psx-cache: `SlotCache::reserve` and `mark_ready` (`begin_load`, `finish_load`).
- psx-gpu: `VideoMode` and `Resolution` at the crate root (`psx_gpu::display`),
  `vsync` (`psx_rt::interrupts::wait_vblank`), `submit_linked_list_async_raw`
  (`chain::submit_async_raw`), `BlendMode::from_tpage_bits`
  (`from_texture_page_bits`), `with_material_packet_texcoords` (`with_material`)
  and the unsafe `with_staged_slot_prepacked_unchecked` constructors
  (`with_staged_slot_prepacked_colors`, which is safe).
- psx-gte: `ops::rtps`, `rtpt`, `nccs`, `ncct` (`project_single`,
  `project_triple`, `light_color_single`, `light_color_triple`), the `mfc2!` and
  `mtc2!` macros (`read_data!`, `write_data!`), and `scene::rtpt_kick`,
  `classic_otz3_from_sum`, `aabb_outside_clip4`, `screen_area_mac0_scheduled`
  (`start_project_triple`, `classic_ordering_depth3_from_sum`,
  `is_aabb_outside_clip4`, `screen_area_scheduled`).
- psx-io: the `cdrom` module (`psx_io::cd`, and the register constants in
  `psx_hw::cd`), the `cdda` module (`cd::audio::EndDetector` and
  `PlaybackStarter`), `PlaybackStarter::tick` and `started` (`tick_on`,
  `has_started`), `dma::Channel::Cdrom`, `dma::set_madr`, `set_bcr_manual` and
  `set_chcr` (`dma::start`, or `dma::raw` for probes), `gpu::write_gp0`,
  `write_gp1`, `gpustat` and `wait_cmd_ready` (`write_command`,
  `write_display_control`, `status`, `wait_command_ready`), `irq::ack` and
  `irq::source` (`acknowledge`, `psx_hw::irq::source`), `read8`, `write8`
  (`read_u8`, `write_u8`), `dma::clear_ordering_table`
  (`OrderingTableClearDma::clear_table`) and the free `cd::dispatch_command`,
  `poll_data_sector`, `restore_irq_output`, `try_status`, `try_set_target_lba`,
  `try_start_reading`, `try_mute`, `try_pause_until_complete` and
  `try_play_position` (the same names as methods on `Cd`).
- hello-memcard: `Diagnostic::new` (`new_on`).
- psx-mc: `MAX_NAME` (`MAX_NAME_LEN`).
- psx-osk: `Dir` (`Direction`) and `Y0` (`PANEL_TOP`, and drop the `- 14`).
- psx-rt: `cache::flush_i_cache`, and `interrupts::queue_gp1_at_vblank`,
  `take_pending_gp1`, `gp1_queue_pending` (`flush_instruction_cache`,
  `queue_display_control_at_vblank`, `take_queued_display_control`,
  `is_display_control_queued`).
- psx-spu: `Voice::key_on`, `key_off` and `irq_pending` (`start`, `release`,
  `is_irq_pending`).
- psx-vram: `Tpage`, `TexDepth`, `Clut::uv_clut_word` and
  `TexturePage::uv_tpage_word` (`TexturePage`, `TextureDepth`, `uv_word`).

Games and tools migrated before the removal (names only, EXEs unchanged where
they could be built): psxcel, celeste-collection-psx, psoxide-arcade, voxide,
nitroxide, quake-psx, oot-psx, wipeout-psx, hk-psx (the files no lane had
open) and the editor's toolchain probe. Still on the removed names, and
needing the rename at their next repin: hk-psx (the files other lanes had
open, `game/src/cd_stream.rs`, `render.rs`, `main.rs`, `display.rs` and
others, and `shared/hk-cache` for `begin_load` and `finish_load`; the renames
are ready on the branch `lane/sdk-deprecations-hk-busy`), and gh-psx, ds-psx,
and alttp-psx, whose pins predate the replacements, and psx-demo-disc's launcher (the demo lanes are replacing it).

## Source 2026.09.05

This source snapshot is tagged `source-2026.09.05`. Download versions are
listed separately below; source cleanup does not replace an already published disc.

- Split the SDK from the editor and emulator; existing Git revisions still resolve.
- Downstream builds can lock generated component sources outside their cache.
- Corrected sector-reader API documentation and declared temporary-register ownership in packed rotation decoders.

No new binary download accompanies this source snapshot.
