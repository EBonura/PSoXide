# Renames under the PSoXide convention

Every public SDK item that broke [the convention](NAMING.md), its new path,
the reason, and how often each game's `main` uses the old name.

How to read it:

- **Status**: `done` means the new name is the real item on
  `naming/psoxide-convention` and the old name is a deprecated forwarder
  (games keep compiling and get a warning that names the new item).
  `deferred` means no forwarder is possible (a public field) and the rename
  waits for a breaking release.
- **Uses** counts lines in files that use the crate, per game, on each
  repo's `main` as of 2026-10-03 (`git grep`; vendored SDK copies excluded).
  It is approximate: short identifiers can match unrelated code. `new` means
  the item arrived with the DMA rework (SDK 3fd5a0f21) and no game uses it
  yet. Games: editor (PSoXide-editor), emu (PSoXide-emulator), alttp, cs,
  ds, gh, hk, hl, nitro (nitroxide), oot, pico8, arcade (psoxide-arcade),
  demo (psx-demo-disc), psxcel, quake, voxide, wipeout.
- **Reason** codes: `mnemonic` (a hardware mnemonic or register name),
  `register` (a register-level constant, now in psx-hw), `psyq` (a historical
  SDK name), `getter` (`get_` prefix), `predicate` (bool query without
  `is_`/`has_`), `abbrev`, `acronym`, `unit`, `count` (`n_` prefix),
  `ctor`, `iter`, `module` (one module per device), `spelling`, `stutter`,
  `raw suffix`, `clash`.

