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

#[derive(Debug, Clone, Copy, Default)]
pub struct GfxController {
    pub set_reset: u8,        // idx 0, low 4 bits
    pub enable_set_reset: u8, // idx 1, low 4 bits
    pub color_compare: u8,    // idx 2
    pub rotate: u8,           // idx 3 bits 0..2
    pub logic: u8,            // idx 3 bits 3..4: 0 copy,1 AND,2 OR,3 XOR
    pub read_map: u8,         // idx 4
    pub write_mode: u8,       // idx 5 bits 0..1
    pub read_mode: u8,        // idx 5 bit 3
    pub color_dont_care: u8,  // idx 7
    pub bit_mask: u8,         // idx 8
}

fn apply_logic(logic: u8, value: u8, latch: u8) -> u8 {
    match logic {
        1 => value & latch,
        2 => value | latch,
        3 => value ^ latch,
        _ => value,
    }
}

/// Write one byte through the VGA write datapath into all four planes. `planes[i]`
/// is plane i's slice; `latches` are the four latch registers. Spec section 4.
pub fn write_planes(
    planes: &mut [[u8; 1]; VGA_PLANES],
    data: u8,
    gc: &GfxController,
    latches: &[u8; VGA_PLANES],
) {
    let rotated = data.rotate_right(u32::from(gc.rotate & 7));
    for plane in 0..VGA_PLANES {
        let latch = latches[plane];
        let value = match gc.write_mode {
            1 => {
                planes[plane][0] = latch; // WM1: latches straight to planes
                continue;
            }
            2 => {
                if (data >> plane) & 1 != 0 { 0xFF } else { 0x00 } // WM2
            }
            3 => {
                if (gc.set_reset >> plane) & 1 != 0 { 0xFF } else { 0x00 } // WM3 color
            }
            _ => {
                // WM0: set/reset substitution where enabled, else rotated data.
                if (gc.enable_set_reset >> plane) & 1 != 0 {
                    if (gc.set_reset >> plane) & 1 != 0 { 0xFF } else { 0x00 }
                } else {
                    rotated
                }
            }
        };
        let mask = if gc.write_mode == 3 {
            gc.bit_mask & rotated
        } else {
            gc.bit_mask
        };
        let alu = apply_logic(gc.logic, value, latch);
        planes[plane][0] = (alu & mask) | (latch & !mask);
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

    #[test]
    fn write_mode_0_applies_rotate_setreset_logic_and_bitmask() {
        // Latches preloaded to 0xFF on all planes; write 0x0F with bit mask 0xF0,
        // copy logic, no set/reset. Result per plane = (data & mask) | (latch & !mask)
        // = (0x0F & 0xF0) | (0xFF & 0x0F) = 0x00 | 0x0F = 0x0F.
        let mut planes = [[0u8; 1]; VGA_PLANES];
        let mut gc = GfxController::default();
        gc.bit_mask = 0xF0;
        let latches = [0xFFu8; VGA_PLANES];
        write_planes(&mut planes, 0x0F, &gc, &latches);
        for p in &planes {
            assert_eq!(p[0], 0x0F);
        }
    }

    #[test]
    fn write_mode_0_set_reset_substitutes_color_per_plane() {
        // Enable set/reset on all planes, set/reset value = 0b1010 (planes 1 and 3).
        // With full bit mask and copy, each enabled plane writes its set/reset bit
        // expanded to 0xFF or 0x00.
        let mut planes = [[0u8; 1]; VGA_PLANES];
        let mut gc = GfxController::default();
        gc.bit_mask = 0xFF;
        gc.enable_set_reset = 0x0F;
        gc.set_reset = 0b1010;
        let latches = [0u8; VGA_PLANES];
        write_planes(&mut planes, 0x00, &gc, &latches);
        assert_eq!(planes[0][0], 0x00);
        assert_eq!(planes[1][0], 0xFF);
        assert_eq!(planes[2][0], 0x00);
        assert_eq!(planes[3][0], 0xFF);
    }

    #[test]
    fn write_mode_1_copies_latches_to_planes() {
        let mut planes = [[0u8; 1]; VGA_PLANES];
        let mut gc = GfxController::default();
        gc.write_mode = 1;
        let latches = [0x12, 0x34, 0x56, 0x78];
        write_planes(&mut planes, 0x00, &gc, &latches); // data ignored in WM1
        for plane in 0..VGA_PLANES {
            assert_eq!(planes[plane][0], latches[plane]);
        }
    }

    #[test]
    fn write_mode_2_expands_color_nibble_per_plane() {
        let mut planes = [[0u8; 1]; VGA_PLANES];
        let mut gc = GfxController::default();
        gc.write_mode = 2;
        gc.bit_mask = 0xFF;
        let latches = [0u8; VGA_PLANES];
        write_planes(&mut planes, 0b0101, &gc, &latches); // planes 0 and 2 set
        assert_eq!(planes[0][0], 0xFF);
        assert_eq!(planes[1][0], 0x00);
        assert_eq!(planes[2][0], 0xFF);
        assert_eq!(planes[3][0], 0x00);
    }

    #[test]
    fn write_mode_3_uses_set_reset_color_with_rotated_bitmask() {
        // Effective mask = bit_mask (0xFF) & rotated data (0xF0, rotate=0) = 0xF0.
        // Set/Reset 0b0011 -> planes 0,1 color 0xFF, planes 2,3 color 0x00.
        // Result = (color & 0xF0) | (latch 0 & 0x0F).
        let mut planes = [[0u8; 1]; VGA_PLANES];
        let mut gc = GfxController::default();
        gc.write_mode = 3;
        gc.set_reset = 0b0011;
        gc.bit_mask = 0xFF;
        gc.rotate = 0;
        let latches = [0u8; VGA_PLANES];
        write_planes(&mut planes, 0xF0, &gc, &latches);
        assert_eq!(planes[0][0], 0xF0);
        assert_eq!(planes[1][0], 0xF0);
        assert_eq!(planes[2][0], 0x00);
        assert_eq!(planes[3][0], 0x00);
    }
}
