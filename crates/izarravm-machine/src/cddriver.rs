// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The IzarraCD device-request executor: the host-side port of TOKACD.SYS.
//!
//! With the CD consolidation the BIOS is the CD driver, so the request
//! packets that used to reach TOKACD's Strategy/Interrupt entries reach this
//! module instead — through INT 2Fh AX=1510h, or through the ROM device
//! header's stubs and the Lotura doorbell. The dispatcher, the length/unit
//! gates, every IOCTL control block, and the audio state machine are a
//! faithful transcription of TOKACD.SYS's source (formerly
//! `crates/izarravm-firmware/roms/dos/tokacd.asm`; removed with slice CD-3,
//! complete in git history);
//! where TOKACD issued an ATAPI packet over the wire, this port calls
//! `AtapiDevice::execute` with the same CDB, so the device-visible behavior
//! (unit-attention retry, sense mapping, audio interruption on reads and
//! seeks) stays what the wire produced.

use super::*;

// Request-header offsets (RBIL table 02597; tokacd.asm RH_*).
const RH_LENGTH: u32 = 0;
const RH_UNIT: u32 = 1;
const RH_COMMAND: u32 = 2;
const RH_STATUS: u32 = 3;
const RH_DATA: u32 = 13;
const RH_IOCTL_PTR: u32 = 14;
const RH_IOCTL_COUNT: u32 = 18;
const RH_READ_ADDR: u32 = 13;
const RH_READ_BUFFER: u32 = 14;
const RH_READ_COUNT: u32 = 18;
const RH_READ_START: u32 = 20;
const RH_READ_MODE: u32 = 24;
const RH_PLAY_ADDR: u32 = 13;
const RH_PLAY_START: u32 = 14;
const RH_PLAY_COUNT: u32 = 18;

const ST_DONE: u16 = 0x0100;
const ST_BUSY: u16 = 0x0200;
const ST_ERROR: u16 = 0x8000;

const ERR_UNIT: u16 = 0x01;
const ERR_NOT_READY: u16 = 0x02;
const ERR_COMMAND: u16 = 0x03;
const ERR_LENGTH: u16 = 0x05;
const ERR_SECTOR: u16 = 0x08;
const ERR_READ: u16 = 0x0B;
const ERR_GENERAL: u16 = 0x0C;
const ERR_CHANGED: u16 = 0x0F;

const MEDIA_UNKNOWN: u8 = 0;
const MEDIA_PRESENT: u8 = 1;
const MEDIA_ABSENT: u8 = 2;

/// The driver's resident state, exactly the variables TOKACD kept beside its
/// device header. Addressing forms: `last_start`/`last_end` hold an HSG LBA
/// when the play that set them was HSG, or a packed `00:MM:SS:FF` dword when
/// it was Red Book (`last_addr_mode`), because IOCTL Audio Status reports
/// the range in the caller's own representation.
#[derive(Debug, Clone)]
pub(crate) struct CdDriverState {
    media_state: u8,
    media_latch: bool,
    door_open: bool,
    door_locked: bool,
    audio_playing: bool,
    audio_paused: bool,
    last_addr_mode: u8,
    last_start: u32,
    last_end: u32,
    head_lba: u32,
    audio_channels: [u8; 8],
}

impl Default for CdDriverState {
    fn default() -> Self {
        CdDriverState {
            media_state: MEDIA_UNKNOWN,
            media_latch: false,
            door_open: false,
            door_locked: false,
            audio_playing: false,
            audio_paused: false,
            last_addr_mode: 0,
            last_start: 0,
            last_end: 0,
            head_lba: 0,
            audio_channels: [0, 0xFF, 1, 0xFF, 2, 0xFF, 3, 0xFF],
        }
    }
}

impl CdDriverState {
    fn clear_audio(&mut self) {
        self.audio_playing = false;
        self.audio_paused = false;
        self.last_addr_mode = 0;
        self.last_start = 0;
        self.last_end = 0;
    }
}

