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

/// Commands written to `PORT_COMMAND`.
pub const CMD_CRC: u8 = 1;
pub const CMD_SNAPSHOT: u8 = 2;
pub const CMD_EXIT: u8 = 3;

const REG_FILE_SIZE: usize = 16;

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
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_zlib_check_value() {
        // The canonical CRC-32 check: "123456789" -> 0xCBF43926.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn data_register_round_trips_and_auto_increments() {
        let mut ut = UnitTester::default();
        ut.write_port(PORT_INDEX, REG_W as u8);
        ut.write_port(PORT_DATA, 0x40); // W low
        ut.write_port(PORT_DATA, 0x01); // W high -> 0x0140 = 320
        ut.write_port(PORT_INDEX, REG_W as u8);
        assert_eq!(ut.read_port(PORT_DATA), Some(0x40));
        assert_eq!(ut.read_port(PORT_DATA), Some(0x01));
        let (_, _, w, _) = ut.rect();
        assert_eq!(w, 320);
    }

    #[test]
    fn command_write_is_latched_for_deferred_execution() {
        let mut ut = UnitTester::default();
        assert_eq!(ut.read_port(PORT_COMMAND), Some(0)); // ready
        ut.write_port(PORT_COMMAND, CMD_CRC);
        assert_eq!(ut.take_pending(), Some(CMD_CRC));
        assert_eq!(ut.take_pending(), None);
    }
}
