// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! In-memory CD-ROM image backing for the ATAPI drive.
//!
//! Two source layouts are supported:
//!
//! - A plain ISO: one MODE1 data track of 2048-byte sectors. The image length
//!   divides evenly by 2048 and every sector is a data sector.
//! - A CUE sheet: a multi-track disc, either as one FILE shared by every
//!   track ([`CdImage::from_cue`]) or one FILE per track
//!   ([`CdImage::from_cue_files`], the common layout for a rip with CD audio).
//!   The CUE lists each track's MODE (MODE1/2048, MODE1/2352, MODE1/2448,
//!   MODE2/2048, MODE2/2336, MODE2/2352, AUDIO, or CDG) and its start
//!   INDEX 01 as an MM:SS:FF address relative to its own FILE. Data tracks
//!   read back 2048 logical bytes per sector; AUDIO tracks read back the raw
//!   2352-byte Red Book frame. MODE1/2448 and CDG store each frame with a
//!   96-byte subchannel tail appended (2352 + 96); the tail only grows the
//!   per-frame stride and is discarded on read, so the payload offset matches
//!   the untailed mode.
//!
//! Sector framing: a 2048-byte data track stores the user data directly; a
//! 2352-byte MODE1 track wraps each 2048-byte payload in the Red Book sync,
//! header, and ECC/EDC, so the user data sits at byte offset 16 of the frame.
//! A MODE2 (CD-XA) track adds an 8-byte subheader on top of that: MODE2/2352
//! carries the full sync+header+subheader wrapper (payload at offset 24) and
//! MODE2/2336 carries the subheader alone (payload at offset 8). CD-XA also
//! has two *forms*, and form is a per-sector property rather than a per-track
//! one: a Form 2 sector (streaming media, no logical payload) is detected via
//! the submode byte and read as absent, the same as a data read of audio.
//! `read_data_sector` unwraps all of this so the ATAPI READ commands always
//! hand back 2048-byte logical sectors regardless of the on-disc framing.
//!
//! A third source, [`CdImage::from_folder`], mounts a host folder: the
//! `iso9660` module materializes the metadata (PVD, path tables, directory
//! records) into a small in-memory image, but file *contents* are read lazily
//! from the host filesystem on each sector access rather than being copied in
//! up front. This is a single data track, same as a plain ISO.

/// Bytes in a logical (MODE1) data sector handed to the guest.
pub const DATA_SECTOR: usize = 2048;
/// Bytes in a raw Red Book frame (the on-disc sector for AUDIO and MODE1/2352).
pub const RAW_SECTOR: usize = 2352;
/// Bytes in a MODE2 frame stored without sync and header (subheader leads).
pub const MODE2_SECTOR: usize = 2336;
/// Bytes in a frame stored with its 96-byte subchannel tail (2352 + 96).
pub const SUBCHANNEL_SECTOR: usize = 2448;
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
    /// MODE1 data stored as 2448-byte frames: a 2352-byte Red Book frame
    /// followed by a 96-byte subchannel tail that is discarded on read.
    Mode1_2448,
    /// CD-XA MODE2 stored as bare 2048-byte Form 1 payloads.
    Mode2_2048,
    /// CD-XA MODE2 stored as 2336-byte frames: 8-byte subheader then payload.
    Mode2_2336,
    /// CD-XA MODE2 stored as full 2352-byte frames: 12 sync + 4 header +
    /// 8 subheader, so the Form 1 payload sits at offset 24.
    Mode2_2352,
    /// Red Book CD-DA audio: raw 2352-byte stereo frames.
    Audio,
    /// CD+G audio: a 2352-byte Red Book frame followed by the 96-byte
    /// subchannel tail carrying the graphics stream, which is discarded.
    AudioCdg,
}

