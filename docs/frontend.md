# Frontend architecture

The desktop frontend lives in [`emu/crates/frontend`](../emu/crates/frontend). It's a single-threaded wgpu + egui app that drives the emulator core directly -- no `Arc<Mutex<_>>`, no message-passing, no separate render thread. The UI reads emulator state in place each frame.

## Why single-threaded?

Both prior attempts (`psoxide`, `PSoXide-2`) experimented with threaded architectures and consistently paid more in debugging cost than they saved in performance. A 33.87 MHz PS1 CPU is not going to out-run a modern host in any foreseeable scenario; serializing CPU stepping and rendering on the same thread keeps the debugger honest and the code boring.

## Stack

| Crate | Version | Role |
|---|---|---|
| `winit` | 0.30 | Window + event loop |
| `wgpu` | 24 | GPU surface + texture upload |
| `egui` | 0.31 | Immediate-mode UI |
| `egui-wgpu` | 0.31 | egui backend against wgpu |
| `egui-winit` | 0.31 | egui input from winit events |
| `pollster` | 0.3 | Block-on for wgpu async setup |

These are pinned deliberately -- both priors independently converged on this set, and any bump is a conscious decision, not drift.

## Module layout

```text
src/
├── main.rs           # winit ApplicationHandler shell, event dispatch, run loop
├── app.rs            # AppState -- emulator + UI state, no Arc/Mutex
├── gfx.rs            # Graphics -- wgpu surface + egui renderer + VRAM/display textures
├── theme.rs          # charcoal/teal palette, VT323 + Lucide fonts, section helpers
├── icons.rs          # Lucide codepoint constants
├── cli.rs            # headless subcommands (launch, validate, build-project-disc, ...)
├── disasm.rs         # MIPS disassembler for the registers/memory panels
└── ui/
    ├── mod.rs           # draw_layout -- composes toolbar/sidebar/framebuffer/overlays
    ├── toolbar.rs       # top strip: status, EMU/DRAW/HOST/MIPS/dt/AUDIO, transport, toggles
    ├── debug_sidebar.rs # right sidebar docking the four debug sections below
    ├── registers.rs     # CPU + COP0 + history + breakpoints section
    ├── memory.rs        # hex+ASCII / disasm viewer, quick-jump, BP toggle
    ├── profiler.rs      # FrameProfiler data model + profiler section + CSV/stderr
    ├── vram.rs          # 1024×512 VRAM image section (true 2:1 aspect)
    ├── framebuffer.rs   # central 4:3 game framebuffer image
    ├── hud.rs           # HudState rolling metrics (data only; toolbar renders it)
    ├── burn.rs          # CD-R burn window
    └── menu.rs          # PSX-style overlay menu, Painter-drawn on Middle layer
```

## Layer order, outside-in

1. **Toolbar** (`egui::TopBottomPanel::top`) -- status dot, live metrics, transport controls, volume, debug toggles.
2. **Debug sidebar** (`egui::SidePanel::right`, hidden by default) -- one resizable sidebar docking four `CollapsingHeader` sections: CPU Registers, Memory, VRAM, Frame Profiler. All sections lay out width-aware: the GPR grid reflows 1/2/4 columns, the hex dump adapts bytes-per-row (16/8/4), profiler bars stretch with the panel, and the VRAM image keeps its true 2:1 aspect.
3. **Central panel** -- the live PS1 framebuffer at 4:3.
4. **Menu overlay** on `egui::Order::Middle` -- dims background, slides animated category icons.
5. **Burn window / status toast** on top.

Each section is its own module, so adding a new one is about 150 lines and touching `ui/debug_sidebar.rs`.

## Data flow per frame

```text
winit event
  → Shell::window_event
    → (keyboard?) merge_key() into pending MenuInput
    → (redraw?) → run_frame:
        1. dt from Instant
        2. MenuState::update(input) → Option<MenuAction>
        3. ui::apply_menu_action (run/step/reset/toggle panels)
        4. run loop: bus + cpu → exec_history ring, breakpoint check
        5. GPU command-log drain (+ opt-in --gpu-compute shadow replay)
        6. Graphics::prepare_vram(state.bus?.gpu.vram)
        7. HW renderer scale/update + frame replay
        8. Graphics::render(|ctx| ui::draw_layout(...))
        9. FrameProfiler::record(sample)
```

