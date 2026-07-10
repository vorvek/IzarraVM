//! Per-directory 8.3 name table for the Katea host-folder facade. One instance
//! per directory (FAT 8.3 uniqueness is per-directory). It owns the folding (via
//! `fat_name`) and a bidirectional record so a folded name maps back to its host
//! path — the read side uses the folded name for the directory entry, the write
//! side uses the reverse map.

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
    // Limit: superseded for writes by `KateaTreeVolume::dir_paths` +
    // `existing_files` (seeded once at mount, no O(n) scan). Retained for a future
    // delete/rename milestone, which needs name->host-path resolution.
    #[allow(dead_code)]
    pub(crate) fn host_path(&self, name: &[u8; 11]) -> Option<&PathBuf> {
        self.map.iter().find(|(n, _)| n == name).map(|(_, p)| p)
    }
}

#[cfg(test)]
#[path = "katea_names_test.rs"]
mod tests;
