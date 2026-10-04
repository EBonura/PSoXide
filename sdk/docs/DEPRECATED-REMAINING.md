# Deprecated items still in use

Every `#[deprecated]` item that survived the dead-code purge has at least one caller in a game repo, the editor or the emulator at the revisions below, so deleting it today would break that repo's build. The list is for migrating the callers. Delete an item in the same change that removes its last caller.

Callers were found by searching each repo's `main` for the item's name, qualified by its module path, imports, receiver type or distinctive method name, then reading the hits. A caller found only through a re-export (for example `psx_engine::attributed_clip`) is listed as well. Re-run the search before relying on a row: a repo that has moved past the revision below may no longer call the item. A caller that reaches an item through a name the search could not tie to it (a method on a value whose type is only inferred) can be missing.

Searched: wipeout-psx 0435760, nitroxide fe4228f, voxide 6b05698, hk-psx 4ae9338, hl-psx 33aaa50, cs-psx 25cb954, quake-psx d13d6e6, oot-psx a763a6d, psxcel c08096c, PSoXide-editor 1212e00, PSoXide-emulator aa23f32 (all `main`, 2026-10-04).

The items deleted when this list was written (zero callers at those revisions): `psx_io::sio`, `psx_io::spu`, `psx_io::gte`, `psx_mc::sio`, `psx_fmv::bs`, `psx_fmv::str`, `psx_spu::tones` and the tone blobs, and about 250 forwarders, constants and aliases.

## psx-asset

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_asset::Mesh::vert_count` | renamed to `vertex_count` | **nitroxide** game/src/draw.rs:3303,3334,3401 |
| `psx_asset::WorldSector::ceiling_triangle_present` | renamed to `has_ceiling_triangle` | **PSoXide-editor** editor/crates/psxed-project/src/playtest/manifest.rs:3273; engine/crates/psx-engine/src/world_render.rs:1425,1652 |
| `psx_asset::WorldSector::floor_triangle_present` | renamed to `has_floor_triangle` | **PSoXide-editor** editor/crates/psxed-project/src/playtest/manifest.rs:3257; engine/crates/psx-engine/src/world_render.rs:1343,1572 |
| `psx_asset::WorldSector::floor_triangle_walkable` | renamed to `is_floor_triangle_walkable` | **PSoXide-editor** editor/crates/psxed-project/src/playtest/manifest.rs:3261 |
| `psx_asset::WorldSector::floor_walkable` | renamed to `is_floor_walkable` | **PSoXide-editor** editor/crates/psxed-project/src/playtest/manifest.rs:3167 |
| `psx_asset::WorldSectorFloorCollision::walkable` | renamed to `is_walkable` | **PSoXide-editor** engine/crates/psx-engine/src/character_motor.rs:2341 |
| `psx_asset::hma1::Aff` | renamed to `Affine` | **cs-psx** game/src/main.rs:2467,2468,2471<br>**hl-psx** game/src/main.rs:2835,2836,2839 |
| `psx_asset::hmd8::Model::compact_visible_body_frames_raw` | use `Model::compact_visible_bodies`, which takes the buffer by `&mut` | **cs-psx** game/src/main.rs:4663<br>**hl-psx** game/src/main.rs:4909 |
| `psx_asset::hmd8::Model::load_with_vertex_cap` | renamed to `from_bytes_with_vertex_cap` | **cs-psx** game/src/model.rs:7<br>**hl-psx** game/src/model.rs:7 |
| `psx_asset::hmd8::Model::n_clips` | use `clip_count()` | **cs-psx** game/src/main.rs:9902,9908<br>**hl-psx** game/src/main.rs:14104,14130,14136,14203,14216 |
| `psx_asset::hmd8::Model::n_frames` | use `frame_count()` | **cs-psx** game/src/main.rs:27726<br>**hl-psx** game/src/main.rs:34772 |
| `psx_asset::hmd8::Model::n_hitboxes` | use `hitbox_count()` | **cs-psx** game/src/main.rs:14550,14568<br>**hl-psx** game/src/main.rs:20062,20082 |
| `psx_asset::hmd8::Model::n_ranges` | use `bone_range_count()` | **cs-psx** game/src/main.rs:21694<br>**hl-psx** game/src/main.rs:27706 |
| `psx_asset::hmd8::Model::n_tris` | use `triangle_count()` | **cs-psx** game/src/main.rs:4182,9542,20408,20436,20792<br>**hl-psx** game/src/main.rs:4429,13657,26378,26406,26682,26814 |
| `psx_asset::hmd8::Model::n_verts` | use `vertex_count()` | **cs-psx** game/src/main.rs:4181,23937,23938<br>**hl-psx** game/src/main.rs:4428 |
| `psx_asset::hmd8::Model::range` | renamed to `bone_range` | **cs-psx** game/src/main.rs:21695<br>**hl-psx** game/src/main.rs:27707 |
| `psx_asset::hmd8::Model::tri` | renamed to `triangle` | **hl-psx** game/src/main.rs:29087 |
| `psx_asset::hmd8::Model::tri_uv_words` | renamed to `triangle_uv_words` | **cs-psx** game/src/main.rs:16657,20100<br>**hl-psx** game/src/main.rs:22694,25866,26683,26684 |
| `psx_asset::hmd8::Model::vert` | renamed to `vertex` | **cs-psx** game/src/main.rs:16516,16531,16572,16581,16583,16595,16800,17227,17237,19935,19936,19937,19938,21093,21730,27961<br>**hl-psx** game/src/main.rs:22553,22568,22609,22618,22620,22632,22891,23193,23203,25702,25703,25704,25705,26564,27333,27341,27742,35013 |
| `psx_asset::hmd8::Model::vert_gte_words` | renamed to `vertex_gte_words` | **cs-psx** game/src/main.rs:21716,21717,21718<br>**hl-psx** game/src/main.rs:27728,27729,27730 |

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

## psx-fx

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_fx::particles::ParticlePool::render_into_ot` | links rects through the deprecated `OrderingTable::add`; use `render_into_frame` | **cs-psx** game/src/main.rs:28028<br>**hl-psx** game/src/main.rs:35063 |

