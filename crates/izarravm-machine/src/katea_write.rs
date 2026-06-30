//! Pure write-interpretation helpers for the Katea M2 write engine. These parse
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
/// deleted, and system files. Delete/rename are out of M2 scope, so a vanished or
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
    for e in bytes.chunks_exact(32) {
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

/// Classify an entry. `system` is the set of folded 8.3 names that must never be
/// materialized (the InMemory boot files). Conservative: anything ambiguous Skips.
pub(crate) fn classify(e: &DirEntry, system: &HashSet<[u8; 11]>) -> EntryAction {
    if e.name[0] == b'.' || e.name[0] == b' ' {
        return EntryAction::Skip; // `.` / `..` or blank name
    }
    if e.attr & ATTR_LFN == ATTR_LFN || e.attr & ATTR_VOLUME_LABEL != 0 {
        return EntryAction::Skip; // LFN fragment or volume label
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
pub(crate) fn chain(first: u32, max: usize, fat_entry: impl Fn(u32) -> u32) -> Option<Vec<u32>> {
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
mod tests {
    use super::*;
    use crate::fat32::{FAT32_EOC, fat32_dir_entry};
    use std::collections::HashSet;

    fn name83(s: &str) -> [u8; 11] {
        let mut n = [b' '; 11];
        let (b, x) = s.split_once('.').unwrap_or((s, ""));
        n[..b.len()].copy_from_slice(b.as_bytes());
        n[8..8 + x.len()].copy_from_slice(x.as_bytes());
        n
    }

    #[test]
    fn parse_dir_reads_entries_and_stops_at_free() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&fat32_dir_entry(&name83("A.TXT"), 0x20, 7, 0, 0, 123));
        bytes.extend_from_slice(&fat32_dir_entry(&name83("SUB"), 0x10, 9, 0, 0, 0));
        bytes.extend_from_slice(&[0u8; 32]); // free -> stop
        bytes.extend_from_slice(&fat32_dir_entry(&name83("Z.TXT"), 0x20, 99, 0, 0, 1));
        let es = parse_dir(&bytes);
        assert_eq!(es.len(), 2, "stops at the free entry");
        assert_eq!(es[0].name, name83("A.TXT"));
        assert_eq!(es[0].first_cluster, 7);
        assert_eq!(es[0].size, 123);
        assert_eq!(es[1].first_cluster, 9);
        assert_eq!(es[1].attr & 0x10, 0x10);
    }

    #[test]
    fn classify_skips_dots_lfn_volume_and_system_files() {
        let mut sys = HashSet::new();
        sys.insert(name83("KERNEL.SYS"));
        let mk = |n: &str, attr: u8, fc: u32, sz: u32| DirEntry {
            name: name83(n),
            attr,
            first_cluster: fc,
            size: sz,
        };
        // A real FAT `.` / `..` entry has name[0] == 0x2E (the test helper's
        // `name83(".")` would yield an all-spaces name, which is NOT what reconcile
        // sees from live directory clusters), so build the dot bytes explicitly.
        let dot = DirEntry {
            name: {
                let mut n = [b' '; 11];
                n[0] = b'.';
                n
            },
            attr: 0x10,
            first_cluster: 2,
            size: 0,
        };
        let dotdot = DirEntry {
            name: {
                let mut n = [b' '; 11];
                n[0] = b'.';
                n[1] = b'.';
                n
            },
            attr: 0x10,
            first_cluster: 2,
            size: 0,
        };
        assert_eq!(classify(&dot, &sys), EntryAction::Skip, "real `.` entry");
        assert_eq!(
            classify(&dotdot, &sys),
            EntryAction::Skip,
            "real `..` entry"
        );
        // A blank first byte (a malformed/empty slot) is skipped defensively.
        assert_eq!(
            classify(&mk(" ", 0x20, 9, 1), &sys),
            EntryAction::Skip,
            "blank name"
        );
        assert_eq!(classify(&mk("X", 0x0F, 0, 0), &sys), EntryAction::Skip); // LFN
        assert_eq!(classify(&mk("LABEL", 0x08, 0, 0), &sys), EntryAction::Skip); // vol label
        assert_eq!(
            classify(&mk("KERNEL.SYS", 0x20, 3, 9), &sys),
            EntryAction::Skip
        );
        assert_eq!(
            classify(&mk("GAMES", 0x10, 5, 0), &sys),
            EntryAction::MakeDir {
                name: name83("GAMES"),
                first_cluster: 5
            }
        );
        assert_eq!(
            classify(&mk("NEW.TXT", 0x20, 7, 11), &sys),
            EntryAction::MakeFile {
                name: name83("NEW.TXT"),
                first_cluster: 7,
                size: 11
            }
        );
    }

    #[test]
    fn chain_follows_to_eoc_and_holds_on_break() {
        // 5 -> 6 -> EOC.
        let fat = |c: u32| match c {
            5 => 6,
            6 => FAT32_EOC,
            _ => 0,
        };
        assert_eq!(chain(5, 16, fat), Some(vec![5, 6]));
        // A free entry mid-chain -> hold (None).
        let broken = |c: u32| if c == 5 { 0 } else { FAT32_EOC };
        assert_eq!(chain(5, 16, broken), None);
        // first < 2 -> empty chain (empty file).
        assert_eq!(chain(0, 16, fat), Some(Vec::new()));
        // Non-terminating -> hold.
        let loopy = |_c: u32| 5u32;
        assert_eq!(chain(5, 8, loopy), None);
    }

    #[test]
    fn fingerprint_differs_on_same_length_content_change() {
        assert_ne!(fingerprint(b"line1\r\n"), fingerprint(b"line2\r\n"));
        assert_eq!(fingerprint(b"abc"), fingerprint(b"abc"));
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join(format!("katea_aw_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("OUT.TXT");
        std::fs::write(&target, b"old").unwrap();
        atomic_write(&target, b"new content").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new content");
        // No stray temp file remains.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains("kattmp"))
            .collect();
        assert!(leftovers.is_empty(), "no .kattmp left behind");
        std::fs::remove_dir_all(&dir).ok();
    }
}
