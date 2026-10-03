// SPDX-License-Identifier: GPL-2.0-or-later
//! Renamed to [`crate::hardware`].
//!
//! Every item here forwards to its new path and is deprecated.

use crate::hardware;

/// Moved to [`hardware::Slot`].
#[deprecated(note = "moved to `psx_mc::hardware::Slot`")]
pub type Slot = hardware::Slot;
/// Moved to [`hardware::TransportFault`].
#[deprecated(note = "moved to `psx_mc::hardware::TransportFault`")]
pub type TransportFault = hardware::TransportFault;
/// Moved to [`hardware::TransportTrace`].
#[deprecated(note = "moved to `psx_mc::hardware::TransportTrace`")]
pub type TransportTrace = hardware::TransportTrace;
/// Moved to [`hardware::Timing`].
#[deprecated(note = "moved to `psx_mc::hardware::Timing`")]
pub type Timing = hardware::Timing;
/// Moved to [`hardware::HardwareCard`].
#[deprecated(note = "moved to `psx_mc::hardware::HardwareCard`")]
pub type HardwareCard = hardware::HardwareCard;
