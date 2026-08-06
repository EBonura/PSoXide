// SPDX-License-Identifier: GPL-2.0-or-later
//! PSoXide emulator core.
//!
//! At this stage the core exposes just enough to load a BIOS, seat a
//! CPU at the reset vector, and fetch its first instruction. No
//! execution yet -- this is the thin wire along which the rest of the
//! emulator will be strung.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

pub mod bus;
pub mod cdrom;
pub mod cpu;
pub mod dma;
pub mod fastboot;
pub mod freelook;
pub mod gpu;
pub mod hle_bios;
pub mod input_tape;
pub mod irq;
pub mod mdec;
pub mod mmio_trace;
pub mod pad;
pub mod scheduler;
pub(crate) mod serde_big_array;
pub mod sio;
mod sio1;
pub mod snapshot;
pub mod spu;
pub mod telemetry;
pub mod timers;
pub mod vram;

// Root re-exports: only what the two consumers (frontend and
// psx-gpu-render) actually reach by name. Everything else stays on
// its module path.
pub use bus::Bus;
pub use cpu::{Cpu, InstructionCacheProfileSnapshot};
pub use fastboot::{
    fast_boot_disc, fast_boot_disc_with_hle, warm_bios_for_disc_fast_boot,
    DISC_FAST_BOOT_WARMUP_STEPS,
};
pub use freelook::FreelookState;
pub use gpu::{DisplayArea, Gpu};
pub use input_tape::{
    game_image_hash, game_image_hash_parts, read_tape, tape_from_bytes, tape_from_csv, tape_to_csv,
    write_tape, PadSample,
};
pub use pad::{button, ButtonState};
pub use psx_gte_core::GteProfileSnapshot;
pub use snapshot::{EmulatorState, EmulatorStateRef};
pub use telemetry::{GuestTelemetry, GuestTelemetryEvent, GuestTelemetryKind};
pub use vram::{Vram, VRAM_HEIGHT, VRAM_WIDTH};
