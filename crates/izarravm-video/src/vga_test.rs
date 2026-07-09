// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const BIOS_TEXT_WHITE: u8 = 0x3F;

/// Write a character/attribute pair into a text cell (row, col).
fn text_put(vga: &mut Vga, row: usize, col: usize, ch: u8, attr: u8) {
    let i = row * vga.text_columns + col;
    vga.write_u8(i * 2, ch).unwrap();
    vga.write_u8(i * 2 + 1, attr).unwrap();
}

#[path = "vga_cga_test.rs"]
mod cga;
#[path = "vga_core_test.rs"]
mod core_behavior;
#[path = "vga_graphics_modes_test.rs"]
mod graphics_modes;
#[path = "vga_hercules_test.rs"]
mod hercules;
#[path = "vga_output_control_test.rs"]
mod output_control;
#[path = "vga_palette_status_test.rs"]
mod palette_status;
#[path = "vga_text_test.rs"]
mod text;
