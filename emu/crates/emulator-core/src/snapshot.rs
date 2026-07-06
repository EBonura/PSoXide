//! Save-state payload types.
//!
//! `emulator-core` doesn't know anything about the on-disk save-state
//! format (that lives in `psoxide_settings::savestate`, kept
//! emulator-core-agnostic to avoid a circular dependency) -- it only
//! needs to hand the format layer something serializable. These two
//! types are that "something": a borrowed form for writing (no need to
//! clone a multi-megabyte `Cpu`/`Bus` just to hand the encoder
//! ownership) and an owned form for reading back.

use crate::{Bus, Cpu};

/// Borrowed view over a running emulator, used only for *writing* a
/// save state. Deliberately holds references rather than owned copies
/// -- `Bus` alone carries multiple megabytes of RAM/VRAM/SPU RAM, and
/// a save is just "serialize what's already there," not "duplicate it
/// first."
#[derive(serde::Serialize)]
pub struct EmulatorStateRef<'a> {
    /// CPU registers, GTE state, and load/branch-delay machinery.
    pub cpu: &'a Cpu,
    /// Full bus + peripheral state (minus the disc image and BIOS
    /// bytes -- see the `#[serde(skip)]` docs on [`Bus`]'s fields).
    pub bus: &'a Bus,
}

/// Owned emulator state produced by loading a save state back off
/// disk. The frontend still has to patch in the two fields
/// deliberately excluded from serialization -- the currently-loaded
/// BIOS image and a remounted disc handle -- before this is usable as
/// a live `Bus`; see `psoxide_settings::savestate` and this crate's
/// `bus.rs`/`cdrom.rs` field docs for exactly which fields those are
/// and why.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct EmulatorState {
    /// CPU registers, GTE state, and load/branch-delay machinery.
    pub cpu: Cpu,
    /// Full bus + peripheral state (minus the disc image and BIOS
    /// bytes).
    pub bus: Bus,
}
