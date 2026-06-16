//! Margo, the VEGA 2D engine. This slice covers the display register block and
//! the linear frame buffer; the blit engine arrives in later slices.

pub const MARGO_VRAM_SIZE: usize = 4 * 1024 * 1024;
pub const MARGO_ID_VALUE: u32 = 0x4D47_0100; // 'M' 'G', version 1.00
pub const MARGO_CAPS_VALUE: u32 = 0x0000_0000; // no engine ops in this slice

pub const REG_ID: usize = 0x0000;
pub const REG_CAPS: usize = 0x0004;
pub const REG_STATUS: usize = 0x0008;
pub const REG_CONTROL: usize = 0x000c;
pub const REG_DISP_MODE: usize = 0x0010;
pub const REG_DISP_WIDTH: usize = 0x0014;
pub const REG_DISP_HEIGHT: usize = 0x0018;
pub const REG_DISP_BPP: usize = 0x001c;
pub const REG_DISP_PITCH: usize = 0x0020;
pub const REG_DISP_START: usize = 0x0024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MargoDisplay {
    pub mode: u16,
    pub width: u32,
    pub height: u32,
    pub bpp: u32,
    pub pitch: u32,
    pub start: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Margo {
    vram: Vec<u8>,
    display: MargoDisplay,
    control: u32,
}

impl Default for Margo {
    fn default() -> Self {
        Self {
            vram: vec![0; MARGO_VRAM_SIZE],
            display: MargoDisplay::default(),
            control: 0,
        }
    }
}

impl Margo {
    pub fn display(&self) -> MargoDisplay {
        self.display
    }

    pub fn set_mode_640x480x8(&mut self) {
        self.display = MargoDisplay {
            mode: 0x0101,
            width: 640,
            height: 480,
            bpp: 8,
            pitch: 640,
            start: 0,
        };
    }

    pub fn read_vram_u8(&self, offset: usize) -> u8 {
        self.vram.get(offset).copied().unwrap_or(0)
    }

    pub fn write_vram_u8(&mut self, offset: usize, value: u8) {
        if let Some(slot) = self.vram.get_mut(offset) {
            *slot = value;
        }
    }

    pub fn vram(&self) -> &[u8] {
        &self.vram
    }

    pub fn vram_mut(&mut self) -> &mut [u8] {
        &mut self.vram
    }

    /// The visible scanout surface: `pitch * height` bytes starting at `start`.
    /// Returns an empty slice when no mode has been set (pitch or height is 0);
    /// callers reach this only when Margo is the active display, after a mode-set.
    pub fn visible_surface(&self) -> &[u8] {
        let start = (self.display.start as usize).min(self.vram.len());
        let len = (self.display.pitch as usize).saturating_mul(self.display.height as usize);
        let end = (start + len).min(self.vram.len());
        &self.vram[start..end]
    }

    fn register_u32(&self, reg: usize) -> u32 {
        match reg {
            REG_ID => MARGO_ID_VALUE,
            REG_CAPS => MARGO_CAPS_VALUE,
            REG_STATUS => 0, // BUSY clear, no FIFO in this slice
            REG_CONTROL => self.control,
            REG_DISP_MODE => u32::from(self.display.mode),
            REG_DISP_WIDTH => self.display.width,
            REG_DISP_HEIGHT => self.display.height,
            REG_DISP_BPP => self.display.bpp,
            REG_DISP_PITCH => self.display.pitch,
            REG_DISP_START => self.display.start,
            _ => 0,
        }
    }

    pub fn read_mmio_u8(&self, offset: usize) -> u8 {
        let reg = offset & !0x3;
        let byte = offset & 0x3;
        (self.register_u32(reg) >> (8 * byte)) as u8
    }

    pub fn write_mmio_u8(&mut self, offset: usize, value: u8) {
        let reg = offset & !0x3;
        let byte = offset & 0x3;
        // Only CONTROL and DISP_START are writable in this slice. Everything else
        // (identity, caps, the display geometry set by the host mode-set) is
        // read-only to the bus.
        let target = match reg {
            REG_CONTROL => &mut self.control,
            REG_DISP_START => &mut self.display.start,
            _ => return,
        };
        let shift = 8 * byte;
        *target = (*target & !(0xff_u32 << shift)) | (u32::from(value) << shift);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_reg_u32(margo: &Margo, offset: usize) -> u32 {
        (0..4)
            .map(|i| u32::from(margo.read_mmio_u8(offset + i)) << (8 * i))
            .fold(0, |a, b| a | b)
    }

    #[test]
    fn reports_identity_caps_and_display() {
        let mut margo = Margo::default();
        assert_eq!(read_reg_u32(&margo, REG_ID), MARGO_ID_VALUE);
        assert_eq!(read_reg_u32(&margo, REG_CAPS), 0);

        margo.set_mode_640x480x8();
        assert_eq!(read_reg_u32(&margo, REG_DISP_WIDTH), 640);
        assert_eq!(read_reg_u32(&margo, REG_DISP_HEIGHT), 480);
        assert_eq!(read_reg_u32(&margo, REG_DISP_BPP), 8);
        assert_eq!(read_reg_u32(&margo, REG_DISP_PITCH), 640);
    }

    #[test]
    fn disp_start_is_writable_byte_by_byte() {
        let mut margo = Margo::default();
        // Distinct values in every lane prove the byte recombination, not just
        // a single shift.
        margo.write_mmio_u8(REG_DISP_START, 0x01);
        margo.write_mmio_u8(REG_DISP_START + 1, 0x02);
        margo.write_mmio_u8(REG_DISP_START + 2, 0x03);
        margo.write_mmio_u8(REG_DISP_START + 3, 0x04);
        assert_eq!(read_reg_u32(&margo, REG_DISP_START), 0x0403_0201);
    }

    #[test]
    fn disp_dimensions_are_read_only_to_the_bus() {
        let mut margo = Margo::default();
        margo.set_mode_640x480x8();
        margo.write_mmio_u8(REG_DISP_WIDTH, 0); // ignored
        assert_eq!(read_reg_u32(&margo, REG_DISP_WIDTH), 640);
    }

    #[test]
    fn vram_reads_and_writes() {
        let mut margo = Margo::default();
        margo.write_vram_u8(100, 0xab);
        assert_eq!(margo.read_vram_u8(100), 0xab);
        assert_eq!(margo.vram().len(), MARGO_VRAM_SIZE);
    }

    #[test]
    fn visible_surface_tracks_the_mode() {
        let mut margo = Margo::default();
        assert!(margo.visible_surface().is_empty()); // no mode set yet

        margo.set_mode_640x480x8();
        margo.write_vram_u8(0, 0x11);
        let last = 640 * 480 - 1;
        margo.write_vram_u8(last, 0x22);
        // A byte just past the visible surface must not appear in it.
        margo.write_vram_u8(640 * 480, 0x33);

        let surface = margo.visible_surface();
        assert_eq!(surface.len(), 640 * 480);
        assert_eq!(surface[0], 0x11);
        assert_eq!(surface[last], 0x22);
    }
}
