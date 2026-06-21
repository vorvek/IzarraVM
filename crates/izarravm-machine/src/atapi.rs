//! ATAPI command interpreter for the CD-ROM drive.
//!
//! This holds the mounted [`CdImage`], the audio-playback state, and the most
//! recent sense data, and turns a 12-byte ATAPI command descriptor block (CDB)
//! into a data-in buffer. The IDE register file (`ide.rs`) owns the ATA-level
//! handshake (status, byte count, interrupts); this layer is the SCSI/MMC
//! payload. The split keeps the bus-facing state machine separate from the
//! command set so each is testable on its own.
//!
//! Command set implemented (per the SFF-8020i / MMC packet command set): TEST
//! UNIT READY, REQUEST SENSE, INQUIRY (standard + EVPD pages 0x00/0x80/0x83),
//! START/STOP UNIT, PREVENT/ALLOW MEDIUM REMOVAL, READ CAPACITY, SEEK, READ
//! HEADER, READ TOC/PMA/ATIP, READ(10), READ(12), MODE SENSE(10), and the
//! CD-Audio set PLAY AUDIO(10), PLAY AUDIO MSF, PAUSE/RESUME, STOP, READ
//! SUB-CHANNEL. IDENTIFY PACKET DEVICE is answered by the register file directly
//! since it is an ATA command, not a packet command.

use crate::cdimage::{CdImage, DATA_SECTOR, lba_to_msf, msf_to_lba};

/// 12x CD-ROM transfer ceiling: ~1800 KB/s sustained. Used by the timing model
/// in the machine to charge a read its mechanical cost, mirroring the floppy.
pub const CD_BYTES_PER_SEC: f64 = 1_800.0 * 1024.0;
/// Worst-case full-stroke seek for a 12x drive, ~100 ms. A read pays a fraction
/// of this proportional to how far the head moved.
pub const CD_SEEK_MAX_SECS: f64 = 0.100;

/// SCSI sense keys this device reports.
pub mod sense_key {
    pub const NO_SENSE: u8 = 0x00;
    pub const NOT_READY: u8 = 0x02;
    pub const ILLEGAL_REQUEST: u8 = 0x05;
    pub const UNIT_ATTENTION: u8 = 0x06;
}

/// Additional sense codes (ASC/ASCQ pairs) used by the replies.
pub mod asc {
    pub const NO_ADDITIONAL: (u8, u8) = (0x00, 0x00);
    pub const NOT_READY_NO_MEDIUM: (u8, u8) = (0x3A, 0x00);
    pub const INVALID_COMMAND: (u8, u8) = (0x20, 0x00);
    pub const INVALID_FIELD_IN_CDB: (u8, u8) = (0x24, 0x00);
    pub const LBA_OUT_OF_RANGE: (u8, u8) = (0x21, 0x00);
    pub const MEDIUM_MAY_HAVE_CHANGED: (u8, u8) = (0x28, 0x00);
    pub const MEDIUM_REMOVAL_PREVENTED: (u8, u8) = (0x53, 0x02);
}

/// ATA status BSY bit (0x80). Asserted while a command or packet is in flight
/// and cleared when the result phase is ready. The IDE register file (`ide.rs`)
/// runs each command synchronously, so the busy window is momentary; this device
/// models the bit so the register file can publish it on the status port.
// ponytail: the synchronous model never opens a real busy window, so ide.rs has
// no place to publish this bit; it stays defined for fidelity but uncalled.
#[allow(dead_code)]
pub const BSY: u8 = 0x80;

/// The ATAPI Interrupt Reason register is the sector-count register, reinterpreted
/// during a packet transfer: bit0 = C/D (1 = command/CDB phase, 0 = data phase),
/// bit1 = I/O (1 = transfer to host, 0 = transfer from host). The three states a
/// packet command moves through map to three byte values.
pub mod interrupt_reason {
    /// Awaiting the command packet (CDB): C/D=1, I/O=0.
    pub const AWAIT_PACKET: u8 = 0x01;
    /// Data-in armed (device-to-host): C/D=0, I/O=1.
    pub const DATA_IN: u8 = 0x02;
    /// Command complete: C/D=1, I/O=1.
    pub const COMMAND_COMPLETE: u8 = 0x03;
}

/// Outcome of interpreting one CDB.
pub enum CmdResult {
    /// Command completed; the device returns this data-in buffer to the host.
    /// Empty for a command with no data phase (TEST UNIT READY, PLAY, etc.).
    Data(Vec<u8>),
    /// Command failed; CHECK CONDITION with the sense already latched.
    Error,
}

/// CD audio playback state, advanced by the machine clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Playback {
    /// True while audio is being played (not paused, not stopped).
    pub playing: bool,
    /// True when paused (held position, resumable).
    pub paused: bool,
    /// Current play LBA (the next frame the mixer will stream).
    pub current_lba: u32,
    /// One past the last LBA to play; playback stops when current reaches it.
    pub end_lba: u32,
}

impl Playback {
    fn stop(&mut self) {
        self.playing = false;
        self.paused = false;
    }
}

/// The ATAPI CD-ROM device: a mounted image, playback state, and latched sense.
#[derive(Debug, Default)]
pub struct AtapiDevice {
    image: Option<CdImage>,
    play: Playback,
    /// Latched sense: (key, asc, ascq). REQUEST SENSE returns and clears it.
    sense_key: u8,
    asc: u8,
    ascq: u8,
    /// Failing LBA latched for an LBA-out-of-range error, surfaced by REQUEST
    /// SENSE in the INFORMATION field with the VALID bit set.
    sense_information: Option<u32>,
    /// Set on a fresh mount so the first TEST UNIT READY reports UNIT ATTENTION
    /// (medium changed), as a real drive does after a disc swap.
    media_changed: bool,
    /// Latched by PREVENT/ALLOW MEDIUM REMOVAL (0x1E). While true, START STOP UNIT
    /// refuses to eject the tray.
    prevent_removal: bool,
    /// START STOP UNIT spin state. Cosmetic: the model serves data regardless, but
    /// drivers that stop then start the unit see the flag flip.
    started: bool,
    /// The interrupt-reason byte (C/D, I/O) for the current packet phase. The IDE
    /// register file reads this through [`Self::interrupt_reason`] and publishes it
    /// on the sector-count port; this is the device-side source of truth.
    interrupt_reason: u8,
}