/// Packed Red Book `00:MM:SS:FF` to LBA, the way TOKACD's converter worked:
/// the high byte is MASKED (not validated), out-of-range seconds/frames are
/// accepted numerically, and only an address inside the 150-frame lead-in
/// fails. Syntax validation (frames < 75, seconds < 60, high byte zero) was
/// a separate check TOKACD performed at the PLAY site only —
/// `play_msf_syntax_ok` below carries that part.
fn packed_msf_to_lba(packed: u32) -> Option<u32> {
    let frame = packed as u8;
    let second = (packed >> 8) as u8;
    let minute = (packed >> 16) as u8;
    let frames = (u32::from(minute) * 60 + u32::from(second)) * 75 + u32::from(frame);
    frames.checked_sub(150)
}

/// The PLAY request's Red Book syntax gate (tokacd.asm request_play):
/// frames < 75, seconds < 60, and a zero high byte.
fn play_msf_syntax_ok(packed: u32) -> bool {
    packed & 0xFF00_0000 == 0 && (packed as u8) < 75 && ((packed >> 8) as u8) < 60
}

fn lba_to_packed_msf(lba: u32) -> u32 {
    let frames = lba + 150;
    let frame = frames % 75;
    let second = (frames / 75) % 60;
    let minute = frames / (75 * 60);
    frame | (second << 8) | (minute << 16)
}

impl Machine {
    /// Execute one CD device-driver request whose header begins at guest
    /// linear `header`. Both the header and every pointer inside it (the
    /// IOCTL control block, the read buffer — real-mode far pointers, offset
    /// word then segment word) belong to the caller and are accessed through
    /// its mapping.
    pub(crate) fn cd_device_request(&mut self, header: u32) {
        let ax = self.cd_dispatch(header);
        // TOKACD refreshed the cached audio flags from the subchannel before
        // composing the status word, so a range that finished playing drops
        // the busy bit on the next request.
        if self.cd_driver.audio_playing {
            let _ = self.cd_read_subchannel();
        }
        let mut status = ax | ST_DONE;
        if self.cd_driver.audio_playing {
            status |= ST_BUSY;
        }
        self.write_guest_linear_block(header + RH_STATUS, &status.to_le_bytes());
    }

    fn cd_dispatch(&mut self, header: u32) -> u16 {
        self.write_guest_linear_block(header + RH_STATUS, &0u16.to_le_bytes());
        let command = self.cd_req_u8(header, RH_COMMAND);
        if command == 0 {
            if self.cd_req_u8(header, RH_LENGTH) < 23 {
                return ST_ERROR | ERR_LENGTH;
            }
            return self.cd_request_init(header);
        }
        if self.cd_req_u8(header, RH_UNIT) != 0 {
            return ST_ERROR | ERR_UNIT;
        }
        // One length gate for every application-issued command, accepting the
        // 13-byte fixed header (Quake declares 13 and fills the fields past
        // it; see tokacd.asm's dispatch comment).
        if self.cd_req_u8(header, RH_LENGTH) < 13 {
            return ST_ERROR | ERR_LENGTH;
        }
        match command {
            3 => self.cd_ioctl_input(header),
            7 => 0,
            12 => self.cd_ioctl_output(header),
            13 | 14 => 0, // OPEN / CLOSE bookkeeping only
            128 => self.cd_request_read_long(header),
            130 | 131 => self.cd_request_seek(header),
            132 => self.cd_request_play(header),
            133 => self.cd_request_stop(),
            136 => self.cd_request_resume(),
            _ => ST_ERROR | ERR_COMMAND,
        }
    }

    fn cd_request_init(&mut self, header: u32) -> u16 {
        // Zero units (character device; MSCDEX reads the count from the
        // extended header), and a nonzero end address: the ROM block that
        // holds the header and its stubs.
        self.write_guest_linear_block(header + RH_DATA, &[0u8]);
        let mut end = [0u8; 4];
        end[..2].copy_from_slice(&0x0457u16.to_le_bytes());
        end[2..].copy_from_slice(&CD_DEVICE_HEADER_SEG.to_le_bytes());
        self.write_guest_linear_block(header + RH_DATA + 1, &end);
        self.write_guest_linear_block(header + RH_DATA + 5, &[0u8; 5]);
        self.cd_driver = CdDriverState {
            audio_channels: self.cd_driver.audio_channels,
            ..CdDriverState::default()
        };
        0
    }

