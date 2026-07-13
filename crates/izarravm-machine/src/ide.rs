// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! IDE/ATA register file for the secondary channel, hosting the ATAPI CD-ROM as
//! the secondary master.
//!
//! Channel choice: the CD-ROM lives on the **secondary** IDE channel, the
//! conventional home for an optical drive, at command block 0x170-0x177, control
//! block 0x376, IRQ15. The primary channel (0x1F0/0x3F6, IRQ14) hosts the hard
//! disk. Only the secondary master is populated; selecting its slave reports a
//! command error.
//!
//! ATAPI handshake modeled (SFF-8020i): the host issues the ATA PACKET command
//! (0xA0) to the command register, then writes the 12-byte command descriptor
//! block to the data register. Command acceptance, seeks, spin-up, and each PIO
//! sector become visible on the machine master timeline. IDENTIFY PACKET DEVICE
//! (0xA1) and the ATA soft-reset path are handled directly. DMA is not modeled:
//! transfers are PIO, which every ATAPI driver and IZCDEX supports.

use crate::atapi::{self, AtapiDevice, CmdResult};
use crate::cdimage::DATA_SECTOR;
use izarravm_core::MASTER_CLOCK_HZ;

/// Secondary-channel command-block base (0x170-0x177).
pub const SECONDARY_CMD_BASE: u16 = 0x170;
/// Secondary-channel control/alt-status port.
pub const SECONDARY_CTRL: u16 = 0x376;
/// The IRQ the secondary channel raises on command completion.
pub const SECONDARY_IRQ: u8 = 15;

const COMMAND_LATENCY_TICKS: u64 = MASTER_CLOCK_HZ / 10_000; // 100 us
const PACKET_ACCEPT_TICKS: u64 = MASTER_CLOCK_HZ / 20_000; // 50 us accelerated DRQ
const SPIN_UP_TICKS: u64 = MASTER_CLOCK_HZ / 5; // 200 ms
const MAX_SEEK_TICKS: u64 = MASTER_CLOCK_HZ / 10; // 100 ms
const CD_BYTES_PER_SECOND: u64 = 1_800 * 1024; // 12x CD-ROM

pub(crate) fn sector_transfer_ticks() -> u64 {
    (DATA_SECTOR as u128 * MASTER_CLOCK_HZ as u128).div_ceil(CD_BYTES_PER_SECOND as u128) as u64
}