| Old | New | Reason | Status | Uses |
| --- | --- | --- | --- | --- |
| `psx_io::read8` | `psx_io::read_u8` | unit | done | 39 (editor 21, cs 6, hk 6, nitro 2, oot 4) |
| `psx_io::read16` | `psx_io::read_u16` | unit | done | 95 (editor 95) |
| `psx_io::read32` | `psx_io::read_u32` | unit | done | 36 (editor 36) |
| `psx_io::write8` | `psx_io::write_u8` | unit | done | 57 (editor 26, cs 8, ds 5, hk 9, oot 9) |
| `psx_io::write16` | `psx_io::write_u16` | unit | done | 104 (editor 104) |
| `psx_io::write32` | `psx_io::write_u32` | unit | done | 46 (editor 46) |
| `psx_io::cdrom` | `psx_io::cd` | acronym | done | 43 (editor 21, hk 1, hl 15, nitro 2, arcade 1, demo 1, quake 1, wipeout 1) |
| `psx_io::cdda` | `psx_io::cd::audio` | module | done | 8 (gh 2, nitro 1, arcade 2, demo 1, quake 1, wipeout 1) |
| `psx_io::cdda::CddaClock` | `psx_io::cd::audio::PlaybackClock` | module | done | 9 (gh 5, arcade 2, demo 2) |
| `psx_io::cdda::CddaEndDetector` | `psx_io::cd::audio::EndDetector` | module | done | 15 (hk 2, nitro 3, arcade 2, demo 2, quake 3, wipeout 3) |
| `psx_io::cdda::CddaStarter` | `psx_io::cd::audio::PlaybackStarter` | module | done | 28 (gh 3, hk 2, nitro 5, arcade 6, demo 3, quake 6, wipeout 3) |
| `psx_io::cdda::CddaEndDetector::armed` | `psx_io::cd::audio::EndDetector::is_armed` | predicate | done | 4 (arcade 2, demo 2) |
| `psx_io::cdda::CddaClock::playing` | `psx_io::cd::audio::PlaybackClock::is_playing` | predicate | done | 4 (arcade 2, demo 2) |
| `psx_io::cdda::CddaStarter::started` | `psx_io::cd::audio::PlaybackStarter::has_started` | predicate | done | 13 (gh 3, hk 1, nitro 1, arcade 3, demo 3, quake 1, wipeout 1) |
| `psx_io::cdrom::get_stat` | `psx_io::cd::status` | getter+mnemonic | done | 0 |
| `psx_io::cdrom::try_get_stat` | `psx_io::cd::try_status` | getter+mnemonic | done | 16 (editor 4, hk 7, hl 1, arcade 1, demo 1, quake 1, wipeout 1) |
| `psx_io::cdrom::get_loc_p` | `psx_io::cd::play_position` | getter+mnemonic | done | 0 |
| `psx_io::cdrom::try_get_loc_p` | `psx_io::cd::try_play_position` | getter+mnemonic | done | 8 (editor 3, hk 2, hl 1, arcade 1, demo 1) |
| `psx_io::cdrom::try_set_loc_lba` | `psx_io::cd::try_set_target_lba` | mnemonic | done | 7 (editor 6, hk 1) |
| `psx_io::cdrom::try_read_n` | `psx_io::cd::try_start_reading` | mnemonic | done | 3 (editor 3) |
| `psx_io::cdrom::demute` | `psx_io::cd::unmute` | mnemonic | done | 0 |
| `psx_io::cdrom::try_demute` | `psx_io::cd::try_unmute` | mnemonic | done | 9 (editor 7, hk 1, hl 1) |
| `psx_io::cdrom::BASE` | `psx_hw::cd::BASE` | register | done | 19 (editor 10, cs 1, hk 6, nitro 2) |
| `psx_io::cdrom::CMD_*` | `psx_hw::cd::CMD_*` | register | done ((12 command bytes, names kept)) | 56 (editor 32, cs 5, ds 9, hk 7, nitro 1, wipeout 2) |
| `psx_io::cdrom::MODE_*` | `psx_hw::cd::MODE_*` | register | done ((4 mode bits, names kept)) | 22 (editor 10, cs 1, hk 5, hl 3, quake 2, wipeout 1) |
| `psx_io::cdrom::STAT_*` | `psx_hw::cd::STAT_*` | register | done ((4 status bits, names kept)) | 8 (editor 4, hk 2, oot 2) |
| `psx_io::gpu::write_gp0` | `psx_io::gpu::write_command` | mnemonic | done | 141 (editor 112, hk 12, hl 8, pico8 6, quake 3) |
| `psx_io::gpu::write_gp1` | `psx_io::gpu::write_display_control` | mnemonic | done | 56 (editor 51, cs 1, hk 3, hl 1) |
| `psx_io::gpu::gpustat` | `psx_io::gpu::status` | mnemonic | done | 40 (editor 38, hk 2) |
| `psx_io::gpu::gpuread` | `psx_io::gpu::read_data` | mnemonic | done | 6 (editor 6) |
| `psx_io::gpu::wait_cmd_ready` | `psx_io::gpu::wait_command_ready` | abbrev | done | 44 (editor 31, hk 9, hl 1, pico8 3) |
| `psx_io::gpu::try_wait_cmd_ready` | `psx_io::gpu::try_wait_command_ready` | abbrev | done | 2 (editor 2) |
| `psx_io::irq::stat` | `psx_io::irq::pending` | mnemonic | done | 5 (editor 5) |
| `psx_io::irq::ack` | `psx_io::irq::acknowledge` | abbrev | done | 38 (editor 21, cs 4, ds 2, hk 5, oot 6) |
| `psx_io::irq::I_STAT` | `psx_hw::irq::I_STAT` | register | done | 8 (editor 8) |
| `psx_io::irq::I_MASK` | `psx_hw::irq::I_MASK` | register | done | 4 (editor 3, cs 1) |
| `psx_io::irq::source` | `psx_hw::irq::source` | register | done ((11 bit constants, names kept)) | 0 |
| `psx_io::sio::{DATA,STAT,MODE,CTRL,BAUD}` | `psx_hw::sio::sio0::{DATA,STAT,MODE,CTRL,BAUD}` | register | done | 27 (editor 20, arcade 4, demo 3) |
| `psx_io::spu::SPU_BASE` | `psx_hw::spu::BASE` | register | done | 25 (editor 25) |
| `psx_io::spu::{SPUCNT,SPUSTAT,TRANSFER_ADDR,TRANSFER_DATA,TRANSFER_CTRL}` | `psx_hw::spu::{SPUCNT,SPUSTAT,TRANSFER_ADDR,TRANSFER_DATA,TRANSFER_CTRL}` | register | done | 133 (editor 128, pico8 5) |
| `psx_io::sio` | `(removed after deprecation)` | register | done (only held register addresses) | 0 |
| `psx_io::spu` | `(removed after deprecation)` | register | done (only held register addresses) | 70 (editor 68, pico8 2) |
| `psx_gpu::material::BlendMode::from_tpage_bits` | `psx_gpu::material::BlendMode::from_texture_page_bits` | psyq | done | 5 (emu 5) |
| `psx_gpu::material::BlendMode::tpage_bits` | `psx_gpu::material::BlendMode::texture_page_bits` | psyq | done | 9 (cs 5, hl 4) |
| `psx_gpu::material::TextureMaterial::tpage_word` | `psx_gpu::material::TextureMaterial::texture_page_word` | psyq | done | 5 (editor 3, cs 1, hl 1) |
| `psx_gpu::material::TextureMaterial::raw_texture` | `psx_gpu::material::TextureMaterial::is_raw_texture` | predicate | done | 0 |
| `psx_gpu::material::TextureMaterial::dither` | `psx_gpu::material::TextureMaterial::is_dithered` | predicate | done | 1 (editor 1) |
| `psx_gpu::ordered::OrderedCommandStream::draw_sync` | `psx_gpu::ordered::OrderedCommandStream::flush` | psyq | done | 73 (editor 13, alttp 2, cs 8, hk 7, hl 10, oot 1, pico8 12, arcade 4, demo 3, quake 5, voxide 4, wipeout 4) |
| `psx_gpu::ordered::CommandStreamDma::draw_sync` | `psx_gpu::ordered::CommandStreamDma::wait_idle` | psyq | done | 73 (editor 13, alttp 2, cs 8, hk 7, hl 10, oot 1, pico8 12, arcade 4, demo 3, quake 5, voxide 4, wipeout 4) |
| `psx_gpu::ordered::CommandStreamDma::busy` | `psx_gpu::ordered::CommandStreamDma::is_busy` | predicate | done | 2 (voxide 2) |
| `psx_gte::mfc2` | `psx_gte::read_data` | mnemonic | done | 119 (editor 101, nitro 8, voxide 3, wipeout 7) |
| `psx_gte::mtc2` | `psx_gte::write_data` | mnemonic | done | 165 (editor 131, nitro 23, voxide 3, wipeout 8) |
| `psx_gte::cfc2` | `psx_gte::read_control` | mnemonic | done | 9 (editor 9) |
| `psx_gte::ctc2` | `psx_gte::write_control` | mnemonic | done | 102 (editor 102) |
| `psx_gte::ops::rtps` | `psx_gte::ops::project_single` | mnemonic | done | 8 (editor 5, nitro 1, voxide 1, wipeout 1) |
| `psx_gte::ops::rtpt` | `psx_gte::ops::project_triple` | mnemonic | done | 8 (editor 6, nitro 1, wipeout 1) |
| `psx_gte::ops::nclip` | `psx_gte::ops::screen_winding` | mnemonic | done | 30 (editor 30) |
| `psx_gte::ops::avsz3` | `psx_gte::ops::average_z3` | mnemonic | done | 3 (editor 3) |
| `psx_gte::ops::avsz4` | `psx_gte::ops::average_z4` | mnemonic | done | 2 (editor 2) |
| `psx_gte::ops::op_sf1` | `psx_gte::ops::outer_product` | mnemonic | done | 5 (editor 5) |
| `psx_gte::ops::sqr` | `psx_gte::ops::square` | mnemonic | done | 3 (editor 3) |
| `psx_gte::ops::sqr_sf0` | `psx_gte::ops::square_unshifted` | mnemonic | done | 0 |
| `psx_gte::ops::gpf` | `psx_gte::ops::scale_vector` | mnemonic | done | 2 (editor 2) |
| `psx_gte::ops::gpf_sf0` | `psx_gte::ops::scale_vector_unshifted` | mnemonic | done | 0 |
| `psx_gte::ops::gpl` | `psx_gte::ops::scale_vector_accumulate` | mnemonic | done | 2 (editor 2) |
| `psx_gte::ops::intpl` | `psx_gte::ops::interpolate_far_color` | mnemonic | done | 2 (editor 2) |
| `psx_gte::ops::dpcs` | `psx_gte::ops::depth_cue_single` | mnemonic | done | 2 (editor 2) |
| `psx_gte::ops::dpct` | `psx_gte::ops::depth_cue_triple` | mnemonic | done | 2 (editor 2) |
| `psx_gte::ops::dcpl` | `psx_gte::ops::depth_cue_light` | mnemonic | done | 2 (editor 2) |
| `psx_gte::ops::ncs` | `psx_gte::ops::light_single` | mnemonic | done | 2 (editor 2) |
| `psx_gte::ops::nct` | `psx_gte::ops::light_triple` | mnemonic | done | 2 (editor 2) |
| `psx_gte::ops::nccs` | `psx_gte::ops::light_color_single` | mnemonic | done | 4 (editor 2, nitro 2) |
| `psx_gte::ops::ncct` | `psx_gte::ops::light_color_triple` | mnemonic | done | 3 (editor 2, nitro 1) |
| `psx_gte::ops::ncds` | `psx_gte::ops::light_color_depth_single` | mnemonic | done | 2 (editor 2) |
| `psx_gte::ops::ncdt` | `psx_gte::ops::light_color_depth_triple` | mnemonic | done | 2 (editor 2) |
| `psx_gte::ops::cc` | `psx_gte::ops::color_color` | mnemonic | done | 2 (editor 2) |
| `psx_gte::ops::cdp` | `psx_gte::ops::color_depth_cue` | mnemonic | done | 2 (editor 2) |
| `psx_gte::ops::mvmva_rt_v0_tr_sf1` | `psx_gte::ops::rotate_translate_v0` | mnemonic | done | 9 (editor 9) |
| `psx_gte::ops::mvmva_rt_v0_fc_sf1` | `psx_gte::ops::rotate_v0_far_color` | mnemonic | done | 1 (editor 1) |
| `psx_gte::scene::RtptInFlight` | `psx_gte::scene::ProjectTripleInFlight` | mnemonic | done | 0 |
| `psx_gte::scene::rtpt_kick` | `psx_gte::scene::start_project_triple` | mnemonic | done | 6 (editor 3, voxide 1, wipeout 2) |
| `psx_gte::scene::screen_area_mac0` | `psx_gte::scene::screen_area` | mnemonic | done | 1 (editor 1) |
| `psx_gte::scene::screen_area_mac0_scheduled` | `psx_gte::scene::screen_area_scheduled` | mnemonic | done | 7 (editor 4, cs 1, hl 1, wipeout 1) |
| `psx_gte::scene::read_flag` | `psx_gte::scene::error_flags` | mnemonic | done | 1 (editor 1) |
| `psx_gte::scene::set_avsz_weights` | `psx_gte::scene::set_average_z_weights` | mnemonic | done | 13 (editor 12, quake 1) |
| `psx_gte::scene::average_z3_otz` | `psx_gte::scene::average_z3_ordering_depth` | psyq | done | 0 |
| `psx_gte::scene::average_z4_otz` | `psx_gte::scene::average_z4_ordering_depth` | psyq | done | 0 |
| `psx_gte::scene::classic_otz3_from_sum` | `psx_gte::scene::classic_ordering_depth3_from_sum` | psyq | done | 7 (editor 7) |
| `psx_gte::scene::screen_area_and_classic_otz3_scheduled` | `psx_gte::scene::screen_area_and_classic_ordering_depth3_scheduled` | psyq | done | 1 (editor 1) |
| `psx_gte::scene::load_background_colour` | `psx_gte::scene::load_background_color` | spelling | done | 0 |
| `psx_gte::scene::load_far_colour` | `psx_gte::scene::load_far_color` | spelling | done | 1 (editor 1) |
| `psx_gte::scene::load_light_colour_matrix` | `psx_gte::scene::load_light_color_matrix` | spelling | done | 1 (editor 1) |
| `psx_gte::scene::aabb_outside_clip4` | `psx_gte::scene::is_aabb_outside_clip4` | predicate | done | 7 (editor 1, quake 6) |
| `psx_gte::scene::screen_triangle_back_facing` | `psx_gte::scene::is_screen_triangle_back_facing` | predicate | done | 0 |
| `psx_spu::Voice::key_on` | `psx_spu::Voice::start` | mnemonic | done | 38 (editor 18, alttp 2, hk 12, nitro 1, oot 2, pico8 1, wipeout 2) |
| `psx_spu::Voice::key_off` | `psx_spu::Voice::release` | mnemonic | done | 57 (editor 26, alttp 1, hk 21, nitro 3, oot 3, quake 1, wipeout 2) |
| `psx_spu::Voice::voices_ended` | `psx_spu::Voice::ended_voices` | order | done | 2 (editor 2) |
| `psx_spu::SpuAddr::reg_field` | `psx_spu::SpuAddr::register_value` | abbrev | done | 0 |
| `psx_spu::irq_pending` | `psx_spu::is_irq_pending` | predicate | done | 1 (hk 1) |
| `psx_rt::bios::putchar` | `psx_rt::bios::write_char` | psyq | done | 0 |
| `psx_rt::cache::flush_i_cache` | `psx_rt::cache::flush_instruction_cache` | abbrev | done | 13 (editor 3, cs 1, ds 1, hk 3, oot 1, arcade 1, demo 1, quake 1, wipeout 1) |
| `psx_rt::interrupts::fault_badvaddr` | `psx_rt::interrupts::fault_bad_address` | mnemonic | done | 0 |
| `psx_rt::interrupts::fault_epc` | `psx_rt::interrupts::fault_pc` | mnemonic | done | 0 |
| `psx_rt::interrupts::queue_gp1_at_vblank` | `psx_rt::interrupts::queue_display_control_at_vblank` | mnemonic | done | 10 (editor 1, cs 1, hk 6, hl 1, wipeout 1) |
| `psx_rt::interrupts::take_pending_gp1` | `psx_rt::interrupts::take_queued_display_control` | mnemonic | done | 5 (editor 2, cs 1, hk 1, hl 1) |
| `psx_rt::interrupts::gp1_queue_pending` | `psx_rt::interrupts::is_display_control_queued` | mnemonic+predicate | done | 25 (editor 1, cs 1, hk 19, hl 1, wipeout 3) |
| `psx_rt::interrupts::handler_installed` | `psx_rt::interrupts::is_handler_installed` | predicate | done | 0 |
| `psx_rt::interrupts::stack_safe_handler_installed` | `psx_rt::interrupts::is_stack_safe_handler_installed` | predicate | done | 0 |
| `psx_rt::interrupts::cpu_interrupts_enabled` | `psx_rt::interrupts::are_interrupts_enabled` | predicate | done | 0 |
| `psx_rt::interrupts::CAUSE_BD` | `psx_hw::cop0::CAUSE_BD` | register | done | 0 |
| `psx_rt::scratchpad::on_scratchpad_stack` | `psx_rt::scratchpad::is_on_scratchpad_stack` | predicate | done | 2 (hl 2) |
| `psx_pad::poll_port1_diag` | `psx_pad::poll_port1_diagnostics` | abbrev | done | 8 (editor 5, cs 1, hl 2) |
| `psx_pad::RawPoll::ack_complete` | `psx_pad::RawPoll::is_fully_acknowledged` | predicate+abbrev | done | 0 |
| `psx_pad::ActionInput::held` | `psx_pad::ActionInput::is_held` | predicate | done | 30 (editor 2, cs 12, hl 10, nitro 1, voxide 5) |
| `psx_pad::ActionInput::pressed` | `psx_pad::ActionInput::just_pressed` | consistency | done (matches PadTracker::just_pressed) | 34 (nitro 1, wipeout 33) |
| `psx_pad::ActionInput::released` | `psx_pad::ActionInput::just_released` | consistency | done (matches PadTracker::just_released) | 0 |
| `psx_pad::Deadzone::outside` | `psx_pad::Deadzone::is_outside` | predicate | done | 4 (cs 2, hl 2) |
| `psx_pad::Deadzone::outside_axis` | `psx_pad::Deadzone::is_outside_axis` | predicate | done | 0 |
| `psx_asset::hmd8::Model::load` | `psx_asset::hmd8::Model::from_bytes` | ctor | done | 26 (editor 15, cs 5, hl 5, nitro 1) |
| `psx_asset::hmd8::Model::load_with_vertex_cap` | `psx_asset::hmd8::Model::from_bytes_with_vertex_cap` | ctor | done | 3 (editor 1, cs 1, hl 1) |
| `psx_asset::hmd8::Model::vert` | `psx_asset::hmd8::Model::vertex` | abbrev | done | 36 (editor 2, cs 16, hl 18) |
| `psx_asset::hmd8::Model::vert_unchecked` | `psx_asset::hmd8::Model::vertex_unchecked` | abbrev | done | 0 |
| `psx_asset::hmd8::Model::vert_gte_words` | `psx_asset::hmd8::Model::vertex_gte_words` | abbrev | done | 6 (cs 3, hl 3) |
| `psx_asset::hmd8::Model::vert_gte_words_unchecked` | `psx_asset::hmd8::Model::vertex_gte_words_unchecked` | abbrev | done | 0 |
| `psx_asset::hmd8::Model::tri` | `psx_asset::hmd8::Model::triangle` | abbrev | done | 30 (editor 11, cs 9, hl 10) |
| `psx_asset::hmd8::Model::tri_unchecked` | `psx_asset::hmd8::Model::triangle_unchecked` | abbrev | done | 0 |
| `psx_asset::hmd8::Model::tri_uv_words` | `psx_asset::hmd8::Model::triangle_uv_words` | abbrev | done | 9 (editor 2, cs 2, hl 5) |
| `psx_asset::hmd8::Model::tri_uv_words_unchecked` | `psx_asset::hmd8::Model::triangle_uv_words_unchecked` | abbrev | done | 0 |
| `psx_asset::hmd8::Model::tri_normal` | `psx_asset::hmd8::Model::triangle_normal` | abbrev | done | 0 |
| `psx_asset::hmd8::Model::tri_normal_unchecked` | `psx_asset::hmd8::Model::triangle_normal_unchecked` | abbrev | done | 0 |
| `psx_asset::hmd8::Model::range` | `psx_asset::hmd8::Model::bone_range` | abbrev | done | 25 (editor 23, cs 1, hl 1) |
| `psx_asset::hmd8::Model::hma1` | `psx_asset::hmd8::Model::tracks` | format name | done | 1 (editor 1) |
| `psx_asset::hmd8::Model.n_verts` | `psx_asset::hmd8::Model::vertex_count()` | count | done (field kept, deprecated) | 17 (editor 1, cs 9, hl 7) |
| `psx_asset::hmd8::Model.n_tris` | `psx_asset::hmd8::Model::triangle_count()` | count | done (field kept, deprecated) | 35 (editor 10, cs 13, hl 12) |
| `psx_asset::hmd8::Model.n_frames` | `psx_asset::hmd8::Model::frame_count()` | count | done (field kept, deprecated) | 33 (editor 1, cs 17, hl 15) |
| `psx_asset::hmd8::Model.n_clips` | `psx_asset::hmd8::Model::clip_count()` | count | done (field kept, deprecated) | 31 (editor 2, cs 13, hl 16) |
| `psx_asset::hmd8::Model.n_bones` | `psx_asset::hmd8::Model::bone_count()` | count | done (field kept, deprecated) | 6 (cs 3, hl 3) |
| `psx_asset::hmd8::Model.n_ranges` | `psx_asset::hmd8::Model::bone_range_count()` | count | done (field kept, deprecated) | 5 (editor 1, cs 2, hl 2) |
| `psx_asset::hmd8::Tri` | `psx_asset::hmd8::Triangle` | abbrev | done | 6 (oot 6) |
| `psx_asset::hmd8::DEFAULT_MAX_VERTS` | `psx_asset::hmd8::DEFAULT_MAX_VERTICES` | abbrev | done | 0 |
| `psx_asset::hma1::Aff` | `psx_asset::hma1::Affine` | abbrev | done | 7 (editor 1, cs 3, hl 3) |
| `psx_asset::hma1::Model::n_bones` | `psx_asset::hma1::Model::bone_count` | count | done | 0 |
| `psx_asset::hma1::Model::n_clips` | `psx_asset::hma1::Model::clip_count` | count | done | 0 |
| `psx_asset::hma1::N_RATES` | `psx_asset::hma1::RATE_COUNT` | count | done | 0 |
| `psx_asset::Mesh::vert_count` | `psx_asset::Mesh::vertex_count` | abbrev | done (matches Model::vertex_count) | 6 (editor 3, nitro 3) |
| `psx_asset::Model::double_sided` | `psx_asset::Model::is_double_sided` | predicate | done | 5 (editor 5) |
| `psx_asset::Texture::index_zero_transparent` | `psx_asset::Texture::is_index_zero_transparent` | predicate | done | 27 (editor 26, arcade 1) |
| `psx_asset::World::fog_enabled` | `psx_asset::World::is_fog_enabled` | predicate | done | 1 (editor 1) |
| `psx_asset::World::static_vertex_lighting` | `psx_asset::World::has_static_vertex_lighting` | predicate | done | 1 (editor 1) |
| `psx_asset::WorldSector::floor_walkable` | `psx_asset::WorldSector::is_floor_walkable` | predicate | done | 3 (editor 3) |
| `psx_asset::WorldSector::floor_triangle_present` | `psx_asset::WorldSector::has_floor_triangle` | predicate | done | 5 (editor 5) |
| `psx_asset::WorldSector::floor_triangle_walkable` | `psx_asset::WorldSector::is_floor_triangle_walkable` | predicate | done | 2 (editor 2) |
| `psx_asset::WorldSector::ceiling_triangle_present` | `psx_asset::WorldSector::has_ceiling_triangle` | predicate | done | 4 (editor 4) |
| `psx_asset::WorldSector::ceiling_triangle_walkable` | `psx_asset::WorldSector::is_ceiling_triangle_walkable` | predicate | done | 0 |
| `psx_asset::WorldSectorFloorCollision::walkable` | `psx_asset::WorldSectorFloorCollision::is_walkable` | predicate | done | 1 (editor 1) |
| `psx_asset::WorldWall::solid` | `psx_asset::WorldWall::is_solid` | predicate | done | 2 (editor 2) |
| `psx_fmv::bs` | `psx_fmv::bitstream` | abbrev | done | 0 |
| `psx_fmv::bs::BsError` | `psx_fmv::bitstream::DecodeError` | stutter | done | 0 |
| `psx_fmv::str` | `psx_fmv::stream` | shadows core::str | done | 0 |
| `psx_fmv::mdec::Tables::dma_ready` | `psx_fmv::mdec::Tables::is_dma_ready` | predicate | done | 0 |
| `psx_fmv::mdec::{COMMAND_*,CONTROL_*,DECODE_*,STATUS_*}` | `psx_hw::mdec::{COMMAND_*,CONTROL_*,DECODE_*,STATUS_*}` | register | done ((14 constants, names kept)) | 10 (editor 10) |
| `psx_mc::sio` | `psx_mc::hardware` | mnemonic | done | 0 |
| `psx_mc::MAX_NAME` | `psx_mc::MAX_NAME_LEN` | unit | done | 2 (psxcel 1, voxide 1) |
| `psx_pack::cd::SectorReader::diag` | `psx_pack::cd::SectorReader::diagnostics` | abbrev | done | 3 (editor 2, quake 1) |
| `psx_pack::cd::SectorReader::demute` | `psx_pack::cd::SectorReader::unmute` | mnemonic | done | 0 |
| `psx_font::FontAtlas::tpage` | `psx_font::FontAtlas::texture_page` | psyq | done | 0 |
| `psx_font::TextBlend::abr` | `psx_font::TextBlend::semi_transparency_bits` | mnemonic | done | 0 |
| `psx_font::hex::u16_hex` | `psx_font::hex::format_u16` | order | done | 22 (editor 18, arcade 4) |
| `psx_fx::particles::Particle::alive` | `psx_fx::particles::Particle::is_alive` | predicate | done | 2 (editor 2) |
| `psx_osk::Dir` | `psx_osk::Direction` | abbrev | done | 8 (psxcel 8) |
| `psx_osk::PANEL_H` | `psx_osk::PANEL_HEIGHT` | abbrev | done | 0 |
| `psx_osk::Y0` | `psx_osk::PANEL_TOP` | abbrev | done | 1 (psxcel 1) |
| `psx_asset::hmd8::Model.n_hitboxes` | `psx_asset::hmd8::Model::hitbox_count()` | count | done (field kept, deprecated) | 5 (cs 2, hl 3) |
| `psx_asset::hmd8::Tri.idx` | `psx_asset::hmd8::Triangle.indices` | abbrev | deferred (no alias possible for a field; next breaking release) | 159 (editor 23, alttp 1, cs 65, hl 70) |
| `psx_asset::hmd8::Tri.tex` | `psx_asset::hmd8::Triangle.texture` | abbrev | deferred (no alias possible for a field; next breaking release) | 82 (editor 19, cs 34, hl 29) |
| `psx_gpu::prim::RectFlat.color_cmd` | `psx_gpu::prim::RectFlat.color_command` | abbrev | deferred (repr(C) packet field; next breaking release) | 32 (editor 21, cs 1, hk 2, hl 1, nitro 2, voxide 5) |
| `psx_gte::lighting::Light.colour` | `psx_gte::lighting::Light.color` | spelling | deferred (no alias possible for a field; next breaking release) | 6 (editor 6) |
| `psx_io::dma::madr` | `psx_io::dma::address` | mnemonic | done | 2 (editor 2) |
| `psx_io::dma::chcr` | `psx_io::dma::control` | mnemonic | done | 4 (editor 4) |
| `psx_io::dma::set_madr` | `psx_io::dma::raw::set_address` | mnemonic | done (already deprecated by the DMA rework; note now names the new raw fn) | 22 (editor 21, oot 1) |
| `psx_io::dma::set_chcr` | `psx_io::dma::raw::set_control` | mnemonic | done (already deprecated by the DMA rework) | 26 (editor 25, oot 1) |
| `psx_io::dma::set_bcr_manual` | `psx_io::dma::start + size_words` | mnemonic | done (already deprecated by the DMA rework) | 13 (editor 12, oot 1) |
| `psx_io::dma::set_bcr_block` | `psx_io::dma::start + size_blocks` | mnemonic | done (already deprecated by the DMA rework) | 9 (editor 9) |
| `psx_io::dma::raw::set_madr` | `psx_io::dma::raw::set_address` | mnemonic | done | new |
| `psx_io::dma::raw::set_bcr` | `psx_io::dma::raw::set_size` | mnemonic | done | new |
| `psx_io::dma::raw::set_chcr` | `psx_io::dma::raw::set_control` | mnemonic | done | new |
| `psx_io::dma::bcr_words` | `psx_io::dma::size_words` | mnemonic | done | new |
| `psx_io::dma::bcr_blocks` | `psx_io::dma::size_blocks` | mnemonic | done | new |
| `psx_io::dma::Transfer.madr` | `psx_io::dma::Transfer.address` | mnemonic | done (renamed outright: new with the DMA rework, no game uses it) | new |
| `psx_io::dma::Transfer.bcr` | `psx_io::dma::Transfer.size` | mnemonic | done (renamed outright) | new |
| `psx_io::dma::Transfer.chcr` | `psx_io::dma::Transfer.control` | mnemonic | done (renamed outright) | new |
| `psx_io::dma::Channel::base` | `psx_io::dma::Channel::register_base` | abbrev | done | 26 (editor 7, hk 19) |
| `psx_io::dma::Channel::dpcr_enable_bit` | `psx_io::dma::Channel::enable_bit` | mnemonic | done | 0 |
| `psx_io::dma::Channel::Cdrom` | `psx_io::dma::Channel::Cd` | acronym | done (old name kept as a deprecated associated const) | 23 (editor 18, oot 5) |
| `psx_io::dma::Channel::Pio` | `psx_io::dma::Channel::Expansion` | mnemonic | done | 5 (editor 5) |
| `psx_io::dma::Channel::Otc` | `psx_io::dma::Channel::OrderingTableClear` | mnemonic | done | 26 (editor 26) |
| `psx_io::dma::DEFAULT_DMA_SPINS` | `psx_io::dma::DEFAULT_SPINS` | stutter | done | 6 (editor 6) |
| `psx_io::dma::{DPCR,DICR,CHCR_*}` | `psx_hw::dma::{DPCR,DICR,CHCR_*}` | register | done ((10 constants, names kept)) | 50 (editor 50) |
| `psx_io::periph::Cdrom` | `psx_io::periph::Cd` | acronym | done | new |
| `psx_io::periph::OtcDma` | `psx_io::periph::OrderingTableClearDma` | mnemonic | done | new |
| `psx_io::periph::Sio0` | `psx_io::periph::ControllerPort` | mnemonic | done | new |
| `psx_rt::Peripherals.{otc_dma,cdrom,sio0}` | `psx_rt::Peripherals.{ordering_table_clear_dma,cd,controller_port}` | follows type | done (renamed outright: new with the DMA rework) | new |
| `psx_gpu::draw_sync` | `psx_gpu::wait_idle` | psyq | done | 77 (editor 14, alttp 2, cs 8, hk 8, hl 10, oot 1, pico8 14, arcade 4, demo 3, quake 5, voxide 4, wipeout 4) |
| `psx_gpu::draw_done` | `psx_gpu::is_draw_done` | predicate | done | 1 (quake 1) |
| `psx_gpu::configure_vsync_timer` | `psx_gpu::configure_scanline_timer` | psyq | done (it programs Timer 1 to count HBlanks) | 3 (editor 1, cs 1, hl 1) |
| `psx_gpu::submit_linked_list_raw_async` | `psx_gpu::submit_linked_list_async_raw` | raw suffix | done | new |
| `psx_gpu::ordered::GpuDma` | `psx_gpu::ordered::GpuChannel` | clash | done (clashed with psx_io::periph::GpuDma) | 0 |
| `psx_gpu::ot::OrderingTable::clear_via_otc_dma` | `psx_gpu::ot::OrderingTable::clear_with_dma` | mnemonic | done | 0 |
| `psx_gpu::ot::OrderingTable::iter_packets` | `psx_gpu::ot::OrderingTable::packets` | iter | done | 2 (editor 1, emu 1) |
| `psx_gpu::ot::OtPacketIter` | `psx_gpu::ot::Packets` | iter+abbrev | done | 0 |
| `psx_vram::Tpage` | `psx_vram::TexturePage` | psyq | done | 229 (editor 57, alttp 31, cs 16, ds 2, gh 2, hk 25, hl 15, nitro 8, oot 12, pico8 18, arcade 21, demo 12, psxcel 2, quake 3, voxide 5) |
| `psx_vram::Tpage::uv_tpage_word` | `psx_vram::TexturePage::uv_word` | psyq+stutter | done | 119 (editor 20, alttp 36, cs 9, hk 13, hl 8, nitro 11, oot 6, pico8 3, arcade 6, demo 4, quake 1, voxide 2) |
| `psx_vram::Clut::uv_clut_word` | `psx_vram::Clut::uv_word` | stutter | done | 127 (editor 19, alttp 36, cs 10, hk 18, hl 9, nitro 10, oot 6, pico8 7, arcade 5, demo 3, quake 1, voxide 3) |
| `psx_vram::TexDepth` | `psx_vram::TextureDepth` | abbrev | done (matches psx_gpu::TextureDepth) | 217 (editor 59, alttp 31, cs 15, ds 2, gh 2, hk 24, hl 14, nitro 8, oot 18, pico8 13, arcade 14, demo 7, psxcel 2, quake 3, voxide 5) |
| `psx_vram::Color555::has_stp` | `psx_vram::Color555::is_semi_transparent` | mnemonic | done | 0 |
| `psx_vram::Color555::with_stp` | `psx_vram::Color555::with_semi_transparency` | mnemonic | done | 0 |
| `psx_vram::TextureWindowAtlas::page_is_empty` | `psx_vram::TextureWindowAtlas::is_page_empty` | predicate | done | 0 |
