//! In-memory CD-ROM image backing for the ATAPI drive.
//!
//! Two source layouts are supported:
//!
//! - A plain ISO: one MODE1 data track of 2048-byte sectors. The image length
//!   divides evenly by 2048 and every sector is a data sector.
//! - A CUE sheet with one BIN file: a multi-track disc. The CUE names the BIN
//!   and lists each track's MODE (MODE1/2048, MODE1/2352, or AUDIO/2352) and its
//!   start INDEX 01 as an MM:SS:FF address. Data tracks read back 2048 logical
//!   bytes per sector; AUDIO tracks read back the raw 2352-byte Red Book frame.
//!
//! Sector framing: a 2048-byte data track stores the user data directly; a
//! 2352-byte data track wraps each 2048-byte payload in the Red Book sync,
//! header, and ECC/EDC, so the user data sits at byte offset 16 of the frame.
//! `read_data_sector` unwraps that so the ATAPI READ commands always hand back
//! 2048-byte logical sectors regardless of the on-disc framing.

/// Bytes in a logical (MODE1) data sector handed to the guest.
pub const DATA_SECTOR: usize = 2048;
/// Bytes in a raw Red Book frame (the on-disc sector for AUDIO and MODE1/2352).
pub const RAW_SECTOR: usize = 2352;
/// Frames per second on a CD (the FF field of MM:SS:FF runs 0..75).
pub const FRAMES_PER_SEC: u32 = 75;
/// The 150-frame (2-second) lead-in offset: LBA 0 is absolute MSF 00:02:00.
pub const LEAD_IN_FRAMES: u32 = 150;

/// One track's kind, which fixes its sector framing and its TOC ADR/control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackMode {
    /// MODE1 data stored as bare 2048-byte sectors.
    Mode1_2048,
    /// MODE1 data stored as 2352-byte Red Book frames (payload at offset 16).
    Mode1_2352,
    /// Red Book CD-DA audio: raw 2352-byte stereo frames.
    Audio,
}

impl TrackMode {
    /// Bytes this track occupies per sector in the backing image.
    pub fn raw_size(self) -> usize {
        match self {
            TrackMode::Mode1_2048 => DATA_SECTOR,
            TrackMode::Mode1_2352 | TrackMode::Audio => RAW_SECTOR,
        }
    }

    pub fn is_audio(self) -> bool {
        matches!(self, TrackMode::Audio)
    }
}

/// One entry in the disc's track table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Track {
    /// 1-based track number as it appears in the TOC.
    pub number: u8,
    pub mode: TrackMode,
    /// First user LBA of this track (the INDEX 01 address, lead-in removed).
    pub start_lba: u32,
    /// Sector count in this track.
    pub sectors: u32,
    /// Byte offset of this track's first sector within the backing image.
    pub image_offset: usize,
}

impl Track {
    /// Last LBA (exclusive) covered by this track.
    pub fn end_lba(&self) -> u32 {
        self.start_lba + self.sectors
    }
}

/// A mounted CD image: the backing bytes plus the parsed track table.
#[derive(Debug, Clone)]
pub struct CdImage {
    bytes: Vec<u8>,
    tracks: Vec<Track>,
    /// Total user sectors across all tracks (the disc capacity).
    total_sectors: u32,
}

impl CdImage {
    /// Mount a plain ISO: a single MODE1/2048 data track. The length must divide
    /// evenly by 2048.
    pub fn from_iso(bytes: Vec<u8>) -> Result<Self, String> {
        if bytes.is_empty() || bytes.len() % DATA_SECTOR != 0 {
            return Err(format!(
                "ISO length {} is not a multiple of {DATA_SECTOR}",
                bytes.len()
            ));
        }
        let sectors = (bytes.len() / DATA_SECTOR) as u32;
        let track = Track {
            number: 1,
            mode: TrackMode::Mode1_2048,
            start_lba: 0,
            sectors,
            image_offset: 0,
        };
        Ok(Self {
            bytes,
            tracks: vec![track],
            total_sectors: sectors,
        })
    }

