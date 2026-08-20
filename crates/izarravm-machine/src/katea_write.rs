// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Pure write-interpretation helpers for the Katea write engine. These parse
//! the guest's own directory + FAT bytes (no INT 21h, no DOS internals) so the
//! reconcile pass in `katea_tree.rs` can decide what finished files to mirror to
//! the host folder. Everything here is pure except `atomic_write`.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::Path;

/// FAT subdirectory attribute; volume-label bit; LFN attribute.
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_VOLUME_LABEL: u8 = 0x08;
const ATTR_LFN: u8 = 0x0F;

/// A parsed 32-byte directory entry we might act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DirEntry {
    pub name: [u8; 11],
    pub attr: u8,
    pub first_cluster: u32,
    pub size: u32,
}

/// What to do with one entry. `Skip` covers dot/dotdot, LFN, volume label, free,
/// deleted, and system files. Delete and rename are handled elsewhere, so a vanished or
/// re-pointed entry is never destructive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EntryAction {
    Skip,
    MakeDir {
        name: [u8; 11],
        first_cluster: u32,
    },
    MakeFile {
        name: [u8; 11],
        first_cluster: u32,
        size: u32,
    },
}

/// Parse a directory's concatenated cluster bytes into entries, stopping at the
/// first free (`0x00`) entry (the FAT convention for "no further entries").
/// Deleted (`0xE5`) entries are dropped here.
pub(crate) fn parse_dir(bytes: &[u8]) -> Vec<DirEntry> {
    let mut out = Vec::new();
    for e in bytes.as_chunks::<32>().0 {
        match e[0] {
            0x00 => break,
            0xE5 => continue,
            _ => {}
        }
        let first_cluster = (u16::from_le_bytes([e[20], e[21]]) as u32) << 16
            | u16::from_le_bytes([e[26], e[27]]) as u32;
        let size = u32::from_le_bytes([e[28], e[29], e[30], e[31]]);
        let mut name = [0u8; 11];
        name.copy_from_slice(&e[0..11]);
        out.push(DirEntry {
            name,
            attr: e[11],
            first_cluster,
            size,
        });
    }
    out
}

/// Bytes fatgen103 forbids in a short name, plus the separators and the wildcards.
/// A directory entry is guest bytes: nothing stops a guest from writing `A\..\..\`
/// into one, and the reconcile pass turns a short name straight into a host path
/// under the mounted folder. `Path::join` on such a name walks out of the mount,
/// so the illegal bytes are rejected here -- at the one point every host path in
/// this engine is derived from -- rather than at each of the join sites.
/// Rejecting these can never orphan a name Katea itself synthesized: `fat_name`
/// builds every mounted short name from a strict allowlist that shares no byte
/// with this set, so only a name the guest invented can land here.
const ILLEGAL_83_BYTES: &[u8] = b"\"*+,/:;<=>?[\\]|.";

/// Whether this short name is safe to turn into a host path inside the mount.
pub(crate) fn name_is_host_safe(name: &[u8; 11]) -> bool {
    !name
        .iter()
        .any(|b| *b < 0x20 || *b == 0x7F || ILLEGAL_83_BYTES.contains(b))
}

/// Classify an entry. `system` is the set of folded 8.3 names that must never be
/// materialized (the InMemory boot files). Conservative: anything ambiguous Skips.
pub(crate) fn classify(e: &DirEntry, system: &HashSet<[u8; 11]>) -> EntryAction {
    if e.name[0] == b'.' || e.name[0] == b' ' {
        return EntryAction::Skip; // `.` / `..` or blank name
    }
    if e.attr & ATTR_LFN == ATTR_LFN || e.attr & ATTR_VOLUME_LABEL != 0 {
        return EntryAction::Skip; // LFN fragment or volume label
    }
    if !name_is_host_safe(&e.name) {
        return EntryAction::Skip; // would escape the mount or is not a FAT name
    }
    if system.contains(&e.name) {
        return EntryAction::Skip;
    }
    if e.attr & ATTR_DIRECTORY != 0 {
        if e.first_cluster < 2 {
            return EntryAction::Skip; // a directory must name a real cluster
        }
        return EntryAction::MakeDir {
            name: e.name,
            first_cluster: e.first_cluster,
        };
    }
    EntryAction::MakeFile {
        name: e.name,
        first_cluster: e.first_cluster,
        size: e.size,
    }
}

/// Follow a cluster chain `first -> EOC` using `fat_entry`. Returns the ordered
/// clusters, or `None` if the chain hits a free/reserved entry or fails to
/// terminate within `max` clusters (corrupt/cyclic FAT) — the caller then holds.
/// A `first` below 2 yields an empty chain (a legitimately empty file).
/// `FnMut`, not `Fn`: the reconcile path resolves entries through a per-walk FAT
/// sector cursor (`katea_tree::FatWalk`), which the closure has to be able to
/// update. Every existing caller passes a closure that borrows shared state and
/// is unaffected.
pub(crate) fn chain(
    first: u32,
    max: usize,
    mut fat_entry: impl FnMut(u32) -> u32,
) -> Option<Vec<u32>> {
    if first < 2 {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    let mut c = first;
    for _ in 0..max {
        out.push(c);
        let next = fat_entry(c) & 0x0FFF_FFFF;
        if next >= 0x0FFF_FFF8 {
            return Some(out); // EOC
        }
        if next < 2 {
            return None; // free/reserved mid-chain: incomplete, hold
        }
        c = next;
    }
    None // didn't terminate: corrupt, hold
}

/// A cheap content fingerprint so an unchanged file is not re-written every pass.
/// A same-length overwrite changes content -> changes the fingerprint -> rewrites.
/// `DefaultHasher` is not stable across toolchains, which is fine: the fingerprint
/// cache is session-only and never persisted, so values are only ever compared
/// within one process run. Do not lift this into a persistent store as-is.
pub(crate) fn fingerprint(data: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut h);
    h.finish()
}

/// Write `data` to `path` atomically: a temp file in the same directory, then a
/// rename over the target (replaces an existing file on win32 and unix). On any
/// error the original target is left untouched (the temp is best-effort removed).
pub(crate) fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let file_name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("katea: materialize target has no file name"))?;
    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(".kattmp");
    let tmp = path.with_file_name(&tmp_name);
    std::fs::write(&tmp, data)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
#[path = "katea_write_test.rs"]
mod tests;
