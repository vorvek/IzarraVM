//! IDE/ATA register file for the secondary channel, hosting the ATAPI CD-ROM as
//! the secondary master.
//!
//! Channel choice: the CD-ROM lives on the **secondary** IDE channel, the
//! conventional home for an optical drive, at command block 0x170-0x177, control
//! block 0x376, IRQ15. The primary channel (0x1F0/0x3F6, IRQ14) is left free for
//! a future hard disk. Only the master device (drive 0) is populated; selecting
//! the slave reads back a not-present status.
//!
//! ATAPI handshake modeled (SFF-8020i): the host issues the ATA PACKET command
//! (0xA0) to the command register, then writes the 12-byte command descriptor
//! block to the data register. The device runs the packet and, for a data-in
//! command, presents the result through the data register with the byte-count
//! limit (cylinder low/high) set and DRQ raised, asserting IRQ15 if interrupts
//! are enabled. IDENTIFY PACKET DEVICE (0xA1) and the ATA soft-reset path are
//! handled directly. DMA is not modeled: transfers are PIO, which every ATAPI
//! driver and ICDEX supports.

use crate::atapi::{AtapiDevice, CmdResult};
use crate::cdimage::DATA_SECTOR;

/// Secondary-channel command-block base (0x170-0x177).
pub const SECONDARY_CMD_BASE: u16 = 0x170;
/// Secondary-channel control/alt-status port.
pub const SECONDARY_CTRL: u16 = 0x376;
/// The IRQ the secondary channel raises on command completion.
pub const SECONDARY_IRQ: u8 = 15;

/// ATA status register bits.
mod status {
    pub const ERR: u8 = 0x01; // error
    pub const DRQ: u8 = 0x08; // data request: a PIO transfer is ready
    pub const DSC: u8 = 0x10; // device seek complete / service
    pub const DRDY: u8 = 0x40; // device ready
    // BSY (0x80) is never set: the model runs each command synchronously inside
    // the port write, so the host never observes a busy window.
}

/// What the register file is waiting for on the data port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Idle: no command in flight.
    Idle,
    /// Awaiting the 12-byte command packet (after a 0xA0 PACKET command).
    AwaitPacket,
    /// Presenting data-in bytes to the host (the buffer is being drained).
    DataIn,
}

/// One IDE channel hosting a single ATAPI device as the master.
#[derive(Debug)]
pub struct IdeChannel {
    device: AtapiDevice,
    // ATA task-file registers.
    features: u8,
    sector_count: u8,
    lba_low: u8,
    lba_mid: u8,      // byte-count low for ATAPI
    lba_high: u8,     // byte-count high for ATAPI
    drive_select: u8, // bit 4 selects master(0)/slave(1)
    status: u8,
    error: u8,
    /// nIEN (control register bit 1): when set, interrupts are disabled.
    interrupts_disabled: bool,
    phase: Phase,
    /// The 12-byte packet being assembled on the data port.
    packet: [u8; 12],
    packet_filled: usize,
    /// The data-in buffer being drained on the data port, and the cursor into it.
    data_in: Vec<u8>,
    data_in_pos: usize,
    /// End offset (exclusive) of the DRQ block currently presented to the host.
    /// When the cursor reaches it the next block is armed, or the phase ends.
    data_in_block_end: usize,
    /// Per-command host byte-count limit (cylinder low/high at PACKET time). Zero
    /// means no limit was programmed, so the whole buffer goes out in one block.
    byte_count_limit: usize,
    /// Set when a command completes so the machine forwards IRQ15 to the PIC.
    irq_pending: bool,
    /// Pending mechanical time (seconds) for the last data command, drained by
    /// the machine so a read costs wall-clock time like the floppy.
    pending_stall: f64,
    /// Bytes moved by the last data command, for the access LED.
    last_access_bytes: usize,
}