    // --- ATAPI helpers -----------------------------------------------------

    /// TOKACD's `execute_checked`: run the CDB; on CHECK CONDITION fetch
    /// sense, keep a medium-change latched (and retry the packet once), and
    /// map the sense to the DOS driver error code.
    fn cd_exec_checked(&mut self, cdb: &[u8; 12], read_context: bool) -> Result<Vec<u8>, u16> {
        for attempt in 0..2 {
            match self.ide.device_mut().execute(cdb) {
                atapi::CmdResult::Data(data) => return Ok(data),
                atapi::CmdResult::Error => {
                    let mut sense_cdb = [0u8; 12];
                    sense_cdb[0] = 0x03;
                    sense_cdb[4] = 18;
                    let sense = match self.ide.device_mut().execute(&sense_cdb) {
                        atapi::CmdResult::Data(data) if data.len() >= 13 => data,
                        _ => return Err(ERR_GENERAL),
                    };
                    let key = sense[2] & 0x0F;
                    let asc = sense[12];
                    if key == 0x06 && asc == 0x28 && attempt == 0 {
                        // Medium changed: latch for IOCTL input 9 and retry
                        // the original packet once.
                        self.cd_driver.media_latch = true;
                        self.cd_driver.clear_audio();
                        self.cd_driver.head_lba = 0;
                        continue;
                    }
                    return Err(match (key, asc) {
                        (0x02, 0x3A) => ERR_NOT_READY,
                        (0x06, 0x28) => {
                            self.cd_driver.media_latch = true;
                            ERR_CHANGED
                        }
                        (_, 0x21) => ERR_SECTOR,
                        _ if read_context => ERR_READ,
                        _ => ERR_GENERAL,
                    });
                }
            }
        }
        unreachable!("the retry loop returns on both arms");
    }

    /// TEST UNIT READY: the media-change observation point.
    fn cd_ensure_ready(&mut self) -> Result<(), u16> {
        let cdb = [0u8; 12];
        match self.cd_exec_checked(&cdb, false) {
            Ok(_) => {
                self.cd_driver.media_state = MEDIA_PRESENT;
                self.cd_driver.door_open = false;
                Ok(())
            }
            Err(code) => {
                if code == ERR_NOT_READY {
                    if self.cd_driver.media_state == MEDIA_PRESENT {
                        self.cd_driver.media_latch = true;
                    }
                    self.cd_driver.media_state = MEDIA_ABSENT;
                    self.cd_driver.clear_audio();
                    self.cd_driver.head_lba = 0;
                }
                Err(code)
            }
        }
    }

    /// READ SUBCHANNEL current position: updates the cached head LBA and the
    /// playing/paused flags from the audio-status byte.
    fn cd_read_subchannel(&mut self) -> Result<Vec<u8>, u16> {
        self.cd_ensure_ready()?;
        let mut cdb = [0u8; 12];
        cdb[0] = 0x42;
        cdb[1] = 0x02;
        cdb[2] = 0x40;
        cdb[3] = 0x01;
        cdb[8] = 16;
        let data = self.cd_exec_checked(&cdb, false)?;
        if data.len() >= 16 {
            let packed =
                u32::from(data[11]) | (u32::from(data[10]) << 8) | (u32::from(data[9]) << 16);
            if let Some(lba) = packed_msf_to_lba(packed) {
                self.cd_driver.head_lba = lba;
            }
            match data[1] {
                0x11 => {
                    self.cd_driver.audio_playing = true;
                    self.cd_driver.audio_paused = false;
                }
                0x12 => {
                    self.cd_driver.audio_playing = false;
                    self.cd_driver.audio_paused = true;
                }
                _ => self.cd_driver.clear_audio(),
            }
        }
        Ok(data)
    }

