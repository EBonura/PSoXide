// SPDX-License-Identifier: GPL-2.0-or-later
//! Host-side drive model and route simulator for `psx-residency`.
//!
//! The simulator runs the real [`psx_residency::Residency`] against a model of the
//! console's drive, so a route through a region graph can be checked for
//! deadline slack and misses long before the transport exists on the console.
//! Nothing here is part of a guest build: games depend on `psx-residency`
//! only, and this crate is `no_std` plus `alloc` so it still compiles for the
//! console target in the SDK lint.
//!
//! # What is measured and what is assumed
//!
//! The drive model's numbers come from the streaming survey and are labelled
//! as the survey labels them:
//!
//! * Read: 6.49 ms per sector at double speed and 13.20 ms at single speed,
//!   measured on silicon, within 1% of the specification.
//! * Seek: 11, 79, 137 and 310 ms for 1, 16, 128 and 512 sectors, measured on
//!   silicon, with a run-to-run swing of about 2x (rotational phase dominates).
//!
//! Everything else is an assumption, and the model says so in its types:
//! seeks between measured points are interpolated linearly, seeks beyond 512
//! sectors are clamped to the 512-sector value unless the caller supplies one
//! ([`DriveModel::with_far_seek_millis`]), a cancelled read stops
//! instantly unless the caller supplies an abort time, and install work costs
//! nothing unless the caller supplies ticks per region (the design lists the
//! install cost as unmeasured). A simulation result is only as good as these.
//!
//! # Use
//!
//! ```
//! use psx_residency_sim::{simulate, RegionGraph, Route, SimConfig};
//!
//! // Four regions of 16 sectors, 400 units apart, laid out in route order.
//! let mut graph = RegionGraph::chain(4, 16, 400);
//! graph.layout_in_order(1_000, 0);
//! graph.needs_with_neighbors();
//! // Sprint along them at 375 units per second.
//! let route = Route::walk(&graph, &[0, 1, 2, 3], 375);
//! let config = SimConfig::new(96, 900);
//! let report = simulate(&graph, &route, &config);
//! assert_eq!(report.misses, 0);
//! assert!(report.min_slack_millis > 0);
//! ```

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod churn;
pub mod drive;
pub mod run;
pub mod world;

pub use churn::{churn_report, ChurnConfig, ChurnReport};
pub use drive::{
    DriveModel, Variance, MEASURED_SEEK_MILLIS, READ_MICROS_PER_SECTOR_DOUBLE,
    READ_MICROS_PER_SECTOR_SINGLE,
};
pub use run::{simulate, SimConfig, SimReport, WorstMiss};
pub use world::{RegionGraph, Route, RouteSample, SimRegion, TICKS_PER_SECOND};