/// ATA status register bits.
mod status {
    pub const ERR: u8 = 0x01; // error
    pub const DRQ: u8 = 0x08; // data request: a PIO transfer is ready
    pub const DSC: u8 = 0x10; // device seek complete / service
    pub const DRDY: u8 = 0x40; // device ready
    pub const BSY: u8 = 0x80; // command or media operation in progress
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
    /// Receiving a packet command's parameter list from the host.
    DataOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingAction {
    AcceptPacket,
    ExecutePacket([u8; 12]),
    CompleteDataOut,
    PresentReadSector,
    CompleteSeek { lba: u32 },
    PrepareIdentify,
    DeviceReset,
    IdentifyNak,
    Diagnostic,
    Abort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingCommand {
    ticks_remaining: u64,
    action: PendingAction,
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
    /// End offset of the media bytes whose transfer deadline has elapsed. Read
    /// commands grow this one sector at a time; short control replies expose the
    /// whole buffer at their command deadline.
    data_in_ready_end: usize,
    /// The active packet command's host-to-device parameter list.
    data_out: Vec<u8>,
    data_out_expected: usize,
    data_out_block_end: usize,
    /// Per-command host byte-count limit (cylinder low/high at PACKET time). Zero
    /// means no limit was programmed, so the whole buffer goes out in one block.
    byte_count_limit: usize,
    /// Set when a command completes so the machine forwards IRQ15 to the PIC.
    irq_pending: bool,
    pending_command: Option<PendingCommand>,
    /// Current optical head location and the first LBA of the active read.
    head_lba: u32,
    read_lba: u32,
    /// Bytes moved by the last data command, for the access LED.
    last_access_bytes: usize,
    /// Test seam that leaves PACKET commands unanswered so guest timeout paths
    /// can run against a present drive.
    test_stall_packet: bool,
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
            data_in_ready_end: 0,
            data_out: Vec::new(),
            data_out_expected: 0,
            data_out_block_end: 0,
            byte_count_limit: 0,
            irq_pending: false,
            pending_command: None,
            head_lba: 0,
            read_lba: 0,
            last_access_bytes: 0,
            test_stall_packet: false,
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

    pub(crate) fn set_test_stall_packet(&mut self, enabled: bool) {
        self.test_stall_packet = enabled;
    }

    #[cfg(test)]
    pub(crate) fn transport_state_snapshot(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}:{}:{}:{}:{:?}:{}:{:?}:{}:{}:{}:{:?}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{:?}:{}",
            self.features,
            self.sector_count,
            self.lba_low,
            self.lba_mid,
            self.lba_high,
            self.drive_select,
            self.status,
            self.error,
            self.phase,
            self.interrupts_disabled,
            self.packet,
            self.packet_filled,
            self.data_in_pos,
            self.data_in_block_end,
            self.data_in,
            self.data_in_ready_end,
            self.data_out_expected,
            self.data_out_block_end,
            self.byte_count_limit,
            self.irq_pending,
            self.head_lba,
            self.read_lba,
            self.last_access_bytes,
            self.data_out.len(),
            self.pending_command,
            self.device.non_playback_state_snapshot()
        )
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

    pub(crate) fn irq_enabled(&self) -> bool {
        !self.interrupts_disabled
    }

    /// Master ticks until the next command or media boundary.
    pub(crate) fn ticks_until_completion(&self) -> Option<u64> {
        self.pending_command
            .as_ref()
            .map(|pending| pending.ticks_remaining)
    }

    /// Master ticks until progress can produce an interrupt. An execute boundary
    /// may schedule mechanical work instead, in which case a halted machine
    /// immediately asks again for the new deadline.
    pub(crate) fn ticks_until_irq(&self) -> Option<u64> {
        self.pending_command
            .as_ref()
            .filter(|pending| pending.action != PendingAction::AcceptPacket)
            .map(|pending| pending.ticks_remaining)
    }

    /// Advance pending work on the authoritative machine timeline. Consume
    /// surplus ticks when one internal boundary schedules another so a split or
    /// unsplit advance reaches the same state.
    pub(crate) fn advance_master_ticks(&mut self, ticks: u64) {
        let mut remaining = ticks;
        loop {
            let Some(pending) = self.pending_command.as_mut() else {
                return;
            };
            if remaining < pending.ticks_remaining {
                pending.ticks_remaining -= remaining;
                return;
            }
            remaining -= pending.ticks_remaining;
            let action = self.pending_command.take().unwrap().action;
            self.finish_pending(action);
            if remaining == 0 {
                return;
            }
        }
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
        if self.status & status::BSY != 0 && reg != 0 {
            return true;
        }
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
        self.data_in_ready_end = 0;
        self.data_out.clear();
        self.data_out_expected = 0;
        self.data_out_block_end = 0;
        self.byte_count_limit = 0;
        self.pending_command = None;
        self.irq_pending = false;
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
        if self.pending_command.is_some() || self.phase != Phase::Idle {
            return;
        }
        if !self.master_selected() {
            self.schedule(PendingAction::Abort, COMMAND_LATENCY_TICKS);
            return;
        }
        match command {
            0xA0 if self.test_stall_packet => {}
            0xA0 => self.schedule(PendingAction::AcceptPacket, PACKET_ACCEPT_TICKS),
            0xA1 => self.schedule(PendingAction::PrepareIdentify, COMMAND_LATENCY_TICKS),
            0x08 => self.schedule(PendingAction::DeviceReset, COMMAND_LATENCY_TICKS),
            0xEC => self.schedule(PendingAction::IdentifyNak, COMMAND_LATENCY_TICKS),
            0x90 => self.schedule(PendingAction::Diagnostic, COMMAND_LATENCY_TICKS),
            // NOP and unsupported commands abort after command latency.
            _ => self.schedule(PendingAction::Abort, COMMAND_LATENCY_TICKS),
        }
    }

    fn schedule(&mut self, action: PendingAction, ticks: u64) {
        self.phase = Phase::Idle;
        self.status = status::BSY;
        self.error = 0;
        self.irq_pending = false;
        self.pending_command = Some(PendingCommand {
            ticks_remaining: ticks.max(1),
            action,
        });
    }

    fn finish_pending(&mut self, action: PendingAction) {
        match action {
            PendingAction::AcceptPacket => self.begin_packet(),
            PendingAction::ExecutePacket(cdb) => self.execute_packet(cdb),
            PendingAction::CompleteDataOut => self.complete_data_out(),
            PendingAction::PresentReadSector => self.present_read_sector(),
            PendingAction::CompleteSeek { lba } => {
                self.head_lba = lba;
                self.complete_packet();
            }
            PendingAction::PrepareIdentify => self.identify_packet_device(),
            PendingAction::DeviceReset => {
                self.soft_reset();
                self.raise_irq();
            }
            PendingAction::IdentifyNak => self.identify_nak(),
            PendingAction::Diagnostic => self.execute_diagnostic(),
            PendingAction::Abort => self.abort(),
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
        // before issuing PACKET. Capture it now so the data path can chunk a large
        // data-in transfer into DRQ blocks no bigger than the limit.
        self.byte_count_limit = u16::from_le_bytes([self.lba_mid, self.lba_high]) as usize;
        // Arm the CDB phase: the device sets C/D=1, I/O=0 (command, from host).
        // Publish it on the sector-count (interrupt-reason) port so a host that
        // polls the reason before feeding the CDB sees the command phase.
        self.device.arm_packet();
        self.publish_interrupt_reason();
        self.status = status::DRDY | status::DRQ;
        self.error = 0;
    }

    /// IDENTIFY PACKET DEVICE (0xA1): present the 512-byte identify block.
    fn identify_packet_device(&mut self) {
        let block = identify_block();
        self.data_in = block;
        self.data_in_pos = 0;
        self.data_in_ready_end = self.data_in.len();
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
        self.raise_irq();
    }

    fn abort(&mut self) {
        self.phase = Phase::Idle;
        self.data_in.clear();
        self.data_in_pos = 0;
        self.data_in_block_end = 0;
        self.data_in_ready_end = 0;
        self.data_out.clear();
        self.data_out_expected = 0;
        self.data_out_block_end = 0;
        self.status = status::DRDY | status::ERR;
        self.error = 0x04;
        self.raise_irq();
    }

    fn read_data_byte(&mut self) -> u8 {
        if self.phase != Phase::DataIn {
            return 0;
        }
        let byte = self.data_in.get(self.data_in_pos).copied().unwrap_or(0);
        self.data_in_pos += 1;
        if self.data_in_pos >= self.data_in.len() {
            // Whole transfer complete: drop DRQ, go idle. The data phase is over,
            // so the interrupt reason advances to command-complete (C/D=1, I/O=1).
            self.phase = Phase::Idle;
            self.data_in.clear();
            self.data_in_pos = 0;
            self.data_in_block_end = 0;
            self.data_in_ready_end = 0;
            self.sector_count = atapi::interrupt_reason::COMMAND_COMPLETE;
            self.status = status::DRDY | status::DSC;
            self.raise_irq();
        } else if self.data_in_pos >= self.data_in_ready_end {
            // The ready sector drained. Keep later sectors busy until their own
            // transfer deadlines instead of exposing the whole HLE buffer.
            self.schedule(PendingAction::PresentReadSector, sector_transfer_ticks());
        } else if self.data_in_pos >= self.data_in_block_end {
            // The host byte-count block drained inside the ready sector.
            self.present_data_block();
            self.raise_irq();
        }
        byte
    }

    fn write_data_byte(&mut self, value: u8) {
        match self.phase {
            Phase::AwaitPacket => {
                if self.packet_filled < self.packet.len() {
                    self.packet[self.packet_filled] = value;
                    self.packet_filled += 1;
                }
                if self.packet_filled == self.packet.len() {
                    let cdb = self.packet;
                    self.schedule(PendingAction::ExecutePacket(cdb), COMMAND_LATENCY_TICKS);
                }
            }
            Phase::DataOut => {
                self.data_out.push(value);
                if self.data_out.len() >= self.data_out_expected {
                    self.schedule(PendingAction::CompleteDataOut, COMMAND_LATENCY_TICKS);
                } else if self.data_out.len() >= self.data_out_block_end {
                    self.present_data_out_block();
                    self.raise_irq();
                }
            }
            _ => {}
        }
    }

    /// Execute an assembled CDB after command latency. Short replies become
    /// visible now; reads and seeks schedule their mechanical boundary.
    fn execute_packet(&mut self, cdb: [u8; 12]) {
        if let Some(length) = data_out_length(&cdb).filter(|&length| length > 0) {
            self.begin_data_out(length);
            return;
        }
        match self.device.execute(&cdb) {
            CmdResult::Data(buf) => {
                if buf.is_empty() {
                    if cdb[0] == 0x2B {
                        let lba = packet_lba(&cdb);
                        let delay = self.media_delay(lba);
                        if delay > 0 {
                            self.schedule(PendingAction::CompleteSeek { lba }, delay);
                        } else {
                            self.head_lba = lba;
                            self.complete_packet();
                        }
                    } else {
                        self.complete_packet();
                    }
                } else {
                    self.data_in = buf;
                    self.data_in_pos = 0;
                    self.data_in_ready_end = 0;
                    self.error = 0;
                    self.publish_interrupt_reason();
                    if is_read_opcode(cdb[0]) {
                        self.read_lba = packet_lba(&cdb);
                        self.last_access_bytes = 0;
                        let delay = self
                            .media_delay(self.read_lba)
                            .saturating_add(sector_transfer_ticks());
                        self.schedule(PendingAction::PresentReadSector, delay);
                    } else {
                        self.data_in_ready_end = self.data_in.len();
                        self.phase = Phase::DataIn;
                        self.present_data_block();
                        self.raise_irq();
                    }
                }
            }
            CmdResult::Error => {
                // The device left the reason on command-complete; the host reads
                // CHECK CONDITION from the status/error registers.
                self.phase = Phase::Idle;
                self.status = status::DRDY | status::ERR;
                self.error = 0x04; // ABRT / CHECK CONDITION (sense already latched)
                self.publish_interrupt_reason();
                self.raise_irq();
            }
        }
    }

    fn begin_data_out(&mut self, length: usize) {
        self.phase = Phase::DataOut;
        self.data_out.clear();
        self.data_out.reserve(length);
        self.data_out_expected = length;
        self.last_access_bytes = 0;
        self.device.arm_data_out();
        self.publish_interrupt_reason();
        self.error = 0;
        self.present_data_out_block();
        self.raise_irq();
    }

    fn complete_data_out(&mut self) {
        let result = self.device.mode_select_data(&self.data_out);
        self.last_access_bytes = self.last_access_bytes.saturating_add(self.data_out.len());
        self.data_out.clear();
        self.data_out_expected = 0;
        self.data_out_block_end = 0;
        match result {
            CmdResult::Data(_) => self.complete_packet(),
            CmdResult::Error => {
                self.phase = Phase::Idle;
                self.publish_interrupt_reason();
                self.status = status::DRDY | status::ERR;
                self.error = 0x04;
                self.raise_irq();
            }
        }
    }

    fn present_read_sector(&mut self) {
        let sector_start = self.data_in_ready_end;
        self.data_in_ready_end = self
            .data_in_ready_end
            .saturating_add(DATA_SECTOR)
            .min(self.data_in.len());
        self.head_lba = self
            .read_lba
            .saturating_add((sector_start / DATA_SECTOR) as u32);
        self.last_access_bytes = self
            .last_access_bytes
            .saturating_add(self.data_in_ready_end - sector_start);
        self.phase = Phase::DataIn;
        self.sector_count = atapi::interrupt_reason::DATA_IN;
        self.error = 0;
        self.present_data_block();
        self.raise_irq();
    }

    fn complete_packet(&mut self) {
        self.phase = Phase::Idle;
        self.sector_count = atapi::interrupt_reason::COMMAND_COMPLETE;
        self.status = status::DRDY | status::DSC;
        self.error = 0;
        self.raise_irq();
    }

    fn media_delay(&mut self, lba: u32) -> u64 {
        let spin = if self.device.ensure_started() {
            SPIN_UP_TICKS
        } else {
            0
        };
        let sectors = self
            .device
            .image()
            .map_or(1, crate::cdimage::CdImage::total_sectors)
            .max(1);
        let distance = self.head_lba.abs_diff(lba).min(sectors);
        let seek = (u128::from(distance) * u128::from(MAX_SEEK_TICKS) / u128::from(sectors)) as u64;
        spin.saturating_add(seek)
    }

    /// Arm the next data-in DRQ block at the current cursor: set the byte count
    /// to this block's size, raise DRQ, and raise the IRQ. The block is the
    /// remaining bytes capped by the host byte-count limit, or the whole
    /// remainder when no limit (or a limit at least as large) was programmed.
    fn present_data_block(&mut self) {
        let remaining = self.data_in_ready_end - self.data_in_pos;
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

    fn present_data_out_block(&mut self) {
        let remaining = self.data_out_expected - self.data_out.len();
        let block = if self.byte_count_limit > 0 && self.byte_count_limit < remaining {
            self.byte_count_limit
        } else {
            remaining
        };
        self.data_out_block_end = self.data_out.len() + block;
        self.lba_mid = (block & 0xFF) as u8;
        self.lba_high = ((block >> 8) & 0xFF) as u8;
        self.status = status::DRDY | status::DRQ | status::DSC;
    }

    /// Copy the device's current interrupt reason (C/D, I/O) onto the sector-count
    /// register, which is the ATAPI Interrupt Reason register the host polls.
    fn publish_interrupt_reason(&mut self) {
        self.sector_count = self.device.interrupt_reason();
    }

    fn raise_irq(&mut self) {
        self.irq_pending = true;
    }
}

fn is_read_opcode(opcode: u8) -> bool {
    matches!(opcode, 0x28 | 0xA8 | 0xBE)
}

fn data_out_length(cdb: &[u8; 12]) -> Option<usize> {
    (cdb[0] == 0x55).then(|| u16::from_be_bytes([cdb[7], cdb[8]]) as usize)
}

fn packet_lba(cdb: &[u8; 12]) -> u32 {
    u32::from_be_bytes([cdb[2], cdb[3], cdb[4], cdb[5]])
}

/// The 512-byte IDENTIFY PACKET DEVICE response. Word 0 marks an ATAPI removable
/// CD-ROM; the model number and firmware strings are byte-swapped ASCII per ATA.
fn identify_block() -> Vec<u8> {
    let mut words = [0u16; 256];
    // General config: bits 15-14 = 10b (ATAPI device), bits 12-8 = 0x05 (CD-ROM
    // command set), bit 7 = removable, bits 6-5 = 10b accelerated command DRQ,
    // and bits 1-0 = 0 (12-byte packet).
    words[0] = 0x85C0;
    // LBA is supported. DMA stays clear until the secondary ATAPI DMA path is
    // implemented; the BMIDE register bank alone is not a transfer engine.
    words[49] = 0x0200;
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
#[path = "ide_test.rs"]
mod tests;