impl AtapiDevice {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mount a CD image, raising the medium-changed condition the next command
    /// reports as UNIT ATTENTION.
    pub fn insert(&mut self, image: CdImage) {
        self.image = Some(image);
        self.media_changed = true;
        self.play = Playback::default();
        self.set_sense(
            sense_key::UNIT_ATTENTION,
            asc::MEDIUM_MAY_HAVE_CHANGED.0,
            asc::MEDIUM_MAY_HAVE_CHANGED.1,
        );
    }

    /// Eject the disc, leaving the drive empty.
    pub fn eject(&mut self) {
        self.image = None;
        self.media_changed = true;
        self.play = Playback::default();
    }

    pub fn is_loaded(&self) -> bool {
        self.image.is_some()
    }

    pub fn image(&self) -> Option<&CdImage> {
        self.image.as_ref()
    }

    pub fn playback(&self) -> Playback {
        self.play
    }

    // ponytail: these two tray/spin queries feed ide.rs (status port), which does
    // not surface them on any ATA register, so they have no in-crate caller yet.
    /// Whether PREVENT/ALLOW MEDIUM REMOVAL currently locks the tray.
    #[allow(dead_code)]
    pub fn removal_prevented(&self) -> bool {
        self.prevent_removal
    }

    /// Whether START STOP UNIT has the unit spun up.
    #[allow(dead_code)]
    pub fn started(&self) -> bool {
        self.started
    }

    /// The interrupt-reason (C/D, I/O) byte for the phase the last command left
    /// the device in. The IDE register file reads this to drive the sector-count
    /// port. Awaiting a packet is signalled by [`Self::arm_packet`].
    pub fn interrupt_reason(&self) -> u8 {
        self.interrupt_reason
    }

    /// Mark the device as awaiting the command packet (CDB write phase). Sets the
    /// interrupt reason to C/D=1, I/O=0. Called by the register file when the host
    /// issues the ATA PACKET command, before the CDB arrives.
    pub fn arm_packet(&mut self) {
        self.interrupt_reason = interrupt_reason::AWAIT_PACKET;
    }

    fn set_sense(&mut self, key: u8, asc: u8, ascq: u8) {
        self.sense_key = key;
        self.asc = asc;
        self.ascq = ascq;
    }

    fn fail(&mut self, key: u8, code: (u8, u8)) -> CmdResult {
        self.set_sense(key, code.0, code.1);
        CmdResult::Error
    }

    /// Latch a failing LBA alongside a sense condition so REQUEST SENSE can report
    /// it in the INFORMATION field with the VALID bit set.
    fn fail_at_lba(&mut self, key: u8, code: (u8, u8), lba: u32) -> CmdResult {
        self.sense_information = Some(lba);
        self.fail(key, code)
    }

    /// Advance audio playback by `frames` Red Book frames, stopping at the end
    /// of the play range. Called by the mixer as it consumes frames.
    pub fn advance_play(&mut self, frames: u32) {
        if !self.play.playing {
            return;
        }
        self.play.current_lba = self.play.current_lba.saturating_add(frames);
        if self.play.current_lba >= self.play.end_lba {
            self.play.current_lba = self.play.end_lba;
            self.play.stop();
        }
    }

    /// The Red Book frame at the current play position, without advancing. Returns
    /// None when not playing or off the end of the play range. A non-audio LBA
    /// inside the range yields silence (a zeroed frame) rather than data. The
    /// mixer reads this, consumes its samples, then calls `advance_play` to step
    /// to the next frame.
    pub fn peek_audio_frame(&self) -> Option<[u8; crate::cdimage::RAW_SECTOR]> {
        if !self.play.playing || self.play.current_lba >= self.play.end_lba {
            return None;
        }
        let lba = self.play.current_lba;
        let frame = self.image.as_ref()?.read_audio_frame(lba);
        Some(frame.unwrap_or([0u8; crate::cdimage::RAW_SECTOR]))
    }

    /// Pull the next audio frame to render, advancing the play position by one
    /// frame. Returns None when not playing or off the end. A convenience wrapper
    /// over `peek_audio_frame` + `advance_play(1)` used by the unit tests.
    #[cfg(test)]
    pub fn next_audio_frame(&mut self) -> Option<[u8; crate::cdimage::RAW_SECTOR]> {
        let frame = self.peek_audio_frame()?;
        self.advance_play(1);
        Some(frame)
    }

    /// Interpret a 12-byte CDB and return its data-in buffer (or an error with
    /// latched sense). `alloc_len` caps the returned buffer the way the ATA byte
    /// count limit register does on hardware; callers truncate to it.
    pub fn execute(&mut self, cdb: &[u8; 12]) -> CmdResult {
        let result = match cdb[0] {
            0x00 => self.test_unit_ready(),
            0x03 => self.request_sense(cdb),
            0x12 => self.inquiry(cdb),
            0x1B => self.start_stop_unit(cdb),
            0x1E => self.prevent_allow_removal(cdb),
            0x25 => self.read_capacity(),
            0x28 => self.read10(cdb),
            0x2B => self.seek(cdb),
            0x42 => self.read_subchannel(cdb),
            0x43 => self.read_toc(cdb),
            0x44 => self.read_header(cdb),
            0x45 => self.play_audio10(cdb),
            0x47 => self.play_audio_msf(cdb),
            0x4B => self.pause_resume(cdb),
            0x4E => self.stop_audio(),
            0x5A => self.mode_sense10(cdb),
            0xA8 => self.read12(cdb),
            0xBD => self.mechanism_status(cdb),
            _ => self.fail(sense_key::ILLEGAL_REQUEST, asc::INVALID_COMMAND),
        };
        // Reflect the resulting transfer phase in the interrupt reason: a data-in
        // buffer leaves the device armed for the data phase, while a no-data
        // success and any error land on command-complete (C/D=1, I/O=1). The IDE
        // register file flips DATA_IN back to COMMAND_COMPLETE once the buffer
        // drains; this device exposes the entry value.
        self.interrupt_reason = match &result {
            CmdResult::Data(buf) if !buf.is_empty() => interrupt_reason::DATA_IN,
            _ => interrupt_reason::COMMAND_COMPLETE,
        };
        result
    }