impl TrackMode {
    /// Bytes this track occupies per sector in the backing image.
    pub fn raw_size(self) -> usize {
        match self {
            TrackMode::Mode1_2048 | TrackMode::Mode2_2048 => DATA_SECTOR,
            TrackMode::Mode2_2336 => MODE2_SECTOR,
            TrackMode::Mode1_2352 | TrackMode::Mode2_2352 | TrackMode::Audio => RAW_SECTOR,
            TrackMode::Mode1_2448 | TrackMode::AudioCdg => SUBCHANNEL_SECTOR,
        }
    }

    pub fn is_audio(self) -> bool {
        matches!(self, TrackMode::Audio | TrackMode::AudioCdg)
    }

    /// Byte offset of the 2048-byte user payload inside one stored frame, or
    /// None for a mode that has no logical payload at all. MODE1/2352 wraps the
    /// payload in 12 sync + 4 header bytes; MODE2/2352 adds an 8-byte subheader
    /// on top of that; MODE2/2336 carries the subheader alone; the /2048 forms
    /// store the payload bare. An AUDIO track returns None -- a data read of
    /// audio fails on hardware too.
    pub fn payload_offset(self) -> Option<usize> {
        match self {
            TrackMode::Mode1_2048 | TrackMode::Mode2_2048 => Some(0),
            TrackMode::Mode2_2336 => Some(8),
            TrackMode::Mode1_2352 | TrackMode::Mode1_2448 => Some(16),
            TrackMode::Mode2_2352 => Some(24),
            TrackMode::Audio | TrackMode::AudioCdg => None,
        }
    }

    /// Byte offset of the XA submode byte inside one stored frame, for the
    /// modes that carry a subheader. Bit 5 (0x20) set marks a Form 2 sector.
    /// MODE2/2048 has no subheader, so it has no submode byte.
    pub fn submode_offset(self) -> Option<usize> {
        match self {
            TrackMode::Mode2_2352 => Some(18),
            TrackMode::Mode2_2336 => Some(2),
            TrackMode::Mode1_2048
            | TrackMode::Mode1_2352
            | TrackMode::Mode2_2048
            | TrackMode::Audio
            | TrackMode::Mode1_2448
            | TrackMode::AudioCdg => None,
        }
    }
}

/// One entry in the disc's track table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Track {
    /// 1-based track number as it appears in the TOC.
    pub number: u8,
    pub mode: TrackMode,
    /// First user LBA of this track on the disc timeline. Equal to this track's
    /// INDEX 01 address only when no PREGAP precedes it (on this track or any
    /// earlier one) -- a PREGAP shifts this LBA forward without changing the
    /// INDEX 01 address itself.
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

/// Where a `CdImage`'s sector bytes actually live.
#[derive(Debug, Clone)]
enum Backing {
    /// The whole image is resident in memory (a plain ISO or a CUE+BIN pair).
    Bytes(Vec<u8>),
    /// A folder mount: `meta` holds the synthesized ISO9660 metadata sectors
    /// (PVD, path tables, directory records), and each entry in `extents`
    /// maps a contiguous LBA range past the metadata to a host file that is
    /// read lazily, sector by sector, rather than copied in up front.
    Folder {
        meta: Vec<u8>,
        extents: Vec<crate::iso9660::FileExtent>,
    },
}

/// A mounted CD image: the backing bytes plus the parsed track table.
#[derive(Debug, Clone)]
pub struct CdImage {
    backing: Backing,
    tracks: Vec<Track>,
    /// Total user sectors across all tracks (the disc capacity).
    total_sectors: u32,
}

