//! The streamed-world region streamer (design 2026-10-08, M7): reads each
//! region of a streamed PXBSP world off the disc and installs it into the
//! resident map, with every step bounded so it can run beside a frame.
//!
//! One region at a time moves through four stages:
//!
//! 1. **Read.** The region's sectors come off the disc through the shared
//!    `psx-cdstream` transport ([`RegionRead`]), a few per pump.
//! 2. **Verify and relocate.** Each landed sector goes straight into
//!    [`PxbspResidentMap::install_feed`]: the running FNV-1a, the per-record
//!    checks and the slot-base relocation all happen on the sector in the
//!    transport's staging RAM, so the payload never needs a RAM copy. The
//!    work per pump is bounded by the sector budget.
//! 3. **Textures.** The materials the region's faces use are queued for VRAM
//!    through the caller's callback (the VRAM runtime, behind the GPU access
//!    guard); the region waits until they are all resident.
//! 4. **Link.** One halfword in each of three top trees makes the region
//!    visible, in the same call that saw the textures ready.
//!
//! Regions install in disc order (one seek each) into the slot of the same
//! number, so the resident face order equals the whole-map order whatever the
//! disc order is. There is no eviction yet.

use psx_bsp::pxbsp_resident::stream::{RegionEntry, RegionInstall, StreamError};
use psx_bsp::pxbsp_resident::PxbspResidentMap;

use crate::cd_stream::{CdController, RegionRead, RegionReadProgress};

/// Most regions one streamer tracks.
pub const MAX_STREAM_REGIONS: usize = 64;

/// A region that fails to read or install is retried this many times before
/// the streamer gives up.
pub const REGION_RETRIES: u8 = 3;

/// Counters for the gates; all monotonic.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RegionStreamStats {
    /// Regions linked into the map.
    pub installed: u32,
    /// Sectors handed to the installer.
    pub sectors: u32,
    /// Pumps that did any work.
    pub steps: u32,
    /// Regions restarted after a failed read or install.
    pub retries: u32,
    /// Last failure code (0 when none): a `cd_stream` status or
    /// [`stream_error_code`].
    pub last_error: u32,
    /// Pumps that found the region staged but its textures not yet resident.
    pub texture_waits: u32,
    /// Times a region's read started its run over after the run failed (the
    /// read's own restarts, not counted in `retries`).
    pub read_restarts: u32,
    /// Pumps each region took from its first read to its link, in install
    /// order (the first [`PUMP_HISTORY`] regions).
    pub region_pumps: [u16; PUMP_HISTORY],
}

/// Regions whose install latency [`RegionStreamStats::region_pumps`] keeps.
pub const PUMP_HISTORY: usize = 16;

/// What a pump left the streamer in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamStatus {
    /// More regions to read, install or admit.
    Working,
    /// Every region is linked.
    Complete,
    /// A region failed [`REGION_RETRIES`] times; the code is the last error.
    Failed(u32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Idle,
    Reading,
    Staged,
    Complete,
    Failed(u32),
}

/// A small stable code for a [`StreamError`], disjoint from the
/// `cd_stream` status codes (which stay below 100).
pub fn stream_error_code(error: StreamError) -> u32 {
    100 + match error {
        StreamError::BadIndex(_) => 1,
        StreamError::BadRegion(_) => 2,
        StreamError::BadReference(_) => 3,
        StreamError::BadChecksum => 4,
        StreamError::ShortPayload => 5,
        StreamError::RegionOutOfRange => 6,
        StreamError::SlotOutOfRange => 7,
        StreamError::SlotBusy => 8,
        StreamError::AlreadyInstalled => 9,
        StreamError::NotInstalled => 10,
        StreamError::ExceedsSlot(_) => 11,
        StreamError::NotStreamed => 12,
    }
}

