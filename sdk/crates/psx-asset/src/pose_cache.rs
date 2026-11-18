//! Joint poses of version 7 clips, whose translations follow their parents.
//!
//! A v7 joint's pose needs its parent's decoded pose, so sampling one joint
//! means sampling its whole chain. [`PoseCache`] remembers every joint it has
//! decoded for a few (clip, position) pairs, so the renderer's joint loop and
//! the gameplay reads of the same tick (hurtboxes, sockets, scarf) decode each
//! joint once. On the console one cache serves the whole program and is
//! emptied every tick by [`clear_pose_cache`]; elsewhere (host tools, tests on
//! many threads) every query walks its chain, which gives the same poses.

// Off the console only the tests construct a cache.
#![cfg_attr(not(target_arch = "mips"), allow(dead_code))]

use crate::tracks::{self, TrackPos, Tracks};
use crate::JointPose;
use psxed_format::animation::tracks as fmt;

/// (clip, position) pairs one cache holds. A crossfade reads two clips joint by
/// joint, a masked layer a third, and gameplay reads the pose it was given.
const ENTRIES: usize = 4;

const EMPTY_POSE: JointPose = JointPose {
    matrix: [[0; 3]; 3],
    translation: psx_gte::math::Vec3I32::ZERO,
};

#[derive(Copy, Clone)]
struct Entry {
    /// Address of the clip blob, 0 when the entry is free.
    blob: usize,
    /// Q8 track position (`frame << 8 | fraction`).
    pos_q8: u32,
    /// One bit per joint whose pose is in `poses`.
    valid: u32,
    poses: [JointPose; fmt::MAX_JOINTS_V7],
}

impl Entry {
    const EMPTY: Self = Self {
        blob: 0,
        pos_q8: 0,
        valid: 0,
        poses: [EMPTY_POSE; fmt::MAX_JOINTS_V7],
    };
}

/// Decoded v7 joint poses for a few (clip, position) pairs.
pub(crate) struct PoseCache {
    entries: [Entry; ENTRIES],
    /// Entry the last query used (checked first).
    last: u8,
    /// Entry the next miss replaces.
    victim: u8,
}

impl PoseCache {
    /// A cache holding nothing.
    pub(crate) const EMPTY: Self = Self {
        entries: [Entry::EMPTY; ENTRIES],
        last: 0,
        victim: 0,
    };

    /// Forget every pose (the clips behind them may be about to move).
    pub(crate) fn clear(&mut self) {
        for entry in &mut self.entries {
            entry.blob = 0;
            entry.valid = 0;
        }
    }

    fn entry(&mut self, blob: usize, pos_q8: u32) -> &mut Entry {
        let last = self.last as usize;
        if self.entries[last].blob == blob && self.entries[last].pos_q8 == pos_q8 {
            return &mut self.entries[last];
        }
        let mut index = ENTRIES;
        for (i, entry) in self.entries.iter().enumerate() {
            if entry.blob == blob && entry.pos_q8 == pos_q8 {
                index = i;
                break;
            }
        }
        if index == ENTRIES {
            index = self.victim as usize;
            self.victim = ((index + 1) % ENTRIES) as u8;
            let entry = &mut self.entries[index];
            entry.blob = blob;
            entry.pos_q8 = pos_q8;
            entry.valid = 0;
        }
        self.last = index as u8;
        &mut self.entries[index]
    }