    /// READ CAPACITY: total addressable sectors (last LBA + 1).
    fn cd_disc_capacity(&mut self) -> Result<u32, u16> {
        self.cd_ensure_ready()?;
        let mut cdb = [0u8; 12];
        cdb[0] = 0x25;
        let data = self.cd_exec_checked(&cdb, false)?;
        if data.len() < 4 {
            return Err(ERR_GENERAL);
        }
        Ok(u32::from_be_bytes(data[..4].try_into().unwrap()).wrapping_add(1))
    }

    // --- data requests -----------------------------------------------------

    fn cd_request_start_lba(&mut self, header: u32) -> Option<u32> {
        let start = self.cd_req_u32(header, RH_READ_START);
        if self.cd_req_u8(header, RH_READ_ADDR) == 0 {
            Some(start)
        } else {
            packed_msf_to_lba(start)
        }
    }

    fn cd_request_read_long(&mut self, header: u32) -> u16 {
        if self.cd_req_u8(header, RH_READ_ADDR) > 1 || self.cd_req_u8(header, RH_READ_MODE) != 0 {
            return ST_ERROR | ERR_COMMAND;
        }
        let Some(lba0) = self.cd_request_start_lba(header) else {
            return ST_ERROR | ERR_COMMAND;
        };
        let count = self.cd_req_u16(header, RH_READ_COUNT);
        if count == 0 {
            return 0;
        }
        if let Err(code) = self.cd_ensure_ready() {
            return ST_ERROR | code;
        }
        // TOKACD canonicalized the transfer pointer with 16-bit segment
        // arithmetic (`normalize_far_pointer`): seg += off >> 4 WRAPPING AT
        // 64K, off &= 0Fh. A caller may pass a noncanonical pointer whose
        // "negative" segment relies on that wrap to land back in low memory
        // (CDPROT's 0x37 case), so the same arithmetic applies here.
        let ptr = self.cd_req_u32(header, RH_READ_BUFFER);
        let seg = (ptr >> 16) as u16;
        let off = ptr as u16;
        let canon_seg = seg.wrapping_add(off >> 4);
        let mut buffer = (u32::from(canon_seg) << 4) + u32::from(off & 0xF);
        let mut done = 0u16;
        let mut result = 0u16;
        for i in 0..u32::from(count) {
            let mut cdb = [0u8; 12];
            cdb[0] = 0x28;
            cdb[2..6].copy_from_slice(&(lba0 + i).to_be_bytes());
            cdb[8] = 1;
            match self.cd_exec_checked(&cdb, true) {
                Ok(sector) => {
                    // A sector that cannot reach the caller's mapping is a
                    // read error with a truthful transfer count, the same
                    // contract the EDD packet keeps.
                    if self.write_guest_linear_block(buffer, &sector) < sector.len() {
                        result = ST_ERROR | ERR_READ;
                        break;
                    }
                    buffer = buffer.wrapping_add(cdimage::DATA_SECTOR as u32);
                    done += 1;
                    self.cd_driver.clear_audio();
                    self.cd_driver.head_lba = lba0 + i + 1;
                }
                Err(code) => {
                    result = ST_ERROR | code;
                    break;
                }
            }
        }
        self.cd_accesses += 1;
        // The count field is where a block driver reports what actually
        // moved, so a short transfer is visible there and not only in the
        // status word.
        self.write_guest_linear_block(header + RH_READ_COUNT, &done.to_le_bytes());
        result
    }

    fn cd_request_seek(&mut self, header: u32) -> u16 {
        if self.cd_req_u8(header, RH_READ_ADDR) > 1 {
            return ST_ERROR | ERR_COMMAND;
        }
        let Some(lba) = self.cd_request_start_lba(header) else {
            return ST_ERROR | ERR_COMMAND;
        };
        if let Err(code) = self.cd_ensure_ready() {
            return ST_ERROR | code;
        }
        let mut cdb = [0u8; 12];
        cdb[0] = 0x2B;
        cdb[2..6].copy_from_slice(&lba.to_be_bytes());
        match self.cd_exec_checked(&cdb, false) {
            Ok(_) => {
                self.cd_driver.head_lba = lba;
                self.cd_driver.clear_audio();
                0
            }
            Err(code) => ST_ERROR | code,
        }
    }