    fn test_unit_ready(&mut self) -> CmdResult {
        if self.image.is_none() {
            return self.fail(sense_key::NOT_READY, asc::NOT_READY_NO_MEDIUM);
        }
        if self.media_changed {
            self.media_changed = false;
            return self.fail(sense_key::UNIT_ATTENTION, asc::MEDIUM_MAY_HAVE_CHANGED);
        }
        CmdResult::Data(Vec::new())
    }

    fn request_sense(&mut self, cdb: &[u8; 12]) -> CmdResult {
        // Fixed-format sense data (18 bytes), SPC.
        let alloc = cdb[4] as usize;
        let mut buf = vec![0u8; 18];
        buf[0] = 0x70; // current error, fixed format
        // When a failing LBA was latched (LBA out of range), set the VALID bit
        // (byte 0 bit 7) and place the LBA in the INFORMATION field (bytes 3-6).
        if let Some(lba) = self.sense_information {
            buf[0] |= 0x80; // VALID
            buf[3..7].copy_from_slice(&lba.to_be_bytes());
        }
        buf[2] = self.sense_key & 0x0F;
        buf[7] = 10; // additional sense length (bytes beyond index 7)
        buf[12] = self.asc;
        buf[13] = self.ascq;
        // REQUEST SENSE clears the latched condition.
        self.set_sense(
            sense_key::NO_SENSE,
            asc::NO_ADDITIONAL.0,
            asc::NO_ADDITIONAL.1,
        );
        self.sense_information = None;
        truncate(buf, alloc)
    }

    fn inquiry(&mut self, cdb: &[u8; 12]) -> CmdResult {
        let alloc = cdb[4] as usize;
        let evpd = cdb[1] & 0x01 != 0;
        let page = cdb[2];
        if evpd {
            return self.inquiry_vpd(page, alloc);
        }
        // A non-EVPD request with a nonzero page code is illegal.
        if page != 0 {
            return self.fail(sense_key::ILLEGAL_REQUEST, asc::INVALID_FIELD_IN_CDB);
        }
        let mut buf = vec![0u8; 36];
        buf[0] = 0x05; // peripheral device type 5 = CD-ROM
        buf[1] = 0x80; // RMB: removable medium
        buf[2] = 0x00; // ANSI version 0 (ATAPI), matches many real CD drives
        buf[3] = 0x21; // response data format 1, ATAPI
        buf[4] = 31; // additional length
        write_ascii(&mut buf[8..16], "Izarra");
        write_ascii(&mut buf[16..32], "CD-ROM 12X");
        write_ascii(&mut buf[32..36], "1.0");
        truncate(buf, alloc)
    }

    /// INQUIRY with EVPD set: a vital product data page. Page 0x00 is the supported
    /// page list, 0x80 the unit serial number; any other page is an illegal field.
    fn inquiry_vpd(&mut self, page: u8, alloc: usize) -> CmdResult {
        const SERIAL: &str = "IZARRA-CD-0001";
        match page {
            0x00 => {
                // Supported VPD pages: header (4 bytes) then the page-code list.
                let pages = [0x00u8, 0x80, 0x83];
                let mut buf = vec![0u8; 4];
                buf[0] = 0x05; // peripheral device type
                buf[1] = 0x00; // page code
                buf[3] = pages.len() as u8; // page length
                buf.extend_from_slice(&pages);
                truncate(buf, alloc)
            }
            0x80 => {
                // Unit serial number page.
                let serial = SERIAL.as_bytes();
                let mut buf = vec![0u8; 4];
                buf[0] = 0x05;
                buf[1] = 0x80;
                buf[3] = serial.len() as u8;
                buf.extend_from_slice(serial);
                truncate(buf, alloc)
            }
            0x83 => {
                // Device identification: a single ASCII (codeset 2) identifier
                // carrying the serial, the minimum a probe expects from page 0x83.
                let serial = SERIAL.as_bytes();
                let mut desc = vec![0u8; 4];
                desc[0] = 0x02; // ASCII codeset, vendor-specific id type
                desc[3] = serial.len() as u8;
                desc.extend_from_slice(serial);
                let mut buf = vec![0u8; 4];
                buf[0] = 0x05;
                buf[1] = 0x83;
                let len = desc.len() as u16;
                buf[2..4].copy_from_slice(&len.to_be_bytes());
                buf.extend_from_slice(&desc);
                truncate(buf, alloc)
            }
            _ => self.fail(sense_key::ILLEGAL_REQUEST, asc::INVALID_FIELD_IN_CDB),
        }
    }

    /// PREVENT/ALLOW MEDIUM REMOVAL (0x1E). Byte 4 bit 0 latches the prevent flag,
    /// locking the tray against an eject. NOT READY when the drive is empty.
    fn prevent_allow_removal(&mut self, cdb: &[u8; 12]) -> CmdResult {
        if self.image.is_none() {
            return self.fail(sense_key::NOT_READY, asc::NOT_READY_NO_MEDIUM);
        }
        self.prevent_removal = cdb[4] & 0x01 != 0;
        CmdResult::Data(Vec::new())
    }

    /// START STOP UNIT (0x1B). Byte 4: LoEj (bit 1) requests a tray eject, Start
    /// (bit 0) spins the unit up or down. LoEj with Start clear ejects, but only
    /// when removal is not prevented; otherwise CHECK CONDITION with medium-removal
    /// prevented. ponytail: the GUI owns the host file, so an eject-on-command only
    /// marks the tray state, it does not close the backing image.
    fn start_stop_unit(&mut self, cdb: &[u8; 12]) -> CmdResult {
        let loej = cdb[4] & 0x02 != 0;
        let start = cdb[4] & 0x01 != 0;
        if loej && !start {
            // Eject request. SFF-8020i (Tables 84/156/157) requires sense key NOT READY,
            // not ILLEGAL REQUEST, when the medium is locked by PREVENT/ALLOW.
            if self.prevent_removal {
                return self.fail(sense_key::NOT_READY, asc::MEDIUM_REMOVAL_PREVENTED);
            }
            self.eject();
            return CmdResult::Data(Vec::new());
        }
        if loej && start {
            // Load (close tray): nothing host-side to load, accept.
            self.started = true;
            return CmdResult::Data(Vec::new());
        }
        // No eject: Start bit flips the spin state.
        self.started = start;
        CmdResult::Data(Vec::new())
    }

