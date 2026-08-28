// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Read-side ISO9660 index for the IzarraCD host redirector.
//!
//! The redirector answers DOS file operations on the CD drive from the host,
//! so it needs the disc's directory tree in a host structure instead of
//! re-scanning sectors per call the way a guest redirector does. `build`
//! parses the whole tree once per mounted medium; the owner caches the result
//! keyed on the ATAPI `media_generation` counter.
//!
//! Names are held in the FCB form DOS searches use: 11 bytes, 8+3,
//! space-padded, upper case, with the ISO `;version` suffix stripped. A name
//! that does not fit 8.3 is truncated field-by-field, which matches what a
//! period CD extension showed for out-of-profile names. This is the same
//! namespace `iso9660.rs` (the folder-mount image *builder*) emits, so a
//! folder-mounted disc round-trips exactly.

use crate::cdimage::{CdImage, DATA_SECTOR};

/// One directory entry, pre-converted to the DOS-facing form.
#[derive(Debug, Clone)]
pub(crate) struct IsoEntry {
    /// FCB-form name: 8 name bytes then 3 extension bytes, space-padded.
    pub name: [u8; 11],
    /// DOS attribute byte: 0x10 directory, 0x02 hidden.
    pub attr: u8,
    /// First LBA of the file or directory extent.
    pub lba: u32,
    /// Extent length in bytes.
    pub size: u32,
    /// DOS-packed modification time and date from the ISO recording stamp.
    pub dos_time: u16,
    pub dos_date: u16,
    /// Index into `IsoIndex::dirs` when this entry is a directory.
    pub subdir: Option<u32>,
}

impl IsoEntry {
    pub(crate) fn is_dir(&self) -> bool {
        self.attr & 0x10 != 0
    }
}

/// One parsed directory: its entries in disc order, `.`/`..` excluded.
#[derive(Debug, Default)]
pub(crate) struct IsoDir {
    pub entries: Vec<IsoEntry>,
    /// First LBA of this directory's own extent — the redirector's FindNext
    /// cursor names its directory by this value.
    pub extent_lba: u32,
}

/// The parsed directory tree of one mounted medium. `dirs[0]` is the root.
#[derive(Debug)]
pub(crate) struct IsoIndex {
    pub dirs: Vec<IsoDir>,
    /// Total sectors from the PVD volume-space field, for GetSpace.
    pub volume_sectors: u32,
    /// The first 11 bytes of the PVD volume identifier, space-padded: the
    /// DOS volume label a FindFirst with attribute 08h returns.
    pub volume_label: [u8; 11],
    /// Directory index by extent LBA. The redirector's FindNext cursor names
    /// its directory by LBA, so a resumed search re-finds it here.
    dir_by_lba: std::collections::HashMap<u32, u32>,
}

/// Upper bound on parsed directories. A real disc holds a few thousand at
/// most; the bound stops a corrupt image from looping the parser.
const MAX_DIRS: usize = 65_536;

