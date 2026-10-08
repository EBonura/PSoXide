//! The edge of the crate: what the engine asks of its host, and what it hands
//! to the transport.

use crate::key::{Class, ResourceKey};
use crate::pool::{Handle, PageRun};

/// Where a resource lives on the disc and how much room it needs.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Extent {
    /// First sector of the resource on the disc.
    pub lba: u32,
    /// Sectors to read.
    pub sectors: u32,
    /// Pages the resource needs in its pool (equal to `sectors` for RAM
    /// pages, one per slot for texture pages).
    pub pages: u32,
    /// Payload length in bytes (at most `pages` pages).
    pub bytes: u32,
}

/// Identifies one read request from submission until its outcome is reported.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct RequestId(pub u32);

/// One read for the transport to perform: `sectors` sectors from `lba` into
/// `run` of the pool named by `handle`, sector `n` landing at page
/// `run.first_page + n`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ReadRequest {
    /// Identity to report the outcome against.
    pub id: RequestId,
    /// The resource being read.
    pub key: ResourceKey,
    /// Priority class at submission (see [`Host::promote`]).
    pub class: Class,
    /// Allocation the sectors land in.
    pub handle: Handle,
    /// The pages of that allocation.
    pub run: PageRun,
    /// First sector on the disc.
    pub lba: u32,
    /// Sector count.
    pub sectors: u32,
    /// Ticks until the data is needed, as the engine's wanted entry said
    /// (`u32::MAX` when unknown). Earliest deadline first inside a class.
    pub deadline_ticks: u32,
    /// An in-flight request that ends exactly at `lba`. A transport can chain
    /// the two without a seek.
    pub contiguous_with: Option<RequestId>,
}

/// A transport's answer to [`Host::submit`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Submit {
    /// The request is queued.
    Accepted,
    /// The queue is full; nothing was queued.
    Full,
}

/// How a request ended, reported with [`crate::Residency::complete_read`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ReadOutcome {
    /// Every sector landed.
    Done,
    /// The read failed (drive error, checksum). The resource backs off.
    Failed,
    /// The transport stopped the request after [`Host::cancel`]; no sector
    /// will land in the pages any more.
    Cancelled,
}

/// Pages freed by an eviction, reported by [`crate::Residency::step`]. Unlink
/// the resource (patch the parent, drop cached views) before using resident
/// data again; the handle is already stale.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Eviction {
    /// The evicted resource.
    pub key: ResourceKey,
    /// Its former allocation (stale now).
    pub handle: Handle,
    /// The pages it held.
    pub run: PageRun,
}

impl Eviction {
    /// A placeholder for initialising an eviction buffer
    /// (`[Eviction::NONE; 8]`); [`crate::Residency::step`] overwrites the
    /// entries it reports.
    pub const NONE: Eviction = Eviction {
        key: match ResourceKey::from_u32(0) {
            Some(key) => key,
            None => panic!("key 0 is valid"),
        },
        handle: Handle::NONE,
        run: PageRun {
            first_page: 0,
            page_count: 0,
        },
    };
}

/// Everything the engine needs from the game, the region graph and the
/// transport. One value implements it; the engine borrows it for a call.
pub trait Host {
    /// Disc position and size of `key`, or `None` when the key is unknown.
    fn extent(&self, key: ResourceKey) -> Option<Extent>;

    /// Walk distance from the viewer to `key`, in the same units as
    /// [`crate::Wanted::distance`]. Asked only for resident resources the
    /// current wanted list did not mention, when eviction needs to rank them.
    fn walk_distance(&self, key: ResourceKey) -> u32;

    /// Whether the transport can take another request right now.
    fn can_accept(&self) -> bool {
        true
    }

    /// Queue `request`. A [`Submit::Full`] answer rolls the allocation back.
    fn submit(&mut self, request: &ReadRequest) -> Submit;

    /// Stop `request`. The transport reports [`ReadOutcome::Cancelled`] (or
    /// [`ReadOutcome::Done`] if the sectors had already landed) through
    /// [`crate::Residency::complete_read`]; the pages stay reserved until then.
    fn cancel(&mut self, request: RequestId);

    /// A request already in flight is now wanted at a more urgent class.
    fn promote(&mut self, _request: RequestId, _class: Class) {}

    /// Move `page_count` pages of `pool` from `from_page` to `to_page` (a
    /// `memmove`). Called only for categories configured as movable.
    fn move_pages(&mut self, _pool: u8, _from_page: u32, _to_page: u32, _page_count: u32) {}
}