impl CdImage {
    /// Mount a plain ISO: a single MODE1/2048 data track. The length must divide
    /// evenly by 2048.
    pub fn from_iso(bytes: Vec<u8>) -> Result<Self, String> {
        if bytes.is_empty() || !bytes.len().is_multiple_of(DATA_SECTOR) {
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
            backing: Backing::Bytes(bytes),
            tracks: vec![track],
            total_sectors: sectors,
        })
    }

    /// Mount a host folder as a single-track data disc. The folder's contents
    /// are laid out as ISO9660 metadata (see [`crate::iso9660::build`]); file
    /// bytes are not copied in, they are read from the host lazily as sectors
    /// are requested. Refuses folders whose total content exceeds the CD-ROM
    /// capacity guard (see [`crate::iso9660::MAX_IMAGE_BYTES`]).
    pub fn from_folder(built: crate::iso9660::BuiltImage) -> Result<Self, String> {
        let crate::iso9660::BuiltImage {
            meta,
            extents,
            total_sectors,
        } = built;
        let track = Track {
            number: 1,
            mode: TrackMode::Mode1_2048,
            start_lba: 0,
            sectors: total_sectors,
            image_offset: 0,
        };
        Ok(Self {
            backing: Backing::Folder { meta, extents },
            tracks: vec![track],
            total_sectors,
        })
    }

    /// Mount from a CUE sheet and a single BIN. Convenience wrapper over
    /// [`CdImage::from_cue_files`] for the common one-file sheet: every track
    /// is bound to `bin` regardless of what the sheet's FILE lines name.
    pub fn from_cue(cue: &str, bin: Vec<u8>) -> Result<Self, String> {
        let (_names, mut tracks) = parse_cue(cue)?;
        // One BIN: every track binds to it regardless of what the sheet's FILE
        // lines name, so flatten every track onto the single file up front.
        for track in &mut tracks {
            track.file_index = 0;
        }
        Self::build(tracks, &[bin.as_slice()])
    }

    /// Mount from a CUE sheet and the files it names. Each FILE opens a new
    /// byte origin: a track's offsets are relative to its own file, while the
    /// LBA timeline runs continuously across all of them. A track that is last
    /// in its file runs to that file's end.
    pub fn from_cue_files(cue: &str, files: Vec<(String, Vec<u8>)>) -> Result<Self, String> {
        let (names, tracks) = parse_cue(cue)?;
        // Resolve each FILE the sheet names to the bytes the caller supplied,
        // in sheet order. Borrowed slices, not owned copies: a CUE may legally
        // name the same file twice (that's how a sheet declares two tracks
        // living in one file), and an owned copy would clone those bytes once
        // per mention instead of once per file.
        let mut file_bytes: Vec<&[u8]> = Vec::with_capacity(names.len());
        for name in &names {
            let found = files
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                .ok_or_else(|| format!("CUE names a file that was not supplied: {name}"))?;
            file_bytes.push(found.1.as_slice());
        }
        Self::build(tracks, &file_bytes)
    }

    /// Shared track-table construction for both CUE entry points. `file_bytes`
    /// is indexed by each track's `file_index`; `from_cue` stamps every track
    /// to index 0 against its single-element slice before calling in, so this
    /// function never needs to know which entry point built its input.
    ///
    /// The INDEX addresses give each track's start in sectors (frames), so a
    /// track's sector count is the delta to the next track's start *within the
    /// same file* (a track that is last in its file runs to that file's own
    /// end at its own sector size). Byte offsets are the running sum of
    /// preceding tracks' actual byte spans within their file, since a
    /// mixed-mode file packs different sector sizes back to back: 2048 for
    /// MODE1/2048, 2352 for AUDIO and MODE1/2352. The track frame addresses
    /// stay the logical (sector-count) timeline regardless of byte size.
    ///
    /// Two timelines. `disc_lba` is what the guest sees, and PREGAP frames
    /// advance it without any bytes behind them. The CUE's INDEX addresses are
    /// positions within a file, so a track's byte span still comes from the
    /// delta between consecutive INDEX 01 values *in that file*. This
    /// derivation assumes tracks within a single file appear in non-decreasing
    /// INDEX 01 order (the `n.start_frame.saturating_sub(p.start_frame)` below
    /// silently floors an out-of-order pair to zero sectors instead of
    /// erroring); a sheet that violates this, within one file, mounts with a
    /// track table and `total_sectors` that no longer match the on-disc
    /// reality.
    fn build(tracks_in: Vec<CueTrack>, file_bytes: &[&[u8]]) -> Result<Self, String> {
        if tracks_in.is_empty() {
            return Err("CUE sheet declared no tracks".to_string());
        }
        if file_bytes.is_empty() {
            return Err("CUE sheet declared no FILE".to_string());
        }

        // Each file keeps its own byte cursor; `disc_lba` runs across all of them.
        // Size the backing up front: a disc's worth of bytes reallocated a few
        // times during the concatenation is copying nobody needs to pay for.
        let mut cursors = vec![0usize; file_bytes.len()];
        let total_bytes = file_bytes.iter().map(|b| b.len()).sum();
        let mut concatenated: Vec<u8> = Vec::with_capacity(total_bytes);
        let mut file_base = Vec::with_capacity(file_bytes.len());
        for bytes in file_bytes {
            file_base.push(concatenated.len());
            concatenated.extend_from_slice(bytes);
        }

        let mut tracks = Vec::with_capacity(tracks_in.len());
        let mut disc_lba = 0u32;
        for (i, p) in tracks_in.iter().enumerate() {
            let fi = p.file_index;
            // `fi` is always in range here: `from_cue` stamps every track to
            // index 0 against a one-element `file_bytes`, and
            // `from_cue_files` builds one `file_bytes` entry per FILE that
            // `parse_cue` drew `file_index` from, in the same order. This is
            // a `.get()`-over-index habit, not a live ambiguity -- same as
            // the bounds check in `read_data_sector`.
            let bytes = *file_bytes
                .get(fi)
                .ok_or_else(|| format!("track {} references an absent FILE", p.number))?;
            let raw = p.mode.raw_size();
            // The next track bounds this one only if it shares this file;
            // otherwise this track runs to its own file's end.
            let next_in_file = tracks_in.get(i + 1).filter(|n| n.file_index == fi);
            let sectors = match next_in_file {
                Some(n) => n.start_frame.saturating_sub(p.start_frame),
                None => ((bytes.len().saturating_sub(cursors[fi])) / raw) as u32,
            };
            let span = sectors as usize * raw;
            if cursors[fi] + span > bytes.len() {
                return Err(format!(
                    "track {} (offset {}, {span} bytes) runs past its file ({} bytes)",
                    p.number,
                    cursors[fi],
                    bytes.len()
                ));
            }
            disc_lba += p.pregap_frames;
            tracks.push(Track {
                number: p.number,
                mode: p.mode,
                start_lba: disc_lba,
                sectors,
                image_offset: file_base[fi] + cursors[fi],
            });
            cursors[fi] += span;
            disc_lba += sectors;
        }

        Ok(Self {
            backing: Backing::Bytes(concatenated),
            tracks,
            total_sectors: disc_lba,
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
    /// lands outside any track, in an AUDIO track (data reads of audio fail on
    /// hardware too), or on a CD-XA Form 2 sector (streaming media, not a
    /// logical sector). MODE1/2352 and MODE2/2336/2352 frames are unwrapped to
    /// their 2048-byte payload.
    ///
    /// For a folder mount, a host file read error never panics the device path:
    /// it is logged and served as a zero-filled sector instead.
    pub fn read_data_sector(&self, lba: u32) -> Option<[u8; DATA_SECTOR]> {
        let track = self.track_at_lba(lba)?;
        // An AUDIO track has no logical payload; hardware fails a data read of
        // one too. This is the only audio guard -- it covers both backings.
        let payload_offset = track.mode.payload_offset()?;
        match &self.backing {
            Backing::Bytes(bytes) => {
                let raw = track.mode.raw_size();
                let frame_off = track.image_offset + (lba - track.start_lba) as usize * raw;
                // Form is a per-sector property: an XA track may mix Form 1 and
                // Form 2. A Form 2 sector carries a 2324-byte streaming payload,
                // not a 2048-byte logical sector, so it reads as absent -- the
                // same answer hardware gives for a data read of an audio track.
                // The `bytes.get(..)?` below would also return None for a frame
                // truncated mid-track, indistinguishable from a real Form 2
                // rejection, but `from_cue`'s mount-time bounds check already
                // guarantees every in-track LBA's frame fits inside the BIN, so
                // that path is defense-in-depth, not a live ambiguity.
                if let Some(submode) = track.mode.submode_offset()
                    && bytes.get(frame_off + submode)? & 0x20 != 0
                {
                    return None;
                }
                let payload_off = frame_off + payload_offset;
                let slice = bytes.get(payload_off..payload_off + DATA_SECTOR)?;
                let mut out = [0u8; DATA_SECTOR];
                out.copy_from_slice(slice);
                Some(out)
            }
            Backing::Folder { meta, extents } => Some(read_folder_sector(meta, extents, lba)),
        }
    }

    /// Read one raw 2352-byte audio frame at `lba`, used by the CD-Audio mixer.
    /// Returns None outside an AUDIO track or past the image. A folder mount has
    /// no audio track, so this always returns None for it.
    pub fn read_audio_frame(&self, lba: u32) -> Option<[u8; RAW_SECTOR]> {
        let track = self.track_at_lba(lba)?;
        if !track.mode.is_audio() {
            return None;
        }
        let Backing::Bytes(bytes) = &self.backing else {
            return None;
        };
        let stride = track.mode.raw_size();
        let frame_off = track.image_offset + (lba - track.start_lba) as usize * stride;
        // A CD+G frame is 2448 bytes: the Red Book audio leads, the 96-byte
        // subchannel tail follows and is not audio.
        let slice = bytes.get(frame_off..frame_off + RAW_SECTOR)?;
        let mut out = [0u8; RAW_SECTOR];
        out.copy_from_slice(slice);
        Some(out)
    }

    pub fn track_count(&self) -> u8 {
        self.tracks.len() as u8
    }
}

/// Serve one data sector of a folder mount: from the resident `meta` bytes if
/// `lba` falls inside them, otherwise by seeking into whichever host file's
/// extent covers `lba` and reading its sector directly (never the whole
/// file). A read past a file's own length, or a host I/O error, yields a
/// zero-filled sector rather than a panic: the device path must stay up even
/// if a file was moved or deleted out from under a live mount.
fn read_folder_sector(
    meta: &[u8],
    extents: &[crate::iso9660::FileExtent],
    lba: u32,
) -> [u8; DATA_SECTOR] {
    let meta_sectors = (meta.len() / DATA_SECTOR) as u32;
    if lba < meta_sectors {
        let off = lba as usize * DATA_SECTOR;
        let mut out = [0u8; DATA_SECTOR];
        out.copy_from_slice(&meta[off..off + DATA_SECTOR]);
        return out;
    }
    let Some(extent) = extents
        .iter()
        .find(|e| lba >= e.start_lba && lba < e.start_lba + e.sectors)
    else {
        return [0u8; DATA_SECTOR];
    };
    let sector_index = (lba - extent.start_lba) as u64;
    let byte_off = sector_index * DATA_SECTOR as u64;
    let mut out = [0u8; DATA_SECTOR];
    if byte_off >= extent.len {
        // Fully past the file's real content (a zero-fill tail sector that
        // exists only to round the extent up to a whole sector).
        return out;
    }
    let want = (extent.len - byte_off).min(DATA_SECTOR as u64) as usize;
    match read_file_range(&extent.host_path, byte_off, want) {
        Ok(data) => out[..data.len()].copy_from_slice(&data),
        Err(err) => {
            eprintln!(
                "cdimage: failed to read {} at offset {byte_off}: {err}",
                extent.host_path.display()
            );
        }
    }
    out
}

/// Read exactly `want` bytes from `path` starting at `offset`, without
/// loading the whole file into memory.
fn read_file_range(path: &std::path::Path, offset: u64, want: usize) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; want];
    file.read_exact(&mut buf)?;
    Ok(buf)
}

