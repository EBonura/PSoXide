# Changelog

## Unreleased

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
  `irq::source` (`acknowledge`, `psx_hw::irq::source`), and `read8`, `write8`
  (`read_u8`, `write_u8`).
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
needing the rename at their next repin: hk-psx (`game/src/cd_stream.rs`,
`xa_player.rs`, `music.rs`, `audio_probe.rs`, `exit_fade.rs`, `render.rs`,
`main.rs`, `display.rs` and the other files other lanes had open;
`shared/hk-cache` for `begin_load` and `finish_load`), and gh-psx, ds-psx,
alttp-psx and psx-demo-disc, whose pins predate the replacements.

## Source 2026.09.05

This source snapshot is tagged `source-2026.09.05`. Download versions are
listed separately below; source cleanup does not replace an already published disc.

- Split the SDK from the editor and emulator; existing Git revisions still resolve.
- Downstream builds can lock generated component sources outside their cache.
- Corrected sector-reader API documentation and declared temporary-register ownership in packed rotation decoders.

No new binary download accompanies this source snapshot.
