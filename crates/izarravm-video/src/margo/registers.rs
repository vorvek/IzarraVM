// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::Margo;

pub const MARGO_MMIO_SIZE: usize = 0x0001_0000; // 64 KB register block
pub const MARGO_ID_VALUE: u32 = 0x4D47_0100; // 'M' 'G', version 1.00
pub const MARGO_CAPS_VALUE: u32 = 0x0000_0fff; // bits 0 FILL, 1 COPY, 2 COLOR_EXPAND, 3 LINE, 4 ROP3, 5 CLIP, 6 COLORKEY, 7 PATTERN_FILL, 8 CURSOR, 9 OVERLAY, 10 PUSHER, 11 DITHER

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
pub const REG_CURSOR_CTRL: usize = 0x0028;
pub const REG_CURSOR_ADDR: usize = 0x002c;
pub const REG_CURSOR_POS: usize = 0x0030;
pub const REG_CURSOR_FG: usize = 0x0034;
pub const REG_CURSOR_BG: usize = 0x0038;
pub const REG_OVL_CTRL: usize = 0x0040;
pub const REG_OVL_SRC_Y: usize = 0x0044;
pub const REG_OVL_SRC_PITCH: usize = 0x0048;
pub const REG_OVL_SRC_DIM: usize = 0x004c;
pub const REG_OVL_SRC_U: usize = 0x0050;
pub const REG_OVL_SRC_V: usize = 0x0054;
pub const REG_OVL_DST_XY: usize = 0x0058;
pub const REG_OVL_DST_DIM: usize = 0x005c;
pub const REG_OVL_COLORKEY: usize = 0x0060;

pub const REG_PUSH_CTRL: usize = 0x0080;
pub const REG_PUSH_BASE: usize = 0x0084;
pub const REG_PUSH_SIZE: usize = 0x0088;
pub const REG_PUSH_PUT: usize = 0x008c;
pub const REG_PUSH_GET: usize = 0x0090;

// Blit engine registers (section 7.3). All R/W; the engine reads the ones it
// needs when COMMAND fires. The block 0x100..0x150 is a flat R/W store.
pub const REG_DST_BASE: usize = 0x0100;
pub const REG_DST_PITCH: usize = 0x0104;
pub const REG_SRC_BASE: usize = 0x0108;
pub const REG_SRC_PITCH: usize = 0x010c;
pub const REG_DEPTH: usize = 0x0110;
pub const REG_DST_XY: usize = 0x0114;
pub const REG_SRC_XY: usize = 0x0118;
pub const REG_DIM: usize = 0x011c;
pub const REG_FG_COLOR: usize = 0x0120;
pub const REG_BG_COLOR: usize = 0x0124;
pub const REG_ROP: usize = 0x0128;
pub const REG_COLORKEY: usize = 0x012c;
pub const REG_FLAGS: usize = 0x0130;
pub const REG_CLIP_TL: usize = 0x0134;
pub const REG_CLIP_BR: usize = 0x0138;
pub const REG_LINE_START: usize = 0x013c;
pub const REG_LINE_END: usize = 0x0140;
pub const REG_PAT_BASE: usize = 0x0144;
pub const REG_COMMAND: usize = 0x0150;
pub const REG_MONO_DATA: usize = 0x0160;

pub(super) const CURSOR_BASE: usize = 0x0028;
pub(super) const CURSOR_REGS: usize = 5; // 0x0028..0x003C: CTRL, ADDR, POS, FG, BG
pub(super) const OVL_BASE: usize = 0x0040;
pub(super) const OVL_REGS: usize = 9; // 0x0040..=0x0060: CTRL, SRC_Y, SRC_PITCH, SRC_DIM, SRC_U, SRC_V, DST_XY, DST_DIM, COLORKEY
pub(super) const PUSH_BASE_REG: usize = 0x0080;
pub(super) const PUSH_REGS: usize = 5; // 0x0080..=0x0090: CTRL, BASE, SIZE, PUT, GET
pub(super) const BLIT_BASE: usize = 0x0100;
pub(super) const BLIT_REGS: usize = 20; // 0x100..0x150, twenty 32-bit slots; COMMAND at 0x150 is handled separately

/// The DMA pusher's register state, read by the machine that drives the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PusherRegs {
    pub enabled: bool,
    pub base: u32,
    pub size: u32,
    pub put: u32,
    pub get: u32,
}

impl Margo {
    /// The DMA pusher registers, for the machine-level engine (section 7.9).
    pub fn pusher(&self) -> PusherRegs {
        PusherRegs {
            enabled: self.pusher[0] & 0x1 != 0,
            base: self.pusher[1],
            size: self.pusher[2],
            put: self.pusher[3],
            get: self.pusher[4],
        }
    }

    /// Advance the pusher's read offset PUSH_GET (engine-owned; read-only to the bus).
    pub fn set_pusher_get(&mut self, get: u32) {
        self.pusher[4] = get;
    }
}