    /// Mount from a CUE sheet and its single BIN file. `cue` is the sheet text;
    /// `bin` is the raw image the sheet's `FILE` line names. The track table is
    /// derived from the TRACK/INDEX lines; per-track sector counts come from the
    /// span to the next track's start (the last track runs to the end of the BIN).
    pub fn from_cue(cue: &str, bin: Vec<u8>) -> Result<Self, String> {
        let parsed = parse_cue(cue)?;
        if parsed.is_empty() {
            return Err("CUE sheet declared no tracks".to_string());
        }

        // The INDEX addresses give each track's start in sectors (frames), so a
        // track's sector count is the delta to the next track's start (the last
        // runs to the end of the BIN at its own sector size). Byte offsets are the
        // running sum of preceding tracks' actual byte spans, since a mixed-mode
        // BIN packs different sector sizes back to back: 2048 for MODE1/2048,
        // 2352 for AUDIO and MODE1/2352. The track frame addresses stay the
        // logical (sector-count) timeline regardless of byte size.
        let mut tracks = Vec::with_capacity(parsed.len());
        let mut total_sectors = 0u32;
        let mut image_offset = 0usize;
        for (i, p) in parsed.iter().enumerate() {
            let raw = p.mode.raw_size();
            // Sector count: the span to the next track's start frame, or the
            // bytes left in the BIN for the last track.
            let sectors = match parsed.get(i + 1) {
                Some(n) => n.start_frame.saturating_sub(p.start_frame),
                None => ((bin.len().saturating_sub(image_offset)) / raw) as u32,
            };
            let span = sectors as usize * raw;
            if image_offset + span > bin.len() {
                return Err(format!(
                    "track {} (offset {image_offset}, {span} bytes) runs past the BIN ({} bytes)",
                    p.number,
                    bin.len()
                ));
            }
            tracks.push(Track {
                number: p.number,
                mode: p.mode,
                start_lba: p.start_frame,
                sectors,
                image_offset,
            });
            image_offset += span;
            total_sectors = total_sectors.max(p.start_frame + sectors);
        }

        Ok(Self {
            bytes: bin,
            tracks,
            total_sectors,
        })
    }

    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    /// Disc capacity in user sectors (the value READ CAPACITY reports, less one).
    pub fn total_sectors(&self) -> u32 {
        self.total_sectors
    }

    /// The track an LBA falls in, or None past the end of the disc.
    pub fn track_at_lba(&self, lba: u32) -> Option<&Track> {
        self.tracks
            .iter()
            .find(|t| lba >= t.start_lba && lba < t.end_lba())
    }

    /// Read one 2048-byte logical data sector at `lba`. Returns None when the LBA
    /// lands outside any track or in an AUDIO track (data reads of audio fail on
    /// hardware too). MODE1/2352 frames are unwrapped to their 2048-byte payload.
    pub fn read_data_sector(&self, lba: u32) -> Option<[u8; DATA_SECTOR]> {
        let track = self.track_at_lba(lba)?;
        if track.mode.is_audio() {
            return None;
        }
        let raw = track.mode.raw_size();
        let frame_off = track.image_offset + (lba - track.start_lba) as usize * raw;
        // MODE1/2352 stores the 2048-byte user data at offset 16 (12 sync + 4
        // header); MODE1/2048 stores it at the frame start.
        let payload_off = match track.mode {
            TrackMode::Mode1_2352 => frame_off + 16,
            _ => frame_off,
        };
        let slice = self.bytes.get(payload_off..payload_off + DATA_SECTOR)?;
        let mut out = [0u8; DATA_SECTOR];
        out.copy_from_slice(slice);
        Some(out)
    }

    /// Read one raw 2352-byte audio frame at `lba`, used by the CD-Audio mixer.
    /// Returns None outside an AUDIO track or past the image.
    pub fn read_audio_frame(&self, lba: u32) -> Option<[u8; RAW_SECTOR]> {
        let track = self.track_at_lba(lba)?;
        if !track.mode.is_audio() {
            return None;
        }
        let frame_off = track.image_offset + (lba - track.start_lba) as usize * RAW_SECTOR;
        let slice = self.bytes.get(frame_off..frame_off + RAW_SECTOR)?;
        let mut out = [0u8; RAW_SECTOR];
        out.copy_from_slice(slice);
        Some(out)
    }

    pub fn track_count(&self) -> u8 {
        self.tracks.len() as u8
    }
}

/// A track as read from the CUE before sector counts are derived.
struct CueTrack {
    number: u8,
    mode: TrackMode,
    start_frame: u32,
}

