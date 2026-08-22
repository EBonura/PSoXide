// SPDX-License-Identifier: GPL-2.0-or-later
//! Fixed-capacity scratch storage that initializes only the entries it uses.
//!
//! Large `[T::EMPTY; N]` locals compile to a full `memset` on the PS1 even
//! when a frame writes and reads only a short prefix. [`FixedScratch`] keeps
//! the same explicit capacity contract while leaving unused entries
//! uninitialized. The contained type is restricted to [`Copy`] values so
//! clearing the logical length never has to run destructors.

use core::mem::MaybeUninit;

/// Minimal append contract shared by fixed scratch and caller-owned slices.
///
/// Collection routines use this trait so their existing slice APIs and their
/// no-clear [`FixedScratch`] APIs execute the same loop and preserve the same
/// stable insertion order.
pub trait BoundedSink<T> {
    /// Append `value`, returning `false` when the explicit capacity is full.
    fn try_push(&mut self, value: T) -> bool;
}

/// Fixed-capacity, allocation-free scratch whose unused tail is never cleared.
pub struct FixedScratch<T: Copy, const CAPACITY: usize> {
    values: [MaybeUninit<T>; CAPACITY],
    len: usize,
}

impl<T: Copy, const CAPACITY: usize> FixedScratch<T, CAPACITY> {
    /// Create empty scratch without initializing its backing entries.
    pub const fn new() -> Self {
        Self {
            values: [const { MaybeUninit::uninit() }; CAPACITY],
            len: 0,
        }
    }

    /// Number of initialized entries currently held.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the initialized prefix is empty.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Total compile-time entry capacity.
    pub const fn capacity(&self) -> usize {
        CAPACITY
    }

    /// Forget the initialized prefix without touching backing memory.
    #[inline(always)]
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Append one entry, returning `false` without modifying the buffer when
    /// it is full.
    #[inline(always)]
    pub fn try_push(&mut self, value: T) -> bool {
        let Some(slot) = self.values.get_mut(self.len) else {
            return false;
        };
        slot.write(value);
        self.len += 1;
        true
    }

    /// Borrow exactly the initialized prefix.
    #[inline(always)]
    pub fn as_slice(&self) -> &[T] {
        // SAFETY: `try_push` initializes each entry before advancing `len`,
        // and `clear` can only shrink the exposed prefix.
        unsafe { core::slice::from_raw_parts(self.values.as_ptr().cast::<T>(), self.len) }
    }

    /// Mutably borrow exactly the initialized prefix.
    #[inline(always)]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        // SAFETY: the same initialized-prefix invariant as `as_slice` holds,
        // and the exclusive borrow of `self` guarantees unique access.
        unsafe { core::slice::from_raw_parts_mut(self.values.as_mut_ptr().cast::<T>(), self.len) }
    }
}

impl<T: Copy, const CAPACITY: usize> Default for FixedScratch<T, CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy, const CAPACITY: usize> BoundedSink<T> for FixedScratch<T, CAPACITY> {
    #[inline(always)]
    fn try_push(&mut self, value: T) -> bool {
        FixedScratch::try_push(self, value)
    }
}

/// Bounded append adapter over an already initialized caller-owned slice.
///
/// This retains compatibility for APIs that historically accepted `&mut
/// [T]`, while letting those APIs share their collection loop with
/// [`FixedScratch`].
pub struct SliceSink<'a, T> {
    values: &'a mut [T],
    len: usize,
}

impl<'a, T> SliceSink<'a, T> {
    /// Wrap an output slice with an initially empty logical prefix.
    pub fn new(values: &'a mut [T]) -> Self {
        Self { values, len: 0 }
    }

    /// Number of entries written to the slice.
    pub const fn len(&self) -> usize {
        self.len
    }
}

impl<T> BoundedSink<T> for SliceSink<'_, T> {
    #[inline(always)]
    fn try_push(&mut self, value: T) -> bool {
        let Some(slot) = self.values.get_mut(self.len) else {
            return false;
        };
        *slot = value;
        self.len += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_scratch_exposes_only_successful_pushes_and_clears_logically() {
        let mut values = FixedScratch::<u32, 2>::new();
        assert!(values.is_empty());
        assert!(values.try_push(7));
        assert!(values.try_push(11));
        assert!(!values.try_push(13));
        assert_eq!(values.as_slice(), &[7, 11]);
        values.as_mut_slice()[1] = 17;
        assert_eq!(values.as_slice(), &[7, 17]);
        values.clear();
        assert!(values.is_empty());
        assert!(values.try_push(23));
        assert_eq!(values.as_slice(), &[23]);
    }

    #[test]
    fn slice_sink_reports_capacity_without_overwriting_existing_tail() {
        let mut values = [3u8, 4];
        let mut sink = SliceSink::new(&mut values[..1]);
        assert!(sink.try_push(9));
        assert!(!sink.try_push(10));
        assert_eq!(sink.len(), 1);
        assert_eq!(values, [9, 4]);
    }
}
