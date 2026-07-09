//! NEC uPD765A / Intel 8272A floppy disk controller, the chip a guest programs
//! directly through ports 0x3F2-0x3F7.
//!
//! Built clean-room from the uPD765A datasheet and the IBM PC/AT technical
//! reference. The floppy is INT 13h HLE for most guests, but a few program the
//! FDC straight: this models the register file, the three-phase command protocol
//! (command, execution, result), and the ST0-ST3 status registers, and drives
//! sector data over DMA channel 2.
//!
//! The chip here is pure state: it decodes commands and parameters, tracks the
//! per-drive head position and the seek/recalibrate interrupt, and produces a
//! transfer request the machine fulfils against the mounted image and the 8237A.
//! It does not own the floppy or the DMA controller; the machine bridges them so
//! READ/WRITE DATA push bytes through the real channel-2 datapath.

/// Main status register (0x3F4) bit positions.
mod msr {
    /// RQM: the data register is ready for a byte transfer with the CPU.
    pub const RQM: u8 = 0x80;
    /// DIO: data direction. Set means FDC->CPU (a result byte is waiting);
    /// clear means CPU->FDC (the chip wants a command or parameter byte).
    pub const DIO: u8 = 0x40;
    /// NDM: a non-DMA execution phase is in progress. Always clear here: every
    /// modeled transfer runs over DMA.
    #[allow(dead_code)]
    pub const NDM: u8 = 0x20;
    /// CB: a command is in progress (set from the first command byte through the
    /// last result byte).
    pub const CB: u8 = 0x10;
    // Bits 3-0 are the per-drive seek-busy flags (DnB); not modeled as busy
    // because seeks complete inside the command.
}

/// Status register 0 (ST0) bit fields.
mod st0 {
    /// Interrupt code, bits 7-6: 00 normal termination, 01 abnormal, 10 invalid
    /// command, 11 ready-line changed during polling.
    pub const IC_NORMAL: u8 = 0x00;
    pub const IC_ABNORMAL: u8 = 0x40;
    pub const IC_INVALID: u8 = 0x80;
    /// SE: seek end. Set when a RECALIBRATE or SEEK finishes.
    pub const SE: u8 = 0x20;
    // bit2 head address, bits1-0 drive select are OR'd in from the unit.
}

/// Status register 3 (ST3) bit fields, returned by SENSE DRIVE STATUS.
mod st3 {
    /// TS: two-sided media. The modeled drives are all double-sided.
    pub const TWO_SIDED: u8 = 0x08;
    /// T0: track 0. Set while the head is over cylinder 0.
    pub const TRACK0: u8 = 0x10;
    /// RY: drive ready. Set when media is present.
    pub const READY: u8 = 0x20;
    /// WP: write protected. Modeled media is writable, so this stays clear.
    #[allow(dead_code)]
    pub const WRITE_PROTECT: u8 = 0x40;
    // bit2 head address, bits1-0 drive select are OR'd in from the unit.
}

/// What the chip is doing with the data register right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Idle: the next data-register write is a command opcode.
    Command,
    /// Collecting the parameter bytes a command needs before it executes.
    Parameters,
    /// Handing result bytes back to the CPU, one read at a time.
    Result,
}

/// A data transfer the machine must run for a READ DATA or WRITE DATA command.
/// The chip produces this after the parameter phase; the machine reads or writes
/// the addressed sector against the mounted image and moves the bytes over DMA
/// channel 2, then calls `complete_transfer` to enter the result phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TransferRequest {
    pub read: bool, // true READ DATA (disk->memory), false WRITE DATA (memory->disk)
    pub drive: u8,
    pub cylinder: u8,
    pub head: u8,
    pub sector: u8,         // first sector id (1-based)
    pub end_sector: u8,     // last sector id on the track to transfer (EOT parameter)
    pub bytes_per_sec: u16, // 128 << N from the N parameter (commonly 512)
}

/// The 8272A register file and command engine.
#[derive(Debug, Clone)]
pub(crate) struct Fdc {
    dor: u8,                                   // digital output register (0x3F2)
    phase: Phase,                              // current data-register phase
    command: Vec<u8>,                          // command opcode + parameter bytes collected so far
    needed_params: usize,                      // parameter bytes the current command still expects
    result: Vec<u8>,                           // result bytes the CPU reads back, last byte first
    present_cyl: [u8; 4],                      // tracked head cylinder per drive
    irq_pending: bool,    // a completion/seek interrupt is waiting for the host
    seek_interrupt: bool, // the pending interrupt came from RECALIBRATE/SEEK
    st0: u8,              // latched ST0 for SENSE INTERRUPT STATUS
    media_present: bool,  // whether a disk is mounted (drives the RY/disk-change bits)
    disk_changed: bool,   // DIR bit7: latched media-change line
    pending_transfer: Option<TransferRequest>, // a READ/WRITE the machine must run
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
            irq_pending: false,
            seek_interrupt: false,
            st0: 0,
            media_present: false,
            disk_changed: false,
            pending_transfer: None,
        }
    }
}

