// SPDX-License-Identifier: GPL-2.0-or-later
//! Residency policy for streamed data.
//!
//! A game that streams its world needs the same few decisions again and again:
//! which resources must be in RAM, VRAM or SPU RAM now, which to read next,
//! what to throw out to make room, and what to do when a read fails. This
//! crate makes those decisions and nothing else. It never touches the drive:
//! requests go out through the [`Host`] trait and outcomes come back through
//! [`Residency::complete_read`], so the same policy runs against the
//! interrupt-driven transport on the console and against the drive model in
//! `psx-residency-sim` on the host.
//!
//! # Vocabulary
//!
//! * A [`ResourceKey`] names one streamed resource: a [`Category`] (world
//!   region, collision region, texture page, archetype pack, audio bank, or a
//!   game-defined one) and the engine's dense index inside it.
//! * Each category has a [`CategoryConfig`] budget (pages, entries) and
//!   allocates from a page pool. A [`PagePool`] hands out contiguous runs of
//!   pages and a [`Handle`] that stops resolving the moment its run is freed
//!   or reused; [`PageBuffer`] is the RAM behind a RAM pool.
//! * Every frame the game passes the [`Wanted`] list to [`Residency::step`]:
//!   key, [`Class`] (demand, combat fill, lead, background, audio), walk
//!   distance and an optional deadline. Pinned keys ([`Residency::pin`]) are
//!   wanted too, at their pin class.
//!
//! # The frame loop
//!
//! 1. `step(&wanted, &mut host, &mut evictions)`. Misses are ordered by class,
//!    deadline and distance and issued to the host until the transport is at
//!    depth. A request that arrives while the same resource is already in
//!    flight or landed is adopted, not repeated, and a more urgent class
//!    promotes the request in flight. Room comes from free pages, then from
//!    sliding movable runs down (compaction), then from evicting the cheapest
//!    contiguous window.
//! 2. Apply the returned evictions: unlink the resource from whatever points
//!    at it. Their handles are already stale.
//! 3. The transport reports each request with [`Residency::complete_read`].
//!    A category that installs lands the payload as [`State::Landed`]; the
//!    game calls [`Residency::begin_install`], does its verify, relocate and
//!    upload work in slices, then [`Residency::finish_install`] (or
//!    [`Residency::fail_install`]). Only then is the resource resident.
//! 4. Read resident bytes through `PageBuffer::bytes(residency.try_run(handle)?, len)`.
//!    A handle outlives its data only by turning stale, never by pointing at
//!    someone else's.
//!
//! # Eviction
//!
//! A victim is never pinned, never reading or installing, and never wanted at
//! a class [`Config::protect_wanted`] protects. Among the rest, in order:
//! tier 0 (unwanted, beyond the category's keep radius), tier 1 (unwanted,
//! inside the hysteresis band), tier 2 (wanted, but lower priority than the
//! requester by class, or by more than [`Config::displace_margin`] of distance
//! inside a class). Each requester class has a ceiling tier it may reach
//! ([`Config::max_victim_tier`]): a background fill takes only tier 0, a lead
//! request reaches tier 1, a demand tier 2. Inside a tier the farthest goes
//! first, then the least recently used. Because a run must be contiguous, the
//! engine picks the cheapest window of adjacent evictable runs (lowest worst
//! tier, then fewest victims, then farthest, then oldest), not just the best
//! single victim. Eviction commits inside [`Residency::step`], at the frame
//! boundary, and stops rather than evict what it cannot report.
//!
//! # Budgets and CPU
//!
//! Per-category page and entry budgets are enforced before anything is
//! allocated: a category over budget evicts its own entries, never another
//! category's. A step also has a bookkeeping budget ([`Config::step_budget`])
//! counted in work units: one per wanted entry examined, one per table entry
//! or pool run the eviction search visits, one per sort swap, and
//! `1 + pages / 8` per compaction move. Demand and combat-fill entries are
//! never deferred; the rest stop when the budget runs out and are picked up
//! next window (the wanted list is stateless). The unit is relative; convert
//! it to cycles by measuring on the target.
//!
//! # Failure
//!
//! A failed read or install frees its pages and puts the key in backoff
//! ([`Backoff`]): 16 windows after the first failure, doubling to 512. The
//! key is never abandoned. A cancelled request keeps its pages reserved until
//! the transport reports it finished, because sectors may still be landing.
//!
//! # Other modules
//!
//! [`slots`] is the fixed-slot policy lifted from hk-psx's `room_residency.rs`
//! (private arenas, a wishlist, a stored-then-decoded pipeline) for games that
//! keep a handful of equally sized slots. `psx-residency-sim`, a separate
//! host-only crate, models the drive from the silicon seek and read
//! measurements and replays routes against the real engine to report deadline
//! slack and misses.
//!
//! Unlike `psx-cache`, which caches small values in fixed slots, this crate
//! owns variable-length contiguous page runs and the request lifecycle.
//!
//! ```
//! use psx_residency::*;
//!
//! struct Disc;
//! impl Host for Disc {
//!     fn extent(&self, key: ResourceKey) -> Option<Extent> {
//!         Some(Extent { lba: 100 * key.index(), sectors: 4, pages: 4, bytes: 8000 })
//!     }
//!     fn walk_distance(&self, _: ResourceKey) -> u32 { 0 }
//!     fn submit(&mut self, _: &ReadRequest) -> Submit { Submit::Accepted }
//!     fn cancel(&mut self, _: RequestId) {}
//! }
//!
//! let mut config = Config::<2>::new();
//! config.pools[0] = PoolConfig { capacity_pages: 16, placement: Placement::BestFit };
//! config.categories[0] = CategoryConfig {
//!     pool: 0, max_pages: 16, max_entries: 4, movable: true, installs: false, keep_radius: 300,
//! };
//! let mut residency = Residency::<8, 4, 2>::new(config);
//! let region = ResourceKey::new(Category::WORLD_REGION, 7).unwrap();
//! let mut evictions = [];
//!
//! let report = residency.step(&[Wanted::new(region, Class::Demand, 0)], &mut Disc, &mut evictions);
//! assert_eq!(report.stats.issued, 1);
//! let request = residency.request_of(region).unwrap();
//! residency.complete_read(request, ReadOutcome::Done);
//! assert!(residency.is_resident(region));
//! ```
#![no_std]

#[cfg(test)]
extern crate std;

mod backoff;
mod buffer;
mod config;
mod engine;
mod host;
mod key;
mod pool;
pub mod slots;
#[cfg(test)]
mod tests;

pub use backoff::Backoff;
pub use buffer::PageBuffer;
pub use config::{CategoryConfig, Config, PoolConfig};
pub use engine::{Residency, State, StepReport, Totals, Wanted, WindowStats};
pub use host::{Eviction, Extent, Host, ReadOutcome, ReadRequest, RequestId, Submit};
pub use key::{Category, Class, ResourceKey};
pub use pool::{Fragmentation, Handle, Moved, PagePool, PageRun, Placement, PAGE_BYTES};