## psx-gpu

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_gpu::Resolution` | moved to `psx_gpu::display::Resolution` | **PSoXide-editor** engine/crates/psx-engine/src/app.rs:40,349,397; engine/crates/psx-engine/src/game_app.rs:3146,3341; engine/examples/editor-playtest/src/main.rs:105; engine/examples/hardware-tests/src/console_tests.rs:21,205,206; engine/examples/hardware-tests/src/display_widths.rs:31,74,75,76,77,79,319; engine/examples/hardware-tests/src/gpu_probes.rs:23,216; engine/examples/hardware-tests/src/main.rs:24,2833,3322; engine/examples/toolchain-probe/src/main.rs:18,38<br>**cs-psx** game/src/main.rs:94,22968; game/src/settings.rs:8,159<br>**hk-psx** game/src/main.rs:9,491<br>**hl-psx** game/src/main.rs:104,29278; game/src/settings.rs:8,144<br>**oot-psx** game/src/main.rs:56,260<br>**quake-psx** game/src/platform.rs:555<br>**voxide** game/src/main.rs:62,2551<br>**wipeout-psx** game/src/main.rs:53,194 |
| `psx_gpu::VideoMode` | moved to `psx_gpu::display::VideoMode` | **PSoXide-editor** engine/crates/psx-engine/src/app.rs:40,347,385,386,396; engine/examples/editor-playtest/src/main.rs:105,1235,1237,1240,1289; engine/examples/hardware-tests/src/console_tests.rs:21,205; engine/examples/hardware-tests/src/display_widths.rs:31,72,319; engine/examples/hardware-tests/src/gpu_probes.rs:23,216; engine/examples/hardware-tests/src/main.rs:24,2833,3321; engine/examples/toolchain-probe/src/main.rs:18,38<br>**cs-psx** game/src/main.rs:94,22968; game/src/settings.rs:8,159<br>**hk-psx** game/src/main.rs:9,491<br>**hl-psx** game/src/main.rs:104,29278; game/src/settings.rs:8,144<br>**oot-psx** game/src/main.rs:56,260<br>**quake-psx** game/src/platform.rs:555<br>**voxide** game/src/main.rs:62,2551<br>**wipeout-psx** game/src/main.rs:53,194 |
| `psx_gpu::arm_draw_done` | use `Gpu::arm_draw_done` | **cs-psx** game/src/main.rs:27926<br>**hk-psx** game/src/menu.rs:124; game/src/render.rs:1551,1610; game/src/scene_transition.rs:23,39<br>**hl-psx** game/src/main.rs:34985<br>**wipeout-psx** game/src/render.rs:672 |
| `psx_gpu::configure_vsync_timer` | Timer 1 belongs to `psx_io::timers`; set its mode there | **cs-psx** game/src/main.rs:24185<br>**hl-psx** game/src/main.rs:30966 |
| `psx_gpu::draw_done` | renamed to `is_draw_done` | **quake-psx** game/src/platform.rs:403 |
| `psx_gpu::draw_line_mono` | use `gpu.draw(&LineMono::new(..))` | **oot-psx** game/src/modeltest.rs:367<br>**psxcel** game/src/main.rs:36,2244 |
| `psx_gpu::draw_quad_flat` | use `gpu.draw(&QuadFlat::new(..))` | **cs-psx** game/src/main.rs:1284,1296,1911,1974,1975,2061,2062,2070,2184,2185,2196; game/src/menu.rs:709,724,781,820,1078<br>**hl-psx** game/src/main.rs:1628,1640,2178,2242,2243,2332,2333,2341,2469,2531,2532,2543; game/src/menu.rs:811,826,883,922,1186<br>**nitroxide** game/src/main.rs:1452<br>**psxcel** game/src/main.rs:36,1725 |
| `psx_gpu::draw_quad_textured` | use `gpu.draw(&QuadTexturedMaterial::with_material(..))` | **nitroxide** game/src/main.rs:1254<br>**quake-psx** game/src/intro.rs:67<br>**voxide** game/src/main.rs:4351 |
| `psx_gpu::draw_quad_textured_gouraud_material` | use `gpu.draw(&QuadTexturedGouraud::with_material(..))` | **oot-psx** game/src/font.rs:200,210; game/src/hud.rs:234,312,342; game/src/skybox.rs:192; game/src/title.rs:793 |
| `psx_gpu::draw_quad_textured_material` | use `gpu.draw(&QuadTexturedMaterial::with_material(..))` | **cs-psx** game/src/main.rs:1329,1345,22680; game/src/menu.rs:293,383,388,410,504,675<br>**hl-psx** game/src/main.rs:1653,1669; game/src/menu.rs:394,485,490,512,606,777<br>**oot-psx** game/src/hud.rs:273,376 |
| `psx_gpu::draw_rect_flat` | use `gpu.draw(&QuadFlat::rect(origin, size, color))` | **PSoXide-editor** engine/examples/toolchain-probe/src/main.rs:48<br>**hk-psx** game/src/menu.rs:100,101,107,108<br>**psxcel** game/src/main.rs:36,2250<br>**voxide** game/src/main.rs:4064,4065,4066,4067,4068,4069,4076,4077,4092,4095,4104,4114,4115,4116,4117,4193,4196,4205,4215,4216,4217,4218,4426,4428,4430,4484 |
| `psx_gpu::draw_sprite_material` | use `gpu.set_draw_mode(material)` and `gpu.draw(&Sprite::with_material(..))` | **cs-psx** game/src/main.rs:1362,1374<br>**hk-psx** game/src/menu.rs:39,58,176<br>**hl-psx** game/src/main.rs:1686,1698 |
| `psx_gpu::draw_sync` | use `Gpu::wait_idle` | **cs-psx** game/src/main.rs:1992,2371,24178,24181,27895,28358; game/src/menu.rs:470,1430<br>**hk-psx** game/src/disc.rs:1033; game/src/hero_light.rs:87; game/src/main.rs:635; game/src/render.rs:301; game/src/scene_transition.rs:28,30; game/src/vram_cache.rs:60<br>**hl-psx** game/src/main.rs:2260,2471,2771,30959,30962,34956,35277; game/src/menu.rs:572,1360,1588<br>**quake-psx** game/src/intro.rs:100,108; game/src/platform.rs:130,813,957<br>**voxide** game/src/main.rs:3886,4386,4431,10538<br>**wipeout-psx** game/src/menus.rs:129,137; game/src/screen.rs:86,258 |
| `psx_gpu::draw_tri_flat` | use `gpu.draw(&TriFlat::new(..))` | **cs-psx** game/src/main.rs:1305,1316<br>**nitroxide** game/src/main.rs:1440<br>**oot-psx** game/src/main.rs:1436,1437<br>**psxcel** game/src/main.rs:36,1794 |
| `psx_gpu::draw_tri_flat_blended` | use `gpu.set_draw_mode(material)` and `gpu.draw(&TriFlat::new(..).translucent())` | **cs-psx** game/src/main.rs:1261,1275<br>**hl-psx** game/src/main.rs:1605,1619<br>**oot-psx** game/src/intro.rs:1121,1122,1473,1474; game/src/main.rs:1398,1405,1441,1448,1458,1459,1582,1583; game/src/menu.rs:177,178,195,196; game/src/message.rs:130,137 |
| `psx_gpu::draw_tri_gouraud` | use `gpu.draw(&TriGouraud::new(..))` | **nitroxide** game/src/draw.rs:578,579; game/src/main.rs:1393,1394<br>**oot-psx** game/src/intro.rs:1484,1485,1490,1494; game/src/menu.rs:165,166,171,172; game/src/modeltest.rs:412,413 |
| `psx_gpu::draw_tri_textured_material` | use `gpu.draw(&TriTextured::with_material(..))` | **hl-psx** game/src/main.rs:28939 |
| `psx_gpu::fill_rect` | use `gpu.draw(&FillRect::new(origin, size, color))` | **PSoXide-editor** engine/examples/toolchain-probe/src/main.rs:44<br>**hk-psx** game/src/exit_fade.rs:54; game/src/scene_transition.rs:30<br>**nitroxide** game/src/main.rs:1186,1187,1243,1290,1291<br>**quake-psx** game/src/platform.rs:566,567 |
| `psx_gpu::framebuf::FrameBuffer` | use `psx_gpu::display::DoubleBuffer`, whose methods take the `Gpu` | **cs-psx** game/src/main.rs:94,1961,2004,2031,2033,2048,2057,2174,2241,3858,22969,23541; game/src/menu.rs:21,422,868,1241<br>**hk-psx** game/src/audio_probe.rs:60,68,129,183; game/src/main.rs:9,493; game/src/menu.rs:3,121,136,144,174,193,200,205; tests/menu_runtime.rs:79,86,90,95,101,108,111,115<br>**hl-psx** game/src/main.rs:104,2229,2272,2299,2301,2316,2326,2462,2521,2595,4130,29279,29957; game/src/menu.rs:21,524,970,1338,1377<br>**oot-psx** game/src/bg.rs:12,43; game/src/intro.rs:29,451,701,823,1157; game/src/main.rs:56,261,378,1232,1474,1592,1723; game/src/menu.rs:15,67; game/src/modeltest.rs:28,81<br>**quake-psx** game/src/intro.rs:11,28; game/src/platform.rs:3,34,121,562,593<br>**voxide** game/src/main.rs:55,2552,3720,3898,4313,4398,4412,4423,4809,4867,4960,4989,10516 |
| `psx_gpu::init` | use `Gpu::new(dma, DisplayConfig::new(mode, res))` | **PSoXide-editor** engine/examples/toolchain-probe/src/main.rs:38<br>**cs-psx** game/src/main.rs:22968<br>**hk-psx** game/src/main.rs:491<br>**hl-psx** game/src/main.rs:29278<br>**oot-psx** game/src/main.rs:260<br>**quake-psx** game/src/platform.rs:555<br>**voxide** game/src/main.rs:2551<br>**wipeout-psx** game/src/main.rs:194 |
| `psx_gpu::material::BlendMode::from_tpage_bits` | renamed to `from_texture_page_bits` | **PSoXide-emulator** emu/crates/emulator-core/src/gpu.rs:2502,3812; emu/crates/emulator-core/src/gpu/tests.rs:332,333,334,335,337 |
| `psx_gpu::material::BlendMode::tpage_bits` | renamed to `texture_page_bits` | **cs-psx** game/src/hud.rs:210,212; game/src/menu.rs:358,364,565<br>**hl-psx** game/src/hud.rs:172; game/src/menu.rs:460,466,667 |
| `psx_gpu::material::TextureMaterial::apply_draw_mode` | use `Gpu::set_draw_mode` | **nitroxide** game/src/draw.rs:516,2113 |
| `psx_gpu::material::TextureMaterial::tpage_word` | renamed to `texture_page_word` | **cs-psx** game/src/main.rs:1338<br>**hl-psx** game/src/main.rs:1662 |
| `psx_gpu::ot::OrderingTable::add` | keeps the packet's address past the borrow and trusts `words`; use `OrderingTable::frame` and `OtFrame::add` | **nitroxide** game/src/draw.rs:7378 |
| `psx_gpu::ot::OrderingTable::insert` | use `OtFrame::add_raw`, through `frame()` or `resume_frame()` | **voxide** game/src/main.rs:6069,6458,6527,7052,7190,7319,8158 |
| `psx_gpu::ot::OrderingTable::insert_packed_commands_reverse_unchecked` | use `OtFrame::add_packed_commands_reverse_unchecked`, through `frame()` or `resume_frame()` | **quake-psx** game/src/platform.rs:826,871 |
| `psx_gpu::ot::OrderingTable::insert_tagged_packet_stream_unchecked` | use `OtFrame::add_tagged_packet_stream_unchecked`, through `frame()` or `resume_frame()` | **quake-psx** game/src/platform.rs:750 |
| `psx_gpu::ot::OrderingTable::insert_unchecked` | use `OtFrame::add_raw_unchecked`, through `frame()` or `resume_frame()` | **voxide** game/src/main.rs:8861,8886<br>**wipeout-psx** game/src/order.rs:73; game/src/render.rs:626; tools/order-tests/lib.rs:56 |
| `psx_gpu::ot::OrderingTable::submit` | nothing proves the linked packets are alive; use `OrderingTable::frame` and `OtFrame::submit` | **oot-psx** game/src/room.rs:737<br>**voxide** game/src/main.rs:3868,10482 |
| `psx_gpu::ot::OrderingTable::submit_async` | returns mid-walk with the table still mutable; use `OtFrame::submit_with`, `FrameStorage::draw_async` or `FramePair` | **hk-psx** game/src/render.rs:1558<br>**voxide** game/src/main.rs:10507 |
| `psx_gpu::prim::TriTextured::with_material_packet_texcoords` | identical to `with_material` | **PSoXide-emulator** emu/crates/emulator-core/src/gpu/tests.rs:1260,1261 |
| `psx_gpu::prim::TriTexturedGouraud::with_material_packet_texcoords` | identical to `with_material` | **PSoXide-emulator** emu/crates/emulator-core/src/gpu/tests.rs:1612,1618 |
| `psx_gpu::set_display_offset` | use `Gpu::set_display` with `DisplayConfig::with_offset` | **cs-psx** game/src/settings.rs:8,159<br>**hl-psx** game/src/settings.rs:8,144 |
| `psx_gpu::set_draw_area` | use `Gpu::set_draw_area` | **PSoXide-editor** engine/examples/toolchain-probe/src/main.rs:39<br>**cs-psx** game/src/main.rs:1174,1963,1995,22970,27937,28329<br>**hk-psx** game/src/main.rs:499<br>**hl-psx** game/src/main.rs:2231,2263,29280<br>**nitroxide** game/src/draw.rs:307<br>**oot-psx** game/src/main.rs:262<br>**psxcel** game/src/main.rs:2122,2127<br>**quake-psx** game/src/platform.rs:563<br>**voxide** game/src/main.rs:2553 |
| `psx_gpu::set_draw_offset` | use `Gpu::set_draw_offset` | **PSoXide-editor** engine/examples/toolchain-probe/src/main.rs:40<br>**cs-psx** game/src/main.rs:1161,1162,1964,1996,22971,28104,28118<br>**hk-psx** game/src/main.rs:499<br>**hl-psx** game/src/main.rs:1515,1536,2232,2264,29281<br>**oot-psx** game/src/main.rs:299<br>**quake-psx** game/src/platform.rs:564<br>**voxide** game/src/main.rs:2554 |
| `psx_gpu::signal_draw_done` | use `Gpu::signal_draw_done` | **hk-psx** game/src/menu.rs:124; game/src/scene_transition.rs:23,39<br>**quake-psx** game/src/platform.rs:319,407 |
| `psx_gpu::submit_linked_list` | use `OrderingTable::frame`, `Gpu::submit_static`, or the unsafe `chain::submit_raw` | **cs-psx** game/src/main.rs:1080<br>**hl-psx** game/src/main.rs:1443 |
| `psx_gpu::submit_linked_list_async` | use `OrderingTable::frame`, `Gpu::submit_static`, or the unsafe `chain::submit_async_raw` | **cs-psx** game/src/main.rs:1157,26544<br>**hl-psx** game/src/main.rs:33401<br>**wipeout-psx** game/src/render.rs:674,682 |
| `psx_gpu::submit_linked_list_wait` | use `chain::wait`, which takes the `GpuDma` token | **cs-psx** game/src/main.rs:1141,26519<br>**hk-psx** game/src/render.rs:693,910<br>**hl-psx** game/src/main.rs:1510,33377<br>**nitroxide** game/src/draw.rs:7364<br>**quake-psx** game/src/platform.rs:129,405,812<br>**voxide** game/src/main.rs:10537<br>**wipeout-psx** game/src/screen.rs:85,206,257 |
| `psx_gpu::vsync` | busy-waits a fixed 242 HBlanks from the call site instead of \ syncing to the display; use psx_rt::interrupts::wait_vblank() | **oot-psx** game/src/main.rs:294,1301,1519,1665,1695,1793; game/src/menu.rs:95,106; game/src/modeltest.rs:248 |
| `psx_gpu::wait_idle` | use `Gpu::wait_idle` | **PSoXide-editor** engine/examples/toolchain-probe/src/main.rs:43 |

## psx-gte

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_gte::ops::nccs` | renamed to light_color_single | **nitroxide** game/src/draw.rs:3935,3960 |
| `psx_gte::ops::ncct` | renamed to light_color_triple | **nitroxide** game/src/draw.rs:3923 |
| `psx_gte::ops::rtps` | renamed to project_single | **nitroxide** game/src/draw.rs:3954<br>**voxide** game/src/main.rs:8710<br>**wipeout-psx** game/src/trails.rs:488 |
| `psx_gte::ops::rtpt` | renamed to project_triple | **nitroxide** game/src/draw.rs:3910<br>**wipeout-psx** game/src/trails.rs:363 |
| `psx_gte::regs::mfc2` | renamed to `read_data!` | **PSoXide-editor** engine/examples/hardware-tests/src/main.rs:7188,7189,7190,7191,7192,7193<br>**nitroxide** game/src/draw.rs:3787,3911,3912,3924,3936,3955,3963; game/src/main.rs:12<br>**voxide** game/src/main.rs:12,8729,8730<br>**wipeout-psx** game/src/trails.rs:364,365,366,367,368,369,489 |
| `psx_gte::regs::mtc2` | renamed to `write_data!` | **PSoXide-editor** engine/examples/hardware-tests/src/main.rs:7070,7071,7072,7073,7074,7075,7113,7114,7115,7116,7117,7118,7181,7182,7183,7184,7185,7186<br>**nitroxide** game/src/draw.rs:3787,3902,3903,3904,3905,3906,3907,3914,3915,3916,3917,3918,3919,3920,3931,3932,3933,3951,3952,3956,3957,3958; game/src/main.rs:12<br>**voxide** game/src/main.rs:12,8707,8708<br>**wipeout-psx** game/src/trails.rs:355,356,357,358,359,360,485,486 |
| `psx_gte::scene::aabb_outside_clip4` | renamed to `is_aabb_outside_clip4` | **quake-psx** game/src/renderer.rs:3284,6039,6106,6505,6582,6629 |
| `psx_gte::scene::classic_otz3_from_sum` | renamed to `classic_ordering_depth3_from_sum` | **PSoXide-editor** engine/examples/editor-playtest/src/runtime_config.rs:848,856 |
| `psx_gte::scene::rtpt_kick` | renamed to `start_project_triple` | **voxide** game/src/main.rs:8687<br>**wipeout-psx** game/src/render.rs:887,1974 |
| `psx_gte::scene::screen_area_mac0_scheduled` | renamed to `screen_area_scheduled` | **cs-psx** game/src/main.rs:9423<br>**hl-psx** game/src/main.rs:13572<br>**wipeout-psx** game/src/render.rs:906 |
| `psx_gte::scene::set_avsz_weights` | renamed to `set_average_z_weights` | **quake-psx** game/src/platform.rs:610 |