impl Default for IdeChannel {
    fn default() -> Self {
        Self {
            device: AtapiDevice::new(),
            features: 0,
            sector_count: 0,
            lba_low: 0,
            lba_mid: 0,
            lba_high: 0,
            drive_select: 0,
            status: status::DRDY | status::DSC,
            error: 0,
            interrupts_disabled: false,
            phase: Phase::Idle,
            packet: [0u8; 12],
            packet_filled: 0,
            data_in: Vec::new(),
            data_in_pos: 0,
            data_in_block_end: 0,
            byte_count_limit: 0,
            irq_pending: false,
            pending_stall: 0.0,
            last_access_bytes: 0,
        }
    }
}

impl IdeChannel {
    pub fn new() -> Self {
        let mut channel = Self::default();
        // Power-up presents the same diagnostic code and ATAPI signature as a
        // hardware reset (ATA 5.2.9): device 0 passed in the Error register and
        // the packet-device signature in the byte-count registers, so the BIOS
        // sees them immediately without first issuing a reset.
        channel.soft_reset();
        channel
    }

    pub fn device(&self) -> &AtapiDevice {
        &self.device
    }

    pub fn device_mut(&mut self) -> &mut AtapiDevice {
        &mut self.device
    }

    /// Whether the master device is selected (drive bit 4 == 0).
    fn master_selected(&self) -> bool {
        self.drive_select & 0x10 == 0
    }

    /// Take the pending IRQ flag (the machine forwards it to the PIC). Honors
    /// nIEN: a disabled-interrupt channel never forwards.
    pub fn take_irq(&mut self) -> bool {
        let pending = self.irq_pending && !self.interrupts_disabled;
        self.irq_pending = false;
        pending
    }

    /// Take and clear the pending mechanical-time charge (seconds) for the last
    /// data command, so the machine can stall the guest the way the floppy does.
    pub fn take_stall_secs(&mut self) -> f64 {
        let secs = self.pending_stall;
        self.pending_stall = 0.0;
        secs
    }

    /// Take and clear the access-byte count for the GUI LED.
    pub fn take_access_bytes(&mut self) -> usize {
        let bytes = self.last_access_bytes;
        self.last_access_bytes = 0;
        bytes
    }

    /// Whether a given port belongs to this channel.
    pub fn owns_port(port: u16) -> bool {
        (SECONDARY_CMD_BASE..=SECONDARY_CMD_BASE + 7).contains(&port) || port == SECONDARY_CTRL
    }

    /// Read one byte from a channel port. The data register (0x170) returns the
    /// next data-in byte; the rest return their task-file values.
    pub fn read_port(&mut self, port: u16) -> Option<u8> {
        if port == SECONDARY_CTRL {
            // Alt status: the status register without clearing the IRQ.
            return Some(self.status);
        }
        if !(SECONDARY_CMD_BASE..=SECONDARY_CMD_BASE + 7).contains(&port) {
            return None;
        }
        let reg = port - SECONDARY_CMD_BASE;
        let value = match reg {
            0 => self.read_data_byte(),
            1 => self.error,
            2 => self.sector_count,
            3 => self.lba_low,
            4 => self.lba_mid,
            5 => self.lba_high,
            6 => self.drive_select,
            7 => {
                // Reading the status register clears a pending interrupt latch on
                // hardware; the machine has already (or will) forward it.
                self.irq_pending = false;
                self.status
            }
            _ => 0xFF,
        };
        Some(value)
    }

