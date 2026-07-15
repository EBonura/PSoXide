/// GPUSTAT -- the status register the CPU polls to check whether the
/// GPU is ready for commands, ready to upload VRAM, etc.
///
/// Bits 26 and 28 reflect whether the emulated command pipeline has
/// room for more work. Bit 27 is driven separately by the active
/// VRAM-to-CPU transfer, bit 25 (DMA request) is computed from the DMA
/// direction bits 29:30, and bit 31 toggles at VBlank.
#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct GpuStatus {
    pub(super) raw: u32,
}

impl GpuStatus {
    pub(super) fn new() -> Self {
        // Reset defaults matching Redux's `SoftGPU::impl::open` /
        // `softReset`, both of which initialise `m_statusRet` to
        // `0x14802000`:
        //   bit 23 (DISPLAY_DISABLE) = 1
        //   bit 21 = 1 (reserved, Redux sets on reset)
        //   bit 13 (INTERLACE_FIELD) = 1
        //   bit 31 (DRAWING_ODD) = 0 at power-on; first VBlank XORs it
        //          to 1, second back to 0, etc. Earlier we initialised
        //          this to 1 (matching the "odd field looks ready"
        //          intuition), but parity divergence at step 19259778
        //          showed Redux is the authority and starts at 0.
        // Ready bits 26/28 are filled in by `read`; VRAM-ready (27) is
        // gated on an active VRAM→CPU transfer.
        Self { raw: 0x1480_2000 }
    }

    /// Compose the observable GPUSTAT word. `vram_send_ready` is the
    /// live "is a VRAM→CPU transfer in progress and has pixels
    /// waiting" flag owned by the GPU proper.
    pub(super) fn read(
        &self,
        vram_send_ready: bool,
        command_ready: bool,
        dma_block_ready: bool,
    ) -> u32 {
        let mut ret = self.raw;

        // Ready bits: 26 (cmd FIFO ready) + 28 (DMA block ready).
        // The earlier always-ready soft-GPU model made every rendering
        // command appear instantaneous to code using DrawPrim/DrawSync.
        // Real silicon deasserts these once queued work fills the input
        // path, which is what makes CPU command submission settle at the
        // GPU's actual raster/VRAM bandwidth.
        // Bit 27 (VRAM→CPU ready) is only set while software is
        // actually pulling pixels from GPUREAD; Redux's default is
        // 0 and BIOS/game code polls this bit to detect transfer
        // completion.
        if command_ready {
            ret |= 0x0400_0000;
        } else {
            ret &= !0x0400_0000;
        }
        if dma_block_ready {
            ret |= 0x1000_0000;
        } else {
            ret &= !0x1000_0000;
        }
        if vram_send_ready {
            ret |= 0x0800_0000;
        } else {
            ret &= !0x0800_0000;
        }

        // Bit 25 (DMA data request), from the public real-silicon GPUSTAT
        // suite: direction 0 clears it, FIFO direction 1 asserts it,
        // CPU-to-GP0 direction 2 mirrors bit 28, and GPUREAD-to-CPU
        // direction 3 mirrors bit 27.
        ret &= !0x0200_0000;
        match (ret >> 29) & 3 {
            1 => ret |= 0x0200_0000,
            2 if ret & 0x1000_0000 != 0 => ret |= 0x0200_0000,
            3 if ret & 0x0800_0000 != 0 => ret |= 0x0200_0000,
            _ => {}
        }
        ret
    }

    /// GP1 command dispatch. GP1 commands are simple enough to inline
    /// here; GP0 is more elaborate (packet assembly, texture upload,
    /// primitives) and gets a dedicated module when we actually render.
    pub(super) fn toggle_field(&mut self) {
        self.raw ^= 0x8000_0000;
    }

    pub(super) fn gp1_write(&mut self, value: u32) {
        // GP1 writes that affect GpuStatus itself are handled here.
        // Display-area writes (0x05 / 0x06 / 0x07 / 0x08) are routed
        // through `Gpu::apply_gp1_display` in the outer impl so they
        // can update the display-area fields. We still apply the
        // subset that touches GPUSTAT bits (0x08).
        let cmd = (value >> 24) & 0xFF;
        match cmd {
            0x00 => *self = Self::new(),
            0x02 => self.raw &= !(1 << 24),
            0x03 => {
                if value & 1 != 0 {
                    self.raw |= 1 << 23;
                } else {
                    self.raw &= !(1 << 23);
                }
            }
            0x04 => {
                self.raw = (self.raw & !0x6000_0000) | ((value & 3) << 29);
            }
            0x08 => {
                // GP1 0x08: display mode. Update the GPUSTAT bits that
                // mirror it (16:17 for h-res-1, 18 for v-res, 19 for
                // mode, 20 for 24bpp, 21 for v-interlace, 22 for h-res-2).
                let hres1 = value & 0x3;
                let vres = (value >> 2) & 0x1;
                let mode = (value >> 3) & 0x1;
                let bpp = (value >> 4) & 0x1;
                let vinter = (value >> 5) & 0x1;
                let hres2 = (value >> 6) & 0x1;
                let status_bits = (hres1 << 17)
                    | (vres << 19)
                    | (mode << 20)
                    | (bpp << 21)
                    | (vinter << 22)
                    | (hres2 << 16);
                self.raw = (self.raw & !0x007F_0000) | status_bits;
            }
            _ => {}
        }
    }
}