impl Fdc {
    /// Whether `port` is one of the controller's register ports. 0x3F6 is left
    /// out: on the AT that address is the hard-disk controller's, not the FDC's,
    /// and 0x3F7 is shared (the FDC owns its read for the DIR and its write for
    /// the CCR data rate). 0x3F0/0x3F1 are the PS/2 status registers A/B, accepted
    /// but not modeled.
    // Limit: 0x3F0/0x3F1 read back 0; a guest probing the PS/2 status-register
    // A/B bits sees nothing. The upgrade path is to model those two bytes if a
    // guest is found that reads the drive-select / write-protect mirror from them.
    pub(crate) fn owns_port(port: u16) -> bool {
        matches!(port, 0x3F0 | 0x3F1 | 0x3F2 | 0x3F4 | 0x3F5 | 0x3F7)
    }

    /// Tell the chip whether a disk is mounted. Drives the ST3 ready bit and the
    /// DIR disk-change line; the machine calls this when media is mounted or
    /// ejected so SENSE DRIVE STATUS and a DIR read report the truth.
    pub(crate) fn set_media_present(&mut self, present: bool) {
        if present != self.media_present {
            // Any change of the ready line latches the disk-change signal, which a
            // guest clears by stepping the head (a seek) to fresh media.
            self.disk_changed = true;
        }
        self.media_present = present;
    }

    /// DMA + interrupt enable (/NDMAGATE, DOR bit3). When clear the controller
    /// holds /DACK and /IRQ off the bus; a guest that drives the FDC by polling
    /// rather than DMA clears it. The transfer datapath still runs in the model;
    /// only the IRQ gate honors this bit.
    fn dma_irq_enabled(&self) -> bool {
        self.dor & 0x08 != 0
    }

    /// Port read. Returns None for ports this chip does not source on a read
    /// (0x3F2 DOR is write-mostly but reads back the last value; 0x3F7 reads the
    /// DIR, while a write to 0x3F7 is the CCR data-rate register).
    pub(crate) fn read_port(&mut self, port: u16) -> Option<u8> {
        match port {
            0x3F2 => Some(self.dor),
            0x3F4 => Some(self.main_status()),
            0x3F5 => Some(self.read_data()),
            0x3F7 => Some(self.read_dir()),
            _ => None,
        }
    }

    /// Port write. Returns true if the chip claims the port.
    pub(crate) fn write_port(&mut self, port: u16, value: u8) -> bool {
        match port {
            0x3F2 => {
                self.write_dor(value);
                true
            }
            0x3F5 => {
                self.write_data(value);
                true
            }
            0x3F7 => true, // CCR data-rate select: accepted, no observable state
            _ => false,
        }
    }

    /// Compose the main status register (0x3F4). RQM and DIO follow the phase; CB
    /// is set whenever a command is mid-flight (command, execution-pending, or
    /// result phase).
    fn main_status(&self) -> u8 {
        let mut s = msr::RQM; // the data register is always ready in this model
        match self.phase {
            Phase::Result => s |= msr::DIO | msr::CB, // FDC->CPU, command still busy
            Phase::Parameters => s |= msr::CB,        // CPU->FDC, command busy
            Phase::Command => {}                      // CPU->FDC, idle
        }
        if self.pending_transfer.is_some() {
            // An execution phase the machine has not yet run: still command-busy,
            // and the chip is not asking the CPU for a byte.
            s = (s | msr::CB) & !msr::RQM;
        }
        s
    }

    /// DIR read (0x3F7). Bit7 is the disk-change line; the rest read 0 here.
    fn read_dir(&mut self) -> u8 {
        if self.disk_changed { 0x80 } else { 0x00 }
    }