    // --- audio requests ----------------------------------------------------

    fn cd_request_play(&mut self, header: u32) -> u16 {
        let addr_mode = self.cd_req_u8(header, RH_PLAY_ADDR);
        if addr_mode > 1 {
            return ST_ERROR | ERR_COMMAND;
        }
        let input_start = self.cd_req_u32(header, RH_PLAY_START);
        let count = self.cd_req_u32(header, RH_PLAY_COUNT);
        // Syntax before device traffic, as TOKACD ordered it: a malformed
        // Red Book address fails ERR_SECTOR with no CDB sent.
        if addr_mode != 0 && !play_msf_syntax_ok(input_start) {
            return ST_ERROR | ERR_SECTOR;
        }
        let capacity = match self.cd_disc_capacity() {
            Ok(capacity) => capacity,
            Err(code) => return ST_ERROR | code,
        };
        let lba_start = if addr_mode == 0 {
            input_start
        } else {
            match packed_msf_to_lba(input_start) {
                Some(lba) => lba,
                None => return ST_ERROR | ERR_SECTOR,
            }
        };
        if lba_start >= capacity {
            return ST_ERROR | ERR_SECTOR;
        }
        let Some(lba_end) = lba_start.checked_add(count) else {
            return ST_ERROR | ERR_SECTOR;
        };
        if lba_end > capacity {
            return ST_ERROR | ERR_SECTOR;
        }
        // The status range is retained in the caller's own addressing form.
        let (stat_start, stat_end) = if addr_mode == 0 {
            (lba_start, lba_end)
        } else {
            (lba_to_packed_msf(lba_start), lba_to_packed_msf(lba_end))
        };
        let start_msf = lba_to_packed_msf(lba_start);
        let end_msf = lba_to_packed_msf(lba_end);
        let mut cdb = [0u8; 12];
        cdb[0] = 0x47;
        cdb[3] = (start_msf >> 16) as u8;
        cdb[4] = (start_msf >> 8) as u8;
        cdb[5] = start_msf as u8;
        cdb[6] = (end_msf >> 16) as u8;
        cdb[7] = (end_msf >> 8) as u8;
        cdb[8] = end_msf as u8;
        if let Err(code) = self.cd_exec_checked(&cdb, false) {
            return ST_ERROR | code;
        }
        if count == 0 {
            self.cd_driver.clear_audio();
            return 0;
        }
        self.cd_driver.last_addr_mode = addr_mode;
        self.cd_driver.last_start = stat_start;
        self.cd_driver.last_end = stat_end;
        self.cd_driver.head_lba = lba_start;
        self.cd_driver.audio_paused = false;
        self.cd_driver.audio_playing = true;
        0
    }

    fn cd_request_stop(&mut self) -> u16 {
        if !self.cd_driver.audio_playing {
            if self.cd_driver.audio_paused {
                return 0;
            }
            self.cd_driver.clear_audio();
            return 0;
        }
        let data = match self.cd_read_subchannel() {
            Ok(data) => data,
            Err(code) => return ST_ERROR | code,
        };
        if !self.cd_driver.audio_playing {
            self.cd_driver.clear_audio();
            return 0;
        }
        // Save the current absolute position as the point RESUME continues
        // from, in the retained range's own addressing form.
        let packed = u32::from(data[11]) | (u32::from(data[10]) << 8) | (u32::from(data[9]) << 16);
        let Some(lba) = packed_msf_to_lba(packed) else {
            return ST_ERROR | ERR_GENERAL;
        };
        self.cd_driver.head_lba = lba;
        self.cd_driver.last_start = if self.cd_driver.last_addr_mode == 0 {
            lba
        } else {
            packed
        };
        let mut cdb = [0u8; 12];
        cdb[0] = 0x4B;
        match self.cd_exec_checked(&cdb, false) {
            Ok(_) => {
                self.cd_driver.audio_playing = false;
                self.cd_driver.audio_paused = true;
                0
            }
            Err(code) => ST_ERROR | code,
        }
    }

