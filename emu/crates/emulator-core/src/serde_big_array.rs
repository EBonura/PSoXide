//! `#[serde(with = ...)]` helpers for fixed-size arrays past serde's
//! built-in cap.
//!
//! `serde`'s array `Serialize`/`Deserialize` impls are hand-written for
//! lengths `0..=32` -- there is no blanket const-generic impl for
//! arbitrary `N` (confirmed against the pinned `serde 1.0.228`; this
//! isn't a version-specific gap serde has ever closed). Every
//! `[T; N]` or `Box<[T; N]>` field in the emulator's state graph with
//! `N > 32` that must actually round-trip (not `#[serde(skip)]`'d as
//! diagnostics) goes through one of these two modules instead.
//!
//! For plain `[u8; N]` buffers, prefer `#[serde(with = "serde_bytes")]`-
//! style byte-oriented (de)serialization (see `bus::big_bytes`) --
//! postcard encodes raw bytes far more compactly than a generic
//! element-by-element sequence. These two modules are the fallback
//! for element types other than `u8` (here: `u16` VRAM/SPU-RAM words,
//! `i32` MDEC quantization tables).

use serde::de::{Error as _, SeqAccess, Visitor};
use serde::ser::SerializeTuple;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::marker::PhantomData;

/// For a plain (unboxed) `[T; N]` field.
pub(crate) mod array {
    use super::*;

    pub fn serialize<S, T, const N: usize>(arr: &[T; N], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        T: Serialize,
    {
        let mut tup = serializer.serialize_tuple(N)?;
        for item in arr {
            tup.serialize_element(item)?;
        }
        tup.end()
    }

    pub fn deserialize<'de, D, T, const N: usize>(deserializer: D) -> Result<[T; N], D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de> + Copy + Default,
    {
        struct ArrayVisitor<T, const N: usize>(PhantomData<T>);

        impl<'de, T, const N: usize> Visitor<'de> for ArrayVisitor<T, N>
        where
            T: Deserialize<'de> + Copy + Default,
        {
            type Value = [T; N];

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "an array of length {N}")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut out = [T::default(); N];
                for (i, slot) in out.iter_mut().enumerate() {
                    *slot = seq
                        .next_element()?
                        .ok_or_else(|| A::Error::invalid_length(i, &self))?;
                }
                Ok(out)
            }
        }

        deserializer.deserialize_tuple(N, ArrayVisitor::<T, N>(PhantomData))
    }
}

/// For a `Box<[T; N]>` field -- thin wrapper around [`array`] so the
/// boxed buffers used for VRAM and SPU RAM don't need to unbox/rebox
/// at every call site.
pub(crate) mod boxed_array {
    use super::*;

    pub fn serialize<S, T, const N: usize>(arr: &[T; N], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        T: Serialize,
    {
        array::serialize(arr, serializer)
    }

    /// Deliberately does NOT go through [`array::deserialize`]: that
    /// helper builds the full `[T; N]` as a stack local before the
    /// caller boxes it, which is fine for the small (≤256-element)
    /// arrays it's meant for but reliably blows the stack in debug
    /// builds for VRAM (1 MiB) / SPU RAM (512 KiB) -- `Box::new(x)`
    /// constructs `x` on the stack first and only then copies it to
    /// the heap unless the optimizer manages to elide that (it
    /// usually doesn't in unoptimized builds). Building the buffer as
    /// a `Vec` instead keeps every byte heap-resident from the first
    /// element on.
    pub fn deserialize<'de, D, T, const N: usize>(deserializer: D) -> Result<Box<[T; N]>, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de>,
    {
        struct VecVisitor<T, const N: usize>(PhantomData<T>);

        impl<'de, T, const N: usize> Visitor<'de> for VecVisitor<T, N>
        where
            T: Deserialize<'de>,
        {
            type Value = Vec<T>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "an array of length {N}")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut out = Vec::with_capacity(N);
                while let Some(item) = seq.next_element()? {
                    out.push(item);
                }
                Ok(out)
            }
        }

        let vec = deserializer.deserialize_tuple(N, VecVisitor::<T, N>(PhantomData))?;
        let len = vec.len();
        vec.into_boxed_slice()
            .try_into()
            .map_err(|_| D::Error::invalid_length(len, &"an array of the expected length"))
    }
}