    /// DOR write (0x3F2). bit7-4 motor on for drives 3-0, bit3 DMA+IRQ enable,
    /// bit2 /reset (0 holds the chip in reset), bits1-0 drive select.
    fn write_dor(&mut self, value: u8) {
        let leaving_reset = self.dor & 0x04 == 0 && value & 0x04 != 0;
        let entering_reset = self.dor & 0x04 != 0 && value & 0x04 == 0;
        self.dor = value;
        if entering_reset {
            self.enter_reset();
        }
        if leaving_reset {
            // Coming out of reset the controller raises an interrupt and presents
            // an ST0 of 0xC0 (abnormal-termination / ready-changed) on the first
            // SENSE INTERRUPT STATUS, the documented power-up handshake.
            self.irq_pending = true;
            self.seek_interrupt = true;
            self.st0 = 0xC0;
        }
    }

    /// Hold the chip in reset: clear the phase, drop any in-flight command, and
    /// reset the data-register FIFOs. The DOR value itself is preserved.
    fn enter_reset(&mut self) {
        self.phase = Phase::Command;
        self.command.clear();
        self.result.clear();
        self.needed_params = 0;
        self.pending_transfer = None;
        self.irq_pending = false;
        self.seek_interrupt = false;
    }

    /// Data register read (0x3F5): hand back the next result byte. Returns 0 when
    /// no result is pending, the way the chip drives the bus with nothing to say.
    fn read_data(&mut self) -> u8 {
        if self.phase != Phase::Result {
            return 0;
        }
        // Result bytes were pushed in reverse so the first byte the CPU reads is
        // the last pushed; pop from the end.
        let byte = self.result.pop().unwrap_or(0);
        if self.result.is_empty() {
            self.phase = Phase::Command;
        }
        byte
    }

    /// Data register write (0x3F5): a command opcode or a parameter byte.
    fn write_data(&mut self, value: u8) {
        match self.phase {
            Phase::Command => self.begin_command(value),
            Phase::Parameters => {
                self.command.push(value);
                // command holds the opcode plus the parameters collected so far,
                // so its length is one more than the parameter count; execute once
                // every expected parameter is in.
                if self.command.len() > self.needed_params {
                    self.execute_command();
                }
            }
            Phase::Result => {
                // The CPU should not write during the result phase; ignore it, as
                // the chip does (the byte is not a command).
            }
        }
    }