    fn cd_request_resume(&mut self) -> u16 {
        if !self.cd_driver.audio_paused {
            return ST_ERROR | ERR_GENERAL;
        }
        if let Err(code) = self.cd_ensure_ready() {
            return ST_ERROR | code;
        }
        if !self.cd_driver.audio_paused {
            return ST_ERROR | ERR_GENERAL;
        }
        let mut cdb = [0u8; 12];
        cdb[0] = 0x4B;
        cdb[8] = 1;
        if let Err(code) = self.cd_exec_checked(&cdb, false) {
            return ST_ERROR | code;
        }
        match self.cd_read_subchannel() {
            Ok(_) if self.cd_driver.audio_playing => 0,
            Ok(_) => ST_ERROR | ERR_GENERAL,
            Err(code) => {
                self.cd_driver.clear_audio();
                ST_ERROR | code
            }
        }
    }

    // --- IOCTL input -------------------------------------------------------

    fn cd_ioctl_input(&mut self, header: u32) -> u16 {
        let ptr = self.cd_req_u32(header, RH_IOCTL_PTR);
        let cb = ((ptr >> 16) << 4).wrapping_add(ptr & 0xFFFF);
        let count = self.cd_req_u16(header, RH_IOCTL_COUNT);
        let code = self.read_guest_linear_block(cb, 1)[0];
        let need: u16 = match code {
            0 | 6 | 8 => 5,
            1 => 6,
            4 => 9,
            7 => 4,
            9 => 2,
            10 | 11 => 7,
            12 | 15 => 11,
            _ => return ST_ERROR | ERR_COMMAND,
        };
        if count < need {
            return ST_ERROR | ERR_LENGTH;
        }
        match code {
            // Device-header address: the ROM header.
            0 => {
                let mut out = [0u8; 4];
                out[..2].copy_from_slice(&CD_DEVICE_HEADER_OFF.to_le_bytes());
                out[2..].copy_from_slice(&CD_DEVICE_HEADER_SEG.to_le_bytes());
                self.write_guest_linear_block(cb + 1, &out);
                0
            }
            1 => self.cd_ioctl_head(cb),
            4 => {
                let channels = self.cd_driver.audio_channels;
                self.write_guest_linear_block(cb + 1, &channels);
                0
            }
            // Device status: cooked reads, read-only media, audio play,
            // channel control, HSG and Red Book addressing; door bits.
            6 => {
                let mut status = 0x0310u32;
                if self.cd_driver.door_open {
                    status |= 0x0001;
                }
                if !self.cd_driver.door_locked {
                    status |= 0x0002;
                }
                self.write_guest_linear_block(cb + 1, &status.to_le_bytes());
                0
            }
            7 => {
                if self.read_guest_linear_block(cb + 1, 1)[0] != 0 {
                    return ST_ERROR | ERR_COMMAND;
                }
                self.write_guest_linear_block(cb + 2, &2048u16.to_le_bytes());
                0
            }
            8 => match self.cd_disc_capacity() {
                Ok(sectors) => {
                    self.write_guest_linear_block(cb + 1, &sectors.to_le_bytes());
                    0
                }
                Err(code) => ST_ERROR | code,
            },
            9 => {
                let _ = self.cd_ensure_ready();
                let byte = if self.cd_driver.media_latch {
                    self.cd_driver.media_latch = false;
                    0xFF
                } else if self.cd_driver.media_state == MEDIA_PRESENT {
                    1
                } else {
                    0
                };
                self.write_guest_linear_block(cb + 1, &[byte]);
                0
            }
            10 => self.cd_ioctl_audio_disk(cb),
            11 => self.cd_ioctl_audio_track(cb),
            12 => self.cd_ioctl_audio_q(cb),
            15 => {
                let mut out = [0u8; 10];
                out[..2].copy_from_slice(&u16::from(self.cd_driver.audio_paused).to_le_bytes());
                out[2..6].copy_from_slice(&self.cd_driver.last_start.to_le_bytes());
                out[6..10].copy_from_slice(&self.cd_driver.last_end.to_le_bytes());
                self.write_guest_linear_block(cb + 1, &out);
                0
            }
            _ => unreachable!("gated above"),
        }
    }

