# Deprecated items still in use

Every `#[deprecated]` item that survived the dead-code purge has at least one caller in a game repo, the editor or the emulator at the revisions below, so deleting it today would break that repo's build. The list is for migrating the callers. Delete an item in the same change that removes its last caller.

Callers were found by searching each repo's `main` for the item's name, qualified by its module path, imports, receiver type or distinctive method name, then reading the hits. A caller found only through a re-export (for example `psx_engine::attributed_clip`) is listed as well. Re-run the search before relying on a row: a repo that has moved past the revision below may no longer call the item. A caller that reaches an item through a name the search could not tie to it (a method on a value whose type is only inferred) can be missing.

Searched: wipeout-psx 0435760, nitroxide d56553e, voxide 0dfc487, hk-psx 0f5c8d0, hl-psx fa5eb61, cs-psx 538f50c, quake-psx ab671da, oot-psx a763a6d, psxcel c08096c, PSoXide-editor 1212e00, PSoXide-emulator aa23f32 (all `main` on GitHub, 2026-10-04).

The items deleted when this list was written (zero callers at those revisions): `psx_io::sio`, `psx_io::spu`, `psx_io::gte`, `psx_mc::sio`, `psx_fmv::bs`, `psx_fmv::str`, `psx_spu::tones` and the tone blobs, and about 270 forwarders, constants and aliases.

## psx-asset

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_asset::Mesh::vert_count` | renamed to `vertex_count` | **nitroxide** game/src/draw.rs:3310,3341,3408 |
| `psx_asset::WorldSector::ceiling_triangle_present` | renamed to `has_ceiling_triangle` | **PSoXide-editor** editor/crates/psxed-project/src/playtest/manifest.rs:3273; engine/crates/psx-engine/src/world_render.rs:1425,1652 |
| `psx_asset::WorldSector::floor_triangle_present` | renamed to `has_floor_triangle` | **PSoXide-editor** editor/crates/psxed-project/src/playtest/manifest.rs:3257; engine/crates/psx-engine/src/world_render.rs:1343,1572 |
| `psx_asset::WorldSector::floor_triangle_walkable` | renamed to `is_floor_triangle_walkable` | **PSoXide-editor** editor/crates/psxed-project/src/playtest/manifest.rs:3261 |
| `psx_asset::WorldSector::floor_walkable` | renamed to `is_floor_walkable` | **PSoXide-editor** editor/crates/psxed-project/src/playtest/manifest.rs:3167 |
| `psx_asset::WorldSectorFloorCollision::walkable` | renamed to `is_walkable` | **PSoXide-editor** engine/crates/psx-engine/src/character_motor.rs:2341 |
| `psx_asset::hmd8::Model::n_tris` | use `triangle_count()` | **cs-psx** game/src/main.rs:9543,20438,20466,20822<br>**hl-psx** game/src/main.rs:13639,26394,26422,26698,26830 |
| `psx_asset::hmd8::Model::n_verts` | use `vertex_count()` | **cs-psx** game/src/main.rs:24012,24013 |
| `psx_asset::hmd8::Model::tri_uv_words` | renamed to `triangle_uv_words` | **cs-psx** game/src/main.rs:16670,20130<br>**hl-psx** game/src/main.rs:22688,25882,26699,26700 |
| `psx_asset::hmd8::Model::vert` | renamed to `vertex` | **cs-psx** game/src/main.rs:16529,16544,16585,16594,16596,16608,16817,17244,17254,19965,19966,19967,19968,21123,28046<br>**hl-psx** game/src/main.rs:22547,22562,22603,22612,22614,22626,22889,23191,23201,25718,25719,25720,25721,26580,27353,27361,35109 |

## psx-cache

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_cache::SlotCache::mark_ready` | renamed to `finish_load`, which checks the key still owns the slot | **hk-psx** shared/hk-cache/src/lib.rs:122 |
| `psx_cache::SlotCache::reserve` | renamed to `begin_load` | **hk-psx** shared/hk-cache/src/lib.rs:119 |

