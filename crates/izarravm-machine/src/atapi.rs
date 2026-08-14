// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

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
//! HEADER, READ TOC/PMA/ATIP, READ(10), READ(12), READ CD, MODE SENSE(10),
//! MODE SELECT(10) (pages 0x0E audio control, 0x2A capabilities), and the
//! CD-Audio set PLAY AUDIO(10), PLAY AUDIO MSF, PAUSE/RESUME, STOP, READ
//! SUB-CHANNEL. IDENTIFY PACKET DEVICE is answered by the register file directly
//! since it is an ATA command, not a packet command.

use crate::cdimage::{CdImage, DATA_SECTOR, FRAMES_PER_SEC, lba_to_msf, msf_to_lba};
use izarravm_core::{CanonicalFieldWriter, CanonicalStateError};

/// 12x CD-ROM transfer ceiling reported by MODE SENSE: about 1800 KB/s.
pub const CD_BYTES_PER_SEC: f64 = 1_800.0 * 1024.0;

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
// Limit: the synchronous model never opens a real busy window, so ide.rs has
// no place to publish this bit; it stays defined for fidelity but uncalled.
#[allow(dead_code)]
pub const BSY: u8 = 0x80;

/// The ATAPI Interrupt Reason register is the sector-count register, reinterpreted
/// during a packet transfer: bit0 = C/D (1 = command/CDB phase, 0 = data phase),
/// bit1 = I/O (1 = transfer to host, 0 = transfer from host). The four packet
/// phases map to four byte values.
pub mod interrupt_reason {
    /// Data-out armed (host-to-device): C/D=0, I/O=0.
    pub const DATA_OUT: u8 = 0x00;
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
    mixer_lba: Option<u32>,
    /// Changes whenever playback begins a new range or is explicitly stopped,
    /// and whenever media changes. The machine mixer uses this to reset its
    /// intra-sector sample cursor without treating pause/resume as a restart.
    playback_epoch: u64,
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
    /// START STOP UNIT spin state. The IDE transport uses it to schedule spin-up
    /// before the next seek or read.
    started: bool,
    /// The interrupt-reason byte (C/D, I/O) for the current packet phase. The IDE
    /// register file reads this through [`Self::interrupt_reason`] and publishes it
    /// on the sector-count port; this is the device-side source of truth.
    interrupt_reason: u8,
    /// CD audio control (mode page 0x0E) output-port volumes, ports 0 and 1.
    /// MODE SELECT(10) stores them; MODE SENSE(10) reports them. 0xFF is full
    /// volume, the reset value a drive powers up with.
    audio_volume: [u8; 2],
}

impl AtapiDevice {
    pub fn new() -> Self {
        Self {
            // A drive powers up with both audio output ports at full volume.
            audio_volume: [0xFF, 0xFF],
            ..Self::default()
        }
    }

    /// Current CD-audio output volume (mode page 0x0E ports 0 and 1), 0xFF full.
    pub fn audio_volume(&self) -> [u8; 2] {
        self.audio_volume
    }