    /// Start a new command: latch the opcode and decide how many parameter bytes
    /// follow. The low five bits select the command; bits 7-5 (MT/MFM/SK) are
    /// modifiers the model accepts but does not vary behavior on.
    fn begin_command(&mut self, opcode: u8) {
        self.command.clear();
        self.result.clear();
        self.command.push(opcode);
        let params = match opcode & 0x1F {
            0x03 => 2, // SPECIFY: SRT/HUT, HLT/ND
            0x04 => 1, // SENSE DRIVE STATUS: HDS+DS
            0x07 => 1, // RECALIBRATE: DS
            0x08 => 0, // SENSE INTERRUPT STATUS: no parameters
            0x06 => 8, // READ DATA: HDS+DS, C, H, R, N, EOT, GPL, DTL
            0x05 => 8, // WRITE DATA: same parameter shape as READ DATA
            0x0A => 1, // READ ID: HDS+DS
            0x0F => 2, // SEEK: HDS+DS, NCN
            0x13 => 3, // CONFIGURE: 0, config, precomp
            0x10 => 0, // VERSION: no parameters
            _ => {
                // Unknown opcode: the chip enters an invalid-command result of a
                // single ST0 = 0x80 byte and waits for it to be read.
                self.command.clear();
                self.result.clear();
                self.result.push(st0::IC_INVALID);
                self.phase = Phase::Result;
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

    /// Drive + head from a command's first parameter byte (HDS in bit2, DS in
    /// bits1-0), the shared layout of most commands' first byte.
    fn drive_head(&self) -> (u8, u8) {
        let p = self.command.get(1).copied().unwrap_or(0);
        (p & 0x03, (p >> 2) & 0x01)
    }

    /// Build an ST0 with the interrupt code, optional seek-end, and the drive and
    /// head OR'd in from the addressed unit.
    fn make_st0(&self, ic: u8, seek_end: bool, drive: u8, head: u8) -> u8 {
        let mut v = ic | (drive & 0x03) | ((head & 0x01) << 2);
        if seek_end {
            v |= st0::SE;
        }
        v
    }

    /// Run the command once its parameters are in. Commands with a data transfer
    /// stage a TransferRequest; the rest finish here.
    fn execute_command(&mut self) {
        let opcode = self.command[0] & 0x1F;
        match opcode {
            0x03 => self.cmd_specify(),
            0x04 => self.cmd_sense_drive_status(),
            0x07 => self.cmd_recalibrate(),
            0x08 => self.cmd_sense_interrupt(),
            0x0F => self.cmd_seek(),
            0x10 => self.cmd_version(),
            0x13 => self.cmd_configure(),
            0x0A => self.cmd_read_id(),
            0x06 => self.cmd_read_write(true),
            0x05 => self.cmd_read_write(false),
            _ => {
                self.finish_with_result(vec![st0::IC_INVALID]);
            }
        }
    }

    /// Push a result vector and enter the result phase. The vector is given in CPU
    /// read order (first byte the CPU reads first); it is reversed internally so a
    /// pop hands bytes back in order. An empty result returns to the idle command
    /// phase with no result handshake (SPECIFY, CONFIGURE).
    fn finish_with_result(&mut self, mut bytes: Vec<u8>) {
        self.command.clear();
        self.needed_params = 0;
        if bytes.is_empty() {
            self.phase = Phase::Command;
            self.result.clear();
            return;
        }
        bytes.reverse();
        self.result = bytes;
        self.phase = Phase::Result;
    }

    /// SPECIFY (0x03): load the step-rate / head-load timing. No result phase and
    /// no interrupt, exactly as the chip behaves.
    fn cmd_specify(&mut self) {
        // Limit: timing parameters (SRT/HUT/HLT) are accepted and dropped; we
        // do not model head-load or step-rate delays. The upgrade path is to feed
        // these into Floppy::access_duration_secs so a guest-programmed step rate
        // changes the modeled seek time.
        self.finish_with_result(vec![]);
    }

    /// SENSE DRIVE STATUS (0x04): return ST3 for the addressed drive.
    fn cmd_sense_drive_status(&mut self) {
        let (drive, head) = self.drive_head();
        let mut st3v = TWO_SIDED_BASE | (drive & 0x03) | ((head & 0x01) << 2);
        if self.present_cyl[drive as usize] == 0 {
            st3v |= st3::TRACK0;
        }
        if self.media_present {
            st3v |= st3::READY;
        }
        self.finish_with_result(vec![st3v]);
    }

    /// RECALIBRATE (0x07): step the head to cylinder 0 and raise a seek interrupt.
    /// No result phase; the host clears the interrupt with SENSE INTERRUPT STATUS.
    fn cmd_recalibrate(&mut self) {
        let drive = self.command.get(1).copied().unwrap_or(0) & 0x03;
        self.present_cyl[drive as usize] = 0;
        self.disk_changed = false; // stepping the head clears the change latch
        self.st0 = self.make_st0(st0::IC_NORMAL, true, drive, 0);
        self.irq_pending = true;
        self.seek_interrupt = true;
        self.finish_with_result(vec![]);
    }

    /// SEEK (0x0F): move the head to the new cylinder number and raise a seek
    /// interrupt. No result phase.
    fn cmd_seek(&mut self) {
        let (drive, head) = self.drive_head();
        let ncn = self.command.get(2).copied().unwrap_or(0);
        self.present_cyl[drive as usize] = ncn;
        self.disk_changed = false;
        self.st0 = self.make_st0(st0::IC_NORMAL, true, drive, head);
        self.irq_pending = true;
        self.seek_interrupt = true;
        self.finish_with_result(vec![]);
    }

    /// SENSE INTERRUPT STATUS (0x08): the only way to clear a seek/recal
    /// interrupt. Returns ST0 and the present cylinder number when an interrupt is
    /// pending; otherwise ST0 = 0x80 (invalid) and no PCN, the documented "no
    /// interrupt pending" reply.
    fn cmd_sense_interrupt(&mut self) {
        if self.seek_interrupt {
            let drive = (self.st0 & 0x03) as usize;
            let pcn = self.present_cyl[drive];
            self.seek_interrupt = false;
            self.irq_pending = false;
            self.finish_with_result(vec![self.st0, pcn]);
        } else {
            // No pending interrupt: invalid-command status, single byte, no PCN.
            self.finish_with_result(vec![st0::IC_INVALID]);
        }
    }

    /// VERSION (0x10): identify the controller. 0x90 marks an enhanced
    /// (uPD765B / 82077-class) part, the value PC/AT-era code checks for.
    fn cmd_version(&mut self) {
        self.finish_with_result(vec![0x90]);
    }

    /// CONFIGURE (0x13): set FIFO/polling options. Accepted, no result phase.
    fn cmd_configure(&mut self) {
        // Limit: the FIFO threshold, EIS (implied seek) and polling-disable
        // bits are accepted and ignored. The upgrade path is to honor EIS so a
        // READ/WRITE seeks to the target cylinder first, and to expose the FIFO
        // threshold if a non-DMA transfer path is ever added.
        self.finish_with_result(vec![]);
    }

    /// READ ID (0x0A): return the id of the next sector under the head. With no
    /// rotational model the chip reports sector 1 of the current cylinder.
    fn cmd_read_id(&mut self) {
        let (drive, head) = self.drive_head();
        let cyl = self.present_cyl[drive as usize];
        let st0v = self.make_st0(st0::IC_NORMAL, false, drive, head);
        // Result is ST0, ST1, ST2, then the C/H/R/N address mark of the sector.
        // Limit: with no MFM/rotation model READ ID always names sector 1, N=2
        // (512-byte sectors). The upgrade path is a real index/rotation counter so
        // the reported sector advances with disk angle.
        self.finish_with_result(vec![st0v, 0, 0, cyl, head, 1, 2]);
    }

    /// READ DATA (0x06) / WRITE DATA (0x05): set up the execution-phase transfer
    /// the machine runs over DMA channel 2. The result phase is produced later by
    /// `complete_transfer` once the bytes have moved.
    fn cmd_read_write(&mut self, read: bool) {
        // Parameter bytes (after the opcode): HDS+DS, C, H, R, N, EOT, GPL, DTL.
        let p = &self.command[1..];
        let drive = p[0] & 0x03;
        let head = (p[0] >> 2) & 0x01;
        let cyl = p[1];
        let sector = p[3];
        let n = p[4];
        let eot = p[5];
        let bytes_per_sec = 128u16 << (n.min(7)); // N encodes 128<<N bytes/sector
        self.present_cyl[drive as usize] = cyl; // an access implies the head is here
        self.pending_transfer = Some(TransferRequest {
            read,
            drive,
            cylinder: cyl,
            head,
            sector,
            end_sector: eot,
            bytes_per_sec,
        });
        // The machine sees pending_transfer set and runs it; until then the chip
        // is command-busy with RQM low. command/needed_params are cleared so a
        // stray data write does not corrupt the staged transfer.
        self.command.clear();
        self.needed_params = 0;
    }

    /// The machine takes the staged transfer (if any) to run the floppy + DMA
    /// datapath. Returns None when nothing is pending.
    pub(crate) fn take_transfer(&mut self) -> Option<TransferRequest> {
        self.pending_transfer.take()
    }

    /// Finish a READ/WRITE DATA command after the machine moved the data. `last`
    /// is the address of the last sector actually transferred; `success` false
    /// means the access ran off the media. Produces the seven-byte result phase
    /// (ST0, ST1, ST2, C, H, R, N) and raises the completion interrupt.
    pub(crate) fn complete_transfer(
        &mut self,
        req: TransferRequest,
        last_cyl: u8,
        last_head: u8,
        last_sector: u8,
        success: bool,
    ) {
        let ic = if success {
            st0::IC_NORMAL
        } else {
            st0::IC_ABNORMAL
        };
        let st0v = self.make_st0(ic, false, req.drive, req.head);
        // ST1 is clean on a normal completion; on failure bit2 ND (no data /
        // sector not found) marks the missing or off-media sector.
        // Limit: ST1 bit7 EN (end of cylinder) is never set, so a guest that
        // reads to the very last sector without a TC will not see EN. The upgrade
        // path is to flag EN when the walk reaches geom.sectors before TC.
        let st1 = if success { 0x00 } else { 0x04 };
        let n = match req.bytes_per_sec {
            128 => 0,
            256 => 1,
            512 => 2,
            1024 => 3,
            _ => 2,
        };
        self.st0 = st0v;
        self.irq_pending = true;
        self.seek_interrupt = false; // a data interrupt is cleared by reading results
        self.finish_with_result(vec![st0v, st1, 0, last_cyl, last_head, last_sector, n]);
    }

    /// Take the pending IRQ6 edge, if any, for the host's IRQ-collection step.
    /// Honors the DOR DMA/IRQ gate: a disabled gate masks the line.
    pub(crate) fn take_irq(&mut self) -> bool {
        if self.irq_pending && self.dma_irq_enabled() {
            // For a data-transfer interrupt the edge is one-shot; a seek interrupt
            // stays pending until SENSE INTERRUPT STATUS clears it, but the host
            // only needs one edge to vector its ISR, so clear the edge either way.
            self.irq_pending = false;
            true
        } else {
            false
        }
    }
}

/// ST3 always reports two-sided media for the modeled drives.
const TWO_SIDED_BASE: u8 = st3::TWO_SIDED;

#[cfg(test)]
#[path = "fdc_test.rs"]
mod tests;