## psx-io

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_io::cd::acknowledge_irq` | use `Cd::acknowledge_irq` with the `Cd` token | **PSoXide-editor** engine/crates/psx-engine/src/game_app.rs:866; engine/crates/psx-game-runtime/src/cd_stream/hw.rs:472; engine/examples/hardware-tests/src/main.rs:5621,5626,5631,5636,5766,5772,5777 |
| `psx_io::cd::audio::PlaybackStarter::started` | renamed to `has_started` | **nitroxide** game/src/music.rs:234<br>**quake-psx** game/src/music.rs:233<br>**wipeout-psx** game/src/music/cdda.rs:76 |
| `psx_io::cd::audio::PlaybackStarter::tick` | use `tick_on` with the `Cd` token | **nitroxide** game/src/music.rs:222<br>**quake-psx** game/src/music.rs:228<br>**wipeout-psx** game/src/music/cdda.rs:72 |
| `psx_io::cd::discard_response` | use `Cd::discard_response` with the `Cd` token | **PSoXide-editor** engine/crates/psx-engine/src/game_app.rs:865; engine/examples/hardware-tests/src/main.rs:5620,5625,5630,5635,5771,5776 |
| `psx_io::cd::dispatch_command` | use `Cd::dispatch_command` with the `Cd` token | **PSoXide-editor** engine/crates/psx-engine/src/game_app.rs:836; engine/examples/hardware-tests/src/main.rs:5609 |
| `psx_io::cd::irq_flag_value` | use `Cd::irq_flag_value` with the `Cd` token | **PSoXide-editor** engine/crates/psx-engine/src/game_app.rs:859; engine/crates/psx-game-runtime/src/cd_stream/hw.rs:476; engine/examples/hardware-tests/src/main.rs:5617,5764 |
| `psx_io::cd::poll_data_sector` | use `Cd::poll_data_sector` with the `Cd` token | **PSoXide-editor** engine/crates/psx-goldsrc/src/chunk_stream.rs:79; engine/examples/hardware-tests/src/lever_probes.rs:1197 |
| `psx_io::cd::reader::SectorReader::diag` | renamed to `diagnostics` | **quake-psx** game/src/platform.rs:1103 |
| `psx_io::cd::reader::SectorReader::new` | use `SectorReader::with_cd` with the `Cd` token | **PSoXide-editor** engine/crates/psx-chainloader/src/runtime.rs:141; engine/crates/psx-goldsrc/tests/oracles/legacy-cdstream.rs:40; engine/examples/hardware-tests/src/cd_chain_probe.rs:590; engine/examples/hardware-tests/src/lever_probes.rs:1148; engine/examples/hardware-tests/src/xa_loop.rs:40<br>**cs-psx** game/src/cdstream.rs:26<br>**hk-psx** game/src/disc.rs:369<br>**hl-psx** game/src/cdstream.rs:26<br>**nitroxide** game/src/assets.rs:23<br>**quake-psx** game/src/platform.rs:41<br>**voxide** game/src/sfx.rs:92<br>**wipeout-psx** game/src/loader.rs:11 |
| `psx_io::cd::restore_irq_output` | use `Cd::restore_irq_output` with the `Cd` token | **PSoXide-editor** engine/crates/psx-engine/src/game_app.rs:879,896; engine/examples/hardware-tests/src/main.rs:5640 |
| `psx_io::cd::try_command` | use `Cd::try_command` with the `Cd` token | **PSoXide-editor** engine/crates/psx-engine/src/game_app.rs:793 |
| `psx_io::cd::try_mute` | use `Cd::try_mute` with the `Cd` token | **PSoXide-editor** engine/examples/hardware-tests/src/audio_probe.rs:183; engine/examples/hardware-tests/src/main.rs:5700,5734,5754 |
| `psx_io::cd::try_pause_until_complete` | use `Cd::try_pause_until_complete` with the `Cd` token | **PSoXide-editor** engine/crates/psx-engine/src/game_app.rs:905; engine/examples/hardware-tests/src/audio_probe.rs:117,192 |
| `psx_io::cd::try_play_position` | use `Cd::try_play_position` with the `Cd` token | **PSoXide-editor** engine/examples/hardware-tests/src/main.rs:4633,5723,5750 |
| `psx_io::cd::try_play_track` | use `Cd::try_play_track` with the `Cd` token | **PSoXide-editor** engine/examples/game-magikaaaaaarp-pong/src/main.rs:1078; engine/examples/hardware-tests/src/main.rs:5667,5716,5743 |
| `psx_io::cd::try_set_mode` | use `Cd::try_set_mode` with the `Cd` token | **PSoXide-editor** engine/examples/game-magikaaaaaarp-pong/src/main.rs:1074; engine/examples/hardware-tests/src/audio_probe.rs:176; engine/examples/hardware-tests/src/main.rs:4626,5663,5678,5712,5742,5825 |
| `psx_io::cd::try_set_target_lba` | use `Cd::try_set_target_lba` with the `Cd` token | **PSoXide-editor** engine/examples/hardware-tests/src/audio_probe.rs:177; engine/examples/hardware-tests/src/main.rs:5679,5788,5799,5811,5827 |
| `psx_io::cd::try_start_reading` | use `Cd::try_start_reading` with the `Cd` token | **PSoXide-editor** engine/examples/hardware-tests/src/audio_probe.rs:178; engine/examples/hardware-tests/src/main.rs:5680,5828 |
| `psx_io::cd::try_status` | use `Cd::try_status` with the `Cd` token | **PSoXide-editor** engine/examples/hardware-tests/src/cd_chain_probe.rs:664; engine/examples/hardware-tests/src/main.rs:4619,10177,11189 |
| `psx_io::cd::try_unmute` | use `Cd::try_unmute` with the `Cd` token | **PSoXide-editor** engine/examples/game-magikaaaaaarp-pong/src/main.rs:1076; engine/examples/hardware-tests/src/audio_probe.rs:118,169,174,186,193; engine/examples/hardware-tests/src/main.rs:5670 |
| `psx_io::cdda::CddaEndDetector` | renamed to `psx_io::cd::audio::EndDetector` | **nitroxide** game/src/music.rs:34,104,125<br>**quake-psx** game/src/music.rs:12,55,70<br>**wipeout-psx** game/src/music/cdda.rs:10,15,24 |
| `psx_io::cdda::CddaStarter` | renamed to `psx_io::cd::audio::PlaybackStarter` | **nitroxide** game/src/music.rs:34,103,124,185,192<br>**quake-psx** game/src/music.rs:12,54,69,156,164,186<br>**wipeout-psx** game/src/music/cdda.rs:10,14,23 |
| `psx_io::cdrom::BASE` | moved to `psx_hw::cd::BASE` | **cs-psx** game/src/cd_irq.rs:18<br>**hk-psx** game/src/cd_stream.rs:10<br>**nitroxide** game/src/music.rs:79,80 |
| `psx_io::cdrom::CMD_GETSTAT` | moved to `psx_hw::cd::CMD_GETSTAT` | **nitroxide** game/src/music.rs:239 |
| `psx_io::cdrom::CMD_PAUSE` | moved to `psx_hw::cd::CMD_PAUSE` | **cs-psx** game/src/cd_irq.rs:150<br>**hk-psx** game/src/cd_stream.rs:112 |
| `psx_io::cdrom::CMD_READN` | moved to `psx_hw::cd::CMD_READN` | **cs-psx** game/src/cd_irq.rs:311<br>**hk-psx** game/src/cd_stream.rs:217 |
| `psx_io::cdrom::CMD_SEEKL` | moved to `psx_hw::cd::CMD_SEEKL` | **cs-psx** game/src/cd_irq.rs:308<br>**hk-psx** game/src/cd_stream.rs:214 |
| `psx_io::cdrom::CMD_SETLOC` | moved to `psx_hw::cd::CMD_SETLOC` | **cs-psx** game/src/cd_irq.rs:186<br>**hk-psx** game/src/cd_stream.rs:131,225 |
| `psx_io::cdrom::CMD_SETMODE` | moved to `psx_hw::cd::CMD_SETMODE` | **cs-psx** game/src/cd_irq.rs:310<br>**hk-psx** game/src/cd_stream.rs:216 |
| `psx_io::cdrom::MODE_AUTO_PAUSE` | moved to `psx_hw::cd::MODE_AUTO_PAUSE` | **hl-psx** game/src/main.rs:5861<br>**wipeout-psx** game/src/music/cdda.rs:83 |
| `psx_io::cdrom::MODE_CDDA` | moved to `psx_hw::cd::MODE_CDDA` | **hl-psx** game/src/main.rs:5860<br>**wipeout-psx** game/src/music/cdda.rs:83 |
| `psx_io::cdrom::MODE_DOUBLE_SPEED` | moved to `psx_hw::cd::MODE_DOUBLE_SPEED` | **cs-psx** game/src/cd_irq.rs:310<br>**hk-psx** game/src/cd_stream.rs:216<br>**hl-psx** game/src/main.rs:5859 |
| `psx_io::cdrom::PlayPosition` | moved to `psx_io::cd::PlayPosition` | **hk-psx** game/src/audio_probe.rs:108; game/src/xa_player.rs:110 |
| `psx_io::cdrom::SectorPollError` | moved to `psx_io::cd::SectorPollError` | **PSoXide-editor** engine/crates/psx-goldsrc/tests/support/chunk_stream_harness.rs:108,131,161 |
| `psx_io::cdrom::acknowledge_irq` | moved to `psx_io::cd::acknowledge_irq` | **cs-psx** game/src/cd_irq.rs:227,289,293,301,306<br>**hk-psx** game/src/cd_stream.rs:201,202,209,212,240<br>**nitroxide** game/src/music.rs:276 |
| `psx_io::cdrom::bcd_to_bin` | moved to `psx_io::cd::bcd_to_bin` | **nitroxide** game/src/music.rs:90<br>**quake-psx** game/src/music.rs:257 |
| `psx_io::cdrom::bin_to_bcd` | moved to `psx_io::cd::bin_to_bcd` | **cs-psx** game/src/cd_irq.rs:170,171,172<br>**hk-psx** game/src/cd_stream.rs:123 |
| `psx_io::cdrom::discard_response` | moved to `psx_io::cd::discard_response` | **cs-psx** game/src/cd_irq.rs:122,226,288,292,300,305<br>**hk-psx** game/src/cd_stream.rs:99,201,202,209,212,240<br>**nitroxide** game/src/music.rs:275 |
| `psx_io::cdrom::dispatch_command` | moved to `psx_io::cd::dispatch_command` | **nitroxide** game/src/music.rs:239<br>**wipeout-psx** game/src/music/cdda.rs:109 |
| `psx_io::cdrom::irq_flag_value` | moved to `psx_io::cd::irq_flag_value` | **cs-psx** game/src/cd_irq.rs:285<br>**hk-psx** game/src/cd_stream.rs:198<br>**nitroxide** game/src/music.rs:259<br>**wipeout-psx** game/src/music/cdda.rs:92 |
| `psx_io::cdrom::poll_data_sector` | moved to `psx_io::cd::poll_data_sector` | **PSoXide-editor** engine/crates/psx-goldsrc/tests/oracles/legacy-cdstream.rs:453 |
| `psx_io::cdrom::restore_irq_output` | moved to `psx_io::cd::restore_irq_output` | **nitroxide** game/src/music.rs:284,293<br>**wipeout-psx** game/src/music/cdda.rs:46,103 |
| `psx_io::cdrom::set_audio_mixer` | use `Cd::set_audio_mixer` with the `Cd` token | **hk-psx** game/src/xa_player.rs:59 |
| `psx_io::cdrom::try_command` | moved to `psx_io::cd::try_command` | **hk-psx** game/src/audio_probe.rs:115,117; game/src/xa_player.rs:55,69<br>**hl-psx** game/src/main.rs:5869,5871<br>**nitroxide** game/src/music.rs:302<br>**quake-psx** game/src/music.rs:250 |
| `psx_io::cdrom::try_demute` | renamed to `psx_io::cd::try_unmute` | **hk-psx** game/src/audio_probe.rs:113; game/src/xa_player.rs:53<br>**hl-psx** game/src/main.rs:5875 |
| `psx_io::cdrom::try_get_loc_p` | renamed to `psx_io::cd::try_play_position` | **hk-psx** game/src/audio_probe.rs:108; game/src/xa_player.rs:110<br>**hl-psx** game/src/main.rs:5926 |
| `psx_io::cdrom::try_get_stat` | renamed to `psx_io::cd::try_status` | **hl-psx** game/src/main.rs:5915<br>**quake-psx** game/src/music.rs:235<br>**wipeout-psx** game/src/music/cdda.rs:84 |
| `psx_io::cdrom::try_mute` | moved to `psx_io::cd::try_mute` | **hl-psx** game/src/main.rs:5894,5937,5957 |
| `psx_io::cdrom::try_pause` | use `Cd::try_pause` with the `Cd` token | **hl-psx** game/src/main.rs:5895,5958<br>**nitroxide** game/src/music.rs:184<br>**quake-psx** game/src/music.rs:155 |
| `psx_io::cdrom::try_pause_until_complete` | moved to `psx_io::cd::try_pause_until_complete` | **hk-psx** game/src/audio_probe.rs:142,177; game/src/music.rs:100,408; game/src/xa_player.rs:76,84<br>**quake-psx** game/src/music.rs:185<br>**wipeout-psx** game/src/music/cdda.rs:66 |
| `psx_io::cdrom::try_play_track` | moved to `psx_io::cd::try_play_track` | **hl-psx** game/src/main.rs:5873 |
| `psx_io::cdrom::try_set_loc_lba` | renamed to `psx_io::cd::try_set_target_lba` | **hk-psx** game/src/audio_probe.rs:116; game/src/xa_player.rs:69 |
| `psx_io::cdrom::try_set_mode` | moved to `psx_io::cd::try_set_mode` | **hk-psx** game/src/audio_probe.rs:114; game/src/xa_player.rs:54<br>**hl-psx** game/src/main.rs:5858<br>**wipeout-psx** game/src/music/cdda.rs:83 |
| `psx_io::dma::Channel::Cdrom` | renamed to `Channel::Cd` | **oot-psx** game/src/loader.rs:135,161,162,163,165 |
| `psx_io::dma::clear_ordering_table` | use `OrderingTableClearDma::clear_table` with the token | **PSoXide-editor** engine/examples/hardware-tests/src/main.rs:9931 |
| `psx_io::dma::set_bcr_manual` | use the unsafe `dma::start` with `dma::size_words`, or `dma::raw::set_size` for probes | **oot-psx** game/src/loader.rs:162 |
| `psx_io::dma::set_chcr` | a safe control store starts DMA from safe code; use the unsafe `dma::start`, `dma::abort` to stop a channel, or `dma::raw::set_control` for probes | **oot-psx** game/src/loader.rs:163 |
| `psx_io::dma::set_madr` | a safe address store lets safe code aim DMA anywhere in RAM; use the unsafe `dma::start`, or `dma::raw::set_address` for probes | **oot-psx** game/src/loader.rs:161 |
| `psx_io::gpu::gpustat` | renamed to `status` | **hk-psx** game/src/animation_cache.rs:143; game/src/exit_fade.rs:32 |
| `psx_io::gpu::wait_cmd_ready` | renamed to `wait_command_ready` | **hk-psx** game/src/animation_cache.rs:111; game/src/exit_fade.rs:12,53,66,67,68,75,76; game/src/vram_cache.rs:60<br>**hl-psx** game/src/main.rs:28990 |
| `psx_io::gpu::write_gp0` | renamed to `write_command` | **hk-psx** game/src/animation_cache.rs:112; game/src/exit_fade.rs:12,53,66,67,68,75,78,79,80,81; game/src/vram_cache.rs:60<br>**hl-psx** game/src/main.rs:28993,28996,28997,28998,28999,29000,29001,29002<br>**quake-psx** game/src/platform.rs:814,815,816 |
| `psx_io::gpu::write_gp1` | renamed to `write_display_control` | **cs-psx** game/src/main.rs:4841<br>**hk-psx** game/src/exit_fade.rs:48,57; game/src/presentation.rs:84<br>**hl-psx** game/src/main.rs:5093 |
| `psx_io::irq::ack` | renamed to `acknowledge` | **cs-psx** game/src/cd_irq.rs:110,121,190,228<br>**hk-psx** game/src/audio_stream.rs:259; game/src/cd_stream.rs:89,98,241,259<br>**oot-psx** game/src/loader.rs:168 |
| `psx_io::irq::source::CDROM` | moved to `psx_hw::irq::source::CDROM` | **cs-psx** game/src/cd_irq.rs:19<br>**hk-psx** game/src/cd_stream.rs:11 |
| `psx_io::irq::source::DMA` | moved to `psx_hw::irq::source::DMA` | **oot-psx** game/src/loader.rs:168 |
| `psx_io::irq::source::SPU` | moved to `psx_hw::irq::source::SPU` | **hk-psx** game/src/audio_stream.rs:234 |
| `psx_io::read8` | renamed to `read_u8` | **cs-psx** game/src/cd_irq.rs:131,250,264,265,266,267<br>**hk-psx** game/src/cd_stream.rs:104,167,173,174,207,208<br>**nitroxide** game/src/music.rs:266,267<br>**oot-psx** game/src/loader.rs:20,200,214,215 |
| `psx_io::write8` | renamed to `write_u8` | **cs-psx** game/src/cd_irq.rs:91,97,118,125,140,144,184,246<br>**hk-psx** game/src/cd_stream.rs:71,73,96,100,107,110,130,164,224<br>**oot-psx** game/src/loader.rs:20,137,138,148,158,188,190,221,230 |

## psx-math

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_math::attributed_clip::clip_convex_plane` | use the safe `clip_to_plane` (checked) or `clip_to_plane_unchecked` (proven capacity) | **voxide** game/src/main.rs:66,7781 |
| `psx_math::attributed_clip::clip_convex_plane_uninit` | use the safe `clip_to_plane_uninit` (checked) or `clip_to_plane_uninit_unchecked` (proven capacity) | **quake-psx** game/src/renderer.rs:15,6851 |

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
| `psx_pad::Deadzone::outside` | renamed to `is_outside` | **cs-psx** game/src/main.rs:3596,3600<br>**hl-psx** game/src/main.rs:4043,4047 |
| `psx_pad::PadReader::poll` | use `poll_on` with the `ControllerPort` token | **wipeout-psx** game/src/screen.rs:287,293 |
| `psx_pad::enable_analog_port1` | use `enable_analog_on` with the `ControllerPort` token | **PSoXide-editor** engine/crates/psx-engine/src/app.rs:46,697; engine/examples/hardware-tests/src/controller_test.rs:115,133; engine/examples/hardware-tests/src/main.rs:9875<br>**cs-psx** game/src/main.rs:101,3891,3901,23549<br>**hl-psx** game/src/main.rs:111,4163,4173,29965<br>**nitroxide** game/src/main.rs:1638,1837<br>**oot-psx** game/src/main.rs:59,388,638<br>**quake-psx** game/src/input.rs:3,87; game/src/quake.rs:56<br>**voxide** game/src/main.rs:69,2570,2709 |
| `psx_pad::enable_analog_port2` | use `enable_analog_on` with the `ControllerPort` token | **PSoXide-editor** engine/examples/hardware-tests/src/controller_test.rs:116,136<br>**cs-psx** game/src/main.rs:23550,24625<br>**nitroxide** game/src/main.rs:1639,1839 |
| `psx_pad::poll_port1` | use `poll_on` with the `ControllerPort` token | **PSoXide-editor** engine/examples/hardware-tests/src/console_tests.rs:114,120; engine/examples/hardware-tests/src/controller_test.rs:117,134; engine/examples/hardware-tests/src/fmv_diag.rs:1471; engine/examples/hardware-tests/src/fmv_test.rs:59,62,65; engine/examples/hardware-tests/src/lever_probes.rs:1205; engine/examples/hardware-tests/src/main.rs:5255,5413,9872,10202,10204,10206<br>**cs-psx** game/src/main.rs:101,2252,2256,3892,23028,23088,24212,28207; game/src/menu.rs:23,438,908<br>**hl-psx** game/src/main.rs:111,2607,2611,4164,29356,29420,30993,35141; game/src/menu.rs:23,540,1010,1363<br>**oot-psx** game/src/intro.rs:32,477,669,736; game/src/main.rs:59,816,1256,1492,1611,1749; game/src/menu.rs:18,74,103; game/src/modeltest.rs:32,104<br>**quake-psx** game/src/input.rs:3,58,88; game/src/intro.rs:12,48,51<br>**voxide** game/src/main.rs:73,75,2615,2695,3732,3740,3817,4332,4335; game/src/tunelab.rs:74 |
| `psx_pad::poll_port1_diag` | use `poll_diagnostics_on` with the `ControllerPort` token | **cs-psx** game/src/main.rs:101,3863<br>**hl-psx** game/src/main.rs:113,4135 |
| `psx_pad::poll_port1_diagnostics` | use `poll_diagnostics_on` with the `ControllerPort` token | **PSoXide-editor** engine/examples/hardware-tests/src/main.rs:3100,6161,9839,9856,9876 |
| `psx_pad::poll_port2` | use `poll_on` with the `ControllerPort` token | **PSoXide-editor** engine/crates/psx-engine/src/scene.rs:20,267; engine/examples/hardware-tests/src/controller_test.rs:117,126,137<br>**cs-psx** game/src/main.rs:24620 |
| `psx_pad::require_analog_port1` | use `require_analog_on` with the `ControllerPort` token | **PSoXide-editor** engine/crates/psx-engine/src/app.rs:46,538<br>**hk-psx** game/src/main.rs:505<br>**wipeout-psx** game/src/main.rs:210 |

