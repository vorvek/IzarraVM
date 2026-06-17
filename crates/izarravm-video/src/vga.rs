//! The legacy VGA core: 256 KB planar VRAM, the VGA register blocks, a
//! cycle-coupled beam clock, and a catch-up rasterizer. This is Margo's
//! VGA-compatibility personality (one chip, one frame store, one RAMDAC).

pub const VGA_PLANE_SIZE: usize = 64 * 1024;
pub const VGA_PLANES: usize = 4;
pub const VGA_PLANAR_SIZE: usize = VGA_PLANE_SIZE * VGA_PLANES; // 256 KB

/// CRTC vertical/horizontal timing, in scan-counter (undoubled) units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrtcTiming {
    pub htotal_chars: u32,
    pub char_width: u32, // 8 or 9, from Sequencer Clocking Mode
    pub hdisp_end: u32,  // dots
    pub vtotal: u32,
    pub vdisp_end: u32,
    pub vblank_start: u32,
    pub vblank_end: u32,
    pub vretrace_start: u32,
    pub vretrace_end: u32,
    pub max_scan: u32,
    pub double_scan: bool,
    pub start_address: u32,
    pub offset: u32,
}

impl CrtcTiming {
    /// Standard 80x25 text (mode 03h): 70 Hz, 9-dot chars. Boot default so the
    /// beam math is valid before any graphics mode-set.
    pub fn text_03h() -> Self {
        Self {
            htotal_chars: 100, char_width: 9, hdisp_end: 720,
            vtotal: 449, vdisp_end: 400, vblank_start: 407, vblank_end: 442,
            vretrace_start: 412, vretrace_end: 414,
            max_scan: 15, double_scan: false,
            start_address: 0, offset: 80,
        }
    }

    /// Mode 0Dh: 320x200x16 planar, 70 Hz, double-scanned, 8-dot chars.
    pub fn mode_0dh() -> Self {
        Self {
            htotal_chars: 100, char_width: 8, hdisp_end: 320,
            vtotal: 449, vdisp_end: 400, vblank_start: 407, vblank_end: 442,
            vretrace_start: 412, vretrace_end: 414,
            max_scan: 1, double_scan: true,
            start_address: 0, offset: 40,
        }
    }

    /// Total dots per frame = htotal_dots * vtotal (scan-counter lines).
    pub fn frame_dots(&self) -> u64 {
        (self.htotal_chars * self.char_width) as u64 * self.vtotal as u64
    }
}

#[derive(Debug, Clone)]
pub struct Vga {
    pub(crate) vram: Vec<u8>,
    pub(crate) crtc: CrtcTiming,
}

impl Default for Vga {
    fn default() -> Self {
        Self {
            vram: vec![0; VGA_PLANAR_SIZE],
            crtc: CrtcTiming::text_03h(),
        }
    }
}

impl Vga {
    pub fn frame_dots(&self) -> u64 {
        self.crtc.frame_dots()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boots_with_defined_frame_dots_and_zeroed_vram() {
        let vga = Vga::default();
        assert_eq!(vga.vram.len(), VGA_PLANAR_SIZE);
        assert!(vga.vram.iter().all(|&b| b == 0));
        // frame_dots must be non-zero at boot (default text timing) so the
        // per-instruction beam advance never divides by zero. (Spec §3/§6.)
        assert!(vga.frame_dots() > 0, "frame_dots must be defined before any mode-set");
    }
}