    /// Write one byte to a channel port. Word writes to the data register split
    /// into two byte writes at the bus layer, so the packet/data path is byte-fed.
    pub fn write_port(&mut self, port: u16, value: u8) -> bool {
        if port == SECONDARY_CTRL {
            // Device control: bit 1 = nIEN, bit 2 = SRST (soft reset).
            self.interrupts_disabled = value & 0x02 != 0;
            if value & 0x04 != 0 {
                self.soft_reset();
            }
            return true;
        }
        if !(SECONDARY_CMD_BASE..=SECONDARY_CMD_BASE + 7).contains(&port) {
            return false;
        }
        let reg = port - SECONDARY_CMD_BASE;
        match reg {
            0 => self.write_data_byte(value),
            1 => self.features = value,
            2 => self.sector_count = value,
            3 => self.lba_low = value,
            4 => self.lba_mid = value,
            5 => self.lba_high = value,
            6 => self.drive_select = value,
            7 => self.write_command(value),
            _ => {}
        }
        true
    }

    fn soft_reset(&mut self) {
        self.phase = Phase::Idle;
        self.packet_filled = 0;
        self.data_in.clear();
        self.data_in_pos = 0;
        self.data_in_block_end = 0;
        self.byte_count_limit = 0;
        self.status = status::DRDY | status::DSC;
        // Diagnostic code: device 0 passed (ATA 5.2.9). `new` runs this on
        // construction so power-up presents the same code, as real hardware does.
        self.error = 0x01;
        // ATAPI signature on the byte-count registers so the host can tell a
        // packet device from an ATA disk after reset.
        self.sector_count = 0x01;
        self.lba_low = 0x01;
        self.lba_mid = 0x14;
        self.lba_high = 0xEB;
    }

    fn write_command(&mut self, command: u8) {
        if !self.master_selected() {
            // No slave device: a command to it sets ERR.
            self.status = status::ERR;
            return;
        }
        match command {
            0xA0 => self.begin_packet(),
            0xA1 => self.identify_packet_device(),
            0x08 => self.soft_reset(),   // DEVICE RESET
            0xEC => self.identify_nak(), // IDENTIFY DEVICE: ATAPI aborts it
            0x90 => self.execute_diagnostic(),
            0x00 => {
                // NOP (ATA-3 7.13): always aborts, never a silent success.
                self.status = status::DRDY | status::ERR;
                self.error = 0x04; // ABRT
            }
            _ => {
                // Unsupported command: abort.
                self.status = status::DRDY | status::ERR;
                self.error = 0x04; // ABRT
            }
        }
    }

    /// EXECUTE DEVICE DIAGNOSTIC (0x90): mandatory, and the BIOS probes through
    /// it. Report device 0 passed and leave the ATAPI signature so detection
    /// still sees a packet device. Completes without ERR and raises the IRQ.
    fn execute_diagnostic(&mut self) {
        self.error = 0x01; // device 0 passed diagnostics
        self.sector_count = 0x01;
        self.lba_low = 0x01;
        self.lba_mid = 0x14;
        self.lba_high = 0xEB;
        self.status = status::DRDY | status::DSC;
        self.raise_irq();
    }

    /// ATA PACKET (0xA0): the device prepares to receive the 12-byte CDB on the
    /// data register, raising DRQ.
    fn begin_packet(&mut self) {
        self.phase = Phase::AwaitPacket;
        self.packet_filled = 0;
        self.packet = [0u8; 12];
        // The host has already written the byte-count limit (cylinder low/high)
        // before issuing PACKET. Capture it now so run_packet can chunk a large
        // data-in transfer into DRQ blocks no bigger than the limit.
        self.byte_count_limit = u16::from_le_bytes([self.lba_mid, self.lba_high]) as usize;
        self.status = status::DRDY | status::DRQ;
        self.error = 0;
    }

    /// IDENTIFY PACKET DEVICE (0xA1): present the 512-byte identify block.
    fn identify_packet_device(&mut self) {
        let block = identify_block();
        self.data_in = block;
        self.data_in_pos = 0;
        self.phase = Phase::DataIn;
        // IDENTIFY ignores the host byte-count limit: the whole block is one DRQ.
        self.byte_count_limit = 0;
        self.present_data_block();
        self.raise_irq();
    }