## psx-rt

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_rt::cache::flush_i_cache` | renamed to `flush_instruction_cache` | **cs-psx** game/src/cd_irq.rs:340<br>**hk-psx** game/src/cd_stream.rs:257; game/src/modules.rs:304,353<br>**oot-psx** game/src/vbl.rs:62<br>**quake-psx** game/src/platform.rs:337<br>**wipeout-psx** game/src/overlay.rs:108,210 |
| `psx_rt::interrupts::gp1_queue_pending` | renamed to `is_display_control_queued` | **cs-psx** game/src/main.rs:4836<br>**hk-psx** game/src/animation_cache.rs:42,142; game/src/main.rs:756,819,903; game/src/menu.rs:127; game/src/presentation.rs:63,76; game/src/render.rs:242,682,689,695,1347; game/src/scene_transition.rs:10,25,41; game/src/vram_cache.rs:36<br>**hl-psx** game/src/main.rs:5088<br>**wipeout-psx** game/src/screen.rs:84,139,188,250,256 |
| `psx_rt::interrupts::queue_gp1_at_vblank` | renamed to `queue_display_control_at_vblank` | **cs-psx** game/src/main.rs:26545<br>**hk-psx** game/src/menu.rs:125; game/src/presentation.rs:57; game/src/scene_transition.rs:24,40<br>**hl-psx** game/src/main.rs:33402<br>**wipeout-psx** game/src/screen.rs:44,146,214,236 |
| `psx_rt::interrupts::take_pending_gp1` | renamed to `take_queued_display_control` | **cs-psx** game/src/main.rs:4839<br>**hk-psx** game/src/presentation.rs:83<br>**hl-psx** game/src/main.rs:5091 |

## psx-settings

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_settings::load_slot_one` | use `load_from_slot_one` with the `ControllerPort` token | **PSoXide-editor** engine/examples/game-breakout/src/main.rs:356; engine/examples/game-invaders/src/main.rs:529; engine/examples/game-magikaaaaaarp-pong/src/main.rs:380; engine/examples/game-pong/src/main.rs:276<br>**nitroxide** game/src/main.rs:1640<br>**voxide** game/src/main.rs:3980 |
| `psx_settings::save_slot_one` | use `save_to_slot_one` with the `ControllerPort` token | **PSoXide-editor** engine/examples/game-breakout/src/main.rs:318; engine/examples/game-invaders/src/main.rs:487; engine/examples/game-magikaaaaaarp-pong/src/main.rs:366; engine/examples/game-pong/src/main.rs:215<br>**nitroxide** game/src/main.rs:767<br>**voxide** game/src/main.rs:4004 |