    fn cd_ioctl_head(&mut self, cb: u32) -> u16 {
        let mode = self.read_guest_linear_block(cb + 1, 1)[0];
        if mode > 1 {
            return ST_ERROR | ERR_COMMAND;
        }
        if self.cd_driver.audio_playing || self.cd_driver.audio_paused {
            match self.cd_read_subchannel() {
                Ok(_) => {}
                Err(code) => return ST_ERROR | code,
            }
        }
        let value = if mode == 0 {
            self.cd_driver.head_lba
        } else {
            lba_to_packed_msf(self.cd_driver.head_lba)
        };
        self.write_guest_linear_block(cb + 2, &value.to_le_bytes());
        0
    }

    /// READ TOC (MSF) for the lead-out: first track, last track, and the
    /// lead-out address as packed frame/second/minute bytes.
    fn cd_ioctl_audio_disk(&mut self, cb: u32) -> u16 {
        if let Err(code) = self.cd_ensure_ready() {
            return ST_ERROR | code;
        }
        let mut cdb = [0u8; 12];
        cdb[0] = 0x43;
        cdb[1] = 0x02;
        cdb[6] = 0xAA;
        cdb[8] = 12;
        let data = match self.cd_exec_checked(&cdb, false) {
            Ok(data) if data.len() >= 12 => data,
            Ok(_) => return ST_ERROR | ERR_GENERAL,
            Err(code) => return ST_ERROR | code,
        };
        let out = [data[2], data[3], data[11], data[10], data[9], 0];
        self.write_guest_linear_block(cb + 1, &out);
        0
    }

    /// READ TOC (MSF) for one track: its start address and control byte
    /// (ADR/control nibbles swapped into the DOS order).
    fn cd_ioctl_audio_track(&mut self, cb: u32) -> u16 {
        let track = self.read_guest_linear_block(cb + 1, 1)[0];
        if let Err(code) = self.cd_ensure_ready() {
            return ST_ERROR | code;
        }
        let mut cdb = [0u8; 12];
        cdb[0] = 0x43;
        cdb[1] = 0x02;
        cdb[6] = track;
        cdb[8] = 12;
        let data = match self.cd_exec_checked(&cdb, false) {
            Ok(data) if data.len() >= 12 => data,
            Ok(_) => return ST_ERROR | ERR_GENERAL,
            Err(code) => return ST_ERROR | code,
        };
        let control = data[5].rotate_left(4);
        let out = [data[11], data[10], data[9], 0, control];
        self.write_guest_linear_block(cb + 2, &out);
        0
    }

    /// Audio Q-channel: ADR/control, track, index, track-relative and
    /// absolute positions from the current-position subchannel page.
    fn cd_ioctl_audio_q(&mut self, cb: u32) -> u16 {
        let data = match self.cd_read_subchannel() {
            Ok(data) if data.len() >= 16 => data,
            Ok(_) => return ST_ERROR | ERR_GENERAL,
            Err(code) => return ST_ERROR | code,
        };
        let control = data[5].rotate_left(4);
        let out = [
            control, data[6], data[7], data[13], data[14], data[15], 0, data[9], data[10], data[11],
        ];
        self.write_guest_linear_block(cb + 1, &out);
        0
    }

    // --- IOCTL output ------------------------------------------------------