## psx-fmv

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_fmv::mdec::load_tables` | use `Mdec::load_tables` with the `MdecDma` token | **PSoXide-editor** engine/examples/hardware-tests/src/fmv_diag.rs:550 |
| `psx_fmv::mdec::reset` | use `Mdec::reset` with the `MdecDma` token | **PSoXide-editor** engine/examples/hardware-tests/src/fmv_diag.rs:547 |

## psx-gpu

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_gpu::Resolution` | moved to `psx_gpu::display::Resolution` | **PSoXide-editor** engine/crates/psx-engine/src/app.rs:40,349,397; engine/crates/psx-engine/src/game_app.rs:3146,3341; engine/examples/editor-playtest/src/main.rs:105; engine/examples/hardware-tests/src/console_tests.rs:21,205,206; engine/examples/hardware-tests/src/display_widths.rs:31,74,75,76,77,79,319; engine/examples/hardware-tests/src/gpu_probes.rs:23,216; engine/examples/hardware-tests/src/main.rs:24,2833,3322; engine/examples/toolchain-probe/src/main.rs:18,38<br>**cs-psx** game/src/main.rs:88,23043; game/src/settings.rs:8,12<br>**hk-psx** game/src/main.rs:9,491<br>**hl-psx** game/src/main.rs:99,29345; game/src/settings.rs:8,12<br>**oot-psx** game/src/main.rs:56,260<br>**quake-psx** game/src/platform.rs:353<br>**voxide** game/src/main.rs:62,2551<br>**wipeout-psx** game/src/main.rs:53,194 |
| `psx_gpu::VideoMode` | moved to `psx_gpu::display::VideoMode` | **PSoXide-editor** engine/crates/psx-engine/src/app.rs:40,347,385,386,396; engine/examples/editor-playtest/src/main.rs:105,1235,1237,1240,1289; engine/examples/hardware-tests/src/console_tests.rs:21,205; engine/examples/hardware-tests/src/display_widths.rs:31,72,319; engine/examples/hardware-tests/src/gpu_probes.rs:23,216; engine/examples/hardware-tests/src/main.rs:24,2833,3321; engine/examples/toolchain-probe/src/main.rs:18,38<br>**cs-psx** game/src/settings.rs:8,12<br>**hk-psx** game/src/main.rs:9,491<br>**hl-psx** game/src/settings.rs:8,12<br>**oot-psx** game/src/main.rs:56,260<br>**quake-psx** game/src/platform.rs:353<br>**voxide** game/src/main.rs:62,2551<br>**wipeout-psx** game/src/main.rs:53,194 |
| `psx_gpu::arm_draw_done` | use `Gpu::arm_draw_done` | **hk-psx** game/src/menu.rs:124; game/src/render.rs:1551,1610; game/src/scene_transition.rs:23,39<br>**wipeout-psx** game/src/render.rs:672 |
| `psx_gpu::draw_line_mono` | use `gpu.draw(&LineMono::new(..))` | **oot-psx** game/src/modeltest.rs:367<br>**psxcel** game/src/main.rs:36,2244 |
| `psx_gpu::draw_quad_flat` | use `gpu.draw(&QuadFlat::new(..))` | **nitroxide** game/src/main.rs:1452<br>**psxcel** game/src/main.rs:36,1725 |
| `psx_gpu::draw_quad_textured` | use `gpu.draw(&QuadTexturedMaterial::with_material(..))` | **nitroxide** game/src/main.rs:1254<br>**quake-psx** game/src/intro.rs:67<br>**voxide** game/src/main.rs:4353 |
| `psx_gpu::draw_quad_textured_gouraud_material` | use `gpu.draw(&QuadTexturedGouraud::with_material(..))` | **oot-psx** game/src/font.rs:200,210; game/src/hud.rs:234,312,342; game/src/skybox.rs:192; game/src/title.rs:793 |
| `psx_gpu::draw_quad_textured_material` | use `gpu.draw(&QuadTexturedMaterial::with_material(..))` | **oot-psx** game/src/hud.rs:273,376 |
| `psx_gpu::draw_rect_flat` | use `gpu.draw(&QuadFlat::rect(origin, size, color))` | **PSoXide-editor** engine/examples/toolchain-probe/src/main.rs:48<br>**hk-psx** game/src/menu.rs:100,101,107,108<br>**psxcel** game/src/main.rs:36,2250<br>**voxide** game/src/main.rs:4066,4067,4068,4069,4070,4071,4078,4079,4094,4097,4106,4116,4117,4118,4119,4195,4198,4207,4217,4218,4219,4220,4428,4430,4432,4486 |
| `psx_gpu::draw_sprite_material` | use `gpu.set_draw_mode(material)` and `gpu.draw(&Sprite::with_material(..))` | **hk-psx** game/src/menu.rs:39,58,176 |
| `psx_gpu::draw_sync` | use `Gpu::wait_idle` | **hk-psx** game/src/disc.rs:1033; game/src/hero_light.rs:87; game/src/main.rs:635; game/src/render.rs:301; game/src/scene_transition.rs:28,30; game/src/vram_cache.rs:60<br>**voxide** game/src/main.rs:3888,4388,4433,10545<br>**wipeout-psx** game/src/menus.rs:129,137; game/src/screen.rs:86,258 |
| `psx_gpu::draw_tri_flat` | use `gpu.draw(&TriFlat::new(..))` | **nitroxide** game/src/main.rs:1440<br>**oot-psx** game/src/main.rs:1436,1437<br>**psxcel** game/src/main.rs:36,1794 |
| `psx_gpu::draw_tri_flat_blended` | use `gpu.set_draw_mode(material)` and `gpu.draw(&TriFlat::new(..).translucent())` | **oot-psx** game/src/intro.rs:1121,1122,1473,1474; game/src/main.rs:1398,1405,1441,1448,1458,1459,1582,1583; game/src/menu.rs:177,178,195,196; game/src/message.rs:130,137 |
| `psx_gpu::draw_tri_gouraud` | use `gpu.draw(&TriGouraud::new(..))` | **nitroxide** game/src/draw.rs:579,580; game/src/main.rs:1393,1394<br>**oot-psx** game/src/intro.rs:1484,1485,1490,1494; game/src/menu.rs:165,166,171,172; game/src/modeltest.rs:412,413 |
| `psx_gpu::fill_rect` | use `gpu.draw(&FillRect::new(origin, size, color))` | **PSoXide-editor** engine/examples/toolchain-probe/src/main.rs:44<br>**hk-psx** game/src/exit_fade.rs:54; game/src/scene_transition.rs:30<br>**nitroxide** game/src/main.rs:1186,1187,1243,1290,1291<br>**quake-psx** game/src/platform.rs:362,363 |
| `psx_gpu::framebuf::FrameBuffer` | use `psx_gpu::display::DoubleBuffer`, whose methods take the `Gpu` | **hk-psx** game/src/audio_probe.rs:60,68,129,183; game/src/main.rs:9,493; game/src/menu.rs:3,121,136,144,174,193,200,205; tests/menu_runtime.rs:79,86,90,95,101,108,111,115<br>**oot-psx** game/src/bg.rs:12,43; game/src/intro.rs:29,451,701,823,1157; game/src/main.rs:56,261,378,1232,1474,1592,1723; game/src/menu.rs:15,67; game/src/modeltest.rs:28,81<br>**quake-psx** game/src/intro.rs:11,28; game/src/platform.rs:3,34,121,358,389<br>**voxide** game/src/main.rs:55,2552,3720,3900,4315,4400,4414,4425,4811,4869,4962,4991,10523 |
| `psx_gpu::init` | use `Gpu::new(dma, DisplayConfig::new(mode, res))` | **PSoXide-editor** engine/examples/toolchain-probe/src/main.rs:38<br>**hk-psx** game/src/main.rs:491<br>**oot-psx** game/src/main.rs:260<br>**quake-psx** game/src/platform.rs:353<br>**voxide** game/src/main.rs:2551<br>**wipeout-psx** game/src/main.rs:194 |
| `psx_gpu::material::BlendMode::from_tpage_bits` | renamed to `from_texture_page_bits` | **PSoXide-emulator** emu/crates/emulator-core/src/gpu.rs:2502,3812; emu/crates/emulator-core/src/gpu/tests.rs:332,333,334,335,337 |
| `psx_gpu::material::TextureMaterial::apply_draw_mode` | use `Gpu::set_draw_mode` | **nitroxide** game/src/draw.rs:517,2114 |
| `psx_gpu::ot::OrderingTable::insert` | use `OtFrame::add_raw`, through `frame()` or `resume_frame()` | **voxide** game/src/main.rs:6071,6460,6529,7054,7192,7321,8160 |
| `psx_gpu::ot::OrderingTable::insert_packed_commands_reverse_unchecked` | use `OtFrame::add_packed_commands_reverse_unchecked`, through `frame()` or `resume_frame()` | **quake-psx** game/src/platform.rs:626,675 |
| `psx_gpu::ot::OrderingTable::insert_tagged_packet_stream_unchecked` | use `OtFrame::add_tagged_packet_stream_unchecked`, through `frame()` or `resume_frame()` | **quake-psx** game/src/platform.rs:550 |
| `psx_gpu::ot::OrderingTable::insert_unchecked` | use `OtFrame::add_raw_unchecked`, through `frame()` or `resume_frame()` | **voxide** game/src/main.rs:8863,8888<br>**wipeout-psx** game/src/order.rs:73; game/src/render.rs:626; tools/order-tests/lib.rs:56 |
| `psx_gpu::ot::OrderingTable::submit` | nothing proves the linked packets are alive; use `OrderingTable::frame` and `OtFrame::submit` | **oot-psx** game/src/room.rs:737 |
| `psx_gpu::ot::OrderingTable::submit_async` | returns mid-walk with the table still mutable; use `OtFrame::submit_with`, `FrameStorage::draw_async` or `FramePair` | **hk-psx** game/src/render.rs:1558 |
| `psx_gpu::ot::TAG_SCOPED_TEXTURE_WINDOW` | has no effect: the scoped texture-window coalescing that read it was removed | **quake-psx** crates/quake-affine/src/classic_affine.rs:14,2283,2346 |
| `psx_gpu::prim::ClassicQuadTexturedGouraud::with_staged_slot_prepacked_unchecked` | renamed to `with_staged_slot_prepacked_colors`, which is safe | **quake-psx** crates/quake-affine/src/classic_affine.rs:1424,1711 |
| `psx_gpu::prim::ClassicTriTexturedGouraud::with_staged_slot_prepacked_unchecked` | renamed to `with_staged_slot_prepacked_colors`, which is safe | **quake-psx** crates/quake-affine/src/classic_affine.rs:1363,1490,1645 |
| `psx_gpu::prim::QuadTexturedGouraud::with_staged_slot_prepacked_unchecked` | renamed to `with_staged_slot_prepacked_colors`, which is safe | **quake-psx** crates/quake-affine/src/classic_affine.rs:2320 |
| `psx_gpu::prim::TriTextured::with_material_packet_texcoords` | identical to `with_material` | **PSoXide-emulator** emu/crates/emulator-core/src/gpu/tests.rs:1260,1261 |
| `psx_gpu::prim::TriTexturedGouraud::with_material_packet_texcoords` | identical to `with_material` | **PSoXide-emulator** emu/crates/emulator-core/src/gpu/tests.rs:1612,1618 |
| `psx_gpu::prim::TriTexturedGouraud::with_staged_slot_prepacked_unchecked` | renamed to `with_staged_slot_prepacked_colors`, which is safe | **quake-psx** crates/quake-affine/src/classic_affine.rs:2260 |
| `psx_gpu::set_draw_area` | use `Gpu::set_draw_area` | **PSoXide-editor** engine/examples/toolchain-probe/src/main.rs:39<br>**hk-psx** game/src/main.rs:499<br>**nitroxide** game/src/draw.rs:308<br>**oot-psx** game/src/main.rs:262<br>**psxcel** game/src/main.rs:2122,2127<br>**quake-psx** game/src/platform.rs:359<br>**voxide** game/src/main.rs:2553 |
| `psx_gpu::set_draw_offset` | use `Gpu::set_draw_offset` | **PSoXide-editor** engine/examples/toolchain-probe/src/main.rs:40<br>**hk-psx** game/src/main.rs:499<br>**oot-psx** game/src/main.rs:299<br>**quake-psx** game/src/platform.rs:360<br>**voxide** game/src/main.rs:2554 |
| `psx_gpu::signal_draw_done` | use `Gpu::signal_draw_done` | **hk-psx** game/src/menu.rs:124; game/src/scene_transition.rs:23,39 |
| `psx_gpu::submit_linked_list_async` | use `OrderingTable::frame`, `Gpu::submit_static`, or the unsafe `chain::submit_async_raw` | **wipeout-psx** game/src/render.rs:674,682 |
| `psx_gpu::submit_linked_list_async_raw` | use `chain::submit_async_raw`, which takes the `GpuDma` token | **quake-psx** game/src/platform.rs:682 |
| `psx_gpu::submit_linked_list_raw` | use `chain::submit_raw`, which takes the `GpuDma` token | **voxide** game/src/main.rs:3870,10486 |
| `psx_gpu::submit_linked_list_raw_async` | use `chain::submit_async_raw`, which takes the `GpuDma` token | **nitroxide** game/src/draw.rs:7647<br>**voxide** game/src/main.rs:10514 |
| `psx_gpu::submit_linked_list_wait` | use `chain::wait`, which takes the `GpuDma` token | **hk-psx** game/src/render.rs:693,910<br>**nitroxide** game/src/draw.rs:7371<br>**quake-psx** game/src/platform.rs:129,612<br>**voxide** game/src/main.rs:10544<br>**wipeout-psx** game/src/screen.rs:85,206,257 |
| `psx_gpu::vsync` | busy-waits a fixed 242 HBlanks from the call site instead of \ syncing to the display; use psx_rt::interrupts::wait_vblank() | **oot-psx** game/src/main.rs:294,1301,1519,1665,1695,1793; game/src/menu.rs:95,106; game/src/modeltest.rs:248 |
| `psx_gpu::wait_idle` | use `Gpu::wait_idle` | **PSoXide-editor** engine/examples/toolchain-probe/src/main.rs:43<br>**quake-psx** game/src/intro.rs:100,108; game/src/platform.rs:130,613,763 |

