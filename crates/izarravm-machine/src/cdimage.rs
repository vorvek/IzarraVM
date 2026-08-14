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
// The audio-source contract in izarravm-core names the same quantity, and its
// trait's return type is built from its own constant. If these ever disagree
// the two sides of the seam stop type-checking against each other in a way that
// is easy to misread as a trait mismatch, so fail here instead, at the source.
const _: () = assert!(RAW_SECTOR == izarravm_core::AUDIO_FRAME_BYTES);
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
    /// This track's 16-bit samples are stored big-endian, because its `FILE`
    /// line said `MOTOROLA`. Audio tracks only: the byte order of a data
    /// track's payload is the guest's business, not the drive's.
    pub byte_swapped: bool,
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

/// Where one CUE `FILE`'s frames come from.
///
/// Cloning an [`Audio`](CueSource::Audio) shares the one decode rather than
/// starting a second: two mounts of the same sheet describe the same file, and
/// a decode in flight is the thing both of them want.
#[derive(Clone)]
pub enum CueSource {
    /// The file's bytes are on-disc frames, laid out exactly as the sheet's
    /// track modes describe. This is every BINARY file and the only thing that
    /// existed before compressed audio was supported.
    Raw(Vec<u8>),
    /// The file is an encoded audio container. It contributes no bytes to the
    /// disc backing at all: its length comes from the source and so do its
    /// frames.
    Audio(std::sync::Arc<dyn izarravm_core::AudioTrackSource>),
}

/// Written by hand rather than derived. A derived `Debug` on the `Raw` arm
/// prints every byte of a disc the first time anything formats one, which for
/// the images this mounts is hundreds of megabytes into a log line.
impl std::fmt::Debug for CueSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CueSource::Raw(bytes) => f.debug_tuple("Raw").field(&bytes.len()).finish(),
            CueSource::Audio(source) => f.debug_tuple("Audio").field(source).finish(),
        }
    }
}

