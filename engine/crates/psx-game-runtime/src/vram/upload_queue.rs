//! Async VRAM upload queue, carved out of `editor-playtest`'s
//! `vram_upload_queue` module (phase 1, vram_runtime slice). Jobs are
//! stepped a bounded number of texture rows per call so a room
//! activation's upload burst spreads across background ticks instead
//! of stalling a frame; [`super::VramRuntime`] owns the one instance.
//!
//! Jobs identify their source texture by [`AssetId`] and re-resolve the
//! bytes through the caller's resolver on every step (phase 1.5): the
//! queue retains no byte slices across frames, so upload sources only
//! need to outlive each step call instead of being `&'static`.

use super::{upload_clut, upload_opaque_clut, VramSlotClutMode};
use psx_asset::Texture;
use psx_engine::telemetry;
use psx_level::AssetId;
use psx_vram::{upload_bytes, VramRect};

// Concurrent in-flight texture uploads. A room activation can request a burst
// of texture uploads at once; anything that does not fit here is dropped to the
// untextured fallback until the material pump re-queues it, so a slightly deeper
// queue reduces those transient drops during heavy streaming.
// Room material textures are requested in bursts when the active-room window
// moves. cortex_v1's recorded route overflowed a 12-job queue 128 times while
// completing only 21 uploads, and every rejection is a dropped texture rather
// than a deferred one. A rejected request is invisible to the completion pump,
// so widening the queue turns drops into ordinary pending work at a cost of a
// few hundred bytes and no per-frame time.
const VRAM_UPLOAD_QUEUE_CAP: usize = 24;

#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) enum VramUploadKind {
    TextureAndClut,
    ClutOnly,
}

#[derive(Copy, Clone)]
pub(crate) struct VramUploadJob {
    pub(crate) active: bool,
    pub(crate) slot_index: u16,
    pub(crate) asset: AssetId,
    pub(crate) clut_mode: VramSlotClutMode,
    pub(crate) kind: VramUploadKind,
    pub(crate) texture_x: u16,
    pub(crate) texture_y: u16,
    pub(crate) texture_width_halfwords: u16,
    pub(crate) texture_height_rows: u16,
    pub(crate) next_texture_row: u16,
    pub(crate) clut_x: u16,
    pub(crate) clut_y: u16,
    pub(crate) clut_entries: u16,
    pub(crate) clut_uploaded: bool,
}

impl VramUploadJob {
    const EMPTY: Self = Self {
        active: false,
        slot_index: 0,
        asset: AssetId(0),
        clut_mode: VramSlotClutMode::OpaqueZero,
        kind: VramUploadKind::TextureAndClut,
        texture_x: 0,
        texture_y: 0,
        texture_width_halfwords: 0,
        texture_height_rows: 0,
        next_texture_row: 0,
        clut_x: 0,
        clut_y: 0,
        clut_entries: 0,
        clut_uploaded: false,
    };

    fn texture_complete(self) -> bool {
        self.kind == VramUploadKind::ClutOnly || self.next_texture_row >= self.texture_height_rows
    }

    fn complete(self) -> bool {
        self.texture_complete() && self.clut_uploaded
    }
}

pub(crate) struct VramUploadQueue {
    jobs: [VramUploadJob; VRAM_UPLOAD_QUEUE_CAP],
}

impl VramUploadQueue {
    pub(crate) const fn new() -> Self {
        Self {
            jobs: [VramUploadJob::EMPTY; VRAM_UPLOAD_QUEUE_CAP],
        }
    }

    pub(crate) fn contains(&self, asset: AssetId, clut_mode: VramSlotClutMode) -> bool {
        let mut i = 0usize;
        while i < self.jobs.len() {
            let job = self.jobs[i];
            if job.active && job.asset == asset && job.clut_mode == clut_mode {
                return true;
            }
            i += 1;
        }
        false
    }

    pub(crate) fn has_free_slot(&self) -> bool {
        let mut i = 0usize;
        while i < self.jobs.len() {
            if !self.jobs[i].active {
                return true;
            }
            i += 1;
        }
        false
    }

    pub(crate) fn is_idle(&self) -> bool {
        let mut i = 0usize;
        while i < self.jobs.len() {
            if self.jobs[i].active {
                return false;
            }
            i += 1;
        }
        true
    }