    /// Pose of `joint` at track position `pos_q8` (`pos` is the same position
    /// mapped onto the clip's rates), decoding whatever part of its chain this
    /// cache does not hold yet.
    ///
    /// # Safety
    /// `tracks` must come from [`tracks::validate`] for a v7 clip whose blob
    /// starts at `blob`, and `joint < joint_count`.
    pub(crate) unsafe fn pose(
        &mut self,
        tracks: &Tracks,
        blob: usize,
        pos_q8: u32,
        pos: &TrackPos,
        joint: usize,
    ) -> JointPose {
        let entry = self.entry(blob, pos_q8);
        if entry.valid & (1 << joint) != 0 {
            return entry.poses[joint];
        }
        // Joints from `joint` up to the first ancestor already held (or the
        // chain's absolute joint), decoded top down. `validate` proved no chain
        // loops, so it visits at most `joint_count <= 32` joints.
        let mut chain = [0u8; fmt::MAX_JOINTS_V7];
        let mut n = 0;
        let mut j = joint;
        loop {
            chain[n] = j as u8;
            n += 1;
            // SAFETY: `j` is a joint of the validated clip.
            match unsafe { tracks::parent(tracks, j) } {
                Some(p) if entry.valid & (1 << p) == 0 => j = p,
                _ => break,
            }
        }
        while n > 0 {
            n -= 1;
            let j = chain[n] as usize;
            // SAFETY: `j` is a joint of the validated clip.
            let mut pose = unsafe { tracks::joint_pose(tracks, j, pos) };
            // SAFETY: as above.
            if let Some(p) = unsafe { tracks::parent(tracks, j) } {
                // SAFETY: a v7 clip; the parent is valid in `entry` (decoded
                // above or held before).
                unsafe { tracks::derive(tracks, j, &mut pose, &entry.poses[p]) };
            }
            entry.poses[j] = pose;
            entry.valid |= 1 << j;
        }
        entry.poses[joint]
    }
}

/// Pose of `joint` at `pos`, walking its whole chain with no cache.
///
/// # Safety
/// `tracks` must come from [`tracks::validate`] and `joint < joint_count`.
pub(crate) unsafe fn chain_pose(tracks: &Tracks, pos: &TrackPos, joint: usize) -> JointPose {
    let mut chain = [0u8; fmt::MAX_JOINTS_V7];
    let mut n = 0;
    let mut j = joint;
    loop {
        chain[n] = j as u8;
        n += 1;
        // SAFETY: `j` is a joint of the validated clip; a v7 chain visits at
        // most `joint_count <= 32` joints and a v6 joint has no parent.
        match unsafe { tracks::parent(tracks, j) } {
            Some(p) => j = p,
            None => break,
        }
    }
    n -= 1;
    // SAFETY: `chain[n]` is a joint of the validated clip.
    let mut pose = unsafe { tracks::joint_pose(tracks, chain[n] as usize, pos) };
    while n > 0 {
        n -= 1;
        let j = chain[n] as usize;
        // SAFETY: as above.
        let mut child = unsafe { tracks::joint_pose(tracks, j, pos) };
        // SAFETY: a joint with a parent belongs to a v7 clip.
        unsafe { tracks::derive(tracks, j, &mut child, &pose) };
        pose = child;
    }
    pose
}

#[cfg(target_arch = "mips")]
mod global {
    use core::cell::UnsafeCell;

    use super::PoseCache;

    pub(super) struct Global(pub(super) UnsafeCell<PoseCache>);

    // SAFETY: the console runs one thread of control and no interrupt handler
    // samples animation, so the cache is never reached twice at once.
    unsafe impl Sync for Global {}

    pub(super) static CACHE: Global = Global(UnsafeCell::new(PoseCache::EMPTY));
}

/// Pose of `joint` of a v7 clip: through the program's cache on the console,
/// walking the chain elsewhere.
///
/// # Safety
/// `tracks` must come from [`tracks::validate`] for a v7 clip whose blob starts
/// at `blob`, and `joint < joint_count`.
#[inline]
pub(crate) unsafe fn v7_pose(
    tracks: &Tracks,
    blob: usize,
    pos_q8: u32,
    pos: &TrackPos,
    joint: usize,
) -> JointPose {
    #[cfg(target_arch = "mips")]
    {
        // SAFETY: one thread of control (see `Global`); the reference does not
        // outlive this call. The caller's contract covers `pose`.
        unsafe { (*global::CACHE.0.get()).pose(tracks, blob, pos_q8, pos, joint) }
    }
    #[cfg(not(target_arch = "mips"))]
    {
        let _ = (blob, pos_q8);
        // SAFETY: forwarded from the caller.
        unsafe { chain_pose(tracks, pos, joint) }
    }
}

/// Forget every cached v7 joint pose. Call once per tick, before anything
/// samples a clip, and whenever clip memory may have been reused: the cache
/// knows a clip by its address.
pub fn clear_pose_cache() {
    #[cfg(target_arch = "mips")]
    // SAFETY: one thread of control (see `Global`).
    unsafe {
        (*global::CACHE.0.get()).clear();
    }
}