/// Reads and installs every region of a streamed world, one at a time.
pub struct RegionStreamer {
    phase: Phase,
    read: RegionRead,
    job: Option<RegionInstall>,
    /// Region ids in disc order.
    order: [u16; MAX_STREAM_REGIONS],
    count: usize,
    next: usize,
    retries: u8,
    /// Pumps since the region in flight started.
    pumps: u16,
    /// Restarts of the region's read already added to the stats.
    restarts_seen: u16,
    stats: RegionStreamStats,
}

impl RegionStreamer {
    /// An idle streamer; [`Self::begin`] arms it.
    pub const fn new() -> Self {
        Self {
            phase: Phase::Idle,
            read: RegionRead::new(),
            job: None,
            order: [0; MAX_STREAM_REGIONS],
            count: 0,
            next: 0,
            retries: 0,
            pumps: 0,
            restarts_seen: 0,
            stats: RegionStreamStats {
                installed: 0,
                sectors: 0,
                steps: 0,
                retries: 0,
                last_error: 0,
                texture_waits: 0,
                read_restarts: 0,
                region_pumps: [0; PUMP_HISTORY],
            },
        }
    }

    /// Arm the streamer for a freshly loaded streamed map: every region, in
    /// disc order. Returns `false` for a map that is not streamed or has more
    /// than [`MAX_STREAM_REGIONS`] regions.
    pub fn begin(&mut self, map: &PxbspResidentMap) -> bool {
        *self = Self::new();
        let Some(state) = map.streaming() else {
            return false;
        };
        let regions = &state.index().regions;
        if regions.len() > MAX_STREAM_REGIONS {
            return false;
        }
        self.count = regions.len();
        let mut i = 0;
        while i < self.count {
            // Insertion by first sector: stable, and regions are few.
            let sector = regions[i].sector_start;
            let mut at = i;
            while at > 0 && regions[self.order[at - 1] as usize].sector_start > sector {
                self.order[at] = self.order[at - 1];
                at -= 1;
            }
            self.order[at] = i as u16;
            i += 1;
        }
        self.phase = if self.count == 0 {
            Phase::Complete
        } else {
            Phase::Idle
        };
        true
    }

    /// Whether every region is linked.
    pub const fn is_complete(&self) -> bool {
        matches!(self.phase, Phase::Complete)
    }

    /// Counters since [`Self::begin`].
    pub const fn stats(&self) -> &RegionStreamStats {
        &self.stats
    }

    /// Regions installed out of the total, as a Q12 fraction.
    pub fn progress_q12(&self) -> i32 {
        if self.count == 0 {
            return 4096;
        }
        (self.stats.installed.min(self.count as u32) as i32 * 4096) / self.count as i32
    }

    /// The region being read or admitted, if any.
    pub fn current_region(&self) -> Option<u16> {
        self.job.as_ref().map(RegionInstall::region)
    }

