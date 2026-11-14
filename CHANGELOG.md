# Changelog

## Unreleased

- `psx_pad::Controllers` (both `new()` and `digital()`) reads a pad that is slow to come up from its first answer. A port that
  answers "nobody there" was left alone for 59 polls and probed once in 60, so a pad that needed more than a moment was ignored
  for two seconds. PCSX-ReARMed's DualShock, in either mode, acknowledges the address byte and answers `0xFF` to the command
  byte (what an empty socket does on that port) for its first 33 video frames (31 to 33 whatever the polls a frame): a game on `Controllers::digital()` first saw a held
  button at its 124th frame there, a plain digital pad at its 20th, and the earlier poll at its 33rd. The port is now polled
  every time for its first 128 such answers (and for 128 after a pad leaves), then as before. A plain pad that answers the first
  poll never reaches this code; an empty port 2 pays 128 failed polls once (about 20,000 cycles each in the emulator). The
  PSoXide emulator's DualShock answers from the first poll and cannot show the bug. Hardware-tests v2.16 adds the console's
  answer (`PAD DIGITAL FIRST POLL`, records `0x6FC`-`0x6FE`; the suite number v2.16 is now this step's, so the unlanded present-path branch, which uses v2.16 and v2.17, renumbers when it is rebased).
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
  pad engine, which came later, and not on a console with today's `/ACK`-paced driver, which none has run. The mechanism that
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
  with `SELECT_EARLY` off never needed, is compiled out. On the emulator, bench program, one poll of an analog pad between 100,000
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
  `Transport` outside the SDK adds `clock()`. Behind `SELECT_EARLY` in `psx-pad`, off until a console passes
  hardware-tests v2.13: until then every poll selects and waits as before.
  With it on, on the PSoXide emulator: one poll of an analog pad 23,054 cycles at `efdd3083c`, 16,129 with
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
- `SectorReader::stop` waits 16,384 spins (about 296,000 clocks, 8.9 ms) for Pause's second response after every read, and the
  wait always runs out: on the console the response comes 1.2 to 3.9 million clocks after the acknowledge (hardware-tests v2.7),
  and the emulator models it. The wait came in with the reader, ported with the poll limits of a loader that had run on a console (the commit lists the limits, it records no finding for this wait),
  and the doc written with `stop_proved` already said `stop` returns at the acknowledge. `psx_io::cd::reader::STOP_AT_ACKNOWLEDGE`
  (off) makes it do that; a load of 100 reads of 1 to 8 sectors, one after another, takes 240,867 HBlanks as shipped and 232,076 with it on the emulator (15.3 s and 14.7 s, the same bytes). Off until a console passes hardware-tests v2.14 (below); then the constant and the wait are deleted.
- Hardware tests v2.14 (machine-code baseline pinned, drift 0 against v2.13): step `CD STOP AT ACKNOWLEDGE` and records `0x6F8` to `0x6FB`, four pass-or-fail cases (forty reads each,
  every sector checked) for `stop` with and without its wait for the second response. What a pass and a fail mean is in
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