## psx-gte

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_gte::ops::nccs` | renamed to light_color_single | **nitroxide** game/src/draw.rs:3942,3967 |
| `psx_gte::ops::ncct` | renamed to light_color_triple | **nitroxide** game/src/draw.rs:3930 |
| `psx_gte::ops::rtps` | renamed to project_single | **nitroxide** game/src/draw.rs:3961<br>**voxide** game/src/main.rs:8712<br>**wipeout-psx** game/src/trails.rs:488 |
| `psx_gte::ops::rtpt` | renamed to project_triple | **nitroxide** game/src/draw.rs:3917<br>**wipeout-psx** game/src/trails.rs:363 |
| `psx_gte::regs::mfc2` | renamed to `read_data!` | **PSoXide-editor** engine/examples/hardware-tests/src/main.rs:7188,7189,7190,7191,7192,7193<br>**nitroxide** game/src/draw.rs:3794,3918,3919,3931,3943,3962,3970; game/src/main.rs:12<br>**voxide** game/src/main.rs:12,8731,8732<br>**wipeout-psx** game/src/trails.rs:364,365,366,367,368,369,489 |
| `psx_gte::regs::mtc2` | renamed to `write_data!` | **PSoXide-editor** engine/examples/hardware-tests/src/main.rs:7070,7071,7072,7073,7074,7075,7113,7114,7115,7116,7117,7118,7181,7182,7183,7184,7185,7186<br>**nitroxide** game/src/draw.rs:3794,3909,3910,3911,3912,3913,3914,3921,3922,3923,3924,3925,3926,3927,3938,3939,3940,3958,3959,3963,3964,3965; game/src/main.rs:12<br>**voxide** game/src/main.rs:12,8709,8710<br>**wipeout-psx** game/src/trails.rs:355,356,357,358,359,360,485,486 |
| `psx_gte::scene::aabb_outside_clip4` | renamed to `is_aabb_outside_clip4` | **quake-psx** game/src/renderer.rs:6512 |
| `psx_gte::scene::classic_otz3_from_sum` | renamed to `classic_ordering_depth3_from_sum` | **PSoXide-editor** engine/examples/editor-playtest/src/runtime_config.rs:848,856 |
| `psx_gte::scene::rtpt_kick` | renamed to `start_project_triple` | **voxide** game/src/main.rs:8689<br>**wipeout-psx** game/src/render.rs:887,1974 |
| `psx_gte::scene::screen_area_mac0_scheduled` | renamed to `screen_area_scheduled` | **wipeout-psx** game/src/render.rs:906 |

## psx-io

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_io::cd::acknowledge_irq` | use `Cd::acknowledge_irq` with the `Cd` token | **PSoXide-editor** engine/crates/psx-engine/src/game_app.rs:866; engine/crates/psx-game-runtime/src/cd_stream/hw.rs:472; engine/examples/hardware-tests/src/main.rs:5621,5626,5631,5636,5766,5772,5777<br>**cs-psx** game/src/cd_irq.rs:227,294,298,308,313 |
| `psx_io::cd::audio::PlaybackStarter::started` | renamed to `has_started` | **nitroxide** game/src/music.rs:234<br>**wipeout-psx** game/src/music/cdda.rs:76 |
| `psx_io::cd::audio::PlaybackStarter::tick` | use `tick_on` with the `Cd` token | **nitroxide** game/src/music.rs:222<br>**quake-psx** game/src/music.rs:227<br>**wipeout-psx** game/src/music/cdda.rs:72 |
| `psx_io::cd::discard_response` | use `Cd::discard_response` with the `Cd` token | **PSoXide-editor** engine/crates/psx-engine/src/game_app.rs:865; engine/examples/hardware-tests/src/main.rs:5620,5625,5630,5635,5771,5776<br>**cs-psx** game/src/cd_irq.rs:122,226,293,297,307,312 |
| `psx_io::cd::dispatch_command` | use `Cd::dispatch_command` with the `Cd` token | **PSoXide-editor** engine/crates/psx-engine/src/game_app.rs:836; engine/examples/hardware-tests/src/main.rs:5609 |
| `psx_io::cd::irq_flag_value` | use `Cd::irq_flag_value` with the `Cd` token | **PSoXide-editor** engine/crates/psx-engine/src/game_app.rs:859; engine/crates/psx-game-runtime/src/cd_stream/hw.rs:476; engine/examples/hardware-tests/src/main.rs:5617,5764<br>**cs-psx** game/src/cd_irq.rs:290 |
| `psx_io::cd::poll_data_sector` | use `Cd::poll_data_sector` with the `Cd` token | **PSoXide-editor** engine/crates/psx-goldsrc/src/chunk_stream.rs:79; engine/examples/hardware-tests/src/lever_probes.rs:1197 |
| `psx_io::cd::reader::SectorReader::new` | use `SectorReader::with_cd` with the `Cd` token | **PSoXide-editor** engine/crates/psx-chainloader/src/runtime.rs:141; engine/crates/psx-goldsrc/tests/oracles/legacy-cdstream.rs:40; engine/examples/hardware-tests/src/cd_chain_probe.rs:590; engine/examples/hardware-tests/src/lever_probes.rs:1148; engine/examples/hardware-tests/src/xa_loop.rs:40<br>**cs-psx** game/src/cdstream.rs:26<br>**hk-psx** game/src/disc.rs:369<br>**hl-psx** game/src/cdstream.rs:26; game/src/music.rs:73<br>**nitroxide** game/src/assets.rs:23<br>**quake-psx** game/src/platform.rs:41<br>**voxide** game/src/sfx.rs:92<br>**wipeout-psx** game/src/loader.rs:11 |
| `psx_io::cd::restore_irq_output` | use `Cd::restore_irq_output` with the `Cd` token | **PSoXide-editor** engine/crates/psx-engine/src/game_app.rs:879,896; engine/examples/hardware-tests/src/main.rs:5640 |
| `psx_io::cd::try_command` | use `Cd::try_command` with the `Cd` token | **PSoXide-editor** engine/crates/psx-engine/src/game_app.rs:793<br>**quake-psx** game/src/music.rs:249 |
| `psx_io::cd::try_mute` | use `Cd::try_mute` with the `Cd` token | **PSoXide-editor** engine/examples/hardware-tests/src/audio_probe.rs:183; engine/examples/hardware-tests/src/main.rs:5700,5734,5754 |
| `psx_io::cd::try_pause` | use `Cd::try_pause` with the `Cd` token | **quake-psx** game/src/music.rs:154 |
| `psx_io::cd::try_pause_until_complete` | use `Cd::try_pause_until_complete` with the `Cd` token | **PSoXide-editor** engine/crates/psx-engine/src/game_app.rs:905; engine/examples/hardware-tests/src/audio_probe.rs:117,192<br>**quake-psx** game/src/music.rs:184 |
| `psx_io::cd::try_play_position` | use `Cd::try_play_position` with the `Cd` token | **PSoXide-editor** engine/examples/hardware-tests/src/main.rs:4633,5723,5750 |
| `psx_io::cd::try_play_track` | use `Cd::try_play_track` with the `Cd` token | **PSoXide-editor** engine/examples/game-magikaaaaaarp-pong/src/main.rs:1078; engine/examples/hardware-tests/src/main.rs:5667,5716,5743 |
| `psx_io::cd::try_set_mode` | use `Cd::try_set_mode` with the `Cd` token | **PSoXide-editor** engine/examples/game-magikaaaaaarp-pong/src/main.rs:1074; engine/examples/hardware-tests/src/audio_probe.rs:176; engine/examples/hardware-tests/src/main.rs:4626,5663,5678,5712,5742,5825 |
| `psx_io::cd::try_set_target_lba` | use `Cd::try_set_target_lba` with the `Cd` token | **PSoXide-editor** engine/examples/hardware-tests/src/audio_probe.rs:177; engine/examples/hardware-tests/src/main.rs:5679,5788,5799,5811,5827 |
| `psx_io::cd::try_start_reading` | use `Cd::try_start_reading` with the `Cd` token | **PSoXide-editor** engine/examples/hardware-tests/src/audio_probe.rs:178; engine/examples/hardware-tests/src/main.rs:5680,5828 |
| `psx_io::cd::try_status` | use `Cd::try_status` with the `Cd` token | **PSoXide-editor** engine/examples/hardware-tests/src/cd_chain_probe.rs:664; engine/examples/hardware-tests/src/main.rs:4619,10177,11189<br>**quake-psx** game/src/music.rs:234 |
| `psx_io::cd::try_unmute` | use `Cd::try_unmute` with the `Cd` token | **PSoXide-editor** engine/examples/game-magikaaaaaarp-pong/src/main.rs:1076; engine/examples/hardware-tests/src/audio_probe.rs:118,169,174,186,193; engine/examples/hardware-tests/src/main.rs:5670 |
| `psx_io::cdda::CddaEndDetector` | renamed to `psx_io::cd::audio::EndDetector` | **nitroxide** game/src/music.rs:34,104,125<br>**wipeout-psx** game/src/music/cdda.rs:10,15,24 |
| `psx_io::cdda::CddaStarter` | renamed to `psx_io::cd::audio::PlaybackStarter` | **nitroxide** game/src/music.rs:34,103,124,185,192<br>**wipeout-psx** game/src/music/cdda.rs:10,14,23 |
| `psx_io::cdrom::BASE` | moved to `psx_hw::cd::BASE` | **hk-psx** game/src/cd_stream.rs:10<br>**nitroxide** game/src/music.rs:79,80 |
| `psx_io::cdrom::CMD_GETSTAT` | moved to `psx_hw::cd::CMD_GETSTAT` | **nitroxide** game/src/music.rs:239 |
| `psx_io::cdrom::CMD_PAUSE` | moved to `psx_hw::cd::CMD_PAUSE` | **hk-psx** game/src/cd_stream.rs:112 |
| `psx_io::cdrom::CMD_READN` | moved to `psx_hw::cd::CMD_READN` | **hk-psx** game/src/cd_stream.rs:217 |
| `psx_io::cdrom::CMD_SEEKL` | moved to `psx_hw::cd::CMD_SEEKL` | **hk-psx** game/src/cd_stream.rs:214 |
| `psx_io::cdrom::CMD_SETLOC` | moved to `psx_hw::cd::CMD_SETLOC` | **hk-psx** game/src/cd_stream.rs:131,225 |
| `psx_io::cdrom::CMD_SETMODE` | moved to `psx_hw::cd::CMD_SETMODE` | **hk-psx** game/src/cd_stream.rs:216 |
| `psx_io::cdrom::MODE_AUTO_PAUSE` | moved to `psx_hw::cd::MODE_AUTO_PAUSE` | **wipeout-psx** game/src/music/cdda.rs:83 |
| `psx_io::cdrom::MODE_CDDA` | moved to `psx_hw::cd::MODE_CDDA` | **wipeout-psx** game/src/music/cdda.rs:83 |
| `psx_io::cdrom::MODE_DOUBLE_SPEED` | moved to `psx_hw::cd::MODE_DOUBLE_SPEED` | **hk-psx** game/src/cd_stream.rs:216 |
| `psx_io::cdrom::PlayPosition` | moved to `psx_io::cd::PlayPosition` | **hk-psx** game/src/audio_probe.rs:108; game/src/xa_player.rs:110 |
| `psx_io::cdrom::SectorPollError` | moved to `psx_io::cd::SectorPollError` | **PSoXide-editor** engine/crates/psx-goldsrc/tests/support/chunk_stream_harness.rs:108,131,161 |
| `psx_io::cdrom::acknowledge_irq` | moved to `psx_io::cd::acknowledge_irq` | **hk-psx** game/src/cd_stream.rs:201,202,209,212,240<br>**nitroxide** game/src/music.rs:276 |
| `psx_io::cdrom::bcd_to_bin` | moved to `psx_io::cd::bcd_to_bin` | **nitroxide** game/src/music.rs:90 |
| `psx_io::cdrom::bin_to_bcd` | moved to `psx_io::cd::bin_to_bcd` | **hk-psx** game/src/cd_stream.rs:123 |
| `psx_io::cdrom::discard_response` | moved to `psx_io::cd::discard_response` | **hk-psx** game/src/cd_stream.rs:99,201,202,209,212,240<br>**nitroxide** game/src/music.rs:275 |
| `psx_io::cdrom::dispatch_command` | moved to `psx_io::cd::dispatch_command` | **nitroxide** game/src/music.rs:239<br>**wipeout-psx** game/src/music/cdda.rs:109 |
| `psx_io::cdrom::irq_flag_value` | moved to `psx_io::cd::irq_flag_value` | **hk-psx** game/src/cd_stream.rs:198<br>**nitroxide** game/src/music.rs:259<br>**wipeout-psx** game/src/music/cdda.rs:92 |
| `psx_io::cdrom::poll_data_sector` | moved to `psx_io::cd::poll_data_sector` | **PSoXide-editor** engine/crates/psx-goldsrc/tests/oracles/legacy-cdstream.rs:453 |
| `psx_io::cdrom::restore_irq_output` | moved to `psx_io::cd::restore_irq_output` | **nitroxide** game/src/music.rs:284,293<br>**wipeout-psx** game/src/music/cdda.rs:46,103 |
| `psx_io::cdrom::set_audio_mixer` | moved to `psx_io::cd::set_audio_mixer` | **hk-psx** game/src/xa_player.rs:59 |
| `psx_io::cdrom::try_command` | moved to `psx_io::cd::try_command` | **hk-psx** game/src/audio_probe.rs:115,117; game/src/xa_player.rs:55,69<br>**nitroxide** game/src/music.rs:302 |
| `psx_io::cdrom::try_demute` | renamed to `psx_io::cd::try_unmute` | **hk-psx** game/src/audio_probe.rs:113; game/src/xa_player.rs:53 |
| `psx_io::cdrom::try_get_loc_p` | renamed to `psx_io::cd::try_play_position` | **hk-psx** game/src/audio_probe.rs:108; game/src/xa_player.rs:110 |
| `psx_io::cdrom::try_get_stat` | renamed to `psx_io::cd::try_status` | **wipeout-psx** game/src/music/cdda.rs:84 |
| `psx_io::cdrom::try_pause` | moved to `psx_io::cd::try_pause` | **nitroxide** game/src/music.rs:184 |
| `psx_io::cdrom::try_pause_until_complete` | moved to `psx_io::cd::try_pause_until_complete` | **hk-psx** game/src/audio_probe.rs:142,177; game/src/music.rs:100,408; game/src/xa_player.rs:76,84<br>**wipeout-psx** game/src/music/cdda.rs:66 |
| `psx_io::cdrom::try_set_loc_lba` | renamed to `psx_io::cd::try_set_target_lba` | **hk-psx** game/src/audio_probe.rs:116; game/src/xa_player.rs:69 |
| `psx_io::cdrom::try_set_mode` | moved to `psx_io::cd::try_set_mode` | **hk-psx** game/src/audio_probe.rs:114; game/src/xa_player.rs:54<br>**wipeout-psx** game/src/music/cdda.rs:83 |
| `psx_io::dma::Channel::Cdrom` | renamed to `Channel::Cd` | **oot-psx** game/src/loader.rs:135,161,162,163,165 |
| `psx_io::dma::clear_ordering_table` | use `OrderingTableClearDma::clear_table` with the token | **PSoXide-editor** engine/examples/hardware-tests/src/main.rs:9931 |
| `psx_io::dma::set_bcr_manual` | use the unsafe `dma::start` with `dma::size_words`, or `dma::raw::set_size` for probes | **oot-psx** game/src/loader.rs:162 |
| `psx_io::dma::set_chcr` | renamed to `set_control` | **oot-psx** game/src/loader.rs:163 |
| `psx_io::dma::set_chcr` | a safe control store starts DMA from safe code; use the unsafe `dma::start`, `dma::abort` to stop a channel, or `dma::raw::set_control` for probes | **oot-psx** game/src/loader.rs:163 |
| `psx_io::dma::set_madr` | renamed to `set_address` | **oot-psx** game/src/loader.rs:161 |
| `psx_io::dma::set_madr` | a safe address store lets safe code aim DMA anywhere in RAM; use the unsafe `dma::start`, or `dma::raw::set_address` for probes | **oot-psx** game/src/loader.rs:161 |
| `psx_io::gpu::gpustat` | renamed to `status` | **hk-psx** game/src/animation_cache.rs:143; game/src/exit_fade.rs:32 |
| `psx_io::gpu::wait_cmd_ready` | renamed to `wait_command_ready` | **hk-psx** game/src/animation_cache.rs:111; game/src/exit_fade.rs:12,53,66,67,68,75,76; game/src/vram_cache.rs:60 |
| `psx_io::gpu::write_gp0` | renamed to `write_command` | **hk-psx** game/src/animation_cache.rs:112; game/src/exit_fade.rs:12,53,66,67,68,75,78,79,80,81; game/src/vram_cache.rs:60 |
| `psx_io::gpu::write_gp1` | renamed to `write_display_control` | **hk-psx** game/src/exit_fade.rs:48,57; game/src/presentation.rs:84 |
| `psx_io::irq::ack` | renamed to `acknowledge` | **hk-psx** game/src/audio_stream.rs:259; game/src/cd_stream.rs:89,98,241,259<br>**oot-psx** game/src/loader.rs:168 |
| `psx_io::irq::source::CDROM` | moved to `psx_hw::irq::source::CDROM` | **cs-psx** game/src/cd_irq.rs:19<br>**hk-psx** game/src/cd_stream.rs:11 |
| `psx_io::irq::source::DMA` | moved to `psx_hw::irq::source::DMA` | **oot-psx** game/src/loader.rs:168 |
| `psx_io::irq::source::SPU` | moved to `psx_hw::irq::source::SPU` | **hk-psx** game/src/audio_stream.rs:234 |
| `psx_io::read8` | renamed to `read_u8` | **hk-psx** game/src/cd_stream.rs:104,167,173,174,207,208<br>**nitroxide** game/src/music.rs:266,267<br>**oot-psx** game/src/loader.rs:20,200,214,215 |
| `psx_io::write8` | renamed to `write_u8` | **hk-psx** game/src/cd_stream.rs:71,73,96,100,107,110,130,164,224<br>**oot-psx** game/src/loader.rs:20,137,138,148,158,188,190,221,230 |

