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

use crate::fat32::FAT32_EOC;
use crate::katea_names::NameTable;
use crate::katea_volume::{
    FAT0_MEDIA, FileSource, NUM_FATS, PART_START, RESERVED_SECTORS, ROOT_CLUSTER, SECTOR,
    fat_size_sectors, sectors_per_cluster,
};
use std::collections::HashSet;
use std::path::Path;

/// Cap recursion so a pathological tree (or an undetected loop) can't run away;
/// also roughly the depth DOS's 64-char path limit allows.
const MAX_DEPTH: usize = 32;

/// Floor on the data-cluster count so the synthesized partition is always a
/// valid, boot-tested FAT32. This is exactly the M0 static disk's data-cluster
/// count: `(PART_SECTORS - used) / spc` for the proven-bootable 96256-sector
/// partition (`used = 32 + 2*741`). Flooring here means a small host folder
/// reproduces M0's known-good geometry (`part_sectors = 96256`, `fatsz = 741`)
/// instead of landing just under `sectors_per_cluster`'s FAT32 floor (66601
/// sectors), where it would panic. A larger folder grows past this floor.
const MIN_DATA_CLUSTERS: u32 = 94_742;

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

/// The synthesized disk's geometry, derived from the tree's cluster needs.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Geometry {
    pub spc: u8,
    pub fatsz: u32,
    pub part_start: u32, // = PART_START
    pub part_sectors: u32,
    pub total_sectors: u32,     // whole disk
    pub first_data_sector: u32, // partition-relative; cluster 2 begins here
    pub count_of_clusters: u32,
}

/// Entries a directory needs: its files + subdirs, plus `.`/`..` for non-root.
fn entry_count(dir: &TreeDir, is_root: bool) -> u32 {
    let dot = if is_root { 0 } else { 2 };
    dot + dir.files.len() as u32 + dir.subdirs.len() as u32
}

/// Clusters a chain of `bytes` needs at this cluster size (>=1, even for empty).
fn clusters_for(bytes: u64, cluster_bytes: u32) -> u32 {
    (bytes.div_ceil(u64::from(cluster_bytes)) as u32).max(1)
}

/// Sum the cluster needs of the whole tree, pick the geometry that fits, then
/// assign first_cluster/cluster_count across the tree depth-first. The root is
/// cluster 2.
///
/// The cluster size (`spc`) the FAT and partition are sized with must match the
/// one `sectors_per_cluster` derives from the final partition size, or the BPB
/// is internally inconsistent and the disk won't boot. We reach that fixed point
/// by computing the *final* partition size each iteration (not a padded guess)
/// and re-deriving `spc` from it; if it disagrees we adopt the larger and redo.
/// `sectors_per_cluster`'s table is monotonic in size and the partition only
/// grows with `spc`, so the loop climbs the table at most a few steps and stops.
pub(crate) fn allocate(tree: &mut HostTree) -> Geometry {
    let mut spc: u8 = 1;
    let geo = loop {
        let cluster_bytes = u32::from(spc) * SECTOR as u32;
        let used_data = tree_cluster_demand(tree, cluster_bytes);
        // Need a valid FAT32; pad with headroom (25%) so DIR shows free space and
        // M2 has room, and floor at the boot-tested M0 cluster count so the small-
        // folder partition reproduces M0's known-good geometry (see
        // MIN_DATA_CLUSTERS) rather than landing just under the FAT32 floor.
        let needed = used_data.max(1);
        let count_of_clusters = (needed + needed / 4).max(MIN_DATA_CLUSTERS);
        // Size the FAT from the whole partition it lives in, exactly as M0 does
        // (`fat_size_sectors(PART_SECTORS, spc)`): the formula's divisor accounts
        // for the FAT sectors, so passing the full partition is self-correcting.
        // We do not know `fatsz` until we size the partition, and the partition
        // size includes the FAT — so close the loop by re-deriving `fatsz` from
        // the partition built with the previous estimate until it is stable (it
        // settles in one or two steps because the data region dominates).
        let data_sectors = count_of_clusters * u32::from(spc);
        let mut fatsz = fat_size_sectors(u32::from(RESERVED_SECTORS) + data_sectors, spc);
        loop {
            let part = u32::from(RESERVED_SECTORS) + u32::from(NUM_FATS) * fatsz + data_sectors;
            let next_fatsz = fat_size_sectors(part, spc);
            if next_fatsz == fatsz {
                break;
            }
            fatsz = next_fatsz;
        }
        let used = u32::from(RESERVED_SECTORS) + u32::from(NUM_FATS) * fatsz;
        let part_sectors = used + data_sectors;
        // Self-consistency: the spc the table picks for THIS partition must equal
        // the spc we sized with. If not, climb to it and recompute from scratch.
        let derived = sectors_per_cluster(part_sectors);
        if derived != spc {
            spc = derived;
            continue;
        }
        break Geometry {
            spc,
            fatsz,
            part_start: PART_START,
            part_sectors,
            total_sectors: PART_START + part_sectors,
            first_data_sector: used,
            count_of_clusters,
        };
    };
    debug_assert_eq!(sectors_per_cluster(geo.part_sectors), geo.spc);
    // Assign clusters now that geometry is fixed.
    let cluster_bytes = u32::from(geo.spc) * SECTOR as u32;
    let mut next = ROOT_CLUSTER; // 2
    assign_dir(&mut tree.root, true, 0, &mut next, cluster_bytes);
    geo
}