## psx-sfx

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_sfx::Bank::upload` | use `Bank::upload_on` with the `Spu` driver | **nitroxide** game/src/audio.rs:130 |

## psx-spu

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_spu::Voice::key_off` | renamed to `release` | **PSoXide-editor** engine/crates/psx-goldsrc/tests/oracles/legacy-hsfx-runtime.rs:34,45,61,408,426,474; tools/psoxide-dev/src/main.rs:1688<br>**hk-psx** game/src/ambience.rs:199,302,422; game/src/audio.rs:115,190,226,283,295; game/src/audio_stream.rs:320; game/src/focus_audio.rs:72,75,81,88; game/src/geo_audio.rs:64; game/src/runner_audio.rs:65,79,90,100; game/src/scene_sfx.rs:56,108,118<br>**nitroxide** game/src/audio.rs:199,264,282<br>**oot-psx** game/src/music.rs:259,291,342<br>**quake-psx** game/src/audio.rs:362<br>**wipeout-psx** game/src/audio.rs:559,588 |
| `psx_spu::Voice::key_on` | renamed to `start` | **PSoXide-editor** engine/crates/psx-goldsrc/tests/oracles/legacy-hsfx-runtime.rs:460<br>**hk-psx** game/src/ambience.rs:365; game/src/audio.rs:119,191,229; game/src/audio_stream.rs:305; game/src/focus_audio.rs:75,81; game/src/geo_audio.rs:69; game/src/runner_audio.rs:72,94; game/src/scene_sfx.rs:113,123<br>**nitroxide** game/src/audio.rs:261<br>**oot-psx** game/src/audio.rs:205; game/src/music.rs:355<br>**wipeout-psx** game/src/audio.rs:433,466,596 |
| `psx_spu::init` | use `Spu::new` with the `SpuDma` token | **PSoXide-editor** engine/crates/psx-engine/src/game_app.rs:688; engine/crates/psx-goldsrc/src/hsfx.rs:436; engine/crates/psx-goldsrc/tests/oracles/legacy-hsfx-runtime.rs:95; engine/examples/game-breakout/src/main.rs:351; engine/examples/game-invaders/src/main.rs:521; engine/examples/game-magikaaaaaarp-pong/src/main.rs:376; engine/examples/game-pong/src/main.rs:271; engine/examples/hardware-tests/src/audio_probe.rs:108; engine/examples/hardware-tests/src/handoff_probe.rs:187,268,327,345; engine/examples/hardware-tests/src/reverb_probe.rs:241,320,346,420; engine/examples/hardware-tests/src/ring_probe.rs:207; engine/examples/hardware-tests/src/sample_probe.rs:176; engine/examples/hardware-tests/src/spu_probe.rs:203,416; engine/examples/hardware-tests/src/transition_probe.rs:124,251,260; engine/examples/hardware-tests/src/voice_probe.rs:139,215; engine/examples/hardware-tests/src/xa_loop.rs:294<br>**hk-psx** game/src/audio.rs:60<br>**nitroxide** game/src/audio.rs:93<br>**oot-psx** game/src/audio.rs:139<br>**quake-psx** game/src/quake.rs:59<br>**voxide** game/src/sfx.rs:77<br>**wipeout-psx** game/src/audio.rs:158 |
| `psx_spu::irq_pending` | renamed to `is_irq_pending` | **hk-psx** game/src/audio_stream.rs:340 |
| `psx_spu::upload_adpcm` | use `Spu::upload_adpcm` with the `SpuDma` token | **PSoXide-editor** engine/crates/psx-engine/src/game_app.rs:1379; engine/crates/psx-goldsrc/src/hsfx.rs:457,465,506,523; engine/crates/psx-goldsrc/tests/oracles/legacy-hsfx-runtime.rs:116,124,161,173; engine/examples/hardware-tests/src/audio_link.rs:146,153; engine/examples/hardware-tests/src/audio_probe.rs:112; engine/examples/hardware-tests/src/handoff_probe.rs:278,386; engine/examples/hardware-tests/src/main.rs:8276,9261,9464; engine/examples/hardware-tests/src/reverb_probe.rs:331,461; engine/examples/hardware-tests/src/ring_probe.rs:539,540; engine/examples/hardware-tests/src/sample_probe.rs:276; engine/examples/hardware-tests/src/spu_probe.rs:346,487,695,700,718,720,721,726,749; engine/examples/hardware-tests/src/transition_probe.rs:198,313; engine/examples/hardware-tests/src/voice_probe.rs:190,266<br>**hk-psx** game/src/ambience.rs:103,258; game/src/audio.rs:64,78,97; game/src/audio_stream.rs:275; game/src/focus_audio.rs:42; game/src/geo_audio.rs:43; game/src/runner_audio.rs:40; game/src/scene_sfx.rs:64<br>**oot-psx** game/src/audio.rs:161; game/src/music.rs:222<br>**quake-psx** game/src/audio.rs:772,792<br>**voxide** game/src/sfx.rs:117,499,648<br>**wipeout-psx** game/src/audio.rs:159,655 |

