// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    CHIP_TREX0, CHIP_TREX1, SST_NCC_TABLE0_Q3, SST_NCC_TABLE0_Y0, SST_NCC_TABLE1_Q3,
    SST_NCC_TABLE1_Y0, clamp_ncc, merge_byte, signed_ncc_component,
};

const Y_REGISTER_COUNT: usize = 4;
const I_REGISTER_COUNT: usize = 4;
const PALETTE_WRITE: u32 = 1 << 31;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct NccTable {
    y: [u32; Y_REGISTER_COUNT],
    i: [u32; I_REGISTER_COUNT],
    q: [u32; 4],
}

impl NccTable {
    fn write_byte(&mut self, register: usize, byte: usize, value: u8) {
        merge_byte(self.register_mut(register), byte, value);
    }

    fn write(&mut self, register: usize, value: u32) {
        *self.register_mut(register) = value;
    }

    fn register_mut(&mut self, register: usize) -> &mut u32 {
        match register {
            0..=3 => &mut self.y[register],
            4..=7 => &mut self.i[register - Y_REGISTER_COUNT],
            8..=11 => &mut self.q[register - Y_REGISTER_COUNT - I_REGISTER_COUNT],
            _ => unreachable!("NCC register index is decoded before use"),
        }
    }

    fn color(&self, raw: u8) -> (u8, u8, u8) {
        let y_index = usize::from(raw >> 4);
        let i_index = usize::from((raw >> 2) & 0x03);
        let q_index = usize::from(raw & 0x03);
        let y = ((self.y[y_index >> 2] >> ((y_index & 3) * 8)) & 0xff) as i32;
        let i = self.i[i_index];
        let q = self.q[q_index];
        (
            clamp_ncc(y + signed_ncc_component(i, 18) + signed_ncc_component(q, 18)),
            clamp_ncc(y + signed_ncc_component(i, 9) + signed_ncc_component(q, 9)),
            clamp_ncc(y + signed_ncc_component(i, 0) + signed_ncc_component(q, 0)),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NccState {
    tables: [[NccTable; 2]; 2],
    table0_iq_write: [[u32; 8]; 2],
    palette: [[u32; 256]; 2],
}

impl Default for NccState {
    fn default() -> Self {
        Self {
            tables: [[NccTable::default(); 2]; 2],
            table0_iq_write: [[0; 8]; 2],
            palette: [[0; 256]; 2],
        }
    }
}

impl NccState {
    /// Whether `register` names an NCC table entry at all, without
    /// touching `self`. Slice 1 of
    /// `dev_docs/2026-09-05-distira-async-overlap-review.md`: `write_mmio_u8`
    /// used to call `Distira::raster_owned_mut` (which joins any in-flight
    /// batch) unconditionally before this check, on every register write --
    /// harmless while `join_raster` was a no-op (slice 0b), but under real
    /// async it collapsed the swap's overlap window to "until the next
    /// register write of any kind", which is every register write a Glide
    /// driver issues setting up the next triangle. A write that does not
    /// name an NCC register never needs `RasterOwned` at all -- gating the
    /// join on this cheap, `&self`-free check first is what gets the window
    /// back.
    pub(super) fn touches_register(register: usize) -> bool {
        decode_register(register).is_some()
    }

    pub(super) fn write_register(
        &mut self,
        chip: usize,
        register: usize,
        byte: usize,
        value: u8,
    ) -> bool {
        let Some((table, register)) = decode_register(register) else {
            return false;
        };
        if chip & CHIP_TREX0 != 0 {
            self.write_tmu_register(0, table, register, byte, value);
        }
        if chip & CHIP_TREX1 != 0 {
            self.write_tmu_register(1, table, register, byte, value);
        }
        true
    }

    pub(super) fn color(&self, tmu: usize, table: usize, raw: u8) -> (u8, u8, u8) {
        self.tables[tmu][table].color(raw)
    }

    pub(super) fn palette(&self, tmu: usize, index: usize) -> u32 {
        self.palette[tmu][index]
    }

    fn write_tmu_register(
        &mut self,
        tmu: usize,
        table: usize,
        register: usize,
        byte: usize,
        value: u8,
    ) {
        if table == 0 && register >= 4 {
            self.write_table0_iq_or_palette(tmu, register, byte, value);
        } else {
            self.tables[tmu][table].write_byte(register, byte, value);
        }
    }

    fn write_table0_iq_or_palette(&mut self, tmu: usize, register: usize, byte: usize, value: u8) {
        let slot = &mut self.table0_iq_write[tmu][register - 4];
        merge_byte(slot, byte, value);
        if byte != 3 {
            return;
        }

        let raw = *slot;
        if raw & PALETTE_WRITE != 0 {
            let index = ((raw >> 23) & 0xfe) as usize | (register & 1);
            self.palette[tmu][index] = raw | 0xff00_0000;
        } else {
            self.tables[tmu][0].write(register, raw);
        }
    }
}

fn decode_register(register: usize) -> Option<(usize, usize)> {
    if (SST_NCC_TABLE0_Y0..=SST_NCC_TABLE0_Q3).contains(&register) {
        return (register - SST_NCC_TABLE0_Y0)
            .is_multiple_of(4)
            .then_some((0, (register - SST_NCC_TABLE0_Y0) / 4));
    }
    if (SST_NCC_TABLE1_Y0..=SST_NCC_TABLE1_Q3).contains(&register) {
        return (register - SST_NCC_TABLE1_Y0)
            .is_multiple_of(4)
            .then_some((1, (register - SST_NCC_TABLE1_Y0) / 4));
    }
    None
}
