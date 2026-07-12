// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

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
    pub mode_flags: u8,       // idx 5 bits 4..6: odd/even + shift modes
    pub color_dont_care: u8,  // idx 7
    pub bit_mask: u8,         // idx 8
    // idx 6 Miscellaneous Graphics: bit 0 graphics (vs alphanumeric), bit 1 chain
    // odd/even, bits 3-2 memory map select. Stored as written; the fields are
    // decoded by `aperture` (FreeVGA gfxreg.htm 06h).
    pub misc: u8,
}

impl GfxController {
    pub(super) fn mode_odd_even(&self) -> bool {
        self.mode_flags & 0x10 != 0
    }
}

/// The decoded Graphics Controller Miscellaneous register (index 06h): the CPU
/// aperture window the legacy A0000/B0000 mapping points at, plus the two mode
/// flags the bus and the read/write decode consult.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GfxAperture {
    /// Aperture base linear address (A0000, A0000, B0000, or B8000).
    pub base: u32,
    /// Aperture length in bytes (0x20000, 0x10000, 0x8000, or 0x8000).
    pub length: u32,
    /// Misc bit 0: graphics mode (clear = alphanumeric/text).
    pub graphics: bool,
    /// Misc bit 1: chain odd/even enable.
    pub chain_odd_even: bool,
}

impl GfxController {
    /// Decode the Miscellaneous register (06h) into the selected aperture window
    /// and the graphics / chain-odd-even flags. Memory Map Select (bits 3-2):
    /// 00 = A0000-BFFFF (128K), 01 = A0000-AFFFF (64K), 10 = B0000-B7FFF (32K),
    /// 11 = B8000-BFFFF (32K). FreeVGA gfxreg.htm 06h.
    pub fn aperture(&self) -> GfxAperture {
        let (base, length) = match (self.misc >> 2) & 0x03 {
            0b00 => (0xA_0000, 0x2_0000),
            0b01 => (0xA_0000, 0x1_0000),
            0b10 => (0xB_0000, 0x0_8000),
            _ => (0xB_8000, 0x0_8000),
        };
        GfxAperture {
            base,
            length,
            graphics: self.misc & 0x01 != 0,
            chain_odd_even: self.misc & 0x02 != 0,
        }
    }
}

fn apply_logic(logic: u8, value: u8, latch: u8) -> u8 {
    match logic {
        1 => value & latch,
        2 => value | latch,
        3 => value ^ latch,
        _ => value,
    }
}

/// Read one byte through the VGA read datapath, loading the four latches.
/// Spec section 4.
pub fn read_planes(
    planes: &[[u8; 1]; VGA_PLANES],
    gc: &GfxController,
    latches: &mut [u8; VGA_PLANES],
) -> u8 {
    for plane in 0..VGA_PLANES {
        latches[plane] = planes[plane][0];
    }
    if gc.read_mode == 0 {
        return planes[(gc.read_map & 3) as usize][0];
    }
    // Read mode 1: per bit, set the result bit where every cared-about plane
    // matches the corresponding color_compare bit.
    let mut result = 0u8;
    for bit in 0..8 {
        let mut matches = true;
        for (plane, slot) in planes.iter().enumerate() {
            if (gc.color_dont_care >> plane) & 1 == 0 {
                continue;
            }
            let plane_bit = (slot[0] >> bit) & 1;
            let cmp_bit = (gc.color_compare >> plane) & 1;
            if plane_bit != cmp_bit {
                matches = false;
                break;
            }
        }
        if matches {
            result |= 1 << bit;
        }
    }
    result
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
                if (gc.set_reset >> plane) & 1 != 0 {
                    0xFF
                } else {
                    0x00
                } // WM3 color
            }
            _ => {
                // WM0: set/reset substitution where enabled, else rotated data.
                if (gc.enable_set_reset >> plane) & 1 != 0 {
                    if (gc.set_reset >> plane) & 1 != 0 {
                        0xFF
                    } else {
                        0x00
                    }
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

/// Map a display-address counter value `ma` to a per-plane byte offset, applying
/// the CRTC byte/word/doubleword addressing transform and the 16-bit (64 KB)
/// counter wrap. `mode_control` is CRTC index 17h, `underline_loc` is index 14h.
pub fn display_offset(mode_control: u8, underline_loc: u8, ma: u32) -> usize {
    display_offset_row(mode_control, underline_loc, ma, 0)
}

pub fn display_counter(mode_control: u8, underline_loc: u8, row_base: u32, column: u32) -> u32 {
    let divisor = if underline_loc & 0x20 != 0 {
        4
    } else if mode_control & 0x08 != 0 {
        2
    } else {
        1
    };
    row_base + column / divisor
}

pub fn display_offset_row(mode_control: u8, underline_loc: u8, ma: u32, row_scan: u32) -> usize {
    let mut addr = if mode_control & 0x40 != 0 {
        ma // byte mode (CR17 bit 6): identity
    } else if underline_loc & 0x40 != 0 {
        // Doubleword mode (CR14 bit 6): MA0/MA1 are forced low, MA2..MA15 receive
        // A0..A13; CR17 bits 0/1 may still replace MA13/MA14 with row-scan bits.
        ma << 2
    } else {
        // word mode: rotate left 1, MA15 (CR17 bit 5 = 1) or MA13 (= 0) -> bit 0
        let wrap_bit = if mode_control & 0x20 != 0 { 15 } else { 13 };
        (ma << 1) | ((ma >> wrap_bit) & 1)
    };
    if mode_control & 0x01 == 0 {
        addr = (addr & !(1 << 13)) | ((row_scan & 0x01) << 13);
    }
    if mode_control & 0x02 == 0 {
        addr = (addr & !(1 << 14)) | (((row_scan >> 1) & 0x01) << 14);
    }
    (addr as usize) % VGA_PLANE_SIZE
}
