// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! NEC uPD765A / Intel 8272A floppy disk controller.
//!
//! The controller owns its command, execution, and result phases. Mechanical
//! work is scheduled in fixed master-clock ticks. The machine supplies one DMA
//! byte when the controller reaches a data deadline, so channel 2 progresses one
//! 8237 cycle at a time instead of copying a whole sector in an I/O-port write.

use izarravm_core::MASTER_CLOCK_HZ;

use crate::floppy::Geometry;

/// Main status register (0x3F4) bit positions.
mod msr {
    pub const RQM: u8 = 0x80;
    pub const DIO: u8 = 0x40;
    pub const NDM: u8 = 0x20;
    pub const CB: u8 = 0x10;
}

/// Status register 0 (ST0) bit fields.
mod st0 {
    pub const IC_NORMAL: u8 = 0x00;
    pub const IC_ABNORMAL: u8 = 0x40;
    pub const IC_INVALID: u8 = 0x80;
    pub const SE: u8 = 0x20;
}

/// Status register 3 (ST3) bit fields.
mod st3 {
    pub const TWO_SIDED: u8 = 0x08;
    pub const TRACK0: u8 = 0x10;
    pub const READY: u8 = 0x20;
}

const MILLIS_TICKS: u64 = MASTER_CLOCK_HZ / 1_000;
const MOTOR_SPIN_UP_TICKS: u64 = MASTER_CLOCK_HZ / 2;
const MOTOR_SPIN_DOWN_TICKS: u64 = MASTER_CLOCK_HZ * 2;
const REVOLUTION_TICKS: u64 = MASTER_CLOCK_HZ / 5;
const NO_READY_TIMEOUT_TICKS: u64 = MASTER_CLOCK_HZ;
const FLOPPY_SECTOR_BYTES: u16 = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Command,
    Parameters,
    Execution,
    Result,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MotorPhase {
    Stopped,
    Starting,
    Running,
    Stopping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Motor {
    phase: MotorPhase,
    deadline: Option<u64>,
    rotation_epoch: u64,
}

impl Default for Motor {
    fn default() -> Self {
        Self {
            phase: MotorPhase::Stopped,
            deadline: None,
            rotation_epoch: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MediaTiming {
    cylinders: u16,
    heads: u8,
    sectors: u8,
    bytes_per_second: u64,
}

impl From<Geometry> for MediaTiming {
    fn from(value: Geometry) -> Self {
        Self {
            cylinders: value.cylinders,
            heads: value.heads,
            sectors: value.sectors,
            bytes_per_second: if value.drive_type == 0x04 {
                62_500
            } else {
                31_250
            },
        }
    }
}

/// A READ DATA or WRITE DATA command decoded into its guest-visible address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TransferRequest {
    pub read: bool,
    pub drive: u8,
    pub cylinder: u8,
    pub head: u8,
    pub sector: u8,
    pub end_sector: u8,
    pub bytes_per_sec: u16,
}

/// One byte that has reached the FDC data separator and now needs channel 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DmaByteRequest {
    pub transfer: TransferRequest,
    pub sector: u8,
    pub offset: u16,
}

/// Result of servicing one FDC DMA request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DmaByteOutcome {
    pub transferred: bool,
    pub terminal_count: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TransferState {
    request: TransferRequest,
    media: Option<MediaTiming>,
    sector: u8,
    offset: u16,
    last_sector: u8,
    moved_any: bool,
    deadline: Option<u64>,
    awaiting_dma: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimedOperation {
    Seek {
        deadline: u64,
        drive: u8,
        head: u8,
        cylinder: u8,
    },
    ReadId {
        deadline: u64,
        drive: u8,
        head: u8,
        sector: u8,
        valid: bool,
    },
    Transfer(TransferState),
}

impl TimedOperation {
    fn deadline(self) -> Option<u64> {
        match self {
            Self::Seek { deadline, .. } | Self::ReadId { deadline, .. } => Some(deadline),
            Self::Transfer(state) => state.deadline,
        }
    }
}

/// The 8272A register file, motors, and timed command engine.
#[derive(Debug, Clone)]
pub(crate) struct Fdc {
    dor: u8,
    phase: Phase,
    command: Vec<u8>,
    needed_params: usize,
    result: Vec<u8>,
    present_cyl: [u8; 4],
    seek_busy: u8,
    irq_edge_pending: bool,
    seek_interrupt: bool,
    st0: u8,
    media: Option<MediaTiming>,
    disk_changed: bool,
    motors: [Motor; 4],
    operation: Option<TimedOperation>,
    now_ticks: u64,
    step_rate_ticks: u64,
    head_load_ticks: u64,
    non_dma: bool,
}

impl Default for Fdc {
    fn default() -> Self {
        Self {
            dor: 0,
            phase: Phase::Command,
            command: Vec::new(),
            needed_params: 0,
            result: Vec::new(),
            present_cyl: [0; 4],
            seek_busy: 0,
            irq_edge_pending: false,
            seek_interrupt: false,
            st0: 0,
            media: None,
            disk_changed: false,
            motors: [Motor::default(); 4],
            operation: None,
            now_ticks: 0,
            step_rate_ticks: 3 * MILLIS_TICKS,
            head_load_ticks: 4 * MILLIS_TICKS,
            non_dma: false,
        }
    }
}

impl Fdc {
    pub(crate) fn owns_port(port: u16) -> bool {
        matches!(port, 0x3F0 | 0x3F1 | 0x3F2 | 0x3F4 | 0x3F5 | 0x3F7)
    }

    pub(crate) fn set_media_geometry(&mut self, geometry: Option<Geometry>) {
        let present = geometry.is_some();
        if present != self.media.is_some() {
            self.disk_changed = true;
        }
        self.media = geometry.map(MediaTiming::from);
    }

    fn dma_irq_enabled(&self) -> bool {
        self.dor & 0x08 != 0
    }

    fn motor_enabled(&self, drive: u8) -> bool {
        self.dor & (0x10 << (drive & 0x03)) != 0
    }

    fn drive_ready(&self, drive: u8) -> bool {
        self.media.is_some()
            && self.motor_enabled(drive)
            && matches!(self.motors[drive as usize].phase, MotorPhase::Running)
    }

    pub(crate) fn read_port(&mut self, port: u16) -> Option<u8> {
        match port {
            0x3F2 => Some(self.dor),
            0x3F4 => Some(self.main_status()),
            0x3F5 => Some(self.read_data()),
            0x3F7 => Some(if self.disk_changed { 0x80 } else { 0x00 }),
            _ => None,
        }
    }

    pub(crate) fn write_port_at(&mut self, port: u16, value: u8, now_ticks: u64) -> bool {
        self.now_ticks = self.now_ticks.max(now_ticks);
        match port {
            0x3F2 => {
                self.write_dor(value);
                true
            }
            0x3F5 => {
                self.write_data(value);
                true
            }
            0x3F7 => true,
            _ => false,
        }
    }

    fn main_status(&self) -> u8 {
        let mut status = self.seek_busy & 0x0F;
        match self.phase {
            Phase::Command => status |= msr::RQM,
            Phase::Parameters => status |= msr::RQM | msr::CB,
            Phase::Execution => {
                status |= msr::CB;
                if self.non_dma {
                    status |= msr::NDM;
                }
            }
            Phase::Result => status |= msr::RQM | msr::DIO | msr::CB,
        }
        status
    }

    fn write_dor(&mut self, value: u8) {
        let old = self.dor;
        let leaving_reset = old & 0x04 == 0 && value & 0x04 != 0;
        let entering_reset = old & 0x04 != 0 && value & 0x04 == 0;
        self.dor = value;

        for drive in 0..4u8 {
            let bit = 0x10 << drive;
            match (old & bit != 0, value & bit != 0) {
                (false, true) => self.start_motor(drive),
                (true, false) => self.stop_motor(drive),
                _ => {}
            }
        }

        if entering_reset {
            self.enter_reset();
        }
        if leaving_reset {
            self.irq_edge_pending = true;
            self.seek_interrupt = true;
            self.st0 = 0xC0;
        }
    }

    fn start_motor(&mut self, drive: u8) {
        let motor = &mut self.motors[drive as usize];
        match motor.phase {
            MotorPhase::Stopped => {
                motor.phase = MotorPhase::Starting;
                motor.deadline = Some(self.now_ticks.saturating_add(MOTOR_SPIN_UP_TICKS));
            }
            MotorPhase::Stopping => {
                motor.phase = MotorPhase::Running;
                motor.deadline = None;
            }
            MotorPhase::Starting | MotorPhase::Running => {}
        }
    }

    fn stop_motor(&mut self, drive: u8) {
        let motor = &mut self.motors[drive as usize];
        if motor.phase != MotorPhase::Stopped {
            motor.phase = MotorPhase::Stopping;
            motor.deadline = Some(self.now_ticks.saturating_add(MOTOR_SPIN_DOWN_TICKS));
        }
    }

    fn enter_reset(&mut self) {
        self.phase = Phase::Command;
        self.command.clear();
        self.result.clear();
        self.needed_params = 0;
        self.operation = None;
        self.seek_busy = 0;
        self.irq_edge_pending = false;
        self.seek_interrupt = false;
    }

    fn read_data(&mut self) -> u8 {
        if self.phase != Phase::Result {
            return 0;
        }
        if !self.seek_interrupt {
            self.irq_edge_pending = false;
        }
        let byte = self.result.pop().unwrap_or(0);
        if self.result.is_empty() {
            self.phase = Phase::Command;
        }
        byte
    }

    fn write_data(&mut self, value: u8) {
        match self.phase {
            Phase::Command => self.begin_command(value),
            Phase::Parameters => {
                self.command.push(value);
                if self.command.len() > self.needed_params {
                    self.execute_command();
                }
            }
            Phase::Execution | Phase::Result => {}
        }
    }

    fn begin_command(&mut self, opcode: u8) {
        self.command.clear();
        self.result.clear();
        self.command.push(opcode);
        let params = match opcode & 0x1F {
            0x03 => 2,
            0x04 => 1,
            0x05 | 0x06 => 8,
            0x07 => 1,
            0x08 | 0x10 => 0,
            0x0A => 1,
            0x0F => 2,
            0x13 => 3,
            _ => {
                self.finish_with_result(vec![st0::IC_INVALID]);
                return;
            }
        };
        self.needed_params = params;
        if params == 0 {
            self.execute_command();
        } else {
            self.phase = Phase::Parameters;
        }
    }

    fn drive_head(&self) -> (u8, u8) {
        let parameter = self.command.get(1).copied().unwrap_or(0);
        (parameter & 0x03, (parameter >> 2) & 0x01)
    }

    fn make_st0(&self, interrupt_code: u8, seek_end: bool, drive: u8, head: u8) -> u8 {
        let mut value = interrupt_code | (drive & 0x03) | ((head & 0x01) << 2);
        if seek_end {
            value |= st0::SE;
        }
        value
    }

    fn execute_command(&mut self) {
        match self.command[0] & 0x1F {
            0x03 => self.cmd_specify(),
            0x04 => self.cmd_sense_drive_status(),
            0x05 => self.cmd_read_write(false),
            0x06 => self.cmd_read_write(true),
            0x07 => self.cmd_recalibrate(),
            0x08 => self.cmd_sense_interrupt(),
            0x0A => self.cmd_read_id(),
            0x0F => self.cmd_seek(),
            0x10 => self.finish_with_result(vec![0x90]),
            0x13 => self.finish_with_result(vec![]),
            _ => self.finish_with_result(vec![st0::IC_INVALID]),
        }
    }

    fn finish_with_result(&mut self, mut bytes: Vec<u8>) {
        self.command.clear();
        self.needed_params = 0;
        if bytes.is_empty() {
            self.result.clear();
            self.phase = Phase::Command;
        } else {
            bytes.reverse();
            self.result = bytes;
            self.phase = Phase::Result;
        }
    }

    fn cmd_specify(&mut self) {
        let timing = self.command.get(1).copied().unwrap_or(0);
        let options = self.command.get(2).copied().unwrap_or(0);
        let step_ms = u64::from(16 - (timing >> 4)).max(1);
        let head_load_ms = u64::from(options >> 1) * 2;
        self.step_rate_ticks = step_ms * MILLIS_TICKS;
        self.head_load_ticks = head_load_ms * MILLIS_TICKS;
        self.non_dma = options & 1 != 0;
        self.finish_with_result(vec![]);
    }

    fn cmd_sense_drive_status(&mut self) {
        let (drive, head) = self.drive_head();
        let mut value = st3::TWO_SIDED | drive | (head << 2);
        if self.present_cyl[drive as usize] == 0 {
            value |= st3::TRACK0;
        }
        if self.drive_ready(drive) {
            value |= st3::READY;
        }
        self.finish_with_result(vec![value]);
    }

    fn cmd_recalibrate(&mut self) {
        let drive = self.command.get(1).copied().unwrap_or(0) & 0x03;
        self.schedule_seek(drive, 0, 0);
    }

    fn cmd_seek(&mut self) {
        let (drive, head) = self.drive_head();
        let cylinder = self.command.get(2).copied().unwrap_or(0);
        self.schedule_seek(drive, head, cylinder);
    }

    fn schedule_seek(&mut self, drive: u8, head: u8, cylinder: u8) {
        let current = self.present_cyl[drive as usize];
        let tracks = current.abs_diff(cylinder).max(1);
        let delay = u64::from(tracks).saturating_mul(self.step_rate_ticks);
        self.seek_busy |= 1 << drive;
        self.operation = Some(TimedOperation::Seek {
            deadline: self.deadline_after(delay),
            drive,
            head,
            cylinder,
        });
        self.finish_with_result(vec![]);
    }

    fn cmd_sense_interrupt(&mut self) {
        if self.seek_interrupt {
            let drive = (self.st0 & 0x03) as usize;
            let cylinder = self.present_cyl[drive];
            self.seek_interrupt = false;
            self.irq_edge_pending = false;
            self.finish_with_result(vec![self.st0, cylinder]);
        } else {
            self.finish_with_result(vec![st0::IC_INVALID]);
        }
    }

    fn cmd_read_id(&mut self) {
        let (drive, head) = self.drive_head();
        let Some((deadline, sector)) = self.next_sector_deadline(drive, 1) else {
            self.operation = Some(TimedOperation::ReadId {
                deadline: self.deadline_after(NO_READY_TIMEOUT_TICKS),
                drive,
                head,
                sector: 1,
                valid: false,
            });
            self.phase = Phase::Execution;
            self.command.clear();
            return;
        };
        self.operation = Some(TimedOperation::ReadId {
            deadline,
            drive,
            head,
            sector,
            valid: true,
        });
        self.phase = Phase::Execution;
        self.command.clear();
    }

    fn cmd_read_write(&mut self, read: bool) {
        let parameters = &self.command[1..];
        let drive = parameters[0] & 0x03;
        let head = (parameters[0] >> 2) & 0x01;
        let cylinder = parameters[1];
        let sector = parameters[3];
        let n = parameters[4].min(7);
        let request = TransferRequest {
            read,
            drive,
            cylinder,
            head,
            sector,
            end_sector: parameters[5],
            bytes_per_sec: 128u16 << n,
        };

        let media = self.media;
        let deadline = self
            .first_data_deadline(request, media)
            .unwrap_or_else(|| self.deadline_after(NO_READY_TIMEOUT_TICKS));
        self.operation = Some(TimedOperation::Transfer(TransferState {
            request,
            media,
            sector,
            offset: 0,
            last_sector: sector,
            moved_any: false,
            deadline: Some(deadline),
            awaiting_dma: false,
        }));
        self.phase = Phase::Execution;
        self.command.clear();
        self.needed_params = 0;
    }

    fn first_data_deadline(
        &self,
        request: TransferRequest,
        media: Option<MediaTiming>,
    ) -> Option<u64> {
        let media = media?;
        if request.bytes_per_sec != FLOPPY_SECTOR_BYTES
            || request.cylinder as u16 >= media.cylinders
            || request.head >= media.heads
            || request.sector == 0
            || request.sector > media.sectors
            || !self.motor_enabled(request.drive)
        {
            return None;
        }

        let motor_ready = self.motor_ready_tick(request.drive)?;
        let tracks = self.present_cyl[request.drive as usize].abs_diff(request.cylinder);
        let head_ready = self.now_ticks.saturating_add(
            u64::from(tracks)
                .saturating_mul(self.step_rate_ticks)
                .saturating_add(self.head_load_ticks),
        );
        let ready = motor_ready.max(head_ready);
        self.rotation_wait(request.drive, request.sector, media.sectors, ready)
            .map(|wait| ready.saturating_add(wait.max(1)))
    }

    fn motor_ready_tick(&self, drive: u8) -> Option<u64> {
        let motor = self.motors[drive as usize];
        match motor.phase {
            MotorPhase::Running => Some(self.now_ticks),
            MotorPhase::Starting => motor.deadline,
            MotorPhase::Stopped | MotorPhase::Stopping => None,
        }
    }

    fn next_sector_deadline(&self, drive: u8, minimum_sector: u8) -> Option<(u64, u8)> {
        let media = self.media?;
        if !self.motor_enabled(drive) {
            return None;
        }
        let ready = self
            .motor_ready_tick(drive)?
            .max(self.now_ticks.saturating_add(self.head_load_ticks));
        let phase = self.rotation_phase_at(drive, ready)?;
        for sector in minimum_sector.max(1)..=media.sectors {
            let target = sector_phase(sector, media.sectors);
            if target >= phase {
                return Some((ready.saturating_add((target - phase).max(1)), sector));
            }
        }
        let target = sector_phase(1, media.sectors);
        Some((
            ready.saturating_add((REVOLUTION_TICKS - phase + target).max(1)),
            1,
        ))
    }

    fn rotation_wait(&self, drive: u8, sector: u8, sectors_per_track: u8, at: u64) -> Option<u64> {
        let phase = self.rotation_phase_at(drive, at)?;
        let target = sector_phase(sector, sectors_per_track);
        Some(if target >= phase {
            target - phase
        } else {
            REVOLUTION_TICKS - phase + target
        })
    }

    fn rotation_phase_at(&self, drive: u8, at: u64) -> Option<u64> {
        let motor = self.motors[drive as usize];
        let epoch = match motor.phase {
            MotorPhase::Running | MotorPhase::Stopping => motor.rotation_epoch,
            MotorPhase::Starting => motor.deadline?,
            MotorPhase::Stopped => return None,
        };
        Some(at.saturating_sub(epoch) % REVOLUTION_TICKS)
    }

    fn deadline_after(&self, delta: u64) -> u64 {
        self.now_ticks.saturating_add(delta.max(1))
    }

    fn next_deadline(&self) -> Option<u64> {
        self.operation
            .and_then(TimedOperation::deadline)
            .into_iter()
            .chain(self.motors.iter().filter_map(|motor| motor.deadline))
            .min()
    }

    pub(crate) fn ticks_until_event(&self, machine_now: u64) -> Option<u64> {
        self.next_deadline()
            .map(|deadline| deadline.saturating_sub(machine_now).max(1))
    }

    /// Advance to an absolute master tick. Internal motor and command events are
    /// consumed in deadline order. A DMA byte is returned to the machine and the
    /// controller pauses at that exact tick until `complete_dma_byte` is called.
    pub(crate) fn advance_to(&mut self, target_ticks: u64) -> Option<DmaByteRequest> {
        if target_ticks < self.now_ticks {
            return None;
        }
        loop {
            let Some(deadline) = self.next_deadline() else {
                self.now_ticks = target_ticks;
                return None;
            };
            if deadline > target_ticks {
                self.now_ticks = target_ticks;
                return None;
            }
            self.now_ticks = deadline;
            self.advance_motors_at_deadline(deadline);

            let due = self
                .operation
                .is_some_and(|operation| operation.deadline() == Some(deadline));
            if !due {
                continue;
            }
            let operation = self.operation.take().expect("due FDC operation");
            match operation {
                TimedOperation::Seek {
                    drive,
                    head,
                    cylinder,
                    ..
                } => self.complete_seek(drive, head, cylinder),
                TimedOperation::ReadId {
                    drive,
                    head,
                    sector,
                    valid,
                    ..
                } => self.complete_read_id(drive, head, sector, valid),
                TimedOperation::Transfer(mut state) => {
                    let valid = state.media.is_some_and(|media| {
                        self.media.is_some()
                            && self.drive_ready(state.request.drive)
                            && self.dma_irq_enabled()
                            && state.request.bytes_per_sec == FLOPPY_SECTOR_BYTES
                            && (state.request.cylinder as u16) < media.cylinders
                            && state.request.head < media.heads
                            && state.sector != 0
                            && state.sector <= media.sectors
                    });
                    if !valid {
                        self.finish_transfer(state, false);
                        continue;
                    }
                    self.present_cyl[state.request.drive as usize] = state.request.cylinder;
                    self.disk_changed = false;
                    state.deadline = None;
                    state.awaiting_dma = true;
                    let request = DmaByteRequest {
                        transfer: state.request,
                        sector: state.sector,
                        offset: state.offset,
                    };
                    self.operation = Some(TimedOperation::Transfer(state));
                    return Some(request);
                }
            }
        }
    }

    fn advance_motors_at_deadline(&mut self, deadline: u64) {
        for motor in &mut self.motors {
            if motor.deadline != Some(deadline) {
                continue;
            }
            match motor.phase {
                MotorPhase::Starting => {
                    motor.phase = MotorPhase::Running;
                    motor.deadline = None;
                    motor.rotation_epoch = deadline;
                }
                MotorPhase::Stopping => {
                    motor.phase = MotorPhase::Stopped;
                    motor.deadline = None;
                    motor.rotation_epoch = deadline;
                }
                MotorPhase::Stopped | MotorPhase::Running => motor.deadline = None,
            }
        }
    }

    fn complete_seek(&mut self, drive: u8, head: u8, cylinder: u8) {
        self.present_cyl[drive as usize] = cylinder;
        self.disk_changed = false;
        self.seek_busy &= !(1 << drive);
        self.st0 = self.make_st0(st0::IC_NORMAL, true, drive, head);
        self.seek_interrupt = true;
        self.irq_edge_pending = true;
    }

    fn complete_read_id(&mut self, drive: u8, head: u8, sector: u8, valid: bool) {
        let cylinder = self.present_cyl[drive as usize];
        let interrupt_code = if valid && self.drive_ready(drive) {
            st0::IC_NORMAL
        } else {
            st0::IC_ABNORMAL
        };
        let st1 = if interrupt_code == st0::IC_NORMAL {
            0
        } else {
            0x04
        };
        let status = self.make_st0(interrupt_code, false, drive, head);
        self.st0 = status;
        self.seek_interrupt = false;
        self.irq_edge_pending = true;
        self.finish_with_result(vec![status, st1, 0, cylinder, head, sector, 2]);
    }

    pub(crate) fn complete_dma_byte(&mut self, outcome: DmaByteOutcome) {
        let Some(TimedOperation::Transfer(mut state)) = self.operation.take() else {
            return;
        };
        if !state.awaiting_dma {
            self.operation = Some(TimedOperation::Transfer(state));
            return;
        }
        state.awaiting_dma = false;
        if !outcome.transferred {
            self.finish_transfer(state, false);
            return;
        }

        state.moved_any = true;
        state.last_sector = state.sector;
        if outcome.terminal_count {
            self.finish_transfer(state, true);
            return;
        }

        state.offset += 1;
        if state.offset < state.request.bytes_per_sec {
            let byte_ticks = MASTER_CLOCK_HZ / state.media.unwrap().bytes_per_second;
            state.deadline = Some(self.deadline_after(byte_ticks));
            self.operation = Some(TimedOperation::Transfer(state));
            return;
        }

        if state.sector >= state.request.end_sector {
            self.finish_transfer(state, true);
            return;
        }
        state.sector = state.sector.saturating_add(1);
        state.offset = 0;
        let Some(media) = state.media else {
            self.finish_transfer(state, false);
            return;
        };
        if state.sector > media.sectors {
            self.finish_transfer(state, false);
            return;
        }
        let wait = self
            .rotation_wait(
                state.request.drive,
                state.sector,
                media.sectors,
                self.now_ticks,
            )
            .unwrap_or(NO_READY_TIMEOUT_TICKS);
        state.deadline = Some(self.deadline_after(wait));
        self.operation = Some(TimedOperation::Transfer(state));
    }

    fn finish_transfer(&mut self, state: TransferState, success: bool) {
        let success = success && state.moved_any;
        let interrupt_code = if success {
            st0::IC_NORMAL
        } else {
            st0::IC_ABNORMAL
        };
        let status = self.make_st0(
            interrupt_code,
            false,
            state.request.drive,
            state.request.head,
        );
        let st1 = if success { 0 } else { 0x04 };
        let n = match state.request.bytes_per_sec {
            128 => 0,
            256 => 1,
            512 => 2,
            1024 => 3,
            _ => 2,
        };
        self.st0 = status;
        self.seek_interrupt = false;
        self.irq_edge_pending = true;
        self.finish_with_result(vec![
            status,
            st1,
            0,
            state.request.cylinder,
            state.request.head,
            state.last_sector,
            n,
        ]);
    }

    pub(crate) fn take_irq(&mut self) -> bool {
        if self.irq_edge_pending && self.dma_irq_enabled() {
            self.irq_edge_pending = false;
            true
        } else {
            false
        }
    }
}

fn sector_phase(sector: u8, sectors_per_track: u8) -> u64 {
    u64::from(sector.saturating_sub(1)) * REVOLUTION_TICKS / u64::from(sectors_per_track.max(1))
}

#[cfg(test)]
#[path = "fdc_test.rs"]
mod tests;
