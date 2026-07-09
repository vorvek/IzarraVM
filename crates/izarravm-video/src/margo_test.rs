// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn read_reg_u32(margo: &Margo, offset: usize) -> u32 {
    (0..4)
        .map(|i| u32::from(margo.read_mmio_u8(offset + i)) << (8 * i))
        .fold(0, |a, b| a | b)
}

// Write a 32-bit register through the byte-granular MMIO path.
fn write_reg(margo: &mut Margo, offset: usize, value: u32) {
    for (i, b) in value.to_le_bytes().into_iter().enumerate() {
        margo.write_mmio_u8(offset + i, b);
    }
}

fn setup_fill(margo: &mut Margo) {
    write_reg(margo, REG_DST_BASE, 0);
    write_reg(margo, REG_DST_PITCH, 8);
    write_reg(margo, REG_DEPTH, 1);
    write_reg(margo, REG_DST_XY, (1 << 16) | 1); // y=1, x=1
    write_reg(margo, REG_DIM, (2 << 16) | 2); // h=2, w=2
    write_reg(margo, REG_FG_COLOR, 0x0000_00ab);
    write_reg(margo, REG_ROP, 0xf0);
}

#[path = "margo_blitter_test.rs"]
mod blitter;
#[path = "margo_device_test.rs"]
mod device;
