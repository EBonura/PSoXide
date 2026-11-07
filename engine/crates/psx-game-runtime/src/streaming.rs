//! Streaming primitives that are not tied to any world format.
//!
//! [`page_pool`] is the sector-page RAM pool, [`scheduler_policy`] the
//! backoff, pin, generation and counter pieces of a residency scheduler. Both
//! were salvaged from the grid-world room streamer before it was deleted.

pub mod page_pool;
pub mod scheduler_policy;