## psx-math

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_math::attributed_clip::clip_convex_plane` | use the safe `clip_to_plane` (checked) or `clip_to_plane_unchecked` (proven capacity) | **voxide** game/src/main.rs:66,7783 |
| `psx_math::attributed_clip::clip_convex_plane_uninit` | use the safe `clip_to_plane_uninit` (checked) or `clip_to_plane_uninit_unchecked` (proven capacity) | **quake-psx** game/src/renderer.rs:15,6859 |

## psx-mc

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_mc::MAX_NAME` | renamed to `MAX_NAME_LEN` | **psxcel** game/src/main.rs:112<br>**voxide** game/src/save.rs:600 |
| `psx_mc::hardware::HardwareCard::new` | use `HardwareCard::on_port` with the `ControllerPort` token | **PSoXide-editor** engine/examples/editor-playtest/src/playtest_runtime.rs:10,32<br>**hk-psx** game/src/save.rs:334,403,440<br>**hl-psx** game/src/save.rs:361<br>**psxcel** game/src/main.rs:167,1248,1274,1313,1327<br>**voxide** game/src/save.rs:205,448,532,565<br>**wipeout-psx** game/src/saves.rs:56 |

## psx-osk

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_osk::Dir` | renamed to `Direction` | **psxcel** game/src/main.rs:40,445,1370,1371,1372,1373,2206,2209 |
| `psx_osk::Y0` | this is the top of the key rows, 14 below `PANEL_TOP`; use `PANEL_TOP` | **psxcel** game/src/main.rs:68 |

## psx-pad

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_pad::PadReader::poll` | use `poll_on` with the `ControllerPort` token | **wipeout-psx** game/src/screen.rs:287,293 |
| `psx_pad::enable_analog_port1` | use `enable_analog_on` with the `ControllerPort` token | **PSoXide-editor** engine/crates/psx-engine/src/app.rs:46,697; engine/examples/hardware-tests/src/controller_test.rs:115,133; engine/examples/hardware-tests/src/main.rs:9875<br>**cs-psx** game/src/main.rs:102,3893,3903<br>**hl-psx** game/src/main.rs:116,4175,4185<br>**nitroxide** game/src/main.rs:1638,1837<br>**oot-psx** game/src/main.rs:59,388,638<br>**quake-psx** game/src/input.rs:3,87; game/src/quake.rs:56<br>**voxide** game/src/main.rs:69,2570,2709 |
| `psx_pad::enable_analog_port2` | use `enable_analog_on` with the `ControllerPort` token | **PSoXide-editor** engine/examples/hardware-tests/src/controller_test.rs:116,136<br>**cs-psx** game/src/main.rs:23625,24705<br>**nitroxide** game/src/main.rs:1639,1839 |
| `psx_pad::poll_port1` | use `poll_on` with the `ControllerPort` token | **PSoXide-editor** engine/examples/hardware-tests/src/console_tests.rs:114,120; engine/examples/hardware-tests/src/controller_test.rs:117,134; engine/examples/hardware-tests/src/fmv_diag.rs:1471; engine/examples/hardware-tests/src/fmv_test.rs:59,62,65; engine/examples/hardware-tests/src/lever_probes.rs:1205; engine/examples/hardware-tests/src/main.rs:5255,5413,9872,10202,10204,10206<br>**cs-psx** game/src/main.rs:102,2259,2263,23102,23162,28303; game/src/menu.rs:24,431,901<br>**hl-psx** game/src/main.rs:114,2622,2626,4176,29442,29506,30056,35255; game/src/menu.rs:24,533,1011,1364<br>**oot-psx** game/src/intro.rs:32,477,669,736; game/src/main.rs:59,816,1256,1492,1611,1749; game/src/menu.rs:18,74,103; game/src/modeltest.rs:32,104<br>**quake-psx** game/src/input.rs:3,58,88; game/src/intro.rs:12,48,51<br>**voxide** game/src/main.rs:73,75,2615,2695,3732,3740,3817,4334,4337; game/src/tunelab.rs:74 |
| `psx_pad::poll_port1_diagnostics` | use `poll_diagnostics_on` with the `ControllerPort` token | **PSoXide-editor** engine/examples/hardware-tests/src/main.rs:3100,6161,9839,9856,9876 |
| `psx_pad::poll_port2` | use `poll_on` with the `ControllerPort` token | **PSoXide-editor** engine/crates/psx-engine/src/scene.rs:20,267; engine/examples/hardware-tests/src/controller_test.rs:117,126,137 |
| `psx_pad::require_analog_port1` | use `require_analog_on` with the `ControllerPort` token | **PSoXide-editor** engine/crates/psx-engine/src/app.rs:46,538<br>**cs-psx** game/src/main.rs:23624<br>**hk-psx** game/src/main.rs:505<br>**hl-psx** game/src/main.rs:114,29351,30059<br>**wipeout-psx** game/src/main.rs:210 |