/// Parse a CUE sheet into its track list. Recognizes `TRACK n MODE1/2048`,
/// `MODE1/2352`, and `AUDIO`, with each track's `INDEX 01 MM:SS:FF` start. The
/// `FILE` and `PREGAP`/`INDEX 00` lines are accepted and ignored: a single-BIN
/// CUE keeps every track in one file, and INDEX 00 pregap is folded into the
/// preceding track's data on most rips.
fn parse_cue(cue: &str) -> Result<Vec<CueTrack>, String> {
    let mut tracks: Vec<CueTrack> = Vec::new();
    let mut pending: Option<(u8, TrackMode)> = None;

    for line in cue.lines() {
        let mut words = line.split_whitespace();
        let Some(keyword) = words.next() else {
            continue;
        };
        match keyword.to_ascii_uppercase().as_str() {
            "TRACK" => {
                let number: u8 = words
                    .next()
                    .and_then(|n| n.parse().ok())
                    .ok_or_else(|| format!("bad TRACK number in '{line}'"))?;
                let mode = match words.next().map(str::to_ascii_uppercase).as_deref() {
                    Some("MODE1/2048") => TrackMode::Mode1_2048,
                    Some("MODE1/2352") => TrackMode::Mode1_2352,
                    Some("AUDIO") => TrackMode::Audio,
                    Some(other) => return Err(format!("unsupported TRACK mode '{other}'")),
                    None => return Err(format!("missing TRACK mode in '{line}'")),
                };
                pending = Some((number, mode));
            }
            "INDEX" => {
                let idx: u8 = words.next().and_then(|n| n.parse().ok()).unwrap_or(0);
                // Only INDEX 01 (the track's user-data start) sets the address.
                if idx != 1 {
                    continue;
                }
                let msf = words
                    .next()
                    .ok_or_else(|| format!("missing INDEX time in '{line}'"))?;
                let frame = parse_msf(msf)?;
                let (number, mode) =
                    pending.ok_or_else(|| format!("INDEX before TRACK in '{line}'"))?;
                tracks.push(CueTrack {
                    number,
                    mode,
                    start_frame: frame,
                });
                pending = None;
            }
            _ => {}
        }
    }

    tracks.sort_by_key(|t| t.number);
    Ok(tracks)
}

/// Convert an MM:SS:FF address to an absolute frame number on the BIN timeline.
/// The CUE timeline starts at 00:00:00 = frame 0 (no lead-in is stored in a
/// BIN), so this is a direct MSF-to-frame conversion.
fn parse_msf(msf: &str) -> Result<u32, String> {
    let parts: Vec<&str> = msf.split(':').collect();
    if parts.len() != 3 {
        return Err(format!("malformed MSF '{msf}'"));
    }
    let m: u32 = parts[0]
        .parse()
        .map_err(|_| format!("bad minutes '{msf}'"))?;
    let s: u32 = parts[1]
        .parse()
        .map_err(|_| format!("bad seconds '{msf}'"))?;
    let f: u32 = parts[2]
        .parse()
        .map_err(|_| format!("bad frames '{msf}'"))?;
    if s >= 60 || f >= FRAMES_PER_SEC {
        return Err(format!("MSF field out of range '{msf}'"));
    }
    Ok((m * 60 + s) * FRAMES_PER_SEC + f)
}

/// Convert a user LBA to an absolute MSF (MM, SS, FF) including the 150-frame
/// lead-in: LBA 0 maps to 00:02:00. Used by READ TOC's MSF format.
pub fn lba_to_msf(lba: u32) -> (u8, u8, u8) {
    let total = lba + LEAD_IN_FRAMES;
    let m = total / (60 * FRAMES_PER_SEC);
    let s = (total / FRAMES_PER_SEC) % 60;
    let f = total % FRAMES_PER_SEC;
    (m as u8, s as u8, f as u8)
}