    /// Mount a CD image, raising the medium-changed condition the next command
    /// reports as UNIT ATTENTION.
    pub fn insert(&mut self, image: CdImage) {
        self.image = Some(image);
        self.media_changed = true;
        self.started = false;
        self.play = Playback::default();
        self.mixer_lba = None;
        self.bump_playback_epoch();
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
        self.started = false;
        self.prevent_removal = false;
        self.play = Playback::default();
        self.mixer_lba = None;
        self.bump_playback_epoch();
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

    pub(crate) fn audio_capable(&self) -> bool {
        self.image
            .as_ref()
            .is_some_and(|image| image.tracks().iter().any(|track| track.mode.is_audio()))
    }

    pub(crate) fn playback_epoch(&self) -> u64 {
        self.playback_epoch
    }

    /// Start or resume playback from the drive's front panel. This changes only
    /// playback state and never passes through the packet-command interpreter.
    pub(crate) fn front_panel_play(&mut self) {
        if self.play.paused {
            self.play.paused = false;
            self.play.playing = self.play.current_lba < self.play.end_lba;
            return;
        }
        let Some((start, end)) = self.image.as_ref().and_then(|image| {
            image
                .tracks()
                .iter()
                .find(|track| track.mode.is_audio())
                .map(|track| (track.start_lba, image.total_sectors()))
        }) else {
            return;
        };
        self.set_play_range(start, end);
    }

    /// Stop playback from the drive's front panel without executing a CDB.
    pub(crate) fn front_panel_stop(&mut self) {
        self.stop_playback();
    }

    fn bump_playback_epoch(&mut self) {
        self.playback_epoch = self.playback_epoch.wrapping_add(1);
    }

    fn set_play_range(&mut self, start: u32, end: u32) {
        self.play = Playback {
            playing: start < end,
            paused: false,
            current_lba: start,
            end_lba: end,
        };
        self.mixer_lba = (start < end).then_some(start);
        self.bump_playback_epoch();
    }

    fn stop_playback(&mut self) {
        self.play.stop();
        self.mixer_lba = None;
        self.bump_playback_epoch();
    }

    fn interrupt_audio_for_head_movement(&mut self) {
        if self.play.playing || self.play.paused {
            self.stop_playback();
        }
    }

    #[cfg(test)]
    pub(crate) fn non_playback_state_snapshot(&self) -> String {
        format!(
            "{}:{}:{}:{:?}:{}:{}:{}:{}:{:?}",
            self.sense_key,
            self.asc,
            self.ascq,
            self.sense_information,
            self.media_changed,
            self.prevent_removal,
            self.started,
            self.interrupt_reason,
            self.audio_volume
        )
    }

    // Limit: these two tray/spin queries feed ide.rs (status port), which does
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

    /// Spin up on demand for a media operation. Returns true only when the IDE
    /// transport must schedule the spin-up delay.
    pub(crate) fn ensure_started(&mut self) -> bool {
        let was_stopped = !self.started;
        self.started = true;
        was_stopped
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

    /// Mark the device as ready to receive a packet command's parameter list.
    pub fn arm_data_out(&mut self) {
        self.interrupt_reason = interrupt_reason::DATA_OUT;
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
    #[cfg(test)]
    pub fn peek_audio_frame(&self) -> Option<[u8; crate::cdimage::RAW_SECTOR]> {
        if !self.play.playing || self.play.current_lba >= self.play.end_lba {
            return None;
        }
        let lba = self.play.current_lba;
        let frame = self.image.as_ref()?.read_audio_frame(lba);
        Some(frame.unwrap_or([0u8; crate::cdimage::RAW_SECTOR]))
    }

    pub(crate) fn peek_mixer_audio_frame(&self) -> Option<[u8; crate::cdimage::RAW_SECTOR]> {
        if !self.play.playing {
            return None;
        }
        let lba = self.mixer_lba?;
        if lba >= self.play.end_lba {
            return None;
        }
        Some(
            self.image
                .as_ref()?
                .read_audio_frame(lba)
                .unwrap_or([0u8; crate::cdimage::RAW_SECTOR]),
        )
    }

    pub(crate) fn mixer_audio_active(&self) -> bool {
        self.play.playing && self.mixer_lba.is_some()
    }

    pub(crate) fn advance_mixer_audio(&mut self, frames: u32) {
        let Some(lba) = self.mixer_lba else {
            return;
        };
        let next = lba.saturating_add(frames);
        self.mixer_lba = (next < self.play.end_lba).then_some(next);
        // Warm the next track before the head reaches it. Without this, a play
        // spanning the disc clips the opening of every track after the first,
        // because a decode only starts when a frame is asked for.
        if let Some(image) = self.image.as_ref() {
            image.warm_upcoming(next);
        }
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
            0x55 => self.mode_select10(cdb),
            0x5A => self.mode_sense10(cdb),
            0xA8 => self.read12(cdb),
            0xBD => self.mechanism_status(cdb),
            0xBE => self.read_cd(cdb),
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
    /// locking the tray against an eject. Locking an empty drive reports NOT READY,
    /// but an unlock always succeeds so reset software can clear a stale latch.
    fn prevent_allow_removal(&mut self, cdb: &[u8; 12]) -> CmdResult {
        let prevent = cdb[4] & 0x01 != 0;
        if prevent && self.image.is_none() {
            return self.fail(sense_key::NOT_READY, asc::NOT_READY_NO_MEDIUM);
        }
        self.prevent_removal = prevent;
        CmdResult::Data(Vec::new())
    }

    /// START STOP UNIT (0x1B). Byte 4: LoEj (bit 1) requests a tray eject, Start
    /// (bit 0) spins the unit up or down. LoEj with Start clear ejects, but only
    /// when removal is not prevented; otherwise CHECK CONDITION with medium-removal
    /// prevented. Limit: the GUI owns the host file, so an eject-on-command only
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

    /// READ CD (0xBE). Byte 1 bits 7-5 are the expected sector type, bytes 2-5 the
    /// starting LBA, bytes 6-8 the transfer length in sectors (24-bit big-endian),
    /// byte 9 the main-channel selection. The model serves the 2048-byte user data
    /// of Mode 1 / Mode 2 Form 1 sectors, behaving like READ(10) when the host asks
    /// for user data. The expected sector type is validated against the track at
    /// the LBA: type 1 (CD-DA) over a data track, or a data type over an audio
    /// track, is an illegal field.
    // Limit: only the 2048-byte user-data main-channel field is returned. The
    // sync header, sub-header, and C2/EDC/ECC selections (byte 9 other bits) are
    // not synthesized; a guest that asks for raw 2352-byte frames gets user data.
    fn read_cd(&mut self, cdb: &[u8; 12]) -> CmdResult {
        // Expected sector type is byte 1 bits 7-5 (bit 4 is DAP, bit 0 RELADR).
        let expected_type = (cdb[1] >> 5) & 0x07;
        let lba = u32::from_be_bytes([cdb[2], cdb[3], cdb[4], cdb[5]]);
        let count = u32::from_be_bytes([0, cdb[6], cdb[7], cdb[8]]);
        // The main-channel selection (byte 9) must request the user data field
        // (bit 4); a request for no main-channel data returns nothing.
        let want_user_data = cdb[9] & 0x10 != 0;
        let Some(image) = &self.image else {
            return self.fail(sense_key::NOT_READY, asc::NOT_READY_NO_MEDIUM);
        };
        if count == 0 || !want_user_data {
            return CmdResult::Data(Vec::new());
        }
        let end = lba.saturating_add(count);
        if end > image.total_sectors() {
            return self.fail_at_lba(sense_key::ILLEGAL_REQUEST, asc::LBA_OUT_OF_RANGE, lba);
        }
        // Validate the expected sector type against the track. Type 0 (any) skips
        // the check; type 1 is CD-DA (audio); types 2-5 are the data modes.
        let is_audio = image.track_at_lba(lba).map(|t| t.mode.is_audio());
        match (expected_type, is_audio) {
            (1, Some(false)) | (2..=5, Some(true)) => {
                return self.fail(sense_key::ILLEGAL_REQUEST, asc::INVALID_FIELD_IN_CDB);
            }
            _ => {}
        }
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
        self.interrupt_audio_for_head_movement();
        CmdResult::Data(Vec::new())
    }

    /// READ HEADER (0x44). Bytes 2-5 hold the LBA; byte 1 bit 1 selects an MSF
    /// address over LBA. Returns a 4-byte header followed by the 4-byte address:
    /// the data-mode byte (0x01 for a MODE1 data sector, 0x00 for an audio or hole)
    /// then three reserved bytes, then the requested address. Limit: the model
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
        self.interrupt_audio_for_head_movement();
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

    /// MODE SENSE(10) (0x5A). Byte 2 bits 5-0 are the page code, bits 7-6 the
    /// page-control field (0 current, 1 changeable, 2 default, 3 saved); the model
    /// reports the same values for every control. Returns an 8-byte header then the
    /// requested page(s). Page 0x2A is CD/DVD capabilities, 0x0E CD audio control,
    /// and 0x3F asks for every supported page. An unsupported page is an illegal
    /// field.
    fn mode_sense10(&mut self, cdb: &[u8; 12]) -> CmdResult {
        let page = cdb[2] & 0x3F;
        let alloc = u16::from_be_bytes([cdb[7], cdb[8]]) as usize;
        let mut page_bytes = Vec::new();
        match page {
            0x2A => page_bytes.extend_from_slice(&caps_page_2a()),
            0x0E => page_bytes.extend_from_slice(&audio_page_0e(self.audio_volume)),
            0x3F => {
                // All supported pages, ascending page-code order.
                page_bytes.extend_from_slice(&audio_page_0e(self.audio_volume));
                page_bytes.extend_from_slice(&caps_page_2a());
            }
            _ => return self.fail(sense_key::ILLEGAL_REQUEST, asc::INVALID_FIELD_IN_CDB),
        }
        let mut buf = vec![0u8; 8];
        let total = (page_bytes.len() + 6) as u16; // mode data length excludes its own 2 bytes
        buf[0..2].copy_from_slice(&total.to_be_bytes());
        buf[2] = 0x05; // medium type: CD-ROM
        buf.extend_from_slice(&page_bytes);
        truncate(buf, alloc)
    }

    /// MODE SELECT(10) (0x55). The CDB names a parameter-list length (bytes 7-8);
    /// the parameter list itself (header plus mode pages) arrives in a data-out
    /// phase. This call acknowledges the command; the page list is applied through
    /// [`Self::mode_select_data`].
    fn mode_select10(&mut self, _cdb: &[u8; 12]) -> CmdResult {
        CmdResult::Data(Vec::new())
    }

    /// Apply a MODE SELECT(10) parameter list (the header plus mode pages the host
    /// wrote in the data-out phase). Walks the pages and stores the ones the model
    /// tracks (page 0x0E CD audio control: the two output-port volumes). Unknown
    /// pages are skipped, the way a forgiving drive treats vendor pages. Returns
    /// Error with latched sense on a malformed list.
    pub fn mode_select_data(&mut self, params: &[u8]) -> CmdResult {
        self.interrupt_reason = interrupt_reason::COMMAND_COMPLETE;
        // 8-byte MODE SELECT(10) parameter header, then a block-descriptor area
        // whose length is bytes 6-7, then the mode pages.
        if params.len() < 8 {
            return self.fail(sense_key::ILLEGAL_REQUEST, asc::INVALID_FIELD_IN_CDB);
        }
        let bd_len = u16::from_be_bytes([params[6], params[7]]) as usize;
        let mut idx = 8 + bd_len;
        while idx + 2 <= params.len() {
            let page_code = params[idx] & 0x3F;
            let page_len = params[idx + 1] as usize;
            let body_start = idx + 2;
            let body_end = body_start + page_len;
            if body_end > params.len() {
                return self.fail(sense_key::ILLEGAL_REQUEST, asc::INVALID_FIELD_IN_CDB);
            }
            if page_code == 0x0E {
                // CD audio control page. Output port 0 volume is byte 9, output
                // port 1 volume is byte 11 of the page (offsets 7 and 9 past the
                // 2-byte page header).
                let body = &params[body_start..body_end];
                if body.len() >= 8 {
                    self.audio_volume[0] = body[7];
                }
                if body.len() >= 10 {
                    self.audio_volume[1] = body[9];
                }
            }
            idx = body_end;
        }
        CmdResult::Data(Vec::new())
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
        let Some(image) = self.image.as_ref() else {
            return self.fail(sense_key::NOT_READY, asc::NOT_READY_NO_MEDIUM);
        };
        if end < start || end > image.total_sectors() {
            return self.fail(sense_key::ILLEGAL_REQUEST, asc::INVALID_FIELD_IN_CDB);
        }
        let mut lba = start;
        while lba < end {
            let Some(track) = image.track_at_lba(lba) else {
                return self.fail(sense_key::ILLEGAL_REQUEST, asc::INVALID_FIELD_IN_CDB);
            };
            if !track.mode.is_audio() {
                return self.fail(sense_key::ILLEGAL_REQUEST, asc::INVALID_FIELD_IN_CDB);
            }
            lba = track.end_lba().min(end);
        }
        self.set_play_range(start, end);
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
        self.stop_playback();
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
            let track = self.image.as_ref().and_then(|image| {
                image.track_at_lba(lba).or_else(|| {
                    image
                        .tracks()
                        .iter()
                        .rev()
                        .find(|track| track.start_lba <= lba)
                })
            });
            let mut block = vec![0u8; 12];
            block[0] = 0x01; // sub-channel data format code
            block[1] = track_control(track.is_some_and(|track| track.mode.is_audio()));
            block[2] = track.map_or(1, |track| track.number);
            block[3] = 1; // index
            put_addr(&mut block[4..8], lba, msf); // absolute address
            let relative_lba = track.map_or(lba, |track| lba.saturating_sub(track.start_lba));
            put_relative_addr(&mut block[8..12], relative_lba, msf);
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

/// Write a track-relative address. Unlike an absolute MSF address, this has no
/// 150-frame lead-in offset.
fn put_relative_addr(out: &mut [u8], frames: u32, msf: bool) {
    if msf {
        out[0] = 0;
        out[1] = (frames / (FRAMES_PER_SEC * 60)) as u8;
        out[2] = ((frames / FRAMES_PER_SEC) % 60) as u8;
        out[3] = (frames % FRAMES_PER_SEC) as u8;
    } else {
        out.copy_from_slice(&frames.to_be_bytes());
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

/// The CD-ROM Capabilities and Mechanical Status page (0x2A) per MMC. The read
/// speeds let a driver size the drive at 12x; the format bytes advertise what the
/// drive can read and play.
fn caps_page_2a() -> Vec<u8> {
    let mut p = vec![0u8; 22];
    p[0] = 0x2A; // page code
    p[1] = 20; // page length (20 bytes after this header byte)
    // Read-format support (byte 2): bit0 CD-R, bit1 CD-RW, bit2 method-2 (the
    // drive can read written CD-R/RW media).
    p[2] = 0x07;
    // No write support (byte 3 stays 0: this is a read-only drive).
    // Mechanism/format support (byte 4): bit0 audio play, bit4 Mode 2 Form 1,
    // bit5 Mode 2 Form 2, bit6 multisession.
    p[4] = 0x71;
    // CD-DA capabilities (byte 5): bit0 CD-DA commands supported, bit1 CD-DA
    // stream is accurate.
    p[5] = 0x03;
    // Max read speed in KB/s (bytes 8-9) and current read speed (bytes 14-15).
    let speed = (CD_BYTES_PER_SEC / 1024.0) as u16;
    p[8..10].copy_from_slice(&speed.to_be_bytes());
    p[14..16].copy_from_slice(&speed.to_be_bytes());
    p
}

/// The CD audio control mode page (0x0E) per MMC. Carries the two CD-audio
/// output-port selections and volumes; the model reports the stored volumes.
/// Output port 0 is byte 8 (channel select) and byte 9 (volume); output port 1
/// is byte 10/11. Channels are wired stereo: port 0 = left, port 1 = right.
fn audio_page_0e(volume: [u8; 2]) -> Vec<u8> {
    let mut p = vec![0u8; 16];
    p[0] = 0x0E; // page code
    p[1] = 14; // page length (14 bytes after this header byte)
    p[8] = 0x01; // output port 0 -> channel 0 (left)
    p[9] = volume[0]; // output port 0 volume
    p[10] = 0x02; // output port 1 -> channel 1 (right)
    p[11] = volume[1]; // output port 1 volume
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

/// Borrowed ATAPI device state for canonical comparison: sense latches,
/// removal/spin/attention flags, the interrupt-reason byte, MODE SELECT
/// volumes, and the guest-deterministic playback record. The host-mixer
/// streaming cursor (`mixer_lba`) and its discontinuity signal
/// (`playback_epoch`) advance with host audio drain, not guest time, and are
/// excluded as host-presentation state; guest-visible position reporting uses
/// `play.current_lba`, which the fixed timeline advances.
pub(crate) struct CanonicalAtapiDevice<'a> {
    device: &'a AtapiDevice,
}

impl AtapiDevice {
    pub(crate) fn canonical_projection(&self) -> CanonicalAtapiDevice<'_> {
        CanonicalAtapiDevice { device: self }
    }
}

impl CanonicalAtapiDevice<'_> {
    pub(crate) fn write_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        out.write_u8(self.device.sense_key)?;
        out.write_u8(self.device.asc)?;
        out.write_u8(self.device.ascq)?;
        out.write_bool(self.device.sense_information.is_some())?;
        out.write_u32(self.device.sense_information.unwrap_or(0))?;
        out.write_bool(self.device.media_changed)?;
        out.write_bool(self.device.prevent_removal)?;
        out.write_bool(self.device.started)?;
        out.write_u8(self.device.interrupt_reason)?;
        out.write_u8(self.device.audio_volume[0])?;
        out.write_u8(self.device.audio_volume[1])?;
        out.write_bool(self.device.play.playing)?;
        out.write_bool(self.device.play.paused)?;
        out.write_u32(self.device.play.current_lba)?;
        out.write_u32(self.device.play.end_lba)
    }
}

#[cfg(test)]
#[path = "atapi_test.rs"]
mod tests;