## psx-vram

| Item | Instead | Callers |
| --- | --- | --- |
| `psx_vram::Clut::uv_clut_word` | renamed to `uv_word` | **nitroxide** game/src/draw.rs:1600,1646,1650,1654,1664,1667,1675,8050,8055 |
| `psx_vram::TexDepth` | renamed to `TextureDepth` | **cs-psx** game/src/hltext.rs:7,150; game/src/hud.rs:12,209,212; game/src/menu.rs:25,27,262,282,353,354,564,691; game/src/vram.rs:19,380<br>**hk-psx** game/src/blocker_roller.rs:154,171; game/src/dialogue.rs:3,113; game/src/game_map.rs:28,150; game/src/hero_light.rs:21,110; game/src/hud.rs:4,34,66; game/src/menu.rs:6,38,60,184; game/src/render.rs:6,331,881,905; game/src/shaman.rs:178,225; game/src/title_card.rs:13,97<br>**hl-psx** game/src/hltext.rs:7,151; game/src/hud.rs:18,171; game/src/menu.rs:25,27,362,383,455,456,666,793; game/src/vram.rs:19,348<br>**nitroxide** game/src/draw.rs:39,1571,1572,1573; game/src/main.rs:28,43,46,52<br>**oot-psx** game/src/font.rs:53,64; game/src/hud.rs:18,134; game/src/skybox.rs:27,69; game/src/title.rs:52,662,664; game/src/vram.rs:30,124,125,127,131,369,379,424,439<br>**psxcel** game/src/main.rs:42,47<br>**quake-psx** game/src/intro.rs:13,15,17<br>**voxide** game/src/main.rs:78,1005,4305; game/src/tex.rs:10,3157 |
| `psx_vram::TexturePage::uv_tpage_word` | renamed to `uv_word` | **nitroxide** game/src/draw.rs:1600,1639,1646,1650,1654,1664,1667,1675,8050,8055 |
| `psx_vram::Tpage` | renamed to `TexturePage` | **cs-psx** game/src/hltext.rs:7,150; game/src/hud.rs:12,209,212; game/src/menu.rs:25,27,262,282,353,354,564,691; game/src/vram.rs:19,380<br>**hk-psx** game/src/blocker_roller.rs:154,171; game/src/dialogue.rs:3,113; game/src/game_map.rs:28,150; game/src/hero_light.rs:21,110; game/src/hud.rs:4,34,66; game/src/menu.rs:6,38,60,184; game/src/render.rs:6,331,739,881,905; game/src/shaman.rs:178,225; game/src/title_card.rs:13,97<br>**hl-psx** game/src/hltext.rs:7,151; game/src/hud.rs:18,171; game/src/menu.rs:25,27,362,383,455,456,666,793; game/src/vram.rs:19,348<br>**nitroxide** game/src/draw.rs:39,1571,1572,1573; game/src/main.rs:28,43,46,52<br>**oot-psx** game/src/font.rs:53,64; game/src/hud.rs:18,134; game/src/skybox.rs:27,69; game/src/title.rs:52,666; game/src/vram.rs:30,372,431<br>**psxcel** game/src/main.rs:42,47<br>**quake-psx** game/src/intro.rs:13,15,17<br>**voxide** game/src/main.rs:78,1005,4305; game/src/tex.rs:10,3157 |
