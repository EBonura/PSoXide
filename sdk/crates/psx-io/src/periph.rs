//! Zero-size ownership tokens for peripherals with shared state.
//!
//! One token exists per device whose state two drivers could otherwise
//! fight over: the GPU's DMA channel (an in-flight linked-list walk and a
//! GP0 write interleave), the ordering-table clear, MDEC and SPU channels, the CD-ROM
//! controller (three drivers program it) and the controller port (pads and memory
//! cards share it). An API that drives one of them takes the token by `&mut`, so
//! the borrow checker sees the conflict. The tokens are empty, so passing
//! them costs nothing at run time.
//!
//! Get the whole set once with `psx_rt::Peripherals::take()`. A token is a
//! logic guard, not a memory-safety one: the SDK's DMA APIs prove buffer
//! lifetimes through references and slices whether or not a token is
//! involved. That is why [`GpuDma::steal`] and friends are `unsafe` only in
//! the "you are breaking an ownership invariant" sense.

macro_rules! token {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug)]
        pub struct $name {
            _private: (),
        }

        impl $name {
            /// Make a token without going through `psx_rt::Peripherals::take()`.
            ///
            /// # Safety
            ///
            /// The caller must not use this token while another one of the
            /// same type is in use: two owners of one device each assume
            /// they are the only driver programming it.
            #[inline(always)]
            pub const unsafe fn steal() -> Self {
                Self { _private: () }
            }
        }
    };
}

token!(
    /// DMA channel 2 (RAM to GP0, and GPUREAD to RAM) and the GP0 command
    /// stream it feeds.
    GpuDma
);
token!(
    /// DMA channel 6, the ordering-table clear.
    #[doc(alias = "OTC")]
    OrderingTableClearDma
);
token!(
    /// DMA channels 0 and 1, into and out of the MDEC.
    MdecDma
);
token!(
    /// DMA channel 4 (RAM to SPU RAM and back) and the SPU transfer port.
    SpuDma
);
token!(
    /// The CD-ROM controller and DMA channel 3.
    #[doc(alias = "CDROM")]
    Cd
);
token!(
    /// The controller and memory-card port.
    #[doc(alias = "SIO0")]
    ControllerPort
);

/// Renamed to [`OrderingTableClearDma`].
#[deprecated(note = "renamed to `OrderingTableClearDma`")]
pub type OtcDma = OrderingTableClearDma;
/// Renamed to [`Cd`].
#[deprecated(note = "renamed to `Cd`")]
pub type Cdrom = Cd;
/// Renamed to [`ControllerPort`].
#[deprecated(note = "renamed to `ControllerPort`")]
pub type Sio0 = ControllerPort;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_zero_sized() {
        assert_eq!(core::mem::size_of::<GpuDma>(), 0);
        assert_eq!(core::mem::size_of::<OrderingTableClearDma>(), 0);
        assert_eq!(core::mem::size_of::<MdecDma>(), 0);
        assert_eq!(core::mem::size_of::<SpuDma>(), 0);
        assert_eq!(core::mem::size_of::<Cd>(), 0);
        assert_eq!(core::mem::size_of::<ControllerPort>(), 0);
    }
}
