//! Recursive, read-only, lazy host-folder directory tree for the Katea
//! controller. Generalizes the flat `KateaVolume` (M0) into a full FAT32
//! directory tree whose FAT and directory sectors are computed on demand, so
//! RAM scales with the entry count rather than the disk or file sizes.

// Limit: this is the first M1 task — the tree model + the metadata-only walk.
// The cluster fields (`first_cluster`/`cluster_count`/`parent_first_cluster`)
// and the types/`build_tree` are exercised only by the Task 3 unit test so far;
// cluster allocation, computed FAT/dir sectors, and `KateaTreeVolume` (the
// non-test consumers) land in the later M1 tasks. Removed as those tasks wire
// these up.
#![allow(dead_code)]

use crate::katea_names::NameTable;
use crate::katea_volume::FileSource;
use std::path::Path;

/// Cap recursion so a pathological tree (or an undetected loop) can't run away;
/// also roughly the depth DOS's 64-char path limit allows.
const MAX_DEPTH: usize = 32;

#[derive(Debug)]
pub(crate) struct TreeFile {
    pub name: [u8; 11],
    pub source: FileSource, // InMemory (system files) or HostFile (lazy)
    pub first_cluster: u32, // assigned in Task 4
    pub cluster_count: u32,
}

#[derive(Debug)]
pub(crate) struct TreeSubdir {
    pub name: [u8; 11],
    pub dir: TreeDir,
}

#[derive(Debug, Default)]
pub(crate) struct TreeDir {
    pub files: Vec<TreeFile>,
    pub subdirs: Vec<TreeSubdir>,
    pub first_cluster: u32, // this directory's first cluster (root = 2)
    pub cluster_count: u32,
    pub parent_first_cluster: u32, // for `..`; 0 when the parent is the root
}

#[derive(Debug)]
pub(crate) struct HostTree {
    pub root: TreeDir,
}

/// Build the tree from a host folder, overlaying the in-memory system files at
/// the root first (so the disk still boots). Metadata only — never reads host
/// file contents. Cluster fields are zero here; Task 4 assigns them.
pub(crate) fn build_tree(root: &Path, system_files: &[(String, Vec<u8>)]) -> HostTree {
    let mut names = NameTable::new();
    let mut dir = TreeDir::default();

    // System files first, with their canonical 8.3 names reserved.
    for (name, bytes) in system_files {
        let n = fold_literal_83(name);
        names.reserve(n);
        dir.files.push(TreeFile {
            name: n,
            source: FileSource::InMemory(bytes.clone()),
            first_cluster: 0,
            cluster_count: 0,
        });
    }

    walk_into(root, &mut dir, &mut names, 1);
    HostTree { root: dir }
}

fn walk_into(host: &Path, dir: &mut TreeDir, names: &mut NameTable, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    let mut entries: Vec<std::fs::DirEntry> = match std::fs::read_dir(host) {
        Ok(rd) => rd.filter_map(Result::ok).collect(),
        Err(_) => return,
    };
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for e in entries {
        // metadata (not symlink_metadata): we already skip symlinks below.
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_symlink() {
            continue; // ponytail: no symlink following in M1 (loop-safe)
        }
        let path = e.path();
        if ft.is_dir() {
            let name = names.add_host(&path, true);
            let mut child = TreeDir::default();
            let mut child_names = NameTable::new(); // a fresh table per directory
            walk_into_with(&path, &mut child, &mut child_names, depth + 1);
            dir.subdirs.push(TreeSubdir { name, dir: child });
        } else if ft.is_file() {
            let Ok(md) = e.metadata() else { continue };
            let name = names.add_host(&path, false);
            dir.files.push(TreeFile {
                name,
                source: FileSource::HostFile {
                    path,
                    len: md.len(),
                },
                first_cluster: 0,
                cluster_count: 0,
            });
        }
        // Non-regular (device/fifo/etc.) is neither dir nor file -> skipped.
    }
}

// `walk_into` is the root entry (names already holds the system reservations);
// subdirectories get their own NameTable via this helper.
fn walk_into_with(host: &Path, dir: &mut TreeDir, names: &mut NameTable, depth: usize) {
    walk_into(host, dir, names, depth);
}

/// Fold a known-valid 8.3 system file name like "KERNEL.SYS" to the 11-byte
/// field (split on the dot, uppercase, space-pad). The caller guarantees 8.3.
fn fold_literal_83(name: &str) -> [u8; 11] {
    let mut out = [b' '; 11];
    let (base, ext) = name.split_once('.').unwrap_or((name, ""));
    let b = base.as_bytes();
    let x = ext.as_bytes();
    out[..b.len().min(8)].copy_from_slice(&b[..b.len().min(8)]);
    out[8..8 + x.len().min(3)].copy_from_slice(&x[..x.len().min(3)]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> std::path::PathBuf {
        // A unique temp dir; the test cleans it up at the end.
        let p = std::env::temp_dir().join(format!("katea_tree_{}_{name}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn walks_a_host_folder_into_a_tree_metadata_only_skipping_non_files() {
        let root = scratch("walk");
        fs::write(root.join("hello.txt"), b"hi").unwrap();
        fs::create_dir_all(root.join("GAMES/HELLO")).unwrap();
        fs::write(root.join("GAMES/HELLO/HELLO.COM"), vec![0u8; 600]).unwrap();

        // Two in-memory "system" files overlaid at the root (as mount does).
        let sys = vec![
            ("KERNEL.SYS".to_string(), vec![0xEBu8; 70]),
            ("COMMAND.COM".to_string(), vec![0u8; 50]),
        ];
        let tree = build_tree(&root, &sys);

        // Root: 2 system files + hello.txt + the GAMES subdir.
        assert_eq!(tree.root.files.len(), 3, "2 system + hello.txt");
        assert_eq!(tree.root.subdirs.len(), 1, "GAMES");
        assert_eq!(&tree.root.subdirs[0].name, b"GAMES      ");

        // GAMES -> HELLO -> HELLO.COM, len read from metadata (not contents).
        let games = &tree.root.subdirs[0].dir;
        assert_eq!(games.subdirs.len(), 1);
        let hello = &games.subdirs[0].dir;
        assert_eq!(hello.files.len(), 1);
        assert_eq!(&hello.files[0].name, b"HELLO   COM");
        assert_eq!(hello.files[0].source.len(), 600);

        fs::remove_dir_all(&root).ok();
    }
}
