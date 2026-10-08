//! The residency engine: what is wanted, what is resident, what to read next
//! and what to evict to make room.

use crate::config::Config;
use crate::host::{Eviction, Extent, Host, ReadOutcome, ReadRequest, RequestId, Submit};
use crate::key::{Class, ResourceKey};
use crate::pool::{Handle, PagePool, PageRun, Placement};

const NO_ENTRY: usize = usize::MAX;

/// One resource the engine wants resident this frame.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Wanted {
    /// The resource.
    pub key: ResourceKey,
    /// How urgent it is.
    pub class: Class,
    /// Walk distance from the viewer, in the engine's units. Nearer is more
    /// urgent inside a class, and farther entries are evicted first.
    pub distance: u32,
    /// Ticks until the data is needed (`u32::MAX` when unknown). Earlier
    /// deadlines are requested first inside a class.
    pub deadline_ticks: u32,
}

impl Wanted {
    /// A wanted entry with no deadline.
    pub const fn new(key: ResourceKey, class: Class, distance: u32) -> Self {
        Self {
            key,
            class,
            distance,
            deadline_ticks: u32::MAX,
        }
    }

    /// The same entry with a deadline.
    pub const fn with_deadline_ticks(mut self, deadline_ticks: u32) -> Self {
        self.deadline_ticks = deadline_ticks;
        self
    }

    const EMPTY: Wanted = Wanted::new(ResourceKey::from_u32(0).unwrap(), Class::Demand, 0);

    fn order(&self) -> (Class, u32, u32, u32) {
        (
            self.class,
            self.deadline_ticks,
            self.distance,
            self.key.as_u32(),
        )
    }
}

/// Lifecycle of one tracked resource.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    /// Not loaded, no pages (an entry exists only because it is pinned).
    Absent = 0,
    /// A read request is in flight into reserved pages.
    Reading,
    /// The bytes have landed but are not verified or installed yet; nothing
    /// may use them. The grid-free equivalent of the old "stored" state.
    Landed,
    /// An install (verify, relocate, upload) is running on the landed bytes.
    Installing,
    /// Installed and usable.
    Resident,
    /// The last read or install failed; retried after a backoff.
    Failed,
}

/// Counters for one [`Residency::step`] window.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowStats {
    /// Wanted entries examined.
    pub requests: u32,
    /// Of those, entries not resident.
    pub misses: u32,
    /// Wanted entries of class lead or lower.
    pub prefetch_requests: u32,
    /// Reads issued to the transport.
    pub issued: u32,
    /// Wanted entries whose bytes are read, landing or installing.
    pub pending_loads: u32,
    /// Wanted entries that were already in flight or landed though nothing
    /// wanted them last window: their work was adopted, not repeated.
    pub adopted: u32,
    /// In-flight requests promoted to a more urgent class.
    pub promoted: u32,
    /// In-flight requests cancelled to make way for a demand.
    pub cancelled: u32,
    /// Resources evicted.
    pub evictions: u32,
    /// Reads and installs that failed since the previous window.
    pub failed_loads: u32,
    /// Requests that found no room even after evicting every allowed victim.
    pub protected_full: u32,
    /// Times issuing stopped because the transport was full.
    pub transport_full: u32,
    /// Requests skipped because the CPU budget ran out.
    pub deferred: u32,
    /// Misses skipped because the resource is inside its retry backoff.
    pub backoff_skipped: u32,
    /// Wanted keys the host does not know.
    pub unknown_keys: u32,
    /// Wanted keys whose category is not configured.
    pub unconfigured: u32,
    /// Requests that exceed their category's page budget outright.
    pub over_budget: u32,
    /// Requests dropped because the entry table was full.
    pub table_full: u32,
    /// Wanted entries beyond the table capacity, ignored.
    pub wanted_dropped: u32,
    /// Requests held back because the eviction report buffer was too small.
    pub eviction_overflow: u32,
    /// Runs moved by compaction.
    pub compactions: u32,
    /// Pages moved by compaction.
    pub pages_moved: u32,
    /// Bookkeeping work units spent (see the crate docs).
    pub work_used: u32,
}

/// What a [`Residency::step`] did.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct StepReport {
    /// This window's counters.
    pub stats: WindowStats,
    /// Entries written to the eviction buffer.
    pub eviction_count: usize,
}

/// Counters since creation.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    /// Windows stepped.
    pub windows: u32,
    /// Reads issued.
    pub issued: u32,
    /// Reads completed successfully.
    pub completed: u32,
    /// Reads and installs that failed.
    pub failed_loads: u32,
    /// Resources evicted.
    pub evictions: u32,
    /// Evicted resources that were never used since they landed (their read
    /// was wasted).
    pub wasted_evictions: u32,
    /// Requests cancelled.
    pub cancelled: u32,
}

#[derive(Copy, Clone)]
struct Entry {
    key: ResourceKey,
    in_use: bool,
    state: State,
    class: Class,
    pin_class: Class,
    pin_count: u8,
    failures: u8,
    cancel_requested: bool,
    used_since_landed: bool,
    slot: u16,
    request: u32,
    hold_until: u32,
    last_used: u32,
    wanted_epoch: u32,
    distance: u32,
    distance_epoch: u32,
    deadline_ticks: u32,
    bytes: u32,
    lba_end: u32,
}

struct Budget {
    left: u32,
    used: u32,
}