/// Total data clusters the tree consumes (directories + files), for sizing.
fn tree_cluster_demand(tree: &HostTree, cluster_bytes: u32) -> u32 {
    fn dir_demand(dir: &TreeDir, is_root: bool, cluster_bytes: u32) -> u32 {
        let mut n = clusters_for(u64::from(entry_count(dir, is_root)) * 32, cluster_bytes);
        for f in &dir.files {
            n += clusters_for(f.source.len(), cluster_bytes);
        }
        for s in &dir.subdirs {
            n += dir_demand(&s.dir, false, cluster_bytes);
        }
        n
    }
    dir_demand(&tree.root, true, cluster_bytes)
}

/// Depth-first: assign this directory's chain, then its files' chains, then
/// recurse into subdirectories. `parent` is the parent dir's first cluster.
fn assign_dir(dir: &mut TreeDir, is_root: bool, parent: u32, next: &mut u32, cluster_bytes: u32) {
    dir.first_cluster = *next;
    dir.parent_first_cluster = parent;
    dir.cluster_count = clusters_for(u64::from(entry_count(dir, is_root)) * 32, cluster_bytes);
    *next += dir.cluster_count;
    for f in &mut dir.files {
        f.first_cluster = *next;
        f.cluster_count = clusters_for(f.source.len(), cluster_bytes);
        *next += f.cluster_count;
    }
    for s in &mut dir.subdirs {
        let parent_fc = dir.first_cluster;
        assign_dir(&mut s.dir, false, parent_fc, next, cluster_bytes);
    }
}

/// The computed-on-demand FAT. Task 4 assigns every chain as one *contiguous
/// run* of clusters, so a used cluster's FAT entry is simply `c + 1` unless `c`
/// is the last cluster of its run (then EOC), and any cluster the tree never
/// touched is free (0). We therefore store only the set of run-end clusters and
/// `next_free` (the first never-allocated cluster) and derive every FAT entry —
/// and any FAT sector — from those, so RAM scales with the chain count rather
/// than the disk size.
pub(crate) struct ClusterIndex {
    next_free: u32,           // first cluster never allocated
    chain_ends: HashSet<u32>, // last cluster of every chain (-> EOC)
}

impl ClusterIndex {
    pub(crate) fn build(tree: &HostTree, _geo: &Geometry) -> Self {
        let mut chain_ends = HashSet::new();
        let mut next_free = ROOT_CLUSTER;
        fn visit(dir: &TreeDir, ends: &mut HashSet<u32>, next_free: &mut u32) {
            push_run(dir.first_cluster, dir.cluster_count, ends, next_free);
            for f in &dir.files {
                push_run(f.first_cluster, f.cluster_count, ends, next_free);
            }
            for s in &dir.subdirs {
                visit(&s.dir, ends, next_free);
            }
        }
        fn push_run(first: u32, count: u32, ends: &mut HashSet<u32>, next_free: &mut u32) {
            if count == 0 {
                return;
            }
            ends.insert(first + count - 1);
            *next_free = (*next_free).max(first + count);
        }
        visit(&tree.root, &mut chain_ends, &mut next_free);
        Self {
            next_free,
            chain_ends,
        }
    }

    /// The FAT entry value for cluster `c` (28-bit).
    pub(crate) fn fat_entry(&self, c: u32) -> u32 {
        match c {
            0 => FAT0_MEDIA,
            1 => FAT32_EOC,
            _ if c < self.next_free => {
                if self.chain_ends.contains(&c) {
                    FAT32_EOC
                } else {
                    c + 1
                }
            }
            _ => 0, // free
        }
    }