Key pattern: `run_frame` destructures `state` so `state.bus`, `state.cpu`, and `state.exec_history` are three disjoint field borrows Rust accepts simultaneously. A `&mut self` method on `AppState` would block that.

## VRAM upload

`Graphics` owns a persistent `wgpu::Texture` (1024×512, `Rgba8UnormSrgb`, `TEXTURE_BINDING | COPY_DST`) registered with the egui-wgpu renderer as a native texture once at startup. Every frame, `prepare_vram` decodes the 16bpp VRAM into an RGBA8 scratch buffer (full-range `(v<<3)|(v>>2)` expansion, not the naive `v<<3` that loses 8% of white brightness) and `queue.write_texture`s it onto the persistent target.

The VRAM panel then renders the single `egui::Image` referencing this texture -- all three panels (game view, VRAM view, and future framebuffer clip) will eventually share the same upload by differing only in their `uv` rect.

## Frame profiler

The Frame Profiler is a section of the debug sidebar (toolbar bug icon or
Menu -> Debug to open). It records a rolling sample per redraw: input/Menu, guest emulation, SPU/audio, command-log
drain, compute replay, VRAM upload, hardware-render scale/clone/replay, and
egui/wgpu presentation. The same sample includes emulated frame count, CPU
ticks, bus cycles, emulated VBlank cadence, draw-producing VBlank cadence,
step-cap misses, GTE command load, GP0 command counts, draw/image command
splits, FIFO words, and current hardware-renderer scale.

For terminal-accessible measurements, launch with `PSOXIDE_PROFILE=1` to print
a rolling one-line average roughly once per second. Use
`PSOXIDE_PROFILE=trace` to print every frame. In those lines, host timings
remain in milliseconds, while `emu_hz`, `draw_hz`, `cyc_f`, `budget_f`,
`instr_f`, `gte_f`, and `gtecy_f` describe the emulated PS1 workload and
cadence. Guest-stage fields ending in `_v` are normalized per rendered visual
frame; fields ending in `_hit` are normalized per telemetry span.

For a repeatable playtest render benchmark, `make profile-demo7-camera-sweep`
cooks demo7, builds the playtest with a deterministic slow orbit camera, and
prints the headless guest profile including room, model, GTE, and room-surface
micro-profiler counters.

## Menu mechanics

Three categories (Game / Debug / System) plus infrastructure for expansion. Drawn entirely through `egui::Painter` on a middle layer -- no high-level widgets -- which keeps it snappy and position-locked.

- `anim_x` interpolates toward `target_x` at `10/dt`, yielding the signature horizontal slide.
- Selection uses a 3-pixel accent-color bar on the left edge of the item rect.
- Input: arrows navigate, Enter/Space confirms, Escape toggles open/closed.
- Gamepad support will land alongside the controller subsystem.

## Keyboard shortcuts

| Key | Action |
|---|---|
| Esc | Toggle Menu (and back-out when navigating) |
| ↑ ↓ ← → | Navigate Menu (items / categories) |
| Enter / Space | Confirm item |

## Debugging loop

The frontend is designed to double as a live debugger:

1. Pause (Menu → Game → Pause, or just open the Menu with Esc).
2. Scroll the memory viewer with the quick-jump buttons to interesting addresses.
3. Toggle a breakpoint with "Set BP" at the current viewer address; the row highlights in the accent color.
4. Resume. The run loop checks `breakpoints.contains(&cpu.pc())` before each `cpu.step` and pauses on match.
5. The register panel shows last 64 retired instructions, live COP0 state, and all active breakpoints.

## What's intentionally absent

- **No threads.** The single-threaded loop above is a design decision, not
  a milestone gap; see "Why single-threaded?".
- **No UI snapshot tests.** The data layer (profiler averaging, menu model)
  is unit-tested; visual layout is verified by running the app.

(The gaps this section once listed -- gamepad, audio, framebuffer display,
save states -- have all since landed: gilrs input, cpal output, the live
central framebuffer, and savestates via `psoxide-settings`.)
