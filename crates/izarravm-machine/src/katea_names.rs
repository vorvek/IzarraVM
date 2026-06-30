//! Per-directory 8.3 name table for the Katea host-folder facade. One instance
//! per directory (FAT 8.3 uniqueness is per-directory). It owns the folding (via
//! `fat_name`) and a bidirectional record so a folded name maps back to its host
//! path — the read side uses the folded name for the directory entry, the write
//! side (M2) uses the reverse map.

use crate::fat_name;
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub(crate) struct NameTable {
    used: Vec<[u8; 11]>,
    map: Vec<([u8; 11], PathBuf)>, // folded 8.3 -> host path, insertion order
}

impl NameTable {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Reserve a name (e.g. a system file) so later host names collide off it.
    pub(crate) fn reserve(&mut self, name: [u8; 11]) {
        if !self.used.contains(&name) {
            self.used.push(name);
        }
    }

    /// Fold a host path to a unique 8.3 name in this directory and record the
    /// reverse mapping. `is_dir` keeps a dotted directory name whole.
    pub(crate) fn add_host(&mut self, path: &Path, is_dir: bool) -> [u8; 11] {
        let name = fat_name::unique_name(path, is_dir, &mut self.used);
        self.map.push((name, path.to_path_buf()));
        name
    }

    /// Reverse lookup: the host path a folded name came from, if any.
    // Limit: superseded for M2's write path by `KateaTreeVolume::dir_paths` +
    // `existing_files` (seeded once at mount, no O(n) scan). Retained for a future
    // delete/rename milestone, which needs name->host-path resolution.
    #[allow(dead_code)]
    pub(crate) fn host_path(&self, name: &[u8; 11]) -> Option<&PathBuf> {
        self.map.iter().find(|(n, _)| n == name).map(|(_, p)| p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn folds_files_and_dirs_and_resolves_collisions_and_reverse_lookup() {
        let mut t = NameTable::new();
        // Reserve a system name so a host file can't alias it.
        t.reserve(*b"KERNEL  SYS");
        let a = t.add_host(&PathBuf::from("/h/Readme.txt"), false);
        let b = t.add_host(&PathBuf::from("/h/readme.txt"), false); // collides
        let d = t.add_host(&PathBuf::from("/h/My.Games"), true); // a directory
        assert_eq!(&a, b"README  TXT");
        assert_eq!(&b, b"README~1TXT");
        assert_eq!(&d, b"MYGAMES    ");
        // Reverse: the folded name maps back to the host path (for M2 writes).
        assert_eq!(t.host_path(&a), Some(&PathBuf::from("/h/Readme.txt")));
        assert_eq!(t.host_path(b"KERNEL  SYS"), None); // a reserved name has no host path
    }
}