    fn read_capacity(&mut self) -> CmdResult {
        let Some(image) = &self.image else {
            return self.fail(sense_key::NOT_READY, asc::NOT_READY_NO_MEDIUM);
        };
        // READ CAPACITY reports the LBA of the LAST sector and the block size.
        let last = image.total_sectors().saturating_sub(1);
        let mut buf = vec![0u8; 8];
        buf[0..4].copy_from_slice(&last.to_be_bytes());
        buf[4..8].copy_from_slice(&(DATA_SECTOR as u32).to_be_bytes());
        CmdResult::Data(buf)
    }

    fn read10(&mut self, cdb: &[u8; 12]) -> CmdResult {
        let lba = u32::from_be_bytes([cdb[2], cdb[3], cdb[4], cdb[5]]);
        let count = u16::from_be_bytes([cdb[7], cdb[8]]) as u32;
        self.read_sectors(lba, count)
    }

    fn read12(&mut self, cdb: &[u8; 12]) -> CmdResult {
        let lba = u32::from_be_bytes([cdb[2], cdb[3], cdb[4], cdb[5]]);
        let count = u32::from_be_bytes([cdb[6], cdb[7], cdb[8], cdb[9]]);
        self.read_sectors(lba, count)
    }

    /// SEEK (0x2B). LBA in bytes 2-5 big-endian, no data phase. Validates the LBA
    /// against the disc capacity and reports NOT READY when empty. A successful
    /// seek returns an empty data buffer; the model has no head to move.
    fn seek(&mut self, cdb: &[u8; 12]) -> CmdResult {
        let lba = u32::from_be_bytes([cdb[2], cdb[3], cdb[4], cdb[5]]);
        let Some(image) = &self.image else {
            return self.fail(sense_key::NOT_READY, asc::NOT_READY_NO_MEDIUM);
        };
        if lba >= image.total_sectors() {
            return self.fail_at_lba(sense_key::ILLEGAL_REQUEST, asc::LBA_OUT_OF_RANGE, lba);
        }
        CmdResult::Data(Vec::new())
    }

    /// READ HEADER (0x44). Bytes 2-5 hold the LBA; byte 1 bit 1 selects an MSF
    /// address over LBA. Returns a 4-byte header followed by the 4-byte address:
    /// the data-mode byte (0x01 for a MODE1 data sector, 0x00 for an audio or hole)
    /// then three reserved bytes, then the requested address. ponytail: the model
    /// does not synthesize the full CD sub-header, just the mode and address a
    /// driver probes.
    fn read_header(&mut self, cdb: &[u8; 12]) -> CmdResult {
        let msf = cdb[1] & 0x02 != 0;
        let lba = u32::from_be_bytes([cdb[2], cdb[3], cdb[4], cdb[5]]);
        let alloc = u16::from_be_bytes([cdb[7], cdb[8]]) as usize;
        let Some(image) = &self.image else {
            return self.fail(sense_key::NOT_READY, asc::NOT_READY_NO_MEDIUM);
        };
        if lba >= image.total_sectors() {
            return self.fail_at_lba(sense_key::ILLEGAL_REQUEST, asc::LBA_OUT_OF_RANGE, lba);
        }
        let data_mode = match image.track_at_lba(lba) {
            Some(t) if !t.mode.is_audio() => 0x01,
            _ => 0x00,
        };
        let mut buf = vec![0u8; 8];
        buf[0] = data_mode;
        put_addr(&mut buf[4..8], lba, msf);
        truncate(buf, alloc)
    }

    fn read_sectors(&mut self, lba: u32, count: u32) -> CmdResult {
        let Some(image) = &self.image else {
            return self.fail(sense_key::NOT_READY, asc::NOT_READY_NO_MEDIUM);
        };
        if count == 0 {
            return CmdResult::Data(Vec::new());
        }
        let end = lba.saturating_add(count);
        if end > image.total_sectors() {
            return self.fail_at_lba(sense_key::ILLEGAL_REQUEST, asc::LBA_OUT_OF_RANGE, lba);
        }
        let mut buf = Vec::with_capacity(count as usize * DATA_SECTOR);
        for l in lba..end {
            match image.read_data_sector(l) {
                Some(sector) => buf.extend_from_slice(&sector),
                // A read that lands in an audio track or a hole reports an
                // illegal mode for this track.
                None => return self.fail(sense_key::ILLEGAL_REQUEST, asc::INVALID_FIELD_IN_CDB),
            }
        }
        CmdResult::Data(buf)
    }

    /// READ TOC/PMA/ATIP (0x43). Format 0 (TOC) returns one track descriptor per
    /// track plus the lead-out (track 0xAA). MSF bit (byte 1, bit 1) selects MSF
    /// addresses over LBA. The starting track number is byte 6.
    fn read_toc(&mut self, cdb: &[u8; 12]) -> CmdResult {
        let Some(image) = &self.image else {
            return self.fail(sense_key::NOT_READY, asc::NOT_READY_NO_MEDIUM);
        };
        let msf = cdb[1] & 0x02 != 0;
        let format = cdb[2] & 0x0F;
        let start_track = cdb[6];
        let alloc = u16::from_be_bytes([cdb[7], cdb[8]]) as usize;

        // Only TOC format 0 is modeled in full; ATIP/PMA are out of scope and
        // fall back to an empty TOC header rather than faulting.
        if format != 0 {
            let mut buf = vec![0u8; 4];
            buf[2] = 1; // first track
            buf[3] = image.track_count(); // last track
            let len = (buf.len() - 2) as u16;
            buf[0..2].copy_from_slice(&len.to_be_bytes());
            return truncate(buf, alloc);
        }

        let tracks = image.tracks();
        let first = tracks.first().map(|t| t.number).unwrap_or(1);
        let last = tracks.last().map(|t| t.number).unwrap_or(1);

        let mut body = Vec::new();
        for t in tracks {
            if t.number < start_track {
                continue;
            }
            body.extend_from_slice(&toc_descriptor(
                t.number,
                track_control(t.mode.is_audio()),
                t.start_lba,
                msf,
            ));
        }
        // Lead-out descriptor (track number 0xAA) at the disc capacity.
        if start_track <= 0xAA {
            body.extend_from_slice(&toc_descriptor(
                0xAA,
                0x14, // data, lead-out
                image.total_sectors(),
                msf,
            ));
        }

        let mut buf = vec![0u8; 4];
        buf[2] = first;
        buf[3] = last;
        buf.extend_from_slice(&body);
        let data_len = (buf.len() - 2) as u16;
        buf[0..2].copy_from_slice(&data_len.to_be_bytes());
        truncate(buf, alloc)
    }

