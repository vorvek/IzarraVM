//! Assemble a read-only FAT32 volume from a host folder.
//!
//! Unlike the FAT12 floppy (fat12.rs), which materializes the whole 1.44 MB
//! image, a FAT32 volume's data region is tens of megabytes even at the 65525
//! cluster floor. So this builds a sparse volume: it keeps the serialized FAT
//! and only the allocated clusters' bytes, and answers `read_sector` on demand.
//! Reserved gaps and unallocated clusters read back as zeros. That is the shape
//! the absolute-sector path (INT 25h/26h, AH=7305h) consumes one sector at a
//! time.
//!
//! Files and subdirectories under the input folder are laid down read-only. The
//! volume is read-mostly: there is no write-back to the host folder.

use crate::fat_name::unique_name;
use crate::fat32::{
    FAT_ATTR_DIRECTORY, FAT32_EOC, Fat32Geometry, Fat32Table, fat32_boot_sector, fat32_dir_entry,
    fat32_dot_entries, fat32_fsinfo_sector, fat32_geometry,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const SECTOR: usize = 512;
const DIR_ENTRY_SIZE: usize = 32;
/// A regular file's directory-entry attribute (archive bit).
const ATTR_ARCHIVE: u8 = 0x20;

/// A read-only synthesized FAT32 volume answered one 512-byte sector at a time.
#[derive(Debug)]
pub struct Fat32Volume {
    geo: Fat32Geometry,
    volume_id: u32,
    /// One serialized FAT (geo.fat_size_sectors * 512 bytes); mirrored on read.
    fat_bytes: Vec<u8>,
    /// Allocated clusters keyed by cluster number, each cluster-sized. Clusters
    /// not present read back as zeros.
    clusters: BTreeMap<u32, Vec<u8>>,
    /// FSInfo hints: free data clusters and the next free cluster number.
    free_count: u32,
    next_free: u32,
}

impl Fat32Volume {
    /// Total volume size in 512-byte sectors (BPB TotSec32).
    pub fn total_sectors(&self) -> u32 {
        self.geo.total_sectors
    }

    /// The computed geometry, for callers that need the BPB layout.
    pub fn geometry(&self) -> &Fat32Geometry {
        &self.geo
    }

    /// Read one 512-byte sector by absolute LBA. Out-of-range or unallocated
    /// sectors read as zeros.
    pub fn read_sector(&self, lba: u32) -> [u8; SECTOR] {
        let geo = &self.geo;
        // Boot sector (and its backup copy at BPB_BkBootSec).
        if lba == 0 || lba == u32::from(geo.backup_boot_sector) {
            return fat32_boot_sector(geo, self.volume_id);
        }
        // FSInfo at its sector, and the backup copy that rides the boot-record
        // backup at the same offset (fatgen103: the backup record is a full copy).
        let fsinfo_backup = u32::from(geo.backup_boot_sector) + u32::from(geo.fsinfo_sector);
        if lba == u32::from(geo.fsinfo_sector) || lba == fsinfo_backup {
            return fat32_fsinfo_sector(self.free_count, self.next_free);
        }
        // FAT region: num_fats identical copies, each fat_size_sectors long.
        let reserved = u32::from(geo.reserved_sectors);
        let fat_end = reserved + u32::from(geo.num_fats) * geo.fat_size_sectors;
        if (reserved..fat_end).contains(&lba) {
            let within = (lba - reserved) % geo.fat_size_sectors;
            return self.fat_slice(within as usize);
        }
        // Data region: cluster 2 begins at first_data_sector.
        if lba >= geo.first_data_sector {
            let data_lba = lba - geo.first_data_sector;
            let spc = u32::from(geo.sectors_per_cluster);
            let cluster = 2 + data_lba / spc;
            let sector_in_cluster = (data_lba % spc) as usize;
            if let Some(buf) = self.clusters.get(&cluster) {
                let off = sector_in_cluster * SECTOR;
                let mut out = [0u8; SECTOR];
                out.copy_from_slice(&buf[off..off + SECTOR]);
                return out;
            }
        }
        [0u8; SECTOR]
    }

    /// One sector of the FAT, zero-padded past the serialized entries.
    fn fat_slice(&self, sector: usize) -> [u8; SECTOR] {
        let mut out = [0u8; SECTOR];
        let off = sector * SECTOR;
        if let Some(slice) = self.fat_bytes.get(off..off + SECTOR) {
            out.copy_from_slice(slice);
        }
        out
    }
}

/// Mutable state threaded through the directory walk.
struct Builder {
    geo: Fat32Geometry,
    fat: Fat32Table,
    /// Next cluster number to hand out; starts at 2 (the root cluster).
    next_free: u32,
    clusters: BTreeMap<u32, Vec<u8>>,
    cluster_bytes: usize,
}

impl Builder {
    /// Free data clusters remaining.
    fn free_clusters(&self) -> u32 {
        (self.geo.count_of_clusters + 2).saturating_sub(self.next_free)
    }

    /// Hand out one fresh cluster, or None when the volume is full.
    fn alloc_one(&mut self) -> Option<u32> {
        if self.free_clusters() == 0 {
            return None;
        }
        let c = self.next_free;
        self.next_free += 1;
        Some(c)
    }

    /// Allocate `n` clusters, link them into a chain, terminate with EOC, and
    /// return the chain. None if there is not enough free space.
    fn alloc_chain(&mut self, n: u32) -> Option<Vec<u32>> {
        if n == 0 || self.free_clusters() < n {
            return None;
        }
        let chain: Vec<u32> = (0..n).map(|_| self.alloc_one().unwrap()).collect();
        for w in chain.windows(2) {
            self.fat.set(w[0], w[1]);
        }
        self.fat.set(*chain.last().unwrap(), FAT32_EOC);
        Some(chain)
    }

    /// Store `data` across a fresh cluster chain and return the first cluster.
    /// An empty file occupies no clusters and reports cluster 0. None on a full
    /// volume.
    fn store_file(&mut self, data: &[u8]) -> Option<u32> {
        if data.is_empty() {
            return Some(0);
        }
        let n = data.len().div_ceil(self.cluster_bytes) as u32;
        let chain = self.alloc_chain(n)?;
        for (i, &cl) in chain.iter().enumerate() {
            let start = i * self.cluster_bytes;
            let end = (start + self.cluster_bytes).min(data.len());
            let mut buf = vec![0u8; self.cluster_bytes];
            buf[..end - start].copy_from_slice(&data[start..end]);
            self.clusters.insert(cl, buf);
        }
        Some(chain[0])
    }

    /// Lay `entries` across `first_cluster`'s chain, extending it with more
    /// clusters when the entries overflow one cluster. The first cluster is
    /// already allocated (so a child's ".." can name it); this links any extra
    /// clusters and terminates the chain.
    fn store_dir(&mut self, first_cluster: u32, entries: &[[u8; DIR_ENTRY_SIZE]]) {
        let per_cluster = self.cluster_bytes / DIR_ENTRY_SIZE;
        let needed = entries.len().div_ceil(per_cluster).max(1);
        let mut chain = vec![first_cluster];
        for _ in 1..needed {
            match self.alloc_one() {
                Some(c) => chain.push(c),
                None => {
                    // Best-effort: the dropped entries' file/subdir clusters
                    // stay allocated (orphaned), harmless on a near-full volume.
                    eprintln!("fat32: out of space extending a directory; truncating it");
                    break;
                }
            }
        }
        for w in chain.windows(2) {
            self.fat.set(w[0], w[1]);
        }
        self.fat.set(*chain.last().unwrap(), FAT32_EOC);

        // Serialize the entries, then split into cluster-sized chunks. Entries
        // past the chain's capacity are dropped (logged above).
        let capacity = chain.len() * self.cluster_bytes;
        let mut flat = vec![0u8; capacity];
        for (i, e) in entries.iter().enumerate() {
            let off = i * DIR_ENTRY_SIZE;
            if off + DIR_ENTRY_SIZE <= capacity {
                flat[off..off + DIR_ENTRY_SIZE].copy_from_slice(e);
            }
        }
        for (i, &cl) in chain.iter().enumerate() {
            let start = i * self.cluster_bytes;
            self.clusters
                .insert(cl, flat[start..start + self.cluster_bytes].to_vec());
        }
    }

    /// Walk one host directory whose first cluster is `self_cluster`, emitting
    /// child files and subdirectories, then lay this directory's own entries
    /// across its chain. `parent_cluster` is what a subdir's ".." names (0 when
    /// the parent is the root). The root has no "." / ".." entries.
    fn build_dir(
        &mut self,
        dir: &Path,
        self_cluster: u32,
        parent_cluster: u32,
        is_root: bool,
    ) -> Result<(), String> {
        let mut entries: Vec<[u8; DIR_ENTRY_SIZE]> = Vec::new();
        if !is_root {
            let dots = fat32_dot_entries(self_cluster, parent_cluster);
            entries.push(dots[0..32].try_into().unwrap());
            entries.push(dots[32..64].try_into().unwrap());
        }
        let mut used_names: Vec<[u8; 11]> = Vec::new();

        let read = fs::read_dir(dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
        let mut children: Vec<_> = read.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        children.sort();

        for path in children {
            let raw = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if raw.is_empty() {
                continue;
            }
            let meta = match fs::metadata(&path) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("fat32: skipping {}: {e}", path.display());
                    continue;
                }
            };

            if meta.is_dir() {
                // Allocate this subdir's first cluster before recursing so its
                // children can point ".." back at it.
                let Some(child_cluster) = self.alloc_one() else {
                    eprintln!("fat32: out of space, skipping directory {}", path.display());
                    continue;
                };
                // A child of the root names cluster 0 in its ".." (fatgen103
                // 6.5), not the root's real cluster 2.
                let parent_for_child = if is_root { 0 } else { self_cluster };
                self.build_dir(&path, child_cluster, parent_for_child, false)?;
                let name = unique_name(&path, true, &mut used_names);
                entries.push(fat32_dir_entry(
                    &name,
                    FAT_ATTR_DIRECTORY,
                    child_cluster,
                    0,
                    0,
                    0,
                ));
            } else if meta.is_file() {
                let data = match fs::read(&path) {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("fat32: skipping {}: {e}", path.display());
                        continue;
                    }
                };
                let Some(head) = self.store_file(&data) else {
                    eprintln!(
                        "fat32: out of space, skipping {} ({} bytes)",
                        path.display(),
                        data.len()
                    );
                    continue;
                };
                let name = unique_name(&path, false, &mut used_names);
                entries.push(fat32_dir_entry(
                    &name,
                    ATTR_ARCHIVE,
                    head,
                    0,
                    0,
                    data.len() as u32,
                ));
            }
        }

        self.store_dir(self_cluster, &entries);
        Ok(())
    }
}

