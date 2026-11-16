# Changelog

## Unreleased

- `psx_asset::le::read_u16_unchecked` and `read_u32_unchecked` are indexed byte loads joined by `from_le_bytes` instead of `read_unaligned`: same `lwl`/`lwr` loads, but a game that adopts them in place of its own byte readers keeps its inlining (measured in Counter-Strike's `map.rs`: the `read_unaligned` form cost 3,216 B of free RAM, this form gains 2,752 B).
- The little-endian readers moved to `psx_math::le` (a crate with no dependencies, so `psx-io`, `psx-pack`, `psx-mc`, `psx-seq` and `psx-settings` use them); `psx_asset::le` is a re-export, so every existing path still resolves, but the functions are now `psx_math::le::*` in a link map and a PGO profile that names `psx_asset::le::read_u32` must be rebound (`psoxide-pgo rebind --rename`). New `get_u16`, `get_u32`, `get_i16`, `get_i32` return `None` past the end (one length test, then the same indexed byte loads). The SDK's own private readers in `psx-bsp`, `psx-io::iso`, `psx-pack`, `psx-mc`, `psx-seq`, `psx-settings`, `psx-asset::hmd8`, `psx-fmv` (stream, bitstream), `psx-level` and `psx-game-runtime` (world pack) call them; `psx-asset`'s pointer readers (`hma1`, `tracks`) and its inline `from_le_bytes` sites keep their shape on purpose.
- `psx_pack::WordFnv32`: the word-wise FNV hash (`(hash ^ word) * 0x0100_0193`, one round a word) Hollow Knight's module transport, Ocarina and WipEout hash with, as assembly like `Fnv32` (the multiply as `x * 403` on the multiplier plus `x << 24`, the next word fetched in its wait), with `update`, `update_raw` (a raw pointer, each word read once) and `push`. `psx_residency::arena::checksum` is `WordFnv32::of`. `psx_pack::fnv1a32_const` and `fnv1a32_const_from` are the same byte FNV-1a for `const` contexts; `psx-level`'s `StableId`, `psx-grid`'s layout and mover hashes and its toolchain probe, `psx-iso`'s stored checksums, `psx-residency-sim`, the `cd-stream-benchmark` checksums of `psx-game-runtime` and `hello-memcard` use them. `psx-iso`, `psx-level`, `psx-grid` and `psx-game-runtime` gain a `psx-pack` dependency.
- `psx_game_runtime::save` computes its FNV-1a-64 frame checksum with `psx_math::WideU` instead of `psx_hw::hash::fnv1a_64`, so the guest does no 64-bit arithmetic of its own; the stored eight bytes are unchanged (a test holds them equal to `fnv1a_64(..).to_le_bytes()` over every length).
- `psx_fx::Xorshift32`: Marsaglia's 32-bit xorshift (13, 17, 5) with a bare-state `step` like `NrLcg::step`, for the five guest copies NitroXide, VoXide's and the launcher's playlist and Celeste's synth wrote out.
- `SectorReader::read_at(lba, buffer)` (start, read one sector, stop) and `SectorReader::find(sector, from, path)`, and `psx_io::iso::find_in_root(sector, read, names, out)` (the volume descriptor, then up to four root sectors, every name looked up in one pass), replace the read closure pasted into nine programs; `psx-fmv`'s player, the SDK examples and the hardware test use them.
- `psx_hw::gpu::pack_*` are the one GP0 word packers: `psx_goldsrc::fx::gp0_vertex`, `gp0_xy`, `gp0_color` and `gp0_texcoord` are re-exports of `pack_vertex`, `pack_xy`, `pack_color` and `pack_texcoord`, and the inline `x as u16 as u32 | y as u16 as u32 << 16` in `psx-bsp`, `psx-grid`, `psx-engine`, `psx-vector`, `psx-goldsrc`, `psx-asset` and `psx-loading` call `pack_vertex`. `psx-grid`, `psx-vector` and `psx-loading` gain a `psx-hw` dependency.
- `psx_hw::hash::crc32` and `Crc32` (IEEE 802.3, a 16-entry table, two steps a byte): the one CRC-32 for the SDK's crates; `psx-vector-cook`'s PNG writer uses it. The hardware-test payload frame and the standalone `clut-probe-report` keep their own on purpose (a measuring tool does not link the code it measures).
- `psx_math::fmt::u32_hex(buf, v, digits)` and the decimal writers replace the private digit loops of `psx-rt` (panic handler, crash screen, `tty::print_hex_u32`), `psx-io`'s `diag::step_hex` and `psx-engine`'s micro-game score. `psx-rt` gains a `psx-math` dependency.
- `psx_asset` decodes a quaternion to a matrix in one place (`quat::to_matrix`) for both `hma1` and `tracks` keys.
- `psx_hw::adpcm` holds the SPU-ADPCM five-filter table, the prediction and nibble rounding and `quantise_block` (the closed-loop quantiser of one 28-sample block, `no_std`, 32-bit, the error reported through a callback); `psx-audio-cook`'s encoder and decoder, its XA encoder and `psxed-audio` use the one table, and `psx-audio-cook`'s output is byte for byte what it was.
- `psx_math::color::scale_rgb_q8(rgb, num)`: each channel times `num / 256` as a shift, no divide and no clamp, the body the launcher's and the loader's `scale` wrote by hand (`scale_rgb` is the signed, clamping form and costs about 6 KB in a program with sixty call sites). It is out of line (one body of about 100 B shared by every call): in the demo disc launcher (about sixty call sites) the text is 2,580 B smaller than with the body inlined, and NitroXide's `fx::dim` is 272 B smaller than with `scale_rgb`.
- `psx_chainloader::paint::gp0_packet` and `display_buffer` are public, and `paint::set_draw_rect(y, x0, y0, w, h)` aims drawing at a buffer row with a clip rectangle in one packet, so the demo disc's loader can drop its own `send`, `display` and `target`. The chain loader's own functions are unchanged.
- `psx_display::Brightness`: the darker step is the brighter one (8 grey, one blend unit) for every program; the doc says what Hollow Knight and WipEout, which drew their own overlay with 6 down, see when they adopt it. Doc fixes: the `psx_math::ease` header, the `psx_io::controller_port` example (it called the retired `PadReader::poll`), `DEPRECATED-REMAINING.md` (the present-queue and CD forwarders were missing), and a note in the two scripts that still pass `-disable-mips-df-backward-search` on why (they link without the hazard patcher).
- `psx_hw::hash::Fnv1a64` and `fnv1a_64` exist only off the console (`cfg(not(target_arch = "mips"))`): a guest that calls them fails to compile instead of linking a 64-bit multiply the numeric guard cannot see. A guest folds the same hash with `psx_math::WideU::fnv1a_step` (as `psx-game-runtime`'s save frame does). `crc32` and `Crc32` stay on both.
- `psx_math::color::scale_rgb_q8_inline`: `scale_rgb_q8` always inlined, for a hot per-frame caller with few sites (NitroXide's HUD dim lost 5 flips in its heavy window through the call); the call form stays for many-site and cold callers (the doc says which).
- `psx_engine::sky::fill_layered_sky_samples::<W, H>`: the layered-sky texel lattice for a fixed screen with the corners unrolled and the per-corner calls inlined, for a renderer that keeps its own lattice cache (Quake's rolled loop over the out-of-line functions cost +0.117% of its bench); `screen_view_ray` is `#[inline]`. A test holds it equal to the per-corner calls.
- `psoxide.ld` hooks, all inert when unset (every SDK example executable is byte-identical): `RAM_SIZE_OVERRIDE` (a script placed before it, as for `STACK_RESERVE_OVERRIDE`; the map records it and psoxide-size reads it; the stack starts 100h below the top of RAM), `__initial_pc` for the header's initial PC, and `psoxide-boot.ld` (boot-only memory: `-T psoxide-boot.ld -T psoxide.ld`, `BOOT_VMA`, `BOOT_LMA`, a program-defined `__boot_entry`) with `__load_end` and `__heap_start = __bss_end` so an overlay INSERTed after .bss works. The restored games' psyq.ld can derive from it.
- `psx_io::iso::read_root_sector(sector, read)`: the volume descriptor and the first root-directory sector into the caller's buffer, for a caller that walks the entries itself with `find_in_directory` (a size check on each hit, its own failure policy). The demo disc launcher's `locate_files` and NitroXide's `locate` convert at zero bytes (the launcher executable is byte-identical, NitroXide's `enter_flow_state` is 56 B smaller); `find_in_root` and `SectorReader::find` stay for the callers that want the whole walk.
- `psx_hw::gpu::gp1::display_mode_in_range` and `display_mode_hres2_in_range`: the mode word without the argument masks (the caller keeps `hres_1` below 4 and `vres` below 2; a debug assertion only), like the other `_in_range` builders; `display_mode` and `display_mode_hres2` are unchanged.
- `psx_mc::dir::checksum_inline`: `dir::checksum` always inlined, for a caller whose own loop is inline today; `checksum` is unchanged.
- `Fnv32::push_shared` and `WordFnv32::push_shared`: `push` as a call. `push` inlines about 36 B at every site (right for a loop that maps each value first); a caller that hashes a few scalars at many sites uses the call (eight scalars at one site: 324 B inlined, 232 B as calls) or collects them and calls `update`. Hollow Knight's atlas fingerprint (+524 B inlined) and Ocarina's `proxy_key` (+384 B) are those callers. `push` itself is unchanged.
- `psoxide-pgo rebind` aligns a function edit above 16M table cells in linear memory (Hirschberg) instead of refusing it, up to 2G cells; edits under the old cap take the same path as before and give the same output. Half-Life's `play` (4,298 lines) carries in one step.
- `psx_hw::gpu::gp1::display_mode_hres2(hres_1, vres, pal, depth_24, interlace)` is `display_mode`'s word with bit 6 (the 368-wide mode) set; `display_mode` is unchanged.
- `psx_mc::dir` (the directory-entry offsets `E_*`, the allocation states `ST_*`, `LINK_NONE`, `is_free` and the XOR `checksum`) and `Card::read_dir`, `Card::find`, `Card::chain` (with `psx_mc::Chain`) are public, so a program that reads a card's directory itself (the restored games' BIOS-style layer) calls the one implementation. Same code: the psx-mc object is the same size.
- The checked `psx_math::le::read_u16`, `read_u32` and their `_inline` forms read through one unaligned load again (the shape they had before the byte-load change to the unchecked forms): Ocarina's header parsers (`Skm::new`, `Prm::new`, `Col::parse`, `TitleCam::new`) are back to their size at the previous pin, and `psx_pack::WordFnv32::of` carries the kernel inline so `psx_residency::arena::checksum` is the size it was.
- `psx_math::le` gains `read_u16_inline`/`read_u32_inline` (checked, always inlined) and `read_u16_ptr`/`read_u32_ptr` (unchecked from a base pointer), the shapes the private readers of psx-pack, psx-mc, psx-fmv and `psx_asset::hmd8` had; `get_*` is the `bytes.get(o..o+n)` form.
- `psx_pack::Fnv32::update` and `update_bulk` multiply by the FNV prime as `x * 403` on the multiplier (6 cycles with the constant as the first operand, 13 for the whole prime) plus `x << 24`, and fetch the next byte or word while it runs, instead of eight shifts and adds. Same hash for every input and split. Measured in the emulator on a guest at the games' flags, 1 KiB aligned: `update_bulk` 13,769 to 11,458 cycles (13.4 to 11.2 a byte, below Hollow Knight's old word-fetching copy at 13.4), `update` 19,599 to 13,581 (19.1 to 13.3, below the old byte-loop copies at 26.1); instructions a byte 12 to 7.5 (bulk) and 14 to 9 (update). `update` stays 80 bytes, `update_bulk` goes 344 to 300. `hello-fnv` (a guest example, `FNV PASS` on the TTY) runs both against plain FNV-1a over every length 0..=67 at every alignment with every two- and three-way split, since the host tests build the portable form only.
- `hazard-scan` and `hazard-patch` read the two image shapes psoxide.ld's builds do not make, so no program has to stay on `-disable-mips-df-backward-search` because the SDK's tools cannot read its image. A headerless blob (the demo disc's chain loader, which has no psx-rt) is `--raw LOAD [--entry ADDR]`: the tools read it as a PS-EXE with a made-up header, its code is the span of the map's `.text` input sections (no `__text_start` is needed; without a map the whole blob is scanned as under `--whole-image`), and `hazard-patch --raw` only checks, never writes (a blob has no `HAZARD_TRAMPOLINES` array, so a hazard fails the build and the compiler's delay-slot flags are what keep it clean). A program linked with a boot overlay (psyq-boot.ld: boot-only code stored after `.data`, run from the start of the heap) needs no flag: the map's `.boot` section and `__boot_start`/`__boot_load_end` make the overlay a module at its run address, its `.text` input sections are code, its hazards are rerouted through the resident array and written back where the bytes are stored, and the map check takes the entry from `__boot_entry` and the stored size from the end of the overlay's stored bytes. Images of psoxide.ld's shape are untouched: on Quake and NitroXide with 25 planted hazards each, the old and new tools write the same bytes and print the same output. Host tests in `tests/shapes.rs` build a clean and a hazardous image of each shape.
- `psoxide-pgo rebind` takes `--rename OLD=NEW` (repeatable, portable names as `bind` prints them): a profile function that moved to another module or crate binds to its new path. A profile names a callee by path, and a sample-profile inline hint is lost when the path changed (psx_asset's readers moving into `psx_asset::le` stopped inlining in one game until the profile was renamed).
- `psx_telemetry::receipts!` and the `receipts` feature (a `[diag]` tap in `sdk/profile.toml`): the one place a game puts the result receipts a headless journey reads back from its RAM (a match snapshot, a settings block, a music state; their `#[no_mangle]` statics and the stores that fill them). Wrapped in the macro they are compiled and linked only in a build with the feature, so a shipping image holds none of them and an image that has them is a diagnostic build (`mkisopsx` refuses it without `--diag`). `psx_telemetry::RECEIPTS` is the same switch as a constant. A program that does not use it links byte for byte the same.
- Shared pieces for games to delete their own copies, all additions (no existing game's executable changes): `psx_hw::memory::to_kseg0`; `psx_pack::Fnv32` (resumable FNV-1a 32, equal to `fnv1a32` in pieces); `psx_asset::le` (the little-endian readers `lib.rs` kept private); `Vec3I32::{saturating_add, saturating_sub, dot_q12, scale_q12}` in psx-gte-core; `psx_math::ease` (Q8 and Q12 curves and `lerp_q8`) and `fmt::{u32_dec_padded, clock_dec, tenths_dec}`; `psx_gpu::prim::{TriFlatBlended, QuadFlatBlended, TriGouraudBlended, DrawArea, ClutLoad16}` and `RectFlat::translucent`. Each has a host test proving equality with the games' hand-written versions over their input ranges.
- `psoxide.ld` reads `STACK_RESERVE_OVERRIDE` when a link defines it (a script placed before it, or `--defsym`); with nothing set the reserve is the same 0x8000. `psoxide-size` evaluates the form from the link map.
- `psx-chainloader`'s `paint` builds its GP1 and GP0 words from `psx_hw::gpu` (the crate's compiled object is identical).
- `psoxide-pgo bind PROFILE ELF` and `rebind` print how much of a sample profile finds a symbol in the build (samples and functions, the ten largest unbound keys) before anything else, under normalised names, and fail below 90% (`--allow-unbound`, `PSOXIDE_PGO_ALLOW_UNBOUND=1`, `--min-bound`). A profile written by the plain converter spells every crate's disambiguator, so after an SDK change that re-hashes a game's crates only 25.7% to 28.1% of the samples of two older profiles of one game had a symbol (99.1% for the one made at the current hashes); normalised, 98.6% to 99.1%. `collect` already wrote portable names and `rebind` already read raw ones, so a game on `apply` loses nothing; a game that hands the converter's file straight to `-Zprofile-sample-use` must `rebind` it first (README, "Names are the key"). `rebind` writes the same file as before. Two symbols that share a portable key are never merged: both count as unbound and `bind` names them.
- One RAM measure. `psoxide-pgo` takes free RAM and the "fits" comparison from `psoxide_size::ram` (`free_ram`, `fits`) instead of its own copy, and measures the map a `--link` hook leaves in preference to a `free N` the hook printed (warning when they differ; a game's own report has over-reported by 256 B). `psoxide-pgo ladder --margin BYTES` (default 0) asks for the floor plus a margin; exactly at the requirement fits, one byte under does not.
- `psoxide-hwtest scan-expansion2` tracks register constants over the control-flow graph of the image instead of down the instruction stream. A constant now survives only where every path reaching a use agrees on it: it dies at function entry (a `jal` target, or a code address held as data), for caller-saved registers after a call's delay slot, at a join of paths that disagree, and a function reached by no path (or only through a pointer the code forms, a callback or handler) is its own entry with nothing known, scanned with the constants it forms itself. Before, an I/O-base `lui` left in an argument register by one function leaked into the next function's callee-saved copy of its own argument, and 81 loads and stores through an ordinary struct pointer were reported as Expansion 2 accesses. A real debug-console store (the `host-tty` diagnostic builds) still fails.
- One lookup of a file on the disc, in `psx-io` (`psx_io::iso`), replacing `psx_fmv::iso` and the copies of it. `root_directory` and `find_in_directory` read the records out of a sector the caller has read (plain functions: a program with one call site gets them inlined, one with several gets one body), and `find(sector, read, from, path)` is the loop around them for a program that hands over a sector read and a path, over every sector of every directory down the path. File names are upper case as ISO 9660 writes them and compare byte for byte up to the disc's `;version` (the old lookup also folded case); a name that only starts with the wanted name does not match; a record that ends exactly at the end of a sector is found; a damaged record cannot read past the sector. The name compare is a loop with the name's length hidden from the optimiser (an empty assembly block), so the function is a leaf with no call and no registers saved, and the compiler neither unrolls it for a constant name nor specialises it for names of one length. `SectorReader::read_one(lba, buffer)` is start_read, read_sector and stop for the one-sector read every lookup makes. `psx_fmv::player::find_root_file` and the seven examples that carried their own read-and-lookup glue use it. Measured against what each program linked before, at the same pin and flags: Hollow Knight (the hand-written lookup it carries) 815,584 bytes of text and 15,208 free below the module pool become 815,576 and 15,208; NitroXide 600,808 text and 112,792 free RAM become 600,812 and 112,792 (its own call of the old `psx_fmv::iso::find_in_directory`, 216 bytes out of line, becomes the new one, also 216). The previous `psx_fmv::iso` cost Hollow Knight 44 bytes of text and, because `.data` is 128-byte aligned, 128 bytes of free RAM.
- `psx_pad::stick_bits::<THRESHOLD>` tests each axis with `else if` instead of two independent tests (a threshold below 0 is now a compile error; it was documented as at least 0). The results are the same, checked by a host test over all 65,536 stick byte pairs at every threshold from 0 to 127 against both the plain per-axis rule and the byte-level rule a game writes without centring. The independent form made a platformer collection 364 bytes larger when it adopted the fold (its three inlined call sites grew); the `else if` form is 220 bytes smaller than that game's own hand-written fold, measured on its real build: text 259348 to 259128 and free RAM 1376892 to 1377116 (every one of the four changed functions shrinks).
- The console's direct-access guard is linked only by a program that can arm it, whatever its LTO setting. The by-name call to the guard (`__psx_rt_direct_access_guard`) made every program that links the GPU write paths link `psx_rt::present::wait_idle`, the walk stop it calls, the draw-done signal and its list (1,084 bytes of symbols; one thin-LTO game lost 1,220 bytes of text and 1,268 bytes of free RAM from the pin alone), because with thin LTO the armed flag is read inline in other crates and the compiler cannot prove it false. Now `run_armed_guard` calls `__psx_direct_access_guard`, which `sdk/psoxide.ld` defines as the address of `wait_idle` when the function is in the image (it sits in its own `.text.__psx_rt_wait_idle` window) and 0 otherwise, and `arm_direct_access_guard` carries an `R_MIPS_NONE` relocation against `__psx_rt_wait_idle`, so every arm keeps it. The call is still direct, so `tools/guard_stack_gate.sh` passes with one change: it looks for the symbol `__psx_rt_wait_idle` (the function's symbol name on the console) instead of `psx_rt::present::wait_idle`. Measured on a thin-LTO game that never arms: text 20 bytes smaller and data, bss and free RAM equal to or better than the SDK before the by-name call (`run_armed_guard` 60 bytes against 80, one pointer gone). Programs that arm (`hello-present`, `hello-present-queue`, `hello-present-cd`, `hello-flipcap`): the same size to the byte, the same instructions apart from two functions whose scheduling moved; `__psx_rt_wait_idle` is the old `wait_idle` renamed and moved to the front of `.text`. A program that links with a script that replaces `psoxide.ld` must add the window and the `__psx_direct_access_guard` definition if it can arm the guard (a link error names the symbol otherwise; the exact lines are in `sdk/docs/PRESENT.md`, "A program with its own linker script"); derived copies of `psoxide.ld` and extra overlay scripts need nothing. The overlay packer ignores `R_MIPS_NONE` relocations, and the placement search knows the window as a fixed placement (a test reads it from the shipped script). `sdk/docs/PRESENT.md` has the mechanism and why a weak definition, a `PROVIDE` default or compile-time folding do not do it.
- `psx_math::Mask64` gains `trailing_zeros()`, `leading_zeros()` and `count_ones()` (64, 64 and 0 for the empty set, like `u64`), the bit queries a 64-row mesher runs on its column masks. Measured on mipsel against the `u64` methods: `trailing_zeros` 116 bytes, 29 instructions, the same as `u64::trailing_zeros`; `leading_zeros` 208 bytes against 276; `count_ones` 168 against 168. Host tests check them against `u64` on every single-bit, prefix and suffix word and 500 random words. A shift by a signed amount is not a separate operation: it is `if n >= 0 { m.shl_by(n) } else { m.shr_by(-n) }`, which is what the caller's own branch already is. A program that does not call them links byte for byte the same.
- `psoxide-hwtest scan-expansion2` tells a data word that code can reach from one that only looks like an Expansion 2 address. A program with embedded asset bytes (a cooked texture or sound blob) holds words that look like such an address by chance, every named port was "not referenced", and the scan still failed, which gets a guard of a real console fault ignored. A loaded or stored address is judged exactly as before; a data word now fails only when some code forms its own address or the value it holds (`lui` + `addiu`/`ori`, or the target of a load), and otherwise it is listed as a note and the scan passes. `--map LINK.MAP` classifies by input section instead: a word fails when its section holds pointers (alignment 4 or more) and code forms an address inside the section, and a byte-aligned blob never does. `--strict` is the old behaviour (every data word fails) and `make hardware-tests-disc` uses it. Without a map a pointer table read by index is not proven, and the note says so. A diagnostic build that stores to the debug console still fails, with and without `--strict`.
- `psx_vram::ClutRowAllocator` keeps its per-row slot occupancy in `psx_math::Mask64` instead of a `u64`: five `psx-numeric-allow` comments go, and the code is smaller. Measured on mipsel for an 8-row allocator behind non-inlined wrappers: `alloc` 1164 bytes against 1420 (291 instructions against 355; the scan step of one row is 16 instructions against 19) and `free` 332 against 432. On the real program that links it (editor-playtest) `free` is 40 bytes smaller, `init_runtime_arenas` 36 bytes larger (a one-time boot cost) and nothing else in the image changes, so .text is 4 bytes smaller; that needs the struct to keep its 8-byte alignment (`#[repr(align(8))]`), without which the boot copy of the allocator grows by 224 bytes and the image by 188. Behaviour is unchanged (the host tests of the allocator pass). `psx-vram` now depends on `psx-math`. The other `1u64 << n` mask sites (`psx-game-runtime`, `psx-engine`, the editor-playtest example) are not moved yet.
- `psx_math::Mask64` gains `shl_by(n)`, `shr_by(n)` (variable shifts; the count is taken modulo 64 like `WideU::shl_by`) and `range(lo, hi)` (the bits `lo..hi`), the three operations a 64-row column bit set needed beyond `bit` and `first`, so `1u64 << n`, `bits << y0` and `((1 << n) - 1) << y0` no longer need a `u64` and its numeric-guard exception. Measured on mipsel against the bare `u64` forms: `shl_by` 52 bytes and 11 instructions on the `n < 32` path against 76 and 15 for `u64 << n`; `shr_by` 52 and 11 against 56 and 12; `range` 140 bytes, the same as `((1u64 << c) - 1) << lo`, and `Mask64::first(c).shl_by(lo)` (the width form) 52 against 72 for a constant width. Host tests check every shift count (0 to 129) over random and edge words against `u64`, and every `range(lo, hi)` pair up to 80 against a `u128` oracle. A program that does not call them links byte for byte the same.
- `psx_math::WideU::fnv1a_step(byte)` and `WideU::FNV1A_OFFSET`: the 64-bit FNV-1a hash as a shared step, so a program that checks a 64-bit identity hash over a file format shared with its host cooker needs no `u64` and no numeric-guard exception of its own. It compiles to the bare `u64` loop (`(h ^ byte) * 0x100000001b3`): on mipsel the per-byte loop is 19 instructions with one `multu`, and a function that hashes a slice is 92 bytes, the same instruction stream as the native `u64` code and less than half of the same loop written with `WideU` shifts and adds (172 bytes, 39 instructions a byte). Host tests check the published vectors and random data against a `u64` reference. A program that does not call it links byte for byte the same.
- `psx_pad`: the stick fold and the edge tracker are each as small as the hand-written code they replace (adopting both in a first-person game costs zero bytes of text and of free RAM, measured on its real build; the landed `StickDpad` cost 420 to 800 bytes in two games). `stick_bits::<THRESHOLD>(x, y)` is a pure const-generic per-axis fold; `RadialStick<ENTER, EXIT>` is the hysteresis fold (`StickDpad` and `StickShape` are gone); `PadEdges` keeps one word and `update` returns a `PadFrame` (held, previous, pressed, released, `just_pressed`, `just_released`, `group_pressed`, `PadFrame::new` for a caller with its own previous state). `PadTracker` is unchanged and its generated code is identical.
- `runtime-numeric-guard` is as strict as the scanner games run: a wide type written as a literal suffix (`1u64`, `0_i128`) is a violation too. Seventeen sites in SDK and engine guest code were missed; each is a bit select or mask on a 64-bit value that a neighbouring line already carries a reviewed exception for (or const-evaluated code), and now has its own `psx-numeric-allow-next-line` marker with the reason. No code changed.
- `psx_telemetry::emit::console` is a measurement-build write like the rest of `emit` (it is `debug_log`). It was compiled into every mips build and stored to the debug-log port (inside Expansion 2, beyond the entry state's 128 bytes), which faults on a console. A default build that calls it is byte-identical to one that does not and `psoxide-hwtest scan-expansion2` finds no store; a build with `emit` has them. No SDK or engine code called it; psx-goldsrc keeps its own unconditional copy (`psx_goldsrc::telemetry::console`).
- Host tools and small shared pieces that games carried copies of, with nothing changed for a game that does not call them (a guest that does not call a new function links byte for byte the same):
  `psoxide-dev no64-scan [--floats] [--exempt FILE]... DIR...` runs the runtime numeric scanner on a program's own directories
  (the scanner five games copied; it judges a wide type written as a literal suffix, `1u64`, which `runtime-numeric-guard`
  does not yet); `psoxide-size ram MAP [--items] [--floor BYTES|FILE]` reports the free RAM a link leaves and where the rest
  went, and `psoxide-size block EXE MAGIC:VERSION:LEN:NAME...` finds a versioned state block in an executable (the
  `ram_report` and `gate-symbols` tools games carried); `psoxide-dev style-fmt [--check]` formats a program's tracked Rust.
  `psx_telemetry::emit` gains `debug_write` (a log write that does not end the line) and `sort_class` (the depth-sort probe's
  class word, port `+0x28`), under the same `emit` feature as every other writer, so a game's own writes to those ports can go;
  editor-playtest uses `sort_class` and loses a register-literal exception. `psx_pad::StickDpad` folds the left stick into
  d-pad bits, per axis or radial, with the entry and exit thresholds as parameters, for `PadTracker`, which gains `previous()` (last frame's mask) and `settle()` (previous state := current at a resume point, held buttons stay held and are not hidden, unlike `prime`).
- The architecture gate refuses an image that holds a frame-path function as a function: `[inline]` in `sdk/profile.toml`
  lists them (`psx_gpu::chain::start_walk` first) and `inline-gate` (`tools/psoxide-hazard`, run by `psoxide-pgo` with the
  hazard patch, the stack guard and the cache gate) reads the link map after the link. The compiler's inline choice depends on
  the size of the program, so the examples never showed the call, return and instruction-cache cost one game paid on every
  chain submit (thirteen late frames in a test journey against six; `sdk/docs/GATE.md`, "Inlined frame-path functions"). Tests:
  a map with the symbol is refused and one without it passes; the profile test fails on an entry that names nothing.
- `psx_gpu::chain::start_walk`, the kick behind every chain submit (`submit_async_raw`, `Frame::submit`, `OtFrame::submit_with`,
  `Gpu::submit_static`, the compatibility entry points), is `inline(always)`. It was left to the compiler, which inlined it in
  small programs and, in a larger one, compiled it as a function: a call and return, a second copy of its instructions in
  the instruction cache and its loads and stores around them on every submit. The body did not get dearer (an out-of-line copy
  executes 19.0 instructions on SDK `46f40d507373` and 19.5 on `6d1cb39fb425`, the difference being the direct-access guard
  tests every GPU DMA start carries since the present path became part of every build), so the examples (`hello-splash`,
  `hello-ot` with one and with two submit sites, `hello-cddrain`) link the same code with and without the attribute and
  nothing in them changes; a program that never submits a chain still does not link the chain path.
- Hardware tests v2.20 (not yet run on a console; gated on the emulator): the linear run asks nobody anything (the DualShock motor
  questions are the menu row `DUALSHOCK MOTOR QUESTIONS`, and the linear run writes their records as unanswered, every id kept);
  step `GTE NCLIP VARIANTS` (`0x980`-`0x9AF`, INFO) runs the sequences of conformance cases 22 and 116 to 120, which fail on the
  console since 2026-11-14, as built, after filling every GTE register, with one more nop and after zeroing them, keeping the raw MAC0; the XA clock
  step adds `0x330`-`0x338`, raw head and clock readings, the time of each `Player::poll` and its counts, fields that cannot
  clamp; companions `0x653`-`0x654` and `0x874`-`0x87B` carry the fields that clamp at `0xFFFF`. No existing record id changes.
  The machine-code baseline for v2.20 is pinned (drift 0 against v2.19) and `docs/hardware-refs/px8-emulator-v2.20.txt` is the
  emulator baseline. Details in `docs/hardware-test-versions.md`.
- The first console run of the pad and present work, 2026-11-14 (SCPH-9002; hardware-tests v2.18; two runs, an SCPH-1200 and an
  SCPH-110, both DualShocks; the QR pages are `docs/hardware-refs/px8-silicon-2026-11-14-v2.18-run1-scph1200.txt` and
  `...-run2-scph110.txt`, now read by `psoxide-hwtest compare`). Passed, identical on both runs: the early select (`0x6F0`-`0x6F4`), the
  stop returning at the acknowledge (`0x6F8`-`0x6FB`), the analog handshake with its interrupt masking (`0x6F5`-`0x6F7`), a DualShock read
  from its first poll (`0x6FC`-`0x6FE`) and the present path (`0x970`-`0x976`). The numbers are in the entries below and in
  `docs/hardware-test-versions.md`. Two corrections to what the SDK said before: the masking of interrupts around a configuration
  packet is required (an unmasked interrupt over the `/ACK` loses the packet on a console), not insurance; and an SCPH-1200 and an
  SCPH-110 both answer as DualShocks (ID `0x73`) once the handshake has run. Open, cause not determined: conformance cases 22 and
  116 to 120 passed on every earlier console run and fail on both of these (they stay failing cases); the XA clock step reads the
  clamp floor on every console run and is not a measurement; nine fields clamp at `0xFFFF`. Details in `docs/hardware-test-versions.md`.
- `psx_pad::Controllers` (both `new()` and `digital()`) reads a pad that is slow to come up from its first answer. A port that
  answers "nobody there" was left alone for 59 polls and probed once in 60, so a pad that needed more than a moment was ignored
  for two seconds. PCSX-ReARMed's DualShock, in either mode, acknowledges the address byte and answers `0xFF` to the command
  byte (what an empty socket does on that port) for its first 33 video frames (31 to 33 whatever the polls a frame): a game on `Controllers::digital()` first saw a held
  button at its 124th frame there, a plain digital pad at its 20th, and the earlier poll at its 33rd. The port is now polled
  every time for its first 128 such answers (and for 128 after a pad leaves), then as before. A plain pad that answers the first
  poll never reaches this code; an empty port 2 pays 128 failed polls once (about 20,000 cycles each in the emulator). The
  PSoXide emulator's DualShock answers from the first poll and cannot show the bug. Hardware-tests v2.16 adds the console's
  answer (`PAD DIGITAL FIRST POLL`, records `0x6FC`-`0x6FE`). The same suite version makes `SIO0 SELECT EARLY` send its bytes with CPU interrupts masked, as `Controllers` does for a configuration packet, so a VBlank over a pad's `/ACK` cannot fail a poll of that step (medians of `0x6F0`-`0x6F4` move by 11 to 35 cycles).
- The boot splash costs a program less. The mark is stored LZ coded in 16-bit words (2,746 bytes against 4,692; 512-word
  window, a flag byte per eight tokens) and expanded sixteen rows at a time through a 1 KiB buffer, with the same 4,096 words
  out (a host test pins the FNV-1a of them); `FontAtlas::upload` is out of line, so every caller shares one copy; the
  palette is read in place instead of copied and patched on the stack; and the line advances by the font's advance width
  instead of calling `glyph_advance` for every letter. `hello-splash` (the splash at every brightness step, then another
  line, one call site): text 14,252 -> 15,472, rodata 5,814 -> 3,902 bytes (1,912 fewer in all after the decoder's growth,
  and a program that already links the splash's font or upload code gains more: NitroXide, Celeste and VoXide free 2,512,
  1,776 and 1,568 bytes of RAM); the 2,494 route ticks of the whole run and the 2,091 of a run with three skip presses are
  pixel-identical, and the setup before the first frame takes 252,770 cycles against 365,165. The splash now waits with `psx_rt::interrupts::try_wait_vblank`, which does
  not install the VBlank counter (`install_vblank_counter` reaches the I-cache flush): the caller installs it, a program
  that has not asserts in a debug build and gives up after the wait's bound in a release build. `tools/splash_gate.sh`
  (run by `make lint`) walks the tree of every `psx_splash::show` with `stack-guard --forbid __psx_rt_flush_i_cache`, so a
  program that runs its title code from an overlay can prove its call tree clean. psx-rt gets a host `try_wait_vblank`.
- `psx-engine`: `Scene::boot_splash` is the one place a game asks for the boot splash. It gets the controller port (a game
  that keeps the player's brightness on the card reads it there) and returns `Some(BootSplash { line, brightness })`;
  `App::run` plays `psx_splash::show` before the scene is initialised, on a GPU and buffers it then sets up afresh, so
  nothing of the splash reaches the scene. The default is `None`: a scene that does not ask links none of the splash, and
  `hello-engine` is byte-identical with and without this change. A game no longer steals the peripherals to show the
  splash before the engine starts.

No program depends on a BIOS any more, not a call into one and not the state one leaves behind (Manny's rule:
every program works on every Sony BIOS and on OpenBIOS).

- One present path for every program (`psx_rt::present`, `sdk/docs/PRESENT.md`), with no cargo feature: the
  VBlank handler, the command recording and the direct-access guard are part of every build, and the
  `present-queue` features of psx-io, psx-rt and psx-engine no longer do anything (they stay so an old manifest
  resolves). A frame is a DMA chain (`Presenter::submit`, in one part or several: `Part::More` then `Part::Final`)
  or drawing through the GP0 port (`Presenter::present_drawn`, `present_blocking`); `present::show` and
  `present::set_display_enabled` cover a movie frame and a display gate, and `present::set_cap` is the cap's home.
  The handler flips a finished frame at the first edge the cap allows without waiting for its successor (the old
  queue showed frame N only when frame N + 1 was kicked), and a part is kicked as soon as the frame before it is
  on screen and DMA channel 2 is idle, by the handler at an edge or by the thread at submit and in every wait,
  with one kick implementation in assembly assembled twice (the thread's copy masks interrupts itself); the flip
  stays the handler's alone. One draw-done fallback instead of two. The presenter links the draw area, offset and
  clear in front of a chain frame, and the guard points the GPU at the next side after a frame is handed over.
  Host tests run the thread-side code against a model of the handler and the GPU with an event injected at every
  shared-word access (`present_model.rs`); `make hello-present-queue-gate` checks every frame's kick and flip edge
  by edge at 60, 30 and 20 fps and under a varying load, on the retail model and on an FPGA core's model that
  never raises GPUSTAT bit 24. psx-engine's runner hands every frame to the presenter (its own queued-flip wait,
  timeout and take-and-write recovery are gone). psx-gpu's packet draw is one out-of-line copy per packet type and
  the ready-wait reads GPUSTAT first and tests the recording only when the GPU is busy, so a program that draws
  through the port links less text than before and pays one byte test per packet for the recording.
  Deprecated, listed in `sdk/profile.toml`: `interrupts::queue_display_control_at_vblank`,
  `is_display_control_queued`, `take_queued_display_control`, `set_min_flip_vblanks`, `min_flip_vblanks`,
  `present::publish_raw` (and `present::start`). hardware-tests v2.17: step `PRESENT ONE PATH`.
  `make hello-present-cd-gate` presents chain and drawn frames under polled and interrupt-driven CD reads and
  across instruction-cache flushes; hardware-tests v2.18: step `PRESENT UNDER CD AND FLUSH`.
  A program that only draws through the port pays nothing on its port writes: drawn frames start with
  `Presenter::begin_frame` (the wait for the frame before), and the recording and guard flags, which only a
  chain hand-over or a recording sets, fold away. An engine scene that returns
  `RenderSubmission::PresentQueue` declares `Scene::PRESENT_QUEUE`; every other engine game links none of
  the chain path.

- `psx-math`: `clip_lanes::clip_lanes8_plane` runs its hand-scheduled kernel on every console build, and the `asm-kernels`
  feature of `psx-math` is gone. The feature chose between two implementations of one job per game. The kernel is
  bit-identical to the portable definition: `hello-asmprobe` runs both on 120,000 deterministic inputs (triangles to
  8-gons, destination capacity 0 to 8, planes through vertices, repeated vertices, every lane at the `i32` limits and next to
  the multiples of 4096) and the FNV hash over every output byte is the same for both, and the harness fails when the
  kernel is changed in three different places. Cycles per call on the emulator, portable definition then kernel:
  triangle inside 742 to 493, triangle outside 535 to 320, triangle with one vertex out 1,791 to 961, with two out
  1,726 to 907, quad with one out 1,913 to 1,066, quad with alternate vertices out 2,967 to 1,537, seven vertices with one
  out 2,279 to 1,377 (the kernel is 33% to 48% fewer cycles on every mix). The kernel is 992 bytes of text and 24 bytes of
  stack; the portable definition compiled stand-alone is 1,628 bytes and 120. Nothing outside `psx-goldsrc` calls the
  function, and a program that does not call it links none of it (the kernel has its own section). A program that
  enabled the feature removes it from its `Cargo.toml` and the `psx-math/asm-kernels` entry from its `psx-gate.toml`;
  its frames do not change. The portable definition stays as `clip_lanes8_plane_reference`, what the host and the probe run.

- `psx-gpu`: the `asm-kernels` feature and its tagged-stream link kernel are gone; `OrderingTable::link_tagged_packet_stream_unchecked`
  is the one inline-asm loop in `ot.rs` that every program already ran (no program enabled the feature, only `hello-asmprobe`).
  The kernel was bit-identical but won only on long streams: 23 cycles a packet against the loop's 28.5, and 28 cycles more
  setup, so per call (loop to kernel) 1 packet 76 to 104, 3 packets 133 to 148, 8 packets 274 to 263, 30 packets 898 to 762, 100 packets
  2,887 to 2,354, break-even about 6 packets, and 608 bytes of text against 104. `hello-asmprobe` now compares the console's loop with the portable
  definition over every stream length to 40, longer ones, thirteen slot distributions and the largest table (65,535 entries, with
  a guard word after it), and fails when the loop is changed in three different places.


- `Controllers::digital()` for a game that never uses sticks: `Controllers<false>`, the type's const parameter (`Controllers`
  is `Controllers<true>`, so every call site that exists is unchanged). The same `poll`, with the handshake, its retries and the
  motor mapping not compiled in, so a poll is what `poll_on` sends: five bytes to a digital pad, nine to a pad that is already
  analog. A program that uses `Controllers::new()` links byte-identical code (NitroXide's image is identical before and after).
  Measured on the one game that could use it, a PICO-8 port with a 100,000-cycle frame loop: text of the pad code 5,636 bytes
  with `poll_on`, 6,152 with `Controllers::digital()` in a bench program; a bench poll of a digital pad 15,853 cycles with
  `poll_on` and 16,060 with `digital()` (an analog pad 22,925 and 23,165), where the profile of the whole bench loop is 124,155
  and 124,131 cycles an iteration; in the game's own journeys (first three rooms, cart reset) the CPU time of a tick is +0.46%
  and +0.16% higher, about 270 cycles a poll (64 more instructions in the poll and a different code layout). That is not equal,
  so the game is not moved and the SDK's deprecated `poll_port1` stays until it is.
- `psx-hw` names four SPU registers a console sound diagnostic read by address: a voice's current volume,
  `spu::voice::current_vol_left(n)` and `current_vol_right(n)` (4 bytes a voice, from the current-volume bank), and the main
  volume as it stands now, `spu::MAIN_VOL_LEFT_CURRENT` and `MAIN_VOL_RIGHT_CURRENT`.
- `psx_pad::Controllers` protects a DualShock's analog handshake against an interrupt, for every game, and a pad left digital is
  asked again (`sdk/docs/PAD.md`, "A pad left digital"). Three games carried console-found protection against a pad left digital
  for the whole session (the handshake before interrupts were enabled; a request repeated while the pad read digital, bounded in
  Quake and not in VoXide or the launcher) and lost it on moving to `Controllers`, whose handshake runs at a game's first poll,
  with interrupts on in most games, reads none of its answers and repeats no packet. The history is in the repositories: those
  failures were seen on the earlier synchronous drivers (VoXide 2026-07-06, Quake before 2026-08-25), not on the interrupt-driven
  pad engine, which came later, and not on a console with today's `/ACK`-paced driver, which no console had run until 2026-11-14 (the first entry above). The mechanism that
  applies to today's driver as well is that a pad's `/ACK` is a pulse of about 90 cycles found by reading `STAT`, and an interrupt
  handler running over it hides it and the packet is dropped. `Transport` gains `mask_interrupts()` and `restore_interrupts()`
  (a `ControllerPort` masks and restores the CPU's interrupts, a model does nothing), and `Controllers` sends every configuration
  packet between them, from its first byte to its last (about 15,000 cycles; a pending interrupt is taken after). A pad that
  answered a packet in full (only a DualShock acknowledges all nine bytes) and still reports digital, an unknown ID or
  configuration mode after the handshake gets the handshake, or the exit for configuration mode, again, three times in all for the
  plug-in, which now bounds the exit retry too (it was three a poll for as long as the pad stayed parked). A digital pad pays
  nothing more than it paid. Host test: a DualShock whose set-analog packet loses its `/ACK` stays digital for 500 polls on the
  code before this change (the `0x44` sent once) and ends analog on the first poll after it; a pad that refuses analog is asked
  three more times and no more; a digital pad is never asked again. A normal frame pays nothing: the poll's own code gains no work
  (a test of `mode == Analog` before the retry logic, which moved out of line) and the early select's bookkeeping, which a poll
  with the early select off, as it was then, never needed, is compiled out. On the emulator, bench program, one poll of an analog pad between 100,000
  cycles of other work: 23,221 cycles before, 23,187 after (both ports, port 2 empty: 23,677 and 23,653); text 6,896 bytes before
  and 7,000 after, rodata 72 and 108. In NitroXide (origin main 35938ff): text +28, rodata +52, 80 bytes in all, the two
  `CP0` mask sequences, the retry and the constants that are loaded from memory since the mask is a barrier for the packets
  built in registers. Hardware tests v2.15, step `PAD CONFIG UNDER IRQ` (records `0x6F5`-`0x6F7`): the handshake with a timer
  interrupt forced over the `/ACK` of the set-analog packet, unmasked and as shipped, and the plain first-poll handshake with
  VBlank on, fifteen rounds each, for the one console session; `make hardware-tests` on the emulator: 150 pass and the two known
  fails (139, 201), `0x6F5` loses the packet 15 rounds of 15 and recovers on the second, `0x6F6` and `0x6F7` never do.
- `psx-spu`'s reverb path costs a program less RAM and no time: the zero block `start_reverb` cleared the work area from was a 256-byte
  static (the 256 B it cost every game that calls it); it is now a block of 128 bytes on the caller's stack for the length of the
  call (`start_reverb`'s frame grows from 40 to 200 bytes; pieces of 16 words, 64 bytes, take 177 and 258 HBlanks against today's 174
  and 255, so they are not used); the DMA write session (begin, send, end) is one routine for uploads, the silence block and the
  reverb clear instead of being inlined into each; the 32 coefficient writes are a loop, not 32 unrolled stores; and a fill enables the
  channel and works out its block size and count once, not for every piece (`psx_io::dma::SpuDma::write_counted`, which
  `write_blocks` now calls after enabling the channel). In a program that calls `start_reverb`, `stop_reverb` and `set_reverb_depth`
  (opt-level 3): text 9,384 -> 8,560, data 840 -> 616, bss 12 -> 12, total 10,236 -> 9,188. `start_reverb` takes 174, 255, 174, 254,
  174, 255 HBlanks before and 171, 251, 172, 251, 171, 250 after (three presets, two areas), and every SPU register read back after
  `start_reverb`, `set_reverb_depth` and `stop_reverb` is identical.
- `psx-splash` links less: `draw_line` walks the line by character with no re-slicing and no division by zero, so the
  splash holds no panic path of its own (a build that keeps formatted panics no longer links `slice_error_fail`, the
  `usize` formatter or `report_panic` for it), and its text loop is built for size. In a program that only calls the
  splash: 17,840 -> 16,704 bytes (text, data, bss) at opt-level 3, 16,064 -> 15,728 at "s"; with formatted panics
  18,524 -> 16,620 and 19,980 -> 18,572. The 25 frames sampled every 6th VBlank are byte-identical.
- `psx_io::cd`, `psx_io::cd::xa`: smaller CD command paths and a smaller XA player, measured on Hollow Knight's link
  (its own copy of the player, `xa_player.rs`, is deleted for this). The polled command paths inlined a 16-byte
  response drain and the response read at every call (five drains in one function); each is now one function
  (`drain_response_limited(16)` and an out-of-line `read_response_fifo`), the same register reads and writes in the
  same order and count per command. The acknowledge and the flag read stay inlined on purpose: out of line they sat in
  the polling loop of `SectorReader` and aliased it in the instruction cache (Hollow Knight: 31 percent more refills in
  its first loads, doors up to 0.7 s longer). `Player::poll` is one function, the loop restart and the stop are one
  copy each, the song's end is kept as an LBA, GetlocP is decoded from the three bytes it needs, and a suspension is a
  flag plus the next poll's Pause and head reading instead of code in the poll, so a program that never suspends does
  not pay for it. Link maps, Hollow Knight shipping disc: free RAM 22,340 before; 22,812 with its own player on this
  SDK; 22,460 with the SDK player. The 352 bytes the SDK player has over Hollow Knight's copy of the same job buy
  results that name the failed drive command (`Error`) in place of a bool, `suspend`, `resume` and
  `with_cd_suspended`, per-song speed (`DriveSpeed`, single and double) and the head sample behind `SongClock`; they
  are not a regression to chase.
- `psx-splash`: the one boot splash. `psx_splash::show(gpu, buffers, line, brightness, buttons)` plays the Bonnie
  Studios mark and one line of text with a sweeping sheen (150 frames, a fresh Cross, Circle or Start press skips it
  after 8) and returns; the line is the only thing a game chooses (`"Built with PSoXide"`, for example). The mark is
  stored once, run-length coded (4,692 bytes of the 8,192 a plain copy takes), and nothing is linked into a program
  that does not call it. It reads the pad only through the closure it is given, lifts the presentation cap for its own
  150 frames and puts it back. VoXide and the Celeste collection each swapped their own copy for the call (throwaway
  branches, not landed): every frame of the splash is pixel-identical on the emulator (31 and 35 checkpoints) and
  VoXide's own strict splash golden passes unchanged. Link maps: VoXide text +1,184, rodata -3,597, free RAM +2,416;
  Celeste text +2,212, rodata -3,395, free RAM +1,184. The splash is VBlank-locked, so its length is unchanged; CPU
  cycles inside it, measured on the emulator: VoXide +24,142 once before the first frame (it uploads its own font
  atlas) and +1,296 a frame, Celeste about +8,200 a frame (its own wait loop against psx-rt's is the likely part; not
  split further).
- `psx_rt::dispatch` saves and restores `$28`. This toolchain allocates the global pointer as a general register
  (107 of the 1,600 functions of one linked game write it with no save), so a handler could return with the
  interrupted code's `$28` changed; the two wrappers the dispatcher replaced saved it and the dispatcher did not. A
  game whose hot loops keep a live value in `$28` and whose music pump ran as a VBlank hook broke at the first note.
  The routine is 8 bytes larger (344 to 352) and a dispatch costs two instructions and about 8 cycles more
  (285.3 to 293.2 cycles for the routine itself, hello-gteirq on the emulator). The frame and the stack do not grow
  (the slot was free). `hello-gteirq` has a `$28` check, run by `make hello-gteirq-gate`: the foreground holds a
  known value in `$28` while a VBlank hook and a root counter handler overwrite it (fails on the dispatcher without
  the save, 9,625 changed passes; passes with it). `docs/INTERRUPTS.md` lists what is saved and why.
- `psx_rt::dispatch`: one interrupt dispatcher, a short assembly routine. psx-rt's exception handler is the only
  handler in the vector; code that needs an interrupt registers with `on_vblank(handler)` (4 slots, run after psx-rt's
  VBlank work) or `attach(source, handler)` / `detach(source)`, and the handler runs on one shared 512-byte private
  stack after a single save of the caller-saved registers (`docs/INTERRUPTS.md`). psx-cdstream (the transport and
  `drain`) is on it: its two exception wrappers, their stacks and contexts are gone, and the CD handler is registered
  only while the stream has the source open (a foreground call only masks it for its own length).
  `install_vblank_counter` is out of line. Free RAM against the previous SDK (link maps, PGO off): hello-tri, which
  registers nothing, -160 bytes; VoXide, which registers nothing and installs from three places, +204;
  hello-cdstream +1,400; hello-cddrain +704. Cycles per interrupt on the emulator (table in
  `docs/INTERRUPTS.md`): a VBlank with nothing registered +11.1 (113.6 to 124.7, five instructions); a VBlank in a
  program with a CD handler about 43 to 51 fewer; a CD interrupt 242 fewer in hello-cddrain, and in hello-cdstream,
  over eight placements of its handler in the instruction cache, 21,832 to 22,012 against 21,851 to 22,180 before.
  `hello-gteirq` checks the dispatcher's order against its Rust reference and runs its RTPS loop under root
  counter 2 interrupts served by it.
- The select left open is on for every game that reads through `Controllers`: a console passed hardware-tests v2.13
  (`SIO0 SELECT EARLY`, records `0x6F0` to `0x6F4`). On 2026-11-14, console SCPH-9002, two runs with two DualShocks (an SCPH-1200 and an
  SCPH-110), the five cases read `0x88`, `0x888`, `0x888`, `0x888`, `0x8088` on both runs (every poll clean, the select kept on the three
  cases that expect it, a card frame read in the last). Median cycles of a poll, run 1 / run 2: the sequence before 22,654 / 23,201; the
  select one frame early 15,819 / 15,961; half a second early 15,678 / 15,845; no gap 22,597 / 23,142 (within 60 cycles of the sequence
  before, so the root counter that times the setup runs at the clock rate); with a card frame read between 22,680 / 22,964. The
  early select saves 6,800 to 7,400 cycles a poll on the console. `SELECT_EARLY` and the sequence it kept are gone. The poll costs
  measured with it off, in the handshake retry entry above, are the cost before this change.
- `psx_pad::Controllers`: one way for every game to read its pads, and `psx-engine`'s `App` and `Ctx::refresh_second_pad`
  read through it. `poll(&mut port, Port::One)` reads one port (a two-player game makes a second call for port 2, so a
  game that never asks for it pays nothing); the first time a port answers it runs one handshake (analog requested and
  locked, a video frame between packets), a port that did not answer is probed once in 60 polls instead of every call,
  and a garbled packet returns the last clean state. `set_rumble(&mut port, port, rumble)` maps the motors the first
  time it is called for a pad and then rides every poll; `analog(port)` says whether a pad is analog, digital or absent.
  `App` no longer calls `require_analog_on` at boot or `enable_analog_on` on a hot-plug: the first poll of a pad does
  both, with the one-frame spacing of `require_analog_on` (a pad plugged in mid-game now holds the loop about four
  frames, as at boot). `PadState` is word-aligned.
- The pad's setup time can pass while the game works (`sdk/docs/PAD.md`, "The select is left open"):
  `psx_io::controller_port::Transport` gains `clock()` (root counter 0, the system clock psx-rt leaves running),
  `open(port)` and `begin_pad(port, opened)`. A `Controllers` poll ends by asserting `/CS` on the port it expects
  next, and the next poll clocks its bytes as soon as `PAD_SETUP_CYCLES` (7,168 cycles, what the 1,024 status reads
  took on a console) have passed since then, measured on the clock, so a poll that comes at once still waits all of
  it. A select that a card transfer or anything else released is made again with the old delay. Anyone implementing
  `Transport` outside the SDK adds `clock()`.
  On the PSoXide emulator: one poll of an analog pad 23,054 cycles at `efdd3083c`, 16,129 with
  `Controllers` (port 2 empty and asked for every loop: 43,218 and about 16,600). NitroXide's attract bench 38.91
  flips a second before the pad engine was deleted, 38.73 on SDK main, 39.08 with this; its image has 100 bytes less
  text than on SDK main. VoXide, which still reads through `poll_port1`, has 548 bytes less text than at `efdd3083c`
  and puts the same bytes on the wire.

- The synchronous pad poll (`psx_pad::poll_on`) accepts an `/ACK` as the live level or as the latched interrupt bit, so
  it works on PCSX-ReARMed, which latches `STAT` bit 9 and never shows bit 7. A port that shows the level takes the same
  register accesses as before and no `CTRL` write mid-packet. When only the latch answers, `psx_io::controller_port::Transport::exchange`
  waits for the level to release, then writes `CTRL` once with the ACK interrupt armed and the latch cleared. An empty
  socket on such a port reads `Disconnected` in one poll, not `Unknown` after four. `Transport::exchange` now takes the
  `Port`, and `Transport` gains `control()` and `ack_irq_armed()`: anyone implementing `Transport` outside the SDK must add
  `control()` (a read of `CTRL`). Reported by Thomas Bertani, with a patch.
- The interrupt-driven pad engine is deleted: `psx_pad::engine` and `psx_pad::console` with the `irq-engine` feature, psx-engine's `pad-irq-engine` and `pad-irq` features and the branches behind them, editor-playtest's features for them, `sdk/docs/PAD-IRQ-ENGINE.md` and the `pad-engine-check` example. The synchronous driver is the one driver for every game (`sdk/docs/PAD.md`). A game that enabled `pad-irq-engine` drops it from its features.
- `SectorReader::stop` returns at Pause's acknowledge instead of waiting 16,384 spins (about 296,000 clocks, 8.9 ms) for its
  second response. The wait always ran out: on the console the response comes 1.2 to 3.9 million clocks after the acknowledge
  (hardware-tests v2.7), and the emulator models it. The wait came in with the reader, ported with the poll limits of a loader that had
  run on a console (the commit lists the limits, it records no finding for this wait), and the doc written with `stop_proved`
  already said `stop` returns at the acknowledge. A console passed hardware-tests v2.14 (`CD STOP AT ACKNOWLEDGE`, records
  `0x6F8` to `0x6FB`), so `psx_io::cd::reader::STOP_AT_ACKNOWLEDGE`, the wait and `SectorReader::stop_waiting` (which ran both
  sequences for the step) are deleted. On 2026-11-14, console SCPH-9002, two runs, forty reads each, every sector checked:
  all 180 sectors of the mixed reads and all 40 of the single reads arrived with no error and no retry in all four cases of both runs.
  Time in units of 16 HBlanks (about a millisecond), waiting against early return, run 1 / run 2: mixed reads 7,511 against 6,725
  / 6,925 against 5,557; single reads 6,183 against 6,077 / 5,536 against 5,103. The early return was faster in every pair, by 106 to
  1,368 units, and the spread between the two runs is as large as the saving, so no single figure is quoted. On the emulator a load of
  100 reads of 1 to 8 sectors, one after another, took 240,867 HBlanks with the wait and 232,076 without (15.3 s and 14.7 s, the same
  bytes).
- Hardware tests v2.14 (machine-code baseline pinned, drift 0 against v2.13): step `CD STOP AT ACKNOWLEDGE` and records `0x6F8` to `0x6FB`, four pass-or-fail cases (forty reads each,
  every sector checked) for `stop` with and without its wait for the second response. Retired in v2.19 with its verdict: the step is
  gone and the ids are not reused; the decoder still names them so captures of v2.14 to v2.18 read. What a pass and a fail meant is in
  `docs/hardware-test-versions.md`.
- The hardware-tests machine-code baseline for v2.13 is pinned (`docs/hardware-refs/hwtest-machine-code-v2.13.txt`, made with the
  command `make hwtest-verify-code` prints, from the v2.10 file: 218 probes, drift 0, so no measured block changed shape across
  v2.11 to v2.13), and `make hwtest-verify-code` passes again. A host test now fails `make test` when `SUITE_VERSION` has no
  baseline file, so a bump cannot leave one out again (v2.11, v2.12 and v2.13 had).
- Hardware tests v2.13: step `SIO0 SELECT EARLY` and records `0x6F0` to `0x6F4`, five pass-or-fail cases on port 1 for the
  pad poll on a select left open (the sequence before, one video frame, half a second, no gap, a card frame read in the
  gap). What a pass and a fail of each mean is in `docs/hardware-test-versions.md`.
- Hardware tests v2.12: the interrupt-driven pad engine is gone, and its steps with it (`ENGINE SETUP SWEEP`, `ENGINE ACK PACING`,
  `ENGINE TIMED PACING`, `ENGINE AND CARD LEASE`, `ENGINE UNDER LOAD`, `PAD HOT-PLUG WINDOW`); their record ids are retired.
- Hardware tests v2.11: step `SIO0 LATCH AND ARM` and records `0x6E0` to `0x6E3` (does `STAT` bit 9 set with `CTRL` bit 12
  clear, and does one `CTRL` write after an acknowledged byte disturb the rest of the packet). The v2.10 baselines in
  `docs/hardware-refs` do not match v2.11 until they are regenerated.
- `VramAllocator::with_clut_band` keeps the draw-done block in the band's bottom right corner (`draw_done_scratch()`, it takes eight CLUT slots) instead of the corner of VRAM, so the texture page there is allocatable again (Cortex's twelfth free page); `psx_game_runtime::vram::VramRuntime` points the sequence at it with `set_draw_done_scratch`. Allocators without a band, and every game that does not use `VramAllocator`, keep (1008, 504).
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
- `console-diag` no longer turns `host-tty` on. A test hardware-tests disc built with it stopped on the fault panel at boot on a
  console, a data bus error on the first byte stored to the debug console: Expansion 2 is 128 bytes in the entry state and the debug
  console is at offset 0x80. The emulator does not model the error, so every emulator run had passed. The stop panel draws on screen and
  needs no tty. `tty.rs` and `psx_hw::memory::expansion2::DEBUG_CONSOLE` said a retail console ignores the store; they now say what it does.
  The earlier entry (below) names `console-diag` as turning it on; it no longer does.
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