/// A track as read from the CUE before sector counts are derived.
struct CueTrack {
    number: u8,
    mode: TrackMode,
    start_frame: u32,
    /// Frames declared by PREGAP: present on the disc timeline, absent from the
    /// file. They shift this track and everything after it to a higher LBA
    /// without consuming any bytes.
    pregap_frames: u32,
    /// Index into the sheet's FILE list. Each FILE opens a new byte origin;
    /// every TRACK until the next FILE belongs to it.
    file_index: usize,
}

/// Parse a CUE sheet into its FILE list and track list. Recognizes
/// `TRACK n MODE1/2048`, `MODE1/2352`, `MODE1/2448`, `MODE2/2048`,
/// `MODE2/2336`, `MODE2/2352`, `AUDIO`, and `CDG`, with each track's
/// `INDEX 01 MM:SS:FF` start. Each `FILE` line opens a new byte origin, named
/// in the returned file list in sheet order; every track records which FILE
/// it belongs to. `PREGAP` and `INDEX 00` both mean different things: PREGAP
/// is not stored in the file and advances the disc LBA timeline by itself
/// (handled in `build`), while INDEX 00 addresses bytes that ARE in the file
/// and are folded into the preceding track's span by only ever reading
/// INDEX 01 here.
fn parse_cue(cue: &str) -> Result<(Vec<String>, Vec<CueTrack>), String> {
    let mut files: Vec<String> = Vec::new();
    let mut tracks: Vec<CueTrack> = Vec::new();
    let mut pending: Option<(u8, TrackMode)> = None;
    let mut pending_pregap = 0u32;

    for line in cue.lines() {
        let trimmed = line.trim();
        let mut words = trimmed.split_whitespace();
        let Some(keyword) = words.next() else {
            continue;
        };
        match keyword.to_ascii_uppercase().as_str() {
            "FILE" => {
                let rest = trimmed[keyword.len()..].trim_start();
                let name = if let Some(rest) = rest.strip_prefix('"') {
                    rest.split('"').next()
                } else {
                    rest.split_whitespace().next()
                };
                let name = name
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| format!("missing FILE name in '{line}'"))?;
                files.push(name.to_string());
            }
            "TRACK" => {
                let number: u8 = words
                    .next()
                    .and_then(|n| n.parse().ok())
                    .ok_or_else(|| format!("bad TRACK number in '{line}'"))?;
                let mode = match words.next().map(str::to_ascii_uppercase).as_deref() {
                    Some("MODE1/2048") => TrackMode::Mode1_2048,
                    Some("MODE1/2352") => TrackMode::Mode1_2352,
                    Some("MODE2/2048") => TrackMode::Mode2_2048,
                    Some("MODE2/2336") => TrackMode::Mode2_2336,
                    Some("MODE2/2352") => TrackMode::Mode2_2352,
                    Some("MODE1/2448") => TrackMode::Mode1_2448,
                    Some("AUDIO") => TrackMode::Audio,
                    Some("CDG") => TrackMode::AudioCdg,
                    Some(other) => return Err(format!("unsupported TRACK mode '{other}'")),
                    None => return Err(format!("missing TRACK mode in '{line}'")),
                };
                pending = Some((number, mode));
                pending_pregap = 0;
            }
            "PREGAP" => {
                let msf = words
                    .next()
                    .ok_or_else(|| format!("missing PREGAP time in '{line}'"))?;
                pending_pregap = parse_msf(msf)?;
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
                    pregap_frames: pending_pregap,
                    file_index: files.len().saturating_sub(1),
                });
                pending = None;
                pending_pregap = 0;
            }
            _ => {}
        }
    }

    tracks.sort_by_key(|t| t.number);
    Ok((files, tracks))
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
#[path = "cdimage_test.rs"]
mod tests;