/// Assemble a read-only FAT32 volume of `volume_bytes` from the files under
/// `root`, with `volume_id` as the serial. 8.3 names are folded through the
/// shared name rules (uppercase, illegal stripped, `~n` on collision). Files or
/// directories that overflow the free space are skipped with a log line.
///
/// Ceiling: read-only. Guest writes are not synced back to the host folder.
pub fn build_fat32(root: &Path, volume_bytes: u64, volume_id: u32) -> Result<Fat32Volume, String> {
    let geo = fat32_geometry(volume_bytes)
        .ok_or_else(|| format!("{volume_bytes} bytes is not a valid FAT32 size"))?;
    let cluster_bytes = usize::from(geo.sectors_per_cluster) * SECTOR;
    let mut b = Builder {
        geo,
        fat: Fat32Table::new(&geo),
        next_free: 2,
        clusters: BTreeMap::new(),
        cluster_bytes,
    };

    // Reserve cluster 2 for the root, then walk it.
    let root_cluster = b
        .alloc_one()
        .ok_or("volume has no room for a root cluster")?;
    debug_assert_eq!(root_cluster, geo.root_cluster);
    b.build_dir(root, root_cluster, 0, true)?;

    let free_count = b.free_clusters();
    let next_free = b.next_free;
    Ok(Fat32Volume {
        geo,
        volume_id,
        fat_bytes: b.fat.to_bytes(&geo),
        clusters: b.clusters,
        free_count,
        next_free,
    })
}

#[cfg(test)]
#[path = "fat32_volume_test.rs"]
mod tests;