    /// One 512-byte sector of a FAT copy: the `sector`-th sector holds entries
    /// `[sector*128 .. sector*128+128)`. Past the entries it is zero.
    pub(crate) fn fat_sector(&self, sector: u32, _geo: &Geometry) -> [u8; SECTOR] {
        let mut out = [0u8; SECTOR];
        let base = sector * 128; // 128 FAT32 entries per 512B sector
        for i in 0..128u32 {
            let v = (self.fat_entry(base + i) & 0x0FFF_FFFF).to_le_bytes();
            let off = (i as usize) * 4;
            out[off..off + 4].copy_from_slice(&v);
        }
        out
    }

    pub(crate) fn next_free(&self) -> u32 {
        self.next_free
    }
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

    #[test]
    fn allocation_chains_dirs_and_files_and_sizes_the_disk() {
        let root = scratch("alloc");
        std::fs::create_dir_all(root.join("SUB")).unwrap();
        std::fs::write(root.join("SUB/A.TXT"), vec![0u8; 600]).unwrap(); // 2 clusters at 512B/clu
        let sys = vec![("KERNEL.SYS".to_string(), vec![0u8; 100])];
        let mut tree = build_tree(&root, &sys);
        let geo = allocate(&mut tree);

        // Root is cluster 2 (one cluster: KERNEL.SYS + SUB = 2 entries).
        assert_eq!(tree.root.first_cluster, 2);
        assert_eq!(tree.root.cluster_count, 1);
        // SUB is a subdir directory chain; its `..` points at the root (cluster 2).
        let sub = &tree.root.subdirs[0].dir;
        assert!(sub.first_cluster >= 3);
        assert_eq!(sub.parent_first_cluster, 2);
        // A.TXT spans 2 clusters.
        assert_eq!(sub.files[0].cluster_count, 2);
        // Geometry: a valid FAT32 (>= 65525 clusters), spc derived, fatsz via the
        // kernel formula (not fatgen103).
        assert!(geo.count_of_clusters >= 65525);
        assert!(geo.total_sectors > geo.part_start);
        // The geometry must be self-consistent: the spc used to size the FAT/disk
        // must equal the one `sectors_per_cluster` picks for the final partition.
        assert_eq!(sectors_per_cluster(geo.part_sectors), geo.spc);
        // first_data_sector == reserved + NUM_FATS * fatsz; total == part_start + part_sectors.
        assert_eq!(
            geo.first_data_sector,
            u32::from(RESERVED_SECTORS) + u32::from(NUM_FATS) * geo.fatsz
        );
        assert_eq!(geo.total_sectors, geo.part_start + geo.part_sectors);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn fat_sector_reflects_the_allocated_chains() {
        let root = scratch("fat");
        std::fs::write(root.join("A.TXT"), vec![0u8; 600]).unwrap(); // 2 clusters
        let sys = vec![("KERNEL.SYS".to_string(), vec![0u8; 100])]; // 1 cluster
        let mut tree = build_tree(&root, &sys);
        let geo = allocate(&mut tree);
        let idx = ClusterIndex::build(&tree, &geo);

        // FAT[0] media, FAT[1] EOC, FAT[2]=root (single cluster -> EOC).
        assert_eq!(idx.fat_entry(0) & 0x0FFF_FFFF, 0x0FFF_FFF8);
        assert_eq!(idx.fat_entry(1), 0x0FFF_FFFF);
        assert_eq!(idx.fat_entry(2), 0x0FFF_FFFF); // root, 1 cluster
        // A.TXT occupies 2 contiguous clusters c -> c+1 -> EOC.
        let a = tree
            .root
            .files
            .iter()
            .find(|f| &f.name == b"A       TXT")
            .unwrap();
        assert_eq!(idx.fat_entry(a.first_cluster), a.first_cluster + 1);
        assert_eq!(idx.fat_entry(a.first_cluster + 1), 0x0FFF_FFFF);
        // A free cluster past the end is 0.
        assert_eq!(idx.fat_entry(geo.count_of_clusters + 2), 0);

        // The first FAT sector (partition-relative LBA RESERVED_SECTORS) holds the
        // first 128 entries little-endian.
        let s = idx.fat_sector(0, &geo);
        assert_eq!(
            u32::from_le_bytes([s[0], s[1], s[2], s[3]]) & 0x0FFF_FFFF,
            0x0FFF_FFF8
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
