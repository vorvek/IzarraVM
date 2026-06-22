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
        // Boot sector (and its backup copy).
        if lba == 0 || lba == u32::from(geo.backup_boot_sector) {
            return fat32_boot_sector(geo, self.volume_id);
        }
        if lba == u32::from(geo.fsinfo_sector) {
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
mod tests {
    use super::*;

    /// A small but valid FAT32 size (64 MB) for tests.
    const TEST_BYTES: u64 = 64 * 1024 * 1024;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "fat32_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Read a directory's entries by following its FAT cluster chain.
    fn read_dir_entries(vol: &Fat32Volume, first_cluster: u32) -> Vec<[u8; DIR_ENTRY_SIZE]> {
        let geo = vol.geometry();
        let spc = u32::from(geo.sectors_per_cluster);
        let mut out = Vec::new();
        let mut cl = first_cluster;
        // Walk the chain via the serialized FAT (4 bytes per entry, LE).
        while cl >= 2 && !crate::fat32::fat32_is_eoc(cl) {
            for s in 0..spc {
                let lba = geo.first_data_sector + (cl - 2) * spc + s;
                let sec = vol.read_sector(lba);
                for chunk in sec.chunks_exact(DIR_ENTRY_SIZE) {
                    out.push(chunk.try_into().unwrap());
                }
            }
            let i = cl as usize * 4;
            cl = u32::from_le_bytes([
                vol.fat_bytes[i],
                vol.fat_bytes[i + 1],
                vol.fat_bytes[i + 2],
                vol.fat_bytes[i + 3],
            ]) & 0x0fff_ffff;
        }
        out
    }

    /// Find an entry by 11-byte name in a directory; return (first_cluster, size).
    fn find_entry(entries: &[[u8; DIR_ENTRY_SIZE]], name11: &[u8; 11]) -> Option<(u32, u32)> {
        for e in entries {
            if e[0] == 0x00 {
                break;
            }
            if &e[..11] == name11 {
                let cluster = (u32::from(u16::from_le_bytes([e[20], e[21]])) << 16)
                    | u32::from(u16::from_le_bytes([e[26], e[27]]));
                let size = u32::from_le_bytes([e[28], e[29], e[30], e[31]]);
                return Some((cluster, size));
            }
        }
        None
    }

    /// Recover a file's bytes by following its FAT chain through read_sector.
    fn read_file(vol: &Fat32Volume, first_cluster: u32, size: u32) -> Vec<u8> {
        let geo = vol.geometry();
        let spc = u32::from(geo.sectors_per_cluster);
        let mut out = Vec::new();
        let mut cl = first_cluster;
        while cl >= 2 && !crate::fat32::fat32_is_eoc(cl) {
            for s in 0..spc {
                let lba = geo.first_data_sector + (cl - 2) * spc + s;
                out.extend_from_slice(&vol.read_sector(lba));
            }
            let i = cl as usize * 4;
            cl = u32::from_le_bytes([
                vol.fat_bytes[i],
                vol.fat_bytes[i + 1],
                vol.fat_bytes[i + 2],
                vol.fat_bytes[i + 3],
            ]) & 0x0fff_ffff;
        }
        out.truncate(size as usize);
        out
    }

    #[test]
    fn boot_sector_and_fsinfo_are_valid() {
        let dir = temp_dir("bpb");
        std::fs::write(dir.join("A.TXT"), b"x").unwrap();
        let vol = build_fat32(&dir, TEST_BYTES, 0x1234_5678).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        let boot = vol.read_sector(0);
        assert_eq!(
            u16::from_le_bytes([boot[11], boot[12]]),
            512,
            "bytes/sector"
        );
        assert_eq!(&boot[82..90], b"FAT32   ", "filesystem type");
        assert_eq!(&boot[510..512], &[0x55, 0xAA], "boot signature");
        // The backup boot sector mirrors sector 0.
        let backup = vol.read_sector(u32::from(vol.geometry().backup_boot_sector));
        assert_eq!(boot, backup);
        // FSInfo carries its two signatures.
        let fsi = vol.read_sector(u32::from(vol.geometry().fsinfo_sector));
        assert_eq!(
            u32::from_le_bytes([fsi[0], fsi[1], fsi[2], fsi[3]]),
            0x4161_5252
        );
        assert_eq!(
            u32::from_le_bytes([fsi[484], fsi[485], fsi[486], fsi[487]]),
            0x6141_7272
        );
    }

    #[test]
    fn root_file_round_trips() {
        let dir = temp_dir("rootfile");
        // Larger than one sector so it spans the data region across sectors.
        let payload: Vec<u8> = (0..1500u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(dir.join("hello.txt"), &payload).unwrap();
        let vol = build_fat32(&dir, TEST_BYTES, 1).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        let root = read_dir_entries(&vol, vol.geometry().root_cluster);
        let (cluster, size) = find_entry(&root, b"HELLO   TXT").expect("HELLO.TXT in root");
        assert_eq!(size as usize, payload.len());
        assert_eq!(read_file(&vol, cluster, size), payload);
    }

    #[test]
    fn subdirectory_has_dot_entries_and_its_file() {
        let dir = temp_dir("subdir");
        let sub = dir.join("GAMES");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("READ.ME"), b"in a subdir").unwrap();
        let vol = build_fat32(&dir, TEST_BYTES, 1).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        let root = read_dir_entries(&vol, vol.geometry().root_cluster);
        let (games_cluster, _) = find_entry(&root, b"GAMES      ").expect("GAMES in root");

        let sub_entries = read_dir_entries(&vol, games_cluster);
        // "." names the subdir itself; ".." names the root (cluster 0 per spec).
        assert_eq!(&sub_entries[0][..11], b".          ");
        let dot_cluster = (u32::from(u16::from_le_bytes([sub_entries[0][20], sub_entries[0][21]]))
            << 16)
            | u32::from(u16::from_le_bytes([sub_entries[0][26], sub_entries[0][27]]));
        assert_eq!(dot_cluster, games_cluster, "\".\" points at the subdir");
        assert_eq!(&sub_entries[1][..11], b"..         ");
        let dotdot_cluster =
            (u32::from(u16::from_le_bytes([sub_entries[1][20], sub_entries[1][21]])) << 16)
                | u32::from(u16::from_le_bytes([sub_entries[1][26], sub_entries[1][27]]));
        assert_eq!(dotdot_cluster, 0, "\"..\" of a root child is cluster 0");

        let (cl, size) = find_entry(&sub_entries, b"READ    ME ").expect("READ.ME in GAMES");
        assert_eq!(read_file(&vol, cl, size), b"in a subdir");
    }

    #[test]
    fn empty_file_has_zero_cluster_and_size() {
        let dir = temp_dir("empty");
        std::fs::write(dir.join("EMPTY.DAT"), b"").unwrap();
        let vol = build_fat32(&dir, TEST_BYTES, 1).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        let root = read_dir_entries(&vol, vol.geometry().root_cluster);
        let (cluster, size) = find_entry(&root, b"EMPTY   DAT").expect("EMPTY.DAT in root");
        assert_eq!(cluster, 0, "an empty file occupies no clusters");
        assert_eq!(size, 0);
    }

    #[test]
    fn too_small_a_volume_is_rejected() {
        let dir = temp_dir("toosmall");
        std::fs::write(dir.join("A.TXT"), b"x").unwrap();
        // 16 MB is below the FAT32 cluster floor.
        let r = build_fat32(&dir, 16 * 1024 * 1024, 1);
        std::fs::remove_dir_all(&dir).ok();
        assert!(r.is_err(), "a sub-FAT32 volume size is rejected");
    }
}