## psx-rt

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_rt::cache::flush_i_cache` | renamed to `flush_instruction_cache` | **hk-psx** game/src/cd_stream.rs:257; game/src/modules.rs:304,353<br>**oot-psx** game/src/vbl.rs:62<br>**wipeout-psx** game/src/overlay.rs:108,210 |
| `psx_rt::interrupts::gp1_queue_pending` | renamed to `is_display_control_queued` | **hk-psx** game/src/animation_cache.rs:42,142; game/src/main.rs:756,819,903; game/src/menu.rs:127; game/src/presentation.rs:63,76; game/src/render.rs:242,682,689,695,1347; game/src/scene_transition.rs:10,25,41; game/src/vram_cache.rs:36<br>**wipeout-psx** game/src/screen.rs:84,139,188,250,256 |
| `psx_rt::interrupts::queue_gp1_at_vblank` | renamed to `queue_display_control_at_vblank` | **hk-psx** game/src/menu.rs:125; game/src/presentation.rs:57; game/src/scene_transition.rs:24,40<br>**wipeout-psx** game/src/screen.rs:44,146,214,236 |
| `psx_rt::interrupts::take_pending_gp1` | renamed to `take_queued_display_control` | **hk-psx** game/src/presentation.rs:83 |

## psx-settings

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_settings::load_slot_one` | use `load_from_slot_one` with the `ControllerPort` token | **PSoXide-editor** engine/examples/game-breakout/src/main.rs:356; engine/examples/game-invaders/src/main.rs:529; engine/examples/game-magikaaaaaarp-pong/src/main.rs:380; engine/examples/game-pong/src/main.rs:276<br>**nitroxide** game/src/main.rs:1640<br>**voxide** game/src/main.rs:3982 |
| `psx_settings::save_slot_one` | use `save_to_slot_one` with the `ControllerPort` token | **PSoXide-editor** engine/examples/game-breakout/src/main.rs:318; engine/examples/game-invaders/src/main.rs:487; engine/examples/game-magikaaaaaarp-pong/src/main.rs:366; engine/examples/game-pong/src/main.rs:215<br>**nitroxide** game/src/main.rs:767<br>**voxide** game/src/main.rs:4006 |