impl IsoIndex {
    /// Parse the mounted medium's primary volume descriptor and directory
    /// tree. `None` when there is no PVD (audio-only or corrupt media).
    pub(crate) fn build(image: &CdImage) -> Option<IsoIndex> {
        let pvd = image.read_data_sector(16)?;
        if pvd[0] != 0x01 || &pvd[1..6] != b"CD001" {
            return None;
        }
        let volume_sectors = u32::from_le_bytes(pvd[80..84].try_into().unwrap());
        let root_len = usize::from(pvd[156]);
        if root_len < 34 || 156 + root_len > pvd.len() {
            return None;
        }
        let root_lba = u32::from_le_bytes(pvd[158..162].try_into().unwrap());
        let root_size = u32::from_le_bytes(pvd[166..170].try_into().unwrap());
        let mut volume_label = [b' '; 11];
        volume_label.copy_from_slice(&pvd[40..51]);

        let mut index = IsoIndex {
            dirs: vec![IsoDir {
                entries: Vec::new(),
                extent_lba: root_lba,
            }],
            volume_sectors,
            volume_label,
            dir_by_lba: std::collections::HashMap::new(),
        };
        index.dir_by_lba.insert(root_lba, 0);
        // Depth-first over (dir index, extent LBA, extent size). Every
        // directory is entered through exactly one parent record on a valid
        // disc. A directory entry whose extent is already known — `.`, `..`,
        // or a doubly-referenced extent on a malformed disc — is listed but
        // not recursed into, which is also what keeps the walk finite.
        let mut queue = vec![(0u32, root_lba, root_size)];
        while let Some((dir_idx, lba, size)) = queue.pop() {
            let entries = parse_directory(image, lba, size, dir_idx != 0);
            let mut parsed = Vec::with_capacity(entries.len());
            for mut entry in entries {
                if entry.is_dir()
                    && index.dirs.len() < MAX_DIRS
                    && !index.dir_by_lba.contains_key(&entry.lba)
                {
                    let child_idx = index.dirs.len() as u32;
                    index.dirs.push(IsoDir {
                        entries: Vec::new(),
                        extent_lba: entry.lba,
                    });
                    index.dir_by_lba.insert(entry.lba, child_idx);
                    queue.push((child_idx, entry.lba, entry.size));
                    entry.subdir = Some(child_idx);
                }
                parsed.push(entry);
            }
            index.dirs[dir_idx as usize].entries = parsed;
        }
        Some(index)
    }

    /// The directory whose extent starts at `lba`.
    pub(crate) fn dir_by_lba(&self, lba: u32) -> Option<&IsoDir> {
        self.dir_by_lba
            .get(&lba)
            .map(|&idx| &self.dirs[idx as usize])
    }

    /// Resolve a DOS path (`\GAME\DATA.BIN`, leading drive prefix and either
    /// separator accepted) to its entry. The root itself has no entry, so the
    /// empty path returns `None`; `lookup_dir` serves the root case.
    pub(crate) fn lookup(&self, path: &str) -> Option<&IsoEntry> {
        let mut components = normalized_components(path);
        let last = components.pop()?;
        let dir = self.dir_at(&components)?;
        let target = fcb_name(last.as_bytes());
        dir.entries.iter().find(|entry| entry.name == target)
    }

    /// Resolve a DOS path to a directory (the root for an empty path).
    pub(crate) fn lookup_dir(&self, path: &str) -> Option<&IsoDir> {
        self.dir_at(&normalized_components(path))
    }

    fn dir_at(&self, components: &[String]) -> Option<&IsoDir> {
        let mut dir = &self.dirs[0];
        for component in components {
            let target = fcb_name(component.as_bytes());
            let entry = dir
                .entries
                .iter()
                .find(|entry| entry.name == target && entry.is_dir())?;
            dir = &self.dirs[entry.subdir? as usize];
        }
        Some(dir)
    }
}

fn normalized_components(path: &str) -> Vec<String> {
    let mut normalized = path.replace('/', "\\");
    if normalized.as_bytes().get(1) == Some(&b':') {
        normalized.drain(..2);
    }
    normalized
        .split('\\')
        .filter(|part| !part.is_empty() && *part != ".")
        .map(str::to_string)
        .collect()
}

