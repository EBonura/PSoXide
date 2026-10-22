# Rust style

We follow [Google's Rust style guide](https://google.github.io/styleguide/rustguide.html),
which itself defers to the [official Rust style guide](https://doc.rust-lang.org/style-guide/)
and the [Rust API guidelines](https://rust-lang.github.io/api-guidelines/). Where this file
records a deviation, this file wins. Everywhere else the guide wins over taste.

## Rule 0: games run well first

Frame time, RAM, VRAM, binary size and load time all outrank style. No style change may cost
any of them. Mechanical changes (formatting, renames, docs, `#[must_use]`) keep guest binaries
byte-identical. Changes that can alter codegen (lint fixes, iterator or loop rewrites, panic
handling, extra trait impls or derives that add code) need a measurement showing no cost.
Where style and efficiency conflict, keep the efficient form with a one-line comment saying
why. Manny, 2026-10-22: getting the games to run well and efficiently trumps style guides.

Status: the formatter settings and the lints marked "deny" are enforced today. The
deviations under "Project deltas" were **APPROVED** by Manny on 2026-10-22.

## Scope

The SDK crates (`sdk/crates/*`, `crates/*`) are library code, so the guide's API design rules
are binding there. Games, the emulator, the editor and tools are application code, where the
same rules are a strong recommendation: prefer simplicity, do not generalise early.

## What is enforced, and where

| Rule family | Mechanism | State |
|---|---|---|
| Formatting: `max_width = 100`, lower-case hex literals, `format_macro_matchers`, one module per `use` | `rustfmt.toml` (root and `sdk/`); `make fmt-check` in CI, which also covers `sdk/examples/*` | enforced |
| `wrong_self_convention`, `should_implement_trait`, `len_without_is_empty`, `borrowed_box`, `ptr_arg`, `new_without_default`, `new_ret_no_self`, `missing_safety_doc`, `undocumented_unsafe_blocks`, `unnecessary_unwrap`, `from_over_into`, `inherent_to_string`, `enum_glob_use`, `rc_buffer`, `rc_mutex`, `get_first`, `unsafe_op_in_unsafe_fn`, `missing_docs` | `[workspace.lints]` in `Cargo.toml` and `sdk/Cargo.toml`, run with `-D warnings` by `make lint` | enforced on every crate that opts in with `[lints] workspace = true` |
| `unwrap_used`, `missing_errors_doc`, `missing_panics_doc`, `must_use_candidate`, `exhaustive_enums`, `exhaustive_structs`, `wildcard_imports`, `string_add`, `string_add_assign`, `fn_params_excessive_bools`, `needless_for_each`, `Rc`/`Arc` (`disallowed_types`), `catch_unwind` (`disallowed_methods`), `missing_docs` on crates that have not opted in | allowed in the manifests, counted by `tools/style-ratchet.sh` against `tools/style-ratchet.baseline` | ratchet: a count may fall, never rise |
| Naming, newtypes, trait design, getters, `Ext` suffixes, abbreviations, module-qualified calls | review | by hand |

`make lint` runs the ratchet last. `tools/style-ratchet.sh --report` prints today's counts;
after a cleanup, `tools/style-ratchet.sh --update` lowers the baseline in the same change.
The ratchet counts library and binary targets only. Tests may unwrap and panic, as the guide
allows (`allow-unwrap-in-tests` in `clippy.toml`). It also measures the SDK for the PS1
target, because the guest-only code is compiled out of a host build. It needs `jq`.

Run `make fmt` before pushing and `make lint` before asking for review. Three of the rustfmt
options are unstable, so formatting needs the pinned nightly, which `rust-toolchain.toml`
already selects.

## Project deltas (APPROVED 2026-10-22)

Each row is a place where the guide, applied as written, does not fit a PlayStation 1 target
or this tree. None is a general exemption: the last column says what replaces the rule.

| # | Guide rule | Why it does not fit here | Project rule |
|---|---|---|---|
| 1 | No unstable features | The `mipsel-sony-psx` target is tier 3 and needs `-Zbuild-std`. BIOS trampolines and asm kernels need `asm_experimental_arch`, games use `optimize_attribute`, builds use `-Cpanic=immediate-abort` and `-Zfmt-debug=none` | Unstable features only in guest (MIPS) crates, from a short allowlist (`asm_experimental_arch`, `optimize_attribute`), each use with a comment saying why. Host crates (cookers, tools, emulator, editor) use none. The nightly stays date-pinned in `rust-toolchain.toml` |
| 2 | Prefer `expect` to `unwrap`; panic only for bugs | Guest builds turn a panic into a bare `break` and drop the message. Measured: `expect("long message")` and `unwrap()` produce byte-identical images, so the message costs nothing, but a panic path costs code. Per-frame code should not carry one | Library code: `expect("reason")` where a panic is unavoidable, pattern matching or a total function otherwise. Guest per-frame code: no `unwrap`/`expect` and no indexing that can panic unless measured and commented. Developer-facing host binaries may panic, as the guide allows |
| 3 | `#[non_exhaustive]` on public enums and structs across team boundaries | Many SDK enums are closed by the silicon (GP0 commands, DMA channels, register fields). Games pin the SDK by revision, so a new variant is a deliberate repin the compiler should flag | `#[non_exhaustive]` on open-ended public types: error enums, event kinds, file-format versions, option bags. Closed hardware types carry `#[allow(clippy::exhaustive_enums)] // closed by hardware` |
| 4 | `unsafe` only without a safe equivalent, kept in a small separate crate | A PS1 SDK is hardware access: MMIO, DMA, scratchpad, BIOS calls | Register addresses live in `psx-hw` (already a repo gate) and MMIO access in the HAL crates. Every `unsafe` block, impl and fn has a `SAFETY:` comment (enforced). Game crates do not touch registers; `unsafe` in a game is for guest-global state or measured hot paths and says which |
| 5 | Library parameters take iterators | `impl IntoIterator` monomorphises per call site (code size on a 2 MB machine), and DMA, the GTE and packet lists need contiguous memory | SDK guest APIs take slices where the hardware needs contiguous data or where size or cycles were measured. Host library code follows the guide |
| 6 | Generics over `dyn`; `impl Into` where ergonomic | The guide already lets code size win | Guest code: no `impl Into` parameters; `dyn` only where it removes measured code size. Host code follows the guide |
| 7 | Avoid `Rc`/`Arc`; no std threads or `async` [Chromium] | The emulator and editor are multithreaded GUI programs (audio, wgpu, web player) | Guest code has none of them. Host code may use `Arc` and threads where unique ownership is unreasonable; a new use carries `#[allow(clippy::disallowed_types, reason = "...")]` |
| 8 | Lower-case hex literals | Hardware documentation prints addresses and opcodes in upper case | Lower case anyway (rustfmt does it). Addresses are named through `psx_hw` instead of spelled |
| 9 | `#[rustfmt::skip]` only for manual alignment or generated code | Opcode tables, GTE command words, asm templates, generated glyph and cook tables | These count as manual alignment and generated code. A generated file carries an `@generated` header and is listed in `rustfmt.toml` `ignore` |
| 10 | `macro_rules!` only when regular code cannot do it | Guest code needs guaranteed inlining and register-definition tables | Allowed for those two cases, with a one-line reason |
| 11 | No unexplained abbreviations | The domain is full of established acronyms | Allowed without comment: GPU, GTE, SPU, DMA, CD, XA, VRAM, OT, MDEC, IRQ, SIO, BIOS, MIPS, COP0, BSP, PVS, LOD, UV, RGB, FMV. Anything else is spelled out; a new acronym is added to this list in the change that introduces it |
| 12 | Examples in the crate docs; document panics | Doctests cannot run for the MIPS target | Guest-crate examples are `no_run` or `ignore`, with the host-testable part split out. `# Errors` and `# Panics` sections on public library functions (ratchet) |
| 13 | (not in the guide) numeric casts | Guest math is 32-bit fixed point with no floats and no 64-bit arithmetic, gated by `guest_symbol_gate.sh` | `as` casts in fixed-point code are by design and the cast lints stay off. Fixed-point quantities become newtypes where they cross an API |
| 14 | Chromium and Fuchsia rules: flat file layout, `chromium::import!`, `rust_gtest_interop`, `fuchsia_sync` | Other projects' build systems | Not adopted. Cargo's `src/` with module directories stays; tests are plain `#[test]`, plus Miri for the SDK |
| 15 | rustfmt style edition | The guide names none | Edition 2021 formatting for now. Moving to the 2024 style is a separate whole-tree reformat |

## Where the tree stands

`tools/style-ratchet.baseline` is the authoritative count. Cleanups in order of payoff:
`#[must_use]` (mechanical, no codegen change), `# Errors` and `# Panics` sections, API items
out of non-leaf modules (`psx-io/src/cd.rs`, `psx-mc`), then `#[non_exhaustive]` per delta 3.
