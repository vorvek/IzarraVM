//! Guest-visible regression-test device, modelled on 86Box's Unit Tester
//! (`src/device/unittester.c`) but done the Izarra-native way: a dedicated
//! Lotura register file instead of 86Box's magic-sequence-on-port-0x80, since a
//! fixed fantasy machine has free ports to spare.
//!
//! It lets a guest test program (a boot-suite .asm image or a Toka-DOS .COM)
//! drive the emulator to a known video state and self-check it: ask for the
//! zlib CRC-32 of a framebuffer rectangle, snapshot the screen to a host file
//! for baseline capture, and exit the machine with a code for CI. The guest can
//! compare the returned CRC against an embedded known-good value and report
//! through the existing RESULT_BLOCK + HLT path, or just `Exit` with a code.
//!
//! Wire protocol (byte I/O only, like the rest of the bus):
//!
//! - `0xE4` index: write selects a register-file offset; read returns it.
//! - `0xE5` data: write stores a byte at the index and post-increments it; read
//!   returns the byte at the index and post-increments.
//! - `0xE6` command: write executes a command; read returns 0 (always ready,
//!   because the run loop resolves a command before the guest's next instruction
//!   can read back).
//!
//! Register file (little-endian):
//!   [0..2] X   [2..4] Y   [4..6] W   [6..8] H   (rectangle, set before CRC)
//!   [8..12] CRC result    [12] exit code (set before Exit)
//!   [16] benchmark selector  [17..21] iterations  [21..25] aux  [25] status

/// I/O ports. 0xE0-0xE3 are the other Lotura registers; this device owns the
/// next three.
pub const PORT_INDEX: u16 = 0xE4;
pub const PORT_DATA: u16 = 0xE5;
pub const PORT_COMMAND: u16 = 0xE6;

/// Register-file offsets.
pub const REG_X: usize = 0;
pub const REG_Y: usize = 2;
pub const REG_W: usize = 4;
pub const REG_H: usize = 6;
pub const REG_CRC: usize = 8;
pub const REG_EXIT: usize = 12;

/// Neurketa benchmark region, above the video-test registers. The host preloads
/// REG_SELECTOR before boot; the guest reads it to pick a payload, then writes
/// its iteration count, aux value, and status before CMD_EXIT.
pub const REG_SELECTOR: usize = 16;
pub const REG_RESULT_ITER: usize = 17;
pub const REG_RESULT_AUX: usize = 21;
pub const REG_RESULT_STATUS: usize = 25;

/// Commands written to `PORT_COMMAND`.
pub const CMD_CRC: u8 = 1;
pub const CMD_SNAPSHOT: u8 = 2;
pub const CMD_EXIT: u8 = 3;

const REG_FILE_SIZE: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitTester {
    index: usize,
    regs: [u8; REG_FILE_SIZE],
    /// A command written this cycle, awaiting the run loop (which needs &mut
    /// Machine to read the framebuffer / touch the host filesystem / stop).
    pending: Option<u8>,
}

impl Default for UnitTester {
    fn default() -> Self {
        Self {
            index: 0,
            regs: [0; REG_FILE_SIZE],
            pending: None,
        }
    }
}

impl UnitTester {
    /// Handle a port read; `None` if the port is not ours.
    pub fn read_port(&mut self, port: u16) -> Option<u8> {
        match port {
            PORT_INDEX => Some(self.index as u8),
            PORT_DATA => {
                let value = self.regs.get(self.index).copied().unwrap_or(0);
                self.advance_index();
                Some(value)
            }
            PORT_COMMAND => Some(0),
            _ => None,
        }
    }

    /// Handle a port write; `false` if the port is not ours.
    pub fn write_port(&mut self, port: u16, value: u8) -> bool {
        match port {
            PORT_INDEX => {
                self.index = usize::from(value);
                true
            }
            PORT_DATA => {
                if let Some(slot) = self.regs.get_mut(self.index) {
                    *slot = value;
                }
                self.advance_index();
                true
            }
            PORT_COMMAND => {
                self.pending = Some(value);
                true
            }
            _ => false,
        }
    }

    fn advance_index(&mut self) {
        self.index = (self.index + 1) % REG_FILE_SIZE;
    }

    /// The command awaiting deferred execution, cleared on read.
    pub fn take_pending(&mut self) -> Option<u8> {
        self.pending.take()
    }

    /// The rectangle the guest programmed, as `(x, y, w, h)`.
    pub fn rect(&self) -> (u16, u16, u16, u16) {
        (
            u16::from_le_bytes([self.regs[REG_X], self.regs[REG_X + 1]]),
            u16::from_le_bytes([self.regs[REG_Y], self.regs[REG_Y + 1]]),
            u16::from_le_bytes([self.regs[REG_W], self.regs[REG_W + 1]]),
            u16::from_le_bytes([self.regs[REG_H], self.regs[REG_H + 1]]),
        )
    }

    /// Store a computed CRC so the guest can read it back at `REG_CRC`.
    pub fn set_crc(&mut self, crc: u32) {
        self.regs[REG_CRC..REG_CRC + 4].copy_from_slice(&crc.to_le_bytes());
    }

    /// The exit code the guest programmed at `REG_EXIT`.
    pub fn exit_code(&self) -> u8 {
        self.regs[REG_EXIT]
    }

    /// Read a single register-file byte, 0 if the offset is out of range.
    pub fn reg_u8(&self, offset: usize) -> u8 {
        self.regs.get(offset).copied().unwrap_or(0)
    }

    /// Write a single register-file byte; ignored if the offset is out of range.
    pub fn set_reg_u8(&mut self, offset: usize, value: u8) {
        if let Some(slot) = self.regs.get_mut(offset) {
            *slot = value;
        }
    }

    /// Read a little-endian u32 starting at `offset`, missing bytes treated as 0.
    pub fn reg_u32(&self, offset: usize) -> u32 {
        let byte = |i: usize| self.regs.get(offset + i).copied().unwrap_or(0);
        u32::from_le_bytes([byte(0), byte(1), byte(2), byte(3)])
    }
}

/// Standard zlib/IEEE CRC-32 (polynomial 0xEDB88320), the same value 86Box's
/// Unit Tester returns. Bit-by-bit so no 1 KiB table is carried for a function
/// called a handful of times per test run.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
#[path = "unittester_test.rs"]
mod tests;