impl Budget {
    fn charge(&mut self, units: u32) {
        self.used = self.used.saturating_add(units);
        self.left = self.left.saturating_sub(units);
    }

    fn is_exhausted(&self) -> bool {
        self.left == 0
    }
}

struct EvictionSink<'a> {
    buffer: &'a mut [Eviction],
    len: usize,
}

#[derive(Copy, Clone)]
struct Requester {
    class: Class,
    distance: u32,
}

/// Cost of clearing a window, smaller is better: worst tier, victim count,
/// inverse of the nearest victim's distance, newest victim's last use, first
/// page.
type WindowCost = (u8, u32, u32, u32, u32);

#[derive(Copy, Clone)]
struct RunInfo {
    first: u32,
    count: u32,
    entry: usize,
    tier: Option<u8>,
    distance: u32,
    last_used: u32,
}

const EMPTY_RUN_INFO: RunInfo = RunInfo {
    first: 0,
    count: 0,
    entry: NO_ENTRY,
    tier: None,
    distance: 0,
    last_used: 0,
};

/// Residency of streamed resources over page pools.
///
/// `ENTRIES` is how many resources it can track at once (resident, in flight,
/// landed, backing off or pinned), `SLOTS` the most simultaneous allocations
/// in any one pool, `CATEGORIES` the number of category numbers and pools. The
/// whole state is plain integers sized by those three constants, so a static
/// lives in `.bss` and nothing allocates.
///
/// See the crate docs for the frame loop. In short: each frame the engine
/// calls [`Residency::step`] with what it wants; the step issues reads through
/// the [`Host`], evicts what must go and reports it; the transport reports
/// outcomes with [`Residency::complete_read`]; the game installs landed
/// payloads with [`Residency::begin_install`] and
/// [`Residency::finish_install`].
pub struct Residency<const ENTRIES: usize, const SLOTS: usize, const CATEGORIES: usize> {
    config: Config<CATEGORIES>,
    pools: [PagePool<SLOTS>; CATEGORIES],
    /// Per pool, per slot: entry index plus one (0 means none).
    owner: [[u16; SLOTS]; CATEGORIES],
    entries: [Entry; ENTRIES],
    category_pages: [u32; CATEGORIES],
    category_entries: [u16; CATEGORIES],
    epoch: u32,
    next_request: u32,
    in_flight: u32,
    failed_mark: u32,
    totals: Totals,
}