    fn mode_sense10(&mut self, cdb: &[u8; 12]) -> CmdResult {
        let page = cdb[2] & 0x3F;
        let alloc = u16::from_be_bytes([cdb[7], cdb[8]]) as usize;
        // 8-byte MODE SENSE(10) header, then the requested page. We answer the
        // CD-ROM capabilities page (0x2A) used by ICDEX/drivers to probe speed.
        let mut page_bytes = Vec::new();
        if page == 0x2A || page == 0x3F {
            page_bytes.extend_from_slice(&caps_page_2a());
        }
        let mut buf = vec![0u8; 8];
        let total = (page_bytes.len() + 6) as u16; // mode data length excludes its own 2 bytes
        buf[0..2].copy_from_slice(&total.to_be_bytes());
        buf[2] = 0x05; // medium type: CD-ROM
        buf.extend_from_slice(&page_bytes);
        truncate(buf, alloc)
    }

    fn play_audio10(&mut self, cdb: &[u8; 12]) -> CmdResult {
        let lba = u32::from_be_bytes([cdb[2], cdb[3], cdb[4], cdb[5]]);
        let count = u16::from_be_bytes([cdb[7], cdb[8]]) as u32;
        self.start_play(lba, lba.saturating_add(count))
    }

    fn play_audio_msf(&mut self, cdb: &[u8; 12]) -> CmdResult {
        // Bytes 3-5 are the starting MSF, 6-8 the ending MSF.
        let start = msf_to_lba(cdb[3], cdb[4], cdb[5]);
        let end = msf_to_lba(cdb[6], cdb[7], cdb[8]);
        self.start_play(start, end)
    }

    fn start_play(&mut self, start: u32, end: u32) -> CmdResult {
        if self.image.is_none() {
            return self.fail(sense_key::NOT_READY, asc::NOT_READY_NO_MEDIUM);
        }
        if end < start {
            return self.fail(sense_key::ILLEGAL_REQUEST, asc::INVALID_FIELD_IN_CDB);
        }
        self.play = Playback {
            playing: start < end,
            paused: false,
            current_lba: start,
            end_lba: end,
        };
        CmdResult::Data(Vec::new())
    }

    fn pause_resume(&mut self, cdb: &[u8; 12]) -> CmdResult {
        // Byte 8 bit 0: 1 = resume, 0 = pause.
        let resume = cdb[8] & 0x01 != 0;
        if resume {
            if self.play.paused {
                self.play.paused = false;
                self.play.playing = self.play.current_lba < self.play.end_lba;
            }
        } else if self.play.playing {
            self.play.playing = false;
            self.play.paused = true;
        }
        CmdResult::Data(Vec::new())
    }

    fn stop_audio(&mut self) -> CmdResult {
        self.play.stop();
        CmdResult::Data(Vec::new())
    }

    /// READ SUB-CHANNEL (0x42), sub-channel data format 1 (current position).
    /// Reports the audio status and, when requested, the current play address.
    fn read_subchannel(&mut self, cdb: &[u8; 12]) -> CmdResult {
        let msf = cdb[1] & 0x02 != 0;
        let subq = cdb[2] & 0x40 != 0; // SubQ bit: include sub-channel data
        let format = cdb[3];
        let alloc = u16::from_be_bytes([cdb[7], cdb[8]]) as usize;

        let audio_status = if self.play.playing {
            0x11 // audio play in progress
        } else if self.play.paused {
            0x12 // audio play paused
        } else {
            0x13 // audio play completed / no current status
        };

        let mut buf = vec![0u8; 4];
        buf[1] = audio_status;
        if subq && format == 0x01 {
            // CURRENT POSITION data block (12 bytes).
            let lba = self.play.current_lba;
            let track = self
                .image
                .as_ref()
                .and_then(|i| i.track_at_lba(lba))
                .map(|t| t.number)
                .unwrap_or(1);
            let mut block = vec![0u8; 12];
            block[0] = 0x01; // sub-channel data format code
            block[1] = 0x10; // ADR=1, control=data/audio (audio here)
            block[2] = track;
            block[3] = 1; // index
            put_addr(&mut block[4..8], lba, msf); // absolute address
            put_addr(&mut block[8..12], lba, msf); // track-relative (approx)
            buf.extend_from_slice(&block);
        }
        let data_len = (buf.len() - 2) as u16;
        buf[2..4].copy_from_slice(&data_len.to_be_bytes());
        truncate(buf, alloc)
    }

    /// MECHANISM STATUS (0xBD): a minimal 8-byte reply so drivers that probe it
    /// see a ready, non-changing mechanism.
    fn mechanism_status(&mut self, cdb: &[u8; 12]) -> CmdResult {
        let alloc = u16::from_be_bytes([cdb[8], cdb[9]]) as usize;
        let buf = vec![0u8; 8];
        truncate(buf, alloc)
    }
}

/// Build one 8-byte TOC track descriptor for READ TOC format 0.
fn toc_descriptor(track: u8, control: u8, lba: u32, msf: bool) -> [u8; 8] {
    let mut d = [0u8; 8];
    d[0] = 0; // reserved
    d[1] = control; // ADR (high nibble) | control (low nibble)
    d[2] = track;
    d[3] = 0; // reserved
    put_addr(&mut d[4..8], lba, msf);
    d
}

/// Write a track address into a 4-byte field as either an LBA (big-endian) or
/// MSF (0, M, S, F).
fn put_addr(out: &mut [u8], lba: u32, msf: bool) {
    if msf {
        let (m, s, f) = lba_to_msf(lba);
        out[0] = 0;
        out[1] = m;
        out[2] = s;
        out[3] = f;
    } else {
        out.copy_from_slice(&lba.to_be_bytes());
    }
}

/// ADR/control nibble for a TOC entry: ADR=1, control = 0x04 (data) or 0x00
/// (audio), placed in the low nibble.
fn track_control(is_audio: bool) -> u8 {
    if is_audio {
        0x10 // ADR=1, control=0 (audio, 2 channels)
    } else {
        0x14 // ADR=1, control=4 (data track)
    }
}