/// A mounted CD image: the backing bytes plus the parsed track table.
#[derive(Debug, Clone)]
pub struct CdImage {
    backing: Backing,
    tracks: Vec<Track>,
    /// Tracks whose frames come from a decoder rather than from `backing`,
    /// keyed by track number.
    ///
    /// A map rather than a field on [`Track`] because `Track` is a `Copy` TOC
    /// record that the ATAPI TOC path iterates by value, and an `Arc` in it
    /// would end that. Keyed by track number rather than by index so that the
    /// key means the same thing as the number in the message when a mount
    /// refuses a layout.
    audio_sources:
        std::collections::HashMap<u8, std::sync::Arc<dyn izarravm_core::AudioTrackSource>>,
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
            byte_swapped: false,
        };
        Ok(Self {
            backing: Backing::Bytes(bytes),
            tracks: vec![track],
            audio_sources: Default::default(),
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
            byte_swapped: false,
        };
        Ok(Self {
            backing: Backing::Folder { meta, extents },
            tracks: vec![track],
            audio_sources: Default::default(),
            total_sectors,
        })
    }

    /// Mount from a CUE sheet and a single BIN. Convenience wrapper over
    /// [`CdImage::from_cue_files`] for the common one-file sheet: every track
    /// is bound to `bin`, whatever the sheet's one FILE line names it. The
    /// byte order comes from the sheet -- that FILE line's type token, or
    /// BINARY when the sheet declares no FILE at all.
    ///
    /// A sheet naming two or more files is an error here, not a best effort:
    /// its tracks are addressed from the head of their own files and cannot be
    /// laid out against one blob. Use [`CdImage::from_cue_files`] for those.
    pub fn from_cue(cue: &str, bin: Vec<u8>) -> Result<Self, String> {
        let (files, mut tracks) = parse_cue(cue)?;
        // A sheet naming two or more files is not something one blob can be a
        // rough answer to. Each FILE opens its own byte origin, and the tracks
        // after the first are addressed from the head of *their* file; laying
        // them out sequentially against a single blob puts track 2 wherever
        // track 1 happened to end. The result is not this disc read
        // approximately, it is a different disc served without a word, so
        // refuse instead and name the entry point that can honor the layout.
        if files.len() > 1 {
            return Err(format!(
                "CUE names {} files but a single BIN was supplied; \
                 use CdImage::from_cue_files with the files the sheet names",
                files.len()
            ));
        }
        // One BIN: every track binds to it, whatever the sheet's FILE line
        // names it. Past the check above this loop cannot actually change a
        // track -- `parse_cue` indexes tracks by `files.len().saturating_sub(1)`
        // and a sheet with at most one FILE gives 0 for every track either
        // way. It stays because that is a fact about a formula in another
        // function, and what `build`'s contract needs is the property itself:
        // this entry point hands in one file and every track pointing at it.
        // Stating it here costs one pass over a handful of tracks and does not
        // go stale if that formula ever changes.
        for track in &mut tracks {
            track.file_index = 0;
        }
        // The type comes from the *first* FILE line, which after the check
        // above is the only FILE line there is. This is not a fix for anything
        // a user could have seen: on the loader's path this entry point is
        // reached only when the sheet named no file at all, and a single-BIN
        // `FILE "d.bin" MOTOROLA` sheet arrives through `from_cue_files`,
        // which has read the token all along. It is the API surface that was
        // wrong -- a caller handing this function a sheet and the bytes that
        // sheet describes got BINARY imposed on it whatever the sheet said,
        // and MOTOROLA is the one token no amount of sniffing can recover. A
        // sheet naming no file leaves nothing to read, so the fallback stays
        // BINARY: the reading that changes nothing.
        let file_type = files
            .first()
            .map(|(_name, file_type)| file_type.clone())
            .unwrap_or(CueFileType::Binary);
        // The one BIN is raw by construction: this entry point takes bytes, and
        // an encoded file reaches the disc model only through
        // [`CdImage::from_cue_sources`], which is where sniffing happens.
        let source = CueSource::Raw(bin);
        Self::build(
            tracks,
            &[BuildFile {
                // This entry point takes bytes, not a path, so there is no file
                // name to report -- and the checks that would name one are the
                // encoded-audio ones, which a `Raw` source cannot reach anyway.
                name: "the CUE's BIN",
                source: &source,
                file_type,
            }],
        )
    }

    /// Mount from a CUE sheet and the sources its FILEs resolve to. Each FILE
    /// opens a new byte origin: a track's offsets are relative to its own file,
    /// while the LBA timeline runs continuously across all of them. A track
    /// that is last in its file runs to that file's end.
    ///
    /// A [`CueSource::Audio`] file is the exception to all of that. It has no
    /// bytes here at all: its length comes from the decoder and so do its
    /// frames, so it neither occupies space in the backing nor bounds a track
    /// by a byte span.
    ///
    /// `files` must correspond exactly to the sheet's FILE lines, in any
    /// order. A name the sheet declares and `files` omits is an error, and so
    /// is the reverse -- a file supplied that the sheet never named means the
    /// caller and this parser disagree about what the sheet says, which is not
    /// a disagreement to resolve by mounting one side's reading in silence.
    ///
    /// This is the general entry point and the one the loader calls;
    /// [`CdImage::from_cue_files`] is the all-raw case expressed through it.
    /// Guards belong here rather than in that wrapper, or they sit off the
    /// production path.
    pub fn from_cue_sources(cue: &str, files: Vec<(String, CueSource)>) -> Result<Self, String> {
        let (files_in_sheet, tracks) = parse_cue(cue)?;
        // A repeated FILE name is not the "two tracks share one file" layout
        // (that is a single FILE section followed by multiple TRACK/INDEX
        // blocks, and file_index dedupes those naturally). It is a sheet
        // whose *sections* repeat a name, and `build` cannot honor that: it
        // resolves each section to its own `file_index` and always starts a
        // new file_index's cursor at 0, so a second section for the same
        // name would silently read back byte 0 of the file instead of that
        // section's own INDEX 01 offset -- a wrong-data mount, not an error.
        // No mainstream ripper emits this layout, so reject it loudly instead
        // of trying to make it work.
        let mut seen = std::collections::HashSet::with_capacity(files_in_sheet.len());
        for (name, _type) in &files_in_sheet {
            if !seen.insert(name.to_ascii_lowercase()) {
                return Err(format!(
                    "CUE names {name} in more than one FILE section; a file shared by \
                     several tracks belongs in one FILE with multiple TRACK entries"
                ));
            }
        }
        // Resolve each FILE the sheet names to the bytes the caller supplied,
        // in sheet order. Borrowed slices, not owned copies: names are now
        // unique per section (checked above), so this is one lookup per file.
        // What the sheet said about a file and what backs it are pushed as one
        // value, so they cannot fall out of step on the way in.
        let mut build_files: Vec<BuildFile<'_>> = Vec::with_capacity(files_in_sheet.len());
        for (name, file_type) in &files_in_sheet {
            let found = files
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                .ok_or_else(|| format!("CUE names a file that was not supplied: {name}"))?;
            build_files.push(BuildFile {
                name: name.as_str(),
                source: &found.1,
                file_type: file_type.clone(),
            });
        }
        // Every FILE the sheet names now has bytes behind it, but the reverse
        // was never asked: a file the caller supplied and the sheet did not
        // name simply vanished here. That silence is why the BOM bug went
        // unnoticed -- the loader read two files off disk, `parse_cue` listed
        // one because the marker had fused onto the first FILE keyword, and
        // the extra went nowhere while a track bound to the wrong file's
        // bytes. Two scanners disagreed about what the sheet said and only the
        // quieter one got a vote.
        //
        // After the loop above, every sheet name matched some supplied file
        // and sheet names are unique (checked at the top), so a count that
        // still differs means strictly more was supplied than named. Checking
        // it *here* rather than before the loop is what keeps a genuinely
        // missing file reported by name: equal counts with different names is
        // the loop's error to raise, and it names the file the sheet wanted.
        //
        // The loader dedups by name before reading, so a well-formed sheet
        // cannot trip this. That is the point: it costs a comparison and it
        // makes the next dropped FILE line loud, whatever the cause.
        if files.len() != build_files.len() {
            let unnamed: Vec<&str> = files
                .iter()
                .map(|(name, _bytes)| name.as_str())
                .filter(|name| {
                    !files_in_sheet
                        .iter()
                        .any(|(in_sheet, _type)| in_sheet.eq_ignore_ascii_case(name))
                })
                .collect();
            // `unnamed` is empty when the surplus is a *repeat* of a name the
            // sheet does name, so the counts carry the message on their own
            // and the list only elaborates when it has something to say.
            let detail = if unnamed.is_empty() {
                String::new()
            } else {
                format!(" (not named by the sheet: {})", unnamed.join(", "))
            };
            return Err(format!(
                "{} files were supplied for a CUE that names {}{detail}",
                files.len(),
                files_in_sheet.len()
            ));
        }
        Self::build(tracks, &build_files)
    }

    /// Mount from a CUE sheet and the raw bytes its FILEs name -- the all-raw
    /// case of [`CdImage::from_cue_sources`], which see for the rules on how
    /// `files` must line up with the sheet.
    ///
    /// Kept as its own name because a sheet with no compressed audio is still
    /// the common case and every caller of it predates decoding.
    pub fn from_cue_files(cue: &str, files: Vec<(String, Vec<u8>)>) -> Result<Self, String> {
        let files = files
            .into_iter()
            .map(|(name, bytes)| (name, CueSource::Raw(bytes)))
            .collect();
        Self::from_cue_sources(cue, files)
    }

    /// Shared track-table construction for both CUE entry points. `files` holds
    /// one [`BuildFile`] per FILE the sheet named, in sheet order, so it is
    /// indexed by each track's `file_index`; each entry carries that file's
    /// bytes and its declared type token together. `from_cue` stamps every
    /// track to index 0 against its single-element slice before calling in, so
    /// this function never needs to know which entry point built its input.
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
    fn build(tracks_in: Vec<CueTrack>, files: &[BuildFile<'_>]) -> Result<Self, String> {
        if tracks_in.is_empty() {
            return Err("CUE sheet declared no tracks".to_string());
        }
        if files.is_empty() {
            return Err("CUE sheet declared no FILE".to_string());
        }

        // Each file keeps its own byte cursor; `disc_lba` runs across all of them.
        // Size the backing up front: a disc's worth of bytes reallocated a few
        // times during the concatenation is copying nobody needs to pay for.
        //
        // Only raw files have bytes to concatenate. An audio-sourced file
        // occupies no space in the backing, so its `file_base` entry is the
        // same offset as its successor's -- harmless, because nothing ever
        // reads a source-backed track's `image_offset`.
        let mut cursors = vec![0usize; files.len()];
        let total_bytes = files.iter().map(|f| f.raw_bytes().len()).sum();
        let mut concatenated: Vec<u8> = Vec::with_capacity(total_bytes);
        let mut file_base = Vec::with_capacity(files.len());
        for file in files {
            file_base.push(concatenated.len());
            concatenated.extend_from_slice(file.raw_bytes());
        }

        let mut tracks = Vec::with_capacity(tracks_in.len());
        let mut audio_sources = std::collections::HashMap::new();
        let mut disc_lba = 0u32;
        for (i, p) in tracks_in.iter().enumerate() {
            let fi = p.file_index;
            // `fi` is always in range here: `from_cue` stamps every track to
            // index 0 against a one-element `files`, and `from_cue_files`
            // builds one `files` entry per FILE that `parse_cue` drew
            // `file_index` from, in the same order. This is a
            // `.get()`-over-index habit, not a live ambiguity -- same as the
            // bounds check in `read_data_sector`. It is also now the *only*
            // per-file lookup in this loop: bytes and type token arrive
            // together, so there is no second `.get()` left to answer a
            // different question about the same index.
            let file = files
                .get(fi)
                .ok_or_else(|| format!("track {} references an absent FILE", p.number))?;
            // PREGAP advances the guest's timeline with no bytes behind it, so
            // it applies whichever way this track's frames are produced. It is
            // hoisted above the split so both branches share one statement
            // rather than each remembering to do it.
            disc_lba += p.pregap_frames;

            let (sectors, image_offset, byte_swapped) = match file.source {
                CueSource::Audio(audio) => {
                    // Two layouts an encoded file cannot represent, refused
                    // here rather than mounted approximately. Both are checked
                    // before the length is taken, so a sheet that trips either
                    // never reaches the TOC at all.
                    //
                    // A data track needs bytes at a known offset with a known
                    // sector geometry, and a decoder offers neither: it hands
                    // back Red Book audio frames and nothing else. Mounting one
                    // anyway gives a data track that reads back silence, which
                    // is a game that starts and then cannot load.
                    if !p.mode.is_audio() {
                        return Err(format!(
                            "{} is an encoded audio file, but the CUE declares track {} on it \
                             as a data track; a data track must be raw",
                            file.name, p.number
                        ));
                    }
                    // The sector count comes from the whole file's duration, so
                    // there is no byte offset at which a second track inside it
                    // would begin and nothing to divide. This is asked of
                    // encoded files alone: many tracks sharing one raw BIN is
                    // an ordinary layout -- Tomb Raider Gold's sheet puts 60 on
                    // one -- and it stays supported.
                    if tracks_in.iter().filter(|t| t.file_index == fi).count() > 1 {
                        return Err(format!(
                            "{} is an encoded audio file named by more than one TRACK; \
                             one encoded file holds exactly one track",
                            file.name
                        ));
                    }
                    // A decoded file's length comes from the decoder, and the
                    // byte accounting is skipped entirely -- not as an
                    // optimization, but because the file has no bytes here. The
                    // span, the "runs past its file" bounds check and the cursor
                    // advance would all be measured against a length of zero:
                    // the check would reject every decoded mount, and the usual
                    // sector derivation would make the track zero-length,
                    // collapsing every later track onto one LBA.
                    if audio_sources.insert(p.number, audio.clone()).is_some() {
                        // Unreachable through either entry point today --
                        // `parse_cue` would have to emit two tracks with one
                        // number -- but silently replacing a source is a
                        // wrong-data mount, which is the class this whole
                        // change exists to remove. Say so instead.
                        return Err(format!("CUE declares track {} twice", p.number));
                    }
                    (audio.sectors(), 0usize, false)
                }
                CueSource::Raw(bytes) => {
                    let raw = p.mode.raw_size();
                    // The next track bounds this one only if it shares this
                    // file; otherwise this track runs to its own file's end.
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
                    let offset = file_base[fi] + cursors[fi];
                    cursors[fi] += span;
                    // MOTOROLA is declared per FILE but only means anything for
                    // audio: it describes the byte order of 16-bit samples, and
                    // a data track has no samples. Resolving it here, once per
                    // track, keeps the read path from having to reach back to
                    // the sheet.
                    //
                    // It is asked of raw files alone. A decoder hands back
                    // frames already in host order, so there is no stored byte
                    // order left to correct, and a sheet that says MOTOROLA
                    // over an Ogg is describing a file it does not have.
                    let swapped = p.mode.is_audio() && file.file_type == CueFileType::Motorola;
                    (sectors, offset, swapped)
                }
            };

            tracks.push(Track {
                number: p.number,
                mode: p.mode,
                start_lba: disc_lba,
                sectors,
                image_offset,
                byte_swapped,
            });
            disc_lba += sectors;
        }

        Ok(Self {
            backing: Backing::Bytes(concatenated),
            tracks,
            audio_sources,
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
        // A decoded track's frames never live in the backing. The index is
        // relative to the track's own start, so the pregap that `build` counted
        // into `start_lba` is already off the number by the time the decoder
        // sees it -- an encoded file holds the audio alone and knows nothing
        // about gaps.
        //
        // None from here is not an error: it is a frame the worker has not
        // reached yet, and the mixer renders it as silence and moves on, which
        // is also what a real drive does rather than stall the disc.
        if let Some(source) = self.audio_sources.get(&track.number) {
            return source.frame(lba - track.start_lba);
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
        if track.byte_swapped {
            // Big-endian samples: swap the two bytes of each 16-bit sample. The
            // stereo pairing is unaffected -- L and R stay where they are, only
            // each sample's own byte order changes. A Red Book frame is 588
            // stereo pairs of two 16-bit samples, so 2352 bytes divide into
            // exactly 1176 whole samples and no partial one can be left over.
            for sample in out.chunks_exact_mut(2) {
                sample.swap(0, 1);
            }
        }
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

/// A `FILE` line's trailing type token.
///
/// The token is advisory everywhere except `Motorola`. Rippers routinely write
/// `FILE "track02.ogg" WAVE`, or even `BINARY`, for a compressed file, so what
/// a file actually contains is decided by sniffing its bytes and an unrecognized
/// token is kept for diagnostics rather than rejected, upper-cased so a message
/// quoting it reads the same however the ripper cased it. `MOTOROLA` is the
/// exception because it is the only signal for big-endian sample order, which
/// no amount of looking at the bytes can recover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CueFileType {
    /// Raw little-endian bytes, and the default when a sheet omits the token.
    Binary,
    /// Raw big-endian 16-bit samples.
    Motorola,
    /// Anything else, upper-cased but otherwise kept as written.
    Other(String),
}

impl CueFileType {
    fn parse(token: Option<&str>) -> Self {
        match token.map(str::to_ascii_uppercase).as_deref() {
            None | Some("BINARY") => CueFileType::Binary,
            Some("MOTOROLA") => CueFileType::Motorola,
            Some(other) => CueFileType::Other(other.to_string()),
        }
    }
}

/// One entry of a sheet's FILE list: the name the line declares and the type
/// token that followed it. A pair rather than a struct because callers here and
/// downstream destructure it positionally; it is *named* because spelling the
/// pair out inline makes `parse_cue`'s return type trip clippy's
/// `type_complexity` gate, which CI runs as `-D warnings`. The lint scores the
/// signature as written, so any name would have satisfied it.
type CueFile = (String, CueFileType);

/// One FILE the sheet named, paired with the bytes standing behind it.
///
/// `build` needs several facts per file -- the bytes, the declared type token,
/// and more to come as the audio formats land -- and the obvious way to pass
/// them is one slice per fact, indexed in lockstep by a track's `file_index`.
/// That shape cannot state its own invariant: nothing makes the slices the same
/// length, and the two out-of-range readings disagreed in the dangerous
/// direction. A missing entry in the bytes slice raised an error, while a
/// missing entry in the type slice read as "not MOTOROLA" -- so a desync would
/// not have failed the mount, it would have served a big-endian disc as noise
/// without a complaint. Carrying one slice of *this* means a file's facts are
/// one value that cannot come apart, and the next per-file fact is a field here
/// rather than another slice the caller must keep aligned by hand.
struct BuildFile<'a> {
    /// What the sheet called this file, for the mount errors that have to name
    /// it. `from_cue` is handed a blob rather than a named file, so it supplies
    /// a description of that blob instead.
    name: &'a str,
    source: &'a CueSource,
    file_type: CueFileType,
}

impl BuildFile<'_> {
    /// The bytes this file contributes to the concatenated backing. An encoded
    /// audio file contributes none: its frames come from its decoder, and the
    /// empty slice here is the literal truth about the backing rather than a
    /// stand-in for absent data.
    fn raw_bytes(&self) -> &[u8] {
        match self.source {
            CueSource::Raw(bytes) => bytes,
            CueSource::Audio(_) => &[],
        }
    }
}

/// Parse a CUE sheet into its FILE list and track list. Recognizes
/// `TRACK n MODE1/2048`, `MODE1/2352`, `MODE1/2448`, `MODE2/2048`,
/// `MODE2/2336`, `MODE2/2352`, `AUDIO`, and `CDG`, with each track's
/// `INDEX 01 MM:SS:FF` start. Each `FILE` line opens a new byte origin, named
/// in the returned file list in sheet order alongside its declared type token
/// (see [`CueFileType`]); every track records which FILE it belongs to.
/// `PREGAP` and `INDEX 00` both mean different things: PREGAP is not stored in
/// the file and advances the disc LBA timeline by itself (handled in `build`),
/// while INDEX 00 addresses bytes that ARE in the file and are folded into the
/// preceding track's span by only ever reading INDEX 01 here.
fn parse_cue(cue: &str) -> Result<(Vec<CueFile>, Vec<CueTrack>), String> {
    let mut files: Vec<CueFile> = Vec::new();
    let mut tracks: Vec<CueTrack> = Vec::new();
    let mut pending: Option<(u8, TrackMode)> = None;
    let mut pending_pregap = 0u32;

    // A sheet saved by a Windows editor often opens with a UTF-8 BOM, and
    // `str::trim` will not take it off: U+FEFF is a `Cf` format character, not
    // `White_Space`. Left in place it fuses onto the first line's keyword, so
    // "FILE" arrives as "\u{FEFF}FILE", matches no arm below, and falls
    // through the catch-all. Dropping a line silently is harmless for the
    // comment keywords this parser ignores anyway, but a dropped *first* FILE
    // line shifts every later FILE down one slot while tracks keep binding by
    // `files.len().saturating_sub(1)` -- so the sheet mounts, raises nothing,
    // and serves the wrong file's bytes. Stripping it once here, rather than
    // per line, is deliberate: a BOM is a stream marker and only the first
    // line can legitimately carry one.
    let cue = cue.strip_prefix('\u{feff}').unwrap_or(cue);

    for line in cue.lines() {
        let trimmed = line.trim();
        let mut words = trimmed.split_whitespace();
        let Some(keyword) = words.next() else {
            continue;
        };
        match keyword.to_ascii_uppercase().as_str() {
            "FILE" => {
                let rest = trimmed[keyword.len()..].trim_start();
                let (name, after) = if let Some(rest) = rest.strip_prefix('"') {
                    match rest.split_once('"') {
                        Some((name, after)) => (Some(name), after),
                        None => (None, ""),
                    }
                } else {
                    let mut words = rest.splitn(2, char::is_whitespace);
                    (words.next(), words.next().unwrap_or(""))
                };
                let name = name
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| format!("missing FILE name in '{line}'"))?;
                let file_type = CueFileType::parse(after.split_whitespace().next());
                files.push((name.to_string(), file_type));
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