impl<const ENTRIES: usize, const SLOTS: usize, const CATEGORIES: usize>
    Residency<ENTRIES, SLOTS, CATEGORIES>
{
    const EMPTY_POOL: PagePool<SLOTS> = PagePool::new(0, 0, Placement::FirstFit);
    const EMPTY_ENTRY: Entry = Entry {
        key: ResourceKey::from_u32(0).unwrap(),
        in_use: false,
        state: State::Absent,
        class: Class::Demand,
        pin_class: Class::Demand,
        pin_count: 0,
        failures: 0,
        cancel_requested: false,
        used_since_landed: false,
        slot: 0,
        request: 0,
        hold_until: 0,
        last_used: 0,
        wanted_epoch: 0,
        distance: 0,
        distance_epoch: 0,
        deadline_ticks: u32::MAX,
        bytes: 0,
        lba_end: 0,
    };

    /// An empty engine for `config`. `const`, so a static can hold it.
    pub const fn new(config: Config<CATEGORIES>) -> Self {
        const { assert!(ENTRIES < u16::MAX as usize && SLOTS < u16::MAX as usize) };
        let mut pools = [Self::EMPTY_POOL; CATEGORIES];
        let mut pool = 0;
        while pool < CATEGORIES {
            pools[pool] = PagePool::new(
                pool as u8,
                config.pools[pool].capacity_pages,
                config.pools[pool].placement,
            );
            pool += 1;
        }
        Self {
            config,
            pools,
            owner: [[0; SLOTS]; CATEGORIES],
            entries: [Self::EMPTY_ENTRY; ENTRIES],
            category_pages: [0; CATEGORIES],
            category_entries: [0; CATEGORIES],
            epoch: 0,
            next_request: 1,
            in_flight: 0,
            failed_mark: 0,
            totals: Totals {
                windows: 0,
                issued: 0,
                completed: 0,
                failed_loads: 0,
                evictions: 0,
                wasted_evictions: 0,
                cancelled: 0,
            },
        }
    }

    /// The configuration.
    pub const fn config(&self) -> &Config<CATEGORIES> {
        &self.config
    }

    /// Counters since creation.
    pub const fn totals(&self) -> Totals {
        self.totals
    }

    /// Pool number `pool`, for fragmentation and occupancy reports.
    pub fn pool(&self, pool: usize) -> Option<&PagePool<SLOTS>> {
        self.pools.get(pool)
    }

    /// Pages `category` currently holds.
    pub fn category_pages(&self, category: crate::Category) -> u32 {
        self.category_pages
            .get(category.number() as usize)
            .copied()
            .unwrap_or(0)
    }

    /// Entries `category` currently holds pages for.
    pub fn category_entry_count(&self, category: crate::Category) -> usize {
        self.category_entries
            .get(category.number() as usize)
            .copied()
            .unwrap_or(0) as usize
    }

    /// Resources currently tracked.
    pub fn tracked_count(&self) -> usize {
        self.entries.iter().filter(|entry| entry.in_use).count()
    }

    /// Requests in flight (including ones being cancelled).
    pub const fn in_flight_count(&self) -> usize {
        self.in_flight as usize
    }

    /// The scheduling window number: how many steps have run.
    pub const fn window(&self) -> u32 {
        self.epoch
    }

    // ---- lookups -------------------------------------------------------

    fn find_entry(&self, key: ResourceKey) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.in_use && entry.key == key)
    }

    /// Lifecycle state of `key` ([`State::Absent`] when untracked).
    pub fn state(&self, key: ResourceKey) -> State {
        self.find_entry(key)
            .map_or(State::Absent, |index| self.entries[index].state)
    }

    /// Whether `key` is installed and usable.
    pub fn is_resident(&self, key: ResourceKey) -> bool {
        self.state(key) == State::Resident
    }

    /// Handle of `key`'s allocation, in any state that holds pages.
    pub fn handle(&self, key: ResourceKey) -> Option<Handle> {
        let entry = &self.entries[self.find_entry(key)?];
        self.entry_handle(entry)
    }

    /// Handle of `key`'s allocation when it is resident.
    pub fn resident_handle(&self, key: ResourceKey) -> Option<Handle> {
        let entry = &self.entries[self.find_entry(key)?];
        (entry.state == State::Resident)
            .then(|| self.entry_handle(entry))
            .flatten()
    }

    /// Pages `handle` names, or `None` when it is stale. The one way to turn a
    /// handle into bytes (with [`crate::PageBuffer::bytes`]); call it on every
    /// use unless the entry is pinned.
    pub fn try_run(&self, handle: Handle) -> Option<PageRun> {
        self.pools.get(handle.pool() as usize)?.try_run(handle)
    }

    /// Payload length in bytes of `key` (0 when untracked).
    pub fn payload_bytes(&self, key: ResourceKey) -> usize {
        self.find_entry(key)
            .map_or(0, |index| self.entries[index].bytes as usize)
    }

    /// The request in flight for `key`.
    pub fn request_of(&self, key: ResourceKey) -> Option<RequestId> {
        let entry = &self.entries[self.find_entry(key)?];
        (entry.state == State::Reading).then_some(RequestId(entry.request))
    }

    fn entry_handle(&self, entry: &Entry) -> Option<Handle> {
        let pool = self.pool_of_key(entry.key)?;
        self.pools[pool].handle_of_slot(entry.slot)
    }

    fn pool_of_key(&self, key: ResourceKey) -> Option<usize> {
        let category = key.category().number() as usize;
        let config = self.config.categories.get(category)?;
        (config.max_entries > 0 && (config.pool as usize) < CATEGORIES)
            .then_some(config.pool as usize)
    }

    fn holds_pages(state: State) -> bool {
        matches!(
            state,
            State::Reading | State::Landed | State::Installing | State::Resident
        )
    }

    fn hold_active(&self, entry: &Entry) -> bool {
        entry.failures > 0 && (entry.hold_until.wrapping_sub(self.epoch) as i32) > 0
    }

    // ---- pins and use --------------------------------------------------

    /// Pin `key`: it cannot be evicted or moved while pinned, and the next
    /// steps request it at `class` if it is not resident. Pins nest (up to
    /// 255). `false` when the category is not configured, the table is full or the pin count is saturated.
    pub fn pin(&mut self, key: ResourceKey, class: Class) -> bool {
        if self.pool_of_key(key).is_none() {
            return false;
        }
        let Some(index) = self.ensure_entry(key) else {
            return false;
        };
        let entry = &mut self.entries[index];
        if entry.pin_count == u8::MAX {
            return false;
        }
        entry.pin_class = if entry.pin_count == 0 {
            class
        } else {
            entry.pin_class.min(class)
        };
        entry.pin_count += 1;
        self.sync_lock(index);
        true
    }

    /// Drop one pin on `key`. `false` when it was not pinned.
    pub fn unpin(&mut self, key: ResourceKey) -> bool {
        let Some(index) = self.find_entry(key) else {
            return false;
        };
        if self.entries[index].pin_count == 0 {
            return false;
        }
        self.entries[index].pin_count -= 1;
        self.sync_lock(index);
        self.release_if_idle(index);
        true
    }

    /// Pins held on `key`.
    pub fn pin_count(&self, key: ResourceKey) -> u8 {
        self.find_entry(key)
            .map_or(0, |index| self.entries[index].pin_count)
    }

    /// Mark `key` as used now (refreshes its LRU age). Resident entries that
    /// the wanted list names are refreshed by [`Residency::step`] already.
    pub fn touch(&mut self, key: ResourceKey) {
        if let Some(index) = self.find_entry(key) {
            let entry = &mut self.entries[index];
            entry.last_used = self.epoch;
            entry.used_since_landed = true;
        }
    }

    fn sync_lock(&mut self, index: usize) {
        let entry = self.entries[index];
        if !Self::holds_pages(entry.state) {
            return;
        }
        let Some(pool) = self.pool_of_key(entry.key) else {
            return;
        };
        let category = entry.key.category().number() as usize;
        let locked = matches!(entry.state, State::Reading | State::Installing)
            || entry.pin_count > 0
            || !self.config.categories[category].movable;
        if let Some(handle) = self.pools[pool].handle_of_slot(entry.slot) {
            self.pools[pool].set_locked(handle, locked);
        }
    }

    // ---- table management ------------------------------------------------

    fn ensure_entry(&mut self, key: ResourceKey) -> Option<usize> {
        if let Some(index) = self.find_entry(key) {
            return Some(index);
        }
        let free = self
            .entries
            .iter()
            .position(|entry| !entry.in_use)
            .or_else(|| {
                // Reclaim a backed-off entry nothing wants, expired holds first.
                let mut best: Option<(bool, usize)> = None;
                for (index, entry) in self.entries.iter().enumerate() {
                    if entry.state == State::Failed
                        && entry.pin_count == 0
                        && entry.wanted_epoch != self.epoch
                    {
                        let expired = !self.hold_active(entry);
                        if best.is_none_or(|(best_expired, _)| expired && !best_expired) {
                            best = Some((expired, index));
                        }
                    }
                }
                best.map(|(_, index)| index)
            })?;
        self.entries[free] = Self::EMPTY_ENTRY;
        self.entries[free].in_use = true;
        self.entries[free].key = key;
        Some(free)
    }

    fn release_if_idle(&mut self, index: usize) {
        let entry = &self.entries[index];
        if entry.in_use
            && entry.state == State::Absent
            && entry.pin_count == 0
            && entry.failures == 0
        {
            self.entries[index] = Self::EMPTY_ENTRY;
        }
    }

    /// Free the pages of `index` and clear its slot bookkeeping. Returns the
    /// handle and run it held.
    fn free_pages_of(&mut self, index: usize) -> Option<(Handle, PageRun)> {
        let entry = self.entries[index];
        let pool = self.pool_of_key(entry.key)?;
        let handle = self.pools[pool].handle_of_slot(entry.slot)?;
        let run = self.pools[pool].try_run(handle)?;
        self.pools[pool].free(handle);
        self.owner[pool][entry.slot as usize] = 0;
        let category = entry.key.category().number() as usize;
        self.category_pages[category] -= run.page_count;
        self.category_entries[category] -= 1;
        Some((handle, run))
    }

    // ---- transport and install callbacks ---------------------------------

    /// The transport finished `request`. Returns `false` when the id is not an
    /// in-flight request (already reported, or never issued).
    ///
    /// A `Done` that arrives after the engine asked to cancel is kept: the
    /// bytes are good, so the cancelled work is adopted.
    pub fn complete_read(&mut self, request: RequestId, outcome: ReadOutcome) -> bool {
        let Some(index) = self.entries.iter().position(|entry| {
            entry.in_use && entry.state == State::Reading && entry.request == request.0
        }) else {
            return false;
        };
        self.in_flight -= 1;
        self.entries[index].cancel_requested = false;
        match outcome {
            ReadOutcome::Done => {
                let category = self.entries[index].key.category().number() as usize;
                let installs = self.config.categories[category].installs;
                let entry = &mut self.entries[index];
                entry.failures = 0;
                entry.hold_until = self.epoch;
                entry.state = if installs {
                    State::Landed
                } else {
                    State::Resident
                };
                entry.last_used = self.epoch;
                entry.used_since_landed = false;
                self.totals.completed += 1;
                self.sync_lock(index);
            }
            ReadOutcome::Failed => self.fail_entry(index),
            ReadOutcome::Cancelled => {
                self.totals.cancelled += 1;
                self.free_pages_of(index);
                self.entries[index].state = State::Absent;
                self.release_if_idle(index);
            }
        }
        true
    }

    fn fail_entry(&mut self, index: usize) {
        self.free_pages_of(index);
        let backoff = self.config.backoff;
        let epoch = self.epoch;
        let entry = &mut self.entries[index];
        entry.failures = entry.failures.saturating_add(1);
        entry.hold_until = epoch.wrapping_add(backoff.windows(entry.failures));
        entry.state = State::Failed;
        self.totals.failed_loads += 1;
    }

    /// Start installing the landed payload of `key`: it is held in place and
    /// invisible until [`Residency::finish_install`]. Returns the allocation to
    /// install into, or `None` when `key` has not landed.
    pub fn begin_install(&mut self, key: ResourceKey) -> Option<Handle> {
        let index = self.find_entry(key)?;
        if self.entries[index].state != State::Landed {
            return None;
        }
        self.entries[index].state = State::Installing;
        self.sync_lock(index);
        let entry = self.entries[index];
        self.entry_handle(&entry)
    }

    /// The install succeeded: `key` is resident.
    pub fn finish_install(&mut self, key: ResourceKey) -> bool {
        let Some(index) = self.find_entry(key) else {
            return false;
        };
        if self.entries[index].state != State::Installing {
            return false;
        }
        let entry = &mut self.entries[index];
        entry.state = State::Resident;
        entry.last_used = self.epoch;
        self.sync_lock(index);
        true
    }

    /// Abandon the install without blame (for example the payload is no longer
    /// wanted): the pages are freed and the resource is simply absent.
    pub fn abort_install(&mut self, key: ResourceKey) -> bool {
        let Some(index) = self.find_entry(key) else {
            return false;
        };
        if self.entries[index].state != State::Installing {
            return false;
        }
        self.free_pages_of(index);
        self.entries[index].state = State::Absent;
        self.release_if_idle(index);
        true
    }

    /// The install failed (bad checksum, malformed payload): the pages are
    /// freed and the resource backs off before the next read.
    pub fn fail_install(&mut self, key: ResourceKey) -> bool {
        let Some(index) = self.find_entry(key) else {
            return false;
        };
        if self.entries[index].state != State::Installing {
            return false;
        }
        self.fail_entry(index);
        true
    }

    /// The landed payload to install next: `demand` if it has landed,
    /// otherwise the most urgent landed payload the last step wanted.
    pub fn next_install(&self, demand: Option<ResourceKey>) -> Option<ResourceKey> {
        if let Some(key) = demand {
            if self.state(key) == State::Landed {
                return Some(key);
            }
        }
        self.entries
            .iter()
            .filter(|entry| {
                entry.in_use && entry.state == State::Landed && entry.wanted_epoch == self.epoch
            })
            .min_by_key(|entry| {
                (
                    entry.class,
                    entry.deadline_ticks,
                    entry.distance,
                    entry.key.as_u32(),
                )
            })
            .map(|entry| entry.key)
    }

    /// Evict every resident or landed, unpinned resource not in `keep`.
    /// Returns how many were evicted; stops early when `evictions` is full
    /// (call again). Reading and installing entries are never touched.
    pub fn evict_unlisted(&mut self, keep: &[ResourceKey], evictions: &mut [Eviction]) -> usize {
        let mut sink = EvictionSink {
            buffer: evictions,
            len: 0,
        };
        for index in 0..ENTRIES {
            let entry = self.entries[index];
            if !entry.in_use
                || !matches!(entry.state, State::Resident | State::Landed)
                || entry.pin_count > 0
                || keep.contains(&entry.key)
            {
                continue;
            }
            if sink.len == sink.buffer.len() {
                break;
            }
            self.evict_entry(index, &mut sink);
        }
        sink.len
    }

    fn evict_entry(&mut self, index: usize, sink: &mut EvictionSink<'_>) {
        let entry = self.entries[index];
        if let Some((handle, run)) = self.free_pages_of(index) {
            sink.buffer[sink.len] = Eviction {
                key: entry.key,
                handle,
                run,
            };
            sink.len += 1;
        }
        self.totals.evictions += 1;
        if !entry.used_since_landed {
            self.totals.wasted_evictions += 1;
        }
        self.entries[index].state = State::Absent;
        self.entries[index].failures = 0;
        self.release_if_idle(index);
    }

    // ---- the frame step ------------------------------------------------------

    /// Run one scheduling window.
    ///
    /// `wanted` is everything the game wants resident now, in any order; the
    /// engine orders it by class, deadline and distance. Pinned resources are
    /// added at their pin class. Misses are issued to `host` nearest first;
    /// room is made by moving pages or by evicting; evictions are written to
    /// `evictions` and the step never evicts more than fits there. Apply them
    /// (unlink, drop cached views) before reading resident data again.
    pub fn step<H: Host>(
        &mut self,
        wanted: &[Wanted],
        host: &mut H,
        evictions: &mut [Eviction],
    ) -> StepReport {
        self.epoch = self.epoch.wrapping_add(1).max(1);
        self.totals.windows += 1;
        let mut stats = WindowStats {
            failed_loads: self.totals.failed_loads - self.failed_mark,
            ..WindowStats::default()
        };
        self.failed_mark = self.totals.failed_loads;
        let mut budget = Budget {
            left: self.config.step_budget,
            used: 0,
        };
        let mut sink = EvictionSink {
            buffer: evictions,
            len: 0,
        };

        // Gather: pins first (they must not be truncated), then the list.
        let mut items = [Wanted::EMPTY; ENTRIES];
        let mut count = 0usize;
        for entry in &self.entries {
            if entry.in_use && entry.pin_count > 0 && count < ENTRIES {
                items[count] = Wanted::new(entry.key, entry.pin_class, 0);
                count += 1;
            }
        }
        for want in wanted {
            let configured = self
                .config
                .categories
                .get(want.key.category().number() as usize)
                .is_some_and(|category| category.max_entries > 0);
            if !configured {
                stats.unconfigured += 1;
            } else if count == ENTRIES {
                stats.wanted_dropped += 1;
            } else {
                items[count] = *want;
                count += 1;
            }
        }
        budget.charge(count as u32);
        // Insertion sort: nearly sorted input (the usual case) costs O(n).
        for i in 1..count {
            let mut j = i;
            while j > 0 && items[j].order() < items[j - 1].order() {
                items.swap(j, j - 1);
                j -= 1;
                budget.charge(1);
            }
        }

        // Pass 1: mark entries wanted, count misses, adopt, promote.
        let mut needs_issue = [false; ENTRIES];
        for n in 0..count {
            let want = items[n];
            let exempt = want.class <= Class::CombatFill;
            if !exempt && budget.is_exhausted() {
                stats.deferred += 1;
                continue;
            }
            budget.charge(1);
            stats.requests += 1;
            if want.class >= Class::Lead {
                stats.prefetch_requests += 1;
            }
            let Some(index) = self.find_entry(want.key) else {
                stats.misses += 1;
                needs_issue[n] = true;
                continue;
            };
            let epoch = self.epoch;
            let first_mark = self.entries[index].wanted_epoch != epoch;
            if first_mark {
                let entry = self.entries[index];
                let wanted_last_window = entry.wanted_epoch.wrapping_add(1) == epoch;
                if matches!(
                    entry.state,
                    State::Reading | State::Landed | State::Installing
                ) && !wanted_last_window
                {
                    stats.adopted += 1;
                }
                if entry.state == State::Reading && want.class < entry.class {
                    host.promote(RequestId(entry.request), want.class);
                    stats.promoted += 1;
                }
                let entry = &mut self.entries[index];
                entry.class = if entry.state == State::Reading {
                    entry.class.min(want.class)
                } else {
                    want.class
                };
                entry.wanted_epoch = epoch;
                entry.distance = want.distance;
                entry.distance_epoch = epoch;
                entry.deadline_ticks = want.deadline_ticks;
            }
            let entry = self.entries[index];
            match entry.state {
                State::Resident => {
                    let entry = &mut self.entries[index];
                    entry.last_used = epoch;
                    entry.used_since_landed = true;
                }
                State::Reading | State::Landed | State::Installing => {
                    stats.misses += 1;
                    stats.pending_loads += 1;
                }
                State::Failed => {
                    stats.misses += 1;
                    if self.hold_active(&entry) {
                        stats.backoff_skipped += 1;
                    } else if first_mark {
                        needs_issue[n] = true;
                    }
                }
                State::Absent => {
                    stats.misses += 1;
                    if first_mark {
                        needs_issue[n] = true;
                    }
                }
            }
        }

        // Pass 2: issue reads, most urgent first.
        for n in 0..count {
            if !needs_issue[n] {
                continue;
            }
            let want = items[n];
            let exempt = want.class <= Class::CombatFill;
            if !exempt && budget.is_exhausted() {
                stats.deferred += 1;
                continue;
            }
            if let Some(index) = self.find_entry(want.key) {
                let entry = self.entries[index];
                let idle = matches!(entry.state, State::Absent)
                    || (entry.state == State::Failed && !self.hold_active(&entry));
                if !idle {
                    continue;
                }
            }
            if self.active_in_flight() >= self.config.max_in_flight as usize
                && !(self.config.cancel_for_demand
                    && want.class == Class::Demand
                    && self.cancel_least_urgent(want, host, &mut stats))
            {
                stats.transport_full += 1;
                break;
            }
            if !host.can_accept() {
                stats.transport_full += 1;
                break;
            }
            let Some(extent) = host.extent(want.key) else {
                stats.unknown_keys += 1;
                continue;
            };
            budget.charge(1);
            let Some(index) = self.ensure_entry(want.key) else {
                stats.table_full += 1;
                continue;
            };
            let requester = Requester {
                class: want.class,
                distance: want.distance,
            };
            let Some((handle, run)) = self.place(
                want.key,
                requester,
                extent,
                host,
                &mut sink,
                &mut budget,
                &mut stats,
            ) else {
                stats.protected_full += 1;
                self.release_if_idle(index);
                continue;
            };
            let id = RequestId(self.next_request);
            self.next_request = self.next_request.wrapping_add(1).max(1);
            let contiguous_with = self
                .entries
                .iter()
                .find(|other| {
                    other.in_use
                        && other.state == State::Reading
                        && !other.cancel_requested
                        && other.lba_end == extent.lba
                })
                .map(|other| RequestId(other.request));
            let request = ReadRequest {
                id,
                key: want.key,
                class: want.class,
                handle,
                run,
                lba: extent.lba,
                sectors: extent.sectors,
                deadline_ticks: want.deadline_ticks,
                contiguous_with,
            };
            if host.submit(&request) == Submit::Full {
                let pool = handle.pool() as usize;
                self.pools[pool].free(handle);
                stats.transport_full += 1;
                self.release_if_idle(index);
                break;
            }
            let pool = handle.pool() as usize;
            let category = want.key.category().number() as usize;
            self.owner[pool][handle.slot() as usize] = index as u16 + 1;
            self.category_pages[category] += run.page_count;
            self.category_entries[category] += 1;
            self.in_flight += 1;
            self.totals.issued += 1;
            stats.issued += 1;
            let epoch = self.epoch;
            let entry = &mut self.entries[index];
            entry.state = State::Reading;
            entry.slot = handle.slot();
            entry.class = want.class;
            entry.request = id.0;
            entry.bytes = extent.bytes;
            entry.lba_end = extent.lba + extent.sectors;
            entry.cancel_requested = false;
            entry.used_since_landed = false;
            entry.last_used = epoch;
            entry.wanted_epoch = epoch;
            entry.distance = want.distance;
            entry.distance_epoch = epoch;
            entry.deadline_ticks = want.deadline_ticks;
            self.sync_lock(index);
        }

        stats.work_used = budget.used;
        StepReport {
            stats: WindowStats {
                evictions: sink.len as u32,
                ..stats
            },
            eviction_count: sink.len,
        }
    }

    fn active_in_flight(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| {
                entry.in_use && entry.state == State::Reading && !entry.cancel_requested
            })
            .count()
    }

    fn cancel_least_urgent<H: Host>(
        &mut self,
        demand: Wanted,
        host: &mut H,
        stats: &mut WindowStats,
    ) -> bool {
        let mut worst: Option<(usize, (Class, u32, u32))> = None;
        for (index, entry) in self.entries.iter().enumerate() {
            if !entry.in_use
                || entry.state != State::Reading
                || entry.cancel_requested
                || entry.pin_count > 0
            {
                continue;
            }
            if entry.class <= demand.class {
                continue;
            }
            let rank = (entry.class, entry.deadline_ticks, entry.distance);
            if worst.is_none_or(|(_, best)| rank > best) {
                worst = Some((index, rank));
            }
        }
        let Some((index, _)) = worst else {
            return false;
        };
        host.cancel(RequestId(self.entries[index].request));
        self.entries[index].cancel_requested = true;
        stats.cancelled += 1;
        true
    }

    // ---- placement and eviction ------------------------------------------------

    fn entry_pages(&self, index: usize) -> u32 {
        let entry = &self.entries[index];
        self.pool_of_key(entry.key)
            .and_then(|pool| self.pools[pool].handle_of_slot(entry.slot))
            .and_then(|handle| self.try_run(handle))
            .map_or(0, |run| run.page_count)
    }

    fn distance_of<H: Host>(&mut self, index: usize, host: &H, budget: &mut Budget) -> u32 {
        let entry = &mut self.entries[index];
        if entry.distance_epoch != self.epoch {
            budget.charge(1);
            entry.distance = host.walk_distance(entry.key);
            entry.distance_epoch = self.epoch;
        }
        entry.distance
    }

    /// The eviction tier of `index` for `requester`, or `None` when it may
    /// not be evicted for it. 0: unwanted and beyond the keep radius. 1:
    /// unwanted inside the hysteresis band. 2: wanted this window, but at a
    /// lower priority than the requester.
    fn victim_tier<H: Host>(
        &mut self,
        index: usize,
        requester: Requester,
        host: &H,
        budget: &mut Budget,
    ) -> Option<u8> {
        let entry = self.entries[index];
        if !entry.in_use
            || !matches!(entry.state, State::Resident | State::Landed)
            || entry.pin_count > 0
        {
            return None;
        }
        let tier = if entry.wanted_epoch == self.epoch {
            if self.config.protect_wanted[entry.class.as_index()] {
                return None;
            }
            let lower = entry.class > requester.class
                || (entry.class == requester.class
                    && entry.distance
                        > requester
                            .distance
                            .saturating_add(self.config.displace_margin));
            if !lower {
                return None;
            }
            2
        } else {
            let distance = self.distance_of(index, host, budget);
            let category = entry.key.category().number() as usize;
            if distance > self.config.categories[category].keep_radius {
                0
            } else {
                1
            }
        };
        (tier <= self.config.max_victim_tier[requester.class.as_index()]).then_some(tier)
    }

    /// The best victim among entries of `category` not already `chosen`:
    /// lowest tier, then farthest, then least recently used, then lowest key.
    fn best_victim<H: Host>(
        &mut self,
        category: usize,
        requester: Requester,
        chosen: &[bool; ENTRIES],
        host: &H,
        budget: &mut Budget,
    ) -> Option<usize> {
        let mut best: Option<(usize, (u8, u32, u32, u32))> = None;
        for (index, &taken) in chosen.iter().enumerate() {
            let entry = self.entries[index];
            if !entry.in_use
                || taken
                || entry.key.category().number() as usize != category
                || !Self::holds_pages(entry.state)
            {
                continue;
            }
            budget.charge(1);
            let Some(tier) = self.victim_tier(index, requester, host, budget) else {
                continue;
            };
            let rank = (
                tier,
                u32::MAX - self.entries[index].distance,
                entry.last_used,
                entry.key.as_u32(),
            );
            if best.is_none_or(|(_, best_rank)| rank < best_rank) {
                best = Some((index, rank));
            }
        }
        best.map(|(index, _)| index)
    }

    /// Find or make room for `extent.pages` contiguous pages for `key`.
    #[allow(clippy::too_many_arguments)]
    fn place<H: Host>(
        &mut self,
        key: ResourceKey,
        requester: Requester,
        extent: Extent,
        host: &mut H,
        sink: &mut EvictionSink<'_>,
        budget: &mut Budget,
        stats: &mut WindowStats,
    ) -> Option<(Handle, PageRun)> {
        let category = key.category().number() as usize;
        let config = self.config.categories[category];
        let pool_number = self.pool_of_key(key)?;
        let pages = extent.pages;
        if pages == 0
            || pages > config.max_pages
            || pages > self.pools[pool_number].capacity_pages()
        {
            stats.over_budget += 1;
            return None;
        }

        // 1. Category budget: choose own-category victims until it fits.
        let mut chosen = [false; ENTRIES];
        let mut chosen_count = 0usize;
        let mut held_pages = self.category_pages[category];
        let mut held_entries = self.category_entries[category];
        while held_pages + pages > config.max_pages || held_entries >= config.max_entries {
            let victim = self.best_victim(category, requester, &chosen, host, budget)?;
            chosen[victim] = true;
            chosen_count += 1;
            held_pages -= self.entry_pages(victim);
            held_entries -= 1;
        }

        // 2. Free room already fits.
        if chosen_count == 0 && self.pools[pool_number].live_count() < SLOTS {
            if let Some(allocation) = self.pools[pool_number].allocate(pages) {
                return Some(allocation);
            }
            // 3. Enough free pages but scattered: slide movable runs down.
            while self.pools[pool_number].free_pages() >= pages
                && self.pools[pool_number].find_fit(pages).is_none()
                && !budget.is_exhausted()
            {
                let moved = self.pools[pool_number].compact_step(|from, to| {
                    host.move_pages(pool_number as u8, from.first_page, to, from.page_count);
                });
                let Some(moved) = moved else { break };
                stats.compactions += 1;
                stats.pages_moved += moved.from.page_count;
                budget.charge(1 + moved.from.page_count / 8);
            }
            if let Some(allocation) = self.pools[pool_number].allocate(pages) {
                return Some(allocation);
            }
        }

        // 4. Evict: the cheapest contiguous window of evictable runs.
        let need_slot = self.pools[pool_number].live_count() >= SLOTS && chosen_count == 0;
        let mut info = [EMPTY_RUN_INFO; SLOTS];
        let mut live = 0usize;
        for (slot, run, _) in self.pools[pool_number].iter_runs() {
            let owner = self.owner[pool_number][slot as usize] as usize;
            info[live] = RunInfo {
                first: run.first_page,
                count: run.page_count,
                entry: if owner == 0 { NO_ENTRY } else { owner - 1 },
                tier: None,
                distance: 0,
                last_used: 0,
            };
            live += 1;
        }
        for run in info.iter_mut().take(live) {
            if run.entry == NO_ENTRY {
                continue;
            }
            // `chosen` entries are going anyway: free of charge.
            let tier = if chosen[run.entry] {
                Some(0)
            } else {
                self.victim_tier(run.entry, requester, host, budget)
            };
            run.tier = tier;
            run.distance = self.entries[run.entry].distance;
            run.last_used = self.entries[run.entry].last_used;
        }
        let capacity = self.pools[pool_number].capacity_pages();
        let mut best: Option<(WindowCost, usize, usize)> = None;
        for start in 0..=live {
            let left = if start == 0 {
                0
            } else {
                info[start - 1].first + info[start - 1].count
            };
            let mut end = start;
            let mut max_tier = 0u8;
            let mut min_distance = u32::MAX;
            let mut max_used = 0u32;
            loop {
                budget.charge(1);
                let right = if end < live {
                    info[end].first
                } else {
                    capacity
                };
                let victims = end - start;
                if right - left >= pages && (victims > 0 || !need_slot) {
                    let cost = (
                        max_tier,
                        victims as u32,
                        if victims == 0 {
                            0
                        } else {
                            u32::MAX - min_distance
                        },
                        max_used,
                        left,
                    );
                    if best.is_none_or(|(best_cost, _, _)| cost < best_cost) {
                        best = Some((cost, start, end));
                    }
                    break;
                }
                if end >= live {
                    break;
                }
                let Some(tier) = info[end].tier else { break };
                max_tier = max_tier.max(tier);
                min_distance = min_distance.min(info[end].distance);
                max_used = max_used.max(info[end].last_used);
                end += 1;
            }
        }
        let (_, start, end) = best?;
        for run in &info[start..end] {
            if !chosen[run.entry] {
                chosen[run.entry] = true;
                chosen_count += 1;
            }
        }
        if chosen_count > sink.buffer.len() - sink.len {
            stats.eviction_overflow += 1;
            return None;
        }
        for (index, &victim) in chosen.iter().enumerate() {
            if victim {
                self.evict_entry(index, sink);
            }
        }
        let allocation = self.pools[pool_number].allocate(pages);
        debug_assert!(allocation.is_some(), "a window was cleared for this run");
        allocation
    }

    /// Check every cross-structure invariant (host tests and debug builds).
    #[cfg(any(test, debug_assertions))]
    pub fn check_invariants(&self) {
        for pool in &self.pools {
            pool.check_invariants();
        }
        let mut pages = [0u32; CATEGORIES];
        let mut entries = [0u16; CATEGORIES];
        let mut reading = 0u32;
        for (index, entry) in self.entries.iter().enumerate() {
            if !entry.in_use {
                continue;
            }
            if entry.state == State::Reading {
                reading += 1;
            }
            if Self::holds_pages(entry.state) {
                let pool = self.pool_of_key(entry.key).expect("a configured category");
                let handle = self.pools[pool]
                    .handle_of_slot(entry.slot)
                    .expect("entry holds a live slot");
                let run = self.pools[pool].try_run(handle).unwrap();
                assert_eq!(self.owner[pool][entry.slot as usize] as usize, index + 1);
                let category = entry.key.category().number() as usize;
                pages[category] += run.page_count;
                entries[category] += 1;
                assert!(run.page_count >= entry.bytes.div_ceil(crate::PAGE_BYTES as u32));
            } else {
                assert!(
                    entry.state != State::Resident,
                    "resident entry without pages"
                );
            }
            // No two entries share a key.
            for other in &self.entries[index + 1..] {
                assert!(!(other.in_use && other.key == entry.key), "duplicate key");
            }
        }
        assert_eq!(pages, self.category_pages);
        assert_eq!(entries, self.category_entries);
        assert_eq!(reading, self.in_flight);
        for category in 0..CATEGORIES {
            let config = self.config.categories[category];
            assert!(self.category_pages[category] <= config.max_pages);
            assert!(self.category_entries[category] <= config.max_entries);
        }
        for (number, pool) in self.pools.iter().enumerate() {
            assert!(pool.used_pages() <= pool.capacity_pages());
            let live = self.owner[number]
                .iter()
                .enumerate()
                .filter(|(slot, owner)| **owner != 0 && pool.handle_of_slot(*slot as u16).is_some())
                .count();
            assert_eq!(live, pool.live_count(), "owner map out of step");
        }
    }
}