## psx-sfx

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_sfx::Bank::upload` | use `Bank::upload_on` with the `Spu` driver | **nitroxide** game/src/audio.rs:130 |

## psx-spu

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_spu::Voice::key_off` | renamed to `release` | **PSoXide-editor** engine/crates/psx-goldsrc/tests/oracles/legacy-hsfx-runtime.rs:34,45,61,408,426,474; tools/psoxide-dev/src/main.rs:1688<br>**hk-psx** game/src/ambience.rs:199,302,422; game/src/audio.rs:115,190,226,283,295; game/src/audio_stream.rs:320; game/src/focus_audio.rs:72,75,81,88; game/src/geo_audio.rs:64; game/src/runner_audio.rs:65,79,90,100; game/src/scene_sfx.rs:56,108,118<br>**nitroxide** game/src/audio.rs:199,264,282<br>**oot-psx** game/src/music.rs:259,291,342<br>**wipeout-psx** game/src/audio.rs:559,588 |
| `psx_spu::Voice::key_on` | renamed to `start` | **PSoXide-editor** engine/crates/psx-goldsrc/tests/oracles/legacy-hsfx-runtime.rs:460<br>**hk-psx** game/src/ambience.rs:365; game/src/audio.rs:119,191,229; game/src/audio_stream.rs:305; game/src/focus_audio.rs:75,81; game/src/geo_audio.rs:69; game/src/runner_audio.rs:72,94; game/src/scene_sfx.rs:113,123<br>**nitroxide** game/src/audio.rs:261<br>**oot-psx** game/src/audio.rs:205; game/src/music.rs:355<br>**wipeout-psx** game/src/audio.rs:433,466,596 |
| `psx_spu::init` | use `Spu::new` with the `SpuDma` token | **PSoXide-editor** engine/crates/psx-engine/src/game_app.rs:688; engine/crates/psx-goldsrc/src/hsfx.rs:436; engine/crates/psx-goldsrc/tests/oracles/legacy-hsfx-runtime.rs:95; engine/examples/game-breakout/src/main.rs:351; engine/examples/game-invaders/src/main.rs:521; engine/examples/game-magikaaaaaarp-pong/src/main.rs:376; engine/examples/game-pong/src/main.rs:271; engine/examples/hardware-tests/src/audio_probe.rs:108; engine/examples/hardware-tests/src/handoff_probe.rs:187,268,327,345; engine/examples/hardware-tests/src/reverb_probe.rs:241,320,346,420; engine/examples/hardware-tests/src/ring_probe.rs:207; engine/examples/hardware-tests/src/sample_probe.rs:176; engine/examples/hardware-tests/src/spu_probe.rs:203,416; engine/examples/hardware-tests/src/transition_probe.rs:124,251,260; engine/examples/hardware-tests/src/voice_probe.rs:139,215; engine/examples/hardware-tests/src/xa_loop.rs:294<br>**hk-psx** game/src/audio.rs:60<br>**nitroxide** game/src/audio.rs:93<br>**oot-psx** game/src/audio.rs:139<br>**quake-psx** game/src/quake.rs:59<br>**voxide** game/src/sfx.rs:77<br>**wipeout-psx** game/src/audio.rs:158 |
| `psx_spu::irq_pending` | renamed to `is_irq_pending` | **hk-psx** game/src/audio_stream.rs:340 |
| `psx_spu::upload_adpcm` | use `Spu::upload_adpcm` with the `SpuDma` token | **PSoXide-editor** engine/crates/psx-engine/src/game_app.rs:1379; engine/crates/psx-goldsrc/src/hsfx.rs:457,465,506,523; engine/crates/psx-goldsrc/tests/oracles/legacy-hsfx-runtime.rs:116,124,161,173; engine/examples/hardware-tests/src/audio_link.rs:146,153; engine/examples/hardware-tests/src/audio_probe.rs:112; engine/examples/hardware-tests/src/handoff_probe.rs:278,386; engine/examples/hardware-tests/src/main.rs:8276,9261,9464; engine/examples/hardware-tests/src/reverb_probe.rs:331,461; engine/examples/hardware-tests/src/ring_probe.rs:539,540; engine/examples/hardware-tests/src/sample_probe.rs:276; engine/examples/hardware-tests/src/spu_probe.rs:346,487,695,700,718,720,721,726,749; engine/examples/hardware-tests/src/transition_probe.rs:198,313; engine/examples/hardware-tests/src/voice_probe.rs:190,266<br>**hk-psx** game/src/ambience.rs:103,258; game/src/audio.rs:64,78,97; game/src/audio_stream.rs:275; game/src/focus_audio.rs:42; game/src/geo_audio.rs:43; game/src/runner_audio.rs:40; game/src/scene_sfx.rs:64<br>**oot-psx** game/src/audio.rs:161; game/src/music.rs:222<br>**quake-psx** game/src/audio.rs:772,792<br>**voxide** game/src/sfx.rs:117,499,648<br>**wipeout-psx** game/src/audio.rs:159,655 |

