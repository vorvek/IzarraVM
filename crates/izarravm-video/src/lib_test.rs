// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn mode13h_framebuffer_has_expected_size() {
    let framebuffer = Framebuffer::mode13h();
    assert_eq!(framebuffer.width, 320);
    assert_eq!(framebuffer.height, 200);
    assert_eq!(framebuffer.indexed_pixels.len(), MODE13H_MEMORY_SIZE);
}

#[test]
fn default_dac_matches_stock_vga() {
    // The stock VGA mode-13h default palette (vgabios palette3). Key entries:
    // 0 black, 1 EGA blue (6-bit 0x2A -> 8-bit 0xAA), 6 brown, 15 white, and
    // 255 black (the source's tail is all black).
    let dac = Dac::default();
    assert_eq!(dac.rgb888(0), (0, 0, 0));
    assert_eq!(dac.rgb888(1), (0, 0, 0xAA));
    assert_eq!(dac.rgb888(6), (0xAA, 0x55, 0x00));
    assert_eq!(dac.rgb888(15), (0xFF, 0xFF, 0xFF));
    assert_eq!(dac.rgb888(255), (0, 0, 0));
}