    fn cd_ioctl_output(&mut self, header: u32) -> u16 {
        let ptr = self.cd_req_u32(header, RH_IOCTL_PTR);
        let cb = ((ptr >> 16) << 4).wrapping_add(ptr & 0xFFFF);
        let count = self.cd_req_u16(header, RH_IOCTL_COUNT);
        let code = self.read_guest_linear_block(cb, 1)[0];
        let need: u16 = match code {
            0 | 2 | 5 => 1,
            1 => 2,
            3 => 9,
            _ => return ST_ERROR | ERR_COMMAND,
        };
        if count < need {
            return ST_ERROR | ERR_LENGTH;
        }
        match code {
            // Eject: release any lock, then START STOP UNIT with LoEj.
            0 => {
                let mut unlock = [0u8; 12];
                unlock[0] = 0x1E;
                let _ = self.cd_exec_checked(&unlock, false);
                let mut eject = [0u8; 12];
                eject[0] = 0x1B;
                eject[4] = 0x02;
                if let Err(code) = self.cd_exec_checked(&eject, false) {
                    return ST_ERROR | code;
                }
                self.cd_driver.door_locked = false;
                self.cd_driver.door_open = true;
                self.cd_driver.media_state = MEDIA_ABSENT;
                self.cd_driver.media_latch = true;
                self.cd_driver.clear_audio();
                self.cd_driver.head_lba = 0;
                0
            }
            1 => {
                let lock = self.read_guest_linear_block(cb + 1, 1)[0] & 1;
                let mut cdb = [0u8; 12];
                cdb[0] = 0x1E;
                cdb[4] = lock;
                if let Err(code) = self.cd_exec_checked(&cdb, false) {
                    return ST_ERROR | code;
                }
                self.cd_driver.door_locked = lock != 0;
                0
            }
            2 => {
                // TOKACD led with an ATA soft reset (SRST via ATA_CONTROL)
                // before these CDBs. The host port owns the device object
                // directly, so the bus-level reset has no equivalent here;
                // the visible protocol (stop, unlock, state reset) matches.
                let mut stop = [0u8; 12];
                stop[0] = 0x4E;
                if let Err(code) = self.cd_exec_checked(&stop, false) {
                    return ST_ERROR | code;
                }
                let mut unlock = [0u8; 12];
                unlock[0] = 0x1E;
                if let Err(code) = self.cd_exec_checked(&unlock, false) {
                    return ST_ERROR | code;
                }
                self.cd_driver.door_locked = false;
                self.cd_driver.door_open = false;
                self.cd_driver.media_state = MEDIA_UNKNOWN;
                self.cd_driver.media_latch = true;
                self.cd_driver.clear_audio();
                self.cd_driver.head_lba = 0;
                0
            }
            3 => {
                let channels = self.read_guest_linear_block(cb + 1, 8);
                self.cd_driver.audio_channels.copy_from_slice(&channels);
                // MODE SELECT page 0Eh with the four channel/volume pairs, as
                // TOKACD sent on the wire.
                let mut cdb = [0u8; 12];
                cdb[0] = 0x55;
                cdb[1] = 0x10;
                cdb[8] = 24;
                if let Err(code) = self.cd_exec_checked(&cdb, false) {
                    return ST_ERROR | code;
                }
                let mut param = [0u8; 24];
                param[8] = 0x0E;
                param[9] = 14;
                param[16..20].copy_from_slice(&self.cd_driver.audio_channels[..4]);
                self.ide.device_mut().mode_select_data(&param);
                0
            }
            5 => {
                let mut cdb = [0u8; 12];
                cdb[0] = 0x1B;
                cdb[4] = 0x03;
                if let Err(code) = self.cd_exec_checked(&cdb, false) {
                    return ST_ERROR | code;
                }
                self.cd_driver.door_open = false;
                self.cd_driver.media_state = MEDIA_UNKNOWN;
                0
            }
            _ => unreachable!("gated above"),
        }
    }

    // --- little helpers ----------------------------------------------------

    fn cd_req_u8(&mut self, header: u32, offset: u32) -> u8 {
        self.read_guest_linear_block(header + offset, 1)[0]
    }

    fn cd_req_u16(&mut self, header: u32, offset: u32) -> u16 {
        let bytes = self.read_guest_linear_block(header + offset, 2);
        u16::from_le_bytes([bytes[0], bytes[1]])
    }

    fn cd_req_u32(&mut self, header: u32, offset: u32) -> u32 {
        let bytes = self.read_guest_linear_block(header + offset, 4);
        u32::from_le_bytes(bytes.try_into().unwrap())
    }
}