    /// Advance by at most `sectors` sectors of reading and installing.
    ///
    /// `pack_lba` is the absolute LBA of the region pack. `textures(map,
    /// mask)` receives the materials a staged region's faces use (bit `m` for
    /// material `m`) and returns `true` once every one is resident; it is
    /// called again on later pumps until it does. With `wait` the pump stays
    /// with the drive for a sector already on its way, which only a loading
    /// screen should do.
    pub fn step(
        &mut self,
        cd: &mut CdController,
        map: &mut PxbspResidentMap,
        pack_lba: u32,
        sectors: usize,
        wait: bool,
        textures: &mut impl FnMut(&PxbspResidentMap, u32) -> bool,
    ) -> StreamStatus {
        let mut budget = sectors;
        let mut worked = false;
        if self.next < self.count && !matches!(self.phase, Phase::Failed(_)) {
            self.pumps = self.pumps.saturating_add(1);
        }
        loop {
            match self.phase {
                Phase::Complete => return StreamStatus::Complete,
                Phase::Failed(code) => return StreamStatus::Failed(code),
                Phase::Idle => {
                    if self.next >= self.count {
                        self.phase = Phase::Complete;
                        continue;
                    }
                    let region = self.order[self.next];
                    let entry: RegionEntry = map
                        .streaming()
                        .map(|state| state.index().regions[region as usize])
                        .unwrap_or_default();
                    match self.start_region(map, region, entry, pack_lba) {
                        Ok(()) => {}
                        Err(code) => {
                            self.fail(cd, code);
                            continue;
                        }
                    }
                }
                Phase::Reading => {
                    if budget == 0 {
                        break;
                    }
                    let Self { read, job, .. } = self;
                    let job = job.as_mut().expect("a read has its install");
                    let before = read.landed();
                    let mut sink = |bytes: &[u8]| {
                        map.install_feed(job, bytes)
                            .map(|_| ())
                            .map_err(stream_error_code)
                    };
                    let progress = read.poll(cd, budget, wait, &mut sink);
                    let landed = (read.landed() - before) as usize;
                    let restarts = read.restarts();
                    self.stats.read_restarts += u32::from(restarts - self.restarts_seen);
                    self.restarts_seen = restarts;
                    self.stats.sectors += landed as u32;
                    budget = budget.saturating_sub(landed.max(1));
                    worked |= landed > 0;
                    match progress {
                        RegionReadProgress::Reading => break,
                        RegionReadProgress::Done => {
                            if self.job.as_ref().is_some_and(RegionInstall::is_staged) {
                                self.phase = Phase::Staged;
                            } else {
                                self.fail(cd, stream_error_code(StreamError::ShortPayload));
                            }
                        }
                        RegionReadProgress::Failed(code) => {
                            self.fail(cd, code);
                        }
                    }
                }
                Phase::Staged => {
                    let job = self.job.as_mut().expect("a staged region has its install");
                    if !textures(map, job.materials()) {
                        self.stats.texture_waits += 1;
                        break;
                    }
                    match map.link_region(job) {
                        Ok(()) => {
                            if let Some(slot) = self.stats.region_pumps.get_mut(self.next) {
                                *slot = self.pumps;
                            }
                            self.pumps = 0;
                            self.stats.installed += 1;
                            self.job = None;
                            self.next += 1;
                            self.retries = 0;
                            self.phase = Phase::Idle;
                            worked = true;
                        }
                        Err(error) => self.fail(cd, stream_error_code(error)),
                    }
                }
            }
        }
        if worked {
            self.stats.steps += 1;
        }
        match self.phase {
            Phase::Complete => StreamStatus::Complete,
            Phase::Failed(code) => StreamStatus::Failed(code),
            _ => StreamStatus::Working,
        }
    }

    fn start_region(
        &mut self,
        map: &PxbspResidentMap,
        region: u16,
        entry: RegionEntry,
        pack_lba: u32,
    ) -> Result<(), u32> {
        let state = map
            .streaming()
            .ok_or_else(|| stream_error_code(StreamError::NotStreamed))?;
        // A slot per region keeps the resident face order equal to the
        // whole-map order; a smaller pool takes whatever slot is free.
        let slot = if state.slot_count() >= self.count {
            region
        } else {
            state
                .free_slot()
                .ok_or_else(|| stream_error_code(StreamError::SlotBusy))?
        };
        let job = map.begin_install(region, slot).map_err(stream_error_code)?;
        self.job = Some(job);
        self.restarts_seen = 0;
        self.read
            .start(pack_lba.saturating_add(entry.sector_start), entry.sectors());
        self.phase = Phase::Reading;
        Ok(())
    }

    /// Drop the region in flight and either restart it or give up.
    fn fail(&mut self, cd: &mut CdController, code: u32) {
        self.read.abort(cd);
        self.job = None;
        self.stats.last_error = code;
        if self.retries >= REGION_RETRIES {
            self.phase = Phase::Failed(code);
        } else {
            self.retries += 1;
            self.stats.retries += 1;
            self.phase = Phase::Idle;
        }
    }
}

impl Default for RegionStreamer {
    fn default() -> Self {
        Self::new()
    }
}