    /// IDENTIFY DEVICE (0xEC) on an ATAPI device aborts with the ATAPI signature
    /// left in place, the standard way a host learns the device is packet-only.
    fn identify_nak(&mut self) {
        self.status = status::DRDY | status::ERR;
        self.error = 0x04; // ABRT
        self.sector_count = 0x01;
        self.lba_low = 0x01;
        self.lba_mid = 0x14;
        self.lba_high = 0xEB;
    }

    fn read_data_byte(&mut self) -> u8 {
        if self.phase != Phase::DataIn {
            return 0;
        }
        let byte = self.data_in.get(self.data_in_pos).copied().unwrap_or(0);
        self.data_in_pos += 1;
        if self.data_in_pos >= self.data_in.len() {
            // Whole transfer complete: drop DRQ, go idle.
            self.phase = Phase::Idle;
            self.data_in.clear();
            self.data_in_pos = 0;
            self.data_in_block_end = 0;
            self.status = status::DRDY | status::DSC;
        } else if self.data_in_pos >= self.data_in_block_end {
            // Block done but more data remains: arm the next DRQ block and pulse
            // the IRQ, the way ATAPI signals the host to drain the next block.
            self.present_data_block();
            self.raise_irq();
        }
        byte
    }

    fn write_data_byte(&mut self, value: u8) {
        if self.phase != Phase::AwaitPacket {
            return;
        }
        if self.packet_filled < self.packet.len() {
            self.packet[self.packet_filled] = value;
            self.packet_filled += 1;
        }
        if self.packet_filled == self.packet.len() {
            self.run_packet();
        }
    }

    /// Execute the assembled 12-byte CDB through the ATAPI device and set up the
    /// data-in phase (or completion) accordingly.
    fn run_packet(&mut self) {
        let cdb = self.packet;
        match self.device.execute(&cdb) {
            CmdResult::Data(buf) => {
                self.charge_time(cdb[0], &buf);
                if buf.is_empty() {
                    // Non-data command: complete with DRDY, raise the IRQ.
                    self.phase = Phase::Idle;
                    self.status = status::DRDY | status::DSC;
                    self.error = 0;
                } else {
                    // Data-in: present the first DRQ block. The host byte-count
                    // limit caps each block; the rest go out as the host drains.
                    self.data_in = buf;
                    self.data_in_pos = 0;
                    self.phase = Phase::DataIn;
                    self.error = 0;
                    self.present_data_block();
                }
            }
            CmdResult::Error => {
                self.phase = Phase::Idle;
                self.status = status::DRDY | status::ERR;
                self.error = 0x04; // ABRT / CHECK CONDITION (sense already latched)
            }
        }
        self.raise_irq();
    }

    /// Arm the next data-in DRQ block at the current cursor: set the byte count
    /// to this block's size, raise DRQ, and raise the IRQ. The block is the
    /// remaining bytes capped by the host byte-count limit, or the whole
    /// remainder when no limit (or a limit at least as large) was programmed.
    fn present_data_block(&mut self) {
        let remaining = self.data_in.len() - self.data_in_pos;
        let block = if self.byte_count_limit > 0 && self.byte_count_limit < remaining {
            self.byte_count_limit
        } else {
            remaining
        };
        self.data_in_block_end = self.data_in_pos + block;
        self.lba_mid = (block & 0xFF) as u8;
        self.lba_high = ((block >> 8) & 0xFF) as u8;
        self.status = status::DRDY | status::DRQ | status::DSC;
    }

    /// Charge the mechanical time and access-byte count for a read command, the
    /// way the floppy charges seek + transfer. Only the data-returning READ
    /// commands cost time; control commands are instant.
    fn charge_time(&mut self, opcode: u8, buf: &[u8]) {
        if matches!(opcode, 0x28 | 0xA8) && !buf.is_empty() {
            let bytes = buf.len();
            self.last_access_bytes = bytes;
            // A fixed seek component plus the 12x transfer time. Pragmatic: a
            // fraction of the full-stroke seek, independent of head position
            // (the model does not track CD head geometry).
            let transfer = bytes as f64 / crate::atapi::CD_BYTES_PER_SEC;
            let seek = crate::atapi::CD_SEEK_MAX_SECS * 0.2;
            self.pending_stall += seek + transfer;
        }
        let _ = DATA_SECTOR;
    }