## psx-vram

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_vram::Clut::uv_clut_word` | renamed to `uv_word` | **nitroxide** game/src/draw.rs:1601,1647,1651,1655,1665,1668,1676,8086,8091 |
| `psx_vram::TexDepth` | renamed to `TextureDepth` | **hk-psx** game/src/blocker_roller.rs:154,171; game/src/dialogue.rs:3,113; game/src/game_map.rs:28,150; game/src/hero_light.rs:21,110; game/src/hud.rs:4,34,66; game/src/menu.rs:6,38,60,184; game/src/render.rs:6,331,881,905; game/src/shaman.rs:178,225; game/src/title_card.rs:13,97<br>**nitroxide** game/src/draw.rs:40,1572,1573,1574; game/src/main.rs:28,43,46,52<br>**oot-psx** game/src/font.rs:53,64; game/src/hud.rs:18,134; game/src/skybox.rs:27,69; game/src/title.rs:52,662,664; game/src/vram.rs:30,124,125,127,131,369,379,424,439<br>**psxcel** game/src/main.rs:42,47<br>**voxide** game/src/main.rs:78,1005,4307; game/src/tex.rs:10,3157 |
| `psx_vram::TexturePage::uv_tpage_word` | renamed to `uv_word` | **nitroxide** game/src/draw.rs:1601,1640,1647,1651,1655,1665,1668,1676,8086,8091 |
| `psx_vram::Tpage` | renamed to `TexturePage` | **hk-psx** game/src/blocker_roller.rs:154,171; game/src/dialogue.rs:3,113; game/src/game_map.rs:28,150; game/src/hero_light.rs:21,110; game/src/hud.rs:4,34,66; game/src/menu.rs:6,38,60,184; game/src/render.rs:6,331,739,881,905; game/src/shaman.rs:178,225; game/src/title_card.rs:13,97<br>**nitroxide** game/src/draw.rs:40,1572,1573,1574; game/src/main.rs:28,43,46,52<br>**oot-psx** game/src/font.rs:53,64; game/src/hud.rs:18,134; game/src/skybox.rs:27,69; game/src/title.rs:52,666; game/src/vram.rs:30,372,431<br>**psxcel** game/src/main.rs:42,47<br>**voxide** game/src/main.rs:78,1005,4307; game/src/tex.rs:10,3157 |