    pub(crate) fn push(&mut self, job: VramUploadJob) -> bool {
        let mut i = 0usize;
        while i < self.jobs.len() {
            if !self.jobs[i].active {
                self.jobs[i] = job;
                return true;
            }
            i += 1;
        }
        false
    }

    pub(crate) fn step<'r>(
        &mut self,
        row_budget: u16,
        resolve: &impl Fn(AssetId) -> Option<&'r [u8]>,
        mut mark_ready: impl FnMut(usize),
    ) -> bool {
        let mut remaining_rows = row_budget;
        let mut completed_any = false;
        let mut i = 0usize;
        while i < self.jobs.len() && remaining_rows > 0 {
            if !self.jobs[i].active {
                i += 1;
                continue;
            }

            telemetry::stage_begin(telemetry::stage::VRAM_UPLOAD);
            if !self.jobs[i].texture_complete() {
                let rows = self.upload_texture_rows(i, remaining_rows, resolve);
                remaining_rows = remaining_rows.saturating_sub(rows.max(1));
            } else if !self.jobs[i].clut_uploaded {
                self.upload_clut(i, resolve);
                remaining_rows = remaining_rows.saturating_sub(1);
            }
            telemetry::stage_end(telemetry::stage::VRAM_UPLOAD);

            if self.jobs[i].complete() {
                mark_ready(self.jobs[i].slot_index as usize);
                telemetry::counter(telemetry::counter::ROOM_TEXTURE_UPLOADS, 1);
                self.jobs[i] = VramUploadJob::EMPTY;
                completed_any = true;
            }
            i += 1;
        }
        completed_any
    }

    fn upload_texture_rows<'r>(
        &mut self,
        index: usize,
        row_budget: u16,
        resolve: &impl Fn(AssetId) -> Option<&'r [u8]>,
    ) -> u16 {
        let Some(bytes) = resolve(self.jobs[index].asset) else {
            self.jobs[index] = VramUploadJob::EMPTY;
            return 0;
        };
        let Some(texture) = Texture::from_bytes(bytes).ok() else {
            self.jobs[index] = VramUploadJob::EMPTY;
            return 0;
        };
        let row_bytes = usize::from(self.jobs[index].texture_width_halfwords).saturating_mul(2);
        if row_bytes == 0
            || texture.pixel_bytes().len()
                < row_bytes.saturating_mul(usize::from(self.jobs[index].texture_height_rows))
        {
            self.jobs[index] = VramUploadJob::EMPTY;
            return 0;
        }

        let mut uploaded = 0u16;
        while uploaded < row_budget
            && self.jobs[index].next_texture_row < self.jobs[index].texture_height_rows
        {
            let row = self.jobs[index].next_texture_row;
            let offset = usize::from(row).saturating_mul(row_bytes);
            upload_bytes(
                VramRect::new(
                    self.jobs[index].texture_x,
                    self.jobs[index].texture_y.saturating_add(row),
                    self.jobs[index].texture_width_halfwords,
                    1,
                ),
                &texture.pixel_bytes()[offset..offset + row_bytes],
            );
            self.jobs[index].next_texture_row = self.jobs[index].next_texture_row.saturating_add(1);
            uploaded = uploaded.saturating_add(1);
        }
        uploaded
    }

    fn upload_clut<'r>(&mut self, index: usize, resolve: &impl Fn(AssetId) -> Option<&'r [u8]>) {
        let Some(bytes) = resolve(self.jobs[index].asset) else {
            self.jobs[index] = VramUploadJob::EMPTY;
            return;
        };
        let Some(texture) = Texture::from_bytes(bytes).ok() else {
            self.jobs[index] = VramUploadJob::EMPTY;
            return;
        };
        let clut_bytes = texture.clut_bytes();
        let expected_len = usize::from(self.jobs[index].clut_entries).saturating_mul(2);
        if clut_bytes.len() < expected_len {
            self.jobs[index] = VramUploadJob::EMPTY;
            return;
        }
        let rect = VramRect::new(
            self.jobs[index].clut_x,
            self.jobs[index].clut_y,
            self.jobs[index].clut_entries,
            1,
        );
        if self.jobs[index].clut_mode == VramSlotClutMode::OpaqueZero {
            upload_opaque_clut(rect, &clut_bytes[..expected_len]);
        } else {
            upload_clut(rect, &clut_bytes[..expected_len]);
        }
        self.jobs[index].clut_uploaded = true;
    }
}
