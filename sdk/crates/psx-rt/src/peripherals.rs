//! The one-per-program set of peripheral ownership tokens.

use crate::critical_section::{self, Mutex};
use core::cell::Cell;
use psx_io::periph::{Cdrom, GpuDma, MdecDma, OtcDma, Sio0, SpuDma};

/// Every shared-state peripheral token, handed out once.
///
/// ```ignore
/// let mut p = psx_rt::Peripherals::take().unwrap();
/// let mut frame = ot.frame();
/// // ...
/// frame.submit(&mut p.gpu_dma);
/// ```
///
/// The tokens are zero-sized, so `Peripherals` is too; moving it or its
/// fields around costs nothing.
#[derive(Debug)]
#[non_exhaustive]
pub struct Peripherals {
    /// DMA channel 2 and the GP0 stream it feeds.
    pub gpu_dma: GpuDma,
    /// DMA channel 6, the ordering-table clear.
    pub otc_dma: OtcDma,
    /// DMA channels 0 and 1 (MDEC in and out).
    pub mdec_dma: MdecDma,
    /// DMA channel 4 and the SPU transfer port.
    pub spu_dma: SpuDma,
    /// The CD-ROM controller and DMA channel 3.
    pub cdrom: Cdrom,
    /// SIO0, the pad and memory-card port.
    pub sio0: Sio0,
}

static TAKEN: Mutex<Cell<bool>> = Mutex::new(Cell::new(false));

impl Peripherals {
    /// The token set, the first time this is called; `None` after that.
    ///
    /// One flag test inside a short critical section.
    pub fn take() -> Option<Self> {
        critical_section::with(|cs| {
            let taken = TAKEN.borrow(cs);
            if taken.get() {
                return None;
            }
            taken.set(true);
            // SAFETY: the flag guarantees this is the only set handed out by
            // `take`; `steal` callers carry their own contract.
            Some(unsafe { Self::steal() })
        })
    }

    /// The token set, whether or not [`take`](Self::take) already ran.
    ///
    /// # Safety
    ///
    /// The caller must not use a token from this set while the same token
    /// from another set is in use (see `psx_io::periph`).
    pub unsafe fn steal() -> Self {
        // SAFETY: forwarded contract.
        unsafe {
            Self {
                gpu_dma: GpuDma::steal(),
                otc_dma: OtcDma::steal(),
                mdec_dma: MdecDma::steal(),
                spu_dma: SpuDma::steal(),
                cdrom: Cdrom::steal(),
                sio0: Sio0::steal(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_hands_the_set_out_once() {
        assert_eq!(core::mem::size_of::<Peripherals>(), 0);
        let first = Peripherals::take();
        assert!(first.is_some());
        assert!(Peripherals::take().is_none());
    }
}