    fn raise_irq(&mut self) {
        self.irq_pending = true;
    }
}

/// The 512-byte IDENTIFY PACKET DEVICE response. Word 0 marks an ATAPI removable
/// CD-ROM; the model number and firmware strings are byte-swapped ASCII per ATA.
fn identify_block() -> Vec<u8> {
    let mut words = [0u16; 256];
    // General config: bits 15-14 = 10b (ATAPI device), bits 12-8 = 0x05 (CD-ROM
    // command set), bits 6-5 = removable, bits 1-0 = 0 (12-byte packet).
    words[0] = 0x85C0;
    // Capabilities: DMA + LBA supported bits (cosmetic for PIO use).
    words[49] = 0x0300;
    // ATAPI signature fields per ATA-4 word 0 already cover the type.
    put_string(&mut words[10..20], "IZARRA-CD-0001"); // serial number
    put_string(&mut words[23..27], "1.0 "); // firmware revision
    put_string(&mut words[27..47], "Izarra CD-ROM 12X"); // model number
    // Field validity / ATAPI specifics left at defaults.

    let mut bytes = Vec::with_capacity(512);
    for w in words {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    bytes
}

/// Write an ASCII string into an ATA word field with the byte-swap ATA uses
/// (first char in the high byte of the first word). Space-padded.
fn put_string(words: &mut [u16], text: &str) {
    let src = text.as_bytes();
    let byte_at = |i: usize| -> u8 { src.get(i).copied().unwrap_or(b' ') };
    for (i, w) in words.iter_mut().enumerate() {
        let hi = byte_at(i * 2);
        let lo = byte_at(i * 2 + 1);
        *w = (u16::from(hi) << 8) | u16::from(lo);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdimage::{CdImage, DATA_SECTOR};

    fn data_disc(sectors: u32) -> CdImage {
        let mut bytes = vec![0u8; sectors as usize * DATA_SECTOR];
        for s in 0..sectors as usize {
            bytes[s * DATA_SECTOR] = (s as u8).wrapping_add(0x50);
        }
        CdImage::from_iso(bytes).unwrap()
    }

    /// Drive the full PACKET handshake for a READ(10) of one sector at `lba` and
    /// return the drained data-in buffer.
    fn packet_read10(ch: &mut IdeChannel, lba: u32) -> Vec<u8> {
        // PACKET command.
        ch.write_port(SECONDARY_CMD_BASE + 7, 0xA0);
        assert_eq!(ch.status & status::DRQ, status::DRQ);
        // Feed the 12-byte CDB through the data register.
        let mut cdb = [0u8; 12];
        cdb[0] = 0x28;
        cdb[2..6].copy_from_slice(&lba.to_be_bytes());
        cdb[7] = 0;
        cdb[8] = 1; // one sector
        for b in cdb {
            ch.write_port(SECONDARY_CMD_BASE, b);
        }
        // After the packet, data-in is armed and the byte count is set.
        let count = u16::from_le_bytes([ch.lba_mid, ch.lba_high]) as usize;
        assert_eq!(count, DATA_SECTOR);
        assert_eq!(ch.status & status::DRQ, status::DRQ);
        // Drain the data register.
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            out.push(ch.read_port(SECONDARY_CMD_BASE).unwrap());
        }
        out
    }

    #[test]
    fn packet_read10_round_trips_a_sector() {
        let mut ch = IdeChannel::new();
        ch.device_mut().insert(data_disc(8));
        // Clear the post-insert unit attention with a TEST UNIT READY.
        ch.write_port(SECONDARY_CMD_BASE + 7, 0xA0);
        for b in [0u8; 12] {
            ch.write_port(SECONDARY_CMD_BASE, b);
        }
        let buf = packet_read10(&mut ch, 2);
        assert_eq!(buf.len(), DATA_SECTOR);
        assert_eq!(buf[0], 0x52); // 0x50 + 2
        // After draining, DRQ drops and the channel is idle/ready.
        assert_eq!(ch.status & status::DRQ, 0);
        assert_eq!(ch.status & status::DRDY, status::DRDY);
    }

    #[test]
    fn packet_command_raises_irq_when_enabled() {
        let mut ch = IdeChannel::new();
        ch.device_mut().insert(data_disc(4));
        // Run a TEST UNIT READY packet.
        ch.write_port(SECONDARY_CMD_BASE + 7, 0xA0);
        for b in [0u8; 12] {
            ch.write_port(SECONDARY_CMD_BASE, b);
        }
        assert!(ch.take_irq());
        // A second take clears it.
        assert!(!ch.take_irq());
    }

    #[test]
    fn nien_suppresses_the_irq() {
        let mut ch = IdeChannel::new();
        ch.device_mut().insert(data_disc(4));
        // Set nIEN via the control register.
        ch.write_port(SECONDARY_CTRL, 0x02);
        ch.write_port(SECONDARY_CMD_BASE + 7, 0xA0);
        for b in [0u8; 12] {
            ch.write_port(SECONDARY_CMD_BASE, b);
        }
        assert!(!ch.take_irq());
    }

    #[test]
    fn identify_packet_device_returns_512_bytes() {
        let mut ch = IdeChannel::new();
        ch.write_port(SECONDARY_CMD_BASE + 7, 0xA1);
        let count = u16::from_le_bytes([ch.lba_mid, ch.lba_high]) as usize;
        assert_eq!(count, 512);
        let mut block = Vec::new();
        for _ in 0..512 {
            block.push(ch.read_port(SECONDARY_CMD_BASE).unwrap());
        }
        // Word 0 low/high bytes: 0x85C0 little-endian.
        assert_eq!(block[0], 0xC0);
        assert_eq!(block[1], 0x85);
    }

    #[test]
    fn soft_reset_leaves_the_atapi_signature() {
        let mut ch = IdeChannel::new();
        ch.write_port(SECONDARY_CTRL, 0x04); // SRST
        ch.write_port(SECONDARY_CTRL, 0x00);
        assert_eq!((ch.lba_mid, ch.lba_high), (0x14, 0xEB));
    }

    #[test]
    fn slave_select_makes_commands_error() {
        let mut ch = IdeChannel::new();
        ch.write_port(SECONDARY_CMD_BASE + 6, 0x10); // select slave
        ch.write_port(SECONDARY_CMD_BASE + 7, 0xA0);
        assert_eq!(ch.status & status::ERR, status::ERR);
    }

    #[test]
    fn read10_charges_seek_and_transfer_time() {
        let mut ch = IdeChannel::new();
        ch.device_mut().insert(data_disc(8));
        // clear unit attention
        ch.write_port(SECONDARY_CMD_BASE + 7, 0xA0);
        for b in [0u8; 12] {
            ch.write_port(SECONDARY_CMD_BASE, b);
        }
        let _ = ch.take_stall_secs();
        let _ = packet_read10(&mut ch, 0);
        let secs = ch.take_stall_secs();
        assert!(secs > 0.0);
        assert_eq!(ch.take_access_bytes(), DATA_SECTOR);
    }

    #[test]
    fn nop_command_always_aborts() {
        let mut ch = IdeChannel::new();
        ch.write_port(SECONDARY_CMD_BASE + 7, 0x00); // NOP
        assert_eq!(ch.status & status::DRDY, status::DRDY);
        assert_eq!(ch.status & status::ERR, status::ERR);
        assert_eq!(ch.error, 0x04);
    }

    #[test]
    fn execute_diagnostic_passes_with_atapi_signature() {
        let mut ch = IdeChannel::new();
        ch.write_port(SECONDARY_CMD_BASE + 7, 0x90); // EXECUTE DEVICE DIAGNOSTIC
        // Device 0 passed and no error bit, so BIOS detection still sees it.
        assert_eq!(ch.error, 0x01);
        assert_eq!(ch.status & status::ERR, 0);
        assert_eq!(ch.status & status::DRDY, status::DRDY);
        // The ATAPI signature stays in the byte-count registers.
        assert_eq!((ch.sector_count, ch.lba_low), (0x01, 0x01));
        assert_eq!((ch.lba_mid, ch.lba_high), (0x14, 0xEB));
        // Completion raises the IRQ.
        assert!(ch.take_irq());
    }

    #[test]
    fn packet_read_chunks_to_the_byte_count_limit() {
        let mut ch = IdeChannel::new();
        ch.device_mut().insert(data_disc(8));
        // Clear the post-insert unit attention with a TEST UNIT READY.
        ch.write_port(SECONDARY_CMD_BASE + 7, 0xA0);
        for b in [0u8; 12] {
            ch.write_port(SECONDARY_CMD_BASE, b);
        }

        // Discard the TUR completion interrupt so the block IRQs below are the
        // only ones the assertions see.
        ch.take_irq();

        // Read two sectors so the data-in buffer is larger than one limit block.
        // The limit is deliberately NOT a divisor of the total, so the final block
        // is a short remainder (1500, 1500, 1096 over 4096) exercising the partial
        // block path, not just full-limit blocks.
        let sectors = 2usize;
        let total = sectors * DATA_SECTOR; // 4096
        let limit = 1500usize;

        // Program a byte-count limit smaller than the data before PACKET.
        ch.write_port(SECONDARY_CMD_BASE + 4, (limit & 0xFF) as u8); // cyl low
        ch.write_port(SECONDARY_CMD_BASE + 5, (limit >> 8) as u8); // cyl high
        ch.write_port(SECONDARY_CMD_BASE + 7, 0xA0); // PACKET
        let mut cdb = [0u8; 12];
        cdb[0] = 0x28; // READ(10)
        cdb[2..6].copy_from_slice(&0u32.to_be_bytes()); // lba 0
        cdb[7] = 0;
        cdb[8] = sectors as u8;
        for b in cdb {
            ch.write_port(SECONDARY_CMD_BASE, b);
        }

        // The first DRQ block arms an interrupt.
        assert!(ch.take_irq(), "the first data block raises IRQ15");

        // Drain block by block. Each block's byte count is the limit, except the
        // last, which is the remainder. Each new block re-raises the interrupt.
        let mut out = Vec::with_capacity(total);
        let mut drained = 0usize;
        while drained < total {
            assert_eq!(ch.status & status::DRQ, status::DRQ);
            let count = u16::from_le_bytes([ch.lba_mid, ch.lba_high]) as usize;
            let expected = (total - drained).min(limit);
            assert_eq!(count, expected);
            for _ in 0..count {
                out.push(ch.read_port(SECONDARY_CMD_BASE).unwrap());
            }
            drained += count;
            if drained < total {
                assert!(ch.take_irq(), "each new data block re-raises IRQ15");
            }
        }

        // After the last block, DRQ drops and the channel is idle/ready.
        assert_eq!(ch.status & status::DRQ, 0);
        assert_eq!(ch.status & status::DRDY, status::DRDY);
        // The reassembled data matches the two sectors read from lba 0.
        assert_eq!(out.len(), total);
        assert_eq!(out[0], 0x50); // sector 0 marker
        assert_eq!(out[DATA_SECTOR], 0x51); // sector 1 marker
    }
}
