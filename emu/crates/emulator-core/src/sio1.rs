//! SIO1 serial port register model.
//!
//! SIO1 is the general-purpose serial port at `0x1F80_1050`. It shares the
//! register shape of SIO0 but is not connected to controllers or memory cards.
//! The external byte stream is not emulated yet; the configuration registers
//! and their hardware masks are architectural and are still observable.

mod offset {
    pub const DATA: u32 = 0x0;
    pub const STAT: u32 = 0x4;
    pub const MODE: u32 = 0x8;
    pub const CTRL: u32 = 0xA;
    pub const BAUD: u32 = 0xE;
}

const MODE_WRITE_MASK: u16 = 0x017F;
const CTRL_RESET: u16 = 1 << 6;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Sio1 {
    data: u8,
    mode: u16,
    ctrl: u16,
    baud: u16,
}

impl Sio1 {
    pub const BASE: u32 = 0x1F80_1050;
    pub const SIZE: u32 = 0x10;

    pub fn new() -> Self {
        Self {
            data: 0,
            mode: 0,
            ctrl: 0,
            baud: 0,
        }
    }

    pub fn contains(phys: u32) -> bool {
        (Self::BASE..Self::BASE + Self::SIZE).contains(&phys)
    }

    pub fn read32(&self, phys: u32) -> u32 {
        match phys - Self::BASE {
            offset::DATA => self.data as u32,
            // TX-ready and TX-idle. No receive byte is queued.
            offset::STAT => 0x0000_0005,
            offset::MODE => self.mode as u32,
            offset::CTRL => self.ctrl as u32,
            offset::BAUD => self.baud as u32,
            _ => 0,
        }
    }

    pub fn write16(&mut self, phys: u32, value: u16) {
        match phys - Self::BASE {
            offset::DATA => self.data = value as u8,
            offset::STAT => {}
            offset::MODE => self.mode = value & MODE_WRITE_MASK,
            offset::CTRL => {
                if value & CTRL_RESET != 0 {
                    self.mode = 0;
                    self.ctrl = 0;
                    self.baud = 0;
                    self.data = 0;
                } else {
                    self.ctrl = value;
                }
            }
            offset::BAUD => self.baud = value,
            _ => {}
        }
    }
}

impl Default for Sio1 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_masks_reserved_bits_like_silicon() {
        let mut sio = Sio1::new();
        sio.write16(Sio1::BASE + offset::MODE, 0x5678);
        assert_eq!(sio.read32(Sio1::BASE + offset::MODE), 0x78);
    }

    #[test]
    fn control_reset_clears_visible_configuration() {
        let mut sio = Sio1::new();
        sio.write16(Sio1::BASE + offset::MODE, 0x0123);
        sio.write16(Sio1::BASE + offset::BAUD, 0x4567);
        sio.write16(Sio1::BASE + offset::CTRL, 0x5678);
        assert_eq!(sio.read32(Sio1::BASE + offset::MODE), 0);
        assert_eq!(sio.read32(Sio1::BASE + offset::CTRL), 0);
        assert_eq!(sio.read32(Sio1::BASE + offset::BAUD), 0);
    }
}