/// Parse one directory extent. `dot_entries` selects whether the ISO self
/// and parent records (ids 00h/01h) become `.` and `..` entries — a
/// subdirectory lists them, the way DOS and the IZCDEX redirector did; the
/// root does not.
fn parse_directory(image: &CdImage, lba: u32, size: u32, dot_entries: bool) -> Vec<IsoEntry> {
    let mut entries = Vec::new();
    let sectors = size.div_ceil(DATA_SECTOR as u32);
    for sector_index in 0..sectors {
        let Some(sector) = image.read_data_sector(lba + sector_index) else {
            break;
        };
        let mut offset = 0usize;
        while offset < sector.len() {
            let record_len = usize::from(sector[offset]);
            if record_len == 0 {
                // Records never straddle sectors; a zero length byte pads to
                // the next sector.
                break;
            }
            if offset + record_len > sector.len() || record_len < 34 {
                break;
            }
            let record = &sector[offset..offset + record_len];
            offset += record_len;
            let id_len = usize::from(record[32]);
            if id_len == 0 || 33 + id_len > record.len() {
                continue;
            }
            let id = &record[33..33 + id_len];
            let dot_name = match id {
                [0x00] if dot_entries => *b".          ",
                [0x01] if dot_entries => *b"..         ",
                [0x00] | [0x01] => continue, // the root lists neither
                _ => [0u8; 11],
            };
            let flags = record[25];
            // DOS attributes the way the IZCDEX redirector mapped them: ISO
            // DIR -> subdirectory, ISO HIDDEN (existence) -> hidden, and every
            // non-directory carries read-only (a CD file cannot be written).
            let is_dir = flags & 0x02 != 0;
            let name = if dot_name[0] == 0 {
                fcb_name(split_name(id))
            } else {
                dot_name
            };
            entries.push(IsoEntry {
                name,
                attr: (u8::from(is_dir) << 4)
                    | (u8::from(flags & 0x01 != 0) << 1)
                    | u8::from(!is_dir),
                lba: u32::from_le_bytes(record[2..6].try_into().unwrap()),
                size: u32::from_le_bytes(record[10..14].try_into().unwrap()),
                dos_time: dos_time(record),
                dos_date: dos_date(record),
                subdir: None,
            });
        }
    }
    entries
}

/// Strip the ISO `;version` suffix from a file identifier.
fn split_name(id: &[u8]) -> &[u8] {
    match id.iter().position(|&b| b == b';') {
        Some(at) => &id[..at],
        None => id,
    }
}

/// Convert an ISO file identifier (version already stripped) to the 11-byte
/// FCB search form: 8 name + 3 extension, upper case, space-padded, each
/// field truncated when over-long. A trailing `.` (directory-style empty
/// extension) drops away.
pub(crate) fn fcb_name(id: &[u8]) -> [u8; 11] {
    let mut out = [b' '; 11];
    let (base, ext) = match id.iter().rposition(|&b| b == b'.') {
        Some(at) => (&id[..at], &id[at + 1..]),
        None => (id, &id[..0]),
    };
    for (slot, &b) in out[..8].iter_mut().zip(base.iter()) {
        *slot = b.to_ascii_uppercase();
    }
    for (slot, &b) in out[8..].iter_mut().zip(ext.iter()) {
        *slot = b.to_ascii_uppercase();
    }
    out
}

/// Convert a search pattern to the 11-byte FCB template form: like
/// `fcb_name`, and `*` fills the rest of its field with `?` wildcards.
pub(crate) fn fcb_pattern(pattern: &[u8]) -> [u8; 11] {
    let mut out = [b' '; 11];
    let (base, ext) = match pattern.iter().rposition(|&b| b == b'.') {
        Some(at) => (&pattern[..at], &pattern[at + 1..]),
        None => (pattern, &pattern[..0]),
    };
    let (name_field, ext_field) = out.split_at_mut(8);
    for (field, source) in [(name_field, base), (ext_field, ext)] {
        for (i, &b) in source.iter().enumerate() {
            if i == field.len() {
                break;
            }
            if b == b'*' {
                field[i..].fill(b'?');
                break;
            }
            field[i] = b.to_ascii_uppercase();
        }
    }
    out
}

/// DOS packed time from the 7-byte ISO recording stamp at record offset 18.
fn dos_time(record: &[u8]) -> u16 {
    let hour = u16::from(record[21].min(23));
    let minute = u16::from(record[22].min(59));
    let second = u16::from(record[23].min(59));
    (hour << 11) | (minute << 5) | (second / 2)
}

/// DOS packed date from the same stamp. ISO years count from 1900; DOS dates
/// from 1980, clamped at both ends.
fn dos_date(record: &[u8]) -> u16 {
    let year = u16::from(record[18]).saturating_sub(80).min(127);
    let month = u16::from(record[19].clamp(1, 12));
    let day = u16::from(record[20].clamp(1, 31));
    (year << 9) | (month << 5) | day
}

#[cfg(test)]
#[path = "cdiso_test.rs"]
mod cdiso_tests;