/// The CD-ROM Capabilities and Mechanical Status page (0x2A), enough fields for
/// a driver to read the 12x speed. Length byte plus the speed words.
fn caps_page_2a() -> Vec<u8> {
    let mut p = vec![0u8; 22];
    p[0] = 0x2A; // page code
    p[1] = 20; // page length
    p[2] = 0x01; // CD-R read supported bit (cosmetic)
    // Max read speed in KB/s (byte 8-9) and current read speed (byte 14-15).
    let speed = (CD_BYTES_PER_SEC / 1024.0) as u16;
    p[8..10].copy_from_slice(&speed.to_be_bytes());
    p[14..16].copy_from_slice(&speed.to_be_bytes());
    p
}

/// Truncate a data-in buffer to the host's allocation length. A zero allocation
/// means "no data wanted" and returns an empty buffer.
fn truncate(mut buf: Vec<u8>, alloc: usize) -> CmdResult {
    if alloc < buf.len() {
        buf.truncate(alloc);
    }
    CmdResult::Data(buf)
}

/// Copy an ASCII string into a fixed field, space-padded and truncated.
fn write_ascii(field: &mut [u8], text: &str) {
    for slot in field.iter_mut() {
        *slot = b' ';
    }
    for (slot, b) in field.iter_mut().zip(text.bytes()) {
        *slot = b;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdimage::{CdImage, DATA_SECTOR, RAW_SECTOR};

    fn data_disc(sectors: u32) -> CdImage {
        let mut bytes = vec![0u8; sectors as usize * DATA_SECTOR];
        for s in 0..sectors as usize {
            bytes[s * DATA_SECTOR] = (s as u8).wrapping_add(0x40);
        }
        CdImage::from_iso(bytes).unwrap()
    }

    fn cdb(op: u8) -> [u8; 12] {
        let mut c = [0u8; 12];
        c[0] = op;
        c
    }

    fn data(result: CmdResult) -> Vec<u8> {
        match result {
            CmdResult::Data(d) => d,
            CmdResult::Error => panic!("expected data, got error"),
        }
    }

    #[test]
    fn read10_returns_the_right_sector() {
        let mut dev = AtapiDevice::new();
        dev.insert(data_disc(8));
        let mut c = cdb(0x28);
        c[5] = 3; // LBA 3
        c[8] = 1; // 1 sector
        let buf = data(dev.execute(&c));
        assert_eq!(buf.len(), DATA_SECTOR);
        assert_eq!(buf[0], 0x43); // 0x40 + 3
    }

    #[test]
    fn read10_past_end_is_an_error() {
        let mut dev = AtapiDevice::new();
        dev.insert(data_disc(4));
        let mut c = cdb(0x28);
        c[5] = 4; // LBA 4, past the 4-sector disc
        c[8] = 1;
        assert!(matches!(dev.execute(&c), CmdResult::Error));
        assert_eq!(dev.sense_key, sense_key::ILLEGAL_REQUEST);
    }

    #[test]
    fn read_capacity_reports_last_lba() {
        let mut dev = AtapiDevice::new();
        dev.insert(data_disc(10));
        let buf = data(dev.execute(&cdb(0x25)));
        let last = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let block = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        assert_eq!(last, 9); // last LBA of a 10-sector disc
        assert_eq!(block, DATA_SECTOR as u32);
    }

    #[test]
    fn read_toc_lists_tracks_and_leadout() {
        let mut dev = AtapiDevice::new();
        dev.insert(data_disc(6));
        let mut c = cdb(0x43);
        c[8] = 200; // allocation length, plenty
        let buf = data(dev.execute(&c));
        // header: data length (2), first track, last track.
        assert_eq!(buf[2], 1); // first track
        assert_eq!(buf[3], 1); // last track (one data track)
        // First descriptor starts at byte 4; the lead-out (0xAA) follows.
        assert_eq!(buf[4 + 2], 1); // track number of first descriptor
        assert_eq!(buf[4 + 8 + 2], 0xAA); // lead-out track number
    }

    #[test]
    fn inquiry_reports_cdrom_type() {
        let mut dev = AtapiDevice::new();
        dev.insert(data_disc(2));
        let mut c = cdb(0x12);
        c[4] = 36;
        let buf = data(dev.execute(&c));
        assert_eq!(buf[0], 0x05); // CD-ROM peripheral type
        assert_eq!(buf[1] & 0x80, 0x80); // removable
    }

    #[test]
    fn test_unit_ready_reports_no_medium_when_empty() {
        let mut dev = AtapiDevice::new();
        assert!(matches!(dev.execute(&cdb(0x00)), CmdResult::Error));
        assert_eq!(dev.sense_key, sense_key::NOT_READY);
    }

    #[test]
    fn first_ready_after_insert_is_unit_attention_then_clears() {
        let mut dev = AtapiDevice::new();
        dev.insert(data_disc(2));
        // First TEST UNIT READY reports the medium-changed unit attention.
        assert!(matches!(dev.execute(&cdb(0x00)), CmdResult::Error));
        assert_eq!(dev.sense_key, sense_key::UNIT_ATTENTION);
        // It clears, so the next is ready.
        assert!(matches!(dev.execute(&cdb(0x00)), CmdResult::Data(_)));
    }

    #[test]
    fn request_sense_returns_then_clears_latched_sense() {
        let mut dev = AtapiDevice::new();
        // No medium -> latch NOT READY.
        let _ = dev.execute(&cdb(0x00));
        let mut c = cdb(0x03);
        c[4] = 18;
        let buf = data(dev.execute(&c));
        assert_eq!(buf[2] & 0x0F, sense_key::NOT_READY);
        assert_eq!(buf[12], asc::NOT_READY_NO_MEDIUM.0);
        // A second REQUEST SENSE now reports NO SENSE.
        let buf2 = data(dev.execute(&c));
        assert_eq!(buf2[2] & 0x0F, sense_key::NO_SENSE);
    }

    fn audio_disc() -> CdImage {
        // 1 data sector, then 100 audio frames with a nonzero marker.
        let cue = "TRACK 01 MODE1/2048\nINDEX 01 00:00:00\n\
                   TRACK 02 AUDIO\nINDEX 01 00:00:01\n";
        let mut bin = vec![0u8; DATA_SECTOR + 100 * RAW_SECTOR];
        for b in bin[DATA_SECTOR..].iter_mut() {
            *b = 0x20;
        }
        CdImage::from_cue(cue, bin).unwrap()
    }

    #[test]
    fn play_audio_arms_playback_and_streams_frames() {
        let mut dev = AtapiDevice::new();
        dev.insert(audio_disc());
        // Play from LBA 1 (audio start) for 4 frames.
        let mut c = cdb(0x45);
        c[5] = 1; // LBA 1
        c[8] = 4; // 4 frames
        assert!(matches!(dev.execute(&c), CmdResult::Data(_)));
        assert!(dev.playback().playing);
        // The mixer pulls frames until the range is consumed.
        let mut frames = 0;
        while dev.next_audio_frame().is_some() {
            frames += 1;
            if frames > 10 {
                break;
            }
        }
        assert_eq!(frames, 4);
        assert!(!dev.playback().playing);
    }

    #[test]
    fn pause_then_resume_toggles_playing() {
        let mut dev = AtapiDevice::new();
        dev.insert(audio_disc());
        let mut c = cdb(0x45);
        c[5] = 1;
        c[8] = 50;
        let _ = dev.execute(&c);
        // Pause (byte 8 bit0 = 0).
        let _ = dev.execute(&cdb(0x4B));
        assert!(!dev.playback().playing && dev.playback().paused);
        // Resume (byte 8 bit0 = 1).
        let mut resume = cdb(0x4B);
        resume[8] = 0x01;
        let _ = dev.execute(&resume);
        assert!(dev.playback().playing);
        // Stop.
        let _ = dev.execute(&cdb(0x4E));
        assert!(!dev.playback().playing);
    }

    #[test]
    fn read_subchannel_reports_audio_status() {
        let mut dev = AtapiDevice::new();
        dev.insert(audio_disc());
        let mut play = cdb(0x45);
        play[5] = 1;
        play[8] = 10;
        let _ = dev.execute(&play);
        let mut c = cdb(0x42);
        c[2] = 0x40; // SubQ
        c[3] = 0x01; // current position format
        c[8] = 48;
        let buf = data(dev.execute(&c));
        assert_eq!(buf[1], 0x11); // audio play in progress
    }

    #[test]
    fn unknown_command_is_illegal_request() {
        let mut dev = AtapiDevice::new();
        dev.insert(data_disc(2));
        assert!(matches!(dev.execute(&cdb(0xFF)), CmdResult::Error));
        assert_eq!(dev.sense_key, sense_key::ILLEGAL_REQUEST);
        assert_eq!(dev.asc, asc::INVALID_COMMAND.0);
    }

    // Slice A: interrupt reason (C/D, I/O) + BSY const.

    #[test]
    fn arm_packet_sets_await_packet_reason() {
        let mut dev = AtapiDevice::new();
        dev.arm_packet();
        assert_eq!(dev.interrupt_reason(), interrupt_reason::AWAIT_PACKET);
        assert_eq!(interrupt_reason::AWAIT_PACKET, 0x01); // C/D=1, I/O=0
    }

    #[test]
    fn data_in_command_leaves_data_in_reason() {
        let mut dev = AtapiDevice::new();
        dev.insert(data_disc(8));
        let _ = dev.execute(&cdb(0x00)); // clear unit attention
        let mut c = cdb(0x28); // READ(10), returns a sector
        c[5] = 0;
        c[8] = 1;
        let _ = dev.execute(&c);
        assert_eq!(dev.interrupt_reason(), interrupt_reason::DATA_IN);
        assert_eq!(interrupt_reason::DATA_IN, 0x02); // C/D=0, I/O=1
    }

    #[test]
    fn no_data_command_lands_on_command_complete() {
        let mut dev = AtapiDevice::new();
        dev.insert(data_disc(2));
        let _ = dev.execute(&cdb(0x00)); // clear unit attention
        let _ = dev.execute(&cdb(0x00)); // TEST UNIT READY, no data
        assert_eq!(dev.interrupt_reason(), interrupt_reason::COMMAND_COMPLETE);
        assert_eq!(interrupt_reason::COMMAND_COMPLETE, 0x03); // C/D=1, I/O=1
    }

    #[test]
    fn error_lands_on_command_complete_reason() {
        let mut dev = AtapiDevice::new();
        // No medium: TEST UNIT READY errors.
        assert!(matches!(dev.execute(&cdb(0x00)), CmdResult::Error));
        assert_eq!(dev.interrupt_reason(), interrupt_reason::COMMAND_COMPLETE);
    }

    #[test]
    fn bsy_const_is_the_high_bit() {
        assert_eq!(BSY, 0x80);
    }

    // Slice B: START STOP UNIT (0x1B) + PREVENT/ALLOW (0x1E).

    #[test]
    fn prevent_allow_latches_the_flag() {
        let mut dev = AtapiDevice::new();
        dev.insert(data_disc(2));
        let mut c = cdb(0x1E);
        c[4] = 0x01; // prevent
        assert!(matches!(dev.execute(&c), CmdResult::Data(_)));
        assert!(dev.removal_prevented());
        c[4] = 0x00; // allow
        let _ = dev.execute(&c);
        assert!(!dev.removal_prevented());
    }

    #[test]
    fn prevent_allow_not_ready_when_empty() {
        let mut dev = AtapiDevice::new();
        assert!(matches!(dev.execute(&cdb(0x1E)), CmdResult::Error));
        assert_eq!(dev.sense_key, sense_key::NOT_READY);
    }

    #[test]
    fn start_stop_eject_clears_the_disc() {
        let mut dev = AtapiDevice::new();
        dev.insert(data_disc(2));
        let mut c = cdb(0x1B);
        c[4] = 0x02; // LoEj=1, Start=0: eject
        assert!(matches!(dev.execute(&c), CmdResult::Data(_)));
        assert!(!dev.is_loaded());
    }

    #[test]
    fn start_stop_eject_blocked_when_removal_prevented() {
        let mut dev = AtapiDevice::new();
        dev.insert(data_disc(2));
        let mut prevent = cdb(0x1E);
        prevent[4] = 0x01;
        let _ = dev.execute(&prevent);
        let mut eject = cdb(0x1B);
        eject[4] = 0x02; // eject
        assert!(matches!(dev.execute(&eject), CmdResult::Error));
        assert_eq!(dev.sense_key, sense_key::NOT_READY);
        assert_eq!((dev.asc, dev.ascq), asc::MEDIUM_REMOVAL_PREVENTED);
        assert!(dev.is_loaded()); // still mounted
    }

    #[test]
    fn start_stop_flips_started_flag() {
        let mut dev = AtapiDevice::new();
        dev.insert(data_disc(2));
        let mut start = cdb(0x1B);
        start[4] = 0x01; // Start=1
        let _ = dev.execute(&start);
        assert!(dev.started());
        let mut stop = cdb(0x1B);
        stop[4] = 0x00; // Start=0, no eject
        let _ = dev.execute(&stop);
        assert!(!dev.started());
        assert!(dev.is_loaded()); // a plain stop does not eject
    }

    // Slice C: SEEK (0x2B).

    #[test]
    fn seek_in_range_succeeds_with_no_data() {
        let mut dev = AtapiDevice::new();
        dev.insert(data_disc(8));
        let mut c = cdb(0x2B);
        c[5] = 4; // LBA 4, in range
        let buf = data(dev.execute(&c));
        assert!(buf.is_empty());
    }

    #[test]
    fn seek_past_end_is_lba_out_of_range() {
        let mut dev = AtapiDevice::new();
        dev.insert(data_disc(4));
        let mut c = cdb(0x2B);
        c[5] = 4; // LBA 4 on a 4-sector disc
        assert!(matches!(dev.execute(&c), CmdResult::Error));
        assert_eq!(dev.sense_key, sense_key::ILLEGAL_REQUEST);
        assert_eq!(dev.asc, asc::LBA_OUT_OF_RANGE.0);
    }

    #[test]
    fn seek_not_ready_when_empty() {
        let mut dev = AtapiDevice::new();
        assert!(matches!(dev.execute(&cdb(0x2B)), CmdResult::Error));
        assert_eq!(dev.sense_key, sense_key::NOT_READY);
    }

    // Slice D: REQUEST SENSE info field, INQUIRY EVPD, READ HEADER.

    #[test]
    fn request_sense_carries_failing_lba_with_valid_bit() {
        let mut dev = AtapiDevice::new();
        dev.insert(data_disc(4));
        let _ = dev.execute(&cdb(0x00)); // clear unit attention
        // SEEK past the end latches the failing LBA.
        let mut seek = cdb(0x2B);
        seek[5] = 7;
        assert!(matches!(dev.execute(&seek), CmdResult::Error));
        let mut c = cdb(0x03);
        c[4] = 18;
        let buf = data(dev.execute(&c));
        assert_eq!(buf[0] & 0x80, 0x80); // VALID bit
        let info = u32::from_be_bytes([buf[3], buf[4], buf[5], buf[6]]);
        assert_eq!(info, 7);
        // A clean error (no LBA) leaves the VALID bit clear next time.
        let _ = dev.execute(&cdb(0x00)); // ready, no latch
        let buf2 = data(dev.execute(&c));
        assert_eq!(buf2[0] & 0x80, 0x00);
    }

    #[test]
    fn inquiry_evpd_supported_pages() {
        let mut dev = AtapiDevice::new();
        dev.insert(data_disc(2));
        let mut c = cdb(0x12);
        c[1] = 0x01; // EVPD
        c[2] = 0x00; // supported pages
        c[4] = 32;
        let buf = data(dev.execute(&c));
        assert_eq!(buf[1], 0x00); // page code
        let len = buf[3] as usize;
        let pages = &buf[4..4 + len];
        assert!(pages.contains(&0x00));
        assert!(pages.contains(&0x80));
        assert!(pages.contains(&0x83));
    }

    #[test]
    fn inquiry_evpd_unit_serial() {
        let mut dev = AtapiDevice::new();
        dev.insert(data_disc(2));
        let mut c = cdb(0x12);
        c[1] = 0x01; // EVPD
        c[2] = 0x80; // unit serial
        c[4] = 64;
        let buf = data(dev.execute(&c));
        assert_eq!(buf[1], 0x80);
        let len = buf[3] as usize;
        let serial = &buf[4..4 + len];
        assert_eq!(serial, b"IZARRA-CD-0001");
    }

    #[test]
    fn inquiry_evpd_unknown_page_is_illegal() {
        let mut dev = AtapiDevice::new();
        dev.insert(data_disc(2));
        let mut c = cdb(0x12);
        c[1] = 0x01; // EVPD
        c[2] = 0x55; // unsupported page
        c[4] = 32;
        assert!(matches!(dev.execute(&c), CmdResult::Error));
        assert_eq!(dev.sense_key, sense_key::ILLEGAL_REQUEST);
    }

    #[test]
    fn inquiry_nonzero_page_without_evpd_is_illegal() {
        let mut dev = AtapiDevice::new();
        dev.insert(data_disc(2));
        let mut c = cdb(0x12);
        c[2] = 0x80; // page code without EVPD
        c[4] = 36;
        assert!(matches!(dev.execute(&c), CmdResult::Error));
        assert_eq!(dev.sense_key, sense_key::ILLEGAL_REQUEST);
    }

    #[test]
    fn read_header_reports_data_mode_and_address() {
        let mut dev = AtapiDevice::new();
        dev.insert(data_disc(8));
        let _ = dev.execute(&cdb(0x00)); // clear unit attention
        let mut c = cdb(0x44);
        c[5] = 3; // LBA 3
        c[8] = 8; // allocation
        let buf = data(dev.execute(&c));
        assert_eq!(buf.len(), 8);
        assert_eq!(buf[0], 0x01); // MODE1 data
        let lba = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        assert_eq!(lba, 3);
    }

    #[test]
    fn read_header_msf_address() {
        let mut dev = AtapiDevice::new();
        dev.insert(data_disc(8));
        let _ = dev.execute(&cdb(0x00));
        let mut c = cdb(0x44);
        c[1] = 0x02; // MSF
        c[5] = 0; // LBA 0 -> 00:02:00 with lead-in
        c[8] = 8;
        let buf = data(dev.execute(&c));
        // Byte 4 reserved (0), then M, S, F.
        assert_eq!((buf[5], buf[6], buf[7]), (0, 2, 0));
    }

    #[test]
    fn read_header_past_end_is_out_of_range() {
        let mut dev = AtapiDevice::new();
        dev.insert(data_disc(4));
        let _ = dev.execute(&cdb(0x00));
        let mut c = cdb(0x44);
        c[5] = 9;
        c[8] = 8;
        assert!(matches!(dev.execute(&c), CmdResult::Error));
        assert_eq!(dev.sense_key, sense_key::ILLEGAL_REQUEST);
        assert_eq!(dev.asc, asc::LBA_OUT_OF_RANGE.0);
    }
}