/// Convert an absolute MSF back to a user LBA (the inverse of `lba_to_msf`),
/// used by PLAY AUDIO MSF. Saturates at 0 if the MSF is inside the lead-in.
pub fn msf_to_lba(m: u8, s: u8, f: u8) -> u32 {
    let frames = (u32::from(m) * 60 + u32::from(s)) * FRAMES_PER_SEC + u32::from(f);
    frames.saturating_sub(LEAD_IN_FRAMES)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal single-track ISO whose first two sectors carry markers.
    fn tiny_iso(sectors: u32) -> Vec<u8> {
        let mut bytes = vec![0u8; sectors as usize * DATA_SECTOR];
        bytes[0] = 0xCD;
        bytes[DATA_SECTOR] = 0x02; // first byte of LBA 1
        bytes
    }

    #[test]
    fn iso_is_one_data_track() {
        let img = CdImage::from_iso(tiny_iso(4)).unwrap();
        assert_eq!(img.track_count(), 1);
        assert_eq!(img.total_sectors(), 4);
        let t = &img.tracks()[0];
        assert_eq!(t.mode, TrackMode::Mode1_2048);
        assert_eq!((t.start_lba, t.sectors), (0, 4));
    }

    #[test]
    fn iso_reads_back_logical_sectors() {
        let img = CdImage::from_iso(tiny_iso(4)).unwrap();
        assert_eq!(img.read_data_sector(0).unwrap()[0], 0xCD);
        assert_eq!(img.read_data_sector(1).unwrap()[0], 0x02);
        // Past the end reads nothing.
        assert!(img.read_data_sector(4).is_none());
    }

    #[test]
    fn iso_rejects_unaligned_length() {
        assert!(CdImage::from_iso(vec![0u8; 100]).is_err());
        assert!(CdImage::from_iso(Vec::new()).is_err());
    }

    #[test]
    fn cue_parses_data_plus_audio_tracks() {
        // Track 1: MODE1/2048 data, 2 sectors starting at frame 0.
        // Track 2: AUDIO, starting at frame 2 (right after the data).
        let cue = "FILE \"disc.bin\" BINARY\n\
                   TRACK 01 MODE1/2048\n\
                   INDEX 01 00:00:00\n\
                   TRACK 02 AUDIO\n\
                   INDEX 01 00:00:02\n";
        // Data: 2 sectors * 2048. Audio: 3 frames * 2352.
        let mut bin = vec![0u8; 2 * DATA_SECTOR + 3 * RAW_SECTOR];
        bin[0] = 0xAA; // data LBA 0 marker
        let audio_off = 2 * DATA_SECTOR;
        bin[audio_off] = 0xBB; // audio frame 0 marker
        let img = CdImage::from_cue(cue, bin).unwrap();
        assert_eq!(img.track_count(), 2);
        let t1 = img.tracks()[0];
        let t2 = img.tracks()[1];
        assert_eq!(t1.mode, TrackMode::Mode1_2048);
        assert_eq!((t1.start_lba, t1.sectors), (0, 2));
        assert_eq!(t2.mode, TrackMode::Audio);
        assert_eq!((t2.start_lba, t2.sectors), (2, 3));
        // Data sector reads back through the data track.
        assert_eq!(img.read_data_sector(0).unwrap()[0], 0xAA);
        // Audio frame reads back through the audio path; data read of audio fails.
        assert_eq!(img.read_audio_frame(2).unwrap()[0], 0xBB);
        assert!(img.read_data_sector(2).is_none());
    }

    #[test]
    fn cue_unwraps_mode1_2352_payload() {
        let cue = "FILE \"d.bin\" BINARY\nTRACK 01 MODE1/2352\nINDEX 01 00:00:00\n";
        let mut bin = vec![0u8; RAW_SECTOR];
        bin[16] = 0x7E; // user data starts at offset 16 in a raw frame
        let img = CdImage::from_cue(cue, bin).unwrap();
        assert_eq!(img.read_data_sector(0).unwrap()[0], 0x7E);
    }

    #[test]
    fn msf_round_trips_through_lba() {
        // LBA 0 is 00:02:00 with the lead-in.
        assert_eq!(lba_to_msf(0), (0, 2, 0));
        assert_eq!(msf_to_lba(0, 2, 0), 0);
        // 75 frames after the lead-in is one second later.
        assert_eq!(lba_to_msf(75), (0, 3, 0));
        assert_eq!(msf_to_lba(0, 3, 0), 75);
    }

    #[test]
    fn cue_rejects_unknown_mode() {
        let cue = "TRACK 01 MODE2/2336\nINDEX 01 00:00:00\n";
        assert!(CdImage::from_cue(cue, vec![0u8; RAW_SECTOR]).is_err());
    }
}
